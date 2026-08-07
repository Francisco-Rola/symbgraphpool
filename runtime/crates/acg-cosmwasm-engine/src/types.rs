use std::fmt;

use cosmwasm_std::{Binary, Coin, Empty, Event, Response};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Address(String);

impl Address {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Address {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl From<&str> for Address {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for Address {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CodeId(pub u64);

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CodeChecksum([u8; 32]);

impl CodeChecksum {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(self) -> String {
        hex::encode(self.0)
    }
}

impl fmt::Debug for CodeChecksum {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CodeChecksum")
            .field(&self.to_hex())
            .finish()
    }
}

impl fmt::Display for CodeChecksum {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.to_hex())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodeKind {
    Wasm,
    Native,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodeMetadata {
    pub code_id: CodeId,
    pub checksum: CodeChecksum,
    pub kind: CodeKind,
    pub label: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TransactionId(pub u64);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockContext {
    pub height: u64,
    pub time_nanos: u64,
    pub chain_id: String,
    pub transaction_index: Option<u32>,
}

impl Default for BlockContext {
    fn default() -> Self {
        Self {
            height: 1,
            time_nanos: 1_000_000_000,
            chain_id: "acg-local".to_owned(),
            transaction_index: Some(0),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContractMetadata {
    pub address: Address,
    pub code_id: CodeId,
    pub code_checksum: CodeChecksum,
    pub creator: Address,
    pub admin: Option<Address>,
    pub label: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessKind {
    StorageRead,
    StorageScan,
    StorageWrite,
    StorageRemove,
    BankRead,
    BankWrite,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AccessRecord {
    pub transaction_id: TransactionId,
    pub call_depth: u32,
    pub contract: Address,
    pub kind: AccessKind,
    pub key: Vec<u8>,
    pub range_end: Option<Vec<u8>>,
    pub value: Option<Vec<u8>>,
    pub reverted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionOutcome {
    pub transaction_id: TransactionId,
    pub contract: Address,
    pub events: Vec<Event>,
    pub data: Option<Binary>,
    pub accesses: Vec<AccessRecord>,
    pub created_contracts: Vec<Address>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueryOutcome {
    pub contract: Address,
    pub data: Binary,
    pub accesses: Vec<AccessRecord>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExecutionRequest {
    Instantiate {
        transaction_id: TransactionId,
        sender: Address,
        code_id: CodeId,
        admin: Option<Address>,
        label: String,
        funds: Vec<Coin>,
        msg: Binary,
    },
    Execute {
        transaction_id: TransactionId,
        sender: Address,
        contract: Address,
        funds: Vec<Coin>,
        msg: Binary,
    },
}

pub type NativeResponse = Response<Empty>;

impl ExecutionRequest {
    pub fn transaction_id(&self) -> TransactionId {
        match self {
            Self::Instantiate { transaction_id, .. } | Self::Execute { transaction_id, .. } => {
                *transaction_id
            }
        }
    }
}
