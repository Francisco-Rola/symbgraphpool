use std::{
    collections::BTreeSet,
    time::{Duration, Instant},
};

use acg_candidate_graph::{
    CandidateGraph, CandidateGraphBuilder, CandidateGraphError, RiskBoundedSchedule,
    RiskBoundedScheduler, RiskBoundedSchedulerConfig, SchedulingError as GraphSchedulingError,
    WeightedCandidateGraphConfig,
};
use acg_core::{ConflictKinds, TxIndex};
use acg_cosmwasm_adapter::{AdapterError, CosmWasmCandidateAdapter};
use acg_cosmwasm_engine::{CanonicalTxDisposition, CosmWasmEngine, ValidationConflict};
use acg_feedback::{AdaptiveFeedbackStore, ApplySummary};
use acg_profile_graph::ProfileGraph;
use acg_validator_sim::{
    BlockExecutionError, BlockExecutionReport, ExecutionDependency, ExecutionDependencyClass,
    ExecutionPlan, ExecutionWave, ProducedBlock, SchedulingError as RuntimeSchedulingError,
    SerialBlockExecutor, SplitPhaseSpeculativeExecutionReport,
};
use thiserror::Error;

use crate::{
    RuntimeFeedbackEngine, RuntimeFeedbackError, ValidationEvidence, ValidationEvidenceKind,
};

/// Runtime-facing configuration for Brick 4D adaptive block planning.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdaptivePlanningConfig {
    /// Posterior floor for unresolved `Unknown` static candidate relationships. Proven symbolic
    /// and runtime-discovered topology stays materialized so concrete evidence can soften it.
    pub edge_materialization_threshold: f64,
    /// Hard/soft thresholds, risk budget and optional wave capacity from Brick 4C.
    pub scheduler: RiskBoundedSchedulerConfig,
}

impl Default for AdaptivePlanningConfig {
    fn default() -> Self {
        Self {
            edge_materialization_threshold: 0.05,
            scheduler: RiskBoundedSchedulerConfig::default(),
        }
    }
}

impl AdaptivePlanningConfig {
    pub fn validate(&self) -> Result<(), AdaptivePipelineError> {
        WeightedCandidateGraphConfig {
            epoch: 0,
            edge_materialization_threshold: self.edge_materialization_threshold,
        }
        .validate()?;
        self.scheduler.validate()?;
        Ok(())
    }
}

/// Wall-clock breakdown of one adaptive pre-consensus planning pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdaptivePlanningMetrics {
    pub adapter: Duration,
    pub candidate_graph: Duration,
    pub scheduler: Duration,
    pub schedule_validation: Duration,
    pub plan_conversion: Duration,
}

impl AdaptivePlanningMetrics {
    pub fn total(&self) -> Duration {
        self.adapter
            + self.candidate_graph
            + self.scheduler
            + self.schedule_validation
            + self.plan_conversion
    }
}

/// One Brick 4D planning result before any speculative parallel executor exists.
#[derive(Debug)]
pub struct AdaptiveBlockPlan {
    pub candidate_graph: CandidateGraph,
    pub schedule: RiskBoundedSchedule,
    /// Runtime dependency plan. `waves` are scheduler levels for diagnostics; split-phase Brick
    /// 5C.6 execution uses `dependencies` as a ready DAG rather than imposing level barriers.
    pub speculative_execution_plan: ExecutionPlan,
}

impl AdaptiveBlockPlan {
    /// Serial block-order execution used as the Brick 4D correctness boundary.
    ///
    /// Candidate `TxIndex` values are block positions, so this plan deliberately ignores the
    /// speculative wave grouping and executes the finalized/predicted block order one-by-one.
    pub fn canonical_serial_execution_plan(&self) -> ExecutionPlan {
        canonical_serial_plan(self.candidate_graph.transactions().len())
    }

    pub fn max_wave_width(&self) -> usize {
        self.schedule
            .waves
            .iter()
            .map(|wave| wave.transaction_indices.len())
            .max()
            .unwrap_or(0)
    }
}

/// Result of one adaptive plan -> serial execution -> runtime-feedback iteration.
#[derive(Debug)]
pub struct AdaptiveBlockRun {
    pub plan: AdaptiveBlockPlan,
    /// The actual execution plan used by Brick 4D. This remains canonical and serial until Brick 5.
    pub execution_plan: ExecutionPlan,
    pub execution_report: BlockExecutionReport,
    pub feedback_summary: ApplySummary,
}

/// Connects the CosmWasm adapter, weighted candidate graph, Brick 4C scheduler and Brick 3 feedback
/// engine while retaining canonical serial execution.
///
/// Each call plans from the feedback state available at the start of the block, executes the block
/// serially, then applies concrete canonical evidence. The next block therefore observes the
/// updated posterior without requiring speculative parallel commit semantics.
pub struct AdaptiveSerialPipeline {
    adapter: CosmWasmCandidateAdapter,
    feedback: RuntimeFeedbackEngine,
    planning_config: AdaptivePlanningConfig,
}

impl AdaptiveSerialPipeline {
    pub fn new(
        adapter: CosmWasmCandidateAdapter,
        feedback: RuntimeFeedbackEngine,
        planning_config: AdaptivePlanningConfig,
    ) -> Result<Self, AdaptivePipelineError> {
        planning_config.validate()?;
        Ok(Self {
            adapter,
            feedback,
            planning_config,
        })
    }

    pub fn feedback_store(&self) -> &AdaptiveFeedbackStore {
        self.feedback.store()
    }

    pub fn planning_config(&self) -> &AdaptivePlanningConfig {
        &self.planning_config
    }

    /// Apply concrete accesses observed during pre-consensus speculative execution.
    ///
    /// This is intentionally separate from planning so callers can decide when a completed
    /// predicted block becomes evidence for future blocks. In the split-phase pipeline it is
    /// applied immediately after pre-execution finishes.
    pub fn process_pre_execution_report(
        &mut self,
        profile_graph: &ProfileGraph,
        plan: &AdaptiveBlockPlan,
        report: &BlockExecutionReport,
        epoch: u64,
    ) -> Result<ApplySummary, AdaptivePipelineError> {
        Ok(self.feedback.process_pre_execution(
            profile_graph,
            &plan.candidate_graph,
            report,
            epoch,
        )?)
    }

    /// Apply replay evidence from post-consensus reconciliation.
    ///
    /// Each item is a concrete read dependency that failed validation and was attributed by the
    /// engine to the nearest prepared canonical predecessor whose write set changed that value.
    /// Replays therefore reinforce real dependencies in addition to the pre-execution access
    /// evidence used for negative/positive learning.
    pub fn process_reconciliation_report(
        &mut self,
        profile_graph: &ProfileGraph,
        plan: &AdaptiveBlockPlan,
        report: &SplitPhaseSpeculativeExecutionReport,
        epoch: u64,
    ) -> Result<ApplySummary, AdaptivePipelineError> {
        // Replayed transactions are real post-consensus executions. Compare each corrected replay
        // trace with the final canonical outcomes of every transaction in the decided block, but
        // only emit observations for pairs containing at least one replayed transaction. This
        // captures replay-vs-reused evidence without double-counting reused-vs-reused pairs that
        // were already observed during pre-execution.
        let replayed_transactions = report
            .reconciliation
            .iter()
            .filter(|diagnostic| diagnostic.disposition == CanonicalTxDisposition::Replayed)
            .map(|diagnostic| {
                Ok(TxIndex(
                    u32::try_from(diagnostic.transaction_index).map_err(|_| {
                        RuntimeFeedbackError::TransactionIndexOverflow(diagnostic.transaction_index)
                    })?,
                ))
            })
            .collect::<Result<BTreeSet<_>, RuntimeFeedbackError>>()?;
        let replay_summary = self.feedback.process_replay_execution(
            profile_graph,
            &plan.candidate_graph,
            &report.block,
            &replayed_transactions,
            epoch,
        )?;

        // Validation attribution adds targeted positive evidence for the concrete dependency that
        // forced each replay. This is stronger than access-overlap evidence because it records a
        // value that was actually stale relative to a canonical predecessor.
        let mut evidence = Vec::with_capacity(report.dependency_evidence.len());
        for item in &report.dependency_evidence {
            let Some(transaction) = report.reconciliation.get(item.transaction_index) else {
                continue;
            };
            let Some(validation) = transaction.validation.as_ref() else {
                continue;
            };
            let Some(conflict) = validation.conflicts().get(item.conflict_index) else {
                continue;
            };
            evidence.push(ValidationEvidence {
                predecessor: TxIndex(u32::try_from(item.predecessor_index).map_err(|_| {
                    RuntimeFeedbackError::TransactionIndexOverflow(item.predecessor_index)
                })?),
                transaction: TxIndex(u32::try_from(item.transaction_index).map_err(|_| {
                    RuntimeFeedbackError::TransactionIndexOverflow(item.transaction_index)
                })?),
                kind: ValidationEvidenceKind::Replayed {
                    conflict_kinds: conflict_kinds_for_validation(conflict),
                },
            });
        }
        let validation_summary = self.feedback.process_validation(
            profile_graph,
            &plan.candidate_graph,
            &evidence,
            epoch,
        )?;
        Ok(merge_apply_summaries(replay_summary, validation_summary))
    }

    /// Creates the weighted transaction graph and risk-bounded waves for `block` without executing
    /// or mutating feedback state.
    pub fn plan_block(
        &self,
        engine: &CosmWasmEngine,
        profile_graph: &ProfileGraph,
        block: &ProducedBlock,
    ) -> Result<AdaptiveBlockPlan, AdaptivePipelineError> {
        self.plan_block_with_metrics(engine, profile_graph, block)
            .map(|(plan, _)| plan)
    }

    /// Same planning operation as [`Self::plan_block`], with a wall-clock breakdown for the
    /// pre-consensus budget. The timings are instrumentation only and do not affect scheduling.
    pub fn plan_block_with_metrics(
        &self,
        engine: &CosmWasmEngine,
        profile_graph: &ProfileGraph,
        block: &ProducedBlock,
    ) -> Result<(AdaptiveBlockPlan, AdaptivePlanningMetrics), AdaptivePipelineError> {
        let epoch = block.context.height;

        let started = Instant::now();
        let candidates = self.adapter.adapt_block(engine, profile_graph, block)?;
        let adapter = started.elapsed();

        let started = Instant::now();
        let candidate_graph = CandidateGraphBuilder::new(profile_graph).build_weighted(
            candidates,
            self.feedback.store(),
            self.feedback.adaptive_config(),
            WeightedCandidateGraphConfig {
                epoch,
                edge_materialization_threshold: self.planning_config.edge_materialization_threshold,
            },
        )?;
        let candidate_graph_elapsed = started.elapsed();

        let scheduler = RiskBoundedScheduler::new(self.planning_config.scheduler)?;
        let started = Instant::now();
        let schedule = scheduler.schedule(&candidate_graph)?;
        let scheduler_elapsed = started.elapsed();

        let started = Instant::now();
        schedule.validate_against(&candidate_graph, &self.planning_config.scheduler)?;
        let schedule_validation = started.elapsed();

        let started = Instant::now();
        let speculative_execution_plan = execution_plan_from_schedule(&schedule)?;
        let plan_conversion = started.elapsed();

        Ok((
            AdaptiveBlockPlan {
                candidate_graph,
                schedule,
                speculative_execution_plan,
            },
            AdaptivePlanningMetrics {
                adapter,
                candidate_graph: candidate_graph_elapsed,
                scheduler: scheduler_elapsed,
                schedule_validation,
                plan_conversion,
            },
        ))
    }

    /// Runs one complete Brick 4D iteration.
    ///
    /// The adaptive schedule is retained for inspection/metrics, but the actual execution plan is
    /// canonical and serial. Concrete access evidence is applied only after execution completes.
    pub fn run_block(
        &mut self,
        engine: &CosmWasmEngine,
        profile_graph: &ProfileGraph,
        block: &ProducedBlock,
    ) -> Result<AdaptiveBlockRun, AdaptivePipelineError> {
        let plan = self.plan_block(engine, profile_graph, block)?;
        let execution_plan = plan.canonical_serial_execution_plan();
        execution_plan.validate()?;
        let execution_report =
            SerialBlockExecutor::new(engine.clone()).execute(block, &execution_plan)?;
        let feedback_summary = self.feedback.process_block(
            profile_graph,
            &plan.candidate_graph,
            &execution_report,
            block.context.height,
        )?;

        Ok(AdaptiveBlockRun {
            plan,
            execution_plan,
            execution_report,
            feedback_summary,
        })
    }
}

fn execution_plan_from_schedule(
    schedule: &RiskBoundedSchedule,
) -> Result<ExecutionPlan, AdaptivePipelineError> {
    let plan = ExecutionPlan {
        transaction_count: schedule.transaction_count,
        waves: schedule
            .waves
            .iter()
            .map(|wave| ExecutionWave {
                transaction_indices: wave
                    .transaction_indices
                    .iter()
                    .map(|tx_index| tx_index.0 as usize)
                    .collect(),
            })
            .collect(),
        dependencies: schedule
            .ordering_dependencies
            .iter()
            .map(|dependency| ExecutionDependency {
                predecessor_index: dependency.predecessor.0 as usize,
                successor_index: dependency.successor.0 as usize,
                class: match dependency.class {
                    acg_candidate_graph::EdgeClass::Hard => ExecutionDependencyClass::Hard,
                    acg_candidate_graph::EdgeClass::Soft => ExecutionDependencyClass::Soft,
                    acg_candidate_graph::EdgeClass::Low => unreachable!(
                        "low candidate edges are never emitted as execution dependencies"
                    ),
                },
            })
            .collect(),
    };
    plan.validate()?;
    Ok(plan)
}

fn canonical_serial_plan(transaction_count: usize) -> ExecutionPlan {
    ExecutionPlan {
        transaction_count,
        waves: (0..transaction_count)
            .map(|index| ExecutionWave {
                transaction_indices: vec![index],
            })
            .collect(),
        dependencies: Vec::new(),
    }
}

fn merge_apply_summaries(left: ApplySummary, right: ApplySummary) -> ApplySummary {
    ApplySummary {
        positive_observations: left.positive_observations + right.positive_observations,
        negative_observations: left.negative_observations + right.negative_observations,
        fallback_edges_created: left.fallback_edges_created + right.fallback_edges_created,
        candidate_misses: left.candidate_misses + right.candidate_misses,
    }
}

fn conflict_kinds_for_validation(conflict: &ValidationConflict) -> ConflictKinds {
    match conflict {
        ValidationConflict::BankBalance { .. } | ValidationConflict::BankAllBalances { .. } => {
            ConflictKinds::WRITE_READ | ConflictKinds::BALANCE
        }
        ValidationConflict::ContractMetadata { .. }
        | ValidationConflict::Storage { .. }
        | ValidationConflict::StorageRange { .. } => ConflictKinds::WRITE_READ,
    }
}

#[derive(Debug, Error)]
pub enum AdaptivePipelineError {
    #[error(transparent)]
    Adapter(#[from] AdapterError),
    #[error(transparent)]
    CandidateGraph(#[from] CandidateGraphError),
    #[error(transparent)]
    GraphScheduling(#[from] GraphSchedulingError),
    #[error(transparent)]
    RuntimeScheduling(#[from] RuntimeSchedulingError),
    #[error(transparent)]
    Execution(#[from] BlockExecutionError),
    #[error(transparent)]
    Feedback(#[from] RuntimeFeedbackError),
}

#[cfg(test)]
mod tests {
    use acg_core::TxIndex;

    use super::*;

    #[test]
    fn planning_metrics_total_sums_every_instrumented_stage() {
        let metrics = AdaptivePlanningMetrics {
            adapter: Duration::from_millis(1),
            candidate_graph: Duration::from_millis(2),
            scheduler: Duration::from_millis(3),
            schedule_validation: Duration::from_millis(4),
            plan_conversion: Duration::from_millis(5),
        };
        assert_eq!(metrics.total(), Duration::from_millis(15));
    }

    #[test]
    fn planning_config_validates_materialization_and_scheduler_parameters() {
        let invalid_threshold = AdaptivePlanningConfig {
            edge_materialization_threshold: 1.1,
            ..AdaptivePlanningConfig::default()
        };
        assert!(matches!(
            invalid_threshold.validate().unwrap_err(),
            AdaptivePipelineError::CandidateGraph(
                CandidateGraphError::InvalidMaterializationThreshold(_)
            )
        ));

        let invalid_scheduler = AdaptivePlanningConfig {
            scheduler: RiskBoundedSchedulerConfig {
                soft_threshold: 0.9,
                hard_threshold: 0.8,
                ..RiskBoundedSchedulerConfig::default()
            },
            ..AdaptivePlanningConfig::default()
        };
        assert!(matches!(
            invalid_scheduler.validate().unwrap_err(),
            AdaptivePipelineError::GraphScheduling(GraphSchedulingError::ThresholdOrder { .. })
        ));
    }

    #[test]
    fn adaptive_waves_convert_to_runtime_plan_while_canonical_plan_stays_serial() {
        let schedule = RiskBoundedSchedule {
            transaction_count: 3,
            waves: vec![
                acg_candidate_graph::ScheduledWave {
                    transaction_indices: vec![TxIndex(0), TxIndex(2)],
                },
                acg_candidate_graph::ScheduledWave {
                    transaction_indices: vec![TxIndex(1)],
                },
            ],
            ordering_dependencies: Vec::new(),
        };
        let speculative = execution_plan_from_schedule(&schedule).unwrap();
        assert_eq!(speculative.waves[0].transaction_indices, vec![0, 2]);
        assert_eq!(speculative.waves[1].transaction_indices, vec![1]);

        let canonical = canonical_serial_plan(3);
        assert_eq!(canonical.waves.len(), 3);
        assert_eq!(canonical.waves[0].transaction_indices, vec![0]);
        assert_eq!(canonical.waves[1].transaction_indices, vec![1]);
        assert_eq!(canonical.waves[2].transaction_indices, vec![2]);
        canonical.validate().unwrap();
    }
}
