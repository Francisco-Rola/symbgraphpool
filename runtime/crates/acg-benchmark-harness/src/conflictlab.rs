use std::{
    collections::BTreeMap,
    env, fs,
    hint::black_box,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use acg_core::{ContractCodeHash, RuntimeId};
use acg_cosmwasm_engine::{
    Address, BlockContext, CosmWasmEngine, EngineConfig, ExecutionRequest, NativeCallContext,
    NativeContract, TransactionId, WasmInstanceLifecycle,
};
use acg_evaluation::RunIdentity;
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use acg_validator_sim::{
    BlockProducer, BlockProducerConfig, IngressConfig, Mempool, ProducedBlock,
    RateControlledIngress, DEFAULT_BENCHMARK_INGRESS_TPS,
};
use cosmwasm_std::{to_json_binary, Binary, Empty, Env, MessageInfo, Reply, Response, Uint128};
use serde::{Deserialize, Serialize};

use crate::{parameter, BenchmarkWorkload, HarnessError, PreparedBenchmark};

const CONFLICTLAB_SYMBOLIC: &[u8] =
    include_bytes!("../../../../benchmarks/symbolic/conflictlab.symbolic.json");
const BASIS_POINTS: u16 = 10_000;
const OPAQUE_ACCOUNT_PREFIX: &[u8] = b"ACGOPAQUE\0";
const DEFAULT_WASM_RELATIVE_PATH: &str =
    "benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_conflictlab.wasm";

/// ConflictLab is the controlled tuning workload for the common benchmark harness.
///
/// It can execute a fast native contract for harness tests or the real release-mode CosmWasm
/// artifact for performance evaluation. Transaction complexity is varied without changing the
/// logical conflict relation: every transaction credits one account while optional compute,
/// repeated storage rounds, and payload bytes add service cost.
#[derive(Clone, Copy, Debug, Default)]
pub struct ConflictLabWorkload;

impl BenchmarkWorkload for ConflictLabWorkload {
    fn name(&self) -> &'static str {
        "conflictlab"
    }

    fn prepare(&self, run: &RunIdentity) -> Result<Box<dyn PreparedBenchmark>, HarnessError> {
        let config = ConflictLabConfig::from_run(run)?;
        let (engine, code_id, mut environment) =
            setup_engine(config.execution_backend, config.vm_instance_lifecycle)?;
        environment.insert(
            "conflictlab_prediction_quality".to_owned(),
            config.prediction_quality.as_str().to_owned(),
        );
        environment.insert(
            "conflictlab_prediction_buckets".to_owned(),
            config.prediction_buckets.to_string(),
        );
        environment.insert(
            "conflictlab_vm_instance_lifecycle".to_owned(),
            vm_instance_lifecycle_name(config.vm_instance_lifecycle).to_owned(),
        );
        environment.insert(
            "conflictlab_complexity_mix".to_owned(),
            config.complexity_mix.as_str().to_owned(),
        );
        let checksum = engine
            .code_metadata(code_id)
            .ok_or_else(|| {
                HarnessError::Runtime("registered ConflictLab code metadata missing".to_owned())
            })?
            .checksum;
        let instantiate_msg = match config.execution_backend {
            ExecutionBackend::Native => Binary::default(),
            ExecutionBackend::Wasm => to_json_binary(&ConflictLabInstantiateMsg {
                admin: None,
                fee_bps: 0,
                epoch: 0,
            })
            .map_err(|error| HarnessError::Runtime(error.to_string()))?,
        };
        let contract = engine
            .instantiate(
                TransactionId(900),
                BlockContext::default(),
                Address::new("creator"),
                code_id,
                None,
                "conflictlab-harness".to_owned(),
                Vec::new(),
                instantiate_msg,
            )
            .map_err(|error| HarnessError::Runtime(error.to_string()))?
            .contract;

        let context = IngestionContext::new(
            RuntimeId::new("cosmwasm").map_err(|error| HarnessError::Runtime(error.to_string()))?,
            ContractCodeHash(*checksum.as_bytes()),
            1,
        );
        let symbolic = conflictlab_symbolic(config.prediction_quality)?;
        let profiles = normalize_document(
            parse_slice(&symbolic).map_err(|error| HarnessError::Runtime(error.to_string()))?,
            &context,
        )
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
        let artifact = ProfileGraphArtifact::compile(profiles, &EdgeBuildConfig::default())
            .map_err(|error| HarnessError::Runtime(error.to_string()))?;
        let graph = ProfileGraph::load(artifact, GraphLoadConfig::default())
            .map_err(|error| HarnessError::Runtime(error.to_string()))?;

        let mut generator =
            ConflictLabGenerator::new(run.seed, config.accounts, config.warmup_hot_bps);
        let mut blocks = Vec::with_capacity(config.warmup_blocks.saturating_add(1));
        let mut next_transaction_id = 1_000_u64;
        for block_offset in 0..config.warmup_blocks {
            let height = u64::try_from(block_offset)
                .map_err(|_| HarnessError::NumericOverflow)?
                .saturating_add(1);
            let block = generate_block(
                &contract,
                &mut generator,
                GenerateBlockConfig {
                    offered_transactions: config.transactions,
                    first_transaction_id: next_transaction_id,
                    height,
                    selection_seed: run.seed ^ height,
                    prediction_quality: config.prediction_quality,
                    prediction_buckets: config.prediction_buckets,
                    work: WorkShape {
                        work_iterations: config.warmup_work_iterations,
                        storage_rounds: config.warmup_storage_rounds,
                        payload_bytes: config.warmup_payload_bytes,
                    },
                    complexity_mix: config.complexity_mix,
                    simulation: config.simulation,
                },
            )?;
            next_transaction_id = next_transaction_id.saturating_add(
                u64::try_from(config.transactions).map_err(|_| HarnessError::NumericOverflow)?,
            );
            blocks.push(block);
        }

        generator.hot_bps = config.hot_bps;
        let measured_height = u64::try_from(config.warmup_blocks)
            .map_err(|_| HarnessError::NumericOverflow)?
            .saturating_add(1);
        blocks.push(generate_block(
            &contract,
            &mut generator,
            GenerateBlockConfig {
                offered_transactions: config.transactions,
                first_transaction_id: next_transaction_id,
                height: measured_height,
                selection_seed: run.seed ^ measured_height,
                prediction_quality: config.prediction_quality,
                prediction_buckets: config.prediction_buckets,
                work: WorkShape {
                    work_iterations: config.work_iterations,
                    storage_rounds: config.storage_rounds,
                    payload_bytes: config.payload_bytes,
                },
                complexity_mix: config.complexity_mix,
                simulation: config.simulation,
            },
        )?);
        let measured_block = blocks.pop().ok_or_else(|| {
            HarnessError::Runtime("ConflictLab produced no measured block".to_owned())
        })?;

        Ok(Box::new(PreparedConflictLab {
            engine,
            graph,
            contract,
            accounts: config.accounts,
            environment,
            warmup_blocks: blocks,
            measured_block,
        }))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PredictionQuality {
    Exact,
    Bucketed,
    Coarse,
    Opaque,
}

impl PredictionQuality {
    fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Bucketed => "bucketed",
            Self::Coarse => "coarse",
            Self::Opaque => "opaque",
        }
    }

    fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "exact" => Ok(Self::Exact),
            "bucketed" => Ok(Self::Bucketed),
            "coarse" => Ok(Self::Coarse),
            "opaque" => Ok(Self::Opaque),
            other => Err(HarnessError::WorkloadParameter(format!(
                "prediction_quality must be exact, bucketed, coarse, or opaque; got {other:?}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ComplexityMix {
    #[default]
    Homogeneous,
    Light80Medium15Heavy5,
    Balanced,
    Light10Medium30Heavy60,
}

impl ComplexityMix {
    fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "homogeneous" => Ok(Self::Homogeneous),
            "80-15-5" => Ok(Self::Light80Medium15Heavy5),
            "33-34-33" => Ok(Self::Balanced),
            "10-30-60" => Ok(Self::Light10Medium30Heavy60),
            other => Err(HarnessError::WorkloadParameter(format!(
                "complexity_mix must be homogeneous, 80-15-5, 33-34-33, or 10-30-60; got {other:?}"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Homogeneous => "homogeneous",
            Self::Light80Medium15Heavy5 => "80-15-5",
            Self::Balanced => "33-34-33",
            Self::Light10Medium30Heavy60 => "10-30-60",
        }
    }

    fn work_shape(self, homogeneous: WorkShape, selector: u64) -> WorkShape {
        let sample = selector % 100;
        match self {
            Self::Homogeneous => homogeneous,
            Self::Light80Medium15Heavy5 if sample < 80 => light_work_shape(),
            Self::Light80Medium15Heavy5 if sample < 95 => medium_work_shape(),
            Self::Light80Medium15Heavy5 => heavy_work_shape(),
            Self::Balanced if sample < 33 => light_work_shape(),
            Self::Balanced if sample < 67 => medium_work_shape(),
            Self::Balanced => heavy_work_shape(),
            Self::Light10Medium30Heavy60 if sample < 10 => light_work_shape(),
            Self::Light10Medium30Heavy60 if sample < 40 => medium_work_shape(),
            Self::Light10Medium30Heavy60 => heavy_work_shape(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ExecutionBackend {
    Native,
    Wasm,
}

impl ExecutionBackend {
    fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "native" => Ok(Self::Native),
            "wasm" => Ok(Self::Wasm),
            other => Err(HarnessError::WorkloadParameter(format!(
                "execution_backend must be native or wasm, got {other:?}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MempoolPolicy {
    Fifo,
    ReverseFifo,
    SeededShuffle,
}

impl MempoolPolicy {
    fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "fifo" => Ok(Self::Fifo),
            "reverse-fifo" => Ok(Self::ReverseFifo),
            "seeded-shuffle" => Ok(Self::SeededShuffle),
            other => Err(HarnessError::WorkloadParameter(format!(
                "sim.mempool_policy must be fifo, reverse-fifo, or seeded-shuffle; got {other:?}"
            ))),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct SimulationConfig {
    admission_tps: u64,
    block_interval_ms: u64,
    block_size: usize,
    mempool_policy: MempoolPolicy,
}

#[derive(Clone, Copy, Debug)]
struct ConflictLabConfig {
    transactions: usize,
    warmup_blocks: usize,
    accounts: u64,
    hot_bps: u16,
    work_iterations: u64,
    storage_rounds: u32,
    payload_bytes: usize,
    warmup_hot_bps: u16,
    warmup_work_iterations: u64,
    warmup_storage_rounds: u32,
    warmup_payload_bytes: usize,
    execution_backend: ExecutionBackend,
    vm_instance_lifecycle: WasmInstanceLifecycle,
    prediction_quality: PredictionQuality,
    complexity_mix: ComplexityMix,
    prediction_buckets: u16,
    simulation: SimulationConfig,
}

impl ConflictLabConfig {
    fn from_run(run: &RunIdentity) -> Result<Self, HarnessError> {
        const WORKLOAD_KEYS: &[&str] = &[
            "transactions",
            "warmup_blocks",
            "accounts",
            "hot_account_probability_bps",
            "work_iterations",
            "storage_rounds",
            "payload_bytes",
            "complexity",
            "contention",
            "warmup_hot_account_probability_bps",
            "warmup_work_iterations",
            "warmup_storage_rounds",
            "warmup_payload_bytes",
            "execution_backend",
            "vm_instance_lifecycle",
            "prediction_quality",
            "complexity_mix",
            "prediction_buckets",
            "sim.admission_tps",
            "sim.block_interval_ms",
            "sim.block_size",
            "sim.mempool_policy",
        ];
        for key in run.parameters.keys() {
            if !key.starts_with("acg.") && !WORKLOAD_KEYS.contains(&key.as_str()) {
                return Err(HarnessError::WorkloadParameter(format!(
                    "unknown ConflictLab parameter {key:?}"
                )));
            }
        }

        let transactions = parameter(&run.parameters, "transactions", 200_usize)?;
        let warmup_blocks = parameter(&run.parameters, "warmup_blocks", 0_usize)?;
        let accounts = parameter(&run.parameters, "accounts", 16_u64)?;
        let hot_bps = parameter(&run.parameters, "hot_account_probability_bps", 0_u16)?;
        let work_iterations = parameter(&run.parameters, "work_iterations", 0_u64)?;
        let storage_rounds = parameter(&run.parameters, "storage_rounds", 0_u32)?;
        let payload_bytes = parameter(&run.parameters, "payload_bytes", 0_usize)?;
        let warmup_hot_bps = parameter(
            &run.parameters,
            "warmup_hot_account_probability_bps",
            hot_bps,
        )?;
        let warmup_work_iterations =
            parameter(&run.parameters, "warmup_work_iterations", work_iterations)?;
        let warmup_storage_rounds =
            parameter(&run.parameters, "warmup_storage_rounds", storage_rounds)?;
        let warmup_payload_bytes =
            parameter(&run.parameters, "warmup_payload_bytes", payload_bytes)?;
        let execution_backend = ExecutionBackend::parse(
            run.parameters
                .get("execution_backend")
                .map(String::as_str)
                .unwrap_or("native"),
        )?;
        let vm_instance_lifecycle = parse_vm_instance_lifecycle(
            run.parameters
                .get("vm_instance_lifecycle")
                .map(String::as_str)
                .unwrap_or("reuse"),
        )?;
        let complexity_mix = ComplexityMix::parse(
            run.parameters
                .get("complexity_mix")
                .map(String::as_str)
                .unwrap_or("homogeneous"),
        )?;
        let prediction_quality = PredictionQuality::parse(
            run.parameters
                .get("prediction_quality")
                .map(String::as_str)
                .unwrap_or("exact"),
        )?;
        let prediction_buckets = parameter(&run.parameters, "prediction_buckets", 8_u16)?;
        let block_size = parameter(&run.parameters, "sim.block_size", transactions)?;
        let admission_tps = parameter(
            &run.parameters,
            "sim.admission_tps",
            DEFAULT_BENCHMARK_INGRESS_TPS,
        )?;
        let block_interval_ms = parameter(&run.parameters, "sim.block_interval_ms", 2_000_u64)?;
        let mempool_policy = MempoolPolicy::parse(
            run.parameters
                .get("sim.mempool_policy")
                .map(String::as_str)
                .unwrap_or("fifo"),
        )?;

        if transactions == 0 {
            return Err(HarnessError::WorkloadParameter(
                "transactions must be greater than zero".to_owned(),
            ));
        }
        if accounts == 0 {
            return Err(HarnessError::WorkloadParameter(
                "accounts must be greater than zero".to_owned(),
            ));
        }
        if prediction_quality == PredictionQuality::Bucketed
            && !(2..=64).contains(&prediction_buckets)
        {
            return Err(HarnessError::WorkloadParameter(
                "prediction_buckets must be within 2..=64 for bucketed prediction".to_owned(),
            ));
        }
        if block_size == 0 {
            return Err(HarnessError::WorkloadParameter(
                "sim.block_size must be greater than zero".to_owned(),
            ));
        }
        if admission_tps == 0 {
            return Err(HarnessError::WorkloadParameter(
                "sim.admission_tps must be greater than zero".to_owned(),
            ));
        }
        if block_interval_ms == 0 {
            return Err(HarnessError::WorkloadParameter(
                "sim.block_interval_ms must be greater than zero".to_owned(),
            ));
        }
        if hot_bps > BASIS_POINTS || warmup_hot_bps > BASIS_POINTS {
            return Err(HarnessError::WorkloadParameter(format!(
                "hot-account probabilities must be <= {BASIS_POINTS} basis points"
            )));
        }

        Ok(Self {
            transactions,
            warmup_blocks,
            accounts,
            hot_bps,
            work_iterations,
            storage_rounds,
            payload_bytes,
            warmup_hot_bps,
            warmup_work_iterations,
            warmup_storage_rounds,
            warmup_payload_bytes,
            execution_backend,
            vm_instance_lifecycle,
            prediction_quality,
            complexity_mix,
            prediction_buckets,
            simulation: SimulationConfig {
                admission_tps,
                block_interval_ms,
                block_size,
                mempool_policy,
            },
        })
    }
}

fn conflictlab_symbolic(prediction_quality: PredictionQuality) -> Result<Vec<u8>, HarnessError> {
    if prediction_quality != PredictionQuality::Coarse {
        return Ok(CONFLICTLAB_SYMBOLIC.to_vec());
    }

    let mut document: serde_json::Value = serde_json::from_slice(CONFLICTLAB_SYMBOLIC)
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
    let profiles = document
        .get_mut("profiles")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| HarnessError::Runtime("ConflictLab symbolic profiles missing".to_owned()))?;
    let credit = profiles
        .iter_mut()
        .find(|profile| {
            profile
                .get("entrypoint")
                .and_then(serde_json::Value::as_str)
                == Some("execute::Credit")
        })
        .ok_or_else(|| {
            HarnessError::Runtime("ConflictLab Credit symbolic profile missing".to_owned())
        })?;
    let accesses = credit
        .get_mut("accesses")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| HarnessError::Runtime("ConflictLab Credit accesses missing".to_owned()))?;
    for access in accesses {
        let key = access
            .get_mut("key")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| HarnessError::Runtime("ConflictLab Credit key missing".to_owned()))?;
        key.insert("depends_on".to_owned(), serde_json::Value::Null);
    }
    serde_json::to_vec(&document).map_err(|error| HarnessError::Runtime(error.to_string()))
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn conflictlab_wasm_path() -> PathBuf {
    env::var_os("ACG_CONFLICTLAB_WASM")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository_root().join(DEFAULT_WASM_RELATIVE_PATH))
}

fn parse_vm_instance_lifecycle(value: &str) -> Result<WasmInstanceLifecycle, HarnessError> {
    match value {
        "reuse" => Ok(WasmInstanceLifecycle::Reuse),
        "recycle" => Ok(WasmInstanceLifecycle::Recycle),
        other => Err(HarnessError::WorkloadParameter(format!(
            "vm_instance_lifecycle must be reuse or recycle, got {other:?}"
        ))),
    }
}

fn vm_instance_lifecycle_name(value: WasmInstanceLifecycle) -> &'static str {
    match value {
        WasmInstanceLifecycle::Reuse => "reuse",
        WasmInstanceLifecycle::Recycle => "recycle",
    }
}

fn setup_engine(
    backend: ExecutionBackend,
    vm_instance_lifecycle: WasmInstanceLifecycle,
) -> Result<
    (
        CosmWasmEngine,
        acg_cosmwasm_engine::CodeId,
        BTreeMap<String, String>,
    ),
    HarnessError,
> {
    let engine = CosmWasmEngine::new(EngineConfig {
        wasm_instance_lifecycle: vm_instance_lifecycle,
        ..EngineConfig::default()
    });
    let mut environment = BTreeMap::new();
    match backend {
        ExecutionBackend::Native => {
            let code_id = engine
                .register_native("conflictlab-harness", Arc::new(ConflictLabRuntime))
                .map_err(|error| HarnessError::Runtime(error.to_string()))?;
            environment.insert("conflictlab_backend".to_owned(), "native".to_owned());
            Ok((engine, code_id, environment))
        }
        ExecutionBackend::Wasm => {
            let path = conflictlab_wasm_path();
            let wasm = fs::read(&path).map_err(|error| {
                HarnessError::Runtime(format!(
                    "failed to read ConflictLab Wasm at {}: {error}; build it with \
                     `cargo build --manifest-path benchmarks/Cargo.toml -p \
                     acg-benchmark-conflictlab --release --target wasm32-unknown-unknown` \
                     or set ACG_CONFLICTLAB_WASM",
                    path.display()
                ))
            })?;
            if wasm.is_empty() {
                return Err(HarnessError::Runtime(format!(
                    "ConflictLab Wasm artifact is empty: {}",
                    path.display()
                )));
            }
            let code_id = engine
                .upload_wasm(wasm)
                .map_err(|error| HarnessError::Runtime(error.to_string()))?;
            let checksum = engine
                .code_metadata(code_id)
                .ok_or_else(|| {
                    HarnessError::Runtime("ConflictLab Wasm metadata missing".to_owned())
                })?
                .checksum;
            environment.insert("conflictlab_backend".to_owned(), "wasm".to_owned());
            environment.insert(
                "conflictlab_wasm_path".to_owned(),
                path.display().to_string(),
            );
            environment.insert("conflictlab_wasm_checksum".to_owned(), checksum.to_hex());
            Ok((engine, code_id, environment))
        }
    }
}

struct PreparedConflictLab {
    engine: CosmWasmEngine,
    graph: ProfileGraph,
    contract: Address,
    accounts: u64,
    environment: BTreeMap<String, String>,
    warmup_blocks: Vec<ProducedBlock>,
    measured_block: ProducedBlock,
}

impl PreparedBenchmark for PreparedConflictLab {
    fn engine(&self) -> &CosmWasmEngine {
        &self.engine
    }

    fn profile_graph(&self) -> &ProfileGraph {
        &self.graph
    }

    fn warmup_blocks(&self) -> &[ProducedBlock] {
        &self.warmup_blocks
    }

    fn measured_block(&self) -> &ProducedBlock {
        &self.measured_block
    }

    fn environment_metadata(&self) -> BTreeMap<String, String> {
        self.environment.clone()
    }

    fn canonical_state_bytes(&self) -> Result<Vec<u8>, HarnessError> {
        let mut bytes = Vec::new();
        append_bytes(&mut bytes, self.contract.as_str().as_bytes());
        bytes.extend_from_slice(&self.accounts.to_be_bytes());
        for account_id in 0..self.accounts {
            let account = format!("account-{account_id}");
            append_bytes(&mut bytes, account.as_bytes());
            let outcome = self
                .engine
                .query(
                    BlockContext::default(),
                    self.contract.clone(),
                    to_json_binary(&ConflictLabQueryMsg::Balance {
                        account: account.clone(),
                    })
                    .map_err(|error| HarnessError::Runtime(error.to_string()))?,
                )
                .map_err(|error| HarnessError::Runtime(error.to_string()))?;
            append_bytes(&mut bytes, outcome.data.as_slice());
        }
        Ok(bytes)
    }
}

#[derive(Serialize)]
struct ConflictLabInstantiateMsg {
    admin: Option<String>,
    fee_bps: u16,
    epoch: u64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConflictLabExecuteMsg {
    Credit {
        account: String,
        amount: Uint128,
        #[serde(default)]
        work_iterations: u64,
        #[serde(default)]
        storage_rounds: u32,
        #[serde(default)]
        payload: Binary,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConflictLabQueryMsg {
    Balance { account: String },
}

#[derive(Serialize, Deserialize)]
struct AmountResponse {
    amount: Uint128,
}

struct ConflictLabRuntime;

impl NativeContract for ConflictLabRuntime {
    fn instantiate(
        &self,
        _context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        _msg: Binary,
    ) -> Result<Response<Empty>, String> {
        Ok(Response::new())
    }

    fn execute(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        _info: MessageInfo,
        msg: Binary,
    ) -> Result<Response<Empty>, String> {
        let ConflictLabExecuteMsg::Credit {
            account,
            amount,
            work_iterations,
            storage_rounds,
            payload,
        } = serde_json::from_slice(msg.as_slice()).map_err(|error| error.to_string())?;
        let effective_account =
            opaque_account_from_payload(payload.as_slice()).unwrap_or(account.as_str());
        let key = format!("balance/{effective_account}");
        let mut current = context
            .storage_get(key.as_bytes())
            .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
            .map(u128::from_be_bytes)
            .unwrap_or_default();
        for _ in 0..storage_rounds {
            context.storage_set(key.as_bytes(), current.to_be_bytes());
            current = context
                .storage_get(key.as_bytes())
                .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
                .map(u128::from_be_bytes)
                .unwrap_or_default();
        }
        let checksum = deterministic_work(
            work_iterations,
            current as u64 ^ amount.u128() as u64,
            payload.as_slice(),
        );
        context.storage_set(
            key.as_bytes(),
            current.saturating_add(amount.u128()).to_be_bytes(),
        );
        Ok(Response::new().add_attribute("work_checksum", checksum.to_string()))
    }

    fn query(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        msg: Binary,
    ) -> Result<Binary, String> {
        let query: ConflictLabQueryMsg =
            serde_json::from_slice(msg.as_slice()).map_err(|error| error.to_string())?;
        match query {
            ConflictLabQueryMsg::Balance { account } => {
                let key = format!("balance/{account}");
                let amount = context
                    .storage_get(key.as_bytes())
                    .and_then(|bytes| <[u8; 16]>::try_from(bytes).ok())
                    .map(u128::from_be_bytes)
                    .unwrap_or_default();
                to_json_binary(&AmountResponse {
                    amount: Uint128::new(amount),
                })
                .map_err(|error| error.to_string())
            }
        }
    }

    fn reply(
        &self,
        _context: &mut NativeCallContext,
        _env: Env,
        _reply: Reply,
    ) -> Result<Response<Empty>, String> {
        Err("reply not used".to_owned())
    }
}

#[derive(Clone, Copy)]
struct WorkShape {
    work_iterations: u64,
    storage_rounds: u32,
    payload_bytes: usize,
}

fn light_work_shape() -> WorkShape {
    WorkShape {
        work_iterations: 4_096,
        storage_rounds: 1,
        payload_bytes: 64,
    }
}

fn medium_work_shape() -> WorkShape {
    WorkShape {
        work_iterations: 32_768,
        storage_rounds: 2,
        payload_bytes: 256,
    }
}

fn heavy_work_shape() -> WorkShape {
    WorkShape {
        work_iterations: 262_144,
        storage_rounds: 4,
        payload_bytes: 1_024,
    }
}

#[derive(Clone, Copy)]
struct GenerateBlockConfig {
    offered_transactions: usize,
    first_transaction_id: u64,
    height: u64,
    selection_seed: u64,
    prediction_quality: PredictionQuality,
    prediction_buckets: u16,
    work: WorkShape,
    complexity_mix: ComplexityMix,
    simulation: SimulationConfig,
}

fn generate_block(
    contract: &Address,
    generator: &mut ConflictLabGenerator,
    config: GenerateBlockConfig,
) -> Result<ProducedBlock, HarnessError> {
    let GenerateBlockConfig {
        offered_transactions,
        first_transaction_id,
        height,
        selection_seed,
        prediction_quality,
        prediction_buckets,
        work,
        complexity_mix,
        simulation,
    } = config;
    let mempool = Mempool::default();
    let mut ingress = RateControlledIngress::new(
        IngressConfig {
            transactions_per_second: simulation.admission_tps,
        },
        0,
    )
    .map_err(|error| HarnessError::Runtime(error.to_string()))?;

    for offset in 0..offered_transactions {
        let transaction_id = first_transaction_id
            .saturating_add(u64::try_from(offset).map_err(|_| HarnessError::NumericOverflow)?);
        let actual_account = generator.next_account();
        let mut selector_rng = SplitMix64::new(transaction_id ^ selection_seed);
        let transaction_work = complexity_mix.work_shape(work, selector_rng.next_u64());
        let base_payload = deterministic_payload(
            transaction_work.payload_bytes,
            transaction_id ^ selection_seed,
        );
        let (account, payload) = match prediction_quality {
            PredictionQuality::Exact | PredictionQuality::Coarse => (actual_account, base_payload),
            PredictionQuality::Bucketed => {
                let bucket = prediction_bucket(&actual_account, prediction_buckets);
                (
                    format!("bucket-{bucket}"),
                    bucketed_payload(&actual_account, base_payload),
                )
            }
            PredictionQuality::Opaque => (
                format!("prediction-{transaction_id}"),
                opaque_payload(&actual_account, base_payload),
            ),
        };
        let request = ExecutionRequest::Execute {
            transaction_id: TransactionId(transaction_id),
            sender: Address::new("client"),
            contract: contract.clone(),
            funds: Vec::new(),
            msg: to_json_binary(&ConflictLabExecuteMsg::Credit {
                account,
                amount: Uint128::new(1),
                work_iterations: transaction_work.work_iterations,
                storage_rounds: transaction_work.storage_rounds,
                payload: Binary::from(payload),
            })
            .map_err(|error| HarnessError::Runtime(error.to_string()))?,
        };
        ingress.enqueue(request);
    }

    let block_interval = Duration::from_millis(simulation.block_interval_ms);
    let interval_nanos = block_interval.as_nanos().min(u128::from(u64::MAX)) as u64;
    ingress.pump_until(interval_nanos, &mempool);
    if mempool.is_empty() {
        return Err(HarnessError::WorkloadParameter(format!(
            "sim.admission_tps={} and sim.block_interval_ms={} admit no transactions in one \
             block window",
            simulation.admission_tps, simulation.block_interval_ms
        )));
    }
    let producer_config = BlockProducerConfig {
        block_interval,
        first_block_height: height,
        first_block_time_nanos: interval_nanos,
        max_transactions_per_block: Some(simulation.block_size),
        ..BlockProducerConfig::default()
    };
    let block = match simulation.mempool_policy {
        MempoolPolicy::Fifo => BlockProducer::fifo(producer_config)
            .map_err(|error| HarnessError::Runtime(error.to_string()))?
            .produce_next(&mempool),
        MempoolPolicy::ReverseFifo => BlockProducer::reverse_fifo(producer_config)
            .map_err(|error| HarnessError::Runtime(error.to_string()))?
            .produce_next(&mempool),
        MempoolPolicy::SeededShuffle => {
            { BlockProducer::seeded_shuffle(producer_config, selection_seed) }
                .map_err(|error| HarnessError::Runtime(error.to_string()))?
                .produce_next(&mempool)
        }
    };
    if block.transactions.is_empty() {
        return Err(HarnessError::Runtime(
            "ConflictLab produced an empty block after admission".to_owned(),
        ));
    }
    Ok(block)
}

struct ConflictLabGenerator {
    rng: SplitMix64,
    accounts: u64,
    hot_bps: u16,
}

impl ConflictLabGenerator {
    fn new(seed: u64, accounts: u64, hot_bps: u16) -> Self {
        Self {
            rng: SplitMix64::new(seed),
            accounts,
            hot_bps,
        }
    }

    fn next_account(&mut self) -> String {
        let choose_hot = self.accounts == 1
            || self.rng.next_u64() % u64::from(BASIS_POINTS) < u64::from(self.hot_bps);
        let account_id = if choose_hot {
            0
        } else {
            self.rng.next_u64() % self.accounts
        };
        format!("account-{account_id}")
    }
}

struct SplitMix64 {
    state: u64,
}

impl SplitMix64 {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut value = self.state;
        value = (value ^ (value >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        value = (value ^ (value >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        value ^ (value >> 31)
    }
}

fn prediction_bucket(account: &str, buckets: u16) -> u16 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in account.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    (hash % u64::from(buckets.max(1))) as u16
}

fn bucketed_payload(account: &str, payload: Vec<u8>) -> Vec<u8> {
    let header_len = OPAQUE_ACCOUNT_PREFIX.len() + account.len() + 1;
    if payload.len() < header_len {
        return opaque_payload(account, payload);
    }
    let payload_budget = payload.len() - header_len;
    let mut encoded = Vec::with_capacity(payload.len());
    encoded.extend_from_slice(OPAQUE_ACCOUNT_PREFIX);
    encoded.extend_from_slice(account.as_bytes());
    encoded.push(0);
    encoded.extend_from_slice(&payload[..payload_budget]);
    encoded
}

fn opaque_payload(account: &str, payload: Vec<u8>) -> Vec<u8> {
    let mut encoded =
        Vec::with_capacity(OPAQUE_ACCOUNT_PREFIX.len() + account.len() + 1 + payload.len());
    encoded.extend_from_slice(OPAQUE_ACCOUNT_PREFIX);
    encoded.extend_from_slice(account.as_bytes());
    encoded.push(0);
    encoded.extend_from_slice(&payload);
    encoded
}

fn opaque_account_from_payload(payload: &[u8]) -> Option<&str> {
    let encoded = payload.strip_prefix(OPAQUE_ACCOUNT_PREFIX)?;
    let terminator = encoded.iter().position(|byte| *byte == 0)?;
    std::str::from_utf8(&encoded[..terminator]).ok()
}

fn deterministic_payload(bytes: usize, seed: u64) -> Vec<u8> {
    let mut rng = SplitMix64::new(seed);
    (0..bytes).map(|_| rng.next_u64() as u8).collect()
}

fn deterministic_work(iterations: u64, seed: u64, payload: &[u8]) -> u64 {
    let mut value = seed ^ 0xD6E8_FEB8_6659_FD93;
    for (index, byte) in payload.iter().copied().enumerate() {
        value = value
            .wrapping_add(u64::from(byte).wrapping_mul((index as u64).wrapping_add(1)))
            .rotate_left((index & 31) as u32);
    }
    for index in 0..iterations {
        value = value
            .wrapping_add(index.rotate_left((index & 31) as u32))
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ value.rotate_right(11);
    }
    black_box(value)
}

fn append_bytes(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    output.extend_from_slice(value);
}
