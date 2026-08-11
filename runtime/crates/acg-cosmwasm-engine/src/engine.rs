use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use cosmwasm_std::{
    Addr, Attribute, BankMsg, Binary, BlockInfo, Coin, ContractInfo, ContractResult, CosmosMsg,
    Empty, Env, Event, MessageInfo, MsgResponse, Reply, ReplyOn, Response, SubMsg, SubMsgResponse,
    SubMsgResult, Timestamp, TransactionInfo, WasmMsg,
};
use cosmwasm_vm::{
    call_execute, call_instantiate, call_query, call_reply, Backend, InstanceOptions,
};
use parking_lot::{Condvar, Mutex, RwLock};
use rayon::prelude::*;
use rayon::ThreadPoolBuilder;
use sha2::{Digest, Sha256};

use crate::api::{validate_address, EngineApi};
use crate::cache::{WasmCacheConfig, WasmCacheMetrics, WasmModuleCache};
use crate::error::{EngineError, EngineResult};
use crate::native::{NativeCallContext, NativeContract};
use crate::parallel::{
    ParallelExecutionConfig, ParallelSpeculativeBlockOutcome, ParallelSpeculativeExecutionMetrics,
    PostConsensusTimings, PredictionMatchMetrics, PreparedSpeculativeBlock,
    ReconciliationDependencyEvidence, SpeculativeDependency, SpeculativeDependencyClass,
    SpeculativeWave, SplitPhaseSpeculativeBlockOutcome,
};
use crate::querier::EngineQuerier;
use crate::speculative::{
    CanonicalTransaction, CanonicalTxDisposition, CanonicalTxResult, SpeculativeBlockOutcome,
    SpeculativeExecutionMetrics, SpeculativeExecutionOutcome, SpeculativeExecutionStatus,
    SpeculativeTxResult, StateSnapshot, StateWriteSet,
};
use crate::state::{code_id_of, SharedTx, SharedWorld, TransactionState, WorldState};
use crate::storage::EngineStorage;
use crate::types::{
    AccessKind, Address, BlockContext, CodeChecksum, CodeId, CodeKind, CodeMetadata,
    ContractMetadata, ExecutionOutcome, ExecutionRequest, QueryOutcome, TransactionId,
};
use crate::validation::{
    apply_write_set, validate_dependencies, write_set_touches_conflict, ValidationOutcome,
};

const EXECUTE_RESPONSE_TYPE_URL: &str = "/cosmwasm.wasm.v1.MsgExecuteContractResponse";
const INSTANTIATE_RESPONSE_TYPE_URL: &str = "/cosmwasm.wasm.v1.MsgInstantiateContractResponse";
const BANK_SEND_RESPONSE_TYPE_URL: &str = "/cosmos.bank.v1beta1.MsgSendResponse";
const BANK_BURN_RESPONSE_TYPE_URL: &str = "/cosmos.bank.v1beta1.MsgBurnResponse";

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub gas_limit: u64,
    pub max_call_depth: u32,
    pub contract_address_prefix: String,
    pub wasm_cache: WasmCacheConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            gas_limit: 10_000_000_000_000,
            max_call_depth: 32,
            contract_address_prefix: "contract".to_owned(),
            wasm_cache: WasmCacheConfig::default(),
        }
    }
}

#[derive(Clone)]
pub(crate) enum CodeArtifact {
    Wasm(cosmwasm_std::Checksum),
    Native(Arc<dyn NativeContract>),
}

#[derive(Clone)]
pub(crate) struct CodeRecord {
    pub metadata: CodeMetadata,
    pub artifact: CodeArtifact,
}

pub(crate) struct EngineCore {
    pub config: EngineConfig,
    pub state: SharedWorld,
    engine_identity: Arc<()>,
    codes: RwLock<BTreeMap<CodeId, CodeRecord>>,
    wasm_cache: WasmModuleCache,
    next_code_id: AtomicU64,
}

#[derive(Clone)]
pub struct CosmWasmEngine {
    core: Arc<EngineCore>,
}

impl CosmWasmEngine {
    pub fn try_new(config: EngineConfig) -> EngineResult<Self> {
        let wasm_cache = WasmModuleCache::new(&config.wasm_cache)?;
        Ok(Self {
            core: Arc::new(EngineCore {
                config,
                state: Arc::new(RwLock::new(WorldState::default())),
                engine_identity: Arc::new(()),
                codes: RwLock::new(BTreeMap::new()),
                wasm_cache,
                next_code_id: AtomicU64::new(1),
            }),
        })
    }

    pub fn new(config: EngineConfig) -> Self {
        Self::try_new(config).expect("failed to initialize CosmWasm engine")
    }

    pub fn upload_wasm(&self, wasm: Vec<u8>) -> EngineResult<CodeId> {
        if wasm.is_empty() {
            return Err(EngineError::Vm(
                "Wasm bytecode must not be empty".to_owned(),
            ));
        }
        let vm_checksum = self.core.wasm_cache.save_wasm(&wasm)?;
        let checksum = checksum_from_vm(vm_checksum);
        let code_id = CodeId(self.core.next_code_id.fetch_add(1, Ordering::Relaxed));
        let metadata = CodeMetadata {
            code_id,
            checksum,
            kind: CodeKind::Wasm,
            label: format!("wasm:{}", checksum.to_hex()),
        };
        self.core.codes.write().insert(
            code_id,
            CodeRecord {
                metadata,
                artifact: CodeArtifact::Wasm(vm_checksum),
            },
        );
        Ok(code_id)
    }

    pub fn wasm_cache_metrics(&self) -> WasmCacheMetrics {
        self.core.wasm_cache.metrics()
    }

    pub fn wasm_cache_dir(&self) -> &std::path::Path {
        self.core.wasm_cache.base_dir()
    }

    pub fn pin_wasm(&self, code_id: CodeId) -> EngineResult<()> {
        match self.code(code_id)?.artifact {
            CodeArtifact::Wasm(checksum) => self.core.wasm_cache.pin(&checksum),
            CodeArtifact::Native(_) => Ok(()),
        }
    }

    pub fn unpin_wasm(&self, code_id: CodeId) -> EngineResult<()> {
        match self.code(code_id)?.artifact {
            CodeArtifact::Wasm(checksum) => self.core.wasm_cache.unpin(&checksum),
            CodeArtifact::Native(_) => Ok(()),
        }
    }

    pub fn register_native(
        &self,
        label: impl Into<String>,
        contract: Arc<dyn NativeContract>,
    ) -> EngineResult<CodeId> {
        let label = label.into();
        validate_code_label(&label)?;
        let checksum = checksum_native_label(&label);
        let code_id = CodeId(self.core.next_code_id.fetch_add(1, Ordering::Relaxed));
        let metadata = CodeMetadata {
            code_id,
            checksum,
            kind: CodeKind::Native,
            label,
        };
        self.core.codes.write().insert(
            code_id,
            CodeRecord {
                metadata,
                artifact: CodeArtifact::Native(contract),
            },
        );
        Ok(code_id)
    }

    pub fn code_metadata(&self, code_id: CodeId) -> Option<CodeMetadata> {
        self.core
            .codes
            .read()
            .get(&code_id)
            .map(|record| record.metadata.clone())
    }

    pub fn contract_code_metadata(&self, address: &Address) -> Option<CodeMetadata> {
        self.contract_metadata(address)
            .and_then(|metadata| self.code_metadata(metadata.code_id))
    }

    /// Predict the deterministic address allocated for an instantiation ordinal in a transaction.
    ///
    /// The top-level instantiate call uses ordinal 0. Nested instantiations increment the ordinal
    /// transaction-locally. This helper lets pre-execution graph construction assign the same
    /// contract-instance identity that execution will later commit.
    pub fn predict_contract_address(&self, transaction_id: TransactionId, ordinal: u32) -> Address {
        Address::new(format!(
            "{}-{}-{ordinal}",
            self.core.config.contract_address_prefix, transaction_id.0
        ))
    }

    pub fn set_balance(&self, address: impl Into<Address>, coins: &[Coin]) -> EngineResult<()> {
        let address = address.into();
        validate_public_address(&address)?;
        validate_balance_seed(coins)?;
        let mut world = self.core.state.write();
        for coin in coins {
            let key = (address.clone(), coin.denom.clone());
            if coin.amount.is_zero() {
                world.balances.remove(&key);
            } else {
                world.balances.insert(key, coin.amount);
            }
        }
        Ok(())
    }

    pub fn balance(&self, address: impl Into<Address>, denom: &str) -> u128 {
        let address = address.into();
        self.core
            .state
            .read()
            .balances
            .get(&(address, denom.to_owned()))
            .copied()
            .unwrap_or_default()
            .u128()
    }

    pub fn all_balances(&self, address: impl Into<Address>) -> Vec<Coin> {
        let address = address.into();
        self.core
            .state
            .read()
            .balances
            .iter()
            .filter(|((owner, _), amount)| owner == &address && !amount.is_zero())
            .map(|((_, denom), amount)| Coin {
                denom: denom.clone(),
                amount: *amount,
            })
            .collect()
    }

    pub fn contract_metadata(&self, address: &Address) -> Option<ContractMetadata> {
        self.core.state.read().contracts.get(address).cloned()
    }

    pub fn raw_storage(&self, address: &Address, key: &[u8]) -> Option<Vec<u8>> {
        self.core
            .state
            .read()
            .storage
            .get(address)
            .and_then(|storage| storage.get(key).cloned())
    }

    /// Capture a detached, immutable view of the current world state for speculative execution.
    pub fn snapshot(&self) -> StateSnapshot {
        StateSnapshot {
            core: self.core.clone(),
            state: Arc::new(RwLock::new(self.core.state.read().clone())),
        }
    }

    /// Execute one transaction against a detached snapshot without mutating canonical state.
    ///
    /// Contract/runtime failures are represented in `SpeculativeTxResult::status` so validation can
    /// later decide whether the same failure is reusable. The outer `EngineResult` is reserved for
    /// misuse of the speculative API itself, such as passing a snapshot from another engine.
    pub fn execute_speculative(
        &self,
        snapshot: &StateSnapshot,
        block: BlockContext,
        request: ExecutionRequest,
    ) -> EngineResult<SpeculativeTxResult> {
        if !Arc::ptr_eq(&self.core, &snapshot.core) {
            return Err(EngineError::InvalidConfiguration(
                "state snapshot belongs to a different CosmWasm engine".to_owned(),
            ));
        }

        let transaction_id = request.transaction_id();
        let receipt_block = block.clone();
        let receipt_request = request.clone();
        let (result, tx) =
            self.execute_request_on_state(snapshot.state.clone(), block, request, false);
        let state = tx.lock();
        let read_dependencies = state.read_dependencies.clone();
        let write_set = if result.is_ok() {
            state.write_set()
        } else {
            StateWriteSet::default()
        };
        let mut accesses = state.accesses.clone();
        drop(state);

        let status = match result {
            Ok(outcome) => SpeculativeExecutionStatus::Succeeded(SpeculativeExecutionOutcome {
                contract: outcome.contract,
                events: outcome.events,
                data: outcome.data,
                created_contracts: outcome.created_contracts,
            }),
            Err(error) => {
                for access in &mut accesses {
                    access.reverted = true;
                }
                SpeculativeExecutionStatus::Failed(error)
            }
        };

        Ok(SpeculativeTxResult {
            transaction_id,
            block: receipt_block,
            request: receipt_request,
            status,
            accesses,
            read_dependencies,
            write_set,
            engine_identity: self.core.engine_identity.clone(),
        })
    }

    /// Validate a detached speculative receipt against the current canonical world state.
    pub fn validate_speculative(
        &self,
        result: &SpeculativeTxResult,
    ) -> EngineResult<ValidationOutcome> {
        self.ensure_receipt_belongs_to_engine(result)?;
        Ok(validate_dependencies(
            &self.core.state,
            &result.read_dependencies,
        ))
    }

    /// Drain a block in canonical order, reusing valid speculative receipts and replaying only
    /// receipts whose recorded dependencies no longer match canonical predecessor state.
    ///
    /// Transaction-level contract/runtime failures are returned inside each `CanonicalTxResult`;
    /// the outer `EngineResult` is reserved for malformed coordinator input or foreign receipts.
    pub fn execute_canonical_with_speculation(
        &self,
        transactions: Vec<CanonicalTransaction>,
        speculative_results: Vec<SpeculativeTxResult>,
    ) -> EngineResult<SpeculativeBlockOutcome> {
        let mut canonical_bindings = BTreeMap::new();
        for transaction in &transactions {
            let transaction_id = transaction.transaction_id();
            if canonical_bindings
                .insert(
                    transaction_id,
                    (transaction.block.clone(), transaction.request.clone()),
                )
                .is_some()
            {
                return Err(EngineError::InvalidConfiguration(format!(
                    "duplicate canonical transaction ID {}",
                    transaction_id.0
                )));
            }
        }

        let mut receipts = BTreeMap::new();
        for result in speculative_results {
            self.ensure_receipt_belongs_to_engine(&result)?;
            let Some((expected_block, expected_request)) =
                canonical_bindings.get(&result.transaction_id)
            else {
                return Err(EngineError::InvalidConfiguration(format!(
                    "speculative receipt references transaction ID {} that is not in the canonical block",
                    result.transaction_id.0
                )));
            };
            if &result.block != expected_block || &result.request != expected_request {
                return Err(EngineError::InvalidConfiguration(format!(
                    "speculative receipt for transaction ID {} was produced from a different block context or request",
                    result.transaction_id.0
                )));
            }
            let transaction_id = result.transaction_id;
            if receipts.insert(transaction_id, result).is_some() {
                return Err(EngineError::InvalidConfiguration(format!(
                    "duplicate speculative receipt for transaction ID {}",
                    transaction_id.0
                )));
            }
        }

        let mut metrics = SpeculativeExecutionMetrics {
            speculative_results: receipts.len() as u64,
            ..SpeculativeExecutionMetrics::default()
        };
        let mut outcomes = Vec::with_capacity(transactions.len());

        for transaction in transactions {
            let transaction_id = transaction.transaction_id();
            let Some(receipt) = receipts.remove(&transaction_id) else {
                metrics.canonical_transactions += 1;
                let result = self.execute_request(transaction.block, transaction.request);
                outcomes.push(CanonicalTxResult {
                    transaction_id,
                    disposition: CanonicalTxDisposition::Canonical,
                    validation: None,
                    result,
                });
                continue;
            };

            let validation = validate_dependencies(&self.core.state, &receipt.read_dependencies);
            if validation.is_valid() {
                metrics.reused_results += 1;
                let result = self.commit_reused_receipt(receipt);
                outcomes.push(CanonicalTxResult {
                    transaction_id,
                    disposition: CanonicalTxDisposition::ReusedSpeculative,
                    validation: Some(validation),
                    result,
                });
            } else {
                metrics.invalidated_results += 1;
                metrics.replayed_transactions += 1;
                let result = self.execute_request(transaction.block, transaction.request);
                outcomes.push(CanonicalTxResult {
                    transaction_id,
                    disposition: CanonicalTxDisposition::Replayed,
                    validation: Some(validation),
                    result,
                });
            }
        }

        debug_assert!(receipts.is_empty());
        Ok(SpeculativeBlockOutcome {
            transactions: outcomes,
            metrics,
        })
    }

    /// Pre-execute a predicted block without mutating canonical state.
    ///
    /// This is the speculative half of the Brick-5C.5 split-phase pipeline. Every wave runs
    /// through a bounded worker pool. As receipts become available, successful write sets are
    /// applied only to a detached provisional snapshot in canonical-prefix order so later waves of
    /// the *same block* can observe predicted predecessor effects. The MiniWarehouse 5C.5 pipeline
    /// invokes this only after the previous block has canonically committed.
    pub fn preexecute_parallel_waves(
        &self,
        config: ParallelExecutionConfig,
        transactions: Vec<CanonicalTransaction>,
        waves: Vec<SpeculativeWave>,
    ) -> EngineResult<PreparedSpeculativeBlock> {
        let base = self.snapshot();
        self.preexecute_parallel_waves_from_snapshot(&base, config, transactions, waves)
    }

    /// Same as [`Self::preexecute_parallel_waves`], but starts from an explicit detached snapshot.
    ///
    /// This explicit-snapshot overload remains useful for deterministic tests and controlled
    /// experiments. The default 5C.5 pipeline does not execute block `N + 1` from block `N`'s
    /// predicted successor; it waits for canonical commit and calls [`Self::preexecute_parallel_waves`].
    pub fn preexecute_parallel_waves_from_snapshot(
        &self,
        base: &StateSnapshot,
        config: ParallelExecutionConfig,
        transactions: Vec<CanonicalTransaction>,
        waves: Vec<SpeculativeWave>,
    ) -> EngineResult<PreparedSpeculativeBlock> {
        if !Arc::ptr_eq(&self.core, &base.core) {
            return Err(EngineError::InvalidConfiguration(
                "state snapshot belongs to a different CosmWasm engine".to_owned(),
            ));
        }
        let canonical_bindings = validate_speculative_wave_plan(config, &transactions, &waves)?;
        let pool = build_speculative_pool(config)?;
        let wave_widths = waves.iter().map(SpeculativeWave::len).collect::<Vec<_>>();
        let predicted_successor = base.fork();
        let mut receipts = BTreeMap::new();
        let mut provisional_cursor = 0_usize;

        for wave in waves {
            let wave_snapshot = predicted_successor.fork();
            let wave_transactions = wave
                .transaction_ids
                .iter()
                .map(|transaction_id| {
                    canonical_bindings
                        .get(transaction_id)
                        .expect("wave plan was validated before execution")
                        .clone()
                })
                .collect::<Vec<_>>();

            let wave_receipts = pool.install(|| {
                wave_transactions
                    .into_par_iter()
                    .map(|transaction| {
                        self.execute_speculative(
                            &wave_snapshot,
                            transaction.block,
                            transaction.request,
                        )
                    })
                    .collect::<Vec<_>>()
            });

            for receipt in wave_receipts {
                let receipt = receipt?;
                let transaction_id = receipt.transaction_id;
                if receipts.insert(transaction_id, receipt).is_some() {
                    return Err(EngineError::Internal(format!(
                        "duplicate speculative receipt for transaction ID {}",
                        transaction_id.0
                    )));
                }
            }

            // Advance only the canonical prefix whose receipts are available. This avoids making
            // a later canonical transaction visible to an earlier one merely because the scheduler
            // placed the later transaction in an earlier speculative wave.
            while provisional_cursor < transactions.len() {
                let transaction_id = transactions[provisional_cursor].transaction_id();
                let Some(receipt) = receipts.get(&transaction_id) else {
                    break;
                };
                if receipt.is_success() {
                    apply_write_set(&predicted_successor.state, &receipt.write_set);
                }
                provisional_cursor += 1;
            }
        }

        if provisional_cursor != transactions.len() || receipts.len() != transactions.len() {
            return Err(EngineError::Internal(
                "validated speculative wave plan did not produce the complete predicted block"
                    .to_owned(),
            ));
        }

        let ordered_receipts = transactions
            .iter()
            .map(|transaction| {
                receipts
                    .remove(&transaction.transaction_id())
                    .expect("every canonical transaction produced one receipt")
            })
            .collect::<Vec<_>>();

        Ok(PreparedSpeculativeBlock {
            predicted_transactions: transactions,
            receipts: ordered_receipts,
            metrics: ParallelSpeculativeExecutionMetrics {
                workers: config.workers,
                wave_widths,
                speculative: SpeculativeExecutionMetrics {
                    speculative_results: canonical_bindings.len() as u64,
                    ..SpeculativeExecutionMetrics::default()
                },
                dependency_count: 0,
                hard_dependency_count: 0,
            },
            predicted_successor,
        })
    }

    /// Pre-execute a predicted block with pairwise dependency-driven readiness and versioned
    /// speculative visibility.
    ///
    /// Unlike the legacy strict-wave path, scheduler waves are only diagnostic levels. A
    /// transaction becomes runnable as soon as every explicit predecessor completes. Each
    /// successful receipt is immediately published as a canonical-index-tagged speculative
    /// version. A transaction at canonical index `i` builds its snapshot from all completed
    /// successful versions with index `< i`, so future-canonical writes can never flow backward
    /// while completed independent predecessors become visible without waiting for a contiguous
    /// global prefix.
    pub fn preexecute_dependency_plan(
        &self,
        config: ParallelExecutionConfig,
        transactions: Vec<CanonicalTransaction>,
        waves: Vec<SpeculativeWave>,
        dependencies: Vec<SpeculativeDependency>,
    ) -> EngineResult<PreparedSpeculativeBlock> {
        let base = self.snapshot();
        self.preexecute_dependency_plan_from_snapshot(
            &base,
            config,
            transactions,
            waves,
            dependencies,
        )
    }

    /// Explicit-snapshot variant of [`Self::preexecute_dependency_plan`].
    pub fn preexecute_dependency_plan_from_snapshot(
        &self,
        base: &StateSnapshot,
        config: ParallelExecutionConfig,
        transactions: Vec<CanonicalTransaction>,
        waves: Vec<SpeculativeWave>,
        dependencies: Vec<SpeculativeDependency>,
    ) -> EngineResult<PreparedSpeculativeBlock> {
        if !Arc::ptr_eq(&self.core, &base.core) {
            return Err(EngineError::InvalidConfiguration(
                "state snapshot belongs to a different CosmWasm engine".to_owned(),
            ));
        }

        let validated =
            validate_speculative_dependency_plan(config, &transactions, &waves, &dependencies)?;
        let wave_widths = waves.iter().map(SpeculativeWave::len).collect::<Vec<_>>();
        let transaction_count = transactions.len();
        if transaction_count == 0 {
            return Ok(PreparedSpeculativeBlock {
                predicted_transactions: transactions,
                receipts: Vec::new(),
                metrics: ParallelSpeculativeExecutionMetrics {
                    workers: config.workers,
                    wave_widths,
                    speculative: SpeculativeExecutionMetrics::default(),
                    dependency_count: dependencies.len(),
                    hard_dependency_count: validated.hard_dependency_count,
                },
                predicted_successor: base.fork(),
            });
        }

        let mut ready = BTreeSet::new();
        for (index, remaining) in validated.remaining_predecessors.iter().enumerate() {
            if *remaining == 0 {
                ready.insert(index);
            }
        }
        let shared = Arc::new((
            Mutex::new(DependencyExecutionState {
                ready,
                remaining_predecessors: validated.remaining_predecessors,
                successors: validated.successors,
                receipts: (0..transaction_count).map(|_| None).collect(),
                completed: 0,
                in_flight: 0,
                failure: None,
            }),
            Condvar::new(),
        ));
        let transactions = Arc::new(transactions);
        let worker_count = config.workers.min(transaction_count).max(1);

        std::thread::scope(|scope| {
            for _ in 0..worker_count {
                let shared = shared.clone();
                let transactions = transactions.clone();
                scope.spawn(move || loop {
                    let transaction_index = {
                        let (state_lock, ready_changed) = &*shared;
                        let mut state = state_lock.lock();
                        loop {
                            if state.failure.is_some() || state.completed == transaction_count {
                                return;
                            }
                            if let Some(index) = state.ready.pop_first() {
                                state.in_flight += 1;
                                break index;
                            }
                            if state.in_flight == 0 {
                                state.failure = Some(
                                    "dependency-driven speculative execution reached a readiness deadlock"
                                        .to_owned(),
                                );
                                ready_changed.notify_all();
                                return;
                            }
                            ready_changed.wait(&mut state);
                        }
                    };

                    // Snapshot all completed earlier-canonical versions at launch time. Hard and
                    // scheduler-separated soft predecessors are guaranteed complete before the
                    // transaction becomes ready; unrelated earlier transactions may still race,
                    // which is intentional speculation and remains protected by concrete replay.
                    let visible_write_sets = {
                        let (state_lock, _) = &*shared;
                        let state = state_lock.lock();
                        state.receipts[..transaction_index]
                            .iter()
                            .filter_map(|receipt| receipt.as_ref())
                            .filter(|receipt| receipt.is_success())
                            .map(|receipt| receipt.write_set.clone())
                            .collect::<Vec<_>>()
                    };
                    let snapshot = base.fork();
                    for write_set in &visible_write_sets {
                        apply_write_set(&snapshot.state, write_set);
                    }

                    let transaction = transactions[transaction_index].clone();
                    let execution = self.execute_speculative(
                        &snapshot,
                        transaction.block,
                        transaction.request,
                    );

                    let (state_lock, ready_changed) = &*shared;
                    let mut state = state_lock.lock();
                    state.in_flight = state.in_flight.saturating_sub(1);
                    match execution {
                        Ok(receipt) => {
                            state.receipts[transaction_index] = Some(receipt);
                            state.completed += 1;
                            let successors = state.successors[transaction_index].clone();
                            for successor in successors {
                                let remaining = &mut state.remaining_predecessors[successor];
                                *remaining = remaining.saturating_sub(1);
                                if *remaining == 0 {
                                    state.ready.insert(successor);
                                }
                            }
                        }
                        Err(error) => {
                            state.failure = Some(format!(
                                "dependency-driven speculative transaction {} failed at API level: {error}",
                                transaction_index
                            ));
                        }
                    }
                    ready_changed.notify_all();
                });
            }
        });

        let (state_lock, _) = &*shared;
        let mut state = state_lock.lock();
        if let Some(failure) = state.failure.take() {
            return Err(EngineError::Internal(failure));
        }
        if state.completed != transaction_count {
            return Err(EngineError::Internal(format!(
                "dependency-driven speculative execution completed {} of {} transactions",
                state.completed, transaction_count
            )));
        }
        let mut ordered_receipts = Vec::with_capacity(transaction_count);
        for (index, receipt) in state.receipts.iter_mut().enumerate() {
            ordered_receipts.push(receipt.take().ok_or_else(|| {
                EngineError::Internal(format!(
                    "dependency-driven speculative execution did not produce receipt {index}"
                ))
            })?);
        }
        drop(state);

        // Advisory complete predicted successor. Applying detached receipts in canonical order
        // preserves deterministic projection even though physical execution completed out of order.
        let predicted_successor = base.fork();
        for receipt in &ordered_receipts {
            if receipt.is_success() {
                apply_write_set(&predicted_successor.state, &receipt.write_set);
            }
        }

        Ok(PreparedSpeculativeBlock {
            predicted_transactions: Arc::try_unwrap(transactions)
                .unwrap_or_else(|transactions| (*transactions).clone()),
            receipts: ordered_receipts,
            metrics: ParallelSpeculativeExecutionMetrics {
                workers: config.workers,
                wave_widths,
                speculative: SpeculativeExecutionMetrics {
                    speculative_results: transaction_count as u64,
                    ..SpeculativeExecutionMetrics::default()
                },
                dependency_count: dependencies.len(),
                hard_dependency_count: validated.hard_dependency_count,
            },
            predicted_successor,
        })
    }

    /// Reconcile a post-consensus canonical block against receipts prepared before consensus.
    ///
    /// Matching receipts are validated strictly in canonical block order. Missing/mismatched
    /// predictions execute canonically, stale receipts replay, and predictions absent from the
    /// decided block are discarded. No predictive graph decision is trusted as a correctness
    /// boundary; only concrete read-dependency validation can authorize reuse.
    pub fn reconcile_prepared_block(
        &self,
        transactions: Vec<CanonicalTransaction>,
        prepared: PreparedSpeculativeBlock,
    ) -> EngineResult<SplitPhaseSpeculativeBlockOutcome> {
        let mut canonical_ids = BTreeSet::new();
        for transaction in &transactions {
            let transaction_id = transaction.transaction_id();
            if !canonical_ids.insert(transaction_id) {
                return Err(EngineError::InvalidConfiguration(format!(
                    "duplicate canonical transaction ID {}",
                    transaction_id.0
                )));
            }
        }

        let predicted_count = prepared.predicted_transactions.len();
        let canonical_order_ids = transactions
            .iter()
            .map(CanonicalTransaction::transaction_id)
            .collect::<Vec<_>>();
        let mut receipts = BTreeMap::new();
        for receipt in prepared.receipts {
            self.ensure_receipt_belongs_to_engine(&receipt)?;
            let transaction_id = receipt.transaction_id;
            if receipts.insert(transaction_id, receipt).is_some() {
                return Err(EngineError::InvalidConfiguration(format!(
                    "duplicate speculative receipt for transaction ID {}",
                    transaction_id.0
                )));
            }
        }

        let mut metrics = SpeculativeExecutionMetrics {
            speculative_results: predicted_count as u64,
            ..SpeculativeExecutionMetrics::default()
        };
        let mut prediction = PredictionMatchMetrics {
            predicted_transactions: predicted_count as u64,
            decided_transactions: transactions.len() as u64,
            ..PredictionMatchMetrics::default()
        };
        let prepared_write_sets = receipts
            .iter()
            .map(|(transaction_id, receipt)| (*transaction_id, receipt.write_set.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut outcomes = Vec::with_capacity(transactions.len());
        let mut dependency_evidence = Vec::new();
        let reconciliation_started = Instant::now();
        let mut timings = PostConsensusTimings::default();

        for (transaction_index, transaction) in transactions.into_iter().enumerate() {
            let transaction_id = transaction.transaction_id();
            let started = Instant::now();
            let matching_receipt = receipts.remove(&transaction_id).and_then(|receipt| {
                if receipt.block == transaction.block && receipt.request == transaction.request {
                    Some(receipt)
                } else {
                    prediction.discarded_predictions += 1;
                    None
                }
            });
            timings.receipt_matching += started.elapsed();

            let Some(receipt) = matching_receipt else {
                prediction.missing_predictions += 1;
                metrics.canonical_transactions += 1;
                let started = Instant::now();
                let result = self.execute_request(transaction.block, transaction.request);
                timings.replay_or_missing_execution += started.elapsed();
                outcomes.push(CanonicalTxResult {
                    transaction_id,
                    disposition: CanonicalTxDisposition::Canonical,
                    validation: None,
                    result,
                });
                continue;
            };

            prediction.matched_transactions += 1;
            let started = Instant::now();
            let validation = validate_dependencies(&self.core.state, &receipt.read_dependencies);
            timings.validation += started.elapsed();
            if validation.is_valid() {
                metrics.reused_results += 1;
                let started = Instant::now();
                let result = self.commit_reused_receipt(receipt);
                timings.commit_reused += started.elapsed();
                outcomes.push(CanonicalTxResult {
                    transaction_id,
                    disposition: CanonicalTxDisposition::ReusedSpeculative,
                    validation: Some(validation),
                    result,
                });
            } else {
                metrics.invalidated_results += 1;
                metrics.replayed_transactions += 1;
                for (conflict_index, conflict) in validation.conflicts().iter().enumerate() {
                    if let Some(predecessor_index) = (0..transaction_index).rev().find(|index| {
                        prepared_write_sets
                            .get(&canonical_order_ids[*index])
                            .is_some_and(|write_set| {
                                write_set_touches_conflict(write_set, conflict)
                            })
                    }) {
                        dependency_evidence.push(ReconciliationDependencyEvidence {
                            predecessor_index,
                            transaction_index,
                            conflict_index,
                        });
                    }
                }
                let started = Instant::now();
                let result = self.execute_request(transaction.block, transaction.request);
                timings.replay_or_missing_execution += started.elapsed();
                outcomes.push(CanonicalTxResult {
                    transaction_id,
                    disposition: CanonicalTxDisposition::Replayed,
                    validation: Some(validation),
                    result,
                });
            }
        }

        prediction.discarded_predictions = prediction
            .discarded_predictions
            .saturating_add(receipts.len() as u64);
        timings.total = reconciliation_started.elapsed();

        Ok(SplitPhaseSpeculativeBlockOutcome {
            transactions: outcomes,
            speculative: metrics,
            prediction,
            timings,
            dependency_evidence,
        })
    }

    /// Execute Brick-4 waves through a bounded Rayon worker pool, then validate and commit
    /// receipts strictly in canonical block order.
    ///
    /// Every wave is launched against one immutable snapshot captured after the canonical prefix
    /// made available by previous waves has been drained. The method waits for all speculative
    /// executions in a wave to finish before launching the next wave. Physical worker completion
    /// order never affects canonical commit order.
    pub fn execute_parallel_waves(
        &self,
        config: ParallelExecutionConfig,
        transactions: Vec<CanonicalTransaction>,
        waves: Vec<SpeculativeWave>,
    ) -> EngineResult<ParallelSpeculativeBlockOutcome> {
        if config.workers == 0 {
            return Err(EngineError::InvalidConfiguration(
                "parallel speculative worker count must be greater than zero".to_owned(),
            ));
        }

        let mut canonical_bindings = BTreeMap::new();
        for transaction in &transactions {
            let transaction_id = transaction.transaction_id();
            if canonical_bindings
                .insert(transaction_id, transaction.clone())
                .is_some()
            {
                return Err(EngineError::InvalidConfiguration(format!(
                    "duplicate canonical transaction ID {}",
                    transaction_id.0
                )));
            }
        }

        let mut scheduled = BTreeSet::new();
        for (wave_index, wave) in waves.iter().enumerate() {
            if wave.is_empty() {
                return Err(EngineError::InvalidConfiguration(format!(
                    "speculative wave {wave_index} is empty"
                )));
            }
            for transaction_id in &wave.transaction_ids {
                if !canonical_bindings.contains_key(transaction_id) {
                    return Err(EngineError::InvalidConfiguration(format!(
                        "speculative wave {wave_index} references transaction ID {} that is not in the canonical block",
                        transaction_id.0
                    )));
                }
                if !scheduled.insert(*transaction_id) {
                    return Err(EngineError::InvalidConfiguration(format!(
                        "transaction ID {} appears in more than one speculative wave",
                        transaction_id.0
                    )));
                }
            }
        }

        if scheduled.len() != transactions.len() {
            let missing = transactions
                .iter()
                .filter_map(|transaction| {
                    let transaction_id = transaction.transaction_id();
                    (!scheduled.contains(&transaction_id)).then_some(transaction_id.0)
                })
                .collect::<Vec<_>>();
            return Err(EngineError::InvalidConfiguration(format!(
                "speculative wave plan does not cover canonical transaction IDs {missing:?}"
            )));
        }

        let pool = ThreadPoolBuilder::new()
            .num_threads(config.workers)
            .thread_name(|index| format!("acg-speculative-{index}"))
            .build()
            .map_err(|error| {
                EngineError::InvalidConfiguration(format!(
                    "failed to build speculative worker pool: {error}"
                ))
            })?;

        let wave_widths = waves.iter().map(SpeculativeWave::len).collect::<Vec<_>>();
        let mut speculative_metrics = SpeculativeExecutionMetrics {
            speculative_results: transactions.len() as u64,
            ..SpeculativeExecutionMetrics::default()
        };
        let mut pending_receipts = BTreeMap::new();
        let mut outcomes = Vec::with_capacity(transactions.len());
        let mut canonical_cursor = 0_usize;

        for wave in waves {
            // Snapshot capture occurs after the previous wave's available canonical prefix was
            // drained. Every transaction in this wave sees exactly the same immutable base state.
            let snapshot = self.snapshot();
            let wave_transactions = wave
                .transaction_ids
                .iter()
                .map(|transaction_id| {
                    canonical_bindings
                        .get(transaction_id)
                        .expect("wave plan was validated before execution")
                        .clone()
                })
                .collect::<Vec<_>>();

            let receipts = pool.install(|| {
                wave_transactions
                    .into_par_iter()
                    .map(|transaction| {
                        self.execute_speculative(&snapshot, transaction.block, transaction.request)
                    })
                    .collect::<Vec<_>>()
            });

            for receipt in receipts {
                let receipt = receipt?;
                let transaction_id = receipt.transaction_id;
                if pending_receipts.insert(transaction_id, receipt).is_some() {
                    return Err(EngineError::Internal(format!(
                        "duplicate speculative receipt for transaction ID {}",
                        transaction_id.0
                    )));
                }
            }

            while canonical_cursor < transactions.len() {
                let transaction = &transactions[canonical_cursor];
                let transaction_id = transaction.transaction_id();
                let Some(receipt) = pending_receipts.remove(&transaction_id) else {
                    break;
                };

                let validation =
                    validate_dependencies(&self.core.state, &receipt.read_dependencies);
                if validation.is_valid() {
                    speculative_metrics.reused_results += 1;
                    let result = self.commit_reused_receipt(receipt);
                    outcomes.push(CanonicalTxResult {
                        transaction_id,
                        disposition: CanonicalTxDisposition::ReusedSpeculative,
                        validation: Some(validation),
                        result,
                    });
                } else {
                    speculative_metrics.invalidated_results += 1;
                    speculative_metrics.replayed_transactions += 1;
                    let result = self
                        .execute_request(transaction.block.clone(), transaction.request.clone());
                    outcomes.push(CanonicalTxResult {
                        transaction_id,
                        disposition: CanonicalTxDisposition::Replayed,
                        validation: Some(validation),
                        result,
                    });
                }
                canonical_cursor += 1;
            }
        }

        if canonical_cursor != transactions.len() || !pending_receipts.is_empty() {
            return Err(EngineError::Internal(
                "validated speculative wave plan did not drain the complete canonical block"
                    .to_owned(),
            ));
        }

        Ok(ParallelSpeculativeBlockOutcome {
            transactions: outcomes,
            metrics: ParallelSpeculativeExecutionMetrics {
                workers: config.workers,
                wave_widths,
                speculative: speculative_metrics,
                dependency_count: 0,
                hard_dependency_count: 0,
            },
        })
    }

    fn ensure_receipt_belongs_to_engine(&self, result: &SpeculativeTxResult) -> EngineResult<()> {
        if !Arc::ptr_eq(&result.engine_identity, &self.core.engine_identity) {
            return Err(EngineError::InvalidConfiguration(
                "speculative receipt belongs to a different CosmWasm engine".to_owned(),
            ));
        }
        Ok(())
    }

    fn commit_reused_receipt(
        &self,
        receipt: SpeculativeTxResult,
    ) -> Result<ExecutionOutcome, EngineError> {
        let SpeculativeTxResult {
            transaction_id,
            status,
            accesses,
            write_set,
            ..
        } = receipt;

        match status {
            SpeculativeExecutionStatus::Succeeded(outcome) => {
                apply_write_set(&self.core.state, &write_set);
                Ok(ExecutionOutcome {
                    transaction_id,
                    contract: outcome.contract,
                    events: outcome.events,
                    data: outcome.data,
                    accesses,
                    created_contracts: outcome.created_contracts,
                })
            }
            SpeculativeExecutionStatus::Failed(error) => {
                debug_assert!(write_set.is_empty());
                Err(error)
            }
        }
    }

    pub fn execute_request(
        &self,
        block: BlockContext,
        request: ExecutionRequest,
    ) -> EngineResult<ExecutionOutcome> {
        self.execute_request_on_state(self.core.state.clone(), block, request, true)
            .0
    }

    #[allow(clippy::too_many_arguments)]
    pub fn instantiate(
        &self,
        transaction_id: TransactionId,
        block: BlockContext,
        sender: Address,
        code_id: CodeId,
        admin: Option<Address>,
        label: String,
        funds: Vec<Coin>,
        msg: Binary,
    ) -> EngineResult<ExecutionOutcome> {
        self.execute_request(
            block,
            ExecutionRequest::Instantiate {
                transaction_id,
                sender,
                code_id,
                admin,
                label,
                funds,
                msg,
            },
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn execute(
        &self,
        transaction_id: TransactionId,
        block: BlockContext,
        sender: Address,
        contract: Address,
        funds: Vec<Coin>,
        msg: Binary,
    ) -> EngineResult<ExecutionOutcome> {
        self.execute_request(
            block,
            ExecutionRequest::Execute {
                transaction_id,
                sender,
                contract,
                funds,
                msg,
            },
        )
    }

    pub fn query(
        &self,
        block: BlockContext,
        contract: Address,
        msg: Binary,
    ) -> EngineResult<QueryOutcome> {
        validate_public_address(&contract)?;
        let tx = self.begin_transaction(TransactionId(0));
        let data = query_contract_shared(
            self.core.clone(),
            tx.clone(),
            block,
            contract.clone(),
            msg,
            0,
        )?;
        let accesses = tx.lock().accesses.clone();
        Ok(QueryOutcome {
            contract,
            data,
            accesses,
        })
    }

    fn execute_request_on_state(
        &self,
        base: SharedWorld,
        block: BlockContext,
        request: ExecutionRequest,
        commit: bool,
    ) -> (EngineResult<ExecutionOutcome>, SharedTx) {
        let transaction_id = request.transaction_id();
        let tx = self.begin_transaction_on(base, transaction_id);
        let result = match request {
            ExecutionRequest::Instantiate {
                sender,
                code_id,
                admin,
                label,
                funds,
                msg,
                ..
            } => self.instantiate_in_transaction(
                tx.clone(),
                transaction_id,
                block,
                sender,
                code_id,
                admin,
                label,
                funds,
                msg,
            ),
            ExecutionRequest::Execute {
                sender,
                contract,
                funds,
                msg,
                ..
            } => self.execute_in_transaction(
                tx.clone(),
                transaction_id,
                block,
                sender,
                contract,
                funds,
                msg,
            ),
        };

        if commit && result.is_ok() {
            tx.lock().commit();
        }
        (result, tx)
    }

    #[allow(clippy::too_many_arguments)]
    fn instantiate_in_transaction(
        &self,
        tx: SharedTx,
        transaction_id: TransactionId,
        block: BlockContext,
        sender: Address,
        code_id: CodeId,
        admin: Option<Address>,
        label: String,
        funds: Vec<Coin>,
        msg: Binary,
    ) -> EngineResult<ExecutionOutcome> {
        validate_public_address(&sender)?;
        if let Some(admin) = &admin {
            validate_public_address(admin)?;
        }
        validate_contract_label(&label)?;
        let code = self.code(code_id)?;

        let contract = tx
            .lock()
            .allocate_contract_address(&self.core.config.contract_address_prefix)?;
        tx.lock().create_contract(ContractMetadata {
            address: contract.clone(),
            code_id,
            code_checksum: code.metadata.checksum,
            creator: sender.clone(),
            admin,
            label,
        })?;
        tx.lock()
            .transfer(&sender, &contract, &funds, &contract, 0)?;

        let response = invoke_contract(
            self.core.clone(),
            tx.clone(),
            block.clone(),
            contract.clone(),
            sender,
            funds,
            msg,
            0,
            Entrypoint::Instantiate,
        )?;
        let mut processed = process_response(
            self.core.clone(),
            tx.clone(),
            block,
            contract.clone(),
            response,
            0,
        )?;
        processed.events.insert(
            0,
            Event::new("instantiate").add_attribute("_contract_address", contract.as_str()),
        );
        let state = tx.lock();
        Ok(ExecutionOutcome {
            transaction_id,
            contract,
            events: processed.events,
            data: processed.data,
            accesses: state.accesses.clone(),
            created_contracts: state.created_addresses(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn execute_in_transaction(
        &self,
        tx: SharedTx,
        transaction_id: TransactionId,
        block: BlockContext,
        sender: Address,
        contract: Address,
        funds: Vec<Coin>,
        msg: Binary,
    ) -> EngineResult<ExecutionOutcome> {
        validate_public_address(&sender)?;
        validate_public_address(&contract)?;
        code_id_of(&tx, &contract)?;
        tx.lock()
            .transfer(&sender, &contract, &funds, &contract, 0)?;

        let response = invoke_contract(
            self.core.clone(),
            tx.clone(),
            block.clone(),
            contract.clone(),
            sender,
            funds,
            msg,
            0,
            Entrypoint::Execute,
        )?;
        let processed = process_response(
            self.core.clone(),
            tx.clone(),
            block,
            contract.clone(),
            response,
            0,
        )?;
        let state = tx.lock();
        Ok(ExecutionOutcome {
            transaction_id,
            contract,
            events: processed.events,
            data: processed.data,
            accesses: state.accesses.clone(),
            created_contracts: state.created_addresses(),
        })
    }

    fn begin_transaction(&self, transaction_id: TransactionId) -> SharedTx {
        self.begin_transaction_on(self.core.state.clone(), transaction_id)
    }

    fn begin_transaction_on(&self, base: SharedWorld, transaction_id: TransactionId) -> SharedTx {
        Arc::new(parking_lot::Mutex::new(TransactionState::new(
            base,
            transaction_id,
        )))
    }

    fn code(&self, code_id: CodeId) -> EngineResult<CodeRecord> {
        self.core.code(code_id)
    }
}

struct ValidatedDependencyPlan {
    remaining_predecessors: Vec<usize>,
    successors: Vec<Vec<usize>>,
    hard_dependency_count: usize,
}

struct DependencyExecutionState {
    ready: BTreeSet<usize>,
    remaining_predecessors: Vec<usize>,
    successors: Vec<Vec<usize>>,
    receipts: Vec<Option<SpeculativeTxResult>>,
    completed: usize,
    in_flight: usize,
    failure: Option<String>,
}

fn validate_speculative_dependency_plan(
    config: ParallelExecutionConfig,
    transactions: &[CanonicalTransaction],
    waves: &[SpeculativeWave],
    dependencies: &[SpeculativeDependency],
) -> EngineResult<ValidatedDependencyPlan> {
    let canonical_bindings = validate_speculative_wave_plan(config, transactions, waves)?;
    let index_by_id = transactions
        .iter()
        .enumerate()
        .map(|(index, transaction)| (transaction.transaction_id(), index))
        .collect::<BTreeMap<_, _>>();
    debug_assert_eq!(canonical_bindings.len(), index_by_id.len());

    let wave_by_id = waves
        .iter()
        .enumerate()
        .flat_map(|(wave_index, wave)| {
            wave.transaction_ids
                .iter()
                .copied()
                .map(move |transaction_id| (transaction_id, wave_index))
        })
        .collect::<BTreeMap<_, _>>();

    let mut remaining_predecessors = vec![0_usize; transactions.len()];
    let mut successors = vec![Vec::new(); transactions.len()];
    let mut seen = BTreeSet::new();
    let mut hard_dependency_count = 0;

    for dependency in dependencies {
        let predecessor = *index_by_id.get(&dependency.predecessor).ok_or_else(|| {
            EngineError::InvalidConfiguration(format!(
                "speculative dependency references unknown predecessor transaction ID {}",
                dependency.predecessor.0
            ))
        })?;
        let successor = *index_by_id.get(&dependency.successor).ok_or_else(|| {
            EngineError::InvalidConfiguration(format!(
                "speculative dependency references unknown successor transaction ID {}",
                dependency.successor.0
            ))
        })?;
        if predecessor >= successor {
            return Err(EngineError::InvalidConfiguration(format!(
                "speculative dependency {} -> {} does not follow canonical block order",
                dependency.predecessor.0, dependency.successor.0
            )));
        }
        if !seen.insert((predecessor, successor)) {
            return Err(EngineError::InvalidConfiguration(format!(
                "duplicate speculative dependency {} -> {}",
                dependency.predecessor.0, dependency.successor.0
            )));
        }
        let predecessor_wave = wave_by_id[&dependency.predecessor];
        let successor_wave = wave_by_id[&dependency.successor];
        if predecessor_wave >= successor_wave {
            return Err(EngineError::InvalidConfiguration(format!(
                "speculative dependency {} -> {} is not represented by increasing scheduler levels ({predecessor_wave} -> {successor_wave})",
                dependency.predecessor.0, dependency.successor.0
            )));
        }
        remaining_predecessors[successor] += 1;
        successors[predecessor].push(successor);
        if dependency.class == SpeculativeDependencyClass::Hard {
            hard_dependency_count += 1;
        }
    }
    for next in &mut successors {
        next.sort_unstable();
        next.dedup();
    }

    Ok(ValidatedDependencyPlan {
        remaining_predecessors,
        successors,
        hard_dependency_count,
    })
}

fn validate_speculative_wave_plan(
    config: ParallelExecutionConfig,
    transactions: &[CanonicalTransaction],
    waves: &[SpeculativeWave],
) -> EngineResult<BTreeMap<TransactionId, CanonicalTransaction>> {
    if config.workers == 0 {
        return Err(EngineError::InvalidConfiguration(
            "parallel speculative worker count must be greater than zero".to_owned(),
        ));
    }

    let mut canonical_bindings = BTreeMap::new();
    for transaction in transactions {
        let transaction_id = transaction.transaction_id();
        if canonical_bindings
            .insert(transaction_id, transaction.clone())
            .is_some()
        {
            return Err(EngineError::InvalidConfiguration(format!(
                "duplicate canonical transaction ID {}",
                transaction_id.0
            )));
        }
    }

    let mut scheduled = BTreeSet::new();
    for (wave_index, wave) in waves.iter().enumerate() {
        if wave.is_empty() {
            return Err(EngineError::InvalidConfiguration(format!(
                "speculative wave {wave_index} is empty"
            )));
        }
        for transaction_id in &wave.transaction_ids {
            if !canonical_bindings.contains_key(transaction_id) {
                return Err(EngineError::InvalidConfiguration(format!(
                    "speculative wave {wave_index} references transaction ID {} that is not in the canonical block",
                    transaction_id.0
                )));
            }
            if !scheduled.insert(*transaction_id) {
                return Err(EngineError::InvalidConfiguration(format!(
                    "transaction ID {} appears in more than one speculative wave",
                    transaction_id.0
                )));
            }
        }
    }

    if scheduled.len() != transactions.len() {
        let missing = transactions
            .iter()
            .filter_map(|transaction| {
                let transaction_id = transaction.transaction_id();
                (!scheduled.contains(&transaction_id)).then_some(transaction_id.0)
            })
            .collect::<Vec<_>>();
        return Err(EngineError::InvalidConfiguration(format!(
            "speculative wave plan does not cover canonical transaction IDs {missing:?}"
        )));
    }

    Ok(canonical_bindings)
}

fn build_speculative_pool(config: ParallelExecutionConfig) -> EngineResult<rayon::ThreadPool> {
    ThreadPoolBuilder::new()
        .num_threads(config.workers)
        .thread_name(|index| format!("acg-speculative-{index}"))
        .build()
        .map_err(|error| {
            EngineError::InvalidConfiguration(format!(
                "failed to build speculative worker pool: {error}"
            ))
        })
}

impl Default for CosmWasmEngine {
    fn default() -> Self {
        Self::new(EngineConfig::default())
    }
}

impl EngineCore {
    pub(crate) fn code(&self, code_id: CodeId) -> EngineResult<CodeRecord> {
        self.codes
            .read()
            .get(&code_id)
            .cloned()
            .ok_or(EngineError::UnknownCode(code_id))
    }
}

#[derive(Clone, Copy)]
enum Entrypoint {
    Instantiate,
    Execute,
}

struct ProcessedResponse {
    events: Vec<Event>,
    data: Option<Binary>,
}

struct DispatchedMessage {
    events: Vec<Event>,
    reply_events: Vec<Event>,
    data: Option<Binary>,
    msg_responses: Vec<MsgResponse>,
}

struct SubmessageOutcome {
    events: Vec<Event>,
    data_candidate: Option<Binary>,
}

#[allow(clippy::too_many_arguments)]
fn invoke_contract(
    core: Arc<EngineCore>,
    tx: SharedTx,
    block: BlockContext,
    contract: Address,
    caller: Address,
    funds: Vec<Coin>,
    msg: Binary,
    depth: u32,
    entrypoint: Entrypoint,
) -> EngineResult<Response<Empty>> {
    ensure_depth(&core, depth)?;
    let code_id = code_id_of(&tx, &contract)?;
    let artifact = core.code(code_id)?.artifact;
    let env = make_env(&block, &contract);

    match artifact {
        CodeArtifact::Native(native) => {
            let transaction_id = tx.lock().transaction_id;
            let mut context =
                NativeCallContext::new(transaction_id, block, contract, caller.clone(), depth, tx);
            let result = match entrypoint {
                Entrypoint::Instantiate => native.instantiate(
                    &mut context,
                    env,
                    MessageInfo {
                        sender: Addr::unchecked(caller.as_str()),
                        funds,
                    },
                    msg,
                ),
                Entrypoint::Execute => native.execute(
                    &mut context,
                    env,
                    MessageInfo {
                        sender: Addr::unchecked(caller.as_str()),
                        funds,
                    },
                    msg,
                ),
            };
            result.map_err(EngineError::Native)
        }
        CodeArtifact::Wasm(checksum) => {
            let backend = Backend {
                api: EngineApi,
                storage: EngineStorage::new(contract.clone(), depth, tx.clone()),
                querier: EngineQuerier::new(core.clone(), tx, contract, block, depth),
            };
            let mut instance = core.wasm_cache.get_instance(
                &checksum,
                backend,
                InstanceOptions {
                    gas_limit: core.config.gas_limit,
                },
            )?;
            let info = MessageInfo {
                sender: Addr::unchecked(caller.as_str()),
                funds,
            };
            let result: ContractResult<Response<Empty>> = match entrypoint {
                Entrypoint::Instantiate => call_instantiate(&mut instance, &env, &info, &msg)?,
                Entrypoint::Execute => call_execute(&mut instance, &env, &info, &msg)?,
            };
            drop(instance.recycle());
            result.into_result().map_err(EngineError::Contract)
        }
    }
}

fn invoke_reply(
    core: Arc<EngineCore>,
    tx: SharedTx,
    block: BlockContext,
    contract: Address,
    reply: Reply,
    depth: u32,
) -> EngineResult<Response<Empty>> {
    ensure_depth(&core, depth)?;
    let code_id = code_id_of(&tx, &contract)?;
    let artifact = core.code(code_id)?.artifact;
    let env = make_env(&block, &contract);
    match artifact {
        CodeArtifact::Native(native) => {
            let transaction_id = tx.lock().transaction_id;
            let mut context = NativeCallContext::new(
                transaction_id,
                block,
                contract.clone(),
                contract,
                depth,
                tx,
            );
            native
                .reply(&mut context, env, reply)
                .map_err(EngineError::Native)
        }
        CodeArtifact::Wasm(checksum) => {
            let backend = Backend {
                api: EngineApi,
                storage: EngineStorage::new(contract.clone(), depth, tx.clone()),
                querier: EngineQuerier::new(core.clone(), tx, contract, block, depth),
            };
            let mut instance = core.wasm_cache.get_instance(
                &checksum,
                backend,
                InstanceOptions {
                    gas_limit: core.config.gas_limit,
                },
            )?;
            let result: ContractResult<Response<Empty>> = call_reply(&mut instance, &env, &reply)?;
            drop(instance.recycle());
            result.into_result().map_err(EngineError::Contract)
        }
    }
}

pub(crate) fn query_contract_shared(
    core: Arc<EngineCore>,
    tx: SharedTx,
    block: BlockContext,
    contract: Address,
    msg: Binary,
    depth: u32,
) -> EngineResult<Binary> {
    ensure_depth(&core, depth)?;
    let checkpoint = tx.lock().clone();
    let trace_start = checkpoint.accesses.len();
    let dependency_start = checkpoint.read_dependencies.len();
    let code_id = code_id_of(&tx, &contract)?;
    let artifact = core.code(code_id)?.artifact;
    let env = make_env(&block, &contract);

    let result = match artifact {
        CodeArtifact::Native(native) => {
            let mut context = NativeCallContext::new(
                tx.lock().transaction_id,
                block,
                contract.clone(),
                Address::new("query"),
                depth,
                tx.clone(),
            );
            native
                .query(&mut context, env, msg)
                .map_err(EngineError::Native)
        }
        CodeArtifact::Wasm(checksum) => {
            let backend = Backend {
                api: EngineApi,
                storage: EngineStorage::new(contract.clone(), depth, tx.clone()),
                querier: EngineQuerier::new(core.clone(), tx.clone(), contract, block, depth),
            };
            let mut instance = core.wasm_cache.get_instance(
                &checksum,
                backend,
                InstanceOptions {
                    gas_limit: core.config.gas_limit,
                },
            )?;
            let result: ContractResult<Binary> = call_query(&mut instance, &env, &msg)?;
            drop(instance.recycle());
            result.into_result().map_err(EngineError::Contract)
        }
    };

    let current = tx.lock().clone();
    let mut attempted = current.accesses[trace_start..].to_vec();
    let wrote = attempted.iter().any(|access| {
        matches!(
            access.kind,
            AccessKind::StorageWrite | AccessKind::StorageRemove | AccessKind::BankWrite
        )
    }) || current.created_contracts != checkpoint.created_contracts;
    if wrote {
        let mut restored = checkpoint;
        let attempted_dependencies = current.read_dependencies[dependency_start..].to_vec();
        for access in &mut attempted {
            access.reverted = true;
        }
        restored.accesses.extend(attempted);
        restored.read_dependencies.extend(attempted_dependencies);
        *tx.lock() = restored;
        return Err(EngineError::Contract(
            "query attempted to mutate state".to_owned(),
        ));
    }

    result
}

fn process_response(
    core: Arc<EngineCore>,
    tx: SharedTx,
    block: BlockContext,
    contract: Address,
    response: Response<Empty>,
    depth: u32,
) -> EngineResult<ProcessedResponse> {
    let mut events = normalize_response_events(&contract, response.attributes, response.events);
    let mut data = response.data;

    for submessage in response.messages {
        let child = dispatch_submessage(
            core.clone(),
            tx.clone(),
            block.clone(),
            contract.clone(),
            submessage,
            depth + 1,
        )?;
        events.extend(child.events);
        if let Some(candidate) = child.data_candidate {
            data = Some(candidate);
        }
    }

    Ok(ProcessedResponse { events, data })
}

#[allow(deprecated)]
fn dispatch_submessage(
    core: Arc<EngineCore>,
    tx: SharedTx,
    block: BlockContext,
    caller_contract: Address,
    submessage: SubMsg<Empty>,
    depth: u32,
) -> EngineResult<SubmessageOutcome> {
    ensure_depth(&core, depth)?;
    let checkpoint = tx.lock().clone();
    let trace_start = checkpoint.accesses.len();
    let dependency_start = checkpoint.read_dependencies.len();
    let result = dispatch_message(
        core.clone(),
        tx.clone(),
        block.clone(),
        caller_contract.clone(),
        submessage.msg,
        depth,
    );

    let succeeded = result.is_ok();
    if !succeeded {
        let current = tx.lock().clone();
        let mut restored = checkpoint;
        let mut attempted = current.accesses[trace_start..].to_vec();
        let attempted_dependencies = current.read_dependencies[dependency_start..].to_vec();
        for access in &mut attempted {
            access.reverted = true;
        }
        restored.accesses.extend(attempted);
        restored.read_dependencies.extend(attempted_dependencies);
        *tx.lock() = restored;
    }

    let should_reply = matches!(submessage.reply_on, ReplyOn::Always)
        || (succeeded && matches!(submessage.reply_on, ReplyOn::Success))
        || (!succeeded && matches!(submessage.reply_on, ReplyOn::Error));

    if should_reply {
        let (emitted_events, sub_result) = match &result {
            Ok(dispatched) => (
                dispatched.events.clone(),
                SubMsgResult::Ok(SubMsgResponse {
                    events: dispatched.reply_events.clone(),
                    data: dispatched.data.clone(),
                    msg_responses: dispatched.msg_responses.clone(),
                }),
            ),
            Err(error) => (Vec::new(), SubMsgResult::Err(error.to_string())),
        };
        let reply = Reply {
            id: submessage.id,
            payload: submessage.payload,
            gas_used: 0,
            result: sub_result,
        };
        let response = invoke_reply(
            core.clone(),
            tx.clone(),
            block.clone(),
            caller_contract.clone(),
            reply,
            depth,
        )?;
        let reply_processed = process_response(core, tx, block, caller_contract, response, depth)?;
        let mut events = emitted_events;
        events.extend(reply_processed.events);
        return Ok(SubmessageOutcome {
            events,
            data_candidate: reply_processed.data,
        });
    }

    result.map(|dispatched| SubmessageOutcome {
        events: dispatched.events,
        data_candidate: None,
    })
}

fn dispatch_message(
    core: Arc<EngineCore>,
    tx: SharedTx,
    block: BlockContext,
    caller_contract: Address,
    message: CosmosMsg<Empty>,
    depth: u32,
) -> EngineResult<DispatchedMessage> {
    match message {
        CosmosMsg::Bank(BankMsg::Send { to_address, amount }) => {
            let target = Address::new(to_address);
            validate_public_address(&target)?;
            tx.lock()
                .transfer(&caller_contract, &target, &amount, &caller_contract, depth)?;
            let events = vec![Event::new("transfer")
                .add_attribute("sender", caller_contract.as_str())
                .add_attribute("recipient", target.as_str())];
            Ok(DispatchedMessage {
                reply_events: events.clone(),
                events,
                data: None,
                msg_responses: vec![MsgResponse {
                    type_url: BANK_SEND_RESPONSE_TYPE_URL.to_owned(),
                    value: Binary::default(),
                }],
            })
        }
        CosmosMsg::Bank(BankMsg::Burn { amount }) => {
            tx.lock()
                .burn(&caller_contract, &amount, &caller_contract, depth)?;
            let events = vec![Event::new("burn").add_attribute("sender", caller_contract.as_str())];
            Ok(DispatchedMessage {
                reply_events: events.clone(),
                events,
                data: None,
                msg_responses: vec![MsgResponse {
                    type_url: BANK_BURN_RESPONSE_TYPE_URL.to_owned(),
                    value: Binary::default(),
                }],
            })
        }
        CosmosMsg::Wasm(WasmMsg::Execute {
            contract_addr,
            msg,
            funds,
        }) => {
            let target = Address::new(contract_addr);
            validate_public_address(&target)?;
            code_id_of(&tx, &target)?;
            tx.lock()
                .transfer(&caller_contract, &target, &funds, &caller_contract, depth)?;
            let response = invoke_contract(
                core.clone(),
                tx.clone(),
                block.clone(),
                target.clone(),
                caller_contract,
                funds,
                msg,
                depth,
                Entrypoint::Execute,
            )?;
            let processed = process_response(core, tx, block, target, response, depth)?;
            let response_bytes =
                encode_execute_response(processed.data.as_ref().map(Binary::as_slice));
            Ok(DispatchedMessage {
                reply_events: processed.events.clone(),
                events: processed.events,
                data: Some(Binary::new(response_bytes.clone())),
                msg_responses: vec![MsgResponse {
                    type_url: EXECUTE_RESPONSE_TYPE_URL.to_owned(),
                    value: Binary::new(response_bytes),
                }],
            })
        }
        CosmosMsg::Wasm(WasmMsg::Instantiate {
            admin,
            code_id,
            msg,
            funds,
            label,
        }) => {
            validate_contract_label(&label)?;
            let code_id = CodeId(code_id);
            let code = core.code(code_id)?;
            let address = tx
                .lock()
                .allocate_contract_address(&core.config.contract_address_prefix)?;
            let admin = admin.map(Address::new);
            if let Some(admin) = &admin {
                validate_public_address(admin)?;
            }
            tx.lock().create_contract(ContractMetadata {
                address: address.clone(),
                code_id,
                code_checksum: code.metadata.checksum,
                creator: caller_contract.clone(),
                admin,
                label,
            })?;
            tx.lock()
                .transfer(&caller_contract, &address, &funds, &caller_contract, depth)?;
            let response = invoke_contract(
                core.clone(),
                tx.clone(),
                block.clone(),
                address.clone(),
                caller_contract,
                funds,
                msg,
                depth,
                Entrypoint::Instantiate,
            )?;
            let mut processed =
                process_response(core, tx, block, address.clone(), response, depth)?;
            processed.events.insert(
                0,
                Event::new("instantiate").add_attribute("_contract_address", address.as_str()),
            );
            let response_bytes = encode_instantiate_response(
                &address,
                processed.data.as_ref().map(Binary::as_slice),
            );
            Ok(DispatchedMessage {
                reply_events: processed.events.clone(),
                events: processed.events,
                data: Some(Binary::new(response_bytes.clone())),
                msg_responses: vec![MsgResponse {
                    type_url: INSTANTIATE_RESPONSE_TYPE_URL.to_owned(),
                    value: Binary::new(response_bytes),
                }],
            })
        }
        other => Err(EngineError::UnsupportedMessage(format!("{other:?}"))),
    }
}

fn normalize_response_events(
    contract: &Address,
    attributes: Vec<Attribute>,
    custom_events: Vec<Event>,
) -> Vec<Event> {
    let mut events = Vec::with_capacity(custom_events.len() + usize::from(!attributes.is_empty()));
    if !attributes.is_empty() {
        let mut wasm_event =
            Event::new("wasm").add_attribute("_contract_address", contract.as_str());
        for attribute in attributes {
            wasm_event = wasm_event.add_attribute(attribute.key, attribute.value);
        }
        events.push(wasm_event);
    }

    for mut event in custom_events {
        event.ty = format!("wasm-{}", event.ty);
        event.attributes.insert(
            0,
            Attribute {
                key: "_contract_address".to_owned(),
                value: contract.to_string(),
            },
        );
        events.push(event);
    }
    events
}

fn make_env(block: &BlockContext, contract: &Address) -> Env {
    Env {
        block: BlockInfo {
            height: block.height,
            time: Timestamp::from_nanos(block.time_nanos),
            chain_id: block.chain_id.clone(),
        },
        transaction: block
            .transaction_index
            .map(|index| TransactionInfo { index }),
        contract: ContractInfo {
            address: Addr::unchecked(contract.as_str()),
        },
    }
}

fn ensure_depth(core: &EngineCore, depth: u32) -> EngineResult<()> {
    if depth > core.config.max_call_depth {
        Err(EngineError::MaxCallDepth(core.config.max_call_depth))
    } else {
        Ok(())
    }
}

fn validate_public_address(address: &Address) -> EngineResult<()> {
    validate_address(address.as_str()).map_err(|reason| EngineError::InvalidAddress {
        address: address.to_string(),
        reason,
    })
}

fn validate_code_label(label: &str) -> EngineResult<()> {
    if label.trim().is_empty() {
        return Err(EngineError::InvalidCodeLabel {
            label: label.to_owned(),
            reason: "label must not be empty".to_owned(),
        });
    }
    if label.len() > 256 {
        return Err(EngineError::InvalidCodeLabel {
            label: label.to_owned(),
            reason: "label exceeds 256 bytes".to_owned(),
        });
    }
    Ok(())
}

fn validate_contract_label(label: &str) -> EngineResult<()> {
    if label.trim().is_empty() {
        return Err(EngineError::InvalidContractLabel {
            label: label.to_owned(),
            reason: "label must not be empty".to_owned(),
        });
    }
    if label.len() > 128 {
        return Err(EngineError::InvalidContractLabel {
            label: label.to_owned(),
            reason: "label exceeds 128 bytes".to_owned(),
        });
    }
    Ok(())
}

fn validate_balance_seed(coins: &[Coin]) -> EngineResult<()> {
    let mut denoms = BTreeSet::new();
    for coin in coins {
        if coin.denom.is_empty() {
            return Err(EngineError::InvalidCoin {
                denom: coin.denom.clone(),
                amount: coin.amount.u128(),
                reason: "denomination must not be empty".to_owned(),
            });
        }
        if !denoms.insert(coin.denom.clone()) {
            return Err(EngineError::DuplicateDenomination(coin.denom.clone()));
        }
    }
    Ok(())
}

fn checksum_from_vm(checksum: cosmwasm_std::Checksum) -> CodeChecksum {
    let mut bytes = [0_u8; 32];
    bytes.copy_from_slice(checksum.as_slice());
    CodeChecksum::new(bytes)
}

fn checksum_native_label(label: &str) -> CodeChecksum {
    let mut hasher = Sha256::new();
    hasher.update(b"acg-native-code-v1\0");
    hasher.update(label.as_bytes());
    checksum_from_digest(hasher.finalize())
}

fn checksum_from_digest(digest: impl AsRef<[u8]>) -> CodeChecksum {
    let mut bytes = [0_u8; 32];
    bytes.copy_from_slice(digest.as_ref());
    CodeChecksum::new(bytes)
}

fn encode_execute_response(data: Option<&[u8]>) -> Vec<u8> {
    let mut output = Vec::new();
    if let Some(data) = data.filter(|data| !data.is_empty()) {
        encode_length_delimited(1, data, &mut output);
    }
    output
}

fn encode_instantiate_response(address: &Address, data: Option<&[u8]>) -> Vec<u8> {
    let mut output = Vec::new();
    encode_length_delimited(1, address.as_str().as_bytes(), &mut output);
    if let Some(data) = data.filter(|data| !data.is_empty()) {
        encode_length_delimited(2, data, &mut output);
    }
    output
}

fn encode_length_delimited(field_number: u32, value: &[u8], output: &mut Vec<u8>) {
    encode_varint(u64::from((field_number << 3) | 2), output);
    encode_varint(value.len() as u64, output);
    output.extend_from_slice(value);
}

fn encode_varint(mut value: u64, output: &mut Vec<u8>) {
    while value >= 0x80 {
        output.push((value as u8) | 0x80);
        value >>= 7;
    }
    output.push(value as u8);
}
