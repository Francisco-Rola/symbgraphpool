#!/usr/bin/env python3
"""Install the reviewed subset of the S4 fifth batch, leaving unresolved rows fail-closed.

The installer validates checked-in review conclusions against the freshly generated local evidence
artifact before changing the review base/workspace.  Reviewed rows may add owner-scoped
implementation-profile mappings (important for generic proxies). Pending/unresolved rows are kept
in the workspace but never become executable mappings. New conservative native aliases must be
registered by the checked-in fifth-batch family extension.
"""
from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path
from typing import Any


def read(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def atomic(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    tmp.replace(path)


def generated_family_by_runtime(doc: dict) -> dict[str, dict]:
    return {
        str(row.get("runtime_code_family") or ""): row
        for row in (doc.get("families") or [])
        if row.get("runtime_code_family")
    }


def generated_contract_ids(family: dict) -> set[str]:
    ids: set[str] = set()
    for owner in family.get("owner_evidence") or []:
        source = owner.get("verified_source") or {}
        if source.get("contract_identifier"):
            ids.add(str(source["contract_identifier"]))
    for target in family.get("delegate_targets_exact") or []:
        source = target.get("verified_source") or {}
        if source.get("contract_identifier"):
            ids.add(str(source["contract_identifier"]))
    return ids


def validate_evidence_record(record: dict, generated_by_family: dict[str, dict]) -> None:
    blocker = str(record.get("blocker_runtime_code_family") or "")
    decision_family = str(record.get("decision_runtime_code_family") or blocker)
    owner = str(record.get("storage_owner") or "").lower()
    family = generated_by_family.get(blocker)
    if family is None:
        raise SystemExit(f"fifth-batch evidence blocker family not present in generated dossier: {blocker}")
    owners = {str(x).lower() for x in (family.get("all_observed_storage_owners") or [])}
    if owner and owner not in owners:
        raise SystemExit(f"fifth-batch evidence owner {owner} is not observed for blocker family {blocker}")

    generated_direct = {str(x.get("value") or "") for x in (family.get("direct_selectors_exact") or [])}
    generated_calls = {str(x.get("value") or "") for x in (family.get("call_frame_selectors_exact") or [])}
    if not set(map(str, record.get("direct_selectors") or [])).issubset(generated_direct):
        raise SystemExit(f"checked-in direct selector evidence drifted for blocker family {blocker}")
    if not set(map(str, record.get("call_frame_selectors") or [])).issubset(generated_calls):
        raise SystemExit(f"checked-in call-frame selector evidence drifted for blocker family {blocker}")

    target = str(record.get("delegate_target") or "").lower()
    if target:
        targets = {
            (str(x.get("target_address") or "").lower(), str(x.get("target_runtime_code_family") or ""))
            for x in (family.get("delegate_targets_exact") or [])
        }
        if (target, decision_family) not in targets:
            raise SystemExit(
                f"checked-in delegate evidence drifted for blocker {blocker}: "
                f"expected target={target} family={decision_family}"
            )

    expected_ids = set(map(str, record.get("verified_contract_identifiers") or []))
    actual_ids = generated_contract_ids(family)
    if not expected_ids.issubset(actual_ids):
        missing = sorted(expected_ids - actual_ids)
        raise SystemExit(f"verified-source identity evidence drifted for blocker {blocker}: missing {missing}")


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("--review-base", type=Path, required=True)
    ap.add_argument("--workspace-decisions", type=Path, required=True)
    ap.add_argument("--family-extension", type=Path, required=True)
    ap.add_argument("--fifth-batch-decisions", type=Path, required=True)
    ap.add_argument("--review-evidence", type=Path, required=True)
    ap.add_argument("--generated-evidence", type=Path, required=True)
    ap.add_argument("--pending-output", type=Path, required=True)
    ns = ap.parse_args()

    base = read(ns.review_base)
    workspace = read(ns.workspace_decisions)
    ext = read(ns.family_extension)
    batch = read(ns.fifth_batch_decisions)
    evidence = read(ns.review_evidence)
    generated = read(ns.generated_evidence)
    for label, doc in (
        ("review base", base),
        ("workspace decisions", workspace),
        ("family extension", ext),
        ("fifth batch", batch),
        ("review evidence", evidence),
        ("generated evidence", generated),
    ):
        if doc.get("dataset") != "vegeta-s4":
            raise SystemExit(f"{label} must have dataset=vegeta-s4")

    generated_by_family = generated_family_by_runtime(generated)
    evidence_by_id = {
        str(row.get("evidence_id") or ""): row
        for row in (evidence.get("records") or [])
        if row.get("evidence_id")
    }
    native = dict(base.get("native_code_families") or {})
    added_native: list[str] = []
    for name, config in sorted((ext.get("native_code_families") or {}).items()):
        if name in native and native[name] != config:
            raise SystemExit(f"refusing to replace a different existing native family definition: {name}")
        if name not in native:
            added_native.append(name)
        native[name] = config

    rows = sorted(
        batch.get("decisions") or [],
        key=lambda r: (int(r.get("priority", 9999)), str(r.get("runtime_code_family", ""))),
    )
    if not rows:
        raise SystemExit("fifth-batch decisions file is empty")

    current = [dict(row) for row in workspace.get("decisions") or []]
    index = {
        str(row.get("runtime_code_family") or ""): i
        for i, row in enumerate(current)
        if row.get("runtime_code_family")
    }

    reviewed_count = 0
    newly_validated_reviewed = 0
    already_applied_reviewed = 0
    for row in rows:
        family = str(row.get("runtime_code_family") or "")
        blocker = str(row.get("blocker_runtime_code_family") or family)
        status = str(row.get("review_status") or "pending").lower()
        evidence_id = str(row.get("evidence_id") or "")
        conclusion = str(row.get("review_conclusion") or "").strip()
        sources = row.get("evidence_sources") or []
        ev = evidence_by_id.get(evidence_id)
        if status not in {"reviewed", "pending", "unresolved"}:
            raise SystemExit(f"unsupported fifth-batch review_status={status!r} for {family or row}")
        if not family or not blocker or not evidence_id or not ev:
            raise SystemExit(f"fifth-batch row lacks family/blocker/evidence linkage: {row}")
        if str(ev.get("decision_runtime_code_family") or "") != family:
            raise SystemExit(f"review evidence {evidence_id!r} does not match decision runtime family {family}")
        if str(ev.get("blocker_runtime_code_family") or "") != blocker:
            raise SystemExit(f"review evidence {evidence_id!r} does not match blocker runtime family {blocker}")
        if str(ev.get("storage_owner") or "").lower() != str(row.get("address") or "").lower():
            raise SystemExit(f"review evidence {evidence_id!r} does not match decision storage owner")
        if not conclusion or not sources or not str(ev.get("review_conclusion") or "").strip() or not (ev.get("sources") or []):
            raise SystemExit(f"fifth-batch row/evidence is incomplete: {evidence_id!r}")

        if status == "reviewed":
            reviewed_count += 1
            target = str(row.get("reviewed_native_family") or "")
            basis = str(row.get("mapping_basis") or "").strip()
            if not target or not basis:
                raise SystemExit(f"reviewed fifth-batch row lacks native target/mapping basis: {family}")
            if target not in native:
                raise SystemExit(
                    f"fifth-batch family {family} references unknown native family {target!r}; "
                    "register the reviewed implementation/dependency family first"
                )

            old = current[index[family]] if family in index else None
            old_status = str((old or {}).get("review_status") or "pending").lower()
            if old_status == "reviewed":
                # Historical reviewed rows are authoritative cumulative state. They must still
                # match the checked-in decision exactly, but they do not need to reappear in a
                # newly regenerated, shortlist-scoped evidence dossier.
                if old != row:
                    raise SystemExit(
                        f"refusing to replace an existing different reviewed decision for runtime family {family}"
                    )
                already_applied_reviewed += 1
            else:
                # Only genuinely new/promoted reviewed rows depend on the current transient
                # generated dossier. This keeps incremental reruns idempotent while preserving
                # fail-closed evidence validation for every new executable mapping.
                validate_evidence_record(ev, generated_by_family)
                newly_validated_reviewed += 1
        else:
            if row.get("reviewed_native_family") or str(row.get("mapping_basis") or "").strip():
                raise SystemExit(f"non-reviewed fifth-batch row must not carry an executable mapping: {family}")

    if reviewed_count == 0:
        raise SystemExit("fifth batch contains no reviewed executable rows")

    # All validation passed: persist the extension and review decisions atomically per file.
    base["native_code_families"] = native
    base["expected_native_code_families"] = len(native)
    base["candidate_only"] = True
    base["s4_fifth_batch_native_extension"] = str(ns.family_extension)
    base["s4_fifth_batch_review_evidence"] = str(ns.review_evidence)
    base["s4_fifth_batch_review_evidence_sha256"] = hashlib.sha256(ns.review_evidence.read_bytes()).hexdigest()
    atomic(ns.review_base, base)

    appended: list[str] = []
    promoted: list[str] = []
    unchanged: list[str] = []
    for row0 in rows:
        row = dict(row0)
        family = str(row["runtime_code_family"])
        if family not in index:
            current.append(row)
            index[family] = len(current) - 1
            appended.append(family)
            continue
        i = index[family]
        old = current[i]
        if old == row:
            unchanged.append(family)
            continue
        old_status = str(old.get("review_status") or "pending").lower()
        new_status = str(row.get("review_status") or "pending").lower()
        if old_status != "reviewed":
            current[i] = row
            promoted.append(family)
            continue
        if new_status != "reviewed":
            raise SystemExit(f"refusing to replace an existing reviewed decision with {new_status}: {family}")
        raise SystemExit(f"refusing to replace an existing different reviewed decision for runtime family {family}")

    workspace["decisions"] = sorted(
        current,
        key=lambda r: (int(r.get("priority", 9999)), str(r.get("runtime_code_family", ""))),
    )
    workspace["installed_fifth_batch_source"] = str(ns.fifth_batch_decisions)
    workspace["installed_fifth_batch_native_extension"] = str(ns.family_extension)
    workspace["installed_fifth_batch_review_evidence"] = str(ns.review_evidence)
    workspace["installed_fifth_batch_generated_evidence"] = str(ns.generated_evidence)
    workspace["installed_fifth_batch_appended_families"] = appended
    workspace["installed_fifth_batch_promoted_families"] = promoted
    workspace["installed_fifth_batch_unchanged_families"] = unchanged
    atomic(ns.workspace_decisions, workspace)

    pending = [
        dict(r) for r in workspace["decisions"]
        if str(r.get("review_status") or "pending").lower() != "reviewed"
    ]
    atomic(ns.pending_output, {
        "schema_version": 1,
        "dataset": "vegeta-s4",
        "purpose": "Remaining explicit manual-review rows after the reviewed fifth-batch subset; non-executable until reviewed.",
        "source": str(ns.fifth_batch_decisions),
        "pending_count": len(pending),
        "decisions": pending,
    })
    print(f"fifth-batch conservative native aliases added: {len(added_native)}")
    print(f"fifth-batch reviewed executable rows: {reviewed_count}")
    print(f"fifth-batch new reviewed rows evidence-validated: {newly_validated_reviewed}")
    print(f"fifth-batch already-applied reviewed rows skipped from transient evidence validation: {already_applied_reviewed}")
    print(f"fifth-batch rows appended: {len(appended)}")
    print(f"fifth-batch pending rows promoted/replaced: {len(promoted)}")
    print(f"fifth-batch identical rows retained: {len(unchanged)}")
    print(f"workspace unresolved/pending rows after merge: {len(pending)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
