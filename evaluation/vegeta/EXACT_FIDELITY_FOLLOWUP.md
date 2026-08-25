# Vegeta S3 exact-ground-truth follow-up

This evaluation is deliberately **post-execution**. It does not expose historical concrete EVM
storage keys to native planning, state priming, symbolic analysis, or scheduling.

Run after the exact/hybrid SLOAD/SSTORE corpus and native execution outputs exist:

```bash
bash scripts/run-vegeta-s3-exact-followup.sh
```

The wrapper fails if a previously frozen pre-execution gate fails. The gate file is
`evaluation/vegeta/s3-exact-followup-gates.v1.json`; it re-encodes the already-frozen 95% aggregate
source-conflict, 80% median conflict-bearing-block, 75% semantic-transaction, and 50%
semantic-call-frame requirements. Do not retune these gates from the exact-ground-truth result.

## Outputs

`exact-fidelity-followup.{json,txt}` contains the publication-facing summary and machine-readable
report. The CSVs expose ranked false-negative causes, hot keys, critical-path-gap blocks, and exact
mapping coverage per block.

The report performs four follow-ups:

1. **Exact mapping gate recheck.** Source conflict-pair coverage is recomputed from the exact source
   storage graph using the already-frozen native instance catalog. A pair is considered covered when
   at least one concrete source storage owner causing the pair has a frozen native instance mapping.
2. **Fallback sensitivity.** Every transaction listed in the exact corpus manifest's
   `trace_semantics_exceptions` is removed from **both** source and native graphs, along with all
   incident pairs, and the headline topology metrics are recomputed. This quantifies whether the
   explicit public-RPC fallback exceptions are topologically material.
3. **FN / critical-path diagnosis.** Remaining false negatives are ranked by source owner, profile,
   key, and coverage class. Critical-path credit is reported separately so fixes can prioritize
   dependencies that matter to the source longest path rather than chasing raw pair count.
4. **Hot-key diagnosis.** Source and native concrete keys are ranked by their contribution when they
   tie for a per-block hot-key maximum, with native family/instance/action provenance attached.

When the original public-RPC S3 corpus is present, the report also computes an exact-SLOAD/SSTORE vs
prestateTracer ground-truth ablation. That ablation is evaluation evidence only; it is never used to
alter native execution.

## Interpretation policy

A `mapped-owner-semantic-gap` false negative means the source owner is already represented by a
frozen native instance but the concrete native execution still misses that source conflict pair. It
is a diagnostic bucket, **not** proof of a particular bug. Inspect high critical-path-credit rows and
patch only principled semantic/state-model errors. Unmapped long-tail false negatives remain stated
coverage limitations unless a separately motivated native family extension is justified.

If the frozen gates pass and the dominant critical-path false negatives do not reveal a principled
translation bug, freeze topology fidelity and proceed to the scheduler/performance experiment. Keep
native bank-ledger dependencies as a separate augmentation rather than relabeling them as source
storage false positives.
