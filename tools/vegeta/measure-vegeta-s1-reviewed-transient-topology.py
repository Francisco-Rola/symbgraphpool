#!/usr/bin/env python3
"""Cheap S1 topology diagnostic for reviewed transient/no-net-change source effects.

This does NOT claim exact EVM SLOAD/SSTORE ground truth. It augments the public-RPC
prestate conflict graph with a narrowly reviewed semantic model derived from the
already-prepared execution messages for three dominant classes whose writes can be
invisible to prestateTracer diffMode when their values are restored within a tx:

  * Universal Router execution lock
  * Blur execution guard
  * WETH balance updates

The result is a diagnostic bound: it asks how much of the native-only topology is
explained by reviewed source semantics without issuing any new RPC calls.
"""
from __future__ import annotations

import argparse
import json
from collections import defaultdict
from pathlib import Path


def iter_jsonl(path: Path):
    with path.open(encoding="utf-8") as fh:
        for line in fh:
            line = line.strip()
            if line:
                yield json.loads(line)


def pairs_from_rows(rows):
    readers = defaultdict(set)
    writers = defaultdict(set)
    for i, (reads, writes) in enumerate(rows):
        for key in reads:
            readers[key].add(i)
        for key in writes:
            writers[key].add(i)
    pairs = set()
    for key, ws in writers.items():
        touched = readers[key] | ws
        for i in ws:
            for j in touched:
                if i != j:
                    pairs.add((min(i, j), max(i, j)))
    return pairs


def source_rows(block):
    return [
        (set(tx.get("reads") or []), set(tx.get("writes") or []))
        for tx in block.get("transactions") or []
    ]


def native_identity(access):
    kind = str(access.get("kind") or "")
    key = str(access.get("key_hex") or "")
    contract = str(access.get("contract") or "")
    if kind.startswith("storage_"):
        if kind == "storage_scan":
            return f"storage:{contract}:{key}:{access.get('range_end_hex') or ''}"
        return f"storage:{contract}:{key}"
    return None


def native_rows(block):
    rows = []
    for tx in block.get("transactions") or []:
        reads, writes = set(), set()
        tx_reverted = bool(tx.get("source_failed")) or tx.get("execution_status") == "reverted"
        for access in tx.get("accesses") or []:
            kind = str(access.get("kind") or "")
            if not kind.startswith("storage_"):
                continue
            ident = native_identity(access)
            if not ident:
                continue
            is_write = (
                kind in {"storage_write", "storage_remove"}
                and not tx_reverted
                and not access.get("reverted")
            )
            (writes if is_write else reads).add(ident)
        rows.append((reads, writes))
    return rows


def _msg(call):
    msg = call.get("msg")
    return msg if isinstance(msg, dict) else {}


def _add(reads, writes, key, writer=True):
    (writes if writer else reads).add(key)


def reviewed_semantic_rows(block):
    """Return aggregate and per-class logical accesses from prepared source semantics."""
    aggregate = []
    by_class = {
        "universal_router_lock": [],
        "blur_execution_guard": [],
        "wrapped_native_balances": [],
    }
    for tx in block.get("transactions") or []:
        agg_r, agg_w = set(), set()
        class_rw = {name: (set(), set()) for name in by_class}
        tx_failed = bool(tx.get("source_failed"))
        for call in tx.get("calls") or []:
            if call.get("kind") not in {"execute", "query"}:
                continue
            family = str(call.get("family") or "")
            iid = str(call.get("instance_id") or family)
            msg = _msg(call)
            reverted = tx_failed or call.get("source_revert_scope_action_id") is not None

            # Reviewed route/Blur guards. These are deliberately logical keys, not EVM slots.
            if family in {"marketplace-router", "universal-router"}:
                if "execute_route" in msg or "v3_swap_callback" in msg:
                    key = f"reviewed:{iid}:router_lock"
                    r, w = class_rw["universal_router_lock"]
                    _add(r, w, key, writer=not reverted)
                    agg_r.add(key) if reverted else agg_w.add(key)
                if "blur_settle" in msg:
                    key = f"reviewed:{iid}:blur_execution_guard"
                    r, w = class_rw["blur_execution_guard"]
                    _add(r, w, key, writer=not reverted)
                    agg_r.add(key) if reverted else agg_w.add(key)

            if family != "wrapped-native-token":
                continue

            r, w = class_rw["wrapped_native_balances"]
            sender = str(call.get("sender") or "")
            def balance_key(addr):
                return f"reviewed:{iid}:balance:{addr.lower()}" if addr else None
            def touch(addr, writer):
                key = balance_key(addr)
                if not key:
                    return
                effective_writer = writer and not reverted
                _add(r, w, key, writer=effective_writer)
                _add(agg_r, agg_w, key, writer=effective_writer)

            if "deposit" in msg or "withdraw" in msg:
                touch(sender, True)
            elif "transfer" in msg:
                body = msg.get("transfer") or {}
                touch(sender, True)
                touch(str(body.get("recipient") or ""), True)
            elif "transfer_from" in msg:
                body = msg.get("transfer_from") or {}
                touch(str(body.get("owner") or ""), True)
                touch(str(body.get("recipient") or ""), True)
            elif "balance" in msg:
                body = msg.get("balance") or {}
                touch(str(body.get("address") or ""), False)

        aggregate.append((agg_r, agg_w))
        for name in by_class:
            by_class[name].append(class_rw[name])
    return aggregate, by_class


def critical_path(n, pairs):
    preds = defaultdict(list)
    for i, j in pairs:
        preds[j].append(i)
    dp = [1] * n
    for j in range(n):
        if preds[j]:
            dp[j] = 1 + max(dp[i] for i in preds[j])
    return max(dp, default=0)


def ratio(a, b):
    return a / b if b else None


def pct(x):
    return "n/a" if x is None else f"{100*x:.2f}%"


def main(argv=None):
    ap = argparse.ArgumentParser()
    ap.add_argument("--corpus", type=Path, required=True)
    ap.add_argument("--execution-plan", type=Path, required=True)
    ap.add_argument("--native-accesses", type=Path, required=True)
    ap.add_argument("--output-json", type=Path, required=True)
    ap.add_argument("--output-text", type=Path, required=True)
    ns = ap.parse_args(argv)

    src = {int(b["block_number"]): b for b in iter_jsonl(ns.corpus)}
    exe = {int(b["block_number"]): b for b in iter_jsonl(ns.execution_plan)}
    nat = {int(b["block_number"]): b for b in iter_jsonl(ns.native_accesses)}
    if set(src) != set(exe) or set(src) != set(nat):
        raise SystemExit(
            f"block set mismatch source={len(src)} execution={len(exe)} native={len(nat)}"
        )

    baseline_all = set()
    augmented_all = set()
    native_all = set()
    semantic_all = set()
    semantic_by_class = defaultdict(set)
    cp_source = cp_augmented = cp_native = 0
    tx_total = 0

    per_block = []
    for bn in sorted(src):
        srows = source_rows(src[bn])
        nrows = native_rows(nat[bn])
        sem_rows, class_rows = reviewed_semantic_rows(exe[bn])
        if not (len(srows) == len(nrows) == len(sem_rows)):
            raise SystemExit(
                f"tx count mismatch block={bn} source={len(srows)} execution={len(sem_rows)} native={len(nrows)}"
            )
        sp = pairs_from_rows(srows)
        np = pairs_from_rows(nrows)
        sem = pairs_from_rows(sem_rows)
        aug = sp | sem
        class_pairs = {name: pairs_from_rows(rows) for name, rows in class_rows.items()}

        baseline_all |= {(bn, i, j) for i, j in sp}
        augmented_all |= {(bn, i, j) for i, j in aug}
        native_all |= {(bn, i, j) for i, j in np}
        semantic_all |= {(bn, i, j) for i, j in sem}
        for name, pairs in class_pairs.items():
            semantic_by_class[name] |= {(bn, i, j) for i, j in pairs}

        cp_source += critical_path(len(srows), sp)
        cp_augmented += critical_path(len(srows), aug)
        cp_native += critical_path(len(nrows), np)
        tx_total += len(srows)
        per_block.append({
            "block_number": bn,
            "transactions": len(srows),
            "baseline_source_pairs": len(sp),
            "reviewed_semantic_pairs": len(sem),
            "augmented_source_pairs": len(aug),
            "native_pairs": len(np),
            "baseline_native_intersection": len(sp & np),
            "augmented_native_intersection": len(aug & np),
            "source_critical_path": critical_path(len(srows), sp),
            "augmented_source_critical_path": critical_path(len(srows), aug),
            "native_critical_path": critical_path(len(nrows), np),
        })

    baseline_fp = native_all - baseline_all
    augmented_fp = native_all - augmented_all
    augmented_fn = augmented_all - native_all
    baseline_inter = len(native_all & baseline_all)
    augmented_inter = len(native_all & augmented_all)
    p0 = ratio(baseline_inter, len(native_all))
    r0 = ratio(baseline_inter, len(baseline_all))
    p1 = ratio(augmented_inter, len(native_all))
    r1 = ratio(augmented_inter, len(augmented_all))

    classes = {}
    explained_union = set()
    for name, pairs in semantic_by_class.items():
        explained = baseline_fp & pairs
        explained_union |= explained
        classes[name] = {
            "semantic_pairs": len(pairs),
            "added_to_public_source": len(pairs - baseline_all),
            "baseline_native_false_positives_explained": len(explained),
        }

    report = {
        "schema_version": 1,
        "methodology": {
            "status": "diagnostic-not-ground-truth",
            "purpose": "bound public-prestateTracer undercount without new RPC tracing",
            "source_baseline": "public-RPC touched-storage reads plus final-state-changing writes",
            "reviewed_augmentation": [
                "Universal Router execute-route/v3-callback execution lock",
                "Blur settle execution guard",
                "wrapped-native-token balance reads/writes decoded from prepared messages",
            ],
            "independence_warning": "augmentation is derived from reviewed translation semantics, not exact EVM SLOAD/SSTORE traces; use it to decide whether an apparent FP is plausibly explained, not to claim exact source storage ground truth",
            "runtime_workload_changed": False,
        },
        "blocks": len(src),
        "transactions": tx_total,
        "baseline": {
            "source_pairs": len(baseline_all),
            "native_pairs": len(native_all),
            "intersection": baseline_inter,
            "precision": p0,
            "recall": r0,
            "false_positive": len(baseline_fp),
            "critical_path_sum": {"source": cp_source, "native": cp_native},
        },
        "reviewed_augmented": {
            "source_pairs": len(augmented_all),
            "native_pairs": len(native_all),
            "intersection": augmented_inter,
            "precision": p1,
            "recall": r1,
            "native_only_remaining": len(augmented_fp),
            "augmented_source_only": len(augmented_fn),
            "baseline_false_positives_explained": len(explained_union),
            "baseline_false_positive_explained_fraction": ratio(len(explained_union), len(baseline_fp)),
            "critical_path_sum": {"source_public": cp_source, "source_augmented": cp_augmented, "native": cp_native},
            "critical_path_native_to_augmented_ratio": ratio(cp_native, cp_augmented),
        },
        "classes": classes,
        "per_block": per_block,
    }

    ns.output_json.parent.mkdir(parents=True, exist_ok=True)
    ns.output_json.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")
    lines = [
        "Vegeta S1 reviewed transient-topology augmentation (diagnostic, no RPC)",
        "",
        f"blocks: {len(src)}",
        f"transactions: {tx_total}",
        "",
        "PUBLIC-RPC BASELINE",
        f"source pairs: {len(baseline_all)}",
        f"native pairs: {len(native_all)}",
        f"precision: {pct(p0)}",
        f"recall: {pct(r0)}",
        f"native-only pairs: {len(baseline_fp)}",
        f"critical path sum: source={cp_source} native={cp_native}",
        "",
        "REVIEWED TRANSIENT AUGMENTATION",
        f"augmented source pairs: {len(augmented_all)}",
        f"intersection with native: {augmented_inter}",
        f"precision: {pct(p1)}",
        f"recall: {pct(r1)}",
        f"baseline native-only pairs explained: {len(explained_union)}/{len(baseline_fp)} ({pct(ratio(len(explained_union),len(baseline_fp)))})",
        f"native-only pairs remaining: {len(augmented_fp)}",
        f"augmented-source-only pairs: {len(augmented_fn)}",
        f"critical path sum: public_source={cp_source} augmented_source={cp_augmented} native={cp_native}",
        f"native/augmented critical-path ratio: {ratio(cp_native,cp_augmented):.4f}" if cp_augmented else "native/augmented critical-path ratio: n/a",
        "",
        "EXPLANATION BY REVIEWED CLASS",
    ]
    for name, row in sorted(classes.items(), key=lambda kv: -kv[1]["baseline_native_false_positives_explained"]):
        lines.append(
            f"{name}: semantic_pairs={row['semantic_pairs']} added_source_pairs={row['added_to_public_source']} "
            f"explains_baseline_fp={row['baseline_native_false_positives_explained']}"
        )
    lines += [
        "",
        "Interpretation:",
        "This is a cheap diagnostic bound, not exact SLOAD/SSTORE ground truth. It does not modify",
        "the workload or scheduler. If it explains most native-only edges and aligns the critical",
        "path, the low public-RPC precision is primarily a write-oracle artifact rather than a reason",
        "to delete real native state dependencies.",
    ]
    ns.output_text.write_text("\n".join(lines) + "\n")
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
