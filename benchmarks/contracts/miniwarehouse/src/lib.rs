//! MiniWarehouse is a compact, non-compliant TPC-C-inspired CosmWasm workload.
//! It preserves the useful conflict structure—warehouse, district, customer, stock, order,
//! order-line, new-order, and history records—without implementing the official benchmark.

use cosmwasm_std::{
    entry_point, to_json_binary, Addr, Binary, Deps, DepsMut, Env, MessageInfo,
    Order as RangeOrder, Response, StdError, StdResult, Uint128,
};
use cw_storage_plus::{Item, Map};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct InstantiateMsg {
    pub admin: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExecuteMsg {
    SeedWarehouse {
        warehouse_id: u64,
        tax_bps: u16,
    },
    SeedDistrict {
        warehouse_id: u64,
        district_id: u64,
        tax_bps: u16,
        next_order_id: u64,
    },
    SeedCustomer {
        warehouse_id: u64,
        district_id: u64,
        customer_id: u64,
        discount_bps: u16,
    },
    SeedStock {
        warehouse_id: u64,
        item_id: u64,
        quantity: u32,
    },
    Restock {
        warehouse_id: u64,
        item_id: u64,
        quantity: u32,
    },
    NewOrder {
        warehouse_id: u64,
        district_id: u64,
        customer_id: u64,
        order_id: u64,
        lines: Vec<NewOrderLine>,
    },
    Payment {
        warehouse_id: u64,
        district_id: u64,
        customer_id: u64,
        amount: Uint128,
        history_id: u64,
    },
    Delivery {
        warehouse_id: u64,
        district_id: u64,
        order_id: u64,
        carrier_id: u64,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct NewOrderLine {
    pub item_id: u64,
    pub supply_warehouse_id: u64,
    pub quantity: u32,
    pub unit_price: Uint128,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum QueryMsg {
    Warehouse {
        warehouse_id: u64,
    },
    Customer {
        warehouse_id: u64,
        district_id: u64,
        customer_id: u64,
    },
    Stock {
        warehouse_id: u64,
        item_id: u64,
    },
    OrderStatus {
        warehouse_id: u64,
        district_id: u64,
        customer_id: u64,
        order_id: u64,
    },
    StockLevel {
        warehouse_id: u64,
        district_id: u64,
        threshold: u32,
        item_ids: Vec<u64>,
    },
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct Config {
    pub admin: Addr,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct Warehouse {
    pub tax_bps: u16,
    pub payment_ytd: Uint128,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct District {
    pub tax_bps: u16,
    pub next_order_id: u64,
    pub payment_ytd: Uint128,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct Customer {
    pub discount_bps: u16,
    pub payment_total: Uint128,
    pub delivery_total: Uint128,
    pub last_order_id: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct Stock {
    pub quantity: u32,
    pub ytd: u64,
    pub order_count: u64,
    pub remote_count: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct OrderRecord {
    pub customer_id: u64,
    pub carrier_id: Option<u64>,
    pub total: Uint128,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct OrderLineRecord {
    pub item_id: u64,
    pub supply_warehouse_id: u64,
    pub quantity: u32,
    pub amount: Uint128,
    pub delivered: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct HistoryRecord {
    pub warehouse_id: u64,
    pub district_id: u64,
    pub customer_id: u64,
    pub amount: Uint128,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct OrderStatusResponse {
    pub customer: Customer,
    pub order: OrderRecord,
    pub lines: Vec<OrderLineRecord>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, JsonSchema)]
pub struct StockLevelResponse {
    pub low_stock_items: Vec<u64>,
}

const CONFIG: Item<Config> = Item::new("config");
const WAREHOUSES: Map<&str, Warehouse> = Map::new("warehouses");
const DISTRICTS: Map<&str, District> = Map::new("districts");
const CUSTOMERS: Map<&str, Customer> = Map::new("customers");
const STOCK: Map<&str, Stock> = Map::new("stock");
const ORDERS: Map<&str, OrderRecord> = Map::new("orders");
const NEW_ORDERS: Map<&str, bool> = Map::new("new_orders");
const ORDER_LINES: Map<&str, OrderLineRecord> = Map::new("order_lines");
const HISTORY: Map<&str, HistoryRecord> = Map::new("history");

#[derive(Error, Debug, PartialEq)]
pub enum ContractError {
    #[error("{0}")]
    Std(#[from] StdError),
    #[error("unauthorized")]
    Unauthorized,
    #[error("warehouse not found")]
    WarehouseNotFound,
    #[error("district not found")]
    DistrictNotFound,
    #[error("customer not found")]
    CustomerNotFound,
    #[error("stock item not found")]
    StockNotFound,
    #[error("order not found")]
    OrderNotFound,
    #[error("new order marker not found")]
    NewOrderNotFound,
    #[error("expected order id {expected}, got {actual}")]
    UnexpectedOrderId { expected: u64, actual: u64 },
    #[error("insufficient stock for item {item_id}")]
    InsufficientStock { item_id: u64 },
    #[error("order must contain at least one line")]
    EmptyOrder,
}

#[entry_point]
pub fn instantiate(
    deps: DepsMut,
    _env: Env,
    info: MessageInfo,
    msg: InstantiateMsg,
) -> Result<Response, ContractError> {
    let admin = match msg.admin {
        Some(admin) => deps.api.addr_validate(&admin)?,
        None => info.sender,
    };
    CONFIG.save(deps.storage, &Config { admin })?;
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
        ExecuteMsg::SeedWarehouse {
            warehouse_id,
            tax_bps,
        } => execute_seed_warehouse(deps, info, warehouse_id, tax_bps),
        ExecuteMsg::SeedDistrict {
            warehouse_id,
            district_id,
            tax_bps,
            next_order_id,
        } => execute_seed_district(
            deps,
            info,
            warehouse_id,
            district_id,
            tax_bps,
            next_order_id,
        ),
        ExecuteMsg::SeedCustomer {
            warehouse_id,
            district_id,
            customer_id,
            discount_bps,
        } => execute_seed_customer(
            deps,
            info,
            warehouse_id,
            district_id,
            customer_id,
            discount_bps,
        ),
        ExecuteMsg::SeedStock {
            warehouse_id,
            item_id,
            quantity,
        } => execute_seed_stock(deps, info, warehouse_id, item_id, quantity),
        ExecuteMsg::Restock {
            warehouse_id,
            item_id,
            quantity,
        } => execute_restock(deps, warehouse_id, item_id, quantity),
        ExecuteMsg::NewOrder {
            warehouse_id,
            district_id,
            customer_id,
            order_id,
            lines,
        } => execute_new_order(
            deps,
            warehouse_id,
            district_id,
            customer_id,
            order_id,
            lines,
        ),
        ExecuteMsg::Payment {
            warehouse_id,
            district_id,
            customer_id,
            amount,
            history_id,
        } => execute_payment(
            deps,
            warehouse_id,
            district_id,
            customer_id,
            amount,
            history_id,
        ),
        ExecuteMsg::Delivery {
            warehouse_id,
            district_id,
            order_id,
            carrier_id,
        } => execute_delivery(deps, warehouse_id, district_id, order_id, carrier_id),
    }
}

fn execute_seed_warehouse(
    deps: DepsMut,
    info: MessageInfo,
    warehouse_id: u64,
    tax_bps: u16,
) -> Result<Response, ContractError> {
    ensure_admin(deps.as_ref(), &info.sender)?;
    let key = warehouse_key(warehouse_id);
    WAREHOUSES.save(
        deps.storage,
        &key,
        &Warehouse {
            tax_bps,
            payment_ytd: Uint128::zero(),
        },
    )?;
    Ok(Response::new().add_attribute("action", "seed_warehouse"))
}

fn execute_seed_district(
    deps: DepsMut,
    info: MessageInfo,
    warehouse_id: u64,
    district_id: u64,
    tax_bps: u16,
    next_order_id: u64,
) -> Result<Response, ContractError> {
    ensure_admin(deps.as_ref(), &info.sender)?;
    let key = district_key(warehouse_id, district_id);
    DISTRICTS.save(
        deps.storage,
        &key,
        &District {
            tax_bps,
            next_order_id,
            payment_ytd: Uint128::zero(),
        },
    )?;
    Ok(Response::new().add_attribute("action", "seed_district"))
}

fn execute_seed_customer(
    deps: DepsMut,
    info: MessageInfo,
    warehouse_id: u64,
    district_id: u64,
    customer_id: u64,
    discount_bps: u16,
) -> Result<Response, ContractError> {
    ensure_admin(deps.as_ref(), &info.sender)?;
    let key = customer_key(warehouse_id, district_id, customer_id);
    CUSTOMERS.save(
        deps.storage,
        &key,
        &Customer {
            discount_bps,
            payment_total: Uint128::zero(),
            delivery_total: Uint128::zero(),
            last_order_id: None,
        },
    )?;
    Ok(Response::new().add_attribute("action", "seed_customer"))
}

fn execute_seed_stock(
    deps: DepsMut,
    info: MessageInfo,
    warehouse_id: u64,
    item_id: u64,
    quantity: u32,
) -> Result<Response, ContractError> {
    ensure_admin(deps.as_ref(), &info.sender)?;
    let key = stock_key(warehouse_id, item_id);
    STOCK.save(
        deps.storage,
        &key,
        &Stock {
            quantity,
            ytd: 0,
            order_count: 0,
            remote_count: 0,
        },
    )?;
    Ok(Response::new().add_attribute("action", "seed_stock"))
}

fn execute_restock(
    deps: DepsMut,
    warehouse_id: u64,
    item_id: u64,
    quantity: u32,
) -> Result<Response, ContractError> {
    let key = stock_key(warehouse_id, item_id);
    let mut stock = STOCK
        .may_load(deps.storage, &key)?
        .ok_or(ContractError::StockNotFound)?;
    stock.quantity = stock.quantity.saturating_add(quantity);
    STOCK.save(deps.storage, &key, &stock)?;
    Ok(Response::new().add_attribute("action", "restock"))
}

fn execute_new_order(
    deps: DepsMut,
    warehouse_id: u64,
    district_id: u64,
    customer_id: u64,
    order_id: u64,
    lines: Vec<NewOrderLine>,
) -> Result<Response, ContractError> {
    if lines.is_empty() {
        return Err(ContractError::EmptyOrder);
    }
    let warehouse = WAREHOUSES
        .may_load(deps.storage, &warehouse_key(warehouse_id))?
        .ok_or(ContractError::WarehouseNotFound)?;

    let district_storage_key = district_key(warehouse_id, district_id);
    let mut district = DISTRICTS
        .may_load(deps.storage, &district_storage_key)?
        .ok_or(ContractError::DistrictNotFound)?;
    if district.next_order_id != order_id {
        return Err(ContractError::UnexpectedOrderId {
            expected: district.next_order_id,
            actual: order_id,
        });
    }
    district.next_order_id = district.next_order_id.saturating_add(1);
    DISTRICTS.save(deps.storage, &district_storage_key, &district)?;

    let customer_storage_key = customer_key(warehouse_id, district_id, customer_id);
    let mut customer = CUSTOMERS
        .may_load(deps.storage, &customer_storage_key)?
        .ok_or(ContractError::CustomerNotFound)?;
    customer.last_order_id = Some(order_id);
    CUSTOMERS.save(deps.storage, &customer_storage_key, &customer)?;

    let mut total = Uint128::zero();
    for (line_number, line) in lines.into_iter().enumerate() {
        let stock_storage_key = stock_key(line.supply_warehouse_id, line.item_id);
        let mut stock = STOCK
            .may_load(deps.storage, &stock_storage_key)?
            .ok_or(ContractError::StockNotFound)?;
        if stock.quantity < line.quantity {
            return Err(ContractError::InsufficientStock {
                item_id: line.item_id,
            });
        }
        stock.quantity -= line.quantity;
        stock.ytd = stock.ytd.saturating_add(u64::from(line.quantity));
        stock.order_count = stock.order_count.saturating_add(1);
        if line.supply_warehouse_id != warehouse_id {
            stock.remote_count = stock.remote_count.saturating_add(1);
        }
        STOCK.save(deps.storage, &stock_storage_key, &stock)?;

        let amount = line
            .unit_price
            .checked_mul(Uint128::from(line.quantity))
            .map_err(StdError::overflow)?;
        total = total.checked_add(amount).map_err(StdError::overflow)?;
        let line_key = order_line_key(warehouse_id, district_id, order_id, line_number as u64);
        ORDER_LINES.save(
            deps.storage,
            &line_key,
            &OrderLineRecord {
                item_id: line.item_id,
                supply_warehouse_id: line.supply_warehouse_id,
                quantity: line.quantity,
                amount,
                delivered: false,
            },
        )?;
    }

    let tax_multiplier = 10_000_u128 + u128::from(warehouse.tax_bps) + u128::from(district.tax_bps);
    total = total.multiply_ratio(tax_multiplier, 10_000_u128);

    let order_storage_key = order_key(warehouse_id, district_id, order_id);
    ORDERS.save(
        deps.storage,
        &order_storage_key,
        &OrderRecord {
            customer_id,
            carrier_id: None,
            total,
        },
    )?;
    NEW_ORDERS.save(deps.storage, &order_storage_key, &true)?;
    Ok(Response::new().add_attribute("action", "new_order"))
}

fn execute_payment(
    deps: DepsMut,
    warehouse_id: u64,
    district_id: u64,
    customer_id: u64,
    amount: Uint128,
    history_id: u64,
) -> Result<Response, ContractError> {
    let warehouse_storage_key = warehouse_key(warehouse_id);
    let mut warehouse = WAREHOUSES
        .may_load(deps.storage, &warehouse_storage_key)?
        .ok_or(ContractError::WarehouseNotFound)?;
    warehouse.payment_ytd = warehouse
        .payment_ytd
        .checked_add(amount)
        .map_err(StdError::overflow)?;
    WAREHOUSES.save(deps.storage, &warehouse_storage_key, &warehouse)?;

    let district_storage_key = district_key(warehouse_id, district_id);
    let mut district = DISTRICTS
        .may_load(deps.storage, &district_storage_key)?
        .ok_or(ContractError::DistrictNotFound)?;
    district.payment_ytd = district
        .payment_ytd
        .checked_add(amount)
        .map_err(StdError::overflow)?;
    DISTRICTS.save(deps.storage, &district_storage_key, &district)?;

    let customer_storage_key = customer_key(warehouse_id, district_id, customer_id);
    let mut customer = CUSTOMERS
        .may_load(deps.storage, &customer_storage_key)?
        .ok_or(ContractError::CustomerNotFound)?;
    customer.payment_total = customer
        .payment_total
        .checked_add(amount)
        .map_err(StdError::overflow)?;
    CUSTOMERS.save(deps.storage, &customer_storage_key, &customer)?;

    let history_storage_key = history_key(history_id);
    HISTORY.save(
        deps.storage,
        &history_storage_key,
        &HistoryRecord {
            warehouse_id,
            district_id,
            customer_id,
            amount,
        },
    )?;
    Ok(Response::new().add_attribute("action", "payment"))
}

fn execute_delivery(
    deps: DepsMut,
    warehouse_id: u64,
    district_id: u64,
    order_id: u64,
    carrier_id: u64,
) -> Result<Response, ContractError> {
    let order_storage_key = order_key(warehouse_id, district_id, order_id);
    if NEW_ORDERS
        .may_load(deps.storage, &order_storage_key)?
        .is_none()
    {
        return Err(ContractError::NewOrderNotFound);
    }
    NEW_ORDERS.remove(deps.storage, &order_storage_key);

    let mut order = ORDERS
        .may_load(deps.storage, &order_storage_key)?
        .ok_or(ContractError::OrderNotFound)?;
    order.carrier_id = Some(carrier_id);
    ORDERS.save(deps.storage, &order_storage_key, &order)?;

    let prefix = order_line_prefix(warehouse_id, district_id, order_id);
    let line_records = ORDER_LINES
        .range(deps.storage, None, None, RangeOrder::Ascending)
        .filter(|item| {
            item.as_ref()
                .map(|(key, _)| key.starts_with(&prefix))
                .unwrap_or(true)
        })
        .collect::<StdResult<Vec<_>>>()?;
    let mut delivered_total = Uint128::zero();
    for (key, mut line) in line_records {
        delivered_total = delivered_total
            .checked_add(line.amount)
            .map_err(StdError::overflow)?;
        line.delivered = true;
        ORDER_LINES.save(deps.storage, &key, &line)?;
    }

    let customer_storage_key = customer_key(warehouse_id, district_id, order.customer_id);
    let mut customer = CUSTOMERS
        .may_load(deps.storage, &customer_storage_key)?
        .ok_or(ContractError::CustomerNotFound)?;
    customer.delivery_total = customer
        .delivery_total
        .checked_add(delivered_total)
        .map_err(StdError::overflow)?;
    CUSTOMERS.save(deps.storage, &customer_storage_key, &customer)?;
    Ok(Response::new().add_attribute("action", "delivery"))
}

fn ensure_admin(deps: Deps, sender: &Addr) -> Result<(), ContractError> {
    let config = CONFIG.load(deps.storage)?;
    if config.admin.as_str() != sender.as_str() {
        return Err(ContractError::Unauthorized);
    }
    Ok(())
}

#[entry_point]
pub fn query(deps: Deps, _env: Env, msg: QueryMsg) -> StdResult<Binary> {
    match msg {
        QueryMsg::Warehouse { warehouse_id } => {
            to_json_binary(&WAREHOUSES.load(deps.storage, &warehouse_key(warehouse_id))?)
        }
        QueryMsg::Customer {
            warehouse_id,
            district_id,
            customer_id,
        } => to_json_binary(&CUSTOMERS.load(
            deps.storage,
            &customer_key(warehouse_id, district_id, customer_id),
        )?),
        QueryMsg::Stock {
            warehouse_id,
            item_id,
        } => to_json_binary(&STOCK.load(deps.storage, &stock_key(warehouse_id, item_id))?),
        QueryMsg::OrderStatus {
            warehouse_id,
            district_id,
            customer_id,
            order_id,
        } => to_json_binary(&query_order_status(
            deps,
            warehouse_id,
            district_id,
            customer_id,
            order_id,
        )?),
        QueryMsg::StockLevel {
            warehouse_id,
            district_id,
            threshold,
            item_ids,
        } => to_json_binary(&query_stock_level(
            deps,
            warehouse_id,
            district_id,
            threshold,
            item_ids,
        )?),
    }
}

fn query_order_status(
    deps: Deps,
    warehouse_id: u64,
    district_id: u64,
    customer_id: u64,
    order_id: u64,
) -> StdResult<OrderStatusResponse> {
    let customer = CUSTOMERS.load(
        deps.storage,
        &customer_key(warehouse_id, district_id, customer_id),
    )?;
    let order = ORDERS.load(
        deps.storage,
        &order_key(warehouse_id, district_id, order_id),
    )?;
    let prefix = order_line_prefix(warehouse_id, district_id, order_id);
    let lines = ORDER_LINES
        .range(deps.storage, None, None, RangeOrder::Ascending)
        .filter_map(|item| match item {
            Ok((key, line)) if key.starts_with(&prefix) => Some(Ok(line)),
            Ok(_) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<StdResult<Vec<_>>>()?;
    Ok(OrderStatusResponse {
        customer,
        order,
        lines,
    })
}

fn query_stock_level(
    deps: Deps,
    warehouse_id: u64,
    district_id: u64,
    threshold: u32,
    item_ids: Vec<u64>,
) -> StdResult<StockLevelResponse> {
    DISTRICTS.load(deps.storage, &district_key(warehouse_id, district_id))?;
    let mut low_stock_items = Vec::new();
    for item_id in item_ids {
        let stock = STOCK.load(deps.storage, &stock_key(warehouse_id, item_id))?;
        if stock.quantity < threshold {
            low_stock_items.push(item_id);
        }
    }
    Ok(StockLevelResponse { low_stock_items })
}

fn warehouse_key(warehouse_id: u64) -> String {
    warehouse_id.to_string()
}

fn district_key(warehouse_id: u64, district_id: u64) -> String {
    format!("{warehouse_id}:{district_id}")
}

fn customer_key(warehouse_id: u64, district_id: u64, customer_id: u64) -> String {
    format!("{warehouse_id}:{district_id}:{customer_id}")
}

fn stock_key(warehouse_id: u64, item_id: u64) -> String {
    format!("{warehouse_id}:{item_id}")
}

fn order_key(warehouse_id: u64, district_id: u64, order_id: u64) -> String {
    format!("{warehouse_id}:{district_id}:{order_id}")
}

fn order_line_prefix(warehouse_id: u64, district_id: u64, order_id: u64) -> String {
    format!("{warehouse_id}:{district_id}:{order_id}:")
}

fn order_line_key(warehouse_id: u64, district_id: u64, order_id: u64, line_number: u64) -> String {
    format!(
        "{}{line_number}",
        order_line_prefix(warehouse_id, district_id, order_id)
    )
}

fn history_key(history_id: u64) -> String {
    history_id.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use cosmwasm_std::{
        from_json,
        testing::{mock_dependencies, mock_env, mock_info},
    };

    fn instantiate_and_seed() -> cosmwasm_std::OwnedDeps<
        cosmwasm_std::MemoryStorage,
        cosmwasm_std::testing::MockApi,
        cosmwasm_std::testing::MockQuerier,
    > {
        let mut deps = mock_dependencies();
        instantiate(
            deps.as_mut(),
            mock_env(),
            mock_info("admin", &[]),
            InstantiateMsg { admin: None },
        )
        .unwrap();
        let admin = mock_info("admin", &[]);
        execute(
            deps.as_mut(),
            mock_env(),
            admin.clone(),
            ExecuteMsg::SeedWarehouse {
                warehouse_id: 1,
                tax_bps: 100,
            },
        )
        .unwrap();
        execute(
            deps.as_mut(),
            mock_env(),
            admin.clone(),
            ExecuteMsg::SeedDistrict {
                warehouse_id: 1,
                district_id: 1,
                tax_bps: 50,
                next_order_id: 1,
            },
        )
        .unwrap();
        execute(
            deps.as_mut(),
            mock_env(),
            admin.clone(),
            ExecuteMsg::SeedCustomer {
                warehouse_id: 1,
                district_id: 1,
                customer_id: 7,
                discount_bps: 200,
            },
        )
        .unwrap();
        execute(
            deps.as_mut(),
            mock_env(),
            admin,
            ExecuteMsg::SeedStock {
                warehouse_id: 1,
                item_id: 99,
                quantity: 20,
            },
        )
        .unwrap();
        deps
    }

    #[test]
    fn new_order_and_delivery_touch_the_expected_records() {
        let mut deps = instantiate_and_seed();
        execute(
            deps.as_mut(),
            mock_env(),
            mock_info("buyer", &[]),
            ExecuteMsg::NewOrder {
                warehouse_id: 1,
                district_id: 1,
                customer_id: 7,
                order_id: 1,
                lines: vec![NewOrderLine {
                    item_id: 99,
                    supply_warehouse_id: 1,
                    quantity: 3,
                    unit_price: Uint128::new(10),
                }],
            },
        )
        .unwrap();
        execute(
            deps.as_mut(),
            mock_env(),
            mock_info("carrier", &[]),
            ExecuteMsg::Delivery {
                warehouse_id: 1,
                district_id: 1,
                order_id: 1,
                carrier_id: 4,
            },
        )
        .unwrap();

        let response: OrderStatusResponse = from_json(
            query(
                deps.as_ref(),
                mock_env(),
                QueryMsg::OrderStatus {
                    warehouse_id: 1,
                    district_id: 1,
                    customer_id: 7,
                    order_id: 1,
                },
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response.order.carrier_id, Some(4));
        assert!(response.lines.iter().all(|line| line.delivered));
    }

    #[test]
    fn stock_level_reads_only_requested_items() {
        let deps = instantiate_and_seed();
        let response: StockLevelResponse = from_json(
            query(
                deps.as_ref(),
                mock_env(),
                QueryMsg::StockLevel {
                    warehouse_id: 1,
                    district_id: 1,
                    threshold: 25,
                    item_ids: vec![99],
                },
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(response.low_stock_items, vec![99]);
    }
}
