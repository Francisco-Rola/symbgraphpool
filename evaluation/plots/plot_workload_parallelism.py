#!/usr/bin/env python3
import argparse
from pathlib import Path
import matplotlib.pyplot as plt
from common import read_json,save
p=argparse.ArgumentParser(); p.add_argument('--result-root',type=Path,required=True); p.add_argument('--output-dir',type=Path,required=True); a=p.parse_args()
src=a.result_root/'01-s1/summary/summary.json'
if src.exists():
 d=read_json(src).get('workload_parallelism') or {}; tr={int(r['workers']):r for r in d.get('translated_wasmd_by_workers',[])}
 if tr:
  w=max(tr); row=tr[w]
  labels=['Wasmd hot-key','Wasmd cost-weighted','Ready-wave cost model','Observed tx conc.']
  vals=[row.get('hot_key_chain_ratio',0),row.get('weighted_hot_key_parallelism',0),row.get('ready_wave_ideal_worker_speedup',0),row.get('post_tx_concurrency',0)]
  fig,ax=plt.subplots(figsize=(6.4,4.0)); ax.bar(labels,vals); ax.axhline(float(w),linestyle='--',linewidth=.8,label=f'{w}-worker ceiling'); ax.set_ylabel('Parallelism (×)'); ax.set_title(f'S1-derived Wasmd parallelism ({w} workers)'); ax.tick_params(axis='x',rotation=18); ax.legend(); save(fig,a.output_dir/'fig04b-s1-workload-parallelism.pdf'); plt.close(fig)
