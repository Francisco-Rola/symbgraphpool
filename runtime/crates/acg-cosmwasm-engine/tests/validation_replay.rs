use std::sync::Arc;

use acg_cosmwasm_engine::{
    Address, BlockContext, CanonicalTransaction, CanonicalTxDisposition, CodeId, CosmWasmEngine,
    ExecutionOutcome, ExecutionRequest, NativeCallContext, NativeContract, ReadDependency,
    SpeculativeTxResult, TransactionId, ValidationConflict,
};
use cosmwasm_std::{
    to_json_binary, Binary, Coin, Empty, Env, MessageInfo, Reply, ReplyOn, Response, SubMsg,
    WasmMsg,
};
use serde_json::{json, Value};

struct ValidationContract;

impl NativeContract for ValidationContract {
    fn instantiate(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        _msg: Binary,
    ) -> Result<Response<Empty>, String> {
        context.storage_set(b"count", 0_u64.to_be_bytes());
        context.storage_set(b"item/a", b"one");
        context.storage_set(b"item/b", b"two");
        context.storage_set(b"local", b"old");
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
                let count = read_u64(context.storage_get(b"count"));
                context.storage_set(b"count", count.saturating_add(1).to_be_bytes());
                Ok(Response::new().add_attribute("action", "increment"))
            }
            Some("set") | Some("blind_write") => {
                let key = message["key"]
                    .as_str()
                    .ok_or("missing key")?
                    .as_bytes()
                    .to_vec();
                let value = message["value"]
                    .as_str()
                    .ok_or("missing value")?
                    .as_bytes()
                    .to_vec();
                context.storage_set(key, value);
                Ok(Response::new())
            }
            Some("remove") => {
                let key = message["key"]
                    .as_str()
                    .ok_or("missing key")?
                    .as_bytes()
                    .to_vec();
                context.storage_remove(key);
                Ok(Response::new())
            }
            Some("read_then_write") => {
                let key = message["key"]
                    .as_str()
                    .ok_or("missing key")?
                    .as_bytes()
                    .to_vec();
                let value = context.storage_get(&key).unwrap_or_default();
                context.storage_set(b"observed", value);
                Ok(Response::new())
            }
            Some("range_count") => {
                let entries = context.storage_range(Some(b"item/"), Some(b"item0"));
                context.storage_set(b"seen", (entries.len() as u64).to_be_bytes());
                Ok(Response::new())
            }
            Some("write_then_read") => {
                context.storage_set(b"local", b"mine");
                let value = context.storage_get(b"local").unwrap_or_default();
                context.storage_set(b"echo", value);
                Ok(Response::new())
            }
            Some("write_then_range") => {
                context.storage_set(b"item/a", b"mine");
                let entries = context.storage_range(Some(b"item/"), Some(b"item0"));
                context.storage_set(b"seen", (entries.len() as u64).to_be_bytes());
                Ok(Response::new())
            }
            Some("read_balance") => {
                let owner = Address::new(message["owner"].as_str().ok_or("missing owner")?);
                let denom = message["denom"].as_str().ok_or("missing denom")?;
                let amount = context.balance(&owner, denom);
                context.storage_set(b"balance_seen", amount.to_be_bytes());
                Ok(Response::new())
            }
            Some("observe_all_balances") => {
                let owner = Address::new(message["owner"].as_str().ok_or("missing owner")?);
                let balances = context.all_balances(&owner);
                context.storage_set(b"balance_count", (balances.len() as u64).to_be_bytes());
                Ok(Response::new())
            }
            Some("send") => {
                let to = Address::new(message["to"].as_str().ok_or("missing to")?);
                let denom = message["denom"].as_str().ok_or("missing denom")?;
                let amount = message["amount"].as_u64().ok_or("missing amount")?;
                context
                    .send(&to, &[Coin::new(u128::from(amount), denom)])
                    .map_err(|error| error.to_string())?;
                Ok(Response::new())
            }
            Some("fail_if_count_zero") => {
                let count = read_u64(context.storage_get(b"count"));
                if count == 0 {
                    context.storage_set(b"partial", b"rolled_back");
                    Err("count is zero".to_owned())
                } else {
                    context.storage_set(b"recovered", b"yes");
                    Ok(Response::new().add_attribute("action", "recovered"))
                }
            }
            Some("fail_after_read_write") => {
                let _ = context.storage_get(b"count");
                context.storage_set(b"partial", b"rolled_back");
                Err("child failure".to_owned())
            }
            Some("noop") => Ok(Response::new()),
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

struct RouterContract;

impl NativeContract for RouterContract {
    fn instantiate(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        _msg: Binary,
    ) -> Result<Response<Empty>, String> {
        context.storage_set(b"status", b"ready");
        Ok(Response::new())
    }

    fn execute(
        &self,
        _context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        msg: Binary,
    ) -> Result<Response<Empty>, String> {
        let message: Value =
            serde_json::from_slice(msg.as_slice()).map_err(|error| error.to_string())?;
        let target = message["target"]
            .as_str()
            .ok_or("missing target")?
            .to_owned();
        Ok(Response::new().add_submessage(SubMsg {
            id: 7,
            payload: Binary::default(),
            msg: WasmMsg::Execute {
                contract_addr: target,
                msg: to_json_binary(&json!({ "action": "fail_after_read_write" }))
                    .map_err(|error| error.to_string())?,
                funds: Vec::new(),
            }
            .into(),
            gas_limit: None,
            reply_on: ReplyOn::Error,
        }))
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
        context: &mut NativeCallContext,
        _env: Env,
        reply: Reply,
    ) -> Result<Response<Empty>, String> {
        if reply.id != 7 || reply.result.is_ok() {
            return Err("unexpected reply".to_owned());
        }
        context.storage_set(b"status", b"recovered");
        Ok(Response::new())
    }
}

fn read_u64(value: Option<Vec<u8>>) -> u64 {
    value
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
        .map(u64::from_be_bytes)
        .unwrap_or_default()
}

fn block(index: u32) -> BlockContext {
    BlockContext {
        transaction_index: Some(index),
        ..BlockContext::default()
    }
}

fn register_fixture(engine: &CosmWasmEngine) -> CodeId {
    engine
        .register_native("validation-fixture", Arc::new(ValidationContract))
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
            "fixture".to_owned(),
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

fn speculate(
    engine: &CosmWasmEngine,
    snapshot: &acg_cosmwasm_engine::StateSnapshot,
    index: u32,
    request: ExecutionRequest,
) -> SpeculativeTxResult {
    engine
        .execute_speculative(snapshot, block(index), request)
        .unwrap()
}

fn execute(engine: &CosmWasmEngine, index: u32, request: ExecutionRequest) -> ExecutionOutcome {
    engine.execute_request(block(index), request).unwrap()
}

#[test]
fn unchanged_point_read_validates_and_speculative_result_is_reused() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let req = request(
        10,
        &contract,
        json!({"action":"read_then_write","key":"count"}),
    );
    let receipt = speculate(&engine, &engine.snapshot(), 0, req.clone());

    assert!(engine.validate_speculative(&receipt).unwrap().is_valid());
    let outcome = engine
        .execute_canonical_with_speculation(vec![canonical(0, req)], vec![receipt])
        .unwrap();

    assert_eq!(
        outcome.transactions[0].disposition,
        CanonicalTxDisposition::ReusedSpeculative
    );
    assert_eq!(outcome.metrics.reused_results, 1);
    assert_eq!(outcome.metrics.replayed_transactions, 0);
    assert_eq!(
        engine.raw_storage(&contract, b"observed"),
        Some(0_u64.to_be_bytes().to_vec())
    );
}

#[test]
fn predecessor_point_change_invalidates_and_replays() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let snapshot = engine.snapshot();
    let read = request(
        11,
        &contract,
        json!({"action":"read_then_write","key":"count"}),
    );
    let receipt = speculate(&engine, &snapshot, 1, read.clone());
    let increment = request(10, &contract, json!({"action":"increment"}));

    let outcome = engine
        .execute_canonical_with_speculation(
            vec![canonical(0, increment), canonical(1, read)],
            vec![receipt],
        )
        .unwrap();

    assert_eq!(
        outcome.transactions[1].disposition,
        CanonicalTxDisposition::Replayed
    );
    assert_eq!(outcome.metrics.invalidated_results, 1);
    assert_eq!(outcome.metrics.replayed_transactions, 1);
    assert_eq!(
        engine.raw_storage(&contract, b"observed"),
        Some(1_u64.to_be_bytes().to_vec())
    );
}

#[test]
fn insertion_of_key_observed_missing_invalidates() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let req = request(
        10,
        &contract,
        json!({"action":"read_then_write","key":"missing"}),
    );
    let receipt = speculate(&engine, &engine.snapshot(), 0, req);

    execute(
        &engine,
        1,
        request(
            11,
            &contract,
            json!({"action":"set","key":"missing","value":"now"}),
        ),
    );
    let validation = engine.validate_speculative(&receipt).unwrap();
    assert!(!validation.is_valid());
    assert!(validation.conflicts().iter().any(|conflict| matches!(conflict, ValidationConflict::Storage { key, expected: None, actual: Some(_), .. } if key.as_slice() == b"missing")));
}

#[test]
fn deletion_of_observed_key_invalidates() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let req = request(
        10,
        &contract,
        json!({"action":"read_then_write","key":"item/a"}),
    );
    let receipt = speculate(&engine, &engine.snapshot(), 0, req);

    execute(
        &engine,
        1,
        request(11, &contract, json!({"action":"remove","key":"item/a"})),
    );
    assert!(!engine.validate_speculative(&receipt).unwrap().is_valid());
}

#[test]
fn transaction_local_point_write_masks_predecessor_change() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let req = request(10, &contract, json!({"action":"write_then_read"}));
    let receipt = speculate(&engine, &engine.snapshot(), 0, req);

    execute(
        &engine,
        1,
        request(
            11,
            &contract,
            json!({"action":"set","key":"local","value":"predecessor"}),
        ),
    );
    assert!(engine.validate_speculative(&receipt).unwrap().is_valid());
}

#[test]
fn range_value_change_invalidates() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let req = request(10, &contract, json!({"action":"range_count"}));
    let receipt = speculate(&engine, &engine.snapshot(), 0, req);

    execute(
        &engine,
        1,
        request(
            11,
            &contract,
            json!({"action":"set","key":"item/b","value":"changed"}),
        ),
    );
    assert!(!engine.validate_speculative(&receipt).unwrap().is_valid());
}

#[test]
fn range_insertion_phantom_invalidates() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let receipt = speculate(
        &engine,
        &engine.snapshot(),
        0,
        request(10, &contract, json!({"action":"range_count"})),
    );

    execute(
        &engine,
        1,
        request(
            11,
            &contract,
            json!({"action":"set","key":"item/c","value":"three"}),
        ),
    );
    assert!(!engine.validate_speculative(&receipt).unwrap().is_valid());
}

#[test]
fn range_deletion_phantom_invalidates() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let receipt = speculate(
        &engine,
        &engine.snapshot(),
        0,
        request(10, &contract, json!({"action":"range_count"})),
    );

    execute(
        &engine,
        1,
        request(11, &contract, json!({"action":"remove","key":"item/a"})),
    );
    assert!(!engine.validate_speculative(&receipt).unwrap().is_valid());
}

#[test]
fn transaction_local_range_mask_avoids_false_invalidation() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let receipt = speculate(
        &engine,
        &engine.snapshot(),
        0,
        request(10, &contract, json!({"action":"write_then_range"})),
    );

    execute(
        &engine,
        1,
        request(
            11,
            &contract,
            json!({"action":"set","key":"item/a","value":"predecessor"}),
        ),
    );
    assert!(engine.validate_speculative(&receipt).unwrap().is_valid());
}

#[test]
fn bank_balance_change_invalidates() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    engine
        .set_balance("bob", &[Coin::new(10_u128, "utest")])
        .unwrap();
    let receipt = speculate(
        &engine,
        &engine.snapshot(),
        0,
        request(
            10,
            &contract,
            json!({"action":"read_balance","owner":"bob","denom":"utest"}),
        ),
    );

    engine
        .set_balance("bob", &[Coin::new(11_u128, "utest")])
        .unwrap();
    assert!(!engine.validate_speculative(&receipt).unwrap().is_valid());
}

#[test]
fn all_balances_new_denomination_invalidates() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    engine
        .set_balance("bob", &[Coin::new(10_u128, "uatom")])
        .unwrap();
    let receipt = speculate(
        &engine,
        &engine.snapshot(),
        0,
        request(
            10,
            &contract,
            json!({"action":"observe_all_balances","owner":"bob"}),
        ),
    );

    engine
        .set_balance("bob", &[Coin::new(5_u128, "uusdc")])
        .unwrap();
    assert!(!engine.validate_speculative(&receipt).unwrap().is_valid());
}

#[test]
fn all_balances_masked_denomination_is_ignored_by_range_validation() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    engine
        .set_balance("bob", &[Coin::new(10_u128, "utest")])
        .unwrap();
    let mut receipt = speculate(
        &engine,
        &engine.snapshot(),
        0,
        request(
            10,
            &contract,
            json!({"action":"observe_all_balances","owner":"bob"}),
        ),
    );
    receipt.read_dependencies.retain_mut(|dependency| match dependency {
        ReadDependency::BankAllBalances {
            base_balances,
            masked_denoms,
            ..
        } => {
            base_balances.retain(|(denom, _)| denom != "utest");
            masked_denoms.push("utest".to_owned());
            true
        }
        ReadDependency::ContractMetadata { .. } => true,
        _ => false,
    });

    engine
        .set_balance("bob", &[Coin::new(99_u128, "utest")])
        .unwrap();
    assert!(engine.validate_speculative(&receipt).unwrap().is_valid());
}

#[test]
fn contract_metadata_change_invalidates_speculative_instantiation() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let instantiate = ExecutionRequest::Instantiate {
        transaction_id: TransactionId(50),
        sender: Address::from("alice"),
        code_id,
        admin: None,
        label: "speculative".to_owned(),
        funds: Vec::new(),
        msg: Binary::default(),
    };
    let receipt = speculate(&engine, &engine.snapshot(), 0, instantiate.clone());

    execute(&engine, 0, instantiate);
    assert!(!engine.validate_speculative(&receipt).unwrap().is_valid());
}

#[test]
fn valid_receipt_applies_storage_write_set() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let req = request(
        10,
        &contract,
        json!({"action":"set","key":"committed","value":"yes"}),
    );
    let receipt = speculate(&engine, &engine.snapshot(), 0, req.clone());

    engine
        .execute_canonical_with_speculation(vec![canonical(0, req)], vec![receipt])
        .unwrap();
    assert_eq!(
        engine.raw_storage(&contract, b"committed"),
        Some(b"yes".to_vec())
    );
}

#[test]
fn valid_receipt_applies_storage_deletion() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let req = request(10, &contract, json!({"action":"remove","key":"item/a"}));
    let receipt = speculate(&engine, &engine.snapshot(), 0, req.clone());

    engine
        .execute_canonical_with_speculation(vec![canonical(0, req)], vec![receipt])
        .unwrap();
    assert_eq!(engine.raw_storage(&contract, b"item/a"), None);
}

#[test]
fn valid_receipt_applies_bank_write_set() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    engine
        .set_balance(contract.clone(), &[Coin::new(10_u128, "utest")])
        .unwrap();
    let req = request(
        10,
        &contract,
        json!({"action":"send","to":"bob","denom":"utest","amount":3}),
    );
    let receipt = speculate(&engine, &engine.snapshot(), 0, req.clone());

    engine
        .execute_canonical_with_speculation(vec![canonical(0, req)], vec![receipt])
        .unwrap();
    assert_eq!(engine.balance(contract, "utest"), 7);
    assert_eq!(engine.balance("bob", "utest"), 3);
}

#[test]
fn valid_speculative_instantiation_commits_contract_metadata_and_storage() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let req = ExecutionRequest::Instantiate {
        transaction_id: TransactionId(60),
        sender: Address::from("alice"),
        code_id,
        admin: None,
        label: "created".to_owned(),
        funds: Vec::new(),
        msg: Binary::default(),
    };
    let expected_address = engine.predict_contract_address(TransactionId(60), 0);
    let receipt = speculate(&engine, &engine.snapshot(), 0, req.clone());
    assert!(engine.contract_metadata(&expected_address).is_none());

    let outcome = engine
        .execute_canonical_with_speculation(vec![canonical(0, req)], vec![receipt])
        .unwrap();
    assert_eq!(
        outcome.transactions[0].disposition,
        CanonicalTxDisposition::ReusedSpeculative
    );
    assert!(engine.contract_metadata(&expected_address).is_some());
    assert_eq!(
        engine.raw_storage(&expected_address, b"count"),
        Some(0_u64.to_be_bytes().to_vec())
    );
}

#[test]
fn unchanged_failed_speculative_result_is_reused_without_writes() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let req = request(10, &contract, json!({"action":"fail_if_count_zero"}));
    let receipt = speculate(&engine, &engine.snapshot(), 0, req.clone());
    assert!(!receipt.is_success());
    assert!(receipt.write_set.is_empty());

    let outcome = engine
        .execute_canonical_with_speculation(vec![canonical(0, req)], vec![receipt])
        .unwrap();
    assert_eq!(
        outcome.transactions[0].disposition,
        CanonicalTxDisposition::ReusedSpeculative
    );
    assert!(outcome.transactions[0].result.is_err());
    assert_eq!(engine.raw_storage(&contract, b"partial"), None);
}

#[test]
fn changed_dependency_replays_failed_receipt_and_result_can_change() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let snapshot = engine.snapshot();
    let fail = request(11, &contract, json!({"action":"fail_if_count_zero"}));
    let receipt = speculate(&engine, &snapshot, 1, fail.clone());
    assert!(!receipt.is_success());

    let outcome = engine
        .execute_canonical_with_speculation(
            vec![
                canonical(0, request(10, &contract, json!({"action":"increment"}))),
                canonical(1, fail),
            ],
            vec![receipt],
        )
        .unwrap();
    assert_eq!(
        outcome.transactions[1].disposition,
        CanonicalTxDisposition::Replayed
    );
    assert!(outcome.transactions[1].result.is_ok());
    assert_eq!(
        engine.raw_storage(&contract, b"recovered"),
        Some(b"yes".to_vec())
    );
}

#[test]
fn reverted_nested_child_read_still_invalidates_parent_receipt() {
    let engine = CosmWasmEngine::default();
    let child_code = register_fixture(&engine);
    let router_code = engine
        .register_native("validation-router", Arc::new(RouterContract))
        .unwrap();
    let child = instantiate_fixture(&engine, child_code);
    let router = engine
        .instantiate(
            TransactionId(2),
            block(0),
            Address::from("alice"),
            router_code,
            None,
            "router".to_owned(),
            Vec::new(),
            Binary::default(),
        )
        .unwrap()
        .contract;
    let receipt = speculate(
        &engine,
        &engine.snapshot(),
        0,
        request(10, &router, json!({"target": child.as_str()})),
    );
    assert!(receipt.is_success());

    execute(
        &engine,
        1,
        request(11, &child, json!({"action":"increment"})),
    );
    assert!(!engine.validate_speculative(&receipt).unwrap().is_valid());
}

#[test]
fn blind_write_remains_reusable_after_predecessor_write_to_same_key() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let req = request(
        11,
        &contract,
        json!({"action":"blind_write","key":"winner","value":"later"}),
    );
    let receipt = speculate(&engine, &engine.snapshot(), 1, req.clone());

    execute(
        &engine,
        0,
        request(
            10,
            &contract,
            json!({"action":"blind_write","key":"winner","value":"earlier"}),
        ),
    );
    assert!(engine.validate_speculative(&receipt).unwrap().is_valid());
    engine
        .execute_canonical_with_speculation(vec![canonical(1, req)], vec![receipt])
        .unwrap();
    assert_eq!(
        engine.raw_storage(&contract, b"winner"),
        Some(b"later".to_vec())
    );
}

#[test]
fn multiple_invalid_receipts_are_selectively_replayed() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let snapshot = engine.snapshot();
    let b = request(
        11,
        &contract,
        json!({"action":"read_then_write","key":"count"}),
    );
    let c = request(
        12,
        &contract,
        json!({"action":"read_then_write","key":"count"}),
    );
    let b_receipt = speculate(&engine, &snapshot, 1, b.clone());
    let c_receipt = speculate(&engine, &snapshot, 2, c.clone());

    let outcome = engine
        .execute_canonical_with_speculation(
            vec![
                canonical(0, request(10, &contract, json!({"action":"increment"}))),
                canonical(1, b),
                canonical(2, c),
            ],
            vec![b_receipt, c_receipt],
        )
        .unwrap();
    assert_eq!(outcome.metrics.invalidated_results, 2);
    assert_eq!(outcome.metrics.replayed_transactions, 2);
    assert_eq!(
        outcome.transactions[1].disposition,
        CanonicalTxDisposition::Replayed
    );
    assert_eq!(
        outcome.transactions[2].disposition,
        CanonicalTxDisposition::Replayed
    );
}

#[test]
fn exact_request_and_block_context_are_bound_to_receipt() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let req = request(10, &contract, json!({"action":"noop"}));
    let receipt = speculate(&engine, &engine.snapshot(), 0, req.clone());
    let mismatched = canonical(1, req);

    let error = engine
        .execute_canonical_with_speculation(vec![mismatched], vec![receipt])
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("different block context or request"));
}

#[test]
fn malformed_receipt_is_rejected_before_any_canonical_state_is_mutated() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let second = request(11, &contract, json!({"action":"noop"}));
    let receipt = speculate(&engine, &engine.snapshot(), 1, second.clone());

    let error = engine
        .execute_canonical_with_speculation(
            vec![
                canonical(
                    0,
                    request(
                        10,
                        &contract,
                        json!({"action":"set","key":"must_not_commit","value":"x"}),
                    ),
                ),
                canonical(2, second),
            ],
            vec![receipt],
        )
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("different block context or request"));
    assert_eq!(engine.raw_storage(&contract, b"must_not_commit"), None);
}

#[test]
fn exact_request_is_bound_to_receipt_even_when_transaction_id_matches() {
    let engine = CosmWasmEngine::default();
    let code_id = register_fixture(&engine);
    let contract = instantiate_fixture(&engine, code_id);
    let original = request(10, &contract, json!({"action":"noop"}));
    let receipt = speculate(&engine, &engine.snapshot(), 0, original);
    let changed = request(
        10,
        &contract,
        json!({"action":"set","key":"x","value":"changed"}),
    );

    let error = engine
        .execute_canonical_with_speculation(vec![canonical(0, changed)], vec![receipt])
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("different block context or request"));
}

#[test]
fn receipt_from_different_engine_is_rejected() {
    let first = CosmWasmEngine::default();
    let second = CosmWasmEngine::default();
    let code_id = register_fixture(&first);
    let contract = instantiate_fixture(&first, code_id);
    let receipt = speculate(
        &first,
        &first.snapshot(),
        0,
        request(10, &contract, json!({"action":"noop"})),
    );

    let error = second.validate_speculative(&receipt).unwrap_err();
    assert!(error.to_string().contains("different CosmWasm engine"));
}

#[test]
fn serial_and_speculative_validation_replay_produce_equivalent_canonical_results() {
    let serial = CosmWasmEngine::default();
    let speculative = CosmWasmEngine::default();
    let serial_code = register_fixture(&serial);
    let speculative_code = register_fixture(&speculative);
    let serial_contract = instantiate_fixture(&serial, serial_code);
    let speculative_contract = instantiate_fixture(&speculative, speculative_code);
    assert_eq!(serial_contract, speculative_contract);

    let serial_requests = vec![
        request(
            10,
            &serial_contract,
            json!({"action":"blind_write","key":"x","value":"A"}),
        ),
        request(
            11,
            &serial_contract,
            json!({"action":"set","key":"y","value":"B"}),
        ),
        request(
            12,
            &serial_contract,
            json!({"action":"read_then_write","key":"x"}),
        ),
    ];
    let speculative_requests = vec![
        request(
            10,
            &speculative_contract,
            json!({"action":"blind_write","key":"x","value":"A"}),
        ),
        request(
            11,
            &speculative_contract,
            json!({"action":"set","key":"y","value":"B"}),
        ),
        request(
            12,
            &speculative_contract,
            json!({"action":"read_then_write","key":"x"}),
        ),
    ];

    let serial_outcomes = serial_requests
        .into_iter()
        .enumerate()
        .map(|(index, request)| execute(&serial, index as u32, request))
        .collect::<Vec<_>>();

    let snapshot = speculative.snapshot();
    let receipt_a = speculate(&speculative, &snapshot, 0, speculative_requests[0].clone());
    let receipt_c = speculate(&speculative, &snapshot, 2, speculative_requests[2].clone());
    let block_outcome = speculative
        .execute_canonical_with_speculation(
            speculative_requests
                .into_iter()
                .enumerate()
                .map(|(index, request)| canonical(index as u32, request))
                .collect(),
            vec![receipt_a, receipt_c],
        )
        .unwrap();

    assert_eq!(
        block_outcome.transactions[0].disposition,
        CanonicalTxDisposition::ReusedSpeculative
    );
    assert_eq!(
        block_outcome.transactions[1].disposition,
        CanonicalTxDisposition::Canonical
    );
    assert_eq!(
        block_outcome.transactions[2].disposition,
        CanonicalTxDisposition::Replayed
    );
    assert_eq!(block_outcome.metrics.reused_results, 1);
    assert_eq!(block_outcome.metrics.invalidated_results, 1);
    assert_eq!(block_outcome.metrics.canonical_transactions, 1);

    for key in [b"x".as_slice(), b"y".as_slice(), b"observed".as_slice()] {
        assert_eq!(
            serial.raw_storage(&serial_contract, key),
            speculative.raw_storage(&speculative_contract, key)
        );
    }
    assert_eq!(
        serial.all_balances(serial_contract.clone()),
        speculative.all_balances(speculative_contract.clone())
    );
    assert_eq!(
        serial.contract_metadata(&serial_contract),
        speculative.contract_metadata(&speculative_contract)
    );
    assert!(serial.snapshot().same_world_state(&speculative.snapshot()));

    for (serial_result, speculative_result) in
        serial_outcomes.iter().zip(&block_outcome.transactions)
    {
        let speculative_result = speculative_result.result.as_ref().unwrap();
        assert_eq!(
            serial_result.transaction_id,
            speculative_result.transaction_id
        );
        assert_eq!(serial_result.contract, speculative_result.contract);
        assert_eq!(serial_result.events, speculative_result.events);
        assert_eq!(serial_result.data, speculative_result.data);
        assert_eq!(
            serial_result.created_contracts,
            speculative_result.created_contracts
        );
    }
}

#[test]
fn metrics_rates_are_well_defined() {
    let metrics = acg_cosmwasm_engine::SpeculativeExecutionMetrics {
        speculative_results: 4,
        reused_results: 3,
        invalidated_results: 1,
        replayed_transactions: 1,
        canonical_transactions: 2,
    };
    assert_eq!(metrics.reuse_rate(), 0.75);
    assert_eq!(metrics.validation_failure_rate(), 0.25);
    assert_eq!(metrics.replay_rate(6), 1.0 / 6.0);
    assert_eq!(
        acg_cosmwasm_engine::SpeculativeExecutionMetrics::default().reuse_rate(),
        0.0
    );
    assert_eq!(metrics.replay_rate(0), 0.0);
}
