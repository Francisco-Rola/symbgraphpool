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
    def test_fixed_campaign_consensus_window_and_six_strategy_throughput(self):
        strategies = [
            "cosmos-wasmd-direct-serial",
            "cosmos-wasmd-symbgraph-rust-exact-trace-oracle",
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
                    elif strategy == "cosmos-wasmd-symbgraph-rust-exact-trace-oracle":
                        # Hindsight oracle pre-work must never inflate the campaign
                        # consensus window used to compare deployable systems.
                        pre = 100
                        post = 2
                        total = 102
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
                    row = {
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
                    }
                    if strategy == "cosmos-wasmd-symbgraph-rust-exact-trace-oracle":
                        row.update({
                            "oracle_source_trace_missing": 1 if block == 0 else 0,
                            "symb_dependency_edges": 3,
                            "symb_total_estimated_cost": 100,
                            "symb_critical_path_cost": 20,
                        })
                    elif strategy == "cosmos-wasmd-symbgraph-rust":
                        row.update({
                            "symb_dependency_edges": 5,
                            "symb_total_estimated_cost": 100,
                            "symb_critical_path_cost": 40,
                        })
                    rows.append(row)
            records.write_text("".join(json.dumps(r) + "\n" for r in rows))
            out = td / "summary"
            subprocess.run([sys.executable, str(SUMMARIZER), "--records", str(records), "--output-dir", str(out)], check=True)
            obj = json.loads((out / "summary.json").read_text())
            self.assertEqual(obj["consensus_window_nanos"], 40)
            by = {(r["strategy"], r["workers"]): r for r in obj["rows"]}
            serial = by[("cosmos-wasmd-direct-serial", 4)]
            oracle = by[("cosmos-wasmd-symbgraph-rust-exact-trace-oracle", 4)]
            vegeta = by[("cosmos-wasmd-vegeta", 4)]
            acg = by[("cosmos-wasmd-symbgraph-rust", 4)]
            # Every strategy is charged the same two 40 ns consensus windows.
            # Serial: 20 tx / (2*40 + 200) ns. Vegeta: 20 tx / (2*40 + 20) ns.
            self.assertAlmostEqual(vegeta["throughput_speedup"], 280 / 100)
            # ACG: 20 tx / (2*40 + 10) ns versus the same fixed-window serial baseline.
            self.assertAlmostEqual(acg["throughput_speedup"], 280 / 90)
            self.assertAlmostEqual(acg["post_x"], 20.0)
            self.assertAlmostEqual(serial["post_x"], 1.0)
            self.assertAlmostEqual(oracle["structural_parallelism"], 5.0)
            self.assertAlmostEqual(acg["structural_parallelism"], 2.5)
            self.assertEqual(oracle["source_trace_missing"], 1.0)
            self.assertIn("Rust-ACG perfect-access headroom", (out / "summary.txt").read_text())
            self.assertEqual(
                obj["throughput_definition"]["all_strategies"],
                "transactions / (blocks * consensus_window + sum(post_consensus))",
            )

    def test_summarizer_rejects_partial_strategy_campaign(self):
        strategies = [
            "cosmos-wasmd-direct-serial",
            "cosmos-wasmd-symbgraph-rust-exact-trace-oracle",
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
                            "cosmos-wasmd-vegeta", "cosmos-wasmd-symbgraph-rust",
                            "cosmos-wasmd-symbgraph-rust-exact-trace-oracle",
                        } else 0,
                        "post_consensus_nanos": 80 if strategy in {
                            "cosmos-wasmd-vegeta", "cosmos-wasmd-symbgraph-rust",
                            "cosmos-wasmd-symbgraph-rust-exact-trace-oracle",
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

    def test_no_exact_oracle_summary_accepts_five_system_campaign(self):
        strategies = [
            "cosmos-wasmd-direct-serial", "cosmos-wasmd-block-stm",
            "cosmos-wasmd-aria-fb", "cosmos-wasmd-vegeta", "cosmos-wasmd-symbgraph-rust",
        ]
        with tempfile.TemporaryDirectory() as td:
            td = Path(td); records = td / "records.jsonl"
            rows = []
            for strategy in strategies:
                for block in range(2):
                    pre = 25 if strategy in {"cosmos-wasmd-vegeta", "cosmos-wasmd-symbgraph-rust"} else 0
                    rows.append({
                        "strategy": strategy, "workers": 2, "sample": 0, "block_number": block,
                        "transactions": 10, "matched_serial_nanos": 100, "strategy_total_nanos": 100,
                        "pre_consensus_nanos": pre, "post_consensus_nanos": 75 if pre else 100,
                        "serial_equivalent": True,
                    })
            records.write_text("".join(json.dumps(r)+"\n" for r in rows))
            out = td / "out"
            subprocess.run([sys.executable, str(SUMMARIZER), "--records", str(records), "--output-dir", str(out), "--no-exact-oracle"], check=True)
            obj = json.loads((out / "summary.json").read_text())
            self.assertFalse(obj["exact_oracle_enabled"])
            self.assertEqual(len(obj["rows"]), 5)
            self.assertNotIn("ACG-Oracle", (out / "summary.txt").read_text())
            self.assertIn("no hindsight exact-access oracle", obj["throughput_definition"]["interpretation"])

    def test_rust_acg_only_summary_accepts_diagnostic_subset(self):
        strategies = [
            "cosmos-wasmd-direct-serial",
            "cosmos-wasmd-symbgraph-rust-exact-trace-oracle",
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
                    if strategy == "cosmos-wasmd-symbgraph-rust-exact-trace-oracle":
                        # The hindsight oracle must not set C even when its pre-phase is larger.
                        pre = 100
                        post = 2
                        total = 102
                    elif strategy == "cosmos-wasmd-symbgraph-rust":
                        pre = 20 + 5 * block
                        post = 5
                        total = pre + post
                    row = {
                        "strategy": strategy,
                        "workers": 2,
                        "sample": 0,
                        "block_number": block,
                        "transactions": 10,
                        "matched_serial_nanos": 100,
                        "strategy_total_nanos": total,
                        "pre_consensus_nanos": pre,
                        "post_consensus_nanos": post,
                        "serial_equivalent": True,
                        "reexecutions": 0,
                    }
                    if strategy == "cosmos-wasmd-symbgraph-rust-exact-trace-oracle":
                        row.update({
                            "oracle_translation_compensation_edges": 4,
                            "symb_dependency_edges": 3,
                            "symb_total_estimated_cost": 100,
                            "symb_critical_path_cost": 20,
                        })
                    elif strategy == "cosmos-wasmd-symbgraph-rust":
                        row.update({
                            "symb_dependency_edges": 5,
                            "symb_total_estimated_cost": 100,
                            "symb_critical_path_cost": 40,
                        })
                    rows.append(row)
            records.write_text("".join(json.dumps(r) + "\n" for r in rows))
            out = td / "summary"
            subprocess.run(
                [
                    sys.executable, str(SUMMARIZER),
                    "--records", str(records),
                    "--output-dir", str(out),
                    "--rust-acg-only",
                ],
                check=True,
            )
            obj = json.loads((out / "summary.json").read_text())
            self.assertEqual(obj["consensus_window_nanos"], 25)
            self.assertEqual(len(obj["rows"]), 3)
            self.assertIn("rust-only diagnostic campaign", obj["consensus_window_definition"])
            text = (out / "summary.txt").read_text()
            self.assertIn("Rust-ACG perfect-access headroom", text)
            self.assertNotIn("AriaFB ports", text)
            self.assertNotIn("Vegeta ports", text)

    def test_go_harness_contains_ariafb_same_wasmd_row(self):
        main = (ROOT / "benchmarks" / "cosmos-wasmd-blockstm-s3" / "main.go").read_text()
        runner = (ROOT / "benchmarks" / "cosmos-wasmd-blockstm-s3" / "policy_runner.go").read_text()
        self.assertIn('Strategy: "cosmos-wasmd-aria-fb"', main)
        oracle = (ROOT / "benchmarks" / "cosmos-wasmd-blockstm-s3" / "exact_trace_acg_oracle.go").read_text()
        rust_runner = (ROOT / "benchmarks" / "cosmos-wasmd-blockstm-s3" / "rust_symbgraph_runner.go").read_text()
        self.assertIn('Strategy: "cosmos-wasmd-symbgraph-rust-exact-trace-oracle"', main)
        self.assertNotIn("executeTrackedHistoricalOracle", main)
        self.assertNotIn("NewExactAccessOracleRunner", main)
        self.assertIn("buildExactTraceACGPlan", oracle)
        self.assertIn("NewRustSymbGraphExactTraceOracleRunner", rust_runner)
        self.assertIn("requireZeroReplay", rust_runner)
        self.assertIn("NewAriaFBRunner", main)
        self.assertIn("ariaRule2ForwardFallbacks", runner)
        self.assertIn("ariaDirectPredecessors", runner)
        self.assertIn("ariaFallbackEdges", runner)
        self.assertIn("vegetaProposalOrder", runner)
        self.assertIn("nextVegetaBatch", runner)
        self.assertIn("vegetaValidateBatch", runner)
        self.assertIn('SerialReferenceScope: "aria-derived-serialization"', main)
        self.assertIn('SerialReferenceScope: "vegeta-derived-serialization"', main)


if __name__ == "__main__":
    unittest.main()
