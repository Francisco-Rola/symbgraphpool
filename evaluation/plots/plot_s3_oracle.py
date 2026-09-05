#!/usr/bin/env python3
import argparse
from pathlib import Path
import matplotlib.pyplot as plt
from common import read_csv,f,save
p=argparse.ArgumentParser(); p.add_argument('--result-root',type=Path,required=True); p.add_argument('--output-dir',type=Path,required=True); a=p.parse_args(); src=a.result_root/'03-s3-breakdown/summary/summary.csv'
if src.exists():
 rows=read_csv(src); workers=max((int(float(r['workers'])) for r in rows),default=0); rs=[r for r in rows if int(float(r['workers']))==workers and r.get('label') in {'Rust-ACG','ACG-Oracle'}]
 if rs:
  order={'Rust-ACG':0,'ACG-Oracle':1}; rs.sort(key=lambda r:order[r['label']]); fig,ax=plt.subplots(figsize=(5.5,3.8)); ys=[f(r,'throughput_speedup') for r in rs]; errs=[f(r,'throughput_speedup_ci95') for r in rs]; ax.bar([r['label'] for r in rs],ys,yerr=errs if any(errs) else None,capsize=3); ax.set_ylabel('Replay speedup vs Serial'); ax.set_title(f'S3 current ACG vs perfect-access oracle ({workers} workers)'); save(fig,a.output_dir/'fig05b-s3-oracle-headroom.pdf'); plt.close(fig)
