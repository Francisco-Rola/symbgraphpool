//! Manifest-driven common benchmark harness for the Adaptive Conflict Graph runtime.
//!
//! One [`RunIdentity`](acg_evaluation::RunIdentity) corresponds to one measured block. Workload
//! adapters may create deterministic warm-up blocks before it. The harness independently prepares
//! a canonical serial reference and the requested speculative mode, verifies identical initial
//! state/workload generation, executes both, emits one stable Phase-5E [`ExperimentRecord`], and
//! finally evaluates the complete dataset through Phase 5F.
//!
//! The harness is performance/evaluation infrastructure only. It never changes canonical validity
//! or commit semantics.

mod baselines;
mod conflictlab;
mod vegeta_eth;

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::{Duration, Instant},
};

use acg_candidate_graph::{CostAwareEdgePolicyConfig, RiskBoundedSchedulerConfig};
use acg_core::RuntimeId;
use acg_cosmwasm_adapter::{CosmWasmAdapterConfig, CosmWasmCandidateAdapter};
use acg_cosmwasm_engine::{ContractExecutionDiagnostics, CosmWasmEngine, ParallelExecutionConfig};
use acg_evaluation::{
    AcceptanceError, AdaptiveStateRecord, ConsensusExecutionRecord, CorrectnessRecord,
    ExecutionRecord, ExperimentAcceptanceReport, ExperimentManifest, ExperimentMetadata,
    ExperimentRecord, ExperimentRecordError, FeedbackRecord, FeedbackTimingRecord,
    ParallelismRecord, ParallelismReference, PipelineTimingRecord, PlanningRecord, RunIdentity,
    SchedulingRecord, EXPERIMENT_RECORD_SCHEMA_VERSION,
};
use acg_feedback::{AdaptiveFeedbackConfig, ApplySummary};
use acg_profile_graph::ProfileGraph;
use acg_runtime_feedback::{
    AccessConflictDetector, AdaptiveBlockPlan, AdaptivePlanningConfig, AdaptiveSerialPipeline,
    BlockEconomicsObservation, ObservedConflict, RegimeChangeConfig, RuntimeFeedbackEngine,
    RuntimeFeedbackWeights, SerialBypassConfig, TraceConflictConfig,
};
use acg_validator_sim::{
    BlockExecutionReport, ExecutionPlan, ExecutionWave, ProducedBlock, SerialBlockExecutor,
    SpeculativeParallelBlockExecutor,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use baselines::ExecutionStrategy;
use baselines::{BaselineExecutionContext, BaselineMeasuredExecution};
pub use conflictlab::ConflictLabWorkload;
pub use vegeta_eth::VegetaEthWorkload;

pub const BENCHMARK_HARNESS_SCHEMA_VERSION: u16 = 3;

/// Built-in speculative policy ablations supported by the common harness.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HarnessMode {
    /// Static symbolic/prior graph. Concrete execution is validated/replayed but no feedback is
    /// retained for later blocks.
    Static,
    /// Phase-3/4 conflict-probability learning only. Replay/serialization cost observations are
    /// intentionally excluded.
    ProbabilityOnly,
    /// Full Phase-5D/5E conflict probability + replay cost/fan-out + learned serialization cost.
    CostAware,
}

impl HarnessMode {
    pub fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "static" | "static-prior" => Ok(Self::Static),
            "probability" | "probability-only" => Ok(Self::ProbabilityOnly),
            "cost-aware" | "adaptive" => Ok(Self::CostAware),
            other => Err(HarnessError::UnsupportedMode(other.to_owned())),
        }
    }
}

/// A fully deterministic prepared workload instance.
///
/// Workload setup is performed twice for every run: once for the serial reference and once for the
/// speculative mode. The harness checks that the initial state encoding and all generated blocks
/// are exactly identical before either side is measured.
pub trait PreparedBenchmark: Send {
    fn engine(&self) -> &CosmWasmEngine;
    fn profile_graph(&self) -> &ProfileGraph;
    fn warmup_blocks(&self) -> &[ProducedBlock];
    /// Consensus-decided warmup blocks. Defaults to the predicted blocks for workloads without
    /// candidate/decision divergence.
    fn warmup_decided_blocks(&self) -> &[ProducedBlock] {
        self.warmup_blocks()
    }
    fn measured_block(&self) -> &ProducedBlock;
    /// Consensus-decided measured block. Defaults to the predicted block.
    fn measured_decided_block(&self) -> &ProducedBlock {
        self.measured_block()
    }
    fn canonical_state_bytes(&self) -> Result<Vec<u8>, HarnessError>;

    /// Optional human-readable state snapshot used only by opt-in correctness diagnostics.
    /// Publication records continue to use the deterministic canonical-state digest above.
    fn canonical_state_diagnostics(&self) -> Result<Option<serde_json::Value>, HarnessError> {
        Ok(None)
    }

    /// Extra workload-specific environment metadata copied into the stable experiment record.
    fn environment_metadata(&self) -> BTreeMap<String, String> {
        BTreeMap::new()
    }
}

/// Workload adapter boundary used by the common runner.
pub trait BenchmarkWorkload: Send + Sync {
    fn name(&self) -> &'static str;
    fn prepare(&self, run: &RunIdentity) -> Result<Box<dyn PreparedBenchmark>, HarnessError>;
}

#[derive(Default)]
pub struct WorkloadRegistry {
    workloads: BTreeMap<String, Arc<dyn BenchmarkWorkload>>,
}

impl WorkloadRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_builtin_workloads() -> Self {
        let mut registry = Self::new();
        registry.register(ConflictLabWorkload);
        registry.register(VegetaEthWorkload);
        registry
    }

    pub fn register<W>(&mut self, workload: W)
    where
        W: BenchmarkWorkload + 'static,
    {
        self.workloads
            .insert(workload.name().to_owned(), Arc::new(workload));
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn BenchmarkWorkload>> {
        self.workloads.get(name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.workloads.keys().map(String::as_str)
    }
}

pub struct BenchmarkHarness {
    registry: WorkloadRegistry,
    repo_root: PathBuf,
}

impl BenchmarkHarness {
    pub fn new(registry: WorkloadRegistry, repo_root: impl Into<PathBuf>) -> Self {
        Self {
            registry,
            repo_root: repo_root.into(),
        }
    }

    pub fn with_builtin_workloads(repo_root: impl Into<PathBuf>) -> Self {
        Self::new(WorkloadRegistry::with_builtin_workloads(), repo_root)
    }

    pub fn registry(&self) -> &WorkloadRegistry {
        &self.registry
    }

    pub fn run_manifest(
        &self,
        manifest: &ExperimentManifest,
    ) -> Result<HarnessOutcome, HarnessError> {
        manifest.validate()?;
        let mut records = Vec::with_capacity(manifest.runs.len());
        for run in &manifest.runs {
            records.push(self.run_one(manifest, run)?);
        }
        let acceptance = manifest.evaluate(&records);
        Ok(HarnessOutcome {
            records,
            acceptance,
        })
    }

    pub fn run_manifest_to_files(
        &self,
        manifest: &ExperimentManifest,
        records_path: impl AsRef<Path>,
        acceptance_path: impl AsRef<Path>,
    ) -> Result<HarnessOutcome, HarnessError> {
        let outcome = self.run_manifest(manifest)?;
        let records_path = records_path.as_ref();
        if let Some(parent) = records_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let acceptance_path = acceptance_path.as_ref();
        if let Some(parent) = acceptance_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let mut jsonl = Vec::new();
        for record in &outcome.records {
            jsonl.extend(record.to_json_line()?);
        }
        fs::write(records_path, jsonl)?;
        fs::write(acceptance_path, outcome.acceptance.to_pretty_json()?)?;
        Ok(outcome)
    }

    pub fn run_one(
        &self,
        manifest: &ExperimentManifest,
        run: &RunIdentity,
    ) -> Result<ExperimentRecord, HarnessError> {
        if run.workers == 0 || run.workers > manifest.physical_core_limit {
            return Err(HarnessError::WorkerBudget {
                workers: run.workers,
                physical_cores: manifest.physical_core_limit,
            });
        }
        let workload = self
            .registry
            .get(&run.workload)
            .ok_or_else(|| HarnessError::UnknownWorkload(run.workload.clone()))?;
        let strategy = ExecutionStrategy::parse(&run.mode)?;
        let tuning = HarnessTuningConfig::from_parameters(&run.parameters)?;

        let serial = workload.prepare(run)?;
        let adaptive = workload.prepare(run)?;
        ensure_deterministic_preparation(serial.as_ref(), adaptive.as_ref())?;

        let serial_reference =
            run_serial_reference(serial.as_ref(), strategy == ExecutionStrategy::ExactAccess)?;
        let serial_state = serial.canonical_state_bytes()?;

        if strategy.is_baseline() {
            return run_baseline_strategy(BaselineRunContext {
                manifest,
                run,
                repo_root: &self.repo_root,
                workload_name: workload.name(),
                serial: serial.as_ref(),
                adaptive: adaptive.as_ref(),
                serial_reference: &serial_reference,
                serial_state: &serial_state,
                strategy,
                tuning,
            });
        }
        let mode = strategy
            .adaptive_mode()
            .expect("non-baseline execution strategy must map to a SymbGraph mode");

        let graph = adaptive.profile_graph();
        let engine = adaptive.engine();
        let feedback = RuntimeFeedbackEngine::new(
            graph,
            0,
            tuning.trace_config,
            RuntimeFeedbackWeights::default(),
            tuning.feedback_config,
        )
        .map_err(display_error)?;
        let adapter = CosmWasmCandidateAdapter::new(
            CosmWasmAdapterConfig::new(RuntimeId::new("cosmwasm").map_err(display_error)?, 1)
                .map_err(display_error)?,
        );
        let mut pipeline = AdaptiveSerialPipeline::new(adapter, feedback, tuning.planning_config)
            .map_err(display_error)?;
        let measured_compact_equivalence_groups =
            pipeline.planning_config().compact_equivalence_groups;
        if let Some(warmup_compact_equivalence_groups) = tuning.warmup_compact_equivalence_groups {
            pipeline.set_compact_equivalence_groups(warmup_compact_equivalence_groups);
        }
        let measured_workers =
            usize::try_from(run.workers).map_err(|_| HarnessError::NumericOverflow)?;
        let executor = SpeculativeParallelBlockExecutor::new(
            engine.clone(),
            ParallelExecutionConfig {
                workers: measured_workers,
            },
        );
        let warmup_executor = if let Some(warmup_workers) = tuning.warmup_workers {
            let physical_cores = usize::try_from(manifest.physical_core_limit)
                .map_err(|_| HarnessError::NumericOverflow)?;
            if warmup_workers > physical_cores {
                return Err(HarnessError::WorkloadParameter(format!(
                    "acg.warmup_workers={warmup_workers} exceeds physical_core_limit={physical_cores}"
                )));
            }
            Some(SpeculativeParallelBlockExecutor::new(
                engine.clone(),
                ParallelExecutionConfig {
                    workers: warmup_workers,
                },
            ))
        } else {
            None
        };
        let warmup_executor_ref = warmup_executor.as_ref().unwrap_or(&executor);

        if adaptive.warmup_blocks().len() != adaptive.warmup_decided_blocks().len() {
            return Err(HarnessError::NonDeterministicTransactions);
        }
        for (predicted, decided) in adaptive
            .warmup_blocks()
            .iter()
            .zip(adaptive.warmup_decided_blocks())
        {
            execute_adaptive_block(
                &mut pipeline,
                AdaptiveExecutionContext {
                    mode,
                    graph,
                    parallel_executor: warmup_executor_ref,
                    predicted_block: predicted,
                    decided_block: decided,
                    consensus_cutoff: tuning.consensus_cutoff,
                    measured: false,
                },
            )?;
        }

        if tuning.warmup_compact_equivalence_groups.is_some() {
            pipeline.set_compact_equivalence_groups(measured_compact_equivalence_groups);
        }

        let measured = execute_adaptive_block(
            &mut pipeline,
            AdaptiveExecutionContext {
                mode,
                graph,
                parallel_executor: &executor,
                predicted_block: adaptive.measured_block(),
                decided_block: adaptive.measured_decided_block(),
                consensus_cutoff: tuning.consensus_cutoff,
                measured: true,
            },
        )?
        .ok_or(HarnessError::MissingMeasuredArtifacts)?;
        let adaptive_state = adaptive.canonical_state_bytes()?;
        let correctness = CorrectnessRecord::from_state_bytes(&adaptive_state, &serial_state);
        if correctness.serial_equivalent == Some(false) {
            maybe_write_correctness_diagnostics(
                manifest,
                run,
                serial.as_ref(),
                adaptive.as_ref(),
                &serial_state,
                &adaptive_state,
                &correctness,
            )?;
        }

        let serial_services = serial_services(&serial_reference.report)?;
        let serial_equivalent_work_nanos = nanos(serial_reference.wall);
        let concrete_conflicts = AccessConflictDetector::new(tuning.trace_config)
            .detect(&serial_reference.report)
            .map_err(display_error)?;
        let perfect_conflict_dag_bound_nanos =
            concrete_conflict_dag_bound(&serial_services, &concrete_conflicts)?;
        let worker_count = u64::from(run.workers).max(1);
        let perfect_worker_capacity_bound_nanos = ceil_div_u64(
            serial_services
                .iter()
                .copied()
                .fold(0_u64, u64::saturating_add),
            worker_count,
        );
        let perfect_conflict_parallel_lower_bound_nanos =
            perfect_conflict_dag_bound_nanos.max(perfect_worker_capacity_bound_nanos);
        let same_candidate_and_decision = adaptive.measured_block().transactions
            == adaptive.measured_decided_block().transactions;
        let serial_cost_dag_bound_nanos = if same_candidate_and_decision {
            if serial_services.len() != measured.plan.candidate_graph.transactions().len() {
                return Err(HarnessError::SerialReferenceShape {
                    serial_transactions: serial_services.len(),
                    measured_transactions: measured.plan.candidate_graph.transactions().len(),
                });
            }
            Some(dag_bound_from_services(&measured.plan, &serial_services))
        } else {
            // A candidate-DAG critical-path projection is not meaningful once consensus changes
            // transaction identity or order. Keep the real decided-block serial wall, but omit the
            // counterfactual DAG-bound diagnostic instead of pairing decided service times with
            // unrelated candidate indices.
            None
        };
        let parallelism_reference = ParallelismReference {
            serial_equivalent_work_nanos: Some(serial_equivalent_work_nanos),
            serial_cost_dag_bound_nanos,
            perfect_conflict_dag_bound_nanos: Some(perfect_conflict_dag_bound_nanos),
            perfect_conflict_parallel_lower_bound_nanos: Some(
                perfect_conflict_parallel_lower_bound_nanos,
            ),
        };

        let mut metadata = ExperimentMetadata {
            experiment_id: manifest.experiment_id.clone(),
            workload: run.workload.clone(),
            mode: run.mode.clone(),
            run_index: run.run_index,
            seed: run.seed,
            workers: run.workers,
            physical_cores: manifest.physical_core_limit,
            parameters: run.parameters.clone(),
            ..ExperimentMetadata::default()
        }
        .capture_standard_environment(&self.repo_root);
        metadata.environment.insert(
            "benchmark_harness_schema".to_owned(),
            BENCHMARK_HARNESS_SCHEMA_VERSION.to_string(),
        );
        metadata
            .environment
            .insert("benchmark_adapter".to_owned(), workload.name().to_owned());
        metadata.environment.extend(adaptive.environment_metadata());

        let pipeline_timing = measured
            .pipeline_timing
            .with_serial_reference(serial_reference.wall);
        let consensus = measured
            .consensus
            .with_serial_reference(serial_reference.wall);
        let adaptive_state = adaptive_state_record(
            &pipeline,
            graph,
            adaptive.measured_decided_block().context.height,
            &tuning.feedback_config,
        )?;
        let record = match &measured.execution {
            MeasuredExecution::Speculative(execution) => ExperimentRecord::from_runtime(
                metadata,
                measured.planning_metrics,
                pipeline.planning_config(),
                &measured.plan,
                &execution.preexecution_report,
                &execution.preexecution_metrics,
                &execution.reconciliation,
                parallelism_reference,
                measured.feedback_summary,
                measured.feedback_timing,
                adaptive_state,
                pipeline_timing,
                consensus,
                correctness,
            ),
            MeasuredExecution::SerialBypass(execution) => ExperimentRecord::from_serial_bypass(
                metadata,
                measured.planning_metrics,
                pipeline.planning_config(),
                &measured.plan,
                &execution.report,
                execution.wall,
                &execution.contract_diagnostics,
                parallelism_reference,
                adaptive_state,
                pipeline_timing,
                consensus,
                correctness,
            ),
        };
        Ok(record)
    }
}

struct BaselineRunContext<'a> {
    manifest: &'a ExperimentManifest,
    run: &'a RunIdentity,
    repo_root: &'a Path,
    workload_name: &'static str,
    serial: &'a dyn PreparedBenchmark,
    adaptive: &'a dyn PreparedBenchmark,
    serial_reference: &'a SerialReference,
    serial_state: &'a [u8],
    strategy: ExecutionStrategy,
    tuning: HarnessTuningConfig,
}

fn run_baseline_strategy(
    context: BaselineRunContext<'_>,
) -> Result<ExperimentRecord, HarnessError> {
    let BaselineRunContext {
        manifest,
        run,
        repo_root,
        workload_name,
        serial,
        adaptive,
        serial_reference,
        serial_state,
        strategy,
        tuning,
    } = context;
    if adaptive.warmup_blocks().len() != adaptive.warmup_decided_blocks().len() {
        return Err(HarnessError::NonDeterministicTransactions);
    }
    if strategy == ExecutionStrategy::ExactAccess
        && adaptive.warmup_blocks().len() != serial_reference.warmup_reports.len()
    {
        return Err(HarnessError::NonDeterministicTransactions);
    }
    let workers = usize::try_from(run.workers).map_err(|_| HarnessError::NumericOverflow)?;
    for (warmup_index, (predicted, decided)) in adaptive
        .warmup_blocks()
        .iter()
        .zip(adaptive.warmup_decided_blocks())
        .enumerate()
    {
        let oracle_report = if strategy == ExecutionStrategy::ExactAccess {
            serial_reference.warmup_reports.get(warmup_index)
        } else {
            None
        };
        baselines::execute_baseline_block(BaselineExecutionContext {
            strategy,
            engine: adaptive.engine(),
            workers,
            predicted_block: predicted,
            decided_block: decided,
            oracle_report,
            trace_config: tuning.trace_config,
            consensus_cutoff: tuning.consensus_cutoff,
            measured: false,
        })?;
    }

    let measured = baselines::execute_baseline_block(BaselineExecutionContext {
        strategy,
        engine: adaptive.engine(),
        workers,
        predicted_block: adaptive.measured_block(),
        decided_block: adaptive.measured_decided_block(),
        oracle_report: Some(&serial_reference.report),
        trace_config: tuning.trace_config,
        consensus_cutoff: tuning.consensus_cutoff,
        measured: true,
    })?
    .ok_or(HarnessError::MissingMeasuredArtifacts)?;

    let adaptive_state_bytes = adaptive.canonical_state_bytes()?;
    let correctness = CorrectnessRecord::from_state_bytes(&adaptive_state_bytes, serial_state);
    if correctness.serial_equivalent == Some(false) {
        maybe_write_correctness_diagnostics(
            manifest,
            run,
            serial,
            adaptive,
            serial_state,
            &adaptive_state_bytes,
            &correctness,
        )?;
    }

    let serial_services = serial_services(&serial_reference.report)?;
    let serial_equivalent_work_nanos = nanos(serial_reference.wall);
    let concrete_conflicts = AccessConflictDetector::new(tuning.trace_config)
        .detect(&serial_reference.report)
        .map_err(display_error)?;
    let perfect_conflict_dag_bound_nanos =
        concrete_conflict_dag_bound(&serial_services, &concrete_conflicts)?;
    let worker_count = u64::from(run.workers).max(1);
    let perfect_worker_capacity_bound_nanos = ceil_div_u64(
        serial_services
            .iter()
            .copied()
            .fold(0_u64, u64::saturating_add),
        worker_count,
    );
    let perfect_conflict_parallel_lower_bound_nanos =
        perfect_conflict_dag_bound_nanos.max(perfect_worker_capacity_bound_nanos);
    let serial_cost_dag_bound_nanos = Some(execution_plan_dag_bound(
        &measured.parallelism_plan,
        &serial_services,
    ));
    let parallelism_reference = ParallelismReference {
        serial_equivalent_work_nanos: Some(serial_equivalent_work_nanos),
        serial_cost_dag_bound_nanos,
        perfect_conflict_dag_bound_nanos: Some(perfect_conflict_dag_bound_nanos),
        perfect_conflict_parallel_lower_bound_nanos: Some(
            perfect_conflict_parallel_lower_bound_nanos,
        ),
    };

    let mut metadata = ExperimentMetadata {
        experiment_id: manifest.experiment_id.clone(),
        workload: run.workload.clone(),
        mode: run.mode.clone(),
        run_index: run.run_index,
        seed: run.seed,
        workers: run.workers,
        physical_cores: manifest.physical_core_limit,
        parameters: run.parameters.clone(),
        ..ExperimentMetadata::default()
    }
    .capture_standard_environment(repo_root);
    metadata.environment.insert(
        "benchmark_harness_schema".to_owned(),
        BENCHMARK_HARNESS_SCHEMA_VERSION.to_string(),
    );
    metadata
        .environment
        .insert("benchmark_adapter".to_owned(), workload_name.to_owned());
    metadata.environment.insert(
        "execution_strategy".to_owned(),
        measured.strategy.family.clone(),
    );
    metadata.environment.insert(
        "baseline_semantics".to_owned(),
        "canonical-decided-order".to_owned(),
    );
    metadata.environment.extend(adaptive.environment_metadata());

    let planning = PlanningRecord {
        scheduler_nanos: nanos(measured.planning_wall),
        total_nanos: nanos(measured.planning_wall),
        ..PlanningRecord::default()
    };
    let scheduling = SchedulingRecord::from_execution_plan(
        &measured.scheduling_plan,
        measured.concrete_relationships,
    );
    let pipeline_timing = PipelineTimingRecord::from_durations(
        measured.planning_wall,
        measured.preexecution_wall,
        Duration::ZERO,
        measured.reconciliation_wall,
        Duration::ZERO,
        measured.total_wall,
    )
    .with_serial_reference(serial_reference.wall);
    let consensus = measured
        .consensus
        .with_serial_reference(serial_reference.wall);

    let (parallelism, mut execution) = match &measured.execution {
        BaselineMeasuredExecution::Serial(execution) => (
            ParallelismRecord::from_serial_execution(
                &execution.report,
                execution.wall,
                parallelism_reference,
            ),
            ExecutionRecord::from_serial_execution(
                &execution.report,
                execution.wall,
                &execution.diagnostics,
            ),
        ),
        BaselineMeasuredExecution::Speculative(execution) => (
            ParallelismRecord::from_execution_plan(
                &measured.parallelism_plan,
                &execution.preexecution_report,
                &execution.preexecution_metrics,
                parallelism_reference,
            ),
            ExecutionRecord::from_speculative_reports(
                &execution.preexecution_metrics,
                &execution.reconciliation,
            ),
        ),
    };
    // The generic speculative record only sees the final reconciliation object. For order-execute
    // and speculate-order-replay baselines, include any parallel replay/batch work in the actual
    // post-consensus critical path reported by the strategy runner.
    execution.post_consensus_total_nanos = consensus.post_consensus_nanos;

    Ok(ExperimentRecord {
        schema_version: EXPERIMENT_RECORD_SCHEMA_VERSION,
        metadata,
        planning,
        scheduling,
        parallelism,
        execution,
        feedback: FeedbackRecord::default(),
        feedback_timing: FeedbackTimingRecord::default(),
        adaptive_state: AdaptiveStateRecord::default(),
        pipeline_timing,
        consensus,
        strategy: Some(measured.strategy),
        correctness,
    })
}

pub struct HarnessOutcome {
    pub records: Vec<ExperimentRecord>,
    pub acceptance: ExperimentAcceptanceReport,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HarnessTuningConfig {
    pub planning_config: AdaptivePlanningConfig,
    /// Optional paired-reference override used only while executing warm-up blocks.
    ///
    /// Normal runs leave this unset. The compaction semantic-reference campaign sets it to
    /// `Some(false)` so compact and dense measured blocks start from an identical dense-trained
    /// posterior and canonical state.
    pub warmup_compact_equivalence_groups: Option<bool>,
    /// Optional worker-count override used only for warm-up blocks.
    ///
    /// Paired semantic-reference campaigns use a single warm-up worker so independently executed
    /// runs follow the same speculative/reconciliation trajectory before the measured toggle.
    /// The measured block still uses `run.workers`.
    pub warmup_workers: Option<usize>,
    pub feedback_config: AdaptiveFeedbackConfig,
    pub trace_config: TraceConflictConfig,
    pub consensus_cutoff: Duration,
}

impl HarnessTuningConfig {
    pub fn from_parameters(parameters: &BTreeMap<String, String>) -> Result<Self, HarnessError> {
        const ACG_KEYS: &[&str] = &[
            "acg.edge_materialization_threshold",
            "acg.compact_equivalence_groups",
            "acg.warmup_compact_equivalence_groups",
            "acg.warmup_workers",
            "acg.soft_threshold",
            "acg.hard_threshold",
            "acg.risk_budget",
            "acg.max_wave_width",
            "acg.exploration_rate",
            "acg.exploration_risk_budget",
            "acg.exploration_min_uncertainty",
            "acg.exploration_max_transactions_per_block",
            "acg.independent_observations_before_softening",
            "acg.softening_min_confidence",
            "acg.serial_bypass_enabled",
            "acg.serial_bypass_min_transactions",
            "acg.serial_bypass_min_projected_speedup",
            "acg.serial_bypass_service_cost_reference_nanos_per_transaction",
            "acg.serial_bypass_economics_ema_alpha",
            "acg.serial_bypass_min_economics_observations",
            "acg.serial_bypass_projected_speedup_hysteresis",
            "acg.serial_bypass_max_consecutive_bypasses",
            "acg.serial_bypass_immediate_speedup_floor",
            "acg.serial_bypass_buffered_preexecution",
            "acg.regime_change_enabled",
            "acg.regime_service_cost_drop_ratio",
            "acg.regime_contention_increase_ratio",
            "acg.regime_contention_increase_absolute",
            "acg.regime_retained_evidence",
            "acg.regime_probation_bypass_blocks",
            "acg.regime_probation_min_projected_speedup",
            "acg.serialization_cost_reference_nanos",
            "acg.invalidation_fanout_weight",
            "acg.pre_consensus_serialization_weight",
            "acg.post_consensus_replay_weight",
            "acg.feedback_retention_factor",
            "acg.feedback_confidence_scale",
            "acg.fallback_prior_probability",
            "acg.fallback_prior_strength",
            "acg.feedback_epsilon",
            "acg.candidate_miss_verification_weight_threshold",
            "acg.include_reverted_accesses",
        ];
        for key in parameters.keys().filter(|key| key.starts_with("acg.")) {
            if !ACG_KEYS.contains(&key.as_str()) {
                return Err(HarnessError::WorkloadParameter(format!(
                    "unknown common harness parameter {key:?}"
                )));
            }
        }

        let planning_default = AdaptivePlanningConfig::default();
        let planning = AdaptivePlanningConfig {
            edge_materialization_threshold: parameter(
                parameters,
                "acg.edge_materialization_threshold",
                planning_default.edge_materialization_threshold,
            )?,
            compact_equivalence_groups: parameter(
                parameters,
                "acg.compact_equivalence_groups",
                planning_default.compact_equivalence_groups,
            )?,
            scheduler: RiskBoundedSchedulerConfig {
                soft_threshold: parameter(
                    parameters,
                    "acg.soft_threshold",
                    planning_default.scheduler.soft_threshold,
                )?,
                hard_threshold: parameter(
                    parameters,
                    "acg.hard_threshold",
                    planning_default.scheduler.hard_threshold,
                )?,
                risk_budget: parameter(
                    parameters,
                    "acg.risk_budget",
                    planning_default.scheduler.risk_budget,
                )?,
                max_wave_width: optional_usize_parameter(
                    parameters,
                    "acg.max_wave_width",
                    planning_default.scheduler.max_wave_width,
                )?,
                exploration_rate: parameter(
                    parameters,
                    "acg.exploration_rate",
                    planning_default.scheduler.exploration_rate,
                )?,
                exploration_risk_budget: parameter(
                    parameters,
                    "acg.exploration_risk_budget",
                    planning_default.scheduler.exploration_risk_budget,
                )?,
                exploration_min_uncertainty: parameter(
                    parameters,
                    "acg.exploration_min_uncertainty",
                    planning_default.scheduler.exploration_min_uncertainty,
                )?,
                exploration_max_transactions_per_block: parameter(
                    parameters,
                    "acg.exploration_max_transactions_per_block",
                    planning_default
                        .scheduler
                        .exploration_max_transactions_per_block,
                )?,
                independent_observations_before_softening: parameter(
                    parameters,
                    "acg.independent_observations_before_softening",
                    planning_default
                        .scheduler
                        .independent_observations_before_softening,
                )?,
                softening_min_confidence: parameter(
                    parameters,
                    "acg.softening_min_confidence",
                    planning_default.scheduler.softening_min_confidence,
                )?,
            },
            cost_policy: CostAwareEdgePolicyConfig {
                serialization_cost_reference_nanos: parameter(
                    parameters,
                    "acg.serialization_cost_reference_nanos",
                    planning_default
                        .cost_policy
                        .serialization_cost_reference_nanos,
                )?,
                invalidation_fanout_weight: parameter(
                    parameters,
                    "acg.invalidation_fanout_weight",
                    planning_default.cost_policy.invalidation_fanout_weight,
                )?,
                pre_consensus_serialization_weight: parameter(
                    parameters,
                    "acg.pre_consensus_serialization_weight",
                    planning_default
                        .cost_policy
                        .pre_consensus_serialization_weight,
                )?,
                post_consensus_replay_weight: parameter(
                    parameters,
                    "acg.post_consensus_replay_weight",
                    planning_default.cost_policy.post_consensus_replay_weight,
                )?,
            },
            serial_bypass: SerialBypassConfig {
                enabled: parameter(
                    parameters,
                    "acg.serial_bypass_enabled",
                    planning_default.serial_bypass.enabled,
                )?,
                min_transactions: parameter(
                    parameters,
                    "acg.serial_bypass_min_transactions",
                    planning_default.serial_bypass.min_transactions,
                )?,
                min_projected_speedup: parameter(
                    parameters,
                    "acg.serial_bypass_min_projected_speedup",
                    planning_default.serial_bypass.min_projected_speedup,
                )?,
                service_cost_reference_nanos_per_transaction: parameter(
                    parameters,
                    "acg.serial_bypass_service_cost_reference_nanos_per_transaction",
                    planning_default
                        .serial_bypass
                        .service_cost_reference_nanos_per_transaction,
                )?,
                economics_ema_alpha: parameter(
                    parameters,
                    "acg.serial_bypass_economics_ema_alpha",
                    planning_default.serial_bypass.economics_ema_alpha,
                )?,
                min_economics_observations: parameter(
                    parameters,
                    "acg.serial_bypass_min_economics_observations",
                    planning_default.serial_bypass.min_economics_observations,
                )?,
                projected_speedup_hysteresis: parameter(
                    parameters,
                    "acg.serial_bypass_projected_speedup_hysteresis",
                    planning_default.serial_bypass.projected_speedup_hysteresis,
                )?,
                max_consecutive_bypasses: parameter(
                    parameters,
                    "acg.serial_bypass_max_consecutive_bypasses",
                    planning_default.serial_bypass.max_consecutive_bypasses,
                )?,
                immediate_speedup_floor: parameter(
                    parameters,
                    "acg.serial_bypass_immediate_speedup_floor",
                    planning_default.serial_bypass.immediate_speedup_floor,
                )?,
                buffered_preexecution: parameter(
                    parameters,
                    "acg.serial_bypass_buffered_preexecution",
                    planning_default.serial_bypass.buffered_preexecution,
                )?,
            },
            regime_change: RegimeChangeConfig {
                enabled: parameter(
                    parameters,
                    "acg.regime_change_enabled",
                    planning_default.regime_change.enabled,
                )?,
                service_cost_drop_ratio: parameter(
                    parameters,
                    "acg.regime_service_cost_drop_ratio",
                    planning_default.regime_change.service_cost_drop_ratio,
                )?,
                contention_increase_ratio: parameter(
                    parameters,
                    "acg.regime_contention_increase_ratio",
                    planning_default.regime_change.contention_increase_ratio,
                )?,
                contention_increase_absolute: parameter(
                    parameters,
                    "acg.regime_contention_increase_absolute",
                    planning_default.regime_change.contention_increase_absolute,
                )?,
                retained_evidence: parameter(
                    parameters,
                    "acg.regime_retained_evidence",
                    planning_default.regime_change.retained_evidence,
                )?,
                probation_bypass_blocks: parameter(
                    parameters,
                    "acg.regime_probation_bypass_blocks",
                    planning_default.regime_change.probation_bypass_blocks,
                )?,
                probation_min_projected_speedup: parameter(
                    parameters,
                    "acg.regime_probation_min_projected_speedup",
                    planning_default
                        .regime_change
                        .probation_min_projected_speedup,
                )?,
            },
        };

        let feedback_default = AdaptiveFeedbackConfig::default();
        let feedback = AdaptiveFeedbackConfig {
            retention_factor: parameter(
                parameters,
                "acg.feedback_retention_factor",
                feedback_default.retention_factor,
            )?,
            confidence_scale: parameter(
                parameters,
                "acg.feedback_confidence_scale",
                feedback_default.confidence_scale,
            )?,
            fallback_prior_probability: parameter(
                parameters,
                "acg.fallback_prior_probability",
                feedback_default.fallback_prior_probability,
            )?,
            fallback_prior_strength: parameter(
                parameters,
                "acg.fallback_prior_strength",
                feedback_default.fallback_prior_strength,
            )?,
            epsilon: parameter(parameters, "acg.feedback_epsilon", feedback_default.epsilon)?,
            candidate_miss_verification_weight_threshold: parameter(
                parameters,
                "acg.candidate_miss_verification_weight_threshold",
                feedback_default.candidate_miss_verification_weight_threshold,
            )?,
        };
        let trace = TraceConflictConfig {
            include_reverted_accesses: parameter(
                parameters,
                "acg.include_reverted_accesses",
                TraceConflictConfig::default().include_reverted_accesses,
            )?,
        };

        let consensus_cutoff_ms = parameter(parameters, "consensus_cutoff_ms", 500_u64)?;
        if consensus_cutoff_ms == 0 {
            return Err(HarnessError::WorkloadParameter(
                "consensus_cutoff_ms must be greater than zero".to_owned(),
            ));
        }

        let warmup_compact_equivalence_groups =
            optional_parameter(parameters, "acg.warmup_compact_equivalence_groups")?;
        let warmup_workers = optional_parameter::<usize>(parameters, "acg.warmup_workers")?;
        if warmup_workers == Some(0) {
            return Err(HarnessError::WorkloadParameter(
                "acg.warmup_workers must be greater than zero when configured".to_owned(),
            ));
        }

        planning.validate().map_err(display_error)?;
        feedback.validate().map_err(display_error)?;
        Ok(Self {
            planning_config: planning,
            warmup_compact_equivalence_groups,
            warmup_workers,
            feedback_config: feedback,
            trace_config: trace,
            consensus_cutoff: Duration::from_millis(consensus_cutoff_ms),
        })
    }
}

struct MeasuredSplitExecution {
    preexecution_report: BlockExecutionReport,
    preexecution_metrics: acg_cosmwasm_engine::ParallelSpeculativeExecutionMetrics,
    reconciliation: acg_validator_sim::SplitPhaseSpeculativeExecutionReport,
}

struct MeasuredSerialBypassExecution {
    report: BlockExecutionReport,
    wall: Duration,
    contract_diagnostics: ContractExecutionDiagnostics,
}

enum MeasuredExecution {
    Speculative(Box<MeasuredSplitExecution>),
    SerialBypass(Box<MeasuredSerialBypassExecution>),
}

struct MeasuredAdaptiveBlock {
    plan: AdaptiveBlockPlan,
    planning_metrics: acg_runtime_feedback::AdaptivePlanningMetrics,
    execution: MeasuredExecution,
    feedback_summary: ApplySummary,
    feedback_timing: FeedbackTimingRecord,
    pipeline_timing: PipelineTimingRecord,
    consensus: ConsensusExecutionRecord,
}

struct AdaptiveExecutionContext<'a> {
    mode: HarnessMode,
    graph: &'a ProfileGraph,
    parallel_executor: &'a SpeculativeParallelBlockExecutor,
    predicted_block: &'a ProducedBlock,
    decided_block: &'a ProducedBlock,
    consensus_cutoff: Duration,
    measured: bool,
}

fn execute_adaptive_block(
    pipeline: &mut AdaptiveSerialPipeline,
    context: AdaptiveExecutionContext<'_>,
) -> Result<Option<MeasuredAdaptiveBlock>, HarnessError> {
    let AdaptiveExecutionContext {
        mode,
        graph,
        parallel_executor,
        predicted_block,
        decided_block,
        consensus_cutoff,
        measured,
    } = context;
    let total_started = Instant::now();

    let planning_started = Instant::now();
    let (plan, planning_metrics) = pipeline
        .plan_block_with_metrics(parallel_executor.engine(), graph, predicted_block)
        .map_err(display_error)?;
    let planning_wall = planning_started.elapsed();

    // Admission bypass is a real fail-safe: once selected, do no speculative execution at all.
    // Wait for the decided block and execute it directly through the canonical serial executor.
    // This makes a bypass cost approximately the serial reference instead of paying detached
    // snapshot/receipt/reconciliation overhead just to serialize the speculative path.
    if plan.serial_bypassed
        && !pipeline
            .planning_config()
            .serial_bypass
            .buffered_preexecution
    {
        let pre_consensus = planning_wall.min(consensus_cutoff);
        let cutoff_overrun = planning_wall.saturating_sub(consensus_cutoff);
        let serial_started = Instant::now();
        let (report, contract_diagnostics) =
            SerialBlockExecutor::new(parallel_executor.engine().clone())
                .execute_with_diagnostics(
                    decided_block,
                    &canonical_serial_plan(decided_block.transactions.len()),
                )
                .map_err(display_error)?;
        let serial_wall = serial_started.elapsed();
        let post_consensus = cutoff_overrun + serial_wall;
        let total_adaptive_block_wall = total_started.elapsed();
        let serial_service_nanos = serial_service_nanos_from_report(&report);
        pipeline
            .observe_block_economics(BlockEconomicsObservation {
                epoch: decided_block.context.height,
                serial_service_nanos,
                transaction_count: decided_block.transactions.len(),
                pre_consensus,
                post_consensus,
                feedback_summary: ApplySummary::default(),
                serial_bypassed: true,
            })
            .map_err(display_error)?;

        if !measured {
            return Ok(None);
        }

        let divergence = block_divergence_stats(predicted_block, decided_block);
        let consensus = ConsensusExecutionRecord {
            cutoff_nanos: nanos(consensus_cutoff),
            candidate_transactions: u64::try_from(predicted_block.transactions.len())
                .unwrap_or(u64::MAX),
            decided_transactions: u64::try_from(decided_block.transactions.len())
                .unwrap_or(u64::MAX),
            shared_transactions: divergence.shared_transactions,
            same_position_transactions: divergence.same_position_transactions,
            common_prefix_transactions: divergence.common_prefix_transactions,
            prepared_receipts: 0,
            successful_preexecution_receipts: Some(0),
            failed_preexecution_receipts: Some(0),
            receipts_ready_by_cutoff: 0,
            receipts_completed_after_cutoff: 0,
            cutoff_reached: !cutoff_overrun.is_zero(),
            pre_consensus_nanos: nanos(pre_consensus),
            pre_consensus_overrun_nanos: nanos(cutoff_overrun),
            post_consensus_nanos: nanos(post_consensus),
            bottleneck_nanos: nanos(pre_consensus.max(post_consensus)),
            ..ConsensusExecutionRecord::default()
        };
        return Ok(Some(MeasuredAdaptiveBlock {
            plan,
            planning_metrics,
            execution: MeasuredExecution::SerialBypass(Box::new(MeasuredSerialBypassExecution {
                report,
                wall: serial_wall,
                contract_diagnostics,
            })),
            feedback_summary: ApplySummary::default(),
            feedback_timing: FeedbackTimingRecord::default(),
            pipeline_timing: PipelineTimingRecord::from_durations(
                planning_wall,
                Duration::ZERO,
                Duration::ZERO,
                serial_wall,
                Duration::ZERO,
                total_adaptive_block_wall,
            ),
            consensus,
        }));
    }

    let buffered_serial_executor;
    let executor = if plan.serial_bypassed {
        buffered_serial_executor = SpeculativeParallelBlockExecutor::new(
            parallel_executor.engine().clone(),
            ParallelExecutionConfig { workers: 1 },
        );
        &buffered_serial_executor
    } else {
        parallel_executor
    };
    let remaining_budget = consensus_cutoff.saturating_sub(planning_wall);
    let preexecution_started = Instant::now();
    let prepared = executor
        .prepare_with_cutoff(
            predicted_block,
            &plan.speculative_execution_plan,
            remaining_budget,
        )
        .map_err(display_error)?;
    let preexecution_metrics = prepared.metrics.clone();
    let successful_preexecution_receipts = u64::try_from(
        prepared
            .receipts
            .iter()
            .filter(|receipt| receipt.is_success())
            .count(),
    )
    .unwrap_or(u64::MAX);
    let failed_preexecution_receipts = u64::try_from(
        prepared
            .receipts
            .iter()
            .filter(|receipt| !receipt.is_success())
            .count(),
    )
    .unwrap_or(u64::MAX);
    let preexecution_report = executor
        .pre_execution_report(predicted_block, &prepared)
        .map_err(display_error)?;
    let preexecution_wall = preexecution_started.elapsed();

    let pre_feedback_started = Instant::now();
    let pre_summary = if plan.serial_bypassed {
        ApplySummary::default()
    } else {
        match mode {
            HarnessMode::Static => ApplySummary::default(),
            HarnessMode::ProbabilityOnly => pipeline
                .process_pre_execution_probability_only(
                    graph,
                    &plan,
                    &preexecution_report,
                    predicted_block.context.height,
                )
                .map_err(display_error)?,
            HarnessMode::CostAware => pipeline
                .process_pre_execution_report(
                    graph,
                    &plan,
                    &preexecution_report,
                    predicted_block.context.height,
                )
                .map_err(display_error)?,
        }
    };
    let pre_feedback_duration = if mode == HarnessMode::Static || plan.serial_bypassed {
        Duration::ZERO
    } else {
        pre_feedback_started.elapsed()
    };

    let pre_consensus_eligible = planning_wall + preexecution_wall + pre_feedback_duration;
    let pre_consensus = pre_consensus_eligible.min(consensus_cutoff);
    let cutoff_overrun = pre_consensus_eligible.saturating_sub(consensus_cutoff);

    let reconciliation_started = Instant::now();
    let reconciliation = executor
        .validate_prepared(decided_block, prepared)
        .map_err(display_error)?;
    let reconciliation_wall = reconciliation_started.elapsed();

    let post_feedback_started = Instant::now();
    let post_summary = if plan.serial_bypassed {
        ApplySummary::default()
    } else {
        match mode {
            HarnessMode::Static => ApplySummary::default(),
            HarnessMode::ProbabilityOnly => pipeline
                .process_reconciliation_probability_only(
                    graph,
                    &plan,
                    &reconciliation,
                    decided_block.context.height,
                )
                .map_err(display_error)?,
            HarnessMode::CostAware => pipeline
                .process_reconciliation_report(
                    graph,
                    &plan,
                    &reconciliation,
                    decided_block.context.height,
                )
                .map_err(display_error)?,
        }
    };
    let post_feedback_duration = if mode == HarnessMode::Static || plan.serial_bypassed {
        Duration::ZERO
    } else {
        post_feedback_started.elapsed()
    };
    let post_consensus = cutoff_overrun + reconciliation_wall + post_feedback_duration;
    let total_adaptive_block_wall = total_started.elapsed();
    let feedback_summary = merge_apply_summaries(pre_summary, post_summary);

    let estimated_serial_service_nanos =
        estimated_serial_service_nanos(&preexecution_report, &reconciliation);
    pipeline
        .observe_block_economics(BlockEconomicsObservation {
            epoch: decided_block.context.height,
            serial_service_nanos: estimated_serial_service_nanos,
            transaction_count: decided_block.transactions.len(),
            pre_consensus,
            post_consensus,
            feedback_summary,
            serial_bypassed: plan.serial_bypassed,
        })
        .map_err(display_error)?;

    if !measured {
        return Ok(None);
    }

    let divergence = block_divergence_stats(predicted_block, decided_block);
    let diagnostics = &preexecution_metrics.dependency_diagnostics;
    let consensus = ConsensusExecutionRecord {
        cutoff_nanos: nanos(consensus_cutoff),
        candidate_transactions: u64::try_from(predicted_block.transactions.len())
            .unwrap_or(u64::MAX),
        decided_transactions: u64::try_from(decided_block.transactions.len()).unwrap_or(u64::MAX),
        shared_transactions: divergence.shared_transactions,
        same_position_transactions: divergence.same_position_transactions,
        common_prefix_transactions: divergence.common_prefix_transactions,
        prepared_receipts: preexecution_metrics.speculative.speculative_results,
        successful_preexecution_receipts: Some(successful_preexecution_receipts),
        failed_preexecution_receipts: Some(failed_preexecution_receipts),
        receipts_ready_by_cutoff: diagnostics.receipts_ready_by_cutoff,
        receipts_completed_after_cutoff: diagnostics.receipts_completed_after_cutoff,
        cutoff_reached: diagnostics.cutoff_reached,
        pre_consensus_nanos: nanos(pre_consensus),
        pre_consensus_overrun_nanos: nanos(cutoff_overrun),
        post_consensus_nanos: nanos(post_consensus),
        bottleneck_nanos: nanos(pre_consensus.max(post_consensus)),
        ..ConsensusExecutionRecord::default()
    };

    Ok(Some(MeasuredAdaptiveBlock {
        plan,
        planning_metrics,
        execution: MeasuredExecution::Speculative(Box::new(MeasuredSplitExecution {
            preexecution_report,
            preexecution_metrics,
            reconciliation,
        })),
        feedback_summary,
        feedback_timing: FeedbackTimingRecord::from_durations(
            pre_feedback_duration,
            post_feedback_duration,
        ),
        pipeline_timing: PipelineTimingRecord::from_durations(
            planning_wall,
            preexecution_wall,
            pre_feedback_duration,
            reconciliation_wall,
            post_feedback_duration,
            total_adaptive_block_wall,
        ),
        consensus,
    }))
}

#[derive(Clone, Copy, Debug, Default)]
struct BlockDivergenceStats {
    shared_transactions: u64,
    same_position_transactions: u64,
    common_prefix_transactions: u64,
}

fn block_divergence_stats(
    predicted: &ProducedBlock,
    decided: &ProducedBlock,
) -> BlockDivergenceStats {
    let predicted_ids = predicted
        .transactions
        .iter()
        .map(|transaction| transaction.transaction_id())
        .collect::<std::collections::BTreeSet<_>>();
    let decided_ids = decided
        .transactions
        .iter()
        .map(|transaction| transaction.transaction_id())
        .collect::<std::collections::BTreeSet<_>>();
    let shared_transactions =
        u64::try_from(predicted_ids.intersection(&decided_ids).count()).unwrap_or(u64::MAX);
    let same_position_transactions = u64::try_from(
        predicted
            .transactions
            .iter()
            .zip(&decided.transactions)
            .filter(|(left, right)| left.transaction_id() == right.transaction_id())
            .count(),
    )
    .unwrap_or(u64::MAX);
    let common_prefix_transactions = u64::try_from(
        predicted
            .transactions
            .iter()
            .zip(&decided.transactions)
            .take_while(|(left, right)| left.transaction_id() == right.transaction_id())
            .count(),
    )
    .unwrap_or(u64::MAX);
    BlockDivergenceStats {
        shared_transactions,
        same_position_transactions,
        common_prefix_transactions,
    }
}

fn serial_service_nanos_from_report(report: &BlockExecutionReport) -> u64 {
    report.transactions.iter().fold(0_u64, |total, execution| {
        total.saturating_add(nanos(execution.timing.service_duration))
    })
}

fn estimated_serial_service_nanos(
    preexecution: &BlockExecutionReport,
    reconciliation: &acg_validator_sim::SplitPhaseSpeculativeExecutionReport,
) -> u64 {
    let pre_service = preexecution
        .transactions
        .iter()
        .filter_map(|execution| {
            execution.result.as_ref().ok().map(|_| {
                (
                    execution.transaction_id,
                    nanos(execution.timing.service_duration),
                )
            })
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    reconciliation
        .reconciliation
        .iter()
        .fold(0_u64, |total, diagnostic| {
            let service = if diagnostic.reexecution_duration.is_zero() {
                pre_service
                    .get(&diagnostic.transaction_id)
                    .copied()
                    .unwrap_or(0)
            } else {
                nanos(diagnostic.reexecution_duration)
            };
            total.saturating_add(service)
        })
}

struct SerialReference {
    warmup_reports: Vec<BlockExecutionReport>,
    report: BlockExecutionReport,
    wall: Duration,
}

fn maybe_write_correctness_diagnostics(
    manifest: &ExperimentManifest,
    run: &RunIdentity,
    serial: &dyn PreparedBenchmark,
    adaptive: &dyn PreparedBenchmark,
    serial_state: &[u8],
    adaptive_state: &[u8],
    correctness: &CorrectnessRecord,
) -> Result<(), HarnessError> {
    let Some(directory) = std::env::var_os("ACG_CORRECTNESS_DIAGNOSTICS_DIR") else {
        return Ok(());
    };
    let directory = PathBuf::from(directory);
    fs::create_dir_all(&directory)?;

    let first_mismatch_byte = serial_state
        .iter()
        .zip(adaptive_state.iter())
        .position(|(serial_byte, adaptive_byte)| serial_byte != adaptive_byte)
        .or_else(|| {
            (serial_state.len() != adaptive_state.len())
                .then(|| serial_state.len().min(adaptive_state.len()))
        });

    let document = serde_json::json!({
        "schema_version": 1,
        "experiment_id": manifest.experiment_id.as_str(),
        "run": run,
        "correctness": {
            "serial_equivalent": correctness.serial_equivalent,
            "canonical_state_digest": correctness.canonical_state_digest.as_deref(),
            "serial_reference_digest": correctness.serial_reference_digest.as_deref(),
            "serial_state_bytes": serial_state.len(),
            "adaptive_state_bytes": adaptive_state.len(),
            "first_mismatch_byte": first_mismatch_byte,
        },
        "serial_state": serial.canonical_state_diagnostics()?,
        "adaptive_state": adaptive.canonical_state_diagnostics()?,
    });

    let safe_experiment = manifest
        .experiment_id
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let safe_mode = run
        .mode
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect::<String>();
    let path = directory.join(format!(
        "{}-run{:04}-{}-seed{}.json",
        safe_experiment, run.run_index, safe_mode, run.seed
    ));
    fs::write(path, serde_json::to_vec_pretty(&document)?)?;
    Ok(())
}

fn run_serial_reference(
    prepared: &dyn PreparedBenchmark,
    retain_warmup_reports: bool,
) -> Result<SerialReference, HarnessError> {
    let executor = acg_validator_sim::SerialBlockExecutor::new(prepared.engine().clone());
    let mut warmup_reports = if retain_warmup_reports {
        Vec::with_capacity(prepared.warmup_decided_blocks().len())
    } else {
        Vec::new()
    };
    for block in prepared.warmup_decided_blocks() {
        let report = executor
            .execute(block, &canonical_serial_plan(block.transactions.len()))
            .map_err(display_error)?;
        if retain_warmup_reports {
            warmup_reports.push(report);
        }
    }
    let block = prepared.measured_decided_block();
    let started = Instant::now();
    let report = executor
        .execute(block, &canonical_serial_plan(block.transactions.len()))
        .map_err(display_error)?;
    Ok(SerialReference {
        warmup_reports,
        report,
        wall: started.elapsed(),
    })
}

fn canonical_serial_plan(transaction_count: usize) -> ExecutionPlan {
    ExecutionPlan {
        transaction_count,
        waves: (0..transaction_count)
            .map(|transaction| ExecutionWave {
                transaction_indices: vec![transaction],
            })
            .collect(),
        dependencies: Vec::new(),
    }
}

fn ensure_deterministic_preparation(
    serial: &dyn PreparedBenchmark,
    adaptive: &dyn PreparedBenchmark,
) -> Result<(), HarnessError> {
    if serial.warmup_blocks() != adaptive.warmup_blocks()
        || serial.warmup_decided_blocks() != adaptive.warmup_decided_blocks()
        || serial.measured_block() != adaptive.measured_block()
        || serial.measured_decided_block() != adaptive.measured_decided_block()
    {
        return Err(HarnessError::NonDeterministicTransactions);
    }
    let serial_state = serial.canonical_state_bytes()?;
    let adaptive_state = adaptive.canonical_state_bytes()?;
    if serial_state != adaptive_state {
        return Err(HarnessError::NonDeterministicInitialState);
    }
    Ok(())
}

fn execution_plan_dag_bound(plan: &ExecutionPlan, services: &[u64]) -> u64 {
    let mut completion = vec![0_u64; services.len()];
    for wave in &plan.waves {
        for &transaction in &wave.transaction_indices {
            let predecessor_completion = plan
                .dependencies
                .iter()
                .filter(|dependency| dependency.successor_index == transaction)
                .map(|dependency| completion[dependency.predecessor_index])
                .max()
                .unwrap_or(0);
            completion[transaction] = predecessor_completion.saturating_add(services[transaction]);
        }
    }
    completion.into_iter().max().unwrap_or(0)
}

fn serial_services(report: &BlockExecutionReport) -> Result<Vec<u64>, HarnessError> {
    let mut services = vec![None; report.transactions.len()];
    for execution in &report.transactions {
        if execution.transaction_index >= services.len()
            || services[execution.transaction_index].is_some()
        {
            return Err(HarnessError::MalformedSerialReport);
        }
        services[execution.transaction_index] = Some(nanos(execution.timing.service_duration));
    }
    services
        .into_iter()
        .collect::<Option<Vec<_>>>()
        .ok_or(HarnessError::MalformedSerialReport)
}

fn adaptive_state_record(
    pipeline: &AdaptiveSerialPipeline,
    graph: &ProfileGraph,
    epoch: u64,
    config: &AdaptiveFeedbackConfig,
) -> Result<AdaptiveStateRecord, HarnessError> {
    let store = pipeline.feedback_store();
    let mut relationship_count = 0_u64;
    let mut probability_sum = 0.0_f64;
    let mut confidence_sum = 0.0_f64;
    let mut miss_history = 0_u64;
    for edge in graph.edges() {
        let estimate = store
            .estimate_static_edge(edge.index, epoch, config)
            .map_err(display_error)?;
        relationship_count = relationship_count.saturating_add(1);
        probability_sum += estimate.probability;
        confidence_sum += estimate.confidence;
        if estimate.has_candidate_miss_history() {
            miss_history = miss_history.saturating_add(1);
        }
    }
    for edge in store.fallback_edges() {
        let estimate = store
            .estimate_fallback_edge(edge.id, epoch, config)
            .map_err(display_error)?;
        relationship_count = relationship_count.saturating_add(1);
        probability_sum += estimate.probability;
        confidence_sum += estimate.confidence;
        if estimate.has_candidate_miss_history() {
            miss_history = miss_history.saturating_add(1);
        }
    }
    let scale = f64::from(u16::MAX);
    let mean_probability_q16 = if relationship_count == 0 {
        0
    } else {
        ((probability_sum / relationship_count as f64) * scale)
            .round()
            .clamp(0.0, scale) as u64
    };
    let mean_confidence_q16 = if relationship_count == 0 {
        0
    } else {
        ((confidence_sum / relationship_count as f64) * scale)
            .round()
            .clamp(0.0, scale) as u64
    };
    Ok(AdaptiveStateRecord {
        static_relationships: u64::try_from(graph.edges().len()).unwrap_or(u64::MAX),
        runtime_fallback_relationships: u64::try_from(store.fallback_edges().len())
            .unwrap_or(u64::MAX),
        candidate_miss_history_relationships: miss_history,
        mean_probability_q16,
        mean_confidence_q16,
    })
}

fn concrete_conflict_dag_bound(
    services: &[u64],
    conflicts: &[ObservedConflict],
) -> Result<u64, HarnessError> {
    let mut predecessors = vec![Vec::<usize>::new(); services.len()];
    for conflict in conflicts {
        let left = conflict.left.0 as usize;
        let right = conflict.right.0 as usize;
        if left >= services.len() || right >= services.len() || left == right {
            return Err(HarnessError::MalformedSerialReport);
        }
        let (predecessor, successor) = if left < right {
            (left, right)
        } else {
            (right, left)
        };
        predecessors[successor].push(predecessor);
    }
    for items in &mut predecessors {
        items.sort_unstable();
        items.dedup();
    }
    let mut completion = vec![0_u64; services.len()];
    for transaction in 0..services.len() {
        let predecessor_completion = predecessors[transaction]
            .iter()
            .map(|predecessor| completion[*predecessor])
            .max()
            .unwrap_or(0);
        completion[transaction] = predecessor_completion.saturating_add(services[transaction]);
    }
    Ok(completion.into_iter().max().unwrap_or(0))
}

fn ceil_div_u64(value: u64, divisor: u64) -> u64 {
    if value == 0 {
        0
    } else {
        1 + (value - 1) / divisor.max(1)
    }
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

fn merge_apply_summaries(left: ApplySummary, right: ApplySummary) -> ApplySummary {
    ApplySummary {
        positive_observations: left
            .positive_observations
            .saturating_add(right.positive_observations),
        negative_observations: left
            .negative_observations
            .saturating_add(right.negative_observations),
        fallback_edges_created: left
            .fallback_edges_created
            .saturating_add(right.fallback_edges_created),
        candidate_misses: left.candidate_misses.saturating_add(right.candidate_misses),
        replay_impact_observations: left
            .replay_impact_observations
            .saturating_add(right.replay_impact_observations),
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

pub(crate) fn parameter<T>(
    parameters: &BTreeMap<String, String>,
    key: &'static str,
    default: T,
) -> Result<T, HarnessError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    parameters.get(key).map_or(Ok(default), |value| {
        value.parse::<T>().map_err(|error| HarnessError::Parameter {
            key,
            value: value.clone(),
            reason: error.to_string(),
        })
    })
}

fn optional_parameter<T>(
    parameters: &BTreeMap<String, String>,
    key: &'static str,
) -> Result<Option<T>, HarnessError>
where
    T: FromStr,
    T::Err: std::fmt::Display,
{
    parameters
        .get(key)
        .map(|value| {
            value.parse::<T>().map_err(|error| HarnessError::Parameter {
                key,
                value: value.clone(),
                reason: error.to_string(),
            })
        })
        .transpose()
}

fn optional_usize_parameter(
    parameters: &BTreeMap<String, String>,
    key: &'static str,
    default: Option<usize>,
) -> Result<Option<usize>, HarnessError> {
    let Some(value) = parameters.get(key) else {
        return Ok(default);
    };
    if matches!(value.as_str(), "none" | "null") {
        return Ok(None);
    }
    value
        .parse::<usize>()
        .map(Some)
        .map_err(|error| HarnessError::Parameter {
            key,
            value: value.clone(),
            reason: error.to_string(),
        })
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn display_error(error: impl std::fmt::Display) -> HarnessError {
    HarnessError::Runtime(error.to_string())
}

#[derive(Debug, Error)]
pub enum HarnessError {
    #[error("unknown benchmark workload {0:?}; register a workload adapter before running it")]
    UnknownWorkload(String),
    #[error("unsupported benchmark mode {0:?}; supported modes are serial, aria-fb, vegeta, exact-access, static, probability-only, cost-aware")]
    UnsupportedMode(String),
    #[error("run requests {workers} workers with a physical-core limit of {physical_cores}")]
    WorkerBudget { workers: u32, physical_cores: u32 },
    #[error("invalid harness parameter {key}={value:?}: {reason}")]
    Parameter {
        key: &'static str,
        value: String,
        reason: String,
    },
    #[error("invalid workload parameter: {0}")]
    WorkloadParameter(String),
    #[error(
        "workload adapter generated different transactions across serial and speculative setup"
    )]
    NonDeterministicTransactions,
    #[error(
        "workload adapter generated different initial state across serial and speculative setup"
    )]
    NonDeterministicInitialState,
    #[error("serial reference report is malformed")]
    MalformedSerialReport,
    #[error("serial reference has {serial_transactions} transactions but measured adaptive plan has {measured_transactions}")]
    SerialReferenceShape {
        serial_transactions: usize,
        measured_transactions: usize,
    },
    #[error("measured adaptive block did not retain measurement artifacts")]
    MissingMeasuredArtifacts,
    #[error("numeric value cannot be represented on this platform")]
    NumericOverflow,
    #[error("runtime benchmark operation failed: {0}")]
    Runtime(String),
    #[error(transparent)]
    Acceptance(#[from] AcceptanceError),
    #[error(transparent)]
    ExperimentRecord(#[from] ExperimentRecordError),
    #[error("experiment JSON serialization failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("benchmark I/O failed: {0}")]
    Io(#[from] std::io::Error),
}
