#!/usr/bin/env python3
"""Summarize fixed-calibration compute-intensity sweeps for S1/S4."""
from __future__ import annotations
import argparse,csv,json,math,statistics
from pathlib import Path

STRATEGIES={
 'cosmos-wasmd-direct-serial':'Serial','cosmos-wasmd-block-stm':'BlockSTM','cosmos-wasmd-aria-fb':'AriaFB',
 'cosmos-wasmd-vegeta':'Vegeta','cosmos-wasmd-symbgraph-rust':'Rust-ACG'}

def f(r,k):
 try:return float(r.get(k) or 0)
 except:return 0.0

def fit(xs,ys):
 if len(xs)<2:return 0.0,0.0,0.0
 xm=statistics.fmean(xs); ym=statistics.fmean(ys); den=sum((x-xm)**2 for x in xs)
 slope=sum((x-xm)*(y-ym) for x,y in zip(xs,ys))/den if den else 0.0; intercept=ym-slope*xm
 pred=[intercept+slope*x for x in xs]; ssr=sum((y-p)**2 for y,p in zip(ys,pred)); sst=sum((y-ym)**2 for y in ys)
 return intercept,slope,1-ssr/sst if sst else 1.0

def main():
 ap=argparse.ArgumentParser(); ap.add_argument('--root',type=Path,required=True); ap.add_argument('--dataset',required=True); ap.add_argument('--scales',required=True); ap.add_argument('--workers',type=int,required=True); ap.add_argument('--output-dir',type=Path,required=True); a=ap.parse_args()
 scales=[float(x) for x in a.scales.split(',') if x.strip()]; rows=[]; serial=[]
 for scale in scales:
  label=('%.12g'%scale).replace('.','p'); p=a.root/f'scale-{label}'/'summary'/'summary.csv'
  if not p.is_file(): raise SystemExit(f'missing compute-sweep summary: {p}')
  data=list(csv.DictReader(p.open(encoding='utf-8')))
  selected=[r for r in data if int(float(r.get('workers') or 0))==a.workers and r.get('strategy') in STRATEGIES]
  if len(selected)!=len(STRATEGIES): raise SystemExit(f'incomplete strategies at scale={scale}: {len(selected)}')
  for r in selected:
   row={'dataset':a.dataset,'scale':scale,'workers':a.workers,'strategy':r['strategy'],'label':STRATEGIES[r['strategy']],
        'throughput_tps':f(r,'throughput_tps'),'replay_speedup':f(r,'throughput_speedup'),'post_ms':f(r,'post_ms'),
        'tail_speedup':f(r,'overlap_tail_x'),'commit_speedup':f(r,'commit_x'),'reexec_pct':f(r,'reexec_pct')}
   rows.append(row)
   if row['label']=='Serial': serial.append(row)
 xs=[r['scale'] for r in serial]; ys=[r['post_ms'] for r in serial]; intercept,slope,r2=fit(xs,ys)
 for row in rows:
  pred=intercept+slope*row['scale']; row['fitted_supplement_share_pct']=100*max(0,slope*row['scale'])/pred if pred>0 else 0.0
 a.output_dir.mkdir(parents=True,exist_ok=True); out=a.output_dir/'compute-sensitivity.csv'
 with out.open('w',newline='',encoding='utf-8') as fh:
  w=csv.DictWriter(fh,fieldnames=list(rows[0]) if rows else []); w.writeheader(); w.writerows(rows)
 obj={'schema_version':1,'dataset':a.dataset,'workers':a.workers,'scales':scales,'serial_linear_fit':{'fixed_ms':intercept,'supplemental_ms_per_scale':slope,'r2':r2},'rows':rows}
 (a.output_dir/'compute-sensitivity.json').write_text(json.dumps(obj,indent=2)+'\n',encoding='utf-8')
 print(out)
if __name__=='__main__': main()
