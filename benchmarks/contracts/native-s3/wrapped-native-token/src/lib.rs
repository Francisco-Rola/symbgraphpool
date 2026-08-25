//! Wrapped native-token analogue for the Vegeta S3 WETH family.
//!
//! The wrapped denomination is immutable code configuration rather than contract storage.  WETH9
//! likewise does not read a mutable "denom" slot on every deposit/withdraw, so persisting this
//! value would create an artificial cross-transaction storage dependency in the native replay.
use cosmwasm_std::{
    entry_point, to_json_binary, Addr, BankMsg, Binary, Coin, Deps, DepsMut, Env, MessageInfo,
    Response, StdError, StdResult, Uint128,
};
use cw_storage_plus::Map;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const NATIVE_DENOM: &str = "unative";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct InstantiateMsg {
    pub denom: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecuteMsg {
    Deposit {},
    Withdraw { amount: Uint128 },
    Transfer { recipient: String, amount: Uint128 },
    TransferFrom { owner: String, recipient: String, amount: Uint128 },
    Approve { spender: String, amount: Uint128 },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryMsg {
    Balance { address: String },
    Allowance { owner: String, spender: String },
    TotalSupply {},
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct AmountResponse {
    pub amount: Uint128,
}

const BALANCES: Map<&str, Uint128> = Map::new("balances");
const ALLOWANCES: Map<(&str, &str), Uint128> = Map::new("allowances");

#[derive(Error, Debug, PartialEq)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),
    #[error("invalid denomination")]
    InvalidDenom,
    #[error("invalid funds")]
    InvalidFunds,
    #[error("insufficient funds")]
    InsufficientFunds,
    #[error("insufficient allowance")]
    InsufficientAllowance,
}

#[entry_point]
pub fn instantiate(
    _deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    if msg.denom != NATIVE_DENOM {
        return Err(ContractError::InvalidDenom);
    }
    Ok(Response::new())
}

#[entry_point]
pub fn execute(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::Deposit {} => deposit(deps, info),
        ExecuteMsg::Withdraw { amount } => withdraw(deps, info.sender, amount),
        ExecuteMsg::Transfer { recipient, amount } => transfer(deps, info.sender, recipient, amount),
        ExecuteMsg::TransferFrom { owner, recipient, amount } => {
            transfer_from(deps, info.sender, owner, recipient, amount)
        }
        ExecuteMsg::Approve { spender, amount } => {
            let spender = deps.api.addr_validate(&spender)?;
            ALLOWANCES.save(
                deps.storage,
                (info.sender.as_str(), spender.as_str()),
                &amount,
            )?;
            Ok(Response::new())
        }
    }
}

fn deposit(deps: DepsMut, info: MessageInfo) -> Result<Response, ContractError> {
    let amount = info
        .funds
        .iter()
        .find(|coin| coin.denom == NATIVE_DENOM)
        .map(|coin| coin.amount)
        .unwrap_or_default();
    if amount.is_zero() || info.funds.iter().any(|coin| coin.denom != NATIVE_DENOM) {
        return Err(ContractError::InvalidFunds);
    }
    credit(deps.storage, info.sender.as_str(), amount)?;
    Ok(Response::new())
}

fn withdraw(
    deps: DepsMut,
    sender: Addr,
    amount: Uint128,
) -> Result<Response, ContractError> {
    debit(deps.storage, sender.as_str(), amount)?;
    Ok(Response::new().add_message(BankMsg::Send {
        to_address: sender.into_string(),
        amount: vec![Coin {
            denom: NATIVE_DENOM.to_string(),
            amount,
        }],
    }))
}

fn transfer(
    deps: DepsMut,
    sender: Addr,
    recipient: String,
    amount: Uint128,
) -> Result<Response, ContractError> {
    let recipient = deps.api.addr_validate(&recipient)?;
    debit(deps.storage, sender.as_str(), amount)?;
    credit(deps.storage, recipient.as_str(), amount)?;
    Ok(Response::new())
}

fn transfer_from(
    deps: DepsMut,
    spender: Addr,
    owner: String,
    recipient: String,
    amount: Uint128,
) -> Result<Response, ContractError> {
    let owner = deps.api.addr_validate(&owner)?;
    let recipient = deps.api.addr_validate(&recipient)?;
    let allowance = ALLOWANCES
        .may_load(deps.storage, (owner.as_str(), spender.as_str()))?
        .unwrap_or_default();
    if allowance < amount {
        return Err(ContractError::InsufficientAllowance);
    }
    ALLOWANCES.save(
        deps.storage,
        (owner.as_str(), spender.as_str()),
        &allowance.checked_sub(amount).map_err(StdError::overflow)?,
    )?;
    debit(deps.storage, owner.as_str(), amount)?;
    credit(deps.storage, recipient.as_str(), amount)?;
    Ok(Response::new())
}

fn debit(
    storage: &mut dyn cosmwasm_std::Storage,
    address: &str,
    amount: Uint128,
) -> Result<(), ContractError> {
    let balance = BALANCES.may_load(storage, address)?.unwrap_or_default();
    if balance < amount {
        return Err(ContractError::InsufficientFunds);
    }
    BALANCES.save(
        storage,
        address,
        &balance.checked_sub(amount).map_err(StdError::overflow)?,
    )?;
    Ok(())
}

fn credit(
    storage: &mut dyn cosmwasm_std::Storage,
    address: &str,
    amount: Uint128,
) -> StdResult<()> {
    let balance = BALANCES.may_load(storage, address)?.unwrap_or_default();
    BALANCES.save(
        storage,
        address,
        &balance.checked_add(amount).map_err(StdError::overflow)?,
    )
}

#[entry_point]
pub fn query(deps: Deps, env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Balance { address } => to_json_binary(&AmountResponse {
            amount: BALANCES.may_load(deps.storage, &address)?.unwrap_or_default(),
        }),
        QueryMsg::Allowance { owner, spender } => to_json_binary(&AmountResponse {
            amount: ALLOWANCES
                .may_load(deps.storage, (&owner, &spender))?
                .unwrap_or_default(),
        }),
        QueryMsg::TotalSupply {} => {
            let balance = deps
                .querier
                .query_balance(env.contract.address, NATIVE_DENOM)?;
            to_json_binary(&AmountResponse {
                amount: balance.amount,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::from_json;
    use cosmwasm_std::testing::{mock_dependencies, mock_env, mock_info};

    #[test]
    fn immutable_denom_is_never_persisted_in_contract_storage() {
        let mut deps = mock_dependencies();
        instantiate(
            deps.as_mut(),
            mock_env(),
            mock_info("admin", &[]),
            InstantiateMsg {
                denom: NATIVE_DENOM.into(),
            },
        )
        .unwrap();
        assert!(deps.as_ref().storage.get(b"denom").is_none());

        execute(
            deps.as_mut(),
            mock_env(),
            mock_info(
                "alice",
                &[Coin {
                    denom: NATIVE_DENOM.into(),
                    amount: Uint128::new(50),
                }],
            ),
            ExecuteMsg::Deposit {},
        )
        .unwrap();
        assert!(deps.as_ref().storage.get(b"denom").is_none());

        let response = execute(
            deps.as_mut(),
            mock_env(),
            mock_info("alice", &[]),
            ExecuteMsg::Withdraw {
                amount: Uint128::new(20),
            },
        )
        .unwrap();
        assert_eq!(response.messages.len(), 1);
        assert_eq!(
            BALANCES.load(deps.as_ref().storage, "alice").unwrap(),
            Uint128::new(30)
        );
        assert!(deps.as_ref().storage.get(b"denom").is_none());
    }

    #[test]
    fn total_supply_reads_native_collateral_without_contract_config_storage() {
        let mut deps = mock_dependencies();
        let env = mock_env();
        instantiate(
            deps.as_mut(),
            env.clone(),
            mock_info("admin", &[]),
            InstantiateMsg {
                denom: NATIVE_DENOM.into(),
            },
        )
        .unwrap();
        deps.querier.update_balance(
            env.contract.address.clone(),
            vec![Coin {
                denom: NATIVE_DENOM.into(),
                amount: Uint128::new(77),
            }],
        );
        let raw = query(deps.as_ref(), env, QueryMsg::TotalSupply {}).unwrap();
        let response: AmountResponse = from_json(raw).unwrap();
        assert_eq!(response.amount, Uint128::new(77));
        assert!(deps.as_ref().storage.get(b"denom").is_none());
    }

    #[test]
    fn instantiate_rejects_noncanonical_denom_without_writing_state() {
        let mut deps = mock_dependencies();
        let error = instantiate(
            deps.as_mut(),
            mock_env(),
            mock_info("admin", &[]),
            InstantiateMsg {
                denom: "ueth".into(),
            },
        )
        .unwrap_err();
        assert_eq!(error, ContractError::InvalidDenom);
        assert!(deps.as_ref().storage.get(b"denom").is_none());
    }
}
