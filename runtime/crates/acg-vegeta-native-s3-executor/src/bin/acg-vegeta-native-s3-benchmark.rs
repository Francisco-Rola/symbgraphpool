use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    io::{self, BufRead, BufReader, BufWriter, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use acg_core::ConflictKinds;
use acg_cosmwasm_engine::{
    AccessKind, Address, BlockContext, BundleCall, ContractExecutionDiagnostics, CosmWasmEngine,
    DependencyPreexecutionDiagnostics, EngineConfig, ParallelExecutionConfig, ScopedBundleCall,
    SpeculativeTxResult, TransactionId, WasmInstanceLifecycle,
};
use acg_runtime_feedback::{AccessConflictDetector, ObservedConflict, TraceConflictConfig};
use acg_vegeta_native_s3_executor::{ComputeCalibration, ComputeMetric};
use acg_validator_sim::{
    DirectDagBlockExecutor, DirectDagExecutionDiagnostics, ExecutionDependency,
    ExecutionDependencyClass, ExecutionPlan,
    ExecutionWave, PendingTransaction, ProducedBlock, SerialBlockExecutor,
    SpeculativeParallelBlockExecutor,
};
use cosmwasm_std::{Binary, Coin, Uint128};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const FIRST_BLOCK: u64 = 16_774_645;
const LAST_BLOCK: u64 = 16_774_745;
const EXPECTED_TRANSACTIONS: usize = 13_783;
const DEFAULT_STRATEGIES: [&str; 7] = [
    "serial",
    "aria-fb",
    "vegeta",
    "exact-access",
    "static",
    "probability-only",
    "cost-aware",
];

type AnyError = Box<dyn std::error::Error>;
fn invalid(msg: impl Into<String>) -> AnyError {
    io::Error::new(io::ErrorKind::InvalidInput, msg.into()).into()
}

#[derive(Clone, Debug, Deserialize)]
struct Manifest {
    wasm_artifacts: BTreeMap<String, String>,
    instances: Vec<InstanceSpec>,
    #[serde(default)]
    bank_seeds: Vec<BankSeed>,
    #[serde(default)]
    priming_calls: Vec<CallSpec>,
}
#[derive(Clone, Debug, Deserialize)]
struct InstanceSpec {
    instance_id: String,
    family: String,
    instantiate_msg: Value,
}
#[derive(Clone, Debug, Deserialize)]
struct BankSeed {
    address: String,
    denom: String,
    amount: String,
}
#[derive(Clone, Debug, Deserialize)]
struct ExecutionBlock {
    block_number: u64,
    timestamp: u64,
    transactions: Vec<ExecutionTx>,
}
#[derive(Clone, Debug, Deserialize)]
struct ExecutionTx {
    tx_index: usize,
    tx_hash: String,
    source_failed: bool,
    calls: Vec<CallSpec>,
    #[serde(default)]
    skipped_actions: usize,
}
#[derive(Clone, Debug, Deserialize)]
struct CallSpec {
    kind: String,
    #[serde(default)]
    family: Option<String>,
    #[serde(default)]
    instance_id: Option<String>,
    #[serde(default)]
    sender: Option<String>,
    #[serde(default)]
    msg: Option<Value>,
    #[serde(default)]
    funds: Vec<CoinSpec>,
    #[serde(default)]
    from: Option<String>,
    #[serde(default)]
    to: Option<String>,
    #[serde(default)]
    coins: Vec<CoinSpec>,
    #[serde(default)]
    source_revert_scope_action_id: Option<u64>,
}
#[derive(Clone, Debug, Deserialize)]
struct CoinSpec {
    denom: String,
    amount: String,
}

#[derive(Clone, Debug)]
struct Args {
    repo_root: PathBuf,
    manifest: PathBuf,
    execution_plan: PathBuf,
    symbolic_dir: PathBuf,
    output: PathBuf,
    workers: usize,
    samples: usize,
    cutoff: Duration,
    probability_threshold: f64,
    cost_bypass_speedup: f64,
    order_seed: u64,
    strategies: Vec<String>,
    compute_weights: Option<PathBuf>,
    compute_metric: ComputeMetric,
    compute_scale: f64,
    compute_base_total_nanos: u64,
    compute_iterations_per_nano: Option<f64>,
    runtime_profile: bool,
}

#[derive(Clone, Debug, Deserialize)]
struct SymbolicDocument {
    contract: String,
    #[serde(default)]
    profiles: Vec<SymbolicProfile>,
}
#[derive(Clone, Debug, Deserialize)]
struct SymbolicProfile {
    entrypoint: String,
    #[serde(default)]
    accesses: Vec<SymbolicAccess>,
}
#[derive(Clone, Debug, Deserialize)]
struct SymbolicAccess {
    kind: String,
    resource: String,
    key: SymbolicKey,
}
#[derive(Clone, Debug, Deserialize)]
struct SymbolicKey {
    #[serde(default)]
    semantic_name: Option<String>,
    #[serde(default)]
    depends_on: Option<SymbolicDependency>,
}
#[derive(Clone, Debug, Deserialize)]
struct SymbolicDependency {
    #[serde(default)]
    origin_input: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct PredictedLocation {
    scope: String,
    resource: String,
    key: String,
}
#[derive(Clone, Debug)]
struct PredictedAccess {
    location: PredictedLocation,
    write: bool,
}

#[derive(Default)]
struct SymbolicPredictor {
    profiles: BTreeMap<(String, String), Vec<SymbolicAccess>>,
}

#[derive(Clone, Debug, Default)]
struct PairStats {
    observations: u64,
    conflicts: u64,
}
#[derive(Clone, Debug, Default)]
struct CostStats {
    observations: u64,
    total_nanos: u128,
}
#[derive(Default)]
struct FeedbackModel {
    pairs: BTreeMap<(String, String), PairStats>,
    costs: BTreeMap<String, CostStats>,
    global_cost: CostStats,
}

#[derive(Debug, Serialize)]
struct Record {
    schema_version: u32,
    dataset: &'static str,
    sample: usize,
    block_number: u64,
    strategy: String,
    workers: usize,
    wasm_instance_lifecycle: &'static str,
    compute_calibration_metric: &'static str,
    compute_scale: f64,
    compute_base_total_nanos: u64,
    compute_iterations_per_nano: f64,
    compute_block_iterations: u64,
    transactions: usize,
    semantic_calls: usize,
    skipped_actions: usize,
    matched_serial_nanos: u64,
    strategy_total_nanos: u64,
    matched_serial_speedup: f64,
    planning_nanos: u64,
    preexecution_nanos: u64,
    reconciliation_nanos: u64,
    post_consensus_nanos: u64,
    cutoff_overrun_nanos: u64,
    prepared_receipts: u64,
    reused_receipts: u64,
    replayed_transactions: u64,
    canonical_transactions: u64,
    discovered_conflicts: u64,
    reference_conflicts: u64,
    dependency_edges: usize,
    waves: usize,
    max_wave_width: usize,
    serial_bypassed: bool,
    projected_speedup: Option<f64>,
    serial_equivalent: bool,
    feedback_scope: &'static str,
    symbolic_source: &'static str,
    planning_source: &'static str,
    phase_model: &'static str,
    consensus_cutoff_nanos: u64,
    pre_consensus_nanos: u64,
    consensus_bottleneck_nanos: u64,
    post_consensus_speedup: f64,
    feedback_nanos: u64,
    probability_threshold: f64,
    cost_bypass_speedup: f64,
    strategy_order_seed: u64,
    runtime_profile: Option<RuntimeProfileRecord>,
    evaluation_config_id: &'static str,
}

#[derive(Clone, Debug, Default, Serialize)]
struct RuntimeProfileRecord {
    profile_kind: String,
    worker_phase_wall_nanos: u64,
    aggregate_ready_wait_nanos: u64,
    aggregate_transaction_service_nanos: u64,
    aggregate_visibility_capture_nanos: u64,
    aggregate_publish_and_unblock_nanos: u64,
    aggregate_request_execution_nanos: u64,
    aggregate_receipt_finalization_nanos: u64,
    aggregate_backend_construction_nanos: u64,
    aggregate_wasm_instance_acquire_nanos: u64,
    aggregate_wasm_entrypoint_nanos: u64,
    aggregate_host_storage_nanos: u64,
    aggregate_host_query_nanos: u64,
    aggregate_transaction_lock_wait_nanos: u64,
    aggregate_canonical_state_read_lock_wait_nanos: u64,
    aggregate_canonical_state_read_hold_nanos: u64,
    aggregate_mvcc_lock_wait_nanos: u64,
    aggregate_mvcc_publish_nanos: u64,
    aggregate_commit_lock_wait_nanos: u64,
    aggregate_commit_lock_hold_nanos: u64,
    commit_batches: u64,
    commit_write_sets: u64,
    max_in_flight: usize,
    wasm_instance_acquires: u64,
    wasm_instance_reuse_hits: u64,
    wasm_instance_pool_misses: u64,
    canonical_state_reads: u64,
}

fn runtime_profile_from_contract(
    kind: &str,
    contract: &ContractExecutionDiagnostics,
) -> RuntimeProfileRecord {
    RuntimeProfileRecord {
        profile_kind: kind.to_owned(),
        aggregate_request_execution_nanos: nanos(contract.aggregate_request_execution),
        aggregate_receipt_finalization_nanos: nanos(contract.aggregate_receipt_finalization),
        aggregate_backend_construction_nanos: nanos(contract.aggregate_backend_construction),
        aggregate_wasm_instance_acquire_nanos: nanos(contract.aggregate_wasm_instance_acquire),
        aggregate_wasm_entrypoint_nanos: nanos(contract.aggregate_wasm_entrypoint),
        aggregate_host_storage_nanos: nanos(contract.aggregate_host_storage),
        aggregate_host_query_nanos: nanos(contract.aggregate_host_query),
        aggregate_transaction_lock_wait_nanos: nanos(contract.aggregate_transaction_lock_wait),
        aggregate_canonical_state_read_lock_wait_nanos: nanos(
            contract.aggregate_canonical_state_read_lock_wait,
        ),
        aggregate_canonical_state_read_hold_nanos: nanos(
            contract.aggregate_canonical_state_read_hold,
        ),
        aggregate_mvcc_lock_wait_nanos: nanos(contract.aggregate_mvcc_lock_wait),
        aggregate_mvcc_publish_nanos: nanos(contract.aggregate_mvcc_publish),
        wasm_instance_acquires: contract.wasm_instance_acquires,
        wasm_instance_reuse_hits: contract.wasm_instance_reuse_hits,
        wasm_instance_pool_misses: contract.wasm_instance_pool_misses,
        canonical_state_reads: contract.canonical_state_reads,
        ..RuntimeProfileRecord::default()
    }
}

fn runtime_profile_from_dependency(
    diagnostics: &DependencyPreexecutionDiagnostics,
) -> RuntimeProfileRecord {
    let mut profile = runtime_profile_from_contract("dependency-mvcc", &diagnostics.contract);
    profile.worker_phase_wall_nanos = nanos(diagnostics.worker_phase_wall);
    profile.aggregate_ready_wait_nanos = nanos(diagnostics.aggregate_ready_wait);
    profile.aggregate_transaction_service_nanos = nanos(diagnostics.aggregate_contract_execution);
    profile.aggregate_visibility_capture_nanos = nanos(diagnostics.aggregate_visibility_capture);
    profile.aggregate_publish_and_unblock_nanos = nanos(diagnostics.aggregate_publish_and_unblock);
    profile.max_in_flight = diagnostics.max_in_flight;
    profile
}

fn runtime_profile_from_direct(diagnostics: &DirectDagExecutionDiagnostics) -> RuntimeProfileRecord {
    let mut profile = runtime_profile_from_contract("exact-direct", &diagnostics.contract);
    profile.worker_phase_wall_nanos = nanos(diagnostics.worker_phase_wall);
    profile.aggregate_ready_wait_nanos = nanos(diagnostics.aggregate_ready_wait);
    profile.aggregate_transaction_service_nanos = nanos(diagnostics.aggregate_transaction_service);
    profile.aggregate_commit_lock_wait_nanos = nanos(diagnostics.commit.lock_wait);
    profile.aggregate_commit_lock_hold_nanos = nanos(diagnostics.commit.lock_hold);
    profile.commit_batches = diagnostics.commit.batches;
    profile.commit_write_sets = diagnostics.commit.write_sets;
    profile.max_in_flight = diagnostics.max_in_flight;
    profile
}

#[derive(Default)]
struct StrategyMetrics {
    total: Duration,
    planning: Duration,
    preexecution: Duration,
    reconciliation: Duration,
    post_consensus: Duration,
    cutoff_overrun: Duration,
    pre_consensus: Duration,
    feedback: Duration,
    prepared_receipts: u64,
    reused_receipts: u64,
    replayed_transactions: u64,
    canonical_transactions: u64,
    discovered_conflicts: u64,
    reference_conflicts: u64,
    dependency_edges: usize,
    waves: usize,
    max_wave_width: usize,
    serial_bypassed: bool,
    projected_speedup: Option<f64>,
    runtime_profile: Option<RuntimeProfileRecord>,
}

fn parse_args() -> Result<Args, AnyError> {
    let mut repo_root = PathBuf::from(".");
    let mut manifest = None;
    let mut execution_plan = None;
    let mut symbolic_dir = PathBuf::from("benchmarks/symbolic/native-s3");
    let mut output = None;
    let mut workers = 6usize;
    let mut samples = 3usize;
    let mut cutoff_ms = 5_000u64;
    let mut probability_threshold = 0.50f64;
    let mut cost_bypass_speedup = 1.05f64;
    let mut order_seed = 2_026_082_501u64;
    let mut strategies = DEFAULT_STRATEGIES.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let mut compute_weights = None;
    let mut compute_metric = ComputeMetric::None;
    let mut compute_scale = 0.0_f64;
    let mut compute_base_total_ms = 1_000_u64;
    let mut compute_iterations_per_nano = None;
    let mut runtime_profile = false;
    let mut it = env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--repo-root" => repo_root = PathBuf::from(it.next().ok_or_else(|| invalid("missing --repo-root value"))?),
            "--manifest" => manifest = Some(PathBuf::from(it.next().ok_or_else(|| invalid("missing --manifest value"))?)),
            "--execution-plan" => execution_plan = Some(PathBuf::from(it.next().ok_or_else(|| invalid("missing --execution-plan value"))?)),
            "--symbolic-dir" => symbolic_dir = PathBuf::from(it.next().ok_or_else(|| invalid("missing --symbolic-dir value"))?),
            "--output" => output = Some(PathBuf::from(it.next().ok_or_else(|| invalid("missing --output value"))?)),
            "--workers" => workers = it.next().ok_or_else(|| invalid("missing --workers value"))?.parse()?,
            "--samples" => samples = it.next().ok_or_else(|| invalid("missing --samples value"))?.parse()?,
            "--consensus-cutoff-ms" => cutoff_ms = it.next().ok_or_else(|| invalid("missing --consensus-cutoff-ms value"))?.parse()?,
            "--probability-threshold" => probability_threshold = it.next().ok_or_else(|| invalid("missing --probability-threshold value"))?.parse()?,
            "--cost-bypass-speedup" => cost_bypass_speedup = it.next().ok_or_else(|| invalid("missing --cost-bypass-speedup value"))?.parse()?,
            "--order-seed" => order_seed = it.next().ok_or_else(|| invalid("missing --order-seed value"))?.parse()?,
            "--compute-weights" => compute_weights = Some(PathBuf::from(it.next().ok_or_else(|| invalid("missing --compute-weights value"))?)),
            "--compute-metric" => compute_metric = ComputeMetric::parse(&it.next().ok_or_else(|| invalid("missing --compute-metric value"))?)?,
            "--compute-scale" => compute_scale = it.next().ok_or_else(|| invalid("missing --compute-scale value"))?.parse()?,
            "--compute-base-total-ms" => compute_base_total_ms = it.next().ok_or_else(|| invalid("missing --compute-base-total-ms value"))?.parse()?,
            "--compute-iterations-per-nano" => compute_iterations_per_nano = Some(it.next().ok_or_else(|| invalid("missing --compute-iterations-per-nano value"))?.parse()?),
            "--runtime-profile" => runtime_profile = true,
            "--strategies" => {
                let value = it.next().ok_or_else(|| invalid("missing --strategies value"))?;
                strategies = value.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect();
            }
            "-h" | "--help" => return Err(invalid("usage: acg-vegeta-native-s3-benchmark --manifest FILE --execution-plan FILE --output FILE [--repo-root ROOT] [--symbolic-dir DIR] [--workers 6] [--samples 3] [--consensus-cutoff-ms 5000] [--probability-threshold 0.5] [--cost-bypass-speedup 1.05] [--order-seed 2026082501] [--strategies serial,aria-fb,vegeta,exact-access,exact-direct,static,probability-only,cost-aware] [--compute-weights FILE --compute-metric none|steps|gas --compute-scale X --compute-base-total-ms 1000 --compute-iterations-per-nano X] [--runtime-profile]")),
            _ => return Err(invalid(format!("unknown argument: {arg}"))),
        }
    }
    if workers == 0 || samples == 0 {
        return Err(invalid("workers and samples must be greater than zero"));
    }
    if !(0.0..=1.0).contains(&probability_threshold) {
        return Err(invalid("probability threshold must be in [0,1]"));
    }
    if !compute_scale.is_finite() || compute_scale < 0.0 {
        return Err(invalid("compute scale must be finite and non-negative"));
    }
    if strategies.is_empty() {
        return Err(invalid("at least one strategy must be selected"));
    }
    for strategy in &strategies {
        if !matches!(strategy.as_str(), "serial" | "aria-fb" | "vegeta" | "exact-access" | "exact-direct" | "static" | "probability-only" | "cost-aware") {
            return Err(invalid(format!("unknown strategy {strategy}")));
        }
    }
    Ok(Args {
        repo_root,
        manifest: manifest.ok_or_else(|| invalid("--manifest is required"))?,
        execution_plan: execution_plan.ok_or_else(|| invalid("--execution-plan is required"))?,
        symbolic_dir,
        output: output.ok_or_else(|| invalid("--output is required"))?,
        workers,
        samples,
        cutoff: Duration::from_millis(cutoff_ms),
        probability_threshold,
        cost_bypass_speedup,
        order_seed,
        strategies,
        compute_weights,
        compute_metric,
        compute_scale,
        compute_base_total_nanos: compute_base_total_ms.saturating_mul(1_000_000),
        compute_iterations_per_nano,
        runtime_profile,
    })
}

fn resolve(root: &Path, path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.is_absolute() { p } else { root.join(p) }
}
fn binary_json(v: &Value) -> Result<Binary, AnyError> {
    Ok(Binary::from(serde_json::to_vec(v)?))
}
fn coins(rows: &[CoinSpec]) -> Result<Vec<Coin>, AnyError> {
    rows.iter().map(|c| Ok(Coin { denom: c.denom.clone(), amount: Uint128::new(c.amount.parse::<u128>()?) })).collect()
}

fn build_call(spec: &CallSpec, addresses: &BTreeMap<String, Address>) -> Result<BundleCall, AnyError> {
    match spec.kind.as_str() {
        "execute" => {
            let iid = spec.instance_id.as_ref().ok_or_else(|| invalid("execute call missing instance_id"))?;
            Ok(BundleCall::Execute {
                sender: Address::new(spec.sender.clone().ok_or_else(|| invalid("execute call missing sender"))?),
                contract: addresses.get(iid).ok_or_else(|| invalid(format!("unknown instance {iid}")))?.clone(),
                funds: coins(&spec.funds)?,
                msg: binary_json(spec.msg.as_ref().ok_or_else(|| invalid("execute call missing msg"))?)?,
            })
        }
        "query" => {
            let iid = spec.instance_id.as_ref().ok_or_else(|| invalid("query call missing instance_id"))?;
            Ok(BundleCall::Query {
                contract: addresses.get(iid).ok_or_else(|| invalid(format!("unknown instance {iid}")))?.clone(),
                msg: binary_json(spec.msg.as_ref().ok_or_else(|| invalid("query call missing msg"))?)?,
            })
        }
        "bank_send" => Ok(BundleCall::BankSend {
            from: Address::new(spec.from.clone().ok_or_else(|| invalid("bank_send missing from"))?),
            to: Address::new(spec.to.clone().ok_or_else(|| invalid("bank_send missing to"))?),
            coins: coins(&spec.coins)?,
        }),
        "noop" => Ok(BundleCall::Noop),
        other => Err(invalid(format!("unsupported call kind {other}"))),
    }
}

fn read_execution_blocks(path: &Path) -> Result<Vec<ExecutionBlock>, AnyError> {
    let reader = BufReader::new(fs::File::open(path)?);
    let mut blocks: Vec<ExecutionBlock> = Vec::new();
    for line in reader.lines() {
        let line = line?;
        if !line.trim().is_empty() {
            blocks.push(serde_json::from_str(&line)?);
        }
    }
    if blocks.len() != 101 {
        return Err(invalid(format!("expected 101 S3 execution blocks, found {}", blocks.len())));
    }
    for (offset, block) in blocks.iter().enumerate() {
        let expected = FIRST_BLOCK + offset as u64;
        if block.block_number != expected {
            return Err(invalid(format!(
                "S3 block order mismatch at offset {offset}: {} != {expected}",
                block.block_number
            )));
        }
        for (position, tx) in block.transactions.iter().enumerate() {
            if tx.tx_index != position {
                return Err(invalid(format!(
                    "block {} tx index mismatch at position {position}: {}",
                    block.block_number, tx.tx_index
                )));
            }
            if tx.tx_hash.is_empty() {
                return Err(invalid(format!("block {} tx {position} has empty tx hash", block.block_number)));
            }
        }
    }
    if blocks.last().map(|b| b.block_number) != Some(LAST_BLOCK) {
        return Err(invalid("S3 execution plan does not end at the frozen final block"));
    }
    let transactions = blocks.iter().map(|b| b.transactions.len()).sum::<usize>();
    if transactions != EXPECTED_TRANSACTIONS {
        return Err(invalid(format!(
            "expected {EXPECTED_TRANSACTIONS} S3 transactions, found {transactions}"
        )));
    }
    Ok(blocks)
}

fn setup_engine(
    repo_root: &Path,
    manifest: &Manifest,
    raw_blocks: &[ExecutionBlock],
    calibration: &ComputeCalibration,
) -> Result<(CosmWasmEngine, Vec<ProducedBlock>), AnyError> {
    let engine = CosmWasmEngine::new(EngineConfig {
        gas_limit: u64::MAX,
        wasm_instance_lifecycle: WasmInstanceLifecycle::Reuse,
        ..EngineConfig::default()
    });
    let mut codes = BTreeMap::new();
    for (family, path) in &manifest.wasm_artifacts {
        codes.insert(family.clone(), engine.upload_wasm(fs::read(resolve(repo_root, path))?)?);
    }
    for seed in &manifest.bank_seeds {
        engine.set_balance(
            Address::new(seed.address.clone()),
            &[Coin { denom: seed.denom.clone(), amount: Uint128::new(seed.amount.parse()?) }],
        )?;
    }
    let setup_block = BlockContext {
        height: FIRST_BLOCK - 1,
        time_nanos: 1_678_170_000_000_000_000,
        chain_id: "vegeta-s3-native".to_owned(),
        transaction_index: Some(0),
    };
    let mut addresses = BTreeMap::new();
    for (i, spec) in manifest.instances.iter().enumerate() {
        let code = *codes.get(&spec.family).ok_or_else(|| invalid(format!("missing code for {}", spec.family)))?;
        let outcome = engine.instantiate(
            TransactionId(1_000_000 + i as u64),
            setup_block.clone(),
            Address::new("native-s3-admin"),
            code,
            None,
            spec.instance_id.clone(),
            vec![],
            binary_json(&spec.instantiate_msg)?,
        )?;
        addresses.insert(spec.instance_id.clone(), outcome.contract);
    }
    for (i, spec) in manifest.priming_calls.iter().enumerate() {
        engine.execute_bundle(
            TransactionId(2_000_000 + i as u64),
            setup_block.clone(),
            &[build_call(spec, &addresses)?],
        )?;
    }

    let mut blocks = Vec::with_capacity(raw_blocks.len());
    for block in raw_blocks {
        let mut transactions = Vec::with_capacity(block.transactions.len());
        for tx in &block.transactions {
            let compute_iterations = calibration.iterations_for(block.block_number, tx.tx_index, &tx.tx_hash)?;
            let mut scoped_calls = Vec::with_capacity(tx.calls.len() + usize::from(compute_iterations > 0));
            if compute_iterations > 0 {
                scoped_calls.push(ScopedBundleCall {
                    call: BundleCall::DeterministicCompute { iterations: compute_iterations },
                    source_revert_scope: None,
                });
            }
            scoped_calls.extend(tx.calls.iter().map(|spec| {
                Ok(ScopedBundleCall {
                    call: build_call(spec, &addresses)?,
                    source_revert_scope: spec.source_revert_scope_action_id,
                })
            }).collect::<Result<Vec<_>, AnyError>>()?);
            let tid = TransactionId(((block.block_number - FIRST_BLOCK) * 100_000 + tx.tx_index as u64) + 10_000_000);
            transactions.push(PendingTransaction {
                request: acg_cosmwasm_engine::ExecutionRequest::Bundle {
                    transaction_id: tid,
                    calls: scoped_calls,
                    source_failed: tx.source_failed,
                },
                admitted_at_nanos: 0,
                admission_sequence: tx.tx_index as u64,
            });
        }
        blocks.push(ProducedBlock {
            context: BlockContext {
                height: block.block_number,
                time_nanos: block.timestamp.saturating_mul(1_000_000_000),
                chain_id: "vegeta-s3-native".to_owned(),
                transaction_index: None,
            },
            transactions,
        });
    }
    Ok((engine, blocks))
}

fn normalize_entrypoint(value: &str) -> String {
    value.chars().filter(|c| c.is_ascii_alphanumeric()).flat_map(char::to_lowercase).collect()
}

impl SymbolicPredictor {
    fn load(root: &Path, symbolic_dir: &Path) -> Result<Self, AnyError> {
        let dir = if symbolic_dir.is_absolute() { symbolic_dir.to_path_buf() } else { root.join(symbolic_dir) };
        let mut profiles = BTreeMap::new();
        for entry in fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") { continue; }
            let doc: SymbolicDocument = serde_json::from_slice(&fs::read(&path)?)?;
            for profile in doc.profiles {
                profiles.insert((doc.contract.clone(), normalize_entrypoint(&profile.entrypoint)), profile.accesses);
            }
        }
        Ok(Self { profiles })
    }

    fn predict_tx(&self, tx: &ExecutionTx) -> Vec<PredictedAccess> {
        let mut out = Vec::new();
        for call in &tx.calls {
            if call.kind == "bank_send" {
                for coin in &call.coins {
                    for address in [call.from.as_deref(), call.to.as_deref()].into_iter().flatten() {
                        out.push(PredictedAccess {
                            location: PredictedLocation { scope: "bank".to_owned(), resource: address.to_owned(), key: coin.denom.clone() },
                            write: true,
                        });
                    }
                }
                continue;
            }
            if call.kind == "execute" {
                for coin in &call.funds {
                    if let Some(sender) = &call.sender {
                        out.push(PredictedAccess {
                            location: PredictedLocation { scope: "bank".to_owned(), resource: sender.clone(), key: coin.denom.clone() },
                            write: true,
                        });
                    }
                    if let Some(iid) = &call.instance_id {
                        out.push(PredictedAccess {
                            location: PredictedLocation { scope: "bank".to_owned(), resource: iid.clone(), key: coin.denom.clone() },
                            write: true,
                        });
                    }
                }
            }
            let Some(family) = call.family.as_ref() else { continue; };
            let Some(iid) = call.instance_id.as_ref() else { continue; };
            let action = call.msg.as_ref().and_then(Value::as_object).and_then(|m| m.keys().next()).cloned().unwrap_or_else(|| call.kind.clone());
            let entrypoint = normalize_entrypoint(&format!("{}::{action}", call.kind));
            let Some(accesses) = self.profiles.get(&(family.clone(), entrypoint)) else { continue; };
            for access in accesses {
                let key = resolve_symbolic_key(call, &access.key).unwrap_or_else(|| "*".to_owned());
                out.push(PredictedAccess {
                    location: PredictedLocation { scope: iid.clone(), resource: access.resource.clone(), key },
                    write: access.kind != "read",
                });
            }
        }
        out
    }
}

fn resolve_symbolic_key(call: &CallSpec, key: &SymbolicKey) -> Option<String> {
    let Some(dep) = key.depends_on.as_ref() else {
        return match key.semantic_name.as_deref() {
            Some("singleton") | None => Some("singleton".to_owned()),
            _ => Some("*".to_owned()),
        };
    };
    let expr = dep.origin_input.as_deref()?;
    let payload = call.msg.as_ref().and_then(Value::as_object).and_then(|m| m.values().next()).and_then(Value::as_object);
    fn atom(name: &str, call: &CallSpec, payload: Option<&serde_json::Map<String, Value>>) -> Option<String> {
        let name = name.trim();
        if name == "info.sender" { return call.sender.clone(); }
        let value = payload?.get(name)?;
        Some(match value {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            other => other.to_string(),
        })
    }
    if expr.starts_with('(') && expr.ends_with(')') {
        let inner = &expr[1..expr.len() - 1];
        let values = inner.split(',').map(|part| atom(part, call, payload)).collect::<Option<Vec<_>>>()?;
        return Some(values.join("|"));
    }
    atom(expr, call, payload)
}

fn predicted_conflict(left: &[PredictedAccess], right: &[PredictedAccess]) -> bool {
    left.iter().any(|a| right.iter().any(|b| {
        let same_namespace = a.location.scope == b.location.scope && a.location.resource == b.location.resource;
        let key_overlap = a.location.key == b.location.key || a.location.key == "*" || b.location.key == "*";
        same_namespace && key_overlap && (a.write || b.write)
    }))
}

fn static_edges(predictor: &SymbolicPredictor, block: &ExecutionBlock) -> BTreeSet<(usize, usize)> {
    let predicted = block.transactions.iter().map(|tx| predictor.predict_tx(tx)).collect::<Vec<_>>();
    let mut edges = BTreeSet::new();
    for left in 0..predicted.len() {
        for right in left + 1..predicted.len() {
            if predicted_conflict(&predicted[left], &predicted[right]) { edges.insert((left, right)); }
        }
    }
    edges
}

fn tx_signature(tx: &ExecutionTx) -> String {
    let mut parts = Vec::new();
    for call in tx.calls.iter().take(4) {
        let action = call.msg.as_ref().and_then(Value::as_object).and_then(|m| m.keys().next()).cloned().unwrap_or_else(|| call.kind.clone());
        parts.push(format!(
            "{}@{}:{}:{}",
            call.family.as_deref().unwrap_or("system"),
            call.instance_id.as_deref().or(call.from.as_deref()).unwrap_or("global"),
            call.kind,
            action
        ));
    }
    if tx.calls.len() > 4 { parts.push(format!("+{}", tx.calls.len() - 4)); }
    if parts.is_empty() { parts.push("empty".to_owned()); }
    parts.join("|")
}

fn signature_pair_key(left: &str, right: &str) -> (String, String) {
    if left <= right {
        (left.to_owned(), right.to_owned())
    } else {
        (right.to_owned(), left.to_owned())
    }
}

impl FeedbackModel {
    fn probability(&self, left: &str, right: &str, static_prior: bool) -> f64 {
        match self.pairs.get(&signature_pair_key(left, right)) {
            Some(stats) if stats.observations > 0 => stats.conflicts as f64 / stats.observations as f64,
            _ if static_prior => 0.75,
            _ => 0.0,
        }
    }
    fn mean_cost(&self, sig: &str) -> f64 {
        self.costs.get(sig).filter(|s| s.observations > 0).map(|s| s.total_nanos as f64 / s.observations as f64)
            .or_else(|| (self.global_cost.observations > 0).then(|| self.global_cost.total_nanos as f64 / self.global_cost.observations as f64))
            .unwrap_or(1.0)
    }
    fn probability_edges(&self, block: &ExecutionBlock, static_set: &BTreeSet<(usize, usize)>, threshold: f64) -> BTreeSet<(usize, usize)> {
        let sigs = block.transactions.iter().map(tx_signature).collect::<Vec<_>>();
        let mut edges = BTreeSet::new();
        for left in 0..sigs.len() {
            for right in left + 1..sigs.len() {
                let prior = static_set.contains(&(left, right));
                if self.probability(&sigs[left], &sigs[right], prior) >= threshold { edges.insert((left, right)); }
            }
        }
        edges
    }
    fn cost_edges(
        &self,
        block: &ExecutionBlock,
        static_set: &BTreeSet<(usize, usize)>,
        workers: usize,
    ) -> BTreeSet<(usize, usize)> {
        let sigs = block.transactions.iter().map(tx_signature).collect::<Vec<_>>();
        let mut edges = BTreeSet::new();
        for left in 0..sigs.len() {
            for right in left + 1..sigs.len() {
                let prior = static_set.contains(&(left, right));
                let p = self.probability(&sigs[left], &sigs[right], prior);
                if p <= 0.0 { continue; }
                let replay_cost = self.mean_cost(&sigs[right]);
                let serialization_cost = self.mean_cost(&sigs[left]).min(replay_cost) / workers.max(1) as f64;
                if p * replay_cost >= serialization_cost { edges.insert((left, right)); }
            }
        }
        edges
    }
    fn update(&mut self, block: &ExecutionBlock, conflicts: &[ObservedConflict], service_nanos: &[u64]) {
        let sigs = block.transactions.iter().map(tx_signature).collect::<Vec<_>>();
        let actual = conflicts.iter().map(|c| {
            let a = c.left.0 as usize; let b = c.right.0 as usize;
            if a < b { (a,b) } else { (b,a) }
        }).collect::<BTreeSet<_>>();
        for left in 0..sigs.len() {
            for right in left + 1..sigs.len() {
                let row = self
                    .pairs
                    .entry(signature_pair_key(&sigs[left], &sigs[right]))
                    .or_default();
                row.observations += 1;
                row.conflicts += if actual.contains(&(left, right)) { 1 } else { 0 };
            }
        }
        for (sig, nanos) in sigs.iter().zip(service_nanos.iter().copied()) {
            let row = self.costs.entry(sig.clone()).or_default();
            row.observations += 1; row.total_nanos += nanos as u128;
            self.global_cost.observations += 1; self.global_cost.total_nanos += nanos as u128;
        }
    }
}

fn plan_from_edges(count: usize, edges: &BTreeSet<(usize, usize)>) -> ExecutionPlan {
    if count == 0 { return ExecutionPlan { transaction_count: 0, waves: Vec::new(), dependencies: Vec::new() }; }
    let mut levels = vec![0usize; count];
    for &(left, right) in edges {
        levels[right] = levels[right].max(levels[left].saturating_add(1));
    }
    let max_level = *levels.iter().max().unwrap_or(&0);
    let mut waves = vec![Vec::new(); max_level + 1];
    for (index, level) in levels.into_iter().enumerate() { waves[level].push(index); }
    ExecutionPlan {
        transaction_count: count,
        waves: waves.into_iter().filter(|w| !w.is_empty()).map(|transaction_indices| ExecutionWave { transaction_indices }).collect(),
        dependencies: edges.iter().map(|&(left,right)| ExecutionDependency { predecessor_index: left, successor_index: right, class: ExecutionDependencyClass::Hard }).collect(),
    }
}
fn serial_plan(count: usize) -> ExecutionPlan {
    ExecutionPlan { transaction_count: count, waves: (0..count).map(|i| ExecutionWave { transaction_indices: vec![i] }).collect(), dependencies: Vec::new() }
}
fn fully_parallel_plan(count: usize) -> ExecutionPlan {
    if count == 0 { return ExecutionPlan { transaction_count: 0, waves: vec![], dependencies: vec![] }; }
    ExecutionPlan { transaction_count: count, waves: vec![ExecutionWave { transaction_indices: (0..count).collect() }], dependencies: Vec::new() }
}
fn conflict_edges(conflicts: &[ObservedConflict]) -> BTreeSet<(usize, usize)> {
    conflicts.iter().map(|c| {
        let left=c.left.0 as usize; let right=c.right.0 as usize;
        if left < right {(left,right)} else {(right,left)}
    }).collect()
}
fn aria_rule2(conflicts: &[ObservedConflict]) -> BTreeSet<usize> {
    #[derive(Default)] struct K { waw: bool, war: bool, raw: bool }
    let mut by = BTreeMap::<usize,K>::new();
    for c in conflicts {
        let right = c.right.0 as usize;
        let row = by.entry(right).or_default();
        row.waw |= c.conflict_kinds.contains(ConflictKinds::WRITE_WRITE);
        row.war |= c.conflict_kinds.contains(ConflictKinds::READ_WRITE);
        row.raw |= c.conflict_kinds.contains(ConflictKinds::WRITE_READ);
    }
    by.into_iter().filter_map(|(i,k)| (k.waw || (k.war && k.raw)).then_some(i)).collect()
}

type Footprint = BTreeSet<(String, u8, Vec<u8>, Option<Vec<u8>>, bool)>;
fn kind_code(kind: &AccessKind) -> u8 { match kind { AccessKind::StorageRead=>0, AccessKind::StorageScan=>1, AccessKind::StorageWrite=>2, AccessKind::StorageRemove=>3, AccessKind::BankRead=>4, AccessKind::BankWrite=>5 } }
fn footprint(receipt: &SpeculativeTxResult) -> Footprint {
    receipt.accesses.iter().map(|a| (a.contract.to_string(), kind_code(&a.kind), a.key.clone(), a.range_end.clone(), a.reverted)).collect()
}
fn plan_shape(plan: &ExecutionPlan) -> (usize, usize) {
    (plan.waves.len(), plan.waves.iter().map(|w| w.transaction_indices.len()).max().unwrap_or(0))
}
fn nanos(d: Duration) -> u64 { u64::try_from(d.as_nanos()).unwrap_or(u64::MAX) }

fn estimate_projected_speedup(model: &FeedbackModel, block: &ExecutionBlock, plan: &ExecutionPlan, workers: usize) -> f64 {
    if block.transactions.is_empty() { return 1.0; }
    let costs = block.transactions.iter().map(|tx| model.mean_cost(&tx_signature(tx))).collect::<Vec<_>>();
    let serial: f64 = costs.iter().sum();
    let mut cp = vec![0.0f64; costs.len()];
    let mut preds = vec![Vec::new(); costs.len()];
    for dep in &plan.dependencies { preds[dep.successor_index].push(dep.predecessor_index); }
    for i in 0..costs.len() {
        let base = preds[i].iter().map(|&p| cp[p]).fold(0.0f64, f64::max);
        cp[i] = base + costs[i];
    }
    let critical = cp.into_iter().fold(0.0f64, f64::max);
    let lower = critical.max(serial / workers.max(1) as f64);
    if lower > 0.0 { serial / lower } else { 1.0 }
}

fn ensure_successful_report(
    label: &str,
    report: &acg_validator_sim::BlockExecutionReport,
) -> Result<(), AnyError> {
    if report.failed() == 0 {
        return Ok(());
    }
    let failures = report
        .transactions
        .iter()
        .filter_map(|tx| tx.result.as_ref().err().map(|error| format!("{}:{error}", tx.transaction_index)))
        .collect::<Vec<_>>();
    Err(invalid(format!(
        "{label} block {} contains {} native execution failure(s): {}",
        report.block_height,
        report.failed(),
        failures.join("; ")
    )))
}

fn execute_serial(engine: &CosmWasmEngine, block: &ProducedBlock) -> Result<(acg_validator_sim::BlockExecutionReport, Duration), AnyError> {
    let started = Instant::now();
    let report = SerialBlockExecutor::new(engine.clone()).execute(block, &serial_plan(block.transactions.len()))?;
    let elapsed = started.elapsed();
    ensure_successful_report("matched serial reference", &report)?;
    Ok((report, elapsed))
}

fn speculative_run(
    engine: &CosmWasmEngine,
    block: &ProducedBlock,
    plan: &ExecutionPlan,
    workers: usize,
    cutoff: Duration,
    planning: Duration,
    collect_runtime_profile: bool,
) -> Result<StrategyMetrics, AnyError> {
    let total_started = Instant::now();
    let executor = SpeculativeParallelBlockExecutor::new(engine.clone(), ParallelExecutionConfig { workers });
    let remaining = cutoff.saturating_sub(planning);
    let pre_started = Instant::now();
    let prepared = executor.prepare_with_cutoff(block, plan, remaining)?;
    let preexecution = pre_started.elapsed();
    let eligible = planning.saturating_add(preexecution);
    let pre_consensus = eligible.min(cutoff);
    let overrun = eligible.saturating_sub(cutoff);
    let prepared_receipts = prepared.receipts.len() as u64;
    let runtime_profile = collect_runtime_profile
        .then(|| runtime_profile_from_dependency(&prepared.metrics.dependency_diagnostics));
    let rec_started = Instant::now();
    let rec = executor.validate_prepared(block, prepared)?;
    let reconciliation = rec_started.elapsed();
    ensure_successful_report("speculative reconciliation", &rec.block)?;
    let (waves,max_wave_width)=plan_shape(plan);
    Ok(StrategyMetrics {
        total: total_started.elapsed().saturating_add(planning),
        planning,
        preexecution,
        reconciliation,
        post_consensus: overrun.saturating_add(reconciliation),
        cutoff_overrun: overrun,
        pre_consensus,
        prepared_receipts,
        reused_receipts: rec.speculative.reused_results,
        replayed_transactions: rec.speculative.replayed_transactions,
        canonical_transactions: rec.speculative.canonical_transactions,
        dependency_edges: plan.dependencies.len(),
        waves,
        max_wave_width,
        runtime_profile,
        ..StrategyMetrics::default()
    })
}

fn execute_strategy(
    strategy: &str,
    engine: &CosmWasmEngine,
    block: &ProducedBlock,
    raw: &ExecutionBlock,
    predictor: &SymbolicPredictor,
    feedback: &FeedbackModel,
    reference_conflicts: &[ObservedConflict],
    direct_executor: Option<&DirectDagBlockExecutor>,
    workers: usize,
    cutoff: Duration,
    probability_threshold: f64,
    bypass_speedup: f64,
    collect_runtime_profile: bool,
) -> Result<StrategyMetrics, AnyError> {
    match strategy {
        "serial" => {
            let started=Instant::now();
            let report=SerialBlockExecutor::new(engine.clone()).execute(block,&serial_plan(block.transactions.len()))?;
            ensure_successful_report("serial strategy", &report)?;
            let wall=started.elapsed();
            Ok(StrategyMetrics { total:wall, post_consensus:wall, waves:block.transactions.len(), max_wave_width:if block.transactions.is_empty(){0}else{1}, ..StrategyMetrics::default() })
        }
        "aria-fb" => {
            let total_started=Instant::now();
            let executor=SpeculativeParallelBlockExecutor::new(engine.clone(),ParallelExecutionConfig{workers});
            let plan=fully_parallel_plan(block.transactions.len());
            let pre_started=Instant::now();
            let mut prepared=executor.prepare(block,&plan)?;
            let preexecution=pre_started.elapsed();
            let report=executor.pre_execution_report(block,&prepared)?;
            let planning_started=Instant::now();
            let conflicts=AccessConflictDetector::new(TraceConflictConfig::default()).detect(&report)?;
            let abort=aria_rule2(&conflicts);
            let abort_ids=abort.iter().filter_map(|&i|block.transactions.get(i).map(|t|t.transaction_id())).collect::<BTreeSet<_>>();
            prepared.receipts.retain(|r|!abort_ids.contains(&r.transaction_id));
            let planning=planning_started.elapsed();
            let prepared_receipts=prepared.receipts.len() as u64;
            let rec_started=Instant::now();
            let rec=executor.validate_prepared(block,prepared)?;
            let reconciliation=rec_started.elapsed();
            ensure_successful_report("aria-fb reconciliation", &rec.block)?;
            let wall=total_started.elapsed();
            Ok(StrategyMetrics { total:wall, planning, preexecution, reconciliation, post_consensus:wall, prepared_receipts, reused_receipts:rec.speculative.reused_results, replayed_transactions:rec.speculative.replayed_transactions, canonical_transactions:rec.speculative.canonical_transactions, discovered_conflicts:conflicts.len() as u64, waves:1, max_wave_width:block.transactions.len(), ..StrategyMetrics::default() })
        }
        "vegeta" => {
            let total_started=Instant::now();
            let executor=SpeculativeParallelBlockExecutor::new(engine.clone(),ParallelExecutionConfig{workers});
            let discovery_plan=fully_parallel_plan(block.transactions.len());
            let discovery_started=Instant::now();
            let discovery=executor.prepare_with_cutoff(block,&discovery_plan,cutoff)?;
            let discovery_wall=discovery_started.elapsed();
            let report=executor.pre_execution_report(block,&discovery)?;
            let planning_started=Instant::now();
            let conflicts=AccessConflictDetector::new(TraceConflictConfig::default()).detect(&report)?;
            let replay_plan=plan_from_edges(block.transactions.len(),&conflict_edges(&conflicts));
            let discovery_fp=discovery.receipts.iter().map(|r|(r.transaction_id,footprint(r))).collect::<BTreeMap<_,_>>();
            let planning=planning_started.elapsed();
            let eligible=discovery_wall.saturating_add(planning);
            let pre_consensus=eligible.min(cutoff);
            let overrun=eligible.saturating_sub(cutoff);
            let replay_started=Instant::now();
            let mut replay=executor.prepare(block,&replay_plan)?;
            replay.receipts.retain(|r| discovery_fp.get(&r.transaction_id).is_some_and(|fp|fp==&footprint(r)));
            let replay_wall=replay_started.elapsed();
            let prepared_receipts=replay.receipts.len() as u64;
            let rec_started=Instant::now();
            let rec=executor.validate_prepared(block,replay)?;
            let reconciliation=rec_started.elapsed();
            ensure_successful_report("vegeta reconciliation", &rec.block)?;
            let (waves,max_wave_width)=plan_shape(&replay_plan);
            Ok(StrategyMetrics { total:total_started.elapsed(), planning, preexecution:discovery_wall.saturating_add(replay_wall), reconciliation, post_consensus:overrun.saturating_add(replay_wall).saturating_add(reconciliation), cutoff_overrun:overrun, pre_consensus, prepared_receipts, reused_receipts:rec.speculative.reused_results, replayed_transactions:rec.speculative.replayed_transactions, canonical_transactions:rec.speculative.canonical_transactions, discovered_conflicts:conflicts.len() as u64, dependency_edges:replay_plan.dependencies.len(), waves, max_wave_width, ..StrategyMetrics::default() })
        }
        "exact-access" => {
            let planning_started=Instant::now();
            let plan=plan_from_edges(block.transactions.len(),&conflict_edges(reference_conflicts));
            let planning=planning_started.elapsed();
            let mut m=speculative_run(engine,block,&plan,workers,cutoff,planning,collect_runtime_profile)?;
            m.discovered_conflicts=reference_conflicts.len() as u64;
            Ok(m)
        }
        "exact-direct" => {
            let planning_started=Instant::now();
            let plan=plan_from_edges(block.transactions.len(),&conflict_edges(reference_conflicts));
            let planning=planning_started.elapsed();
            let total_started=Instant::now();
            let executor=direct_executor.ok_or_else(|| invalid("exact-direct strategy missing persistent direct executor"))?;
            let (report, runtime_profile)=if collect_runtime_profile {
                let (report, diagnostics)=executor.execute_with_diagnostics(block,&plan)?;
                (report, Some(runtime_profile_from_direct(&diagnostics)))
            } else {
                (executor.execute(block,&plan)?, None)
            };
            ensure_successful_report("exact direct DAG replay", &report)?;
            let replay=total_started.elapsed();
            let (waves,max_wave_width)=plan_shape(&plan);
            Ok(StrategyMetrics {
                total:planning.saturating_add(replay),
                planning,
                reconciliation:replay,
                post_consensus:planning.saturating_add(replay),
                dependency_edges:plan.dependencies.len(),
                waves,
                max_wave_width,
                discovered_conflicts:reference_conflicts.len() as u64,
                runtime_profile,
                ..StrategyMetrics::default()
            })
        }
        "static" | "probability-only" | "cost-aware" => {
            let planning_started=Instant::now();
            let static_set=static_edges(predictor,raw);
            let mut edges=match strategy {
                "static"=>static_set.clone(),
                "probability-only"=>feedback.probability_edges(raw,&static_set,probability_threshold),
                "cost-aware"=>feedback.cost_edges(raw,&static_set,workers),
                _=>unreachable!(),
            };
            let mut plan=plan_from_edges(block.transactions.len(),&edges);
            let mut serial_bypassed=false;
            let mut projected=None;
            if strategy=="cost-aware" {
                let speedup=estimate_projected_speedup(feedback,raw,&plan,workers);
                projected=Some(speedup);
                if speedup < bypass_speedup {
                    edges.clear();
                    plan=serial_plan(block.transactions.len());
                    serial_bypassed=true;
                }
            }
            let planning=planning_started.elapsed();
            if serial_bypassed {
                let started=Instant::now();
                let report=SerialBlockExecutor::new(engine.clone()).execute(block,&plan)?;
                let wall=started.elapsed();
                ensure_successful_report("cost-aware serial bypass", &report)?;
                let pre_consensus=planning.min(cutoff);
                let cutoff_overrun=planning.saturating_sub(cutoff);
                return Ok(StrategyMetrics { total:planning.saturating_add(wall), planning, reconciliation:wall, post_consensus:cutoff_overrun.saturating_add(wall), cutoff_overrun, pre_consensus, dependency_edges:0, waves:plan.waves.len(), max_wave_width:1, serial_bypassed, projected_speedup:projected, ..StrategyMetrics::default() });
            }
            let mut m=speculative_run(engine,block,&plan,workers,cutoff,planning,collect_runtime_profile)?;
            m.serial_bypassed=serial_bypassed; m.projected_speedup=projected;
            Ok(m)
        }
        other=>Err(invalid(format!("unknown strategy {other}"))),
    }
}

fn splitmix64(mut value: u64) -> u64 {
    value = value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    value ^ (value >> 31)
}

fn strategy_order<'a>(strategies: &'a [String], sample: usize, seed: u64) -> Vec<&'a str> {
    let mut rows = strategies.iter().map(String::as_str).collect::<Vec<_>>();
    let mut state = seed ^ (sample as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
    for index in (1..rows.len()).rev() {
        state = splitmix64(state);
        rows.swap(index, (state as usize) % (index + 1));
    }
    rows
}

fn strategy_provenance(strategy: &str) -> (&'static str, &'static str) {
    match strategy {
        "serial" => ("none", "post-consensus serial"),
        "aria-fb" => ("current-block concrete speculative accesses", "post-consensus discovery+fallback"),
        "vegeta" => ("current-block concrete discovery accesses", "pre-consensus discovery; post-consensus dependency replay"),
        "exact-access" => ("evaluation-only matched-serial concrete-access oracle", "pre-consensus oracle plan+execution; post-consensus validation"),
        "exact-direct" => ("evaluation-only matched-serial concrete-access oracle", "post-consensus direct canonical DAG replay; no snapshot/MVCC/receipt validation"),
        "static" => ("checked-in source-derived symbolic profiles + public native call inputs", "pre-consensus symbolic plan+execution; post-consensus validation"),
        "probability-only" => ("source-derived symbolic prior + strictly prior-block conflict feedback", "pre-consensus adaptive plan+execution; post-consensus validation"),
        "cost-aware" => ("source-derived symbolic prior + strictly prior-block conflict/cost feedback", "pre-consensus adaptive plan+execution or serial bypass"),
        _ => ("unknown", "unknown"),
    }
}

fn main() -> Result<(), AnyError> {
    let args=parse_args()?;
    let manifest:Manifest=serde_json::from_slice(&fs::read(&args.manifest)?)?;
    let raw_blocks=read_execution_blocks(&args.execution_plan)?;
    let predictor=SymbolicPredictor::load(&args.repo_root,&args.symbolic_dir)?;
    let calibration = if args.compute_metric == ComputeMetric::None || args.compute_scale == 0.0 {
        ComputeCalibration::load(Path::new("."), args.compute_metric, args.compute_scale, args.compute_base_total_nanos, args.compute_iterations_per_nano)?
    } else {
        let weights = args.compute_weights.as_ref().ok_or_else(|| invalid("--compute-weights is required when compute calibration is enabled"))?;
        ComputeCalibration::load(weights, args.compute_metric, args.compute_scale, args.compute_base_total_nanos, args.compute_iterations_per_nano)?
    };
    let calibration_meta = calibration.metadata();
    eprintln!("native-s3 benchmark compute calibration metric={} scale={} base_total_ms={:.1} iter_per_ns={:.6}", calibration_meta.metric, calibration_meta.scale, calibration_meta.base_total_nanos as f64/1e6, calibration_meta.iterations_per_nano);
    if let Some(parent)=args.output.parent(){fs::create_dir_all(parent)?;}
    let mut writer=BufWriter::new(fs::File::create(&args.output)?);

    for sample in 0..args.samples {
        for strategy in strategy_order(&args.strategies, sample, args.order_seed) {
            eprintln!("native-s3 benchmark sample={} strategy={} setup",sample,strategy);
            let (reference_engine, reference_blocks)=setup_engine(&args.repo_root,&manifest,&raw_blocks,&calibration)?;
            let (strategy_engine, strategy_blocks)=setup_engine(&args.repo_root,&manifest,&raw_blocks,&calibration)?;
            let direct_executor = if strategy == "exact-direct" {
                Some(DirectDagBlockExecutor::new(strategy_engine.clone(), args.workers)?)
            } else {
                None
            };
            let mut feedback=FeedbackModel::default();
            for (block_index, raw) in raw_blocks.iter().enumerate() {
                let reference_block=&reference_blocks[block_index];
                let strategy_block=&strategy_blocks[block_index];

                // Static/probability/cost-aware planning must not observe the current block's
                // concrete reference accesses. Their planner is invoked inside execute_strategy
                // before the reference-conflict slice is consumed. The reference execution is a
                // matched control and feedback source for *future* blocks only.
                let (reference_report, reference_wall)=execute_serial(&reference_engine,reference_block)?;
                let detector=AccessConflictDetector::new(TraceConflictConfig::default());
                let reference_conflicts=detector.detect(&reference_report)?;
                let service_nanos=reference_report.transactions.iter().map(|tx|nanos(tx.timing.service_duration)).collect::<Vec<_>>();

                let mut metrics=execute_strategy(
                    strategy,&strategy_engine,strategy_block,raw,&predictor,&feedback,&reference_conflicts,
                    direct_executor.as_ref(),args.workers,args.cutoff,args.probability_threshold,args.cost_bypass_speedup,
                    args.runtime_profile,
                )?;
                if matches!(strategy, "probability-only" | "cost-aware") {
                    let feedback_started=Instant::now();
                    feedback.update(raw,&reference_conflicts,&service_nanos);
                    metrics.feedback=feedback_started.elapsed();
                    metrics.total=metrics.total.saturating_add(metrics.feedback);
                    metrics.post_consensus=metrics.post_consensus.saturating_add(metrics.feedback);
                }
                let serial_equivalent=strategy_engine.snapshot().same_world_state(&reference_engine.snapshot());
                if !serial_equivalent {
                    return Err(invalid(format!("state mismatch sample={sample} strategy={strategy} block={}",raw.block_number)));
                }
                let serial_nanos=nanos(reference_wall);
                let total_nanos=nanos(metrics.total);
                let record=Record {
                    schema_version:1,dataset:"vegeta-s3-native-seven-strategy",sample,block_number:raw.block_number,
                    strategy:strategy.to_owned(),workers:args.workers,wasm_instance_lifecycle:"reuse",
                    compute_calibration_metric:calibration_meta.metric,compute_scale:calibration_meta.scale,
                    compute_base_total_nanos:calibration_meta.base_total_nanos,compute_iterations_per_nano:calibration_meta.iterations_per_nano,
                    compute_block_iterations:raw.transactions.iter().map(|tx|calibration.iterations_for(raw.block_number,tx.tx_index,&tx.tx_hash)).collect::<Result<Vec<_>,_>>()?.into_iter().sum(),
                    transactions:raw.transactions.len(),
                    semantic_calls:raw.transactions.iter().map(|t|t.calls.len()).sum(),
                    skipped_actions:raw.transactions.iter().map(|t|t.skipped_actions).sum(),
                    matched_serial_nanos:serial_nanos,strategy_total_nanos:total_nanos,
                    matched_serial_speedup:if total_nanos>0{serial_nanos as f64/total_nanos as f64}else{0.0},
                    planning_nanos:nanos(metrics.planning),preexecution_nanos:nanos(metrics.preexecution),
                    reconciliation_nanos:nanos(metrics.reconciliation),post_consensus_nanos:nanos(metrics.post_consensus),
                    cutoff_overrun_nanos:nanos(metrics.cutoff_overrun),prepared_receipts:metrics.prepared_receipts,
                    reused_receipts:metrics.reused_receipts,replayed_transactions:metrics.replayed_transactions,
                    canonical_transactions:metrics.canonical_transactions,discovered_conflicts:metrics.discovered_conflicts,
                    reference_conflicts:reference_conflicts.len() as u64,
                    dependency_edges:metrics.dependency_edges,waves:metrics.waves,max_wave_width:metrics.max_wave_width,
                    serial_bypassed:metrics.serial_bypassed,projected_speedup:metrics.projected_speedup,
                    serial_equivalent,feedback_scope:"strictly-prior-blocks-only",symbolic_source:"checked-in-source-derived-native-s3-profiles",
                    planning_source:strategy_provenance(strategy).0,phase_model:strategy_provenance(strategy).1,
                    consensus_cutoff_nanos:nanos(args.cutoff),pre_consensus_nanos:nanos(metrics.pre_consensus),
                    consensus_bottleneck_nanos:nanos(metrics.pre_consensus.max(metrics.post_consensus)),
                    post_consensus_speedup:if metrics.post_consensus.is_zero(){0.0}else{serial_nanos as f64/nanos(metrics.post_consensus) as f64},
                    feedback_nanos:nanos(metrics.feedback),probability_threshold:args.probability_threshold,
                    cost_bypass_speedup:args.cost_bypass_speedup,strategy_order_seed:args.order_seed,
                    runtime_profile:metrics.runtime_profile,
                    evaluation_config_id:"vegeta-s3-native-scheduler-v1",
                };
                serde_json::to_writer(&mut writer,&record)?; writer.write_all(b"\n")?; writer.flush()?;
                eprintln!("native-s3 benchmark sample={} strategy={} block={} speedup={:.3} replay={} post_ms={:.3}",sample,strategy,raw.block_number,record.matched_serial_speedup,record.replayed_transactions,record.post_consensus_nanos as f64/1e6);
            }
        }
    }
    Ok(())
}


#[cfg(test)]
mod tests {
    use super::*;

    fn tx(family: &str, action: &str) -> ExecutionTx {
        ExecutionTx {
            tx_index: 0,
            tx_hash: "0xfixture".to_owned(),
            source_failed: false,
            calls: vec![CallSpec {
                kind: "execute".to_owned(),
                family: Some(family.to_owned()),
                instance_id: Some(format!("{family}:instance")),
                sender: Some("alice".to_owned()),
                msg: Some(serde_json::json!({action: {"recipient": "bob"}})),
                funds: vec![],
                from: None,
                to: None,
                coins: vec![],
                source_revert_scope_action_id: None,
            }],
            skipped_actions: 0,
        }
    }

    #[test]
    fn dependency_plan_respects_canonical_levels() {
        let edges = BTreeSet::from([(0, 2), (1, 2), (2, 3)]);
        let plan = plan_from_edges(4, &edges);
        assert!(plan.validate().is_ok());
        assert_eq!(plan.dependencies.len(), 3);
        assert_eq!(plan.waves[0].transaction_indices, vec![0, 1]);
        assert_eq!(plan.waves[1].transaction_indices, vec![2]);
        assert_eq!(plan.waves[2].transaction_indices, vec![3]);
    }

    #[test]
    fn wildcard_symbolic_key_conflicts_with_specific_write() {
        let left = vec![PredictedAccess {
            location: PredictedLocation {
                scope: "token".to_owned(),
                resource: "balances".to_owned(),
                key: "*".to_owned(),
            },
            write: false,
        }];
        let right = vec![PredictedAccess {
            location: PredictedLocation {
                scope: "token".to_owned(),
                resource: "balances".to_owned(),
                key: "alice".to_owned(),
            },
            write: true,
        }];
        assert!(predicted_conflict(&left, &right));
    }

    #[test]
    fn feedback_pair_probability_is_order_independent_and_prior_only() {
        let mut model = FeedbackModel::default();
        assert_eq!(model.probability("a", "b", false), 0.0);
        assert_eq!(model.probability("a", "b", true), 0.75);
        model.pairs.insert(
            signature_pair_key("b", "a"),
            PairStats {
                observations: 4,
                conflicts: 1,
            },
        );
        assert_eq!(model.probability("a", "b", false), 0.25);
        assert_eq!(model.probability("b", "a", false), 0.25);
    }

    #[test]
    fn static_edge_builder_uses_source_derived_profile_keys() {
        let mut predictor = SymbolicPredictor::default();
        predictor.profiles.insert(
            ("fixture".to_owned(), "executetransfer".to_owned()),
            vec![SymbolicAccess {
                kind: "write".to_owned(),
                resource: "balances".to_owned(),
                key: SymbolicKey {
                    semantic_name: Some("address".to_owned()),
                    depends_on: Some(SymbolicDependency {
                        origin_input: Some("recipient".to_owned()),
                    }),
                },
            }],
        );
        let mut a = tx("fixture", "transfer");
        let mut b = tx("fixture", "transfer");
        a.tx_index = 0;
        b.tx_index = 1;
        let block = ExecutionBlock {
            block_number: FIRST_BLOCK,
            timestamp: 0,
            transactions: vec![a, b],
        };
        assert_eq!(static_edges(&predictor, &block), BTreeSet::from([(0, 1)]));
    }

    #[test]
    fn exact_access_is_explicitly_labeled_as_oracle() {
        let (source, phase) = strategy_provenance("exact-access");
        assert!(source.contains("evaluation-only"));
        assert!(source.contains("oracle"));
        assert!(phase.contains("oracle"));
    }

    #[test]
    fn unknown_keyed_resource_without_input_becomes_wildcard() {
        let mut fixture = tx("fixture", "transfer");
        let call = fixture.calls.remove(0);
        let key = SymbolicKey { semantic_name: Some("address".to_owned()), depends_on: None };
        assert_eq!(resolve_symbolic_key(&call, &key), Some("*".to_owned()));
        let singleton = SymbolicKey { semantic_name: Some("singleton".to_owned()), depends_on: None };
        assert_eq!(resolve_symbolic_key(&call, &singleton), Some("singleton".to_owned()));
    }

    #[test]
    fn strategy_order_is_seeded_permutation() {
        let strategies = DEFAULT_STRATEGIES.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
        let first = strategy_order(&strategies, 0, 2026082501);
        let second = strategy_order(&strategies, 1, 2026082501);
        let first_set = first.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(first_set, DEFAULT_STRATEGIES.iter().copied().collect::<BTreeSet<_>>());
        assert_ne!(first, second);
    }
}
