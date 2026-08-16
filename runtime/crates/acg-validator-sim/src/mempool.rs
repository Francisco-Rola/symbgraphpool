use std::collections::VecDeque;
use std::sync::Arc;

use acg_cosmwasm_engine::{ExecutionRequest, TransactionId};
use parking_lot::Mutex;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingTransaction {
    pub request: ExecutionRequest,
    pub admitted_at_nanos: u64,
    pub admission_sequence: u64,
}

impl PendingTransaction {
    pub fn transaction_id(&self) -> TransactionId {
        self.request.transaction_id()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdmissionReceipt {
    pub transaction_id: TransactionId,
    pub admission_sequence: u64,
    pub mempool_len: usize,
}

#[derive(Default)]
struct MempoolState {
    queue: VecDeque<PendingTransaction>,
    next_admission_sequence: u64,
}

/// Thread-safe FIFO mempool. Admission intentionally accepts every transaction.
#[derive(Clone, Default)]
pub struct Mempool {
    state: Arc<Mutex<MempoolState>>,
}

impl Mempool {
    pub fn admit(&self, request: ExecutionRequest, admitted_at_nanos: u64) -> AdmissionReceipt {
        let transaction_id = request.transaction_id();
        let mut state = self.state.lock();
        let admission_sequence = state.next_admission_sequence;
        state.next_admission_sequence = state.next_admission_sequence.saturating_add(1);
        state.queue.push_back(PendingTransaction {
            request,
            admitted_at_nanos,
            admission_sequence,
        });
        AdmissionReceipt {
            transaction_id,
            admission_sequence,
            mempool_len: state.queue.len(),
        }
    }

    pub fn len(&self) -> usize {
        self.state.lock().queue.len()
    }

    pub fn is_empty(&self) -> bool {
        self.state.lock().queue.is_empty()
    }

    pub fn snapshot(&self) -> Vec<PendingTransaction> {
        self.state.lock().queue.iter().cloned().collect()
    }

    /// Return up to `limit` FIFO transactions without removing them.
    ///
    /// This is the deterministic next-block prediction used by the Phase-5C.5 pre-consensus
    /// pipeline. With the default FIFO admission policy, a later `produce_next` call will select
    /// the same prefix unless the selection policy itself changes.
    pub fn peek_fifo(&self, limit: usize) -> Vec<PendingTransaction> {
        self.state
            .lock()
            .queue
            .iter()
            .take(limit)
            .cloned()
            .collect()
    }

    pub(crate) fn drain_fifo(&self, limit: usize) -> Vec<PendingTransaction> {
        let mut state = self.state.lock();
        let count = limit.min(state.queue.len());
        state.queue.drain(..count).collect()
    }
}
