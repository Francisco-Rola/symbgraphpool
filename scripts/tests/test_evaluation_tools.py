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
                            "observed_service_dag_bound_nanos": wall,
                            "observed_service_work_nanos": 2000,
                            "worker_capacity_bound_nanos": 1000,
                            "parallel_lower_bound_nanos": max(wall, 1000),
                            "service_inflation_milli": 1000,
                            "scheduler_realization_milli": 1000,
                            "scheduler_realization_corrected_milli": 1000,
                        },
                        "planning": {"total_nanos": 100},
                        "feedback_timing": {"total_nanos": 10},
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


if __name__ == "__main__":
    unittest.main()
