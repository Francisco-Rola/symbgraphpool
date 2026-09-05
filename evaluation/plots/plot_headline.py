#!/usr/bin/env python3
import argparse
from pathlib import Path
import matplotlib.pyplot as plt
from common import read_csv,errorbar_series,save,f

p=argparse.ArgumentParser(); p.add_argument('--result-root',type=Path,required=True); p.add_argument('--output-dir',type=Path,required=True); a=p.parse_args()
for dataset,sub in [('S1-derived Wasmd','01-s1'),('S4-derived Wasmd','02-s4')]:
    summary_dir=a.result_root/sub/'summary'
    src=summary_dir/'summary.csv'
    if not src.exists(): continue
    rows=read_csv(src)
    slug='s1' if dataset.startswith('S1') else 's4'

    fig,ax=plt.subplots(figsize=(6.4,4.0))
    for label in ['BlockSTM','AriaFB','Vegeta','Rust-ACG']:
        errorbar_series(ax,[r for r in rows if r.get('label')==label],xkey='workers',ykey='throughput_speedup',label=label)
    ax.axhline(1.0,linewidth=0.8); ax.set_xlabel('Workers'); ax.set_ylabel('Replay speedup vs Serial'); ax.set_title(f'{dataset}: consensus-visible replay'); ax.legend()
    save(fig,a.output_dir/f'fig01-{slug}-replay-scaling.pdf'); plt.close(fig)

    sweep=summary_dir/'consensus-sweep.csv'
    if not sweep.exists(): continue
    overlap=read_csv(sweep)
    windows=sorted({f(r,'consensus_window_ms') for r in overlap})
    if len(windows) != 1:
        # Multi-window curves belong only to experiment 14.
        continue
    c=windows[0]
    for metric,ylabel,suffix,title_suffix in [
        ('overlap_tail_x',f'Overlap-aware tail speedup @ {c:g} ms','tail-scaling','overlap-aware execution tail'),
        ('commit_x',f'Proposal-to-commit speedup @ {c:g} ms','commit-scaling','modeled proposal-to-commit'),
    ]:
        fig,ax=plt.subplots(figsize=(6.4,4.0))
        for label in ['BlockSTM','AriaFB','Vegeta','Rust-ACG']:
            errorbar_series(ax,[r for r in overlap if r.get('label')==label],xkey='workers',ykey=metric,label=label)
        ax.axhline(1.0,linewidth=0.8); ax.set_xlabel('Workers'); ax.set_ylabel(ylabel); ax.set_title(f'{dataset}: {title_suffix}'); ax.legend()
        save(fig,a.output_dir/f'fig01-{slug}-{suffix}.pdf'); plt.close(fig)

    acg=[r for r in overlap if r.get('label')=='Rust-ACG']
    if acg:
        fig,ax=plt.subplots(figsize=(6.4,3.8))
        errorbar_series(ax,acg,xkey='workers',ykey='pre_coverage_pct',label='Rust-ACG')
        ax.set_ylim(0,105); ax.set_xlabel('Workers'); ax.set_ylabel(f'Blocks with P ≤ {c:g} ms (%)'); ax.set_title(f'{dataset}: pre-consensus completion coverage')
        save(fig,a.output_dir/f'fig01-{slug}-coverage.pdf'); plt.close(fig)
