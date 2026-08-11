use std::time::Duration;

use acg_cosmwasm_engine::BlockContext;
use thiserror::Error;

use crate::mempool::{Mempool, PendingTransaction};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProducedBlock {
    pub context: BlockContext,
    pub transactions: Vec<PendingTransaction>,
}

pub trait BlockSelectionPolicy: Send + Sync {
    fn select(&self, mempool: &Mempool, limit: usize) -> Vec<PendingTransaction>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FifoSelectionPolicy;

impl BlockSelectionPolicy for FifoSelectionPolicy {
    fn select(&self, mempool: &Mempool, limit: usize) -> Vec<PendingTransaction> {
        mempool.drain_fifo(limit)
    }
}

#[derive(Clone, Debug)]
pub struct BlockProducerConfig {
    pub block_interval: Duration,
    pub chain_id: String,
    /// Height assigned to the first produced block.
    pub first_block_height: u64,
    /// Virtual timestamp assigned to the first produced block.
    pub first_block_time_nanos: u64,
    /// None drains all currently admitted transactions into the next block.
    pub max_transactions_per_block: Option<usize>,
}

impl Default for BlockProducerConfig {
    fn default() -> Self {
        Self {
            block_interval: Duration::from_secs(2),
            chain_id: "acg-local".to_owned(),
            first_block_height: 1,
            first_block_time_nanos: 2_000_000_000,
            max_transactions_per_block: None,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum BlockProducerError {
    #[error("block interval must be greater than zero")]
    ZeroBlockInterval,
    #[error("chain ID must not be empty")]
    EmptyChainId,
}

pub struct BlockProducer<P = FifoSelectionPolicy> {
    config: BlockProducerConfig,
    selection: P,
    next_height: u64,
    next_time_nanos: u64,
}

impl BlockProducer<FifoSelectionPolicy> {
    pub fn fifo(config: BlockProducerConfig) -> Result<Self, BlockProducerError> {
        Self::new(config, FifoSelectionPolicy)
    }

    /// Preview the exact FIFO prefix and block context that `produce_next` would use, without
    /// draining the mempool or advancing height/time.
    pub fn preview_next(&self, mempool: &Mempool) -> ProducedBlock {
        let limit = self.config.max_transactions_per_block.unwrap_or(usize::MAX);
        ProducedBlock {
            context: BlockContext {
                height: self.next_height,
                time_nanos: self.next_time_nanos,
                chain_id: self.config.chain_id.clone(),
                transaction_index: None,
            },
            transactions: mempool.peek_fifo(limit),
        }
    }
}

impl<P: BlockSelectionPolicy> BlockProducer<P> {
    pub fn new(config: BlockProducerConfig, selection: P) -> Result<Self, BlockProducerError> {
        if config.block_interval.is_zero() {
            return Err(BlockProducerError::ZeroBlockInterval);
        }
        if config.chain_id.is_empty() {
            return Err(BlockProducerError::EmptyChainId);
        }
        let next_height = config.first_block_height;
        let next_time_nanos = config.first_block_time_nanos;
        Ok(Self {
            config,
            selection,
            next_height,
            next_time_nanos,
        })
    }

    pub fn next_block_time_nanos(&self) -> u64 {
        self.next_time_nanos
    }

    pub fn config(&self) -> &BlockProducerConfig {
        &self.config
    }

    pub fn produce_next(&mut self, mempool: &Mempool) -> ProducedBlock {
        let limit = self.config.max_transactions_per_block.unwrap_or(usize::MAX);
        let transactions = self.selection.select(mempool, limit);
        let context = BlockContext {
            height: self.next_height,
            time_nanos: self.next_time_nanos,
            chain_id: self.config.chain_id.clone(),
            transaction_index: None,
        };
        self.next_height = self.next_height.saturating_add(1);
        self.next_time_nanos = self
            .next_time_nanos
            .saturating_add(duration_nanos(self.config.block_interval));
        ProducedBlock {
            context,
            transactions,
        }
    }
}

fn duration_nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}
