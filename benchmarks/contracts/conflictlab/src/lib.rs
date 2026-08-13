//! ConflictLab is a deliberately small CosmWasm contract for debugging symbolic conflict graphs.
//! Each entrypoint isolates one storage-access pattern: singleton fields, input-keyed maps,
//! composite keys, same-profile conflicts, state-dependent keys, delegation, and wildcard scans.

use cosmwasm_std::{
    entry_point, to_json_binary, Addr, Binary, Deps, DepsMut, Env, MessageInfo,
    Order as RangeOrder, Response, StdError, StdResult, Uint128,
};
use cw_storage_plus::{Item, Map};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const MAX_FEE_BPS: u16 = 10_000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct InstantiateMsg {
    pub admin: Option<String>,
    pub fee_bps: u16,
    pub epoch: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecuteMsg {
    Credit {
        account: String,
        amount: Uint128,
        #[serde(default)]
        work_iterations: u64,
        #[serde(default)]
        storage_rounds: u32,
        #[serde(default)]
        payload: Binary,
    },
    Transfer {
        from: String,
        to: String,
        amount: Uint128,
    },
    Approve {
        owner: String,
        spender: String,
        amount: Uint128,
    },
    TransferFrom {
        owner: String,
        spender: String,
        to: String,
        amount: Uint128,
    },
    IncrementCounter {
        shard_id: u64,
    },
    ConditionalCredit {
        account: String,
        expected_epoch: u64,
        amount: Uint128,
    },
    SetFee {
        new_fee_bps: u16,
    },
    SetEpoch {
        new_epoch: u64,
    },
    ReceiveTransfer {
        account: String,
        amount: Uint128,
    },
    CreateOrder {
        order_id: u64,
        owner: String,
        amount: Uint128,
    },
    CancelOrder {
        order_id: u64,
    },
    ResetAllBalances {},
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryMsg {
    Config {},
    Balance { account: String },
    Allowance { owner: String, spender: String },
    Counter { shard_id: u64 },
    Order { order_id: u64 },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct ConfigResponse {
    pub admin: String,
    pub fee_bps: u16,
    pub epoch: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct AmountResponse {
    pub amount: Uint128,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct CounterResponse {
    pub value: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct OrderResponse {
    pub order: Option<OrderRecord>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct OrderRecord {
    pub owner: Addr,
    pub amount: Uint128,
}

const ADMIN: Item<Addr> = Item::new("config/admin");
const FEE_BPS: Item<u16> = Item::new("config/fee_bps");
const EPOCH: Item<u64> = Item::new("config/epoch");
const BALANCES: Map<&str, Uint128> = Map::new("balances");
const ALLOWANCES: Map<&str, Uint128> = Map::new("allowances");
const COUNTERS: Map<u64, u64> = Map::new("counters");
const ORDERS: Map<u64, OrderRecord> = Map::new("orders");

#[derive(Error, Debug, PartialEq)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),
    #[error("unauthorized")]
    Unauthorized,
    #[error("fee basis points must not exceed 10000")]
    InvalidFee,
    #[error("insufficient funds")]
    InsufficientFunds,
    #[error("insufficient allowance")]
    InsufficientAllowance,
    #[error("epoch mismatch")]
    EpochMismatch,
    #[error("order already exists")]
    OrderExists,
    #[error("order not found")]
    OrderNotFound,
}

#[entry_point]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    if msg.fee_bps > MAX_FEE_BPS {
        return Err(ContractError::InvalidFee);
    }
    let admin = match msg.admin {
        Some(admin) => deps.api.addr_validate(&admin)?,
        None => info.sender,
    };
    ADMIN.save(deps.storage, &admin)?;
    FEE_BPS.save(deps.storage, &msg.fee_bps)?;
    EPOCH.save(deps.storage, &msg.epoch)?;
    Ok(Response::new().add_attribute("action", "instantiate"))
}

#[entry_point]
pub fn execute(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::Credit {
            account,
            amount,
            work_iterations,
            storage_rounds,
            payload,
        } => execute_credit(
            deps,
            account,
            amount,
            work_iterations,
            storage_rounds,
            payload,
        ),
        ExecuteMsg::Transfer { from, to, amount } => execute_transfer(deps, info, from, to, amount),
        ExecuteMsg::Approve {
            owner,
            spender,
            amount,
        } => execute_approve(deps, info, owner, spender, amount),
        ExecuteMsg::TransferFrom {
            owner,
            spender,
            to,
            amount,
        } => execute_transfer_from(deps, info, owner, spender, to, amount),
        ExecuteMsg::IncrementCounter { shard_id } => execute_increment_counter(deps, shard_id),
        ExecuteMsg::ConditionalCredit {
            account,
            expected_epoch,
            amount,
        } => execute_conditional_credit(deps, account, expected_epoch, amount),
        ExecuteMsg::SetFee { new_fee_bps } => execute_set_fee(deps, info, new_fee_bps),
        ExecuteMsg::SetEpoch { new_epoch } => execute_set_epoch(deps, info, new_epoch),
        ExecuteMsg::ReceiveTransfer { account, amount } => {
            execute_receive_transfer(deps, account, amount)
        }
        ExecuteMsg::CreateOrder {
            order_id,
            owner,
            amount,
        } => execute_create_order(deps, order_id, owner, amount),
        ExecuteMsg::CancelOrder { order_id } => execute_cancel_order(deps, info, order_id),
        ExecuteMsg::ResetAllBalances {} => execute_reset_all_balances(deps, info),
    }
}

fn execute_credit(
    deps: DepsMut,
    account: String,
    amount: Uint128,
    work_iterations: u64,
    storage_rounds: u32,
    payload: Binary,
) -> Result<Response, ContractError> {
    let account = deps.api.addr_validate(&account)?;
    let mut balance = BALANCES
        .may_load(deps.storage, account.as_str())?
        .unwrap_or_default();

    // Repeat semantically neutral read/write rounds on the same account key. This deliberately
    // changes host-storage intensity without changing ConflictLab's conflict relation.
    for _ in 0..storage_rounds {
        BALANCES.save(deps.storage, account.as_str(), &balance)?;
        balance = BALANCES
            .may_load(deps.storage, account.as_str())?
            .unwrap_or_default();
    }

    let checksum = deterministic_work(
        work_iterations,
        balance.u128() as u64 ^ amount.u128() as u64,
        payload.as_slice(),
    );
    BALANCES.save(
        deps.storage,
        account.as_str(),
        &balance.checked_add(amount).map_err(StdError::overflow)?,
    )?;
    Ok(Response::new()
        .add_attribute("action", "credit")
        .add_attribute("account", account)
        .add_attribute("work_checksum", checksum.to_string()))
}

fn deterministic_work(iterations: u64, seed: u64, payload: &[u8]) -> u64 {
    let mut value = seed ^ 0xD6E8_FEB8_6659_FD93;
    for (index, byte) in payload.iter().copied().enumerate() {
        value = value
            .wrapping_add(u64::from(byte).wrapping_mul((index as u64).wrapping_add(1)))
            .rotate_left((index & 31) as u32);
    }
    for index in 0..iterations {
        value = value
            .wrapping_add(index.rotate_left((index & 31) as u32))
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ value.rotate_right(11);
    }
    // Returning the checksum as a response attribute makes the work observable and prevents the
    // optimizer from deleting the loop while keeping persistent state unchanged.
    value
}

fn execute_transfer(
    deps: DepsMut,
    info: MessageInfo,
    from: String,
    to: String,
    amount: Uint128,
) -> Result<Response, ContractError> {
    let from = deps.api.addr_validate(&from)?;
    let to = deps.api.addr_validate(&to)?;
    if info.sender != from {
        return Err(ContractError::Unauthorized);
    }
    debit_balance(deps.storage, from.as_str(), amount)?;
    credit_balance(deps.storage, to.as_str(), amount)?;
    Ok(Response::new()
        .add_attribute("action", "transfer")
        .add_attribute("from", from)
        .add_attribute("to", to))
}

fn execute_approve(
    deps: DepsMut,
    info: MessageInfo,
    owner: String,
    spender: String,
    amount: Uint128,
) -> Result<Response, ContractError> {
    let owner = deps.api.addr_validate(&owner)?;
    let spender = deps.api.addr_validate(&spender)?;
    if info.sender != owner {
        return Err(ContractError::Unauthorized);
    }
    let key = allowance_key(owner.as_str(), spender.as_str());
    ALLOWANCES.save(deps.storage, &key, &amount)?;
    Ok(Response::new().add_attribute("action", "approve"))
}

fn execute_transfer_from(
    deps: DepsMut,
    info: MessageInfo,
    owner: String,
    spender: String,
    to: String,
    amount: Uint128,
) -> Result<Response, ContractError> {
    let owner = deps.api.addr_validate(&owner)?;
    let spender = deps.api.addr_validate(&spender)?;
    let to = deps.api.addr_validate(&to)?;
    if info.sender != spender {
        return Err(ContractError::Unauthorized);
    }
    let key = allowance_key(owner.as_str(), spender.as_str());
    let allowance = ALLOWANCES.may_load(deps.storage, &key)?.unwrap_or_default();
    if allowance < amount {
        return Err(ContractError::InsufficientAllowance);
    }
    ALLOWANCES.save(
        deps.storage,
        &key,
        &allowance.checked_sub(amount).map_err(StdError::overflow)?,
    )?;
    debit_balance(deps.storage, owner.as_str(), amount)?;
    credit_balance(deps.storage, to.as_str(), amount)?;
    Ok(Response::new().add_attribute("action", "transfer_from"))
}

fn execute_increment_counter(deps: DepsMut, shard_id: u64) -> Result<Response, ContractError> {
    let value = COUNTERS
        .may_load(deps.storage, shard_id)?
        .unwrap_or_default();
    COUNTERS.save(deps.storage, shard_id, &value.saturating_add(1))?;
    Ok(Response::new()
        .add_attribute("action", "increment_counter")
        .add_attribute("shard_id", shard_id.to_string()))
}

fn execute_conditional_credit(
    deps: DepsMut,
    account: String,
    expected_epoch: u64,
    amount: Uint128,
) -> Result<Response, ContractError> {
    let epoch = EPOCH.load(deps.storage)?;
    if epoch != expected_epoch {
        return Err(ContractError::EpochMismatch);
    }
    let account = deps.api.addr_validate(&account)?;
    let balance = BALANCES
        .may_load(deps.storage, account.as_str())?
        .unwrap_or_default();
    BALANCES.save(
        deps.storage,
        account.as_str(),
        &balance.checked_add(amount).map_err(StdError::overflow)?,
    )?;
    Ok(Response::new().add_attribute("action", "conditional_credit"))
}

fn execute_set_fee(
    deps: DepsMut,
    info: MessageInfo,
    new_fee_bps: u16,
) -> Result<Response, ContractError> {
    if new_fee_bps > MAX_FEE_BPS {
        return Err(ContractError::InvalidFee);
    }
    ensure_admin(deps.as_ref(), &info.sender)?;
    FEE_BPS.save(deps.storage, &new_fee_bps)?;
    Ok(Response::new().add_attribute("action", "set_fee"))
}

fn execute_set_epoch(
    deps: DepsMut,
    info: MessageInfo,
    new_epoch: u64,
) -> Result<Response, ContractError> {
    ensure_admin(deps.as_ref(), &info.sender)?;
    EPOCH.save(deps.storage, &new_epoch)?;
    Ok(Response::new().add_attribute("action", "set_epoch"))
}

fn execute_receive_transfer(
    deps: DepsMut,
    account: String,
    amount: Uint128,
) -> Result<Response, ContractError> {
    execute_credit(deps, account, amount, 0, 0, Binary::default())
        .map(|response| response.add_attribute("delegated_from", "receive_transfer"))
}

fn execute_create_order(
    deps: DepsMut,
    order_id: u64,
    owner: String,
    amount: Uint128,
) -> Result<Response, ContractError> {
    if ORDERS.may_load(deps.storage, order_id)?.is_some() {
        return Err(ContractError::OrderExists);
    }
    let owner = deps.api.addr_validate(&owner)?;
    debit_balance(deps.storage, owner.as_str(), amount)?;
    ORDERS.save(deps.storage, order_id, &OrderRecord { owner, amount })?;
    Ok(Response::new().add_attribute("action", "create_order"))
}

fn execute_cancel_order(
    deps: DepsMut,
    info: MessageInfo,
    order_id: u64,
) -> Result<Response, ContractError> {
    let order = ORDERS
        .may_load(deps.storage, order_id)?
        .ok_or(ContractError::OrderNotFound)?;
    if info.sender != order.owner {
        return Err(ContractError::Unauthorized);
    }
    ORDERS.remove(deps.storage, order_id);
    credit_balance(deps.storage, order.owner.as_str(), order.amount)?;
    Ok(Response::new().add_attribute("action", "cancel_order"))
}

fn execute_reset_all_balances(deps: DepsMut, info: MessageInfo) -> Result<Response, ContractError> {
    ensure_admin(deps.as_ref(), &info.sender)?;
    let accounts = BALANCES
        .keys(deps.storage, None, None, RangeOrder::Ascending)
        .collect::<StdResult<Vec<_>>>()?;
    for account in &accounts {
        BALANCES.remove(deps.storage, account.as_str());
    }
    Ok(Response::new()
        .add_attribute("action", "reset_all_balances")
        .add_attribute("removed", accounts.len().to_string()))
}

fn debit_balance(
    storage: &mut dyn cosmwasm_std::Storage,
    account: &str,
    amount: Uint128,
) -> Result<(), ContractError> {
    let balance = BALANCES.may_load(storage, account)?.unwrap_or_default();
    if balance < amount {
        return Err(ContractError::InsufficientFunds);
    }
    BALANCES.save(
        storage,
        account,
        &balance.checked_sub(amount).map_err(StdError::overflow)?,
    )?;
    Ok(())
}

fn credit_balance(
    storage: &mut dyn cosmwasm_std::Storage,
    account: &str,
    amount: Uint128,
) -> Result<(), ContractError> {
    let balance = BALANCES.may_load(storage, account)?.unwrap_or_default();
    BALANCES.save(
        storage,
        account,
        &balance.checked_add(amount).map_err(StdError::overflow)?,
    )?;
    Ok(())
}

fn allowance_key(owner: &str, spender: &str) -> String {
    format!("{owner}|{spender}")
}

fn ensure_admin(deps: Deps, sender: &Addr) -> Result<(), ContractError> {
    if ADMIN.load(deps.storage)?.as_str() != sender.as_str() {
        return Err(ContractError::Unauthorized);
    }
    Ok(())
}

#[entry_point]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Config {} => to_json_binary(&query_config(deps)?),
        QueryMsg::Balance { account } => to_json_binary(&query_balance(deps, account)?),
        QueryMsg::Allowance { owner, spender } => {
            to_json_binary(&query_allowance(deps, owner, spender)?)
        }
        QueryMsg::Counter { shard_id } => to_json_binary(&query_counter(deps, shard_id)?),
        QueryMsg::Order { order_id } => to_json_binary(&query_order(deps, order_id)?),
    }
}

fn query_config(deps: Deps) -> StdResult<ConfigResponse> {
    Ok(ConfigResponse {
        admin: ADMIN.load(deps.storage)?.into_string(),
        fee_bps: FEE_BPS.load(deps.storage)?,
        epoch: EPOCH.load(deps.storage)?,
    })
}

fn query_balance(deps: Deps, account: String) -> StdResult<AmountResponse> {
    let account = deps.api.addr_validate(&account)?;
    Ok(AmountResponse {
        amount: BALANCES
            .may_load(deps.storage, account.as_str())?
            .unwrap_or_default(),
    })
}

fn query_allowance(deps: Deps, owner: String, spender: String) -> StdResult<AmountResponse> {
    let owner = deps.api.addr_validate(&owner)?;
    let spender = deps.api.addr_validate(&spender)?;
    let key = allowance_key(owner.as_str(), spender.as_str());
    Ok(AmountResponse {
        amount: ALLOWANCES.may_load(deps.storage, &key)?.unwrap_or_default(),
    })
}

fn query_counter(deps: Deps, shard_id: u64) -> StdResult<CounterResponse> {
    Ok(CounterResponse {
        value: COUNTERS
            .may_load(deps.storage, shard_id)?
            .unwrap_or_default(),
    })
}

fn query_order(deps: Deps, order_id: u64) -> StdResult<OrderResponse> {
    Ok(OrderResponse {
        order: ORDERS.may_load(deps.storage, order_id)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::{
        from_json,
        testing::{mock_dependencies, mock_env, mock_info},
    };

    #[test]
    fn disjoint_counter_shards_and_balance_keys_behave_independently() {
        let mut deps = mock_dependencies();
        instantiate(
            deps.as_mut(),
            mock_env(),
            mock_info("admin", &[]),
            InstantiateMsg {
                admin: None,
                fee_bps: 25,
                epoch: 7,
            },
        )
        .unwrap();

        execute(
            deps.as_mut(),
            mock_env(),
            mock_info("alice", &[]),
            ExecuteMsg::Credit {
                account: "alice".to_owned(),
                amount: Uint128::new(100),
                work_iterations: 0,
                storage_rounds: 0,
                payload: Binary::default(),
            },
        )
        .unwrap();
        execute(
            deps.as_mut(),
            mock_env(),
            mock_info("alice", &[]),
            ExecuteMsg::IncrementCounter { shard_id: 1 },
        )
        .unwrap();
        execute(
            deps.as_mut(),
            mock_env(),
            mock_info("alice", &[]),
            ExecuteMsg::IncrementCounter { shard_id: 2 },
        )
        .unwrap();

        let balance: AmountResponse = from_json(
            query(
                deps.as_ref(),
                mock_env(),
                QueryMsg::Balance {
                    account: "alice".to_owned(),
                },
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(balance.amount, Uint128::new(100));

        let counter: CounterResponse =
            from_json(query(deps.as_ref(), mock_env(), QueryMsg::Counter { shard_id: 2 }).unwrap())
                .unwrap();
        assert_eq!(counter.value, 1);
    }

    #[test]
    fn credit_complexity_controls_do_not_change_balance_semantics() {
        let mut deps = mock_dependencies();
        instantiate(
            deps.as_mut(),
            mock_env(),
            mock_info("admin", &[]),
            InstantiateMsg {
                admin: None,
                fee_bps: 0,
                epoch: 1,
            },
        )
        .unwrap();

        let response = execute(
            deps.as_mut(),
            mock_env(),
            mock_info("client", &[]),
            ExecuteMsg::Credit {
                account: "alice".to_owned(),
                amount: Uint128::new(7),
                work_iterations: 64,
                storage_rounds: 3,
                payload: Binary::from(vec![1, 2, 3, 4]),
            },
        )
        .unwrap();

        assert_eq!(
            BALANCES.load(deps.as_ref().storage, "alice").unwrap(),
            Uint128::new(7)
        );
        assert!(response.attributes.iter().any(|attribute| attribute.key == "work_checksum"));
    }

    #[test]
    fn receive_transfer_delegates_to_credit() {
        let mut deps = mock_dependencies();
        instantiate(
            deps.as_mut(),
            mock_env(),
            mock_info("admin", &[]),
            InstantiateMsg {
                admin: None,
                fee_bps: 0,
                epoch: 1,
            },
        )
        .unwrap();
        execute(
            deps.as_mut(),
            mock_env(),
            mock_info("relayer", &[]),
            ExecuteMsg::ReceiveTransfer {
                account: "alice".to_owned(),
                amount: Uint128::new(12),
            },
        )
        .unwrap();

        assert_eq!(
            BALANCES.load(deps.as_ref().storage, "alice").unwrap(),
            Uint128::new(12)
        );
    }
}
