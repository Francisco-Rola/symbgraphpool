use acg_candidate_graph::{
    CandidateGraphBuilder, CandidateGraphError, CandidateTransaction, CostAwareEdgePolicyConfig,
    EdgeClass, EdgeProvenance, RiskBoundedSchedulerConfig, WeightedCandidateGraphConfig,
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
        cost_policy: Default::default(),
        compact_immature_equivalence_edges: false,
        independent_observations_before_softening: 8,
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
    assert!((edge.scheduling_risk() - edge.probability()).abs() <= Q16_EPSILON);
    assert_eq!(edge.replay_cost_confidence(), 0.0);
    assert!(!edge.is_historical_override());
}

#[test]
fn symbolic_true_edge_remains_materialized_above_adaptive_threshold() {
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
    assert_eq!(above.edges().len(), 1);
    assert_eq!(above.edges()[0].predicate_result, PredicateResult::True);
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

    let pruned = CandidateGraphBuilder::new(&graph)
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
            weighted_config(0, estimate.probability + (1.0 - estimate.probability) / 2.0),
        )
        .unwrap();
    assert!(pruned.edges().is_empty());
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

    let retained = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            vec![
                tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
                tx(&graph, 2, "execute::Credit", 2, json!({"account":"bob"})),
            ],
            &store,
            &feedback_config,
            weighted_config(1, estimate.probability + (1.0 - estimate.probability) / 2.0),
        )
        .unwrap();
    assert!(retained.edges()[0].is_historical_override());
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

    let retained = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            transactions(),
            &store,
            &feedback_config,
            weighted_config(1, estimate.probability + (1.0 - estimate.probability) / 2.0),
        )
        .unwrap();
    let retained_edge = retained.edge_between(TxIndex(0), TxIndex(1)).unwrap();
    assert_eq!(retained_edge.runtime_edge_id(), Some(fallback.id));
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
fn legacy_binary_builder_preserves_pre_phase4_behavior() {
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
    assert_eq!(edge.scheduling_risk(), 1.0);
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

#[test]
fn replay_cost_changes_scheduling_risk_without_rewriting_raw_conflict_probability() {
    let graph = graph();
    let credit = profile_id(&graph, "execute::Credit");
    let edge_index = static_edge(&graph, "execute::Credit", "execute::Credit");
    let feedback_config = AdaptiveFeedbackConfig {
        retention_factor: 1.0,
        confidence_scale: 1.0,
        ..AdaptiveFeedbackConfig::default()
    };
    let cost_policy = CostAwareEdgePolicyConfig {
        serialization_cost_reference_nanos: 100_000,
        invalidation_fanout_weight: 1.0,
        ..CostAwareEdgePolicyConfig::default()
    };

    let build_store = |replay_cost_nanos, invalidated_descendants| {
        let mut store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
        let mut observations = ObservationBuffer::default();
        for offset in 0..8_u64 {
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
        observations.push(
            ConflictObservation::conflict(
                credit,
                credit,
                TxId(200),
                TxId(201),
                ConflictKinds::WRITE_WRITE,
                ObservationSource::Replay,
                ObservationTarget::Static { edge_index },
                4.0,
                1,
                true,
            )
            .unwrap()
            .with_replay_impact(replay_cost_nanos, invalidated_descendants),
        );
        store
            .apply_batch(&graph, observations, &feedback_config)
            .unwrap();
        store
    };

    let cheap_store = build_store(25_000, 0);
    let expensive_store = build_store(2_000_000, 3);
    let transactions = || {
        vec![
            tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
            tx(&graph, 2, "execute::Credit", 1, json!({"account":"alice"})),
        ]
    };
    let config = WeightedCandidateGraphConfig {
        epoch: 1,
        edge_materialization_threshold: 0.0,
        cost_policy,
        compact_immature_equivalence_edges: false,
        independent_observations_before_softening: 8,
    };
    let cheap = CandidateGraphBuilder::new(&graph)
        .build_weighted(transactions(), &cheap_store, &feedback_config, config)
        .unwrap();
    let expensive = CandidateGraphBuilder::new(&graph)
        .build_weighted(transactions(), &expensive_store, &feedback_config, config)
        .unwrap();
    let cheap_edge = cheap.edge_between(TxIndex(0), TxIndex(1)).unwrap();
    let expensive_edge = expensive.edge_between(TxIndex(0), TxIndex(1)).unwrap();

    assert!((cheap_edge.probability() - expensive_edge.probability()).abs() <= Q16_EPSILON);
    assert_eq!(cheap_edge.expected_replay_cost_nanos, 25_000);
    assert_eq!(expensive_edge.expected_replay_cost_nanos, 2_000_000);
    assert_eq!(expensive_edge.expected_invalidated_descendants(), 3.0);
    assert!(expensive_edge.scheduling_risk() > cheap_edge.scheduling_risk() + 0.5);

    let scheduler = RiskBoundedSchedulerConfig {
        soft_threshold: 0.20,
        hard_threshold: 0.70,
        risk_budget: 0.20,
        max_wave_width: None,
        exploration_rate: 0.0,
        exploration_risk_budget: 0.90,
        exploration_min_uncertainty: 0.35,
        exploration_max_transactions_per_block: 0,
        independent_observations_before_softening: 8,
    };
    assert_eq!(scheduler.classify(cheap_edge), EdgeClass::Soft);
    assert_eq!(scheduler.classify(expensive_edge), EdgeClass::Hard);
}

#[test]
fn learned_serialization_cost_changes_risk_while_preserving_replay_and_probability_inputs() {
    let graph = graph();
    let credit = profile_id(&graph, "execute::Credit");
    let edge_index = static_edge(&graph, "execute::Credit", "execute::Credit");
    let feedback_config = AdaptiveFeedbackConfig {
        retention_factor: 1.0,
        confidence_scale: 1.0,
        ..AdaptiveFeedbackConfig::default()
    };
    let cost_policy = CostAwareEdgePolicyConfig {
        serialization_cost_reference_nanos: 250_000,
        invalidation_fanout_weight: 0.5,
        ..CostAwareEdgePolicyConfig::default()
    };

    let build_store = |serialization_cost_nanos| {
        let mut store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
        let mut observations = ObservationBuffer::default();
        for offset in 0..8_u64 {
            observations.push(
                ConflictObservation::independent(
                    credit,
                    credit,
                    TxId(300 + offset * 2),
                    TxId(301 + offset * 2),
                    ObservationSource::CanonicalExecution,
                    ObservationTarget::Static { edge_index },
                    1.0,
                    1,
                    true,
                )
                .unwrap(),
            );
        }
        observations.push(
            ConflictObservation::conflict(
                credit,
                credit,
                TxId(400),
                TxId(401),
                ConflictKinds::WRITE_WRITE,
                ObservationSource::Replay,
                ObservationTarget::Static { edge_index },
                4.0,
                1,
                true,
            )
            .unwrap()
            .with_replay_impact(1_000_000, 1),
        );
        store
            .apply_batch(&graph, observations, &feedback_config)
            .unwrap();
        for _ in 0..8 {
            store
                .record_static_serialization_cost(
                    edge_index,
                    serialization_cost_nanos,
                    1.0,
                    1,
                    &feedback_config,
                )
                .unwrap();
        }
        store
    };

    let cheap_to_serialize = build_store(25_000);
    let expensive_to_serialize = build_store(5_000_000);
    let transactions = || {
        vec![
            tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
            tx(&graph, 2, "execute::Credit", 1, json!({"account":"alice"})),
        ]
    };
    let config = WeightedCandidateGraphConfig {
        epoch: 1,
        edge_materialization_threshold: 0.0,
        cost_policy,
        compact_immature_equivalence_edges: false,
        independent_observations_before_softening: 8,
    };
    let cheap = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            transactions(),
            &cheap_to_serialize,
            &feedback_config,
            config,
        )
        .unwrap();
    let expensive = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            transactions(),
            &expensive_to_serialize,
            &feedback_config,
            config,
        )
        .unwrap();
    let cheap_edge = cheap.edge_between(TxIndex(0), TxIndex(1)).unwrap();
    let expensive_edge = expensive.edge_between(TxIndex(0), TxIndex(1)).unwrap();

    assert!((cheap_edge.probability() - expensive_edge.probability()).abs() <= Q16_EPSILON);
    assert_eq!(
        cheap_edge.expected_replay_cost_nanos,
        expensive_edge.expected_replay_cost_nanos
    );
    assert_eq!(cheap_edge.expected_serialization_cost_nanos, 25_000);
    assert_eq!(expensive_edge.expected_serialization_cost_nanos, 5_000_000);
    assert!(cheap_edge.serialization_cost_confidence() > 0.99);
    assert!(expensive_edge.serialization_cost_confidence() > 0.99);
    assert!(cheap_edge.scheduling_risk() > expensive_edge.scheduling_risk() + 0.5);
}

#[test]
fn immature_equivalence_clique_is_materialized_as_a_chain_with_logical_coverage() {
    let graph = graph();
    let store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let feedback_config = feedback_config();
    let transactions = (0..8_u64)
        .map(|offset| {
            tx(
                &graph,
                offset + 1,
                "execute::Credit",
                1,
                json!({"account":"alice"}),
            )
        })
        .collect();
    let candidate = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            transactions,
            &store,
            &feedback_config,
            WeightedCandidateGraphConfig {
                epoch: 0,
                edge_materialization_threshold: 0.0,
                cost_policy: Default::default(),
                compact_immature_equivalence_edges: true,
                independent_observations_before_softening: 8,
            },
        )
        .unwrap();

    assert_eq!(candidate.logical_edge_count(), 28);
    assert_eq!(candidate.edges().len(), 7);
    assert_eq!(candidate.compact_groups().len(), 1);
    assert!(candidate.contains_candidate_pair(TxIndex(0), TxIndex(7)));
    assert!(candidate.edge_between(TxIndex(0), TxIndex(7)).is_none());

    let scheduler =
        acg_candidate_graph::RiskBoundedScheduler::new(RiskBoundedSchedulerConfig::default())
            .unwrap();
    let schedule = scheduler.schedule(&candidate).unwrap();
    assert_eq!(schedule.ordering_dependencies.len(), 7);
    schedule
        .validate_against(&candidate, scheduler.config())
        .unwrap();
}

#[test]
fn mature_soft_equivalence_clique_stays_compact_with_pairwise_schedule_semantics() {
    let graph = graph();
    let credit = profile_id(&graph, "execute::Credit");
    let edge_index = static_edge(&graph, "execute::Credit", "execute::Credit");
    let feedback_config = feedback_config();
    let mut store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let mut observations = ObservationBuffer::default();
    for offset in 0..8_u64 {
        observations.push(
            ConflictObservation::independent(
                credit,
                credit,
                TxId(1_000 + offset * 2),
                TxId(1_001 + offset * 2),
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

    let transactions = || {
        (0..8_u64)
            .map(|offset| {
                tx(
                    &graph,
                    offset + 1,
                    "execute::Credit",
                    1,
                    json!({"account":"alice"}),
                )
            })
            .collect::<Vec<_>>()
    };
    let build = |compact| {
        CandidateGraphBuilder::new(&graph)
            .build_weighted(
                transactions(),
                &store,
                &feedback_config,
                WeightedCandidateGraphConfig {
                    epoch: 1,
                    edge_materialization_threshold: 0.0,
                    cost_policy: Default::default(),
                    compact_immature_equivalence_edges: compact,
                    independent_observations_before_softening: 8,
                },
            )
            .unwrap()
    };
    let pairwise = build(false);
    let compact = build(true);
    assert_eq!(pairwise.logical_edge_count(), 28);
    assert_eq!(pairwise.edges().len(), 28);
    assert_eq!(compact.logical_edge_count(), 28);
    assert_eq!(compact.edges().len(), 7);
    assert_eq!(compact.compact_groups().len(), 1);

    let scheduler_config = RiskBoundedSchedulerConfig {
        soft_threshold: 0.20,
        hard_threshold: 1.0,
        risk_budget: 0.0,
        max_wave_width: None,
        exploration_rate: 0.0,
        exploration_risk_budget: 0.90,
        exploration_min_uncertainty: 0.35,
        exploration_max_transactions_per_block: 0,
        independent_observations_before_softening: 8,
    };
    let scheduler = acg_candidate_graph::RiskBoundedScheduler::new(scheduler_config).unwrap();
    assert_eq!(
        scheduler.classify_edge(pairwise.edges().first().unwrap()),
        EdgeClass::Soft
    );
    let pairwise_schedule = scheduler.schedule(&pairwise).unwrap();
    let compact_schedule = scheduler.schedule(&compact).unwrap();
    assert_eq!(compact_schedule.waves, pairwise_schedule.waves);
    compact_schedule
        .validate_against(&compact, scheduler.config())
        .unwrap();
    pairwise_schedule
        .validate_against(&pairwise, scheduler.config())
        .unwrap();
    assert!(
        compact_schedule.pre_reduction_ordering_dependencies
            < pairwise_schedule.pre_reduction_ordering_dependencies
    );
}

#[test]
fn consensus_phase_cost_weights_must_be_finite_and_positive() {
    let invalid_pre = CostAwareEdgePolicyConfig {
        pre_consensus_serialization_weight: 0.0,
        ..CostAwareEdgePolicyConfig::default()
    };
    assert!(matches!(
        invalid_pre.validate().unwrap_err(),
        CandidateGraphError::InvalidPhaseWeight(value) if value == 0.0
    ));

    let invalid_post = CostAwareEdgePolicyConfig {
        post_consensus_replay_weight: f64::NAN,
        ..CostAwareEdgePolicyConfig::default()
    };
    assert!(matches!(
        invalid_post.validate().unwrap_err(),
        CandidateGraphError::InvalidPhaseWeight(value) if value.is_nan()
    ));
}
