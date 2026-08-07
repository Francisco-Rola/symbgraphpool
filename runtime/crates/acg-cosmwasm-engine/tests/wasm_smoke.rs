use acg_cosmwasm_engine::{Address, BlockContext, CosmWasmEngine, TransactionId};
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
