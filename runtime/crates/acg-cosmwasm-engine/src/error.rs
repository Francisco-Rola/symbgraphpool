use crate::types::{Address, CodeId};
use thiserror::Error;

pub type EngineResult<T> = Result<T, EngineError>;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("invalid engine configuration: {0}")]
    InvalidConfiguration(String),
    #[error("unknown code ID {0:?}")]
    UnknownCode(CodeId),
    #[error("unknown contract {0}")]
    UnknownContract(Address),
    #[error("contract address already exists: {0}")]
    ContractAlreadyExists(Address),
    #[error("invalid address {address:?}: {reason}")]
    InvalidAddress { address: String, reason: String },
    #[error("invalid code label {label:?}: {reason}")]
    InvalidCodeLabel { label: String, reason: String },
    #[error("invalid contract label {label:?}: {reason}")]
    InvalidContractLabel { label: String, reason: String },
    #[error("invalid coin {amount} {denom:?}: {reason}")]
    InvalidCoin {
        denom: String,
        amount: u128,
        reason: String,
    },
    #[error("duplicate denomination in a coin list: {0}")]
    DuplicateDenomination(String),
    #[error("insufficient funds for {address}: need {needed} {denom}, have {available}")]
    InsufficientFunds {
        address: Address,
        denom: String,
        needed: u128,
        available: u128,
    },
    #[error("arithmetic overflow while updating a balance")]
    BalanceOverflow,
    #[error("maximum nested call depth {0} exceeded")]
    MaxCallDepth(u32),
    #[error("unsupported Cosmos message: {0}")]
    UnsupportedMessage(String),
    #[error("bundle call {call_index} failed: {error}")]
    BundleCallFailed {
        call_index: usize,
        error: String,
    },
    #[error("contract returned an error: {0}")]
    Contract(String),
    #[error("native contract error: {0}")]
    Native(String),
    #[error("CosmWasm VM error: {0}")]
    Vm(String),
    #[error("state lock or internal engine invariant failed: {0}")]
    Internal(String),
}

impl From<cosmwasm_vm::VmError> for EngineError {
    fn from(value: cosmwasm_vm::VmError) -> Self {
        Self::Vm(value.to_string())
    }
}
