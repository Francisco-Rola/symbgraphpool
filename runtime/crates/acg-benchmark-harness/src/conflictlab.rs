use std::{hint::black_box, sync::Arc};

use acg_core::{ContractCodeHash, RuntimeId};
use acg_cosmwasm_engine::{
    Address, BlockContext, CosmWasmEngine, ExecutionRequest, NativeCallContext, NativeContract,
    TransactionId,
};
use acg_evaluation::RunIdentity;
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use acg_validator_sim::{BlockProducer, BlockProducerConfig, Mempool, ProducedBlock};
use cosmwasm_std::{to_json_binary, Binary, Empty, Env, MessageInfo, Reply, Response};
use serde_json::{json, Value};

use crate::{parameter, BenchmarkWorkload, HarnessError, PreparedBenchmark};

const CONFLICTLAB_SYMBOLIC: &[u8] =
    include_bytes!("../../../../benchmarks/symbolic/conflictlab.symbolic.json");
const BASIS_POINTS: u16 = 10_000;

/// Built-in deterministic ConflictLab adapter used to validate and tune the common harness.
///
/// Workload parameters:
/// - `transactions` (default 200)
/// - `warmup_blocks` (default 0)
/// - `accounts` (default 16)
/// - `hot_account_probability_bps` (default 0)
/// - `work_iterations` (default 0)
/// - `warmup_hot_account_probability_bps` (defaults to measured hot probability)
/// - `warmup_work_iterations` (defaults to measured work iterations)
///
/// Keys prefixed with `acg.` are reserved for the common scheduler/feedback tuning layer and are
/// ignored by this adapter.
#[derive(Clone, Copy, Debug, Default)]
pub struct ConflictLabWorkload;

impl BenchmarkWorkload for ConflictLabWorkload {
    fn name(&self) -> &'static str {
        "conflictlab"
    }

    fn prepare(&self, run: &RunIdentity) -> Result<Box<dyn PreparedBenchmark>, HarnessError> {
        let config = ConflictLabConfig::from_run(run)?;
        let engine = CosmWasmEngine::default();
        let code_id = engine
            .register_native("conflictlab-harness", Arc::new(ConflictLabRuntime))
            .map_err(|error| HarnessError::Runtime(error.to_string()))?;
        let checksum = engine
            .code_metadata(code_id)
            .ok_or_else(|| {
                HarnessError::Runtime("registered ConflictLab code metadata missing".to_owned())
            })?
            .checksum;
        let contract = engine
            .instantiate(
                TransactionId(900),
                BlockContext::default(),
                Address::new("creator"),
                code_id,
                None,
                "conflictlab-harness".to_owned(),
                Vec::new(),
                Binary::default(),
            )
            .map_err(|error| HarnessError::Runtime(error.to_string()))?
            .contract;

        let context = IngestionContext::new(
            RuntimeId::new("cosmwasm").map_err(|error| HarnessError::Runtime(error.to_string()))?,
            ContractCodeHash(*checksum.as_bytes()),
            1,
        );
        let profiles = normalize_document(
            parse_slice(CONFLICTLAB_SYMBOLIC)
                .map_err(|error| HarnessError::Runtime(error.to_string()))?,
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
                config.transactions,
                next_transaction_id,
                height,
                config.warmup_work_iterations,
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
            config.transactions,
            next_transaction_id,
            measured_height,
            config.work_iterations,
        )?);
        let measured_block = blocks.pop().ok_or_else(|| {
            HarnessError::Runtime("ConflictLab produced no measured block".to_owned())
        })?;

        Ok(Box::new(PreparedConflictLab {
            engine,
            graph,
            contract,
            accounts: config.accounts,
            warmup_blocks: blocks,
            measured_block,
        }))
    }
}

#[derive(Clone, Copy, Debug)]
struct ConflictLabConfig {
    transactions: usize,
    warmup_blocks: usize,
    accounts: u64,
    hot_bps: u16,
    work_iterations: u64,
    warmup_hot_bps: u16,
    warmup_work_iterations: u64,
}

impl ConflictLabConfig {
    fn from_run(run: &RunIdentity) -> Result<Self, HarnessError> {
        const WORKLOAD_KEYS: &[&str] = &[
            "transactions",
            "warmup_blocks",
            "accounts",
            "hot_account_probability_bps",
            "work_iterations",
            "warmup_hot_account_probability_bps",
            "warmup_work_iterations",
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
        let warmup_hot_bps = parameter(
            &run.parameters,
            "warmup_hot_account_probability_bps",
            hot_bps,
        )?;
        let warmup_work_iterations =
            parameter(&run.parameters, "warmup_work_iterations", work_iterations)?;
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
        if hot_bps > BASIS_POINTS {
            return Err(HarnessError::WorkloadParameter(format!(
                "hot_account_probability_bps must be <= {BASIS_POINTS}, got {hot_bps}"
            )));
        }
        if warmup_hot_bps > BASIS_POINTS {
            return Err(HarnessError::WorkloadParameter(format!(
                "warmup_hot_account_probability_bps must be <= {BASIS_POINTS}, got {warmup_hot_bps}"
            )));
        }
        Ok(Self {
            transactions,
            warmup_blocks,
            accounts,
            hot_bps,
            work_iterations,
            warmup_hot_bps,
            warmup_work_iterations,
        })
    }
}

struct PreparedConflictLab {
    engine: CosmWasmEngine,
    graph: ProfileGraph,
    contract: Address,
    accounts: u64,
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

    fn canonical_state_bytes(&self) -> Result<Vec<u8>, HarnessError> {
        let mut bytes = Vec::new();
        append_bytes(&mut bytes, self.contract.as_str().as_bytes());
        bytes.extend_from_slice(&self.accounts.to_be_bytes());
        for account_id in 0..self.accounts {
            let account = format!("account-{account_id}");
            append_bytes(&mut bytes, account.as_bytes());
            let key = format!("balance/{account}");
            match self.engine.raw_storage(&self.contract, key.as_bytes()) {
                Some(value) => {
                    bytes.push(1);
                    append_bytes(&mut bytes, &value);
                }
                None => bytes.push(0),
            }
        }
        Ok(bytes)
    }
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
        let value: Value =
            serde_json::from_slice(msg.as_slice()).map_err(|error| error.to_string())?;
        let credit = value
            .get("credit")
            .and_then(Value::as_object)
            .ok_or_else(|| "expected credit message".to_owned())?;
        let account = credit
            .get("account")
            .and_then(Value::as_str)
            .ok_or_else(|| "credit account missing".to_owned())?;
        let amount = credit
            .get("amount")
            .and_then(Value::as_u64)
            .ok_or_else(|| "credit amount missing".to_owned())?;
        let work_iterations = credit
            .get("work_iterations")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let key = format!("balance/{account}");
        let current = context
            .storage_get(key.as_bytes())
            .and_then(|bytes| <[u8; 8]>::try_from(bytes).ok())
            .map(u64::from_be_bytes)
            .unwrap_or_default();
        deterministic_work(work_iterations, current ^ amount);
        context.storage_set(key.as_bytes(), current.saturating_add(amount).to_be_bytes());
        Ok(Response::new())
    }

    fn query(
        &self,
        _context: &mut NativeCallContext,
        _env: Env,
        _msg: Binary,
    ) -> Result<Binary, String> {
        Ok(Binary::default())
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

fn generate_block(
    contract: &Address,
    generator: &mut ConflictLabGenerator,
    transactions: usize,
    first_transaction_id: u64,
    height: u64,
    work_iterations: u64,
) -> Result<ProducedBlock, HarnessError> {
    let mempool = Mempool::default();
    for offset in 0..transactions {
        let transaction_id = first_transaction_id
            .saturating_add(u64::try_from(offset).map_err(|_| HarnessError::NumericOverflow)?);
        let account = generator.next_account();
        let request = ExecutionRequest::Execute {
            transaction_id: TransactionId(transaction_id),
            sender: Address::new("client"),
            contract: contract.clone(),
            funds: Vec::new(),
            msg: to_json_binary(&json!({
                "credit": {
                    "account": account,
                    "amount": 1_u64,
                    "work_iterations": work_iterations
                }
            }))
            .map_err(|error| HarnessError::Runtime(error.to_string()))?,
        };
        mempool.admit(
            request,
            u64::try_from(offset).map_err(|_| HarnessError::NumericOverflow)?,
        );
    }
    let mut block = BlockProducer::fifo(BlockProducerConfig::default())
        .map_err(|error| HarnessError::Runtime(error.to_string()))?
        .produce_next(&mempool);
    block.context.height = height;
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

fn deterministic_work(iterations: u64, seed: u64) {
    let mut value = seed ^ 0xD6E8_FEB8_6659_FD93;
    for index in 0..iterations {
        value = value
            .wrapping_add(index.rotate_left((index & 31) as u32))
            .wrapping_mul(0x9E37_79B9_7F4A_7C15)
            ^ value.rotate_right(11);
    }
    black_box(value);
}

fn append_bytes(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    output.extend_from_slice(value);
}
