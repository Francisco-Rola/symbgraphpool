//! CosmWasm/validator-runtime feedback collection for adaptive conflict statistics.

mod adaptive_pipeline;

pub use adaptive_pipeline::{
    AdaptiveBlockPlan, AdaptiveBlockRun, AdaptivePipelineError, AdaptivePlanningConfig,
    AdaptivePlanningMetrics, AdaptiveSerialPipeline, BlockEconomicsObservation, RegimeChangeConfig,
    RegimeChangeObservation, ReplayAttribution, SerialBypassConfig, SerializationAttribution,
};

use std::collections::{BTreeMap, BTreeSet};

use acg_candidate_graph::{CandidateGraph, EdgeProvenance};
use acg_core::{ConflictKinds, ProfileEdgeIndex, ProfileId, TxIndex};
use acg_cosmwasm_engine::{AccessKind, AccessRecord, Address};
use acg_feedback::{
    AdaptiveFeedbackConfig, AdaptiveFeedbackStore, AggregatedConflictBatch,
    AggregatedObservationBuffer, ApplySummary, ConflictObservation, FeedbackCheckpoint,
    FeedbackError, ObservationBuffer, ObservationSource, ObservationTarget,
};
use acg_profile_graph::ProfileGraph;
use acg_validator_sim::BlockExecutionReport;
use thiserror::Error;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TraceConflictConfig {
    /// Reverted child accesses remain useful audit evidence but are excluded from canonical
    /// committed-conflict attribution by default.
    pub include_reverted_accesses: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RuntimeFeedbackWeights {
    pub pre_execution_conflict: f64,
    pub pre_execution_independent: f64,
    pub canonical_conflict: f64,
    pub canonical_independent: f64,
    pub validation_conflict: f64,
    pub validation_independent: f64,
    pub replay_conflict: f64,
    pub replay_independent: f64,
}

impl Default for RuntimeFeedbackWeights {
    fn default() -> Self {
        Self {
            pre_execution_conflict: 1.0,
            pre_execution_independent: 1.0,
            canonical_conflict: 3.0,
            canonical_independent: 3.0,
            validation_conflict: 3.0,
            validation_independent: 3.0,
            replay_conflict: 4.0,
            replay_independent: 4.0,
        }
    }
}

impl RuntimeFeedbackWeights {
    fn validate(&self) -> Result<(), RuntimeFeedbackError> {
        for (name, value) in [
            ("pre_execution_conflict", self.pre_execution_conflict),
            ("pre_execution_independent", self.pre_execution_independent),
            ("canonical_conflict", self.canonical_conflict),
            ("canonical_independent", self.canonical_independent),
            ("validation_conflict", self.validation_conflict),
            ("validation_independent", self.validation_independent),
            ("replay_conflict", self.replay_conflict),
            ("replay_independent", self.replay_independent),
        ] {
            if !value.is_finite() || value <= 0.0 {
                return Err(RuntimeFeedbackError::InvalidWeight {
                    name: name.to_owned(),
                    value,
                });
            }
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ObservedConflict {
    pub left: TxIndex,
    pub right: TxIndex,
    pub conflict_kinds: ConflictKinds,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
enum ExactScope {
    ContractStorage(Address),
    GlobalBank,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct ExactLocation {
    scope: ExactScope,
    key: Vec<u8>,
}

#[derive(Default)]
struct Participants {
    readers: BTreeSet<TxIndex>,
    writers: BTreeSet<TxIndex>,
}

struct ScanAccess {
    transaction: TxIndex,
    contract: Address,
    start: Vec<u8>,
    end: Option<Vec<u8>>,
}

#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
struct FootprintAccessMode {
    read: bool,
    write: bool,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct FootprintScan {
    contract: Address,
    start: Vec<u8>,
    end: Option<Vec<u8>>,
}

#[derive(Clone, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
struct TransactionConflictFootprint {
    exact: BTreeMap<ExactLocation, FootprintAccessMode>,
    scans: Vec<FootprintScan>,
}

impl TransactionConflictFootprint {
    fn record(&mut self, access: &AccessRecord) {
        match &access.kind {
            AccessKind::StorageRead => {
                self.exact
                    .entry(ExactLocation {
                        scope: ExactScope::ContractStorage(access.contract.clone()),
                        key: access.key.clone(),
                    })
                    .or_default()
                    .read = true;
            }
            AccessKind::StorageWrite | AccessKind::StorageRemove => {
                self.exact
                    .entry(ExactLocation {
                        scope: ExactScope::ContractStorage(access.contract.clone()),
                        key: access.key.clone(),
                    })
                    .or_default()
                    .write = true;
            }
            AccessKind::StorageScan => self.scans.push(FootprintScan {
                contract: access.contract.clone(),
                start: access.key.clone(),
                end: access.range_end.clone(),
            }),
            AccessKind::BankRead => {
                self.exact
                    .entry(ExactLocation {
                        scope: ExactScope::GlobalBank,
                        key: access.key.clone(),
                    })
                    .or_default()
                    .read = true;
            }
            AccessKind::BankWrite => {
                self.exact
                    .entry(ExactLocation {
                        scope: ExactScope::GlobalBank,
                        key: access.key.clone(),
                    })
                    .or_default()
                    .write = true;
            }
        }
    }

    fn finish(&mut self) {
        self.scans.sort();
        self.scans.dedup();
    }
}

/// Generates concrete read/write and write/write overlaps from execution artifacts.
///
/// Exact keys are indexed, so ordinary conflicts are generated from accesses rather than all
/// transaction pairs. Storage scans are compared only with writes in the same contract namespace.
#[derive(Clone, Copy, Debug)]
pub struct AccessConflictDetector {
    config: TraceConflictConfig,
}

impl AccessConflictDetector {
    pub fn new(config: TraceConflictConfig) -> Self {
        Self { config }
    }

    pub fn detect(
        &self,
        report: &BlockExecutionReport,
    ) -> Result<Vec<ObservedConflict>, RuntimeFeedbackError> {
        Ok(self
            .detect_map(report)?
            .into_iter()
            .map(|((left, right), conflict_kinds)| ObservedConflict {
                left,
                right,
                conflict_kinds,
            })
            .collect())
    }

    fn detect_map(
        &self,
        report: &BlockExecutionReport,
    ) -> Result<BTreeMap<(TxIndex, TxIndex), ConflictKinds>, RuntimeFeedbackError> {
        self.detect_map_with_compact_exclusion(report, None)
    }

    fn detect_map_excluding_compact(
        &self,
        report: &BlockExecutionReport,
        candidate_graph: &CandidateGraph,
    ) -> Result<BTreeMap<(TxIndex, TxIndex), ConflictKinds>, RuntimeFeedbackError> {
        self.detect_map_with_compact_exclusion(report, Some(candidate_graph))
    }

    fn detect_map_with_compact_exclusion(
        &self,
        report: &BlockExecutionReport,
        compact_graph: Option<&CandidateGraph>,
    ) -> Result<BTreeMap<(TxIndex, TxIndex), ConflictKinds>, RuntimeFeedbackError> {
        let mut exact = BTreeMap::<ExactLocation, Participants>::new();
        let mut scans = Vec::<ScanAccess>::new();
        let mut seen_indices = BTreeSet::<usize>::new();

        for execution in &report.transactions {
            if !seen_indices.insert(execution.transaction_index) {
                return Err(RuntimeFeedbackError::DuplicateExecutionIndex(
                    execution.transaction_index,
                ));
            }
            let Some(outcome) = execution.result.as_ref().ok() else {
                continue;
            };
            if outcome.transaction_id != execution.transaction_id {
                return Err(RuntimeFeedbackError::OutcomeTransactionIdMismatch {
                    execution: execution.transaction_id.0,
                    outcome: outcome.transaction_id.0,
                });
            }
            let tx_index = TxIndex(u32::try_from(execution.transaction_index).map_err(|_| {
                RuntimeFeedbackError::TransactionIndexOverflow(execution.transaction_index)
            })?);
            for access in &outcome.accesses {
                if access.transaction_id != execution.transaction_id {
                    return Err(RuntimeFeedbackError::AccessTransactionIdMismatch {
                        execution: execution.transaction_id.0,
                        access: access.transaction_id.0,
                    });
                }
                if access.reverted && !self.config.include_reverted_accesses {
                    continue;
                }
                index_access(tx_index, access, &mut exact, &mut scans);
            }
        }

        let mut conflicts = BTreeMap::<(TxIndex, TxIndex), ConflictKinds>::new();
        for (location, participants) in &exact {
            let scope_kind = if matches!(&location.scope, ExactScope::GlobalBank) {
                ConflictKinds::BALANCE
            } else {
                ConflictKinds::empty()
            };

            let compact_read_write = compact_graph.is_some_and(|graph| {
                let mut members = participants
                    .readers
                    .iter()
                    .chain(&participants.writers)
                    .copied()
                    .collect::<Vec<_>>();
                members.sort_unstable();
                members.dedup();
                members.len() >= 2 && graph.compact_provenance_covering(&members).is_some()
            });
            if !compact_read_write {
                for writer in &participants.writers {
                    for reader in &participants.readers {
                        if writer != reader {
                            add_read_write(
                                &mut conflicts,
                                *reader,
                                *writer,
                                scope_kind,
                                compact_graph,
                            );
                        }
                    }
                }
            }

            let compact_write_write = compact_graph.is_some_and(|graph| {
                if participants.writers.len() < 2 {
                    return false;
                }
                let writers = participants.writers.iter().copied().collect::<Vec<_>>();
                graph.compact_provenance_covering(&writers).is_some()
            });
            if !compact_write_write {
                for (offset, left) in participants.writers.iter().enumerate() {
                    for right in participants.writers.iter().skip(offset + 1) {
                        add_kind(
                            &mut conflicts,
                            *left,
                            *right,
                            ConflictKinds::WRITE_WRITE | scope_kind,
                            compact_graph,
                        );
                    }
                }
            }
        }

        let storage_writes = exact.iter().filter_map(|(location, participants)| {
            let ExactScope::ContractStorage(contract) = &location.scope else {
                return None;
            };
            Some((contract, location.key.as_slice(), &participants.writers))
        });
        for (contract, key, writers) in storage_writes {
            for scan in scans.iter().filter(|scan| &scan.contract == contract) {
                if !range_contains(key, &scan.start, scan.end.as_deref()) {
                    continue;
                }
                let compact_scan_write = compact_graph.is_some_and(|graph| {
                    let mut members = writers.iter().copied().collect::<Vec<_>>();
                    members.push(scan.transaction);
                    members.sort_unstable();
                    members.dedup();
                    members.len() >= 2 && graph.compact_provenance_covering(&members).is_some()
                });
                if compact_scan_write {
                    continue;
                }
                for writer in writers {
                    if *writer != scan.transaction {
                        add_read_write(
                            &mut conflicts,
                            scan.transaction,
                            *writer,
                            ConflictKinds::empty(),
                            compact_graph,
                        );
                    }
                }
            }
        }

        Ok(conflicts)
    }

    fn transaction_footprints(
        &self,
        report: &BlockExecutionReport,
        transaction_count: usize,
    ) -> Result<Vec<Option<TransactionConflictFootprint>>, RuntimeFeedbackError> {
        let mut footprints = vec![None; transaction_count];
        for execution in &report.transactions {
            if execution.transaction_index >= transaction_count {
                return Err(RuntimeFeedbackError::TransactionIndexOverflow(
                    execution.transaction_index,
                ));
            }
            let Some(outcome) = execution.result.as_ref().ok() else {
                continue;
            };
            let mut footprint = TransactionConflictFootprint::default();
            for access in &outcome.accesses {
                if access.transaction_id != execution.transaction_id {
                    return Err(RuntimeFeedbackError::AccessTransactionIdMismatch {
                        execution: execution.transaction_id.0,
                        access: access.transaction_id.0,
                    });
                }
                if access.reverted && !self.config.include_reverted_accesses {
                    continue;
                }
                footprint.record(access);
            }
            footprint.finish();
            footprints[execution.transaction_index] = Some(footprint);
        }
        Ok(footprints)
    }
}

fn index_access(
    tx_index: TxIndex,
    access: &AccessRecord,
    exact: &mut BTreeMap<ExactLocation, Participants>,
    scans: &mut Vec<ScanAccess>,
) {
    match &access.kind {
        AccessKind::StorageRead => {
            exact
                .entry(ExactLocation {
                    scope: ExactScope::ContractStorage(access.contract.clone()),
                    key: access.key.clone(),
                })
                .or_default()
                .readers
                .insert(tx_index);
        }
        AccessKind::StorageWrite | AccessKind::StorageRemove => {
            exact
                .entry(ExactLocation {
                    scope: ExactScope::ContractStorage(access.contract.clone()),
                    key: access.key.clone(),
                })
                .or_default()
                .writers
                .insert(tx_index);
        }
        AccessKind::StorageScan => scans.push(ScanAccess {
            transaction: tx_index,
            contract: access.contract.clone(),
            start: access.key.clone(),
            end: access.range_end.clone(),
        }),
        AccessKind::BankRead => {
            exact
                .entry(ExactLocation {
                    scope: ExactScope::GlobalBank,
                    key: access.key.clone(),
                })
                .or_default()
                .readers
                .insert(tx_index);
        }
        AccessKind::BankWrite => {
            exact
                .entry(ExactLocation {
                    scope: ExactScope::GlobalBank,
                    key: access.key.clone(),
                })
                .or_default()
                .writers
                .insert(tx_index);
        }
    }
}

fn range_contains(key: &[u8], start: &[u8], end: Option<&[u8]>) -> bool {
    key >= start && end.map_or(true, |end| key < end)
}

fn footprint_conflict_kinds_ordered(
    left: &TransactionConflictFootprint,
    right: &TransactionConflictFootprint,
) -> ConflictKinds {
    let mut kinds = ConflictKinds::empty();
    for (location, left_mode) in &left.exact {
        let Some(right_mode) = right.exact.get(location) else {
            continue;
        };
        let mut location_kinds = ConflictKinds::empty();
        if left_mode.write && right_mode.write {
            location_kinds |= ConflictKinds::WRITE_WRITE;
        }
        if left_mode.read && right_mode.write {
            location_kinds |= ConflictKinds::READ_WRITE;
        }
        if left_mode.write && right_mode.read {
            location_kinds |= ConflictKinds::WRITE_READ;
        }
        if !location_kinds.is_empty() && matches!(&location.scope, ExactScope::GlobalBank) {
            location_kinds |= ConflictKinds::BALANCE;
        }
        kinds |= location_kinds;
    }

    for scan in &left.scans {
        if right.exact.iter().any(|(location, mode)| {
            let ExactScope::ContractStorage(contract) = &location.scope else {
                return false;
            };
            mode.write
                && contract == &scan.contract
                && range_contains(&location.key, &scan.start, scan.end.as_deref())
        }) {
            kinds |= ConflictKinds::READ_WRITE;
        }
    }
    for scan in &right.scans {
        if left.exact.iter().any(|(location, mode)| {
            let ExactScope::ContractStorage(contract) = &location.scope else {
                return false;
            };
            mode.write
                && contract == &scan.contract
                && range_contains(&location.key, &scan.start, scan.end.as_deref())
        }) {
            kinds |= ConflictKinds::WRITE_READ;
        }
    }
    kinds
}

fn compact_group_conflict_summary(
    members: &[TxIndex],
    footprints: &[Option<TransactionConflictFootprint>],
    evidence_participants: Option<&BTreeSet<TxIndex>>,
) -> Result<(usize, ConflictKinds), RuntimeFeedbackError> {
    let mut classes = BTreeMap::<(TransactionConflictFootprint, bool), Vec<TxIndex>>::new();
    for member in members {
        let footprint = footprints
            .get(member.0 as usize)
            .and_then(Option::as_ref)
            .ok_or(RuntimeFeedbackError::MissingConflictFootprint(*member))?;
        let participant = evidence_participants.is_some_and(|set| set.contains(member));
        classes
            .entry((footprint.clone(), participant))
            .or_default()
            .push(*member);
    }

    let classes = classes.into_iter().collect::<Vec<_>>();
    let mut conflicts = 0_usize;
    let mut conflict_kinds = ConflictKinds::empty();
    for left_index in 0..classes.len() {
        let ((left_footprint, left_participant), left_members) = &classes[left_index];
        for (right_offset, ((right_footprint, right_participant), right_members)) in
            classes.iter().enumerate().skip(left_index)
        {
            if evidence_participants.is_some() && !*left_participant && !*right_participant {
                continue;
            }

            let kinds = if left_index == right_offset {
                footprint_conflict_kinds_ordered(left_footprint, right_footprint)
            } else {
                let left_before_right = left_members
                    .last()
                    .zip(right_members.first())
                    .is_some_and(|(left, right)| left < right);
                let right_before_left = right_members
                    .last()
                    .zip(left_members.first())
                    .is_some_and(|(right, left)| right < left);
                if left_before_right {
                    footprint_conflict_kinds_ordered(left_footprint, right_footprint)
                } else if right_before_left {
                    footprint_conflict_kinds_ordered(right_footprint, left_footprint)
                } else {
                    footprint_conflict_kinds_ordered(left_footprint, right_footprint)
                        | footprint_conflict_kinds_ordered(right_footprint, left_footprint)
                }
            };
            if kinds.is_empty() {
                continue;
            }
            let count = if left_index == right_offset {
                choose_two(left_members.len())
            } else {
                left_members.len().saturating_mul(right_members.len())
            };
            conflicts = conflicts.saturating_add(count);
            conflict_kinds |= kinds;
        }
    }
    Ok((conflicts, conflict_kinds))
}

fn add_read_write(
    conflicts: &mut BTreeMap<(TxIndex, TxIndex), ConflictKinds>,
    reader: TxIndex,
    writer: TxIndex,
    extra_kind: ConflictKinds,
    compact_graph: Option<&CandidateGraph>,
) {
    let (left, right) = canonical_tx_pair(reader, writer);
    let kind = if left == reader {
        ConflictKinds::READ_WRITE
    } else {
        ConflictKinds::WRITE_READ
    };
    add_kind(conflicts, left, right, kind | extra_kind, compact_graph);
}

fn add_kind(
    conflicts: &mut BTreeMap<(TxIndex, TxIndex), ConflictKinds>,
    left: TxIndex,
    right: TxIndex,
    kind: ConflictKinds,
    compact_graph: Option<&CandidateGraph>,
) {
    let pair = canonical_tx_pair(left, right);
    if compact_graph.is_some_and(|graph| graph.compact_provenance_between(pair.0, pair.1).is_some())
    {
        return;
    }
    conflicts
        .entry(pair)
        .and_modify(|current| *current |= kind)
        .or_insert(kind);
}

fn canonical_tx_pair(left: TxIndex, right: TxIndex) -> (TxIndex, TxIndex) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

fn canonical_profile_pair(left: ProfileId, right: ProfileId) -> (ProfileId, ProfileId) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum CollectorRelationship {
    Static {
        edge_index: ProfileEdgeIndex,
        source: ProfileId,
        target: ProfileId,
    },
    Runtime {
        source: ProfileId,
        target: ProfileId,
    },
}

impl CollectorRelationship {
    fn source_profile(self) -> ProfileId {
        match self {
            Self::Static { source, .. } | Self::Runtime { source, .. } => source,
        }
    }

    fn target_profile(self) -> ProfileId {
        match self {
            Self::Static { target, .. } | Self::Runtime { target, .. } => target,
        }
    }

    fn observation_target(self) -> ObservationTarget {
        match self {
            Self::Static { edge_index, .. } => ObservationTarget::Static { edge_index },
            Self::Runtime { .. } => ObservationTarget::RuntimeDiscovered,
        }
    }
}

fn collector_relationship_for_provenance(
    provenance: EdgeProvenance,
    profile_pair: (ProfileId, ProfileId),
) -> CollectorRelationship {
    match provenance {
        EdgeProvenance::Static { profile_edge_index } => CollectorRelationship::Static {
            edge_index: profile_edge_index,
            source: profile_pair.0,
            target: profile_pair.1,
        },
        EdgeProvenance::RuntimeDiscovered { .. } => CollectorRelationship::Runtime {
            source: profile_pair.0,
            target: profile_pair.1,
        },
    }
}

fn collector_relationship_for_edge(
    edge: &acg_candidate_graph::TransactionEdge,
    profile_pair: (ProfileId, ProfileId),
) -> CollectorRelationship {
    collector_relationship_for_provenance(edge.provenance, profile_pair)
}

fn collector_relationship_for_profiles(
    graph: &ProfileGraph,
    profile_pair: (ProfileId, ProfileId),
) -> CollectorRelationship {
    graph
        .edge_between_profiles(profile_pair.0, profile_pair.1)
        .map(|edge_index| CollectorRelationship::Static {
            edge_index,
            source: profile_pair.0,
            target: profile_pair.1,
        })
        .unwrap_or(CollectorRelationship::Runtime {
            source: profile_pair.0,
            target: profile_pair.1,
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ValidationEvidenceKind {
    Independent,
    Invalidated {
        conflict_kinds: ConflictKinds,
    },
    Replayed {
        conflict_kinds: ConflictKinds,
        replay_cost_nanos: u64,
        invalidated_descendants: u32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ValidationEvidence {
    pub predecessor: TxIndex,
    pub transaction: TxIndex,
    pub kind: ValidationEvidenceKind,
}

/// Phase 5E local estimate of the marginal ready-time delay caused by one scheduled dependency.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SerializationCostEvidence {
    pub predecessor: TxIndex,
    pub transaction: TxIndex,
    pub marginal_ready_delay_nanos: u64,
}

/// Upstream-aggregated Phase-5E serialization evidence keyed by learned relationship.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AggregatedSerializationCostBuffer {
    batches: BTreeMap<EdgeProvenance, (u64, usize)>,
}

impl AggregatedSerializationCostBuffer {
    pub fn record(&mut self, provenance: EdgeProvenance, marginal_ready_delay_nanos: u64) {
        let entry = self.batches.entry(provenance).or_default();
        entry.0 = entry.0.saturating_add(marginal_ready_delay_nanos);
        entry.1 = entry.1.saturating_add(1);
    }

    pub fn observations(&self) -> usize {
        self.batches
            .values()
            .fold(0_usize, |total, (_, count)| total.saturating_add(*count))
    }

    pub fn relationship_batches(&self) -> usize {
        self.batches.len()
    }
}

/// Converts concrete execution/validation evidence into runtime-independent observations.
#[derive(Clone, Copy, Debug)]
pub struct BlockFeedbackCollector {
    detector: AccessConflictDetector,
    weights: RuntimeFeedbackWeights,
}

impl BlockFeedbackCollector {
    pub fn new(
        trace_config: TraceConflictConfig,
        weights: RuntimeFeedbackWeights,
    ) -> Result<Self, RuntimeFeedbackError> {
        weights.validate()?;
        Ok(Self {
            detector: AccessConflictDetector::new(trace_config),
            weights,
        })
    }

    pub fn collect_pre_execution(
        &self,
        profile_graph: &ProfileGraph,
        candidate_graph: &CandidateGraph,
        report: &BlockExecutionReport,
        feedback_store: &AdaptiveFeedbackStore,
        epoch: u64,
    ) -> Result<ObservationBuffer, RuntimeFeedbackError> {
        self.collect_access_report(
            profile_graph,
            candidate_graph,
            report,
            feedback_store,
            epoch,
            ObservationSource::PreExecution,
            self.weights.pre_execution_conflict,
            self.weights.pre_execution_independent,
            None,
        )
    }

    pub fn collect_block(
        &self,
        profile_graph: &ProfileGraph,
        candidate_graph: &CandidateGraph,
        report: &BlockExecutionReport,
        feedback_store: &AdaptiveFeedbackStore,
        epoch: u64,
    ) -> Result<ObservationBuffer, RuntimeFeedbackError> {
        self.collect_access_report(
            profile_graph,
            candidate_graph,
            report,
            feedback_store,
            epoch,
            ObservationSource::CanonicalExecution,
            self.weights.canonical_conflict,
            self.weights.canonical_independent,
            None,
        )
    }

    /// Collect concrete accesses for post-consensus replay evidence.
    ///
    /// `report` contains the final canonical outcomes for the whole decided block, while
    /// `replayed_transactions` scopes observations to pairs with at least one transaction that
    /// actually re-executed. This includes replay-vs-reused evidence without double-counting
    /// reused-vs-reused pairs already observed during pre-execution.
    pub fn collect_replay_execution(
        &self,
        profile_graph: &ProfileGraph,
        candidate_graph: &CandidateGraph,
        report: &BlockExecutionReport,
        replayed_transactions: &BTreeSet<TxIndex>,
        feedback_store: &AdaptiveFeedbackStore,
        epoch: u64,
    ) -> Result<ObservationBuffer, RuntimeFeedbackError> {
        if replayed_transactions.is_empty() {
            return Ok(ObservationBuffer::default());
        }
        self.collect_access_report(
            profile_graph,
            candidate_graph,
            report,
            feedback_store,
            epoch,
            ObservationSource::Replay,
            self.weights.replay_conflict,
            self.weights.replay_independent,
            Some(replayed_transactions),
        )
    }

    pub fn collect_pre_execution_aggregated(
        &self,
        profile_graph: &ProfileGraph,
        candidate_graph: &CandidateGraph,
        report: &BlockExecutionReport,
        feedback_store: &AdaptiveFeedbackStore,
        epoch: u64,
    ) -> Result<AggregatedObservationBuffer, RuntimeFeedbackError> {
        self.collect_access_report_aggregated(
            profile_graph,
            candidate_graph,
            report,
            feedback_store,
            epoch,
            self.weights.pre_execution_conflict,
            self.weights.pre_execution_independent,
            None,
        )
    }

    pub fn collect_block_aggregated(
        &self,
        profile_graph: &ProfileGraph,
        candidate_graph: &CandidateGraph,
        report: &BlockExecutionReport,
        feedback_store: &AdaptiveFeedbackStore,
        epoch: u64,
    ) -> Result<AggregatedObservationBuffer, RuntimeFeedbackError> {
        self.collect_access_report_aggregated(
            profile_graph,
            candidate_graph,
            report,
            feedback_store,
            epoch,
            self.weights.canonical_conflict,
            self.weights.canonical_independent,
            None,
        )
    }

    pub fn collect_replay_execution_aggregated(
        &self,
        profile_graph: &ProfileGraph,
        candidate_graph: &CandidateGraph,
        report: &BlockExecutionReport,
        replayed_transactions: &BTreeSet<TxIndex>,
        feedback_store: &AdaptiveFeedbackStore,
        epoch: u64,
    ) -> Result<AggregatedObservationBuffer, RuntimeFeedbackError> {
        if replayed_transactions.is_empty() {
            return Ok(AggregatedObservationBuffer::default());
        }
        self.collect_access_report_aggregated(
            profile_graph,
            candidate_graph,
            report,
            feedback_store,
            epoch,
            self.weights.replay_conflict,
            self.weights.replay_independent,
            Some(replayed_transactions),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_access_report_aggregated(
        &self,
        profile_graph: &ProfileGraph,
        candidate_graph: &CandidateGraph,
        report: &BlockExecutionReport,
        feedback_store: &AdaptiveFeedbackStore,
        epoch: u64,
        conflict_weight: f64,
        independent_weight: f64,
        evidence_participants: Option<&BTreeSet<TxIndex>>,
    ) -> Result<AggregatedObservationBuffer, RuntimeFeedbackError> {
        let successful = successful_transactions(report)?;
        validate_report_candidate_alignment(candidate_graph, report)?;
        let footprints = self
            .detector
            .transaction_footprints(report, candidate_graph.transactions().len())?;
        // Compact-group conflicts are counted from execution-footprint classes below. Excluding
        // those pairs from the residual detector avoids constructing one BTreeMap entry per
        // logical pair while still retaining exact candidate-miss detection outside the groups.
        let observed = self
            .detector
            .detect_map_excluding_compact(report, candidate_graph)?;

        let mut buffer = AggregatedObservationBuffer::default();
        let mut candidate_totals = BTreeMap::<CollectorRelationship, usize>::new();
        let mut candidate_totals_by_profile = BTreeMap::<(ProfileId, ProfileId), usize>::new();
        let mut observed_candidate_conflicts = BTreeMap::<CollectorRelationship, usize>::new();
        let mut observed_conflicts_by_profile = BTreeMap::<(ProfileId, ProfileId), usize>::new();
        let mut observed_candidate_conflicts_by_profile =
            BTreeMap::<(ProfileId, ProfileId), usize>::new();
        // Direct verification of predicate-False pairs that exist only because a prior candidate
        // miss broadened this static profile relationship. Once enough of these pairs are clean,
        // the feedback store can retire the broad override without forgetting the miss itself.
        let mut miss_verification_totals =
            BTreeMap::<ProfileEdgeIndex, (ProfileId, ProfileId, usize)>::new();
        let mut miss_verification_conflicts = BTreeMap::<ProfileEdgeIndex, usize>::new();

        for edge in candidate_graph.edges() {
            if candidate_graph.provenance_is_compact(edge.provenance) {
                continue;
            }
            let pair = canonical_tx_pair(edge.source, edge.target);
            if !pair_in_evidence_scope(pair, evidence_participants)
                || !successful.contains(&pair.0)
                || !successful.contains(&pair.1)
            {
                continue;
            }
            let left = candidate_transaction(candidate_graph, pair.0)?;
            let right = candidate_transaction(candidate_graph, pair.1)?;
            let profile_pair = canonical_profile_pair(left.profile_id, right.profile_id);
            let relationship = collector_relationship_for_edge(edge, profile_pair);
            *candidate_totals.entry(relationship).or_default() += 1;
            *candidate_totals_by_profile.entry(profile_pair).or_default() += 1;
            if edge.is_historical_override() {
                if let EdgeProvenance::Static { profile_edge_index } = edge.provenance {
                    let entry = miss_verification_totals
                        .entry(profile_edge_index)
                        .or_insert((left.profile_id, right.profile_id, 0));
                    entry.2 = entry.2.saturating_add(1);
                }
            }
        }

        for group in candidate_graph.compact_groups() {
            let eligible_members = group
                .members()
                .iter()
                .copied()
                .filter(|index| successful.contains(index))
                .collect::<Vec<_>>();
            if eligible_members.len() < 2 {
                continue;
            }
            let scoped_pairs =
                eligible_pairs_within_group(&eligible_members, evidence_participants);
            if scoped_pairs == 0 {
                continue;
            }
            let left = candidate_transaction(candidate_graph, eligible_members[0])?;
            let right = candidate_transaction(candidate_graph, eligible_members[1])?;
            let profile_pair = canonical_profile_pair(left.profile_id, right.profile_id);
            let relationship =
                collector_relationship_for_provenance(group.provenance(), profile_pair);
            *candidate_totals.entry(relationship).or_default() += scoped_pairs;
            *candidate_totals_by_profile.entry(profile_pair).or_default() += scoped_pairs;

            let (conflicts, conflict_kinds) = compact_group_conflict_summary(
                &eligible_members,
                &footprints,
                evidence_participants,
            )?;
            debug_assert!(conflicts <= scoped_pairs);
            if conflicts > 0 {
                buffer.record_conflict_batch(AggregatedConflictBatch {
                    source_profile: left.profile_id,
                    target_profile: right.profile_id,
                    conflict_kinds,
                    target: relationship.observation_target(),
                    weight: conflict_weight,
                    epoch,
                    candidate_edge_present: true,
                    count: conflicts,
                })?;
                *observed_candidate_conflicts
                    .entry(relationship)
                    .or_default() += conflicts;
                *observed_conflicts_by_profile
                    .entry(profile_pair)
                    .or_default() += conflicts;
                *observed_candidate_conflicts_by_profile
                    .entry(profile_pair)
                    .or_default() += conflicts;
            }
        }

        for (pair, conflict_kinds) in &observed {
            if !pair_in_evidence_scope(*pair, evidence_participants) {
                continue;
            }
            let left = candidate_transaction(candidate_graph, pair.0)?;
            let right = candidate_transaction(candidate_graph, pair.1)?;
            let profile_pair = canonical_profile_pair(left.profile_id, right.profile_id);
            let relationship = collector_relationship_for_profiles(profile_graph, profile_pair);
            let candidate_relationship = candidate_graph
                .candidate_provenance_between(pair.0, pair.1)
                .map(|provenance| collector_relationship_for_provenance(provenance, profile_pair));
            if let Some(edge) = candidate_graph.edge_between(pair.0, pair.1) {
                if edge.is_historical_override() {
                    if let EdgeProvenance::Static { profile_edge_index } = edge.provenance {
                        *miss_verification_conflicts
                            .entry(profile_edge_index)
                            .or_default() += 1;
                    }
                }
            }
            buffer.record_conflict(
                left.profile_id,
                right.profile_id,
                *conflict_kinds,
                relationship.observation_target(),
                conflict_weight,
                epoch,
                candidate_relationship.is_some(),
            )?;
            *observed_conflicts_by_profile
                .entry(profile_pair)
                .or_default() += 1;
            if let Some(candidate_relationship) = candidate_relationship {
                *observed_candidate_conflicts
                    .entry(candidate_relationship)
                    .or_default() += 1;
                *observed_candidate_conflicts_by_profile
                    .entry(profile_pair)
                    .or_default() += 1;
            }
        }

        for (relationship, total) in candidate_totals {
            let conflicts = observed_candidate_conflicts
                .get(&relationship)
                .copied()
                .unwrap_or(0);
            let independent = total.saturating_sub(conflicts);
            buffer.record_independent_count(
                relationship.source_profile(),
                relationship.target_profile(),
                relationship.observation_target(),
                independent_weight,
                epoch,
                independent,
            )?;
        }

        for (edge_index, (source_profile, target_profile, total)) in miss_verification_totals {
            let conflicts = miss_verification_conflicts
                .get(&edge_index)
                .copied()
                .unwrap_or(0);
            buffer.record_candidate_miss_verification(
                source_profile,
                target_profile,
                ObservationTarget::Static { edge_index },
                independent_weight,
                epoch,
                total.saturating_sub(conflicts),
                conflicts,
            )?;
        }

        let successful_buckets = bucket_successful_by_profile(candidate_graph, &successful)?;
        for fallback in feedback_store.fallback_edges() {
            let profile_pair = canonical_profile_pair(fallback.source, fallback.target);
            let eligible = eligible_pairs_for_profile_relationship(
                &successful_buckets,
                profile_pair,
                evidence_participants,
            );
            if eligible == 0 {
                continue;
            }
            let conflicts = observed_conflicts_by_profile
                .get(&profile_pair)
                .copied()
                .unwrap_or(0);
            let candidate_total = candidate_totals_by_profile
                .get(&profile_pair)
                .copied()
                .unwrap_or(0);
            let candidate_conflicts = observed_candidate_conflicts_by_profile
                .get(&profile_pair)
                .copied()
                .unwrap_or(0);
            let candidate_independent = candidate_total.saturating_sub(candidate_conflicts);
            let independent = eligible
                .saturating_sub(conflicts)
                .saturating_sub(candidate_independent);
            buffer.record_independent_count(
                profile_pair.0,
                profile_pair.1,
                ObservationTarget::RuntimeDiscovered,
                independent_weight,
                epoch,
                independent,
            )?;
        }

        Ok(buffer)
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_access_report(
        &self,
        profile_graph: &ProfileGraph,
        candidate_graph: &CandidateGraph,
        report: &BlockExecutionReport,
        feedback_store: &AdaptiveFeedbackStore,
        epoch: u64,
        observation_source: ObservationSource,
        conflict_weight: f64,
        independent_weight: f64,
        evidence_participants: Option<&BTreeSet<TxIndex>>,
    ) -> Result<ObservationBuffer, RuntimeFeedbackError> {
        let successful = successful_transactions(report)?;
        validate_report_candidate_alignment(candidate_graph, report)?;
        let observed = self.detector.detect(report)?;
        let observed_map: BTreeMap<_, _> = observed
            .iter()
            .map(|conflict| ((conflict.left, conflict.right), conflict.conflict_kinds))
            .collect();

        let mut buffer = ObservationBuffer::default();
        let mut compared_pairs = BTreeSet::<(TxIndex, TxIndex)>::new();

        for conflict in observed {
            let pair = canonical_tx_pair(conflict.left, conflict.right);
            if !pair_in_evidence_scope(pair, evidence_participants) {
                continue;
            }
            let left = candidate_transaction(candidate_graph, conflict.left)?;
            let right = candidate_transaction(candidate_graph, conflict.right)?;
            let candidate_present =
                candidate_graph.contains_candidate_pair(conflict.left, conflict.right);
            let target = static_or_runtime_target(profile_graph, left.profile_id, right.profile_id);
            let mut observation = ConflictObservation::conflict(
                left.profile_id,
                right.profile_id,
                left.tx_id,
                right.tx_id,
                conflict.conflict_kinds,
                observation_source,
                target,
                conflict_weight,
                epoch,
                candidate_present,
            )?;
            if candidate_graph
                .edge_between(conflict.left, conflict.right)
                .is_some_and(|edge| edge.is_historical_override())
            {
                observation = observation.with_candidate_miss_verification();
            }
            buffer.push(observation);
            compared_pairs.insert(canonical_tx_pair(conflict.left, conflict.right));
        }

        for edge in candidate_graph.edges() {
            if candidate_graph.provenance_is_compact(edge.provenance) {
                continue;
            }
            let pair = canonical_tx_pair(edge.source, edge.target);
            if !pair_in_evidence_scope(pair, evidence_participants)
                || !successful.contains(&pair.0)
                || !successful.contains(&pair.1)
                || observed_map.contains_key(&pair)
                || !compared_pairs.insert(pair)
            {
                continue;
            }
            let left = candidate_transaction(candidate_graph, pair.0)?;
            let right = candidate_transaction(candidate_graph, pair.1)?;
            let target = match edge.provenance {
                EdgeProvenance::Static { profile_edge_index } => ObservationTarget::Static {
                    edge_index: profile_edge_index,
                },
                EdgeProvenance::RuntimeDiscovered { .. } => ObservationTarget::RuntimeDiscovered,
            };
            let mut observation = ConflictObservation::independent(
                left.profile_id,
                right.profile_id,
                left.tx_id,
                right.tx_id,
                observation_source,
                target,
                independent_weight,
                epoch,
                true,
            )?;
            if edge.is_historical_override() {
                observation = observation.with_candidate_miss_verification();
            }
            buffer.push(observation);
        }

        for group in candidate_graph.compact_groups() {
            for (left_offset, &left_index) in group.members().iter().enumerate() {
                for &right_index in group.members().iter().skip(left_offset + 1) {
                    let pair = canonical_tx_pair(left_index, right_index);
                    if !pair_in_evidence_scope(pair, evidence_participants)
                        || !successful.contains(&pair.0)
                        || !successful.contains(&pair.1)
                        || observed_map.contains_key(&pair)
                        || !compared_pairs.insert(pair)
                    {
                        continue;
                    }
                    let left = candidate_transaction(candidate_graph, pair.0)?;
                    let right = candidate_transaction(candidate_graph, pair.1)?;
                    let target = match group.provenance() {
                        EdgeProvenance::Static { profile_edge_index } => {
                            ObservationTarget::Static {
                                edge_index: profile_edge_index,
                            }
                        }
                        EdgeProvenance::RuntimeDiscovered { .. } => {
                            ObservationTarget::RuntimeDiscovered
                        }
                    };
                    buffer.push(ConflictObservation::independent(
                        left.profile_id,
                        right.profile_id,
                        left.tx_id,
                        right.tx_id,
                        observation_source,
                        target,
                        independent_weight,
                        epoch,
                        true,
                    )?);
                }
            }
        }

        let successful_buckets = bucket_successful_by_profile(candidate_graph, &successful)?;
        for fallback in feedback_store.fallback_edges() {
            let source = successful_buckets.get(&fallback.source);
            let target = successful_buckets.get(&fallback.target);
            let (Some(source), Some(target)) = (source, target) else {
                continue;
            };
            if fallback.source == fallback.target {
                for (offset, left) in source.iter().enumerate() {
                    for right in source.iter().skip(offset + 1) {
                        maybe_add_fallback_negative(
                            candidate_graph,
                            &observed_map,
                            &mut compared_pairs,
                            &mut buffer,
                            *left,
                            *right,
                            observation_source,
                            independent_weight,
                            epoch,
                            evidence_participants,
                        )?;
                    }
                }
            } else {
                for left in source {
                    for right in target {
                        maybe_add_fallback_negative(
                            candidate_graph,
                            &observed_map,
                            &mut compared_pairs,
                            &mut buffer,
                            *left,
                            *right,
                            observation_source,
                            independent_weight,
                            epoch,
                            evidence_participants,
                        )?;
                    }
                }
            }
        }

        Ok(buffer)
    }

    pub fn collect_validation_aggregated(
        &self,
        profile_graph: &ProfileGraph,
        candidate_graph: &CandidateGraph,
        feedback_store: &AdaptiveFeedbackStore,
        evidence: &[ValidationEvidence],
        epoch: u64,
    ) -> Result<AggregatedObservationBuffer, RuntimeFeedbackError> {
        let mut buffer = AggregatedObservationBuffer::default();
        for item in evidence {
            let pair = canonical_tx_pair(item.predecessor, item.transaction);
            let left = candidate_transaction(candidate_graph, pair.0)?;
            let right = candidate_transaction(candidate_graph, pair.1)?;
            let candidate_present = candidate_graph.contains_candidate_pair(pair.0, pair.1);
            let static_edge =
                profile_graph.edge_between_profiles(left.profile_id, right.profile_id);
            let fallback_exists = feedback_store
                .fallback_edge(left.profile_id, right.profile_id)
                .is_some();

            match item.kind {
                ValidationEvidenceKind::Independent => {
                    let target = if let Some(edge_index) = static_edge {
                        ObservationTarget::Static { edge_index }
                    } else if fallback_exists {
                        ObservationTarget::RuntimeDiscovered
                    } else {
                        continue;
                    };
                    buffer.record_independent_count(
                        left.profile_id,
                        right.profile_id,
                        target,
                        self.weights.validation_independent,
                        epoch,
                        1,
                    )?;
                }
                ValidationEvidenceKind::Invalidated { conflict_kinds } => {
                    let target = static_edge
                        .map(|edge_index| ObservationTarget::Static { edge_index })
                        .unwrap_or(ObservationTarget::RuntimeDiscovered);
                    buffer.record_conflict(
                        left.profile_id,
                        right.profile_id,
                        conflict_kinds,
                        target,
                        self.weights.validation_conflict,
                        epoch,
                        candidate_present,
                    )?;
                }
                ValidationEvidenceKind::Replayed {
                    conflict_kinds,
                    replay_cost_nanos,
                    invalidated_descendants,
                } => {
                    let target = static_edge
                        .map(|edge_index| ObservationTarget::Static { edge_index })
                        .unwrap_or(ObservationTarget::RuntimeDiscovered);
                    buffer.record_conflict_with_replay(
                        left.profile_id,
                        right.profile_id,
                        conflict_kinds,
                        target,
                        self.weights.replay_conflict,
                        epoch,
                        candidate_present,
                        Some((replay_cost_nanos, invalidated_descendants)),
                    )?;
                }
            }
        }
        Ok(buffer)
    }

    pub fn collect_validation(
        &self,
        profile_graph: &ProfileGraph,
        candidate_graph: &CandidateGraph,
        feedback_store: &AdaptiveFeedbackStore,
        evidence: &[ValidationEvidence],
        epoch: u64,
    ) -> Result<ObservationBuffer, RuntimeFeedbackError> {
        let mut buffer = ObservationBuffer::default();
        for item in evidence {
            let pair = canonical_tx_pair(item.predecessor, item.transaction);
            let left = candidate_transaction(candidate_graph, pair.0)?;
            let right = candidate_transaction(candidate_graph, pair.1)?;
            let candidate_present = candidate_graph.contains_candidate_pair(pair.0, pair.1);
            let static_edge =
                profile_graph.edge_between_profiles(left.profile_id, right.profile_id);
            let fallback_exists = feedback_store
                .fallback_edge(left.profile_id, right.profile_id)
                .is_some();

            match item.kind {
                ValidationEvidenceKind::Independent => {
                    let target = if let Some(edge_index) = static_edge {
                        ObservationTarget::Static { edge_index }
                    } else if fallback_exists {
                        ObservationTarget::RuntimeDiscovered
                    } else {
                        // Explicit independence for a profile pair with no tracked relationship does
                        // not create a negative-only fallback edge.
                        continue;
                    };
                    buffer.push(ConflictObservation::independent(
                        left.profile_id,
                        right.profile_id,
                        left.tx_id,
                        right.tx_id,
                        ObservationSource::Validation,
                        target,
                        self.weights.validation_independent,
                        epoch,
                        candidate_present,
                    )?);
                }
                ValidationEvidenceKind::Invalidated { conflict_kinds } => {
                    let target = static_edge
                        .map(|edge_index| ObservationTarget::Static { edge_index })
                        .unwrap_or(ObservationTarget::RuntimeDiscovered);
                    buffer.push(ConflictObservation::conflict(
                        left.profile_id,
                        right.profile_id,
                        left.tx_id,
                        right.tx_id,
                        conflict_kinds,
                        ObservationSource::Validation,
                        target,
                        self.weights.validation_conflict,
                        epoch,
                        candidate_present,
                    )?);
                }
                ValidationEvidenceKind::Replayed {
                    conflict_kinds,
                    replay_cost_nanos,
                    invalidated_descendants,
                } => {
                    let target = static_edge
                        .map(|edge_index| ObservationTarget::Static { edge_index })
                        .unwrap_or(ObservationTarget::RuntimeDiscovered);
                    buffer.push(
                        ConflictObservation::conflict(
                            left.profile_id,
                            right.profile_id,
                            left.tx_id,
                            right.tx_id,
                            conflict_kinds,
                            ObservationSource::Replay,
                            target,
                            self.weights.replay_conflict,
                            epoch,
                            candidate_present,
                        )?
                        .with_replay_impact(replay_cost_nanos, invalidated_descendants),
                    );
                }
            }
        }
        Ok(buffer)
    }
}

fn successful_transactions(
    report: &BlockExecutionReport,
) -> Result<BTreeSet<TxIndex>, RuntimeFeedbackError> {
    report
        .transactions
        .iter()
        .filter(|execution| execution.result.is_ok())
        .map(|execution| {
            Ok(TxIndex(
                u32::try_from(execution.transaction_index).map_err(|_| {
                    RuntimeFeedbackError::TransactionIndexOverflow(execution.transaction_index)
                })?,
            ))
        })
        .collect()
}

fn validate_report_candidate_alignment(
    candidate_graph: &CandidateGraph,
    report: &BlockExecutionReport,
) -> Result<(), RuntimeFeedbackError> {
    let mut seen = BTreeSet::new();
    for execution in &report.transactions {
        if execution.transaction_index >= candidate_graph.transactions().len() {
            return Err(RuntimeFeedbackError::ExecutionIndexOutOfBounds {
                index: execution.transaction_index,
                candidate_count: candidate_graph.transactions().len(),
            });
        }
        if !seen.insert(execution.transaction_index) {
            return Err(RuntimeFeedbackError::DuplicateExecutionIndex(
                execution.transaction_index,
            ));
        }
        let candidate = &candidate_graph.transactions()[execution.transaction_index];
        if candidate.tx_id.0 != execution.transaction_id.0 {
            return Err(RuntimeFeedbackError::TransactionIdMismatch {
                index: execution.transaction_index,
                candidate: candidate.tx_id.0,
                execution: execution.transaction_id.0,
            });
        }
    }
    Ok(())
}

fn static_or_runtime_target(
    graph: &ProfileGraph,
    left: ProfileId,
    right: ProfileId,
) -> ObservationTarget {
    graph
        .edge_between_profiles(left, right)
        .map(|edge_index| ObservationTarget::Static { edge_index })
        .unwrap_or(ObservationTarget::RuntimeDiscovered)
}

fn candidate_transaction(
    graph: &CandidateGraph,
    index: TxIndex,
) -> Result<&acg_candidate_graph::CandidateTransaction, RuntimeFeedbackError> {
    graph
        .transaction(index)
        .ok_or(RuntimeFeedbackError::CandidateTransactionMissing(index))
}

fn bucket_successful_by_profile(
    graph: &CandidateGraph,
    successful: &BTreeSet<TxIndex>,
) -> Result<BTreeMap<ProfileId, Vec<TxIndex>>, RuntimeFeedbackError> {
    let mut buckets = BTreeMap::<ProfileId, Vec<TxIndex>>::new();
    for index in successful {
        let transaction = candidate_transaction(graph, *index)?;
        buckets
            .entry(transaction.profile_id)
            .or_default()
            .push(*index);
    }
    Ok(buckets)
}

fn eligible_pairs_for_profile_relationship(
    buckets: &BTreeMap<ProfileId, Vec<TxIndex>>,
    profile_pair: (ProfileId, ProfileId),
    evidence_participants: Option<&BTreeSet<TxIndex>>,
) -> usize {
    let Some(source) = buckets.get(&profile_pair.0) else {
        return 0;
    };
    let Some(target) = buckets.get(&profile_pair.1) else {
        return 0;
    };

    let participant_count = |bucket: &[TxIndex]| -> usize {
        evidence_participants.map_or(0, |participants| {
            bucket
                .iter()
                .filter(|index| participants.contains(index))
                .count()
        })
    };

    if profile_pair.0 == profile_pair.1 {
        let total = choose_two(source.len());
        match evidence_participants {
            None => total,
            Some(_) => {
                let participants = participant_count(source);
                total.saturating_sub(choose_two(source.len().saturating_sub(participants)))
            }
        }
    } else {
        let total = source.len().saturating_mul(target.len());
        match evidence_participants {
            None => total,
            Some(_) => {
                let source_participants = participant_count(source);
                let target_participants = participant_count(target);
                let outside = source
                    .len()
                    .saturating_sub(source_participants)
                    .saturating_mul(target.len().saturating_sub(target_participants));
                total.saturating_sub(outside)
            }
        }
    }
}

fn eligible_pairs_within_group(
    members: &[TxIndex],
    evidence_participants: Option<&BTreeSet<TxIndex>>,
) -> usize {
    let total = choose_two(members.len());
    match evidence_participants {
        None => total,
        Some(participants) => {
            let participant_count = members
                .iter()
                .filter(|index| participants.contains(index))
                .count();
            total.saturating_sub(choose_two(members.len().saturating_sub(participant_count)))
        }
    }
}

fn choose_two(count: usize) -> usize {
    count.saturating_mul(count.saturating_sub(1)) / 2
}

fn pair_in_evidence_scope(
    pair: (TxIndex, TxIndex),
    evidence_participants: Option<&BTreeSet<TxIndex>>,
) -> bool {
    match evidence_participants {
        Some(participants) => participants.contains(&pair.0) || participants.contains(&pair.1),
        None => true,
    }
}

#[allow(clippy::too_many_arguments)]
fn maybe_add_fallback_negative(
    graph: &CandidateGraph,
    observed: &BTreeMap<(TxIndex, TxIndex), ConflictKinds>,
    compared: &mut BTreeSet<(TxIndex, TxIndex)>,
    buffer: &mut ObservationBuffer,
    left_index: TxIndex,
    right_index: TxIndex,
    observation_source: ObservationSource,
    weight: f64,
    epoch: u64,
    evidence_participants: Option<&BTreeSet<TxIndex>>,
) -> Result<(), RuntimeFeedbackError> {
    let pair = canonical_tx_pair(left_index, right_index);
    if !pair_in_evidence_scope(pair, evidence_participants)
        || observed.contains_key(&pair)
        || !compared.insert(pair)
    {
        return Ok(());
    }
    let left = candidate_transaction(graph, pair.0)?;
    let right = candidate_transaction(graph, pair.1)?;
    buffer.push(ConflictObservation::independent(
        left.profile_id,
        right.profile_id,
        left.tx_id,
        right.tx_id,
        observation_source,
        ObservationTarget::RuntimeDiscovered,
        weight,
        epoch,
        graph.contains_candidate_pair(pair.0, pair.1),
    )?);
    Ok(())
}

/// Convenience facade that owns both collection and adaptive state.
pub struct RuntimeFeedbackEngine {
    collector: BlockFeedbackCollector,
    store: AdaptiveFeedbackStore,
    adaptive_config: AdaptiveFeedbackConfig,
}

impl RuntimeFeedbackEngine {
    pub fn new(
        profile_graph: &ProfileGraph,
        initial_epoch: u64,
        trace_config: TraceConflictConfig,
        weights: RuntimeFeedbackWeights,
        adaptive_config: AdaptiveFeedbackConfig,
    ) -> Result<Self, RuntimeFeedbackError> {
        adaptive_config.validate()?;
        Ok(Self {
            collector: BlockFeedbackCollector::new(trace_config, weights)?,
            store: AdaptiveFeedbackStore::from_graph(profile_graph, initial_epoch)?,
            adaptive_config,
        })
    }

    pub fn restore(
        profile_graph: &ProfileGraph,
        checkpoint: FeedbackCheckpoint,
        initial_epoch: u64,
        trace_config: TraceConflictConfig,
        weights: RuntimeFeedbackWeights,
        adaptive_config: AdaptiveFeedbackConfig,
    ) -> Result<Self, RuntimeFeedbackError> {
        adaptive_config.validate()?;
        Ok(Self {
            collector: BlockFeedbackCollector::new(trace_config, weights)?,
            store: AdaptiveFeedbackStore::restore(profile_graph, checkpoint, initial_epoch)?,
            adaptive_config,
        })
    }

    pub fn store(&self) -> &AdaptiveFeedbackStore {
        &self.store
    }

    pub fn adaptive_config(&self) -> &AdaptiveFeedbackConfig {
        &self.adaptive_config
    }

    /// Rapidly weaken stale probability/replay/serialization evidence after a detected workload
    /// regime change while preserving cumulative diagnostics.
    pub fn decay_for_regime_change(
        &mut self,
        epoch: u64,
        retained_evidence: f64,
    ) -> Result<(), RuntimeFeedbackError> {
        self.store
            .decay_for_regime_change(epoch, retained_evidence, &self.adaptive_config)?;
        Ok(())
    }

    pub fn process_pre_execution(
        &mut self,
        profile_graph: &ProfileGraph,
        candidate_graph: &CandidateGraph,
        report: &BlockExecutionReport,
        epoch: u64,
    ) -> Result<ApplySummary, RuntimeFeedbackError> {
        let observations = self.collector.collect_pre_execution_aggregated(
            profile_graph,
            candidate_graph,
            report,
            &self.store,
            epoch,
        )?;
        Ok(self
            .store
            .apply_aggregated_batch(profile_graph, observations, &self.adaptive_config)?)
    }

    pub fn process_block(
        &mut self,
        profile_graph: &ProfileGraph,
        candidate_graph: &CandidateGraph,
        report: &BlockExecutionReport,
        epoch: u64,
    ) -> Result<ApplySummary, RuntimeFeedbackError> {
        let observations = self.collector.collect_block_aggregated(
            profile_graph,
            candidate_graph,
            report,
            &self.store,
            epoch,
        )?;
        Ok(self
            .store
            .apply_aggregated_batch(profile_graph, observations, &self.adaptive_config)?)
    }

    pub fn process_replay_execution(
        &mut self,
        profile_graph: &ProfileGraph,
        candidate_graph: &CandidateGraph,
        report: &BlockExecutionReport,
        replayed_transactions: &BTreeSet<TxIndex>,
        epoch: u64,
    ) -> Result<ApplySummary, RuntimeFeedbackError> {
        let observations = self.collector.collect_replay_execution_aggregated(
            profile_graph,
            candidate_graph,
            report,
            replayed_transactions,
            &self.store,
            epoch,
        )?;
        Ok(self
            .store
            .apply_aggregated_batch(profile_graph, observations, &self.adaptive_config)?)
    }

    pub fn process_validation(
        &mut self,
        profile_graph: &ProfileGraph,
        candidate_graph: &CandidateGraph,
        evidence: &[ValidationEvidence],
        epoch: u64,
    ) -> Result<ApplySummary, RuntimeFeedbackError> {
        let observations = self.collector.collect_validation_aggregated(
            profile_graph,
            candidate_graph,
            &self.store,
            evidence,
            epoch,
        )?;
        Ok(self
            .store
            .apply_aggregated_batch(profile_graph, observations, &self.adaptive_config)?)
    }

    /// Apply Phase 5E dependency serialization-cost evidence.
    ///
    /// The evidence is local performance data only. It changes future speculative scheduling
    /// policy but never validation or canonical state.
    pub fn process_serialization_costs(
        &mut self,
        candidate_graph: &CandidateGraph,
        evidence: &[SerializationCostEvidence],
        epoch: u64,
    ) -> Result<ApplySummary, RuntimeFeedbackError> {
        let mut buffer = AggregatedSerializationCostBuffer::default();
        for item in evidence {
            let edge = candidate_graph
                .edge_between(item.predecessor, item.transaction)
                .ok_or(
                    RuntimeFeedbackError::SerializationEvidenceMissingCandidateEdge {
                        predecessor: item.predecessor,
                        transaction: item.transaction,
                    },
                )?;
            buffer.record(edge.provenance, item.marginal_ready_delay_nanos);
        }
        self.process_serialization_cost_aggregates(buffer, epoch)
    }

    /// Apply serialization evidence that was already grouped by learned relationship upstream.
    pub fn process_serialization_cost_aggregates(
        &mut self,
        buffer: AggregatedSerializationCostBuffer,
        epoch: u64,
    ) -> Result<ApplySummary, RuntimeFeedbackError> {
        let mut summary = ApplySummary::default();
        for (provenance, (total_cost_nanos, observations)) in buffer.batches {
            let applied = match provenance {
                EdgeProvenance::Static { profile_edge_index } => {
                    self.store.record_static_serialization_cost_batch(
                        profile_edge_index,
                        total_cost_nanos,
                        observations,
                        epoch,
                        &self.adaptive_config,
                    )?
                }
                EdgeProvenance::RuntimeDiscovered { runtime_edge_id } => {
                    self.store.record_fallback_serialization_cost_batch(
                        runtime_edge_id,
                        total_cost_nanos,
                        observations,
                        epoch,
                        &self.adaptive_config,
                    )?
                }
            };
            summary.serialization_cost_observations = summary
                .serialization_cost_observations
                .saturating_add(applied.serialization_cost_observations);
            summary.attributed_serialization_cost_nanos = summary
                .attributed_serialization_cost_nanos
                .saturating_add(applied.attributed_serialization_cost_nanos);
            summary.serialization_cost_batches_applied = summary
                .serialization_cost_batches_applied
                .saturating_add(applied.serialization_cost_batches_applied);
        }
        Ok(summary)
    }

    pub fn checkpoint(
        &self,
        profile_graph: &ProfileGraph,
    ) -> Result<FeedbackCheckpoint, RuntimeFeedbackError> {
        Ok(self.store.checkpoint(profile_graph)?)
    }
}

#[derive(Debug, Error)]
pub enum RuntimeFeedbackError {
    #[error("feedback weight {name} must be finite and positive, got {value}")]
    InvalidWeight { name: String, value: f64 },
    #[error("transaction index {0} cannot be represented as TxIndex")]
    TransactionIndexOverflow(usize),
    #[error("execution report contains duplicate transaction index {0}")]
    DuplicateExecutionIndex(usize),
    #[error(
        "execution transaction ID {execution} does not match outcome transaction ID {outcome}"
    )]
    OutcomeTransactionIdMismatch { execution: u64, outcome: u64 },
    #[error("execution transaction ID {execution} does not match access transaction ID {access}")]
    AccessTransactionIdMismatch { execution: u64, access: u64 },
    #[error(
        "execution index {index} is outside candidate graph with {candidate_count} transactions"
    )]
    ExecutionIndexOutOfBounds {
        index: usize,
        candidate_count: usize,
    },
    #[error(
        "transaction ID mismatch at index {index}: candidate={candidate}, execution={execution}"
    )]
    TransactionIdMismatch {
        index: usize,
        candidate: u64,
        execution: u64,
    },
    #[error("candidate transaction {0:?} is missing")]
    CandidateTransactionMissing(TxIndex),
    #[error("successful transaction {0:?} is missing a conflict footprint")]
    MissingConflictFootprint(TxIndex),
    #[error(
        "serialization-cost evidence for {predecessor:?} -> {transaction:?} has no candidate edge"
    )]
    SerializationEvidenceMissingCandidateEdge {
        predecessor: TxIndex,
        transaction: TxIndex,
    },
    #[error(transparent)]
    Feedback(#[from] FeedbackError),
}
