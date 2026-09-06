#!/usr/bin/env python3
"""Copy the symbolic analyses referenced by a frozen native-family map into a workload-local bundle."""
from __future__ import annotations
import argparse, hashlib, json, shutil
from pathlib import Path

ROOT=Path(__file__).resolve().parents[2]
def main():
    ap=argparse.ArgumentParser(description=__doc__); ap.add_argument('--family-map',type=Path,required=True); ap.add_argument('--output-dir',type=Path,required=True); ns=ap.parse_args()
    doc=json.loads(ns.family_map.read_text(encoding='utf-8')); ns.output_dir.mkdir(parents=True,exist_ok=True)
    copied=[]; seen=set()
    for name,row in sorted((doc.get('native_code_families') or {}).items()):
        rel=row.get('symbolic_analysis')
        if not rel: raise SystemExit(f"native family {name} has no symbolic_analysis path")
        src=(ROOT/rel).resolve()
        if not src.is_file(): raise SystemExit(f"missing symbolic analysis for {name}: {src}")
        if src in seen: continue
        seen.add(src); dst=ns.output_dir/src.name; shutil.copy2(src,dst)
        copied.append({'native_code_family':name,'source':str(rel),'file':dst.name,'sha256':hashlib.sha256(dst.read_bytes()).hexdigest()})
    manifest={'schema_version':1,'dataset':doc.get('dataset'),'family_map':str(ns.family_map),'profiles':copied}
    (ns.output_dir/'manifest.json').write_text(json.dumps(manifest,indent=2,sort_keys=True)+'\n',encoding='utf-8')
    print(f"symbolic bundle: profiles={len(copied)} output={ns.output_dir}")
    return 0
if __name__=='__main__': raise SystemExit(main())
