from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
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


cost = load_module("vegeta_cost_fidelity", "scripts/vegeta/analyze-native-s3-cost-fidelity.py")
scaling = load_module("vegeta_replay_scaling", "scripts/vegeta/summarize-native-s3-replay-scaling.py")


class CostFidelityTests(unittest.TestCase):
    def test_conflict_dag_includes_raw_war_and_waw(self):
        items = [
            (0, {"reads": set(), "writes": {"k"}, "steps": 10, "gas_used": 10}),
            (1, {"reads": {"k"}, "writes": set(), "steps": 20, "gas_used": 20}),
            (2, {"reads": set(), "writes": {"k"}, "steps": 30, "gas_used": 30}),
        ]
        preds = cost.block_dependencies(items)
        self.assertEqual(preds[0], set())
        self.assertEqual(preds[1], {0})
        self.assertEqual(preds[2], {0, 1})
        path, weight = cost.weighted_critical_path(items, preds, "steps")
        self.assertEqual(path, {0, 1, 2})
        self.assertEqual(weight, 60)

    def test_cost_fidelity_cli_joins_hashes_and_reports_overweight(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            traces = td / "traces" / "1"
            traces.mkdir(parents=True)
            txs = [
                (0, "0xaaa", 10, ["k"], [], 100_000),
                (1, "0xbbb", 90, ["k"], ["k"], 900_000),
                (2, "0xccc", 100, [], [], 100_000),
            ]
            for idx, h, steps, reads, writes, _ in txs:
                (traces / f"{idx:04}-{h[2:]}.json").write_text(json.dumps({
                    "tx_hash": h,
                    "result": {"gasUsed": steps + 21000, "steps": steps, "reads": reads, "writes": writes},
                }))
            native = td / "native.jsonl"
            native.write_text(json.dumps({"block_number": 1, "wasm_instance_lifecycle": "reuse", "transactions": [
                {"tx_index": idx, "tx_hash": h, "native_execution_nanos": ns, "semantic_calls": 1, "skipped_actions": 0}
                for idx, h, _, _, _, ns in txs
            ]}) + "\n")
            out = td / "out"
            subprocess.run([
                sys.executable, str(ROOT / "scripts/vegeta/analyze-native-s3-cost-fidelity.py"),
                "--native-accesses", str(native), "--source-traces-dir", str(td / "traces"),
                "--output-dir", str(out),
            ], check=True)
            summary = json.loads((out / "summary.json").read_text())
            self.assertEqual(summary["matched_transactions"], 3)
            self.assertEqual(summary["hash_mismatches"], 0)
            self.assertGreater(summary["critical_path_native_overweight_ratio"], 1.0)

    def test_cost_fidelity_allows_known_missing_source_and_excludes_affected_block(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            traces = td / "traces" / "1"
            traces.mkdir(parents=True)
            (traces / "0000-aaa.json").write_text(json.dumps({
                "tx_hash": "0xaaa",
                "result": {"gasUsed": 21010, "steps": 10, "reads": [], "writes": ["k"]},
            }))
            traces2 = td / "traces" / "2"
            traces2.mkdir(parents=True)
            (traces2 / "0000-ccc.json").write_text(json.dumps({
                "tx_hash": "0xccc",
                "result": {"gasUsed": 21020, "steps": 20, "reads": [], "writes": []},
            }))
            native = td / "native.jsonl"
            native.write_text(
                json.dumps({"block_number": 1, "wasm_instance_lifecycle": "reuse", "transactions": [
                    {"tx_index": 0, "tx_hash": "0xaaa", "native_execution_nanos": 100_000, "semantic_calls": 1, "skipped_actions": 0},
                    {"tx_index": 1, "tx_hash": "0xbbb", "native_execution_nanos": 200_000, "semantic_calls": 1, "skipped_actions": 0},
                ]}) + "\n" +
                json.dumps({"block_number": 2, "wasm_instance_lifecycle": "reuse", "transactions": [
                    {"tx_index": 0, "tx_hash": "0xccc", "native_execution_nanos": 300_000, "semantic_calls": 1, "skipped_actions": 0},
                ]}) + "\n"
            )
            out = td / "out"
            subprocess.run([
                sys.executable, str(ROOT / "scripts/vegeta/analyze-native-s3-cost-fidelity.py"),
                "--native-accesses", str(native), "--source-traces-dir", str(td / "traces"),
                "--output-dir", str(out), "--max-missing-source", "1",
            ], check=True)
            summary = json.loads((out / "summary.json").read_text())
            self.assertEqual(summary["missing_source_transactions"], 1)
            self.assertEqual(summary["critical_path_excluded_blocks"], [1])
            self.assertEqual(summary["steps_weighted_critical_path"]["critical_count"], 1)


class ReplayScalingSummaryTests(unittest.TestCase):
    def test_aggregate_replay_speedup_uses_full_range_totals(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            records = td / "records.jsonl"
            rows = []
            for block, serial, replay in [(1, 100, 50), (2, 300, 100)]:
                rows.append({
                    "workers": 4, "sample": 0, "strategy": "exact-direct", "block_number": block,
                    "transactions": 1, "matched_serial_nanos": serial, "strategy_total_nanos": replay,
                    "preexecution_nanos": 0, "reconciliation_nanos": replay, "post_consensus_nanos": replay,
                    "serial_equivalent": True,
                })
            records.write_text("".join(json.dumps(r) + "\n" for r in rows))
            out = td / "out"
            subprocess.run([
                sys.executable, str(ROOT / "scripts/vegeta/summarize-native-s3-replay-scaling.py"),
                "--records", str(records), "--output-dir", str(out),
            ], check=True)
            summary = json.loads((out / "summary.json").read_text())
            self.assertAlmostEqual(summary[0]["replay_speedup_median"], 400 / 150)
            self.assertTrue(summary[0]["serial_equivalent"])


class ComputeCalibrationToolTests(unittest.TestCase):
    def test_compute_weight_builder_allows_missing_and_preserves_source_cost(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            plan = td / "plan.jsonl"
            plan.write_text(json.dumps({
                "block_number": 1, "timestamp": 0, "transactions": [
                    {"tx_index": 0, "tx_hash": "0xaaa", "source_failed": False, "calls": []},
                    {"tx_index": 1, "tx_hash": "0xbbb", "source_failed": False, "calls": []},
                ]
            }) + "\n")
            traces = td / "traces" / "1"
            traces.mkdir(parents=True)
            (traces / "0000-aaa.json").write_text(json.dumps({
                "tx_hash": "0xaaa",
                "result": {"gasUsed": 21100, "steps": 123, "reads": [], "writes": []},
            }))
            weights = td / "weights.jsonl"
            summary = td / "summary.json"
            subprocess.run([
                sys.executable, str(ROOT / "scripts/vegeta/build-native-s3-compute-weights.py"),
                "--execution-plan", str(plan), "--source-traces-dir", str(td / "traces"),
                "--output", str(weights), "--summary", str(summary), "--max-missing-source", "1",
            ], check=True)
            rows = [json.loads(line) for line in weights.read_text().splitlines()]
            self.assertEqual(rows[0]["source_opcode_steps"], 123)
            self.assertEqual(rows[0]["source_gas_used"], 21100)
            self.assertTrue(rows[0]["source_trace_present"])
            self.assertFalse(rows[1]["source_trace_present"])
            self.assertIsNone(rows[1]["source_opcode_steps"])
            meta = json.loads(summary.read_text())
            self.assertEqual(meta["missing_source_transactions"], 1)
            self.assertEqual(meta["source_opcode_steps_total"], 123)

    def test_compute_sweep_summary_tracks_net_speedup_and_fidelity(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            records = td / "records.jsonl"
            rows = []
            for strategy, serial, total, replay in [
                ("serial", 100, 95, 0),
                ("exact-direct", 100, 50, 50),
            ]:
                rows.append({
                    "compute_calibration_metric": "steps", "compute_scale": 1.0,
                    "workers": 4, "sample": 0, "strategy": strategy, "block_number": 1,
                    "transactions": 1, "matched_serial_nanos": serial, "strategy_total_nanos": total,
                    "preexecution_nanos": 0, "reconciliation_nanos": replay,
                    "post_consensus_nanos": total, "serial_equivalent": True,
                })
            records.write_text("".join(json.dumps(row) + "\n" for row in rows))
            profiles = td / "profiles" / "steps-1" / "cost-fidelity"
            profiles.mkdir(parents=True)
            (profiles / "summary.json").write_text(json.dumps({
                "compute_calibration": {"metric": "steps", "scale": 1.0},
                "correlation": {
                    "native_vs_steps_pearson": 0.8, "native_vs_steps_spearman": 0.9,
                    "native_vs_gas_pearson": 0.7, "native_vs_gas_spearman": 0.75,
                },
                "critical_path_native_overweight_ratio": 1.05,
                "native_execution_us": {"median": 150.0},
                "compute_iterations_total": 1234,
                "missing_source_transactions": 0,
            }))
            out = td / "out"
            subprocess.run([
                sys.executable, str(ROOT / "scripts/vegeta/summarize-native-s3-compute-sweep.py"),
                "--records", str(records), "--profiles-root", str(td / "profiles"),
                "--output-dir", str(out),
            ], check=True)
            scaling = json.loads((out / "scaling-summary.json").read_text())
            direct = next(row for row in scaling if row["strategy"] == "exact-direct")
            self.assertAlmostEqual(direct["active_speedup_median"], 2.0)
            self.assertAlmostEqual(direct["normalized_active_speedup_median"], 2.0 / (100 / 95))
            fidelity = json.loads((out / "fidelity-summary.json").read_text())
            self.assertEqual(fidelity[0]["critical_path_native_overweight_ratio"], 1.05)


class RuntimeConcurrencyProfileTests(unittest.TestCase):
    def test_runtime_profile_summary_reports_effective_concurrency_and_lock_shares(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            records = td / "records.jsonl"
            rows = [
                {
                    "compute_calibration_metric": "steps", "compute_scale": 4.0,
                    "workers": 4, "sample": 0, "strategy": "serial", "block_number": 1,
                    "transactions": 2, "matched_serial_nanos": 1000, "strategy_total_nanos": 1000,
                },
                {
                    "compute_calibration_metric": "steps", "compute_scale": 4.0,
                    "workers": 4, "sample": 0, "strategy": "exact-direct", "block_number": 1,
                    "transactions": 2, "matched_serial_nanos": 1000, "strategy_total_nanos": 500,
                    "runtime_profile": {
                        "profile_kind": "exact-direct",
                        "worker_phase_wall_nanos": 400,
                        "aggregate_ready_wait_nanos": 200,
                        "aggregate_transaction_service_nanos": 800,
                        "aggregate_request_execution_nanos": 700,
                        "aggregate_wasm_instance_acquire_nanos": 70,
                        "aggregate_wasm_entrypoint_nanos": 500,
                        "aggregate_host_storage_nanos": 140,
                        "aggregate_host_query_nanos": 0,
                        "aggregate_transaction_lock_wait_nanos": 35,
                        "aggregate_canonical_state_read_lock_wait_nanos": 14,
                        "aggregate_canonical_state_read_hold_nanos": 28,
                        "aggregate_mvcc_lock_wait_nanos": 0,
                        "aggregate_mvcc_publish_nanos": 0,
                        "aggregate_commit_lock_wait_nanos": 5,
                        "aggregate_commit_lock_hold_nanos": 25,
                        "commit_batches": 1, "commit_write_sets": 2, "max_in_flight": 2,
                        "wasm_instance_acquires": 10, "wasm_instance_reuse_hits": 9,
                        "wasm_instance_pool_misses": 1, "canonical_state_reads": 20,
                    },
                },
            ]
            records.write_text("".join(json.dumps(row) + "\n" for row in rows))
            out = td / "out"
            subprocess.run([
                sys.executable, str(ROOT / "scripts/vegeta/summarize-native-s3-runtime-profile.py"),
                "--records", str(records), "--output-dir", str(out),
            ], check=True)
            summary = json.loads((out / "runtime-profile-summary.json").read_text())
            direct = next(row for row in summary if row["strategy"] == "exact-direct")
            self.assertAlmostEqual(direct["net_speedup"], 2.0)
            self.assertAlmostEqual(direct["effective_service_concurrency"], 2.0)
            self.assertAlmostEqual(direct["ready_wait_capacity_fraction"], 0.125)
            self.assertAlmostEqual(direct["canonical_read_wait_request_fraction"], 0.02)
            self.assertAlmostEqual(direct["wasm_reuse_hit_rate"], 0.9)


    def test_perf_parser_accepts_perf7_modifiers_and_units(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            raw = td / "perf.csv"
            raw.write_text(
                "200.000;msec;task-clock:u;200000000;100.00;CPUs utilized\n"
                "10;;context-switches:u;100000000;100.00;\n"
                "1000000;;cpu_core/cycles/u;100000000;100.00;\n"
                "2000000;;cpu_core/instructions/u;100000000;100.00;\n"
                "0.250;;seconds time elapsed;;;\n"
            )
            sys.path.insert(0, str(ROOT / "scripts/vegeta"))
            from perf_stat import perf_health
            h = perf_health(raw, 250.0)
            self.assertTrue(h["working"])
            self.assertAlmostEqual(h["task_clock_ms"], 200.0)
            self.assertAlmostEqual(h["avg_cpus"], 0.8)
            self.assertAlmostEqual(h["ipc"], 2.0)

    def test_publication_matrix_summary_keeps_cosmos_scope_separate(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            native = td / "native.jsonl"
            base = {
                "workers":4,"sample":0,"block_number":1,"transactions":2,
                "matched_serial_nanos":1000,"post_consensus_nanos":1000,
                "replayed_transactions":0,"reused_receipts":0,"prepared_receipts":0,
                "serial_equivalent":True,
            }
            rows=[]
            for strategy,total,post in [("serial",950,950),("static",500,250),("exact-access",450,100)]:
                r=dict(base); r.update(strategy=strategy,strategy_total_nanos=total,post_consensus_nanos=post)
                rows.append(r)
            native.write_text("".join(json.dumps(r)+"\n" for r in rows))
            cosmos=td/"cosmos.jsonl"
            cosmos.write_text(json.dumps({
                "workers":4,"sample":0,"block_number":1,"transactions":2,
                "matched_serial_nanos":1000,"strategy_total_nanos":600,
                "serial_equivalent":True,"execution_attempts":3,"reexecutions":1,
                "baseline_scope":"actual-cosmos-sdk-blockstm-on-native-access-replay-not-cosmwasm-vm",
            })+"\n")
            out=td/"out"
            subprocess.run([sys.executable,str(ROOT/"scripts/vegeta/summarize-native-s3-publication-matrix.py"),
                "--native-records",str(native),"--cosmos-records",str(cosmos),"--output-dir",str(out)],check=True)
            obj=json.loads((out/"summary.json").read_text())
            static=next(r for r in obj["rows"] if r["strategy"]=="static")
            self.assertAlmostEqual(static["net_active_speedup"], (1000/500)/(1000/950))
            cosmos_row=next(r for r in obj["rows"] if r["strategy"]=="cosmos-block-stm-access-replay")
            self.assertIn("not-cosmwasm-vm", cosmos_row["scope"])
            self.assertAlmostEqual(static["active_wall_ms"], 0.0005)
            self.assertIn("post_p95_ms", static)

    def test_evaluation_readme_is_a_continuation_checkpoint(self):
        readme = (ROOT / "evaluation/vegeta/README.md").read_text()
        self.assertIn("Continuation rule", readme)
        self.assertIn("vegeta-s3-wasmd-wasm-genesis-params-hotfix.patch", readme)
        self.assertIn("Immediate next steps", readme)

    def test_runtime_profile_smoke_validation_is_strategy_aware(self):
        source = (ROOT / "scripts/run-vegeta-s3-runtime-concurrency-profile.sh").read_text()
        self.assertIn('requested={s.strip() for s in sys.argv[2].split', source)
        self.assertIn("if 'exact-access' in requested:", source)
        self.assertIn("if 'exact-direct' in requested:", source)

    def test_cosmos_blockstm_harness_uses_current_v054_go126_line(self):
        mod = (ROOT / "benchmarks/cosmos-blockstm-s3/go.mod").read_text()
        main = (ROOT / "benchmarks/cosmos-blockstm-s3/main.go").read_text()
        driver = (ROOT / "scripts/run-vegeta-s3-publication-matrix.sh").read_text()
        preflight = (ROOT / "scripts/check-vegeta-cosmos-blockstm.sh").read_text()
        self.assertIn("go 1.26.5", mod)
        self.assertIn("github.com/cosmos/cosmos-sdk v0.54.4", mod)
        self.assertIn('github.com/cosmos/cosmos-sdk/store/v2/types', main)
        self.assertIn('github.com/cosmos/cosmos-sdk/baseapp/txnrunner', main)
        self.assertNotIn('github.com/cosmos/cosmos-sdk/blockstm"', main)
        self.assertIn('txnrunner.NewSTMRunner', main)
        self.assertIn('const preEstimate = false', main)
        self.assertIn('actual-cosmos-sdk-txnrunner-blockstm-on-native-access-replay-not-cosmwasm-vm', main)
        self.assertIn('const cosmosSDKVersion = "v0.54.4"', main)
        self.assertIn("VEGETA_S3_COSMOS_GO_TOOLCHAIN", driver)
        self.assertIn("GOTOOLCHAIN", preflight)
        self.assertIn("go test ./...", preflight)

    def test_runtime_profile_driver_keeps_reuse_and_three_frozen_profiles(self):
        source = (ROOT / "scripts/run-vegeta-s3-runtime-concurrency-profile.sh").read_text()
        self.assertIn("VEGETA_S3_PROFILE_PROFILES", source)
        self.assertIn("none-0) run_profile none 0 none-0", source)
        self.assertIn("steps-4) run_profile steps 4 steps-4", source)
        self.assertIn("gas-4) run_profile gas 4 gas-4", source)
        benchmark = (ROOT / "runtime/crates/acg-vegeta-native-s3-executor/src/bin/acg-vegeta-native-s3-benchmark.rs").read_text()
        self.assertIn('wasm_instance_lifecycle:"reuse"', benchmark)
        self.assertIn('"--runtime-profile" => runtime_profile = true', benchmark)

    def test_publication_matrix_supports_wasmd_baseline_and_bootstrap_ci(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            native = td / "native.jsonl"
            native_rows=[]
            for sample in (0,1):
                for block in (1,2):
                    base={"workers":4,"sample":sample,"block_number":block,"transactions":2,
                          "matched_serial_nanos":1000+sample*100,"replayed_transactions":0,
                          "reused_receipts":0,"prepared_receipts":0,"serial_equivalent":True}
                    for strategy,total,post in (("serial",950+sample*50,950+sample*50),("static",600+sample*50,250+sample*20)):
                        r=dict(base); r.update(strategy=strategy,strategy_total_nanos=total,post_consensus_nanos=post); native_rows.append(r)
            native.write_text("".join(json.dumps(r)+"\n" for r in native_rows))
            wasmd=td/"wasmd.jsonl"
            wr=[]
            for sample in (0,1):
                for block in (1,2):
                    wr.append({"workers":4,"sample":sample,"block_number":block,"transactions":2,
                        "matched_serial_nanos":2000+sample*100,"strategy_total_nanos":1000+sample*50,
                        "serial_equivalent":True,"execution_attempts":2,"reexecutions":0,
                        "strategy":"cosmos-wasmd-block-stm",
                        "baseline_scope":"actual-wasmd-wasmvm-cosmos-sdk-txnrunner-blockstm-no-ante-abci"})
            wasmd.write_text("".join(json.dumps(r)+"\n" for r in wr))
            out=td/"out"
            subprocess.run([sys.executable,str(ROOT/"scripts/vegeta/summarize-native-s3-publication-matrix.py"),
                "--native-records",str(native),"--wasmd-records",str(wasmd),"--output-dir",str(out)],check=True)
            obj=json.loads((out/"summary.json").read_text())
            row=next(r for r in obj["rows"] if r["strategy"]=="cosmos-wasmd-block-stm")
            self.assertEqual(row["samples"],2)
            self.assertIsNotNone(row["active_wall_ms_ci95_low"])
            self.assertIn("wasmd-wasmvm",row["scope"])
            text=(out/"summary.txt").read_text()
            self.assertIn("p95-ms",text)
            self.assertIn("95% CIs",text)

    def test_publication_driver_has_paper_mode_and_full_wasmd_blockstm(self):
        driver=(ROOT/"scripts/run-vegeta-s3-publication-matrix.sh").read_text()
        preflight=(ROOT/"scripts/check-vegeta-cosmos-wasmd-blockstm.sh").read_text()
        mod=(ROOT/"benchmarks/cosmos-wasmd-blockstm-s3/go.mod").read_text()
        main=(ROOT/"benchmarks/cosmos-wasmd-blockstm-s3/main.go").read_text()
        self.assertIn('paper) DEFAULT_WORKERS="1,2,4,8,16"; DEFAULT_SAMPLES="5"',driver)
        self.assertIn("VEGETA_S3_PUBLICATION_COSMOS_WASMD_BLOCKSTM",driver)
        self.assertIn("--wasmd-records",driver)
        self.assertIn("github.com/CosmWasm/wasmd v0.70.3",mod)
        self.assertIn("github.com/cosmos/cosmos-sdk v0.54.4",mod)
        self.assertIn("wasmapp.NewWasmApp",main)
        self.assertRegex(main, r"NewWasmApp\(log\.NewNopLogger\(\), dbm\.NewMemDB\(\), true,")
        self.assertIn("loadLatest=true",main)
        self.assertIn("baseapp.SetChainID(chainID)",main)
        self.assertIn("benchmarkGenesisWithValidator",main)
        self.assertIn("wasmapp.GenesisStateWithValSet",main)
        self.assertIn("cmted25519.GenPrivKeyFromSecret",main)
        self.assertIn("sdk.DefaultPowerReduction.MulRaw(10)",main)
        self.assertIn('wasm "github.com/CosmWasm/wasmd/x/wasm"',main)
        self.assertIn("wasm.AppModuleBasic{}",main)
        self.assertIn("wasmBasic.DefaultGenesis(a.AppCodec())",main)
        self.assertIn("wasmkeeper.NewDefaultPermissionKeeper",main)
        self.assertIn("txnrunner.NewSTMRunner",main)
        self.assertIn("actual-wasmd-wasmvm-cosmos-sdk-txnrunner-blockstm-no-ante-abci",main)
        self.assertIn("--setup-only",preflight)


if __name__ == "__main__":
    unittest.main()
