//! Marketplace/router state model for the pre-declared Vegeta S3 marketplace-router family.
//!
//! The contract intentionally models only source-reviewed logical state classes: marketplace order
//! status, per-trader counters/nonces, the Universal Router execution lock, and Blur's internal
//! execution guard.  It does not import historical EVM storage keys or require hidden per-tx setup.
use cosmwasm_std::{entry_point, to_json_binary, Binary, Deps, DepsMut, Env, MessageInfo, Response, StdResult};
use cw_storage_plus::{Item, Map};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct InstantiateMsg {}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecuteMsg {
    RegisterOrder { order_id: String },
    SettleOrder { order_id: String },
    ValidateOrder { order_id: String },
    CancelOrder { order_id: String },
    IncrementCounter {},
    ExecuteRoute { route_id: String },
    V3SwapCallback { route_id: String },
    BlurSettle { order_id: String },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryMsg {
    GetOrderStatus { order_id: String },
    GetCounter { address: String },
    ReadRouteLock {},
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub enum OrderState {
    Registered,
    Validated,
    Fulfilled,
    Cancelled,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct OrderStatus {
    pub state: Option<OrderState>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct CounterResponse {
    pub value: u64,
}

const ORDERS: Map<&str, OrderState> = Map::new("orders");
const COUNTERS: Map<&str, u64> = Map::new("counters");
const ROUTER_LOCK: Item<bool> = Item::new("router_lock");
const BLUR_EXECUTION_GUARD: Item<bool> = Item::new("blur_execution_guard");

#[derive(Error, Debug, PartialEq)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] cosmwasm_std::StdError),
}

#[entry_point]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    _info: MessageInfo,
    _msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    ROUTER_LOCK.save(deps.storage, &false)?;
    BLUR_EXECUTION_GUARD.save(deps.storage, &false)?;
    Ok(Response::new())
}

fn save_order(
    deps: DepsMut,
    order_id: &str,
    state: OrderState,
) -> Result<Response, ContractError> {
    ORDERS.save(deps.storage, order_id, &state)?;
    Ok(Response::new())
}

fn touch_guard(
    storage: &mut dyn cosmwasm_std::Storage,
    guard: &Item<bool>,
) -> StdResult<()> {
    let value = guard.may_load(storage)?.unwrap_or(false);
    // Preserve the post-call state while retaining the execution-time storage dependency.
    guard.save(storage, &value)
}

#[entry_point]
pub fn execute(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    msg: ExecuteMsg,
) -> Result<Response, ContractError> {
    match msg {
        ExecuteMsg::RegisterOrder { order_id } => {
            save_order(deps, &order_id, OrderState::Registered)
        }
        ExecuteMsg::SettleOrder { order_id } => {
            save_order(deps, &order_id, OrderState::Fulfilled)
        }
        ExecuteMsg::ValidateOrder { order_id } => {
            save_order(deps, &order_id, OrderState::Validated)
        }
        ExecuteMsg::CancelOrder { order_id } => {
            save_order(deps, &order_id, OrderState::Cancelled)
        }
        ExecuteMsg::IncrementCounter {} => {
            COUNTERS.update(deps.storage, info.sender.as_str(), |old| -> StdResult<_> {
                Ok(old.unwrap_or_default() + 1)
            })?;
            Ok(Response::new())
        }
        ExecuteMsg::ExecuteRoute { route_id: _ } | ExecuteMsg::V3SwapCallback { route_id: _ } => {
            touch_guard(deps.storage, &ROUTER_LOCK)?;
            Ok(Response::new())
        }
        ExecuteMsg::BlurSettle { order_id } => {
            touch_guard(deps.storage, &BLUR_EXECUTION_GUARD)?;
            ORDERS.save(deps.storage, &order_id, &OrderState::Fulfilled)?;
            Ok(Response::new())
        }
    }
}

#[entry_point]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::GetOrderStatus { order_id } => to_json_binary(&OrderStatus {
            state: ORDERS.may_load(deps.storage, &order_id)?,
        }),
        QueryMsg::GetCounter { address } => to_json_binary(&CounterResponse {
            value: COUNTERS
                .may_load(deps.storage, &address)?
                .unwrap_or_default(),
        }),
        QueryMsg::ReadRouteLock {} => to_json_binary(&ROUTER_LOCK.may_load(deps.storage)?.unwrap_or(false)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::testing::{mock_dependencies, mock_env, mock_info};

    #[test]
    fn settlement_and_cancel_do_not_require_hidden_order_priming() {
        let mut deps = mock_dependencies();
        instantiate(
            deps.as_mut(),
            mock_env(),
            mock_info("admin", &[]),
            InstantiateMsg {},
        )
        .unwrap();
        execute(
            deps.as_mut(),
            mock_env(),
            mock_info("taker", &[]),
            ExecuteMsg::SettleOrder {
                order_id: "order-a".into(),
            },
        )
        .unwrap();
        execute(
            deps.as_mut(),
            mock_env(),
            mock_info("maker", &[]),
            ExecuteMsg::CancelOrder {
                order_id: "order-b".into(),
            },
        )
        .unwrap();
        assert_eq!(
            ORDERS.load(deps.as_ref().storage, "order-a").unwrap(),
            OrderState::Fulfilled
        );
        assert_eq!(
            ORDERS.load(deps.as_ref().storage, "order-b").unwrap(),
            OrderState::Cancelled
        );
    }

    #[test]
    fn counters_are_sender_keyed() {
        let mut deps = mock_dependencies();
        instantiate(
            deps.as_mut(),
            mock_env(),
            mock_info("admin", &[]),
            InstantiateMsg {},
        )
        .unwrap();
        execute(
            deps.as_mut(),
            mock_env(),
            mock_info("alice", &[]),
            ExecuteMsg::IncrementCounter {},
        )
        .unwrap();
        execute(
            deps.as_mut(),
            mock_env(),
            mock_info("bob", &[]),
            ExecuteMsg::IncrementCounter {},
        )
        .unwrap();
        assert_eq!(COUNTERS.load(deps.as_ref().storage, "alice").unwrap(), 1);
        assert_eq!(COUNTERS.load(deps.as_ref().storage, "bob").unwrap(), 1);
    }

    #[test]
    fn router_and_blur_guards_are_separate_source_semantic_singletons() {
        let mut deps = mock_dependencies();
        instantiate(
            deps.as_mut(),
            mock_env(),
            mock_info("admin", &[]),
            InstantiateMsg {},
        )
        .unwrap();
        execute(
            deps.as_mut(),
            mock_env(),
            mock_info("alice", &[]),
            ExecuteMsg::ExecuteRoute {
                route_id: "route-a".into(),
            },
        )
        .unwrap();
        execute(
            deps.as_mut(),
            mock_env(),
            mock_info("alice", &[]),
            ExecuteMsg::BlurSettle {
                order_id: "order-a".into(),
            },
        )
        .unwrap();
        assert!(!ROUTER_LOCK.load(deps.as_ref().storage).unwrap());
        assert!(!BLUR_EXECUTION_GUARD.load(deps.as_ref().storage).unwrap());
        assert_eq!(
            ORDERS.load(deps.as_ref().storage, "order-a").unwrap(),
            OrderState::Fulfilled
        );
    }
}
