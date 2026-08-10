//! Deterministic MiniWarehouse workload generation for validator-simulation benchmarks.
//!
//! This crate emits the same JSON wire format as the source-controlled MiniWarehouse CosmWasm
//! contract without depending on the benchmark contract crate itself. Keeping the generator in the
//! runtime workspace lets experiments produce concrete [`ExecutionRequest`] values and feed them
//! directly into `RateControlledIngress`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use acg_cosmwasm_engine::{Address, ExecutionRequest, TransactionId};
use cosmwasm_std::{to_json_binary, Uint128};
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const BASIS_POINTS_DENOMINATOR: u16 = 10_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MiniWarehouseScale {
    pub warehouse_count: u64,
    pub districts_per_warehouse: u64,
    pub customers_per_district: u64,
    pub items_per_warehouse: u64,
}

impl Default for MiniWarehouseScale {
    fn default() -> Self {
        Self {
            warehouse_count: 4,
            districts_per_warehouse: 4,
            customers_per_district: 100,
            items_per_warehouse: 1_000,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MiniWarehouseMix {
    pub new_order: u32,
    pub payment: u32,
    pub delivery: u32,
    pub restock: u32,
}

impl MiniWarehouseMix {
    pub const fn total(self) -> u64 {
        self.new_order as u64 + self.payment as u64 + self.delivery as u64 + self.restock as u64
    }
}

impl Default for MiniWarehouseMix {
    fn default() -> Self {
        // TPC-C-inspired write-heavy mix. Restock is a synthetic replacement for benchmark
        // transaction types that are read-only in this compact contract.
        Self {
            new_order: 45,
            payment: 43,
            delivery: 4,
            restock: 8,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MiniWarehouseWorkloadConfig {
    pub contract: Address,
    pub admin: Address,
    pub client: Address,
    pub scale: MiniWarehouseScale,
    pub mix: MiniWarehouseMix,
    pub seed: u64,
    pub first_transaction_id: u64,
    pub first_history_id: u64,
    pub first_order_id: u64,
    pub initial_stock_quantity: u32,
    pub min_order_lines: u32,
    pub max_order_lines: u32,
    pub max_order_line_quantity: u32,
    pub unit_price: Uint128,
    pub remote_stock_probability_bps: u16,
    pub hot_warehouse_id: u64,
    pub hot_warehouse_probability_bps: u16,
}

impl MiniWarehouseWorkloadConfig {
    pub fn for_contract(contract: Address) -> Self {
        Self {
            contract,
            admin: Address::new("admin"),
            client: Address::new("client"),
            scale: MiniWarehouseScale::default(),
            mix: MiniWarehouseMix::default(),
            seed: 0xAC61_2026,
            first_transaction_id: 1,
            first_history_id: 1,
            first_order_id: 1,
            initial_stock_quantity: 10_000,
            min_order_lines: 5,
            max_order_lines: 10,
            max_order_line_quantity: 5,
            unit_price: Uint128::new(1),
            remote_stock_probability_bps: 100,
            hot_warehouse_id: 1,
            hot_warehouse_probability_bps: 0,
        }
    }

    pub fn validate(&self) -> Result<(), WorkloadError> {
        if self.scale.warehouse_count == 0 {
            return Err(WorkloadError::ZeroWarehouses);
        }
        if self.scale.districts_per_warehouse == 0 {
            return Err(WorkloadError::ZeroDistricts);
        }
        if self.scale.customers_per_district == 0 {
            return Err(WorkloadError::ZeroCustomers);
        }
        if self.scale.items_per_warehouse == 0 {
            return Err(WorkloadError::ZeroItems);
        }
        if self.min_order_lines == 0 || self.min_order_lines > self.max_order_lines {
            return Err(WorkloadError::InvalidOrderLineRange {
                min: self.min_order_lines,
                max: self.max_order_lines,
            });
        }
        if u64::from(self.max_order_lines) > self.scale.items_per_warehouse {
            return Err(WorkloadError::TooManyOrderLinesForItemDomain {
                max_order_lines: self.max_order_lines,
                items_per_warehouse: self.scale.items_per_warehouse,
            });
        }
        if self.max_order_line_quantity == 0 {
            return Err(WorkloadError::ZeroOrderLineQuantity);
        }
        if self.initial_stock_quantity == 0 {
            return Err(WorkloadError::ZeroInitialStock);
        }
        if self.mix.total() == 0 {
            return Err(WorkloadError::EmptyTransactionMix);
        }
        validate_bps(
            "remote_stock_probability_bps",
            self.remote_stock_probability_bps,
        )?;
        validate_bps(
            "hot_warehouse_probability_bps",
            self.hot_warehouse_probability_bps,
        )?;
        if !(1..=self.scale.warehouse_count).contains(&self.hot_warehouse_id) {
            return Err(WorkloadError::InvalidHotWarehouse {
                hot_warehouse_id: self.hot_warehouse_id,
                warehouse_count: self.scale.warehouse_count,
            });
        }
        Ok(())
    }
}

fn validate_bps(field: &'static str, value: u16) -> Result<(), WorkloadError> {
    if value > BASIS_POINTS_DENOMINATOR {
        Err(WorkloadError::BasisPointsOutOfRange { field, value })
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MiniWarehouseOperation {
    SeedWarehouse,
    SeedDistrict,
    SeedCustomer,
    SeedStock,
    Restock,
    NewOrder,
    Payment,
    Delivery,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GeneratedTransaction {
    pub operation: MiniWarehouseOperation,
    pub request: ExecutionRequest,
}

impl GeneratedTransaction {
    pub fn transaction_id(&self) -> TransactionId {
        self.request.transaction_id()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MiniWarehouseExecuteMsg {
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
        lines: Vec<MiniWarehouseNewOrderLine>,
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MiniWarehouseNewOrderLine {
    pub item_id: u64,
    pub supply_warehouse_id: u64,
    pub quantity: u32,
    pub unit_price: Uint128,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct MiniWarehouseOrderKey {
    pub warehouse_id: u64,
    pub district_id: u64,
    pub order_id: u64,
}

/// Deterministic stateful generator for structurally coherent MiniWarehouse write traffic.
pub struct MiniWarehouseWorkloadGenerator {
    config: MiniWarehouseWorkloadConfig,
    rng: SplitMix64,
    next_transaction_id: u64,
    next_history_id: u64,
    next_order_ids: BTreeMap<(u64, u64), u64>,
    undelivered_orders: VecDeque<MiniWarehouseOrderKey>,
}

impl MiniWarehouseWorkloadGenerator {
    pub fn new(config: MiniWarehouseWorkloadConfig) -> Result<Self, WorkloadError> {
        config.validate()?;
        let mut next_order_ids = BTreeMap::new();
        for warehouse_id in 1..=config.scale.warehouse_count {
            for district_id in 1..=config.scale.districts_per_warehouse {
                next_order_ids.insert((warehouse_id, district_id), config.first_order_id);
            }
        }
        Ok(Self {
            rng: SplitMix64::new(config.seed),
            next_transaction_id: config.first_transaction_id,
            next_history_id: config.first_history_id,
            config,
            next_order_ids,
            undelivered_orders: VecDeque::new(),
        })
    }

    pub fn config(&self) -> &MiniWarehouseWorkloadConfig {
        &self.config
    }

    pub fn pending_deliveries(&self) -> usize {
        self.undelivered_orders.len()
    }

    /// Produces deterministic admin transactions that initialize every configured warehouse,
    /// district, customer and stock item. The caller should submit these before the measured run.
    pub fn bootstrap(&mut self) -> Result<Vec<GeneratedTransaction>, WorkloadError> {
        let mut transactions = Vec::new();
        let scale = self.config.scale;
        let admin = self.config.admin.clone();
        let first_order_id = self.config.first_order_id;
        let initial_stock_quantity = self.config.initial_stock_quantity;
        for warehouse_id in 1..=scale.warehouse_count {
            transactions.push(self.execute(
                MiniWarehouseOperation::SeedWarehouse,
                admin.clone(),
                MiniWarehouseExecuteMsg::SeedWarehouse {
                    warehouse_id,
                    tax_bps: 100,
                },
            )?);
            for district_id in 1..=scale.districts_per_warehouse {
                transactions.push(self.execute(
                    MiniWarehouseOperation::SeedDistrict,
                    admin.clone(),
                    MiniWarehouseExecuteMsg::SeedDistrict {
                        warehouse_id,
                        district_id,
                        tax_bps: 50,
                        next_order_id: first_order_id,
                    },
                )?);
                for customer_id in 1..=scale.customers_per_district {
                    transactions.push(self.execute(
                        MiniWarehouseOperation::SeedCustomer,
                        admin.clone(),
                        MiniWarehouseExecuteMsg::SeedCustomer {
                            warehouse_id,
                            district_id,
                            customer_id,
                            discount_bps: 0,
                        },
                    )?);
                }
            }
            for item_id in 1..=scale.items_per_warehouse {
                transactions.push(self.execute(
                    MiniWarehouseOperation::SeedStock,
                    admin.clone(),
                    MiniWarehouseExecuteMsg::SeedStock {
                        warehouse_id,
                        item_id,
                        quantity: initial_stock_quantity,
                    },
                )?);
            }
        }
        Ok(transactions)
    }

    pub fn generate(&mut self, count: usize) -> Result<Vec<GeneratedTransaction>, WorkloadError> {
        (0..count).map(|_| self.generate_one()).collect()
    }

    pub fn generate_requests(
        &mut self,
        count: usize,
    ) -> Result<Vec<ExecutionRequest>, WorkloadError> {
        Ok(self
            .generate(count)?
            .into_iter()
            .map(|generated| generated.request)
            .collect())
    }

    pub fn generate_one(&mut self) -> Result<GeneratedTransaction, WorkloadError> {
        match self.choose_operation() {
            MiniWarehouseOperation::NewOrder => self.generate_new_order(),
            MiniWarehouseOperation::Payment => self.generate_payment(),
            MiniWarehouseOperation::Delivery if self.undelivered_orders.is_empty() => {
                // Never emit an invalid delivery just because the configured mix selected one
                // before any order has been generated.
                self.generate_new_order()
            }
            MiniWarehouseOperation::Delivery => self.generate_delivery(),
            MiniWarehouseOperation::Restock => self.generate_restock(),
            MiniWarehouseOperation::SeedWarehouse
            | MiniWarehouseOperation::SeedDistrict
            | MiniWarehouseOperation::SeedCustomer
            | MiniWarehouseOperation::SeedStock => {
                unreachable!("bootstrap operations are not part of the steady-state mix")
            }
        }
    }

    fn choose_operation(&mut self) -> MiniWarehouseOperation {
        let mix = self.config.mix;
        let draw = self.rng.below(mix.total());
        let mut boundary = u64::from(mix.new_order);
        if draw < boundary {
            return MiniWarehouseOperation::NewOrder;
        }
        boundary += u64::from(mix.payment);
        if draw < boundary {
            return MiniWarehouseOperation::Payment;
        }
        boundary += u64::from(mix.delivery);
        if draw < boundary {
            return MiniWarehouseOperation::Delivery;
        }
        MiniWarehouseOperation::Restock
    }

    fn generate_new_order(&mut self) -> Result<GeneratedTransaction, WorkloadError> {
        let warehouse_id = self.choose_home_warehouse();
        let districts_per_warehouse = self.config.scale.districts_per_warehouse;
        let customers_per_district = self.config.scale.customers_per_district;
        let items_per_warehouse = self.config.scale.items_per_warehouse;
        let min_order_lines = self.config.min_order_lines;
        let max_order_lines = self.config.max_order_lines;
        let max_order_line_quantity = self.config.max_order_line_quantity;
        let unit_price = self.config.unit_price;
        let client = self.config.client.clone();
        let district_id = self.one_based(districts_per_warehouse);
        let customer_id = self.one_based(customers_per_district);
        let order_id = self.allocate_order_id(warehouse_id, district_id)?;
        let line_count = self.inclusive_u32(min_order_lines, max_order_lines);
        let mut selected_items = BTreeSet::new();
        let mut lines = Vec::with_capacity(line_count as usize);
        while lines.len() < line_count as usize {
            let item_id = self.one_based(items_per_warehouse);
            if !selected_items.insert(item_id) {
                continue;
            }
            let supply_warehouse_id = self.choose_supply_warehouse(warehouse_id);
            lines.push(MiniWarehouseNewOrderLine {
                item_id,
                supply_warehouse_id,
                quantity: self.inclusive_u32(1, max_order_line_quantity),
                unit_price,
            });
        }
        let order_key = MiniWarehouseOrderKey {
            warehouse_id,
            district_id,
            order_id,
        };
        let generated = self.execute(
            MiniWarehouseOperation::NewOrder,
            client,
            MiniWarehouseExecuteMsg::NewOrder {
                warehouse_id,
                district_id,
                customer_id,
                order_id,
                lines,
            },
        )?;
        self.undelivered_orders.push_back(order_key);
        Ok(generated)
    }

    fn generate_payment(&mut self) -> Result<GeneratedTransaction, WorkloadError> {
        let warehouse_id = self.choose_home_warehouse();
        let districts_per_warehouse = self.config.scale.districts_per_warehouse;
        let customers_per_district = self.config.scale.customers_per_district;
        let client = self.config.client.clone();
        let district_id = self.one_based(districts_per_warehouse);
        let customer_id = self.one_based(customers_per_district);
        let amount = Uint128::from(self.inclusive_u64(1, 100));
        let history_id = self.allocate_history_id()?;
        self.execute(
            MiniWarehouseOperation::Payment,
            client,
            MiniWarehouseExecuteMsg::Payment {
                warehouse_id,
                district_id,
                customer_id,
                amount,
                history_id,
            },
        )
    }

    fn generate_delivery(&mut self) -> Result<GeneratedTransaction, WorkloadError> {
        let order = self
            .undelivered_orders
            .pop_front()
            .expect("delivery generation requires a pending order");
        let carrier_id = self.inclusive_u64(1, 10);
        let client = self.config.client.clone();
        self.execute(
            MiniWarehouseOperation::Delivery,
            client,
            MiniWarehouseExecuteMsg::Delivery {
                warehouse_id: order.warehouse_id,
                district_id: order.district_id,
                order_id: order.order_id,
                carrier_id,
            },
        )
    }

    fn generate_restock(&mut self) -> Result<GeneratedTransaction, WorkloadError> {
        let warehouse_id = self.choose_home_warehouse();
        let items_per_warehouse = self.config.scale.items_per_warehouse;
        let client = self.config.client.clone();
        let item_id = self.one_based(items_per_warehouse);
        let max_quantity = self.config.max_order_line_quantity.saturating_mul(4);
        let quantity = self.inclusive_u32(1, max_quantity);
        self.execute(
            MiniWarehouseOperation::Restock,
            client,
            MiniWarehouseExecuteMsg::Restock {
                warehouse_id,
                item_id,
                quantity,
            },
        )
    }

    fn execute(
        &mut self,
        operation: MiniWarehouseOperation,
        sender: Address,
        msg: MiniWarehouseExecuteMsg,
    ) -> Result<GeneratedTransaction, WorkloadError> {
        let transaction_id = self.allocate_transaction_id()?;
        let binary = to_json_binary(&msg).map_err(WorkloadError::SerializeMessage)?;
        Ok(GeneratedTransaction {
            operation,
            request: ExecutionRequest::Execute {
                transaction_id,
                sender,
                contract: self.config.contract.clone(),
                funds: Vec::new(),
                msg: binary,
            },
        })
    }

    fn choose_home_warehouse(&mut self) -> u64 {
        let probability = self.config.hot_warehouse_probability_bps;
        let hot_warehouse_id = self.config.hot_warehouse_id;
        let warehouse_count = self.config.scale.warehouse_count;
        if self.rng.basis_points(probability) {
            hot_warehouse_id
        } else {
            self.one_based(warehouse_count)
        }
    }

    fn choose_supply_warehouse(&mut self, home_warehouse_id: u64) -> u64 {
        let warehouse_count = self.config.scale.warehouse_count;
        let probability = self.config.remote_stock_probability_bps;
        if warehouse_count <= 1 || !self.rng.basis_points(probability) {
            return home_warehouse_id;
        }
        let offset = self.rng.below(warehouse_count.saturating_sub(1));
        let zero_based_home = home_warehouse_id - 1;
        let zero_based_remote = if offset >= zero_based_home {
            offset + 1
        } else {
            offset
        };
        zero_based_remote + 1
    }

    fn allocate_transaction_id(&mut self) -> Result<TransactionId, WorkloadError> {
        let id = self.next_transaction_id;
        self.next_transaction_id = self
            .next_transaction_id
            .checked_add(1)
            .ok_or(WorkloadError::TransactionIdExhausted)?;
        Ok(TransactionId(id))
    }

    fn allocate_history_id(&mut self) -> Result<u64, WorkloadError> {
        let id = self.next_history_id;
        self.next_history_id = self
            .next_history_id
            .checked_add(1)
            .ok_or(WorkloadError::HistoryIdExhausted)?;
        Ok(id)
    }

    fn allocate_order_id(
        &mut self,
        warehouse_id: u64,
        district_id: u64,
    ) -> Result<u64, WorkloadError> {
        let next = self
            .next_order_ids
            .get_mut(&(warehouse_id, district_id))
            .expect("all configured districts have an order-id counter");
        let id = *next;
        *next = next.checked_add(1).ok_or(WorkloadError::OrderIdExhausted {
            warehouse_id,
            district_id,
        })?;
        Ok(id)
    }

    fn one_based(&mut self, upper: u64) -> u64 {
        self.rng.below(upper) + 1
    }

    fn inclusive_u64(&mut self, min: u64, max: u64) -> u64 {
        min + self.rng.below(max - min + 1)
    }

    fn inclusive_u32(&mut self, min: u32, max: u32) -> u32 {
        self.inclusive_u64(u64::from(min), u64::from(max)) as u32
    }
}

pub fn decode_execute_request(
    request: &ExecutionRequest,
) -> Result<MiniWarehouseExecuteMsg, WorkloadError> {
    let ExecutionRequest::Execute { msg, .. } = request else {
        return Err(WorkloadError::ExpectedExecuteRequest);
    };
    serde_json::from_slice(msg.as_slice()).map_err(WorkloadError::DecodeMessage)
}

#[derive(Clone, Copy, Debug)]
struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }

    fn below(&mut self, upper: u64) -> u64 {
        debug_assert!(upper > 0);
        let zone = u64::MAX - (u64::MAX % upper);
        loop {
            let value = self.next();
            if value < zone {
                return value % upper;
            }
        }
    }

    fn basis_points(&mut self, probability_bps: u16) -> bool {
        probability_bps == BASIS_POINTS_DENOMINATOR
            || (probability_bps != 0
                && self.below(u64::from(BASIS_POINTS_DENOMINATOR)) < u64::from(probability_bps))
    }
}

#[derive(Debug, Error)]
pub enum WorkloadError {
    #[error("MiniWarehouse scale must contain at least one warehouse")]
    ZeroWarehouses,
    #[error("MiniWarehouse scale must contain at least one district per warehouse")]
    ZeroDistricts,
    #[error("MiniWarehouse scale must contain at least one customer per district")]
    ZeroCustomers,
    #[error("MiniWarehouse scale must contain at least one item per warehouse")]
    ZeroItems,
    #[error("invalid order-line range {min}..={max}")]
    InvalidOrderLineRange { min: u32, max: u32 },
    #[error(
        "maximum order-line count {max_order_lines} exceeds item domain {items_per_warehouse}"
    )]
    TooManyOrderLinesForItemDomain {
        max_order_lines: u32,
        items_per_warehouse: u64,
    },
    #[error("maximum order-line quantity must be greater than zero")]
    ZeroOrderLineQuantity,
    #[error("initial stock quantity must be greater than zero")]
    ZeroInitialStock,
    #[error("steady-state transaction mix must contain at least one operation")]
    EmptyTransactionMix,
    #[error("{field}={value} exceeds 10,000 basis points")]
    BasisPointsOutOfRange { field: &'static str, value: u16 },
    #[error(
        "hot warehouse {hot_warehouse_id} is outside configured warehouse range 1..={warehouse_count}"
    )]
    InvalidHotWarehouse {
        hot_warehouse_id: u64,
        warehouse_count: u64,
    },
    #[error("transaction-id space exhausted")]
    TransactionIdExhausted,
    #[error("history-id space exhausted")]
    HistoryIdExhausted,
    #[error("order-id space exhausted for warehouse {warehouse_id}, district {district_id}")]
    OrderIdExhausted { warehouse_id: u64, district_id: u64 },
    #[error("failed to serialize MiniWarehouse execute message: {0}")]
    SerializeMessage(cosmwasm_std::StdError),
    #[error("expected an execute request")]
    ExpectedExecuteRequest,
    #[error("failed to decode MiniWarehouse execute message: {0}")]
    DecodeMessage(serde_json::Error),
}
