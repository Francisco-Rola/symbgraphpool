package main

import (
	"bufio"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"flag"
	"fmt"
	"math"
	"os"
	"runtime"
	"sync/atomic"
	"time"

	log "cosmossdk.io/log/v2"
	abci "github.com/cometbft/cometbft/abci/types"
	dbm "github.com/cosmos/cosmos-db"
	"github.com/cosmos/cosmos-sdk/baseapp/txnrunner"
	store "github.com/cosmos/cosmos-sdk/store/v2"
	storetypes "github.com/cosmos/cosmos-sdk/store/v2/types"
	sdk "github.com/cosmos/cosmos-sdk/types"
)

const cosmosSDKVersion = "v0.54.4"

type Access struct {
	Kind        string  `json:"kind"`
	Contract    string  `json:"contract"`
	KeyHex      string  `json:"key_hex"`
	RangeEndHex *string `json:"range_end_hex"`
	Reverted    bool    `json:"reverted"`
}

type Tx struct {
	TxIndex           int      `json:"tx_index"`
	TxHash            string   `json:"tx_hash"`
	SourceFailed      bool     `json:"source_failed"`
	ComputeIterations uint64   `json:"compute_iterations"`
	Accesses          []Access `json:"accesses"`
	GoIterations      uint64   `json:"-"`
}

type Block struct {
	BlockNumber              uint64  `json:"block_number"`
	ComputeMetric            string  `json:"compute_calibration_metric"`
	ComputeScale             float64 `json:"compute_scale"`
	ComputeBaseTotalNanos    uint64  `json:"compute_base_total_nanos"`
	ComputeIterationsPerNano float64 `json:"compute_iterations_per_nano"`
	Transactions             []Tx    `json:"transactions"`
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
	BaselineScope        string  `json:"baseline_scope"`
	BlockSTMPreEstimate  bool    `json:"block_stm_pre_estimate"`
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
	iterations := uint64(2_000_000)
	var elapsed time.Duration
	for {
		start := time.Now()
		sink := deterministicCompute(iterations)
		runtime.KeepAlive(sink)
		elapsed = time.Since(start)
		if elapsed >= 100*time.Millisecond || iterations >= 1_000_000_000 {
			break
		}
		iterations *= 2
	}
	if elapsed <= 0 {
		return 1
	}
	return float64(iterations) / float64(elapsed.Nanoseconds())
}

func readBlocks(path string) ([]Block, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 64*1024), 64*1024*1024)
	var blocks []Block
	for scanner.Scan() {
		if len(scanner.Bytes()) == 0 {
			continue
		}
		var b Block
		if err := json.Unmarshal(scanner.Bytes(), &b); err != nil {
			return nil, err
		}
		blocks = append(blocks, b)
	}
	return blocks, scanner.Err()
}

func calibrateBlocks(blocks []Block, goIterPerNs float64) {
	var sourceIterations uint64
	var baseNanos uint64
	var scale float64
	for bi := range blocks {
		if bi == 0 {
			baseNanos = blocks[bi].ComputeBaseTotalNanos
			scale = blocks[bi].ComputeScale
		}
		for ti := range blocks[bi].Transactions {
			sourceIterations += blocks[bi].Transactions[ti].ComputeIterations
		}
	}
	if sourceIterations == 0 || scale <= 0 || baseNanos == 0 {
		return
	}
	target := float64(baseNanos) * scale * goIterPerNs
	for bi := range blocks {
		for ti := range blocks[bi].Transactions {
			share := float64(blocks[bi].Transactions[ti].ComputeIterations) / float64(sourceIterations)
			n := share * target
			if n > float64(math.MaxUint64) {
				n = float64(math.MaxUint64)
			}
			blocks[bi].Transactions[ti].GoIterations = uint64(math.Round(n))
		}
	}
}

func decodeHex(s string) []byte {
	b, err := hex.DecodeString(s)
	if err != nil {
		panic(err)
	}
	return b
}

func scopedKey(a Access, raw []byte) []byte {
	prefix := byte(1)
	switch a.Kind {
	case "bank_read", "bank_write":
		prefix = 2
	}
	contract := []byte(a.Contract)
	out := make([]byte, 0, 1+len(contract)+1+len(raw))
	out = append(out, prefix)
	out = append(out, contract...)
	out = append(out, 0)
	out = append(out, raw...)
	return out
}

func applyTx(tx *Tx, ms storetypes.MultiStore, key storetypes.StoreKey) {
	if tx.GoIterations > 0 {
		sink := deterministicCompute(tx.GoIterations)
		runtime.KeepAlive(sink)
	}
	kv := ms.GetKVStore(key)
	var value [16]byte
	binary.LittleEndian.PutUint64(value[:8], uint64(tx.TxIndex))
	binary.LittleEndian.PutUint64(value[8:], uint64(len(tx.Accesses)))
	for _, a := range tx.Accesses {
		raw := decodeHex(a.KeyHex)
		k := scopedKey(a, raw)
		switch a.Kind {
		case "storage_read", "bank_read":
			_ = kv.Get(k)
		case "storage_scan":
			var end []byte
			if a.RangeEndHex != nil {
				end = scopedKey(a, decodeHex(*a.RangeEndHex))
			}
			it := kv.Iterator(k, end)
			for ; it.Valid(); it.Next() {
				_ = it.Key()
				_ = it.Value()
			}
			it.Close()
		case "storage_write", "bank_write":
			if !a.Reverted {
				kv.Set(k, value[:])
			}
		case "storage_remove":
			if !a.Reverted {
				kv.Delete(k)
			}
		default:
			panic("unknown access kind: " + a.Kind)
		}
	}
}

func digestStore(ms storetypes.MultiStore, key storetypes.StoreKey) [32]byte {
	kv := ms.GetKVStore(key)
	it := kv.Iterator(nil, nil)
	defer it.Close()
	h := sha256.New()
	for ; it.Valid(); it.Next() {
		k := append([]byte(nil), it.Key()...)
		v := append([]byte(nil), it.Value()...)
		var n [8]byte
		binary.LittleEndian.PutUint64(n[:], uint64(len(k)))
		h.Write(n[:])
		h.Write(k)
		binary.LittleEndian.PutUint64(n[:], uint64(len(v)))
		h.Write(n[:])
		h.Write(v)
	}
	var out [32]byte
	copy(out[:], h.Sum(nil))
	return out
}

func executeSerial(b *Block, ms storetypes.MultiStore, key storetypes.StoreKey) time.Duration {
	start := time.Now()
	for i := range b.Transactions {
		applyTx(&b.Transactions[i], ms, key)
	}
	return time.Since(start)
}

func newBenchmarkMultiStore(key storetypes.StoreKey) storetypes.CommitMultiStore {
	cms := store.NewCommitMultiStore(dbm.NewMemDB(), log.NewNopLogger())
	cms.MountStoreWithDB(key, storetypes.StoreTypeDB, nil)
	if err := cms.LoadLatestVersion(); err != nil {
		panic(err)
	}
	return cms
}

func encodedTransactions(b *Block) [][]byte {
	txs := make([][]byte, len(b.Transactions))
	for i := range b.Transactions {
		var raw [8]byte
		binary.LittleEndian.PutUint64(raw[:], uint64(i))
		txs[i] = append([]byte(nil), raw[:]...)
	}
	return txs
}

func main() {
	input := flag.String("input", "", "native S3 access JSONL from acg-vegeta-native-s3-executor")
	output := flag.String("output", "", "output JSONL")
	workers := flag.Int("workers", 4, "Block-STM executors")
	samples := flag.Int("samples", 1, "independent full-range samples")
	goIterPerNs := flag.Float64("go-iterations-per-nano", 0, "pin Go compute calibration")
	calibrateOnly := flag.Bool("calibrate-only", false, "print Go deterministic-compute iterations/ns and exit")
	flag.Parse()
	if *calibrateOnly {
		fmt.Printf("%.9f\n", calibrateIterationsPerNano())
		return
	}
	if *input == "" || *output == "" || *workers <= 0 || *samples <= 0 {
		flag.Usage()
		os.Exit(2)
	}
	blocks, err := readBlocks(*input)
	if err != nil {
		panic(err)
	}
	if len(blocks) == 0 {
		panic("empty input")
	}
	if *goIterPerNs <= 0 {
		*goIterPerNs = calibrateIterationsPerNano()
	}
	calibrateBlocks(blocks, *goIterPerNs)
	fmt.Fprintf(os.Stderr, "cosmos-block-stm access replay sdk=%s workers=%d samples=%d go-iter/ns=%.6f\n", cosmosSDKVersion, *workers, *samples, *goIterPerNs)

	out, err := os.Create(*output)
	if err != nil {
		panic(err)
	}
	defer out.Close()
	w := bufio.NewWriter(out)
	defer w.Flush()

	storeKey := storetypes.NewKVStoreKey("s3")
	storeKeys := []storetypes.StoreKey{storeKey}
	// The frozen access-replay records are not encoded Cosmos SDK messages, so the
	// SDK's message-aware pre-estimator cannot be used here. The runner still uses
	// the actual Cosmos SDK Block-STM optimistic execution/validation path.
	const preEstimate = false
	txDecoder := sdk.TxDecoder(func([]byte) (sdk.Tx, error) { return nil, nil })
	runner := txnrunner.NewSTMRunner(
		txDecoder,
		storeKeys,
		*workers,
		preEstimate,
		func(storetypes.MultiStore) string { return sdk.DefaultBondDenom },
	)
	for sample := 0; sample < *samples; sample++ {
		serialState := newBenchmarkMultiStore(storeKey)
		stmState := newBenchmarkMultiStore(storeKey)
		for bi := range blocks {
			b := &blocks[bi]
			serialWall := executeSerial(b, serialState, storeKey)
			var attempts atomic.Uint64
			txBytes := encodedTransactions(b)
			started := time.Now()
			_, err := runner.Run(
				context.Background(),
				stmState,
				txBytes,
				func(_ []byte, _ sdk.Tx, ms storetypes.MultiStore, txIndex int, _ map[string]any) *abci.ExecTxResult {
					attempts.Add(1)
					applyTx(&b.Transactions[txIndex], ms, storeKey)
					return &abci.ExecTxResult{}
				},
			)
			wall := time.Since(started)
			if err != nil {
				panic(err)
			}
			serialDigest := digestStore(serialState, storeKey)
			stmDigest := digestStore(stmState, storeKey)
			eq := serialDigest == stmDigest
			if !eq {
				panic(fmt.Sprintf("state mismatch sample=%d block=%d", sample, b.BlockNumber))
			}
			attemptCount := attempts.Load()
			rec := Record{
				SchemaVersion: 1, Dataset: "vegeta-s3-cosmos-block-stm-access-replay",
				Sample: sample, BlockNumber: b.BlockNumber, Strategy: "cosmos-block-stm-access-replay",
				Workers: *workers, MatchedSerialNanos: uint64(serialWall.Nanoseconds()),
				StrategyTotalNanos: uint64(wall.Nanoseconds()), Transactions: len(b.Transactions),
				ExecutionAttempts: attemptCount,
				Reexecutions:      attemptCount - uint64(len(b.Transactions)), SerialEquivalent: eq,
				ComputeMetric: b.ComputeMetric, ComputeScale: b.ComputeScale,
				GoIterationsPerNano: *goIterPerNs, CosmosSDKVersion: cosmosSDKVersion,
				BaselineScope:       "actual-cosmos-sdk-txnrunner-blockstm-on-native-access-replay-not-cosmwasm-vm",
				BlockSTMPreEstimate: preEstimate,
			}
			if rec.StrategyTotalNanos > 0 {
				rec.MatchedSerialSpeedup = float64(rec.MatchedSerialNanos) / float64(rec.StrategyTotalNanos)
			}
			if err := json.NewEncoder(w).Encode(&rec); err != nil {
				panic(err)
			}
			serialState.Commit()
			stmState.Commit()
		}
	}
}
