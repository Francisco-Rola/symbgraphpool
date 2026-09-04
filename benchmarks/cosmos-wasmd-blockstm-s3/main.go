package main

import (
	"bufio"
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"flag"
	"fmt"
	"io"
	"math"
	"os"
	"path/filepath"
	"runtime"
	"runtime/pprof"
	"sort"
	"strconv"
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
	pruningtypes "github.com/cosmos/cosmos-sdk/store/v2/pruning/types"
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
	ariaFBScope          = "actual-wasmd-wasmvm-cosmos-sdk-ariafb-rule2-hotchain-staged-canonical-state-safety-fallback-no-ante-abci"
	exactACGOracleScope  = "evaluation-only-rust-acg-perfect-source-sload-sstore-hard-dependencies-same-mvcc-validation-zero-replay"
	profileBaselineScope = "actual-wasmd-wasmvm-cosmos-sdk-txnrunner-blockstm-w4-pprof-unmeasured"
	chainID              = "vegeta-s3-wasmd-blockstm"
)

var (
	benchmarkIAVLCacheSize   = 500_000
	benchmarkIAVLSyncPruning = false
)

func intEnvDefault(name string, fallback int) int {
	value, ok := os.LookupEnv(name)
	if !ok || strings.TrimSpace(value) == "" {
		return fallback
	}
	parsed, err := strconv.Atoi(strings.TrimSpace(value))
	if err != nil {
		return fallback
	}
	return parsed
}

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
	WasmArtifacts    map[string]string `json:"wasm_artifacts"`
	Instances        []InstanceSpec    `json:"instances"`
	BankSeeds        []BankSeed        `json:"bank_seeds"`
	PrimingCalls     []CallSpec        `json:"priming_calls"`
	LogicalAddresses []string          `json:"logical_addresses,omitempty"`
	Blocks           int               `json:"blocks,omitempty"`
	Transactions     int               `json:"transactions,omitempty"`
	FirstTimestamp   uint64            `json:"first_timestamp,omitempty"`
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
	BlockNumber          uint64  `json:"block_number"`
	TxIndex              int     `json:"tx_index"`
	TxHash               string  `json:"tx_hash"`
	SourceTracePresent   bool    `json:"source_trace_present"`
	SourceComputePresent bool    `json:"source_compute_present"`
	SourceComputeMetric  string  `json:"source_compute_metric"`
	SourceComputeUnits   *uint64 `json:"source_compute_units"`
	SourceOpcodeSteps    *uint64 `json:"source_opcode_steps"`
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
	Metric         string
	Weights        map[[2]uint64]TxWeight
}

type Record struct {
	SchemaVersion                        int     `json:"schema_version"`
	Dataset                              string  `json:"dataset"`
	Sample                               int     `json:"sample"`
	BlockNumber                          uint64  `json:"block_number"`
	Strategy                             string  `json:"strategy"`
	Workers                              int     `json:"workers"`
	MatchedSerialNanos                   uint64  `json:"matched_serial_nanos"`
	HistoricalSerialNanos                uint64  `json:"historical_serial_nanos"`
	StrategyTotalNanos                   uint64  `json:"strategy_total_nanos"`
	PreConsensusNanos                    uint64  `json:"pre_consensus_nanos,omitempty"`
	PostConsensusNanos                   uint64  `json:"post_consensus_nanos,omitempty"`
	MatchedSerialSpeedup                 float64 `json:"matched_serial_speedup"`
	Transactions                         int     `json:"transactions"`
	ExecutionAttempts                    uint64  `json:"execution_attempts"`
	Reexecutions                         uint64  `json:"reexecutions"`
	SerialEquivalent                     bool    `json:"serial_equivalent"`
	SerialReferenceScope                 string  `json:"serial_reference_scope"`
	SerialCommitVersion                  int64   `json:"serial_commit_version,omitempty"`
	SerialCommitHash                     string  `json:"serial_commit_hash,omitempty"`
	ComputeMetric                        string  `json:"compute_calibration_metric"`
	ComputeScale                         float64 `json:"compute_scale"`
	GoIterationsPerNano                  float64 `json:"go_iterations_per_nano"`
	CosmosSDKVersion                     string  `json:"cosmos_sdk_version"`
	WasmdVersion                         string  `json:"wasmd_version"`
	IAVLCacheSize                        int     `json:"iavl_cache_size"`
	IAVLSyncPruning                      bool    `json:"iavl_sync_pruning"`
	EvaluatorSHA256                      string  `json:"evaluator_sha256"`
	BaselineScope                        string  `json:"baseline_scope"`
	BlockSTMPreEstimate                  bool    `json:"block_stm_pre_estimate"`
	SpeculatedTransactions               uint64  `json:"speculated_transactions,omitempty"`
	ReusedTransactions                   uint64  `json:"reused_transactions,omitempty"`
	ValidationNanos                      uint64  `json:"validation_nanos,omitempty"`
	ReplayExecutionNanos                 uint64  `json:"replay_execution_nanos,omitempty"`
	ConflictAnalysisNanos                uint64  `json:"conflict_analysis_nanos,omitempty"`
	DiscoveredConflicts                  uint64  `json:"discovered_conflicts,omitempty"`
	ForwardFallbacks                     uint64  `json:"forward_fallbacks,omitempty"`
	SafetyReplays                        uint64  `json:"safety_replays,omitempty"`
	VegetaSnapshotBuildNanos             uint64  `json:"vegeta_snapshot_build_nanos,omitempty"`
	VegetaSnapshotPointHits              uint64  `json:"vegeta_snapshot_point_hits,omitempty"`
	VegetaSnapshotPointMisses            uint64  `json:"vegeta_snapshot_point_misses,omitempty"`
	VegetaSnapshotRangeHits              uint64  `json:"vegeta_snapshot_range_hits,omitempty"`
	VegetaSnapshotRangeMisses            uint64  `json:"vegeta_snapshot_range_misses,omitempty"`
	VegetaPostBatches                    uint64  `json:"vegeta_post_batches,omitempty"`
	VegetaPostSingletonBatches           uint64  `json:"vegeta_post_singleton_batches,omitempty"`
	VegetaPostMaxBatch                   uint64  `json:"vegeta_post_max_batch,omitempty"`
	VegetaReadySelectionNanos            uint64  `json:"vegeta_ready_selection_nanos,omitempty"`
	VegetaPreExecWorkNanos               uint64  `json:"vegeta_pre_exec_work_nanos,omitempty"`
	VegetaPreExecSpanNanos               uint64  `json:"vegeta_pre_exec_span_nanos,omitempty"`
	VegetaPostExecWorkNanos              uint64  `json:"vegeta_post_exec_work_nanos,omitempty"`
	VegetaPostExecSpanNanos              uint64  `json:"vegeta_post_exec_span_nanos,omitempty"`
	VegetaPostWideExecWorkNanos          uint64  `json:"vegeta_post_wide_exec_work_nanos,omitempty"`
	VegetaPostWideExecSpanNanos          uint64  `json:"vegeta_post_wide_exec_span_nanos,omitempty"`
	VegetaPostWideTransactions           uint64  `json:"vegeta_post_wide_transactions,omitempty"`
	VegetaLongestChain                   uint64  `json:"vegeta_longest_chain,omitempty"`
	VegetaChainCount                     uint64  `json:"vegeta_chain_count,omitempty"`
	VegetaAlg3ValidationNanos            uint64  `json:"vegeta_alg3_validation_nanos,omitempty"`
	VegetaRangeValidationNanos           uint64  `json:"vegeta_range_validation_nanos,omitempty"`
	VegetaIntrinsicReexecutionNanos      uint64  `json:"vegeta_intrinsic_reexecution_nanos,omitempty"`
	VegetaHistoricalFallbackNanos        uint64  `json:"vegeta_historical_fallback_nanos,omitempty"`
	VegetaHistoricalFallbackTransactions uint64  `json:"vegeta_historical_fallback_transactions,omitempty"`
	VegetaCanonicalFallback              bool    `json:"vegeta_canonical_fallback,omitempty"`
	VegetaCanonicalFallbackNanos         uint64  `json:"vegeta_canonical_fallback_nanos,omitempty"`
	VegetaCanonicalFallbackReason        string  `json:"vegeta_canonical_fallback_reason,omitempty"`
	OracleSourceReads                    uint64  `json:"oracle_source_reads,omitempty"`
	OracleSourceWrites                   uint64  `json:"oracle_source_writes,omitempty"`
	OracleSourceTraceMissing             int     `json:"oracle_source_trace_missing,omitempty"`
	OracleAdapterHardEdges               int     `json:"oracle_adapter_hard_edges,omitempty"`
	OracleTranslationCompensationEdges   int     `json:"oracle_translation_compensation_edges,omitempty"`
	OracleMissingBarrierEdges            int     `json:"oracle_missing_barrier_edges,omitempty"`

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
	db           dbm.DB
	permissioned *wasmkeeper.PermissionedKeeper
	contracts    map[string]sdk.AccAddress
	addresses    map[string]sdk.AccAddress
	repl         map[string]string
	prepared     map[preparedCallKey]preparedCall
	home         string
}

type benchAppTemplate struct {
	dbSnapshot  string
	dbEntries   uint64
	prepared    map[preparedCallKey]preparedCall
	contracts   map[string]sdk.AccAddress
	addresses   map[string]sdk.AccAddress
	repl        map[string]string
	home        string
	stateDigest [32]byte
	stateBytes  uint64
}

func cloneAddrMap(src map[string]sdk.AccAddress) map[string]sdk.AccAddress {
	out := make(map[string]sdk.AccAddress, len(src))
	for k, v := range src {
		out[k] = append(sdk.AccAddress(nil), v...)
	}
	return out
}

func cloneStringMap(src map[string]string) map[string]string {
	out := make(map[string]string, len(src))
	for k, v := range src {
		out[k] = v
	}
	return out
}

const setupDBRestoreBatchEntries = 16 * 1024

func captureSetupDB(db dbm.DB) (string, uint64, uint64, error) {
	f, err := os.CreateTemp("", "symbgraph-wasmd-setup-db-*.snapshot")
	if err != nil {
		return "", 0, 0, err
	}
	path := f.Name()
	ok := false
	defer func() {
		_ = f.Close()
		if !ok {
			_ = os.Remove(path)
		}
	}()

	w := bufio.NewWriterSize(f, 1<<20)
	it, err := db.Iterator(nil, nil)
	if err != nil {
		return "", 0, 0, err
	}
	defer it.Close()

	var entries uint64
	var bytes uint64
	var header [8]byte
	for ; it.Valid(); it.Next() {
		key := it.Key()
		value := it.Value()
		if len(key) > math.MaxUint32 || len(value) > math.MaxUint32 {
			return "", 0, 0, fmt.Errorf("setup database entry too large: key=%d value=%d", len(key), len(value))
		}
		binary.LittleEndian.PutUint32(header[:4], uint32(len(key)))
		binary.LittleEndian.PutUint32(header[4:], uint32(len(value)))
		if _, err := w.Write(header[:]); err != nil {
			return "", 0, 0, err
		}
		if _, err := w.Write(key); err != nil {
			return "", 0, 0, err
		}
		if _, err := w.Write(value); err != nil {
			return "", 0, 0, err
		}
		entries++
		bytes += uint64(len(key) + len(value))
	}
	if err := it.Error(); err != nil {
		return "", 0, 0, err
	}
	if err := w.Flush(); err != nil {
		return "", 0, 0, err
	}
	if err := f.Close(); err != nil {
		return "", 0, 0, err
	}
	ok = true
	return path, entries, bytes, nil
}

func restoreSetupDB(snapshot string) (dbm.DB, error) {
	f, err := os.Open(snapshot)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	db := dbm.NewMemDB()
	r := bufio.NewReaderSize(f, 1<<20)
	var header [8]byte
	batch := db.NewBatchWithSize(setupDBRestoreBatchEntries)
	batchEntries := 0
	flush := func() error {
		if batchEntries == 0 {
			return nil
		}
		if err := batch.Write(); err != nil {
			return err
		}
		if err := batch.Close(); err != nil {
			return err
		}
		batch = db.NewBatchWithSize(setupDBRestoreBatchEntries)
		batchEntries = 0
		return nil
	}
	defer func() { _ = batch.Close() }()

	for {
		_, err := io.ReadFull(r, header[:])
		if err == io.EOF {
			break
		}
		if err != nil {
			_ = db.Close()
			return nil, fmt.Errorf("read setup snapshot header: %w", err)
		}
		keyLen := binary.LittleEndian.Uint32(header[:4])
		valueLen := binary.LittleEndian.Uint32(header[4:])
		key := make([]byte, int(keyLen))
		value := make([]byte, int(valueLen))
		if _, err := io.ReadFull(r, key); err != nil {
			_ = db.Close()
			return nil, fmt.Errorf("read setup snapshot key: %w", err)
		}
		if _, err := io.ReadFull(r, value); err != nil {
			_ = db.Close()
			return nil, fmt.Errorf("read setup snapshot value: %w", err)
		}
		if err := batch.Set(key, value); err != nil {
			_ = db.Close()
			return nil, err
		}
		batchEntries++
		if batchEntries >= setupDBRestoreBatchEntries {
			if err := flush(); err != nil {
				_ = db.Close()
				return nil, err
			}
		}
	}
	if err := flush(); err != nil {
		_ = db.Close()
		return nil, err
	}
	return db, nil
}

func copyFile(src, dst string, mode os.FileMode) error {
	in, err := os.Open(src)
	if err != nil {
		return err
	}
	defer in.Close()
	out, err := os.OpenFile(dst, os.O_CREATE|os.O_TRUNC|os.O_WRONLY, mode.Perm())
	if err != nil {
		return err
	}
	_, copyErr := io.Copy(out, in)
	closeErr := out.Close()
	if copyErr != nil {
		return copyErr
	}
	return closeErr
}

func copySetupHome(src, dst string) error {
	return filepath.Walk(src, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(src, path)
		if err != nil {
			return err
		}
		if rel == "." {
			return nil
		}
		target := filepath.Join(dst, rel)
		if info.IsDir() {
			return os.MkdirAll(target, info.Mode().Perm())
		}
		if info.Mode().IsRegular() {
			if err := os.MkdirAll(filepath.Dir(target), 0o755); err != nil {
				return err
			}
			return copyFile(path, target, info.Mode())
		}
		// Wasmd's benchmark home is expected to contain directories and regular
		// cache files only. Fail closed rather than sharing a special file or
		// symlink between independently mutated benchmark apps.
		return fmt.Errorf("unsupported setup-home entry %s mode=%s", path, info.Mode())
	})
}

func captureBenchAppTemplate(b *benchApp) (*benchAppTemplate, error) {
	snapshot, entries, stateBytes, err := captureSetupDB(b.db)
	if err != nil {
		return nil, fmt.Errorf("snapshot setup database: %w", err)
	}
	home, err := os.MkdirTemp("", "symbgraph-wasmd-blockstm-template-")
	if err != nil {
		_ = os.Remove(snapshot)
		return nil, err
	}
	if err := copySetupHome(b.home, home); err != nil {
		_ = os.Remove(snapshot)
		_ = os.RemoveAll(home)
		return nil, fmt.Errorf("snapshot wasm setup home: %w", err)
	}
	return &benchAppTemplate{
		dbSnapshot:  snapshot,
		dbEntries:   entries,
		prepared:    b.prepared,
		contracts:   cloneAddrMap(b.contracts),
		addresses:   cloneAddrMap(b.addresses),
		repl:        cloneStringMap(b.repl),
		home:        home,
		stateDigest: digestApp(b.app),
		stateBytes:  stateBytes,
	}, nil
}

func (t *benchAppTemplate) close() {
	if t == nil {
		return
	}
	if t.dbSnapshot != "" {
		_ = os.Remove(t.dbSnapshot)
	}
	if t.home != "" {
		_ = os.RemoveAll(t.home)
	}
}

func newBenchAppFromTemplate(t *benchAppTemplate, blocks []ExecutionBlock, verify bool) (*benchApp, error) {
	home, err := os.MkdirTemp("", "symbgraph-wasmd-blockstm-clone-")
	if err != nil {
		return nil, err
	}
	cleanup := func() { _ = os.RemoveAll(home) }
	if err := copySetupHome(t.home, home); err != nil {
		cleanup()
		return nil, fmt.Errorf("clone wasm setup home: %w", err)
	}
	db, err := restoreSetupDB(t.dbSnapshot)
	if err != nil {
		cleanup()
		return nil, fmt.Errorf("clone setup database: %w", err)
	}
	opts := mapAppOptions{flags.FlagHome: home, "wasm": map[string]any{}}
	a := wasmapp.NewWasmApp(
		log.NewNopLogger(), db, true, opts, nil,
		baseapp.SetChainID(chainID),
		baseapp.SetPruning(pruningtypes.NewPruningOptions(pruningtypes.PruningEverything)),
		baseapp.SetIAVLCacheSize(benchmarkIAVLCacheSize),
		baseapp.SetIAVLSyncPruning(benchmarkIAVLSyncPruning),
	)
	pk := wasmkeeper.NewDefaultPermissionKeeper(&a.WasmKeeper)
	b := &benchApp{
		app:          a,
		db:           db,
		permissioned: pk,
		contracts:    cloneAddrMap(t.contracts),
		addresses:    cloneAddrMap(t.addresses),
		repl:         cloneStringMap(t.repl),
		prepared:     t.prepared,
		home:         home,
	}
	if b.prepared == nil {
		if err := b.prepareWorkloadCalls(blocks); err != nil {
			b.close()
			return nil, fmt.Errorf("prepare cloned workload calls: %w", err)
		}
	}
	if verify && digestApp(a) != t.stateDigest {
		b.close()
		return nil, fmt.Errorf("cloned Wasmd setup state differs from template")
	}
	committedCtx := a.NewContext(true)
	for id, addr := range b.contracts {
		if !a.WasmKeeper.HasContractInfo(committedCtx, addr) {
			b.close()
			return nil, fmt.Errorf("cloned wasm instance %s (%s) missing", id, addr)
		}
	}
	return b, nil
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

type planStream struct {
	f *os.File
	s *bufio.Scanner
}

func openPlanStream(path string) (*planStream, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	s := bufio.NewScanner(f)
	s.Buffer(make([]byte, 64<<10), 64<<20)
	return &planStream{f: f, s: s}, nil
}

func (p *planStream) Close() error { return p.f.Close() }

func (p *planStream) Next() (ExecutionBlock, bool, error) {
	for p.s.Scan() {
		if strings.TrimSpace(p.s.Text()) == "" {
			continue
		}
		var block ExecutionBlock
		if err := json.Unmarshal(p.s.Bytes(), &block); err != nil {
			return ExecutionBlock{}, false, err
		}
		return block, true, nil
	}
	if err := p.s.Err(); err != nil {
		return ExecutionBlock{}, false, err
	}
	return ExecutionBlock{}, false, nil
}

func firstPlanBlock(path string) (ExecutionBlock, error) {
	p, err := openPlanStream(path)
	if err != nil {
		return ExecutionBlock{}, err
	}
	defer p.Close()
	b, ok, err := p.Next()
	if err != nil {
		return ExecutionBlock{}, err
	}
	if !ok {
		return ExecutionBlock{}, fmt.Errorf("empty execution plan")
	}
	return b, nil
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
		present := r.SourceTracePresent
		if r.SourceComputeUnits != nil {
			u = *r.SourceComputeUnits
			present = true
			if c.Metric == "" && r.SourceComputeMetric != "" {
				c.Metric = r.SourceComputeMetric
			}
		} else if r.SourceOpcodeSteps != nil {
			u = *r.SourceOpcodeSteps
			if c.Metric == "" {
				c.Metric = "opcode_steps"
			}
		}
		c.Weights[[2]uint64{r.BlockNumber, uint64(r.TxIndex)}] = TxWeight{Hash: strings.ToLower(r.TxHash), Units: u, Present: present}
		if present {
			c.Total += u
		}
	}
	if c.Metric == "" {
		c.Metric = "none"
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
	for _, address := range m.LogicalAddresses {
		set[address] = struct{}{}
	}
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

// executeOrderedBlockBranch replays one block in the supplied transaction order
// on a disposable block-level cache. It is used as a short-lived safety oracle
// for scheduler-derived serializations without keeping a second full Wasmd app
// alive for the entire campaign.
func executeOrderedBlockBranch(b *benchApp, blockCtx sdk.Context, block ExecutionBlock, cal Calibration, order []int) (*trackingMultiStore, uint64, error) {
	if len(order) != len(block.Transactions) {
		return nil, 0, fmt.Errorf("serial reference order length mismatch block=%d order=%d txs=%d", block.BlockNumber, len(order), len(block.Transactions))
	}
	seen := make([]bool, len(block.Transactions))
	branch := newTrackingMultiStore(blockCtx.MultiStore())
	started := time.Now()
	for _, idx := range order {
		if idx < 0 || idx >= len(block.Transactions) {
			return nil, 0, fmt.Errorf("serial reference order index out of range block=%d idx=%d", block.BlockNumber, idx)
		}
		if seen[idx] {
			return nil, 0, fmt.Errorf("serial reference order duplicates block=%d idx=%d", block.BlockNumber, idx)
		}
		seen[idx] = true
		txStore := branch.CacheMultiStore()
		ctx := blockCtx.WithMultiStore(txStore).WithEventManager(sdk.NewEventManager()).WithGasMeter(storetypes.NewInfiniteGasMeter())
		tx := block.Transactions[idx]
		if err := b.executeTxIsolated(ctx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); err != nil {
			return nil, 0, err
		}
		txStore.Write()
	}
	return branch, uint64(time.Since(started).Nanoseconds()), nil
}

func historicalOrder(txCount int) []int {
	order := make([]int, txCount)
	for i := range order {
		order[i] = i
	}
	return order
}

func isHistoricalOrder(order []int, txCount int) bool {
	if len(order) != txCount {
		return false
	}
	for i, idx := range order {
		if idx != i {
			return false
		}
	}
	return true
}

// executeHistoricalBlockBranch replays one block in canonical transaction order
// on a disposable block-level CacheMultiStore. Each transaction still receives
// its own child cache, so transaction atomicity matches the direct serial path.
// The returned branch is not committed; callers can compare it with a staged
// scheduler result and choose which one to write into the app block context.
func executeHistoricalBlockBranch(b *benchApp, blockCtx sdk.Context, block ExecutionBlock, cal Calibration) (*trackingMultiStore, uint64, error) {
	return executeOrderedBlockBranch(b, blockCtx, block, cal, historicalOrder(len(block.Transactions)))
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
	db := dbm.NewMemDB()
	a := wasmapp.NewWasmApp(
		log.NewNopLogger(), db, true, opts, nil,
		baseapp.SetChainID(chainID),
		baseapp.SetPruning(pruningtypes.NewPruningOptions(pruningtypes.PruningEverything)),
		baseapp.SetIAVLCacheSize(benchmarkIAVLCacheSize),
		baseapp.SetIAVLSyncPruning(benchmarkIAVLSyncPruning),
	)
	genesisState, e := benchmarkGenesisWithValidator(a)
	if e != nil {
		return nil, fmt.Errorf("build benchmark genesis validator set: %w", e)
	}
	genesis, e := json.Marshal(genesisState)
	if e != nil {
		return nil, e
	}
	firstTime := int64(1_678_170_000)
	if m.FirstTimestamp > 1 {
		firstTime = int64(m.FirstTimestamp) - 2
	} else if len(blocks) > 0 && blocks[0].Timestamp > 1 {
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
	setupStarted := time.Now()
	if len(m.Instances) >= 500 || len(m.PrimingCalls) >= 10000 {
		fmt.Fprintf(os.Stderr, "Wasmd setup: instances=%d priming_calls=%d\n", len(m.Instances), len(m.PrimingCalls))
	}
	for instanceIndex, i := range m.Instances {
		msg := jsonBytes(i.InstantiateMsg, repl)
		addr, _, e := pk.Instantiate(ctx, codes[i.Family], admin, nil, msg, i.InstanceID, nil)
		if e != nil {
			return nil, fmt.Errorf("instantiate %s/%s: %w", i.Family, i.InstanceID, e)
		}
		contracts[i.InstanceID] = addr
		repl[i.InstanceID] = addr.String()
		if len(m.Instances) >= 500 && ((instanceIndex+1)%500 == 0 || instanceIndex+1 == len(m.Instances)) {
			fmt.Fprintf(os.Stderr, "Wasmd setup: instantiated %d/%d elapsed=%s\n", instanceIndex+1, len(m.Instances), time.Since(setupStarted).Round(time.Second))
		}
	}
	b := &benchApp{
		app:          a,
		db:           db,
		permissioned: pk,
		contracts:    contracts,
		addresses:    addrs,
		repl:         repl,
		home:         home,
	}
	primeStarted := time.Now()
	for n, c := range m.PrimingCalls {
		if e := b.executeCall(ctx, c, b.repl); e != nil {
			return nil, fmt.Errorf("priming call %d: %w", n, e)
		}
		if len(m.PrimingCalls) >= 10000 && ((n+1)%10000 == 0 || n+1 == len(m.PrimingCalls)) {
			fmt.Fprintf(os.Stderr, "Wasmd setup: primed %d/%d elapsed=%s\n", n+1, len(m.PrimingCalls), time.Since(primeStarted).Round(time.Second))
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
func (b *benchApp) close() {
	if b == nil {
		return
	}
	_ = b.app.Close()
	_ = os.RemoveAll(b.home)
}
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

func (b *benchApp) prepareBlockCalls(block ExecutionBlock) error {
	return b.prepareWorkloadCalls([]ExecutionBlock{block})
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
// branch. BlockSTM/SymbGraph/AriaFB/Vegeta already allocate a disposable
// transaction MultiStore for each attempt, so another top-level CacheContext
// would make every Wasmd store access traverse two transaction cache layers.
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

func sha256FileHex(path string) (string, error) {
	f, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer f.Close()
	h := sha256.New()
	if _, err := io.Copy(h, f); err != nil {
		return "", err
	}
	return hex.EncodeToString(h.Sum(nil)), nil
}

type serialOracleKey struct {
	sample      int
	blockNumber uint64
}

type serialOracleEntry struct {
	nanos        uint64
	transactions int
	commitID     storetypes.CommitID
}

func loadSerialOracle(path string, dataset string, workers, samples, expectedBlocks int, computeScale, iterPerNs float64, iavlCacheSize int, iavlSyncPruning bool, evaluatorSHA256 string) (map[serialOracleKey]serialOracleEntry, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()

	out := make(map[serialOracleKey]serialOracleEntry, samples*expectedBlocks)
	sc := bufio.NewScanner(f)
	sc.Buffer(make([]byte, 64*1024), 4*1024*1024)
	line := 0
	for sc.Scan() {
		line++
		if strings.TrimSpace(sc.Text()) == "" {
			continue
		}
		var rec Record
		if err := json.Unmarshal(sc.Bytes(), &rec); err != nil {
			return nil, fmt.Errorf("decode serial oracle %s line %d: %w", path, line, err)
		}
		if rec.Strategy != "cosmos-wasmd-direct-serial" {
			return nil, fmt.Errorf("serial oracle %s line %d has strategy %q", path, line, rec.Strategy)
		}
		if rec.Dataset != dataset || rec.Workers != workers || rec.Sample < 0 || rec.Sample >= samples {
			return nil, fmt.Errorf("serial oracle metadata mismatch line %d: dataset=%q workers=%d sample=%d", line, rec.Dataset, rec.Workers, rec.Sample)
		}
		if rec.ComputeScale != computeScale || rec.GoIterationsPerNano != iterPerNs {
			return nil, fmt.Errorf("serial oracle calibration mismatch line %d: scale=%g iter/ns=%.9g", line, rec.ComputeScale, rec.GoIterationsPerNano)
		}
		if rec.IAVLCacheSize != iavlCacheSize || rec.IAVLSyncPruning != iavlSyncPruning {
			return nil, fmt.Errorf("serial oracle IAVL mismatch line %d: cache=%d sync_pruning=%v; want cache=%d sync_pruning=%v", line, rec.IAVLCacheSize, rec.IAVLSyncPruning, iavlCacheSize, iavlSyncPruning)
		}
		if rec.EvaluatorSHA256 == "" || rec.EvaluatorSHA256 != evaluatorSHA256 {
			return nil, fmt.Errorf("serial oracle evaluator mismatch line %d: got=%q want=%q; regenerate serial with the current evaluator", line, rec.EvaluatorSHA256, evaluatorSHA256)
		}
		if rec.SerialCommitVersion <= 0 || rec.SerialCommitHash == "" {
			return nil, fmt.Errorf("serial oracle %s line %d lacks canonical CommitID fields; rerun isolated serial with the current harness", path, line)
		}
		hash, err := hex.DecodeString(rec.SerialCommitHash)
		if err != nil || len(hash) == 0 {
			return nil, fmt.Errorf("serial oracle %s line %d has invalid commit hash %q", path, line, rec.SerialCommitHash)
		}
		key := serialOracleKey{sample: rec.Sample, blockNumber: rec.BlockNumber}
		if _, exists := out[key]; exists {
			return nil, fmt.Errorf("serial oracle %s duplicates sample=%d block=%d", path, rec.Sample, rec.BlockNumber)
		}
		out[key] = serialOracleEntry{
			nanos:        rec.StrategyTotalNanos,
			transactions: rec.Transactions,
			commitID:     storetypes.CommitID{Version: rec.SerialCommitVersion, Hash: hash},
		}
	}
	if err := sc.Err(); err != nil {
		return nil, err
	}
	expected := samples * expectedBlocks
	if len(out) != expected {
		return nil, fmt.Errorf("serial oracle %s incomplete: rows=%d expected=%d", path, len(out), expected)
	}
	return out, nil
}

func commitIDsEqual(a, b storetypes.CommitID) bool {
	// Every strategy clone starts from the same committed setup version and
	// commits exactly once per source block. The Cosmos SDK CommitID is the
	// canonical Merkle commitment to all persistent multistore state, so after
	// commit it is the correct O(1) equivalence check. Re-scanning every KV pair
	// is prohibitively expensive for S1's multi-million-entry reconstructed
	// state and is unnecessary for per-block correctness.
	return a.Version == b.Version && len(a.Hash) > 0 && len(b.Hash) > 0 && bytes.Equal(a.Hash, b.Hash)
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
					if err := b.executeTxIsolated(ctx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); err != nil {
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
	streamPlan := flag.Bool("stream-plan", boolEnvDefault("VEGETA_WASMD_STREAM_PLAN", false), "stream the execution plan one block at a time to bound memory on large workloads")
	maxBlocks := flag.Int("max-blocks", 0, "execute only the first N plan blocks (0 = all); intended for large-workload smoke/regression runs")
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
	reuseSetupTemplate := flag.Bool("reuse-setup-template", boolEnvDefault("VEGETA_WASMD_REUSE_SETUP_TEMPLATE", true), "initialize/prime Wasmd once, snapshot the committed setup state, and clone that snapshot for every strategy/sample")
	campaignStrategy := flag.String("campaign-strategy", "all", "run one scheduler strategy per process to bound live Wasmd state: all|serial|blockstm|ariafb|symbgraph-rust|vegeta|exact-oracle")
	serialOraclePath := flag.String("serial-oracle", "", "serial strategy JSONL with per-block CommitID/timing oracle; avoids a second live Wasmd app in isolated non-serial campaigns")
	profileDir := flag.String(
		"profile-dir",
		os.Getenv("VEGETA_S3_WASMD_PPROF_DIR"),
		"write an additional unmeasured 4-worker Wasmd TxRunner CPU/mutex/block profile to this directory",
	)
	campaignProfileDir := flag.String(
		"campaign-profile-dir",
		os.Getenv("VEGETA_WASMD_CAMPAIGN_PROFILE_DIR"),
		"write CPU + post-GC live-heap profiles for the measured campaign loop to this directory",
	)
	iavlCacheSize := flag.Int(
		"iavl-cache-size",
		intEnvDefault("VEGETA_WASMD_IAVL_CACHE_SIZE", 500_000),
		"IAVL node-cache size per mounted store; 0 disables the node cache (useful with MemDB to avoid duplicating state in Go heap)",
	)
	iavlSyncPruning := flag.Bool(
		"iavl-sync-pruning",
		boolEnvDefault("VEGETA_WASMD_IAVL_SYNC_PRUNING", false),
		"finish IAVL pruning inside Commit instead of allowing asynchronous pruning to overlap the next measured block",
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
	datasetLabel := flag.String("dataset", rustEnvOr("VEGETA_WASMD_DATASET", "vegeta-s3-wasmd-blockstm"), "dataset label written to every record")
	exactOracle := flag.Bool("exact-oracle", boolEnvDefault("VEGETA_WASMD_EXACT_ORACLE", true), "run the hindsight-only exact-access ACG oracle; disable for large datasets without exact SLOAD/SSTORE traces")
	exactTraceDirDefault := os.Getenv("VEGETA_S3_EXACT_TRACE_DIR")
	if exactTraceDirDefault == "" {
		exactTraceDirDefault = "benchmarks/corpora/vegeta-ethereum/s3-exact-sload-sstore/tx-traces"
	}
	exactTraceDir := flag.String(
		"exact-trace-dir",
		exactTraceDirDefault,
		"evaluation-only exact Ethereum SLOAD/SSTORE trace directory for the Rust-ACG perfect-access oracle",
	)
	exactNativeAccessesDefault := os.Getenv("VEGETA_S3_EXACT_NATIVE_ACCESSES")
	if exactNativeAccessesDefault == "" {
		exactNativeAccessesDefault = "benchmarks/corpora/vegeta-ethereum/s3/native-execution/native-accesses.jsonl"
	}
	exactNativeAccesses := flag.String(
		"exact-native-accesses",
		exactNativeAccessesDefault,
		"frozen native-translation access audit used only to compensate EVM->Wasmd translation-induced RAW aliases in the Rust-ACG perfect-access oracle",
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
	rustACGOnly := flag.Bool("rust-acg-only", boolEnv("VEGETA_S3_RUST_ACG_ONLY"), "run only matched direct-serial + Rust ACG + ACG-Oracle rows (skip BlockSTM, AriaFB, and Vegeta) for policy diagnostics")
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
	if *iavlCacheSize < 0 {
		panic("--iavl-cache-size must be >= 0")
	}
	benchmarkIAVLCacheSize = *iavlCacheSize
	benchmarkIAVLSyncPruning = *iavlSyncPruning
	fmt.Fprintf(os.Stderr, "Wasmd IAVL config: cache_size=%d sync_pruning=%v pruning=everything\n", benchmarkIAVLCacheSize, benchmarkIAVLSyncPruning)
	executablePath, executableErr := os.Executable()
	if executableErr != nil {
		panic(fmt.Errorf("resolve evaluator executable: %w", executableErr))
	}
	evaluatorSHA256, evaluatorHashErr := sha256FileHex(executablePath)
	if evaluatorHashErr != nil {
		panic(fmt.Errorf("hash evaluator executable: %w", evaluatorHashErr))
	}
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
	*campaignStrategy = strings.ToLower(strings.TrimSpace(*campaignStrategy))
	switch *campaignStrategy {
	case "all", "serial", "blockstm", "ariafb", "symbgraph-rust", "vegeta", "exact-oracle":
	default:
		panic(fmt.Sprintf("unsupported --campaign-strategy=%q", *campaignStrategy))
	}
	if *campaignStrategy == "exact-oracle" && !*exactOracle {
		panic("--campaign-strategy=exact-oracle requires --exact-oracle=true")
	}
	if *rustACGOnly && *campaignStrategy != "all" {
		panic("--rust-acg-only is incompatible with --campaign-strategy isolation")
	}
	if *rustACGOnly && !*exactOracle {
		panic("--rust-acg-only requires --exact-oracle=true")
	}
	setupSDKConfig()
	if *calOnly {
		fmt.Printf("%.9f\n", calibrateIterationsPerNano())
		return
	}
	if *manifestPath == "" || *planPath == "" || *workers < 1 || *samples < 1 || *maxBlocks < 0 {
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
	var blocks []ExecutionBlock
	planBlockCount := manifest.Blocks
	var e error
	if *streamPlan {
		if len(manifest.LogicalAddresses) == 0 {
			panic("--stream-plan requires an execution manifest with logical_addresses; regenerate it with the large-workload native preparation pipeline")
		}
		_, e = firstPlanBlock(*planPath)
		if e != nil {
			panic(e)
		}
		if planBlockCount <= 0 {
			panic("--stream-plan requires execution manifest block count metadata; regenerate the manifest")
		}
	} else {
		blocks, e = readPlan(*planPath)
		if e != nil {
			panic(e)
		}
		if len(blocks) == 0 {
			panic("empty execution plan")
		}
		planBlockCount = len(blocks)
	}
	if *maxBlocks > 0 && *maxBlocks < planBlockCount {
		planBlockCount = *maxBlocks
		if !*streamPlan {
			blocks = blocks[:planBlockCount]
		}
	}
	if *streamPlan && (*investigateOverhead || *profileOnlyRunner != "" || *symbProfileDir != "") {
		panic("--stream-plan is for publication matrix execution; unmeasured whole-campaign profilers require the in-memory S3 plan")
	}
	if *setupOnly {
		initBlocks := blocks
		if *streamPlan {
			initBlocks = nil
		}
		b, e := newBenchApp(*repoRoot, manifest, initBlocks)
		if e != nil {
			panic(e)
		}
		b.close()
		fmt.Printf("PASS: Wasmd/WasmVM setup dataset=%s contracts=%d blocks=%d scope=%s stream-plan=%v max-blocks=%d\n", *datasetLabel, len(manifest.Instances), planBlockCount, baselineScope, *streamPlan, *maxBlocks)
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
	var exactTraceIndex *exactEthereumTraceIndex
	var exactNativeTranslationIndex *exactNativeTranslationIndex
	if *exactOracle {
		exactTraceIndex, e = loadExactEthereumTraceIndex(resolveRepoPath(*repoRoot, *exactTraceDir))
		if e != nil {
			panic(e)
		}
		exactNativeTranslationIndex, e = loadExactNativeTranslationIndex(resolveRepoPath(*repoRoot, *exactNativeAccesses))
		if e != nil {
			panic(fmt.Errorf("load ACG-Oracle translation compensation: %w", e))
		}
	}
	f, e := os.Create(*output)
	if e != nil {
		panic(e)
	}
	defer f.Close()
	w := bufio.NewWriter(f)
	defer w.Flush()
	fmt.Fprintf(os.Stderr, "wasmd scheduler matrix dataset=%s sdk=%s wasmd=%s workers=%d samples=%d go-iter/ns=%.6f symbolic=rust-acg:%s exact-oracle=%v variant=%s rust-only=%v max-blocks=%d\n", *datasetLabel, cosmosSDKVersion, wasmdVersion, *workers, *samples, *iterPerNs, resolveRepoPath(*repoRoot, *symbolicDir), *exactOracle, rustRunnerOptions.Variant(), *rustACGOnly, *maxBlocks)
	var diagnosticDirectNanos uint64
	var diagnosticSerialDigests [][32]byte
	if *investigateOverhead {
		diagnosticSerialDigests = make([][32]byte, len(blocks))
	}
	runAllStrategies := *campaignStrategy == "all"
	runSerialRecord := runAllStrategies || *campaignStrategy == "serial"
	runBlockSTM := runAllStrategies || *campaignStrategy == "blockstm"
	runAriaFB := runAllStrategies || *campaignStrategy == "ariafb"
	runSymbGraph := runAllStrategies || *campaignStrategy == "symbgraph-rust"
	runVegeta := runAllStrategies || *campaignStrategy == "vegeta"
	runExactOracle := *exactOracle && (runAllStrategies || *campaignStrategy == "exact-oracle")
	useSerialOracle := !runAllStrategies && !runSerialRecord && strings.TrimSpace(*serialOraclePath) != ""
	var serialOracle map[serialOracleKey]serialOracleEntry
	if useSerialOracle {
		serialOracle, e = loadSerialOracle(*serialOraclePath, *datasetLabel, *workers, *samples, planBlockCount, *scale, *iterPerNs, benchmarkIAVLCacheSize, benchmarkIAVLSyncPruning, evaluatorSHA256)
		if e != nil {
			panic(fmt.Errorf("load serial CommitID oracle: %w", e))
		}
		fmt.Fprintf(os.Stderr, "Wasmd serial oracle: loaded rows=%d source=%s live_serial_app=false\n", len(serialOracle), *serialOraclePath)
	}
	if *investigateOverhead && useSerialOracle {
		panic("--investigate-overhead requires a live serial app; omit --serial-oracle")
	}
	if runAllStrategies {
		fmt.Fprintln(os.Stderr, "Wasmd campaign strategy=all live_state_mode=legacy-matrix")
	} else {
		fmt.Fprintf(os.Stderr, "Wasmd campaign strategy=%s live_state_mode=isolated\n", *campaignStrategy)
	}
	initBlocks := blocks
	if *streamPlan {
		initBlocks = nil
	}
	var setupTemplate *benchAppTemplate
	if *reuseSetupTemplate {
		started := time.Now()
		fmt.Fprintf(os.Stderr, "Wasmd setup template: building once for %d instances / %d priming calls\n", len(manifest.Instances), len(manifest.PrimingCalls))
		seed, err := newBenchApp(*repoRoot, manifest, initBlocks)
		if err != nil {
			panic(err)
		}
		setupTemplate, err = captureBenchAppTemplate(seed)
		seed.close()
		// The setup seed can hold hundreds of megabytes of MemDB/IAVL state.
		// Reclaim it before creating strategy clones so its dead heap does not
		// overlap the live benchmark apps and trigger late stop-the-world GC.
		runtime.GC()
		if err != nil {
			panic(err)
		}
		defer setupTemplate.close()
		fmt.Fprintf(os.Stderr, "Wasmd setup template: ready db_entries=%d state_bytes=%d storage=disk-snapshot elapsed=%s\n", setupTemplate.dbEntries, setupTemplate.stateBytes, time.Since(started).Round(time.Second))
	}
	newSampleApp := func(sample int, role string, verify bool) (*benchApp, error) {
		started := time.Now()
		var app *benchApp
		var err error
		if setupTemplate != nil {
			app, err = newBenchAppFromTemplate(setupTemplate, initBlocks, verify)
		} else {
			app, err = newBenchApp(*repoRoot, manifest, initBlocks)
		}
		if err == nil && setupTemplate != nil {
			fmt.Fprintf(os.Stderr, "Wasmd setup clone: sample=%d role=%s elapsed=%s\n", sample, role, time.Since(started).Round(time.Millisecond))
		}
		return app, err
	}
	for sample := 0; sample < *samples; sample++ {
		rustBridge, e := NewRustSymbGraphBridgeWithPlanningAndDiagnostics(*repoRoot, *symbolicDir, rustPlanningOverrides, *rustDependencyDiagnostics)
		if e != nil {
			panic(e)
		}
		var serial *benchApp
		if !useSerialOracle {
			serial, e = newSampleApp(sample, "serial", true)
			if e != nil {
				rustBridge.Close()
				panic(e)
			}
		}
		var stm, aria, symb, vegeta, acgOracle *benchApp
		if runBlockSTM {
			stm, e = newSampleApp(sample, "blockstm", false)
			if e != nil {
				serial.close()
				rustBridge.Close()
				panic(e)
			}
		}
		if runAriaFB {
			aria, e = newSampleApp(sample, "ariafb", false)
			if e != nil {
				serial.close()
				if stm != nil {
					stm.close()
				}
				rustBridge.Close()
				panic(e)
			}
		}
		if runSymbGraph {
			symb, e = newSampleApp(sample, "symbgraph-rust", false)
			if e != nil {
				serial.close()
				if stm != nil {
					stm.close()
				}
				if aria != nil {
					aria.close()
				}
				rustBridge.Close()
				panic(e)
			}
		}
		if runVegeta {
			vegeta, e = newSampleApp(sample, "vegeta", false)
			if e != nil {
				serial.close()
				if stm != nil {
					stm.close()
				}
				if aria != nil {
					aria.close()
				}
				if symb != nil {
					symb.close()
				}
				rustBridge.Close()
				panic(e)
			}
		}
		if runExactOracle {
			acgOracle, e = newSampleApp(sample, "acg-oracle", false)
			if e != nil {
				serial.close()
				if stm != nil {
					stm.close()
				}
				if aria != nil {
					aria.close()
				}
				if symb != nil {
					symb.close()
				}
				if vegeta != nil {
					vegeta.close()
				}
				rustBridge.Close()
				panic(e)
			}
		}

		func() {
			defer rustBridge.Close()
			defer serial.close()
			if stm != nil {
				defer stm.close()
			}
			if aria != nil {
				defer aria.close()
			}
			if symb != nil {
				defer symb.close()
			}
			if vegeta != nil {
				defer vegeta.close()
			}
			if acgOracle != nil {
				defer acgOracle.close()
			}

			const preEstimate = false
			// Store keys are app-local identity objects. Prefer the Block-STM app;
			// isolated campaigns backed by the persisted serial oracle intentionally
			// keep no serial Wasmd app alive. Other strategies still construct this
			// unused runner from one of their existing apps to keep setup uniform.
			runnerStoreApp := serial
			for _, candidate := range []*benchApp{stm, aria, symb, vegeta, acgOracle} {
				if candidate != nil {
					runnerStoreApp = candidate
					break
				}
			}
			if runnerStoreApp == nil {
				panic("no live Wasmd app available for scheduler store keys")
			}
			blockSTMRunner := txnrunner.NewSTMRunner(
				sdk.TxDecoder(func([]byte) (sdk.Tx, error) { return nil, nil }),
				runnerStoreApp.app.GetStoreKeys(),
				*workers,
				preEstimate,
				func(storetypes.MultiStore) string { return sdk.DefaultBondDenom },
			)
			ariaRunner := NewAriaFBRunner(*workers)
			vegetaRunner := NewVegetaRunner(*workers)

			// Optional campaign-only profiling starts after setup restoration and app
			// cloning so profiles describe scheduler/execution work rather than the
			// multi-million-entry setup snapshot. The heap profile is captured after
			// CPU profiling stops and a forced GC while the live strategy app is still
			// reachable, making retained state and branch costs visible.
			if *campaignProfileDir != "" {
				if e := os.MkdirAll(*campaignProfileDir, 0o755); e != nil {
					panic(e)
				}
				profilePrefix := fmt.Sprintf("%s-sample%d", *campaignStrategy, sample)
				cpuPath := filepath.Join(*campaignProfileDir, profilePrefix+".cpu.pprof")
				cpuFile, e := os.Create(cpuPath)
				if e != nil {
					panic(e)
				}
				if e := pprof.StartCPUProfile(cpuFile); e != nil {
					_ = cpuFile.Close()
					panic(e)
				}
				fmt.Fprintf(os.Stderr, "Wasmd campaign profiling: cpu=%s\n", cpuPath)
				defer func() {
					pprof.StopCPUProfile()
					if e := cpuFile.Close(); e != nil {
						fmt.Fprintf(os.Stderr, "warning: close campaign CPU profile: %v\n", e)
					}
					runtime.GC()
					heapPath := filepath.Join(*campaignProfileDir, profilePrefix+".heap.pprof")
					if e := writeRuntimeProfile("heap", heapPath); e != nil {
						fmt.Fprintf(os.Stderr, "warning: write campaign heap profile: %v\n", e)
					} else {
						fmt.Fprintf(os.Stderr, "Wasmd campaign profiling: heap=%s\n", heapPath)
					}
				}()
			}

			campaignStarted := time.Now()
			fmt.Fprintln(os.Stderr, "Wasmd state equivalence: canonical committed-state CommitID checks")
			var cumulativeSerialNanos uint64
			var cumulativeBlockSTMNanos uint64
			var cumulativeBlockSTMTx uint64
			var cumulativeBlockSTMAttempts uint64
			var cumulativeVegetaNanos uint64
			var cumulativeVegetaPreNanos uint64
			var cumulativeVegetaPostNanos uint64
			var cumulativeVegetaReferenceNanos uint64
			var cumulativeVegetaReexecutions uint64
			var cumulativeVegetaSafetyReplays uint64
			var cumulativeVegetaCanonicalFallbackBlocks uint64
			var cumulativeVegetaSnapshotBuildNanos uint64
			var cumulativeVegetaSnapshotPointHits uint64
			var cumulativeVegetaSnapshotPointMisses uint64
			var cumulativeVegetaPostBatches uint64
			var cumulativeVegetaPostSingletonBatches uint64
			var cumulativeVegetaPostMaxBatch uint64
			var cumulativeVegetaReadySelectionNanos uint64
			var cumulativeVegetaAlg3ValidationNanos uint64
			var cumulativeVegetaRangeValidationNanos uint64
			var cumulativeVegetaIntrinsicReexecutionNanos uint64
			var cumulativeVegetaHistoricalFallbackNanos uint64
			var cumulativeVegetaPreExecWorkNanos uint64
			var cumulativeVegetaPreExecSpanNanos uint64
			var cumulativeVegetaPostExecWorkNanos uint64
			var cumulativeVegetaPostExecSpanNanos uint64
			var cumulativeVegetaPostWideExecWorkNanos uint64
			var cumulativeVegetaPostWideExecSpanNanos uint64
			var cumulativeVegetaPostWideTransactions uint64
			var cumulativeVegetaLongestChainSum uint64
			var cumulativeVegetaTransactions uint64

			processBlock := func(blockOffset int, block ExecutionBlock) {
				header := tmproto.Header{ChainID: chainID, Height: int64(blockOffset + 2), Time: time.Unix(int64(block.Timestamp), 0)}

				// Historical-order direct Wasmd serial is the common throughput control.
				// Isolated non-serial campaigns consume its persisted per-block timing and
				// canonical CommitID instead of keeping a second mutable Wasmd app alive.
				// Vegeta and AriaFB stage their scheduler result plus short-lived serial
				// safety branches on one live app. Neither keeps a second full Wasmd
				// reference app alive across the campaign.
				var serialNanos uint64
				var serialCommitID storetypes.CommitID
				if useSerialOracle {
					oracle, ok := serialOracle[serialOracleKey{sample: sample, blockNumber: block.BlockNumber}]
					if !ok {
						panic(fmt.Sprintf("serial oracle missing sample=%d block=%d", sample, block.BlockNumber))
					}
					if oracle.transactions != len(block.Transactions) {
						panic(fmt.Sprintf("serial oracle transaction-count mismatch sample=%d block=%d oracle=%d plan=%d", sample, block.BlockNumber, oracle.transactions, len(block.Transactions)))
					}
					serialNanos = oracle.nanos
					serialCommitID = oracle.commitID
				} else {
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
					serialNanos = uint64(serialWall.Nanoseconds())
					serialCommitID = serial.app.LastCommitID()
					if *investigateOverhead && sample == 0 {
						diagnosticDirectNanos += serialNanos
						// The overhead diagnostic intentionally retains the historical full
						// state digest. Normal publication/smoke runs use CommitID below.
						diagnosticSerialDigests[blockOffset] = digestApp(serial.app)
					}
					serialRec := Record{
						SchemaVersion:         1,
						Dataset:               *datasetLabel,
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
						SerialCommitVersion:   serialCommitID.Version,
						SerialCommitHash:      hex.EncodeToString(serialCommitID.Hash),
						ComputeMetric:         cal.Metric,
						ComputeScale:          *scale,
						GoIterationsPerNano:   *iterPerNs,
						CosmosSDKVersion:      cosmosSDKVersion,
						WasmdVersion:          wasmdVersion,
						IAVLCacheSize:         benchmarkIAVLCacheSize,
						IAVLSyncPruning:       benchmarkIAVLSyncPruning,
						EvaluatorSHA256:       evaluatorSHA256,
						BaselineScope:         directSerialScope,
						BlockSTMPreEstimate:   false,
					}
					if runSerialRecord {
						if e := json.NewEncoder(w).Encode(&serialRec); e != nil {
							panic(e)
						}
					}
				}
				cumulativeSerialNanos += serialNanos

				estimatedCosts := make([]uint32, len(block.Transactions))
				for i, tx := range block.Transactions {
					estimatedCosts[i] = boundedCost(cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash))
				}

				if runExactOracle {
					// Evaluation-only Rust-ACG upper bound. The source exact SLOAD/SSTORE
					// trace replaces symbolic access inference, but execution is the same
					// ready-DAG MVCC path and indexed canonical validation used by Rust-ACG.
					// Exact source reads receive only the hard RAW visibility dependencies
					// required by canonical validation; adapter bank/funds resources are
					// merged, and any runtime replay aborts the campaign.
					oracleBlockCtx := acgOracle.app.NewNextBlockContext(header)
					oracleStart := time.Now()
					oraclePlanStarted := time.Now()
					oraclePlan, oracleTraceDiag, oracleErr := exactTraceIndex.buildExactTraceACGPlanWithTranslation(block, exactNativeTranslationIndex)
					oraclePlanNanos := uint64(time.Since(oraclePlanStarted).Nanoseconds())
					if oracleErr != nil {
						panic(oracleErr)
					}
					oracleRunner := NewRustSymbGraphExactTraceOracleRunner(*workers, block, estimatedCosts, oraclePlan, oraclePlanNanos, rustRunnerOptions)
					oracleRunner.SetSerialServiceNanos(serialNanos)
					_, oracleErr = oracleRunner.Run(context.Background(), oracleBlockCtx.MultiStore(), txBytes(block), func(_ []byte, _ sdk.Tx, ms storetypes.MultiStore, idx int, _ map[string]any) *abci.ExecTxResult {
						ctx := oracleBlockCtx.WithMultiStore(ms).WithEventManager(sdk.NewEventManager()).WithGasMeter(storetypes.NewInfiniteGasMeter())
						tx := block.Transactions[idx]
						if err := acgOracle.executeTxIsolated(ctx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); err != nil {
							return &abci.ExecTxResult{Code: 1, Log: err.Error()}
						}
						return &abci.ExecTxResult{}
					})
					oracleWall := uint64(time.Since(oracleStart).Nanoseconds())
					if oracleErr != nil {
						panic(oracleErr)
					}
					if err := commitFinalizeState(acgOracle.app); err != nil {
						panic(err)
					}
					oracleEq := commitIDsEqual(serialCommitID, acgOracle.app.LastCommitID())
					if !oracleEq {
						panic(fmt.Sprintf("exact-trace Rust-ACG oracle state mismatch sample=%d block=%d", sample, block.BlockNumber))
					}
					oracleStats := oracleRunner.LastStats()
					oracleDiag := oracleRunner.LastDiagnostics()
					if oracleStats.Reexecutions != 0 {
						panic(fmt.Sprintf("exact-trace Rust-ACG oracle violated zero-replay contract sample=%d block=%d reexecutions=%d", sample, block.BlockNumber, oracleStats.Reexecutions))
					}
					oracleRec := Record{
						SchemaVersion: 1, Dataset: *datasetLabel, Sample: sample, BlockNumber: block.BlockNumber, Strategy: "cosmos-wasmd-symbgraph-rust-exact-trace-oracle", Workers: *workers,
						MatchedSerialNanos: serialNanos, HistoricalSerialNanos: serialNanos, StrategyTotalNanos: oracleWall, PreConsensusNanos: oracleStats.PreConsensusNanos, PostConsensusNanos: oracleStats.PostConsensusNanos, Transactions: len(block.Transactions),
						ExecutionAttempts: oracleStats.Attempts, Reexecutions: oracleStats.Reexecutions, SerialEquivalent: oracleEq, SerialReferenceScope: "historical-block-order", ComputeMetric: cal.Metric, ComputeScale: *scale,
						GoIterationsPerNano: *iterPerNs, CosmosSDKVersion: cosmosSDKVersion, WasmdVersion: wasmdVersion, IAVLCacheSize: benchmarkIAVLCacheSize, IAVLSyncPruning: benchmarkIAVLSyncPruning, EvaluatorSHA256: evaluatorSHA256, BaselineScope: exactACGOracleScope, BlockSTMPreEstimate: false,
						SpeculatedTransactions: oracleStats.Speculated, ReusedTransactions: oracleStats.Reused, ValidationNanos: oracleDiag.ValidationNanos, ReplayExecutionNanos: oracleDiag.ReplayExecutionNanos,
						DiscoveredConflicts: uint64(oracleTraceDiag.DependencyEdges), OracleSourceReads: oracleTraceDiag.SourceReads, OracleSourceWrites: oracleTraceDiag.SourceWrites,
						OracleSourceTraceMissing: oracleTraceDiag.MissingTraces, OracleAdapterHardEdges: oracleTraceDiag.AdapterHardEdges, OracleTranslationCompensationEdges: oracleTraceDiag.TranslationCompensationEdges, OracleMissingBarrierEdges: oracleTraceDiag.MissingBarrierEdges,
						SymbGraphVariant: oracleDiag.Variant, SymbPlanNanos: oracleDiag.PlanNanos, SymbPreexecutionNanos: oracleDiag.PreexecutionNanos, SymbBranchCreateNanos: oracleDiag.BranchCreateNanos,
						SymbVisibilityNanos: oracleDiag.VisibilityNanos, SymbSpecExecutionNanos: oracleDiag.SpecExecutionNanos, SymbDeltaCaptureNanos: oracleDiag.DeltaCaptureNanos, SymbMVCCPublishNanos: oracleDiag.MVCCPublishNanos,
						SymbReconciliationNanos: oracleDiag.ReconciliationNanos, SymbValidationNanos: oracleDiag.ValidationNanos, SymbReplayExecutionNanos: oracleDiag.ReplayExecutionNanos,
						SymbDependencyEdges: oracleDiag.DependencyEdges, SymbPhysicalCandidateEdges: oracleDiag.PhysicalCandidateEdges, SymbLogicalCandidateEdges: oracleDiag.LogicalCandidateEdges,
						SymbParentDependenciesBeforeReduction: oracleDiag.ParentDependenciesBeforeReduction, SymbParentDependenciesElidedReduction: oracleDiag.ParentDependenciesElidedReduction,
						SymbInitialReady: oracleDiag.InitialReady, SymbMaxReady: oracleDiag.MaxReady, SymbAverageReady: oracleDiag.AverageReady(), SymbMaxActive: oracleDiag.MaxActive,
						SymbCriticalPathTx: oracleDiag.CriticalPathTx, SymbCriticalPathCost: oracleDiag.CriticalPathCost, SymbTotalEstimatedCost: oracleDiag.TotalEstimatedCost, SymbDAGParallelism: oracleDiag.DAGParallelism,
						SymbWorkerUtilization: oracleDiag.WorkerUtilization, SymbWorkerIdleNanos: oracleDiag.WorkerIdleNanos, SymbMVCCPointReads: oracleDiag.MVCCPointReads, SymbMVCCVersionHits: oracleDiag.MVCCVersionHits,
						SymbMVCCBaseFallbacks: oracleDiag.MVCCBaseFallbacks, SymbMVCCRangeReads: oracleDiag.MVCCRangeReads, SymbMVCCRangeOverlayKeys: oracleDiag.MVCCRangeOverlayKeys,
						SymbMVCCPublishes: oracleDiag.MVCCPublishes, SymbMVCCPublishedKeys: oracleDiag.MVCCPublishedKeys, SymbCandidateHard: oracleDiag.CandidateHard, SymbOrderedHard: oracleDiag.OrderedHard,
					}
					if oracleRec.StrategyTotalNanos > 0 {
						oracleRec.MatchedSerialSpeedup = float64(oracleRec.MatchedSerialNanos) / float64(oracleRec.StrategyTotalNanos)
					}
					if e := json.NewEncoder(w).Encode(&oracleRec); e != nil {
						panic(e)
					}
				}

				if runBlockSTM && !*rustACGOnly {
					// Cosmos SDK Block-STM: native SDK MVCC/scheduler baseline.
					stmBlockCtx := stm.app.NewNextBlockContext(header)
					var stmAttempts atomic.Uint64
					stmStart := time.Now()
					_, e := blockSTMRunner.Run(context.Background(), stmBlockCtx.MultiStore(), txBytes(block), func(_ []byte, _ sdk.Tx, ms storetypes.MultiStore, idx int, _ map[string]any) *abci.ExecTxResult {
						stmAttempts.Add(1)
						ctx := stmBlockCtx.WithMultiStore(ms).WithEventManager(sdk.NewEventManager()).WithGasMeter(storetypes.NewInfiniteGasMeter())
						tx := block.Transactions[idx]
						if e := stm.executeTxIsolated(ctx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); e != nil {
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
					stmEq := commitIDsEqual(serialCommitID, stm.app.LastCommitID())
					if !stmEq {
						panic(fmt.Sprintf("block-stm state mismatch sample=%d block=%d", sample, block.BlockNumber))
					}
					stmA := stmAttempts.Load()
					cumulativeBlockSTMNanos += uint64(stmWall.Nanoseconds())
					cumulativeBlockSTMTx += uint64(len(block.Transactions))
					cumulativeBlockSTMAttempts += stmA
					stmRec := Record{SchemaVersion: 1, Dataset: *datasetLabel, Sample: sample, BlockNumber: block.BlockNumber, Strategy: "cosmos-wasmd-block-stm", Workers: *workers, MatchedSerialNanos: serialNanos, HistoricalSerialNanos: serialNanos, StrategyTotalNanos: uint64(stmWall.Nanoseconds()), PostConsensusNanos: uint64(stmWall.Nanoseconds()), Transactions: len(block.Transactions), ExecutionAttempts: stmA, Reexecutions: stmA - uint64(len(block.Transactions)), SerialEquivalent: stmEq, SerialReferenceScope: "historical-block-order", ComputeMetric: cal.Metric, ComputeScale: *scale, GoIterationsPerNano: *iterPerNs, CosmosSDKVersion: cosmosSDKVersion, WasmdVersion: wasmdVersion, IAVLCacheSize: benchmarkIAVLCacheSize, IAVLSyncPruning: benchmarkIAVLSyncPruning, EvaluatorSHA256: evaluatorSHA256, BaselineScope: baselineScope, BlockSTMPreEstimate: preEstimate}
					if stmRec.StrategyTotalNanos > 0 {
						stmRec.MatchedSerialSpeedup = float64(stmRec.MatchedSerialNanos) / float64(stmRec.StrategyTotalNanos)
					}
					if e := json.NewEncoder(w).Encode(&stmRec); e != nil {
						panic(e)
					}

				}

				if runAriaFB && !*rustACGOnly {
					// AriaFB is allowed to speculate and derive an alternative serialization,
					// but this benchmark consumes a fixed historical Ethereum block stream. A
					// different post-block state can make source-successful transactions in later
					// blocks invalid (allowance/ownership/nonces). Stage the Aria block, build a
					// disposable historical-order branch from the same block-start state, and
					// commit Aria only when both branches have identical final writes. Semantic
					// execution failures under the derived order trigger the same canonical
					// fallback instead of poisoning persistent state or panicking.
					ariaBlockCtx := aria.app.NewNextBlockContext(header)
					ariaBranch := newTrackingMultiStoreForSpeculation(ariaBlockCtx.MultiStore(), aria.app.GetStoreKeys())
					ariaStart := time.Now()
					_, ariaErr := ariaRunner.Run(context.Background(), ariaBranch, txBytes(block), func(_ []byte, _ sdk.Tx, ms storetypes.MultiStore, idx int, _ map[string]any) *abci.ExecTxResult {
						ctx := ariaBlockCtx.WithMultiStore(ms).WithEventManager(sdk.NewEventManager()).WithGasMeter(storetypes.NewInfiniteGasMeter())
						tx := block.Transactions[idx]
						if e := aria.executeTxIsolated(ctx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); e != nil {
							return &abci.ExecTxResult{Code: 1, Log: e.Error()}
						}
						return &abci.ExecTxResult{}
					})
					ariaWall := uint64(time.Since(ariaStart).Nanoseconds())
					var semanticErr *ariaSemanticExecutionError
					semanticFallback := errors.As(ariaErr, &semanticErr)
					if ariaErr != nil && !semanticFallback {
						panic(ariaErr)
					}

					canonicalBranch, canonicalReplayNanos, e := executeHistoricalBlockBranch(aria, ariaBlockCtx, block, cal)
					if e != nil {
						panic(fmt.Sprintf("aria-fb canonical safety replay failed after derived-order issue=%v: %v", ariaErr, e))
					}
					canonicalMismatch := !semanticFallback && !trackingBranchesFinalStateEqual(ariaBranch, canonicalBranch)
					canonicalFallback := semanticFallback || canonicalMismatch
					ariaStats := ariaRunner.LastStats()
					strategyNanos := ariaWall
					if canonicalFallback {
						reason := "post-block state differs from historical order"
						if semanticFallback {
							reason = semanticErr.Error()
						}
						fmt.Fprintf(os.Stderr, "Wasmd Aria canonical fallback: sample=%d block=%d txs=%d reason=%s\n", sample, block.BlockNumber, len(block.Transactions), reason)
						canonicalBranch.Write()
						ariaRunner.MarkCanonicalFallback(len(block.Transactions))
						strategyNanos += canonicalReplayNanos
						ariaStats.Attempts += uint64(len(block.Transactions))
						ariaStats.Reexecutions += uint64(len(block.Transactions))
						ariaStats.SafetyReplays += uint64(len(block.Transactions))
						ariaStats.ReplayExecutionNanos += canonicalReplayNanos
						ariaStats.PostConsensusNanos += canonicalReplayNanos
					} else {
						ariaBranch.Write()
					}
					if e := commitFinalizeState(aria.app); e != nil {
						panic(e)
					}
					ariaEq := commitIDsEqual(serialCommitID, aria.app.LastCommitID())
					if !ariaEq {
						panic(fmt.Sprintf("aria-fb canonical state mismatch sample=%d block=%d fallback=%v semantic=%v order=%v", sample, block.BlockNumber, canonicalFallback, semanticFallback, ariaRunner.LastSerializationOrder()))
					}
					ariaRec := Record{
						SchemaVersion: 1, Dataset: *datasetLabel, Sample: sample, BlockNumber: block.BlockNumber, Strategy: "cosmos-wasmd-aria-fb", Workers: *workers,
						MatchedSerialNanos: serialNanos, HistoricalSerialNanos: serialNanos, StrategyTotalNanos: strategyNanos, PostConsensusNanos: ariaStats.PostConsensusNanos, Transactions: len(block.Transactions),
						ExecutionAttempts: ariaStats.Attempts, Reexecutions: ariaStats.Reexecutions, SerialEquivalent: ariaEq, SerialReferenceScope: "historical-block-order", ComputeMetric: cal.Metric, ComputeScale: *scale,
						GoIterationsPerNano: *iterPerNs, CosmosSDKVersion: cosmosSDKVersion, WasmdVersion: wasmdVersion, IAVLCacheSize: benchmarkIAVLCacheSize, IAVLSyncPruning: benchmarkIAVLSyncPruning, EvaluatorSHA256: evaluatorSHA256, BaselineScope: ariaFBScope, BlockSTMPreEstimate: false,
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

				if runSymbGraph {
					// SymbGraph Rust: crates/acg-* owns symbolic parsing, candidate graph
					// construction, conflict predicates, adaptive feedback, and risk-bounded
					// ordering. Go executes only the emitted ordering_dependencies DAG against
					// real Wasmd/WasmVM state and performs concrete validation/replay.
					symbBlockCtx := symb.app.NewNextBlockContext(header)
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
					symbEq := commitIDsEqual(serialCommitID, symb.app.LastCommitID())
					if !symbEq {
						panic(fmt.Sprintf("symbgraph-rust state mismatch sample=%d block=%d", sample, block.BlockNumber))
					}
					symbStats := symbRunner.LastStats()
					symbDiag := symbRunner.LastDiagnostics()
					symbPlan := symbRunner.LastPlan()
					symbRec := Record{
						SchemaVersion: 1, Dataset: *datasetLabel, Sample: sample, BlockNumber: block.BlockNumber,
						Strategy: "cosmos-wasmd-symbgraph-rust", Workers: *workers, MatchedSerialNanos: serialNanos, HistoricalSerialNanos: serialNanos,
						StrategyTotalNanos: uint64(symbWall.Nanoseconds()), PreConsensusNanos: symbStats.PreConsensusNanos, PostConsensusNanos: symbStats.PostConsensusNanos, Transactions: len(block.Transactions), ExecutionAttempts: symbStats.Attempts,
						Reexecutions: symbStats.Reexecutions, SerialEquivalent: symbEq, SerialReferenceScope: "historical-block-order", ComputeMetric: cal.Metric, ComputeScale: *scale,
						GoIterationsPerNano: *iterPerNs, CosmosSDKVersion: cosmosSDKVersion, WasmdVersion: wasmdVersion,
						IAVLCacheSize: benchmarkIAVLCacheSize, IAVLSyncPruning: benchmarkIAVLSyncPruning, EvaluatorSHA256: evaluatorSHA256,
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

				}

				if runVegeta && !*rustACGOnly {
					// Vegeta speculation is staged at block scope so its derived-order safety
					// reference can be short-lived. The previous implementation kept a full
					// vegeta-reference Wasmd app alive for all 5,000 blocks, roughly doubling
					// mutable state and causing severe late-run GC pauses. Here both the
					// scheduler result and serial safety replay branch from the same block-start
					// state on one app. A historical-order branch gates the committed state so
					// scheduler reordering cannot poison later source-successful transactions.
					vegetaBlockCtx := vegeta.app.NewNextBlockContext(header)
					vegetaBranch := newTrackingMultiStoreForSpeculation(vegetaBlockCtx.MultiStore(), vegeta.app.GetStoreKeys())
					vegetaStart := time.Now()
					_, vegetaErr := vegetaRunner.Run(context.Background(), vegetaBranch, txBytes(block), func(_ []byte, _ sdk.Tx, ms storetypes.MultiStore, idx int, _ map[string]any) *abci.ExecTxResult {
						ctx := vegetaBlockCtx.WithMultiStore(ms).WithEventManager(sdk.NewEventManager()).WithGasMeter(storetypes.NewInfiniteGasMeter())
						tx := block.Transactions[idx]
						if e := vegeta.executeTxIsolated(ctx, block, tx, cal.iterations(block.BlockNumber, tx.TxIndex, tx.TxHash)); e != nil {
							return &abci.ExecTxResult{Code: 1, Log: e.Error()}
						}
						return &abci.ExecTxResult{}
					})
					vegetaWall := uint64(time.Since(vegetaStart).Nanoseconds())
					var semanticErr *vegetaSemanticExecutionError
					semanticFallback := errors.As(vegetaErr, &semanticErr)
					if vegetaErr != nil && !semanticFallback {
						panic(vegetaErr)
					}

					serializationOrder := vegetaRunner.LastSerializationOrder()
					var derivedBranch *trackingMultiStore
					var vegetaSerialNanos uint64
					semanticReason := ""
					if semanticFallback {
						semanticReason = semanticErr.Error()
					} else {
						derivedBranch, vegetaSerialNanos, e = executeOrderedBlockBranch(vegeta, vegetaBlockCtx, block, cal, serializationOrder)
						if e != nil {
							// The scheduler completed, but its derived serialization is not
							// semantically executable on this fixed historical workload. Discard
							// the staged branch and require historical order to succeed below.
							semanticFallback = true
							semanticReason = fmt.Sprintf("derived-order safety replay failed: %v", e)
						} else if !trackingBranchesFinalStateEqual(vegetaBranch, derivedBranch) {
							panic(fmt.Sprintf("vegeta serialization mismatch sample=%d block=%d proposal=%v serialization=%v", sample, block.BlockNumber, vegetaRunner.LastProposalOrder(), serializationOrder))
						}
					}

					canonicalBranch := derivedBranch
					canonicalReplayNanos := vegetaSerialNanos
					if semanticFallback || !isHistoricalOrder(serializationOrder, len(block.Transactions)) {
						canonicalBranch, canonicalReplayNanos, e = executeHistoricalBlockBranch(vegeta, vegetaBlockCtx, block, cal)
						if e != nil {
							panic(fmt.Sprintf("vegeta canonical historical replay failed sample=%d block=%d derived_issue=%q: %v", sample, block.BlockNumber, semanticReason, e))
						}
					}
					canonicalMismatch := !semanticFallback && !trackingBranchesFinalStateEqual(vegetaBranch, canonicalBranch)
					canonicalFallback := semanticFallback || canonicalMismatch
					canonicalFallbackReason := ""
					var canonicalFallbackNanos uint64
					var historicalFallbackTransactions uint64
					vegetaStats := vegetaRunner.LastStats()
					strategyNanos := vegetaWall
					if canonicalFallback {
						reason := "post-block state differs from historical order"
						if semanticFallback {
							reason = semanticReason
						}
						canonicalFallbackReason = reason
						canonicalFallbackNanos = canonicalReplayNanos
						historicalFallbackTransactions = uint64(len(block.Transactions))
						fmt.Fprintf(os.Stderr, "Wasmd Vegeta canonical fallback: sample=%d block=%d txs=%d proposal=%v serialization=%v reason=%s\n", sample, block.BlockNumber, len(block.Transactions), vegetaRunner.LastProposalOrder(), serializationOrder, reason)
						canonicalBranch.Write()
						vegetaRunner.MarkCanonicalFallback(len(block.Transactions))
						strategyNanos += canonicalReplayNanos
						vegetaStats.Attempts += uint64(len(block.Transactions))
						// Historical-state gating is our fixed-trace adaptation, not part of
						// Vegeta Algorithm 3 replay. Keep it in StrategyTotalNanos and the
						// dedicated historical-fallback metric, but do not contaminate the
						// paper replay throughput or intrinsic re-execution counters.
						// The matched serial baseline must follow the state actually committed.
						// Normal Vegeta blocks use the derived serialization reference; canonical
						// fallback blocks commit historical order, so compare them to the measured
						// historical replay instead of retaining an unrelated derived-order timing.
						vegetaSerialNanos = canonicalReplayNanos
						cumulativeVegetaCanonicalFallbackBlocks++
					} else {
						vegetaBranch.Write()
					}
					if e := commitFinalizeState(vegeta.app); e != nil {
						panic(e)
					}
					vegetaEq := commitIDsEqual(serialCommitID, vegeta.app.LastCommitID())
					if !vegetaEq {
						panic(fmt.Sprintf("vegeta canonical state mismatch sample=%d block=%d fallback=%v proposal=%v serialization=%v", sample, block.BlockNumber, canonicalFallback, vegetaRunner.LastProposalOrder(), serializationOrder))
					}
					cumulativeVegetaNanos += strategyNanos
					cumulativeVegetaPreNanos += vegetaStats.PreConsensusNanos
					cumulativeVegetaPostNanos += vegetaStats.PostConsensusNanos
					cumulativeVegetaReferenceNanos += vegetaSerialNanos
					cumulativeVegetaReexecutions += vegetaStats.Reexecutions
					cumulativeVegetaSafetyReplays += vegetaStats.SafetyReplays
					cumulativeVegetaSnapshotBuildNanos += vegetaStats.SnapshotBuildNanos
					cumulativeVegetaSnapshotPointHits += vegetaStats.SnapshotPointHits
					cumulativeVegetaSnapshotPointMisses += vegetaStats.SnapshotPointMisses
					cumulativeVegetaPostBatches += vegetaStats.PostBatches
					cumulativeVegetaPostSingletonBatches += vegetaStats.PostSingletonBatches
					cumulativeVegetaReadySelectionNanos += vegetaStats.ReadySelectionNanos
					cumulativeVegetaAlg3ValidationNanos += vegetaStats.VegetaAlg3ValidationNanos
					cumulativeVegetaRangeValidationNanos += vegetaStats.VegetaRangeValidationNanos
					cumulativeVegetaIntrinsicReexecutionNanos += vegetaStats.VegetaIntrinsicReexecutionNanos
					cumulativeVegetaHistoricalFallbackNanos += canonicalFallbackNanos
					cumulativeVegetaPreExecWorkNanos += vegetaStats.PreExecWorkNanos
					cumulativeVegetaPreExecSpanNanos += vegetaStats.PreExecSpanNanos
					cumulativeVegetaPostExecWorkNanos += vegetaStats.PostExecWorkNanos
					cumulativeVegetaPostExecSpanNanos += vegetaStats.PostExecSpanNanos
					cumulativeVegetaPostWideExecWorkNanos += vegetaStats.PostWideExecWorkNanos
					cumulativeVegetaPostWideExecSpanNanos += vegetaStats.PostWideExecSpanNanos
					cumulativeVegetaPostWideTransactions += vegetaStats.PostWideTransactions
					cumulativeVegetaLongestChainSum += vegetaStats.VegetaLongestChain
					cumulativeVegetaTransactions += uint64(len(block.Transactions))
					if vegetaStats.PostMaxBatch > cumulativeVegetaPostMaxBatch {
						cumulativeVegetaPostMaxBatch = vegetaStats.PostMaxBatch
					}
					vegetaRec := Record{
						SchemaVersion: 1, Dataset: *datasetLabel, Sample: sample, BlockNumber: block.BlockNumber, Strategy: "cosmos-wasmd-vegeta", Workers: *workers,
						MatchedSerialNanos: vegetaSerialNanos, HistoricalSerialNanos: serialNanos, StrategyTotalNanos: strategyNanos, PreConsensusNanos: vegetaStats.PreConsensusNanos, PostConsensusNanos: vegetaStats.PostConsensusNanos, Transactions: len(block.Transactions),
						ExecutionAttempts: vegetaStats.Attempts, Reexecutions: vegetaStats.Reexecutions, SerialEquivalent: vegetaEq, SerialReferenceScope: "vegeta-derived-serialization+historical-state-gate", ComputeMetric: cal.Metric, ComputeScale: *scale,
						GoIterationsPerNano: *iterPerNs, CosmosSDKVersion: cosmosSDKVersion, WasmdVersion: wasmdVersion, IAVLCacheSize: benchmarkIAVLCacheSize, IAVLSyncPruning: benchmarkIAVLSyncPruning, EvaluatorSHA256: evaluatorSHA256, BaselineScope: vegetaScope, BlockSTMPreEstimate: false,
						SpeculatedTransactions: vegetaStats.Speculated, ReusedTransactions: vegetaStats.Reused, ValidationNanos: vegetaStats.ValidationNanos, ReplayExecutionNanos: vegetaStats.ReplayExecutionNanos,
						ConflictAnalysisNanos: vegetaStats.ConflictAnalysisNanos, DiscoveredConflicts: vegetaStats.DiscoveredConflicts, ForwardFallbacks: vegetaStats.ForwardFallbacks, SafetyReplays: vegetaStats.SafetyReplays,
						VegetaSnapshotBuildNanos: vegetaStats.SnapshotBuildNanos, VegetaSnapshotPointHits: vegetaStats.SnapshotPointHits, VegetaSnapshotPointMisses: vegetaStats.SnapshotPointMisses,
						VegetaSnapshotRangeHits: vegetaStats.SnapshotRangeHits, VegetaSnapshotRangeMisses: vegetaStats.SnapshotRangeMisses, VegetaPostBatches: vegetaStats.PostBatches,
						VegetaPostSingletonBatches: vegetaStats.PostSingletonBatches, VegetaPostMaxBatch: vegetaStats.PostMaxBatch, VegetaReadySelectionNanos: vegetaStats.ReadySelectionNanos,
						VegetaPreExecWorkNanos: vegetaStats.PreExecWorkNanos, VegetaPreExecSpanNanos: vegetaStats.PreExecSpanNanos, VegetaPostExecWorkNanos: vegetaStats.PostExecWorkNanos, VegetaPostExecSpanNanos: vegetaStats.PostExecSpanNanos,
						VegetaPostWideExecWorkNanos: vegetaStats.PostWideExecWorkNanos, VegetaPostWideExecSpanNanos: vegetaStats.PostWideExecSpanNanos, VegetaPostWideTransactions: vegetaStats.PostWideTransactions,
						VegetaLongestChain: vegetaStats.VegetaLongestChain, VegetaChainCount: vegetaStats.VegetaChainCount,
						VegetaAlg3ValidationNanos: vegetaStats.VegetaAlg3ValidationNanos, VegetaRangeValidationNanos: vegetaStats.VegetaRangeValidationNanos,
						VegetaIntrinsicReexecutionNanos: vegetaStats.VegetaIntrinsicReexecutionNanos, VegetaHistoricalFallbackNanos: canonicalFallbackNanos,
						VegetaHistoricalFallbackTransactions: historicalFallbackTransactions,
						VegetaCanonicalFallback:              canonicalFallback, VegetaCanonicalFallbackNanos: canonicalFallbackNanos, VegetaCanonicalFallbackReason: canonicalFallbackReason,
					}
					if vegetaRec.StrategyTotalNanos > 0 {
						vegetaRec.MatchedSerialSpeedup = float64(vegetaRec.MatchedSerialNanos) / float64(vegetaRec.StrategyTotalNanos)
					}
					if e := json.NewEncoder(w).Encode(&vegetaRec); e != nil {
						panic(e)
					}
				}

				completed := blockOffset + 1
				if planBlockCount >= 20 && (completed%10 == 0 || completed == planBlockCount) {
					if runBlockSTM && !runAllStrategies {
						reexec := cumulativeBlockSTMAttempts - cumulativeBlockSTMTx
						reexecPct := 0.0
						if cumulativeBlockSTMTx > 0 {
							reexecPct = 100 * float64(reexec) / float64(cumulativeBlockSTMTx)
						}
						serialLabel := "serial_ref"
						if useSerialOracle {
							serialLabel = "serial_oracle"
						}
						fmt.Fprintf(os.Stderr, "Wasmd campaign progress: sample=%d blocks=%d/%d source_block=%d elapsed=%s %s=%s blockstm=%s attempts=%d reexec=%d reexec_pct=%.2f%% live_serial_app=%v\n", sample, completed, planBlockCount, block.BlockNumber, time.Since(campaignStarted).Round(time.Second), serialLabel, time.Duration(cumulativeSerialNanos).Round(time.Millisecond), time.Duration(cumulativeBlockSTMNanos).Round(time.Millisecond), cumulativeBlockSTMAttempts, reexec, reexecPct, !useSerialOracle)
					} else if runVegeta && !runAllStrategies {
						serialLabel := "serial_ref"
						if useSerialOracle {
							serialLabel = "serial_oracle"
						}
						postX := 0.0
						historicalNetX := 0.0
						historicalPostX := 0.0
						if cumulativeVegetaNanos > 0 {
							historicalNetX = float64(cumulativeSerialNanos) / float64(cumulativeVegetaNanos)
						}
						if cumulativeVegetaPostNanos > 0 {
							postX = float64(cumulativeVegetaReferenceNanos) / float64(cumulativeVegetaPostNanos)
							historicalPostX = float64(cumulativeSerialNanos) / float64(cumulativeVegetaPostNanos)
						}
						snapshotLookups := cumulativeVegetaSnapshotPointHits + cumulativeVegetaSnapshotPointMisses
						snapshotHitPct := 0.0
						if snapshotLookups > 0 {
							snapshotHitPct = 100 * float64(cumulativeVegetaSnapshotPointHits) / float64(snapshotLookups)
						}
						singletonPct := 0.0
						if cumulativeVegetaPostBatches > 0 {
							singletonPct = 100 * float64(cumulativeVegetaPostSingletonBatches) / float64(cumulativeVegetaPostBatches)
						}
						preConcurrency := 0.0
						if cumulativeVegetaPreExecSpanNanos > 0 {
							preConcurrency = float64(cumulativeVegetaPreExecWorkNanos) / float64(cumulativeVegetaPreExecSpanNanos)
						}
						wideConcurrency := 0.0
						if cumulativeVegetaPostWideExecSpanNanos > 0 {
							wideConcurrency = float64(cumulativeVegetaPostWideExecWorkNanos) / float64(cumulativeVegetaPostWideExecSpanNanos)
						}
						chainRatio := 0.0
						if cumulativeVegetaLongestChainSum > 0 {
							chainRatio = float64(cumulativeVegetaTransactions) / float64(cumulativeVegetaLongestChainSum)
						}
						fmt.Fprintf(os.Stderr, "Wasmd campaign progress: sample=%d blocks=%d/%d source_block=%d elapsed=%s %s=%s vegeta=%s pre=%s post=%s work_x=%.2fx matched_post_x=%.2fx replay_x=%.2fx pre_cf=%.2fx wide_cf=%.2fx wide_txs=%d matched_ref=%s chain_ratio=%.2fx reexec=%d safety=%d canonical_fallback_blocks=%d alg3_val=%s range_val=%s intrinsic_reexec=%s historical_fallback=%s snapshot_build=%s snapshot_hit=%.1f%% ready=%s post_batches=%d singleton_batches=%.1f%% max_batch=%d live_vegeta_reference=false overlay=post-snapshot ready_state=incremental\n", sample, completed, planBlockCount, block.BlockNumber, time.Since(campaignStarted).Round(time.Second), serialLabel, time.Duration(cumulativeSerialNanos).Round(time.Millisecond), time.Duration(cumulativeVegetaNanos).Round(time.Millisecond), time.Duration(cumulativeVegetaPreNanos).Round(time.Millisecond), time.Duration(cumulativeVegetaPostNanos).Round(time.Millisecond), historicalNetX, postX, historicalPostX, preConcurrency, wideConcurrency, cumulativeVegetaPostWideTransactions, time.Duration(cumulativeVegetaReferenceNanos).Round(time.Millisecond), chainRatio, cumulativeVegetaReexecutions, cumulativeVegetaSafetyReplays, cumulativeVegetaCanonicalFallbackBlocks, time.Duration(cumulativeVegetaAlg3ValidationNanos).Round(time.Millisecond), time.Duration(cumulativeVegetaRangeValidationNanos).Round(time.Millisecond), time.Duration(cumulativeVegetaIntrinsicReexecutionNanos).Round(time.Millisecond), time.Duration(cumulativeVegetaHistoricalFallbackNanos).Round(time.Millisecond), time.Duration(cumulativeVegetaSnapshotBuildNanos).Round(time.Millisecond), snapshotHitPct, time.Duration(cumulativeVegetaReadySelectionNanos).Round(time.Millisecond), cumulativeVegetaPostBatches, singletonPct, cumulativeVegetaPostMaxBatch)
					} else {
						fmt.Fprintf(os.Stderr, "Wasmd campaign progress: sample=%d blocks=%d/%d source_block=%d elapsed=%s\n", sample, completed, planBlockCount, block.BlockNumber, time.Since(campaignStarted).Round(time.Second))
					}
				}
			}
			prepareStreamBlock := func(block ExecutionBlock) {
				if !*streamPlan {
					return
				}
				apps := make([]*benchApp, 0, 8)
				for _, app := range []*benchApp{serial, stm, aria, symb, vegeta, acgOracle} {
					if app != nil {
						apps = append(apps, app)
					}
				}
				for _, app := range apps {
					if err := app.prepareBlockCalls(block); err != nil {
						panic(err)
					}
				}
			}
			if *streamPlan {
				stream, err := openPlanStream(*planPath)
				if err != nil {
					panic(err)
				}
				defer stream.Close()
				blockOffset := 0
				for {
					if blockOffset >= planBlockCount {
						break
					}
					block, ok, err := stream.Next()
					if err != nil {
						panic(err)
					}
					if !ok {
						break
					}
					prepareStreamBlock(block)
					processBlock(blockOffset, block)
					blockOffset++
				}
				if blockOffset != planBlockCount {
					panic(fmt.Sprintf("streamed execution plan block count mismatch: manifest=%d observed=%d", planBlockCount, blockOffset))
				}
			} else {
				for blockOffset, block := range blocks {
					processBlock(blockOffset, block)
				}
			}

		}()
		// All sample apps are closed by the isolated sample scope above. Reclaim
		// their MemDB/IAVL heaps before the next sample so multi-sample paper runs
		// do not inherit stale heap pressure from previous samples.
		runtime.GC()
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
