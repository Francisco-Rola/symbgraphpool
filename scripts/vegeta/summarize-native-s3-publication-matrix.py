#!/usr/bin/env python3
from __future__ import annotations
import argparse, csv, hashlib, json, random, statistics
from collections import defaultdict
from pathlib import Path

NATIVE_ORDER = ["serial","aria-fb","vegeta","static","probability-only","cost-aware","exact-direct","exact-access"]
EXTERNAL_ORDER = ["cosmos-block-stm-access-replay","cosmos-wasmd-block-stm"]


def read_jsonl(path):
    rows=[]
    if not path or not Path(path).exists(): return rows
    for line in Path(path).read_text(encoding='utf-8').splitlines():
        if line.strip(): rows.append(json.loads(line))
    return rows


def percentile(values, q):
    xs=sorted(float(x) for x in values)
    if not xs: return 0.0
    if len(xs)==1: return xs[0]
    pos=(len(xs)-1)*q
    lo=int(pos); hi=min(lo+1,len(xs)-1); frac=pos-lo
    return xs[lo]*(1-frac)+xs[hi]*frac


def bootstrap_median_ci(values, seed_key, reps=4000):
    xs=[float(x) for x in values]
    if len(xs)<2: return None, None
    seed=int.from_bytes(hashlib.sha256(seed_key.encode()).digest()[:8],'little')
    rng=random.Random(seed); n=len(xs); boots=[]
    for _ in range(reps):
        boots.append(statistics.median(xs[rng.randrange(n)] for _ in range(n)))
    return percentile(boots,.025), percentile(boots,.975)


def native_per_sample(rows):
    groups=defaultdict(list)
    for r in rows: groups[(int(r['workers']),str(r['strategy']),int(r['sample']))].append(r)
    out=[]
    for (workers,strategy,sample),g in sorted(groups.items()):
        serial=sum(int(r['matched_serial_nanos']) for r in g)
        total=sum(int(r['strategy_total_nanos']) for r in g)
        post=sum(int(r['post_consensus_nanos']) for r in g)
        tx=sum(int(r['transactions']) for r in g)
        replay=sum(int(r['replayed_transactions']) for r in g)
        reuse=sum(int(r['reused_receipts']) for r in g)
        prepared=sum(int(r['prepared_receipts']) for r in g)
        post_blocks=[int(r['post_consensus_nanos'])/1e6 for r in g]
        out.append({
            'family':'native-cosmwasm','workers':workers,'strategy':strategy,'sample':sample,
            'blocks':len(g),'transactions':tx,'matched_serial_wall_ms':serial/1e6,
            'active_wall_ms':total/1e6,'post_wall_ms':post/1e6,
            'post_p95_ms':percentile(post_blocks,.95),'post_p99_ms':percentile(post_blocks,.99),
            'active_speedup':serial/total if total else 0.0,'post_speedup':serial/post if post else 0.0,
            'replay_rate':replay/tx if tx else 0.0,'reuse_rate':reuse/prepared if prepared else 0.0,
            'serial_equivalent':all(bool(r['serial_equivalent']) for r in g),'scope':'native-cosmwasm'
        })
    controls={(r['workers'],r['sample']):r['active_speedup'] for r in out if r['strategy']=='serial'}
    for r in out:
        c=controls.get((r['workers'],r['sample']),1.0)
        r['net_active_speedup']=r['active_speedup']/c if c else None
    return out


def external_per_sample(rows, family):
    groups=defaultdict(list)
    for r in rows:
        groups[(int(r['workers']),str(r.get('strategy') or family),int(r['sample']))].append(r)
    out=[]
    for (workers,strategy,sample),g in sorted(groups.items()):
        serial=sum(int(r['matched_serial_nanos']) for r in g)
        total=sum(int(r['strategy_total_nanos']) for r in g)
        tx=sum(int(r['transactions']) for r in g)
        attempts=sum(int(r.get('execution_attempts',0)) for r in g)
        rex=sum(int(r.get('reexecutions',0)) for r in g)
        block_ms=[int(r['strategy_total_nanos'])/1e6 for r in g]
        speed=serial/total if total else 0.0
        out.append({
            'family':family,'workers':workers,'strategy':strategy,'sample':sample,
            'blocks':len(g),'transactions':tx,'matched_serial_wall_ms':serial/1e6,
            'active_wall_ms':total/1e6,'post_wall_ms':total/1e6,
            'post_p95_ms':percentile(block_ms,.95),'post_p99_ms':percentile(block_ms,.99),
            'active_speedup':speed,'net_active_speedup':speed,'post_speedup':speed,
            'replay_rate':rex/tx if tx else 0.0,'reuse_rate':0.0,
            'serial_equivalent':all(bool(r['serial_equivalent']) for r in g),
            'scope':g[0].get('baseline_scope',family),'execution_attempts':attempts,
        })
    return out


def aggregate(rows):
    groups=defaultdict(list)
    for r in rows: groups[(r['family'],r['workers'],r['strategy'],r['scope'])].append(r)
    out=[]
    metric_keys=['matched_serial_wall_ms','active_wall_ms','post_wall_ms','active_speedup','net_active_speedup','post_speedup','post_p95_ms','post_p99_ms','replay_rate','reuse_rate']
    def rank(strategy):
        if strategy in NATIVE_ORDER: return NATIVE_ORDER.index(strategy)
        if strategy in EXTERNAL_ORDER: return 100+EXTERNAL_ORDER.index(strategy)
        return 999
    for (family,workers,strategy,scope),g in sorted(groups.items(),key=lambda x:(x[0][1],rank(x[0][2]),x[0][0])):
        row={
            'family':family,'workers':workers,'strategy':strategy,'samples':len(g),
            'blocks_per_sample':min(r['blocks'] for r in g),'transactions_per_sample':min(r['transactions'] for r in g),
            'serial_equivalent':all(r['serial_equivalent'] for r in g),'scope':scope,
        }
        for k in metric_keys:
            vals=[float(r[k]) for r in g]
            row[k]=statistics.median(vals)
            lo,hi=bootstrap_median_ci(vals,f'{family}|{workers}|{strategy}|{k}')
            row[k+'_ci95_low']=lo; row[k+'_ci95_high']=hi
        out.append(row)
    return out


def ci_text(r,key,digits=2):
    v=r[key]; lo=r.get(key+'_ci95_low'); hi=r.get(key+'_ci95_high')
    if lo is None or hi is None: return f'{v:.{digits}f}[n/a]'
    return f'{v:.{digits}f}[{lo:.{digits}f},{hi:.{digits}f}]'


def render(rows):
    lines=[
        'Vegeta S3 publication matrix — steps-calibrated native workload','',
        'Native rows execute the actual CosmWasm semantic port with VM lifecycle=reuse.',
        'cosmos-block-stm-access-replay uses the actual Cosmos SDK TxRunner Block-STM engine on exported native accesses + matched deterministic compute.',
        'cosmos-wasmd-block-stm executes the actual native S3 Wasm artifacts through Wasmd/WasmVM + bank/account/wasm keepers under Cosmos SDK TxRunner Block-STM; it bypasses ante/signature/ABCI tx decoding.','',
        'Times are full 101-block sample totals; p95/p99 are per-block post-order/consensus-visible latencies.',
        '95% CIs are deterministic bootstrap intervals over independent full-range samples; n=1 development rows report n/a.','',
        'workers strategy                       n active-ms[95%CI]              post-ms[95%CI]                net-x[95%CI]            post-x[95%CI]           p95-ms p99-ms replay% reuse% serial-eq scope'
    ]
    for r in rows:
        lines.append(
            f"{r['workers']:>7} {r['strategy']:<30} {r['samples']:>2} "
            f"{ci_text(r,'active_wall_ms',1):<29} {ci_text(r,'post_wall_ms',1):<29} "
            f"{ci_text(r,'net_active_speedup',3):<23} {ci_text(r,'post_speedup',3):<23} "
            f"{r['post_p95_ms']:7.2f} {r['post_p99_ms']:7.2f} {100*r['replay_rate']:7.2f} {100*r['reuse_rate']:6.2f} "
            f"{'yes' if r['serial_equivalent'] else 'NO ':>9} {r['scope']}"
        )
    lines += [
        '', 'Publication interpretation:',
        '  static/probability-only/cost-aware are deployable SymbGraph schedulers; exact-access/exact-direct are evaluation oracles/diagnostics.',
        '  net-x divides native rows by the same worker/sample serial control to reduce paired-run order bias; external Block-STM rows use their own matched serial implementation.',
        '  cosmos-wasmd-block-stm is the stronger app/VM baseline: real Wasmd/WasmVM execution under SDK Block-STM, but without ante/signature/ABCI decoding.',
        '  keep cosmos-block-stm-access-replay as an algorithm/access-substrate diagnostic rather than a native CosmWasm result.',
    ]
    return '\n'.join(lines)+'\n'


def main():
    ap=argparse.ArgumentParser()
    ap.add_argument('--native-records',required=True)
    ap.add_argument('--cosmos-records')
    ap.add_argument('--wasmd-records')
    ap.add_argument('--output-dir',required=True)
    args=ap.parse_args()
    per=(native_per_sample(read_jsonl(args.native_records))+
         external_per_sample(read_jsonl(args.cosmos_records),'cosmos-block-stm-access-replay')+
         external_per_sample(read_jsonl(args.wasmd_records),'cosmos-wasmd-block-stm'))
    rows=aggregate(per); out=Path(args.output_dir); out.mkdir(parents=True,exist_ok=True)
    (out/'summary.json').write_text(json.dumps({'schema_version':2,'rows':rows,'per_sample':per},indent=2)+'\n',encoding='utf-8')
    (out/'summary.txt').write_text(render(rows),encoding='utf-8')
    for name,data in [('summary.csv',rows),('per-sample.csv',per)]:
        if data:
            with open(out/name,'w',newline='',encoding='utf-8') as f:
                cols=[]
                for r in data:
                    for k in r:
                        if k not in cols: cols.append(k)
                w=csv.DictWriter(f,fieldnames=cols); w.writeheader(); w.writerows(data)
    print(render(rows),end='')
if __name__=='__main__': main()
