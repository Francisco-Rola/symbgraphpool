use std::sync::Arc;

use acg_cosmwasm_engine::{
    AccessKind, Address, BlockContext, CodeId, CosmWasmEngine, EngineError, ExecutionRequest,
    NativeCallContext, NativeContract, ReadDependency, SpeculativeExecutionStatus, StateSnapshot,
    TransactionId,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use cosmwasm_std::{
    to_json_binary, BankMsg, Binary, Coin, Empty, Env, MessageInfo, Reply, ReplyOn, Response,
    SubMsg, WasmMsg,
};
use serde_json::{json, Value};

const HACKATOM_BASE64: &str = include_str!("../testdata/hackatom_1.2.wasm.b64");

struct FixtureContract;

impl NativeContract for FixtureContract {
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
            Some("range_then_mutate") => {
                let entries = context.storage_range(Some(b"item/"), Some(b"item0"));
                context.storage_remove(b"item/a");
                context.storage_set(b"item/c", b"three");
                context.storage_set(b"seen", (entries.len() as u64).to_be_bytes());
                Ok(Response::new())
            }
            Some("write_then_read") => {
                context.storage_set(b"local", b"new");
                let value = context.storage_get(b"local").unwrap_or_default();
                context.storage_set(b"echo", value);
                Ok(Response::new())
            }
            Some("write_then_range") => {
                context.storage_set(b"item/a", b"local");
                let entries = context.storage_range(Some(b"item/"), Some(b"item0"));
                context.storage_set(b"seen", (entries.len() as u64).to_be_bytes());
                Ok(Response::new())
            }
            Some("send") => {
                let target = message["to"].as_str().ok_or("missing target")?.to_owned();
                let amount = message["amount"].as_u64().ok_or("missing amount")?;
                Ok(Response::new().add_message(BankMsg::Send {
                    to_address: target,
                    amount: vec![Coin::new(u128::from(amount), "utest")],
                }))
            }
            Some("observe_all_balances") => {
                let owner = Address::new(message["owner"].as_str().ok_or("missing owner")?);
                let balances = context.all_balances(&owner);
                context.storage_set(b"balance_count", (balances.len() as u64).to_be_bytes());
                Ok(Response::new())
            }
            Some("send_then_observe_all_balances") => {
                let target = Address::new(message["to"].as_str().ok_or("missing target")?);
                let amount = message["amount"].as_u64().ok_or("missing amount")?;
                context
                    .send(&target, &[Coin::new(u128::from(amount), "utest")])
                    .map_err(|error| error.to_string())?;
                let owner = context.contract.clone();
                let balances = context.all_balances(&owner);
                context.storage_set(b"balance_count", (balances.len() as u64).to_be_bytes());
                Ok(Response::new())
            }
            Some("fail_after_read_write") => {
                let _ = context.storage_get(b"count");
                context.storage_set(b"partial", b"must_rollback");
                Err("intentional failure".to_owned())
            }
            other => Err(format!("unsupported fixture action: {other:?}")),
        }
    }

    fn query(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        msg: Binary,
    ) -> Result<Binary, String> {
        let message: Value =
            serde_json::from_slice(msg.as_slice()).map_err(|error| error.to_string())?;
        match message.get("query").and_then(Value::as_str) {
            Some("count") => to_json_binary(&json!({
                "count": read_u64(context.storage_get(b"count"))
            }))
            .map_err(|error| error.to_string()),
            other => Err(format!("unsupported fixture query: {other:?}")),
        }
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
        context: &mut NativeCallContext,
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
        context.storage_set(b"status", b"waiting");
        Ok(Response::new().add_submessage(SubMsg {
            id: 17,
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
        context: &mut NativeCallContext,
        _env: Env,
        _msg: Binary,
    ) -> Result<Binary, String> {
        Ok(Binary::from(
            context.storage_get(b"status").unwrap_or_default(),
        ))
    }

    fn reply(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        reply: Reply,
    ) -> Result<Response<Empty>, String> {
        if reply.id != 17 || reply.result.is_ok() {
            return Err("unexpected reply".to_owned());
        }
        context.storage_set(b"status", b"recovered");
        Ok(Response::new().add_attribute("action", "recovered"))
    }
}

fn read_u64(value: Option<Vec<u8>>) -> u64 {
    value
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
        .map(u64::from_be_bytes)
        .unwrap_or_default()
}

fn register(engine: &CosmWasmEngine, label: &str, contract: Arc<dyn NativeContract>) -> CodeId {
    engine.register_native(label, contract).unwrap()
}

fn instantiate_native(
    engine: &CosmWasmEngine,
    tx: u64,
    sender: &str,
    code_id: CodeId,
    funds: Vec<Coin>,
) -> Address {
    engine
        .instantiate(
            TransactionId(tx),
            BlockContext::default(),
            Address::from(sender),
            code_id,
            None,
            "fixture".to_owned(),
            funds,
            Binary::default(),
        )
        .unwrap()
        .contract
}

fn speculative_execute(
    engine: &CosmWasmEngine,
    snapshot: &StateSnapshot,
    tx: u64,
    contract: Address,
    msg: Binary,
) -> acg_cosmwasm_engine::SpeculativeTxResult {
    engine
        .execute_speculative(
            snapshot,
            BlockContext::default(),
            ExecutionRequest::Execute {
                transaction_id: TransactionId(tx),
                sender: Address::from("alice"),
                contract,
                funds: Vec::new(),
                msg,
            },
        )
        .unwrap()
}

#[test]
fn speculative_storage_execution_is_detached_and_records_point_dependencies_and_writes() {
    let engine = CosmWasmEngine::default();
    let code_id = register(&engine, "spec-fixture", Arc::new(FixtureContract));
    let contract = instantiate_native(&engine, 1, "alice", code_id, Vec::new());
    let snapshot = engine.snapshot();

    let result = speculative_execute(
        &engine,
        &snapshot,
        2,
        contract.clone(),
        to_json_binary(&json!({ "action": "increment" })).unwrap(),
    );

    assert!(result.is_success());
    assert_eq!(
        engine.raw_storage(&contract, b"count"),
        Some(0_u64.to_be_bytes().to_vec())
    );
    assert!(result.read_dependencies.iter().any(|dependency| {
        matches!(
            dependency,
            ReadDependency::Storage { contract: dependency_contract, key, value }
                if dependency_contract == &contract
                    && key == b"count"
                    && value.as_deref() == Some(0_u64.to_be_bytes().as_slice())
        )
    }));
    assert!(result.write_set.storage.iter().any(|write| {
        write.contract == contract
            && write.key == b"count"
            && write.value.as_deref() == Some(1_u64.to_be_bytes().as_slice())
    }));
    assert!(result
        .accesses
        .iter()
        .any(|access| matches!(&access.kind, AccessKind::StorageRead)));
    assert!(result
        .accesses
        .iter()
        .any(|access| matches!(&access.kind, AccessKind::StorageWrite)));
}

#[test]
fn snapshot_remains_stable_after_canonical_state_changes() {
    let engine = CosmWasmEngine::default();
    let code_id = register(&engine, "snapshot-fixture", Arc::new(FixtureContract));
    let contract = instantiate_native(&engine, 1, "alice", code_id, Vec::new());
    let snapshot = engine.snapshot();

    for tx in [2_u64, 3] {
        engine
            .execute(
                TransactionId(tx),
                BlockContext::default(),
                Address::from("alice"),
                contract.clone(),
                Vec::new(),
                to_json_binary(&json!({ "action": "increment" })).unwrap(),
            )
            .unwrap();
    }
    assert_eq!(
        engine.raw_storage(&contract, b"count"),
        Some(2_u64.to_be_bytes().to_vec())
    );

    let result = speculative_execute(
        &engine,
        &snapshot,
        4,
        contract.clone(),
        to_json_binary(&json!({ "action": "increment" })).unwrap(),
    );
    assert!(result.write_set.storage.iter().any(|write| {
        write.contract == contract
            && write.key == b"count"
            && write.value.as_deref() == Some(1_u64.to_be_bytes().as_slice())
    }));
    assert_eq!(
        engine.raw_storage(&contract, b"count"),
        Some(2_u64.to_be_bytes().to_vec())
    );
}

#[test]
fn speculative_range_records_phantom_sensitive_dependency_and_delete_write() {
    let engine = CosmWasmEngine::default();
    let code_id = register(&engine, "range-fixture", Arc::new(FixtureContract));
    let contract = instantiate_native(&engine, 1, "alice", code_id, Vec::new());
    let snapshot = engine.snapshot();

    let result = speculative_execute(
        &engine,
        &snapshot,
        2,
        contract.clone(),
        to_json_binary(&json!({ "action": "range_then_mutate" })).unwrap(),
    );

    let range = result
        .read_dependencies
        .iter()
        .find_map(|dependency| match dependency {
            ReadDependency::StorageRange {
                contract: dependency_contract,
                start,
                end,
                base_entries,
                masked_keys,
            } if dependency_contract == &contract => Some((start, end, base_entries, masked_keys)),
            _ => None,
        })
        .expect("missing storage range dependency");
    assert_eq!(range.0.as_deref(), Some(b"item/".as_slice()));
    assert_eq!(range.1.as_deref(), Some(b"item0".as_slice()));
    assert_eq!(
        range.2.as_slice(),
        &[
            (b"item/a".to_vec(), b"one".to_vec()),
            (b"item/b".to_vec(), b"two".to_vec()),
        ]
    );
    assert!(range.3.is_empty());
    assert!(result.write_set.storage.iter().any(|write| {
        write.contract == contract && write.key == b"item/a" && write.value.is_none()
    }));
    assert!(result.write_set.storage.iter().any(|write| {
        write.contract == contract
            && write.key == b"item/c"
            && write.value.as_deref() == Some(b"three".as_slice())
    }));
    assert_eq!(
        engine.raw_storage(&contract, b"item/a"),
        Some(b"one".to_vec())
    );
    assert_eq!(engine.raw_storage(&contract, b"item/c"), None);
}

#[test]
fn transaction_local_write_masks_range_base_dependency() {
    let engine = CosmWasmEngine::default();
    let code_id = register(&engine, "range-mask-fixture", Arc::new(FixtureContract));
    let contract = instantiate_native(&engine, 1, "alice", code_id, Vec::new());
    let snapshot = engine.snapshot();

    let result = speculative_execute(
        &engine,
        &snapshot,
        2,
        contract.clone(),
        to_json_binary(&json!({ "action": "write_then_range" })).unwrap(),
    );

    let (base_entries, masked_keys) = result
        .read_dependencies
        .iter()
        .find_map(|dependency| match dependency {
            ReadDependency::StorageRange {
                contract: dependency_contract,
                base_entries,
                masked_keys,
                ..
            } if dependency_contract == &contract => Some((base_entries, masked_keys)),
            _ => None,
        })
        .expect("missing storage range dependency");
    assert_eq!(
        base_entries.as_slice(),
        &[(b"item/b".to_vec(), b"two".to_vec())]
    );
    assert_eq!(masked_keys.as_slice(), &[b"item/a".to_vec()]);
}

#[test]
fn transaction_local_write_masks_point_read_dependency() {
    let engine = CosmWasmEngine::default();
    let code_id = register(&engine, "overlay-fixture", Arc::new(FixtureContract));
    let contract = instantiate_native(&engine, 1, "alice", code_id, Vec::new());
    let snapshot = engine.snapshot();

    let result = speculative_execute(
        &engine,
        &snapshot,
        2,
        contract.clone(),
        to_json_binary(&json!({ "action": "write_then_read" })).unwrap(),
    );

    assert!(!result.read_dependencies.iter().any(|dependency| {
        matches!(
            dependency,
            ReadDependency::Storage { contract: dependency_contract, key, .. }
                if dependency_contract == &contract && key == b"local"
        )
    }));
    assert!(result.write_set.storage.iter().any(|write| {
        write.contract == contract
            && write.key == b"echo"
            && write.value.as_deref() == Some(b"new".as_slice())
    }));
}

#[test]
fn speculative_bank_transfer_is_detached_and_records_bank_dependencies() {
    let engine = CosmWasmEngine::default();
    engine
        .set_balance("alice", &[Coin::new(100_u128, "utest")])
        .unwrap();
    let code_id = register(&engine, "bank-fixture", Arc::new(FixtureContract));
    let contract = instantiate_native(
        &engine,
        1,
        "alice",
        code_id,
        vec![Coin::new(40_u128, "utest")],
    );
    let snapshot = engine.snapshot();

    let result = speculative_execute(
        &engine,
        &snapshot,
        2,
        contract.clone(),
        to_json_binary(&json!({ "action": "send", "to": "bob", "amount": 15 })).unwrap(),
    );

    assert_eq!(engine.balance(contract.clone(), "utest"), 40);
    assert_eq!(engine.balance("bob", "utest"), 0);
    assert!(result.read_dependencies.iter().any(|dependency| {
        matches!(
            dependency,
            ReadDependency::BankBalance { address, denom, amount }
                if address == &contract && denom == "utest" && *amount == 40
        )
    }));
    assert!(result.read_dependencies.iter().any(|dependency| {
        matches!(
            dependency,
            ReadDependency::BankBalance { address, denom, amount }
                if address.as_str() == "bob" && denom == "utest" && *amount == 0
        )
    }));
    assert!(result.write_set.balances.iter().any(|write| {
        write.address == contract && write.denom == "utest" && write.amount == 25
    }));
    assert!(result.write_set.balances.iter().any(|write| {
        write.address.as_str() == "bob" && write.denom == "utest" && write.amount == 15
    }));
}

#[test]
fn all_balances_dependency_captures_denom_phantoms() {
    let engine = CosmWasmEngine::default();
    let code_id = register(&engine, "all-balances-fixture", Arc::new(FixtureContract));
    let contract = instantiate_native(&engine, 1, "alice", code_id, Vec::new());
    engine
        .set_balance(
            contract.clone(),
            &[Coin::new(7_u128, "ufoo"), Coin::new(40_u128, "utest")],
        )
        .unwrap();
    let snapshot = engine.snapshot();

    let result = speculative_execute(
        &engine,
        &snapshot,
        2,
        contract.clone(),
        to_json_binary(&json!({
            "action": "observe_all_balances",
            "owner": contract.as_str()
        }))
        .unwrap(),
    );

    let dependency = result
        .read_dependencies
        .iter()
        .find_map(|dependency| match dependency {
            ReadDependency::BankAllBalances {
                address,
                base_balances,
                masked_denoms,
            } if address == &contract => Some((base_balances, masked_denoms)),
            _ => None,
        })
        .expect("missing all-balances dependency");
    assert_eq!(
        dependency.0.as_slice(),
        &[("ufoo".to_owned(), 7), ("utest".to_owned(), 40)]
    );
    assert!(dependency.1.is_empty());
    assert_eq!(engine.raw_storage(&contract, b"balance_count"), None);
}

#[test]
fn local_bank_write_masks_all_balances_base_dependency() {
    let engine = CosmWasmEngine::default();
    let code_id = register(&engine, "bank-mask-fixture", Arc::new(FixtureContract));
    let contract = instantiate_native(&engine, 1, "alice", code_id, Vec::new());
    engine
        .set_balance(
            contract.clone(),
            &[Coin::new(7_u128, "ufoo"), Coin::new(40_u128, "utest")],
        )
        .unwrap();
    let snapshot = engine.snapshot();

    let result = speculative_execute(
        &engine,
        &snapshot,
        2,
        contract.clone(),
        to_json_binary(&json!({
            "action": "send_then_observe_all_balances",
            "to": "bob",
            "amount": 15
        }))
        .unwrap(),
    );

    let (base_balances, masked_denoms) = result
        .read_dependencies
        .iter()
        .find_map(|dependency| match dependency {
            ReadDependency::BankAllBalances {
                address,
                base_balances,
                masked_denoms,
            } if address == &contract => Some((base_balances, masked_denoms)),
            _ => None,
        })
        .expect("missing all-balances dependency");
    assert_eq!(base_balances.as_slice(), &[("ufoo".to_owned(), 7)]);
    assert_eq!(masked_denoms.as_slice(), &["utest".to_owned()]);
    assert_eq!(engine.balance(contract, "utest"), 40);
    assert_eq!(engine.balance("bob", "utest"), 0);
}

#[test]
fn handled_reverted_child_keeps_read_dependencies_but_discards_child_writes() {
    let engine = CosmWasmEngine::default();
    let fixture_code = register(&engine, "child-fixture", Arc::new(FixtureContract));
    let router_code = register(&engine, "router-fixture", Arc::new(RouterContract));
    let child = instantiate_native(&engine, 1, "alice", fixture_code, Vec::new());
    let router = instantiate_native(&engine, 2, "alice", router_code, Vec::new());
    let snapshot = engine.snapshot();

    let result = speculative_execute(
        &engine,
        &snapshot,
        3,
        router.clone(),
        to_json_binary(&json!({ "target": child.as_str() })).unwrap(),
    );

    assert!(result.is_success());
    assert!(result.read_dependencies.iter().any(|dependency| {
        matches!(
            dependency,
            ReadDependency::Storage { contract, key, value }
                if contract == &child
                    && key == b"count"
                    && value.as_deref() == Some(0_u64.to_be_bytes().as_slice())
        )
    }));
    assert!(result.accesses.iter().any(|access| {
        access.contract == child
            && access.key == b"partial"
            && matches!(&access.kind, AccessKind::StorageWrite)
            && access.reverted
    }));
    assert!(!result
        .write_set
        .storage
        .iter()
        .any(|write| write.contract == child && write.key == b"partial"));
    assert!(result.write_set.storage.iter().any(|write| {
        write.contract == router
            && write.key == b"status"
            && write.value.as_deref() == Some(b"recovered".as_slice())
    }));
    assert_eq!(engine.raw_storage(&child, b"partial"), None);
    assert_eq!(
        engine.raw_storage(&router, b"status"),
        Some(b"ready".to_vec())
    );
}

#[test]
fn failed_top_level_speculation_keeps_dependencies_but_has_no_commit_write_set() {
    let engine = CosmWasmEngine::default();
    let code_id = register(&engine, "failure-fixture", Arc::new(FixtureContract));
    let contract = instantiate_native(&engine, 1, "alice", code_id, Vec::new());
    let snapshot = engine.snapshot();

    let result = speculative_execute(
        &engine,
        &snapshot,
        2,
        contract.clone(),
        to_json_binary(&json!({ "action": "fail_after_read_write" })).unwrap(),
    );

    assert!(matches!(
        &result.status,
        SpeculativeExecutionStatus::Failed(EngineError::Native(_))
    ));
    assert!(result.write_set.is_empty());
    assert!(!result.accesses.is_empty());
    assert!(result.accesses.iter().all(|access| access.reverted));
    assert!(result.read_dependencies.iter().any(|dependency| {
        matches!(
            dependency,
            ReadDependency::Storage { contract: dependency_contract, key, .. }
                if dependency_contract == &contract && key == b"count"
        )
    }));
    assert_eq!(engine.raw_storage(&contract, b"partial"), None);
}

#[test]
fn speculative_instantiate_captures_created_contract_without_registering_it() {
    let engine = CosmWasmEngine::default();
    let code_id = register(&engine, "instantiate-fixture", Arc::new(FixtureContract));
    let snapshot = engine.snapshot();
    let transaction_id = TransactionId(41);
    let predicted = engine.predict_contract_address(transaction_id, 0);

    let result = engine
        .execute_speculative(
            &snapshot,
            BlockContext::default(),
            ExecutionRequest::Instantiate {
                transaction_id,
                sender: Address::from("alice"),
                code_id,
                admin: None,
                label: "speculative".to_owned(),
                funds: Vec::new(),
                msg: Binary::default(),
            },
        )
        .unwrap();

    assert!(result.is_success());
    assert_eq!(engine.contract_metadata(&predicted), None);
    assert_eq!(result.write_set.created_contracts.len(), 1);
    assert_eq!(result.write_set.created_contracts[0].address, predicted);
    assert!(result.write_set.storage.iter().any(|write| {
        write.contract == predicted
            && write.key == b"count"
            && write.value.as_deref() == Some(0_u64.to_be_bytes().as_slice())
    }));
    assert!(result.read_dependencies.iter().any(|dependency| {
        matches!(
            dependency,
            ReadDependency::ContractMetadata { address, metadata: None }
                if address == &predicted
        )
    }));
}

#[test]
fn rejects_snapshot_from_a_different_engine() {
    let engine_a = CosmWasmEngine::default();
    let engine_b = CosmWasmEngine::default();
    let snapshot = engine_a.snapshot();

    let error = engine_b
        .execute_speculative(
            &snapshot,
            BlockContext::default(),
            ExecutionRequest::Execute {
                transaction_id: TransactionId(1),
                sender: Address::from("alice"),
                contract: Address::from("contract-x"),
                funds: Vec::new(),
                msg: Binary::default(),
            },
        )
        .unwrap_err();

    assert!(matches!(error, EngineError::InvalidConfiguration(_)));
}

#[test]
fn speculative_real_wasm_execution_does_not_commit_bank_send() {
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
            "spec-hackatom".to_owned(),
            vec![Coin::new(100_u128, "utest")],
            to_json_binary(&json!({
                "verifier": "verifier",
                "beneficiary": "beneficiary"
            }))
            .unwrap(),
        )
        .unwrap()
        .contract;
    let snapshot = engine.snapshot();

    let result = engine
        .execute_speculative(
            &snapshot,
            BlockContext::default(),
            ExecutionRequest::Execute {
                transaction_id: TransactionId(2),
                sender: Address::from("verifier"),
                contract: contract.clone(),
                funds: Vec::new(),
                msg: Binary::from(br#"{"release":{}}"#.as_slice()),
            },
        )
        .unwrap();

    assert!(result.is_success());
    assert_eq!(engine.balance(contract.clone(), "utest"), 100);
    assert_eq!(engine.balance("beneficiary", "utest"), 0);
    assert!(result
        .write_set
        .balances
        .iter()
        .any(|write| { write.address == contract && write.denom == "utest" && write.amount == 0 }));
    assert!(result.write_set.balances.iter().any(|write| {
        write.address.as_str() == "beneficiary" && write.denom == "utest" && write.amount == 100
    }));
}
