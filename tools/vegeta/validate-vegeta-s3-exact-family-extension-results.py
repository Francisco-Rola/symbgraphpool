#!/usr/bin/env python3
"""Validate the intended effects of the Vegeta S3 exact-ground-truth family extension.

This validator intentionally does not invent new topology thresholds.  It enforces only the
previously frozen workload-fidelity gates plus mechanical invariants of this patch: finalized
semantic-volume measurements, the four reviewed owner mappings, and removal of the wrapped-native
contract-local ``denom`` key.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DEFAULT_FOLLOWUP = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/native-execution/exact-followup/exact-fidelity-followup.json"
DEFAULT_CATALOG = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/native-plan/native-instance-catalog.json"
DEFAULT_OUT = ROOT / "benchmarks/corpora/vegeta-ethereum/s3/native-execution/exact-family-extension-validation"

REVIEWED_OWNERS = {
    "0x00000000000001ad428e4906ae43d8f9852d0dd6",
    "0xef1c6e67703c7bd7107eed8303fbe6ec2554bf6b",
    "0x000000000000ad05ccc4f10045630fb830b95127",
    "0x00000000006c3852cbef3e08e8df289169ede581",
}


def read_json(path: Path) -> dict:
    return json.loads(path.read_text(encoding="utf-8"))


def validate(followup: dict, catalog: dict) -> dict:
    errors: list[str] = []
    gates = followup.get("frozen_gate_recheck") or {}
    if gates.get("semantic_measurement_source") != "final-mapping-simulation":
        errors.append("semantic-volume gates are not sourced from final-mapping-simulation")
    if gates.get("accepted") is not True:
        errors.append("one or more previously frozen workload-fidelity gates still fail")

    owner_to_families: dict[str, set[str]] = {}
    for row in catalog.get("instances") or []:
        owner = str(row.get("source_storage_owner") or "").lower()
        if owner:
            owner_to_families.setdefault(owner, set()).add(str(row.get("native_code_family") or ""))
    for owner in sorted(REVIEWED_OWNERS):
        if "marketplace-router" not in owner_to_families.get(owner, set()):
            errors.append(f"reviewed exact-FN owner is not mapped to marketplace-router: {owner}")

    # The old artificial wrapped-native config key was literal UTF-8 `denom` => 64656e6f6d.
    denom_suffix = ":64656e6f6d"
    offending = []
    for row in ((followup.get("hot_key_diagnostics") or {}).get("native_ranked_keys") or []):
        families = set(row.get("families") or [])
        if "wrapped-native-token" in families and str(row.get("key") or "").endswith(denom_suffix):
            offending.append(str(row.get("key")))
    if offending:
        errors.append(
            "wrapped-native immutable denom still appears as contract storage: " + ", ".join(offending)
        )

    topology = followup.get("topology_baseline") or {}
    conflicts = topology.get("conflict_pairs") or {}
    critical = topology.get("critical_path") or {}
    hot = topology.get("hot_key_chain") or {}
    return {
        "schema_version": 1,
        "dataset": "vegeta-s3-exact-family-extension-validation",
        "accepted": not errors,
        "errors": errors,
        "frozen_gate_status": gates.get("accepted"),
        "semantic_measurement_source": gates.get("semantic_measurement_source"),
        "reviewed_owner_count": len(REVIEWED_OWNERS),
        "reviewed_owners_mapped_to_marketplace_router": sum(
            "marketplace-router" in owner_to_families.get(owner, set()) for owner in REVIEWED_OWNERS
        ),
        "wrapped_native_denom_storage_key_absent": not offending,
        "topology_report_only_no_new_threshold": {
            "precision": conflicts.get("precision"),
            "recall": conflicts.get("recall"),
            "f1": conflicts.get("f1"),
            "source_conflict_pairs": conflicts.get("source"),
            "native_conflict_pairs": conflicts.get("native"),
            "critical_path_source_sum": critical.get("source_sum"),
            "critical_path_native_sum": critical.get("native_sum"),
            "critical_path_relative_error": critical.get("relative_error"),
            "hot_key_source_sum": hot.get("source_sum"),
            "hot_key_native_sum": hot.get("native_sum"),
            "hot_key_relative_error": hot.get("relative_error"),
        },
        "policy_note": (
            "No post-hoc precision/recall/critical-path threshold is added here. Only the previously "
            "frozen coverage gates and patch-mechanical invariants are enforced."
        ),
    }


def render(report: dict) -> str:
    topo = report["topology_report_only_no_new_threshold"]
    lines = [
        "Vegeta S3 exact family-extension validation",
        "",
        f"accepted: {'yes' if report['accepted'] else 'no'}",
        f"frozen gates: {'PASS' if report['frozen_gate_status'] else 'FAIL'}",
        f"semantic-volume source: {report['semantic_measurement_source']}",
        f"reviewed marketplace owners mapped: {report['reviewed_owners_mapped_to_marketplace_router']} / {report['reviewed_owner_count']}",
        f"wrapped-native contract-storage denom key absent: {'yes' if report['wrapped_native_denom_storage_key_absent'] else 'no'}",
        "",
        "Topology (reported only; no new threshold):",
        f"  precision: {topo['precision']}",
        f"  recall: {topo['recall']}",
        f"  F1: {topo['f1']}",
        f"  conflict pairs: source={topo['source_conflict_pairs']} native={topo['native_conflict_pairs']}",
        f"  critical path: source={topo['critical_path_source_sum']} native={topo['critical_path_native_sum']} relative_error={topo['critical_path_relative_error']}",
        f"  hot-key chain: source={topo['hot_key_source_sum']} native={topo['hot_key_native_sum']} relative_error={topo['hot_key_relative_error']}",
        "",
        report["policy_note"],
    ]
    lines.extend(f"ERROR: {error}" for error in report["errors"])
    return "\n".join(lines) + "\n"


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--followup", type=Path, default=DEFAULT_FOLLOWUP)
    parser.add_argument("--instance-catalog", type=Path, default=DEFAULT_CATALOG)
    parser.add_argument("--output-dir", type=Path, default=DEFAULT_OUT)
    args = parser.parse_args()
    report = validate(read_json(args.followup), read_json(args.instance_catalog))
    args.output_dir.mkdir(parents=True, exist_ok=True)
    (args.output_dir / "validation.json").write_text(json.dumps(report, indent=2) + "\n")
    text = render(report)
    (args.output_dir / "validation.txt").write_text(text)
    print(text, end="")
    return 0 if report["accepted"] else 2


if __name__ == "__main__":
    raise SystemExit(main())
