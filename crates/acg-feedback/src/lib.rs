//! Runtime-independent conflict observations, adaptive edge statistics, and feedback checkpoints.

use std::collections::{BTreeMap, BTreeSet};

use acg_core::{ConflictKinds, ProfileEdgeIndex, ProfileId, StableProfileKey, TxId};
use acg_profile_graph::ProfileGraph;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const FEEDBACK_CHECKPOINT_VERSION: u16 = 1;

/// Where one concrete observation came from.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSource {
    /// Speculative execution against a predicted state/order.
    PreExecution,
    /// Concrete accesses observed while executing the canonical serial block.
    CanonicalExecution,
    /// Pair-specific validation evidence supplied by a speculative executor.
    Validation,
    /// A replay/re-execution dependency attributed to a predecessor.
    Replay,
}

/// Whether the compared transaction pair interfered.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObservationOutcome {
    Conflict { conflict_kinds: ConflictKinds },
    Independent,
}

/// Destination for an observation.
///
/// Runtime-discovered observations are keyed by their profile pair and either create or update a
/// fallback edge in [`AdaptiveFeedbackStore`].
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ObservationTarget {
    Static { edge_index: ProfileEdgeIndex },
    RuntimeDiscovered,
}

/// One positive or negative piece of evidence.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ConflictObservation {
    pub source_profile: ProfileId,
    pub target_profile: ProfileId,
    pub left_tx: TxId,
    pub right_tx: TxId,
    pub outcome: ObservationOutcome,
    pub observation_source: ObservationSource,
    pub target: ObservationTarget,
    pub weight: f64,
    pub epoch: u64,
    /// False for a concrete overlap that the candidate graph failed to materialize.
    pub candidate_edge_present: bool,
}

impl ConflictObservation {
    #[allow(clippy::too_many_arguments)]
    pub fn conflict(
        source_profile: ProfileId,
        target_profile: ProfileId,
        left_tx: TxId,
        right_tx: TxId,
        conflict_kinds: ConflictKinds,
        observation_source: ObservationSource,
        target: ObservationTarget,
        weight: f64,
        epoch: u64,
        candidate_edge_present: bool,
    ) -> Result<Self, FeedbackError> {
        if conflict_kinds.is_empty() {
            return Err(FeedbackError::EmptyConflictKinds);
        }
        Self::new(
            source_profile,
            target_profile,
            left_tx,
            right_tx,
            ObservationOutcome::Conflict { conflict_kinds },
            observation_source,
            target,
            weight,
            epoch,
            candidate_edge_present,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn independent(
        source_profile: ProfileId,
        target_profile: ProfileId,
        left_tx: TxId,
        right_tx: TxId,
        observation_source: ObservationSource,
        target: ObservationTarget,
        weight: f64,
        epoch: u64,
        candidate_edge_present: bool,
    ) -> Result<Self, FeedbackError> {
        Self::new(
            source_profile,
            target_profile,
            left_tx,
            right_tx,
            ObservationOutcome::Independent,
            observation_source,
            target,
            weight,
            epoch,
            candidate_edge_present,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn new(
        source_profile: ProfileId,
        target_profile: ProfileId,
        left_tx: TxId,
        right_tx: TxId,
        outcome: ObservationOutcome,
        observation_source: ObservationSource,
        target: ObservationTarget,
        weight: f64,
        epoch: u64,
        candidate_edge_present: bool,
    ) -> Result<Self, FeedbackError> {
        validate_weight(weight)?;
        let (source_profile, target_profile) =
            canonical_profile_pair(source_profile, target_profile);
        Ok(Self {
            source_profile,
            target_profile,
            left_tx,
            right_tx,
            outcome,
            observation_source,
            target,
            weight,
            epoch,
            candidate_edge_present,
        })
    }
}

/// Per-block observation buffer. Absence from this buffer is deliberately not negative evidence.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ObservationBuffer {
    observations: Vec<ConflictObservation>,
}

impl ObservationBuffer {
    pub fn push(&mut self, observation: ConflictObservation) {
        self.observations.push(observation);
    }

    pub fn extend(&mut self, observations: impl IntoIterator<Item = ConflictObservation>) {
        self.observations.extend(observations);
    }

    pub fn observations(&self) -> &[ConflictObservation] {
        &self.observations
    }

    pub fn len(&self) -> usize {
        self.observations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.observations.is_empty()
    }

    pub fn into_observations(self) -> Vec<ConflictObservation> {
        self.observations
    }
}

/// Decayed Beta-Bernoulli state for one profile relationship.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct BetaStatistics {
    pub alpha: f64,
    pub beta: f64,
    pub last_update_epoch: u64,
    pub positive_observations: u64,
    pub negative_observations: u64,
}

impl BetaStatistics {
    pub fn new(alpha: f64, beta: f64, last_update_epoch: u64) -> Result<Self, FeedbackError> {
        if !alpha.is_finite() || alpha <= 0.0 {
            return Err(FeedbackError::InvalidAlpha(alpha));
        }
        if !beta.is_finite() || beta <= 0.0 {
            return Err(FeedbackError::InvalidBeta(beta));
        }
        Ok(Self {
            alpha,
            beta,
            last_update_epoch,
            positive_observations: 0,
            negative_observations: 0,
        })
    }

    pub fn from_probability(
        probability: f64,
        prior_strength: f64,
        epsilon: f64,
        epoch: u64,
    ) -> Result<Self, FeedbackError> {
        validate_probability(probability)?;
        if !prior_strength.is_finite() || prior_strength < 0.0 {
            return Err(FeedbackError::InvalidPriorStrength(prior_strength));
        }
        if !epsilon.is_finite() || epsilon <= 0.0 {
            return Err(FeedbackError::InvalidEpsilon(epsilon));
        }
        Self::new(
            epsilon + prior_strength * probability,
            epsilon + prior_strength * (1.0 - probability),
            epoch,
        )
    }

    pub fn probability(&self) -> f64 {
        self.alpha / (self.alpha + self.beta)
    }

    pub fn posterior_mass(&self) -> f64 {
        self.alpha + self.beta
    }

    pub fn confidence(&self, scale: f64) -> Result<f64, FeedbackError> {
        if !scale.is_finite() || scale <= 0.0 {
            return Err(FeedbackError::InvalidConfidenceScale(scale));
        }
        Ok(1.0 - (-self.posterior_mass() / scale).exp())
    }

    /// Projects decay to `epoch` without mutating stored counters.
    pub fn estimate_at(
        &self,
        epoch: u64,
        config: &AdaptiveFeedbackConfig,
    ) -> Result<EdgeEstimate, FeedbackError> {
        config.validate()?;
        let mut projected = *self;
        projected.decay_to(epoch, config.retention_factor)?;
        Ok(EdgeEstimate {
            alpha: projected.alpha,
            beta: projected.beta,
            probability: projected.probability(),
            posterior_mass: projected.posterior_mass(),
            confidence: projected.confidence(config.confidence_scale)?,
            epoch,
        })
    }

    fn decay_to(&mut self, epoch: u64, retention_factor: f64) -> Result<(), FeedbackError> {
        if epoch < self.last_update_epoch {
            return Err(FeedbackError::StaleObservationEpoch {
                observation_epoch: epoch,
                last_update_epoch: self.last_update_epoch,
            });
        }
        let delta = epoch - self.last_update_epoch;
        if delta > 0 {
            let decay = retention_factor.powf(delta as f64);
            self.alpha = (self.alpha * decay).max(f64::MIN_POSITIVE);
            self.beta = (self.beta * decay).max(f64::MIN_POSITIVE);
            self.last_update_epoch = epoch;
        }
        Ok(())
    }

    fn apply(
        &mut self,
        observation: &ConflictObservation,
        retention_factor: f64,
    ) -> Result<(), FeedbackError> {
        self.decay_to(observation.epoch, retention_factor)?;
        match observation.outcome {
            ObservationOutcome::Conflict { .. } => {
                self.alpha += observation.weight;
                self.positive_observations = self.positive_observations.saturating_add(1);
            }
            ObservationOutcome::Independent => {
                self.beta += observation.weight;
                self.negative_observations = self.negative_observations.saturating_add(1);
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EdgeEstimate {
    pub alpha: f64,
    pub beta: f64,
    pub probability: f64,
    pub posterior_mass: f64,
    pub confidence: f64,
    pub epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AdaptiveFeedbackConfig {
    /// Retained evidence per epoch in `(0, 1]`.
    pub retention_factor: f64,
    pub confidence_scale: f64,
    pub fallback_prior_probability: f64,
    pub fallback_prior_strength: f64,
    pub epsilon: f64,
}

impl Default for AdaptiveFeedbackConfig {
    fn default() -> Self {
        Self {
            retention_factor: 0.99,
            confidence_scale: 20.0,
            fallback_prior_probability: 0.5,
            fallback_prior_strength: 2.0,
            epsilon: 0.25,
        }
    }
}

impl AdaptiveFeedbackConfig {
    pub fn validate(&self) -> Result<(), FeedbackError> {
        if !self.retention_factor.is_finite()
            || self.retention_factor <= 0.0
            || self.retention_factor > 1.0
        {
            return Err(FeedbackError::InvalidRetentionFactor(self.retention_factor));
        }
        if !self.confidence_scale.is_finite() || self.confidence_scale <= 0.0 {
            return Err(FeedbackError::InvalidConfidenceScale(self.confidence_scale));
        }
        validate_probability(self.fallback_prior_probability)?;
        if !self.fallback_prior_strength.is_finite() || self.fallback_prior_strength < 0.0 {
            return Err(FeedbackError::InvalidPriorStrength(
                self.fallback_prior_strength,
            ));
        }
        if !self.epsilon.is_finite() || self.epsilon <= 0.0 {
            return Err(FeedbackError::InvalidEpsilon(self.epsilon));
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RuntimeEdgeId(pub u32);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RuntimeDiscoveredEdge {
    pub id: RuntimeEdgeId,
    pub source: ProfileId,
    pub target: ProfileId,
    pub source_key: StableProfileKey,
    pub target_key: StableProfileKey,
    pub conflict_kinds: ConflictKinds,
    pub discovered_epoch: u64,
    pub review_required: bool,
    pub statistics: BetaStatistics,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ApplySummary {
    pub positive_observations: usize,
    pub negative_observations: usize,
    pub fallback_edges_created: usize,
    /// Concrete conflicts absent from the candidate graph, whether due to predicate or topology miss.
    pub candidate_misses: usize,
}

/// Mutable statistics separated from the immutable [`ProfileGraph`] topology.
#[derive(Debug)]
pub struct AdaptiveFeedbackStore {
    static_statistics: Vec<BetaStatistics>,
    fallback_edges: Vec<RuntimeDiscoveredEdge>,
    fallback_by_pair: BTreeMap<(ProfileId, ProfileId), usize>,
}

impl AdaptiveFeedbackStore {
    pub fn from_graph(graph: &ProfileGraph, initial_epoch: u64) -> Result<Self, FeedbackError> {
        let mut static_statistics = Vec::with_capacity(graph.edges().len());
        for edge in graph.edges() {
            let (alpha, beta) = graph
                .edge_prior(edge.index)
                .ok_or(FeedbackError::UnknownStaticEdge(edge.index))?;
            static_statistics.push(BetaStatistics::new(
                f64::from(alpha),
                f64::from(beta),
                initial_epoch,
            )?);
        }
        Ok(Self {
            static_statistics,
            fallback_edges: Vec::new(),
            fallback_by_pair: BTreeMap::new(),
        })
    }

    pub fn static_statistics(&self, edge: ProfileEdgeIndex) -> Option<&BetaStatistics> {
        self.static_statistics.get(edge.0 as usize)
    }

    pub fn fallback_edges(&self) -> &[RuntimeDiscoveredEdge] {
        &self.fallback_edges
    }

    pub fn fallback_edge(
        &self,
        source: ProfileId,
        target: ProfileId,
    ) -> Option<&RuntimeDiscoveredEdge> {
        let pair = canonical_profile_pair(source, target);
        self.fallback_by_pair
            .get(&pair)
            .and_then(|index| self.fallback_edges.get(*index))
    }

    pub fn apply_batch(
        &mut self,
        graph: &ProfileGraph,
        buffer: ObservationBuffer,
        config: &AdaptiveFeedbackConfig,
    ) -> Result<ApplySummary, FeedbackError> {
        config.validate()?;
        let mut observations = buffer.into_observations();
        observations.sort_by_key(|observation| observation.epoch);
        let mut summary = ApplySummary::default();

        for observation in &observations {
            validate_weight(observation.weight)?;
            if !observation.candidate_edge_present
                && matches!(observation.outcome, ObservationOutcome::Conflict { .. })
            {
                summary.candidate_misses = summary.candidate_misses.saturating_add(1);
            }
            match observation.outcome {
                ObservationOutcome::Conflict { .. } => {
                    summary.positive_observations = summary.positive_observations.saturating_add(1)
                }
                ObservationOutcome::Independent => {
                    summary.negative_observations = summary.negative_observations.saturating_add(1)
                }
            }

            match observation.target {
                ObservationTarget::Static { edge_index } => {
                    validate_static_target(graph, edge_index, observation)?;
                    let statistics = self
                        .static_statistics
                        .get_mut(edge_index.0 as usize)
                        .ok_or(FeedbackError::UnknownStaticEdge(edge_index))?;
                    statistics.apply(observation, config.retention_factor)?;
                }
                ObservationTarget::RuntimeDiscovered => {
                    let pair = canonical_profile_pair(
                        observation.source_profile,
                        observation.target_profile,
                    );
                    let edge_index = if let Some(index) = self.fallback_by_pair.get(&pair).copied()
                    {
                        index
                    } else if matches!(observation.outcome, ObservationOutcome::Independent) {
                        return Err(FeedbackError::IndependentObservationWithoutFallback {
                            source_profile: pair.0,
                            target_profile: pair.1,
                        });
                    } else {
                        let edge = create_fallback_edge(
                            graph,
                            observation,
                            config,
                            self.fallback_edges.len(),
                        )?;
                        self.fallback_edges.push(edge);
                        let index = self.fallback_edges.len() - 1;
                        self.fallback_by_pair.insert(pair, index);
                        summary.fallback_edges_created =
                            summary.fallback_edges_created.saturating_add(1);
                        index
                    };
                    let edge = &mut self.fallback_edges[edge_index];
                    if let ObservationOutcome::Conflict { conflict_kinds } = observation.outcome {
                        edge.conflict_kinds |= conflict_kinds;
                    }
                    edge.statistics
                        .apply(observation, config.retention_factor)?;
                }
            }
        }
        Ok(summary)
    }

    pub fn checkpoint(&self, graph: &ProfileGraph) -> Result<FeedbackCheckpoint, FeedbackError> {
        if self.static_statistics.len() != graph.edges().len() {
            return Err(FeedbackError::StaticStatisticsLengthMismatch {
                statistics: self.static_statistics.len(),
                edges: graph.edges().len(),
            });
        }
        let static_edges = graph
            .edges()
            .iter()
            .zip(&self.static_statistics)
            .map(|(edge, statistics)| {
                let source = graph
                    .profile(edge.source)
                    .ok_or(FeedbackError::UnknownProfile(edge.source))?
                    .definition
                    .stable_key;
                let target = graph
                    .profile(edge.target)
                    .ok_or(FeedbackError::UnknownProfile(edge.target))?
                    .definition
                    .stable_key;
                Ok(StaticEdgeCheckpoint {
                    source,
                    target,
                    statistics: *statistics,
                })
            })
            .collect::<Result<Vec<_>, FeedbackError>>()?;
        let fallback_edges = self
            .fallback_edges
            .iter()
            .map(|edge| FallbackEdgeCheckpoint {
                source: edge.source_key,
                target: edge.target_key,
                conflict_kinds: edge.conflict_kinds,
                discovered_epoch: edge.discovered_epoch,
                review_required: edge.review_required,
                statistics: edge.statistics,
            })
            .collect();
        Ok(FeedbackCheckpoint {
            format_version: FEEDBACK_CHECKPOINT_VERSION,
            static_edges,
            fallback_edges,
        })
    }

    pub fn restore(
        graph: &ProfileGraph,
        checkpoint: FeedbackCheckpoint,
        initial_epoch: u64,
    ) -> Result<Self, FeedbackError> {
        if checkpoint.format_version != FEEDBACK_CHECKPOINT_VERSION {
            return Err(FeedbackError::UnsupportedCheckpointVersion {
                actual: checkpoint.format_version,
                supported: FEEDBACK_CHECKPOINT_VERSION,
            });
        }
        let mut store = Self::from_graph(graph, initial_epoch)?;
        let mut restored_static = BTreeSet::new();
        for saved in checkpoint.static_edges {
            let source = graph
                .resolve(saved.source)
                .ok_or(FeedbackError::UnknownStableProfile(saved.source))?;
            let target = graph
                .resolve(saved.target)
                .ok_or(FeedbackError::UnknownStableProfile(saved.target))?;
            let edge_index = graph.edge_between_profiles(source, target).ok_or(
                FeedbackError::CheckpointStaticEdgeMissing {
                    source_key: saved.source,
                    target_key: saved.target,
                },
            )?;
            if !restored_static.insert(edge_index) {
                return Err(FeedbackError::DuplicateStaticCheckpoint(edge_index));
            }
            validate_statistics(saved.statistics)?;
            store.static_statistics[edge_index.0 as usize] = saved.statistics;
        }

        for saved in checkpoint.fallback_edges {
            let source = graph
                .resolve(saved.source)
                .ok_or(FeedbackError::UnknownStableProfile(saved.source))?;
            let target = graph
                .resolve(saved.target)
                .ok_or(FeedbackError::UnknownStableProfile(saved.target))?;
            let pair = canonical_profile_pair(source, target);
            if store.fallback_by_pair.contains_key(&pair) {
                return Err(FeedbackError::DuplicateFallbackCheckpoint {
                    source_profile: pair.0,
                    target_profile: pair.1,
                });
            }
            validate_statistics(saved.statistics)?;
            if saved.conflict_kinds.is_empty() {
                return Err(FeedbackError::EmptyFallbackConflictKinds);
            }
            let id = RuntimeEdgeId(
                u32::try_from(store.fallback_edges.len())
                    .map_err(|_| FeedbackError::TooManyFallbackEdges)?,
            );
            let edge = RuntimeDiscoveredEdge {
                id,
                source: pair.0,
                target: pair.1,
                source_key: saved.source.min(saved.target),
                target_key: saved.source.max(saved.target),
                conflict_kinds: saved.conflict_kinds,
                discovered_epoch: saved.discovered_epoch,
                review_required: saved.review_required,
                statistics: saved.statistics,
            };
            store.fallback_edges.push(edge);
            store
                .fallback_by_pair
                .insert(pair, store.fallback_edges.len() - 1);
        }
        Ok(store)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FeedbackCheckpoint {
    pub format_version: u16,
    pub static_edges: Vec<StaticEdgeCheckpoint>,
    pub fallback_edges: Vec<FallbackEdgeCheckpoint>,
}

impl FeedbackCheckpoint {
    pub fn to_pretty_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec_pretty(self)
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, FeedbackError> {
        let checkpoint: Self = serde_json::from_slice(bytes)?;
        if checkpoint.format_version != FEEDBACK_CHECKPOINT_VERSION {
            return Err(FeedbackError::UnsupportedCheckpointVersion {
                actual: checkpoint.format_version,
                supported: FEEDBACK_CHECKPOINT_VERSION,
            });
        }
        Ok(checkpoint)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StaticEdgeCheckpoint {
    pub source: StableProfileKey,
    pub target: StableProfileKey,
    pub statistics: BetaStatistics,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FallbackEdgeCheckpoint {
    pub source: StableProfileKey,
    pub target: StableProfileKey,
    pub conflict_kinds: ConflictKinds,
    pub discovered_epoch: u64,
    pub review_required: bool,
    pub statistics: BetaStatistics,
}

fn create_fallback_edge(
    graph: &ProfileGraph,
    observation: &ConflictObservation,
    config: &AdaptiveFeedbackConfig,
    fallback_count: usize,
) -> Result<RuntimeDiscoveredEdge, FeedbackError> {
    let pair = canonical_profile_pair(observation.source_profile, observation.target_profile);
    let source_key = graph
        .profile(pair.0)
        .ok_or(FeedbackError::UnknownProfile(pair.0))?
        .definition
        .stable_key;
    let target_key = graph
        .profile(pair.1)
        .ok_or(FeedbackError::UnknownProfile(pair.1))?
        .definition
        .stable_key;
    let conflict_kinds = match observation.outcome {
        ObservationOutcome::Conflict { conflict_kinds } => conflict_kinds,
        ObservationOutcome::Independent => ConflictKinds::empty(),
    };
    let id = RuntimeEdgeId(
        u32::try_from(fallback_count).map_err(|_| FeedbackError::TooManyFallbackEdges)?,
    );
    Ok(RuntimeDiscoveredEdge {
        id,
        source: pair.0,
        target: pair.1,
        source_key: source_key.min(target_key),
        target_key: source_key.max(target_key),
        conflict_kinds,
        discovered_epoch: observation.epoch,
        review_required: true,
        statistics: BetaStatistics::from_probability(
            config.fallback_prior_probability,
            config.fallback_prior_strength,
            config.epsilon,
            observation.epoch,
        )?,
    })
}

fn validate_static_target(
    graph: &ProfileGraph,
    edge_index: ProfileEdgeIndex,
    observation: &ConflictObservation,
) -> Result<(), FeedbackError> {
    let edge = graph
        .edges()
        .get(edge_index.0 as usize)
        .ok_or(FeedbackError::UnknownStaticEdge(edge_index))?;
    let expected = canonical_profile_pair(edge.source, edge.target);
    let actual = canonical_profile_pair(observation.source_profile, observation.target_profile);
    if expected != actual {
        return Err(FeedbackError::StaticObservationProfileMismatch {
            edge_index,
            expected_source: expected.0,
            expected_target: expected.1,
            actual_source: actual.0,
            actual_target: actual.1,
        });
    }
    Ok(())
}

fn validate_statistics(statistics: BetaStatistics) -> Result<(), FeedbackError> {
    if !statistics.alpha.is_finite() || statistics.alpha <= 0.0 {
        return Err(FeedbackError::InvalidAlpha(statistics.alpha));
    }
    if !statistics.beta.is_finite() || statistics.beta <= 0.0 {
        return Err(FeedbackError::InvalidBeta(statistics.beta));
    }
    Ok(())
}

fn canonical_profile_pair(left: ProfileId, right: ProfileId) -> (ProfileId, ProfileId) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn validate_probability(value: f64) -> Result<(), FeedbackError> {
    if !value.is_finite() || !(0.0..=1.0).contains(&value) {
        return Err(FeedbackError::InvalidProbability(value));
    }
    Ok(())
}

fn validate_weight(value: f64) -> Result<(), FeedbackError> {
    if !value.is_finite() || value <= 0.0 {
        return Err(FeedbackError::InvalidObservationWeight(value));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum FeedbackError {
    #[error("observation weight must be finite and positive, got {0}")]
    InvalidObservationWeight(f64),
    #[error("probability must be finite and within [0, 1], got {0}")]
    InvalidProbability(f64),
    #[error("alpha must be finite and positive, got {0}")]
    InvalidAlpha(f64),
    #[error("beta must be finite and positive, got {0}")]
    InvalidBeta(f64),
    #[error("prior strength must be finite and non-negative, got {0}")]
    InvalidPriorStrength(f64),
    #[error("epsilon must be finite and positive, got {0}")]
    InvalidEpsilon(f64),
    #[error("retention factor must be finite and in (0, 1], got {0}")]
    InvalidRetentionFactor(f64),
    #[error("confidence scale must be finite and positive, got {0}")]
    InvalidConfidenceScale(f64),
    #[error("conflict observation must include at least one conflict kind")]
    EmptyConflictKinds,
    #[error("runtime-discovered fallback checkpoint must include conflict kinds")]
    EmptyFallbackConflictKinds,
    #[error("unknown static profile edge {0:?}")]
    UnknownStaticEdge(ProfileEdgeIndex),
    #[error("unknown profile {0:?}")]
    UnknownProfile(ProfileId),
    #[error("unknown stable profile {0}")]
    UnknownStableProfile(StableProfileKey),
    #[error(
        "observation for static edge {edge_index:?} has profile pair ({actual_source:?}, {actual_target:?}); expected ({expected_source:?}, {expected_target:?})"
    )]
    StaticObservationProfileMismatch {
        edge_index: ProfileEdgeIndex,
        expected_source: ProfileId,
        expected_target: ProfileId,
        actual_source: ProfileId,
        actual_target: ProfileId,
    },
    #[error(
        "negative runtime-discovered observation has no existing fallback edge for ({source_profile:?}, {target_profile:?})"
    )]
    IndependentObservationWithoutFallback {
        source_profile: ProfileId,
        target_profile: ProfileId,
    },
    #[error(
        "observation epoch {observation_epoch} precedes the edge's last update epoch {last_update_epoch}"
    )]
    StaleObservationEpoch {
        observation_epoch: u64,
        last_update_epoch: u64,
    },
    #[error("too many runtime-discovered fallback edges for u32 identifiers")]
    TooManyFallbackEdges,
    #[error("static statistics length {statistics} does not match graph edge count {edges}")]
    StaticStatisticsLengthMismatch { statistics: usize, edges: usize },
    #[error("unsupported feedback checkpoint version {actual}; supported version is {supported}")]
    UnsupportedCheckpointVersion { actual: u16, supported: u16 },
    #[error("checkpoint references missing static profile edge {source_key} -> {target_key}")]
    CheckpointStaticEdgeMissing {
        source_key: StableProfileKey,
        target_key: StableProfileKey,
    },
    #[error("duplicate static edge checkpoint for {0:?}")]
    DuplicateStaticCheckpoint(ProfileEdgeIndex),
    #[error("duplicate fallback checkpoint for ({source_profile:?}, {target_profile:?})")]
    DuplicateFallbackCheckpoint {
        source_profile: ProfileId,
        target_profile: ProfileId,
    },
    #[error("invalid feedback checkpoint JSON: {0}")]
    Json(#[from] serde_json::Error),
}
