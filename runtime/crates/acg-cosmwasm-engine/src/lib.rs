//! Minimal deterministic CosmWasm execution engine for research and benchmarking.
//!
//! The crate intentionally keeps consensus, staking, IBC, governance and chain networking
//! outside the engine. It provides contract code/instance registries, transactional storage,
//! native bank balances, nested Wasm calls, smart queries and execution access traces.

mod api;
mod cache;
mod engine;
mod error;
mod native;
mod querier;
mod state;
mod storage;
mod types;

pub use crate::cache::{WasmCacheConfig, WasmCacheMetrics};
pub use crate::engine::{CosmWasmEngine, EngineConfig};
pub use crate::error::{EngineError, EngineResult};
pub use crate::native::{NativeCallContext, NativeContract};
pub use crate::types::{
    AccessKind, AccessRecord, Address, BlockContext, CodeChecksum, CodeId, CodeKind, CodeMetadata,
    ContractMetadata, ExecutionOutcome, ExecutionRequest, NativeResponse, QueryOutcome,
    TransactionId,
};
