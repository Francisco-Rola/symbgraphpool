//! Brick 5F acceptance manifests and machine-readable evaluation gates.
//!
//! Acceptance is deliberately separate from execution. A record can be rejected as incomplete,
//! scientifically inadmissible, or a performance regression without changing any canonical state
//! transition or speculative scheduling decision.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::Path,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{ExperimentRecord, ExperimentRecordError, EXPERIMENT_RECORD_SCHEMA_VERSION};

pub const EXPERIMENT_MANIFEST_SCHEMA_VERSION: u16 = 1;
pub const ACCEPTANCE_REPORT_SCHEMA_VERSION: u16 = 1;

const PUBLICATION_ENVIRONMENT_KEYS: &[&str] =
    &["cpu_model", "git_dirty", "kernel", "logical_cores", "os"];

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
pub struct RunIdentity {
    pub workload: String,
    pub mode: String,
    pub run_index: u32,
    pub seed: u64,
    pub workers: u32,
    #[serde(default)]
    pub parameters: BTreeMap<String, String>,
}

impl RunIdentity {
    pub fn from_record(record: &ExperimentRecord) -> Self {
        Self {
            workload: record.metadata.workload.clone(),
            mode: record.metadata.mode.clone(),
            run_index: record.metadata.run_index,
            seed: record.metadata.seed,
            workers: record.metadata.workers,
            parameters: record.metadata.parameters.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PerformanceAcceptancePolicy {
    /// Reject when `observed_service_dag / serial_cost_dag` exceeds this threshold.
    pub max_service_inflation_milli: Option<u64>,
    /// Reject when `execution_wall / observed_service_dag` exceeds this threshold.
    pub max_scheduler_realization_milli: Option<u64>,
    /// Reject when `planning_total / execution_wall` exceeds this threshold.
    pub max_planning_overhead_milli: Option<u64>,
    /// Reject when `feedback_update / execution_wall` exceeds this threshold.
    pub max_feedback_overhead_milli: Option<u64>,
    /// Reject when `serial_equivalent_work / execution_wall` is below this threshold.
    pub min_parallel_speedup_milli: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AcceptancePolicy {
    pub require_started_at_utc: bool,
    pub require_git_revision: bool,
    pub require_build_profile: bool,
    pub require_rustc_version: bool,
    pub require_clean_git_tree: bool,
    pub required_build_profile: Option<String>,
    pub require_nonempty_parameters: bool,
    pub require_serial_reference: bool,
    pub require_correctness_digests: bool,
    pub require_serial_equivalent: bool,
    #[serde(default)]
    pub required_environment_keys: Vec<String>,
    #[serde(default)]
    pub performance: PerformanceAcceptancePolicy,
}

impl AcceptancePolicy {
    /// Publication-grade completeness and correctness checks, without assuming any performance
    /// threshold. Performance thresholds are experiment-specific and must be configured explicitly.
    pub fn publication() -> Self {
        Self {
            require_started_at_utc: true,
            require_git_revision: true,
            require_build_profile: true,
            require_rustc_version: true,
            require_clean_git_tree: true,
            required_build_profile: Some("release".to_owned()),
            require_nonempty_parameters: true,
            require_serial_reference: true,
            require_correctness_digests: true,
            require_serial_equivalent: true,
            required_environment_keys: PUBLICATION_ENVIRONMENT_KEYS
                .iter()
                .map(|key| (*key).to_owned())
                .collect(),
            performance: PerformanceAcceptancePolicy::default(),
        }
    }

    /// A relaxed policy for mechanism smoke tests. Schema, identity, worker-budget, and internal
    /// consistency checks still run, but publication metadata and serial-reference fields may be
    /// absent.
    pub fn smoke() -> Self {
        Self {
            require_started_at_utc: false,
            require_git_revision: false,
            require_build_profile: false,
            require_rustc_version: false,
            require_clean_git_tree: false,
            required_build_profile: None,
            require_nonempty_parameters: false,
            require_serial_reference: false,
            require_correctness_digests: false,
            require_serial_equivalent: false,
            required_environment_keys: Vec::new(),
            performance: PerformanceAcceptancePolicy::default(),
        }
    }
}

impl Default for AcceptancePolicy {
    fn default() -> Self {
        Self::publication()
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExperimentManifest {
    pub schema_version: u16,
    pub experiment_id: String,
    pub record_schema_version: u16,
    pub physical_core_limit: u32,
    pub policy: AcceptancePolicy,
    pub runs: Vec<RunIdentity>,
}

impl ExperimentManifest {
    pub fn new(
        experiment_id: impl Into<String>,
        physical_core_limit: u32,
        runs: Vec<RunIdentity>,
    ) -> Self {
        Self {
            schema_version: EXPERIMENT_MANIFEST_SCHEMA_VERSION,
            experiment_id: experiment_id.into(),
            record_schema_version: EXPERIMENT_RECORD_SCHEMA_VERSION,
            physical_core_limit,
            policy: AcceptancePolicy::publication(),
            runs,
        }
    }

    pub fn validate(&self) -> Result<(), AcceptanceError> {
        let mut issues = Vec::new();
        if self.schema_version != EXPERIMENT_MANIFEST_SCHEMA_VERSION {
            issues.push(format!(
                "unsupported manifest schema version {}; supported version is {}",
                self.schema_version, EXPERIMENT_MANIFEST_SCHEMA_VERSION
            ));
        }
        if self.record_schema_version != EXPERIMENT_RECORD_SCHEMA_VERSION {
            issues.push(format!(
                "manifest requires experiment-record schema {}, but this build supports {}",
                self.record_schema_version, EXPERIMENT_RECORD_SCHEMA_VERSION
            ));
        }
        if self.experiment_id.trim().is_empty() {
            issues.push("experiment_id must not be empty".to_owned());
        }
        if self.physical_core_limit == 0 {
            issues.push("physical_core_limit must be greater than zero".to_owned());
        }
        if self.runs.is_empty() {
            issues.push("manifest must enumerate at least one run".to_owned());
        }

        let mut seen = BTreeSet::new();
        for (index, run) in self.runs.iter().enumerate() {
            if run.workload.trim().is_empty() {
                issues.push(format!("runs[{index}].workload must not be empty"));
            }
            if run.mode.trim().is_empty() {
                issues.push(format!("runs[{index}].mode must not be empty"));
            }
            if run.workers == 0 {
                issues.push(format!("runs[{index}].workers must be greater than zero"));
            }
            if self.physical_core_limit != 0 && run.workers > self.physical_core_limit {
                issues.push(format!(
                    "runs[{index}] requests {} workers above physical-core limit {}",
                    run.workers, self.physical_core_limit
                ));
            }
            if !seen.insert(run.clone()) {
                issues.push(format!("runs[{index}] duplicates an earlier run identity"));
            }
        }

        if issues.is_empty() {
            Ok(())
        } else {
            Err(AcceptanceError::InvalidManifest { issues })
        }
    }

    pub fn to_pretty_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, AcceptanceError> {
        let manifest: Self = serde_json::from_slice(bytes)?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn evaluate(&self, records: &[ExperimentRecord]) -> ExperimentAcceptanceReport {
        evaluate_experiment(self, records)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AcceptanceIssueCategory {
    PerformanceRegression,
    Incomplete,
    ConfigurationError,
    CorrectnessFailure,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AcceptanceIssue {
    pub category: AcceptanceIssueCategory,
    pub code: String,
    pub message: String,
}

impl AcceptanceIssue {
    fn new(
        category: AcceptanceIssueCategory,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            category,
            code: code.into(),
            message: message.into(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunAcceptanceStatus {
    Accepted,
    PerformanceRegression,
    Incomplete,
    ConfigurationError,
    CorrectnessFailure,
}

impl RunAcceptanceStatus {
    fn from_issues(issues: &[AcceptanceIssue]) -> Self {
        let mut status = Self::Accepted;
        for issue in issues {
            let candidate = match issue.category {
                AcceptanceIssueCategory::PerformanceRegression => Self::PerformanceRegression,
                AcceptanceIssueCategory::Incomplete => Self::Incomplete,
                AcceptanceIssueCategory::ConfigurationError => Self::ConfigurationError,
                AcceptanceIssueCategory::CorrectnessFailure => Self::CorrectnessFailure,
            };
            status = status.max(candidate);
        }
        status
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DerivedAcceptanceMetrics {
    pub planning_overhead_milli: Option<u64>,
    pub feedback_overhead_milli: Option<u64>,
    pub parallel_speedup_milli: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RunAcceptanceReport {
    pub identity: RunIdentity,
    pub status: RunAcceptanceStatus,
    pub issues: Vec<AcceptanceIssue>,
    pub derived: DerivedAcceptanceMetrics,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExperimentAcceptanceStatus {
    Accepted,
    PerformanceRegression,
    Incomplete,
    ConfigurationError,
    CorrectnessFailure,
}

impl From<RunAcceptanceStatus> for ExperimentAcceptanceStatus {
    fn from(status: RunAcceptanceStatus) -> Self {
        match status {
            RunAcceptanceStatus::Accepted => Self::Accepted,
            RunAcceptanceStatus::PerformanceRegression => Self::PerformanceRegression,
            RunAcceptanceStatus::Incomplete => Self::Incomplete,
            RunAcceptanceStatus::ConfigurationError => Self::ConfigurationError,
            RunAcceptanceStatus::CorrectnessFailure => Self::CorrectnessFailure,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExperimentAcceptanceReport {
    pub schema_version: u16,
    pub experiment_id: String,
    pub status: ExperimentAcceptanceStatus,
    pub expected_runs: u64,
    pub observed_runs: u64,
    pub accepted_runs: u64,
    pub performance_regressions: u64,
    pub incomplete_runs: u64,
    pub configuration_errors: u64,
    pub correctness_failures: u64,
    pub missing_runs: Vec<RunIdentity>,
    pub unexpected_runs: Vec<RunIdentity>,
    pub duplicate_runs: Vec<RunIdentity>,
    pub run_reports: Vec<RunAcceptanceReport>,
}

impl ExperimentAcceptanceReport {
    pub fn accepted(&self) -> bool {
        self.status == ExperimentAcceptanceStatus::Accepted
    }

    pub fn to_pretty_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, AcceptanceError> {
        let report: Self = serde_json::from_slice(bytes)?;
        if report.schema_version != ACCEPTANCE_REPORT_SCHEMA_VERSION {
            return Err(AcceptanceError::UnsupportedAcceptanceReportSchema {
                actual: report.schema_version,
                supported: ACCEPTANCE_REPORT_SCHEMA_VERSION,
            });
        }
        Ok(report)
    }
}

pub fn evaluate_record(
    record: &ExperimentRecord,
    policy: &AcceptancePolicy,
    expected_experiment_id: &str,
    physical_core_limit: u32,
) -> RunAcceptanceReport {
    let identity = RunIdentity::from_record(record);
    let mut issues = Vec::new();

    if record.schema_version != EXPERIMENT_RECORD_SCHEMA_VERSION {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::ConfigurationError,
            "unsupported_record_schema",
            format!(
                "record schema {} does not match supported schema {}",
                record.schema_version, EXPERIMENT_RECORD_SCHEMA_VERSION
            ),
        ));
    }
    if record.metadata.experiment_id != expected_experiment_id {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::ConfigurationError,
            "experiment_id_mismatch",
            format!(
                "record experiment_id {:?} does not match manifest {:?}",
                record.metadata.experiment_id, expected_experiment_id
            ),
        ));
    }
    if record.metadata.workload.trim().is_empty() || record.metadata.mode.trim().is_empty() {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::Incomplete,
            "missing_run_identity",
            "workload and mode must be non-empty",
        ));
    }

    require_optional_string(
        &mut issues,
        policy.require_started_at_utc,
        record.metadata.started_at_utc.as_deref(),
        "missing_started_at_utc",
        "started_at_utc is required for publication records",
    );
    require_optional_string(
        &mut issues,
        policy.require_git_revision,
        record.metadata.git_revision.as_deref(),
        "missing_git_revision",
        "git_revision is required for publication records",
    );
    require_optional_string(
        &mut issues,
        policy.require_build_profile,
        record.metadata.build_profile.as_deref(),
        "missing_build_profile",
        "build_profile is required for publication records",
    );
    require_optional_string(
        &mut issues,
        policy.require_rustc_version,
        record.metadata.rustc_version.as_deref(),
        "missing_rustc_version",
        "rustc_version is required for publication records",
    );
    if let Some(required_profile) = nonempty(policy.required_build_profile.as_deref()) {
        match nonempty(record.metadata.build_profile.as_deref()) {
            Some(actual_profile) if actual_profile != required_profile => {
                issues.push(AcceptanceIssue::new(
                    AcceptanceIssueCategory::ConfigurationError,
                    "build_profile_mismatch",
                    format!(
                        "build profile {actual_profile:?} does not match required profile {required_profile:?}"
                    ),
                ));
            }
            Some(_) => {}
            None => issues.push(AcceptanceIssue::new(
                AcceptanceIssueCategory::Incomplete,
                "missing_required_build_profile",
                format!("required build profile {required_profile:?} is not recorded"),
            )),
        }
    }
    if policy.require_clean_git_tree {
        match record
            .metadata
            .environment
            .get("git_dirty")
            .map(String::as_str)
        {
            Some("false") => {}
            Some("true") => issues.push(AcceptanceIssue::new(
                AcceptanceIssueCategory::ConfigurationError,
                "dirty_git_tree",
                "publication performance records require a clean Git worktree",
            )),
            Some(value) if !value.trim().is_empty() => issues.push(AcceptanceIssue::new(
                AcceptanceIssueCategory::ConfigurationError,
                "invalid_git_dirty_metadata",
                format!("git_dirty must be true or false, got {value:?}"),
            )),
            _ => issues.push(AcceptanceIssue::new(
                AcceptanceIssueCategory::Incomplete,
                "missing_git_dirty_metadata",
                "clean-tree acceptance requires explicit git_dirty metadata",
            )),
        }
    }

    for key in &policy.required_environment_keys {
        let present = record
            .metadata
            .environment
            .get(key)
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false);
        if !present {
            issues.push(AcceptanceIssue::new(
                AcceptanceIssueCategory::Incomplete,
                "missing_environment_metadata",
                format!("required environment key {key:?} is missing or empty"),
            ));
        }
    }
    if policy.require_nonempty_parameters && record.metadata.parameters.is_empty() {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::Incomplete,
            "missing_workload_parameters",
            "publication records must include explicit workload parameters",
        ));
    }

    if record.metadata.workers == 0 || record.metadata.physical_cores == 0 {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::ConfigurationError,
            "invalid_worker_budget",
            "workers and physical_cores must both be greater than zero",
        ));
    }
    if record.metadata.workers > record.metadata.physical_cores {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::ConfigurationError,
            "worker_oversubscription",
            format!(
                "{} workers exceed record physical-core budget {}",
                record.metadata.workers, record.metadata.physical_cores
            ),
        ));
    }
    if physical_core_limit != 0 && record.metadata.workers > physical_core_limit {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::ConfigurationError,
            "manifest_core_budget_exceeded",
            format!(
                "{} workers exceed manifest physical-core limit {}",
                record.metadata.workers, physical_core_limit
            ),
        ));
    }
    if record.execution.workers != u64::from(record.metadata.workers) {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::ConfigurationError,
            "execution_worker_mismatch",
            format!(
                "metadata reports {} workers but execution reports {}",
                record.metadata.workers, record.execution.workers
            ),
        ));
    }
    if record.execution.transactions == 0 {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::Incomplete,
            "zero_transactions",
            "evaluation records must contain at least one transaction",
        ));
    }
    if record.execution.max_in_flight > record.execution.workers {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::ConfigurationError,
            "max_in_flight_exceeds_workers",
            format!(
                "max_in_flight {} exceeds execution worker count {}",
                record.execution.max_in_flight, record.execution.workers
            ),
        ));
    }
    if record.parallelism.actual_execution_wall_nanos == 0 {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::Incomplete,
            "zero_execution_wall",
            "actual execution wall must be non-zero",
        ));
    }

    let edge_classes = record
        .scheduling
        .low_edges
        .saturating_add(record.scheduling.soft_edges)
        .saturating_add(record.scheduling.hard_edges);
    if edge_classes != record.scheduling.candidate_edges {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::ConfigurationError,
            "candidate_edge_class_count_mismatch",
            format!(
                "low+soft+hard edge classes total {edge_classes}, candidate_edges is {}",
                record.scheduling.candidate_edges
            ),
        ));
    }
    let dependency_classes = record
        .scheduling
        .soft_dependencies
        .saturating_add(record.scheduling.hard_dependencies);
    if dependency_classes != record.scheduling.ordering_dependencies {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::ConfigurationError,
            "dependency_class_count_mismatch",
            format!(
                "soft+hard dependencies total {dependency_classes}, ordering_dependencies is {}",
                record.scheduling.ordering_dependencies
            ),
        ));
    }
    let feedback_total = record
        .feedback_timing
        .pre_execution_update_nanos
        .saturating_add(record.feedback_timing.reconciliation_update_nanos);
    if feedback_total != record.feedback_timing.total_nanos {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::ConfigurationError,
            "feedback_timing_total_mismatch",
            format!(
                "feedback timing components total {feedback_total} ns, stored total is {} ns",
                record.feedback_timing.total_nanos
            ),
        ));
    }
    let largest_planning_stage = [
        record.planning.adapter_nanos,
        record.planning.candidate_graph_nanos,
        record.planning.scheduler_nanos,
        record.planning.schedule_validation_nanos,
        record.planning.plan_conversion_nanos,
    ]
    .into_iter()
    .max()
    .unwrap_or(0);
    if record.planning.total_nanos < largest_planning_stage {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::ConfigurationError,
            "planning_total_smaller_than_stage",
            "planning total cannot be smaller than an individual planning stage",
        ));
    }

    if policy.require_serial_reference {
        require_metric(
            &mut issues,
            record.parallelism.serial_equivalent_work_nanos,
            "missing_serial_equivalent_work",
            "serial_equivalent_work_nanos is required",
        );
        require_metric(
            &mut issues,
            record.parallelism.serial_cost_dag_bound_nanos,
            "missing_serial_cost_dag_bound",
            "serial_cost_dag_bound_nanos is required",
        );
        require_metric(
            &mut issues,
            record.parallelism.observed_service_dag_bound_nanos,
            "missing_observed_service_dag_bound",
            "observed_service_dag_bound_nanos is required",
        );
        require_metric(
            &mut issues,
            record.parallelism.service_inflation_milli,
            "missing_service_inflation",
            "service_inflation_milli is required when serial references are required",
        );
        require_metric(
            &mut issues,
            record.parallelism.scheduler_realization_milli,
            "missing_scheduler_realization",
            "scheduler_realization_milli is required when serial references are required",
        );
    }

    let canonical_digest = nonempty(record.correctness.canonical_state_digest.as_deref());
    let serial_digest = nonempty(record.correctness.serial_reference_digest.as_deref());
    if policy.require_correctness_digests {
        if canonical_digest.is_none() {
            issues.push(AcceptanceIssue::new(
                AcceptanceIssueCategory::Incomplete,
                "missing_canonical_digest",
                "canonical_state_digest is required",
            ));
        }
        if serial_digest.is_none() {
            issues.push(AcceptanceIssue::new(
                AcceptanceIssueCategory::Incomplete,
                "missing_serial_reference_digest",
                "serial_reference_digest is required",
            ));
        }
    }
    if let (Some(canonical), Some(serial)) = (canonical_digest, serial_digest) {
        if canonical != serial {
            issues.push(AcceptanceIssue::new(
                AcceptanceIssueCategory::CorrectnessFailure,
                "state_digest_mismatch",
                "canonical and serial-reference state digests differ",
            ));
        }
    }
    if policy.require_serial_equivalent && record.correctness.serial_equivalent.is_none() {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::Incomplete,
            "serial_equivalence_unknown",
            "serial_equivalent must be explicitly recorded",
        ));
    }
    if record.correctness.serial_equivalent == Some(false) {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::CorrectnessFailure,
            "serial_equivalence_failed",
            "runtime reported a serial-equivalence failure",
        ));
    }

    let derived = DerivedAcceptanceMetrics {
        planning_overhead_milli: ratio_milli(
            record.planning.total_nanos,
            record.parallelism.actual_execution_wall_nanos,
        ),
        feedback_overhead_milli: ratio_milli(
            record.feedback_timing.total_nanos,
            record.parallelism.actual_execution_wall_nanos,
        ),
        parallel_speedup_milli: record
            .parallelism
            .serial_equivalent_work_nanos
            .and_then(|serial| ratio_milli(serial, record.parallelism.actual_execution_wall_nanos)),
    };

    evaluate_performance(record, policy.performance, derived, &mut issues);

    RunAcceptanceReport {
        identity,
        status: RunAcceptanceStatus::from_issues(&issues),
        issues,
        derived,
    }
}

pub fn evaluate_experiment(
    manifest: &ExperimentManifest,
    records: &[ExperimentRecord],
) -> ExperimentAcceptanceReport {
    let manifest_valid = manifest.validate().is_ok();
    let expected: BTreeSet<_> = manifest.runs.iter().cloned().collect();
    let mut observed_counts = BTreeMap::<RunIdentity, u64>::new();
    for record in records {
        *observed_counts
            .entry(RunIdentity::from_record(record))
            .or_default() += 1;
    }

    let missing_runs = manifest
        .runs
        .iter()
        .filter(|identity| !observed_counts.contains_key(*identity))
        .cloned()
        .collect::<Vec<_>>();
    let unexpected_runs = observed_counts
        .keys()
        .filter(|identity| !expected.contains(*identity))
        .cloned()
        .collect::<Vec<_>>();
    let duplicate_runs = observed_counts
        .iter()
        .filter(|(_, count)| **count > 1)
        .map(|(identity, _)| identity.clone())
        .collect::<Vec<_>>();

    let mut run_reports = records
        .iter()
        .map(|record| {
            evaluate_record(
                record,
                &manifest.policy,
                &manifest.experiment_id,
                manifest.physical_core_limit,
            )
        })
        .collect::<Vec<_>>();
    run_reports.sort_by(|left, right| left.identity.cmp(&right.identity));

    let mut accepted_runs = 0_u64;
    let mut performance_regressions = 0_u64;
    let mut incomplete_runs = 0_u64;
    let mut configuration_errors = 0_u64;
    let mut correctness_failures = 0_u64;
    let mut status = if manifest_valid {
        ExperimentAcceptanceStatus::Accepted
    } else {
        ExperimentAcceptanceStatus::ConfigurationError
    };

    for report in &run_reports {
        match report.status {
            RunAcceptanceStatus::Accepted => accepted_runs = accepted_runs.saturating_add(1),
            RunAcceptanceStatus::PerformanceRegression => {
                performance_regressions = performance_regressions.saturating_add(1)
            }
            RunAcceptanceStatus::Incomplete => incomplete_runs = incomplete_runs.saturating_add(1),
            RunAcceptanceStatus::ConfigurationError => {
                configuration_errors = configuration_errors.saturating_add(1)
            }
            RunAcceptanceStatus::CorrectnessFailure => {
                correctness_failures = correctness_failures.saturating_add(1)
            }
        }
        status = status.max(report.status.into());
    }

    if !missing_runs.is_empty() {
        status = status.max(ExperimentAcceptanceStatus::Incomplete);
    }
    if !unexpected_runs.is_empty() || !duplicate_runs.is_empty() {
        status = status.max(ExperimentAcceptanceStatus::ConfigurationError);
    }

    ExperimentAcceptanceReport {
        schema_version: ACCEPTANCE_REPORT_SCHEMA_VERSION,
        experiment_id: manifest.experiment_id.clone(),
        status,
        expected_runs: u64::try_from(manifest.runs.len()).unwrap_or(u64::MAX),
        observed_runs: u64::try_from(records.len()).unwrap_or(u64::MAX),
        accepted_runs,
        performance_regressions,
        incomplete_runs,
        configuration_errors,
        correctness_failures,
        missing_runs,
        unexpected_runs,
        duplicate_runs,
        run_reports,
    }
}

pub fn read_records_jsonl(
    path: impl AsRef<Path>,
) -> Result<Vec<ExperimentRecord>, AcceptanceError> {
    let bytes = fs::read(path)?;
    let text = std::str::from_utf8(&bytes).map_err(|error| AcceptanceError::InvalidUtf8 {
        message: error.to_string(),
    })?;
    let mut records = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let record = ExperimentRecord::from_json(line.as_bytes()).map_err(|error| {
            AcceptanceError::InvalidRecordLine {
                line: line_index + 1,
                source: Box::new(error),
            }
        })?;
        records.push(record);
    }
    Ok(records)
}

fn evaluate_performance(
    record: &ExperimentRecord,
    policy: PerformanceAcceptancePolicy,
    derived: DerivedAcceptanceMetrics,
    issues: &mut Vec<AcceptanceIssue>,
) {
    threshold_max(
        issues,
        "service_inflation_regression",
        "service inflation",
        record.parallelism.service_inflation_milli,
        policy.max_service_inflation_milli,
    );
    threshold_max(
        issues,
        "scheduler_realization_regression",
        "scheduler realization",
        record.parallelism.scheduler_realization_milli,
        policy.max_scheduler_realization_milli,
    );
    threshold_max(
        issues,
        "planning_overhead_regression",
        "planning overhead",
        derived.planning_overhead_milli,
        policy.max_planning_overhead_milli,
    );
    threshold_max(
        issues,
        "feedback_overhead_regression",
        "feedback overhead",
        derived.feedback_overhead_milli,
        policy.max_feedback_overhead_milli,
    );

    if let Some(minimum) = policy.min_parallel_speedup_milli {
        match derived.parallel_speedup_milli {
            Some(actual) if actual < minimum => issues.push(AcceptanceIssue::new(
                AcceptanceIssueCategory::PerformanceRegression,
                "parallel_speedup_regression",
                format!("parallel speedup {actual} milli is below minimum {minimum} milli"),
            )),
            Some(_) => {}
            None => issues.push(AcceptanceIssue::new(
                AcceptanceIssueCategory::Incomplete,
                "missing_parallel_speedup_metric",
                "minimum speedup is configured but serial/work-wall data is unavailable",
            )),
        }
    }
}

fn threshold_max(
    issues: &mut Vec<AcceptanceIssue>,
    code: &str,
    label: &str,
    actual: Option<u64>,
    maximum: Option<u64>,
) {
    let Some(maximum) = maximum else {
        return;
    };
    match actual {
        Some(actual) if actual > maximum => issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::PerformanceRegression,
            code,
            format!("{label} {actual} milli exceeds maximum {maximum} milli"),
        )),
        Some(_) => {}
        None => issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::Incomplete,
            "missing_performance_metric",
            format!("{label} threshold is configured but the metric is unavailable"),
        )),
    }
}

fn require_optional_string(
    issues: &mut Vec<AcceptanceIssue>,
    required: bool,
    value: Option<&str>,
    code: &str,
    message: &str,
) {
    if required && nonempty(value).is_none() {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::Incomplete,
            code,
            message,
        ));
    }
}

fn require_metric(
    issues: &mut Vec<AcceptanceIssue>,
    value: Option<u64>,
    code: &str,
    message: &str,
) {
    if value.is_none() {
        issues.push(AcceptanceIssue::new(
            AcceptanceIssueCategory::Incomplete,
            code,
            message,
        ));
    }
}

fn nonempty(value: Option<&str>) -> Option<&str> {
    value.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
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

#[derive(Debug, Error)]
pub enum AcceptanceError {
    #[error("invalid experiment manifest: {issues:?}")]
    InvalidManifest { issues: Vec<String> },
    #[error(
        "unsupported acceptance-report schema version {actual}; supported version is {supported}"
    )]
    UnsupportedAcceptanceReportSchema { actual: u16, supported: u16 },
    #[error("invalid experiment record on JSONL line {line}: {source}")]
    InvalidRecordLine {
        line: usize,
        source: Box<ExperimentRecordError>,
    },
    #[error("experiment JSON/manifest error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("experiment input/output error: {0}")]
    Io(#[from] std::io::Error),
    #[error("experiment JSONL is not UTF-8: {message}")]
    InvalidUtf8 { message: String },
}
