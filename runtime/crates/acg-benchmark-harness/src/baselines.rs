//! Same-VM baseline strategies for cross-system evaluation.
//!
//! These implementations intentionally share the exact CosmWasm engine, block generator,
//! canonical state, worker budget, and concrete read-set validator used by SymbGraphPool. This
//! keeps execution substrate differences out of the first baseline comparison. Where an upstream
//! system permits a different serial order, the baseline keeps the consensus-decided order and
//! records that adaptation explicitly in [`StrategyRecord`].

use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

use acg_core::ConflictKinds;
use acg_cosmwasm_engine::{
    AccessKind, Address, ContractExecutionDiagnostics, CosmWasmEngine, ParallelExecutionConfig,
    ParallelSpeculativeExecutionMetrics, PreparedSpeculativeBlock, SpeculativeTxResult,
    TransactionId,
};
use acg_evaluation::{ConsensusExecutionRecord, StrategyRecord};
use acg_runtime_feedback::{AccessConflictDetector, ObservedConflict, TraceConflictConfig};
use acg_validator_sim::{
    BlockExecutionReport, ExecutionDependency, ExecutionDependencyClass, ExecutionPlan,
    ExecutionWave, ProducedBlock, SerialBlockExecutor, SpeculativeParallelBlockExecutor,
    SplitPhaseSpeculativeExecutionReport,
};
use serde::{Deserialize, Serialize};

use crate::{block_divergence_stats, canonical_serial_plan, nanos, HarnessError, HarnessMode};

/// Execution strategies accepted by the common benchmark harness.
///
/// The first four are cross-system controls/baselines. The final three are the existing
/// SymbGraphPool policy variants and preserve their historical manifest names.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionStrategy {
    Serial,
    AriaFb,
    Vegeta,
    ExactAccess,
    SymbGraphStatic,
    SymbGraphProbability,
    SymbGraphCost,
}

impl ExecutionStrategy {
    pub fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "serial" => Ok(Self::Serial),
            "aria-fb" | "ariafb" => Ok(Self::AriaFb),
            "vegeta" | "vegeta-like" => Ok(Self::Vegeta),
            "exact-access" | "exact-access-dag" => Ok(Self::ExactAccess),
            "static" | "static-prior" => Ok(Self::SymbGraphStatic),
            "probability" | "probability-only" => Ok(Self::SymbGraphProbability),
            "cost-aware" | "adaptive" => Ok(Self::SymbGraphCost),
            other => Err(HarnessError::UnsupportedMode(other.to_owned())),
        }
    }

    pub(crate) fn adaptive_mode(self) -> Option<HarnessMode> {
        match self {
            Self::SymbGraphStatic => Some(HarnessMode::Static),
            Self::SymbGraphProbability => Some(HarnessMode::ProbabilityOnly),
            Self::SymbGraphCost => Some(HarnessMode::CostAware),
            Self::Serial | Self::AriaFb | Self::Vegeta | Self::ExactAccess => None,
        }
    }

    pub(crate) fn is_baseline(self) -> bool {
        self.adaptive_mode().is_none()
    }
}

pub(crate) enum BaselineMeasuredExecution {
    Serial(Box<BaselineSerialExecution>),
    Speculative(Box<BaselineSpeculativeExecution>),
}

pub(crate) struct BaselineSerialExecution {
    pub report: BlockExecutionReport,
    pub wall: Duration,
    pub diagnostics: ContractExecutionDiagnostics,
}

pub(crate) struct BaselineSpeculativeExecution {
    pub preexecution_report: BlockExecutionReport,
    pub preexecution_metrics: ParallelSpeculativeExecutionMetrics,
    pub reconciliation: SplitPhaseSpeculativeExecutionReport,
}

pub(crate) struct MeasuredBaselineBlock {
    pub scheduling_plan: ExecutionPlan,
    pub parallelism_plan: ExecutionPlan,
    pub concrete_relationships: u64,
    pub planning_wall: Duration,
    pub preexecution_wall: Duration,
    pub reconciliation_wall: Duration,
    pub total_wall: Duration,
    pub consensus: ConsensusExecutionRecord,
    pub strategy: StrategyRecord,
    pub execution: BaselineMeasuredExecution,
}

pub(crate) struct BaselineExecutionContext<'a> {
    pub strategy: ExecutionStrategy,
    pub engine: &'a CosmWasmEngine,
    pub workers: usize,
    pub predicted_block: &'a ProducedBlock,
    pub decided_block: &'a ProducedBlock,
    pub oracle_report: Option<&'a BlockExecutionReport>,
    pub trace_config: TraceConflictConfig,
    pub consensus_cutoff: Duration,
    pub measured: bool,
}

pub(crate) fn execute_baseline_block(
    context: BaselineExecutionContext<'_>,
) -> Result<Option<MeasuredBaselineBlock>, HarnessError> {
    match context.strategy {
        ExecutionStrategy::Serial => execute_serial(context),
        ExecutionStrategy::AriaFb => execute_aria_fb(context),
        ExecutionStrategy::Vegeta => execute_vegeta(context),
        ExecutionStrategy::ExactAccess => execute_exact_access(context),
        ExecutionStrategy::SymbGraphStatic
        | ExecutionStrategy::SymbGraphProbability
        | ExecutionStrategy::SymbGraphCost => Err(HarnessError::Runtime(
            "adaptive SymbGraph strategy routed through baseline executor".to_owned(),
        )),
    }
}

fn execute_serial(
    context: BaselineExecutionContext<'_>,
) -> Result<Option<MeasuredBaselineBlock>, HarnessError> {
    let total_started = Instant::now();
    let execution_started = Instant::now();
    let (report, diagnostics) = SerialBlockExecutor::new(context.engine.clone())
        .execute_with_diagnostics(
            context.decided_block,
            &canonical_serial_plan(context.decided_block.transactions.len()),
        )
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
    let wall = execution_started.elapsed();
    let total_wall = total_started.elapsed();
    if !context.measured {
        return Ok(None);
    }

    let transaction_count = context.decided_block.transactions.len();
    let serial_plan = canonical_serial_record_plan(transaction_count);
    let consensus = consensus_record(
        context.predicted_block,
        context.decided_block,
        context.consensus_cutoff,
        Duration::ZERO,
        Duration::ZERO,
        wall,
        None,
    );
    Ok(Some(MeasuredBaselineBlock {
        scheduling_plan: serial_plan.clone(),
        parallelism_plan: serial_plan,
        concrete_relationships: 0,
        planning_wall: Duration::ZERO,
        preexecution_wall: Duration::ZERO,
        reconciliation_wall: wall,
        total_wall,
        consensus,
        strategy: StrategyRecord {
            family: "serial".to_owned(),
            implementation: "canonical-decided-order".to_owned(),
            ..StrategyRecord::default()
        },
        execution: BaselineMeasuredExecution::Serial(Box::new(BaselineSerialExecution {
            report,
            wall,
            diagnostics,
        })),
    }))
}

fn execute_aria_fb(
    context: BaselineExecutionContext<'_>,
) -> Result<Option<MeasuredBaselineBlock>, HarnessError> {
    let total_started = Instant::now();
    let executor = SpeculativeParallelBlockExecutor::new(
        context.engine.clone(),
        ParallelExecutionConfig {
            workers: context.workers,
        },
    );
    let plan = fully_parallel_plan(context.decided_block.transactions.len());

    // AriaFB is an order-execute baseline: the entire block is first executed against one
    // snapshot after consensus. We apply the Rule-2 forward-dependency test described by Vegeta's
    // AriaFB baseline, then retain the engine's concrete read-set validation as an additional
    // conservative boundary required to preserve the consensus-decided order exactly.
    let post_started = Instant::now();
    let execution_started = Instant::now();
    let mut prepared = executor
        .prepare(context.decided_block, &plan)
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
    let preexecution_report = executor
        .pre_execution_report(context.decided_block, &prepared)
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
    // AriaFB is entirely post-consensus, but keep its batch execution, conflict analysis, and
    // fallback/commit validation as disjoint pipeline stages. The previous accounting reported
    // the whole post-consensus interval as `reconciliation_wall` while also reporting the nested
    // planning interval separately, which double-counted planning in the stage sum.
    let batch_execution_wall = execution_started.elapsed();
    let planning_started = Instant::now();
    let conflicts = AccessConflictDetector::new(context.trace_config)
        .detect(&preexecution_report)
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
    let abort_indices = aria_rule2_forward_fallbacks(&conflicts);
    let abort_ids = abort_indices
        .iter()
        .filter_map(|&index| {
            context
                .decided_block
                .transactions
                .get(index)
                .map(|pending| pending.transaction_id())
        })
        .collect::<BTreeSet<_>>();
    prepared
        .receipts
        .retain(|receipt| !abort_ids.contains(&receipt.transaction_id));
    let planning_wall = planning_started.elapsed();
    let preexecution_metrics = prepared.metrics.clone();
    let reconciliation_started = Instant::now();
    let reconciliation = executor
        .validate_prepared(context.decided_block, prepared)
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
    let reconciliation_wall = reconciliation_started.elapsed();
    let post_consensus = post_started.elapsed();
    let total_wall = total_started.elapsed();
    if !context.measured {
        return Ok(None);
    }

    let consensus = consensus_record(
        context.predicted_block,
        context.decided_block,
        context.consensus_cutoff,
        Duration::ZERO,
        Duration::ZERO,
        post_consensus,
        None,
    );
    Ok(Some(MeasuredBaselineBlock {
        scheduling_plan: plan.clone(),
        parallelism_plan: plan,
        concrete_relationships: u64::try_from(conflicts.len()).unwrap_or(u64::MAX),
        planning_wall,
        preexecution_wall: batch_execution_wall,
        reconciliation_wall,
        total_wall,
        consensus,
        strategy: StrategyRecord {
            family: "aria-fb".to_owned(),
            implementation: "rule2-forward-fallback+canonical-readset-validation".to_owned(),
            discovered_conflicts: u64::try_from(conflicts.len()).unwrap_or(u64::MAX),
            forward_conflict_fallbacks: u64::try_from(abort_ids.len()).unwrap_or(u64::MAX),
            ..StrategyRecord::default()
        },
        execution: BaselineMeasuredExecution::Speculative(Box::new(BaselineSpeculativeExecution {
            preexecution_report,
            preexecution_metrics,
            reconciliation,
        })),
    }))
}

fn execute_vegeta(
    context: BaselineExecutionContext<'_>,
) -> Result<Option<MeasuredBaselineBlock>, HarnessError> {
    let total_started = Instant::now();
    let executor = SpeculativeParallelBlockExecutor::new(
        context.engine.clone(),
        ParallelExecutionConfig {
            workers: context.workers,
        },
    );

    // Speculation: execute candidate transactions fully in parallel without committing state.
    let discovery_plan = fully_parallel_plan(context.predicted_block.transactions.len());
    let discovery_started = Instant::now();
    let discovery = executor
        .prepare_with_cutoff(
            context.predicted_block,
            &discovery_plan,
            context.consensus_cutoff,
        )
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
    let discovery_report = executor
        .pre_execution_report(context.predicted_block, &discovery)
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
    let discovery_wall = discovery_started.elapsed();

    // Order: derive concrete dependencies from the speculative R/W footprints. We intentionally
    // keep canonical decided order instead of Vegeta Rule-1 transaction reordering, so this
    // same-harness baseline has exactly the same state-machine semantics as the serial reference.
    let planning_started = Instant::now();
    let conflicts = AccessConflictDetector::new(context.trace_config)
        .detect(&discovery_report)
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
    let replay_plan = dependency_plan_for_decided_order(
        context.predicted_block,
        context.decided_block,
        &conflicts,
    )?;
    let replay_dependencies = u64::try_from(replay_plan.dependencies.len()).unwrap_or(u64::MAX);
    let discovery_footprints = prepared_footprints(&discovery);
    let planning_wall = planning_started.elapsed();
    let pre_consensus_eligible = discovery_wall.saturating_add(planning_wall);
    let pre_consensus = pre_consensus_eligible.min(context.consensus_cutoff);
    let cutoff_overrun = pre_consensus_eligible.saturating_sub(context.consensus_cutoff);
    let discovery_metrics = discovery.metrics.clone();
    let discovery_receipts = u64::try_from(discovery.receipts.len()).unwrap_or(u64::MAX);
    let successful_receipts = u64::try_from(
        discovery
            .receipts
            .iter()
            .filter(|receipt| receipt.is_success())
            .count(),
    )
    .unwrap_or(u64::MAX);
    let failed_receipts = discovery_receipts.saturating_sub(successful_receipts);

    // Replay: execute the decided block using the dependency DAG supplied by speculation. Any
    // transaction whose concrete R/W footprint changes is withheld from the parallel commit and
    // deterministically falls back to canonical execution during reconciliation.
    let replay_started = Instant::now();
    let mut replay = executor
        .prepare(context.decided_block, &replay_plan)
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
    let access_set_mismatch_ids = replay
        .receipts
        .iter()
        .filter_map(|receipt| {
            let replay_footprint = access_footprint(receipt);
            match discovery_footprints.get(&receipt.transaction_id) {
                Some(discovery_footprint) if discovery_footprint == &replay_footprint => None,
                _ => Some(receipt.transaction_id),
            }
        })
        .collect::<BTreeSet<_>>();
    replay
        .receipts
        .retain(|receipt| !access_set_mismatch_ids.contains(&receipt.transaction_id));
    let reconciliation = executor
        .validate_prepared(context.decided_block, replay)
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
    let replay_parallel_wall = replay_started.elapsed();
    let post_consensus = cutoff_overrun.saturating_add(replay_parallel_wall);
    let total_wall = total_started.elapsed();
    if !context.measured {
        return Ok(None);
    }

    let consensus = consensus_record(
        context.predicted_block,
        context.decided_block,
        context.consensus_cutoff,
        pre_consensus,
        cutoff_overrun,
        post_consensus,
        Some(PreparedConsensusMetrics {
            prepared_receipts: discovery_receipts,
            successful_receipts,
            failed_receipts,
            receipts_ready_by_cutoff: discovery_metrics
                .dependency_diagnostics
                .receipts_ready_by_cutoff,
            receipts_completed_after_cutoff: discovery_metrics
                .dependency_diagnostics
                .receipts_completed_after_cutoff,
            cutoff_reached: discovery_metrics.dependency_diagnostics.cutoff_reached,
        }),
    );
    Ok(Some(MeasuredBaselineBlock {
        scheduling_plan: replay_plan,
        parallelism_plan: discovery_plan,
        concrete_relationships: u64::try_from(conflicts.len()).unwrap_or(u64::MAX),
        planning_wall,
        preexecution_wall: discovery_wall,
        reconciliation_wall: replay_parallel_wall,
        total_wall,
        consensus,
        strategy: StrategyRecord {
            family: "vegeta-like".to_owned(),
            implementation: "speculate-order-replay;canonical-order-no-rule1-reordering".to_owned(),
            pre_consensus_execution: true,
            discovery_transactions: u64::try_from(discovery_report.transactions.len())
                .unwrap_or(u64::MAX),
            discovered_conflicts: u64::try_from(conflicts.len()).unwrap_or(u64::MAX),
            access_set_mismatch_fallbacks: u64::try_from(access_set_mismatch_ids.len())
                .unwrap_or(u64::MAX),
            replay_dependencies,
            replay_parallel_nanos: nanos(replay_parallel_wall),
            ..StrategyRecord::default()
        },
        execution: BaselineMeasuredExecution::Speculative(Box::new(BaselineSpeculativeExecution {
            preexecution_report: discovery_report,
            preexecution_metrics: discovery_metrics,
            reconciliation,
        })),
    }))
}

fn execute_exact_access(
    context: BaselineExecutionContext<'_>,
) -> Result<Option<MeasuredBaselineBlock>, HarnessError> {
    if context.predicted_block.transactions != context.decided_block.transactions {
        return Err(HarnessError::WorkloadParameter(
            "exact-access baseline currently requires identical candidate and decided transactions; it is an evaluation-only declared-access oracle, not a consensus-lookahead oracle"
                .to_owned(),
        ));
    }
    let oracle_report = context.oracle_report.ok_or_else(|| {
        HarnessError::Runtime("exact-access baseline requires a serial oracle report".to_owned())
    })?;
    let total_started = Instant::now();
    let planning_started = Instant::now();
    let conflicts = AccessConflictDetector::new(context.trace_config)
        .detect(oracle_report)
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
    let plan =
        dependency_plan_from_conflicts(context.decided_block.transactions.len(), &conflicts)?;
    let replay_dependencies = u64::try_from(plan.dependencies.len()).unwrap_or(u64::MAX);
    let planning_wall = planning_started.elapsed();

    let executor = SpeculativeParallelBlockExecutor::new(
        context.engine.clone(),
        ParallelExecutionConfig {
            workers: context.workers,
        },
    );
    let remaining_budget = context.consensus_cutoff.saturating_sub(planning_wall);
    let preexecution_started = Instant::now();
    let prepared = executor
        .prepare_with_cutoff(context.predicted_block, &plan, remaining_budget)
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
    let preexecution_report = executor
        .pre_execution_report(context.predicted_block, &prepared)
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
    let preexecution_wall = preexecution_started.elapsed();
    let pre_consensus_eligible = planning_wall.saturating_add(preexecution_wall);
    let pre_consensus = pre_consensus_eligible.min(context.consensus_cutoff);
    let cutoff_overrun = pre_consensus_eligible.saturating_sub(context.consensus_cutoff);
    let prepared_receipts = u64::try_from(prepared.receipts.len()).unwrap_or(u64::MAX);
    let successful_receipts = u64::try_from(
        prepared
            .receipts
            .iter()
            .filter(|receipt| receipt.is_success())
            .count(),
    )
    .unwrap_or(u64::MAX);
    let failed_receipts = prepared_receipts.saturating_sub(successful_receipts);
    let preexecution_metrics = prepared.metrics.clone();

    let reconciliation_started = Instant::now();
    let reconciliation = executor
        .validate_prepared(context.decided_block, prepared)
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
    let reconciliation_wall = reconciliation_started.elapsed();
    let post_consensus = cutoff_overrun.saturating_add(reconciliation_wall);
    let total_wall = total_started.elapsed();
    if !context.measured {
        return Ok(None);
    }

    let consensus = consensus_record(
        context.predicted_block,
        context.decided_block,
        context.consensus_cutoff,
        pre_consensus,
        cutoff_overrun,
        post_consensus,
        Some(PreparedConsensusMetrics {
            prepared_receipts,
            successful_receipts,
            failed_receipts,
            receipts_ready_by_cutoff: preexecution_metrics
                .dependency_diagnostics
                .receipts_ready_by_cutoff,
            receipts_completed_after_cutoff: preexecution_metrics
                .dependency_diagnostics
                .receipts_completed_after_cutoff,
            cutoff_reached: preexecution_metrics.dependency_diagnostics.cutoff_reached,
        }),
    );
    Ok(Some(MeasuredBaselineBlock {
        scheduling_plan: plan.clone(),
        parallelism_plan: plan,
        concrete_relationships: u64::try_from(conflicts.len()).unwrap_or(u64::MAX),
        planning_wall,
        preexecution_wall,
        reconciliation_wall,
        total_wall,
        consensus,
        strategy: StrategyRecord {
            family: "exact-access".to_owned(),
            implementation: "hindsight-concrete-access-dag+canonical-readset-validation".to_owned(),
            pre_consensus_execution: true,
            oracle_accesses: true,
            discovery_transactions: u64::try_from(oracle_report.transactions.len())
                .unwrap_or(u64::MAX),
            discovered_conflicts: u64::try_from(conflicts.len()).unwrap_or(u64::MAX),
            replay_dependencies,
            ..StrategyRecord::default()
        },
        execution: BaselineMeasuredExecution::Speculative(Box::new(BaselineSpeculativeExecution {
            preexecution_report,
            preexecution_metrics,
            reconciliation,
        })),
    }))
}

#[derive(Clone, Copy)]
struct PreparedConsensusMetrics {
    prepared_receipts: u64,
    successful_receipts: u64,
    failed_receipts: u64,
    receipts_ready_by_cutoff: u64,
    receipts_completed_after_cutoff: u64,
    cutoff_reached: bool,
}

fn consensus_record(
    predicted_block: &ProducedBlock,
    decided_block: &ProducedBlock,
    cutoff: Duration,
    pre_consensus: Duration,
    pre_consensus_overrun: Duration,
    post_consensus: Duration,
    prepared: Option<PreparedConsensusMetrics>,
) -> ConsensusExecutionRecord {
    let divergence = block_divergence_stats(predicted_block, decided_block);
    let prepared = prepared.unwrap_or(PreparedConsensusMetrics {
        prepared_receipts: 0,
        successful_receipts: 0,
        failed_receipts: 0,
        receipts_ready_by_cutoff: 0,
        receipts_completed_after_cutoff: 0,
        cutoff_reached: false,
    });
    ConsensusExecutionRecord {
        cutoff_nanos: nanos(cutoff),
        candidate_transactions: u64::try_from(predicted_block.transactions.len())
            .unwrap_or(u64::MAX),
        decided_transactions: u64::try_from(decided_block.transactions.len()).unwrap_or(u64::MAX),
        shared_transactions: divergence.shared_transactions,
        same_position_transactions: divergence.same_position_transactions,
        common_prefix_transactions: divergence.common_prefix_transactions,
        prepared_receipts: prepared.prepared_receipts,
        successful_preexecution_receipts: Some(prepared.successful_receipts),
        failed_preexecution_receipts: Some(prepared.failed_receipts),
        receipts_ready_by_cutoff: prepared.receipts_ready_by_cutoff,
        receipts_completed_after_cutoff: prepared.receipts_completed_after_cutoff,
        cutoff_reached: prepared.cutoff_reached,
        pre_consensus_nanos: nanos(pre_consensus),
        pre_consensus_overrun_nanos: nanos(pre_consensus_overrun),
        post_consensus_nanos: nanos(post_consensus),
        bottleneck_nanos: nanos(pre_consensus.max(post_consensus)),
        ..ConsensusExecutionRecord::default()
    }
}

fn canonical_serial_record_plan(transaction_count: usize) -> ExecutionPlan {
    ExecutionPlan {
        transaction_count,
        waves: (0..transaction_count)
            .map(|transaction_index| ExecutionWave {
                transaction_indices: vec![transaction_index],
            })
            .collect(),
        dependencies: (1..transaction_count)
            .map(|successor_index| ExecutionDependency {
                predecessor_index: successor_index - 1,
                successor_index,
                class: ExecutionDependencyClass::Hard,
            })
            .collect(),
    }
}

fn fully_parallel_plan(transaction_count: usize) -> ExecutionPlan {
    ExecutionPlan {
        transaction_count,
        waves: if transaction_count == 0 {
            Vec::new()
        } else {
            vec![ExecutionWave {
                transaction_indices: (0..transaction_count).collect(),
            }]
        },
        dependencies: Vec::new(),
    }
}

fn dependency_plan_from_conflicts(
    transaction_count: usize,
    conflicts: &[ObservedConflict],
) -> Result<ExecutionPlan, HarnessError> {
    let pairs = conflicts
        .iter()
        .map(|conflict| (conflict.left.0 as usize, conflict.right.0 as usize))
        .collect::<BTreeSet<_>>();
    dependency_plan_from_pairs(transaction_count, pairs)
}

fn dependency_plan_for_decided_order(
    predicted_block: &ProducedBlock,
    decided_block: &ProducedBlock,
    conflicts: &[ObservedConflict],
) -> Result<ExecutionPlan, HarnessError> {
    let decided_index = decided_block
        .transactions
        .iter()
        .enumerate()
        .map(|(index, pending)| (pending.transaction_id(), index))
        .collect::<BTreeMap<_, _>>();
    let mut pairs = BTreeSet::new();
    for conflict in conflicts {
        let Some(left_id) = predicted_block
            .transactions
            .get(conflict.left.0 as usize)
            .map(|pending| pending.transaction_id())
        else {
            continue;
        };
        let Some(right_id) = predicted_block
            .transactions
            .get(conflict.right.0 as usize)
            .map(|pending| pending.transaction_id())
        else {
            continue;
        };
        let (Some(&left), Some(&right)) =
            (decided_index.get(&left_id), decided_index.get(&right_id))
        else {
            continue;
        };
        if left == right {
            continue;
        }
        pairs.insert(if left < right {
            (left, right)
        } else {
            (right, left)
        });
    }
    dependency_plan_from_pairs(decided_block.transactions.len(), pairs)
}

fn dependency_plan_from_pairs(
    transaction_count: usize,
    pairs: BTreeSet<(usize, usize)>,
) -> Result<ExecutionPlan, HarnessError> {
    for &(predecessor, successor) in &pairs {
        if predecessor >= transaction_count
            || successor >= transaction_count
            || predecessor >= successor
        {
            return Err(HarnessError::Runtime(format!(
                "invalid concrete dependency {predecessor}->{successor} for {transaction_count} transactions"
            )));
        }
    }
    let pairs = transitively_reduce_pairs(transaction_count, pairs);
    let mut levels = vec![0_usize; transaction_count];
    for &(predecessor, successor) in &pairs {
        levels[successor] = levels[successor].max(levels[predecessor].saturating_add(1));
    }
    let max_level = levels.iter().copied().max().unwrap_or(0);
    let mut waves = vec![Vec::new(); max_level.saturating_add(1)];
    for (transaction, level) in levels.into_iter().enumerate() {
        waves[level].push(transaction);
    }
    let waves = waves
        .into_iter()
        .filter(|transactions| !transactions.is_empty())
        .map(|transaction_indices| ExecutionWave {
            transaction_indices,
        })
        .collect::<Vec<_>>();
    let dependencies = pairs
        .into_iter()
        .map(|(predecessor_index, successor_index)| ExecutionDependency {
            predecessor_index,
            successor_index,
            class: ExecutionDependencyClass::Hard,
        })
        .collect::<Vec<_>>();
    let plan = ExecutionPlan {
        transaction_count,
        waves,
        dependencies,
    };
    plan.validate()
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
    Ok(plan)
}

fn transitively_reduce_pairs(
    transaction_count: usize,
    pairs: BTreeSet<(usize, usize)>,
) -> BTreeSet<(usize, usize)> {
    if pairs.len() <= 1 || transaction_count <= 1 {
        return pairs;
    }
    let mut outgoing = vec![Vec::<usize>::new(); transaction_count];
    for &(predecessor, successor) in &pairs {
        outgoing[predecessor].push(successor);
    }
    for successors in &mut outgoing {
        successors.sort_unstable();
        successors.dedup();
    }

    let words = transaction_count.div_ceil(64);
    let mut reachable = vec![vec![0_u64; words]; transaction_count];
    let mut reduced = BTreeSet::new();
    for predecessor in (0..transaction_count).rev() {
        for &successor in &outgoing[predecessor] {
            let word = successor / 64;
            let mask = 1_u64 << (successor % 64);
            if reachable[predecessor][word] & mask != 0 {
                continue;
            }
            reduced.insert((predecessor, successor));
            reachable[predecessor][word] |= mask;
            let successor_reachability = reachable[successor].clone();
            for (target, source) in reachable[predecessor]
                .iter_mut()
                .zip(successor_reachability)
            {
                *target |= source;
            }
        }
    }
    reduced
}

fn aria_rule2_forward_fallbacks(conflicts: &[ObservedConflict]) -> BTreeSet<usize> {
    #[derive(Default)]
    struct DependencyKinds {
        waw: bool,
        war: bool,
        raw: bool,
    }

    let mut by_transaction = BTreeMap::<usize, DependencyKinds>::new();
    for conflict in conflicts {
        let entry = by_transaction.entry(conflict.right.0 as usize).or_default();
        entry.waw |= conflict.conflict_kinds.contains(ConflictKinds::WRITE_WRITE);
        // left reads, right writes: the later/right transaction has a WAR dependency.
        entry.war |= conflict.conflict_kinds.contains(ConflictKinds::READ_WRITE);
        // left writes, right reads: the later/right transaction has a RAW dependency.
        entry.raw |= conflict.conflict_kinds.contains(ConflictKinds::WRITE_READ);
    }
    by_transaction
        .into_iter()
        .filter_map(|(transaction, kinds)| {
            (kinds.waw || (kinds.war && kinds.raw)).then_some(transaction)
        })
        .collect()
}

type AccessFootprint = BTreeSet<(Address, u8, Vec<u8>, Option<Vec<u8>>, bool)>;

fn prepared_footprints(
    prepared: &PreparedSpeculativeBlock,
) -> BTreeMap<TransactionId, AccessFootprint> {
    prepared
        .receipts
        .iter()
        .map(|receipt| (receipt.transaction_id, access_footprint(receipt)))
        .collect()
}

fn access_footprint(receipt: &SpeculativeTxResult) -> AccessFootprint {
    receipt
        .accesses
        .iter()
        .map(|access| {
            (
                access.contract.clone(),
                access_kind_code(&access.kind),
                access.key.clone(),
                access.range_end.clone(),
                access.reverted,
            )
        })
        .collect()
}

fn access_kind_code(kind: &AccessKind) -> u8 {
    match kind {
        AccessKind::StorageRead => 0,
        AccessKind::StorageScan => 1,
        AccessKind::StorageWrite => 2,
        AccessKind::StorageRemove => 3,
        AccessKind::BankRead => 4,
        AccessKind::BankWrite => 5,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use acg_core::TxIndex;

    fn conflict(left: u32, right: u32, kinds: ConflictKinds) -> ObservedConflict {
        ObservedConflict {
            left: TxIndex(left),
            right: TxIndex(right),
            conflict_kinds: kinds,
        }
    }

    #[test]
    fn rule2_forward_dependency_matches_ariafb_conditions() {
        let conflicts = vec![
            conflict(0, 1, ConflictKinds::WRITE_READ),
            conflict(0, 2, ConflictKinds::READ_WRITE),
            conflict(1, 2, ConflictKinds::WRITE_READ),
            conflict(0, 3, ConflictKinds::WRITE_WRITE),
        ];
        let fallback = aria_rule2_forward_fallbacks(&conflicts);
        assert!(!fallback.contains(&1));
        assert!(fallback.contains(&2));
        assert!(fallback.contains(&3));
    }

    #[test]
    fn concrete_dependency_plan_removes_transitive_edges() {
        let plan = dependency_plan_from_pairs(3, BTreeSet::from([(0, 1), (0, 2), (1, 2)])).unwrap();
        assert_eq!(
            plan.dependencies
                .iter()
                .map(|dependency| (dependency.predecessor_index, dependency.successor_index))
                .collect::<Vec<_>>(),
            vec![(0, 1), (1, 2)]
        );
    }

    #[test]
    fn concrete_dependency_plan_groups_independent_transactions() {
        let plan = dependency_plan_from_pairs(5, BTreeSet::from([(0, 2), (2, 4), (1, 3)])).unwrap();
        assert_eq!(plan.waves[0].transaction_indices, vec![0, 1]);
        assert_eq!(plan.waves[1].transaction_indices, vec![2, 3]);
        assert_eq!(plan.waves[2].transaction_indices, vec![4]);
        assert_eq!(plan.dependencies.len(), 3);
        plan.validate().unwrap();
    }
}
