use std::sync::Arc;

use acg_cosmwasm_engine::{
    Address, BlockContext, BundleCall, CanonicalTransaction, CosmWasmEngine, EngineConfig,
    ExecutionRequest, NativeCallContext, NativeContract, ScopedBundleCall, TransactionId,
};
use cosmwasm_std::{to_json_binary, Binary, Empty, Env, MessageInfo, Reply, Response};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ExecuteMsg {
    Set { key: String, value: String },
    Fail {},
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum QueryMsg {
    Value { key: String },
}

struct Fixture;

impl NativeContract for Fixture {
    fn instantiate(
        &self,
        _context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        _msg: Binary,
    ) -> Result<Response<Empty>, String> {
        Ok(Response::new())
    }

    fn execute(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        msg: Binary,
    ) -> Result<Response<Empty>, String> {
        match serde_json::from_slice::<ExecuteMsg>(msg.as_slice()).map_err(|e| e.to_string())? {
            ExecuteMsg::Set { key, value } => {
                context.storage_set(key.into_bytes(), value.into_bytes());
                Ok(Response::new())
            }
            ExecuteMsg::Fail {} => {
                let _ = context.storage_get(b"failure-guard");
                Err("fixture failure".to_owned())
            }
        }
    }

    fn query(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        msg: Binary,
    ) -> Result<Binary, String> {
        let QueryMsg::Value { key } =
            serde_json::from_slice(msg.as_slice()).map_err(|e| e.to_string())?;
        to_json_binary(&context.storage_get(key.as_bytes())).map_err(|e| e.to_string())
    }

    fn reply(
        &self,
        _context: &mut NativeCallContext,
        _env: Env,
        _reply: Reply,
    ) -> Result<Response<Empty>, String> {
        Err("no replies".to_owned())
    }
}

fn setup() -> (CosmWasmEngine, Address) {
    let engine = CosmWasmEngine::new(EngineConfig::default());
    let code = engine
        .register_native("bundle-fixture", Arc::new(Fixture))
        .unwrap();
    let contract = engine
        .instantiate(
            TransactionId(1),
            BlockContext::default(),
            Address::new("creator"),
            code,
            None,
            "fixture".to_owned(),
            vec![],
            Binary::from(b"{}".to_vec()),
        )
        .unwrap()
        .contract;
    (engine, contract)
}

#[test]
fn bundle_query_observes_prior_write_and_success_commits_once() {
    let (engine, contract) = setup();
    let outcome = engine
        .execute_bundle(
            TransactionId(2),
            BlockContext::default(),
            &[
                BundleCall::Execute {
                    sender: Address::new("alice"),
                    contract: contract.clone(),
                    funds: vec![],
                    msg: to_json_binary(&ExecuteMsg::Set {
                        key: "k".to_owned(),
                        value: "v".to_owned(),
                    })
                    .unwrap(),
                },
                BundleCall::Query {
                    contract: contract.clone(),
                    msg: to_json_binary(&QueryMsg::Value {
                        key: "k".to_owned(),
                    })
                    .unwrap(),
                },
            ],
        )
        .unwrap();
    assert!(outcome.committed);
    assert!(outcome.failure.is_none());
    assert!(outcome.reverted_scopes.is_empty());
    assert_eq!(outcome.query_results.len(), 1);
    let observed: Option<Vec<u8>> =
        serde_json::from_slice(outcome.query_results[0].data.as_slice()).unwrap();
    assert_eq!(observed, Some(b"v".to_vec()));
    assert_eq!(engine.raw_storage(&contract, b"k"), Some(b"v".to_vec()));
    assert!(outcome
        .accesses
        .iter()
        .all(|a| a.transaction_id == TransactionId(2)));
    assert_eq!(outcome.call_access_spans.len(), 2);
    assert_eq!(outcome.call_access_spans[0].call_index, 0);
    assert_eq!(outcome.call_access_spans[1].call_index, 1);
    assert!(outcome.call_access_spans[0].access_end > outcome.call_access_spans[0].access_start);
    assert!(outcome.call_access_spans[1].access_end > outcome.call_access_spans[1].access_start);
    assert!(outcome.call_access_spans[0].access_end <= outcome.call_access_spans[1].access_start);
}

#[test]
fn bundle_failure_rolls_back_earlier_calls() {
    let (engine, contract) = setup();
    let result = engine.execute_bundle(
        TransactionId(3),
        BlockContext::default(),
        &[
            BundleCall::Execute {
                sender: Address::new("alice"),
                contract: contract.clone(),
                funds: vec![],
                msg: to_json_binary(&ExecuteMsg::Set {
                    key: "k".to_owned(),
                    value: "v".to_owned(),
                })
                .unwrap(),
            },
            BundleCall::Execute {
                sender: Address::new("alice"),
                contract: contract.clone(),
                funds: vec![],
                msg: to_json_binary(&ExecuteMsg::Fail {}).unwrap(),
            },
        ],
    );
    assert!(result.is_err());
    assert_eq!(engine.raw_storage(&contract, b"k"), None);
}

#[test]
fn reverted_bundle_exposes_accesses_without_committing() {
    let (engine, contract) = setup();
    let outcome = engine
        .execute_bundle_reverted(
            TransactionId(4),
            BlockContext::default(),
            &[BundleCall::Execute {
                sender: Address::new("alice"),
                contract: contract.clone(),
                funds: vec![],
                msg: to_json_binary(&ExecuteMsg::Set {
                    key: "k".to_owned(),
                    value: "v".to_owned(),
                })
                .unwrap(),
            }],
        )
        .unwrap();
    assert!(!outcome.committed);
    assert!(outcome.failure.is_none());
    assert!(outcome.reverted_scopes.is_empty());
    assert!(!outcome.accesses.is_empty());
    assert!(outcome.accesses.iter().all(|a| a.reverted));
    assert_eq!(engine.raw_storage(&contract, b"k"), None);
}

#[test]
fn tolerant_reverted_bundle_stops_on_native_error_and_keeps_failure_accesses() {
    let (engine, contract) = setup();
    let outcome = engine
        .execute_bundle_reverted_tolerant(
            TransactionId(5),
            BlockContext::default(),
            &[
                BundleCall::Execute {
                    sender: Address::new("alice"),
                    contract: contract.clone(),
                    funds: vec![],
                    msg: to_json_binary(&ExecuteMsg::Set {
                        key: "before".to_owned(),
                        value: "v".to_owned(),
                    })
                    .unwrap(),
                },
                BundleCall::Execute {
                    sender: Address::new("alice"),
                    contract: contract.clone(),
                    funds: vec![],
                    msg: to_json_binary(&ExecuteMsg::Fail {}).unwrap(),
                },
                BundleCall::Execute {
                    sender: Address::new("alice"),
                    contract: contract.clone(),
                    funds: vec![],
                    msg: to_json_binary(&ExecuteMsg::Set {
                        key: "after".to_owned(),
                        value: "should-not-run".to_owned(),
                    })
                    .unwrap(),
                },
            ],
        )
        .unwrap();

    assert!(!outcome.committed);
    let failure = outcome
        .failure
        .as_ref()
        .expect("native failure must be retained");
    assert_eq!(failure.call_index, 1);
    assert!(failure.error.contains("fixture failure"));
    assert!(outcome.accesses.iter().all(|a| a.reverted));
    assert!(outcome.accesses.iter().any(|a| a.key == b"failure-guard"));
    assert!(!outcome.accesses.iter().any(|a| a.key == b"after"));
    assert_eq!(outcome.call_access_spans.len(), 2);
    assert_eq!(outcome.call_access_spans[0].call_index, 0);
    assert_eq!(outcome.call_access_spans[1].call_index, 1);
    assert_eq!(engine.raw_storage(&contract, b"before"), None);
    assert_eq!(engine.raw_storage(&contract, b"after"), None);
}

#[test]
fn strict_bundle_error_reports_failing_call_index() {
    let (engine, contract) = setup();
    let error = engine
        .execute_bundle(
            TransactionId(6),
            BlockContext::default(),
            &[
                BundleCall::Execute {
                    sender: Address::new("alice"),
                    contract: contract.clone(),
                    funds: vec![],
                    msg: to_json_binary(&ExecuteMsg::Set {
                        key: "before".to_owned(),
                        value: "v".to_owned(),
                    })
                    .unwrap(),
                },
                BundleCall::Execute {
                    sender: Address::new("alice"),
                    contract,
                    funds: vec![],
                    msg: to_json_binary(&ExecuteMsg::Fail {}).unwrap(),
                },
            ],
        )
        .unwrap_err();

    match error {
        acg_cosmwasm_engine::EngineError::BundleCallFailed { call_index, error } => {
            assert_eq!(call_index, 1);
            assert!(error.contains("fixture failure"));
        }
        other => panic!("expected BundleCallFailed, got {other:?}"),
    }
}

#[test]
fn successful_outer_bundle_discards_caught_internal_revert_scope() {
    let (engine, contract) = setup();
    let outcome = engine
        .execute_bundle_with_reverted_scopes(
            TransactionId(7),
            BlockContext::default(),
            &[
                ScopedBundleCall {
                    source_revert_scope: None,
                    call: BundleCall::Execute {
                        sender: Address::new("alice"),
                        contract: contract.clone(),
                        funds: vec![],
                        msg: to_json_binary(&ExecuteMsg::Set {
                            key: "outer_before".to_owned(),
                            value: "committed".to_owned(),
                        })
                        .unwrap(),
                    },
                },
                ScopedBundleCall {
                    source_revert_scope: Some(11),
                    call: BundleCall::Execute {
                        sender: Address::new("alice"),
                        contract: contract.clone(),
                        funds: vec![],
                        msg: to_json_binary(&ExecuteMsg::Set {
                            key: "inner".to_owned(),
                            value: "rolled-back".to_owned(),
                        })
                        .unwrap(),
                    },
                },
                ScopedBundleCall {
                    source_revert_scope: Some(11),
                    call: BundleCall::Execute {
                        sender: Address::new("alice"),
                        contract: contract.clone(),
                        funds: vec![],
                        msg: to_json_binary(&ExecuteMsg::Fail {}).unwrap(),
                    },
                },
                ScopedBundleCall {
                    source_revert_scope: None,
                    call: BundleCall::Execute {
                        sender: Address::new("alice"),
                        contract: contract.clone(),
                        funds: vec![],
                        msg: to_json_binary(&ExecuteMsg::Set {
                            key: "outer_after".to_owned(),
                            value: "committed".to_owned(),
                        })
                        .unwrap(),
                    },
                },
            ],
        )
        .unwrap();

    assert!(outcome.committed);
    assert!(outcome.failure.is_none());
    assert_eq!(outcome.reverted_scopes.len(), 1);
    let scope = &outcome.reverted_scopes[0];
    assert_eq!(scope.scope_id, 11);
    assert_eq!(scope.first_call_index, 1);
    assert_eq!(scope.last_call_index, 2);
    assert_eq!(scope.failure.as_ref().map(|f| f.call_index), Some(2));

    assert_eq!(
        engine.raw_storage(&contract, b"outer_before"),
        Some(b"committed".to_vec())
    );
    assert_eq!(engine.raw_storage(&contract, b"inner"), None);
    assert_eq!(
        engine.raw_storage(&contract, b"outer_after"),
        Some(b"committed".to_vec())
    );

    let inner = outcome
        .accesses
        .iter()
        .find(|a| a.key == b"inner")
        .expect("inner scope access retained");
    assert!(inner.reverted);
    assert!(outcome
        .accesses
        .iter()
        .any(|a| a.key == b"outer_after" && !a.reverted));
    assert_eq!(
        outcome
            .call_access_spans
            .iter()
            .map(|s| s.call_index)
            .collect::<Vec<_>>(),
        vec![0, 1, 2, 3]
    );
    for span in &outcome.call_access_spans {
        assert!(span.access_start <= span.access_end);
        assert!(span.access_end <= outcome.accesses.len());
    }
}

#[test]
fn reverted_scope_observes_prior_outer_write_without_committing_inner_write() {
    let (engine, contract) = setup();
    let outcome = engine
        .execute_bundle_with_reverted_scopes(
            TransactionId(8),
            BlockContext::default(),
            &[
                ScopedBundleCall {
                    source_revert_scope: None,
                    call: BundleCall::Execute {
                        sender: Address::new("alice"),
                        contract: contract.clone(),
                        funds: vec![],
                        msg: to_json_binary(&ExecuteMsg::Set {
                            key: "visible".to_owned(),
                            value: "outer".to_owned(),
                        })
                        .unwrap(),
                    },
                },
                ScopedBundleCall {
                    source_revert_scope: Some(22),
                    call: BundleCall::Query {
                        contract: contract.clone(),
                        msg: to_json_binary(&QueryMsg::Value {
                            key: "visible".to_owned(),
                        })
                        .unwrap(),
                    },
                },
                ScopedBundleCall {
                    source_revert_scope: Some(22),
                    call: BundleCall::Execute {
                        sender: Address::new("alice"),
                        contract: contract.clone(),
                        funds: vec![],
                        msg: to_json_binary(&ExecuteMsg::Set {
                            key: "discard".to_owned(),
                            value: "inner".to_owned(),
                        })
                        .unwrap(),
                    },
                },
            ],
        )
        .unwrap();

    assert_eq!(outcome.reverted_scopes.len(), 1);
    assert!(outcome.reverted_scopes[0].failure.is_none());
    assert_eq!(
        engine.raw_storage(&contract, b"visible"),
        Some(b"outer".to_vec())
    );
    assert_eq!(engine.raw_storage(&contract, b"discard"), None);
    assert!(outcome
        .accesses
        .iter()
        .any(|a| a.key == b"visible" && a.reverted));
    assert!(outcome
        .accesses
        .iter()
        .any(|a| a.key == b"discard" && a.reverted));
}

#[test]
fn bundle_request_speculation_reuses_successful_atomic_write_set() {
    let (engine, contract) = setup();
    let block = BlockContext::default();
    let request = ExecutionRequest::Bundle {
        transaction_id: TransactionId(100),
        calls: vec![ScopedBundleCall {
            source_revert_scope: None,
            call: BundleCall::Execute {
                sender: Address::new("alice"),
                contract: contract.clone(),
                funds: vec![],
                msg: to_json_binary(&ExecuteMsg::Set {
                    key: "speculative".to_owned(),
                    value: "committed".to_owned(),
                })
                .unwrap(),
            },
        }],
        source_failed: false,
    };

    let snapshot = engine.snapshot();
    let receipt = engine
        .execute_speculative(&snapshot, block.clone(), request.clone())
        .unwrap();
    assert!(receipt.is_success());
    assert!(!receipt.write_set.is_empty());
    assert_eq!(engine.raw_storage(&contract, b"speculative"), None);

    let outcome = engine
        .execute_canonical_with_speculation(
            vec![CanonicalTransaction::new(block, request)],
            vec![receipt],
        )
        .unwrap();
    assert_eq!(outcome.metrics.reused_results, 1);
    assert_eq!(outcome.metrics.replayed_transactions, 0);
    assert_eq!(
        engine.raw_storage(&contract, b"speculative"),
        Some(b"committed".to_vec())
    );
}

#[test]
fn source_failed_bundle_request_has_no_commit_ready_speculative_writes() {
    let (engine, contract) = setup();
    let block = BlockContext::default();
    let request = ExecutionRequest::Bundle {
        transaction_id: TransactionId(101),
        calls: vec![
            ScopedBundleCall {
                source_revert_scope: None,
                call: BundleCall::Execute {
                    sender: Address::new("alice"),
                    contract: contract.clone(),
                    funds: vec![],
                    msg: to_json_binary(&ExecuteMsg::Set {
                        key: "before_failure".to_owned(),
                        value: "rolled-back".to_owned(),
                    })
                    .unwrap(),
                },
            },
            ScopedBundleCall {
                source_revert_scope: None,
                call: BundleCall::Execute {
                    sender: Address::new("alice"),
                    contract: contract.clone(),
                    funds: vec![],
                    msg: to_json_binary(&ExecuteMsg::Fail {}).unwrap(),
                },
            },
        ],
        source_failed: true,
    };

    let receipt = engine
        .execute_speculative(&engine.snapshot(), block.clone(), request.clone())
        .unwrap();
    assert!(receipt.is_success());
    assert!(receipt.write_set.is_empty());
    assert!(!receipt.accesses.is_empty());
    assert!(receipt.accesses.iter().all(|access| access.reverted));
    assert!(receipt
        .accesses
        .iter()
        .any(|access| access.key == b"failure-guard"));

    let outcome = engine
        .execute_canonical_with_speculation(
            vec![CanonicalTransaction::new(block, request)],
            vec![receipt],
        )
        .unwrap();
    assert_eq!(outcome.metrics.reused_results, 1);
    assert_eq!(engine.raw_storage(&contract, b"before_failure"), None);
}

#[test]
fn bundle_request_speculation_discards_caught_internal_scope_only() {
    let (engine, contract) = setup();
    let block = BlockContext::default();
    let request = ExecutionRequest::Bundle {
        transaction_id: TransactionId(102),
        calls: vec![
            ScopedBundleCall {
                source_revert_scope: None,
                call: BundleCall::Execute {
                    sender: Address::new("alice"),
                    contract: contract.clone(),
                    funds: vec![],
                    msg: to_json_binary(&ExecuteMsg::Set {
                        key: "outer_before_spec".to_owned(),
                        value: "committed".to_owned(),
                    })
                    .unwrap(),
                },
            },
            ScopedBundleCall {
                source_revert_scope: Some(77),
                call: BundleCall::Execute {
                    sender: Address::new("alice"),
                    contract: contract.clone(),
                    funds: vec![],
                    msg: to_json_binary(&ExecuteMsg::Set {
                        key: "inner_spec".to_owned(),
                        value: "rolled-back".to_owned(),
                    })
                    .unwrap(),
                },
            },
            ScopedBundleCall {
                source_revert_scope: Some(77),
                call: BundleCall::Execute {
                    sender: Address::new("alice"),
                    contract: contract.clone(),
                    funds: vec![],
                    msg: to_json_binary(&ExecuteMsg::Fail {}).unwrap(),
                },
            },
            ScopedBundleCall {
                source_revert_scope: None,
                call: BundleCall::Execute {
                    sender: Address::new("alice"),
                    contract: contract.clone(),
                    funds: vec![],
                    msg: to_json_binary(&ExecuteMsg::Set {
                        key: "outer_after_spec".to_owned(),
                        value: "committed".to_owned(),
                    })
                    .unwrap(),
                },
            },
        ],
        source_failed: false,
    };

    let receipt = engine
        .execute_speculative(&engine.snapshot(), block.clone(), request.clone())
        .unwrap();
    assert!(receipt.is_success());
    assert!(receipt
        .accesses
        .iter()
        .any(|access| access.key == b"inner_spec" && access.reverted));
    assert!(receipt
        .write_set
        .storage
        .iter()
        .any(|write| write.key == b"outer_before_spec"));
    assert!(receipt
        .write_set
        .storage
        .iter()
        .any(|write| write.key == b"outer_after_spec"));
    assert!(!receipt
        .write_set
        .storage
        .iter()
        .any(|write| write.key == b"inner_spec"));

    let outcome = engine
        .execute_canonical_with_speculation(
            vec![CanonicalTransaction::new(block, request)],
            vec![receipt],
        )
        .unwrap();
    assert_eq!(outcome.metrics.reused_results, 1);
    assert_eq!(
        engine.raw_storage(&contract, b"outer_before_spec"),
        Some(b"committed".to_vec())
    );
    assert_eq!(engine.raw_storage(&contract, b"inner_spec"), None);
    assert_eq!(
        engine.raw_storage(&contract, b"outer_after_spec"),
        Some(b"committed".to_vec())
    );
}

#[test]
fn deterministic_compute_bundle_call_has_no_state_accesses() {
    let (engine, _) = setup();
    let before = engine.snapshot();
    let outcome = engine
        .execute_bundle(
            TransactionId(9_999),
            BlockContext::default(),
            &[BundleCall::DeterministicCompute { iterations: 10_000 }],
        )
        .unwrap();
    assert!(outcome.accesses.is_empty());
    assert!(outcome.created_contracts.is_empty());
    assert!(engine.snapshot().same_world_state(&before));
}
