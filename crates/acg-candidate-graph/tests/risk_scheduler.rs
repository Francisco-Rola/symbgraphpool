use acg_candidate_graph::{
    CandidateGraphBuilder, CandidateTransaction, EdgeClass, RiskBoundedScheduler,
    RiskBoundedSchedulerConfig, WeightedCandidateGraphConfig,
};
use acg_core::{ContractCodeHash, InstanceId, RuntimeId, TxId, TxIndex};
use acg_feedback::{
    AdaptiveFeedbackConfig, AdaptiveFeedbackStore, ConflictObservation, ObservationBuffer,
    ObservationSource, ObservationTarget,
};
use acg_predicate::InputBindings;
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use serde_json::json;

const CONFLICTLAB: &[u8] = include_bytes!("../../../benchmarks/symbolic/conflictlab.symbolic.json");

fn graph() -> ProfileGraph {
    let raw = parse_slice(CONFLICTLAB).unwrap();
    let context = IngestionContext::new(
        RuntimeId::new("cosmwasm").unwrap(),
        ContractCodeHash([9; 32]),
        1,
    );
    let profiles = normalize_document(raw, &context).unwrap();
    let artifact = ProfileGraphArtifact::compile(profiles, &EdgeBuildConfig::default()).unwrap();
    ProfileGraph::load(artifact, GraphLoadConfig::default()).unwrap()
}

fn profile_id(graph: &ProfileGraph, entrypoint: &str) -> acg_core::ProfileId {
    graph
        .profiles()
        .iter()
        .find(|profile| profile.definition.entrypoint_name == entrypoint)
        .unwrap()
        .id
}

fn candidate(
    graph: &ProfileGraph,
    id: u64,
    position: u32,
    entrypoint: &str,
    instance: u32,
    bindings: serde_json::Value,
) -> CandidateTransaction {
    CandidateTransaction {
        tx_id: TxId(id),
        predicted_position: position,
        inclusion_probability: 1.0,
        profile_id: profile_id(graph, entrypoint),
        instance_id: InstanceId(instance),
        input_bindings: InputBindings::from_value(bindings),
        estimated_execution_cost: 1,
    }
}

fn feedback_config() -> AdaptiveFeedbackConfig {
    AdaptiveFeedbackConfig {
        retention_factor: 1.0,
        ..AdaptiveFeedbackConfig::default()
    }
}

fn build_weighted(
    graph: &ProfileGraph,
    store: &AdaptiveFeedbackStore,
    feedback_config: &AdaptiveFeedbackConfig,
) -> acg_candidate_graph::CandidateGraph {
    CandidateGraphBuilder::new(graph)
        .build_weighted(
            vec![
                candidate(
                    graph,
                    1,
                    0,
                    "execute::Credit",
                    1,
                    json!({"account":"alice"}),
                ),
                candidate(
                    graph,
                    2,
                    1,
                    "execute::Credit",
                    1,
                    json!({"account":"alice"}),
                ),
                candidate(
                    graph,
                    3,
                    2,
                    "execute::IncrementCounter",
                    1,
                    json!({"shard_id":7}),
                ),
            ],
            store,
            feedback_config,
            WeightedCandidateGraphConfig {
                epoch: 1,
                edge_materialization_threshold: 0.0,
            },
        )
        .unwrap()
}

#[test]
fn weighted_candidate_edges_drive_hard_wave_dependencies_through_the_public_api() {
    let graph = graph();
    let store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let feedback_config = feedback_config();
    let candidate_graph = build_weighted(&graph, &store, &feedback_config);
    let credit_edge = candidate_graph
        .edge_between(TxIndex(0), TxIndex(1))
        .unwrap();
    assert!(candidate_graph
        .edge_between(TxIndex(0), TxIndex(2))
        .is_none());

    let probability = credit_edge.probability();
    let scheduler = RiskBoundedScheduler::new(RiskBoundedSchedulerConfig {
        soft_threshold: probability,
        hard_threshold: probability,
        risk_budget: 0.0,
        max_wave_width: None,
        independent_observations_before_softening: 8,
    })
    .unwrap();
    assert_eq!(scheduler.classify_edge(credit_edge), EdgeClass::Hard);

    let schedule = scheduler.schedule(&candidate_graph).unwrap();
    assert_eq!(schedule.wave_for(TxIndex(0)), Some(0));
    assert_eq!(schedule.wave_for(TxIndex(1)), Some(1));
    assert_eq!(schedule.wave_for(TxIndex(2)), Some(0));
    schedule
        .validate_against(&candidate_graph, scheduler.config())
        .unwrap();
}

#[test]
fn learned_negative_evidence_can_change_a_future_pair_from_hard_to_soft() {
    let graph = graph();
    let credit = profile_id(&graph, "execute::Credit");
    let edge_index = graph.edge_between_profiles(credit, credit).unwrap();
    let feedback_config = feedback_config();
    let mut store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();

    let before_graph = build_weighted(&graph, &store, &feedback_config);
    let before_probability = before_graph
        .edge_between(TxIndex(0), TxIndex(1))
        .unwrap()
        .probability();

    let mut observations = ObservationBuffer::default();
    for offset in 0..12u64 {
        observations.push(
            ConflictObservation::independent(
                credit,
                credit,
                TxId(100 + offset * 2),
                TxId(101 + offset * 2),
                ObservationSource::CanonicalExecution,
                ObservationTarget::Static { edge_index },
                1.0,
                1,
                true,
            )
            .unwrap(),
        );
    }
    store
        .apply_batch(&graph, observations, &feedback_config)
        .unwrap();

    let after_graph = build_weighted(&graph, &store, &feedback_config);
    let after_probability = after_graph
        .edge_between(TxIndex(0), TxIndex(1))
        .unwrap()
        .probability();
    assert!(after_probability < before_probability);

    let threshold = (before_probability + after_probability) / 2.0;
    let scheduler = RiskBoundedScheduler::new(RiskBoundedSchedulerConfig {
        soft_threshold: 0.0,
        hard_threshold: threshold,
        risk_budget: 1.0,
        max_wave_width: None,
        independent_observations_before_softening: 8,
    })
    .unwrap();

    assert_eq!(
        scheduler.classify_edge(before_graph.edge_between(TxIndex(0), TxIndex(1)).unwrap()),
        EdgeClass::Hard
    );
    assert_eq!(
        scheduler.classify_edge(after_graph.edge_between(TxIndex(0), TxIndex(1)).unwrap()),
        EdgeClass::Soft
    );

    // A proven symbolic relationship is softened by concrete evidence, not deleted by the generic
    // materialization floor.
    let retained_after_softening = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            vec![
                candidate(
                    &graph,
                    1,
                    0,
                    "execute::Credit",
                    1,
                    json!({"account":"alice"}),
                ),
                candidate(
                    &graph,
                    2,
                    1,
                    "execute::Credit",
                    1,
                    json!({"account":"alice"}),
                ),
            ],
            &store,
            &feedback_config,
            WeightedCandidateGraphConfig {
                epoch: 1,
                edge_materialization_threshold: threshold,
            },
        )
        .unwrap();
    let retained_edge = retained_after_softening
        .edge_between(TxIndex(0), TxIndex(1))
        .unwrap();
    assert_eq!(scheduler.classify_edge(retained_edge), EdgeClass::Soft);

    let before_schedule = scheduler.schedule(&before_graph).unwrap();
    let after_schedule = scheduler.schedule(&after_graph).unwrap();
    assert_ne!(
        before_schedule.wave_for(TxIndex(0)),
        before_schedule.wave_for(TxIndex(1))
    );
    assert_eq!(
        after_schedule.wave_for(TxIndex(0)),
        after_schedule.wave_for(TxIndex(1))
    );
}
