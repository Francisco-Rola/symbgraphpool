#!/usr/bin/env python3
"""Fail-closed publication-readiness gate for the frozen Vegeta S4 native translation."""
from __future__ import annotations
import argparse,json
from pathlib import Path

def load(p): return json.loads(p.read_text(encoding='utf-8'))
def main():
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--family-map',type=Path,required=True); ap.add_argument('--family-coverage',type=Path,required=True); ap.add_argument('--translation-coverage',type=Path,required=True); ap.add_argument('--semantic-coverage',type=Path,required=True); ap.add_argument('--transaction-deficit',type=Path,required=True)
    ap.add_argument('--min-conflict',type=float,default=.95); ap.add_argument('--min-median-block',type=float,default=.80); ap.add_argument('--min-semantic-tx',type=float,default=.80); ap.add_argument('--min-contention-tx',type=float,default=.80); ap.add_argument('--output',type=Path,required=True); ap.add_argument('--text-output',type=Path,required=True); ap.add_argument('--allow-low',action='store_true'); ns=ap.parse_args()
    fmap=load(ns.family_map); fam=load(ns.family_coverage); trans=load(ns.translation_coverage); sem=load(ns.semantic_coverage); deficit=load(ns.transaction_deficit)
    if fmap.get('candidate_only'): raise SystemExit('refusing candidate_only S4 family map; freeze a reviewed evaluation/vegeta/s4-native-family-map.v1.json first')
    metrics={
      'family_conflict':float((fam.get('source_conflict_coverage') or {}).get('coverage',0)),
      'family_median':float((fam.get('block_balanced_conflict_coverage') or {}).get('median_coverage') or 0),
      'semantic_conflict':float(sem.get('coverage',0)),
      'semantic_median':float((sem.get('block_balanced') or {}).get('median_coverage') or 0),
      'semantic_tx':float(((deficit.get('denominators') or {}).get('all_source_transactions') or {}).get('successful_reviewed_state_coverage',0)),
      'contention_tx':float(((deficit.get('denominators') or {}).get('source_conflict_participating_transactions') or {}).get('successful_reviewed_state_coverage',0)),
      'implementation_ready':bool((trans.get('implementation_readiness') or {}).get('native_execution_ready')),
    }
    gates={
      'family_conflict':metrics['family_conflict']>=ns.min_conflict,
      'family_median':metrics['family_median']>=ns.min_median_block,
      'semantic_conflict':metrics['semantic_conflict']>=ns.min_conflict,
      'semantic_median':metrics['semantic_median']>=ns.min_median_block,
      'semantic_tx':metrics['semantic_tx']>=ns.min_semantic_tx,
      'contention_tx':metrics['contention_tx']>=ns.min_contention_tx,
      'implementation_ready':metrics['implementation_ready'],
    }
    ready=all(gates.values())
    report={'schema_version':1,'dataset':'vegeta-s4','ready':ready,'metrics':metrics,'gates':gates,'thresholds':{'conflict':ns.min_conflict,'median_block':ns.min_median_block,'semantic_tx':ns.min_semantic_tx,'contention_tx':ns.min_contention_tx},'oracle_scope':'no exact SLOAD/SSTORE oracle; S4 is a real-trace translation/throughput workload'}
    ns.output.parent.mkdir(parents=True,exist_ok=True); ns.output.write_text(json.dumps(report,indent=2,sort_keys=True)+'\n',encoding='utf-8')
    lines=['Vegeta S4 native readiness','',f"ready: {'PASS' if ready else 'FAIL'}",f"family conflict coverage: {100*metrics['family_conflict']:.2f}%",f"family median block coverage: {100*metrics['family_median']:.2f}%",f"selector-reviewed conflict coverage: {100*metrics['semantic_conflict']:.2f}%",f"selector-reviewed median block: {100*metrics['semantic_median']:.2f}%",f"successful reviewed-state tx: {100*metrics['semantic_tx']:.2f}%",f"successful reviewed-state conflict participants: {100*metrics['contention_tx']:.2f}%",f"implementation ready: {metrics['implementation_ready']}"]
    ns.text_output.write_text('\n'.join(lines)+'\n',encoding='utf-8'); print('\n'.join(lines))
    if not ready and not ns.allow_low: raise SystemExit('S4 native translation is below publication readiness gates')
    return 0
if __name__=='__main__': raise SystemExit(main())
