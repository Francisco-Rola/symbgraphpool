use acg_candidate_graph::{
    CandidateGraphBuilder, CandidateGraphError, CandidateTransaction, EdgeProvenance,
    WeightedCandidateGraphConfig,
};
use acg_core::{ConflictKinds, ContractCodeHash, InstanceId, RuntimeId, TxId, TxIndex};
use acg_feedback::{
    AdaptiveFeedbackConfig, AdaptiveFeedbackStore, ConflictObservation, ObservationBuffer,
    ObservationSource, ObservationTarget,
};
use acg_predicate::{InputBindings, PredicateResult};
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use serde_json::json;

const CONFLICTLAB: &[u8] = include_bytes!("../../../benchmarks/symbolic/conflictlab.symbolic.json");
const Q16_EPSILON: f64 = 1.0 / u16::MAX as f64;

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

fn static_edge(graph: &ProfileGraph, left: &str, right: &str) -> acg_core::ProfileEdgeIndex {
    graph
        .edge_between_profiles(profile_id(graph, left), profile_id(graph, right))
        .unwrap()
}

fn tx(
    graph: &ProfileGraph,
    id: u64,
    entrypoint: &str,
    instance: u32,
    bindings: serde_json::Value,
) -> CandidateTransaction {
    CandidateTransaction {
        tx_id: TxId(id),
        predicted_position: id as u32,
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

fn weighted_config(epoch: u64, threshold: f64) -> WeightedCandidateGraphConfig {
    WeightedCandidateGraphConfig {
        epoch,
        edge_materialization_threshold: threshold,
    }
}

#[test]
fn weighted_static_edge_carries_posterior_confidence_kinds_and_provenance() {
    let graph = graph();
    let edge_index = static_edge(&graph, "execute::Credit", "execute::Credit");
    let store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let feedback_config = feedback_config();
    let estimate = store
        .estimate_static_edge(edge_index, 0, &feedback_config)
        .unwrap();

    let candidate = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            vec![
                tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
                tx(&graph, 2, "execute::Credit", 1, json!({"account":"alice"})),
            ],
            &store,
            &feedback_config,
            weighted_config(0, 0.0),
        )
        .unwrap();

    let edge = candidate.edge_between(TxIndex(0), TxIndex(1)).unwrap();
    assert_eq!(
        edge.provenance,
        EdgeProvenance::Static {
            profile_edge_index: edge_index
        }
    );
    assert_eq!(edge.profile_edge_index(), Some(edge_index));
    assert_eq!(edge.runtime_edge_id(), None);
    assert_eq!(edge.predicate_result, PredicateResult::True);
    assert_eq!(
        edge.conflict_kinds,
        graph.edges()[edge_index.0 as usize].conflict_kinds
    );
    assert!((edge.probability() - estimate.probability).abs() <= Q16_EPSILON);
    assert!((edge.confidence() - estimate.confidence).abs() <= Q16_EPSILON);
    assert!(!edge.is_historical_override());
}

#[test]
fn weighted_materialization_threshold_is_inclusive_and_uses_unquantized_posterior() {
    let graph = graph();
    let edge_index = static_edge(&graph, "execute::Credit", "execute::Credit");
    let store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let feedback_config = feedback_config();
    let probability = store
        .estimate_static_edge(edge_index, 0, &feedback_config)
        .unwrap()
        .probability;
    let transactions = || {
        vec![
            tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
            tx(&graph, 2, "execute::Credit", 1, json!({"account":"alice"})),
        ]
    };

    let at_boundary = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            transactions(),
            &store,
            &feedback_config,
            weighted_config(0, probability),
        )
        .unwrap();
    assert_eq!(at_boundary.edges().len(), 1);

    let above = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            transactions(),
            &store,
            &feedback_config,
            weighted_config(0, probability + (1.0 - probability) / 2.0),
        )
        .unwrap();
    assert!(above.edges().is_empty());
}

#[test]
fn weighted_unknown_predicate_is_materialized_with_the_adaptive_posterior() {
    let graph = graph();
    let edge_index = static_edge(&graph, "execute::ResetAllBalances", "execute::Credit");
    let store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let feedback_config = feedback_config();
    let estimate = store
        .estimate_static_edge(edge_index, 0, &feedback_config)
        .unwrap();

    let candidate = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            vec![
                tx(
                    &graph,
                    1,
                    "execute::ResetAllBalances",
                    1,
                    json!({"info":{"sender":"admin"}}),
                ),
                tx(&graph, 2, "execute::Credit", 1, json!({"account":"alice"})),
            ],
            &store,
            &feedback_config,
            weighted_config(0, 0.0),
        )
        .unwrap();

    let edge = candidate.edge_between(TxIndex(0), TxIndex(1)).unwrap();
    assert_eq!(edge.predicate_result, PredicateResult::Unknown);
    assert_eq!(edge.profile_edge_index(), Some(edge_index));
    assert!((edge.probability() - estimate.probability).abs() <= Q16_EPSILON);
    assert!(!edge.is_historical_override());
}

#[test]
fn false_predicate_remains_pruned_without_concrete_candidate_miss_history() {
    let graph = graph();
    let store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let feedback_config = feedback_config();
    let candidate = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            vec![
                tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
                tx(&graph, 2, "execute::Credit", 1, json!({"account":"bob"})),
            ],
            &store,
            &feedback_config,
            weighted_config(0, 0.0),
        )
        .unwrap();
    assert!(candidate.edges().is_empty());
}

#[test]
fn concrete_candidate_miss_overrides_false_predicate_and_same_instance_fast_path() {
    let graph = graph();
    let credit = profile_id(&graph, "execute::Credit");
    let edge_index = static_edge(&graph, "execute::Credit", "execute::Credit");
    let feedback_config = feedback_config();
    let mut store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let mut observations = ObservationBuffer::default();
    observations.push(
        ConflictObservation::conflict(
            credit,
            credit,
            TxId(100),
            TxId(101),
            ConflictKinds::WRITE_WRITE,
            ObservationSource::CanonicalExecution,
            ObservationTarget::Static { edge_index },
            3.0,
            1,
            false,
        )
        .unwrap(),
    );
    store
        .apply_batch(&graph, observations, &feedback_config)
        .unwrap();
    let estimate = store
        .estimate_static_edge(edge_index, 1, &feedback_config)
        .unwrap();
    assert!(estimate.has_candidate_miss_history());

    // Different keys and different instances would both be eliminated by the symbolic fast path
    // before a concrete miss had disproved absolute pruning for this profile relationship.
    let candidate = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            vec![
                tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
                tx(&graph, 2, "execute::Credit", 2, json!({"account":"bob"})),
            ],
            &store,
            &feedback_config,
            weighted_config(1, 0.0),
        )
        .unwrap();

    let edge = candidate.edge_between(TxIndex(0), TxIndex(1)).unwrap();
    assert_eq!(edge.predicate_result, PredicateResult::False);
    assert!(edge.is_historical_override());
    assert_eq!(edge.profile_edge_index(), Some(edge_index));
    assert!((edge.probability() - estimate.probability).abs() <= Q16_EPSILON);
}

#[test]
fn runtime_discovered_fallback_becomes_a_weighted_future_candidate_edge() {
    let graph = graph();
    let credit = profile_id(&graph, "execute::Credit");
    let counter = profile_id(&graph, "execute::IncrementCounter");
    assert!(graph.edge_between_profiles(credit, counter).is_none());
    let feedback_config = feedback_config();
    let mut store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let mut observations = ObservationBuffer::default();
    observations.push(
        ConflictObservation::conflict(
            credit,
            counter,
            TxId(100),
            TxId(101),
            ConflictKinds::READ_WRITE | ConflictKinds::WRITE_WRITE,
            ObservationSource::CanonicalExecution,
            ObservationTarget::RuntimeDiscovered,
            3.0,
            1,
            false,
        )
        .unwrap(),
    );
    store
        .apply_batch(&graph, observations, &feedback_config)
        .unwrap();
    let fallback = store.fallback_edge(credit, counter).unwrap();
    let estimate = store
        .estimate_fallback_edge(fallback.id, 1, &feedback_config)
        .unwrap();

    let transactions = || {
        vec![
            tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
            tx(
                &graph,
                2,
                "execute::IncrementCounter",
                9,
                json!({"shard_id":7}),
            ),
        ]
    };
    let candidate = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            transactions(),
            &store,
            &feedback_config,
            weighted_config(1, 0.0),
        )
        .unwrap();
    let edge = candidate.edge_between(TxIndex(0), TxIndex(1)).unwrap();
    assert_eq!(edge.profile_edge_index(), None);
    assert_eq!(edge.runtime_edge_id(), Some(fallback.id));
    assert_eq!(edge.predicate_result, PredicateResult::Unknown);
    assert_eq!(edge.conflict_kinds, fallback.conflict_kinds);
    assert!((edge.probability() - estimate.probability).abs() <= Q16_EPSILON);
    assert!((edge.confidence() - estimate.confidence).abs() <= Q16_EPSILON);

    let pruned = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            transactions(),
            &store,
            &feedback_config,
            weighted_config(1, estimate.probability + (1.0 - estimate.probability) / 2.0),
        )
        .unwrap();
    assert!(pruned.edges().is_empty());
}

#[test]
fn weighted_build_projects_decay_without_mutating_feedback_state() {
    let graph = graph();
    let edge_index = static_edge(&graph, "execute::Credit", "execute::Credit");
    let store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let config = AdaptiveFeedbackConfig {
        retention_factor: 0.5,
        ..AdaptiveFeedbackConfig::default()
    };
    let before = *store.static_statistics(edge_index).unwrap();
    let now = store.estimate_static_edge(edge_index, 0, &config).unwrap();

    let candidate = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            vec![
                tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
                tx(&graph, 2, "execute::Credit", 1, json!({"account":"alice"})),
            ],
            &store,
            &config,
            weighted_config(5, 0.0),
        )
        .unwrap();
    let edge = &candidate.edges()[0];
    assert!(edge.confidence() < now.confidence);
    assert_eq!(*store.static_statistics(edge_index).unwrap(), before);
}

#[test]
fn legacy_binary_builder_preserves_pre_brick4_behavior() {
    let graph = graph();
    let edge_index = static_edge(&graph, "execute::Credit", "execute::Credit");
    let candidate = CandidateGraphBuilder::new(&graph)
        .build(vec![
            tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
            tx(&graph, 2, "execute::Credit", 1, json!({"account":"alice"})),
        ])
        .unwrap();
    let edge = &candidate.edges()[0];
    assert_eq!(edge.profile_edge_index(), Some(edge_index));
    assert_eq!(edge.probability(), 1.0);
    assert_eq!(edge.confidence(), 0.0);
}

#[test]
fn invalid_weighted_materialization_threshold_is_rejected() {
    let graph = graph();
    let store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let feedback_config = feedback_config();
    let transactions = vec![
        tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
        tx(&graph, 2, "execute::Credit", 1, json!({"account":"alice"})),
    ];

    let error = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            transactions,
            &store,
            &feedback_config,
            weighted_config(0, 1.1),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        CandidateGraphError::InvalidMaterializationThreshold(value) if value == 1.1
    ));

    let error = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            vec![
                tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
                tx(&graph, 2, "execute::Credit", 1, json!({"account":"alice"})),
            ],
            &store,
            &feedback_config,
            weighted_config(0, f64::NAN),
        )
        .unwrap_err();
    assert!(matches!(
        error,
        CandidateGraphError::InvalidMaterializationThreshold(value) if value.is_nan()
    ));
}
