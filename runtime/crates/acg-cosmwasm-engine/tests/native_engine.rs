use std::sync::Arc;

use acg_cosmwasm_engine::{
    Address, BlockContext, CodeId, CosmWasmEngine, EngineError, NativeCallContext, NativeContract,
    TransactionId,
};
use cosmwasm_std::{
    to_json_binary, BankMsg, Binary, Coin, Empty, Env, MessageInfo, Reply, ReplyOn, Response,
    SubMsg, WasmMsg,
};
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
        Ok(Response::new().add_attribute("action", "instantiate_counter"))
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
                Ok(Response::new().add_attribute("action", "increment"))
            }
            Some("fail_after_write") => {
                context.storage_set(b"partial", b"must_rollback");
                Err("intentional child failure".to_owned())
            }
            Some("return_data") => {
                Ok(Response::new().set_data(Binary::from(b"child-data".as_slice())))
            }
            other => Err(format!("unsupported counter action: {other:?}")),
        }
    }

    fn query(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        msg: Binary,
    ) -> Result<Binary, String> {
        let message: Value = serde_json::from_slice(msg.as_slice()).map_err(|e| e.to_string())?;
        match message.get("query").and_then(Value::as_str) {
            Some("count") => {
                let current = read_u64(context.storage_get(b"count"));
                to_json_binary(&json!({ "count": current })).map_err(|e| e.to_string())
            }
            Some("attempt_mutation") => {
                context.storage_set(b"illegal_query_write", b"must_not_commit");
                Ok(Binary::default())
            }
            Some("attempt_same_value_write") => {
                let current = context.storage_get(b"count").unwrap_or_default();
                context.storage_set(b"count", current);
                Ok(Binary::default())
            }
            other => Err(format!("unsupported counter query: {other:?}")),
        }
    }
}

struct RangeContract;

impl NativeContract for RangeContract {
    fn instantiate(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        _msg: Binary,
    ) -> Result<Response<Empty>, String> {
        context.storage_set(b"item/a", b"one");
        context.storage_set(b"item/b", b"two");
        Ok(Response::new())
    }

    fn execute(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        _msg: Binary,
    ) -> Result<Response<Empty>, String> {
        context.storage_remove(b"item/a");
        context.storage_set(b"item/b", b"updated");
        context.storage_set(b"item/c", b"three");
        Ok(Response::new())
    }

    fn query(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        _msg: Binary,
    ) -> Result<Binary, String> {
        let entries = context.storage_range(Some(b"item/"), Some(b"item0"));
        let printable: Vec<(String, String)> = entries
            .into_iter()
            .map(|(key, value)| {
                (
                    String::from_utf8_lossy(&key).into_owned(),
                    String::from_utf8_lossy(&value).into_owned(),
                )
            })
            .collect();
        to_json_binary(&printable).map_err(|e| e.to_string())
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
        let message: Value = serde_json::from_slice(msg.as_slice()).map_err(|e| e.to_string())?;
        let action = message.get("action").and_then(Value::as_str);
        match action {
            Some("forward") => {
                let target = message["target"]
                    .as_str()
                    .ok_or("missing target")?
                    .to_owned();
                Ok(Response::new().add_message(WasmMsg::Execute {
                    contract_addr: target,
                    msg: to_json_binary(&json!({ "action": "increment" }))
                        .map_err(|e| e.to_string())?,
                    funds: Vec::new(),
                }))
            }
            Some("send") => {
                let to = message["to"].as_str().ok_or("missing to")?.to_owned();
                let amount = message["amount"].as_u64().ok_or("missing amount")?;
                Ok(Response::new().add_message(BankMsg::Send {
                    to_address: to,
                    amount: vec![Coin::new(u128::from(amount), "utest")],
                }))
            }
            Some("unhandled_failure") => {
                context.storage_set(b"status", b"changed_before_child");
                let target = message["target"]
                    .as_str()
                    .ok_or("missing target")?
                    .to_owned();
                Ok(Response::new().add_message(WasmMsg::Execute {
                    contract_addr: target,
                    msg: to_json_binary(&json!({ "action": "fail_after_write" }))
                        .map_err(|e| e.to_string())?,
                    funds: Vec::new(),
                }))
            }
            Some("handled_failure") => {
                context.storage_set(b"status", b"waiting_for_reply");
                let target = message["target"]
                    .as_str()
                    .ok_or("missing target")?
                    .to_owned();
                Ok(Response::new().add_submessage(SubMsg {
                    id: 7,
                    payload: Binary::from(b"recover".as_slice()),
                    msg: WasmMsg::Execute {
                        contract_addr: target,
                        msg: to_json_binary(&json!({ "action": "fail_after_write" }))
                            .map_err(|e| e.to_string())?,
                        funds: Vec::new(),
                    }
                    .into(),
                    gas_limit: None,
                    reply_on: ReplyOn::Error,
                }))
            }
            Some("reply_without_data") => {
                let target = message["target"]
                    .as_str()
                    .ok_or("missing target")?
                    .to_owned();
                Ok(Response::new()
                    .set_data(Binary::from(b"parent-data".as_slice()))
                    .add_submessage(SubMsg {
                        id: 8,
                        payload: Binary::default(),
                        msg: WasmMsg::Execute {
                            contract_addr: target,
                            msg: to_json_binary(&json!({ "action": "return_data" }))
                                .map_err(|e| e.to_string())?,
                            funds: Vec::new(),
                        }
                        .into(),
                        gas_limit: None,
                        reply_on: ReplyOn::Success,
                    }))
            }
            other => Err(format!("unsupported router action: {other:?}")),
        }
    }

    fn query(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        _msg: Binary,
    ) -> Result<Binary, String> {
        let status = context.storage_get(b"status").unwrap_or_default();
        to_json_binary(&json!({ "status": String::from_utf8_lossy(&status) }))
            .map_err(|e| e.to_string())
    }

    fn reply(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        reply: Reply,
    ) -> Result<Response<Empty>, String> {
        match reply.id {
            7 if reply.result.is_err() => {
                context.storage_set(b"status", b"recovered");
                Ok(Response::new().add_attribute("action", "recovered"))
            }
            8 if reply.result.is_ok() => Ok(Response::new()),
            _ => Err("unexpected reply".to_owned()),
        }
    }
}

struct FactoryContract {
    child_code_id: CodeId,
}

impl NativeContract for FactoryContract {
    fn instantiate(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        _msg: Binary,
    ) -> Result<Response<Empty>, String> {
        context.storage_remove(b"child");
        Ok(Response::new())
    }

    fn execute(
        &self,
        _context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        msg: Binary,
    ) -> Result<Response<Empty>, String> {
        let message: Value = serde_json::from_slice(msg.as_slice()).map_err(|e| e.to_string())?;
        if message.get("action").and_then(Value::as_str) != Some("create_child") {
            return Err("unsupported factory action".to_owned());
        }
        Ok(Response::new().add_submessage(SubMsg {
            id: 9,
            payload: Binary::from(b"create-child".as_slice()),
            msg: WasmMsg::Instantiate {
                admin: None,
                code_id: self.child_code_id.0,
                msg: Binary::default(),
                funds: Vec::new(),
                label: "nested-counter".to_owned(),
            }
            .into(),
            gas_limit: None,
            reply_on: ReplyOn::Success,
        }))
    }

    fn query(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        _msg: Binary,
    ) -> Result<Binary, String> {
        let child = context.storage_get(b"child").unwrap_or_default();
        to_json_binary(&json!({ "child": String::from_utf8_lossy(&child) }))
            .map_err(|e| e.to_string())
    }

    #[allow(deprecated)]
    fn reply(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        reply: Reply,
    ) -> Result<Response<Empty>, String> {
        if reply.id != 9 {
            return Err("unexpected reply ID".to_owned());
        }
        let response = reply.result.into_result()?;
        let encoded = response
            .msg_responses
            .first()
            .map(|item| item.value.as_slice())
            .or_else(|| response.data.as_ref().map(Binary::as_slice))
            .ok_or("missing instantiate response")?;
        let address = decode_first_length_delimited(encoded)?;
        context.storage_set(b"child", address.as_bytes());
        Ok(Response::new()
            .add_attribute("action", "child_created")
            .set_data(Binary::from(b"factory-reply".as_slice())))
    }
}

fn read_u64(value: Option<Vec<u8>>) -> u64 {
    value
        .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
        .map(u64::from_be_bytes)
        .unwrap_or_default()
}

fn decode_first_length_delimited(bytes: &[u8]) -> Result<String, String> {
    if bytes.first().copied() != Some(0x0a) {
        return Err("instantiate response does not start with protobuf field 1".to_owned());
    }
    let (length, consumed) = decode_varint(&bytes[1..])?;
    let start = 1 + consumed;
    let length = usize::try_from(length).map_err(|_| "address length overflow")?;
    let end = start.checked_add(length).ok_or("address length overflow")?;
    let address = bytes
        .get(start..end)
        .ok_or("truncated instantiate response")?;
    String::from_utf8(address.to_vec()).map_err(|e| e.to_string())
}

fn decode_varint(bytes: &[u8]) -> Result<(u64, usize), String> {
    let mut value = 0_u64;
    for (index, byte) in bytes.iter().copied().enumerate().take(10) {
        value |= u64::from(byte & 0x7f) << (index * 7);
        if byte & 0x80 == 0 {
            return Ok((value, index + 1));
        }
    }
    Err("invalid protobuf varint".to_owned())
}

fn register_native(
    engine: &CosmWasmEngine,
    label: &str,
    contract: Arc<dyn NativeContract>,
) -> CodeId {
    engine.register_native(label, contract).unwrap()
}

fn instantiate(
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
            "test".to_owned(),
            funds,
            Binary::default(),
        )
        .unwrap()
        .contract
}

#[test]
fn commits_native_storage_and_queries_it() {
    let engine = CosmWasmEngine::default();
    let code_id = register_native(&engine, "counter-v1", Arc::new(CounterContract));
    let counter = instantiate(&engine, 1, "alice", code_id, vec![]);

    let outcome = engine
        .execute(
            TransactionId(2),
            BlockContext::default(),
            Address::from("alice"),
            counter.clone(),
            vec![],
            to_json_binary(&json!({ "action": "increment" })).unwrap(),
        )
        .unwrap();

    assert!(outcome.accesses.iter().any(|access| access.call_depth == 0));
    let query = engine
        .query(
            BlockContext::default(),
            counter,
            to_json_binary(&json!({ "query": "count" })).unwrap(),
        )
        .unwrap();
    let value: Value = serde_json::from_slice(query.data.as_slice()).unwrap();
    assert_eq!(value["count"], 1);
}

#[test]
fn range_queries_merge_committed_state_and_transaction_overlays() {
    let engine = CosmWasmEngine::default();
    let code_id = register_native(&engine, "range-v1", Arc::new(RangeContract));
    let contract = instantiate(&engine, 1, "alice", code_id, vec![]);

    engine
        .execute(
            TransactionId(2),
            BlockContext::default(),
            Address::from("alice"),
            contract.clone(),
            vec![],
            Binary::default(),
        )
        .unwrap();

    let query = engine
        .query(BlockContext::default(), contract, Binary::default())
        .unwrap();
    let entries: Vec<(String, String)> = serde_json::from_slice(query.data.as_slice()).unwrap();
    assert_eq!(
        entries,
        vec![
            ("item/b".to_owned(), "updated".to_owned()),
            ("item/c".to_owned(), "three".to_owned()),
        ]
    );
    assert!(query
        .accesses
        .iter()
        .any(|access| matches!(access.kind, acg_cosmwasm_engine::AccessKind::StorageScan)));
}

#[test]
fn rejects_and_reverts_query_side_writes() {
    let engine = CosmWasmEngine::default();
    let code_id = register_native(&engine, "counter-v1", Arc::new(CounterContract));
    let counter = instantiate(&engine, 1, "alice", code_id, vec![]);

    let error = engine
        .query(
            BlockContext::default(),
            counter.clone(),
            to_json_binary(&json!({ "query": "attempt_mutation" })).unwrap(),
        )
        .unwrap_err();

    assert!(matches!(error, EngineError::Contract(_)));
    assert_eq!(engine.raw_storage(&counter, b"illegal_query_write"), None);
}

#[test]
fn rejects_query_writes_even_when_the_value_is_unchanged() {
    let engine = CosmWasmEngine::default();
    let code_id = register_native(&engine, "counter-v1", Arc::new(CounterContract));
    let counter = instantiate(&engine, 1, "alice", code_id, vec![]);

    let error = engine
        .query(
            BlockContext::default(),
            counter.clone(),
            to_json_binary(&json!({ "query": "attempt_same_value_write" })).unwrap(),
        )
        .unwrap_err();

    assert!(matches!(error, EngineError::Contract(_)));
    assert_eq!(
        engine.raw_storage(&counter, b"count"),
        Some(0_u64.to_be_bytes().to_vec())
    );
}

#[test]
fn transfers_bank_funds_atomically() {
    let engine = CosmWasmEngine::default();
    engine
        .set_balance("alice", &[Coin::new(100_u128, "utest")])
        .unwrap();
    let router_code = register_native(&engine, "router-v1", Arc::new(RouterContract));
    let router = instantiate(
        &engine,
        1,
        "alice",
        router_code,
        vec![Coin::new(40_u128, "utest")],
    );

    engine
        .execute(
            TransactionId(2),
            BlockContext::default(),
            Address::from("alice"),
            router.clone(),
            vec![],
            to_json_binary(&json!({ "action": "send", "to": "bob", "amount": 15 })).unwrap(),
        )
        .unwrap();

    assert_eq!(engine.balance("alice", "utest"), 60);
    assert_eq!(engine.balance(router, "utest"), 25);
    assert_eq!(engine.balance("bob", "utest"), 15);
}

#[test]
fn self_transfer_does_not_mint_funds() {
    let engine = CosmWasmEngine::default();
    engine
        .set_balance("alice", &[Coin::new(100_u128, "utest")])
        .unwrap();
    let router_code = register_native(&engine, "router-v1", Arc::new(RouterContract));
    let router = instantiate(
        &engine,
        1,
        "alice",
        router_code,
        vec![Coin::new(40_u128, "utest")],
    );

    engine
        .execute(
            TransactionId(2),
            BlockContext::default(),
            Address::from("alice"),
            router.clone(),
            vec![],
            to_json_binary(&json!({
                "action": "send",
                "to": router.as_str(),
                "amount": 15
            }))
            .unwrap(),
        )
        .unwrap();

    assert_eq!(engine.balance(router, "utest"), 40);
}

#[test]
fn executes_nested_contract_messages() {
    let engine = CosmWasmEngine::default();
    let counter_code = register_native(&engine, "counter-v1", Arc::new(CounterContract));
    let router_code = register_native(&engine, "router-v1", Arc::new(RouterContract));
    let counter = instantiate(&engine, 1, "alice", counter_code, vec![]);
    let router = instantiate(&engine, 2, "alice", router_code, vec![]);

    let outcome = engine
        .execute(
            TransactionId(3),
            BlockContext::default(),
            Address::from("alice"),
            router,
            vec![],
            to_json_binary(&json!({ "action": "forward", "target": counter.as_str() })).unwrap(),
        )
        .unwrap();

    assert!(outcome.accesses.iter().any(|access| access.call_depth == 1));
    let query = engine
        .query(
            BlockContext::default(),
            counter,
            to_json_binary(&json!({ "query": "count" })).unwrap(),
        )
        .unwrap();
    let value: Value = serde_json::from_slice(query.data.as_slice()).unwrap();
    assert_eq!(value["count"], 1);
}

#[test]
fn reply_without_data_preserves_parent_response_data() {
    let engine = CosmWasmEngine::default();
    let counter_code = register_native(&engine, "counter-v1", Arc::new(CounterContract));
    let router_code = register_native(&engine, "router-v1", Arc::new(RouterContract));
    let counter = instantiate(&engine, 1, "alice", counter_code, vec![]);
    let router = instantiate(&engine, 2, "alice", router_code, vec![]);

    let outcome = engine
        .execute(
            TransactionId(3),
            BlockContext::default(),
            Address::from("alice"),
            router,
            vec![],
            to_json_binary(&json!({
                "action": "reply_without_data",
                "target": counter.as_str()
            }))
            .unwrap(),
        )
        .unwrap();

    assert_eq!(outcome.data, Some(Binary::from(b"parent-data".as_slice())));
}

#[test]
fn nested_instantiate_returns_address_through_reply() {
    let engine = CosmWasmEngine::default();
    let counter_code = register_native(&engine, "counter-v1", Arc::new(CounterContract));
    let factory_code = register_native(
        &engine,
        "factory-v1",
        Arc::new(FactoryContract {
            child_code_id: counter_code,
        }),
    );
    let factory = instantiate(&engine, 1, "alice", factory_code, vec![]);

    let outcome = engine
        .execute(
            TransactionId(2),
            BlockContext::default(),
            Address::from("alice"),
            factory.clone(),
            vec![],
            to_json_binary(&json!({ "action": "create_child" })).unwrap(),
        )
        .unwrap();

    assert_eq!(outcome.created_contracts.len(), 1);
    assert_eq!(
        outcome.data,
        Some(Binary::from(b"factory-reply".as_slice()))
    );
    let child = outcome.created_contracts[0].clone();
    let metadata = engine.contract_metadata(&child).unwrap();
    assert_eq!(metadata.code_id, counter_code);

    let query = engine
        .query(BlockContext::default(), factory, Binary::default())
        .unwrap();
    let value: Value = serde_json::from_slice(query.data.as_slice()).unwrap();
    assert_eq!(value["child"], child.as_str());
}

#[test]
fn unhandled_nested_failure_rolls_back_the_transaction() {
    let engine = CosmWasmEngine::default();
    let counter_code = register_native(&engine, "counter-v1", Arc::new(CounterContract));
    let router_code = register_native(&engine, "router-v1", Arc::new(RouterContract));
    let counter = instantiate(&engine, 1, "alice", counter_code, vec![]);
    let router = instantiate(&engine, 2, "alice", router_code, vec![]);

    let error = engine
        .execute(
            TransactionId(3),
            BlockContext::default(),
            Address::from("alice"),
            router.clone(),
            vec![],
            to_json_binary(&json!({
                "action": "unhandled_failure",
                "target": counter.as_str()
            }))
            .unwrap(),
        )
        .unwrap_err();
    assert!(matches!(error, EngineError::Native(_)));
    assert_eq!(
        engine.raw_storage(&router, b"status"),
        Some(b"ready".to_vec())
    );
    assert_eq!(engine.raw_storage(&counter, b"partial"), None);
}

#[test]
fn reply_on_error_keeps_parent_state_and_reverts_child_state() {
    let engine = CosmWasmEngine::default();
    let counter_code = register_native(&engine, "counter-v1", Arc::new(CounterContract));
    let router_code = register_native(&engine, "router-v1", Arc::new(RouterContract));
    let counter = instantiate(&engine, 1, "alice", counter_code, vec![]);
    let router = instantiate(&engine, 2, "alice", router_code, vec![]);

    let outcome = engine
        .execute(
            TransactionId(3),
            BlockContext::default(),
            Address::from("alice"),
            router.clone(),
            vec![],
            to_json_binary(&json!({
                "action": "handled_failure",
                "target": counter.as_str()
            }))
            .unwrap(),
        )
        .unwrap();

    assert_eq!(
        engine.raw_storage(&router, b"status"),
        Some(b"recovered".to_vec())
    );
    assert_eq!(engine.raw_storage(&counter, b"partial"), None);
    assert!(outcome.accesses.iter().any(|access| access.reverted));
}

#[test]
fn code_checksums_are_stable_and_part_of_contract_metadata() {
    let engine = CosmWasmEngine::default();
    let code_a = register_native(&engine, "counter-v1", Arc::new(CounterContract));
    let code_b = register_native(&engine, "counter-v1", Arc::new(CounterContract));
    let metadata_a = engine.code_metadata(code_a).unwrap();
    let metadata_b = engine.code_metadata(code_b).unwrap();
    assert_ne!(code_a, code_b);
    assert_eq!(metadata_a.checksum, metadata_b.checksum);

    let counter = instantiate(&engine, 1, "alice", code_a, vec![]);
    assert_eq!(
        engine.contract_metadata(&counter).unwrap().code_checksum,
        metadata_a.checksum
    );
}
