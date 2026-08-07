use std::collections::VecDeque;

use acg_cosmwasm_engine::ExecutionRequest;
use thiserror::Error;

use crate::mempool::Mempool;

/// Benchmark default based on Injective's published 25,000 TPS throughput figure.
pub const DEFAULT_BENCHMARK_INGRESS_TPS: u64 = 25_000;
const NANOS_PER_SECOND: u128 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IngressConfig {
    pub transactions_per_second: u64,
}

impl Default for IngressConfig {
    fn default() -> Self {
        Self {
            transactions_per_second: DEFAULT_BENCHMARK_INGRESS_TPS,
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum IngressError {
    #[error("ingress rate must be greater than zero transactions per second")]
    ZeroRate,
}

/// Deterministic virtual-time ingress source.
///
/// Transactions are queued by a workload generator and admitted at evenly spaced virtual
/// timestamps. No wall-clock thread is required, which keeps benchmark runs reproducible.
pub struct RateControlledIngress {
    config: IngressConfig,
    epoch_nanos: u64,
    next_slot: u64,
    source: VecDeque<ExecutionRequest>,
}

impl RateControlledIngress {
    pub fn new(config: IngressConfig, epoch_nanos: u64) -> Result<Self, IngressError> {
        if config.transactions_per_second == 0 {
            return Err(IngressError::ZeroRate);
        }
        Ok(Self {
            config,
            epoch_nanos,
            // Slot zero is the source epoch itself. The first transaction arrives after one
            // configured inter-arrival interval, which yields exactly `rate * seconds` arrivals
            // over an inclusive integer-second benchmark window.
            next_slot: 1,
            source: VecDeque::new(),
        })
    }

    pub fn enqueue(&mut self, request: ExecutionRequest) {
        self.source.push_back(request);
    }

    pub fn enqueue_all(&mut self, requests: impl IntoIterator<Item = ExecutionRequest>) {
        self.source.extend(requests);
    }

    pub fn queued(&self) -> usize {
        self.source.len()
    }

    pub fn next_arrival_nanos(&self) -> Option<u64> {
        if self.source.is_empty() {
            None
        } else {
            Some(self.arrival_for_slot(self.next_slot))
        }
    }

    pub fn pump_until(&mut self, now_nanos: u64, mempool: &Mempool) -> usize {
        let mut admitted = 0;
        while !self.source.is_empty() {
            let arrival = self.arrival_for_slot(self.next_slot);
            if arrival > now_nanos {
                break;
            }
            let request = self
                .source
                .pop_front()
                .expect("source is non-empty after loop condition");
            mempool.admit(request, arrival);
            self.next_slot = self.next_slot.saturating_add(1);
            admitted += 1;
        }
        admitted
    }

    fn arrival_for_slot(&self, slot: u64) -> u64 {
        let offset = u128::from(slot).saturating_mul(NANOS_PER_SECOND)
            / u128::from(self.config.transactions_per_second);
        u128::from(self.epoch_nanos)
            .saturating_add(offset)
            .min(u128::from(u64::MAX)) as u64
    }
}
