#!/usr/bin/env python3
"""Materialize a canonical ConflictLab paper grid for smoke/debug/paper execution."""
import argparse, json
from pathlib import Path
p=argparse.ArgumentParser(); p.add_argument('src',type=Path); p.add_argument('dst',type=Path); p.add_argument('--workers',type=int,required=True); p.add_argument('--profile',choices=['smoke','debug','paper'],required=True)
a=p.parse_args(); a.src=a.src.resolve(); d=json.loads(a.src.read_text()); m=d['matrix']; seeds=m.get('seeds',[11,47,101,313,997])
policy=d.get('policy_file')
if policy:
    q=Path(policy)
    if not q.is_absolute(): q=(a.src.parent/q).resolve()
    if not q.is_file(): raise SystemExit(f'missing policy file: {q}')
    d['policy_file']=str(q)
if a.profile=='smoke': seeds=seeds[:1]
elif a.profile=='debug': seeds=seeds[:min(3,len(seeds))]
m['seeds']=seeds; m['workers']=[a.workers]; d['physical_core_limit']=max(a.workers,int(d.get('physical_core_limit',a.workers)))
a.dst.parent.mkdir(parents=True,exist_ok=True); a.dst.write_text(json.dumps(d,indent=2)+'\n')
