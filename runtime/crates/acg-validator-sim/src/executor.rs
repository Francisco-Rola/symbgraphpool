use acg_cosmwasm_engine::{CosmWasmEngine, EngineError, ExecutionOutcome, TransactionId};
use thiserror::Error;

use crate::block::ProducedBlock;
use crate::scheduler::{ExecutionPlan, SchedulingError};

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

#[derive(Debug, Error)]
pub enum BlockExecutionError {
    #[error(transparent)]
    InvalidPlan(#[from] SchedulingError),
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
