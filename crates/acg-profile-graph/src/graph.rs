use std::collections::HashMap;

use acg_core::{
    ConflictKinds, EdgeRelation, PredicateTemplate, ProfileDefinition, ProfileEdgeIndex, ProfileId,
    StableProfileKey,
};
use thiserror::Error;

use crate::{ProfileGraphArtifact, PROFILE_GRAPH_ARTIFACT_VERSION};

#[derive(Clone, Copy, Debug)]
pub struct GraphLoadConfig {
    pub symbolic_prior_strength: f32,
    pub epsilon: f32,
}

impl Default for GraphLoadConfig {
    fn default() -> Self {
        Self {
            symbolic_prior_strength: 4.0,
            epsilon: 0.25,
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProfileRecord {
    pub id: ProfileId,
    pub definition: ProfileDefinition,
}

#[derive(Clone, Debug)]
pub struct LoadedProfileEdge {
    pub index: ProfileEdgeIndex,
    pub source: ProfileId,
    pub target: ProfileId,
    pub relation: EdgeRelation,
    pub conflict_kinds: ConflictKinds,
    pub symbolic_score: f32,
    pub predicate: PredicateTemplate,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AdjacencyEntry {
    pub neighbor: ProfileId,
    pub edge_index: ProfileEdgeIndex,
}

/// Immutable topology plus separate mutable-statistic arrays.
#[derive(Debug)]
pub struct ProfileGraph {
    profiles: Vec<ProfileRecord>,
    stable_to_id: HashMap<StableProfileKey, ProfileId>,
    edges: Vec<LoadedProfileEdge>,
    adjacency_offsets: Vec<u32>,
    adjacency_entries: Vec<AdjacencyEntry>,
    edge_alpha: Vec<f32>,
    edge_beta: Vec<f32>,
}

impl ProfileGraph {
    pub fn load(
        mut artifact: ProfileGraphArtifact,
        config: GraphLoadConfig,
    ) -> Result<Self, GraphLoadError> {
        validate_load_config(config)?;
        if artifact.format_version != PROFILE_GRAPH_ARTIFACT_VERSION {
            return Err(GraphLoadError::UnsupportedFormatVersion {
                actual: artifact.format_version,
                supported: PROFILE_GRAPH_ARTIFACT_VERSION,
            });
        }

        artifact.profiles.sort_by_key(|profile| profile.stable_key);
        let profile_count = u32::try_from(artifact.profiles.len())
            .map_err(|_| GraphLoadError::TooManyProfiles(artifact.profiles.len()))?;
        let mut profiles = Vec::with_capacity(artifact.profiles.len());
        let mut stable_to_id = HashMap::with_capacity(artifact.profiles.len());
        for (index, definition) in artifact.profiles.into_iter().enumerate() {
            let id = ProfileId(u32::try_from(index).expect("profile count validated"));
            if stable_to_id.insert(definition.stable_key, id).is_some() {
                return Err(GraphLoadError::DuplicateProfileKey(definition.stable_key));
            }
            if definition.descriptor.stable_key() != definition.stable_key {
                return Err(GraphLoadError::ProfileKeyMismatch {
                    entrypoint: definition.entrypoint_name.clone(),
                    stored: definition.stable_key,
                    computed: definition.descriptor.stable_key(),
                });
            }
            profiles.push(ProfileRecord { id, definition });
        }

        let mut unresolved_edges = artifact
            .edges
            .into_iter()
            .map(|edge| {
                let source = stable_to_id
                    .get(&edge.source)
                    .copied()
                    .ok_or(GraphLoadError::UnknownEdgeEndpoint(edge.source))?;
                let target = stable_to_id
                    .get(&edge.target)
                    .copied()
                    .ok_or(GraphLoadError::UnknownEdgeEndpoint(edge.target))?;
                if source > target {
                    return Err(GraphLoadError::NonCanonicalEdge {
                        source_profile: source,
                        target_profile: target,
                    });
                }
                Ok((source, target, edge))
            })
            .collect::<Result<Vec<_>, GraphLoadError>>()?;
        unresolved_edges.sort_by_key(|(source, target, _)| (*source, *target));

        let edge_count = u32::try_from(unresolved_edges.len())
            .map_err(|_| GraphLoadError::TooManyEdges(unresolved_edges.len()))?;
        for pair in unresolved_edges.windows(2) {
            if pair[0].0 == pair[1].0 && pair[0].1 == pair[1].1 {
                return Err(GraphLoadError::DuplicateEdgePair {
                    source_profile: pair[0].0,
                    target_profile: pair[0].1,
                });
            }
        }

        let mut adjacency_buckets = vec![Vec::<AdjacencyEntry>::new(); profile_count as usize];
        let mut edges = Vec::with_capacity(unresolved_edges.len());
        let mut edge_alpha = Vec::with_capacity(unresolved_edges.len());
        let mut edge_beta = Vec::with_capacity(unresolved_edges.len());
        for (index, (source, target, edge)) in unresolved_edges.into_iter().enumerate() {
            if !edge.symbolic_score.is_finite() || !(0.0..=1.0).contains(&edge.symbolic_score) {
                return Err(GraphLoadError::InvalidSymbolicScore(edge.symbolic_score));
            }
            let edge_index = ProfileEdgeIndex(u32::try_from(index).expect("edge count validated"));
            adjacency_buckets[source.0 as usize].push(AdjacencyEntry {
                neighbor: target,
                edge_index,
            });
            if source != target {
                adjacency_buckets[target.0 as usize].push(AdjacencyEntry {
                    neighbor: source,
                    edge_index,
                });
            }
            edge_alpha.push(config.epsilon + config.symbolic_prior_strength * edge.symbolic_score);
            edge_beta.push(
                config.epsilon + config.symbolic_prior_strength * (1.0 - edge.symbolic_score),
            );
            edges.push(LoadedProfileEdge {
                index: edge_index,
                source,
                target,
                relation: edge.relation,
                conflict_kinds: edge.conflict_kinds,
                symbolic_score: edge.symbolic_score,
                predicate: edge.predicate,
            });
        }

        let mut adjacency_offsets = Vec::with_capacity(profile_count as usize + 1);
        let mut adjacency_entries = Vec::new();
        adjacency_offsets.push(0);
        for bucket in &mut adjacency_buckets {
            bucket.sort_by_key(|entry| (entry.neighbor, entry.edge_index));
            adjacency_entries.extend_from_slice(bucket);
            adjacency_offsets.push(
                u32::try_from(adjacency_entries.len()).map_err(|_| {
                    GraphLoadError::TooManyAdjacencyEntries(adjacency_entries.len())
                })?,
            );
        }

        debug_assert_eq!(edges.len(), edge_count as usize);
        Ok(Self {
            profiles,
            stable_to_id,
            edges,
            adjacency_offsets,
            adjacency_entries,
            edge_alpha,
            edge_beta,
        })
    }

    pub fn profiles(&self) -> &[ProfileRecord] {
        &self.profiles
    }

    pub fn edges(&self) -> &[LoadedProfileEdge] {
        &self.edges
    }

    pub fn resolve(&self, stable_key: StableProfileKey) -> Option<ProfileId> {
        self.stable_to_id.get(&stable_key).copied()
    }

    pub fn profile(&self, id: ProfileId) -> Option<&ProfileRecord> {
        self.profiles.get(id.0 as usize)
    }

    pub fn neighbors(&self, id: ProfileId) -> &[AdjacencyEntry] {
        let Some(start) = self.adjacency_offsets.get(id.0 as usize).copied() else {
            return &[];
        };
        let Some(end) = self.adjacency_offsets.get(id.0 as usize + 1).copied() else {
            return &[];
        };
        &self.adjacency_entries[start as usize..end as usize]
    }

    pub fn edge_prior(&self, edge: ProfileEdgeIndex) -> Option<(f32, f32)> {
        let index = edge.0 as usize;
        Some((*self.edge_alpha.get(index)?, *self.edge_beta.get(index)?))
    }
}

#[derive(Debug, Error)]
pub enum GraphLoadError {
    #[error("unsupported graph artifact version {actual}; supported version is {supported}")]
    UnsupportedFormatVersion { actual: u16, supported: u16 },
    #[error("profile artifact contains too many profiles for u32 ids: {0}")]
    TooManyProfiles(usize),
    #[error("profile artifact contains too many edges for u32 ids: {0}")]
    TooManyEdges(usize),
    #[error("profile artifact contains too many adjacency entries for u32 offsets: {0}")]
    TooManyAdjacencyEntries(usize),
    #[error("duplicate stable profile key {0}")]
    DuplicateProfileKey(StableProfileKey),
    #[error(
        "stored profile key for {entrypoint:?} does not match descriptor: stored={stored}, computed={computed}"
    )]
    ProfileKeyMismatch {
        entrypoint: String,
        stored: StableProfileKey,
        computed: StableProfileKey,
    },
    #[error("edge references unknown profile key {0}")]
    UnknownEdgeEndpoint(StableProfileKey),
    #[error(
        "edge endpoints must be stored in stable-key order, got ({source_profile:?}, {target_profile:?})"
    )]
    NonCanonicalEdge {
        source_profile: ProfileId,
        target_profile: ProfileId,
    },
    #[error("duplicate profile edge pair ({source_profile:?}, {target_profile:?})")]
    DuplicateEdgePair {
        source_profile: ProfileId,
        target_profile: ProfileId,
    },
    #[error("symbolic score must be finite and within [0, 1], got {0}")]
    InvalidSymbolicScore(f32),
    #[error("symbolic prior strength must be finite and non-negative, got {0}")]
    InvalidPriorStrength(f32),
    #[error("epsilon must be finite and positive, got {0}")]
    InvalidEpsilon(f32),
}

fn validate_load_config(config: GraphLoadConfig) -> Result<(), GraphLoadError> {
    if !config.symbolic_prior_strength.is_finite() || config.symbolic_prior_strength < 0.0 {
        return Err(GraphLoadError::InvalidPriorStrength(
            config.symbolic_prior_strength,
        ));
    }
    if !config.epsilon.is_finite() || config.epsilon <= 0.0 {
        return Err(GraphLoadError::InvalidEpsilon(config.epsilon));
    }
    Ok(())
}
