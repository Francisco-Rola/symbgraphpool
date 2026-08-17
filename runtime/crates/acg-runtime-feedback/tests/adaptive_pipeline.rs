use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Barrier,
    },
    time::Duration,
};

use acg_candidate_graph::{EdgeClass, RiskBoundedSchedulerConfig};
use acg_core::{ContractCodeHash, RuntimeId, TxIndex};
use acg_cosmwasm_adapter::{CosmWasmAdapterConfig, CosmWasmCandidateAdapter};
use acg_cosmwasm_engine::{
    Address, BlockContext, CanonicalTxDisposition, CosmWasmEngine, ExecutionRequest,
    NativeCallContext, NativeContract, ParallelExecutionConfig, TransactionId, ValidationConflict,
};
use acg_feedback::AdaptiveFeedbackConfig;
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_runtime_feedback::{
    AdaptivePlanningConfig, AdaptiveSerialPipeline, RuntimeFeedbackEngine, RuntimeFeedbackWeights,
    TraceConflictConfig,
};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use acg_validator_sim::{
    BlockProducer, BlockProducerConfig, ExecutionPlan, ExecutionWave, Mempool,
    SpeculativeParallelBlockExecutor,
};
use cosmwasm_std::{to_json_binary, Binary, Empty, Env, MessageInfo, Reply, Response};
use serde_json::{json, Value};

const CONFLICTLAB: &[u8] =
    include_bytes!("../../../../benchmarks/symbolic/conflictlab.symbolic.json");

struct ConflictLabRuntime {
    first_execute_barrier: Option<FirstExecuteBarrier>,
}

struct FirstExecuteBarrier {
    barrier: Barrier,
    participants: usize,
    calls: AtomicUsize,
}

impl ConflictLabRuntime {
    fn normal() -> Self {
        Self {
            first_execute_barrier: None,
        }
    }

    fn barrier_first_execute_calls(participants: usize) -> Self {
        assert!(participants > 0);
        Self {
            first_execute_barrier: Some(FirstExecuteBarrier {
                barrier: Barrier::new(participants),
                participants,
                calls: AtomicUsize::new(0),
            }),
        }
    }
}

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
        if let Some(sync) = &self.first_execute_barrier {
            let call = sync.calls.fetch_add(1, Ordering::SeqCst);
            if call < sync.participants {
                sync.barrier.wait();
            }
        }
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
    setup_with_runtime(ConflictLabRuntime::normal())
}

fn setup_with_runtime(
    runtime: ConflictLabRuntime,
) -> (
    CosmWasmEngine,
    Address,
    ProfileGraph,
    AdaptiveSerialPipeline,
) {
    let engine = CosmWasmEngine::default();
    let code_id = engine
        .register_native("conflictlab-phase4d", Arc::new(runtime))
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
            compact_equivalence_groups: true,
            scheduler: RiskBoundedSchedulerConfig {
                soft_threshold: 0.30,
                hard_threshold: 0.70,
                risk_budget: 0.30,
                max_wave_width: None,
                exploration_rate: 0.0,
                exploration_risk_budget: 0.90,
                exploration_min_uncertainty: 0.35,
                exploration_max_transactions_per_block: 0,
                independent_observations_before_softening: 8,
                softening_min_confidence: 0.25,
            },
            cost_policy: Default::default(),
            serial_bypass: Default::default(),
            regime_change: Default::default(),
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

    // Phase 4D never hands the wide speculative plan to the serial executor.
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

#[test]
fn phase5d_reconciliation_attribution_measures_replay_cost_and_transitive_fanout() {
    let (engine, contract, graph, mut pipeline) =
        setup_with_runtime(ConflictLabRuntime::barrier_first_execute_calls(4));
    let mempool = Mempool::default();
    for id in 1..=4_u64 {
        mempool.admit(credit(id, &contract, "alice"), id);
    }
    let block = BlockProducer::fifo(BlockProducerConfig::default())
        .unwrap()
        .produce_next(&mempool);
    let plan = pipeline.plan_block(&engine, &graph, &block).unwrap();

    // Intentionally ignore the conservative adaptive dependencies so reconciliation has real stale
    // receipts to attribute. This is an evaluation fixture, not a production scheduling path.
    let wide_plan = ExecutionPlan {
        transaction_count: 4,
        waves: vec![ExecutionWave {
            transaction_indices: vec![0, 1, 2, 3],
        }],
        dependencies: Vec::new(),
    };
    let executor = SpeculativeParallelBlockExecutor::new(
        engine.clone(),
        ParallelExecutionConfig { workers: 4 },
    );
    let prepared = executor.prepare(&block, &wide_plan).unwrap();
    let report = executor.validate_prepared(&block, prepared).unwrap();
    assert!(report.speculative.replayed_transactions > 0);
    assert!(!report.dependency_evidence.is_empty());

    let attributions = pipeline
        .reconciliation_attributions(&plan, &report)
        .unwrap();
    assert_eq!(attributions.len(), report.dependency_evidence.len());
    assert!(attributions.iter().all(|item| item.candidate_edge_present));
    assert!(attributions.iter().all(|item| item
        .conflict_kinds
        .contains(acg_core::ConflictKinds::WRITE_READ)));
    assert!(attributions
        .iter()
        .all(|item| matches!(&item.conflict, ValidationConflict::Storage { key, .. } if key.as_slice() == b"balance/alice")));
    assert!(attributions
        .iter()
        .any(|item| item.invalidated_descendants > 0));

    // If one replay has multiple concrete conflicts, 5D splits the direct replay duration across
    // them. The shares must exactly conserve the measured direct replay time per transaction.
    let mut attributed_nanos = BTreeMap::<usize, u64>::new();
    for item in &attributions {
        *attributed_nanos
            .entry(item.transaction.0 as usize)
            .or_default() += item.replay_cost_nanos;
    }
    let mut saw_measured_replay = false;
    for diagnostic in &report.reconciliation {
        if diagnostic.disposition != CanonicalTxDisposition::Replayed {
            assert_eq!(diagnostic.reexecution_duration, Duration::ZERO);
            continue;
        }
        saw_measured_replay |= !diagnostic.reexecution_duration.is_zero();
        if let Some(attributed) = attributed_nanos.get(&diagnostic.transaction_index) {
            assert_eq!(
                u128::from(*attributed),
                diagnostic.reexecution_duration.as_nanos()
            );
        }
    }
    assert!(saw_measured_replay);

    let edge_index = plan
        .candidate_graph
        .edge_between(TxIndex(0), TxIndex(1))
        .unwrap()
        .profile_edge_index()
        .unwrap();
    let summary = pipeline
        .process_reconciliation_report(&graph, &plan, &report, block.context.height)
        .unwrap();
    assert_eq!(summary.replay_impact_observations, attributions.len());
    assert!(summary.attributed_replay_cost_nanos > 0);
    assert!(summary.attributed_invalidated_descendants > 0);
    let replay_cost = pipeline
        .feedback_store()
        .estimate_static_replay_cost(
            edge_index,
            block.context.height,
            &AdaptiveFeedbackConfig {
                retention_factor: 1.0,
                ..AdaptiveFeedbackConfig::default()
            },
        )
        .unwrap();
    assert!(replay_cost.replay_observations > 0);
    assert!(replay_cost.total_replay_cost_nanos > 0);
    assert!(replay_cost.total_invalidated_descendants > 0);
}
