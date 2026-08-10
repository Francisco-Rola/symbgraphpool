use std::sync::Arc;

use acg_candidate_graph::CandidateGraphBuilder;
use acg_core::{ContractCodeHash, EntrypointSelector, RuntimeId, TxIndex};
use acg_cosmwasm_adapter::{CosmWasmAdapterConfig, CosmWasmCandidateAdapter};
use acg_cosmwasm_engine::{
    Address, BlockContext, CosmWasmEngine, NativeCallContext, NativeContract, TransactionId,
};
use acg_predicate::PredicateResult;
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use acg_validator_sim::{BlockProducer, BlockProducerConfig, Mempool};
use cosmwasm_std::{to_json_binary, Binary, Empty, Env, MessageInfo, Reply, Response};
use serde_json::json;

const CONFLICTLAB: &[u8] =
    include_bytes!("../../../../benchmarks/symbolic/conflictlab.symbolic.json");

struct NoopContract;

impl NativeContract for NoopContract {
    fn instantiate(
        &self,
        _context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        _msg: Binary,
    ) -> Result<Response<Empty>, String> {
        Ok(Response::new())
    }

    fn execute(
        &self,
        _context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        _msg: Binary,
    ) -> Result<Response<Empty>, String> {
        Ok(Response::new())
    }

    fn query(
        &self,
        _context: &mut NativeCallContext,
        _env: Env,
        _msg: Binary,
    ) -> Result<Binary, String> {
        Ok(Binary::default())
    }

    fn reply(
        &self,
        _context: &mut NativeCallContext,
        _env: Env,
        _reply: Reply,
    ) -> Result<Response<Empty>, String> {
        Ok(Response::new())
    }
}

fn graph_for_code(checksum: acg_cosmwasm_engine::CodeChecksum) -> ProfileGraph {
    let raw = parse_slice(CONFLICTLAB).unwrap();
    let context = IngestionContext::new(
        RuntimeId::new("cosmwasm").unwrap(),
        ContractCodeHash(*checksum.as_bytes()),
        1,
    );
    let profiles = normalize_document(raw, &context).unwrap();
    let artifact = ProfileGraphArtifact::compile(profiles, &EdgeBuildConfig::default()).unwrap();
    ProfileGraph::load(artifact, GraphLoadConfig::default()).unwrap()
}

fn graph_for_code_with_credit_selector(
    checksum: acg_cosmwasm_engine::CodeChecksum,
    selector: EntrypointSelector,
) -> ProfileGraph {
    let raw = parse_slice(CONFLICTLAB).unwrap();
    let mut context = IngestionContext::new(
        RuntimeId::new("cosmwasm").unwrap(),
        ContractCodeHash(*checksum.as_bytes()),
        1,
    );
    context
        .selector_overrides
        .insert("execute::Credit".to_owned(), selector);
    let profiles = normalize_document(raw, &context).unwrap();
    let artifact = ProfileGraphArtifact::compile(profiles, &EdgeBuildConfig::default()).unwrap();
    ProfileGraph::load(artifact, GraphLoadConfig::default()).unwrap()
}

fn setup_instances() -> (
    CosmWasmEngine,
    acg_cosmwasm_engine::CodeId,
    Address,
    Address,
) {
    let engine = CosmWasmEngine::default();
    let code_id = engine
        .register_native("conflictlab-adapter-test", Arc::new(NoopContract))
        .unwrap();
    let first = engine
        .instantiate(
            TransactionId(100),
            BlockContext::default(),
            Address::new("creator"),
            code_id,
            None,
            "first".to_owned(),
            Vec::new(),
            Binary::from(br#"{}"#.to_vec()),
        )
        .unwrap()
        .contract;
    let second = engine
        .instantiate(
            TransactionId(101),
            BlockContext::default(),
            Address::new("creator"),
            code_id,
            None,
            "second".to_owned(),
            Vec::new(),
            Binary::from(br#"{}"#.to_vec()),
        )
        .unwrap()
        .contract;
    (engine, code_id, first, second)
}

fn execute_request(
    id: u64,
    sender: &str,
    contract: Address,
    variant: serde_json::Value,
) -> acg_cosmwasm_engine::ExecutionRequest {
    acg_cosmwasm_engine::ExecutionRequest::Execute {
        transaction_id: TransactionId(id),
        sender: Address::new(sender),
        contract,
        funds: Vec::new(),
        msg: to_json_binary(&variant).unwrap(),
    }
}

#[test]
fn runtime_requests_resolve_profiles_instances_and_bindings() {
    let (engine, code_id, first, second) = setup_instances();
    let checksum = engine.code_metadata(code_id).unwrap().checksum;
    let graph = graph_for_code(checksum);
    let adapter = CosmWasmCandidateAdapter::new(
        CosmWasmAdapterConfig::new(RuntimeId::new("cosmwasm").unwrap(), 1).unwrap(),
    );

    let mempool = Mempool::default();
    mempool.admit(
        execute_request(
            1,
            "alice",
            first.clone(),
            json!({"credit":{"account":"alice","amount":"5"}}),
        ),
        0,
    );
    mempool.admit(
        execute_request(
            2,
            "alice",
            first.clone(),
            json!({"credit":{"account":"alice","amount":"7"}}),
        ),
        0,
    );
    mempool.admit(
        execute_request(
            3,
            "bob",
            first.clone(),
            json!({"credit":{"account":"bob","amount":"9"}}),
        ),
        0,
    );
    mempool.admit(
        execute_request(
            4,
            "alice",
            second,
            json!({"credit":{"account":"alice","amount":"11"}}),
        ),
        0,
    );

    let mut producer = BlockProducer::fifo(BlockProducerConfig::default()).unwrap();
    let block = producer.produce_next(&mempool);
    let candidates = adapter.adapt_block(&engine, &graph, &block).unwrap();

    assert_eq!(candidates.len(), 4);
    assert_eq!(candidates[0].input_bindings.root()["account"], "alice");
    assert_eq!(
        candidates[0].input_bindings.root()["info"]["sender"],
        "alice"
    );
    assert_eq!(candidates[0].profile_id, candidates[1].profile_id);
    assert_eq!(candidates[0].instance_id, candidates[1].instance_id);
    assert_ne!(candidates[0].instance_id, candidates[3].instance_id);

    let candidate_graph = CandidateGraphBuilder::new(&graph)
        .build(candidates)
        .unwrap();
    assert_eq!(
        candidate_graph
            .edge_between(TxIndex(0), TxIndex(1))
            .unwrap()
            .predicate_result,
        PredicateResult::True
    );
    assert!(candidate_graph
        .edge_between(TxIndex(0), TxIndex(2))
        .is_none());
    assert!(candidate_graph
        .edge_between(TxIndex(0), TxIndex(3))
        .is_none());
}

#[test]
fn instantiate_request_resolves_instantiate_profile_and_unique_pending_instance() {
    let (engine, code_id, _, _) = setup_instances();
    let checksum = engine.code_metadata(code_id).unwrap().checksum;
    let graph = graph_for_code(checksum);
    let adapter = CosmWasmCandidateAdapter::new(
        CosmWasmAdapterConfig::new(RuntimeId::new("cosmwasm").unwrap(), 1).unwrap(),
    );
    let mempool = Mempool::default();
    for id in [10_u64, 11] {
        mempool.admit(
            acg_cosmwasm_engine::ExecutionRequest::Instantiate {
                transaction_id: TransactionId(id),
                sender: Address::new("creator"),
                code_id,
                admin: None,
                label: format!("contract-{id}"),
                funds: Vec::new(),
                msg: to_json_binary(&json!({"admin":null,"fee_bps":30,"epoch":1})).unwrap(),
            },
            0,
        );
    }
    let mut producer = BlockProducer::fifo(BlockProducerConfig::default()).unwrap();
    let block = producer.produce_next(&mempool);
    let candidates = adapter.adapt_block(&engine, &graph, &block).unwrap();
    assert_eq!(candidates.len(), 2);
    assert_ne!(candidates[0].instance_id, candidates[1].instance_id);
    assert_eq!(candidates[0].input_bindings.root()["msg"]["epoch"], 1);

    let expected_instance = candidates[0].instance_id;
    let outcome = engine
        .execute_request(block.context.clone(), block.transactions[0].request.clone())
        .unwrap();
    assert_eq!(
        adapter
            .instance_registry()
            .resolve_address(&outcome.contract)
            .unwrap(),
        expected_instance
    );
}

#[test]
fn selector_override_resolves_chain_native_profile_identity() {
    let (engine, code_id, first, _) = setup_instances();
    let checksum = engine.code_metadata(code_id).unwrap().checksum;
    let selector = EntrypointSelector(42);
    let graph = graph_for_code_with_credit_selector(checksum, selector);
    let mut config = CosmWasmAdapterConfig::new(RuntimeId::new("cosmwasm").unwrap(), 1).unwrap();
    config
        .selector_overrides
        .insert("execute::Credit".to_owned(), selector);
    let adapter = CosmWasmCandidateAdapter::new(config);
    let mempool = Mempool::default();
    mempool.admit(
        execute_request(
            20,
            "alice",
            first,
            json!({"credit":{"account":"alice","amount":"1"}}),
        ),
        0,
    );
    let mut producer = BlockProducer::fifo(BlockProducerConfig::default()).unwrap();
    let block = producer.produce_next(&mempool);
    let candidates = adapter.adapt_block(&engine, &graph, &block).unwrap();
    assert_eq!(candidates.len(), 1);
    let profile = graph.profile(candidates[0].profile_id).unwrap();
    assert_eq!(profile.definition.entrypoint_name, "execute::Credit");
    assert_eq!(
        profile.definition.descriptor.numeric_entrypoint_selector,
        selector
    );
}
