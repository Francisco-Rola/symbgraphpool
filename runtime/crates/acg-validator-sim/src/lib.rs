//! Deterministic single-validator runtime simulation for benchmarks.
//!
//! Networking and consensus are deliberately modeled as timing/admission components rather than
//! real protocols. All submitted transactions are accepted, block production is FIFO by default,
//! and execution is serial until speculative validation is introduced.

mod block;
mod executor;
mod ingress;
mod mempool;
mod pipeline;
mod scheduler;

pub use crate::block::{
    BlockProducer, BlockProducerConfig, BlockProducerError, BlockSelectionPolicy,
    FifoSelectionPolicy, ProducedBlock,
};
pub use crate::executor::{
    BlockExecutionError, BlockExecutionReport, BlockExecutor, SerialBlockExecutor,
    TransactionExecution,
};
pub use crate::ingress::{
    IngressConfig, IngressError, RateControlledIngress, DEFAULT_BENCHMARK_INGRESS_TPS,
};
pub use crate::mempool::{AdmissionReceipt, Mempool, PendingTransaction};
pub use crate::pipeline::{PipelineError, SingleValidatorRuntime};
pub use crate::scheduler::{
    BlockScheduler, ExecutionPlan, ExecutionWave, FifoScheduler, SchedulingError,
};
