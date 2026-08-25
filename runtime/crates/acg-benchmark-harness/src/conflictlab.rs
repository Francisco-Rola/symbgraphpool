use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs,
    hint::black_box,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use acg_core::{ContractCodeHash, RuntimeId};
use acg_cosmwasm_engine::{
    Address, BlockContext, CodeId, CosmWasmEngine, EngineConfig, ExecutionRequest,
    NativeCallContext, NativeContract, TransactionId, WasmInstanceLifecycle,
};
use acg_evaluation::RunIdentity;
use acg_profile_graph::{EdgeBuildConfig, GraphLoadConfig, ProfileGraph, ProfileGraphArtifact};
use acg_symbolic_json::{normalize_document, parse_slice, IngestionContext};
use acg_validator_sim::{
    BlockProducer, BlockProducerConfig, IngressConfig, Mempool, ProducedBlock,
    RateControlledIngress, DEFAULT_BENCHMARK_INGRESS_TPS,
};
use cosmwasm_std::{
    to_json_binary, Binary, Coin, Empty, Env, MessageInfo, Reply, Response, Uint128,
};
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
        let (engine, code_id, mut environment) = setup_engine(
            config.execution_backend,
            config.vm_instance_lifecycle,
            config.vm_gas_limit,
        )?;
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
            "conflictlab_vm_instance_lifecycle_safe".to_owned(),
            (config.vm_instance_lifecycle == WasmInstanceLifecycle::Recycle).to_string(),
        );
        environment.insert(
            "conflictlab_vm_gas_limit".to_owned(),
            config.vm_gas_limit.to_string(),
        );
        environment.insert(
            "conflictlab_retained_vm_scope".to_owned(),
            if config.vm_instance_lifecycle == WasmInstanceLifecycle::Reuse {
                "benchmark-scoped-nonbinding-gas"
            } else {
                "fresh-instance"
            }
            .to_owned(),
        );
        environment.insert(
            "conflictlab_complexity_mix".to_owned(),
            config.complexity_mix.as_str().to_owned(),
        );
        environment.insert(
            "conflictlab_consensus_divergence".to_owned(),
            config.consensus_divergence.as_str().to_owned(),
        );
        environment.insert(
            "conflictlab_symbolic_granularity".to_owned(),
            config.symbolic_granularity.as_str().to_owned(),
        );
        environment.insert(
            "conflictlab_prediction_fault_mode".to_owned(),
            config.prediction_fault_mode.as_str().to_owned(),
        );
        environment.insert(
            "conflictlab_prediction_fault_rate_bps".to_owned(),
            config.prediction_fault_rate_bps.to_string(),
        );
        environment.insert(
            "conflictlab_operation_mix".to_owned(),
            config.operation_mix.as_str().to_owned(),
        );
        environment.insert(
            "conflictlab_parallelism_lanes".to_owned(),
            config.parallelism_lanes.to_string(),
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
        seed_conflictlab_state(&engine, &contract, config)?;

        let context = IngestionContext::new(
            RuntimeId::new("cosmwasm").map_err(|error| HarnessError::Runtime(error.to_string()))?,
            ContractCodeHash(*checksum.as_bytes()),
            1,
        );
        let symbolic =
            conflictlab_symbolic(config.prediction_quality, config.symbolic_granularity)?;
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
        let mut alternate_generator = ConflictLabGenerator::new(
            run.seed ^ 0xA5A5_5A5A_D1CE_B10C,
            config.accounts,
            config.warmup_hot_bps,
        );
        let total_warmups = config
            .warmup_blocks
            .saturating_add(config.postchange_warmup_blocks);
        let mut predicted_blocks = Vec::with_capacity(total_warmups.saturating_add(1));
        let mut decided_blocks = Vec::with_capacity(total_warmups.saturating_add(1));
        let mut next_transaction_id = 1_000_u64;

        for block_offset in 0..config.warmup_blocks {
            let height = u64::try_from(block_offset)
                .map_err(|_| HarnessError::NumericOverflow)?
                .saturating_add(1);
            let generation = GenerateBlockConfig {
                offered_transactions: config.transactions,
                first_transaction_id: next_transaction_id,
                height,
                selection_seed: run.seed ^ height,
                code_id,
                prediction_quality: config.prediction_quality,
                prediction_buckets: config.prediction_buckets,
                prediction_fault_mode: config.prediction_fault_mode,
                prediction_fault_rate_bps: config.prediction_fault_rate_bps,
                operation_mix: config.operation_mix,
                parallelism_lanes: config.parallelism_lanes,
                work: WorkShape {
                    work_iterations: config.warmup_work_iterations,
                    storage_rounds: config.warmup_storage_rounds,
                    payload_bytes: config.warmup_payload_bytes,
                },
                complexity_mix: config.complexity_mix,
                simulation: config.simulation,
            };
            let predicted = generate_block(&contract, &mut generator, generation)?;
            let alternate = generate_block(
                &contract,
                &mut alternate_generator,
                GenerateBlockConfig {
                    first_transaction_id: alternate_transaction_base(next_transaction_id, height),
                    selection_seed: (run.seed ^ 0xD1CE_B10C) ^ height,
                    ..generation
                },
            )?;
            let decided =
                apply_consensus_divergence(&predicted, &alternate, config.consensus_divergence)?;
            next_transaction_id = next_transaction_id.saturating_add(
                u64::try_from(config.transactions).map_err(|_| HarnessError::NumericOverflow)?,
            );
            predicted_blocks.push(predicted);
            decided_blocks.push(decided);
        }

        // Optional post-change warmups let the evaluation sample the k-th block after an abrupt
        // workload transition while retaining one-record-per-run manifests. These blocks use the
        // measured regime and therefore update feedback/admission before the final measured block.
        generator.hot_bps = config.hot_bps;
        alternate_generator.hot_bps = config.hot_bps;
        for transition_offset in 0..config.postchange_warmup_blocks {
            let block_offset = config.warmup_blocks.saturating_add(transition_offset);
            let height = u64::try_from(block_offset)
                .map_err(|_| HarnessError::NumericOverflow)?
                .saturating_add(1);
            let (prediction_fault_mode, prediction_fault_rate_bps) =
                config.measured_regime_fault(transition_offset);
            let generation = GenerateBlockConfig {
                offered_transactions: config.transactions,
                first_transaction_id: next_transaction_id,
                height,
                selection_seed: run.seed ^ height,
                code_id,
                prediction_quality: config.prediction_quality,
                prediction_buckets: config.prediction_buckets,
                prediction_fault_mode,
                prediction_fault_rate_bps,
                operation_mix: config.operation_mix,
                parallelism_lanes: config.parallelism_lanes,
                work: WorkShape {
                    work_iterations: config.work_iterations,
                    storage_rounds: config.storage_rounds,
                    payload_bytes: config.payload_bytes,
                },
                complexity_mix: config.complexity_mix,
                simulation: config.simulation,
            };
            let predicted = generate_block(&contract, &mut generator, generation)?;
            let alternate = generate_block(
                &contract,
                &mut alternate_generator,
                GenerateBlockConfig {
                    first_transaction_id: alternate_transaction_base(next_transaction_id, height),
                    selection_seed: (run.seed ^ 0xD1CE_B10C) ^ height,
                    ..generation
                },
            )?;
            let decided =
                apply_consensus_divergence(&predicted, &alternate, config.consensus_divergence)?;
            next_transaction_id = next_transaction_id.saturating_add(
                u64::try_from(config.transactions).map_err(|_| HarnessError::NumericOverflow)?,
            );
            predicted_blocks.push(predicted);
            decided_blocks.push(decided);
        }

        let measured_height = u64::try_from(total_warmups)
            .map_err(|_| HarnessError::NumericOverflow)?
            .saturating_add(1);
        let (prediction_fault_mode, prediction_fault_rate_bps) =
            config.measured_regime_fault(config.postchange_warmup_blocks);
        let generation = GenerateBlockConfig {
            offered_transactions: config.transactions,
            first_transaction_id: next_transaction_id,
            height: measured_height,
            selection_seed: run.seed ^ measured_height,
            code_id,
            prediction_quality: config.prediction_quality,
            prediction_buckets: config.prediction_buckets,
            prediction_fault_mode,
            prediction_fault_rate_bps,
            operation_mix: config.operation_mix,
            parallelism_lanes: config.parallelism_lanes,
            work: WorkShape {
                work_iterations: config.work_iterations,
                storage_rounds: config.storage_rounds,
                payload_bytes: config.payload_bytes,
            },
            complexity_mix: config.complexity_mix,
            simulation: config.simulation,
        };
        let measured_block = generate_block(&contract, &mut generator, generation)?;
        let measured_alternate = generate_block(
            &contract,
            &mut alternate_generator,
            GenerateBlockConfig {
                first_transaction_id: alternate_transaction_base(
                    next_transaction_id,
                    measured_height,
                ),
                selection_seed: (run.seed ^ 0xD1CE_B10C) ^ measured_height,
                ..generation
            },
        )?;
        let measured_decided_block = apply_consensus_divergence(
            &measured_block,
            &measured_alternate,
            config.consensus_divergence,
        )?;

        let canonical_queries = collect_canonical_queries(
            &engine,
            &contract,
            config.accounts,
            decided_blocks
                .iter()
                .chain(std::iter::once(&measured_decided_block)),
        )?;
        let bank_balance_addresses = if config.operation_mix.uses_bank_funds() {
            let mut addresses = (0..config.accounts)
                .map(|account| Address::new(format!("account-{account}")))
                .collect::<Vec<_>>();
            addresses.push(contract.clone());
            addresses
        } else {
            Vec::new()
        };

        Ok(Box::new(PreparedConflictLab {
            engine,
            graph,
            contract,
            accounts: config.accounts,
            canonical_queries,
            bank_balance_addresses,
            environment,
            warmup_blocks: predicted_blocks,
            warmup_decided_blocks: decided_blocks,
            measured_block,
            measured_decided_block,
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

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum SymbolicGranularity {
    #[default]
    Fine,
    Resource,
    Profile,
}

impl SymbolicGranularity {
    fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "fine" => Ok(Self::Fine),
            "resource" => Ok(Self::Resource),
            "profile" => Ok(Self::Profile),
            other => Err(HarnessError::WorkloadParameter(format!(
                "symbolic_granularity must be fine, resource, or profile; got {other:?}"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Fine => "fine",
            Self::Resource => "resource",
            Self::Profile => "profile",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PredictionFaultMode {
    #[default]
    None,
    HiddenKey,
    SpuriousKey,
}

impl PredictionFaultMode {
    fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "none" => Ok(Self::None),
            "hidden-key" => Ok(Self::HiddenKey),
            "spurious-key" => Ok(Self::SpuriousKey),
            other => Err(HarnessError::WorkloadParameter(format!(
                "prediction_fault_mode must be none, hidden-key, or spurious-key; got {other:?}"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::HiddenKey => "hidden-key",
            Self::SpuriousKey => "spurious-key",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum OperationMix {
    #[default]
    Credit,
    PointMixed,
    StatefulMixed,
    RangeDelete,
    BankFunds,
    BankMixed,
    Instantiate,
    Full,
}

impl OperationMix {
    fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "credit" => Ok(Self::Credit),
            "point-mixed" => Ok(Self::PointMixed),
            "stateful-mixed" => Ok(Self::StatefulMixed),
            "range-delete" => Ok(Self::RangeDelete),
            "bank-funds" => Ok(Self::BankFunds),
            "bank-mixed" => Ok(Self::BankMixed),
            "instantiate" => Ok(Self::Instantiate),
            "full" => Ok(Self::Full),
            other => Err(HarnessError::WorkloadParameter(format!(
                "operation_mix must be credit, point-mixed, stateful-mixed, range-delete, bank-funds, bank-mixed, instantiate, or full; got {other:?}"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Credit => "credit",
            Self::PointMixed => "point-mixed",
            Self::StatefulMixed => "stateful-mixed",
            Self::RangeDelete => "range-delete",
            Self::BankFunds => "bank-funds",
            Self::BankMixed => "bank-mixed",
            Self::Instantiate => "instantiate",
            Self::Full => "full",
        }
    }

    fn requires_seeded_contract_balances(self) -> bool {
        matches!(self, Self::StatefulMixed | Self::Full)
    }

    fn uses_bank_funds(self) -> bool {
        matches!(self, Self::BankFunds | Self::BankMixed)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum ConsensusDivergence {
    #[default]
    Identical,
    Tail5,
    Tail20,
    Reorder5,
    Reorder20,
    TailReorder10,
}

impl ConsensusDivergence {
    fn parse(value: &str) -> Result<Self, HarnessError> {
        match value {
            "identical" => Ok(Self::Identical),
            "tail-5pct" => Ok(Self::Tail5),
            "tail-20pct" => Ok(Self::Tail20),
            "reorder-5pct" => Ok(Self::Reorder5),
            "reorder-20pct" => Ok(Self::Reorder20),
            "tail-reorder-10pct" => Ok(Self::TailReorder10),
            other => Err(HarnessError::WorkloadParameter(format!(
                "consensus_divergence must be identical, tail-5pct, tail-20pct, reorder-5pct, reorder-20pct, or tail-reorder-10pct; got {other:?}"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Identical => "identical",
            Self::Tail5 => "tail-5pct",
            Self::Tail20 => "tail-20pct",
            Self::Reorder5 => "reorder-5pct",
            Self::Reorder20 => "reorder-20pct",
            Self::TailReorder10 => "tail-reorder-10pct",
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
    vm_gas_limit: u64,
    prediction_quality: PredictionQuality,
    complexity_mix: ComplexityMix,
    prediction_buckets: u16,
    symbolic_granularity: SymbolicGranularity,
    prediction_fault_mode: PredictionFaultMode,
    prediction_fault_rate_bps: u16,
    /// Number of measured-regime blocks for which the configured prediction fault remains active.
    /// `usize::MAX` preserves the historical persistent-fault behavior; `1` models a transient
    /// one-block fault followed by clean recovery blocks.
    prediction_fault_duration_blocks: usize,
    operation_mix: OperationMix,
    /// Zero keeps the normal random/hot-account generator. A positive value assigns transaction
    /// `i` to `account-(i % parallelism_lanes)`, creating exactly that many balanced conflict
    /// chains for the controlled parallelism-ceiling experiment.
    parallelism_lanes: u64,
    postchange_warmup_blocks: usize,
    consensus_divergence: ConsensusDivergence,
    simulation: SimulationConfig,
}

fn measured_regime_fault(
    mode: PredictionFaultMode,
    rate_bps: u16,
    duration_blocks: usize,
    regime_block_offset: usize,
) -> (PredictionFaultMode, u16) {
    if regime_block_offset < duration_blocks {
        (mode, rate_bps)
    } else {
        (PredictionFaultMode::None, 0)
    }
}

impl ConflictLabConfig {
    fn measured_regime_fault(self, regime_block_offset: usize) -> (PredictionFaultMode, u16) {
        measured_regime_fault(
            self.prediction_fault_mode,
            self.prediction_fault_rate_bps,
            self.prediction_fault_duration_blocks,
            regime_block_offset,
        )
    }

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
            "vm_gas_limit",
            "prediction_quality",
            "complexity_mix",
            "prediction_buckets",
            "symbolic_granularity",
            "prediction_fault_mode",
            "prediction_fault_rate_bps",
            "prediction_fault_duration_blocks",
            "operation_mix",
            "parallelism_lanes",
            "postchange_warmup_blocks",
            "consensus_divergence",
            "consensus_cutoff_ms",
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
                .unwrap_or("recycle"),
        )?;
        let vm_gas_limit = parameter(
            &run.parameters,
            "vm_gas_limit",
            EngineConfig::default().gas_limit,
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
        let symbolic_granularity = SymbolicGranularity::parse(
            run.parameters
                .get("symbolic_granularity")
                .map(String::as_str)
                .unwrap_or("fine"),
        )?;
        let prediction_fault_mode = PredictionFaultMode::parse(
            run.parameters
                .get("prediction_fault_mode")
                .map(String::as_str)
                .unwrap_or("none"),
        )?;
        let prediction_fault_rate_bps =
            parameter(&run.parameters, "prediction_fault_rate_bps", 0_u16)?;
        let prediction_fault_duration_blocks = parameter(
            &run.parameters,
            "prediction_fault_duration_blocks",
            usize::MAX,
        )?;
        let operation_mix = OperationMix::parse(
            run.parameters
                .get("operation_mix")
                .map(String::as_str)
                .unwrap_or("credit"),
        )?;
        let parallelism_lanes = parameter(&run.parameters, "parallelism_lanes", 0_u64)?;
        let postchange_warmup_blocks =
            parameter(&run.parameters, "postchange_warmup_blocks", 0_usize)?;
        let consensus_divergence = ConsensusDivergence::parse(
            run.parameters
                .get("consensus_divergence")
                .map(String::as_str)
                .unwrap_or("identical"),
        )?;
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
        if prediction_fault_rate_bps > BASIS_POINTS {
            return Err(HarnessError::WorkloadParameter(format!(
                "prediction_fault_rate_bps must be <= {BASIS_POINTS}"
            )));
        }
        if prediction_fault_mode == PredictionFaultMode::None && prediction_fault_rate_bps != 0 {
            return Err(HarnessError::WorkloadParameter(
                "prediction_fault_rate_bps requires prediction_fault_mode != none".to_owned(),
            ));
        }
        if execution_backend == ExecutionBackend::Native && operation_mix != OperationMix::Credit {
            return Err(HarnessError::WorkloadParameter(
                "non-credit operation_mix values require execution_backend=wasm".to_owned(),
            ));
        }
        if parallelism_lanes > 0 {
            if operation_mix != OperationMix::Credit {
                return Err(HarnessError::WorkloadParameter(
                    "parallelism_lanes is defined only for operation_mix=credit".to_owned(),
                ));
            }
            if parallelism_lanes > accounts {
                return Err(HarnessError::WorkloadParameter(format!(
                    "parallelism_lanes={parallelism_lanes} exceeds accounts={accounts}"
                )));
            }
            let transaction_count =
                u64::try_from(transactions).map_err(|_| HarnessError::NumericOverflow)?;
            if parallelism_lanes > transaction_count {
                return Err(HarnessError::WorkloadParameter(format!(
                    "parallelism_lanes={parallelism_lanes} exceeds transactions={transactions}"
                )));
            }
            if hot_bps != 0 || warmup_hot_bps != 0 {
                return Err(HarnessError::WorkloadParameter(
                    "parallelism_lanes requires hot_account_probability_bps=0 (including warmup)"
                        .to_owned(),
                ));
            }
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
            vm_gas_limit,
            prediction_quality,
            complexity_mix,
            prediction_buckets,
            symbolic_granularity,
            prediction_fault_mode,
            prediction_fault_rate_bps,
            prediction_fault_duration_blocks,
            operation_mix,
            parallelism_lanes,
            postchange_warmup_blocks,
            consensus_divergence,
            simulation: SimulationConfig {
                admission_tps,
                block_interval_ms,
                block_size,
                mempool_policy,
            },
        })
    }
}

fn conflictlab_symbolic(
    prediction_quality: PredictionQuality,
    granularity: SymbolicGranularity,
) -> Result<Vec<u8>, HarnessError> {
    let mut document: serde_json::Value = serde_json::from_slice(CONFLICTLAB_SYMBOLIC)
        .map_err(|error| HarnessError::Runtime(error.to_string()))?;

    if prediction_quality == PredictionQuality::Coarse {
        let profiles = document
            .get_mut("profiles")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or_else(|| {
                HarnessError::Runtime("ConflictLab symbolic profiles missing".to_owned())
            })?;
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
            .ok_or_else(|| {
                HarnessError::Runtime("ConflictLab Credit accesses missing".to_owned())
            })?;
        for access in accesses {
            let key = access
                .get_mut("key")
                .and_then(serde_json::Value::as_object_mut)
                .ok_or_else(|| {
                    HarnessError::Runtime("ConflictLab Credit key missing".to_owned())
                })?;
            key.insert("depends_on".to_owned(), serde_json::Value::Null);
        }
    }

    match granularity {
        SymbolicGranularity::Fine => {}
        SymbolicGranularity::Resource => {
            let profiles = document
                .get_mut("profiles")
                .and_then(serde_json::Value::as_array_mut)
                .ok_or_else(|| {
                    HarnessError::Runtime("ConflictLab symbolic profiles missing".to_owned())
                })?;
            for profile in profiles {
                let accesses = profile
                    .get_mut("accesses")
                    .and_then(serde_json::Value::as_array_mut)
                    .ok_or_else(|| {
                        HarnessError::Runtime("ConflictLab profile accesses missing".to_owned())
                    })?;
                for access in accesses {
                    let key = access
                        .get_mut("key")
                        .and_then(serde_json::Value::as_object_mut)
                        .ok_or_else(|| {
                            HarnessError::Runtime("ConflictLab access key missing".to_owned())
                        })?;
                    // Resource granularity is a conservative whole-resource collapse, not an
                    // unresolved logical key. Raw semantic-name lists normalize to
                    // SemanticKeyKind::FieldSet, which profile-edge derivation represents as
                    // KeyMatch::WholeResource. Using a scalar here would instead create an
                    // unresolved key and let adaptive probability thresholds prune relationships
                    // that this ablation is supposed to retain conservatively.
                    key.insert(
                        "semantic_name".to_owned(),
                        serde_json::json!(["__whole_resource__"]),
                    );
                    key.insert("depends_on".to_owned(), serde_json::Value::Null);
                }
            }
        }
        SymbolicGranularity::Profile => {
            let resources = document
                .get_mut("storage_resources")
                .and_then(serde_json::Value::as_object_mut)
                .ok_or_else(|| {
                    HarnessError::Runtime("ConflictLab storage resources missing".to_owned())
                })?;
            resources.insert(
                "__PROFILE_STATE__".to_owned(),
                serde_json::json!({"key_semantic_name":"all contract state"}),
            );
            let profiles = document
                .get_mut("profiles")
                .and_then(serde_json::Value::as_array_mut)
                .ok_or_else(|| {
                    HarnessError::Runtime("ConflictLab symbolic profiles missing".to_owned())
                })?;
            for profile in profiles {
                let accesses = profile
                    .get_mut("accesses")
                    .and_then(serde_json::Value::as_array_mut)
                    .ok_or_else(|| {
                        HarnessError::Runtime("ConflictLab profile accesses missing".to_owned())
                    })?;
                for access in accesses {
                    let object = access.as_object_mut().ok_or_else(|| {
                        HarnessError::Runtime("ConflictLab access is not an object".to_owned())
                    })?;
                    object.insert(
                        "resource".to_owned(),
                        serde_json::Value::String("__PROFILE_STATE__".to_owned()),
                    );
                    object.insert(
                        "key".to_owned(),
                        serde_json::json!({"semantic_name":["all"],"depends_on":null}),
                    );
                    object.insert(
                        "guard".to_owned(),
                        serde_json::json!({"expression":"true","dependency_kind":"none"}),
                    );
                }
            }
        }
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
            "vm_instance_lifecycle must be recycle or benchmark-scoped retained reuse, got {other:?}"
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
    vm_gas_limit: u64,
) -> Result<
    (
        CosmWasmEngine,
        acg_cosmwasm_engine::CodeId,
        BTreeMap<String, String>,
    ),
    HarnessError,
> {
    let engine = CosmWasmEngine::new(EngineConfig {
        gas_limit: vm_gas_limit,
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

fn seed_conflictlab_state(
    engine: &CosmWasmEngine,
    contract: &Address,
    config: ConflictLabConfig,
) -> Result<(), HarnessError> {
    if config.operation_mix.requires_seeded_contract_balances() {
        for account_id in 0..config.accounts {
            let account = format!("account-{account_id}");
            let request = ExecutionRequest::Execute {
                transaction_id: TransactionId(100_000_000_u64.saturating_add(account_id)),
                sender: Address::new("seed"),
                contract: contract.clone(),
                funds: Vec::new(),
                msg: to_json_binary(&ConflictLabExecuteMsg::Credit {
                    account,
                    amount: Uint128::new(1_000_000),
                    work_iterations: 0,
                    storage_rounds: 0,
                    payload: Binary::default(),
                })
                .map_err(|error| HarnessError::Runtime(error.to_string()))?,
            };
            engine
                .execute_request(BlockContext::default(), request)
                .map_err(|error| HarnessError::Runtime(error.to_string()))?;
        }
    }

    if config.operation_mix.uses_bank_funds() {
        for account_id in 0..config.accounts {
            engine
                .set_balance(
                    Address::new(format!("account-{account_id}")),
                    &[Coin::new(1_000_000_u128, "uconflict")],
                )
                .map_err(|error| HarnessError::Runtime(error.to_string()))?;
        }
    }
    Ok(())
}

#[derive(Clone)]
struct CanonicalQuery {
    contract: Address,
    msg: Binary,
}

fn collect_canonical_queries<'a>(
    engine: &CosmWasmEngine,
    contract: &Address,
    accounts: u64,
    blocks: impl Iterator<Item = &'a ProducedBlock>,
) -> Result<Vec<CanonicalQuery>, HarnessError> {
    let mut dedup = BTreeSet::<Vec<u8>>::new();
    let mut queries = Vec::<CanonicalQuery>::new();
    let mut add_query = |target: Address, msg: Binary| {
        let mut key = Vec::new();
        key.extend_from_slice(target.as_str().as_bytes());
        key.push(0);
        key.extend_from_slice(msg.as_slice());
        if dedup.insert(key) {
            queries.push(CanonicalQuery {
                contract: target,
                msg,
            });
        }
    };

    // Preserve the historical correctness digest's complete account coverage.
    for account_id in 0..accounts {
        add_query(
            contract.clone(),
            to_json_binary(&ConflictLabQueryMsg::Balance {
                account: format!("account-{account_id}"),
            })
            .map_err(|error| HarnessError::Runtime(error.to_string()))?,
        );
    }

    for block in blocks {
        for pending in &block.transactions {
            match &pending.request {
                ExecutionRequest::Instantiate { transaction_id, .. } => {
                    let address = engine.predict_contract_address(*transaction_id, 0);
                    add_query(
                        address,
                        to_json_binary(&ConflictLabQueryMsg::Config {})
                            .map_err(|error| HarnessError::Runtime(error.to_string()))?,
                    );
                }
                ExecutionRequest::Execute {
                    contract: target,
                    msg,
                    ..
                } if target == contract => {
                    let message: ConflictLabExecuteMsg = serde_json::from_slice(msg.as_slice())
                        .map_err(|error| HarnessError::Runtime(error.to_string()))?;
                    match message {
                        ConflictLabExecuteMsg::Credit { account, .. }
                        | ConflictLabExecuteMsg::ReceiveTransfer { account, .. }
                        | ConflictLabExecuteMsg::ConditionalCredit { account, .. } => {
                            add_query(
                                contract.clone(),
                                to_json_binary(&ConflictLabQueryMsg::Balance { account })
                                    .map_err(|error| HarnessError::Runtime(error.to_string()))?,
                            );
                        }
                        ConflictLabExecuteMsg::Transfer { from, to, .. } => {
                            for account in [from, to] {
                                add_query(
                                    contract.clone(),
                                    to_json_binary(&ConflictLabQueryMsg::Balance { account })
                                        .map_err(|error| {
                                            HarnessError::Runtime(error.to_string())
                                        })?,
                                );
                            }
                        }
                        ConflictLabExecuteMsg::Approve { owner, spender, .. } => {
                            add_query(
                                contract.clone(),
                                to_json_binary(&ConflictLabQueryMsg::Allowance { owner, spender })
                                    .map_err(|error| HarnessError::Runtime(error.to_string()))?,
                            );
                        }
                        ConflictLabExecuteMsg::TransferFrom {
                            owner, spender, to, ..
                        } => {
                            add_query(
                                contract.clone(),
                                to_json_binary(&ConflictLabQueryMsg::Allowance {
                                    owner: owner.clone(),
                                    spender,
                                })
                                .map_err(|error| HarnessError::Runtime(error.to_string()))?,
                            );
                            for account in [owner, to] {
                                add_query(
                                    contract.clone(),
                                    to_json_binary(&ConflictLabQueryMsg::Balance { account })
                                        .map_err(|error| {
                                            HarnessError::Runtime(error.to_string())
                                        })?,
                                );
                            }
                        }
                        ConflictLabExecuteMsg::IncrementCounter { shard_id } => {
                            add_query(
                                contract.clone(),
                                to_json_binary(&ConflictLabQueryMsg::Counter { shard_id })
                                    .map_err(|error| HarnessError::Runtime(error.to_string()))?,
                            );
                        }
                        ConflictLabExecuteMsg::SetFee { .. }
                        | ConflictLabExecuteMsg::SetEpoch { .. } => {
                            add_query(
                                contract.clone(),
                                to_json_binary(&ConflictLabQueryMsg::Config {})
                                    .map_err(|error| HarnessError::Runtime(error.to_string()))?,
                            );
                        }
                        ConflictLabExecuteMsg::CreateOrder {
                            order_id, owner, ..
                        } => {
                            add_query(
                                contract.clone(),
                                to_json_binary(&ConflictLabQueryMsg::Order { order_id })
                                    .map_err(|error| HarnessError::Runtime(error.to_string()))?,
                            );
                            add_query(
                                contract.clone(),
                                to_json_binary(&ConflictLabQueryMsg::Balance { account: owner })
                                    .map_err(|error| HarnessError::Runtime(error.to_string()))?,
                            );
                        }
                        ConflictLabExecuteMsg::CancelOrder { order_id } => {
                            add_query(
                                contract.clone(),
                                to_json_binary(&ConflictLabQueryMsg::Order { order_id })
                                    .map_err(|error| HarnessError::Runtime(error.to_string()))?,
                            );
                        }
                        ConflictLabExecuteMsg::ObserveBankBalance { shard_id, .. }
                        | ConflictLabExecuteMsg::ObserveAllBankBalances { shard_id, .. } => {
                            add_query(
                                contract.clone(),
                                to_json_binary(&ConflictLabQueryMsg::Counter { shard_id })
                                    .map_err(|error| HarnessError::Runtime(error.to_string()))?,
                            );
                        }
                        ConflictLabExecuteMsg::ResetAllBalances {} => {}
                    }
                }
                ExecutionRequest::Execute { .. } | ExecutionRequest::Bundle { .. } => {}
            }
        }
    }
    Ok(queries)
}

struct PreparedConflictLab {
    engine: CosmWasmEngine,
    graph: ProfileGraph,
    contract: Address,
    accounts: u64,
    canonical_queries: Vec<CanonicalQuery>,
    bank_balance_addresses: Vec<Address>,
    environment: BTreeMap<String, String>,
    warmup_blocks: Vec<ProducedBlock>,
    warmup_decided_blocks: Vec<ProducedBlock>,
    measured_block: ProducedBlock,
    measured_decided_block: ProducedBlock,
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

    fn warmup_decided_blocks(&self) -> &[ProducedBlock] {
        &self.warmup_decided_blocks
    }

    fn measured_block(&self) -> &ProducedBlock {
        &self.measured_block
    }

    fn measured_decided_block(&self) -> &ProducedBlock {
        &self.measured_decided_block
    }

    fn environment_metadata(&self) -> BTreeMap<String, String> {
        self.environment.clone()
    }

    fn canonical_state_bytes(&self) -> Result<Vec<u8>, HarnessError> {
        let mut bytes = Vec::new();
        append_bytes(&mut bytes, self.contract.as_str().as_bytes());
        bytes.extend_from_slice(&self.accounts.to_be_bytes());
        for query in &self.canonical_queries {
            append_bytes(&mut bytes, query.contract.as_str().as_bytes());
            if self.engine.contract_metadata(&query.contract).is_some() {
                bytes.push(1);
                let outcome = self
                    .engine
                    .query(
                        BlockContext::default(),
                        query.contract.clone(),
                        query.msg.clone(),
                    )
                    .map_err(|error| HarnessError::Runtime(error.to_string()))?;
                append_bytes(&mut bytes, outcome.data.as_slice());
            } else {
                bytes.push(0);
            }
        }
        for address in &self.bank_balance_addresses {
            append_bytes(&mut bytes, address.as_str().as_bytes());
            bytes.extend_from_slice(
                &self
                    .engine
                    .balance(address.clone(), "uconflict")
                    .to_be_bytes(),
            );
        }
        Ok(bytes)
    }

    fn canonical_state_diagnostics(&self) -> Result<Option<serde_json::Value>, HarnessError> {
        let mut queries = Vec::with_capacity(self.canonical_queries.len());
        for (index, query) in self.canonical_queries.iter().enumerate() {
            let request = serde_json::from_slice::<serde_json::Value>(query.msg.as_slice())
                .unwrap_or_else(
                    |_| serde_json::json!({"raw_hex": bytes_to_hex(query.msg.as_slice())}),
                );
            let present = self.engine.contract_metadata(&query.contract).is_some();
            let response = if present {
                let outcome = self
                    .engine
                    .query(
                        BlockContext::default(),
                        query.contract.clone(),
                        query.msg.clone(),
                    )
                    .map_err(|error| HarnessError::Runtime(error.to_string()))?;
                serde_json::from_slice::<serde_json::Value>(outcome.data.as_slice()).unwrap_or_else(
                    |_| serde_json::json!({"raw_hex": bytes_to_hex(outcome.data.as_slice())}),
                )
            } else {
                serde_json::Value::Null
            };
            queries.push(serde_json::json!({
                "index": index,
                "contract": query.contract.as_str(),
                "contract_present": present,
                "request": request,
                "response": response,
            }));
        }

        let bank_balances = self
            .bank_balance_addresses
            .iter()
            .map(|address| {
                serde_json::json!({
                    "address": address.as_str(),
                    "denom": "uconflict",
                    "amount": self.engine.balance(address.clone(), "uconflict").to_string(),
                })
            })
            .collect::<Vec<_>>();

        Ok(Some(serde_json::json!({
            "contract": self.contract.as_str(),
            "accounts": self.accounts,
            "queries": queries,
            "bank_balances": bank_balances,
        })))
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
    Transfer {
        from: String,
        to: String,
        amount: Uint128,
    },
    Approve {
        owner: String,
        spender: String,
        amount: Uint128,
    },
    TransferFrom {
        owner: String,
        spender: String,
        to: String,
        amount: Uint128,
    },
    IncrementCounter {
        shard_id: u64,
    },
    ConditionalCredit {
        account: String,
        expected_epoch: u64,
        amount: Uint128,
    },
    SetFee {
        new_fee_bps: u16,
    },
    SetEpoch {
        new_epoch: u64,
    },
    ReceiveTransfer {
        account: String,
        amount: Uint128,
    },
    CreateOrder {
        order_id: u64,
        owner: String,
        amount: Uint128,
    },
    CancelOrder {
        order_id: u64,
    },
    ResetAllBalances {},
    ObserveBankBalance {
        account: String,
        denom: String,
        shard_id: u64,
    },
    ObserveAllBankBalances {
        account: String,
        shard_id: u64,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ConflictLabQueryMsg {
    Config {},
    Balance { account: String },
    Allowance { owner: String, spender: String },
    Counter { shard_id: u64 },
    Order { order_id: u64 },
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
        let message: ConflictLabExecuteMsg =
            serde_json::from_slice(msg.as_slice()).map_err(|error| error.to_string())?;
        let ConflictLabExecuteMsg::Credit {
            account,
            amount,
            work_iterations,
            storage_rounds,
            payload,
        } = message
        else {
            return Err("native ConflictLab runtime supports only credit operation_mix".to_owned());
        };
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
            _ => Err("native ConflictLab runtime supports only balance queries".to_owned()),
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
    // Intended to represent cheap transfer/storage-heavy transactions: tens of microseconds on
    // the reference host once the Wasm instance is warm. Exact service time is reported per run.
    WorkShape {
        work_iterations: 8_192,
        storage_rounds: 1,
        payload_bytes: 96,
    }
}

fn medium_work_shape() -> WorkShape {
    // Mid-range contract/application transaction. The benchmark reports measured service time
    // rather than claiming this synthetic tier maps to one particular chain.
    WorkShape {
        work_iterations: 131_072,
        storage_rounds: 3,
        payload_bytes: 512,
    }
}

fn heavy_work_shape() -> WorkShape {
    // Expensive application/contract transaction, deliberately large enough that B512 can cross
    // the 250 ms consensus cutoff on serialized/high-contention schedules.
    WorkShape {
        work_iterations: 786_432,
        storage_rounds: 6,
        payload_bytes: 2_048,
    }
}

#[derive(Clone, Copy)]
struct GenerateBlockConfig {
    offered_transactions: usize,
    first_transaction_id: u64,
    height: u64,
    selection_seed: u64,
    code_id: CodeId,
    prediction_quality: PredictionQuality,
    prediction_buckets: u16,
    prediction_fault_mode: PredictionFaultMode,
    prediction_fault_rate_bps: u16,
    operation_mix: OperationMix,
    parallelism_lanes: u64,
    work: WorkShape,
    complexity_mix: ComplexityMix,
    simulation: SimulationConfig,
}

fn generate_block(
    contract: &Address,
    generator: &mut ConflictLabGenerator,
    config: GenerateBlockConfig,
) -> Result<ProducedBlock, HarnessError> {
    let mempool = Mempool::default();
    let mut ingress = RateControlledIngress::new(
        IngressConfig {
            transactions_per_second: config.simulation.admission_tps,
        },
        0,
    )
    .map_err(|error| HarnessError::Runtime(error.to_string()))?;

    for offset in 0..config.offered_transactions {
        let transaction_id = config
            .first_transaction_id
            .saturating_add(u64::try_from(offset).map_err(|_| HarnessError::NumericOverflow)?);
        let request = generate_request(contract, generator, config, offset, transaction_id)?;
        ingress.enqueue(request);
    }

    let block_interval = Duration::from_millis(config.simulation.block_interval_ms);
    let interval_nanos = block_interval.as_nanos().min(u128::from(u64::MAX)) as u64;
    ingress.pump_until(interval_nanos, &mempool);
    if mempool.is_empty() {
        return Err(HarnessError::WorkloadParameter(format!(
            "sim.admission_tps={} and sim.block_interval_ms={} admit no transactions in one block window",
            config.simulation.admission_tps, config.simulation.block_interval_ms
        )));
    }
    let producer_config = BlockProducerConfig {
        block_interval,
        first_block_height: config.height,
        first_block_time_nanos: interval_nanos,
        max_transactions_per_block: Some(config.simulation.block_size),
        ..BlockProducerConfig::default()
    };
    let block = match config.simulation.mempool_policy {
        MempoolPolicy::Fifo => BlockProducer::fifo(producer_config)
            .map_err(|error| HarnessError::Runtime(error.to_string()))?
            .produce_next(&mempool),
        MempoolPolicy::ReverseFifo => BlockProducer::reverse_fifo(producer_config)
            .map_err(|error| HarnessError::Runtime(error.to_string()))?
            .produce_next(&mempool),
        MempoolPolicy::SeededShuffle => {
            { BlockProducer::seeded_shuffle(producer_config, config.selection_seed) }
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

fn controlled_parallelism_account(
    offset: usize,
    parallelism_lanes: u64,
) -> Result<Option<String>, HarnessError> {
    if parallelism_lanes == 0 {
        return Ok(None);
    }
    let offset = u64::try_from(offset).map_err(|_| HarnessError::NumericOverflow)?;
    Ok(Some(format!("account-{}", offset % parallelism_lanes)))
}

fn generate_request(
    contract: &Address,
    generator: &mut ConflictLabGenerator,
    config: GenerateBlockConfig,
    offset: usize,
    transaction_id: u64,
) -> Result<ExecutionRequest, HarnessError> {
    let actual_account = controlled_parallelism_account(offset, config.parallelism_lanes)?
        .unwrap_or_else(|| generator.next_account());
    let mut selector_rng = SplitMix64::new(transaction_id ^ config.selection_seed);
    let selector = selector_rng.next_u64();
    let transaction_work = config
        .complexity_mix
        .work_shape(config.work, selector_rng.next_u64());
    let base_payload = deterministic_payload(
        transaction_work.payload_bytes,
        transaction_id ^ config.selection_seed,
    );

    let credit = |sender: Address, funds: Vec<Coin>| -> Result<ExecutionRequest, HarnessError> {
        let (account, payload) = predicted_credit_binding(
            &actual_account,
            base_payload.clone(),
            transaction_id,
            PredictionBindingConfig {
                quality: config.prediction_quality,
                buckets: config.prediction_buckets,
                fault_mode: config.prediction_fault_mode,
                fault_rate_bps: config.prediction_fault_rate_bps,
                selection_seed: config.selection_seed,
            },
        );
        Ok(ExecutionRequest::Execute {
            transaction_id: TransactionId(transaction_id),
            sender,
            contract: contract.clone(),
            funds,
            msg: to_json_binary(&ConflictLabExecuteMsg::Credit {
                account,
                amount: Uint128::new(1),
                work_iterations: transaction_work.work_iterations,
                storage_rounds: transaction_work.storage_rounds,
                payload: Binary::from(payload),
            })
            .map_err(|error| HarnessError::Runtime(error.to_string()))?,
        })
    };

    if config.operation_mix == OperationMix::Instantiate {
        return Ok(ExecutionRequest::Instantiate {
            transaction_id: TransactionId(transaction_id),
            sender: Address::new("creator"),
            code_id: config.code_id,
            admin: None,
            label: format!("conflictlab-{transaction_id}"),
            funds: Vec::new(),
            msg: to_json_binary(&ConflictLabInstantiateMsg {
                admin: None,
                fee_bps: u16::try_from(transaction_id % 100).unwrap_or(0),
                epoch: config.height,
            })
            .map_err(|error| HarnessError::Runtime(error.to_string()))?,
        });
    }

    if config.operation_mix == OperationMix::BankFunds {
        return credit(
            Address::new(actual_account.clone()),
            vec![Coin::new(1_u128, "uconflict")],
        );
    }

    if config.operation_mix == OperationMix::BankMixed {
        if offset % 2 == 0 {
            return credit(
                Address::new(actual_account.clone()),
                vec![Coin::new(1_u128, "uconflict")],
            );
        }
        let shard_id = selector % generator.accounts;
        let message = if offset % 4 == 1 {
            ConflictLabExecuteMsg::ObserveBankBalance {
                account: actual_account,
                denom: "uconflict".to_owned(),
                shard_id,
            }
        } else {
            ConflictLabExecuteMsg::ObserveAllBankBalances {
                account: actual_account,
                shard_id,
            }
        };
        return execute_request(transaction_id, "observer", contract, message);
    }

    let request = match config.operation_mix {
        OperationMix::Credit => return credit(Address::new("client"), Vec::new()),
        OperationMix::PointMixed => match selector % 100 {
            0..=54 => return credit(Address::new("client"), Vec::new()),
            55..=69 => execute_request(
                transaction_id,
                "client",
                contract,
                ConflictLabExecuteMsg::IncrementCounter {
                    shard_id: selector % generator.accounts,
                },
            )?,
            70..=81 => execute_request(
                transaction_id,
                "client",
                contract,
                ConflictLabExecuteMsg::ConditionalCredit {
                    account: actual_account.clone(),
                    expected_epoch: 0,
                    amount: Uint128::new(1),
                },
            )?,
            82..=91 => execute_request(
                transaction_id,
                &actual_account,
                contract,
                ConflictLabExecuteMsg::Approve {
                    owner: actual_account.clone(),
                    spender: "spender".to_owned(),
                    amount: Uint128::new(100),
                },
            )?,
            _ => execute_request(
                transaction_id,
                "client",
                contract,
                ConflictLabExecuteMsg::ReceiveTransfer {
                    account: actual_account.clone(),
                    amount: Uint128::new(1),
                },
            )?,
        },
        OperationMix::StatefulMixed | OperationMix::Full => {
            let phase = offset % 32;
            let pair_owner = format!("account-{}", (offset / 32) as u64 % generator.accounts);
            let pair_to = next_account_name(&pair_owner, generator.accounts, selector);
            match phase {
                24 => execute_request(
                    transaction_id,
                    "client",
                    contract,
                    ConflictLabExecuteMsg::CreateOrder {
                        order_id: transaction_id,
                        owner: pair_owner,
                        amount: Uint128::new(1),
                    },
                )?,
                25 => execute_request(
                    transaction_id,
                    &pair_owner,
                    contract,
                    ConflictLabExecuteMsg::CancelOrder {
                        order_id: transaction_id.saturating_sub(1),
                    },
                )?,
                26 => execute_request(
                    transaction_id,
                    &pair_owner,
                    contract,
                    ConflictLabExecuteMsg::Approve {
                        owner: pair_owner.clone(),
                        spender: "spender".to_owned(),
                        amount: Uint128::new(100),
                    },
                )?,
                27 => execute_request(
                    transaction_id,
                    "spender",
                    contract,
                    ConflictLabExecuteMsg::TransferFrom {
                        owner: pair_owner,
                        spender: "spender".to_owned(),
                        to: pair_to,
                        amount: Uint128::new(1),
                    },
                )?,
                28 => execute_request(
                    transaction_id,
                    "creator",
                    contract,
                    ConflictLabExecuteMsg::SetFee {
                        new_fee_bps: u16::try_from(selector % 1_000).unwrap_or(0),
                    },
                )?,
                29 if config.operation_mix == OperationMix::Full => execute_request(
                    transaction_id,
                    "creator",
                    contract,
                    ConflictLabExecuteMsg::SetEpoch { new_epoch: 0 },
                )?,
                30 => execute_request(
                    transaction_id,
                    "client",
                    contract,
                    ConflictLabExecuteMsg::ReceiveTransfer {
                        account: actual_account.clone(),
                        amount: Uint128::new(1),
                    },
                )?,
                _ => match selector % 100 {
                    0..=39 => return credit(Address::new("client"), Vec::new()),
                    40..=64 => {
                        let to = next_account_name(&actual_account, generator.accounts, selector);
                        execute_request(
                            transaction_id,
                            &actual_account,
                            contract,
                            ConflictLabExecuteMsg::Transfer {
                                from: actual_account.clone(),
                                to,
                                amount: Uint128::new(1),
                            },
                        )?
                    }
                    65..=79 => execute_request(
                        transaction_id,
                        "client",
                        contract,
                        ConflictLabExecuteMsg::IncrementCounter {
                            shard_id: selector % generator.accounts,
                        },
                    )?,
                    80..=91 => execute_request(
                        transaction_id,
                        "client",
                        contract,
                        ConflictLabExecuteMsg::ConditionalCredit {
                            account: actual_account.clone(),
                            expected_epoch: 0,
                            amount: Uint128::new(1),
                        },
                    )?,
                    _ => return credit(Address::new("client"), Vec::new()),
                },
            }
        }
        OperationMix::RangeDelete => {
            if offset == config.offered_transactions / 2 {
                execute_request(
                    transaction_id,
                    "creator",
                    contract,
                    ConflictLabExecuteMsg::ResetAllBalances {},
                )?
            } else {
                return credit(Address::new("client"), Vec::new());
            }
        }
        OperationMix::BankFunds | OperationMix::BankMixed | OperationMix::Instantiate => {
            unreachable!()
        }
    };
    Ok(request)
}

fn execute_request(
    transaction_id: u64,
    sender: &str,
    contract: &Address,
    message: ConflictLabExecuteMsg,
) -> Result<ExecutionRequest, HarnessError> {
    Ok(ExecutionRequest::Execute {
        transaction_id: TransactionId(transaction_id),
        sender: Address::new(sender),
        contract: contract.clone(),
        funds: Vec::new(),
        msg: to_json_binary(&message).map_err(|error| HarnessError::Runtime(error.to_string()))?,
    })
}

fn next_account_name(account: &str, accounts: u64, selector: u64) -> String {
    if accounts <= 1 {
        return account.to_owned();
    }
    let mut account_id = selector.rotate_left(17) % accounts;
    let candidate = format!("account-{account_id}");
    if candidate == account {
        account_id = (account_id + 1) % accounts;
    }
    format!("account-{account_id}")
}

#[derive(Clone, Copy, Debug)]
struct PredictionBindingConfig {
    quality: PredictionQuality,
    buckets: u16,
    fault_mode: PredictionFaultMode,
    fault_rate_bps: u16,
    selection_seed: u64,
}

fn predicted_credit_binding(
    actual_account: &str,
    payload: Vec<u8>,
    transaction_id: u64,
    config: PredictionBindingConfig,
) -> (String, Vec<u8>) {
    // Hidden-key faults are transaction-local so they deliberately split true equivalence
    // classes and exercise false-negative recovery. Spurious-key faults are selected by the
    // actual logical key (stable within a block), so every occurrence of the same real key is
    // coarsened consistently: this creates false positives without also hiding true conflicts.
    let fault_sample = match config.fault_mode {
        PredictionFaultMode::SpuriousKey => {
            (u64::from(prediction_bucket(actual_account, BASIS_POINTS))
                + config.selection_seed % u64::from(BASIS_POINTS))
                % u64::from(BASIS_POINTS)
        }
        PredictionFaultMode::None | PredictionFaultMode::HiddenKey => {
            SplitMix64::new(transaction_id ^ config.selection_seed ^ 0xF017_FA17).next_u64()
                % u64::from(BASIS_POINTS)
        }
    };
    if fault_sample < u64::from(config.fault_rate_bps) {
        return match config.fault_mode {
            PredictionFaultMode::None => (actual_account.to_owned(), payload),
            PredictionFaultMode::HiddenKey => (
                format!("fault-hidden-{transaction_id}"),
                opaque_payload(actual_account, payload),
            ),
            PredictionFaultMode::SpuriousKey => (
                "fault-spurious-hot".to_owned(),
                opaque_payload(actual_account, payload),
            ),
        };
    }

    match config.quality {
        PredictionQuality::Exact | PredictionQuality::Coarse => {
            (actual_account.to_owned(), payload)
        }
        PredictionQuality::Bucketed => {
            let bucket = prediction_bucket(actual_account, config.buckets);
            (
                format!("bucket-{bucket}"),
                bucketed_payload(actual_account, payload),
            )
        }
        PredictionQuality::Opaque => (
            format!("prediction-{transaction_id}"),
            opaque_payload(actual_account, payload),
        ),
    }
}

fn alternate_transaction_base(first_transaction_id: u64, height: u64) -> u64 {
    1_000_000_000_u64
        .saturating_add(height.saturating_mul(1_000_000))
        .saturating_add(first_transaction_id)
}

fn divergence_count(len: usize, percent: usize) -> usize {
    if len == 0 || percent == 0 {
        0
    } else {
        len.saturating_mul(percent).saturating_add(99) / 100
    }
}

fn apply_consensus_divergence(
    predicted: &ProducedBlock,
    alternate: &ProducedBlock,
    divergence: ConsensusDivergence,
) -> Result<ProducedBlock, HarnessError> {
    if predicted.transactions.len() != alternate.transactions.len() {
        return Err(HarnessError::Runtime(
            "candidate and alternate block sizes differ".to_owned(),
        ));
    }
    let mut decided = predicted.clone();
    let len = decided.transactions.len();
    let (replace_percent, reorder_percent) = match divergence {
        ConsensusDivergence::Identical => (0, 0),
        ConsensusDivergence::Tail5 => (5, 0),
        ConsensusDivergence::Tail20 => (20, 0),
        ConsensusDivergence::Reorder5 => (0, 5),
        ConsensusDivergence::Reorder20 => (0, 20),
        ConsensusDivergence::TailReorder10 => (10, 10),
    };
    let replace_count = divergence_count(len, replace_percent).min(len);
    if replace_count > 0 {
        let start = len - replace_count;
        decided.transactions[start..].clone_from_slice(&alternate.transactions[start..]);
    }
    let reorder_count = divergence_count(len, reorder_percent).min(len);
    if reorder_count > 1 {
        decided.transactions[len - reorder_count..].reverse();
    }
    Ok(decided)
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

fn bytes_to_hex(value: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(value.len().saturating_mul(2));
    for byte in value {
        output.push(char::from(HEX[usize::from(*byte >> 4)]));
        output.push(char::from(HEX[usize::from(*byte & 0x0f)]));
    }
    output
}

fn append_bytes(output: &mut Vec<u8>, value: &[u8]) {
    output.extend_from_slice(&u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
    output.extend_from_slice(value);
}

#[cfg(test)]
mod v1_evaluation_tests {
    use super::*;

    #[test]
    fn resource_granularity_compiles_to_whole_resource_predicates() {
        let bytes =
            conflictlab_symbolic(PredictionQuality::Exact, SymbolicGranularity::Resource).unwrap();
        let document = parse_slice(&bytes).unwrap();
        let context = IngestionContext::new(
            RuntimeId::new("cosmwasm").unwrap(),
            ContractCodeHash([9; 32]),
            1,
        );
        let profiles = normalize_document(document, &context).unwrap();

        let accesses = profiles
            .iter()
            .flat_map(|profile| profile.accesses.iter())
            .collect::<Vec<_>>();
        assert!(!accesses.is_empty());
        assert!(accesses.iter().all(|access| {
            access.semantic_key_kind == acg_core::SemanticKeyKind::FieldSet
                && access.key_dependency.is_none()
        }));

        let artifact =
            ProfileGraphArtifact::compile(profiles, &EdgeBuildConfig::default()).unwrap();
        let graph = ProfileGraph::load(artifact, GraphLoadConfig::default()).unwrap();
        assert!(!graph.edges().is_empty());
        assert!(graph.edges().iter().all(|edge| {
            !edge.predicate.clauses.is_empty()
                && edge
                    .predicate
                    .clauses
                    .iter()
                    .all(|clause| matches!(&clause.key_match, acg_core::KeyMatch::WholeResource))
        }));
    }

    #[test]
    fn symbolic_granularity_ablation_keeps_documents_normalizable() {
        for granularity in [
            SymbolicGranularity::Fine,
            SymbolicGranularity::Resource,
            SymbolicGranularity::Profile,
        ] {
            let bytes = conflictlab_symbolic(PredictionQuality::Exact, granularity).unwrap();
            let document = parse_slice(&bytes).unwrap();
            let context = IngestionContext::new(
                RuntimeId::new("cosmwasm").unwrap(),
                ContractCodeHash([7; 32]),
                1,
            );
            let profiles = normalize_document(document, &context).unwrap();
            let artifact =
                ProfileGraphArtifact::compile(profiles, &EdgeBuildConfig::default()).unwrap();
            let graph = ProfileGraph::load(artifact, GraphLoadConfig::default()).unwrap();
            assert!(!graph.profiles().is_empty());
            assert!(!graph.edges().is_empty());
        }
    }

    #[test]
    fn controlled_prediction_faults_change_prediction_without_changing_runtime_key() {
        let actual = "account-7";
        let payload = vec![1, 2, 3, 4];
        let (hidden_prediction, hidden_payload) = predicted_credit_binding(
            actual,
            payload.clone(),
            9,
            PredictionBindingConfig {
                quality: PredictionQuality::Exact,
                buckets: 8,
                fault_mode: PredictionFaultMode::HiddenKey,
                fault_rate_bps: BASIS_POINTS,
                selection_seed: 10,
            },
        );
        assert_ne!(hidden_prediction, actual);
        assert_eq!(opaque_account_from_payload(&hidden_payload), Some(actual));

        let (spurious_prediction, spurious_payload) = predicted_credit_binding(
            actual,
            payload,
            11,
            PredictionBindingConfig {
                quality: PredictionQuality::Exact,
                buckets: 8,
                fault_mode: PredictionFaultMode::SpuriousKey,
                fault_rate_bps: BASIS_POINTS,
                selection_seed: 12,
            },
        );
        assert_eq!(spurious_prediction, "fault-spurious-hot");
        assert_eq!(opaque_account_from_payload(&spurious_payload), Some(actual));

        let selected = (0..100)
            .map(|index| format!("account-{index}"))
            .find(|account| {
                predicted_credit_binding(
                    account,
                    Vec::new(),
                    1,
                    PredictionBindingConfig {
                        quality: PredictionQuality::Exact,
                        buckets: 8,
                        fault_mode: PredictionFaultMode::SpuriousKey,
                        fault_rate_bps: 5_000,
                        selection_seed: 12,
                    },
                )
                .0 == "fault-spurious-hot"
            })
            .expect("half-rate spurious fault selects at least one account");
        let first = predicted_credit_binding(
            &selected,
            Vec::new(),
            1,
            PredictionBindingConfig {
                quality: PredictionQuality::Exact,
                buckets: 8,
                fault_mode: PredictionFaultMode::SpuriousKey,
                fault_rate_bps: 5_000,
                selection_seed: 12,
            },
        );
        let second = predicted_credit_binding(
            &selected,
            Vec::new(),
            999_999,
            PredictionBindingConfig {
                quality: PredictionQuality::Exact,
                buckets: 8,
                fault_mode: PredictionFaultMode::SpuriousKey,
                fault_rate_bps: 5_000,
                selection_seed: 12,
            },
        );
        assert_eq!(first.0, second.0);
        assert_eq!(
            opaque_account_from_payload(&first.1),
            Some(selected.as_str())
        );
        assert_eq!(
            opaque_account_from_payload(&second.1),
            Some(selected.as_str())
        );
    }

    #[test]
    fn transient_prediction_fault_applies_only_to_configured_regime_blocks() {
        assert_eq!(
            measured_regime_fault(PredictionFaultMode::HiddenKey, 1_000, 1, 0),
            (PredictionFaultMode::HiddenKey, 1_000)
        );
        assert_eq!(
            measured_regime_fault(PredictionFaultMode::HiddenKey, 1_000, 1, 1),
            (PredictionFaultMode::None, 0)
        );
        assert_eq!(
            measured_regime_fault(PredictionFaultMode::SpuriousKey, 500, 3, 2),
            (PredictionFaultMode::SpuriousKey, 500)
        );
    }

    #[test]
    fn controlled_parallelism_lanes_assign_balanced_conflict_chains() {
        let lanes = 6_u64;
        let accounts = (0..24)
            .map(|offset| {
                controlled_parallelism_account(offset, lanes)
                    .unwrap()
                    .unwrap()
            })
            .collect::<Vec<_>>();
        for lane in 0..lanes {
            assert_eq!(
                accounts
                    .iter()
                    .filter(|account| *account == &format!("account-{lane}"))
                    .count(),
                4
            );
        }
        assert_eq!(controlled_parallelism_account(0, 0).unwrap(), None);
    }

    #[test]
    fn parallelism_lanes_parameter_is_rejected_when_it_cannot_mean_balanced_credit_chains() {
        let mut parameters = BTreeMap::new();
        parameters.insert("transactions".to_owned(), "24".to_owned());
        parameters.insert("accounts".to_owned(), "4".to_owned());
        parameters.insert("parallelism_lanes".to_owned(), "6".to_owned());
        let run = RunIdentity {
            workload: "conflictlab".to_owned(),
            mode: "static".to_owned(),
            run_index: 1,
            seed: 1,
            workers: 6,
            parameters,
        };
        let error = ConflictLabConfig::from_run(&run).unwrap_err();
        assert!(error
            .to_string()
            .contains("parallelism_lanes=6 exceeds accounts=4"));
    }

    #[test]
    fn v1_operation_mix_axis_includes_runtime_semantics_cases() {
        for value in [
            "credit",
            "point-mixed",
            "stateful-mixed",
            "range-delete",
            "bank-funds",
            "bank-mixed",
            "instantiate",
            "full",
        ] {
            assert_eq!(OperationMix::parse(value).unwrap().as_str(), value);
        }
    }
}
