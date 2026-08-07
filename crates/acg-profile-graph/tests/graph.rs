use acg_core::{ContractCodeHash, EdgeRelation, KeyMatch, RuntimeId};
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};

const FIXTURE: &[u8] = include_bytes!(
    "../../acg-symbolic-json/tests/fixtures/astroport_pair_compact_config_fields.json"
);

fn compile_artifact() -> ProfileGraphArtifact {
    let raw = parse_slice(FIXTURE).unwrap();
    let context = IngestionContext::new(
        RuntimeId::new("cosmwasm").unwrap(),
        ContractCodeHash([9; 32]),
        1,
    );
    let profiles = normalize_document(raw, &context).unwrap();
    ProfileGraphArtifact::compile(profiles, &EdgeBuildConfig::default()).unwrap()
}

#[test]
fn loads_profiles_with_dense_deterministic_ids() {
    let artifact = compile_artifact();
    let mut reordered = artifact.clone();
    reordered.profiles.reverse();
    reordered.edges.reverse();

    let first = ProfileGraph::load(artifact, GraphLoadConfig::default()).unwrap();
    let second = ProfileGraph::load(reordered, GraphLoadConfig::default()).unwrap();

    let first_keys: Vec<_> = first
        .profiles()
        .iter()
        .map(|record| (record.id, record.definition.stable_key))
        .collect();
    let second_keys: Vec<_> = second
        .profiles()
        .iter()
        .map(|record| (record.id, record.definition.stable_key))
        .collect();
    assert_eq!(first_keys, second_keys);
    assert_eq!(first.profiles().len(), 20);
    assert_eq!(first.edges().len(), 66);
    assert_eq!(
        first
            .edges()
            .iter()
            .filter(|edge| edge.relation == EdgeRelation::Conditional)
            .count(),
        55
    );
    assert_eq!(
        first
            .edges()
            .iter()
            .filter(|edge| edge.relation == EdgeRelation::Unknown)
            .count(),
        11
    );
    assert!(first
        .profiles()
        .windows(2)
        .all(|pair| pair[0].definition.stable_key < pair[1].definition.stable_key));
}

#[test]
fn excludes_read_read_profile_pairs() {
    let graph = ProfileGraph::load(compile_artifact(), GraphLoadConfig::default()).unwrap();
    let pair = graph
        .profiles()
        .iter()
        .find(|record| record.definition.entrypoint_name == "query::Pair")
        .unwrap();
    let pool = graph
        .profiles()
        .iter()
        .find(|record| record.definition.entrypoint_name == "query::Pool")
        .unwrap();

    assert!(!graph.edges().iter().any(|edge| {
        (edge.source == pair.id && edge.target == pool.id)
            || (edge.source == pool.id && edge.target == pair.id)
    }));
}

#[test]
fn derives_input_key_equality_for_balance_conflicts() {
    let graph = ProfileGraph::load(compile_artifact(), GraphLoadConfig::default()).unwrap();
    let swap = graph
        .profiles()
        .iter()
        .find(|record| record.definition.entrypoint_name == "execute::Swap")
        .unwrap();
    let balance_query = graph
        .profiles()
        .iter()
        .find(|record| record.definition.entrypoint_name == "query::AssetBalanceAt")
        .unwrap();

    let edge = graph
        .edges()
        .iter()
        .find(|edge| {
            (edge.source == swap.id && edge.target == balance_query.id)
                || (edge.source == balance_query.id && edge.target == swap.id)
        })
        .unwrap();
    assert_eq!(edge.relation, EdgeRelation::Conditional);
    assert!(edge.predicate.clauses.iter().any(|clause| {
        clause.resource.as_str() == "BALANCES"
            && matches!(
                &clause.key_match,
                KeyMatch::InputEquality { left, right }
                    if (left.expression.as_str() == "offer_asset.info"
                        && right.expression.as_str() == "asset_info")
                        || (left.expression.as_str() == "asset_info"
                            && right.expression.as_str() == "offer_asset.info")
            )
    }));
}

#[test]
fn initializes_beta_prior_arrays() {
    let graph = ProfileGraph::load(compile_artifact(), GraphLoadConfig::default()).unwrap();
    let first_edge = graph.edges().first().unwrap();
    let (alpha, beta) = graph.edge_prior(first_edge.index).unwrap();
    assert!(alpha > 0.0);
    assert!(beta > 0.0);
    assert!((alpha / (alpha + beta) - first_edge.symbolic_score).abs() < 0.1);
}

#[test]
fn artifact_json_round_trips_before_dense_id_assignment() {
    let artifact = compile_artifact();
    let bytes = artifact.to_pretty_json().unwrap();
    let decoded = ProfileGraphArtifact::from_json(&bytes).unwrap();
    assert_eq!(artifact, decoded);
    let graph = ProfileGraph::load(decoded, GraphLoadConfig::default()).unwrap();
    assert_eq!(graph.profiles().len(), 20);
    assert_eq!(graph.edges().len(), 66);
}

#[test]
fn self_profile_predicates_are_symmetric() {
    let graph = ProfileGraph::load(compile_artifact(), GraphLoadConfig::default()).unwrap();
    let swap = graph
        .profiles()
        .iter()
        .find(|record| record.definition.entrypoint_name == "execute::Swap")
        .unwrap();
    let edge = graph
        .edges()
        .iter()
        .find(|edge| edge.source == swap.id && edge.target == swap.id)
        .unwrap();

    let write_guard = "env.block.time.seconds() > CONFIG.block_time_last";
    assert!(edge.predicate.clauses.iter().any(|clause| {
        clause.semantic_key_component == "block_time_last"
            && clause.left_guard.expression == "true"
            && clause.right_guard.expression == write_guard
    }));
    assert!(edge.predicate.clauses.iter().any(|clause| {
        clause.semantic_key_component == "block_time_last"
            && clause.left_guard.expression == write_guard
            && clause.right_guard.expression == "true"
    }));
}
