use acg_candidate_graph::{
    CandidateGraph, CandidateGraphBuilder, CandidateTransaction, CostAwareEdgePolicyConfig,
    EdgeClass, RiskBoundedSchedulerConfig, WeightedCandidateGraphConfig,
};
use acg_core::{ConflictKinds, ContractCodeHash, InstanceId, RuntimeId, TxId, TxIndex};
use acg_feedback::AdaptiveFeedbackConfig;
use acg_predicate::InputBindings;
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_runtime_feedback::{
    RuntimeFeedbackEngine, RuntimeFeedbackWeights, TraceConflictConfig, ValidationEvidence,
    ValidationEvidenceKind,
};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use serde_json::json;

const CONFLICTLAB: &[u8] =
    include_bytes!("../../../../benchmarks/symbolic/conflictlab.symbolic.json");
const Q16_EPSILON: f64 = 1.0 / u16::MAX as f64;

fn graph() -> ProfileGraph {
    let context = IngestionContext::new(
        RuntimeId::new("cosmwasm").unwrap(),
        ContractCodeHash([0x5d; 32]),
        1,
    );
    let profiles = normalize_document(parse_slice(CONFLICTLAB).unwrap(), &context).unwrap();
    let artifact = ProfileGraphArtifact::compile(profiles, &EdgeBuildConfig::default()).unwrap();
    ProfileGraph::load(artifact, GraphLoadConfig::default()).unwrap()
}

fn credit_profile(graph: &ProfileGraph) -> acg_core::ProfileId {
    graph
        .profiles()
        .iter()
        .find(|profile| profile.definition.entrypoint_name == "execute::Credit")
        .unwrap()
        .id
}

fn transactions(graph: &ProfileGraph) -> Vec<CandidateTransaction> {
    let profile_id = credit_profile(graph);
    vec![
        CandidateTransaction {
            tx_id: TxId(1),
            predicted_position: 0,
            inclusion_probability: 1.0,
            profile_id,
            instance_id: InstanceId(1),
            input_bindings: InputBindings::from_value(json!({"account":"alice"})),
            estimated_execution_cost: 1,
        },
        CandidateTransaction {
            tx_id: TxId(2),
            predicted_position: 1,
            inclusion_probability: 1.0,
            profile_id,
            instance_id: InstanceId(1),
            input_bindings: InputBindings::from_value(json!({"account":"alice"})),
            estimated_execution_cost: 1,
        },
    ]
}

fn candidate(
    graph: &ProfileGraph,
    feedback: &RuntimeFeedbackEngine,
    epoch: u64,
    cost_policy: CostAwareEdgePolicyConfig,
) -> CandidateGraph {
    CandidateGraphBuilder::new(graph)
        .build_weighted(
            transactions(graph),
            feedback.store(),
            feedback.adaptive_config(),
            WeightedCandidateGraphConfig {
                epoch,
                edge_materialization_threshold: 0.0,
                cost_policy,
                compact_immature_equivalence_edges: false,
                independent_observations_before_softening: 8,
            },
        )
        .unwrap()
}

fn edge(graph: &CandidateGraph) -> &acg_candidate_graph::TransactionEdge {
    graph.edge_between(TxIndex(0), TxIndex(1)).unwrap()
}

#[test]
fn brick5d_closed_loop_hardens_after_expensive_replay_then_relaxes_after_phase_change() {
    let graph = graph();
    let adaptive_config = AdaptiveFeedbackConfig {
        retention_factor: 0.5,
        confidence_scale: 1.0,
        ..AdaptiveFeedbackConfig::default()
    };
    let cost_policy = CostAwareEdgePolicyConfig {
        serialization_cost_reference_nanos: 250_000,
        invalidation_fanout_weight: 0.5,
        ..CostAwareEdgePolicyConfig::default()
    };
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
    let mut feedback = RuntimeFeedbackEngine::new(
        &graph,
        0,
        TraceConflictConfig::default(),
        RuntimeFeedbackWeights::default(),
        adaptive_config,
    )
    .unwrap();

    // Phase 1: repeated concrete independence is enough to soften the symbolic relationship.
    for epoch in 1..=8_u64 {
        let current = candidate(&graph, &feedback, epoch, cost_policy);
        feedback
            .process_validation(
                &graph,
                &current,
                &[ValidationEvidence {
                    predecessor: TxIndex(0),
                    transaction: TxIndex(1),
                    kind: ValidationEvidenceKind::Independent,
                }],
                epoch,
            )
            .unwrap();
    }
    let low_contention = candidate(&graph, &feedback, 8, cost_policy);
    let low_edge = edge(&low_contention);
    assert_eq!(scheduler.classify(low_edge), EdgeClass::Soft);
    assert_eq!(low_edge.replay_cost_confidence(), 0.0);
    let low_probability = low_edge.probability();
    let low_risk = low_edge.scheduling_risk();

    // Phase 2: one measured, high-fan-out replay makes the same relationship expensive enough to
    // schedule conservatively even though the raw probability history is still separately visible.
    feedback
        .process_validation(
            &graph,
            &low_contention,
            &[ValidationEvidence {
                predecessor: TxIndex(0),
                transaction: TxIndex(1),
                kind: ValidationEvidenceKind::Replayed {
                    conflict_kinds: ConflictKinds::WRITE_READ,
                    replay_cost_nanos: 5_000_000,
                    invalidated_descendants: 4,
                },
            }],
            9,
        )
        .unwrap();
    let expensive = candidate(&graph, &feedback, 9, cost_policy);
    let expensive_edge = edge(&expensive);
    assert_eq!(scheduler.classify(expensive_edge), EdgeClass::Hard);
    assert!(expensive_edge.replay_cost_confidence() > 0.9);
    assert_eq!(expensive_edge.expected_replay_cost_nanos, 5_000_000);
    assert_eq!(expensive_edge.expected_invalidated_descendants(), 4.0);
    assert!(expensive_edge.scheduling_risk() > low_risk);

    // Brick 5D.2 checkpointing must preserve the decision-driving cost state exactly.
    let checkpoint = feedback.checkpoint(&graph).unwrap();
    let restored = RuntimeFeedbackEngine::restore(
        &graph,
        checkpoint,
        0,
        TraceConflictConfig::default(),
        RuntimeFeedbackWeights::default(),
        adaptive_config,
    )
    .unwrap();
    let restored_expensive = candidate(&graph, &restored, 9, cost_policy);
    assert!(
        (edge(&restored_expensive).scheduling_risk() - expensive_edge.scheduling_risk()).abs()
            <= Q16_EPSILON
    );

    // Phase 3: workload behavior changes back to independence. Probability learns the new phase and
    // old replay-cost confidence decays, allowing the known symbolic relationship to soften again.
    for epoch in 10..=30_u64 {
        let current = candidate(&graph, &feedback, epoch, cost_policy);
        feedback
            .process_validation(
                &graph,
                &current,
                &[ValidationEvidence {
                    predecessor: TxIndex(0),
                    transaction: TxIndex(1),
                    kind: ValidationEvidenceKind::Independent,
                }],
                epoch,
            )
            .unwrap();
    }
    let recovered = candidate(&graph, &feedback, 30, cost_policy);
    let recovered_edge = edge(&recovered);
    assert_eq!(scheduler.classify(recovered_edge), EdgeClass::Soft);
    assert!(recovered_edge.probability() < low_probability);
    assert!(recovered_edge.replay_cost_confidence() < 0.01);
    assert!(recovered_edge.scheduling_risk() < expensive_edge.scheduling_risk());

    println!(
        "Brick 5D closed loop: low p={:.3} risk={:.3} {:?}; expensive p={:.3} risk={:.3} cost={}ns fanout={:.1} {:?}; recovered p={:.3} risk={:.3} cost_conf={:.4} {:?}",
        low_edge.probability(),
        low_edge.scheduling_risk(),
        scheduler.classify(low_edge),
        expensive_edge.probability(),
        expensive_edge.scheduling_risk(),
        expensive_edge.expected_replay_cost_nanos,
        expensive_edge.expected_invalidated_descendants(),
        scheduler.classify(expensive_edge),
        recovered_edge.probability(),
        recovered_edge.scheduling_risk(),
        recovered_edge.replay_cost_confidence(),
        scheduler.classify(recovered_edge),
    );
}
