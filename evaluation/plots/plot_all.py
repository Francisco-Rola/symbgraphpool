#!/usr/bin/env python3
import os,subprocess,sys
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
result=Path(os.environ.get('PAPER_EVAL_RESULT_ROOT',ROOT/'benchmark-results/paper-eval'))
out=Path(os.environ.get('PAPER_EVAL_FIGURE_DIR',ROOT/'evaluation/figures'))
out.mkdir(parents=True,exist_ok=True)
HERE=Path(__file__).resolve().parent
for script in ['plot_headline.py','plot_consensus_overlap.py','plot_native.py','plot_upper_bound.py','plot_contention.py','plot_workload_parallelism.py','plot_s3_breakdown.py','plot_s3_oracle.py','plot_ablation.py','plot_block_size.py']:
 subprocess.run([sys.executable,str(HERE/script),'--result-root',str(result),'--output-dir',str(out)],check=True)

feature_jobs=[
 # Precision on the hard/full semantic case: granularity is the only x-axis variable.
 ('07-prediction/prediction-granularity/aggregate/plot-long.csv','prediction_precision','symbolic_granularity',None,
  ['contention=75pct','operation_mix=full'],'fig08a-symbolic-granularity.pdf','Symbolic granularity vs prediction precision','Prediction precision'),
 # Recovery from a one-block 10% hidden-key fault under high contention.
 ('07-prediction/prediction-recovery/aggregate/plot-long.csv','replayed_transactions','postchange_warmup_blocks',None,
  ['contention=75pct','prediction_fault_mode=hidden-key','prediction_fault_rate_bps=1000'],'fig08b-prediction-recovery.pdf','Recovery after hidden-key prediction fault','Replayed transactions'),
 # Low->hot regime shift, with bypass enabled, to expose control-plane recovery.
 ('08-adaptation/aggregate/plot-long.csv','replayed_transactions','postchange_warmup_blocks',None,
  ['contention=90pct','warmup_hot_account_probability_bps=1000','acg.serial_bypass_enabled=true'],'fig09-adaptation.pdf','Adaptation after low-to-hot workload transition','Replayed transactions'),
 # A representative difficult consensus-divergence slice.
 ('11-consensus/aggregate/plot-long.csv','throughput_speedup','consensus_cutoff_ms',None,
  ['prediction_quality=bucketed','consensus_divergence=tail-reorder-10pct','contention=75pct','complexity=mixed'],'fig10-consensus-cutoff.pdf','Consensus cutoff under candidate/decided divergence','Throughput speedup'),
 # Compare compact vs dense graph representation on the high-contention bucketed case.
 ('13-compaction/aggregate/plot-long.csv','planning_ms','transactions','acg.compact_equivalence_groups',
  ['prediction_quality=bucketed','contention=75pct'],'fig11-compaction.pdf','Planning cost with/without graph compaction','Planning time (ms)'),
]
for rel,metric,x,series,filters,name,title,ylabel in feature_jobs:
 src=result/rel
 if not src.exists(): continue
 cmd=[sys.executable,str(HERE/'plot_feature_grid.py'),'--csv',str(src),'--metric',metric,'--x',x,'--output',str(out/name),'--title',title,'--ylabel',ylabel]
 if series: cmd += ['--series',series]
 for flt in filters: cmd += ['--filter',flt]
 subprocess.run(cmd,check=True)
subprocess.run([sys.executable,str(HERE/'summarize_semantics.py'),'--result-root',str(result),'--output-dir',str(out)],check=True)
figs=sorted(p.name for p in out.iterdir() if p.is_file())
(out/'INDEX.txt').write_text('\n'.join(figs)+'\n',encoding='utf-8')
print(f'figures/tables: {out}')
