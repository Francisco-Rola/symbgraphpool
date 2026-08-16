//! Deterministic single-validator runtime simulation for benchmarks.
//!
//! Networking and consensus are deliberately modeled as timing/admission components rather than
//! real protocols. All submitted transactions are accepted, block production is FIFO by default,
//! while split-phase Phase-5 execution can use dependency-driven speculative pre-execution with
//! canonical validation/replay as the correctness boundary.

mod block;
mod executor;
mod ingress;
mod mempool;
mod pipeline;
mod scheduler;

pub use crate::block::{
    BlockProducer, BlockProducerConfig, BlockProducerError, BlockSelectionPolicy,
    FifoSelectionPolicy, ProducedBlock, ReverseFifoSelectionPolicy, SeededShuffleSelectionPolicy,
};
pub use crate::executor::{
    BlockExecutionError, BlockExecutionReport, BlockExecutor, ReconciliationTransactionDiagnostic,
    SerialBlockExecutor, SpeculativeParallelBlockExecutor, SplitPhaseSpeculativeExecutionReport,
    TransactionExecution, TransactionExecutionTiming,
};
pub use crate::ingress::{
    IngressConfig, IngressError, RateControlledIngress, DEFAULT_BENCHMARK_INGRESS_TPS,
};
pub use crate::mempool::{AdmissionReceipt, Mempool, PendingTransaction};
pub use crate::pipeline::{PipelineError, SingleValidatorRuntime};
pub use crate::scheduler::{
    BlockScheduler, ExecutionDependency, ExecutionDependencyClass, ExecutionPlan, ExecutionWave,
    FifoScheduler, SchedulingError,
};
