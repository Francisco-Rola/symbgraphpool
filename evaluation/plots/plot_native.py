#!/usr/bin/env python3
import argparse
from pathlib import Path
import matplotlib.pyplot as plt
from common import read_csv,errorbar_series,save
p=argparse.ArgumentParser(); p.add_argument('--result-root',type=Path,required=True); p.add_argument('--output-dir',type=Path,required=True); a=p.parse_args()
base=a.result_root/'04-native'; names=['miniwarehouse-hot0','miniwarehouse-hot9000','native-mix']
for name in names:
 src=base/name/'summary/summary.csv'
 if not src.exists(): continue
 rows=read_csv(src); fig,ax=plt.subplots(figsize=(6.4,4.0))
 for label in ['BlockSTM','AriaFB','Vegeta','Rust-ACG']:
  errorbar_series(ax,[r for r in rows if r.get('label')==label],xkey='workers',ykey='throughput_speedup',label=label)
 ax.axhline(1.0,linewidth=0.8); ax.set_xlabel('Workers'); ax.set_ylabel('Replay speedup vs Serial'); ax.set_title(name.replace('-',' ')); ax.legend()
 save(fig,a.output_dir/f'fig02-native-{name}.pdf'); plt.close(fig)
