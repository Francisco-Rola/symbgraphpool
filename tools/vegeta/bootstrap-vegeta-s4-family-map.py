#!/usr/bin/env python3
"""Create an S4 candidate family map by reusing already-reviewed runtime-code mappings.

The output is a review candidate, not publication evidence. Runtime-code-family mappings are copied
unchanged from the reviewed base map because identical historical bytecode is the same code family;
S4-specific coverage and selector semantics must still be audited before preparation.
"""
from __future__ import annotations
import argparse, hashlib, json
from pathlib import Path


def read(path: Path): return json.loads(path.read_text(encoding='utf-8'))

def write(path: Path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp=path.with_suffix(path.suffix+'.tmp'); tmp.write_text(json.dumps(value,indent=2,sort_keys=True)+'\n',encoding='utf-8'); tmp.replace(path)


def main():
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--base-map',type=Path,required=True)
    ap.add_argument('--family-summary',type=Path,required=True)
    ap.add_argument('--output',type=Path,required=True)
    ns=ap.parse_args()
    base=read(ns.base_map); summary=read(ns.family_summary)
    observed={str(r.get('runtime_code_family')) for r in summary.get('runtime_families') or [] if r.get('runtime_code_family')}
    mappings=[dict(r) for r in base.get('profile_mappings') or [] if str(r.get('ethereum_profile_family')) in observed]
    used_native={str(r['native_code_family']) for r in mappings}
    native={k:v for k,v in (base.get('native_code_families') or {}).items() if k in used_native}
    out={
      'schema_version':'1-s4-bootstrap-reviewed-runtime-families',
      'dataset':'vegeta-s4',
      'status':'candidate-from-reviewed-identical-runtime-families-pending-s4-selector-review',
      'candidate_only':True,
      'review_status':'NOT PUBLICATION READY: freeze as evaluation/vegeta/s4-native-family-map.v1.json only after S4 selector/family review and coverage gates pass',
      'bootstrap_base_map':str(ns.base_map),
      'bootstrap_base_map_sha256':hashlib.sha256(ns.base_map.read_bytes()).hexdigest(),
      'methodology_note':'Only identical runtime-code-family mappings already reviewed for the base map are reused automatically. New S4 bytecode families/selectors remain unmapped until explicit review. S4 has no exact SLOAD/SSTORE oracle.',
      'target_conflict_coverage':0.95,
      'selected_profile_families':len(mappings),
      'translation_rules':[
        'Every source transaction remains in the same block and transaction position.',
        'Identical runtime-code-family mappings may reuse previously reviewed native semantics, but S4 selector coverage is recomputed independently.',
        'Unsupported/unreviewed calls remain explicit background fallback or mapped-opaque actions; they are never silently approximated.',
        'Concrete source storage accesses are offline coverage evidence only and are never embedded in the native execution plan or exposed to prediction.',
        'S4 has no exact SLOAD/SSTORE oracle; exact-access headroom claims remain restricted to S3.',
      ],
      'expected_native_code_families':len(native),
      'expected_profile_mappings':len(mappings),
      'native_code_families':native,
      'profile_mappings':mappings,
      'observed_source_conflict_coverage':None,
      'observed_source_conflict_pairs':None,
      'observed_total_conflict_pairs':None,
    }
    write(ns.output,out)
    print(f"S4 candidate map: reused profile families={len(mappings)} native families={len(native)}")
    print(f"wrote {ns.output}")
    return 0
if __name__=='__main__': raise SystemExit(main())
