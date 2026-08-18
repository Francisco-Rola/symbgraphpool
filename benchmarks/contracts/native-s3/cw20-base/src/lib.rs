//! Minimal CW20-like contract used by the Vegeta S3 native workload.
//! The implementation intentionally models only the fungible-token semantics exercised by S3.

use cosmwasm_std::{entry_point, to_json_binary, Addr, Binary, Deps, DepsMut, Env, MessageInfo, Response, StdError, StdResult, Uint128};
use cw_storage_plus::{Item, Map};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct InstantiateMsg {
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
    #[serde(default)]
    pub initial_balances: Vec<InitialBalance>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct InitialBalance {
    pub address: String,
    pub amount: Uint128,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecuteMsg {
    Transfer { recipient: String, amount: Uint128 },
    TransferFrom { owner: String, recipient: String, amount: Uint128 },
    Approve { spender: String, amount: Uint128 },
    Burn { amount: Uint128 },
    Mint { recipient: String, amount: Uint128 },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryMsg {
    Balance { address: String },
    Allowance { owner: String, spender: String },
    TotalSupply {},
    Decimals {},
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct TokenInfo {
    pub name: String,
    pub symbol: String,
    pub decimals: u8,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct AmountResponse { pub amount: Uint128 }
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct DecimalsResponse { pub decimals: u8 }

const TOKEN_INFO: Item<TokenInfo> = Item::new("token_info");
const TOTAL_SUPPLY: Item<Uint128> = Item::new("total_supply");
const BALANCES: Map<&str, Uint128> = Map::new("balances");
const ALLOWANCES: Map<(&str, &str), Uint128> = Map::new("allowances");

#[derive(Error, Debug, PartialEq)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),
    #[error("insufficient funds")]
    InsufficientFunds,
    #[error("insufficient allowance")]
    InsufficientAllowance,
}

#[entry_point]
pub fn instantiate(deps: DepsMut, _env: Env, _info: MessageInfo, msg: InstantiateMsg) -> Result<Response, ContractError> {
    TOKEN_INFO.save(deps.storage, &TokenInfo { name: msg.name, symbol: msg.symbol, decimals: msg.decimals })?;
    let mut total = Uint128::zero();
    for initial in msg.initial_balances {
        let address = deps.api.addr_validate(&initial.address)?;
        BALANCES.save(deps.storage, address.as_str(), &initial.amount)?;
        total = total.checked_add(initial.amount).map_err(StdError::overflow)?;
    }
    TOTAL_SUPPLY.save(deps.storage, &total)?;
    Ok(Response::new().add_attribute("action", "instantiate"))
}

#[entry_point]
pub fn execute(deps: DepsMut, _env: Env, info: MessageInfo, msg: ExecuteMsg) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::Transfer { recipient, amount } => transfer(deps, info.sender, recipient, amount),
        ExecuteMsg::TransferFrom { owner, recipient, amount } => transfer_from(deps, info.sender, owner, recipient, amount),
        ExecuteMsg::Approve { spender, amount } => approve(deps, info.sender, spender, amount),
        ExecuteMsg::Burn { amount } => burn(deps, info.sender, amount),
        ExecuteMsg::Mint { recipient, amount } => mint(deps, recipient, amount),
    }
}

fn transfer(deps: DepsMut, sender: Addr, recipient: String, amount: Uint128) -> Result<Response, ContractError> {
    let recipient = deps.api.addr_validate(&recipient)?;
    debit(deps.storage, sender.as_str(), amount)?;
    credit(deps.storage, recipient.as_str(), amount)?;
    Ok(Response::new().add_attribute("action", "transfer"))
}

fn transfer_from(deps: DepsMut, spender: Addr, owner: String, recipient: String, amount: Uint128) -> Result<Response, ContractError> {
    let owner = deps.api.addr_validate(&owner)?;
    let recipient = deps.api.addr_validate(&recipient)?;
    let allowance = ALLOWANCES.may_load(deps.storage, (owner.as_str(), spender.as_str()))?.unwrap_or_default();
    if allowance < amount { return Err(ContractError::InsufficientAllowance); }
    ALLOWANCES.save(deps.storage, (owner.as_str(), spender.as_str()), &allowance.checked_sub(amount).map_err(StdError::overflow)?)?;
    debit(deps.storage, owner.as_str(), amount)?;
    credit(deps.storage, recipient.as_str(), amount)?;
    Ok(Response::new().add_attribute("action", "transfer_from"))
}

fn approve(deps: DepsMut, owner: Addr, spender: String, amount: Uint128) -> Result<Response, ContractError> {
    let spender = deps.api.addr_validate(&spender)?;
    ALLOWANCES.save(deps.storage, (owner.as_str(), spender.as_str()), &amount)?;
    Ok(Response::new().add_attribute("action", "approve"))
}

fn burn(deps: DepsMut, owner: Addr, amount: Uint128) -> Result<Response, ContractError> {
    debit(deps.storage, owner.as_str(), amount)?;
    TOTAL_SUPPLY.update(deps.storage, |total| total.checked_sub(amount).map_err(StdError::overflow))?;
    Ok(Response::new().add_attribute("action", "burn"))
}

fn mint(deps: DepsMut, recipient: String, amount: Uint128) -> Result<Response, ContractError> {
    let recipient = deps.api.addr_validate(&recipient)?;
    credit(deps.storage, recipient.as_str(), amount)?;
    TOTAL_SUPPLY.update(deps.storage, |total| total.checked_add(amount).map_err(StdError::overflow))?;
    Ok(Response::new().add_attribute("action", "mint"))
}

fn debit(storage: &mut dyn cosmwasm_std::Storage, address: &str, amount: Uint128) -> Result<(), ContractError> {
    let current = BALANCES.may_load(storage, address)?.unwrap_or_default();
    if current < amount { return Err(ContractError::InsufficientFunds); }
    BALANCES.save(storage, address, &current.checked_sub(amount).map_err(StdError::overflow)?)?;
    Ok(())
}

fn credit(storage: &mut dyn cosmwasm_std::Storage, address: &str, amount: Uint128) -> Result<(), ContractError> {
    let current = BALANCES.may_load(storage, address)?.unwrap_or_default();
    BALANCES.save(storage, address, &current.checked_add(amount).map_err(StdError::overflow)?)?;
    Ok(())
}

#[entry_point]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Balance { address } => to_json_binary(&AmountResponse { amount: BALANCES.may_load(deps.storage, &address)?.unwrap_or_default() }),
        QueryMsg::Allowance { owner, spender } => to_json_binary(&AmountResponse { amount: ALLOWANCES.may_load(deps.storage, (owner.as_str(), spender.as_str()))?.unwrap_or_default() }),
        QueryMsg::TotalSupply {} => to_json_binary(&AmountResponse { amount: TOTAL_SUPPLY.load(deps.storage)? }),
        QueryMsg::Decimals {} => to_json_binary(&DecimalsResponse { decimals: TOKEN_INFO.load(deps.storage)?.decimals }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::testing::{mock_dependencies, mock_env, mock_info};

    #[test]
    fn transfer_and_allowance_paths_update_input_keyed_state() {
        let mut deps = mock_dependencies();
        instantiate(deps.as_mut(), mock_env(), mock_info("creator", &[]), InstantiateMsg {
            name: "Token".into(), symbol: "TOK".into(), decimals: 6,
            initial_balances: vec![InitialBalance { address: "alice".into(), amount: Uint128::new(100) }],
        }).unwrap();
        execute(deps.as_mut(), mock_env(), mock_info("alice", &[]), ExecuteMsg::Approve { spender: "bob".into(), amount: Uint128::new(40) }).unwrap();
        execute(deps.as_mut(), mock_env(), mock_info("bob", &[]), ExecuteMsg::TransferFrom { owner: "alice".into(), recipient: "carol".into(), amount: Uint128::new(25) }).unwrap();
        assert_eq!(BALANCES.load(deps.as_ref().storage, "alice").unwrap(), Uint128::new(75));
        assert_eq!(BALANCES.load(deps.as_ref().storage, "carol").unwrap(), Uint128::new(25));
        assert_eq!(ALLOWANCES.load(deps.as_ref().storage, ("alice", "bob")).unwrap(), Uint128::new(15));
    }
}
