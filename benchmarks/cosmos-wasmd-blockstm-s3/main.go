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
	"runtime/pprof"
	"sort"
	"strings"
	"sync/atomic"
	"time"

	log "cosmossdk.io/log/v2"
	sdkmath "cosmossdk.io/math"
	wasmapp "github.com/CosmWasm/wasmd/app"
	wasm "github.com/CosmWasm/wasmd/x/wasm"
	wasmkeeper "github.com/CosmWasm/wasmd/x/wasm/keeper"
	wasmtypes "github.com/CosmWasm/wasmd/x/wasm/types"
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
	cosmosSDKVersion     = "v0.54.4"
	wasmdVersion         = "v0.70.3"
	baselineScope        = "actual-wasmd-wasmvm-cosmos-sdk-txnrunner-blockstm-prepared-payloads-no-ante-abci"
	directSerialScope    = "actual-wasmd-wasmvm-cosmos-sdk-direct-keeper-serial-prepared-payloads-no-ante-abci"
	symbGraphStaticScope = "actual-wasmd-wasmvm-cosmos-sdk-symbgraph-static-diagnostic-symbolic-predict-single-cache-fingerprint-validate-replay-no-ante-abci"
	vegetaScope          = "actual-wasmd-wasmvm-cosmos-sdk-vegeta-upstream-hotkey-reorder-rule2-dag-accesschange-reexecute-no-ante-abci"
	ariaFBScope          = "actual-wasmd-wasmvm-cosmos-sdk-ariafb-upstream-rule2-hotchain-dag-fallback-no-ante-abci"
	profileBaselineScope = "actual-wasmd-wasmvm-cosmos-sdk-txnrunner-blockstm-w4-pprof-unmeasured"
	chainID              = "vegeta-s3-wasmd-blockstm"
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
	SchemaVersion          int     `json:"schema_version"`
	Dataset                string  `json:"dataset"`
	Sample                 int     `json:"sample"`
	BlockNumber            uint64  `json:"block_number"`
	Strategy               string  `json:"strategy"`
	Workers                int     `json:"workers"`
	MatchedSerialNanos     uint64  `json:"matched_serial_nanos"`
	HistoricalSerialNanos  uint64  `json:"historical_serial_nanos"`
	StrategyTotalNanos     uint64  `json:"strategy_total_nanos"`
	PreConsensusNanos      uint64  `json:"pre_consensus_nanos,omitempty"`
	PostConsensusNanos     uint64  `json:"post_consensus_nanos,omitempty"`
	MatchedSerialSpeedup   float64 `json:"matched_serial_speedup"`
	Transactions           int     `json:"transactions"`
	ExecutionAttempts      uint64  `json:"execution_attempts"`
	Reexecutions           uint64  `json:"reexecutions"`
	SerialEquivalent       bool    `json:"serial_equivalent"`
	SerialReferenceScope   string  `json:"serial_reference_scope"`
	ComputeMetric          string  `json:"compute_calibration_metric"`
	ComputeScale           float64 `json:"compute_scale"`
	GoIterationsPerNano    float64 `json:"go_iterations_per_nano"`
	CosmosSDKVersion       string  `json:"cosmos_sdk_version"`
	WasmdVersion           string  `json:"wasmd_version"`
	BaselineScope          string  `json:"baseline_scope"`
	BlockSTMPreEstimate    bool    `json:"block_stm_pre_estimate"`
	SpeculatedTransactions uint64  `json:"speculated_transactions,omitempty"`
	ReusedTransactions     uint64  `json:"reused_transactions,omitempty"`
	ValidationNanos        uint64  `json:"validation_nanos,omitempty"`
	ReplayExecutionNanos   uint64  `json:"replay_execution_nanos,omitempty"`
	ConflictAnalysisNanos  uint64  `json:"conflict_analysis_nanos,omitempty"`
	DiscoveredConflicts    uint64  `json:"discovered_conflicts,omitempty"`
	ForwardFallbacks       uint64  `json:"forward_fallbacks,omitempty"`
	SafetyReplays          uint64  `json:"safety_replays,omitempty"`

	SymbGraphVariant                      string                `json:"symbgraph_variant,omitempty"`
	SymbPlanNanos                         uint64                `json:"symb_plan_nanos,omitempty"`
	SymbPreexecutionNanos                 uint64                `json:"symb_preexecution_nanos,omitempty"`
	SymbBranchCreateNanos                 uint64                `json:"symb_branch_create_nanos,omitempty"`
	SymbVisibilityNanos                   uint64                `json:"symb_visibility_nanos,omitempty"`
	SymbSpecExecutionNanos                uint64                `json:"symb_spec_execution_nanos,omitempty"`
	SymbDeltaCaptureNanos                 uint64                `json:"symb_delta_capture_nanos,omitempty"`
	SymbMVCCPublishNanos                  uint64                `json:"symb_mvcc_publish_nanos,omitempty"`
	SymbFeedbackBuildNanos                uint64                `json:"symb_feedback_build_nanos,omitempty"`
	SymbReconciliationNanos               uint64                `json:"symb_reconciliation_nanos,omitempty"`
	SymbValidationNanos                   uint64                `json:"symb_validation_nanos,omitempty"`
	SymbReplayExecutionNanos              uint64                `json:"symb_replay_execution_nanos,omitempty"`
	SymbRustFeedbackNanos                 uint64                `json:"symb_rust_feedback_nanos,omitempty"`
	SymbDependencyEdges                   int                   `json:"symb_dependency_edges,omitempty"`
	SymbFeedbackPairs                     int                   `json:"symb_feedback_pairs,omitempty"`
	SymbPhysicalCandidateEdges            int                   `json:"symb_physical_candidate_edges"`
	SymbLogicalCandidateEdges             int                   `json:"symb_logical_candidate_edges"`
	SymbCompactCandidateGroups            int                   `json:"symb_compact_candidate_groups"`
	SymbParentDependenciesBeforeReduction int                   `json:"symb_parent_dependencies_before_reduction"`
	SymbParentDependenciesElidedReduction int                   `json:"symb_parent_dependencies_elided_reduction"`
	SymbInitialReady                      int                   `json:"symb_initial_ready,omitempty"`
	SymbMaxReady                          int                   `json:"symb_max_ready,omitempty"`
	SymbAverageReady                      float64               `json:"symb_average_ready,omitempty"`
	SymbMaxActive                         int                   `json:"symb_max_active,omitempty"`
	SymbCriticalPathTx                    int                   `json:"symb_critical_path_tx,omitempty"`
	SymbCriticalPathCost                  uint64                `json:"symb_critical_path_cost,omitempty"`
	SymbTotalEstimatedCost                uint64                `json:"symb_total_estimated_cost,omitempty"`
	SymbDAGParallelism                    float64               `json:"symb_dag_parallelism,omitempty"`
	SymbWorkerUtilization                 float64               `json:"symb_worker_utilization,omitempty"`
	SymbWorkerIdleNanos                   uint64                `json:"symb_worker_idle_nanos,omitempty"`
	SymbMVCCPointReads                    uint64                `json:"symb_mvcc_point_reads,omitempty"`
	SymbMVCCVersionHits                   uint64                `json:"symb_mvcc_version_hits,omitempty"`
	SymbMVCCBaseFallbacks                 uint64                `json:"symb_mvcc_base_fallbacks,omitempty"`
	SymbMVCCRangeReads                    uint64                `json:"symb_mvcc_range_reads,omitempty"`
	SymbMVCCRangeOverlayKeys              uint64                `json:"symb_mvcc_range_overlay_keys,omitempty"`
	SymbMVCCPublishes                     uint64                `json:"symb_mvcc_publishes,omitempty"`
	SymbMVCCPublishedKeys                 uint64                `json:"symb_mvcc_published_keys,omitempty"`
	SymbPlanning                          *rustPlanningSnapshot `json:"symb_planning,omitempty"`
	SymbDependencyReasons                 map[string]int        `json:"symb_dependency_reasons,omitempty"`
	SymbDependencyPrimary                 map[string]int        `json:"symb_dependency_primary,omitempty"`
	SymbCriticalPath                      []int                 `json:"symb_critical_path,omitempty"`
	SymbCriticalPathReasons               map[string]int        `json:"symb_critical_path_reasons,omitempty"`
	SymbCriticalPathCostByReason          map[string]uint64     `json:"symb_critical_path_cost_by_reason,omitempty"`
	SymbDependencyProvenance              map[string]int        `json:"symb_dependency_provenance,omitempty"`
	SymbDependencyDecisions               map[string]int        `json:"symb_dependency_decisions,omitempty"`
	SymbCriticalPathProvenance            map[string]int        `json:"symb_critical_path_provenance,omitempty"`
	SymbCriticalPathDecisions             map[string]int        `json:"symb_critical_path_decisions,omitempty"`
	SymbCandidateHard                     int                   `json:"symb_candidate_hard,omitempty"`
	SymbCandidateSoft                     int                   `json:"symb_candidate_soft,omitempty"`
	SymbCandidateLow                      int                   `json:"symb_candidate_low,omitempty"`
	SymbOrderedHard                       int                   `json:"symb_ordered_hard,omitempty"`
	SymbOrderedSoft                       int                   `json:"symb_ordered_soft,omitempty"`
	SymbOracleConflictEdges               int                   `json:"symb_oracle_conflict_edges,omitempty"`
	SymbOracleCriticalPathTx              int                   `json:"symb_oracle_critical_path_tx,omitempty"`
	SymbOracleCriticalPathCost            uint64                `json:"symb_oracle_critical_path_cost,omitempty"`
	SymbOracleDAGParallelism              float64               `json:"symb_oracle_dag_parallelism,omitempty"`
	SymbOracleCriticalPath                []int                 `json:"symb_oracle_critical_path,omitempty"`
	SymbSerializationGap                  float64               `json:"symb_serialization_gap,omitempty"`
	SymbPlanRequestBuildNanos             uint64                `json:"symb_plan_request_build_nanos,omitempty"`
	SymbPlanRequestMarshalNanos           uint64                `json:"symb_plan_request_marshal_nanos,omitempty"`
	SymbPlanCGORoundTripNanos             uint64                `json:"symb_plan_cgo_roundtrip_nanos,omitempty"`
	SymbPlanResponseUnmarshalNanos        uint64                `json:"symb_plan_response_unmarshal_nanos,omitempty"`
	SymbPlanRustDecodeNanos               uint64                `json:"symb_plan_rust_decode_nanos,omitempty"`
	SymbPlanResolveComponentsNanos        uint64                `json:"symb_plan_resolve_components_nanos,omitempty"`
	SymbPlanCandidateGraphNanos           uint64                `json:"symb_plan_candidate_graph_nanos,omitempty"`
	SymbPlanSchedulerNanos                uint64                `json:"symb_plan_scheduler_nanos,omitempty"`
	SymbPlanProjectionNanos               uint64                `json:"symb_plan_projection_nanos,omitempty"`
	SymbPlanFeedbackPairsNanos            uint64                `json:"symb_plan_feedback_pairs_nanos,omitempty"`
	SymbPlanFinalizeNanos                 uint64                `json:"symb_plan_finalize_nanos,omitempty"`
	SymbPlanBridgeOtherNanos              uint64                `json:"symb_plan_bridge_other_nanos,omitempty"`
}

type preparedCallKey struct {
	blockNumber uint64
	txIndex     int
	callIndex   int
}

type preparedCall struct {
	kind     string
	contract sdk.AccAddress
	sender   sdk.AccAddress
	msg      []byte
	funds    sdk.Coins
	from     sdk.AccAddress
	to       sdk.AccAddress
	coins    sdk.Coins
}

type benchApp struct {
	app          *wasmapp.WasmApp
	permissioned *wasmkeeper.PermissionedKeeper
	contracts    map[string]sdk.AccAddress
	addresses    map[string]sdk.AccAddress
	repl         map[string]string
	prepared     map[preparedCallKey]preparedCall
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

func commitFinalizeState(a *wasmapp.WasmApp) error {
	// This harness mutates BaseApp's finalize-state directly via keepers and the
	// standalone Block-STM runner rather than through BaseApp.FinalizeBlock.
	// Flush that finalize cache into the root CommitMultiStore before Commit.
	// Cosmos SDK exposes SimWriteState specifically for this simulation/test path.
	a.SimWriteState()
	_, err := a.Commit()
	return err
}

func executeSerialOrderReference(b *benchApp, header tmproto.Header, block ExecutionBlock, cal Calibration, order []int) (uint64, error) {
	if len(order) != len(block.Transactions) {
		return 0, fmt.Errorf("serial reference order length mismatch block=%d order=%d txs=%d", block.BlockNumber, len(order), len(block.Transactions))
	}
	seen := make([]bool, len(block.Transactions))
	ctx := b.app.NewNextBlockContext(header)
	started := time.Now()
	for _, idx := range order {
		if idx < 0 || idx >= len(block.Transactions) {
			return 0, fmt.Errorf("serial reference order index out of range block=%d idx=%d", block.BlockNumber, idx)
		}
		if seen[idx] {
			return 0, fmt.Errorf("serial reference order duplicates block=%d idx=%d", block.BlockNumber, idx)
		}
		seen[idx] = true
		tx := block.Transactions[idx]
		if err := b.executeTx(ctx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); err != nil {
			return 0, err
		}
	}
	wall := uint64(time.Since(started).Nanoseconds())
	if err := commitFinalizeState(b.app); err != nil {
		return 0, err
	}
	return wall, nil
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
	if e = commitFinalizeState(a); e != nil {
		return nil, fmt.Errorf("commit benchmark genesis state: %w", e)
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
		// BankSeed denominations model source-chain assets, not Cosmos governance
		// policy. Mark each seeded denom send-enabled so Wasm BankMsg::Send and
		// replayed bank_send calls can transfer the source native asset (e.g.
		// unative) during setup and measured execution.
		a.BankKeeper.SetSendEnabled(ctx, s.Denom, true)
		i, ok := sdkmath.NewIntFromString(s.Amount)
		if !ok {
			return nil, fmt.Errorf("invalid seed amount %s", s.Amount)
		}
		if e := a.BankKeeper.UncheckedSetBalance(ctx, addrs[s.Address], sdk.NewCoin(s.Denom, i)); e != nil {
			return nil, e
		}
	}
	// The benchmark setup calls the Wasm keeper directly to upload code.
	// Seed the x/wasm params collection before PermissionedKeeper.Create reads
	// it. This setup-only write happens before any measured workload execution.
	if e := a.WasmKeeper.SetParams(ctx, wasmtypes.DefaultParams()); e != nil {
		return nil, fmt.Errorf("initialize wasm keeper params: %w", e)
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
	b := &benchApp{
		app:          a,
		permissioned: pk,
		contracts:    contracts,
		addresses:    addrs,
		repl:         repl,
		home:         home,
	}
	for n, c := range m.PrimingCalls {
		if e := b.executeCall(ctx, c, b.repl); e != nil {
			return nil, fmt.Errorf("priming call %d: %w", n, e)
		}
	}
	if e := b.prepareWorkloadCalls(blocks); e != nil {
		return nil, fmt.Errorf("prepare workload calls: %w", e)
	}
	if e = commitFinalizeState(a); e != nil {
		return nil, fmt.Errorf("commit wasm benchmark setup state: %w", e)
	}
	committedCtx := a.NewContext(true)
	for id, addr := range contracts {
		if !a.WasmKeeper.HasContractInfo(committedCtx, addr) {
			return nil, fmt.Errorf("wasm instance %s (%s) missing after setup commit", id, addr)
		}
	}
	return b, nil
}
func (b *benchApp) close()                          { _ = b.app.Close(); _ = os.RemoveAll(b.home) }
func (b *benchApp) replacements() map[string]string { return b.repl }

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

func (b *benchApp) prepareWorkloadCalls(blocks []ExecutionBlock) error {
	prepared := make(map[preparedCallKey]preparedCall)
	for _, block := range blocks {
		for _, tx := range block.Transactions {
			for callIndex, c := range tx.Calls {
				key := preparedCallKey{blockNumber: block.BlockNumber, txIndex: tx.TxIndex, callIndex: callIndex}
				pc := preparedCall{kind: c.Kind}
				switch c.Kind {
				case "noop":
				case "execute":
					if c.InstanceID == nil || c.Sender == nil {
						return fmt.Errorf("block %d tx %d call %d execute missing instance/sender", block.BlockNumber, tx.TxIndex, callIndex)
					}
					contract, ok := b.contracts[*c.InstanceID]
					if !ok {
						return fmt.Errorf("block %d tx %d call %d unknown instance %q", block.BlockNumber, tx.TxIndex, callIndex, *c.InstanceID)
					}
					sender, ok := b.addresses[*c.Sender]
					if !ok {
						return fmt.Errorf("block %d tx %d call %d unknown sender %q", block.BlockNumber, tx.TxIndex, callIndex, *c.Sender)
					}
					pc.contract = contract
					pc.sender = sender
					pc.msg = jsonBytes(c.Msg, b.repl)
					pc.funds = coins(c.Funds)
				case "query":
					if c.InstanceID == nil {
						return fmt.Errorf("block %d tx %d call %d query missing instance", block.BlockNumber, tx.TxIndex, callIndex)
					}
					contract, ok := b.contracts[*c.InstanceID]
					if !ok {
						return fmt.Errorf("block %d tx %d call %d unknown instance %q", block.BlockNumber, tx.TxIndex, callIndex, *c.InstanceID)
					}
					pc.contract = contract
					pc.msg = jsonBytes(c.Msg, b.repl)
				case "bank_send":
					if c.From == nil || c.To == nil {
						return fmt.Errorf("block %d tx %d call %d bank_send missing endpoint", block.BlockNumber, tx.TxIndex, callIndex)
					}
					from, ok := b.addresses[*c.From]
					if !ok {
						return fmt.Errorf("block %d tx %d call %d unknown bank sender %q", block.BlockNumber, tx.TxIndex, callIndex, *c.From)
					}
					to, ok := b.addresses[*c.To]
					if !ok {
						return fmt.Errorf("block %d tx %d call %d unknown bank recipient %q", block.BlockNumber, tx.TxIndex, callIndex, *c.To)
					}
					pc.from = from
					pc.to = to
					pc.coins = coins(c.Coins)
				default:
					return fmt.Errorf("block %d tx %d call %d unsupported call kind %s", block.BlockNumber, tx.TxIndex, callIndex, c.Kind)
				}
				prepared[key] = pc
			}
		}
	}
	b.prepared = prepared
	return nil
}

func (b *benchApp) preparedCall(block ExecutionBlock, tx ExecutionTx, callIndex int) (preparedCall, error) {
	key := preparedCallKey{blockNumber: block.BlockNumber, txIndex: tx.TxIndex, callIndex: callIndex}
	pc, ok := b.prepared[key]
	if !ok {
		return preparedCall{}, fmt.Errorf("missing prepared call block=%d tx=%d call=%d", block.BlockNumber, tx.TxIndex, callIndex)
	}
	return pc, nil
}

func (b *benchApp) executePreparedCall(ctx sdk.Context, block ExecutionBlock, tx ExecutionTx, callIndex int) error {
	pc, e := b.preparedCall(block, tx, callIndex)
	if e != nil {
		return e
	}
	switch pc.kind {
	case "noop":
		return nil
	case "execute":
		_, e := b.permissioned.Execute(ctx, pc.contract, pc.sender, pc.msg, pc.funds)
		return e
	case "query":
		_, e := b.app.WasmKeeper.QuerySmart(ctx, pc.contract, pc.msg)
		return e
	case "bank_send":
		return b.app.BankKeeper.SendCoins(ctx, pc.from, pc.to, pc.coins)
	default:
		return fmt.Errorf("unsupported prepared call kind %s", pc.kind)
	}
}

func (b *benchApp) executeTxCalls(ctx sdk.Context, block ExecutionBlock, tx ExecutionTx) error {
	for i := 0; i < len(tx.Calls); {
		c := tx.Calls[i]
		if c.SourceRevertScopeActionID != nil {
			// Source-side reverted scopes still need to execute because their
			// reads can affect control flow, but their writes must be discarded.
			scope := *c.SourceRevertScopeActionID
			child, _ := ctx.CacheContext()
			for i < len(tx.Calls) && tx.Calls[i].SourceRevertScopeActionID != nil && *tx.Calls[i].SourceRevertScopeActionID == scope {
				if e := b.executePreparedCall(child, block, tx, i); e != nil {
					break
				}
				i++
			}
			for i < len(tx.Calls) && tx.Calls[i].SourceRevertScopeActionID != nil && *tx.Calls[i].SourceRevertScopeActionID == scope {
				i++
			}
			continue
		}
		if e := b.executePreparedCall(ctx, block, tx, i); e != nil {
			return fmt.Errorf("block %d tx %d call %d: %w", block.BlockNumber, tx.TxIndex, i, e)
		}
		i++
	}
	return nil
}

func (b *benchApp) executeTx(ctx sdk.Context, block ExecutionBlock, tx ExecutionTx, compute uint64) error {
	if compute > 0 {
		sink := deterministicCompute(compute)
		runtime.KeepAlive(sink)
	}
	outer, write := ctx.CacheContext()
	if e := b.executeTxCalls(outer, block, tx); e != nil {
		if tx.SourceFailed {
			return nil
		}
		return e
	}
	if !tx.SourceFailed {
		write()
	}
	return nil
}

// executeTxIsolated executes directly on a caller-owned private transaction
// branch. SymbGraph/Vegeta already allocate a disposable CacheMultiStore for
// each attempt, so another top-level CacheContext would make every Wasmd store
// access traverse two transaction cache layers.
//
// The caller either writes this branch after validation or discards it. That
// branch is therefore the transaction atomicity boundary. Expected-failure
// transactions retain executeTx's inner disposable cache so all top-level
// effects are discarded. Reverted source scopes still use child caches inside
// executeTxCalls.
func (b *benchApp) executeTxIsolated(ctx sdk.Context, block ExecutionBlock, tx ExecutionTx, compute uint64) error {
	if tx.SourceFailed {
		return b.executeTx(ctx, block, tx, compute)
	}
	if compute > 0 {
		sink := deterministicCompute(compute)
		runtime.KeepAlive(sink)
	}
	return b.executeTxCalls(ctx, block, tx)
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

func writeRuntimeProfile(name, path string) error {
	p := pprof.Lookup(name)
	if p == nil {
		return fmt.Errorf("runtime profile %q unavailable", name)
	}
	f, err := os.Create(path)
	if err != nil {
		return err
	}
	defer f.Close()
	return p.WriteTo(f, 0)
}

func profileWasmdBlockSTM4(repoRoot string, m Manifest, blocks []ExecutionBlock, cal Calibration, profileDir string) error {
	if profileDir == "" {
		return nil
	}
	if err := os.MkdirAll(profileDir, 0o755); err != nil {
		return err
	}

	b, err := newBenchApp(repoRoot, m, blocks)
	if err != nil {
		return fmt.Errorf("create 4-worker profiling app: %w", err)
	}
	defer b.close()

	const workers = 4
	const preEstimate = false
	runner := txnrunner.NewSTMRunner(
		sdk.TxDecoder(func([]byte) (sdk.Tx, error) { return nil, nil }),
		b.app.GetStoreKeys(),
		workers,
		preEstimate,
		func(storetypes.MultiStore) string { return sdk.DefaultBondDenom },
	)

	cpuPath := filepath.Join(profileDir, "cosmos-wasmd-block-stm-w4.cpu.pprof")
	cpuFile, err := os.Create(cpuPath)
	if err != nil {
		return err
	}
	if err := pprof.StartCPUProfile(cpuFile); err != nil {
		cpuFile.Close()
		return err
	}

	// Profile only the 101-block TxRunner execution, not app creation or setup.
	runtime.SetBlockProfileRate(1)
	oldMutexFraction := runtime.SetMutexProfileFraction(1)
	runErr := func() error {
		for blockOffset, block := range blocks {
			header := tmproto.Header{
				ChainID: chainID,
				Height:  int64(blockOffset + 2),
				Time:    time.Unix(int64(block.Timestamp), 0),
			}
			blockCtx := b.app.NewNextBlockContext(header)
			_, err := runner.Run(
				context.Background(),
				blockCtx.MultiStore(),
				txBytes(block),
				func(_ []byte, _ sdk.Tx, ms storetypes.MultiStore, idx int, _ map[string]any) *abci.ExecTxResult {
					ctx := blockCtx.
						WithMultiStore(ms).
						WithEventManager(sdk.NewEventManager()).
						WithGasMeter(storetypes.NewInfiniteGasMeter())
					tx := block.Transactions[idx]
					if err := b.executeTx(ctx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); err != nil {
						return &abci.ExecTxResult{Code: 1, Log: err.Error()}
					}
					return &abci.ExecTxResult{}
				},
			)
			if err != nil {
				return fmt.Errorf("profile block %d: %w", block.BlockNumber, err)
			}
			if err := commitFinalizeState(b.app); err != nil {
				return fmt.Errorf("profile commit block %d: %w", block.BlockNumber, err)
			}
		}
		return nil
	}()

	pprof.StopCPUProfile()
	cpuCloseErr := cpuFile.Close()
	runtime.SetBlockProfileRate(0)
	runtime.SetMutexProfileFraction(oldMutexFraction)

	if runErr != nil {
		return runErr
	}
	if cpuCloseErr != nil {
		return cpuCloseErr
	}
	if err := writeRuntimeProfile("mutex", filepath.Join(profileDir, "cosmos-wasmd-block-stm-w4.mutex.pprof")); err != nil {
		return err
	}
	if err := writeRuntimeProfile("block", filepath.Join(profileDir, "cosmos-wasmd-block-stm-w4.block.pprof")); err != nil {
		return err
	}

	meta := map[string]any{
		"dataset":                    "vegeta-s3-wasmd-blockstm",
		"strategy":                   "cosmos-wasmd-block-stm",
		"workers":                    workers,
		"blocks":                     len(blocks),
		"scope":                      profileBaselineScope,
		"cosmos_sdk_version":         cosmosSDKVersion,
		"wasmd_version":              wasmdVersion,
		"compute_scale":              cal.Scale,
		"go_iterations_per_nano":     cal.IterPerNano,
		"block_stm_pre_estimate":     preEstimate,
		"measurement_included":       false,
		"profile_execution_separate": true,
	}
	metaBytes, err := json.MarshalIndent(meta, "", "  ")
	if err != nil {
		return err
	}
	metaBytes = append(metaBytes, '\n')
	return os.WriteFile(filepath.Join(profileDir, "cosmos-wasmd-block-stm-w4.profile.json"), metaBytes, 0o644)
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
	profileDir := flag.String(
		"profile-dir",
		os.Getenv("VEGETA_S3_WASMD_PPROF_DIR"),
		"write an additional unmeasured 4-worker Wasmd TxRunner CPU/mutex/block profile to this directory",
	)
	symbProfileDir := flag.String(
		"symbgraph-profile-dir",
		os.Getenv("VEGETA_S3_WASMD_SYMBGRAPH_PPROF_DIR"),
		"write an additional unmeasured 2-worker SymbGraph CPU profile and allocation deltas to this directory",
	)
	profileOnlyRunner := flag.String(
		"profile-only-runner",
		"",
		"run one isolated unmeasured profile and exit: direct-serial|outer-cache-serial|symbgraph-static",
	)
	profileOutputDir := flag.String(
		"profile-output-dir",
		"",
		"output directory for --profile-only-runner CPU/allocs/mutex profiles",
	)
	profileOnlyKind := flag.String(
		"profile-only-kind",
		"cpu-alloc",
		"isolated profile kind for --profile-only-runner: cpu-alloc|mutex",
	)
	symbolicDirDefault := os.Getenv("VEGETA_S3_SYMBOLIC_DIR")
	if symbolicDirDefault == "" {
		symbolicDirDefault = "benchmarks/symbolic/native-s3"
	}
	symbolicDir := flag.String(
		"symbolic-dir",
		symbolicDirDefault,
		"source-derived S3 symbolic profile directory (same input used by the native Rust scheduler)",
	)
	rustVisibility := flag.String(
		"symbgraph-rust-visibility",
		rustEnvOr("VEGETA_S3_RUST_ACG_VISIBILITY", rustVisibilityMVCC),
		"Rust ACG Wasmd launch visibility: mvcc|materialized",
	)
	rustValidation := flag.String(
		"symbgraph-rust-validation",
		rustEnvOr("VEGETA_S3_RUST_ACG_VALIDATION", rustValidationIndexed),
		"Rust ACG canonical validation: indexed|scan",
	)
	rustFeedback := flag.String(
		"symbgraph-rust-feedback",
		rustEnvOr("VEGETA_S3_RUST_ACG_FEEDBACK", rustFeedbackProfile),
		"Rust ACG concrete feedback pair selection: profile|all-pairs",
	)
	rustEdgeMaterialization := flag.String("symbgraph-rust-edge-materialization-threshold", os.Getenv("VEGETA_S3_RUST_ACG_EDGE_MATERIALIZATION_THRESHOLD"), "override Rust ACG edge materialization threshold; blank keeps Rust default")
	rustSoftThreshold := flag.String("symbgraph-rust-soft-threshold", os.Getenv("VEGETA_S3_RUST_ACG_SOFT_THRESHOLD"), "override Rust ACG soft scheduling threshold; blank keeps Rust default")
	rustHardThreshold := flag.String("symbgraph-rust-hard-threshold", os.Getenv("VEGETA_S3_RUST_ACG_HARD_THRESHOLD"), "override Rust ACG hard scheduling threshold; blank keeps Rust default")
	rustRiskBudget := flag.String("symbgraph-rust-risk-budget", os.Getenv("VEGETA_S3_RUST_ACG_RISK_BUDGET"), "override Rust ACG aggregate soft-risk budget; blank keeps Rust default")
	rustExplorationRate := flag.String("symbgraph-rust-exploration-rate", os.Getenv("VEGETA_S3_RUST_ACG_EXPLORATION_RATE"), "override Rust ACG deterministic exploration fraction; blank keeps Rust default")
	rustExplorationRiskBudget := flag.String("symbgraph-rust-exploration-risk-budget", os.Getenv("VEGETA_S3_RUST_ACG_EXPLORATION_RISK_BUDGET"), "override Rust ACG exploration risk budget; blank keeps Rust default")
	rustExplorationMinUncertainty := flag.String("symbgraph-rust-exploration-min-uncertainty", os.Getenv("VEGETA_S3_RUST_ACG_EXPLORATION_MIN_UNCERTAINTY"), "override minimum uncertainty for exploration; blank keeps Rust default")
	rustExplorationMaxTransactions := flag.String("symbgraph-rust-exploration-max-transactions", os.Getenv("VEGETA_S3_RUST_ACG_EXPLORATION_MAX_TRANSACTIONS"), "override maximum exploration transactions per block; blank keeps Rust default")
	rustIndependenceBeforeSoftening := flag.String("symbgraph-rust-independence-before-softening", os.Getenv("VEGETA_S3_RUST_ACG_INDEPENDENCE_BEFORE_SOFTENING"), "override independent observations required before hard-to-soft demotion; blank keeps Rust default")
	rustSofteningMinConfidence := flag.String("symbgraph-rust-softening-min-confidence", os.Getenv("VEGETA_S3_RUST_ACG_SOFTENING_MIN_CONFIDENCE"), "override minimum confidence for hard-to-soft demotion; blank keeps Rust default")
	rustACGOnly := flag.Bool("rust-acg-only", boolEnv("VEGETA_S3_RUST_ACG_ONLY"), "run only matched direct-serial + Rust ACG rows (skip BlockSTM and Vegeta) for policy diagnostics")
	rustDependencyDiagnostics := flag.Bool("symbgraph-rust-dependency-diagnostics", boolEnv("VEGETA_S3_RUST_ACG_DEPENDENCY_DIAGNOSTICS"), "emit dependency/critical-path reason metadata from Rust; disabled by default to avoid perturbing publication timing")
	investigateOverhead := flag.Bool(
		"investigate-overhead",
		boolEnv("VEGETA_S3_WASMD_INVESTIGATE"),
		"run unmeasured Wasmd serial controls that isolate extra-cache and access-tracking overhead",
	)
	diagnosticsOutput := flag.String(
		"diagnostics-output",
		os.Getenv("VEGETA_S3_WASMD_DIAGNOSTICS_OUTPUT"),
		"write Wasmd overhead diagnostics JSON (default: <output>.overhead.json)",
	)
	flag.Parse()
	rustRunnerOptions, rustOptionsErr := (RustSymbGraphRunnerOptions{
		Visibility: *rustVisibility,
		Validation: *rustValidation,
		Feedback:   *rustFeedback,
	}).Normalize()
	if rustOptionsErr != nil {
		panic(rustOptionsErr)
	}
	rustPlanningOverrides, rustPlanningErr := rustPlanningOverridesFromStrings(
		*rustEdgeMaterialization,
		*rustSoftThreshold,
		*rustHardThreshold,
		*rustRiskBudget,
		*rustExplorationRate,
		*rustExplorationRiskBudget,
		*rustExplorationMinUncertainty,
		*rustExplorationMaxTransactions,
		*rustIndependenceBeforeSoftening,
		*rustSofteningMinConfidence,
	)
	if rustPlanningErr != nil {
		panic(rustPlanningErr)
	}
	setupSDKConfig()
	if *calOnly {
		fmt.Printf("%.9f\n", calibrateIterationsPerNano())
		return
	}
	if *manifestPath == "" || *planPath == "" || *workers < 1 || *samples < 1 {
		flag.Usage()
		os.Exit(2)
	}
	if !*setupOnly && *weightsPath == "" {
		flag.Usage()
		os.Exit(2)
	}
	if !*setupOnly && *profileOnlyRunner == "" && *output == "" {
		flag.Usage()
		os.Exit(2)
	}
	if *profileOnlyRunner != "" && *profileOutputDir == "" {
		flag.Usage()
		os.Exit(2)
	}
	if *investigateOverhead && *diagnosticsOutput == "" && *output != "" {
		*diagnosticsOutput = *output + ".overhead.json"
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
	// The publication SymbGraph row is planned entirely by crates/acg-* through
	// the Rust FFI. The legacy Go symbolic predictor is loaded only for the
	// explicitly requested symbgraph-static diagnostic profiler.
	var symbolicAccesses symbolicAccessIndex
	staticProfileRequested := *profileOnlyRunner == "symbgraph-static" || (*workers == 2 && *symbProfileDir != "")
	if staticProfileRequested {
		symbolicPredictor, err := loadSymbolicPredictor(*repoRoot, *symbolicDir)
		if err != nil {
			panic(err)
		}
		symbolicAccesses = buildSymbolicAccessIndex(symbolicPredictor, blocks)
	}
	if *profileOnlyRunner != "" {
		if e := profileWasmdRunner(*repoRoot, manifest, blocks, cal, symbolicAccesses, *profileOnlyRunner, *profileOnlyKind, *workers, *profileOutputDir); e != nil {
			panic(e)
		}
		return
	}
	f, e := os.Create(*output)
	if e != nil {
		panic(e)
	}
	defer f.Close()
	w := bufio.NewWriter(f)
	defer w.Flush()
	fmt.Fprintf(os.Stderr, "wasmd scheduler matrix sdk=%s wasmd=%s workers=%d samples=%d go-iter/ns=%.6f symbolic=rust-acg:%s variant=%s rust-only=%v\n", cosmosSDKVersion, wasmdVersion, *workers, *samples, *iterPerNs, resolveRepoPath(*repoRoot, *symbolicDir), rustRunnerOptions.Variant(), *rustACGOnly)
	var diagnosticDirectNanos uint64
	var diagnosticSerialDigests [][32]byte
	if *investigateOverhead {
		diagnosticSerialDigests = make([][32]byte, len(blocks))
	}
	for sample := 0; sample < *samples; sample++ {
		rustBridge, e := NewRustSymbGraphBridgeWithPlanningAndDiagnostics(*repoRoot, *symbolicDir, rustPlanningOverrides, *rustDependencyDiagnostics)
		if e != nil {
			panic(e)
		}
		serial, e := newBenchApp(*repoRoot, manifest, blocks)
		if e != nil {
			rustBridge.Close()
			panic(e)
		}
		stm, e := newBenchApp(*repoRoot, manifest, blocks)
		if e != nil {
			serial.close()
			rustBridge.Close()
			panic(e)
		}
		aria, e := newBenchApp(*repoRoot, manifest, blocks)
		if e != nil {
			serial.close()
			stm.close()
			rustBridge.Close()
			panic(e)
		}
		symb, e := newBenchApp(*repoRoot, manifest, blocks)
		if e != nil {
			serial.close()
			stm.close()
			aria.close()
			rustBridge.Close()
			panic(e)
		}
		vegeta, e := newBenchApp(*repoRoot, manifest, blocks)
		if e != nil {
			serial.close()
			stm.close()
			aria.close()
			symb.close()
			rustBridge.Close()
			panic(e)
		}
		ariaReference, e := newBenchApp(*repoRoot, manifest, blocks)
		if e != nil {
			serial.close()
			stm.close()
			aria.close()
			symb.close()
			vegeta.close()
			rustBridge.Close()
			panic(e)
		}
		vegetaReference, e := newBenchApp(*repoRoot, manifest, blocks)
		if e != nil {
			serial.close()
			stm.close()
			aria.close()
			symb.close()
			vegeta.close()
			ariaReference.close()
			rustBridge.Close()
			panic(e)
		}

		func() {
			defer rustBridge.Close()
			defer serial.close()
			defer stm.close()
			defer aria.close()
			defer symb.close()
			defer vegeta.close()
			defer ariaReference.close()
			defer vegetaReference.close()

			const preEstimate = false
			blockSTMRunner := txnrunner.NewSTMRunner(
				sdk.TxDecoder(func([]byte) (sdk.Tx, error) { return nil, nil }),
				stm.app.GetStoreKeys(),
				*workers,
				preEstimate,
				func(storetypes.MultiStore) string { return sdk.DefaultBondDenom },
			)
			ariaRunner := NewAriaFBRunner(*workers)
			vegetaRunner := NewVegetaRunner(*workers)

			for blockOffset, block := range blocks {
				header := tmproto.Header{ChainID: chainID, Height: int64(blockOffset + 2), Time: time.Unix(int64(block.Timestamp), 0)}

				// Historical-order direct Wasmd serial is the common throughput control.
				// Serial, BlockSTM, and Rust-ACG also use it as their correctness oracle.
				// Vegeta and AriaFB time and verify independent serial references for
				// their own legal derived serialization orders below.
				serialCtx := serial.app.NewNextBlockContext(header)
				serialStart := time.Now()
				for _, tx := range block.Transactions {
					if e := serial.executeTx(serialCtx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); e != nil {
						panic(e)
					}
				}
				serialWall := time.Since(serialStart)
				if e := commitFinalizeState(serial.app); e != nil {
					panic(e)
				}
				serialNanos := uint64(serialWall.Nanoseconds())
				serialDigest := digestApp(serial.app)
				if *investigateOverhead && sample == 0 {
					diagnosticDirectNanos += serialNanos
					diagnosticSerialDigests[blockOffset] = serialDigest
				}
				serialRec := Record{
					SchemaVersion:         1,
					Dataset:               "vegeta-s3-wasmd-blockstm",
					Sample:                sample,
					BlockNumber:           block.BlockNumber,
					Strategy:              "cosmos-wasmd-direct-serial",
					Workers:               *workers,
					MatchedSerialNanos:    serialNanos,
					HistoricalSerialNanos: serialNanos,
					StrategyTotalNanos:    serialNanos,
					PostConsensusNanos:    serialNanos,
					MatchedSerialSpeedup:  1,
					Transactions:          len(block.Transactions),
					ExecutionAttempts:     uint64(len(block.Transactions)),
					Reexecutions:          0,
					SerialEquivalent:      true,
					SerialReferenceScope:  "historical-block-order",
					ComputeMetric:         "steps",
					ComputeScale:          *scale,
					GoIterationsPerNano:   *iterPerNs,
					CosmosSDKVersion:      cosmosSDKVersion,
					WasmdVersion:          wasmdVersion,
					BaselineScope:         directSerialScope,
					BlockSTMPreEstimate:   false,
				}
				if e := json.NewEncoder(w).Encode(&serialRec); e != nil {
					panic(e)
				}

				if !*rustACGOnly {
					// Cosmos SDK Block-STM: native SDK MVCC/scheduler baseline.
					stmBlockCtx := stm.app.NewNextBlockContext(header)
					var stmAttempts atomic.Uint64
					stmStart := time.Now()
					_, e := blockSTMRunner.Run(context.Background(), stmBlockCtx.MultiStore(), txBytes(block), func(_ []byte, _ sdk.Tx, ms storetypes.MultiStore, idx int, _ map[string]any) *abci.ExecTxResult {
						stmAttempts.Add(1)
						ctx := stmBlockCtx.WithMultiStore(ms).WithEventManager(sdk.NewEventManager()).WithGasMeter(storetypes.NewInfiniteGasMeter())
						tx := block.Transactions[idx]
						if e := stm.executeTx(ctx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); e != nil {
							return &abci.ExecTxResult{Code: 1, Log: e.Error()}
						}
						return &abci.ExecTxResult{}
					})
					stmWall := time.Since(stmStart)
					if e != nil {
						panic(e)
					}
					if e := commitFinalizeState(stm.app); e != nil {
						panic(e)
					}
					stmEq := serialDigest == digestApp(stm.app)
					if !stmEq {
						panic(fmt.Sprintf("block-stm state mismatch sample=%d block=%d", sample, block.BlockNumber))
					}
					stmA := stmAttempts.Load()
					stmRec := Record{SchemaVersion: 1, Dataset: "vegeta-s3-wasmd-blockstm", Sample: sample, BlockNumber: block.BlockNumber, Strategy: "cosmos-wasmd-block-stm", Workers: *workers, MatchedSerialNanos: serialNanos, HistoricalSerialNanos: serialNanos, StrategyTotalNanos: uint64(stmWall.Nanoseconds()), PostConsensusNanos: uint64(stmWall.Nanoseconds()), Transactions: len(block.Transactions), ExecutionAttempts: stmA, Reexecutions: stmA - uint64(len(block.Transactions)), SerialEquivalent: stmEq, SerialReferenceScope: "historical-block-order", ComputeMetric: "steps", ComputeScale: *scale, GoIterationsPerNano: *iterPerNs, CosmosSDKVersion: cosmosSDKVersion, WasmdVersion: wasmdVersion, BaselineScope: baselineScope, BlockSTMPreEstimate: preEstimate}
					if stmRec.StrategyTotalNanos > 0 {
						stmRec.MatchedSerialSpeedup = float64(stmRec.MatchedSerialNanos) / float64(stmRec.StrategyTotalNanos)
					}
					if e := json.NewEncoder(w).Encode(&stmRec); e != nil {
						panic(e)
					}

				}

				if !*rustACGOnly {
					// AriaFB on the same Wasmd/WasmVM state machine. The initial Aria
					// batch executes after consensus from one block-start snapshot. Rule-2
					// survivors commit in a valid Aria serialization order; aborts use the
					// attached Vegeta repository's hot-chain dependency-DAG fallback.
					ariaBlockCtx := aria.app.NewNextBlockContext(header)
					ariaStart := time.Now()
					_, e = ariaRunner.Run(context.Background(), ariaBlockCtx.MultiStore(), txBytes(block), func(_ []byte, _ sdk.Tx, ms storetypes.MultiStore, idx int, _ map[string]any) *abci.ExecTxResult {
						ctx := ariaBlockCtx.WithMultiStore(ms).WithEventManager(sdk.NewEventManager()).WithGasMeter(storetypes.NewInfiniteGasMeter())
						tx := block.Transactions[idx]
						if e := aria.executeTxIsolated(ctx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); e != nil {
							return &abci.ExecTxResult{Code: 1, Log: e.Error()}
						}
						return &abci.ExecTxResult{}
					})
					ariaWall := time.Since(ariaStart)
					if e != nil {
						panic(e)
					}
					if e := commitFinalizeState(aria.app); e != nil {
						panic(e)
					}
					ariaSerialNanos, e := executeSerialOrderReference(ariaReference, header, block, cal, ariaRunner.LastSerializationOrder())
					if e != nil {
						panic(e)
					}
					ariaEq := digestApp(ariaReference.app) == digestApp(aria.app)
					if !ariaEq {
						panic(fmt.Sprintf("aria-fb serialization mismatch sample=%d block=%d order=%v", sample, block.BlockNumber, ariaRunner.LastSerializationOrder()))
					}
					ariaStats := ariaRunner.LastStats()
					ariaRec := Record{
						SchemaVersion: 1, Dataset: "vegeta-s3-wasmd-blockstm", Sample: sample, BlockNumber: block.BlockNumber, Strategy: "cosmos-wasmd-aria-fb", Workers: *workers,
						MatchedSerialNanos: ariaSerialNanos, HistoricalSerialNanos: serialNanos, StrategyTotalNanos: uint64(ariaWall.Nanoseconds()), PostConsensusNanos: ariaStats.PostConsensusNanos, Transactions: len(block.Transactions),
						ExecutionAttempts: ariaStats.Attempts, Reexecutions: ariaStats.Reexecutions, SerialEquivalent: ariaEq, SerialReferenceScope: "aria-derived-serialization", ComputeMetric: "steps", ComputeScale: *scale,
						GoIterationsPerNano: *iterPerNs, CosmosSDKVersion: cosmosSDKVersion, WasmdVersion: wasmdVersion, BaselineScope: ariaFBScope, BlockSTMPreEstimate: false,
						SpeculatedTransactions: ariaStats.Speculated, ReusedTransactions: ariaStats.Reused, ValidationNanos: ariaStats.ValidationNanos, ReplayExecutionNanos: ariaStats.ReplayExecutionNanos,
						ConflictAnalysisNanos: ariaStats.ConflictAnalysisNanos, DiscoveredConflicts: ariaStats.DiscoveredConflicts, ForwardFallbacks: ariaStats.ForwardFallbacks, SafetyReplays: ariaStats.SafetyReplays,
					}
					if ariaRec.StrategyTotalNanos > 0 {
						ariaRec.MatchedSerialSpeedup = float64(ariaRec.MatchedSerialNanos) / float64(ariaRec.StrategyTotalNanos)
					}
					if e := json.NewEncoder(w).Encode(&ariaRec); e != nil {
						panic(e)
					}
				}

				// SymbGraph Rust: crates/acg-* owns symbolic parsing, candidate graph
				// construction, conflict predicates, adaptive feedback, and risk-bounded
				// ordering. Go executes only the emitted ordering_dependencies DAG against
				// real Wasmd/WasmVM state and performs concrete validation/replay.
				symbBlockCtx := symb.app.NewNextBlockContext(header)
				estimatedCosts := make([]uint32, len(block.Transactions))
				for i, tx := range block.Transactions {
					estimatedCosts[i] = boundedCost(cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash))
				}
				symbRunner := NewRustSymbGraphRunnerWithOptions(*workers, block, rustBridge, estimatedCosts, rustRunnerOptions)
				symbRunner.SetSerialServiceNanos(serialNanos)
				symbStart := time.Now()
				_, e = symbRunner.Run(context.Background(), symbBlockCtx.MultiStore(), txBytes(block), func(_ []byte, _ sdk.Tx, ms storetypes.MultiStore, idx int, _ map[string]any) *abci.ExecTxResult {
					ctx := symbBlockCtx.WithMultiStore(ms).WithEventManager(sdk.NewEventManager()).WithGasMeter(storetypes.NewInfiniteGasMeter())
					tx := block.Transactions[idx]
					if e := symb.executeTxIsolated(ctx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); e != nil {
						return &abci.ExecTxResult{Code: 1, Log: e.Error()}
					}
					return &abci.ExecTxResult{}
				})
				symbWall := time.Since(symbStart)
				if e != nil {
					panic(e)
				}
				if e := commitFinalizeState(symb.app); e != nil {
					panic(e)
				}
				symbEq := serialDigest == digestApp(symb.app)
				if !symbEq {
					panic(fmt.Sprintf("symbgraph-rust state mismatch sample=%d block=%d", sample, block.BlockNumber))
				}
				symbStats := symbRunner.LastStats()
				symbDiag := symbRunner.LastDiagnostics()
				symbPlan := symbRunner.LastPlan()
				symbRec := Record{
					SchemaVersion: 1, Dataset: "vegeta-s3-wasmd-blockstm", Sample: sample, BlockNumber: block.BlockNumber,
					Strategy: "cosmos-wasmd-symbgraph-rust", Workers: *workers, MatchedSerialNanos: serialNanos, HistoricalSerialNanos: serialNanos,
					StrategyTotalNanos: uint64(symbWall.Nanoseconds()), PreConsensusNanos: symbStats.PreConsensusNanos, PostConsensusNanos: symbStats.PostConsensusNanos, Transactions: len(block.Transactions), ExecutionAttempts: symbStats.Attempts,
					Reexecutions: symbStats.Reexecutions, SerialEquivalent: symbEq, SerialReferenceScope: "historical-block-order", ComputeMetric: "steps", ComputeScale: *scale,
					GoIterationsPerNano: *iterPerNs, CosmosSDKVersion: cosmosSDKVersion, WasmdVersion: wasmdVersion,
					BaselineScope: rustRunnerOptions.Scope(), BlockSTMPreEstimate: false, SpeculatedTransactions: symbStats.Speculated,
					ReusedTransactions: symbStats.Reused, ValidationNanos: symbDiag.ValidationNanos, ReplayExecutionNanos: symbDiag.ReplayExecutionNanos,
					SymbGraphVariant: symbDiag.Variant, SymbPlanNanos: symbDiag.PlanNanos, SymbPreexecutionNanos: symbDiag.PreexecutionNanos,
					SymbBranchCreateNanos: symbDiag.BranchCreateNanos, SymbVisibilityNanos: symbDiag.VisibilityNanos,
					SymbSpecExecutionNanos: symbDiag.SpecExecutionNanos, SymbDeltaCaptureNanos: symbDiag.DeltaCaptureNanos,
					SymbMVCCPublishNanos: symbDiag.MVCCPublishNanos, SymbFeedbackBuildNanos: symbDiag.FeedbackBuildNanos,
					SymbReconciliationNanos: symbDiag.ReconciliationNanos, SymbValidationNanos: symbDiag.ValidationNanos,
					SymbReplayExecutionNanos: symbDiag.ReplayExecutionNanos, SymbRustFeedbackNanos: symbDiag.RustFeedbackNanos,
					SymbDependencyEdges: symbDiag.DependencyEdges, SymbFeedbackPairs: symbDiag.FeedbackPairs,
					SymbPhysicalCandidateEdges: symbDiag.PhysicalCandidateEdges, SymbLogicalCandidateEdges: symbDiag.LogicalCandidateEdges, SymbCompactCandidateGroups: symbDiag.CompactCandidateGroups,
					SymbParentDependenciesBeforeReduction: symbDiag.ParentDependenciesBeforeReduction, SymbParentDependenciesElidedReduction: symbDiag.ParentDependenciesElidedReduction,
					SymbInitialReady: symbDiag.InitialReady, SymbMaxReady: symbDiag.MaxReady, SymbAverageReady: symbDiag.AverageReady(),
					SymbMaxActive: symbDiag.MaxActive, SymbCriticalPathTx: symbDiag.CriticalPathTx, SymbCriticalPathCost: symbDiag.CriticalPathCost,
					SymbTotalEstimatedCost: symbDiag.TotalEstimatedCost, SymbDAGParallelism: symbDiag.DAGParallelism,
					SymbWorkerUtilization: symbDiag.WorkerUtilization, SymbWorkerIdleNanos: symbDiag.WorkerIdleNanos,
					SymbMVCCPointReads: symbDiag.MVCCPointReads, SymbMVCCVersionHits: symbDiag.MVCCVersionHits,
					SymbMVCCBaseFallbacks: symbDiag.MVCCBaseFallbacks, SymbMVCCRangeReads: symbDiag.MVCCRangeReads,
					SymbMVCCRangeOverlayKeys: symbDiag.MVCCRangeOverlayKeys, SymbMVCCPublishes: symbDiag.MVCCPublishes,
					SymbMVCCPublishedKeys: symbDiag.MVCCPublishedKeys,
					SymbPlanning:          symbPlan.Planning, SymbDependencyReasons: symbDiag.DependencyReasons, SymbDependencyPrimary: symbDiag.DependencyPrimary,
					SymbCriticalPath: symbDiag.CriticalPath, SymbCriticalPathReasons: symbDiag.CriticalPathReasons, SymbCriticalPathCostByReason: symbDiag.CriticalPathCostByReason,
					SymbDependencyProvenance: symbDiag.DependencyProvenance, SymbDependencyDecisions: symbDiag.DependencyDecisions,
					SymbCriticalPathProvenance: symbDiag.CriticalPathProvenance, SymbCriticalPathDecisions: symbDiag.CriticalPathDecisions,
					SymbCandidateHard: symbDiag.CandidateHard, SymbCandidateSoft: symbDiag.CandidateSoft, SymbCandidateLow: symbDiag.CandidateLow,
					SymbOrderedHard: symbDiag.OrderedHard, SymbOrderedSoft: symbDiag.OrderedSoft,
					SymbOracleConflictEdges: symbDiag.OracleConflictEdges, SymbOracleCriticalPathTx: symbDiag.OracleCriticalPathTx,
					SymbOracleCriticalPathCost: symbDiag.OracleCriticalPathCost, SymbOracleDAGParallelism: symbDiag.OracleDAGParallelism,
					SymbOracleCriticalPath: symbDiag.OracleCriticalPath, SymbSerializationGap: symbDiag.SerializationGap,
					SymbPlanRequestBuildNanos: symbDiag.PlanRequestBuildNanos, SymbPlanRequestMarshalNanos: symbDiag.PlanRequestMarshalNanos,
					SymbPlanCGORoundTripNanos: symbDiag.PlanCGORoundTripNanos, SymbPlanResponseUnmarshalNanos: symbDiag.PlanResponseUnmarshalNanos,
					SymbPlanRustDecodeNanos: symbDiag.PlanRustDecodeNanos, SymbPlanResolveComponentsNanos: symbDiag.PlanResolveComponentsNanos,
					SymbPlanCandidateGraphNanos: symbDiag.PlanCandidateGraphNanos, SymbPlanSchedulerNanos: symbDiag.PlanSchedulerNanos,
					SymbPlanProjectionNanos: symbDiag.PlanProjectionNanos, SymbPlanFeedbackPairsNanos: symbDiag.PlanFeedbackPairsNanos,
					SymbPlanFinalizeNanos: symbDiag.PlanFinalizeNanos, SymbPlanBridgeOtherNanos: symbDiag.PlanBridgeOtherNanos,
				}
				if symbRec.StrategyTotalNanos > 0 {
					symbRec.MatchedSerialSpeedup = float64(symbRec.MatchedSerialNanos) / float64(symbRec.StrategyTotalNanos)
				}
				if e := json.NewEncoder(w).Encode(&symbRec); e != nil {
					panic(e)
				}

				if !*rustACGOnly {
					// Vegeta SpeculateMod + ParallelMod adaptation: pre-consensus
					// execution discovers actual accesses and a hot-key proposal reorder;
					// after consensus every transaction executes in Rule-2-compatible DAG
					// batches and only unsafe access-set changes are executed again.
					vegetaBlockCtx := vegeta.app.NewNextBlockContext(header)
					vegetaStart := time.Now()
					_, e = vegetaRunner.Run(context.Background(), vegetaBlockCtx.MultiStore(), txBytes(block), func(_ []byte, _ sdk.Tx, ms storetypes.MultiStore, idx int, _ map[string]any) *abci.ExecTxResult {
						ctx := vegetaBlockCtx.WithMultiStore(ms).WithEventManager(sdk.NewEventManager()).WithGasMeter(storetypes.NewInfiniteGasMeter())
						tx := block.Transactions[idx]
						if e := vegeta.executeTxIsolated(ctx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); e != nil {
							return &abci.ExecTxResult{Code: 1, Log: e.Error()}
						}
						return &abci.ExecTxResult{}
					})
					vegetaWall := time.Since(vegetaStart)
					if e != nil {
						panic(e)
					}
					if e := commitFinalizeState(vegeta.app); e != nil {
						panic(e)
					}
					vegetaSerialNanos, e := executeSerialOrderReference(vegetaReference, header, block, cal, vegetaRunner.LastSerializationOrder())
					if e != nil {
						panic(e)
					}
					vegetaEq := digestApp(vegetaReference.app) == digestApp(vegeta.app)
					if !vegetaEq {
						panic(fmt.Sprintf("vegeta serialization mismatch sample=%d block=%d proposal=%v serialization=%v", sample, block.BlockNumber, vegetaRunner.LastProposalOrder(), vegetaRunner.LastSerializationOrder()))
					}
					vegetaStats := vegetaRunner.LastStats()
					vegetaRec := Record{
						SchemaVersion: 1, Dataset: "vegeta-s3-wasmd-blockstm", Sample: sample, BlockNumber: block.BlockNumber, Strategy: "cosmos-wasmd-vegeta", Workers: *workers,
						MatchedSerialNanos: vegetaSerialNanos, HistoricalSerialNanos: serialNanos, StrategyTotalNanos: uint64(vegetaWall.Nanoseconds()), PreConsensusNanos: vegetaStats.PreConsensusNanos, PostConsensusNanos: vegetaStats.PostConsensusNanos, Transactions: len(block.Transactions),
						ExecutionAttempts: vegetaStats.Attempts, Reexecutions: vegetaStats.Reexecutions, SerialEquivalent: vegetaEq, SerialReferenceScope: "vegeta-derived-serialization", ComputeMetric: "steps", ComputeScale: *scale,
						GoIterationsPerNano: *iterPerNs, CosmosSDKVersion: cosmosSDKVersion, WasmdVersion: wasmdVersion, BaselineScope: vegetaScope, BlockSTMPreEstimate: false,
						SpeculatedTransactions: vegetaStats.Speculated, ReusedTransactions: vegetaStats.Reused, ValidationNanos: vegetaStats.ValidationNanos, ReplayExecutionNanos: vegetaStats.ReplayExecutionNanos,
						ConflictAnalysisNanos: vegetaStats.ConflictAnalysisNanos, DiscoveredConflicts: vegetaStats.DiscoveredConflicts, ForwardFallbacks: vegetaStats.ForwardFallbacks, SafetyReplays: vegetaStats.SafetyReplays,
					}
					if vegetaRec.StrategyTotalNanos > 0 {
						vegetaRec.MatchedSerialSpeedup = float64(vegetaRec.MatchedSerialNanos) / float64(vegetaRec.StrategyTotalNanos)
					}
					if e := json.NewEncoder(w).Encode(&vegetaRec); e != nil {
						panic(e)
					}
				}

			}
		}()
	}

	if *investigateOverhead {
		if e := w.Flush(); e != nil {
			panic(e)
		}
		fmt.Fprintln(os.Stderr, "running unmeasured Wasmd overhead controls (not publication timing)")
		diag, e := runWasmdOverheadDiagnostics(
			*repoRoot,
			manifest,
			blocks,
			cal,
			*workers,
			diagnosticDirectNanos,
			diagnosticSerialDigests,
		)
		if e != nil {
			panic(e)
		}
		if e := writeWasmdOverheadDiagnostics(*diagnosticsOutput, diag); e != nil {
			panic(e)
		}
		fmt.Fprintf(
			os.Stderr,
			"WASMD-OVERHEAD direct=%.3fs outer-cache=%.3fs (%.3fx direct) tracked-outer-cache=%.3fs (%.3fx direct; %.3fx outer-cache) tracked-single-cache=%.3fs (%.3fx direct) output=%s\n",
			float64(diag.DirectSerialNanos)/1e9,
			float64(diag.OuterCacheSerial.ActiveNanos)/1e9,
			diag.OuterCacheVsDirect,
			float64(diag.TrackedOuterCacheSerial.ActiveNanos)/1e9,
			diag.TrackedVsDirect,
			diag.TrackedVsOuterCache,
			float64(diag.TrackedSingleCacheSerial.ActiveNanos)/1e9,
			diag.TrackedSingleCacheVsDirect,
			*diagnosticsOutput,
		)
		fmt.Fprintf(
			os.Stderr,
			"WASMD-OVERHEAD-PHASE outer-cache branch=%.3fs execute=%.3fs write=%.3fs | tracked-outer branch=%.3fs execute=%.3fs write=%.3fs | tracked-single branch=%.3fs execute=%.3fs write=%.3fs\n",
			float64(diag.OuterCacheSerial.BranchNanos)/1e9,
			float64(diag.OuterCacheSerial.ExecuteNanos)/1e9,
			float64(diag.OuterCacheSerial.WriteNanos)/1e9,
			float64(diag.TrackedOuterCacheSerial.BranchNanos)/1e9,
			float64(diag.TrackedOuterCacheSerial.ExecuteNanos)/1e9,
			float64(diag.TrackedOuterCacheSerial.WriteNanos)/1e9,
			float64(diag.TrackedSingleCacheSerial.BranchNanos)/1e9,
			float64(diag.TrackedSingleCacheSerial.ExecuteNanos)/1e9,
			float64(diag.TrackedSingleCacheSerial.WriteNanos)/1e9,
		)

	}

	// SymbGraph tracker profiling is also a separate replay. CPU profiling covers
	// only the 101-block scheduler execution; allocation counters are deltas from
	// runtime.MemStats around that same interval.
	if *workers == 2 && *symbProfileDir != "" {
		if e := w.Flush(); e != nil {
			panic(e)
		}
		fmt.Fprintf(os.Stderr, "profiling unmeasured Wasmd SymbGraph workers=2 -> %s\n", *symbProfileDir)
		if e := profileWasmdSymbGraph2(*repoRoot, manifest, blocks, cal, symbolicAccesses, *symbProfileDir); e != nil {
			panic(e)
		}
	}

	// Profiling is deliberately a separate replay after all benchmark records
	// have been written, so pprof instrumentation cannot contaminate publication
	// timings. Set --profile-dir or VEGETA_S3_WASMD_PPROF_DIR; only workers=4 runs it.
	if *workers == 4 && *profileDir != "" {
		if e := w.Flush(); e != nil {
			panic(e)
		}
		fmt.Fprintf(os.Stderr, "profiling unmeasured Wasmd Block-STM workers=4 -> %s\n", *profileDir)
		if e := profileWasmdBlockSTM4(*repoRoot, manifest, blocks, cal, *profileDir); e != nil {
			panic(e)
		}
	}

}
