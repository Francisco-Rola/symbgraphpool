use std::collections::BTreeSet;

use thiserror::Error;

use crate::block::ProducedBlock;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionWave {
    pub transaction_indices: Vec<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ExecutionDependencyClass {
    Soft,
    Hard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExecutionDependency {
    pub predecessor_index: usize,
    pub successor_index: usize,
    pub class: ExecutionDependencyClass,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExecutionPlan {
    pub transaction_count: usize,
    /// Dependency levels retained for diagnostics and theoretical parallelism. The speculative
    /// executor does not impose a global barrier between these levels.
    pub waves: Vec<ExecutionWave>,
    /// Pairwise execution-order constraints. A successor becomes runnable as soon as all of its
    /// predecessors complete and publish their speculative state versions.
    pub dependencies: Vec<ExecutionDependency>,
}

impl ExecutionPlan {
    pub fn validate(&self) -> Result<(), SchedulingError> {
        let mut seen = BTreeSet::new();
        let mut level_by_transaction = vec![None; self.transaction_count];
        for (level_index, wave) in self.waves.iter().enumerate() {
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
                level_by_transaction[index] = Some(level_index);
            }
        }
        if seen.len() != self.transaction_count {
            return Err(SchedulingError::MissingTransactions {
                scheduled: seen.len(),
                transaction_count: self.transaction_count,
            });
        }

        let mut dependencies = BTreeSet::new();
        for dependency in &self.dependencies {
            if dependency.predecessor_index >= self.transaction_count {
                return Err(SchedulingError::OutOfBounds {
                    index: dependency.predecessor_index,
                    transaction_count: self.transaction_count,
                });
            }
            if dependency.successor_index >= self.transaction_count {
                return Err(SchedulingError::OutOfBounds {
                    index: dependency.successor_index,
                    transaction_count: self.transaction_count,
                });
            }
            if dependency.predecessor_index >= dependency.successor_index {
                return Err(SchedulingError::NonCanonicalDependency {
                    predecessor: dependency.predecessor_index,
                    successor: dependency.successor_index,
                });
            }
            let predecessor_level = level_by_transaction[dependency.predecessor_index]
                .expect("plan completeness checked before dependencies");
            let successor_level = level_by_transaction[dependency.successor_index]
                .expect("plan completeness checked before dependencies");
            if predecessor_level >= successor_level {
                return Err(SchedulingError::DependencyLevelViolation {
                    predecessor: dependency.predecessor_index,
                    successor: dependency.successor_index,
                    predecessor_level,
                    successor_level,
                });
            }
            if !dependencies.insert((dependency.predecessor_index, dependency.successor_index)) {
                return Err(SchedulingError::DuplicateDependency {
                    predecessor: dependency.predecessor_index,
                    successor: dependency.successor_index,
                });
            }
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
            dependencies: Vec::new(),
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
    #[error("execution dependency {predecessor} -> {successor} does not follow canonical order")]
    NonCanonicalDependency {
        predecessor: usize,
        successor: usize,
    },
    #[error("execution dependency {predecessor} -> {successor} must advance scheduler levels, got {predecessor_level} -> {successor_level}")]
    DependencyLevelViolation {
        predecessor: usize,
        successor: usize,
        predecessor_level: usize,
        successor_level: usize,
    },
    #[error("execution dependency {predecessor} -> {successor} appears more than once")]
    DuplicateDependency {
        predecessor: usize,
        successor: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn execution_dependency_must_advance_diagnostic_level() {
        let plan = ExecutionPlan {
            transaction_count: 2,
            waves: vec![ExecutionWave {
                transaction_indices: vec![0, 1],
            }],
            dependencies: vec![ExecutionDependency {
                predecessor_index: 0,
                successor_index: 1,
                class: ExecutionDependencyClass::Hard,
            }],
        };
        assert!(matches!(
            plan.validate(),
            Err(SchedulingError::DependencyLevelViolation { .. })
        ));
    }
}
