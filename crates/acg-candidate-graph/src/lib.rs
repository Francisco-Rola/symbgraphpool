//! Runtime-independent construction of per-block transaction conflict graphs.

pub mod scheduler;

pub use scheduler::{
    EdgeClass, RiskBoundedSchedule, RiskBoundedScheduler, RiskBoundedSchedulerConfig,
    ScheduledDependency, ScheduledWave, SchedulingError,
};

use std::collections::{BTreeMap, BTreeSet};

use acg_core::{ConflictKinds, InstanceId, ProfileEdgeIndex, ProfileId, TxId, TxIndex};
use acg_feedback::{
    AdaptiveFeedbackConfig, AdaptiveFeedbackStore, EdgeEstimate, FeedbackError, ReplayCostEstimate,
    RuntimeEdgeId, SerializationCostEstimate,
};
use acg_predicate::{CompiledPredicate, InputBindings, PredicateResult};
use acg_profile_graph::ProfileGraph;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const Q16_MAX: f64 = u16::MAX as f64;

#[derive(Clone, Debug, PartialEq)]
pub struct CandidateTransaction {
    pub tx_id: TxId,
    pub predicted_position: u32,
    pub inclusion_probability: f32,
    pub profile_id: ProfileId,
    pub instance_id: InstanceId,
    pub input_bindings: InputBindings,
    pub estimated_execution_cost: u32,
}

impl CandidateTransaction {
    pub fn validate(&self) -> Result<(), CandidateGraphError> {
        if !self.inclusion_probability.is_finite()
            || !(0.0..=1.0).contains(&self.inclusion_probability)
        {
            return Err(CandidateGraphError::InvalidInclusionProbability {
                tx_id: self.tx_id,
                value: self.inclusion_probability,
            });
        }
        Ok(())
    }
}

/// Where a concrete transaction edge came from.
///
/// Static edges are backed by the immutable symbolic profile graph. Runtime-discovered edges are
/// persistent topology misses learned from concrete execution.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EdgeProvenance {
    Static {
        profile_edge_index: ProfileEdgeIndex,
    },
    RuntimeDiscovered {
        runtime_edge_id: RuntimeEdgeId,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransactionEdge {
    pub source: TxIndex,
    pub target: TxIndex,
    pub provenance: EdgeProvenance,
    pub predicate_result: PredicateResult,
    pub conflict_kinds: ConflictKinds,
    /// Conflict probability quantized to `[0, 65535]` for compact candidate-graph storage.
    pub probability_q16: u16,
    /// Operational confidence quantized to `[0, 65535]`.
    pub confidence_q16: u16,
    /// Concrete executions that observed a real conflict for this relationship.
    #[serde(default)]
    pub concrete_conflict_observations: u32,
    /// Concrete executions that observed the candidate relationship to be independent.
    ///
    /// Known symbolic/runtime topology starts hard. The scheduler uses this counter, rather than
    /// total observations, as the maturity gate for hard-to-soft demotion: a relationship only
    /// becomes eligible for speculation after repeated executions *did not* observe the conflict.
    #[serde(default)]
    pub concrete_independent_observations: u32,
    /// Brick 5D cost-adjusted scheduling risk. Raw conflict probability remains separately visible.
    #[serde(default)]
    pub scheduling_risk_q16: u16,
    /// Decayed mean direct replay cost learned for this profile relationship.
    #[serde(default)]
    pub expected_replay_cost_nanos: u64,
    /// Decayed mean transitive replay fan-out, in milli-transactions.
    #[serde(default)]
    pub expected_invalidated_descendants_milli: u32,
    /// Confidence in the replay-cost estimate, quantized to Q16.
    #[serde(default)]
    pub replay_cost_confidence_q16: u16,
    /// Brick 5E learned marginal dependency-ready delay for this profile relationship.
    #[serde(default)]
    pub expected_serialization_cost_nanos: u64,
    /// Confidence in the learned serialization-cost estimate, quantized to Q16.
    #[serde(default)]
    pub serialization_cost_confidence_q16: u16,
}

impl TransactionEdge {
    pub fn profile_edge_index(&self) -> Option<ProfileEdgeIndex> {
        match self.provenance {
            EdgeProvenance::Static { profile_edge_index } => Some(profile_edge_index),
            EdgeProvenance::RuntimeDiscovered { .. } => None,
        }
    }

    pub fn runtime_edge_id(&self) -> Option<RuntimeEdgeId> {
        match self.provenance {
            EdgeProvenance::Static { .. } => None,
            EdgeProvenance::RuntimeDiscovered { runtime_edge_id } => Some(runtime_edge_id),
        }
    }

    pub fn probability(&self) -> f64 {
        dequantize_q16(self.probability_q16)
    }

    pub fn confidence(&self) -> f64 {
        dequantize_q16(self.confidence_q16)
    }

    pub fn scheduling_risk(&self) -> f64 {
        // Legacy/binary edges serialized before Brick 5D may have a zero default here. Preserve
        // their old semantics by falling back to raw probability when there is no cost evidence.
        if self.scheduling_risk_q16 == 0 && self.replay_cost_confidence_q16 == 0 {
            self.probability()
        } else {
            dequantize_q16(self.scheduling_risk_q16)
        }
    }

    pub fn replay_cost_confidence(&self) -> f64 {
        dequantize_q16(self.replay_cost_confidence_q16)
    }

    pub fn serialization_cost_confidence(&self) -> f64 {
        dequantize_q16(self.serialization_cost_confidence_q16)
    }

    /// Effective Brick 5E serialization reference after confidence-weighted blending with the
    /// configured fallback.
    pub fn effective_serialization_cost_nanos(
        &self,
        fallback_serialization_cost_nanos: u64,
    ) -> u64 {
        effective_serialization_cost_nanos(
            SerializationCostEstimate {
                expected_serialization_cost_nanos: self.expected_serialization_cost_nanos as f64,
                observation_weight: 0.0,
                confidence: self.serialization_cost_confidence(),
                observations: 0,
                total_serialization_cost_nanos: 0,
                epoch: 0,
            },
            fallback_serialization_cost_nanos,
        )
        .round()
        .clamp(1.0, u64::MAX as f64) as u64
    }

    pub fn expected_invalidated_descendants(&self) -> f64 {
        f64::from(self.expected_invalidated_descendants_milli) / 1000.0
    }

    /// A concrete symbolic predicate evaluated false, but prior runtime execution proved that
    /// candidate construction can miss this profile relationship.
    pub fn is_historical_override(&self) -> bool {
        matches!(self.provenance, EdgeProvenance::Static { .. })
            && self.predicate_result == PredicateResult::False
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransactionAdjacency {
    pub neighbor: TxIndex,
    pub edge_index: u32,
}

#[derive(Clone, Debug)]
pub struct CompactCandidateGroup {
    provenance: EdgeProvenance,
    members: Vec<TxIndex>,
    logical_edges: usize,
    edge_template: TransactionEdge,
}

impl CompactCandidateGroup {
    pub fn provenance(&self) -> EdgeProvenance {
        self.provenance
    }

    pub fn members(&self) -> &[TxIndex] {
        &self.members
    }

    pub fn logical_edges(&self) -> usize {
        self.logical_edges
    }

    pub fn edge_template(&self) -> &TransactionEdge {
        &self.edge_template
    }
}

#[derive(Clone, Debug)]
pub struct CandidateGraph {
    transactions: Vec<CandidateTransaction>,
    edges: Vec<TransactionEdge>,
    compact_groups: Vec<CompactCandidateGroup>,
    compact_memberships: Vec<Vec<u32>>,
    adjacency_offsets: Vec<u32>,
    adjacency_entries: Vec<TransactionAdjacency>,
}

impl CandidateGraph {
    /// Build a transaction-only graph for an explicit serial-bypass plan.
    ///
    /// The bypass path deliberately skips candidate-edge materialization and adaptive feedback for
    /// the block; callers still retain transaction metadata for execution/accounting.
    pub fn from_transactions(
        transactions: Vec<CandidateTransaction>,
    ) -> Result<Self, CandidateGraphError> {
        u32::try_from(transactions.len())
            .map_err(|_| CandidateGraphError::TooManyTransactions(transactions.len()))?;
        for transaction in &transactions {
            transaction.validate()?;
        }
        finish_graph(transactions, Vec::new(), Vec::new())
    }

    pub fn transactions(&self) -> &[CandidateTransaction] {
        &self.transactions
    }

    pub fn edges(&self) -> &[TransactionEdge] {
        &self.edges
    }

    pub fn compact_groups(&self) -> &[CompactCandidateGroup] {
        &self.compact_groups
    }

    pub fn logical_edge_count(&self) -> usize {
        self.edges.len().saturating_add(
            self.compact_groups
                .iter()
                .map(|group| {
                    group
                        .logical_edges
                        .saturating_sub(group.members.len().saturating_sub(1))
                })
                .sum::<usize>(),
        )
    }

    pub fn provenance_is_compact(&self, provenance: EdgeProvenance) -> bool {
        self.compact_groups
            .iter()
            .any(|group| group.provenance == provenance)
    }

    pub fn transaction(&self, index: TxIndex) -> Option<&CandidateTransaction> {
        self.transactions.get(index.0 as usize)
    }

    pub fn neighbors(&self, index: TxIndex) -> &[TransactionAdjacency] {
        let Some(start) = self.adjacency_offsets.get(index.0 as usize).copied() else {
            return &[];
        };
        let Some(end) = self.adjacency_offsets.get(index.0 as usize + 1).copied() else {
            return &[];
        };
        &self.adjacency_entries[start as usize..end as usize]
    }

    pub fn edge_between(&self, left: TxIndex, right: TxIndex) -> Option<&TransactionEdge> {
        let neighbors = self.neighbors(left);
        let offset = neighbors
            .binary_search_by_key(&right, |entry| entry.neighbor)
            .ok()?;
        self.edges.get(neighbors[offset].edge_index as usize)
    }

    pub fn candidate_provenance_between(
        &self,
        left: TxIndex,
        right: TxIndex,
    ) -> Option<EdgeProvenance> {
        if let Some(edge) = self.edge_between(left, right) {
            return Some(edge.provenance);
        }
        let left_groups = self.compact_memberships.get(left.0 as usize)?;
        let right_groups = self.compact_memberships.get(right.0 as usize)?;
        let mut left_offset = 0;
        let mut right_offset = 0;
        while left_offset < left_groups.len() && right_offset < right_groups.len() {
            match left_groups[left_offset].cmp(&right_groups[right_offset]) {
                std::cmp::Ordering::Less => left_offset += 1,
                std::cmp::Ordering::Greater => right_offset += 1,
                std::cmp::Ordering::Equal => {
                    return self
                        .compact_groups
                        .get(left_groups[left_offset] as usize)
                        .map(|group| group.provenance);
                }
            }
        }
        None
    }

    pub fn contains_candidate_pair(&self, left: TxIndex, right: TxIndex) -> bool {
        self.candidate_provenance_between(left, right).is_some()
    }
}

/// Brick 5D conversion from conflict probability + measured replay impact into scheduling risk.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CostAwareEdgePolicyConfig {
    /// Fallback wall-time cost used until Brick 5E has confident learned serialization evidence.
    /// The field name is retained for source compatibility with Brick 5D configuration.
    pub serialization_cost_reference_nanos: u64,
    /// Additional penalty per expected transitive invalidation descendant.
    pub invalidation_fanout_weight: f64,
}

impl Default for CostAwareEdgePolicyConfig {
    fn default() -> Self {
        Self {
            serialization_cost_reference_nanos: 250_000,
            invalidation_fanout_weight: 0.50,
        }
    }
}

impl CostAwareEdgePolicyConfig {
    pub fn validate(&self) -> Result<(), CandidateGraphError> {
        if self.serialization_cost_reference_nanos == 0 {
            return Err(CandidateGraphError::ZeroSerializationCostReference);
        }
        if !self.invalidation_fanout_weight.is_finite() || self.invalidation_fanout_weight < 0.0 {
            return Err(CandidateGraphError::InvalidInvalidationFanoutWeight(
                self.invalidation_fanout_weight,
            ));
        }
        Ok(())
    }
}

/// Inputs controlling adaptive candidate-edge materialization.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct WeightedCandidateGraphConfig {
    /// Epoch at which adaptive edge statistics are projected for this candidate block.
    pub epoch: u64,
    /// Posterior floor for ordinary unresolved (`Unknown`) static relationships.
    ///
    /// Concrete `True` predicate matches, historical candidate-miss overrides, and persisted
    /// runtime-discovered topology stay materialized so learning can demote them from Hard to
    /// Soft without silently deleting a known dependency relationship.
    pub edge_materialization_threshold: f64,
    /// Cost-aware Brick 5D policy. With no replay-cost evidence, scheduling risk exactly matches
    /// the Brick 4 posterior probability.
    pub cost_policy: CostAwareEdgePolicyConfig,
    /// Compact provable equivalence cliques into canonical chains while the relationship is
    /// guaranteed hard by the scheduler's independence-maturity gate.
    pub compact_immature_equivalence_edges: bool,
    pub independent_observations_before_softening: u32,
}

impl WeightedCandidateGraphConfig {
    pub fn validate(&self) -> Result<(), CandidateGraphError> {
        if !self.edge_materialization_threshold.is_finite()
            || !(0.0..=1.0).contains(&self.edge_materialization_threshold)
        {
            return Err(CandidateGraphError::InvalidMaterializationThreshold(
                self.edge_materialization_threshold,
            ));
        }
        self.cost_policy.validate()?;
        Ok(())
    }
}

#[derive(Debug)]
pub struct CandidateGraphBuilder<'graph> {
    profile_graph: &'graph ProfileGraph,
    compiled_predicates: Vec<CompiledPredicate>,
    profile_count: usize,
}

impl<'graph> CandidateGraphBuilder<'graph> {
    pub fn new(profile_graph: &'graph ProfileGraph) -> Self {
        let compiled_predicates = profile_graph
            .edges()
            .iter()
            .map(|edge| CompiledPredicate::compile(&edge.predicate))
            .collect();
        Self {
            profile_graph,
            compiled_predicates,
            profile_count: profile_graph.profiles().len(),
        }
    }

    /// Builds the pre-Brick-4 binary graph.
    ///
    /// Materialized symbolic edges carry probability 1.0 and confidence 0.0. Keeping this path
    /// intact provides the binary-graph baseline and preserves existing callers while Brick 4
    /// introduces adaptive construction through [`Self::build_weighted`].
    pub fn build(
        &self,
        transactions: Vec<CandidateTransaction>,
    ) -> Result<CandidateGraph, CandidateGraphError> {
        let (buckets, instance_buckets) = self.prepare_transactions(&transactions)?;
        let mut edges = Vec::<TransactionEdge>::new();
        let mut compact_groups = Vec::<CompactCandidateGroup>::new();
        let materialization_context = StaticMaterializationContext {
            transactions: &transactions,
            buckets: &buckets,
            instance_buckets: &instance_buckets,
            compiled_predicates: &self.compiled_predicates,
        };
        let mut visited_profile_edges = BTreeSet::<ProfileEdgeIndex>::new();

        for (profile_offset, bucket) in buckets.iter().enumerate() {
            if bucket.is_empty() {
                continue;
            }
            let profile_id = ProfileId(
                u32::try_from(profile_offset).expect("profile count is represented by ProfileId"),
            );
            for adjacency in self.profile_graph.neighbors(profile_id) {
                if buckets[adjacency.neighbor.0 as usize].is_empty()
                    || !visited_profile_edges.insert(adjacency.edge_index)
                {
                    continue;
                }
                let profile_edge = &self.profile_graph.edges()[adjacency.edge_index.0 as usize];
                materialize_static_profile_edge(
                    &materialization_context,
                    &mut edges,
                    &mut compact_groups,
                    profile_edge,
                    StaticMaterialization::Binary,
                )?;
            }
        }

        finish_graph(transactions, edges, compact_groups)
    }

    /// Builds an adaptive weighted candidate graph from symbolic predicates plus runtime history.
    ///
    /// The profile posterior is the primary probability estimate. A concrete `False` predicate is
    /// still pruned unless concrete execution has previously recorded a candidate miss for that
    /// static edge. Runtime-discovered fallback topology is traversed alongside static adjacency.
    pub fn build_weighted(
        &self,
        transactions: Vec<CandidateTransaction>,
        feedback_store: &AdaptiveFeedbackStore,
        feedback_config: &AdaptiveFeedbackConfig,
        config: WeightedCandidateGraphConfig,
    ) -> Result<CandidateGraph, CandidateGraphError> {
        config.validate()?;
        feedback_config.validate()?;
        let (buckets, instance_buckets) = self.prepare_transactions(&transactions)?;
        let mut edges = Vec::<TransactionEdge>::new();
        let mut compact_groups = Vec::<CompactCandidateGroup>::new();
        let materialization_context = StaticMaterializationContext {
            transactions: &transactions,
            buckets: &buckets,
            instance_buckets: &instance_buckets,
            compiled_predicates: &self.compiled_predicates,
        };
        let mut visited_profile_edges = BTreeSet::<ProfileEdgeIndex>::new();
        let mut visited_runtime_edges = BTreeSet::<RuntimeEdgeId>::new();

        for (profile_offset, bucket) in buckets.iter().enumerate() {
            if bucket.is_empty() {
                continue;
            }
            let profile_id = ProfileId(
                u32::try_from(profile_offset).expect("profile count is represented by ProfileId"),
            );

            for adjacency in self.profile_graph.neighbors(profile_id) {
                if buckets[adjacency.neighbor.0 as usize].is_empty()
                    || !visited_profile_edges.insert(adjacency.edge_index)
                {
                    continue;
                }
                let estimate = feedback_store.estimate_static_edge(
                    adjacency.edge_index,
                    config.epoch,
                    feedback_config,
                )?;
                let replay_cost = feedback_store.estimate_static_replay_cost(
                    adjacency.edge_index,
                    config.epoch,
                    feedback_config,
                )?;
                let serialization_cost = feedback_store.estimate_static_serialization_cost(
                    adjacency.edge_index,
                    config.epoch,
                    feedback_config,
                )?;
                let profile_edge = &self.profile_graph.edges()[adjacency.edge_index.0 as usize];
                let adaptive_materialization = AdaptiveMaterialization {
                    estimate,
                    replay_cost,
                    serialization_cost,
                    edge_materialization_threshold: config.edge_materialization_threshold,
                    cost_policy: config.cost_policy,
                    compact_immature_equivalence_edges: config.compact_immature_equivalence_edges,
                    independent_observations_before_softening: config
                        .independent_observations_before_softening,
                };
                materialize_static_profile_edge(
                    &materialization_context,
                    &mut edges,
                    &mut compact_groups,
                    profile_edge,
                    StaticMaterialization::Adaptive(&adaptive_materialization),
                )?;
            }

            for fallback in feedback_store.fallback_edges_for_profile(profile_id) {
                if !visited_runtime_edges.insert(fallback.id) {
                    continue;
                }
                let other = if fallback.source == profile_id {
                    fallback.target
                } else {
                    fallback.source
                };
                if buckets[other.0 as usize].is_empty() {
                    continue;
                }
                let estimate = feedback_store.estimate_fallback_edge(
                    fallback.id,
                    config.epoch,
                    feedback_config,
                )?;
                let replay_cost = feedback_store.estimate_fallback_replay_cost(
                    fallback.id,
                    config.epoch,
                    feedback_config,
                )?;
                let serialization_cost = feedback_store.estimate_fallback_serialization_cost(
                    fallback.id,
                    config.epoch,
                    feedback_config,
                )?;
                materialize_runtime_fallback_edge(
                    &buckets,
                    &mut edges,
                    fallback,
                    estimate,
                    replay_cost,
                    serialization_cost,
                    config.cost_policy,
                );
            }
        }

        finish_graph(transactions, edges, compact_groups)
    }

    fn prepare_transactions(
        &self,
        transactions: &[CandidateTransaction],
    ) -> Result<PreparedBuckets, CandidateGraphError> {
        u32::try_from(transactions.len())
            .map_err(|_| CandidateGraphError::TooManyTransactions(transactions.len()))?;

        let mut buckets = vec![Vec::<TxIndex>::new(); self.profile_count];
        let mut instance_buckets =
            vec![BTreeMap::<InstanceId, Vec<TxIndex>>::new(); self.profile_count];
        for (index, transaction) in transactions.iter().enumerate() {
            transaction.validate()?;
            let profile_index = transaction.profile_id.0 as usize;
            if profile_index >= self.profile_count {
                return Err(CandidateGraphError::UnknownProfileId {
                    tx_id: transaction.tx_id,
                    profile_id: transaction.profile_id,
                });
            }
            let tx_index = TxIndex(u32::try_from(index).expect("transaction count validated"));
            buckets[profile_index].push(tx_index);
            instance_buckets[profile_index]
                .entry(transaction.instance_id)
                .or_default()
                .push(tx_index);
        }
        Ok((buckets, instance_buckets))
    }
}

type PreparedBuckets = (Vec<Vec<TxIndex>>, Vec<BTreeMap<InstanceId, Vec<TxIndex>>>);

#[derive(Clone, Copy)]
struct AdaptiveMaterialization {
    estimate: EdgeEstimate,
    replay_cost: ReplayCostEstimate,
    serialization_cost: SerializationCostEstimate,
    edge_materialization_threshold: f64,
    cost_policy: CostAwareEdgePolicyConfig,
    compact_immature_equivalence_edges: bool,
    independent_observations_before_softening: u32,
}

#[derive(Clone, Copy)]
enum StaticMaterialization<'a> {
    Binary,
    Adaptive(&'a AdaptiveMaterialization),
}

impl StaticMaterialization<'_> {
    fn may_compact_equivalence_clique(self) -> bool {
        match self {
            Self::Binary => false,
            Self::Adaptive(adaptive) => {
                adaptive.compact_immature_equivalence_edges
                    && adaptive.independent_observations_before_softening != 0
                    && adaptive.estimate.negative_observations
                        < u64::from(adaptive.independent_observations_before_softening)
            }
        }
    }

    fn has_candidate_miss_history(self) -> bool {
        match self {
            Self::Binary => false,
            Self::Adaptive(adaptive) => adaptive.estimate.has_candidate_miss_history(),
        }
    }

    fn should_materialize(self, predicate_result: PredicateResult) -> bool {
        match self {
            Self::Binary => predicate_result != PredicateResult::False,
            Self::Adaptive(adaptive) => match predicate_result {
                // A concrete predicate match is known symbolic topology. Keep it present so
                // concrete execution can soften the relationship rather than erase it.
                PredicateResult::True => true,
                // A False predicate is normally pruned, unless runtime execution has already
                // demonstrated that this static pruning rule can miss a real dependency.
                PredicateResult::False => adaptive.estimate.has_candidate_miss_history(),
                // Only unresolved static topology remains subject to the generic materialization
                // floor.
                PredicateResult::Unknown => {
                    adaptive.estimate.probability >= adaptive.edge_materialization_threshold
                }
            },
        }
    }

    fn edge_metrics(self) -> EdgeMetrics {
        match self {
            Self::Binary => EdgeMetrics {
                probability_q16: u16::MAX,
                confidence_q16: 0,
                concrete_conflict_observations: 0,
                concrete_independent_observations: 0,
                scheduling_risk_q16: u16::MAX,
                expected_replay_cost_nanos: 0,
                expected_invalidated_descendants_milli: 0,
                replay_cost_confidence_q16: 0,
                expected_serialization_cost_nanos: 0,
                serialization_cost_confidence_q16: 0,
            },
            Self::Adaptive(adaptive) => edge_metrics(
                adaptive.estimate,
                adaptive.replay_cost,
                adaptive.serialization_cost,
                adaptive.cost_policy,
            ),
        }
    }
}

struct StaticMaterializationContext<'a> {
    transactions: &'a [CandidateTransaction],
    buckets: &'a [Vec<TxIndex>],
    instance_buckets: &'a [BTreeMap<InstanceId, Vec<TxIndex>>],
    compiled_predicates: &'a [CompiledPredicate],
}

fn materialize_static_profile_edge(
    context: &StaticMaterializationContext<'_>,
    edges: &mut Vec<TransactionEdge>,
    compact_groups: &mut Vec<CompactCandidateGroup>,
    profile_edge: &acg_profile_graph::LoadedProfileEdge,
    mode: StaticMaterialization<'_>,
) -> Result<(), CandidateGraphError> {
    let source_bucket = &context.buckets[profile_edge.source.0 as usize];
    let target_bucket = &context.buckets[profile_edge.target.0 as usize];
    let predicate = &context.compiled_predicates[profile_edge.index.0 as usize];

    // Once concrete execution has disproved candidate pruning for this relationship, do not let
    // the predicate's static same-instance fast path hide future pairs before the predicate can be
    // evaluated and the historical override policy applied.
    let may_use_instance_fast_path =
        predicate.requires_same_instance() && !mode.has_candidate_miss_history();

    if profile_edge.source == profile_edge.target
        && may_use_instance_fast_path
        && mode.may_compact_equivalence_clique()
        && materialize_equivalence_chains(
            context.transactions,
            edges,
            compact_groups,
            profile_edge,
            predicate,
            source_bucket,
            mode,
        )?
    {
        return Ok(());
    }

    if may_use_instance_fast_path {
        let source_instances = &context.instance_buckets[profile_edge.source.0 as usize];
        let target_instances = &context.instance_buckets[profile_edge.target.0 as usize];
        if profile_edge.source == profile_edge.target {
            for instance_bucket in source_instances.values() {
                materialize_same_bucket_pairs(
                    context.transactions,
                    edges,
                    profile_edge,
                    predicate,
                    instance_bucket,
                    mode,
                )?;
            }
        } else {
            for (instance_id, source_transactions) in source_instances {
                let Some(target_transactions) = target_instances.get(instance_id) else {
                    continue;
                };
                materialize_cross_bucket_pairs(
                    context.transactions,
                    edges,
                    profile_edge,
                    predicate,
                    source_transactions,
                    target_transactions,
                    mode,
                )?;
            }
        }
    } else if profile_edge.source == profile_edge.target {
        materialize_same_bucket_pairs(
            context.transactions,
            edges,
            profile_edge,
            predicate,
            source_bucket,
            mode,
        )?;
    } else {
        materialize_cross_bucket_pairs(
            context.transactions,
            edges,
            profile_edge,
            predicate,
            source_bucket,
            target_bucket,
            mode,
        )?;
    }
    Ok(())
}

fn materialize_equivalence_chains(
    transactions: &[CandidateTransaction],
    edges: &mut Vec<TransactionEdge>,
    compact_groups: &mut Vec<CompactCandidateGroup>,
    profile_edge: &acg_profile_graph::LoadedProfileEdge,
    predicate: &CompiledPredicate,
    bucket: &[TxIndex],
    mode: StaticMaterialization<'_>,
) -> Result<bool, CandidateGraphError> {
    let mut groups = BTreeMap::new();
    for &tx_index in bucket {
        let transaction = &transactions[tx_index.0 as usize];
        let Some(key) =
            predicate.equivalence_key(transaction.instance_id, &transaction.input_bindings)
        else {
            return Ok(false);
        };
        groups.entry(key).or_insert_with(Vec::new).push(tx_index);
    }
    let metrics = mode.edge_metrics();
    let provenance = EdgeProvenance::Static {
        profile_edge_index: profile_edge.index,
    };
    for members in groups.into_values().filter(|members| members.len() >= 2) {
        let logical_edges = members
            .len()
            .saturating_mul(members.len().saturating_sub(1))
            / 2;
        for pair in members.windows(2) {
            push_edge(
                edges,
                pair[0],
                pair[1],
                provenance,
                PredicateResult::True,
                profile_edge.conflict_kinds,
                metrics,
            );
        }
        let edge_template = TransactionEdge {
            source: members[0],
            target: members[1],
            provenance,
            predicate_result: PredicateResult::True,
            conflict_kinds: profile_edge.conflict_kinds,
            probability_q16: metrics.probability_q16,
            confidence_q16: metrics.confidence_q16,
            concrete_conflict_observations: metrics.concrete_conflict_observations,
            concrete_independent_observations: metrics.concrete_independent_observations,
            scheduling_risk_q16: metrics.scheduling_risk_q16,
            expected_replay_cost_nanos: metrics.expected_replay_cost_nanos,
            expected_invalidated_descendants_milli: metrics.expected_invalidated_descendants_milli,
            replay_cost_confidence_q16: metrics.replay_cost_confidence_q16,
            expected_serialization_cost_nanos: metrics.expected_serialization_cost_nanos,
            serialization_cost_confidence_q16: metrics.serialization_cost_confidence_q16,
        };
        compact_groups.push(CompactCandidateGroup {
            provenance,
            members,
            logical_edges,
            edge_template,
        });
    }
    Ok(true)
}

fn materialize_same_bucket_pairs(
    transactions: &[CandidateTransaction],
    edges: &mut Vec<TransactionEdge>,
    profile_edge: &acg_profile_graph::LoadedProfileEdge,
    predicate: &CompiledPredicate,
    bucket: &[TxIndex],
    mode: StaticMaterialization<'_>,
) -> Result<(), CandidateGraphError> {
    for (left_offset, &left_index) in bucket.iter().enumerate() {
        for &right_index in bucket.iter().skip(left_offset + 1) {
            maybe_materialize_static_edge(
                transactions,
                edges,
                profile_edge,
                predicate,
                left_index,
                right_index,
                mode,
            )?;
        }
    }
    Ok(())
}

fn materialize_cross_bucket_pairs(
    transactions: &[CandidateTransaction],
    edges: &mut Vec<TransactionEdge>,
    profile_edge: &acg_profile_graph::LoadedProfileEdge,
    predicate: &CompiledPredicate,
    left_bucket: &[TxIndex],
    right_bucket: &[TxIndex],
    mode: StaticMaterialization<'_>,
) -> Result<(), CandidateGraphError> {
    for &left_index in left_bucket {
        for &right_index in right_bucket {
            maybe_materialize_static_edge(
                transactions,
                edges,
                profile_edge,
                predicate,
                left_index,
                right_index,
                mode,
            )?;
        }
    }
    Ok(())
}

fn maybe_materialize_static_edge(
    transactions: &[CandidateTransaction],
    edges: &mut Vec<TransactionEdge>,
    profile_edge: &acg_profile_graph::LoadedProfileEdge,
    predicate: &CompiledPredicate,
    left_index: TxIndex,
    right_index: TxIndex,
    mode: StaticMaterialization<'_>,
) -> Result<(), CandidateGraphError> {
    let left = &transactions[left_index.0 as usize];
    let right = &transactions[right_index.0 as usize];
    let result = predicate.evaluate(
        left.instance_id,
        &left.input_bindings,
        right.instance_id,
        &right.input_bindings,
    );
    if !mode.should_materialize(result) {
        return Ok(());
    }
    let metrics = mode.edge_metrics();
    push_edge(
        edges,
        left_index,
        right_index,
        EdgeProvenance::Static {
            profile_edge_index: profile_edge.index,
        },
        result,
        profile_edge.conflict_kinds,
        metrics,
    );
    Ok(())
}

fn materialize_runtime_fallback_edge(
    buckets: &[Vec<TxIndex>],
    edges: &mut Vec<TransactionEdge>,
    fallback: &acg_feedback::RuntimeDiscoveredEdge,
    estimate: EdgeEstimate,
    replay_cost: ReplayCostEstimate,
    serialization_cost: SerializationCostEstimate,
    cost_policy: CostAwareEdgePolicyConfig,
) {
    let source_bucket = &buckets[fallback.source.0 as usize];
    let target_bucket = &buckets[fallback.target.0 as usize];
    let metrics = edge_metrics(estimate, replay_cost, serialization_cost, cost_policy);
    if fallback.source == fallback.target {
        for (left_offset, &left_index) in source_bucket.iter().enumerate() {
            for &right_index in source_bucket.iter().skip(left_offset + 1) {
                push_edge(
                    edges,
                    left_index,
                    right_index,
                    EdgeProvenance::RuntimeDiscovered {
                        runtime_edge_id: fallback.id,
                    },
                    PredicateResult::Unknown,
                    fallback.conflict_kinds,
                    metrics,
                );
            }
        }
    } else {
        for &left_index in source_bucket {
            for &right_index in target_bucket {
                push_edge(
                    edges,
                    left_index,
                    right_index,
                    EdgeProvenance::RuntimeDiscovered {
                        runtime_edge_id: fallback.id,
                    },
                    PredicateResult::Unknown,
                    fallback.conflict_kinds,
                    metrics,
                );
            }
        }
    }
}

#[derive(Clone, Copy)]
struct EdgeMetrics {
    probability_q16: u16,
    confidence_q16: u16,
    concrete_conflict_observations: u32,
    concrete_independent_observations: u32,
    scheduling_risk_q16: u16,
    expected_replay_cost_nanos: u64,
    expected_invalidated_descendants_milli: u32,
    replay_cost_confidence_q16: u16,
    expected_serialization_cost_nanos: u64,
    serialization_cost_confidence_q16: u16,
}

fn edge_metrics(
    estimate: EdgeEstimate,
    replay_cost: ReplayCostEstimate,
    serialization_cost: SerializationCostEstimate,
    cost_policy: CostAwareEdgePolicyConfig,
) -> EdgeMetrics {
    let scheduling_risk = cost_adjusted_scheduling_risk(
        estimate.probability,
        replay_cost,
        serialization_cost,
        cost_policy,
    );
    EdgeMetrics {
        probability_q16: quantize_q16(estimate.probability),
        confidence_q16: quantize_q16(estimate.confidence),
        concrete_conflict_observations: u32::try_from(estimate.positive_observations)
            .unwrap_or(u32::MAX),
        concrete_independent_observations: u32::try_from(estimate.negative_observations)
            .unwrap_or(u32::MAX),
        scheduling_risk_q16: quantize_q16(scheduling_risk),
        expected_replay_cost_nanos: replay_cost
            .expected_replay_cost_nanos
            .round()
            .clamp(0.0, u64::MAX as f64) as u64,
        expected_invalidated_descendants_milli: (replay_cost.expected_invalidated_descendants
            * 1000.0)
            .round()
            .clamp(0.0, u32::MAX as f64) as u32,
        replay_cost_confidence_q16: quantize_q16(replay_cost.confidence.clamp(0.0, 1.0)),
        expected_serialization_cost_nanos: serialization_cost
            .expected_serialization_cost_nanos
            .round()
            .clamp(0.0, u64::MAX as f64) as u64,
        serialization_cost_confidence_q16: quantize_q16(
            serialization_cost.confidence.clamp(0.0, 1.0),
        ),
    }
}

fn effective_serialization_cost_nanos(
    serialization_cost: SerializationCostEstimate,
    fallback_serialization_cost_nanos: u64,
) -> f64 {
    let fallback = fallback_serialization_cost_nanos.max(1) as f64;
    if serialization_cost.confidence <= 0.0 {
        return fallback;
    }
    // A measured zero marginal delay is real evidence that this dependency is effectively free
    // under the observed schedule. Keep a one-nanosecond floor only to avoid division by zero; do
    // not confuse zero cost with missing evidence (which is represented by zero confidence).
    let learned = serialization_cost
        .expected_serialization_cost_nanos
        .max(1.0);
    fallback + serialization_cost.confidence.clamp(0.0, 1.0) * (learned - fallback)
}

fn cost_adjusted_scheduling_risk(
    conflict_probability: f64,
    replay_cost: ReplayCostEstimate,
    serialization_cost: SerializationCostEstimate,
    cost_policy: CostAwareEdgePolicyConfig,
) -> f64 {
    if replay_cost.confidence <= 0.0 || replay_cost.expected_replay_cost_nanos <= 0.0 {
        return conflict_probability;
    }
    let fanout_multiplier =
        1.0 + cost_policy.invalidation_fanout_weight * replay_cost.expected_invalidated_descendants;
    let expected_speculation_penalty =
        conflict_probability * replay_cost.expected_replay_cost_nanos * fanout_multiplier;
    let serialization_reference = effective_serialization_cost_nanos(
        serialization_cost,
        cost_policy.serialization_cost_reference_nanos,
    );
    let cost_risk = (expected_speculation_penalty / serialization_reference).clamp(0.0, 1.0);
    (conflict_probability + replay_cost.confidence * (cost_risk - conflict_probability))
        .clamp(0.0, 1.0)
}

fn push_edge(
    edges: &mut Vec<TransactionEdge>,
    left_index: TxIndex,
    right_index: TxIndex,
    provenance: EdgeProvenance,
    predicate_result: PredicateResult,
    conflict_kinds: ConflictKinds,
    metrics: EdgeMetrics,
) {
    let (source, target) = if left_index <= right_index {
        (left_index, right_index)
    } else {
        (right_index, left_index)
    };
    edges.push(TransactionEdge {
        source,
        target,
        provenance,
        predicate_result,
        conflict_kinds,
        probability_q16: metrics.probability_q16,
        confidence_q16: metrics.confidence_q16,
        concrete_conflict_observations: metrics.concrete_conflict_observations,
        concrete_independent_observations: metrics.concrete_independent_observations,
        scheduling_risk_q16: metrics.scheduling_risk_q16,
        expected_replay_cost_nanos: metrics.expected_replay_cost_nanos,
        expected_invalidated_descendants_milli: metrics.expected_invalidated_descendants_milli,
        replay_cost_confidence_q16: metrics.replay_cost_confidence_q16,
        expected_serialization_cost_nanos: metrics.expected_serialization_cost_nanos,
        serialization_cost_confidence_q16: metrics.serialization_cost_confidence_q16,
    });
}

fn finish_graph(
    transactions: Vec<CandidateTransaction>,
    mut edges: Vec<TransactionEdge>,
    compact_groups: Vec<CompactCandidateGroup>,
) -> Result<CandidateGraph, CandidateGraphError> {
    edges.sort_by_key(|edge| (edge.source, edge.target, edge.provenance));
    let mut adjacency = vec![Vec::<TransactionAdjacency>::new(); transactions.len()];
    for (edge_offset, edge) in edges.iter().enumerate() {
        let edge_index = u32::try_from(edge_offset)
            .map_err(|_| CandidateGraphError::TooManyEdges(edges.len()))?;
        adjacency[edge.source.0 as usize].push(TransactionAdjacency {
            neighbor: edge.target,
            edge_index,
        });
        adjacency[edge.target.0 as usize].push(TransactionAdjacency {
            neighbor: edge.source,
            edge_index,
        });
    }

    let mut adjacency_offsets = Vec::with_capacity(transactions.len() + 1);
    let mut adjacency_entries = Vec::new();
    adjacency_offsets.push(0);
    for entries in &mut adjacency {
        entries.sort_by_key(|entry| (entry.neighbor, entry.edge_index));
        adjacency_entries.extend_from_slice(entries);
        adjacency_offsets.push(
            u32::try_from(adjacency_entries.len())
                .map_err(|_| CandidateGraphError::TooManyAdjacencyEntries)?,
        );
    }

    let mut compact_memberships = vec![Vec::<u32>::new(); transactions.len()];
    for (group_index, group) in compact_groups.iter().enumerate() {
        let group_index = u32::try_from(group_index)
            .map_err(|_| CandidateGraphError::TooManyEdges(compact_groups.len()))?;
        for member in &group.members {
            compact_memberships[member.0 as usize].push(group_index);
        }
    }
    for memberships in &mut compact_memberships {
        memberships.sort_unstable();
    }

    Ok(CandidateGraph {
        transactions,
        edges,
        compact_groups,
        compact_memberships,
        adjacency_offsets,
        adjacency_entries,
    })
}

fn quantize_q16(value: f64) -> u16 {
    debug_assert!(value.is_finite() && (0.0..=1.0).contains(&value));
    (value * Q16_MAX).round() as u16
}

fn dequantize_q16(value: u16) -> f64 {
    f64::from(value) / Q16_MAX
}

#[derive(Debug, Error)]
pub enum CandidateGraphError {
    #[error("candidate block contains too many transactions for u32 indexes: {0}")]
    TooManyTransactions(usize),
    #[error("candidate graph contains too many edges for u32 indexes: {0}")]
    TooManyEdges(usize),
    #[error("candidate graph contains too many adjacency entries for u32 offsets")]
    TooManyAdjacencyEntries,
    #[error("transaction {tx_id:?} references unknown profile {profile_id:?}")]
    UnknownProfileId { tx_id: TxId, profile_id: ProfileId },
    #[error("transaction {tx_id:?} has invalid inclusion probability {value}")]
    InvalidInclusionProbability { tx_id: TxId, value: f32 },
    #[error("edge materialization threshold must be finite and within [0, 1], got {0}")]
    InvalidMaterializationThreshold(f64),
    #[error("Brick 5D serialization cost reference must be greater than zero")]
    ZeroSerializationCostReference,
    #[error("Brick 5D invalidation fan-out weight must be finite and non-negative, got {0}")]
    InvalidInvalidationFanoutWeight(f64),
    #[error(transparent)]
    Feedback(#[from] FeedbackError),
}
