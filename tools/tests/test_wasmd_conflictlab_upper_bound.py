import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


class ConflictLabWasmdUpperBoundTests(unittest.TestCase):
    def test_generator_builds_unique_credit_keys(self):
        with tempfile.TemporaryDirectory() as td:
            out = Path(td) / "inputs"
            subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "evaluation/workloads/generate_conflictlab.py"),
                    "--output-dir",
                    str(out),
                    "--blocks",
                    "2",
                    "--transactions",
                    "8",
                    "--lanes",
                    "8",
                    "--work-iterations",
                    "64",
                    "--payload-bytes",
                    "8",
                ],
                cwd=ROOT,
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            )
            manifest = json.loads((out / "execution-manifest.json").read_text())
            self.assertIn("lanes8-tx8", manifest["dataset"])
            self.assertEqual(manifest["controlled_parallelism"]["lanes"], 8)
            self.assertEqual(manifest["transactions"], 16)
            self.assertTrue((out / "symbolic/conflictlab.symbolic.json").is_file())

            blocks = [json.loads(line) for line in (out / "execution-plan.jsonl").read_text().splitlines() if line]
            self.assertEqual(len(blocks), 2)
            for block in blocks:
                accounts = []
                for tx in block["transactions"]:
                    self.assertEqual(len(tx["calls"]), 1)
                    call = tx["calls"][0]
                    self.assertEqual(call["family"], "conflictlab")
                    self.assertEqual(call["kind"], "execute")
                    credit = call["msg"]["credit"]
                    accounts.append(credit["account"])
                    self.assertEqual(credit["work_iterations"], 64)
                    self.assertEqual(credit["storage_rounds"], 0)
                self.assertEqual(len(accounts), len(set(accounts)))

    def test_upper_bound_summary_gate_accepts_zero_conflict_synthetic_rows(self):
        # Minimal records sufficient to exercise the summary/gate logic without
        # binding the test to the full Go evaluator schema.
        rows = []
        strategies = [
            "cosmos-wasmd-direct-serial",
            "cosmos-wasmd-block-stm",
            "cosmos-wasmd-aria-fb",
            "cosmos-wasmd-vegeta",
            "cosmos-wasmd-symbgraph-rust",
        ]
        for workers in (1, 2):
            for strategy in strategies:
                row = {
                    "strategy": strategy,
                    "workers": workers,
                    "sample": 0,
                    "block_number": 1,
                    "transactions": 8,
                    "post_consensus_nanos": 8_000_000 // workers,
                    "pre_consensus_nanos": 0,
                    "execution_attempts": 8,
                    "reexecutions": 0,
                    "serial_equivalent": True,
                }
                if strategy == "cosmos-wasmd-aria-fb":
                    row.update({"aria_initial_exec_work_nanos": 8_000_000, "aria_initial_batch_nanos": 8_000_000 // workers})
                elif strategy == "cosmos-wasmd-vegeta":
                    row.update({
                        "vegeta_post_exec_work_nanos": 8_000_000,
                        "vegeta_post_exec_span_nanos": 8_000_000 // workers,
                        "vegeta_total_estimated_cost": 8,
                        "vegeta_ready_worker_lower_bound_cost": 8 // workers if workers == 1 else 4,
                    })
                elif strategy == "cosmos-wasmd-symbgraph-rust":
                    row.update({
                        "symb_preexecution_nanos": 8_000_000 // workers,
                        "symb_worker_utilization": 1.0,
                        "symb_max_active": workers,
                    })
                rows.append(row)

        with tempfile.TemporaryDirectory() as td:
            td = Path(td)
            records = td / "records.jsonl"
            records.write_text("".join(json.dumps(row) + "\n" for row in rows))
            out = td / "report.txt"
            subprocess.run(
                [
                    sys.executable,
                    str(ROOT / "evaluation/wasmd/summarize_conflictlab_upper_bound.py"),
                    "--records",
                    str(records),
                    "--output",
                    str(out),
                    "--host-kind",
                    "test",
                ],
                cwd=ROOT,
                check=True,
                stdout=subprocess.PIPE,
                text=True,
            )
            text = out.read_text()
            self.assertIn("PASS: no concrete dependency/fallback signal", text)
            self.assertIn("ACG-pre-scale", text)


if __name__ == "__main__":
    unittest.main()
