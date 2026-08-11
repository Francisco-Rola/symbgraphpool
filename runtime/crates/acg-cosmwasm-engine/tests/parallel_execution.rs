use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;

use acg_cosmwasm_engine::{
    Address, BlockContext, CanonicalTransaction, CanonicalTxDisposition, CodeId, CosmWasmEngine,
    ExecutionRequest, NativeCallContext, NativeContract, ParallelExecutionConfig,
    ParallelSpeculativeBlockOutcome, SpeculativeDependency, SpeculativeDependencyClass,
    SpeculativeWave, TransactionId,
};
use cosmwasm_std::{to_json_binary, Binary, Coin, Empty, Env, MessageInfo, Response};
use serde_json::{json, Value};

const HACKATOM_BASE64: &str = include_str!("../testdata/hackatom_1.2.wasm.b64");

#[derive(Default)]
struct ConcurrencyProbe {
    active: AtomicUsize,
    max_active: AtomicUsize,
    completions: Mutex<Vec<u64>>,
    lifecycle: Mutex<Vec<String>>,
}

impl ConcurrencyProbe {
    fn enter(&self, wave: u64, transaction_id: TransactionId) {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        let mut observed = self.max_active.load(Ordering::SeqCst);
        while active > observed {
            match self.max_active.compare_exchange_weak(
                observed,
                active,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(actual) => observed = actual,
            }
        }
        self.lifecycle
            .lock()
            .unwrap()
            .push(format!("start:{wave}:{}", transaction_id.0));
    }

    fn exit(&self, wave: u64, transaction_id: TransactionId) {
        self.lifecycle
            .lock()
            .unwrap()
            .push(format!("end:{wave}:{}", transaction_id.0));
        self.completions.lock().unwrap().push(transaction_id.0);
        self.active.fetch_sub(1, Ordering::SeqCst);
    }

    fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
    }

    fn completions(&self) -> Vec<u64> {
        self.completions.lock().unwrap().clone()
    }

    fn lifecycle(&self) -> Vec<String> {
        self.lifecycle.lock().unwrap().clone()
    }
}

struct ParallelContract {
    probe: Arc<ConcurrencyProbe>,
}

impl NativeContract for ParallelContract {
    fn instantiate(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        _msg: Binary,
    ) -> Result<Response<Empty>, String> {
        context.storage_set(b"source", b"zero");
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
            Some("noop") => Ok(Response::new()),
            Some("blind_set") => {
                let key = message["key"].as_str().ok_or("missing key")?;
                let value = message["value"].as_str().ok_or("missing value")?;
                context.storage_set(key.as_bytes(), value.as_bytes());
                Ok(Response::new())
            }
            Some("read_then_write") => {
                let source = message["source"].as_str().ok_or("missing source")?;
                let target = message["target"].as_str().ok_or("missing target")?;
                let value = context.storage_get(source.as_bytes()).unwrap_or_default();
                context.storage_set(target.as_bytes(), value);
                Ok(Response::new())
            }
            Some("read_then_write_probe") => {
                let source = message["source"].as_str().ok_or("missing source")?;
                let target = message["target"].as_str().ok_or("missing target")?;
                let wave = message.get("wave").and_then(Value::as_u64).unwrap_or(0);
                self.probe.enter(wave, context.transaction_id);
                let value = context.storage_get(source.as_bytes()).unwrap_or_default();
                context.storage_set(target.as_bytes(), value);
                self.probe.exit(wave, context.transaction_id);
                Ok(Response::new())
            }
            Some("sleep_read_then_write") => {
                let source = message["source"].as_str().ok_or("missing source")?;
                let target = message["target"].as_str().ok_or("missing target")?;
                let delay_ms = message["delay_ms"].as_u64().ok_or("missing delay_ms")?;
                let wave = message.get("wave").and_then(Value::as_u64).unwrap_or(0);
                self.probe.enter(wave, context.transaction_id);
                std::thread::sleep(Duration::from_millis(delay_ms));
                let value = context.storage_get(source.as_bytes()).unwrap_or_default();
                context.storage_set(target.as_bytes(), value);
                self.probe.exit(wave, context.transaction_id);
                Ok(Response::new())
            }
            Some("sleep_set") => {
                let key = message["key"].as_str().ok_or("missing key")?;
                let value = message["value"].as_str().ok_or("missing value")?;
                let delay_ms = message["delay_ms"].as_u64().ok_or("missing delay_ms")?;
                let wave = message.get("wave").and_then(Value::as_u64).unwrap_or(0);
                self.probe.enter(wave, context.transaction_id);
                std::thread::sleep(Duration::from_millis(delay_ms));
                context.storage_set(key.as_bytes(), value.as_bytes());
                self.probe.exit(wave, context.transaction_id);
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

fn block(index: u32) -> BlockContext {
    BlockContext {
        transaction_index: Some(index),
        ..BlockContext::default()
    }
}

fn setup() -> (CosmWasmEngine, Address, Arc<ConcurrencyProbe>) {
    let engine = CosmWasmEngine::default();
    let probe = Arc::new(ConcurrencyProbe::default());
    let code_id = register_fixture(&engine, probe.clone());
    let contract = instantiate_fixture(&engine, code_id);
    (engine, contract, probe)
}

fn register_fixture(engine: &CosmWasmEngine, probe: Arc<ConcurrencyProbe>) -> CodeId {
    engine
        .register_native("parallel-fixture", Arc::new(ParallelContract { probe }))
        .unwrap()
}

fn instantiate_fixture(engine: &CosmWasmEngine, code_id: CodeId) -> Address {
    engine
        .instantiate(
            TransactionId(1),
            block(0),
            Address::from("alice"),
            code_id,
            None,
            "parallel-fixture".to_owned(),
            Vec::new(),
            Binary::default(),
        )
        .unwrap()
        .contract
}

fn request(tx: u64, contract: &Address, message: Value) -> ExecutionRequest {
    ExecutionRequest::Execute {
        transaction_id: TransactionId(tx),
        sender: Address::from("alice"),
        contract: contract.clone(),
        funds: Vec::new(),
        msg: to_json_binary(&message).unwrap(),
    }
}

fn canonical(index: u32, request: ExecutionRequest) -> CanonicalTransaction {
    CanonicalTransaction::new(block(index), request)
}

fn tx_ids(values: &[u64]) -> SpeculativeWave {
    SpeculativeWave::new(values.iter().copied().map(TransactionId).collect())
}

fn hard_dependency(predecessor: u64, successor: u64) -> SpeculativeDependency {
    SpeculativeDependency {
        predecessor: TransactionId(predecessor),
        successor: TransactionId(successor),
        class: SpeculativeDependencyClass::Hard,
    }
}

fn execute_parallel(
    engine: &CosmWasmEngine,
    workers: usize,
    transactions: Vec<CanonicalTransaction>,
    waves: Vec<SpeculativeWave>,
) -> ParallelSpeculativeBlockOutcome {
    engine
        .execute_parallel_waves(ParallelExecutionConfig { workers }, transactions, waves)
        .unwrap()
}

fn serial_execute(engine: &CosmWasmEngine, transactions: &[CanonicalTransaction]) {
    for transaction in transactions {
        engine
            .execute_request(transaction.block.clone(), transaction.request.clone())
            .unwrap();
    }
}

fn mixed_transactions(contract: &Address) -> Vec<CanonicalTransaction> {
    vec![
        canonical(
            0,
            request(
                10,
                contract,
                json!({"action":"blind_set","key":"a","value":"one"}),
            ),
        ),
        canonical(
            1,
            request(
                11,
                contract,
                json!({"action":"read_then_write","source":"a","target":"seen_a"}),
            ),
        ),
        canonical(
            2,
            request(
                12,
                contract,
                json!({"action":"blind_set","key":"b","value":"two"}),
            ),
        ),
        canonical(
            3,
            request(
                13,
                contract,
                json!({"action":"read_then_write","source":"b","target":"seen_b"}),
            ),
        ),
    ]
}

#[test]
fn workers_one_two_and_four_preserve_serial_world_state_and_canonical_results() {
    let (serial, serial_contract, _) = setup();
    let serial_transactions = mixed_transactions(&serial_contract);
    serial_execute(&serial, &serial_transactions);
    let serial_snapshot = serial.snapshot();

    for workers in [1_usize, 2, 4] {
        let (parallel, parallel_contract, _) = setup();
        let transactions = mixed_transactions(&parallel_contract);
        let outcome = execute_parallel(
            &parallel,
            workers,
            transactions,
            vec![tx_ids(&[10, 12]), tx_ids(&[11, 13])],
        );

        assert!(serial_snapshot.same_world_state(&parallel.snapshot()));
        assert_eq!(
            outcome
                .transactions
                .iter()
                .map(|result| result.transaction_id.0)
                .collect::<Vec<_>>(),
            vec![10, 11, 12, 13]
        );
        assert!(outcome
            .transactions
            .iter()
            .all(|result| result.result.is_ok()));
        assert_eq!(outcome.metrics.workers, workers);
        assert_eq!(outcome.metrics.speculative.speculative_results, 4);
        assert_eq!(outcome.metrics.speculative.replayed_transactions, 1);
    }
}

#[test]
fn a_wave_larger_than_the_worker_pool_is_fully_executed_and_reused() {
    let (engine, contract, _) = setup();
    let transactions = (0_u32..12)
        .map(|index| {
            canonical(
                index,
                request(10 + u64::from(index), &contract, json!({"action":"noop"})),
            )
        })
        .collect::<Vec<_>>();
    let ids = (10_u64..22).collect::<Vec<_>>();

    let outcome = execute_parallel(&engine, 2, transactions, vec![tx_ids(&ids)]);

    assert_eq!(outcome.transactions.len(), 12);
    assert_eq!(outcome.metrics.wave_widths, vec![12]);
    assert_eq!(outcome.metrics.max_wave_width(), 12);
    assert_eq!(outcome.metrics.speculative.reused_results, 12);
    assert_eq!(outcome.metrics.speculative.replayed_transactions, 0);
    assert!((outcome.metrics.equal_cost_theoretical_speedup() - 2.0).abs() < f64::EPSILON);
}

#[test]
fn bounded_pool_uses_multiple_workers_without_exceeding_the_configured_limit() {
    let (engine, contract, probe) = setup();
    let transactions = (0_u32..8)
        .map(|index| {
            canonical(
                index,
                request(
                    10 + u64::from(index),
                    &contract,
                    json!({
                        "action":"sleep_set",
                        "key":format!("k{index}"),
                        "value":format!("v{index}"),
                        "delay_ms":40,
                        "wave":0
                    }),
                ),
            )
        })
        .collect::<Vec<_>>();
    let ids = (10_u64..18).collect::<Vec<_>>();

    let outcome = execute_parallel(&engine, 2, transactions, vec![tx_ids(&ids)]);

    assert_eq!(outcome.metrics.speculative.reused_results, 8);
    assert!(
        probe.max_active() >= 2,
        "the wave never achieved concurrent execution"
    );
    assert!(probe.max_active() <= 2, "worker bound was exceeded");
}

#[test]
fn uneven_transaction_costs_do_not_change_canonical_commit_results() {
    let (engine, contract, probe) = setup();
    let delays = [90_u64, 5, 70, 10, 50, 0];
    let transactions = delays
        .iter()
        .enumerate()
        .map(|(index, delay_ms)| {
            canonical(
                index as u32,
                request(
                    10 + index as u64,
                    &contract,
                    json!({
                        "action":"sleep_set",
                        "key":"winner",
                        "value":index.to_string(),
                        "delay_ms":delay_ms,
                        "wave":0
                    }),
                ),
            )
        })
        .collect::<Vec<_>>();
    let ids = (10_u64..16).collect::<Vec<_>>();

    let outcome = execute_parallel(&engine, 3, transactions, vec![tx_ids(&ids)]);

    assert_eq!(
        engine.raw_storage(&contract, b"winner"),
        Some(b"5".to_vec())
    );
    assert_eq!(outcome.metrics.speculative.reused_results, 6);
    assert_eq!(outcome.metrics.speculative.replayed_transactions, 0);
    assert!(probe.max_active() <= 3);
    assert!(probe.max_active() >= 2);
}

#[test]
fn deliberately_reversed_worker_completion_order_cannot_change_canonical_commit_order() {
    let (engine, contract, probe) = setup();
    let delays = [240_u64, 160, 80, 0];
    let transactions = delays
        .iter()
        .enumerate()
        .map(|(index, delay_ms)| {
            canonical(
                index as u32,
                request(
                    10 + index as u64,
                    &contract,
                    json!({
                        "action":"sleep_set",
                        "key":"ordered",
                        "value":index.to_string(),
                        "delay_ms":delay_ms,
                        "wave":0
                    }),
                ),
            )
        })
        .collect::<Vec<_>>();

    let outcome = execute_parallel(&engine, 4, transactions, vec![tx_ids(&[10, 11, 12, 13])]);

    assert_eq!(probe.completions(), vec![13, 12, 11, 10]);
    assert_eq!(
        outcome
            .transactions
            .iter()
            .map(|result| result.transaction_id.0)
            .collect::<Vec<_>>(),
        vec![10, 11, 12, 13]
    );
    assert_eq!(
        engine.raw_storage(&contract, b"ordered"),
        Some(b"3".to_vec())
    );
    assert!(outcome
        .transactions
        .iter()
        .all(|result| result.disposition == CanonicalTxDisposition::ReusedSpeculative));
}

#[test]
fn repeated_parallel_runs_are_deterministic_despite_worker_scheduling() {
    let mut expected_dispositions = None;
    let mut expected_values = None;

    for _ in 0..5 {
        let (engine, contract, _) = setup();
        let transactions = mixed_transactions(&contract);
        let outcome = execute_parallel(
            &engine,
            4,
            transactions,
            vec![tx_ids(&[10, 12]), tx_ids(&[11, 13])],
        );
        let dispositions = outcome
            .transactions
            .iter()
            .map(|result| result.disposition)
            .collect::<Vec<_>>();
        let values = [b"a".as_slice(), b"seen_a", b"b", b"seen_b"]
            .into_iter()
            .map(|key| engine.raw_storage(&contract, key))
            .collect::<Vec<_>>();

        match (&expected_dispositions, &expected_values) {
            (Some(expected_dispositions), Some(expected_values)) => {
                assert_eq!(&dispositions, expected_dispositions);
                assert_eq!(&values, expected_values);
            }
            _ => {
                expected_dispositions = Some(dispositions);
                expected_values = Some(values);
            }
        }
    }
}

#[test]
fn strict_launch_barrier_finishes_current_wave_before_next_wave_starts() {
    let (engine, contract, probe) = setup();
    let mut transactions = Vec::new();
    for index in 0_u32..4 {
        transactions.push(canonical(
            index,
            request(
                10 + u64::from(index),
                &contract,
                json!({
                    "action":"sleep_set",
                    "key":format!("wave0-{index}"),
                    "value":"done",
                    "delay_ms":30,
                    "wave":0
                }),
            ),
        ));
    }
    for index in 4_u32..8 {
        transactions.push(canonical(
            index,
            request(
                10 + u64::from(index),
                &contract,
                json!({
                    "action":"sleep_set",
                    "key":format!("wave1-{index}"),
                    "value":"done",
                    "delay_ms":0,
                    "wave":1
                }),
            ),
        ));
    }

    execute_parallel(
        &engine,
        4,
        transactions,
        vec![tx_ids(&[10, 11, 12, 13]), tx_ids(&[14, 15, 16, 17])],
    );

    let lifecycle = probe.lifecycle();
    let last_wave_zero_end = lifecycle
        .iter()
        .rposition(|entry| entry.starts_with("end:0:"))
        .unwrap();
    let first_wave_one_start = lifecycle
        .iter()
        .position(|entry| entry.starts_with("start:1:"))
        .unwrap();
    assert!(last_wave_zero_end < first_wave_one_start);
}

#[test]
fn later_wave_snapshot_includes_the_canonical_prefix_drained_from_previous_waves() {
    let (engine, contract, _) = setup();
    let transactions = vec![
        canonical(
            0,
            request(
                10,
                &contract,
                json!({"action":"blind_set","key":"source","value":"one"}),
            ),
        ),
        canonical(
            1,
            request(
                11,
                &contract,
                json!({"action":"read_then_write","source":"source","target":"seen"}),
            ),
        ),
    ];

    let outcome = execute_parallel(&engine, 2, transactions, vec![tx_ids(&[10]), tx_ids(&[11])]);

    assert_eq!(
        engine.raw_storage(&contract, b"seen"),
        Some(b"one".to_vec())
    );
    assert_eq!(outcome.metrics.speculative.reused_results, 2);
    assert_eq!(outcome.metrics.speculative.replayed_transactions, 0);
}

#[test]
fn pending_receipt_waits_for_missing_canonical_predecessor_then_replays_if_stale() {
    let (engine, contract, _) = setup();
    let transactions = vec![
        canonical(
            0,
            request(
                10,
                &contract,
                json!({"action":"blind_set","key":"a","value":"A"}),
            ),
        ),
        canonical(
            1,
            request(
                11,
                &contract,
                json!({"action":"blind_set","key":"source","value":"one"}),
            ),
        ),
        canonical(
            2,
            request(
                12,
                &contract,
                json!({"action":"read_then_write","source":"source","target":"seen"}),
            ),
        ),
    ];

    let outcome = execute_parallel(
        &engine,
        2,
        transactions,
        vec![tx_ids(&[10, 12]), tx_ids(&[11])],
    );

    assert_eq!(
        outcome
            .transactions
            .iter()
            .map(|result| result.disposition)
            .collect::<Vec<_>>(),
        vec![
            CanonicalTxDisposition::ReusedSpeculative,
            CanonicalTxDisposition::ReusedSpeculative,
            CanonicalTxDisposition::Replayed,
        ]
    );
    assert_eq!(outcome.metrics.speculative.invalidated_results, 1);
    assert_eq!(outcome.metrics.speculative.replayed_transactions, 1);
    assert_eq!(
        engine.raw_storage(&contract, b"seen"),
        Some(b"one".to_vec())
    );
}

#[test]
fn dependency_executor_publishes_completed_predecessor_without_waiting_for_level_barrier() {
    let (engine, contract, probe) = setup();
    let transactions = vec![
        canonical(0, request(10, &contract, json!({"action":"noop"}))),
        canonical(
            1,
            request(
                11,
                &contract,
                json!({
                    "action":"sleep_set",
                    "key":"unrelated",
                    "value":"slow",
                    "delay_ms":120,
                    "wave":0
                }),
            ),
        ),
        canonical(
            2,
            request(
                12,
                &contract,
                json!({"action":"blind_set","key":"source","value":"one"}),
            ),
        ),
        canonical(
            3,
            request(
                13,
                &contract,
                json!({
                    "action":"read_then_write_probe",
                    "source":"source",
                    "target":"seen",
                    "wave":1
                }),
            ),
        ),
    ];

    let prepared = engine
        .preexecute_dependency_plan(
            ParallelExecutionConfig { workers: 4 },
            transactions.clone(),
            vec![tx_ids(&[10, 11, 12]), tx_ids(&[13])],
            vec![hard_dependency(12, 13)],
        )
        .unwrap();

    // Tx13 depends only on tx12. It may begin as soon as tx12 publishes its version, even while
    // unrelated tx11 from the previous diagnostic level is still running.
    let lifecycle = probe.lifecycle();
    let tx13_start = lifecycle
        .iter()
        .position(|entry| entry == "start:1:13")
        .expect("dependent successor never started");
    let tx11_end = lifecycle
        .iter()
        .position(|entry| entry == "end:0:11")
        .expect("slow unrelated transaction never completed");
    assert!(
        tx13_start < tx11_end,
        "dependency executor retained a global level barrier"
    );
    assert_eq!(prepared.metrics.dependency_count, 1);
    assert_eq!(prepared.metrics.hard_dependency_count, 1);

    let outcome = engine
        .reconcile_prepared_block(transactions, prepared)
        .unwrap();
    assert_eq!(outcome.speculative.reused_results, 4);
    assert_eq!(outcome.speculative.replayed_transactions, 0);
    assert_eq!(
        engine.raw_storage(&contract, b"seen"),
        Some(b"one".to_vec())
    );
}

#[test]
fn dependency_executor_never_exposes_future_canonical_version_to_earlier_transaction() {
    let (engine, contract, probe) = setup();
    let transactions = vec![
        canonical(
            0,
            request(
                10,
                &contract,
                json!({
                    "action":"sleep_read_then_write",
                    "source":"source",
                    "target":"seen",
                    "delay_ms":100,
                    "wave":0
                }),
            ),
        ),
        canonical(
            1,
            request(
                11,
                &contract,
                json!({
                    "action":"sleep_set",
                    "key":"source",
                    "value":"future",
                    "delay_ms":0,
                    "wave":0
                }),
            ),
        ),
    ];

    let prepared = engine
        .preexecute_dependency_plan(
            ParallelExecutionConfig { workers: 2 },
            transactions.clone(),
            vec![tx_ids(&[10, 11])],
            Vec::new(),
        )
        .unwrap();

    let lifecycle = probe.lifecycle();
    let future_end = lifecycle
        .iter()
        .position(|entry| entry == "end:0:11")
        .expect("future-canonical transaction did not finish");
    let earlier_end = lifecycle
        .iter()
        .position(|entry| entry == "end:0:10")
        .expect("earlier transaction did not finish");
    assert!(
        future_end < earlier_end,
        "test did not exercise out-of-order completion"
    );

    let outcome = engine
        .reconcile_prepared_block(transactions, prepared)
        .unwrap();
    assert_eq!(outcome.speculative.reused_results, 2);
    assert_eq!(outcome.speculative.replayed_transactions, 0);
    assert_eq!(
        engine.raw_storage(&contract, b"seen"),
        Some(b"zero".to_vec())
    );
    assert_eq!(
        engine.raw_storage(&contract, b"source"),
        Some(b"future".to_vec())
    );
}

#[test]
fn dependency_executor_rejects_dependency_that_does_not_advance_scheduler_level() {
    let (engine, contract, _) = setup();
    let transactions = vec![
        canonical(0, request(10, &contract, json!({"action":"noop"}))),
        canonical(1, request(11, &contract, json!({"action":"noop"}))),
    ];
    let before = engine.snapshot();
    assert!(engine
        .preexecute_dependency_plan(
            ParallelExecutionConfig { workers: 2 },
            transactions,
            vec![tx_ids(&[10, 11])],
            vec![hard_dependency(10, 11)],
        )
        .is_err());
    assert!(before.same_world_state(&engine.snapshot()));
}

#[test]
fn malformed_wave_plans_are_rejected_before_canonical_state_changes() {
    let cases = vec![
        vec![SpeculativeWave::new(Vec::new())],
        vec![tx_ids(&[10]), tx_ids(&[10])],
        vec![tx_ids(&[99])],
        Vec::new(),
    ];

    for waves in cases {
        let (engine, contract, _) = setup();
        let transactions = vec![canonical(
            0,
            request(
                10,
                &contract,
                json!({"action":"blind_set","key":"should_not_exist","value":"x"}),
            ),
        )];
        assert!(engine
            .execute_parallel_waves(ParallelExecutionConfig { workers: 2 }, transactions, waves)
            .is_err());
        assert!(engine.raw_storage(&contract, b"should_not_exist").is_none());
    }
}

#[test]
fn zero_workers_is_rejected_before_canonical_state_changes() {
    let (engine, contract, _) = setup();
    let transactions = vec![canonical(
        0,
        request(
            10,
            &contract,
            json!({"action":"blind_set","key":"should_not_exist","value":"x"}),
        ),
    )];

    assert!(engine
        .execute_parallel_waves(
            ParallelExecutionConfig { workers: 0 },
            transactions,
            vec![tx_ids(&[10])],
        )
        .is_err());
    assert!(engine.raw_storage(&contract, b"should_not_exist").is_none());
}

#[test]
fn empty_parallel_block_is_supported() {
    let engine = CosmWasmEngine::default();
    let outcome = execute_parallel(&engine, 4, Vec::new(), Vec::new());
    assert!(outcome.transactions.is_empty());
    assert_eq!(outcome.metrics.wave_count(), 0);
    assert_eq!(outcome.metrics.speculative.speculative_results, 0);
    assert_eq!(outcome.metrics.equal_cost_theoretical_speedup(), 0.0);
}

#[test]
fn real_wasm_transactions_execute_through_the_parallel_pool_and_reconcile_canonically() {
    let engine = CosmWasmEngine::default();
    engine
        .set_balance("verifier", &[Coin::new(300_u128, "utest")])
        .unwrap();
    let compact: String = HACKATOM_BASE64.split_whitespace().collect();
    let wasm = STANDARD.decode(compact).unwrap();
    let code_id = engine.upload_wasm(wasm).unwrap();

    let first = engine
        .instantiate(
            TransactionId(1),
            block(0),
            Address::from("verifier"),
            code_id,
            None,
            "parallel-hackatom-a".to_owned(),
            vec![Coin::new(100_u128, "utest")],
            to_json_binary(&json!({
                "verifier": "verifier",
                "beneficiary": "beneficiary"
            }))
            .unwrap(),
        )
        .unwrap()
        .contract;
    let second = engine
        .instantiate(
            TransactionId(2),
            block(0),
            Address::from("verifier"),
            code_id,
            None,
            "parallel-hackatom-b".to_owned(),
            vec![Coin::new(100_u128, "utest")],
            to_json_binary(&json!({
                "verifier": "verifier",
                "beneficiary": "beneficiary"
            }))
            .unwrap(),
        )
        .unwrap()
        .contract;

    let transactions = vec![
        canonical(
            0,
            ExecutionRequest::Execute {
                transaction_id: TransactionId(10),
                sender: Address::from("verifier"),
                contract: first.clone(),
                funds: Vec::new(),
                msg: Binary::from(br#"{"release":{}}"#.as_slice()),
            },
        ),
        canonical(
            1,
            ExecutionRequest::Execute {
                transaction_id: TransactionId(11),
                sender: Address::from("verifier"),
                contract: second.clone(),
                funds: Vec::new(),
                msg: Binary::from(br#"{"release":{}}"#.as_slice()),
            },
        ),
    ];

    let outcome = execute_parallel(&engine, 2, transactions, vec![tx_ids(&[10, 11])]);

    assert!(outcome
        .transactions
        .iter()
        .all(|result| result.result.is_ok()));
    assert_eq!(engine.balance(first, "utest"), 0);
    assert_eq!(engine.balance(second, "utest"), 0);
    assert_eq!(engine.balance("beneficiary", "utest"), 200);
    assert_eq!(outcome.metrics.speculative.reused_results, 1);
    assert_eq!(outcome.metrics.speculative.replayed_transactions, 1);
}

#[test]
fn split_phase_preexecution_does_not_mutate_canonical_state_until_reconciliation() {
    let (engine, contract, _) = setup();
    let transactions = vec![canonical(
        0,
        request(
            10,
            &contract,
            json!({"action":"blind_set","key":"prepared","value":"yes"}),
        ),
    )];

    let prepared = engine
        .preexecute_parallel_waves(
            ParallelExecutionConfig { workers: 2 },
            transactions.clone(),
            vec![tx_ids(&[10])],
        )
        .unwrap();

    assert!(engine.raw_storage(&contract, b"prepared").is_none());
    assert_eq!(prepared.predicted_transaction_count(), 1);
    assert_eq!(prepared.metrics.speculative.speculative_results, 1);

    let reconciled = engine
        .reconcile_prepared_block(transactions, prepared)
        .unwrap();
    assert_eq!(reconciled.speculative.reused_results, 1);
    assert_eq!(reconciled.prediction.precision(), 1.0);
    assert_eq!(reconciled.prediction.coverage(), 1.0);
    assert_eq!(
        engine.raw_storage(&contract, b"prepared"),
        Some(b"yes".to_vec())
    );
}

#[test]
fn predicted_successor_can_seed_next_block_before_current_block_is_committed() {
    let (engine, contract, _) = setup();
    let first = vec![canonical(
        0,
        request(
            10,
            &contract,
            json!({"action":"blind_set","key":"source","value":"one"}),
        ),
    )];
    let prepared_first = engine
        .preexecute_parallel_waves(
            ParallelExecutionConfig { workers: 2 },
            first.clone(),
            vec![tx_ids(&[10])],
        )
        .unwrap();
    let predicted_after_first = prepared_first.predicted_successor_snapshot();

    let second = vec![canonical(
        0,
        request(
            11,
            &contract,
            json!({"action":"read_then_write","source":"source","target":"seen"}),
        ),
    )];
    let prepared_second = engine
        .preexecute_parallel_waves_from_snapshot(
            &predicted_after_first,
            ParallelExecutionConfig { workers: 2 },
            second.clone(),
            vec![tx_ids(&[11])],
        )
        .unwrap();

    // Neither pre-consensus phase changed canonical state.
    assert_eq!(
        engine.raw_storage(&contract, b"source"),
        Some(b"zero".to_vec())
    );
    assert!(engine.raw_storage(&contract, b"seen").is_none());

    let first_outcome = engine
        .reconcile_prepared_block(first, prepared_first)
        .unwrap();
    let second_outcome = engine
        .reconcile_prepared_block(second, prepared_second)
        .unwrap();

    assert_eq!(first_outcome.speculative.reused_results, 1);
    assert_eq!(second_outcome.speculative.reused_results, 1);
    assert_eq!(second_outcome.speculative.replayed_transactions, 0);
    assert_eq!(
        engine.raw_storage(&contract, b"seen"),
        Some(b"one".to_vec())
    );
}

#[test]
fn split_phase_reconciliation_handles_prediction_misses_without_trusting_receipt_identity() {
    let (engine, contract, _) = setup();
    let predicted = vec![canonical(
        0,
        request(10, &contract, json!({"action":"noop"})),
    )];
    let prepared = engine
        .preexecute_parallel_waves(
            ParallelExecutionConfig { workers: 2 },
            predicted,
            vec![tx_ids(&[10])],
        )
        .unwrap();

    let decided = vec![canonical(
        0,
        request(
            10,
            &contract,
            json!({"action":"blind_set","key":"actual","value":"decided"}),
        ),
    )];
    let outcome = engine.reconcile_prepared_block(decided, prepared).unwrap();

    assert_eq!(outcome.prediction.matched_transactions, 0);
    assert_eq!(outcome.prediction.discarded_predictions, 1);
    assert_eq!(outcome.prediction.missing_predictions, 1);
    assert_eq!(outcome.speculative.canonical_transactions, 1);
    assert_eq!(
        engine.raw_storage(&contract, b"actual"),
        Some(b"decided".to_vec())
    );
}

#[test]
fn split_phase_prediction_metrics_count_extra_and_missing_transactions() {
    let (engine, contract, _) = setup();
    let predicted = vec![
        canonical(0, request(10, &contract, json!({"action":"noop"}))),
        canonical(1, request(11, &contract, json!({"action":"noop"}))),
    ];
    let prepared = engine
        .preexecute_parallel_waves(
            ParallelExecutionConfig { workers: 2 },
            predicted,
            vec![tx_ids(&[10, 11])],
        )
        .unwrap();

    let decided = vec![
        canonical(0, request(10, &contract, json!({"action":"noop"}))),
        canonical(
            1,
            request(
                12,
                &contract,
                json!({"action":"blind_set","key":"missing","value":"executed"}),
            ),
        ),
    ];
    let outcome = engine.reconcile_prepared_block(decided, prepared).unwrap();

    assert_eq!(outcome.prediction.predicted_transactions, 2);
    assert_eq!(outcome.prediction.decided_transactions, 2);
    assert_eq!(outcome.prediction.matched_transactions, 1);
    assert_eq!(outcome.prediction.discarded_predictions, 1);
    assert_eq!(outcome.prediction.missing_predictions, 1);
    assert!((outcome.prediction.precision() - 0.5).abs() < f64::EPSILON);
    assert!((outcome.prediction.coverage() - 0.5).abs() < f64::EPSILON);
    assert_eq!(outcome.speculative.reused_results, 1);
    assert_eq!(outcome.speculative.canonical_transactions, 1);
}

#[test]
fn split_phase_execution_matches_serial_world_state_with_selective_replay() {
    let (serial, serial_contract, _) = setup();
    let serial_transactions = mixed_transactions(&serial_contract);
    serial_execute(&serial, &serial_transactions);
    let serial_snapshot = serial.snapshot();

    let (split, split_contract, _) = setup();
    let transactions = mixed_transactions(&split_contract);
    let prepared = split
        .preexecute_parallel_waves(
            ParallelExecutionConfig { workers: 4 },
            transactions.clone(),
            vec![tx_ids(&[10, 12]), tx_ids(&[11, 13])],
        )
        .unwrap();
    let outcome = split
        .reconcile_prepared_block(transactions, prepared)
        .unwrap();

    assert!(serial_snapshot.same_world_state(&split.snapshot()));
    assert_eq!(outcome.transactions.len(), 4);
    assert!(outcome
        .transactions
        .iter()
        .all(|result| result.result.is_ok()));
    assert!(outcome.speculative.reused_results > 0);
    assert!(outcome.timings.total >= outcome.timings.validation);
}
