use acg_cosmwasm_engine::{Address, BlockContext, CosmWasmEngine, ExecutionRequest, TransactionId};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use cosmwasm_std::{to_json_binary, Binary, Coin};
use serde_json::json;

const HACKATOM_BASE64: &str = include_str!("../testdata/hackatom_1.2.wasm.b64");

#[test]
fn executes_real_wasm_and_applies_emitted_bank_send() {
    let engine = CosmWasmEngine::default();
    engine
        .set_balance("verifier", &[Coin::new(250_u128, "utest")])
        .unwrap();
    let compact: String = HACKATOM_BASE64.split_whitespace().collect();
    let wasm = STANDARD.decode(compact).unwrap();
    let code_id = engine.upload_wasm(wasm).unwrap();

    let instantiated = engine
        .instantiate(
            TransactionId(1),
            BlockContext::default(),
            Address::from("verifier"),
            code_id,
            None,
            "hackatom".to_owned(),
            vec![Coin::new(100_u128, "utest")],
            to_json_binary(&json!({
                "verifier": "verifier",
                "beneficiary": "beneficiary"
            }))
            .unwrap(),
        )
        .unwrap();

    engine
        .execute(
            TransactionId(2),
            BlockContext::default(),
            Address::from("verifier"),
            instantiated.contract.clone(),
            vec![],
            Binary::from(br#"{"release":{}}"#.as_slice()),
        )
        .unwrap();

    assert_eq!(engine.balance("verifier", "utest"), 150);
    assert_eq!(engine.balance(instantiated.contract, "utest"), 0);
    assert_eq!(engine.balance("beneficiary", "utest"), 100);
}

#[test]
fn reusable_and_recycled_vm_lifecycles_produce_identical_canonical_state() {
    use acg_cosmwasm_engine::{EngineConfig, WasmInstanceLifecycle};

    fn run(
        lifecycle: WasmInstanceLifecycle,
    ) -> (
        u128,
        u128,
        u128,
        acg_cosmwasm_engine::ContractExecutionDiagnostics,
    ) {
        let engine = CosmWasmEngine::new(EngineConfig {
            wasm_instance_lifecycle: lifecycle,
            ..EngineConfig::default()
        });
        engine
            .set_balance("verifier", &[Coin::new(250_u128, "utest")])
            .unwrap();
        let compact: String = HACKATOM_BASE64.split_whitespace().collect();
        let wasm = STANDARD.decode(compact).unwrap();
        let code_id = engine.upload_wasm(wasm).unwrap();
        let instantiated = engine
            .instantiate(
                TransactionId(1),
                BlockContext::default(),
                Address::from("verifier"),
                code_id,
                None,
                "hackatom".to_owned(),
                vec![Coin::new(100_u128, "utest")],
                to_json_binary(&json!({
                    "verifier": "verifier",
                    "beneficiary": "beneficiary"
                }))
                .unwrap(),
            )
            .unwrap();
        let (result, diagnostics) = engine.execute_request_with_diagnostics(
            BlockContext::default(),
            ExecutionRequest::Execute {
                transaction_id: TransactionId(2),
                sender: Address::from("verifier"),
                contract: instantiated.contract.clone(),
                funds: vec![],
                msg: Binary::from(br#"{"release":{}}"#.as_slice()),
            },
        );
        result.unwrap();
        (
            engine.balance("verifier", "utest"),
            engine.balance(instantiated.contract, "utest"),
            engine.balance("beneficiary", "utest"),
            diagnostics,
        )
    }

    assert_eq!(
        EngineConfig::default().wasm_instance_lifecycle,
        WasmInstanceLifecycle::Reuse
    );

    let reused = run(WasmInstanceLifecycle::Reuse);
    let recycled = run(WasmInstanceLifecycle::Recycle);
    assert_eq!(reused.0, recycled.0);
    assert_eq!(reused.1, recycled.1);
    assert_eq!(reused.2, recycled.2);
    assert_eq!(reused.3.wasm_instance_recycles, 0);
    assert_eq!(reused.3.wasm_instance_reuse_hits, 1);
    assert_eq!(reused.3.wasm_instance_pool_misses, 0);
    assert_eq!(recycled.3.wasm_instance_recycles, 1);
    assert_eq!(recycled.3.wasm_instance_reuse_hits, 0);
}

#[test]
fn reusable_vm_does_not_keep_temporary_cache_alive_after_engine_drop() {
    use acg_cosmwasm_engine::{EngineConfig, WasmInstanceLifecycle};

    let cache_dir = {
        let engine = CosmWasmEngine::new(EngineConfig {
            wasm_instance_lifecycle: WasmInstanceLifecycle::Reuse,
            ..EngineConfig::default()
        });
        engine
            .set_balance("verifier", &[Coin::new(250_u128, "utest")])
            .unwrap();
        let compact: String = HACKATOM_BASE64.split_whitespace().collect();
        let wasm = STANDARD.decode(compact).unwrap();
        let code_id = engine.upload_wasm(wasm).unwrap();
        engine
            .instantiate(
                TransactionId(1),
                BlockContext::default(),
                Address::from("verifier"),
                code_id,
                None,
                "cache-lifetime".to_owned(),
                vec![],
                to_json_binary(&json!({
                    "verifier": "verifier",
                    "beneficiary": "beneficiary"
                }))
                .unwrap(),
            )
            .unwrap();

        let cache_dir = engine.wasm_cache_dir().to_path_buf();
        assert!(cache_dir.exists());
        cache_dir
    };

    assert!(
        !cache_dir.exists(),
        "reusable VM retained the temporary CosmWasm cache directory {}",
        cache_dir.display()
    );
}
