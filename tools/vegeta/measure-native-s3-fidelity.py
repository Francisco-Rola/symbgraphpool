#!/usr/bin/env python3
"""Compare concrete native CosmWasm accesses against Vegeta S3 source-trace conflicts.

Besides aggregate precision/recall, this emits causal attribution for false-positive and
false-negative transaction-pair edges. The attribution is diagnostic only: source concrete keys
are loaded after native execution and are never fed back into planning or execution.
"""
from __future__ import annotations
import argparse, json, statistics
from collections import Counter, defaultdict
from pathlib import Path
from typing import Any

ROOT=Path(__file__).resolve().parents[2]
DEFAULT_CORPUS=ROOT/'benchmarks/corpora/vegeta-ethereum/s3/corpus.jsonl'
DEFAULT_NATIVE=ROOT/'benchmarks/corpora/vegeta-ethereum/s3/native-execution/native-accesses.jsonl'
DEFAULT_PLAN=ROOT/'benchmarks/corpora/vegeta-ethereum/s3/native-plan/native-plan.jsonl'
DEFAULT_OUT=ROOT/'benchmarks/corpora/vegeta-ethereum/s3/native-execution'


def iter_jsonl(path:Path):
    with path.open() as fh:
        for line in fh:
            line=line.strip()
            if line:
                yield json.loads(line)

def read_jsonl(path:Path):
    # Stream the file instead of Path.read_text(), which briefly keeps both the raw
    # JSONL text and all decoded objects resident at once. S1 audit files are large.
    return list(iter_jsonl(path))

def pairs_from_rw(rows:list[tuple[set[str],set[str]]]):
    readers=defaultdict(set); writers=defaultdict(set)
    for i,(rs,ws) in enumerate(rows):
        for k in rs: readers[k].add(i)
        for k in ws: writers[k].add(i)
    pairs=set()
    for k,ws in writers.items():
        touched=readers[k]|ws
        for i in ws:
            for j in touched:
                if i!=j: pairs.add((min(i,j),max(i,j)))
    return pairs,readers,writers

def conflict_cause_keys(rows:list[tuple[set[str],set[str]]], i:int, j:int)->set[str]:
    """Concrete keys that make transaction pair (i,j) conflict."""
    ri,wi=rows[i]; rj,wj=rows[j]
    shared=(ri|wi)&(rj|wj)
    return {k for k in shared if k in wi or k in wj}

def source_rows(block):
    return [(set(tx.get('reads') or []),set(tx.get('writes') or [])) for tx in block.get('transactions') or []]

def native_identity(a:dict)->str:
    kind=a['kind']; key=a.get('key_hex',''); contract=a.get('contract','')
    if kind.startswith('storage_'):
        return f"storage:{contract}:{key}:{a.get('range_end_hex') or ''}" if kind=='storage_scan' else f"storage:{contract}:{key}"
    # Bank conflict identity is account/denom encoded in the engine key, independent of caller contract.
    return f"bank:{key}"

def native_rows(block, include_bank:bool=False):
    rows=[]
    for tx in block.get('transactions') or []:
        r=set(); w=set(); reverted=bool(tx.get('source_failed')) or tx.get('execution_status')=='reverted'
        for a in tx.get('accesses') or []:
            kind=str(a.get('kind') or '')
            if kind.startswith('bank_') and not include_bank:
                continue
            ident=native_identity(a)
            is_write=kind in {'storage_write','storage_remove','bank_write'} and not reverted and not a.get('reverted')
            (w if is_write else r).add(ident)
        rows.append((r,w))
    return rows

def native_provenance(block, include_bank:bool=False):
    """Per-transaction, per-concrete-key provenance for post-hoc attribution."""
    rows=[]
    for tx in block.get('transactions') or []:
        by_key=defaultdict(list)
        for a in tx.get('accesses') or []:
            kind=str(a.get('kind') or '')
            if kind.startswith('bank_') and not include_bank:
                continue
            ident=native_identity(a)
            by_key[ident].append({
                'kind':a.get('kind'),
                'contract':a.get('contract'),
                'family':a.get('family'),
                'instance_id':a.get('instance_id'),
                'bundle_call_index':a.get('bundle_call_index'),
                'origin_action_id':a.get('origin_action_id'),
                'semantic_action':a.get('semantic_action'),
                'reverted':bool(a.get('reverted')),
            })
        rows.append(by_key)
    return rows

def critical_path(n:int,pairs:set[tuple[int,int]])->int:
    preds=defaultdict(list)
    for i,j in pairs: preds[j].append(i)
    dp=[1]*n
    for j in range(n):
        if preds[j]: dp[j]=1+max(dp[i] for i in preds[j])
    return max(dp,default=0)

def critical_path_edges(n:int,pairs:set[tuple[int,int]])->set[tuple[int,int]]:
    """Edges that belong to at least one longest path in the canonical-order DAG."""
    if not pairs or n<=0: return set()
    succ=defaultdict(list); preds=defaultdict(list)
    for i,j in pairs:
        succ[i].append(j); preds[j].append(i)
    end=[1]*n
    for j in range(n):
        if preds[j]: end[j]=1+max(end[i] for i in preds[j])
    start=[1]*n
    for i in range(n-1,-1,-1):
        if succ[i]: start[i]=1+max(start[j] for j in succ[i])
    longest=max(end,default=0)
    return {(i,j) for i,j in pairs if end[i]+start[j]==longest}

def hot_key_chain(rows):
    touched=defaultdict(set)
    for i,(r,w) in enumerate(rows):
        for k in r|w: touched[k].add(i)
    return max((len(v) for v in touched.values()),default=0)

def pct(x): return f"{100*x:.2f}%" if x is not None else 'n/a'
def ratio(a,b): return a/b if b else None

def source_owner(key:str)->str|None:
    parts=str(key).split('/')
    if len(parts)>=3 and parts[0]=='evm' and len(parts[1])==40:
        try: int(parts[1],16)
        except ValueError: return None
        return '0x'+parts[1].lower()
    return None

def source_slot(key:str)->str|None:
    parts=str(key).split('/')
    return '0x'+parts[2].lower() if len(parts)>=3 and parts[0]=='evm' else None

def storage_resource_guess(key_hex:str)->str:
    """Best-effort cw-storage-plus namespace decoder; diagnostic, not semantic ground truth."""
    try: raw=bytes.fromhex(key_hex)
    except ValueError: return 'opaque'
    if not raw: return 'empty'
    if len(raw)>=2:
        n=int.from_bytes(raw[:2],'big')
        if 0<n<=len(raw)-2:
            ns=raw[2:2+n]
            if all(32<=b<127 for b in ns):
                return ns.decode('ascii')
    if all(32<=b<127 for b in raw):
        return raw.decode('ascii')
    prefix=[]
    for b in raw:
        if 32<=b<127: prefix.append(b)
        else:
            if len(prefix)>=3: break
            prefix=[]
    if len(prefix)>=3:
        return bytes(prefix[:48]).decode('ascii','replace')
    return 'opaque'

def native_key_description(key:str, records:list[dict])->dict:
    if key.startswith('bank:'):
        return {'key':key,'kind':'bank','contract':None,'resource_guess':'bank'}
    parts=key.split(':',3)
    contract=parts[1] if len(parts)>1 else None
    key_hex=parts[2] if len(parts)>2 else ''
    families=sorted({x.get('family') for x in records if x.get('family')})
    instances=sorted({x.get('instance_id') for x in records if x.get('instance_id')})
    actions=Counter(x.get('semantic_action') or 'unknown' for x in records)
    return {
        'key':key,'kind':'storage','contract':contract,'key_hex':key_hex,
        'resource_guess':storage_resource_guess(key_hex),
        'families':families,'instances':instances,
        'semantic_actions':[{'name':k,'access_records':v} for k,v in actions.most_common(8)],
    }

def owner_profile_index(plan_path:Path|None)->dict[str,set[str]]:
    out=defaultdict(set)
    if not plan_path or not plan_path.exists(): return out
    # Attribution needs only owner/profile metadata. Stream it so a large S1 native
    # plan is never materialized as a second full in-memory JSON object graph.
    for block in iter_jsonl(plan_path):
        for tx in block.get('transactions') or []:
            for a in tx.get('native_actions') or []:
                owner=str(a.get('storage_context_address') or '').lower()
                family=str(a.get('ethereum_profile_family') or '').lower()
                if owner.startswith('0x') and len(owner)==42 and len(family)==64:
                    out[owner].add(family)
    return out

def _new_bucket():
    return {'pair_credit':0.0,'pair_incidence':0,'sole_cause_pairs':0,
            'critical_path_pair_credit':0.0,'critical_path_pair_incidence':0,
            '_blocks':set()}

def _add_bucket(acc:dict,label:str,credit:float,block:int,critical:bool,sole:bool):
    row=acc.setdefault(label,_new_bucket())
    row['pair_credit']+=credit; row['pair_incidence']+=1; row['_blocks'].add(block)
    if sole: row['sole_cause_pairs']+=1
    if critical:
        row['critical_path_pair_credit']+=credit
        row['critical_path_pair_incidence']+=1

def _rank_buckets(acc:dict, limit:int|None=None):
    rows=[]
    for label,row in acc.items():
        x={k:v for k,v in row.items() if k!='_blocks'}
        x['label']=label; x['blocks']=len(row['_blocks']); x['sample_blocks']=sorted(row['_blocks'])[:8]
        rows.append(x)
    rows.sort(key=lambda x:(-x['pair_credit'],-x['critical_path_pair_credit'],-x['pair_incidence'],x['label']))
    return rows if limit is None else rows[:limit]

def _split_credit(labels:set[str], credit:float):
    labels={x for x in labels if x}
    if not labels: labels={'unknown'}
    each=credit/len(labels)
    return [(x,each) for x in sorted(labels)]

def build_attribution(src_blocks:list[dict], nat_blocks:list[dict], plan_path:Path|None=None, top:int=25, include_bank:bool=False):
    sm={b['block_number']:b for b in src_blocks}; nm={b['block_number']:b for b in nat_blocks}
    profile_idx=owner_profile_index(plan_path)
    fp_family={}; fp_instance={}; fp_resource={}; fp_action={}; fp_key={}
    fn_owner={}; fn_profile={}; fn_key={}
    fp_blocks=[]; fn_blocks=[]; cp_blocks=[]
    fp_pairs_total=fn_pairs_total=0
    fp_key_incidences=fn_key_incidences=0
    fp_critical_edges=fn_critical_edges=0
    native_cp_sum=tp_cp_sum=source_cp_sum=0

    # key-specific rich metadata is accumulated separately from bucket counters.
    fp_key_meta=defaultdict(lambda:{'records':[]})
    fn_key_meta={}

    for bn in sorted(sm):
        sb,nb=sm[bn],nm[bn]; srows=source_rows(sb); nrows=native_rows(nb,include_bank=include_bank); prov=native_provenance(nb,include_bank=include_bank)
        sp,_,_=pairs_from_rw(srows); np,_,_=pairs_from_rw(nrows)
        fp=np-sp; fn=sp-np; tp=np&sp
        native_critical=critical_path_edges(len(nrows),np)
        source_critical=critical_path_edges(len(srows),sp)
        ncp=critical_path(len(nrows),np); tcp=critical_path(len(nrows),tp); scp=critical_path(len(srows),sp)
        native_cp_sum+=ncp; tp_cp_sum+=tcp; source_cp_sum+=scp
        cp_blocks.append({
            'block_number':bn,'source_critical_path':scp,'native_critical_path':ncp,
            'true_positive_only_native_critical_path':tcp,
            'fp_inflation':ncp-tcp,
            'fp_edges_on_any_native_longest_path':len(fp&native_critical),
            'fn_edges_on_any_source_longest_path':len(fn&source_critical),
        })
        fp_blocks.append({'block_number':bn,'false_positive_pairs':len(fp),'native_pairs':len(np),'source_pairs':len(sp),'critical_path_fp_inflation':ncp-tcp})
        fn_blocks.append({'block_number':bn,'false_negative_pairs':len(fn),'source_pairs':len(sp),'native_pairs':len(np)})
        fp_pairs_total+=len(fp); fn_pairs_total+=len(fn)
        fp_critical_edges+=len(fp&native_critical); fn_critical_edges+=len(fn&source_critical)

        for i,j in fp:
            causes=conflict_cause_keys(nrows,i,j)
            if not causes: continue
            fp_key_incidences+=len(causes)
            per_key=1.0/len(causes)
            critical=(i,j) in native_critical
            sole=len(causes)==1
            for key in causes:
                records=list(prov[i].get(key,[]))+list(prov[j].get(key,[]))
                fp_key_meta[key]['records'].extend(records)
                _add_bucket(fp_key,key,per_key,bn,critical,sole)
                desc=native_key_description(key,records)
                families=set(desc.get('families') or ([] if desc['kind']=='storage' else ['system-bank']))
                instances=set(desc.get('instances') or ([] if desc['kind']=='storage' else ['system-bank']))
                resource={f"{next(iter(families),'unknown')}::{desc['resource_guess']}"}
                actions={x.get('semantic_action') or 'unknown' for x in records}
                for label,c in _split_credit(families,per_key): _add_bucket(fp_family,label,c,bn,critical,sole)
                for label,c in _split_credit(instances,per_key): _add_bucket(fp_instance,label,c,bn,critical,sole)
                for label,c in _split_credit(resource,per_key): _add_bucket(fp_resource,label,c,bn,critical,sole)
                for label,c in _split_credit(actions,per_key): _add_bucket(fp_action,label,c,bn,critical,sole)

        for i,j in fn:
            causes=conflict_cause_keys(srows,i,j)
            if not causes: continue
            fn_key_incidences+=len(causes)
            per_key=1.0/len(causes)
            critical=(i,j) in source_critical
            sole=len(causes)==1
            for key in causes:
                owner=source_owner(key) or 'unknown'
                slot=source_slot(key)
                fn_key_meta[key]={'owner':owner,'slot':slot}
                _add_bucket(fn_key,key,per_key,bn,critical,sole)
                _add_bucket(fn_owner,owner,per_key,bn,critical,sole)
                profiles=profile_idx.get(owner,set()) if owner!='unknown' else set()
                for label,c in _split_credit(profiles,per_key): _add_bucket(fn_profile,label,c,bn,critical,sole)

    # Convert key rows and add decoded provenance.
    fp_key_rows=[]
    for row in _rank_buckets(fp_key,None):
        key=row.pop('label')
        desc=native_key_description(key,fp_key_meta[key]['records'])
        desc.update(row); fp_key_rows.append(desc)
    fn_key_rows=[]
    for row in _rank_buckets(fn_key,None):
        key=row.pop('label'); desc={'key':key,**fn_key_meta.get(key,{})}; desc.update(row); fn_key_rows.append(desc)

    fp_blocks.sort(key=lambda x:(-x['false_positive_pairs'],-x['critical_path_fp_inflation'],x['block_number']))
    fn_blocks.sort(key=lambda x:(-x['false_negative_pairs'],x['block_number']))
    cp_blocks.sort(key=lambda x:(-x['fp_inflation'],-x['fp_edges_on_any_native_longest_path'],x['block_number']))

    return {
        'schema_version':1,
        'dataset':'vegeta-s3-native',
        'summary':{
            'false_positive_pairs':fp_pairs_total,'false_negative_pairs':fn_pairs_total,
            'false_positive_pair_key_incidences':fp_key_incidences,
            'false_negative_pair_key_incidences':fn_key_incidences,
            'false_positive_edges_on_any_native_longest_path':fp_critical_edges,
            'false_negative_edges_on_any_source_longest_path':fn_critical_edges,
        },
        'false_positive':{
            'by_family':_rank_buckets(fp_family,top),
            'by_instance':_rank_buckets(fp_instance,top),
            'by_resource':_rank_buckets(fp_resource,top),
            'by_semantic_action':_rank_buckets(fp_action,top),
            'top_concrete_keys':fp_key_rows[:top],
            'top_blocks':fp_blocks[:top],
        },
        'false_negative':{
            'by_source_owner':_rank_buckets(fn_owner,top),
            'by_source_profile_family':_rank_buckets(fn_profile,top),
            'top_source_keys':fn_key_rows[:top],
            'top_blocks':fn_blocks[:top],
        },
        'critical_path_attribution':{
            'source_sum':source_cp_sum,
            'native_sum':native_cp_sum,
            'true_positive_only_native_sum':tp_cp_sum,
            'native_minus_true_positive_only_sum':native_cp_sum-tp_cp_sum,
            'definition':'true-positive-only removes all native-only conflict edges while retaining native/source intersection edges; the difference isolates native critical-path dependence on false-positive edges, but is not a claim that every excess node is uniquely attributable to one key',
            'top_blocks_by_false_positive_inflation':cp_blocks[:top],
        },
        'methodology':{
            'comparison_scope':'storage+bank' if include_bank else 'contract-storage-only',
            'false_positive_cause':'a concrete native key shared by the pair with at least one committed native writer',
            'false_negative_cause':'a concrete source EVM storage key shared by the pair with at least one source writer',
            'fractional_pair_credit':'when multiple keys cause one pair, one pair of credit is divided equally across those keys; family/instance/resource/action credit is then divided across labels observed on that key, so aggregate credit remains comparable to pair counts',
            'critical_path_edge':'edge belongs to at least one longest path in the canonical-order conflict DAG',
            'resource_guess':'best-effort decoding of cw-storage-plus namespace prefixes from concrete native key bytes; diagnostic label only',
            'source_profile_family':'joined post hoc from native-plan storage-context metadata; source concrete read/write keys are never consumed by execution or planning',
        },
    }

def full_native_augmentation(src_blocks:list[dict], nat_blocks:list[dict]):
    sm={b['block_number']:b for b in src_blocks}; nm={b['block_number']:b for b in nat_blocks}
    source_all=set(); storage_all=set(); full_all=set()
    source_cp=storage_cp=full_cp=0
    source_hot=storage_hot=full_hot=0
    per=[]
    for bn in sorted(sm):
        srows=source_rows(sm[bn]); storage_rows=native_rows(nm[bn],include_bank=False); full_rows=native_rows(nm[bn],include_bank=True)
        sp,_,_=pairs_from_rw(srows); stp,_,_=pairs_from_rw(storage_rows); fp,_,_=pairs_from_rw(full_rows)
        source_all|={(bn,i,j) for i,j in sp}; storage_all|={(bn,i,j) for i,j in stp}; full_all|={(bn,i,j) for i,j in fp}
        scp=critical_path(len(srows),sp); stcp=critical_path(len(storage_rows),stp); fcp=critical_path(len(full_rows),fp)
        source_cp+=scp; storage_cp+=stcp; full_cp+=fcp
        source_hot+=hot_key_chain(srows); storage_hot+=hot_key_chain(storage_rows); full_hot+=hot_key_chain(full_rows)
        per.append({'block_number':bn,'storage_only_pairs':len(stp),'full_native_pairs':len(fp),'additional_pairs_from_bank_ledger':len(fp-stp),'storage_only_critical_path':stcp,'full_native_critical_path':fcp,'critical_path_delta_from_bank_ledger':fcp-stcp})
    inter=len(source_all&full_all)
    p=ratio(inter,len(full_all)); r=ratio(inter,len(source_all))
    per.sort(key=lambda x:(-x['critical_path_delta_from_bank_ledger'],-x['additional_pairs_from_bank_ledger'],x['block_number']))
    return {'definition':'full native topology includes contract storage plus native bank-ledger accesses; source precision/recall remains storage-only because the source corpus contains EVM storage keys rather than account-balance keys','source_pairs':len(source_all),'storage_only_native_pairs':len(storage_all),'full_native_pairs':len(full_all),'additional_pairs_from_bank_ledger':len(full_all-storage_all),'intersection_with_source':inter,'precision_against_storage_source':p,'recall_against_storage_source':r,'false_positive_against_storage_source':len(full_all-source_all),'false_negative_against_storage_source':len(source_all-full_all),'critical_path_sum':{'source':source_cp,'storage_only_native':storage_cp,'full_native':full_cp,'bank_delta':full_cp-storage_cp},'hot_key_chain_sum':{'source':source_hot,'storage_only_native':storage_hot,'full_native':full_hot,'bank_delta':full_hot-storage_hot},'top_blocks_by_bank_critical_path_delta':per[:25]}

def _fmt_rank(title, rows, field='pair_credit', n=12):
    lines=[title]
    if not rows: return lines+['  (none)']
    for i,row in enumerate(rows[:n],1):
        label=row.get('label') or row.get('key') or '?'
        lines.append(f"  {i:2d}. {label} credit={row.get(field,0):.2f} incidences={row.get('pair_incidence',0)} critical_credit={row.get('critical_path_pair_credit',0):.2f} blocks={row.get('blocks',0)}")
    return lines

def attribution_text(a:dict, dataset:str='vegeta-s3-native')->str:
    s=a['summary']; cp=a['critical_path_attribution']; fp=a['false_positive']; fn=a['false_negative']
    lines=[
        f'Vegeta native topology false-positive / false-negative attribution ({dataset})','',
        f"false-positive pairs: {s['false_positive_pairs']}",
        f"false-negative pairs: {s['false_negative_pairs']}",
        f"FP pair-key incidences: {s['false_positive_pair_key_incidences']}",
        f"FN pair-key incidences: {s['false_negative_pair_key_incidences']}",
        f"FP edges on any native longest path: {s['false_positive_edges_on_any_native_longest_path']}",
        f"FN edges on any source longest path: {s['false_negative_edges_on_any_source_longest_path']}",'',
        f"critical path: source={cp['source_sum']} native={cp['native_sum']} true-positive-only-native={cp['true_positive_only_native_sum']} native-minus-TP-only={cp['native_minus_true_positive_only_sum']}",'',
    ]
    lines += _fmt_rank('Top FP native families (fractional pair credit):',fp['by_family'])
    lines += [''] + _fmt_rank('Top FP native instances:',fp['by_instance'])
    lines += [''] + _fmt_rank('Top FP decoded resources:',fp['by_resource'])
    lines += [''] + _fmt_rank('Top FP semantic actions:',fp['by_semantic_action'])
    lines += ['','Top FP concrete keys:']
    for i,row in enumerate(fp['top_concrete_keys'][:12],1):
        fam=','.join(row.get('families') or []) or ('system-bank' if row.get('kind')=='bank' else 'unknown')
        inst=','.join(row.get('instances') or []) or '-'
        lines.append(f"  {i:2d}. credit={row['pair_credit']:.2f} critical={row['critical_path_pair_credit']:.2f} resource={row.get('resource_guess')} family={fam} instance={inst} key={row['key']}")
    lines += ['','Top blocks by FP critical-path inflation:']
    for i,row in enumerate(cp['top_blocks_by_false_positive_inflation'][:12],1):
        lines.append(f"  {i:2d}. block={row['block_number']} fp_inflation={row['fp_inflation']} native_cp={row['native_critical_path']} tp_only_cp={row['true_positive_only_native_critical_path']} fp_longest_edges={row['fp_edges_on_any_native_longest_path']}")
    lines += [''] + _fmt_rank('Top FN source owners:',fn['by_source_owner'])
    lines += [''] + _fmt_rank('Top FN source profile families:',fn['by_source_profile_family'])
    lines += ['','Top FN source keys:']
    for i,row in enumerate(fn['top_source_keys'][:12],1):
        lines.append(f"  {i:2d}. credit={row['pair_credit']:.2f} critical={row['critical_path_pair_credit']:.2f} owner={row.get('owner')} slot={row.get('slot')} key={row['key']}")
    lines += ['',
        'Interpretation: rank by fractional pair credit first, then inspect concrete keys/resources and',
        'critical-path-bearing FP edges. This is post-execution diagnosis only; it must not be used as',
        'an oracle to inject historical EVM keys into native planning, execution, or symbolic profiles.'
    ]
    return '\n'.join(lines)+'\n'

def main(argv=None):
    ap=argparse.ArgumentParser(); ap.add_argument('--corpus',type=Path,default=DEFAULT_CORPUS); ap.add_argument('--native-accesses',type=Path,default=DEFAULT_NATIVE); ap.add_argument('--native-plan',type=Path,default=DEFAULT_PLAN); ap.add_argument('--output-dir',type=Path,default=DEFAULT_OUT); ap.add_argument('--attribution-top',type=int,default=25); ap.add_argument('--dataset',default='vegeta-s3-native'); ns=ap.parse_args(argv)
    src=read_jsonl(ns.corpus); nat=read_jsonl(ns.native_accesses); sm={b['block_number']:b for b in src}; nm={b['block_number']:b for b in nat}
    if set(sm)!=set(nm): raise SystemExit(f"block set mismatch: source={len(sm)} native={len(nm)}")
    src_pairs_all=set(); nat_pairs_all=set(); per=[]; src_cp=nat_cp=src_hot=nat_hot=0; tx_total=0
    for bn in sorted(sm):
        sb,nb=sm[bn],nm[bn]; srows=source_rows(sb); nrows=native_rows(nb,include_bank=False)
        if len(srows)!=len(nrows): raise SystemExit(f"tx count mismatch block {bn}: source={len(srows)} native={len(nrows)}")
        sp,_,_=pairs_from_rw(srows); np,_,_=pairs_from_rw(nrows)
        src_pairs_all|={(bn,i,j) for i,j in sp}; nat_pairs_all|={(bn,i,j) for i,j in np}
        inter=len(sp&np); precision=ratio(inter,len(np)); recall=ratio(inter,len(sp))
        sc=critical_path(len(srows),sp); nc=critical_path(len(nrows),np); sh=hot_key_chain(srows); nh=hot_key_chain(nrows)
        src_cp+=sc; nat_cp+=nc; src_hot+=sh; nat_hot+=nh; tx_total+=len(srows)
        per.append({'block_number':bn,'transactions':len(srows),'source_conflict_pairs':len(sp),'native_conflict_pairs':len(np),'intersection_conflict_pairs':inter,'precision':precision,'recall':recall,'source_conflict_dag_critical_path':sc,'native_conflict_dag_critical_path':nc,'source_hot_key_chain':sh,'native_hot_key_chain':nh})
    inter=len(src_pairs_all&nat_pairs_all); p=ratio(inter,len(nat_pairs_all)); r=ratio(inter,len(src_pairs_all)); f1=(2*p*r/(p+r)) if p is not None and r is not None and p+r else None
    attribution=build_attribution(src,nat,ns.native_plan,max(1,ns.attribution_top),include_bank=False); augmentation=full_native_augmentation(src,nat)
    report={'schema_version':2,'dataset':ns.dataset,'blocks':len(sm),'transactions':tx_total,'comparison_scope':'contract-storage-only','conflict_pairs':{'source':len(src_pairs_all),'native':len(nat_pairs_all),'intersection':inter,'precision':p,'recall':r,'f1':f1,'false_positive':len(nat_pairs_all-src_pairs_all),'false_negative':len(src_pairs_all-nat_pairs_all)},'critical_chain_fidelity':{'definition':'sum over blocks of longest path in canonical-order conflict DAG using source EVM storage and native contract storage only','source_sum':src_cp,'native_sum':nat_cp,'ratio':ratio(nat_cp,src_cp),'relative_error':abs(nat_cp-src_cp)/src_cp if src_cp else None},'vegeta_hot_key_chain_fidelity':{'definition':'sum over blocks of maximum transactions touching one comparable contract-storage key','source_sum':src_hot,'native_sum':nat_hot,'ratio':ratio(nat_hot,src_hot),'relative_error':abs(nat_hot-src_hot)/src_hot if src_hot else None},'full_native_augmentation':augmentation,'attribution':{'json':'native-topology-attribution.json','text':'native-topology-attribution.txt','comparison_scope':'contract-storage-only','false_positive_edges_on_any_native_longest_path':attribution['summary']['false_positive_edges_on_any_native_longest_path'],'native_true_positive_only_critical_path_sum':attribution['critical_path_attribution']['true_positive_only_native_sum']},'per_block':per,'methodology':{'source_conflicts':'public-RPC touched-storage reads plus changed-storage writes from corpus; conflict requires common EVM storage key with at least one writer','native_conflicts':'primary fidelity uses concrete CosmWasm contract-storage accesses; native bank-ledger accesses are reported separately because no like-for-like EVM account-balance key exists in this source corpus','topology_unit':'transaction pair within the same block','warning':'full native performance may still include additional bank-ledger dependencies'}}
    out=ns.output_dir; out.mkdir(parents=True,exist_ok=True); (out/'native-topology-fidelity.json').write_text(json.dumps(report,indent=2,sort_keys=True)+'\n'); (out/'native-topology-attribution.json').write_text(json.dumps(attribution,indent=2,sort_keys=True)+'\n'); (out/'native-topology-attribution.txt').write_text(attribution_text(attribution,ns.dataset))
    conflict_blocks=[x for x in per if x['source_conflict_pairs']]; medp=statistics.median([x['precision'] for x in conflict_blocks if x['precision'] is not None]) if conflict_blocks else None; medr=statistics.median([x['recall'] for x in conflict_blocks if x['recall'] is not None]) if conflict_blocks else None; aug=augmentation
    lines=[f'Vegeta native CosmWasm topology fidelity ({ns.dataset})','',f"blocks: {len(sm)}",f"transactions: {tx_total}",'','PRIMARY COMPARISON: source EVM storage vs native CosmWasm contract storage',f"source conflict pairs: {len(src_pairs_all)}",f"native storage conflict pairs: {len(nat_pairs_all)}",f"intersection: {inter}",f"precision: {pct(p)}",f"recall: {pct(r)}",f"F1: {pct(f1)}",f"false positives: {len(nat_pairs_all-src_pairs_all)}",f"false negatives: {len(src_pairs_all-nat_pairs_all)}",'',f"ordered storage conflict-DAG critical-path sum: source={src_cp} native={nat_cp} ratio={report['critical_chain_fidelity']['ratio']:.4f} relative_error={pct(report['critical_chain_fidelity']['relative_error'])}",f"storage hot-key chain sum: source={src_hot} native={nat_hot} ratio={report['vegeta_hot_key_chain_fidelity']['ratio']:.4f} relative_error={pct(report['vegeta_hot_key_chain_fidelity']['relative_error'])}",'',f"median per-conflict-block precision: {pct(medp)}",f"median per-conflict-block recall: {pct(medr)}",'',f"FP edges on any native storage longest path: {attribution['summary']['false_positive_edges_on_any_native_longest_path']}",f"native storage true-positive-only critical-path sum: {attribution['critical_path_attribution']['true_positive_only_native_sum']}",'','FULL NATIVE AUGMENTATION: contract storage + native bank ledger',f"full native conflict pairs: {aug['full_native_pairs']}",f"additional pairs introduced by bank ledger: {aug['additional_pairs_from_bank_ledger']}",f"full-native precision against storage-only source: {pct(aug['precision_against_storage_source'])}",f"full-native recall against storage-only source: {pct(aug['recall_against_storage_source'])}",f"critical-path sum: storage-only-native={aug['critical_path_sum']['storage_only_native']} full-native={aug['critical_path_sum']['full_native']} bank_delta={aug['critical_path_sum']['bank_delta']}",'','Important: the source corpus contains EVM storage keys, not account-balance keys.','Precision/recall are therefore storage-to-storage. Native bank dependencies remain real execution','dependencies and are reported separately rather than mislabeled as storage-topology false positives.','See native-topology-attribution.txt/json for storage FP/FN diagnosis.']
    (out/'native-topology-fidelity.txt').write_text('\n'.join(lines)+'\n'); print('\n'.join(lines)); print(); print('wrote native-topology-attribution.txt/json'); return 0
if __name__=='__main__': raise SystemExit(main())
