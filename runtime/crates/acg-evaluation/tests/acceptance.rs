use std::{collections::BTreeMap, fs};

use acg_evaluation::{
    acceptance::read_records_jsonl, AcceptancePolicy, CorrectnessRecord, ExecutionRecord,
    ExperimentAcceptanceStatus, ExperimentManifest, ExperimentMetadata, ExperimentRecord,
    FeedbackRecord, FeedbackTimingRecord, ParallelismRecord, PerformanceAcceptancePolicy,
    PlanningRecord, RunAcceptanceStatus, RunIdentity, SchedulingRecord,
    EXPERIMENT_RECORD_SCHEMA_VERSION,
};

fn complete_record() -> ExperimentRecord {
    ExperimentRecord {
        schema_version: EXPERIMENT_RECORD_SCHEMA_VERSION,
        metadata: ExperimentMetadata {
            experiment_id: "brick5f-acceptance".to_owned(),
            workload: "conflictlab".to_owned(),
            mode: "cost-aware".to_owned(),
            run_index: 1,
            seed: 42,
            workers: 6,
            physical_cores: 6,
            started_at_utc: Some("2026-08-13T21:30:00Z".to_owned()),
            git_revision: Some("0123456789abcdef".to_owned()),
            build_profile: Some("release".to_owned()),
            rustc_version: Some("rustc 1.75.0".to_owned()),
            environment: BTreeMap::from([
                ("cpu_model".to_owned(), "reference-six-core".to_owned()),
                ("git_dirty".to_owned(), "false".to_owned()),
                ("kernel".to_owned(), "6.x".to_owned()),
                ("logical_cores".to_owned(), "12".to_owned()),
                ("os".to_owned(), "linux".to_owned()),
            ]),
            parameters: BTreeMap::from([
                ("conflict_rate".to_owned(), "0.10".to_owned()),
                ("transactions".to_owned(), "200".to_owned()),
            ]),
        },
        planning: PlanningRecord {
            adapter_nanos: 10_000,
            candidate_graph_nanos: 50_000,
            scheduler_nanos: 5_000,
            schedule_validation_nanos: 2_000,
            plan_conversion_nanos: 3_000,
            total_nanos: 70_000,
        },
        scheduling: SchedulingRecord {
            candidate_edges: 5,
            low_edges: 1,
            soft_edges: 2,
            hard_edges: 2,
            ordering_dependencies: 3,
            soft_dependencies: 1,
            hard_dependencies: 2,
            wave_count: 4,
            max_wave_width: 6,
            ..SchedulingRecord::default()
        },
        parallelism: ParallelismRecord {
            serial_equivalent_work_nanos: Some(10_000_000),
            serial_cost_dag_bound_nanos: Some(2_000_000),
            observed_service_dag_bound_nanos: Some(2_200_000),
            actual_execution_wall_nanos: 2_300_000,
            service_inflation_milli: Some(1_100),
            scheduler_realization_milli: Some(1_045),
        },
        execution: ExecutionRecord {
            transactions: 200,
            workers: 6,
            dependency_count: 3,
            hard_dependency_count: 2,
            max_in_flight: 6,
            speculative_results: 200,
            reused_results: 196,
            invalidated_results: 4,
            replayed_transactions: 4,
            ..ExecutionRecord::default()
        },
        feedback: FeedbackRecord::default(),
        feedback_timing: FeedbackTimingRecord {
            pre_execution_update_nanos: 1_000,
            reconciliation_update_nanos: 500,
            total_nanos: 1_500,
        },
        correctness: CorrectnessRecord {
            canonical_state_digest: Some("same-state".to_owned()),
            serial_reference_digest: Some("same-state".to_owned()),
            serial_equivalent: Some(true),
        },
    }
}

fn manifest_for(record: &ExperimentRecord) -> ExperimentManifest {
    ExperimentManifest::new(
        record.metadata.experiment_id.clone(),
        record.metadata.physical_cores,
        vec![RunIdentity::from_record(record)],
    )
}

#[test]
fn publication_manifest_accepts_complete_record_and_round_trips_report() {
    let record = complete_record();
    let manifest = manifest_for(&record);
    manifest.validate().unwrap();

    let report = manifest.evaluate(std::slice::from_ref(&record));
    assert!(report.accepted());
    assert_eq!(report.status, ExperimentAcceptanceStatus::Accepted);
    assert_eq!(report.accepted_runs, 1);
    assert_eq!(report.run_reports[0].status, RunAcceptanceStatus::Accepted);
    assert_eq!(
        report.run_reports[0].derived.parallel_speedup_milli,
        Some(4_348)
    );

    let manifest_json = manifest.to_pretty_json().unwrap();
    assert_eq!(
        ExperimentManifest::from_json(&manifest_json).unwrap(),
        manifest
    );
    let report_json = report.to_pretty_json().unwrap();
    assert_eq!(
        acg_evaluation::ExperimentAcceptanceReport::from_json(&report_json).unwrap(),
        report
    );
}

#[test]
fn publication_gate_distinguishes_incomplete_correctness_and_configuration_failures() {
    let base = complete_record();
    let manifest = manifest_for(&base);

    let mut incomplete = base.clone();
    incomplete.metadata.git_revision = None;
    let report = manifest.evaluate(&[incomplete]);
    assert_eq!(report.status, ExperimentAcceptanceStatus::Incomplete);
    assert!(report.run_reports[0]
        .issues
        .iter()
        .any(|issue| issue.code == "missing_git_revision"));

    let mut incorrect = base.clone();
    incorrect.correctness.canonical_state_digest = Some("different".to_owned());
    incorrect.correctness.serial_equivalent = Some(false);
    let report = manifest.evaluate(&[incorrect]);
    assert_eq!(
        report.status,
        ExperimentAcceptanceStatus::CorrectnessFailure
    );
    assert!(report.run_reports[0]
        .issues
        .iter()
        .any(|issue| issue.code == "state_digest_mismatch"));

    let mut oversubscribed = base.clone();
    oversubscribed.metadata.workers = 7;
    oversubscribed.execution.workers = 7;
    let report = manifest.evaluate(&[oversubscribed]);
    assert_eq!(
        report.status,
        ExperimentAcceptanceStatus::ConfigurationError
    );
    assert!(report.run_reports[0]
        .issues
        .iter()
        .any(|issue| issue.code == "worker_oversubscription"));

    let mut dirty_debug = base;
    dirty_debug
        .metadata
        .environment
        .insert("git_dirty".to_owned(), "true".to_owned());
    dirty_debug.metadata.build_profile = Some("debug".to_owned());
    let report = manifest.evaluate(&[dirty_debug]);
    assert_eq!(
        report.status,
        ExperimentAcceptanceStatus::ConfigurationError
    );
    let codes = report.run_reports[0]
        .issues
        .iter()
        .map(|issue| issue.code.as_str())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"dirty_git_tree"));
    assert!(codes.contains(&"build_profile_mismatch"));
}

#[test]
fn performance_thresholds_are_explicit_and_do_not_change_publication_validity_by_default() {
    let record = complete_record();
    let mut manifest = manifest_for(&record);
    assert!(manifest.evaluate(std::slice::from_ref(&record)).accepted());

    manifest.policy.performance = PerformanceAcceptancePolicy {
        max_scheduler_realization_milli: Some(1_020),
        max_planning_overhead_milli: Some(20),
        min_parallel_speedup_milli: Some(5_000),
        ..PerformanceAcceptancePolicy::default()
    };
    let report = manifest.evaluate(&[record]);
    assert_eq!(
        report.status,
        ExperimentAcceptanceStatus::PerformanceRegression
    );
    let codes = report.run_reports[0]
        .issues
        .iter()
        .map(|issue| issue.code.as_str())
        .collect::<Vec<_>>();
    assert!(codes.contains(&"scheduler_realization_regression"));
    assert!(codes.contains(&"planning_overhead_regression"));
    assert!(codes.contains(&"parallel_speedup_regression"));
}

#[test]
fn manifest_detects_missing_unexpected_and_duplicate_samples() {
    let first = complete_record();
    let mut second = complete_record();
    second.metadata.run_index = 2;
    second.metadata.seed = 43;
    let manifest = ExperimentManifest::new(
        "brick5f-acceptance",
        6,
        vec![
            RunIdentity::from_record(&first),
            RunIdentity::from_record(&second),
        ],
    );

    let mut unexpected = complete_record();
    unexpected.metadata.run_index = 99;
    unexpected.metadata.seed = 999;
    let report = manifest.evaluate(&[first.clone(), first, unexpected]);
    assert_eq!(
        report.status,
        ExperimentAcceptanceStatus::ConfigurationError
    );
    assert_eq!(report.missing_runs, vec![RunIdentity::from_record(&second)]);
    assert_eq!(report.duplicate_runs.len(), 1);
    assert_eq!(report.unexpected_runs.len(), 1);
}

#[test]
fn manifest_validation_rejects_duplicate_and_oversubscribed_run_specs() {
    let record = complete_record();
    let identity = RunIdentity::from_record(&record);
    let duplicate =
        ExperimentManifest::new("brick5f-acceptance", 6, vec![identity.clone(), identity]);
    assert!(duplicate.validate().is_err());

    let mut too_wide = RunIdentity::from_record(&record);
    too_wide.workers = 7;
    let oversubscribed = ExperimentManifest::new("brick5f-acceptance", 6, vec![too_wide]);
    assert!(oversubscribed.validate().is_err());
}

#[test]
fn jsonl_reader_and_state_digest_helper_are_stable() {
    let record = complete_record();
    let mut path = std::env::temp_dir();
    path.push(format!("acg-brick5f-{}.jsonl", std::process::id()));
    let bytes = [record.to_json_line().unwrap(), b"\n".to_vec()].concat();
    fs::write(&path, bytes).unwrap();
    let restored = read_records_jsonl(&path).unwrap();
    fs::remove_file(&path).unwrap();
    assert_eq!(restored, vec![record]);

    let equal = CorrectnessRecord::from_state_bytes(b"canonical", b"canonical");
    assert_eq!(equal.serial_equivalent, Some(true));
    assert_eq!(equal.canonical_state_digest, equal.serial_reference_digest);
    let different = CorrectnessRecord::from_state_bytes(b"canonical", b"serial");
    assert_eq!(different.serial_equivalent, Some(false));
    assert_ne!(
        different.canonical_state_digest,
        different.serial_reference_digest
    );
}

#[test]
fn smoke_policy_accepts_mechanism_records_without_publication_provenance() {
    let mut record = complete_record();
    record.metadata.started_at_utc = None;
    record.metadata.git_revision = None;
    record.metadata.build_profile = None;
    record.metadata.rustc_version = None;
    record.metadata.environment.clear();
    record.metadata.parameters.clear();
    record.parallelism.serial_equivalent_work_nanos = None;
    record.parallelism.serial_cost_dag_bound_nanos = None;
    record.parallelism.observed_service_dag_bound_nanos = None;
    record.parallelism.service_inflation_milli = None;
    record.parallelism.scheduler_realization_milli = None;
    record.correctness = CorrectnessRecord::default();

    let mut manifest = manifest_for(&record);
    manifest.policy = AcceptancePolicy::smoke();
    assert!(manifest.evaluate(&[record]).accepted());
}
