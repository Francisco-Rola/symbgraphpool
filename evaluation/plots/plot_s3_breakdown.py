#!/usr/bin/env python3
import argparse,json
from pathlib import Path
from collections import defaultdict
import matplotlib.pyplot as plt
from common import save
p=argparse.ArgumentParser(); p.add_argument('--result-root',type=Path,required=True); p.add_argument('--output-dir',type=Path,required=True); a=p.parse_args(); src=a.result_root/'03-s3-breakdown/records.jsonl'
if src.exists():
 rows=[json.loads(x) for x in src.read_text().splitlines() if x.strip()]; rows=[r for r in rows if r.get('strategy')=='cosmos-wasmd-symbgraph-rust']; workers=max((int(r['workers']) for r in rows),default=0); rows=[r for r in rows if int(r['workers'])==workers]
 phases=[('Plan','symb_plan_nanos'),('Preexecute','symb_preexecution_nanos'),('Reconcile','symb_reconciliation_nanos'),('Validate','symb_validation_nanos'),('Replay','symb_replay_execution_nanos')]; vals=[sum(int(r.get(k,0) or 0) for r in rows)/1e6 for _,k in phases]
 fig,ax=plt.subplots(figsize=(6.4,4.0)); ax.bar([n for n,_ in phases],vals); ax.set_ylabel('Summed time over S3 (ms)'); ax.set_title(f'Rust-ACG phase breakdown, {workers} workers'); ax.tick_params(axis='x',rotation=20); save(fig,a.output_dir/'fig05-s3-phase-breakdown.pdf'); plt.close(fig)
