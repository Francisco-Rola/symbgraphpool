#!/usr/bin/env python3
"""Structural/correctness validator for the ConflictLab 1.0 submission experiment suite."""
from __future__ import annotations
import argparse, json, math, statistics
from collections import Counter, defaultdict
from pathlib import Path

EXPECTED = {
    "conflictlab-v1-core-state": 960,
    "conflictlab-v1-cutoff-divergence": 1440,
    "conflictlab-v1-serial-cutoff": 72,
    "conflictlab-v1-compaction-reference": 240,
    "conflictlab-v1-symbolic-granularity": 72,
    "conflictlab-v1-prediction-fault-recovery": 336,
    "conflictlab-v1-adaptation-transitions": 288,
    "conflictlab-v1-execution-semantics": 126,
    "conflictlab-v1-block-scaling": 168,
    "conflictlab-v1-policy-pareto": 96,
    "conflictlab-v1-bucket-sensitivity": 48,
    "conflictlab-v1-ordering-sensitivity": 36,
    "conflictlab-v1-vm-lifecycle": 24,
    "conflictlab-v1-statistical-headlines": 720,
    "conflictlab-v1-long-run-soak": 4,
}

def load(path: Path):
    out=[]
    for i,line in enumerate(path.read_text().splitlines(),1):
        if line.strip():
            try: out.append(json.loads(line))
            except json.JSONDecodeError as e: raise SystemExit(f"{path}:{i}: {e}")
    return out

def p(r,k,default=None): return r.get("metadata",{}).get("parameters",{}).get(k,default)
def env(r,k,default=None): return r.get("metadata",{}).get("environment",{}).get(k,default)
def med(xs): return statistics.median(xs) if xs else math.nan

def fail(msg): raise SystemExit(f"FAIL: {msg}")

def main():
    ap=argparse.ArgumentParser(); ap.add_argument("records",type=Path); ap.add_argument("--allow-partial",action="store_true")
    a=ap.parse_args(); records=load(a.records)
    if not records: fail("no records")
    counts=Counter(r.get("metadata",{}).get("experiment_id") for r in records)
    if not a.allow_partial:
        if counts != Counter(EXPECTED):
            fail(f"campaign counts differ: got={dict(counts)} expected={EXPECTED}")
    else:
        unknown=set(counts)-set(EXPECTED)
        if unknown: fail(f"unknown campaigns: {sorted(unknown)}")
    for i,r in enumerate(records,1):
        if r.get("schema_version") != 3: fail(f"record {i}: schema_version != 3")
        if r.get("correctness",{}).get("serial_equivalent") is not True: fail(f"record {i}: not serial-equivalent")
        md=r.get("metadata",{})
        if md.get("workers",0)>6 or md.get("physical_cores",0)>6: fail(f"record {i}: exceeds 6-core budget")
        if env(r,"conflictlab_backend") != "wasm": fail(f"record {i}: non-Wasm backend")
        oracle=r.get("parallelism",{}).get("perfect_conflict_parallel_lower_bound_nanos")
        serial=r.get("parallelism",{}).get("serial_equivalent_work_nanos")
        if not isinstance(oracle,int) or oracle<=0: fail(f"record {i}: missing perfect-conflict oracle bound")
        if isinstance(serial,int) and oracle>serial: fail(f"record {i}: oracle lower bound exceeds serial work")
        consensus=r.get("consensus",{})
        successful=consensus.get("successful_preexecution_receipts")
        failed=consensus.get("failed_preexecution_receipts")
        if (successful is None) != (failed is None):
            fail(f"record {i}: partial pre-execution success/failure accounting")
        if successful is not None and successful+failed != consensus.get("prepared_receipts"):
            fail(f"record {i}: pre-execution success/failure counts do not sum to prepared receipts")
    print(f"records={len(records)} correct={sum(r['correctness']['serial_equivalent'] is True for r in records)}/{len(records)}")

    # Uncontrolled predictor paths should remain miss-free; bank-mixed and injected hidden faults are intentional exceptions.
    unexpected=[]
    for r in records:
        if r.get("feedback",{}).get("candidate_misses",0)==0: continue
        exp=r["metadata"]["experiment_id"]
        intentional=(exp=="conflictlab-v1-prediction-fault-recovery" and p(r,"prediction_fault_mode")=="hidden-key") or (exp=="conflictlab-v1-execution-semantics" and p(r,"operation_mix")=="bank-mixed")
        if not intentional: unexpected.append((exp,p(r,"operation_mix"),r["feedback"]["candidate_misses"]))
    if unexpected: fail(f"unexpected candidate misses, first={unexpected[:5]}")

    # Dense/compact pairs: same logical relationship classification and state, compact representation never larger.
    comp=[r for r in records if r["metadata"]["experiment_id"]=="conflictlab-v1-compaction-reference"]
    if comp:
        pairs=defaultdict(dict)
        for r in comp:
            params=dict(r["metadata"]["parameters"]); toggle=params.pop("acg.compact_equivalence_groups")
            key=(r["metadata"]["mode"],r["metadata"]["seed"],tuple(sorted(params.items())))
            pairs[key][toggle]=r
        strict=0
        for key,v in pairs.items():
            if set(v)!={"true","false"}: fail(f"incomplete compaction pair {key}")
            c,d=v["true"],v["false"]
            for field in ("candidate_edges","low_edges","soft_edges","hard_edges"):
                if c["scheduling"].get(field)!=d["scheduling"].get(field): fail(f"compaction changed logical {field}: {key}")
            for field in ("wave_count","max_wave_width"):
                if c["scheduling"].get(field)!=d["scheduling"].get(field): fail(f"compaction changed scheduling semantics {field}: {key}")
            for field in ("positive_observations","negative_observations","candidate_misses"):
                if c["feedback"].get(field)!=d["feedback"].get(field): fail(f"compaction changed logical feedback {field}: {key}")
            for field in ("mean_probability_q16","mean_confidence_q16"):
                if c.get("adaptive_state",{}).get(field)!=d.get("adaptive_state",{}).get(field): fail(f"compaction changed posterior state {field}: {key}")
            if c["correctness"]["canonical_state_digest"]!=d["correctness"]["canonical_state_digest"]: fail("compact/dense digest mismatch")
            cm=c["scheduling"]["materialized_candidate_edges"]; dm=d["scheduling"]["materialized_candidate_edges"]
            if cm>dm: fail(f"compact materialization larger than dense: {key}")
            if c["scheduling"].get("scheduled_dependencies",0)>d["scheduling"].get("scheduled_dependencies",0): fail(f"compact READY-DAG larger than dense: {key}")
            strict += cm<dm
        if strict==0: fail("compaction campaign never reduced materialization")
        print(f"compaction_pairs={len(pairs)} strict_reductions={strict}")

    # Binding cutoffs must actually exercise both partial and complete pre-execution regimes.
    cut=[r for r in records if r["metadata"]["experiment_id"]=="conflictlab-v1-cutoff-divergence"]
    if cut:
        for ms in (25,50,100):
            xs=[r for r in cut if int(p(r,"consensus_cutoff_ms"))==ms]
            hits=sum(bool(r["consensus"]["cutoff_reached"]) for r in xs)
            if hits==0: fail(f"{ms}ms cutoff never bound")
            print(f"cutoff={ms}ms reached={hits}/{len(xs)}")
        if not any(not r["consensus"]["cutoff_reached"] for r in cut if int(p(r,"consensus_cutoff_ms"))>=250):
            fail("long cutoff regimes never demonstrated complete pre-execution")

    serial_cut=[r for r in records if r["metadata"]["experiment_id"]=="conflictlab-v1-serial-cutoff"]
    if serial_cut:
        fractions=[]
        for r in serial_cut:
            d=max(1,r["consensus"]["decided_transactions"]); fractions.append(r["consensus"]["receipts_ready_by_cutoff"]/d)
        if not any(0.05 < x < 0.95 for x in fractions): fail("serial cutoff campaign has no partial-prefix case")
        if not any(x>=0.999 for x in fractions): fail("serial cutoff campaign has no fully-preexecuted case")
        print(f"serial_prefix_ready_range={min(fractions):.3f}..{max(fractions):.3f}")

    # Prediction-fault safety/recovery machinery must be exercised.
    faults=[r for r in records if r["metadata"]["experiment_id"]=="conflictlab-v1-prediction-fault-recovery"]
    if faults:
        hidden=[r for r in faults if p(r,"prediction_fault_mode")=="hidden-key"]
        spurious=[r for r in faults if p(r,"prediction_fault_mode")=="spurious-key"]
        if sum(r["feedback"]["candidate_misses"] for r in hidden)==0: fail("hidden-key faults produced no candidate misses")
        if max((r.get("adaptive_state",{}).get("runtime_fallback_relationships",0) for r in hidden),default=0)==0: fail("hidden-key faults produced no runtime fallback relationship")
        if sum(r["feedback"]["candidate_misses"] for r in spurious)!=0: fail("spurious-key false positives unexpectedly produced candidate misses")
        print(f"fault_hidden_candidate_misses={sum(r['feedback']['candidate_misses'] for r in hidden)} fallback_max={max(r.get('adaptive_state',{}).get('runtime_fallback_relationships',0) for r in hidden)}")

    # Runtime semantics counters prove each intentionally targeted state mechanism was actually hit.
    sem=[r for r in records if r["metadata"]["experiment_id"]=="conflictlab-v1-execution-semantics"]
    if sem:
        by=defaultdict(list)
        for r in sem: by[p(r,"operation_mix")].append(r)
        expected={"point-mixed","stateful-mixed","range-delete","bank-funds","bank-mixed","instantiate","full"}
        if set(by)!=expected: fail(f"operation mixes missing: {expected-set(by)}")
        def total(mix,field): return sum(r["execution"]["contract"].get(field,0) for r in by[mix])
        if total("range-delete","host_storage_scans")<=0 or total("range-delete","host_storage_removes")<=0: fail("range-delete did not exercise scan/remove")
        if sum(r["execution"]["contract"].get("receipt_balance_writes",0) for r in by["bank-funds"]+by["bank-mixed"])<=0: fail("bank mixes did not exercise balance writes")
        if total("bank-mixed","host_queries")<=0: fail("bank-mixed did not exercise host queries")
        if total("bank-mixed","mvcc_balance_reads")<=0 or total("bank-mixed","mvcc_all_balances_reads")<=0: fail("bank-mixed did not exercise point and all-balance MVCC reads")
        if sum(r["execution"]["contract"].get("receipt_created_contracts",0) for r in by["instantiate"])<=0: fail("instantiate did not exercise contract creation receipts")
        if total("stateful-mixed","host_storage_removes")<=0: fail("stateful-mixed did not exercise order deletion")
        stateful_receipt_failures=sum(
            r.get("consensus",{}).get("failed_preexecution_receipts") or 0
            for r in by["stateful-mixed"]+by["full"]
        )
        if stateful_receipt_failures<=0: fail("stateful/full workloads did not exercise failed speculative receipts")
        print(f"execution_semantics=point,delete/range,bank-write,bank-query,all-balances,contract-create,stateful-remove failed_speculative_receipts={stateful_receipt_failures}")

    # Adaptation telemetry and long-run state must be present.
    transitions=[r for r in records if r["metadata"]["experiment_id"]=="conflictlab-v1-adaptation-transitions"]
    if transitions:
        depths={int(p(r,"postchange_warmup_blocks")) for r in transitions}
        if depths!={0,1,2,4,8,16}: fail(f"transition depths incorrect: {depths}")
        if not any(r.get("adaptive_state",{}).get("mean_confidence_q16",0)>0 for r in transitions): fail("adaptive-state telemetry absent")
    soak=[r for r in records if r["metadata"]["experiment_id"]=="conflictlab-v1-long-run-soak"]
    if soak and not all(int(p(r,"warmup_blocks"))>=1000 for r in soak): fail("soak does not include >=1000 prior blocks")

    dirty=sum(env(r,"git_dirty")=="true" for r in records)
    if dirty: print(f"WARNING: git_dirty=true in {dirty}/{len(records)} records; final paper rerun should use a clean commit")
    print("PASS: ConflictLab 1.0 submission-suite structural/correctness validation")
if __name__=="__main__": main()
