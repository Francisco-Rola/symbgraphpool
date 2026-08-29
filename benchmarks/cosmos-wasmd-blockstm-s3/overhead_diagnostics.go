package main

import (
	"encoding/json"
	"fmt"
	"os"
	"time"

	tmproto "github.com/cometbft/cometbft/proto/tendermint/types"
	storetypes "github.com/cosmos/cosmos-sdk/store/v2/types"
	sdk "github.com/cosmos/cosmos-sdk/types"
)

type serialOverheadControl struct {
	ActiveNanos      uint64 `json:"active_nanos"`
	BranchNanos      uint64 `json:"branch_nanos"`
	ExecuteNanos     uint64 `json:"execute_nanos"`
	WriteNanos       uint64 `json:"write_nanos"`
	SerialEquivalent bool   `json:"serial_equivalent"`
}

type wasmdOverheadDiagnostics struct {
	SchemaVersion               int                   `json:"schema_version"`
	Workers                     int                   `json:"workers"`
	DirectSerialNanos           uint64                `json:"direct_serial_nanos"`
	OuterCacheSerial            serialOverheadControl `json:"outer_cache_serial"`
	TrackedOuterCacheSerial     serialOverheadControl `json:"tracked_outer_cache_serial"`
	TrackedSingleCacheSerial    serialOverheadControl `json:"tracked_single_cache_serial"`
	OuterCacheVsDirect          float64               `json:"outer_cache_vs_direct"`
	TrackedVsDirect             float64               `json:"tracked_vs_direct"`
	TrackedVsOuterCache         float64               `json:"tracked_vs_outer_cache"`
	TrackedSingleCacheVsDirect  float64               `json:"tracked_single_cache_vs_direct"`
	SingleCacheVsTrackedOldPath float64               `json:"single_cache_vs_tracked_old_path"`
	Notes                       []string              `json:"notes"`
}

func boolEnv(name string) bool {
	switch os.Getenv(name) {
	case "1", "true", "TRUE", "yes", "YES", "on", "ON":
		return true
	default:
		return false
	}
}

func addSerialOverheadControl(dst *serialOverheadControl, src serialOverheadControl) {
	dst.ActiveNanos += src.ActiveNanos
	dst.BranchNanos += src.BranchNanos
	dst.ExecuteNanos += src.ExecuteNanos
	dst.WriteNanos += src.WriteNanos
	dst.SerialEquivalent = dst.SerialEquivalent && src.SerialEquivalent
}

func runOuterCacheSerialBlock(
	b *benchApp,
	blockOffset int,
	block ExecutionBlock,
	cal Calibration,
	tracked bool,
	singleCache bool,
) (serialOverheadControl, error) {
	out := serialOverheadControl{SerialEquivalent: true}
	header := tmproto.Header{
		ChainID: chainID,
		Height:  int64(blockOffset + 2),
		Time:    time.Unix(int64(block.Timestamp), 0),
	}
	blockCtx := b.app.NewNextBlockContext(header)
	activeStart := time.Now()

	for _, tx := range block.Transactions {
		branchStart := time.Now()
		if tracked {
			// This is exactly the extra isolation layer currently used by the
			// SymbGraph and Vegeta ports. executeTx then creates its own inner
			// CacheContext, so successful writes traverse two cache layers.
			branch := newTrackingMultiStore(blockCtx.MultiStore())
			out.BranchNanos += uint64(time.Since(branchStart).Nanoseconds())

			ctx := blockCtx.
				WithMultiStore(branch).
				WithEventManager(sdk.NewEventManager()).
				WithGasMeter(storetypes.NewInfiniteGasMeter())
			execStart := time.Now()
			compute := cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)
			var err error
			if singleCache {
				err = b.executeTxIsolated(ctx, block, tx, compute)
			} else {
				err = b.executeTx(ctx, block, tx, compute)
			}
			if err != nil {
				return out, err
			}
			out.ExecuteNanos += uint64(time.Since(execStart).Nanoseconds())

			writeStart := time.Now()
			branch.Write()
			out.WriteNanos += uint64(time.Since(writeStart).Nanoseconds())
			continue
		}

		// Same double-cache shape, but without the tracking wrapper. The gap from
		// direct serial isolates the extra CacheMultiStore/copy/merge tax.
		branch := blockCtx.MultiStore().CacheMultiStore()
		out.BranchNanos += uint64(time.Since(branchStart).Nanoseconds())
		ctx := blockCtx.
			WithMultiStore(branch).
			WithEventManager(sdk.NewEventManager()).
			WithGasMeter(storetypes.NewInfiniteGasMeter())
		execStart := time.Now()
		if err := b.executeTx(ctx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); err != nil {
			return out, err
		}
		out.ExecuteNanos += uint64(time.Since(execStart).Nanoseconds())

		writeStart := time.Now()
		branch.Write()
		out.WriteNanos += uint64(time.Since(writeStart).Nanoseconds())
	}

	out.ActiveNanos = uint64(time.Since(activeStart).Nanoseconds())
	if err := commitFinalizeState(b.app); err != nil {
		return out, err
	}
	return out, nil
}

func runWasmdOverheadDiagnostics(
	repoRoot string,
	manifest Manifest,
	blocks []ExecutionBlock,
	cal Calibration,
	workers int,
	directSerialNanos uint64,
	serialDigests [][32]byte,
) (wasmdOverheadDiagnostics, error) {
	cacheApp, err := newBenchApp(repoRoot, manifest, blocks)
	if err != nil {
		return wasmdOverheadDiagnostics{}, err
	}
	defer cacheApp.close()

	trackedApp, err := newBenchApp(repoRoot, manifest, blocks)
	if err != nil {
		return wasmdOverheadDiagnostics{}, err
	}
	defer trackedApp.close()

	singleCacheApp, err := newBenchApp(repoRoot, manifest, blocks)
	if err != nil {
		return wasmdOverheadDiagnostics{}, err
	}
	defer singleCacheApp.close()

	cacheTotal := serialOverheadControl{SerialEquivalent: true}
	trackedTotal := serialOverheadControl{SerialEquivalent: true}
	singleCacheTotal := serialOverheadControl{SerialEquivalent: true}

	// Alternate which diagnostic executes first per block. This does not make
	// them publication measurements, but avoids a trivial fixed ordering bias.
	for i, block := range blocks {
		runCache := func() error {
			row, err := runOuterCacheSerialBlock(cacheApp, i, block, cal, false, false)
			if err != nil {
				return err
			}
			if i < len(serialDigests) && digestApp(cacheApp.app) != serialDigests[i] {
				return fmt.Errorf("outer-cache serial state mismatch block=%d", block.BlockNumber)
			}
			addSerialOverheadControl(&cacheTotal, row)
			return nil
		}
		runTracked := func() error {
			row, err := runOuterCacheSerialBlock(trackedApp, i, block, cal, true, false)
			if err != nil {
				return err
			}
			if i < len(serialDigests) && digestApp(trackedApp.app) != serialDigests[i] {
				return fmt.Errorf("tracked-outer-cache serial state mismatch block=%d", block.BlockNumber)
			}
			addSerialOverheadControl(&trackedTotal, row)
			return nil
		}
		runSingleCache := func() error {
			row, err := runOuterCacheSerialBlock(singleCacheApp, i, block, cal, true, true)
			if err != nil {
				return err
			}
			if i < len(serialDigests) && digestApp(singleCacheApp.app) != serialDigests[i] {
				return fmt.Errorf("tracked-single-cache serial state mismatch block=%d", block.BlockNumber)
			}
			addSerialOverheadControl(&singleCacheTotal, row)
			return nil
		}
		if i%2 == 0 {
			if err := runCache(); err != nil {
				return wasmdOverheadDiagnostics{}, err
			}
			if err := runTracked(); err != nil {
				return wasmdOverheadDiagnostics{}, err
			}
			if err := runSingleCache(); err != nil {
				return wasmdOverheadDiagnostics{}, err
			}
		} else {
			if err := runTracked(); err != nil {
				return wasmdOverheadDiagnostics{}, err
			}
			if err := runCache(); err != nil {
				return wasmdOverheadDiagnostics{}, err
			}
			if err := runSingleCache(); err != nil {
				return wasmdOverheadDiagnostics{}, err
			}
		}
	}

	out := wasmdOverheadDiagnostics{
		SchemaVersion:            1,
		Workers:                  workers,
		DirectSerialNanos:        directSerialNanos,
		OuterCacheSerial:         cacheTotal,
		TrackedOuterCacheSerial:  trackedTotal,
		TrackedSingleCacheSerial: singleCacheTotal,
		Notes: []string{
			"direct serial is the first publication sample's existing cosmos-wasmd-direct-serial timing",
			"outer-cache serial reproduces the old double-cache shape without access tracking",
			"tracked-outer-cache serial reproduces the old double-cache shape with the now lock-free tracker",
			"tracked-single-cache serial is the optimized path: one transaction cache layer plus the same lock-free actual-access tracker, but no parallel scheduling",
			"diagnostic controls execute after publication rows and are never included in the publication summary",
		},
	}
	if directSerialNanos > 0 {
		out.OuterCacheVsDirect = float64(cacheTotal.ActiveNanos) / float64(directSerialNanos)
		out.TrackedVsDirect = float64(trackedTotal.ActiveNanos) / float64(directSerialNanos)
		out.TrackedSingleCacheVsDirect = float64(singleCacheTotal.ActiveNanos) / float64(directSerialNanos)
	}
	if cacheTotal.ActiveNanos > 0 {
		out.TrackedVsOuterCache = float64(trackedTotal.ActiveNanos) / float64(cacheTotal.ActiveNanos)
	}
	if trackedTotal.ActiveNanos > 0 {
		out.SingleCacheVsTrackedOldPath = float64(singleCacheTotal.ActiveNanos) / float64(trackedTotal.ActiveNanos)
	}
	return out, nil
}

func writeWasmdOverheadDiagnostics(path string, d wasmdOverheadDiagnostics) error {
	bz, err := json.MarshalIndent(d, "", "  ")
	if err != nil {
		return err
	}
	bz = append(bz, '\n')
	return os.WriteFile(path, bz, 0o644)
}
