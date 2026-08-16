#!/usr/bin/env python3
"""Reviewer-facing summary of the ConflictLab 1.0 submission experiment suite."""
from __future__ import annotations
import argparse,json,math,statistics
from collections import defaultdict,Counter
from pathlib import Path

from conflictlab_v1_miss_policy import (
    MISS_CLASS_LABELS,
    MISS_CLASS_ORDER,
    candidate_misses,
    classify_candidate_miss,
    has_recovery_evidence,
)

def load(p): return [json.loads(x) for x in p.read_text().splitlines() if x.strip()]
def param(r,k,d=None): return r.get('metadata',{}).get('parameters',{}).get(k,d)
def env(r,k,d=None): return r.get('metadata',{}).get('environment',{}).get(k,d)
def med(xs): return statistics.median(xs) if xs else math.nan
def pct(xs,q):
    if not xs:return math.nan
    a=sorted(xs); x=q*(len(a)-1); lo=int(math.floor(x)); hi=int(math.ceil(x));
    return a[lo] if lo==hi else a[lo]*(hi-x)+a[hi]*(x-lo)
def ratio_milli(r,path):
    cur=r
    for k in path.split('.'):
        cur=cur.get(k,{}) if isinstance(cur,dict) else None
    return (cur/1000.0) if isinstance(cur,(int,float)) else math.nan
def ms(v): return v/1e6 if isinstance(v,(int,float)) else math.nan
def f(x,n=2): return 'n/a' if not math.isfinite(x) else f'{x:.{n}f}'
def group(records,key):
    d=defaultdict(list)
    for r in records:d[key(r)].append(r)
    return d

def write(records):
    lines=[]; add=lines.append
    counts=Counter(r['metadata']['experiment_id'] for r in records)
    add('# ConflictLab 1.0 submission-suite summary'); add('')
    add(f"records={len(records)} serial_equivalent={sum(r['correctness'].get('serial_equivalent') is True for r in records)}/{len(records)} campaigns={len(counts)}")
    add(f"dirty_records={sum(env(r,'git_dirty')=='true' for r in records)}")
    add('VM lifecycle: benchmark-scoped retained Instance reuse for canonical performance runs with vm_gas_limit=u64::MAX; fresh/recycle remains the semantic control in the lifecycle sentinel.')
    add('Primary names: post-consensus validation latency; phase-bottleneck speedup = serial/max(pre,post); sequential speedup = serial/actual non-overlapped adaptive block wall.')
    add('')
    add('## Campaign counts')
    for k,v in sorted(counts.items()): add(f'- {k}: {v}')

    missed=[r for r in records if candidate_misses(r)>0]
    if missed:
        add('');add('## Candidate-miss attribution')
        add('Schema-3 records aggregate misses per block, so V1.0 can classify the source capability from controlled workload/fault parameters but cannot retroactively name the exact profile pair without new telemetry.')
        add('| class | records with misses | candidate misses | recovery evidence | serial-equivalent |')
        add('|---|---:|---:|---:|---:|')
        by_class=group(missed,classify_candidate_miss)
        for classification in MISS_CLASS_ORDER:
            xs=by_class.get(classification,[])
            if not xs: continue
            add(f"| {MISS_CLASS_LABELS[classification]} | {len(xs)} | {sum(candidate_misses(r) for r in xs)} | {sum(has_recovery_evidence(r) for r in xs)}/{len(xs)} | {sum(r['correctness'].get('serial_equivalent') is True for r in xs)}/{len(xs)} |")
        add('');add('### Candidate misses by operation mix')
        add('| operation mix | class | records with misses | candidate misses | miss-history rels median | fallback rels median |')
        add('|---|---|---:|---:|---:|---:|')
        for (mix,classification),xs in sorted(group(missed,lambda r:(param(r,'operation_mix','n/a'),classify_candidate_miss(r))).items()):
            add(f"| {mix} | {MISS_CLASS_LABELS[classification]} | {len(xs)} | {sum(candidate_misses(r) for r in xs)} | {f(med([r.get('adaptive_state',{}).get('candidate_miss_history_relationships',0) for r in xs]),1)} | {f(med([r.get('adaptive_state',{}).get('runtime_fallback_relationships',0) for r in xs]),1)} |")

    core=[r for r in records if r['metadata']['experiment_id']=='conflictlab-v1-core-state']
    if core:
        add('');add('## Core end-to-end')
        add('| mode | n | validation median | phase-bottleneck median | sequential median | p95 post ms | oracle speedup | oracle realization | bypass |')
        add('|---|---:|---:|---:|---:|---:|---:|---:|---:|')
        for mode,xs in group(core,lambda r:r['metadata']['mode']).items():
            val=[ratio_milli(r,'consensus.validation_latency_speedup_milli') for r in xs]
            bot=[ratio_milli(r,'consensus.throughput_speedup_milli') for r in xs]
            seq=[ratio_milli(r,'pipeline_timing.end_to_end_speedup_milli') for r in xs]
            post=[ms(r['consensus']['post_consensus_nanos']) for r in xs]
            oracle=[]; real=[]
            for r in xs:
                s=r['parallelism'].get('serial_equivalent_work_nanos'); b=r['parallelism'].get('perfect_conflict_parallel_lower_bound_nanos'); w=r['parallelism'].get('actual_execution_wall_nanos')
                if b: oracle.append(s/b); real.append(w/b)
            add(f"| {mode} | {len(xs)} | {f(med(val))}x | {f(med(bot))}x | {f(med(seq))}x | {f(pct(post,.95))} | {f(med(oracle))}x | {f(med(real))}x | {sum(r['planning']['serial_bypassed'] for r in xs)} |")

    cut=[r for r in records if r['metadata']['experiment_id']=='conflictlab-v1-cutoff-divergence']
    if cut:
        add('');add('## Consensus cutoff sensitivity (all divergence modes)')
        add('| cutoff ms | n | cutoff hit | ready % | reuse % | post ms | validation | phase bottleneck |')
        add('|---:|---:|---:|---:|---:|---:|---:|---:|')
        for c,xs in sorted(group(cut,lambda r:int(param(r,'consensus_cutoff_ms'))).items()):
            ready=[100*r['consensus']['receipts_ready_by_cutoff']/max(1,r['consensus']['candidate_transactions']) for r in xs]
            reuse=[100*r['execution']['reused_results']/max(1,r['consensus']['decided_transactions']) for r in xs]
            add(f"| {c} | {len(xs)} | {sum(r['consensus']['cutoff_reached'] for r in xs)}/{len(xs)} | {f(med(ready),1)} | {f(med(reuse),1)} | {f(med([ms(r['consensus']['post_consensus_nanos']) for r in xs]))} | {f(med([ratio_milli(r,'consensus.validation_latency_speedup_milli') for r in xs]))}x | {f(med([ratio_milli(r,'consensus.throughput_speedup_milli') for r in xs]))}x |")
        add('');add('### Divergence decomposition')
        add('| divergence | n | shared % | same-pos % | reuse % | discarded | missing | invalidated | replayed |')
        add('|---|---:|---:|---:|---:|---:|---:|---:|---:|')
        for d,xs in sorted(group(cut,lambda r:param(r,'consensus_divergence')).items()):
            shared=[100*r['consensus']['shared_transactions']/max(1,r['consensus']['candidate_transactions']) for r in xs]; same=[100*r['consensus']['same_position_transactions']/max(1,r['consensus']['candidate_transactions']) for r in xs]
            reuse=[100*r['execution']['reused_results']/max(1,r['consensus']['decided_transactions']) for r in xs]
            add(f"| {d} | {len(xs)} | {f(med(shared),1)} | {f(med(same),1)} | {f(med(reuse),1)} | {f(med([r['execution']['discarded_predictions'] for r in xs]),1)} | {f(med([r['execution']['missing_predictions'] for r in xs]),1)} | {f(med([r['execution']['invalidated_results'] for r in xs]),1)} | {f(med([r['execution']['replayed_transactions'] for r in xs]),1)} |")

    sc=[r for r in records if r['metadata']['experiment_id']=='conflictlab-v1-serial-cutoff']
    if sc:
        add('');add('## Buffered serial prefix')
        for c,xs in sorted(group(sc,lambda r:int(param(r,'consensus_cutoff_ms'))).items()):
            ready=[100*r['consensus']['receipts_ready_by_cutoff']/max(1,r['consensus']['decided_transactions']) for r in xs]
            add(f"- {c}ms: ready median={f(med(ready),1)}%, cutoff-hit={sum(r['consensus']['cutoff_reached'] for r in xs)}/{len(xs)}, post median={f(med([ms(r['consensus']['post_consensus_nanos']) for r in xs]))}ms")

    comp=[r for r in records if r['metadata']['experiment_id']=='conflictlab-v1-compaction-reference']
    if comp:
        add('');add('## Compact vs dense reference')
        add('| block | compact | logical edges | materialized | planning ms | feedback ms |')
        add('|---:|---|---:|---:|---:|---:|')
        for (b,t),xs in sorted(group(comp,lambda r:(int(param(r,'transactions')),param(r,'acg.compact_equivalence_groups'))).items()):
            add(f"| {b} | {t} | {f(med([r['scheduling']['candidate_edges'] for r in xs]),1)} | {f(med([r['scheduling']['materialized_candidate_edges'] for r in xs]),1)} | {f(med([ms(r['planning']['total_nanos']) for r in xs]))} | {f(med([ms(r['feedback_timing']['total_nanos']) for r in xs]))} |")

    gran=[r for r in records if r['metadata']['experiment_id']=='conflictlab-v1-symbolic-granularity']
    if gran:
        add('');add('## Symbolic granularity')
        add('| mode | granularity | logical edges | materialized | predictor precision | predictor recall | phase bottleneck | replay |')
        add('|---|---|---:|---:|---:|---:|---:|---:|')
        for (mode,g),xs in sorted(group(gran,lambda r:(r['metadata']['mode'],param(r,'symbolic_granularity'))).items()):
            precision=[]; recall=[]
            for r in xs:
                pos=r['feedback'].get('positive_observations',0); neg=r['feedback'].get('negative_observations',0); miss=r['feedback'].get('candidate_misses',0)
                if pos+neg: precision.append(pos/(pos+neg))
                if pos+miss: recall.append(pos/(pos+miss))
            add(f"| {mode} | {g} | {f(med([r['scheduling']['candidate_edges'] for r in xs]),1)} | {f(med([r['scheduling']['materialized_candidate_edges'] for r in xs]),1)} | {f(med(precision),3)} | {f(med(recall),3)} | {f(med([ratio_milli(r,'consensus.throughput_speedup_milli') for r in xs]))}x | {f(med([r['execution']['replayed_transactions'] for r in xs]),1)} |")

    faults=[r for r in records if r['metadata']['experiment_id']=='conflictlab-v1-prediction-fault-recovery']
    if faults:
        add('');add('## Predictor fault recovery')
        add('| mode | fault | rate bps | prior fault blocks | candidate misses | predictor precision | predictor recall | fallback rels | mean posterior | phase bottleneck |')
        add('|---|---|---:|---:|---:|---:|---:|---:|---:|---:|')
        for (mode,m,rate,depth),xs in sorted(group(faults,lambda r:(r['metadata']['mode'],param(r,'prediction_fault_mode'),int(param(r,'prediction_fault_rate_bps')),int(param(r,'postchange_warmup_blocks')))).items()):
            prob=[r.get('adaptive_state',{}).get('mean_probability_q16',0)/65535 for r in xs]
            precision=[]; recall=[]
            for r in xs:
                pos=r['feedback'].get('positive_observations',0); neg=r['feedback'].get('negative_observations',0); miss=r['feedback'].get('candidate_misses',0)
                if pos+neg: precision.append(pos/(pos+neg))
                if pos+miss: recall.append(pos/(pos+miss))
            add(f"| {mode} | {m} | {rate} | {depth} | {f(med([r['feedback']['candidate_misses'] for r in xs]),1)} | {f(med(precision),3)} | {f(med(recall),3)} | {f(med([r.get('adaptive_state',{}).get('runtime_fallback_relationships',0) for r in xs]),1)} | {f(med(prob),3)} | {f(med([ratio_milli(r,'consensus.throughput_speedup_milli') for r in xs]))}x |")

    trans=[r for r in records if r['metadata']['experiment_id']=='conflictlab-v1-adaptation-transitions']
    if trans:
        add('');add('## Non-stationary adaptation by blocks since regime change')
        add('| mode | transition | depth | admission | bypass % | posterior | confidence | replay | phase bottleneck |')
        add('|---|---|---:|---|---:|---:|---:|---:|---:|')
        def transition_name(r):
            old_hot=int(param(r,'warmup_hot_account_probability_bps',param(r,'hot_account_probability_bps'))); new_hot=int(param(r,'hot_account_probability_bps'))
            old_work=int(param(r,'warmup_work_iterations',param(r,'work_iterations'))); new_work=int(param(r,'work_iterations'))
            if old_hot!=new_hot: return f'hot {old_hot}->{new_hot}'
            return f'work {old_work}->{new_work}'
        for (mode,t,d,adm),xs in sorted(group(trans,lambda r:(r['metadata']['mode'],transition_name(r),int(param(r,'postchange_warmup_blocks')),param(r,'acg.serial_bypass_enabled'))).items()):
            add(f"| {mode} | {t} | {d} | {adm} | {100*sum(r['planning']['serial_bypassed'] for r in xs)/len(xs):.1f} | {f(med([r.get('adaptive_state',{}).get('mean_probability_q16',0)/65535 for r in xs]),3)} | {f(med([r.get('adaptive_state',{}).get('mean_confidence_q16',0)/65535 for r in xs]),3)} | {f(med([r['execution']['replayed_transactions'] for r in xs]),1)} | {f(med([ratio_milli(r,'consensus.throughput_speedup_milli') for r in xs]))}x |")

    sem=[r for r in records if r['metadata']['experiment_id']=='conflictlab-v1-execution-semantics']
    if sem:
        add('');add('## Runtime semantics coverage')
        add('| mix | scan | remove | host query | bank read | all-bal read | balance writes | created contracts | failed spec receipts | misses | correct |')
        add('|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|')
        for m,xs in group(sem,lambda r:param(r,'operation_mix')).items():
            c=lambda fld:sum(r['execution']['contract'].get(fld,0) for r in xs)
            failed=sum((r.get('consensus',{}).get('failed_preexecution_receipts') or 0) for r in xs)
            add(f"| {m} | {c('host_storage_scans')} | {c('host_storage_removes')} | {c('host_queries')} | {c('mvcc_balance_reads')} | {c('mvcc_all_balances_reads')} | {sum(r['execution']['contract'].get('receipt_balance_writes',0) for r in xs)} | {sum(r['execution']['contract'].get('receipt_created_contracts',0) for r in xs)} | {failed} | {sum(r['feedback']['candidate_misses'] for r in xs)} | {sum(r['correctness']['serial_equivalent'] is True for r in xs)}/{len(xs)} |")

    scale=[r for r in records if r['metadata']['experiment_id']=='conflictlab-v1-block-scaling']
    if scale:
        add('');add('## Block-size scaling (fixed 6 workers)')
        add('| block | logical edges | materialized | planning ms | feedback ms | phase bottleneck |')
        add('|---:|---:|---:|---:|---:|---:|')
        for b,xs in sorted(group(scale,lambda r:int(param(r,'transactions'))).items()):
            add(f"| {b} | {f(med([r['scheduling']['candidate_edges'] for r in xs]),1)} | {f(med([r['scheduling']['materialized_candidate_edges'] for r in xs]),1)} | {f(med([ms(r['planning']['total_nanos']) for r in xs]))} | {f(med([ms(r['feedback_timing']['total_nanos']) for r in xs]))} | {f(med([ratio_milli(r,'consensus.throughput_speedup_milli') for r in xs]))}x |")

    pareto=[r for r in records if r['metadata']['experiment_id']=='conflictlab-v1-policy-pareto']
    if pareto:
        add('');add('## Policy risk Pareto')
        add('| mode | risk | validation | phase bottleneck | post ms | reuse % | replay |')
        add('|---|---:|---:|---:|---:|---:|---:|')
        for (mode,risk),xs in sorted(group(pareto,lambda r:(r['metadata']['mode'],float(param(r,'acg.risk_budget')))).items()):
            reuse=[100*r['execution']['reused_results']/max(1,r['consensus']['decided_transactions']) for r in xs]
            add(f"| {mode} | {risk:.2f} | {f(med([ratio_milli(r,'consensus.validation_latency_speedup_milli') for r in xs]))}x | {f(med([ratio_milli(r,'consensus.throughput_speedup_milli') for r in xs]))}x | {f(med([ms(r['consensus']['post_consensus_nanos']) for r in xs]))} | {f(med(reuse),1)} | {f(med([r['execution']['replayed_transactions'] for r in xs]),1)} |")

    buckets=[r for r in records if r['metadata']['experiment_id']=='conflictlab-v1-bucket-sensitivity']
    if buckets:
        add('');add('## Prediction bucket sensitivity')
        add('| mode | buckets | materialized | predictor precision | replay | phase bottleneck |')
        add('|---|---:|---:|---:|---:|---:|')
        for (mode,bucket_count),xs in sorted(group(buckets,lambda r:(r['metadata']['mode'],int(param(r,'prediction_buckets')))).items()):
            precision=[]
            for r in xs:
                pos=r['feedback'].get('positive_observations',0); neg=r['feedback'].get('negative_observations',0)
                if pos+neg: precision.append(pos/(pos+neg))
            add(f"| {mode} | {bucket_count} | {f(med([r['scheduling']['materialized_candidate_edges'] for r in xs]),1)} | {f(med(precision),3)} | {f(med([r['execution']['replayed_transactions'] for r in xs]),1)} | {f(med([ratio_milli(r,'consensus.throughput_speedup_milli') for r in xs]))}x |")

    ordering=[r for r in records if r['metadata']['experiment_id']=='conflictlab-v1-ordering-sensitivity']
    if ordering:
        add('');add('## Block-order sensitivity')
        add('| mode | mempool order | validation | phase bottleneck | replay | oracle realization |')
        add('|---|---|---:|---:|---:|---:|')
        for (mode,policy),xs in sorted(group(ordering,lambda r:(r['metadata']['mode'],param(r,'sim.mempool_policy'))).items()):
            realization=[]
            for r in xs:
                b=r['parallelism'].get('perfect_conflict_parallel_lower_bound_nanos'); w=r['parallelism'].get('actual_execution_wall_nanos')
                if b: realization.append(w/b)
            add(f"| {mode} | {policy} | {f(med([ratio_milli(r,'consensus.validation_latency_speedup_milli') for r in xs]))}x | {f(med([ratio_milli(r,'consensus.throughput_speedup_milli') for r in xs]))}x | {f(med([r['execution']['replayed_transactions'] for r in xs]),1)} | {f(med(realization))}x |")

    lifecycle=[r for r in records if r['metadata']['experiment_id']=='conflictlab-v1-vm-lifecycle']
    if lifecycle:
        add('');add('## VM lifecycle sentinel')
        add('| complexity | lifecycle | VM acquire us/tx | phase bottleneck | sequential | correct |')
        add('|---|---|---:|---:|---:|---:|')
        for (cx,life),xs in sorted(group(lifecycle,lambda r:(param(r,'complexity'),param(r,'vm_instance_lifecycle'))).items()):
            vm=[]
            for r in xs:
                tx=max(1,r['execution']['transactions']); vm.append(r['execution']['contract'].get('aggregate_wasm_instance_acquire_nanos',0)/tx/1000)
            add(f"| {cx} | {life} | {f(med(vm),2)} | {f(med([ratio_milli(r,'consensus.throughput_speedup_milli') for r in xs]))}x | {f(med([ratio_milli(r,'pipeline_timing.end_to_end_speedup_milli') for r in xs]))}x | {sum(r['correctness']['serial_equivalent'] is True for r in xs)}/{len(xs)} |")

    if core:
        enabled=[r for r in core if param(r,'acg.serial_bypass_enabled')=='true']
        disabled=[r for r in core if param(r,'acg.serial_bypass_enabled')=='false']
        def admission_key(r):
            params=dict(r['metadata']['parameters']); params.pop('acg.serial_bypass_enabled',None)
            return (r['metadata']['mode'],r['metadata']['seed'],tuple(sorted(params.items())))
        disabled_by_key={admission_key(r):r for r in disabled}
        add('');add('## Admission behavior')
        add('| mode | enabled blocks | bypassed | bypass rate | bypass improved matched bottleneck | median bottleneck delta when bypassed | projected speedup median |')
        add('|---|---:|---:|---:|---:|---:|---:|')
        for mode,xs in group(enabled,lambda r:r['metadata']['mode']).items():
            projected=[r['planning'].get('serial_bypass_projected_speedup_milli')/1000 for r in xs if isinstance(r['planning'].get('serial_bypass_projected_speedup_milli'),(int,float))]
            bypassed=[r for r in xs if r['planning']['serial_bypassed']]; deltas=[]
            for r in bypassed:
                other=disabled_by_key.get(admission_key(r))
                if other is not None:
                    deltas.append(ratio_milli(r,'consensus.throughput_speedup_milli')-ratio_milli(other,'consensus.throughput_speedup_milli'))
            add(f"| {mode} | {len(xs)} | {len(bypassed)} | {100*len(bypassed)/len(xs):.1f}% | {sum(d>0 for d in deltas)}/{len(deltas)} | {f(med(deltas))}x | {f(med(projected))}x |")

    stat=[r for r in records if r['metadata']['experiment_id']=='conflictlab-v1-statistical-headlines']
    if stat:
        add('');add('## Statistical headline distribution (20 seeds/configuration)')
        add('| mode | n | bottleneck median | bottleneck p05/p95 | validation median | post p95 ms | sequential median |')
        add('|---|---:|---:|---:|---:|---:|---:|')
        for m,xs in group(stat,lambda r:r['metadata']['mode']).items():
            b=[ratio_milli(r,'consensus.throughput_speedup_milli') for r in xs]; v=[ratio_milli(r,'consensus.validation_latency_speedup_milli') for r in xs]
            add(f"| {m} | {len(xs)} | {f(med(b))}x | {f(pct(b,.05))}/{f(pct(b,.95))}x | {f(med(v))}x | {f(pct([ms(r['consensus']['post_consensus_nanos']) for r in xs],.95))} | {f(med([ratio_milli(r,'pipeline_timing.end_to_end_speedup_milli') for r in xs]))}x |")

    soak=[r for r in records if r['metadata']['experiment_id']=='conflictlab-v1-long-run-soak']
    if soak:
        add('');add('## Long-run soak')
        for r in soak: add(f"- mode={r['metadata']['mode']} seed={r['metadata']['seed']} prior_blocks={param(r,'warmup_blocks')} correct={r['correctness']['serial_equivalent']} fallback_relationships={r.get('adaptive_state',{}).get('runtime_fallback_relationships',0)} phase_bottleneck={f(ratio_milli(r,'consensus.throughput_speedup_milli'))}x")
    return '\n'.join(lines)+'\n'

def main():
    ap=argparse.ArgumentParser();ap.add_argument('records',type=Path);ap.add_argument('--output',type=Path,required=True);ap.add_argument('--markdown',type=Path)
    a=ap.parse_args(); text=write(load(a.records)); a.output.write_text(text)
    if a.markdown:a.markdown.write_text(text)
    print(text,end='')
if __name__=='__main__':main()
