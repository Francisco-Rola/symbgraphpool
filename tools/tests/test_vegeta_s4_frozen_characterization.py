import hashlib, json, subprocess, sys, tempfile, unittest
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]

class VegetaS4FrozenCharacterizationTests(unittest.TestCase):
    def test_frozen_characterization_ranks_storage_conflict_family_without_rpc(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td); calls=d/'call-cache'; calls.mkdir(); out=d/'out'
            owner='0x'+'11'*20; other='0x'+'22'*20; slot='evm/'+'11'*20+'/'+'00'*32
            block={
                'schema_version':1,'block_number':18581726,'block_hash':'0xabc','timestamp':1,
                'transactions':[
                    {'tx_index':0,'tx_hash':'0x01','from':other,'to':owner,'selector':'0xa9059cbb','input':'0xa9059cbb','gas_used':100,'reads':[],'writes':[slot]},
                    {'tx_index':1,'tx_hash':'0x02','from':other,'to':owner,'selector':'0x70a08231','input':'0x70a08231','gas_used':50,'reads':[slot],'writes':[]},
                ],
            }
            (d/'corpus.jsonl').write_text(json.dumps(block)+'\n')
            thin=dict(block); thin['transactions']=[{k:v for k,v in tx.items() if k not in ('reads','writes')} for tx in block['transactions']]
            (d/'thin.jsonl').write_text(json.dumps(thin)+'\n')
            code='0x6000'; family=hashlib.sha256(bytes.fromhex('6000')).hexdigest()
            (d/'code.json').write_text(json.dumps({owner:{'block_number':18581726,'code':code},other:{'block_number':18581726,'code':'0x'}}))
            (d/'relevant.json').write_text(json.dumps({'schema_version':1,'addresses':[{'address':owner,'first_seen_block':18581726},{'address':other,'first_seen_block':18581726}]}))
            traced={'schema_version':1,'block_number':18581726,'block_hash':'0xabc','transactions':[
                {'tx_hash':'0x01','result':{'type':'CALL','to':owner,'input':'0xa9059cbb','calls':[]}},
                {'tx_hash':'0x02','result':{'type':'CALL','to':owner,'input':'0x70a08231','calls':[]}},
            ]}
            (calls/'18581726.json').write_text(json.dumps(traced))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/characterize-vegeta-s4-frozen.py'),
                '--corpus',str(d/'corpus.jsonl'),'--thin-corpus',str(d/'thin.jsonl'),'--call-cache',str(calls),
                '--code-cache',str(d/'code.json'),'--relevant-addresses',str(d/'relevant.json'),'--output-dir',str(out)],check=True)
            report=json.loads((out/'family-summary.json').read_text())
            self.assertEqual(report['dataset'],'vegeta-s4')
            self.assertEqual(report['blocks'],1); self.assertEqual(report['transactions'],2)
            row=next(r for r in report['runtime_families'] if r['runtime_code_family']==family)
            self.assertEqual(row['conflict_owner_pair_attributions'],1)
            self.assertEqual(row['direct_transactions'],2)
            selectors=json.loads((out/'selector-summary.json').read_text())
            self.assertEqual(len(selectors['direct_destination_selectors']),2)

if __name__=='__main__': unittest.main()
