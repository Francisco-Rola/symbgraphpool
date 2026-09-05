import csv
import json
import runpy
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
GENERATOR = ROOT / "tools" / "internal" / "generate-manifest-matrix.py"
AGGREGATOR = ROOT / "tools" / "internal" / "aggregate-experiment.py"
V1_CACHE_CHECK = ROOT / "tools" / "internal" / "check-conflictlab-v1-campaign-cache.py"
V1_CORRECTNESS_DIAG = ROOT / "tools" / "internal" / "diagnose-conflictlab-v1-correctness.py"
V1_VALIDATOR = ROOT / "tools" / "internal" / "validate-conflictlab-v1.py"
PARALLELISM_SUMMARY = ROOT / "tools" / "internal" / "summarize-conflictlab-parallelism.py"
FOUR_FIX_SUMMARY = ROOT / "tools" / "internal" / "summarize-conflictlab-fixes.py"
CONFLICTLAB_FIXTURES = ROOT / "tools" / "tests" / "fixtures" / "conflictlab"
BASELINE_GRID = CONFLICTLAB_FIXTURES / "conflictlab-strategy-smoke.grid.json"
BASELINE_SUMMARY = ROOT / "tools" / "internal" / "summarize-baseline-comparison.py"

sys.path.insert(0, str(ROOT / "tools" / "internal"))
from conflictlab_v1_miss_policy import (  # noqa: E402
    COARSE_SYMBOLIC_GRANULARITY,
    INJECTED_PREDICTION_FAULT,
    RUNTIME_ONLY_DEPENDENCY,
    STATE_DERIVED_SYMBOLIC_KEY,
    UNEXPECTED_INPUT_RESOLVED,
    validate_candidate_miss_policy,
)


class EvaluationToolTests(unittest.TestCase):
    def test_matrix_generator_expands_cases_modes_workers_and_seeds(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            policy = {
                "require_started_at_utc": False,
                "require_git_revision": False,
                "require_build_profile": False,
                "require_rustc_version": False,
                "require_clean_git_tree": False,
                "required_build_profile": None,
                "require_nonempty_parameters": False,
                "require_serial_reference": True,
                "require_correctness_digests": True,
                "require_serial_equivalent": True,
                "required_environment_keys": [],
                "performance": {
                    "max_service_inflation_milli": None,
                    "max_scheduler_realization_milli": None,
                    "max_planning_overhead_milli": None,
                    "max_feedback_overhead_milli": None,
                    "min_parallel_speedup_milli": None,
                },
            }
            spec = {
                "schema_version": 1,
                "experiment_id": "matrix-test",
                "physical_core_limit": 6,
                "policy": policy,
                "base_run": {"workload": "conflictlab", "parameters": {"transactions": "20"}},
                "matrix": {
                    "modes": ["static", "cost-aware"],
                    "workers": [1, 2],
                    "seeds": [11, 22],
                    "parameters": {"accounts": [2, 4]},
                    "cases": [
                        {"parameters": {"complexity": "tiny", "work_iterations": 0}},
                        {"parameters": {"complexity": "heavy", "work_iterations": 1000}},
                    ],
                },
                "order_seed": 7,
            }
            source = temp / "grid.json"
            output = temp / "manifest.json"
            source.write_text(json.dumps(spec), encoding="utf-8")
            subprocess.run([sys.executable, str(GENERATOR), str(source), str(output)], check=True)
            manifest = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(len(manifest["runs"]), 32)
            self.assertEqual(sorted(run["run_index"] for run in manifest["runs"]), list(range(1, 33)))
            self.assertTrue(all(run["workers"] <= 6 for run in manifest["runs"]))
            self.assertEqual({run["parameters"]["complexity"] for run in manifest["runs"]}, {"tiny", "heavy"})

    def test_baseline_smoke_matrix_has_all_seven_strategies(self):
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "manifest.json"
            subprocess.run(
                [sys.executable, str(GENERATOR), str(BASELINE_GRID), str(output)],
                check=True,
            )
            manifest = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(len(manifest["runs"]), 63)
            self.assertEqual(
                {run["mode"] for run in manifest["runs"]},
                {
                    "serial",
                    "aria-fb",
                    "vegeta",
                    "exact-access",
                    "static",
                    "probability-only",
                    "cost-aware",
                },
            )
            self.assertEqual({run["seed"] for run in manifest["runs"]}, {11, 47, 101})
            self.assertTrue(
                all(run["parameters"]["consensus_divergence"] == "identical" for run in manifest["runs"])
            )

    def test_matched_serial_normalization_uses_direct_serial_same_seed_and_parameters(self):
        namespace = runpy.run_path(str(AGGREGATOR), run_name="aggregate_test")
        records = [
            {
                "metadata": {
                    "experiment_id": "baseline",
                    "workload": "conflictlab",
                    "mode": "serial",
                    "workers": 6,
                    "seed": 11,
                    "parameters": {"contention": "25pct"},
                },
                "pipeline_timing": {"total_adaptive_block_nanos": 1000},
            },
            {
                "metadata": {
                    "experiment_id": "baseline",
                    "workload": "conflictlab",
                    "mode": "static",
                    "workers": 6,
                    "seed": 11,
                    "parameters": {"contention": "25pct"},
                },
                "pipeline_timing": {"total_adaptive_block_nanos": 400},
            },
        ]
        flat = [{}, {}]
        namespace["add_matched_serial_metrics"](records, flat)
        self.assertEqual(flat[0]["derived.matched_serial_speedup"], 1.0)
        self.assertEqual(flat[1]["derived.matched_serial_total_nanos"], 1000.0)
        self.assertEqual(flat[1]["derived.matched_serial_speedup"], 2.5)

    def test_baseline_summary_reports_matched_serial_ranges_and_controls(self):
        namespace = runpy.run_path(str(BASELINE_SUMMARY), run_name="baseline_summary_test")
        records = []
        for mode, wall in {
            "serial": 1000,
            "aria-fb": 800,
            "vegeta": 700,
            "exact-access": 400,
            "static": 500,
            "probability-only": 600,
            "cost-aware": 650,
        }.items():
            records.append(
                {
                    "metadata": {
                        "experiment_id": "baseline",
                        "workload": "conflictlab",
                        "mode": mode,
                        "workers": 6,
                        "seed": 11,
                        "parameters": {
                            "contention": "25pct",
                            "hot_account_probability_bps": "2500",
                            "acg.serial_bypass_enabled": "false",
                            "acg.regime_change_enabled": "false",
                            "consensus_divergence": "identical",
                        },
                    },
                    "pipeline_timing": {
                        "total_adaptive_block_nanos": wall,
                        "serial_reference_execution_nanos": 1000,
                    },
                    "consensus": {
                        "serial_validation_latency_nanos": 1000,
                        "bottleneck_nanos": wall,
                        "post_consensus_nanos": wall,
                    },
                    "execution": {"replayed_transactions": 0},
                    "strategy": {
                        "discovered_conflicts": 0,
                        "replay_dependencies": 0,
                        "forward_conflict_fallbacks": 0,
                        "access_set_mismatch_fallbacks": 0,
                    },
                    "correctness": {"serial_equivalent": True},
                }
            )
        serial_index = namespace["build_serial_index"](records)
        static = next(record for record in records if record["metadata"]["mode"] == "static")
        self.assertEqual(namespace["matched_serial_speedup"](static, serial_index), 2.0)
        self.assertEqual(namespace["parameter_value"](records, "acg.serial_bypass_enabled"), "false")
        self.assertEqual(namespace["fmt_range"]([1.5, 2.0, 2.5]), "1.50/2.00/2.50x")
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            source = temp / "records.jsonl"
            report = temp / "report.txt"
            matched = temp / "matched.csv"
            source.write_text("".join(json.dumps(record) + "\n" for record in records), encoding="utf-8")
            subprocess.run(
                [
                    sys.executable,
                    str(BASELINE_SUMMARY),
                    str(source),
                    "--output",
                    str(report),
                    "--matched-output",
                    str(matched),
                ],
                check=True,
                capture_output=True,
                text=True,
            )
            report_text = report.read_text(encoding="utf-8")
            self.assertIn("matched direct-Serial actual block wall", report_text)
            self.assertIn("serial_bypass_enabled=false", report_text)
            self.assertIn("2.00/2.00/2.00x", report_text)
            with matched.open(newline="", encoding="utf-8") as handle:
                matched_rows = list(csv.DictReader(handle))
            static_row = next(row for row in matched_rows if row["mode"] == "static")
            self.assertAlmostEqual(float(static_row["matched_serial_speedup"]), 2.0)

    def test_aggregator_emits_flat_wide_and_long_plot_tables(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            records = temp / "records.jsonl"
            rows = []
            for index, wall in enumerate((1000, 1200), start=1):
                rows.append(
                    {
                        "metadata": {
                            "workload": "conflictlab",
                            "mode": "cost-aware",
                            "workers": 2,
                            "run_index": index,
                            "seed": index,
                            "parameters": {"complexity": "tiny"},
                        },
                        "parallelism": {
                            "actual_execution_wall_nanos": wall,
                            "serial_equivalent_work_nanos": 2000,
                            "serial_cost_dag_bound_nanos": 1000,
                            "perfect_conflict_dag_bound_nanos": 500,
                            "perfect_conflict_parallel_lower_bound_nanos": 500,
                            "observed_service_dag_bound_nanos": wall,
                            "observed_service_work_nanos": 2000,
                            "worker_capacity_bound_nanos": 1000,
                            "parallel_lower_bound_nanos": max(wall, 1000),
                            "service_inflation_milli": 1000,
                            "scheduler_realization_milli": 1000,
                            "scheduler_realization_corrected_milli": 1000,
                        },
                        "planning": {"total_nanos": 100, "serial_bypassed": False},
                        "feedback_timing": {"total_nanos": 10},
                        "adaptive_state": {
                            "static_relationships": 3,
                            "runtime_fallback_relationships": 1,
                            "candidate_miss_history_relationships": 1,
                            "mean_probability_q16": 32768,
                            "mean_confidence_q16": 16384,
                        },
                        "pipeline_timing": {
                            "planning_nanos": 100,
                            "preexecution_nanos": wall,
                            "pre_execution_feedback_nanos": 10,
                            "reconciliation_nanos": 100,
                            "reconciliation_feedback_nanos": 0,
                            "total_adaptive_block_nanos": wall + 210,
                            "serial_reference_execution_nanos": 2000,
                            "end_to_end_speedup_milli": round(2000 * 1000 / (wall + 210)),
                        },
                        "execution": {
                            "transactions": 2,
                            "replay_or_missing_execution_nanos": 0,
                            "replayed_transactions": 0,
                            "invalidated_results": 0,
                            "reused_results": 2,
                            "hard_dependency_count": 0,
                            "max_in_flight": 2,
                            "contract": {
                                "aggregate_wasm_instance_acquire_nanos": 10,
                                "wasm_instance_reuse_hits": 1,
                                "wasm_instance_pool_misses": 1,
                                "aggregate_wasm_entrypoint_nanos": 100,
                                "aggregate_wasm_recycle_nanos": 5,
                                "aggregate_host_storage_nanos": 20,
                                "aggregate_mvcc_storage_point_nanos": 2,
                                "aggregate_mvcc_storage_range_nanos": 0,
                            },
                        },
                        "scheduling": {
                            "candidate_edges": 10,
                            "materialized_candidate_edges": 5,
                            "pre_reduction_dependencies": 3,
                            "scheduled_dependencies": 3,
                            "edges_elided_by_reduction": 0,
                            "hard_dependencies": 3,
                        },
                        "feedback": {
                            "positive_observations": 1,
                            "negative_observations": 1,
                            "serialization_cost_observations": 0,
                            "serialization_cost_batches_applied": 0,
                            "replay_impact_observations": 0,
                            "observation_batches_applied": 0,
                        },
                    }
                )
            records.write_text("".join(json.dumps(row) + "\n" for row in rows), encoding="utf-8")
            output = temp / "out"
            subprocess.run([sys.executable, str(AGGREGATOR), str(records), "--out-dir", str(output)], check=True)
            self.assertTrue((output / "records-flat.csv").exists())
            self.assertTrue((output / "summary-wide.csv").exists())
            with (output / "plot-long.csv").open(newline="", encoding="utf-8") as handle:
                plot = list(csv.DictReader(handle))
            speedup = next(row for row in plot if row["metric"] == "speedup")
            self.assertEqual(speedup["n"], "2")
            self.assertAlmostEqual(float(speedup["mean"]), (2.0 + 2000 / 1200) / 2)
            corrected = next(row for row in plot if row["metric"] == "scheduler_realization")
            self.assertEqual(corrected["n"], "2")
            self.assertAlmostEqual(float(corrected["mean"]), 1.0)
            pipeline = next(row for row in plot if row["metric"] == "pipeline_speedup")
            self.assertEqual(pipeline["n"], "2")
            validation = next(row for row in plot if row["metric"] == "validation_latency_speedup")
            self.assertAlmostEqual(float(validation["mean"]), 20.0)
            throughput = next(row for row in plot if row["metric"] == "throughput_speedup")
            self.assertAlmostEqual(
                float(throughput["mean"]),
                (2000 / 1110 + 2000 / 1310) / 2,
            )
            acg_tps = next(row for row in plot if row["metric"] == "acg_throughput_tps")
            self.assertAlmostEqual(
                float(acg_tps["mean"]),
                (2e9 / 1110 + 2e9 / 1310) / 2,
            )
            materialization = next(
                row for row in plot if row["metric"] == "candidate_materialization_compression"
            )
            self.assertAlmostEqual(float(materialization["mean"]), 2.0)
            reuse_hits = next(row for row in plot if row["metric"] == "wasm_instance_reuse_hits")
            self.assertAlmostEqual(float(reuse_hits["mean"]), 1.0)
            feedback_unit = next(
                row for row in plot if row["metric"] == "feedback_us_per_observation"
            )
            self.assertAlmostEqual(float(feedback_unit["mean"]), 0.005)
            oracle = next(row for row in plot if row["metric"] == "perfect_conflict_oracle_speedup")
            self.assertAlmostEqual(float(oracle["mean"]), 4.0)
            realization = next(row for row in plot if row["metric"] == "oracle_realization")
            self.assertAlmostEqual(float(realization["mean"]), (2.0 + 2.4) / 2)
            fallback = next(
                row for row in plot if row["metric"] == "adaptive_runtime_fallback_relationships"
            )
            self.assertAlmostEqual(float(fallback["mean"]), 1.0)
            probability = next(row for row in plot if row["metric"] == "adaptive_mean_probability")
            self.assertAlmostEqual(float(probability["mean"]), 32768 / 65535)
            precision = next(row for row in plot if row["metric"] == "prediction_precision")
            self.assertAlmostEqual(float(precision["mean"]), 0.5)
            recall = next(row for row in plot if row["metric"] == "prediction_recall")
            self.assertAlmostEqual(float(recall["mean"]), 1.0)
            hard_dependencies = next(
                row for row in plot if row["metric"] == "hard_dependencies"
            )
            self.assertAlmostEqual(float(hard_dependencies["mean"]), 3.0)

    def test_consensus_window_counts_serial_fallback_preexecution_before_consensus(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            records = temp / "records.jsonl"
            record = {
                "metadata": {
                    "experiment_id": "fallback-window-test",
                    "workload": "conflictlab",
                    "mode": "static",
                    "workers": 1,
                    "run_index": 1,
                    "seed": 1,
                    "parameters": {},
                },
                "parallelism": {
                    "actual_execution_wall_nanos": 1_000,
                    "serial_equivalent_work_nanos": 1_200,
                    "serial_cost_dag_bound_nanos": 1_200,
                    "observed_service_dag_bound_nanos": 1_000,
                    "observed_service_work_nanos": 1_000,
                    "worker_capacity_bound_nanos": 1_000,
                    "parallel_lower_bound_nanos": 1_000,
                    "service_inflation_milli": 1_000,
                    "scheduler_realization_milli": 1_000,
                    "scheduler_realization_corrected_milli": 1_000,
                },
                "planning": {"total_nanos": 100, "serial_bypassed": True},
                "feedback_timing": {"total_nanos": 0},
                "pipeline_timing": {
                    "planning_nanos": 100,
                    "preexecution_nanos": 1_000,
                    "pre_execution_feedback_nanos": 0,
                    "reconciliation_nanos": 0,
                    "reconciliation_feedback_nanos": 0,
                    "total_adaptive_block_nanos": 1_100,
                    "serial_reference_execution_nanos": 1_200,
                    "end_to_end_speedup_milli": 1_091,
                },
                "execution": {
                    "transactions": 2,
                    "replay_or_missing_execution_nanos": 0,
                    "replayed_transactions": 0,
                    "invalidated_results": 0,
                    "reused_results": 0,
                    "hard_dependency_count": 0,
                    "max_in_flight": 1,
                    "contract": {
                        "aggregate_wasm_instance_acquire_nanos": 0,
                        "wasm_instance_reuse_hits": 2,
                        "wasm_instance_pool_misses": 0,
                        "aggregate_wasm_entrypoint_nanos": 900,
                        "aggregate_wasm_recycle_nanos": 0,
                        "aggregate_host_storage_nanos": 0,
                        "aggregate_mvcc_storage_point_nanos": 0,
                        "aggregate_mvcc_storage_range_nanos": 0,
                        "aggregate_request_execution_nanos": 1_000,
                    },
                },
                "scheduling": {
                    "candidate_edges": 0,
                    "materialized_candidate_edges": 0,
                    "pre_reduction_dependencies": 0,
                    "scheduled_dependencies": 0,
                    "edges_elided_by_reduction": 0,
                },
                "feedback": {
                    "positive_observations": 0,
                    "negative_observations": 0,
                    "serialization_cost_observations": 0,
                    "serialization_cost_batches_applied": 0,
                    "replay_impact_observations": 0,
                    "observation_batches_applied": 0,
                },
            }
            records.write_text(json.dumps(record) + "\n", encoding="utf-8")
            output = temp / "out"
            subprocess.run(
                [
                    sys.executable,
                    str(AGGREGATOR),
                    str(records),
                    "--out-dir",
                    str(output),
                    "--preconsensus-window-ms",
                    "0.0005",
                ],
                check=True,
            )
            with (output / "records-flat.csv").open(newline="", encoding="utf-8") as handle:
                flat = next(csv.DictReader(handle))
            self.assertAlmostEqual(float(flat["derived.preconsensus_eligible_nanos"]), 1_100.0)
            self.assertAlmostEqual(float(flat["derived.pre_consensus_nanos"]), 500.0)
            self.assertAlmostEqual(float(flat["derived.preconsensus_spill_nanos"]), 600.0)
            self.assertAlmostEqual(float(flat["derived.post_consensus_nanos"]), 600.0)
            self.assertAlmostEqual(float(flat["derived.validation_latency_speedup"]), 2.0)
            self.assertAlmostEqual(float(flat["derived.throughput_speedup"]), 2.0)



    def test_aggregator_prefers_physically_recorded_consensus_split(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            records = temp / "records.jsonl"
            record = {
                "metadata": {
                    "experiment_id": "actual-cutoff-test", "workload": "conflictlab",
                    "mode": "cost-aware", "workers": 2, "run_index": 1, "seed": 1,
                    "parameters": {"consensus_cutoff_ms": "250"},
                },
                "parallelism": {"actual_execution_wall_nanos": 800, "serial_equivalent_work_nanos": 2000},
                "planning": {"total_nanos": 100, "serial_bypassed": False},
                "feedback_timing": {"total_nanos": 0},
                "pipeline_timing": {
                    "planning_nanos": 100, "preexecution_nanos": 800,
                    "pre_execution_feedback_nanos": 0, "reconciliation_nanos": 200,
                    "reconciliation_feedback_nanos": 0, "total_adaptive_block_nanos": 1100,
                    "serial_reference_execution_nanos": 2000, "end_to_end_speedup_milli": 1818,
                },
                "consensus": {
                    "cutoff_nanos": 250000000, "candidate_transactions": 2, "decided_transactions": 2,
                    "shared_transactions": 2, "same_position_transactions": 2, "common_prefix_transactions": 2,
                    "prepared_receipts": 2, "successful_preexecution_receipts": 1,
                    "failed_preexecution_receipts": 1, "receipts_ready_by_cutoff": 1,
                    "receipts_completed_after_cutoff": 1, "cutoff_reached": True,
                    "pre_consensus_nanos": 700, "pre_consensus_overrun_nanos": 200,
                    "post_consensus_nanos": 400, "bottleneck_nanos": 700,
                    "serial_validation_latency_nanos": 2000,
                    "validation_latency_speedup_milli": 5000, "throughput_speedup_milli": 2857,
                },
                "execution": {
                    "transactions": 2, "replay_or_missing_execution_nanos": 0, "replayed_transactions": 0,
                    "invalidated_results": 0, "reused_results": 2, "hard_dependency_count": 0,
                    "max_in_flight": 2, "contract": {},
                },
                "scheduling": {"candidate_edges": 0, "materialized_candidate_edges": 0, "pre_reduction_dependencies": 0, "scheduled_dependencies": 0, "edges_elided_by_reduction": 0},
                "feedback": {"positive_observations": 0, "negative_observations": 0, "serialization_cost_observations": 0, "serialization_cost_batches_applied": 0, "replay_impact_observations": 0, "observation_batches_applied": 0},
            }
            records.write_text(json.dumps(record) + "\n", encoding="utf-8")
            output = temp / "out"
            subprocess.run([sys.executable, str(AGGREGATOR), str(records), "--out-dir", str(output)], check=True)
            with (output / "records-flat.csv").open(newline="", encoding="utf-8") as handle:
                flat = next(csv.DictReader(handle))
            self.assertAlmostEqual(float(flat["derived.pre_consensus_nanos"]), 700.0)
            self.assertAlmostEqual(float(flat["derived.preconsensus_spill_nanos"]), 200.0)
            self.assertAlmostEqual(float(flat["derived.post_consensus_nanos"]), 400.0)
            self.assertAlmostEqual(float(flat["derived.validation_latency_speedup"]), 5.0)
            self.assertAlmostEqual(float(flat["derived.throughput_speedup"]), 2000 / 700)
            with (output / "plot-long.csv").open(newline="", encoding="utf-8") as handle:
                plot = list(csv.DictReader(handle))
            successful = next(row for row in plot if row["metric"] == "successful_preexecution_receipts")
            failed = next(row for row in plot if row["metric"] == "failed_preexecution_receipts")
            self.assertAlmostEqual(float(successful["mean"]), 1.0)
            self.assertAlmostEqual(float(failed["mean"]), 1.0)




    def test_v1_campaign_cache_reuses_only_exact_accepted_manifest(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            campaign = temp / "core-state"
            campaign.mkdir()
            manifest = {
                "schema_version": 1,
                "experiment_id": "conflictlab-v1-core-state",
                "record_schema_version": 3,
                "physical_core_limit": 6,
                "policy": {},
                "runs": [
                    {
                        "workload": "conflictlab",
                        "mode": "static",
                        "run_index": 1,
                        "seed": 11,
                        "workers": 6,
                        "parameters": {"transactions": "32"},
                    }
                ],
            }
            expected = temp / "expected.json"
            expected.write_text(json.dumps(manifest), encoding="utf-8")
            (campaign / "manifest.json").write_text(json.dumps(manifest), encoding="utf-8")
            acceptance = {
                "experiment_id": "conflictlab-v1-core-state",
                "status": "accepted",
                "expected_runs": 1,
                "observed_runs": 1,
                "accepted_runs": 1,
                "performance_regressions": 0,
                "incomplete_runs": 0,
                "configuration_errors": 0,
                "correctness_failures": 0,
            }
            (campaign / "acceptance.json").write_text(json.dumps(acceptance), encoding="utf-8")
            record = {
                "metadata": {
                    "experiment_id": "conflictlab-v1-core-state",
                    **manifest["runs"][0],
                }
            }
            (campaign / "records.jsonl").write_text(json.dumps(record) + "\n", encoding="utf-8")

            accepted = subprocess.run(
                [sys.executable, str(V1_CACHE_CHECK), str(campaign), str(expected)],
                text=True,
                capture_output=True,
            )
            self.assertEqual(accepted.returncode, 0, accepted.stdout + accepted.stderr)
            self.assertIn("accepted 1-run campaign", accepted.stdout)

            acceptance["status"] = "incomplete"
            (campaign / "acceptance.json").write_text(json.dumps(acceptance), encoding="utf-8")
            incomplete = subprocess.run(
                [sys.executable, str(V1_CACHE_CHECK), str(campaign), str(expected)],
                text=True,
                capture_output=True,
            )
            self.assertEqual(incomplete.returncode, 1)
            self.assertIn("not 'accepted'", incomplete.stdout)

    def test_v1_submission_suite_expands_to_fixed_six_core_evidence_matrix(self):
        expected = {
            "v1-core-state.grid.json": 960,
            "v1-cutoff-divergence.grid.json": 1440,
            "v1-serial-cutoff.grid.json": 72,
            "v1-compaction-reference.grid.json": 240,
            "v1-symbolic-granularity.grid.json": 72,
            "v1-prediction-fault-recovery.grid.json": 336,
            "v1-adaptation-transitions.grid.json": 288,
            "v1-execution-semantics.grid.json": 126,
            "v1-block-scaling.grid.json": 168,
            "v1-policy-pareto.grid.json": 96,
            "v1-bucket-sensitivity.grid.json": 48,
            "v1-ordering-sensitivity.grid.json": 36,
            "v1-vm-lifecycle.grid.json": 24,
            "v1-statistical-headlines.grid.json": 720,
            "v1-long-run-soak.grid.json": 4,
        }
        manifests = {}
        total = 0
        for name, count in expected.items():
            source = CONFLICTLAB_FIXTURES / name
            with tempfile.TemporaryDirectory() as temp:
                output = Path(temp) / "manifest.json"
                subprocess.run([sys.executable, str(GENERATOR), str(source), str(output)], check=True)
                manifest = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(len(manifest["runs"]), count)
            self.assertTrue(all(run["workers"] == 6 for run in manifest["runs"]))
            self.assertTrue(all(run["parameters"]["execution_backend"] == "wasm" for run in manifest["runs"]))
            self.assertFalse(any(
                key.startswith("acg.exploration_")
                for run in manifest["runs"]
                for key in run["parameters"]
            ))
            manifests[name] = manifest
            total += count
        self.assertEqual(total, 4630)
        for manifest in manifests.values():
            for run in manifest["runs"]:
                self.assertEqual(run["parameters"]["acg.softening_min_confidence"], "0.20")
                self.assertEqual(run["parameters"]["acg.serial_bypass_immediate_speedup_floor"], "1.0")
                self.assertEqual(run["parameters"]["acg.regime_change_enabled"], "true")
                self.assertEqual(run["parameters"]["acg.regime_retained_evidence"], "0.20")
                self.assertEqual(run["parameters"]["acg.regime_probation_bypass_blocks"], "2")
                self.assertEqual(run["parameters"]["acg.regime_probation_min_projected_speedup"], "1.10")
                self.assertEqual(run["parameters"]["acg.candidate_miss_verification_weight_threshold"], "32.0")
        max_gas = str((1 << 64) - 1)
        for name, manifest in manifests.items():
            self.assertTrue(
                all(run["parameters"].get("vm_gas_limit") == max_gas for run in manifest["runs"]),
                f"{name} does not use the non-binding retained-VM gas budget",
            )
            lifecycles = {run["parameters"]["vm_instance_lifecycle"] for run in manifest["runs"]}
            if name == "v1-vm-lifecycle.grid.json":
                self.assertEqual(lifecycles, {"reuse", "recycle"})
            else:
                self.assertEqual(lifecycles, {"reuse"}, f"{name} is not retained-VM canonical")
            buffered = {run["parameters"]["acg.serial_bypass_buffered_preexecution"] for run in manifest["runs"]}
            if name == "v1-serial-cutoff.grid.json":
                self.assertEqual(buffered, {"true"})
            else:
                self.assertEqual(buffered, {"false"})

        cutoff = manifests["v1-cutoff-divergence.grid.json"]["runs"]
        self.assertEqual(
            {run["parameters"]["consensus_cutoff_ms"] for run in cutoff},
            {"25", "50", "100", "250", "500"},
        )
        self.assertEqual(
            {run["parameters"]["consensus_divergence"] for run in cutoff},
            {"identical", "reorder-5pct", "reorder-20pct", "tail-5pct", "tail-20pct", "tail-reorder-10pct"},
        )

        semantics = manifests["v1-execution-semantics.grid.json"]["runs"]
        self.assertEqual(
            {run["parameters"]["operation_mix"] for run in semantics},
            {"point-mixed", "stateful-mixed", "range-delete", "bank-funds", "bank-mixed", "instantiate", "full"},
        )

        compaction = manifests["v1-compaction-reference.grid.json"]["runs"]
        self.assertEqual(
            {run["parameters"]["acg.compact_equivalence_groups"] for run in compaction},
            {"true", "false"},
        )
        self.assertEqual(
            {run["parameters"]["consensus_cutoff_ms"] for run in compaction},
            {"5000"},
            "dense/compact semantic reference must use a non-binding consensus window",
        )
        self.assertEqual(
            {run["parameters"]["acg.warmup_compact_equivalence_groups"] for run in compaction},
            {"false"},
            "dense/compact measured pairs must use the same dense warm-up representation",
        )
        self.assertEqual(
            {run["parameters"]["acg.warmup_workers"] for run in compaction},
            {"1"},
            "dense/compact reference warm-up must be deterministic across independent runs",
        )
        self.assertEqual({int(run["parameters"]["sim.block_size"]) for run in compaction}, {32, 64, 128, 256, 512})

        scaling = manifests["v1-block-scaling.grid.json"]["runs"]
        self.assertEqual(
            {int(run["parameters"]["sim.block_size"]) for run in scaling},
            {32, 64, 128, 256, 512, 1024, 2048},
        )
        self.assertEqual({run["workers"] for run in scaling}, {6})

        faults = manifests["v1-prediction-fault-recovery.grid.json"]["runs"]
        self.assertEqual(
            {run["parameters"]["prediction_fault_mode"] for run in faults},
            {"none", "hidden-key", "spurious-key"},
        )
        self.assertEqual(
            {run["parameters"]["postchange_warmup_blocks"] for run in faults},
            {"0", "1", "4", "8"},
        )
        hidden = [run for run in faults if run["parameters"]["prediction_fault_mode"] == "hidden-key"]
        spurious = [run for run in faults if run["parameters"]["prediction_fault_mode"] == "spurious-key"]
        self.assertEqual({run["parameters"]["prediction_fault_duration_blocks"] for run in hidden}, {"1"})
        self.assertNotEqual({run["parameters"]["prediction_fault_duration_blocks"] for run in spurious}, {"1"})

        headlines = manifests["v1-statistical-headlines.grid.json"]["runs"]
        self.assertEqual(len({run["seed"] for run in headlines}), 20)
        soak = manifests["v1-long-run-soak.grid.json"]["runs"]
        self.assertTrue(all(run["parameters"]["warmup_blocks"] == "1000" for run in soak))

    def test_four_fix_focused_matrices_are_small_and_target_the_new_mechanisms(self):
        expected = {
            "fix-reorder-readset.grid.json": 6,
            "fix-regime-failsafe.grid.json": 100,
            "fix-miss-recovery.grid.json": 16,
            "fix-cost-throughput.grid.json": 24,
        }
        manifests = {}
        for name, count in expected.items():
            source = CONFLICTLAB_FIXTURES / name
            with tempfile.TemporaryDirectory() as temp:
                output = Path(temp) / "manifest.json"
                subprocess.run([sys.executable, str(GENERATOR), str(source), str(output)], check=True)
                manifest = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(len(manifest["runs"]), count)
            self.assertEqual({run["workers"] for run in manifest["runs"]}, {6})
            self.assertTrue(all(run["parameters"]["consensus_cutoff_ms"] == "5000" for run in manifest["runs"]))
            self.assertEqual(
                {run["parameters"]["acg.serial_bypass_buffered_preexecution"] for run in manifest["runs"]},
                {"false"},
            )
            manifests[name] = manifest
        self.assertEqual(sum(expected.values()), 146)

        reorder = manifests["fix-reorder-readset.grid.json"]["runs"]
        self.assertEqual({run["parameters"]["consensus_divergence"] for run in reorder}, {"reorder-20pct"})

        regime = manifests["fix-regime-failsafe.grid.json"]["runs"]
        self.assertEqual({run["parameters"]["acg.serial_bypass_enabled"] for run in regime}, {"true"})
        self.assertEqual({run["seed"] for run in regime}, {11, 47, 101, 211, 307})
        self.assertEqual(
            {run["parameters"]["postchange_warmup_blocks"] for run in regime},
            {"0", "1", "2", "3", "4"},
        )
        self.assertEqual({run["parameters"]["acg.regime_probation_bypass_blocks"] for run in regime}, {"2"})
        self.assertEqual(
            {run["parameters"]["acg.regime_probation_min_projected_speedup"] for run in regime},
            {"1.10"},
        )

        miss = manifests["fix-miss-recovery.grid.json"]["runs"]
        self.assertEqual({run["parameters"]["prediction_fault_mode"] for run in miss}, {"hidden-key"})
        self.assertEqual({run["parameters"]["prediction_fault_duration_blocks"] for run in miss}, {"1"})

        cost = manifests["fix-cost-throughput.grid.json"]["runs"]
        self.assertEqual({run["mode"] for run in cost}, {"probability-only", "cost-aware"})
        self.assertEqual({run["parameters"]["acg.serial_bypass_enabled"] for run in cost}, {"false"})

    def test_four_fix_summarizer_direct_bypass_is_structural_not_timing_gated(self):
        namespace = runpy.run_path(str(FOUR_FIX_SUMMARY), run_name="four_fix_summary_test")
        violations = namespace["direct_bypass_violations"](
            {
                "metadata": {
                    "parameters": {"acg.serial_bypass_buffered_preexecution": "false"}
                },
                "planning": {"serial_bypassed": True},
                "execution": {
                    "transactions": 256,
                    "workers": 1,
                    "speculative_results": 0,
                    "predicted_transactions": 0,
                    "preexecution_executor_total_nanos": 0,
                    "preexecution_worker_wall_nanos": 0,
                    "reused_results": 0,
                    "invalidated_results": 0,
                    "replayed_transactions": 0,
                    "canonical_transactions": 256,
                },
                "consensus": {
                    "prepared_receipts": 0,
                    "successful_preexecution_receipts": 0,
                    "failed_preexecution_receipts": 0,
                },
                "pipeline_timing": {
                    "preexecution_nanos": 0,
                    "serial_reference_execution_nanos": 10,
                    "total_adaptive_block_nanos": 20,
                },
                "feedback_timing": {"total_nanos": 0},
            }
        )
        self.assertEqual(violations, [])

    def test_four_fix_summarizer_writes_report_even_when_validation_fails(self):
        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            for name in ("reorder", "regime", "miss", "cost"):
                directory = temp / name
                directory.mkdir(parents=True)
                (directory / "records.jsonl").write_text("", encoding="utf-8")
            report = temp / "fix-validation-report.txt"
            result = subprocess.run(
                [sys.executable, str(FOUR_FIX_SUMMARY), str(temp), "--output", str(report)],
                text=True,
                capture_output=True,
            )
            self.assertNotEqual(result.returncode, 0)
            self.assertTrue(report.exists())
            text = report.read_text(encoding="utf-8")
            self.assertIn("1) Reordered receipt reuse", text)
            self.assertIn("2) Regime-change fail-safe", text)
            self.assertIn("3) Transient hidden-key miss recovery", text)
            self.assertIn("4) Cost-aware combined-pipeline-wall objective", text)
            self.assertIn("FAIL: focused validation completed", text)

    def test_parallelism_ceiling_matrix_has_balanced_lane_and_amortization_cases(self):
        source = CONFLICTLAB_FIXTURES / "parallelism-ceiling.grid.json"
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "manifest.json"
            subprocess.run([sys.executable, str(GENERATOR), str(source), str(output)], check=True)
            manifest = json.loads(output.read_text(encoding="utf-8"))
        runs = manifest["runs"]
        self.assertEqual(len(runs), 120)
        self.assertEqual({run["workers"] for run in runs}, {6})
        self.assertEqual({run["mode"] for run in runs}, {"static", "probability-only"})
        self.assertEqual(
            {int(run["parameters"]["parallelism_lanes"]) for run in runs if run["parameters"]["work_iterations"] == "786432"},
            {1, 2, 3, 4, 6, 384},
        )
        self.assertEqual(
            {int(run["parameters"]["work_iterations"]) for run in runs if run["parameters"]["parallelism_lanes"] == "384"},
            {8192, 131072, 786432, 1572864},
        )
        self.assertTrue(all(run["parameters"]["prediction_quality"] == "exact" for run in runs))
        self.assertTrue(all(run["parameters"]["hot_account_probability_bps"] == "0" for run in runs))
        self.assertTrue(all(run["parameters"]["acg.serial_bypass_enabled"] == "false" for run in runs))

    def test_parallelism_summary_reports_oracle_executor_and_overhead(self):
        def record(seed, worker_wall):
            return {
                "metadata": {
                    "experiment_id": "conflictlab-parallelism-ceiling",
                    "mode": "static",
                    "run_index": seed,
                    "seed": seed,
                    "workers": 6,
                    "parameters": {
                        "parallelism_lanes": "6",
                        "work_iterations": "786432",
                        "complexity": "high",
                        "prediction_quality": "exact",
                        "consensus_divergence": "identical",
                    },
                },
                "correctness": {"serial_equivalent": True},
                "parallelism": {
                    "serial_equivalent_work_nanos": 6000,
                    "serial_cost_dag_bound_nanos": 1000,
                    "perfect_conflict_parallel_lower_bound_nanos": 1000,
                    "observed_service_dag_bound_nanos": 1050,
                    "observed_service_work_nanos": 6300,
                    "worker_capacity_bound_nanos": 1050,
                    "parallel_lower_bound_nanos": 1050,
                    "actual_execution_wall_nanos": worker_wall,
                },
                "consensus": {
                    "bottleneck_nanos": 1500,
                    "candidate_transactions": 6,
                    "prepared_receipts": 6,
                    "cutoff_reached": False,
                },
                "pipeline_timing": {
                    "planning_nanos": 100,
                    "preexecution_nanos": worker_wall + 50,
                    "pre_execution_feedback_nanos": 0,
                    "reconciliation_nanos": 100,
                    "reconciliation_feedback_nanos": 0,
                    "total_adaptive_block_nanos": 1800,
                },
                "execution": {
                    "transactions": 6,
                    "max_in_flight": 6,
                    "dependency_plan_setup_nanos": 25,
                    "preexecution_worker_wall_nanos": worker_wall,
                    "aggregate_ready_wait_nanos": 30,
                    "aggregate_visibility_capture_nanos": 60,
                    "aggregate_contract_execution_nanos": 5400,
                    "aggregate_publish_and_unblock_nanos": 120,
                    "contract": {
                        "aggregate_wasm_instance_acquire_nanos": 12,
                        "aggregate_wasm_entrypoint_nanos": 5200,
                        "aggregate_host_storage_nanos": 200,
                        "aggregate_mvcc_storage_point_nanos": 100,
                        "aggregate_mvcc_publish_nanos": 80,
                    },
                },
            }

        with tempfile.TemporaryDirectory() as temp:
            temp = Path(temp)
            records = temp / "records.jsonl"
            report = temp / "report.txt"
            csv_path = temp / "report.csv"
            records.write_text(
                "\n".join(json.dumps(record(seed, wall)) for seed, wall in [(1, 1200), (2, 1300)]) + "\n",
                encoding="utf-8",
            )
            subprocess.run(
                [sys.executable, str(PARALLELISM_SUMMARY), str(records), "--output", str(report), "--csv", str(csv_path)],
                check=True,
            )
            text = report.read_text(encoding="utf-8")
            with csv_path.open(encoding="utf-8") as handle:
                rows = list(csv.DictReader(handle))
        self.assertIn("Lane sweep at high compute", text)
        self.assertIn("Wall-clock overhead breakdown", text)
        self.assertEqual(len(rows), 1)
        self.assertAlmostEqual(float(rows[0]["oracle_speedup_x"]), 6.0)
        self.assertGreater(float(rows[0]["executor_oracle_efficiency_pct"]), 75.0)

    def test_v1_candidate_miss_policy_accepts_measured_exception_classes_with_recovery(self):
        def record(
            experiment_id,
            operation_mix,
            misses,
            *,
            fault="none",
            miss_history=1,
            granularity="fine",
        ):
            return {
                "metadata": {
                    "experiment_id": experiment_id,
                    "mode": "probability-only",
                    "seed": 7,
                    "run_index": 1,
                    "parameters": {
                        "operation_mix": operation_mix,
                        "prediction_fault_mode": fault,
                        "symbolic_granularity": granularity,
                    },
                },
                "feedback": {
                    "candidate_misses": misses,
                    "fallback_edges_created": 0,
                },
                "adaptive_state": {
                    "candidate_miss_history_relationships": miss_history,
                    "runtime_fallback_relationships": 0,
                },
            }

        records = [
            record("conflictlab-v1-execution-semantics", "stateful-mixed", 5),
            record("conflictlab-v1-symbolic-granularity", "full", 2),
            record("conflictlab-v1-execution-semantics", "bank-mixed", 3),
            record(
                "conflictlab-v1-symbolic-granularity",
                "point-mixed",
                6,
                granularity="resource",
            ),
            record(
                "conflictlab-v1-prediction-fault-recovery",
                "credit",
                4,
                fault="hidden-key",
            ),
        ]
        totals, counts = validate_candidate_miss_policy(records)
        self.assertEqual(totals[STATE_DERIVED_SYMBOLIC_KEY], 7)
        self.assertEqual(counts[STATE_DERIVED_SYMBOLIC_KEY], 2)
        self.assertEqual(totals[RUNTIME_ONLY_DEPENDENCY], 3)
        self.assertEqual(totals[INJECTED_PREDICTION_FAULT], 4)
        self.assertEqual(totals[COARSE_SYMBOLIC_GRANULARITY], 6)
        self.assertEqual(counts[COARSE_SYMBOLIC_GRANULARITY], 1)
        self.assertEqual(totals[UNEXPECTED_INPUT_RESOLVED], 0)

    def test_v1_validator_accepts_hidden_key_recovery_via_miss_history(self):
        max_gas = str((1 << 64) - 1)
        record = {
            "schema_version": 3,
            "metadata": {
                "experiment_id": "conflictlab-v1-prediction-fault-recovery",
                "mode": "probability-only",
                "seed": 11,
                "run_index": 1,
                "workers": 6,
                "physical_cores": 6,
                "parameters": {
                    "operation_mix": "credit",
                    "prediction_fault_mode": "hidden-key",
                    "vm_instance_lifecycle": "reuse",
                    "vm_gas_limit": max_gas,
                },
                "environment": {
                    "conflictlab_backend": "wasm",
                    "conflictlab_vm_instance_lifecycle": "reuse",
                    "conflictlab_vm_gas_limit": max_gas,
                    "conflictlab_retained_vm_scope": "benchmark-scoped-nonbinding-gas",
                    "conflictlab_vm_instance_lifecycle_safe": "false",
                },
            },
            "correctness": {
                "serial_equivalent": True,
                "canonical_state_digest": "same",
            },
            "parallelism": {
                "perfect_conflict_parallel_lower_bound_nanos": 1,
                "serial_equivalent_work_nanos": 2,
            },
            "consensus": {},
            "feedback": {
                "candidate_misses": 4,
                "fallback_edges_created": 0,
            },
            "adaptive_state": {
                "candidate_miss_history_relationships": 1,
                "runtime_fallback_relationships": 0,
            },
        }
        with tempfile.TemporaryDirectory() as temp:
            records = Path(temp) / "records.jsonl"
            records.write_text(json.dumps(record) + "\n", encoding="utf-8")
            result = subprocess.run(
                [sys.executable, str(V1_VALIDATOR), str(records), "--allow-partial"],
                text=True,
                capture_output=True,
            )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("recovered_records=1/1", result.stdout)
        self.assertIn("miss_history_max=1", result.stdout)
        self.assertIn("fallback_max=0", result.stdout)

    def test_v1_candidate_miss_policy_rejects_input_resolved_misses(self):
        record = {
            "metadata": {
                "experiment_id": "conflictlab-v1-core-state",
                "mode": "static",
                "seed": 11,
                "run_index": 1,
                "parameters": {
                    "operation_mix": "credit",
                    "prediction_fault_mode": "none",
                },
            },
            "feedback": {"candidate_misses": 1, "fallback_edges_created": 1},
            "adaptive_state": {"candidate_miss_history_relationships": 1},
        }
        with self.assertRaisesRegex(ValueError, "unexpected input-resolved candidate misses"):
            validate_candidate_miss_policy([record])

    def test_v1_candidate_miss_policy_keeps_fine_point_mixed_strict(self):
        record = {
            "metadata": {
                "experiment_id": "conflictlab-v1-symbolic-granularity",
                "mode": "probability-only",
                "seed": 11,
                "run_index": 3,
                "parameters": {
                    "operation_mix": "point-mixed",
                    "prediction_fault_mode": "none",
                    "symbolic_granularity": "fine",
                },
            },
            "feedback": {"candidate_misses": 1, "fallback_edges_created": 1},
            "adaptive_state": {"candidate_miss_history_relationships": 1},
        }
        with self.assertRaisesRegex(ValueError, "unexpected input-resolved candidate misses"):
            validate_candidate_miss_policy([record])

    def test_v1_candidate_miss_policy_requires_recovery_evidence_for_state_derived_misses(self):
        record = {
            "metadata": {
                "experiment_id": "conflictlab-v1-execution-semantics",
                "mode": "cost-aware",
                "seed": 23,
                "run_index": 5,
                "parameters": {
                    "operation_mix": "stateful-mixed",
                    "prediction_fault_mode": "none",
                },
            },
            "feedback": {"candidate_misses": 2, "fallback_edges_created": 0},
            "adaptive_state": {
                "candidate_miss_history_relationships": 0,
                "runtime_fallback_relationships": 0,
            },
        }
        with self.assertRaisesRegex(ValueError, "lacked fallback/miss-history recovery evidence"):
            validate_candidate_miss_policy([record])


    def test_v1_correctness_diagnostics_generate_filtered_manifests(self):
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)
            adaptation = out / "adaptation-transitions"
            execution = out / "execution-semantics"
            adaptation.mkdir()
            execution.mkdir()

            bad_run = {
                "workload": "conflictlab",
                "mode": "probability-only",
                "run_index": 1,
                "seed": 47,
                "workers": 6,
                "parameters": {
                    "operation_mix": "credit",
                    "transactions": "512",
                    "postchange_warmup_blocks": "8",
                },
            }
            good_run = {
                "workload": "conflictlab",
                "mode": "probability-only",
                "run_index": 2,
                "seed": 101,
                "workers": 6,
                "parameters": {
                    "operation_mix": "credit",
                    "transactions": "512",
                    "postchange_warmup_blocks": "0",
                },
            }
            miss_run = {
                "workload": "conflictlab",
                "mode": "probability-only",
                "run_index": 1,
                "seed": 47,
                "workers": 6,
                "parameters": {
                    "operation_mix": "point-mixed",
                    "transactions": "128",
                },
            }
            base_manifest = {
                "schema_version": 1,
                "record_schema_version": 3,
                "physical_core_limit": 6,
                "policy": {},
            }
            (adaptation / "manifest.json").write_text(
                json.dumps({
                    **base_manifest,
                    "experiment_id": "conflictlab-v1-adaptation-transitions",
                    "runs": [bad_run, good_run],
                }),
                encoding="utf-8",
            )
            (execution / "manifest.json").write_text(
                json.dumps({
                    **base_manifest,
                    "experiment_id": "conflictlab-v1-execution-semantics",
                    "runs": [miss_run],
                }),
                encoding="utf-8",
            )
            (adaptation / "acceptance.json").write_text(
                json.dumps({
                    "status": "correctness_failure",
                    "accepted_runs": 1,
                    "performance_regressions": 0,
                    "incomplete_runs": 0,
                    "configuration_errors": 0,
                    "correctness_failures": 1,
                }),
                encoding="utf-8",
            )
            (execution / "acceptance.json").write_text(
                json.dumps({
                    "status": "accepted",
                    "accepted_runs": 1,
                    "performance_regressions": 0,
                    "incomplete_runs": 0,
                    "configuration_errors": 0,
                    "correctness_failures": 0,
                }),
                encoding="utf-8",
            )

            def record(run, experiment_id, equivalent, misses=0):
                return {
                    "metadata": {"experiment_id": experiment_id, **run},
                    "correctness": {
                        "serial_equivalent": equivalent,
                        "canonical_state_digest": "same" if equivalent else "adaptive",
                        "serial_reference_digest": "same" if equivalent else "serial",
                    },
                    "feedback": {"candidate_misses": misses},
                    "adaptive_state": {"candidate_miss_history_relationships": 1 if misses else 0},
                }

            records = [
                record(bad_run, "conflictlab-v1-adaptation-transitions", False),
                record(good_run, "conflictlab-v1-adaptation-transitions", True),
                record(miss_run, "conflictlab-v1-execution-semantics", True, misses=4),
            ]
            (out / "records.jsonl").write_text(
                "".join(json.dumps(item) + "\n" for item in records),
                encoding="utf-8",
            )

            subprocess.run(
                [sys.executable, str(V1_CORRECTNESS_DIAG), str(out)],
                check=True,
                stdout=subprocess.DEVNULL,
            )
            diag = out / "correctness-diagnostics"
            self.assertTrue((diag / "incorrect-records.csv").is_file())
            self.assertTrue((diag / "failure-discrimination.csv").is_file())
            plan = json.loads((diag / "rerun-plan.json").read_text(encoding="utf-8"))
            self.assertEqual({item["reason"] for item in plan}, {
                "serial-non-equivalent",
                "unexpected-input-resolved-candidate-miss",
            })
            filtered = json.loads(
                (diag / "manifests/adaptation-transitions-incorrect.manifest.json").read_text(encoding="utf-8")
            )
            self.assertEqual([run["run_index"] for run in filtered["runs"]], [1])
            strict = json.loads(
                (diag / "manifests/execution-semantics-unexpected-miss.manifest.json").read_text(encoding="utf-8")
            )
            self.assertEqual([run["run_index"] for run in strict["runs"]], [1])



if __name__ == "__main__":
    unittest.main()
