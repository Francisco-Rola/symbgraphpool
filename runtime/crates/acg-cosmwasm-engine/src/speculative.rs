use std::{sync::Arc, time::Duration};

use cosmwasm_std::{Binary, Event};

use crate::engine::EngineCore;
use crate::error::EngineError;
use crate::state::SharedWorld;
use crate::types::{
    AccessRecord, Address, BlockContext, ContractMetadata, ExecutionOutcome, ExecutionRequest,
    TransactionId,
};
use crate::validation::ValidationOutcome;

/// Immutable transaction-visible snapshot used as the base for speculative execution.
///
/// The snapshot owns a detached copy of world state. Later canonical mutations therefore do not
/// change what a transaction executing against this snapshot observes.
#[derive(Clone)]
pub struct StateSnapshot {
    pub(crate) core: Arc<EngineCore>,
    pub(crate) state: SharedWorld,
}

impl StateSnapshot {
    /// Compare the complete contract registry, storage, and bank state of two snapshots.
    ///
    /// Engine code registries are immutable execution metadata and are intentionally not part of
    /// this world-state comparison.
    pub fn same_world_state(&self, other: &Self) -> bool {
        let left = self.state.read();
        let right = other.state.read();
        (*left).eq(&*right)
    }
}

/// A state read whose observed value can affect whether a speculative result remains reusable.
///
/// Point reads record the value observed from the immutable base snapshot. Reads satisfied by a
/// transaction-local write are intentionally omitted because they do not depend on predecessor
/// state. Range reads retain the unmasked base entries so later validation can detect insertions,
/// removals, and value changes (phantoms included).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadDependency {
    ContractMetadata {
        address: Address,
        metadata: Option<ContractMetadata>,
    },
    Storage {
        contract: Address,
        key: Vec<u8>,
        value: Option<Vec<u8>>,
    },
    StorageRange {
        contract: Address,
        start: Option<Vec<u8>>,
        end: Option<Vec<u8>>,
        base_entries: Vec<(Vec<u8>, Vec<u8>)>,
        masked_keys: Vec<Vec<u8>>,
    },
    BankBalance {
        address: Address,
        denom: String,
        amount: u128,
    },
    BankAllBalances {
        address: Address,
        base_balances: Vec<(String, u128)>,
        masked_denoms: Vec<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StorageWrite {
    pub contract: Address,
    pub key: Vec<u8>,
    /// `None` represents a removal.
    pub value: Option<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BalanceWrite {
    pub address: Address,
    pub denom: String,
    /// Zero represents removal of the canonical balance entry.
    pub amount: u128,
}

/// Detached state mutations produced by a successful speculative transaction.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StateWriteSet {
    pub storage: Vec<StorageWrite>,
    pub balances: Vec<BalanceWrite>,
    pub created_contracts: Vec<ContractMetadata>,
}

impl StateWriteSet {
    pub fn is_empty(&self) -> bool {
        self.storage.is_empty() && self.balances.is_empty() && self.created_contracts.is_empty()
    }
}

#[derive(Clone, Debug)]
pub struct SpeculativeExecutionOutcome {
    pub contract: Address,
    pub events: Vec<Event>,
    pub data: Option<Binary>,
    pub created_contracts: Vec<Address>,
}

#[derive(Debug)]
pub enum SpeculativeExecutionStatus {
    Succeeded(SpeculativeExecutionOutcome),
    Failed(EngineError),
}

impl SpeculativeExecutionStatus {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Succeeded(_))
    }

    pub fn outcome(&self) -> Option<&SpeculativeExecutionOutcome> {
        match self {
            Self::Succeeded(outcome) => Some(outcome),
            Self::Failed(_) => None,
        }
    }

    pub fn error(&self) -> Option<&EngineError> {
        match self {
            Self::Succeeded(_) => None,
            Self::Failed(error) => Some(error),
        }
    }
}

/// Brick 5E validator-local timing for one speculative execution.
///
/// Offsets are measured from the dependency worker phase origin. They are observational only and
/// never participate in validation, replay correctness, or consensus-visible state.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpeculativeExecutionTiming {
    pub started_after_phase: Duration,
    pub completed_after_phase: Duration,
    pub service_duration: Duration,
}

/// Detached result of one transaction executed against a [`StateSnapshot`].
///
/// A failed top-level transaction has an empty commit-ready `write_set`, while `accesses` retains
/// its attempted reads and writes marked as reverted. `read_dependencies` are retained on failure
/// because the observed predecessor state may have determined that failure.
#[derive(Debug)]
pub struct SpeculativeTxResult {
    pub transaction_id: TransactionId,
    /// Exact block context used to produce this receipt. Reuse is only valid for the same context.
    pub block: BlockContext,
    /// Exact request used to produce this receipt. Reuse is only valid for the same request.
    pub request: ExecutionRequest,
    pub status: SpeculativeExecutionStatus,
    pub accesses: Vec<AccessRecord>,
    pub read_dependencies: Vec<ReadDependency>,
    pub write_set: StateWriteSet,
    /// Brick 5E local execution timing used only for cost estimation and experiment records.
    pub execution_timing: SpeculativeExecutionTiming,
    pub(crate) engine_identity: Arc<()>,
}

impl SpeculativeTxResult {
    pub fn is_success(&self) -> bool {
        self.status.is_success()
    }
}

/// One transaction in canonical block order, including the exact block context visible to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CanonicalTransaction {
    pub block: BlockContext,
    pub request: ExecutionRequest,
}

impl CanonicalTransaction {
    pub fn new(block: BlockContext, request: ExecutionRequest) -> Self {
        Self { block, request }
    }

    pub fn transaction_id(&self) -> TransactionId {
        self.request.transaction_id()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanonicalTxDisposition {
    /// A valid speculative receipt was committed without re-executing the contract.
    ReusedSpeculative,
    /// A speculative receipt was invalid and the transaction was re-executed canonically.
    Replayed,
    /// No speculative receipt was supplied; the transaction executed canonically once.
    Canonical,
}

#[derive(Debug)]
pub struct CanonicalTxResult {
    pub transaction_id: TransactionId,
    pub disposition: CanonicalTxDisposition,
    /// Present when a speculative receipt existed. Valid receipts have an empty conflict list.
    pub validation: Option<ValidationOutcome>,
    /// Wall time spent canonically executing this transaction during reconciliation.
    ///
    /// This is non-zero only for a replayed speculative receipt or a transaction that had no
    /// matching pre-consensus receipt. Reused receipts keep this at zero. Brick 5D uses the replay
    /// duration as cost evidence; it is observational and never participates in correctness.
    pub reexecution_duration: Duration,
    pub result: Result<ExecutionOutcome, EngineError>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpeculativeExecutionMetrics {
    pub speculative_results: u64,
    pub reused_results: u64,
    pub invalidated_results: u64,
    pub replayed_transactions: u64,
    pub canonical_transactions: u64,
}

impl SpeculativeExecutionMetrics {
    pub fn reuse_rate(&self) -> f64 {
        ratio(self.reused_results, self.speculative_results)
    }

    pub fn validation_failure_rate(&self) -> f64 {
        ratio(self.invalidated_results, self.speculative_results)
    }

    pub fn replay_rate(&self, block_transactions: usize) -> f64 {
        if block_transactions == 0 {
            0.0
        } else {
            self.replayed_transactions as f64 / block_transactions as f64
        }
    }
}

#[derive(Debug)]
pub struct SpeculativeBlockOutcome {
    pub transactions: Vec<CanonicalTxResult>,
    pub metrics: SpeculativeExecutionMetrics,
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}
