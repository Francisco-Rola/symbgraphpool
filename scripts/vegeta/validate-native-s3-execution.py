#!/usr/bin/env python3
import argparse,json
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
def main():
 ap=argparse.ArgumentParser(); ap.add_argument('--output-dir',type=Path,default=ROOT/'benchmarks/corpora/vegeta-ethereum/s3/native-execution'); ns=ap.parse_args()
 access=ns.output_dir/'native-accesses.jsonl'; fidelity=ns.output_dir/'native-topology-fidelity.json'; attribution=ns.output_dir/'native-topology-attribution.json'; manifest=ns.output_dir/'execution-manifest.json'; errors=[]
 if not manifest.exists(): errors.append(f'missing {manifest}')
 else:
  m=json.loads(manifest.read_text()); cp=(m.get('normalization') or {}).get('caller_provenance') or {}
  if cp.get('mode')!='exact': errors.append(f"expected exact EVM msg.sender provenance, got {cp.get('mode')!r}")
  if cp.get('source')!='derived-geth-callTracer-effective-msg.sender': errors.append(f"unexpected caller provenance source {cp.get('source')!r}")
  if int(cp.get('missing_actions') or 0)!=0: errors.append(f"native execution plan contains {cp.get('missing_actions')} actions without explicit ethereum_msg_sender")
 if not access.exists(): errors.append(f'missing {access}')
 else:
  blocks=[json.loads(x) for x in access.read_text().splitlines() if x.strip()]; tx=sum(len(b.get('transactions',[])) for b in blocks)
  if len(blocks)!=101: errors.append(f'expected 101 executed blocks, got {len(blocks)}')
  if tx!=13783: errors.append(f'expected 13783 executed transactions, got {tx}')
  bad=[(b['block_number'],t['tx_index'],t['execution_status']) for b in blocks for t in b.get('transactions',[]) if t.get('execution_status') not in {'committed','reverted'}]
  if bad: errors.append(f'unexpected execution statuses: {bad[:5]}')
 if not fidelity.exists(): errors.append(f'missing {fidelity}')
 else:
  d=json.loads(fidelity.read_text());
  for path in [('conflict_pairs','precision'),('conflict_pairs','recall'),('critical_chain_fidelity','ratio'),('vegeta_hot_key_chain_fidelity','ratio')]:
   x=d
   for k in path: x=x.get(k) if isinstance(x,dict) else None
   if x is None: errors.append('missing metric '+'.'.join(path))
 if not attribution.exists(): errors.append(f'missing {attribution}')
 else:
  a=json.loads(attribution.read_text()); s=a.get('summary') or {}
  if fidelity.exists():
   d=json.loads(fidelity.read_text()); cp=d.get('conflict_pairs') or {}
   if s.get('false_positive_pairs')!=cp.get('false_positive'): errors.append('attribution false-positive count does not match fidelity report')
   if s.get('false_negative_pairs')!=cp.get('false_negative'): errors.append('attribution false-negative count does not match fidelity report')
  for path in [('critical_path_attribution','true_positive_only_native_sum'),('false_positive','by_family'),('false_positive','top_concrete_keys'),('false_negative','by_source_owner')]:
   x=a
   for k in path: x=x.get(k) if isinstance(x,dict) else None
   if x is None: errors.append('missing attribution '+'.'.join(path))
 print('Vegeta S3 native execution validation'); print(); print('accepted:', 'yes' if not errors else 'no'); print('errors:',len(errors));
 for e in errors: print('ERROR:',e)
 return 1 if errors else 0
if __name__=='__main__': raise SystemExit(main())
