use std::collections::BTreeMap;

use acg_evaluation::{
    CorrectnessRecord, ExecutionRecord, ExperimentMetadata, ExperimentRecord, FeedbackRecord,
    FeedbackTimingRecord, ParallelismRecord, PlanningRecord, SchedulingRecord,
    EXPERIMENT_RECORD_SCHEMA_VERSION,
};

fn record() -> ExperimentRecord {
    ExperimentRecord {
        schema_version: EXPERIMENT_RECORD_SCHEMA_VERSION,
        metadata: ExperimentMetadata {
            experiment_id: "brick5e-schema".to_owned(),
            workload: "conflictlab".to_owned(),
            mode: "adaptive".to_owned(),
            run_index: 2,
            seed: 42,
            workers: 6,
            physical_cores: 6,
            started_at_utc: Some("2026-08-13T20:00:00Z".to_owned()),
            git_revision: Some("deadbeef".to_owned()),
            build_profile: Some("release".to_owned()),
            rustc_version: Some("1.75.0".to_owned()),
            environment: BTreeMap::from([
                ("cpu_model".to_owned(), "reference-six-core".to_owned()),
                ("os".to_owned(), "linux".to_owned()),
            ]),
            parameters: BTreeMap::from([
                ("conflict_rate".to_owned(), "0.25".to_owned()),
                ("transactions".to_owned(), "200".to_owned()),
            ]),
        },
        planning: PlanningRecord {
            total_nanos: 1_000,
            ..PlanningRecord::default()
        },
        scheduling: SchedulingRecord {
            candidate_edges: 5,
            soft_edges: 3,
            hard_edges: 2,
            scheduling_risk_q16_sum: 123_456,
            ..SchedulingRecord::default()
        },
        parallelism: ParallelismRecord {
            serial_equivalent_work_nanos: Some(10_000),
            serial_cost_dag_bound_nanos: Some(4_000),
            observed_service_dag_bound_nanos: Some(5_000),
            actual_execution_wall_nanos: 5_500,
            service_inflation_milli: Some(1_250),
            scheduler_realization_milli: Some(1_100),
            ..ParallelismRecord::default()
        },
        execution: ExecutionRecord {
            transactions: 200,
            workers: 6,
            reused_results: 190,
            replayed_transactions: 10,
            ..ExecutionRecord::default()
        },
        feedback: FeedbackRecord {
            replay_impact_observations: 2,
            attributed_replay_cost_nanos: 1_500_000,
            serialization_cost_observations: 4,
            attributed_serialization_cost_nanos: 800_000,
            ..FeedbackRecord::default()
        },
        feedback_timing: FeedbackTimingRecord {
            pre_execution_update_nanos: 300,
            reconciliation_update_nanos: 200,
            total_nanos: 500,
        },
        correctness: CorrectnessRecord {
            canonical_state_digest: Some("abc".to_owned()),
            serial_reference_digest: Some("abc".to_owned()),
            serial_equivalent: Some(true),
        },
    }
}

#[test]
fn experiment_record_json_is_deterministic_and_round_trips() {
    let record = record();
    let first = record.to_json_line().unwrap();
    let second = record.to_json_line().unwrap();
    assert_eq!(first, second);
    assert_eq!(first.last(), Some(&b'\n'));

    let restored = ExperimentRecord::from_json(&first).unwrap();
    assert_eq!(restored, record);
}

#[test]
fn experiment_record_rejects_unknown_schema_version() {
    let mut value = serde_json::to_value(record()).unwrap();
    value["schema_version"] = serde_json::json!(99);
    let bytes = serde_json::to_vec(&value).unwrap();
    assert!(ExperimentRecord::from_json(&bytes).is_err());
}

#[test]
fn experiment_record_schema_v1_remains_readable() {
    let mut value = serde_json::to_value(record()).unwrap();
    value["schema_version"] = serde_json::json!(1);
    if let Some(scheduling) = value
        .get_mut("scheduling")
        .and_then(serde_json::Value::as_object_mut)
    {
        scheduling.remove("pre_reduction_dependencies");
        scheduling.remove("scheduled_dependencies");
        scheduling.remove("edges_elided_by_reduction");
    }
    if let Some(parallelism) = value
        .get_mut("parallelism")
        .and_then(serde_json::Value::as_object_mut)
    {
        parallelism.remove("observed_service_work_nanos");
        parallelism.remove("worker_capacity_bound_nanos");
        parallelism.remove("parallel_lower_bound_nanos");
        parallelism.remove("scheduler_realization_corrected_milli");
    }
    if let Some(feedback) = value
        .get_mut("feedback")
        .and_then(serde_json::Value::as_object_mut)
    {
        feedback.remove("observation_batches_applied");
        feedback.remove("serialization_cost_batches_applied");
    }
    let bytes = serde_json::to_vec(&value).unwrap();
    let restored = ExperimentRecord::from_json(&bytes).unwrap();
    assert_eq!(restored.schema_version, 1);
    assert_eq!(restored.scheduling.pre_reduction_dependencies, 0);
    assert_eq!(restored.parallelism.parallel_lower_bound_nanos, None);
    assert_eq!(restored.feedback.observation_batches_applied, 0);
}
