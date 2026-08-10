use std::sync::Arc;

use acg_candidate_graph::CandidateGraphBuilder;
use acg_core::{ContractCodeHash, RuntimeId, TxIndex};
use acg_cosmwasm_adapter::{CosmWasmAdapterConfig, CosmWasmCandidateAdapter};
use acg_cosmwasm_engine::{
    Address, BlockContext, CosmWasmEngine, ExecutionRequest, NativeCallContext, NativeContract,
    TransactionId,
};
use acg_miniwarehouse_workload::{
    decode_execute_request, MiniWarehouseExecuteMsg, MiniWarehouseMix, MiniWarehouseScale,
    MiniWarehouseWorkloadConfig, MiniWarehouseWorkloadGenerator,
};
use acg_predicate::PredicateResult;
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use acg_validator_sim::{BlockProducer, BlockProducerConfig, Mempool};
use cosmwasm_std::{to_json_binary, Binary, Empty, Env, MessageInfo, Reply, Response};

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
        .register_native("miniwarehouse-workload-pipeline", Arc::new(NoopContract))
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

fn restock_request(
    transaction_id: u64,
    contract: Address,
    warehouse_id: u64,
    item_id: u64,
) -> ExecutionRequest {
    ExecutionRequest::Execute {
        transaction_id: TransactionId(transaction_id),
        sender: Address::new("client"),
        contract,
        funds: Vec::new(),
        msg: to_json_binary(&MiniWarehouseExecuteMsg::Restock {
            warehouse_id,
            item_id,
            quantity: 5,
        })
        .unwrap(),
    }
}

#[test]
fn generated_new_order_flows_through_adapter_into_exact_stock_edges() {
    let (engine, contract, graph, adapter) = setup();
    let mut config = MiniWarehouseWorkloadConfig::for_contract(contract.clone());
    config.scale = MiniWarehouseScale {
        warehouse_count: 2,
        districts_per_warehouse: 1,
        customers_per_district: 2,
        items_per_warehouse: 4,
    };
    config.mix = MiniWarehouseMix {
        new_order: 1,
        payment: 0,
        delivery: 0,
        restock: 0,
    };
    config.min_order_lines = 1;
    config.max_order_lines = 1;
    config.remote_stock_probability_bps = 10_000;

    let mut generator = MiniWarehouseWorkloadGenerator::new(config).unwrap();
    let generated = generator.generate_one().unwrap();
    let MiniWarehouseExecuteMsg::NewOrder { lines, .. } =
        decode_execute_request(&generated.request).unwrap()
    else {
        panic!("expected generated NewOrder");
    };
    let line = &lines[0];
    let nonmatching_item = if line.item_id == 1 { 2 } else { 1 };

    let mempool = Mempool::default();
    mempool.admit(generated.request, 0);
    mempool.admit(
        restock_request(
            1_000,
            contract.clone(),
            line.supply_warehouse_id,
            line.item_id,
        ),
        1,
    );
    mempool.admit(
        restock_request(1_001, contract, line.supply_warehouse_id, nonmatching_item),
        2,
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
