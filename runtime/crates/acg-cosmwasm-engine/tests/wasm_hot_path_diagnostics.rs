use acg_cosmwasm_engine::{
    Address, BlockContext, CanonicalTransaction, CosmWasmEngine, ExecutionRequest,
    ParallelExecutionConfig, SpeculativeWave, TransactionId,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use cosmwasm_std::{to_json_binary, Binary, Coin};
use serde_json::json;

const HACKATOM_BASE64: &str = include_str!("../testdata/hackatom_1.2.wasm.b64");

#[test]
fn dependency_preexecution_reports_wasm_and_host_hot_path_activity() {
    let engine = CosmWasmEngine::default();
    engine
        .set_balance("verifier", &[Coin::new(250_u128, "utest")])
        .unwrap();
    let compact: String = HACKATOM_BASE64.split_whitespace().collect();
    let wasm = STANDARD.decode(compact).unwrap();
    let code_id = engine.upload_wasm(wasm).unwrap();
    let contract = engine
        .instantiate(
            TransactionId(1),
            BlockContext::default(),
            Address::from("verifier"),
            code_id,
            None,
            "diagnostic-hackatom".to_owned(),
            vec![Coin::new(100_u128, "utest")],
            to_json_binary(&json!({
                "verifier": "verifier",
                "beneficiary": "beneficiary"
            }))
            .unwrap(),
        )
        .unwrap()
        .contract;

    let request = ExecutionRequest::Execute {
        transaction_id: TransactionId(2),
        sender: Address::from("verifier"),
        contract,
        funds: Vec::new(),
        msg: Binary::from(br#"{"release":{}}"#.as_slice()),
    };
    let transaction = CanonicalTransaction::new(
        BlockContext {
            transaction_index: Some(0),
            ..BlockContext::default()
        },
        request,
    );

    let prepared = engine
        .preexecute_dependency_plan(
            ParallelExecutionConfig { workers: 1 },
            vec![transaction.clone()],
            vec![SpeculativeWave::new(vec![TransactionId(2)])],
            Vec::new(),
        )
        .unwrap();

    let diagnostics = &prepared.metrics.dependency_diagnostics.contract;
    assert_eq!(diagnostics.wasm_instance_acquires, 1);
    assert_eq!(diagnostics.wasm_entrypoint_calls, 1);
    assert_eq!(diagnostics.wasm_instance_recycles, 1);
    assert!(diagnostics.wasm_cache_pinned_hits >= 1);
    assert_eq!(diagnostics.wasm_cache_misses, 0);
    assert!(diagnostics.host_storage_gets > 0);
    assert!(diagnostics.mvcc_storage_point_reads > 0);
    assert!(diagnostics.receipt_access_records > 0);
    assert!(diagnostics.receipt_read_dependencies > 0);

    let outcome = engine
        .reconcile_prepared_block(vec![transaction], prepared)
        .unwrap();
    assert_eq!(outcome.speculative.reused_results, 1);
    assert_eq!(outcome.speculative.replayed_transactions, 0);
}
