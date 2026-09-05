#!/usr/bin/env python3
"""Plot one selected metric from a ConflictLab aggregate/plot-long.csv.

Use --filter key=value to select one publication slice from a multidimensional
matrix.  Keys may be bare parameter names or full ``param.<name>`` columns.
Use --series PARAM to split each scheduler mode by an additional parameter.
"""
import argparse
from pathlib import Path
import matplotlib.pyplot as plt
from common import read_csv,f,save

p=argparse.ArgumentParser()
p.add_argument('--csv',type=Path,required=True)
p.add_argument('--metric',required=True)
p.add_argument('--x',required=True)
p.add_argument('--series')
p.add_argument('--filter',action='append',default=[])
p.add_argument('--output',type=Path,required=True)
p.add_argument('--title',default='ConflictLab feature sweep')
p.add_argument('--ylabel',default='Value')
a=p.parse_args()


def col(name):
    return name if name.startswith('param.') else 'param.'+name


def parse_filter(item):
    if '=' not in item:
        raise SystemExit(f'--filter expects key=value, got {item!r}')
    k,v=item.split('=',1)
    return col(k),v

if a.csv.exists():
 rows=[r for r in read_csv(a.csv) if r.get('metric')==a.metric]
 for k,v in map(parse_filter,a.filter):
  rows=[r for r in rows if str(r.get(k,''))==v]
 groups={}
 for r in rows:
  label=r.get('mode','')
  if a.series:
   label += f" / {a.series}={r.get(col(a.series),'')}"
  groups.setdefault(label,[]).append((r.get(col(a.x),''),f(r,'mean'),f(r,'ci95_low'),f(r,'ci95_high')))
 if groups:
  fig,ax=plt.subplots(figsize=(6.4,4.0))
  for label,ps in groups.items():
   def key(row):
    try:return float(row[0])
    except (TypeError,ValueError):return str(row[0])
   ps=sorted(ps,key=key); xs=[q[0] for q in ps]; ys=[q[1] for q in ps]
   lo=[max(0.0,q[1]-q[2]) for q in ps]; hi=[max(0.0,q[3]-q[1]) for q in ps]
   ax.errorbar(xs,ys,yerr=[lo,hi] if any(lo+hi) else None,marker='o',capsize=3,label=label)
  ax.set_xlabel(a.x.replace('_',' ')); ax.set_ylabel(a.ylabel); ax.set_title(a.title); ax.legend(); ax.tick_params(axis='x',rotation=20); save(fig,a.output); plt.close(fig)
