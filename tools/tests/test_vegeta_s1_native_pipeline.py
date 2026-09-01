import json
import importlib.util
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class VegetaS1NativePipelineTests(unittest.TestCase):
    def run_py(self, rel, *args, check=True):
        return subprocess.run([sys.executable, str(ROOT / rel), *map(str, args)], text=True, capture_output=True, check=check)

    def test_prepare_inputs_strips_access_sets_and_keeps_gas(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            corpus = td / "corpus.jsonl"
            corpus.write_text(json.dumps({
                "schema_version": 1, "block_number": 10, "block_hash": "0xabc", "timestamp": 123,
                "transactions": [{
                    "tx_index": 0, "tx_hash": "0x01", "from": "0x" + "11"*20,
                    "to": "0x" + "22"*20, "selector": "0xa9059cbb", "input": "0x", "value": "0x0",
                    "gas_used": 21000, "failed": False,
                    "reads": ["evm/" + "33"*20 + "/" + "00"*32],
                    "writes": ["evm/" + "44"*20 + "/" + "01"*32],
                }],
            }) + "\n")
            out = td / "out"
            self.run_py("tools/vegeta/prepare-vegeta-s1-inputs.py", "--corpus", corpus, "--output-dir", out)
            thin = json.loads((out / "thin-corpus.jsonl").read_text())
            tx = thin["transactions"][0]
            self.assertNotIn("reads", tx); self.assertNotIn("writes", tx)
            self.assertEqual(tx["gas_used"], 21000)
            addresses = {x["address"] for x in json.loads((out / "relevant-addresses.json").read_text())["addresses"]}
            self.assertIn("0x" + "22"*20, addresses)
            self.assertIn("0x" + "33"*20, addresses)
            self.assertIn("0x" + "44"*20, addresses)

    def test_s1_prefix_validator_detects_source_identity(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            block = {"block_number": 16774645, "block_hash": "0xbeef", "transactions": [{"tx_index": 0, "tx_hash": "0x01"}]}
            s1 = td / "s1.jsonl"; s3 = td / "s3.jsonl"
            s1.write_text(json.dumps(block)+"\n"); s3.write_text(json.dumps(block)+"\n")
            p = self.run_py("tools/vegeta/validate-vegeta-s1-prefix.py", "--s1-corpus", s1, "--s3-corpus", s3, "--prefix-blocks", "1")
            self.assertIn("PASS", p.stdout)

    def test_gas_compute_weights_respects_block_prefix(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td); plan = td / "plan.jsonl"; out = td / "weights.jsonl"; summary = td / "summary.json"
            plan.write_text("".join(json.dumps({"block_number": bn, "transactions": [
                {"tx_index": 0, "tx_hash": f"0x{bn:x}", "source_compute_proxy": bn}
            ]})+"\n" for bn in (7, 8, 9)))
            self.run_py("tools/vegeta/build-native-s3-compute-weights.py", "--execution-plan", plan, "--fallback-plan-gas", "--max-blocks", "2", "--output", out, "--summary", summary)
            rows = [json.loads(line) for line in out.read_text().splitlines() if line.strip()]
            self.assertEqual([r["block_number"] for r in rows], [7, 8])
            self.assertEqual(json.loads(summary.read_text())["transactions"], 2)

    def test_gas_compute_weights_stream_without_exact_traces(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td); plan = td / "plan.jsonl"; out = td / "weights.jsonl"; summary = td / "summary.json"
            plan.write_text(json.dumps({"block_number": 7, "transactions": [
                {"tx_index": 0, "tx_hash": "0xa", "source_compute_proxy": 11},
                {"tx_index": 1, "tx_hash": "0xb", "source_compute_proxy": 13},
            ]})+"\n")
            self.run_py("tools/vegeta/build-native-s3-compute-weights.py", "--execution-plan", plan, "--fallback-plan-gas", "--output", out, "--summary", summary)
            s = json.loads(summary.read_text())
            self.assertEqual(s["compute_metric"], "gas_used")
            self.assertEqual(s["source_compute_units_total"], 24)
            self.assertEqual(s["memory_mode"], "streaming")

    def test_prepared_only_validation_does_not_require_native_access_audit(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            (td / "execution-plan.jsonl").write_text(json.dumps({"block_number": 1, "transactions": []})+"\n")
            (td / "execution-manifest.json").write_text(json.dumps({
                "normalization": {"caller_provenance": {
                    "mode": "exact", "source": "derived-geth-callTracer-effective-msg.sender", "missing_actions": 0,
                }}
            })+"\n")
            p = self.run_py("tools/vegeta/validate-native-s3-execution.py", "--output-dir", td, "--prepared-only")
            self.assertIn("accepted: yes", p.stdout)

    def test_wasmd_large_workload_streaming_is_wired(self):
        main = (ROOT / "benchmarks/cosmos-wasmd-blockstm-s3/main.go").read_text()
        eval_sh = (ROOT / "scripts/eval-wasmd.sh").read_text()
        wrapper = (ROOT / "tools/legacy-scripts/run-vegeta-s1-wasmd-eval.sh").read_text()
        self.assertIn('"stream-plan"', main)
        self.assertIn("openPlanStream", main)
        self.assertIn("prepareBlockCalls", main)
        self.assertIn("EVAL_WASMD_STREAM_PLAN", eval_sh)
        self.assertIn("EVAL_WASMD_STREAM_PLAN=1", wrapper)
        self.assertIn("EVAL_WASMD_MAX_BLOCKS", eval_sh)
        self.assertIn('"max-blocks"', main)
        self.assertIn('MODE" == "smoke"', wrapper)
        self.assertIn("EVAL_WASMD_MAX_BLOCKS=101", wrapper)


    def test_s1_v2_family_map_has_reviewed_drop_and_stargate_extensions(self):
        doc = json.loads((ROOT / "evaluation/vegeta/s1-native-family-map.v2.json").read_text())
        self.assertEqual(doc["expected_native_code_families"], 12)
        self.assertEqual(doc["expected_profile_mappings"], 30)
        self.assertIn("cw721-drop", doc["native_code_families"])
        self.assertIn("stargate-cw20", doc["native_code_families"])
        self.assertIn("universal-router", doc["native_code_families"])
        owner_to_family = {}
        for row in doc["profile_mappings"]:
            for owner in row.get("storage_owner_scope") or []:
                owner_to_family[owner.lower()] = row["native_code_family"]
        self.assertEqual(owner_to_family["0xa6cd272874ee7c872eb66801eff62784c0b13285"], "cw721-drop")
        self.assertEqual(owner_to_family["0xaf5191b0de278c7286d6c7cc6ab6bb8a73ba2cd6"], "stargate-cw20")
        self.assertEqual(owner_to_family["0xef1c6e67703c7bd7107eed8303fbe6ec2554bf6b"], "universal-router")

    def test_s1_reviewed_native_manifest_source_and_symbolic_validate(self):
        p = self.run_py(
            "tools/vegeta/validate-native-s3-implementation.py",
            "--repo-root", ROOT,
            "--manifest", "evaluation/vegeta/s1-native-implementation-manifest.v1.json",
        )
        self.assertIn("accepted: yes", p.stdout)
        self.assertIn("families: 14", p.stdout)

    def test_selector_aware_conflict_gate_rejects_opaque_owner_label(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            owner = "0x" + "ab" * 20
            key = "evm/" + "ab" * 20 + "/" + "00" * 32
            corpus = td / "corpus.jsonl"
            plan = td / "plan.jsonl"
            out = td / "out.json"
            txt = td / "out.txt"
            corpus.write_text(json.dumps({
                "block_number": 7,
                "transactions": [
                    {"tx_index": 0, "reads": [], "writes": [key]},
                    {"tx_index": 1, "reads": [key], "writes": []},
                ],
            }) + "\n")
            base_action = {
                "storage_context_address": owner,
                "native_code_family": "cw721-drop",
                "translation_status": "mapped-native-call",
            }
            plan.write_text(json.dumps({
                "block_number": 7,
                "transactions": [
                    {"native_actions": [{**base_action, "dispatch": "mapped-entrypoint"}]},
                    {"native_actions": [{**base_action, "dispatch": "mapped-opaque-selector"}]},
                ],
            }) + "\n")
            self.run_py(
                "tools/vegeta/audit-vegeta-semantic-conflict-coverage.py",
                "--corpus", corpus, "--native-plan", plan,
                "--output", out, "--text-output", txt,
            )
            report = json.loads(out.read_text())
            self.assertEqual(report["total_unique_conflict_pairs"], 1)
            self.assertEqual(report["semantic_unique_conflict_pairs"], 0)
            self.assertEqual(report["coverage"], 0.0)
            self.assertEqual(report["schema_version"], 4)
            self.assertEqual(report["opaque_selector_conflict_gain"][0]["storage_owner"], owner)
            self.assertEqual(report["opaque_selector_conflict_gain"][0]["exact_single_selector_gain"], 1)

    def test_selector_aware_conflict_gate_accepts_reviewed_pair(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            owner = "0x" + "cd" * 20
            key = "evm/" + "cd" * 20 + "/" + "00" * 32
            corpus = td / "corpus.jsonl"
            plan = td / "plan.jsonl"
            out = td / "out.json"
            txt = td / "out.txt"
            corpus.write_text(json.dumps({
                "block_number": 8,
                "transactions": [
                    {"tx_index": 0, "reads": [], "writes": [key]},
                    {"tx_index": 1, "reads": [key], "writes": []},
                ],
            }) + "\n")
            action = {
                "storage_context_address": owner,
                "native_code_family": "cw721-drop",
                "translation_status": "mapped-native-call",
                "dispatch": "mapped-entrypoint",
            }
            plan.write_text(json.dumps({
                "block_number": 8,
                "transactions": [
                    {"native_actions": [action]}, {"native_actions": [action]},
                ],
            }) + "\n")
            self.run_py(
                "tools/vegeta/audit-vegeta-semantic-conflict-coverage.py",
                "--corpus", corpus, "--native-plan", plan,
                "--output", out, "--text-output", txt,
            )
            report = json.loads(out.read_text())
            self.assertEqual(report["semantic_unique_conflict_pairs"], 1)
            self.assertEqual(report["coverage"], 1.0)

    def test_state_read_counts_but_pure_does_not_and_revert_is_touch_only(self):
        with tempfile.TemporaryDirectory() as td:
            td=Path(td); owner="0x"+"ab"*20; key="evm/"+"ab"*20+"/"+"00"*32
            corpus=td/"c.jsonl"; plan=td/"p.jsonl"; out=td/"o.json"; txt=td/"o.txt"
            corpus.write_text(json.dumps({"block_number":11,"transactions":[{"tx_index":0,"reads":[],"writes":[key]},{"tx_index":1,"reads":[key],"writes":[]}]})+"\n")
            def a(effect, failed=False):
                return {"storage_context_address":owner,"native_code_family":"cw721-drop","translation_status":"mapped-native-call","dispatch":"mapped-entrypoint","semantic_effect":effect,"failed_frame":failed}
            plan.write_text(json.dumps({"block_number":11,"transactions":[{"native_actions":[a("READ_WRITE")]},{"native_actions":[a("STATE_READ")]}]})+"\n")
            self.run_py("tools/vegeta/audit-vegeta-semantic-conflict-coverage.py","--corpus",corpus,"--native-plan",plan,"--output",out,"--text-output",txt)
            r=json.loads(out.read_text()); self.assertEqual(r["coverage"],1.0); self.assertEqual(r["committed_coverage"],1.0)
            plan.write_text(json.dumps({"block_number":11,"transactions":[{"native_actions":[a("READ_WRITE")]},{"native_actions":[a("PURE")]}]})+"\n")
            self.run_py("tools/vegeta/audit-vegeta-semantic-conflict-coverage.py","--corpus",corpus,"--native-plan",plan,"--output",out,"--text-output",txt)
            r=json.loads(out.read_text()); self.assertEqual(r["coverage"],0.0)
            plan.write_text(json.dumps({"block_number":11,"transactions":[{"native_actions":[a("READ_WRITE",True)]},{"native_actions":[a("STATE_READ")]}]})+"\n")
            self.run_py("tools/vegeta/audit-vegeta-semantic-conflict-coverage.py","--corpus",corpus,"--native-plan",plan,"--output",out,"--text-output",txt)
            r=json.loads(out.read_text()); self.assertEqual(r["coverage"],1.0); self.assertEqual(r["committed_coverage"],0.0)
            self.assertEqual(r["selector_status_counts"]["reviewed_reverted_state_frame"],1)

    def test_inlined_reviewed_delegate_semantics_can_cover_proxy_owner(self):
        with tempfile.TemporaryDirectory() as td:
            td=Path(td); owner="0x"+"bc"*20; key="evm/"+"bc"*20+"/"+"00"*32
            corpus=td/"c.jsonl"; plan=td/"p.jsonl"; out=td/"o.json"; txt=td/"o.txt"
            corpus.write_text(json.dumps({"block_number":12,"transactions":[{"tx_index":0,"reads":[],"writes":[key]},{"tx_index":1,"reads":[key],"writes":[]}]})+"\n")
            write={"storage_context_address":owner,"native_code_family":"cw721-drop","translation_status":"inlined-delegatecall","dispatch":"inlined-reviewed-entrypoint","semantic_entrypoint":"execute::mint_drop","semantic_effect":"READ_WRITE","failed_frame":False}
            read={"storage_context_address":owner,"native_code_family":"cw721-drop","translation_status":"inlined-delegatecall","dispatch":"inlined-reviewed-entrypoint","semantic_entrypoint":"query::owner_of","semantic_effect":"STATE_READ","failed_frame":False}
            plan.write_text(json.dumps({"block_number":12,"transactions":[{"native_actions":[write]},{"native_actions":[read]}]})+"\n")
            self.run_py("tools/vegeta/audit-vegeta-semantic-conflict-coverage.py","--corpus",corpus,"--native-plan",plan,"--output",out,"--text-output",txt)
            r=json.loads(out.read_text()); self.assertEqual(r["coverage"],1.0); self.assertEqual(r["selector_status_counts"]["reviewed_inlined_delegate_entrypoint"],2)

    def test_opaque_selector_gain_reports_direct_and_two_selector_synergy(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td); owner = "0x" + "ef" * 20; key = "evm/" + "ef" * 20 + "/" + "00" * 32
            corpus = td / "corpus.jsonl"; plan = td / "plan.jsonl"; out = td / "out.json"; txt = td / "out.txt"
            corpus.write_text(json.dumps({"block_number": 9, "transactions": [
                {"tx_index":0,"reads":[],"writes":[key]}, {"tx_index":1,"reads":[key],"writes":[key]}, {"tx_index":2,"reads":[key],"writes":[]},
            ]}) + "\n")
            def action(dispatch, selector, failed=False, call_type="CALL"):
                return {"storage_context_address":owner,"native_code_family":"universal-router","translation_status":"mapped-native-call","dispatch":dispatch,"selector":selector,"failed_frame":failed,"call_type":call_type}
            plan.write_text(json.dumps({"block_number":9,"transactions":[
                {"native_actions":[action("mapped-entrypoint","0x3593564c")]},
                {"native_actions":[action("mapped-opaque-selector","0xaaaaaaaa")]},
                {"native_actions":[action("mapped-opaque-selector","0xbbbbbbbb")]},
            ]}) + "\n")
            self.run_py("tools/vegeta/audit-vegeta-semantic-conflict-coverage.py","--corpus",corpus,"--native-plan",plan,"--output",out,"--text-output",txt)
            report=json.loads(out.read_text()); rows={r["selector"]:r for r in report["opaque_selector_conflict_gain"]}
            self.assertEqual(rows["0xaaaaaaaa"]["exact_single_selector_gain"],1)
            self.assertEqual(rows["0xbbbbbbbb"]["exact_single_selector_gain"],1)
            self.assertTrue(any(x["exact_joint_gain"]==1 for x in report["top_two_selector_synergies"]))

    def test_reverted_opaque_frames_are_classified_as_review_potential(self):
        with tempfile.TemporaryDirectory() as td:
            td=Path(td); owner="0x"+"aa"*20; key="evm/"+"aa"*20+"/"+"00"*32
            corpus=td/"c.jsonl"; plan=td/"p.jsonl"; out=td/"o.json"; txt=td/"o.txt"
            corpus.write_text(json.dumps({"block_number":10,"transactions":[{"tx_index":0,"reads":[],"writes":[key]},{"tx_index":1,"reads":[key],"writes":[]}]})+"\n")
            base={"storage_context_address":owner,"native_code_family":"universal-router","translation_status":"mapped-native-call","call_type":"CALL"}
            plan.write_text(json.dumps({"block_number":10,"transactions":[{"native_actions":[{**base,"dispatch":"mapped-entrypoint","selector":"0x3593564c","failed_frame":False}]},{"native_actions":[{**base,"dispatch":"mapped-opaque-selector","selector":"0xdeadbeef","failed_frame":True}]}]})+"\n")
            self.run_py("tools/vegeta/audit-vegeta-semantic-conflict-coverage.py","--corpus",corpus,"--native-plan",plan,"--output",out,"--text-output",txt)
            row=json.loads(out.read_text())["opaque_selector_conflict_gain"][0]
            self.assertEqual(row["frame_classification"]["reverted"],1)
            self.assertEqual(row["exact_single_selector_gain"],1)
            self.assertTrue(row["review_priority_eligible"])

    def test_s1_transaction_deficit_reports_aligned_diagnostic_denominators(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            owner = "0x" + "ab" * 20
            key = "evm/" + "ab" * 20 + "/" + "00" * 32
            corpus = td / "corpus.jsonl"
            plan = td / "plan.jsonl"
            out = td / "deficit.json"
            txt = td / "deficit.txt"
            corpus.write_text(json.dumps({
                "block_number": 11,
                "transactions": [
                    {"tx_index": 0, "tx_hash": "0x01", "reads": [], "writes": [key]},
                    {"tx_index": 1, "tx_hash": "0x02", "reads": [key], "writes": []},
                    {"tx_index": 2, "tx_hash": "0x03", "reads": [], "writes": []},
                ],
            }) + "\n")
            plan.write_text(json.dumps({
                "block_number": 11,
                "transactions": [
                    {"tx_hash": "0x01", "native_actions": [{
                        "storage_context_address": owner, "native_code_family": "cw721-drop",
                        "translation_status": "mapped-native-call", "dispatch": "mapped-entrypoint",
                        "semantic_effect": "READ_WRITE", "selector": "0xaaaaaaaa", "failed_frame": False,
                    }]},
                    {"tx_hash": "0x02", "native_actions": [{
                        "storage_context_address": owner, "native_code_family": "cw721-drop",
                        "translation_status": "mapped-native-call", "dispatch": "mapped-opaque-selector",
                        "semantic_effect": "OPAQUE", "selector": "0x29a0eee8", "failed_frame": False,
                        "call_type": "CALL",
                    }]},
                    {"tx_hash": "0x03", "native_actions": [{
                        "ethereum_code_address": "0x" + "cd" * 20,
                        "translation_status": "background-fallback", "dispatch": "background-fallback",
                        "semantic_effect": "OPAQUE", "selector": "0xdeadbeef", "failed_frame": False,
                        "call_type": "CALL",
                    }]},
                ],
            }) + "\n")
            self.run_py(
                "tools/vegeta/analyze-vegeta-s1-transaction-deficit.py",
                "--corpus", corpus, "--native-plan", plan,
                "--output", out, "--text-output", txt, "--target-coverage", "0.80",
            )
            report = json.loads(out.read_text())
            all_row = report["denominators"]["all_source_transactions"]
            state_row = report["denominators"]["source_storage_access_transactions"]
            conflict_row = report["denominators"]["source_conflict_participating_transactions"]
            self.assertEqual((all_row["successful_reviewed_state_transactions"], all_row["transactions"]), (1, 3))
            self.assertEqual((state_row["successful_reviewed_state_transactions"], state_row["transactions"]), (1, 2))
            self.assertEqual((conflict_row["successful_reviewed_state_transactions"], conflict_row["transactions"]), (1, 2))
            self.assertEqual(report["additional_successful_reviewed_state_transactions_needed_for_current_gate"], 2)
            contention = report["contention_scheduler_diagnostic"]
            self.assertEqual(contention["target_successful_reviewed_state_transactions"], 2)
            self.assertEqual(contention["additional_successful_reviewed_state_transactions_needed"], 1)
            self.assertFalse(contention["target_met"])
            self.assertFalse(contention["publication_gate_changed"])
            self.assertIn("contention-oriented 80% target", txt.read_text())
            self.assertIn("FAIL", txt.read_text())
            candidate = report["mapped_owner_opaque_candidates"][0]
            self.assertEqual(candidate["selector"], "0x29a0eee8")
            self.assertEqual(candidate["deficit_transactions"], 1)
            self.assertEqual(candidate["conflict_participant_deficit_transactions"], 1)
            self.assertEqual(candidate["source_state_access_deficit_transactions"], 1)

    def test_s1_readiness_profiles_keep_scheduler_and_replay_claims_separate(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            translation = td / "translation.json"
            semantic = td / "semantic.json"
            deficit = td / "deficit.json"
            out = td / "ready.json"
            txt = td / "ready.txt"
            translation.write_text(json.dumps({
                "calls": {"reviewed_state_touch_frame_coverage": 0.41},
                "implementation_readiness": {"native_execution_ready": True},
            }))
            semantic.write_text(json.dumps({
                "coverage": 0.954,
                "block_balanced": {"median_coverage": 0.854},
            }))
            deficit.write_text(json.dumps({
                "denominators": {
                    "all_source_transactions": {
                        "transactions": 1000,
                        "successful_reviewed_state_transactions": 721,
                        "successful_reviewed_state_coverage": 0.721,
                    },
                    "source_conflict_participating_transactions": {
                        "transactions": 200,
                        "successful_reviewed_state_transactions": 161,
                        "successful_reviewed_state_coverage": 0.805,
                    },
                }
            }))
            self.run_py(
                "tools/vegeta/evaluate-vegeta-s1-readiness.py",
                "--translation-coverage", translation,
                "--semantic-conflict-coverage", semantic,
                "--transaction-deficit", deficit,
                "--profile", "scheduler-fidelity",
                "--output", out,
                "--text-output", txt,
            )
            report = json.loads(out.read_text())
            self.assertTrue(report["profiles"]["scheduler-fidelity"]["ready"])
            self.assertFalse(report["profiles"]["semantic-replay"]["ready"])
            self.assertTrue(report["selected_profile_ready"])
            self.assertTrue(report["common_gates"]["reviewed_state_touch_frame_coverage"]["diagnostic"])
            readiness_text = txt.read_text()
            self.assertIn("scheduler-fidelity", readiness_text)
            self.assertIn("semantic-replay", readiness_text)
            self.assertIn("DIAGNOSTIC (not gated)", readiness_text)

            proc = subprocess.run([
                sys.executable, str(ROOT / "tools/vegeta/evaluate-vegeta-s1-readiness.py"),
                "--translation-coverage", str(translation),
                "--semantic-conflict-coverage", str(semantic),
                "--transaction-deficit", str(deficit),
                "--profile", "semantic-replay",
            ], cwd=ROOT, text=True, capture_output=True)
            self.assertNotEqual(proc.returncode, 0)
            self.assertIn("below threshold", proc.stderr)

    def test_blitkin_mint_selector_is_owner_scoped_not_family_wide(self):
        planner_path = ROOT / "tools/vegeta/build-native-s3-plan.py"
        sys.path.insert(0, str(ROOT / "tools/vegeta"))
        spec = importlib.util.spec_from_file_location("vegeta_build_native_owner_scope", planner_path)
        planner = importlib.util.module_from_spec(spec); spec.loader.exec_module(planner)
        blitkin = "0xbd18e233e12f2a066f5b5a351285ab5a39b1f2ac"
        other = "0x" + "de" * 20
        profile = "profile-cw721-drop"
        frozen = {
            "dataset": "vegeta-s1",
            "profile_mappings": [{
                "ethereum_profile_family": profile,
                "native_code_family": "cw721-drop",
                "storage_owner_scope": [blitkin, other],
            }],
        }
        cache = {blitkin: {"code": "0x6000"}, other: {"code": "0x6000"}}
        resolver = planner.FamilyResolver(frozen, cache, {"resolution_records": []})
        def word(n): return int(n).to_bytes(32, "big").hex()
        calldata = "0x29a0eee8" + word(2) + word(7)
        frame = {"type": "CALL", "from": "0x" + "11" * 20, "to": blitkin, "input": calldata, "value": "0x0"}
        mapped = planner.translate_call_tree(frame, resolver)[0]
        self.assertEqual(mapped["dispatch"], "mapped-entrypoint")
        self.assertEqual(mapped["native_entrypoint"], "execute::mint_drop_one")
        self.assertEqual(mapped["arguments"], {"trunk_id": 2, "critter_id": 7})
        other_frame = {**frame, "to": other}
        opaque = planner.translate_call_tree(other_frame, resolver)[0]
        self.assertEqual(opaque["dispatch"], "mapped-opaque-selector")
        self.assertEqual(opaque["native_entrypoint"], "opaque::0x29a0eee8")
        self.assertNotIn("0x29a0eee8", planner.S1_ENTRYPOINT_EXTENSIONS["cw721-drop"])
        self.assertIn("0x29a0eee8", planner.S1_OWNER_ENTRYPOINT_EXTENSIONS[blitkin])

        # c96602d9 == allowlistMint(uint8,uint8,bytes32[]) for the reviewed Blitkin owner.
        proof = word(2) + ("ab" * 32) + ("cd" * 32)
        allowlist = "0xc96602d9" + word(72) + word(13) + word(96) + proof
        allowlist_frame = {**frame, "input": allowlist, "value": hex(50_000_000_000_000_000)}
        allowlist_mapped = planner.translate_call_tree(allowlist_frame, resolver)[0]
        self.assertEqual(allowlist_mapped["dispatch"], "mapped-entrypoint")
        self.assertEqual(allowlist_mapped["native_entrypoint"], "execute::allowlist_mint_drop_one")
        self.assertEqual(allowlist_mapped["arguments"], {"trunk_id": 72, "critter_id": 13})
        allowlist_opaque = planner.translate_call_tree({**allowlist_frame, "to": other}, resolver)[0]
        self.assertEqual(allowlist_opaque["dispatch"], "mapped-opaque-selector")
        self.assertNotIn("0xc96602d9", planner.S1_ENTRYPOINT_EXTENSIONS["cw721-drop"])
        self.assertIn("0xc96602d9", planner.S1_OWNER_ENTRYPOINT_EXTENSIONS[blitkin])

        mia = "0x885523263378d6f27a5b8c533ad3b05ab9e105b5"
        mia_profile = "profile-mia"
        mia_frozen = {
            "dataset": "vegeta-s1",
            "profile_mappings": [{
                "ethereum_profile_family": mia_profile,
                "native_code_family": "cw721-mintable",
                "storage_owner_scope": [mia, other],
            }],
        }
        mia_resolver = planner.FamilyResolver(mia_frozen, {mia: {"code": "0x6000"}, other: {"code": "0x6000"}}, {"resolution_records": []})
        mia_frame = {"type": "CALL", "from": "0x" + "12" * 20, "to": mia, "input": "0xfd883998", "value": "0x0"}
        mia_mapped = planner.translate_call_tree(mia_frame, mia_resolver)[0]
        self.assertEqual(mia_mapped["dispatch"], "mapped-entrypoint")
        self.assertEqual(mia_mapped["native_entrypoint"], "execute::mint_verified_event")
        self.assertEqual(mia_mapped["semantic_effect"], "READ_WRITE")
        mia_opaque = planner.translate_call_tree({**mia_frame, "to": other}, mia_resolver)[0]
        self.assertEqual(mia_opaque["dispatch"], "mapped-opaque-selector")
        self.assertNotIn("0xfd883998", planner.ENTRYPOINTS["cw721-mintable"])
        self.assertIn("0xfd883998", planner.S1_OWNER_ENTRYPOINT_EXTENSIONS[mia])

        # Blur Exchange V1 cancelOrder(Order) is source/ABI verified as selector 0xf4acd740.
        blur = "0x000000000000ad05ccc4f10045630fb830b95127"
        blur_other = "0x" + "ef" * 20
        blur_frozen = {
            "dataset": "vegeta-s1",
            "profile_mappings": [{
                "ethereum_profile_family": "profile-marketplace",
                "native_code_family": "marketplace-router",
                "storage_owner_scope": [blur, blur_other],
            }],
        }
        blur_resolver = planner.FamilyResolver(
            blur_frozen, {blur: {"code": "0x6000"}, blur_other: {"code": "0x6000"}},
            {"resolution_records": []},
        )
        blur_frame = {
            "type": "CALL", "from": "0x" + "13" * 20, "to": blur,
            "input": "0xf4acd740" + word(1) + word(2), "value": "0x0",
        }
        blur_mapped = planner.translate_call_tree(blur_frame, blur_resolver)[0]
        self.assertEqual(blur_mapped["dispatch"], "mapped-entrypoint")
        self.assertEqual(blur_mapped["native_entrypoint"], "execute::cancel_order")
        self.assertEqual(blur_mapped["semantic_effect"], "READ_WRITE")
        blur_opaque = planner.translate_call_tree({**blur_frame, "to": blur_other}, blur_resolver)[0]
        self.assertEqual(blur_opaque["dispatch"], "mapped-opaque-selector")
        self.assertNotIn("0xf4acd740", planner.S1_ENTRYPOINT_EXTENSIONS["marketplace-router"])
        self.assertIn("0xf4acd740", planner.S1_OWNER_ENTRYPOINT_EXTENSIONS[blur])

    def test_reviewed_s1_selector_extensions_and_execution_adapters(self):
        planner_path = ROOT / "tools/vegeta/build-native-s3-plan.py"
        sys.path.insert(0, str(ROOT / "tools/vegeta"))
        spec = importlib.util.spec_from_file_location("vegeta_build_native_selectors", planner_path)
        planner = importlib.util.module_from_spec(spec); spec.loader.exec_module(planner)
        self.assertIn("0x42966c68", planner.S1_ENTRYPOINT_EXTENSIONS["fiat-token-cw20"])
        self.assertIn("0xd505accf", planner.S1_ENTRYPOINT_EXTENSIONS["fiat-token-cw20"])
        self.assertIn("0x52c7f8dc", planner.S1_ENTRYPOINT_EXTENSIONS["xen-like"])
        self.assertIn("0xdb980f4f", planner.S1_ENTRYPOINT_EXTENSIONS["cw721-drop"])
        self.assertIn("0x97474f13", planner.S1_ENTRYPOINT_EXTENSIONS["cw721-drop"])
        self.assertIn("0x9ff70755", planner.S1_ENTRYPOINT_EXTENSIONS["cw721-drop"])
        self.assertIn("0xa8174404", planner.S1_ENTRYPOINT_EXTENSIONS["marketplace-router"])
        self.assertIn("0x46423aa7", planner.S1_ENTRYPOINT_EXTENSIONS["marketplace-router"])
        self.assertEqual(planner.S1_ENTRYPOINT_EXTENSIONS["universal-router"]["0x"][0], "reviewed::receive_eth_stateless")

        path = ROOT / "tools/vegeta/prepare-native-s3-execution.py"
        spec = importlib.util.spec_from_file_location("vegeta_prepare_native_extensions", path)
        mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
        def w(n): return int(n).to_bytes(32,"big")
        blur_input="0xf4acd740"+(w(1)+w(2)+w(3)).hex()
        blur_action={"ethereum_input":blur_input,"arguments":{},"native_instance_id":"marketplace-router:0x"+"77"*20,"action_id":24}
        blur_call=mod.translate("marketplace-router","execute::cancel_order",None,{},blur_action,"0x"+"88"*20,mod.TokenIdRemapper())
        self.assertEqual(blur_call["kind"],"execute")
        self.assertIn("cancel_order",blur_call["msg"])
        self.assertTrue(blur_call["msg"]["cancel_order"]["order_id"])
        owner="0x"+"11"*20; spender="0x"+"22"*20
        data="0xd505accf" + (bytes.fromhex("00"*12+"11"*20)+bytes.fromhex("00"*12+"22"*20)+w(77)+w(999)+w(27)+w(1)+w(2)).hex()
        a={"ethereum_input":data,"arguments":{"owner":owner,"spender":spender,"amount":77},"native_instance_id":"fiat-token-cw20:0x"+"33"*20,"action_id":1}
        call=mod.translate("fiat-token-cw20","execute::permit",None,{},a,"0x"+"44"*20,mod.TokenIdRemapper())
        self.assertEqual(call["sender"],owner); self.assertEqual(call["msg"]["permit"]["amount"],str(mod.approval_amount(77)))
        mint="0xdb980f4f"+(w(3)+w(5)).hex(); a={"ethereum_input":mint,"arguments":{"phase_index":3,"quantity":5},"native_instance_id":"cw721-drop:0x"+"55"*20,"action_id":2}
        call=mod.translate("cw721-drop","execute::mint_phase_drop",None,{},a,"0x"+"66"*20,mod.TokenIdRemapper())
        self.assertEqual(call["msg"]["mint_drop"]["quantity"],5); self.assertEqual(call["msg"]["mint_drop"]["stage_key"],"phase:3")
        one="0x29a0eee8"+(w(2)+w(7)).hex(); a={"ethereum_input":one,"arguments":{"trunk_id":2,"critter_id":7},"native_instance_id":"cw721-drop:0x"+"55"*20,"action_id":22}
        call=mod.translate("cw721-drop","execute::mint_drop_one",None,{},a,"0x"+"66"*20,mod.TokenIdRemapper())
        self.assertEqual(call["msg"]["mint_drop"]["quantity"],1)
        # allowlistMint(uint8,uint8,bytes32[]): head is two traits + proof offset; proof has 2 words.
        allowlist="0xc96602d9"+(w(72)+w(13)+w(96)+w(2)+w(111)+w(222)).hex()
        a={"ethereum_input":allowlist,"arguments":{"trunk_id":72,"critter_id":13},"native_instance_id":"cw721-drop:0x"+"55"*20,"action_id":23}
        call=mod.translate("cw721-drop","execute::allowlist_mint_drop_one",None,{},a,"0x"+"66"*20,mod.TokenIdRemapper())
        self.assertEqual(call["msg"]["mint_drop"]["quantity"],1)
        self.assertEqual(call["source_allowlist_traits"],{"trunk_id":72,"critter_id":13})
        self.assertEqual(call["source_allowlist_proof_words"],2)
        # mintBatch(uint64[] quantities,bytes32[][] proofs,uint256[] phaseIndices,uint64 publicQuantity)
        # Head offsets: quantities at 128 bytes; dummy empty proofs/phase arrays follow.
        head=w(128)+w(224)+w(256)+w(4)
        quantities=w(2)+w(3)+w(5)
        empty_proofs=w(0); empty_phases=w(0)
        batch="0x9ff70755"+(head+quantities+empty_proofs+empty_phases).hex()
        a={"ethereum_input":batch,"arguments":{},"native_instance_id":"cw721-drop:0x"+"55"*20,"action_id":3}
        call=mod.translate("cw721-drop","execute::mint_batch_drop",None,{},a,"0x"+"66"*20,mod.TokenIdRemapper())
        self.assertEqual(call["msg"]["mint_drop"]["quantity"],12)

    def test_s1_owner_scoped_airdrop_and_event_backed_drop_adapters(self):
        planner_path = ROOT / "tools/vegeta/build-native-s3-plan.py"
        sys.path.insert(0, str(ROOT / "tools/vegeta"))
        spec = importlib.util.spec_from_file_location("vegeta_build_native_airdrops", planner_path)
        planner = importlib.util.module_from_spec(spec); spec.loader.exec_module(planner)
        expected = {
            "0x798116c6858dc4be729820d36554c4c427629744": ("0xba09f3d7", "execute::airdrop_public_drop"),
            "0x925fe29ff5db1614e1344c803543ccbf60fd1641": ("0xcc47a40b", "execute::reserve_drop"),
            "0xf66ef61f504a6d326d7bf1771f4b613af57c7126": ("0xcc47a40b", "execute::reserve_drop"),
            "0x0e6d176b5c50e2600da92c8ea7f4eed178e9bd07": ("0x8ba4cc3c", "execute::airdrop_drop"),
            "0x1b1d2dccc2d3f25d7791e9dc4751856ec5eeafaa": ("0xc204642c", "execute::airdrop_array_drop"),
            "0x7974e0b19d8ee4daf3fdfecb2420507c198d3dbe": ("0x93a69f89", "execute::airdrop_phase_drop"),
            "0xeae506c1bcd0f77f0802ca630f65bca442ba0bd9": ("0x2f6f98e1", "execute::mint_event_backed_drop"),
        }
        for owner, (selector, entrypoint) in expected.items():
            self.assertEqual(planner.S1_OWNER_ENTRYPOINT_EXTENSIONS[owner][selector][0], entrypoint)
            self.assertNotIn(selector, planner.S1_ENTRYPOINT_EXTENSIONS["cw721-drop"])

        path = ROOT / "tools/vegeta/prepare-native-s3-execution.py"
        spec = importlib.util.spec_from_file_location("vegeta_prepare_native_airdrops", path)
        mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
        def w(n): return int(n).to_bytes(32, "big")
        def aw(addr): return bytes.fromhex("00" * 12 + addr[2:])
        recipient = "0x" + "34" * 20
        iid = "cw721-drop:0x" + "12" * 20
        caller = "0x" + "56" * 20
        # airdropPublic(uint64[],address[]): one observed S1 pair.
        data = "0xba09f3d7" + (w(64) + w(128) + w(1) + w(44) + w(1) + aw(recipient)).hex()
        call = mod.translate("cw721-drop", "execute::airdrop_public_drop", None, {}, {"ethereum_input": data, "arguments": {}, "native_instance_id": iid}, caller, mod.TokenIdRemapper())
        self.assertEqual(call["msg"]["mint_drop"]["quantity"], 44); self.assertEqual(call["msg"]["mint_drop"]["recipient"], recipient)
        # reserve/airdrop(address,uint256).
        data = "0xcc47a40b" + (aw(recipient) + w(10)).hex()
        call = mod.translate("cw721-drop", "execute::reserve_drop", None, {}, {"ethereum_input": data, "arguments": {"recipient": recipient, "quantity": 10}, "native_instance_id": iid}, caller, mod.TokenIdRemapper())
        self.assertEqual(call["msg"]["mint_drop"]["quantity"], 10)
        # airdrop(address[],uint256), one recipient in observed S1.
        data = "0xc204642c" + (w(64) + w(10) + w(1) + aw(recipient)).hex()
        call = mod.translate("cw721-drop", "execute::airdrop_array_drop", None, {}, {"ethereum_input": data, "arguments": {}, "native_instance_id": iid}, caller, mod.TokenIdRemapper())
        self.assertEqual(call["msg"]["mint_drop"]["quantity"], 10); self.assertEqual(call["msg"]["mint_drop"]["recipient"], recipient)
        # airdropForPhase(uint256,uint64[],address[]), one recipient/quantity in observed S1.
        data = "0x93a69f89" + (w(3) + w(96) + w(160) + w(1) + w(1) + w(1) + aw(recipient)).hex()
        call = mod.translate("cw721-drop", "execute::airdrop_phase_drop", None, {}, {"ethereum_input": data, "arguments": {}, "native_instance_id": iid}, caller, mod.TokenIdRemapper())
        self.assertEqual(call["msg"]["mint_drop"]["quantity"], 1); self.assertEqual(call["msg"]["mint_drop"]["recipient"], recipient)

        with tempfile.TemporaryDirectory() as td:
            td = Path(td); report = td / "mints.json"
            owner = "0x1b1d2dccc2d3f25d7791e9dc4751856ec5eeafaa"; tx_hash = "0x" + "aa" * 32
            report.write_text(json.dumps({
                "schema_version": 2, "dataset": "vegeta-s1",
                "summary": {"all_observed_sequences_plus_one": True, "all_token_ids_fit_u64": True},
                "owners": {owner: {"first_token_id": 1, "transactions": {tx_hash: {
                    "mint_count": 2, "token_ids": [1, 2], "recipients": [recipient, recipient],
                }}}},
            }) + "\n")
            sequence = mod.Cw721DropMintSequence(report)
            action = {"storage_context_address": owner, "ethereum_input": "0x2955a21d", "arguments": {"quantity": 5, "nonce": 7, "recipient": recipient}, "native_instance_id": "cw721-drop:" + owner, "action_id": 1}
            call = mod.translate("cw721-drop", "execute::signed_mint_drop", None, {"tx_hash": tx_hash}, action, caller, mod.TokenIdRemapper(), None, sequence)
            self.assertEqual(call["msg"]["mint_drop"]["quantity"], 2)
            self.assertEqual(call["source_requested_mint_quantity"], 5)
            self.assertEqual(call["source_committed_mint_quantity"], 2)

            event_owner = "0xeae506c1bcd0f77f0802ca630f65bca442ba0bd9"; event_hash = "0x" + "bb" * 32
            report.write_text(json.dumps({
                "schema_version": 2, "dataset": "vegeta-s1",
                "summary": {"all_observed_sequences_plus_one": True, "all_token_ids_fit_u64": True},
                "owners": {event_owner: {"first_token_id": 41, "transactions": {event_hash: {
                    "mint_count": 20, "token_ids": list(range(41, 61)), "recipients": [recipient] * 20,
                }}}},
            }) + "\n")
            sequence = mod.Cw721DropMintSequence(report)
            action = {"storage_context_address": event_owner, "ethereum_input": "0x2f6f98e1" + (w(1)+w(2)).hex(), "arguments": {}, "native_instance_id": "cw721-drop:" + event_owner, "action_id": 2}
            call = mod.translate("cw721-drop", "execute::mint_event_backed_drop", None, {"tx_hash": event_hash}, action, caller, mod.TokenIdRemapper(), None, sequence)
            self.assertEqual(call["msg"]["mint_drop"]["quantity"], 20); self.assertEqual(call["msg"]["mint_drop"]["recipient"], recipient)

    def test_cw721_drop_translation_audit_recognizes_airdrop_and_reserve_mint_paths(self):
        path = ROOT / "tools/vegeta/audit-vegeta-s1-cw721-drop-translation.py"
        spec = importlib.util.spec_from_file_location("vegeta_cw721_drop_translation_audit", path)
        mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
        for ep in (
            "execute::signed_mint_drop",
            "execute::purchase_drop",
            "execute::airdrop_public_drop",
            "execute::airdrop_array_drop",
            "execute::airdrop_phase_drop",
            "execute::reserve_drop",
        ):
            self.assertTrue(mod.is_drop_mint_entrypoint(ep), ep)
        for ep in ("execute::transfer_nft", "query::owner_of", "reviewed::token_uri_stateless"):
            self.assertFalse(mod.is_drop_mint_entrypoint(ep), ep)

    def test_reviewed_stateless_noop_does_not_require_native_instance(self):
        path = ROOT / "tools/vegeta/prepare-native-s3-execution.py"
        spec = importlib.util.spec_from_file_location("vegeta_prepare_native_noop", path)
        mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
        action = {"action_id": 9, "selector": "0x79df72bd", "ethereum_input": "0x79df72bd"}
        call = mod.translate(
            "marketplace-router", "reviewed::get_order_hash_stateless", None, {}, action,
            "0x" + "11"*20, mod.TokenIdRemapper(),
        )
        self.assertEqual(call["kind"], "noop")
        self.assertNotIn("instance_id", call)
        instances = {}; stats = {}
        iid = mod.register_translated_instance(call, "marketplace-router", instances, stats)
        self.assertIsNone(iid)
        self.assertEqual(instances, {})
        self.assertEqual(stats["reviewed_noop_calls"], 1)
        self.assertNotIn("contract_calls", stats)
        with self.assertRaisesRegex(ValueError, "missing required instance_id"):
            mod.register_translated_instance(
                {"kind": "execute", "origin_action_id": 10}, "marketplace-router", {}, {}
            )

    def test_stargate_mainnet_bridge_adapter_uses_qty_and_decodes_receive_payload(self):
        path = ROOT / "tools/vegeta/prepare-native-s3-execution.py"
        spec = importlib.util.spec_from_file_location("vegeta_prepare_native", path)
        mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
        def w(n): return int(n).to_bytes(32, "big")
        # sendTokens(uint16,bytes,uint256,address,bytes): quantity is ABI word 2.
        send = b"\x2e\x15\x23\x8c" + w(102) + w(160) + w(123456789) + w(0) + w(224)
        action = {"ethereum_input": "0x" + send.hex(), "native_instance_id": "stargate-cw20:0x" + "11"*20}
        call = mod.translate("stargate-cw20", "execute::bridge_send", None, {}, action, "0x" + "22"*20, mod.TokenIdRemapper())
        self.assertEqual(call["msg"]["bridge_send"]["amount"], str(mod.amount(123456789)))
        # lzReceive payload is abi.encode(bytes to,uint256 qty).
        recipient = bytes.fromhex("33"*20); src = bytes.fromhex("44"*20); qty = 777
        nested = w(64) + w(qty) + w(20) + recipient + b"\0"*12
        top = w(101) + w(128) + w(9) + w(192) + w(20) + src + b"\0"*12 + w(len(nested)) + nested
        recv = "0x001d3567" + top.hex()
        decoded_recipient, decoded_qty = mod.stargate_receive_payload(recv)
        self.assertEqual(decoded_recipient, "0x" + "33"*20)
        self.assertEqual(decoded_qty, qty)


    def test_cw721_drop_mint_sequence_alignment_and_strict_quantity_validation(self):
        path = ROOT / "tools/vegeta/prepare-native-s3-execution.py"
        spec = importlib.util.spec_from_file_location("vegeta_prepare_native_mints", path)
        mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
        with tempfile.TemporaryDirectory() as td:
            td = Path(td); report = td / "mints.json"
            owner = "0x" + "ab" * 20; tx_hash = "0x" + "11" * 32
            report.write_text(json.dumps({
                "schema_version": 2,
                "dataset": "vegeta-s1",
                "summary": {"all_observed_sequences_plus_one": True, "all_token_ids_fit_u64": True},
                "owners": {owner: {
                    "first_token_id": 3847,
                    "transactions": {tx_hash: {
                        "mint_count": 2, "token_ids": [3847, 3848],
                        "recipients": ["0x" + "cd" * 20, "0x" + "cd" * 20],
                    }},
                }},
            }) + "\n")
            sequence = mod.Cw721DropMintSequence(report)
            iid = "cw721-drop:" + owner
            self.assertEqual(sequence.first_token_id(iid), 3847)
            self.assertIsNone(sequence.owner_for_instance("cw721-drop:0x" + "ef" * 20))
            effect = sequence.mint_effect(
                {"storage_context_address": owner}, {"tx_hash": tx_hash}, require_recipient=True
            )
            self.assertEqual(effect["quantity"], 2)
            self.assertEqual(effect["recipient"], "0x" + "cd" * 20)
            msg = mod.instantiate_msg("cw721-drop", set(), iid, sequence)
            self.assertEqual(msg["next_token_id"], 3847)
            ok = mod.validate_drop_mint_translation(sequence, {(owner, tx_hash): 2})
            self.assertTrue(ok["validated"])
            self.assertTrue(mod.counts_for_drop_mint_event_validation({"source_failed": False}, {}))
            self.assertFalse(mod.counts_for_drop_mint_event_validation({"source_failed": True}, {}))
            self.assertFalse(mod.counts_for_drop_mint_event_validation(
                {"source_failed": False}, {"source_revert_scope_action_id": 17}
            ))
            with self.assertRaises(ValueError):
                mod.validate_drop_mint_translation(sequence, {(owner, tx_hash): 1})

    def test_prepare_wrapper_collects_and_reuses_cw721_mint_audit(self):
        prepare = (ROOT / "tools/legacy-scripts/run-vegeta-s1-prepare-native.sh").read_text()
        audit = (ROOT / "tools/legacy-scripts/run-vegeta-s1-cw721-mint-audit.sh").read_text()
        collector = (ROOT / "tools/vegeta/collect-vegeta-cw721-drop-mints.py").read_text()
        self.assertIn("run-vegeta-s1-cw721-mint-audit.sh", prepare)
        self.assertIn("--cw721-drop-mint-sequence", prepare)
        self.assertIn("eth_getLogs", collector)
        self.assertIn("ZERO_ADDRESS_TOPIC", collector)
        self.assertIn("all_observed_sequences_plus_one", collector)
        self.assertIn('"schema_version": 2', collector)
        self.assertIn('"recipients": []', collector)
        self.assertIn("MINT_SEQUENCE_CURRENT", prepare)
        self.assertIn("collect-vegeta-cw721-drop-mints.py", audit)

    def test_mia_selector_mint_audit_correlates_committed_effects_without_guessing_abi(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            plan = td / "plan.jsonl"; logs = td / "logs.json"; out = td / "audit.json"; txt = td / "audit.txt"
            owner = "0x885523263378d6f27a5b8c533ad3b05ab9e105b5"
            sender = "0x" + "12" * 20
            committed_hash = "0x" + "aa" * 32
            reverted_hash = "0x" + "bb" * 32
            extra_hash = "0x" + "cc" * 32
            action = {
                "action_id": 0, "parent_action_id": None, "storage_context_address": owner,
                "ethereum_code_address": owner, "ethereum_msg_sender": sender,
                "selector": "0xfd883998", "call_type": "CALL", "failed_frame": False,
                "dispatch": "mapped-opaque-selector", "semantic_effect": "OPAQUE",
            }
            plan.write_text(json.dumps({"block_number": 16774645, "transactions": [
                {"tx_hash": committed_hash, "source_failed": False, "native_actions": [action]},
                {"tx_hash": reverted_hash, "source_failed": False, "native_actions": [{**action, "failed_frame": True}]},
            ]}) + "\n")
            transfer_topic = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
            zero = "0x" + "00" * 32
            recipient = "0x" + "00" * 12 + sender[2:]
            def log(tx_hash, token_id, index):
                return {
                    "address": owner, "transactionHash": tx_hash,
                    "blockNumber": "0x1000000", "transactionIndex": "0x0", "logIndex": hex(index),
                    "topics": [transfer_topic, zero, recipient, "0x" + token_id.to_bytes(32, "big").hex()],
                }
            # One committed fd883998 mint and one unrelated owner mint. The reverted selector emits no log.
            logs.write_text(json.dumps([log(committed_hash, 7, 0), log(extra_hash, 8, 1)]))
            self.run_py(
                "tools/vegeta/audit-vegeta-s1-erc721-selector-mints.py",
                "--native-plan", plan, "--logs-json", logs, "--owner", owner, "--selector", "0xfd883998",
                "--output", out, "--text-output", txt,
            )
            report = json.loads(out.read_text()); summary = report["summary"]
            self.assertEqual(summary["source_committed_selector_actions"], 1)
            self.assertEqual(summary["source_reverted_selector_actions"], 1)
            self.assertTrue(summary["exactly_one_mint_event_per_committed_selector_tx"])
            self.assertTrue(summary["reverted_selector_txs_have_no_committed_mint_events"])
            self.assertTrue(summary["all_checked_mint_recipients_equal_msg_sender"])
            self.assertTrue(summary["target_token_ids_unique"])
            self.assertEqual(summary["target_distinct_token_id_count"], 1)
            self.assertEqual(summary["min_target_token_id"], 7)
            self.assertEqual(summary["max_target_token_id"], 7)
            self.assertEqual(summary["extra_owner_mint_transactions_not_using_target_selector"], 1)
            self.assertIn("High conflict gain alone", txt.read_text())


    def test_owner_scoped_erc721_selector_effect_audit_classifies_public_logs_and_calldata(self):
        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            plan = td / "plan.jsonl"; logs = td / "logs.json"; out = td / "audit.json"; txt = td / "audit.txt"
            owner = "0xbd18e233e12f2a066f5b5a351285ab5a39b1f2ac"
            sender = "0x" + "12" * 20
            committed_hash = "0x" + "aa" * 32
            reverted_hash = "0x" + "bb" * 32
            selector = "0xc96602d9"
            def word(n): return int(n).to_bytes(32, "big").hex()
            action = {
                "action_id": 0, "parent_action_id": None, "storage_context_address": owner,
                "ethereum_code_address": owner, "ethereum_msg_sender": sender,
                "ethereum_input": selector + word(7) + word(64), "ethereum_value": "0x0",
                "selector": selector, "call_type": "CALL", "failed_frame": False,
                "dispatch": "mapped-opaque-selector", "semantic_effect": "OPAQUE",
            }
            plan.write_text(json.dumps({"block_number": 16774645, "transactions": [
                {"tx_hash": committed_hash, "source_failed": False, "native_actions": [action]},
                {"tx_hash": reverted_hash, "source_failed": False, "native_actions": [{**action, "failed_frame": True}]},
            ]}) + "\n")
            transfer_topic = "0xddf252ad1be2c89b69c2b068fc378daa952ba7f163c4a11628f55a4df523b3ef"
            zero = "0x" + "00" * 32
            recipient = "0x" + "00" * 12 + sender[2:]
            logs.write_text(json.dumps([{
                "address": owner, "transactionHash": committed_hash,
                "blockNumber": "0x1000000", "transactionIndex": "0x0", "logIndex": "0x0",
                "topics": [transfer_topic, zero, recipient, "0x" + (9).to_bytes(32, "big").hex()],
                "data": "0x",
            }]))
            self.run_py(
                "tools/vegeta/audit-vegeta-s1-erc721-selector-effects.py",
                "--native-plan", plan, "--logs-json", logs, "--owner", owner, "--selector", selector,
                "--output", out, "--text-output", txt,
            )
            report = json.loads(out.read_text()); summary = report["summary"]
            self.assertEqual(summary["source_committed_selector_actions"], 1)
            self.assertEqual(summary["source_reverted_selector_actions"], 1)
            self.assertEqual(summary["erc721_transfer_effect_counts"], {"mint": 1})
            self.assertEqual(summary["reverted_selector_tx_with_committed_owner_logs"], 0)
            self.assertEqual(summary["calldata_byte_length_distribution"], {"68": 2})
            self.assertFalse(summary["semantic_promotion_performed"])
            self.assertIn("word[0]", txt.read_text())
            self.assertIn("never promotes", txt.read_text())

    def test_mia_verified_event_adapter_consumes_frozen_log_audit_and_fail_closes(self):
        path = ROOT / "tools/vegeta/prepare-native-s3-execution.py"
        spec = importlib.util.spec_from_file_location("vegeta_prepare_mia_verified", path)
        mod = importlib.util.module_from_spec(spec); spec.loader.exec_module(mod)
        with tempfile.TemporaryDirectory() as td:
            td = Path(td); audit = td / "mia.json"
            owner = "0x885523263378d6f27a5b8c533ad3b05ab9e105b5"
            sender = "0x" + "34" * 20
            committed = "0x" + "11" * 32; reverted = "0x" + "22" * 32
            base_summary = {
                "selector_transactions": 2, "selector_actions": 2,
                "committed_selector_transactions": 1, "target_mint_events": 1,
                "all_committed_selector_txs_have_mint_events": True,
                "exactly_one_mint_event_per_committed_selector_tx": True,
                "reverted_selector_txs_have_no_committed_mint_events": True,
                "all_checked_mint_recipients_equal_msg_sender": True,
                "target_token_ids_fit_u64": True,
                "extra_owner_mint_transactions_not_using_target_selector": 0,
            }
            audit.write_text(json.dumps({
                "dataset": "vegeta-s1", "owner": owner, "selector": "0xfd883998",
                "summary": base_summary,
                "transactions": {
                    committed: {
                        "committed_selector_actions": 1, "reverted_selector_actions": 0,
                        "committed_msg_senders": [sender],
                        "mint_events": [{"recipient": sender, "token_id": 765}],
                    },
                    reverted: {
                        "committed_selector_actions": 0, "reverted_selector_actions": 1,
                        "committed_msg_senders": [], "mint_events": [],
                    },
                },
            }) + "\n")
            audits = mod.Erc721SelectorMintAudits([audit]); ids = mod.TokenIdRemapper()
            def mia_word(n): return int(n).to_bytes(32, "big")
            def mia_input(token_id):
                return "0xfd883998" + (mia_word(int(sender, 16)) + mia_word(96) + mia_word(token_id) + mia_word(98) + mia_word(0) + mia_word(0) + mia_word(0) + mia_word(0)).hex()
            action = {
                "action_id": 7, "selector": "0xfd883998", "storage_context_address": owner,
                "native_instance_id": "cw721-mintable:" + owner, "ethereum_input": mia_input(765),
            }
            call = mod.translate(
                "cw721-mintable", "execute::mint_verified_event", None, {"tx_hash": committed},
                action, sender, ids, audits,
            )
            self.assertEqual(call["sender"], "native-s3-admin")
            self.assertEqual(call["msg"]["mint"]["owner"], sender)
            self.assertEqual(call["source_token_id"], 765)
            self.assertTrue(call["selector_mint_effect_audit"])
            reverted_action = {**action, "ethereum_input": mia_input(440)}
            reverted_call = mod.translate(
                "cw721-mintable", "execute::mint_verified_event", None, {"tx_hash": reverted},
                reverted_action, sender, ids, audits,
            )
            self.assertTrue(reverted_call["reverted_token_id_from_public_calldata"])
            self.assertEqual(reverted_call["source_token_id"],440)
            mismatch_action = {**action, "ethereum_input": mia_input(766)}
            with self.assertRaises(ValueError):
                mod.translate(
                    "cw721-mintable", "execute::mint_verified_event", None, {"tx_hash": committed},
                    mismatch_action, sender, mod.TokenIdRemapper(), audits,
                )
            with self.assertRaises(ValueError):
                mod.translate(
                    "cw721-mintable", "execute::mint_verified_event", None, {"tx_hash": committed},
                    action, sender, mod.TokenIdRemapper(), None,
                )

            duplicate = td / "duplicate.json"
            duplicate_doc = json.loads(audit.read_text())
            duplicate_doc["summary"]["selector_transactions"] = 3
            duplicate_doc["summary"]["selector_actions"] = 3
            duplicate_doc["summary"]["committed_selector_transactions"] = 2
            duplicate_doc["summary"]["target_mint_events"] = 2
            duplicate_doc["transactions"]["0x" + "33" * 32] = {
                "committed_selector_actions": 1, "reverted_selector_actions": 0,
                "committed_msg_senders": [sender],
                "mint_events": [{"recipient": sender, "token_id": 765}],
            }
            duplicate.write_text(json.dumps(duplicate_doc) + "\n")
            with self.assertRaises(ValueError):
                mod.Erc721SelectorMintAudits([duplicate])

    def test_s1_semantic_only_wrapper_and_locked_new_contracts(self):
        wrapper = (ROOT / "tools/legacy-scripts/run-vegeta-s1-semantic-coverage.sh").read_text()
        prepare = (ROOT / "tools/legacy-scripts/run-vegeta-s1-prepare-native.sh").read_text()
        lock = (ROOT / "benchmarks/Cargo.lock").read_text()
        self.assertIn("VEGETA_S1_REUSE_CACHED_COVERAGE_INPUTS=1", wrapper)
        self.assertIn("audit-vegeta-semantic-conflict-coverage.py", wrapper)
        self.assertIn("analyze-vegeta-s1-transaction-deficit.py", wrapper)
        self.assertIn("transaction-deficit.txt", wrapper)
        self.assertTrue((ROOT / "tools/legacy-scripts/run-vegeta-s1-blitkin-c96602d9-effect-audit.sh").exists())
        self.assertTrue((ROOT / "tools/legacy-scripts/run-vegeta-s1-mia-fd883998-effect-audit.sh").exists())
        self.assertTrue((ROOT / "tools/vegeta/audit-vegeta-s1-erc721-selector-effects.py").exists())
        self.assertIn("run-vegeta-s1-semantic-coverage.sh", prepare)
        self.assertIn("run-vegeta-s1-mia-mint-audit.sh", prepare)
        self.assertIn("--erc721-selector-mint-audit", prepare)
        self.assertIn('name = "acg-benchmark-native-s3-cw721-drop"', lock)
        self.assertIn('name = "acg-benchmark-native-s3-stargate-cw20"', lock)



if __name__ == "__main__":
    unittest.main()
