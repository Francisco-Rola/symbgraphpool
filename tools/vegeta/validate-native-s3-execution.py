#!/usr/bin/env python3
"""Validate prepared/native Vegeta execution artifacts for S3 or larger datasets."""
from __future__ import annotations

import argparse
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


def count_plan(path: Path) -> tuple[int, int]:
    blocks = txs = 0
    with path.open(encoding="utf-8") as f:
        for line in f:
            if not line.strip():
                continue
            row = json.loads(line)
            blocks += 1
            txs += len(row.get("transactions") or [])
    return blocks, txs


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument('--output-dir', type=Path, default=ROOT/'benchmarks/corpora/vegeta-ethereum/s3/native-execution')
    ap.add_argument('--plan', type=Path, default=None, help='execution-plan.jsonl; defaults to <output-dir>/execution-plan.jsonl')
    ap.add_argument('--skip-fidelity', action='store_true', help='do not require post-execution source/native topology fidelity reports')
    ap.add_argument('--prepared-only', action='store_true', help='validate only prepared manifest/plan; do not require native-access execution audit or fidelity reports')
    ns = ap.parse_args()
    access = ns.output_dir/'native-accesses.jsonl'
    fidelity = ns.output_dir/'native-topology-fidelity.json'
    attribution = ns.output_dir/'native-topology-attribution.json'
    manifest = ns.output_dir/'execution-manifest.json'
    plan = ns.plan or (ns.output_dir/'execution-plan.jsonl')
    errors = []

    if not manifest.exists():
        errors.append(f'missing {manifest}')
    else:
        m = json.loads(manifest.read_text())
        cp = (m.get('normalization') or {}).get('caller_provenance') or {}
        if cp.get('mode') != 'exact': errors.append(f"expected exact EVM msg.sender provenance, got {cp.get('mode')!r}")
        if cp.get('source') != 'derived-geth-callTracer-effective-msg.sender': errors.append(f"unexpected caller provenance source {cp.get('source')!r}")
        if int(cp.get('missing_actions') or 0) != 0: errors.append(f"native execution plan contains {cp.get('missing_actions')} actions without explicit ethereum_msg_sender")

    if not plan.exists():
        errors.append(f'missing {plan}')
        expected_blocks = expected_txs = None
    else:
        expected_blocks, expected_txs = count_plan(plan)

    if not ns.prepared_only:
        if not access.exists():
            errors.append(f'missing {access}')
        else:
            blocks = txs = 0
            bad = []
            with access.open(encoding='utf-8') as f:
                for line in f:
                    if not line.strip(): continue
                    b = json.loads(line); blocks += 1
                    rows = b.get('transactions') or []; txs += len(rows)
                    for t in rows:
                        if t.get('execution_status') not in {'committed','reverted'} and len(bad) < 5:
                            bad.append((b.get('block_number'), t.get('tx_index'), t.get('execution_status')))
            if expected_blocks is not None and blocks != expected_blocks: errors.append(f'expected {expected_blocks} executed blocks, got {blocks}')
            if expected_txs is not None and txs != expected_txs: errors.append(f'expected {expected_txs} executed transactions, got {txs}')
            if bad: errors.append(f'unexpected execution statuses: {bad}')

    if not ns.skip_fidelity and not ns.prepared_only:
        if not fidelity.exists(): errors.append(f'missing {fidelity}')
        else:
            d = json.loads(fidelity.read_text())
            if d.get('comparison_scope') != 'contract-storage-only': errors.append(f"unexpected fidelity comparison scope {d.get('comparison_scope')!r}")
            for path_keys in [('conflict_pairs','precision'),('conflict_pairs','recall'),('critical_chain_fidelity','ratio'),('vegeta_hot_key_chain_fidelity','ratio'),('full_native_augmentation','full_native_pairs'),('full_native_augmentation','additional_pairs_from_bank_ledger')]:
                x = d
                for k in path_keys: x = x.get(k) if isinstance(x,dict) else None
                if x is None: errors.append('missing metric '+'.'.join(path_keys))
        if not attribution.exists(): errors.append(f'missing {attribution}')
        else:
            a = json.loads(attribution.read_text()); s = a.get('summary') or {}
            if fidelity.exists():
                d = json.loads(fidelity.read_text()); cp = d.get('conflict_pairs') or {}
                if s.get('false_positive_pairs') != cp.get('false_positive'): errors.append('attribution false-positive count does not match fidelity report')
                if s.get('false_negative_pairs') != cp.get('false_negative'): errors.append('attribution false-negative count does not match fidelity report')
            for path_keys in [('critical_path_attribution','true_positive_only_native_sum'),('false_positive','by_family'),('false_positive','top_concrete_keys'),('false_negative','by_source_owner')]:
                x = a
                for k in path_keys: x = x.get(k) if isinstance(x,dict) else None
                if x is None: errors.append('missing attribution '+'.'.join(path_keys))

    print('Vegeta native execution validation')
    print()
    print('accepted:', 'yes' if not errors else 'no')
    if expected_blocks is not None: print(f'expected plan: blocks={expected_blocks} transactions={expected_txs}')
    print('native access audit:', 'skipped (prepared-only)' if ns.prepared_only else 'required')
    print('fidelity reports:', 'skipped' if (ns.skip_fidelity or ns.prepared_only) else 'required')
    print('errors:', len(errors))
    for e in errors: print('ERROR:', e)
    return 1 if errors else 0


if __name__ == '__main__':
    raise SystemExit(main())
