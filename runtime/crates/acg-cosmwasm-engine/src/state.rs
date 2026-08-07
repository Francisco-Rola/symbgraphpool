use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use cosmwasm_std::{Coin, Uint128};
use parking_lot::{Mutex, RwLock};

use crate::error::{EngineError, EngineResult};
use crate::types::{AccessKind, AccessRecord, Address, CodeId, ContractMetadata, TransactionId};

#[derive(Default)]
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
    pub transaction_id: TransactionId,
    pub storage_writes: BTreeMap<(Address, Vec<u8>), Option<Vec<u8>>>,
    pub balance_writes: BTreeMap<(Address, String), Uint128>,
    pub created_contracts: BTreeMap<Address, ContractMetadata>,
    pub accesses: Vec<AccessRecord>,
    next_instance_ordinal: u32,
}

impl TransactionState {
    pub fn new(base: SharedWorld, transaction_id: TransactionId) -> Self {
        Self {
            base,
            transaction_id,
            storage_writes: BTreeMap::new(),
            balance_writes: BTreeMap::new(),
            created_contracts: BTreeMap::new(),
            accesses: Vec::new(),
            next_instance_ordinal: 0,
        }
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

    pub fn contract(&self, address: &Address) -> Option<ContractMetadata> {
        self.created_contracts
            .get(address)
            .cloned()
            .or_else(|| self.base.read().contracts.get(address).cloned())
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
        let value = self
            .storage_writes
            .get(&overlay_key)
            .cloned()
            .unwrap_or_else(|| {
                self.base
                    .read()
                    .storage
                    .get(contract)
                    .and_then(|entries| entries.get(key).cloned())
            });
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

        let mut merged = self
            .base
            .read()
            .storage
            .get(contract)
            .cloned()
            .unwrap_or_default();

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
        let value = self.balance_writes.get(&key).copied().unwrap_or_else(|| {
            self.base
                .read()
                .balances
                .get(&key)
                .copied()
                .unwrap_or_default()
        });
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
        let base_denoms: BTreeSet<String> = self
            .base
            .read()
            .balances
            .keys()
            .filter(|(owner, _)| owner == address)
            .map(|(_, denom)| denom.clone())
            .collect();
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
