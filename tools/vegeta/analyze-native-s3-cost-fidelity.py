#!/usr/bin/env python3
import argparse, csv, json, math, statistics
from collections import defaultdict
from pathlib import Path


def pearson(xs, ys):
    if len(xs) < 2: return 0.0
    mx, my = statistics.fmean(xs), statistics.fmean(ys)
    num=sum((x-mx)*(y-my) for x,y in zip(xs,ys))
    dx=sum((x-mx)**2 for x in xs); dy=sum((y-my)**2 for y in ys)
    return num/math.sqrt(dx*dy) if dx and dy else 0.0


def ranks(values):
    order=sorted(range(len(values)), key=lambda i: values[i])
    out=[0.0]*len(values); i=0
    while i < len(order):
        j=i+1
        while j < len(order) and values[order[j]] == values[order[i]]: j+=1
        rank=(i+j-1)/2 + 1
        for k in range(i,j): out[order[k]]=rank
        i=j
    return out


def spearman(xs,ys): return pearson(ranks(xs),ranks(ys)) if len(xs)>1 else 0.0

def med(xs): return statistics.median(xs) if xs else 0.0

def pct(xs,p):
    if not xs: return 0.0
    xs=sorted(xs); pos=(len(xs)-1)*p; lo=int(pos); hi=min(lo+1,len(xs)-1); f=pos-lo
    return xs[lo]*(1-f)+xs[hi]*f


def load_native(path):
    rows={}; lifecycles=set(); calibration=set()
    for line in open(path,encoding='utf-8'):
        if not line.strip(): continue
        block=json.loads(line)
        lifecycles.add(block.get('wasm_instance_lifecycle'))
        calibration.add((
            block.get('compute_calibration_metric','none'),
            float(block.get('compute_scale',0.0) or 0.0),
            int(block.get('compute_base_total_nanos',0) or 0),
            float(block.get('compute_iterations_per_nano',0.0) or 0.0),
        ))
        bn=block['block_number']
        for tx in block['transactions']:
            rows[(bn,tx['tx_index'])]=tx
    if len(calibration) != 1:
        raise SystemExit(f'native cost input mixes compute calibration configurations: {sorted(calibration)}')
    metric,scale,base_total_nanos,iterations_per_nano=next(iter(calibration))
    return rows,lifecycles,{
        'metric':metric,'scale':scale,'base_total_nanos':base_total_nanos,
        'iterations_per_nano':iterations_per_nano,
    }


def load_traces(root):
    traces={}
    root=Path(root)
    for block_dir in sorted(root.iterdir()):
        if not block_dir.is_dir() or not block_dir.name.isdigit(): continue
        bn=int(block_dir.name)
        for path in block_dir.glob('*.json'):
            try: idx=int(path.name.split('-',1)[0])
            except ValueError: continue
            obj=json.load(open(path,encoding='utf-8')); r=obj.get('result',{})
            traces[(bn,idx)]={
                'tx_hash':obj.get('tx_hash',''), 'gas_used':int(r.get('gasUsed') or 0),
                'steps':int(r.get('steps') or 0), 'reads':set(r.get('reads') or []),
                'writes':set(r.get('writes') or []),
            }
    return traces


def block_dependencies(items):
    # Canonical conflict DAG: RAW/WAW depend on latest writer; WAR depends on all readers since write.
    preds={idx:set() for idx,_ in items}
    last_writer={}; readers=defaultdict(set)
    for idx,t in items:
        for key in t['reads']:
            if key in last_writer: preds[idx].add(last_writer[key])
            readers[key].add(idx)
        for key in t['writes']:
            if key in last_writer: preds[idx].add(last_writer[key])
            preds[idx].update(r for r in readers[key] if r != idx)
            readers[key].clear(); last_writer[key]=idx
    return preds


def weighted_critical_path(items,preds,weight_key):
    score={}; parent={}
    lookup=dict(items)
    for idx,_ in items:
        best=None; best_score=0.0
        for p in preds[idx]:
            if score.get(p,0.0) > best_score:
                best_score=score[p]; best=p
        score[idx]=best_score + float(lookup[idx][weight_key])
        parent[idx]=best
    if not score: return set(),0.0
    end=max(score,key=score.get); path=set(); cur=end
    while cur is not None:
        path.add(cur); cur=parent[cur]
    return path,score[end]


def main():
    ap=argparse.ArgumentParser(description='Compare native S3 transaction cost with frozen EVM gas/opcode cost and critical-path placement.')
    ap.add_argument('--native-accesses',required=True)
    ap.add_argument('--source-traces-dir',required=True)
    ap.add_argument('--output-dir',required=True)
    ap.add_argument('--max-missing-source',type=int,default=0,help='allow this many native transactions to lack frozen source traces')
    args=ap.parse_args()
    native,native_lifecycles,compute_calibration=load_native(args.native_accesses); traces=load_traces(args.source_traces_dir)
    if native_lifecycles != {'reuse'}:
        raise SystemExit(f'native cost input must use Wasm reuse lifecycle, got {sorted(str(x) for x in native_lifecycles)}')
    common=sorted(set(native)&set(traces))
    if not common: raise SystemExit('no native/source transaction matches')
    missing_native=sorted(set(traces)-set(native)); missing_source=sorted(set(native)-set(traces))
    if len(missing_source) > args.max_missing_source:
        raise SystemExit(f'missing source transactions: {len(missing_source)} exceeds allowance {args.max_missing_source}')
    incomplete_blocks=sorted({bn for bn,_ in missing_source})
    critical_steps=set(); critical_gas=set(); block_diag=[]
    by_block=defaultdict(list)
    for (bn,idx) in common: by_block[bn].append((idx,traces[(bn,idx)]))
    for bn,items in sorted(by_block.items()):
        if bn in incomplete_blocks:
            continue
        items.sort(); preds=block_dependencies(items)
        p_steps,w_steps=weighted_critical_path(items,preds,'steps')
        p_gas,w_gas=weighted_critical_path(items,preds,'gas_used')
        critical_steps.update((bn,i) for i in p_steps); critical_gas.update((bn,i) for i in p_gas)
        block_diag.append({'block_number':bn,'transactions':len(items),'dependency_edges':sum(map(len,preds.values())),
                           'steps_critical_transactions':len(p_steps),'steps_weighted_critical_cost':w_steps,
                           'gas_critical_transactions':len(p_gas),'gas_weighted_critical_cost':w_gas})
    rows=[]
    for key in common:
        bn,idx=key; n=native[key]; t=traces[key]
        nh=n.get('tx_hash','').lower(); sh=t.get('tx_hash','').lower()
        hash_match=(not nh or not sh or nh==sh)
        rows.append({'block_number':bn,'tx_index':idx,'tx_hash':n.get('tx_hash',''),
                     'hash_match':hash_match,'native_execution_nanos':int(n.get('native_execution_nanos') or 0),
                     'source_gas_used':t['gas_used'],'source_opcode_steps':t['steps'],
                     'semantic_calls':int(n.get('semantic_calls') or 0),'skipped_actions':int(n.get('skipped_actions') or 0),
                     'compute_iterations':int(n.get('compute_iterations') or 0),
                     'critical_path_eligible':bn not in incomplete_blocks,
                     'on_steps_critical_path':key in critical_steps,'on_gas_critical_path':key in critical_gas})
    valid=[r for r in rows if r['native_execution_nanos']>0]
    nanos=[r['native_execution_nanos'] for r in valid]; gas=[r['source_gas_used'] for r in valid]; steps=[r['source_opcode_steps'] for r in valid]
    def group(flag):
        eligible=[r for r in valid if r['critical_path_eligible']]
        a=[r for r in eligible if r[flag]]; b=[r for r in eligible if not r[flag]]
        return {
            'critical_count':len(a),'off_critical_count':len(b),
            'critical_native_median_us':med([r['native_execution_nanos']/1e3 for r in a]),
            'off_critical_native_median_us':med([r['native_execution_nanos']/1e3 for r in b]),
            'critical_source_steps_median':med([r['source_opcode_steps'] for r in a]),
            'off_critical_source_steps_median':med([r['source_opcode_steps'] for r in b]),
            'critical_native_cost_share':sum(r['native_execution_nanos'] for r in a)/sum(nanos) if nanos else 0,
            'critical_steps_cost_share':sum(r['source_opcode_steps'] for r in a)/sum(steps) if sum(steps) else 0,
            'critical_gas_cost_share':sum(r['source_gas_used'] for r in a)/sum(gas) if sum(gas) else 0,
        }
    summary={
        'matched_transactions':len(common),'missing_native_transactions':len(missing_native),'missing_source_transactions':len(missing_source),
        'native_wasm_instance_lifecycles':sorted(native_lifecycles),
        'compute_calibration':compute_calibration,
        'compute_iterations_total':sum(r['compute_iterations'] for r in rows),
        'missing_source_transaction_details':[{'block_number':bn,'tx_index':idx,'tx_hash':native[(bn,idx)].get('tx_hash','')} for bn,idx in missing_source],
        'critical_path_excluded_blocks':incomplete_blocks,
        'hash_mismatches':sum(not r['hash_match'] for r in rows),
        'native_execution_us':{'median':med([x/1e3 for x in nanos]),'p95':pct([x/1e3 for x in nanos],.95)},
        'source_gas_used':{'median':med(gas),'p95':pct(gas,.95)},
        'source_opcode_steps':{'median':med(steps),'p95':pct(steps,.95)},
        'correlation':{
            'native_vs_gas_pearson':pearson(nanos,gas),'native_vs_gas_spearman':spearman(nanos,gas),
            'native_vs_steps_pearson':pearson(nanos,steps),'native_vs_steps_spearman':spearman(nanos,steps),
        },
        'steps_weighted_critical_path':group('on_steps_critical_path'),
        'gas_weighted_critical_path':group('on_gas_critical_path'),
    }
    # Distortion signal: >1 means native puts a larger share of compute on the source critical chain.
    g=summary['steps_weighted_critical_path']
    summary['critical_path_native_overweight_ratio']=(g['critical_native_cost_share']/g['critical_steps_cost_share'] if g['critical_steps_cost_share'] else None)
    out=Path(args.output_dir); out.mkdir(parents=True,exist_ok=True)
    with open(out/'tx-costs.csv','w',newline='',encoding='utf-8') as f:
        w=csv.DictWriter(f,fieldnames=list(rows[0])); w.writeheader(); w.writerows(rows)
    with open(out/'block-critical-paths.csv','w',newline='',encoding='utf-8') as f:
        w=csv.DictWriter(f,fieldnames=list(block_diag[0])); w.writeheader(); w.writerows(block_diag)
    (out/'summary.json').write_text(json.dumps(summary,indent=2)+'\n',encoding='utf-8')
    c=summary['correlation']; cp=summary['steps_weighted_critical_path']
    cal=summary['compute_calibration']
    lines=['Native S3 workload-cost fidelity','',
           f"compute calibration: metric={cal['metric']} scale={cal['scale']} base-total-ms={cal['base_total_nanos']/1e6:.1f} iter/ns={cal['iterations_per_nano']:.6f}",
           f"compute iterations total: {summary['compute_iterations_total']}",
           f"matched transactions: {summary['matched_transactions']}",f"missing source traces (allowed): {summary['missing_source_transactions']}",f"critical-path blocks excluded for incomplete traces: {len(summary['critical_path_excluded_blocks'])}",f"hash mismatches: {summary['hash_mismatches']}",
           f"native vs EVM steps: Pearson {c['native_vs_steps_pearson']:.3f}, Spearman {c['native_vs_steps_spearman']:.3f}",
           f"native vs gasUsed:   Pearson {c['native_vs_gas_pearson']:.3f}, Spearman {c['native_vs_gas_spearman']:.3f}",'',
           'Steps-weighted source critical path:',
           f"  transactions: {cp['critical_count']} critical / {cp['off_critical_count']} off-critical",
           f"  native cost share on source critical path: {cp['critical_native_cost_share']:.2%}",
           f"  source opcode-step share on same path:     {cp['critical_steps_cost_share']:.2%}",
           f"  native-overweight ratio: {summary['critical_path_native_overweight_ratio']}",'',
           'Critical-path metrics exclude any block containing a missing source trace; correlation still uses every matched transaction.',
           'A low native/EVM correlation means the semantic port does not preserve transaction compute weight.',
           'A native-overweight ratio above 1 means relatively more native CPU is concentrated on the source critical chain, which suppresses observable parallel speedup even when dependency topology is preserved.']
    (out/'summary.txt').write_text('\n'.join(lines)+'\n',encoding='utf-8')

if __name__=='__main__': main()
