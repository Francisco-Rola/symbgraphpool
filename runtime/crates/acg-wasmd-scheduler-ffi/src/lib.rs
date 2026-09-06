//! C ABI bridge that keeps the Rust ACG implementation authoritative while allowing a Go Wasmd
//! executor to provide block candidates and concrete execution feedback.
//!
//! The ABI is intentionally block-granular. No Cosmos SDK, Wasmd, keeper, or KV-store pointer
//! crosses this boundary.

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet},
    panic::{catch_unwind, AssertUnwindSafe},
    ptr, slice,
    time::Instant,
};

use acg_candidate_graph::{
    AtomicCandidateTransaction, CandidateComponent, CandidateGraph, EdgeClass, EdgeProvenance,
    PreparedCandidateGraphBuilder, RiskBoundedScheduler, WeightedCandidateGraphConfig,
};
use acg_core::{
    ConflictKinds, ContractCodeHash, InstanceId, ProfileEdgeIndex, ProfileId, RuntimeId, TxId,
    TxIndex,
};
use acg_feedback::{
    AdaptiveFeedbackConfig, AdaptiveFeedbackStore, ConflictObservation, ObservationBuffer,
    ObservationSource, ObservationTarget,
};
use acg_predicate::{InputBindings, PredicateResult};
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_runtime_feedback::{AdaptivePlanningConfig, RuntimeFeedbackWeights};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const RUNTIME_NAME: &str = "cosmwasm";
const PROFILE_SCHEMA_VERSION: u16 = 1;

#[repr(C)]
pub struct AcgByteBuffer {
    pub ptr: *mut u8,
    pub len: usize,
    pub cap: usize,
}

impl Default for AcgByteBuffer {
    fn default() -> Self {
        Self {
            ptr: ptr::null_mut(),
            len: 0,
            cap: 0,
        }
    }
}

#[derive(Debug, Deserialize)]
struct BridgeConfig {
    documents: Vec<SymbolicDocument>,
    #[serde(default)]
    planning: PlanningOverrides,
    #[serde(default)]
    dependency_diagnostics: bool,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct PlanningOverrides {
    edge_materialization_threshold: Option<f64>,
    soft_threshold: Option<f64>,
    hard_threshold: Option<f64>,
    risk_budget: Option<f64>,
    exploration_rate: Option<f64>,
    exploration_risk_budget: Option<f64>,
    exploration_min_uncertainty: Option<f64>,
    exploration_max_transactions_per_block: Option<usize>,
    independent_observations_before_softening: Option<u32>,
    softening_min_confidence: Option<f64>,
}

impl PlanningOverrides {
    fn apply(self, config: &mut AdaptivePlanningConfig) {
        if let Some(value) = self.edge_materialization_threshold {
            config.edge_materialization_threshold = value;
        }
        if let Some(value) = self.soft_threshold {
            config.scheduler.soft_threshold = value;
        }
        if let Some(value) = self.hard_threshold {
            config.scheduler.hard_threshold = value;
        }
        if let Some(value) = self.risk_budget {
            config.scheduler.risk_budget = value;
        }
        if let Some(value) = self.exploration_rate {
            config.scheduler.exploration_rate = value;
        }
        if let Some(value) = self.exploration_risk_budget {
            config.scheduler.exploration_risk_budget = value;
        }
        if let Some(value) = self.exploration_min_uncertainty {
            config.scheduler.exploration_min_uncertainty = value;
        }
        if let Some(value) = self.exploration_max_transactions_per_block {
            config.scheduler.exploration_max_transactions_per_block = value;
        }
        if let Some(value) = self.independent_observations_before_softening {
            config.scheduler.independent_observations_before_softening = value;
        }
        if let Some(value) = self.softening_min_confidence {
            config.scheduler.softening_min_confidence = value;
        }
    }
}

#[derive(Debug, Deserialize)]
struct SymbolicDocument {
    family: String,
    document: Value,
}

#[derive(Clone, Debug, Deserialize)]
struct PlanComponent {
    family: String,
    instance: String,
    entrypoint: String,
    bindings: Value,
    estimated_execution_cost: u32,
}

#[derive(Clone, Debug, Deserialize)]
struct PlanTransaction {
    tx_id: u64,
    components: Vec<PlanComponent>,
    #[serde(default)]
    hard_resources: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PlanRequest {
    epoch: u64,
    transactions: Vec<PlanTransaction>,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum DependencyClass {
    Soft,
    Hard,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct ParentDependency {
    predecessor: usize,
    successor: usize,
    class: DependencyClass,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
struct ParentPair {
    left: usize,
    right: usize,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum DependencyReason {
    SymbolicHard,
    AdaptiveHard,
    SoftRisk,
    BankResource,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum DependencyProvenance {
    StaticPredicate,
    StaticProfile,
    RuntimeDiscovered,
    BankResource,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "snake_case")]
enum DependencyDecision {
    Hard,
    SoftSerialized,
    BankHard,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
struct ParentDependencyReasons {
    predecessor: usize,
    successor: usize,
    reasons: Vec<DependencyReason>,
    provenance: Vec<DependencyProvenance>,
    decisions: Vec<DependencyDecision>,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
struct PlanningTimings {
    rust_decode_nanos: u64,
    resolve_components_nanos: u64,
    candidate_graph_nanos: u64,
    scheduler_nanos: u64,
    projection_nanos: u64,
    feedback_pairs_nanos: u64,
    finalize_nanos: u64,
}

#[derive(Clone, Copy, Debug, Default, Serialize)]
struct CandidateDecisionCounts {
    hard: usize,
    soft: usize,
    low: usize,
    ordered_hard: usize,
    ordered_soft: usize,
}

#[derive(Clone, Copy, Debug, Serialize)]
struct PlanningSnapshot {
    edge_materialization_threshold: f64,
    soft_threshold: f64,
    hard_threshold: f64,
    risk_budget: f64,
    exploration_rate: f64,
    exploration_risk_budget: f64,
    exploration_min_uncertainty: f64,
    exploration_max_transactions_per_block: usize,
    independent_observations_before_softening: u32,
    softening_min_confidence: f64,
}

impl From<AdaptivePlanningConfig> for PlanningSnapshot {
    fn from(config: AdaptivePlanningConfig) -> Self {
        Self {
            edge_materialization_threshold: config.edge_materialization_threshold,
            soft_threshold: config.scheduler.soft_threshold,
            hard_threshold: config.scheduler.hard_threshold,
            risk_budget: config.scheduler.risk_budget,
            exploration_rate: config.scheduler.exploration_rate,
            exploration_risk_budget: config.scheduler.exploration_risk_budget,
            exploration_min_uncertainty: config.scheduler.exploration_min_uncertainty,
            exploration_max_transactions_per_block: config
                .scheduler
                .exploration_max_transactions_per_block,
            independent_observations_before_softening: config
                .scheduler
                .independent_observations_before_softening,
            softening_min_confidence: config.scheduler.softening_min_confidence,
        }
    }
}

#[derive(Debug, Serialize)]
struct PlanResponse {
    transaction_count: usize,
    component_count: usize,
    candidate_edges: usize,
    logical_candidate_edges: usize,
    compact_candidate_groups: usize,
    pre_reduction_dependencies: usize,
    parent_dependencies_before_reduction: usize,
    parent_dependencies_elided_by_reduction: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    planning: Option<PlanningSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    planning_timings: Option<PlanningTimings>,
    candidate_decisions: CandidateDecisionCounts,
    dependencies: Vec<ParentDependency>,
    dependency_reasons: Vec<ParentDependencyReasons>,
    feedback_pairs: Vec<ParentPair>,
    levels: Vec<Vec<usize>>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum FeedbackSource {
    PreExecution,
    Replay,
}

#[derive(Clone, Debug, Deserialize)]
struct PairObservation {
    left: usize,
    right: usize,
    conflict_kinds: u8,
    conflict: bool,
    source: FeedbackSource,
}

#[derive(Clone, Debug, Deserialize)]
struct ReplayObservation {
    predecessor: usize,
    transaction: usize,
    conflict_kinds: u8,
    replay_cost_nanos: u64,
    invalidated_descendants: u32,
}

#[derive(Clone, Debug, Deserialize)]
struct SerializationObservation {
    predecessor: usize,
    transaction: usize,
    marginal_ready_delay_nanos: u64,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
struct EconomicsObservation {
    serial_service_nanos: u64,
    pre_consensus_nanos: u64,
    post_consensus_nanos: u64,
    transaction_count: usize,
}

#[derive(Debug, Deserialize)]
struct FeedbackRequest {
    epoch: u64,
    #[serde(default)]
    observations: Vec<PairObservation>,
    #[serde(default)]
    replay_attributions: Vec<ReplayObservation>,
    #[serde(default)]
    serialization: Vec<SerializationObservation>,
    #[serde(default)]
    economics: EconomicsObservation,
}

#[derive(Debug, Default, Serialize)]
struct FeedbackResponse {
    applied_observations: usize,
    positive_observations: usize,
    negative_observations: usize,
    unattributed_runtime_conflicts: usize,
    replay_attributions_applied: usize,
    serialization_observations_applied: usize,
    regime_evidence_decayed: bool,
}

#[derive(Debug)]
struct LastPlan {
    graph: CandidateGraph,
    parent_profiles: Vec<Vec<ProfileId>>,
    candidate_provenances: BTreeMap<(TxIndex, TxIndex), BTreeSet<EdgeProvenance>>,
}

#[derive(Clone, Copy, Debug, Default)]
struct RecentEconomics {
    mean_service_nanos: f64,
    conflict_rate: f64,
    has_conflict_rate: bool,
}

struct SchedulerState {
    profile_graph: ProfileGraph,
    prepared_candidate_builder: PreparedCandidateGraphBuilder,
    profile_lookup: BTreeMap<String, BTreeMap<String, ProfileId>>,
    instances: BTreeMap<String, InstanceId>,
    next_instance: u32,
    feedback: AdaptiveFeedbackStore,
    feedback_config: AdaptiveFeedbackConfig,
    planning_config: AdaptivePlanningConfig,
    feedback_weights: RuntimeFeedbackWeights,
    recent_economics: Option<RecentEconomics>,
    dependency_diagnostics: bool,
    last_plan: Option<LastPlan>,
}

#[repr(C)]
pub struct AcgWasmdScheduler {
    state: SchedulerState,
}

impl SchedulerState {
    fn new(config: BridgeConfig) -> Result<Self, String> {
        let BridgeConfig {
            documents,
            planning,
            dependency_diagnostics,
        } = config;
        if documents.is_empty() {
            return Err("bridge configuration contains no symbolic documents".to_owned());
        }
        let runtime_id = RuntimeId::new(RUNTIME_NAME).map_err(|error| error.to_string())?;
        let mut all_profiles = Vec::new();

        for item in documents {
            let bytes = serde_json::to_vec(&item.document).map_err(|error| error.to_string())?;
            let raw = parse_slice(&bytes).map_err(|error| format!("{}: {error}", item.family))?;
            if raw.contract != item.family {
                return Err(format!(
                    "symbolic family mismatch: config={:?} document={:?}",
                    item.family, raw.contract
                ));
            }
            let code_hash = family_code_hash(&item.family);
            let context =
                IngestionContext::new(runtime_id.clone(), code_hash, PROFILE_SCHEMA_VERSION);
            let profiles = normalize_document(raw, &context)
                .map_err(|error| format!("{}: {error}", item.family))?;
            all_profiles.extend(profiles);
        }

        let artifact = ProfileGraphArtifact::compile(all_profiles, &EdgeBuildConfig::default())
            .map_err(|error| error.to_string())?;
        let profile_graph = ProfileGraph::load(artifact, GraphLoadConfig::default())
            .map_err(|error| error.to_string())?;

        // Symbolic documents are an offline artifact. Resolve their stable profile identities once
        // and compile every predicate once; per-block planning should instantiate only the
        // concrete transactions that reference this immutable topology.
        let mut profile_lookup = BTreeMap::<String, BTreeMap<String, ProfileId>>::new();
        for profile in profile_graph.profiles() {
            let family = profile.definition.contract_name.clone();
            let entrypoint = profile.definition.entrypoint_name.clone();
            if profile_lookup
                .entry(family.clone())
                .or_default()
                .insert(entrypoint.clone(), profile.id)
                .is_some()
            {
                return Err(format!(
                    "duplicate Rust symbolic profile for {family:?} {entrypoint:?}"
                ));
            }
        }
        let prepared_candidate_builder = PreparedCandidateGraphBuilder::new(&profile_graph);
        let feedback = AdaptiveFeedbackStore::from_graph(&profile_graph, 0)
            .map_err(|error| error.to_string())?;
        let mut planning_config = AdaptivePlanningConfig::default();
        planning.apply(&mut planning_config);
        planning_config
            .validate()
            .map_err(|error| error.to_string())?;

        Ok(Self {
            profile_graph,
            prepared_candidate_builder,
            profile_lookup,
            instances: BTreeMap::new(),
            next_instance: 0,
            feedback,
            feedback_config: AdaptiveFeedbackConfig::default(),
            planning_config,
            feedback_weights: RuntimeFeedbackWeights::default(),
            recent_economics: None,
            dependency_diagnostics,
            last_plan: None,
        })
    }

    fn resolve_profile(&self, family: &str, entrypoint: &str) -> Result<ProfileId, String> {
        self.profile_lookup
            .get(family)
            .and_then(|entrypoints| entrypoints.get(entrypoint))
            .copied()
            .ok_or_else(|| format!("no Rust symbolic profile for {family:?} {entrypoint:?}"))
    }

    fn resolve_instance(&mut self, instance: &str) -> Result<InstanceId, String> {
        if let Some(id) = self.instances.get(instance) {
            return Ok(*id);
        }
        let id = InstanceId(self.next_instance);
        self.next_instance = self
            .next_instance
            .checked_add(1)
            .ok_or_else(|| "validator-local InstanceId space exhausted".to_owned())?;
        self.instances.insert(instance.to_owned(), id);
        Ok(id)
    }

    #[cfg(test)]
    fn plan(&mut self, request: PlanRequest) -> Result<PlanResponse, String> {
        self.plan_with_decode(request, 0)
    }

    fn plan_with_decode(
        &mut self,
        request: PlanRequest,
        rust_decode_nanos: u64,
    ) -> Result<PlanResponse, String> {
        let parent_count = request.transactions.len();
        let mut timings = PlanningTimings {
            rust_decode_nanos,
            ..PlanningTimings::default()
        };

        let started = Instant::now();
        let component_count = request
            .transactions
            .iter()
            .map(|transaction| transaction.components.len())
            .sum::<usize>();
        let mut atomic_transactions = Vec::with_capacity(parent_count);
        let mut parent_profiles = vec![Vec::<ProfileId>::new(); parent_count];
        let mut profile_parents = vec![Vec::<usize>::new(); self.profile_graph.profiles().len()];
        for (parent, transaction) in request.transactions.iter().enumerate() {
            let mut components = Vec::with_capacity(transaction.components.len());
            let mut estimated_execution_cost = 0_u32;
            for component in &transaction.components {
                let profile_id = self.resolve_profile(&component.family, &component.entrypoint)?;
                let instance_id = self.resolve_instance(&component.instance)?;
                components.push(CandidateComponent {
                    profile_id,
                    instance_id,
                    input_bindings: InputBindings::from_value(component.bindings.clone()),
                });
                estimated_execution_cost =
                    estimated_execution_cost.saturating_add(component.estimated_execution_cost);
                parent_profiles[parent].push(profile_id);
                profile_parents[profile_id.0 as usize].push(parent);
            }
            parent_profiles[parent].sort_unstable();
            parent_profiles[parent].dedup();
            atomic_transactions.push(AtomicCandidateTransaction {
                tx_id: TxId(transaction.tx_id),
                predicted_position: u32::try_from(parent)
                    .map_err(|_| "too many atomic block transactions for TxIndex".to_owned())?,
                inclusion_probability: 1.0,
                estimated_execution_cost,
                components,
            });
        }
        for parents in &mut profile_parents {
            parents.sort_unstable();
            parents.dedup();
        }
        timings.resolve_components_nanos = nanos_u64(started.elapsed());

        let started = Instant::now();
        let atomic_build = self
            .prepared_candidate_builder
            .build_weighted_atomic(
                &self.profile_graph,
                atomic_transactions,
                &self.feedback,
                &self.feedback_config,
                WeightedCandidateGraphConfig {
                    epoch: request.epoch,
                    edge_materialization_threshold: self
                        .planning_config
                        .edge_materialization_threshold,
                    cost_policy: self.planning_config.cost_policy,
                    compact_immature_equivalence_edges: self
                        .planning_config
                        .compact_equivalence_groups,
                    independent_observations_before_softening: self
                        .planning_config
                        .scheduler
                        .independent_observations_before_softening,
                },
            )
            .map_err(|error| error.to_string())?;
        let logical_component_edges = atomic_build.logical_component_edges();
        let candidate_provenances = atomic_build.candidate_provenance_map().clone();
        let graph = atomic_build.into_graph();
        timings.candidate_graph_nanos = nanos_u64(started.elapsed());

        let started = Instant::now();
        let scheduler = RiskBoundedScheduler::new(self.planning_config.scheduler)
            .map_err(|error| error.to_string())?;
        // `schedule()` constructs the full SchedulingAnalysis and retains a debug assertion
        // against the candidate graph. Re-validating here rebuilt that analysis a second time on
        // every production block, doubling classification/reduction work for diagnostics that are
        // already covered by the core scheduler tests.
        let schedule = scheduler
            .schedule(&graph)
            .map_err(|error| error.to_string())?;
        timings.scheduler_nanos = nanos_u64(started.elapsed());

        let mut candidate_decisions = CandidateDecisionCounts::default();
        if self.dependency_diagnostics {
            for edge in graph.edges() {
                if graph.pair_is_parallel(edge.source, edge.target) {
                    continue;
                }
                match self.planning_config.scheduler.classify(edge) {
                    EdgeClass::Hard => candidate_decisions.hard += 1,
                    EdgeClass::Soft => candidate_decisions.soft += 1,
                    EdgeClass::Low => candidate_decisions.low += 1,
                }
            }
            for group in graph.parallel_groups() {
                for evidence in group.evidences() {
                    if graph.provenance_is_compact(evidence.provenance) {
                        continue;
                    }
                    match self.planning_config.scheduler.classify(evidence) {
                        EdgeClass::Hard => candidate_decisions.hard += 1,
                        EdgeClass::Soft => candidate_decisions.soft += 1,
                        EdgeClass::Low => candidate_decisions.low += 1,
                    }
                }
            }
            for group in graph.compact_groups() {
                let count = group
                    .logical_edges()
                    .saturating_sub(group.members().len().saturating_sub(1));
                match self
                    .planning_config
                    .scheduler
                    .classify(group.edge_template())
                {
                    EdgeClass::Hard => candidate_decisions.hard += count,
                    EdgeClass::Soft => candidate_decisions.soft += count,
                    EdgeClass::Low => candidate_decisions.low += count,
                }
            }
            for dependency in &schedule.ordering_dependencies {
                match dependency.class {
                    EdgeClass::Hard => candidate_decisions.ordered_hard += 1,
                    EdgeClass::Soft | EdgeClass::Low => candidate_decisions.ordered_soft += 1,
                }
            }
        }

        let started = Instant::now();
        let mut dependencies = BTreeMap::<(usize, usize), DependencyClass>::new();
        let mut dependency_diagnostics = DependencyDiagnostics::default();
        for dependency in &schedule.ordering_dependencies {
            let left = dependency.predecessor.0 as usize;
            let right = dependency.successor.0 as usize;
            if self.dependency_diagnostics {
                let (reason, provenance, decision) = scheduled_dependency_diagnostics(
                    &graph,
                    &self.planning_config.scheduler,
                    *dependency,
                );
                add_parent_dependency_with_diagnostics(
                    &mut dependencies,
                    &mut dependency_diagnostics,
                    left,
                    right,
                    edge_class_to_dependency(dependency.class),
                    DependencyDiagnostic {
                        reason,
                        provenance,
                        decision,
                    },
                );
            } else {
                add_parent_dependency(
                    &mut dependencies,
                    left,
                    right,
                    edge_class_to_dependency(dependency.class),
                );
            }
        }

        // The candidate graph is already atomic, so there is no component->parent projection or
        // hard-edge restoration here. Profile-level relationships were aggregated before
        // scheduling inside acg-candidate-graph; only adapter-level resources remain to merge.

        // Bank/funds state is intentionally outside the contract symbolic profile graph. The Go
        // adapter supplies exact logical resources and Rust adds deterministic hard chains.
        let mut resource_users = BTreeMap::<&str, Vec<usize>>::new();
        for (parent, transaction) in request.transactions.iter().enumerate() {
            for resource in &transaction.hard_resources {
                resource_users
                    .entry(resource.as_str())
                    .or_default()
                    .push(parent);
            }
        }
        for users in resource_users.values_mut() {
            users.sort_unstable();
            users.dedup();
            for pair in users.windows(2) {
                if self.dependency_diagnostics {
                    add_parent_dependency_with_diagnostics(
                        &mut dependencies,
                        &mut dependency_diagnostics,
                        pair[0],
                        pair[1],
                        DependencyClass::Hard,
                        DependencyDiagnostic {
                            reason: DependencyReason::BankResource,
                            provenance: DependencyProvenance::BankResource,
                            decision: DependencyDecision::BankHard,
                        },
                    );
                } else {
                    add_parent_dependency(
                        &mut dependencies,
                        pair[0],
                        pair[1],
                        DependencyClass::Hard,
                    );
                }
            }
        }

        // RiskBoundedScheduler already transitively reduces the atomic ACG ordering DAG. Exact
        // adapter-level bank/funds resources can reintroduce redundant paths, so reduce once more
        // after merging those resources and send Go the minimal equivalent execution relation.
        let parent_dependencies_before_reduction = dependencies.len();
        dependencies = transitive_reduce_parent_dependencies(parent_count, dependencies);
        let parent_dependencies_elided_by_reduction =
            parent_dependencies_before_reduction.saturating_sub(dependencies.len());
        if self.dependency_diagnostics {
            dependency_diagnostics.retain_dependencies(&dependencies);
        }

        let dependency_reasons = dependency_diagnostics.into_parent_reasons();
        let dependencies = dependencies
            .into_iter()
            .map(|((predecessor, successor), class)| ParentDependency {
                predecessor,
                successor,
                class,
            })
            .collect::<Vec<_>>();
        timings.projection_nanos = nanos_u64(started.elapsed());

        // Go only needs concrete access comparisons for parent transaction pairs represented in
        // the persistent relationship model. Use profile adjacency directly instead of rediscovering
        // relationships with an O(parent^2 * components^2) scan after every block plan. Persisted
        // runtime-discovered fallback topology participates exactly like static profile adjacency.
        let started = Instant::now();
        let mut feedback_pairs = BTreeSet::<(usize, usize)>::new();
        for edge in self.profile_graph.edges() {
            insert_profile_parent_pairs(
                &mut feedback_pairs,
                &profile_parents,
                edge.source,
                edge.target,
            );
        }
        for fallback in self.feedback.fallback_edges() {
            insert_profile_parent_pairs(
                &mut feedback_pairs,
                &profile_parents,
                fallback.source,
                fallback.target,
            );
        }
        let feedback_pairs = feedback_pairs
            .into_iter()
            .map(|(left, right)| ParentPair { left, right })
            .collect::<Vec<_>>();
        timings.feedback_pairs_nanos = nanos_u64(started.elapsed());

        let started = Instant::now();
        let levels = parent_levels(parent_count, &dependencies)?;
        timings.finalize_nanos = nanos_u64(started.elapsed());
        let response = PlanResponse {
            transaction_count: parent_count,
            component_count,
            candidate_edges: graph.physical_edge_count(),
            logical_candidate_edges: logical_component_edges,
            compact_candidate_groups: graph.compact_groups().len(),
            pre_reduction_dependencies: schedule.pre_reduction_ordering_dependencies,
            parent_dependencies_before_reduction,
            parent_dependencies_elided_by_reduction,
            planning: self
                .dependency_diagnostics
                .then(|| PlanningSnapshot::from(self.planning_config)),
            planning_timings: self.dependency_diagnostics.then_some(timings),
            candidate_decisions,
            dependencies,
            dependency_reasons,
            feedback_pairs,
            levels,
        };
        self.last_plan = Some(LastPlan {
            graph,
            parent_profiles,
            candidate_provenances,
        });
        Ok(response)
    }

    fn feedback(&mut self, request: FeedbackRequest) -> Result<FeedbackResponse, String> {
        let last = self
            .last_plan
            .as_ref()
            .ok_or_else(|| "feedback received before any block plan".to_owned())?;
        let mut response = FeedbackResponse::default();
        let mut buffer = ObservationBuffer::default();

        for observation in &request.observations {
            if observation.left >= last.parent_profiles.len()
                || observation.right >= last.parent_profiles.len()
                || observation.left == observation.right
            {
                return Err(format!(
                    "invalid parent observation pair ({}, {})",
                    observation.left, observation.right
                ));
            }
            let static_edges = parent_static_edges(
                &self.profile_graph,
                last,
                observation.left,
                observation.right,
            );
            if static_edges.is_empty() {
                if observation.conflict {
                    response.unattributed_runtime_conflicts =
                        response.unattributed_runtime_conflicts.saturating_add(1);
                }
                continue;
            }
            let weight = match observation.source {
                FeedbackSource::PreExecution => self.feedback_weights.pre_execution_conflict,
                FeedbackSource::Replay => self.feedback_weights.replay_conflict,
            };
            let independent_weight = match observation.source {
                FeedbackSource::PreExecution => self.feedback_weights.pre_execution_independent,
                FeedbackSource::Replay => self.feedback_weights.replay_independent,
            };
            for edge_index in static_edges {
                let edge = &self.profile_graph.edges()[edge_index.0 as usize];
                let candidate_present = parent_has_static_candidate(
                    last,
                    observation.left,
                    observation.right,
                    edge_index,
                );
                // Positive observations from non-candidate pairs are valuable: they
                // expose candidate misses. Absence from the candidate graph, however,
                // is not negative evidence for a static relationship. Counting every
                // profile-adjacent non-candidate pair as independent dilutes sparse,
                // key-specific conflicts (for example MiniWarehouse NewOrder pairs).
                if !observation.conflict && !candidate_present {
                    continue;
                }
                let item = if observation.conflict {
                    let kinds =
                        ConflictKinds::from_bits(observation.conflict_kinds).ok_or_else(|| {
                            format!("invalid conflict kind bits {}", observation.conflict_kinds)
                        })?;
                    ConflictObservation::conflict(
                        edge.source,
                        edge.target,
                        TxId(observation.left as u64),
                        TxId(observation.right as u64),
                        kinds,
                        feedback_source(observation.source),
                        ObservationTarget::Static { edge_index },
                        weight,
                        request.epoch,
                        candidate_present,
                    )
                    .map_err(|error| error.to_string())?
                } else {
                    ConflictObservation::independent(
                        edge.source,
                        edge.target,
                        TxId(observation.left as u64),
                        TxId(observation.right as u64),
                        feedback_source(observation.source),
                        ObservationTarget::Static { edge_index },
                        independent_weight,
                        request.epoch,
                        candidate_present,
                    )
                    .map_err(|error| error.to_string())?
                };
                buffer.push(item);
                response.applied_observations = response.applied_observations.saturating_add(1);
                if observation.conflict {
                    response.positive_observations =
                        response.positive_observations.saturating_add(1);
                } else {
                    response.negative_observations =
                        response.negative_observations.saturating_add(1);
                }
            }
        }

        for replay in &request.replay_attributions {
            if replay.predecessor >= last.parent_profiles.len()
                || replay.transaction >= last.parent_profiles.len()
                || replay.predecessor == replay.transaction
            {
                return Err(format!(
                    "invalid replay attribution pair ({}, {})",
                    replay.predecessor, replay.transaction
                ));
            }
            let static_edges = parent_static_edges(
                &self.profile_graph,
                last,
                replay.predecessor,
                replay.transaction,
            );
            if static_edges.is_empty() {
                response.unattributed_runtime_conflicts =
                    response.unattributed_runtime_conflicts.saturating_add(1);
                continue;
            }
            let edge_count = u64::try_from(static_edges.len()).unwrap_or(u64::MAX).max(1);
            let base_cost = replay.replay_cost_nanos / edge_count;
            let remainder = replay.replay_cost_nanos % edge_count;
            for (offset, edge_index) in static_edges.into_iter().enumerate() {
                let edge = &self.profile_graph.edges()[edge_index.0 as usize];
                let kinds = ConflictKinds::from_bits(replay.conflict_kinds).ok_or_else(|| {
                    format!(
                        "invalid replay conflict kind bits {}",
                        replay.conflict_kinds
                    )
                })?;
                let extra = if (offset as u64) < remainder { 1 } else { 0 };
                let item = ConflictObservation::conflict(
                    edge.source,
                    edge.target,
                    TxId(replay.predecessor as u64),
                    TxId(replay.transaction as u64),
                    kinds,
                    ObservationSource::Replay,
                    ObservationTarget::Static { edge_index },
                    self.feedback_weights.replay_conflict,
                    request.epoch,
                    parent_has_static_candidate(
                        last,
                        replay.predecessor,
                        replay.transaction,
                        edge_index,
                    ),
                )
                .map_err(|error| error.to_string())?
                .with_replay_impact(
                    base_cost.saturating_add(extra),
                    replay.invalidated_descendants,
                );
                buffer.push(item);
                response.applied_observations = response.applied_observations.saturating_add(1);
                response.positive_observations = response.positive_observations.saturating_add(1);
                response.replay_attributions_applied =
                    response.replay_attributions_applied.saturating_add(1);
            }
        }

        if !buffer.is_empty() {
            self.feedback
                .apply_batch(&self.profile_graph, buffer, &self.feedback_config)
                .map_err(|error| error.to_string())?;
        }

        for serialization in &request.serialization {
            let provenances = parent_candidate_provenances(
                last,
                serialization.predecessor,
                serialization.transaction,
            );
            if provenances.is_empty() {
                continue;
            }
            let count = u64::try_from(provenances.len()).unwrap_or(u64::MAX).max(1);
            let base = serialization.marginal_ready_delay_nanos / count;
            let remainder = serialization.marginal_ready_delay_nanos % count;
            for (offset, provenance) in provenances.into_iter().enumerate() {
                let value = base.saturating_add(if (offset as u64) < remainder { 1 } else { 0 });
                match provenance {
                    EdgeProvenance::Static { profile_edge_index } => {
                        self.feedback
                            .record_static_serialization_cost(
                                profile_edge_index,
                                value,
                                1.0,
                                request.epoch,
                                &self.feedback_config,
                            )
                            .map_err(|error| error.to_string())?;
                    }
                    EdgeProvenance::RuntimeDiscovered { runtime_edge_id } => {
                        self.feedback
                            .record_fallback_serialization_cost(
                                runtime_edge_id,
                                value,
                                1.0,
                                request.epoch,
                                &self.feedback_config,
                            )
                            .map_err(|error| error.to_string())?;
                    }
                }
                response.serialization_observations_applied = response
                    .serialization_observations_applied
                    .saturating_add(1);
            }
        }

        response.regime_evidence_decayed = self.observe_economics(
            request.epoch,
            request.economics,
            response.positive_observations,
            response.negative_observations,
        )?;
        Ok(response)
    }

    fn observe_economics(
        &mut self,
        epoch: u64,
        observation: EconomicsObservation,
        positives: usize,
        negatives: usize,
    ) -> Result<bool, String> {
        if observation.transaction_count == 0 || observation.serial_service_nanos == 0 {
            return Ok(false);
        }
        let transaction_count = observation.transaction_count as f64;
        let service = observation.serial_service_nanos as f64 / transaction_count;
        let total = positives.saturating_add(negatives);
        let current_conflict_rate = if total == 0 {
            None
        } else {
            Some(positives as f64 / total as f64)
        };
        let regime = self.planning_config.regime_change;
        let alpha = self.planning_config.serial_bypass.economics_ema_alpha;
        let previous = self.recent_economics;
        let service_drop = regime.enabled
            && previous.is_some_and(|previous| {
                service < previous.mean_service_nanos * regime.service_cost_drop_ratio
            });
        let contention_increase = regime.enabled
            && previous
                .filter(|previous| previous.has_conflict_rate)
                .zip(current_conflict_rate)
                .is_some_and(|(previous, current)| {
                    current >= previous.conflict_rate + regime.contention_increase_absolute
                        && current
                            >= previous.conflict_rate.max(f64::MIN_POSITIVE)
                                * regime.contention_increase_ratio
                });
        let decayed = service_drop || contention_increase;
        if decayed {
            self.feedback
                .decay_for_regime_change(epoch, regime.retained_evidence, &self.feedback_config)
                .map_err(|error| error.to_string())?;
        }

        let conflict_rate = match (previous, current_conflict_rate) {
            (Some(previous), Some(current))
                if !contention_increase && previous.has_conflict_rate =>
            {
                alpha * current + (1.0 - alpha) * previous.conflict_rate
            }
            (_, Some(current)) => current,
            (Some(previous), None) => previous.conflict_rate,
            (None, None) => 0.0,
        };
        let has_conflict_rate = current_conflict_rate.is_some()
            || previous.is_some_and(|previous| previous.has_conflict_rate);
        let mean_service_nanos = match previous {
            Some(previous) if !service_drop => {
                alpha * service + (1.0 - alpha) * previous.mean_service_nanos
            }
            _ => service,
        };
        self.recent_economics = Some(RecentEconomics {
            mean_service_nanos,
            conflict_rate,
            has_conflict_rate,
        });
        let _phase_speedup_denominator = observation
            .pre_consensus_nanos
            .max(observation.post_consensus_nanos)
            .max(1);
        Ok(decayed)
    }
}

fn family_code_hash(family: &str) -> ContractCodeHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"acg.wasmd-family.v1\0");
    hasher.update(family.as_bytes());
    ContractCodeHash(*hasher.finalize().as_bytes())
}

fn feedback_source(source: FeedbackSource) -> ObservationSource {
    match source {
        FeedbackSource::PreExecution => ObservationSource::PreExecution,
        FeedbackSource::Replay => ObservationSource::Replay,
    }
}

fn edge_class_to_dependency(class: EdgeClass) -> DependencyClass {
    match class {
        EdgeClass::Hard => DependencyClass::Hard,
        EdgeClass::Soft | EdgeClass::Low => DependencyClass::Soft,
    }
}

fn nanos_u64(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn edge_dependency_provenance(
    edge: &acg_candidate_graph::TransactionEdge,
    class: EdgeClass,
) -> (DependencyReason, DependencyProvenance) {
    let reason = if class == EdgeClass::Soft {
        DependencyReason::SoftRisk
    } else {
        match edge.provenance {
            EdgeProvenance::Static { .. } if edge.predicate_result == PredicateResult::True => {
                DependencyReason::SymbolicHard
            }
            EdgeProvenance::Static { .. } | EdgeProvenance::RuntimeDiscovered { .. } => {
                DependencyReason::AdaptiveHard
            }
        }
    };
    let provenance = match edge.provenance {
        EdgeProvenance::Static { .. } if edge.predicate_result == PredicateResult::True => {
            DependencyProvenance::StaticPredicate
        }
        EdgeProvenance::Static { .. } => DependencyProvenance::StaticProfile,
        EdgeProvenance::RuntimeDiscovered { .. } => DependencyProvenance::RuntimeDiscovered,
    };
    (reason, provenance)
}

fn scheduled_dependency_diagnostics(
    graph: &CandidateGraph,
    config: &acg_candidate_graph::RiskBoundedSchedulerConfig,
    dependency: acg_candidate_graph::ScheduledDependency,
) -> (DependencyReason, DependencyProvenance, DependencyDecision) {
    let decision = if dependency.class == EdgeClass::Hard {
        DependencyDecision::Hard
    } else {
        DependencyDecision::SoftSerialized
    };
    if let Some(edge) = graph.edges().iter().find(|edge| {
        !graph.pair_is_parallel(edge.source, edge.target)
            && edge.source == dependency.predecessor
            && edge.target == dependency.successor
            && config.classify(edge) == dependency.class
    }) {
        let (reason, provenance) = edge_dependency_provenance(edge, dependency.class);
        return (reason, provenance, decision);
    }
    if let Some(group) = graph.parallel_groups().iter().find(|group| {
        let pair = canonical_candidate_pair(group.source(), group.target());
        pair == canonical_candidate_pair(dependency.predecessor, dependency.successor)
    }) {
        let evidence = if dependency.class == EdgeClass::Hard {
            group
                .evidences()
                .iter()
                .find(|edge| config.classify(edge) == EdgeClass::Hard)
        } else {
            group
                .evidences()
                .iter()
                .find(|edge| config.classify(edge) == EdgeClass::Soft)
        };
        if let Some(edge) = evidence {
            let (reason, provenance) = edge_dependency_provenance(edge, dependency.class);
            return (reason, provenance, decision);
        }
    }
    if let Some(group) = graph.compact_groups().iter().find(|group| {
        group.members().contains(&dependency.predecessor)
            && group.members().contains(&dependency.successor)
            && config.classify(group.edge_template()) == dependency.class
    }) {
        let (reason, provenance) =
            edge_dependency_provenance(group.edge_template(), dependency.class);
        return (reason, provenance, decision);
    }
    (
        if dependency.class == EdgeClass::Soft {
            DependencyReason::SoftRisk
        } else {
            DependencyReason::AdaptiveHard
        },
        DependencyProvenance::RuntimeDiscovered,
        decision,
    )
}

fn canonical_candidate_pair(left: TxIndex, right: TxIndex) -> (TxIndex, TxIndex) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn canonical_parent_pair(left: usize, right: usize) -> Option<(usize, usize)> {
    match left.cmp(&right) {
        Ordering::Less => Some((left, right)),
        Ordering::Equal => None,
        Ordering::Greater => Some((right, left)),
    }
}

fn insert_profile_parent_pairs(
    output: &mut BTreeSet<(usize, usize)>,
    profile_parents: &[Vec<usize>],
    source: ProfileId,
    target: ProfileId,
) {
    let Some(source_parents) = profile_parents.get(source.0 as usize) else {
        return;
    };
    let Some(target_parents) = profile_parents.get(target.0 as usize) else {
        return;
    };
    if source == target {
        for (offset, &left) in source_parents.iter().enumerate() {
            for &right in source_parents.iter().skip(offset + 1) {
                if let Some(pair) = canonical_parent_pair(left, right) {
                    output.insert(pair);
                }
            }
        }
    } else {
        for &left in source_parents {
            for &right in target_parents {
                if let Some(pair) = canonical_parent_pair(left, right) {
                    output.insert(pair);
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct DependencyDiagnostic {
    reason: DependencyReason,
    provenance: DependencyProvenance,
    decision: DependencyDecision,
}

#[derive(Default)]
struct DependencyDiagnostics {
    reasons: BTreeMap<(usize, usize), BTreeSet<DependencyReason>>,
    provenance: BTreeMap<(usize, usize), BTreeSet<DependencyProvenance>>,
    decisions: BTreeMap<(usize, usize), BTreeSet<DependencyDecision>>,
}

impl DependencyDiagnostics {
    fn add(&mut self, left: usize, right: usize, diagnostic: DependencyDiagnostic) {
        let Some(pair) = canonical_parent_pair(left, right) else {
            return;
        };
        self.reasons
            .entry(pair)
            .or_default()
            .insert(diagnostic.reason);
        self.provenance
            .entry(pair)
            .or_default()
            .insert(diagnostic.provenance);
        self.decisions
            .entry(pair)
            .or_default()
            .insert(diagnostic.decision);
    }

    fn retain_dependencies(&mut self, dependencies: &BTreeMap<(usize, usize), DependencyClass>) {
        self.reasons
            .retain(|pair, _| dependencies.contains_key(pair));
        self.provenance
            .retain(|pair, _| dependencies.contains_key(pair));
        self.decisions
            .retain(|pair, _| dependencies.contains_key(pair));
    }

    fn into_parent_reasons(self) -> Vec<ParentDependencyReasons> {
        let Self {
            reasons,
            mut provenance,
            mut decisions,
        } = self;
        reasons
            .into_iter()
            .map(
                |((predecessor, successor), reasons)| ParentDependencyReasons {
                    predecessor,
                    successor,
                    reasons: reasons.into_iter().collect(),
                    provenance: provenance
                        .remove(&(predecessor, successor))
                        .unwrap_or_default()
                        .into_iter()
                        .collect(),
                    decisions: decisions
                        .remove(&(predecessor, successor))
                        .unwrap_or_default()
                        .into_iter()
                        .collect(),
                },
            )
            .collect()
    }
}

fn add_parent_dependency_with_diagnostics(
    dependencies: &mut BTreeMap<(usize, usize), DependencyClass>,
    diagnostics: &mut DependencyDiagnostics,
    left: usize,
    right: usize,
    class: DependencyClass,
    diagnostic: DependencyDiagnostic,
) {
    add_parent_dependency(dependencies, left, right, class);
    diagnostics.add(left, right, diagnostic);
}

fn add_parent_dependency(
    dependencies: &mut BTreeMap<(usize, usize), DependencyClass>,
    left: usize,
    right: usize,
    class: DependencyClass,
) {
    let Some(pair) = canonical_parent_pair(left, right) else {
        return;
    };
    dependencies
        .entry(pair)
        .and_modify(|existing| {
            if class == DependencyClass::Hard {
                *existing = DependencyClass::Hard;
            }
        })
        .or_insert(class);
}

fn transitive_reduce_parent_dependencies(
    count: usize,
    dependencies: BTreeMap<(usize, usize), DependencyClass>,
) -> BTreeMap<(usize, usize), DependencyClass> {
    if count <= 1 || dependencies.len() <= 1 {
        return dependencies;
    }

    let mut outgoing = vec![Vec::<(usize, DependencyClass)>::new(); count];
    for ((predecessor, successor), class) in dependencies {
        debug_assert!(predecessor < successor);
        if predecessor >= count || successor >= count {
            continue;
        }
        outgoing[predecessor].push((successor, class));
    }
    for successors in &mut outgoing {
        successors.sort_by_key(|(successor, _)| *successor);
        successors.dedup_by_key(|(successor, _)| *successor);
    }

    let words = count.div_ceil(64);
    let mut reachable = vec![vec![0_u64; words]; count];
    let mut reduced = BTreeMap::new();
    for predecessor in (0..count).rev() {
        for &(successor, class) in &outgoing[predecessor] {
            let word = successor / 64;
            let bit = successor % 64;
            if reachable[predecessor][word] & (1_u64 << bit) != 0 {
                continue;
            }
            reduced.insert((predecessor, successor), class);
            reachable[predecessor][word] |= 1_u64 << bit;
            let successor_reachability = reachable[successor].clone();
            for (target, source) in reachable[predecessor]
                .iter_mut()
                .zip(successor_reachability.iter())
            {
                *target |= *source;
            }
        }
    }
    reduced
}

fn parent_levels(
    count: usize,
    dependencies: &[ParentDependency],
) -> Result<Vec<Vec<usize>>, String> {
    let mut predecessors = vec![Vec::<usize>::new(); count];
    for dependency in dependencies {
        if dependency.predecessor >= count || dependency.successor >= count {
            return Err("projected dependency references unknown parent transaction".to_owned());
        }
        predecessors[dependency.successor].push(dependency.predecessor);
    }
    let mut levels = vec![0usize; count];
    let mut max_level = 0usize;
    for index in 0..count {
        let level = predecessors[index]
            .iter()
            .map(|predecessor| levels[*predecessor].saturating_add(1))
            .max()
            .unwrap_or(0);
        levels[index] = level;
        max_level = max_level.max(level);
    }
    let mut output = vec![Vec::new(); max_level.saturating_add(1)];
    if count == 0 {
        output.clear();
        return Ok(output);
    }
    for (transaction, level) in levels.into_iter().enumerate() {
        output[level].push(transaction);
    }
    Ok(output)
}

fn parent_static_edges(
    profile_graph: &ProfileGraph,
    plan: &LastPlan,
    left_parent: usize,
    right_parent: usize,
) -> BTreeSet<ProfileEdgeIndex> {
    let mut output = BTreeSet::new();
    let Some(left_profiles) = plan.parent_profiles.get(left_parent) else {
        return output;
    };
    let Some(right_profiles) = plan.parent_profiles.get(right_parent) else {
        return output;
    };
    for &left in left_profiles {
        for &right in right_profiles {
            if let Some(edge_index) = profile_graph.edge_between_profiles(left, right) {
                output.insert(edge_index);
            }
        }
    }
    output
}

fn parent_has_static_candidate(
    plan: &LastPlan,
    left_parent: usize,
    right_parent: usize,
    edge_index: ProfileEdgeIndex,
) -> bool {
    let left = TxIndex(left_parent as u32);
    let right = TxIndex(right_parent as u32);
    let pair = if left <= right {
        (left, right)
    } else {
        (right, left)
    };
    if plan.candidate_provenances.get(&pair).is_some_and(|items| {
        items.contains(&EdgeProvenance::Static {
            profile_edge_index: edge_index,
        })
    }) {
        return true;
    }
    plan.graph.compact_groups().iter().any(|group| {
        matches!(
            group.provenance(),
            EdgeProvenance::Static { profile_edge_index } if profile_edge_index == edge_index
        ) && group.members().binary_search(&left).is_ok()
            && group.members().binary_search(&right).is_ok()
    })
}

fn parent_candidate_provenances(
    plan: &LastPlan,
    left_parent: usize,
    right_parent: usize,
) -> BTreeSet<EdgeProvenance> {
    let left = TxIndex(left_parent as u32);
    let right = TxIndex(right_parent as u32);
    let pair = if left <= right {
        (left, right)
    } else {
        (right, left)
    };
    let mut output = plan
        .candidate_provenances
        .get(&pair)
        .cloned()
        .unwrap_or_default();
    for group in plan.graph.compact_groups() {
        if group.members().binary_search(&left).is_ok()
            && group.members().binary_search(&right).is_ok()
        {
            output.insert(group.provenance());
        }
    }
    output
}

fn read_input<'a>(ptr: *const u8, len: usize) -> Result<&'a [u8], String> {
    if ptr.is_null() && len != 0 {
        return Err("null JSON pointer with non-zero length".to_owned());
    }
    if len == 0 {
        return Ok(&[]);
    }
    // SAFETY: the caller promises a readable input allocation for the duration of the call.
    Ok(unsafe { slice::from_raw_parts(ptr, len) })
}

fn buffer_from_vec(mut value: Vec<u8>) -> AcgByteBuffer {
    let buffer = AcgByteBuffer {
        ptr: value.as_mut_ptr(),
        len: value.len(),
        cap: value.capacity(),
    };
    std::mem::forget(value);
    buffer
}

unsafe fn set_buffer(out: *mut AcgByteBuffer, value: Vec<u8>) {
    if !out.is_null() {
        // SAFETY: caller supplied a writable out pointer.
        unsafe { *out = buffer_from_vec(value) };
    }
}

unsafe fn set_error(out: *mut AcgByteBuffer, message: impl Into<String>) {
    unsafe { set_buffer(out, message.into().into_bytes()) };
}

fn ffi_result<T>(operation: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(operation))
        .map_err(|_| "Rust scheduler bridge panicked".to_owned())?
}

/// Creates a scheduler from a JSON bridge configuration.
///
/// # Safety
/// `json_ptr` must reference `json_len` readable bytes for the duration of this call. If
/// `error_out` is non-null, it must point to writable `AcgByteBuffer` storage.
#[no_mangle]
pub unsafe extern "C" fn acg_wasmd_scheduler_new(
    json_ptr: *const u8,
    json_len: usize,
    error_out: *mut AcgByteBuffer,
) -> *mut AcgWasmdScheduler {
    let result = ffi_result(|| {
        let bytes = read_input(json_ptr, json_len)?;
        let config: BridgeConfig =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        let state = SchedulerState::new(config)?;
        Ok(Box::new(AcgWasmdScheduler { state }))
    });
    match result {
        Ok(scheduler) => Box::into_raw(scheduler),
        Err(error) => {
            // SAFETY: error_out is an optional caller-owned out pointer.
            unsafe { set_error(error_out, error) };
            ptr::null_mut()
        }
    }
}

/// Plans one block and returns a JSON-encoded `PlanResponse`.
///
/// # Safety
/// `scheduler` must be a live pointer returned by `acg_wasmd_scheduler_new` and must not be used
/// concurrently. `json_ptr` must reference `json_len` readable bytes. Non-null output pointers
/// must reference writable `AcgByteBuffer` storage.
#[no_mangle]
pub unsafe extern "C" fn acg_wasmd_scheduler_plan(
    scheduler: *mut AcgWasmdScheduler,
    json_ptr: *const u8,
    json_len: usize,
    result_out: *mut AcgByteBuffer,
    error_out: *mut AcgByteBuffer,
) -> i32 {
    let result = ffi_result(|| {
        if scheduler.is_null() {
            return Err("null scheduler".to_owned());
        }
        let bytes = read_input(json_ptr, json_len)?;
        let decode_started = Instant::now();
        let request: PlanRequest =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        let decode_nanos = nanos_u64(decode_started.elapsed());
        // SAFETY: null was checked; the Go wrapper serializes calls per scheduler instance.
        let scheduler = unsafe { &mut *scheduler };
        let response = scheduler.state.plan_with_decode(request, decode_nanos)?;
        serde_json::to_vec(&response).map_err(|error| error.to_string())
    });
    match result {
        Ok(bytes) => {
            // SAFETY: result_out is an optional caller-owned out pointer.
            unsafe { set_buffer(result_out, bytes) };
            0
        }
        Err(error) => {
            // SAFETY: error_out is an optional caller-owned out pointer.
            unsafe { set_error(error_out, error) };
            1
        }
    }
}

/// Applies concrete execution feedback and returns a JSON-encoded feedback response.
///
/// # Safety
/// `scheduler` must be a live pointer returned by `acg_wasmd_scheduler_new` and must not be used
/// concurrently. `json_ptr` must reference `json_len` readable bytes. Non-null output pointers
/// must reference writable `AcgByteBuffer` storage.
#[no_mangle]
pub unsafe extern "C" fn acg_wasmd_scheduler_feedback(
    scheduler: *mut AcgWasmdScheduler,
    json_ptr: *const u8,
    json_len: usize,
    result_out: *mut AcgByteBuffer,
    error_out: *mut AcgByteBuffer,
) -> i32 {
    let result = ffi_result(|| {
        if scheduler.is_null() {
            return Err("null scheduler".to_owned());
        }
        let bytes = read_input(json_ptr, json_len)?;
        let request: FeedbackRequest =
            serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        // SAFETY: null was checked; the Go wrapper serializes calls per scheduler instance.
        let scheduler = unsafe { &mut *scheduler };
        let response = scheduler.state.feedback(request)?;
        serde_json::to_vec(&response).map_err(|error| error.to_string())
    });
    match result {
        Ok(bytes) => {
            // SAFETY: result_out is an optional caller-owned out pointer.
            unsafe { set_buffer(result_out, bytes) };
            0
        }
        Err(error) => {
            // SAFETY: error_out is an optional caller-owned out pointer.
            unsafe { set_error(error_out, error) };
            1
        }
    }
}

/// Releases a scheduler allocated by `acg_wasmd_scheduler_new`.
///
/// # Safety
/// `scheduler` must be null or a live pointer returned by `acg_wasmd_scheduler_new` that has not
/// previously been freed.
#[no_mangle]
pub unsafe extern "C" fn acg_wasmd_scheduler_free(scheduler: *mut AcgWasmdScheduler) {
    if scheduler.is_null() {
        return;
    }
    // SAFETY: ownership was transferred by acg_wasmd_scheduler_new and must be returned once.
    unsafe { drop(Box::from_raw(scheduler)) };
}

/// Releases a byte buffer returned through this FFI.
///
/// # Safety
/// `buffer` must be empty or be an `AcgByteBuffer` returned by this crate that has not previously
/// been freed.
#[no_mangle]
pub unsafe extern "C" fn acg_wasmd_scheduler_buffer_free(buffer: AcgByteBuffer) {
    if buffer.ptr.is_null() {
        return;
    }
    // SAFETY: buffers returned by this crate are leaked Vec allocations with these exact parts.
    unsafe { drop(Vec::from_raw_parts(buffer.ptr, buffer.len, buffer.cap)) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn native_profile(name: &str) -> Value {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let bytes = std::fs::read(root.join(format!(
            "benchmarks/symbolic/native-s3/{name}.symbolic.json"
        )))
        .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    fn state() -> SchedulerState {
        SchedulerState::new(BridgeConfig {
            documents: vec![SymbolicDocument {
                family: "cw20-base".to_owned(),
                document: native_profile("cw20-base"),
            }],
            planning: PlanningOverrides::default(),
            dependency_diagnostics: true,
        })
        .unwrap()
    }

    fn transfer(instance: &str, sender: &str, recipient: &str) -> PlanComponent {
        PlanComponent {
            family: "cw20-base".to_owned(),
            instance: instance.to_owned(),
            entrypoint: "execute::Transfer".to_owned(),
            bindings: json!({"recipient": recipient, "amount": "1", "info": {"sender": sender}}),
            estimated_execution_cost: 1,
        }
    }

    #[test]
    fn real_native_profile_builds_same_instance_dependency() {
        let mut state = state();
        let plan = state
            .plan(PlanRequest {
                epoch: 1,
                transactions: vec![
                    PlanTransaction {
                        tx_id: 1,
                        components: vec![transfer("token-a", "alice", "bob")],
                        hard_resources: vec![],
                    },
                    PlanTransaction {
                        tx_id: 2,
                        components: vec![transfer("token-a", "alice", "carol")],
                        hard_resources: vec![],
                    },
                ],
            })
            .unwrap();
        assert_eq!(plan.transaction_count, 2);
        assert!(plan
            .dependencies
            .iter()
            .any(|dependency| dependency.predecessor == 0 && dependency.successor == 1));
        assert_eq!(plan.feedback_pairs, vec![ParentPair { left: 0, right: 1 }]);
    }

    #[test]
    fn atomic_multicall_transactions_are_scheduled_as_parent_nodes() {
        let mut state = state();
        let plan = state
            .plan(PlanRequest {
                epoch: 1,
                transactions: vec![
                    PlanTransaction {
                        tx_id: 1,
                        components: vec![
                            transfer("token-a", "alice", "bob"),
                            transfer("token-a", "alice", "bob"),
                        ],
                        hard_resources: vec![],
                    },
                    PlanTransaction {
                        tx_id: 2,
                        components: vec![
                            transfer("token-a", "alice", "carol"),
                            transfer("token-a", "alice", "carol"),
                        ],
                        hard_resources: vec![],
                    },
                ],
            })
            .unwrap();
        assert_eq!(plan.transaction_count, 2);
        assert_eq!(plan.component_count, 4);
        assert!(plan.logical_candidate_edges > plan.candidate_edges);
        assert!(
            plan.candidate_edges <= 2,
            "duplicate component products must collapse before scheduling"
        );
        assert!(plan
            .dependencies
            .iter()
            .any(|dependency| dependency.predecessor == 0 && dependency.successor == 1));
    }

    #[test]
    fn different_instances_do_not_gain_a_fake_global_barrier() {
        let mut state = state();
        let plan = state
            .plan(PlanRequest {
                epoch: 1,
                transactions: vec![
                    PlanTransaction {
                        tx_id: 1,
                        components: vec![transfer("token-a", "alice", "bob")],
                        hard_resources: vec![],
                    },
                    PlanTransaction {
                        tx_id: 2,
                        components: vec![transfer("token-b", "alice", "carol")],
                        hard_resources: vec![],
                    },
                ],
            })
            .unwrap();
        assert!(plan.dependencies.is_empty());
        assert_eq!(plan.levels, vec![vec![0, 1]]);
    }

    #[test]
    fn adapter_bank_resources_are_hard_dependencies() {
        let mut state = state();
        let plan = state
            .plan(PlanRequest {
                epoch: 1,
                transactions: vec![
                    PlanTransaction {
                        tx_id: 1,
                        components: vec![],
                        hard_resources: vec!["bank:alice:uatom".to_owned()],
                    },
                    PlanTransaction {
                        tx_id: 2,
                        components: vec![],
                        hard_resources: vec!["bank:alice:uatom".to_owned()],
                    },
                ],
            })
            .unwrap();
        assert_eq!(
            plan.dependencies,
            vec![ParentDependency {
                predecessor: 0,
                successor: 1,
                class: DependencyClass::Hard
            }]
        );
        assert_eq!(
            plan.dependency_reasons,
            vec![ParentDependencyReasons {
                predecessor: 0,
                successor: 1,
                reasons: vec![DependencyReason::BankResource],
                provenance: vec![DependencyProvenance::BankResource],
                decisions: vec![DependencyDecision::BankHard],
            }]
        );
    }

    #[test]
    fn parent_projection_reduction_elides_transitive_edges() {
        let dependencies = BTreeMap::from([
            ((0, 1), DependencyClass::Hard),
            ((0, 2), DependencyClass::Hard),
            ((1, 2), DependencyClass::Hard),
        ]);
        let reduced = transitive_reduce_parent_dependencies(3, dependencies);
        assert_eq!(
            reduced,
            BTreeMap::from([
                ((0, 1), DependencyClass::Hard),
                ((1, 2), DependencyClass::Hard),
            ])
        );
    }

    #[test]
    fn planning_overrides_reach_the_real_risk_bounded_scheduler_config() {
        let mut state = SchedulerState::new(BridgeConfig {
            documents: vec![SymbolicDocument {
                family: "cw20-base".to_owned(),
                document: native_profile("cw20-base"),
            }],
            planning: PlanningOverrides {
                risk_budget: Some(0.55),
                hard_threshold: Some(0.90),
                independent_observations_before_softening: Some(4),
                ..PlanningOverrides::default()
            },
            dependency_diagnostics: true,
        })
        .unwrap();
        let plan = state
            .plan(PlanRequest {
                epoch: 1,
                transactions: vec![PlanTransaction {
                    tx_id: 1,
                    components: vec![transfer("token-a", "alice", "bob")],
                    hard_resources: vec![],
                }],
            })
            .unwrap();
        let planning = plan.planning.expect("diagnostic planning snapshot");
        assert!((planning.risk_budget - 0.55).abs() < f64::EPSILON);
        assert!((planning.hard_threshold - 0.90).abs() < f64::EPSILON);
        assert_eq!(planning.independent_observations_before_softening, 4);
        let timings = plan.planning_timings.expect("planning timings");
        assert_eq!(timings.rust_decode_nanos, 0);
        assert!(
            plan.candidate_decisions.hard
                + plan.candidate_decisions.soft
                + plan.candidate_decisions.low
                <= plan.logical_candidate_edges
        );
    }

    #[test]
    fn feedback_reaches_existing_adaptive_store() {
        let mut state = state();
        let _ = state
            .plan(PlanRequest {
                epoch: 1,
                transactions: vec![
                    PlanTransaction {
                        tx_id: 1,
                        components: vec![transfer("token-a", "alice", "bob")],
                        hard_resources: vec![],
                    },
                    PlanTransaction {
                        tx_id: 2,
                        components: vec![transfer("token-a", "alice", "carol")],
                        hard_resources: vec![],
                    },
                ],
            })
            .unwrap();
        let response = state
            .feedback(FeedbackRequest {
                epoch: 1,
                observations: vec![PairObservation {
                    left: 0,
                    right: 1,
                    conflict_kinds: ConflictKinds::WRITE_READ.bits(),
                    conflict: true,
                    source: FeedbackSource::PreExecution,
                }],
                replay_attributions: vec![],
                serialization: vec![SerializationObservation {
                    predecessor: 0,
                    transaction: 1,
                    marginal_ready_delay_nanos: 123,
                }],
                economics: EconomicsObservation {
                    serial_service_nanos: 1000,
                    pre_consensus_nanos: 800,
                    post_consensus_nanos: 200,
                    transaction_count: 2,
                },
            })
            .unwrap();
        assert!(response.applied_observations > 0);
        assert!(response.serialization_observations_applied > 0);
    }
}
