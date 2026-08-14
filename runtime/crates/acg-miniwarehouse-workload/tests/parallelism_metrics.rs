use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use acg_candidate_graph::{EdgeClass, RiskBoundedSchedulerConfig};
use acg_core::{ContractCodeHash, RuntimeId};
use acg_cosmwasm_adapter::{CosmWasmAdapterConfig, CosmWasmCandidateAdapter};
use acg_cosmwasm_engine::{
    Address, BlockContext, CanonicalTxDisposition, CosmWasmEngine,
    DependencyPreexecutionDiagnostics, EngineConfig, ExecutionOutcome, ExecutionRequest,
    ParallelExecutionConfig, PreparedSpeculativeBlock, StateWriteSet, TransactionId,
    ValidationConflict,
};
use acg_feedback::AdaptiveFeedbackConfig;
use acg_miniwarehouse_workload::{
    GeneratedTransaction, MiniWarehouseExecuteMsg, MiniWarehouseMix, MiniWarehouseOperation,
    MiniWarehouseScale, MiniWarehouseWorkloadConfig, MiniWarehouseWorkloadGenerator,
};
use acg_predicate::PredicateResult;
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_runtime_feedback::{
    AdaptiveBlockPlan, AdaptivePlanningConfig, AdaptivePlanningMetrics, AdaptiveSerialPipeline,
    RuntimeFeedbackEngine, RuntimeFeedbackWeights, TraceConflictConfig,
};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use acg_validator_sim::{
    BlockProducer, BlockProducerConfig, ExecutionDependencyClass, ExecutionPlan, Mempool,
    ProducedBlock, ReconciliationTransactionDiagnostic, SpeculativeParallelBlockExecutor,
    SplitPhaseSpeculativeExecutionReport,
};
use cosmwasm_std::{to_json_binary, Binary};

const MINIWAREHOUSE_SYMBOLIC: &[u8] =
    include_bytes!("../../../../benchmarks/symbolic/miniwarehouse.symbolic.json");
const INSTANTIATE_TX_ID: u64 = 900_000;
const BOOTSTRAP_TX_ID_BASE: u64 = 10_000_000_000;
const DISTRICTS_PER_WAREHOUSE: u64 = 10;
const CUSTOMERS_PER_DISTRICT: u64 = 3_000;
const ITEMS_PER_WAREHOUSE: u64 = 100_000;
const DEFAULT_WASM_RELATIVE_PATH: &str =
    "benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_miniwarehouse.wasm";

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .expect("failed to resolve repository root")
}

fn miniwarehouse_wasm_path() -> PathBuf {
    env::var_os("ACG_MW_WASM")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository_root().join(DEFAULT_WASM_RELATIVE_PATH))
}

fn load_miniwarehouse_wasm() -> Option<(PathBuf, Vec<u8>)> {
    let explicit_path = env::var_os("ACG_MW_WASM").is_some();
    let path = miniwarehouse_wasm_path();
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if !explicit_path && error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "skipping MiniWarehouse parallelism metrics: Wasm artifact not found at {}; \
                 build it with `cargo build --manifest-path benchmarks/Cargo.toml \
                 -p acg-benchmark-miniwarehouse --release --target wasm32-unknown-unknown` \
                 or set ACG_MW_WASM",
                path.display()
            );
            return None;
        }
        Err(error) => {
            panic!(
                "failed to read MiniWarehouse Wasm at {}: {error}; build it with `cargo build --manifest-path benchmarks/Cargo.toml -p acg-benchmark-miniwarehouse --release --target wasm32-unknown-unknown` or set ACG_MW_WASM",
                path.display()
            )
        }
    };
    assert!(!bytes.is_empty(), "MiniWarehouse Wasm artifact is empty");
    Some((path, bytes))
}

fn setup_engine(wasm: &[u8]) -> (CosmWasmEngine, Address, ContractCodeHash) {
    let engine = CosmWasmEngine::new(EngineConfig::default());
    let code_id = engine.upload_wasm(wasm.to_vec()).unwrap();
    let checksum = engine.code_metadata(code_id).unwrap().checksum;
    let instantiate_msg = Binary::from(br#"{"admin":null}"#.to_vec());
    let contract = engine
        .instantiate(
            TransactionId(INSTANTIATE_TX_ID),
            BlockContext::default(),
            Address::new("admin"),
            code_id,
            None,
            "miniwarehouse".to_owned(),
            Vec::new(),
            instantiate_msg,
        )
        .unwrap()
        .contract;
    (engine, contract, ContractCodeHash(*checksum.as_bytes()))
}

fn compile_profile_graph(code_hash: ContractCodeHash) -> ProfileGraph {
    let context = IngestionContext::new(RuntimeId::new("cosmwasm").unwrap(), code_hash, 1);
    let profiles =
        normalize_document(parse_slice(MINIWAREHOUSE_SYMBOLIC).unwrap(), &context).unwrap();
    let artifact = ProfileGraphArtifact::compile(profiles, &EdgeBuildConfig::default()).unwrap();
    ProfileGraph::load(artifact, GraphLoadConfig::default()).unwrap()
}

#[derive(Clone, Debug)]
struct MeasurementConfig {
    total_transactions: usize,
    block_size: usize,
    block_interval: Duration,
    consensus_window: Duration,
    arrival_tps: f64,
    initial_backlog_transactions: usize,
    total_workers: usize,
    preexecution_workers: usize,
    require_preexecution_deadline: bool,
    edge_materialization_threshold: f64,
    soft_threshold: f64,
    hard_threshold: f64,
    risk_budget: f64,
    max_wave_width: Option<usize>,
    symbolic_hard_soften_after: u32,
    print_each_block: bool,
}

impl MeasurementConfig {
    fn effective_preexecution_workers(&self) -> usize {
        self.preexecution_workers
    }

    fn from_env() -> Self {
        let total_transactions = env_usize_alias("ACG_MW_TOTAL_TXS", "ACG_MW_TXS", 1_000);
        let block_size = env_usize("ACG_MW_BLOCK_SIZE", 50);
        let block_interval =
            Duration::from_millis(env_u64_allow_zero("ACG_MW_BLOCK_INTERVAL_MS", 700));
        let consensus_window =
            Duration::from_millis(env_u64_allow_zero("ACG_MW_CONSENSUS_MS", 700));
        assert!(
            !block_interval.is_zero(),
            "ACG_MW_BLOCK_INTERVAL_MS must be > 0"
        );
        assert!(
            !consensus_window.is_zero(),
            "ACG_MW_CONSENSUS_MS must be > 0"
        );

        let default_workers = std::thread::available_parallelism()
            .map(|count| count.get().min(6))
            .unwrap_or(1);
        let total_workers =
            env_usize_alias("ACG_MW_TOTAL_WORKERS", "ACG_MW_WORKERS", default_workers);
        // Pre-execution no longer overlaps canonical validation, so after block N commits the
        // whole configured worker budget can execute the already-planned block N+1.
        let preexecution_workers = total_workers;
        let default_arrival_tps = block_size as f64 / block_interval.as_secs_f64();
        let arrival_tps = env_f64("ACG_MW_ARRIVAL_TPS", default_arrival_tps);
        let initial_backlog_transactions = env_usize_allow_zero(
            "ACG_MW_INITIAL_BACKLOG_TXS",
            block_size.saturating_mul(3).min(total_transactions),
        )
        .min(total_transactions);

        let edge_materialization_threshold = env_probability("ACG_MW_MATERIALIZATION", 0.05);
        let soft_threshold = env_probability("ACG_MW_SOFT_THRESHOLD", 0.20);
        let hard_threshold = env_probability("ACG_MW_HARD_THRESHOLD", 0.80);
        let risk_budget = env_probability("ACG_MW_RISK_BUDGET", 0.20);
        assert!(
            soft_threshold <= hard_threshold,
            "soft threshold must be <= hard threshold"
        );
        let symbolic_hard_soften_after =
            u32::try_from(env_usize_allow_zero("ACG_MW_SYMBOLIC_HARD_SOFTEN_AFTER", 8))
                .unwrap_or(u32::MAX);

        Self {
            total_transactions,
            block_size,
            block_interval,
            consensus_window,
            arrival_tps,
            initial_backlog_transactions,
            total_workers,
            preexecution_workers,
            require_preexecution_deadline: env_bool(
                "ACG_MW_REQUIRE_PREEXEC_WITHIN_CONSENSUS",
                true,
            ),
            edge_materialization_threshold,
            soft_threshold,
            hard_threshold,
            risk_budget,
            max_wave_width: env_optional_usize("ACG_MW_MAX_WAVE_WIDTH"),
            symbolic_hard_soften_after,
            print_each_block: env_bool("ACG_MW_PRINT_EACH_BLOCK", false),
        }
    }
}

#[derive(Clone, Debug)]
struct TimedBlock {
    block: ProducedBlock,
    proposal_nanos: u64,
    decision_nanos: u64,
}

struct PlannedMeasurement {
    plan: AdaptiveBlockPlan,
    planning: AdaptivePlanningMetrics,
}

struct PreparedMeasurement {
    plan: AdaptiveBlockPlan,
    planning: AdaptivePlanningMetrics,
    prepared: PreparedSpeculativeBlock,
    preexecution: Duration,
    preexecution_feedback: Duration,
    /// Planning work that was not hidden behind validation of the predecessor block.
    planning_overhang: Duration,
    /// Work that must fit after the predecessor commits and before this block is decided.
    consensus_critical_preparation: Duration,
}

#[derive(Clone, Debug, Default)]
struct ProfilePairStats {
    materialized: u64,
    predicate_true: u64,
    predicate_unknown: u64,
    historical_false_override: u64,
    low: u64,
    soft: u64,
    hard: u64,
}

impl ProfilePairStats {
    fn merge(&mut self, other: Self) {
        self.materialized += other.materialized;
        self.predicate_true += other.predicate_true;
        self.predicate_unknown += other.predicate_unknown;
        self.historical_false_override += other.historical_false_override;
        self.low += other.low;
        self.soft += other.soft;
        self.hard += other.hard;
    }

    fn unknown_rate(&self) -> f64 {
        if self.materialized == 0 {
            0.0
        } else {
            self.predicate_unknown as f64 / self.materialized as f64
        }
    }
}

#[derive(Default)]
struct EdgeStats {
    possible_pairs: u64,
    materialized: u64,
    low: u64,
    soft: u64,
    hard: u64,
    predicate_true: u64,
    predicate_unknown: u64,
    historical_false_override: u64,
    profile_pairs: BTreeMap<String, ProfilePairStats>,
}

impl EdgeStats {
    fn merge(&mut self, other: Self) {
        self.possible_pairs += other.possible_pairs;
        self.materialized += other.materialized;
        self.low += other.low;
        self.soft += other.soft;
        self.hard += other.hard;
        self.predicate_true += other.predicate_true;
        self.predicate_unknown += other.predicate_unknown;
        self.historical_false_override += other.historical_false_override;
        for (pair, pair_stats) in other.profile_pairs {
            self.profile_pairs
                .entry(pair)
                .or_default()
                .merge(pair_stats);
        }
    }
}

#[derive(Default)]
struct RunTotals {
    planning: AdaptivePlanningMetrics,
    preexecution: Duration,
    preexecution_feedback: Duration,
    reconciliation_feedback: Duration,
    post_consensus: Duration,
    receipt_matching: Duration,
    validation: Duration,
    replay_or_missing: Duration,
    commit_reused: Duration,
    reconciliation_bookkeeping: Duration,
    executor_wrapper_overhead: Duration,
    canonical_fallback: Duration,
    serial: Duration,
    serial_transaction_work: Duration,
    theoretical_ideal: Duration,
    dependency_preexecution: DependencyPreexecutionDiagnostics,
    edges: EdgeStats,
    waves: usize,
    wave_widths: Vec<usize>,
    execution_dependencies: u64,
    hard_execution_dependencies: u64,
    speculative_results: u64,
    reused: u64,
    invalidated: u64,
    replayed: u64,
    canonical_missing: u64,
    predicted: u64,
    decided: u64,
    matched: u64,
    discarded_predictions: u64,
    missing_predictions: u64,
    preconsensus_deadline_hits: usize,
    preconsensus_deadline_misses: usize,
    overlapped_planning_transitions: usize,
    planning_overlap: Duration,
    planning_overhang: Duration,
    consensus_preparation_durations: Vec<Duration>,
    invalidations: InvalidationAttributionTotals,
}

#[derive(Clone, Copy, Debug)]
struct CandidateRelationDiagnostic {
    predicate: PredicateResult,
    class: EdgeClass,
}

#[derive(Clone, Debug, Default)]
struct ReceiptWriteSummary {
    storage: Vec<(Address, Vec<u8>)>,
    balances: Vec<(Address, String)>,
    created_contracts: Vec<Address>,
}

impl ReceiptWriteSummary {
    fn from_write_set(write_set: &StateWriteSet) -> Self {
        Self {
            storage: write_set
                .storage
                .iter()
                .map(|write| (write.contract.clone(), write.key.clone()))
                .collect(),
            balances: write_set
                .balances
                .iter()
                .map(|write| (write.address.clone(), write.denom.clone()))
                .collect(),
            created_contracts: write_set
                .created_contracts
                .iter()
                .map(|metadata| metadata.address.clone())
                .collect(),
        }
    }

    fn touches_conflict(&self, conflict: &ValidationConflict) -> bool {
        match conflict {
            ValidationConflict::ContractMetadata { address, .. } => self
                .created_contracts
                .iter()
                .any(|candidate| candidate == address),
            ValidationConflict::Storage { contract, key, .. } => {
                self.storage
                    .iter()
                    .any(|(candidate_contract, candidate_key)| {
                        candidate_contract == contract && candidate_key == key
                    })
            }
            ValidationConflict::StorageRange {
                contract,
                expected,
                actual,
                ..
            } => self
                .storage
                .iter()
                .any(|(candidate_contract, candidate_key)| {
                    candidate_contract == contract
                        && range_conflict_changed_key(expected, actual, candidate_key)
                }),
            ValidationConflict::BankBalance { address, denom, .. } => {
                self.balances
                    .iter()
                    .any(|(candidate_address, candidate_denom)| {
                        candidate_address == address && candidate_denom == denom
                    })
            }
            ValidationConflict::BankAllBalances {
                address,
                expected,
                actual,
                ..
            } => self
                .balances
                .iter()
                .any(|(candidate_address, candidate_denom)| {
                    candidate_address == address
                        && balance_conflict_changed_denom(expected, actual, candidate_denom)
                }),
        }
    }
}

fn range_conflict_changed_key(
    expected: &[(Vec<u8>, Vec<u8>)],
    actual: &[(Vec<u8>, Vec<u8>)],
    key: &[u8],
) -> bool {
    let expected_value = expected
        .iter()
        .find(|(candidate, _)| candidate.as_slice() == key)
        .map(|(_, value)| value.as_slice());
    let actual_value = actual
        .iter()
        .find(|(candidate, _)| candidate.as_slice() == key)
        .map(|(_, value)| value.as_slice());
    expected_value != actual_value
}

fn balance_conflict_changed_denom(
    expected: &[(String, u128)],
    actual: &[(String, u128)],
    denom: &str,
) -> bool {
    let expected_amount = expected
        .iter()
        .find(|(candidate, _)| candidate == denom)
        .map(|(_, amount)| *amount)
        .unwrap_or_default();
    let actual_amount = actual
        .iter()
        .find(|(candidate, _)| candidate == denom)
        .map(|(_, amount)| *amount)
        .unwrap_or_default();
    expected_amount != actual_amount
}

#[derive(Clone, Copy)]
enum ExecutionGuardDiagnostic {
    Direct(ExecutionDependencyClass),
    Transitive,
}

struct BlockInvalidationContext {
    operations: Vec<&'static str>,
    wave_by_transaction: Vec<usize>,
    wave_width_by_transaction: Vec<usize>,
    writes: Vec<ReceiptWriteSummary>,
    candidate_relations: BTreeMap<(usize, usize), CandidateRelationDiagnostic>,
    execution_dependencies: BTreeMap<(usize, usize), ExecutionDependencyClass>,
    dependency_ancestors: Vec<BTreeSet<usize>>,
}

impl BlockInvalidationContext {
    fn build(
        block: &ProducedBlock,
        plan: &AdaptiveBlockPlan,
        prepared: &PreparedSpeculativeBlock,
        pipeline: &AdaptiveSerialPipeline,
        operation_by_transaction: &BTreeMap<u64, &'static str>,
    ) -> Self {
        let transaction_count = block.transactions.len();
        assert_eq!(prepared.receipts.len(), transaction_count);
        assert_eq!(
            plan.speculative_execution_plan.transaction_count,
            transaction_count
        );

        let operations = block
            .transactions
            .iter()
            .map(|pending| {
                operation_by_transaction
                    .get(&pending.transaction_id().0)
                    .copied()
                    .unwrap_or("unknown")
            })
            .collect::<Vec<_>>();

        let mut wave_by_transaction = vec![usize::MAX; transaction_count];
        let mut wave_width_by_transaction = vec![0_usize; transaction_count];
        for (wave_index, wave) in plan.speculative_execution_plan.waves.iter().enumerate() {
            for &transaction_index in &wave.transaction_indices {
                wave_by_transaction[transaction_index] = wave_index;
                wave_width_by_transaction[transaction_index] = wave.transaction_indices.len();
            }
        }
        assert!(wave_by_transaction.iter().all(|wave| *wave != usize::MAX));

        let writes = prepared
            .receipts
            .iter()
            .map(|receipt| ReceiptWriteSummary::from_write_set(&receipt.write_set))
            .collect();

        let mut candidate_relations = BTreeMap::new();
        for edge in plan.candidate_graph.edges() {
            let source = edge.source.0 as usize;
            let target = edge.target.0 as usize;
            let key = ordered_pair(source, target);
            candidate_relations
                .entry(key)
                .or_insert(CandidateRelationDiagnostic {
                    predicate: edge.predicate_result,
                    class: pipeline.planning_config().scheduler.classify(edge),
                });
        }

        let execution_dependencies = plan
            .speculative_execution_plan
            .dependencies
            .iter()
            .map(|dependency| {
                (
                    (dependency.predecessor_index, dependency.successor_index),
                    dependency.class,
                )
            })
            .collect::<BTreeMap<_, _>>();
        let mut direct_predecessors = vec![Vec::<usize>::new(); transaction_count];
        for &(predecessor, successor) in execution_dependencies.keys() {
            direct_predecessors[successor].push(predecessor);
        }
        let mut dependency_ancestors = vec![BTreeSet::<usize>::new(); transaction_count];
        for successor in 0..transaction_count {
            for predecessor in direct_predecessors[successor].iter().copied() {
                dependency_ancestors[successor].insert(predecessor);
                let inherited = dependency_ancestors[predecessor].clone();
                dependency_ancestors[successor].extend(inherited);
            }
        }

        Self {
            operations,
            wave_by_transaction,
            wave_width_by_transaction,
            writes,
            candidate_relations,
            execution_dependencies,
            dependency_ancestors,
        }
    }

    fn nearest_predecessor_writer(
        &self,
        victim_index: usize,
        conflict: &ValidationConflict,
    ) -> Option<usize> {
        (0..victim_index)
            .rev()
            .find(|&index| self.writes[index].touches_conflict(conflict))
    }

    fn execution_guard(
        &self,
        writer_index: usize,
        victim_index: usize,
    ) -> Option<ExecutionGuardDiagnostic> {
        self.execution_dependencies
            .get(&(writer_index, victim_index))
            .copied()
            .map(ExecutionGuardDiagnostic::Direct)
            .or_else(|| {
                self.dependency_ancestors[victim_index]
                    .contains(&writer_index)
                    .then_some(ExecutionGuardDiagnostic::Transitive)
            })
    }
}

fn ordered_pair(left: usize, right: usize) -> (usize, usize) {
    if left <= right {
        (left, right)
    } else {
        (right, left)
    }
}

#[derive(Default)]
struct InvalidationAttributionTotals {
    invalidated_transactions: u64,
    invalidated_with_unguarded_conflict: u64,
    invalidated_with_cascade_candidate: u64,
    invalidated_with_unattributed_conflict: u64,
    validation_conflicts: u64,
    guarded_by_execution_dependency: u64,
    unguarded_predecessor: u64,
    cascade_from_replayed_dependency: u64,
    guarded_reused_unexplained: u64,
    unattributed: u64,
    same_wave: u64,
    earlier_wave: u64,
    later_wave: u64,
    by_operation_pair: BTreeMap<String, u64>,
    by_dependency: BTreeMap<String, u64>,
    by_candidate_relation: BTreeMap<String, u64>,
    by_execution_guard: BTreeMap<String, u64>,
    by_cause: BTreeMap<String, u64>,
    wave_width_population: BTreeMap<usize, u64>,
    wave_width_invalidated: BTreeMap<usize, u64>,
    position_population: [u64; 10],
    position_invalidated: [u64; 10],
}

impl InvalidationAttributionTotals {
    fn observe_block(
        &mut self,
        context: &BlockInvalidationContext,
        diagnostics: &[ReconciliationTransactionDiagnostic],
    ) {
        assert_eq!(context.operations.len(), diagnostics.len());
        let transaction_count = diagnostics.len();

        for diagnostic in diagnostics {
            let index = diagnostic.transaction_index;
            let width = context.wave_width_by_transaction[index];
            *self.wave_width_population.entry(width).or_default() += 1;
            self.position_population[position_decile(index, transaction_count)] += 1;

            if diagnostic.disposition != CanonicalTxDisposition::Replayed {
                continue;
            }

            self.invalidated_transactions += 1;
            *self.wave_width_invalidated.entry(width).or_default() += 1;
            self.position_invalidated[position_decile(index, transaction_count)] += 1;

            let Some(validation) = diagnostic.validation.as_ref() else {
                continue;
            };
            let mut transaction_has_unguarded = false;
            let mut transaction_has_cascade = false;
            let mut transaction_has_unattributed = false;
            for conflict in validation.conflicts() {
                self.validation_conflicts += 1;
                *self
                    .by_dependency
                    .entry(conflict_dependency_label(conflict))
                    .or_default() += 1;

                let Some(writer_index) = context.nearest_predecessor_writer(index, conflict) else {
                    self.unattributed += 1;
                    transaction_has_unattributed = true;
                    *self
                        .by_cause
                        .entry("unattributed:no preceding prepared writer".to_owned())
                        .or_default() += 1;
                    *self
                        .by_operation_pair
                        .entry(format!("unknown -> {}", context.operations[index]))
                        .or_default() += 1;
                    *self
                        .by_candidate_relation
                        .entry("no predecessor writer".to_owned())
                        .or_default() += 1;
                    *self
                        .by_execution_guard
                        .entry("no predecessor writer".to_owned())
                        .or_default() += 1;
                    continue;
                };

                let writer_wave = context.wave_by_transaction[writer_index];
                let victim_wave = context.wave_by_transaction[index];
                match writer_wave.cmp(&victim_wave) {
                    std::cmp::Ordering::Equal => self.same_wave += 1,
                    std::cmp::Ordering::Less => self.earlier_wave += 1,
                    std::cmp::Ordering::Greater => self.later_wave += 1,
                }

                *self
                    .by_operation_pair
                    .entry(format!(
                        "{} -> {}",
                        context.operations[writer_index], context.operations[index]
                    ))
                    .or_default() += 1;

                let relation_label = context
                    .candidate_relations
                    .get(&ordered_pair(writer_index, index))
                    .map(candidate_relation_label)
                    .unwrap_or_else(|| "NO_EDGE".to_owned());
                *self
                    .by_candidate_relation
                    .entry(relation_label)
                    .or_default() += 1;

                let writer_disposition = diagnostics[writer_index].disposition;
                let execution_guard = context.execution_guard(writer_index, index);
                let guard_label = match execution_guard {
                    Some(ExecutionGuardDiagnostic::Direct(ExecutionDependencyClass::Hard)) => {
                        "Hard dependency"
                    }
                    Some(ExecutionGuardDiagnostic::Direct(ExecutionDependencyClass::Soft)) => {
                        "Soft dependency"
                    }
                    Some(ExecutionGuardDiagnostic::Transitive) => "transitive dependency path",
                    None => "none",
                };
                *self
                    .by_execution_guard
                    .entry(guard_label.to_owned())
                    .or_default() += 1;

                let cause = match execution_guard {
                    Some(_) if writer_disposition == CanonicalTxDisposition::Replayed => {
                        self.guarded_by_execution_dependency += 1;
                        self.cascade_from_replayed_dependency += 1;
                        transaction_has_cascade = true;
                        "cascade:scheduled predecessor receipt later replayed"
                    }
                    Some(_) => {
                        self.guarded_by_execution_dependency += 1;
                        self.guarded_reused_unexplained += 1;
                        "unexpected:scheduled predecessor was canonically accepted"
                    }
                    None => {
                        self.unguarded_predecessor += 1;
                        transaction_has_unguarded = true;
                        match writer_wave.cmp(&victim_wave) {
                            std::cmp::Ordering::Equal => {
                                "speculation:no dependency within same scheduler level"
                            }
                            std::cmp::Ordering::Less => {
                                "speculation:no dependency from earlier scheduler level"
                            }
                            std::cmp::Ordering::Greater => {
                                "prediction miss:no dependency and predecessor is in later level"
                            }
                        }
                    }
                };
                *self.by_cause.entry(cause.to_owned()).or_default() += 1;
            }
            if transaction_has_unguarded {
                self.invalidated_with_unguarded_conflict += 1;
            }
            if transaction_has_cascade {
                self.invalidated_with_cascade_candidate += 1;
            }
            if transaction_has_unattributed {
                self.invalidated_with_unattributed_conflict += 1;
            }
        }
    }
}

fn position_decile(index: usize, transaction_count: usize) -> usize {
    if transaction_count == 0 {
        0
    } else {
        (index.saturating_mul(10) / transaction_count).min(9)
    }
}

fn candidate_relation_label(relation: &CandidateRelationDiagnostic) -> String {
    format!(
        "{}/{}",
        predicate_label(relation.predicate),
        edge_class_label(relation.class)
    )
}

fn predicate_label(result: PredicateResult) -> &'static str {
    match result {
        PredicateResult::True => "True",
        PredicateResult::False => "FalseOverride",
        PredicateResult::Unknown => "Unknown",
    }
}

fn edge_class_label(class: EdgeClass) -> &'static str {
    match class {
        EdgeClass::Low => "Low",
        EdgeClass::Soft => "Soft",
        EdgeClass::Hard => "Hard",
    }
}

fn conflict_dependency_label(conflict: &ValidationConflict) -> String {
    match conflict {
        ValidationConflict::ContractMetadata { .. } => "contract_metadata".to_owned(),
        ValidationConflict::Storage { key, .. } => {
            format!("storage:{}", storage_key_class(key))
        }
        ValidationConflict::StorageRange { start, .. } => format!(
            "storage_range:{}",
            start
                .as_deref()
                .map(storage_key_class)
                .unwrap_or("unbounded")
        ),
        ValidationConflict::BankBalance { .. } => "bank_balance".to_owned(),
        ValidationConflict::BankAllBalances { .. } => "bank_all_balances".to_owned(),
    }
}

fn storage_key_class(key: &[u8]) -> &'static str {
    // cw-storage-plus prefixes namespaces with binary length bytes, so classify by namespace bytes
    // rather than assuming the complete storage key is valid UTF-8 or starts with the namespace.
    if contains_bytes(key, b"warehouses") {
        "warehouse"
    } else if contains_bytes(key, b"districts") {
        "district"
    } else if contains_bytes(key, b"customers") {
        "customer"
    } else if contains_bytes(key, b"stock") {
        "stock"
    } else if contains_bytes(key, b"new_orders") {
        "new_order"
    } else if contains_bytes(key, b"order_lines") {
        "order_line"
    } else if contains_bytes(key, b"orders") {
        "order"
    } else if contains_bytes(key, b"history") {
        "history"
    } else if contains_bytes(key, b"config") {
        "config"
    } else {
        "other"
    }
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}

fn operation_name(operation: MiniWarehouseOperation) -> &'static str {
    match operation {
        MiniWarehouseOperation::NewOrder => "new_order",
        MiniWarehouseOperation::Payment => "payment",
        MiniWarehouseOperation::Delivery => "delivery",
        MiniWarehouseOperation::Restock => "restock",
        MiniWarehouseOperation::SeedWarehouse => "seed_warehouse",
        MiniWarehouseOperation::SeedDistrict => "seed_district",
        MiniWarehouseOperation::SeedCustomer => "seed_customer",
        MiniWarehouseOperation::SeedStock => "seed_stock",
    }
}

fn operation_by_transaction(transactions: &[GeneratedTransaction]) -> BTreeMap<u64, &'static str> {
    transactions
        .iter()
        .map(|generated| {
            (
                generated.request.transaction_id().0,
                operation_name(generated.operation),
            )
        })
        .collect()
}

fn adaptive_pipeline(
    graph: &ProfileGraph,
    measurement: &MeasurementConfig,
) -> AdaptiveSerialPipeline {
    let adapter = CosmWasmCandidateAdapter::new(
        CosmWasmAdapterConfig::new(RuntimeId::new("cosmwasm").unwrap(), 1).unwrap(),
    );
    let feedback = RuntimeFeedbackEngine::new(
        graph,
        0,
        TraceConflictConfig::default(),
        RuntimeFeedbackWeights::default(),
        AdaptiveFeedbackConfig {
            retention_factor: 1.0,
            ..AdaptiveFeedbackConfig::default()
        },
    )
    .unwrap();
    AdaptiveSerialPipeline::new(
        adapter,
        feedback,
        AdaptivePlanningConfig {
            edge_materialization_threshold: measurement.edge_materialization_threshold,
            scheduler: RiskBoundedSchedulerConfig {
                soft_threshold: measurement.soft_threshold,
                hard_threshold: measurement.hard_threshold,
                risk_budget: measurement.risk_budget,
                max_wave_width: measurement.max_wave_width,
                exploration_rate: 0.0,
                exploration_risk_budget: 0.90,
                exploration_min_uncertainty: 0.35,
                exploration_max_transactions_per_block: 0,
                independent_observations_before_softening: measurement.symbolic_hard_soften_after,
            },
            cost_policy: Default::default(),
            serial_bypass: Default::default(),
        },
    )
    .unwrap()
}

fn workload_config(contract: Address) -> MiniWarehouseWorkloadConfig {
    let mut config = MiniWarehouseWorkloadConfig::for_contract(contract);
    config.scale = MiniWarehouseScale {
        warehouse_count: env_u64("ACG_MW_WAREHOUSES", 4),
        districts_per_warehouse: DISTRICTS_PER_WAREHOUSE,
        customers_per_district: CUSTOMERS_PER_DISTRICT,
        items_per_warehouse: ITEMS_PER_WAREHOUSE,
    };
    config.mix = MiniWarehouseMix::default();
    config.seed = env_u64("ACG_MW_SEED", 42);
    config.min_order_lines = 3;
    config.max_order_lines = 6;
    config.remote_stock_probability_bps = env_u16("ACG_MW_REMOTE_BPS", 100);
    config.hot_warehouse_probability_bps = env_u16("ACG_MW_HOT_BPS", 2_500);
    config
}

#[derive(Default)]
struct SparseBootstrapResources {
    warehouses: BTreeSet<u64>,
    districts: BTreeSet<(u64, u64)>,
    customers: BTreeSet<(u64, u64, u64)>,
    stocks: BTreeSet<(u64, u64)>,
}

fn measured_execute_msg(generated: &GeneratedTransaction) -> MiniWarehouseExecuteMsg {
    let ExecutionRequest::Execute { msg, .. } = &generated.request else {
        panic!("MiniWarehouse measured workload emitted a non-execute request");
    };
    serde_json::from_slice(msg.as_slice()).expect("invalid MiniWarehouse execute JSON")
}

fn sparse_bootstrap_resources(measured: &[GeneratedTransaction]) -> SparseBootstrapResources {
    let mut resources = SparseBootstrapResources::default();
    for generated in measured {
        match measured_execute_msg(generated) {
            MiniWarehouseExecuteMsg::NewOrder {
                warehouse_id,
                district_id,
                customer_id,
                lines,
                ..
            } => {
                resources.warehouses.insert(warehouse_id);
                resources.districts.insert((warehouse_id, district_id));
                resources
                    .customers
                    .insert((warehouse_id, district_id, customer_id));
                for line in lines {
                    resources.warehouses.insert(line.supply_warehouse_id);
                    resources
                        .stocks
                        .insert((line.supply_warehouse_id, line.item_id));
                }
            }
            MiniWarehouseExecuteMsg::Payment {
                warehouse_id,
                district_id,
                customer_id,
                ..
            } => {
                resources.warehouses.insert(warehouse_id);
                resources.districts.insert((warehouse_id, district_id));
                resources
                    .customers
                    .insert((warehouse_id, district_id, customer_id));
            }
            MiniWarehouseExecuteMsg::Delivery {
                warehouse_id,
                district_id,
                ..
            } => {
                resources.warehouses.insert(warehouse_id);
                resources.districts.insert((warehouse_id, district_id));
            }
            MiniWarehouseExecuteMsg::Restock {
                warehouse_id,
                item_id,
                ..
            } => {
                resources.warehouses.insert(warehouse_id);
                resources.stocks.insert((warehouse_id, item_id));
            }
            MiniWarehouseExecuteMsg::SeedWarehouse { .. }
            | MiniWarehouseExecuteMsg::SeedDistrict { .. }
            | MiniWarehouseExecuteMsg::SeedCustomer { .. }
            | MiniWarehouseExecuteMsg::SeedStock { .. } => {
                panic!("measured MiniWarehouse workload unexpectedly contains seed transactions");
            }
        }
    }
    resources
}

fn bootstrap_transaction(
    transaction_id: u64,
    operation: MiniWarehouseOperation,
    admin: &Address,
    contract: &Address,
    msg: MiniWarehouseExecuteMsg,
) -> GeneratedTransaction {
    GeneratedTransaction {
        operation,
        request: ExecutionRequest::Execute {
            transaction_id: TransactionId(transaction_id),
            sender: admin.clone(),
            contract: contract.clone(),
            funds: Vec::new(),
            msg: to_json_binary(&msg).unwrap(),
        },
    }
}

fn sparse_bootstrap(
    config: &MiniWarehouseWorkloadConfig,
    measured: &[GeneratedTransaction],
) -> (Vec<GeneratedTransaction>, SparseBootstrapResources) {
    let resources = sparse_bootstrap_resources(measured);
    let mut transaction_id = BOOTSTRAP_TX_ID_BASE;
    let mut transactions = Vec::with_capacity(
        resources.warehouses.len()
            + resources.districts.len()
            + resources.customers.len()
            + resources.stocks.len(),
    );

    for &warehouse_id in &resources.warehouses {
        transactions.push(bootstrap_transaction(
            transaction_id,
            MiniWarehouseOperation::SeedWarehouse,
            &config.admin,
            &config.contract,
            MiniWarehouseExecuteMsg::SeedWarehouse {
                warehouse_id,
                tax_bps: 100,
            },
        ));
        transaction_id += 1;
    }
    for &(warehouse_id, district_id) in &resources.districts {
        transactions.push(bootstrap_transaction(
            transaction_id,
            MiniWarehouseOperation::SeedDistrict,
            &config.admin,
            &config.contract,
            MiniWarehouseExecuteMsg::SeedDistrict {
                warehouse_id,
                district_id,
                tax_bps: 50,
                next_order_id: config.first_order_id,
            },
        ));
        transaction_id += 1;
    }
    for &(warehouse_id, district_id, customer_id) in &resources.customers {
        transactions.push(bootstrap_transaction(
            transaction_id,
            MiniWarehouseOperation::SeedCustomer,
            &config.admin,
            &config.contract,
            MiniWarehouseExecuteMsg::SeedCustomer {
                warehouse_id,
                district_id,
                customer_id,
                discount_bps: 0,
            },
        ));
        transaction_id += 1;
    }
    for &(warehouse_id, item_id) in &resources.stocks {
        transactions.push(bootstrap_transaction(
            transaction_id,
            MiniWarehouseOperation::SeedStock,
            &config.admin,
            &config.contract,
            MiniWarehouseExecuteMsg::SeedStock {
                warehouse_id,
                item_id,
                quantity: config.initial_stock_quantity,
            },
        ));
        transaction_id += 1;
    }

    (transactions, resources)
}

fn execute_bootstrap(engine: &CosmWasmEngine, requests: &[GeneratedTransaction]) {
    for (index, generated) in requests.iter().enumerate() {
        let block = BlockContext {
            transaction_index: Some(index as u32),
            ..BlockContext::default()
        };
        engine
            .execute_request(block, generated.request.clone())
            .unwrap_or_else(|error| panic!("bootstrap transaction {index} failed: {error}"));
    }
}

fn build_timed_blocks(
    requests: &[GeneratedTransaction],
    measurement: &MeasurementConfig,
) -> (Vec<TimedBlock>, Vec<u64>, Vec<u64>) {
    let mempool = Mempool::default();
    let block_interval_nanos = duration_nanos(measurement.block_interval);
    let consensus_nanos = duration_nanos(measurement.consensus_window);
    let producer_config = BlockProducerConfig {
        block_interval: measurement.block_interval,
        first_block_time_nanos: block_interval_nanos,
        max_transactions_per_block: Some(measurement.block_size),
        ..BlockProducerConfig::default()
    };
    let mut producer = BlockProducer::fifo(producer_config).unwrap();

    let arrivals = requests
        .iter()
        .enumerate()
        .map(|(index, generated)| {
            let arrival_nanos = if index < measurement.initial_backlog_transactions {
                0
            } else {
                let ordinal = index - measurement.initial_backlog_transactions + 1;
                ((ordinal as f64 / measurement.arrival_tps) * 1_000_000_000.0) as u64
            };
            (arrival_nanos, generated.request.clone())
        })
        .collect::<Vec<_>>();

    let mut next_arrival = 0_usize;
    let mut included = 0_usize;
    let mut blocks = Vec::new();
    let mut proposal_waits = Vec::with_capacity(requests.len());
    let mut decision_waits = Vec::with_capacity(requests.len());
    let mut guard = 0_usize;

    while included < requests.len() {
        guard += 1;
        assert!(
            guard < 1_000_000,
            "virtual block simulation failed to make progress"
        );
        let proposal_nanos = producer.next_block_time_nanos();
        while next_arrival < arrivals.len() && arrivals[next_arrival].0 <= proposal_nanos {
            let (arrival_nanos, request) = &arrivals[next_arrival];
            mempool.admit(request.clone(), *arrival_nanos);
            next_arrival += 1;
        }

        let preview = producer.preview_next(&mempool);
        let produced = producer.produce_next(&mempool);
        assert_eq!(
            preview, produced,
            "FIFO preview must equal the decided local proposal"
        );
        if produced.transactions.is_empty() {
            continue;
        }

        let decision_nanos = proposal_nanos.saturating_add(consensus_nanos);
        for pending in &produced.transactions {
            proposal_waits.push(proposal_nanos.saturating_sub(pending.admitted_at_nanos));
            decision_waits.push(decision_nanos.saturating_sub(pending.admitted_at_nanos));
        }
        included += produced.transactions.len();
        blocks.push(TimedBlock {
            block: produced,
            proposal_nanos,
            decision_nanos,
        });
    }

    (blocks, proposal_waits, decision_waits)
}

fn execute_serial_timed(
    engine: &CosmWasmEngine,
    block: &ProducedBlock,
) -> (Duration, Vec<Duration>, Vec<ExecutionOutcome>) {
    let block_start = Instant::now();
    let mut per_tx = Vec::with_capacity(block.transactions.len());
    let mut outcomes = Vec::with_capacity(block.transactions.len());

    for (index, pending) in block.transactions.iter().enumerate() {
        let mut context = block.context.clone();
        context.transaction_index = Some(u32::try_from(index).unwrap());
        let tx_start = Instant::now();
        let outcome = engine
            .execute_request(context, pending.request.clone())
            .unwrap_or_else(|error| panic!("serial transaction {index} failed: {error}"));
        per_tx.push(tx_start.elapsed());
        outcomes.push(outcome);
    }

    (block_start.elapsed(), per_tx, outcomes)
}

fn plan_block(
    pipeline: &AdaptiveSerialPipeline,
    engine: &CosmWasmEngine,
    graph: &ProfileGraph,
    block: &ProducedBlock,
) -> PlannedMeasurement {
    let (plan, planning) = pipeline
        .plan_block_with_metrics(engine, graph, block)
        .unwrap();
    PlannedMeasurement { plan, planning }
}

fn preexecute_planned_block(
    executor: &SpeculativeParallelBlockExecutor,
    pipeline: &mut AdaptiveSerialPipeline,
    profile_graph: &ProfileGraph,
    block: &ProducedBlock,
    planned: PlannedMeasurement,
    planning_overlap: Duration,
) -> PreparedMeasurement {
    // This call snapshots canonical engine state *now*. The benchmark only invokes it after the
    // predecessor block has reconciled/committed, so speculative execution never runs against a
    // guessed post-predecessor state.
    let started = Instant::now();
    let prepared = executor
        .prepare(block, &planned.plan.speculative_execution_plan)
        .unwrap();
    let preexecution = started.elapsed();

    // Successful pre-consensus receipts are concrete executions too. Feed their read/write traces
    // into the adaptive model immediately so planning for following blocks can soften symbolic
    // relationships that repeatedly prove independent, or reinforce observed conflicts.
    let feedback_started = Instant::now();
    let preexecution_report = executor.pre_execution_report(block, &prepared).unwrap();
    pipeline
        .process_pre_execution_report(
            profile_graph,
            &planned.plan,
            &preexecution_report,
            block.context.height,
        )
        .unwrap();
    let preexecution_feedback = feedback_started.elapsed();

    let hidden_planning = planning_overlap.min(planned.planning.total());
    let planning_overhang = planned.planning.total().saturating_sub(hidden_planning);
    let consensus_critical_preparation = planning_overhang + preexecution + preexecution_feedback;
    PreparedMeasurement {
        plan: planned.plan,
        planning: planned.planning,
        prepared,
        preexecution,
        preexecution_feedback,
        planning_overhang,
        consensus_critical_preparation,
    }
}

enum PostConsensusResult {
    Reconciled(Box<SplitPhaseSpeculativeExecutionReport>),
    CanonicalFallback { outcomes: Vec<ExecutionOutcome> },
}

fn commit_current_block(
    executor: &SpeculativeParallelBlockExecutor,
    engine: &CosmWasmEngine,
    block: &ProducedBlock,
    prepared: PreparedSpeculativeBlock,
    deadline_hit: bool,
) -> PostConsensusResult {
    if deadline_hit {
        PostConsensusResult::Reconciled(Box::new(
            executor.validate_prepared(block, prepared).unwrap(),
        ))
    } else {
        let (_, _, outcomes) = execute_serial_timed(engine, block);
        PostConsensusResult::CanonicalFallback { outcomes }
    }
}

fn edge_stats(
    plan: &AdaptiveBlockPlan,
    pipeline: &AdaptiveSerialPipeline,
    profile_graph: &ProfileGraph,
) -> EdgeStats {
    let transaction_count = plan.candidate_graph.transactions().len() as u64;
    let mut stats = EdgeStats {
        possible_pairs: transaction_count.saturating_mul(transaction_count.saturating_sub(1)) / 2,
        materialized: plan.candidate_graph.edges().len() as u64,
        ..EdgeStats::default()
    };

    for edge in plan.candidate_graph.edges() {
        let class = pipeline.planning_config().scheduler.classify(edge);
        match class {
            EdgeClass::Low => stats.low += 1,
            EdgeClass::Soft => stats.soft += 1,
            EdgeClass::Hard => stats.hard += 1,
        }
        match edge.predicate_result {
            PredicateResult::True => stats.predicate_true += 1,
            PredicateResult::Unknown => stats.predicate_unknown += 1,
            PredicateResult::False => stats.historical_false_override += 1,
        }

        let source = plan.candidate_graph.transaction(edge.source).unwrap();
        let target = plan.candidate_graph.transaction(edge.target).unwrap();
        let source_name = &profile_graph
            .profile(source.profile_id)
            .unwrap()
            .definition
            .entrypoint_name;
        let target_name = &profile_graph
            .profile(target.profile_id)
            .unwrap()
            .definition
            .entrypoint_name;
        let pair = if source_name <= target_name {
            format!("{source_name} <-> {target_name}")
        } else {
            format!("{target_name} <-> {source_name}")
        };
        let pair_stats = stats.profile_pairs.entry(pair).or_default();
        pair_stats.materialized += 1;
        match edge.predicate_result {
            PredicateResult::True => pair_stats.predicate_true += 1,
            PredicateResult::Unknown => pair_stats.predicate_unknown += 1,
            PredicateResult::False => pair_stats.historical_false_override += 1,
        }
        match class {
            EdgeClass::Low => pair_stats.low += 1,
            EdgeClass::Soft => pair_stats.soft += 1,
            EdgeClass::Hard => pair_stats.hard += 1,
        }
    }
    stats
}

fn ideal_cost_time(per_tx: &[Duration], plan: &ExecutionPlan, workers: usize) -> Duration {
    if per_tx.is_empty() || workers == 0 {
        return Duration::ZERO;
    }
    assert_eq!(per_tx.len(), plan.transaction_count);

    // Dependency-driven execution no longer waits at global wave barriers. A useful lower bound is
    // therefore the larger of (a) total work divided by workers and (b) the weighted critical path
    // through the explicit dependency DAG.
    let total_work = per_tx.iter().map(Duration::as_secs_f64).sum::<f64>();
    let worker_bound = total_work / workers as f64;

    let mut predecessors = vec![Vec::<usize>::new(); plan.transaction_count];
    for dependency in &plan.dependencies {
        predecessors[dependency.successor_index].push(dependency.predecessor_index);
    }
    let mut finish = vec![0.0_f64; plan.transaction_count];
    for index in 0..plan.transaction_count {
        let ready_at = predecessors[index]
            .iter()
            .map(|&predecessor| finish[predecessor])
            .fold(0.0_f64, f64::max);
        finish[index] = ready_at + per_tx[index].as_secs_f64();
    }
    let critical_path = finish.into_iter().fold(0.0_f64, f64::max);
    Duration::from_secs_f64(worker_bound.max(critical_path))
}

fn operation_counts(transactions: &[GeneratedTransaction]) -> BTreeMap<&'static str, usize> {
    let mut counts = BTreeMap::new();
    for generated in transactions {
        *counts
            .entry(operation_name(generated.operation))
            .or_default() += 1;
    }
    counts
}

fn percentile_usize(sorted: &[usize], percentile: f64) -> usize {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((sorted.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[rank]
}

fn percentile_u64(sorted: &[u64], percentile: f64) -> u64 {
    if sorted.is_empty() {
        return 0;
    }
    let rank = ((sorted.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[rank]
}

fn percentile_duration(sorted: &[Duration], percentile: f64) -> Duration {
    if sorted.is_empty() {
        return Duration::ZERO;
    }
    let rank = ((sorted.len() as f64 * percentile).ceil() as usize)
        .saturating_sub(1)
        .min(sorted.len() - 1);
    sorted[rank]
}

fn duration_nanos(duration: Duration) -> u64 {
    duration.as_nanos().min(u128::from(u64::MAX)) as u64
}

fn env_usize(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_usize_alias(primary: &str, fallback: &str, default: usize) -> usize {
    env::var(primary)
        .ok()
        .or_else(|| env::var(fallback).ok())
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_usize_allow_zero(name: &str, default: usize) -> usize {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(default)
}

fn env_optional_usize(name: &str) -> Option<usize> {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 0)
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(default)
}

fn env_u64_allow_zero(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_u16(name: &str, default: u16) -> u16 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|value| *value <= 10_000)
        .unwrap_or(default)
}

fn env_f64(name: &str, default: f64) -> f64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(default)
}

fn env_probability(name: &str, default: f64) -> f64 {
    env::var(name)
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite() && (0.0..=1.0).contains(value))
        .unwrap_or(default)
}

fn env_bool(name: &str, default: bool) -> bool {
    env::var(name)
        .ok()
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(default)
}

fn add_planning(total: &mut AdaptivePlanningMetrics, value: AdaptivePlanningMetrics) {
    total.adapter += value.adapter;
    total.candidate_graph += value.candidate_graph;
    total.scheduler += value.scheduler;
    total.schedule_validation += value.schedule_validation;
    total.plan_conversion += value.plan_conversion;
}

fn print_ranked_counts(title: &str, counts: &BTreeMap<String, u64>, limit: usize) {
    println!("{title}");
    let mut ranked = counts.iter().collect::<Vec<_>>();
    ranked.sort_by(|left, right| right.1.cmp(left.1).then_with(|| left.0.cmp(right.0)));
    for (label, count) in ranked.into_iter().take(limit) {
        println!("    {label:<56} {count:>8}");
    }
    if counts.is_empty() {
        println!("    (none)");
    }
}

#[test]
fn miniwarehouse_prints_expected_vs_realized_parallelism() {
    let measurement = MeasurementConfig::from_env();
    let Some((wasm_path, wasm)) = load_miniwarehouse_wasm() else {
        return;
    };
    let (serial_engine, serial_contract, code_hash) = setup_engine(&wasm);
    let (adaptive_engine, adaptive_contract, adaptive_code_hash) = setup_engine(&wasm);
    assert_eq!(serial_contract, adaptive_contract);
    assert_eq!(code_hash, adaptive_code_hash);

    let profile_graph = compile_profile_graph(code_hash);
    let mut pipeline = adaptive_pipeline(&profile_graph, &measurement);
    let workload = workload_config(serial_contract.clone());
    let mut generator = MiniWarehouseWorkloadGenerator::new(workload.clone()).unwrap();

    // Generate the measured stream first, then seed only the base resources it actually touches.
    // This preserves a large 3k-customer/100k-item key domain without spending benchmark startup
    // time executing hundreds of thousands of unrelated Wasm seed transactions.
    let measured = generator.generate(measurement.total_transactions).unwrap();
    let (bootstrap, bootstrap_resources) = sparse_bootstrap(&workload, &measured);
    let bootstrap_started = Instant::now();
    execute_bootstrap(&serial_engine, &bootstrap);
    execute_bootstrap(&adaptive_engine, &bootstrap);
    let bootstrap_elapsed = bootstrap_started.elapsed();
    assert!(serial_engine
        .snapshot()
        .same_world_state(&adaptive_engine.snapshot()));

    let operation_counts = operation_counts(&measured);
    let operation_by_transaction = operation_by_transaction(&measured);
    let (blocks, mut proposal_waits, mut decision_waits) =
        build_timed_blocks(&measured, &measurement);
    assert!(!blocks.is_empty());
    assert_eq!(
        blocks
            .iter()
            .map(|block| block.block.transactions.len())
            .sum::<usize>(),
        measurement.total_transactions
    );

    // Serial baseline: only post-consensus canonical execution is timed. It runs independently so
    // its CPU work does not interfere with the adaptive overlap measurement below.
    let mut serial_block_times = Vec::with_capacity(blocks.len());
    let mut serial_tx_costs = Vec::with_capacity(blocks.len());
    for timed in &blocks {
        let (elapsed, per_tx, outcomes) = execute_serial_timed(&serial_engine, &timed.block);
        assert_eq!(outcomes.len(), timed.block.transactions.len());
        serial_block_times.push(elapsed);
        serial_tx_costs.push(per_tx);
    }

    let executor = SpeculativeParallelBlockExecutor::new(
        adaptive_engine.clone(),
        ParallelExecutionConfig {
            workers: measurement.preexecution_workers,
        },
    );

    // First block has no predecessor validation window to hide planning behind, so both planning
    // and pre-execution consume its initial consensus budget.
    let first_planned = plan_block(
        &pipeline,
        &adaptive_engine,
        &profile_graph,
        &blocks[0].block,
    );
    let mut current = Some(preexecute_planned_block(
        &executor,
        &mut pipeline,
        &profile_graph,
        &blocks[0].block,
        first_planned,
        Duration::ZERO,
    ));
    let mut totals = RunTotals::default();

    for index in 0..blocks.len() {
        let prepared_current = current
            .take()
            .expect("every decided block must have a preparation record");
        let timed = &blocks[index];
        let block_transactions = timed.block.transactions.len();
        let stats = edge_stats(&prepared_current.plan, &pipeline, &profile_graph);
        totals.edges.merge(stats);
        totals.waves += prepared_current.prepared.metrics.wave_count();
        totals.wave_widths.extend(
            prepared_current
                .prepared
                .metrics
                .wave_widths
                .iter()
                .copied(),
        );
        totals.execution_dependencies += prepared_current.prepared.metrics.dependency_count as u64;
        totals.hard_execution_dependencies +=
            prepared_current.prepared.metrics.hard_dependency_count as u64;
        totals
            .dependency_preexecution
            .merge(&prepared_current.prepared.metrics.dependency_diagnostics);
        totals.theoretical_ideal += ideal_cost_time(
            &serial_tx_costs[index],
            &prepared_current.plan.speculative_execution_plan,
            measurement.effective_preexecution_workers(),
        );
        add_planning(&mut totals.planning, prepared_current.planning);
        totals.preexecution += prepared_current.preexecution;
        totals.preexecution_feedback += prepared_current.preexecution_feedback;
        totals.planning_overhang += prepared_current.planning_overhang;
        totals
            .consensus_preparation_durations
            .push(prepared_current.consensus_critical_preparation);
        totals.serial += serial_block_times[index];
        totals.serial_transaction_work += serial_tx_costs[index]
            .iter()
            .copied()
            .fold(Duration::ZERO, |total, duration| total + duration);

        let deadline_hit =
            prepared_current.consensus_critical_preparation <= measurement.consensus_window;
        if deadline_hit {
            totals.preconsensus_deadline_hits += 1;
        } else {
            totals.preconsensus_deadline_misses += 1;
            if measurement.require_preexecution_deadline {
                panic!(
                    "block {} speculative preparation took {:.3} ms after its predecessor commit, exceeding the configured {:.3} ms consensus window; increase ACG_MW_CONSENSUS_MS or set ACG_MW_REQUIRE_PREEXEC_WITHIN_CONSENSUS=0 to measure fallback behavior",
                    timed.block.context.height,
                    prepared_current.consensus_critical_preparation.as_secs_f64() * 1e3,
                    measurement.consensus_window.as_secs_f64() * 1e3,
                );
            }
        }

        if measurement.print_each_block {
            let hidden_planning = prepared_current
                .planning
                .total()
                .saturating_sub(prepared_current.planning_overhang);
            println!();
            println!(
                "---------------- Block {} ----------------",
                timed.block.context.height
            );
            println!("txs:                         {block_transactions}");
            println!(
                "proposal virtual time:       {:.3} s",
                timed.proposal_nanos as f64 / 1e9
            );
            println!(
                "decision virtual time:       {:.3} s",
                timed.decision_nanos as f64 / 1e9
            );
            println!(
                "graph/schedule planning:      {:>8.3} ms",
                prepared_current.planning.total().as_secs_f64() * 1e3
            );
            println!(
                "planning hidden by prev N:    {:>8.3} ms",
                hidden_planning.as_secs_f64() * 1e3
            );
            println!(
                "planning overhang after N:    {:>8.3} ms",
                prepared_current.planning_overhang.as_secs_f64() * 1e3
            );
            println!(
                "post-commit pre-execution:    {:>8.3} ms",
                prepared_current.preexecution.as_secs_f64() * 1e3
            );
            println!(
                "preexecution feedback:        {:>8.3} ms",
                prepared_current.preexecution_feedback.as_secs_f64() * 1e3
            );
            println!(
                "execution dependencies:       {} (hard {})",
                prepared_current.prepared.metrics.dependency_count,
                prepared_current.prepared.metrics.hard_dependency_count
            );
            let diagnostics = &prepared_current.prepared.metrics.dependency_diagnostics;
            println!(
                "dependency worker wall:       {:>8.3} ms",
                diagnostics.worker_phase_wall.as_secs_f64() * 1e3
            );
            println!(
                "aggregate visibility capture: {:>8.3} ms",
                diagnostics.aggregate_visibility_capture.as_secs_f64() * 1e3
            );
            println!(
                "aggregate contract execution: {:>8.3} ms",
                diagnostics.aggregate_contract_execution.as_secs_f64() * 1e3
            );
            println!(
                "aggregate publish/unblock:    {:>8.3} ms",
                diagnostics.aggregate_publish_and_unblock.as_secs_f64() * 1e3
            );
            println!(
                "visibility masks captured:    {}",
                diagnostics.visibility_masks_captured
            );
            println!(
                "max in-flight txs:            {}",
                diagnostics.max_in_flight
            );
            println!(
                "consensus-critical prep:      {:>8.3} ms",
                prepared_current
                    .consensus_critical_preparation
                    .as_secs_f64()
                    * 1e3
            );
            println!(
                "consensus budget:             {:>8.3} ms",
                measurement.consensus_window.as_secs_f64() * 1e3
            );
            println!(
                "ready by decision:            {}",
                if deadline_hit { "YES" } else { "NO" }
            );
            println!(
                "materialized edges:           {}",
                prepared_current.plan.candidate_graph.edges().len()
            );
            println!(
                "waves:                        {}",
                prepared_current.prepared.metrics.wave_count()
            );
        }

        let attribution_context = BlockInvalidationContext::build(
            &timed.block,
            &prepared_current.plan,
            &prepared_current.prepared,
            &pipeline,
            &operation_by_transaction,
        );
        let predicted_count = prepared_current.prepared.predicted_transaction_count() as u64;
        let speculative_count = prepared_current
            .prepared
            .metrics
            .speculative
            .speculative_results;
        let next_block = blocks.get(index + 1).map(|next| &next.block);
        let can_overlap_planning = next_block.is_some() && measurement.total_workers > 1;

        // Only *planning* for N+1 may overlap post-consensus validation/commit of N. Full
        // pre-execution is deliberately delayed until N has committed, so it snapshots the actual
        // canonical post-N state rather than a predicted successor state.
        let (post_result, next_planned, post_elapsed) = if can_overlap_planning {
            totals.overlapped_planning_transitions += 1;
            let next_block = next_block.expect("checked above");
            std::thread::scope(|scope| {
                let planning_handle = scope
                    .spawn(|| plan_block(&pipeline, &adaptive_engine, &profile_graph, next_block));
                let started = Instant::now();
                let result = commit_current_block(
                    &executor,
                    &adaptive_engine,
                    &timed.block,
                    prepared_current.prepared,
                    deadline_hit,
                );
                let post_elapsed = started.elapsed();
                let planned = planning_handle
                    .join()
                    .expect("next-block planning thread panicked");
                (result, Some(planned), post_elapsed)
            })
        } else {
            let started = Instant::now();
            let result = commit_current_block(
                &executor,
                &adaptive_engine,
                &timed.block,
                prepared_current.prepared,
                deadline_hit,
            );
            let post_elapsed = started.elapsed();
            let planned = next_block.map(|next_block| {
                plan_block(&pipeline, &adaptive_engine, &profile_graph, next_block)
            });
            (result, planned, post_elapsed)
        };

        totals.post_consensus += post_elapsed;
        match post_result {
            PostConsensusResult::Reconciled(report) => {
                let known_reconciliation = report.timings.receipt_matching
                    + report.timings.validation
                    + report.timings.replay_or_missing_execution
                    + report.timings.commit_reused;
                totals.receipt_matching += report.timings.receipt_matching;
                totals.validation += report.timings.validation;
                totals.replay_or_missing += report.timings.replay_or_missing_execution;
                totals.commit_reused += report.timings.commit_reused;
                totals.reconciliation_bookkeeping +=
                    report.timings.total.saturating_sub(known_reconciliation);
                totals.executor_wrapper_overhead +=
                    post_elapsed.saturating_sub(report.timings.total);
                totals.speculative_results += report.speculative.speculative_results;
                totals.reused += report.speculative.reused_results;
                totals.invalidated += report.speculative.invalidated_results;
                totals.replayed += report.speculative.replayed_transactions;
                totals.canonical_missing += report.speculative.canonical_transactions;
                totals.predicted += report.prediction.predicted_transactions;
                totals.decided += report.prediction.decided_transactions;
                totals.matched += report.prediction.matched_transactions;
                totals.discarded_predictions += report.prediction.discarded_predictions;
                totals.missing_predictions += report.prediction.missing_predictions;
                totals
                    .invalidations
                    .observe_block(&attribution_context, &report.reconciliation);
                let feedback_started = Instant::now();
                pipeline
                    .process_reconciliation_report(
                        &profile_graph,
                        &prepared_current.plan,
                        &report,
                        timed.block.context.height,
                    )
                    .unwrap();
                totals.reconciliation_feedback += feedback_started.elapsed();
                assert_eq!(report.block.successful(), block_transactions);
                assert_eq!(report.block.failed(), 0);
            }
            PostConsensusResult::CanonicalFallback { outcomes } => {
                assert_eq!(outcomes.len(), block_transactions);
                totals.canonical_fallback += post_elapsed;
                totals.speculative_results += speculative_count;
                totals.predicted += predicted_count;
                totals.decided += block_transactions as u64;
                totals.discarded_predictions += predicted_count;
                totals.canonical_missing += block_transactions as u64;
                totals.missing_predictions += block_transactions as u64;
            }
        }

        if let (Some(next_block), Some(planned)) = (next_block, next_planned) {
            let planning_overlap = if can_overlap_planning {
                let hidden = planned.planning.total().min(post_elapsed);
                totals.planning_overlap += hidden;
                hidden
            } else {
                Duration::ZERO
            };

            // Canonical state now includes block N. Only now do we launch N+1 speculative
            // execution; `prepare()` snapshots this committed state internally.
            current = Some(preexecute_planned_block(
                &executor,
                &mut pipeline,
                &profile_graph,
                next_block,
                planned,
                planning_overlap,
            ));
        }
    }

    assert!(serial_engine
        .snapshot()
        .same_world_state(&adaptive_engine.snapshot()));

    proposal_waits.sort_unstable();
    decision_waits.sort_unstable();
    totals.wave_widths.sort_unstable();
    totals.consensus_preparation_durations.sort_unstable();

    let total_transactions = measurement.total_transactions as f64;
    let avg_wave_width = if totals.waves == 0 {
        0.0
    } else {
        total_transactions / totals.waves as f64
    };
    let exposed_parallelism = avg_wave_width;
    let preexecution_theoretical_speedup = if totals.theoretical_ideal.is_zero() {
        0.0
    } else {
        totals.serial.as_secs_f64() / totals.theoretical_ideal.as_secs_f64()
    };
    let preexecution_realized_speedup = if totals.preexecution.is_zero() {
        0.0
    } else {
        totals.serial.as_secs_f64() / totals.preexecution.as_secs_f64()
    };
    let preexecution_realization = if preexecution_theoretical_speedup > 0.0 {
        preexecution_realized_speedup / preexecution_theoretical_speedup
    } else {
        0.0
    };
    let dependency_diagnostics = &totals.dependency_preexecution;
    let contract_diagnostics = &dependency_diagnostics.contract;
    let contract_outer_accounted = contract_diagnostics.aggregate_request_execution
        + contract_diagnostics.aggregate_receipt_finalization;
    let contract_outer_residual = dependency_diagnostics
        .aggregate_contract_execution
        .saturating_sub(contract_outer_accounted);
    let wasm_runtime_total = contract_diagnostics.aggregate_wasm_instance_acquire
        + contract_diagnostics.aggregate_wasm_entrypoint
        + contract_diagnostics.aggregate_wasm_recycle;
    let host_storage_operations = contract_diagnostics.host_storage_gets
        + contract_diagnostics.host_storage_scans
        + contract_diagnostics.host_storage_nexts
        + contract_diagnostics.host_storage_sets
        + contract_diagnostics.host_storage_removes;
    let wasm_calls_per_tx = if totals.speculative_results == 0 {
        0.0
    } else {
        contract_diagnostics.wasm_entrypoint_calls as f64 / totals.speculative_results as f64
    };
    let host_storage_ops_per_tx = if totals.speculative_results == 0 {
        0.0
    } else {
        host_storage_operations as f64 / totals.speculative_results as f64
    };
    let speculative_contract_cost_inflation = if totals.serial_transaction_work.is_zero() {
        0.0
    } else {
        dependency_diagnostics
            .aggregate_contract_execution
            .as_secs_f64()
            / totals.serial_transaction_work.as_secs_f64()
    };
    let execution_phase_wall = dependency_diagnostics.worker_phase_wall;
    let effective_contract_concurrency = if execution_phase_wall.is_zero() {
        0.0
    } else {
        dependency_diagnostics
            .aggregate_contract_execution
            .as_secs_f64()
            / execution_phase_wall.as_secs_f64()
    };
    let execution_threads = measurement.preexecution_workers;
    let worker_capacity = execution_phase_wall.as_secs_f64() * execution_threads as f64;
    let aggregate_worker_busy = dependency_diagnostics
        .aggregate_worker_stage_time()
        .saturating_sub(dependency_diagnostics.aggregate_ready_wait);
    let measured_worker_busy_fraction = if worker_capacity == 0.0 {
        0.0
    } else {
        aggregate_worker_busy.as_secs_f64() / worker_capacity
    };
    let measured_worker_wait_fraction = if worker_capacity == 0.0 {
        0.0
    } else {
        dependency_diagnostics.aggregate_ready_wait.as_secs_f64() / worker_capacity
    };
    let preexecution_wrapper_overhead = totals
        .preexecution
        .saturating_sub(dependency_diagnostics.executor_total);
    let executor_wall_accounted =
        dependency_diagnostics.dependency_plan_setup + dependency_diagnostics.worker_phase_wall;
    let executor_coordinator_residual = dependency_diagnostics
        .executor_total
        .saturating_sub(executor_wall_accounted);
    let visibility_words_per_tx = if totals.speculative_results == 0 {
        0.0
    } else {
        dependency_diagnostics.visibility_words_copied as f64 / totals.speculative_results as f64
    };
    let published_mvcc_versions = dependency_diagnostics.published_storage_versions
        + dependency_diagnostics.published_balance_versions
        + dependency_diagnostics.published_contract_versions;
    let post_consensus_speedup = if totals.post_consensus.is_zero() {
        0.0
    } else {
        totals.serial.as_secs_f64() / totals.post_consensus.as_secs_f64()
    };
    let reuse_rate = if totals.speculative_results == 0 {
        0.0
    } else {
        totals.reused as f64 / totals.speculative_results as f64
    };
    let replay_rate = totals.replayed as f64 / total_transactions;
    let prediction_precision = if totals.predicted == 0 {
        0.0
    } else {
        totals.matched as f64 / totals.predicted as f64
    };
    let prediction_coverage = if totals.decided == 0 {
        0.0
    } else {
        totals.matched as f64 / totals.decided as f64
    };
    let edge_density = if totals.edges.possible_pairs == 0 {
        0.0
    } else {
        totals.edges.materialized as f64 / totals.edges.possible_pairs as f64
    };
    let deadline_hit_rate = totals.preconsensus_deadline_hits as f64 / blocks.len() as f64;
    let total_consensus_critical =
        totals.planning_overhang + totals.preexecution + totals.preexecution_feedback;
    let average_consensus_critical = total_consensus_critical.as_secs_f64() / blocks.len() as f64;
    let average_budget_fraction =
        average_consensus_critical / measurement.consensus_window.as_secs_f64();

    let mut profile_pairs = totals.edges.profile_pairs.into_iter().collect::<Vec<_>>();
    profile_pairs.sort_by(|left, right| {
        right
            .1
            .materialized
            .cmp(&left.1.materialized)
            .then_with(|| left.0.cmp(&right.0))
    });

    println!();
    println!("============================================================");
    println!(" MiniWarehouse Brick-5C.7 block-local MVCC measurement");
    println!("============================================================");
    println!("runtime model:                 real MiniWarehouse CosmWasm Wasm");
    println!("Wasm artifact:                 {}", wasm_path.display());
    println!("Wasm bytes:                    {}", wasm.len());
    println!(
        "total transactions:            {}",
        measurement.total_transactions
    );
    println!("blocks:                        {}", blocks.len());
    println!("max tx / block:                {}", measurement.block_size);
    println!(
        "block interval:                {:.3} ms",
        measurement.block_interval.as_secs_f64() * 1e3
    );
    println!(
        "consensus window:              {:.3} ms",
        measurement.consensus_window.as_secs_f64() * 1e3
    );
    println!(
        "arrival rate:                  {:.2} tx/s",
        measurement.arrival_tps
    );
    println!(
        "initial mempool backlog:       {} tx",
        measurement.initial_backlog_transactions
    );
    println!(
        "total worker budget:           {}",
        measurement.total_workers
    );
    println!("post-consensus validation:     1 worker (serial)");
    println!(
        "post-commit worker budget:     {}",
        measurement.preexecution_workers
    );
    println!(
        "post-commit execution workers:  {}",
        measurement.effective_preexecution_workers()
    );
    println!("dependency executor:           READY-DAG / block-local MVCC");
    println!(
        "symbolic hard soften after:    {} concrete observations",
        measurement.symbolic_hard_soften_after
    );
    println!(
        "warehouses:                    {}",
        workload.scale.warehouse_count
    );
    println!(
        "districts / warehouse:         {}",
        workload.scale.districts_per_warehouse
    );
    println!(
        "customers / district:          {}",
        workload.scale.customers_per_district
    );
    println!(
        "items / warehouse:             {}",
        workload.scale.items_per_warehouse
    );
    println!(
        "N+1 graph planning overlaps N validate: {}",
        if measurement.total_workers > 1 {
            "YES"
        } else {
            "NO"
        }
    );
    println!("N+1 preexecution overlaps N validate: NO");
    println!("preexecution snapshots committed N state: YES");
    println!(
        "require preexec within consensus: {}",
        if measurement.require_preexecution_deadline {
            "YES"
        } else {
            "NO"
        }
    );
    println!(
        "overlapped planning transitions:  {}",
        totals.overlapped_planning_transitions
    );
    println!();
    println!("Sparse Wasm bootstrap (not included in measured block timings)");
    println!("  seed transactions / engine: {:>10}", bootstrap.len());
    println!(
        "  touched warehouses:          {:>10}",
        bootstrap_resources.warehouses.len()
    );
    println!(
        "  touched districts:           {:>10}",
        bootstrap_resources.districts.len()
    );
    println!(
        "  touched customers:           {:>10}",
        bootstrap_resources.customers.len()
    );
    println!(
        "  touched stock keys:          {:>10}",
        bootstrap_resources.stocks.len()
    );
    println!(
        "  both engines bootstrap wall: {:>10.3} ms",
        bootstrap_elapsed.as_secs_f64() * 1e3
    );
    println!();
    println!("Workload");
    for (operation, count) in operation_counts {
        println!("  {operation:<18} {count:>8}");
    }
    println!();
    println!("Mempool / inclusion latency (virtual time)");
    println!(
        "  p50 admission -> proposal:   {:>10.3} ms",
        percentile_u64(&proposal_waits, 0.50) as f64 / 1e6
    );
    println!(
        "  p95 admission -> proposal:   {:>10.3} ms",
        percentile_u64(&proposal_waits, 0.95) as f64 / 1e6
    );
    println!(
        "  p50 admission -> decision:   {:>10.3} ms",
        percentile_u64(&decision_waits, 0.50) as f64 / 1e6
    );
    println!(
        "  p95 admission -> decision:   {:>10.3} ms",
        percentile_u64(&decision_waits, 0.95) as f64 / 1e6
    );
    println!();
    println!("Prediction / planning / post-commit preexecution (all blocks)");
    println!(
        "  adapter/binding:             {:>10.3} ms",
        totals.planning.adapter.as_secs_f64() * 1e3
    );
    println!(
        "  candidate graph:             {:>10.3} ms",
        totals.planning.candidate_graph.as_secs_f64() * 1e3
    );
    println!(
        "  scheduler:                   {:>10.3} ms",
        totals.planning.scheduler.as_secs_f64() * 1e3
    );
    println!(
        "  schedule validation:         {:>10.3} ms",
        totals.planning.schedule_validation.as_secs_f64() * 1e3
    );
    println!(
        "  plan conversion:             {:>10.3} ms",
        totals.planning.plan_conversion.as_secs_f64() * 1e3
    );
    println!(
        "  planning hidden by prev N:   {:>10.3} ms",
        totals.planning_overlap.as_secs_f64() * 1e3
    );
    println!(
        "  planning overhang after N:   {:>10.3} ms",
        totals.planning_overhang.as_secs_f64() * 1e3
    );
    println!(
        "  post-commit pre-execution:   {:>10.3} ms",
        totals.preexecution.as_secs_f64() * 1e3
    );
    println!(
        "  preexecution feedback:       {:>10.3} ms",
        totals.preexecution_feedback.as_secs_f64() * 1e3
    );
    println!(
        "  post-replay feedback:        {:>10.3} ms",
        totals.reconciliation_feedback.as_secs_f64() * 1e3
    );
    println!(
        "  avg consensus-critical prep: {:>10.3} ms",
        average_consensus_critical * 1e3
    );
    println!(
        "  average consensus budget:    {:>9.1}%",
        100.0 * average_budget_fraction
    );
    println!(
        "  p50 critical prep / block:   {:>10.3} ms",
        percentile_duration(&totals.consensus_preparation_durations, 0.50).as_secs_f64() * 1e3
    );
    println!(
        "  p95 critical prep / block:   {:>10.3} ms",
        percentile_duration(&totals.consensus_preparation_durations, 0.95).as_secs_f64() * 1e3
    );
    println!(
        "  deadline hit rate:           {:>9.1}%",
        100.0 * deadline_hit_rate
    );
    println!(
        "  deadline misses:             {:>10}",
        totals.preconsensus_deadline_misses
    );
    println!();
    println!("Candidate graph across real block boundaries");
    println!(
        "  possible within-block pairs: {:>10}",
        totals.edges.possible_pairs
    );
    println!(
        "  materialized edges:          {:>10}",
        totals.edges.materialized
    );
    println!(
        "  edge density:                {:>9.2}%",
        100.0 * edge_density
    );
    let predicate_true_rate = if totals.edges.materialized == 0 {
        0.0
    } else {
        totals.edges.predicate_true as f64 / totals.edges.materialized as f64
    };
    let predicate_unknown_rate = if totals.edges.materialized == 0 {
        0.0
    } else {
        totals.edges.predicate_unknown as f64 / totals.edges.materialized as f64
    };
    println!(
        "  predicate True:              {:>10} ({:>5.1}%)",
        totals.edges.predicate_true,
        100.0 * predicate_true_rate
    );
    println!(
        "  predicate Unknown:           {:>10} ({:>5.1}%)",
        totals.edges.predicate_unknown,
        100.0 * predicate_unknown_rate
    );
    println!(
        "  historical False override:   {:>10}",
        totals.edges.historical_false_override
    );
    println!(
        "  Low / Soft / Hard:           {} / {} / {}",
        totals.edges.low, totals.edges.soft, totals.edges.hard
    );
    println!("  concrete profile-pair predicate diagnostics:");
    println!(
        "    {:<42} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>8}",
        "profile pair", "total", "True", "Unknown", "False", "Low", "Soft", "Hard", "Unk%"
    );
    for (pair, pair_stats) in profile_pairs.into_iter().take(15) {
        println!(
            "    {pair:<42} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7.1}%",
            pair_stats.materialized,
            pair_stats.predicate_true,
            pair_stats.predicate_unknown,
            pair_stats.historical_false_override,
            pair_stats.low,
            pair_stats.soft,
            pair_stats.hard,
            100.0 * pair_stats.unknown_rate(),
        );
    }
    println!();
    println!("Schedule / exposed parallelism");
    println!("  waves:                       {:>10}", totals.waves);
    println!("  average wave width:          {:>10.2}", avg_wave_width);
    println!(
        "  p50 wave width:              {:>10}",
        percentile_usize(&totals.wave_widths, 0.50)
    );
    println!(
        "  p95 wave width:              {:>10}",
        percentile_usize(&totals.wave_widths, 0.95)
    );
    println!(
        "  max wave width:              {:>10}",
        totals.wave_widths.last().copied().unwrap_or(0)
    );
    println!(
        "  exposed N/waves:             {:>10.2}x",
        exposed_parallelism
    );
    println!(
        "  execution dependencies:      {:>10}",
        totals.execution_dependencies
    );
    println!(
        "  hard execution dependencies: {:>10}",
        totals.hard_execution_dependencies
    );
    println!();
    println!("Pre-consensus speculative execution acceleration");
    println!(
        "  serial-equivalent work:      {:>10.3} ms",
        totals.serial.as_secs_f64() * 1e3
    );
    println!(
        "  actual parallel preexecute:  {:>10.3} ms",
        totals.preexecution.as_secs_f64() * 1e3
    );
    println!(
        "  dependency-DAG lower bound:  {:>10.3} ms",
        totals.theoretical_ideal.as_secs_f64() * 1e3
    );
    println!(
        "  theoretical speedup:         {:>10.2}x",
        preexecution_theoretical_speedup
    );
    println!(
        "  REALIZED preexec speedup:    {:>10.2}x",
        preexecution_realized_speedup
    );
    println!(
        "  preexec/theoretical:         {:>9.1}%",
        100.0 * preexecution_realization
    );
    println!(
        "  consensus-critical prep:     {:>10.3} ms",
        total_consensus_critical.as_secs_f64() * 1e3
    );
    println!();
    println!("Block-local MVCC dependency executor cost diagnosis");
    println!(
        "  NOTE: worker-stage times below are aggregate across workers and overlap in wall time."
    );
    println!(
        "  engine dependency total:     {:>10.3} ms",
        dependency_diagnostics.executor_total.as_secs_f64() * 1e3
    );
    println!(
        "  dependency plan/setup:       {:>10.3} ms",
        dependency_diagnostics.dependency_plan_setup.as_secs_f64() * 1e3
    );
    println!(
        "  worker phase wall:           {:>10.3} ms",
        dependency_diagnostics.worker_phase_wall.as_secs_f64() * 1e3
    );
    println!(
        "  execution phase wall excl fill:{:>8.3} ms",
        execution_phase_wall.as_secs_f64() * 1e3
    );
    println!(
        "  aggregate ready/lock wait:   {:>10.3} ms",
        dependency_diagnostics.aggregate_ready_wait.as_secs_f64() * 1e3
    );
    println!(
        "  aggregate visibility capture:{:>10.3} ms",
        dependency_diagnostics
            .aggregate_visibility_capture
            .as_secs_f64()
            * 1e3
    );
    println!(
        "  aggregate contract+receipt:  {:>10.3} ms",
        dependency_diagnostics
            .aggregate_contract_execution
            .as_secs_f64()
            * 1e3
    );
    println!(
        "  aggregate publish/unblock:   {:>10.3} ms",
        dependency_diagnostics
            .aggregate_publish_and_unblock
            .as_secs_f64()
            * 1e3
    );
    println!(
        "  aggregate measured stages:   {:>10.3} ms",
        dependency_diagnostics
            .aggregate_worker_stage_time()
            .as_secs_f64()
            * 1e3
    );
    println!(
        "  serial tx work sum:          {:>10.3} ms",
        totals.serial_transaction_work.as_secs_f64() * 1e3
    );
    println!(
        "  speculative contract cost / serial: {:>6.2}x",
        speculative_contract_cost_inflation
    );
    println!(
        "  effective contract concurrency:     {:>6.2}x",
        effective_contract_concurrency
    );
    println!(
        "  worker busy capacity:               {:>5.1}%",
        100.0 * measured_worker_busy_fraction
    );
    println!(
        "  worker ready/wait capacity:         {:>5.1}%",
        100.0 * measured_worker_wait_fraction
    );
    println!(
        "  validator/base-snapshot wrapper:{:>10.3} ms",
        preexecution_wrapper_overhead.as_secs_f64() * 1e3
    );
    println!(
        "  executor coordinator residual: {:>10.3} ms",
        executor_coordinator_residual.as_secs_f64() * 1e3
    );
    println!(
        "  max in-flight transactions:   {:>10}",
        dependency_diagnostics.max_in_flight
    );
    println!("  per-tx full-world deep copies:{:>10}", 0);
    println!("  historical write-set replays: {:>10}", 0);
    println!(
        "  visibility masks captured:    {:>10}",
        dependency_diagnostics.visibility_masks_captured
    );
    println!(
        "  visibility words copied:      {:>10}",
        dependency_diagnostics.visibility_words_copied
    );
    println!(
        "  visibility words / tx:        {:>10.2}",
        visibility_words_per_tx
    );
    println!(
        "  MVCC storage versions published:{:>8}",
        dependency_diagnostics.published_storage_versions
    );
    println!(
        "  MVCC balance versions published:{:>8}",
        dependency_diagnostics.published_balance_versions
    );
    println!(
        "  MVCC contract versions published:{:>7}",
        dependency_diagnostics.published_contract_versions
    );
    println!(
        "  total MVCC versions published: {:>10}",
        published_mvcc_versions
    );
    println!();
    println!("Contract/runtime hot-path diagnosis");
    println!("  NOTE: nested times below overlap; host/MVCC callbacks occur inside Wasm entrypoint time.");
    println!(
        "  aggregate request execution:   {:>10.3} ms",
        contract_diagnostics
            .aggregate_request_execution
            .as_secs_f64()
            * 1e3
    );
    println!(
        "  aggregate receipt finalization: {:>9.3} ms",
        contract_diagnostics
            .aggregate_receipt_finalization
            .as_secs_f64()
            * 1e3
    );
    println!(
        "  outer contract timing residual: {:>9.3} ms",
        contract_outer_residual.as_secs_f64() * 1e3
    );
    println!(
        "  aggregate pre-contract setup:   {:>10.3} ms",
        contract_diagnostics
            .aggregate_precontract_setup
            .as_secs_f64()
            * 1e3
    );
    println!(
        "  aggregate response processing:  {:>10.3} ms",
        contract_diagnostics
            .aggregate_response_processing
            .as_secs_f64()
            * 1e3
    );
    println!(
        "  aggregate outcome assembly:     {:>10.3} ms",
        contract_diagnostics
            .aggregate_outcome_assembly
            .as_secs_f64()
            * 1e3
    );
    println!(
        "  aggregate backend construction: {:>10.3} ms",
        contract_diagnostics
            .aggregate_backend_construction
            .as_secs_f64()
            * 1e3
    );
    println!(
        "  aggregate Wasm instance acquire:{:>10.3} ms",
        contract_diagnostics
            .aggregate_wasm_instance_acquire
            .as_secs_f64()
            * 1e3
    );
    println!(
        "  aggregate Wasm entrypoint:      {:>10.3} ms",
        contract_diagnostics.aggregate_wasm_entrypoint.as_secs_f64() * 1e3
    );
    println!(
        "  aggregate Wasm recycle:         {:>10.3} ms",
        contract_diagnostics.aggregate_wasm_recycle.as_secs_f64() * 1e3
    );
    println!(
        "  aggregate Wasm runtime total:   {:>10.3} ms",
        wasm_runtime_total.as_secs_f64() * 1e3
    );
    println!(
        "  aggregate host storage callbacks:{:>9.3} ms",
        contract_diagnostics.aggregate_host_storage.as_secs_f64() * 1e3
    );
    println!(
        "  aggregate host queries:         {:>10.3} ms",
        contract_diagnostics.aggregate_host_query.as_secs_f64() * 1e3
    );
    println!(
        "  aggregate tx-lock wait in host: {:>10.3} ms",
        contract_diagnostics
            .aggregate_transaction_lock_wait
            .as_secs_f64()
            * 1e3
    );
    println!(
        "  aggregate MVCC storage point:   {:>10.3} ms",
        contract_diagnostics
            .aggregate_mvcc_storage_point
            .as_secs_f64()
            * 1e3
    );
    println!(
        "  aggregate MVCC storage range:   {:>10.3} ms",
        contract_diagnostics
            .aggregate_mvcc_storage_range
            .as_secs_f64()
            * 1e3
    );
    println!(
        "  aggregate MVCC balance point:   {:>10.3} ms",
        contract_diagnostics
            .aggregate_mvcc_balance_point
            .as_secs_f64()
            * 1e3
    );
    println!(
        "  aggregate MVCC all balances:    {:>10.3} ms",
        contract_diagnostics
            .aggregate_mvcc_all_balances
            .as_secs_f64()
            * 1e3
    );
    println!(
        "  aggregate MVCC contract lookup: {:>10.3} ms",
        contract_diagnostics.aggregate_mvcc_contract.as_secs_f64() * 1e3
    );
    println!(
        "  aggregate MVCC lock wait:       {:>10.3} ms",
        contract_diagnostics.aggregate_mvcc_lock_wait.as_secs_f64() * 1e3
    );
    println!(
        "  aggregate MVCC publish:         {:>10.3} ms",
        contract_diagnostics.aggregate_mvcc_publish.as_secs_f64() * 1e3
    );
    println!(
        "  Wasm instance acquires:         {:>10}",
        contract_diagnostics.wasm_instance_acquires
    );
    println!(
        "  Wasm entrypoint calls:          {:>10} ({:.2}/tx)",
        contract_diagnostics.wasm_entrypoint_calls, wasm_calls_per_tx
    );
    println!(
        "  Wasm instance recycles:         {:>10}",
        contract_diagnostics.wasm_instance_recycles
    );
    println!(
        "  Wasm cache pinned/mem/fs/miss:  {} / {} / {} / {}",
        contract_diagnostics.wasm_cache_pinned_hits,
        contract_diagnostics.wasm_cache_memory_hits,
        contract_diagnostics.wasm_cache_fs_hits,
        contract_diagnostics.wasm_cache_misses
    );
    println!(
        "  host storage get/scan/next/set/remove: {} / {} / {} / {} / {}",
        contract_diagnostics.host_storage_gets,
        contract_diagnostics.host_storage_scans,
        contract_diagnostics.host_storage_nexts,
        contract_diagnostics.host_storage_sets,
        contract_diagnostics.host_storage_removes
    );
    println!(
        "  host storage ops / tx:          {:>10.2}",
        host_storage_ops_per_tx
    );
    println!(
        "  host query calls:               {:>10}",
        contract_diagnostics.host_queries
    );
    println!(
        "  MVCC storage point reads:       {:>10}",
        contract_diagnostics.mvcc_storage_point_reads
    );
    println!(
        "  MVCC point version hits:        {:>10}",
        contract_diagnostics.mvcc_storage_point_hits
    );
    println!(
        "  MVCC point base fallbacks:      {:>10}",
        contract_diagnostics.mvcc_storage_base_fallbacks
    );
    println!(
        "  MVCC storage range reads:       {:>10}",
        contract_diagnostics.mvcc_storage_range_reads
    );
    println!(
        "  MVCC balance/all/contract reads: {} / {} / {}",
        contract_diagnostics.mvcc_balance_reads,
        contract_diagnostics.mvcc_all_balances_reads,
        contract_diagnostics.mvcc_contract_reads
    );
    println!(
        "  receipt access records:         {:>10}",
        contract_diagnostics.receipt_access_records
    );
    println!(
        "  receipt read dependencies:      {:>10}",
        contract_diagnostics.receipt_read_dependencies
    );
    println!(
        "  receipt storage/balance/contract writes: {} / {} / {}",
        contract_diagnostics.receipt_storage_writes,
        contract_diagnostics.receipt_balance_writes,
        contract_diagnostics.receipt_created_contracts
    );
    println!();
    println!("Prediction quality");
    println!("  predicted receipts:          {:>10}", totals.predicted);
    println!("  matched decided txs:         {:>10}", totals.matched);
    println!(
        "  discarded predictions:       {:>10}",
        totals.discarded_predictions
    );
    println!(
        "  missing predictions:         {:>10}",
        totals.missing_predictions
    );
    println!(
        "  precision:                   {:>9.1}%",
        100.0 * prediction_precision
    );
    println!(
        "  coverage:                    {:>9.1}%",
        100.0 * prediction_coverage
    );
    let accounted_post_consensus = totals.receipt_matching
        + totals.validation
        + totals.replay_or_missing
        + totals.commit_reused
        + totals.reconciliation_bookkeeping
        + totals.executor_wrapper_overhead
        + totals.canonical_fallback;
    let timing_accounting_gap = totals
        .post_consensus
        .saturating_sub(accounted_post_consensus);

    println!();
    println!("Post-consensus critical-path acceleration");
    println!(
        "  SERIAL baseline:             {:>10.3} ms",
        totals.serial.as_secs_f64() * 1e3
    );
    println!(
        "  adaptive receipt matching:   {:>10.3} ms",
        totals.receipt_matching.as_secs_f64() * 1e3
    );
    println!(
        "  adaptive validation:         {:>10.3} ms",
        totals.validation.as_secs_f64() * 1e3
    );
    println!(
        "  adaptive replay/missing:     {:>10.3} ms",
        totals.replay_or_missing.as_secs_f64() * 1e3
    );
    println!(
        "  adaptive reused commit:      {:>10.3} ms",
        totals.commit_reused.as_secs_f64() * 1e3
    );
    println!(
        "  reconciliation bookkeeping:  {:>10.3} ms",
        totals.reconciliation_bookkeeping.as_secs_f64() * 1e3
    );
    println!(
        "  executor/report wrapper:     {:>10.3} ms",
        totals.executor_wrapper_overhead.as_secs_f64() * 1e3
    );
    println!(
        "  canonical deadline fallback: {:>10.3} ms",
        totals.canonical_fallback.as_secs_f64() * 1e3
    );
    println!(
        "  timing accounting gap:       {:>10.3} ms",
        timing_accounting_gap.as_secs_f64() * 1e3
    );
    println!(
        "  ADAPTIVE total:              {:>10.3} ms",
        totals.post_consensus.as_secs_f64() * 1e3
    );
    println!(
        "  POST-CONSENSUS SPEEDUP:      {:>10.2}x",
        post_consensus_speedup
    );
    println!();
    println!("Speculation quality");
    println!(
        "  speculative results:         {:>10}",
        totals.speculative_results
    );
    println!("  reused results:              {:>10}", totals.reused);
    println!("  invalidated results:         {:>10}", totals.invalidated);
    println!("  replayed transactions:       {:>10}", totals.replayed);
    println!(
        "  no usable receipt:           {:>10}",
        totals.canonical_missing
    );
    let invalidation_rate = if totals.speculative_results == 0 {
        0.0
    } else {
        totals.invalidated as f64 / totals.speculative_results as f64
    };
    println!(
        "  reuse rate:                  {:>9.1}%",
        100.0 * reuse_rate
    );
    println!(
        "  invalidation rate:           {:>9.1}%",
        100.0 * invalidation_rate
    );
    println!(
        "  replay rate:                 {:>9.1}%",
        100.0 * replay_rate
    );
    println!();
    println!("Invalidation attribution (validation conflicts, not just transactions)");
    println!(
        "  invalidated transactions:    {:>10}",
        totals.invalidations.invalidated_transactions
    );
    println!(
        "  concrete validation conflicts:{:>10}",
        totals.invalidations.validation_conflicts
    );
    println!(
        "  txs with unguarded predecessor:{:>9}",
        totals.invalidations.invalidated_with_unguarded_conflict
    );
    println!(
        "  txs with cascade candidate:  {:>10}",
        totals.invalidations.invalidated_with_cascade_candidate
    );
    println!(
        "  txs with unattributed conflict:{:>8}",
        totals.invalidations.invalidated_with_unattributed_conflict
    );
    println!(
        "  guarded by execution dependency:{:>8}",
        totals.invalidations.guarded_by_execution_dependency
    );
    println!(
        "  unguarded predecessor conflicts:{:>8}",
        totals.invalidations.unguarded_predecessor
    );
    println!(
        "  cascade: guarded predecessor replayed:{:>5}",
        totals.invalidations.cascade_from_replayed_dependency
    );
    println!(
        "  guarded+reused unexplained:   {:>10}",
        totals.invalidations.guarded_reused_unexplained
    );
    println!(
        "  unattributed conflicts:       {:>10}",
        totals.invalidations.unattributed
    );
    println!(
        "  writer level relation same/earlier/later: {} / {} / {}",
        totals.invalidations.same_wave,
        totals.invalidations.earlier_wave,
        totals.invalidations.later_wave
    );

    print_ranked_counts("  cause breakdown:", &totals.invalidations.by_cause, 12);
    print_ranked_counts(
        "  predecessor -> invalidated operation:",
        &totals.invalidations.by_operation_pair,
        12,
    );
    print_ranked_counts(
        "  failing dependency:",
        &totals.invalidations.by_dependency,
        12,
    );
    print_ranked_counts(
        "  candidate relation for attributed predecessor:",
        &totals.invalidations.by_candidate_relation,
        12,
    );
    print_ranked_counts(
        "  execution guard for attributed predecessor:",
        &totals.invalidations.by_execution_guard,
        12,
    );

    println!("  invalidation rate by speculative wave width:");
    println!(
        "    {:>8} {:>10} {:>12} {:>10}",
        "width", "txs", "invalidated", "rate"
    );
    for (width, population) in &totals.invalidations.wave_width_population {
        let invalidated = totals
            .invalidations
            .wave_width_invalidated
            .get(width)
            .copied()
            .unwrap_or_default();
        let rate = if *population == 0 {
            0.0
        } else {
            invalidated as f64 / *population as f64
        };
        println!(
            "    {:>8} {:>10} {:>12} {:>9.1}%",
            width,
            population,
            invalidated,
            100.0 * rate
        );
    }

    println!("  invalidation rate by canonical position decile:");
    println!(
        "    {:>8} {:>10} {:>12} {:>10}",
        "decile", "txs", "invalidated", "rate"
    );
    for decile in 0..10 {
        let population = totals.invalidations.position_population[decile];
        let invalidated = totals.invalidations.position_invalidated[decile];
        let rate = if population == 0 {
            0.0
        } else {
            invalidated as f64 / population as f64
        };
        println!(
            "    {:>3}-{:>3}% {:>10} {:>12} {:>9.1}%",
            decile * 10,
            (decile + 1) * 10,
            population,
            invalidated,
            100.0 * rate
        );
    }
    println!("============================================================");
    println!("NOTE: all block/consensus/mempool times are virtual protocol time; only computation is measured with wall-clock Instant.");
    println!("NOTE: ACG_MW_CONSENSUS_MS is an independent configurable design window for completing post-commit pre-execution; it is not constrained to the block batching interval.");
    println!("NOTE: only N+1 graph/schedule planning may overlap validation of N. N+1 pre-execution starts after N commits and snapshots the actual canonical post-N state.");
    println!("NOTE: the measured execution path uses the real MiniWarehouse CosmWasm Wasm artifact; sparse bootstrap only avoids initializing untouched keys in the large benchmark domain.");
    println!("NOTE: pre-consensus speculative-execution speedup and post-consensus critical-path speedup are separate metrics and are not divided into one another.");
    println!("NOTE: invalidation predecessor attribution selects the nearest canonical predecessor whose prepared write set touches the concrete failed dependency.");
    println!("NOTE: an explicit execution dependency guarantees that predecessor completed before the victim launched; a replayed guarded predecessor is therefore reported as a cascade candidate.");
    println!("NOTE: scheduler waves are diagnostic dependency levels only. The executor launches ready transactions across levels without global barriers and resolves completed earlier-canonical versions lazily through block-local MVCC.");
    println!("NOTE: dependency cost diagnostics intentionally measure the current implementation's per-transaction deep snapshot clone plus replay of completed earlier versions; aggregate worker-stage times overlap and must not be summed as wall-clock components.");
    println!("NOTE: the current post-consensus validator is intentionally single-threaded. Parallel validation remains a future Brick-5 optimization; these diagnostics come first.");
    println!();
}
