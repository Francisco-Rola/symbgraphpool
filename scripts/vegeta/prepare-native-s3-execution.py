#!/usr/bin/env python3
"""Compile the translated Vegeta S3 call plan into executable CosmWasm bundles.

This stage never reads the source corpus's concrete read/write keys. It consumes only call-plan
metadata, verified selector rules, and implementation metadata. Concrete EVM reads/writes remain
reserved for the post-execution fidelity evaluator.
"""
from __future__ import annotations

import argparse, hashlib, json, os, time, urllib.error, urllib.request
from collections import defaultdict
from pathlib import Path
from typing import Any

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_PLAN = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/native-plan/native-plan.jsonl"
DEFAULT_SELECTOR = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/native-plan/selector-semantic-map.json"
DEFAULT_CODE_CACHE = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/characterization/code-cache.json"
DEFAULT_IMPL = ROOT / "evaluation/vegeta/s3-native-implementation-manifest.v1.json"
DEFAULT_OUT = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/native-execution"
SEED = 10**24
PAIR_RESERVE = 10**15
U64_MAX = (1 << 64) - 1


class TokenIdRemapper:
    """Map arbitrary source token IDs to a collision-free u64 domain per native instance.

    Ethereum NFT/ERC1155 IDs are uint256 while the native benchmark contracts intentionally use
    u64 storage keys.  A modulo/truncation mapping could merge distinct source tokens and invent
    false conflicts, so the execution preparation stage assigns a dense per-instance ID on first
    observation.  Equality is preserved exactly within each instance.
    """

    def __init__(self):
        self._maps: dict[str, dict[int, int]] = defaultdict(dict)

    def map(self, instance_id: str, source_token_id: Any) -> int:
        raw = intv(source_token_id)
        mapping = self._maps[instance_id]
        if raw in mapping:
            return mapping[raw]
        native = len(mapping) + 1
        if native > U64_MAX:
            raise OverflowError(f"too many distinct token IDs for native instance {instance_id}")
        mapping[raw] = native
        return native

    def summary(self) -> dict[str, Any]:
        counts = {iid: len(mapping) for iid, mapping in sorted(self._maps.items())}
        return {
            "policy": "per-instance-dense-u64-bijection",
            "instances": len(counts),
            "distinct_source_token_ids": sum(counts.values()),
            "max_distinct_ids_per_instance": max(counts.values(), default=0),
            "collision_free": True,
            "purpose": (
                "preserve source token-ID equality/conflict topology without truncating uint256 IDs "
                "into the native u64 storage-key domain"
            ),
        }


def erc721_transfer_parts(data: str, args: dict[str, Any]) -> tuple[str, str, int]:
    """Decode ERC721 `(from, to, tokenId)` without aliasing word 0 as the token ID."""
    owner = norm_addr(args.get("owner") or args.get("from")) or abi_addr(data, 0)
    recipient = norm_addr(args.get("recipient") or args.get("to")) or abi_addr(data, 1)
    token_id = intv(args.get("token_id", abi_uint(data, 2)))
    return owner, recipient, token_id


def note_nft_initial_owner(
    owners: dict[str, dict[int, str]],
    priorities: dict[tuple[str, int], int],
    instance_id: str,
    token_id: int,
    owner: str,
    priority: int,
) -> None:
    """Keep the strongest first observation of a token's pre-execution owner."""
    key = (instance_id, token_id)
    old = priorities.get(key, -1)
    if priority > old:
        owners[instance_id][token_id] = owner
        priorities[key] = priority


def read_json(path: Path) -> Any: return json.loads(path.read_text())
def norm_addr(v: Any) -> str | None:
    if v is None: return None
    s=str(v).lower(); s=s[2:] if s.startswith('0x') else s
    if len(s)!=40: return None
    try: int(s,16)
    except ValueError: return None
    return '0x'+s

def intv(v: Any) -> int:
    if v is None: return 0
    if isinstance(v,int): return v
    s=str(v).strip().lower(); return int(s,16) if s.startswith('0x') else int(s or '0')

def amount(v: Any) -> int:
    x=intv(v)
    return 0 if x==0 else 1 + (x % 1_000_000)

def approval_amount(v: Any) -> int:
    """Normalize ERC20-style approvals without breaking successful transferFrom paths.

    Ordinary transfer amounts are folded into a small positive domain, but independently folding an
    approval can invert the source relation `allowance >= spend` (e.g. uint256::MAX approval folding
    below a later transfer amount).  For topology fidelity we only need zero-vs-positive approval and
    the allowance storage key.  Therefore zero stays zero and every positive approval becomes the
    large SEED sentinel used for pre-state priming.
    """
    return 0 if intv(v) == 0 else SEED

def runtime_family(code: Any) -> str | None:
    s=str(code or '').lower(); s=s[2:] if s.startswith('0x') else s
    if not s: return None
    return hashlib.sha256(bytes.fromhex(s)).hexdigest()

def word(data: str, i: int) -> bytes:
    raw=bytes.fromhex(data[2:] if data.startswith('0x') else data)
    start=4+32*i
    return raw[start:start+32].ljust(32,b'\0')
def abi_addr(data: str,i:int)->str: return '0x'+word(data,i)[12:].hex()
def abi_uint(data: str,i:int)->int: return int.from_bytes(word(data,i),'big')
def abi_bool(data: str,i:int)->bool: return abi_uint(data,i)!=0
def abi_bytes32(data: str,i:int)->str: return '0x'+word(data,i).hex()

def abi_word_uint(v: int) -> str: return f"{int(v):064x}"
def abi_word_addr(v: str) -> str:
    a=norm_addr(v)
    if not a: raise ValueError(f"invalid address for ABI encoding: {v!r}")
    return ("0"*24)+a[2:]
def decode_abi_addr(result: str) -> str | None:
    s=str(result or "")
    if not s.startswith("0x"): return None
    raw=s[2:]
    if len(raw)<64: return None
    return norm_addr("0x"+raw[-40:])
def decode_abi_bool(result: str) -> bool | None:
    s=str(result or "")
    if not s.startswith("0x"): return None
    raw=s[2:]
    if len(raw)<64: return None
    try: return int(raw[-64:],16)!=0
    except ValueError: return None

class HistoricalEthCallCache:
    """Resumable high-level EVM state reads used only to reconstruct S3's initial state.

    The cache stores eth_call results, never storage-slot reads/writes.  Queries are made at the
    predecessor block so the native replay starts from the same logical ERC721 ownership/approval
    state without importing the concrete storage keys later used for fidelity measurement.
    """
    def __init__(self, rpc_url: str | None, block_number: int, path: Path):
        self.rpc_url=(rpc_url or "").strip()
        self.block_number=int(block_number)
        self.path=path
        self.data={"schema_version":1,"block_number":self.block_number,"calls":{}}
        if path.exists():
            try:
                loaded=json.loads(path.read_text())
                if int(loaded.get("block_number",-1))==self.block_number and isinstance(loaded.get("calls"),dict):
                    self.data=loaded
            except Exception:
                pass

    def _save(self):
        self.path.parent.mkdir(parents=True,exist_ok=True)
        tmp=self.path.with_suffix(self.path.suffix+".tmp")
        tmp.write_text(json.dumps(self.data,indent=2,sort_keys=True)+"\n")
        tmp.replace(self.path)

    def call(self, to: str, data: str) -> str | None:
        to=norm_addr(to)
        if not to: return None
        data="0x"+str(data).removeprefix("0x").lower()
        key=f"{to}|{data}"
        cached=self.data["calls"].get(key)
        if isinstance(cached,dict):
            return cached.get("result") if cached.get("status")=="ok" else None
        if not self.rpc_url:
            return None
        payload=json.dumps({
            "jsonrpc":"2.0","id":1,"method":"eth_call",
            "params":[{"to":to,"data":data},hex(self.block_number)],
        }).encode()
        last=None
        for attempt in range(4):
            try:
                req=urllib.request.Request(
                    self.rpc_url,data=payload,
                    headers={"Content-Type":"application/json","User-Agent":"symbgraphpool-vegeta-s3/1"},
                    method="POST",
                )
                with urllib.request.urlopen(req,timeout=45) as resp:
                    body=json.loads(resp.read())
                if body.get("error"):
                    self.data["calls"][key]={"status":"error","error":body["error"]}
                    self._save()
                    return None
                result=body.get("result")
                if isinstance(result,str):
                    self.data["calls"][key]={"status":"ok","result":result}
                    self._save()
                    return result
                last=f"invalid eth_call response: {body!r}"
            except (urllib.error.URLError, TimeoutError, OSError, json.JSONDecodeError) as e:
                last=str(e)
                if attempt<3: time.sleep(0.5*(2**attempt))
        raise RuntimeError(f"historical eth_call failed after retries for {to} {data}: {last}")

    def has_cached_calls(self) -> bool:
        return bool(self.data.get("calls"))

def resolve_cw721_initial_state(
    resolver: HistoricalEthCallCache,
    token_sources: dict[str,dict[int,int]],
    operator_pairs: set[tuple[str,str,str]],
) -> dict[str,Any]:
    """Resolve ERC721 owner/getApproved/isApprovedForAll at the predecessor block."""
    owners: dict[str,dict[int,str]]=defaultdict(dict)
    approvals: dict[tuple[str,int],str]={}
    operators: set[tuple[str,str,str]]=set()
    stats=defaultdict(int)
    for iid, native_to_raw in sorted(token_sources.items()):
        contract=norm_addr(iid.split(":",1)[1] if ":" in iid else None)
        if not contract: continue
        for native_tid, raw_tid in sorted(native_to_raw.items()):
            owner_result=resolver.call(contract,"0x6352211e"+abi_word_uint(raw_tid))
            owner=decode_abi_addr(owner_result or "")
            if owner and owner!="0x"+"0"*40:
                owners[iid][native_tid]=owner; stats["owners_resolved"]+=1
                approved_result=resolver.call(contract,"0x081812fc"+abi_word_uint(raw_tid))
                approved=decode_abi_addr(approved_result or "")
                if approved and approved!="0x"+"0"*40:
                    approvals[(iid,native_tid)]=approved; stats["token_approvals_resolved"]+=1
            else:
                stats["owner_queries_unresolved"]+=1
    for iid,owner,operator in sorted(operator_pairs):
        contract=norm_addr(iid.split(":",1)[1] if ":" in iid else None)
        owner=norm_addr(owner); operator=norm_addr(operator)
        if not contract or not owner or not operator: continue
        result=resolver.call(contract,"0xe985e9c5"+abi_word_addr(owner)+abi_word_addr(operator))
        allowed=decode_abi_bool(result or "")
        if allowed is True:
            operators.add((iid,owner,operator)); stats["operator_approvals_true"]+=1
        elif allowed is False:
            stats["operator_approvals_false"]+=1
        else:
            stats["operator_queries_unresolved"]+=1
    return {"owners":owners,"approvals":approvals,"operators":operators,"statistics":dict(stats)}

def caller_for(tx: dict, action: dict, by_id: dict[int,dict], mode: str='exact') -> str:
    """Resolve the callee-visible EVM msg.sender for a translated frame.

    Publication execution consumes `ethereum_msg_sender`, which is reconstructed while walking the
    geth callTracer tree.  Raw callTracer `from` is not sufficient for DELEGATECALL: the trace frame
    is initiated by the proxy/current execution context, but EIP-7 preserves msg.sender from the
    parent execution scope.
    """
    msg_sender=norm_addr(action.get('ethereum_msg_sender'))
    if msg_sender: return msg_sender
    call_type=str(action.get('call_type') or '').upper()
    if mode=='exact' and call_type in {'CALL','STATICCALL','DELEGATECALL','CALLCODE'}:
        raise ValueError(
            f"missing exact ethereum_msg_sender for action {action.get('action_id')} "
            f"type={call_type} code={action.get('ethereum_code_address')}; "
            "regenerate native-plan.jsonl from the current callTracer cache"
        )
    # Diagnostic compatibility path for stale plans.
    frame_from=norm_addr(action.get('ethereum_caller'))
    if frame_from: return frame_from
    parent=action.get('parent_action_id')
    if parent is None: return norm_addr(tx.get('from')) or 'native-s3-user'
    parent_action=by_id.get(int(parent))
    if parent_action:
        return norm_addr(parent_action.get('storage_context_address')) or norm_addr(parent_action.get('ethereum_code_address')) or norm_addr(tx.get('from')) or 'native-s3-user'
    return norm_addr(tx.get('from')) or 'native-s3-user'

def source_revert_scope_action_id(action: dict, by_id: dict[int,dict]) -> int | None:
    """Return the top-most failed call-frame ancestor for a successful top-level transaction.

    Geth callTracer can report a failed internal CALL/DELEGATECALL whose parent catches the error and
    lets the Ethereum transaction commit. Every state change under that failed frame is reverted as
    one EVM call scope. Calls translated from the same failed subtree therefore carry the same scope
    ID so the native executor can run them on one nested overlay and discard the overlay atomically.
    """
    path=[]; cur=action; seen=set()
    while cur is not None:
        aid=cur.get('action_id')
        if aid is not None:
            aid=int(aid)
            if aid in seen: raise ValueError(f"cycle in native action parent chain at action {aid}")
            seen.add(aid)
        path.append(cur)
        parent=cur.get('parent_action_id')
        if parent is None: break
        cur=by_id.get(int(parent))
        if cur is None: break
    for node in reversed(path):
        if bool(node.get('failed_frame')):
            aid=node.get('action_id')
            return int(aid) if aid is not None else None
    return None

def attach_revert_scope(call: dict, action: dict, by_id: dict[int,dict]) -> dict:
    scope=source_revert_scope_action_id(action,by_id)
    if scope is not None: call['source_revert_scope_action_id']=scope
    return call

def canon_ep(ep: str | None) -> str:
    if not ep: return ''
    return ep.replace('marketplace::','').replace('helper::','').lower()

def rule_index(selector_doc: dict, code_cache: dict[str,dict]):
    idx=defaultdict(list)
    for r in selector_doc.get('rules',[]): idx[(str(r.get('runtime_family','')).lower(),str(r.get('selector','0x')).lower())].append(r)
    fam_by_addr={norm_addr(a): runtime_family(row.get('code')) for a,row in code_cache.items() if isinstance(row,dict)}
    return idx,fam_by_addr

def match_rule(action: dict, idx, fam_by_addr):
    a=norm_addr(action.get('ethereum_code_address')); fam=fam_by_addr.get(a); sel=str(action.get('selector') or '0x').lower()
    if not fam: return None
    for r in idx.get((fam,sel),[]):
        scope={norm_addr(x) for x in (r.get('address_scope') or [])}
        if not scope or a in scope: return r
    return None

def instance_id(family: str, action: dict) -> str:
    existing=action.get('native_instance_id')
    if existing and str(existing).startswith(family+':'): return str(existing)
    owner=norm_addr(action.get('storage_context_address')) or norm_addr(action.get('ethereum_code_address'))
    return f"{family}:{owner or 'global'}"

def contract_call(kind: str, family: str, iid: str, sender: str | None, msg: dict, origin: dict, funds=None):
    out={'kind':kind,'family':family,'instance_id':iid,'msg':msg,'origin_action_id':origin.get('action_id'),'origin_selector':origin.get('selector')}
    if sender is not None: out['sender']=sender
    if funds: out['funds']=funds
    return out

def translate(family: str, ep: str, sig: str | None, tx: dict, a: dict, caller: str, token_ids: TokenIdRemapper):
    args=a.get('arguments') or {}; data=str(a.get('ethereum_input') or '0x'); e=canon_ep(ep); ec=e.replace('_',''); iid=instance_id(family,a)
    # Standard fungible interfaces.
    if family in {'cw20-base','controlled-cw20','fee-token-cw20','wrapped-native-token'}:
        if 'transferfrom' in ec:
            owner=args.get('owner') or abi_addr(data,0); recipient=args.get('recipient') or abi_addr(data,1); n=amount(args.get('amount',abi_uint(data,2)))
            return contract_call('execute',family,iid,caller,{'transfer_from':{'owner':owner,'recipient':recipient,'amount':str(n)}},a)
        if ec.endswith('::transfer'):
            recipient=args.get('recipient') or abi_addr(data,0); n=amount(args.get('amount',abi_uint(data,1)))
            return contract_call('execute',family,iid,caller,{'transfer':{'recipient':recipient,'amount':str(n)}},a)
        if 'approve' in ec:
            spender=args.get('spender') or abi_addr(data,0); n=approval_amount(args.get('amount',abi_uint(data,1)))
            return contract_call('execute',family,iid,caller,{'approve':{'spender':spender,'amount':str(n)}},a)
        if ec.endswith('::deposit'):
            n=amount(intv(a.get('ethereum_value'))); n=max(n,1)
            return contract_call('execute',family,iid,caller,{'deposit':{}},a,[{'denom':'unative','amount':str(n)}])
        if ec.endswith('::withdraw'):
            n=amount(args.get('amount',abi_uint(data,0)))
            return contract_call('execute',family,iid,caller,{'withdraw':{'amount':str(max(n,1))}},a)
        if 'balance' in ec and 'allowance' not in ec:
            addr=args.get('address') or abi_addr(data,0)
            return contract_call('query',family,iid,None,{'balance':{'address':addr}},a)
        if 'allowance' in ec:
            owner=args.get('owner') or abi_addr(data,0); spender=args.get('spender') or abi_addr(data,1)
            return contract_call('query',family,iid,None,{'allowance':{'owner':owner,'spender':spender}},a)
        if 'totalsupply' in ec:
            return contract_call('query',family,iid,None,{'total_supply':{}},a)
        if 'decimals' in ec:
            # Only cw20-base exposes this query in v1; other families are a semantic no-op until extended.
            return contract_call('query',family,iid,None,{'decimals':{}},a) if family=='cw20-base' else None
    if family=='astroport-pair':
        if 'swap' in ec:
            a0=intv(args.get('amount0_out')); a1=intv(args.get('amount1_out')); offer=1 if a0>0 else 0
            return contract_call('execute',family,iid,caller,{'swap':{'offer_index':offer,'amount_in':'100','min_out':'0','recipient':args.get('recipient') or caller}},a)
        if 'provideliquidity' in ec or 'mintliquidity' in ec:
            provider=args.get('recipient') or args.get('provider') or caller
            return contract_call('execute',family,iid,caller,{'mint_liquidity':{'provider':provider,'amount0':'1000','amount1':'1000'}},a)
        if ec.endswith('::sync'):
            return contract_call('execute',family,iid,caller,{'sync':{'reserve0':str(PAIR_RESERVE),'reserve1':str(PAIR_RESERVE)}},a)
        if 'reserves' in ec: return contract_call('query',family,iid,None,{'get_reserves':{}},a)
        if 'lpbalance' in ec: return contract_call('query',family,iid,None,{'lp_balance':{'address':args.get('address') or abi_addr(data,0)}},a)
        if 'asset0' in ec or 'token0' in ec: return contract_call('query',family,iid,None,{'token0':{}},a)
        if 'asset1' in ec or 'token1' in ec: return contract_call('query',family,iid,None,{'token1':{}},a)
        return None
    if family=='cw721-mintable':
        if 'transfernft' in ec or 'sendorsafetransfernft' in ec:
            source_owner,recipient,raw_tid=erc721_transfer_parts(data,args); tid=token_ids.map(iid,raw_tid)
            call=contract_call('execute',family,iid,caller,{'transfer_nft':{'recipient':recipient,'token_id':tid}},a)
            call['source_owner']=source_owner; call['source_token_id']=raw_tid
            return call
        if 'approveall' in ec:
            op=args.get('operator') or abi_addr(data,0); approved=args.get('approved',abi_bool(data,1))
            return contract_call('execute',family,iid,caller,{'approve_all':{'operator':op,'approved':bool(approved)}},a)
        if 'approvenft' in ec:
            spender=abi_addr(data,0); raw_tid=abi_uint(data,1); tid=token_ids.map(iid,raw_tid)
            call=contract_call('execute',family,iid,caller,{'approve_nft':{'spender':spender,'token_id':tid}},a); call['source_token_id']=raw_tid
            return call
        if 'ownerof' in ec:
            raw_tid=abi_uint(data,0); call=contract_call('query',family,iid,None,{'owner_of':{'token_id':token_ids.map(iid,raw_tid)}},a); call['source_token_id']=raw_tid
            return call
        if 'tokensbyownercount' in ec:
            return contract_call('query',family,iid,None,{'tokens_by_owner_count':{'owner':abi_addr(data,0)}},a)
    if family=='xen-like':
        if 'claimrank' in ec:
            term=intv(args.get('term',abi_uint(data,0))); return contract_call('execute',family,iid,caller,{'claim_rank':{'term_days':max(0,min(term,1))}},a)
        if 'claimmintrewardandshare' in ec:
            other=args.get('other') or abi_addr(data,0); pct=intv(args.get('pct',abi_uint(data,1))); return contract_call('execute',family,iid,caller,{'claim_mint_reward_and_share':{'other':other,'pct':min(pct,100)}},a)
        if 'claimmintreward' in ec: return contract_call('execute',family,iid,caller,{'claim_mint_reward':{}},a)
        if e.endswith('::transfer'):
            recipient=args.get('recipient') or abi_addr(data,0); return contract_call('execute',family,iid,caller,{'transfer':{'recipient':recipient,'amount':'1'}},a)
        if 'transferfrom' in ec:
            owner=args.get('owner') or abi_addr(data,0); recipient=args.get('recipient') or abi_addr(data,1); return contract_call('execute',family,iid,owner,{'transfer':{'recipient':recipient,'amount':'1'}},a)
        if 'balance' in ec: return contract_call('query',family,iid,None,{'balance':{'address':args.get('address') or abi_addr(data,0)}},a)
    if family=='cw1155-like':
        if 'approveall' in ec:
            return contract_call('execute',family,iid,caller,{'approve_all':{'operator':abi_addr(data,0),'approved':abi_bool(data,1)}},a)
        if 'sendfrom' in ec:
            return contract_call('execute',family,iid,caller,{'send_from':{'from':abi_addr(data,0),'to':abi_addr(data,1),'token_id':token_ids.map(iid,abi_uint(data,2)),'amount':str(max(amount(abi_uint(data,3)),1))}},a)
        if 'balance' in e: return contract_call('query',family,iid,None,{'balance':{'address':abi_addr(data,0),'token_id':token_ids.map(iid,abi_uint(data,1))}},a)
    if family=='marketplace-router':
        if 'incrementcounter' in ec: return contract_call('execute',family,iid,caller,{'increment_counter':{}},a)
        if 'getcounter' in ec: return contract_call('query',family,iid,None,{'get_counter':{'address':abi_addr(data,0)}},a)
        if 'getorderstatus' in ec: return contract_call('query',family,iid,None,{'get_order_status':{'order_id':abi_bytes32(data,0)}},a)
    if family=='operator-filter-helper':
        if 'registerandsubscribe' in ec: return contract_call('execute',family,iid,caller,{'register_and_subscribe':{'registrant':abi_addr(data,0),'subscription':abi_addr(data,1)}},a)
        if 'isoperatorallowed' in ec: return contract_call('query',family,iid,None,{'is_operator_allowed':{'registrant':abi_addr(data,0),'operator':abi_addr(data,1)}},a)
    return None

def instantiate_msg(fam: str, participants: set[str]):
    bals=[{'address':a,'amount':str(SEED)} for a in sorted(participants) if norm_addr(a)]
    if fam=='cw20-base': return {'name':'S3','symbol':'S3','decimals':18,'initial_balances':bals}
    if fam=='controlled-cw20': return {'admin':'native-s3-admin','initial_balances':bals}
    if fam=='fee-token-cw20': return {'admin':'native-s3-admin','fee_collector':'native-s3-fee','fee_bps':10,'initial_balances':bals}
    if fam=='wrapped-native-token': return {'denom':'unative'}
    if fam=='cw721-mintable': return {'admin':'native-s3-admin','name':'S3NFT','symbol':'S3N'}
    if fam=='astroport-pair': return {'asset0':'asset0','asset1':'asset1'}
    if fam=='xen-like': return {'genesis_ts':0}
    return {}

def build(argv=None):
    ap=argparse.ArgumentParser(); ap.add_argument('--plan',type=Path,default=DEFAULT_PLAN); ap.add_argument('--selector-map',type=Path,default=DEFAULT_SELECTOR); ap.add_argument('--code-cache',type=Path,default=DEFAULT_CODE_CACHE); ap.add_argument('--implementation-manifest',type=Path,default=DEFAULT_IMPL); ap.add_argument('--output-dir',type=Path,default=DEFAULT_OUT); ap.add_argument('--initial-state-mode',choices=('rpc','heuristic'),default=os.environ.get('VEGETA_S3_NATIVE_INITIAL_STATE_MODE','rpc')); ap.add_argument('--caller-mode',choices=('exact','heuristic'),default=os.environ.get('VEGETA_S3_NATIVE_CALLER_MODE','exact')); ap.add_argument('--rpc-url',default=os.environ.get('ETH_RPC_URL')); ap.add_argument('--initial-state-cache',type=Path,default=None); ns=ap.parse_args(argv)
    selector=read_json(ns.selector_map); cache={str(k).lower():v for k,v in read_json(ns.code_cache).items() if isinstance(v,dict)}; idx,fam_by_addr=rule_index(selector,cache); impl=read_json(ns.implementation_manifest)
    wasm={r['native_code_family']:r['wasm_artifact'] for r in impl['families']}
    token_ids=TokenIdRemapper()
    blocks=[]; instances={}; participant=defaultdict(set); allowances=set(); nft_tokens=defaultdict(dict); nft_owner_priority={}; nft_ops=set(); nft_operator_pairs=set(); nft_approve_senders=set(); nft_source_tokens=defaultdict(dict); multi_seed=set(); multi_approvals=set(); xen_first={}; bank_senders=set(); stats=defaultdict(int)
    for line in ns.plan.read_text().splitlines():
        if not line.strip(): continue
        b=json.loads(line); ob={'block_number':b['block_number'],'timestamp':b.get('timestamp',0),'transactions':[]}
        for tx in b.get('transactions',[]):
            by_id={int(a['action_id']):a for a in tx.get('native_actions',[]) if a.get('action_id') is not None}; calls=[]; skipped=0
            for a in tx.get('native_actions',[]):
                if a.get('ethereum_msg_sender'): stats['explicit_msg_senders']+=1
                else: stats['missing_msg_senders']+=1
                caller=caller_for(tx,a,by_id,ns.caller_mode); fam=a.get('native_code_family'); ep=a.get('native_entrypoint'); sig=None
                if a.get('translation_status')=='mapped-system-action':
                    ep=str(ep or '')
                    aa=a.get('arguments') or {}
                    if ep=='system::bank_send':
                        frm=aa.get('sender') or caller; to=aa.get('recipient') or norm_addr(a.get('ethereum_code_address')); n=max(amount(aa.get('amount_wei')),1); calls.append(attach_revert_scope({'kind':'bank_send','from':frm,'to':to,'coins':[{'denom':'unative','amount':str(n)}],'origin_action_id':a.get('action_id')},a,by_id)); bank_senders.add(frm); stats['system']+=1
                    else: calls.append(attach_revert_scope({'kind':'noop','origin_action_id':a.get('action_id')},a,by_id)); stats['system']+=1
                    continue
                if not fam or not ep or str(ep).startswith('opaque::'):
                    r=match_rule(a,idx,fam_by_addr)
                    if r: fam=r['native_code_family']; ep=r['native_entrypoint']; sig=r.get('ethereum_function_signature')
                    else: skipped+=1; continue
                if fam=='system':
                    if ep=='system::custodial_value_deposit':
                        n=max(amount(a.get('ethereum_value')),1); to=norm_addr(a.get('ethereum_code_address')); calls.append(attach_revert_scope({'kind':'bank_send','from':caller,'to':to,'coins':[{'denom':'unative','amount':str(n)}],'origin_action_id':a.get('action_id')},a,by_id)); bank_senders.add(caller)
                    else: calls.append(attach_revert_scope({'kind':'noop','origin_action_id':a.get('action_id')},a,by_id))
                    stats['system']+=1; continue
                c=translate(str(fam),str(ep),sig,tx,a,caller,token_ids)
                if c is None: skipped+=1; continue
                attach_revert_scope(c,a,by_id); calls.append(c); iid=c['instance_id']; instances[iid]=str(fam); stats['contract_calls']+=1
                if c['kind']=='execute':
                    m=c['msg']; participant[iid].add(c.get('sender',caller))
                    if 'transfer' in m: participant[iid].add(m['transfer']['recipient'])
                    if 'transfer_from' in m:
                        z=m['transfer_from']; participant[iid]|={z['owner'],z['recipient']}; allowances.add((iid,z['owner'],c.get('sender',caller)))
                    if 'approve' in m: participant[iid].add(c.get('sender',caller))
                    if fam=='wrapped-native-token' and ('deposit' in m or 'withdraw' in m): participant[iid].add(c.get('sender',caller)); bank_senders.add(c.get('sender',caller))
                    if fam=='cw721-mintable':
                        if 'transfer_nft' in m:
                            z=m['transfer_nft']; tid=int(z['token_id']); raw_tid=intv(c.get('source_token_id')); nft_source_tokens[iid][tid]=raw_tid; source_owner=norm_addr(c.get('source_owner')) or c.get('sender',caller)
                            note_nft_initial_owner(nft_tokens,nft_owner_priority,iid,tid,source_owner,3)
                            sender=c.get('sender',caller)
                            if sender!=source_owner: nft_ops.add((iid,source_owner,sender)); nft_operator_pairs.add((iid,source_owner,sender))
                        if 'approve_nft' in m:
                            tid=int(m['approve_nft']['token_id']); raw_tid=intv(c.get('source_token_id')); nft_source_tokens[iid][tid]=raw_tid; sender=c.get('sender',caller)
                            note_nft_initial_owner(nft_tokens,nft_owner_priority,iid,tid,sender,2)
                            nft_approve_senders.add((iid,tid,sender))
                        if 'approve_all' in m:
                            nft_operator_pairs.add((iid,c.get('sender',caller),m['approve_all']['operator']))
                    if fam=='cw1155-like' and 'send_from' in m:
                        z=m['send_from']; multi_seed.add((iid,int(z['token_id']),z['from']));
                        if c.get('sender')!=z['from']: multi_approvals.add((iid,z['from'],c.get('sender')))
                    if fam=='xen-like': xen_first.setdefault((iid,c.get('sender',caller)), next(iter(m)))
                elif c['kind']=='query' and fam=='cw721-mintable' and 'owner_of' in c['msg']:
                    tid=int(c['msg']['owner_of']['token_id']); nft_source_tokens[iid][tid]=intv(c.get('source_token_id')); note_nft_initial_owner(nft_tokens,nft_owner_priority,iid,tid,'native-s3-seed-owner',1)
            scopes={int(c['source_revert_scope_action_id']) for c in calls if c.get('source_revert_scope_action_id') is not None}
            stats['internal_revert_scopes']+=len(scopes)
            stats['calls_in_internal_revert_scopes']+=sum(1 for c in calls if c.get('source_revert_scope_action_id') is not None)
            ob['transactions'].append({'tx_index':tx['tx_index'],'tx_hash':tx['tx_hash'],'source_failed':bool(tx.get('source_failed')),'calls':calls,'skipped_actions':skipped})
            stats['transactions']+=1; stats['skipped_actions']+=skipped
        blocks.append(ob)
    initial_state_meta={'mode':ns.initial_state_mode}
    rpc_nft_approvals={}
    if ns.initial_state_mode=='rpc':
        if not blocks: raise RuntimeError('native S3 plan has no blocks')
        initial_block=min(int(b['block_number']) for b in blocks)-1
        cache_path=ns.initial_state_cache or (ns.output_dir/'evm-initial-state-cache.json')
        resolver=HistoricalEthCallCache(ns.rpc_url,initial_block,cache_path)
        if not ns.rpc_url and not resolver.has_cached_calls():
            raise RuntimeError('RPC-backed native initial state is required but ETH_RPC_URL is unset and no populated cache exists; rerun with ETH_RPC_URL=<archive-capable Ethereum RPC> or explicitly opt into --initial-state-mode heuristic')
        resolved=resolve_cw721_initial_state(resolver,nft_source_tokens,nft_operator_pairs)
        for iid,rows in resolved['owners'].items():
            for tid,owner in rows.items(): note_nft_initial_owner(nft_tokens,nft_owner_priority,iid,int(tid),owner,100)
        rpc_nft_approvals=resolved['approvals']
        nft_ops=set(resolved['operators'])
        initial_state_meta={'mode':'rpc-high-level-abi','block_number':initial_block,'cache':str(cache_path),'cw721':resolved['statistics'],'concrete_storage_keys_used':False}
    else:
        initial_state_meta={'mode':'heuristic','warning':'ERC721 initial owner/approval state inferred from in-window calls; publication runs should use rpc mode','concrete_storage_keys_used':False}

    # all instance participants need valid addresses; include tx actors only where relevant
    manifest_instances=[]
    for iid,fam in sorted(instances.items()): manifest_instances.append({'instance_id':iid,'family':fam,'instantiate_msg':instantiate_msg(fam,participant[iid])})
    prime=[]
    for iid,owner,spender in sorted(allowances):
        fam=instances[iid]
        if fam in {'cw20-base','controlled-cw20','fee-token-cw20','wrapped-native-token'}:
            prime.append(contract_call('execute',fam,iid,owner,{'approve':{'spender':spender,'amount':str(SEED)}},{'action_id':None}))
    for iid,fam in sorted(instances.items()):
        if fam=='astroport-pair': prime.append(contract_call('execute',fam,iid,'native-s3-admin',{'sync':{'reserve0':str(PAIR_RESERVE),'reserve1':str(PAIR_RESERVE)}},{'action_id':None}))
    # WETH: seed token balances through real deposit semantics.
    for iid,fam in sorted(instances.items()):
        if fam=='wrapped-native-token':
            for user in sorted(participant[iid]):
                bank_senders.add(user); prime.append(contract_call('execute',fam,iid,user,{'deposit':{}},{'action_id':None},[{'denom':'unative','amount':str(SEED)}]))
    for iid,tid,sender in sorted(nft_approve_senders):
        owner=nft_tokens.get(iid,{}).get(tid)
        if owner and owner!=sender: nft_ops.add((iid,owner,sender))
    for iid,tokens in sorted(nft_tokens.items()):
        for tid,owner in sorted(tokens.items()): prime.append(contract_call('execute','cw721-mintable',iid,'native-s3-admin',{'mint':{'owner':owner,'token_id':tid,'token_uri':None}},{'action_id':None}))
    for (iid,tid),spender in sorted(rpc_nft_approvals.items()):
        owner=nft_tokens.get(iid,{}).get(int(tid))
        if owner: prime.append(contract_call('execute','cw721-mintable',iid,owner,{'approve_nft':{'spender':spender,'token_id':int(tid)}},{'action_id':None}))
    for iid,owner,operator in sorted(nft_ops):
        prime.append(contract_call('execute','cw721-mintable',iid,owner,{'approve_all':{'operator':operator,'approved':True}},{'action_id':None}))
    for iid,tid,owner in sorted(multi_seed): prime.append(contract_call('execute','cw1155-like',iid,'native-s3-admin',{'mint':{'to':owner,'token_id':tid,'amount':str(SEED)}},{'action_id':None}))
    for iid,owner,op in sorted(multi_approvals): prime.append(contract_call('execute','cw1155-like',iid,owner,{'approve_all':{'operator':op,'approved':True}},{'action_id':None}))
    for (iid,user),first in sorted(xen_first.items()):
        if first in {'claim_mint_reward','claim_mint_reward_and_share'}: prime.append(contract_call('execute','xen-like',iid,user,{'claim_rank':{'term_days':0}},{'action_id':None}))
        elif first=='transfer':
            prime.append(contract_call('execute','xen-like',iid,user,{'claim_rank':{'term_days':0}},{'action_id':None})); prime.append(contract_call('execute','xen-like',iid,user,{'claim_mint_reward':{}},{'action_id':None}))
    out=ns.output_dir; out.mkdir(parents=True,exist_ok=True)
    with (out/'execution-plan.jsonl').open('w') as f:
        for b in blocks: f.write(json.dumps(b,separators=(',',':'))+'\n')
    man={'schema_version':1,'dataset':'vegeta-s3-native','source_plan':str(ns.plan),'selector_map':str(ns.selector_map),'wasm_artifacts':wasm,'instances':manifest_instances,'bank_seeds':[{'address':a,'denom':'unative','amount':str(SEED*4)} for a in sorted(bank_senders) if a],'priming_calls':prime,'normalization':{'amount_policy':'positive EVM transfer amounts mapped to 1+(amount mod 1,000,000); zero remains zero','approval_policy':'zero approval remains zero; every positive ERC20-style approval maps to SEED so normalization cannot invert allowance>=spend for canonically successful transferFrom calls','nft_authorization_policy':'RPC mode reconstructs predecessor-block ERC721 ownerOf/getApproved/isApprovedForAll via high-level eth_call and primes only that logical state; heuristic mode remains an explicit non-publication fallback','caller_provenance':{'mode':ns.caller_mode,'source':'derived-geth-callTracer-effective-msg.sender' if ns.caller_mode=='exact' else 'legacy-frame-from-or-parent-context-fallback','delegatecall_rule':'inherit parent execution-scope msg.sender (EIP-7)' if ns.caller_mode=='exact' else None,'explicit_actions':stats.get('explicit_msg_senders',0),'missing_actions':stats.get('missing_msg_senders',0)},'initial_state':initial_state_meta,'token_ids':token_ids.summary(),'seed_balance':str(SEED),'pair_reserve':str(PAIR_RESERVE),'purpose':'preserve storage/control-path key topology while avoiding uint256/u128, allowance-ordering, historical-state availability, and caught-internal-revert artifacts'},'statistics':dict(stats)}
    (out/'execution-manifest.json').write_text(json.dumps(man,indent=2,sort_keys=True)+'\n')
    print(f"wrote {out/'execution-plan.jsonl'}")
    print(f"instances={len(manifest_instances)} priming_calls={len(prime)} tx={stats['transactions']} contract_calls={stats['contract_calls']} skipped_actions={stats['skipped_actions']}")
    return 0
if __name__=='__main__': raise SystemExit(build())
