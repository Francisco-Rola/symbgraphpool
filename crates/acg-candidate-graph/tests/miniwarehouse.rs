use acg_candidate_graph::{CandidateGraphBuilder, CandidateTransaction};
use acg_core::{ContractCodeHash, InstanceId, RuntimeId, TxId, TxIndex, UnknownReason};
use acg_predicate::{CompiledPredicate, InputBindings, PredicateResult};
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use serde_json::json;

const MINIWAREHOUSE: &[u8] =
    include_bytes!("../../../benchmarks/symbolic/miniwarehouse.symbolic.json");

fn graph() -> ProfileGraph {
    let context = IngestionContext::new(
        RuntimeId::new("cosmwasm").unwrap(),
        ContractCodeHash([41; 32]),
        1,
    );
    let profiles = normalize_document(parse_slice(MINIWAREHOUSE).unwrap(), &context).unwrap();
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

fn tx(
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

fn new_order_bindings(
    warehouse_id: u64,
    district_id: u64,
    customer_id: u64,
    order_id: u64,
    lines: serde_json::Value,
) -> serde_json::Value {
    json!({
        "warehouse_id": warehouse_id,
        "district_id": district_id,
        "customer_id": customer_id,
        "order_id": order_id,
        "lines": lines,
    })
}

#[test]
fn new_order_variable_line_keys_match_remote_and_local_stock_exactly() {
    let graph = graph();
    let candidate = CandidateGraphBuilder::new(&graph)
        .build(vec![
            tx(
                &graph,
                1,
                "execute::NewOrder",
                new_order_bindings(
                    1,
                    1,
                    1,
                    1,
                    json!([
                        {"supply_warehouse_id":2,"item_id":7,"quantity":1,"unit_price":"1"},
                        {"supply_warehouse_id":1,"item_id":9,"quantity":1,"unit_price":"1"}
                    ]),
                ),
            ),
            tx(
                &graph,
                2,
                "execute::Restock",
                json!({"warehouse_id":2,"item_id":7,"quantity":5}),
            ),
            tx(
                &graph,
                3,
                "execute::Restock",
                json!({"warehouse_id":1,"item_id":7,"quantity":5}),
            ),
            tx(
                &graph,
                4,
                "execute::Restock",
                json!({"warehouse_id":1,"item_id":9,"quantity":5}),
            ),
        ])
        .unwrap();

    assert_eq!(
        candidate
            .edge_between(TxIndex(0), TxIndex(1))
            .unwrap()
            .predicate_result,
        PredicateResult::True
    );
    assert!(candidate.edge_between(TxIndex(0), TxIndex(2)).is_none());
    assert_eq!(
        candidate
            .edge_between(TxIndex(0), TxIndex(3))
            .unwrap()
            .predicate_result,
        PredicateResult::True
    );
}

#[test]
fn empty_new_order_input_guard_prunes_warehouse_conflict() {
    let graph = graph();
    let populated = CandidateGraphBuilder::new(&graph)
        .build(vec![
            tx(
                &graph,
                1,
                "execute::NewOrder",
                new_order_bindings(
                    1,
                    1,
                    1,
                    1,
                    json!([{"supply_warehouse_id":1,"item_id":1,"quantity":1,"unit_price":"1"}]),
                ),
            ),
            tx(
                &graph,
                2,
                "execute::Payment",
                json!({
                    "warehouse_id":1,"district_id":2,"customer_id":2,
                    "amount":"1","history_id":1
                }),
            ),
        ])
        .unwrap();
    assert_eq!(
        populated.edges()[0].predicate_result,
        PredicateResult::True,
        "NewOrder reads WAREHOUSES[1] and Payment writes it"
    );

    let empty = CandidateGraphBuilder::new(&graph)
        .build(vec![
            tx(
                &graph,
                1,
                "execute::NewOrder",
                new_order_bindings(1, 1, 1, 1, json!([])),
            ),
            tx(
                &graph,
                2,
                "execute::Payment",
                json!({
                    "warehouse_id":1,"district_id":2,"customer_id":2,
                    "amount":"1","history_id":1
                }),
            ),
        ])
        .unwrap();
    assert!(empty.edges().is_empty());
}

#[test]
fn stock_level_item_vector_matches_any_restock_key() {
    let graph = graph();
    let candidate = CandidateGraphBuilder::new(&graph)
        .build(vec![
            tx(
                &graph,
                1,
                "query::StockLevel",
                json!({
                    "warehouse_id":1,"district_id":1,"threshold":10,"item_ids":[3,7,11]
                }),
            ),
            tx(
                &graph,
                2,
                "execute::Restock",
                json!({"warehouse_id":1,"item_id":7,"quantity":5}),
            ),
            tx(
                &graph,
                3,
                "execute::Restock",
                json!({"warehouse_id":1,"item_id":8,"quantity":5}),
            ),
        ])
        .unwrap();

    assert_eq!(
        candidate
            .edge_between(TxIndex(0), TxIndex(1))
            .unwrap()
            .predicate_result,
        PredicateResult::True
    );
    assert!(candidate.edge_between(TxIndex(0), TxIndex(2)).is_none());
}

#[test]
fn payments_conflict_at_warehouse_granularity_even_for_different_customers() {
    let graph = graph();
    let candidate = CandidateGraphBuilder::new(&graph)
        .build(vec![
            tx(
                &graph,
                1,
                "execute::Payment",
                json!({
                    "warehouse_id":1,"district_id":1,"customer_id":1,
                    "amount":"1","history_id":1
                }),
            ),
            tx(
                &graph,
                2,
                "execute::Payment",
                json!({
                    "warehouse_id":1,"district_id":2,"customer_id":2,
                    "amount":"1","history_id":2
                }),
            ),
            tx(
                &graph,
                3,
                "execute::Payment",
                json!({
                    "warehouse_id":2,"district_id":1,"customer_id":1,
                    "amount":"1","history_id":3
                }),
            ),
        ])
        .unwrap();

    assert_eq!(
        candidate
            .edge_between(TxIndex(0), TxIndex(1))
            .unwrap()
            .predicate_result,
        PredicateResult::True
    );
    assert!(candidate.edge_between(TxIndex(0), TxIndex(2)).is_none());
}

#[test]
fn new_orders_in_different_home_partitions_still_detect_shared_remote_stock() {
    let graph = graph();
    let shared = CandidateGraphBuilder::new(&graph)
        .build(vec![
            tx(
                &graph,
                1,
                "execute::NewOrder",
                new_order_bindings(
                    1,
                    1,
                    1,
                    1,
                    json!([{"supply_warehouse_id":3,"item_id":5,"quantity":1,"unit_price":"1"}]),
                ),
            ),
            tx(
                &graph,
                2,
                "execute::NewOrder",
                new_order_bindings(
                    2,
                    2,
                    2,
                    2,
                    json!([{"supply_warehouse_id":3,"item_id":5,"quantity":1,"unit_price":"1"}]),
                ),
            ),
        ])
        .unwrap();
    assert_eq!(shared.edges().len(), 1);
    assert_eq!(shared.edges()[0].predicate_result, PredicateResult::Unknown);

    let disjoint = CandidateGraphBuilder::new(&graph)
        .build(vec![
            tx(
                &graph,
                1,
                "execute::NewOrder",
                new_order_bindings(
                    1,
                    1,
                    1,
                    1,
                    json!([{"supply_warehouse_id":3,"item_id":5,"quantity":1,"unit_price":"1"}]),
                ),
            ),
            tx(
                &graph,
                2,
                "execute::NewOrder",
                new_order_bindings(
                    2,
                    2,
                    2,
                    2,
                    json!([{"supply_warehouse_id":3,"item_id":6,"quantity":1,"unit_price":"1"}]),
                ),
            ),
        ])
        .unwrap();
    assert!(disjoint.edges().is_empty());
}

#[test]
fn delivery_order_status_reports_precise_order_clause_and_unresolved_alternatives() {
    let graph = graph();
    let delivery = profile_id(&graph, "execute::Delivery");
    let status = profile_id(&graph, "query::OrderStatus");
    let edge = graph
        .edges()
        .iter()
        .find(|edge| {
            (edge.source == delivery && edge.target == status)
                || (edge.source == status && edge.target == delivery)
        })
        .unwrap();
    let compiled = CompiledPredicate::compile(&edge.predicate);

    let delivery_bindings = InputBindings::from_value(json!({
        "warehouse_id":1,"district_id":1,"order_id":9,"carrier_id":2
    }));
    let same_status = InputBindings::from_value(json!({
        "warehouse_id":1,"district_id":1,"customer_id":7,"order_id":9
    }));
    let different_status = InputBindings::from_value(json!({
        "warehouse_id":1,"district_id":1,"customer_id":7,"order_id":10
    }));

    let evaluate = |status_bindings: &InputBindings| {
        if edge.source == delivery {
            compiled.evaluate_detailed(
                InstanceId(1),
                &delivery_bindings,
                InstanceId(1),
                status_bindings,
            )
        } else {
            compiled.evaluate_detailed(
                InstanceId(1),
                status_bindings,
                InstanceId(1),
                &delivery_bindings,
            )
        }
    };

    let same = evaluate(&same_status);
    let different = evaluate(&different_status);
    let order_clause_index = edge
        .predicate
        .clauses
        .iter()
        .position(|clause| clause.resource.as_str() == "ORDERS")
        .unwrap();
    let same_order_clause = &same.clauses[order_clause_index];
    let different_order_clause = &different.clauses[order_clause_index];

    assert_eq!(same.result, PredicateResult::Unknown);
    assert_eq!(same_order_clause.result, PredicateResult::Unknown);
    assert!(same_order_clause
        .unknown_reasons
        .contains(&UnknownReason::StateDependentGuard));
    assert_eq!(different_order_clause.result, PredicateResult::False);
    assert_eq!(different.result, PredicateResult::Unknown);
}
