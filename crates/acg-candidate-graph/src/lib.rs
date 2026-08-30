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
use acg_predicate::{CompiledPredicate, InputBindings, PredicateEquivalenceKey, PredicateResult};
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

/// One symbolic contract entrypoint participating in an atomic block transaction.
///
/// Profiles remain the unit of offline conflict modeling, but schedulers operate on the atomic
/// transaction that owns one or more of these components.
#[derive(Clone, Debug, PartialEq)]
pub struct CandidateComponent {
    pub profile_id: ProfileId,
    pub instance_id: InstanceId,
    pub input_bindings: InputBindings,
}

/// One atomic block transaction described by zero or more symbolic entrypoint profiles.
///
/// The original ACG architecture builds a transaction graph from profile relationships. This type
/// lets workload adapters preserve that boundary when one execution request contains multiple
/// contract calls: component profiles provide conflict evidence, while this parent remains the
/// scheduling/commit unit.
#[derive(Clone, Debug, PartialEq)]
pub struct AtomicCandidateTransaction {
    pub tx_id: TxId,
    pub predicted_position: u32,
    pub inclusion_probability: f32,
    pub estimated_execution_cost: u32,
    pub components: Vec<CandidateComponent>,
}

impl AtomicCandidateTransaction {
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

/// Result of instantiating an atomic transaction graph from profile-level evidence.
///
/// `logical_component_edges` counts concrete component relationships that would have been
/// materialized by a component-node graph. `graph` contains only atomic transaction nodes/edges,
/// while `candidate_provenances` preserves every profile/fallback relationship contributing to an
/// atomic edge so runtime feedback can continue refining the original symbolic model.
#[derive(Clone, Debug)]
pub struct AtomicCandidateGraphBuild {
    graph: CandidateGraph,
    logical_component_edges: usize,
    candidate_provenances: BTreeMap<(TxIndex, TxIndex), BTreeSet<EdgeProvenance>>,
}

impl AtomicCandidateGraphBuild {
    pub fn graph(&self) -> &CandidateGraph {
        &self.graph
    }

    pub fn into_graph(self) -> CandidateGraph {
        self.graph
    }

    pub fn logical_component_edges(&self) -> usize {
        self.logical_component_edges
    }

    pub fn candidate_provenances_between(
        &self,
        left: TxIndex,
        right: TxIndex,
    ) -> BTreeSet<EdgeProvenance> {
        let pair = canonical_tx_pair(left, right);
        let mut output = self
            .candidate_provenances
            .get(&pair)
            .cloned()
            .unwrap_or_default();
        for group in self.graph.compact_groups() {
            if group.members().binary_search(&left).is_ok()
                && group.members().binary_search(&right).is_ok()
            {
                output.insert(group.provenance());
            }
        }
        output
    }

    pub fn candidate_provenance_map(
        &self,
    ) -> &BTreeMap<(TxIndex, TxIndex), BTreeSet<EdgeProvenance>> {
        &self.candidate_provenances
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
    /// Phase 5D cost-adjusted scheduling risk. Raw conflict probability remains separately visible.
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
    /// Phase 5E learned marginal dependency-ready delay for this profile relationship.
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
        // Legacy/binary edges serialized before Phase 5D may have a zero default here. Preserve
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

    /// Effective Phase 5E serialization reference after confidence-weighted blending with the
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
pub struct ParallelCandidateGroup {
    source: TxIndex,
    target: TxIndex,
    evidences: Vec<TransactionEdge>,
}

impl ParallelCandidateGroup {
    pub fn source(&self) -> TxIndex {
        self.source
    }

    pub fn target(&self) -> TxIndex {
        self.target
    }

    /// Distinct profile/runtime relationships contributing scheduling evidence for this atomic
    /// transaction pair. These remain separate so classification, soft-risk composition and
    /// adaptive feedback retain the original ACG semantics.
    pub fn evidences(&self) -> &[TransactionEdge] {
        &self.evidences
    }
}

#[derive(Clone, Debug)]
pub struct CandidateGraph {
    transactions: Vec<CandidateTransaction>,
    edges: Vec<TransactionEdge>,
    parallel_groups: Vec<ParallelCandidateGroup>,
    parallel_pairs: BTreeSet<(TxIndex, TxIndex)>,
    compact_groups: Vec<CompactCandidateGroup>,
    compact_provenances: BTreeSet<EdgeProvenance>,
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

    /// Builds the transaction-count shell used by the direct serial admission path.
    ///
    /// A serial-bypassed block never evaluates candidate relationships or runtime feedback, so
    /// adapting every CosmWasm request into predicate bindings would be pure control-plane cost.
    /// These placeholder transactions exist only to preserve block cardinality for diagnostics.
    pub fn serial_bypass(transaction_count: usize) -> Result<Self, CandidateGraphError> {
        u32::try_from(transaction_count)
            .map_err(|_| CandidateGraphError::TooManyTransactions(transaction_count))?;
        let transactions = (0..transaction_count)
            .map(|index| {
                let index = u32::try_from(index).expect("transaction count validated");
                CandidateTransaction {
                    tx_id: TxId(u64::from(index)),
                    predicted_position: index,
                    inclusion_probability: 1.0,
                    profile_id: ProfileId(0),
                    instance_id: InstanceId(0),
                    input_bindings: InputBindings::empty(),
                    estimated_execution_cost: 0,
                }
            })
            .collect();
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

    pub fn parallel_groups(&self) -> &[ParallelCandidateGroup] {
        &self.parallel_groups
    }

    pub fn pair_is_parallel(&self, left: TxIndex, right: TxIndex) -> bool {
        self.parallel_pairs
            .contains(&canonical_tx_pair(left, right))
    }

    /// Number of physical pair relationships represented in the graph. A parallel evidence group
    /// counts once even though it retains several profile-level evidence records internally.
    pub fn physical_edge_count(&self) -> usize {
        self.edges.len()
    }

    pub fn logical_edge_count(&self) -> usize {
        let parallel_extra = self
            .parallel_groups
            .iter()
            .map(|group| group.evidences.len().saturating_sub(1))
            .sum::<usize>();
        self.edges
            .len()
            .saturating_add(parallel_extra)
            .saturating_add(
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
        self.compact_provenances.contains(&provenance)
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

    pub fn compact_provenance_between(
        &self,
        left: TxIndex,
        right: TxIndex,
    ) -> Option<EdgeProvenance> {
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

    pub fn candidate_provenance_between(
        &self,
        left: TxIndex,
        right: TxIndex,
    ) -> Option<EdgeProvenance> {
        self.edge_between(left, right)
            .map(|edge| edge.provenance)
            .or_else(|| self.compact_provenance_between(left, right))
    }

    /// Returns the compact relationship covering every supplied transaction, when one exists.
    ///
    /// This lets runtime feedback suppress an entire access-equivalence class before generating
    /// transaction pairs. Membership intersection is proportional to the number of compact
    /// relationships per transaction rather than the logical clique size.
    pub fn compact_provenance_covering(&self, members: &[TxIndex]) -> Option<EdgeProvenance> {
        let first = *members.first()?;
        let candidate_groups = self.compact_memberships.get(first.0 as usize)?;
        'groups: for group_index in candidate_groups {
            for member in members.iter().skip(1) {
                let memberships = self.compact_memberships.get(member.0 as usize)?;
                if memberships.binary_search(group_index).is_err() {
                    continue 'groups;
                }
            }
            return self
                .compact_groups
                .get(*group_index as usize)
                .map(|group| group.provenance);
        }
        None
    }

    pub fn contains_candidate_pair(&self, left: TxIndex, right: TxIndex) -> bool {
        self.candidate_provenance_between(left, right).is_some()
    }
}

/// Conversion from conflict probability + measured execution costs into scheduling risk.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CostAwareEdgePolicyConfig {
    /// Fallback pre-consensus serialization delay used until confident learned evidence exists.
    pub serialization_cost_reference_nanos: u64,
    /// Additional penalty per expected transitive invalidation descendant.
    pub invalidation_fanout_weight: f64,
    /// Relative value of one nanosecond of serialization delay in the combined execution wall.
    pub pre_consensus_serialization_weight: f64,
    /// Relative value of one nanosecond of replay/validation work in the combined execution wall.
    pub post_consensus_replay_weight: f64,
}

impl Default for CostAwareEdgePolicyConfig {
    fn default() -> Self {
        Self {
            serialization_cost_reference_nanos: 250_000,
            invalidation_fanout_weight: 1.0,
            pre_consensus_serialization_weight: 1.0,
            post_consensus_replay_weight: 1.0,
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
        if !self.pre_consensus_serialization_weight.is_finite()
            || self.pre_consensus_serialization_weight <= 0.0
        {
            return Err(CandidateGraphError::InvalidPhaseWeight(
                self.pre_consensus_serialization_weight,
            ));
        }
        if !self.post_consensus_replay_weight.is_finite()
            || self.post_consensus_replay_weight <= 0.0
        {
            return Err(CandidateGraphError::InvalidPhaseWeight(
                self.post_consensus_replay_weight,
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
    /// Cost-aware Phase 5D policy. With no replay-cost evidence, scheduling risk exactly matches
    /// the Phase 4 posterior probability.
    pub cost_policy: CostAwareEdgePolicyConfig,
    /// Compact provable equivalence cliques into a group representation at every maturity level.
    ///
    /// The legacy field name is retained for source compatibility. Hard groups use a canonical
    /// chain; mature Soft groups keep full logical clique semantics in the group-aware scheduler
    /// without materializing every transaction pair.
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

/// Immutable, profile-graph-specific candidate construction state.
///
/// Symbolic documents and their predicates are parsed/compiled before block execution. Keeping
/// this object across blocks ensures the online path only instantiates concrete transactions,
/// buckets them by already-resolved profiles/instances, and evaluates precompiled relationships.
#[derive(Clone, Copy, Debug)]
struct PreparedProfileRelationship {
    edge_index: ProfileEdgeIndex,
    source: ProfileId,
    target: ProfileId,
    conflict_kinds: ConflictKinds,
}

#[derive(Debug)]
pub struct PreparedCandidateGraphBuilder {
    compiled_predicates: Vec<CompiledPredicate>,
    relationships: Vec<PreparedProfileRelationship>,
    relationship_adjacency: Vec<Vec<u32>>,
    profile_count: usize,
}

impl PreparedCandidateGraphBuilder {
    pub fn new(profile_graph: &ProfileGraph) -> Self {
        let compiled_predicates = profile_graph
            .edges()
            .iter()
            .map(|edge| CompiledPredicate::compile(&edge.predicate))
            .collect();
        let relationships = profile_graph
            .edges()
            .iter()
            .map(|edge| PreparedProfileRelationship {
                edge_index: edge.index,
                source: edge.source,
                target: edge.target,
                conflict_kinds: edge.conflict_kinds,
            })
            .collect::<Vec<_>>();
        let mut relationship_adjacency = vec![Vec::<u32>::new(); profile_graph.profiles().len()];
        for (offset, relationship) in relationships.iter().enumerate() {
            let offset =
                u32::try_from(offset).expect("profile-edge indexes are represented by u32");
            relationship_adjacency[relationship.source.0 as usize].push(offset);
            if relationship.target != relationship.source {
                relationship_adjacency[relationship.target.0 as usize].push(offset);
            }
        }
        Self {
            compiled_predicates,
            relationships,
            relationship_adjacency,
            profile_count: profile_graph.profiles().len(),
        }
    }

    /// Builds the pre-Phase-4 binary graph using predicates compiled when this prepared builder
    /// was created. `profile_graph` must be the immutable graph used to prepare this builder.
    pub fn build(
        &self,
        profile_graph: &ProfileGraph,
        transactions: Vec<CandidateTransaction>,
    ) -> Result<CandidateGraph, CandidateGraphError> {
        self.debug_assert_compatible(profile_graph);
        let (buckets, instance_buckets) = self.prepare_transactions(&transactions)?;
        let mut edges = Vec::<TransactionEdge>::new();
        let mut compact_groups = Vec::<CompactCandidateGroup>::new();
        let materialization_context = StaticMaterializationContext {
            transactions: &transactions,
            buckets: &buckets,
            instance_buckets: &instance_buckets,
            compiled_predicates: &self.compiled_predicates,
        };
        let mut visited_profile_edges = vec![false; self.compiled_predicates.len()];

        for (profile_offset, bucket) in buckets.iter().enumerate() {
            if bucket.is_empty() {
                continue;
            }
            let profile_id = ProfileId(
                u32::try_from(profile_offset).expect("profile count is represented by ProfileId"),
            );
            for adjacency in profile_graph.neighbors(profile_id) {
                let edge_offset = adjacency.edge_index.0 as usize;
                if buckets[adjacency.neighbor.0 as usize].is_empty()
                    || visited_profile_edges[edge_offset]
                {
                    continue;
                }
                visited_profile_edges[edge_offset] = true;
                let profile_edge = &profile_graph.edges()[edge_offset];
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

    /// Builds an adaptive weighted candidate graph from the immutable profile topology plus
    /// persistent runtime observations. Only concrete block transactions are instantiated here;
    /// symbolic predicate compilation stays outside the online path.
    pub fn build_weighted(
        &self,
        profile_graph: &ProfileGraph,
        transactions: Vec<CandidateTransaction>,
        feedback_store: &AdaptiveFeedbackStore,
        feedback_config: &AdaptiveFeedbackConfig,
        config: WeightedCandidateGraphConfig,
    ) -> Result<CandidateGraph, CandidateGraphError> {
        self.debug_assert_compatible(profile_graph);
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
        let mut visited_profile_edges = vec![false; self.compiled_predicates.len()];
        let mut visited_runtime_edges = BTreeSet::<RuntimeEdgeId>::new();

        for (profile_offset, bucket) in buckets.iter().enumerate() {
            if bucket.is_empty() {
                continue;
            }
            let profile_id = ProfileId(
                u32::try_from(profile_offset).expect("profile count is represented by ProfileId"),
            );

            for adjacency in profile_graph.neighbors(profile_id) {
                let edge_offset = adjacency.edge_index.0 as usize;
                if buckets[adjacency.neighbor.0 as usize].is_empty()
                    || visited_profile_edges[edge_offset]
                {
                    continue;
                }
                visited_profile_edges[edge_offset] = true;
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
                let profile_edge = &profile_graph.edges()[edge_offset];
                let adaptive_materialization = AdaptiveMaterialization {
                    estimate,
                    replay_cost,
                    serialization_cost,
                    edge_materialization_threshold: config.edge_materialization_threshold,
                    cost_policy: config.cost_policy,
                    compact_immature_equivalence_edges: config.compact_immature_equivalence_edges,
                };
                materialize_static_profile_edge(
                    &materialization_context,
                    &mut edges,
                    &mut compact_groups,
                    profile_edge,
                    StaticMaterialization::Adaptive(&adaptive_materialization),
                )?;
            }

            // Persisted topology misses are part of the online model and are intentionally
            // traversed beside the immutable symbolic adjacency. Preparing predicates must not
            // erase the feedback path that lets concrete execution refine candidate topology.
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

    /// Build the scheduling graph directly at the atomic transaction level while retaining
    /// profile-level conflict evidence.
    ///
    /// This is the compound-request counterpart to [`Self::build_weighted`]. Symbolic profiles
    /// remain the offline conflict model, but multiple entrypoint components owned by one runtime
    /// transaction are aggregated before `RiskBoundedScheduler` sees the graph. This avoids
    /// constructing and reducing a large component-node graph only to project it back onto the
    /// atomic commit units afterward.
    pub fn build_weighted_atomic(
        &self,
        profile_graph: &ProfileGraph,
        transactions: Vec<AtomicCandidateTransaction>,
        feedback_store: &AdaptiveFeedbackStore,
        feedback_config: &AdaptiveFeedbackConfig,
        config: WeightedCandidateGraphConfig,
    ) -> Result<AtomicCandidateGraphBuild, CandidateGraphError> {
        self.debug_assert_compatible(profile_graph);
        config.validate()?;
        feedback_config.validate()?;
        let prepared = prepare_atomic_transactions(&transactions, self.profile_count)?;
        let scheduler_transactions = atomic_scheduler_transactions(&transactions);
        let mut evidence = AtomicEvidenceMap::new();
        let mut compact_groups = Vec::<CompactCandidateGroup>::new();
        let mut logical_component_edges = 0_usize;

        // Static profile relationships are an immutable execution plan prepared when the symbolic
        // graph is loaded. Online work only checks which endpoint buckets are active, projects the
        // current feedback estimate, and evaluates the already-compiled predicate.
        let mut visited_relationships = vec![false; self.relationships.len()];
        for (profile_offset, bucket) in prepared.buckets.iter().enumerate() {
            if bucket.is_empty() {
                continue;
            }
            for &relationship_offset in &self.relationship_adjacency[profile_offset] {
                let relationship_offset = relationship_offset as usize;
                if visited_relationships[relationship_offset] {
                    continue;
                }
                visited_relationships[relationship_offset] = true;
                let relationship = self.relationships[relationship_offset];
                if prepared.buckets[relationship.source.0 as usize].is_empty()
                    || prepared.buckets[relationship.target.0 as usize].is_empty()
                {
                    continue;
                }
                let estimate = feedback_store.estimate_static_edge(
                    relationship.edge_index,
                    config.epoch,
                    feedback_config,
                )?;
                let replay_cost = feedback_store.estimate_static_replay_cost(
                    relationship.edge_index,
                    config.epoch,
                    feedback_config,
                )?;
                let serialization_cost = feedback_store.estimate_static_serialization_cost(
                    relationship.edge_index,
                    config.epoch,
                    feedback_config,
                )?;
                let adaptive = AdaptiveMaterialization {
                    estimate,
                    replay_cost,
                    serialization_cost,
                    edge_materialization_threshold: config.edge_materialization_threshold,
                    cost_policy: config.cost_policy,
                    compact_immature_equivalence_edges: config.compact_immature_equivalence_edges,
                };
                materialize_atomic_static_relationship(
                    &transactions,
                    &prepared,
                    &self.compiled_predicates,
                    &mut evidence,
                    &mut compact_groups,
                    &mut logical_component_edges,
                    relationship,
                    StaticMaterialization::Adaptive(&adaptive),
                )?;
            }
        }

        // Runtime-discovered topology is persistent adaptive state. Because fallback relationships
        // have no input predicate, collapse component multiplicity directly to parent pairs while
        // preserving the logical component-pair count for diagnostics.
        for fallback in feedback_store.fallback_edges() {
            if prepared.buckets[fallback.source.0 as usize].is_empty()
                || prepared.buckets[fallback.target.0 as usize].is_empty()
            {
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
            materialize_atomic_runtime_fallback(
                &prepared.buckets,
                &mut evidence,
                &mut logical_component_edges,
                fallback,
                edge_metrics(
                    estimate,
                    replay_cost,
                    serialization_cost,
                    config.cost_policy,
                ),
            );
        }

        // One physical adjacency relationship per atomic transaction pair. Distinct profile-level
        // relationships remain inside a parallel evidence group so the scheduler can reproduce the
        // exact per-profile classification and `1 - Π(1-p)` soft-risk semantics without storing
        // thousands of duplicate pair entries in the physical graph.
        let mut edges = Vec::with_capacity(evidence.len());
        let mut parallel_groups = Vec::new();
        let mut candidate_provenances = BTreeMap::new();
        for (pair, per_provenance) in evidence {
            candidate_provenances.insert(pair, per_provenance.keys().copied().collect());
            let mut pair_evidence = per_provenance
                .into_iter()
                .map(|(provenance, item)| atomic_evidence_edge(pair, provenance, item))
                .collect::<Vec<_>>();
            pair_evidence.sort_by_key(|edge| edge.provenance);
            if pair_evidence.len() == 1 {
                edges.push(pair_evidence.pop().expect("one atomic evidence edge"));
            } else {
                // Keep one representative in the ordinary adjacency so existing neighbor-based
                // diagnostics remain pair-oriented. Scheduling skips this representative and
                // consumes the complete evidence group below.
                edges.push(pair_evidence[0]);
                parallel_groups.push(ParallelCandidateGroup {
                    source: pair.0,
                    target: pair.1,
                    evidences: pair_evidence,
                });
            }
        }
        let graph = finish_graph_with_parallel(
            scheduler_transactions,
            edges,
            parallel_groups,
            compact_groups,
        )?;
        Ok(AtomicCandidateGraphBuild {
            graph,
            logical_component_edges,
            candidate_provenances,
        })
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

    fn debug_assert_compatible(&self, profile_graph: &ProfileGraph) {
        debug_assert_eq!(self.profile_count, profile_graph.profiles().len());
        debug_assert_eq!(self.compiled_predicates.len(), profile_graph.edges().len());
        debug_assert_eq!(self.relationships.len(), profile_graph.edges().len());
        debug_assert_eq!(
            self.relationship_adjacency.len(),
            profile_graph.profiles().len()
        );
    }
}

/// Backward-compatible convenience wrapper for callers that build against one borrowed graph.
/// Long-lived runtimes should retain [`PreparedCandidateGraphBuilder`] across blocks instead of
/// recompiling immutable symbolic predicates for every candidate block.
#[derive(Debug)]
pub struct CandidateGraphBuilder<'graph> {
    profile_graph: &'graph ProfileGraph,
    prepared: PreparedCandidateGraphBuilder,
}

impl<'graph> CandidateGraphBuilder<'graph> {
    pub fn new(profile_graph: &'graph ProfileGraph) -> Self {
        Self {
            profile_graph,
            prepared: PreparedCandidateGraphBuilder::new(profile_graph),
        }
    }

    pub fn build(
        &self,
        transactions: Vec<CandidateTransaction>,
    ) -> Result<CandidateGraph, CandidateGraphError> {
        self.prepared.build(self.profile_graph, transactions)
    }

    pub fn build_weighted(
        &self,
        transactions: Vec<CandidateTransaction>,
        feedback_store: &AdaptiveFeedbackStore,
        feedback_config: &AdaptiveFeedbackConfig,
        config: WeightedCandidateGraphConfig,
    ) -> Result<CandidateGraph, CandidateGraphError> {
        self.prepared.build_weighted(
            self.profile_graph,
            transactions,
            feedback_store,
            feedback_config,
            config,
        )
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
            Self::Adaptive(adaptive) => adaptive.compact_immature_equivalence_edges,
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

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct AtomicComponentRef {
    parent: TxIndex,
    component: u32,
}

struct AtomicPreparedBuckets {
    buckets: Vec<Vec<AtomicComponentRef>>,
    instance_buckets: Vec<BTreeMap<InstanceId, Vec<AtomicComponentRef>>>,
}

#[derive(Clone, Copy)]
struct AtomicEvidenceAccumulator {
    predicate_result: PredicateResult,
    conflict_kinds: ConflictKinds,
    metrics: EdgeMetrics,
}

type AtomicEvidenceMap =
    BTreeMap<(TxIndex, TxIndex), BTreeMap<EdgeProvenance, AtomicEvidenceAccumulator>>;

fn canonical_tx_pair(left: TxIndex, right: TxIndex) -> (TxIndex, TxIndex) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn prepare_atomic_transactions(
    transactions: &[AtomicCandidateTransaction],
    profile_count: usize,
) -> Result<AtomicPreparedBuckets, CandidateGraphError> {
    u32::try_from(transactions.len())
        .map_err(|_| CandidateGraphError::TooManyTransactions(transactions.len()))?;
    let mut buckets = vec![Vec::<AtomicComponentRef>::new(); profile_count];
    let mut instance_buckets =
        vec![BTreeMap::<InstanceId, Vec<AtomicComponentRef>>::new(); profile_count];
    for (parent_offset, transaction) in transactions.iter().enumerate() {
        transaction.validate()?;
        let parent = TxIndex(u32::try_from(parent_offset).expect("transaction count validated"));
        for (component_offset, component) in transaction.components.iter().enumerate() {
            let profile_offset = component.profile_id.0 as usize;
            if profile_offset >= profile_count {
                return Err(CandidateGraphError::UnknownProfileId {
                    tx_id: transaction.tx_id,
                    profile_id: component.profile_id,
                });
            }
            let component_ref = AtomicComponentRef {
                parent,
                component: u32::try_from(component_offset).map_err(|_| {
                    CandidateGraphError::TooManyComponents {
                        tx_id: transaction.tx_id,
                        components: transaction.components.len(),
                    }
                })?,
            };
            buckets[profile_offset].push(component_ref);
            instance_buckets[profile_offset]
                .entry(component.instance_id)
                .or_default()
                .push(component_ref);
        }
    }
    Ok(AtomicPreparedBuckets {
        buckets,
        instance_buckets,
    })
}

fn atomic_scheduler_transactions(
    transactions: &[AtomicCandidateTransaction],
) -> Vec<CandidateTransaction> {
    transactions
        .iter()
        .map(|transaction| {
            let representative = transaction.components.first();
            CandidateTransaction {
                tx_id: transaction.tx_id,
                predicted_position: transaction.predicted_position,
                inclusion_probability: transaction.inclusion_probability,
                // Symbolic profile/instance/bindings are consumed while building the atomic graph.
                // The scheduler only reads ordering/cost fields from these parent nodes afterward;
                // keep a deterministic representative for diagnostics/source compatibility.
                profile_id: representative.map_or(ProfileId(0), |component| component.profile_id),
                instance_id: representative
                    .map_or(InstanceId(0), |component| component.instance_id),
                input_bindings: representative.map_or_else(InputBindings::empty, |component| {
                    component.input_bindings.clone()
                }),
                estimated_execution_cost: transaction.estimated_execution_cost,
            }
        })
        .collect()
}

fn atomic_component(
    transactions: &[AtomicCandidateTransaction],
    component_ref: AtomicComponentRef,
) -> &CandidateComponent {
    &transactions[component_ref.parent.0 as usize].components[component_ref.component as usize]
}

#[allow(clippy::too_many_arguments)]
fn materialize_atomic_static_relationship(
    transactions: &[AtomicCandidateTransaction],
    prepared: &AtomicPreparedBuckets,
    compiled_predicates: &[CompiledPredicate],
    evidence: &mut AtomicEvidenceMap,
    compact_groups: &mut Vec<CompactCandidateGroup>,
    logical_component_edges: &mut usize,
    relationship: PreparedProfileRelationship,
    mode: StaticMaterialization<'_>,
) -> Result<(), CandidateGraphError> {
    let source_bucket = &prepared.buckets[relationship.source.0 as usize];
    let target_bucket = &prepared.buckets[relationship.target.0 as usize];
    let predicate = &compiled_predicates[relationship.edge_index.0 as usize];
    let provenance = EdgeProvenance::Static {
        profile_edge_index: relationship.edge_index,
    };
    let may_use_instance_fast_path =
        predicate.requires_same_instance() && !mode.has_candidate_miss_history();

    if relationship.source == relationship.target
        && may_use_instance_fast_path
        && mode.may_compact_equivalence_clique()
        && materialize_atomic_equivalence_groups(
            transactions,
            evidence,
            compact_groups,
            logical_component_edges,
            predicate,
            source_bucket,
            provenance,
            relationship.conflict_kinds,
            mode,
        )?
    {
        return Ok(());
    }

    if may_use_instance_fast_path {
        let source_instances = &prepared.instance_buckets[relationship.source.0 as usize];
        let target_instances = &prepared.instance_buckets[relationship.target.0 as usize];
        if relationship.source == relationship.target {
            for bucket in source_instances.values() {
                materialize_atomic_same_bucket_pairs(
                    transactions,
                    evidence,
                    logical_component_edges,
                    predicate,
                    bucket,
                    provenance,
                    relationship.conflict_kinds,
                    mode,
                );
            }
        } else {
            for (instance, left_bucket) in source_instances {
                let Some(right_bucket) = target_instances.get(instance) else {
                    continue;
                };
                materialize_atomic_cross_bucket_pairs(
                    transactions,
                    evidence,
                    logical_component_edges,
                    predicate,
                    left_bucket,
                    right_bucket,
                    provenance,
                    relationship.conflict_kinds,
                    mode,
                );
            }
        }
    } else if relationship.source == relationship.target {
        materialize_atomic_same_bucket_pairs(
            transactions,
            evidence,
            logical_component_edges,
            predicate,
            source_bucket,
            provenance,
            relationship.conflict_kinds,
            mode,
        );
    } else {
        materialize_atomic_cross_bucket_pairs(
            transactions,
            evidence,
            logical_component_edges,
            predicate,
            source_bucket,
            target_bucket,
            provenance,
            relationship.conflict_kinds,
            mode,
        );
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn materialize_atomic_equivalence_groups(
    transactions: &[AtomicCandidateTransaction],
    evidence: &mut AtomicEvidenceMap,
    compact_groups: &mut Vec<CompactCandidateGroup>,
    logical_component_edges: &mut usize,
    predicate: &CompiledPredicate,
    bucket: &[AtomicComponentRef],
    provenance: EdgeProvenance,
    conflict_kinds: ConflictKinds,
    mode: StaticMaterialization<'_>,
) -> Result<bool, CandidateGraphError> {
    let groups = if predicate.is_unconditional_same_instance_whole_resource() {
        let mut groups = BTreeMap::<InstanceId, Vec<AtomicComponentRef>>::new();
        for &component_ref in bucket {
            groups
                .entry(atomic_component(transactions, component_ref).instance_id)
                .or_default()
                .push(component_ref);
        }
        groups.into_values().collect::<Vec<_>>()
    } else {
        let mut groups = BTreeMap::<PredicateEquivalenceKey, Vec<AtomicComponentRef>>::new();
        for &component_ref in bucket {
            let component = atomic_component(transactions, component_ref);
            let Some(key) =
                predicate.equivalence_key(component.instance_id, &component.input_bindings)
            else {
                return Ok(false);
            };
            groups.entry(key).or_default().push(component_ref);
        }
        groups.into_values().collect::<Vec<_>>()
    };

    let metrics = mode.edge_metrics();
    for group in groups {
        let mut counts = BTreeMap::<TxIndex, usize>::new();
        for component_ref in &group {
            *counts.entry(component_ref.parent).or_default() += 1;
        }
        if counts.len() < 2 {
            continue;
        }
        let total_pairs = group.len().saturating_mul(group.len().saturating_sub(1)) / 2;
        let internal_pairs = counts
            .values()
            .map(|count| count.saturating_mul(count.saturating_sub(1)) / 2)
            .sum::<usize>();
        *logical_component_edges =
            (*logical_component_edges).saturating_add(total_pairs.saturating_sub(internal_pairs));
        let members = counts.keys().copied().collect::<Vec<_>>();
        let logical_parent_edges = members
            .len()
            .saturating_mul(members.len().saturating_sub(1))
            / 2;
        for pair in members.windows(2) {
            record_atomic_evidence(
                evidence,
                pair[0],
                pair[1],
                provenance,
                PredicateResult::True,
                conflict_kinds,
                metrics,
            );
        }
        let edge_template = TransactionEdge {
            source: members[0],
            target: members[1],
            provenance,
            predicate_result: PredicateResult::True,
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
        };
        compact_groups.push(CompactCandidateGroup {
            provenance,
            members,
            logical_edges: logical_parent_edges,
            edge_template,
        });
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
fn materialize_atomic_same_bucket_pairs(
    transactions: &[AtomicCandidateTransaction],
    evidence: &mut AtomicEvidenceMap,
    logical_component_edges: &mut usize,
    predicate: &CompiledPredicate,
    bucket: &[AtomicComponentRef],
    provenance: EdgeProvenance,
    conflict_kinds: ConflictKinds,
    mode: StaticMaterialization<'_>,
) {
    for (left_offset, &left) in bucket.iter().enumerate() {
        for &right in bucket.iter().skip(left_offset + 1) {
            maybe_materialize_atomic_static_pair(
                transactions,
                evidence,
                logical_component_edges,
                predicate,
                left,
                right,
                provenance,
                conflict_kinds,
                mode,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn materialize_atomic_cross_bucket_pairs(
    transactions: &[AtomicCandidateTransaction],
    evidence: &mut AtomicEvidenceMap,
    logical_component_edges: &mut usize,
    predicate: &CompiledPredicate,
    left_bucket: &[AtomicComponentRef],
    right_bucket: &[AtomicComponentRef],
    provenance: EdgeProvenance,
    conflict_kinds: ConflictKinds,
    mode: StaticMaterialization<'_>,
) {
    for &left in left_bucket {
        for &right in right_bucket {
            maybe_materialize_atomic_static_pair(
                transactions,
                evidence,
                logical_component_edges,
                predicate,
                left,
                right,
                provenance,
                conflict_kinds,
                mode,
            );
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn maybe_materialize_atomic_static_pair(
    transactions: &[AtomicCandidateTransaction],
    evidence: &mut AtomicEvidenceMap,
    logical_component_edges: &mut usize,
    predicate: &CompiledPredicate,
    left: AtomicComponentRef,
    right: AtomicComponentRef,
    provenance: EdgeProvenance,
    conflict_kinds: ConflictKinds,
    mode: StaticMaterialization<'_>,
) {
    if left.parent == right.parent {
        return;
    }
    let left_component = atomic_component(transactions, left);
    let right_component = atomic_component(transactions, right);
    let result = predicate.evaluate(
        left_component.instance_id,
        &left_component.input_bindings,
        right_component.instance_id,
        &right_component.input_bindings,
    );
    if !mode.should_materialize(result) {
        return;
    }
    *logical_component_edges = (*logical_component_edges).saturating_add(1);
    record_atomic_evidence(
        evidence,
        left.parent,
        right.parent,
        provenance,
        result,
        conflict_kinds,
        mode.edge_metrics(),
    );
}

fn materialize_atomic_runtime_fallback(
    buckets: &[Vec<AtomicComponentRef>],
    evidence: &mut AtomicEvidenceMap,
    logical_component_edges: &mut usize,
    fallback: &acg_feedback::RuntimeDiscoveredEdge,
    metrics: EdgeMetrics,
) {
    let source_counts = atomic_parent_counts(&buckets[fallback.source.0 as usize]);
    let target_counts = atomic_parent_counts(&buckets[fallback.target.0 as usize]);
    let provenance = EdgeProvenance::RuntimeDiscovered {
        runtime_edge_id: fallback.id,
    };
    if fallback.source == fallback.target {
        let parents = source_counts.into_iter().collect::<Vec<_>>();
        for (offset, (left, left_count)) in parents.iter().copied().enumerate() {
            for (right, right_count) in parents.iter().copied().skip(offset + 1) {
                *logical_component_edges = (*logical_component_edges)
                    .saturating_add(left_count.saturating_mul(right_count));
                record_atomic_evidence(
                    evidence,
                    left,
                    right,
                    provenance,
                    PredicateResult::Unknown,
                    fallback.conflict_kinds,
                    metrics,
                );
            }
        }
    } else {
        for (left, left_count) in &source_counts {
            for (right, right_count) in &target_counts {
                if left == right {
                    continue;
                }
                *logical_component_edges = (*logical_component_edges)
                    .saturating_add(left_count.saturating_mul(*right_count));
                record_atomic_evidence(
                    evidence,
                    *left,
                    *right,
                    provenance,
                    PredicateResult::Unknown,
                    fallback.conflict_kinds,
                    metrics,
                );
            }
        }
    }
}

fn atomic_parent_counts(bucket: &[AtomicComponentRef]) -> BTreeMap<TxIndex, usize> {
    let mut counts = BTreeMap::new();
    for component in bucket {
        *counts.entry(component.parent).or_default() += 1;
    }
    counts
}

#[allow(clippy::too_many_arguments)]
fn record_atomic_evidence(
    evidence: &mut AtomicEvidenceMap,
    left: TxIndex,
    right: TxIndex,
    provenance: EdgeProvenance,
    predicate_result: PredicateResult,
    conflict_kinds: ConflictKinds,
    metrics: EdgeMetrics,
) {
    let pair = canonical_tx_pair(left, right);
    evidence
        .entry(pair)
        .or_default()
        .entry(provenance)
        .and_modify(|existing| {
            existing.predicate_result = existing.predicate_result.or(predicate_result);
            existing.conflict_kinds |= conflict_kinds;
        })
        .or_insert(AtomicEvidenceAccumulator {
            predicate_result,
            conflict_kinds,
            metrics,
        });
}

fn atomic_evidence_edge(
    pair: (TxIndex, TxIndex),
    provenance: EdgeProvenance,
    item: AtomicEvidenceAccumulator,
) -> TransactionEdge {
    TransactionEdge {
        source: pair.0,
        target: pair.1,
        provenance,
        predicate_result: item.predicate_result,
        conflict_kinds: item.conflict_kinds,
        probability_q16: item.metrics.probability_q16,
        confidence_q16: item.metrics.confidence_q16,
        concrete_conflict_observations: item.metrics.concrete_conflict_observations,
        concrete_independent_observations: item.metrics.concrete_independent_observations,
        scheduling_risk_q16: item.metrics.scheduling_risk_q16,
        expected_replay_cost_nanos: item.metrics.expected_replay_cost_nanos,
        expected_invalidated_descendants_milli: item.metrics.expected_invalidated_descendants_milli,
        replay_cost_confidence_q16: item.metrics.replay_cost_confidence_q16,
        expected_serialization_cost_nanos: item.metrics.expected_serialization_cost_nanos,
        serialization_cost_confidence_q16: item.metrics.serialization_cost_confidence_q16,
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
    let groups = if predicate.is_unconditional_same_instance_whole_resource() {
        // Whole-resource self-profile conflicts are also a true equivalence relation: within one
        // contract instance every member conflicts with every other member. Preserve the complete
        // logical clique while materializing only the compact group chain.
        let mut groups = BTreeMap::<InstanceId, Vec<TxIndex>>::new();
        for &tx_index in bucket {
            let transaction = &transactions[tx_index.0 as usize];
            groups
                .entry(transaction.instance_id)
                .or_default()
                .push(tx_index);
        }
        groups.into_values().collect::<Vec<_>>()
    } else {
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
        groups.into_values().collect::<Vec<_>>()
    };
    let metrics = mode.edge_metrics();
    let provenance = EdgeProvenance::Static {
        profile_edge_index: profile_edge.index,
    };
    for members in groups.into_iter().filter(|members| members.len() >= 2) {
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
    let expected_replay_work = conflict_probability
        * replay_cost.expected_replay_cost_nanos
        * fanout_multiplier
        * cost_policy.post_consensus_replay_weight;
    let expected_serialization_work = effective_serialization_cost_nanos(
        serialization_cost,
        cost_policy.serialization_cost_reference_nanos,
    ) * cost_policy.pre_consensus_serialization_weight;

    // Throughput objective: minimize expected *combined* pipeline execution work. Enforcing the
    // edge pays serialization work; relaxing it pays expected replay work. Keep the calibrated
    // conflict probability unchanged at the break-even point and move it smoothly toward 0 or 1
    // as one action becomes cheaper than the other. This avoids the old 0.5-at-break-even mapping,
    // which could move a well-calibrated probability across scheduler thresholds for no net
    // execution-time benefit.
    let cost_risk = if expected_serialization_work <= 0.0 {
        1.0
    } else if expected_replay_work <= expected_serialization_work {
        conflict_probability * (expected_replay_work / expected_serialization_work)
    } else {
        let ratio = expected_serialization_work / expected_replay_work;
        1.0 - (1.0 - conflict_probability) * ratio
    };
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
    edges: Vec<TransactionEdge>,
    compact_groups: Vec<CompactCandidateGroup>,
) -> Result<CandidateGraph, CandidateGraphError> {
    finish_graph_with_parallel(transactions, edges, Vec::new(), compact_groups)
}

fn finish_graph_with_parallel(
    transactions: Vec<CandidateTransaction>,
    mut edges: Vec<TransactionEdge>,
    mut parallel_groups: Vec<ParallelCandidateGroup>,
    compact_groups: Vec<CompactCandidateGroup>,
) -> Result<CandidateGraph, CandidateGraphError> {
    edges.sort_by_key(|edge| (edge.source, edge.target, edge.provenance));
    parallel_groups.sort_by_key(|group| (group.source, group.target));
    for group in &mut parallel_groups {
        group
            .evidences
            .sort_by_key(|edge| (edge.source, edge.target, edge.provenance));
    }
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

    let compact_provenances = compact_groups
        .iter()
        .map(|group| group.provenance)
        .collect::<BTreeSet<_>>();
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

    let parallel_pairs = parallel_groups
        .iter()
        .map(|group| canonical_tx_pair(group.source, group.target))
        .collect::<BTreeSet<_>>();

    Ok(CandidateGraph {
        transactions,
        edges,
        parallel_groups,
        parallel_pairs,
        compact_groups,
        compact_provenances,
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
    #[error(
        "transaction {tx_id:?} contains too many symbolic components for u32 indexes: {components}"
    )]
    TooManyComponents { tx_id: TxId, components: usize },
    #[error("candidate graph contains too many adjacency entries for u32 offsets")]
    TooManyAdjacencyEntries,
    #[error("transaction {tx_id:?} references unknown profile {profile_id:?}")]
    UnknownProfileId { tx_id: TxId, profile_id: ProfileId },
    #[error("transaction {tx_id:?} has invalid inclusion probability {value}")]
    InvalidInclusionProbability { tx_id: TxId, value: f32 },
    #[error("edge materialization threshold must be finite and within [0, 1], got {0}")]
    InvalidMaterializationThreshold(f64),
    #[error("Phase 5D serialization cost reference must be greater than zero")]
    ZeroSerializationCostReference,
    #[error("Phase 5D invalidation fan-out weight must be finite and non-negative, got {0}")]
    InvalidInvalidationFanoutWeight(f64),
    #[error("pre/post-consensus phase weights must be finite and greater than zero, got {0}")]
    InvalidPhaseWeight(f64),
    #[error(transparent)]
    Feedback(#[from] FeedbackError),
}
