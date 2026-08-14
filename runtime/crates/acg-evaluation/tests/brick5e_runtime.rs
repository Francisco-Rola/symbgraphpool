use std::{
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use acg_candidate_graph::RiskBoundedSchedulerConfig;
use acg_core::{ContractCodeHash, RuntimeId, TxIndex};
use acg_cosmwasm_adapter::{CosmWasmAdapterConfig, CosmWasmCandidateAdapter};
use acg_cosmwasm_engine::{
    Address, BlockContext, CosmWasmEngine, ExecutionRequest, NativeCallContext, NativeContract,
    ParallelExecutionConfig, TransactionId,
};
use acg_evaluation::{
    CorrectnessRecord, ExperimentMetadata, ExperimentRecord, FeedbackTimingRecord,
    ParallelismReference,
};
use acg_feedback::{AdaptiveFeedbackConfig, ApplySummary};
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_runtime_feedback::{
    AdaptivePlanningConfig, AdaptiveSerialPipeline, RuntimeFeedbackEngine, RuntimeFeedbackWeights,
    TraceConflictConfig,
};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use acg_validator_sim::{
    BlockProducer, BlockProducerConfig, Mempool, SpeculativeParallelBlockExecutor,
};
use cosmwasm_std::{to_json_binary, Binary, Empty, Env, MessageInfo, Reply, Response};
use serde_json::{json, Value};

const CONFLICTLAB: &[u8] =
    include_bytes!("../../../../benchmarks/symbolic/conflictlab.symbolic.json");

struct SlowCreditRuntime;

impl NativeContract for SlowCreditRuntime {
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
        let key = format!("balance/{account}");
        let current = context
            .storage_get(key.as_bytes())
            .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
            .map(u64::from_be_bytes)
            .unwrap_or_default();
        thread::sleep(Duration::from_millis(2));
        context.storage_set(key.as_bytes(), current.saturating_add(1).to_be_bytes());
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
        msg: to_json_binary(&json!({"credit":{"account":account,"amount":1_u64}})).unwrap(),
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
        .register_native("conflictlab-brick5e", Arc::new(SlowCreditRuntime))
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
            confidence_scale: 1.0,
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
                hard_threshold: 0.80,
                risk_budget: 0.20,
                max_wave_width: None,
                independent_observations_before_softening: 8,
            },
            cost_policy: Default::default(),
        },
    )
    .unwrap();
    (engine, contract, graph, pipeline)
}

fn block(contract: &Address, first_id: u64, height: u64) -> acg_validator_sim::ProducedBlock {
    let mempool = Mempool::default();
    mempool.admit(credit(first_id, contract, "alice"), 0);
    mempool.admit(credit(first_id + 1, contract, "alice"), 1);
    let mut block = BlockProducer::fifo(BlockProducerConfig::default())
        .unwrap()
        .produce_next(&mempool);
    block.context.height = height;
    block
}

fn merge(left: ApplySummary, right: ApplySummary) -> ApplySummary {
    ApplySummary {
        positive_observations: left.positive_observations + right.positive_observations,
        negative_observations: left.negative_observations + right.negative_observations,
        fallback_edges_created: left.fallback_edges_created + right.fallback_edges_created,
        candidate_misses: left.candidate_misses + right.candidate_misses,
        replay_impact_observations: left.replay_impact_observations
            + right.replay_impact_observations,
        attributed_replay_cost_nanos: left
            .attributed_replay_cost_nanos
            .saturating_add(right.attributed_replay_cost_nanos),
        attributed_invalidated_descendants: left
            .attributed_invalidated_descendants
            .saturating_add(right.attributed_invalidated_descendants),
        observation_batches_applied: left
            .observation_batches_applied
            .saturating_add(right.observation_batches_applied),
        serialization_cost_observations: left
            .serialization_cost_observations
            .saturating_add(right.serialization_cost_observations),
        attributed_serialization_cost_nanos: left
            .attributed_serialization_cost_nanos
            .saturating_add(right.attributed_serialization_cost_nanos),
        serialization_cost_batches_applied: left
            .serialization_cost_batches_applied
            .saturating_add(right.serialization_cost_batches_applied),
    }
}

#[test]
fn brick5e_learns_marginal_serialization_cost_and_emits_stable_record() {
    let (engine, contract, graph, mut pipeline) = setup();
    let executor = SpeculativeParallelBlockExecutor::new(
        engine.clone(),
        ParallelExecutionConfig { workers: 2 },
    );

    // First block supplies the serialization observation.
    let first = block(&contract, 1, 1);
    let (first_plan, _) = pipeline
        .plan_block_with_metrics(&engine, &graph, &first)
        .unwrap();
    let first_edge_index = first_plan
        .candidate_graph
        .edge_between(TxIndex(0), TxIndex(1))
        .unwrap()
        .profile_edge_index()
        .unwrap();
    let first_prepared = executor
        .prepare(&first, &first_plan.speculative_execution_plan)
        .unwrap();
    let first_pre_report = executor
        .pre_execution_report(&first, &first_prepared)
        .unwrap();
    let first_attributions = pipeline
        .serialization_attributions(&first_plan, &first_pre_report)
        .unwrap();
    assert_eq!(first_attributions.len(), 1);
    assert!(first_attributions[0].marginal_ready_delay_nanos >= 1_000_000);
    let first_pre_summary = pipeline
        .process_pre_execution_report(&graph, &first_plan, &first_pre_report, 1)
        .unwrap();
    assert_eq!(first_pre_summary.serialization_cost_observations, 1);
    assert!(first_pre_summary.attributed_serialization_cost_nanos >= 1_000_000);
    let first_reconciliation = executor.validate_prepared(&first, first_prepared).unwrap();
    let first_post_summary = pipeline
        .process_reconciliation_report(&graph, &first_plan, &first_reconciliation, 1)
        .unwrap();
    let _ = merge(first_pre_summary, first_post_summary);

    let learned = pipeline
        .feedback_store()
        .estimate_static_serialization_cost(
            first_edge_index,
            1,
            &AdaptiveFeedbackConfig {
                retention_factor: 1.0,
                confidence_scale: 1.0,
                ..AdaptiveFeedbackConfig::default()
            },
        )
        .unwrap();
    assert!(learned.expected_serialization_cost_nanos >= 1_000_000.0);
    assert!(learned.confidence > 0.0);

    // Second block must consume the learned cost in its candidate edge and produce the current
    // experiment schema.
    // record whose JSON representation is stable and round-trippable.
    let second = block(&contract, 10, 2);
    let (second_plan, planning_metrics) = pipeline
        .plan_block_with_metrics(&engine, &graph, &second)
        .unwrap();
    let second_edge = second_plan
        .candidate_graph
        .edge_between(TxIndex(0), TxIndex(1))
        .unwrap();
    assert!(second_edge.expected_serialization_cost_nanos >= 1_000_000);
    assert!(second_edge.serialization_cost_confidence() > 0.0);

    let second_prepared = executor
        .prepare(&second, &second_plan.speculative_execution_plan)
        .unwrap();
    let preexecution_metrics = second_prepared.metrics.clone();
    let second_pre_report = executor
        .pre_execution_report(&second, &second_prepared)
        .unwrap();
    let feedback_started = Instant::now();
    let second_pre_summary = pipeline
        .process_pre_execution_report(&graph, &second_plan, &second_pre_report, 2)
        .unwrap();
    let pre_feedback_duration = feedback_started.elapsed();
    let second_reconciliation = executor
        .validate_prepared(&second, second_prepared)
        .unwrap();
    let feedback_started = Instant::now();
    let second_post_summary = pipeline
        .process_reconciliation_report(&graph, &second_plan, &second_reconciliation, 2)
        .unwrap();
    let reconciliation_feedback_duration = feedback_started.elapsed();
    let feedback_summary = merge(second_pre_summary, second_post_summary);

    let record = ExperimentRecord::from_runtime(
        ExperimentMetadata {
            experiment_id: "brick5e-runtime".to_owned(),
            workload: "conflictlab".to_owned(),
            mode: "adaptive".to_owned(),
            run_index: 1,
            seed: 0,
            workers: 2,
            physical_cores: 6,
            ..ExperimentMetadata::default()
        },
        planning_metrics,
        pipeline.planning_config(),
        &second_plan,
        &second_pre_report,
        &preexecution_metrics,
        &second_reconciliation,
        ParallelismReference::default(),
        feedback_summary,
        FeedbackTimingRecord::from_durations(
            pre_feedback_duration,
            reconciliation_feedback_duration,
        ),
        CorrectnessRecord {
            serial_equivalent: Some(true),
            ..CorrectnessRecord::default()
        },
    );
    assert_eq!(record.scheduling.serialization_cost_evidence_edges, 1);
    assert!(record.scheduling.expected_serialization_cost_nanos_sum >= 1_000_000);
    assert!(record.feedback.serialization_cost_observations >= 1);
    assert_eq!(record.correctness.serial_equivalent, Some(true));

    let line = record.to_json_line().unwrap();
    assert_eq!(ExperimentRecord::from_json(&line).unwrap(), record);
    println!(
        "\nBRICK5E_RECORD_JSON={}",
        String::from_utf8(line).unwrap().trim_end()
    );
}
