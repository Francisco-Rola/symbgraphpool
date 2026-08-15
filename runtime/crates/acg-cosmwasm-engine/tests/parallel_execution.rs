use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Mutex};
use std::time::Duration;

use acg_cosmwasm_engine::{
    Address, BlockContext, CanonicalTransaction, CosmWasmEngine, ExecutionRequest,
    NativeCallContext, NativeContract, ParallelExecutionConfig, SpeculativeDependency,
    SpeculativeDependencyClass, SpeculativeWave, TransactionId,
};
use cosmwasm_std::{to_json_binary, Binary, Empty, Env, MessageInfo, Response};
use serde_json::{json, Value};

struct ConcurrencyProbe {
    active: AtomicUsize,
    max_active: AtomicUsize,
    lifecycle: Mutex<Vec<String>>,
    barrier: Barrier,
    read_barrier_invocations: AtomicUsize,
}

impl Default for ConcurrencyProbe {
    fn default() -> Self {
        Self {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            lifecycle: Mutex::new(Vec::new()),
            barrier: Barrier::new(2),
            read_barrier_invocations: AtomicUsize::new(0),
        }
    }
}

impl ConcurrencyProbe {
    fn enter(&self, label: &str, transaction_id: TransactionId) {
        let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_active.fetch_max(active, Ordering::SeqCst);
        self.lifecycle
            .lock()
            .unwrap()
            .push(format!("start:{label}:{}", transaction_id.0));
    }

    fn exit(&self, label: &str, transaction_id: TransactionId) {
        self.lifecycle
            .lock()
            .unwrap()
            .push(format!("end:{label}:{}", transaction_id.0));
        self.active.fetch_sub(1, Ordering::SeqCst);
    }

    fn max_active(&self) -> usize {
        self.max_active.load(Ordering::SeqCst)
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
            Some("sleep_set") => {
                let key = message["key"].as_str().ok_or("missing key")?;
                let value = message["value"].as_str().ok_or("missing value")?;
                let delay_ms = message["delay_ms"].as_u64().ok_or("missing delay_ms")?;
                let label = message["label"].as_str().unwrap_or("set");
                self.probe.enter(label, context.transaction_id);
                std::thread::sleep(Duration::from_millis(delay_ms));
                context.storage_set(key.as_bytes(), value.as_bytes());
                self.probe.exit(label, context.transaction_id);
                Ok(Response::new())
            }
            Some("sleep_read_then_write") => {
                let source = message["source"].as_str().ok_or("missing source")?;
                let target = message["target"].as_str().ok_or("missing target")?;
                let delay_ms = message["delay_ms"].as_u64().ok_or("missing delay_ms")?;
                let label = message["label"].as_str().unwrap_or("read");
                self.probe.enter(label, context.transaction_id);
                std::thread::sleep(Duration::from_millis(delay_ms));
                let value = context.storage_get(source.as_bytes()).unwrap_or_default();
                context.storage_set(target.as_bytes(), value);
                self.probe.exit(label, context.transaction_id);
                Ok(Response::new())
            }
            Some("read_sleep_read") => {
                let source = message["source"].as_str().ok_or("missing source")?;
                let target = message["target"].as_str().ok_or("missing target")?;
                let delay_ms = message["delay_ms"].as_u64().ok_or("missing delay_ms")?;
                let first = context.storage_get(source.as_bytes()).unwrap_or_default();
                std::thread::sleep(Duration::from_millis(delay_ms));
                let second = context.storage_get(source.as_bytes()).unwrap_or_default();
                let mut value = first;
                value.push(b'|');
                value.extend(second);
                context.storage_set(target.as_bytes(), value);
                Ok(Response::new())
            }
            Some("barrier_then_set") => {
                let key = message["key"].as_str().ok_or("missing key")?;
                let value = message["value"].as_str().ok_or("missing value")?;
                self.probe.barrier.wait();
                context.storage_set(key.as_bytes(), value.as_bytes());
                Ok(Response::new())
            }
            Some("read_barrier_read") => {
                let source = message["source"].as_str().ok_or("missing source")?;
                let target = message["target"].as_str().ok_or("missing target")?;
                let first = context.storage_get(source.as_bytes()).unwrap_or_default();
                if self
                    .probe
                    .read_barrier_invocations
                    .fetch_add(1, Ordering::SeqCst)
                    == 0
                {
                    self.probe.barrier.wait();
                    std::thread::sleep(Duration::from_millis(20));
                }
                let second = context.storage_get(source.as_bytes()).unwrap_or_default();
                let mut value = first;
                value.push(b'|');
                value.extend(second);
                context.storage_set(target.as_bytes(), value);
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
    let code_id = engine
        .register_native(
            "parallel-mvcc-fixture",
            Arc::new(ParallelContract {
                probe: probe.clone(),
            }),
        )
        .unwrap();
    let contract = engine
        .instantiate(
            TransactionId(1),
            block(0),
            Address::from("alice"),
            code_id,
            None,
            "parallel-mvcc-fixture".to_owned(),
            Vec::new(),
            Binary::default(),
        )
        .unwrap()
        .contract;
    (engine, contract, probe)
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

#[test]
fn independent_transactions_overlap_without_global_wave_barrier() {
    let (engine, contract, probe) = setup();
    let transactions = vec![
        canonical(
            0,
            request(
                10,
                &contract,
                json!({"action":"sleep_set","key":"a","value":"one","delay_ms":50,"label":"a"}),
            ),
        ),
        canonical(
            1,
            request(
                11,
                &contract,
                json!({"action":"sleep_set","key":"b","value":"two","delay_ms":50,"label":"b"}),
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

    assert!(probe.max_active() >= 2);
    assert_eq!(prepared.metrics.dependency_count, 0);
    assert_eq!(
        prepared
            .metrics
            .dependency_diagnostics
            .visibility_masks_captured,
        2
    );

    let outcome = engine
        .reconcile_prepared_block(transactions, prepared)
        .unwrap();
    assert_eq!(outcome.speculative.reused_results, 2);
    assert_eq!(outcome.speculative.replayed_transactions, 0);
}

#[test]
fn dependency_successor_consumes_published_predecessor_while_unrelated_work_is_running() {
    let (engine, contract, probe) = setup();
    let transactions = vec![
        canonical(0, request(10, &contract, json!({"action":"noop"}))),
        canonical(
            1,
            request(
                11,
                &contract,
                json!({"action":"sleep_set","key":"unrelated","value":"slow","delay_ms":120,"label":"unrelated"}),
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
                json!({"action":"sleep_read_then_write","source":"source","target":"seen","delay_ms":0,"label":"successor"}),
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

    let lifecycle = probe.lifecycle();
    let successor_start = lifecycle
        .iter()
        .position(|entry| entry == "start:successor:13")
        .unwrap();
    let unrelated_end = lifecycle
        .iter()
        .position(|entry| entry == "end:unrelated:11")
        .unwrap();
    assert!(successor_start < unrelated_end);

    let diagnostics = &prepared.metrics.dependency_diagnostics;
    assert_eq!(diagnostics.visibility_masks_captured, 4);
    assert!(diagnostics.published_storage_versions >= 3);

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
fn long_publishing_dependency_chain_completes_without_false_readiness_deadlock() {
    let (engine, contract, _) = setup();
    const TRANSACTIONS: u64 = 128;

    let transactions = (0..TRANSACTIONS)
        .map(|index| {
            canonical(
                index as u32,
                request(
                    10 + index,
                    &contract,
                    json!({
                        "action":"blind_set",
                        "key":format!("chain-{index}"),
                        "value":"published",
                    }),
                ),
            )
        })
        .collect::<Vec<_>>();
    let waves = (0..TRANSACTIONS)
        .map(|index| tx_ids(&[10 + index]))
        .collect::<Vec<_>>();
    let dependencies = (0..TRANSACTIONS - 1)
        .map(|index| hard_dependency(10 + index, 11 + index))
        .collect::<Vec<_>>();

    let prepared = engine
        .preexecute_dependency_plan(
            ParallelExecutionConfig { workers: 8 },
            transactions.clone(),
            waves,
            dependencies,
        )
        .unwrap();

    assert_eq!(prepared.receipts.len(), TRANSACTIONS as usize);
    assert_eq!(
        prepared
            .metrics
            .dependency_diagnostics
            .visibility_masks_captured,
        TRANSACTIONS
    );
    assert_eq!(prepared.metrics.dependency_diagnostics.max_in_flight, 1);

    let outcome = engine
        .reconcile_prepared_block(transactions, prepared)
        .unwrap();
    assert_eq!(outcome.speculative.reused_results, TRANSACTIONS);
    assert_eq!(outcome.speculative.replayed_transactions, 0);
}

#[test]
fn mvcc_launch_snapshot_never_observes_future_canonical_version() {
    let (engine, contract, probe) = setup();
    let transactions = vec![
        canonical(
            0,
            request(
                10,
                &contract,
                json!({"action":"sleep_read_then_write","source":"source","target":"seen","delay_ms":100,"label":"earlier"}),
            ),
        ),
        canonical(
            1,
            request(
                11,
                &contract,
                json!({"action":"sleep_set","key":"source","value":"future","delay_ms":0,"label":"future"}),
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
        .position(|entry| entry == "end:future:11")
        .unwrap();
    let earlier_end = lifecycle
        .iter()
        .position(|entry| entry == "end:earlier:10")
        .unwrap();
    assert!(future_end < earlier_end);

    let outcome = engine
        .reconcile_prepared_block(transactions, prepared)
        .unwrap();
    assert_eq!(outcome.speculative.reused_results, 2);
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
fn launch_visibility_is_frozen_even_when_an_earlier_independent_version_finishes_mid_transaction() {
    let (engine, contract, _) = setup();
    let transactions = vec![
        canonical(
            0,
            request(
                10,
                &contract,
                json!({"action":"barrier_then_set","key":"source","value":"one"}),
            ),
        ),
        canonical(
            1,
            request(
                11,
                &contract,
                json!({"action":"read_barrier_read","source":"source","target":"double_seen"}),
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

    let speculative_value = prepared.receipts[1]
        .write_set
        .storage
        .iter()
        .find(|write| write.key.as_slice() == b"double_seen")
        .and_then(|write| write.value.clone());
    assert_eq!(speculative_value, Some(b"zero|zero".to_vec()));

    let outcome = engine
        .reconcile_prepared_block(transactions, prepared)
        .unwrap();
    assert_eq!(outcome.speculative.replayed_transactions, 1);
    assert_eq!(
        engine.raw_storage(&contract, b"double_seen"),
        Some(b"one|one".to_vec())
    );
}

#[test]
fn dependency_must_advance_diagnostic_level() {
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
fn zero_workers_is_rejected_before_speculative_execution() {
    let (engine, contract, _) = setup();
    let transactions = vec![canonical(
        0,
        request(10, &contract, json!({"action":"noop"})),
    )];
    let before = engine.snapshot();

    assert!(engine
        .preexecute_dependency_plan(
            ParallelExecutionConfig { workers: 0 },
            transactions,
            vec![tx_ids(&[10])],
            Vec::new(),
        )
        .is_err());
    assert!(before.same_world_state(&engine.snapshot()));
}

#[test]
fn consensus_cutoff_stops_new_serial_launches_without_preconsensus_commit() {
    let (engine, contract, _) = setup();
    let transactions = vec![
        canonical(
            0,
            request(
                10,
                &contract,
                json!({"action":"sleep_set","key":"first","value":"one","delay_ms":30,"label":"first"}),
            ),
        ),
        canonical(
            1,
            request(
                11,
                &contract,
                json!({"action":"blind_set","key":"second","value":"two"}),
            ),
        ),
    ];
    let before = engine.snapshot();

    let prepared = engine
        .preexecute_dependency_plan_from_snapshot_with_cutoff(
            &before,
            ParallelExecutionConfig { workers: 1 },
            transactions.clone(),
            vec![tx_ids(&[10]), tx_ids(&[11])],
            vec![hard_dependency(10, 11)],
            Some(Duration::from_millis(5)),
        )
        .unwrap();

    assert_eq!(prepared.receipts.len(), 1);
    assert_eq!(prepared.metrics.speculative.speculative_results, 1);
    assert!(prepared.metrics.dependency_diagnostics.cutoff_reached);
    assert_eq!(
        prepared
            .metrics
            .dependency_diagnostics
            .receipts_ready_by_cutoff,
        0
    );
    assert_eq!(
        prepared
            .metrics
            .dependency_diagnostics
            .receipts_completed_after_cutoff,
        1
    );
    assert!(before.same_world_state(&engine.snapshot()));
    assert_eq!(engine.raw_storage(&contract, b"first"), None);
    assert_eq!(engine.raw_storage(&contract, b"second"), None);

    let outcome = engine
        .reconcile_prepared_block(transactions, prepared)
        .unwrap();
    assert_eq!(outcome.speculative.speculative_results, 1);
    assert_eq!(outcome.speculative.reused_results, 1);
    assert_eq!(outcome.speculative.canonical_transactions, 1);
    assert_eq!(
        engine.raw_storage(&contract, b"first"),
        Some(b"one".to_vec())
    );
    assert_eq!(
        engine.raw_storage(&contract, b"second"),
        Some(b"two".to_vec())
    );
}

#[test]
fn consensus_cutoff_stops_parallel_launch_frontier_and_reconciles_remainder() {
    let (engine, contract, _) = setup();
    let transactions = (0_u64..4)
        .map(|index| {
            canonical(
                index as u32,
                request(
                    20 + index,
                    &contract,
                    json!({
                        "action":"sleep_set",
                        "key":format!("parallel-{index}"),
                        "value":format!("value-{index}"),
                        "delay_ms":50,
                        "label":format!("parallel-{index}")
                    }),
                ),
            )
        })
        .collect::<Vec<_>>();
    let before = engine.snapshot();

    let prepared = engine
        .preexecute_dependency_plan_from_snapshot_with_cutoff(
            &before,
            ParallelExecutionConfig { workers: 2 },
            transactions.clone(),
            vec![tx_ids(&[20, 21, 22, 23])],
            Vec::new(),
            Some(Duration::from_millis(15)),
        )
        .unwrap();

    assert!(prepared.metrics.dependency_diagnostics.cutoff_reached);
    assert!(!prepared.receipts.is_empty());
    assert!(prepared.receipts.len() <= 2);
    assert_eq!(
        prepared.metrics.speculative.speculative_results,
        prepared.receipts.len() as u64
    );
    assert_eq!(
        prepared
            .metrics
            .dependency_diagnostics
            .receipts_ready_by_cutoff
            + prepared
                .metrics
                .dependency_diagnostics
                .receipts_completed_after_cutoff,
        prepared.receipts.len() as u64
    );
    assert!(before.same_world_state(&engine.snapshot()));
    for index in 0..4 {
        assert_eq!(
            engine.raw_storage(&contract, format!("parallel-{index}").as_bytes()),
            None
        );
    }

    let prepared_count = prepared.receipts.len() as u64;
    let outcome = engine
        .reconcile_prepared_block(transactions, prepared)
        .unwrap();
    assert_eq!(outcome.speculative.reused_results, prepared_count);
    assert_eq!(
        outcome.speculative.canonical_transactions,
        4 - prepared_count
    );
    for index in 0..4 {
        assert_eq!(
            engine.raw_storage(&contract, format!("parallel-{index}").as_bytes()),
            Some(format!("value-{index}").into_bytes())
        );
    }
}
