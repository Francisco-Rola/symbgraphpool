use std::{collections::BTreeMap, env, fs, io::{self, BufRead, BufReader, BufWriter, Write}, path::{Path, PathBuf}};

use acg_cosmwasm_engine::{
    AccessKind, Address, BlockContext, BundleCall, CosmWasmEngine, EngineConfig, EngineError,
    ScopedBundleCall, TransactionId, WasmInstanceLifecycle,
};
use cosmwasm_std::{Binary, Coin, Uint128};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Deserialize)]
struct Manifest {
    wasm_artifacts: BTreeMap<String, String>,
    instances: Vec<InstanceSpec>,
    #[serde(default)]
    bank_seeds: Vec<BankSeed>,
    #[serde(default)]
    priming_calls: Vec<CallSpec>,
}

#[derive(Debug, Deserialize)]
struct InstanceSpec {
    instance_id: String,
    family: String,
    instantiate_msg: Value,
}

#[derive(Debug, Deserialize)]
struct BankSeed { address: String, denom: String, amount: String }

#[derive(Debug, Deserialize)]
struct ExecutionBlock { block_number: u64, timestamp: u64, transactions: Vec<ExecutionTx> }

#[derive(Debug, Deserialize)]
struct ExecutionTx {
    tx_index: usize,
    tx_hash: String,
    source_failed: bool,
    calls: Vec<CallSpec>,
    #[serde(default)]
    skipped_actions: usize,
}

#[derive(Clone, Debug, Deserialize)]
struct CallSpec {
    kind: String,
    #[serde(default)] family: Option<String>,
    #[serde(default)] instance_id: Option<String>,
    #[serde(default)] sender: Option<String>,
    #[serde(default)] msg: Option<Value>,
    #[serde(default)] funds: Vec<CoinSpec>,
    #[serde(default)] from: Option<String>,
    #[serde(default)] to: Option<String>,
    #[serde(default)] coins: Vec<CoinSpec>,
    #[serde(default)] origin_action_id: Option<usize>,
    #[serde(default)] source_revert_scope_action_id: Option<u64>,
}

#[derive(Clone, Debug, Deserialize)]
struct CoinSpec { denom: String, amount: String }

#[derive(Debug, Serialize)]
struct OutputBlock { block_number: u64, transactions: Vec<OutputTx> }

#[derive(Debug, Serialize)]
struct OutputTx {
    tx_index: usize,
    tx_hash: String,
    source_failed: bool,
    execution_status: String,
    semantic_calls: usize,
    skipped_actions: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    native_failed_call_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    native_failed_origin_action_id: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    native_failure: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    reverted_internal_scopes: Vec<OutputRevertedScope>,
    accesses: Vec<OutputAccess>,
}

#[derive(Debug, Serialize)]
struct OutputRevertedScope {
    scope_action_id: u64,
    first_call_index: usize,
    last_call_index: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    native_failed_call_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    native_failure: Option<String>,
}

#[derive(Debug, Serialize)]
struct OutputAccess {
    kind: &'static str,
    contract: String,
    key_hex: String,
    range_end_hex: Option<String>,
    reverted: bool,
    call_depth: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    bundle_call_index: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    family: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    instance_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    origin_action_id: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    semantic_action: Option<String>,
}

#[derive(Debug)]
struct Args { repo_root: PathBuf, manifest: PathBuf, plan: PathBuf, output: PathBuf }

type AnyError = Box<dyn std::error::Error>;
fn invalid(msg: impl Into<String>) -> AnyError { io::Error::new(io::ErrorKind::InvalidInput, msg.into()).into() }

fn parse_args() -> Result<Args, AnyError> {
    let mut repo_root = PathBuf::from(".");
    let mut manifest = None; let mut plan = None; let mut output = None;
    let mut it = env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--repo-root" => repo_root = PathBuf::from(it.next().ok_or_else(|| invalid("missing value after --repo-root"))?),
            "--manifest" => manifest = Some(PathBuf::from(it.next().ok_or_else(|| invalid("missing value after --manifest"))?)),
            "--plan" => plan = Some(PathBuf::from(it.next().ok_or_else(|| invalid("missing value after --plan"))?)),
            "--output" => output = Some(PathBuf::from(it.next().ok_or_else(|| invalid("missing value after --output"))?)),
            "-h" | "--help" => return Err(invalid("usage: acg-vegeta-native-s3-executor --manifest FILE --plan FILE --output FILE [--repo-root ROOT]")),
            _ => return Err(invalid(format!("unknown argument: {arg}"))),
        }
    }
    Ok(Args { repo_root, manifest: manifest.ok_or_else(|| invalid("--manifest is required"))?, plan: plan.ok_or_else(|| invalid("--plan is required"))?, output: output.ok_or_else(|| invalid("--output is required"))? })
}

fn resolve(root: &Path, path: &str) -> PathBuf { let p=PathBuf::from(path); if p.is_absolute(){p}else{root.join(p)} }
fn binary_json(v: &Value) -> Result<Binary,AnyError> { Ok(Binary::from(serde_json::to_vec(v)?)) }
fn coins(rows: &[CoinSpec]) -> Result<Vec<Coin>,AnyError> {
    rows.iter().map(|c| Ok(Coin{denom:c.denom.clone(), amount:Uint128::new(c.amount.parse::<u128>()?)})).collect()
}

fn build_call(spec: &CallSpec, addresses: &BTreeMap<String,Address>) -> Result<BundleCall,AnyError> {
    match spec.kind.as_str() {
        "execute" => {
            let iid=spec.instance_id.as_ref().ok_or_else(|| invalid("execute call missing instance_id"))?;
            let contract=addresses.get(iid).ok_or_else(|| invalid(format!("unknown instance {iid}")))?.clone();
            Ok(BundleCall::Execute { sender:Address::new(spec.sender.clone().ok_or_else(|| invalid("execute call missing sender"))?), contract, funds:coins(&spec.funds)?, msg:binary_json(spec.msg.as_ref().ok_or_else(|| invalid("execute call missing msg"))?)? })
        }
        "query" => {
            let iid=spec.instance_id.as_ref().ok_or_else(|| invalid("query call missing instance_id"))?;
            let contract=addresses.get(iid).ok_or_else(|| invalid(format!("unknown instance {iid}")))?.clone();
            Ok(BundleCall::Query { contract, msg:binary_json(spec.msg.as_ref().ok_or_else(|| invalid("query call missing msg"))?)? })
        }
        "bank_send" => Ok(BundleCall::BankSend { from:Address::new(spec.from.clone().ok_or_else(|| invalid("bank_send missing from"))?), to:Address::new(spec.to.clone().ok_or_else(|| invalid("bank_send missing to"))?), coins:coins(&spec.coins)? }),
        "noop" => Ok(BundleCall::Noop),
        other => Err(invalid(format!("unsupported call kind {other}"))),
    }
}

fn semantic_action(spec: &CallSpec) -> String {
    if let Some(msg) = spec.msg.as_ref().and_then(Value::as_object) {
        if let Some(name) = msg.keys().next() {
            return format!("{}::{name}", spec.kind);
        }
    }
    spec.kind.clone()
}

fn access_kind(kind: &AccessKind) -> &'static str {
    match kind {
        AccessKind::StorageRead => "storage_read", AccessKind::StorageScan => "storage_scan",
        AccessKind::StorageWrite => "storage_write", AccessKind::StorageRemove => "storage_remove",
        AccessKind::BankRead => "bank_read", AccessKind::BankWrite => "bank_write",
    }
}

fn main() -> Result<(), AnyError> {
    let args=parse_args()?;
    let manifest:Manifest=serde_json::from_slice(&fs::read(&args.manifest)?)?;
    let engine=CosmWasmEngine::new(EngineConfig{gas_limit:u64::MAX,wasm_instance_lifecycle:WasmInstanceLifecycle::Recycle,..EngineConfig::default()});
    let mut codes=BTreeMap::new();
    for (family,path) in &manifest.wasm_artifacts {
        let bytes=fs::read(resolve(&args.repo_root,path)).map_err(|e| invalid(format!("failed to read {family} Wasm {path}: {e}")))?;
        codes.insert(family.clone(),engine.upload_wasm(bytes)?);
    }
    for seed in &manifest.bank_seeds {
        engine.set_balance(Address::new(seed.address.clone()), &[Coin{denom:seed.denom.clone(),amount:Uint128::new(seed.amount.parse()?) }])?;
    }
    let mut addresses=BTreeMap::new();
    let setup_block=BlockContext{height:16_774_644,time_nanos:1_678_170_000_000_000_000,chain_id:"vegeta-s3-native".to_owned(),transaction_index:Some(0)};
    for (i,spec) in manifest.instances.iter().enumerate() {
        let code=*codes.get(&spec.family).ok_or_else(|| invalid(format!("missing code for {}",spec.family)))?;
        let outcome=engine.instantiate(TransactionId(1_000_000+i as u64),setup_block.clone(),Address::new("native-s3-admin"),code,None,spec.instance_id.clone(),vec![],binary_json(&spec.instantiate_msg)?)?;
        addresses.insert(spec.instance_id.clone(),outcome.contract);
    }
    for (i,spec) in manifest.priming_calls.iter().enumerate() {
        let call=build_call(spec,&addresses)?;
        engine.execute_bundle(TransactionId(2_000_000+i as u64),setup_block.clone(),&[call]).map_err(|e| invalid(format!("priming call {i} failed (family={:?} instance={:?} origin={:?}): {e}",spec.family,spec.instance_id,spec.origin_action_id)))?;
    }
    if let Some(parent)=args.output.parent(){fs::create_dir_all(parent)?;}
    let reader=BufReader::new(fs::File::open(&args.plan)?); let mut writer=BufWriter::new(fs::File::create(&args.output)?);
    let mut blocks=0usize; let mut txs=0usize; let mut calls_total=0usize;
    for line in reader.lines() {
        let line=line?; if line.trim().is_empty(){continue;}
        let b:ExecutionBlock=serde_json::from_str(&line)?; let mut out_txs=Vec::with_capacity(b.transactions.len());
        for tx in b.transactions {
            let calls:Vec<BundleCall>=tx.calls.iter().map(|c|build_call(c,&addresses)).collect::<Result<_,_>>().map_err(|e: AnyError| invalid(format!("block {} tx {} {}: {e}",b.block_number,tx.tx_index,tx.tx_hash)))?;
            let block=BlockContext{height:b.block_number,time_nanos:b.timestamp.saturating_mul(1_000_000_000),chain_id:"vegeta-s3-native".to_owned(),transaction_index:Some(tx.tx_index as u32)};
            let tid=TransactionId(((b.block_number-16_774_645)*100_000 + tx.tx_index as u64)+10_000_000);
            let scoped_calls:Vec<ScopedBundleCall>=calls.iter().cloned().zip(tx.calls.iter()).map(|(call,spec)|ScopedBundleCall{call,source_revert_scope:spec.source_revert_scope_action_id}).collect();
            let has_internal_revert_scopes=tx.calls.iter().any(|c|c.source_revert_scope_action_id.is_some());
            let result=if tx.source_failed {
                engine.execute_bundle_reverted_tolerant(tid,block,&calls)
            } else if has_internal_revert_scopes {
                engine.execute_bundle_with_reverted_scopes(tid,block,&scoped_calls)
            } else {
                engine.execute_bundle(tid,block,&calls)
            };
            let outcome=result.map_err(|e| {
                if let EngineError::BundleCallFailed { call_index, error } = &e {
                    let spec=tx.calls.get(*call_index);
                    let msg=spec.and_then(|c|c.msg.as_ref()).map(|v|v.to_string()).unwrap_or_else(|| "-".to_owned());
                    invalid(format!(
                        "native S3 execution failed at block {} tx {} {} (semantic calls={}): strict native call {} failed; family={:?} instance={:?} sender={:?} origin={:?} kind={:?} msg={} error={}",
                        b.block_number,tx.tx_index,tx.tx_hash,calls.len(),call_index,
                        spec.and_then(|c|c.family.as_ref()),spec.and_then(|c|c.instance_id.as_ref()),
                        spec.and_then(|c|c.sender.as_ref()),spec.and_then(|c|c.origin_action_id),
                        spec.map(|c|c.kind.as_str()),msg,error
                    ))
                } else {
                    invalid(format!("native S3 execution failed at block {} tx {} {} (semantic calls={}): {e}",b.block_number,tx.tx_index,tx.tx_hash,calls.len()))
                }
            })?;
            let failed_call_index=outcome.failure.as_ref().map(|f|f.call_index);
            let failed_origin_action_id=failed_call_index.and_then(|i|tx.calls.get(i)).and_then(|c|c.origin_action_id);
            let native_failure=outcome.failure.as_ref().map(|f|f.error.clone());
            if let Some(failure)=outcome.failure.as_ref() {
                if !tx.source_failed {
                    return Err(invalid(format!("unexpected tolerant native failure for successful source tx at block {} tx {} {} call {}: {}",b.block_number,tx.tx_index,tx.tx_hash,failure.call_index,failure.error)));
                }
                let spec=tx.calls.get(failure.call_index);
                eprintln!(
                    "source-reverted tx {}:{} {} stopped at native call {} family={:?} instance={:?} origin={:?}: {}",
                    b.block_number,tx.tx_index,tx.tx_hash,failure.call_index,
                    spec.and_then(|c|c.family.as_ref()),spec.and_then(|c|c.instance_id.as_ref()),
                    spec.and_then(|c|c.origin_action_id),failure.error
                );
            }
            let reverted_internal_scopes=outcome.reverted_scopes.iter().map(|scope|OutputRevertedScope{
                scope_action_id:scope.scope_id,
                first_call_index:scope.first_call_index,
                last_call_index:scope.last_call_index,
                native_failed_call_index:scope.failure.as_ref().map(|f|f.call_index),
                native_failure:scope.failure.as_ref().map(|f|f.error.clone()),
            }).collect::<Vec<_>>();
            for scope in &outcome.reverted_scopes {
                if let Some(failure)=scope.failure.as_ref() {
                    let spec=tx.calls.get(failure.call_index);
                    eprintln!(
                        "caught source-reverted internal scope {} in successful tx {}:{} {} stopped at native call {} family={:?} instance={:?} origin={:?}: {}",
                        scope.scope_id,b.block_number,tx.tx_index,tx.tx_hash,failure.call_index,
                        spec.and_then(|c|c.family.as_ref()),spec.and_then(|c|c.instance_id.as_ref()),
                        spec.and_then(|c|c.origin_action_id),failure.error
                    );
                }
            }
            let committed=outcome.committed;
            let mut access_call_index=vec![None;outcome.accesses.len()];
            for span in &outcome.call_access_spans {
                let start=span.access_start.min(access_call_index.len());
                let end=span.access_end.min(access_call_index.len());
                for slot in &mut access_call_index[start..end] {
                    *slot=Some(span.call_index);
                }
            }
            let accesses=outcome.accesses.into_iter().enumerate().map(|(access_index,a)|{
                let bundle_call_index=access_call_index.get(access_index).copied().flatten();
                let spec=bundle_call_index.and_then(|i|tx.calls.get(i));
                OutputAccess{
                    kind:access_kind(&a.kind),
                    contract:a.contract.to_string(),
                    key_hex:hex::encode(a.key),
                    range_end_hex:a.range_end.map(hex::encode),
                    reverted:a.reverted,
                    call_depth:a.call_depth,
                    bundle_call_index,
                    family:spec.and_then(|c|c.family.clone()),
                    instance_id:spec.and_then(|c|c.instance_id.clone()),
                    origin_action_id:spec.and_then(|c|c.origin_action_id),
                    semantic_action:spec.map(semantic_action),
                }
            }).collect();
            calls_total+=calls.len(); txs+=1;
            out_txs.push(OutputTx{tx_index:tx.tx_index,tx_hash:tx.tx_hash,source_failed:tx.source_failed,execution_status:if committed{"committed"}else{"reverted"}.to_owned(),semantic_calls:calls.len(),skipped_actions:tx.skipped_actions,native_failed_call_index:failed_call_index,native_failed_origin_action_id:failed_origin_action_id,native_failure,reverted_internal_scopes,accesses});
        }
        serde_json::to_writer(&mut writer,&OutputBlock{block_number:b.block_number,transactions:out_txs})?; writer.write_all(b"\n")?; blocks+=1;
        eprintln!("native-s3 block {} complete ({blocks}/101)",b.block_number);
    }
    writer.flush()?;
    eprintln!("native-s3 execution complete: blocks={blocks} transactions={txs} semantic_calls={calls_total}");
    if blocks!=101 { return Err(invalid(format!("expected 101 blocks, executed {blocks}"))); }
    Ok(())
}
