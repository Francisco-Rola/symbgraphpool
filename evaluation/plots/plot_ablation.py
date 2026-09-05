#!/usr/bin/env python3
import argparse
from pathlib import Path
import matplotlib.pyplot as plt
from common import read_csv,f,save
p=argparse.ArgumentParser(); p.add_argument('--result-root',type=Path,required=True); p.add_argument('--output-dir',type=Path,required=True); a=p.parse_args(); src=a.result_root/'09-s3-ablation/summary/ablation.csv'
if src.exists():
 rows=read_csv(src); workers=max((int(float(r['workers'])) for r in rows),default=0); rs=[r for r in rows if int(float(r['workers']))==workers]; order=['legacy','mvcc','mvcc-indexed','optimized']; rs=sorted(rs,key=lambda r:order.index(r['variant']) if r['variant'] in order else 99)
 fig,ax=plt.subplots(figsize=(6.4,4.0)); ax.bar([r['variant'] for r in rs],[f(r,'active_ms') for r in rs]); ax.set_ylabel('Active scheduler time (ms)'); ax.set_title(f'Rust-ACG implementation ablation, S3, {workers} workers'); ax.tick_params(axis='x',rotation=15); save(fig,a.output_dir/'fig06-acg-implementation-ablation.pdf'); plt.close(fig)
