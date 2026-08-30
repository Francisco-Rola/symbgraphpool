#!/usr/bin/env python3
import argparse, csv, json, math, statistics
from pathlib import Path


def percentile(values, q):
    if not values:
        return 0.0
    xs=sorted(values)
    if len(xs)==1: return xs[0]
    pos=(len(xs)-1)*q
    lo=int(math.floor(pos)); hi=int(math.ceil(pos))
    if lo==hi: return xs[lo]
    return xs[lo]*(hi-pos)+xs[hi]*(pos-lo)

def median(values):
    return statistics.median(values) if values else 0.0

def load_rows(path):
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]

def sum_map(rows, key):
    out={}
    for row in rows:
        for k,v in (row.get(key) or {}).items(): out[k]=out.get(k,0)+v
    return out

def sample_metrics(rows):
    symb=[r for r in rows if r.get('strategy')=='cosmos-wasmd-symbgraph-rust']
    serial=[r for r in rows if r.get('strategy')=='cosmos-wasmd-direct-serial']
    if not symb or not serial: raise ValueError('missing direct-serial or Rust-ACG rows')
    active=sum(r['strategy_total_nanos'] for r in symb)/1e6
    post=sum(int(r.get('post_consensus_nanos') or r.get('symb_reconciliation_nanos') or r['strategy_total_nanos']) for r in symb)/1e6
    serial_ms=sum(r['strategy_total_nanos'] for r in serial)/1e6
    tx=sum(r['transactions'] for r in symb)
    replay=sum(r['reexecutions'] for r in symb)
    blocks=len(symb)
    post_times=[int(r.get('post_consensus_nanos') or r.get('symb_reconciliation_nanos') or r['strategy_total_nanos'])/1e6 for r in symb]
    avg=lambda key: sum(float(r.get(key,0) or 0) for r in symb)/blocks
    deps=sum_map(symb,'symb_dependency_primary')
    prov=sum_map(symb,'symb_dependency_provenance')
    decisions=sum_map(symb,'symb_dependency_decisions')
    cp=sum_map(symb,'symb_critical_path_reasons')
    cp_prov=sum_map(symb,'symb_critical_path_provenance')
    cp_decisions=sum_map(symb,'symb_critical_path_decisions')
    cp_cost=sum_map(symb,'symb_critical_path_cost_by_reason')
    plan=symb[-1].get('symb_planning') or {}
    return {
      'active_ms':active,'post_ms':post,'serial_ms':serial_ms,
      'net_x':serial_ms/active if active else 0,'post_x':serial_ms/post if post else 0,
      'replay_pct':100*replay/tx if tx else 0,'p95_ms':percentile(post_times,.95),'p99_ms':percentile(post_times,.99),
      'deps_per_block':avg('symb_dependency_edges'),'dag_x':avg('symb_dag_parallelism'),
      'physical_candidate_edges':avg('symb_physical_candidate_edges'),
      'logical_candidate_edges':avg('symb_logical_candidate_edges'),
      'compact_candidate_groups':avg('symb_compact_candidate_groups'),
      'parent_dependencies_before_reduction':avg('symb_parent_dependencies_before_reduction'),
      'parent_dependencies_elided_reduction':avg('symb_parent_dependencies_elided_reduction'),
      'oracle_edges':avg('symb_oracle_conflict_edges'),'oracle_dag_x':avg('symb_oracle_dag_parallelism'),
      'serialization_gap':avg('symb_serialization_gap'),'oracle_cp_tx':avg('symb_oracle_critical_path_tx'),
      'util_pct':100*avg('symb_worker_utilization'),'ready':avg('symb_average_ready'),
      'cp_tx':avg('symb_critical_path_tx'),'cp_cost':avg('symb_critical_path_cost'),
      'candidate_hard':avg('symb_candidate_hard'),'candidate_soft':avg('symb_candidate_soft'),'candidate_low':avg('symb_candidate_low'),
      'ordered_hard':avg('symb_ordered_hard'),'ordered_soft':avg('symb_ordered_soft'),
      'dep_primary':{k:v/blocks for k,v in deps.items()},'provenance':{k:v/blocks for k,v in prov.items()},
      'decisions':{k:v/blocks for k,v in decisions.items()},'cp_reasons':{k:v/blocks for k,v in cp.items()},
      'cp_provenance':{k:v/blocks for k,v in cp_prov.items()},'cp_decisions':{k:v/blocks for k,v in cp_decisions.items()},
      'cp_cost_reason':{k:v/blocks for k,v in cp_cost.items()},'planning':plan,
      'plan_request_build_ms':avg('symb_plan_request_build_nanos')/1e6,
      'plan_request_marshal_ms':avg('symb_plan_request_marshal_nanos')/1e6,
      'plan_cgo_ms':avg('symb_plan_cgo_roundtrip_nanos')/1e6,
      'plan_response_unmarshal_ms':avg('symb_plan_response_unmarshal_nanos')/1e6,
      'plan_rust_decode_ms':avg('symb_plan_rust_decode_nanos')/1e6,
      'plan_resolve_ms':avg('symb_plan_resolve_components_nanos')/1e6,
      'plan_graph_ms':avg('symb_plan_candidate_graph_nanos')/1e6,
      'plan_scheduler_ms':avg('symb_plan_scheduler_nanos')/1e6,
      'plan_projection_ms':avg('symb_plan_projection_nanos')/1e6,
      'plan_feedback_pairs_ms':avg('symb_plan_feedback_pairs_nanos')/1e6,
      'plan_finalize_ms':avg('symb_plan_finalize_nanos')/1e6,
      'plan_other_ms':avg('symb_plan_bridge_other_nanos')/1e6,
    }

def main():
    ap=argparse.ArgumentParser()
    ap.add_argument('--input-root',required=True)
    ap.add_argument('--output-dir',required=True)
    args=ap.parse_args()
    root=Path(args.input_root); out=Path(args.output_dir); out.mkdir(parents=True,exist_ok=True)
    rows_out=[]
    for policy_dir in sorted(p for p in root.iterdir() if p.is_dir() and (p/'records.jsonl').exists()):
        rows=load_rows(policy_dir/'records.jsonl')
        workers=sorted({r['workers'] for r in rows if r.get('strategy')=='cosmos-wasmd-symbgraph-rust'})
        for w in workers:
            samples=sorted({r['sample'] for r in rows if r['workers']==w})
            mets=[]
            for sample in samples:
                subset=[r for r in rows if r['workers']==w and r['sample']==sample]
                mets.append(sample_metrics(subset))
            first=mets[0]; plan=first['planning']
            row={'policy':policy_dir.name,'workers':w,'samples':len(mets)}
            for key in ['active_ms','post_ms','serial_ms','net_x','post_x','replay_pct','p95_ms','p99_ms','deps_per_block','dag_x','oracle_edges','oracle_dag_x','serialization_gap','oracle_cp_tx','util_pct','ready','cp_tx','cp_cost','candidate_hard','candidate_soft','candidate_low','ordered_hard','ordered_soft','physical_candidate_edges','logical_candidate_edges','compact_candidate_groups','parent_dependencies_before_reduction','parent_dependencies_elided_reduction','plan_request_build_ms','plan_request_marshal_ms','plan_cgo_ms','plan_response_unmarshal_ms','plan_rust_decode_ms','plan_resolve_ms','plan_graph_ms','plan_scheduler_ms','plan_projection_ms','plan_feedback_pairs_ms','plan_finalize_ms','plan_other_ms']:
                row[key]=median([m[key] for m in mets])
            for reason in ['bank_resource','symbolic_hard','adaptive_hard','soft_risk','projection_hard','unknown']:
                row['dep_'+reason]=median([m['dep_primary'].get(reason,0) for m in mets])
                row['cp_'+reason]=median([m['cp_reasons'].get(reason,0) for m in mets])
                row['cp_cost_'+reason]=median([m['cp_cost_reason'].get(reason,0) for m in mets])
            for value in ['static_predicate','static_profile','runtime_discovered','projection','bank_resource','unknown']:
                row['prov_'+value]=median([m['provenance'].get(value,0) for m in mets])
                row['cp_prov_'+value]=median([m['cp_provenance'].get(value,0) for m in mets])
            for value in ['hard','soft_serialized','projection_restore','bank_hard','unknown']:
                row['decision_'+value]=median([m['decisions'].get(value,0) for m in mets])
                row['cp_decision_'+value]=median([m['cp_decisions'].get(value,0) for m in mets])
            for k,v in plan.items(): row['plan_'+k]=v
            rows_out.append(row)
    default={(r['workers']):r for r in rows_out if r['policy']=='default'}
    for row in rows_out:
        base=default.get(row['workers'])
        row['vs_default_pct']=100*(base['active_ms']-row['active_ms'])/base['active_ms'] if base else 0
    rows_out.sort(key=lambda r:(r['workers'],r['active_ms']))
    csv_path=out/'risk-sweep.csv'
    keys=[]
    for r in rows_out:
        for k in r:
            if k not in keys: keys.append(k)
    with csv_path.open('w',newline='') as f:
        w=csv.DictWriter(f,fieldnames=keys); w.writeheader(); w.writerows(rows_out)
    (out/'risk-sweep.json').write_text(json.dumps(rows_out,indent=2,sort_keys=True)+'\n')
    lines=['Wasmd Rust-ACG focused risk-policy sweep','',
           'Positive vs-default means faster. post-x is Vegeta-style serial/post-consensus speedup; net-x remains full end-to-end speedup.',
           'oracle-x is the after-the-fact actual-access DAG parallelism; gap=oracle-x/acg-x (>1 means serialization headroom).','']
    hdr=f"{'policy':<24} {'w':>2} {'n':>2} {'active':>9} {'net-x':>6} {'post':>9} {'post-x':>7} {'vs-def':>7} {'replay':>7} {'acg-x':>6} {'oracle':>6} {'gap':>5} {'util':>6} {'deps':>6} {'phys':>6} {'log':>6} {'grp':>5} {'p-cut':>6}"
    lines.append(hdr)
    for r in rows_out:
        lines.append(f"{r['policy']:<24} {r['workers']:>2} {r['samples']:>2} {r['active_ms']:>9.1f} {r['net_x']:>6.3f} {r['post_ms']:>9.1f} {r['post_x']:>7.2f} {r['vs_default_pct']:>6.1f}% {r['replay_pct']:>6.2f}% {r['dag_x']:>6.2f} {r['oracle_dag_x']:>6.2f} {r['serialization_gap']:>5.2f} {r['util_pct']:>5.1f}% {r['deps_per_block']:>6.1f} {r['physical_candidate_edges']:>6.1f} {r['logical_candidate_edges']:>6.1f} {r['compact_candidate_groups']:>5.1f} {r['parent_dependencies_elided_reduction']:>6.1f}")
    lines += ['', 'Graph-compaction metrics are in risk-sweep.csv/json (physical/logical candidate edges, compact groups, parent dependencies before/final reduction).',
              'Planning subphases are in risk-sweep.csv/json (request-build/marshal, Rust decode, profile resolution, candidate graph, scheduler, projection, feedback-pairs, finalize, bridge remainder).',
              'Provenance and scheduling-decision dimensions are also in CSV/JSON and no longer change identity when policy thresholds change.',
              'Interpretation target: maximize post-x subject to the pre-consensus phase fitting the consensus window; use net-x to track total resource cost and operational headroom.']
    (out/'risk-sweep.txt').write_text('\n'.join(lines)+'\n')
    print('\n'.join(lines))

if __name__=='__main__': main()
