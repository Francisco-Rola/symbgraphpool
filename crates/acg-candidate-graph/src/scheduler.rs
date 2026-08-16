//! Runtime-independent adaptive edge classification and dependency-aware scheduling.

use std::{cmp::Ordering, collections::BTreeMap};

use acg_core::TxIndex;
use acg_predicate::PredicateResult;
use thiserror::Error;

use crate::{CandidateGraph, EdgeProvenance, TransactionEdge};

/// Scheduling treatment derived from symbolic/runtime evidence plus adaptive probability.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum EdgeClass {
    /// Below the soft threshold; ignored by the primary scheduler.
    Low,
    /// May execute concurrently when the aggregate risk budget allows it.
    Soft,
    /// Must execute after its canonical predecessor has completed and published its version.
    Hard,
}

/// One ordering dependency emitted by the scheduler.
///
/// Hard dependencies are always present. Soft dependencies are emitted when the scheduler chose
/// to separate a soft pair into different waves; soft pairs intentionally co-scheduled in one wave
/// remain speculative and therefore have no execution dependency.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ScheduledDependency {
    pub predecessor: TxIndex,
    pub successor: TxIndex,
    pub class: EdgeClass,
}

/// Scheduler parameters for adaptive dependency scheduling.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RiskBoundedSchedulerConfig {
    /// Cost-adjusted scheduling risk at or above which an ordinary adaptive edge contributes
    /// same-wave risk. Before Phase 5D has replay-cost evidence this equals conflict probability.
    pub soft_threshold: f64,
    /// Cost-adjusted scheduling risk at or above which an evidence-mature edge remains a hard
    /// predecessor dependency.
    pub hard_threshold: f64,
    /// Maximum combined soft-conflict risk accepted for one transaction placement.
    pub risk_budget: f64,
    /// Optional upper bound on transactions in one reported wave.
    pub max_wave_width: Option<usize>,
    /// Deterministic fraction of transactions allowed to use the exploration risk budget.
    /// This keeps conservative policies from permanently starving themselves of replay evidence.
    pub exploration_rate: f64,
    /// Risk budget used by transactions selected for controlled exploration.
    pub exploration_risk_budget: f64,
    /// Minimum posterior uncertainty (`1 - confidence`) required before a soft relationship is
    /// eligible for exploration.
    pub exploration_min_uncertainty: f64,
    /// Maximum number of transactions in one block that may consume risk above the production
    /// budget. Since each transaction can be replayed at most once, this bounds exploration-
    /// induced replay exposure per block.
    pub exploration_max_transactions_per_block: usize,
    /// Concrete independence observations required before a symbolic/runtime-discovered hard
    /// relationship may be demoted to soft by its adaptive probability. `0` disables hard-to-soft
    /// demotion.
    ///
    /// Fresh `PredicateResult::True`, historical false-overrides, and runtime-discovered edges are
    /// hard regardless of their prior probability. Repeated observed conflicts do not mature the
    /// softening gate; only executions that did *not* observe the relationship do. After this many
    /// independence observations, the edge remains hard only while its cost-adjusted scheduling
    /// risk is at least `hard_threshold`; otherwise it becomes soft.
    pub independent_observations_before_softening: u32,
}

impl Default for RiskBoundedSchedulerConfig {
    fn default() -> Self {
        Self {
            soft_threshold: 0.20,
            hard_threshold: 0.80,
            risk_budget: 0.20,
            max_wave_width: None,
            exploration_rate: 0.0,
            exploration_risk_budget: 0.90,
            exploration_min_uncertainty: 0.35,
            exploration_max_transactions_per_block: 8,
            independent_observations_before_softening: 8,
        }
    }
}

impl RiskBoundedSchedulerConfig {
    pub fn validate(&self) -> Result<(), SchedulingError> {
        validate_probability("soft_threshold", self.soft_threshold)?;
        validate_probability("hard_threshold", self.hard_threshold)?;
        validate_probability("risk_budget", self.risk_budget)?;
        validate_probability("exploration_rate", self.exploration_rate)?;
        validate_probability("exploration_risk_budget", self.exploration_risk_budget)?;
        validate_probability(
            "exploration_min_uncertainty",
            self.exploration_min_uncertainty,
        )?;
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
    /// Proven symbolic conflicts and concrete runtime-discovered relationships begin Hard. Once
    /// enough concrete evidence exists, their posterior may demote them to Soft. This implements
    /// "hard until disproved by execution" without letting a symbolic prior alone soften a known
    /// relationship. Ordinary Unknown edges retain probability-threshold classification.
    pub fn classify(&self, edge: &TransactionEdge) -> EdgeClass {
        let initially_hard = edge.predicate_result == PredicateResult::True
            || edge.is_historical_override()
            || matches!(edge.provenance, EdgeProvenance::RuntimeDiscovered { .. });

        if initially_hard {
            let evidence_mature = self.independent_observations_before_softening != 0
                && edge.concrete_independent_observations
                    >= self.independent_observations_before_softening;
            if !evidence_mature || edge.scheduling_risk() >= self.hard_threshold {
                EdgeClass::Hard
            } else {
                EdgeClass::Soft
            }
        } else {
            let scheduling_risk = edge.scheduling_risk();
            if scheduling_risk >= self.hard_threshold {
                EdgeClass::Hard
            } else if scheduling_risk >= self.soft_threshold {
                EdgeClass::Soft
            } else {
                EdgeClass::Low
            }
        }
    }
}

/// One dependency level retained for diagnostics/theoretical scheduling metrics.
///
/// The Phase-5 executor no longer treats these as global barriers. Actual launch eligibility is
/// driven by [`RiskBoundedSchedule::ordering_dependencies`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScheduledWave {
    pub transaction_indices: Vec<TxIndex>,
}

/// Runtime-independent schedule produced from a weighted candidate graph.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RiskBoundedSchedule {
    pub transaction_count: usize,
    pub waves: Vec<ScheduledWave>,
    /// Classified ordering dependencies before exact transitive reduction of the final DAG.
    pub pre_reduction_ordering_dependencies: usize,
    /// Ordering dependencies removed because an alternate retained path already preserves
    /// reachability. The legacy field name is retained for source compatibility.
    pub hard_dependencies_elided_by_reduction: usize,
    pub ordering_dependencies: Vec<ScheduledDependency>,
}

impl RiskBoundedSchedule {
    pub fn wave_for(&self, tx_index: TxIndex) -> Option<usize> {
        self.waves
            .iter()
            .position(|wave| wave.transaction_indices.contains(&tx_index))
    }

    pub fn predecessors(
        &self,
        tx_index: TxIndex,
    ) -> impl Iterator<Item = ScheduledDependency> + '_ {
        self.ordering_dependencies
            .iter()
            .copied()
            .filter(move |dependency| dependency.successor == tx_index)
    }

    /// Checks structural validity and dependency/risk constraints against `graph`.
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
            if let Some(max_width) = config.max_wave_width {
                if wave.transaction_indices.len() > max_width {
                    return Err(SchedulingError::WaveCapacityExceeded {
                        wave_index,
                        actual: wave.transaction_indices.len(),
                        maximum: max_width,
                    });
                }
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

        let expected = SchedulingAnalysis::from_graph(graph, config);
        let expected_dependencies = expected.dependencies_for_assignment(&assigned_wave)?;
        if expected_dependencies != self.ordering_dependencies {
            return Err(SchedulingError::DependencySetMismatch);
        }
        let expected_pre_reduction = expected.pre_reduction_dependency_count(&assigned_wave)?;
        let expected_elided = expected_pre_reduction.saturating_sub(expected_dependencies.len());
        if expected_pre_reduction != self.pre_reduction_ordering_dependencies
            || expected_elided != self.hard_dependencies_elided_by_reduction
        {
            return Err(SchedulingError::DependencyReductionMetadataMismatch {
                expected_pre_reduction,
                actual_pre_reduction: self.pre_reduction_ordering_dependencies,
                expected_elided,
                actual_elided: self.hard_dependencies_elided_by_reduction,
            });
        }

        let mut placed_wave = vec![None; graph.transactions().len()];
        let mut canonical_order = (0..graph.transactions().len())
            .map(|offset| TxIndex(offset as u32))
            .collect::<Vec<_>>();
        canonical_order.sort_by(|left, right| expected.compare_canonical_order(*left, *right));

        let mut exploration_used = 0_usize;
        let mut compact_group_wave_counts =
            vec![BTreeMap::<usize, usize>::new(); expected.compact_soft_groups.len()];
        for tx_index in canonical_order {
            let offset = tx_index.0 as usize;
            let wave_index = assigned_wave[offset].expect("completeness checked");
            let risk = expected.soft_risk_with_group_counts(
                tx_index,
                wave_index,
                &placed_wave,
                &compact_group_wave_counts,
            );
            let exploration_allowed = expected.should_explore(tx_index, config, exploration_used);
            let budget = if exploration_allowed {
                config.risk_budget.max(config.exploration_risk_budget)
            } else {
                config.risk_budget
            };
            if risk > budget {
                return Err(SchedulingError::RiskBudgetExceeded {
                    tx_index,
                    wave_index,
                    risk,
                    budget,
                });
            }
            if exploration_allowed && risk > config.risk_budget {
                exploration_used = exploration_used.saturating_add(1);
            }
            placed_wave[offset] = Some(wave_index);
            for group_index in &expected.compact_soft_memberships[offset] {
                *compact_group_wave_counts[*group_index]
                    .entry(wave_index)
                    .or_default() += 1;
            }
        }
        Ok(())
    }
}

/// Greedy scheduler that preserves canonical orientation for every materialized non-Low edge.
///
/// Hard edges must be in an earlier wave. Soft edges may share a wave if the risk budget allows;
/// otherwise they are canonically ordered and emitted as execution dependencies. This makes the
/// reported waves a useful levelization while allowing the runtime executor to launch successors
/// immediately when their actual predecessor set completes instead of imposing global barriers.
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
                pre_reduction_ordering_dependencies: 0,
                hard_dependencies_elided_by_reduction: 0,
                ordering_dependencies: Vec::new(),
            });
        }

        let analysis = SchedulingAnalysis::from_graph(graph, &self.config);
        let mut order = (0..transaction_count)
            .map(|offset| TxIndex(offset as u32))
            .collect::<Vec<_>>();
        order.sort_by(|left, right| analysis.compare_canonical_order(*left, *right));

        let mut waves = Vec::<ScheduledWave>::new();
        let mut assigned_wave = vec![None::<usize>; transaction_count];
        let mut exploration_used = 0_usize;
        let mut compact_group_max_wave = vec![None::<usize>; analysis.compact_soft_groups.len()];
        let mut compact_group_wave_counts =
            vec![BTreeMap::<usize, usize>::new(); analysis.compact_soft_groups.len()];

        for tx_index in order {
            let offset = tx_index.0 as usize;
            let mut minimum_wave = 0;

            // Hard dependencies must complete in a strictly earlier level.
            for &predecessor in &analysis.hard_predecessors[offset] {
                let predecessor_wave = assigned_wave[predecessor.0 as usize].ok_or(
                    SchedulingError::UnscheduledHardPredecessor {
                        predecessor,
                        successor: tx_index,
                    },
                )?;
                minimum_wave = minimum_wave.max(predecessor_wave + 1);
            }

            // A soft neighbor that precedes this transaction canonically may either share its
            // wave (accepted speculation) or appear earlier, never later. Compact soft groups
            // maintain the same monotone-wave rule from one group-level maximum instead of one
            // neighbor entry per logical pair.
            for &(neighbor, _, _) in &analysis.soft_neighbors[offset] {
                if let Some(neighbor_wave) = assigned_wave[neighbor.0 as usize] {
                    minimum_wave = minimum_wave.max(neighbor_wave);
                }
            }
            for group_index in &analysis.compact_soft_memberships[offset] {
                if let Some(group_wave) = compact_group_max_wave[*group_index] {
                    minimum_wave = minimum_wave.max(group_wave);
                }
            }

            let exploration_allowed =
                analysis.should_explore(tx_index, &self.config, exploration_used);
            let effective_budget = if exploration_allowed {
                self.config
                    .risk_budget
                    .max(self.config.exploration_risk_budget)
            } else {
                self.config.risk_budget
            };
            let mut selected_wave = None;
            let mut selected_risk = 0.0;
            for (wave_index, wave) in waves.iter().enumerate().skip(minimum_wave) {
                if !wave_has_capacity(wave, self.config.max_wave_width) {
                    continue;
                }
                let risk = analysis.soft_risk_with_group_counts(
                    tx_index,
                    wave_index,
                    &assigned_wave,
                    &compact_group_wave_counts,
                );
                if risk <= effective_budget {
                    selected_wave = Some(wave_index);
                    selected_risk = risk;
                    break;
                }
            }

            let wave_index = match selected_wave {
                Some(wave_index) => wave_index,
                None => {
                    let wave_index = waves.len();
                    waves.push(ScheduledWave {
                        transaction_indices: Vec::new(),
                    });
                    wave_index
                }
            };
            waves[wave_index].transaction_indices.push(tx_index);
            assigned_wave[offset] = Some(wave_index);
            for group_index in &analysis.compact_soft_memberships[offset] {
                compact_group_max_wave[*group_index] = Some(wave_index);
                *compact_group_wave_counts[*group_index]
                    .entry(wave_index)
                    .or_default() += 1;
            }
            if exploration_allowed && selected_risk > self.config.risk_budget {
                exploration_used = exploration_used.saturating_add(1);
            }
        }

        let pre_reduction_ordering_dependencies =
            analysis.pre_reduction_dependency_count(&assigned_wave)?;
        let ordering_dependencies = analysis.dependencies_for_assignment(&assigned_wave)?;
        let schedule = RiskBoundedSchedule {
            transaction_count,
            waves,
            pre_reduction_ordering_dependencies,
            hard_dependencies_elided_by_reduction: pre_reduction_ordering_dependencies
                .saturating_sub(ordering_dependencies.len()),
            ordering_dependencies,
        };
        debug_assert!(schedule.validate_against(graph, &self.config).is_ok());
        Ok(schedule)
    }
}

#[derive(Debug)]
struct CompactSoftGroup {
    members: Vec<TxIndex>,
    scheduling_risk: f64,
    confidence: f64,
}

#[derive(Debug)]
struct SchedulingAnalysis<'graph> {
    graph: &'graph CandidateGraph,
    hard_predecessors: Vec<Vec<TxIndex>>,
    soft_neighbors: Vec<Vec<(TxIndex, f64, f64)>>,
    compact_soft_groups: Vec<CompactSoftGroup>,
    compact_soft_memberships: Vec<Vec<usize>>,
    edge_classes: BTreeMap<(TxIndex, TxIndex), EdgeClass>,
    hard_dependency_candidates: Vec<ScheduledDependency>,
    reduced_hard_dependencies: Vec<ScheduledDependency>,
}

impl<'graph> SchedulingAnalysis<'graph> {
    fn from_graph(graph: &'graph CandidateGraph, config: &RiskBoundedSchedulerConfig) -> Self {
        let transaction_count = graph.transactions().len();
        let mut analysis = Self {
            graph,
            hard_predecessors: vec![Vec::new(); transaction_count],
            soft_neighbors: vec![Vec::new(); transaction_count],
            compact_soft_groups: Vec::new(),
            compact_soft_memberships: vec![Vec::new(); transaction_count],
            edge_classes: BTreeMap::new(),
            hard_dependency_candidates: Vec::new(),
            reduced_hard_dependencies: Vec::new(),
        };

        // Explicit edges belonging to a compact provenance are representation edges only. The
        // group below carries the complete logical clique semantics, including mature Soft risk.
        for edge in graph.edges() {
            if graph.provenance_is_compact(edge.provenance) {
                continue;
            }
            let class = config.classify(edge);
            analysis
                .edge_classes
                .insert((edge.source, edge.target), class);
            match class {
                EdgeClass::Low => {}
                EdgeClass::Soft => {
                    let scheduling_risk = edge.scheduling_risk();
                    analysis.soft_neighbors[edge.source.0 as usize].push((
                        edge.target,
                        scheduling_risk,
                        edge.confidence(),
                    ));
                    analysis.soft_neighbors[edge.target.0 as usize].push((
                        edge.source,
                        scheduling_risk,
                        edge.confidence(),
                    ));
                }
                EdgeClass::Hard => {
                    let (predecessor, successor) =
                        analysis.canonical_pair(edge.source, edge.target);
                    analysis
                        .hard_dependency_candidates
                        .push(ScheduledDependency {
                            predecessor,
                            successor,
                            class: EdgeClass::Hard,
                        });
                }
            }
        }

        for group in graph.compact_groups() {
            let mut members = group.members().to_vec();
            members.sort_by(|left, right| analysis.compare_canonical_order(*left, *right));
            match config.classify(group.edge_template()) {
                EdgeClass::Low => {}
                EdgeClass::Hard => {
                    for pair in members.windows(2) {
                        analysis
                            .hard_dependency_candidates
                            .push(ScheduledDependency {
                                predecessor: pair[0],
                                successor: pair[1],
                                class: EdgeClass::Hard,
                            });
                    }
                }
                EdgeClass::Soft => {
                    let group_index = analysis.compact_soft_groups.len();
                    for member in &members {
                        analysis.compact_soft_memberships[member.0 as usize].push(group_index);
                    }
                    analysis.compact_soft_groups.push(CompactSoftGroup {
                        members,
                        scheduling_risk: group.edge_template().scheduling_risk(),
                        confidence: group.edge_template().confidence(),
                    });
                }
            }
        }

        for neighbors in &mut analysis.soft_neighbors {
            neighbors.sort_by_key(|(neighbor, _, _)| *neighbor);
        }
        for memberships in &mut analysis.compact_soft_memberships {
            memberships.sort_unstable();
        }
        analysis.hard_dependency_candidates.sort_unstable();
        analysis.hard_dependency_candidates.dedup();

        analysis.reduced_hard_dependencies =
            analysis.transitively_reduce_dependencies(analysis.hard_dependency_candidates.clone());
        for dependency in &analysis.reduced_hard_dependencies {
            analysis.hard_predecessors[dependency.successor.0 as usize]
                .push(dependency.predecessor);
        }
        for predecessors in &mut analysis.hard_predecessors {
            predecessors.sort_unstable();
            predecessors.dedup();
        }
        analysis
    }

    fn canonical_pair(&self, left: TxIndex, right: TxIndex) -> (TxIndex, TxIndex) {
        if self.compare_canonical_order(left, right) != Ordering::Greater {
            (left, right)
        } else {
            (right, left)
        }
    }

    fn compare_canonical_order(&self, left: TxIndex, right: TxIndex) -> Ordering {
        let left_tx = &self.graph.transactions()[left.0 as usize];
        let right_tx = &self.graph.transactions()[right.0 as usize];
        left_tx
            .predicted_position
            .cmp(&right_tx.predicted_position)
            .then_with(|| left.cmp(&right))
    }

    fn dependencies_before_reduction(
        &self,
        assigned_wave: &[Option<usize>],
    ) -> Result<Vec<ScheduledDependency>, SchedulingError> {
        let mut dependencies = self.hard_dependency_candidates.clone();
        for dependency in &dependencies {
            let predecessor_wave = assigned_wave[dependency.predecessor.0 as usize]
                .ok_or(SchedulingError::MissingTransactions)?;
            let successor_wave = assigned_wave[dependency.successor.0 as usize]
                .ok_or(SchedulingError::MissingTransactions)?;
            if predecessor_wave >= successor_wave {
                return Err(SchedulingError::HardDependencyViolation {
                    predecessor: dependency.predecessor,
                    successor: dependency.successor,
                    predecessor_wave,
                    successor_wave,
                });
            }
        }

        for edge in self.graph.edges() {
            if self.graph.provenance_is_compact(edge.provenance) {
                continue;
            }
            let class = self
                .edge_classes
                .get(&(edge.source, edge.target))
                .copied()
                .unwrap_or(EdgeClass::Low);
            if class != EdgeClass::Soft {
                continue;
            }
            let (predecessor, successor) = self.canonical_pair(edge.source, edge.target);
            let predecessor_wave = assigned_wave[predecessor.0 as usize]
                .ok_or(SchedulingError::MissingTransactions)?;
            let successor_wave =
                assigned_wave[successor.0 as usize].ok_or(SchedulingError::MissingTransactions)?;
            if predecessor_wave > successor_wave {
                return Err(SchedulingError::SoftOrderingViolation {
                    predecessor,
                    successor,
                    predecessor_wave,
                    successor_wave,
                });
            }
            if predecessor_wave < successor_wave {
                dependencies.push(ScheduledDependency {
                    predecessor,
                    successor,
                    class,
                });
            }
        }

        for group in &self.compact_soft_groups {
            let mut layers = Vec::<(usize, Vec<TxIndex>)>::new();
            for member in &group.members {
                let wave =
                    assigned_wave[member.0 as usize].ok_or(SchedulingError::MissingTransactions)?;
                if let Some((previous_wave, _)) = layers.last() {
                    if wave < *previous_wave {
                        return Err(SchedulingError::SoftOrderingViolation {
                            predecessor: layers
                                .last()
                                .and_then(|(_, members)| members.last())
                                .copied()
                                .unwrap_or(*member),
                            successor: *member,
                            predecessor_wave: *previous_wave,
                            successor_wave: wave,
                        });
                    }
                }
                match layers.last_mut() {
                    Some((layer_wave, members)) if *layer_wave == wave => members.push(*member),
                    _ => layers.push((wave, vec![*member])),
                }
            }
            for adjacent_layers in layers.windows(2) {
                for predecessor in &adjacent_layers[0].1 {
                    for successor in &adjacent_layers[1].1 {
                        dependencies.push(ScheduledDependency {
                            predecessor: *predecessor,
                            successor: *successor,
                            class: EdgeClass::Soft,
                        });
                    }
                }
            }
        }

        dependencies.sort_unstable();
        dependencies.dedup();
        Ok(dependencies)
    }

    fn dependencies_for_assignment(
        &self,
        assigned_wave: &[Option<usize>],
    ) -> Result<Vec<ScheduledDependency>, SchedulingError> {
        let dependencies = self.dependencies_before_reduction(assigned_wave)?;
        Ok(self.transitively_reduce_dependencies(dependencies))
    }

    /// Exact transitive reduction of an already canonically oriented ordering DAG.
    fn transitively_reduce_dependencies(
        &self,
        dependencies: Vec<ScheduledDependency>,
    ) -> Vec<ScheduledDependency> {
        let transaction_count = self.graph.transactions().len();
        if dependencies.len() <= 1 || transaction_count <= 1 {
            return dependencies;
        }

        let mut canonical_order = (0..transaction_count)
            .map(|offset| TxIndex(offset as u32))
            .collect::<Vec<_>>();
        canonical_order.sort_by(|left, right| self.compare_canonical_order(*left, *right));
        let mut rank = vec![0_usize; transaction_count];
        for (position, tx) in canonical_order.iter().copied().enumerate() {
            rank[tx.0 as usize] = position;
        }

        let mut outgoing = vec![Vec::<ScheduledDependency>::new(); transaction_count];
        for dependency in dependencies {
            outgoing[dependency.predecessor.0 as usize].push(dependency);
        }
        for successors in &mut outgoing {
            successors.sort_by_key(|dependency| rank[dependency.successor.0 as usize]);
            successors.dedup();
        }

        let words = transaction_count.div_ceil(64);
        let mut reachable = vec![vec![0_u64; words]; transaction_count];
        let mut reduced = Vec::new();
        for predecessor in canonical_order.iter().copied().rev() {
            let predecessor_index = predecessor.0 as usize;
            for dependency in outgoing[predecessor_index].iter().copied() {
                let successor_index = dependency.successor.0 as usize;
                if bit_is_set(&reachable[predecessor_index], successor_index) {
                    continue;
                }
                reduced.push(dependency);
                set_bit(&mut reachable[predecessor_index], successor_index);
                let successor_reachability = reachable[successor_index].clone();
                union_bits(&mut reachable[predecessor_index], &successor_reachability);
            }
        }
        reduced.sort_unstable();
        reduced
    }

    fn pre_reduction_dependency_count(
        &self,
        assigned_wave: &[Option<usize>],
    ) -> Result<usize, SchedulingError> {
        Ok(self.dependencies_before_reduction(assigned_wave)?.len())
    }

    fn should_explore(
        &self,
        tx_index: TxIndex,
        config: &RiskBoundedSchedulerConfig,
        exploration_used: usize,
    ) -> bool {
        if config.exploration_rate <= 0.0
            || config.exploration_max_transactions_per_block == 0
            || exploration_used >= config.exploration_max_transactions_per_block
        {
            return false;
        }
        let explicit_uncertainty = self.soft_neighbors[tx_index.0 as usize]
            .iter()
            .filter(|(_, risk, _)| *risk > config.risk_budget)
            .map(|(_, _, confidence)| (1.0 - *confidence).clamp(0.0, 1.0))
            .fold(0.0_f64, f64::max);
        let group_uncertainty = self.compact_soft_memberships[tx_index.0 as usize]
            .iter()
            .filter_map(|group_index| self.compact_soft_groups.get(*group_index))
            .filter(|group| group.scheduling_risk > config.risk_budget)
            .map(|group| (1.0 - group.confidence).clamp(0.0, 1.0))
            .fold(0.0_f64, f64::max);
        let uncertainty = explicit_uncertainty.max(group_uncertainty);
        if uncertainty < config.exploration_min_uncertainty {
            return false;
        }
        deterministic_exploration_sample(tx_index)
            < (config.exploration_rate * uncertainty).clamp(0.0, 1.0)
    }

    fn soft_risk_with_group_counts(
        &self,
        tx_index: TxIndex,
        wave_index: usize,
        assigned_wave: &[Option<usize>],
        compact_group_wave_counts: &[BTreeMap<usize, usize>],
    ) -> f64 {
        let mut independence_probability = 1.0;
        for &(neighbor, probability, _) in &self.soft_neighbors[tx_index.0 as usize] {
            if assigned_wave[neighbor.0 as usize] == Some(wave_index) {
                independence_probability *= 1.0 - probability;
            }
        }
        for group_index in &self.compact_soft_memberships[tx_index.0 as usize] {
            let group = &self.compact_soft_groups[*group_index];
            let same_wave_members = compact_group_wave_counts[*group_index]
                .get(&wave_index)
                .copied()
                .unwrap_or(0);
            independence_probability *=
                (1.0 - group.scheduling_risk).powi(same_wave_members as i32);
        }
        1.0 - independence_probability
    }
}

fn bit_is_set(bits: &[u64], index: usize) -> bool {
    bits[index / 64] & (1_u64 << (index % 64)) != 0
}

fn set_bit(bits: &mut [u64], index: usize) {
    bits[index / 64] |= 1_u64 << (index % 64);
}

fn union_bits(target: &mut [u64], source: &[u64]) {
    for (target, source) in target.iter_mut().zip(source) {
        *target |= *source;
    }
}

fn wave_has_capacity(wave: &ScheduledWave, max_wave_width: Option<usize>) -> bool {
    if let Some(maximum) = max_wave_width {
        return wave.transaction_indices.len() < maximum;
    }
    true
}

fn deterministic_exploration_sample(tx_index: TxIndex) -> f64 {
    let mut value = u64::from(tx_index.0).wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^= value >> 31;
    value as f64 / u64::MAX as f64
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
    #[error("hard dependency {predecessor:?} -> {successor:?} is violated by waves {predecessor_wave} and {successor_wave}")]
    HardDependencyViolation {
        predecessor: TxIndex,
        successor: TxIndex,
        predecessor_wave: usize,
        successor_wave: usize,
    },
    #[error("soft relationship {predecessor:?} -> {successor:?} is reversed by waves {predecessor_wave} and {successor_wave}")]
    SoftOrderingViolation {
        predecessor: TxIndex,
        successor: TxIndex,
        predecessor_wave: usize,
        successor_wave: usize,
    },
    #[error("schedule dependency set does not match candidate-graph classes and wave placement")]
    DependencySetMismatch,
    #[error(
        "dependency-reduction metadata mismatch: pre-reduction expected {expected_pre_reduction}, \
         actual {actual_pre_reduction}; elided expected {expected_elided}, actual {actual_elided}"
    )]
    DependencyReductionMetadataMismatch {
        expected_pre_reduction: usize,
        actual_pre_reduction: usize,
        expected_elided: usize,
        actual_elided: usize,
    },
    #[error("transaction {tx_index:?} has soft risk {risk} in wave {wave_index}, exceeding budget {budget}")]
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
    use acg_feedback::RuntimeEdgeId;
    use acg_predicate::{InputBindings, PredicateResult};

    use super::*;
    use crate::{
        finish_graph, quantize_q16, CandidateTransaction, EdgeProvenance, TransactionEdge,
    };

    fn tx(id: u64, predicted_position: u32) -> CandidateTransaction {
        CandidateTransaction {
            tx_id: TxId(id),
            predicted_position,
            inclusion_probability: 1.0,
            profile_id: ProfileId(0),
            instance_id: InstanceId(0),
            input_bindings: InputBindings::empty(),
            estimated_execution_cost: 10,
        }
    }

    fn edge_with(
        source: u32,
        target: u32,
        probability: f64,
        predicate_result: PredicateResult,
        observations: u32,
    ) -> TransactionEdge {
        TransactionEdge {
            source: TxIndex(source),
            target: TxIndex(target),
            provenance: EdgeProvenance::Static {
                profile_edge_index: ProfileEdgeIndex(0),
            },
            predicate_result,
            conflict_kinds: ConflictKinds::WRITE_WRITE,
            probability_q16: quantize_q16(probability),
            confidence_q16: 0,
            concrete_conflict_observations: 0,
            concrete_independent_observations: observations,
            scheduling_risk_q16: quantize_q16(probability),
            expected_replay_cost_nanos: 0,
            expected_invalidated_descendants_milli: 0,
            replay_cost_confidence_q16: 0,
            expected_serialization_cost_nanos: 0,
            serialization_cost_confidence_q16: 0,
        }
    }

    fn true_edge(source: u32, target: u32, probability: f64) -> TransactionEdge {
        edge_with(source, target, probability, PredicateResult::True, 0)
    }

    fn soft_unknown(source: u32, target: u32, probability: f64) -> TransactionEdge {
        edge_with(source, target, probability, PredicateResult::Unknown, 0)
    }

    fn runtime_edge(
        source: u32,
        target: u32,
        probability: f64,
        observations: u32,
    ) -> TransactionEdge {
        TransactionEdge {
            source: TxIndex(source),
            target: TxIndex(target),
            provenance: EdgeProvenance::RuntimeDiscovered {
                runtime_edge_id: RuntimeEdgeId(0),
            },
            predicate_result: PredicateResult::Unknown,
            conflict_kinds: ConflictKinds::WRITE_WRITE,
            probability_q16: quantize_q16(probability),
            confidence_q16: 0,
            concrete_conflict_observations: 0,
            concrete_independent_observations: observations,
            scheduling_risk_q16: quantize_q16(probability),
            expected_replay_cost_nanos: 0,
            expected_invalidated_descendants_milli: 0,
            replay_cost_confidence_q16: 0,
            expected_serialization_cost_nanos: 0,
            serialization_cost_confidence_q16: 0,
        }
    }

    fn graph(
        transactions: Vec<CandidateTransaction>,
        edges: Vec<TransactionEdge>,
    ) -> CandidateGraph {
        finish_graph(transactions, edges, Vec::new()).unwrap()
    }

    fn config(soft: f64, hard: f64, risk: f64) -> RiskBoundedSchedulerConfig {
        RiskBoundedSchedulerConfig {
            soft_threshold: soft,
            hard_threshold: hard,
            risk_budget: risk,
            max_wave_width: None,
            exploration_rate: 0.0,
            exploration_risk_budget: 0.90,
            exploration_min_uncertainty: 0.35,
            exploration_max_transactions_per_block: 0,
            independent_observations_before_softening: 8,
        }
    }

    #[test]
    fn symbolic_true_starts_hard_even_with_soft_prior() {
        let cfg = config(0.2, 0.8, 0.2);
        assert_eq!(cfg.classify(&true_edge(0, 1, 0.25)), EdgeClass::Hard);
    }

    #[test]
    fn repeated_concrete_conflicts_do_not_mature_symbolic_softening_gate() {
        let cfg = config(0.2, 0.8, 0.2);
        let mut edge = true_edge(0, 1, 0.10);
        edge.concrete_conflict_observations = 100;
        edge.concrete_independent_observations = 0;
        assert_eq!(cfg.classify(&edge), EdgeClass::Hard);
    }

    #[test]
    fn symbolic_true_softens_after_concrete_negative_evidence_lowers_probability() {
        let cfg = config(0.2, 0.8, 0.2);
        let edge = edge_with(0, 1, 0.25, PredicateResult::True, 8);
        assert_eq!(cfg.classify(&edge), EdgeClass::Soft);

        let still_hard = edge_with(0, 1, 0.85, PredicateResult::True, 8);
        assert_eq!(cfg.classify(&still_hard), EdgeClass::Hard);
    }

    #[test]
    fn zero_softening_threshold_keeps_known_topology_hard() {
        let mut cfg = config(0.2, 0.8, 0.2);
        cfg.independent_observations_before_softening = 0;
        let edge = edge_with(0, 1, 0.01, PredicateResult::True, 10_000);
        assert_eq!(cfg.classify(&edge), EdgeClass::Hard);
    }

    #[test]
    fn runtime_discovered_relationship_starts_hard_and_can_soften_after_evidence() {
        let cfg = config(0.2, 0.8, 0.2);
        assert_eq!(cfg.classify(&runtime_edge(0, 1, 0.10, 0)), EdgeClass::Hard);
        assert_eq!(cfg.classify(&runtime_edge(0, 1, 0.10, 8)), EdgeClass::Soft);
    }

    #[test]
    fn historical_false_override_is_known_topology_and_starts_hard() {
        let cfg = config(0.2, 0.8, 0.2);
        let edge = edge_with(0, 1, 0.10, PredicateResult::False, 1);
        assert!(edge.is_historical_override());
        assert_eq!(cfg.classify(&edge), EdgeClass::Hard);
    }

    #[test]
    fn ordinary_unknown_edges_use_probability_thresholds() {
        let cfg = config(0.2, 0.8, 0.2);
        assert_eq!(cfg.classify(&soft_unknown(0, 1, 0.1)), EdgeClass::Low);
        assert_eq!(cfg.classify(&soft_unknown(0, 1, 0.2)), EdgeClass::Soft);
        assert_eq!(cfg.classify(&soft_unknown(0, 1, 0.8)), EdgeClass::Hard);
    }

    #[test]
    fn independent_transactions_share_the_first_level() {
        let graph = graph(vec![tx(1, 0), tx(2, 1), tx(3, 2)], vec![]);
        let scheduler = RiskBoundedScheduler::new(config(0.2, 0.8, 0.2)).unwrap();
        let schedule = scheduler.schedule(&graph).unwrap();
        assert_eq!(schedule.waves.len(), 1);
        assert!(schedule.ordering_dependencies.is_empty());
        schedule
            .validate_against(&graph, scheduler.config())
            .unwrap();
    }

    #[test]
    fn hard_edge_uses_canonical_predicted_order_and_emits_dependency() {
        let graph = graph(vec![tx(1, 20), tx(2, 10)], vec![true_edge(0, 1, 0.25)]);
        let scheduler = RiskBoundedScheduler::new(config(0.2, 0.8, 0.2)).unwrap();
        let schedule = scheduler.schedule(&graph).unwrap();
        assert_eq!(schedule.wave_for(TxIndex(1)), Some(0));
        assert_eq!(schedule.wave_for(TxIndex(0)), Some(1));
        assert_eq!(
            schedule.ordering_dependencies,
            vec![ScheduledDependency {
                predecessor: TxIndex(1),
                successor: TxIndex(0),
                class: EdgeClass::Hard,
            }]
        );
    }

    #[test]
    fn dense_hard_dag_is_transitively_reduced_without_changing_reachability() {
        let graph = graph(
            vec![tx(1, 0), tx(2, 1), tx(3, 2), tx(4, 3)],
            vec![
                true_edge(0, 1, 0.9),
                true_edge(0, 2, 0.9),
                true_edge(0, 3, 0.9),
                true_edge(1, 2, 0.9),
                true_edge(1, 3, 0.9),
                true_edge(2, 3, 0.9),
            ],
        );
        let scheduler = RiskBoundedScheduler::new(config(0.2, 0.8, 0.2)).unwrap();
        let schedule = scheduler.schedule(&graph).unwrap();

        assert_eq!(schedule.pre_reduction_ordering_dependencies, 6);
        assert_eq!(schedule.hard_dependencies_elided_by_reduction, 3);
        assert_eq!(
            schedule.ordering_dependencies,
            vec![
                ScheduledDependency {
                    predecessor: TxIndex(0),
                    successor: TxIndex(1),
                    class: EdgeClass::Hard,
                },
                ScheduledDependency {
                    predecessor: TxIndex(1),
                    successor: TxIndex(2),
                    class: EdgeClass::Hard,
                },
                ScheduledDependency {
                    predecessor: TxIndex(2),
                    successor: TxIndex(3),
                    class: EdgeClass::Hard,
                },
            ]
        );
        assert_eq!(schedule.waves.len(), 4);
        schedule
            .validate_against(&graph, scheduler.config())
            .unwrap();

        let mut malformed = schedule.clone();
        malformed.hard_dependencies_elided_by_reduction -= 1;
        assert!(matches!(
            malformed.validate_against(&graph, scheduler.config()),
            Err(SchedulingError::DependencyReductionMetadataMismatch { .. })
        ));
    }

    #[test]
    fn dense_soft_ordering_is_transitively_reduced_after_wave_placement() {
        let graph = graph(
            vec![tx(1, 0), tx(2, 1), tx(3, 2), tx(4, 3)],
            vec![
                soft_unknown(0, 1, 0.30),
                soft_unknown(0, 2, 0.30),
                soft_unknown(0, 3, 0.30),
                soft_unknown(1, 2, 0.30),
                soft_unknown(1, 3, 0.30),
                soft_unknown(2, 3, 0.30),
            ],
        );
        let scheduler = RiskBoundedScheduler::new(config(0.20, 0.80, 0.0)).unwrap();
        let schedule = scheduler.schedule(&graph).unwrap();

        assert_eq!(schedule.pre_reduction_ordering_dependencies, 6);
        assert_eq!(schedule.hard_dependencies_elided_by_reduction, 3);
        assert_eq!(schedule.ordering_dependencies.len(), 3);
        assert!(schedule
            .ordering_dependencies
            .iter()
            .all(|dependency| dependency.class == EdgeClass::Soft));
        schedule
            .validate_against(&graph, scheduler.config())
            .unwrap();
    }

    #[test]
    fn deterministic_exploration_can_raise_the_effective_soft_risk_budget() {
        let mut cfg = config(0.20, 0.80, 0.20);
        cfg.exploration_rate = 1.0;
        cfg.exploration_risk_budget = 0.90;
        cfg.exploration_min_uncertainty = 0.35;
        cfg.exploration_max_transactions_per_block = 1;
        let graph = graph(vec![tx(1, 0), tx(2, 1)], vec![soft_unknown(0, 1, 0.50)]);
        let schedule = RiskBoundedScheduler::new(cfg)
            .unwrap()
            .schedule(&graph)
            .unwrap();
        assert_eq!(schedule.waves.len(), 1);
        assert!(schedule.ordering_dependencies.is_empty());
    }

    #[test]
    fn targeted_exploration_skips_confident_soft_relationships() {
        let mut cfg = config(0.20, 0.80, 0.20);
        cfg.exploration_rate = 1.0;
        cfg.exploration_risk_budget = 0.90;
        cfg.exploration_min_uncertainty = 0.35;
        cfg.exploration_max_transactions_per_block = 1;
        let mut edge = soft_unknown(0, 1, 0.50);
        edge.confidence_q16 = quantize_q16(0.90);
        let graph = graph(vec![tx(1, 0), tx(2, 1)], vec![edge]);
        let schedule = RiskBoundedScheduler::new(cfg)
            .unwrap()
            .schedule(&graph)
            .unwrap();
        assert_eq!(schedule.waves.len(), 2);
        assert_eq!(schedule.ordering_dependencies.len(), 1);
    }

    #[test]
    fn soft_pair_can_share_a_level_at_the_risk_boundary() {
        let edge = soft_unknown(0, 1, 0.25);
        let probability = edge.probability();
        let graph = graph(vec![tx(1, 0), tx(2, 1)], vec![edge]);
        let schedule = RiskBoundedScheduler::new(config(0.2, 0.8, probability))
            .unwrap()
            .schedule(&graph)
            .unwrap();
        assert_eq!(schedule.waves.len(), 1);
        assert!(schedule.ordering_dependencies.is_empty());
    }

    #[test]
    fn separated_soft_pair_becomes_an_execution_dependency() {
        let graph = graph(vec![tx(1, 0), tx(2, 1)], vec![soft_unknown(0, 1, 0.30)]);
        let schedule = RiskBoundedScheduler::new(config(0.2, 0.8, 0.20))
            .unwrap()
            .schedule(&graph)
            .unwrap();
        assert_eq!(schedule.wave_for(TxIndex(0)), Some(0));
        assert_eq!(schedule.wave_for(TxIndex(1)), Some(1));
        assert_eq!(
            schedule.ordering_dependencies,
            vec![ScheduledDependency {
                predecessor: TxIndex(0),
                successor: TxIndex(1),
                class: EdgeClass::Soft,
            }]
        );
    }

    #[test]
    fn soft_relationship_never_inverts_canonical_order() {
        let graph = graph(
            vec![tx(1, 0), tx(2, 1), tx(3, 2)],
            vec![true_edge(0, 1, 0.1), soft_unknown(1, 2, 0.30)],
        );
        let schedule = RiskBoundedScheduler::new(config(0.2, 0.8, 0.20))
            .unwrap()
            .schedule(&graph)
            .unwrap();
        assert!(schedule.wave_for(TxIndex(0)) < schedule.wave_for(TxIndex(1)));
        assert!(schedule.wave_for(TxIndex(1)) < schedule.wave_for(TxIndex(2)));
    }

    #[test]
    fn max_wave_width_still_bounds_reported_levels() {
        let graph = graph(
            vec![tx(1, 0), tx(2, 1), tx(3, 2), tx(4, 3), tx(5, 4)],
            vec![],
        );
        let mut cfg = config(0.2, 0.8, 0.2);
        cfg.max_wave_width = Some(2);
        let schedule = RiskBoundedScheduler::new(cfg)
            .unwrap()
            .schedule(&graph)
            .unwrap();
        assert_eq!(
            schedule
                .waves
                .iter()
                .map(|wave| wave.transaction_indices.len())
                .collect::<Vec<_>>(),
            vec![2, 2, 1]
        );
    }

    #[test]
    fn invalid_configuration_is_rejected() {
        assert!(matches!(
            RiskBoundedScheduler::new(config(f64::NAN, 0.8, 0.2)),
            Err(SchedulingError::InvalidProbability { .. })
        ));
        assert!(matches!(
            RiskBoundedScheduler::new(config(0.9, 0.8, 0.2)),
            Err(SchedulingError::ThresholdOrder { .. })
        ));
        let mut zero = config(0.2, 0.8, 0.2);
        zero.max_wave_width = Some(0);
        assert_eq!(
            RiskBoundedScheduler::new(zero).unwrap_err(),
            SchedulingError::ZeroWaveWidth
        );
    }
}
