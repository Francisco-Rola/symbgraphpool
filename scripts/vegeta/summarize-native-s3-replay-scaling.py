#!/usr/bin/env python3
import argparse, csv, json, statistics
from collections import defaultdict
from pathlib import Path


def median(xs):
    return statistics.median(xs) if xs else 0.0


def main():
    ap=argparse.ArgumentParser(description='Summarize direct-DAG versus MVCC replay scaling for native Vegeta S3.')
    ap.add_argument('--records', required=True)
    ap.add_argument('--output-dir', required=True)
    args=ap.parse_args()
    rows=[json.loads(line) for line in open(args.records, encoding='utf-8') if line.strip()]
    grouped=defaultdict(list)
    for r in rows:
        grouped[(r['workers'],r['sample'],r['strategy'])].append(r)
    samples=[]
    for (workers,sample,strategy), rs in sorted(grouped.items()):
        serial=sum(r['matched_serial_nanos'] for r in rs)
        total=sum(r['strategy_total_nanos'] for r in rs)
        pre=sum(r['preexecution_nanos'] for r in rs)
        rec=sum(r['reconciliation_nanos'] for r in rs)
        post=sum(r['post_consensus_nanos'] for r in rs)
        samples.append({
            'workers':workers,'sample':sample,'strategy':strategy,
            'blocks':len(rs),'transactions':sum(r['transactions'] for r in rs),
            'matched_serial_ms':serial/1e6,'strategy_total_ms':total/1e6,
            'preexecution_ms':pre/1e6,'reconciliation_ms':rec/1e6,'post_consensus_ms':post/1e6,
            'active_speedup': serial/total if total else 0.0,
            'preexecution_speedup': serial/pre if pre else 0.0,
            'replay_speedup': serial/rec if rec else 0.0,
            'post_speedup': serial/post if post else 0.0,
            'serial_equivalent': all(r.get('serial_equivalent') for r in rs),
        })
    by=defaultdict(list)
    for r in samples: by[(r['workers'],r['strategy'])].append(r)
    summary=[]
    for (workers,strategy), rs in sorted(by.items()):
        summary.append({
            'workers':workers,'strategy':strategy,'samples':len(rs),
            'active_speedup_median':median([r['active_speedup'] for r in rs]),
            'preexecution_speedup_median':median([r['preexecution_speedup'] for r in rs]),
            'replay_speedup_median':median([r['replay_speedup'] for r in rs]),
            'post_speedup_median':median([r['post_speedup'] for r in rs]),
            'strategy_total_ms_median':median([r['strategy_total_ms'] for r in rs]),
            'preexecution_ms_median':median([r['preexecution_ms'] for r in rs]),
            'reconciliation_ms_median':median([r['reconciliation_ms'] for r in rs]),
            'serial_equivalent':all(r['serial_equivalent'] for r in rs),
        })
    out=Path(args.output_dir); out.mkdir(parents=True,exist_ok=True)
    for name,data in [('per-sample.csv',samples),('summary.csv',summary)]:
        with open(out/name,'w',newline='',encoding='utf-8') as f:
            if data:
                w=csv.DictWriter(f,fieldnames=list(data[0])); w.writeheader(); w.writerows(data)
    with open(out/'summary.json','w',encoding='utf-8') as f: json.dump(summary,f,indent=2)
    lines=['Native S3 direct-DAG replay scaling','']
    for r in summary:
        lines.append(f"workers={r['workers']:>2} strategy={r['strategy']:<12} active={r['active_speedup_median']:.3f}x pre={r['preexecution_speedup_median']:.3f}x replay={r['replay_speedup_median']:.3f}x post={r['post_speedup_median']:.3f}x total={r['strategy_total_ms_median']:.1f}ms serial-eq={r['serial_equivalent']}")
    lines += ['', 'Interpretation:', '  exact-direct replay_speedup isolates canonical DAG replay without snapshot/MVCC/receipt machinery.', '  exact-access preexecution_speedup measures the existing dependency/MVCC prepare path against the same matched serial control.', '  A large exact-direct vs exact-access-pre gap identifies execution-substrate overhead rather than workload dependency limits.']
    (out/'summary.txt').write_text('\n'.join(lines)+'\n',encoding='utf-8')

if __name__=='__main__': main()
