package main

import (
	"bufio"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/json"
	"flag"
	"fmt"
	"math"
	"os"
	"path/filepath"
	"runtime"
	"sort"
	"strings"
	"sync/atomic"
	"time"

	log "cosmossdk.io/log/v2"
	sdkmath "cosmossdk.io/math"
	wasmapp "github.com/CosmWasm/wasmd/app"
	wasm "github.com/CosmWasm/wasmd/x/wasm"
	wasmkeeper "github.com/CosmWasm/wasmd/x/wasm/keeper"
	abci "github.com/cometbft/cometbft/abci/types"
	cmted25519 "github.com/cometbft/cometbft/crypto/ed25519"
	tmproto "github.com/cometbft/cometbft/proto/tendermint/types"
	cmttypes "github.com/cometbft/cometbft/types"
	dbm "github.com/cosmos/cosmos-db"
	"github.com/cosmos/cosmos-sdk/baseapp"
	"github.com/cosmos/cosmos-sdk/baseapp/txnrunner"
	"github.com/cosmos/cosmos-sdk/client/flags"
	storetypes "github.com/cosmos/cosmos-sdk/store/v2/types"
	sdk "github.com/cosmos/cosmos-sdk/types"
	authtypes "github.com/cosmos/cosmos-sdk/x/auth/types"
	banktypes "github.com/cosmos/cosmos-sdk/x/bank/types"
)

const (
	cosmosSDKVersion = "v0.54.4"
	wasmdVersion     = "v0.70.3"
	baselineScope    = "actual-wasmd-wasmvm-cosmos-sdk-txnrunner-blockstm-no-ante-abci"
	chainID          = "vegeta-s3-wasmd-blockstm"
)

type mapAppOptions map[string]any

func (m mapAppOptions) Get(k string) any { return m[k] }

type CoinSpec struct {
	Denom  string `json:"denom"`
	Amount string `json:"amount"`
}
type BankSeed struct {
	Address string `json:"address"`
	Denom   string `json:"denom"`
	Amount  string `json:"amount"`
}
type InstanceSpec struct {
	InstanceID     string `json:"instance_id"`
	Family         string `json:"family"`
	InstantiateMsg any    `json:"instantiate_msg"`
}
type CallSpec struct {
	Kind                      string     `json:"kind"`
	Family                    *string    `json:"family"`
	InstanceID                *string    `json:"instance_id"`
	Sender                    *string    `json:"sender"`
	Msg                       any        `json:"msg"`
	Funds                     []CoinSpec `json:"funds"`
	From                      *string    `json:"from"`
	To                        *string    `json:"to"`
	Coins                     []CoinSpec `json:"coins"`
	SourceRevertScopeActionID *uint64    `json:"source_revert_scope_action_id"`
}
type Manifest struct {
	WasmArtifacts map[string]string `json:"wasm_artifacts"`
	Instances     []InstanceSpec    `json:"instances"`
	BankSeeds     []BankSeed        `json:"bank_seeds"`
	PrimingCalls  []CallSpec        `json:"priming_calls"`
}
type ExecutionTx struct {
	TxIndex      int        `json:"tx_index"`
	TxHash       string     `json:"tx_hash"`
	SourceFailed bool       `json:"source_failed"`
	Calls        []CallSpec `json:"calls"`
}
type ExecutionBlock struct {
	BlockNumber  uint64        `json:"block_number"`
	Timestamp    uint64        `json:"timestamp"`
	Transactions []ExecutionTx `json:"transactions"`
}
type WeightRow struct {
	BlockNumber        uint64  `json:"block_number"`
	TxIndex            int     `json:"tx_index"`
	TxHash             string  `json:"tx_hash"`
	SourceTracePresent bool    `json:"source_trace_present"`
	SourceOpcodeSteps  *uint64 `json:"source_opcode_steps"`
}
type TxWeight struct {
	Hash    string
	Units   uint64
	Present bool
}
type Calibration struct {
	Scale          float64
	BaseTotalNanos uint64
	IterPerNano    float64
	Total          uint64
	Weights        map[[2]uint64]TxWeight
}

type Record struct {
	SchemaVersion        int     `json:"schema_version"`
	Dataset              string  `json:"dataset"`
	Sample               int     `json:"sample"`
	BlockNumber          uint64  `json:"block_number"`
	Strategy             string  `json:"strategy"`
	Workers              int     `json:"workers"`
	MatchedSerialNanos   uint64  `json:"matched_serial_nanos"`
	StrategyTotalNanos   uint64  `json:"strategy_total_nanos"`
	MatchedSerialSpeedup float64 `json:"matched_serial_speedup"`
	Transactions         int     `json:"transactions"`
	ExecutionAttempts    uint64  `json:"execution_attempts"`
	Reexecutions         uint64  `json:"reexecutions"`
	SerialEquivalent     bool    `json:"serial_equivalent"`
	ComputeMetric        string  `json:"compute_calibration_metric"`
	ComputeScale         float64 `json:"compute_scale"`
	GoIterationsPerNano  float64 `json:"go_iterations_per_nano"`
	CosmosSDKVersion     string  `json:"cosmos_sdk_version"`
	WasmdVersion         string  `json:"wasmd_version"`
	BaselineScope        string  `json:"baseline_scope"`
	BlockSTMPreEstimate  bool    `json:"block_stm_pre_estimate"`
}

type benchApp struct {
	app          *wasmapp.WasmApp
	permissioned *wasmkeeper.PermissionedKeeper
	contracts    map[string]sdk.AccAddress
	addresses    map[string]sdk.AccAddress
	home         string
}

func deterministicCompute(iterations uint64) uint64 {
	x := uint64(0x9E3779B97F4A7C15)
	for i := uint64(0); i < iterations; i++ {
		x ^= x << 7
		x ^= x >> 9
		x = x*0xBF58476D1CE4E5B9 + i
	}
	return x
}
func calibrateIterationsPerNano() float64 {
	n := uint64(2_000_000)
	for {
		s := time.Now()
		sink := deterministicCompute(n)
		runtime.KeepAlive(sink)
		d := time.Since(s)
		if d >= 100*time.Millisecond || n >= 1_000_000_000 {
			return float64(n) / float64(max64(uint64(d.Nanoseconds()), 1))
		}
		n *= 2
	}
}
func max64(a, b uint64) uint64 {
	if a > b {
		return a
	}
	return b
}

func readJSON(path string, out any) error {
	b, e := os.ReadFile(path)
	if e != nil {
		return e
	}
	return json.Unmarshal(b, out)
}
func readPlan(path string) ([]ExecutionBlock, error) {
	f, e := os.Open(path)
	if e != nil {
		return nil, e
	}
	defer f.Close()
	s := bufio.NewScanner(f)
	s.Buffer(make([]byte, 64<<10), 64<<20)
	var out []ExecutionBlock
	for s.Scan() {
		if strings.TrimSpace(s.Text()) == "" {
			continue
		}
		var b ExecutionBlock
		if e = json.Unmarshal(s.Bytes(), &b); e != nil {
			return nil, e
		}
		out = append(out, b)
	}
	return out, s.Err()
}
func readCalibration(path string, scale float64, baseNanos uint64, iterPerNano float64) (Calibration, error) {
	f, e := os.Open(path)
	if e != nil {
		return Calibration{}, e
	}
	defer f.Close()
	c := Calibration{Scale: scale, BaseTotalNanos: baseNanos, IterPerNano: iterPerNano, Weights: map[[2]uint64]TxWeight{}}
	s := bufio.NewScanner(f)
	s.Buffer(make([]byte, 64<<10), 16<<20)
	for s.Scan() {
		if strings.TrimSpace(s.Text()) == "" {
			continue
		}
		var r WeightRow
		if e = json.Unmarshal(s.Bytes(), &r); e != nil {
			return c, e
		}
		u := uint64(0)
		if r.SourceOpcodeSteps != nil {
			u = *r.SourceOpcodeSteps
		}
		c.Weights[[2]uint64{r.BlockNumber, uint64(r.TxIndex)}] = TxWeight{Hash: strings.ToLower(r.TxHash), Units: u, Present: r.SourceTracePresent}
		if r.SourceTracePresent {
			c.Total += u
		}
	}
	return c, s.Err()
}
func (c Calibration) iterations(block uint64, tx int, hash string) uint64 {
	if c.Scale <= 0 || c.Total == 0 {
		return 0
	}
	w, ok := c.Weights[[2]uint64{block, uint64(tx)}]
	if !ok || !w.Present || w.Units == 0 {
		return 0
	}
	if w.Hash != "" && strings.ToLower(hash) != w.Hash {
		panic(fmt.Sprintf("weight hash mismatch block=%d tx=%d", block, tx))
	}
	n := float64(c.BaseTotalNanos) * c.Scale * (float64(w.Units) / float64(c.Total)) * c.IterPerNano
	if n < 1 {
		return 1
	}
	if n > float64(math.MaxUint64) {
		return math.MaxUint64
	}
	return uint64(math.Round(n))
}

func setupSDKConfig() {
	c := sdk.GetConfig()
	c.SetBech32PrefixForAccount(wasmapp.Bech32PrefixAccAddr, wasmapp.Bech32PrefixAccPub)
	c.SetBech32PrefixForValidator(wasmapp.Bech32PrefixValAddr, wasmapp.Bech32PrefixValPub)
	c.SetBech32PrefixForConsensusNode(wasmapp.Bech32PrefixConsAddr, wasmapp.Bech32PrefixConsPub)
	c.Seal()
}
func logicalAddress(s string) sdk.AccAddress {
	sum := sha256.Sum256([]byte("symbgraph-s3:" + s))
	return sdk.AccAddress(append([]byte(nil), sum[:20]...))
}
func collectStrings(v any, out map[string]struct{}) {
	switch x := v.(type) {
	case string:
		if strings.HasPrefix(x, "0x") || strings.HasPrefix(x, "native-s3-") {
			out[x] = struct{}{}
		}
	case []any:
		for _, z := range x {
			collectStrings(z, out)
		}
	case map[string]any:
		for _, z := range x {
			collectStrings(z, out)
		}
	}
}
func collectLogical(m Manifest, blocks []ExecutionBlock) []string {
	set := map[string]struct{}{"native-s3-admin": {}, "native-s3-fee": {}}
	for _, s := range m.BankSeeds {
		set[s.Address] = struct{}{}
	}
	for _, i := range m.Instances {
		collectStrings(i.InstantiateMsg, set)
	}
	calls := append([]CallSpec(nil), m.PrimingCalls...)
	for _, b := range blocks {
		for _, tx := range b.Transactions {
			calls = append(calls, tx.Calls...)
		}
	}
	for _, c := range calls {
		if c.Sender != nil {
			set[*c.Sender] = struct{}{}
		}
		if c.From != nil {
			set[*c.From] = struct{}{}
		}
		if c.To != nil {
			set[*c.To] = struct{}{}
		}
		collectStrings(c.Msg, set)
	}
	out := make([]string, 0, len(set))
	for s := range set {
		out = append(out, s)
	}
	sort.Strings(out)
	return out
}
func rewrite(v any, repl map[string]string) any {
	switch x := v.(type) {
	case string:
		if y, ok := repl[x]; ok {
			return y
		}
		return x
	case []any:
		y := make([]any, len(x))
		for i, z := range x {
			y[i] = rewrite(z, repl)
		}
		return y
	case map[string]any:
		y := make(map[string]any, len(x))
		for k, z := range x {
			y[k] = rewrite(z, repl)
		}
		return y
	default:
		return x
	}
}
func jsonBytes(v any, repl map[string]string) []byte {
	b, e := json.Marshal(rewrite(v, repl))
	if e != nil {
		panic(e)
	}
	return b
}
func coins(xs []CoinSpec) sdk.Coins {
	out := make(sdk.Coins, 0, len(xs))
	for _, c := range xs {
		i, ok := sdkmath.NewIntFromString(c.Amount)
		if !ok {
			panic("invalid coin amount " + c.Amount)
		}
		out = append(out, sdk.NewCoin(c.Denom, i))
	}
	return out.Sort()
}

func benchmarkGenesisWithValidator(a *wasmapp.WasmApp) (map[string]json.RawMessage, error) {
	// Wasmd's module manager requires a non-empty staking validator set at
	// InitGenesis. Use the same application test helper Wasmd exposes for this
	// purpose so staking/auth/bank genesis remain mutually consistent. The
	// validator is deterministic, exists only for benchmark bootstrap, and all
	// of this work happens before any measured S3 execution.
	priv := cmted25519.GenPrivKeyFromSecret([]byte("symbgraphpool-vegeta-s3-wasmd-validator-v1"))
	valSet := cmttypes.NewValidatorSet([]*cmttypes.Validator{
		cmttypes.NewValidator(priv.PubKey(), 1),
	})
	delegator := logicalAddress("native-s3-genesis-validator")
	genAcc := authtypes.NewBaseAccountWithAddress(delegator)
	// GenesisStateWithValSet bonds one DefaultPowerReduction from the first
	// genesis account for each validator. Keep extra stake in the account so the
	// helper never depends on an exact-balance edge case across SDK patch levels.
	stake := sdk.NewCoin(sdk.DefaultBondDenom, sdk.DefaultPowerReduction.MulRaw(10))
	genesisState, err := wasmapp.GenesisStateWithValSet(
		a.AppCodec(),
		a.DefaultGenesis(),
		valSet,
		[]authtypes.GenesisAccount{genAcc},
		banktypes.Balance{Address: delegator.String(), Coins: sdk.NewCoins(stake)},
	)
	if err != nil {
		return nil, err
	}
	// GenesisStateWithValSet focuses on staking/auth/bank consistency. The
	// benchmark also invokes the Wasm keeper directly during setup, so x/wasm
	// must receive its own module genesis and persist Params before Create().
	// Without this explicit default genesis, Keeper.GetParams fails with
	// collections: not found: key 'no_key' of type cosmwasm.wasm.v1.Params.
	wasmBasic := wasm.AppModuleBasic{}
	genesisState[wasmBasic.Name()] = wasmBasic.DefaultGenesis(a.AppCodec())
	return genesisState, nil
}

func newBenchApp(repoRoot string, m Manifest, blocks []ExecutionBlock) (*benchApp, error) {
	home, e := os.MkdirTemp("", "symbgraph-wasmd-blockstm-")
	if e != nil {
		return nil, e
	}
	opts := mapAppOptions{flags.FlagHome: home, "wasm": map[string]any{}}
	// NewWasmApp mounts the complete Wasmd store-key set, while loadLatest=true
	// asks BaseApp to load/materialize those mounted stores before InitChain. On a
	// fresh MemDB this loads version 0; without it InitChain can enter module
	// genesis with an empty cache-multistore and panic on stores such as x/upgrade.
	a := wasmapp.NewWasmApp(log.NewNopLogger(), dbm.NewMemDB(), true, opts, nil, baseapp.SetChainID(chainID))
	genesisState, e := benchmarkGenesisWithValidator(a)
	if e != nil {
		return nil, fmt.Errorf("build benchmark genesis validator set: %w", e)
	}
	genesis, e := json.Marshal(genesisState)
	if e != nil {
		return nil, e
	}
	firstTime := int64(1_678_170_000)
	if len(blocks) > 0 && blocks[0].Timestamp > 1 {
		firstTime = int64(blocks[0].Timestamp) - 2
	}
	if _, e = a.InitChain(&abci.RequestInitChain{ChainId: chainID, InitialHeight: 1, Time: time.Unix(firstTime, 0), AppStateBytes: genesis}); e != nil {
		return nil, e
	}
	if _, e = a.Commit(); e != nil {
		return nil, e
	}
	// Height 1 is a benchmark-setup block used only to install/instantiate the
	// frozen S3 contracts and seed state. Workload blocks begin at Cosmos height 2.
	// The S3 source timestamps are preserved during workload execution; the native
	// contracts do not consume source block height.
	ctx := a.NewNextBlockContext(tmproto.Header{ChainID: chainID, Height: 1, Time: time.Unix(firstTime+1, 0)})
	addrs := map[string]sdk.AccAddress{}
	repl := map[string]string{}
	for _, s := range collectLogical(m, blocks) {
		ad := logicalAddress(s)
		addrs[s] = ad
		repl[s] = ad.String()
		if a.AccountKeeper.GetAccount(ctx, ad) == nil {
			a.AccountKeeper.SetAccount(ctx, a.AccountKeeper.NewAccountWithAddress(ctx, ad))
		}
	}
	for _, s := range m.BankSeeds {
		i, ok := sdkmath.NewIntFromString(s.Amount)
		if !ok {
			return nil, fmt.Errorf("invalid seed amount %s", s.Amount)
		}
		if e := a.BankKeeper.UncheckedSetBalance(ctx, addrs[s.Address], sdk.NewCoin(s.Denom, i)); e != nil {
			return nil, e
		}
	}
	pk := wasmkeeper.NewDefaultPermissionKeeper(&a.WasmKeeper)
	codes := map[string]uint64{}
	families := make([]string, 0, len(m.WasmArtifacts))
	for f := range m.WasmArtifacts {
		families = append(families, f)
	}
	sort.Strings(families)
	admin := addrs["native-s3-admin"]
	for _, f := range families {
		p := m.WasmArtifacts[f]
		if !filepath.IsAbs(p) {
			p = filepath.Join(repoRoot, p)
		}
		bz, e := os.ReadFile(p)
		if e != nil {
			return nil, e
		}
		id, _, e := pk.Create(ctx, admin, bz, nil)
		if e != nil {
			return nil, fmt.Errorf("store wasm %s: %w", f, e)
		}
		codes[f] = id
	}
	contracts := map[string]sdk.AccAddress{}
	for _, i := range m.Instances {
		msg := jsonBytes(i.InstantiateMsg, repl)
		addr, _, e := pk.Instantiate(ctx, codes[i.Family], admin, nil, msg, i.InstanceID, nil)
		if e != nil {
			return nil, fmt.Errorf("instantiate %s/%s: %w", i.Family, i.InstanceID, e)
		}
		contracts[i.InstanceID] = addr
		repl[i.InstanceID] = addr.String()
	}
	b := &benchApp{app: a, permissioned: pk, contracts: contracts, addresses: addrs, home: home}
	for n, c := range m.PrimingCalls {
		if e := b.executeCall(ctx, c, repl); e != nil {
			return nil, fmt.Errorf("priming call %d: %w", n, e)
		}
	}
	if _, e = a.Commit(); e != nil {
		return nil, e
	}
	return b, nil
}
func (b *benchApp) close() { _ = b.app.Close(); _ = os.RemoveAll(b.home) }
func (b *benchApp) replacements() map[string]string {
	r := map[string]string{}
	for k, v := range b.addresses {
		r[k] = v.String()
	}
	for k, v := range b.contracts {
		r[k] = v.String()
	}
	return r
}
func (b *benchApp) executeCall(ctx sdk.Context, c CallSpec, repl map[string]string) error {
	switch c.Kind {
	case "noop":
		return nil
	case "execute":
		if c.InstanceID == nil || c.Sender == nil {
			return fmt.Errorf("execute missing instance/sender")
		}
		_, e := b.permissioned.Execute(ctx, b.contracts[*c.InstanceID], b.addresses[*c.Sender], jsonBytes(c.Msg, repl), coins(c.Funds))
		return e
	case "query":
		if c.InstanceID == nil {
			return fmt.Errorf("query missing instance")
		}
		_, e := b.app.WasmKeeper.QuerySmart(ctx, b.contracts[*c.InstanceID], jsonBytes(c.Msg, repl))
		return e
	case "bank_send":
		if c.From == nil || c.To == nil {
			return fmt.Errorf("bank_send missing endpoint")
		}
		return b.app.BankKeeper.SendCoins(ctx, b.addresses[*c.From], b.addresses[*c.To], coins(c.Coins))
	default:
		return fmt.Errorf("unsupported call kind %s", c.Kind)
	}
}
func (b *benchApp) executeTx(ctx sdk.Context, block ExecutionBlock, tx ExecutionTx, compute uint64) error {
	if compute > 0 {
		sink := deterministicCompute(compute)
		runtime.KeepAlive(sink)
	}
	repl := b.replacements()
	outer, write := ctx.CacheContext()
	for i := 0; i < len(tx.Calls); {
		c := tx.Calls[i]
		if c.SourceRevertScopeActionID != nil {
			scope := *c.SourceRevertScopeActionID
			child, _ := outer.CacheContext()
			for i < len(tx.Calls) && tx.Calls[i].SourceRevertScopeActionID != nil && *tx.Calls[i].SourceRevertScopeActionID == scope {
				if e := b.executeCall(child, tx.Calls[i], repl); e != nil {
					break
				}
				i++
			}
			for i < len(tx.Calls) && tx.Calls[i].SourceRevertScopeActionID != nil && *tx.Calls[i].SourceRevertScopeActionID == scope {
				i++
			}
			continue
		}
		if e := b.executeCall(outer, c, repl); e != nil {
			if tx.SourceFailed {
				return nil
			}
			return fmt.Errorf("block %d tx %d call %d: %w", block.BlockNumber, tx.TxIndex, i, e)
		}
		i++
	}
	if !tx.SourceFailed {
		write()
	}
	return nil
}

func digestApp(a *wasmapp.WasmApp) [32]byte {
	h := sha256.New()
	keys := append([]storetypes.StoreKey(nil), a.GetStoreKeys()...)
	sort.Slice(keys, func(i, j int) bool { return keys[i].Name() < keys[j].Name() })
	for _, key := range keys {
		kv := a.CommitMultiStore().GetKVStore(key)
		it := kv.Iterator(nil, nil)
		for ; it.Valid(); it.Next() {
			h.Write([]byte(key.Name()))
			h.Write([]byte{0})
			var n [8]byte
			binary.LittleEndian.PutUint64(n[:], uint64(len(it.Key())))
			h.Write(n[:])
			h.Write(it.Key())
			binary.LittleEndian.PutUint64(n[:], uint64(len(it.Value())))
			h.Write(n[:])
			h.Write(it.Value())
		}
		it.Close()
	}
	var out [32]byte
	copy(out[:], h.Sum(nil))
	return out
}
func txBytes(block ExecutionBlock) [][]byte {
	out := make([][]byte, len(block.Transactions))
	for i := range out {
		var b [16]byte
		binary.LittleEndian.PutUint64(b[:8], block.BlockNumber)
		binary.LittleEndian.PutUint64(b[8:], uint64(i))
		out[i] = append([]byte(nil), b[:]...)
	}
	return out
}

func main() {
	manifestPath := flag.String("manifest", "", "native execution manifest")
	planPath := flag.String("plan", "", "native execution plan JSONL")
	weightsPath := flag.String("compute-weights", "", "source compute weights JSONL")
	repoRoot := flag.String("repo-root", ".", "repository root")
	output := flag.String("output", "", "output JSONL")
	workers := flag.Int("workers", 4, "Block-STM workers")
	samples := flag.Int("samples", 1, "full-range samples")
	scale := flag.Float64("compute-scale", 4, "steps compute scale")
	baseMS := flag.Float64("compute-base-total-ms", 1000, "1x aggregate single-core supplement in ms")
	iterPerNs := flag.Float64("go-iterations-per-nano", 0, "pin Go compute calibration")
	calOnly := flag.Bool("calibrate-only", false, "print compute iterations/ns and exit")
	setupOnly := flag.Bool("setup-only", false, "initialize Wasmd + upload/instantiate/prime contracts, then exit")
	flag.Parse()
	setupSDKConfig()
	if *calOnly {
		fmt.Printf("%.9f\n", calibrateIterationsPerNano())
		return
	}
	if *manifestPath == "" || *planPath == "" || *workers < 1 || *samples < 1 {
		flag.Usage()
		os.Exit(2)
	}
	if !*setupOnly && (*weightsPath == "" || *output == "") {
		flag.Usage()
		os.Exit(2)
	}
	var manifest Manifest
	if e := readJSON(*manifestPath, &manifest); e != nil {
		panic(e)
	}
	blocks, e := readPlan(*planPath)
	if e != nil {
		panic(e)
	}
	if len(blocks) == 0 {
		panic("empty execution plan")
	}
	if *setupOnly {
		b, e := newBenchApp(*repoRoot, manifest, blocks)
		if e != nil {
			panic(e)
		}
		b.close()
		fmt.Printf("PASS: Wasmd/WasmVM S3 setup contracts=%d blocks=%d scope=%s\n", len(manifest.Instances), len(blocks), baselineScope)
		return
	}
	if *iterPerNs <= 0 {
		*iterPerNs = calibrateIterationsPerNano()
	}
	cal, e := readCalibration(*weightsPath, *scale, uint64(*baseMS*1e6), *iterPerNs)
	if e != nil {
		panic(e)
	}
	f, e := os.Create(*output)
	if e != nil {
		panic(e)
	}
	defer f.Close()
	w := bufio.NewWriter(f)
	defer w.Flush()
	fmt.Fprintf(os.Stderr, "wasmd Block-STM sdk=%s wasmd=%s workers=%d samples=%d go-iter/ns=%.6f\n", cosmosSDKVersion, wasmdVersion, *workers, *samples, *iterPerNs)
	for sample := 0; sample < *samples; sample++ {
		serial, e := newBenchApp(*repoRoot, manifest, blocks)
		if e != nil {
			panic(e)
		}
		stm, e := newBenchApp(*repoRoot, manifest, blocks)
		if e != nil {
			panic(e)
		}
		func() {
			defer serial.close()
			defer stm.close()
			const preEstimate = false
			runner := txnrunner.NewSTMRunner(sdk.TxDecoder(func([]byte) (sdk.Tx, error) { return nil, nil }), stm.app.GetStoreKeys(), *workers, preEstimate, func(storetypes.MultiStore) string { return sdk.DefaultBondDenom })
			for blockOffset, block := range blocks {
				header := tmproto.Header{ChainID: chainID, Height: int64(blockOffset + 2), Time: time.Unix(int64(block.Timestamp), 0)}
				serialCtx := serial.app.NewNextBlockContext(header)
				ss := time.Now()
				for _, tx := range block.Transactions {
					if e := serial.executeTx(serialCtx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); e != nil {
						panic(e)
					}
				}
				serialWall := time.Since(ss)
				if _, e := serial.app.Commit(); e != nil {
					panic(e)
				}
				stmBlockCtx := stm.app.NewNextBlockContext(header)
				var attempts atomic.Uint64
				start := time.Now()
				_, e := runner.Run(context.Background(), stmBlockCtx.MultiStore(), txBytes(block), func(_ []byte, _ sdk.Tx, ms storetypes.MultiStore, idx int, _ map[string]any) *abci.ExecTxResult {
					attempts.Add(1)
					ctx := stmBlockCtx.WithMultiStore(ms).WithEventManager(sdk.NewEventManager()).WithGasMeter(storetypes.NewInfiniteGasMeter())
					tx := block.Transactions[idx]
					if e := stm.executeTx(ctx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); e != nil {
						return &abci.ExecTxResult{Code: 1, Log: e.Error()}
					}
					return &abci.ExecTxResult{}
				})
				wall := time.Since(start)
				if e != nil {
					panic(e)
				}
				if _, e := stm.app.Commit(); e != nil {
					panic(e)
				}
				eq := digestApp(serial.app) == digestApp(stm.app)
				if !eq {
					panic(fmt.Sprintf("state mismatch sample=%d block=%d", sample, block.BlockNumber))
				}
				a := attempts.Load()
				rec := Record{SchemaVersion: 1, Dataset: "vegeta-s3-wasmd-blockstm", Sample: sample, BlockNumber: block.BlockNumber, Strategy: "cosmos-wasmd-block-stm", Workers: *workers, MatchedSerialNanos: uint64(serialWall.Nanoseconds()), StrategyTotalNanos: uint64(wall.Nanoseconds()), Transactions: len(block.Transactions), ExecutionAttempts: a, Reexecutions: a - uint64(len(block.Transactions)), SerialEquivalent: eq, ComputeMetric: "steps", ComputeScale: *scale, GoIterationsPerNano: *iterPerNs, CosmosSDKVersion: cosmosSDKVersion, WasmdVersion: wasmdVersion, BaselineScope: baselineScope, BlockSTMPreEstimate: preEstimate}
				if rec.StrategyTotalNanos > 0 {
					rec.MatchedSerialSpeedup = float64(rec.MatchedSerialNanos) / float64(rec.StrategyTotalNanos)
				}
				if e := json.NewEncoder(w).Encode(&rec); e != nil {
					panic(e)
				}
			}
		}()
	}
}
