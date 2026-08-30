import csv
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SUMMARIZER = ROOT / "evaluation" / "wasmd" / "summarize.py"


class WasmdPublicationEvalTests(unittest.TestCase):
    def test_fixed_campaign_consensus_window_and_five_strategy_throughput(self):
        strategies = [
            "cosmos-wasmd-direct-serial",
            "cosmos-wasmd-block-stm",
            "cosmos-wasmd-aria-fb",
            "cosmos-wasmd-vegeta",
            "cosmos-wasmd-symbgraph-rust",
        ]
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            records = td / "records.jsonl"
            rows = []
            for block in range(2):
                for strategy in strategies:
                    pre = 0
                    post = 100
                    total = 100
                    if strategy == "cosmos-wasmd-block-stm":
                        post = total = 50
                    elif strategy == "cosmos-wasmd-aria-fb":
                        post = total = 40
                    elif strategy == "cosmos-wasmd-vegeta":
                        pre = 30 + 10 * block
                        post = 10
                        total = pre + post
                    elif strategy == "cosmos-wasmd-symbgraph-rust":
                        pre = 20
                        post = 5
                        total = 25
                    rows.append({
                        "strategy": strategy,
                        "workers": 4,
                        "sample": 0,
                        "block_number": block,
                        "transactions": 10,
                        "matched_serial_nanos": 100,
                        "strategy_total_nanos": total,
                        "pre_consensus_nanos": pre,
                        "post_consensus_nanos": post,
                        "serial_equivalent": True,
                        "reexecutions": 0,
                    })
            records.write_text("".join(json.dumps(r) + "\n" for r in rows))
            out = td / "summary"
            subprocess.run([sys.executable, str(SUMMARIZER), "--records", str(records), "--output-dir", str(out)], check=True)
            obj = json.loads((out / "summary.json").read_text())
            self.assertEqual(obj["consensus_window_nanos"], 40)
            by = {(r["strategy"], r["workers"]): r for r in obj["rows"]}
            serial = by[("cosmos-wasmd-direct-serial", 4)]
            vegeta = by[("cosmos-wasmd-vegeta", 4)]
            acg = by[("cosmos-wasmd-symbgraph-rust", 4)]
            # Every strategy is charged the same two 40 ns consensus windows.
            # Serial: 20 tx / (2*40 + 200) ns. Vegeta: 20 tx / (2*40 + 20) ns.
            self.assertAlmostEqual(vegeta["throughput_speedup"], 280 / 100)
            # ACG: 20 tx / (2*40 + 10) ns versus the same fixed-window serial baseline.
            self.assertAlmostEqual(acg["throughput_speedup"], 280 / 90)
            self.assertAlmostEqual(acg["post_x"], 20.0)
            self.assertAlmostEqual(serial["post_x"], 1.0)
            self.assertEqual(
                obj["throughput_definition"]["all_strategies"],
                "transactions / (blocks * consensus_window + sum(post_consensus))",
            )

    def test_summarizer_rejects_partial_strategy_campaign(self):
        strategies = [
            "cosmos-wasmd-direct-serial",
            "cosmos-wasmd-block-stm",
            "cosmos-wasmd-aria-fb",
            "cosmos-wasmd-vegeta",
            "cosmos-wasmd-symbgraph-rust",
        ]
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            records = td / "records.jsonl"
            rows = []
            for strategy in strategies:
                for block in range(2):
                    if strategy == "cosmos-wasmd-aria-fb" and block == 1:
                        continue
                    rows.append({
                        "strategy": strategy,
                        "workers": 4,
                        "sample": 0,
                        "block_number": block,
                        "transactions": 10,
                        "matched_serial_nanos": 100,
                        "strategy_total_nanos": 100,
                        "pre_consensus_nanos": 20 if strategy in {
                            "cosmos-wasmd-vegeta", "cosmos-wasmd-symbgraph-rust"
                        } else 0,
                        "post_consensus_nanos": 80 if strategy in {
                            "cosmos-wasmd-vegeta", "cosmos-wasmd-symbgraph-rust"
                        } else 100,
                        "serial_equivalent": True,
                    })
            records.write_text("".join(json.dumps(r) + "\n" for r in rows))
            proc = subprocess.run(
                [sys.executable, str(SUMMARIZER), "--records", str(records), "--output-dir", str(td / "out")],
                text=True, capture_output=True,
            )
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("incomplete/mismatched Wasmd campaign", proc.stderr + proc.stdout)

    def test_go_harness_contains_ariafb_same_wasmd_row(self):
        main = (ROOT / "benchmarks" / "cosmos-wasmd-blockstm-s3" / "main.go").read_text()
        runner = (ROOT / "benchmarks" / "cosmos-wasmd-blockstm-s3" / "policy_runner.go").read_text()
        self.assertIn('Strategy: "cosmos-wasmd-aria-fb"', main)
        self.assertIn("NewAriaFBRunner", main)
        self.assertIn("ariaRule2ForwardFallbacks", runner)
        self.assertIn("commitSpeculationWithForcedReplay", runner)


if __name__ == "__main__":
    unittest.main()
