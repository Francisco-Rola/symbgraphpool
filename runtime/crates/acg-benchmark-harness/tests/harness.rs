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
        assert!(record.parallelism.observed_service_work_nanos.is_some());
        assert!(record.parallelism.worker_capacity_bound_nanos.is_some());
        let observed_dag = record.parallelism.observed_service_dag_bound_nanos.unwrap();
        let worker_bound = record.parallelism.worker_capacity_bound_nanos.unwrap();
        let parallel_bound = record.parallelism.parallel_lower_bound_nanos.unwrap();
        assert_eq!(parallel_bound, observed_dag.max(worker_bound));
        assert!(record
            .parallelism
            .scheduler_realization_corrected_milli
            .is_some());
        assert!(
            record.scheduling.pre_reduction_dependencies
                >= record.scheduling.scheduled_dependencies
        );
        assert_eq!(
            record.scheduling.pre_reduction_dependencies - record.scheduling.scheduled_dependencies,
            record.scheduling.edges_elided_by_reduction
        );
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
    assert!(cost_aware.feedback.serialization_cost_batches_applied > 0);
    assert!(
        cost_aware.feedback.serialization_cost_batches_applied
            <= cost_aware.feedback.serialization_cost_observations
    );
    assert!(cost_aware.feedback.attributed_serialization_cost_nanos > 0);
}

#[test]
fn compaction_reference_normalizes_adaptive_warmup_before_measured_toggle() {
    let mut dense = run("probability-only", 1);
    dense.seed = 101;
    dense.parameters.extend(BTreeMap::from([
        ("transactions".to_owned(), "64".to_owned()),
        ("warmup_blocks".to_owned(), "4".to_owned()),
        ("accounts".to_owned(), "64".to_owned()),
        ("hot_account_probability_bps".to_owned(), "2500".to_owned()),
        ("prediction_quality".to_owned(), "bucketed".to_owned()),
        ("prediction_buckets".to_owned(), "8".to_owned()),
        ("consensus_cutoff_ms".to_owned(), "5000".to_owned()),
        (
            "acg.compact_equivalence_groups".to_owned(),
            "false".to_owned(),
        ),
        (
            "acg.warmup_compact_equivalence_groups".to_owned(),
            "false".to_owned(),
        ),
        ("acg.warmup_workers".to_owned(), "1".to_owned()),
    ]));
    let mut compact = dense.clone();
    compact.run_index = 2;
    compact.parameters.insert(
        "acg.compact_equivalence_groups".to_owned(),
        "true".to_owned(),
    );

    let manifest = smoke_manifest(vec![dense, compact]);
    let harness = BenchmarkHarness::with_builtin_workloads(repo_root());
    let outcome = harness.run_manifest(&manifest).unwrap();
    assert_eq!(outcome.records.len(), 2);
    let dense = &outcome.records[0];
    let compact = &outcome.records[1];

    assert_eq!(dense.correctness.serial_equivalent, Some(true));
    assert_eq!(compact.correctness.serial_equivalent, Some(true));
    assert_eq!(
        dense.correctness.canonical_state_digest,
        compact.correctness.canonical_state_digest
    );
    assert_eq!(
        dense.scheduling.candidate_edges,
        compact.scheduling.candidate_edges
    );
    assert_eq!(dense.scheduling.low_edges, compact.scheduling.low_edges);
    assert_eq!(dense.scheduling.soft_edges, compact.scheduling.soft_edges);
    assert_eq!(dense.scheduling.hard_edges, compact.scheduling.hard_edges);
    assert_eq!(dense.scheduling.wave_count, compact.scheduling.wave_count);
    assert_eq!(
        dense.scheduling.max_wave_width,
        compact.scheduling.max_wave_width
    );
    assert_eq!(
        dense.scheduling.scheduled_dependencies,
        compact.scheduling.scheduled_dependencies
    );
    // The measured compact and dense runs start from the same deterministic single-worker
    // warm-up checkpoint, so their *pre-execution* planning/scheduling decisions above must
    // match. Do not require their post-execution feedback/posterior snapshots to match: the
    // measured block runs with the configured parallel worker count, and completion-order
    // differences can legitimately change adaptive feedback attribution even when the logical
    // candidate classes, READY-DAG, canonical result, and safety behavior are equivalent.
    // Same-report compact/dense feedback equivalence is covered deterministically in
    // acg-runtime-feedback's `compact_and_dense_aggregated_feedback_match_on_the_same_execution_report`.
    assert!(
        compact.scheduling.materialized_candidate_edges
            < dense.scheduling.materialized_candidate_edges
    );
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
    values.insert(
        "acg.compact_equivalence_groups".to_owned(),
        "false".to_owned(),
    );
    values.insert(
        "acg.warmup_compact_equivalence_groups".to_owned(),
        "true".to_owned(),
    );
    values.insert("acg.warmup_workers".to_owned(), "1".to_owned());
    values.insert("acg.soft_threshold".to_owned(), "0.33".to_owned());
    values.insert("acg.hard_threshold".to_owned(), "0.91".to_owned());
    values.insert("acg.risk_budget".to_owned(), "0.12".to_owned());
    values.insert("acg.max_wave_width".to_owned(), "4".to_owned());
    values.insert("acg.exploration_rate".to_owned(), "0.40".to_owned());
    values.insert(
        "acg.exploration_min_uncertainty".to_owned(),
        "0.45".to_owned(),
    );
    values.insert(
        "acg.exploration_max_transactions_per_block".to_owned(),
        "3".to_owned(),
    );
    values.insert(
        "acg.serial_bypass_service_cost_reference_nanos_per_transaction".to_owned(),
        "175000".to_owned(),
    );
    values.insert(
        "acg.serial_bypass_economics_ema_alpha".to_owned(),
        "0.25".to_owned(),
    );
    values.insert(
        "acg.serial_bypass_min_economics_observations".to_owned(),
        "3".to_owned(),
    );
    values.insert(
        "acg.serial_bypass_projected_speedup_hysteresis".to_owned(),
        "0.15".to_owned(),
    );
    values.insert(
        "acg.serial_bypass_max_consecutive_bypasses".to_owned(),
        "6".to_owned(),
    );
    values.insert(
        "acg.serial_bypass_buffered_preexecution".to_owned(),
        "true".to_owned(),
    );
    values.insert(
        "acg.regime_probation_bypass_blocks".to_owned(),
        "3".to_owned(),
    );
    values.insert(
        "acg.regime_probation_min_projected_speedup".to_owned(),
        "1.20".to_owned(),
    );
    values.insert(
        "acg.pre_consensus_serialization_weight".to_owned(),
        "0.75".to_owned(),
    );
    values.insert(
        "acg.post_consensus_replay_weight".to_owned(),
        "1.25".to_owned(),
    );
    values.insert("consensus_cutoff_ms".to_owned(), "500".to_owned());
    values.insert(
        "acg.serialization_cost_reference_nanos".to_owned(),
        "500000".to_owned(),
    );
    let tuning = HarnessTuningConfig::from_parameters(&values).unwrap();
    assert!(!tuning.planning_config.compact_equivalence_groups);
    assert_eq!(tuning.warmup_compact_equivalence_groups, Some(true));
    assert_eq!(tuning.warmup_workers, Some(1));
    assert_eq!(tuning.planning_config.scheduler.soft_threshold, 0.33);
    assert_eq!(tuning.planning_config.scheduler.hard_threshold, 0.91);
    assert_eq!(tuning.planning_config.scheduler.risk_budget, 0.12);
    assert_eq!(tuning.planning_config.scheduler.max_wave_width, Some(4));
    assert_eq!(tuning.planning_config.scheduler.exploration_rate, 0.40);
    assert_eq!(
        tuning.planning_config.scheduler.exploration_min_uncertainty,
        0.45
    );
    assert_eq!(
        tuning
            .planning_config
            .scheduler
            .exploration_max_transactions_per_block,
        3
    );
    assert_eq!(
        tuning
            .planning_config
            .serial_bypass
            .service_cost_reference_nanos_per_transaction,
        175_000
    );
    assert_eq!(
        tuning.planning_config.serial_bypass.economics_ema_alpha,
        0.25
    );
    assert_eq!(
        tuning
            .planning_config
            .serial_bypass
            .min_economics_observations,
        3
    );
    assert_eq!(
        tuning
            .planning_config
            .serial_bypass
            .projected_speedup_hysteresis,
        0.15
    );
    assert_eq!(
        tuning
            .planning_config
            .serial_bypass
            .max_consecutive_bypasses,
        6
    );
    assert!(tuning.planning_config.serial_bypass.buffered_preexecution);
    assert_eq!(
        tuning.planning_config.regime_change.probation_bypass_blocks,
        3
    );
    assert_eq!(
        tuning
            .planning_config
            .regime_change
            .probation_min_projected_speedup,
        1.20
    );
    assert_eq!(
        tuning
            .planning_config
            .cost_policy
            .pre_consensus_serialization_weight,
        0.75
    );
    assert_eq!(
        tuning
            .planning_config
            .cost_policy
            .post_consensus_replay_weight,
        1.25
    );
    assert_eq!(tuning.consensus_cutoff.as_millis(), 500);
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
fn non_credit_conflictlab_operation_mixes_require_real_wasm() {
    let mut identity = run("static", 99);
    identity
        .parameters
        .insert("operation_mix".to_owned(), "bank-mixed".to_owned());
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

#[test]
fn serial_bypass_skips_candidate_graph_after_losing_warmup_economics() {
    let mut bypass = run("cost-aware", 13);
    bypass
        .parameters
        .insert("transactions".to_owned(), "32".to_owned());
    bypass
        .parameters
        .insert("accounts".to_owned(), "32".to_owned());
    bypass
        .parameters
        .insert("warmup_blocks".to_owned(), "4".to_owned());
    bypass
        .parameters
        .insert("acg.serial_bypass_enabled".to_owned(), "true".to_owned());
    bypass.parameters.insert(
        "acg.serial_bypass_min_projected_speedup".to_owned(),
        "100.0".to_owned(),
    );

    let harness = BenchmarkHarness::with_builtin_workloads(repo_root());
    let outcome = harness.run_manifest(&smoke_manifest(vec![bypass])).unwrap();
    let record = &outcome.records[0];
    assert!(record.planning.serial_bypassed);
    assert!(record
        .planning
        .serial_bypass_projected_speedup_milli
        .is_some());
    assert!(record.planning.serial_bypass_mean_service_nanos.is_some());
    assert!(record
        .planning
        .serial_bypass_admission_score_milli
        .is_some());
    assert_eq!(record.planning.adapter_nanos, 0);
    assert_eq!(record.planning.candidate_graph_nanos, 0);
    assert_eq!(record.scheduling.candidate_edges, 0);
    assert_eq!(record.execution.dependency_count, 31);
    assert_eq!(record.execution.hard_dependency_count, 31);
    assert_eq!(record.execution.workers, 1);
    assert_eq!(record.execution.max_in_flight, 1);
    assert_eq!(record.execution.speculative_results, 0);
    assert_eq!(record.execution.reused_results, 0);
    assert_eq!(record.execution.canonical_transactions, 32);
    assert_eq!(record.consensus.prepared_receipts, 0);
    assert_eq!(record.pipeline_timing.preexecution_nanos, 0);
    assert_eq!(record.execution.preexecution_worker_wall_nanos, 0);
    assert!(record.execution.replay_or_missing_execution_nanos > 0);
    assert!(record.consensus.post_consensus_nanos > 0);
    assert_eq!(record.feedback.positive_observations, 0);
    assert_eq!(record.feedback.negative_observations, 0);
    assert_eq!(record.correctness.serial_equivalent, Some(true));
}

#[test]
fn buffered_serial_bypass_control_preserves_detached_prefix_semantics() {
    let mut bypass = run("cost-aware", 14);
    bypass
        .parameters
        .insert("transactions".to_owned(), "32".to_owned());
    bypass
        .parameters
        .insert("accounts".to_owned(), "32".to_owned());
    bypass
        .parameters
        .insert("warmup_blocks".to_owned(), "4".to_owned());
    bypass
        .parameters
        .insert("acg.serial_bypass_enabled".to_owned(), "true".to_owned());
    bypass.parameters.insert(
        "acg.serial_bypass_min_projected_speedup".to_owned(),
        "100.0".to_owned(),
    );
    bypass.parameters.insert(
        "acg.serial_bypass_buffered_preexecution".to_owned(),
        "true".to_owned(),
    );

    let harness = BenchmarkHarness::with_builtin_workloads(repo_root());
    let outcome = harness.run_manifest(&smoke_manifest(vec![bypass])).unwrap();
    let record = &outcome.records[0];
    assert!(record.planning.serial_bypassed);
    assert_eq!(record.execution.workers, 1);
    assert_eq!(record.execution.speculative_results, 32);
    assert_eq!(record.execution.reused_results, 32);
    assert_eq!(record.execution.canonical_transactions, 0);
    assert_eq!(record.consensus.prepared_receipts, 32);
    assert!(record.pipeline_timing.preexecution_nanos > 0);
    assert_eq!(record.correctness.serial_equivalent, Some(true));
}

#[test]
fn consensus_divergence_reconciles_candidate_preexecution_against_decided_order() {
    let mut identity = run("probability-only", 15);
    identity
        .parameters
        .insert("transactions".to_owned(), "32".to_owned());
    identity
        .parameters
        .insert("accounts".to_owned(), "32".to_owned());
    identity
        .parameters
        .insert("hot_account_probability_bps".to_owned(), "0".to_owned());
    identity
        .parameters
        .insert("parallelism_lanes".to_owned(), "32".to_owned());
    identity
        .parameters
        .insert("warmup_blocks".to_owned(), "1".to_owned());
    identity.parameters.insert(
        "consensus_divergence".to_owned(),
        "reorder-20pct".to_owned(),
    );
    identity
        .parameters
        .insert("consensus_cutoff_ms".to_owned(), "250".to_owned());

    let harness = BenchmarkHarness::with_builtin_workloads(repo_root());
    let outcome = harness
        .run_manifest(&smoke_manifest(vec![identity]))
        .unwrap();
    let record = &outcome.records[0];
    assert_eq!(record.correctness.serial_equivalent, Some(true));
    assert_eq!(record.consensus.cutoff_nanos, 250_000_000);
    assert_eq!(record.consensus.candidate_transactions, 32);
    assert_eq!(record.consensus.decided_transactions, 32);
    assert_eq!(record.consensus.shared_transactions, 32);
    assert!(record.consensus.same_position_transactions < 32);
    assert!(record.consensus.common_prefix_transactions < 32);

    // Transaction position is not a receipt-identity boundary. Environment-index-dependent
    // contracts are outside ConflictLab's reusable-receipt workload contract, so moved receipts
    // are matched by transaction/request and then accepted or replayed from their concrete read
    // dependencies.
    let moved_transactions = record
        .consensus
        .candidate_transactions
        .saturating_sub(record.consensus.same_position_transactions);
    assert!(moved_transactions > 0);
    assert_eq!(record.execution.discarded_predictions, 0);
    assert_eq!(record.execution.missing_predictions, 0);
    assert_eq!(record.execution.matched_transactions, 32);
    assert_eq!(record.execution.reused_results, 32);
    assert_eq!(record.execution.invalidated_results, 0);
    assert_eq!(record.execution.replayed_transactions, 0);
    assert!(record.execution.reused_results > record.consensus.same_position_transactions);
}

#[test]
fn consensus_tail_replacement_discards_predictions_and_executes_decided_transactions() {
    let mut identity = run("probability-only", 16);
    identity
        .parameters
        .insert("transactions".to_owned(), "32".to_owned());
    identity
        .parameters
        .insert("accounts".to_owned(), "16".to_owned());
    identity
        .parameters
        .insert("warmup_blocks".to_owned(), "1".to_owned());
    identity
        .parameters
        .insert("consensus_divergence".to_owned(), "tail-20pct".to_owned());
    identity
        .parameters
        .insert("consensus_cutoff_ms".to_owned(), "250".to_owned());

    let harness = BenchmarkHarness::with_builtin_workloads(repo_root());
    let outcome = harness
        .run_manifest(&smoke_manifest(vec![identity]))
        .unwrap();
    let record = &outcome.records[0];
    assert_eq!(record.correctness.serial_equivalent, Some(true));
    assert_eq!(record.consensus.candidate_transactions, 32);
    assert_eq!(record.consensus.decided_transactions, 32);
    assert!(record.consensus.shared_transactions < 32);
    assert_eq!(
        record.consensus.same_position_transactions,
        record.consensus.shared_transactions
    );
    assert_eq!(
        record.consensus.common_prefix_transactions,
        record.consensus.shared_transactions
    );
    assert!(record.execution.discarded_predictions > 0);
    assert!(record.execution.missing_predictions > 0);
}

#[test]
fn conflictlab_prediction_quality_modes_expose_soft_edges_and_runtime_misses() {
    let mut coarse = run("static", 10);
    coarse
        .parameters
        .insert("transactions".to_owned(), "12".to_owned());
    coarse
        .parameters
        .insert("accounts".to_owned(), "4".to_owned());
    coarse
        .parameters
        .insert("hot_account_probability_bps".to_owned(), "7500".to_owned());
    coarse
        .parameters
        .insert("prediction_quality".to_owned(), "coarse".to_owned());
    coarse
        .parameters
        .insert("acg.hard_threshold".to_owned(), "0.95".to_owned());
    coarse
        .parameters
        .insert("acg.risk_budget".to_owned(), "1.0".to_owned());

    let harness = BenchmarkHarness::with_builtin_workloads(repo_root());
    let coarse_outcome = harness.run_manifest(&smoke_manifest(vec![coarse])).unwrap();
    let coarse_record = &coarse_outcome.records[0];
    assert!(coarse_record.scheduling.soft_edges > 0);
    assert_eq!(
        coarse_record
            .metadata
            .environment
            .get("conflictlab_prediction_quality")
            .map(String::as_str),
        Some("coarse")
    );

    let mut bucketed = run("probability-only", 12);
    bucketed
        .parameters
        .insert("transactions".to_owned(), "32".to_owned());
    bucketed
        .parameters
        .insert("accounts".to_owned(), "32".to_owned());
    bucketed
        .parameters
        .insert("hot_account_probability_bps".to_owned(), "0".to_owned());
    bucketed
        .parameters
        .insert("warmup_blocks".to_owned(), "0".to_owned());
    bucketed
        .parameters
        .insert("prediction_quality".to_owned(), "bucketed".to_owned());
    bucketed
        .parameters
        .insert("prediction_buckets".to_owned(), "4".to_owned());
    bucketed
        .parameters
        .insert("acg.risk_budget".to_owned(), "0.90".to_owned());
    let bucketed_outcome = harness
        .run_manifest(&smoke_manifest(vec![bucketed]))
        .unwrap();
    let bucketed_record = &bucketed_outcome.records[0];
    let complete_pairs = 32 * 31 / 2;
    assert!(bucketed_record.scheduling.candidate_edges > 0);
    assert!(bucketed_record.scheduling.candidate_edges < complete_pairs);
    assert_eq!(
        bucketed_record
            .metadata
            .environment
            .get("conflictlab_prediction_quality")
            .map(String::as_str),
        Some("bucketed")
    );
    assert_eq!(
        bucketed_record
            .metadata
            .environment
            .get("conflictlab_prediction_buckets")
            .map(String::as_str),
        Some("4")
    );

    let mut opaque = run("cost-aware", 11);
    opaque
        .parameters
        .insert("transactions".to_owned(), "12".to_owned());
    opaque
        .parameters
        .insert("accounts".to_owned(), "2".to_owned());
    opaque
        .parameters
        .insert("hot_account_probability_bps".to_owned(), "10000".to_owned());
    opaque
        .parameters
        .insert("prediction_quality".to_owned(), "opaque".to_owned());
    opaque
        .parameters
        .insert("warmup_blocks".to_owned(), "0".to_owned());
    let opaque_outcome = harness.run_manifest(&smoke_manifest(vec![opaque])).unwrap();
    let opaque_record = &opaque_outcome.records[0];
    assert!(opaque_record.feedback.candidate_misses > 0);
    assert!(opaque_record.feedback.positive_observations > 0);
    assert_eq!(opaque_record.correctness.serial_equivalent, Some(true));
}

#[test]
fn compact_equivalence_planning_keeps_logical_candidate_coverage() {
    let mut identity = run("probability-only", 14);
    identity
        .parameters
        .insert("transactions".to_owned(), "32".to_owned());
    identity
        .parameters
        .insert("accounts".to_owned(), "1".to_owned());
    identity
        .parameters
        .insert("hot_account_probability_bps".to_owned(), "10000".to_owned());
    identity
        .parameters
        .insert("warmup_blocks".to_owned(), "0".to_owned());

    let harness = BenchmarkHarness::with_builtin_workloads(repo_root());
    let outcome = harness
        .run_manifest(&smoke_manifest(vec![identity]))
        .unwrap();
    let record = &outcome.records[0];
    assert_eq!(record.scheduling.candidate_edges, 32 * 31 / 2);
    assert_eq!(record.scheduling.materialized_candidate_edges, 31);
    assert_eq!(record.scheduling.scheduled_dependencies, 31);
    assert_eq!(record.feedback.candidate_misses, 0);
    assert_eq!(record.feedback.positive_observations, 32 * 31 / 2);
    assert_eq!(record.correctness.serial_equivalent, Some(true));
}

#[test]
fn mature_bucketed_soft_relationships_remain_compact_after_feedback() {
    let mut identity = run("probability-only", 16);
    identity
        .parameters
        .insert("transactions".to_owned(), "64".to_owned());
    identity
        .parameters
        .insert("accounts".to_owned(), "64".to_owned());
    identity
        .parameters
        .insert("hot_account_probability_bps".to_owned(), "0".to_owned());
    identity
        .parameters
        .insert("prediction_quality".to_owned(), "bucketed".to_owned());
    identity
        .parameters
        .insert("prediction_buckets".to_owned(), "4".to_owned());
    identity
        .parameters
        .insert("warmup_blocks".to_owned(), "4".to_owned());
    // Four bucketed warm-up blocks are intentionally sufficient to mature ordinary evidence.
    // The production confidence floor must not turn this long-standing compact-soft path hard;
    // regime-change decay is what drives confidence below the floor when evidence is stale.
    identity
        .parameters
        .insert("acg.softening_min_confidence".to_owned(), "0.20".to_owned());
    identity
        .parameters
        .insert("acg.hard_threshold".to_owned(), "0.99".to_owned());
    identity
        .parameters
        .insert("acg.risk_budget".to_owned(), "0.50".to_owned());

    let harness = BenchmarkHarness::with_builtin_workloads(repo_root());
    let outcome = harness
        .run_manifest(&smoke_manifest(vec![identity]))
        .unwrap();
    let record = &outcome.records[0];
    assert!(record.scheduling.soft_edges > 0);
    assert!(record.scheduling.candidate_edges > record.scheduling.materialized_candidate_edges);
    assert!(record.scheduling.materialized_candidate_edges <= 64);
    assert_eq!(record.feedback.candidate_misses, 0);
    assert_eq!(record.correctness.serial_equivalent, Some(true));
}

#[test]
fn conflictlab_mixed_complexity_is_deterministic_and_reported() {
    let mut identity = run("static", 15);
    identity
        .parameters
        .insert("transactions".to_owned(), "64".to_owned());
    identity
        .parameters
        .insert("accounts".to_owned(), "64".to_owned());
    identity
        .parameters
        .insert("complexity_mix".to_owned(), "33-34-33".to_owned());
    identity
        .parameters
        .insert("warmup_blocks".to_owned(), "0".to_owned());

    let left = ConflictLabWorkload.prepare(&identity).unwrap();
    let right = ConflictLabWorkload.prepare(&identity).unwrap();
    assert_eq!(left.measured_block(), right.measured_block());

    let harness = BenchmarkHarness::with_builtin_workloads(repo_root());
    let outcome = harness
        .run_manifest(&smoke_manifest(vec![identity]))
        .unwrap();
    let record = &outcome.records[0];
    assert_eq!(record.correctness.serial_equivalent, Some(true));
    assert_eq!(
        record
            .metadata
            .environment
            .get("conflictlab_complexity_mix")
            .map(String::as_str),
        Some("33-34-33")
    );
}

#[test]
fn common_harness_runs_serial_ariafb_vegeta_and_exact_access_baselines() {
    let mut runs = vec![
        run("serial", 101),
        run("aria-fb", 102),
        run("vegeta", 103),
        run("exact-access", 104),
    ];
    for identity in &mut runs {
        identity
            .parameters
            .insert("consensus_cutoff_ms".to_owned(), "5000".to_owned());
    }
    let harness = BenchmarkHarness::with_builtin_workloads(repo_root());
    let outcome = harness.run_manifest(&smoke_manifest(runs)).unwrap();
    assert_eq!(outcome.records.len(), 4);
    assert!(
        outcome.acceptance.accepted(),
        "baseline smoke acceptance failed: {:#?}",
        outcome.acceptance.run_reports
    );

    for record in &outcome.records {
        assert_eq!(record.correctness.serial_equivalent, Some(true));
        assert_eq!(
            record.correctness.canonical_state_digest,
            record.correctness.serial_reference_digest
        );
        assert!(record.strategy.is_some());
        assert_eq!(record.feedback.positive_observations, 0);
        assert_eq!(record.feedback.negative_observations, 0);
        assert!(record
            .pipeline_timing
            .serial_reference_execution_nanos
            .is_some());
    }

    let serial = &outcome.records[0];
    assert_eq!(serial.strategy.as_ref().unwrap().family, "serial");
    assert_eq!(serial.execution.workers, 1);
    assert_eq!(serial.execution.speculative_results, 0);

    let aria = &outcome.records[1];
    assert_eq!(aria.strategy.as_ref().unwrap().family, "aria-fb");
    assert!(!aria.strategy.as_ref().unwrap().pre_consensus_execution);
    assert!(aria.strategy.as_ref().unwrap().discovered_conflicts > 0);
    assert!(aria.strategy.as_ref().unwrap().forward_conflict_fallbacks > 0);
    assert_eq!(aria.consensus.prepared_receipts, 0);
    assert_eq!(aria.consensus.pre_consensus_nanos, 0);
    let aria_stage_total = aria
        .pipeline_timing
        .planning_nanos
        .saturating_add(aria.pipeline_timing.preexecution_nanos)
        .saturating_add(aria.pipeline_timing.reconciliation_nanos);
    assert!(aria.pipeline_timing.total_adaptive_block_nanos >= aria_stage_total);

    let vegeta = &outcome.records[2];
    assert_eq!(vegeta.strategy.as_ref().unwrap().family, "vegeta-like");
    assert!(vegeta.strategy.as_ref().unwrap().pre_consensus_execution);
    assert!(vegeta.strategy.as_ref().unwrap().discovered_conflicts > 0);
    assert!(vegeta.strategy.as_ref().unwrap().replay_dependencies > 0);

    let exact = &outcome.records[3];
    assert_eq!(exact.strategy.as_ref().unwrap().family, "exact-access");
    assert!(exact.strategy.as_ref().unwrap().oracle_accesses);
    assert_eq!(exact.execution.replayed_transactions, 0);
}

fn vegeta_fixture_parameters() -> BTreeMap<String, String> {
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/vegeta-s3-mini.jsonl")
        .canonicalize()
        .unwrap();
    BTreeMap::from([
        ("vegeta.corpus_path".to_owned(), fixture.display().to_string()),
        ("vegeta.measured_block".to_owned(), "4".to_owned()),
        ("warmup_blocks".to_owned(), "2".to_owned()),
        ("vegeta.prediction_history_blocks".to_owned(), "2".to_owned()),
        ("vegeta.prediction_min_frequency_bps".to_owned(), "1".to_owned()),
        ("vegeta.prediction_max_keys_per_method".to_owned(), "16".to_owned()),
        ("vegeta.work_step_divisor".to_owned(), "0".to_owned()),
        ("execution_backend".to_owned(), "native".to_owned()),
        ("consensus_cutoff_ms".to_owned(), "5000".to_owned()),
        ("acg.serial_bypass_enabled".to_owned(), "false".to_owned()),
        ("acg.regime_change_enabled".to_owned(), "false".to_owned()),
    ])
}

fn vegeta_run(mode: &str, run_index: u32) -> RunIdentity {
    RunIdentity {
        workload: "vegeta-eth".to_owned(),
        mode: mode.to_owned(),
        run_index,
        seed: 7,
        workers: 2,
        parameters: vegeta_fixture_parameters(),
    }
}

#[test]
fn vegeta_eth_workload_preparation_is_deterministic_and_hides_future_accesses() {
    let workload = acg_benchmark_harness::VegetaEthWorkload;
    let identity = vegeta_run("cost-aware", 1);
    let left = workload.prepare(&identity).unwrap();
    let right = workload.prepare(&identity).unwrap();
    assert_eq!(left.warmup_blocks(), right.warmup_blocks());
    assert_eq!(left.measured_block(), right.measured_block());
    assert_eq!(
        left.canonical_state_bytes().unwrap(),
        right.canonical_state_bytes().unwrap()
    );
    assert_eq!(left.measured_block().context.height, 4);
    assert_eq!(left.measured_block().transactions.len(), 3);
    assert_eq!(
        left.environment_metadata()
            .get("vegeta_access_semantics")
            .map(String::as_str),
        Some("evm-storage-sload-sstore-v1")
    );
}

#[test]
fn vegeta_eth_fixture_runs_all_seven_strategies_with_serial_equivalence() {
    let modes = [
        "serial",
        "aria-fb",
        "vegeta",
        "exact-access",
        "static",
        "probability-only",
        "cost-aware",
    ];
    let runs = modes
        .iter()
        .enumerate()
        .map(|(index, mode)| vegeta_run(mode, u32::try_from(index + 1).unwrap()))
        .collect::<Vec<_>>();
    let manifest = smoke_manifest(runs);
    let harness = BenchmarkHarness::with_builtin_workloads(repo_root());
    let outcome = harness.run_manifest(&manifest).unwrap();
    assert_eq!(outcome.records.len(), 7);
    assert!(outcome.acceptance.accepted());
    for record in outcome.records {
        assert_eq!(record.metadata.workload, "vegeta-eth");
        assert_eq!(record.correctness.serial_equivalent, Some(true));
        assert_eq!(
            record.correctness.canonical_state_digest,
            record.correctness.serial_reference_digest
        );
    }
}
