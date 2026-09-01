#!/usr/bin/env python3
from __future__ import annotations

import argparse, hashlib, json, re
from pathlib import Path

FORBIDDEN = ("actual_reads", "actual_writes", "predicted_reads", "predicted_writes")


def norm(s: str) -> str:
    return re.sub(r"\s+", " ", s.strip())


def load_json(path: Path):
    with path.open() as f:
        return json.load(f)


def validate_family(root: Path, family: dict, require_wasm: bool) -> list[str]:
    errors=[]
    src=root/family['source']; sym=root/family['symbolic_analysis']; wasm=root/family['wasm_artifact']
    if not src.is_file(): errors.append(f"{family['native_code_family']}: missing source {family['source']}"); return errors
    if not sym.is_file(): errors.append(f"{family['native_code_family']}: missing symbolic {family['symbolic_analysis']}"); return errors
    if require_wasm and not wasm.is_file(): errors.append(f"{family['native_code_family']}: missing wasm {family['wasm_artifact']}")
    text=src.read_text()
    for token in FORBIDDEN:
        if token in text: errors.append(f"{family['native_code_family']}: forbidden trace-oracle token {token} in source")
    doc=load_json(sym)
    if doc.get('schema_version','').split('-',1)[0] != '3.1': errors.append(f"{family['native_code_family']}: unsupported symbolic schema")
    if doc.get('source') != family['source']: errors.append(f"{family['native_code_family']}: symbolic source path mismatch")
    prov=doc.get('analysis_provenance',{})
    if prov.get('method') != 'llm-source-derived': errors.append(f"{family['native_code_family']}: symbolic provenance method is not llm-source-derived")
    if prov.get('historical_trace_keys_used') is not False: errors.append(f"{family['native_code_family']}: symbolic provenance must explicitly deny historical trace keys")
    digest=hashlib.sha256(src.read_bytes()).hexdigest()
    if prov.get('source_sha256') != digest: errors.append(f"{family['native_code_family']}: source SHA-256 mismatch; regenerate symbolic analysis")
    profiles={p.get('entrypoint') for p in doc.get('profiles',[])}
    for req in family.get('required_profiles',[]):
        if req not in profiles: errors.append(f"{family['native_code_family']}: missing required symbolic profile {req}")
    declared=set(doc.get('storage_resources',{}))
    lines=text.splitlines()
    for profile in doc.get('profiles',[]):
        for access in profile.get('accesses',[]):
            if access.get('resource') not in declared: errors.append(f"{family['native_code_family']}:{profile.get('entrypoint')}: undeclared resource {access.get('resource')}")
            ev=access.get('evidence') or {}
            if ev.get('file') != family['source']: errors.append(f"{family['native_code_family']}:{profile.get('entrypoint')}: evidence file mismatch")
            start,end=ev.get('start_line',0),ev.get('end_line',0)
            if not (1 <= start <= end <= len(lines)):
                errors.append(f"{family['native_code_family']}:{profile.get('entrypoint')}: invalid evidence range {start}-{end}")
            else:
                window='\n'.join(lines[start-1:end])
                if norm(ev.get('code','')) not in norm(window): errors.append(f"{family['native_code_family']}:{profile.get('entrypoint')}: evidence code no longer matches source lines {start}-{end}")
    return errors


def main() -> int:
    ap=argparse.ArgumentParser()
    ap.add_argument('--repo-root', default=Path(__file__).resolve().parents[2], type=Path)
    ap.add_argument('--manifest', default='evaluation/vegeta/s3-native-implementation-manifest.v1.json')
    ap.add_argument('--final-map', default='benchmarks/corpora/vegeta-ethereum/s3/native-plan/final-native-family-map.v2.json')
    ap.add_argument('--selector-map', default='benchmarks/corpora/vegeta-ethereum/s3/native-plan/selector-semantic-map.json')
    ap.add_argument('--require-generated-maps', action='store_true')
    ap.add_argument('--require-wasm-artifacts', action='store_true')
    ap.add_argument('--json-output')
    ap.add_argument('--text-output')
    args=ap.parse_args(); root=args.repo_root.resolve(); manifest=load_json(root/args.manifest)
    errors=[]
    fams={f['native_code_family']:f for f in manifest['families']}
    for family in manifest['families']: errors.extend(validate_family(root,family,args.require_wasm_artifacts))
    for rel,label in ((args.final_map,'final map'),(args.selector_map,'selector map')):
        p=root/rel
        if not p.exists():
            if args.require_generated_maps: errors.append(f"missing generated {label}: {rel}")
            continue
        d=load_json(p)
        if label=='final map':
            used=set(d.get('base_native_code_families',[]))|set(d.get('additional_candidate_archetypes',{}))
        else:
            used={r.get('native_code_family') for r in d.get('rules',[]) if r.get('native_code_family') not in (None,'system')}
        unknown=sorted(used-set(fams))
        if unknown: errors.append(f"{label} references unimplemented families: {', '.join(unknown)}")
    report={'schema_version':1,'accepted':not errors,'families':len(fams),'errors':errors,'require_wasm_artifacts':args.require_wasm_artifacts,'require_generated_maps':args.require_generated_maps}
    dataset=str(manifest.get('dataset') or 'vegeta-s3')
    text=[f'Vegeta native implementation + symbolic provenance validation ({dataset})','',f"accepted: {'yes' if report['accepted'] else 'no'}",f"families: {len(fams)}",f"errors: {len(errors)}"]+[f"ERROR: {e}" for e in errors]
    if args.json_output:
        p=root/args.json_output; p.parent.mkdir(parents=True,exist_ok=True); p.write_text(json.dumps(report,indent=2)+'\n')
    if args.text_output:
        p=root/args.text_output; p.parent.mkdir(parents=True,exist_ok=True); p.write_text('\n'.join(text)+'\n')
    print('\n'.join(text))
    return 0 if report['accepted'] else 1

if __name__=='__main__': raise SystemExit(main())
