use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, AtomicU32, Ordering as AtomicOrdering},
    time::{Duration, Instant},
};

use acg_candidate_graph::{
    CandidateGraph, CandidateGraphBuilder, CandidateGraphError, CostAwareEdgePolicyConfig,
    EdgeClass, RiskBoundedSchedule, RiskBoundedScheduler, RiskBoundedSchedulerConfig,
    ScheduledDependency, ScheduledWave, SchedulingError as GraphSchedulingError,
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
    SerialBlockExecutor, SplitPhaseSpeculativeExecutionReport, TransactionExecution,
};
use thiserror::Error;

use crate::{
    AggregatedSerializationCostBuffer, RuntimeFeedbackEngine, RuntimeFeedbackError,
    ValidationEvidence, ValidationEvidenceKind,
};

/// Cheap admission gate that can bypass adaptive graph construction when smoothed recent
/// planning + execution economics do not beat serial-equivalent service work.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SerialBypassConfig {
    pub enabled: bool,
    pub min_transactions: usize,
    pub min_projected_speedup: f64,
    /// Service-cost scale used to discount optimistic prior-block speedups for very cheap
    /// transactions, where VM/control-plane fixed costs dominate.
    pub service_cost_reference_nanos_per_transaction: u64,
    /// EMA weight applied to the newest whole-block economics observation.
    pub economics_ema_alpha: f64,
    /// Minimum number of observed blocks before the admission gate is allowed to bypass.
    pub min_economics_observations: u32,
    /// Entry/exit margin around `min_projected_speedup` used to prevent mode flapping.
    pub projected_speedup_hysteresis: f64,
    /// Bound on consecutive serial pre-execution decisions before one adaptive block is forced
    /// to refresh the counterfactual parallel economics. This is an admission re-probe, not
    /// scheduler exploration: the probe still uses the normal symbolic graph and selected policy.
    pub max_consecutive_bypasses: u32,
}

impl Default for SerialBypassConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            min_transactions: 32,
            min_projected_speedup: 1.05,
            service_cost_reference_nanos_per_transaction: 150_000,
            economics_ema_alpha: 0.35,
            min_economics_observations: 4,
            projected_speedup_hysteresis: 0.10,
            max_consecutive_bypasses: 4,
        }
    }
}

impl SerialBypassConfig {
    fn validate(&self) -> Result<(), AdaptivePipelineError> {
        if self.min_transactions == 0 {
            return Err(AdaptivePipelineError::InvalidSerialBypassMinTransactions);
        }
        if !self.min_projected_speedup.is_finite() || self.min_projected_speedup <= 0.0 {
            return Err(AdaptivePipelineError::InvalidSerialBypassSpeedup(
                self.min_projected_speedup,
            ));
        }
        if self.service_cost_reference_nanos_per_transaction == 0 {
            return Err(AdaptivePipelineError::InvalidSerialBypassServiceCostReference);
        }
        if !self.economics_ema_alpha.is_finite()
            || self.economics_ema_alpha <= 0.0
            || self.economics_ema_alpha > 1.0
        {
            return Err(AdaptivePipelineError::InvalidSerialBypassEmaAlpha(
                self.economics_ema_alpha,
            ));
        }
        if self.min_economics_observations == 0 {
            return Err(AdaptivePipelineError::InvalidSerialBypassEconomicsObservations);
        }
        if !self.projected_speedup_hysteresis.is_finite() || self.projected_speedup_hysteresis < 0.0
        {
            return Err(AdaptivePipelineError::InvalidSerialBypassHysteresis(
                self.projected_speedup_hysteresis,
            ));
        }
        if self.max_consecutive_bypasses == 0 {
            return Err(AdaptivePipelineError::InvalidSerialBypassMaxConsecutive);
        }
        Ok(())
    }
}

/// Runtime-facing configuration for Phase 4D adaptive block planning.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdaptivePlanningConfig {
    /// Posterior floor for unresolved `Unknown` static candidate relationships. Proven symbolic
    /// and runtime-discovered topology stays materialized so concrete evidence can soften it.
    pub edge_materialization_threshold: f64,
    /// Evaluation/ablation switch for compact equivalence-group materialization. Production and
    /// normal benchmark runs keep this enabled; disabling it builds the dense logical reference
    /// graph so compact-vs-dense semantic equivalence and control-plane savings can be measured.
    pub compact_equivalence_groups: bool,
    /// Hard/soft thresholds, risk budget and optional wave capacity from Phase 4C.
    pub scheduler: RiskBoundedSchedulerConfig,
    /// Phase 5D expected replay-cost policy used to turn posterior probability into scheduling risk.
    pub cost_policy: CostAwareEdgePolicyConfig,
    /// Optional economics gate evaluated from prior whole-block observations before request
    /// adaptation, candidate-graph construction or scheduling.
    pub serial_bypass: SerialBypassConfig,
}

impl Default for AdaptivePlanningConfig {
    fn default() -> Self {
        Self {
            edge_materialization_threshold: 0.05,
            compact_equivalence_groups: true,
            scheduler: RiskBoundedSchedulerConfig::default(),
            cost_policy: CostAwareEdgePolicyConfig::default(),
            serial_bypass: SerialBypassConfig::default(),
        }
    }
}

impl AdaptivePlanningConfig {
    pub fn validate(&self) -> Result<(), AdaptivePipelineError> {
        WeightedCandidateGraphConfig {
            epoch: 0,
            edge_materialization_threshold: self.edge_materialization_threshold,
            cost_policy: self.cost_policy,
            compact_immature_equivalence_edges: self.compact_equivalence_groups,
            independent_observations_before_softening: self
                .scheduler
                .independent_observations_before_softening,
        }
        .validate()?;
        self.scheduler.validate()?;
        self.serial_bypass.validate()?;
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

/// Phase 5D.1 concrete attribution for one validation conflict that forced replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplayAttribution {
    pub predecessor: TxIndex,
    pub transaction: TxIndex,
    pub conflict_kinds: ConflictKinds,
    /// Exact stale concrete dependency (key/range/balance/metadata) that failed validation.
    pub conflict: ValidationConflict,
    /// Share of the replay's measured canonical execution cost attributed to this concrete cause.
    pub replay_cost_nanos: u64,
    /// Number of later replayed transactions transitively reachable through concrete invalidation
    /// evidence from this transaction.
    pub invalidated_descendants: u32,
    /// Whether the candidate graph contained the relationship that caused the replay.
    pub candidate_edge_present: bool,
}

/// Phase 5E realized marginal dependency-ready delay for one scheduled edge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SerializationAttribution {
    pub predecessor: TxIndex,
    pub transaction: TxIndex,
    pub class: EdgeClass,
    pub predecessor_completed_nanos: u64,
    pub alternate_ready_nanos: u64,
    pub successor_started_nanos: u64,
    pub marginal_ready_delay_nanos: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct RecentBlockEconomics {
    projected_speedup: f64,
    mean_service_nanos_per_transaction: f64,
    observations: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct SerialBypassProjection {
    projected_speedup: f64,
    mean_service_nanos_per_transaction: u64,
    admission_score: f64,
    observations: u32,
}

fn serial_bypass_threshold(config: SerialBypassConfig, active: bool) -> f64 {
    if active {
        config.min_projected_speedup + config.projected_speedup_hysteresis
    } else {
        (config.min_projected_speedup - config.projected_speedup_hysteresis).max(0.0)
    }
}

fn should_force_adaptive_probe(
    config: SerialBypassConfig,
    active: bool,
    consecutive_bypasses: u32,
) -> bool {
    active && consecutive_bypasses >= config.max_consecutive_bypasses
}

/// One Phase 4D planning result before any speculative parallel executor exists.
#[derive(Debug)]
pub struct AdaptiveBlockPlan {
    pub candidate_graph: CandidateGraph,
    pub schedule: RiskBoundedSchedule,
    /// Runtime dependency plan. `waves` are scheduler levels for diagnostics; split-phase Phase
    /// 5C.6 execution uses `dependencies` as a ready DAG rather than imposing level barriers.
    pub speculative_execution_plan: ExecutionPlan,
    pub serial_bypassed: bool,
    pub serial_bypass_projected_speedup_milli: Option<u64>,
    pub serial_bypass_mean_service_nanos: Option<u64>,
    pub serial_bypass_admission_score_milli: Option<u64>,
}

impl AdaptiveBlockPlan {
    /// Serial block-order execution used as the Phase 4D correctness boundary.
    ///
    /// Candidate `TxIndex` values are block positions, so this plan deliberately ignores the
    /// speculative wave grouping and executes the finalized/predicted block order one-by-one.
    pub fn canonical_serial_execution_plan(&self) -> ExecutionPlan {
        canonical_serial_plan(self.candidate_graph.transactions().len())
    }

    pub fn is_serial_bypassed(&self) -> bool {
        self.serial_bypassed
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
    /// The actual execution plan used by Phase 4D. This remains canonical and serial until Phase 5.
    pub execution_plan: ExecutionPlan,
    pub execution_report: BlockExecutionReport,
    pub feedback_summary: ApplySummary,
}

/// Connects the CosmWasm adapter, weighted candidate graph, Phase 4C scheduler and Phase 3 feedback
/// engine while retaining canonical serial execution.
///
/// Each call plans from the feedback state available at the start of the block, executes the block
/// serially, then applies concrete canonical evidence. The next block therefore observes the
/// updated posterior without requiring speculative parallel commit semantics.
pub struct AdaptiveSerialPipeline {
    adapter: CosmWasmCandidateAdapter,
    feedback: RuntimeFeedbackEngine,
    planning_config: AdaptivePlanningConfig,
    recent_economics: Option<RecentBlockEconomics>,
    serial_bypass_active: AtomicBool,
    consecutive_serial_bypasses: AtomicU32,
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
            recent_economics: None,
            serial_bypass_active: AtomicBool::new(false),
            consecutive_serial_bypasses: AtomicU32::new(0),
        })
    }

    pub fn feedback_store(&self) -> &AdaptiveFeedbackStore {
        self.feedback.store()
    }

    pub fn planning_config(&self) -> &AdaptivePlanningConfig {
        &self.planning_config
    }

    /// Evaluation-only seam for paired representation experiments.
    ///
    /// The feedback store and execution state are intentionally retained while only the physical
    /// equivalence-group representation is switched. This lets dense and compact measured blocks
    /// start from the same learned posterior instead of allowing representation-dependent warm-up
    /// trajectories to become a confounder.
    pub fn set_compact_equivalence_groups(&mut self, enabled: bool) {
        self.planning_config.compact_equivalence_groups = enabled;
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
        if plan.is_serial_bypassed() {
            return Ok(ApplySummary::default());
        }

        let access_summary = self.feedback.process_pre_execution(
            profile_graph,
            &plan.candidate_graph,
            report,
            epoch,
        )?;
        let serialization = serialization_cost_aggregates(plan, report)?;
        let serialization_summary = self
            .feedback
            .process_serialization_cost_aggregates(serialization, epoch)?;
        Ok(merge_apply_summaries(access_summary, serialization_summary))
    }

    /// Apply only probability/topology evidence from pre-execution.
    ///
    /// This is the Phase 4/5D ablation path used by the common benchmark harness. It deliberately
    /// excludes Phase 5E serialization-cost observations so cost-aware scheduling can be compared
    /// against probability-only learning without changing execution correctness.
    pub fn process_pre_execution_probability_only(
        &mut self,
        profile_graph: &ProfileGraph,
        plan: &AdaptiveBlockPlan,
        report: &BlockExecutionReport,
        epoch: u64,
    ) -> Result<ApplySummary, AdaptivePipelineError> {
        if plan.is_serial_bypassed() {
            return Ok(ApplySummary::default());
        }

        Ok(self.feedback.process_pre_execution(
            profile_graph,
            &plan.candidate_graph,
            report,
            epoch,
        )?)
    }

    /// Apply replay/canonical conflict evidence without replay-cost or fan-out evidence.
    ///
    /// Replayed traces still update conflict probabilities and runtime-discovered topology, but
    /// validation causes are represented as ordinary invalidations rather than cost-bearing replay
    /// observations. This keeps the ablation scientifically useful while canonical replay remains
    /// exactly the same as in the cost-aware mode.
    pub fn process_reconciliation_probability_only(
        &mut self,
        profile_graph: &ProfileGraph,
        plan: &AdaptiveBlockPlan,
        report: &SplitPhaseSpeculativeExecutionReport,
        epoch: u64,
    ) -> Result<ApplySummary, AdaptivePipelineError> {
        if plan.is_serial_bypassed() {
            return Ok(ApplySummary::default());
        }

        let (aligned_report, replayed_transactions) =
            candidate_aligned_replay_report(plan, report)?;
        let replay_summary = self.feedback.process_replay_execution(
            profile_graph,
            &plan.candidate_graph,
            &aligned_report,
            &replayed_transactions,
            epoch,
        )?;

        let attributions = reconciliation_attributions(plan, report)?;
        let evidence = attributions
            .iter()
            .map(|attribution| ValidationEvidence {
                predecessor: attribution.predecessor,
                transaction: attribution.transaction,
                kind: ValidationEvidenceKind::Invalidated {
                    conflict_kinds: attribution.conflict_kinds,
                },
            })
            .collect::<Vec<_>>();
        let validation_summary = self.feedback.process_validation(
            profile_graph,
            &plan.candidate_graph,
            &evidence,
            epoch,
        )?;
        Ok(merge_apply_summaries(replay_summary, validation_summary))
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
        if plan.is_serial_bypassed() {
            return Ok(ApplySummary::default());
        }

        // Replayed transactions are real post-consensus executions. Compare each corrected replay
        // trace with the final canonical outcomes of every transaction in the decided block, but
        // only emit observations for pairs containing at least one replayed transaction. This
        // captures replay-vs-reused evidence without double-counting reused-vs-reused pairs that
        // were already observed during pre-execution.
        let (aligned_report, replayed_transactions) =
            candidate_aligned_replay_report(plan, report)?;
        let replay_summary = self.feedback.process_replay_execution(
            profile_graph,
            &plan.candidate_graph,
            &aligned_report,
            &replayed_transactions,
            epoch,
        )?;

        // Validation attribution adds targeted positive + cost evidence for the concrete
        // dependency that forced each replay. Direct replay time is split across that transaction's
        // concrete conflict attributions so one replay cannot be counted multiple times as cost.
        let attributions = reconciliation_attributions(plan, report)?;
        let evidence = attributions
            .iter()
            .map(|item| ValidationEvidence {
                predecessor: item.predecessor,
                transaction: item.transaction,
                kind: ValidationEvidenceKind::Replayed {
                    conflict_kinds: item.conflict_kinds,
                    replay_cost_nanos: item.replay_cost_nanos,
                    invalidated_descendants: item.invalidated_descendants,
                },
            })
            .collect::<Vec<_>>();
        let validation_summary = self.feedback.process_validation(
            profile_graph,
            &plan.candidate_graph,
            &evidence,
            epoch,
        )?;
        Ok(merge_apply_summaries(replay_summary, validation_summary))
    }

    /// Return Phase 5D.1 replay attributions without mutating feedback state.
    pub fn reconciliation_attributions(
        &self,
        plan: &AdaptiveBlockPlan,
        report: &SplitPhaseSpeculativeExecutionReport,
    ) -> Result<Vec<ReplayAttribution>, AdaptivePipelineError> {
        reconciliation_attributions(plan, report)
    }

    /// Return Phase 5E dependency-ready delay attributions without mutating feedback state.
    pub fn serialization_attributions(
        &self,
        plan: &AdaptiveBlockPlan,
        report: &BlockExecutionReport,
    ) -> Result<Vec<SerializationAttribution>, AdaptivePipelineError> {
        serialization_attributions(plan, report)
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

        // Admission depends only on prior whole-block economics and current block cardinality.
        // Decide before adapting requests so the direct serial path pays no candidate-binding cost.
        let admission_projection = self.serial_bypass_projection(block.transactions.len());
        if let Some(projection) = admission_projection {
            if self.serial_bypass_decision(projection) {
                let candidate_graph = CandidateGraph::serial_bypass(block.transactions.len())?;
                let schedule = serial_bypass_schedule(candidate_graph.transactions().len());
                let speculative_execution_plan = execution_plan_from_schedule(&schedule)?;
                return Ok((
                    AdaptiveBlockPlan {
                        candidate_graph,
                        schedule,
                        speculative_execution_plan,
                        serial_bypassed: true,
                        serial_bypass_projected_speedup_milli: Some(to_milli(
                            projection.projected_speedup,
                        )),
                        serial_bypass_mean_service_nanos: Some(
                            projection.mean_service_nanos_per_transaction,
                        ),
                        serial_bypass_admission_score_milli: Some(to_milli(
                            projection.admission_score,
                        )),
                    },
                    AdaptivePlanningMetrics::default(),
                ));
            }
        }

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
                cost_policy: self.planning_config.cost_policy,
                compact_immature_equivalence_edges: self.planning_config.compact_equivalence_groups,
                independent_observations_before_softening: self
                    .planning_config
                    .scheduler
                    .independent_observations_before_softening,
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
                serial_bypassed: false,
                serial_bypass_projected_speedup_milli: admission_projection
                    .map(|projection| to_milli(projection.projected_speedup)),
                serial_bypass_mean_service_nanos: admission_projection
                    .map(|projection| projection.mean_service_nanos_per_transaction),
                serial_bypass_admission_score_milli: admission_projection
                    .map(|projection| to_milli(projection.admission_score)),
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

    /// Record consensus-aware block economics for the next block's admission gate.
    ///
    /// Admission now optimizes the system objective directly: serial-equivalent service work
    /// divided by the slower of the pre-consensus and post-consensus stages. This replaces the
    /// Phase-5 complexity discount, which became stale once compact planning reduced control-plane
    /// cost by an order of magnitude.
    pub fn observe_block_economics(
        &mut self,
        serial_service_nanos: u64,
        transaction_count: usize,
        pre_consensus: Duration,
        post_consensus: Duration,
        serial_bypassed: bool,
    ) {
        if transaction_count == 0 || serial_service_nanos == 0 {
            return;
        }
        let denominator = duration_to_u64_nanos(pre_consensus.max(post_consensus)).max(1);
        let transaction_count = u64::try_from(transaction_count).unwrap_or(u64::MAX).max(1);
        let sample_speedup = serial_service_nanos as f64 / denominator as f64;
        let sample_service = serial_service_nanos as f64 / transaction_count as f64;
        let alpha = self.planning_config.serial_bypass.economics_ema_alpha;

        self.recent_economics = match self.recent_economics {
            Some(previous) => Some(RecentBlockEconomics {
                // A serial bypass measures serial execution, not the counterfactual adaptive
                // speedup. Keep the last adaptive speedup estimate so bypass cannot become
                // self-confirming; service complexity still adapts and can release hysteresis.
                projected_speedup: if serial_bypassed {
                    previous.projected_speedup
                } else {
                    alpha * sample_speedup + (1.0 - alpha) * previous.projected_speedup
                },
                mean_service_nanos_per_transaction: alpha * sample_service
                    + (1.0 - alpha) * previous.mean_service_nanos_per_transaction,
                observations: if serial_bypassed {
                    previous.observations
                } else {
                    previous.observations.saturating_add(1)
                },
            }),
            None if serial_bypassed => None,
            None => Some(RecentBlockEconomics {
                projected_speedup: sample_speedup,
                mean_service_nanos_per_transaction: sample_service,
                observations: 1,
            }),
        };
    }

    pub fn recent_projected_speedup(&self) -> Option<f64> {
        self.recent_economics
            .map(|economics| economics.projected_speedup)
    }

    fn serial_bypass_projection(&self, transaction_count: usize) -> Option<SerialBypassProjection> {
        let config = self.planning_config.serial_bypass;
        if !config.enabled || transaction_count < config.min_transactions {
            return None;
        }
        let recent = self.recent_economics?;
        // The projected speedup already includes planning, execution, replay and the consensus
        // cutoff split. Do not discount cheap transactions a second time: Phase 5 showed that this
        // caused false serial bypasses after planner compression made cheap parallel blocks viable.
        let admission_score = recent.projected_speedup;
        Some(SerialBypassProjection {
            projected_speedup: recent.projected_speedup,
            mean_service_nanos_per_transaction: recent
                .mean_service_nanos_per_transaction
                .round()
                .clamp(0.0, u64::MAX as f64) as u64,
            admission_score,
            observations: recent.observations,
        })
    }

    fn serial_bypass_decision(&self, projection: SerialBypassProjection) -> bool {
        let config = self.planning_config.serial_bypass;
        if projection.observations < config.min_economics_observations {
            self.serial_bypass_active
                .store(false, AtomicOrdering::Relaxed);
            self.consecutive_serial_bypasses
                .store(0, AtomicOrdering::Relaxed);
            return false;
        }

        let active = self.serial_bypass_active.load(AtomicOrdering::Relaxed);
        if should_force_adaptive_probe(
            config,
            active,
            self.consecutive_serial_bypasses
                .load(AtomicOrdering::Relaxed),
        ) {
            // A serial block does not reveal the counterfactual adaptive throughput. Force one
            // ordinary adaptive block periodically so the EMA can observe whether the regime has
            // changed and release the bypass state. No candidate edge is explored beyond what the
            // selected scheduling policy would normally admit.
            self.serial_bypass_active
                .store(false, AtomicOrdering::Relaxed);
            self.consecutive_serial_bypasses
                .store(0, AtomicOrdering::Relaxed);
            return false;
        }

        let threshold = serial_bypass_threshold(config, active);
        let bypass = projection.admission_score < threshold;
        self.serial_bypass_active
            .store(bypass, AtomicOrdering::Relaxed);
        if bypass {
            self.consecutive_serial_bypasses
                .fetch_add(1, AtomicOrdering::Relaxed);
        } else {
            self.consecutive_serial_bypasses
                .store(0, AtomicOrdering::Relaxed);
        }
        bypass
    }

    /// Runs one complete Phase 4D iteration.
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
        let feedback_summary = if plan.is_serial_bypassed() {
            ApplySummary::default()
        } else {
            self.feedback.process_block(
                profile_graph,
                &plan.candidate_graph,
                &execution_report,
                block.context.height,
            )?
        };

        Ok(AdaptiveBlockRun {
            plan,
            execution_plan,
            execution_report,
            feedback_summary,
        })
    }
}

fn serialization_attributions(
    plan: &AdaptiveBlockPlan,
    report: &BlockExecutionReport,
) -> Result<Vec<SerializationAttribution>, AdaptivePipelineError> {
    let mut attributions = Vec::new();
    visit_serialization_attributions(plan, report, |attribution| {
        attributions.push(attribution);
        Ok(())
    })?;
    Ok(attributions)
}

fn serialization_cost_aggregates(
    plan: &AdaptiveBlockPlan,
    report: &BlockExecutionReport,
) -> Result<AggregatedSerializationCostBuffer, AdaptivePipelineError> {
    let mut buffer = AggregatedSerializationCostBuffer::default();
    visit_serialization_attributions(plan, report, |attribution| {
        let provenance = plan
            .candidate_graph
            .candidate_provenance_between(attribution.predecessor, attribution.transaction)
            .ok_or(
                RuntimeFeedbackError::SerializationEvidenceMissingCandidateEdge {
                    predecessor: attribution.predecessor,
                    transaction: attribution.transaction,
                },
            )?;
        buffer.record(provenance, attribution.marginal_ready_delay_nanos);
        Ok(())
    })?;
    Ok(buffer)
}

fn visit_serialization_attributions(
    plan: &AdaptiveBlockPlan,
    report: &BlockExecutionReport,
    mut visit: impl FnMut(SerializationAttribution) -> Result<(), AdaptivePipelineError>,
) -> Result<(), AdaptivePipelineError> {
    let transaction_count = plan.candidate_graph.transactions().len();
    let mut timings = vec![None; transaction_count];
    for execution in &report.transactions {
        if execution.transaction_index >= transaction_count {
            return Err(RuntimeFeedbackError::ExecutionIndexOutOfBounds {
                index: execution.transaction_index,
                candidate_count: transaction_count,
            }
            .into());
        }
        if timings[execution.transaction_index]
            .replace(execution.timing)
            .is_some()
        {
            return Err(
                RuntimeFeedbackError::DuplicateExecutionIndex(execution.transaction_index).into(),
            );
        }
    }

    let mut predecessors_by_successor = BTreeMap::<TxIndex, Vec<TxIndex>>::new();
    for dependency in &plan.schedule.ordering_dependencies {
        predecessors_by_successor
            .entry(dependency.successor)
            .or_default()
            .push(dependency.predecessor);
    }

    for dependency in &plan.schedule.ordering_dependencies {
        let predecessor_index = dependency.predecessor.0 as usize;
        let successor_index = dependency.successor.0 as usize;
        let (Some(predecessor_timing), Some(successor_timing)) =
            (timings[predecessor_index], timings[successor_index])
        else {
            continue;
        };
        let Some(predecessors) = predecessors_by_successor.get(&dependency.successor) else {
            continue;
        };
        if predecessors
            .iter()
            .any(|index| timings[index.0 as usize].is_none())
        {
            continue;
        }
        if predecessor_timing.completed_after_phase > successor_timing.started_after_phase {
            return Err(AdaptivePipelineError::SerializationTimingViolation {
                predecessor: dependency.predecessor,
                transaction: dependency.successor,
                predecessor_completed_nanos: duration_to_u64_nanos(
                    predecessor_timing.completed_after_phase,
                ),
                successor_started_nanos: duration_to_u64_nanos(
                    successor_timing.started_after_phase,
                ),
            });
        }
        let alternate_ready = predecessors
            .iter()
            .filter(|index| **index != dependency.predecessor)
            .filter_map(|index| timings[index.0 as usize])
            .map(|timing| timing.completed_after_phase)
            .max()
            .unwrap_or(Duration::ZERO);
        let marginal = predecessor_timing
            .completed_after_phase
            .saturating_sub(alternate_ready);
        visit(SerializationAttribution {
            predecessor: dependency.predecessor,
            transaction: dependency.successor,
            class: dependency.class,
            predecessor_completed_nanos: duration_to_u64_nanos(
                predecessor_timing.completed_after_phase,
            ),
            alternate_ready_nanos: duration_to_u64_nanos(alternate_ready),
            successor_started_nanos: duration_to_u64_nanos(successor_timing.started_after_phase),
            marginal_ready_delay_nanos: duration_to_u64_nanos(marginal),
        })?;
    }
    Ok(())
}

fn candidate_aligned_replay_report(
    plan: &AdaptiveBlockPlan,
    report: &SplitPhaseSpeculativeExecutionReport,
) -> Result<(BlockExecutionReport, BTreeSet<TxIndex>), AdaptivePipelineError> {
    let index_by_id = plan
        .candidate_graph
        .transactions()
        .iter()
        .enumerate()
        .map(|(index, candidate)| (candidate.tx_id.0, index))
        .collect::<BTreeMap<_, _>>();
    let mut replayed = BTreeSet::new();
    for diagnostic in &report.reconciliation {
        if diagnostic.disposition != CanonicalTxDisposition::Replayed {
            continue;
        }
        let Some(&candidate_index) = index_by_id.get(&diagnostic.transaction_id.0) else {
            continue;
        };
        replayed.insert(TxIndex(u32::try_from(candidate_index).map_err(|_| {
            RuntimeFeedbackError::TransactionIndexOverflow(candidate_index)
        })?));
    }

    let mut transactions = Vec::new();
    for execution in &report.block.transactions {
        let Some(&candidate_index) = index_by_id.get(&execution.transaction_id.0) else {
            continue;
        };
        let Ok(outcome) = &execution.result else {
            continue;
        };
        transactions.push(TransactionExecution {
            transaction_index: candidate_index,
            transaction_id: execution.transaction_id,
            result: Ok(outcome.clone()),
            timing: execution.timing,
        });
    }
    Ok((
        BlockExecutionReport {
            block_height: report.block.block_height,
            block_time_nanos: report.block.block_time_nanos,
            transactions,
        },
        replayed,
    ))
}

fn reconciliation_attributions(
    plan: &AdaptiveBlockPlan,
    report: &SplitPhaseSpeculativeExecutionReport,
) -> Result<Vec<ReplayAttribution>, AdaptivePipelineError> {
    let mut evidence_count_by_transaction = BTreeMap::<usize, usize>::new();
    let mut adjacency = BTreeMap::<usize, BTreeSet<usize>>::new();
    for item in &report.dependency_evidence {
        *evidence_count_by_transaction
            .entry(item.transaction_index)
            .or_default() += 1;
        adjacency
            .entry(item.predecessor_index)
            .or_default()
            .insert(item.transaction_index);
    }

    let replayed = report
        .reconciliation
        .iter()
        .filter(|item| item.disposition == CanonicalTxDisposition::Replayed)
        .map(|item| item.transaction_index)
        .collect::<BTreeSet<_>>();

    let candidate_index_by_id = plan
        .candidate_graph
        .transactions()
        .iter()
        .enumerate()
        .map(|(index, candidate)| (candidate.tx_id.0, index))
        .collect::<BTreeMap<_, _>>();
    let mut descendant_cache = BTreeMap::<usize, u32>::new();
    let mut attribution_ordinal_by_transaction = BTreeMap::<usize, usize>::new();
    let mut attributions = Vec::with_capacity(report.dependency_evidence.len());
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
        let Some(predecessor_diagnostic) = report.reconciliation.get(item.predecessor_index) else {
            continue;
        };
        let Some(&predecessor_candidate_index) =
            candidate_index_by_id.get(&predecessor_diagnostic.transaction_id.0)
        else {
            continue;
        };
        let Some(&transaction_candidate_index) =
            candidate_index_by_id.get(&transaction.transaction_id.0)
        else {
            continue;
        };
        let predecessor = TxIndex(u32::try_from(predecessor_candidate_index).map_err(|_| {
            RuntimeFeedbackError::TransactionIndexOverflow(predecessor_candidate_index)
        })?);
        let transaction_index =
            TxIndex(u32::try_from(transaction_candidate_index).map_err(|_| {
                RuntimeFeedbackError::TransactionIndexOverflow(transaction_candidate_index)
            })?);
        let attribution_count = evidence_count_by_transaction
            .get(&item.transaction_index)
            .copied()
            .unwrap_or(1)
            .max(1);
        let ordinal = attribution_ordinal_by_transaction
            .entry(item.transaction_index)
            .or_default();
        let replay_total_nanos = duration_to_u64_nanos(transaction.reexecution_duration);
        let divisor = u64::try_from(attribution_count).unwrap_or(u64::MAX).max(1);
        let ordinal_u64 = u64::try_from(*ordinal).unwrap_or(u64::MAX);
        let remainder_bonus = if ordinal_u64 < replay_total_nanos % divisor {
            1
        } else {
            0
        };
        let replay_cost_nanos = replay_total_nanos / divisor + remainder_bonus;
        let total_invalidated_descendants = *descendant_cache
            .entry(item.transaction_index)
            .or_insert_with(|| {
                replay_descendant_count(item.transaction_index, &adjacency, &replayed)
            });
        let descendant_divisor = u32::try_from(attribution_count).unwrap_or(u32::MAX).max(1);
        let descendant_ordinal = u32::try_from(*ordinal).unwrap_or(u32::MAX);
        let descendant_remainder_bonus =
            if descendant_ordinal < total_invalidated_descendants % descendant_divisor {
                1
            } else {
                0
            };
        let invalidated_descendants =
            total_invalidated_descendants / descendant_divisor + descendant_remainder_bonus;
        *ordinal = (*ordinal).saturating_add(1);
        attributions.push(ReplayAttribution {
            predecessor,
            transaction: transaction_index,
            conflict_kinds: conflict_kinds_for_validation(conflict),
            conflict: conflict.clone(),
            replay_cost_nanos,
            invalidated_descendants,
            candidate_edge_present: plan
                .candidate_graph
                .contains_candidate_pair(predecessor, transaction_index),
        });
    }
    Ok(attributions)
}

fn replay_descendant_count(
    root: usize,
    adjacency: &BTreeMap<usize, BTreeSet<usize>>,
    replayed: &BTreeSet<usize>,
) -> u32 {
    let mut pending = adjacency
        .get(&root)
        .into_iter()
        .flatten()
        .copied()
        .collect::<Vec<_>>();
    let mut seen = BTreeSet::new();
    while let Some(transaction) = pending.pop() {
        if !seen.insert(transaction) {
            continue;
        }
        if let Some(children) = adjacency.get(&transaction) {
            pending.extend(children.iter().copied());
        }
    }
    u32::try_from(
        seen.iter()
            .filter(|index| replayed.contains(*index))
            .count(),
    )
    .unwrap_or(u32::MAX)
}

fn to_milli(value: f64) -> u64 {
    (value * 1000.0).round().clamp(0.0, u64::MAX as f64) as u64
}

fn duration_to_u64_nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn serial_bypass_schedule(transaction_count: usize) -> RiskBoundedSchedule {
    let waves = (0..transaction_count)
        .map(|index| ScheduledWave {
            transaction_indices: vec![TxIndex(u32::try_from(index).unwrap_or(u32::MAX))],
        })
        .collect::<Vec<_>>();
    let ordering_dependencies = (1..transaction_count)
        .map(|index| ScheduledDependency {
            predecessor: TxIndex(u32::try_from(index - 1).unwrap_or(u32::MAX)),
            successor: TxIndex(u32::try_from(index).unwrap_or(u32::MAX)),
            class: acg_candidate_graph::EdgeClass::Hard,
        })
        .collect::<Vec<_>>();
    RiskBoundedSchedule {
        transaction_count,
        waves,
        pre_reduction_ordering_dependencies: ordering_dependencies.len(),
        hard_dependencies_elided_by_reduction: 0,
        ordering_dependencies,
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
        replay_impact_observations: left.replay_impact_observations
            + right.replay_impact_observations,
        attributed_replay_cost_nanos: left
            .attributed_replay_cost_nanos
            .saturating_add(right.attributed_replay_cost_nanos),
        attributed_invalidated_descendants: left
            .attributed_invalidated_descendants
            .saturating_add(right.attributed_invalidated_descendants),
        observation_batches_applied: left
            .observation_batches_applied
            .saturating_add(right.observation_batches_applied),
        serialization_cost_observations: left
            .serialization_cost_observations
            .saturating_add(right.serialization_cost_observations),
        attributed_serialization_cost_nanos: left
            .attributed_serialization_cost_nanos
            .saturating_add(right.attributed_serialization_cost_nanos),
        serialization_cost_batches_applied: left
            .serialization_cost_batches_applied
            .saturating_add(right.serialization_cost_batches_applied),
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
    #[error("serial bypass minimum transaction count must be at least one")]
    InvalidSerialBypassMinTransactions,
    #[error("serial bypass minimum projected speedup must be finite and positive, got {0}")]
    InvalidSerialBypassSpeedup(f64),
    #[error("serial bypass service-cost reference must be non-zero")]
    InvalidSerialBypassServiceCostReference,
    #[error("serial bypass EMA alpha must be finite and within (0, 1], got {0}")]
    InvalidSerialBypassEmaAlpha(f64),
    #[error("serial bypass minimum economics observations must be at least one")]
    InvalidSerialBypassEconomicsObservations,
    #[error("serial bypass hysteresis must be finite and non-negative, got {0}")]
    InvalidSerialBypassHysteresis(f64),
    #[error("serial bypass maximum consecutive bypass count must be at least one")]
    InvalidSerialBypassMaxConsecutive,
    #[error(
        "scheduled dependency {predecessor:?} -> {transaction:?} completed at {predecessor_completed_nanos}ns after successor started at {successor_started_nanos}ns"
    )]
    SerializationTimingViolation {
        predecessor: TxIndex,
        transaction: TxIndex,
        predecessor_completed_nanos: u64,
        successor_started_nanos: u64,
    },
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
    fn serial_bypass_config_validates_ema_and_hysteresis() {
        let invalid_alpha = AdaptivePlanningConfig {
            serial_bypass: SerialBypassConfig {
                economics_ema_alpha: 0.0,
                ..SerialBypassConfig::default()
            },
            ..AdaptivePlanningConfig::default()
        };
        assert!(matches!(
            invalid_alpha.validate().unwrap_err(),
            AdaptivePipelineError::InvalidSerialBypassEmaAlpha(_)
        ));

        let invalid_observations = AdaptivePlanningConfig {
            serial_bypass: SerialBypassConfig {
                min_economics_observations: 0,
                ..SerialBypassConfig::default()
            },
            ..AdaptivePlanningConfig::default()
        };
        assert!(matches!(
            invalid_observations.validate().unwrap_err(),
            AdaptivePipelineError::InvalidSerialBypassEconomicsObservations
        ));

        let invalid_bypass_streak = AdaptivePlanningConfig {
            serial_bypass: SerialBypassConfig {
                max_consecutive_bypasses: 0,
                ..SerialBypassConfig::default()
            },
            ..AdaptivePlanningConfig::default()
        };
        assert!(matches!(
            invalid_bypass_streak.validate().unwrap_err(),
            AdaptivePipelineError::InvalidSerialBypassMaxConsecutive
        ));
    }

    #[test]
    fn serial_bypass_hysteresis_requires_a_stronger_signal_to_exit() {
        let config = SerialBypassConfig {
            min_projected_speedup: 1.05,
            projected_speedup_hysteresis: 0.10,
            ..SerialBypassConfig::default()
        };
        assert!((serial_bypass_threshold(config, false) - 0.95).abs() < 1.0e-12);
        assert!((serial_bypass_threshold(config, true) - 1.15).abs() < 1.0e-12);
    }

    #[test]
    fn serial_bypass_forces_a_bounded_counterfactual_refresh() {
        let config = SerialBypassConfig {
            max_consecutive_bypasses: 4,
            ..SerialBypassConfig::default()
        };
        assert!(!should_force_adaptive_probe(config, false, 4));
        assert!(!should_force_adaptive_probe(config, true, 3));
        assert!(should_force_adaptive_probe(config, true, 4));
        assert!(should_force_adaptive_probe(config, true, 9));
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
            pre_reduction_ordering_dependencies: 0,
            hard_dependencies_elided_by_reduction: 0,
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
