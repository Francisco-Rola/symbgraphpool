use std::collections::BTreeSet;
use std::time::Instant;

use cosmwasm_std::Uint128;

use crate::parallel::CanonicalCommitDiagnostics;
use crate::speculative::{ReadDependency, StateWriteSet};
use crate::state::{SharedWorld, WorldState};
use crate::types::{Address, ContractMetadata};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ValidationConflict {
    ContractMetadata {
        dependency_index: usize,
        address: Address,
        expected: Option<ContractMetadata>,
        actual: Option<ContractMetadata>,
    },
    Storage {
        dependency_index: usize,
        contract: Address,
        key: Vec<u8>,
        expected: Option<Vec<u8>>,
        actual: Option<Vec<u8>>,
    },
    StorageRange {
        dependency_index: usize,
        contract: Address,
        start: Option<Vec<u8>>,
        end: Option<Vec<u8>>,
        expected: Vec<(Vec<u8>, Vec<u8>)>,
        actual: Vec<(Vec<u8>, Vec<u8>)>,
    },
    BankBalance {
        dependency_index: usize,
        address: Address,
        denom: String,
        expected: u128,
        actual: u128,
    },
    BankAllBalances {
        dependency_index: usize,
        address: Address,
        expected: Vec<(String, u128)>,
        actual: Vec<(String, u128)>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ValidationOutcome {
    conflicts: Vec<ValidationConflict>,
}

impl ValidationOutcome {
    pub fn valid() -> Self {
        Self::default()
    }

    pub fn is_valid(&self) -> bool {
        self.conflicts.is_empty()
    }

    pub fn conflicts(&self) -> &[ValidationConflict] {
        &self.conflicts
    }

    pub fn into_conflicts(self) -> Vec<ValidationConflict> {
        self.conflicts
    }
}

pub(crate) fn validate_dependencies(
    world: &SharedWorld,
    dependencies: &[ReadDependency],
) -> ValidationOutcome {
    let world = world.read();
    let mut conflicts = Vec::new();

    for (dependency_index, dependency) in dependencies.iter().enumerate() {
        match dependency {
            ReadDependency::ContractMetadata { address, metadata } => {
                let actual = world.contracts.get(address).cloned();
                if actual != *metadata {
                    conflicts.push(ValidationConflict::ContractMetadata {
                        dependency_index,
                        address: address.clone(),
                        expected: metadata.clone(),
                        actual,
                    });
                }
            }
            ReadDependency::Storage {
                contract,
                key,
                value,
            } => {
                let actual = world
                    .storage
                    .get(contract)
                    .and_then(|entries| entries.get(key).cloned());
                if actual != *value {
                    conflicts.push(ValidationConflict::Storage {
                        dependency_index,
                        contract: contract.clone(),
                        key: key.clone(),
                        expected: value.clone(),
                        actual,
                    });
                }
            }
            ReadDependency::StorageRange {
                contract,
                start,
                end,
                base_entries,
                masked_keys,
            } => {
                let masked: BTreeSet<&[u8]> = masked_keys.iter().map(Vec::as_slice).collect();
                let actual = world
                    .storage
                    .get(contract)
                    .map(|entries| {
                        entries
                            .iter()
                            .filter(|(key, _)| {
                                start
                                    .as_deref()
                                    .map_or(true, |start| key.as_slice() >= start)
                                    && end.as_deref().map_or(true, |end| key.as_slice() < end)
                                    && !masked.contains(key.as_slice())
                            })
                            .map(|(key, value)| (key.clone(), value.clone()))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                if actual != *base_entries {
                    conflicts.push(ValidationConflict::StorageRange {
                        dependency_index,
                        contract: contract.clone(),
                        start: start.clone(),
                        end: end.clone(),
                        expected: base_entries.clone(),
                        actual,
                    });
                }
            }
            ReadDependency::BankBalance {
                address,
                denom,
                amount,
            } => {
                let actual = world
                    .balances
                    .get(&(address.clone(), denom.clone()))
                    .copied()
                    .unwrap_or_default()
                    .u128();
                if actual != *amount {
                    conflicts.push(ValidationConflict::BankBalance {
                        dependency_index,
                        address: address.clone(),
                        denom: denom.clone(),
                        expected: *amount,
                        actual,
                    });
                }
            }
            ReadDependency::BankAllBalances {
                address,
                base_balances,
                masked_denoms,
            } => {
                let masked: BTreeSet<&str> = masked_denoms.iter().map(String::as_str).collect();
                let actual = world
                    .balances
                    .iter()
                    .filter(|((owner, denom), amount)| {
                        owner == address && !amount.is_zero() && !masked.contains(denom.as_str())
                    })
                    .map(|((_, denom), amount)| (denom.clone(), amount.u128()))
                    .collect::<Vec<_>>();
                if actual != *base_balances {
                    conflicts.push(ValidationConflict::BankAllBalances {
                        dependency_index,
                        address: address.clone(),
                        expected: base_balances.clone(),
                        actual,
                    });
                }
            }
        }
    }

    ValidationOutcome { conflicts }
}

pub(crate) fn write_set_touches_conflict(
    write_set: &StateWriteSet,
    conflict: &ValidationConflict,
) -> bool {
    match conflict {
        ValidationConflict::ContractMetadata { address, .. } => write_set
            .created_contracts
            .iter()
            .any(|metadata| &metadata.address == address),
        ValidationConflict::Storage { contract, key, .. } => write_set
            .storage
            .iter()
            .any(|write| &write.contract == contract && &write.key == key),
        ValidationConflict::StorageRange {
            contract,
            expected,
            actual,
            ..
        } => write_set.storage.iter().any(|write| {
            &write.contract == contract
                && range_conflict_changed_key(expected, actual, write.key.as_slice())
        }),
        ValidationConflict::BankBalance { address, denom, .. } => write_set
            .balances
            .iter()
            .any(|write| &write.address == address && &write.denom == denom),
        ValidationConflict::BankAllBalances {
            address,
            expected,
            actual,
            ..
        } => write_set.balances.iter().any(|write| {
            &write.address == address
                && balance_conflict_changed_denom(expected, actual, write.denom.as_str())
        }),
    }
}

fn range_conflict_changed_key(
    expected: &[(Vec<u8>, Vec<u8>)],
    actual: &[(Vec<u8>, Vec<u8>)],
    key: &[u8],
) -> bool {
    let expected_value = expected
        .iter()
        .find(|(candidate, _)| candidate.as_slice() == key)
        .map(|(_, value)| value.as_slice());
    let actual_value = actual
        .iter()
        .find(|(candidate, _)| candidate.as_slice() == key)
        .map(|(_, value)| value.as_slice());
    expected_value != actual_value
}

fn balance_conflict_changed_denom(
    expected: &[(String, u128)],
    actual: &[(String, u128)],
    denom: &str,
) -> bool {
    let expected_amount = expected
        .iter()
        .find(|(candidate, _)| candidate == denom)
        .map(|(_, amount)| *amount)
        .unwrap_or_default();
    let actual_amount = actual
        .iter()
        .find(|(candidate, _)| candidate == denom)
        .map(|(_, amount)| *amount)
        .unwrap_or_default();
    expected_amount != actual_amount
}

pub(crate) fn apply_write_set(world: &SharedWorld, write_set: &StateWriteSet) {
    apply_write_sets(world, std::slice::from_ref(write_set));
}

/// Apply several already-ordered write sets while holding the canonical world write lock once.
///
/// Exact-DAG replay uses this to keep worker threads entirely on the read/compute side of the
/// canonical state and let one coordinator perform short batched commits. This preserves canonical
/// transaction order while avoiding N workers repeatedly contending for the same world-state
/// writer lock.
pub(crate) fn apply_write_sets(world: &SharedWorld, write_sets: &[StateWriteSet]) {
    let mut world = world.write();
    apply_write_sets_locked(&mut world, write_sets);
}

pub(crate) fn apply_write_sets_with_diagnostics(
    world: &SharedWorld,
    write_sets: &[StateWriteSet],
) -> CanonicalCommitDiagnostics {
    let wait_started = Instant::now();
    let mut world = world.write();
    let lock_wait = wait_started.elapsed();
    let hold_started = Instant::now();
    let mut diagnostics = CanonicalCommitDiagnostics {
        lock_wait,
        batches: if write_sets.is_empty() { 0 } else { 1 },
        write_sets: u64::try_from(write_sets.len()).unwrap_or(u64::MAX),
        storage_writes: write_sets.iter().fold(0_u64, |total, write_set| {
            total.saturating_add(u64::try_from(write_set.storage.len()).unwrap_or(u64::MAX))
        }),
        balance_writes: write_sets.iter().fold(0_u64, |total, write_set| {
            total.saturating_add(u64::try_from(write_set.balances.len()).unwrap_or(u64::MAX))
        }),
        created_contracts: write_sets.iter().fold(0_u64, |total, write_set| {
            total.saturating_add(
                u64::try_from(write_set.created_contracts.len()).unwrap_or(u64::MAX),
            )
        }),
        ..CanonicalCommitDiagnostics::default()
    };
    apply_write_sets_locked(&mut world, write_sets);
    diagnostics.lock_hold = hold_started.elapsed();
    drop(world);
    diagnostics
}

fn apply_write_sets_locked(world: &mut WorldState, write_sets: &[StateWriteSet]) {
    for write_set in write_sets {
        for metadata in &write_set.created_contracts {
            world
                .contracts
                .insert(metadata.address.clone(), metadata.clone());
            world.storage.entry(metadata.address.clone()).or_default();
        }

        for write in &write_set.storage {
            let storage = world.storage.entry(write.contract.clone()).or_default();
            match &write.value {
                Some(value) => {
                    storage.insert(write.key.clone(), value.clone());
                }
                None => {
                    storage.remove(&write.key);
                }
            }
        }

        for write in &write_set.balances {
            let key = (write.address.clone(), write.denom.clone());
            if write.amount == 0 {
                world.balances.remove(&key);
            } else {
                world.balances.insert(key, Uint128::new(write.amount));
            }
        }
    }
}
