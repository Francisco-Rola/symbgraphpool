use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use cosmwasm_std::{Order, Record};
use cosmwasm_vm::{BackendError, BackendResult, GasInfo, Storage};
use parking_lot::Mutex;

use crate::parallel::ExecutionHotPathDiagnostics;
use crate::state::SharedTx;
use crate::types::Address;

#[derive(Default)]
struct IteratorState {
    records: Vec<Record>,
    position: usize,
}

struct EngineStorageContext {
    contract: Address,
    depth: u32,
    tx: SharedTx,
    diagnostics: Option<Arc<ExecutionHotPathDiagnostics>>,
    iterators: BTreeMap<u32, IteratorState>,
    next_iterator_id: u32,
}

#[derive(Clone)]
pub(crate) struct EngineStorageBinding {
    inner: Arc<Mutex<EngineStorageContext>>,
}

impl EngineStorageBinding {
    pub(crate) fn rebind(&self, contract: Address, depth: u32, tx: SharedTx) {
        let diagnostics = tx.lock().diagnostics();
        let mut context = self.inner.lock();
        context.contract = contract;
        context.depth = depth;
        context.tx = tx;
        context.diagnostics = diagnostics;
        context.iterators.clear();
        context.next_iterator_id = 1;
    }
}

pub(crate) struct EngineStorage {
    binding: EngineStorageBinding,
}

impl EngineStorage {
    pub fn new(contract: Address, depth: u32, tx: SharedTx) -> Self {
        Self::rebindable(contract, depth, tx).0
    }

    pub(crate) fn rebindable(
        contract: Address,
        depth: u32,
        tx: SharedTx,
    ) -> (Self, EngineStorageBinding) {
        let diagnostics = tx.lock().diagnostics();
        let binding = EngineStorageBinding {
            inner: Arc::new(Mutex::new(EngineStorageContext {
                contract,
                depth,
                tx,
                diagnostics,
                iterators: BTreeMap::new(),
                next_iterator_id: 1,
            })),
        };
        (
            Self {
                binding: binding.clone(),
            },
            binding,
        )
    }
}

impl Storage for EngineStorage {
    fn get(&self, key: &[u8]) -> BackendResult<Option<Vec<u8>>> {
        let started = Instant::now();
        let context = self.binding.inner.lock();
        let lock_started = Instant::now();
        let mut tx = context.tx.lock();
        let lock_wait = lock_started.elapsed();
        let value = tx.storage_get(&context.contract, key, context.depth);
        drop(tx);
        if let Some(diagnostics) = &context.diagnostics {
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
        let mut context = self.binding.inner.lock();
        let lock_started = Instant::now();
        let mut tx = context.tx.lock();
        let lock_wait = lock_started.elapsed();
        let mut records = tx.storage_range(&context.contract, start, end, context.depth);
        drop(tx);
        if order == Order::Descending {
            records.reverse();
        }
        let iterator_id = context.next_iterator_id;
        context.next_iterator_id = context.next_iterator_id.saturating_add(1);
        context.iterators.insert(
            iterator_id,
            IteratorState {
                records,
                position: 0,
            },
        );
        if let Some(diagnostics) = &context.diagnostics {
            diagnostics.record_host_storage_scan(started.elapsed(), lock_wait);
        }
        (Ok(iterator_id), GasInfo::with_externally_used(11))
    }

    fn next(&mut self, iterator_id: u32) -> BackendResult<Option<Record>> {
        let started = Instant::now();
        let mut context = self.binding.inner.lock();
        let Some(iterator) = context.iterators.get_mut(&iterator_id) else {
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
            let mut tx = context.tx.lock();
            lock_wait = lock_started.elapsed();
            let _ = tx.storage_get(&context.contract, key, context.depth);
        }
        if let Some(diagnostics) = &context.diagnostics {
            diagnostics.record_host_storage_next(started.elapsed(), lock_wait);
        }
        (Ok(record), GasInfo::with_externally_used(37))
    }

    fn set(&mut self, key: &[u8], value: &[u8]) -> BackendResult<()> {
        let started = Instant::now();
        let context = self.binding.inner.lock();
        let lock_started = Instant::now();
        let mut tx = context.tx.lock();
        let lock_wait = lock_started.elapsed();
        tx.storage_set(
            &context.contract,
            key.to_vec(),
            value.to_vec(),
            context.depth,
        );
        drop(tx);
        if let Some(diagnostics) = &context.diagnostics {
            diagnostics.record_host_storage_set(started.elapsed(), lock_wait);
        }
        (
            Ok(()),
            GasInfo::with_externally_used((key.len() + value.len()) as u64),
        )
    }

    fn remove(&mut self, key: &[u8]) -> BackendResult<()> {
        let started = Instant::now();
        let context = self.binding.inner.lock();
        let lock_started = Instant::now();
        let mut tx = context.tx.lock();
        let lock_wait = lock_started.elapsed();
        tx.storage_remove(&context.contract, key.to_vec(), context.depth);
        drop(tx);
        if let Some(diagnostics) = &context.diagnostics {
            diagnostics.record_host_storage_remove(started.elapsed(), lock_wait);
        }
        (Ok(()), GasInfo::with_externally_used(key.len() as u64))
    }
}
