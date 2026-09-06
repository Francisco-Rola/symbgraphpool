import json, subprocess, sys, tempfile, unittest
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
class PaperEvaluationArtifactTests(unittest.TestCase):
    def test_canonical_experiment_surface_exists(self):
        expected=[f'{i:02d}_' for i in range(1,15)]
        names=[p.name for p in (ROOT/'evaluation/experiments').glob('*.sh')]
        for prefix in expected:
            self.assertTrue(any(n.startswith(prefix) for n in names), prefix)
        self.assertTrue((ROOT/'evaluation/experiments/run-all.sh').exists())
        self.assertTrue((ROOT/'evaluation/PAPER_PLAN.md').exists())
    def test_conflictlab_generator_makes_independent_key_dataset(self):
        with tempfile.TemporaryDirectory() as td:
            out=Path(td)/'w'
            subprocess.run([sys.executable,str(ROOT/'evaluation/workloads/generate_conflictlab.py'),'--output-dir',str(out),'--blocks','1','--transactions','8','--lanes','8','--work-iterations','16'],check=True)
            row=json.loads((out/'execution-plan.jsonl').read_text().splitlines()[0])
            accounts=[tx['calls'][0]['msg']['credit']['account'] for tx in row['transactions']]
            self.assertEqual(len(accounts),len(set(accounts)))
            self.assertEqual(json.loads((out/'workload.json').read_text())['conflict_free'],True)
    def test_s1_headline_is_wasmd_only_and_does_not_require_evm_reproduction(self):
        text=(ROOT/'evaluation/experiments/01_s1_headline.sh').read_text()
        self.assertNotIn('--source-corpus', text)
        plan=(ROOT/'evaluation/PAPER_PLAN.md').read_text()
        self.assertIn('does not claim EVM execution equivalence', plan)

    def test_only_canonical_conflictlab_grids_live_under_evaluation(self):
        old=ROOT/'evaluation/conflictlab'
        self.assertFalse(old.exists())
        grids=sorted((ROOT/'evaluation/grids/conflictlab').glob('*.grid.json'))
        self.assertGreaterEqual(len(grids), 6)
        self.assertTrue((ROOT/'tools/tests/fixtures/conflictlab/conflictlab-strategy-smoke.grid.json').exists())

    def test_s3_exact_breakdown_preserves_known_two_trace_allowance(self):
        text=(ROOT/'evaluation/experiments/03_s3_breakdown.sh').read_text()
        self.assertIn('--exact-oracle 1', text)
        self.assertIn('--allowed-missing-source 2', text)
        self.assertIn('--stream-plan 0', text)
        wrapper=(ROOT/'evaluation/lib/run_wasmd_dataset.sh').read_text()
        self.assertIn('--allowed-missing-source', wrapper)
        self.assertIn('ALLOWED_MISSING_SOURCE=0', wrapper)
        self.assertIn('--stream-plan auto|0|1', wrapper)
        self.assertIn('logical_addresses', wrapper)

    def test_s4_pipeline_is_under_tools_vegeta_and_fail_closed(self):
        text=(ROOT/'evaluation/experiments/02_s4_headline.sh').read_text()
        self.assertIn('PAPER_EVAL_REQUIRE_S4',text)
        self.assertIn('native-execution',text)
        for name in [
            'run-vegeta-s4-collect.sh',
            'run-vegeta-s4-native-inputs.sh',
            'run-vegeta-s4-collect-all.sh',
            'run-vegeta-s4-characterize.sh',
            'run-vegeta-s4-prepare-native.sh',
        ]:
            self.assertTrue((ROOT/'tools/vegeta'/name).exists(), name)
            self.assertFalse((ROOT/'tools/legacy-scripts'/name).exists(), name)
        prep=(ROOT/'evaluation/workloads/prepare_s4.sh').read_text()
        self.assertIn('run-vegeta-s4-characterize.sh', prep)
        self.assertIn('run-vegeta-s4-prepare-native.sh', prep)
        self.assertIn('s4-native-family-map.v1.json', prep)
    def test_consensus_overlap_reporting_is_canonical(self):
        plan=(ROOT/'evaluation/PAPER_PLAN.md').read_text()
        readme=(ROOT/'evaluation/README.md').read_text()
        self.assertIn('tail(C) = R + max(0, P - C)', plan)
        self.assertIn('commit(C) = max(C, P) + R', plan)
        self.assertIn('PAPER_EVAL_CONSENSUS_WINDOW_MS', readme)
        self.assertIn('14_consensus_window_sensitivity.sh', readme)
        self.assertIn('PAPER_EVAL_CONSENSUS_SWEEP_MS', readme)
        common=(ROOT/'evaluation/lib/common.sh').read_text()
        self.assertIn('PAPER_EVAL_CONSENSUS_WINDOW_MS:-300', common)
        sensitivity=(ROOT/'evaluation/experiments/14_consensus_window_sensitivity.sh').read_text()
        self.assertIn('PAPER_EVAL_CONSENSUS_SWEEP_MS', sensitivity)
        self.assertTrue((ROOT/'evaluation/plots/plot_consensus_overlap.py').exists())
        runner=(ROOT/'evaluation/lib/run_wasmd_campaign.sh').read_text()
        self.assertIn('--consensus-windows-ms', runner)
        self.assertIn('--cost-metric', runner)

    def test_publication_docs_do_not_point_to_deleted_eval_wrappers(self):
        for rel in ['README.md','VALIDATION.md','evaluation/README.md','evaluation/wasmd/README.md']:
            text=(ROOT/rel).read_text()
            self.assertNotIn('scripts/eval-wasmd',text)
if __name__=='__main__': unittest.main()
