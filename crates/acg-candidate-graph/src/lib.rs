//! Runtime-independent construction of per-block transaction conflict graphs.

use std::collections::{BTreeMap, BTreeSet};

use acg_core::{InstanceId, ProfileEdgeIndex, ProfileId, TxId, TxIndex};
use acg_predicate::{CompiledPredicate, InputBindings, PredicateResult};
use acg_profile_graph::ProfileGraph;
use serde::{Deserialize, Serialize};
use thiserror::Error;

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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TransactionEdge {
    pub source: TxIndex,
    pub target: TxIndex,
    pub profile_edge_index: ProfileEdgeIndex,
    pub predicate_result: PredicateResult,
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

    pub fn build(
        &self,
        transactions: Vec<CandidateTransaction>,
    ) -> Result<CandidateGraph, CandidateGraphError> {
        let profile_graph = self.profile_graph;
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

        let mut edges = Vec::<TransactionEdge>::new();
        let mut visited_profile_edges = BTreeSet::<ProfileEdgeIndex>::new();
        for (profile_offset, bucket) in buckets.iter().enumerate() {
            if bucket.is_empty() {
                continue;
            }
            let profile_id = ProfileId(
                u32::try_from(profile_offset).expect("profile count is represented by ProfileId"),
            );
            for adjacency in profile_graph.neighbors(profile_id) {
                if buckets[adjacency.neighbor.0 as usize].is_empty()
                    || !visited_profile_edges.insert(adjacency.edge_index)
                {
                    continue;
                }
                let profile_edge = &profile_graph.edges()[adjacency.edge_index.0 as usize];
                materialize_profile_edge(
                    &transactions,
                    &buckets,
                    &instance_buckets,
                    &self.compiled_predicates,
                    &mut edges,
                    profile_edge,
                )?;
            }
        }

        edges.sort_by_key(|edge| (edge.source, edge.target, edge.profile_edge_index));
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
}

fn materialize_profile_edge(
    transactions: &[CandidateTransaction],
    buckets: &[Vec<TxIndex>],
    instance_buckets: &[BTreeMap<InstanceId, Vec<TxIndex>>],
    compiled_predicates: &[CompiledPredicate],
    edges: &mut Vec<TransactionEdge>,
    profile_edge: &acg_profile_graph::LoadedProfileEdge,
) -> Result<(), CandidateGraphError> {
    let source_bucket = &buckets[profile_edge.source.0 as usize];
    let target_bucket = &buckets[profile_edge.target.0 as usize];
    let predicate = &compiled_predicates[profile_edge.index.0 as usize];
    if predicate.requires_same_instance() {
        let source_instances = &instance_buckets[profile_edge.source.0 as usize];
        let target_instances = &instance_buckets[profile_edge.target.0 as usize];
        if profile_edge.source == profile_edge.target {
            for instance_bucket in source_instances.values() {
                materialize_same_bucket_pairs(
                    transactions,
                    edges,
                    profile_edge.index,
                    predicate,
                    instance_bucket,
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
                    profile_edge.index,
                    predicate,
                    source_transactions,
                    target_transactions,
                )?;
            }
        }
    } else if profile_edge.source == profile_edge.target {
        materialize_same_bucket_pairs(
            transactions,
            edges,
            profile_edge.index,
            predicate,
            source_bucket,
        )?;
    } else {
        materialize_cross_bucket_pairs(
            transactions,
            edges,
            profile_edge.index,
            predicate,
            source_bucket,
            target_bucket,
        )?;
    }
    Ok(())
}

fn materialize_same_bucket_pairs(
    transactions: &[CandidateTransaction],
    edges: &mut Vec<TransactionEdge>,
    profile_edge_index: ProfileEdgeIndex,
    predicate: &CompiledPredicate,
    bucket: &[TxIndex],
) -> Result<(), CandidateGraphError> {
    for (left_offset, &left_index) in bucket.iter().enumerate() {
        for &right_index in bucket.iter().skip(left_offset + 1) {
            maybe_materialize_edge(
                transactions,
                edges,
                profile_edge_index,
                predicate,
                left_index,
                right_index,
            )?;
        }
    }
    Ok(())
}

fn materialize_cross_bucket_pairs(
    transactions: &[CandidateTransaction],
    edges: &mut Vec<TransactionEdge>,
    profile_edge_index: ProfileEdgeIndex,
    predicate: &CompiledPredicate,
    left_bucket: &[TxIndex],
    right_bucket: &[TxIndex],
) -> Result<(), CandidateGraphError> {
    for &left_index in left_bucket {
        for &right_index in right_bucket {
            maybe_materialize_edge(
                transactions,
                edges,
                profile_edge_index,
                predicate,
                left_index,
                right_index,
            )?;
        }
    }
    Ok(())
}

fn maybe_materialize_edge(
    transactions: &[CandidateTransaction],
    edges: &mut Vec<TransactionEdge>,
    profile_edge_index: ProfileEdgeIndex,
    predicate: &CompiledPredicate,
    left_index: TxIndex,
    right_index: TxIndex,
) -> Result<(), CandidateGraphError> {
    let left = &transactions[left_index.0 as usize];
    let right = &transactions[right_index.0 as usize];
    let result = predicate.evaluate(
        left.instance_id,
        &left.input_bindings,
        right.instance_id,
        &right.input_bindings,
    );
    if result == PredicateResult::False {
        return Ok(());
    }
    let (source, target) = if left_index <= right_index {
        (left_index, right_index)
    } else {
        (right_index, left_index)
    };
    edges.push(TransactionEdge {
        source,
        target,
        profile_edge_index,
        predicate_result: result,
    });
    Ok(())
}

#[derive(Debug, Error, PartialEq)]
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
}
