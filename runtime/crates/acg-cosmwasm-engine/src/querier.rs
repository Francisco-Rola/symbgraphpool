use std::sync::Arc;

use cosmwasm_std::{
    from_json, to_json_binary, AllBalanceResponse, BalanceResponse, BankQuery, Binary, Coin,
    ContractResult, Empty, QueryRequest, SystemError, SystemResult, WasmQuery,
};
use cosmwasm_vm::{BackendError, BackendResult, GasInfo, Querier};

use crate::engine::{query_contract_shared, EngineCore};
use crate::error::EngineError;
use crate::state::SharedTx;
use crate::types::{Address, BlockContext};

pub(crate) struct EngineQuerier {
    core: Arc<EngineCore>,
    tx: SharedTx,
    caller_contract: Address,
    block: BlockContext,
    depth: u32,
}

impl EngineQuerier {
    pub fn new(
        core: Arc<EngineCore>,
        tx: SharedTx,
        caller_contract: Address,
        block: BlockContext,
        depth: u32,
    ) -> Self {
        Self {
            core,
            tx,
            caller_contract,
            block,
            depth,
        }
    }
}

impl Querier for EngineQuerier {
    fn query_raw(
        &self,
        request: &[u8],
        _gas_limit: u64,
    ) -> BackendResult<SystemResult<ContractResult<Binary>>> {
        let gas = GasInfo::with_externally_used(request.len() as u64);
        let parsed: QueryRequest<Empty> = match from_json(request) {
            Ok(parsed) => parsed,
            Err(error) => {
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
                let amount =
                    self.tx
                        .lock()
                        .balance(&address, &denom, &self.caller_contract, self.depth);
                serialize_contract_response(&BalanceResponse::new(Coin::new(amount.u128(), denom)))
            }
            QueryRequest::Bank(BankQuery::AllBalances { address }) => {
                let address = Address::new(address);
                let balances =
                    self.tx
                        .lock()
                        .all_balances(&address, &self.caller_contract, self.depth);
                serialize_contract_response(&AllBalanceResponse::new(balances))
            }
            QueryRequest::Wasm(WasmQuery::Raw { contract_addr, key }) => {
                let target = Address::new(contract_addr.clone());
                if crate::state::code_id_of(&self.tx, &target).is_err() {
                    SystemResult::Err(SystemError::NoSuchContract {
                        addr: contract_addr,
                    })
                } else {
                    let value = self
                        .tx
                        .lock()
                        .storage_get(&target, key.as_slice(), self.depth + 1)
                        .unwrap_or_default();
                    SystemResult::Ok(ContractResult::Ok(Binary::new(value)))
                }
            }
            QueryRequest::Wasm(WasmQuery::Smart { contract_addr, msg }) => {
                let target = Address::new(contract_addr.clone());
                match query_contract_shared(
                    self.core.clone(),
                    self.tx.clone(),
                    self.block.clone(),
                    target,
                    msg,
                    self.depth + 1,
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
                        return (Err(BackendError::user_err(error.to_string())), gas);
                    }
                }
            }
            other => SystemResult::Err(SystemError::UnsupportedRequest {
                kind: format!("{other:?}"),
            }),
        };

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
