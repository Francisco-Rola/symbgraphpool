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
        owner="0x"+"11"*20; spender="0x"+"22"*20
        data="0xd505accf" + (bytes.fromhex("00"*12+"11"*20)+bytes.fromhex("00"*12+"22"*20)+w(77)+w(999)+w(27)+w(1)+w(2)).hex()
        a={"ethereum_input":data,"arguments":{"owner":owner,"spender":spender,"amount":77},"native_instance_id":"fiat-token-cw20:0x"+"33"*20,"action_id":1}
        call=mod.translate("fiat-token-cw20","execute::permit",None,{},a,"0x"+"44"*20,mod.TokenIdRemapper())
        self.assertEqual(call["sender"],owner); self.assertEqual(call["msg"]["permit"]["amount"],str(mod.approval_amount(77)))
        mint="0xdb980f4f"+(w(3)+w(5)).hex(); a={"ethereum_input":mint,"arguments":{"phase_index":3,"quantity":5},"native_instance_id":"cw721-drop:0x"+"55"*20,"action_id":2}
        call=mod.translate("cw721-drop","execute::mint_phase_drop",None,{},a,"0x"+"66"*20,mod.TokenIdRemapper())
        self.assertEqual(call["msg"]["mint_drop"]["quantity"],5); self.assertEqual(call["msg"]["mint_drop"]["stage_key"],"phase:3")
        # mintBatch(uint64[] quantities,bytes32[][] proofs,uint256[] phaseIndices,uint64 publicQuantity)
        # Head offsets: quantities at 128 bytes; dummy empty proofs/phase arrays follow.
        head=w(128)+w(224)+w(256)+w(4)
        quantities=w(2)+w(3)+w(5)
        empty_proofs=w(0); empty_phases=w(0)
        batch="0x9ff70755"+(head+quantities+empty_proofs+empty_phases).hex()
        a={"ethereum_input":batch,"arguments":{},"native_instance_id":"cw721-drop:0x"+"55"*20,"action_id":3}
        call=mod.translate("cw721-drop","execute::mint_batch_drop",None,{},a,"0x"+"66"*20,mod.TokenIdRemapper())
        self.assertEqual(call["msg"]["mint_drop"]["quantity"],12)

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
                "dataset": "vegeta-s1",
                "summary": {"all_observed_sequences_plus_one": True, "all_token_ids_fit_u64": True},
                "owners": {owner: {
                    "first_token_id": 3847,
                    "transactions": {tx_hash: {"mint_count": 2, "token_ids": [3847, 3848]}},
                }},
            }) + "\n")
            sequence = mod.Cw721DropMintSequence(report)
            iid = "cw721-drop:" + owner
            self.assertEqual(sequence.first_token_id(iid), 3847)
            msg = mod.instantiate_msg("cw721-drop", set(), iid, sequence)
            self.assertEqual(msg["next_token_id"], 3847)
            ok = mod.validate_drop_mint_translation(sequence, {(owner, tx_hash): 2})
            self.assertTrue(ok["validated"])
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
        self.assertIn("collect-vegeta-cw721-drop-mints.py", audit)

    def test_s1_semantic_only_wrapper_and_locked_new_contracts(self):
        wrapper = (ROOT / "tools/legacy-scripts/run-vegeta-s1-semantic-coverage.sh").read_text()
        prepare = (ROOT / "tools/legacy-scripts/run-vegeta-s1-prepare-native.sh").read_text()
        lock = (ROOT / "benchmarks/Cargo.lock").read_text()
        self.assertIn("VEGETA_S1_REUSE_CACHED_COVERAGE_INPUTS=1", wrapper)
        self.assertIn("audit-vegeta-semantic-conflict-coverage.py", wrapper)
        self.assertIn("run-vegeta-s1-semantic-coverage.sh", prepare)
        self.assertIn('name = "acg-benchmark-native-s3-cw721-drop"', lock)
        self.assertIn('name = "acg-benchmark-native-s3-stargate-cw20"', lock)



if __name__ == "__main__":
    unittest.main()
