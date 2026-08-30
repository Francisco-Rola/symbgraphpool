use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use acg_core::{ContractCodeHash, RuntimeId};
use acg_cosmwasm_engine::{
    Address, BlockContext, CosmWasmEngine, EngineConfig, ExecutionRequest, NativeCallContext,
    NativeContract, TransactionId, WasmInstanceLifecycle,
};
use acg_evaluation::RunIdentity;
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use acg_validator_sim::{PendingTransaction, ProducedBlock};
use cosmwasm_std::{to_json_binary, Binary, Empty, Env, MessageInfo, Reply, Response};
use serde::{Deserialize, Serialize};

use crate::{parameter, BenchmarkWorkload, HarnessError, PreparedBenchmark};

const VEGETA_TRACE_SYMBOLIC: &[u8] =
    include_bytes!("../../../../benchmarks/symbolic/vegeta-trace.symbolic.json");
const DEFAULT_CORPUS_RELATIVE_PATH: &str = "benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl";
const DEFAULT_WASM_RELATIVE_PATH: &str =
    "benchmarks/target/wasm32-unknown-unknown/release/acg_benchmark_vegeta_trace.wasm";
const S3_START_BLOCK: u64 = 16_774_645;
const S3_END_BLOCK: u64 = 16_774_745;
const S3_EXPECTED_TRANSACTIONS: usize = 15_129;
const S3_EXPECTED_LONGEST_CHAIN_SUM: u64 = 1_779;
const WETH_MAINNET: &str = "c02aaa39b223fe8d0a0e5c4f27ead9083c756cc2";

#[derive(Clone, Copy, Debug, Default)]
pub struct VegetaEthWorkload;

impl BenchmarkWorkload for VegetaEthWorkload {
    fn name(&self) -> &'static str {
        "vegeta-eth"
    }

    fn prepare(&self, run: &RunIdentity) -> Result<Box<dyn PreparedBenchmark>, HarnessError> {
        let config = VegetaEthConfig::from_run(run)?;
        let corpus = VegetaCorpus::load(&config.corpus_path)?;
        let measured_index = corpus
            .blocks
            .iter()
            .position(|block| block.block_number == config.measured_block)
            .ok_or_else(|| {
                HarnessError::WorkloadParameter(format!(
                    "vegeta measured block {} is not present in {}",
                    config.measured_block,
                    config.corpus_path.display()
                ))
            })?;
        if measured_index < config.warmup_blocks {
            return Err(HarnessError::WorkloadParameter(format!(
                "vegeta measured block {} has only {} preceding corpus blocks but warmup_blocks={}",
                config.measured_block, measured_index, config.warmup_blocks
            )));
        }

        let (engine, code_id, mut environment) = setup_engine(&config)?;
        let checksum = engine
            .code_metadata(code_id)
            .ok_or_else(|| HarnessError::Runtime("Vegeta trace code metadata missing".to_owned()))?
            .checksum;
        let contract = engine
            .instantiate(
                TransactionId(980),
                BlockContext::default(),
                Address::new("creator"),
                code_id,
                None,
                "vegeta-ethereum-trace".to_owned(),
                Vec::new(),
                to_json_binary(&TraceInstantiateMsg {})
                    .map_err(|error| HarnessError::Runtime(error.to_string()))?,
            )
            .map_err(|error| HarnessError::Runtime(error.to_string()))?
            .contract;

        let context = IngestionContext::new(
            RuntimeId::new("cosmwasm").map_err(|error| HarnessError::Runtime(error.to_string()))?,
            ContractCodeHash(*checksum.as_bytes()),
            1,
        );
        let profiles = normalize_document(
            parse_slice(VEGETA_TRACE_SYMBOLIC)
                .map_err(|error| HarnessError::Runtime(error.to_string()))?,
            &context,
        )
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
        let artifact = ProfileGraphArtifact::compile(profiles, &EdgeBuildConfig::default())
            .map_err(|error| HarnessError::Runtime(error.to_string()))?;
        let graph = ProfileGraph::load(artifact, GraphLoadConfig::default())
            .map_err(|error| HarnessError::Runtime(error.to_string()))?;

        let first_warmup = measured_index - config.warmup_blocks;
        let mut warmup_blocks = Vec::with_capacity(config.warmup_blocks);
        for index in first_warmup..measured_index {
            warmup_blocks.push(build_block(
                &corpus,
                index,
                &contract,
                config.prediction,
                config.work,
            )?);
        }
        let measured_block = build_block(
            &corpus,
            measured_index,
            &contract,
            config.prediction,
            config.work,
        )?;

        let canonical_keys = corpus.blocks[first_warmup..=measured_index]
            .iter()
            .flat_map(|block| block.transactions.iter())
            .flat_map(|tx| tx.reads.iter().chain(tx.writes.iter()))
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();

        environment.insert(
            "vegeta_dataset".to_owned(),
            corpus.dataset_label().to_owned(),
        );
        environment.insert(
            "vegeta_corpus_path".to_owned(),
            config.corpus_path.display().to_string(),
        );
        environment.insert(
            "vegeta_measured_ethereum_block".to_owned(),
            config.measured_block.to_string(),
        );
        environment.insert(
            "vegeta_warmup_blocks".to_owned(),
            config.warmup_blocks.to_string(),
        );
        environment.insert(
            "vegeta_prediction_history_blocks".to_owned(),
            config.prediction.history_blocks.to_string(),
        );
        environment.insert(
            "vegeta_prediction_min_frequency_bps".to_owned(),
            config.prediction.min_frequency_bps.to_string(),
        );
        environment.insert(
            "vegeta_prediction_max_keys_per_method".to_owned(),
            config.prediction.max_keys_per_method.to_string(),
        );
        environment.insert(
            "vegeta_access_semantics".to_owned(),
            "evm-storage-sload-sstore-v1".to_owned(),
        );
        environment.insert(
            "vegeta_vm_instance_lifecycle".to_owned(),
            vm_instance_lifecycle_name(config.vm_instance_lifecycle).to_owned(),
        );
        environment.insert(
            "vegeta_vm_gas_limit".to_owned(),
            config.vm_gas_limit.to_string(),
        );
        environment.insert(
            "vegeta_prediction_visibility".to_owned(),
            "profile-references-predicted-arrays-only".to_owned(),
        );
        environment.insert(
            "vegeta_paper_s3_range".to_owned(),
            format!("{S3_START_BLOCK}-{S3_END_BLOCK}"),
        );
        environment.insert(
            "vegeta_paper_s3_transactions".to_owned(),
            S3_EXPECTED_TRANSACTIONS.to_string(),
        );
        environment.insert(
            "vegeta_paper_s3_longest_chain_sum".to_owned(),
            S3_EXPECTED_LONGEST_CHAIN_SUM.to_string(),
        );
        environment.insert(
            "vegeta_paper_weth_hotspot".to_owned(),
            WETH_MAINNET.to_owned(),
        );

        Ok(Box::new(PreparedVegetaEth {
            engine,
            graph,
            contract,
            canonical_keys,
            environment,
            warmup_blocks,
            measured_block,
        }))
    }
}

#[derive(Clone, Debug)]
struct VegetaEthConfig {
    corpus_path: PathBuf,
    measured_block: u64,
    warmup_blocks: usize,
    prediction: PredictionConfig,
    work: WorkConfig,
    backend: ExecutionBackend,
    vm_instance_lifecycle: WasmInstanceLifecycle,
    vm_gas_limit: u64,
}

impl VegetaEthConfig {
    fn from_run(run: &RunIdentity) -> Result<Self, HarnessError> {
        let corpus_path = run
            .parameters
            .get("vegeta.corpus_path")
            .map(PathBuf::from)
            .unwrap_or_else(default_corpus_path);
        let measured_block = parameter(&run.parameters, "vegeta.measured_block", S3_END_BLOCK)?;
        let warmup_blocks = parameter(&run.parameters, "warmup_blocks", 4_usize)?;
        let history_blocks = parameter(
            &run.parameters,
            "vegeta.prediction_history_blocks",
            20_usize,
        )?;
        let min_frequency_bps = parameter(
            &run.parameters,
            "vegeta.prediction_min_frequency_bps",
            2_000_u16,
        )?;
        if min_frequency_bps > 10_000 {
            return Err(HarnessError::WorkloadParameter(format!(
                "vegeta.prediction_min_frequency_bps must be <= 10000, got {min_frequency_bps}"
            )));
        }
        let max_keys_per_method = parameter(
            &run.parameters,
            "vegeta.prediction_max_keys_per_method",
            32_usize,
        )?;
        let work_step_divisor = parameter(&run.parameters, "vegeta.work_step_divisor", 64_u64)?;
        let work_max_iterations =
            parameter(&run.parameters, "vegeta.work_max_iterations", 20_000_u64)?;
        let backend = ExecutionBackend::parse(
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
        let vm_gas_limit = parameter(&run.parameters, "vm_gas_limit", u64::MAX)?;
        Ok(Self {
            corpus_path,
            measured_block,
            warmup_blocks,
            prediction: PredictionConfig {
                history_blocks,
                min_frequency_bps,
                max_keys_per_method,
            },
            work: WorkConfig {
                step_divisor: work_step_divisor,
                max_iterations: work_max_iterations,
            },
            backend,
            vm_instance_lifecycle,
            vm_gas_limit,
        })
    }
}

#[derive(Clone, Copy, Debug)]
struct PredictionConfig {
    history_blocks: usize,
    min_frequency_bps: u16,
    max_keys_per_method: usize,
}

#[derive(Clone, Copy, Debug)]
struct WorkConfig {
    step_divisor: u64,
    max_iterations: u64,
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
                "execution_backend must be native or wasm for vegeta-eth, got {other:?}"
            ))),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
struct CorpusBlock {
    block_number: u64,
    #[serde(default)]
    timestamp: u64,
    transactions: Vec<CorpusTransaction>,
}

#[derive(Clone, Debug, Deserialize)]
struct CorpusTransaction {
    tx_index: usize,
    tx_hash: String,
    from: String,
    to: String,
    selector: String,
    #[serde(default)]
    opcode_steps: u64,
    #[serde(default)]
    reads: Vec<String>,
    #[serde(default)]
    writes: Vec<String>,
}

struct VegetaCorpus {
    blocks: Vec<CorpusBlock>,
}

impl VegetaCorpus {
    fn load(path: &Path) -> Result<Self, HarnessError> {
        let source = fs::read_to_string(path).map_err(|error| {
            HarnessError::Runtime(format!(
                "failed to read Vegeta Ethereum corpus at {}: {error}; reconstruct S3 with tools/vegeta/extract-vegeta-ethereum.py",
                path.display()
            ))
        })?;
        let mut blocks = Vec::new();
        for (line_index, line) in source.lines().enumerate() {
            if line.trim().is_empty() {
                continue;
            }
            let block = serde_json::from_str::<CorpusBlock>(line).map_err(|error| {
                HarnessError::Runtime(format!(
                    "invalid Vegeta corpus JSON at {}:{}: {error}",
                    path.display(),
                    line_index + 1
                ))
            })?;
            for (expected, tx) in block.transactions.iter().enumerate() {
                if tx.tx_index != expected {
                    return Err(HarnessError::WorkloadParameter(format!(
                        "Vegeta corpus block {} transaction order is malformed: expected tx_index {}, got {}",
                        block.block_number, expected, tx.tx_index
                    )));
                }
            }
            blocks.push(block);
        }
        if blocks.is_empty() {
            return Err(HarnessError::WorkloadParameter(format!(
                "Vegeta corpus {} is empty",
                path.display()
            )));
        }
        for pair in blocks.windows(2) {
            if pair[0].block_number >= pair[1].block_number {
                return Err(HarnessError::WorkloadParameter(
                    "Vegeta corpus blocks must be strictly increasing".to_owned(),
                ));
            }
        }
        Ok(Self { blocks })
    }

    fn dataset_label(&self) -> &'static str {
        if self.blocks.first().map(|block| block.block_number) == Some(S3_START_BLOCK)
            && self.blocks.last().map(|block| block.block_number) == Some(S3_END_BLOCK)
            && self.blocks.len() == 101
        {
            "vegeta-s3"
        } else {
            "vegeta-custom"
        }
    }
}

#[derive(Default)]
struct MethodHistory {
    transactions: usize,
    read_counts: BTreeMap<String, usize>,
    write_counts: BTreeMap<String, usize>,
}

fn method_key(tx: &CorpusTransaction) -> String {
    format!(
        "{}:{}",
        tx.to.to_ascii_lowercase(),
        tx.selector.to_ascii_lowercase()
    )
}

fn build_predictions(
    corpus: &VegetaCorpus,
    block_index: usize,
    config: PredictionConfig,
) -> BTreeMap<String, (Vec<String>, Vec<String>)> {
    let history_start = block_index.saturating_sub(config.history_blocks);
    let mut methods = BTreeMap::<String, MethodHistory>::new();
    for block in &corpus.blocks[history_start..block_index] {
        for tx in &block.transactions {
            let history = methods.entry(method_key(tx)).or_default();
            history.transactions = history.transactions.saturating_add(1);
            for key in tx.reads.iter().cloned().collect::<BTreeSet<_>>() {
                *history.read_counts.entry(key).or_default() += 1;
            }
            for key in tx.writes.iter().cloned().collect::<BTreeSet<_>>() {
                *history.write_counts.entry(key).or_default() += 1;
            }
        }
    }
    methods
        .into_iter()
        .map(|(method, history)| {
            let reads = select_prediction_keys(
                history.transactions,
                history.read_counts,
                config.min_frequency_bps,
                config.max_keys_per_method,
            );
            let writes = select_prediction_keys(
                history.transactions,
                history.write_counts,
                config.min_frequency_bps,
                config.max_keys_per_method,
            );
            (method, (reads, writes))
        })
        .collect()
}

fn select_prediction_keys(
    transactions: usize,
    counts: BTreeMap<String, usize>,
    min_frequency_bps: u16,
    max_keys: usize,
) -> Vec<String> {
    if transactions == 0 || max_keys == 0 {
        return Vec::new();
    }
    let mut selected = counts
        .into_iter()
        .filter(|(_, count)| {
            count.saturating_mul(10_000)
                >= transactions.saturating_mul(usize::from(min_frequency_bps))
        })
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    selected.truncate(max_keys);
    selected.into_iter().map(|(key, _)| key).collect()
}

fn build_block(
    corpus: &VegetaCorpus,
    block_index: usize,
    contract: &Address,
    prediction: PredictionConfig,
    work: WorkConfig,
) -> Result<ProducedBlock, HarnessError> {
    let block = &corpus.blocks[block_index];
    let predictions = build_predictions(corpus, block_index, prediction);
    let mut transactions = Vec::with_capacity(block.transactions.len());
    for tx in &block.transactions {
        let (predicted_reads, predicted_writes) = predictions
            .get(&method_key(tx))
            .cloned()
            .unwrap_or_default();
        let work_iterations = if work.step_divisor == 0 {
            0
        } else {
            (tx.opcode_steps / work.step_divisor).min(work.max_iterations)
        };
        let request = ExecutionRequest::Execute {
            transaction_id: TransactionId(
                block.block_number.saturating_mul(100_000).saturating_add(
                    u64::try_from(tx.tx_index).map_err(|_| HarnessError::NumericOverflow)?,
                ),
            ),
            sender: Address::new(format!("eth-{}", tx.from.trim_start_matches("0x"))),
            contract: contract.clone(),
            funds: Vec::new(),
            msg: to_json_binary(&TraceExecuteMsg::Replay {
                predicted_reads,
                predicted_writes,
                actual_reads: tx.reads.clone(),
                actual_writes: tx.writes.clone(),
                work_iterations,
                tx_hash: tx.tx_hash.clone(),
                target: tx.to.clone(),
                selector: tx.selector.clone(),
            })
            .map_err(|error| HarnessError::Runtime(error.to_string()))?,
        };
        transactions.push(PendingTransaction {
            request,
            admitted_at_nanos: 0,
            admission_sequence: u64::try_from(tx.tx_index)
                .map_err(|_| HarnessError::NumericOverflow)?,
        });
    }
    Ok(ProducedBlock {
        context: BlockContext {
            height: block.block_number,
            time_nanos: block.timestamp.saturating_mul(1_000_000_000),
            chain_id: "ethereum-mainnet-vegeta-port".to_owned(),
            transaction_index: None,
        },
        transactions,
    })
}

struct PreparedVegetaEth {
    engine: CosmWasmEngine,
    graph: ProfileGraph,
    contract: Address,
    canonical_keys: Vec<String>,
    environment: BTreeMap<String, String>,
    warmup_blocks: Vec<ProducedBlock>,
    measured_block: ProducedBlock,
}

impl PreparedBenchmark for PreparedVegetaEth {
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
        for key in &self.canonical_keys {
            append_bytes(&mut bytes, key.as_bytes());
            let outcome = self
                .engine
                .query(
                    BlockContext::default(),
                    self.contract.clone(),
                    to_json_binary(&TraceQueryMsg::Value { key: key.clone() })
                        .map_err(|error| HarnessError::Runtime(error.to_string()))?,
                )
                .map_err(|error| HarnessError::Runtime(error.to_string()))?;
            append_bytes(&mut bytes, outcome.data.as_slice());
        }
        Ok(bytes)
    }
}

fn append_bytes(output: &mut Vec<u8>, bytes: &[u8]) {
    let length = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    output.extend_from_slice(&length.to_be_bytes());
    output.extend_from_slice(bytes);
}

#[derive(Serialize, Deserialize)]
struct TraceInstantiateMsg {}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TraceExecuteMsg {
    Replay {
        #[serde(default)]
        predicted_reads: Vec<String>,
        #[serde(default)]
        predicted_writes: Vec<String>,
        actual_reads: Vec<String>,
        actual_writes: Vec<String>,
        #[serde(default)]
        work_iterations: u64,
        tx_hash: String,
        target: String,
        selector: String,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TraceQueryMsg {
    Value { key: String },
}

#[derive(Serialize, Deserialize)]
struct TraceValueResponse {
    value: Option<Binary>,
}

struct VegetaTraceRuntime;

impl NativeContract for VegetaTraceRuntime {
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
        let TraceExecuteMsg::Replay {
            predicted_reads: _,
            predicted_writes: _,
            actual_reads,
            actual_writes,
            work_iterations,
            tx_hash,
            target,
            selector,
        } = serde_json::from_slice(msg.as_slice()).map_err(|error| error.to_string())?;

        let mut checksum = stable_seed(tx_hash.as_bytes());
        checksum ^= stable_seed(target.as_bytes()).rotate_left(7);
        checksum ^= stable_seed(selector.as_bytes()).rotate_left(19);
        for key in &actual_reads {
            checksum = mix(checksum, stable_seed(key.as_bytes()));
            if let Some(value) = context.storage_get(key.as_bytes()) {
                checksum = mix(checksum, stable_seed(&value));
            }
        }
        for round in 0..work_iterations {
            checksum = mix(checksum, round ^ 0x9E37_79B9_7F4A_7C15);
        }
        for key in &actual_writes {
            checksum = mix(checksum, stable_seed(key.as_bytes()));
            let mut value = Vec::with_capacity(24);
            value.extend_from_slice(&checksum.to_be_bytes());
            value.extend_from_slice(&stable_seed(tx_hash.as_bytes()).to_be_bytes());
            value.extend_from_slice(&stable_seed(key.as_bytes()).to_be_bytes());
            context.storage_set(key.as_bytes(), value);
        }
        Ok(Response::new())
    }

    fn query(
        &self,
        context: &mut NativeCallContext,
        _env: Env,
        msg: Binary,
    ) -> Result<Binary, String> {
        let TraceQueryMsg::Value { key } =
            serde_json::from_slice(msg.as_slice()).map_err(|error| error.to_string())?;
        to_json_binary(&TraceValueResponse {
            value: context.storage_get(key.as_bytes()).map(Binary::from),
        })
        .map_err(|error| error.to_string())
    }

    fn reply(
        &self,
        _context: &mut NativeCallContext,
        _env: Env,
        _reply: Reply,
    ) -> Result<Response<Empty>, String> {
        Err("Vegeta trace runtime has no reply entrypoint".to_owned())
    }
}

fn setup_engine(
    config: &VegetaEthConfig,
) -> Result<
    (
        CosmWasmEngine,
        acg_cosmwasm_engine::CodeId,
        BTreeMap<String, String>,
    ),
    HarnessError,
> {
    let engine = CosmWasmEngine::new(EngineConfig {
        gas_limit: config.vm_gas_limit,
        wasm_instance_lifecycle: config.vm_instance_lifecycle,
        ..EngineConfig::default()
    });
    let mut environment = BTreeMap::new();
    match config.backend {
        ExecutionBackend::Native => {
            let code_id = engine
                .register_native("vegeta-ethereum-trace", Arc::new(VegetaTraceRuntime))
                .map_err(|error| HarnessError::Runtime(error.to_string()))?;
            environment.insert("vegeta_backend".to_owned(), "native".to_owned());
            Ok((engine, code_id, environment))
        }
        ExecutionBackend::Wasm => {
            let path = vegeta_trace_wasm_path();
            let wasm = fs::read(&path).map_err(|error| {
                HarnessError::Runtime(format!(
                    "failed to read Vegeta trace Wasm at {}: {error}; build it with `cargo build --manifest-path benchmarks/Cargo.toml -p acg-benchmark-vegeta-trace --release --target wasm32-unknown-unknown` or set ACG_VEGETA_TRACE_WASM",
                    path.display()
                ))
            })?;
            if wasm.is_empty() {
                return Err(HarnessError::Runtime(format!(
                    "Vegeta trace Wasm artifact is empty: {}",
                    path.display()
                )));
            }
            let code_id = engine
                .upload_wasm(wasm)
                .map_err(|error| HarnessError::Runtime(error.to_string()))?;
            let checksum = engine
                .code_metadata(code_id)
                .ok_or_else(|| {
                    HarnessError::Runtime("Vegeta trace Wasm metadata missing".to_owned())
                })?
                .checksum;
            environment.insert("vegeta_backend".to_owned(), "wasm".to_owned());
            environment.insert("vegeta_wasm_path".to_owned(), path.display().to_string());
            environment.insert("vegeta_wasm_checksum".to_owned(), checksum.to_hex());
            Ok((engine, code_id, environment))
        }
    }
}

fn vm_instance_lifecycle_name(value: WasmInstanceLifecycle) -> &'static str {
    match value {
        WasmInstanceLifecycle::Reuse => "reuse",
        WasmInstanceLifecycle::Recycle => "recycle",
    }
}

fn parse_vm_instance_lifecycle(value: &str) -> Result<WasmInstanceLifecycle, HarnessError> {
    match value {
        "reuse" => Ok(WasmInstanceLifecycle::Reuse),
        "recycle" => Ok(WasmInstanceLifecycle::Recycle),
        other => Err(HarnessError::WorkloadParameter(format!(
            "vm_instance_lifecycle must be reuse or recycle for vegeta-eth, got {other:?}"
        ))),
    }
}

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn default_corpus_path() -> PathBuf {
    repository_root().join(DEFAULT_CORPUS_RELATIVE_PATH)
}

fn vegeta_trace_wasm_path() -> PathBuf {
    env::var_os("ACG_VEGETA_TRACE_WASM")
        .map(PathBuf::from)
        .unwrap_or_else(|| repository_root().join(DEFAULT_WASM_RELATIVE_PATH))
}

fn stable_seed(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xCBF2_9CE4_8422_2325_u64, |state, byte| {
        state.wrapping_mul(0x100_0000_01B3) ^ u64::from(*byte)
    })
}

fn mix(mut state: u64, value: u64) -> u64 {
    state ^= value.wrapping_add(0x9E37_79B9_7F4A_7C15);
    state ^= state >> 30;
    state = state.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    state ^= state >> 27;
    state = state.wrapping_mul(0x94D0_49BB_1331_11EB);
    state ^ (state >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prediction_model_uses_only_prior_blocks_and_cross_method_keys() {
        let corpus = VegetaCorpus {
            blocks: vec![
                CorpusBlock {
                    block_number: 1,
                    timestamp: 0,
                    transactions: vec![CorpusTransaction {
                        tx_index: 0,
                        tx_hash: "0x1".to_owned(),
                        from: "0x1".to_owned(),
                        to: "0xa".to_owned(),
                        selector: "0x11111111".to_owned(),
                        opcode_steps: 1,
                        reads: vec!["evm/a/01".to_owned()],
                        writes: vec!["evm/a/02".to_owned()],
                    }],
                },
                CorpusBlock {
                    block_number: 2,
                    timestamp: 0,
                    transactions: vec![CorpusTransaction {
                        tx_index: 0,
                        tx_hash: "0x2".to_owned(),
                        from: "0x2".to_owned(),
                        to: "0xa".to_owned(),
                        selector: "0x11111111".to_owned(),
                        opcode_steps: 1,
                        reads: vec!["future-only".to_owned()],
                        writes: vec![],
                    }],
                },
            ],
        };
        let predictions = build_predictions(
            &corpus,
            1,
            PredictionConfig {
                history_blocks: 20,
                min_frequency_bps: 1,
                max_keys_per_method: 32,
            },
        );
        let (reads, writes) = predictions.get("0xa:0x11111111").unwrap();
        assert_eq!(reads, &vec!["evm/a/01".to_owned()]);
        assert_eq!(writes, &vec!["evm/a/02".to_owned()]);
        assert!(!reads.contains(&"future-only".to_owned()));
    }
}
