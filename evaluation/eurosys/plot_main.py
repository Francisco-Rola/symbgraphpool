#!/usr/bin/env python3
"""Generate the six consolidated EuroSys main-paper figures plus selected supplement plots."""
from __future__ import annotations
import argparse,csv,json,math
from pathlib import Path
import matplotlib.pyplot as plt

STRATEGIES=['Serial','BlockSTM','AriaFB','Vegeta','Rust-ACG']
NON_SERIAL=['BlockSTM','AriaFB','Vegeta','Rust-ACG']

def read_csv(path):
 if not Path(path).is_file(): return []
 with Path(path).open(newline='',encoding='utf-8') as f:return list(csv.DictReader(f))
def f(r,k,d=0.0):
 try:return float(r.get(k) or d)
 except:return d
def save(fig,path):
 path.parent.mkdir(parents=True,exist_ok=True); fig.tight_layout(); fig.savefig(path,bbox_inches='tight'); plt.close(fig)
def maxw(rows): return max((int(f(r,'workers')) for r in rows),default=0)
def selected(rows,label,w=None): return [r for r in rows if r.get('label')==label and (w is None or int(f(r,'workers'))==w)]
def line(ax,rows,x,y,label):
 rs=sorted(rows,key=lambda r:f(r,x));
 if not rs:return
 ys=[f(r,y) for r in rs]; err=[f(r,y+'_ci95') for r in rs]
 ax.errorbar([f(r,x) for r in rs],ys,yerr=err if any(err) else None,marker='o',capsize=2,label=label)
def metric_long(path,metric,filters):
 rows=[r for r in read_csv(path) if r.get('metric')==metric]
 for k,v in filters.items():
  col=k if k.startswith('param.') else 'param.'+k; rows=[r for r in rows if str(r.get(col,''))==str(v)]
 return rows

def main():
 ap=argparse.ArgumentParser(); ap.add_argument('--result-root',type=Path,required=True); ap.add_argument('--output-dir',type=Path,required=True); ap.add_argument('--strict',action='store_true'); a=ap.parse_args(); out=a.output_dir; root=a.result_root
 if a.strict:
  required=[root/'01-s1/summary/summary.csv',root/'02-s4/summary/summary.csv',root/'03-s3-breakdown/records.jsonl',root/'04-native/native-mix/summary/summary.csv',root/'05-upper-bound/records.jsonl',root/'07-prediction/prediction-granularity/aggregate/summary-wide.csv',root/'08-adaptation/aggregate/plot-long.csv',root/'10-block-size',root/'11-consensus/aggregate/plot-long.csv',root/'14-consensus-window-sensitivity/summary/consensus-sweep.csv',root/'eurosys-summary/economics-summary.csv']
  missing=[str(p) for p in required if not p.exists()]
  if missing: raise SystemExit('missing required EuroSys plot inputs:\n  '+'\n  '.join(missing))
 s1=read_csv(root/'01-s1/summary/summary.csv'); s4=read_csv(root/'02-s4/summary/summary.csv'); econ=read_csv(root/'eurosys-summary/economics-summary.csv'); blocks=read_csv(root/'eurosys-summary/block-metrics.csv')
 # Fig 1: real-workload headline at maximum worker count.
 fig,axs=plt.subplots(2,2,figsize=(10.2,7.2)); datasets=[('S1',s1),('S4',s4)]
 for ax,(name,rows),metric,title in [(axs[0,0],datasets[0],'throughput_speedup','S1 replay speedup'),(axs[0,1],datasets[1],'throughput_speedup','S4 replay speedup')]:
  w=maxw(rows); rs=[r for r in rows if int(f(r,'workers'))==w and r.get('label') in NON_SERIAL]; ax.bar([r['label'] for r in rs],[f(r,metric) for r in rs],yerr=[f(r,metric+'_ci95') for r in rs],capsize=2); ax.axhline(1,color='black',linewidth=.7); ax.set_title(f'{title} ({w} workers)'); ax.set_ylabel('× Serial'); ax.tick_params(axis='x',rotation=20)
 for ax,metric,title in [(axs[1,0],'overlap_tail_x','Overlap-aware tail speedup @ 300 ms'),(axs[1,1],'commit_x','Modeled proposal-to-commit speedup @ 300 ms')]:
  xs=[]; vals=[]; errs=[]
  for name,rows in datasets:
   w=maxw(rows); r=next((x for x in rows if x.get('label')=='Rust-ACG' and int(f(x,'workers'))==w),None)
   if r: xs.append(name); vals.append(f(r,metric)); errs.append(f(r,metric+'_ci95'))
  ax.bar(xs,vals,yerr=errs,capsize=2); ax.axhline(1,color='black',linewidth=.7); ax.set_ylabel('× Serial'); ax.set_title(title)
 save(fig,out/'fig01-real-workload-headline.pdf')
 # Fig 2: scaling + block-tail distributions.
 fig,axs=plt.subplots(2,2,figsize=(10.2,7.2))
 for ax,(name,rows) in zip(axs[0],datasets):
  for label in NON_SERIAL: line(ax,selected(rows,label),'workers','overlap_tail_x',label)
  ax.axhline(1,color='black',linewidth=.7); ax.set_xlabel('Workers'); ax.set_ylabel('Tail speedup × Serial'); ax.set_title(f'{name}: worker scaling'); ax.legend(fontsize=8)
 for ax,(name,rows) in zip(axs[1],datasets):
  w=maxw(rows); br=[r for r in blocks if r.get('dataset')==name and int(f(r,'workers'))==w and int(f(r,'sample'))==0]
  for label in NON_SERIAL:
   vals=sorted(f(r,'tail_ms') for r in br if r.get('label')==label)
   if vals: ax.plot(vals,[(i+1)/len(vals) for i in range(len(vals))],label=label)
  ax.set_xlabel('Per-block tail (ms)'); ax.set_ylabel('CDF'); ax.set_title(f'{name}: block-tail distribution ({w} workers)'); ax.legend(fontsize=8)
 save(fig,out/'fig02-scalability-and-tail-distribution.pdf')
 # Fig 3: native generality + controlled contention/ceiling.
 fig,axs=plt.subplots(2,2,figsize=(10.2,7.2)); native=root/'04-native'
 hot=[]
 for d in native.glob('miniwarehouse-hot*'):
  rows=read_csv(d/'summary/summary.csv'); w=maxw(rows)
  try:h=int(d.name.replace('miniwarehouse-hot',''))/100
  except:continue
  for lab in NON_SERIAL:
   r=next((x for x in rows if x.get('label')==lab and int(f(x,'workers'))==w),None)
   if r: hot.append((h,lab,f(r,'throughput_speedup')))
 for lab in NON_SERIAL:
  ps=sorted((h,v) for h,l,v in hot if l==lab)
  if ps: axs[0,0].plot([x for x,_ in ps],[y for _,y in ps],marker='o',label=lab)
 axs[0,0].axhline(1,color='black',linewidth=.7); axs[0,0].set_xlabel('Hot-warehouse probability (%)'); axs[0,0].set_ylabel('Replay speedup × Serial'); axs[0,0].set_title('MiniWarehouse contention sweep'); axs[0,0].legend(fontsize=8)
 nm=read_csv(native/'native-mix/summary/summary.csv')
 for lab in NON_SERIAL: line(axs[0,1],selected(nm,lab),'workers','throughput_speedup',lab)
 axs[0,1].axhline(1,color='black',linewidth=.7); axs[0,1].set_xlabel('Workers'); axs[0,1].set_ylabel('Replay speedup × Serial'); axs[0,1].set_title('Native CW20/CW721/AMM mix'); axs[0,1].legend(fontsize=8)
 pts={lab:[] for lab in NON_SERIAL}
 for d in (root/'06-contention').glob('lanes-*'):
  rows=read_csv(d/'summary/summary.csv'); w=maxw(rows); lanes=int(d.name.split('-')[-1])
  for lab in NON_SERIAL:
   r=next((x for x in rows if x.get('label')==lab and int(f(x,'workers'))==w),None)
   if r: pts[lab].append((lanes,f(r,'throughput_speedup')))
 for lab,ps in pts.items():
  ps=sorted(ps,reverse=True)
  if ps: axs[1,0].plot([x for x,_ in ps],[y for _,y in ps],marker='o',label=lab)
 axs[1,0].set_xscale('log',base=2); axs[1,0].invert_xaxis(); axs[1,0].axhline(1,color='black',linewidth=.7); axs[1,0].set_xlabel('Independent lanes (fewer = more contention)'); axs[1,0].set_ylabel('Replay speedup × Serial'); axs[1,0].set_title('Controlled contention'); axs[1,0].legend(fontsize=8)
 ub=read_csv(root/'05-upper-bound/summary/summary.csv')
 if not ub:
  try: ub=json.loads((root/'05-upper-bound/upper-bound-report.json').read_text()).get('rows',[])
  except: ub=[]
 for lab in NON_SERIAL:
  rs=[r for r in ub if r.get('label')==lab]; y='acg_preexec_scale_vs_1w' if lab=='Rust-ACG' else 'post_scale_vs_1w'; line(axs[1,1],rs,'workers',y,lab)
 axs[1,1].plot([1,max(1,maxw(ub))],[1,max(1,maxw(ub))],linestyle='--',linewidth=.7,label='Ideal'); axs[1,1].set_xlabel('Workers'); axs[1,1].set_ylabel('Scaling vs own 1-worker'); axs[1,1].set_title('Zero-conflict machine ceiling'); axs[1,1].legend(fontsize=8)
 save(fig,out/'fig03-generality-contention-ceiling.pdf')
 # Fig 4: cost/overhead.
 fig,axs=plt.subplots(2,2,figsize=(10.2,7.2)); rec=root/'03-s3-breakdown/records.jsonl'
 if rec.is_file():
  rr=[json.loads(x) for x in rec.read_text().splitlines() if x.strip()]; rr=[r for r in rr if r.get('strategy')=='cosmos-wasmd-symbgraph-rust']; w=max((int(r['workers']) for r in rr),default=0); rr=[r for r in rr if int(r['workers'])==w]
  phases=[('Plan','symb_plan_nanos'),('Preexec','symb_preexecution_nanos'),('Reconcile','symb_reconciliation_nanos'),('Validate','symb_validation_nanos'),('Replay','symb_replay_execution_nanos')]; axs[0,0].bar([x[0] for x in phases],[sum(int(r.get(k,0) or 0) for r in rr)/1e6 for _,k in phases]); axs[0,0].set_ylabel('Summed time (ms)'); axs[0,0].set_title(f'S3 ACG phase breakdown ({w} workers)'); axs[0,0].tick_params(axis='x',rotation=20)
 er=[]
 for name,_ in datasets:
  rows=[r for r in econ if r.get('dataset')==name and r.get('label')=='Rust-ACG']; w=maxw(rows); r=next((x for x in rows if int(f(x,'workers'))==w),None)
  if r: er.append((name,r))
 axs[0,1].bar([x[0] for x in er],[f(x[1],'local_elapsed_vs_serial') for x in er],yerr=[f(x[1],'local_elapsed_vs_serial_ci95') for x in er],capsize=2); axs[0,1].axhline(1,color='black',linewidth=.7); axs[0,1].set_ylabel('(P + R) elapsed / Serial R'); axs[0,1].set_title('Local elapsed-service cost')
 rss=[]
 for name,sub in [('S1','01-s1'),('S4','02-s4')]:
  rs=read_csv(root/sub/'summary/resource-usage.csv'); w=maxw(rs)
  for lab in ['Serial','Rust-ACG']:
   strat={'Serial':'cosmos-wasmd-direct-serial','Rust-ACG':'cosmos-wasmd-symbgraph-rust'}[lab]; r=next((x for x in rs if x.get('strategy')==strat and int(f(x,'workers'))==w),None)
   if r:rss.append((f'{name} {lab}',f(r,'max_rss_kib')/1024))
 if rss: axs[1,0].bar([x for x,_ in rss],[y for _,y in rss]); axs[1,0].set_ylabel('Peak RSS (MiB)'); axs[1,0].set_title('Isolated-process memory'); axs[1,0].tick_params(axis='x',rotation=20)
 bpts={lab:[] for lab in NON_SERIAL}
 for d in (root/'10-block-size').glob('tx-*'):
  rows=read_csv(d/'summary/summary.csv'); w=maxw(rows); tx=int(d.name.split('-')[-1])
  for lab in NON_SERIAL:
   r=next((x for x in rows if x.get('label')==lab and int(f(x,'workers'))==w),None)
   if r:bpts[lab].append((tx,f(r,'throughput_speedup')))
 for lab,ps in bpts.items():
  ps=sorted(ps)
  if ps:axs[1,1].plot([x for x,_ in ps],[y for _,y in ps],marker='o',label=lab)
 axs[1,1].set_xscale('log',base=2); axs[1,1].axhline(1,color='black',linewidth=.7); axs[1,1].set_xlabel('Transactions/block'); axs[1,1].set_ylabel('Replay speedup × Serial'); axs[1,1].set_title('Block-size break-even'); axs[1,1].legend(fontsize=8)
 save(fig,out/'fig04-cost-and-overheads.pdf')
 # Fig 5: prediction quality, oracle headroom, recovery/adaptation.
 fig,axs=plt.subplots(2,2,figsize=(10.2,7.2)); s3=read_csv(root/'03-s3-breakdown/summary/summary.csv'); w=maxw(s3); rs=[r for r in s3 if int(f(r,'workers'))==w and r.get('label') in {'Rust-ACG','ACG-Oracle'}]; axs[0,0].bar([r['label'] for r in rs],[f(r,'throughput_speedup') for r in rs],yerr=[f(r,'throughput_speedup_ci95') for r in rs],capsize=2); axs[0,0].set_ylabel('Replay speedup × Serial'); axs[0,0].set_title('Exact-access oracle headroom')
 wide=read_csv(root/'07-prediction/prediction-granularity/aggregate/summary-wide.csv'); pts=[]
 for r in wide:
  if r.get('param.contention')=='75pct' and r.get('param.operation_mix')=='full': pts.append((f(r,'prediction_precision.mean'),f(r,'throughput_speedup.mean'),r.get('param.symbolic_granularity',''),r.get('mode','')))
 for x,y,g,m in pts: axs[0,1].scatter([x],[y]); axs[0,1].annotate(f'{m}/{g}',(x,y),fontsize=7)
 axs[0,1].set_xlabel('Prediction precision'); axs[0,1].set_ylabel('Throughput speedup'); axs[0,1].set_title('Prediction precision/performance Pareto')
 recov=metric_long(root/'07-prediction/prediction-recovery/aggregate/plot-long.csv','replayed_transactions',{'contention':'75pct','prediction_fault_mode':'hidden-key','prediction_fault_rate_bps':'1000'})
 for mode in sorted({r.get('mode','') for r in recov}):
  q=sorted([r for r in recov if r.get('mode')==mode],key=lambda r:f(r,'param.postchange_warmup_blocks')); axs[1,0].plot([f(r,'param.postchange_warmup_blocks') for r in q],[f(r,'mean') for r in q],marker='o',label=mode)
 axs[1,0].set_xlabel('Blocks after hidden-key fault'); axs[1,0].set_ylabel('Replayed transactions'); axs[1,0].set_title('Prediction-fault recovery'); axs[1,0].legend(fontsize=8)
 adapt=metric_long(root/'08-adaptation/aggregate/plot-long.csv','replayed_transactions',{'contention':'90pct','warmup_hot_account_probability_bps':'1000','acg.serial_bypass_enabled':'true'})
 for mode in sorted({r.get('mode','') for r in adapt}):
  q=sorted([r for r in adapt if r.get('mode')==mode],key=lambda r:f(r,'param.postchange_warmup_blocks')); axs[1,1].plot([f(r,'param.postchange_warmup_blocks') for r in q],[f(r,'mean') for r in q],marker='o',label=mode)
 axs[1,1].set_xlabel('Blocks after regime change'); axs[1,1].set_ylabel('Replayed transactions'); axs[1,1].set_title('Low-to-hot adaptation'); axs[1,1].legend(fontsize=8)
 save(fig,out/'fig05-prediction-and-adaptation.pdf')
 # Fig 6: consensus-window and divergence robustness.
 fig,axs=plt.subplots(1,2,figsize=(10.2,4.0)); cs=read_csv(root/'14-consensus-window-sensitivity/summary/consensus-sweep.csv'); w=maxw(cs)
 for lab in NON_SERIAL: line(axs[0],selected(cs,lab,w),'consensus_window_ms','commit_x',lab)
 axs[0].axhline(1,color='black',linewidth=.7); axs[0].set_xlabel('Consensus window C (ms)'); axs[0].set_ylabel('Modeled commit speedup × Serial'); axs[0].set_title('Consensus-window sensitivity'); axs[0].legend(fontsize=8)
 div=metric_long(root/'11-consensus/aggregate/plot-long.csv','throughput_speedup',{'prediction_quality':'bucketed','contention':'75pct','complexity':'mixed'})
 for d in sorted({r.get('param.consensus_divergence','') for r in div}):
  q=sorted([r for r in div if r.get('param.consensus_divergence')==d],key=lambda r:f(r,'param.consensus_cutoff_ms')); axs[1].plot([f(r,'param.consensus_cutoff_ms') for r in q],[f(r,'mean') for r in q],marker='o',label=d)
 axs[1].axhline(1,color='black',linewidth=.7); axs[1].set_xlabel('Pre-consensus cutoff (ms)'); axs[1].set_ylabel('Throughput speedup'); axs[1].set_title('Candidate/decided divergence'); axs[1].legend(fontsize=7)
 save(fig,out/'fig06-consensus-robustness.pdf')
 # Supplementary plots: compute sensitivity and cold-start trajectories when available.
 comps=[]
 for ds in ['s1','s4']:
  comps += read_csv(root/f'15-compute-sensitivity/{ds}/compute-sensitivity.csv')
 if comps:
  fig,axs=plt.subplots(1,2,figsize=(10.0,3.8))
  for ax,ds in zip(axs,['S1','S4']):
   q=[r for r in comps if r.get('dataset')==ds]
   for lab in NON_SERIAL:
    z=sorted([r for r in q if r.get('label')==lab],key=lambda r:f(r,'scale'));
    if z: ax.plot([f(r,'scale') for r in z],[f(r,'replay_speedup') for r in z],marker='o',label=lab)
   ax.axhline(1,color='black',linewidth=.7); ax.set_xlabel('Compute scale'); ax.set_ylabel('Replay speedup × Serial'); ax.set_title(ds); ax.legend(fontsize=8)
  save(fig,out/'supp-compute-sensitivity.pdf')
 cold=read_csv(root/'eurosys-summary/cold-start.csv')
 if cold:
  fig,axs=plt.subplots(1,2,figsize=(10.0,3.8))
  for ax,ds in zip(axs,['S1','S4']):
   q=[r for r in cold if r.get('dataset')==ds]; ax.plot([f(r,'ordinal') for r in q],[f(r,'tail_speedup_vs_serial') for r in q]); ax.set_xlabel('Block ordinal'); ax.set_ylabel('Rolling tail speedup × Serial'); ax.set_title(f'{ds} cold-start/adaptation')
  save(fig,out/'supp-real-workload-cold-start.pdf')
 print(out)
if __name__=='__main__':main()
