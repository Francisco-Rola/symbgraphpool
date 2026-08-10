use acg_candidate_graph::{CandidateGraphBuilder, CandidateTransaction};
use acg_core::{ContractCodeHash, InstanceId, RuntimeId, TxId};
use acg_cosmwasm_engine::{AccessKind, AccessRecord, Address, ExecutionOutcome, TransactionId};
use acg_feedback::{AdaptiveFeedbackConfig, AdaptiveFeedbackStore};
use acg_predicate::InputBindings;
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_runtime_feedback::{RuntimeFeedbackEngine, RuntimeFeedbackWeights, TraceConflictConfig};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use acg_validator_sim::{BlockExecutionReport, TransactionExecution};
use serde_json::json;

const MINIWAREHOUSE: &[u8] =
    include_bytes!("../../../../benchmarks/symbolic/miniwarehouse.symbolic.json");

fn graph() -> ProfileGraph {
    let raw = parse_slice(MINIWAREHOUSE).unwrap();
    let context = IngestionContext::new(
        RuntimeId::new("cosmwasm").unwrap(),
        ContractCodeHash([5; 32]),
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
    entrypoint: &str,
    bindings: serde_json::Value,
) -> CandidateTransaction {
    CandidateTransaction {
        tx_id: TxId(id),
        predicted_position: id as u32,
        inclusion_probability: 1.0,
        profile_id: profile_id(graph, entrypoint),
        instance_id: InstanceId(1),
        input_bindings: InputBindings::from_value(bindings),
        estimated_execution_cost: 1,
    }
}

fn report(overlap: bool) -> BlockExecutionReport {
    let left_key = if overlap {
        &b"stock/2/7"[..]
    } else {
        &b"stock/2/8"[..]
    };
    let right_key = &b"stock/2/7"[..];
    BlockExecutionReport {
        block_height: 1,
        block_time_nanos: 2_000_000_000,
        transactions: vec![
            execution(0, 1, AccessKind::StorageWrite, left_key),
            execution(1, 2, AccessKind::StorageRead, right_key),
        ],
    }
}

fn execution(
    transaction_index: usize,
    transaction_id: u64,
    kind: AccessKind,
    key: &[u8],
) -> TransactionExecution {
    TransactionExecution {
        transaction_index,
        transaction_id: TransactionId(transaction_id),
        result: Ok(ExecutionOutcome {
            transaction_id: TransactionId(transaction_id),
            contract: Address::new("miniwarehouse"),
            events: Vec::new(),
            data: None,
            accesses: vec![AccessRecord {
                transaction_id: TransactionId(transaction_id),
                call_depth: 0,
                contract: Address::new("miniwarehouse"),
                kind,
                key: key.to_vec(),
                range_end: None,
                value: None,
                reverted: false,
            }],
            created_contracts: Vec::new(),
        }),
    }
}

#[test]
fn miniwarehouse_history_adapts_from_conflicting_to_independent_phase() {
    let graph = graph();
    let candidate_graph = CandidateGraphBuilder::new(&graph)
        .build(vec![
            candidate(
                &graph,
                1,
                "execute::NewOrder",
                json!({
                    "warehouse_id":1,
                    "district_id":1,
                    "customer_id":1,
                    "lines":[{
                        "supply_warehouse_id":2,
                        "item_id":7,
                        "quantity":1
                    }]
                }),
            ),
            candidate(
                &graph,
                2,
                "execute::Restock",
                json!({"warehouse_id":2,"item_id":7,"quantity":5}),
            ),
        ])
        .unwrap();
    assert_eq!(candidate_graph.edges().len(), 1);
    let edge_index = candidate_graph.edges()[0].profile_edge_index;

    let config = AdaptiveFeedbackConfig {
        retention_factor: 0.95,
        ..AdaptiveFeedbackConfig::default()
    };
    let cold = AdaptiveFeedbackStore::from_graph(&graph, 0)
        .unwrap()
        .static_statistics(edge_index)
        .unwrap()
        .probability();
    let mut feedback = RuntimeFeedbackEngine::new(
        &graph,
        0,
        TraceConflictConfig::default(),
        RuntimeFeedbackWeights::default(),
        config,
    )
    .unwrap();

    for epoch in 1..=5 {
        feedback
            .process_block(&graph, &candidate_graph, &report(true), epoch)
            .unwrap();
    }
    let conflict_phase = feedback
        .store()
        .static_statistics(edge_index)
        .unwrap()
        .probability();
    assert!(conflict_phase > cold);

    for epoch in 6..=15 {
        feedback
            .process_block(&graph, &candidate_graph, &report(false), epoch)
            .unwrap();
    }
    let independent_phase = feedback
        .store()
        .static_statistics(edge_index)
        .unwrap()
        .probability();
    assert!(independent_phase < conflict_phase);
}
