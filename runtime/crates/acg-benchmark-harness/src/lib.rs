//! Manifest-driven common benchmark harness for the Adaptive Conflict Graph runtime.
//!
//! One [`RunIdentity`](acg_evaluation::RunIdentity) corresponds to one measured block. Workload
//! adapters may create deterministic warm-up blocks before it. The harness independently prepares
//! a canonical serial reference and the requested speculative mode, verifies identical initial
//! state/workload generation, executes both, emits one stable Brick-5E [`ExperimentRecord`], and
//! finally evaluates the complete dataset through Brick 5F.
//!
//! The harness is performance/evaluation infrastructure only. It never changes canonical validity
//! or commit semantics.

mod conflictlab;

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
use acg_cosmwasm_engine::{CosmWasmEngine, ParallelExecutionConfig};
use acg_evaluation::{
    AcceptanceError, CorrectnessRecord, ExperimentAcceptanceReport, ExperimentManifest,
    ExperimentMetadata, ExperimentRecord, ExperimentRecordError, FeedbackTimingRecord,
    ParallelismReference, RunIdentity,
};
use acg_feedback::{AdaptiveFeedbackConfig, ApplySummary};
use acg_profile_graph::ProfileGraph;
use acg_runtime_feedback::{
    AdaptiveBlockPlan, AdaptivePlanningConfig, AdaptiveSerialPipeline, RuntimeFeedbackEngine,
    RuntimeFeedbackWeights, TraceConflictConfig,
};
use acg_validator_sim::{
    BlockExecutionReport, ExecutionPlan, ExecutionWave, ProducedBlock, SerialBlockExecutor,
    SpeculativeParallelBlockExecutor,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub use conflictlab::ConflictLabWorkload;

pub const BENCHMARK_HARNESS_SCHEMA_VERSION: u16 = 1;

/// Built-in speculative policy ablations supported by the common harness.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum HarnessMode {
    /// Static symbolic/prior graph. Concrete execution is validated/replayed but no feedback is
    /// retained for later blocks.
    Static,
    /// Brick-3/4 conflict-probability learning only. Replay/serialization cost observations are
    /// intentionally excluded.
    ProbabilityOnly,
    /// Full Brick-5D/5E conflict probability + replay cost/fan-out + learned serialization cost.
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
    fn measured_block(&self) -> &ProducedBlock;
    fn canonical_state_bytes(&self) -> Result<Vec<u8>, HarnessError>;
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
        let mode = HarnessMode::parse(&run.mode)?;
        let tuning = HarnessTuningConfig::from_parameters(&run.parameters)?;

        let serial = workload.prepare(run)?;
        let adaptive = workload.prepare(run)?;
        ensure_deterministic_preparation(serial.as_ref(), adaptive.as_ref())?;

        let serial_reference = run_serial_reference(serial.as_ref())?;
        let serial_state = serial.canonical_state_bytes()?;

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
        let executor = SpeculativeParallelBlockExecutor::new(
            engine.clone(),
            ParallelExecutionConfig {
                workers: usize::try_from(run.workers).map_err(|_| HarnessError::NumericOverflow)?,
            },
        );

        for block in adaptive.warmup_blocks() {
            execute_adaptive_block(mode, &mut pipeline, graph, &executor, block, false)?;
        }

        let measured = execute_adaptive_block(
            mode,
            &mut pipeline,
            graph,
            &executor,
            adaptive.measured_block(),
            true,
        )?
        .ok_or(HarnessError::MissingMeasuredArtifacts)?;
        let adaptive_state = adaptive.canonical_state_bytes()?;
        let correctness = CorrectnessRecord::from_state_bytes(&adaptive_state, &serial_state);

        let serial_services = serial_services(&serial_reference.report)?;
        if serial_services.len() != measured.plan.candidate_graph.transactions().len() {
            return Err(HarnessError::SerialReferenceShape {
                serial_transactions: serial_services.len(),
                measured_transactions: measured.plan.candidate_graph.transactions().len(),
            });
        }
        let serial_equivalent_work_nanos = nanos(serial_reference.wall);
        let serial_cost_dag_bound_nanos = dag_bound_from_services(&measured.plan, &serial_services);

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

        Ok(ExperimentRecord::from_runtime(
            metadata,
            measured.planning_metrics,
            pipeline.planning_config(),
            &measured.plan,
            &measured.preexecution_report,
            &measured.preexecution_metrics,
            &measured.reconciliation,
            ParallelismReference {
                serial_equivalent_work_nanos: Some(serial_equivalent_work_nanos),
                serial_cost_dag_bound_nanos: Some(serial_cost_dag_bound_nanos),
            },
            measured.feedback_summary,
            measured.feedback_timing,
            correctness,
        ))
    }
}

pub struct HarnessOutcome {
    pub records: Vec<ExperimentRecord>,
    pub acceptance: ExperimentAcceptanceReport,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HarnessTuningConfig {
    pub planning_config: AdaptivePlanningConfig,
    pub feedback_config: AdaptiveFeedbackConfig,
    pub trace_config: TraceConflictConfig,
}

impl HarnessTuningConfig {
    pub fn from_parameters(parameters: &BTreeMap<String, String>) -> Result<Self, HarnessError> {
        const ACG_KEYS: &[&str] = &[
            "acg.edge_materialization_threshold",
            "acg.soft_threshold",
            "acg.hard_threshold",
            "acg.risk_budget",
            "acg.max_wave_width",
            "acg.independent_observations_before_softening",
            "acg.serialization_cost_reference_nanos",
            "acg.invalidation_fanout_weight",
            "acg.feedback_retention_factor",
            "acg.feedback_confidence_scale",
            "acg.fallback_prior_probability",
            "acg.fallback_prior_strength",
            "acg.feedback_epsilon",
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
                independent_observations_before_softening: parameter(
                    parameters,
                    "acg.independent_observations_before_softening",
                    planning_default
                        .scheduler
                        .independent_observations_before_softening,
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
        };
        let trace = TraceConflictConfig {
            include_reverted_accesses: parameter(
                parameters,
                "acg.include_reverted_accesses",
                TraceConflictConfig::default().include_reverted_accesses,
            )?,
        };

        planning.validate().map_err(display_error)?;
        feedback.validate().map_err(display_error)?;
        Ok(Self {
            planning_config: planning,
            feedback_config: feedback,
            trace_config: trace,
        })
    }
}

struct MeasuredAdaptiveBlock {
    plan: AdaptiveBlockPlan,
    planning_metrics: acg_runtime_feedback::AdaptivePlanningMetrics,
    preexecution_report: BlockExecutionReport,
    preexecution_metrics: acg_cosmwasm_engine::ParallelSpeculativeExecutionMetrics,
    reconciliation: acg_validator_sim::SplitPhaseSpeculativeExecutionReport,
    feedback_summary: ApplySummary,
    feedback_timing: FeedbackTimingRecord,
}

fn execute_adaptive_block(
    mode: HarnessMode,
    pipeline: &mut AdaptiveSerialPipeline,
    graph: &ProfileGraph,
    executor: &SpeculativeParallelBlockExecutor,
    block: &ProducedBlock,
    measured: bool,
) -> Result<Option<MeasuredAdaptiveBlock>, HarnessError> {
    let (plan, planning_metrics) = pipeline
        .plan_block_with_metrics(executor.engine(), graph, block)
        .map_err(display_error)?;
    let prepared = executor
        .prepare(block, &plan.speculative_execution_plan)
        .map_err(display_error)?;
    let preexecution_metrics = prepared.metrics.clone();
    let preexecution_report = executor
        .pre_execution_report(block, &prepared)
        .map_err(display_error)?;

    let pre_feedback_started = Instant::now();
    let pre_summary = match mode {
        HarnessMode::Static => ApplySummary::default(),
        HarnessMode::ProbabilityOnly => pipeline
            .process_pre_execution_probability_only(
                graph,
                &plan,
                &preexecution_report,
                block.context.height,
            )
            .map_err(display_error)?,
        HarnessMode::CostAware => pipeline
            .process_pre_execution_report(graph, &plan, &preexecution_report, block.context.height)
            .map_err(display_error)?,
    };
    let pre_feedback_duration = if mode == HarnessMode::Static {
        Duration::ZERO
    } else {
        pre_feedback_started.elapsed()
    };

    let reconciliation = executor
        .validate_prepared(block, prepared)
        .map_err(display_error)?;
    let post_feedback_started = Instant::now();
    let post_summary = match mode {
        HarnessMode::Static => ApplySummary::default(),
        HarnessMode::ProbabilityOnly => pipeline
            .process_reconciliation_probability_only(
                graph,
                &plan,
                &reconciliation,
                block.context.height,
            )
            .map_err(display_error)?,
        HarnessMode::CostAware => pipeline
            .process_reconciliation_report(graph, &plan, &reconciliation, block.context.height)
            .map_err(display_error)?,
    };
    let post_feedback_duration = if mode == HarnessMode::Static {
        Duration::ZERO
    } else {
        post_feedback_started.elapsed()
    };

    if !measured {
        return Ok(None);
    }
    Ok(Some(MeasuredAdaptiveBlock {
        plan,
        planning_metrics,
        preexecution_report,
        preexecution_metrics,
        reconciliation,
        feedback_summary: merge_apply_summaries(pre_summary, post_summary),
        feedback_timing: FeedbackTimingRecord::from_durations(
            pre_feedback_duration,
            post_feedback_duration,
        ),
    }))
}

struct SerialReference {
    report: BlockExecutionReport,
    wall: Duration,
}

fn run_serial_reference(prepared: &dyn PreparedBenchmark) -> Result<SerialReference, HarnessError> {
    let executor = SerialBlockExecutor::new(prepared.engine().clone());
    for block in prepared.warmup_blocks() {
        executor
            .execute(block, &canonical_serial_plan(block.transactions.len()))
            .map_err(display_error)?;
    }
    let block = prepared.measured_block();
    let started = Instant::now();
    let report = executor
        .execute(block, &canonical_serial_plan(block.transactions.len()))
        .map_err(display_error)?;
    Ok(SerialReference {
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
        || serial.measured_block() != adaptive.measured_block()
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

fn dag_bound_from_services(plan: &AdaptiveBlockPlan, services: &[u64]) -> u64 {
    let mut completion = vec![0_u64; services.len()];
    for transaction in 0..services.len() {
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
        serialization_cost_observations: left
            .serialization_cost_observations
            .saturating_add(right.serialization_cost_observations),
        attributed_serialization_cost_nanos: left
            .attributed_serialization_cost_nanos
            .saturating_add(right.attributed_serialization_cost_nanos),
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
    #[error("unsupported benchmark mode {0:?}; supported modes are static, probability-only, cost-aware")]
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
