use acg_candidate_graph::{
    CandidateGraph, CandidateGraphBuilder, CandidateTransaction, WeightedCandidateGraphConfig,
};
use acg_core::{ConflictKinds, ContractCodeHash, InstanceId, RuntimeId, TxId, TxIndex};
use acg_cosmwasm_engine::{
    AccessKind, AccessRecord, Address, EngineError, ExecutionOutcome, TransactionId,
};
use acg_feedback::{
    AdaptiveFeedbackConfig, AdaptiveFeedbackStore, ObservationOutcome, ObservationSource,
    ObservationTarget,
};
use acg_predicate::InputBindings;
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_runtime_feedback::{
    AccessConflictDetector, BlockFeedbackCollector, RuntimeFeedbackWeights, TraceConflictConfig,
    ValidationEvidence, ValidationEvidenceKind,
};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use acg_validator_sim::{BlockExecutionReport, TransactionExecution};
use serde_json::json;

const CONFLICTLAB: &[u8] =
    include_bytes!("../../../../benchmarks/symbolic/conflictlab.symbolic.json");

fn profile_graph() -> ProfileGraph {
    let raw = parse_slice(CONFLICTLAB).unwrap();
    let context = IngestionContext::new(
        RuntimeId::new("cosmwasm").unwrap(),
        ContractCodeHash([4; 32]),
        1,
    );
    let profiles = normalize_document(raw, &context).unwrap();
    let artifact = ProfileGraphArtifact::compile(profiles, &EdgeBuildConfig::default()).unwrap();
    ProfileGraph::load(artifact, GraphLoadConfig::default()).unwrap()
}

fn profile_id(graph: &ProfileGraph, entrypoint: &str) -> acg_core::ProfileId {
    graph
        .profiles()
        .iter()
        .find(|profile| profile.definition.entrypoint_name == entrypoint)
        .unwrap()
        .id
}

fn candidate_tx(
    graph: &ProfileGraph,
    id: u64,
    entrypoint: &str,
    bindings: serde_json::Value,
) -> CandidateTransaction {
    CandidateTransaction {
        tx_id: TxId(id),
        predicted_position: id as u32,
        inclusion_probability: 1.0,
        profile_id: profile_id(graph, entrypoint),
        instance_id: InstanceId(1),
        input_bindings: InputBindings::from_value(bindings),
        estimated_execution_cost: 1,
    }
}

fn candidate_graph(
    graph: &ProfileGraph,
    transactions: Vec<CandidateTransaction>,
) -> CandidateGraph {
    CandidateGraphBuilder::new(graph)
        .build(transactions)
        .unwrap()
}

fn access(
    transaction_id: u64,
    contract: &str,
    kind: AccessKind,
    key: &[u8],
    range_end: Option<&[u8]>,
    reverted: bool,
) -> AccessRecord {
    AccessRecord {
        transaction_id: TransactionId(transaction_id),
        call_depth: 0,
        contract: Address::new(contract),
        kind,
        key: key.to_vec(),
        range_end: range_end.map(ToOwned::to_owned),
        value: None,
        reverted,
    }
}

fn successful_execution(
    index: usize,
    transaction_id: u64,
    accesses: Vec<AccessRecord>,
) -> TransactionExecution {
    TransactionExecution {
        transaction_index: index,
        transaction_id: TransactionId(transaction_id),
        result: Ok(ExecutionOutcome {
            transaction_id: TransactionId(transaction_id),
            contract: Address::new("contract-a"),
            events: Vec::new(),
            data: None,
            accesses,
            created_contracts: Vec::new(),
        }),
    }
}

fn report(transactions: Vec<TransactionExecution>) -> BlockExecutionReport {
    BlockExecutionReport {
        block_height: 7,
        block_time_nanos: 14_000_000_000,
        transactions,
    }
}

fn collector() -> BlockFeedbackCollector {
    BlockFeedbackCollector::new(
        TraceConflictConfig::default(),
        RuntimeFeedbackWeights::default(),
    )
    .unwrap()
}

#[test]
fn detector_finds_rw_and_ww_but_not_read_read() {
    let report = report(vec![
        successful_execution(
            0,
            1,
            vec![access(
                1,
                "contract-a",
                AccessKind::StorageRead,
                b"k",
                None,
                false,
            )],
        ),
        successful_execution(
            1,
            2,
            vec![access(
                2,
                "contract-a",
                AccessKind::StorageWrite,
                b"k",
                None,
                false,
            )],
        ),
        successful_execution(
            2,
            3,
            vec![access(
                3,
                "contract-a",
                AccessKind::StorageRead,
                b"k",
                None,
                false,
            )],
        ),
        successful_execution(
            3,
            4,
            vec![access(
                4,
                "contract-a",
                AccessKind::StorageWrite,
                b"k",
                None,
                false,
            )],
        ),
    ]);
    let conflicts = AccessConflictDetector::new(TraceConflictConfig::default())
        .detect(&report)
        .unwrap();
    assert!(conflicts.iter().any(|conflict| {
        conflict.left == TxIndex(0)
            && conflict.right == TxIndex(1)
            && conflict.conflict_kinds.contains(ConflictKinds::READ_WRITE)
    }));
    assert!(conflicts.iter().any(|conflict| {
        conflict.left == TxIndex(1)
            && conflict.right == TxIndex(3)
            && conflict.conflict_kinds.contains(ConflictKinds::WRITE_WRITE)
    }));
    assert!(!conflicts
        .iter()
        .any(|conflict| conflict.left == TxIndex(0) && conflict.right == TxIndex(2)));
}

#[test]
fn storage_is_contract_local_but_bank_keys_are_global() {
    let storage = report(vec![
        successful_execution(
            0,
            1,
            vec![access(
                1,
                "contract-a",
                AccessKind::StorageWrite,
                b"same",
                None,
                false,
            )],
        ),
        successful_execution(
            1,
            2,
            vec![access(
                2,
                "contract-b",
                AccessKind::StorageRead,
                b"same",
                None,
                false,
            )],
        ),
    ]);
    assert!(AccessConflictDetector::new(TraceConflictConfig::default())
        .detect(&storage)
        .unwrap()
        .is_empty());

    let bank = report(vec![
        successful_execution(
            0,
            1,
            vec![access(
                1,
                "contract-a",
                AccessKind::BankWrite,
                b"alice\0inj",
                None,
                false,
            )],
        ),
        successful_execution(
            1,
            2,
            vec![access(
                2,
                "contract-b",
                AccessKind::BankRead,
                b"alice\0inj",
                None,
                false,
            )],
        ),
    ]);
    let bank_conflicts = AccessConflictDetector::new(TraceConflictConfig::default())
        .detect(&bank)
        .unwrap();
    assert_eq!(bank_conflicts.len(), 1);
    assert!(bank_conflicts[0]
        .conflict_kinds
        .contains(ConflictKinds::BALANCE));
}

#[test]
fn storage_scans_conflict_only_with_writes_inside_the_range() {
    let report = report(vec![
        successful_execution(
            0,
            1,
            vec![access(
                1,
                "contract-a",
                AccessKind::StorageScan,
                b"a",
                Some(b"m"),
                false,
            )],
        ),
        successful_execution(
            1,
            2,
            vec![access(
                2,
                "contract-a",
                AccessKind::StorageWrite,
                b"k",
                None,
                false,
            )],
        ),
        successful_execution(
            2,
            3,
            vec![access(
                3,
                "contract-a",
                AccessKind::StorageWrite,
                b"z",
                None,
                false,
            )],
        ),
    ]);
    let conflicts = AccessConflictDetector::new(TraceConflictConfig::default())
        .detect(&report)
        .unwrap();
    assert!(conflicts
        .iter()
        .any(|conflict| conflict.left == TxIndex(0) && conflict.right == TxIndex(1)));
    assert!(!conflicts
        .iter()
        .any(|conflict| conflict.left == TxIndex(0) && conflict.right == TxIndex(2)));
}

#[test]
fn reverted_accesses_are_excluded_by_default_and_can_be_included_for_auditing() {
    let report = report(vec![
        successful_execution(
            0,
            1,
            vec![access(
                1,
                "contract-a",
                AccessKind::StorageWrite,
                b"k",
                None,
                true,
            )],
        ),
        successful_execution(
            1,
            2,
            vec![access(
                2,
                "contract-a",
                AccessKind::StorageRead,
                b"k",
                None,
                false,
            )],
        ),
    ]);
    assert!(AccessConflictDetector::new(TraceConflictConfig::default())
        .detect(&report)
        .unwrap()
        .is_empty());
    assert_eq!(
        AccessConflictDetector::new(TraceConflictConfig {
            include_reverted_accesses: true,
        })
        .detect(&report)
        .unwrap()
        .len(),
        1
    );
}

#[test]
fn collector_emits_positive_and_explicit_negative_evidence_for_candidate_edges() {
    let graph = profile_graph();
    let candidate = candidate_graph(
        &graph,
        vec![
            candidate_tx(&graph, 1, "execute::Credit", json!({"account":"alice"})),
            candidate_tx(&graph, 2, "execute::Credit", json!({"account":"alice"})),
        ],
    );
    assert_eq!(candidate.edges().len(), 1);
    let store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();

    let positive_report = report(vec![
        successful_execution(
            0,
            1,
            vec![access(
                1,
                "contract-a",
                AccessKind::StorageWrite,
                b"balance/alice",
                None,
                false,
            )],
        ),
        successful_execution(
            1,
            2,
            vec![access(
                2,
                "contract-a",
                AccessKind::StorageRead,
                b"balance/alice",
                None,
                false,
            )],
        ),
    ]);
    let positive = collector()
        .collect_block(&graph, &candidate, &positive_report, &store, 7)
        .unwrap();
    assert_eq!(positive.len(), 1);
    assert!(matches!(
        positive.observations()[0].outcome,
        ObservationOutcome::Conflict { .. }
    ));

    let negative_report = report(vec![
        successful_execution(
            0,
            1,
            vec![access(
                1,
                "contract-a",
                AccessKind::StorageWrite,
                b"x",
                None,
                false,
            )],
        ),
        successful_execution(
            1,
            2,
            vec![access(
                2,
                "contract-a",
                AccessKind::StorageRead,
                b"y",
                None,
                false,
            )],
        ),
    ]);
    let negative = collector()
        .collect_block(&graph, &candidate, &negative_report, &store, 8)
        .unwrap();
    assert_eq!(negative.len(), 1);
    assert_eq!(
        negative.observations()[0].outcome,
        ObservationOutcome::Independent
    );
}

#[test]
fn unrelated_successful_transactions_without_overlap_are_not_negative_evidence() {
    let graph = profile_graph();
    let candidate = candidate_graph(
        &graph,
        vec![
            candidate_tx(&graph, 1, "execute::Credit", json!({"account":"alice"})),
            candidate_tx(
                &graph,
                2,
                "execute::IncrementCounter",
                json!({"shard_id":1}),
            ),
        ],
    );
    assert!(candidate.edges().is_empty());
    let store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let report = report(vec![
        successful_execution(
            0,
            1,
            vec![access(
                1,
                "contract-a",
                AccessKind::StorageWrite,
                b"balance/alice",
                None,
                false,
            )],
        ),
        successful_execution(
            1,
            2,
            vec![access(
                2,
                "contract-a",
                AccessKind::StorageWrite,
                b"counter/1",
                None,
                false,
            )],
        ),
    ]);
    let observations = collector()
        .collect_block(&graph, &candidate, &report, &store, 7)
        .unwrap();
    assert!(observations.is_empty());
}

#[test]
fn concrete_overlap_without_static_edge_creates_runtime_discovered_evidence() {
    let graph = profile_graph();
    let candidate = candidate_graph(
        &graph,
        vec![
            candidate_tx(&graph, 1, "execute::Credit", json!({"account":"alice"})),
            candidate_tx(
                &graph,
                2,
                "execute::IncrementCounter",
                json!({"shard_id":1}),
            ),
        ],
    );
    let credit = profile_id(&graph, "execute::Credit");
    let counter = profile_id(&graph, "execute::IncrementCounter");
    assert!(graph.edge_between_profiles(credit, counter).is_none());
    let mut store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let report = report(vec![
        successful_execution(
            0,
            1,
            vec![access(
                1,
                "contract-a",
                AccessKind::StorageWrite,
                b"unexpected",
                None,
                false,
            )],
        ),
        successful_execution(
            1,
            2,
            vec![access(
                2,
                "contract-a",
                AccessKind::StorageRead,
                b"unexpected",
                None,
                false,
            )],
        ),
    ]);
    let observations = collector()
        .collect_block(&graph, &candidate, &report, &store, 9)
        .unwrap();
    assert_eq!(observations.len(), 1);
    assert_eq!(
        observations.observations()[0].target,
        ObservationTarget::RuntimeDiscovered
    );
    assert!(!observations.observations()[0].candidate_edge_present);
    let summary = store
        .apply_batch(&graph, observations, &AdaptiveFeedbackConfig::default())
        .unwrap();
    assert_eq!(summary.fallback_edges_created, 1);
    assert!(store.fallback_edge(credit, counter).is_some());
}

#[test]
fn static_predicate_miss_updates_the_existing_profile_edge() {
    let graph = profile_graph();
    let candidate = candidate_graph(
        &graph,
        vec![
            candidate_tx(&graph, 1, "execute::Credit", json!({"account":"alice"})),
            candidate_tx(&graph, 2, "execute::Credit", json!({"account":"bob"})),
        ],
    );
    assert!(candidate.edges().is_empty());
    let store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let report = report(vec![
        successful_execution(
            0,
            1,
            vec![access(
                1,
                "contract-a",
                AccessKind::StorageWrite,
                b"same",
                None,
                false,
            )],
        ),
        successful_execution(
            1,
            2,
            vec![access(
                2,
                "contract-a",
                AccessKind::StorageRead,
                b"same",
                None,
                false,
            )],
        ),
    ]);
    let observations = collector()
        .collect_block(&graph, &candidate, &report, &store, 10)
        .unwrap();
    assert_eq!(observations.len(), 1);
    assert!(matches!(
        observations.observations()[0].target,
        ObservationTarget::Static { .. }
    ));
    assert!(!observations.observations()[0].candidate_edge_present);
}

#[test]
fn failed_transactions_do_not_become_false_negative_evidence() {
    let graph = profile_graph();
    let candidate = candidate_graph(
        &graph,
        vec![
            candidate_tx(&graph, 1, "execute::Credit", json!({"account":"alice"})),
            candidate_tx(&graph, 2, "execute::Credit", json!({"account":"alice"})),
        ],
    );
    let store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let report = report(vec![
        successful_execution(0, 1, Vec::new()),
        TransactionExecution {
            transaction_index: 1,
            transaction_id: TransactionId(2),
            result: Err(EngineError::Contract("failed".to_owned())),
        },
    ]);
    let observations = collector()
        .collect_block(&graph, &candidate, &report, &store, 11)
        .unwrap();
    assert!(observations.is_empty());
}

#[test]
fn an_existing_fallback_pair_collects_future_independence() {
    let graph = profile_graph();
    let candidate = candidate_graph(
        &graph,
        vec![
            candidate_tx(&graph, 1, "execute::Credit", json!({"account":"alice"})),
            candidate_tx(
                &graph,
                2,
                "execute::IncrementCounter",
                json!({"shard_id":1}),
            ),
        ],
    );
    let mut store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let overlap = report(vec![
        successful_execution(
            0,
            1,
            vec![access(
                1,
                "contract-a",
                AccessKind::StorageWrite,
                b"miss",
                None,
                false,
            )],
        ),
        successful_execution(
            1,
            2,
            vec![access(
                2,
                "contract-a",
                AccessKind::StorageRead,
                b"miss",
                None,
                false,
            )],
        ),
    ]);
    let first = collector()
        .collect_block(&graph, &candidate, &overlap, &store, 1)
        .unwrap();
    store
        .apply_batch(&graph, first, &AdaptiveFeedbackConfig::default())
        .unwrap();

    let independent = report(vec![
        successful_execution(
            0,
            1,
            vec![access(
                1,
                "contract-a",
                AccessKind::StorageWrite,
                b"a",
                None,
                false,
            )],
        ),
        successful_execution(
            1,
            2,
            vec![access(
                2,
                "contract-a",
                AccessKind::StorageRead,
                b"b",
                None,
                false,
            )],
        ),
    ]);
    let second = collector()
        .collect_block(&graph, &candidate, &independent, &store, 2)
        .unwrap();
    assert_eq!(second.len(), 1);
    assert_eq!(
        second.observations()[0].outcome,
        ObservationOutcome::Independent
    );
    assert_eq!(
        second.observations()[0].target,
        ObservationTarget::RuntimeDiscovered
    );
}

#[test]
fn weighted_runtime_fallback_edge_collects_independence_for_the_fallback_target() {
    let graph = profile_graph();
    let transactions = vec![
        candidate_tx(&graph, 1, "execute::Credit", json!({"account":"alice"})),
        candidate_tx(
            &graph,
            2,
            "execute::IncrementCounter",
            json!({"shard_id":1}),
        ),
    ];
    let initial_candidate = candidate_graph(&graph, transactions.clone());
    assert!(initial_candidate.edges().is_empty());
    let mut store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let adaptive_config = AdaptiveFeedbackConfig {
        retention_factor: 1.0,
        ..AdaptiveFeedbackConfig::default()
    };
    let overlap = report(vec![
        successful_execution(
            0,
            1,
            vec![access(
                1,
                "contract-a",
                AccessKind::StorageWrite,
                b"miss",
                None,
                false,
            )],
        ),
        successful_execution(
            1,
            2,
            vec![access(
                2,
                "contract-a",
                AccessKind::StorageRead,
                b"miss",
                None,
                false,
            )],
        ),
    ]);
    let observations = collector()
        .collect_block(&graph, &initial_candidate, &overlap, &store, 1)
        .unwrap();
    store
        .apply_batch(&graph, observations, &adaptive_config)
        .unwrap();

    let weighted = CandidateGraphBuilder::new(&graph)
        .build_weighted(
            transactions,
            &store,
            &adaptive_config,
            WeightedCandidateGraphConfig {
                epoch: 1,
                edge_materialization_threshold: 0.0,
            },
        )
        .unwrap();
    assert_eq!(weighted.edges().len(), 1);
    assert!(weighted.edges()[0].runtime_edge_id().is_some());

    let independent = report(vec![
        successful_execution(
            0,
            1,
            vec![access(
                1,
                "contract-a",
                AccessKind::StorageWrite,
                b"a",
                None,
                false,
            )],
        ),
        successful_execution(
            1,
            2,
            vec![access(
                2,
                "contract-a",
                AccessKind::StorageRead,
                b"b",
                None,
                false,
            )],
        ),
    ]);
    let negative = collector()
        .collect_block(&graph, &weighted, &independent, &store, 2)
        .unwrap();
    assert_eq!(negative.len(), 1);
    assert_eq!(
        negative.observations()[0].outcome,
        ObservationOutcome::Independent
    );
    assert_eq!(
        negative.observations()[0].target,
        ObservationTarget::RuntimeDiscovered
    );
    assert!(negative.observations()[0].candidate_edge_present);
}

#[test]
fn validation_and_replay_events_use_stronger_explicit_sources() {
    let graph = profile_graph();
    let candidate = candidate_graph(
        &graph,
        vec![
            candidate_tx(&graph, 1, "execute::Credit", json!({"account":"alice"})),
            candidate_tx(&graph, 2, "execute::Credit", json!({"account":"alice"})),
            candidate_tx(
                &graph,
                3,
                "execute::IncrementCounter",
                json!({"shard_id":1}),
            ),
        ],
    );
    let store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let observations = collector()
        .collect_validation(
            &graph,
            &candidate,
            &store,
            &[
                ValidationEvidence {
                    predecessor: TxIndex(0),
                    transaction: TxIndex(1),
                    kind: ValidationEvidenceKind::Independent,
                },
                ValidationEvidence {
                    predecessor: TxIndex(0),
                    transaction: TxIndex(1),
                    kind: ValidationEvidenceKind::Invalidated {
                        conflict_kinds: ConflictKinds::WRITE_WRITE,
                    },
                },
                ValidationEvidence {
                    predecessor: TxIndex(0),
                    transaction: TxIndex(2),
                    kind: ValidationEvidenceKind::Replayed {
                        conflict_kinds: ConflictKinds::WRITE_READ,
                    },
                },
            ],
            20,
        )
        .unwrap();
    assert_eq!(observations.len(), 3);
    assert_eq!(
        observations.observations()[0].observation_source,
        ObservationSource::Validation
    );
    assert_eq!(
        observations.observations()[1].observation_source,
        ObservationSource::Validation
    );
    assert!(matches!(
        observations.observations()[1].outcome,
        ObservationOutcome::Conflict { .. }
    ));
    assert_eq!(
        observations.observations()[2].observation_source,
        ObservationSource::Replay
    );
    assert_eq!(observations.observations()[2].weight, 4.0);
    assert_eq!(
        observations.observations()[2].target,
        ObservationTarget::RuntimeDiscovered
    );
}

#[test]
fn speculative_pre_execution_uses_lower_default_weight_than_canonical_evidence() {
    let graph = profile_graph();
    let candidate = candidate_graph(
        &graph,
        vec![
            candidate_tx(&graph, 1, "execute::Credit", json!({"account":"alice"})),
            candidate_tx(&graph, 2, "execute::Credit", json!({"account":"alice"})),
        ],
    );
    let store = AdaptiveFeedbackStore::from_graph(&graph, 0).unwrap();
    let report = report(vec![
        successful_execution(
            0,
            1,
            vec![access(
                1,
                "contract-a",
                AccessKind::StorageWrite,
                b"same",
                None,
                false,
            )],
        ),
        successful_execution(
            1,
            2,
            vec![access(
                2,
                "contract-a",
                AccessKind::StorageRead,
                b"same",
                None,
                false,
            )],
        ),
    ]);
    let collector = collector();
    let pre = collector
        .collect_pre_execution(&graph, &candidate, &report, &store, 1)
        .unwrap();
    let canonical = collector
        .collect_block(&graph, &candidate, &report, &store, 1)
        .unwrap();
    assert_eq!(
        pre.observations()[0].observation_source,
        ObservationSource::PreExecution
    );
    assert_eq!(pre.observations()[0].weight, 1.0);
    assert_eq!(canonical.observations()[0].weight, 3.0);
}

#[test]
fn runtime_feedback_engine_batches_updates_and_restores_checkpoint() {
    use acg_runtime_feedback::RuntimeFeedbackEngine;

    let graph = profile_graph();
    let candidate = candidate_graph(
        &graph,
        vec![
            candidate_tx(&graph, 1, "execute::Credit", json!({"account":"alice"})),
            candidate_tx(&graph, 2, "execute::Credit", json!({"account":"alice"})),
        ],
    );
    let edge = graph
        .edge_between_profiles(
            profile_id(&graph, "execute::Credit"),
            profile_id(&graph, "execute::Credit"),
        )
        .unwrap();
    let report = report(vec![
        successful_execution(
            0,
            1,
            vec![access(
                1,
                "contract-a",
                AccessKind::StorageWrite,
                b"same",
                None,
                false,
            )],
        ),
        successful_execution(
            1,
            2,
            vec![access(
                2,
                "contract-a",
                AccessKind::StorageRead,
                b"same",
                None,
                false,
            )],
        ),
    ]);
    let mut feedback = RuntimeFeedbackEngine::new(
        &graph,
        0,
        TraceConflictConfig::default(),
        RuntimeFeedbackWeights::default(),
        AdaptiveFeedbackConfig::default(),
    )
    .unwrap();
    let before = feedback
        .store()
        .static_statistics(edge)
        .unwrap()
        .probability();
    let summary = feedback
        .process_block(&graph, &candidate, &report, 3)
        .unwrap();
    assert_eq!(summary.positive_observations, 1);
    let after = feedback
        .store()
        .static_statistics(edge)
        .unwrap()
        .probability();
    assert!(after > before);

    let checkpoint = feedback.checkpoint(&graph).unwrap();
    let restored = RuntimeFeedbackEngine::restore(
        &graph,
        checkpoint,
        0,
        TraceConflictConfig::default(),
        RuntimeFeedbackWeights::default(),
        AdaptiveFeedbackConfig::default(),
    )
    .unwrap();
    assert_eq!(
        restored.store().static_statistics(edge),
        feedback.store().static_statistics(edge)
    );
}

#[test]
fn detector_rejects_mismatched_access_transaction_ids() {
    let malformed = report(vec![successful_execution(
        0,
        1,
        vec![access(
            99,
            "contract-a",
            AccessKind::StorageRead,
            b"k",
            None,
            false,
        )],
    )]);
    let error = AccessConflictDetector::new(TraceConflictConfig::default())
        .detect(&malformed)
        .unwrap_err();
    assert!(error
        .to_string()
        .contains("does not match access transaction ID"));
}
