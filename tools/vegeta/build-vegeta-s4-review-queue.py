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
    ap.add_argument('--review-seeds',type=Path)
    ap.add_argument('--output-json',type=Path,required=True)
    ap.add_argument('--output-md',type=Path,required=True)
    ap.add_argument('--top',type=int,default=50)
    ns=ap.parse_args()
    fam=read(ns.family_summary); sels=read(ns.selector_summary); cov=read(ns.coverage)
    seed_doc=read(ns.review_seeds) if ns.review_seeds else {'decisions': []}
    seeds_by_address={str(r.get('address') or '').lower():r for r in seed_doc.get('decisions') or [] if r.get('address')}
    addresses={r['address']:r for r in fam.get('top_addresses') or [] if r.get('address')}
    direct_by_addr={}
    for row in sels.get('direct_destination_selectors') or []:
        direct_by_addr.setdefault(row['address'],[]).append({'selector':row['selector'],'count':row['transactions']})
    frames_by_addr={}
    for row in sels.get('call_frame_selectors') or []:
        frames_by_addr.setdefault(row['address'],[]).append({'selector':row['selector'],'count':row['frames']})
    queue=[]; by_family=Counter()
    conflict_rows={str(r.get('address') or '').lower():r for r in (cov.get('top_unmapped_conflict_owners') or [])}
    gas_rows={str(r.get('address') or '').lower():r for r in (cov.get('top_unmapped_state_gas_owners') or [])}
    ranked_addresses=[]
    for row in sorted((cov.get('top_unmapped_conflict_owners') or [])[:ns.top], key=lambda r:-int(r.get('owner_pair_attributions',0) or 0)):
        a=str(row.get('address') or '').lower()
        if a and a not in ranked_addresses: ranked_addresses.append(a)
    for row in sorted((cov.get('top_unmapped_state_gas_owners') or [])[:ns.top], key=lambda r:-int(r.get('gas_attributions',0) or 0)):
        a=str(row.get('address') or '').lower()
        if a and a not in ranked_addresses: ranked_addresses.append(a)
    for address in ranked_addresses[: max(ns.top, len(seeds_by_address))]:
        crow=conflict_rows.get(address,{})
        grow=gas_rows.get(address,{})
        meta=addresses.get(address,{})
        family=meta.get('runtime_code_family')
        pairs=int(crow.get('owner_pair_attributions',grow.get('owner_pair_attributions',0)) or 0)
        if family: by_family[family]+=pairs
        seed=seeds_by_address.get(address,{})
        queue.append({
          'address':address,
          'runtime_code_family':family,
          'owner_pair_attributions':pairs,
          'gas_attributions':int(grow.get('gas_attributions',crow.get('gas_attributions',0)) or 0),
          'unmapped_state_transactions':int(grow.get('transactions',crow.get('transactions',0)) or 0),
          'storage_access_records':int(crow.get('access_records',grow.get('access_records',0)) or 0),
          'direct_selectors':direct_by_addr.get(address,[])[:12],
          'call_frame_selectors':frames_by_addr.get(address,[])[:12],
          'seed_priority':seed.get('priority'),
          'identity_hint':seed.get('identity_hint'),
          'suggested_native_family':seed.get('suggested_native_family'),
          'seed_review_status':seed.get('review_status'),
          'seed_semantic_notes':seed.get('semantic_notes'),
          'review_action':'identify verified source/interface; map only exercised selectors to an implemented native family or leave explicit fallback',
        })
    report={
      'schema_version':1,'dataset':'vegeta-s4',
      'current_conflict_coverage':float((cov.get('source_conflict_coverage') or {}).get('coverage',0.0)),
      'current_block_median_coverage':float((cov.get('block_balanced_conflict_coverage') or {}).get('median_coverage') or 0.0),
      'current_conflict_relevant_access_coverage':float((cov.get('conflict_relevant_storage_access_coverage') or {}).get('access_record_coverage') or 0.0),
      'current_storage_access_coverage_diagnostic':float((cov.get('storage_access_coverage') or {}).get('access_record_coverage') or 0.0),
      'top_unmapped_runtime_families':[{'runtime_code_family':k,'owner_pair_attributions':v} for k,v in by_family.most_common()],
      'review_queue':queue,
      'review_seed_source':str(ns.review_seeds) if ns.review_seeds else None,
      'priority_review_batch':[r for r in queue if r.get('seed_priority') is not None],
      'top_unmapped_state_gas_owners':sorted(queue,key=lambda r:(-r.get('gas_attributions',0),-r.get('owner_pair_attributions',0)))[:20],
      'publication_rule':'Do not promote the candidate family map until new mappings/selectors are manually reviewed and the S4 conflict + median conflict-bearing-block family-freeze gates pass. Access/gas metrics remain diagnostics.',
    }
    write(ns.output_json,report)
    gas=float((cov.get('gas_weighted_family_coverage') or {}).get('fully_selected_family_state_gas_coverage') or 0.0)
    lines=['# Vegeta S4 semantic review queue','',f"Current conflict coverage: {100*report['current_conflict_coverage']:.2f}%",f"Median conflict-bearing block coverage: {100*report['current_block_median_coverage']:.2f}%",f"Conflict-relevant storage-access coverage: {100*report['current_conflict_relevant_access_coverage']:.2f}%",f"All storage-access coverage: {100*report['current_storage_access_coverage_diagnostic']:.2f}% (diagnostic)",f"Fully mapped source-state gas coverage: {100*gas:.2f}% (conservative diagnostic; not a gate)",'']
    if report['priority_review_batch']:
        lines += ['## First review batch (human-attested; suggestions are not executable mappings)']
        for row in sorted(report['priority_review_batch'],key=lambda r:(int(r.get('seed_priority') or 9999),-r['owner_pair_attributions'])):
            lines.append(f"- P{row['seed_priority']} `{row['address']}` family `{row['runtime_code_family']}` — {row.get('identity_hint') or 'identity pending'}; suggested native={row.get('suggested_native_family') or 'new/manual'}; conflicts={row['owner_pair_attributions']}")
        lines += ['', 'Edit the review decisions file, mark a row `reviewed`, set `reviewed_native_family`, and provide a concrete `mapping_basis` before applying it.', '']
    lines += ['## Top unmapped runtime families']
    for row in report['top_unmapped_runtime_families'][:20]: lines.append(f"- `{row['runtime_code_family']}` — owner-pair attributions: {row['owner_pair_attributions']}")
    lines += ['', '## Top unmapped source-state gas owners']
    for row in report['top_unmapped_state_gas_owners'][:20]:
        lines.append(f"- `{row['address']}` — gas attribution: {row['gas_attributions']}; tx: {row['unmapped_state_transactions']}; conflict pairs: {row['owner_pair_attributions']}")
    lines += ['', '## Top unmapped conflict/gas owners']
    for row in queue[:ns.top]:
        ds=', '.join(f"{x['selector']} ({x['count']})" for x in row['direct_selectors'][:6]) or 'none'
        fs=', '.join(f"{x['selector']} ({x['count']})" for x in row['call_frame_selectors'][:6]) or 'none'
        lines += [f"### `{row['address']}`",f"- runtime family: `{row['runtime_code_family'] or 'unknown'}`",f"- conflict-owner pair attributions: {row['owner_pair_attributions']}",f"- storage access records: {row['storage_access_records']}",f"- gas attribution: {row.get('gas_attributions',0)} across {row.get('unmapped_state_transactions',0)} tx",f"- direct selectors: {ds}",f"- call-frame selectors: {fs}"]
        if row.get('seed_priority') is not None:
            lines += [f"- first-batch priority: P{row['seed_priority']}",f"- identity hint: {row.get('identity_hint') or 'none'}",f"- suggested native family (review hint only): {row.get('suggested_native_family') or 'new/manual'}",f"- semantic note: {row.get('seed_semantic_notes') or ''}"]
        lines += ['']
    ns.output_md.parent.mkdir(parents=True,exist_ok=True); ns.output_md.write_text('\n'.join(lines)+'\n',encoding='utf-8')
    print(f"S4 review queue: unmapped owners={len(queue)} conflict coverage={100*report['current_conflict_coverage']:.2f}%")
    print(f"wrote {ns.output_json}"); print(f"wrote {ns.output_md}")
    return 0
if __name__=='__main__': raise SystemExit(main())
