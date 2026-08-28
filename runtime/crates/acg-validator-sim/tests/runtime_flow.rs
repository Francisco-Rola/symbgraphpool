use std::sync::Arc;

use acg_cosmwasm_engine::{
    Address, BlockContext, CodeId, CosmWasmEngine, ExecutionRequest, NativeCallContext,
    NativeContract, TransactionId,
};
use acg_validator_sim::{
    BlockExecutionError, BlockProducer, BlockProducerConfig, BlockScheduler, DirectDagBlockExecutor,
    ExecutionDependency, ExecutionDependencyClass, ExecutionPlan,
    ExecutionWave, FifoScheduler, IngressConfig, Mempool, ProducedBlock, RateControlledIngress,
    SchedulingError, SerialBlockExecutor, SingleValidatorRuntime, DEFAULT_BENCHMARK_INGRESS_TPS,
};
use cosmwasm_std::{to_json_binary, Binary, Empty, Env, MessageInfo, Reply, Response};
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
        let message: Value = serde_json::from_slice(msg.as_slice()).map_err(|e| e.to_string())?;
        match message.get("action").and_then(Value::as_str) {
            Some("increment") => {
                let current = read_u64(context.storage_get(b"count"));
                context.storage_set(b"count", current.saturating_add(1).to_be_bytes());
                Ok(Response::new())
            }
            Some("set") => {
                let value = message
                    .get("value")
                    .and_then(Value::as_u64)
                    .ok_or_else(|| "set requires a u64 value".to_owned())?;
                context.storage_set(b"count", value.to_be_bytes());
                Ok(Response::new())
            }
            Some("fail") => Err("intentional failure".to_owned()),
            other => Err(format!("unsupported action: {other:?}")),
        }
    }

    fn query(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        _msg: Binary,
    ) -> Result<Binary, String> {
        to_json_binary(&json!({ "count": read_u64(context.storage_get(b"count")) }))
            .map_err(|e| e.to_string())
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

fn read_u64(value: Option<Vec<u8>>) -> u64 {
    value
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
        .map(u64::from_be_bytes)
        .unwrap_or_default()
}

fn execute_request(id: u64, contract: &Address, action: &str) -> ExecutionRequest {
    ExecutionRequest::Execute {
        transaction_id: TransactionId(id),
        sender: Address::from("alice"),
        contract: contract.clone(),
        funds: Vec::new(),
        msg: to_json_binary(&json!({ "action": action })).unwrap(),
    }
}

fn placeholder_request(id: u64) -> ExecutionRequest {
    ExecutionRequest::Execute {
        transaction_id: TransactionId(id),
        sender: Address::from("sender"),
        contract: Address::from("contract"),
        funds: Vec::new(),
        msg: Binary::default(),
    }
}

fn set_request(id: u64, contract: &Address, value: u64) -> ExecutionRequest {
    ExecutionRequest::Execute {
        transaction_id: TransactionId(id),
        sender: Address::from("alice"),
        contract: contract.clone(),
        funds: Vec::new(),
        msg: to_json_binary(&json!({ "action": "set", "value": value })).unwrap(),
    }
}

#[derive(Clone, Copy)]
struct ReverseScheduler;

impl BlockScheduler for ReverseScheduler {
    fn schedule(&self, block: &ProducedBlock) -> Result<ExecutionPlan, SchedulingError> {
        Ok(ExecutionPlan {
            transaction_count: block.transactions.len(),
            waves: (0..block.transactions.len())
                .rev()
                .map(|index| ExecutionWave {
                    transaction_indices: vec![index],
                })
                .collect(),
            dependencies: Vec::new(),
        })
    }
}

fn instantiate_counter(engine: &CosmWasmEngine) -> (CodeId, Address) {
    let code_id = engine
        .register_native("validator-counter", Arc::new(CounterContract))
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
    (code_id, contract)
}

#[test]
fn mempool_accepts_everything_and_preserves_fifo_order() {
    let mempool = Mempool::default();
    let first = mempool.admit(placeholder_request(7), 100);
    let second = mempool.admit(placeholder_request(7), 101);

    assert_eq!(first.admission_sequence, 0);
    assert_eq!(second.admission_sequence, 1);
    assert_eq!(mempool.len(), 2);
    let snapshot = mempool.snapshot();
    assert_eq!(snapshot[0].transaction_id(), TransactionId(7));
    assert_eq!(snapshot[1].transaction_id(), TransactionId(7));
}

#[test]
fn rate_controlled_ingress_uses_virtual_time() {
    let mempool = Mempool::default();
    let mut ingress = RateControlledIngress::new(IngressConfig::default(), 1_000).unwrap();
    assert_eq!(IngressConfig::default().transactions_per_second, 25_000);
    assert_eq!(DEFAULT_BENCHMARK_INGRESS_TPS, 25_000);
    ingress.enqueue_all([
        placeholder_request(1),
        placeholder_request(2),
        placeholder_request(3),
    ]);

    assert_eq!(ingress.pump_until(40_999, &mempool), 0);
    assert_eq!(ingress.pump_until(41_000, &mempool), 1);
    assert_eq!(ingress.pump_until(80_999, &mempool), 0);
    assert_eq!(ingress.pump_until(81_000, &mempool), 1);
    assert_eq!(ingress.pump_until(121_000, &mempool), 1);
    assert_eq!(mempool.len(), 3);
}

#[test]
fn block_producer_defaults_to_two_seconds_and_fifo() {
    let mempool = Mempool::default();
    mempool.admit(placeholder_request(1), 0);
    mempool.admit(placeholder_request(2), 0);
    mempool.admit(placeholder_request(3), 0);

    let config = BlockProducerConfig {
        max_transactions_per_block: Some(2),
        ..BlockProducerConfig::default()
    };
    let mut producer = BlockProducer::fifo(config).unwrap();

    let first = producer.produce_next(&mempool);
    assert_eq!(first.context.height, 1);
    assert_eq!(first.context.time_nanos, 2_000_000_000);
    assert_eq!(first.transactions.len(), 2);
    assert_eq!(first.transactions[0].transaction_id(), TransactionId(1));
    assert_eq!(first.transactions[1].transaction_id(), TransactionId(2));

    let second = producer.produce_next(&mempool);
    assert_eq!(second.context.height, 2);
    assert_eq!(second.context.time_nanos, 4_000_000_000);
    assert_eq!(second.transactions[0].transaction_id(), TransactionId(3));
}

#[test]
fn fifo_scheduler_creates_one_serial_wave_per_transaction() {
    let mempool = Mempool::default();
    mempool.admit(placeholder_request(1), 0);
    mempool.admit(placeholder_request(2), 0);
    let block = BlockProducer::fifo(BlockProducerConfig::default())
        .unwrap()
        .produce_next(&mempool);
    let plan = FifoScheduler.schedule(&block).unwrap();

    assert_eq!(plan.transaction_count, 2);
    assert_eq!(plan.waves[0].transaction_indices, vec![0]);
    assert_eq!(plan.waves[1].transaction_indices, vec![1]);
    plan.validate().unwrap();
}

#[test]
fn serial_executor_commits_in_block_order_and_keeps_failed_transactions() {
    let engine = CosmWasmEngine::default();
    let (_, contract) = instantiate_counter(&engine);
    let mempool = Mempool::default();
    mempool.admit(execute_request(2, &contract, "increment"), 0);
    mempool.admit(execute_request(3, &contract, "fail"), 0);
    mempool.admit(execute_request(4, &contract, "increment"), 0);
    let block = BlockProducer::fifo(BlockProducerConfig::default())
        .unwrap()
        .produce_next(&mempool);
    let plan = FifoScheduler.schedule(&block).unwrap();
    let report = SerialBlockExecutor::new(engine.clone())
        .execute(&block, &plan)
        .unwrap();

    assert_eq!(report.successful(), 2);
    assert_eq!(report.failed(), 1);
    assert_eq!(report.transactions[1].transaction_id, TransactionId(3));
    let query = engine
        .query(BlockContext::default(), contract, Binary::default())
        .unwrap();
    let value: Value = serde_json::from_slice(query.data.as_slice()).unwrap();
    assert_eq!(value["count"], 2);
}

#[test]
fn scheduler_is_a_pluggable_execution_order_boundary() {
    let engine = CosmWasmEngine::default();
    let (_, contract) = instantiate_counter(&engine);
    let mut runtime = SingleValidatorRuntime::new(
        engine.clone(),
        BlockProducerConfig::default(),
        ReverseScheduler,
    )
    .unwrap();
    runtime.submit(set_request(2, &contract, 10), 1_000);
    runtime.submit(set_request(3, &contract, 20), 2_000);

    let report = runtime.produce_and_execute().unwrap();
    assert_eq!(report.transactions[0].transaction_id, TransactionId(3));
    assert_eq!(report.transactions[1].transaction_id, TransactionId(2));

    let query = engine
        .query(BlockContext::default(), contract, Binary::default())
        .unwrap();
    let value: Value = serde_json::from_slice(query.data.as_slice()).unwrap();
    assert_eq!(value["count"], 10);
}


#[test]
fn direct_dag_executor_runs_independent_wave_against_canonical_state() {
    let engine = CosmWasmEngine::default();
    let (code_id, first) = instantiate_counter(&engine);
    let second = engine
        .instantiate(
            TransactionId(10),
            BlockContext::default(),
            Address::from("alice"),
            code_id,
            None,
            "counter-2".to_owned(),
            Vec::new(),
            Binary::default(),
        )
        .unwrap()
        .contract;
    let mempool = Mempool::default();
    mempool.admit(execute_request(11, &first, "increment"), 0);
    mempool.admit(execute_request(12, &second, "increment"), 0);
    let block = BlockProducer::fifo(BlockProducerConfig::default())
        .unwrap()
        .produce_next(&mempool);
    let plan = ExecutionPlan {
        transaction_count: 2,
        waves: vec![ExecutionWave {
            transaction_indices: vec![0, 1],
        }],
        dependencies: Vec::new(),
    };

    let report = DirectDagBlockExecutor::new(engine.clone(), 2)
        .unwrap()
        .execute(&block, &plan)
        .unwrap();
    assert_eq!(report.successful(), 2);
    assert_eq!(report.transactions[0].transaction_id, TransactionId(11));
    assert_eq!(report.transactions[1].transaction_id, TransactionId(12));

    for contract in [first, second] {
        let query = engine
            .query(BlockContext::default(), contract, Binary::default())
            .unwrap();
        let value: Value = serde_json::from_slice(query.data.as_slice()).unwrap();
        assert_eq!(value["count"], 1);
    }
}

#[test]
fn direct_dag_runtime_diagnostics_capture_worker_and_commit_hot_paths() {
    let engine = CosmWasmEngine::default();
    let (code_id, first) = instantiate_counter(&engine);
    let second = engine
        .instantiate(
            TransactionId(40),
            BlockContext::default(),
            Address::from("alice"),
            code_id,
            None,
            "counter-profile-2".to_owned(),
            Vec::new(),
            Binary::default(),
        )
        .unwrap()
        .contract;
    let mempool = Mempool::default();
    mempool.admit(execute_request(41, &first, "increment"), 0);
    mempool.admit(execute_request(42, &second, "increment"), 0);
    let block = BlockProducer::fifo(BlockProducerConfig::default())
        .unwrap()
        .produce_next(&mempool);
    let plan = ExecutionPlan {
        transaction_count: 2,
        waves: vec![ExecutionWave {
            transaction_indices: vec![0, 1],
        }],
        dependencies: Vec::new(),
    };

    let (report, diagnostics) = DirectDagBlockExecutor::new(engine, 2)
        .unwrap()
        .execute_with_diagnostics(&block, &plan)
        .unwrap();
    assert_eq!(report.successful(), 2);
    assert!(diagnostics.worker_phase_wall > std::time::Duration::ZERO);
    assert!(diagnostics.aggregate_transaction_service > std::time::Duration::ZERO);
    assert!(diagnostics.max_in_flight >= 1);
    assert!(diagnostics.commit.batches >= 1);
    assert_eq!(diagnostics.commit.write_sets, 2);
    assert!(diagnostics.contract.aggregate_request_execution > std::time::Duration::ZERO);
    assert!(diagnostics.contract.canonical_state_reads > 0);
}

#[test]
fn direct_dag_executor_reuses_pool_across_blocks_and_commits_dependencies() {
    let engine = CosmWasmEngine::default();
    let (_code_id, contract) = instantiate_counter(&engine);
    let executor = DirectDagBlockExecutor::new(engine.clone(), 2).unwrap();

    for round in 0..2_u64 {
        let mempool = Mempool::default();
        mempool.admit(execute_request(20 + round * 2, &contract, "increment"), 0);
        mempool.admit(execute_request(21 + round * 2, &contract, "increment"), 0);
        let block = BlockProducer::fifo(BlockProducerConfig::default())
            .unwrap()
            .produce_next(&mempool);
        let plan = ExecutionPlan {
            transaction_count: 2,
            waves: vec![
                ExecutionWave { transaction_indices: vec![0] },
                ExecutionWave { transaction_indices: vec![1] },
            ],
            dependencies: vec![ExecutionDependency {
                predecessor_index: 0,
                successor_index: 1,
                class: ExecutionDependencyClass::Hard,
            }],
        };
        let report = executor.execute(&block, &plan).unwrap();
        assert_eq!(report.successful(), 2);
    }

    let query = engine
        .query(BlockContext::default(), contract, Binary::default())
        .unwrap();
    let value: Value = serde_json::from_slice(query.data.as_slice()).unwrap();
    assert_eq!(value["count"], 4);
}

#[test]
fn direct_dag_executor_rejects_zero_workers() {
    assert!(matches!(
        DirectDagBlockExecutor::new(CosmWasmEngine::default(), 0),
        Err(BlockExecutionError::InvalidWorkerCount)
    ));
}

#[test]
fn serial_executor_rejects_parallel_waves_until_validation_exists() {
    let engine = CosmWasmEngine::default();
    let mempool = Mempool::default();
    mempool.admit(placeholder_request(1), 0);
    mempool.admit(placeholder_request(2), 0);
    let block = BlockProducer::fifo(BlockProducerConfig::default())
        .unwrap()
        .produce_next(&mempool);
    let plan = ExecutionPlan {
        transaction_count: 2,
        waves: vec![ExecutionWave {
            transaction_indices: vec![0, 1],
        }],
        dependencies: Vec::new(),
    };

    let error = SerialBlockExecutor::new(engine)
        .execute(&block, &plan)
        .unwrap_err();
    assert!(matches!(
        error,
        BlockExecutionError::ParallelWaveUnsupported {
            wave_index: 0,
            width: 2
        }
    ));
}

#[test]
fn default_pipeline_produces_and_executes_a_block() {
    let engine = CosmWasmEngine::default();
    let (_, contract) = instantiate_counter(&engine);
    let mut runtime =
        SingleValidatorRuntime::fifo(engine.clone(), BlockProducerConfig::default()).unwrap();
    runtime.submit(execute_request(2, &contract, "increment"), 1_000);
    runtime.submit(execute_request(3, &contract, "increment"), 2_000);

    let report = runtime.produce_and_execute().unwrap();
    assert_eq!(report.block_height, 1);
    assert_eq!(report.successful(), 2);
    assert!(runtime.mempool().is_empty());
}

#[test]
fn ingress_can_fill_the_mempool_up_to_the_next_block_boundary() {
    let engine = CosmWasmEngine::default();
    let (_, contract) = instantiate_counter(&engine);
    let mut runtime = SingleValidatorRuntime::fifo(engine, BlockProducerConfig::default()).unwrap();
    let mut ingress = RateControlledIngress::new(
        IngressConfig {
            transactions_per_second: 2,
        },
        0,
    )
    .unwrap();
    ingress.enqueue_all([
        execute_request(2, &contract, "increment"),
        execute_request(3, &contract, "increment"),
        execute_request(4, &contract, "increment"),
        execute_request(5, &contract, "increment"),
        execute_request(6, &contract, "increment"),
    ]);

    let boundary = runtime.next_block_time_nanos();
    assert_eq!(boundary, 2_000_000_000);
    assert_eq!(ingress.pump_until(boundary, runtime.mempool()), 4);
    let report = runtime.produce_and_execute().unwrap();
    assert_eq!(report.successful(), 4);
    assert_eq!(ingress.queued(), 1);
}

#[test]
fn fifo_block_preview_is_non_destructive_and_matches_next_produced_batch() {
    let mempool = Mempool::default();
    for id in 1_u64..=5 {
        mempool.admit(placeholder_request(id), id * 10);
    }
    let config = BlockProducerConfig {
        block_interval: std::time::Duration::from_millis(700),
        first_block_time_nanos: 700_000_000,
        max_transactions_per_block: Some(3),
        ..BlockProducerConfig::default()
    };
    let mut producer = BlockProducer::fifo(config).unwrap();

    let preview = producer.preview_next(&mempool);
    assert_eq!(mempool.len(), 5);
    assert_eq!(preview.context.height, 1);
    assert_eq!(preview.context.time_nanos, 700_000_000);
    assert_eq!(preview.transactions.len(), 3);

    let produced = producer.produce_next(&mempool);
    assert_eq!(preview, produced);
    assert_eq!(mempool.len(), 2);
    assert_eq!(producer.next_block_time_nanos(), 1_400_000_000);
}

#[test]
fn diagnostic_block_selection_policies_are_deterministic() {
    let config = BlockProducerConfig {
        max_transactions_per_block: Some(5),
        ..BlockProducerConfig::default()
    };

    let reverse_pool = Mempool::default();
    for id in 1_u64..=5 {
        reverse_pool.admit(placeholder_request(id), id);
    }
    let reverse = BlockProducer::reverse_fifo(config.clone())
        .unwrap()
        .produce_next(&reverse_pool)
        .transactions
        .into_iter()
        .map(|tx| tx.transaction_id().0)
        .collect::<Vec<_>>();
    assert_eq!(reverse, vec![5, 4, 3, 2, 1]);

    let shuffled = |seed| {
        let mempool = Mempool::default();
        for id in 1_u64..=5 {
            mempool.admit(placeholder_request(id), id);
        }
        BlockProducer::seeded_shuffle(config.clone(), seed)
            .unwrap()
            .produce_next(&mempool)
            .transactions
            .into_iter()
            .map(|tx| tx.transaction_id().0)
            .collect::<Vec<_>>()
    };
    let first = shuffled(42);
    assert_eq!(first, shuffled(42));
    assert_ne!(first, vec![1, 2, 3, 4, 5]);
}
