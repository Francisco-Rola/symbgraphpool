use acg_candidate_graph::{
    CandidateGraph, CandidateGraphBuilder, CandidateGraphError, RiskBoundedSchedule,
    RiskBoundedScheduler, RiskBoundedSchedulerConfig, SchedulingError as GraphSchedulingError,
    WeightedCandidateGraphConfig,
};
use acg_cosmwasm_adapter::{AdapterError, CosmWasmCandidateAdapter};
use acg_cosmwasm_engine::CosmWasmEngine;
use acg_feedback::{AdaptiveFeedbackStore, ApplySummary};
use acg_profile_graph::ProfileGraph;
use acg_validator_sim::{
    BlockExecutionError, BlockExecutionReport, ExecutionPlan, ExecutionWave, ProducedBlock,
    SchedulingError as RuntimeSchedulingError, SerialBlockExecutor,
};
use thiserror::Error;

use crate::{RuntimeFeedbackEngine, RuntimeFeedbackError};

/// Runtime-facing configuration for Brick 4D adaptive block planning.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdaptivePlanningConfig {
    /// Do not instantiate weighted candidate edges below this projected posterior probability.
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

/// One Brick 4D planning result before any speculative parallel executor exists.
#[derive(Debug)]
pub struct AdaptiveBlockPlan {
    pub candidate_graph: CandidateGraph,
    pub schedule: RiskBoundedSchedule,
    /// Runtime plan preserving the adaptive wave structure. The current serial executor must not
    /// execute this directly when any wave has width greater than one.
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

    /// Creates the weighted transaction graph and risk-bounded waves for `block` without executing
    /// or mutating feedback state.
    pub fn plan_block(
        &self,
        engine: &CosmWasmEngine,
        profile_graph: &ProfileGraph,
        block: &ProducedBlock,
    ) -> Result<AdaptiveBlockPlan, AdaptivePipelineError> {
        let epoch = block.context.height;
        let candidates = self.adapter.adapt_block(engine, profile_graph, block)?;
        let candidate_graph = CandidateGraphBuilder::new(profile_graph).build_weighted(
            candidates,
            self.feedback.store(),
            self.feedback.adaptive_config(),
            WeightedCandidateGraphConfig {
                epoch,
                edge_materialization_threshold: self.planning_config.edge_materialization_threshold,
            },
        )?;
        let scheduler = RiskBoundedScheduler::new(self.planning_config.scheduler)?;
        let schedule = scheduler.schedule(&candidate_graph)?;
        schedule.validate_against(&candidate_graph, &self.planning_config.scheduler)?;
        let speculative_execution_plan = execution_plan_from_schedule(&schedule)?;

        Ok(AdaptiveBlockPlan {
            candidate_graph,
            schedule,
            speculative_execution_plan,
        })
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
