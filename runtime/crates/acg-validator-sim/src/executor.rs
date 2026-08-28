use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};

use acg_cosmwasm_engine::{
    CanonicalCommitDiagnostics, CanonicalTransaction, CanonicalTxDisposition,
    ContractExecutionDiagnostics, CosmWasmEngine,
    EngineError, ExecutionOutcome, ParallelExecutionConfig, PostConsensusTimings,
    PredictionMatchMetrics, PreparedSpeculativeBlock, ReconciliationDependencyEvidence,
    SpeculativeDependency, SpeculativeDependencyClass, SpeculativeExecutionMetrics,
    SpeculativeWave, SplitPhaseSpeculativeBlockOutcome, StateSnapshot, StateWriteSet, TransactionId,
    ValidationOutcome,
};
use thiserror::Error;

use crate::block::ProducedBlock;
use crate::scheduler::{ExecutionDependencyClass, ExecutionPlan, SchedulingError};

/// Validator-local execution timing used by Phase 5E measurement and cost estimation.
/// These values are never consensus inputs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TransactionExecutionTiming {
    pub started_after_phase: Duration,
    pub completed_after_phase: Duration,
    pub service_duration: Duration,
}

#[derive(Debug)]
pub struct TransactionExecution {
    pub transaction_index: usize,
    pub transaction_id: TransactionId,
    pub result: Result<ExecutionOutcome, EngineError>,
    pub timing: TransactionExecutionTiming,
}

#[derive(Debug)]
pub struct BlockExecutionReport {
    pub block_height: u64,
    pub block_time_nanos: u64,
    pub transactions: Vec<TransactionExecution>,
}

impl BlockExecutionReport {
    pub fn successful(&self) -> usize {
        self.transactions
            .iter()
            .filter(|execution| execution.result.is_ok())
            .count()
    }

    pub fn failed(&self) -> usize {
        self.transactions.len().saturating_sub(self.successful())
    }
}

pub trait BlockExecutor: Send + Sync {
    fn execute(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<BlockExecutionReport, BlockExecutionError>;
}

#[derive(Clone)]
pub struct SerialBlockExecutor {
    engine: CosmWasmEngine,
}

impl SerialBlockExecutor {
    pub fn new(engine: CosmWasmEngine) -> Self {
        Self { engine }
    }

    pub fn engine(&self) -> &CosmWasmEngine {
        &self.engine
    }

    pub fn execute(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<BlockExecutionReport, BlockExecutionError> {
        plan.validate()?;
        for (wave_index, wave) in plan.waves.iter().enumerate() {
            if wave.transaction_indices.len() > 1 {
                return Err(BlockExecutionError::ParallelWaveUnsupported {
                    wave_index,
                    width: wave.transaction_indices.len(),
                });
            }
        }

        let mut executions = Vec::with_capacity(block.transactions.len());
        let phase_started = Instant::now();
        for wave in &plan.waves {
            let transaction_index = wave.transaction_indices[0];
            let pending = &block.transactions[transaction_index];
            let mut context = block.context.clone();
            context.transaction_index =
                Some(u32::try_from(transaction_index).map_err(|_| {
                    BlockExecutionError::TransactionIndexOverflow(transaction_index)
                })?);
            let started_after_phase = phase_started.elapsed();
            let execution_started = Instant::now();
            let result = self
                .engine
                .execute_request(context, pending.request.clone());
            let service_duration = execution_started.elapsed();
            let completed_after_phase = phase_started.elapsed();
            executions.push(TransactionExecution {
                transaction_index,
                transaction_id: pending.transaction_id(),
                result,
                timing: TransactionExecutionTiming {
                    started_after_phase,
                    completed_after_phase,
                    service_duration,
                },
            });
        }

        Ok(BlockExecutionReport {
            block_height: block.context.height,
            block_time_nanos: block.context.time_nanos,
            transactions: executions,
        })
    }

    /// Execute a canonical serial block while collecting the same contract/VM lifecycle
    /// diagnostics used by speculative execution. This path never constructs MVCC state or enters
    /// the READY-DAG executor.
    pub fn execute_with_diagnostics(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<(BlockExecutionReport, ContractExecutionDiagnostics), BlockExecutionError> {
        plan.validate()?;
        for (wave_index, wave) in plan.waves.iter().enumerate() {
            if wave.transaction_indices.len() > 1 {
                return Err(BlockExecutionError::ParallelWaveUnsupported {
                    wave_index,
                    width: wave.transaction_indices.len(),
                });
            }
        }

        let mut executions = Vec::with_capacity(block.transactions.len());
        let mut contract_diagnostics = ContractExecutionDiagnostics::default();
        let phase_started = Instant::now();
        for wave in &plan.waves {
            let transaction_index = wave.transaction_indices[0];
            let pending = &block.transactions[transaction_index];
            let mut context = block.context.clone();
            context.transaction_index =
                Some(u32::try_from(transaction_index).map_err(|_| {
                    BlockExecutionError::TransactionIndexOverflow(transaction_index)
                })?);
            let started_after_phase = phase_started.elapsed();
            let execution_started = Instant::now();
            let (result, diagnostics) = self
                .engine
                .execute_request_with_diagnostics(context, pending.request.clone());
            let service_duration = execution_started.elapsed();
            contract_diagnostics.merge(&diagnostics);
            let completed_after_phase = phase_started.elapsed();
            executions.push(TransactionExecution {
                transaction_index,
                transaction_id: pending.transaction_id(),
                result,
                timing: TransactionExecutionTiming {
                    started_after_phase,
                    completed_after_phase,
                    service_duration,
                },
            });
        }

        Ok((
            BlockExecutionReport {
                block_height: block.context.height,
                block_time_nanos: block.context.time_nanos,
                transactions: executions,
            },
            contract_diagnostics,
        ))
    }
}

/// Per-transaction reconciliation metadata retained for benchmark diagnostics.
///
/// This is observational only: canonical correctness remains entirely inside the engine's
/// dependency validator. Keeping the disposition and concrete validation outcome allows benchmark
/// harnesses to explain why speculative receipts were rejected without re-running validation.
#[derive(Clone, Debug)]
pub struct ReconciliationTransactionDiagnostic {
    pub transaction_index: usize,
    pub transaction_id: TransactionId,
    pub disposition: CanonicalTxDisposition,
    pub validation: Option<ValidationOutcome>,
    /// Canonical execution time paid because speculation could not be reused. Zero for reused
    /// receipts. Phase 5D consumes this only as adaptive cost evidence.
    pub reexecution_duration: std::time::Duration,
}

/// Post-consensus report for split-phase dependency/MVCC reconciliation.
#[derive(Debug)]
pub struct SplitPhaseSpeculativeExecutionReport {
    pub block: BlockExecutionReport,
    pub speculative: SpeculativeExecutionMetrics,
    pub prediction: PredictionMatchMetrics,
    pub timings: PostConsensusTimings,
    pub reconciliation: Vec<ReconciliationTransactionDiagnostic>,
    pub dependency_evidence: Vec<ReconciliationDependencyEvidence>,
}


/// Aggregate runtime diagnostics for exact dependency-DAG direct replay.
///
/// Worker-time fields may exceed wall time because workers overlap. The nested contract timings are
/// not additive: host storage/query time and canonical read-lock time occur inside request/Wasm time.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DirectDagExecutionDiagnostics {
    pub worker_phase_wall: Duration,
    pub aggregate_ready_wait: Duration,
    pub aggregate_transaction_service: Duration,
    pub max_in_flight: usize,
    pub commit: CanonicalCommitDiagnostics,
    pub contract: ContractExecutionDiagnostics,
}

/// Diagnostic canonical executor for an already-validated dependency plan.
///
/// Unlike [`SpeculativeParallelBlockExecutor`], this executor does not materialize speculative
/// receipts or run receipt validation/reconciliation. Worker threads execute ready transactions
/// against canonical predecessor state in deferred-commit mode. They never take the canonical
/// world writer lock. A coordinator drains completed dependency-independent transactions, applies
/// their write sets in canonical order under one short batched writer lock, and only then releases
/// successors. The Rayon pool is created once with the executor and reused across every block.
///
/// Correctness therefore depends on the supplied plan being complete for the concrete accesses of
/// the block. It is intended for exact-access/replay diagnostics, not as a replacement for the
/// validator's speculative correctness boundary.
#[derive(Clone)]
pub struct DirectDagBlockExecutor {
    engine: CosmWasmEngine,
    workers: usize,
    pool: Arc<rayon::ThreadPool>,
}

impl DirectDagBlockExecutor {
    pub fn new(engine: CosmWasmEngine, workers: usize) -> Result<Self, BlockExecutionError> {
        if workers == 0 {
            return Err(BlockExecutionError::InvalidWorkerCount);
        }
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(workers)
            .thread_name(move |index| format!("acg-direct-dag-{workers}-{index}"))
            .build()
            .map_err(|error| BlockExecutionError::WorkerPool(error.to_string()))?;
        Ok(Self {
            engine,
            workers,
            pool: Arc::new(pool),
        })
    }

    pub fn engine(&self) -> &CosmWasmEngine {
        &self.engine
    }

    pub fn workers(&self) -> usize {
        self.workers
    }

    pub fn execute(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<BlockExecutionReport, BlockExecutionError> {
        self.execute_internal(block, plan, false).map(|(report, _)| report)
    }

    pub fn execute_with_diagnostics(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<(BlockExecutionReport, DirectDagExecutionDiagnostics), BlockExecutionError> {
        self.execute_internal(block, plan, true)
    }

    fn execute_internal(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
        collect_diagnostics: bool,
    ) -> Result<(BlockExecutionReport, DirectDagExecutionDiagnostics), BlockExecutionError> {
        plan.validate()?;
        if plan.transaction_count != block.transactions.len() {
            return Err(BlockExecutionError::TransactionCountMismatch {
                plan: plan.transaction_count,
                block: block.transactions.len(),
            });
        }
        for transaction_index in 0..block.transactions.len() {
            let _ = u32::try_from(transaction_index)
                .map_err(|_| BlockExecutionError::TransactionIndexOverflow(transaction_index))?;
        }
        if block.transactions.is_empty() {
            return Ok((
                BlockExecutionReport {
                    block_height: block.context.height,
                    block_time_nanos: block.context.time_nanos,
                    transactions: Vec::new(),
                },
                DirectDagExecutionDiagnostics::default(),
            ));
        }

        let mut successors = vec![Vec::new(); block.transactions.len()];
        let mut indegree = vec![0usize; block.transactions.len()];
        for dependency in &plan.dependencies {
            successors[dependency.predecessor_index].push(dependency.successor_index);
            indegree[dependency.successor_index] += 1;
        }
        let ready = indegree
            .iter()
            .enumerate()
            .filter_map(|(index, &degree)| (degree == 0).then_some(index))
            .collect::<VecDeque<_>>();

        struct FinishedTransaction {
            execution: TransactionExecution,
            write_set: StateWriteSet,
            contract: ContractExecutionDiagnostics,
        }

        struct ReadyState {
            ready: VecDeque<usize>,
            indegree: Vec<usize>,
            in_flight: usize,
            max_in_flight: usize,
            aggregate_ready_wait: Duration,
            committed: usize,
            stop: bool,
            finished: VecDeque<FinishedTransaction>,
            results: Vec<TransactionExecution>,
        }

        let shared = Arc::new((
            Mutex::new(ReadyState {
                ready,
                indegree,
                in_flight: 0,
                max_in_flight: 0,
                aggregate_ready_wait: Duration::ZERO,
                committed: 0,
                stop: false,
                finished: VecDeque::new(),
                results: Vec::with_capacity(block.transactions.len()),
            }),
            Condvar::new(),
        ));
        let phase_started = Instant::now();
        let worker_count = self.workers.min(block.transactions.len());
        let mut scheduler_deadlock = false;
        let mut diagnostics = DirectDagExecutionDiagnostics::default();

        // Keep the coordinator on the caller thread. `ThreadPool::scope` runs the scope body
        // inside the pool and would consume one worker for the coordinator (and deadlock at 1 worker).
        self.pool.in_place_scope(|scope| {
            for _ in 0..worker_count {
                let shared = Arc::clone(&shared);
                let engine = self.engine.clone();
                scope.spawn(move |_| loop {
                    let transaction_index = {
                        let (lock, ready_changed) = &*shared;
                        let mut state = lock.lock().expect("direct DAG ready mutex poisoned");
                        loop {
                            if state.stop || state.committed == block.transactions.len() {
                                break None;
                            }
                            if let Some(index) = state.ready.pop_front() {
                                state.in_flight += 1;
                                state.max_in_flight = state.max_in_flight.max(state.in_flight);
                                break Some(index);
                            }
                            let wait_started = Instant::now();
                            state = ready_changed
                                .wait(state)
                                .expect("direct DAG ready mutex poisoned");
                            state.aggregate_ready_wait += wait_started.elapsed();
                        }
                    };
                    let Some(transaction_index) = transaction_index else {
                        break;
                    };
                    let pending = &block.transactions[transaction_index];
                    let mut context = block.context.clone();
                    context.transaction_index = Some(transaction_index as u32);
                    let started_after_phase = phase_started.elapsed();
                    let execution_started = Instant::now();
                    let (result, write_set, contract) = if collect_diagnostics {
                        engine.execute_request_deferred_with_diagnostics(
                            context,
                            pending.request.clone(),
                        )
                    } else {
                        let (result, write_set) =
                            engine.execute_request_deferred(context, pending.request.clone());
                        (result, write_set, ContractExecutionDiagnostics::default())
                    };
                    let service_duration = execution_started.elapsed();
                    let completed_after_phase = phase_started.elapsed();
                    let execution = TransactionExecution {
                        transaction_index,
                        transaction_id: pending.transaction_id(),
                        result,
                        timing: TransactionExecutionTiming {
                            started_after_phase,
                            completed_after_phase,
                            service_duration,
                        },
                    };

                    let (lock, ready_changed) = &*shared;
                    let mut state = lock.lock().expect("direct DAG ready mutex poisoned");
                    state.in_flight = state.in_flight.saturating_sub(1);
                    state.finished.push_back(FinishedTransaction {
                        execution,
                        write_set,
                        contract,
                    });
                    ready_changed.notify_all();
                });
            }

            // The caller thread acts as the only canonical-state writer. Workers remain entirely
            // on read/compute paths and can continue processing other already-ready transactions
            // while completed transactions accumulate into the next short commit batch.
            loop {
                let mut batch = {
                    let (lock, ready_changed) = &*shared;
                    let mut state = lock.lock().expect("direct DAG ready mutex poisoned");
                    loop {
                        if state.committed == block.transactions.len() {
                            break Vec::new();
                        }
                        if !state.finished.is_empty() {
                            break state.finished.drain(..).collect::<Vec<_>>();
                        }
                        if state.ready.is_empty() && state.in_flight == 0 {
                            state.stop = true;
                            scheduler_deadlock = true;
                            ready_changed.notify_all();
                            break Vec::new();
                        }
                        state = ready_changed
                            .wait(state)
                            .expect("direct DAG ready mutex poisoned");
                    }
                };

                if scheduler_deadlock || batch.is_empty() {
                    break;
                }
                batch.sort_by_key(|finished| finished.execution.transaction_index);
                let mut write_sets = Vec::with_capacity(batch.len());
                let mut committed_executions = Vec::with_capacity(batch.len());
                for finished in batch {
                    if collect_diagnostics {
                        diagnostics.aggregate_transaction_service +=
                            finished.execution.timing.service_duration;
                        diagnostics.contract.merge(&finished.contract);
                    }
                    write_sets.push(finished.write_set);
                    committed_executions.push(finished.execution);
                }
                if collect_diagnostics {
                    let commit = self
                        .engine
                        .apply_canonical_write_sets_with_diagnostics(&write_sets);
                    diagnostics.commit.merge(&commit);
                } else {
                    self.engine.apply_canonical_write_sets(&write_sets);
                }

                let (lock, ready_changed) = &*shared;
                let mut state = lock.lock().expect("direct DAG ready mutex poisoned");
                for execution in committed_executions {
                    let transaction_index = execution.transaction_index;
                    state.results.push(execution);
                    state.committed += 1;
                    for &successor in &successors[transaction_index] {
                        debug_assert!(state.indegree[successor] > 0);
                        state.indegree[successor] -= 1;
                        if state.indegree[successor] == 0 {
                            state.ready.push_back(successor);
                        }
                    }
                }
                ready_changed.notify_all();
            }
        });

        if scheduler_deadlock {
            return Err(BlockExecutionError::DirectDagDeadlock);
        }

        diagnostics.worker_phase_wall = phase_started.elapsed();
        let (lock, _) = &*shared;
        let mut state = lock.lock().expect("direct DAG ready mutex poisoned");
        if collect_diagnostics {
            diagnostics.aggregate_ready_wait = state.aggregate_ready_wait;
            diagnostics.max_in_flight = state.max_in_flight;
        }
        let mut executions = std::mem::take(&mut state.results);
        executions.sort_by_key(|execution| execution.transaction_index);
        Ok((
            BlockExecutionReport {
                block_height: block.context.height,
                block_time_nanos: block.context.time_nanos,
                transactions: executions,
            },
            diagnostics,
        ))
    }
}

#[derive(Clone)]
pub struct SpeculativeParallelBlockExecutor {
    engine: CosmWasmEngine,
    config: ParallelExecutionConfig,
}

impl SpeculativeParallelBlockExecutor {
    pub fn new(engine: CosmWasmEngine, config: ParallelExecutionConfig) -> Self {
        Self { engine, config }
    }

    pub fn engine(&self) -> &CosmWasmEngine {
        &self.engine
    }

    pub fn config(&self) -> ParallelExecutionConfig {
        self.config
    }

    /// Pre-consensus phase: execute the predicted block through its dependency ready-DAG without canonical mutation.
    pub fn prepare(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<PreparedSpeculativeBlock, BlockExecutionError> {
        let snapshot = self.engine.snapshot();
        self.prepare_from_snapshot(&snapshot, block, plan)
    }

    /// Pre-consensus execution with a real launch cutoff. Planning is expected to subtract its
    /// own wall time before calling this method; the executor stops launching new transactions
    /// once `cutoff` elapses, while already-running transactions are allowed to complete.
    pub fn prepare_with_cutoff(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
        cutoff: Duration,
    ) -> Result<PreparedSpeculativeBlock, BlockExecutionError> {
        let snapshot = self.engine.snapshot();
        self.prepare_from_snapshot_with_cutoff(&snapshot, block, plan, cutoff)
    }

    /// Pre-consensus dependency-driven phase starting from an explicit predecessor snapshot.
    pub fn prepare_from_snapshot(
        &self,
        snapshot: &StateSnapshot,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<PreparedSpeculativeBlock, BlockExecutionError> {
        let (canonical_transactions, speculative_waves, dependencies) =
            split_phase_inputs(block, plan)?;
        Ok(self.engine.preexecute_dependency_plan_from_snapshot(
            snapshot,
            self.config,
            canonical_transactions,
            speculative_waves,
            dependencies,
        )?)
    }

    pub fn prepare_from_snapshot_with_cutoff(
        &self,
        snapshot: &StateSnapshot,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
        cutoff: Duration,
    ) -> Result<PreparedSpeculativeBlock, BlockExecutionError> {
        let (canonical_transactions, speculative_waves, dependencies) =
            split_phase_inputs(block, plan)?;
        Ok(self
            .engine
            .preexecute_dependency_plan_from_snapshot_with_cutoff(
                snapshot,
                self.config,
                canonical_transactions,
                speculative_waves,
                dependencies,
                Some(cutoff),
            )?)
    }

    /// Build a concrete-access report from successful pre-consensus receipts without executing
    /// anything again. This is used by adaptive feedback so symbolic hard relationships can learn
    /// from the same speculative executions that produced the receipts.
    pub fn pre_execution_report(
        &self,
        block: &ProducedBlock,
        prepared: &PreparedSpeculativeBlock,
    ) -> Result<BlockExecutionReport, BlockExecutionError> {
        if prepared.predicted_transactions.len() != block.transactions.len() {
            return Err(BlockExecutionError::TransactionCountMismatch {
                plan: prepared.predicted_transactions.len(),
                block: block.transactions.len(),
            });
        }
        let index_by_id = block
            .transactions
            .iter()
            .enumerate()
            .map(|(index, pending)| (pending.transaction_id(), index))
            .collect::<std::collections::BTreeMap<_, _>>();
        let mut transactions = Vec::with_capacity(prepared.receipts.len());
        for receipt in &prepared.receipts {
            let Some(&transaction_index) = index_by_id.get(&receipt.transaction_id) else {
                return Err(BlockExecutionError::Engine(
                    EngineError::InvalidConfiguration(format!(
                        "prepared receipt transaction ID {} is absent from the predicted block",
                        receipt.transaction_id.0
                    )),
                ));
            };
            let Some(outcome) = receipt.status.outcome() else {
                continue;
            };
            transactions.push(TransactionExecution {
                transaction_index,
                transaction_id: receipt.transaction_id,
                timing: TransactionExecutionTiming {
                    started_after_phase: receipt.execution_timing.started_after_phase,
                    completed_after_phase: receipt.execution_timing.completed_after_phase,
                    service_duration: receipt.execution_timing.service_duration,
                },
                result: Ok(ExecutionOutcome {
                    transaction_id: receipt.transaction_id,
                    contract: outcome.contract.clone(),
                    events: outcome.events.clone(),
                    data: outcome.data.clone(),
                    accesses: receipt.accesses.clone(),
                    created_contracts: outcome.created_contracts.clone(),
                }),
            });
        }
        Ok(BlockExecutionReport {
            block_height: block.context.height,
            block_time_nanos: block.context.time_nanos,
            transactions,
        })
    }

    /// Post-consensus phase: validate matching receipts in canonical order, replay stale/missing
    /// work, and commit the decided block.
    pub fn validate_prepared(
        &self,
        block: &ProducedBlock,
        prepared: PreparedSpeculativeBlock,
    ) -> Result<SplitPhaseSpeculativeExecutionReport, BlockExecutionError> {
        let canonical_transactions = canonical_transactions(block)?;
        let outcome = self
            .engine
            .reconcile_prepared_block(canonical_transactions, prepared)?;
        Ok(split_phase_report(block, outcome))
    }

    pub fn execute(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<BlockExecutionReport, BlockExecutionError> {
        let prepared = self.prepare(block, plan)?;
        Ok(self.validate_prepared(block, prepared)?.block)
    }
}

impl BlockExecutor for SpeculativeParallelBlockExecutor {
    fn execute(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<BlockExecutionReport, BlockExecutionError> {
        SpeculativeParallelBlockExecutor::execute(self, block, plan)
    }
}

fn canonical_transactions(
    block: &ProducedBlock,
) -> Result<Vec<CanonicalTransaction>, BlockExecutionError> {
    block
        .transactions
        .iter()
        .enumerate()
        .map(|(transaction_index, pending)| {
            let mut context = block.context.clone();
            context.transaction_index =
                Some(u32::try_from(transaction_index).map_err(|_| {
                    BlockExecutionError::TransactionIndexOverflow(transaction_index)
                })?);
            Ok(CanonicalTransaction::new(context, pending.request.clone()))
        })
        .collect()
}

type SplitPhaseInputs = (
    Vec<CanonicalTransaction>,
    Vec<SpeculativeWave>,
    Vec<SpeculativeDependency>,
);

fn split_phase_inputs(
    block: &ProducedBlock,
    plan: &ExecutionPlan,
) -> Result<SplitPhaseInputs, BlockExecutionError> {
    plan.validate()?;
    if plan.transaction_count != block.transactions.len() {
        return Err(BlockExecutionError::TransactionCountMismatch {
            plan: plan.transaction_count,
            block: block.transactions.len(),
        });
    }
    let canonical_transactions = canonical_transactions(block)?;
    let speculative_waves = plan
        .waves
        .iter()
        .map(|wave| {
            SpeculativeWave::new(
                wave.transaction_indices
                    .iter()
                    .map(|&transaction_index| {
                        block.transactions[transaction_index].transaction_id()
                    })
                    .collect(),
            )
        })
        .collect();
    let dependencies = plan
        .dependencies
        .iter()
        .map(|dependency| SpeculativeDependency {
            predecessor: block.transactions[dependency.predecessor_index].transaction_id(),
            successor: block.transactions[dependency.successor_index].transaction_id(),
            class: match dependency.class {
                ExecutionDependencyClass::Soft => SpeculativeDependencyClass::Soft,
                ExecutionDependencyClass::Hard => SpeculativeDependencyClass::Hard,
            },
        })
        .collect();
    Ok((canonical_transactions, speculative_waves, dependencies))
}

fn split_phase_report(
    block: &ProducedBlock,
    outcome: SplitPhaseSpeculativeBlockOutcome,
) -> SplitPhaseSpeculativeExecutionReport {
    let mut reconciliation = Vec::with_capacity(outcome.transactions.len());
    let transactions = outcome
        .transactions
        .into_iter()
        .enumerate()
        .map(|(transaction_index, result)| {
            reconciliation.push(ReconciliationTransactionDiagnostic {
                transaction_index,
                transaction_id: result.transaction_id,
                disposition: result.disposition,
                validation: result.validation.clone(),
                reexecution_duration: result.reexecution_duration,
            });
            TransactionExecution {
                transaction_index,
                transaction_id: result.transaction_id,
                timing: TransactionExecutionTiming {
                    started_after_phase: Duration::ZERO,
                    completed_after_phase: result.reexecution_duration,
                    service_duration: result.reexecution_duration,
                },
                result: result.result,
            }
        })
        .collect();
    SplitPhaseSpeculativeExecutionReport {
        block: BlockExecutionReport {
            block_height: block.context.height,
            block_time_nanos: block.context.time_nanos,
            transactions,
        },
        speculative: outcome.speculative,
        prediction: outcome.prediction,
        timings: outcome.timings,
        reconciliation,
        dependency_evidence: outcome.dependency_evidence,
    }
}

#[derive(Debug, Error)]
pub enum BlockExecutionError {
    #[error(transparent)]
    InvalidPlan(#[from] SchedulingError),
    #[error(transparent)]
    Engine(#[from] EngineError),
    #[error(
        "execution plan transaction count {plan} does not match block transaction count {block}"
    )]
    TransactionCountMismatch { plan: usize, block: usize },
    #[error(
        "wave {wave_index} has width {width}; parallel execution is disabled until speculative validation is implemented"
    )]
    ParallelWaveUnsupported { wave_index: usize, width: usize },
    #[error("transaction index {0} cannot be represented as a CosmWasm u32 index")]
    TransactionIndexOverflow(usize),
    #[error("direct DAG executor worker count must be greater than zero")]
    InvalidWorkerCount,
    #[error("failed to create direct DAG persistent worker pool: {0}")]
    WorkerPool(String),
    #[error("direct DAG replay reached a scheduler deadlock")]
    DirectDagDeadlock,
}

impl BlockExecutor for SerialBlockExecutor {
    fn execute(
        &self,
        block: &ProducedBlock,
        plan: &ExecutionPlan,
    ) -> Result<BlockExecutionReport, BlockExecutionError> {
        SerialBlockExecutor::execute(self, block, plan)
    }
}
