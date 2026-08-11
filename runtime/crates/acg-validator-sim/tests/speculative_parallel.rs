use std::sync::Arc;

use acg_cosmwasm_engine::{
    Address, BlockContext, CanonicalTxDisposition, CosmWasmEngine, ExecutionRequest,
    NativeCallContext, NativeContract, ParallelExecutionConfig, TransactionId,
};
use acg_validator_sim::{
    BlockProducer, BlockProducerConfig, ExecutionDependency, ExecutionDependencyClass,
    ExecutionPlan, ExecutionWave, Mempool, SpeculativeParallelBlockExecutor,
};
use cosmwasm_std::{to_json_binary, Binary, Empty, Env, MessageInfo, Response};
use serde_json::{json, Value};

struct CounterContract;

impl NativeContract for CounterContract {
    fn instantiate(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        _msg: Binary,
    ) -> Result<Response<Empty>, String> {
        context.storage_set(b"count", 0_u64.to_be_bytes());
        Ok(Response::new())
    }

    fn execute(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        msg: Binary,
    ) -> Result<Response<Empty>, String> {
        let message: Value =
            serde_json::from_slice(msg.as_slice()).map_err(|error| error.to_string())?;
        match message.get("action").and_then(Value::as_str) {
            Some("increment") => {
                let count = context
                    .storage_get(b"count")
                    .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
                    .map(u64::from_be_bytes)
                    .unwrap_or_default();
                context.storage_set(b"count", count.saturating_add(1).to_be_bytes());
                Ok(Response::new())
            }
            other => Err(format!("unsupported action: {other:?}")),
        }
    }

    fn query(
        &self,
        _context: &mut NativeCallContext,
        _env: Env,
        _msg: Binary,
    ) -> Result<Binary, String> {
        Ok(Binary::default())
    }
}

fn setup_engine() -> (CosmWasmEngine, Address) {
    let engine = CosmWasmEngine::default();
    let code_id = engine
        .register_native("parallel-counter", Arc::new(CounterContract))
        .unwrap();
    let contract = engine
        .instantiate(
            TransactionId(1),
            BlockContext::default(),
            Address::from("alice"),
            code_id,
            None,
            "counter".to_owned(),
            Vec::new(),
            Binary::default(),
        )
        .unwrap()
        .contract;
    (engine, contract)
}

fn increment_request(id: u64, contract: &Address) -> ExecutionRequest {
    ExecutionRequest::Execute {
        transaction_id: TransactionId(id),
        sender: Address::from("alice"),
        contract: contract.clone(),
        funds: Vec::new(),
        msg: to_json_binary(&json!({"action":"increment"})).unwrap(),
    }
}

fn make_block(contract: &Address, count: usize) -> acg_validator_sim::ProducedBlock {
    let mempool = Mempool::default();
    for index in 0..count {
        mempool.admit(increment_request(10 + index as u64, contract), 0);
    }
    BlockProducer::fifo(BlockProducerConfig::default())
        .unwrap()
        .produce_next(&mempool)
}

#[test]
fn validator_executor_uses_split_phase_path_and_preserves_serial_counter_semantics() {
    let (engine, contract) = setup_engine();
    let block = make_block(&contract, 4);
    let plan = ExecutionPlan {
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

    let report = executor.execute(&block, &plan).unwrap();

    assert_eq!(report.transactions.len(), 4);
    assert_eq!(report.successful(), 4);
    assert_eq!(
        report
            .transactions
            .iter()
            .map(|execution| execution.transaction_id.0)
            .collect::<Vec<_>>(),
        vec![10, 11, 12, 13]
    );
    assert_eq!(
        engine.raw_storage(&contract, b"count"),
        Some(4_u64.to_be_bytes().to_vec())
    );
}

#[test]
fn validator_executor_rejects_plan_block_transaction_count_mismatch() {
    let (engine, contract) = setup_engine();
    let block = make_block(&contract, 1);
    let plan = ExecutionPlan {
        transaction_count: 2,
        waves: vec![ExecutionWave {
            transaction_indices: vec![0, 1],
        }],
        dependencies: Vec::new(),
    };
    let executor =
        SpeculativeParallelBlockExecutor::new(engine, ParallelExecutionConfig { workers: 2 });

    assert!(executor.execute(&block, &plan).is_err());
}

#[test]
fn split_phase_validator_prepares_without_commit_then_reconciles_decided_block() {
    let (engine, contract) = setup_engine();
    let block = make_block(&contract, 4);
    let plan = ExecutionPlan {
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

    let prepared = executor.prepare(&block, &plan).unwrap();
    assert_eq!(
        engine.raw_storage(&contract, b"count"),
        Some(0_u64.to_be_bytes().to_vec())
    );

    let report = executor.validate_prepared(&block, prepared).unwrap();
    assert_eq!(report.block.successful(), 4);
    assert_eq!(report.prediction.precision(), 1.0);
    assert_eq!(report.prediction.coverage(), 1.0);
    assert_eq!(report.speculative.speculative_results, 4);
    assert_eq!(
        report.speculative.reused_results + report.speculative.replayed_transactions,
        4
    );
    assert_eq!(
        report.speculative.invalidated_results,
        report.speculative.replayed_transactions
    );
    assert!(report.speculative.reused_results >= 1);
    assert_eq!(report.reconciliation.len(), 4);
    assert_eq!(
        report.reconciliation[0].disposition,
        CanonicalTxDisposition::ReusedSpeculative
    );
    assert!(report.reconciliation[0]
        .validation
        .as_ref()
        .is_some_and(|validation| validation.is_valid()));
    assert!(report.reconciliation.iter().all(|diagnostic| {
        match diagnostic.disposition {
            CanonicalTxDisposition::ReusedSpeculative => diagnostic
                .validation
                .as_ref()
                .is_some_and(|validation| validation.is_valid()),
            CanonicalTxDisposition::Replayed => diagnostic
                .validation
                .as_ref()
                .is_some_and(|validation| !validation.is_valid()),
            CanonicalTxDisposition::Canonical => false,
        }
    }));
    assert_eq!(
        engine.raw_storage(&contract, b"count"),
        Some(4_u64.to_be_bytes().to_vec())
    );
}

#[test]
fn split_phase_dependency_chain_reuses_counter_receipts_without_serial_wave_barriers() {
    let (engine, contract) = setup_engine();
    let block = make_block(&contract, 4);
    let plan = ExecutionPlan {
        transaction_count: 4,
        waves: (0..4)
            .map(|index| ExecutionWave {
                transaction_indices: vec![index],
            })
            .collect(),
        dependencies: vec![
            ExecutionDependency {
                predecessor_index: 0,
                successor_index: 1,
                class: ExecutionDependencyClass::Hard,
            },
            ExecutionDependency {
                predecessor_index: 1,
                successor_index: 2,
                class: ExecutionDependencyClass::Hard,
            },
            ExecutionDependency {
                predecessor_index: 2,
                successor_index: 3,
                class: ExecutionDependencyClass::Hard,
            },
        ],
    };
    let executor = SpeculativeParallelBlockExecutor::new(
        engine.clone(),
        ParallelExecutionConfig { workers: 4 },
    );

    let prepared = executor.prepare(&block, &plan).unwrap();
    assert_eq!(prepared.metrics.dependency_count, 3);
    assert_eq!(prepared.metrics.hard_dependency_count, 3);
    assert_eq!(
        engine.raw_storage(&contract, b"count"),
        Some(0_u64.to_be_bytes().to_vec())
    );

    let report = executor.validate_prepared(&block, prepared).unwrap();
    assert_eq!(report.speculative.reused_results, 4);
    assert_eq!(report.speculative.replayed_transactions, 0);
    assert!(report.dependency_evidence.is_empty());
    assert_eq!(
        engine.raw_storage(&contract, b"count"),
        Some(4_u64.to_be_bytes().to_vec())
    );
}

#[test]
fn next_block_preexecution_starts_after_predecessor_commit_and_uses_fresh_state() {
    let (engine, contract) = setup_engine();
    let block_n = make_block(&contract, 1);

    let next_mempool = Mempool::default();
    next_mempool.admit(increment_request(20, &contract), 0);
    let block_n_plus_one = BlockProducer::fifo(BlockProducerConfig::default())
        .unwrap()
        .produce_next(&next_mempool);

    let plan = ExecutionPlan {
        transaction_count: 1,
        waves: vec![ExecutionWave {
            transaction_indices: vec![0],
        }],
        dependencies: Vec::new(),
    };
    let executor = SpeculativeParallelBlockExecutor::new(
        engine.clone(),
        ParallelExecutionConfig { workers: 2 },
    );

    // Block N may already have been pre-executed before its decision.
    let prepared_n = executor.prepare(&block_n, &plan).unwrap();
    assert_eq!(
        engine.raw_storage(&contract, b"count"),
        Some(0_u64.to_be_bytes().to_vec())
    );

    // Planning for N+1 can exist at this point, but N+1 execution is intentionally not launched.
    // Commit N first so the canonical predecessor state becomes count=1.
    let report_n = executor.validate_prepared(&block_n, prepared_n).unwrap();
    assert_eq!(report_n.speculative.reused_results, 1);
    assert_eq!(
        engine.raw_storage(&contract, b"count"),
        Some(1_u64.to_be_bytes().to_vec())
    );

    // Only now snapshot/execute N+1. Its point-read dependency observes count=1, so immediate
    // reconciliation reuses the receipt rather than replaying stale work from the pre-N state.
    let prepared_n_plus_one = executor.prepare(&block_n_plus_one, &plan).unwrap();
    let report_n_plus_one = executor
        .validate_prepared(&block_n_plus_one, prepared_n_plus_one)
        .unwrap();
    assert_eq!(report_n_plus_one.speculative.reused_results, 1);
    assert_eq!(report_n_plus_one.speculative.replayed_transactions, 0);
    assert_eq!(
        engine.raw_storage(&contract, b"count"),
        Some(2_u64.to_be_bytes().to_vec())
    );
}
