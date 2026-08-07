use acg_cosmwasm_engine::{Address, BlockContext, CosmWasmEngine, TransactionId};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use cosmwasm_std::{to_json_binary, Binary, Coin};
use serde_json::json;

const HACKATOM_BASE64: &str = include_str!("../testdata/hackatom_1.2.wasm.b64");

#[test]
fn upload_compiles_and_pins_wasm_for_later_calls() {
    let engine = CosmWasmEngine::default();
    let compact: String = HACKATOM_BASE64.split_whitespace().collect();
    let wasm = STANDARD.decode(compact).unwrap();
    let code_id = engine.upload_wasm(wasm).unwrap();

    let after_upload = engine.wasm_cache_metrics();
    assert_eq!(after_upload.elements_pinned_memory_cache, 1);
    assert_eq!(after_upload.misses, 0);

    engine
        .set_balance("verifier", &[Coin::new(250_u128, "utest")])
        .unwrap();
    let instantiated = engine
        .instantiate(
            TransactionId(1),
            BlockContext::default(),
            Address::from("verifier"),
            code_id,
            None,
            "cached-hackatom".to_owned(),
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
            instantiated.contract,
            Vec::new(),
            Binary::from(br#"{"release":{}}"#.as_slice()),
        )
        .unwrap();

    let after_calls = engine.wasm_cache_metrics();
    assert!(after_calls.hits_pinned_memory_cache >= 2);
    assert_eq!(after_calls.misses, 0);
}
