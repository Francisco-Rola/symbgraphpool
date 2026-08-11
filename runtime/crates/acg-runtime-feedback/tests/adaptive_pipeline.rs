use std::sync::Arc;

use acg_candidate_graph::{EdgeClass, RiskBoundedSchedulerConfig};
use acg_core::{ContractCodeHash, RuntimeId, TxIndex};
use acg_cosmwasm_adapter::{CosmWasmAdapterConfig, CosmWasmCandidateAdapter};
use acg_cosmwasm_engine::{
    Address, BlockContext, CosmWasmEngine, ExecutionRequest, NativeCallContext, NativeContract,
    TransactionId,
};
use acg_feedback::AdaptiveFeedbackConfig;
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_runtime_feedback::{
    AdaptivePlanningConfig, AdaptiveSerialPipeline, RuntimeFeedbackEngine, RuntimeFeedbackWeights,
    TraceConflictConfig,
};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use acg_validator_sim::{BlockProducer, BlockProducerConfig, Mempool};
use cosmwasm_std::{to_json_binary, Binary, Empty, Env, MessageInfo, Reply, Response};
use serde_json::{json, Value};

const CONFLICTLAB: &[u8] =
    include_bytes!("../../../../benchmarks/symbolic/conflictlab.symbolic.json");

struct ConflictLabRuntime;

impl NativeContract for ConflictLabRuntime {
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
        context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        msg: Binary,
    ) -> Result<Response<Empty>, String> {
        let value: Value =
            serde_json::from_slice(msg.as_slice()).map_err(|error| error.to_string())?;
        let credit = value
            .get("credit")
            .and_then(Value::as_object)
            .ok_or_else(|| "expected credit message".to_owned())?;
        let account = credit
            .get("account")
            .and_then(Value::as_str)
            .ok_or_else(|| "credit account missing".to_owned())?;
        let amount = credit
            .get("amount")
            .and_then(Value::as_u64)
            .ok_or_else(|| "credit amount missing".to_owned())?;
        let key = format!("balance/{account}");
        let current = context
            .storage_get(key.as_bytes())
            .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
            .map(u64::from_be_bytes)
            .unwrap_or_default();
        context.storage_set(key.as_bytes(), current.saturating_add(amount).to_be_bytes());
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

fn credit(id: u64, contract: &Address, account: &str) -> ExecutionRequest {
    ExecutionRequest::Execute {
        transaction_id: TransactionId(id),
        sender: Address::new("client"),
        contract: contract.clone(),
        funds: Vec::new(),
        msg: to_json_binary(&json!({
            "credit": {
                "account": account,
                "amount": 1_u64
            }
        }))
        .unwrap(),
    }
}

fn setup() -> (
    CosmWasmEngine,
    Address,
    ProfileGraph,
    AdaptiveSerialPipeline,
) {
    let engine = CosmWasmEngine::default();
    let code_id = engine
        .register_native("conflictlab-brick4d", Arc::new(ConflictLabRuntime))
        .unwrap();
    let checksum = engine.code_metadata(code_id).unwrap().checksum;
    let contract = engine
        .instantiate(
            TransactionId(900),
            BlockContext::default(),
            Address::new("creator"),
            code_id,
            None,
            "conflictlab".to_owned(),
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
    let profiles = normalize_document(parse_slice(CONFLICTLAB).unwrap(), &context).unwrap();
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
                soft_threshold: 0.30,
                hard_threshold: 0.70,
                risk_budget: 0.30,
                max_wave_width: None,
                independent_observations_before_softening: 8,
            },
        },
    )
    .unwrap();

    (engine, contract, graph, pipeline)
}

#[test]
fn conflictlab_adaptive_plan_groups_independent_work_but_executes_canonically_and_learns() {
    let (engine, contract, graph, mut pipeline) = setup();
    let mempool = Mempool::default();
    mempool.admit(credit(1, &contract, "alice"), 0);
    mempool.admit(credit(2, &contract, "alice"), 1);
    mempool.admit(credit(3, &contract, "bob"), 2);
    let block = BlockProducer::fifo(BlockProducerConfig::default())
        .unwrap()
        .produce_next(&mempool);

    let preview = pipeline.plan_block(&engine, &graph, &block).unwrap();
    let preview_edge_index = preview
        .candidate_graph
        .edge_between(TxIndex(0), TxIndex(1))
        .unwrap()
        .profile_edge_index()
        .unwrap();
    let before_preview = *pipeline
        .feedback_store()
        .static_statistics(preview_edge_index)
        .unwrap();
    assert_eq!(
        *pipeline
            .feedback_store()
            .static_statistics(preview_edge_index)
            .unwrap(),
        before_preview
    );

    let run = pipeline.run_block(&engine, &graph, &block).unwrap();
    let alice_edge = run
        .plan
        .candidate_graph
        .edge_between(TxIndex(0), TxIndex(1))
        .unwrap();
    assert_eq!(
        pipeline.planning_config().scheduler.classify(alice_edge),
        EdgeClass::Hard
    );
    assert!(run
        .plan
        .candidate_graph
        .edge_between(TxIndex(0), TxIndex(2))
        .is_none());
    assert!(run
        .plan
        .candidate_graph
        .edge_between(TxIndex(1), TxIndex(2))
        .is_none());

    assert_eq!(run.plan.schedule.waves.len(), 2);
    assert_eq!(
        run.plan.schedule.waves[0].transaction_indices,
        vec![TxIndex(0), TxIndex(2)]
    );
    assert_eq!(
        run.plan.schedule.waves[1].transaction_indices,
        vec![TxIndex(1)]
    );
    assert_eq!(run.plan.max_wave_width(), 2);
    assert_eq!(
        run.plan.speculative_execution_plan.waves[0].transaction_indices,
        vec![0, 2]
    );

    // Brick 4D never hands the wide speculative plan to the serial executor.
    assert_eq!(run.execution_plan.waves.len(), 3);
    assert_eq!(run.execution_plan.waves[0].transaction_indices, vec![0]);
    assert_eq!(run.execution_plan.waves[1].transaction_indices, vec![1]);
    assert_eq!(run.execution_plan.waves[2].transaction_indices, vec![2]);
    assert_eq!(
        run.execution_report
            .transactions
            .iter()
            .map(|execution| execution.transaction_id)
            .collect::<Vec<_>>(),
        vec![TransactionId(1), TransactionId(2), TransactionId(3)]
    );
    assert_eq!(run.execution_report.successful(), 3);
    assert_eq!(run.feedback_summary.positive_observations, 1);
    assert_eq!(run.feedback_summary.negative_observations, 0);

    let edge_index = alice_edge.profile_edge_index().unwrap();
    let learned = pipeline
        .feedback_store()
        .static_statistics(edge_index)
        .unwrap();
    assert_eq!(learned.positive_observations, 1);
    assert!(learned.probability() > alice_edge.probability());
}
