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
    def test_vegeta_paper_replay_throughput_presentation(self):
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
            subprocess.run([sys.executable, str(SUMMARIZER), "--records", str(records), "--output-dir", str(out), "--consensus-windows-ms", "0,0.00002,0.00004"], check=True)
            obj = json.loads((out / "summary.json").read_text())
            self.assertIn("consensus_overlap_model", obj)
            self.assertEqual(obj["consensus_overlap_model"]["windows_ms"], [0.0, 2e-05, 4e-05])
            by = {(r["strategy"], r["workers"]): r for r in obj["rows"]}
            serial = by[("cosmos-wasmd-direct-serial", 4)]
            oracle = by[("cosmos-wasmd-symbgraph-rust-exact-trace-oracle", 4)]
            vegeta = by[("cosmos-wasmd-vegeta", 4)]
            acg = by[("cosmos-wasmd-symbgraph-rust", 4)]
            # Primary throughput follows Vegeta Figure 10: tx / replay (post) time.
            # Serial replay is 200 ns; Vegeta is 20 ns and ACG is 10 ns.
            self.assertAlmostEqual(vegeta["throughput_speedup"], 10.0)
            self.assertAlmostEqual(acg["throughput_speedup"], 20.0)
            sweep = {
                (r["strategy"], r["consensus_window_ms"]): r
                for r in obj["consensus_sweep"] if r["workers"] == 4
            }
            # C=0 charges all prework: Vegeta tail=(30+10)+(40+10)=90ns, ACG tail=50ns.
            self.assertAlmostEqual(sweep[("cosmos-wasmd-vegeta", 0.0)]["overlap_tail_x"], 200/90)
            self.assertAlmostEqual(sweep[("cosmos-wasmd-symbgraph-rust", 0.0)]["overlap_tail_x"], 4.0)
            # At C=20ns, ACG prework fits exactly while Vegeta still overruns.
            self.assertAlmostEqual(sweep[("cosmos-wasmd-symbgraph-rust", 2e-05)]["pre_coverage_pct"], 100.0)
            self.assertAlmostEqual(sweep[("cosmos-wasmd-vegeta", 2e-05)]["pre_coverage_pct"], 0.0)
            self.assertAlmostEqual(sweep[("cosmos-wasmd-symbgraph-rust", 2e-05)]["commit_x"], 4.8)
            self.assertTrue((out / "consensus-sweep.csv").is_file())
            self.assertTrue((out / "consensus-acg-vs-best.csv").is_file())
            self.assertTrue((out / "consensus-optimal-workers.csv").is_file())
            for row in (serial, vegeta, acg):
                self.assertNotIn("consensus_model_speedup", row)
                self.assertNotIn("consensus_model_tps", row)
                self.assertNotIn("post_x", row)
                self.assertNotIn("historical_wall_x", row)
                self.assertNotIn("validation_ms", row)
                self.assertNotIn("replay_pct", row)
                self.assertNotIn("replay_execution_ms", row)
                self.assertIn("reexec_pct", row)
                self.assertIn("reexec_ms", row)
            self.assertAlmostEqual(oracle["structural_parallelism"], 5.0)
            self.assertAlmostEqual(acg["structural_parallelism"], 2.5)
            self.assertEqual(oracle["source_trace_missing"], 1.0)
            summary_text = (out / "summary.txt").read_text()
            self.assertIn("Rust-ACG perfect-access headroom", summary_text)
            self.assertIn("reexec-%", summary_text)
            self.assertIn("reexec-ms", summary_text)
            self.assertNotIn("val-ms", summary_text)
            self.assertNotIn("matched-x", summary_text)
            self.assertNotIn("work-x", summary_text)
            self.assertNotIn("model-x", summary_text)
            self.assertNotIn("Secondary fixed-consensus model window", summary_text)
            self.assertIn("Pre-consensus timing diagnostics", summary_text)
            self.assertIn("tail(C)=R+max(0,P-C)", summary_text)
            self.assertEqual(
                obj["throughput_definition"]["primary_all_strategies"],
                "transactions / sum(post_consensus_nanos)",
            )
            self.assertIn("Vegeta NSDI'25", obj["throughput_definition"]["primary_reference"])

            # Normal single-C summaries mirror the canonical overlap metrics into
            # the primary summary rows/CSV/text table instead of hiding them in
            # consensus-sweep.csv. Use 20 ns here so ACG is fully covered while
            # Vegeta still overruns, making the assertion nontrivial.
            fixed_out = td / "summary-fixed"
            subprocess.run([
                sys.executable, str(SUMMARIZER), "--records", str(records),
                "--output-dir", str(fixed_out), "--consensus-windows-ms", "0.00002",
            ], check=True)
            fixed_obj = json.loads((fixed_out / "summary.json").read_text())
            fixed_by = {(r["strategy"], r["workers"]): r for r in fixed_obj["rows"]}
            fixed_acg = fixed_by[("cosmos-wasmd-symbgraph-rust", 4)]
            fixed_vegeta = fixed_by[("cosmos-wasmd-vegeta", 4)]
            self.assertAlmostEqual(fixed_acg["consensus_window_ms"], 2e-05)
            self.assertAlmostEqual(fixed_acg["overlap_tail_x"], 20.0)
            self.assertAlmostEqual(fixed_acg["pre_coverage_pct"], 100.0)
            self.assertAlmostEqual(fixed_vegeta["pre_coverage_pct"], 0.0)
            with (fixed_out / "summary.csv").open(newline="", encoding="utf-8") as f:
                header = next(csv.reader(f))
            self.assertIn("overlap_tail_x", header)
            self.assertIn("commit_x", header)
            self.assertIn("pre_coverage_pct", header)
            with (fixed_out / "per-sample.csv").open(newline="", encoding="utf-8") as f:
                sample_header = next(csv.reader(f))
            self.assertIn("overlap_tail_x", sample_header)
            fixed_text = (fixed_out / "summary.txt").read_text()
            self.assertIn("tail-x", fixed_text)
            self.assertIn("commit-x", fixed_text)
            self.assertIn("cover-%", fixed_text)

    def test_post_accounting_preserves_zero_and_charges_legacy_canonical_fallback(self):
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
                row = {
                    "strategy": strategy,
                    "workers": 2,
                    "sample": 0,
                    "block_number": 1,
                    "transactions": 10,
                    "matched_serial_nanos": 100,
                    "strategy_total_nanos": 100,
                    "serial_equivalent": True,
                }
                if strategy == "cosmos-wasmd-direct-serial":
                    row["post_consensus_nanos"] = 100
                elif strategy == "cosmos-wasmd-block-stm":
                    row["post_consensus_nanos"] = 80
                elif strategy == "cosmos-wasmd-aria-fb":
                    # Legacy record: intrinsic post excludes the required historical replay.
                    row.update({
                        "post_consensus_nanos": 40,
                        "aria_historical_fallback_nanos": 60,
                        "aria_historical_fallback_transactions": 10,
                    })
                elif strategy == "cosmos-wasmd-vegeta":
                    # Legacy zero was omitted by Go's `omitempty`; it must remain zero before
                    # charging the required historical replay rather than becoming total wall.
                    row.update({
                        "pre_consensus_nanos": 30,
                        "vegeta_historical_fallback_nanos": 70,
                        "vegeta_historical_fallback_transactions": 10,
                    })
                else:
                    row.update({
                        "pre_consensus_nanos": 25,
                        "post_consensus_nanos": 0,
                        "strategy_total_nanos": 25,
                    })
                rows.append(row)
            records.write_text("".join(json.dumps(row) + "\n" for row in rows))
            out = td / "out"
            subprocess.run([
                sys.executable, str(SUMMARIZER), "--records", str(records),
                "--output-dir", str(out), "--no-exact-oracle",
            ], check=True)
            obj = json.loads((out / "summary.json").read_text())
            by = {row["strategy"]: row for row in obj["rows"]}
            self.assertEqual(by["cosmos-wasmd-aria-fb"]["post_ms"], 0.0001)
            self.assertEqual(by["cosmos-wasmd-vegeta"]["post_ms"], 0.00007)
            self.assertEqual(by["cosmos-wasmd-symbgraph-rust"]["post_ms"], 0.0)

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
            self.assertIn("consensus_overlap_model", obj)
            self.assertEqual(len(obj["rows"]), 3)
            text = (out / "summary.txt").read_text()
            self.assertIn("Rust-ACG perfect-access headroom", text)
            self.assertNotIn("AriaFB ports", text)
            self.assertNotIn("Vegeta ports", text)


    def test_s1_parallelism_diagnostic_compares_source_and_translated_bounds(self):
        strategies = [
            "cosmos-wasmd-direct-serial", "cosmos-wasmd-block-stm",
            "cosmos-wasmd-aria-fb", "cosmos-wasmd-vegeta", "cosmos-wasmd-symbgraph-rust",
        ]
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            records = td / "records.jsonl"
            corpus = td / "corpus.jsonl"
            block_numbers = [16_774_645, 16_774_646]
            source_blocks = [
                {
                    "block_number": block_numbers[0],
                    "transactions": [
                        {"reads": [], "writes": ["evm/a/k"], "gas_used": 10},
                        {"reads": ["evm/a/k"], "writes": [], "gas_used": 20},
                        {"reads": [], "writes": ["evm/b/k"], "gas_used": 30},
                    ],
                },
                {
                    "block_number": block_numbers[1],
                    "transactions": [
                        {"reads": [], "writes": ["evm/c/k"], "gas_used": 10},
                        {"reads": [], "writes": ["evm/d/k"], "gas_used": 10},
                    ],
                },
            ]
            corpus.write_text("".join(json.dumps(r) + "\n" for r in source_blocks))
            rows = []
            for block_idx, block_number in enumerate(block_numbers):
                txs = 3 if block_idx == 0 else 2
                for strategy in strategies:
                    row = {
                        "strategy": strategy, "workers": 2, "sample": 0,
                        "block_number": block_number, "transactions": txs,
                        "matched_serial_nanos": 100, "strategy_total_nanos": 100,
                        "post_consensus_nanos": 100, "serial_equivalent": True,
                    }
                    if strategy == "cosmos-wasmd-vegeta":
                        if block_idx == 0:
                            row.update({
                                "vegeta_longest_chain": 3,
                                "vegeta_weighted_longest_chain_cost": 50,
                                "vegeta_total_estimated_cost": 60,
                                "vegeta_hot_key_worker_lower_bound_cost": 50,
                                "vegeta_ready_worker_lower_bound_cost": 50,
                                "vegeta_post_exec_work_nanos": 150,
                                "vegeta_post_exec_span_nanos": 100,
                                "vegeta_post_wide_exec_work_nanos": 140,
                                "vegeta_post_wide_exec_span_nanos": 90,
                                "vegeta_post_batches": 2,
                                "vegeta_post_singleton_batches": 1,
                                "vegeta_post_max_batch": 2,
                            })
                        else:
                            row.update({
                                "vegeta_longest_chain": 1,
                                "vegeta_weighted_longest_chain_cost": 10,
                                "vegeta_total_estimated_cost": 20,
                                "vegeta_hot_key_worker_lower_bound_cost": 10,
                                "vegeta_ready_worker_lower_bound_cost": 10,
                                "vegeta_post_exec_work_nanos": 20,
                                "vegeta_post_exec_span_nanos": 10,
                                "vegeta_post_wide_exec_work_nanos": 20,
                                "vegeta_post_wide_exec_span_nanos": 10,
                                "vegeta_post_batches": 1,
                                "vegeta_post_singleton_batches": 0,
                                "vegeta_post_max_batch": 2,
                            })
                    rows.append(row)
            records.write_text("".join(json.dumps(r) + "\n" for r in rows))
            out = td / "out"
            subprocess.run([
                sys.executable, str(SUMMARIZER), "--records", str(records),
                "--output-dir", str(out), "--no-exact-oracle",
                "--source-corpus", str(corpus), "--vegeta-dataset-tag", "S1", "--cost-metric", "gas_used",
            ], check=True)
            obj = json.loads((out / "summary.json").read_text())
            diag = obj["workload_parallelism"]
            self.assertAlmostEqual(diag["paper_full_dataset"]["hot_key_chain_ratio"], 8.39)
            source = diag["source_prefix_by_workers"][0]
            translated = diag["translated_wasmd_by_workers"][0]
            self.assertAlmostEqual(source["hot_key_chain_ratio"], 5 / 3)
            self.assertAlmostEqual(source["conflict_dag_parallelism"], 5 / 3)
            self.assertAlmostEqual(source["weighted_hot_key_parallelism"], 2.0)
            self.assertAlmostEqual(source["hot_key_ideal_worker_speedup"], 2.0)
            self.assertAlmostEqual(source["weighted_dag_parallelism"], 2.0)
            self.assertAlmostEqual(source["conflict_dag_ideal_worker_speedup"], 2.0)
            self.assertAlmostEqual(translated["hot_key_chain_ratio"], 1.25)
            self.assertAlmostEqual(translated["weighted_hot_key_parallelism"], 80 / 60)
            self.assertAlmostEqual(translated["hot_key_ideal_worker_speedup"], 80 / 60)
            self.assertAlmostEqual(translated["ready_wave_ideal_worker_speedup"], 80 / 60)
            text = (out / "summary.txt").read_text()
            self.assertIn("Workload parallelism diagnostic", text)
            self.assertIn("Vegeta paper S1 full dataset: hot-key chain ratio=8.39x", text)
            self.assertIn("translated/source hot-key ratio=0.750x", text)
            self.assertIn("cost-weighted-hot-key", text)
            self.assertIn("Cost metric for weighted diagnostics: gas_used", text)
            self.assertNotIn("gas-weighted-hot-key", text)

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
        self.assertIn("classifyVegetaPointChange", runner)
        self.assertIn("classifyVegetaRangeChange", runner)
        self.assertIn("serializationOrderFromVegetaMatrix", runner)
        self.assertNotIn("dependencyBetween(actualTrackers[earlier], actualTrackers[later])", runner)
        self.assertIn("VegetaAlg3ValidationNanos", main)
        self.assertIn("VegetaWeightedLongestChainCost", main)
        self.assertIn("vegetaProposalOrderWithParallelismStats", runner)
        self.assertIn("AriaHistoricalFallbackNanos", main)
        self.assertIn("AriaHistoricalFallbackTransactions", main)
        self.assertIn("AriaCanonicalFallback", main)
        self.assertIn("VegetaHistoricalFallbackNanos", main)
        self.assertNotIn("ariaStats.PostConsensusNanos += canonicalReplayNanos", main)
        self.assertNotIn("ariaStats.ReplayExecutionNanos += canonicalReplayNanos", main)
        self.assertNotIn("ariaStats.Reexecutions += uint64(len(block.Transactions))", main)
        self.assertIn('SerialReferenceScope: "historical-block-order"', main)
        self.assertIn('SerialReferenceScope: "vegeta-derived-serialization+historical-state-gate"', main)
        self.assertRegex(main, r'VegetaCanonicalFallback:\s+canonicalFallback')
        self.assertIn('vegetaSerialNanos = canonicalReplayNanos', main)


if __name__ == "__main__":
    unittest.main()
