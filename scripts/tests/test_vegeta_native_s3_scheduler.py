from __future__ import annotations

import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def load_module(name: str, relative: str):
    path = ROOT / relative
    spec = importlib.util.spec_from_file_location(name, path)
    assert spec is not None and spec.loader is not None
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


freeze = load_module("vegeta_archetype_freeze", "scripts/vegeta/evaluate-native-s3-archetype-freeze.py")
summarizer = load_module("vegeta_scheduler_summary", "scripts/vegeta/summarize-native-s3-scheduler.py")
validator = load_module("vegeta_scheduler_validator", "scripts/vegeta/validate-native-s3-scheduler-results.py")


class ArchetypeFreezeTests(unittest.TestCase):
    def test_candidate_independent_freeze_wins_without_implementation_evidence(self):
        ready, rationale = freeze.freeze_decision(True, False)
        self.assertTrue(ready)
        self.assertIn("candidate-independent", rationale)

    def test_candidate_dependent_freeze_requires_real_implementations(self):
        ready, _ = freeze.freeze_decision(False, False)
        self.assertFalse(ready)
        ready, rationale = freeze.freeze_decision(False, True)
        self.assertTrue(ready)
        self.assertIn("implementation-closed", rationale)

    def test_historical_candidates_have_real_source_derived_implementations(self):
        manifest = json.loads(
            (ROOT / "evaluation/vegeta/s3-native-implementation-manifest.v1.json").read_text()
        )
        for family in freeze.CANDIDATES:
            evidence = freeze.implementation_evidence(manifest, family)
            self.assertTrue(evidence["passed"], evidence)
            self.assertIs(evidence["historical_trace_keys_used"], False)


class SchedulerSourcePolicyTests(unittest.TestCase):
    def test_static_scheduler_source_does_not_import_historical_concrete_keys(self):
        source = (ROOT / "runtime/crates/acg-vegeta-native-s3-executor/src/bin/acg-vegeta-native-s3-benchmark.rs").read_text()
        self.assertIn("wasm_instance_lifecycle: WasmInstanceLifecycle::Reuse", source)
        self.assertNotIn("wasm_instance_lifecycle: WasmInstanceLifecycle::Recycle", source)
        executor_source = (ROOT / "runtime/crates/acg-vegeta-native-s3-executor/src/main.rs").read_text()
        self.assertIn("wasm_instance_lifecycle:WasmInstanceLifecycle::Reuse", executor_source)
        self.assertNotIn("wasm_instance_lifecycle:WasmInstanceLifecycle::Recycle", executor_source)
        for forbidden in ("exact-sload", "native-accesses.jsonl", "actual_reads", "actual_writes", "evm/"):
            self.assertNotIn(forbidden, source)
        self.assertIn("evaluation-only matched-serial concrete-access oracle", source)
        self.assertIn("strictly-prior-block", source)

    def test_frozen_config_has_no_performance_acceptance_threshold(self):
        config = json.loads((ROOT / "evaluation/vegeta/s3-native-scheduler.v1.json").read_text())
        self.assertIn("No performance outcome threshold", config["threshold_policy"])
        self.assertEqual(config["strategies"], list(validator.STRATEGIES))


class SchedulerSummaryTests(unittest.TestCase):
    def test_summary_aggregates_samples_without_per_block_weighting_bug(self):
        rows = []
        for sample, total in [(0, 50), (1, 100), (2, 200)]:
            for block in [1, 2]:
                rows.append({
                    "workers": 6,
                    "strategy": "static",
                    "sample": sample,
                    "block_number": block,
                    "matched_serial_nanos": 100,
                    "strategy_total_nanos": total,
                    "matched_serial_speedup": 100 / total,
                    "post_consensus_nanos": total // 2,
                    "planning_nanos": 1,
                    "preexecution_nanos": 2,
                    "reconciliation_nanos": 3,
                    "feedback_nanos": 0,
                    "transactions": 10,
                    "replayed_transactions": 1,
                    "canonical_transactions": 0,
                    "reused_receipts": 9,
                    "prepared_receipts": 10,
                    "serial_bypassed": False,
                    "dependency_edges": 2,
                    "max_wave_width": 5,
                    "serial_equivalent": True,
                })
        report = summarizer.summarize(rows)
        self.assertEqual(len(report["strategies"]), 1)
        row = report["strategies"][0]
        # Per-sample aggregate speedups are 2, 1, 0.5; median is 1.
        self.assertAlmostEqual(row["aggregate_active_wall_speedup"], 1.0)
        self.assertTrue(row["all_serial_equivalent"])


def frozen_fixture_records() -> tuple[list[dict], dict]:
    config = json.loads((ROOT / "evaluation/vegeta/s3-native-scheduler.v1.json").read_text())
    blocks = list(range(config["block_start"], config["block_end"] + 1))
    # 101*136=13736, so add the remaining 47 tx to the first block.
    counts = [136] * len(blocks)
    counts[0] += config["transaction_count"] - sum(counts)
    rows = []
    for sample in range(config["samples"]):
        for strategy in config["strategies"]:
            for block, txs in zip(blocks, counts):
                planning_source = {
                    "serial": "none",
                    "aria-fb": "current-block concrete speculative accesses",
                    "vegeta": "current-block concrete discovery accesses",
                    "exact-access": "evaluation-only matched-serial concrete-access oracle",
                    "static": "checked-in source-derived symbolic profiles + public native call inputs",
                    "probability-only": "source-derived symbolic prior + strictly prior-block conflict feedback",
                    "cost-aware": "source-derived symbolic prior + strictly prior-block conflict/cost feedback",
                }[strategy]
                rows.append({
                    "schema_version": 1,
                    "dataset": "vegeta-s3-native-seven-strategy",
                    "sample": sample,
                    "block_number": block,
                    "strategy": strategy,
                    "workers": config["workers"],
                    "wasm_instance_lifecycle": "reuse",
                    "transactions": txs,
                    "semantic_calls": txs,
                    "skipped_actions": 0,
                    "matched_serial_nanos": 1000,
                    "strategy_total_nanos": 900,
                    "matched_serial_speedup": 1000 / 900,
                    "planning_nanos": 10,
                    "preexecution_nanos": 100,
                    "reconciliation_nanos": 20,
                    "post_consensus_nanos": 20,
                    "cutoff_overrun_nanos": 0,
                    "prepared_receipts": txs,
                    "reused_receipts": txs,
                    "replayed_transactions": 0,
                    "canonical_transactions": 0,
                    "discovered_conflicts": 0,
                    "reference_conflicts": 0,
                    "dependency_edges": 0,
                    "waves": 1,
                    "max_wave_width": txs,
                    "serial_bypassed": False,
                    "projected_speedup": 2.0 if strategy == "cost-aware" else None,
                    "serial_equivalent": True,
                    "feedback_scope": "strictly-prior-blocks-only",
                    "symbolic_source": "checked-in-source-derived-native-s3-profiles",
                    "planning_source": planning_source,
                    "phase_model": "fixture",
                    "consensus_cutoff_nanos": config["consensus_cutoff_ms"] * 1_000_000,
                    "pre_consensus_nanos": 110,
                    "consensus_bottleneck_nanos": 110,
                    "post_consensus_speedup": 50.0,
                    "feedback_nanos": 1 if strategy in {"probability-only", "cost-aware"} else 0,
                    "probability_threshold": config["probability_threshold"],
                    "cost_bypass_speedup": config["cost_bypass_speedup"],
                    "strategy_order_seed": config["strategy_order_seed"],
                    "evaluation_config_id": config["experiment_id"],
                })
    return rows, config


class SchedulerValidatorTests(unittest.TestCase):
    def test_complete_frozen_fixture_is_accepted_without_performance_threshold(self):
        rows, config = frozen_fixture_records()
        report = validator.validate(
            rows,
            config,
            {"freeze_ready": True},
            {"accepted": True, "frozen_gate_status": True},
        )
        self.assertTrue(report["accepted"], report["errors"][:5])
        self.assertIn("none", report["performance_threshold_policy"])

    def test_state_mismatch_is_rejected(self):
        rows, config = frozen_fixture_records()
        rows[0]["serial_equivalent"] = False
        report = validator.validate(rows, config, {"freeze_ready": True}, {"accepted": True, "frozen_gate_status": True})
        self.assertFalse(report["accepted"])
        self.assertTrue(any("not serial-equivalent" in error for error in report["errors"]))


    def test_rust_benchmark_record_schema_matches_emitted_reference_conflicts(self):
        source = (
            ROOT
            / "runtime/crates/acg-vegeta-native-s3-executor/src/bin/acg-vegeta-native-s3-benchmark.rs"
        ).read_text(encoding="utf-8")
        self.assertIn("let mut blocks: Vec<ExecutionBlock> = Vec::new();", source)
        record_start = source.index("struct Record {")
        record_end = source.index("\n}", record_start)
        record_body = source[record_start:record_end]
        self.assertIn("reference_conflicts: u64,", record_body)
        self.assertIn("reference_conflicts:reference_conflicts.len() as u64,", source)

    def test_adaptive_current_block_feedback_label_is_rejected(self):
        rows, config = frozen_fixture_records()
        row = next(r for r in rows if r["strategy"] == "cost-aware")
        row["planning_source"] = "current-block concrete conflict feedback"
        report = validator.validate(rows, config, {"freeze_ready": True}, {"accepted": True, "frozen_gate_status": True})
        self.assertFalse(report["accepted"])
        self.assertTrue(any("prior-block provenance" in error for error in report["errors"]))


if __name__ == "__main__":
    unittest.main()
