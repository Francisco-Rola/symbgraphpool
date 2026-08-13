use std::sync::Arc;

use acg_candidate_graph::{EdgeClass, RiskBoundedSchedulerConfig};
use acg_core::{ContractCodeHash, RuntimeId, TxIndex};
use acg_cosmwasm_adapter::{CosmWasmAdapterConfig, CosmWasmCandidateAdapter};
use acg_cosmwasm_engine::{
    Address, BlockContext, CosmWasmEngine, ExecutionRequest, NativeCallContext, NativeContract,
    TransactionId,
};
use acg_feedback::AdaptiveFeedbackConfig;
use acg_miniwarehouse_workload::{MiniWarehouseExecuteMsg, MiniWarehouseNewOrderLine};
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_runtime_feedback::{
    AdaptivePlanningConfig, AdaptiveSerialPipeline, RuntimeFeedbackEngine, RuntimeFeedbackWeights,
    TraceConflictConfig,
};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use acg_validator_sim::{BlockProducer, BlockProducerConfig, Mempool};
use cosmwasm_std::{to_json_binary, Binary, Empty, Env, MessageInfo, Reply, Response, Uint128};

const MINIWAREHOUSE: &[u8] =
    include_bytes!("../../../../benchmarks/symbolic/miniwarehouse.symbolic.json");

/// Deliberately produces no state accesses. This fixture turns a symbolic NewOrder/Restock stock
/// edge into repeated concrete independence so the test can verify that Brick 4D feeds execution
/// evidence back into the next block's schedule.
struct NoopWarehouseRuntime;

impl NativeContract for NoopWarehouseRuntime {
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
        Err("reply not used".to_owned())
    }
}

fn new_order(id: u64, contract: &Address, order_id: u64) -> ExecutionRequest {
    ExecutionRequest::Execute {
        transaction_id: TransactionId(id),
        sender: Address::new("client"),
        contract: contract.clone(),
        funds: Vec::new(),
        msg: to_json_binary(&MiniWarehouseExecuteMsg::NewOrder {
            warehouse_id: 1,
            district_id: 1,
            customer_id: 1,
            order_id,
            lines: vec![MiniWarehouseNewOrderLine {
                item_id: 7,
                supply_warehouse_id: 2,
                quantity: 1,
                unit_price: Uint128::new(1),
            }],
        })
        .unwrap(),
    }
}

fn restock(id: u64, contract: &Address) -> ExecutionRequest {
    ExecutionRequest::Execute {
        transaction_id: TransactionId(id),
        sender: Address::new("client"),
        contract: contract.clone(),
        funds: Vec::new(),
        msg: to_json_binary(&MiniWarehouseExecuteMsg::Restock {
            warehouse_id: 2,
            item_id: 7,
            quantity: 5,
        })
        .unwrap(),
    }
}

fn setup() -> (
    CosmWasmEngine,
    Address,
    ProfileGraph,
    AdaptiveSerialPipeline,
    BlockProducer,
    Mempool,
) {
    let engine = CosmWasmEngine::default();
    let code_id = engine
        .register_native("miniwarehouse-brick4d", Arc::new(NoopWarehouseRuntime))
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
            Binary::default(),
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
    let feedback = RuntimeFeedbackEngine::new(
        &graph,
        0,
        TraceConflictConfig::default(),
        RuntimeFeedbackWeights::default(),
        AdaptiveFeedbackConfig {
            retention_factor: 1.0,
            ..AdaptiveFeedbackConfig::default()
        },
    )
    .unwrap();
    let pipeline = AdaptiveSerialPipeline::new(
        adapter,
        feedback,
        AdaptivePlanningConfig {
            edge_materialization_threshold: 0.0,
            scheduler: RiskBoundedSchedulerConfig {
                soft_threshold: 0.20,
                hard_threshold: 0.70,
                risk_budget: 0.40,
                max_wave_width: None,
                independent_observations_before_softening: 1,
            },
            cost_policy: Default::default(),
        },
    )
    .unwrap();

    (
        engine,
        contract,
        graph,
        pipeline,
        BlockProducer::fifo(BlockProducerConfig::default()).unwrap(),
        Mempool::default(),
    )
}

fn produce_pair(
    producer: &mut BlockProducer,
    mempool: &Mempool,
    contract: &Address,
    first_id: u64,
) -> acg_validator_sim::ProducedBlock {
    mempool.admit(new_order(first_id, contract, first_id), first_id);
    mempool.admit(restock(first_id + 1, contract), first_id + 1);
    producer.produce_next(mempool)
}

#[test]
fn miniwarehouse_runtime_independence_changes_future_wave_placement() {
    let (engine, contract, graph, mut pipeline, mut producer, mempool) = setup();

    let first_block = produce_pair(&mut producer, &mempool, &contract, 1);
    let first = pipeline.run_block(&engine, &graph, &first_block).unwrap();
    let first_edge = first
        .plan
        .candidate_graph
        .edge_between(TxIndex(0), TxIndex(1))
        .unwrap();
    let edge_index = first_edge.profile_edge_index().unwrap();
    assert_eq!(
        pipeline.planning_config().scheduler.classify(first_edge),
        EdgeClass::Hard
    );
    assert_eq!(first.plan.schedule.waves.len(), 2);
    assert_eq!(first.feedback_summary.negative_observations, 1);

    let second_block = produce_pair(&mut producer, &mempool, &contract, 3);
    let second = pipeline.run_block(&engine, &graph, &second_block).unwrap();
    let second_edge = second
        .plan
        .candidate_graph
        .edge_between(TxIndex(0), TxIndex(1))
        .unwrap();
    assert_eq!(
        pipeline.planning_config().scheduler.classify(second_edge),
        EdgeClass::Soft
    );
    assert!(second_edge.probability() > pipeline.planning_config().scheduler.risk_budget);
    assert_eq!(second.plan.schedule.waves.len(), 2);
    assert_eq!(second.feedback_summary.negative_observations, 1);

    let third_block = produce_pair(&mut producer, &mempool, &contract, 5);
    let third = pipeline.run_block(&engine, &graph, &third_block).unwrap();
    let third_edge = third
        .plan
        .candidate_graph
        .edge_between(TxIndex(0), TxIndex(1))
        .unwrap();
    assert_eq!(
        pipeline.planning_config().scheduler.classify(third_edge),
        EdgeClass::Soft
    );
    assert!(third_edge.probability() <= pipeline.planning_config().scheduler.risk_budget);
    assert_eq!(third.plan.schedule.waves.len(), 1);
    assert_eq!(
        third.plan.schedule.waves[0].transaction_indices,
        vec![TxIndex(0), TxIndex(1)]
    );

    let statistics = pipeline
        .feedback_store()
        .static_statistics(edge_index)
        .unwrap();
    assert_eq!(statistics.negative_observations, 3);
    assert!(third_edge.probability() < first_edge.probability());

    // Even after the adaptive planner emits a wide wave, actual Brick 4D execution remains serial.
    assert_eq!(third.execution_plan.waves.len(), 2);
    assert_eq!(third.execution_plan.waves[0].transaction_indices, vec![0]);
    assert_eq!(third.execution_plan.waves[1].transaction_indices, vec![1]);
    assert_eq!(third.execution_report.successful(), 2);
}
