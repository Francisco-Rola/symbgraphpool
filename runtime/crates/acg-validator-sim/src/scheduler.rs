use std::collections::BTreeSet;

use thiserror::Error;

use crate::block::ProducedBlock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionWave {
    pub transaction_indices: Vec<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionPlan {
    pub transaction_count: usize,
    pub waves: Vec<ExecutionWave>,
}

impl ExecutionPlan {
    pub fn validate(&self) -> Result<(), SchedulingError> {
        let mut seen = BTreeSet::new();
        for wave in &self.waves {
            if wave.transaction_indices.is_empty() {
                return Err(SchedulingError::EmptyWave);
            }
            for &index in &wave.transaction_indices {
                if index >= self.transaction_count {
                    return Err(SchedulingError::OutOfBounds {
                        index,
                        transaction_count: self.transaction_count,
                    });
                }
                if !seen.insert(index) {
                    return Err(SchedulingError::DuplicateTransaction(index));
                }
            }
        }
        if seen.len() != self.transaction_count {
            return Err(SchedulingError::MissingTransactions {
                scheduled: seen.len(),
                transaction_count: self.transaction_count,
            });
        }
        Ok(())
    }
}

pub trait BlockScheduler: Send + Sync {
    fn schedule(&self, block: &ProducedBlock) -> Result<ExecutionPlan, SchedulingError>;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FifoScheduler;

impl BlockScheduler for FifoScheduler {
    fn schedule(&self, block: &ProducedBlock) -> Result<ExecutionPlan, SchedulingError> {
        Ok(ExecutionPlan {
            transaction_count: block.transactions.len(),
            waves: (0..block.transactions.len())
                .map(|index| ExecutionWave {
                    transaction_indices: vec![index],
                })
                .collect(),
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SchedulingError {
    #[error("execution plan contains an empty wave")]
    EmptyWave,
    #[error("transaction index {index} is out of bounds for {transaction_count} transactions")]
    OutOfBounds {
        index: usize,
        transaction_count: usize,
    },
    #[error("transaction index {0} appears more than once in the execution plan")]
    DuplicateTransaction(usize),
    #[error("execution plan schedules {scheduled} of {transaction_count} transactions")]
    MissingTransactions {
        scheduled: usize,
        transaction_count: usize,
    },
}
