use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

use acg_benchmark_harness::{
    BenchmarkHarness, BenchmarkWorkload, ConflictLabWorkload, HarnessError, HarnessTuningConfig,
    WorkloadRegistry,
};
use acg_evaluation::{AcceptancePolicy, ExperimentManifest, ExperimentRecord, RunIdentity};

fn parameters() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("transactions".to_owned(), "16".to_owned()),
        ("warmup_blocks".to_owned(), "1".to_owned()),
        ("accounts".to_owned(), "1".to_owned()),
        ("hot_account_probability_bps".to_owned(), "10000".to_owned()),
        ("work_iterations".to_owned(), "100".to_owned()),
        ("acg.feedback_retention_factor".to_owned(), "1.0".to_owned()),
        ("acg.feedback_confidence_scale".to_owned(), "1.0".to_owned()),
    ])
}

fn run(mode: &str, run_index: u32) -> RunIdentity {
    RunIdentity {
        workload: "conflictlab".to_owned(),
        mode: mode.to_owned(),
        run_index,
        seed: 42,
        workers: 2,
        parameters: parameters(),
    }
}

fn smoke_manifest(runs: Vec<RunIdentity>) -> ExperimentManifest {
    let mut manifest = ExperimentManifest::new("common-harness-smoke", 6, runs);
    manifest.policy = AcceptancePolicy::smoke();
    manifest
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap()
}

#[test]
fn conflictlab_adapter_preparation_is_deterministic() {
    let workload = ConflictLabWorkload;
    let identity = run("cost-aware", 1);
    let left = workload.prepare(&identity).unwrap();
    let right = workload.prepare(&identity).unwrap();
    assert_eq!(left.warmup_blocks(), right.warmup_blocks());
    assert_eq!(left.measured_block(), right.measured_block());
    assert_eq!(
        left.canonical_state_bytes().unwrap(),
        right.canonical_state_bytes().unwrap()
    );
}

#[test]
fn harness_runs_static_probability_and_cost_aware_with_same_correctness_boundary() {
    let manifest = smoke_manifest(vec![
        run("static", 1),
        run("probability-only", 2),
        run("cost-aware", 3),
    ]);
    let harness = BenchmarkHarness::with_builtin_workloads(repo_root());
    let outcome = harness.run_manifest(&manifest).unwrap();
    assert!(outcome.acceptance.accepted());
    assert_eq!(outcome.records.len(), 3);

    for record in &outcome.records {
        assert_eq!(record.correctness.serial_equivalent, Some(true));
        assert_eq!(
            record.correctness.canonical_state_digest,
            record.correctness.serial_reference_digest
        );
        assert!(record.parallelism.serial_equivalent_work_nanos.is_some());
        assert!(record.parallelism.serial_cost_dag_bound_nanos.is_some());
        assert!(record
            .parallelism
            .observed_service_dag_bound_nanos
            .is_some());
        assert_eq!(record.metadata.parameters, parameters());
        assert_eq!(record.metadata.workers, 2);
        assert_eq!(record.metadata.physical_cores, 6);
        assert_eq!(
            record
                .metadata
                .environment
                .get("benchmark_adapter")
                .map(String::as_str),
            Some("conflictlab")
        );
    }

    let static_record = &outcome.records[0];
    assert_eq!(static_record.feedback.positive_observations, 0);
    assert_eq!(static_record.feedback.negative_observations, 0);
    assert_eq!(static_record.feedback.serialization_cost_observations, 0);
    assert_eq!(static_record.feedback.replay_impact_observations, 0);

    let probability = &outcome.records[1];
    assert!(probability.feedback.positive_observations > 0);
    assert_eq!(probability.feedback.serialization_cost_observations, 0);
    assert_eq!(probability.feedback.replay_impact_observations, 0);
    assert_eq!(probability.feedback.attributed_replay_cost_nanos, 0);

    let cost_aware = &outcome.records[2];
    assert!(cost_aware.feedback.positive_observations > 0);
    assert!(cost_aware.feedback.serialization_cost_observations > 0);
    assert!(cost_aware.feedback.attributed_serialization_cost_nanos > 0);
}

#[test]
fn manifest_runner_writes_stable_records_and_acceptance_files() {
    let manifest = smoke_manifest(vec![run("cost-aware", 1)]);
    let harness = BenchmarkHarness::with_builtin_workloads(repo_root());
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let output = std::env::temp_dir().join(format!(
        "acg-common-harness-{}-{unique}",
        std::process::id()
    ));
    let records = output.join("records.jsonl");
    let acceptance = output.join("acceptance.json");
    let outcome = harness
        .run_manifest_to_files(&manifest, &records, &acceptance)
        .unwrap();
    assert!(outcome.acceptance.accepted());

    let lines = fs::read_to_string(&records).unwrap();
    let parsed = lines
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| ExperimentRecord::from_json(line.as_bytes()).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(parsed, outcome.records);
    let report =
        acg_evaluation::ExperimentAcceptanceReport::from_json(&fs::read(&acceptance).unwrap())
            .unwrap();
    assert_eq!(report, outcome.acceptance);
    fs::remove_dir_all(output).unwrap();
}

#[test]
fn tuning_parameters_are_manifest_driven_and_invalid_values_fail_before_execution() {
    let mut values = parameters();
    values.insert("acg.soft_threshold".to_owned(), "0.33".to_owned());
    values.insert("acg.hard_threshold".to_owned(), "0.91".to_owned());
    values.insert("acg.risk_budget".to_owned(), "0.12".to_owned());
    values.insert("acg.max_wave_width".to_owned(), "4".to_owned());
    values.insert(
        "acg.serialization_cost_reference_nanos".to_owned(),
        "500000".to_owned(),
    );
    let tuning = HarnessTuningConfig::from_parameters(&values).unwrap();
    assert_eq!(tuning.planning_config.scheduler.soft_threshold, 0.33);
    assert_eq!(tuning.planning_config.scheduler.hard_threshold, 0.91);
    assert_eq!(tuning.planning_config.scheduler.risk_budget, 0.12);
    assert_eq!(tuning.planning_config.scheduler.max_wave_width, Some(4));
    assert_eq!(
        tuning
            .planning_config
            .cost_policy
            .serialization_cost_reference_nanos,
        500_000
    );

    values.insert("acg.hard_threshold".to_owned(), "not-a-number".to_owned());
    assert!(matches!(
        HarnessTuningConfig::from_parameters(&values),
        Err(HarnessError::Parameter { .. })
    ));

    values.remove("acg.hard_threshold");
    values.insert("acg.typo_threshold".to_owned(), "0.5".to_owned());
    assert!(matches!(
        HarnessTuningConfig::from_parameters(&values),
        Err(HarnessError::WorkloadParameter(_))
    ));

    let mut identity = run("cost-aware", 1);
    identity
        .parameters
        .insert("transactionz".to_owned(), "16".to_owned());
    assert!(matches!(
        ConflictLabWorkload.prepare(&identity),
        Err(HarnessError::WorkloadParameter(_))
    ));
}

#[test]
fn unknown_workload_and_worker_oversubscription_are_rejected() {
    let harness = BenchmarkHarness::new(WorkloadRegistry::new(), repo_root());
    let manifest = smoke_manifest(vec![run("cost-aware", 1)]);
    assert!(matches!(
        harness.run_manifest(&manifest),
        Err(HarnessError::UnknownWorkload(_))
    ));

    let mut oversubscribed = run("cost-aware", 1);
    oversubscribed.workers = 7;
    let manifest = smoke_manifest(vec![oversubscribed]);
    let harness = BenchmarkHarness::with_builtin_workloads(repo_root());
    // Manifest validation itself rejects this before workload execution.
    assert!(matches!(
        harness.run_manifest(&manifest),
        Err(HarnessError::Acceptance(_))
    ));
}

#[test]
fn conflictlab_simulation_and_complexity_knobs_change_block_without_changing_correctness() {
    let mut identity = run("cost-aware", 1);
    identity
        .parameters
        .insert("transactions".to_owned(), "20".to_owned());
    identity
        .parameters
        .insert("complexity".to_owned(), "storage-heavy".to_owned());
    identity
        .parameters
        .insert("work_iterations".to_owned(), "1000".to_owned());
    identity
        .parameters
        .insert("storage_rounds".to_owned(), "3".to_owned());
    identity
        .parameters
        .insert("payload_bytes".to_owned(), "128".to_owned());
    identity
        .parameters
        .insert("sim.admission_tps".to_owned(), "1000".to_owned());
    identity
        .parameters
        .insert("sim.block_interval_ms".to_owned(), "1000".to_owned());
    identity
        .parameters
        .insert("sim.block_size".to_owned(), "5".to_owned());
    identity
        .parameters
        .insert("sim.mempool_policy".to_owned(), "seeded-shuffle".to_owned());

    let prepared = ConflictLabWorkload.prepare(&identity).unwrap();
    assert_eq!(prepared.measured_block().transactions.len(), 5);

    let mut admission_limited = identity.clone();
    admission_limited
        .parameters
        .insert("sim.admission_tps".to_owned(), "2".to_owned());
    let limited = ConflictLabWorkload.prepare(&admission_limited).unwrap();
    assert_eq!(limited.measured_block().transactions.len(), 2);

    let manifest = smoke_manifest(vec![identity]);
    let harness = BenchmarkHarness::with_builtin_workloads(repo_root());
    let outcome = harness.run_manifest(&manifest).unwrap();
    assert!(outcome.acceptance.accepted());
    let record = &outcome.records[0];
    assert_eq!(record.execution.transactions, 5);
    assert_eq!(record.correctness.serial_equivalent, Some(true));
    assert_eq!(
        record
            .metadata
            .environment
            .get("conflictlab_backend")
            .map(String::as_str),
        Some("native")
    );
}
