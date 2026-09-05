#!/usr/bin/env python3
"""Emit the compact semantic-coverage table used by the paper appendix."""
import argparse,csv,json
from pathlib import Path
from common import read_csv,f
p=argparse.ArgumentParser(); p.add_argument('--result-root',type=Path,required=True); p.add_argument('--output-dir',type=Path,required=True); a=p.parse_args()
base=a.result_root/'12-semantics'; src=base/'aggregate/summary-wide.csv'; acceptance=base/'acceptance.json'
if src.exists():
 if acceptance.exists():
  status=json.loads(acceptance.read_text()).get('status')
  if status!='accepted': raise SystemExit(f'semantics matrix not accepted: {status}')
 rows=read_csv(src); out=[]
 for r in rows:
  out.append({
   'mode':r.get('mode',''), 'operation_mix':r.get('param.operation_mix',''), 'contention':r.get('param.contention',''), 'n':r.get('n',''),
   'throughput_speedup':f(r,'throughput_speedup.mean'), 'replayed_transactions':f(r,'replayed_transactions.mean'), 'candidate_misses':f(r,'candidate_misses.mean'),
   'accepted':'yes' if not acceptance.exists() or json.loads(acceptance.read_text()).get('status')=='accepted' else 'no',
  })
 a.output_dir.mkdir(parents=True,exist_ok=True); path=a.output_dir/'table-semantics.csv'
 with path.open('w',newline='',encoding='utf-8') as fh:
  w=csv.DictWriter(fh,fieldnames=list(out[0]) if out else ['mode','operation_mix','contention','n','throughput_speedup','replayed_transactions','candidate_misses','accepted']); w.writeheader(); w.writerows(out)
