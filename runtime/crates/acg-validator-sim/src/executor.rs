use acg_cosmwasm_engine::{
    CanonicalTransaction, CanonicalTxDisposition, CosmWasmEngine, EngineError, ExecutionOutcome,
    ParallelExecutionConfig, PostConsensusTimings, PredictionMatchMetrics,
    PreparedSpeculativeBlock, ReconciliationDependencyEvidence, SpeculativeDependency,
    SpeculativeDependencyClass, SpeculativeExecutionMetrics, SpeculativeWave,
    SplitPhaseSpeculativeBlockOutcome, StateSnapshot, TransactionId, ValidationOutcome,
};
use thiserror::Error;

use crate::block::ProducedBlock;
use crate::scheduler::{ExecutionDependencyClass, ExecutionPlan, SchedulingError};

#[derive(Debug)]
pub struct TransactionExecution {
    pub transaction_index: usize,
    pub transaction_id: TransactionId,
    pub result: Result<ExecutionOutcome, EngineError>,
}

#[derive(Debug)]
pub struct BlockExecutionReport {
    pub block_height: u64,
    pub block_time_nanos: u64,
    pub transactions: Vec<TransactionExecution>,
}

impl BlockExecutionReport {
    pub fn successful(&self) -> usize {
        self.transactions
            .iter()
            .filter(|execution| execution.result.is_ok())
            .count()
    }

    pub fn failed(&self) -> usize {
        self.transactions.len().saturating_sub(self.successful())
    }
}

pub trait BlockExecutor: Send + Sync {
    fn execute(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<BlockExecutionReport, BlockExecutionError>;
}

#[derive(Clone)]
pub struct SerialBlockExecutor {
    engine: CosmWasmEngine,
}

impl SerialBlockExecutor {
    pub fn new(engine: CosmWasmEngine) -> Self {
        Self { engine }
    }

    pub fn engine(&self) -> &CosmWasmEngine {
        &self.engine
    }

    pub fn execute(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<BlockExecutionReport, BlockExecutionError> {
        plan.validate()?;
        for (wave_index, wave) in plan.waves.iter().enumerate() {
            if wave.transaction_indices.len() > 1 {
                return Err(BlockExecutionError::ParallelWaveUnsupported {
                    wave_index,
                    width: wave.transaction_indices.len(),
                });
            }
        }

        let mut executions = Vec::with_capacity(block.transactions.len());
        for wave in &plan.waves {
            let transaction_index = wave.transaction_indices[0];
            let pending = &block.transactions[transaction_index];
            let mut context = block.context.clone();
            context.transaction_index =
                Some(u32::try_from(transaction_index).map_err(|_| {
                    BlockExecutionError::TransactionIndexOverflow(transaction_index)
                })?);
            let result = self
                .engine
                .execute_request(context, pending.request.clone());
            executions.push(TransactionExecution {
                transaction_index,
                transaction_id: pending.transaction_id(),
                result,
            });
        }

        Ok(BlockExecutionReport {
            block_height: block.context.height,
            block_time_nanos: block.context.time_nanos,
            transactions: executions,
        })
    }
}

/// Per-transaction reconciliation metadata retained for benchmark diagnostics.
///
/// This is observational only: canonical correctness remains entirely inside the engine's
/// dependency validator. Keeping the disposition and concrete validation outcome allows benchmark
/// harnesses to explain why speculative receipts were rejected without re-running validation.
#[derive(Clone, Debug)]
pub struct ReconciliationTransactionDiagnostic {
    pub transaction_index: usize,
    pub transaction_id: TransactionId,
    pub disposition: CanonicalTxDisposition,
    pub validation: Option<ValidationOutcome>,
    /// Canonical execution time paid because speculation could not be reused. Zero for reused
    /// receipts. Brick 5D consumes this only as adaptive cost evidence.
    pub reexecution_duration: std::time::Duration,
}

/// Post-consensus report for split-phase dependency/MVCC reconciliation.
#[derive(Debug)]
pub struct SplitPhaseSpeculativeExecutionReport {
    pub block: BlockExecutionReport,
    pub speculative: SpeculativeExecutionMetrics,
    pub prediction: PredictionMatchMetrics,
    pub timings: PostConsensusTimings,
    pub reconciliation: Vec<ReconciliationTransactionDiagnostic>,
    pub dependency_evidence: Vec<ReconciliationDependencyEvidence>,
}

#[derive(Clone)]
pub struct SpeculativeParallelBlockExecutor {
    engine: CosmWasmEngine,
    config: ParallelExecutionConfig,
}

impl SpeculativeParallelBlockExecutor {
    pub fn new(engine: CosmWasmEngine, config: ParallelExecutionConfig) -> Self {
        Self { engine, config }
    }

    pub fn engine(&self) -> &CosmWasmEngine {
        &self.engine
    }

    pub fn config(&self) -> ParallelExecutionConfig {
        self.config
    }

    /// Pre-consensus phase: execute the predicted block through its dependency ready-DAG without canonical mutation.
    pub fn prepare(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<PreparedSpeculativeBlock, BlockExecutionError> {
        let snapshot = self.engine.snapshot();
        self.prepare_from_snapshot(&snapshot, block, plan)
    }

    /// Pre-consensus dependency-driven phase starting from an explicit predecessor snapshot.
    pub fn prepare_from_snapshot(
        &self,
        snapshot: &StateSnapshot,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<PreparedSpeculativeBlock, BlockExecutionError> {
        let (canonical_transactions, speculative_waves, dependencies) =
            split_phase_inputs(block, plan)?;
        Ok(self.engine.preexecute_dependency_plan_from_snapshot(
            snapshot,
            self.config,
            canonical_transactions,
            speculative_waves,
            dependencies,
        )?)
    }

    /// Build a concrete-access report from successful pre-consensus receipts without executing
    /// anything again. This is used by adaptive feedback so symbolic hard relationships can learn
    /// from the same speculative executions that produced the receipts.
    pub fn pre_execution_report(
        &self,
        block: &ProducedBlock,
        prepared: &PreparedSpeculativeBlock,
    ) -> Result<BlockExecutionReport, BlockExecutionError> {
        if prepared.predicted_transactions.len() != block.transactions.len()
            || prepared.receipts.len() != block.transactions.len()
        {
            return Err(BlockExecutionError::TransactionCountMismatch {
                plan: prepared.receipts.len(),
                block: block.transactions.len(),
            });
        }
        let mut transactions = Vec::with_capacity(prepared.receipts.len());
        for (transaction_index, receipt) in prepared.receipts.iter().enumerate() {
            let expected = block.transactions[transaction_index].transaction_id();
            if receipt.transaction_id != expected {
                return Err(BlockExecutionError::Engine(EngineError::InvalidConfiguration(
                    format!(
                        "prepared receipt at index {transaction_index} has transaction ID {}, expected {}",
                        receipt.transaction_id.0, expected.0
                    ),
                )));
            }
            let Some(outcome) = receipt.status.outcome() else {
                continue;
            };
            transactions.push(TransactionExecution {
                transaction_index,
                transaction_id: receipt.transaction_id,
                result: Ok(ExecutionOutcome {
                    transaction_id: receipt.transaction_id,
                    contract: outcome.contract.clone(),
                    events: outcome.events.clone(),
                    data: outcome.data.clone(),
                    accesses: receipt.accesses.clone(),
                    created_contracts: outcome.created_contracts.clone(),
                }),
            });
        }
        Ok(BlockExecutionReport {
            block_height: block.context.height,
            block_time_nanos: block.context.time_nanos,
            transactions,
        })
    }

    /// Post-consensus phase: validate matching receipts in canonical order, replay stale/missing
    /// work, and commit the decided block.
    pub fn validate_prepared(
        &self,
        block: &ProducedBlock,
        prepared: PreparedSpeculativeBlock,
    ) -> Result<SplitPhaseSpeculativeExecutionReport, BlockExecutionError> {
        let canonical_transactions = canonical_transactions(block)?;
        let outcome = self
            .engine
            .reconcile_prepared_block(canonical_transactions, prepared)?;
        Ok(split_phase_report(block, outcome))
    }

    pub fn execute(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<BlockExecutionReport, BlockExecutionError> {
        let prepared = self.prepare(block, plan)?;
        Ok(self.validate_prepared(block, prepared)?.block)
    }
}

impl BlockExecutor for SpeculativeParallelBlockExecutor {
    fn execute(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<BlockExecutionReport, BlockExecutionError> {
        SpeculativeParallelBlockExecutor::execute(self, block, plan)
    }
}

fn canonical_transactions(
    block: &ProducedBlock,
) -> Result<Vec<CanonicalTransaction>, BlockExecutionError> {
    block
        .transactions
        .iter()
        .enumerate()
        .map(|(transaction_index, pending)| {
            let mut context = block.context.clone();
            context.transaction_index =
                Some(u32::try_from(transaction_index).map_err(|_| {
                    BlockExecutionError::TransactionIndexOverflow(transaction_index)
                })?);
            Ok(CanonicalTransaction::new(context, pending.request.clone()))
        })
        .collect()
}

type SplitPhaseInputs = (
    Vec<CanonicalTransaction>,
    Vec<SpeculativeWave>,
    Vec<SpeculativeDependency>,
);

fn split_phase_inputs(
    block: &ProducedBlock,
    plan: &ExecutionPlan,
) -> Result<SplitPhaseInputs, BlockExecutionError> {
    plan.validate()?;
    if plan.transaction_count != block.transactions.len() {
        return Err(BlockExecutionError::TransactionCountMismatch {
            plan: plan.transaction_count,
            block: block.transactions.len(),
        });
    }
    let canonical_transactions = canonical_transactions(block)?;
    let speculative_waves = plan
        .waves
        .iter()
        .map(|wave| {
            SpeculativeWave::new(
                wave.transaction_indices
                    .iter()
                    .map(|&transaction_index| {
                        block.transactions[transaction_index].transaction_id()
                    })
                    .collect(),
            )
        })
        .collect();
    let dependencies = plan
        .dependencies
        .iter()
        .map(|dependency| SpeculativeDependency {
            predecessor: block.transactions[dependency.predecessor_index].transaction_id(),
            successor: block.transactions[dependency.successor_index].transaction_id(),
            class: match dependency.class {
                ExecutionDependencyClass::Soft => SpeculativeDependencyClass::Soft,
                ExecutionDependencyClass::Hard => SpeculativeDependencyClass::Hard,
            },
        })
        .collect();
    Ok((canonical_transactions, speculative_waves, dependencies))
}

fn split_phase_report(
    block: &ProducedBlock,
    outcome: SplitPhaseSpeculativeBlockOutcome,
) -> SplitPhaseSpeculativeExecutionReport {
    let mut reconciliation = Vec::with_capacity(outcome.transactions.len());
    let transactions = outcome
        .transactions
        .into_iter()
        .enumerate()
        .map(|(transaction_index, result)| {
            reconciliation.push(ReconciliationTransactionDiagnostic {
                transaction_index,
                transaction_id: result.transaction_id,
                disposition: result.disposition,
                validation: result.validation.clone(),
                reexecution_duration: result.reexecution_duration,
            });
            TransactionExecution {
                transaction_index,
                transaction_id: result.transaction_id,
                result: result.result,
            }
        })
        .collect();
    SplitPhaseSpeculativeExecutionReport {
        block: BlockExecutionReport {
            block_height: block.context.height,
            block_time_nanos: block.context.time_nanos,
            transactions,
        },
        speculative: outcome.speculative,
        prediction: outcome.prediction,
        timings: outcome.timings,
        reconciliation,
        dependency_evidence: outcome.dependency_evidence,
    }
}

#[derive(Debug, Error)]
pub enum BlockExecutionError {
    #[error(transparent)]
    InvalidPlan(#[from] SchedulingError),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(
        "execution plan transaction count {plan} does not match block transaction count {block}"
    )]
    TransactionCountMismatch { plan: usize, block: usize },
    #[error(
        "wave {wave_index} has width {width}; parallel execution is disabled until speculative validation is implemented"
    )]
    ParallelWaveUnsupported { wave_index: usize, width: usize },
    #[error("transaction index {0} cannot be represented as a CosmWasm u32 index")]
    TransactionIndexOverflow(usize),
}

impl BlockExecutor for SerialBlockExecutor {
    fn execute(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<BlockExecutionReport, BlockExecutionError> {
        SerialBlockExecutor::execute(self, block, plan)
    }
}
