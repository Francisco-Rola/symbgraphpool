//! Runtime-independent conflict observations, adaptive edge statistics, and feedback checkpoints.

use std::collections::{BTreeMap, BTreeSet};

use acg_core::{ConflictKinds, ProfileEdgeIndex, ProfileId, StableProfileKey, TxId};
use acg_profile_graph::ProfileGraph;
use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const FEEDBACK_CHECKPOINT_VERSION: u16 = 3;
const REPLAY_COST_CHECKPOINT_VERSION: u16 = 2;
const LEGACY_FEEDBACK_CHECKPOINT_VERSION: u16 = 1;

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
    /// Phase 5D replay-cost attribution. Zero for ordinary access/validation observations.
    #[serde(default)]
    pub replay_cost_nanos: u64,
    /// Number of later replayed transactions transitively attributable to this invalidation.
    #[serde(default)]
    pub invalidated_descendants: u32,
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
            replay_cost_nanos: 0,
            invalidated_descendants: 0,
        })
    }

    /// Attach measured replay impact to an already constructed conflict observation.
    ///
    /// The probability update remains the same; this additional evidence is consumed by Phase 5D's
    /// cost model when selecting scheduling risk for future blocks.
    pub fn with_replay_impact(
        mut self,
        replay_cost_nanos: u64,
        invalidated_descendants: u32,
    ) -> Self {
        self.replay_cost_nanos = replay_cost_nanos;
        self.invalidated_descendants = invalidated_descendants;
        self
    }

    pub fn has_replay_impact(&self) -> bool {
        self.replay_cost_nanos != 0 || self.invalidated_descendants != 0
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
    /// Concrete conflicts that were absent from the candidate graph when observed.
    ///
    /// For static edges this records predicate/materialization misses. For runtime-discovered
    /// edges it records topology misses. The counter is intentionally not decayed: once runtime
    /// execution has disproved absolute symbolic pruning, later graph construction must be able
    /// to consult adaptive history instead of treating the symbolic result as a proof.
    #[serde(default)]
    pub candidate_miss_observations: u64,
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
            candidate_miss_observations: 0,
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
            positive_observations: projected.positive_observations,
            negative_observations: projected.negative_observations,
            candidate_miss_observations: projected.candidate_miss_observations,
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

    fn apply_aggregate(
        &mut self,
        epoch: u64,
        retention_factor: f64,
        aggregate: &ObservationAggregate,
    ) -> Result<(), FeedbackError> {
        self.decay_to(epoch, retention_factor)?;
        self.alpha += aggregate.positive_weight;
        self.beta += aggregate.negative_weight;
        self.positive_observations = self
            .positive_observations
            .saturating_add(u64::try_from(aggregate.positive_observations).unwrap_or(u64::MAX));
        self.negative_observations = self
            .negative_observations
            .saturating_add(u64::try_from(aggregate.negative_observations).unwrap_or(u64::MAX));
        self.candidate_miss_observations = self
            .candidate_miss_observations
            .saturating_add(u64::try_from(aggregate.candidate_misses).unwrap_or(u64::MAX));
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
    pub positive_observations: u64,
    pub negative_observations: u64,
    pub candidate_miss_observations: u64,
    pub epoch: u64,
}

impl EdgeEstimate {
    /// Whether concrete execution has ever observed a conflict that candidate construction missed.
    ///
    /// Phase 4 uses this as the gate that allows learned history to override an otherwise-false
    /// symbolic predicate. A symbolic prior by itself is not enough to bypass concrete pruning.
    pub fn has_candidate_miss_history(&self) -> bool {
        self.candidate_miss_observations > 0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReplayCostStatistics {
    /// Decayed weighted sum of direct replay wall time.
    pub weighted_replay_cost_nanos: f64,
    /// Decayed weighted sum of transitive replay fan-out.
    pub weighted_invalidated_descendants: f64,
    /// Decayed total observation weight used by both means.
    pub observation_weight: f64,
    pub last_update_epoch: u64,
    pub replay_observations: u64,
    pub total_replay_cost_nanos: u64,
    pub total_invalidated_descendants: u64,
}

impl Default for ReplayCostStatistics {
    fn default() -> Self {
        Self::new(0)
    }
}

impl ReplayCostStatistics {
    pub fn new(last_update_epoch: u64) -> Self {
        Self {
            weighted_replay_cost_nanos: 0.0,
            weighted_invalidated_descendants: 0.0,
            observation_weight: 0.0,
            last_update_epoch,
            replay_observations: 0,
            total_replay_cost_nanos: 0,
            total_invalidated_descendants: 0,
        }
    }

    pub fn estimate_at(
        &self,
        epoch: u64,
        config: &AdaptiveFeedbackConfig,
    ) -> Result<ReplayCostEstimate, FeedbackError> {
        config.validate()?;
        let mut projected = *self;
        projected.decay_to(epoch, config.retention_factor)?;
        let (expected_replay_cost_nanos, expected_invalidated_descendants) =
            if projected.observation_weight > 0.0 {
                (
                    projected.weighted_replay_cost_nanos / projected.observation_weight,
                    projected.weighted_invalidated_descendants / projected.observation_weight,
                )
            } else {
                (0.0, 0.0)
            };
        let confidence = if projected.observation_weight > 0.0 {
            1.0 - (-projected.observation_weight / config.confidence_scale).exp()
        } else {
            0.0
        };
        Ok(ReplayCostEstimate {
            expected_replay_cost_nanos,
            expected_invalidated_descendants,
            observation_weight: projected.observation_weight,
            confidence,
            replay_observations: projected.replay_observations,
            total_replay_cost_nanos: projected.total_replay_cost_nanos,
            total_invalidated_descendants: projected.total_invalidated_descendants,
            epoch,
        })
    }

    fn decay_to(&mut self, epoch: u64, retention_factor: f64) -> Result<(), FeedbackError> {
        if epoch < self.last_update_epoch {
            return Err(FeedbackError::StaleReplayCostEpoch {
                observation_epoch: epoch,
                last_update_epoch: self.last_update_epoch,
            });
        }
        let delta = epoch - self.last_update_epoch;
        if delta > 0 {
            let decay = retention_factor.powf(delta as f64);
            self.weighted_replay_cost_nanos *= decay;
            self.weighted_invalidated_descendants *= decay;
            self.observation_weight *= decay;
            self.last_update_epoch = epoch;
        }
        Ok(())
    }

    fn apply_aggregate(
        &mut self,
        batch: ReplayCostBatch,
        epoch: u64,
        retention_factor: f64,
    ) -> Result<(), FeedbackError> {
        self.decay_to(epoch, retention_factor)?;
        self.weighted_replay_cost_nanos += batch.weighted_replay_cost_nanos;
        self.weighted_invalidated_descendants += batch.weighted_invalidated_descendants;
        self.observation_weight += batch.observation_weight;
        self.replay_observations = self
            .replay_observations
            .saturating_add(u64::try_from(batch.replay_observations).unwrap_or(u64::MAX));
        self.total_replay_cost_nanos = self
            .total_replay_cost_nanos
            .saturating_add(batch.total_replay_cost_nanos);
        self.total_invalidated_descendants = self
            .total_invalidated_descendants
            .saturating_add(batch.total_invalidated_descendants);
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct ReplayCostBatch {
    weighted_replay_cost_nanos: f64,
    weighted_invalidated_descendants: f64,
    observation_weight: f64,
    replay_observations: usize,
    total_replay_cost_nanos: u64,
    total_invalidated_descendants: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ReplayCostEstimate {
    pub expected_replay_cost_nanos: f64,
    pub expected_invalidated_descendants: f64,
    pub observation_weight: f64,
    pub confidence: f64,
    pub replay_observations: u64,
    pub total_replay_cost_nanos: u64,
    pub total_invalidated_descendants: u64,
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

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SerializationCostEstimate {
    pub expected_serialization_cost_nanos: f64,
    pub observation_weight: f64,
    pub confidence: f64,
    pub observations: u64,
    pub total_serialization_cost_nanos: u64,
    pub epoch: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SerializationCostStatistics {
    /// Decayed weighted sum of marginal dependency-ready delay.
    pub weighted_serialization_cost_nanos: f64,
    /// Decayed observation weight.
    pub observation_weight: f64,
    pub last_update_epoch: u64,
    pub observations: u64,
    pub total_serialization_cost_nanos: u64,
}

impl Default for SerializationCostStatistics {
    fn default() -> Self {
        Self::new(0)
    }
}

impl SerializationCostStatistics {
    pub fn new(last_update_epoch: u64) -> Self {
        Self {
            weighted_serialization_cost_nanos: 0.0,
            observation_weight: 0.0,
            last_update_epoch,
            observations: 0,
            total_serialization_cost_nanos: 0,
        }
    }

    pub fn estimate_at(
        &self,
        epoch: u64,
        config: &AdaptiveFeedbackConfig,
    ) -> Result<SerializationCostEstimate, FeedbackError> {
        config.validate()?;
        let mut projected = *self;
        projected.decay_to(epoch, config.retention_factor)?;
        let expected_serialization_cost_nanos = if projected.observation_weight > 0.0 {
            projected.weighted_serialization_cost_nanos / projected.observation_weight
        } else {
            0.0
        };
        let confidence = if projected.observation_weight > 0.0 {
            1.0 - (-projected.observation_weight / config.confidence_scale).exp()
        } else {
            0.0
        };
        Ok(SerializationCostEstimate {
            expected_serialization_cost_nanos,
            observation_weight: projected.observation_weight,
            confidence,
            observations: projected.observations,
            total_serialization_cost_nanos: projected.total_serialization_cost_nanos,
            epoch,
        })
    }

    fn decay_to(&mut self, epoch: u64, retention_factor: f64) -> Result<(), FeedbackError> {
        if epoch < self.last_update_epoch {
            return Err(FeedbackError::StaleSerializationCostEpoch {
                observation_epoch: epoch,
                last_update_epoch: self.last_update_epoch,
            });
        }
        let delta = epoch - self.last_update_epoch;
        if delta > 0 {
            let decay = retention_factor.powf(delta as f64);
            self.weighted_serialization_cost_nanos *= decay;
            self.observation_weight *= decay;
            self.last_update_epoch = epoch;
        }
        Ok(())
    }

    fn apply(
        &mut self,
        serialization_cost_nanos: u64,
        weight: f64,
        epoch: u64,
        retention_factor: f64,
    ) -> Result<(), FeedbackError> {
        validate_weight(weight)?;
        self.apply_aggregate(
            serialization_cost_nanos as f64 * weight,
            weight,
            1,
            serialization_cost_nanos,
            epoch,
            retention_factor,
        )
    }

    fn apply_aggregate(
        &mut self,
        weighted_serialization_cost_nanos: f64,
        observation_weight: f64,
        observations: usize,
        total_serialization_cost_nanos: u64,
        epoch: u64,
        retention_factor: f64,
    ) -> Result<(), FeedbackError> {
        self.decay_to(epoch, retention_factor)?;
        self.weighted_serialization_cost_nanos += weighted_serialization_cost_nanos;
        self.observation_weight += observation_weight;
        self.observations = self
            .observations
            .saturating_add(u64::try_from(observations).unwrap_or(u64::MAX));
        self.total_serialization_cost_nanos = self
            .total_serialization_cost_nanos
            .saturating_add(total_serialization_cost_nanos);
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
    #[serde(default)]
    pub replay_cost_statistics: ReplayCostStatistics,
    #[serde(default)]
    pub serialization_cost_statistics: SerializationCostStatistics,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ObservationBatchTarget {
    Static(ProfileEdgeIndex),
    Runtime(ProfileId, ProfileId),
}

#[derive(Clone, Debug)]
struct ObservationAggregate {
    source_profile: ProfileId,
    target_profile: ProfileId,
    positive_weight: f64,
    negative_weight: f64,
    positive_observations: usize,
    negative_observations: usize,
    candidate_misses: usize,
    conflict_kinds: ConflictKinds,
    replay_impact: ReplayCostBatch,
}

impl ObservationAggregate {
    fn new(source_profile: ProfileId, target_profile: ProfileId) -> Self {
        let (source_profile, target_profile) =
            canonical_profile_pair(source_profile, target_profile);
        Self {
            source_profile,
            target_profile,
            positive_weight: 0.0,
            negative_weight: 0.0,
            positive_observations: 0,
            negative_observations: 0,
            candidate_misses: 0,
            conflict_kinds: ConflictKinds::empty(),
            replay_impact: ReplayCostBatch::default(),
        }
    }

    fn add_observation(&mut self, observation: &ConflictObservation) -> Result<(), FeedbackError> {
        if observation.has_replay_impact()
            && matches!(observation.outcome, ObservationOutcome::Independent)
        {
            return Err(FeedbackError::ReplayImpactRequiresConflict);
        }
        match observation.outcome {
            ObservationOutcome::Conflict { conflict_kinds } => self.add_conflicts(
                conflict_kinds,
                observation.weight,
                1,
                usize::from(!observation.candidate_edge_present),
                observation.has_replay_impact().then_some((
                    observation.replay_cost_nanos,
                    observation.invalidated_descendants,
                )),
            ),
            ObservationOutcome::Independent => self.add_independent(observation.weight, 1),
        }
    }

    fn add_conflicts(
        &mut self,
        conflict_kinds: ConflictKinds,
        weight: f64,
        count: usize,
        candidate_misses: usize,
        replay_impact: Option<(u64, u32)>,
    ) -> Result<(), FeedbackError> {
        validate_weight(weight)?;
        if conflict_kinds.is_empty() {
            return Err(FeedbackError::EmptyConflictKinds);
        }
        let weighted = weighted_count(weight, count)?;
        self.positive_weight += weighted;
        self.positive_observations = self.positive_observations.saturating_add(count);
        self.candidate_misses = self.candidate_misses.saturating_add(candidate_misses);
        self.conflict_kinds |= conflict_kinds;
        if let Some((replay_cost_nanos, invalidated_descendants)) = replay_impact {
            if count != 1 {
                return Err(FeedbackError::ReplayImpactRequiresSingleObservation);
            }
            self.replay_impact.replay_observations =
                self.replay_impact.replay_observations.saturating_add(1);
            self.replay_impact.weighted_replay_cost_nanos += replay_cost_nanos as f64 * weight;
            self.replay_impact.weighted_invalidated_descendants +=
                f64::from(invalidated_descendants) * weight;
            self.replay_impact.observation_weight += weight;
            self.replay_impact.total_replay_cost_nanos = self
                .replay_impact
                .total_replay_cost_nanos
                .saturating_add(replay_cost_nanos);
            self.replay_impact.total_invalidated_descendants = self
                .replay_impact
                .total_invalidated_descendants
                .saturating_add(u64::from(invalidated_descendants));
        }
        Ok(())
    }

    fn add_independent(&mut self, weight: f64, count: usize) -> Result<(), FeedbackError> {
        validate_weight(weight)?;
        self.negative_weight += weighted_count(weight, count)?;
        self.negative_observations = self.negative_observations.saturating_add(count);
        Ok(())
    }
}

/// Per-block feedback accumulator that aggregates directly at the persisted relationship boundary.
///
/// Runtime collectors should prefer this type over constructing one [`ConflictObservation`] per
/// transaction pair. It preserves raw observation counts and total statistical weight while
/// keeping memory and state-application work proportional to the number of learned relationships.
#[derive(Clone, Copy, Debug)]
pub struct AggregatedConflictBatch {
    pub source_profile: ProfileId,
    pub target_profile: ProfileId,
    pub conflict_kinds: ConflictKinds,
    pub target: ObservationTarget,
    pub weight: f64,
    pub epoch: u64,
    pub candidate_edge_present: bool,
    pub count: usize,
}

#[derive(Clone, Debug, Default)]
pub struct AggregatedObservationBuffer {
    aggregates: BTreeMap<(u64, ObservationBatchTarget), ObservationAggregate>,
}

impl AggregatedObservationBuffer {
    pub fn record_observation(
        &mut self,
        observation: &ConflictObservation,
    ) -> Result<(), FeedbackError> {
        let target = observation_batch_target(
            observation.source_profile,
            observation.target_profile,
            observation.target,
        );
        let aggregate = self
            .aggregates
            .entry((observation.epoch, target))
            .or_insert_with(|| {
                ObservationAggregate::new(observation.source_profile, observation.target_profile)
            });
        aggregate.add_observation(observation)
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_conflict(
        &mut self,
        source_profile: ProfileId,
        target_profile: ProfileId,
        conflict_kinds: ConflictKinds,
        target: ObservationTarget,
        weight: f64,
        epoch: u64,
        candidate_edge_present: bool,
    ) -> Result<(), FeedbackError> {
        self.record_conflict_with_replay(
            source_profile,
            target_profile,
            conflict_kinds,
            target,
            weight,
            epoch,
            candidate_edge_present,
            None,
        )
    }

    pub fn record_conflict_batch(
        &mut self,
        batch: AggregatedConflictBatch,
    ) -> Result<(), FeedbackError> {
        if batch.count == 0 {
            return Ok(());
        }
        let batch_target =
            observation_batch_target(batch.source_profile, batch.target_profile, batch.target);
        let aggregate = self
            .aggregates
            .entry((batch.epoch, batch_target))
            .or_insert_with(|| {
                ObservationAggregate::new(batch.source_profile, batch.target_profile)
            });
        aggregate.add_conflicts(
            batch.conflict_kinds,
            batch.weight,
            batch.count,
            if batch.candidate_edge_present {
                0
            } else {
                batch.count
            },
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_conflict_with_replay(
        &mut self,
        source_profile: ProfileId,
        target_profile: ProfileId,
        conflict_kinds: ConflictKinds,
        target: ObservationTarget,
        weight: f64,
        epoch: u64,
        candidate_edge_present: bool,
        replay_impact: Option<(u64, u32)>,
    ) -> Result<(), FeedbackError> {
        let batch_target = observation_batch_target(source_profile, target_profile, target);
        let aggregate = self
            .aggregates
            .entry((epoch, batch_target))
            .or_insert_with(|| ObservationAggregate::new(source_profile, target_profile));
        aggregate.add_conflicts(
            conflict_kinds,
            weight,
            1,
            usize::from(!candidate_edge_present),
            replay_impact,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_independent_count(
        &mut self,
        source_profile: ProfileId,
        target_profile: ProfileId,
        target: ObservationTarget,
        weight: f64,
        epoch: u64,
        count: usize,
    ) -> Result<(), FeedbackError> {
        if count == 0 {
            return Ok(());
        }
        let batch_target = observation_batch_target(source_profile, target_profile, target);
        let aggregate = self
            .aggregates
            .entry((epoch, batch_target))
            .or_insert_with(|| ObservationAggregate::new(source_profile, target_profile));
        aggregate.add_independent(weight, count)
    }

    pub fn relationship_batches(&self) -> usize {
        self.aggregates.len()
    }

    pub fn raw_observations(&self) -> usize {
        self.aggregates.values().fold(0_usize, |total, aggregate| {
            total
                .saturating_add(aggregate.positive_observations)
                .saturating_add(aggregate.negative_observations)
        })
    }
}

fn observation_batch_target(
    source_profile: ProfileId,
    target_profile: ProfileId,
    target: ObservationTarget,
) -> ObservationBatchTarget {
    match target {
        ObservationTarget::Static { edge_index } => ObservationBatchTarget::Static(edge_index),
        ObservationTarget::RuntimeDiscovered => {
            let pair = canonical_profile_pair(source_profile, target_profile);
            ObservationBatchTarget::Runtime(pair.0, pair.1)
        }
    }
}

fn weighted_count(weight: f64, count: usize) -> Result<f64, FeedbackError> {
    let weighted = weight * count as f64;
    if !weighted.is_finite() {
        return Err(FeedbackError::InvalidAggregatedWeight { weight, count });
    }
    Ok(weighted)
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ApplySummary {
    /// Raw concrete pair observations retained for scientific accounting.
    pub positive_observations: usize,
    pub negative_observations: usize,
    pub fallback_edges_created: usize,
    /// Concrete conflicts absent from the candidate graph, whether due to predicate or topology miss.
    pub candidate_misses: usize,
    /// Conflict observations carrying Phase 5D measured replay impact.
    pub replay_impact_observations: usize,
    /// Direct replay nanoseconds attributed across the observations in this batch.
    pub attributed_replay_cost_nanos: u64,
    /// Transitive replay descendants attributed across the observations in this batch.
    pub attributed_invalidated_descendants: u64,
    /// Number of profile-relationship/epoch batches mutated by probability/replay feedback.
    pub observation_batches_applied: usize,
    /// Phase 5E marginal dependency-ready delay observations learned from realized scheduling.
    pub serialization_cost_observations: usize,
    /// Sum of marginal dependency-ready delay attributed in this batch.
    pub attributed_serialization_cost_nanos: u64,
    /// Number of profile relationships mutated by serialization-cost feedback.
    pub serialization_cost_batches_applied: usize,
}

/// Mutable statistics separated from the immutable [`ProfileGraph`] topology.
#[derive(Debug)]
pub struct AdaptiveFeedbackStore {
    static_statistics: Vec<BetaStatistics>,
    static_replay_costs: Vec<ReplayCostStatistics>,
    static_serialization_costs: Vec<SerializationCostStatistics>,
    fallback_edges: Vec<RuntimeDiscoveredEdge>,
    fallback_by_pair: BTreeMap<(ProfileId, ProfileId), usize>,
    fallback_adjacency: BTreeMap<ProfileId, Vec<usize>>,
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
        let static_replay_costs =
            vec![ReplayCostStatistics::new(initial_epoch); graph.edges().len()];
        let static_serialization_costs =
            vec![SerializationCostStatistics::new(initial_epoch); graph.edges().len()];
        Ok(Self {
            static_statistics,
            static_replay_costs,
            static_serialization_costs,
            fallback_edges: Vec::new(),
            fallback_by_pair: BTreeMap::new(),
            fallback_adjacency: BTreeMap::new(),
        })
    }

    pub fn static_statistics(&self, edge: ProfileEdgeIndex) -> Option<&BetaStatistics> {
        self.static_statistics.get(edge.0 as usize)
    }

    pub fn static_replay_cost_statistics(
        &self,
        edge: ProfileEdgeIndex,
    ) -> Option<&ReplayCostStatistics> {
        self.static_replay_costs.get(edge.0 as usize)
    }

    pub fn estimate_static_replay_cost(
        &self,
        edge: ProfileEdgeIndex,
        epoch: u64,
        config: &AdaptiveFeedbackConfig,
    ) -> Result<ReplayCostEstimate, FeedbackError> {
        self.static_replay_cost_statistics(edge)
            .ok_or(FeedbackError::UnknownStaticEdge(edge))?
            .estimate_at(epoch, config)
    }

    pub fn static_serialization_cost_statistics(
        &self,
        edge: ProfileEdgeIndex,
    ) -> Option<&SerializationCostStatistics> {
        self.static_serialization_costs.get(edge.0 as usize)
    }

    pub fn estimate_static_serialization_cost(
        &self,
        edge: ProfileEdgeIndex,
        epoch: u64,
        config: &AdaptiveFeedbackConfig,
    ) -> Result<SerializationCostEstimate, FeedbackError> {
        self.static_serialization_cost_statistics(edge)
            .ok_or(FeedbackError::UnknownStaticEdge(edge))?
            .estimate_at(epoch, config)
    }

    /// Returns a current-epoch estimate for one immutable/static profile edge without mutating it.
    pub fn estimate_static_edge(
        &self,
        edge: ProfileEdgeIndex,
        epoch: u64,
        config: &AdaptiveFeedbackConfig,
    ) -> Result<EdgeEstimate, FeedbackError> {
        self.static_statistics(edge)
            .ok_or(FeedbackError::UnknownStaticEdge(edge))?
            .estimate_at(epoch, config)
    }

    pub fn fallback_edges(&self) -> &[RuntimeDiscoveredEdge] {
        &self.fallback_edges
    }

    pub fn fallback_edge_by_id(&self, id: RuntimeEdgeId) -> Option<&RuntimeDiscoveredEdge> {
        self.fallback_edges.get(id.0 as usize)
    }

    /// Returns a current-epoch estimate for one runtime-discovered relationship without mutating it.
    pub fn estimate_fallback_edge(
        &self,
        id: RuntimeEdgeId,
        epoch: u64,
        config: &AdaptiveFeedbackConfig,
    ) -> Result<EdgeEstimate, FeedbackError> {
        self.fallback_edge_by_id(id)
            .ok_or(FeedbackError::UnknownRuntimeEdge(id))?
            .statistics
            .estimate_at(epoch, config)
    }

    pub fn estimate_fallback_replay_cost(
        &self,
        id: RuntimeEdgeId,
        epoch: u64,
        config: &AdaptiveFeedbackConfig,
    ) -> Result<ReplayCostEstimate, FeedbackError> {
        self.fallback_edge_by_id(id)
            .ok_or(FeedbackError::UnknownRuntimeEdge(id))?
            .replay_cost_statistics
            .estimate_at(epoch, config)
    }

    pub fn estimate_fallback_serialization_cost(
        &self,
        id: RuntimeEdgeId,
        epoch: u64,
        config: &AdaptiveFeedbackConfig,
    ) -> Result<SerializationCostEstimate, FeedbackError> {
        self.fallback_edge_by_id(id)
            .ok_or(FeedbackError::UnknownRuntimeEdge(id))?
            .serialization_cost_statistics
            .estimate_at(epoch, config)
    }

    pub fn record_static_serialization_cost(
        &mut self,
        edge: ProfileEdgeIndex,
        serialization_cost_nanos: u64,
        weight: f64,
        epoch: u64,
        config: &AdaptiveFeedbackConfig,
    ) -> Result<ApplySummary, FeedbackError> {
        config.validate()?;
        let statistics = self
            .static_serialization_costs
            .get_mut(edge.0 as usize)
            .ok_or(FeedbackError::UnknownStaticEdge(edge))?;
        statistics.apply(
            serialization_cost_nanos,
            weight,
            epoch,
            config.retention_factor,
        )?;
        Ok(ApplySummary {
            serialization_cost_observations: 1,
            attributed_serialization_cost_nanos: serialization_cost_nanos,
            serialization_cost_batches_applied: 1,
            ..ApplySummary::default()
        })
    }

    pub fn record_static_serialization_cost_batch(
        &mut self,
        edge: ProfileEdgeIndex,
        total_serialization_cost_nanos: u64,
        observations: usize,
        epoch: u64,
        config: &AdaptiveFeedbackConfig,
    ) -> Result<ApplySummary, FeedbackError> {
        config.validate()?;
        if observations == 0 {
            return Ok(ApplySummary::default());
        }
        let statistics = self
            .static_serialization_costs
            .get_mut(edge.0 as usize)
            .ok_or(FeedbackError::UnknownStaticEdge(edge))?;
        statistics.apply_aggregate(
            total_serialization_cost_nanos as f64,
            observations as f64,
            observations,
            total_serialization_cost_nanos,
            epoch,
            config.retention_factor,
        )?;
        Ok(ApplySummary {
            serialization_cost_observations: observations,
            attributed_serialization_cost_nanos: total_serialization_cost_nanos,
            serialization_cost_batches_applied: 1,
            ..ApplySummary::default()
        })
    }

    pub fn record_fallback_serialization_cost(
        &mut self,
        id: RuntimeEdgeId,
        serialization_cost_nanos: u64,
        weight: f64,
        epoch: u64,
        config: &AdaptiveFeedbackConfig,
    ) -> Result<ApplySummary, FeedbackError> {
        config.validate()?;
        let edge = self
            .fallback_edges
            .get_mut(id.0 as usize)
            .ok_or(FeedbackError::UnknownRuntimeEdge(id))?;
        edge.serialization_cost_statistics.apply(
            serialization_cost_nanos,
            weight,
            epoch,
            config.retention_factor,
        )?;
        Ok(ApplySummary {
            serialization_cost_observations: 1,
            attributed_serialization_cost_nanos: serialization_cost_nanos,
            serialization_cost_batches_applied: 1,
            ..ApplySummary::default()
        })
    }

    pub fn record_fallback_serialization_cost_batch(
        &mut self,
        id: RuntimeEdgeId,
        total_serialization_cost_nanos: u64,
        observations: usize,
        epoch: u64,
        config: &AdaptiveFeedbackConfig,
    ) -> Result<ApplySummary, FeedbackError> {
        config.validate()?;
        if observations == 0 {
            return Ok(ApplySummary::default());
        }
        let edge = self
            .fallback_edges
            .get_mut(id.0 as usize)
            .ok_or(FeedbackError::UnknownRuntimeEdge(id))?;
        edge.serialization_cost_statistics.apply_aggregate(
            total_serialization_cost_nanos as f64,
            observations as f64,
            observations,
            total_serialization_cost_nanos,
            epoch,
            config.retention_factor,
        )?;
        Ok(ApplySummary {
            serialization_cost_observations: observations,
            attributed_serialization_cost_nanos: total_serialization_cost_nanos,
            serialization_cost_batches_applied: 1,
            ..ApplySummary::default()
        })
    }

    /// Runtime-discovered relationships incident to `profile`.
    ///
    /// This adjacency is maintained when fallback edges are created/restored so Phase 4 candidate
    /// construction can traverse learned topology in the same profile-bucket style as the static
    /// graph instead of scanning every fallback relationship.
    pub fn fallback_edges_for_profile(
        &self,
        profile: ProfileId,
    ) -> impl Iterator<Item = &RuntimeDiscoveredEdge> {
        self.fallback_adjacency
            .get(&profile)
            .into_iter()
            .flatten()
            .filter_map(|index| self.fallback_edges.get(*index))
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
        let mut aggregated = AggregatedObservationBuffer::default();
        for observation in buffer.into_observations() {
            aggregated.record_observation(&observation)?;
        }
        self.apply_aggregated_batch(graph, aggregated, config)
    }

    /// Apply feedback that was already aggregated by the runtime collector.
    ///
    /// This is the hot-path API for production feedback collection. It preserves the same raw
    /// observation accounting as [`Self::apply_batch`] without allocating one observation object
    /// per transaction pair or repeating relationship-key lookups in the store.
    pub fn apply_aggregated_batch(
        &mut self,
        graph: &ProfileGraph,
        buffer: AggregatedObservationBuffer,
        config: &AdaptiveFeedbackConfig,
    ) -> Result<ApplySummary, FeedbackError> {
        config.validate()?;
        let mut summary = ApplySummary {
            observation_batches_applied: buffer.aggregates.len(),
            ..ApplySummary::default()
        };

        for ((epoch, target), aggregate) in buffer.aggregates {
            summary.positive_observations = summary
                .positive_observations
                .saturating_add(aggregate.positive_observations);
            summary.negative_observations = summary
                .negative_observations
                .saturating_add(aggregate.negative_observations);
            summary.candidate_misses = summary
                .candidate_misses
                .saturating_add(aggregate.candidate_misses);
            summary.replay_impact_observations = summary
                .replay_impact_observations
                .saturating_add(aggregate.replay_impact.replay_observations);
            summary.attributed_replay_cost_nanos = summary
                .attributed_replay_cost_nanos
                .saturating_add(aggregate.replay_impact.total_replay_cost_nanos);
            summary.attributed_invalidated_descendants = summary
                .attributed_invalidated_descendants
                .saturating_add(aggregate.replay_impact.total_invalidated_descendants);

            match target {
                ObservationBatchTarget::Static(edge_index) => {
                    validate_static_aggregate_target(graph, edge_index, &aggregate)?;
                    let statistics = self
                        .static_statistics
                        .get_mut(edge_index.0 as usize)
                        .ok_or(FeedbackError::UnknownStaticEdge(edge_index))?;
                    statistics.apply_aggregate(epoch, config.retention_factor, &aggregate)?;
                    if aggregate.replay_impact.replay_observations != 0 {
                        let replay_cost = self
                            .static_replay_costs
                            .get_mut(edge_index.0 as usize)
                            .ok_or(FeedbackError::UnknownStaticEdge(edge_index))?;
                        replay_cost.apply_aggregate(
                            aggregate.replay_impact,
                            epoch,
                            config.retention_factor,
                        )?;
                    }
                }
                ObservationBatchTarget::Runtime(source, target) => {
                    let pair = (source, target);
                    let edge_index = if let Some(index) = self.fallback_by_pair.get(&pair).copied()
                    {
                        index
                    } else if aggregate.positive_observations == 0 {
                        return Err(FeedbackError::IndependentObservationWithoutFallback {
                            source_profile: source,
                            target_profile: target,
                        });
                    } else {
                        let edge = create_fallback_edge_from_aggregate(
                            graph,
                            &aggregate,
                            epoch,
                            config,
                            self.fallback_edges.len(),
                        )?;
                        self.fallback_edges.push(edge);
                        let index = self.fallback_edges.len() - 1;
                        self.index_fallback_edge(pair, index);
                        summary.fallback_edges_created =
                            summary.fallback_edges_created.saturating_add(1);
                        index
                    };
                    let edge = &mut self.fallback_edges[edge_index];
                    edge.conflict_kinds |= aggregate.conflict_kinds;
                    edge.statistics
                        .apply_aggregate(epoch, config.retention_factor, &aggregate)?;
                    if aggregate.replay_impact.replay_observations != 0 {
                        edge.replay_cost_statistics.apply_aggregate(
                            aggregate.replay_impact,
                            epoch,
                            config.retention_factor,
                        )?;
                    }
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
                let replay_cost_statistics = self.static_replay_costs[edge.index.0 as usize];
                let serialization_cost_statistics =
                    self.static_serialization_costs[edge.index.0 as usize];
                Ok(StaticEdgeCheckpoint {
                    source,
                    target,
                    statistics: *statistics,
                    replay_cost_statistics,
                    serialization_cost_statistics,
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
                replay_cost_statistics: edge.replay_cost_statistics,
                serialization_cost_statistics: edge.serialization_cost_statistics,
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
        if checkpoint.format_version != FEEDBACK_CHECKPOINT_VERSION
            && checkpoint.format_version != REPLAY_COST_CHECKPOINT_VERSION
            && checkpoint.format_version != LEGACY_FEEDBACK_CHECKPOINT_VERSION
        {
            return Err(FeedbackError::UnsupportedCheckpointVersion {
                actual: checkpoint.format_version,
                supported: FEEDBACK_CHECKPOINT_VERSION,
            });
        }
        let checkpoint_version = checkpoint.format_version;
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
            let replay_cost_statistics = if checkpoint_version == LEGACY_FEEDBACK_CHECKPOINT_VERSION
            {
                ReplayCostStatistics::new(saved.statistics.last_update_epoch)
            } else {
                validate_replay_cost_statistics(saved.replay_cost_statistics)?;
                saved.replay_cost_statistics
            };
            let serialization_cost_statistics = if checkpoint_version < FEEDBACK_CHECKPOINT_VERSION
            {
                SerializationCostStatistics::new(saved.statistics.last_update_epoch)
            } else {
                validate_serialization_cost_statistics(saved.serialization_cost_statistics)?;
                saved.serialization_cost_statistics
            };
            store.static_statistics[edge_index.0 as usize] = saved.statistics;
            store.static_replay_costs[edge_index.0 as usize] = replay_cost_statistics;
            store.static_serialization_costs[edge_index.0 as usize] = serialization_cost_statistics;
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
            let replay_cost_statistics = if checkpoint_version == LEGACY_FEEDBACK_CHECKPOINT_VERSION
            {
                ReplayCostStatistics::new(saved.statistics.last_update_epoch)
            } else {
                validate_replay_cost_statistics(saved.replay_cost_statistics)?;
                saved.replay_cost_statistics
            };
            let serialization_cost_statistics = if checkpoint_version < FEEDBACK_CHECKPOINT_VERSION
            {
                SerializationCostStatistics::new(saved.statistics.last_update_epoch)
            } else {
                validate_serialization_cost_statistics(saved.serialization_cost_statistics)?;
                saved.serialization_cost_statistics
            };
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
                replay_cost_statistics,
                serialization_cost_statistics,
            };
            store.fallback_edges.push(edge);
            let index = store.fallback_edges.len() - 1;
            store.index_fallback_edge(pair, index);
        }
        Ok(store)
    }

    fn index_fallback_edge(&mut self, pair: (ProfileId, ProfileId), index: usize) {
        self.fallback_by_pair.insert(pair, index);
        self.fallback_adjacency
            .entry(pair.0)
            .or_default()
            .push(index);
        if pair.0 != pair.1 {
            self.fallback_adjacency
                .entry(pair.1)
                .or_default()
                .push(index);
        }
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
        if checkpoint.format_version != FEEDBACK_CHECKPOINT_VERSION
            && checkpoint.format_version != REPLAY_COST_CHECKPOINT_VERSION
            && checkpoint.format_version != LEGACY_FEEDBACK_CHECKPOINT_VERSION
        {
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
    #[serde(default)]
    pub replay_cost_statistics: ReplayCostStatistics,
    #[serde(default)]
    pub serialization_cost_statistics: SerializationCostStatistics,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct FallbackEdgeCheckpoint {
    pub source: StableProfileKey,
    pub target: StableProfileKey,
    pub conflict_kinds: ConflictKinds,
    pub discovered_epoch: u64,
    pub review_required: bool,
    pub statistics: BetaStatistics,
    #[serde(default)]
    pub replay_cost_statistics: ReplayCostStatistics,
    #[serde(default)]
    pub serialization_cost_statistics: SerializationCostStatistics,
}

fn create_fallback_edge_from_aggregate(
    graph: &ProfileGraph,
    aggregate: &ObservationAggregate,
    epoch: u64,
    config: &AdaptiveFeedbackConfig,
    fallback_count: usize,
) -> Result<RuntimeDiscoveredEdge, FeedbackError> {
    let pair = canonical_profile_pair(aggregate.source_profile, aggregate.target_profile);
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
    if aggregate.conflict_kinds.is_empty() {
        return Err(FeedbackError::EmptyFallbackConflictKinds);
    }
    let id = RuntimeEdgeId(
        u32::try_from(fallback_count).map_err(|_| FeedbackError::TooManyFallbackEdges)?,
    );
    Ok(RuntimeDiscoveredEdge {
        id,
        source: pair.0,
        target: pair.1,
        source_key: source_key.min(target_key),
        target_key: source_key.max(target_key),
        conflict_kinds: aggregate.conflict_kinds,
        discovered_epoch: epoch,
        review_required: true,
        statistics: BetaStatistics::from_probability(
            config.fallback_prior_probability,
            config.fallback_prior_strength,
            config.epsilon,
            epoch,
        )?,
        replay_cost_statistics: ReplayCostStatistics::new(epoch),
        serialization_cost_statistics: SerializationCostStatistics::new(epoch),
    })
}

fn validate_static_aggregate_target(
    graph: &ProfileGraph,
    edge_index: ProfileEdgeIndex,
    aggregate: &ObservationAggregate,
) -> Result<(), FeedbackError> {
    let edge = graph
        .edges()
        .get(edge_index.0 as usize)
        .ok_or(FeedbackError::UnknownStaticEdge(edge_index))?;
    let expected = canonical_profile_pair(edge.source, edge.target);
    let actual = canonical_profile_pair(aggregate.source_profile, aggregate.target_profile);
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

fn validate_replay_cost_statistics(statistics: ReplayCostStatistics) -> Result<(), FeedbackError> {
    if !statistics.weighted_replay_cost_nanos.is_finite()
        || statistics.weighted_replay_cost_nanos < 0.0
        || !statistics.weighted_invalidated_descendants.is_finite()
        || statistics.weighted_invalidated_descendants < 0.0
        || !statistics.observation_weight.is_finite()
        || statistics.observation_weight < 0.0
    {
        return Err(FeedbackError::InvalidReplayCostStatistics);
    }
    Ok(())
}

fn validate_serialization_cost_statistics(
    statistics: SerializationCostStatistics,
) -> Result<(), FeedbackError> {
    if !statistics.weighted_serialization_cost_nanos.is_finite()
        || statistics.weighted_serialization_cost_nanos < 0.0
        || !statistics.observation_weight.is_finite()
        || statistics.observation_weight < 0.0
    {
        return Err(FeedbackError::InvalidSerializationCostStatistics);
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
    #[error("aggregated observation weight overflowed for weight {weight} and count {count}")]
    InvalidAggregatedWeight { weight: f64, count: usize },
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
    #[error("unknown runtime-discovered profile edge {0:?}")]
    UnknownRuntimeEdge(RuntimeEdgeId),
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
    #[error("replay cost/fan-out evidence can only be attached to a conflict observation")]
    ReplayImpactRequiresConflict,
    #[error("replay impact must be recorded one concrete observation at a time")]
    ReplayImpactRequiresSingleObservation,
    #[error(
        "replay-cost observation epoch {observation_epoch} precedes the cost model's last update epoch {last_update_epoch}"
    )]
    StaleReplayCostEpoch {
        observation_epoch: u64,
        last_update_epoch: u64,
    },
    #[error("replay-cost statistics contain non-finite or negative decayed values")]
    InvalidReplayCostStatistics,
    #[error(
        "serialization-cost observation epoch {observation_epoch} precedes the cost model's last update epoch {last_update_epoch}"
    )]
    StaleSerializationCostEpoch {
        observation_epoch: u64,
        last_update_epoch: u64,
    },
    #[error("serialization-cost statistics contain non-finite or negative decayed values")]
    InvalidSerializationCostStatistics,
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
