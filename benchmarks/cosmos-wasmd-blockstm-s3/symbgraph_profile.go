package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"runtime"
	"runtime/pprof"
	"time"

	abci "github.com/cometbft/cometbft/abci/types"
	tmproto "github.com/cometbft/cometbft/proto/tendermint/types"
	storetypes "github.com/cosmos/cosmos-sdk/store/v2/types"
	sdk "github.com/cosmos/cosmos-sdk/types"
)

type symbGraphProfileMeta struct {
	Dataset                string  `json:"dataset"`
	Strategy               string  `json:"strategy"`
	Workers                int     `json:"workers"`
	Blocks                 int     `json:"blocks"`
	Scope                  string  `json:"scope"`
	CosmosSDKVersion       string  `json:"cosmos_sdk_version"`
	WasmdVersion           string  `json:"wasmd_version"`
	ComputeScale           float64 `json:"compute_scale"`
	GoIterationsPerNano    float64 `json:"go_iterations_per_nano"`
	MeasurementIncluded    bool    `json:"measurement_included"`
	ProfileSeparate        bool    `json:"profile_execution_separate"`
	TotalAllocBytesDelta   uint64  `json:"total_alloc_bytes_delta"`
	MallocsDelta           uint64  `json:"mallocs_delta"`
	FreesDelta             uint64  `json:"frees_delta"`
	HeapAllocBytesAfter    uint64  `json:"heap_alloc_bytes_after"`
	NumGCDelta             uint32  `json:"num_gc_delta"`
	ExecutionAttempts      uint64  `json:"execution_attempts"`
	Reexecutions           uint64  `json:"reexecutions"`
	SpeculatedTransactions uint64  `json:"speculated_transactions"`
	ReusedTransactions     uint64  `json:"reused_transactions"`
}

func profileWasmdSymbGraph2(
	repoRoot string,
	m Manifest,
	blocks []ExecutionBlock,
	cal Calibration,
	accesses symbolicAccessIndex,
	profileDir string,
) error {
	if profileDir == "" {
		return nil
	}
	if err := os.MkdirAll(profileDir, 0o755); err != nil {
		return err
	}
	b, err := newBenchApp(repoRoot, m, blocks)
	if err != nil {
		return fmt.Errorf("create 2-worker SymbGraph profiling app: %w", err)
	}
	defer b.close()

	cpuPath := filepath.Join(profileDir, "cosmos-wasmd-symbgraph-static-w2.cpu.pprof")
	cpuFile, err := os.Create(cpuPath)
	if err != nil {
		return err
	}
	runtime.GC()
	var before runtime.MemStats
	runtime.ReadMemStats(&before)
	if err := pprof.StartCPUProfile(cpuFile); err != nil {
		cpuFile.Close()
		return err
	}

	var total policyRunStats
	runErr := func() error {
		for blockOffset, block := range blocks {
			header := tmproto.Header{ChainID: chainID, Height: int64(blockOffset + 2), Time: time.Unix(int64(block.Timestamp), 0)}
			blockCtx := b.app.NewNextBlockContext(header)
			runner := NewSymbGraphStaticRunner(2, block, accesses)
			_, err := runner.Run(
				context.Background(),
				blockCtx.MultiStore(),
				txBytes(block),
				func(_ []byte, _ sdk.Tx, ms storetypes.MultiStore, idx int, _ map[string]any) *abci.ExecTxResult {
					ctx := blockCtx.WithMultiStore(ms).WithEventManager(sdk.NewEventManager()).WithGasMeter(storetypes.NewInfiniteGasMeter())
					tx := block.Transactions[idx]
					if err := b.executeTxIsolated(ctx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); err != nil {
						return &abci.ExecTxResult{Code: 1, Log: err.Error()}
					}
					return &abci.ExecTxResult{}
				},
			)
			if err != nil {
				return fmt.Errorf("profile SymbGraph block %d: %w", block.BlockNumber, err)
			}
			st := runner.LastStats()
			total.Attempts += st.Attempts
			total.Reexecutions += st.Reexecutions
			total.Speculated += st.Speculated
			total.Reused += st.Reused
			total.Replayed += st.Replayed
			if err := commitFinalizeState(b.app); err != nil {
				return fmt.Errorf("profile SymbGraph commit block %d: %w", block.BlockNumber, err)
			}
		}
		return nil
	}()
	pprof.StopCPUProfile()
	closeErr := cpuFile.Close()
	var after runtime.MemStats
	runtime.ReadMemStats(&after)
	if runErr != nil {
		return runErr
	}
	if closeErr != nil {
		return closeErr
	}

	meta := symbGraphProfileMeta{
		Dataset:                "vegeta-s3-wasmd-blockstm",
		Strategy:               "cosmos-wasmd-symbgraph-static",
		Workers:                2,
		Blocks:                 len(blocks),
		Scope:                  symbGraphStaticScope,
		CosmosSDKVersion:       cosmosSDKVersion,
		WasmdVersion:           wasmdVersion,
		ComputeScale:           cal.Scale,
		GoIterationsPerNano:    cal.IterPerNano,
		MeasurementIncluded:    false,
		ProfileSeparate:        true,
		TotalAllocBytesDelta:   after.TotalAlloc - before.TotalAlloc,
		MallocsDelta:           after.Mallocs - before.Mallocs,
		FreesDelta:             after.Frees - before.Frees,
		HeapAllocBytesAfter:    after.HeapAlloc,
		NumGCDelta:             after.NumGC - before.NumGC,
		ExecutionAttempts:      total.Attempts,
		Reexecutions:           total.Reexecutions,
		SpeculatedTransactions: total.Speculated,
		ReusedTransactions:     total.Reused,
	}
	bz, err := json.MarshalIndent(meta, "", "  ")
	if err != nil {
		return err
	}
	bz = append(bz, '\n')
	return os.WriteFile(filepath.Join(profileDir, "cosmos-wasmd-symbgraph-static-w2.profile.json"), bz, 0o644)
}
