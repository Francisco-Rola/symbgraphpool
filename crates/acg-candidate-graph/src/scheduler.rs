//! Runtime-independent hard/soft edge classification and risk-bounded wave scheduling.

use std::cmp::Ordering;

use acg_core::TxIndex;
use thiserror::Error;

use crate::{CandidateGraph, CandidateTransaction, TransactionEdge};

/// Scheduling treatment derived from a concrete transaction-edge probability.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EdgeClass {
    /// Below the soft threshold; ignored by the primary scheduler.
    Low,
    /// Contributes probabilistic risk when two transactions share a wave.
    Soft,
    /// Oriented by predicted-order-compatible priority and enforced as a predecessor constraint.
    Hard,
}

/// Scheduler parameters for Brick 4's first adaptive wave scheduler.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RiskBoundedSchedulerConfig {
    /// Probability at or above which an edge contributes same-wave risk.
    pub soft_threshold: f64,
    /// Probability at or above which an edge becomes a hard predecessor dependency.
    pub hard_threshold: f64,
    /// Maximum combined soft-conflict risk accepted for one transaction placement.
    pub risk_budget: f64,
    /// Optional upper bound on transactions in one wave.
    pub max_wave_width: Option<usize>,
}

impl Default for RiskBoundedSchedulerConfig {
    fn default() -> Self {
        Self {
            soft_threshold: 0.20,
            hard_threshold: 0.80,
            risk_budget: 0.20,
            max_wave_width: None,
        }
    }
}

impl RiskBoundedSchedulerConfig {
    pub fn validate(&self) -> Result<(), SchedulingError> {
        validate_probability("soft_threshold", self.soft_threshold)?;
        validate_probability("hard_threshold", self.hard_threshold)?;
        validate_probability("risk_budget", self.risk_budget)?;
        if self.soft_threshold > self.hard_threshold {
            return Err(SchedulingError::ThresholdOrder {
                soft_threshold: self.soft_threshold,
                hard_threshold: self.hard_threshold,
            });
        }
        if self.max_wave_width == Some(0) {
            return Err(SchedulingError::ZeroWaveWidth);
        }
        Ok(())
    }

    /// Classifies one already-materialized candidate edge.
    ///
    /// Thresholds are inclusive: `p == hard_threshold` is hard and
    /// `p == soft_threshold` is soft unless it is also hard.
    pub fn classify(&self, edge: &TransactionEdge) -> EdgeClass {
        let probability = edge.probability();
        if probability >= self.hard_threshold {
            EdgeClass::Hard
        } else if probability >= self.soft_threshold {
            EdgeClass::Soft
        } else {
            EdgeClass::Low
        }
    }
}

/// One parallel execution wave. Transactions in different waves retain wave ordering.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledWave {
    pub transaction_indices: Vec<TxIndex>,
}

/// Runtime-independent schedule produced from a weighted candidate graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RiskBoundedSchedule {
    pub transaction_count: usize,
    pub waves: Vec<ScheduledWave>,
}

impl RiskBoundedSchedule {
    pub fn wave_for(&self, tx_index: TxIndex) -> Option<usize> {
        self.waves
            .iter()
            .position(|wave| wave.transaction_indices.contains(&tx_index))
    }

    /// Checks both structural validity and the Brick 4C scheduling constraints against `graph`.
    pub fn validate_against(
        &self,
        graph: &CandidateGraph,
        config: &RiskBoundedSchedulerConfig,
    ) -> Result<(), SchedulingError> {
        config.validate()?;
        if self.transaction_count != graph.transactions().len() {
            return Err(SchedulingError::TransactionCountMismatch {
                plan: self.transaction_count,
                graph: graph.transactions().len(),
            });
        }

        let mut assigned_wave = vec![None; graph.transactions().len()];
        for (wave_index, wave) in self.waves.iter().enumerate() {
            if wave.transaction_indices.is_empty() {
                return Err(SchedulingError::EmptyWave);
            }
            match config.max_wave_width {
                Some(max_width) if wave.transaction_indices.len() > max_width => {
                    return Err(SchedulingError::WaveCapacityExceeded {
                        wave_index,
                        actual: wave.transaction_indices.len(),
                        maximum: max_width,
                    });
                }
                Some(_) | None => {}
            }
            for &tx_index in &wave.transaction_indices {
                let offset = tx_index.0 as usize;
                if offset >= graph.transactions().len() {
                    return Err(SchedulingError::UnknownTransaction(tx_index));
                }
                if assigned_wave[offset].replace(wave_index).is_some() {
                    return Err(SchedulingError::DuplicateTransaction(tx_index));
                }
            }
        }
        if assigned_wave.iter().any(Option::is_none) {
            return Err(SchedulingError::MissingTransactions);
        }

        let analysis = SchedulingAnalysis::from_graph(graph, config);
        let mut priority_order = (0..graph.transactions().len())
            .map(|offset| TxIndex(offset as u32))
            .collect::<Vec<_>>();
        priority_order.sort_by(|left, right| analysis.compare_priority(*left, *right));
        let mut placed_wave = vec![None; graph.transactions().len()];

        for tx_index in priority_order {
            let successor_wave = assigned_wave[tx_index.0 as usize].expect("completeness checked");
            for &predecessor in &analysis.hard_predecessors[tx_index.0 as usize] {
                let predecessor_wave =
                    assigned_wave[predecessor.0 as usize].expect("completeness checked");
                if predecessor_wave >= successor_wave {
                    return Err(SchedulingError::HardDependencyViolation {
                        predecessor,
                        successor: tx_index,
                        predecessor_wave,
                        successor_wave,
                    });
                }
            }

            let risk = analysis.soft_risk(tx_index, successor_wave, &placed_wave);
            if risk > config.risk_budget {
                return Err(SchedulingError::RiskBudgetExceeded {
                    tx_index,
                    wave_index: successor_wave,
                    risk,
                    budget: config.risk_budget,
                });
            }
            placed_wave[tx_index.0 as usize] = Some(successor_wave);
        }
        Ok(())
    }
}

/// Bounded greedy scheduler over a Brick 4B weighted candidate graph.
#[derive(Clone, Copy, Debug)]
pub struct RiskBoundedScheduler {
    config: RiskBoundedSchedulerConfig,
}

impl RiskBoundedScheduler {
    pub fn new(config: RiskBoundedSchedulerConfig) -> Result<Self, SchedulingError> {
        config.validate()?;
        Ok(Self { config })
    }

    pub fn config(&self) -> &RiskBoundedSchedulerConfig {
        &self.config
    }

    pub fn classify_edge(&self, edge: &TransactionEdge) -> EdgeClass {
        self.config.classify(edge)
    }

    pub fn schedule(&self, graph: &CandidateGraph) -> Result<RiskBoundedSchedule, SchedulingError> {
        let transaction_count = graph.transactions().len();
        u32::try_from(transaction_count)
            .map_err(|_| SchedulingError::TooManyTransactions(transaction_count))?;
        if transaction_count == 0 {
            return Ok(RiskBoundedSchedule {
                transaction_count: 0,
                waves: Vec::new(),
            });
        }

        let analysis = SchedulingAnalysis::from_graph(graph, &self.config);
        let mut order = (0..transaction_count)
            .map(|offset| TxIndex(offset as u32))
            .collect::<Vec<_>>();
        order.sort_by(|left, right| analysis.compare_priority(*left, *right));

        let mut waves = Vec::<ScheduledWave>::new();
        let mut assigned_wave = vec![None::<usize>; transaction_count];

        for tx_index in order {
            let mut minimum_wave = 0;
            for &predecessor in &analysis.hard_predecessors[tx_index.0 as usize] {
                let predecessor_wave = assigned_wave[predecessor.0 as usize].ok_or(
                    SchedulingError::UnscheduledHardPredecessor {
                        predecessor,
                        successor: tx_index,
                    },
                )?;
                minimum_wave = minimum_wave.max(predecessor_wave + 1);
            }

            let mut selected_wave = None;
            for (wave_index, wave) in waves.iter().enumerate().skip(minimum_wave) {
                if !wave_has_capacity(wave, self.config.max_wave_width) {
                    continue;
                }
                let risk = analysis.soft_risk(tx_index, wave_index, &assigned_wave);
                if risk <= self.config.risk_budget {
                    selected_wave = Some(wave_index);
                    break;
                }
            }

            let wave_index = match selected_wave {
                Some(wave_index) => wave_index,
                None => {
                    let wave_index = waves.len();
                    debug_assert!(wave_index >= minimum_wave);
                    waves.push(ScheduledWave {
                        transaction_indices: Vec::new(),
                    });
                    wave_index
                }
            };
            waves[wave_index].transaction_indices.push(tx_index);
            assigned_wave[tx_index.0 as usize] = Some(wave_index);
        }

        let schedule = RiskBoundedSchedule {
            transaction_count,
            waves,
        };
        debug_assert!(schedule.validate_against(graph, &self.config).is_ok());
        Ok(schedule)
    }
}

#[derive(Debug)]
struct SchedulingAnalysis<'graph> {
    graph: &'graph CandidateGraph,
    hard_predecessors: Vec<Vec<TxIndex>>,
    soft_neighbors: Vec<Vec<(TxIndex, f64)>>,
    conflict_degree: Vec<usize>,
}

impl<'graph> SchedulingAnalysis<'graph> {
    fn from_graph(graph: &'graph CandidateGraph, config: &RiskBoundedSchedulerConfig) -> Self {
        let transaction_count = graph.transactions().len();
        let mut conflict_degree = vec![0usize; transaction_count];
        for edge in graph.edges() {
            if config.classify(edge) != EdgeClass::Low {
                conflict_degree[edge.source.0 as usize] += 1;
                conflict_degree[edge.target.0 as usize] += 1;
            }
        }

        let mut analysis = Self {
            graph,
            hard_predecessors: vec![Vec::new(); transaction_count],
            soft_neighbors: vec![Vec::new(); transaction_count],
            conflict_degree,
        };

        for edge in graph.edges() {
            match config.classify(edge) {
                EdgeClass::Low => {}
                EdgeClass::Soft => {
                    let probability = edge.probability();
                    analysis.soft_neighbors[edge.source.0 as usize]
                        .push((edge.target, probability));
                    analysis.soft_neighbors[edge.target.0 as usize]
                        .push((edge.source, probability));
                }
                EdgeClass::Hard => {
                    let (predecessor, successor) =
                        if analysis.compare_priority(edge.source, edge.target) != Ordering::Greater
                        {
                            (edge.source, edge.target)
                        } else {
                            (edge.target, edge.source)
                        };
                    analysis.hard_predecessors[successor.0 as usize].push(predecessor);
                }
            }
        }

        for predecessors in &mut analysis.hard_predecessors {
            predecessors.sort_unstable();
            predecessors.dedup();
        }
        for neighbors in &mut analysis.soft_neighbors {
            neighbors.sort_by_key(|(neighbor, _)| *neighbor);
        }
        analysis
    }

    fn compare_priority(&self, left: TxIndex, right: TxIndex) -> Ordering {
        let left_tx = &self.graph.transactions()[left.0 as usize];
        let right_tx = &self.graph.transactions()[right.0 as usize];
        compare_transactions(
            left,
            left_tx,
            self.conflict_degree[left.0 as usize],
            right,
            right_tx,
            self.conflict_degree[right.0 as usize],
        )
    }

    fn soft_risk(
        &self,
        tx_index: TxIndex,
        wave_index: usize,
        assigned_wave: &[Option<usize>],
    ) -> f64 {
        let mut independence_probability = 1.0;
        for &(neighbor, probability) in &self.soft_neighbors[tx_index.0 as usize] {
            if assigned_wave[neighbor.0 as usize] == Some(wave_index) {
                independence_probability *= 1.0 - probability;
            }
        }
        1.0 - independence_probability
    }
}

fn compare_transactions(
    left_index: TxIndex,
    left: &CandidateTransaction,
    left_degree: usize,
    right_index: TxIndex,
    right: &CandidateTransaction,
    right_degree: usize,
) -> Ordering {
    left.predicted_position
        .cmp(&right.predicted_position)
        .then_with(|| {
            right
                .inclusion_probability
                .total_cmp(&left.inclusion_probability)
        })
        .then_with(|| {
            right
                .estimated_execution_cost
                .cmp(&left.estimated_execution_cost)
        })
        .then_with(|| right_degree.cmp(&left_degree))
        .then_with(|| left_index.cmp(&right_index))
}

fn wave_has_capacity(wave: &ScheduledWave, max_wave_width: Option<usize>) -> bool {
    if let Some(maximum) = max_wave_width {
        return wave.transaction_indices.len() < maximum;
    }
    true
}

fn validate_probability(name: &'static str, value: f64) -> Result<(), SchedulingError> {
    if value.is_finite() && (0.0..=1.0).contains(&value) {
        Ok(())
    } else {
        Err(SchedulingError::InvalidProbability { name, value })
    }
}

#[derive(Debug, Error, PartialEq)]
pub enum SchedulingError {
    #[error("{name} must be finite and within [0, 1], got {value}")]
    InvalidProbability { name: &'static str, value: f64 },
    #[error("soft threshold {soft_threshold} must not exceed hard threshold {hard_threshold}")]
    ThresholdOrder {
        soft_threshold: f64,
        hard_threshold: f64,
    },
    #[error("max_wave_width must be greater than zero when configured")]
    ZeroWaveWidth,
    #[error("candidate graph contains too many transactions for TxIndex: {0}")]
    TooManyTransactions(usize),
    #[error("hard predecessor {predecessor:?} for {successor:?} was not scheduled first")]
    UnscheduledHardPredecessor {
        predecessor: TxIndex,
        successor: TxIndex,
    },
    #[error("schedule transaction count {plan} does not match graph count {graph}")]
    TransactionCountMismatch { plan: usize, graph: usize },
    #[error("schedule contains an empty wave")]
    EmptyWave,
    #[error("schedule references unknown transaction {0:?}")]
    UnknownTransaction(TxIndex),
    #[error("transaction {0:?} appears more than once in the schedule")]
    DuplicateTransaction(TxIndex),
    #[error("schedule does not contain every candidate transaction")]
    MissingTransactions,
    #[error("wave {wave_index} contains {actual} transactions, exceeding maximum {maximum}")]
    WaveCapacityExceeded {
        wave_index: usize,
        actual: usize,
        maximum: usize,
    },
    #[error(
        "hard dependency {predecessor:?} -> {successor:?} is violated by waves {predecessor_wave} and {successor_wave}"
    )]
    HardDependencyViolation {
        predecessor: TxIndex,
        successor: TxIndex,
        predecessor_wave: usize,
        successor_wave: usize,
    },
    #[error(
        "transaction {tx_index:?} has soft risk {risk} in wave {wave_index}, exceeding budget {budget}"
    )]
    RiskBudgetExceeded {
        tx_index: TxIndex,
        wave_index: usize,
        risk: f64,
        budget: f64,
    },
}

#[cfg(test)]
mod tests {
    use acg_core::{ConflictKinds, InstanceId, ProfileEdgeIndex, ProfileId, TxId, TxIndex};
    use acg_predicate::{InputBindings, PredicateResult};

    use super::*;
    use crate::{
        finish_graph, quantize_q16, CandidateTransaction, EdgeProvenance, TransactionEdge,
    };

    fn tx(
        id: u64,
        predicted_position: u32,
        inclusion_probability: f32,
        estimated_execution_cost: u32,
    ) -> CandidateTransaction {
        CandidateTransaction {
            tx_id: TxId(id),
            predicted_position,
            inclusion_probability,
            profile_id: ProfileId(0),
            instance_id: InstanceId(0),
            input_bindings: InputBindings::empty(),
            estimated_execution_cost,
        }
    }

    fn edge(source: u32, target: u32, probability: f64) -> TransactionEdge {
        TransactionEdge {
            source: TxIndex(source),
            target: TxIndex(target),
            provenance: EdgeProvenance::Static {
                profile_edge_index: ProfileEdgeIndex(0),
            },
            predicate_result: PredicateResult::True,
            conflict_kinds: ConflictKinds::WRITE_WRITE,
            probability_q16: quantize_q16(probability),
            confidence_q16: 0,
        }
    }

    fn graph(
        transactions: Vec<CandidateTransaction>,
        edges: Vec<TransactionEdge>,
    ) -> CandidateGraph {
        finish_graph(transactions, edges).unwrap()
    }

    fn config(soft: f64, hard: f64, risk: f64) -> RiskBoundedSchedulerConfig {
        RiskBoundedSchedulerConfig {
            soft_threshold: soft,
            hard_threshold: hard,
            risk_budget: risk,
            max_wave_width: None,
        }
    }

    #[test]
    fn classification_thresholds_are_inclusive() {
        let cfg = config(0.2, 0.8, 0.2);
        assert_eq!(cfg.classify(&edge(0, 1, 0.1)), EdgeClass::Low);
        assert_eq!(cfg.classify(&edge(0, 1, 0.2)), EdgeClass::Soft);
        assert_eq!(cfg.classify(&edge(0, 1, 0.799)), EdgeClass::Soft);
        assert_eq!(cfg.classify(&edge(0, 1, 0.8)), EdgeClass::Hard);
        assert_eq!(cfg.classify(&edge(0, 1, 1.0)), EdgeClass::Hard);
    }

    #[test]
    fn confidence_does_not_change_brick4c_probability_classification() {
        let cfg = config(0.2, 0.8, 0.2);
        let low_confidence = edge(0, 1, 0.5);
        let mut high_confidence = low_confidence;
        high_confidence.confidence_q16 = u16::MAX;
        assert_eq!(cfg.classify(&low_confidence), EdgeClass::Soft);
        assert_eq!(cfg.classify(&high_confidence), EdgeClass::Soft);
    }

    #[test]
    fn invalid_configuration_is_rejected() {
        for (name, cfg) in [
            ("soft", config(f64::NAN, 0.8, 0.2)),
            ("hard", config(0.2, 1.1, 0.2)),
            ("risk", config(0.2, 0.8, -0.1)),
        ] {
            assert!(
                matches!(
                    RiskBoundedScheduler::new(cfg),
                    Err(SchedulingError::InvalidProbability { .. })
                ),
                "{name}"
            );
        }
        assert!(matches!(
            RiskBoundedScheduler::new(config(0.9, 0.8, 0.2)),
            Err(SchedulingError::ThresholdOrder { .. })
        ));
        let mut zero_width = config(0.2, 0.8, 0.2);
        zero_width.max_wave_width = Some(0);
        assert_eq!(
            RiskBoundedScheduler::new(zero_width).unwrap_err(),
            SchedulingError::ZeroWaveWidth
        );
    }

    #[test]
    fn independent_transactions_share_the_earliest_wave() {
        let graph = graph(
            vec![tx(1, 0, 1.0, 10), tx(2, 1, 1.0, 10), tx(3, 2, 1.0, 10)],
            vec![],
        );
        let scheduler = RiskBoundedScheduler::new(config(0.2, 0.8, 0.2)).unwrap();
        let schedule = scheduler.schedule(&graph).unwrap();
        assert_eq!(
            schedule.waves,
            vec![ScheduledWave {
                transaction_indices: vec![TxIndex(0), TxIndex(1), TxIndex(2)]
            }]
        );
        schedule
            .validate_against(&graph, scheduler.config())
            .unwrap();
    }

    #[test]
    fn hard_edge_is_oriented_by_predicted_order_and_forces_a_later_wave() {
        let graph = graph(
            vec![tx(1, 20, 1.0, 10), tx(2, 10, 1.0, 10)],
            vec![edge(0, 1, 0.95)],
        );
        let scheduler = RiskBoundedScheduler::new(config(0.2, 0.8, 0.2)).unwrap();
        let schedule = scheduler.schedule(&graph).unwrap();
        assert_eq!(schedule.wave_for(TxIndex(1)), Some(0));
        assert_eq!(schedule.wave_for(TxIndex(0)), Some(1));
        schedule
            .validate_against(&graph, scheduler.config())
            .unwrap();
    }

    #[test]
    fn single_soft_edge_respects_the_risk_budget_boundary() {
        let graph = graph(
            vec![tx(1, 0, 1.0, 10), tx(2, 1, 1.0, 10)],
            vec![edge(0, 1, 0.25)],
        );
        let probability = graph.edges()[0].probability();
        let at_boundary = RiskBoundedScheduler::new(config(0.2, 0.8, probability))
            .unwrap()
            .schedule(&graph)
            .unwrap();
        assert_eq!(at_boundary.waves.len(), 1);

        let below_boundary = RiskBoundedScheduler::new(config(0.2, 0.8, probability / 2.0))
            .unwrap()
            .schedule(&graph)
            .unwrap();
        assert_eq!(below_boundary.waves.len(), 2);
    }

    #[test]
    fn cumulative_soft_risk_can_reject_a_wave_when_each_edge_alone_fits() {
        let graph = graph(
            vec![tx(1, 0, 1.0, 10), tx(2, 1, 1.0, 10), tx(3, 2, 1.0, 10)],
            vec![edge(0, 2, 0.2), edge(1, 2, 0.2)],
        );
        let scheduler = RiskBoundedScheduler::new(config(0.15, 0.8, 0.30)).unwrap();
        let schedule = scheduler.schedule(&graph).unwrap();
        // tx0 and tx1 are independent. tx2 sees 1 - (0.8 * 0.8) = 0.36 risk in wave 0.
        assert_eq!(schedule.wave_for(TxIndex(0)), Some(0));
        assert_eq!(schedule.wave_for(TxIndex(1)), Some(0));
        assert_eq!(schedule.wave_for(TxIndex(2)), Some(1));
        schedule
            .validate_against(&graph, scheduler.config())
            .unwrap();
    }

    #[test]
    fn low_edges_do_not_contribute_to_wave_risk() {
        let graph = graph(
            vec![tx(1, 0, 1.0, 10), tx(2, 1, 1.0, 10)],
            vec![edge(0, 1, 0.19)],
        );
        let scheduler = RiskBoundedScheduler::new(config(0.2, 0.8, 0.0)).unwrap();
        let schedule = scheduler.schedule(&graph).unwrap();
        assert_eq!(schedule.waves.len(), 1);
        schedule
            .validate_against(&graph, scheduler.config())
            .unwrap();
    }

    #[test]
    fn scheduler_chooses_the_earliest_wave_satisfying_hard_and_soft_constraints() {
        let graph = graph(
            vec![
                tx(1, 0, 1.0, 10),
                tx(2, 1, 1.0, 10),
                tx(3, 2, 1.0, 10),
                tx(4, 3, 1.0, 10),
            ],
            vec![
                edge(0, 1, 0.95), // tx1 must be after tx0.
                edge(1, 3, 0.30), // tx3 cannot join tx1's wave at budget 0.20.
            ],
        );
        let scheduler = RiskBoundedScheduler::new(config(0.2, 0.8, 0.20)).unwrap();
        let schedule = scheduler.schedule(&graph).unwrap();
        assert_eq!(schedule.wave_for(TxIndex(0)), Some(0));
        assert_eq!(schedule.wave_for(TxIndex(1)), Some(1));
        assert_eq!(schedule.wave_for(TxIndex(2)), Some(0));
        // Wave 0 is earlier and has no edge to tx3, so it is the earliest valid placement.
        assert_eq!(schedule.wave_for(TxIndex(3)), Some(0));
        schedule
            .validate_against(&graph, scheduler.config())
            .unwrap();
    }

    #[test]
    fn max_wave_width_bounds_parallel_placement() {
        let graph = graph(
            vec![
                tx(1, 0, 1.0, 10),
                tx(2, 1, 1.0, 10),
                tx(3, 2, 1.0, 10),
                tx(4, 3, 1.0, 10),
                tx(5, 4, 1.0, 10),
            ],
            vec![],
        );
        let mut cfg = config(0.2, 0.8, 0.2);
        cfg.max_wave_width = Some(2);
        let scheduler = RiskBoundedScheduler::new(cfg).unwrap();
        let schedule = scheduler.schedule(&graph).unwrap();
        assert_eq!(
            schedule
                .waves
                .iter()
                .map(|wave| wave.transaction_indices.len())
                .collect::<Vec<_>>(),
            vec![2, 2, 1]
        );
        schedule
            .validate_against(&graph, scheduler.config())
            .unwrap();
    }

    #[test]
    fn tied_predicted_positions_use_inclusion_cost_degree_then_index_deterministically() {
        let graph = graph(
            vec![
                tx(1, 7, 0.8, 10), // lower inclusion than tx1
                tx(2, 7, 0.9, 10),
                tx(3, 7, 0.9, 20), // higher cost than tx1
                tx(4, 7, 0.9, 20), // same as tx2 but has conflict degree 1
            ],
            vec![edge(0, 3, 0.3)],
        );
        let scheduler = RiskBoundedScheduler::new(config(0.2, 0.8, 1.0)).unwrap();
        let schedule = scheduler.schedule(&graph).unwrap();
        assert_eq!(schedule.waves.len(), 1);
        assert_eq!(
            schedule.waves[0].transaction_indices,
            vec![TxIndex(3), TxIndex(2), TxIndex(1), TxIndex(0)]
        );
    }

    #[test]
    fn semantic_validator_detects_hard_and_risk_violations() {
        let graph = graph(
            vec![tx(1, 0, 1.0, 10), tx(2, 1, 1.0, 10), tx(3, 2, 1.0, 10)],
            vec![edge(0, 1, 0.95), edge(0, 2, 0.3)],
        );
        let cfg = config(0.2, 0.8, 0.2);

        let hard_violation = RiskBoundedSchedule {
            transaction_count: 3,
            waves: vec![
                ScheduledWave {
                    transaction_indices: vec![TxIndex(0), TxIndex(1)],
                },
                ScheduledWave {
                    transaction_indices: vec![TxIndex(2)],
                },
            ],
        };
        assert!(matches!(
            hard_violation.validate_against(&graph, &cfg),
            Err(SchedulingError::HardDependencyViolation { .. })
        ));

        let risk_violation = RiskBoundedSchedule {
            transaction_count: 3,
            waves: vec![
                ScheduledWave {
                    transaction_indices: vec![TxIndex(0), TxIndex(2)],
                },
                ScheduledWave {
                    transaction_indices: vec![TxIndex(1)],
                },
            ],
        };
        assert!(matches!(
            risk_violation.validate_against(&graph, &cfg),
            Err(SchedulingError::RiskBudgetExceeded { .. })
        ));
    }

    #[test]
    fn empty_graph_has_an_empty_valid_schedule() {
        let graph = graph(vec![], vec![]);
        let scheduler = RiskBoundedScheduler::new(config(0.2, 0.8, 0.2)).unwrap();
        let schedule = scheduler.schedule(&graph).unwrap();
        assert_eq!(schedule.transaction_count, 0);
        assert!(schedule.waves.is_empty());
        schedule
            .validate_against(&graph, scheduler.config())
            .unwrap();
    }
}
