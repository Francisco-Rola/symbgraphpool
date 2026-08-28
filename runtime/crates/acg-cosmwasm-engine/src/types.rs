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

impl BlockContext {
    /// Whether a speculative receipt produced under `self` can be reused under `other` when its
    /// concrete read dependencies still validate.
    ///
    /// Canonical transaction position is intentionally excluded. ConflictLab treats contracts
    /// whose semantics depend on `Env.transaction.index` as outside the reusable-receipt model;
    /// for ordinary state-driven contracts, block reordering must not invalidate an otherwise
    /// valid receipt. Height, time, and chain identity remain part of the semantic environment and
    /// therefore must match exactly.
    pub fn receipt_reuse_compatible_with(&self, other: &Self) -> bool {
        self.height == other.height
            && self.time_nanos == other.time_nanos
            && self.chain_id == other.chain_id
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
    /// One canonical transaction translated into an ordered atomic bundle of native calls.
    ///
    /// This request form exists so the same split-phase speculative executor used by the common
    /// benchmark harness can execute the real Vegeta S3 multi-call CosmWasm translation without
    /// collapsing a source transaction into independent child transactions. `source_failed` means
    /// the source transaction reverted: attempted accesses are retained as reverted evidence, no
    /// writes are committed, and the first translated native call failure is tolerated.
    Bundle {
        transaction_id: TransactionId,
        calls: Vec<ScopedBundleCall>,
        source_failed: bool,
    },
}

pub type NativeResponse = Response<Empty>;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BundleCall {
    Execute {
        sender: Address,
        contract: Address,
        funds: Vec<Coin>,
        msg: Binary,
    },
    Query {
        contract: Address,
        msg: Binary,
    },
    BankSend {
        from: Address,
        to: Address,
        coins: Vec<Coin>,
    },
    /// Deterministic CPU-only benchmark work. This never reads or writes chain state and exists
    /// only to restore source-workload computational intensity in controlled experiments.
    DeterministicCompute {
        iterations: u64,
    },
    Noop,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScopedBundleCall {
    pub call: BundleCall,
    /// Top-most source callTracer frame whose state was reverted while the outer transaction
    /// continued successfully. Calls sharing one scope ID are executed on one nested overlay.
    pub source_revert_scope: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleQueryResult {
    pub call_index: usize,
    pub contract: Address,
    pub data: Binary,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleCallFailure {
    pub call_index: usize,
    pub error: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleCallAccessSpan {
    pub call_index: usize,
    /// Inclusive index into BundleExecutionOutcome::accesses.
    pub access_start: usize,
    /// Exclusive index into BundleExecutionOutcome::accesses.
    pub access_end: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleRevertedScopeOutcome {
    pub scope_id: u64,
    pub first_call_index: usize,
    pub last_call_index: usize,
    pub failure: Option<BundleCallFailure>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleExecutionOutcome {
    pub transaction_id: TransactionId,
    pub events: Vec<Event>,
    pub query_results: Vec<BundleQueryResult>,
    pub accesses: Vec<AccessRecord>,
    /// Access ranges produced by each top-level bundle call. Nested CosmWasm accesses remain
    /// inside the originating call's span.
    pub call_access_spans: Vec<BundleCallAccessSpan>,
    pub created_contracts: Vec<Address>,
    pub committed: bool,
    /// Present only for tolerant top-level reverted execution. Canonical/strict bundle execution
    /// still returns the original EngineError immediately.
    pub failure: Option<BundleCallFailure>,
    /// Nested source call-frame scopes that reverted while an otherwise successful transaction
    /// continued. Their writes are discarded but their accesses are retained with reverted=true.
    pub reverted_scopes: Vec<BundleRevertedScopeOutcome>,
}

impl ExecutionRequest {
    pub fn transaction_id(&self) -> TransactionId {
        match self {
            Self::Instantiate { transaction_id, .. }
            | Self::Execute { transaction_id, .. }
            | Self::Bundle { transaction_id, .. } => *transaction_id,
        }
    }
}
