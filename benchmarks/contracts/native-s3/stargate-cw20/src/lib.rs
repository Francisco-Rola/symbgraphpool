//! Ethereum-mainnet STG analogue for Vegeta S1.
//! Standard ERC20 behavior is modeled directly. On Stargate's main endpoint, sendTokens locks STG
//! in the token contract and lzReceive releases that escrow; total supply does not change. The
//! LayerZero transport itself is outside the scheduler benchmark's semantic scope.

use cosmwasm_std::{entry_point, to_json_binary, Addr, Binary, Deps, DepsMut, Env, MessageInfo, Response, StdError, StdResult, Uint128};
use cw_storage_plus::{Item, Map};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct InitialBalance { pub address: String, pub amount: Uint128 }
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct InstantiateMsg { pub name: String, pub symbol: String, pub decimals: u8, #[serde(default)] pub initial_balances: Vec<InitialBalance>, #[serde(default)] pub escrow_balance: Uint128 }

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecuteMsg {
    Transfer { recipient: String, amount: Uint128 },
    TransferFrom { owner: String, recipient: String, amount: Uint128 },
    Approve { spender: String, amount: Uint128 },
    BridgeSend { amount: Uint128 },
    BridgeReceive { recipient: String, amount: Uint128 },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryMsg { Balance { address: String }, Allowance { owner: String, spender: String }, TotalSupply {}, EscrowBalance {} }
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct TokenInfo { pub name: String, pub symbol: String, pub decimals: u8 }
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct AmountResponse { pub amount: Uint128 }

const TOKEN_INFO: Item<TokenInfo> = Item::new("token_info");
const TOTAL_SUPPLY: Item<Uint128> = Item::new("total_supply");
const BALANCES: Map<&str, Uint128> = Map::new("balances");
const ALLOWANCES: Map<(&str, &str), Uint128> = Map::new("allowances");
const ESCROW_KEY: &str = "__stargate_bridge_escrow__";

#[derive(Error, Debug, PartialEq)]
pub enum ContractError {
    #[error("{0}")] Std(#[from] StdError),
    #[error("insufficient funds")] InsufficientFunds,
    #[error("insufficient allowance")] InsufficientAllowance,
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
    total = total.checked_add(msg.escrow_balance).map_err(StdError::overflow)?;
    TOTAL_SUPPLY.save(deps.storage, &total)?;
    BALANCES.save(deps.storage, ESCROW_KEY, &msg.escrow_balance)?;
    Ok(Response::new())
}

#[entry_point]
pub fn execute(deps: DepsMut, _env: Env, info: MessageInfo, msg: ExecuteMsg) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::Transfer { recipient, amount } => transfer(deps, info.sender, recipient, amount),
        ExecuteMsg::TransferFrom { owner, recipient, amount } => transfer_from(deps, info.sender, owner, recipient, amount),
        ExecuteMsg::Approve { spender, amount } => approve(deps, info.sender, spender, amount),
        ExecuteMsg::BridgeSend { amount } => bridge_send(deps, info.sender, amount),
        ExecuteMsg::BridgeReceive { recipient, amount } => bridge_receive(deps, recipient, amount),
    }
}

fn debit(storage: &mut dyn cosmwasm_std::Storage, address: &str, amount: Uint128) -> Result<(), ContractError> {
    BALANCES.update(storage, address, |value| -> Result<_, ContractError> {
        let value = value.unwrap_or_default();
        if value < amount { return Err(ContractError::InsufficientFunds); }
        Ok(value - amount)
    })?;
    Ok(())
}
fn credit(storage: &mut dyn cosmwasm_std::Storage, address: &str, amount: Uint128) -> Result<(), ContractError> {
    BALANCES.update(storage, address, |value| -> StdResult<_> { Ok(value.unwrap_or_default() + amount) })?;
    Ok(())
}
fn transfer(deps: DepsMut, sender: Addr, recipient: String, amount: Uint128) -> Result<Response, ContractError> {
    let recipient = deps.api.addr_validate(&recipient)?;
    debit(deps.storage, sender.as_str(), amount)?;
    credit(deps.storage, recipient.as_str(), amount)?;
    Ok(Response::new())
}
fn transfer_from(deps: DepsMut, spender: Addr, owner: String, recipient: String, amount: Uint128) -> Result<Response, ContractError> {
    let owner = deps.api.addr_validate(&owner)?;
    let recipient = deps.api.addr_validate(&recipient)?;
    ALLOWANCES.update(deps.storage, (owner.as_str(), spender.as_str()), |value| -> Result<_, ContractError> {
        let value = value.unwrap_or_default();
        if value < amount { return Err(ContractError::InsufficientAllowance); }
        Ok(value - amount)
    })?;
    debit(deps.storage, owner.as_str(), amount)?;
    credit(deps.storage, recipient.as_str(), amount)?;
    Ok(Response::new())
}
fn approve(deps: DepsMut, owner: Addr, spender: String, amount: Uint128) -> Result<Response, ContractError> {
    let spender = deps.api.addr_validate(&spender)?;
    ALLOWANCES.save(deps.storage, (owner.as_str(), spender.as_str()), &amount)?;
    Ok(Response::new())
}
fn bridge_send(deps: DepsMut, sender: Addr, amount: Uint128) -> Result<Response, ContractError> {
    debit(deps.storage, sender.as_str(), amount)?;
    credit(deps.storage, ESCROW_KEY, amount)?;
    Ok(Response::new())
}
fn bridge_receive(deps: DepsMut, recipient: String, amount: Uint128) -> Result<Response, ContractError> {
    let recipient = deps.api.addr_validate(&recipient)?;
    debit(deps.storage, ESCROW_KEY, amount)?;
    credit(deps.storage, recipient.as_str(), amount)?;
    Ok(Response::new())
}

#[entry_point]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Balance { address } => to_json_binary(&AmountResponse { amount: BALANCES.may_load(deps.storage, &address)?.unwrap_or_default() }),
        QueryMsg::Allowance { owner, spender } => to_json_binary(&AmountResponse { amount: ALLOWANCES.may_load(deps.storage, (&owner, &spender))?.unwrap_or_default() }),
        QueryMsg::TotalSupply {} => to_json_binary(&AmountResponse { amount: TOTAL_SUPPLY.load(deps.storage)? }),
        QueryMsg::EscrowBalance {} => to_json_binary(&AmountResponse { amount: BALANCES.may_load(deps.storage, ESCROW_KEY)?.unwrap_or_default() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::testing::{mock_dependencies, mock_env, mock_info};

    #[test]
    fn mainnet_bridge_send_locks_without_changing_supply() {
        let mut deps = mock_dependencies();
        instantiate(deps.as_mut(), mock_env(), mock_info("x", &[]), InstantiateMsg { name: "STG".into(), symbol: "STG".into(), decimals: 18, initial_balances: vec![InitialBalance { address: "alice".into(), amount: Uint128::new(100) }], escrow_balance: Uint128::zero() }).unwrap();
        execute(deps.as_mut(), mock_env(), mock_info("alice", &[]), ExecuteMsg::BridgeSend { amount: Uint128::new(10) }).unwrap();
        assert_eq!(BALANCES.load(deps.as_ref().storage, "alice").unwrap(), Uint128::new(90));
        assert_eq!(BALANCES.load(deps.as_ref().storage, ESCROW_KEY).unwrap(), Uint128::new(10));
        assert_eq!(TOTAL_SUPPLY.load(deps.as_ref().storage).unwrap(), Uint128::new(100));
    }
}
