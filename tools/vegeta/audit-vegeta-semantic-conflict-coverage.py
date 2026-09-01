#!/usr/bin/env python3
"""Measure S1 conflict coverage using reviewed state semantics, with denominator-aligned diagnostics.

The public-RPC S1 source corpus is a *touched-state* instrument rather than a committed-write log.
Accordingly this audit reports three distinct notions instead of conflating them:

1. owner structural coverage: the source conflict pair touches a reviewed/mapped storage owner;
2. selector-reviewed state-touch coverage: both sides have reviewed state-dependent semantics for a
   source-conflicting owner, including reviewed paths that later revert (publication-aligned metric);
3. successful/committed-path coverage: the same test but excluding reverted reviewed frames.

Reviewed STATE_READ, STATE_WRITE, and READ_WRITE entrypoints are state-dependent. PURE entrypoints do
not count toward storage-conflict coverage. Opaque selectors remain excluded and are ranked by exact
pair-unlock potential. Inlined DELEGATECALL implementation frames may provide reviewed semantic
*evidence* for the proxy storage namespace, but they are never emitted as duplicate native calls.
"""
from __future__ import annotations

import argparse
import json
import math
from collections import Counter, defaultdict
from pathlib import Path

from native_s3_planner_compat import (
    SEMANTIC_OPAQUE,
    SEMANTIC_PURE,
    SEMANTIC_READ_WRITE,
    SEMANTIC_STATE_READ,
    SEMANTIC_STATE_WRITE,
    block_conflicts_by_owner,
    semantic_effect_for_entrypoint,
)
from vegeta_corpus import iter_blocks

Candidate = tuple[str, str, str]  # owner, family, selector
PairId = tuple[int, int, int]     # block, left, right
STATE_EFFECTS = {SEMANTIC_STATE_READ, SEMANTIC_STATE_WRITE, SEMANTIC_READ_WRITE}
WRITE_EFFECTS = {SEMANTIC_STATE_WRITE, SEMANTIC_READ_WRITE}


def percentile(values: list[float], q: float) -> float | None:
    if not values:
        return None
    vals = sorted(values)
    pos = (len(vals) - 1) * q
    lo = math.floor(pos)
    hi = math.ceil(pos)
    if lo == hi:
        return vals[lo]
    f = pos - lo
    return vals[lo] * (1 - f) + vals[hi] * f


def action_semantic_effect(action: dict) -> str:
    effect = str(action.get("semantic_effect") or "")
    if effect in STATE_EFFECTS | {SEMANTIC_PURE, SEMANTIC_OPAQUE}:
        return effect
    ep = action.get("semantic_entrypoint") or action.get("native_entrypoint")
    dispatch = str(action.get("dispatch") or "")
    derived = semantic_effect_for_entrypoint(ep, dispatch)
    if derived != SEMANTIC_OPAQUE:
        return derived
    # Backward-compatible fallback for older/generated fixture plans that predate semantic_effect.
    if dispatch in {"mapped-entrypoint", "inlined-reviewed-entrypoint"}:
        return SEMANTIC_READ_WRITE
    if dispatch == "reviewed-stateless-entrypoint":
        return SEMANTIC_PURE
    return SEMANTIC_OPAQUE


def effects_can_conflict(left: set[str], right: set[str]) -> bool:
    """Both sides must touch state and at least one reviewed side must be write-capable."""
    if not (left & STATE_EFFECTS) or not (right & STATE_EFFECTS):
        return False
    return bool((left & WRITE_EFFECTS) or (right & WRITE_EFFECTS))


def opaque_class(action: dict) -> str:
    if action.get("failed_frame"):
        return "reverted"
    if str(action.get("call_type") or "").upper() == "STATICCALL":
        return "successful-read-only"
    return "successful-state-capable"


def tx_semantics(plan_block: dict):
    """Return per-tx committed/touch owner effects plus opaque candidate evidence."""
    committed: list[dict[str, set[str]]] = []
    touched: list[dict[str, set[str]]] = []
    opaque: list[dict[str, set[Candidate]]] = []
    mapped_owners: list[set[str]] = []
    owner_family: dict[str, str] = {}
    reasons = Counter()
    opaque_frames = Counter()
    opaque_classes = Counter()

    for tx in plan_block.get("transactions") or []:
        committed_by_owner: dict[str, set[str]] = defaultdict(set)
        touch_by_owner: dict[str, set[str]] = defaultdict(set)
        opaque_by_owner: dict[str, set[Candidate]] = defaultdict(set)
        tx_mapped_owners: set[str] = set()
        for action in tx.get("native_actions") or []:
            owner = str(action.get("storage_context_address") or "").lower()
            family = str(action.get("native_code_family") or "")
            status = str(action.get("translation_status") or "")
            dispatch = str(action.get("dispatch") or "")
            if not owner.startswith("0x") or len(owner) != 42:
                continue
            if family:
                owner_family.setdefault(owner, family)
            if status in {"mapped-native-call", "inlined-delegatecall"} and family:
                tx_mapped_owners.add(owner)

            if dispatch == "mapped-opaque-selector":
                reasons["mapped_owner_opaque_selector"] += 1
                selector = str(action.get("selector") or "0x").lower()
                cand = (owner, family, selector)
                cls = opaque_class(action)
                opaque_frames[cand] += 1
                opaque_classes[(cand, cls)] += 1
                # Candidate gain is a review *potential*: keep reverted candidates visible rather
                # than pretending a reverted call can never touch state in the source instrument.
                opaque_by_owner[owner].add(cand)
                continue

            effect = action_semantic_effect(action)
            if effect == SEMANTIC_PURE:
                reasons["reviewed_pure_frame"] += 1
                continue
            if effect not in STATE_EFFECTS:
                if status == "inlined-delegatecall":
                    reasons["unreviewed_inlined_delegate_frame"] += 1
                continue

            reasons[f"reviewed_{effect.lower()}_frame"] += 1
            touch_by_owner[owner].add(effect)
            if action.get("failed_frame"):
                reasons["reviewed_reverted_state_frame"] += 1
            else:
                committed_by_owner[owner].add(effect)
                reasons["reviewed_successful_state_frame"] += 1
            if dispatch == "inlined-reviewed-entrypoint":
                reasons["reviewed_inlined_delegate_entrypoint"] += 1

        committed.append(dict(committed_by_owner))
        touched.append(dict(touch_by_owner))
        opaque.append(dict(opaque_by_owner))
        mapped_owners.append(tx_mapped_owners)

    return committed, touched, opaque, mapped_owners, owner_family, reasons, opaque_frames, opaque_classes


def candidate_row(
    cand: Candidate,
    frames: Counter,
    classes: Counter,
    direct: dict[Candidate, set[PairId]],
    synergy: dict[frozenset[Candidate], set[PairId]],
):
    owner, family, selector = cand
    cls = {k[1]: v for k, v in classes.items() if k[0] == cand}
    synergy_pairs: set[PairId] = set()
    for req, pairs in synergy.items():
        if cand in req:
            synergy_pairs.update(pairs)
    return {
        "storage_owner": owner,
        "native_code_family": family,
        "selector": selector,
        "frames": frames[cand],
        "frame_classification": cls,
        "exact_single_selector_gain": len(direct.get(cand, set())),
        "two_selector_unlock_potential": len(synergy_pairs),
        "review_priority_eligible": bool(sum(cls.values())),
    }


def balanced(rows: list[dict], numerator_key: str) -> dict:
    conflict_rows = [x for x in rows if x["total_conflict_pairs"] > 0]
    covs = [x[numerator_key] / x["total_conflict_pairs"] for x in conflict_rows]
    return {
        "conflict_bearing_blocks": len(conflict_rows),
        "median_coverage": percentile(covs, .5),
        "p10_coverage": percentile(covs, .1),
        "p25_coverage": percentile(covs, .25),
        "minimum_coverage": min(covs) if covs else None,
        "blocks_meeting_threshold": {
            f"{int(t*100)}pct": sum(v + 1e-12 >= t for v in covs) for t in (.5, .75, .9, .95)
        },
    }


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--corpus", type=Path, required=True)
    ap.add_argument("--native-plan", type=Path, required=True)
    ap.add_argument("--source-coverage", type=Path, default=None)
    ap.add_argument("--output", type=Path, required=True)
    ap.add_argument("--text-output", type=Path, required=True)
    ns = ap.parse_args()

    total = structural = touched_covered = committed_covered = 0
    per_block: list[dict] = []
    by_family_touch = defaultdict(int)
    by_family_committed = defaultdict(int)
    reasons = Counter()
    opaque_frames = Counter()
    opaque_classes = Counter()
    direct_gain: dict[Candidate, set[PairId]] = defaultdict(set)
    synergy_gain: dict[frozenset[Candidate], set[PairId]] = defaultdict(set)
    blocks = 0

    plan_handle = ns.native_plan.open(encoding="utf-8")
    for source_block in iter_blocks(ns.corpus):
        line = next((x for x in plan_handle if x.strip()), None)
        if line is None:
            raise SystemExit(f"native plan ended before source block {source_block['block_number']}")
        plan_block = json.loads(line)
        bn = int(source_block["block_number"])
        if int(plan_block.get("block_number", -1)) != bn:
            raise SystemExit(f"block mismatch source={bn} plan={plan_block.get('block_number')}")
        source_txs = source_block.get("transactions") or []
        plan_txs = plan_block.get("transactions") or []
        if len(source_txs) != len(plan_txs):
            raise SystemExit(f"block {bn}: source tx={len(source_txs)} plan tx={len(plan_txs)}")

        committed, touched, opaque, mapped_owners, owner_family, local_reasons, local_frames, local_classes = tx_semantics(plan_block)
        reasons.update(local_reasons)
        opaque_frames.update(local_frames)
        opaque_classes.update(local_classes)
        all_pairs, owner_pairs = block_conflicts_by_owner(source_block)
        structural_pairs: set[tuple[int, int]] = set()
        touch_pairs: set[tuple[int, int]] = set()
        committed_pairs: set[tuple[int, int]] = set()
        family_touch = defaultdict(set)
        family_committed = defaultdict(set)

        for owner, pairs in owner_pairs.items():
            owner = owner.lower()
            fam = owner_family.get(owner)
            for left, right in pairs:
                if left >= len(touched) or right >= len(touched):
                    continue
                if owner in mapped_owners[left] and owner in mapped_owners[right]:
                    structural_pairs.add((left, right))
                if effects_can_conflict(set(touched[left].get(owner, set())), set(touched[right].get(owner, set()))):
                    touch_pairs.add((left, right))
                    if fam:
                        family_touch[fam].add((left, right))
                if effects_can_conflict(set(committed[left].get(owner, set())), set(committed[right].get(owner, set()))):
                    committed_pairs.add((left, right))
                    if fam:
                        family_committed[fam].add((left, right))

        # Exact opaque-selector pair unlock potential is measured relative to the denominator-aligned
        # state-touch coverage. A candidate is not counted as covered until its semantics are reviewed.
        for left, right in all_pairs - touch_pairs:
            pid = (bn, left, right)
            for owner, pairs in owner_pairs.items():
                owner = owner.lower()
                if (left, right) not in pairs:
                    continue
                le = set(touched[left].get(owner, set()))
                re = set(touched[right].get(owner, set()))
                lcands = set(opaque[left].get(owner, set()))
                rcands = set(opaque[right].get(owner, set()))
                if re & STATE_EFFECTS:
                    for cand in lcands:
                        direct_gain[cand].add(pid)
                if le & STATE_EFFECTS:
                    for cand in rcands:
                        direct_gain[cand].add(pid)
                for lc in lcands:
                    for rc in rcands:
                        if lc == rc:
                            direct_gain[lc].add(pid)
                        else:
                            synergy_gain[frozenset((lc, rc))].add(pid)

        n = len(all_pairs)
        total += n
        structural += len(structural_pairs)
        touched_covered += len(touch_pairs)
        committed_covered += len(committed_pairs)
        blocks += 1
        for fam, pairs in family_touch.items():
            by_family_touch[fam] += len(pairs)
        for fam, pairs in family_committed.items():
            by_family_committed[fam] += len(pairs)
        per_block.append({
            "block_number": bn,
            "total_conflict_pairs": n,
            "owner_structural_pairs": len(structural_pairs),
            "selector_reviewed_state_touch_pairs": len(touch_pairs),
            "successful_committed_path_pairs": len(committed_pairs),
            "coverage": len(touch_pairs) / n if n else 1.0,
            "committed_coverage": len(committed_pairs) / n if n else 1.0,
        })
        if blocks % 500 == 0:
            print(f"semantic conflict audit blocks={blocks} touch={touched_covered}/{total} committed={committed_covered}/{total}", flush=True)

    if next((x for x in plan_handle if x.strip()), None) is not None:
        raise SystemExit("native plan contains extra blocks after source corpus")

    rows = [candidate_row(c, opaque_frames, opaque_classes, direct_gain, synergy_gain) for c in set(opaque_frames)]
    rows.sort(key=lambda r: (-r["exact_single_selector_gain"], -r["two_selector_unlock_potential"], -r["frames"], r["storage_owner"], r["selector"]))
    top_synergy = []
    for req, pairs in sorted(synergy_gain.items(), key=lambda kv: (-len(kv[1]), sorted(kv[0])))[:100]:
        cs = sorted(req)
        top_synergy.append({
            "candidates": [{"storage_owner": c[0], "native_code_family": c[1], "selector": c[2]} for c in cs],
            "exact_joint_gain": len(pairs),
        })

    source_structural = None
    if ns.source_coverage and ns.source_coverage.exists():
        doc = json.loads(ns.source_coverage.read_text(encoding="utf-8"))
        src = doc.get("source_conflict_coverage") or {}
        source_structural = {
            "unique_conflict_pairs": int(src.get("selected_family_unique_conflict_pairs", 0)),
            "total_unique_conflict_pairs": int(src.get("total_unique_conflict_pairs", total)),
            "coverage": float(src.get("coverage", 0.0)),
            "source": str(ns.source_coverage),
        }

    touch_balanced = balanced(per_block, "selector_reviewed_state_touch_pairs")
    committed_balanced = balanced(per_block, "successful_committed_path_pairs")
    report = {
        "schema_version": 4,
        "dataset": "vegeta-s1",
        "definition": {
            "source_denominator": "public-rpc/prestate touched-state conflict pairs",
            "publication_metric": "selector-reviewed state-touch coverage; STATE_READ/STATE_WRITE/READ_WRITE count, PURE and OPAQUE do not; reviewed reverted state paths remain state-touch evidence but are excluded from committed-path coverage",
            "committed_metric": "successful reviewed state-dependent paths only",
        },
        "total_unique_conflict_pairs": total,
        "owner_structural_unique_conflict_pairs": structural,
        "owner_structural_coverage_from_plan": structural / total if total else 1.0,
        "source_owner_structural_coverage": source_structural,
        "semantic_unique_conflict_pairs": touched_covered,
        "selector_reviewed_state_touch_unique_conflict_pairs": touched_covered,
        "coverage": touched_covered / total if total else 1.0,
        "successful_committed_path_unique_conflict_pairs": committed_covered,
        "committed_coverage": committed_covered / total if total else 1.0,
        "block_balanced": touch_balanced,
        "committed_block_balanced": committed_balanced,
        "by_native_family": [
            {
                "native_code_family": fam,
                "state_touch_pair_attributions": by_family_touch[fam],
                "committed_pair_attributions": by_family_committed.get(fam, 0),
            }
            for fam in sorted(by_family_touch, key=lambda f: (-by_family_touch[f], f))
        ],
        "selector_status_counts": dict(reasons),
        "opaque_selector_conflict_gain": rows[:200],
        "top_two_selector_synergies": top_synergy,
        "per_block": per_block,
    }
    ns.output.parent.mkdir(parents=True, exist_ok=True)
    ns.output.write_text(json.dumps(report, indent=2, sort_keys=True) + "\n")

    structural_display = source_structural or {
        "unique_conflict_pairs": structural,
        "total_unique_conflict_pairs": total,
        "coverage": structural / total if total else 1.0,
    }
    lines = [
        "Vegeta S1 reviewed state-semantics conflict coverage",
        "",
        f"owner structural conflict coverage: {structural_display['unique_conflict_pairs']}/{structural_display['total_unique_conflict_pairs']} ({100*structural_display['coverage']:.2f}%)",
        f"selector-reviewed state-touch coverage: {touched_covered}/{total} ({100*report['coverage']:.2f}%)",
        f"successful committed-path coverage: {committed_covered}/{total} ({100*report['committed_coverage']:.2f}%)",
        f"median state-touch conflict-bearing block coverage: {100*(touch_balanced['median_coverage'] or 0):.2f}%",
        f"p10 state-touch coverage: {100*(touch_balanced['p10_coverage'] or 0):.2f}%",
        f"median committed-path block coverage: {100*(committed_balanced['median_coverage'] or 0):.2f}%",
        f"mapped-owner opaque selector frames: {reasons['mapped_owner_opaque_selector']}",
        f"reviewed reverted state frames: {reasons['reviewed_reverted_state_frame']} (included only in state-touch metric)",
        f"reviewed pure frames: {reasons['reviewed_pure_frame']} (excluded from storage coverage)",
        f"reviewed inlined delegate entrypoints: {reasons['reviewed_inlined_delegate_entrypoint']}",
        "",
    ]
    if rows:
        lines.append("Top opaque selectors by exact state-touch conflict deficit gain:")
        for r in rows[:30]:
            cls = r["frame_classification"]
            lines.append(
                f"  {r['storage_owner']} {r['selector']} family={r['native_code_family']} frames={r['frames']} "
                f"direct_gain={r['exact_single_selector_gain']} two_selector_potential={r['two_selector_unlock_potential']} "
                f"state={cls.get('successful-state-capable',0)} read={cls.get('successful-read-only',0)} reverted={cls.get('reverted',0)}"
            )
    if top_synergy:
        lines += ["", "Top two-selector exact joint deficits:"]
        for row in top_synergy[:10]:
            names = " + ".join(f"{x['storage_owner']}:{x['selector']}" for x in row["candidates"])
            lines.append(f"  {names} gain={row['exact_joint_gain']}")
    lines += [
        "",
        "Publication meaning: this touched-state metric is aligned with the public-RPC source denominator.",
        "STATE_READ/STATE_WRITE/READ_WRITE count; PURE and OPAQUE do not. Reverted reviewed paths are",
        "reported separately and count only for touched-state coverage, never for committed-path coverage.",
    ]
    ns.text_output.write_text("\n".join(lines) + "\n")
    print("\n".join(lines))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
