use acg_core::{ContractCodeHash, EdgeRelation, KeyMatch, RuntimeId};
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};

const CONFLICTLAB: &[u8] = include_bytes!("../../../benchmarks/symbolic/conflictlab.symbolic.json");
const MINIWAREHOUSE: &[u8] =
    include_bytes!("../../../benchmarks/symbolic/miniwarehouse.symbolic.json");

fn graph(bytes: &[u8], code_byte: u8) -> ProfileGraph {
    let context = IngestionContext::new(
        RuntimeId::new("cosmwasm").unwrap(),
        ContractCodeHash([code_byte; 32]),
        1,
    );
    let profiles = normalize_document(parse_slice(bytes).unwrap(), &context).unwrap();
    let artifact = ProfileGraphArtifact::compile(profiles, &EdgeBuildConfig::default()).unwrap();
    ProfileGraph::load(artifact, GraphLoadConfig::default()).unwrap()
}

#[test]
fn conflictlab_graph_has_expected_shape() {
    let graph = graph(CONFLICTLAB, 31);
    assert_eq!(graph.profiles().len(), 18);
    assert_eq!(graph.edges().len(), 63);
    assert_eq!(
        graph
            .edges()
            .iter()
            .filter(|edge| edge.relation == EdgeRelation::Conditional)
            .count(),
        46
    );
    assert_eq!(
        graph
            .edges()
            .iter()
            .filter(|edge| edge.relation == EdgeRelation::Unknown)
            .count(),
        17
    );
}

#[test]
fn conflictlab_counter_edges_use_shard_equality() {
    let graph = graph(CONFLICTLAB, 32);
    let increment = graph
        .profiles()
        .iter()
        .find(|record| record.definition.entrypoint_name == "execute::IncrementCounter")
        .unwrap();
    let query = graph
        .profiles()
        .iter()
        .find(|record| record.definition.entrypoint_name == "query::Counter")
        .unwrap();
    let edge = graph
        .edges()
        .iter()
        .find(|edge| {
            (edge.source == increment.id && edge.target == query.id)
                || (edge.source == query.id && edge.target == increment.id)
        })
        .unwrap();

    assert_eq!(edge.relation, EdgeRelation::Conditional);
    assert!(edge.predicate.clauses.iter().any(|clause| {
        clause.resource.as_str() == "COUNTERS"
            && matches!(
                &clause.key_match,
                KeyMatch::InputEquality { left, right }
                    if left.expression == "shard_id" && right.expression == "shard_id"
            )
    }));
}

#[test]
fn conflictlab_wildcard_balance_reset_is_unknown() {
    let graph = graph(CONFLICTLAB, 33);
    let reset = graph
        .profiles()
        .iter()
        .find(|record| record.definition.entrypoint_name == "execute::ResetAllBalances")
        .unwrap();
    let balance = graph
        .profiles()
        .iter()
        .find(|record| record.definition.entrypoint_name == "query::Balance")
        .unwrap();
    let edge = graph
        .edges()
        .iter()
        .find(|edge| {
            (edge.source == reset.id && edge.target == balance.id)
                || (edge.source == balance.id && edge.target == reset.id)
        })
        .unwrap();
    assert_eq!(edge.relation, EdgeRelation::Unknown);
    assert!(edge
        .predicate
        .clauses
        .iter()
        .any(|clause| matches!(&clause.key_match, KeyMatch::Unresolved)));
}

#[test]
fn miniwarehouse_graph_has_expected_shape() {
    let graph = graph(MINIWAREHOUSE, 34);
    assert_eq!(graph.profiles().len(), 14);
    assert_eq!(graph.edges().len(), 44);
    assert_eq!(
        graph
            .edges()
            .iter()
            .filter(|edge| edge.relation == EdgeRelation::Conditional)
            .count(),
        37
    );
    assert_eq!(
        graph
            .edges()
            .iter()
            .filter(|edge| edge.relation == EdgeRelation::Unknown)
            .count(),
        7
    );
}

#[test]
fn miniwarehouse_stock_edges_bind_line_items_to_stock_queries() {
    let graph = graph(MINIWAREHOUSE, 35);
    let new_order = graph
        .profiles()
        .iter()
        .find(|record| record.definition.entrypoint_name == "execute::NewOrder")
        .unwrap();
    let stock = graph
        .profiles()
        .iter()
        .find(|record| record.definition.entrypoint_name == "query::Stock")
        .unwrap();
    let edge = graph
        .edges()
        .iter()
        .find(|edge| {
            (edge.source == new_order.id && edge.target == stock.id)
                || (edge.source == stock.id && edge.target == new_order.id)
        })
        .unwrap();
    assert_eq!(edge.relation, EdgeRelation::Conditional);
    assert!(edge.predicate.clauses.iter().any(|clause| {
        clause.resource.as_str() == "STOCK"
            && matches!(
                &clause.key_match,
                KeyMatch::InputEquality { left, right }
                    if [left.expression.as_str(), right.expression.as_str()].contains(
                        &"(lines[i].supply_warehouse_id, lines[i].item_id)"
                    )
                        && [left.expression.as_str(), right.expression.as_str()]
                            .contains(&"(warehouse_id, item_id)")
            )
    }));
}

#[test]
fn miniwarehouse_delivery_prefix_and_customer_keys_remain_unknown() {
    let graph = graph(MINIWAREHOUSE, 36);
    let delivery = graph
        .profiles()
        .iter()
        .find(|record| record.definition.entrypoint_name == "execute::Delivery")
        .unwrap();
    let status = graph
        .profiles()
        .iter()
        .find(|record| record.definition.entrypoint_name == "query::OrderStatus")
        .unwrap();
    let edge = graph
        .edges()
        .iter()
        .find(|edge| {
            (edge.source == delivery.id && edge.target == status.id)
                || (edge.source == status.id && edge.target == delivery.id)
        })
        .unwrap();
    assert_eq!(edge.relation, EdgeRelation::Unknown);
    assert!(edge.predicate.clauses.iter().any(|clause| {
        clause.resource.as_str() == "ORDER_LINES"
            && matches!(&clause.key_match, KeyMatch::Unresolved)
    }));
}
