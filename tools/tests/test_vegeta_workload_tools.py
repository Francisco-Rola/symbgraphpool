#!/usr/bin/env python3
from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
VEGETA_DIR = ROOT / "tools" / "vegeta"
sys.path.insert(0, str(VEGETA_DIR))

from vegeta_corpus import (  # noqa: E402
    WETH_MAINNET,
    compute_metrics,
    storage_contract,
    validate_shape,
)


def load_script(name: str):
    path = VEGETA_DIR / name
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), path)
    module = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(module)
    return module


extractor = load_script("extract-vegeta-ethereum.py")
validator = load_script("validate-vegeta-corpus.py")
characterizer = load_script("characterize-vegeta-corpus.py")
dossier_builder = load_script("build-native-family-dossier.py")
native_plan_builder = load_script("build-native-s3-plan.py")
native_plan_validator = load_script("validate-native-s3-plan.py")
background_gap_builder = load_script("build-native-background-gap.py")
final_map_builder = load_script("finalize-native-s3-map.py")
native_impl_validator = load_script("validate-native-s3-implementation.py")
native_execution_preparer = load_script("prepare-native-s3-execution.py")
native_fidelity = load_script("measure-native-s3-fidelity.py")


class VegetaCorpusTests(unittest.TestCase):
    def test_storage_contract_parses_canonical_key(self):
        key = f"evm/{WETH_MAINNET}/" + "01" * 32
        self.assertEqual(storage_contract(key), WETH_MAINNET)
        self.assertIsNone(storage_contract("not/a/canonical/key"))

    def test_metrics_match_paper_definition_of_longest_chain(self):
        weth_key = f"evm/{WETH_MAINNET}/" + "01" * 32
        other = "evm/" + "11" * 20 + "/" + "02" * 32
        blocks = [
            {
                "block_number": 10,
                "transactions": [
                    {"tx_index": 0, "reads": [weth_key], "writes": []},
                    {"tx_index": 1, "reads": [], "writes": [weth_key]},
                    {"tx_index": 2, "reads": [other], "writes": []},
                ],
            },
            {
                "block_number": 11,
                "transactions": [
                    {"tx_index": 0, "reads": [weth_key], "writes": []},
                    {"tx_index": 1, "reads": [weth_key], "writes": []},
                    {"tx_index": 2, "reads": [weth_key], "writes": []},
                ],
            },
        ]
        metrics = compute_metrics(blocks)
        self.assertEqual(metrics["transactions"], 6)
        # Vegeta defines each block's longest chain as the maximum number of
        # transactions that accessed any one key, then sums across blocks.
        self.assertEqual(metrics["longest_chain_sum"], 5)
        self.assertAlmostEqual(metrics["ratio"], 6 / 5)
        self.assertEqual(metrics["dominant_longest_chain_contract"], WETH_MAINNET)
        self.assertEqual(metrics["weth_longest_chain_contribution"], 5)

    def test_shape_rejects_non_contiguous_or_reordered_transactions(self):
        blocks = [
            {
                "block_number": 20,
                "transactions": [
                    {
                        "tx_index": 1,
                        "tx_hash": "0x1",
                        "from": "0x1",
                        "selector": "0x",
                        "reads": [],
                        "writes": [],
                    }
                ],
            }
        ]
        errors = validate_shape(blocks, 20, 21)
        self.assertTrue(any("exact contiguous range" in error for error in errors))
        self.assertTrue(any("tx index mismatch" in error for error in errors))


class ValidatorTests(unittest.TestCase):
    def test_canonical_s3_transaction_count_is_accepted_with_paper_warning(self):
        metrics = {
            "blocks": 101,
            "transactions": 13_783,
            "longest_chain_sum": 1_594,
            "ratio": 13_783 / 1_594,
            "dominant_longest_chain_contract": "dac17f958d2ee523a2206206994597c13d831ec7",
        }
        errors, warnings = validator.validate_metrics(metrics)
        self.assertEqual(errors, [])
        self.assertTrue(any("15129" in warning for warning in warnings))
        self.assertTrue(any("13783" in warning for warning in warnings))

    def test_paper_transaction_count_is_not_used_as_canonical_identity(self):
        metrics = {
            "blocks": 101,
            "transactions": 15_129,
            "longest_chain_sum": 1_779,
            "ratio": 8.50,
            "dominant_longest_chain_contract": WETH_MAINNET,
        }
        errors, _ = validator.validate_metrics(metrics)
        self.assertTrue(any("canonical Ethereum S3 range transaction count mismatch" in error for error in errors))



class ExtractorTests(unittest.TestCase):
    def test_build_block_record_preserves_order_and_deduplicates_accesses(self):
        block = {
            "number": "0x10",
            "hash": "0xABC",
            "timestamp": "0x20",
            "transactions": [
                {
                    "hash": "0xAAAA",
                    "from": "0xBBBB",
                    "to": "0xCCCC",
                    "input": "0x11223344deadbeef",
                    "value": "0x1",
                    "gas": "0x5208",
                }
            ],
        }
        traces = [
            {
                "txHash": "0xaaaa",
                "result": {
                    "reads": ["evm/a/1", "evm/a/1"],
                    "writes": ["evm/a/2"],
                    "steps": 17,
                    "gasUsed": 99,
                    "error": "",
                },
            }
        ]
        record = extractor.build_block_record(block, traces)
        self.assertEqual(record["block_number"], 16)
        self.assertEqual(record["timestamp"], 32)
        tx = record["transactions"][0]
        self.assertEqual(tx["tx_index"], 0)
        self.assertEqual(tx["selector"], "0x11223344")
        self.assertEqual(tx["reads"], ["evm/a/1"])
        self.assertEqual(tx["opcode_steps"], 17)

    def test_build_block_record_rejects_trace_count_mismatch(self):
        block = {"number": "0x10", "transactions": [{}]}
        with self.assertRaisesRegex(RuntimeError, "transactions but 0 traces"):
            extractor.build_block_record(block, [])

    def test_public_rpc_trace_combines_prestate_diff_and_receipt(self):
        address = "0x" + "ab" * 20
        slot1 = "0x1"
        slot2 = "0x2"
        slot3 = "0x3"
        tx_hash = "0x" + "11" * 32
        block = {
            "number": "0x10",
            "transactions": [{"hash": tx_hash}],
        }
        touched = [
            {
                "txHash": tx_hash,
                "result": {
                    address: {
                        "storage": {slot1: "0x01", slot2: "0x02"}
                    }
                },
            }
        ]
        diff = [
            {
                "txHash": tx_hash,
                "result": {
                    "pre": {address: {"storage": {slot2: "0x02"}}},
                    "post": {address: {"storage": {slot2: "0x04", slot3: "0x05"}}},
                },
            }
        ]
        receipts = [
            {
                "transactionHash": tx_hash,
                "gasUsed": "0x5208",
                "status": "0x1",
            }
        ]
        traces = extractor.build_public_trace_items(block, touched, diff, receipts)
        result = traces[0]["result"]
        prefix = "evm/" + "ab" * 20 + "/"
        self.assertEqual(
            result["reads"],
            [prefix + "1".rjust(64, "0"), prefix + "2".rjust(64, "0")],
        )
        self.assertEqual(
            result["writes"],
            [prefix + "2".rjust(64, "0"), prefix + "3".rjust(64, "0")],
        )
        self.assertEqual(result["gasUsed"], 21_000)
        self.assertEqual(result["steps"], 21_000)
        self.assertEqual(result["error"], "")

    def test_public_rpc_trace_marks_reverted_receipt_failed(self):
        tx_hash = "0x" + "22" * 32
        block = {"number": "0x11", "transactions": [{"hash": tx_hash}]}
        empty_trace = [{"txHash": tx_hash, "result": {}}]
        diff_trace = [{"txHash": tx_hash, "result": {"pre": {}, "post": {}}}]
        receipts = [{"transactionHash": tx_hash, "gasUsed": "0x10", "status": "0x0"}]
        traces = extractor.build_public_trace_items(block, empty_trace, diff_trace, receipts)
        self.assertEqual(traces[0]["result"]["error"], "reverted")

    def test_public_rpc_trace_rejects_mismatched_receipt(self):
        tx_hash = "0x" + "33" * 32
        block = {"number": "0x12", "transactions": [{"hash": tx_hash}]}
        touched = [{"txHash": tx_hash, "result": {}}]
        diff = [{"txHash": tx_hash, "result": {"pre": {}, "post": {}}}]
        receipts = [
            {
                "transactionHash": "0x" + "44" * 32,
                "gasUsed": "0x1",
                "status": "0x1",
            }
        ]
        with self.assertRaisesRegex(RuntimeError, "receipt hash"):
            extractor.build_public_trace_items(block, touched, diff, receipts)

    def test_public_rpc_manifest_records_conservative_semantics(self):
        access, source, compute = extractor.manifest_trace_metadata("public-rpc")
        self.assertIn("prestate-touched", access)
        self.assertIn("prestateTracer", source)
        self.assertEqual(compute, "gas_used")


    def test_custom_js_tx_manifest_records_exact_transaction_tracing(self):
        access, source, compute = extractor.manifest_trace_metadata("custom-js-tx")
        self.assertEqual(access, "evm-storage-sload-sstore-v1")
        self.assertIn("debug_traceTransaction", source)
        self.assertEqual(compute, "opcode_steps")

    def test_custom_js_tx_traces_in_block_order_and_checkpoints(self):
        class FakeClient:
            def __init__(self):
                self.calls = []
            def call(self, method, params):
                self.calls.append((method, params))
                tx_hash = params[0]
                return {
                    "reads": [f"evm/{'aa'*20}/{'01'*32}"],
                    "writes": [f"evm/{'aa'*20}/{'02'*32}"],
                    "steps": 10,
                    "gasUsed": 20,
                    "error": "",
                    "tx": tx_hash,
                }

        block = {
            "number": "0x64",
            "transactions": [
                {"hash": "0x" + "11" * 32},
                {"hash": "0x" + "22" * 32},
            ],
        }
        tracer = "{ result: function(){ return {}; } }"
        with tempfile.TemporaryDirectory() as tmp:
            cache = Path(tmp)
            client = FakeClient()
            traces = extractor.trace_block_custom_js_transactions(
                client, block, tracer, 60, cache_dir=cache, resume=True
            )
            self.assertEqual([x["txHash"] for x in traces], [x["hash"] for x in block["transactions"]])
            self.assertEqual(len(client.calls), 2)
            self.assertTrue(all(method == "debug_traceTransaction" for method, _ in client.calls))
            cached = sorted((cache / "100").glob("*.json"))
            self.assertEqual(len(cached), 2)
            first = json.loads(cached[0].read_text())
            self.assertEqual(
                first["tracer_sha256"],
                __import__("hashlib").sha256(tracer.encode("utf-8")).hexdigest(),
            )

            # A second run with the same tracer must be fully cache-backed.
            client2 = FakeClient()
            traces2 = extractor.trace_block_custom_js_transactions(
                client2, block, tracer, 60, cache_dir=cache, resume=True
            )
            self.assertEqual(traces2, traces)
            self.assertEqual(client2.calls, [])

    def test_custom_js_tx_cache_is_invalidated_when_tracer_changes(self):
        class FakeClient:
            def __init__(self):
                self.calls = 0
            def call(self, method, params):
                self.calls += 1
                return {"reads": [], "writes": [], "steps": self.calls, "gasUsed": 1, "error": ""}

        block = {"number": "0x1", "transactions": [{"hash": "0x" + "33" * 32}]}
        with tempfile.TemporaryDirectory() as tmp:
            cache = Path(tmp)
            first = FakeClient()
            extractor.trace_block_custom_js_transactions(
                first, block, "tracer-v1", 60, cache_dir=cache, resume=True
            )
            self.assertEqual(first.calls, 1)
            second = FakeClient()
            extractor.trace_block_custom_js_transactions(
                second, block, "tracer-v2", 60, cache_dir=cache, resume=True
            )
            self.assertEqual(second.calls, 1)

class CharacterizationTests(unittest.TestCase):
    def synthetic_blocks(self):
        a = "0x" + "aa" * 20
        b = "0x" + "bb" * 20
        storage_a = "evm/" + "aa" * 20 + "/" + "01" * 32
        storage_b = "evm/" + "bb" * 20 + "/" + "02" * 32
        return [
            {
                "block_number": 100,
                "transactions": [
                    {
                        "tx_index": 0,
                        "to": a,
                        "selector": "0x11111111",
                        "failed": False,
                        "gas_used": 10,
                        "reads": [storage_a],
                        "writes": [],
                    },
                    {
                        "tx_index": 1,
                        "to": a,
                        "selector": "0x22222222",
                        "failed": False,
                        "gas_used": 20,
                        "reads": [],
                        "writes": [storage_a],
                    },
                    {
                        "tx_index": 2,
                        "to": b,
                        "selector": "0x11111111",
                        "failed": True,
                        "gas_used": 30,
                        "reads": [storage_b],
                        "writes": [],
                    },
                ],
            },
            {
                "block_number": 101,
                "transactions": [
                    {
                        "tx_index": 0,
                        "to": a,
                        "selector": "0x11111111",
                        "failed": False,
                        "gas_used": 40,
                        "reads": [storage_a],
                        "writes": [storage_b],
                    },
                    {
                        "tx_index": 1,
                        "to": "<create>",
                        "selector": "0x",
                        "failed": False,
                        "gas_used": 50,
                        "reads": [],
                        "writes": [],
                    },
                ],
            },
        ]

    def test_characterization_counts_destinations_methods_storage_and_conflicts(self):
        report = characterizer.characterize_blocks(self.synthetic_blocks())
        self.assertEqual(report["corpus"]["transactions"], 5)
        self.assertEqual(report["corpus"]["contract_creations"], 1)
        self.assertEqual(report["direct_destinations"]["unique_addresses"], 2)
        self.assertEqual(report["methods"]["unique_destination_selector_pairs"], 3)
        self.assertEqual(report["selectors"]["unique_selectors"], 2)
        self.assertEqual(report["conflicts"]["total_unique_tx_pairs"], 1)

        destinations = report["direct_destinations"]["ranked"]
        self.assertEqual(destinations[0]["address"], "0x" + "aa" * 20)
        self.assertEqual(destinations[0]["transactions"], 3)
        self.assertEqual(destinations[0]["first_seen_block"], 100)

        storage = {entry["address"]: entry for entry in report["storage"]["ranked"]}
        self.assertEqual(storage["0x" + "aa" * 20]["transactions_touching_storage"], 3)
        self.assertEqual(storage["0x" + "aa" * 20]["conflict_pairs"], 1)
        self.assertEqual(storage["0x" + "bb" * 20]["transactions_touching_storage"], 2)

    def test_code_family_grouping_uses_runtime_bytecode_not_address(self):
        report = characterizer.characterize_blocks(self.synthetic_blocks())
        a = "0x" + "aa" * 20
        b = "0x" + "bb" * 20
        code = "0x6001600055"
        cache = {
            a: {"block_number": 100, "code": code},
            b: {"block_number": 100, "code": code},
        }
        families = characterizer.build_code_families(report, cache)
        self.assertEqual(families["unique_non_empty_code_families"], 1)
        family = [entry for entry in families["ranked"] if entry["family"] != "<empty-code>"][0]
        self.assertEqual(family["address_count"], 2)
        self.assertEqual(family["transactions"], 4)
        self.assertEqual(family["code_bytes"], 5)

    def test_eip1167_detection_extracts_embedded_implementation(self):
        implementation = "12" * 20
        code = "363d3d373d3d3d363d73" + implementation + "5af43d82803e903d91602b57fd5bf3"
        self.assertEqual(
            characterizer.detect_eip1167_implementation(code),
            "0x" + implementation,
        )

    def test_call_tracer_cache_is_block_scoped_hash_checked_and_resumable(self):
        address = "0x" + "aa" * 20
        block = {
            "block_number": 100,
            "block_hash": "0x" + "11" * 32,
            "transactions": [
                {
                    "tx_index": 0,
                    "tx_hash": "0x" + "22" * 32,
                    "to": address,
                    "reads": [],
                    "writes": [],
                }
            ],
        }

        class FakeClient:
            def __init__(self):
                self.calls = 0

            def call(self, method, params):
                self.calls += 1
                self.assert_method = method
                return [
                    {
                        "txHash": block["transactions"][0]["tx_hash"],
                        "result": {
                            "type": "CALL",
                            "from": "0x" + "bb" * 20,
                            "to": address,
                            "input": "0x11111111",
                            "calls": [],
                        },
                    }
                ]

        client = FakeClient()
        with tempfile.TemporaryDirectory() as tmp:
            cache_dir = Path(tmp) / "call-cache"
            first = characterizer.fetch_call_traces(
                [block], client, cache_dir, trace_timeout_seconds=60, reexec=128, delay_ms=0
            )
            self.assertEqual(client.calls, 1)
            self.assertEqual(first[100]["transactions"][0]["tx_hash"], block["transactions"][0]["tx_hash"])
            self.assertTrue((cache_dir / "100.json").exists())

            second = characterizer.fetch_call_traces(
                [block], client, cache_dir, trace_timeout_seconds=60, reexec=128, delay_ms=0
            )
            self.assertEqual(client.calls, 1, "second pass should reuse the validated block cache")
            self.assertEqual(second[100]["block_hash"], block["block_hash"])

    def test_call_summary_tracks_internal_selectors_and_delegatecall_edges(self):
        router = "0x" + "bb" * 20
        token = "0x" + "aa" * 20
        implementation = "0x" + "cc" * 20
        blocks = [{"block_number": 100, "transactions": [{"tx_hash": "0x1"}]}]
        call_blocks = {
            100: {
                "transactions": [
                    {
                        "tx_hash": "0x1",
                        "result": {
                            "type": "CALL",
                            "to": router,
                            "input": "0x12345678",
                            "calls": [
                                {
                                    "type": "CALL",
                                    "to": token,
                                    "input": "0xa9059cbb" + "00" * 64,
                                },
                                {
                                    "type": "DELEGATECALL",
                                    "to": implementation,
                                    "input": "0xdeadbeef",
                                },
                            ],
                        },
                    }
                ]
            }
        }
        summary = characterizer.summarize_call_traces(blocks, call_blocks)
        by_address = {item["address"]: item for item in summary["ranked"]}
        self.assertEqual(by_address[router]["root_invocations"], 1)
        self.assertEqual(by_address[token]["internal_invocations"], 1)
        self.assertEqual(by_address[token]["top_selectors"][0]["selector"], "0xa9059cbb")
        self.assertEqual(summary["delegatecall_edges"][0]["storage_context_candidate"], router)
        self.assertEqual(summary["delegatecall_edges"][0]["implementation_candidate"], implementation)

    def test_relevant_code_scope_includes_destinations_storage_owners_and_internal_callees(self):
        report = characterizer.characterize_blocks(self.synthetic_blocks())
        internal = "0x" + "cc" * 20
        call_summary = {
            "first_seen_block": {internal: 101},
        }
        targets = characterizer.build_code_targets(report, call_summary, "relevant")
        self.assertIn("0x" + "aa" * 20, targets)
        self.assertIn("0x" + "bb" * 20, targets)
        self.assertIn(internal, targets)
        self.assertEqual(targets[internal], 101)

    def test_native_port_candidates_rank_storage_owner_family_by_conflict_coverage(self):
        token = "0x" + "aa" * 20
        router = "0x" + "bb" * 20
        key = "evm/" + "aa" * 20 + "/" + "01" * 32
        blocks = [
            {
                "block_number": 100,
                "transactions": [
                    {
                        "tx_index": 0,
                        "tx_hash": "0x01",
                        "to": router,
                        "selector": "0x12345678",
                        "failed": False,
                        "gas_used": 10,
                        "reads": [key],
                        "writes": [],
                    },
                    {
                        "tx_index": 1,
                        "tx_hash": "0x02",
                        "to": router,
                        "selector": "0x12345678",
                        "failed": False,
                        "gas_used": 10,
                        "reads": [],
                        "writes": [key],
                    },
                ],
            }
        ]
        call_blocks = {
            100: {
                "transactions": [
                    {
                        "tx_hash": "0x01",
                        "result": {
                            "type": "CALL",
                            "to": router,
                            "input": "0x12345678",
                            "calls": [{"type": "CALL", "to": token, "input": "0xa9059cbb"}],
                        },
                    },
                    {
                        "tx_hash": "0x02",
                        "result": {
                            "type": "CALL",
                            "to": router,
                            "input": "0x12345678",
                            "calls": [{"type": "CALL", "to": token, "input": "0xa9059cbb"}],
                        },
                    },
                ]
            }
        }
        report = characterizer.characterize_blocks(blocks)
        calls = characterizer.summarize_call_traces(blocks, call_blocks)
        token_code = "0x6001600055"
        router_code = "0x6002600055"
        code_cache = {
            token: {"block_number": 100, "code": token_code},
            router: {"block_number": 100, "code": router_code},
        }
        candidates = characterizer.build_native_port_candidates(
            blocks, report, call_blocks, calls, code_cache
        )
        self.assertEqual(candidates["total_unique_conflict_pairs"], 1)
        self.assertEqual(candidates["unmapped_code_conflict_pairs"], 0)
        self.assertEqual(len(candidates["ranked"]), 1)
        candidate = candidates["ranked"][0]
        self.assertEqual(candidate["family"], characterizer.runtime_code_sha256(token_code[2:]))
        self.assertEqual(candidate["unique_conflict_pairs_covered"], 1)
        self.assertEqual(candidate["conflict_pair_coverage"], 1.0)
        self.assertEqual(candidate["internal_invocations"], 2)
        self.assertEqual(candidate["transactions_with_invocation"], 2)
        self.assertEqual(candidate["direct_destination_transactions"], 0)
        self.assertEqual(candidates["cumulative_conflict_coverage"][0]["coverage"], 1.0)

    def test_eip1967_slot_probe_decodes_implementation_and_is_resumable(self):
        proxy = "0x" + "aa" * 20
        implementation = "0x" + "cc" * 20

        class FakeClient:
            def __init__(self):
                self.calls = 0

            def call(self, method, params):
                self.calls += 1
                if method != "eth_getStorageAt":
                    raise AssertionError(method)
                if params[1] == characterizer.EIP1967_IMPLEMENTATION_SLOT:
                    return "0x" + "00" * 12 + implementation[2:]
                if params[1] == characterizer.EIP1967_BEACON_SLOT:
                    return "0x" + "00" * 32
                raise AssertionError(params)

        client = FakeClient()
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "proxy-resolution-cache.json"
            first = characterizer.fetch_eip1967_slots(
                {proxy: 100}, client, path, fixed_block=None, delay_ms=0
            )
            self.assertEqual(first[proxy]["implementation_address"], implementation)
            self.assertIsNone(first[proxy]["beacon_address"])
            self.assertEqual(client.calls, 2)
            second = characterizer.fetch_eip1967_slots(
                {proxy: 100}, client, path, fixed_block=None, delay_ms=0
            )
            self.assertEqual(second[proxy]["implementation_address"], implementation)
            self.assertEqual(client.calls, 2, "cached slot probes should be reused")

    def test_native_family_mapping_rewrites_eip1967_storage_context_to_implementation_profile(self):
        proxy = "0x" + "aa" * 20
        implementation = "0x" + "cc" * 20
        key = "evm/" + "aa" * 20 + "/" + "01" * 32
        blocks = [
            {
                "block_number": 100,
                "transactions": [
                    {
                        "tx_index": 0, "tx_hash": "0x01", "to": proxy,
                        "selector": "0xa9059cbb", "failed": False, "gas_used": 10,
                        "reads": [key], "writes": [],
                    },
                    {
                        "tx_index": 1, "tx_hash": "0x02", "to": proxy,
                        "selector": "0xa9059cbb", "failed": False, "gas_used": 10,
                        "reads": [], "writes": [key],
                    },
                ],
            }
        ]
        call_blocks = {
            100: {
                "transactions": [
                    {
                        "tx_hash": "0x01",
                        "result": {
                            "type": "CALL", "to": proxy, "input": "0xa9059cbb",
                            "calls": [{
                                "type": "DELEGATECALL", "from": proxy, "to": implementation,
                                "input": "0xa9059cbb",
                            }],
                        },
                    },
                    {
                        "tx_hash": "0x02",
                        "result": {
                            "type": "CALL", "to": proxy, "input": "0xa9059cbb",
                            "calls": [{
                                "type": "DELEGATECALL", "from": proxy, "to": implementation,
                                "input": "0xa9059cbb",
                            }],
                        },
                    },
                ]
            }
        }
        report = characterizer.characterize_blocks(blocks)
        calls = characterizer.summarize_call_traces(blocks, call_blocks)
        proxy_code = "0x6001600055"
        implementation_code = "0x6002600055"
        code_cache = {
            proxy: {"block_number": 100, "code": proxy_code},
            implementation: {"block_number": 100, "code": implementation_code},
        }
        proxy_cache = {
            proxy: {
                "block_number": 100,
                "implementation_slot": "0x" + "00" * 12 + implementation[2:],
                "implementation_address": implementation,
                "beacon_slot": "0x" + "00" * 32,
                "beacon_address": None,
            }
        }
        mapping = characterizer.build_native_family_mapping_candidates(
            blocks, report, calls, code_cache, proxy_cache
        )
        implementation_family = characterizer.runtime_code_sha256(implementation_code[2:])
        proxy_family = characterizer.runtime_code_sha256(proxy_code[2:])
        resolution = mapping["resolution_records"][0]
        self.assertEqual(resolution["resolution_status"], "resolved-eip1967")
        self.assertEqual(resolution["recommended_profile_family"], implementation_family)
        self.assertEqual(mapping["ranked"][0]["profile_family"], implementation_family)
        self.assertIn(proxy_family, mapping["ranked"][0]["storage_context_families"])
        self.assertEqual(mapping["ranked"][0]["unique_conflict_pairs_covered"], 1)
        self.assertEqual(mapping["ranked"][0]["resolved_proxy_owner_count"], 1)

    def test_generic_delegatecall_is_reported_but_not_silently_rewritten(self):
        owner = "0x" + "aa" * 20
        plugin = "0x" + "cc" * 20
        key = "evm/" + "aa" * 20 + "/" + "01" * 32
        blocks = [{
            "block_number": 100,
            "transactions": [
                {"tx_index": 0, "tx_hash": "0x01", "to": owner, "selector": "0x11111111", "failed": False, "gas_used": 1, "reads": [key], "writes": []},
                {"tx_index": 1, "tx_hash": "0x02", "to": owner, "selector": "0x11111111", "failed": False, "gas_used": 1, "reads": [], "writes": [key]},
            ],
        }]
        call_blocks = {100: {"transactions": [
            {"tx_hash": "0x01", "result": {"type": "CALL", "to": owner, "input": "0x11111111", "calls": [{"type": "DELEGATECALL", "from": owner, "to": plugin, "input": "0x22222222"}]}},
            {"tx_hash": "0x02", "result": {"type": "CALL", "to": owner, "input": "0x11111111", "calls": [{"type": "DELEGATECALL", "from": owner, "to": plugin, "input": "0x22222222"}]}},
        ]}}
        report = characterizer.characterize_blocks(blocks)
        calls = characterizer.summarize_call_traces(blocks, call_blocks)
        owner_code = "0x6001600055"
        plugin_code = "0x6002600055"
        code_cache = {
            owner: {"block_number": 100, "code": owner_code},
            plugin: {"block_number": 100, "code": plugin_code},
        }
        mapping = characterizer.build_native_family_mapping_candidates(
            blocks, report, calls, code_cache, {}
        )
        owner_family = characterizer.runtime_code_sha256(owner_code[2:])
        resolution = mapping["resolution_records"][0]
        self.assertEqual(resolution["resolution_status"], "observed-delegatecall-candidate")
        self.assertEqual(resolution["recommended_profile_family"], owner_family)
        self.assertEqual(mapping["ranked"][0]["profile_family"], owner_family)
        self.assertEqual(resolution["observed_delegatecall_targets"][0]["address"], plugin)


class NativeFamilyDossierTests(unittest.TestCase):
    def _mapping(self):
        return {
            "unique_conflict_pairs": 100,
            "coverage_targets": [
                {"target_coverage": 0.90, "minimum_profile_families": 1, "achieved_coverage": 0.91, "unique_conflict_pairs": 91},
                {"target_coverage": 0.95, "minimum_profile_families": 2, "achieved_coverage": 0.96, "unique_conflict_pairs": 96},
            ],
            "interface_hint_note": "triage only",
            "mapping_semantics": "storage namespaces remain distinct",
            "ranked": [
                {
                    "profile_family": "family-a",
                    "unique_conflict_pairs_covered": 80,
                    "conflict_pair_coverage": 0.80,
                    "storage_owner_count": 1,
                    "resolved_proxy_owner_count": 0,
                    "unresolved_delegatecall_owner_count": 0,
                    "heuristic_interface_hints": ["fungible-token-like"],
                    "top_selectors": [{"selector": "0xa9059cbb", "invocations": 10}],
                    "invocations": 10,
                    "internal_invocations": 2,
                    "root_invocations": 8,
                    "top_storage_owners": [{
                        "address": "0x" + "aa" * 20,
                        "implementation_address": None,
                        "resolution_status": "direct-code",
                    }],
                },
                {
                    "profile_family": "family-b",
                    "unique_conflict_pairs_covered": 20,
                    "conflict_pair_coverage": 0.20,
                    "storage_owner_count": 1,
                    "resolved_proxy_owner_count": 1,
                    "unresolved_delegatecall_owner_count": 0,
                    "heuristic_interface_hints": ["nft-like"],
                    "top_selectors": [{"selector": "0x42842e0e", "invocations": 3}],
                    "invocations": 3,
                    "internal_invocations": 3,
                    "root_invocations": 0,
                    "top_storage_owners": [{
                        "address": "0x" + "bb" * 20,
                        "implementation_address": "0x" + "cc" * 20,
                        "resolution_status": "resolved-eip1967",
                    }],
                },
            ],
        }

    def test_selects_minimum_precomputed_family_count_for_coverage_target(self):
        selected, target = dossier_builder.select_profile_families(self._mapping(), 0.95)
        self.assertEqual(len(selected), 2)
        self.assertEqual(target["unique_conflict_pairs"], 96)

    def test_resolved_proxy_uses_implementation_as_source_address(self):
        item = self._mapping()["ranked"][1]
        source, storage, status = dossier_builder.representative_source_address(item)
        self.assertEqual(source, "0x" + "cc" * 20)
        self.assertEqual(storage, "0x" + "bb" * 20)
        self.assertEqual(status, "resolved-eip1967")

    def test_archetype_recommendation_prioritizes_pair_and_wrapped_native(self):
        self.assertEqual(
            dossier_builder.archetype_recommendation(["fungible-token-like", "constant-product-amm-pair-like"])[0],
            "astroport-pair",
        )
        self.assertEqual(
            dossier_builder.archetype_recommendation(["fungible-token-like", "wrapped-native-token-like"])[0],
            "wrapped-native-token",
        )
        self.assertEqual(dossier_builder.archetype_recommendation([])[0], "manual-review")

    def test_sourcify_summary_preserves_source_and_abi_evidence(self):
        address = "0x" + "aa" * 20
        summary = dossier_builder.summarize_sourcify(
            address,
            {
                "match": "exact_match",
                "runtimeMatch": "exact_match",
                "verifiedAt": "2026-01-01T00:00:00Z",
                "compilation": {
                    "contractIdentifier": "Token.sol:Token",
                    "language": "Solidity",
                    "compilerVersion": "0.8.24",
                },
                "sources": {"Token.sol": {"content": "contract Token {}"}},
                "abi": [{
                    "type": "function",
                    "name": "transfer",
                    "inputs": [{"type": "address"}, {"type": "uint256"}],
                }],
            },
        )
        self.assertEqual(summary["status"], "verified")
        self.assertEqual(summary["contract_identifier"], "Token.sol:Token")
        self.assertEqual(summary["source_files"], ["Token.sol"])
        self.assertIn("transfer(address,uint256)", summary["abi"]["function_signatures"])

    def test_unresolved_delegatecall_candidate_is_carried_into_dossier(self):
        mapping = self._mapping()
        owner = "0x" + "aa" * 20
        target = "0x" + "dd" * 20
        mapping["ranked"][0]["top_storage_owners"][0]["resolution_status"] = "observed-delegatecall-candidate"
        mapping["ranked"][0]["unresolved_delegatecall_owner_count"] = 1
        mapping["resolution_records"] = [{
            "storage_owner": owner,
            "resolution_status": "observed-delegatecall-candidate",
            "observed_delegatecall_targets": [{
                "address": target,
                "family": "impl-family",
                "invocations": 7,
            }],
        }]
        dossier = dossier_builder.build_dossier(
            mapping,
            0.95,
            {target: {"provider": "sourcify-v2", "status": "verified"}},
        )
        candidate = dossier["families"][0]["delegatecall_source_candidates"][0]
        self.assertEqual(candidate["address"], target)
        self.assertEqual(candidate["invocations"], 7)
        self.assertEqual(candidate["source_resolution"]["status"], "verified")

    def test_native_map_is_skeleton_not_completed_symbolic_mapping(self):
        mapping = self._mapping()
        source = {
            "0x" + "aa" * 20: {
                "provider": "sourcify-v2",
                "status": "verified",
                "contract_identifier": "Token.sol:Token",
            }
        }
        dossier = dossier_builder.build_dossier(mapping, 0.95, source)
        native_map = dossier_builder.build_native_family_map(dossier)
        first = native_map["families"][0]
        second = native_map["families"][1]
        self.assertEqual(first["native_archetype"], "cw20-base")
        self.assertIsNone(first["native_contract_source"])
        self.assertIsNone(first["symbolic_analysis"])
        self.assertEqual(second["native_archetype"], "cw721-base")
        self.assertEqual(native_map["status"], "provisional")


class NativeS3PlanTests(unittest.TestCase):
    def _small_fixture(self):
        a = "0x" + "aa" * 20
        b = "0x" + "bb" * 20
        c = "0x" + "cc" * 20
        key_a = "evm/" + "aa" * 20 + "/" + "01" * 32
        code = "0x6001600055"
        family = native_plan_builder.runtime_code_family(code)
        frozen = {
            "dataset": "test",
            "target_conflict_coverage": 0.95,
            "native_code_families": {
                "cw20-base": {
                    "native_contract_source": "missing/cw20.rs",
                    "symbolic_analysis": "missing/cw20.symbolic.json",
                }
            },
            "profile_mappings": [{
                "rank": 1,
                "ethereum_profile_family": family,
                "native_code_family": "cw20-base",
            }],
        }
        mapping = {
            "resolution_records": [{
                "storage_owner": a,
                "recommended_profile_family": family,
                "resolution_status": "direct-code",
            }]
        }
        code_cache = {
            a: {"block_number": 100, "code": code},
            b: {"block_number": 100, "code": code},
        }
        blocks = [{
            "block_number": 100,
            "block_hash": "0x" + "11" * 32,
            "timestamp": 1,
            "transactions": [
                {
                    "tx_index": 0,
                    "tx_hash": "0x" + "01" * 32,
                    "from": c,
                    "to": a,
                    "selector": "0xa9059cbb",
                    "value": "0x0",
                    "gas_used": 10,
                    "failed": False,
                    "reads": [key_a],
                    "writes": [],
                },
                {
                    "tx_index": 1,
                    "tx_hash": "0x" + "02" * 32,
                    "from": c,
                    "to": b,
                    "selector": "0xa9059cbb",
                    "value": "0x0",
                    "gas_used": 11,
                    "failed": False,
                    "reads": [],
                    "writes": [key_a],
                },
            ],
        }]
        calldata_a = "0xa9059cbb" + "00" * 12 + "dd" * 20 + (7).to_bytes(32, "big").hex()
        calldata_b = "0xa9059cbb" + "00" * 12 + "ee" * 20 + (8).to_bytes(32, "big").hex()
        call_cache = {100: {"transactions": [
            {
                "tx_hash": blocks[0]["transactions"][0]["tx_hash"],
                "result": {
                    "type": "CALL", "from": c, "to": a, "input": calldata_a,
                    "calls": [{"type": "DELEGATECALL", "from": a, "to": "0x" + "99" * 20, "input": "0x12345678"}],
                },
            },
            {
                "tx_hash": blocks[0]["transactions"][1]["tx_hash"],
                "result": {"type": "CALL", "from": c, "to": b, "input": calldata_b},
            },
        ]}}
        return blocks, call_cache, frozen, mapping, code_cache, family, a, b

    def test_frozen_real_map_collapses_eleven_profiles_to_seven_native_families(self):
        frozen = json.loads((ROOT / "evaluation/vegeta/s3-native-family-map.v1.json").read_text())
        native_plan_builder.validate_frozen_map(frozen)
        self.assertEqual(len(frozen["profile_mappings"]), 11)
        self.assertEqual(len(frozen["native_code_families"]), 7)
        by_rank = {item["rank"]: item["native_code_family"] for item in frozen["profile_mappings"]}
        self.assertEqual(by_rank[4], "cw721-mintable")
        self.assertEqual(by_rank[8], "xen-like")
        self.assertEqual(by_rank[10], "cw20-base")
        self.assertEqual(by_rank[11], "cw20-base")

    def test_plan_preserves_transactions_decodes_known_arguments_and_keeps_instances_distinct(self):
        blocks, calls, frozen, mapping, code_cache, _, a, b = self._small_fixture()
        resolver = native_plan_builder.FamilyResolver(frozen, code_cache, mapping)
        plan, coverage, catalog = native_plan_builder.build_plan(blocks, calls, frozen, resolver, ROOT)
        self.assertEqual([tx["tx_hash"] for tx in plan[0]["transactions"]], [tx["tx_hash"] for tx in blocks[0]["transactions"]])
        first = plan[0]["transactions"][0]["native_actions"][0]
        second = plan[0]["transactions"][1]["native_actions"][0]
        self.assertEqual(first["native_entrypoint"], "execute::transfer")
        self.assertEqual(first["arguments"]["amount"], 7)
        self.assertEqual(first["arguments"]["recipient"], "0x" + "dd" * 20)
        self.assertEqual(first["native_instance_id"], f"cw20-base:{a}")
        self.assertEqual(second["native_instance_id"], f"cw20-base:{b}")
        self.assertNotEqual(first["native_instance_id"], second["native_instance_id"])
        self.assertEqual(coverage["transactions_retained"], 2)
        self.assertGreaterEqual(catalog["total_instances"], 2)

    def test_delegatecall_is_inlined_into_parent_storage_namespace(self):
        blocks, calls, frozen, mapping, code_cache, _, a, _ = self._small_fixture()
        resolver = native_plan_builder.FamilyResolver(frozen, code_cache, mapping)
        plan, _, _ = native_plan_builder.build_plan(blocks, calls, frozen, resolver, ROOT)
        parent, child = plan[0]["transactions"][0]["native_actions"][:2]
        self.assertEqual(child["translation_status"], "inlined-delegatecall")
        self.assertEqual(child["storage_context_address"], a)
        self.assertEqual(child["native_instance_id"], f"cw20-base:{a}")
        # callTracer's DELEGATECALL frame.from is the proxy/current context, but the implementation
        # observes the parent's original msg.sender.
        self.assertEqual(child["ethereum_caller"], a)
        self.assertEqual(child["ethereum_msg_sender"], parent["ethereum_msg_sender"])

    def test_background_calls_are_retained_and_concrete_accesses_never_enter_plan(self):
        blocks, calls, frozen, mapping, code_cache, _, _, _ = self._small_fixture()
        # Make the second address unselected so the transaction remains as background rather than disappearing.
        b = "0x" + "bb" * 20
        code_cache[b] = {"block_number": 100, "code": "0x6002600055"}
        resolver = native_plan_builder.FamilyResolver(frozen, code_cache, mapping)
        plan, coverage, _ = native_plan_builder.build_plan(blocks, calls, frozen, resolver, ROOT)
        second = plan[0]["transactions"][1]
        self.assertEqual(second["translation_class"], "background-only")
        self.assertEqual(second["native_actions"][0]["translation_status"], "background-fallback")
        self.assertEqual(len(plan[0]["transactions"]), 2)
        self.assertIsNone(native_plan_validator.forbidden_access_path(plan))
        self.assertEqual(coverage["transaction_semantic_coverage"]["background_only_transactions"], 1)


    def test_delegatecall_effective_msg_sender_inherits_root_sender(self):
        blocks, calls, frozen, mapping, code_cache, _, a, _ = self._small_fixture()
        resolver = native_plan_builder.FamilyResolver(frozen, code_cache, mapping)
        user = "0x" + "bc" * 20
        proxy = "0x" + "b1" * 20
        implementation = "0x" + "2d" * 20
        actions = native_plan_builder.translate_call_tree({
            "type": "CALL",
            "from": user,
            "to": proxy,
            "input": "0x42842e0e",
            "value": "0x0",
            "calls": [{
                "type": "DELEGATECALL",
                "from": proxy,
                "to": implementation,
                "input": "0x42842e0e",
                "value": "0x0",
            }],
        }, resolver)
        self.assertEqual(actions[0]["ethereum_caller"], user)
        self.assertEqual(actions[0]["ethereum_msg_sender"], user)
        self.assertEqual(actions[1]["ethereum_caller"], proxy)
        self.assertEqual(actions[1]["ethereum_msg_sender"], user)

    def test_system_actions_map_precompile_empty_code_transfer_and_noop(self):
        blocks, calls, frozen, mapping, code_cache, _, _, _ = self._small_fixture()
        empty = "0x" + "ee" * 20
        code_cache[empty] = {"block_number": 100, "code": "0x"}
        resolver = native_plan_builder.FamilyResolver(frozen, code_cache, mapping)

        transfer = native_plan_builder.translate_call_tree({
            "type": "CALL", "from": "0x" + "cc" * 20, "to": empty, "input": "0x1234", "value": "0x2a"
        }, resolver)[0]
        self.assertEqual(transfer["translation_status"], "mapped-system-action")
        self.assertEqual(transfer["system_action_kind"], "plain-value-transfer")
        self.assertEqual(transfer["native_entrypoint"], "system::bank_send")
        self.assertEqual(transfer["arguments"]["amount_wei"], 42)
        self.assertEqual(transfer["ethereum_caller"], "0x" + "cc" * 20)

        noop = native_plan_builder.translate_call_tree({
            "type": "STATICCALL", "from": "0x" + "cc" * 20, "to": empty, "input": "0xdead", "value": "0x0"
        }, resolver)[0]
        self.assertEqual(noop["system_action_kind"], "empty-code-noop")

        precompile = "0x" + "00" * 19 + "02"
        helper = native_plan_builder.translate_call_tree({
            "type": "STATICCALL", "from": "0x" + "cc" * 20, "to": precompile, "input": "0x1234", "value": "0x0"
        }, resolver)[0]
        self.assertEqual(helper["system_action_kind"], "ethereum-precompile")
        self.assertEqual(helper["native_entrypoint"], "system::precompile::sha256")

    def test_block_balanced_conflict_metrics_report_distribution_and_concentration(self):
        metrics = native_plan_builder.block_balanced_conflict_metrics({
            "per_block": [
                {"block_number": 1, "total_conflict_pairs": 100, "selected_family_conflict_pairs": 100, "coverage": 1.0},
                {"block_number": 2, "total_conflict_pairs": 10, "selected_family_conflict_pairs": 5, "coverage": 0.5},
                {"block_number": 3, "total_conflict_pairs": 5, "selected_family_conflict_pairs": 0, "coverage": 0.0},
                {"block_number": 4, "total_conflict_pairs": 0, "selected_family_conflict_pairs": 0, "coverage": 1.0},
            ]
        })
        self.assertEqual(metrics["conflict_bearing_blocks"], 3)
        self.assertEqual(metrics["zero_conflict_blocks"], 1)
        self.assertAlmostEqual(metrics["median_coverage"], 0.5)
        self.assertEqual(metrics["blocks_meeting_threshold"]["90pct"], 1)
        self.assertAlmostEqual(metrics["source_conflict_concentration"]["top_1_blocks"]["share"], 100 / 115)

    def test_frozen_gate_uses_final_mapping_simulation_for_semantic_volume(self):
        blocks, calls, frozen, mapping, code_cache, _, _, _ = self._small_fixture()
        resolver = native_plan_builder.FamilyResolver(frozen, code_cache, mapping)
        plan, coverage, _ = native_plan_builder.build_plan(blocks, calls, frozen, resolver, ROOT)
        frozen_for_validator = {
            **frozen,
            "native_code_families": {f"family-{i}": {} for i in range(7)},
            "profile_mappings": [
                {"rank": i + 1, "ethereum_profile_family": f"profile-{i}", "native_code_family": f"family-{i % 7}"}
                for i in range(11)
            ],
        }
        gate = {"metrics": {
            "aggregate_source_conflict_coverage": {"minimum": 0.95, "enforced": True},
            "semantic_transaction_coverage": {"minimum": 0.75, "enforced": True},
            "median_conflict_bearing_block_coverage": {"minimum": 0.0, "enforced": True},
            "semantic_call_frame_coverage": {"minimum": 0.50, "enforced": True},
        }}
        simulation = {"simulation": {
            "semantic_transaction_coverage": 0.80,
            "semantic_call_frame_coverage": 0.60,
        }}
        errors, _ = native_plan_validator.validate_plan(
            blocks, plan, coverage, frozen_for_validator, gate_config=gate, simulation=simulation
        )
        self.assertEqual(errors, [])
        simulation["simulation"]["semantic_transaction_coverage"] = 0.70
        errors, _ = native_plan_validator.validate_plan(
            blocks, plan, coverage, frozen_for_validator, gate_config=gate, simulation=simulation
        )
        self.assertTrue(any("semantic_transaction_coverage" in error for error in errors))

    def test_source_conflict_coverage_is_offline_only_and_recomputed_from_owner_mapping(self):
        blocks, _, frozen, mapping, code_cache, _, _, _ = self._small_fixture()
        resolver = native_plan_builder.FamilyResolver(frozen, code_cache, mapping)
        coverage = native_plan_builder.source_conflict_coverage(blocks, resolver)
        self.assertEqual(coverage["total_unique_conflict_pairs"], 1)
        self.assertEqual(coverage["selected_family_unique_conflict_pairs"], 1)
        self.assertEqual(coverage["coverage"], 1.0)

    def test_validator_accepts_preexecution_plan_but_execution_ready_gate_can_fail(self):
        blocks, calls, frozen, mapping, code_cache, _, _, _ = self._small_fixture()
        resolver = native_plan_builder.FamilyResolver(frozen, code_cache, mapping)
        plan, coverage, _ = native_plan_builder.build_plan(blocks, calls, frozen, resolver, ROOT)
        # The tiny unit-test map is one family, so patch cardinalities solely for validator structure here.
        frozen_for_validator = {
            **frozen,
            "native_code_families": {f"family-{i}": {} for i in range(7)},
            "profile_mappings": [
                {"rank": i + 1, "ethereum_profile_family": f"profile-{i}", "native_code_family": f"family-{i % 7}"}
                for i in range(11)
            ],
        }
        errors, warnings = native_plan_validator.validate_plan(
            blocks, plan, coverage, frozen_for_validator, require_execution_ready=False
        )
        self.assertEqual(errors, [])
        self.assertTrue(any("execution" in warning for warning in warnings))
        errors, _ = native_plan_validator.validate_plan(
            blocks, plan, coverage, frozen_for_validator, require_execution_ready=True
        )
        self.assertTrue(any("execution-ready gate failed" in error for error in errors))


class NativeBackgroundGapTests(unittest.TestCase):
    def test_greedy_gap_ranking_targets_background_only_transactions(self):
        a = "0x" + "aa" * 20
        b = "0x" + "bb" * 20
        code_a = "0x6001"
        code_b = "0x6002"
        code_cache = {
            a: {"code": code_a},
            b: {"code": code_b},
        }
        plan = [{
            "block_number": 1,
            "transactions": [
                {
                    "tx_index": 0, "translation_class": "background-only",
                    "native_actions": [{"translation_status": "background-fallback", "ethereum_code_address": a, "selector": "0xa9059cbb", "call_type": "CALL"}],
                },
                {
                    "tx_index": 1, "translation_class": "background-only",
                    "native_actions": [
                        {"translation_status": "background-fallback", "ethereum_code_address": a, "selector": "0xa9059cbb", "call_type": "CALL"},
                        {"translation_status": "background-fallback", "ethereum_code_address": b, "selector": "0x12345678", "call_type": "CALL"},
                    ],
                },
                {
                    "tx_index": 2, "translation_class": "fully-semantic",
                    "native_actions": [
                        {"translation_status": "mapped-system-action", "ethereum_code_address": "0x" + "00" * 19 + "02", "selector": "0x", "call_type": "STATICCALL"},
                        {"translation_status": "background-fallback", "ethereum_code_address": b, "selector": "0x12345678", "call_type": "CALL"},
                    ],
                },
            ],
        }]
        groups, baseline = background_gap_builder.collect_gap(plan, code_cache)
        ranked = background_gap_builder.greedy_marginal_rank(groups, baseline, 10)
        self.assertEqual(baseline["semantic_transactions"], 1)
        self.assertEqual(ranked[0]["representative_address"], a)
        self.assertEqual(ranked[0]["marginal_background_transactions"], 2)
        self.assertAlmostEqual(ranked[0]["potential_semantic_transaction_coverage"], 1.0)

    def test_verified_abi_fold_candidates_are_conservative(self):
        def summary(signatures):
            return {"status": "verified", "abi": {"function_signatures": signatures}}
        erc20 = background_gap_builder.infer_fold_candidate(summary([
            "transfer(address,uint256)", "transferFrom(address,address,uint256)",
            "balanceOf(address)", "approve(address,uint256)",
        ]))
        self.assertEqual(erc20["native_fold_candidate"], "cw20-base")
        nft = background_gap_builder.infer_fold_candidate(summary([
            "ownerOf(uint256)", "setApprovalForAll(address,bool)", "safeTransferFrom(address,address,uint256)"
        ]))
        self.assertEqual(nft["native_fold_candidate"], "cw721-mintable")
        router = background_gap_builder.infer_fold_candidate(summary(["multicall(bytes[])"]))
        self.assertEqual(router["native_fold_candidate"], "router-helper-or-new-family")

    def test_empty_code_gap_group_is_visible_instead_of_being_silently_folded(self):
        empty = "0x" + "ee" * 20
        plan = [{"block_number": 1, "transactions": [{
            "tx_index": 0, "translation_class": "background-only",
            "native_actions": [{"translation_status": "background-fallback", "ethereum_code_address": empty, "selector": "0x", "call_type": "CALL"}],
        }]}]
        groups, _ = background_gap_builder.collect_gap(plan, {empty: {"code": "0x"}})
        self.assertIn("special:empty-code", groups)
        self.assertEqual(groups["special:empty-code"]["family_kind"], "empty-code")


class NativeFinalMappingTests(unittest.TestCase):
    def test_keccak_selector_matches_ethereum_not_fips_sha3(self):
        self.assertEqual(
            final_map_builder.keccak256(b"").hex(),
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470",
        )
        self.assertEqual(
            final_map_builder.selector_for_signature("transfer(address,uint256)"),
            "0xa9059cbb",
        )

    def test_selector_granular_token_fold_does_not_absorb_custom_methods(self):
        source = final_map_builder.normalize_source_summary({
            "status": "verified",
            "contract_identifier": "HEX.sol:HEX",
            "abi": {"function_signatures": [
                "transfer(address,uint256)",
                "transferFrom(address,address,uint256)",
                "balanceOf(address)",
                "approve(address,uint256)",
                "stakeStart(uint256,uint256)",
            ]},
        })
        custom = final_map_builder.selector_for_signature("stakeStart(uint256,uint256)")
        rules = final_map_builder.selector_rules_for_source(
            family="family-a",
            observed_selectors={"0xa9059cbb", custom},
            source=source,
            source_address="0x" + "aa" * 20,
            address_scope=None,
            resolution="direct-runtime-source",
        )
        self.assertEqual([rule["selector"] for rule in rules], ["0xa9059cbb"])
        self.assertEqual(rules[0]["native_code_family"], "cw20-base")

    def test_erc1155_and_operator_registry_are_candidate_archetypes(self):
        erc1155 = final_map_builder.normalize_source_summary({
            "status": "verified",
            "contract_identifier": "ERC1155CreatorImplementation.sol:ERC1155CreatorImplementation",
            "abi": {"function_signatures": [
                "safeTransferFrom(address,address,uint256,uint256,bytes)",
                "balanceOf(address,uint256)",
                "setApprovalForAll(address,bool)",
            ]},
        })
        self.assertEqual(final_map_builder.classify_archetype(erc1155), "cw1155-like")
        registry = final_map_builder.normalize_source_summary({
            "status": "verified",
            "contract_identifier": "src/OperatorFilterRegistry.sol:OperatorFilterRegistry",
            "abi": {"function_signatures": ["isOperatorAllowed(address,address)"]},
        })
        self.assertEqual(final_map_builder.classify_archetype(registry), "operator-filter-helper")

    def test_eip1167_resolution_and_address_scoped_rule_matching(self):
        implementation = "0x" + "cc" * 20
        runtime = "0x363d3d373d3d3d363d73" + implementation[2:] + "5af43d82803e903d91602b57fd5bf3"
        self.assertEqual(final_map_builder.detect_eip1167(runtime), implementation)
        address_a = "0x" + "aa" * 20
        address_b = "0x" + "bb" * 20
        family = final_map_builder.runtime_family(runtime)
        code_cache = {
            address_a: {"code": runtime},
            address_b: {"code": runtime},
        }
        rules = [{
            "runtime_family": family,
            "selector": "0xa9059cbb",
            "address_scope": [address_a],
            "native_code_family": "cw20-base",
            "native_entrypoint": "execute::transfer",
        }]
        index = final_map_builder.rule_index(rules)
        action_a = {"ethereum_code_address": address_a, "selector": "0xa9059cbb"}
        action_b = {"ethereum_code_address": address_b, "selector": "0xa9059cbb"}
        self.assertIsNotNone(final_map_builder.matching_rule(action_a, code_cache, index))
        self.assertIsNone(final_map_builder.matching_rule(action_b, code_cache, index))

    def test_simulation_recovers_only_selector_matched_background_frames(self):
        address = "0x" + "aa" * 20
        code = "0x6001"
        family = final_map_builder.runtime_family(code)
        plan = [{"block_number": 1, "transactions": [
            {"tx_index": 0, "translation_class": "background-only", "native_actions": [
                {"translation_status": "background-fallback", "ethereum_code_address": address, "selector": "0xa9059cbb"},
                {"translation_status": "background-fallback", "ethereum_code_address": address, "selector": "0x12345678"},
            ]},
            {"tx_index": 1, "translation_class": "fully-semantic", "native_actions": [
                {"translation_status": "mapped-system-action", "ethereum_code_address": address, "selector": "0x"},
            ]},
        ]}]
        rules = [{
            "runtime_family": family, "selector": "0xa9059cbb", "address_scope": None,
            "native_code_family": "cw20-base", "native_entrypoint": "execute::transfer",
        }]
        simulation = final_map_builder.simulate(plan, {address: {"code": code}}, rules)
        self.assertEqual(simulation["semantic_transactions"], 2)
        self.assertEqual(simulation["fully_semantic_transactions"], 1)
        self.assertEqual(simulation["mixed_semantic_fallback_transactions"], 1)
        self.assertEqual(simulation["recovered_fallback_frames"], 1)
        self.assertAlmostEqual(simulation["semantic_call_frame_coverage"], 2 / 3)

    def test_unverified_custodial_batch_rules_require_audited_call_shapes(self):
        address = "0x" + "aa" * 20
        child = "0x" + "bb" * 20
        code = "0x6001"
        family = final_map_builder.runtime_family(code)
        plan = [{"block_number": 1, "transactions": [
            {"tx_index": 0, "native_actions": [
                {
                    "action_id": 0, "parent_action_id": None, "depth": 0,
                    "translation_status": "background-fallback",
                    "ethereum_code_address": address, "selector": "0x",
                    "call_type": "CALL", "ethereum_value": "0x10",
                },
            ]},
            {"tx_index": 1, "native_actions": [
                {
                    "action_id": 0, "parent_action_id": None, "depth": 0,
                    "translation_status": "background-fallback",
                    "ethereum_code_address": address, "selector": "0x1a1da075",
                    "call_type": "CALL", "ethereum_value": "0x0",
                },
                {
                    "action_id": 1, "parent_action_id": 0, "depth": 1,
                    "translation_status": "background-fallback",
                    "ethereum_code_address": child, "selector": "0x",
                    "call_type": "CALL", "ethereum_value": "0x5",
                },
            ]},
            {"tx_index": 2, "native_actions": [
                {
                    "action_id": 0, "parent_action_id": None, "depth": 0,
                    "translation_status": "background-fallback",
                    "ethereum_code_address": address, "selector": "0xca350aa6",
                    "call_type": "CALL", "ethereum_value": "0x0",
                },
                {
                    "action_id": 1, "parent_action_id": 0, "depth": 1,
                    "translation_status": "mapped-native-call",
                    "ethereum_code_address": child, "selector": "0xa9059cbb",
                    "call_type": "CALL", "ethereum_value": "0x0",
                },
            ]},
        ]}]
        rules = final_map_builder.audited_custodial_batch_system_rules(
            plan, {address: {"code": code}}, address=address, family=family,
        )
        self.assertEqual([rule["selector"] for rule in rules], ["0x", "0x1a1da075", "0xca350aa6"])
        self.assertTrue(all(rule["address_scope"] == [address] for rule in rules))
        self.assertTrue(all(rule["native_code_family"] == "system" for rule in rules))
        self.assertEqual(rules[0]["native_entrypoint"], "system::custodial_value_deposit")
        self.assertEqual(rules[1]["native_entrypoint"], "system::batch_native_dispatch")
        self.assertEqual(rules[2]["native_entrypoint"], "system::batch_token_dispatch")

    def test_unverified_custodial_batch_rules_fail_closed_on_shape_mismatch(self):
        address = "0x" + "aa" * 20
        child = "0x" + "bb" * 20
        code = "0x6001"
        family = final_map_builder.runtime_family(code)
        plan = [{"block_number": 1, "transactions": [
            {"tx_index": 0, "native_actions": [
                {"depth": 0, "translation_status": "background-fallback", "ethereum_code_address": address,
                 "selector": "0x", "call_type": "CALL", "ethereum_value": "0x1"},
            ]},
            {"tx_index": 1, "native_actions": [
                {"depth": 0, "translation_status": "background-fallback", "ethereum_code_address": address,
                 "selector": "0x1a1da075", "call_type": "CALL", "ethereum_value": "0x0"},
                {"depth": 1, "translation_status": "background-fallback", "ethereum_code_address": child,
                 "selector": "0x", "call_type": "CALL", "ethereum_value": "0x1"},
            ]},
            {"tx_index": 2, "native_actions": [
                {"depth": 0, "translation_status": "background-fallback", "ethereum_code_address": address,
                 "selector": "0xca350aa6", "call_type": "CALL", "ethereum_value": "0x0"},
                {"depth": 1, "translation_status": "background-fallback", "ethereum_code_address": child,
                 "selector": "0xdeadbeef", "call_type": "CALL", "ethereum_value": "0x0"},
            ]},
        ]}]
        self.assertEqual(
            final_map_builder.audited_custodial_batch_system_rules(
                plan, {address: {"code": code}}, address=address, family=family,
            ),
            [],
        )

    def test_rank1_diagnostic_accepts_null_parent_action_id(self):
        address = "0x" + "aa" * 20
        parent_address = "0x" + "bb" * 20
        code = "0x6001"
        family = final_map_builder.runtime_family(code)
        plan = [{
            "block_number": 1,
            "transactions": [{
                "tx_index": 0,
                "native_actions": [
                    {
                        "action_id": 0,
                        "parent_action_id": None,
                        "depth": 0,
                        "translation_status": "background-fallback",
                        "ethereum_code_address": address,
                        "selector": "0x12345678",
                        "call_type": "CALL",
                        "ethereum_value": "0",
                    },
                    {
                        "action_id": 1,
                        "parent_action_id": 2,
                        "depth": 1,
                        "translation_status": "background-fallback",
                        "ethereum_code_address": address,
                        "selector": "0x12345678",
                        "call_type": "CALL",
                        "ethereum_value": "0",
                    },
                    {
                        "action_id": 2,
                        "parent_action_id": None,
                        "depth": 0,
                        "translation_status": "mapped-system-action",
                        "ethereum_code_address": parent_address,
                        "selector": "0x",
                        "call_type": "CALL",
                        "ethereum_value": "0",
                    },
                ],
            }],
        }]
        gap = {"ranked": [{"family": family, "family_kind": "runtime-code", "source_resolution": {"status": "not-found"}}]}
        report = final_map_builder.rank1_diagnostic(plan, gap, {address: {"code": code}})
        self.assertEqual(report["status"], "diagnosed")
        self.assertEqual(report["frames"], 2)
        self.assertEqual(report["root_frames"], 1)
        self.assertEqual(report["internal_frames"], 1)
        self.assertEqual(report["top_parent_addresses"][0], {"address": parent_address, "frames": 1})

    def test_checked_in_fidelity_gates_are_frozen(self):
        gate = json.loads((ROOT / "evaluation/vegeta/s3-native-preexecution-gates.v1.json").read_text())
        self.assertEqual(gate["policy_status"], "frozen-preexecution-fidelity-v1")
        expected = {
            "aggregate_source_conflict_coverage": 0.95,
            "median_conflict_bearing_block_coverage": 0.80,
            "semantic_transaction_coverage": 0.75,
            "semantic_call_frame_coverage": 0.50,
        }
        for name, minimum in expected.items():
            self.assertTrue(gate["metrics"][name]["enforced"])
            self.assertEqual(gate["metrics"][name]["minimum"], minimum)


class NativeS3ImplementationArtifactTests(unittest.TestCase):
    def test_checked_in_native_sources_and_symbolic_evidence_validate(self):
        manifest = json.loads(
            (ROOT / "evaluation/vegeta/s3-native-implementation-manifest.v1.json").read_text()
        )
        self.assertEqual(len(manifest["families"]), 10)
        for family in manifest["families"]:
            errors = native_impl_validator.validate_family(ROOT, family, False)
            self.assertEqual(errors, [], family["native_code_family"])

    def test_symbolic_provenance_fails_closed_after_source_change(self):
        manifest = json.loads(
            (ROOT / "evaluation/vegeta/s3-native-implementation-manifest.v1.json").read_text()
        )
        family = manifest["families"][0]
        with tempfile.TemporaryDirectory() as tmp:
            temp_root = Path(tmp)
            source = temp_root / family["source"]
            symbolic = temp_root / family["symbolic_analysis"]
            source.parent.mkdir(parents=True, exist_ok=True)
            symbolic.parent.mkdir(parents=True, exist_ok=True)
            source.write_text((ROOT / family["source"]).read_text() + "\n// drift\n")
            symbolic.write_text((ROOT / family["symbolic_analysis"]).read_text())
            errors = native_impl_validator.validate_family(temp_root, family, False)
        self.assertTrue(any("source SHA-256 mismatch" in error for error in errors))

    def test_no_native_source_contains_historical_trace_oracle_arrays(self):
        manifest = json.loads(
            (ROOT / "evaluation/vegeta/s3-native-implementation-manifest.v1.json").read_text()
        )
        for family in manifest["families"]:
            text = (ROOT / family["source"]).read_text()
            for forbidden in native_impl_validator.FORBIDDEN:
                self.assertNotIn(forbidden, text, family["native_code_family"])


class EvaluationConfigTests(unittest.TestCase):
    def test_symbolic_profile_exposes_only_prediction_arrays(self):
        profile = json.loads(
            (ROOT / "benchmarks" / "symbolic" / "vegeta-trace.symbolic.json").read_text()
        )
        origins = []
        semantic_components = []
        for entry in profile["profiles"]:
            for access in entry["accesses"]:
                origins.append(access["key"]["depends_on"]["origin_input"])
                semantic_components.append(access["key"]["semantic_name"])
        self.assertEqual(set(origins), {"predicted_reads[i]", "predicted_writes[i]"})
        self.assertFalse(any("actual_" in origin for origin in origins))
        # Profile-edge derivation joins accesses by resource + semantic component.
        # Read and write predictions therefore need the same semantic component to
        # produce read/write as well as write/write candidate relations.
        self.assertEqual(set(semantic_components), {"ethereum_address_storage_slot"})

    def test_seven_strategy_grid_expands_to_three_matched_samples(self):
        grid = ROOT / "evaluation" / "vegeta" / "s3-seven-strategy-smoke.grid.json"
        generator = ROOT / "tools" / "internal" / "generate-manifest-matrix.py"
        with tempfile.TemporaryDirectory() as tmp:
            manifest_path = Path(tmp) / "manifest.json"
            subprocess.run(
                [sys.executable, str(generator), str(grid), str(manifest_path)],
                cwd=ROOT,
                check=True,
                capture_output=True,
                text=True,
            )
            manifest = json.loads(manifest_path.read_text())
        self.assertEqual(len(manifest["runs"]), 21)
        modes = {run["mode"] for run in manifest["runs"]}
        self.assertEqual(
            modes,
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
        blocks = {}
        for run in manifest["runs"]:
            block = run["parameters"]["vegeta.measured_block"]
            blocks.setdefault(block, set()).add(run["mode"])
        self.assertEqual(len(blocks), 3)
        self.assertTrue(all(sample_modes == modes for sample_modes in blocks.values()))


class NativeS3ExecutionPreparationTests(unittest.TestCase):
    def test_amount_normalization_is_bounded_and_preserves_zero(self):
        self.assertEqual(native_execution_preparer.amount(0), 0)
        self.assertEqual(native_execution_preparer.amount(1), 2)
        self.assertLessEqual(native_execution_preparer.amount(2**255), 1_000_000)

    def test_erc721_transfer_from_decodes_word_two_and_preserves_source_owner(self):
        mapper = native_execution_preparer.TokenIdRemapper()
        owner = "0x0628f16e2d1c51f6fe84d4300b63330e75e3a183"
        recipient = "0x091ddba1cae98f1fc34349ce37ab47076ae2ae0d"
        conduit = "0x1e0049783f008a0085193e00003d00cd54003c71"
        def addr_word(addr):
            return ("00" * 12) + addr[2:]
        def calldata(token_id):
            return "0x23b872dd" + addr_word(owner) + addr_word(recipient) + token_id.to_bytes(32, "big").hex()
        action = {
            "action_id": 14,
            "native_instance_id": "cw721-mintable:0x48be0965618ed7b65e577487e1f74f12aca74ef7",
            "ethereum_input": calldata(3210),
            "arguments": {},
        }
        call = native_execution_preparer.translate(
            "cw721-mintable", "execute::TransferNft",
            "transferFrom(address,address,uint256)", {}, action, conduit, mapper,
        )
        self.assertEqual(call["source_owner"], owner)
        self.assertEqual(call["msg"]["transfer_nft"]["recipient"], recipient)
        self.assertEqual(call["msg"]["transfer_nft"]["token_id"], 1)
        action2 = dict(action, action_id=17, ethereum_input=calldata(3211))
        call2 = native_execution_preparer.translate(
            "cw721-mintable", "execute::TransferNft",
            "transferFrom(address,address,uint256)", {}, action2, conduit, mapper,
        )
        self.assertEqual(call2["msg"]["transfer_nft"]["token_id"], 2)

    def test_nft_initial_owner_prefers_first_transfer_over_query_placeholder(self):
        owners = {}
        priorities = {}
        iid = "cw721-mintable:collection"
        # note_nft_initial_owner expects a defaultdict-like outer mapping; preseed the instance.
        owners[iid] = {}
        native_execution_preparer.note_nft_initial_owner(
            owners, priorities, iid, 7, "native-s3-seed-owner", 1
        )
        native_execution_preparer.note_nft_initial_owner(
            owners, priorities, iid, 7, "alice-owner", 3
        )
        native_execution_preparer.note_nft_initial_owner(
            owners, priorities, iid, 7, "bob-owner", 3
        )
        self.assertEqual(owners[iid][7], "alice-owner")

    def test_cw721_initial_state_resolution_uses_high_level_abi_calls(self):
        class FakeResolver:
            def __init__(self):
                self.calls = []
            def call(self, to, data):
                self.calls.append((to, data))
                sel = data[:10]
                if sel == "0x6352211e":  # ownerOf
                    return "0x" + ("00" * 12) + ("11" * 20)
                if sel == "0x081812fc":  # getApproved
                    return "0x" + ("00" * 12) + ("22" * 20)
                if sel == "0xe985e9c5":  # isApprovedForAll
                    return "0x" + ("00" * 31) + "01"
                raise AssertionError(data)

        iid = "cw721-mintable:0x" + ("aa" * 20)
        owner = "0x" + ("11" * 20)
        operator = "0x" + ("33" * 20)
        resolved = native_execution_preparer.resolve_cw721_initial_state(
            FakeResolver(),
            {iid: {9: (1 << 200) + 7}},
            {(iid, owner, operator)},
        )
        self.assertEqual(resolved["owners"][iid][9], owner)
        self.assertEqual(resolved["approvals"][(iid, 9)], "0x" + ("22" * 20))
        self.assertIn((iid, owner, operator), resolved["operators"])
        self.assertEqual(resolved["statistics"]["owners_resolved"], 1)
        self.assertEqual(resolved["statistics"]["token_approvals_resolved"], 1)
        self.assertEqual(resolved["statistics"]["operator_approvals_true"], 1)

    def test_abi_erc721_initial_state_helpers_decode_addresses_and_bool(self):
        addr = "0x" + ("ab" * 20)
        self.assertEqual(
            native_execution_preparer.decode_abi_addr("0x" + ("00" * 12) + ("ab" * 20)),
            addr,
        )
        self.assertTrue(native_execution_preparer.decode_abi_bool("0x" + ("00" * 31) + "01"))
        self.assertFalse(native_execution_preparer.decode_abi_bool("0x" + ("00" * 32)))
        self.assertEqual(len(native_execution_preparer.abi_word_uint(2**255 + 9)), 64)
        self.assertEqual(len(native_execution_preparer.abi_word_addr(addr)), 64)

    def test_source_revert_scope_uses_topmost_failed_ancestor(self):
        by_id = {
            0: {"action_id": 0, "parent_action_id": None, "failed_frame": False},
            1: {"action_id": 1, "parent_action_id": 0, "failed_frame": True},
            6: {"action_id": 6, "parent_action_id": 1, "failed_frame": True},
            7: {"action_id": 7, "parent_action_id": 6, "failed_frame": True},
            8: {"action_id": 8, "parent_action_id": 7, "failed_frame": False},
        }
        self.assertEqual(
            native_execution_preparer.source_revert_scope_action_id(by_id[7], by_id), 1
        )
        # A successful child under the failed subtree still rolls back with that subtree.
        self.assertEqual(
            native_execution_preparer.source_revert_scope_action_id(by_id[8], by_id), 1
        )
        self.assertIsNone(
            native_execution_preparer.source_revert_scope_action_id(by_id[0], by_id)
        )

    def test_attach_revert_scope_marks_call_without_changing_successful_call(self):
        by_id = {
            0: {"action_id": 0, "parent_action_id": None, "failed_frame": False},
            4: {"action_id": 4, "parent_action_id": 0, "failed_frame": True},
        }
        failed_call = native_execution_preparer.attach_revert_scope(
            {"kind": "noop", "origin_action_id": 4}, by_id[4], by_id
        )
        self.assertEqual(failed_call["source_revert_scope_action_id"], 4)
        normal_call = native_execution_preparer.attach_revert_scope(
            {"kind": "noop", "origin_action_id": 0}, by_id[0], by_id
        )
        self.assertNotIn("source_revert_scope_action_id", normal_call)

    def test_exact_caller_mode_rejects_stale_native_plan_action(self):
        tx = {"from": "0x" + ("11" * 20)}
        action = {
            "action_id": 1,
            "parent_action_id": 0,
            "call_type": "CALL",
            "ethereum_caller": "0x" + ("33" * 20),
            "ethereum_code_address": "0x" + ("22" * 20),
        }
        parent = {
            "action_id": 0,
            "parent_action_id": None,
            "call_type": "CALL",
            "storage_context_address": "0x" + ("22" * 20),
        }
        with self.assertRaisesRegex(ValueError, "missing exact ethereum_msg_sender"):
            native_execution_preparer.caller_for(tx, action, {0: parent, 1: action}, "exact")

    def test_exact_caller_mode_uses_effective_msg_sender_not_trace_frame_from(self):
        tx = {"from": "0x" + ("11" * 20)}
        parent = {
            "action_id": 0,
            "parent_action_id": None,
            "call_type": "CALL",
            "storage_context_address": "0x" + ("22" * 20),
        }
        action = {
            "action_id": 1,
            "parent_action_id": 0,
            "call_type": "DELEGATECALL",
            "ethereum_caller": "0x" + ("33" * 20),
            "ethereum_msg_sender": "0x" + ("44" * 20),
            "ethereum_code_address": "0x" + ("22" * 20),
        }
        caller = native_execution_preparer.caller_for(
            tx, action, {0: parent, 1: action}, "exact"
        )
        self.assertEqual(caller, "0x" + ("44" * 20))

    def test_heuristic_caller_mode_retains_legacy_fallback_for_diagnostics(self):
        tx = {"from": "0x" + ("11" * 20)}
        parent = {
            "action_id": 0,
            "parent_action_id": None,
            "call_type": "CALL",
            "storage_context_address": "0x" + ("22" * 20),
        }
        action = {
            "action_id": 1,
            "parent_action_id": 0,
            "call_type": "CALL",
            "ethereum_code_address": "0x" + ("44" * 20),
        }
        caller = native_execution_preparer.caller_for(
            tx, action, {0: parent, 1: action}, "heuristic"
        )
        self.assertEqual(caller, "0x" + ("22" * 20))

    def test_positive_approval_uses_large_sentinel_and_zero_stays_zero(self):
        self.assertEqual(native_execution_preparer.approval_amount(0), 0)
        self.assertEqual(native_execution_preparer.approval_amount(1), native_execution_preparer.SEED)
        self.assertEqual(
            native_execution_preparer.approval_amount((1 << 256) - 1),
            native_execution_preparer.SEED,
        )

    def test_max_uint_approval_cannot_fold_below_following_transfer_from(self):
        mapper = native_execution_preparer.TokenIdRemapper()
        iid = "controlled-cw20:0x" + "aa" * 20
        spender = "0x" + "bb" * 20
        owner = "0x" + "cc" * 20
        recipient = "0x" + "dd" * 20

        approve = native_execution_preparer.translate(
            "controlled-cw20",
            "execute::increase_allowance_or_approve",
            "approve(address,uint256)",
            {},
            {
                "action_id": 1,
                "native_instance_id": iid,
                "arguments": {"spender": spender, "amount": (1 << 256) - 1},
                "ethereum_input": "0x",
            },
            owner,
            mapper,
        )
        transfer_from = native_execution_preparer.translate(
            "controlled-cw20",
            "execute::transfer_from",
            "transferFrom(address,address,uint256)",
            {},
            {
                "action_id": 2,
                "native_instance_id": iid,
                "arguments": {
                    "owner": owner,
                    "recipient": recipient,
                    "amount": 51_772_863_100,
                },
                "ethereum_input": "0x",
            },
            spender,
            mapper,
        )

        approved = int(approve["msg"]["approve"]["amount"])
        spend = int(transfer_from["msg"]["transfer_from"]["amount"])
        self.assertEqual(approved, native_execution_preparer.SEED)
        self.assertGreaterEqual(approved, spend)
        # This is the concrete regression: independent modulo folding used to produce
        # 639,936 approval versus 863,101 spend for the S3 USDC/Curve path.
        self.assertEqual(spend, 863_101)

    def test_token_id_remapper_preserves_equality_without_uint256_truncation(self):
        mapper = native_execution_preparer.TokenIdRemapper()
        huge = (1 << 255) + 123456789
        first = mapper.map("cw721-mintable:instance-a", huge)
        self.assertEqual(first, 1)
        self.assertEqual(mapper.map("cw721-mintable:instance-a", huge), first)
        second = mapper.map("cw721-mintable:instance-a", huge + (1 << 64))
        self.assertEqual(second, 2)
        self.assertNotEqual(first, second)
        # The namespace is per contract instance, matching native state isolation.
        self.assertEqual(mapper.map("cw721-mintable:instance-b", huge), 1)
        self.assertLessEqual(second, native_execution_preparer.U64_MAX)
        self.assertTrue(mapper.summary()["collision_free"])

    def test_huge_cw721_and_cw1155_ids_translate_into_u64_keys(self):
        mapper = native_execution_preparer.TokenIdRemapper()
        huge = (1 << 240) + 77
        word = huge.to_bytes(32, "big").hex()
        addr = ("00" * 12) + ("11" * 20)
        data_721 = "0x42842e0e" + addr + addr + word
        action_721 = {
            "action_id": 1,
            "native_instance_id": "cw721-mintable:0x" + "aa" * 20,
            "ethereum_input": data_721,
            "arguments": {},
        }
        call_721 = native_execution_preparer.translate(
            "cw721-mintable", "execute::TransferNft",
            "safeTransferFrom(address,address,uint256)",
            {}, action_721, "0x" + "22" * 20, mapper,
        )
        native_721 = call_721["msg"]["transfer_nft"]["token_id"]
        self.assertEqual(native_721, 1)
        self.assertLessEqual(native_721, native_execution_preparer.U64_MAX)

        data_1155 = "0xf242432a" + addr + addr + word + (1).to_bytes(32, "big").hex()
        action_1155 = {
            "action_id": 2,
            "native_instance_id": "cw1155-like:0x" + "bb" * 20,
            "ethereum_input": data_1155,
            "arguments": {},
        }
        call_1155 = native_execution_preparer.translate(
            "cw1155-like", "execute::SendFrom",
            "safeTransferFrom(address,address,uint256,uint256,bytes)",
            {}, action_1155, "0x" + "33" * 20, mapper,
        )
        native_1155 = call_1155["msg"]["send_from"]["token_id"]
        self.assertEqual(native_1155, 1)
        self.assertLessEqual(native_1155, native_execution_preparer.U64_MAX)

    def test_selector_rule_matching_respects_address_scope(self):
        family = "11" * 32
        selector_doc = {"rules": [{
            "runtime_family": family, "selector": "0xa9059cbb",
            "address_scope": ["0x" + "aa" * 20],
            "native_code_family": "cw20-base", "native_entrypoint": "execute::Transfer",
        }]}
        code = "60"
        actual_family = native_execution_preparer.runtime_family(code)
        selector_doc["rules"][0]["runtime_family"] = actual_family
        cache = {
            "0x" + "aa" * 20: {"code": code},
            "0x" + "bb" * 20: {"code": code},
        }
        idx, by_addr = native_execution_preparer.rule_index(selector_doc, cache)
        allowed = {"ethereum_code_address": "0x" + "aa" * 20, "selector": "0xa9059cbb"}
        denied = {"ethereum_code_address": "0x" + "bb" * 20, "selector": "0xa9059cbb"}
        self.assertIsNotNone(native_execution_preparer.match_rule(allowed, idx, by_addr))
        self.assertIsNone(native_execution_preparer.match_rule(denied, idx, by_addr))

    def test_native_conflict_metric_uses_bank_key_independent_of_caller_contract(self):
        block = {"transactions": [
            {"source_failed": False, "execution_status": "committed", "accesses": [
                {"kind": "bank_write", "contract": "c1", "key_hex": "aa", "reverted": False}
            ]},
            {"source_failed": False, "execution_status": "committed", "accesses": [
                {"kind": "bank_read", "contract": "c2", "key_hex": "aa", "reverted": False}
            ]},
        ]}
        rows = native_fidelity.native_rows(block, include_bank=True)
        pairs, _, _ = native_fidelity.pairs_from_rw(rows)
        self.assertEqual(pairs, {(0, 1)})
        storage_rows = native_fidelity.native_rows(block)
        storage_pairs, _, _ = native_fidelity.pairs_from_rw(storage_rows)
        self.assertEqual(storage_pairs, set())

    def test_reverted_native_writes_are_not_canonical_writers(self):
        block = {"transactions": [
            {"source_failed": True, "execution_status": "reverted", "accesses": [
                {"kind": "storage_write", "contract": "c", "key_hex": "aa", "reverted": True}
            ]},
            {"source_failed": False, "execution_status": "committed", "accesses": [
                {"kind": "storage_read", "contract": "c", "key_hex": "aa", "reverted": False}
            ]},
        ]}
        rows = native_fidelity.native_rows(block)
        pairs, _, _ = native_fidelity.pairs_from_rw(rows)
        self.assertEqual(pairs, set())

    def test_internal_reverted_native_write_in_committed_tx_is_read_like(self):
        block = {"transactions": [
            {"source_failed": False, "execution_status": "committed", "accesses": [
                {"kind": "storage_write", "contract": "c", "key_hex": "aa", "reverted": True}
            ]},
            {"source_failed": False, "execution_status": "committed", "accesses": [
                {"kind": "storage_read", "contract": "c", "key_hex": "aa", "reverted": False}
            ]},
        ]}
        rows = native_fidelity.native_rows(block)
        pairs, _, _ = native_fidelity.pairs_from_rw(rows)
        self.assertEqual(pairs, set())
        self.assertIn("storage:c:aa", rows[0][0])
        self.assertNotIn("storage:c:aa", rows[0][1])

    def test_conflict_dag_critical_path_can_chain_across_keys(self):
        self.assertEqual(native_fidelity.critical_path(4, {(0, 1), (1, 2), (2, 3)}), 4)

    def test_conflict_cause_keys_only_returns_shared_keys_with_writer(self):
        rows = [
            ({"r-only", "x"}, {"w"}),
            ({"r-only", "w"}, {"x"}),
        ]
        self.assertEqual(native_fidelity.conflict_cause_keys(rows, 0, 1), {"x", "w"})
        self.assertNotIn("r-only", native_fidelity.conflict_cause_keys(rows, 0, 1))

    def test_critical_path_edges_identifies_edges_on_any_longest_path(self):
        pairs = {(0, 1), (0, 2), (1, 3), (2, 3), (0, 3)}
        edges = native_fidelity.critical_path_edges(4, pairs)
        self.assertEqual(edges, {(0, 1), (0, 2), (1, 3), (2, 3)})
        self.assertNotIn((0, 3), edges)

    def test_storage_resource_guess_decodes_cw_storage_plus_namespace(self):
        raw = b"\x00\x08balances" + b"alice"
        self.assertEqual(native_fidelity.storage_resource_guess(raw.hex()), "balances")
        self.assertEqual(native_fidelity.storage_resource_guess(b"total_supply".hex()), "total_supply")

    def test_native_rows_excludes_bank_from_primary_storage_comparison(self):
        block={"transactions":[{"execution_status":"committed","source_failed":False,"accesses":[{"kind":"storage_write","contract":"c","key_hex":"aa","reverted":False},{"kind":"bank_write","contract":"c","key_hex":"bb","reverted":False}]}]}
        self.assertEqual(native_fidelity.native_rows(block)[0][1],{"storage:c:aa"})
        self.assertEqual(native_fidelity.native_rows(block,include_bank=True)[0][1],{"storage:c:aa","bank:bb"})

    def test_full_native_augmentation_counts_bank_added_pair(self):
        src=[{"block_number":1,"transactions":[{"reads":[],"writes":[]},{"reads":[],"writes":[]}]}]
        nat=[{"block_number":1,"transactions":[{"execution_status":"committed","source_failed":False,"accesses":[{"kind":"bank_write","contract":"c","key_hex":"01","reverted":False}]},{"execution_status":"committed","source_failed":False,"accesses":[{"kind":"bank_read","contract":"c","key_hex":"01","reverted":False}]}]}]
        aug=native_fidelity.full_native_augmentation(src,nat)
        self.assertEqual(aug["storage_only_native_pairs"],0)
        self.assertEqual(aug["full_native_pairs"],1)
        self.assertEqual(aug["additional_pairs_from_bank_ledger"],1)
        self.assertEqual(aug["critical_path_sum"]["bank_delta"],1)

    def test_fp_fractional_pair_credit_conserves_pair_count(self):
        src = [{"block_number": 1, "transactions": [
            {"reads": [], "writes": []},
            {"reads": [], "writes": []},
            {"reads": [], "writes": []},
        ]}]
        nat = [{"block_number": 1, "transactions": [
            {"execution_status": "committed", "source_failed": False, "accesses": [
                {"kind": "storage_write", "contract": "c", "key_hex": (b"\x00\x08balancesA").hex(), "family": "cw20-base", "instance_id": "cw20-base:x", "semantic_action": "execute::transfer", "reverted": False},
                {"kind": "storage_write", "contract": "c", "key_hex": (b"\x00\x0aallowancesA").hex(), "family": "cw20-base", "instance_id": "cw20-base:x", "semantic_action": "execute::approve", "reverted": False},
            ]},
            {"execution_status": "committed", "source_failed": False, "accesses": [
                {"kind": "storage_read", "contract": "c", "key_hex": (b"\x00\x08balancesA").hex(), "family": "cw20-base", "instance_id": "cw20-base:x", "semantic_action": "query::balance", "reverted": False},
                {"kind": "storage_read", "contract": "c", "key_hex": (b"\x00\x0aallowancesA").hex(), "family": "cw20-base", "instance_id": "cw20-base:x", "semantic_action": "query::allowance", "reverted": False},
            ]},
            {"execution_status": "committed", "source_failed": False, "accesses": []},
        ]}]
        a = native_fidelity.build_attribution(src, nat, None, 25)
        self.assertEqual(a["summary"]["false_positive_pairs"], 1)
        self.assertEqual(a["summary"]["false_positive_pair_key_incidences"], 2)
        self.assertAlmostEqual(sum(x["pair_credit"] for x in a["false_positive"]["by_family"]), 1.0)
        self.assertAlmostEqual(sum(x["pair_credit"] for x in a["false_positive"]["top_concrete_keys"]), 1.0)



if __name__ == "__main__":
    unittest.main()
