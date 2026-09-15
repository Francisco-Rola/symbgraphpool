#!/usr/bin/env python3
"""Build the two paper-facing EuroSys tables from frozen workload/evaluation artifacts."""
from __future__ import annotations
import argparse,csv,json
from pathlib import Path

ROOT=Path(__file__).resolve().parents[2]

def load(path:Path):
 try:return json.loads(path.read_text(encoding='utf-8'))
 except (OSError,json.JSONDecodeError):return {}

def plan_stats(path:Path):
 blocks=tx=0
 try:
  with path.open(encoding='utf-8') as f:
   for line in f:
    if not line.strip(): continue
    b=json.loads(line); blocks+=1; tx+=len(b.get('transactions') or [])
 except OSError: pass
 return blocks,tx

def get(d,*keys,default=None):
 cur=d
 for k in keys:
  if not isinstance(cur,dict) or k not in cur:return default
  cur=cur[k]
 return cur

def pct(v):
 return '' if v is None else f'{100*float(v):.2f}'

def write_csv(path,rows):
 path.parent.mkdir(parents=True,exist_ok=True); cols=[]
 for r in rows:
  for k in r:
   if k not in cols: cols.append(k)
 with path.open('w',newline='',encoding='utf-8') as f:
  w=csv.DictWriter(f,fieldnames=cols); w.writeheader(); w.writerows(rows)

def tex_escape(v):
 return str(v).replace('\\','\\textbackslash{}').replace('&','\\&').replace('%','\\%').replace('_','\\_').replace('#','\\#')

def write_tex(path,rows,cols,headers,caption,label):
 lines=['\\begin{table*}[t]','\\centering','\\small','\\begin{tabular}{'+'l'*(len(cols))+'}','\\toprule',' & '.join(headers)+' \\\\','\\midrule']
 for r in rows: lines.append(' & '.join(tex_escape(r.get(c,'')) for c in cols)+' \\\\')
 lines += ['\\bottomrule','\\end{tabular}',f'\\caption{{{caption}}}',f'\\label{{{label}}}','\\end{table*}']
 path.write_text('\n'.join(lines)+'\n',encoding='utf-8')

def main():
 ap=argparse.ArgumentParser(); ap.add_argument('--result-root',type=Path,required=True); ap.add_argument('--output-dir',type=Path,required=True); a=ap.parse_args(); a.output_dir.mkdir(parents=True,exist_ok=True)
 s1r=load(ROOT/'benchmarks/corpora/vegeta-ethereum/s1/native-plan/readiness.json'); s4r=load(ROOT/'benchmarks/corpora/vegeta-ethereum/s4/native-plan/readiness.json')
 topo=load(a.result_root/'16-translation-fidelity/topology/native-topology-fidelity.json'); cost=load(a.result_root/'16-translation-fidelity/cost/summary.json')
 workloads=[]
 for name,slug,prov,oracle,ready in [('S1-derived Wasmd','s1','Ethereum S1, translated','No',s1r),('S3-derived Wasmd','s3','Ethereum S3 exact trace, translated','Yes',{}),('S4-derived Wasmd','s4','Ethereum S4, translated','No',s4r)]:
  b,t=plan_stats(ROOT/f'benchmarks/corpora/vegeta-ethereum/{slug}/native-execution/execution-plan.jsonl')
  if slug=='s1':
   conflict=get(ready,'common_gates','reviewed_state_touch_conflict_coverage','value'); contention=get(ready,'profiles','scheduler-fidelity','gates','successful_reviewed_conflict_participant_tx_coverage','value'); semantic=get(ready,'profiles','semantic-replay','gates','successful_reviewed_all_tx_coverage','value')
  elif slug=='s4':
   conflict=get(ready,'metrics','semantic_conflict'); contention=get(ready,'metrics','contention_tx'); semantic=get(ready,'metrics','semantic_tx')
  else: conflict=contention=semantic=None
  topology_precision = pct(get(topo,'conflict_pairs','precision')) if slug=='s3' else '—'
  topology_recall = pct(get(topo,'conflict_pairs','recall')) if slug=='s3' else '—'
  cost_spearman = (f"{float(get(cost,'correlation','native_vs_steps_spearman')):.3f}" if slug=='s3' and get(cost,'correlation','native_vs_steps_spearman') is not None else '—')
  workloads.append({'workload':name,'provenance':prov,'blocks':b or '—','transactions':t or '—','selector_conflict_coverage_pct':pct(conflict) or ('exact audit' if slug=='s3' else '—'),'conflict_participant_tx_pct':pct(contention) or ('exact audit' if slug=='s3' else '—'),'all_tx_semantic_pct':pct(semantic) or ('exact audit' if slug=='s3' else '—'),'exact_topology_precision_pct':topology_precision,'exact_topology_recall_pct':topology_recall,'native_steps_spearman':cost_spearman,'exact_access_oracle':oracle})
 workloads += [
  {'workload':'MiniWarehouse','provenance':'Native generated','blocks':'configurable','transactions':'configurable','selector_conflict_coverage_pct':'native','conflict_participant_tx_pct':'native','all_tx_semantic_pct':'native','exact_topology_precision_pct':'—','exact_topology_recall_pct':'—','native_steps_spearman':'—','exact_access_oracle':'Serial state'},
  {'workload':'NativeMix','provenance':'Native CW20/CW721/AMM','blocks':'configurable','transactions':'configurable','selector_conflict_coverage_pct':'native','conflict_participant_tx_pct':'native','all_tx_semantic_pct':'native','exact_topology_precision_pct':'—','exact_topology_recall_pct':'—','native_steps_spearman':'—','exact_access_oracle':'Serial state'},
  {'workload':'ConflictLab','provenance':'Controlled native Wasm','blocks':'configurable','transactions':'configurable','selector_conflict_coverage_pct':'known by construction','conflict_participant_tx_pct':'known by construction','all_tx_semantic_pct':'native','exact_topology_precision_pct':'—','exact_topology_recall_pct':'—','native_steps_spearman':'—','exact_access_oracle':'Serial state'},
 ]
 write_csv(a.output_dir/'table1-workloads-fidelity.csv',workloads)
 write_tex(a.output_dir/'table1-workloads-fidelity.tex',workloads,['workload','provenance','blocks','transactions','selector_conflict_coverage_pct','conflict_participant_tx_pct','all_tx_semantic_pct','exact_topology_precision_pct','exact_topology_recall_pct','native_steps_spearman','exact_access_oracle'],['Workload','Provenance','Blocks','Tx','Conflict cov. (\\%)','Conflict-tx (\\%)','All-tx sem. (\\%)','Topo. P (\\%)','Topo. R (\\%)','Cost $\\rho$','Exact oracle'],'Workloads and translation/readiness scope. S3 topology/cost fidelity is measured against exact source traces; S1/S4 coverage values are scheduler-facing diagnostics, not EVM semantic-equivalence claims.','tab:workloads')
 # Table 2 is the accepted semantic matrix; preserve every operation case rather than cherry-picking.
 src=a.result_root/'12-semantics/aggregate/summary-wide.csv'; acceptance=load(a.result_root/'12-semantics/acceptance.json'); sem=[]
 if src.is_file():
  with src.open(newline='',encoding='utf-8') as f:
   for r in csv.DictReader(f):
    def fv(k):
     try:return float(r.get(k) or 0)
     except:return 0.0
    sem.append({'mode':r.get('mode',''),'operation_mix':r.get('param.operation_mix',''),'contention':r.get('param.contention',''),'n':r.get('n',''),'throughput_speedup':f"{fv('throughput_speedup.mean'):.3f}",'replayed_transactions':f"{fv('replayed_transactions.mean'):.1f}",'candidate_misses':f"{fv('candidate_misses.mean'):.1f}",'accepted':'yes' if acceptance.get('status')=='accepted' else str(acceptance.get('status') or 'unknown')})
 write_csv(a.output_dir/'table2-semantics-correctness.csv',sem)
 if sem: write_tex(a.output_dir/'table2-semantics-correctness.tex',sem,['mode','operation_mix','contention','n','throughput_speedup','replayed_transactions','candidate_misses','accepted'],['Mode','Operations','Contention','n','Speedup','Replay tx','Misses','Accepted'],'Semantic and correctness coverage under non-point state operations. All rows must pass the release acceptance gate.','tab:semantics')
 print(a.output_dir)
if __name__=='__main__': main()
