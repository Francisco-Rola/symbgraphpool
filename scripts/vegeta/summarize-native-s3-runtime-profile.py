#!/usr/bin/env python3
import argparse, csv, json, math, re, sys
from collections import defaultdict
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
from perf_stat import parse_perf_stat


def load_jsonl(path):
    with open(path, encoding='utf-8') as f:
        for line in f:
            if line.strip():
                yield json.loads(line)


def profile_key(row):
    return (row['compute_calibration_metric'], float(row['compute_scale']), int(row['workers']), row['strategy'])


def parse_perf_file(path):
    events, _unsupported = parse_perf_stat(path)
    return events


def load_perf(perf_dir):
    if not perf_dir or not Path(perf_dir).is_dir():
        return {}
    out={}
    rx=re.compile(r'^perf-(?P<label>.+)-workers-(?P<workers>\d+)-(?P<strategy>[^.]+)\.csv$')
    for path in Path(perf_dir).glob('perf-*.csv'):
        m=rx.match(path.name)
        if not m:
            continue
        meta_path=path.with_suffix('.meta.json')
        if not meta_path.exists():
            continue
        meta=json.load(open(meta_path, encoding='utf-8'))
        events=parse_perf_file(path)
        elapsed_ms=events.get('duration_time') or meta.get('elapsed_nanos', 0)/1e6
        task_clock_ms=events.get('task-clock')
        cycles=events.get('cycles', 0.0)
        instructions=events.get('instructions', 0.0)
        cache_refs=events.get('cache-references', 0.0)
        cache_misses=events.get('cache-misses', 0.0)
        out[(meta['metric'], float(meta['scale']), int(meta['workers']), meta['strategy'])]={
            'perf_elapsed_ms': elapsed_ms,
            'perf_task_clock_ms': task_clock_ms,
            'perf_avg_cpus': task_clock_ms/elapsed_ms if task_clock_ms is not None and elapsed_ms else None,
            'perf_context_switches': events.get('context-switches'),
            'perf_cpu_migrations': events.get('cpu-migrations'),
            'perf_page_faults': events.get('page-faults'),
            'perf_cycles': cycles or None,
            'perf_instructions': instructions or None,
            'perf_ipc': instructions/cycles if cycles else None,
            'perf_cache_references': cache_refs or None,
            'perf_cache_misses': cache_misses or None,
            'perf_cache_miss_rate': cache_misses/cache_refs if cache_refs else None,
            'perf_events_mode': meta.get('events_mode'),
        }
    return out


def safe_ratio(a,b):
    return a/b if b else None


def main():
    ap=argparse.ArgumentParser()
    ap.add_argument('--records', required=True)
    ap.add_argument('--perf-dir')
    ap.add_argument('--output-dir', required=True)
    args=ap.parse_args()
    out_dir=Path(args.output_dir); out_dir.mkdir(parents=True, exist_ok=True)

    groups=defaultdict(list)
    for row in load_jsonl(args.records):
        groups[profile_key(row)].append(row)

    serial_speed={}
    for key, rows in groups.items():
        metric,scale,workers,strategy=key
        serial=sum(r['matched_serial_nanos'] for r in rows)
        total=sum(r['strategy_total_nanos'] for r in rows)
        speed=serial/total if total else 0.0
        if strategy=='serial':
            serial_speed[(metric,scale,workers)]=speed

    perf=load_perf(args.perf_dir)
    summary=[]
    for key in sorted(groups, key=lambda k:(k[0],k[1],k[2],k[3])):
        metric,scale,workers,strategy=key
        rows=groups[key]
        serial=sum(r['matched_serial_nanos'] for r in rows)
        total=sum(r['strategy_total_nanos'] for r in rows)
        active=serial/total if total else 0.0
        control=serial_speed.get((metric,scale,workers),1.0)
        profiles=[r.get('runtime_profile') for r in rows if r.get('runtime_profile')]
        aggregate=defaultdict(int)
        max_flight=0
        kinds=set()
        for p in profiles:
            kinds.add(p.get('profile_kind',''))
            max_flight=max(max_flight,int(p.get('max_in_flight',0)))
            for k,v in p.items():
                if k in {'profile_kind','max_in_flight'} or v is None:
                    continue
                if isinstance(v,(int,float)):
                    aggregate[k]+=v
        wall=aggregate['worker_phase_wall_nanos']
        service=aggregate['aggregate_transaction_service_nanos']
        request=aggregate['aggregate_request_execution_nanos']
        capacity=wall*workers
        row={
            'metric':metric,'scale':scale,'workers':workers,'strategy':strategy,
            'blocks':len(rows),'transactions':sum(r['transactions'] for r in rows),
            'active_speedup':active,'net_speedup':active/control if control else None,
            'profile_kind':','.join(sorted(kinds)) if kinds else None,
            'worker_phase_wall_ms':wall/1e6,
            'aggregate_transaction_service_ms':service/1e6,
            'effective_service_concurrency':safe_ratio(service,wall),
            'ready_wait_capacity_fraction':safe_ratio(aggregate['aggregate_ready_wait_nanos'],capacity),
            'request_capacity_fraction':safe_ratio(request,capacity),
            'wasm_acquire_request_fraction':safe_ratio(aggregate['aggregate_wasm_instance_acquire_nanos'],request),
            'wasm_entrypoint_request_fraction':safe_ratio(aggregate['aggregate_wasm_entrypoint_nanos'],request),
            'host_storage_request_fraction':safe_ratio(aggregate['aggregate_host_storage_nanos'],request),
            'host_query_request_fraction':safe_ratio(aggregate['aggregate_host_query_nanos'],request),
            'tx_lock_wait_request_fraction':safe_ratio(aggregate['aggregate_transaction_lock_wait_nanos'],request),
            'canonical_read_wait_request_fraction':safe_ratio(aggregate['aggregate_canonical_state_read_lock_wait_nanos'],request),
            'canonical_read_hold_request_fraction':safe_ratio(aggregate['aggregate_canonical_state_read_hold_nanos'],request),
            'mvcc_lock_wait_request_fraction':safe_ratio(aggregate['aggregate_mvcc_lock_wait_nanos'],request),
            'mvcc_publish_request_fraction':safe_ratio(aggregate['aggregate_mvcc_publish_nanos'],request),
            'commit_lock_wait_ms':aggregate['aggregate_commit_lock_wait_nanos']/1e6,
            'commit_lock_hold_ms':aggregate['aggregate_commit_lock_hold_nanos']/1e6,
            'commit_lock_wait_strategy_fraction':safe_ratio(aggregate['aggregate_commit_lock_wait_nanos'],total),
            'commit_lock_hold_strategy_fraction':safe_ratio(aggregate['aggregate_commit_lock_hold_nanos'],total),
            'max_in_flight':max_flight,
            'wasm_instance_acquires':aggregate['wasm_instance_acquires'],
            'wasm_instance_reuse_hits':aggregate['wasm_instance_reuse_hits'],
            'wasm_instance_pool_misses':aggregate['wasm_instance_pool_misses'],
            'wasm_reuse_hit_rate':safe_ratio(aggregate['wasm_instance_reuse_hits'],aggregate['wasm_instance_acquires']),
            'canonical_state_reads':aggregate['canonical_state_reads'],
        }
        row.update(perf.get(key,{}))
        summary.append(row)

    json.dump(summary,open(out_dir/'runtime-profile-summary.json','w',encoding='utf-8'),indent=2)
    cols=[]
    for row in summary:
        for k in row:
            if k not in cols: cols.append(k)
    with open(out_dir/'runtime-profile-summary.csv','w',newline='',encoding='utf-8') as f:
        w=csv.DictWriter(f,fieldnames=cols); w.writeheader(); w.writerows(summary)

    lines=['Native S3 runtime concurrency profile','']
    for r in summary:
        if r['strategy']=='serial':
            lines.append(f"profile metric={r['metric']:<8} scale={r['scale']:<4g} workers={r['workers']:>2} strategy=serial       active={r['active_speedup']:.3f}x")
            continue
        ec=r['effective_service_concurrency']
        rw=r['ready_wait_capacity_fraction']
        ca=r['canonical_read_wait_request_fraction']
        ch=r['commit_lock_hold_strategy_fraction']
        ac=r.get('perf_avg_cpus')
        lines.append(
            f"profile metric={r['metric']:<8} scale={r['scale']:<4g} workers={r['workers']:>2} strategy={r['strategy']:<12} "
            f"active={r['active_speedup']:.3f}x net={r['net_speedup']:.3f}x "
            f"service-conc={ec:.2f} ready-cap={rw:.1%} max-flight={r['max_in_flight']} "
            f"canon-read-wait={(ca or 0):.1%} commit-hold={(ch or 0):.1%}"
            + (f" perf-cpus={ac:.2f}" if ac is not None else '')
        )
        lines.append(
            f"  wasm acquire/request={(r['wasm_acquire_request_fraction'] or 0):.1%} "
            f"entrypoint/request={(r['wasm_entrypoint_request_fraction'] or 0):.1%} "
            f"host-storage/request={(r['host_storage_request_fraction'] or 0):.1%} "
            f"tx-lock-wait/request={(r['tx_lock_wait_request_fraction'] or 0):.1%} "
            f"mvcc-lock/request={(r['mvcc_lock_wait_request_fraction'] or 0):.1%} "
            f"reuse-hit={(r['wasm_reuse_hit_rate'] or 0):.1%}"
        )
    lines += ['', 'Interpretation:',
      '  service-conc = aggregate transaction service time / worker phase wall; compare with configured workers.',
      '  ready-cap = aggregate READY-queue wait / total worker capacity; high values mean DAG/scheduler starvation.',
      '  canonical read/commit percentages expose shared-world synchronization pressure.',
      '  perf-cpus = task-clock / elapsed time for the isolated perf run; ~4.0 means about four CPUs were busy on average.',
      '  Nested contract percentages are diagnostic shares, not additive components.']
    text='\n'.join(lines)+'\n'
    (out_dir/'summary.txt').write_text(text,encoding='utf-8')
    print(text,end='')

if __name__=='__main__': main()
