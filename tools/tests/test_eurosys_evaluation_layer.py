import csv,json,subprocess,sys,tempfile,unittest
from pathlib import Path

ROOT=Path(__file__).resolve().parents[2]

class EuroSysEvaluationLayerTests(unittest.TestCase):
    def run_py(self, rel, *args):
        return subprocess.run([sys.executable,str(ROOT/rel),*map(str,args)],cwd=ROOT,check=True,text=True,stdout=subprocess.PIPE,stderr=subprocess.PIPE)

    def test_resource_usage_parser_handles_elapsed_colons(self):
        with tempfile.TemporaryDirectory() as td:
            td=Path(td); raw=td/'raw'; out=td/'out'; raw.mkdir()
            p=raw/'records-w6-symbgraph-rust.jsonl.resource.txt'
            p.write_text('''\n\tUser time (seconds): 12.50\n\tSystem time (seconds): 1.25\n\tElapsed (wall clock) time (h:mm:ss or m:ss): 1:02.50\n\tMaximum resident set size (kbytes): 1048576\n\tMajor (requiring I/O) page faults: 2\n\tMinor (reclaiming a frame) page faults: 3\n\tFile system inputs: 4\n\tFile system outputs: 5\n''')
            self.run_py('evaluation/eurosys/summarize_resource_usage.py','--raw-dir',raw,'--output-dir',out)
            with (out/'resource-usage.csv').open() as fh: row=next(csv.DictReader(fh))
            self.assertEqual(row['strategy'],'cosmos-wasmd-symbgraph-rust')
            self.assertAlmostEqual(float(row['elapsed_seconds']),62.5)
            self.assertEqual(float(row['max_rss_kib']),1048576)

    def test_headline_summary_derives_economics_and_win_rate(self):
        with tempfile.TemporaryDirectory() as td:
            td=Path(td); records=td/'records.jsonl'; out=td/'out'; rows=[]
            strategies=[('cosmos-wasmd-direct-serial',0,0),('cosmos-wasmd-block-stm',0,0),('cosmos-wasmd-symbgraph-rust',2,2)]
            for sample in (0,1):
                for block in (10,11):
                    for strategy,spec,reuse in strategies:
                        post=10_000_000 if strategy=='cosmos-wasmd-direct-serial' else (8_000_000 if strategy=='cosmos-wasmd-block-stm' else 2_000_000)
                        pre=4_000_000 if strategy=='cosmos-wasmd-symbgraph-rust' else 0
                        rows.append({'strategy':strategy,'workers':2,'sample':sample,'block_number':block,'transactions':2,'post_consensus_nanos':post,'pre_consensus_nanos':pre,'strategy_total_nanos':post+pre,'execution_attempts':2+(1 if strategy=='cosmos-wasmd-symbgraph-rust' else 0),'reexecutions':0,'speculated_transactions':spec,'reused_transactions':reuse,'symb_worker_utilization':0.75 if strategy=='cosmos-wasmd-symbgraph-rust' else 0,'symb_worker_idle_nanos':100 if strategy=='cosmos-wasmd-symbgraph-rust' else 0})
            records.write_text(''.join(json.dumps(r)+'\n' for r in rows))
            self.run_py('evaluation/eurosys/summarize_headline_records.py','--dataset',f'S1={records}','--output-dir',out,'--consensus-window-ms','300')
            with (out/'economics-summary.csv').open() as fh: econ=list(csv.DictReader(fh))
            acg=next(r for r in econ if r['label']=='Rust-ACG')
            self.assertAlmostEqual(float(acg['reuse_tx_pct']),100.0)
            self.assertAlmostEqual(float(acg['attempt_amplification']),1.5)
            with (out/'winloss-summary.csv').open() as fh: wins=next(csv.DictReader(fh))
            self.assertAlmostEqual(float(wins['win_pct']),100.0)

    def test_compute_sensitivity_summary_reads_scale_directories(self):
        with tempfile.TemporaryDirectory() as td:
            td=Path(td); root=td/'root'; out=td/'out'
            strategies=['cosmos-wasmd-direct-serial','cosmos-wasmd-block-stm','cosmos-wasmd-aria-fb','cosmos-wasmd-vegeta','cosmos-wasmd-symbgraph-rust']
            for scale in ('0','4'):
                d=root/f'scale-{scale}'/'summary'; d.mkdir(parents=True)
                with (d/'summary.csv').open('w',newline='') as f:
                    w=csv.DictWriter(f,fieldnames=['strategy','workers','throughput_tps','throughput_speedup','post_ms','overlap_tail_x','commit_x','reexec_pct']); w.writeheader()
                    for i,s in enumerate(strategies): w.writerow({'strategy':s,'workers':6,'throughput_tps':100+i,'throughput_speedup':1+i/10,'post_ms':100+float(scale)*10,'overlap_tail_x':1+i/20,'commit_x':1+i/30,'reexec_pct':0})
            self.run_py('evaluation/eurosys/summarize_compute_sensitivity.py','--root',root,'--dataset','S1','--scales','0,4','--workers','6','--output-dir',out)
            with (out/'compute-sensitivity.csv').open() as fh: rows=list(csv.DictReader(fh))
            self.assertEqual(len(rows),10)
            self.assertEqual({float(r['scale']) for r in rows},{0.0,4.0})

if __name__=='__main__': unittest.main()
