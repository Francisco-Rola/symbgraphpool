import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "scripts/vegeta/evaluate-vegeta-s3-exact-followup.py"
SPEC = importlib.util.spec_from_file_location("vegeta_exact_followup", MODULE_PATH)
followup = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(followup)


def evm(owner: str, slot: int) -> str:
    return f"evm/{owner}/{slot:064x}"


def source_tx(index: int, tx_hash: str, reads=None, writes=None):
    return {
        "tx_index": index,
        "tx_hash": tx_hash,
        "reads": list(reads or []),
        "writes": list(writes or []),
        "gas_used": 1,
        "failed": False,
    }


def native_tx(index: int, tx_hash: str, accesses=None):
    return {
        "tx_index": index,
        "tx_hash": tx_hash,
        "source_failed": False,
        "execution_status": "committed",
        "accesses": list(accesses or []),
    }


def storage_access(kind: str, key: str, family="cw20-base", instance="cw20-base:a", action="transfer"):
    return {
        "kind": kind,
        "contract": "native-a",
        "key_hex": key,
        "family": family,
        "instance_id": instance,
        "semantic_action": action,
        "reverted": False,
    }


class ExactFollowupUnitTests(unittest.TestCase):
    def test_exact_mapping_coverage_recomputes_on_exact_conflicts(self):
        owner_a = "aa" * 20
        owner_b = "bb" * 20
        h = ["0x" + f"{i:064x}" for i in range(3)]
        block = {
            "block_number": 1,
            "transactions": [
                source_tx(0, h[0], writes=[evm(owner_a, 1)]),
                source_tx(1, h[1], reads=[evm(owner_a, 1)], writes=[evm(owner_b, 2)]),
                source_tx(2, h[2], reads=[evm(owner_b, 2)]),
            ],
        }
        catalog = {
            "instances": [
                {
                    "source_storage_owner": "0x" + owner_a,
                    "native_code_family": "cw20-base",
                    "ethereum_profile_family": "1" * 64,
                    "native_instance_id": "cw20-base:0x" + owner_a,
                }
            ]
        }
        report = followup.exact_mapping_coverage([block], catalog)
        self.assertEqual(report["total_unique_conflict_pairs"], 2)
        self.assertEqual(report["mapped_owner_unique_conflict_pairs"], 1)
        self.assertAlmostEqual(report["aggregate_coverage"], 0.5)
        self.assertAlmostEqual(report["median_conflict_bearing_block_coverage"], 0.5)

    def test_false_negative_classification_and_critical_path_credit(self):
        owner_a = "aa" * 20
        hashes = ["0x" + f"{i + 10:064x}" for i in range(4)]
        k1, k2, k3 = evm(owner_a, 1), evm(owner_a, 2), evm(owner_a, 3)
        source = {
            "block_number": 7,
            "transactions": [
                source_tx(0, hashes[0], writes=[k1]),
                source_tx(1, hashes[1], reads=[k1], writes=[k2]),
                source_tx(2, hashes[2], reads=[k2], writes=[k3]),
                source_tx(3, hashes[3], reads=[k3]),
            ],
        }
        # Native execution reproduces the first and last dependency but misses the middle one.
        native = {
            "block_number": 7,
            "transactions": [
                native_tx(0, hashes[0], [storage_access("storage_write", "01")]),
                native_tx(1, hashes[1], [storage_access("storage_read", "01")]),
                native_tx(2, hashes[2], [storage_access("storage_write", "03")]),
                native_tx(3, hashes[3], [storage_access("storage_read", "03")]),
            ],
        }
        catalog = {
            "instances": [
                {
                    "source_storage_owner": "0x" + owner_a,
                    "native_code_family": "cw20-base",
                    "ethereum_profile_family": "2" * 64,
                    "native_instance_id": "cw20-base:0x" + owner_a,
                }
            ]
        }
        plan = [
            {
                "block_number": 7,
                "transactions": [
                    {
                        "native_actions": [
                            {
                                "storage_context_address": "0x" + owner_a,
                                "ethereum_profile_family": "2" * 64,
                            }
                        ]
                    }
                ],
            }
        ]
        result = followup.false_negative_diagnostics([source], [native], catalog, plan, top=10)
        self.assertEqual(result["false_negative_pairs"], 1)
        self.assertEqual(result["false_negative_edges_on_any_source_longest_path"], 1)
        self.assertEqual(result["by_coverage_class"][0]["label"], "mapped-owner-semantic-gap")
        self.assertEqual(result["by_coverage_class"][0]["critical_path_pair_credit"], 1.0)
        self.assertEqual(result["top_blocks_by_critical_path_gap"][0]["critical_path_gap"], 2)

    def test_primary_topology_ignores_native_bank_ledger(self):
        owner = "aa" * 20
        hashes = ["0x" + f"{i + 40:064x}" for i in range(2)]
        source = {
            "block_number": 12,
            "transactions": [source_tx(0, hashes[0]), source_tx(1, hashes[1])],
        }
        native = {
            "block_number": 12,
            "transactions": [
                native_tx(
                    0,
                    hashes[0],
                    [{"kind": "bank_write", "key_hex": "alice/usdc", "reverted": False}],
                ),
                native_tx(
                    1,
                    hashes[1],
                    [{"kind": "bank_read", "key_hex": "alice/usdc", "reverted": False}],
                ),
            ],
        }
        result = followup.compute_topology_metrics([source], [native])
        self.assertEqual(result["conflict_pairs"]["native"], 0)
        self.assertEqual(result["conflict_pairs"]["false_positive"], 0)
        full_rows = followup.native_rows(native, include_bank=True)
        self.assertEqual(followup.pairs_from_rw(full_rows), {(0, 1)})

    def test_exact_manifest_fails_closed_on_prestate_only_semantics(self):
        with self.assertRaisesRegex(ValueError, "does not advertise"):
            followup.validate_exact_manifest(
                {
                    "trace_mode": "public-rpc",
                    "access_semantics": "evm-storage-prestate-touched+state-changing-writes-v1",
                }
            )

    def test_source_trace_ablation_exposes_exact_only_conflict(self):
        owner = "aa" * 20
        hashes = ["0x" + f"{i + 20:064x}" for i in range(3)]
        exact = {
            "block_number": 9,
            "transactions": [
                source_tx(0, hashes[0], writes=[evm(owner, 1)]),
                source_tx(1, hashes[1], reads=[evm(owner, 1)], writes=[evm(owner, 2)]),
                source_tx(2, hashes[2], reads=[evm(owner, 2)]),
            ],
        }
        public = {
            "block_number": 9,
            "transactions": [
                source_tx(0, hashes[0], writes=[evm(owner, 1)]),
                source_tx(1, hashes[1], reads=[evm(owner, 1)]),
                source_tx(2, hashes[2]),
            ],
        }
        result = followup.source_trace_ablation([exact], [public])
        self.assertEqual(result["exact"]["conflict_pairs"], 2)
        self.assertEqual(result["public_prestate"]["conflict_pairs"], 1)
        self.assertEqual(result["exact_only_conflict_pairs"], 1)
        self.assertEqual(result["public_only_conflict_pairs"], 0)
        self.assertAlmostEqual(result["public_recall_against_exact"], 0.5)

    def test_gate_recheck_uses_exact_mapping_and_finalized_semantic_metrics(self):
        mapping = {
            "aggregate_coverage": 0.96,
            "median_conflict_bearing_block_coverage": 0.81,
        }
        finalized = {
            "simulation": {
                "semantic_transaction_coverage": 0.76,
                "semantic_call_frame_coverage": 0.51,
            }
        }
        gates = {
            "gates": {
                "aggregate_source_conflict_coverage": {"minimum": 0.95, "enforced": True},
                "median_conflict_bearing_block_coverage": {"minimum": 0.80, "enforced": True},
                "semantic_transaction_coverage": {"minimum": 0.75, "enforced": True},
                "semantic_call_frame_coverage": {"minimum": 0.50, "enforced": True},
            }
        }
        result = followup.evaluate_gates(mapping, finalized, gates)
        self.assertTrue(result["accepted"])
        mapping["aggregate_coverage"] = 0.949
        result = followup.evaluate_gates(mapping, finalized, gates)
        self.assertFalse(result["accepted"])
        failed = [row["name"] for row in result["gates"] if not row["passed"]]
        self.assertEqual(failed, ["aggregate_source_conflict_coverage"])

    def test_fallback_sensitivity_removes_transaction_from_both_graphs(self):
        owner = "aa" * 20
        hashes = ["0x" + f"{i + 30:064x}" for i in range(3)]
        source = {
            "block_number": 11,
            "transactions": [
                source_tx(0, hashes[0], writes=[evm(owner, 1)]),
                source_tx(1, hashes[1], reads=[evm(owner, 1)], writes=[evm(owner, 2)]),
                source_tx(2, hashes[2], reads=[evm(owner, 2)]),
            ],
        }
        native = {
            "block_number": 11,
            "transactions": [
                native_tx(0, hashes[0], [storage_access("storage_write", "01")]),
                native_tx(
                    1,
                    hashes[1],
                    [storage_access("storage_read", "01"), storage_access("storage_write", "02")],
                ),
                native_tx(2, hashes[2], [storage_access("storage_read", "02")]),
            ],
        }
        baseline = followup.compute_topology_metrics([source], [native])
        without = followup.compute_topology_metrics([source], [native], {hashes[1]})
        self.assertEqual(baseline["conflict_pairs"]["source"], 2)
        self.assertEqual(without["transactions"], 2)
        self.assertEqual(without["conflict_pairs"]["source"], 0)
        self.assertEqual(without["conflict_pairs"]["native"], 0)
        details = followup.fallback_transaction_details([source], [native], {hashes[1]})
        self.assertEqual(details[0]["source_incident_conflict_pairs"], 2)
        self.assertGreaterEqual(details[0]["source_incident_longest_path_edges"], 1)


class ExactFollowupEndToEndTests(unittest.TestCase):
    def test_evaluate_writes_report_and_csvs(self):
        owner = "aa" * 20
        hashes = ["0x" + f"{i + 100:064x}" for i in range(3)]
        k1 = evm(owner, 1)
        k2 = evm(owner, 2)
        exact_block = {
            "block_number": 101,
            "transactions": [
                source_tx(0, hashes[0], writes=[k1]),
                source_tx(1, hashes[1], reads=[k1], writes=[k2]),
                source_tx(2, hashes[2], reads=[k2]),
            ],
        }
        public_block = {
            "block_number": 101,
            "transactions": [
                source_tx(0, hashes[0], writes=[k1]),
                source_tx(1, hashes[1], reads=[k1]),
                source_tx(2, hashes[2]),
            ],
        }
        native_block = {
            "block_number": 101,
            "transactions": [
                native_tx(0, hashes[0], [storage_access("storage_write", "01")]),
                native_tx(1, hashes[1], [storage_access("storage_read", "01")]),
                native_tx(2, hashes[2]),
            ],
        }
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            exact_dir = root / "exact"
            exact_dir.mkdir()
            exact_path = exact_dir / "corpus.jsonl"
            exact_path.write_text(json.dumps(exact_block) + "\n")
            (exact_dir / "manifest.json").write_text(
                json.dumps(
                    {
                        "trace_mode": "custom-js-tx",
                        "access_semantics": "evm-storage-sload-sstore-v1+explicit-fallback-exceptions",
                        "trace_semantics_exceptions": [
                            {"tx_hash": hashes[2], "trace_mode": "public-rpc"}
                        ],
                    }
                )
            )
            public_path = root / "public.jsonl"
            public_path.write_text(json.dumps(public_block) + "\n")
            native_path = root / "native.jsonl"
            native_path.write_text(json.dumps(native_block) + "\n")
            plan_path = root / "plan.jsonl"
            plan_path.write_text(
                json.dumps(
                    {
                        "block_number": 101,
                        "transactions": [
                            {
                                "native_actions": [
                                    {
                                        "storage_context_address": "0x" + owner,
                                        "ethereum_profile_family": "3" * 64,
                                    }
                                ]
                            }
                        ],
                    }
                )
                + "\n"
            )
            catalog_path = root / "catalog.json"
            catalog_path.write_text(
                json.dumps(
                    {
                        "instances": [
                            {
                                "source_storage_owner": "0x" + owner,
                                "native_code_family": "cw20-base",
                                "ethereum_profile_family": "3" * 64,
                                "native_instance_id": "cw20-base:0x" + owner,
                            }
                        ]
                    }
                )
            )
            coverage_path = root / "coverage.json"
            coverage_path.write_text(
                json.dumps(
                    {
                        "transaction_semantic_coverage": {"semantic_transaction_coverage": 0.1},
                        "calls": {"semantic_frame_coverage": 0.1},
                    }
                )
            )
            simulation_path = root / "final-mapping-simulation.json"
            simulation_path.write_text(
                json.dumps(
                    {
                        "simulation": {
                            "semantic_transaction_coverage": 1.0,
                            "semantic_call_frame_coverage": 1.0,
                        }
                    }
                )
            )
            gates_path = root / "gates.json"
            gates_path.write_text(
                json.dumps(
                    {
                        "gates": {
                            "aggregate_source_conflict_coverage": {"minimum": 0.5, "enforced": True},
                            "median_conflict_bearing_block_coverage": {"minimum": 0.5, "enforced": True},
                            "semantic_transaction_coverage": {"minimum": 0.5, "enforced": True},
                            "semantic_call_frame_coverage": {"minimum": 0.5, "enforced": True},
                        }
                    }
                )
            )
            out = root / "out"
            report = followup.evaluate(
                exact_corpus=exact_path,
                public_corpus=public_path,
                native_accesses=native_path,
                native_plan=plan_path,
                instance_catalog=catalog_path,
                translation_coverage_path=coverage_path,
                final_mapping_simulation_path=simulation_path,
                gates_path=gates_path,
                exact_manifest_path=None,
                output_dir=out,
                top=10,
            )
            self.assertTrue(report["frozen_gate_recheck"]["accepted"])
            self.assertEqual(
                report["frozen_gate_recheck"]["semantic_measurement_source"],
                "final-mapping-simulation",
            )
            self.assertEqual(report["fallback_sensitivity"]["fallback_hashes"], [hashes[2]])
            for name in (
                "exact-fidelity-followup.json",
                "exact-fidelity-followup.txt",
                "exact-fidelity-fn-ranking.csv",
                "exact-fidelity-hot-keys.csv",
                "exact-fidelity-critical-blocks.csv",
                "exact-fidelity-mapping-per-block.csv",
            ):
                self.assertTrue((out / name).exists(), name)
            text = (out / "exact-fidelity-followup.txt").read_text()
            self.assertIn("Exact SLOAD/SSTORE vs public prestateTracer", text)
            self.assertIn("overall frozen-gate status: PASS", text)


if __name__ == "__main__":
    unittest.main()
