import csv
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
GENERATOR = ROOT / "scripts" / "generate-manifest-matrix.py"
AGGREGATOR = ROOT / "scripts" / "aggregate-experiment.py"
V1_CACHE_CHECK = ROOT / "scripts" / "check-conflictlab-v1-campaign-cache.py"


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

    def test_phase3_matrix_covers_real_wasm_complexity_prediction_and_bypass_axes(self):
        source = ROOT / "evaluation/conflictlab/phase3-system.grid.json"
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "manifest.json"
            subprocess.run([sys.executable, str(GENERATOR), str(source), str(output)], check=True)
            manifest = json.loads(output.read_text(encoding="utf-8"))
        runs = manifest["runs"]
        self.assertEqual(len(runs), 864)
        self.assertEqual({int(run["parameters"]["sim.block_size"]) for run in runs}, {32, 128, 512})
        self.assertEqual({run["parameters"]["complexity"] for run in runs}, {"light", "medium", "heavy"})
        self.assertEqual({run["parameters"]["prediction_quality"] for run in runs}, {"exact", "bucketed"})
        self.assertEqual({run["parameters"]["acg.serial_bypass_enabled"] for run in runs}, {"false", "true"})
        self.assertEqual({run["parameters"]["acg.risk_budget"] for run in runs}, {"0.50", "0.90"})
        self.assertTrue(all(run["parameters"]["execution_backend"] == "wasm" for run in runs))
        self.assertTrue(all(run["parameters"]["warmup_blocks"] == "4" for run in runs))
        self.assertTrue(all(int(run["parameters"]["transactions"]) == int(run["parameters"]["sim.block_size"]) for run in runs))

    def test_phase3_exploration_matrix_keeps_complexity_as_an_axis(self):
        source = ROOT / "evaluation/conflictlab/phase3-exploration.grid.json"
        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp) / "manifest.json"
            subprocess.run([sys.executable, str(GENERATOR), str(source), str(output)], check=True)
            manifest = json.loads(output.read_text(encoding="utf-8"))
        runs = manifest["runs"]
        self.assertEqual(len(runs), 108)
        self.assertEqual({int(run["parameters"]["sim.block_size"]) for run in runs}, {32, 128, 512})
        self.assertEqual({run["parameters"]["complexity"] for run in runs}, {"light", "medium", "heavy"})
        self.assertEqual({run["parameters"]["acg.exploration_rate"] for run in runs}, {"0.00", "0.05", "0.15"})
        self.assertTrue(all(run["parameters"]["prediction_quality"] == "bucketed" for run in runs))
        self.assertTrue(all(run["parameters"]["execution_backend"] == "wasm" for run in runs))

    def test_phase4_matrices_cover_reuse_mixed_complexity_and_targeted_exploration(self):
        expected = {
            "phase4-system.grid.json": 432,
            "phase4-vm-lifecycle.grid.json": 36,
            "phase4-mixed.grid.json": 216,
            "phase4-exploration.grid.json": 48,
        }
        manifests = {}
        for name, count in expected.items():
            source = ROOT / "evaluation/conflictlab" / name
            with tempfile.TemporaryDirectory() as temp:
                output = Path(temp) / "manifest.json"
                subprocess.run([sys.executable, str(GENERATOR), str(source), str(output)], check=True)
                manifest = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(len(manifest["runs"]), count)
            manifests[name] = manifest

        system = manifests["phase4-system.grid.json"]["runs"]
        self.assertEqual({run["parameters"]["complexity"] for run in system}, {"light", "medium", "heavy"})
        self.assertEqual({run["parameters"]["vm_instance_lifecycle"] for run in system}, {"reuse"})
        self.assertEqual({run["parameters"]["acg.serial_bypass_enabled"] for run in system}, {"false", "true"})

        lifecycle = manifests["phase4-vm-lifecycle.grid.json"]["runs"]
        self.assertEqual({run["parameters"]["vm_instance_lifecycle"] for run in lifecycle}, {"reuse", "recycle"})

        mixed = manifests["phase4-mixed.grid.json"]["runs"]
        self.assertEqual(
            {run["parameters"]["complexity_mix"] for run in mixed},
            {"80-15-5", "33-34-33", "10-30-60"},
        )

        exploration = manifests["phase4-exploration.grid.json"]["runs"]
        settings = {
            (
                run["parameters"]["acg.exploration_rate"],
                run["parameters"]["acg.exploration_min_uncertainty"],
                run["parameters"]["acg.exploration_max_transactions_per_block"],
            )
            for run in exploration
        }
        self.assertEqual(
            settings,
            {("0.00", "0.35", "0"), ("0.50", "0.50", "4"), ("0.50", "0.35", "8")},
        )

    def test_phase6_matrices_cover_real_wasm_cutoff_divergence_and_policy_axes(self):
        expected = {
            "phase6-feature-state.grid.json": 576,
            "phase6-consensus-cutoff.grid.json": 96,
            "phase6-serial-preexecution-cutoff.grid.json": 24,
            "phase6-consensus-divergence.grid.json": 192,
            "phase6-policy-sensitivity.grid.json": 48,
            "phase6-vm-lifecycle-sanity.grid.json": 16,
        }
        manifests = {}
        for name, count in expected.items():
            source = ROOT / "evaluation/conflictlab" / name
            with tempfile.TemporaryDirectory() as temp:
                output = Path(temp) / "manifest.json"
                subprocess.run([sys.executable, str(GENERATOR), str(source), str(output)], check=True)
                manifest = json.loads(output.read_text(encoding="utf-8"))
            self.assertEqual(len(manifest["runs"]), count)
            self.assertTrue(all(run["workers"] == 6 for run in manifest["runs"]))
            self.assertTrue(all(run["parameters"]["execution_backend"] == "wasm" for run in manifest["runs"]))
            if name == "phase6-vm-lifecycle-sanity.grid.json":
                self.assertEqual(
                    {run["parameters"]["vm_instance_lifecycle"] for run in manifest["runs"]},
                    {"reuse", "recycle"},
                )
            else:
                self.assertTrue(
                    all(run["parameters"]["vm_instance_lifecycle"] == "reuse" for run in manifest["runs"])
                )
            self.assertFalse(any(
                key.startswith("acg.exploration_")
                for run in manifest["runs"]
                for key in run["parameters"]
            ))
            manifests[name] = manifest

        feature = manifests["phase6-feature-state.grid.json"]["runs"]
        self.assertEqual({int(r["parameters"]["sim.block_size"]) for r in feature}, {32, 128, 512})
        self.assertEqual({r["parameters"]["complexity"] for r in feature}, {"low", "medium", "high", "mixed"})
        self.assertEqual({r["mode"] for r in feature}, {"static", "probability-only", "cost-aware"})
        self.assertEqual({r["parameters"]["prediction_quality"] for r in feature}, {"exact", "bucketed"})
        self.assertEqual({r["parameters"]["acg.serial_bypass_enabled"] for r in feature}, {"false", "true"})

        cutoff = manifests["phase6-consensus-cutoff.grid.json"]["runs"]
        self.assertEqual({r["parameters"]["consensus_cutoff_ms"] for r in cutoff}, {"250", "500", "1000"})

        divergence = manifests["phase6-consensus-divergence.grid.json"]["runs"]
        self.assertEqual(
            {r["parameters"]["consensus_divergence"] for r in divergence},
            {"identical", "tail-5pct", "tail-20pct", "reorder-5pct", "reorder-20pct", "tail-reorder-10pct"},
        )

        policy = manifests["phase6-policy-sensitivity.grid.json"]["runs"]
        self.assertEqual({r["parameters"]["acg.risk_budget"] for r in policy}, {"0.75", "0.90", "0.98"})

        lifecycle = manifests["phase6-vm-lifecycle-sanity.grid.json"]["runs"]
        self.assertEqual({r["parameters"]["complexity"] for r in lifecycle}, {"low", "medium", "high", "mixed"})
        self.assertEqual({r["parameters"]["vm_instance_lifecycle"] for r in lifecycle}, {"reuse", "recycle"})

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
                            "pre_reduction_dependencies": 0,
                            "scheduled_dependencies": 0,
                            "edges_elided_by_reduction": 0,
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
            source = ROOT / "evaluation/conflictlab" / name
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

        headlines = manifests["v1-statistical-headlines.grid.json"]["runs"]
        self.assertEqual(len({run["seed"] for run in headlines}), 20)
        soak = manifests["v1-long-run-soak.grid.json"]["runs"]
        self.assertTrue(all(run["parameters"]["warmup_blocks"] == "1000" for run in soak))


if __name__ == "__main__":
    unittest.main()
