use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use cosmwasm_std::{Coin, Uint128};
use parking_lot::{Mutex, RwLock};

use crate::error::{EngineError, EngineResult};
use crate::mvcc::MvccReadView;
use crate::parallel::ExecutionHotPathDiagnostics;
use crate::speculative::{BalanceWrite, ReadDependency, StateWriteSet, StorageWrite};
use crate::types::{AccessKind, AccessRecord, Address, CodeId, ContractMetadata, TransactionId};

#[derive(Clone, Default, PartialEq, Eq)]
pub(crate) struct WorldState {
    pub contracts: BTreeMap<Address, ContractMetadata>,
    pub storage: BTreeMap<Address, BTreeMap<Vec<u8>, Vec<u8>>>,
    pub balances: BTreeMap<(Address, String), Uint128>,
}

pub(crate) type SharedWorld = Arc<RwLock<WorldState>>;
pub(crate) type SharedTx = Arc<Mutex<TransactionState>>;

#[derive(Clone)]
pub(crate) struct TransactionState {
    pub base: SharedWorld,
    pub mvcc_view: Option<MvccReadView>,
    diagnostics: Option<Arc<ExecutionHotPathDiagnostics>>,
    pub transaction_id: TransactionId,
    pub storage_writes: BTreeMap<(Address, Vec<u8>), Option<Vec<u8>>>,
    pub balance_writes: BTreeMap<(Address, String), Uint128>,
    pub created_contracts: BTreeMap<Address, ContractMetadata>,
    pub accesses: Vec<AccessRecord>,
    pub read_dependencies: Vec<ReadDependency>,
    next_instance_ordinal: u32,
}

impl TransactionState {
    pub fn new(base: SharedWorld, transaction_id: TransactionId) -> Self {
        Self {
            base,
            mvcc_view: None,
            diagnostics: None,
            transaction_id,
            storage_writes: BTreeMap::new(),
            balance_writes: BTreeMap::new(),
            created_contracts: BTreeMap::new(),
            accesses: Vec::new(),
            read_dependencies: Vec::new(),
            next_instance_ordinal: 0,
        }
    }

    pub fn new_mvcc(
        base: SharedWorld,
        mvcc_view: MvccReadView,
        transaction_id: TransactionId,
    ) -> Self {
        let diagnostics = mvcc_view.diagnostics();
        let mut state = Self::new(base, transaction_id);
        state.mvcc_view = Some(mvcc_view);
        state.diagnostics = Some(diagnostics);
        state
    }

    pub(crate) fn diagnostics(&self) -> Option<Arc<ExecutionHotPathDiagnostics>> {
        self.diagnostics.clone()
    }

    fn base_contract(&self, address: &Address) -> Option<ContractMetadata> {
        self.mvcc_view.as_ref().map_or_else(
            || self.base.read().contracts.get(address).cloned(),
            |view| view.contract(address),
        )
    }

    fn base_storage_get(&self, contract: &Address, key: &[u8]) -> Option<Vec<u8>> {
        self.mvcc_view.as_ref().map_or_else(
            || {
                self.base
                    .read()
                    .storage
                    .get(contract)
                    .and_then(|entries| entries.get(key).cloned())
            },
            |view| view.storage_get(contract, key),
        )
    }

    fn base_storage_range(
        &self,
        contract: &Address,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> BTreeMap<Vec<u8>, Vec<u8>> {
        self.mvcc_view.as_ref().map_or_else(
            || {
                self.base
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
                            .collect()
                    })
                    .unwrap_or_default()
            },
            |view| view.storage_range(contract, start, end),
        )
    }

    fn base_balance(&self, address: &Address, denom: &str) -> Uint128 {
        self.mvcc_view.as_ref().map_or_else(
            || {
                self.base
                    .read()
                    .balances
                    .get(&(address.clone(), denom.to_owned()))
                    .copied()
                    .unwrap_or_default()
            },
            |view| view.balance(address, denom),
        )
    }

    fn base_balances(&self, address: &Address) -> BTreeMap<String, Uint128> {
        self.mvcc_view.as_ref().map_or_else(
            || {
                self.base
                    .read()
                    .balances
                    .iter()
                    .filter(|((owner, _), _)| owner == address)
                    .map(|((_, denom), amount)| (denom.clone(), *amount))
                    .collect()
            },
            |view| view.balances(address),
        )
    }

    pub fn allocate_contract_address(&mut self, prefix: &str) -> EngineResult<Address> {
        let ordinal = self.next_instance_ordinal;
        self.next_instance_ordinal = self
            .next_instance_ordinal
            .checked_add(1)
            .ok_or_else(|| EngineError::Internal("contract address ordinal overflow".to_owned()))?;
        Ok(Address::new(format!(
            "{prefix}-{}-{ordinal}",
            self.transaction_id.0
        )))
    }

    pub fn contract(&mut self, address: &Address) -> Option<ContractMetadata> {
        if let Some(metadata) = self.created_contracts.get(address).cloned() {
            return Some(metadata);
        }
        let metadata = self.base_contract(address);
        self.read_dependencies
            .push(ReadDependency::ContractMetadata {
                address: address.clone(),
                metadata: metadata.clone(),
            });
        metadata
    }

    pub fn create_contract(&mut self, metadata: ContractMetadata) -> EngineResult<()> {
        if self.contract(&metadata.address).is_some() {
            return Err(EngineError::ContractAlreadyExists(metadata.address));
        }
        self.created_contracts
            .insert(metadata.address.clone(), metadata);
        Ok(())
    }

    pub fn storage_get(&mut self, contract: &Address, key: &[u8], depth: u32) -> Option<Vec<u8>> {
        let overlay_key = (contract.clone(), key.to_vec());
        let value = if let Some(value) = self.storage_writes.get(&overlay_key) {
            value.clone()
        } else {
            let value = self.base_storage_get(contract, key);
            self.read_dependencies.push(ReadDependency::Storage {
                contract: contract.clone(),
                key: key.to_vec(),
                value: value.clone(),
            });
            value
        };
        self.accesses.push(AccessRecord {
            transaction_id: self.transaction_id,
            call_depth: depth,
            contract: contract.clone(),
            kind: AccessKind::StorageRead,
            key: key.to_vec(),
            range_end: None,
            value: value.clone(),
            reverted: false,
        });
        value
    }

    pub fn storage_set(&mut self, contract: &Address, key: Vec<u8>, value: Vec<u8>, depth: u32) {
        self.storage_writes
            .insert((contract.clone(), key.clone()), Some(value.clone()));
        self.accesses.push(AccessRecord {
            transaction_id: self.transaction_id,
            call_depth: depth,
            contract: contract.clone(),
            kind: AccessKind::StorageWrite,
            key,
            range_end: None,
            value: Some(value),
            reverted: false,
        });
    }

    pub fn storage_remove(&mut self, contract: &Address, key: Vec<u8>, depth: u32) {
        self.storage_writes
            .insert((contract.clone(), key.clone()), None);
        self.accesses.push(AccessRecord {
            transaction_id: self.transaction_id,
            call_depth: depth,
            contract: contract.clone(),
            kind: AccessKind::StorageRemove,
            key,
            range_end: None,
            value: None,
            reverted: false,
        });
    }

    pub fn storage_range(
        &mut self,
        contract: &Address,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
        depth: u32,
    ) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.accesses.push(AccessRecord {
            transaction_id: self.transaction_id,
            call_depth: depth,
            contract: contract.clone(),
            kind: AccessKind::StorageScan,
            key: start.unwrap_or_default().to_vec(),
            range_end: end.map(ToOwned::to_owned),
            value: None,
            reverted: false,
        });

        let base_entries = self.base_storage_range(contract, start, end);
        let masked_keys: Vec<Vec<u8>> = self
            .storage_writes
            .keys()
            .filter(|(write_contract, key)| {
                write_contract == contract
                    && start.map_or(true, |start| key.as_slice() >= start)
                    && end.map_or(true, |end| key.as_slice() < end)
            })
            .map(|(_, key)| key.clone())
            .collect();
        let masked: BTreeSet<Vec<u8>> = masked_keys.iter().cloned().collect();
        let observed_base_entries = base_entries
            .iter()
            .filter(|(key, _)| {
                start.map_or(true, |start| key.as_slice() >= start)
                    && end.map_or(true, |end| key.as_slice() < end)
                    && !masked.contains(key.as_slice())
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        self.read_dependencies.push(ReadDependency::StorageRange {
            contract: contract.clone(),
            start: start.map(ToOwned::to_owned),
            end: end.map(ToOwned::to_owned),
            base_entries: observed_base_entries,
            masked_keys,
        });

        let mut merged = base_entries;
        for ((write_contract, key), value) in &self.storage_writes {
            if write_contract != contract {
                continue;
            }
            match value {
                Some(value) => {
                    merged.insert(key.clone(), value.clone());
                }
                None => {
                    merged.remove(key);
                }
            }
        }

        merged
            .into_iter()
            .filter(|(key, _)| {
                start.map_or(true, |start| key.as_slice() >= start)
                    && end.map_or(true, |end| key.as_slice() < end)
            })
            .collect()
    }

    pub fn balance(
        &mut self,
        address: &Address,
        denom: &str,
        contract: &Address,
        depth: u32,
    ) -> Uint128 {
        let key = (address.clone(), denom.to_owned());
        let value = if let Some(value) = self.balance_writes.get(&key).copied() {
            value
        } else {
            let value = self.base_balance(address, denom);
            self.read_dependencies.push(ReadDependency::BankBalance {
                address: address.clone(),
                denom: denom.to_owned(),
                amount: value.u128(),
            });
            value
        };
        self.accesses.push(AccessRecord {
            transaction_id: self.transaction_id,
            call_depth: depth,
            contract: contract.clone(),
            kind: AccessKind::BankRead,
            key: bank_key(address, denom),
            range_end: None,
            value: Some(value.u128().to_be_bytes().to_vec()),
            reverted: false,
        });
        value
    }

    pub fn set_balance(
        &mut self,
        address: &Address,
        denom: &str,
        value: Uint128,
        contract: &Address,
        depth: u32,
    ) {
        self.balance_writes
            .insert((address.clone(), denom.to_owned()), value);
        self.accesses.push(AccessRecord {
            transaction_id: self.transaction_id,
            call_depth: depth,
            contract: contract.clone(),
            kind: AccessKind::BankWrite,
            key: bank_key(address, denom),
            range_end: None,
            value: Some(value.u128().to_be_bytes().to_vec()),
            reverted: false,
        });
    }

    pub fn all_balances(&mut self, address: &Address, contract: &Address, depth: u32) -> Vec<Coin> {
        let masked_denoms: Vec<String> = self
            .balance_writes
            .keys()
            .filter(|(owner, _)| owner == address)
            .map(|(_, denom)| denom.clone())
            .collect();
        let masked: BTreeSet<String> = masked_denoms.iter().cloned().collect();
        let visible_balances = self.base_balances(address);
        let base_balances: Vec<(String, u128)> = visible_balances
            .iter()
            .filter(|(denom, amount)| !amount.is_zero() && !masked.contains(denom.as_str()))
            .map(|(denom, amount)| (denom.clone(), amount.u128()))
            .collect();
        self.read_dependencies
            .push(ReadDependency::BankAllBalances {
                address: address.clone(),
                base_balances,
                masked_denoms,
            });

        let base_denoms: BTreeSet<String> = visible_balances.keys().cloned().collect();
        let overlay_denoms = self
            .balance_writes
            .keys()
            .filter(|(owner, _)| owner == address)
            .map(|(_, denom)| denom.clone());

        base_denoms
            .into_iter()
            .chain(overlay_denoms)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .filter_map(|denom| {
                let amount = self.balance(address, &denom, contract, depth);
                (!amount.is_zero()).then_some(Coin { denom, amount })
            })
            .collect()
    }

    pub fn transfer(
        &mut self,
        from: &Address,
        to: &Address,
        coins: &[Coin],
        contract: &Address,
        depth: u32,
    ) -> EngineResult<()> {
        let totals = aggregate_coins(coins)?;

        for (denom, amount) in &totals {
            let available = self.balance(from, denom, contract, depth).u128();
            if available < *amount {
                return Err(EngineError::InsufficientFunds {
                    address: from.clone(),
                    denom: denom.clone(),
                    needed: *amount,
                    available,
                });
            }
        }

        if from == to {
            return Ok(());
        }

        for (denom, amount) in totals {
            let from_balance = self.balance(from, &denom, contract, depth).u128();
            let to_balance = self.balance(to, &denom, contract, depth).u128();
            let new_to = to_balance
                .checked_add(amount)
                .ok_or(EngineError::BalanceOverflow)?;
            self.set_balance(
                from,
                &denom,
                Uint128::new(from_balance - amount),
                contract,
                depth,
            );
            self.set_balance(to, &denom, Uint128::new(new_to), contract, depth);
        }
        Ok(())
    }

    pub fn burn(
        &mut self,
        owner: &Address,
        coins: &[Coin],
        contract: &Address,
        depth: u32,
    ) -> EngineResult<()> {
        let totals = aggregate_coins(coins)?;
        for (denom, amount) in totals {
            let available = self.balance(owner, &denom, contract, depth).u128();
            if available < amount {
                return Err(EngineError::InsufficientFunds {
                    address: owner.clone(),
                    denom,
                    needed: amount,
                    available,
                });
            }
            self.set_balance(
                owner,
                &denom,
                Uint128::new(available - amount),
                contract,
                depth,
            );
        }
        Ok(())
    }

    pub fn commit(&self) {
        let mut world = self.base.write();
        for (address, metadata) in &self.created_contracts {
            world.contracts.insert(address.clone(), metadata.clone());
            world.storage.entry(address.clone()).or_default();
        }
        for ((contract, key), value) in &self.storage_writes {
            let storage = world.storage.entry(contract.clone()).or_default();
            match value {
                Some(value) => {
                    storage.insert(key.clone(), value.clone());
                }
                None => {
                    storage.remove(key);
                }
            }
        }
        for (key, value) in &self.balance_writes {
            if value.is_zero() {
                world.balances.remove(key);
            } else {
                world.balances.insert(key.clone(), *value);
            }
        }
    }

    pub fn write_set(&self) -> StateWriteSet {
        StateWriteSet {
            storage: self
                .storage_writes
                .iter()
                .map(|((contract, key), value)| StorageWrite {
                    contract: contract.clone(),
                    key: key.clone(),
                    value: value.clone(),
                })
                .collect(),
            balances: self
                .balance_writes
                .iter()
                .map(|((address, denom), amount)| BalanceWrite {
                    address: address.clone(),
                    denom: denom.clone(),
                    amount: amount.u128(),
                })
                .collect(),
            created_contracts: self.created_contracts.values().cloned().collect(),
        }
    }

    pub fn created_addresses(&self) -> Vec<Address> {
        self.created_contracts.keys().cloned().collect()
    }
}

pub(crate) fn code_id_of(tx: &SharedTx, address: &Address) -> EngineResult<CodeId> {
    tx.lock()
        .contract(address)
        .map(|metadata| metadata.code_id)
        .ok_or_else(|| EngineError::UnknownContract(address.clone()))
}

fn aggregate_coins(coins: &[Coin]) -> EngineResult<BTreeMap<String, u128>> {
    let mut totals = BTreeMap::new();
    for coin in coins {
        if coin.denom.is_empty() {
            return Err(EngineError::InvalidCoin {
                denom: coin.denom.clone(),
                amount: coin.amount.u128(),
                reason: "denomination must not be empty".to_owned(),
            });
        }
        if coin.amount.is_zero() {
            return Err(EngineError::InvalidCoin {
                denom: coin.denom.clone(),
                amount: 0,
                reason: "amount must be positive".to_owned(),
            });
        }
        let entry = totals.entry(coin.denom.clone()).or_insert(0_u128);
        *entry = entry
            .checked_add(coin.amount.u128())
            .ok_or(EngineError::BalanceOverflow)?;
    }
    Ok(totals)
}

fn bank_key(address: &Address, denom: &str) -> Vec<u8> {
    let mut key = address.as_str().as_bytes().to_vec();
    key.push(0);
    key.extend_from_slice(denom.as_bytes());
    key
}
