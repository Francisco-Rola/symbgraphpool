#!/usr/bin/env python3
"""Derive reviewer-facing distributions and speculation economics from Wasmd JSONL records."""
from __future__ import annotations
import argparse, csv, json, math, statistics
from collections import defaultdict
from pathlib import Path

LABELS = {
    "cosmos-wasmd-direct-serial": "Serial",
    "cosmos-wasmd-block-stm": "BlockSTM",
    "cosmos-wasmd-aria-fb": "AriaFB",
    "cosmos-wasmd-vegeta": "Vegeta",
    "cosmos-wasmd-symbgraph-rust": "Rust-ACG",
    "cosmos-wasmd-symbgraph-rust-exact-trace-oracle": "ACG-Oracle",
}
BASELINES = {"cosmos-wasmd-block-stm", "cosmos-wasmd-aria-fb", "cosmos-wasmd-vegeta"}
ACG = "cosmos-wasmd-symbgraph-rust"
T95 = {1:12.706,2:4.303,3:3.182,4:2.776,5:2.571,6:2.447,7:2.365,8:2.306,9:2.262,10:2.228}


def mean_ci(values: list[float]) -> tuple[float,float]:
    if not values: return 0.0,0.0
    m=statistics.fmean(values)
    if len(values)<2: return m,0.0
    return m, T95.get(len(values)-1,1.96)*statistics.stdev(values)/math.sqrt(len(values))


def write_csv(path: Path, rows: list[dict]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    cols=[]
    for r in rows:
        for k in r:
            if k not in cols: cols.append(k)
    with path.open('w',newline='',encoding='utf-8') as f:
        w=csv.DictWriter(f,fieldnames=cols or ['empty']); w.writeheader()
        if rows: w.writerows(rows)


def load(path: Path) -> list[dict]:
    rows=[]
    with path.open(encoding='utf-8') as f:
        for line in f:
            if line.strip(): rows.append(json.loads(line))
    return rows


def parse_dataset(spec: str) -> tuple[str,Path]:
    if '=' not in spec: raise SystemExit(f'--dataset expects NAME=records.jsonl, got {spec!r}')
    name,path=spec.split('=',1); p=Path(path)
    if not p.is_file(): raise SystemExit(f'missing records for {name}: {p}')
    return name,p


def main() -> None:
    ap=argparse.ArgumentParser()
    ap.add_argument('--dataset',action='append',required=True,help='NAME=/path/to/records.jsonl; repeatable')
    ap.add_argument('--output-dir',type=Path,required=True)
    ap.add_argument('--consensus-window-ms',type=float,default=300.0)
    ap.add_argument('--rolling-blocks',type=int,default=50)
    a=ap.parse_args(); C=a.consensus_window_ms
    block_rows=[]; economics_samples=[]; cold_rows=[]; win_rows=[]

    for dataset,path in map(parse_dataset,a.dataset):
        raw=load(path)
        workers=sorted({int(r['workers']) for r in raw})
        maxw=max(workers)
        grouped=defaultdict(list)
        for r in raw: grouped[(r['strategy'],int(r['workers']),int(r['sample']))].append(r)
        econ_by_key={}
        for (strategy,w,sample),rs in grouped.items():
            rs=sorted(rs,key=lambda r:int(r['block_number']))
            tx=sum(int(r.get('transactions',0)) for r in rs)
            pre=sum(int(r.get('pre_consensus_nanos',0) or 0) for r in rs)
            post=sum(int(r.get('post_consensus_nanos',0) or r.get('strategy_total_nanos',0) or 0) for r in rs)
            spec=sum(int(r.get('speculated_transactions',0) or 0) for r in rs)
            reused=sum(int(r.get('reused_transactions',0) or 0) for r in rs)
            reexec=sum(int(r.get('reexecutions',0) or 0) for r in rs)
            attempts=sum(int(r.get('execution_attempts',0) or 0) for r in rs)
            utils=[float(r.get('symb_worker_utilization',0) or 0) for r in rs if r.get('symb_worker_utilization') is not None]
            idle=sum(int(r.get('symb_worker_idle_nanos',0) or 0) for r in rs)
            row={
                'dataset':dataset,'strategy':strategy,'label':LABELS.get(strategy,strategy),'workers':w,'sample':sample,
                'blocks':len(rs),'transactions':tx,'pre_ms':pre/1e6,'post_ms':post/1e6,'local_elapsed_ms':(pre+post)/1e6,
                'speculated_transactions':spec,'reused_transactions':reused,'reexecutions':reexec,'execution_attempts':attempts,
                'reuse_tx_pct':100*reused/tx if tx else 0.0,'reuse_of_speculated_pct':100*reused/spec if spec else 0.0,
                'reexec_pct':100*reexec/tx if tx else 0.0,'attempt_amplification':attempts/tx if tx else 0.0,
                'worker_utilization_pct':100*statistics.fmean(utils) if utils else 0.0,'worker_idle_ms':idle/1e6,
                'pre_coverage_pct':100*sum(1 for r in rs if int(r.get('pre_consensus_nanos',0) or 0) <= C*1e6)/len(rs) if rs else 0.0,
            }
            econ_by_key[(strategy,w,sample)]=row; economics_samples.append(row)
            for ordinal,r in enumerate(rs):
                pre_ms=int(r.get('pre_consensus_nanos',0) or 0)/1e6
                post_ms=int(r.get('post_consensus_nanos',0) or r.get('strategy_total_nanos',0) or 0)/1e6
                tail=post_ms+max(0.0,pre_ms-C); commit=max(C,pre_ms)+post_ms
                block_rows.append({
                    'dataset':dataset,'strategy':strategy,'label':LABELS.get(strategy,strategy),'workers':w,'sample':sample,
                    'ordinal':ordinal,'block_number':int(r['block_number']),'transactions':int(r.get('transactions',0)),
                    'pre_ms':pre_ms,'post_ms':post_ms,'tail_ms':tail,'commit_ms':commit,
                    'execution_attempts':int(r.get('execution_attempts',0) or 0),'reexecutions':int(r.get('reexecutions',0) or 0),
                    'speculated_transactions':int(r.get('speculated_transactions',0) or 0),'reused_transactions':int(r.get('reused_transactions',0) or 0),
                })
        # Normalize local elapsed against matched Serial on the same worker/sample.
        for (strategy,w,sample),row in econ_by_key.items():
            serial=econ_by_key.get(('cosmos-wasmd-direct-serial',w,sample))
            serial_ms=float(serial['post_ms']) if serial else 0.0
            row['local_elapsed_vs_serial']=float(row['local_elapsed_ms'])/serial_ms if serial_ms else 0.0
        # Win/loss and cold-start use the largest worker count and block-aligned records.
        by_block={(r['strategy'],int(r['sample']),int(r['block_number'])):r for r in block_rows if r['dataset']==dataset and int(r['workers'])==maxw}
        samples=sorted({int(r['sample']) for r in block_rows if r['dataset']==dataset and int(r['workers'])==maxw})
        for sample in samples:
            acg=[r for r in block_rows if r['dataset']==dataset and r['strategy']==ACG and int(r['workers'])==maxw and int(r['sample'])==sample]
            acg.sort(key=lambda r:r['ordinal'])
            for r in acg:
                bn=int(r['block_number'])
                baselines=[]
                for strat in BASELINES:
                    b=by_block.get((strat,sample,bn))
                    if b: baselines.append(float(b['tail_ms']))
                serial=by_block.get(('cosmos-wasmd-direct-serial',sample,bn))
                if baselines:
                    best=min(baselines)
                    win_rows.append({'dataset':dataset,'workers':maxw,'sample':sample,'block_number':bn,'acg_tail_ms':r['tail_ms'],'best_baseline_tail_ms':best,'acg_wins':int(float(r['tail_ms'])<best),'acg_vs_best_tail_speedup':best/float(r['tail_ms']) if float(r['tail_ms']) else 0.0})
                if serial:
                    cold_rows.append({'dataset':dataset,'workers':maxw,'sample':sample,'ordinal':r['ordinal'],'block_number':bn,'tail_speedup_vs_serial':float(serial['tail_ms'])/float(r['tail_ms']) if float(r['tail_ms']) else 0.0,'reuse_tx_pct':100*float(r['reused_transactions'])/float(r['transactions']) if float(r['transactions']) else 0.0,'reexec_pct':100*float(r['reexecutions'])/float(r['transactions']) if float(r['transactions']) else 0.0})

    metrics=['pre_ms','post_ms','local_elapsed_ms','reuse_tx_pct','reuse_of_speculated_pct','reexec_pct','attempt_amplification','worker_utilization_pct','worker_idle_ms','pre_coverage_pct','local_elapsed_vs_serial']
    grouped=defaultdict(list)
    for r in economics_samples: grouped[(r['dataset'],r['strategy'],r['workers'])].append(r)
    econ_summary=[]
    for (dataset,strategy,w),rows in sorted(grouped.items()):
        out={'dataset':dataset,'strategy':strategy,'label':LABELS.get(strategy,strategy),'workers':w,'samples':len(rows),'transactions':rows[0]['transactions']}
        for metric in metrics:
            m,ci=mean_ci([float(r[metric]) for r in rows]); out[metric]=m; out[metric+'_ci95']=ci
        econ_summary.append(out)

    # Aggregate win rate by dataset/sample first, then across independent samples.
    by_ds_sample=defaultdict(list)
    for r in win_rows: by_ds_sample[(r['dataset'],r['sample'],r['workers'])].append(r)
    win_samples=[]
    for (dataset,sample,w),rows in sorted(by_ds_sample.items()):
        win_samples.append({'dataset':dataset,'sample':sample,'workers':w,'blocks':len(rows),'win_pct':100*sum(int(r['acg_wins']) for r in rows)/len(rows),'median_acg_vs_best_tail_speedup':statistics.median(float(r['acg_vs_best_tail_speedup']) for r in rows)})
    wins=[]
    by_ds=defaultdict(list)
    for r in win_samples: by_ds[(r['dataset'],r['workers'])].append(r)
    for (dataset,w),rows in sorted(by_ds.items()):
        m,ci=mean_ci([float(r['win_pct']) for r in rows]); s,sc=mean_ci([float(r['median_acg_vs_best_tail_speedup']) for r in rows])
        wins.append({'dataset':dataset,'workers':w,'samples':len(rows),'win_pct':m,'win_pct_ci95':ci,'median_acg_vs_best_tail_speedup':s,'median_acg_vs_best_tail_speedup_ci95':sc})

    # Rolling cold-start averages are computed within each sample, then averaged by ordinal.
    window=max(1,a.rolling_blocks); rolled=[]
    for (dataset,sample,w),rows in defaultdict(list, {k:[] for k in []}).items(): pass
    groups=defaultdict(list)
    for r in cold_rows: groups[(r['dataset'],r['sample'],r['workers'])].append(r)
    for (dataset,sample,w),rows in groups.items():
        rows=sorted(rows,key=lambda x:int(x['ordinal']))
        for i,r in enumerate(rows):
            lo=max(0,i-window+1); chunk=rows[lo:i+1]
            rolled.append({'dataset':dataset,'sample':sample,'workers':w,'ordinal':r['ordinal'],'block_number':r['block_number'],'window_blocks':len(chunk),'tail_speedup_vs_serial':statistics.fmean(float(x['tail_speedup_vs_serial']) for x in chunk),'reuse_tx_pct':statistics.fmean(float(x['reuse_tx_pct']) for x in chunk),'reexec_pct':statistics.fmean(float(x['reexec_pct']) for x in chunk)})
    cold_summary=[]; by_ord=defaultdict(list)
    for r in rolled: by_ord[(r['dataset'],r['workers'],r['ordinal'])].append(r)
    for (dataset,w,ordinal),rows in sorted(by_ord.items()):
        out={'dataset':dataset,'workers':w,'ordinal':ordinal,'samples':len(rows),'window_blocks':rows[0]['window_blocks']}
        for metric in ['tail_speedup_vs_serial','reuse_tx_pct','reexec_pct']:
            m,ci=mean_ci([float(x[metric]) for x in rows]); out[metric]=m; out[metric+'_ci95']=ci
        cold_summary.append(out)

    write_csv(a.output_dir/'block-metrics.csv',block_rows)
    write_csv(a.output_dir/'economics-per-sample.csv',economics_samples)
    write_csv(a.output_dir/'economics-summary.csv',econ_summary)
    write_csv(a.output_dir/'winloss-per-sample.csv',win_samples)
    write_csv(a.output_dir/'winloss-summary.csv',wins)
    write_csv(a.output_dir/'cold-start.csv',cold_summary)
    (a.output_dir/'summary.json').write_text(json.dumps({'schema_version':1,'consensus_window_ms':C,'rolling_blocks':window,'datasets':[x.split('=',1)[0] for x in a.dataset],'winloss':wins},indent=2)+'\n',encoding='utf-8')
    print(a.output_dir)

if __name__=='__main__': main()
