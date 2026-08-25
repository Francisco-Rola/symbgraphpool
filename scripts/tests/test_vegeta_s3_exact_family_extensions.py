import hashlib
import importlib.util
import json
import sys
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
VEGETA_DIR = ROOT / "scripts/vegeta"
if str(VEGETA_DIR) not in sys.path:
    sys.path.insert(0, str(VEGETA_DIR))


def load_module(name: str, rel: str):
    path = ROOT / rel
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


planner = load_module("vegeta_native_plan_builder_extension", "scripts/vegeta/build-native-s3-plan.py")
prepare = load_module("vegeta_native_execution_prepare_extension", "scripts/vegeta/prepare-native-s3-execution.py")
followup = load_module("vegeta_exact_followup_extension", "scripts/vegeta/evaluate-vegeta-s3-exact-followup.py")
result_validator = load_module("vegeta_exact_family_result_validator", "scripts/vegeta/validate-vegeta-s3-exact-family-extension-results.py")


class ExactFamilyExtensionTests(unittest.TestCase):
    def test_v2_family_map_preserves_v1_and_adds_four_reviewed_marketplace_profiles(self):
        v1 = json.loads((ROOT / "evaluation/vegeta/s3-native-family-map.v1.json").read_text())
        v2 = json.loads((ROOT / "evaluation/vegeta/s3-native-family-map.v2.json").read_text())
        planner.validate_frozen_map(v1)
        planner.validate_frozen_map(v2)
        self.assertEqual(len(v1["profile_mappings"]), 11)
        self.assertEqual(len(v2["profile_mappings"]), 15)
        self.assertEqual(len(v2["native_code_families"]), 8)
        old_profiles = {row["ethereum_profile_family"] for row in v1["profile_mappings"]}
        new_rows = [row for row in v2["profile_mappings"] if row["ethereum_profile_family"] not in old_profiles]
        self.assertEqual(len(new_rows), 4)
        self.assertEqual({row["native_code_family"] for row in new_rows}, {"marketplace-router"})
        self.assertTrue(all(row.get("exact_ground_truth_extension") is True for row in new_rows))
        self.assertEqual(v2["target_conflict_coverage"], v1["target_conflict_coverage"])

    def test_reviewed_owner_scope_overrides_stale_proxy_resolution_profile(self):
        v2 = json.loads((ROOT / "evaluation/vegeta/s3-native-family-map.v2.json").read_text())
        owner = "0x000000000000ad05ccc4f10045630fb830b95127"
        mapping_candidates = {
            "resolution_records": [
                {
                    "storage_owner": owner,
                    "recommended_profile_family": "f" * 64,
                }
            ]
        }
        resolver = planner.FamilyResolver(v2, {}, mapping_candidates)
        profile, family = resolver.native_family_for_storage_context(owner)
        self.assertEqual(
            profile,
            "593f1950bcc35516cc41b8c5e012f883050182a10ab79bbdcb59acc58d11432f",
        )
        self.assertEqual(family, "marketplace-router")

    def test_extension_evidence_has_exactly_the_four_dominant_reviewed_owners_and_no_trace_keys(self):
        path = ROOT / "evaluation/vegeta/s3-exact-family-extension.v1.json"
        text = path.read_text()
        doc = json.loads(text)
        owners = {row["storage_owner"] for row in doc["extensions"]}
        self.assertEqual(
            owners,
            {
                "0x00000000000001ad428e4906ae43d8f9852d0dd6",
                "0xef1c6e67703c7bd7107eed8303fbe6ec2554bf6b",
                "0x000000000000ad05ccc4f10045630fb830b95127",
                "0x00000000006c3852cbef3e08e8df289169ede581",
            },
        )
        self.assertFalse(doc["selection_policy"]["historical_concrete_storage_keys_used"])
        self.assertFalse(doc["selection_policy"]["performance_results_used"])
        self.assertNotIn("evm/", text)
        self.assertNotIn("actual_reads", text)
        self.assertNotIn("actual_writes", text)

    def test_only_source_reviewed_marketplace_selectors_are_semantic(self):
        entries = planner.ENTRYPOINTS["marketplace-router"]
        self.assertEqual(entries["0x3593564c"][0], "execute::execute_route")
        self.assertEqual(entries["0xfa461e33"][0], "execute::v3_swap_callback")
        self.assertEqual(entries["0xe04d94ae"][0], "execute::blur_settle")
        self.assertEqual(entries["0xfd9f1e10"][0], "execute::cancel_order")
        self.assertEqual(entries["0x88147732"][0], "execute::validate_order")
        for opaque in ("0x9c7bf938", "0xab7e8cba", "0xf4acd740", "0x46423aa7"):
            self.assertNotIn(opaque, entries)

    def test_marketplace_translation_uses_public_calldata_fingerprint_not_storage_key(self):
        data_a = "0x3593564c" + "11" * 64
        data_b = "0x3593564c" + "22" * 64
        self.assertNotEqual(prepare.calldata_fingerprint(data_a), prepare.calldata_fingerprint(data_b))
        self.assertTrue(prepare.calldata_fingerprint(data_a).startswith("calldata-sha256:"))
        action = {
            "action_id": 1,
            "selector": "0x3593564c",
            "ethereum_input": data_a,
            "storage_context_address": "0x" + "ab" * 20,
        }
        call = prepare.translate(
            "marketplace-router",
            "execute::execute_route",
            None,
            {},
            action,
            "0x" + "cd" * 20,
            prepare.TokenIdRemapper(),
        )
        self.assertEqual(call["family"], "marketplace-router")
        route_id = call["msg"]["execute_route"]["route_id"]
        self.assertEqual(route_id, prepare.calldata_fingerprint(data_a))
        self.assertNotIn("evm/", json.dumps(call))

    def test_wrapped_native_denom_is_immutable_and_symbolic_profile_has_no_denom_resource(self):
        source_path = ROOT / "benchmarks/contracts/native-s3/wrapped-native-token/src/lib.rs"
        source = source_path.read_text()
        self.assertIn('pub const NATIVE_DENOM: &str = "unative";', source)
        self.assertNotIn('Item::new("denom")', source)
        self.assertNotIn("DENOM.load", source)
        self.assertNotIn("DENOM.save", source)
        symbolic = json.loads(
            (ROOT / "benchmarks/symbolic/native-s3/wrapped-native-token.symbolic.json").read_text()
        )
        self.assertNotIn("DENOM", symbolic["storage_resources"])
        self.assertEqual(
            symbolic["analysis_provenance"]["source_sha256"],
            hashlib.sha256(source_path.read_bytes()).hexdigest(),
        )
        total_supply = next(p for p in symbolic["profiles"] if p["entrypoint"] == "query::TotalSupply")
        self.assertEqual(total_supply["accesses"], [])

    def test_marketplace_symbolic_profile_matches_source_and_has_no_artificial_total_fulfilled(self):
        source_path = ROOT / "benchmarks/contracts/native-s3/marketplace-router/src/lib.rs"
        symbolic = json.loads(
            (ROOT / "benchmarks/symbolic/native-s3/marketplace-router.symbolic.json").read_text()
        )
        self.assertNotIn("TOTAL_FULFILLED", source_path.read_text())
        self.assertNotIn("TOTAL_FULFILLED", symbolic["storage_resources"])
        self.assertEqual(
            symbolic["analysis_provenance"]["source_sha256"],
            hashlib.sha256(source_path.read_bytes()).hexdigest(),
        )
        names = {profile["entrypoint"] for profile in symbolic["profiles"]}
        for required in (
            "execute::SettleOrder",
            "execute::ValidateOrder",
            "execute::CancelOrder",
            "execute::ExecuteRoute",
            "execute::V3SwapCallback",
            "execute::BlurSettle",
        ):
            self.assertIn(required, names)

    def test_exact_followup_semantic_gates_ignore_stale_translation_coverage_values(self):
        mapping = {
            "aggregate_coverage": 0.96,
            "median_conflict_bearing_block_coverage": 0.81,
        }
        final = {
            "simulation": {
                "semantic_transaction_coverage": 0.768,
                "semantic_call_frame_coverage": 0.501,
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
        result = followup.evaluate_gates(mapping, final, gates)
        self.assertTrue(result["accepted"])
        self.assertEqual(result["semantic_measurement_source"], "final-mapping-simulation")
        self.assertAlmostEqual(result["measurements"]["semantic_transaction_coverage"], 0.768)
        self.assertAlmostEqual(result["measurements"]["semantic_call_frame_coverage"], 0.501)

    def test_result_validator_enforces_only_frozen_gates_owner_mapping_and_denom_removal(self):
        followup_report = {
            "frozen_gate_recheck": {
                "accepted": True,
                "semantic_measurement_source": "final-mapping-simulation",
            },
            "hot_key_diagnostics": {
                "native_ranked_keys": [
                    {
                        "key": "storage:contract-x:62616c616e636573",
                        "families": ["wrapped-native-token"],
                    }
                ]
            },
            "topology_baseline": {
                "conflict_pairs": {
                    "precision": 0.99, "recall": 0.8, "f1": 0.88,
                    "source": 10, "native": 8,
                },
                "critical_path": {"source_sum": 10, "native_sum": 9, "relative_error": 0.1},
                "hot_key_chain": {"source_sum": 7, "native_sum": 8, "relative_error": 1/7},
            },
        }
        catalog = {
            "instances": [
                {
                    "source_storage_owner": owner,
                    "native_code_family": "marketplace-router",
                }
                for owner in sorted(result_validator.REVIEWED_OWNERS)
            ]
        }
        result = result_validator.validate(followup_report, catalog)
        self.assertTrue(result["accepted"])
        self.assertIn("topology_report_only_no_new_threshold", result)

        followup_report["hot_key_diagnostics"]["native_ranked_keys"] = [
            {
                "key": "storage:contract-x:64656e6f6d",
                "families": ["wrapped-native-token"],
            }
        ]
        result = result_validator.validate(followup_report, catalog)
        self.assertFalse(result["accepted"])
        self.assertTrue(any("immutable denom" in error for error in result["errors"]))


if __name__ == "__main__":
    unittest.main()
