use acg_cosmwasm_engine::{CosmWasmEngine, ExecutionRequest};
use thiserror::Error;

use crate::block::{
    BlockProducer, BlockProducerConfig, BlockProducerError, FifoSelectionPolicy, ProducedBlock,
};
use crate::executor::{BlockExecutionError, BlockExecutionReport, SerialBlockExecutor};
use crate::mempool::{AdmissionReceipt, Mempool};
use crate::scheduler::{BlockScheduler, FifoScheduler, SchedulingError};

/// Default single-validator benchmark pipeline.
///
/// It exposes the components independently while providing a small end-to-end convenience API.
pub struct SingleValidatorRuntime<S = FifoScheduler> {
    mempool: Mempool,
    producer: BlockProducer<FifoSelectionPolicy>,
    scheduler: S,
    executor: SerialBlockExecutor,
}

impl SingleValidatorRuntime<FifoScheduler> {
    pub fn fifo(
        engine: CosmWasmEngine,
        producer_config: BlockProducerConfig,
    ) -> Result<Self, PipelineError> {
        Self::new(engine, producer_config, FifoScheduler)
    }
}

impl<S: BlockScheduler> SingleValidatorRuntime<S> {
    pub fn new(
        engine: CosmWasmEngine,
        producer_config: BlockProducerConfig,
        scheduler: S,
    ) -> Result<Self, PipelineError> {
        Ok(Self {
            mempool: Mempool::default(),
            producer: BlockProducer::fifo(producer_config)?,
            scheduler,
            executor: SerialBlockExecutor::new(engine),
        })
    }

    pub fn mempool(&self) -> &Mempool {
        &self.mempool
    }

    pub fn engine(&self) -> &CosmWasmEngine {
        self.executor.engine()
    }

    pub fn submit(&self, request: ExecutionRequest, admitted_at_nanos: u64) -> AdmissionReceipt {
        self.mempool.admit(request, admitted_at_nanos)
    }

    pub fn next_block_time_nanos(&self) -> u64 {
        self.producer.next_block_time_nanos()
    }

    pub fn produce_block(&mut self) -> ProducedBlock {
        self.producer.produce_next(&self.mempool)
    }

    pub fn execute_block(
        &self,
        block: &ProducedBlock,
    ) -> Result<BlockExecutionReport, PipelineError> {
        let plan = self.scheduler.schedule(block)?;
        Ok(self.executor.execute(block, &plan)?)
    }

    pub fn produce_and_execute(&mut self) -> Result<BlockExecutionReport, PipelineError> {
        let block = self.produce_block();
        self.execute_block(&block)
    }
}

#[derive(Debug, Error)]
pub enum PipelineError {
    #[error(transparent)]
    BlockProducer(#[from] BlockProducerError),
    #[error(transparent)]
    Scheduling(#[from] SchedulingError),
    #[error(transparent)]
    Execution(#[from] BlockExecutionError),
}
