use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use cosmwasm_std::Uint128;
use parking_lot::RwLock;

use crate::parallel::ExecutionHotPathDiagnostics;
use crate::speculative::StateWriteSet;
use crate::state::SharedWorld;
use crate::types::{Address, ContractMetadata};

/// Compact launch-time snapshot of which speculative transaction versions are visible.
///
/// Visibility is frozen when a transaction becomes runnable. Versions published after launch are
/// intentionally invisible to that transaction, preserving repeatable speculative reads even when
/// independent earlier-canonical transactions complete concurrently.
#[derive(Clone, Debug)]
pub(crate) struct VisibilityMask {
    words: Arc<[u64]>,
}

impl VisibilityMask {
    pub fn from_completed(completed: &[u64], canonical_index: usize) -> Self {
        let required_words = canonical_index.div_ceil(64);
        let mut words = completed[..required_words.min(completed.len())].to_vec();
        if canonical_index % 64 != 0 {
            if let Some(last) = words.last_mut() {
                *last &= (1_u64 << (canonical_index % 64)) - 1;
            }
        }
        Self {
            words: words.into(),
        }
    }

    pub fn contains(&self, transaction_index: usize) -> bool {
        let word = transaction_index / 64;
        let bit = transaction_index % 64;
        self.words
            .get(word)
            .is_some_and(|value| value & (1_u64 << bit) != 0)
    }

    pub fn word_count(&self) -> usize {
        self.words.len()
    }
}

type StorageVersions = BTreeMap<Address, BTreeMap<Vec<u8>, BTreeMap<usize, Option<Vec<u8>>>>>;
type BalanceVersions = BTreeMap<Address, BTreeMap<String, BTreeMap<usize, Uint128>>>;
type ContractVersions = BTreeMap<Address, BTreeMap<usize, ContractMetadata>>;

/// Block-local multi-version speculative state.
///
/// The committed predecessor snapshot is shared by all transactions. Successful speculative
/// receipts publish only their detached deltas here, keyed by canonical transaction index. Reads
/// resolve the newest launch-visible version older than the reader and otherwise fall through to
/// the immutable block base. No transaction deep-copies the world or replays predecessor write
/// sets to construct a snapshot.
#[derive(Default)]
pub(crate) struct BlockMvccState {
    storage: RwLock<StorageVersions>,
    balances: RwLock<BalanceVersions>,
    contracts: RwLock<ContractVersions>,
}

impl BlockMvccState {
    #[cfg(test)]
    pub fn publish(&self, transaction_index: usize, write_set: &StateWriteSet) {
        self.publish_inner(transaction_index, write_set, None);
    }

    pub fn publish_with_diagnostics(
        &self,
        transaction_index: usize,
        write_set: &StateWriteSet,
        diagnostics: &ExecutionHotPathDiagnostics,
    ) {
        self.publish_inner(transaction_index, write_set, Some(diagnostics));
    }

    fn publish_inner(
        &self,
        transaction_index: usize,
        write_set: &StateWriteSet,
        diagnostics: Option<&ExecutionHotPathDiagnostics>,
    ) {
        if !write_set.storage.is_empty() {
            let lock_started = Instant::now();
            let mut storage = self.storage.write();
            if let Some(diagnostics) = diagnostics {
                diagnostics.record_mvcc_lock_wait(lock_started.elapsed());
            }
            for write in &write_set.storage {
                storage
                    .entry(write.contract.clone())
                    .or_default()
                    .entry(write.key.clone())
                    .or_default()
                    .insert(transaction_index, write.value.clone());
            }
        }
        if !write_set.balances.is_empty() {
            let lock_started = Instant::now();
            let mut balances = self.balances.write();
            if let Some(diagnostics) = diagnostics {
                diagnostics.record_mvcc_lock_wait(lock_started.elapsed());
            }
            for write in &write_set.balances {
                balances
                    .entry(write.address.clone())
                    .or_default()
                    .entry(write.denom.clone())
                    .or_default()
                    .insert(transaction_index, Uint128::new(write.amount));
            }
        }
        if !write_set.created_contracts.is_empty() {
            let lock_started = Instant::now();
            let mut contracts = self.contracts.write();
            if let Some(diagnostics) = diagnostics {
                diagnostics.record_mvcc_lock_wait(lock_started.elapsed());
            }
            for metadata in &write_set.created_contracts {
                contracts
                    .entry(metadata.address.clone())
                    .or_default()
                    .insert(transaction_index, metadata.clone());
            }
        }
    }

    fn storage_value(
        &self,
        contract: &Address,
        key: &[u8],
        canonical_index: usize,
        visibility: &VisibilityMask,
        diagnostics: &ExecutionHotPathDiagnostics,
    ) -> Option<Option<Vec<u8>>> {
        let lock_started = Instant::now();
        let storage = self.storage.read();
        diagnostics.record_mvcc_lock_wait(lock_started.elapsed());
        let versions = storage.get(contract)?.get(key)?;
        newest_visible(versions, canonical_index, visibility).cloned()
    }

    fn contract_metadata(
        &self,
        address: &Address,
        canonical_index: usize,
        visibility: &VisibilityMask,
        diagnostics: &ExecutionHotPathDiagnostics,
    ) -> Option<ContractMetadata> {
        let lock_started = Instant::now();
        let contracts = self.contracts.read();
        diagnostics.record_mvcc_lock_wait(lock_started.elapsed());
        let versions = contracts.get(address)?;
        newest_visible(versions, canonical_index, visibility).cloned()
    }

    fn balance_value(
        &self,
        address: &Address,
        denom: &str,
        canonical_index: usize,
        visibility: &VisibilityMask,
        diagnostics: &ExecutionHotPathDiagnostics,
    ) -> Option<Uint128> {
        let lock_started = Instant::now();
        let balances = self.balances.read();
        diagnostics.record_mvcc_lock_wait(lock_started.elapsed());
        let versions = balances.get(address)?.get(denom)?;
        newest_visible(versions, canonical_index, visibility).copied()
    }

    fn storage_overlay(
        &self,
        contract: &Address,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        canonical_index: usize,
        visibility: &VisibilityMask,
        diagnostics: &ExecutionHotPathDiagnostics,
    ) -> BTreeMap<Vec<u8>, Option<Vec<u8>>> {
        let lock_started = Instant::now();
        let storage = self.storage.read();
        diagnostics.record_mvcc_lock_wait(lock_started.elapsed());
        let Some(keys) = storage.get(contract) else {
            return BTreeMap::new();
        };
        keys.iter()
            .filter(|(key, _)| {
                start.map_or(true, |start| key.as_slice() >= start)
                    && end.map_or(true, |end| key.as_slice() < end)
            })
            .filter_map(|(key, versions)| {
                newest_visible(versions, canonical_index, visibility)
                    .cloned()
                    .map(|value| (key.clone(), value))
            })
            .collect()
    }

    fn balance_overlay(
        &self,
        address: &Address,
        canonical_index: usize,
        visibility: &VisibilityMask,
        diagnostics: &ExecutionHotPathDiagnostics,
    ) -> BTreeMap<String, Uint128> {
        let lock_started = Instant::now();
        let balances = self.balances.read();
        diagnostics.record_mvcc_lock_wait(lock_started.elapsed());
        let Some(denoms) = balances.get(address) else {
            return BTreeMap::new();
        };
        denoms
            .iter()
            .filter_map(|(denom, versions)| {
                newest_visible(versions, canonical_index, visibility)
                    .copied()
                    .map(|value| (denom.clone(), value))
            })
            .collect()
    }
}

fn newest_visible<'a, T>(
    versions: &'a BTreeMap<usize, T>,
    canonical_index: usize,
    visibility: &VisibilityMask,
) -> Option<&'a T> {
    versions
        .range(..canonical_index)
        .rev()
        .find_map(|(index, value)| visibility.contains(*index).then_some(value))
}

/// Immutable transaction read view over one block-local MVCC state.
#[derive(Clone)]
pub(crate) struct MvccReadView {
    base: SharedWorld,
    versions: Arc<BlockMvccState>,
    canonical_index: usize,
    visibility: VisibilityMask,
    diagnostics: Arc<ExecutionHotPathDiagnostics>,
}

impl MvccReadView {
    pub fn new(
        base: SharedWorld,
        versions: Arc<BlockMvccState>,
        canonical_index: usize,
        visibility: VisibilityMask,
        diagnostics: Arc<ExecutionHotPathDiagnostics>,
    ) -> Self {
        Self {
            base,
            versions,
            canonical_index,
            visibility,
            diagnostics,
        }
    }

    pub(crate) fn diagnostics(&self) -> Arc<ExecutionHotPathDiagnostics> {
        self.diagnostics.clone()
    }

    pub fn contract(&self, address: &Address) -> Option<ContractMetadata> {
        let started = Instant::now();
        let result = self
            .versions
            .contract_metadata(
                address,
                self.canonical_index,
                &self.visibility,
                &self.diagnostics,
            )
            .or_else(|| self.base.read().contracts.get(address).cloned());
        self.diagnostics.record_mvcc_contract(started.elapsed());
        result
    }

    pub fn storage_get(&self, contract: &Address, key: &[u8]) -> Option<Vec<u8>> {
        let started = Instant::now();
        let version = self.versions.storage_value(
            contract,
            key,
            self.canonical_index,
            &self.visibility,
            &self.diagnostics,
        );
        let version_hit = version.is_some();
        let base_fallback = !version_hit;
        let result = match version {
            Some(value) => value,
            None => self
                .base
                .read()
                .storage
                .get(contract)
                .and_then(|entries| entries.get(key).cloned()),
        };
        self.diagnostics
            .record_mvcc_storage_point(started.elapsed(), version_hit, base_fallback);
        result
    }

    pub fn storage_range(
        &self,
        contract: &Address,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> BTreeMap<Vec<u8>, Vec<u8>> {
        let started = Instant::now();
        let mut merged = self
            .base
            .read()
            .storage
            .get(contract)
            .map(|entries| {
                entries
                    .iter()
                    .filter(|(key, _)| {
                        start.map_or(true, |start| key.as_slice() >= start)
                            && end.map_or(true, |end| key.as_slice() < end)
                    })
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        for (key, value) in self.versions.storage_overlay(
            contract,
            start,
            end,
            self.canonical_index,
            &self.visibility,
            &self.diagnostics,
        ) {
            match value {
                Some(value) => {
                    merged.insert(key, value);
                }
                None => {
                    merged.remove(&key);
                }
            }
        }
        self.diagnostics
            .record_mvcc_storage_range(started.elapsed());
        merged
    }

    pub fn balance(&self, address: &Address, denom: &str) -> Uint128 {
        let started = Instant::now();
        let result = self
            .versions
            .balance_value(
                address,
                denom,
                self.canonical_index,
                &self.visibility,
                &self.diagnostics,
            )
            .unwrap_or_else(|| {
                self.base
                    .read()
                    .balances
                    .get(&(address.clone(), denom.to_owned()))
                    .copied()
                    .unwrap_or_default()
            });
        self.diagnostics.record_mvcc_balance(started.elapsed());
        result
    }

    pub fn balances(&self, address: &Address) -> BTreeMap<String, Uint128> {
        let started = Instant::now();
        let mut merged = self
            .base
            .read()
            .balances
            .iter()
            .filter(|((owner, _), _)| owner == address)
            .map(|((_, denom), amount)| (denom.clone(), *amount))
            .collect::<BTreeMap<_, _>>();
        for (denom, amount) in self.versions.balance_overlay(
            address,
            self.canonical_index,
            &self.visibility,
            &self.diagnostics,
        ) {
            if amount.is_zero() {
                merged.remove(&denom);
            } else {
                merged.insert(denom, amount);
            }
        }
        self.diagnostics.record_mvcc_all_balances(started.elapsed());
        merged
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use cosmwasm_std::Uint128;
    use parking_lot::RwLock;

    use super::{BlockMvccState, MvccReadView, VisibilityMask};
    use crate::parallel::ExecutionHotPathDiagnostics;
    use crate::speculative::{BalanceWrite, StateWriteSet, StorageWrite};
    use crate::state::WorldState;
    use crate::types::{Address, CodeChecksum, CodeId, ContractMetadata};

    fn base_world() -> Arc<RwLock<WorldState>> {
        let contract = Address::from("contract");
        let owner = Address::from("alice");
        let mut world = WorldState::default();
        world
            .storage
            .entry(contract)
            .or_default()
            .insert(b"a".to_vec(), b"base-a".to_vec());
        world
            .balances
            .insert((owner, "utest".to_owned()), Uint128::new(5));
        Arc::new(RwLock::new(world))
    }

    #[test]
    fn visibility_mask_is_frozen_at_launch() {
        let mut completed = vec![0_u64; 1];
        completed[0] |= 1 << 1;
        let mask = VisibilityMask::from_completed(&completed, 4);
        completed[0] |= 1 << 2;

        assert!(mask.contains(1));
        assert!(!mask.contains(2));
        assert!(!mask.contains(4));
    }

    #[test]
    fn point_read_uses_newest_visible_earlier_version_and_hides_future_version() {
        let contract = Address::from("contract");
        let versions = Arc::new(BlockMvccState::default());
        versions.publish(
            1,
            &StateWriteSet {
                storage: vec![StorageWrite {
                    contract: contract.clone(),
                    key: b"a".to_vec(),
                    value: Some(b"v1".to_vec()),
                }],
                ..StateWriteSet::default()
            },
        );
        versions.publish(
            5,
            &StateWriteSet {
                storage: vec![StorageWrite {
                    contract: contract.clone(),
                    key: b"a".to_vec(),
                    value: Some(b"future".to_vec()),
                }],
                ..StateWriteSet::default()
            },
        );

        let completed = vec![(1_u64 << 1) | (1_u64 << 5)];
        let view = MvccReadView::new(
            base_world(),
            versions,
            4,
            VisibilityMask::from_completed(&completed, 4),
            Arc::new(ExecutionHotPathDiagnostics::default()),
        );

        assert_eq!(view.storage_get(&contract, b"a"), Some(b"v1".to_vec()));
    }

    #[test]
    fn range_read_merges_visible_insert_update_and_delete_without_materializing_history() {
        let contract = Address::from("contract");
        let versions = Arc::new(BlockMvccState::default());
        versions.publish(
            1,
            &StateWriteSet {
                storage: vec![
                    StorageWrite {
                        contract: contract.clone(),
                        key: b"a".to_vec(),
                        value: None,
                    },
                    StorageWrite {
                        contract: contract.clone(),
                        key: b"b".to_vec(),
                        value: Some(b"v1-b".to_vec()),
                    },
                ],
                ..StateWriteSet::default()
            },
        );
        let completed = vec![1_u64 << 1];
        let view = MvccReadView::new(
            base_world(),
            versions,
            3,
            VisibilityMask::from_completed(&completed, 3),
            Arc::new(ExecutionHotPathDiagnostics::default()),
        );

        let range = view.storage_range(&contract, None, None);
        assert!(!range.contains_key(&b"a"[..]));
        assert_eq!(range.get(&b"b"[..]).map(Vec::as_slice), Some(&b"v1-b"[..]));
    }

    #[test]
    fn contract_metadata_version_obeys_launch_visibility() {
        let address = Address::from("created");
        let versions = Arc::new(BlockMvccState::default());
        let metadata = ContractMetadata {
            address: address.clone(),
            code_id: CodeId(7),
            code_checksum: CodeChecksum::new([3_u8; 32]),
            creator: Address::from("alice"),
            admin: None,
            label: "created".to_owned(),
        };
        versions.publish(
            1,
            &StateWriteSet {
                created_contracts: vec![metadata.clone()],
                ..StateWriteSet::default()
            },
        );

        let visible = MvccReadView::new(
            base_world(),
            versions.clone(),
            3,
            VisibilityMask::from_completed(&[1_u64 << 1], 3),
            Arc::new(ExecutionHotPathDiagnostics::default()),
        );
        let hidden = MvccReadView::new(
            base_world(),
            versions,
            3,
            VisibilityMask::from_completed(&[0], 3),
            Arc::new(ExecutionHotPathDiagnostics::default()),
        );

        assert_eq!(visible.contract(&address), Some(metadata));
        assert_eq!(hidden.contract(&address), None);
    }

    #[test]
    fn balance_read_uses_visible_version_and_zero_removes_from_all_balances() {
        let owner = Address::from("alice");
        let versions = Arc::new(BlockMvccState::default());
        versions.publish(
            1,
            &StateWriteSet {
                balances: vec![BalanceWrite {
                    address: owner.clone(),
                    denom: "utest".to_owned(),
                    amount: 0,
                }],
                ..StateWriteSet::default()
            },
        );
        let completed = vec![1_u64 << 1];
        let view = MvccReadView::new(
            base_world(),
            versions,
            3,
            VisibilityMask::from_completed(&completed, 3),
            Arc::new(ExecutionHotPathDiagnostics::default()),
        );

        assert_eq!(view.balance(&owner, "utest"), Uint128::zero());
        assert!(view.balances(&owner).is_empty());
    }
    #[test]
    fn mvcc_read_diagnostics_distinguish_version_hits_from_base_fallbacks() {
        let contract = Address::from("contract");
        let versions = Arc::new(BlockMvccState::default());
        versions.publish(
            1,
            &StateWriteSet {
                storage: vec![StorageWrite {
                    contract: contract.clone(),
                    key: b"versioned".to_vec(),
                    value: Some(b"mvcc".to_vec()),
                }],
                ..StateWriteSet::default()
            },
        );
        let diagnostics = Arc::new(ExecutionHotPathDiagnostics::default());
        let view = MvccReadView::new(
            base_world(),
            versions,
            3,
            VisibilityMask::from_completed(&[1_u64 << 1], 3),
            diagnostics.clone(),
        );

        assert_eq!(
            view.storage_get(&contract, b"versioned"),
            Some(b"mvcc".to_vec())
        );
        assert_eq!(view.storage_get(&contract, b"a"), Some(b"base-a".to_vec()));

        let observed = diagnostics.snapshot();
        assert_eq!(observed.mvcc_storage_point_reads, 2);
        assert_eq!(observed.mvcc_storage_point_hits, 1);
        assert_eq!(observed.mvcc_storage_base_fallbacks, 1);
    }
}
