#!/usr/bin/env python3
import argparse
from pathlib import Path
import matplotlib.pyplot as plt
from common import read_csv,f,save
p=argparse.ArgumentParser(); p.add_argument('--result-root',type=Path,required=True); p.add_argument('--output-dir',type=Path,required=True); a=p.parse_args()
base=a.result_root/'06-contention'; points={}
for d in base.glob('lanes-*'):
 src=d/'summary/summary.csv'
 if not src.exists(): continue
 lanes=int(d.name.split('-')[1]); rows=read_csv(src)
 for r in rows: points.setdefault(r.get('label'),[]).append((lanes,f(r,'throughput_speedup'),f(r,'throughput_speedup_ci95')))
if points:
 fig,ax=plt.subplots(figsize=(6.4,4.0))
 for label in ['BlockSTM','AriaFB','Vegeta','Rust-ACG']:
  ps=sorted(points.get(label,[]));
  if ps: ax.errorbar([x for x,_,_ in ps],[y for _,y,_ in ps],yerr=[e for *_,e in ps] if any(e for *_,e in ps) else None,marker='o',capsize=3,label=label)
 ax.set_xscale('log'); ax.invert_xaxis(); ax.axhline(1.0,linewidth=.8); ax.set_xlabel('Independent key lanes (fewer = more contention)'); ax.set_ylabel('Replay speedup vs Serial'); ax.set_title('Controlled contention'); ax.legend(); save(fig,a.output_dir/'fig04-contention.pdf'); plt.close(fig)
