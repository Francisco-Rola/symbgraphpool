use acg_candidate_graph::{CandidateGraphBuilder, CandidateTransaction};
use acg_core::{
    ClauseResolution, ContractCodeHash, DependencyKind, EdgeRelation, GuardRef, InstanceId,
    KeyMatch, PredicateClause, ResourceFamily, RuntimeId, TxId, TxIndex, UnknownReason,
};
use acg_predicate::{InputBindings, PredicateResult};
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use serde_json::json;

const CONFLICTLAB: &[u8] = include_bytes!("../../../benchmarks/symbolic/conflictlab.symbolic.json");

fn profile_graph() -> ProfileGraph {
    let raw = parse_slice(CONFLICTLAB).unwrap();
    let context = IngestionContext::new(
        RuntimeId::new("cosmwasm").unwrap(),
        ContractCodeHash([7; 32]),
        1,
    );
    let profiles = normalize_document(raw, &context).unwrap();
    let artifact = ProfileGraphArtifact::compile(profiles, &EdgeBuildConfig::default()).unwrap();
    ProfileGraph::load(artifact, GraphLoadConfig::default()).unwrap()
}

fn mixed_relation_graph() -> ProfileGraph {
    let raw = parse_slice(CONFLICTLAB).unwrap();
    let context = IngestionContext::new(
        RuntimeId::new("cosmwasm").unwrap(),
        ContractCodeHash([8; 32]),
        1,
    );
    let profiles = normalize_document(raw, &context).unwrap();
    let credit_key = profiles
        .iter()
        .find(|profile| profile.entrypoint_name == "execute::Credit")
        .unwrap()
        .stable_key;
    let mut artifact =
        ProfileGraphArtifact::compile(profiles, &EdgeBuildConfig::default()).unwrap();
    let edge = artifact
        .edges
        .iter_mut()
        .find(|edge| edge.source == credit_key && edge.target == credit_key)
        .unwrap();
    edge.predicate.clauses.push(PredicateClause {
        resource: ResourceFamily("BALANCES".to_owned()),
        semantic_key_component: "account".to_owned(),
        resolution: ClauseResolution::Unknown,
        unknown_reasons: vec![UnknownReason::StateDerivedKey],
        require_same_contract_instance: true,
        key_match: KeyMatch::Unresolved,
        left_guard: GuardRef {
            expression: "true".to_owned(),
            dependency_kind: DependencyKind::None,
            delegation_path: Vec::new(),
        },
        right_guard: GuardRef {
            expression: "true".to_owned(),
            dependency_kind: DependencyKind::None,
            delegation_path: Vec::new(),
        },
    });
    edge.predicate.clauses.sort();
    edge.relation = EdgeRelation::summarize_clauses(&edge.predicate.clauses);
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

#[test]
fn exact_input_keys_prune_same_profile_cartesian_pairs() {
    let graph = profile_graph();
    let builder = CandidateGraphBuilder::new(&graph);
    let candidate = builder
        .build(vec![
            tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
            tx(&graph, 2, "execute::Credit", 1, json!({"account":"bob"})),
            tx(&graph, 3, "execute::Credit", 1, json!({"account":"alice"})),
        ])
        .unwrap();

    assert!(candidate.edge_between(TxIndex(0), TxIndex(1)).is_none());
    let edge = candidate.edge_between(TxIndex(0), TxIndex(2)).unwrap();
    assert_eq!(edge.predicate_result, PredicateResult::True);
    assert_eq!(candidate.edge_between(TxIndex(2), TxIndex(0)), Some(edge));
    assert!(candidate.edge_between(TxIndex(1), TxIndex(2)).is_none());
}

#[test]
fn mixed_unknown_profile_edge_still_uses_clause_level_or() {
    let graph = mixed_relation_graph();
    let builder = CandidateGraphBuilder::new(&graph);
    let same_key = builder
        .build(vec![
            tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
            tx(&graph, 2, "execute::Credit", 1, json!({"account":"alice"})),
        ])
        .unwrap();
    assert_eq!(
        same_key.edges()[0].predicate_result,
        PredicateResult::True,
        "a true precise clause must dominate an alternative unknown clause"
    );

    let different_key = builder
        .build(vec![
            tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
            tx(&graph, 2, "execute::Credit", 1, json!({"account":"bob"})),
        ])
        .unwrap();
    assert_eq!(different_key.edges().len(), 1);
    assert_eq!(
        different_key.edges()[0].predicate_result,
        PredicateResult::Unknown,
        "false precise clauses cannot suppress an unresolved alternative"
    );
}

#[test]
fn same_profile_shards_only_conflict_on_equal_shard_id() {
    let graph = profile_graph();
    let builder = CandidateGraphBuilder::new(&graph);
    let candidate = builder
        .build(vec![
            tx(
                &graph,
                1,
                "execute::IncrementCounter",
                1,
                json!({"shard_id":1}),
            ),
            tx(
                &graph,
                2,
                "execute::IncrementCounter",
                1,
                json!({"shard_id":2}),
            ),
            tx(
                &graph,
                3,
                "execute::IncrementCounter",
                1,
                json!({"shard_id":1}),
            ),
        ])
        .unwrap();

    assert!(candidate.edge_between(TxIndex(0), TxIndex(1)).is_none());
    assert_eq!(
        candidate
            .edge_between(TxIndex(0), TxIndex(2))
            .unwrap()
            .predicate_result,
        PredicateResult::True
    );
}

#[test]
fn contract_instance_identity_prunes_disjoint_storage() {
    let graph = profile_graph();
    let builder = CandidateGraphBuilder::new(&graph);
    let candidate = builder
        .build(vec![
            tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
            tx(&graph, 2, "execute::Credit", 2, json!({"account":"alice"})),
        ])
        .unwrap();
    assert!(candidate.edges().is_empty());
}

#[test]
fn unresolved_wildcard_edges_are_materialized_conservatively() {
    let graph = profile_graph();
    let builder = CandidateGraphBuilder::new(&graph);
    let candidate = builder
        .build(vec![
            tx(
                &graph,
                1,
                "execute::ResetAllBalances",
                1,
                json!({"info":{"sender":"admin"}}),
            ),
            tx(&graph, 2, "execute::Credit", 1, json!({"account":"alice"})),
        ])
        .unwrap();
    assert_eq!(candidate.edges().len(), 1);
    assert_eq!(
        candidate.edges()[0].predicate_result,
        PredicateResult::Unknown
    );
}

#[test]
fn input_only_guard_can_eliminate_a_profile_edge() {
    let graph = profile_graph();
    let builder = CandidateGraphBuilder::new(&graph);
    let candidate = builder
        .build(vec![
            tx(
                &graph,
                1,
                "execute::Transfer",
                1,
                json!({"from":"alice", "to":"bob", "amount":"5", "info":{"sender":"mallory"}}),
            ),
            tx(&graph, 2, "execute::Credit", 1, json!({"account":"alice"})),
        ])
        .unwrap();
    assert!(candidate.edges().is_empty());
}

#[test]
fn adjacency_is_symmetric_for_materialized_edges() {
    let graph = profile_graph();
    let builder = CandidateGraphBuilder::new(&graph);
    let candidate = builder
        .build(vec![
            tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"})),
            tx(&graph, 2, "execute::Credit", 1, json!({"account":"alice"})),
        ])
        .unwrap();
    assert_eq!(candidate.neighbors(TxIndex(0)).len(), 1);
    assert_eq!(candidate.neighbors(TxIndex(1)).len(), 1);
    assert_eq!(candidate.neighbors(TxIndex(0))[0].neighbor, TxIndex(1));
    assert_eq!(candidate.neighbors(TxIndex(1))[0].neighbor, TxIndex(0));
}

#[test]
fn rejects_invalid_candidate_metadata() {
    let graph = profile_graph();
    let builder = CandidateGraphBuilder::new(&graph);
    let mut invalid = tx(&graph, 1, "execute::Credit", 1, json!({"account":"alice"}));
    invalid.inclusion_probability = 1.5;
    assert!(builder.build(vec![invalid]).is_err());

    let mut unknown = tx(&graph, 2, "execute::Credit", 1, json!({"account":"alice"}));
    unknown.profile_id = acg_core::ProfileId(u32::MAX);
    assert!(builder.build(vec![unknown]).is_err());
}
