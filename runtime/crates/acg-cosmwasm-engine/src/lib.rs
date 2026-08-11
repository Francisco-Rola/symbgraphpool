//! Minimal deterministic CosmWasm execution engine for research and benchmarking.
//!
//! The crate intentionally keeps consensus, staking, IBC, governance and chain networking
//! outside the engine. It provides contract code/instance registries, transactional storage,
//! native bank balances, nested Wasm calls, smart queries and execution access traces.

mod api;
mod cache;
mod engine;
mod error;
mod mvcc;
mod native;
mod parallel;
mod querier;
mod speculative;
mod state;
mod storage;
mod types;
mod validation;

pub use crate::cache::{WasmCacheConfig, WasmCacheMetrics};
pub use crate::engine::{CosmWasmEngine, EngineConfig};
pub use crate::error::{EngineError, EngineResult};
pub use crate::native::{NativeCallContext, NativeContract};
pub use crate::parallel::{
    ContractExecutionDiagnostics, DependencyPreexecutionDiagnostics, ParallelExecutionConfig,
    ParallelSpeculativeExecutionMetrics, PostConsensusTimings, PredictionMatchMetrics,
    PreparedSpeculativeBlock, ReconciliationDependencyEvidence, SpeculativeDependency,
    SpeculativeDependencyClass, SpeculativeWave, SplitPhaseSpeculativeBlockOutcome,
};
pub use crate::speculative::{
    BalanceWrite, CanonicalTransaction, CanonicalTxDisposition, CanonicalTxResult, ReadDependency,
    SpeculativeBlockOutcome, SpeculativeExecutionMetrics, SpeculativeExecutionOutcome,
    SpeculativeExecutionStatus, SpeculativeTxResult, StateSnapshot, StateWriteSet, StorageWrite,
};
pub use crate::types::{
    AccessKind, AccessRecord, Address, BlockContext, CodeChecksum, CodeId, CodeKind, CodeMetadata,
    ContractMetadata, ExecutionOutcome, ExecutionRequest, NativeResponse, QueryOutcome,
    TransactionId,
};

pub use crate::validation::{ValidationConflict, ValidationOutcome};
