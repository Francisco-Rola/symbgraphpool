use cosmwasm_std::{Binary, Coin, Empty, Env, MessageInfo, Reply, Response};

use crate::error::{EngineError, EngineResult};
use crate::state::SharedTx;
use crate::types::{Address, BlockContext, TransactionId};

pub trait NativeContract: Send + Sync {
    fn instantiate(
        &self,
        context: &mut NativeCallContext,
        env: Env,
        info: MessageInfo,
        msg: Binary,
    ) -> Result<Response<Empty>, String>;

    fn execute(
        &self,
        context: &mut NativeCallContext,
        env: Env,
        info: MessageInfo,
        msg: Binary,
    ) -> Result<Response<Empty>, String>;

    fn query(
        &self,
        context: &mut NativeCallContext,
        env: Env,
        msg: Binary,
    ) -> Result<Binary, String>;

    fn reply(
        &self,
        _context: &mut NativeCallContext,
        _env: Env,
        _reply: Reply,
    ) -> Result<Response<Empty>, String> {
        Err("reply entrypoint not implemented".to_owned())
    }
}

pub struct NativeCallContext {
    pub transaction_id: TransactionId,
    pub block: BlockContext,
    pub contract: Address,
    pub caller: Address,
    pub depth: u32,
    tx: SharedTx,
}

impl NativeCallContext {
    pub(crate) fn new(
        transaction_id: TransactionId,
        block: BlockContext,
        contract: Address,
        caller: Address,
        depth: u32,
        tx: SharedTx,
    ) -> Self {
        Self {
            transaction_id,
            block,
            contract,
            caller,
            depth,
            tx,
        }
    }

    pub fn storage_get(&mut self, key: &[u8]) -> Option<Vec<u8>> {
        self.tx.lock().storage_get(&self.contract, key, self.depth)
    }

    pub fn storage_set(&mut self, key: impl Into<Vec<u8>>, value: impl Into<Vec<u8>>) {
        self.tx
            .lock()
            .storage_set(&self.contract, key.into(), value.into(), self.depth);
    }

    pub fn storage_remove(&mut self, key: impl Into<Vec<u8>>) {
        self.tx
            .lock()
            .storage_remove(&self.contract, key.into(), self.depth);
    }

    pub fn storage_range(
        &mut self,
        start: Option<&[u8]>,
        end: Option<&[u8]>,
    ) -> Vec<(Vec<u8>, Vec<u8>)> {
        self.tx
            .lock()
            .storage_range(&self.contract, start, end, self.depth)
    }

    pub fn balance(&mut self, address: &Address, denom: &str) -> u128 {
        self.tx
            .lock()
            .balance(address, denom, &self.contract, self.depth)
            .u128()
    }

    pub fn all_balances(&mut self, address: &Address) -> Vec<Coin> {
        self.tx
            .lock()
            .all_balances(address, &self.contract, self.depth)
    }

    pub fn send(&mut self, to: &Address, coins: &[Coin]) -> EngineResult<()> {
        self.tx
            .lock()
            .transfer(&self.contract, to, coins, &self.contract, self.depth)
    }

    pub fn contract_code_id(&self, address: &Address) -> EngineResult<crate::types::CodeId> {
        self.tx
            .lock()
            .contract(address)
            .map(|metadata| metadata.code_id)
            .ok_or_else(|| EngineError::UnknownContract(address.clone()))
    }
}
