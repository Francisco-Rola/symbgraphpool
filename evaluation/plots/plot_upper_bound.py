#!/usr/bin/env python3
import argparse
from pathlib import Path
import matplotlib.pyplot as plt
from common import read_json,save
p=argparse.ArgumentParser(); p.add_argument('--result-root',type=Path,required=True); p.add_argument('--output-dir',type=Path,required=True); a=p.parse_args()
src=a.result_root/'05-upper-bound/upper-bound-report.json'
if src.exists():
 d=read_json(src); rows=d['rows']; fig,ax=plt.subplots(figsize=(6.4,4.0))
 workers=sorted({int(r['workers']) for r in rows}); ax.plot(workers,workers,linestyle='--',label='Ideal')
 for label in ['BlockSTM','AriaFB','Vegeta']:
  rs=sorted((r for r in rows if r['label']==label),key=lambda r:r['workers']); errs=[r.get('post_scale_vs_1w_ci95',0.0) for r in rs]; ax.errorbar([r['workers'] for r in rs],[r['post_scale_vs_1w'] for r in rs],yerr=errs if any(errs) else None,marker='o',capsize=3,label=label)
 acg=sorted((r for r in rows if r['label']=='Rust-ACG'),key=lambda r:r['workers']); errs=[r.get('acg_preexec_scale_vs_1w_ci95',0.0) for r in acg]; ax.errorbar([r['workers'] for r in acg],[r['acg_preexec_scale_vs_1w'] for r in acg],yerr=errs if any(errs) else None,marker='o',capsize=3,label='Rust-ACG preexec')
 ax.set_xlabel('Workers'); ax.set_ylabel('Scaling vs own 1-worker run'); ax.set_title('Conflict-free native CosmWasm upper bound'); ax.legend(); save(fig,a.output_dir/'fig03-upper-bound.pdf'); plt.close(fig)
