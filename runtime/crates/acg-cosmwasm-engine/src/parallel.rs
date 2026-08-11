use crate::speculative::{
    CanonicalTransaction, CanonicalTxResult, SpeculativeExecutionMetrics, SpeculativeTxResult,
};
use crate::types::TransactionId;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

/// Bounded worker configuration for dependency-driven speculative execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParallelExecutionConfig {
    /// Maximum number of speculative transactions executing concurrently.
    pub workers: usize,
}

impl Default for ParallelExecutionConfig {
    fn default() -> Self {
        Self { workers: 1 }
    }
}

/// Pairwise dependency used by the dependency-driven speculative executor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SpeculativeDependencyClass {
    Soft,
    Hard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SpeculativeDependency {
    pub predecessor: TransactionId,
    pub successor: TransactionId,
    pub class: SpeculativeDependencyClass,
}

/// One scheduler level expressed in engine transaction identities.
///
/// Levels are retained for diagnostics and theoretical parallelism. The dependency-driven Brick-5
/// executor does not impose a global barrier between them; successors launch as soon as their
/// pairwise [`SpeculativeDependency`] predecessors complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeculativeWave {
    pub transaction_ids: Vec<TransactionId>,
}

impl SpeculativeWave {
    pub fn new(transaction_ids: Vec<TransactionId>) -> Self {
        Self { transaction_ids }
    }

    pub fn len(&self) -> usize {
        self.transaction_ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.transaction_ids.is_empty()
    }
}

/// Nested execution/runtime diagnostics captured by the block-local dependency executor.
///
/// These are aggregate worker times. Several fields are intentionally nested: for example host
/// storage callbacks execute inside Wasm entrypoint time, and MVCC lookup time executes inside host
/// storage callback time. The benchmark report prints them as a hierarchy rather than summing them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ContractExecutionDiagnostics {
    pub aggregate_request_execution: Duration,
    pub aggregate_receipt_finalization: Duration,
    pub aggregate_precontract_setup: Duration,
    pub aggregate_response_processing: Duration,
    pub aggregate_outcome_assembly: Duration,
    pub aggregate_backend_construction: Duration,
    pub aggregate_wasm_instance_acquire: Duration,
    pub aggregate_wasm_entrypoint: Duration,
    pub aggregate_wasm_recycle: Duration,
    pub aggregate_host_storage: Duration,
    pub aggregate_host_query: Duration,
    pub aggregate_transaction_lock_wait: Duration,
    pub aggregate_mvcc_storage_point: Duration,
    pub aggregate_mvcc_storage_range: Duration,
    pub aggregate_mvcc_balance_point: Duration,
    pub aggregate_mvcc_all_balances: Duration,
    pub aggregate_mvcc_contract: Duration,
    pub aggregate_mvcc_lock_wait: Duration,
    pub aggregate_mvcc_publish: Duration,
    pub wasm_instance_acquires: u64,
    pub wasm_entrypoint_calls: u64,
    pub wasm_instance_recycles: u64,
    pub host_storage_gets: u64,
    pub host_storage_scans: u64,
    pub host_storage_nexts: u64,
    pub host_storage_sets: u64,
    pub host_storage_removes: u64,
    pub host_queries: u64,
    pub mvcc_storage_point_reads: u64,
    pub mvcc_storage_point_hits: u64,
    pub mvcc_storage_base_fallbacks: u64,
    pub mvcc_storage_range_reads: u64,
    pub mvcc_balance_reads: u64,
    pub mvcc_all_balances_reads: u64,
    pub mvcc_contract_reads: u64,
    pub receipt_access_records: u64,
    pub receipt_read_dependencies: u64,
    pub receipt_storage_writes: u64,
    pub receipt_balance_writes: u64,
    pub receipt_created_contracts: u64,
    pub wasm_cache_pinned_hits: u64,
    pub wasm_cache_memory_hits: u64,
    pub wasm_cache_fs_hits: u64,
    pub wasm_cache_misses: u64,
}

impl ContractExecutionDiagnostics {
    pub fn merge(&mut self, other: &Self) {
        self.aggregate_request_execution += other.aggregate_request_execution;
        self.aggregate_receipt_finalization += other.aggregate_receipt_finalization;
        self.aggregate_precontract_setup += other.aggregate_precontract_setup;
        self.aggregate_response_processing += other.aggregate_response_processing;
        self.aggregate_outcome_assembly += other.aggregate_outcome_assembly;
        self.aggregate_backend_construction += other.aggregate_backend_construction;
        self.aggregate_wasm_instance_acquire += other.aggregate_wasm_instance_acquire;
        self.aggregate_wasm_entrypoint += other.aggregate_wasm_entrypoint;
        self.aggregate_wasm_recycle += other.aggregate_wasm_recycle;
        self.aggregate_host_storage += other.aggregate_host_storage;
        self.aggregate_host_query += other.aggregate_host_query;
        self.aggregate_transaction_lock_wait += other.aggregate_transaction_lock_wait;
        self.aggregate_mvcc_storage_point += other.aggregate_mvcc_storage_point;
        self.aggregate_mvcc_storage_range += other.aggregate_mvcc_storage_range;
        self.aggregate_mvcc_balance_point += other.aggregate_mvcc_balance_point;
        self.aggregate_mvcc_all_balances += other.aggregate_mvcc_all_balances;
        self.aggregate_mvcc_contract += other.aggregate_mvcc_contract;
        self.aggregate_mvcc_lock_wait += other.aggregate_mvcc_lock_wait;
        self.aggregate_mvcc_publish += other.aggregate_mvcc_publish;
        self.wasm_instance_acquires += other.wasm_instance_acquires;
        self.wasm_entrypoint_calls += other.wasm_entrypoint_calls;
        self.wasm_instance_recycles += other.wasm_instance_recycles;
        self.host_storage_gets += other.host_storage_gets;
        self.host_storage_scans += other.host_storage_scans;
        self.host_storage_nexts += other.host_storage_nexts;
        self.host_storage_sets += other.host_storage_sets;
        self.host_storage_removes += other.host_storage_removes;
        self.host_queries += other.host_queries;
        self.mvcc_storage_point_reads += other.mvcc_storage_point_reads;
        self.mvcc_storage_point_hits += other.mvcc_storage_point_hits;
        self.mvcc_storage_base_fallbacks += other.mvcc_storage_base_fallbacks;
        self.mvcc_storage_range_reads += other.mvcc_storage_range_reads;
        self.mvcc_balance_reads += other.mvcc_balance_reads;
        self.mvcc_all_balances_reads += other.mvcc_all_balances_reads;
        self.mvcc_contract_reads += other.mvcc_contract_reads;
        self.receipt_access_records += other.receipt_access_records;
        self.receipt_read_dependencies += other.receipt_read_dependencies;
        self.receipt_storage_writes += other.receipt_storage_writes;
        self.receipt_balance_writes += other.receipt_balance_writes;
        self.receipt_created_contracts += other.receipt_created_contracts;
        self.wasm_cache_pinned_hits += other.wasm_cache_pinned_hits;
        self.wasm_cache_memory_hits += other.wasm_cache_memory_hits;
        self.wasm_cache_fs_hits += other.wasm_cache_fs_hits;
        self.wasm_cache_misses += other.wasm_cache_misses;
    }
}

/// Per-transaction hot-path collector. Each speculative transaction gets its own collector so the
/// instrumentation itself does not introduce a shared atomic hotspot as worker count increases.
#[derive(Default)]
pub(crate) struct ExecutionHotPathDiagnostics {
    request_execution_ns: AtomicU64,
    receipt_finalization_ns: AtomicU64,
    precontract_setup_ns: AtomicU64,
    response_processing_ns: AtomicU64,
    outcome_assembly_ns: AtomicU64,
    backend_construction_ns: AtomicU64,
    wasm_instance_acquire_ns: AtomicU64,
    wasm_entrypoint_ns: AtomicU64,
    wasm_recycle_ns: AtomicU64,
    host_storage_ns: AtomicU64,
    host_query_ns: AtomicU64,
    transaction_lock_wait_ns: AtomicU64,
    mvcc_storage_point_ns: AtomicU64,
    mvcc_storage_range_ns: AtomicU64,
    mvcc_balance_point_ns: AtomicU64,
    mvcc_all_balances_ns: AtomicU64,
    mvcc_contract_ns: AtomicU64,
    mvcc_lock_wait_ns: AtomicU64,
    mvcc_publish_ns: AtomicU64,
    wasm_instance_acquires: AtomicU64,
    wasm_entrypoint_calls: AtomicU64,
    wasm_instance_recycles: AtomicU64,
    host_storage_gets: AtomicU64,
    host_storage_scans: AtomicU64,
    host_storage_nexts: AtomicU64,
    host_storage_sets: AtomicU64,
    host_storage_removes: AtomicU64,
    host_queries: AtomicU64,
    mvcc_storage_point_reads: AtomicU64,
    mvcc_storage_point_hits: AtomicU64,
    mvcc_storage_base_fallbacks: AtomicU64,
    mvcc_storage_range_reads: AtomicU64,
    mvcc_balance_reads: AtomicU64,
    mvcc_all_balances_reads: AtomicU64,
    mvcc_contract_reads: AtomicU64,
    receipt_access_records: AtomicU64,
    receipt_read_dependencies: AtomicU64,
    receipt_storage_writes: AtomicU64,
    receipt_balance_writes: AtomicU64,
    receipt_created_contracts: AtomicU64,
}

impl ExecutionHotPathDiagnostics {
    fn add_duration(target: &AtomicU64, duration: Duration) {
        let nanos = duration.as_nanos().min(u128::from(u64::MAX)) as u64;
        target.fetch_add(nanos, Ordering::Relaxed);
    }

    pub(crate) fn record_request_execution(&self, duration: Duration) {
        Self::add_duration(&self.request_execution_ns, duration);
    }

    pub(crate) fn record_receipt_finalization(
        &self,
        duration: Duration,
        access_records: usize,
        read_dependencies: usize,
        storage_writes: usize,
        balance_writes: usize,
        created_contracts: usize,
    ) {
        Self::add_duration(&self.receipt_finalization_ns, duration);
        self.receipt_access_records
            .fetch_add(access_records as u64, Ordering::Relaxed);
        self.receipt_read_dependencies
            .fetch_add(read_dependencies as u64, Ordering::Relaxed);
        self.receipt_storage_writes
            .fetch_add(storage_writes as u64, Ordering::Relaxed);
        self.receipt_balance_writes
            .fetch_add(balance_writes as u64, Ordering::Relaxed);
        self.receipt_created_contracts
            .fetch_add(created_contracts as u64, Ordering::Relaxed);
    }

    pub(crate) fn record_precontract_setup(&self, duration: Duration) {
        Self::add_duration(&self.precontract_setup_ns, duration);
    }

    pub(crate) fn record_response_processing(&self, duration: Duration) {
        Self::add_duration(&self.response_processing_ns, duration);
    }

    pub(crate) fn record_outcome_assembly(&self, duration: Duration) {
        Self::add_duration(&self.outcome_assembly_ns, duration);
    }

    pub(crate) fn record_backend_construction(&self, duration: Duration) {
        Self::add_duration(&self.backend_construction_ns, duration);
    }

    pub(crate) fn record_wasm_instance_acquire(&self, duration: Duration) {
        Self::add_duration(&self.wasm_instance_acquire_ns, duration);
        self.wasm_instance_acquires.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_wasm_entrypoint(&self, duration: Duration) {
        Self::add_duration(&self.wasm_entrypoint_ns, duration);
        self.wasm_entrypoint_calls.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_wasm_recycle(&self, duration: Duration) {
        Self::add_duration(&self.wasm_recycle_ns, duration);
        self.wasm_instance_recycles.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_host_storage_get(&self, duration: Duration, lock_wait: Duration) {
        Self::add_duration(&self.host_storage_ns, duration);
        Self::add_duration(&self.transaction_lock_wait_ns, lock_wait);
        self.host_storage_gets.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_host_storage_scan(&self, duration: Duration, lock_wait: Duration) {
        Self::add_duration(&self.host_storage_ns, duration);
        Self::add_duration(&self.transaction_lock_wait_ns, lock_wait);
        self.host_storage_scans.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_host_storage_next(&self, duration: Duration, lock_wait: Duration) {
        Self::add_duration(&self.host_storage_ns, duration);
        Self::add_duration(&self.transaction_lock_wait_ns, lock_wait);
        self.host_storage_nexts.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_host_storage_set(&self, duration: Duration, lock_wait: Duration) {
        Self::add_duration(&self.host_storage_ns, duration);
        Self::add_duration(&self.transaction_lock_wait_ns, lock_wait);
        self.host_storage_sets.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_host_storage_remove(&self, duration: Duration, lock_wait: Duration) {
        Self::add_duration(&self.host_storage_ns, duration);
        Self::add_duration(&self.transaction_lock_wait_ns, lock_wait);
        self.host_storage_removes.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_host_query(&self, duration: Duration, lock_wait: Duration) {
        Self::add_duration(&self.host_query_ns, duration);
        Self::add_duration(&self.transaction_lock_wait_ns, lock_wait);
        self.host_queries.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_mvcc_storage_point(
        &self,
        duration: Duration,
        version_hit: bool,
        base_fallback: bool,
    ) {
        Self::add_duration(&self.mvcc_storage_point_ns, duration);
        self.mvcc_storage_point_reads
            .fetch_add(1, Ordering::Relaxed);
        if version_hit {
            self.mvcc_storage_point_hits.fetch_add(1, Ordering::Relaxed);
        }
        if base_fallback {
            self.mvcc_storage_base_fallbacks
                .fetch_add(1, Ordering::Relaxed);
        }
    }

    pub(crate) fn record_mvcc_storage_range(&self, duration: Duration) {
        Self::add_duration(&self.mvcc_storage_range_ns, duration);
        self.mvcc_storage_range_reads
            .fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_mvcc_balance(&self, duration: Duration) {
        Self::add_duration(&self.mvcc_balance_point_ns, duration);
        self.mvcc_balance_reads.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_mvcc_all_balances(&self, duration: Duration) {
        Self::add_duration(&self.mvcc_all_balances_ns, duration);
        self.mvcc_all_balances_reads.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_mvcc_contract(&self, duration: Duration) {
        Self::add_duration(&self.mvcc_contract_ns, duration);
        self.mvcc_contract_reads.fetch_add(1, Ordering::Relaxed);
    }

    pub(crate) fn record_mvcc_lock_wait(&self, duration: Duration) {
        Self::add_duration(&self.mvcc_lock_wait_ns, duration);
    }

    pub(crate) fn record_mvcc_publish(&self, duration: Duration) {
        Self::add_duration(&self.mvcc_publish_ns, duration);
    }

    pub(crate) fn snapshot(&self) -> ContractExecutionDiagnostics {
        let duration = |value: &AtomicU64| Duration::from_nanos(value.load(Ordering::Relaxed));
        let count = |value: &AtomicU64| value.load(Ordering::Relaxed);
        ContractExecutionDiagnostics {
            aggregate_request_execution: duration(&self.request_execution_ns),
            aggregate_receipt_finalization: duration(&self.receipt_finalization_ns),
            aggregate_precontract_setup: duration(&self.precontract_setup_ns),
            aggregate_response_processing: duration(&self.response_processing_ns),
            aggregate_outcome_assembly: duration(&self.outcome_assembly_ns),
            aggregate_backend_construction: duration(&self.backend_construction_ns),
            aggregate_wasm_instance_acquire: duration(&self.wasm_instance_acquire_ns),
            aggregate_wasm_entrypoint: duration(&self.wasm_entrypoint_ns),
            aggregate_wasm_recycle: duration(&self.wasm_recycle_ns),
            aggregate_host_storage: duration(&self.host_storage_ns),
            aggregate_host_query: duration(&self.host_query_ns),
            aggregate_transaction_lock_wait: duration(&self.transaction_lock_wait_ns),
            aggregate_mvcc_storage_point: duration(&self.mvcc_storage_point_ns),
            aggregate_mvcc_storage_range: duration(&self.mvcc_storage_range_ns),
            aggregate_mvcc_balance_point: duration(&self.mvcc_balance_point_ns),
            aggregate_mvcc_all_balances: duration(&self.mvcc_all_balances_ns),
            aggregate_mvcc_contract: duration(&self.mvcc_contract_ns),
            aggregate_mvcc_lock_wait: duration(&self.mvcc_lock_wait_ns),
            aggregate_mvcc_publish: duration(&self.mvcc_publish_ns),
            wasm_instance_acquires: count(&self.wasm_instance_acquires),
            wasm_entrypoint_calls: count(&self.wasm_entrypoint_calls),
            wasm_instance_recycles: count(&self.wasm_instance_recycles),
            host_storage_gets: count(&self.host_storage_gets),
            host_storage_scans: count(&self.host_storage_scans),
            host_storage_nexts: count(&self.host_storage_nexts),
            host_storage_sets: count(&self.host_storage_sets),
            host_storage_removes: count(&self.host_storage_removes),
            host_queries: count(&self.host_queries),
            mvcc_storage_point_reads: count(&self.mvcc_storage_point_reads),
            mvcc_storage_point_hits: count(&self.mvcc_storage_point_hits),
            mvcc_storage_base_fallbacks: count(&self.mvcc_storage_base_fallbacks),
            mvcc_storage_range_reads: count(&self.mvcc_storage_range_reads),
            mvcc_balance_reads: count(&self.mvcc_balance_reads),
            mvcc_all_balances_reads: count(&self.mvcc_all_balances_reads),
            mvcc_contract_reads: count(&self.mvcc_contract_reads),
            receipt_access_records: count(&self.receipt_access_records),
            receipt_read_dependencies: count(&self.receipt_read_dependencies),
            receipt_storage_writes: count(&self.receipt_storage_writes),
            receipt_balance_writes: count(&self.receipt_balance_writes),
            receipt_created_contracts: count(&self.receipt_created_contracts),
            ..ContractExecutionDiagnostics::default()
        }
    }
}

/// Detailed diagnostics for the block-local MVCC dependency executor.
///
/// Worker-stage fields are aggregate worker time and may exceed wall clock because workers overlap.
/// The MVCC path never deep-copies the block base per transaction and never replays historical
/// write sets into transaction-local snapshots. Instead, each launch captures a compact visibility
/// mask and reads predecessor versions lazily from the shared block-local version index.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DependencyPreexecutionDiagnostics {
    pub executor_total: Duration,
    pub dependency_plan_setup: Duration,
    pub worker_phase_wall: Duration,
    pub aggregate_ready_wait: Duration,
    pub aggregate_visibility_capture: Duration,
    pub aggregate_contract_execution: Duration,
    pub aggregate_publish_and_unblock: Duration,
    pub visibility_masks_captured: u64,
    pub visibility_words_copied: u64,
    pub published_storage_versions: u64,
    pub published_balance_versions: u64,
    pub published_contract_versions: u64,
    pub max_in_flight: usize,
    pub contract: ContractExecutionDiagnostics,
}

impl DependencyPreexecutionDiagnostics {
    pub fn merge(&mut self, other: &Self) {
        self.executor_total += other.executor_total;
        self.dependency_plan_setup += other.dependency_plan_setup;
        self.worker_phase_wall += other.worker_phase_wall;
        self.aggregate_ready_wait += other.aggregate_ready_wait;
        self.aggregate_visibility_capture += other.aggregate_visibility_capture;
        self.aggregate_contract_execution += other.aggregate_contract_execution;
        self.aggregate_publish_and_unblock += other.aggregate_publish_and_unblock;
        self.visibility_masks_captured += other.visibility_masks_captured;
        self.visibility_words_copied += other.visibility_words_copied;
        self.published_storage_versions += other.published_storage_versions;
        self.published_balance_versions += other.published_balance_versions;
        self.published_contract_versions += other.published_contract_versions;
        self.max_in_flight = self.max_in_flight.max(other.max_in_flight);
        self.contract.merge(&other.contract);
    }

    pub fn aggregate_worker_stage_time(&self) -> Duration {
        self.aggregate_ready_wait
            + self.aggregate_visibility_capture
            + self.aggregate_contract_execution
            + self.aggregate_publish_and_unblock
    }
}

/// Scheduling/concurrency measurements for dependency-driven speculative execution.
/// `wave_widths` describe diagnostic scheduler levels, while `dependency_count` describes the
/// actual ready-DAG constraints. Levels are not execution barriers.
#[derive(Clone, Debug, PartialEq)]
pub struct ParallelSpeculativeExecutionMetrics {
    pub workers: usize,
    pub wave_widths: Vec<usize>,
    pub speculative: SpeculativeExecutionMetrics,
    pub dependency_count: usize,
    pub hard_dependency_count: usize,
    pub dependency_diagnostics: DependencyPreexecutionDiagnostics,
}

impl ParallelSpeculativeExecutionMetrics {
    pub fn wave_count(&self) -> usize {
        self.wave_widths.len()
    }

    pub fn max_wave_width(&self) -> usize {
        self.wave_widths.iter().copied().max().unwrap_or(0)
    }

    pub fn exposed_parallelism(&self) -> f64 {
        let transactions = self.speculative.speculative_results as usize;
        if transactions == 0 || self.wave_widths.is_empty() {
            0.0
        } else {
            transactions as f64 / self.wave_widths.len() as f64
        }
    }
}

/// Detached pre-consensus result for one predicted block.
///
/// Receipts are produced from one committed predecessor snapshot plus block-local MVCC versions.
/// No projected successor state is retained: block N+1 always waits for canonical commit of N and
/// snapshots the actual post-N state.
pub struct PreparedSpeculativeBlock {
    pub predicted_transactions: Vec<CanonicalTransaction>,
    pub receipts: Vec<SpeculativeTxResult>,
    pub metrics: ParallelSpeculativeExecutionMetrics,
}

impl PreparedSpeculativeBlock {
    pub fn predicted_transaction_count(&self) -> usize {
        self.predicted_transactions.len()
    }
}

/// How well a pre-consensus predicted block matched the post-consensus decided block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PredictionMatchMetrics {
    pub predicted_transactions: u64,
    pub decided_transactions: u64,
    pub matched_transactions: u64,
    pub discarded_predictions: u64,
    pub missing_predictions: u64,
}

impl PredictionMatchMetrics {
    pub fn precision(&self) -> f64 {
        ratio(self.matched_transactions, self.predicted_transactions)
    }

    pub fn coverage(&self) -> f64 {
        ratio(self.matched_transactions, self.decided_transactions)
    }
}

/// Detailed wall-clock breakdown of the post-consensus critical path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PostConsensusTimings {
    pub receipt_matching: std::time::Duration,
    pub validation: std::time::Duration,
    pub replay_or_missing_execution: std::time::Duration,
    pub commit_reused: std::time::Duration,
    pub total: std::time::Duration,
}

/// One replay attribution emitted by canonical reconciliation.
///
/// `conflict_index` indexes the invalid receipt's [`crate::ValidationOutcome::conflicts`] slice.
/// The validator/runtime-feedback layer maps that concrete conflict to conflict-kind metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconciliationDependencyEvidence {
    pub predecessor_index: usize,
    pub transaction_index: usize,
    pub conflict_index: usize,
}

/// Post-consensus reconciliation result for a block that may have pre-consensus receipts.
#[derive(Debug)]
pub struct SplitPhaseSpeculativeBlockOutcome {
    pub transactions: Vec<CanonicalTxResult>,
    pub speculative: SpeculativeExecutionMetrics,
    pub prediction: PredictionMatchMetrics,
    pub timings: PostConsensusTimings,
    pub dependency_evidence: Vec<ReconciliationDependencyEvidence>,
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{
        ContractExecutionDiagnostics, DependencyPreexecutionDiagnostics,
        ParallelSpeculativeExecutionMetrics, SpeculativeWave,
    };
    use crate::speculative::SpeculativeExecutionMetrics;
    use crate::types::TransactionId;

    #[test]
    fn speculative_wave_reports_width() {
        let wave = SpeculativeWave::new(vec![TransactionId(1), TransactionId(2)]);
        assert_eq!(wave.len(), 2);
        assert!(!wave.is_empty());
        assert!(SpeculativeWave::new(Vec::new()).is_empty());
    }

    #[test]
    fn dependency_metrics_report_diagnostic_levels_without_wave_barrier_math() {
        let metrics = ParallelSpeculativeExecutionMetrics {
            workers: 4,
            wave_widths: vec![5, 3],
            speculative: SpeculativeExecutionMetrics {
                speculative_results: 8,
                ..SpeculativeExecutionMetrics::default()
            },
            dependency_count: 6,
            hard_dependency_count: 4,
            dependency_diagnostics: DependencyPreexecutionDiagnostics::default(),
        };

        assert_eq!(metrics.wave_count(), 2);
        assert_eq!(metrics.max_wave_width(), 5);
        assert!((metrics.exposed_parallelism() - 4.0).abs() < f64::EPSILON);
        assert_eq!(metrics.dependency_count, 6);
        assert_eq!(metrics.hard_dependency_count, 4);
    }

    #[test]
    fn mvcc_diagnostics_merge_additive_work_and_max_concurrency() {
        let mut left = DependencyPreexecutionDiagnostics {
            aggregate_visibility_capture: Duration::from_millis(2),
            visibility_masks_captured: 3,
            visibility_words_copied: 6,
            published_storage_versions: 7,
            max_in_flight: 2,
            contract: ContractExecutionDiagnostics {
                aggregate_wasm_entrypoint: Duration::from_millis(3),
                wasm_entrypoint_calls: 5,
                host_storage_gets: 11,
                ..ContractExecutionDiagnostics::default()
            },
            ..DependencyPreexecutionDiagnostics::default()
        };
        let right = DependencyPreexecutionDiagnostics {
            aggregate_visibility_capture: Duration::from_millis(5),
            visibility_masks_captured: 4,
            visibility_words_copied: 8,
            published_storage_versions: 11,
            max_in_flight: 6,
            contract: ContractExecutionDiagnostics {
                aggregate_wasm_entrypoint: Duration::from_millis(7),
                wasm_entrypoint_calls: 13,
                host_storage_gets: 17,
                ..ContractExecutionDiagnostics::default()
            },
            ..DependencyPreexecutionDiagnostics::default()
        };

        left.merge(&right);

        assert_eq!(left.aggregate_visibility_capture, Duration::from_millis(7));
        assert_eq!(left.visibility_masks_captured, 7);
        assert_eq!(left.visibility_words_copied, 14);
        assert_eq!(left.published_storage_versions, 18);
        assert_eq!(left.max_in_flight, 6);
        assert_eq!(
            left.contract.aggregate_wasm_entrypoint,
            Duration::from_millis(10)
        );
        assert_eq!(left.contract.wasm_entrypoint_calls, 18);
        assert_eq!(left.contract.host_storage_gets, 28);
    }
}
