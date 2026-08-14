use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

use cosmwasm_std::{
    from_json, to_json_binary, AllBalanceResponse, BalanceResponse, BankQuery, Binary, Coin,
    ContractResult, Empty, QueryRequest, SystemError, SystemResult, WasmQuery,
};
use cosmwasm_vm::{BackendError, BackendResult, GasInfo, Querier};
use parking_lot::Mutex;

use crate::engine::{query_contract_shared, EngineCore};
use crate::error::EngineError;
use crate::parallel::ExecutionHotPathDiagnostics;
use crate::state::SharedTx;
use crate::types::{Address, BlockContext};

struct EngineQuerierContext {
    core: Weak<EngineCore>,
    tx: SharedTx,
    caller_contract: Address,
    block: BlockContext,
    depth: u32,
    diagnostics: Option<Arc<ExecutionHotPathDiagnostics>>,
}

#[derive(Clone)]
pub(crate) struct EngineQuerierBinding {
    inner: Arc<Mutex<EngineQuerierContext>>,
}

impl EngineQuerierBinding {
    pub(crate) fn rebind(
        &self,
        core: Arc<EngineCore>,
        tx: SharedTx,
        caller_contract: Address,
        block: BlockContext,
        depth: u32,
    ) {
        let diagnostics = tx.lock().diagnostics();
        let mut context = self.inner.lock();
        context.core = Arc::downgrade(&core);
        context.tx = tx;
        context.caller_contract = caller_contract;
        context.block = block;
        context.depth = depth;
        context.diagnostics = diagnostics;
    }
}

pub(crate) struct EngineQuerier {
    binding: EngineQuerierBinding,
}

impl EngineQuerier {
    pub fn new(
        core: Arc<EngineCore>,
        tx: SharedTx,
        caller_contract: Address,
        block: BlockContext,
        depth: u32,
    ) -> Self {
        Self::rebindable(core, tx, caller_contract, block, depth).0
    }

    pub(crate) fn rebindable(
        core: Arc<EngineCore>,
        tx: SharedTx,
        caller_contract: Address,
        block: BlockContext,
        depth: u32,
    ) -> (Self, EngineQuerierBinding) {
        let diagnostics = tx.lock().diagnostics();
        let binding = EngineQuerierBinding {
            inner: Arc::new(Mutex::new(EngineQuerierContext {
                core: Arc::downgrade(&core),
                tx,
                caller_contract,
                block,
                depth,
                diagnostics,
            })),
        };
        (
            Self {
                binding: binding.clone(),
            },
            binding,
        )
    }
}

impl Querier for EngineQuerier {
    fn query_raw(
        &self,
        request: &[u8],
        _gas_limit: u64,
    ) -> BackendResult<SystemResult<ContractResult<Binary>>> {
        let started = Instant::now();
        let context = self.binding.inner.lock();
        let mut lock_wait = Duration::ZERO;
        let gas = GasInfo::with_externally_used(request.len() as u64);
        let parsed: QueryRequest<Empty> = match from_json(request) {
            Ok(parsed) => parsed,
            Err(error) => {
                if let Some(diagnostics) = &context.diagnostics {
                    diagnostics.record_host_query(started.elapsed(), lock_wait);
                }
                return (
                    Ok(SystemResult::Err(SystemError::InvalidRequest {
                        error: error.to_string(),
                        request: Binary::new(request.to_vec()),
                    })),
                    gas,
                );
            }
        };

        let response = match parsed {
            QueryRequest::Bank(BankQuery::Balance { address, denom }) => {
                let address = Address::new(address);
                let lock_started = Instant::now();
                let mut tx = context.tx.lock();
                lock_wait += lock_started.elapsed();
                let amount = tx.balance(&address, &denom, &context.caller_contract, context.depth);
                drop(tx);
                serialize_contract_response(&BalanceResponse::new(Coin::new(amount.u128(), denom)))
            }
            QueryRequest::Bank(BankQuery::AllBalances { address }) => {
                let address = Address::new(address);
                let lock_started = Instant::now();
                let mut tx = context.tx.lock();
                lock_wait += lock_started.elapsed();
                let balances = tx.all_balances(&address, &context.caller_contract, context.depth);
                drop(tx);
                serialize_contract_response(&AllBalanceResponse::new(balances))
            }
            QueryRequest::Wasm(WasmQuery::Raw { contract_addr, key }) => {
                let target = Address::new(contract_addr.clone());
                if crate::state::code_id_of(&context.tx, &target).is_err() {
                    SystemResult::Err(SystemError::NoSuchContract {
                        addr: contract_addr,
                    })
                } else {
                    let lock_started = Instant::now();
                    let mut tx = context.tx.lock();
                    lock_wait += lock_started.elapsed();
                    let value = tx
                        .storage_get(&target, key.as_slice(), context.depth + 1)
                        .unwrap_or_default();
                    drop(tx);
                    SystemResult::Ok(ContractResult::Ok(Binary::new(value)))
                }
            }
            QueryRequest::Wasm(WasmQuery::Smart { contract_addr, msg }) => {
                let target = Address::new(contract_addr.clone());
                let Some(core) = context.core.upgrade() else {
                    if let Some(diagnostics) = &context.diagnostics {
                        diagnostics.record_host_query(started.elapsed(), lock_wait);
                    }
                    return (
                        Err(BackendError::user_err(
                            "CosmWasm engine was dropped while a reusable VM query was active",
                        )),
                        gas,
                    );
                };
                match query_contract_shared(
                    core,
                    context.tx.clone(),
                    context.block.clone(),
                    target,
                    msg,
                    context.depth + 1,
                ) {
                    Ok(data) => SystemResult::Ok(ContractResult::Ok(data)),
                    Err(EngineError::UnknownContract(_)) => {
                        SystemResult::Err(SystemError::NoSuchContract {
                            addr: contract_addr,
                        })
                    }
                    Err(EngineError::Contract(error) | EngineError::Native(error)) => {
                        SystemResult::Ok(ContractResult::Err(error))
                    }
                    Err(error) => {
                        if let Some(diagnostics) = &context.diagnostics {
                            diagnostics.record_host_query(started.elapsed(), lock_wait);
                        }
                        return (Err(BackendError::user_err(error.to_string())), gas);
                    }
                }
            }
            other => SystemResult::Err(SystemError::UnsupportedRequest {
                kind: format!("{other:?}"),
            }),
        };

        if let Some(diagnostics) = &context.diagnostics {
            diagnostics.record_host_query(started.elapsed(), lock_wait);
        }
        (Ok(response), gas)
    }
}

fn serialize_contract_response<T: serde::Serialize>(
    value: &T,
) -> SystemResult<ContractResult<Binary>> {
    match to_json_binary(value) {
        Ok(binary) => SystemResult::Ok(ContractResult::Ok(binary)),
        Err(error) => SystemResult::Err(SystemError::InvalidResponse {
            error: error.to_string(),
            response: Binary::default(),
        }),
    }
}
