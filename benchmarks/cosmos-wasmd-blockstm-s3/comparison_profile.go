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

type isolatedProfileMeta struct {
	Dataset                string  `json:"dataset"`
	Runner                 string  `json:"runner"`
	ProfileKind            string  `json:"profile_kind"`
	Workers                int     `json:"workers"`
	Blocks                 int     `json:"blocks"`
	CosmosSDKVersion       string  `json:"cosmos_sdk_version"`
	WasmdVersion           string  `json:"wasmd_version"`
	ComputeScale           float64 `json:"compute_scale"`
	GoIterationsPerNano    float64 `json:"go_iterations_per_nano"`
	MeasurementIncluded    bool    `json:"measurement_included"`
	ProfileSeparateProcess bool    `json:"profile_separate_process"`
	WallNanos              uint64  `json:"wall_nanos"`
	TotalAllocBytesDelta   uint64  `json:"total_alloc_bytes_delta"`
	MallocsDelta           uint64  `json:"mallocs_delta"`
	FreesDelta             uint64  `json:"frees_delta"`
	HeapAllocBytesAfter    uint64  `json:"heap_alloc_bytes_after"`
	NumGCDelta             uint32  `json:"num_gc_delta"`
	ExecutionAttempts      uint64  `json:"execution_attempts"`
	Reexecutions           uint64  `json:"reexecutions"`
	SpeculatedTransactions uint64  `json:"speculated_transactions,omitempty"`
	ReusedTransactions     uint64  `json:"reused_transactions,omitempty"`
}

func profileFilePrefix(runner string, workers int) string {
	switch runner {
	case "direct-serial":
		return fmt.Sprintf("cosmos-wasmd-direct-serial-w%d", workers)
	case "outer-cache-serial":
		return fmt.Sprintf("cosmos-wasmd-outer-cache-serial-w%d", workers)
	case "symbgraph-static":
		return fmt.Sprintf("cosmos-wasmd-symbgraph-static-w%d", workers)
	default:
		return fmt.Sprintf("cosmos-wasmd-%s-w%d", runner, workers)
	}
}

func writeNamedRuntimeProfile(name, path string) error {
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

func runDirectSerialProfile(b *benchApp, blocks []ExecutionBlock, cal Calibration) (policyRunStats, error) {
	var stats policyRunStats
	for blockOffset, block := range blocks {
		header := tmproto.Header{ChainID: chainID, Height: int64(blockOffset + 2), Time: time.Unix(int64(block.Timestamp), 0)}
		ctx := b.app.NewNextBlockContext(header)
		for _, tx := range block.Transactions {
			stats.Attempts++
			if err := b.executeTx(ctx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); err != nil {
				return stats, fmt.Errorf("profile direct block=%d tx=%d: %w", block.BlockNumber, tx.TxIndex, err)
			}
		}
		if err := commitFinalizeState(b.app); err != nil {
			return stats, fmt.Errorf("profile direct commit block %d: %w", block.BlockNumber, err)
		}
	}
	return stats, nil
}

func runOuterCacheSerialProfile(b *benchApp, blocks []ExecutionBlock, cal Calibration) (policyRunStats, error) {
	var stats policyRunStats
	for blockOffset, block := range blocks {
		stats.Attempts += uint64(len(block.Transactions))
		if _, err := runOuterCacheSerialBlock(b, blockOffset, block, cal, false, false); err != nil {
			return stats, fmt.Errorf("profile outer-cache block %d: %w", block.BlockNumber, err)
		}
	}
	return stats, nil
}

func runSymbGraphProfile(b *benchApp, blocks []ExecutionBlock, cal Calibration, accesses symbolicAccessIndex, workers int) (policyRunStats, error) {
	var total policyRunStats
	for blockOffset, block := range blocks {
		header := tmproto.Header{ChainID: chainID, Height: int64(blockOffset + 2), Time: time.Unix(int64(block.Timestamp), 0)}
		blockCtx := b.app.NewNextBlockContext(header)
		runner := NewSymbGraphStaticRunner(workers, block, accesses)
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
			return total, fmt.Errorf("profile SymbGraph block %d: %w", block.BlockNumber, err)
		}
		st := runner.LastStats()
		total.Attempts += st.Attempts
		total.Reexecutions += st.Reexecutions
		total.Speculated += st.Speculated
		total.Reused += st.Reused
		total.Replayed += st.Replayed
		if err := commitFinalizeState(b.app); err != nil {
			return total, fmt.Errorf("profile SymbGraph commit block %d: %w", block.BlockNumber, err)
		}
	}
	return total, nil
}

func profileWasmdRunner(
	repoRoot string,
	manifest Manifest,
	blocks []ExecutionBlock,
	cal Calibration,
	accesses symbolicAccessIndex,
	runner string,
	profileKind string,
	workers int,
	profileDir string,
) error {
	switch runner {
	case "direct-serial", "outer-cache-serial", "symbgraph-static":
	default:
		return fmt.Errorf("unsupported profile-only runner %q", runner)
	}
	switch profileKind {
	case "cpu-alloc", "mutex":
	default:
		return fmt.Errorf("unsupported profile-only kind %q", profileKind)
	}
	if workers < 1 {
		workers = 1
	}
	if err := os.MkdirAll(profileDir, 0o755); err != nil {
		return err
	}

	// Each invocation is a fresh process. CPU/allocation and mutex profiling are
	// deliberately separate invocations so mutex sampling does not distort the
	// CPU profile. Allocation snapshots use the runtime's normal sampling rate;
	// exact allocation totals come from MemStats deltas in the sidecar JSON.
	b, err := newBenchApp(repoRoot, manifest, blocks)
	if err != nil {
		return fmt.Errorf("create %s profiling app: %w", runner, err)
	}
	defer b.close()

	prefix := profileFilePrefix(runner, workers)
	var before runtime.MemStats
	var cpuFile *os.File
	var cpuPath, allocBefore, allocAfter string
	oldMutexFraction := 0

	if profileKind == "cpu-alloc" {
		allocBefore = filepath.Join(profileDir, prefix+".allocs-before.pprof")
		allocAfter = filepath.Join(profileDir, prefix+".allocs-after.pprof")
		cpuPath = filepath.Join(profileDir, prefix+".cpu.pprof")
		runtime.GC()
		if err := writeNamedRuntimeProfile("allocs", allocBefore); err != nil {
			return err
		}
		runtime.ReadMemStats(&before)
		cpuFile, err = os.Create(cpuPath)
		if err != nil {
			return err
		}
		if err := pprof.StartCPUProfile(cpuFile); err != nil {
			cpuFile.Close()
			return err
		}
	} else {
		oldMutexFraction = runtime.SetMutexProfileFraction(1)
	}

	started := time.Now()
	var stats policyRunStats
	switch runner {
	case "direct-serial":
		stats, err = runDirectSerialProfile(b, blocks, cal)
	case "outer-cache-serial":
		stats, err = runOuterCacheSerialProfile(b, blocks, cal)
	case "symbgraph-static":
		stats, err = runSymbGraphProfile(b, blocks, cal, accesses, workers)
	}
	wall := time.Since(started)

	if profileKind == "cpu-alloc" {
		pprof.StopCPUProfile()
		closeErr := cpuFile.Close()
		if err != nil {
			return err
		}
		if closeErr != nil {
			return closeErr
		}
		runtime.GC()
		var after runtime.MemStats
		runtime.ReadMemStats(&after)
		if err := writeNamedRuntimeProfile("allocs", allocAfter); err != nil {
			return err
		}
		meta := isolatedProfileMeta{
			Dataset:                "vegeta-s3-wasmd-blockstm",
			Runner:                 runner,
			ProfileKind:            profileKind,
			Workers:                workers,
			Blocks:                 len(blocks),
			CosmosSDKVersion:       cosmosSDKVersion,
			WasmdVersion:           wasmdVersion,
			ComputeScale:           cal.Scale,
			GoIterationsPerNano:    cal.IterPerNano,
			MeasurementIncluded:    false,
			ProfileSeparateProcess: true,
			WallNanos:              uint64(wall.Nanoseconds()),
			TotalAllocBytesDelta:   after.TotalAlloc - before.TotalAlloc,
			MallocsDelta:           after.Mallocs - before.Mallocs,
			FreesDelta:             after.Frees - before.Frees,
			HeapAllocBytesAfter:    after.HeapAlloc,
			NumGCDelta:             after.NumGC - before.NumGC,
			ExecutionAttempts:      stats.Attempts,
			Reexecutions:           stats.Reexecutions,
			SpeculatedTransactions: stats.Speculated,
			ReusedTransactions:     stats.Reused,
		}
		bz, err := json.MarshalIndent(meta, "", "  ")
		if err != nil {
			return err
		}
		bz = append(bz, '\n')
		metaPath := filepath.Join(profileDir, prefix+".profile.json")
		if err := os.WriteFile(metaPath, bz, 0o644); err != nil {
			return err
		}
		fmt.Fprintf(os.Stderr, "PROFILE runner=%s kind=%s workers=%d wall=%.3fs alloc=%.3fGB mallocs=%d gc=%d dir=%s\n",
			runner, profileKind, workers, float64(meta.WallNanos)/1e9,
			float64(meta.TotalAllocBytesDelta)/(1<<30), meta.MallocsDelta, meta.NumGCDelta, profileDir)
		return nil
	}

	// Mutex profile invocation: profiling was disabled during app construction,
	// so this fresh process contains only contention from the requested replay.
	runtime.SetMutexProfileFraction(0)
	mutexPath := filepath.Join(profileDir, prefix+".mutex.pprof")
	mutexErr := writeNamedRuntimeProfile("mutex", mutexPath)
	runtime.SetMutexProfileFraction(oldMutexFraction)
	if err != nil {
		return err
	}
	if mutexErr != nil {
		return mutexErr
	}
	meta := isolatedProfileMeta{
		Dataset:                "vegeta-s3-wasmd-blockstm",
		Runner:                 runner,
		ProfileKind:            profileKind,
		Workers:                workers,
		Blocks:                 len(blocks),
		CosmosSDKVersion:       cosmosSDKVersion,
		WasmdVersion:           wasmdVersion,
		ComputeScale:           cal.Scale,
		GoIterationsPerNano:    cal.IterPerNano,
		MeasurementIncluded:    false,
		ProfileSeparateProcess: true,
		WallNanos:              uint64(wall.Nanoseconds()),
		ExecutionAttempts:      stats.Attempts,
		Reexecutions:           stats.Reexecutions,
		SpeculatedTransactions: stats.Speculated,
		ReusedTransactions:     stats.Reused,
	}
	bz, err := json.MarshalIndent(meta, "", "  ")
	if err != nil {
		return err
	}
	bz = append(bz, '\n')
	if err := os.WriteFile(filepath.Join(profileDir, prefix+".mutex.profile.json"), bz, 0o644); err != nil {
		return err
	}
	fmt.Fprintf(os.Stderr, "PROFILE runner=%s kind=%s workers=%d wall=%.3fs dir=%s\n",
		runner, profileKind, workers, float64(meta.WallNanos)/1e9, profileDir)
	return nil
}
