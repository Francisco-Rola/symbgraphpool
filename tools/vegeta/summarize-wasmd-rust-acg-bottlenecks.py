#!/usr/bin/env python3
import argparse, json
from pathlib import Path

def load(path):
    return [json.loads(line) for line in Path(path).read_text().splitlines() if line.strip()]

def fmt_map(m):
    if not m: return '-'
    return ','.join(f'{k}:{v}' for k,v in sorted(m.items(), key=lambda kv:(-kv[1],kv[0])))

def main():
    ap=argparse.ArgumentParser()
    ap.add_argument('--input',required=True)
    ap.add_argument('--output-dir',required=True)
    ap.add_argument('--top',type=int,default=10)
    args=ap.parse_args()
    out=Path(args.output_dir); out.mkdir(parents=True,exist_ok=True)
    rows=[r for r in load(args.input) if r.get('strategy')=='cosmos-wasmd-symbgraph-rust']
    if not rows: raise SystemExit('no cosmos-wasmd-symbgraph-rust rows')
    detailed=[]; lines=['Wasmd Rust-ACG worst-block diagnostics','']
    for workers in sorted({r['workers'] for r in rows}):
        group=[r for r in rows if r['workers']==workers]
        group.sort(key=lambda r:r['strategy_total_nanos'],reverse=True)
        lines += [f'workers={workers} top {min(args.top,len(group))} slowest blocks',
                  'sample block wall-ms post-ms post-x plan preexec util% acg-x oracle-x gap cand[p/l/g] parent[b/e] cp-tx/o path  provenance  decisions  cp-provenance  plan[graph/sched/proj/other]']
        for r in group[:args.top]:
            item={
              'workers':workers,'sample':r['sample'],'block_number':r['block_number'],
              'wall_ms':r['strategy_total_nanos']/1e6,'serial_ms':r['matched_serial_nanos']/1e6,
              'speedup':r.get('matched_serial_speedup',0),'transactions':r['transactions'],'reexecutions':r['reexecutions'],
              'post_ms':float(r.get('post_consensus_nanos') or r.get('symb_reconciliation_nanos') or r['strategy_total_nanos'])/1e6,
              'post_speedup':float(r['matched_serial_nanos'])/float(r.get('post_consensus_nanos') or r.get('symb_reconciliation_nanos') or r['strategy_total_nanos']),
              'dependencies':r.get('symb_dependency_edges',0),'dag_parallelism':r.get('symb_dag_parallelism',0),
              'physical_candidate_edges':r.get('symb_physical_candidate_edges',0),
              'logical_candidate_edges':r.get('symb_logical_candidate_edges',0),
              'compact_candidate_groups':r.get('symb_compact_candidate_groups',0),
              'parent_dependencies_before_reduction':r.get('symb_parent_dependencies_before_reduction',0),
              'parent_dependencies_elided_reduction':r.get('symb_parent_dependencies_elided_reduction',0),
              'oracle_edges':r.get('symb_oracle_conflict_edges',0),'oracle_dag_parallelism':r.get('symb_oracle_dag_parallelism',0),
              'serialization_gap':r.get('symb_serialization_gap',0),'oracle_critical_path_tx':r.get('symb_oracle_critical_path_tx',0),
              'worker_utilization':r.get('symb_worker_utilization',0),'critical_path_tx':r.get('symb_critical_path_tx',0),
              'critical_path':r.get('symb_critical_path') or [],
              'dependency_primary':r.get('symb_dependency_primary') or {},
              'dependency_provenance':r.get('symb_dependency_provenance') or {},
              'dependency_decisions':r.get('symb_dependency_decisions') or {},
              'critical_path_reasons':r.get('symb_critical_path_reasons') or {},
              'critical_path_provenance':r.get('symb_critical_path_provenance') or {},
              'critical_path_decisions':r.get('symb_critical_path_decisions') or {},
              'critical_path_cost_by_reason':r.get('symb_critical_path_cost_by_reason') or {},
              'planning':r.get('symb_planning') or {},
              'phases_ms':{k:float(r.get(k,0))/1e6 for k in [
                'symb_plan_nanos','symb_preexecution_nanos','symb_reconciliation_nanos','symb_visibility_nanos','symb_spec_execution_nanos','symb_delta_capture_nanos',
                'symb_validation_nanos','symb_replay_execution_nanos','symb_feedback_build_nanos','symb_rust_feedback_nanos',
                'symb_plan_request_build_nanos','symb_plan_request_marshal_nanos','symb_plan_cgo_roundtrip_nanos','symb_plan_response_unmarshal_nanos',
                'symb_plan_rust_decode_nanos','symb_plan_resolve_components_nanos','symb_plan_candidate_graph_nanos','symb_plan_scheduler_nanos',
                'symb_plan_projection_nanos','symb_plan_feedback_pairs_nanos','symb_plan_finalize_nanos','symb_plan_bridge_other_nanos']},
            }
            detailed.append(item)
            ph=item['phases_ms']
            fb=ph['symb_feedback_build_nanos']+ph['symb_rust_feedback_nanos']
            planparts=f"{ph['symb_plan_candidate_graph_nanos']:.1f}/{ph['symb_plan_scheduler_nanos']:.1f}/{ph['symb_plan_projection_nanos']:.1f}/{ph['symb_plan_bridge_other_nanos']:.1f}"
            lines.append(f"{item['sample']:>6} {item['block_number']:>5} {item['wall_ms']:>7.1f} {item['post_ms']:>7.1f} {item['post_speedup']:>6.1f} {ph['symb_plan_nanos']:>5.1f} {ph['symb_preexecution_nanos']:>7.1f} {100*item['worker_utilization']:>5.1f}% {item['dag_parallelism']:>5.2f} {item['oracle_dag_parallelism']:>7.2f} {item['serialization_gap']:>4.2f} {item['physical_candidate_edges']:>4}/{item['logical_candidate_edges']:<4}/{item['compact_candidate_groups']:<3} {item['parent_dependencies_before_reduction']:>4}/{item['parent_dependencies_elided_reduction']:<3} {item['critical_path_tx']:>3}/{item['oracle_critical_path_tx']:<3} {str(item['critical_path']):<18} {fmt_map(item['dependency_provenance']):<30} {fmt_map(item['dependency_decisions']):<24} {fmt_map(item['critical_path_provenance']):<28} {planparts}")
        lines.append('')
    (out/'worst-blocks.json').write_text(json.dumps(detailed,indent=2,sort_keys=True)+'\n')
    (out/'worst-blocks.txt').write_text('\n'.join(lines)+'\n')
    print('\n'.join(lines))

if __name__=='__main__': main()
