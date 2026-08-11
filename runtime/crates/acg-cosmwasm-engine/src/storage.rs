use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use cosmwasm_std::{Order, Record};
use cosmwasm_vm::{BackendError, BackendResult, GasInfo, Storage};

use crate::parallel::ExecutionHotPathDiagnostics;
use crate::state::SharedTx;
use crate::types::Address;

#[derive(Default)]
struct IteratorState {
    records: Vec<Record>,
    position: usize,
}

pub(crate) struct EngineStorage {
    contract: Address,
    depth: u32,
    tx: SharedTx,
    diagnostics: Option<Arc<ExecutionHotPathDiagnostics>>,
    iterators: BTreeMap<u32, IteratorState>,
    next_iterator_id: u32,
}

impl EngineStorage {
    pub fn new(contract: Address, depth: u32, tx: SharedTx) -> Self {
        let diagnostics = tx.lock().diagnostics();
        Self {
            contract,
            depth,
            tx,
            diagnostics,
            iterators: BTreeMap::new(),
            next_iterator_id: 1,
        }
    }
}

impl Storage for EngineStorage {
    fn get(&self, key: &[u8]) -> BackendResult<Option<Vec<u8>>> {
        let started = Instant::now();
        let lock_started = Instant::now();
        let mut tx = self.tx.lock();
        let lock_wait = lock_started.elapsed();
        let value = tx.storage_get(&self.contract, key, self.depth);
        drop(tx);
        if let Some(diagnostics) = &self.diagnostics {
            diagnostics.record_host_storage_get(started.elapsed(), lock_wait);
        }
        (Ok(value), GasInfo::with_externally_used(key.len() as u64))
    }

    fn scan(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        order: Order,
    ) -> BackendResult<u32> {
        let started = Instant::now();
        let lock_started = Instant::now();
        let mut tx = self.tx.lock();
        let lock_wait = lock_started.elapsed();
        let mut records = tx.storage_range(&self.contract, start, end, self.depth);
        drop(tx);
        if order == Order::Descending {
            records.reverse();
        }
        let iterator_id = self.next_iterator_id;
        self.next_iterator_id = self.next_iterator_id.saturating_add(1);
        self.iterators.insert(
            iterator_id,
            IteratorState {
                records,
                position: 0,
            },
        );
        if let Some(diagnostics) = &self.diagnostics {
            diagnostics.record_host_storage_scan(started.elapsed(), lock_wait);
        }
        (Ok(iterator_id), GasInfo::with_externally_used(11))
    }

    fn next(&mut self, iterator_id: u32) -> BackendResult<Option<Record>> {
        let started = Instant::now();
        let Some(iterator) = self.iterators.get_mut(&iterator_id) else {
            return (
                Err(BackendError::IteratorDoesNotExist { id: iterator_id }),
                GasInfo::free(),
            );
        };
        let record = iterator.records.get(iterator.position).cloned();
        if record.is_some() {
            iterator.position += 1;
        }
        let mut lock_wait = std::time::Duration::ZERO;
        if let Some((key, _)) = &record {
            let lock_started = Instant::now();
            let mut tx = self.tx.lock();
            lock_wait = lock_started.elapsed();
            let _ = tx.storage_get(&self.contract, key, self.depth);
        }
        if let Some(diagnostics) = &self.diagnostics {
            diagnostics.record_host_storage_next(started.elapsed(), lock_wait);
        }
        (Ok(record), GasInfo::with_externally_used(37))
    }

    fn set(&mut self, key: &[u8], value: &[u8]) -> BackendResult<()> {
        let started = Instant::now();
        let lock_started = Instant::now();
        let mut tx = self.tx.lock();
        let lock_wait = lock_started.elapsed();
        tx.storage_set(&self.contract, key.to_vec(), value.to_vec(), self.depth);
        drop(tx);
        if let Some(diagnostics) = &self.diagnostics {
            diagnostics.record_host_storage_set(started.elapsed(), lock_wait);
        }
        (
            Ok(()),
            GasInfo::with_externally_used((key.len() + value.len()) as u64),
        )
    }

    fn remove(&mut self, key: &[u8]) -> BackendResult<()> {
        let started = Instant::now();
        let lock_started = Instant::now();
        let mut tx = self.tx.lock();
        let lock_wait = lock_started.elapsed();
        tx.storage_remove(&self.contract, key.to_vec(), self.depth);
        drop(tx);
        if let Some(diagnostics) = &self.diagnostics {
            diagnostics.record_host_storage_remove(started.elapsed(), lock_wait);
        }
        (Ok(()), GasInfo::with_externally_used(key.len() as u64))
    }
}
