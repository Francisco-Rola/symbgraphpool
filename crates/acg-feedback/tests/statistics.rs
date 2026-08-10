use acg_core::{ConflictKinds, ContractCodeHash, RuntimeId, TxId};
use acg_feedback::{
    AdaptiveFeedbackConfig, AdaptiveFeedbackStore, ConflictObservation, FeedbackCheckpoint,
    ObservationBuffer, ObservationSource, ObservationTarget,
};
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};

const CONFLICTLAB: &[u8] = include_bytes!("../../../benchmarks/symbolic/conflictlab.symbolic.json");

fn graph() -> ProfileGraph {
    let raw = parse_slice(CONFLICTLAB).unwrap();
    let context = IngestionContext::new(
        RuntimeId::new("cosmwasm").unwrap(),
        ContractCodeHash([3; 32]),
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

#[test]
fn positive_and_negative_evidence_update_beta_posterior() {
    let graph = graph();
    let edge = static_edge(&graph, "execute::Credit", "execute::Credit");
    let credit = profile_id(&graph, "execute::Credit");
    let mut store = AdaptiveFeedbackStore::from_graph(&graph, 10).unwrap();
    let initial = *store.static_statistics(edge).unwrap();

    let mut positive = ObservationBuffer::default();
    positive.push(
        ConflictObservation::conflict(
            credit,
            credit,
            TxId(1),
            TxId(2),
            ConflictKinds::WRITE_WRITE,
            ObservationSource::CanonicalExecution,
            ObservationTarget::Static { edge_index: edge },
            3.0,
            10,
            true,
        )
        .unwrap(),
    );
    let summary = store
        .apply_batch(&graph, positive, &AdaptiveFeedbackConfig::default())
        .unwrap();
    let after_positive = *store.static_statistics(edge).unwrap();
    assert_eq!(summary.positive_observations, 1);
    assert_eq!(summary.negative_observations, 0);
    assert!(after_positive.probability() > initial.probability());
    assert_eq!(after_positive.positive_observations, 1);

    let mut negative = ObservationBuffer::default();
    negative.push(
        ConflictObservation::independent(
            credit,
            credit,
            TxId(3),
            TxId(4),
            ObservationSource::CanonicalExecution,
            ObservationTarget::Static { edge_index: edge },
            3.0,
            10,
            true,
        )
        .unwrap(),
    );
    store
        .apply_batch(&graph, negative, &AdaptiveFeedbackConfig::default())
        .unwrap();
    let after_negative = store.static_statistics(edge).unwrap();
    assert!(after_negative.probability() < after_positive.probability());
    assert_eq!(after_negative.negative_observations, 1);
}

#[test]
fn decay_reduces_confidence_without_changing_mean_before_new_evidence() {
    let graph = graph();
    let edge = static_edge(&graph, "execute::Credit", "execute::Credit");
    let credit = profile_id(&graph, "execute::Credit");
    let mut store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let config = AdaptiveFeedbackConfig {
        retention_factor: 0.5,
        ..AdaptiveFeedbackConfig::default()
    };
    let before = *store.static_statistics(edge).unwrap();
    let before_confidence = before.confidence(config.confidence_scale).unwrap();

    let mut buffer = ObservationBuffer::default();
    buffer.push(
        ConflictObservation::independent(
            credit,
            credit,
            TxId(1),
            TxId(2),
            ObservationSource::CanonicalExecution,
            ObservationTarget::Static { edge_index: edge },
            0.000_001,
            4,
            true,
        )
        .unwrap(),
    );
    store.apply_batch(&graph, buffer, &config).unwrap();
    let after = store.static_statistics(edge).unwrap();

    // The tiny observation perturbs the mean only negligibly; four epochs halve the prior four times.
    assert!((after.probability() - before.probability()).abs() < 0.000_01);
    assert!(after.confidence(config.confidence_scale).unwrap() < before_confidence);
    assert_eq!(after.last_update_epoch, 4);
}

#[test]
fn batch_is_applied_in_epoch_order_even_when_buffer_is_out_of_order() {
    let graph = graph();
    let edge = static_edge(&graph, "execute::Credit", "execute::Credit");
    let credit = profile_id(&graph, "execute::Credit");
    let mut store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let mut buffer = ObservationBuffer::default();
    for epoch in [5, 2, 4, 3] {
        buffer.push(
            ConflictObservation::conflict(
                credit,
                credit,
                TxId(epoch),
                TxId(epoch + 100),
                ConflictKinds::WRITE_WRITE,
                ObservationSource::CanonicalExecution,
                ObservationTarget::Static { edge_index: edge },
                1.0,
                epoch,
                true,
            )
            .unwrap(),
        );
    }
    store
        .apply_batch(&graph, buffer, &AdaptiveFeedbackConfig::default())
        .unwrap();
    let stats = store.static_statistics(edge).unwrap();
    assert_eq!(stats.last_update_epoch, 5);
    assert_eq!(stats.positive_observations, 4);
}

#[test]
fn an_empty_buffer_is_not_negative_evidence() {
    let graph = graph();
    let edge = static_edge(&graph, "execute::Credit", "execute::Credit");
    let mut store = AdaptiveFeedbackStore::from_graph(&graph, 7).unwrap();
    let before = *store.static_statistics(edge).unwrap();
    let summary = store
        .apply_batch(
            &graph,
            ObservationBuffer::default(),
            &AdaptiveFeedbackConfig::default(),
        )
        .unwrap();
    assert_eq!(summary.positive_observations, 0);
    assert_eq!(*store.static_statistics(edge).unwrap(), before);
}

#[test]
fn runtime_miss_creates_reviewable_fallback_and_later_negative_updates_it() {
    let graph = graph();
    let credit = profile_id(&graph, "execute::Credit");
    let counter = profile_id(&graph, "execute::IncrementCounter");
    assert!(graph.edge_between_profiles(credit, counter).is_none());
    let mut store = AdaptiveFeedbackStore::from_graph(&graph, 1).unwrap();
    let config = AdaptiveFeedbackConfig::default();

    let mut miss = ObservationBuffer::default();
    miss.push(
        ConflictObservation::conflict(
            credit,
            counter,
            TxId(1),
            TxId(2),
            ConflictKinds::WRITE_WRITE,
            ObservationSource::CanonicalExecution,
            ObservationTarget::RuntimeDiscovered,
            3.0,
            2,
            false,
        )
        .unwrap(),
    );
    let summary = store.apply_batch(&graph, miss, &config).unwrap();
    assert_eq!(summary.fallback_edges_created, 1);
    assert_eq!(summary.candidate_misses, 1);
    let fallback = store.fallback_edge(credit, counter).unwrap();
    assert!(fallback.review_required);
    assert!(fallback.conflict_kinds.contains(ConflictKinds::WRITE_WRITE));
    let after_positive = fallback.statistics.probability();

    let mut second_conflict = ObservationBuffer::default();
    second_conflict.push(
        ConflictObservation::conflict(
            credit,
            counter,
            TxId(5),
            TxId(6),
            ConflictKinds::READ_WRITE,
            ObservationSource::Validation,
            ObservationTarget::RuntimeDiscovered,
            2.0,
            2,
            false,
        )
        .unwrap(),
    );
    store.apply_batch(&graph, second_conflict, &config).unwrap();
    let fallback = store.fallback_edge(credit, counter).unwrap();
    assert!(fallback.conflict_kinds.contains(ConflictKinds::WRITE_WRITE));
    assert!(fallback.conflict_kinds.contains(ConflictKinds::READ_WRITE));

    let mut independent = ObservationBuffer::default();
    independent.push(
        ConflictObservation::independent(
            credit,
            counter,
            TxId(3),
            TxId(4),
            ObservationSource::CanonicalExecution,
            ObservationTarget::RuntimeDiscovered,
            3.0,
            3,
            false,
        )
        .unwrap(),
    );
    store.apply_batch(&graph, independent, &config).unwrap();
    let fallback = store.fallback_edge(credit, counter).unwrap();
    assert_eq!(fallback.statistics.negative_observations, 1);
    assert!(fallback.statistics.probability() < after_positive);
}

#[test]
fn checkpoint_round_trip_preserves_static_and_runtime_discovered_statistics() {
    let graph = graph();
    let credit = profile_id(&graph, "execute::Credit");
    let counter = profile_id(&graph, "execute::IncrementCounter");
    let edge = static_edge(&graph, "execute::Credit", "execute::Credit");
    let mut store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let config = AdaptiveFeedbackConfig::default();
    let mut buffer = ObservationBuffer::default();
    buffer.push(
        ConflictObservation::conflict(
            credit,
            credit,
            TxId(1),
            TxId(2),
            ConflictKinds::WRITE_WRITE,
            ObservationSource::Validation,
            ObservationTarget::Static { edge_index: edge },
            2.0,
            4,
            true,
        )
        .unwrap(),
    );
    buffer.push(
        ConflictObservation::conflict(
            credit,
            counter,
            TxId(3),
            TxId(4),
            ConflictKinds::READ_WRITE,
            ObservationSource::Replay,
            ObservationTarget::RuntimeDiscovered,
            4.0,
            4,
            false,
        )
        .unwrap(),
    );
    store.apply_batch(&graph, buffer, &config).unwrap();

    let checkpoint = store.checkpoint(&graph).unwrap();
    let bytes = checkpoint.to_pretty_json().unwrap();
    let decoded = FeedbackCheckpoint::from_json(&bytes).unwrap();
    let restored = AdaptiveFeedbackStore::restore(&graph, decoded, 0).unwrap();
    assert_eq!(
        restored.static_statistics(edge),
        store.static_statistics(edge)
    );
    assert_eq!(restored.fallback_edges(), store.fallback_edges());
}

#[test]
fn stale_observations_are_rejected() {
    let graph = graph();
    let edge = static_edge(&graph, "execute::Credit", "execute::Credit");
    let credit = profile_id(&graph, "execute::Credit");
    let mut store = AdaptiveFeedbackStore::from_graph(&graph, 10).unwrap();
    let mut buffer = ObservationBuffer::default();
    buffer.push(
        ConflictObservation::conflict(
            credit,
            credit,
            TxId(1),
            TxId(2),
            ConflictKinds::WRITE_WRITE,
            ObservationSource::CanonicalExecution,
            ObservationTarget::Static { edge_index: edge },
            1.0,
            9,
            true,
        )
        .unwrap(),
    );
    let error = store
        .apply_batch(&graph, buffer, &AdaptiveFeedbackConfig::default())
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("precedes the edge's last update epoch"));
}

#[test]
fn unsupported_checkpoint_version_is_rejected() {
    let bytes = br#"{"format_version":99,"static_edges":[],"fallback_edges":[]}"#;
    assert!(FeedbackCheckpoint::from_json(bytes).is_err());
}

#[test]
fn estimate_at_projects_decay_without_mutating_stored_statistics() {
    let graph = graph();
    let edge = static_edge(&graph, "execute::Credit", "execute::Credit");
    let store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let stored = *store.static_statistics(edge).unwrap();
    let config = AdaptiveFeedbackConfig {
        retention_factor: 0.5,
        ..AdaptiveFeedbackConfig::default()
    };
    let projected = stored.estimate_at(3, &config).unwrap();
    assert!((projected.probability - stored.probability()).abs() < 1e-12);
    assert!(projected.posterior_mass < stored.posterior_mass());
    assert!(projected.confidence < stored.confidence(config.confidence_scale).unwrap());
    assert_eq!(*store.static_statistics(edge).unwrap(), stored);
}
