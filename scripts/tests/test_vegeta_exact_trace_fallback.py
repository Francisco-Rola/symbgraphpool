import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
MODULE_PATH = ROOT / "scripts/vegeta/extract-vegeta-ethereum.py"
SPEC = importlib.util.spec_from_file_location("vegeta_extractor_fallback_test", MODULE_PATH)
extractor = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(extractor)


class FakeClient:
    def __init__(self):
        self.calls = []

    def call(self, method, params):
        self.calls.append((method, params))
        return {
            "reads": ["evm/" + "aa" * 20 + "/" + "01" * 32],
            "writes": ["evm/" + "aa" * 20 + "/" + "02" * 32],
            "steps": 12,
            "gasUsed": 34,
            "error": "",
        }


class ExactTraceFallbackTests(unittest.TestCase):
    def _fallback_corpus(self, root: Path, fallback_hash: str) -> Path:
        root.mkdir(parents=True, exist_ok=True)
        corpus = root / "corpus.jsonl"
        manifest = root / "manifest.json"
        corpus.write_text(json.dumps({
            "block_number": 100,
            "transactions": [{
                "tx_index": 1,
                "tx_hash": fallback_hash,
                "gas_used": 77,
                "opcode_steps": 77,
                "failed": False,
                "reads": ["evm/" + "bb" * 20 + "/" + "03" * 32],
                "writes": ["evm/" + "bb" * 20 + "/" + "04" * 32],
            }],
        }) + "\n")
        manifest.write_text(json.dumps({
            "trace_mode": "public-rpc",
            "access_semantics": "evm-storage-prestate-touched+state-changing-writes-v1",
            "compute_proxy": "gas_used",
        }) + "\n")
        return corpus

    def test_explicit_fallback_skips_rpc_and_preserves_other_exact_trace(self):
        exact_hash = "0x" + "11" * 32
        fallback_hash = "0x" + "22" * 32
        block = {
            "number": "0x64",
            "transactions": [{"hash": exact_hash}, {"hash": fallback_hash}],
        }
        with tempfile.TemporaryDirectory() as td:
            root = Path(td)
            corpus = self._fallback_corpus(root / "fallback", fallback_hash)
            fallback, provenance = extractor.load_fallback_transactions(
                corpus, {fallback_hash}
            )
            client = FakeClient()
            traces = extractor.trace_block_custom_js_transactions(
                client,
                block,
                "tracer-v1",
                60,
                cache_dir=root / "cache",
                resume=True,
                fallback_traces=fallback,
            )
            self.assertEqual(len(client.calls), 1)
            self.assertEqual(client.calls[0][1][0], exact_hash)
            self.assertEqual(traces[1]["result"]["gasUsed"], 77)
            self.assertEqual(
                provenance[fallback_hash]["access_semantics"],
                "evm-storage-prestate-touched+state-changing-writes-v1",
            )

            second = FakeClient()
            traces2 = extractor.trace_block_custom_js_transactions(
                second,
                block,
                "tracer-v1",
                60,
                cache_dir=root / "cache",
                resume=True,
                fallback_traces=fallback,
            )
            self.assertEqual(second.calls, [])
            self.assertEqual(traces2, traces)

    def test_missing_explicit_fallback_hash_is_fail_closed(self):
        fallback_hash = "0x" + "22" * 32
        missing_hash = "0x" + "33" * 32
        with tempfile.TemporaryDirectory() as td:
            corpus = self._fallback_corpus(Path(td) / "fallback", fallback_hash)
            with self.assertRaisesRegex(RuntimeError, "not found in corpus"):
                extractor.load_fallback_transactions(corpus, {missing_hash})


if __name__ == "__main__":
    unittest.main()
