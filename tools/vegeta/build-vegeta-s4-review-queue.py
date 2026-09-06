#!/usr/bin/env python3
"""Build a concise S4 semantic-review queue from frozen characterization and candidate-map coverage."""
from __future__ import annotations
import argparse, json
from collections import Counter
from pathlib import Path


def read(path: Path): return json.loads(path.read_text(encoding='utf-8'))
def write(path: Path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp=path.with_suffix(path.suffix+'.tmp'); tmp.write_text(json.dumps(value,indent=2,sort_keys=True)+'\n',encoding='utf-8'); tmp.replace(path)


def main():
    ap=argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--family-summary',type=Path,required=True)
    ap.add_argument('--selector-summary',type=Path,required=True)
    ap.add_argument('--coverage',type=Path,required=True)
    ap.add_argument('--output-json',type=Path,required=True)
    ap.add_argument('--output-md',type=Path,required=True)
    ap.add_argument('--top',type=int,default=50)
    ns=ap.parse_args()
    fam=read(ns.family_summary); sels=read(ns.selector_summary); cov=read(ns.coverage)
    addresses={r['address']:r for r in fam.get('top_addresses') or [] if r.get('address')}
    direct_by_addr={}
    for row in sels.get('direct_destination_selectors') or []:
        direct_by_addr.setdefault(row['address'],[]).append({'selector':row['selector'],'count':row['transactions']})
    frames_by_addr={}
    for row in sels.get('call_frame_selectors') or []:
        frames_by_addr.setdefault(row['address'],[]).append({'selector':row['selector'],'count':row['frames']})
    queue=[]; by_family=Counter()
    for row in (cov.get('top_unmapped_conflict_owners') or [])[:ns.top]:
        address=str(row.get('address') or '').lower(); meta=addresses.get(address,{})
        family=meta.get('runtime_code_family')
        pairs=int(row.get('owner_pair_attributions',0) or 0)
        if family: by_family[family]+=pairs
        queue.append({
          'address':address,
          'runtime_code_family':family,
          'owner_pair_attributions':pairs,
          'storage_access_records':int(row.get('access_records',0) or 0),
          'direct_selectors':direct_by_addr.get(address,[])[:12],
          'call_frame_selectors':frames_by_addr.get(address,[])[:12],
          'review_action':'identify verified source/interface; map only exercised selectors to an implemented native family or leave explicit fallback',
        })
    report={
      'schema_version':1,'dataset':'vegeta-s4',
      'current_conflict_coverage':float((cov.get('source_conflict_coverage') or {}).get('coverage',0.0)),
      'current_block_median_coverage':float((cov.get('block_balanced_conflict_coverage') or {}).get('median_coverage') or 0.0),
      'top_unmapped_runtime_families':[{'runtime_code_family':k,'owner_pair_attributions':v} for k,v in by_family.most_common()],
      'review_queue':queue,
      'publication_rule':'Do not promote the candidate family map until new mappings/selectors are manually reviewed and the S4 coverage gates pass.',
    }
    write(ns.output_json,report)
    lines=['# Vegeta S4 semantic review queue','',f"Current conflict coverage: {100*report['current_conflict_coverage']:.2f}%",f"Median conflict-bearing block coverage: {100*report['current_block_median_coverage']:.2f}%",'', '## Top unmapped runtime families']
    for row in report['top_unmapped_runtime_families'][:20]: lines.append(f"- `{row['runtime_code_family']}` — owner-pair attributions: {row['owner_pair_attributions']}")
    lines += ['', '## Top unmapped conflict owners']
    for row in queue[:ns.top]:
        ds=', '.join(f"{x['selector']} ({x['count']})" for x in row['direct_selectors'][:6]) or 'none'
        fs=', '.join(f"{x['selector']} ({x['count']})" for x in row['call_frame_selectors'][:6]) or 'none'
        lines += [f"### `{row['address']}`",f"- runtime family: `{row['runtime_code_family'] or 'unknown'}`",f"- conflict-owner pair attributions: {row['owner_pair_attributions']}",f"- storage access records: {row['storage_access_records']}",f"- direct selectors: {ds}",f"- call-frame selectors: {fs}",'']
    ns.output_md.parent.mkdir(parents=True,exist_ok=True); ns.output_md.write_text('\n'.join(lines)+'\n',encoding='utf-8')
    print(f"S4 review queue: unmapped owners={len(queue)} conflict coverage={100*report['current_conflict_coverage']:.2f}%")
    print(f"wrote {ns.output_json}"); print(f"wrote {ns.output_md}")
    return 0
if __name__=='__main__': raise SystemExit(main())
