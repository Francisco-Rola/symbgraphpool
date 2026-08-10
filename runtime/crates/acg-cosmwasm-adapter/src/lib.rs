//! CosmWasm runtime adapter for the runtime-independent candidate conflict graph.
//!
//! `ExecutionRequest` remains the execution source of truth. This crate derives only the compact
//! metadata required by the conflict-graph online path: profile identity, instance identity and
//! concrete symbolic-input bindings.

use std::collections::BTreeMap;
use std::sync::Arc;

use acg_candidate_graph::CandidateTransaction;
use acg_core::{
    ContractCodeHash, EntrypointKind, EntrypointSelector, InstanceId, ProfileDescriptor, ProfileId,
    RuntimeId, TxId,
};
use acg_cosmwasm_engine::{Address, BlockContext, CodeChecksum, CosmWasmEngine, ExecutionRequest};
use acg_predicate::InputBindings;
use acg_profile_graph::ProfileGraph;
use acg_validator_sim::{PendingTransaction, ProducedBlock};
use cosmwasm_std::{Binary, Coin};
use parking_lot::Mutex;
use serde_json::{json, Map, Value};
use thiserror::Error;

#[derive(Default)]
struct InstanceRegistryState {
    addresses: BTreeMap<Address, InstanceId>,
    next_id: u32,
}

/// Dense validator-local mapping from runtime contract instances to [`InstanceId`].
#[derive(Clone, Default)]
pub struct InstanceRegistry {
    state: Arc<Mutex<InstanceRegistryState>>,
}

impl InstanceRegistry {
    pub fn resolve_address(&self, address: &Address) -> Result<InstanceId, AdapterError> {
        let mut state = self.state.lock();
        if let Some(id) = state.addresses.get(address) {
            return Ok(*id);
        }
        let id = allocate_instance_id(&mut state)?;
        state.addresses.insert(address.clone(), id);
        Ok(id)
    }

    pub fn len(&self) -> usize {
        self.state.lock().addresses.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn allocate_instance_id(state: &mut InstanceRegistryState) -> Result<InstanceId, AdapterError> {
    let id = InstanceId(state.next_id);
    state.next_id = state
        .next_id
        .checked_add(1)
        .ok_or(AdapterError::InstanceIdExhausted)?;
    Ok(id)
}

#[derive(Clone, Debug, PartialEq)]
pub struct DecodedExecuteEntrypoint {
    pub canonical_name: String,
    pub payload: Value,
}

/// Runtime message decoders are pluggable because some contracts encode nested entrypoint enums.
pub trait ExecuteEntrypointDecoder: Send + Sync {
    fn decode(&self, msg: &Binary) -> Result<DecodedExecuteEntrypoint, AdapterError>;
}

/// Default CosmWasm decoder for externally tagged JSON enums such as
/// `{ "transfer": { ... } }` -> `execute::Transfer`.
#[derive(Clone, Copy, Debug, Default)]
pub struct JsonTopLevelEntrypointDecoder;

impl ExecuteEntrypointDecoder for JsonTopLevelEntrypointDecoder {
    fn decode(&self, msg: &Binary) -> Result<DecodedExecuteEntrypoint, AdapterError> {
        let value: Value = serde_json::from_slice(msg.as_slice())
            .map_err(|source| AdapterError::InvalidMessageJson { source })?;
        let Value::Object(object) = value else {
            return Err(AdapterError::ExecuteMessageMustBeObject);
        };
        if object.len() != 1 {
            return Err(AdapterError::AmbiguousExecuteMessage(object.len()));
        }
        let (variant, payload) = object.into_iter().next().expect("one entry validated");
        Ok(DecodedExecuteEntrypoint {
            canonical_name: format!("execute::{}", snake_to_pascal(&variant)?),
            payload,
        })
    }
}

#[derive(Clone)]
pub struct CosmWasmAdapterConfig {
    pub runtime_id: RuntimeId,
    pub profile_schema_version: u16,
    pub selector_overrides: BTreeMap<String, EntrypointSelector>,
}

impl CosmWasmAdapterConfig {
    pub fn new(runtime_id: RuntimeId, profile_schema_version: u16) -> Result<Self, AdapterError> {
        if profile_schema_version == 0 {
            return Err(AdapterError::InvalidProfileSchemaVersion);
        }
        Ok(Self {
            runtime_id,
            profile_schema_version,
            selector_overrides: BTreeMap::new(),
        })
    }
}

/// Converts concrete validator-runtime requests into graph-facing candidate transactions.
#[derive(Clone)]
pub struct CosmWasmCandidateAdapter {
    config: CosmWasmAdapterConfig,
    instances: InstanceRegistry,
    execute_decoder: Arc<dyn ExecuteEntrypointDecoder>,
}

impl CosmWasmCandidateAdapter {
    pub fn new(config: CosmWasmAdapterConfig) -> Self {
        Self {
            config,
            instances: InstanceRegistry::default(),
            execute_decoder: Arc::new(JsonTopLevelEntrypointDecoder),
        }
    }

    pub fn with_execute_decoder(
        config: CosmWasmAdapterConfig,
        execute_decoder: Arc<dyn ExecuteEntrypointDecoder>,
    ) -> Self {
        Self {
            config,
            instances: InstanceRegistry::default(),
            execute_decoder,
        }
    }

    pub fn instance_registry(&self) -> &InstanceRegistry {
        &self.instances
    }

    pub fn adapt_block(
        &self,
        engine: &CosmWasmEngine,
        profile_graph: &ProfileGraph,
        block: &ProducedBlock,
    ) -> Result<Vec<CandidateTransaction>, AdapterError> {
        block
            .transactions
            .iter()
            .enumerate()
            .map(|(position, pending)| {
                self.adapt_pending(engine, profile_graph, &block.context, pending, position)
            })
            .collect()
    }

    pub fn adapt_pending(
        &self,
        engine: &CosmWasmEngine,
        profile_graph: &ProfileGraph,
        block: &BlockContext,
        pending: &PendingTransaction,
        predicted_position: usize,
    ) -> Result<CandidateTransaction, AdapterError> {
        let predicted_position = u32::try_from(predicted_position)
            .map_err(|_| AdapterError::PredictedPositionOverflow(predicted_position))?;
        let (profile_id, instance_id, input_bindings) = match &pending.request {
            ExecutionRequest::Instantiate {
                sender,
                code_id,
                funds,
                msg,
                ..
            } => {
                let code = engine
                    .code_metadata(*code_id)
                    .ok_or(AdapterError::UnknownCodeId(code_id.0))?;
                let profile_id = self.resolve_profile(
                    profile_graph,
                    code.checksum,
                    EntrypointKind::Instantiate,
                    "instantiate",
                )?;
                let predicted_address =
                    engine.predict_contract_address(pending.transaction_id(), 0);
                let instance_id = self.instances.resolve_address(&predicted_address)?;
                let bindings = instantiate_bindings(sender, funds, msg, block)?;
                (profile_id, instance_id, bindings)
            }
            ExecutionRequest::Execute {
                sender,
                contract,
                funds,
                msg,
                ..
            } => {
                let code = engine
                    .contract_code_metadata(contract)
                    .ok_or_else(|| AdapterError::UnknownContract(contract.clone()))?;
                let decoded = self.execute_decoder.decode(msg)?;
                let profile_id = self.resolve_profile(
                    profile_graph,
                    code.checksum,
                    EntrypointKind::Execute,
                    &decoded.canonical_name,
                )?;
                let instance_id = self.instances.resolve_address(contract)?;
                let bindings = execute_bindings(sender, funds, decoded.payload, block)?;
                (profile_id, instance_id, bindings)
            }
        };

        Ok(CandidateTransaction {
            tx_id: TxId(pending.transaction_id().0),
            predicted_position,
            inclusion_probability: 1.0,
            profile_id,
            instance_id,
            input_bindings,
            estimated_execution_cost: 1,
        })
    }

    fn resolve_profile(
        &self,
        graph: &ProfileGraph,
        checksum: CodeChecksum,
        kind: EntrypointKind,
        canonical_name: &str,
    ) -> Result<ProfileId, AdapterError> {
        let selector = self
            .config
            .selector_overrides
            .get(canonical_name)
            .copied()
            .unwrap_or_else(|| EntrypointSelector::from_canonical_name(canonical_name));
        let descriptor = ProfileDescriptor {
            runtime_id: self.config.runtime_id.clone(),
            contract_code_hash: ContractCodeHash(*checksum.as_bytes()),
            entrypoint_kind: kind,
            numeric_entrypoint_selector: selector,
            profile_schema_version: self.config.profile_schema_version,
        };
        graph
            .resolve(descriptor.stable_key())
            .ok_or_else(|| AdapterError::UnknownProfile {
                entrypoint: canonical_name.to_owned(),
                code_hash: checksum.to_hex(),
            })
    }
}

fn instantiate_bindings(
    sender: &Address,
    funds: &[Coin],
    msg: &Binary,
    block: &BlockContext,
) -> Result<InputBindings, AdapterError> {
    let msg_value: Value = serde_json::from_slice(msg.as_slice())
        .map_err(|source| AdapterError::InvalidMessageJson { source })?;
    let mut root = Map::new();
    root.insert("msg".to_owned(), msg_value);
    root.insert("info".to_owned(), info_value(sender, funds)?);
    root.insert("env".to_owned(), env_value(block));
    Ok(InputBindings::from_object(root))
}

fn execute_bindings(
    sender: &Address,
    funds: &[Coin],
    payload: Value,
    block: &BlockContext,
) -> Result<InputBindings, AdapterError> {
    let mut root = match payload {
        Value::Object(object) => object,
        Value::Null => Map::new(),
        value => {
            let mut object = Map::new();
            object.insert("value".to_owned(), value);
            object
        }
    };
    root.insert("info".to_owned(), info_value(sender, funds)?);
    root.insert("env".to_owned(), env_value(block));
    Ok(InputBindings::from_object(root))
}

fn info_value(sender: &Address, funds: &[Coin]) -> Result<Value, AdapterError> {
    let funds = serde_json::to_value(funds)
        .map_err(|source| AdapterError::SerializeRuntimeContext { source })?;
    Ok(json!({
        "sender": sender.as_str(),
        "funds": funds,
    }))
}

fn env_value(block: &BlockContext) -> Value {
    json!({
        "block": {
            "height": block.height,
            "time_nanos": block.time_nanos,
            "chain_id": block.chain_id.as_str(),
        },
        "transaction_index": block.transaction_index,
    })
}

fn snake_to_pascal(value: &str) -> Result<String, AdapterError> {
    if value.is_empty() {
        return Err(AdapterError::EmptyExecuteVariant);
    }
    let mut output = String::with_capacity(value.len());
    for component in value.split('_') {
        if component.is_empty() {
            return Err(AdapterError::InvalidExecuteVariant(value.to_owned()));
        }
        let mut chars = component.chars();
        let first = chars
            .next()
            .ok_or_else(|| AdapterError::InvalidExecuteVariant(value.to_owned()))?;
        output.extend(first.to_uppercase());
        output.extend(chars);
    }
    Ok(output)
}

#[derive(Debug, Error)]
pub enum AdapterError {
    #[error("profile schema version must be non-zero")]
    InvalidProfileSchemaVersion,
    #[error("validator-local InstanceId space is exhausted")]
    InstanceIdExhausted,
    #[error("candidate predicted position {0} cannot be represented as u32")]
    PredictedPositionOverflow(usize),
    #[error("unknown runtime code id {0}")]
    UnknownCodeId(u64),
    #[error("unknown runtime contract {0}")]
    UnknownContract(Address),
    #[error("no profile for entrypoint {entrypoint:?} and code hash {code_hash}")]
    UnknownProfile {
        entrypoint: String,
        code_hash: String,
    },
    #[error("invalid CosmWasm message JSON: {source}")]
    InvalidMessageJson {
        #[source]
        source: serde_json::Error,
    },
    #[error("execute message must be a JSON object")]
    ExecuteMessageMustBeObject,
    #[error("execute message must contain exactly one top-level variant; found {0}")]
    AmbiguousExecuteMessage(usize),
    #[error("execute variant cannot be empty")]
    EmptyExecuteVariant,
    #[error("unsupported execute variant name {0:?}")]
    InvalidExecuteVariant(String),
    #[error("failed to serialize runtime context: {source}")]
    SerializeRuntimeContext {
        #[source]
        source: serde_json::Error,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn top_level_decoder_converts_snake_case_to_profile_name() {
        let decoded = JsonTopLevelEntrypointDecoder
            .decode(&Binary::from(
                br#"{"transfer_from":{"owner":"a"}}"#.to_vec(),
            ))
            .unwrap();
        assert_eq!(decoded.canonical_name, "execute::TransferFrom");
        assert_eq!(decoded.payload["owner"], "a");
    }

    #[test]
    fn instance_registry_is_dense_and_stable() {
        let registry = InstanceRegistry::default();
        let first = registry
            .resolve_address(&Address::new("contract-a"))
            .unwrap();
        let first_again = registry
            .resolve_address(&Address::new("contract-a"))
            .unwrap();
        let second = registry
            .resolve_address(&Address::new("contract-b"))
            .unwrap();
        assert_eq!(first, InstanceId(0));
        assert_eq!(first_again, first);
        assert_eq!(second, InstanceId(1));
        assert_eq!(registry.len(), 2);
    }
}
