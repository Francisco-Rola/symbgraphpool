use std::sync::Arc;

use acg_candidate_graph::CandidateGraphBuilder;
use acg_core::{ContractCodeHash, RuntimeId, TxIndex};
use acg_cosmwasm_adapter::{CosmWasmAdapterConfig, CosmWasmCandidateAdapter};
use acg_cosmwasm_engine::{
    Address, BlockContext, CosmWasmEngine, NativeCallContext, NativeContract, TransactionId,
};
use acg_predicate::PredicateResult;
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use acg_validator_sim::{BlockProducer, BlockProducerConfig, Mempool};
use cosmwasm_std::{to_json_binary, Binary, Empty, Env, MessageInfo, Reply, Response};
use serde_json::{json, Value};

const MINIWAREHOUSE: &[u8] =
    include_bytes!("../../../../benchmarks/symbolic/miniwarehouse.symbolic.json");

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

fn setup() -> (
    CosmWasmEngine,
    Address,
    ProfileGraph,
    CosmWasmCandidateAdapter,
) {
    let engine = CosmWasmEngine::default();
    let code_id = engine
        .register_native("miniwarehouse-adapter-test", Arc::new(NoopContract))
        .unwrap();
    let checksum = engine.code_metadata(code_id).unwrap().checksum;
    let contract = engine
        .instantiate(
            TransactionId(900),
            BlockContext::default(),
            Address::new("creator"),
            code_id,
            None,
            "miniwarehouse".to_owned(),
            Vec::new(),
            Binary::from(br#"{}"#.to_vec()),
        )
        .unwrap()
        .contract;

    let context = IngestionContext::new(
        RuntimeId::new("cosmwasm").unwrap(),
        ContractCodeHash(*checksum.as_bytes()),
        1,
    );
    let profiles = normalize_document(parse_slice(MINIWAREHOUSE).unwrap(), &context).unwrap();
    let artifact = ProfileGraphArtifact::compile(profiles, &EdgeBuildConfig::default()).unwrap();
    let graph = ProfileGraph::load(artifact, GraphLoadConfig::default()).unwrap();
    let adapter = CosmWasmCandidateAdapter::new(
        CosmWasmAdapterConfig::new(RuntimeId::new("cosmwasm").unwrap(), 1).unwrap(),
    );
    (engine, contract, graph, adapter)
}

fn execute_request(
    id: u64,
    contract: Address,
    variant: Value,
) -> acg_cosmwasm_engine::ExecutionRequest {
    acg_cosmwasm_engine::ExecutionRequest::Execute {
        transaction_id: TransactionId(id),
        sender: Address::new("client"),
        contract,
        funds: Vec::new(),
        msg: to_json_binary(&variant).unwrap(),
    }
}

#[test]
fn all_miniwarehouse_execute_variants_resolve_to_profiles() {
    let (engine, contract, graph, adapter) = setup();
    let variants = vec![
        json!({"seed_warehouse":{"warehouse_id":1,"tax_bps":100}}),
        json!({"seed_district":{"warehouse_id":1,"district_id":1,"tax_bps":50,"next_order_id":1}}),
        json!({
            "seed_customer": {
                "warehouse_id": 1,
                "district_id": 1,
                "customer_id": 1,
                "discount_bps": 0
            }
        }),
        json!({"seed_stock":{"warehouse_id":1,"item_id":1,"quantity":100}}),
        json!({"restock":{"warehouse_id":1,"item_id":1,"quantity":10}}),
        json!({"new_order":{"warehouse_id":1,"district_id":1,"customer_id":1,"order_id":1,
            "lines":[{"item_id":1,"supply_warehouse_id":1,"quantity":1,"unit_price":"1"}]}}),
        json!({
            "payment": {
                "warehouse_id": 1,
                "district_id": 1,
                "customer_id": 1,
                "amount": "5",
                "history_id": 1
            }
        }),
        json!({"delivery":{"warehouse_id":1,"district_id":1,"order_id":1,"carrier_id":2}}),
    ];
    let expected = [
        "execute::SeedWarehouse",
        "execute::SeedDistrict",
        "execute::SeedCustomer",
        "execute::SeedStock",
        "execute::Restock",
        "execute::NewOrder",
        "execute::Payment",
        "execute::Delivery",
    ];
    let mempool = Mempool::default();
    for (offset, variant) in variants.into_iter().enumerate() {
        mempool.admit(
            execute_request(offset as u64 + 1, contract.clone(), variant),
            0,
        );
    }
    let mut producer = BlockProducer::fifo(BlockProducerConfig::default()).unwrap();
    let block = producer.produce_next(&mempool);
    let candidates = adapter.adapt_block(&engine, &graph, &block).unwrap();
    let actual = candidates
        .iter()
        .map(|candidate| {
            graph
                .profile(candidate.profile_id)
                .unwrap()
                .definition
                .entrypoint_name
                .as_str()
        })
        .collect::<Vec<_>>();
    assert_eq!(actual, expected.to_vec());
}

#[test]
fn new_order_nested_lines_are_preserved_as_candidate_bindings() {
    let (engine, contract, graph, adapter) = setup();
    let mempool = Mempool::default();
    mempool.admit(
        execute_request(
            1,
            contract,
            json!({"new_order":{
                "warehouse_id":1,"district_id":2,"customer_id":3,"order_id":4,
                "lines":[
                    {"item_id":7,"supply_warehouse_id":2,"quantity":1,"unit_price":"3"},
                    {"item_id":8,"supply_warehouse_id":1,"quantity":2,"unit_price":"5"}
                ]
            }}),
        ),
        0,
    );
    let mut producer = BlockProducer::fifo(BlockProducerConfig::default()).unwrap();
    let block = producer.produce_next(&mempool);
    let candidates = adapter.adapt_block(&engine, &graph, &block).unwrap();
    let bindings = candidates[0].input_bindings.root();

    assert_eq!(bindings["warehouse_id"], 1);
    assert_eq!(bindings["district_id"], 2);
    assert_eq!(bindings["lines"][0]["supply_warehouse_id"], 2);
    assert_eq!(bindings["lines"][0]["item_id"], 7);
    assert_eq!(bindings["lines"][1]["item_id"], 8);
}

#[test]
fn runtime_block_detects_remote_stock_conflict_and_prunes_nonmatching_stock() {
    let (engine, contract, graph, adapter) = setup();
    let mempool = Mempool::default();
    mempool.admit(
        execute_request(
            1,
            contract.clone(),
            json!({"new_order":{
                "warehouse_id":1,"district_id":1,"customer_id":1,"order_id":1,
                "lines":[{"item_id":7,"supply_warehouse_id":2,"quantity":1,"unit_price":"1"}]
            }}),
        ),
        0,
    );
    mempool.admit(
        execute_request(
            2,
            contract.clone(),
            json!({"restock":{"warehouse_id":2,"item_id":7,"quantity":5}}),
        ),
        0,
    );
    mempool.admit(
        execute_request(
            3,
            contract,
            json!({"restock":{"warehouse_id":1,"item_id":7,"quantity":5}}),
        ),
        0,
    );

    let mut producer = BlockProducer::fifo(BlockProducerConfig::default()).unwrap();
    let block = producer.produce_next(&mempool);
    let candidates = adapter.adapt_block(&engine, &graph, &block).unwrap();
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
}
