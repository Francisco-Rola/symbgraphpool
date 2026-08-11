use crate::speculative::{
    CanonicalTransaction, CanonicalTxResult, SpeculativeExecutionMetrics, SpeculativeTxResult,
    StateSnapshot,
};
use crate::types::TransactionId;

/// Bounded worker configuration for speculative wave execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParallelExecutionConfig {
    /// Maximum number of speculative transactions executing concurrently.
    pub workers: usize,
}

impl Default for ParallelExecutionConfig {
    fn default() -> Self {
        Self { workers: 1 }
    }
}

/// Pairwise dependency used by the dependency-driven speculative executor.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SpeculativeDependencyClass {
    Soft,
    Hard,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SpeculativeDependency {
    pub predecessor: TransactionId,
    pub successor: TransactionId,
    pub class: SpeculativeDependencyClass,
}

/// One scheduler level expressed in engine transaction identities.
///
/// Levels are retained for diagnostics and theoretical parallelism. The dependency-driven Brick-5
/// executor does not impose a global barrier between them; successors launch as soon as their
/// pairwise [`SpeculativeDependency`] predecessors complete.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SpeculativeWave {
    pub transaction_ids: Vec<TransactionId>,
}

impl SpeculativeWave {
    pub fn new(transaction_ids: Vec<TransactionId>) -> Self {
        Self { transaction_ids }
    }

    pub fn len(&self) -> usize {
        self.transaction_ids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.transaction_ids.is_empty()
    }
}

/// Scheduling/concurrency measurements retained for both the legacy wave executor and the
/// Brick-5C.6 dependency-driven executor. `wave_widths` describe scheduler levels, while
/// `dependency_count` describes the actual ready-DAG constraints used by split-phase execution.
#[derive(Clone, Debug, PartialEq)]
pub struct ParallelSpeculativeExecutionMetrics {
    pub workers: usize,
    pub wave_widths: Vec<usize>,
    pub speculative: SpeculativeExecutionMetrics,
    pub dependency_count: usize,
    pub hard_dependency_count: usize,
}

impl ParallelSpeculativeExecutionMetrics {
    pub fn wave_count(&self) -> usize {
        self.wave_widths.len()
    }

    pub fn max_wave_width(&self) -> usize {
        self.wave_widths.iter().copied().max().unwrap_or(0)
    }

    pub fn exposed_parallelism(&self) -> f64 {
        let transactions = self.speculative.speculative_results as usize;
        if transactions == 0 || self.wave_widths.is_empty() {
            0.0
        } else {
            transactions as f64 / self.wave_widths.len() as f64
        }
    }

    /// Legacy equal-cost worker-aware bound under strict wave barriers.
    ///
    /// Brick-5C.6 split-phase execution does not impose those barriers; callers measuring the
    /// dependency-driven path should use a DAG critical-path/work bound instead. This method is
    /// retained for Brick-5C compatibility and its legacy tests.
    pub fn equal_cost_theoretical_speedup(&self) -> f64 {
        let transactions = self.speculative.speculative_results as usize;
        let slots = self.equal_cost_worker_time_slots();
        if transactions == 0 || slots == 0 {
            0.0
        } else {
            transactions as f64 / slots as f64
        }
    }

    /// Idealized worker utilization implied by wave widths when every transaction has equal cost.
    pub fn equal_cost_worker_utilization_upper_bound(&self) -> f64 {
        let transactions = self.speculative.speculative_results as usize;
        let slots = self.equal_cost_worker_time_slots();
        if transactions == 0 || slots == 0 || self.workers == 0 {
            0.0
        } else {
            transactions as f64 / (slots * self.workers) as f64
        }
    }

    fn equal_cost_worker_time_slots(&self) -> usize {
        if self.workers == 0 {
            return 0;
        }
        self.wave_widths
            .iter()
            .map(|width| width.div_ceil(self.workers))
            .sum()
    }
}

#[derive(Debug)]
pub struct ParallelSpeculativeBlockOutcome {
    pub transactions: Vec<CanonicalTxResult>,
    pub metrics: ParallelSpeculativeExecutionMetrics,
}

/// Detached pre-consensus result for one predicted block.
///
/// The receipts were produced without mutating canonical state. `predicted_successor` remains a
/// diagnostic projection of the receipt stream. Brick 5C.5+ never uses it as the execution base
/// for the next block: next-block pre-execution waits for the predecessor's canonical commit and
/// snapshots the actual engine state.
pub struct PreparedSpeculativeBlock {
    pub predicted_transactions: Vec<CanonicalTransaction>,
    pub receipts: Vec<SpeculativeTxResult>,
    pub metrics: ParallelSpeculativeExecutionMetrics,
    pub(crate) predicted_successor: StateSnapshot,
}

impl PreparedSpeculativeBlock {
    pub fn predicted_transaction_count(&self) -> usize {
        self.predicted_transactions.len()
    }

    /// Advisory projected successor snapshot retained for diagnostics and experiments.
    ///
    /// Brick 5C.5 does not use this snapshot to execute the next block. Callers that experiment
    /// with chained speculation must still validate every derived receipt canonically.
    pub fn predicted_successor_snapshot(&self) -> StateSnapshot {
        self.predicted_successor.clone()
    }
}

/// How well a pre-consensus predicted block matched the post-consensus decided block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PredictionMatchMetrics {
    pub predicted_transactions: u64,
    pub decided_transactions: u64,
    pub matched_transactions: u64,
    pub discarded_predictions: u64,
    pub missing_predictions: u64,
}

impl PredictionMatchMetrics {
    pub fn precision(&self) -> f64 {
        ratio(self.matched_transactions, self.predicted_transactions)
    }

    pub fn coverage(&self) -> f64 {
        ratio(self.matched_transactions, self.decided_transactions)
    }
}

/// Detailed wall-clock breakdown of the post-consensus critical path.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PostConsensusTimings {
    pub receipt_matching: std::time::Duration,
    pub validation: std::time::Duration,
    pub replay_or_missing_execution: std::time::Duration,
    pub commit_reused: std::time::Duration,
    pub total: std::time::Duration,
}

/// One replay attribution emitted by canonical reconciliation.
///
/// `conflict_index` indexes the invalid receipt's [`crate::ValidationOutcome::conflicts`] slice.
/// The validator/runtime-feedback layer maps that concrete conflict to conflict-kind metadata.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReconciliationDependencyEvidence {
    pub predecessor_index: usize,
    pub transaction_index: usize,
    pub conflict_index: usize,
}

/// Post-consensus reconciliation result for a block that may have pre-consensus receipts.
#[derive(Debug)]
pub struct SplitPhaseSpeculativeBlockOutcome {
    pub transactions: Vec<CanonicalTxResult>,
    pub speculative: SpeculativeExecutionMetrics,
    pub prediction: PredictionMatchMetrics,
    pub timings: PostConsensusTimings,
    pub dependency_evidence: Vec<ReconciliationDependencyEvidence>,
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[cfg(test)]
mod tests {
    use super::{ParallelSpeculativeExecutionMetrics, SpeculativeWave};
    use crate::speculative::SpeculativeExecutionMetrics;
    use crate::types::TransactionId;

    #[test]
    fn speculative_wave_reports_width() {
        let wave = SpeculativeWave::new(vec![TransactionId(1), TransactionId(2)]);
        assert_eq!(wave.len(), 2);
        assert!(!wave.is_empty());
        assert!(SpeculativeWave::new(Vec::new()).is_empty());
    }

    #[test]
    fn equal_cost_metrics_account_for_worker_count_and_wave_barriers() {
        let metrics = ParallelSpeculativeExecutionMetrics {
            workers: 2,
            wave_widths: vec![5, 3],
            speculative: SpeculativeExecutionMetrics {
                speculative_results: 8,
                ..SpeculativeExecutionMetrics::default()
            },
            dependency_count: 0,
            hard_dependency_count: 0,
        };

        assert_eq!(metrics.wave_count(), 2);
        assert_eq!(metrics.max_wave_width(), 5);
        assert!((metrics.exposed_parallelism() - 4.0).abs() < f64::EPSILON);
        // ceil(5 / 2) + ceil(3 / 2) = 3 + 2 = 5 ideal worker-time slots.
        assert!((metrics.equal_cost_theoretical_speedup() - 1.6).abs() < f64::EPSILON);
        assert!((metrics.equal_cost_worker_utilization_upper_bound() - 0.8).abs() < f64::EPSILON);
    }

    #[test]
    fn empty_metrics_are_zero() {
        let metrics = ParallelSpeculativeExecutionMetrics {
            workers: 4,
            wave_widths: Vec::new(),
            speculative: SpeculativeExecutionMetrics::default(),
            dependency_count: 0,
            hard_dependency_count: 0,
        };
        assert_eq!(metrics.exposed_parallelism(), 0.0);
        assert_eq!(metrics.equal_cost_theoretical_speedup(), 0.0);
        assert_eq!(metrics.equal_cost_worker_utilization_upper_bound(), 0.0);
    }
}
