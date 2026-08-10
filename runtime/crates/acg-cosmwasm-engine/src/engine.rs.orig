use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use cosmwasm_std::{
    Addr, Attribute, BankMsg, Binary, BlockInfo, Coin, ContractInfo, ContractResult, CosmosMsg,
    Empty, Env, Event, MessageInfo, MsgResponse, Reply, ReplyOn, Response, SubMsg, SubMsgResponse,
    SubMsgResult, Timestamp, TransactionInfo, WasmMsg,
};
use cosmwasm_vm::{
    call_execute, call_instantiate, call_query, call_reply, Backend, InstanceOptions,
};
use parking_lot::RwLock;
use sha2::{Digest, Sha256};

use crate::api::{validate_address, EngineApi};
use crate::cache::{WasmCacheConfig, WasmCacheMetrics, WasmModuleCache};
use crate::error::{EngineError, EngineResult};
use crate::native::{NativeCallContext, NativeContract};
use crate::querier::EngineQuerier;
use crate::state::{code_id_of, SharedTx, SharedWorld, TransactionState, WorldState};
use crate::storage::EngineStorage;
use crate::types::{
    AccessKind, Address, BlockContext, CodeChecksum, CodeId, CodeKind, CodeMetadata,
    ContractMetadata, ExecutionOutcome, ExecutionRequest, QueryOutcome, TransactionId,
};

const EXECUTE_RESPONSE_TYPE_URL: &str = "/cosmwasm.wasm.v1.MsgExecuteContractResponse";
const INSTANTIATE_RESPONSE_TYPE_URL: &str = "/cosmwasm.wasm.v1.MsgInstantiateContractResponse";
const BANK_SEND_RESPONSE_TYPE_URL: &str = "/cosmos.bank.v1beta1.MsgSendResponse";
const BANK_BURN_RESPONSE_TYPE_URL: &str = "/cosmos.bank.v1beta1.MsgBurnResponse";

#[derive(Clone, Debug)]
pub struct EngineConfig {
    pub gas_limit: u64,
    pub max_call_depth: u32,
    pub contract_address_prefix: String,
    pub wasm_cache: WasmCacheConfig,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            gas_limit: 10_000_000_000_000,
            max_call_depth: 32,
            contract_address_prefix: "contract".to_owned(),
            wasm_cache: WasmCacheConfig::default(),
        }
    }
}

#[derive(Clone)]
pub(crate) enum CodeArtifact {
    Wasm(cosmwasm_std::Checksum),
    Native(Arc<dyn NativeContract>),
}

#[derive(Clone)]
pub(crate) struct CodeRecord {
    pub metadata: CodeMetadata,
    pub artifact: CodeArtifact,
}

pub(crate) struct EngineCore {
    pub config: EngineConfig,
    pub state: SharedWorld,
    codes: RwLock<BTreeMap<CodeId, CodeRecord>>,
    wasm_cache: WasmModuleCache,
    next_code_id: AtomicU64,
}

#[derive(Clone)]
pub struct CosmWasmEngine {
    core: Arc<EngineCore>,
}

impl CosmWasmEngine {
    pub fn try_new(config: EngineConfig) -> EngineResult<Self> {
        let wasm_cache = WasmModuleCache::new(&config.wasm_cache)?;

        Ok(Self {
            core: Arc::new(EngineCore {
                config,
                state: Arc::new(RwLock::new(WorldState::default())),
                codes: RwLock::new(BTreeMap::new()),
                wasm_cache,
                next_code_id: AtomicU64::new(1),
            }),
        })
    }

    pub fn new(config: EngineConfig) -> Self {
        Self::try_new(config).expect("failed to initialize CosmWasm engine")
    }

    pub fn upload_wasm(&self, wasm: Vec<u8>) -> EngineResult<CodeId> {
        if wasm.is_empty() {
            return Err(EngineError::Vm(
                "Wasm bytecode must not be empty".to_owned(),
            ));
        }
        let vm_checksum = self.core.wasm_cache.save_wasm(&wasm)?;
        let checksum = checksum_from_vm(vm_checksum);
        let code_id = CodeId(self.core.next_code_id.fetch_add(1, Ordering::Relaxed));
        let metadata = CodeMetadata {
            code_id,
            checksum,
            kind: CodeKind::Wasm,
            label: format!("wasm:{}", checksum.to_hex()),
        };
        self.core.codes.write().insert(
            code_id,
            CodeRecord {
                metadata,
                artifact: CodeArtifact::Wasm(vm_checksum),
            },
        );
        Ok(code_id)
    }

    pub fn wasm_cache_metrics(&self) -> WasmCacheMetrics {
        self.core.wasm_cache.metrics()
    }

    pub fn wasm_cache_dir(&self) -> &std::path::Path {
        self.core.wasm_cache.base_dir()
    }

    pub fn pin_wasm(&self, code_id: CodeId) -> EngineResult<()> {
        match self.code(code_id)?.artifact {
            CodeArtifact::Wasm(checksum) => self.core.wasm_cache.pin(&checksum),
            CodeArtifact::Native(_) => Ok(()),
        }
    }

    pub fn unpin_wasm(&self, code_id: CodeId) -> EngineResult<()> {
        match self.code(code_id)?.artifact {
            CodeArtifact::Wasm(checksum) => self.core.wasm_cache.unpin(&checksum),
            CodeArtifact::Native(_) => Ok(()),
        }
    }

    pub fn register_native(
        &self,
        label: impl Into<String>,
        contract: Arc<dyn NativeContract>,
    ) -> EngineResult<CodeId> {
        let label = label.into();
        validate_code_label(&label)?;
        let checksum = checksum_native_label(&label);
        let code_id = CodeId(self.core.next_code_id.fetch_add(1, Ordering::Relaxed));
        let metadata = CodeMetadata {
            code_id,
            checksum,
            kind: CodeKind::Native,
            label,
        };
        self.core.codes.write().insert(
            code_id,
            CodeRecord {
                metadata,
                artifact: CodeArtifact::Native(contract),
            },
        );
        Ok(code_id)
    }

    pub fn code_metadata(&self, code_id: CodeId) -> Option<CodeMetadata> {
        self.core
            .codes
            .read()
            .get(&code_id)
            .map(|record| record.metadata.clone())
    }

    pub fn contract_code_metadata(&self, address: &Address) -> Option<CodeMetadata> {
        self.contract_metadata(address)
            .and_then(|metadata| self.code_metadata(metadata.code_id))
    }

    pub fn set_balance(&self, address: impl Into<Address>, coins: &[Coin]) -> EngineResult<()> {
        let address = address.into();
        validate_public_address(&address)?;
        validate_balance_seed(coins)?;
        let mut world = self.core.state.write();
        for coin in coins {
            let key = (address.clone(), coin.denom.clone());
            if coin.amount.is_zero() {
                world.balances.remove(&key);
            } else {
                world.balances.insert(key, coin.amount);
            }
        }
        Ok(())
    }

    pub fn balance(&self, address: impl Into<Address>, denom: &str) -> u128 {
        let address = address.into();
        self.core
            .state
            .read()
            .balances
            .get(&(address, denom.to_owned()))
            .copied()
            .unwrap_or_default()
            .u128()
    }

    pub fn all_balances(&self, address: impl Into<Address>) -> Vec<Coin> {
        let address = address.into();
        self.core
            .state
            .read()
            .balances
            .iter()
            .filter(|((owner, _), amount)| owner == &address && !amount.is_zero())
            .map(|((_, denom), amount)| Coin {
                denom: denom.clone(),
                amount: *amount,
            })
            .collect()
    }

    pub fn contract_metadata(&self, address: &Address) -> Option<ContractMetadata> {
        self.core.state.read().contracts.get(address).cloned()
    }

    pub fn raw_storage(&self, address: &Address, key: &[u8]) -> Option<Vec<u8>> {
        self.core
            .state
            .read()
            .storage
            .get(address)
            .and_then(|storage| storage.get(key).cloned())
    }

    pub fn execute_request(
        &self,
        block: BlockContext,
        request: ExecutionRequest,
    ) -> EngineResult<ExecutionOutcome> {
        match request {
            ExecutionRequest::Instantiate {
                transaction_id,
                sender,
                code_id,
                admin,
                label,
                funds,
                msg,
            } => self.instantiate(
                transaction_id,
                block,
                sender,
                code_id,
                admin,
                label,
                funds,
                msg,
            ),
            ExecutionRequest::Execute {
                transaction_id,
                sender,
                contract,
                funds,
                msg,
            } => self.execute(transaction_id, block, sender, contract, funds, msg),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn instantiate(
        &self,
        transaction_id: TransactionId,
        block: BlockContext,
        sender: Address,
        code_id: CodeId,
        admin: Option<Address>,
        label: String,
        funds: Vec<Coin>,
        msg: Binary,
    ) -> EngineResult<ExecutionOutcome> {
        validate_public_address(&sender)?;
        if let Some(admin) = &admin {
            validate_public_address(admin)?;
        }
        validate_contract_label(&label)?;
        let code = self.code(code_id)?;

        let tx = self.begin_transaction(transaction_id);
        let contract = tx
            .lock()
            .allocate_contract_address(&self.core.config.contract_address_prefix)?;
        tx.lock().create_contract(ContractMetadata {
            address: contract.clone(),
            code_id,
            code_checksum: code.metadata.checksum,
            creator: sender.clone(),
            admin,
            label,
        })?;
        tx.lock()
            .transfer(&sender, &contract, &funds, &contract, 0)?;

        let response = invoke_contract(
            self.core.clone(),
            tx.clone(),
            block.clone(),
            contract.clone(),
            sender,
            funds,
            msg,
            0,
            Entrypoint::Instantiate,
        )?;
        let mut processed = process_response(
            self.core.clone(),
            tx.clone(),
            block,
            contract.clone(),
            response,
            0,
        )?;
        processed.events.insert(
            0,
            Event::new("instantiate").add_attribute("_contract_address", contract.as_str()),
        );
        tx.lock().commit();
        let state = tx.lock();
        Ok(ExecutionOutcome {
            transaction_id,
            contract,
            events: processed.events,
            data: processed.data,
            accesses: state.accesses.clone(),
            created_contracts: state.created_addresses(),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn execute(
        &self,
        transaction_id: TransactionId,
        block: BlockContext,
        sender: Address,
        contract: Address,
        funds: Vec<Coin>,
        msg: Binary,
    ) -> EngineResult<ExecutionOutcome> {
        validate_public_address(&sender)?;
        validate_public_address(&contract)?;
        let tx = self.begin_transaction(transaction_id);
        code_id_of(&tx, &contract)?;
        tx.lock()
            .transfer(&sender, &contract, &funds, &contract, 0)?;

        let response = invoke_contract(
            self.core.clone(),
            tx.clone(),
            block.clone(),
            contract.clone(),
            sender,
            funds,
            msg,
            0,
            Entrypoint::Execute,
        )?;
        let processed = process_response(
            self.core.clone(),
            tx.clone(),
            block,
            contract.clone(),
            response,
            0,
        )?;
        tx.lock().commit();
        let state = tx.lock();
        Ok(ExecutionOutcome {
            transaction_id,
            contract,
            events: processed.events,
            data: processed.data,
            accesses: state.accesses.clone(),
            created_contracts: state.created_addresses(),
        })
    }

    pub fn query(
        &self,
        block: BlockContext,
        contract: Address,
        msg: Binary,
    ) -> EngineResult<QueryOutcome> {
        validate_public_address(&contract)?;
        let tx = self.begin_transaction(TransactionId(0));
        let data = query_contract_shared(
            self.core.clone(),
            tx.clone(),
            block,
            contract.clone(),
            msg,
            0,
        )?;
        let accesses = tx.lock().accesses.clone();
        Ok(QueryOutcome {
            contract,
            data,
            accesses,
        })
    }

    fn begin_transaction(&self, transaction_id: TransactionId) -> SharedTx {
        Arc::new(parking_lot::Mutex::new(TransactionState::new(
            self.core.state.clone(),
            transaction_id,
        )))
    }

    fn code(&self, code_id: CodeId) -> EngineResult<CodeRecord> {
        self.core.code(code_id)
    }
}

impl Default for CosmWasmEngine {
    fn default() -> Self {
        Self::new(EngineConfig::default())
    }
}

impl EngineCore {
    pub(crate) fn code(&self, code_id: CodeId) -> EngineResult<CodeRecord> {
        self.codes
            .read()
            .get(&code_id)
            .cloned()
            .ok_or(EngineError::UnknownCode(code_id))
    }
}

#[derive(Clone, Copy)]
enum Entrypoint {
    Instantiate,
    Execute,
}

struct ProcessedResponse {
    events: Vec<Event>,
    data: Option<Binary>,
}

struct DispatchedMessage {
    events: Vec<Event>,
    reply_events: Vec<Event>,
    data: Option<Binary>,
    msg_responses: Vec<MsgResponse>,
}

struct SubmessageOutcome {
    events: Vec<Event>,
    data_candidate: Option<Binary>,
}

#[allow(clippy::too_many_arguments)]
fn invoke_contract(
    core: Arc<EngineCore>,
    tx: SharedTx,
    block: BlockContext,
    contract: Address,
    caller: Address,
    funds: Vec<Coin>,
    msg: Binary,
    depth: u32,
    entrypoint: Entrypoint,
) -> EngineResult<Response<Empty>> {
    ensure_depth(&core, depth)?;
    let code_id = code_id_of(&tx, &contract)?;
    let artifact = core.code(code_id)?.artifact;
    let env = make_env(&block, &contract);

    match artifact {
        CodeArtifact::Native(native) => {
            let transaction_id = tx.lock().transaction_id;
            let mut context =
                NativeCallContext::new(transaction_id, block, contract, caller.clone(), depth, tx);
            let result = match entrypoint {
                Entrypoint::Instantiate => native.instantiate(
                    &mut context,
                    env,
                    MessageInfo {
                        sender: Addr::unchecked(caller.as_str()),
                        funds,
                    },
                    msg,
                ),
                Entrypoint::Execute => native.execute(
                    &mut context,
                    env,
                    MessageInfo {
                        sender: Addr::unchecked(caller.as_str()),
                        funds,
                    },
                    msg,
                ),
            };
            result.map_err(EngineError::Native)
        }
        CodeArtifact::Wasm(checksum) => {
            let backend = Backend {
                api: EngineApi,
                storage: EngineStorage::new(contract.clone(), depth, tx.clone()),
                querier: EngineQuerier::new(core.clone(), tx, contract, block, depth),
            };
            let mut instance = core.wasm_cache.get_instance(
                &checksum,
                backend,
                InstanceOptions {
                    gas_limit: core.config.gas_limit,
                },
            )?;
            let info = MessageInfo {
                sender: Addr::unchecked(caller.as_str()),
                funds,
            };
            let result: ContractResult<Response<Empty>> = match entrypoint {
                Entrypoint::Instantiate => call_instantiate(&mut instance, &env, &info, &msg)?,
                Entrypoint::Execute => call_execute(&mut instance, &env, &info, &msg)?,
            };
            drop(instance.recycle());
            result.into_result().map_err(EngineError::Contract)
        }
    }
}

fn invoke_reply(
    core: Arc<EngineCore>,
    tx: SharedTx,
    block: BlockContext,
    contract: Address,
    reply: Reply,
    depth: u32,
) -> EngineResult<Response<Empty>> {
    ensure_depth(&core, depth)?;
    let code_id = code_id_of(&tx, &contract)?;
    let artifact = core.code(code_id)?.artifact;
    let env = make_env(&block, &contract);
    match artifact {
        CodeArtifact::Native(native) => {
            let transaction_id = tx.lock().transaction_id;
            let mut context = NativeCallContext::new(
                transaction_id,
                block,
                contract.clone(),
                contract,
                depth,
                tx,
            );
            native
                .reply(&mut context, env, reply)
                .map_err(EngineError::Native)
        }
        CodeArtifact::Wasm(checksum) => {
            let backend = Backend {
                api: EngineApi,
                storage: EngineStorage::new(contract.clone(), depth, tx.clone()),
                querier: EngineQuerier::new(core.clone(), tx, contract, block, depth),
            };
            let mut instance = core.wasm_cache.get_instance(
                &checksum,
                backend,
                InstanceOptions {
                    gas_limit: core.config.gas_limit,
                },
            )?;
            let result: ContractResult<Response<Empty>> = call_reply(&mut instance, &env, &reply)?;
            drop(instance.recycle());
            result.into_result().map_err(EngineError::Contract)
        }
    }
}

pub(crate) fn query_contract_shared(
    core: Arc<EngineCore>,
    tx: SharedTx,
    block: BlockContext,
    contract: Address,
    msg: Binary,
    depth: u32,
) -> EngineResult<Binary> {
    ensure_depth(&core, depth)?;
    let checkpoint = tx.lock().clone();
    let trace_start = checkpoint.accesses.len();
    let code_id = code_id_of(&tx, &contract)?;
    let artifact = core.code(code_id)?.artifact;
    let env = make_env(&block, &contract);

    let result = match artifact {
        CodeArtifact::Native(native) => {
            let mut context = NativeCallContext::new(
                tx.lock().transaction_id,
                block,
                contract.clone(),
                Address::new("query"),
                depth,
                tx.clone(),
            );
            native
                .query(&mut context, env, msg)
                .map_err(EngineError::Native)
        }
        CodeArtifact::Wasm(checksum) => {
            let backend = Backend {
                api: EngineApi,
                storage: EngineStorage::new(contract.clone(), depth, tx.clone()),
                querier: EngineQuerier::new(core.clone(), tx.clone(), contract, block, depth),
            };
            let mut instance = core.wasm_cache.get_instance(
                &checksum,
                backend,
                InstanceOptions {
                    gas_limit: core.config.gas_limit,
                },
            )?;
            let result: ContractResult<Binary> = call_query(&mut instance, &env, &msg)?;
            drop(instance.recycle());
            result.into_result().map_err(EngineError::Contract)
        }
    };

    let current = tx.lock().clone();
    let mut attempted = current.accesses[trace_start..].to_vec();
    let wrote = attempted.iter().any(|access| {
        matches!(
            access.kind,
            AccessKind::StorageWrite | AccessKind::StorageRemove | AccessKind::BankWrite
        )
    }) || current.created_contracts != checkpoint.created_contracts;
    if wrote {
        let mut restored = checkpoint;
        for access in &mut attempted {
            access.reverted = true;
        }
        restored.accesses.extend(attempted);
        *tx.lock() = restored;
        return Err(EngineError::Contract(
            "query attempted to mutate state".to_owned(),
        ));
    }

    result
}

fn process_response(
    core: Arc<EngineCore>,
    tx: SharedTx,
    block: BlockContext,
    contract: Address,
    response: Response<Empty>,
    depth: u32,
) -> EngineResult<ProcessedResponse> {
    let mut events = normalize_response_events(&contract, response.attributes, response.events);
    let mut data = response.data;

    for submessage in response.messages {
        let child = dispatch_submessage(
            core.clone(),
            tx.clone(),
            block.clone(),
            contract.clone(),
            submessage,
            depth + 1,
        )?;
        events.extend(child.events);
        if let Some(candidate) = child.data_candidate {
            data = Some(candidate);
        }
    }

    Ok(ProcessedResponse { events, data })
}

#[allow(deprecated)]
fn dispatch_submessage(
    core: Arc<EngineCore>,
    tx: SharedTx,
    block: BlockContext,
    caller_contract: Address,
    submessage: SubMsg<Empty>,
    depth: u32,
) -> EngineResult<SubmessageOutcome> {
    ensure_depth(&core, depth)?;
    let checkpoint = tx.lock().clone();
    let trace_start = checkpoint.accesses.len();
    let result = dispatch_message(
        core.clone(),
        tx.clone(),
        block.clone(),
        caller_contract.clone(),
        submessage.msg,
        depth,
    );

    let succeeded = result.is_ok();
    if !succeeded {
        let current = tx.lock().clone();
        let mut restored = checkpoint;
        let mut attempted = current.accesses[trace_start..].to_vec();
        for access in &mut attempted {
            access.reverted = true;
        }
        restored.accesses.extend(attempted);
        *tx.lock() = restored;
    }

    let should_reply = matches!(submessage.reply_on, ReplyOn::Always)
        || (succeeded && matches!(submessage.reply_on, ReplyOn::Success))
        || (!succeeded && matches!(submessage.reply_on, ReplyOn::Error));

    if should_reply {
        let (emitted_events, sub_result) = match &result {
            Ok(dispatched) => (
                dispatched.events.clone(),
                SubMsgResult::Ok(SubMsgResponse {
                    events: dispatched.reply_events.clone(),
                    data: dispatched.data.clone(),
                    msg_responses: dispatched.msg_responses.clone(),
                }),
            ),
            Err(error) => (Vec::new(), SubMsgResult::Err(error.to_string())),
        };
        let reply = Reply {
            id: submessage.id,
            payload: submessage.payload,
            gas_used: 0,
            result: sub_result,
        };
        let response = invoke_reply(
            core.clone(),
            tx.clone(),
            block.clone(),
            caller_contract.clone(),
            reply,
            depth,
        )?;
        let reply_processed = process_response(core, tx, block, caller_contract, response, depth)?;
        let mut events = emitted_events;
        events.extend(reply_processed.events);
        return Ok(SubmessageOutcome {
            events,
            data_candidate: reply_processed.data,
        });
    }

    result.map(|dispatched| SubmessageOutcome {
        events: dispatched.events,
        data_candidate: None,
    })
}

fn dispatch_message(
    core: Arc<EngineCore>,
    tx: SharedTx,
    block: BlockContext,
    caller_contract: Address,
    message: CosmosMsg<Empty>,
    depth: u32,
) -> EngineResult<DispatchedMessage> {
    match message {
        CosmosMsg::Bank(BankMsg::Send { to_address, amount }) => {
            let target = Address::new(to_address);
            validate_public_address(&target)?;
            tx.lock()
                .transfer(&caller_contract, &target, &amount, &caller_contract, depth)?;
            let events = vec![Event::new("transfer")
                .add_attribute("sender", caller_contract.as_str())
                .add_attribute("recipient", target.as_str())];
            Ok(DispatchedMessage {
                reply_events: events.clone(),
                events,
                data: None,
                msg_responses: vec![MsgResponse {
                    type_url: BANK_SEND_RESPONSE_TYPE_URL.to_owned(),
                    value: Binary::default(),
                }],
            })
        }
        CosmosMsg::Bank(BankMsg::Burn { amount }) => {
            tx.lock()
                .burn(&caller_contract, &amount, &caller_contract, depth)?;
            let events = vec![Event::new("burn").add_attribute("sender", caller_contract.as_str())];
            Ok(DispatchedMessage {
                reply_events: events.clone(),
                events,
                data: None,
                msg_responses: vec![MsgResponse {
                    type_url: BANK_BURN_RESPONSE_TYPE_URL.to_owned(),
                    value: Binary::default(),
                }],
            })
        }
        CosmosMsg::Wasm(WasmMsg::Execute {
            contract_addr,
            msg,
            funds,
        }) => {
            let target = Address::new(contract_addr);
            validate_public_address(&target)?;
            code_id_of(&tx, &target)?;
            tx.lock()
                .transfer(&caller_contract, &target, &funds, &caller_contract, depth)?;
            let response = invoke_contract(
                core.clone(),
                tx.clone(),
                block.clone(),
                target.clone(),
                caller_contract,
                funds,
                msg,
                depth,
                Entrypoint::Execute,
            )?;
            let processed = process_response(core, tx, block, target, response, depth)?;
            let response_bytes =
                encode_execute_response(processed.data.as_ref().map(Binary::as_slice));
            Ok(DispatchedMessage {
                reply_events: processed.events.clone(),
                events: processed.events,
                data: Some(Binary::new(response_bytes.clone())),
                msg_responses: vec![MsgResponse {
                    type_url: EXECUTE_RESPONSE_TYPE_URL.to_owned(),
                    value: Binary::new(response_bytes),
                }],
            })
        }
        CosmosMsg::Wasm(WasmMsg::Instantiate {
            admin,
            code_id,
            msg,
            funds,
            label,
        }) => {
            validate_contract_label(&label)?;
            let code_id = CodeId(code_id);
            let code = core.code(code_id)?;
            let address = tx
                .lock()
                .allocate_contract_address(&core.config.contract_address_prefix)?;
            let admin = admin.map(Address::new);
            if let Some(admin) = &admin {
                validate_public_address(admin)?;
            }
            tx.lock().create_contract(ContractMetadata {
                address: address.clone(),
                code_id,
                code_checksum: code.metadata.checksum,
                creator: caller_contract.clone(),
                admin,
                label,
            })?;
            tx.lock()
                .transfer(&caller_contract, &address, &funds, &caller_contract, depth)?;
            let response = invoke_contract(
                core.clone(),
                tx.clone(),
                block.clone(),
                address.clone(),
                caller_contract,
                funds,
                msg,
                depth,
                Entrypoint::Instantiate,
            )?;
            let mut processed =
                process_response(core, tx, block, address.clone(), response, depth)?;
            processed.events.insert(
                0,
                Event::new("instantiate").add_attribute("_contract_address", address.as_str()),
            );
            let response_bytes = encode_instantiate_response(
                &address,
                processed.data.as_ref().map(Binary::as_slice),
            );
            Ok(DispatchedMessage {
                reply_events: processed.events.clone(),
                events: processed.events,
                data: Some(Binary::new(response_bytes.clone())),
                msg_responses: vec![MsgResponse {
                    type_url: INSTANTIATE_RESPONSE_TYPE_URL.to_owned(),
                    value: Binary::new(response_bytes),
                }],
            })
        }
        other => Err(EngineError::UnsupportedMessage(format!("{other:?}"))),
    }
}

fn normalize_response_events(
    contract: &Address,
    attributes: Vec<Attribute>,
    custom_events: Vec<Event>,
) -> Vec<Event> {
    let mut events = Vec::with_capacity(custom_events.len() + usize::from(!attributes.is_empty()));
    if !attributes.is_empty() {
        let mut wasm_event =
            Event::new("wasm").add_attribute("_contract_address", contract.as_str());
        for attribute in attributes {
            wasm_event = wasm_event.add_attribute(attribute.key, attribute.value);
        }
        events.push(wasm_event);
    }

    for mut event in custom_events {
        event.ty = format!("wasm-{}", event.ty);
        event.attributes.insert(
            0,
            Attribute {
                key: "_contract_address".to_owned(),
                value: contract.to_string(),
            },
        );
        events.push(event);
    }
    events
}

fn make_env(block: &BlockContext, contract: &Address) -> Env {
    Env {
        block: BlockInfo {
            height: block.height,
            time: Timestamp::from_nanos(block.time_nanos),
            chain_id: block.chain_id.clone(),
        },
        transaction: block
            .transaction_index
            .map(|index| TransactionInfo { index }),
        contract: ContractInfo {
            address: Addr::unchecked(contract.as_str()),
        },
    }
}

fn ensure_depth(core: &EngineCore, depth: u32) -> EngineResult<()> {
    if depth > core.config.max_call_depth {
        Err(EngineError::MaxCallDepth(core.config.max_call_depth))
    } else {
        Ok(())
    }
}

fn validate_public_address(address: &Address) -> EngineResult<()> {
    validate_address(address.as_str()).map_err(|reason| EngineError::InvalidAddress {
        address: address.to_string(),
        reason,
    })
}

fn validate_code_label(label: &str) -> EngineResult<()> {
    if label.trim().is_empty() {
        return Err(EngineError::InvalidCodeLabel {
            label: label.to_owned(),
            reason: "label must not be empty".to_owned(),
        });
    }
    if label.len() > 256 {
        return Err(EngineError::InvalidCodeLabel {
            label: label.to_owned(),
            reason: "label exceeds 256 bytes".to_owned(),
        });
    }
    Ok(())
}

fn validate_contract_label(label: &str) -> EngineResult<()> {
    if label.trim().is_empty() {
        return Err(EngineError::InvalidContractLabel {
            label: label.to_owned(),
            reason: "label must not be empty".to_owned(),
        });
    }
    if label.len() > 128 {
        return Err(EngineError::InvalidContractLabel {
            label: label.to_owned(),
            reason: "label exceeds 128 bytes".to_owned(),
        });
    }
    Ok(())
}

fn validate_balance_seed(coins: &[Coin]) -> EngineResult<()> {
    let mut denoms = BTreeSet::new();
    for coin in coins {
        if coin.denom.is_empty() {
            return Err(EngineError::InvalidCoin {
                denom: coin.denom.clone(),
                amount: coin.amount.u128(),
                reason: "denomination must not be empty".to_owned(),
            });
        }
        if !denoms.insert(coin.denom.clone()) {
            return Err(EngineError::DuplicateDenomination(coin.denom.clone()));
        }
    }
    Ok(())
}

fn checksum_from_vm(checksum: cosmwasm_std::Checksum) -> CodeChecksum {
    let mut bytes = [0_u8; 32];
    bytes.copy_from_slice(checksum.as_slice());
    CodeChecksum::new(bytes)
}

fn checksum_native_label(label: &str) -> CodeChecksum {
    let mut hasher = Sha256::new();
    hasher.update(b"acg-native-code-v1\0");
    hasher.update(label.as_bytes());
    checksum_from_digest(hasher.finalize())
}

fn checksum_from_digest(digest: impl AsRef<[u8]>) -> CodeChecksum {
    let mut bytes = [0_u8; 32];
    bytes.copy_from_slice(digest.as_ref());
    CodeChecksum::new(bytes)
}

fn encode_execute_response(data: Option<&[u8]>) -> Vec<u8> {
    let mut output = Vec::new();
    if let Some(data) = data.filter(|data| !data.is_empty()) {
        encode_length_delimited(1, data, &mut output);
    }
    output
}

fn encode_instantiate_response(address: &Address, data: Option<&[u8]>) -> Vec<u8> {
    let mut output = Vec::new();
    encode_length_delimited(1, address.as_str().as_bytes(), &mut output);
    if let Some(data) = data.filter(|data| !data.is_empty()) {
        encode_length_delimited(2, data, &mut output);
    }
    output
}

fn encode_length_delimited(field_number: u32, value: &[u8], output: &mut Vec<u8>) {
    encode_varint(u64::from((field_number << 3) | 2), output);
    encode_varint(value.len() as u64, output);
    output.extend_from_slice(value);
}

fn encode_varint(mut value: u64, output: &mut Vec<u8>) {
    while value >= 0x80 {
        output.push((value as u8) | 0x80);
        value >>= 7;
    }
    output.push(value as u8);
}
