#!/usr/bin/env python3
import argparse
from pathlib import Path
import matplotlib.pyplot as plt
from common import read_csv,f,save
p=argparse.ArgumentParser(); p.add_argument('--result-root',type=Path,required=True); p.add_argument('--output-dir',type=Path,required=True); a=p.parse_args(); base=a.result_root/'10-block-size'; points={}
for d in base.glob('tx-*'):
 src=d/'summary/summary.csv'
 if not src.exists(): continue
 tx=int(d.name.split('-')[1]);
 for r in read_csv(src): points.setdefault(r.get('label'),[]).append((tx,f(r,'throughput_speedup'),f(r,'throughput_speedup_ci95')))
if points:
 fig,ax=plt.subplots(figsize=(6.4,4.0));
 for label in ['BlockSTM','AriaFB','Vegeta','Rust-ACG']:
  ps=sorted(points.get(label,[]));
  if ps: ax.errorbar([x for x,_,_ in ps],[y for _,y,_ in ps],yerr=[e for *_,e in ps] if any(e for *_,e in ps) else None,marker='o',capsize=3,label=label)
 ax.set_xscale('log',base=2); ax.axhline(1.0,linewidth=.8); ax.set_xlabel('Transactions per block'); ax.set_ylabel('Replay speedup vs Serial'); ax.set_title('Block-size break-even'); ax.legend(); save(fig,a.output_dir/'fig07-block-size-break-even.pdf'); plt.close(fig)
