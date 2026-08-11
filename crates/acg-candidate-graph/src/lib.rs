//! Runtime-independent construction of per-block transaction conflict graphs.

pub mod scheduler;

pub use scheduler::{
    EdgeClass, RiskBoundedSchedule, RiskBoundedScheduler, RiskBoundedSchedulerConfig,
    ScheduledDependency, ScheduledWave, SchedulingError,
};

use std::collections::{BTreeMap, BTreeSet};

use acg_core::{ConflictKinds, InstanceId, ProfileEdgeIndex, ProfileId, TxId, TxIndex};
use acg_feedback::{
    AdaptiveFeedbackConfig, AdaptiveFeedbackStore, EdgeEstimate, FeedbackError, RuntimeEdgeId,
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
pub struct CandidateGraph {
    transactions: Vec<CandidateTransaction>,
    edges: Vec<TransactionEdge>,
    adjacency_offsets: Vec<u32>,
    adjacency_entries: Vec<TransactionAdjacency>,
}

impl CandidateGraph {
    pub fn transactions(&self) -> &[CandidateTransaction] {
        &self.transactions
    }

    pub fn edges(&self) -> &[TransactionEdge] {
        &self.edges
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
        self.edges.iter().find(|edge| {
            (edge.source == left && edge.target == right)
                || (edge.source == right && edge.target == left)
        })
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
                    &transactions,
                    &buckets,
                    &instance_buckets,
                    &self.compiled_predicates,
                    &mut edges,
                    profile_edge,
                    StaticMaterialization::Binary,
                )?;
            }
        }

        finish_graph(transactions, edges)
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
                let profile_edge = &self.profile_graph.edges()[adjacency.edge_index.0 as usize];
                materialize_static_profile_edge(
                    &transactions,
                    &buckets,
                    &instance_buckets,
                    &self.compiled_predicates,
                    &mut edges,
                    profile_edge,
                    StaticMaterialization::Adaptive {
                        estimate,
                        edge_materialization_threshold: config.edge_materialization_threshold,
                    },
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
                materialize_runtime_fallback_edge(&buckets, &mut edges, fallback, estimate);
            }
        }

        finish_graph(transactions, edges)
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
enum StaticMaterialization {
    Binary,
    Adaptive {
        estimate: EdgeEstimate,
        edge_materialization_threshold: f64,
    },
}

impl StaticMaterialization {
    fn has_candidate_miss_history(self) -> bool {
        match self {
            Self::Binary => false,
            Self::Adaptive { estimate, .. } => estimate.has_candidate_miss_history(),
        }
    }

    fn should_materialize(self, predicate_result: PredicateResult) -> bool {
        match self {
            Self::Binary => predicate_result != PredicateResult::False,
            Self::Adaptive {
                estimate,
                edge_materialization_threshold,
            } => match predicate_result {
                // A concrete predicate match is known symbolic topology. Keep it present so
                // concrete execution can soften the relationship rather than erase it.
                PredicateResult::True => true,
                // A False predicate is normally pruned, unless runtime execution has already
                // demonstrated that this static pruning rule can miss a real dependency.
                PredicateResult::False => estimate.has_candidate_miss_history(),
                // Only unresolved static topology remains subject to the generic materialization
                // floor.
                PredicateResult::Unknown => estimate.probability >= edge_materialization_threshold,
            },
        }
    }
    fn probability_confidence_and_observations(self) -> (u16, u16, u32, u32) {
        match self {
            Self::Binary => (u16::MAX, 0, 0, 0),
            Self::Adaptive { estimate, .. } => (
                quantize_q16(estimate.probability),
                quantize_q16(estimate.confidence),
                u32::try_from(estimate.positive_observations).unwrap_or(u32::MAX),
                u32::try_from(estimate.negative_observations).unwrap_or(u32::MAX),
            ),
        }
    }
}

fn materialize_static_profile_edge(
    transactions: &[CandidateTransaction],
    buckets: &[Vec<TxIndex>],
    instance_buckets: &[BTreeMap<InstanceId, Vec<TxIndex>>],
    compiled_predicates: &[CompiledPredicate],
    edges: &mut Vec<TransactionEdge>,
    profile_edge: &acg_profile_graph::LoadedProfileEdge,
    mode: StaticMaterialization,
) -> Result<(), CandidateGraphError> {
    let source_bucket = &buckets[profile_edge.source.0 as usize];
    let target_bucket = &buckets[profile_edge.target.0 as usize];
    let predicate = &compiled_predicates[profile_edge.index.0 as usize];

    // Once concrete execution has disproved candidate pruning for this relationship, do not let
    // the predicate's static same-instance fast path hide future pairs before the predicate can be
    // evaluated and the historical override policy applied.
    let may_use_instance_fast_path =
        predicate.requires_same_instance() && !mode.has_candidate_miss_history();

    if may_use_instance_fast_path {
        let source_instances = &instance_buckets[profile_edge.source.0 as usize];
        let target_instances = &instance_buckets[profile_edge.target.0 as usize];
        if profile_edge.source == profile_edge.target {
            for instance_bucket in source_instances.values() {
                materialize_same_bucket_pairs(
                    transactions,
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
                    transactions,
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
            transactions,
            edges,
            profile_edge,
            predicate,
            source_bucket,
            mode,
        )?;
    } else {
        materialize_cross_bucket_pairs(
            transactions,
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

fn materialize_same_bucket_pairs(
    transactions: &[CandidateTransaction],
    edges: &mut Vec<TransactionEdge>,
    profile_edge: &acg_profile_graph::LoadedProfileEdge,
    predicate: &CompiledPredicate,
    bucket: &[TxIndex],
    mode: StaticMaterialization,
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
    mode: StaticMaterialization,
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
    mode: StaticMaterialization,
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
    let (
        probability_q16,
        confidence_q16,
        concrete_conflict_observations,
        concrete_independent_observations,
    ) = mode.probability_confidence_and_observations();
    push_edge(
        edges,
        left_index,
        right_index,
        EdgeProvenance::Static {
            profile_edge_index: profile_edge.index,
        },
        result,
        profile_edge.conflict_kinds,
        probability_q16,
        confidence_q16,
        concrete_conflict_observations,
        concrete_independent_observations,
    );
    Ok(())
}

fn materialize_runtime_fallback_edge(
    buckets: &[Vec<TxIndex>],
    edges: &mut Vec<TransactionEdge>,
    fallback: &acg_feedback::RuntimeDiscoveredEdge,
    estimate: EdgeEstimate,
) {
    let source_bucket = &buckets[fallback.source.0 as usize];
    let target_bucket = &buckets[fallback.target.0 as usize];
    let probability_q16 = quantize_q16(estimate.probability);
    let confidence_q16 = quantize_q16(estimate.confidence);
    let concrete_conflict_observations =
        u32::try_from(estimate.positive_observations).unwrap_or(u32::MAX);
    let concrete_independent_observations =
        u32::try_from(estimate.negative_observations).unwrap_or(u32::MAX);
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
                    probability_q16,
                    confidence_q16,
                    concrete_conflict_observations,
                    concrete_independent_observations,
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
                    probability_q16,
                    confidence_q16,
                    concrete_conflict_observations,
                    concrete_independent_observations,
                );
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn push_edge(
    edges: &mut Vec<TransactionEdge>,
    left_index: TxIndex,
    right_index: TxIndex,
    provenance: EdgeProvenance,
    predicate_result: PredicateResult,
    conflict_kinds: ConflictKinds,
    probability_q16: u16,
    confidence_q16: u16,
    concrete_conflict_observations: u32,
    concrete_independent_observations: u32,
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
        probability_q16,
        confidence_q16,
        concrete_conflict_observations,
        concrete_independent_observations,
    });
}

fn finish_graph(
    transactions: Vec<CandidateTransaction>,
    mut edges: Vec<TransactionEdge>,
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

    Ok(CandidateGraph {
        transactions,
        edges,
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
    #[error(transparent)]
    Feedback(#[from] FeedbackError),
}
