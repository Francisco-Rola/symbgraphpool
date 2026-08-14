//! Stable Brick 5E experiment records plus Brick 5F acceptance gates shared by all workloads.
//!
//! Wall-clock values are validator-local measurement only. Nothing in this crate is consensus
//! visible or participates in canonical validation/replay decisions.

pub mod acceptance;
mod metadata;

pub use acceptance::{
    AcceptanceError, AcceptanceIssue, AcceptanceIssueCategory, AcceptancePolicy,
    DerivedAcceptanceMetrics, ExperimentAcceptanceReport, ExperimentAcceptanceStatus,
    ExperimentManifest, PerformanceAcceptancePolicy, RunAcceptanceReport, RunAcceptanceStatus,
    RunIdentity, ACCEPTANCE_REPORT_SCHEMA_VERSION, EXPERIMENT_MANIFEST_SCHEMA_VERSION,
};
pub use metadata::sha256_hex;

use std::{collections::BTreeMap, fs::OpenOptions, io::Write, path::Path, time::Duration};

use acg_candidate_graph::EdgeClass;
use acg_cosmwasm_engine::{ContractExecutionDiagnostics, ParallelSpeculativeExecutionMetrics};
use acg_feedback::ApplySummary;
use acg_runtime_feedback::{AdaptiveBlockPlan, AdaptivePlanningConfig, AdaptivePlanningMetrics};
use acg_validator_sim::{BlockExecutionReport, SplitPhaseSpeculativeExecutionReport};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const EXPERIMENT_RECORD_SCHEMA_VERSION: u16 = 3;
const PREVIOUS_EXPERIMENT_RECORD_SCHEMA_VERSION: u16 = 2;
const LEGACY_EXPERIMENT_RECORD_SCHEMA_VERSION: u16 = 1;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExperimentMetadata {
    pub experiment_id: String,
    pub workload: String,
    pub mode: String,
    pub run_index: u32,
    pub seed: u64,
    pub workers: u32,
    pub physical_cores: u32,
    pub started_at_utc: Option<String>,
    pub git_revision: Option<String>,
    pub build_profile: Option<String>,
    pub rustc_version: Option<String>,
    /// Stable host/build metadata such as CPU model, OS, kernel, and logical-core count.
    #[serde(default)]
    pub environment: BTreeMap<String, String>,
    /// Workload-specific configuration, kept separate from host metadata.
    #[serde(default)]
    pub parameters: BTreeMap<String, String>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PlanningRecord {
    pub adapter_nanos: u64,
    pub candidate_graph_nanos: u64,
    pub scheduler_nanos: u64,
    pub schedule_validation_nanos: u64,
    pub plan_conversion_nanos: u64,
    pub total_nanos: u64,
}

impl From<AdaptivePlanningMetrics> for PlanningRecord {
    fn from(metrics: AdaptivePlanningMetrics) -> Self {
        Self {
            adapter_nanos: nanos(metrics.adapter),
            candidate_graph_nanos: nanos(metrics.candidate_graph),
            scheduler_nanos: nanos(metrics.scheduler),
            schedule_validation_nanos: nanos(metrics.schedule_validation),
            plan_conversion_nanos: nanos(metrics.plan_conversion),
            total_nanos: nanos(metrics.total()),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct SchedulingRecord {
    pub candidate_edges: u64,
    pub low_edges: u64,
    pub soft_edges: u64,
    pub hard_edges: u64,
    /// Dependencies implied by classification/wave placement before hard-edge transitive reduction.
    #[serde(default)]
    pub pre_reduction_dependencies: u64,
    /// Final execution dependency count after exact hard-edge transitive reduction.
    #[serde(default)]
    pub scheduled_dependencies: u64,
    /// Hard edges removed because another hard path already preserved the same reachability.
    #[serde(default)]
    pub edges_elided_by_reduction: u64,
    /// Legacy schema-v1 name for the final scheduled dependency count.
    pub ordering_dependencies: u64,
    pub soft_dependencies: u64,
    pub hard_dependencies: u64,
    pub wave_count: u64,
    pub max_wave_width: u64,
    /// Sum of edge raw probabilities in Q16 units. Divide by `candidate_edges * 65535` for mean.
    pub probability_q16_sum: u64,
    /// Sum of cost-adjusted scheduling risks in Q16 units.
    pub scheduling_risk_q16_sum: u64,
    pub replay_cost_evidence_edges: u64,
    pub serialization_cost_evidence_edges: u64,
    pub expected_replay_cost_nanos_sum: u64,
    pub expected_serialization_cost_nanos_sum: u64,
}

impl SchedulingRecord {
    fn from_plan(plan: &AdaptiveBlockPlan, config: &AdaptivePlanningConfig) -> Self {
        let scheduled_dependencies =
            u64::try_from(plan.schedule.ordering_dependencies.len()).unwrap_or(u64::MAX);
        let mut record = Self {
            candidate_edges: u64::try_from(plan.candidate_graph.edges().len()).unwrap_or(u64::MAX),
            pre_reduction_dependencies: u64::try_from(
                plan.schedule.pre_reduction_ordering_dependencies,
            )
            .unwrap_or(u64::MAX),
            scheduled_dependencies,
            edges_elided_by_reduction: u64::try_from(
                plan.schedule.hard_dependencies_elided_by_reduction,
            )
            .unwrap_or(u64::MAX),
            ordering_dependencies: scheduled_dependencies,
            wave_count: u64::try_from(plan.schedule.waves.len()).unwrap_or(u64::MAX),
            max_wave_width: u64::try_from(plan.max_wave_width()).unwrap_or(u64::MAX),
            ..Self::default()
        };

        for edge in plan.candidate_graph.edges() {
            match config.scheduler.classify(edge) {
                EdgeClass::Low => record.low_edges = record.low_edges.saturating_add(1),
                EdgeClass::Soft => record.soft_edges = record.soft_edges.saturating_add(1),
                EdgeClass::Hard => record.hard_edges = record.hard_edges.saturating_add(1),
            }
            record.probability_q16_sum = record
                .probability_q16_sum
                .saturating_add(u64::from(edge.probability_q16));
            record.scheduling_risk_q16_sum = record
                .scheduling_risk_q16_sum
                .saturating_add(u64::from(edge.scheduling_risk_q16));
            if edge.replay_cost_confidence_q16 != 0 {
                record.replay_cost_evidence_edges =
                    record.replay_cost_evidence_edges.saturating_add(1);
            }
            if edge.serialization_cost_confidence_q16 != 0 {
                record.serialization_cost_evidence_edges =
                    record.serialization_cost_evidence_edges.saturating_add(1);
            }
            record.expected_replay_cost_nanos_sum = record
                .expected_replay_cost_nanos_sum
                .saturating_add(edge.expected_replay_cost_nanos);
            record.expected_serialization_cost_nanos_sum = record
                .expected_serialization_cost_nanos_sum
                .saturating_add(edge.expected_serialization_cost_nanos);
        }

        for dependency in &plan.schedule.ordering_dependencies {
            match dependency.class {
                EdgeClass::Hard => {
                    record.hard_dependencies = record.hard_dependencies.saturating_add(1)
                }
                EdgeClass::Soft => {
                    record.soft_dependencies = record.soft_dependencies.saturating_add(1)
                }
                EdgeClass::Low => {}
            }
        }
        record
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContractExecutionRecord {
    pub aggregate_request_execution_nanos: u64,
    pub aggregate_receipt_finalization_nanos: u64,
    pub aggregate_precontract_setup_nanos: u64,
    pub aggregate_response_processing_nanos: u64,
    pub aggregate_outcome_assembly_nanos: u64,
    pub aggregate_backend_construction_nanos: u64,
    pub aggregate_wasm_instance_acquire_nanos: u64,
    pub aggregate_wasm_entrypoint_nanos: u64,
    pub aggregate_wasm_recycle_nanos: u64,
    pub aggregate_host_storage_nanos: u64,
    pub aggregate_host_query_nanos: u64,
    pub aggregate_transaction_lock_wait_nanos: u64,
    pub aggregate_mvcc_storage_point_nanos: u64,
    pub aggregate_mvcc_storage_range_nanos: u64,
    pub aggregate_mvcc_balance_point_nanos: u64,
    pub aggregate_mvcc_all_balances_nanos: u64,
    pub aggregate_mvcc_contract_nanos: u64,
    pub aggregate_mvcc_lock_wait_nanos: u64,
    pub aggregate_mvcc_publish_nanos: u64,
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

impl From<&ContractExecutionDiagnostics> for ContractExecutionRecord {
    fn from(diagnostics: &ContractExecutionDiagnostics) -> Self {
        Self {
            aggregate_request_execution_nanos: nanos(diagnostics.aggregate_request_execution),
            aggregate_receipt_finalization_nanos: nanos(diagnostics.aggregate_receipt_finalization),
            aggregate_precontract_setup_nanos: nanos(diagnostics.aggregate_precontract_setup),
            aggregate_response_processing_nanos: nanos(diagnostics.aggregate_response_processing),
            aggregate_outcome_assembly_nanos: nanos(diagnostics.aggregate_outcome_assembly),
            aggregate_backend_construction_nanos: nanos(diagnostics.aggregate_backend_construction),
            aggregate_wasm_instance_acquire_nanos: nanos(
                diagnostics.aggregate_wasm_instance_acquire,
            ),
            aggregate_wasm_entrypoint_nanos: nanos(diagnostics.aggregate_wasm_entrypoint),
            aggregate_wasm_recycle_nanos: nanos(diagnostics.aggregate_wasm_recycle),
            aggregate_host_storage_nanos: nanos(diagnostics.aggregate_host_storage),
            aggregate_host_query_nanos: nanos(diagnostics.aggregate_host_query),
            aggregate_transaction_lock_wait_nanos: nanos(
                diagnostics.aggregate_transaction_lock_wait,
            ),
            aggregate_mvcc_storage_point_nanos: nanos(diagnostics.aggregate_mvcc_storage_point),
            aggregate_mvcc_storage_range_nanos: nanos(diagnostics.aggregate_mvcc_storage_range),
            aggregate_mvcc_balance_point_nanos: nanos(diagnostics.aggregate_mvcc_balance_point),
            aggregate_mvcc_all_balances_nanos: nanos(diagnostics.aggregate_mvcc_all_balances),
            aggregate_mvcc_contract_nanos: nanos(diagnostics.aggregate_mvcc_contract),
            aggregate_mvcc_lock_wait_nanos: nanos(diagnostics.aggregate_mvcc_lock_wait),
            aggregate_mvcc_publish_nanos: nanos(diagnostics.aggregate_mvcc_publish),
            wasm_instance_acquires: diagnostics.wasm_instance_acquires,
            wasm_entrypoint_calls: diagnostics.wasm_entrypoint_calls,
            wasm_instance_recycles: diagnostics.wasm_instance_recycles,
            host_storage_gets: diagnostics.host_storage_gets,
            host_storage_scans: diagnostics.host_storage_scans,
            host_storage_nexts: diagnostics.host_storage_nexts,
            host_storage_sets: diagnostics.host_storage_sets,
            host_storage_removes: diagnostics.host_storage_removes,
            host_queries: diagnostics.host_queries,
            mvcc_storage_point_reads: diagnostics.mvcc_storage_point_reads,
            mvcc_storage_point_hits: diagnostics.mvcc_storage_point_hits,
            mvcc_storage_base_fallbacks: diagnostics.mvcc_storage_base_fallbacks,
            mvcc_storage_range_reads: diagnostics.mvcc_storage_range_reads,
            mvcc_balance_reads: diagnostics.mvcc_balance_reads,
            mvcc_all_balances_reads: diagnostics.mvcc_all_balances_reads,
            mvcc_contract_reads: diagnostics.mvcc_contract_reads,
            receipt_access_records: diagnostics.receipt_access_records,
            receipt_read_dependencies: diagnostics.receipt_read_dependencies,
            receipt_storage_writes: diagnostics.receipt_storage_writes,
            receipt_balance_writes: diagnostics.receipt_balance_writes,
            receipt_created_contracts: diagnostics.receipt_created_contracts,
            wasm_cache_pinned_hits: diagnostics.wasm_cache_pinned_hits,
            wasm_cache_memory_hits: diagnostics.wasm_cache_memory_hits,
            wasm_cache_fs_hits: diagnostics.wasm_cache_fs_hits,
            wasm_cache_misses: diagnostics.wasm_cache_misses,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub transactions: u64,
    pub workers: u64,
    pub dependency_count: u64,
    pub hard_dependency_count: u64,
    pub preexecution_executor_total_nanos: u64,
    pub dependency_plan_setup_nanos: u64,
    pub preexecution_worker_wall_nanos: u64,
    pub aggregate_ready_wait_nanos: u64,
    pub aggregate_visibility_capture_nanos: u64,
    pub aggregate_contract_execution_nanos: u64,
    pub aggregate_publish_and_unblock_nanos: u64,
    pub visibility_masks_captured: u64,
    pub visibility_words_copied: u64,
    pub published_storage_versions: u64,
    pub published_balance_versions: u64,
    pub published_contract_versions: u64,
    pub max_in_flight: u64,
    pub speculative_results: u64,
    pub reused_results: u64,
    pub invalidated_results: u64,
    pub replayed_transactions: u64,
    pub canonical_transactions: u64,
    pub post_consensus_total_nanos: u64,
    pub receipt_matching_nanos: u64,
    pub validation_nanos: u64,
    pub replay_or_missing_execution_nanos: u64,
    pub commit_reused_nanos: u64,
    pub predicted_transactions: u64,
    pub decided_transactions: u64,
    pub matched_transactions: u64,
    pub discarded_predictions: u64,
    pub missing_predictions: u64,
    pub contract: ContractExecutionRecord,
}

impl ExecutionRecord {
    fn from_reports(
        preexecution: &ParallelSpeculativeExecutionMetrics,
        reconciliation: &SplitPhaseSpeculativeExecutionReport,
    ) -> Self {
        let diagnostics = &preexecution.dependency_diagnostics;
        Self {
            transactions: preexecution.speculative.speculative_results,
            workers: u64::try_from(preexecution.workers).unwrap_or(u64::MAX),
            dependency_count: u64::try_from(preexecution.dependency_count).unwrap_or(u64::MAX),
            hard_dependency_count: u64::try_from(preexecution.hard_dependency_count)
                .unwrap_or(u64::MAX),
            preexecution_executor_total_nanos: nanos(diagnostics.executor_total),
            dependency_plan_setup_nanos: nanos(diagnostics.dependency_plan_setup),
            preexecution_worker_wall_nanos: nanos(diagnostics.worker_phase_wall),
            aggregate_ready_wait_nanos: nanos(diagnostics.aggregate_ready_wait),
            aggregate_visibility_capture_nanos: nanos(diagnostics.aggregate_visibility_capture),
            aggregate_contract_execution_nanos: nanos(diagnostics.aggregate_contract_execution),
            aggregate_publish_and_unblock_nanos: nanos(diagnostics.aggregate_publish_and_unblock),
            visibility_masks_captured: diagnostics.visibility_masks_captured,
            visibility_words_copied: diagnostics.visibility_words_copied,
            published_storage_versions: diagnostics.published_storage_versions,
            published_balance_versions: diagnostics.published_balance_versions,
            published_contract_versions: diagnostics.published_contract_versions,
            max_in_flight: u64::try_from(diagnostics.max_in_flight).unwrap_or(u64::MAX),
            speculative_results: reconciliation.speculative.speculative_results,
            reused_results: reconciliation.speculative.reused_results,
            invalidated_results: reconciliation.speculative.invalidated_results,
            replayed_transactions: reconciliation.speculative.replayed_transactions,
            canonical_transactions: reconciliation.speculative.canonical_transactions,
            post_consensus_total_nanos: nanos(reconciliation.timings.total),
            receipt_matching_nanos: nanos(reconciliation.timings.receipt_matching),
            validation_nanos: nanos(reconciliation.timings.validation),
            replay_or_missing_execution_nanos: nanos(
                reconciliation.timings.replay_or_missing_execution,
            ),
            commit_reused_nanos: nanos(reconciliation.timings.commit_reused),
            predicted_transactions: reconciliation.prediction.predicted_transactions,
            decided_transactions: reconciliation.prediction.decided_transactions,
            matched_transactions: reconciliation.prediction.matched_transactions,
            discarded_predictions: reconciliation.prediction.discarded_predictions,
            missing_predictions: reconciliation.prediction.missing_predictions,
            contract: (&diagnostics.contract).into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParallelismReference {
    pub serial_equivalent_work_nanos: Option<u64>,
    pub serial_cost_dag_bound_nanos: Option<u64>,
}

impl ParallelismReference {
    pub fn from_serial_report(plan: &AdaptiveBlockPlan, report: &BlockExecutionReport) -> Self {
        let services = service_nanos_by_transaction(plan, report);
        let serial_equivalent_work_nanos = services
            .as_ref()
            .map(|values| values.iter().copied().fold(0_u64, u64::saturating_add));
        let serial_cost_dag_bound_nanos = services
            .as_ref()
            .map(|values| dag_bound_from_services(plan, values));
        Self {
            serial_equivalent_work_nanos,
            serial_cost_dag_bound_nanos,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ParallelismRecord {
    pub serial_equivalent_work_nanos: Option<u64>,
    pub serial_cost_dag_bound_nanos: Option<u64>,
    pub observed_service_dag_bound_nanos: Option<u64>,
    /// Sum of observed speculative transaction service times.
    #[serde(default)]
    pub observed_service_work_nanos: Option<u64>,
    /// Work-conservation lower bound: observed service work divided by available workers.
    #[serde(default)]
    pub worker_capacity_bound_nanos: Option<u64>,
    /// Feasible lower bound = max(observed service DAG critical path, worker-capacity bound).
    #[serde(default)]
    pub parallel_lower_bound_nanos: Option<u64>,
    pub actual_execution_wall_nanos: u64,
    /// `observed_service_dag / serial_cost_dag * 1000`.
    pub service_inflation_milli: Option<u64>,
    /// Legacy schema-v1 ratio: `actual_execution_wall / observed_service_dag * 1000`.
    pub scheduler_realization_milli: Option<u64>,
    /// Corrected ratio: `actual_execution_wall / parallel_lower_bound * 1000`.
    #[serde(default)]
    pub scheduler_realization_corrected_milli: Option<u64>,
}

impl ParallelismRecord {
    fn from_reports(
        plan: &AdaptiveBlockPlan,
        preexecution_report: &BlockExecutionReport,
        preexecution_metrics: &ParallelSpeculativeExecutionMetrics,
        reference: ParallelismReference,
    ) -> Self {
        let observed_services = service_nanos_by_transaction(plan, preexecution_report);
        let observed_service_dag_bound_nanos = observed_services
            .as_ref()
            .map(|values| dag_bound_from_services(plan, values));
        let observed_service_work_nanos = observed_services
            .as_ref()
            .map(|values| values.iter().copied().fold(0_u64, u64::saturating_add));
        let worker_capacity_bound_nanos = observed_service_work_nanos.and_then(|work| {
            let workers = u64::try_from(preexecution_metrics.workers).ok()?;
            ceil_div(work, workers)
        });
        let parallel_lower_bound_nanos = match (
            observed_service_dag_bound_nanos,
            worker_capacity_bound_nanos,
        ) {
            (Some(dag), Some(capacity)) => Some(dag.max(capacity)),
            (Some(dag), None) => Some(dag),
            (None, Some(capacity)) => Some(capacity),
            (None, None) => None,
        };
        let actual_execution_wall_nanos = nanos(
            preexecution_metrics
                .dependency_diagnostics
                .worker_phase_wall,
        );
        Self {
            serial_equivalent_work_nanos: reference.serial_equivalent_work_nanos,
            serial_cost_dag_bound_nanos: reference.serial_cost_dag_bound_nanos,
            observed_service_dag_bound_nanos,
            observed_service_work_nanos,
            worker_capacity_bound_nanos,
            parallel_lower_bound_nanos,
            actual_execution_wall_nanos,
            service_inflation_milli: match (
                observed_service_dag_bound_nanos,
                reference.serial_cost_dag_bound_nanos,
            ) {
                (Some(observed), Some(serial)) => ratio_milli(observed, serial),
                _ => None,
            },
            scheduler_realization_milli: observed_service_dag_bound_nanos
                .and_then(|observed| ratio_milli(actual_execution_wall_nanos, observed)),
            scheduler_realization_corrected_milli: parallel_lower_bound_nanos
                .and_then(|bound| ratio_milli(actual_execution_wall_nanos, bound)),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FeedbackTimingRecord {
    pub pre_execution_update_nanos: u64,
    pub reconciliation_update_nanos: u64,
    pub total_nanos: u64,
}

impl FeedbackTimingRecord {
    pub fn from_durations(pre_execution_update: Duration, reconciliation_update: Duration) -> Self {
        let pre_execution_update_nanos = nanos(pre_execution_update);
        let reconciliation_update_nanos = nanos(reconciliation_update);
        Self {
            pre_execution_update_nanos,
            reconciliation_update_nanos,
            total_nanos: pre_execution_update_nanos.saturating_add(reconciliation_update_nanos),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PipelineTimingRecord {
    /// Wall time spent adapting inputs and producing the execution plan.
    pub planning_nanos: u64,
    /// Wall time from speculative prepare through the pre-execution report.
    pub preexecution_nanos: u64,
    /// Wall time spent applying pre-execution feedback.
    pub pre_execution_feedback_nanos: u64,
    /// Wall time spent validating/reconciling the prepared block.
    pub reconciliation_nanos: u64,
    /// Wall time spent applying post-consensus feedback.
    pub reconciliation_feedback_nanos: u64,
    /// Measured wall time from planning start through completion of post-consensus feedback.
    pub total_adaptive_block_nanos: u64,
    /// Serial measured-block execution wall used as the non-adaptive baseline.
    pub serial_reference_execution_nanos: Option<u64>,
    /// `serial_reference_execution / total_adaptive_block`, in milli-units.
    pub end_to_end_speedup_milli: Option<u64>,
}

impl PipelineTimingRecord {
    pub fn from_durations(
        planning: Duration,
        preexecution: Duration,
        pre_execution_feedback: Duration,
        reconciliation: Duration,
        reconciliation_feedback: Duration,
        total_adaptive_block: Duration,
    ) -> Self {
        Self {
            planning_nanos: nanos(planning),
            preexecution_nanos: nanos(preexecution),
            pre_execution_feedback_nanos: nanos(pre_execution_feedback),
            reconciliation_nanos: nanos(reconciliation),
            reconciliation_feedback_nanos: nanos(reconciliation_feedback),
            total_adaptive_block_nanos: nanos(total_adaptive_block),
            serial_reference_execution_nanos: None,
            end_to_end_speedup_milli: None,
        }
    }

    pub fn with_serial_reference(mut self, serial_reference: Duration) -> Self {
        let serial_reference_execution_nanos = nanos(serial_reference);
        self.serial_reference_execution_nanos = Some(serial_reference_execution_nanos);
        self.end_to_end_speedup_milli = ratio_milli(
            serial_reference_execution_nanos,
            self.total_adaptive_block_nanos,
        );
        self
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct FeedbackRecord {
    pub positive_observations: u64,
    pub negative_observations: u64,
    pub fallback_edges_created: u64,
    pub candidate_misses: u64,
    pub replay_impact_observations: u64,
    pub attributed_replay_cost_nanos: u64,
    pub attributed_invalidated_descendants: u64,
    #[serde(default)]
    pub observation_batches_applied: u64,
    pub serialization_cost_observations: u64,
    pub attributed_serialization_cost_nanos: u64,
    #[serde(default)]
    pub serialization_cost_batches_applied: u64,
}

impl From<ApplySummary> for FeedbackRecord {
    fn from(summary: ApplySummary) -> Self {
        Self {
            positive_observations: u64::try_from(summary.positive_observations).unwrap_or(u64::MAX),
            negative_observations: u64::try_from(summary.negative_observations).unwrap_or(u64::MAX),
            fallback_edges_created: u64::try_from(summary.fallback_edges_created)
                .unwrap_or(u64::MAX),
            candidate_misses: u64::try_from(summary.candidate_misses).unwrap_or(u64::MAX),
            replay_impact_observations: u64::try_from(summary.replay_impact_observations)
                .unwrap_or(u64::MAX),
            attributed_replay_cost_nanos: summary.attributed_replay_cost_nanos,
            attributed_invalidated_descendants: summary.attributed_invalidated_descendants,
            observation_batches_applied: u64::try_from(summary.observation_batches_applied)
                .unwrap_or(u64::MAX),
            serialization_cost_observations: u64::try_from(summary.serialization_cost_observations)
                .unwrap_or(u64::MAX),
            attributed_serialization_cost_nanos: summary.attributed_serialization_cost_nanos,
            serialization_cost_batches_applied: u64::try_from(
                summary.serialization_cost_batches_applied,
            )
            .unwrap_or(u64::MAX),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct CorrectnessRecord {
    pub canonical_state_digest: Option<String>,
    pub serial_reference_digest: Option<String>,
    pub serial_equivalent: Option<bool>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExperimentRecord {
    pub schema_version: u16,
    pub metadata: ExperimentMetadata,
    pub planning: PlanningRecord,
    pub scheduling: SchedulingRecord,
    pub parallelism: ParallelismRecord,
    pub execution: ExecutionRecord,
    pub feedback: FeedbackRecord,
    pub feedback_timing: FeedbackTimingRecord,
    #[serde(default)]
    pub pipeline_timing: PipelineTimingRecord,
    pub correctness: CorrectnessRecord,
}

impl ExperimentRecord {
    #[allow(clippy::too_many_arguments)]
    pub fn from_runtime(
        metadata: ExperimentMetadata,
        planning_metrics: AdaptivePlanningMetrics,
        planning_config: &AdaptivePlanningConfig,
        plan: &AdaptiveBlockPlan,
        preexecution_report: &BlockExecutionReport,
        preexecution_metrics: &ParallelSpeculativeExecutionMetrics,
        reconciliation: &SplitPhaseSpeculativeExecutionReport,
        parallelism_reference: ParallelismReference,
        feedback_summary: ApplySummary,
        feedback_timing: FeedbackTimingRecord,
        pipeline_timing: PipelineTimingRecord,
        correctness: CorrectnessRecord,
    ) -> Self {
        Self {
            schema_version: EXPERIMENT_RECORD_SCHEMA_VERSION,
            metadata,
            planning: planning_metrics.into(),
            scheduling: SchedulingRecord::from_plan(plan, planning_config),
            parallelism: ParallelismRecord::from_reports(
                plan,
                preexecution_report,
                preexecution_metrics,
                parallelism_reference,
            ),
            execution: ExecutionRecord::from_reports(preexecution_metrics, reconciliation),
            feedback: feedback_summary.into(),
            feedback_timing,
            pipeline_timing,
            correctness,
        }
    }

    pub fn to_pretty_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }

    pub fn to_json_line(&self) -> Result<Vec<u8>, serde_json::Error> {
        let mut bytes = serde_json::to_vec(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, ExperimentRecordError> {
        let record: Self = serde_json::from_slice(bytes)?;
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> Result<(), ExperimentRecordError> {
        if self.schema_version != EXPERIMENT_RECORD_SCHEMA_VERSION
            && self.schema_version != PREVIOUS_EXPERIMENT_RECORD_SCHEMA_VERSION
            && self.schema_version != LEGACY_EXPERIMENT_RECORD_SCHEMA_VERSION
        {
            return Err(ExperimentRecordError::UnsupportedSchemaVersion {
                actual: self.schema_version,
                supported: EXPERIMENT_RECORD_SCHEMA_VERSION,
            });
        }
        Ok(())
    }

    pub fn append_jsonl(&self, path: impl AsRef<Path>) -> Result<(), ExperimentRecordError> {
        self.validate()?;
        let mut file = OpenOptions::new().create(true).append(true).open(path)?;
        file.write_all(&self.to_json_line()?)?;
        Ok(())
    }
}

fn service_nanos_by_transaction(
    plan: &AdaptiveBlockPlan,
    report: &BlockExecutionReport,
) -> Option<Vec<u64>> {
    let transaction_count = plan.candidate_graph.transactions().len();
    if report.transactions.len() != transaction_count {
        return None;
    }
    let mut services = vec![None; transaction_count];
    for execution in &report.transactions {
        if execution.transaction_index >= transaction_count
            || services[execution.transaction_index].is_some()
        {
            return None;
        }
        services[execution.transaction_index] = Some(nanos(execution.timing.service_duration));
    }
    services.into_iter().collect()
}

fn dag_bound_from_services(plan: &AdaptiveBlockPlan, services: &[u64]) -> u64 {
    let mut completion = vec![0_u64; services.len()];
    for wave in &plan.schedule.waves {
        for transaction in &wave.transaction_indices {
            let transaction = transaction.0 as usize;
            let predecessor_completion = plan
                .schedule
                .ordering_dependencies
                .iter()
                .filter(|dependency| dependency.successor.0 as usize == transaction)
                .map(|dependency| completion[dependency.predecessor.0 as usize])
                .max()
                .unwrap_or(0);
            completion[transaction] = predecessor_completion.saturating_add(services[transaction]);
        }
    }
    completion.into_iter().max().unwrap_or(0)
}

fn ceil_div(numerator: u64, denominator: u64) -> Option<u64> {
    if denominator == 0 {
        None
    } else {
        Some(numerator / denominator + u64::from(numerator % denominator != 0))
    }
}

fn ratio_milli(numerator: u64, denominator: u64) -> Option<u64> {
    if denominator == 0 {
        None
    } else {
        Some(
            numerator
                .saturating_mul(1000)
                .saturating_add(denominator / 2)
                / denominator,
        )
    }
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

#[derive(Debug, Error)]
pub enum ExperimentRecordError {
    #[error(
        "unsupported experiment record schema version {actual}; supported version is {supported}"
    )]
    UnsupportedSchemaVersion { actual: u16, supported: u16 },
    #[error("invalid experiment JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to write experiment record: {0}")]
    Io(#[from] std::io::Error),
}
