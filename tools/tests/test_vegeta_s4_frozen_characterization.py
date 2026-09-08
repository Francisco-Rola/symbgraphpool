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


    def test_s4_corpus_provenance_records_published_mismatch_without_rewriting(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            contract='0x'+'11'*20; eoa='0x'+'22'*20
            block={'schema_version':1,'block_number':10,'block_hash':'0xabc','timestamp':1,'transactions':[
                {'tx_index':0,'tx_hash':'0x01','from':eoa,'to':contract,'selector':'0xa9059cbb','gas_used':100,'failed':False,'reads':[],'writes':[]},
                {'tx_index':1,'tx_hash':'0x02','from':eoa,'to':eoa,'selector':'0x','gas_used':21,'failed':True,'reads':[],'writes':[]},
            ]}
            (d/'corpus.jsonl').write_text(json.dumps(block)+'\n')
            (d/'code.json').write_text(json.dumps({contract:{'code':'0x6000'},eoa:{'code':'0x'}}))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/audit-vegeta-s4-corpus-provenance.py'),
                '--corpus',str(d/'corpus.jsonl'),'--code-cache',str(d/'code.json'),
                '--expected-blocks','1','--expected-start','10','--expected-end','10','--published-transactions','1',
                '--output',str(d/'out.json'),'--text-output',str(d/'out.txt')],check=True)
            out=json.loads((d/'out.json').read_text())
            self.assertTrue(out['internal_integrity']['pass'])
            self.assertEqual(out['transactions'],2)
            self.assertFalse(out['published_comparison']['matches'])
            self.assertEqual(out['published_comparison']['delta_transactions'],1)
            self.assertEqual(out['transaction_classes']['calldata_contract_call'],1)
            self.assertEqual(out['transaction_classes']['plain_value_or_eoa_transfer'],1)

    def test_family_coverage_reports_conservative_state_gas_coverage(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            mapped='0x'+'11'*20; unmapped='0x'+'22'*20
            slot1='evm/'+'11'*20+'/'+'00'*32; slot2='evm/'+'22'*20+'/'+'00'*32
            block={'schema_version':1,'block_number':1,'block_hash':'0xabc','timestamp':1,'transactions':[
                {'tx_index':0,'tx_hash':'0x01','from':mapped,'to':mapped,'selector':'0x','gas_used':100,'failed':False,'reads':[],'writes':[slot1]},
                {'tx_index':1,'tx_hash':'0x02','from':mapped,'to':unmapped,'selector':'0x','gas_used':900,'failed':False,'reads':[],'writes':[slot2]},
            ]}
            (d/'corpus.jsonl').write_text(json.dumps(block)+'\n')
            family=hashlib.sha256(bytes.fromhex('6000')).hexdigest()
            (d/'code.json').write_text(json.dumps({mapped:{'code':'0x6000'},unmapped:{'code':'0x6001'}}))
            (d/'mapping.json').write_text(json.dumps({'resolution_records':[]}))
            (d/'family.json').write_text(json.dumps({'dataset':'vegeta-s4','native_code_families':{'cw20-base':{}},'profile_mappings':[
                {'ethereum_profile_family':family,'native_code_family':'cw20-base','mapping_basis':'test'}]}))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/audit-vegeta-native-family-coverage.py'),
                '--corpus',str(d/'corpus.jsonl'),'--code-cache',str(d/'code.json'),'--mapping-candidates',str(d/'mapping.json'),
                '--family-map',str(d/'family.json'),'--output',str(d/'coverage.json'),'--text-output',str(d/'coverage.txt')],check=True)
            out=json.loads((d/'coverage.json').read_text())
            gas=out['gas_weighted_family_coverage']
            self.assertEqual(gas['source_state_transaction_gas_used'],1000)
            self.assertEqual(gas['fully_selected_family_state_transaction_gas_used'],100)
            self.assertAlmostEqual(gas['fully_selected_family_state_gas_coverage'],0.1)
            storage=out['storage_access_coverage']
            self.assertAlmostEqual(storage['access_record_coverage'],0.5)
            self.assertAlmostEqual(storage['state_owner_occurrence_coverage'],0.5)
            self.assertAlmostEqual(out['conflict_relevant_storage_access_coverage']['access_record_coverage'],1.0)
            self.assertEqual(out['top_unmapped_state_gas_owners'][0]['address'],unmapped)


    def test_semantic_coverage_planner_handles_complementary_blocker_clusters(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            mapped='0x'+'11'*20; blocker_b='0x'+'22'*20; blocker_c='0x'+'33'*20
            slot_a='evm/'+'11'*20+'/'+'00'*32
            slot_b='evm/'+'22'*20+'/'+'00'*32
            slot_c='evm/'+'33'*20+'/'+'00'*32
            block={'schema_version':1,'block_number':1,'block_hash':'0xabc','timestamp':1,'transactions':[
                {'tx_index':0,'tx_hash':'0x01','from':mapped,'to':mapped,'selector':'0x','gas_used':100,'reads':[],'writes':[slot_a]},
                {'tx_index':1,'tx_hash':'0x02','from':mapped,'to':blocker_b,'selector':'0x','gas_used':300,'reads':[],'writes':[slot_b]},
                {'tx_index':2,'tx_hash':'0x03','from':mapped,'to':blocker_b,'selector':'0x','gas_used':600,'reads':[slot_b],'writes':[slot_c]},
            ]}
            (d/'corpus.jsonl').write_text(json.dumps(block)+'\n')
            fam_a=hashlib.sha256(bytes.fromhex('6000')).hexdigest()
            fam_b=hashlib.sha256(bytes.fromhex('6001')).hexdigest()
            fam_c=hashlib.sha256(bytes.fromhex('6002')).hexdigest()
            (d/'code.json').write_text(json.dumps({mapped:{'code':'0x6000'},blocker_b:{'code':'0x6001'},blocker_c:{'code':'0x6002'}}))
            (d/'mapping.json').write_text(json.dumps({'resolution_records':[]}))
            (d/'family.json').write_text(json.dumps({'dataset':'vegeta-s4','native_code_families':{'cw20-base':{}},'profile_mappings':[
                {'ethereum_profile_family':fam_a,'native_code_family':'cw20-base','mapping_basis':'test'}]}))
            (d/'seeds.json').write_text(json.dumps({'dataset':'vegeta-s4','decisions':[
                {'priority':1,'address':blocker_b,'runtime_code_family':fam_b,'identity_hint':'B token','suggested_native_family':'cw20-base'}]}))
            (d/'coverage.json').write_text(json.dumps({'gas_weighted_family_coverage':{'fully_selected_family_state_gas_coverage':0.1}}))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/plan-vegeta-s4-semantic-coverage.py'),
                '--corpus',str(d/'corpus.jsonl'),'--code-cache',str(d/'code.json'),'--mapping-candidates',str(d/'mapping.json'),
                '--family-map',str(d/'family.json'),'--review-seeds',str(d/'seeds.json'),'--coverage',str(d/'coverage.json'),
                '--clusters-output',str(d/'clusters.json'),'--plan-output',str(d/'plan.json'),'--text-output',str(d/'plan.txt'),
                '--target-storage-access','0.9'],check=True)
            clusters=json.loads((d/'clusters.json').read_text())
            plan=json.loads((d/'plan.json').read_text())
            top=clusters['top_clusters'][0]
            self.assertEqual(top['gas_used'],600)
            self.assertEqual(top['blocker_count'],2)
            self.assertAlmostEqual(plan['current_state_gas_coverage'],0.1)
            self.assertAlmostEqual(plan['current_conflict_relevant_access_coverage'],0.0)
            self.assertAlmostEqual(plan['current_storage_access_coverage'],0.25)
            self.assertEqual(plan['schema_version'],4)
            self.assertEqual(plan['target_storage_access_records'],4)
            self.assertEqual(plan['target_conflict_pairs'],1)
            self.assertEqual(plan['greedy_steps'],plan['balanced_plan']['steps'])
            self.assertEqual(plan['greedy_steps'][0]['runtime_code_family'],fam_b)
            self.assertEqual(plan['greedy_steps'][0]['newly_unlocked_gas'],300)
            self.assertEqual(plan['greedy_steps'][0]['newly_covered_conflict_pairs'],1)
            self.assertAlmostEqual(plan['greedy_steps'][0]['cumulative_projected_state_gas_coverage'],0.4)
            self.assertAlmostEqual(plan['greedy_steps'][0]['cumulative_projected_conflict_relevant_access_coverage'],1.0)
            self.assertAlmostEqual(plan['greedy_steps'][0]['cumulative_projected_storage_access_coverage'],0.75)
            self.assertAlmostEqual(plan['greedy_steps'][0]['cumulative_projected_conflict_coverage'],1.0)
            self.assertEqual(plan['greedy_steps'][1]['runtime_code_family'],fam_c)
            self.assertAlmostEqual(plan['greedy_steps'][1]['cumulative_projected_storage_access_coverage'],1.0)
            self.assertEqual(len(plan['greedy_steps']),2)
            self.assertEqual(plan['strict_gas_diagnostic_steps'][0]['runtime_code_family'],fam_b)
            self.assertEqual(plan['strict_gas_diagnostic_steps'][1]['runtime_code_family'],fam_c)
            self.assertEqual(plan['strict_gas_diagnostic_steps'][1]['newly_unlocked_gas'],600)
            self.assertTrue(plan['balanced_plan']['both_targets_reached'])
            self.assertTrue(plan['target_reached_by_plan'])
            self.assertEqual(plan['greedy_steps'][0]['implementation_disposition'],'review-seed-existing-native-candidate')

    def test_semantic_coverage_planner_uses_pair_lookahead_when_it_beats_single_gain(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            mapped='0x'+'11'*20; b='0x'+'22'*20; c='0x'+'33'*20; solo='0x'+'44'*20
            slots={
                mapped:'evm/'+'11'*20+'/'+'00'*32,
                b:'evm/'+'22'*20+'/'+'00'*32,
                c:'evm/'+'33'*20+'/'+'00'*32,
                solo:'evm/'+'44'*20+'/'+'00'*32,
            }
            block={'schema_version':1,'block_number':1,'block_hash':'0xabc','timestamp':1,'transactions':[
                {'tx_index':0,'tx_hash':'0x01','from':mapped,'to':mapped,'selector':'0x','gas_used':100,'reads':[],'writes':[slots[mapped]]},
                {'tx_index':1,'tx_hash':'0x02','from':mapped,'to':solo,'selector':'0x','gas_used':400,'reads':[],'writes':[slots[solo]]},
                {'tx_index':2,'tx_hash':'0x03','from':mapped,'to':b,'selector':'0x','gas_used':1200,'reads':[slots[b]],'writes':[slots[c]]},
            ]}
            (d/'corpus.jsonl').write_text(json.dumps(block)+'\n')
            fams={}
            code={}
            for i,address in enumerate([mapped,b,c,solo]):
                bytecode=f'60{i:02x}'
                code[address]={'code':'0x'+bytecode}
                fams[address]=hashlib.sha256(bytes.fromhex(bytecode)).hexdigest()
            (d/'code.json').write_text(json.dumps(code))
            (d/'mapping.json').write_text(json.dumps({'resolution_records':[]}))
            (d/'family.json').write_text(json.dumps({'dataset':'vegeta-s4','native_code_families':{'cw20-base':{}},'profile_mappings':[
                {'ethereum_profile_family':fams[mapped],'native_code_family':'cw20-base','mapping_basis':'test'}]}))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/plan-vegeta-s4-semantic-coverage.py'),
                '--corpus',str(d/'corpus.jsonl'),'--code-cache',str(d/'code.json'),'--mapping-candidates',str(d/'mapping.json'),
                '--family-map',str(d/'family.json'),'--clusters-output',str(d/'clusters.json'),'--plan-output',str(d/'plan.json'),
                '--text-output',str(d/'plan.txt'),'--target-storage-access','0.99'],check=True)
            plan=json.loads((d/'plan.json').read_text())
            diag=plan['strict_gas_diagnostic_steps']
            self.assertEqual(diag[0]['selection_reason'],'complement-lookahead-diagnostic')
            self.assertEqual(len(diag[0]['lookahead_group']),2)
            self.assertEqual(diag[0]['newly_unlocked_gas'],0)
            self.assertEqual(diag[1]['newly_unlocked_gas'],1200)
            self.assertTrue(all(r['selection_reason']=='publication-gate-deficit-closure' for r in plan['greedy_steps']))

    def test_semantic_coverage_planner_emits_balanced_access_and_conflict_plans(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            mapped='0x'+'11'*20; access_owner='0x'+'22'*20; conflict_owner='0x'+'33'*20
            mapped_slot='evm/'+'11'*20+'/'+'00'*32
            access_slots=['evm/'+'22'*20+'/'+f'{i:064x}' for i in range(10)]
            conflict_slot='evm/'+'33'*20+'/'+'00'*32
            txs=[
                {'tx_index':0,'tx_hash':'0x00','from':mapped,'to':mapped,'selector':'0x','gas_used':10,'reads':[],'writes':[mapped_slot]},
                {'tx_index':1,'tx_hash':'0x01','from':mapped,'to':access_owner,'selector':'0x','gas_used':10,'reads':[],'writes':access_slots},
                {'tx_index':2,'tx_hash':'0x02','from':mapped,'to':access_owner,'selector':'0x','gas_used':10,'reads':[access_slots[0]],'writes':[]},
            ]
            for i in range(5):
                txs.append({'tx_index':3+i,'tx_hash':f'0x1{i}','from':mapped,'to':conflict_owner,'selector':'0x','gas_used':10,'reads':[conflict_slot] if i else [],'writes':[conflict_slot] if i==0 else []})
            block={'schema_version':1,'block_number':1,'block_hash':'0xabc','timestamp':1,'transactions':txs}
            (d/'corpus.jsonl').write_text(json.dumps(block)+'\n')
            codes={mapped:'0x6000',access_owner:'0x6001',conflict_owner:'0x6002'}
            (d/'code.json').write_text(json.dumps({a:{'code':c} for a,c in codes.items()}))
            fam={a:hashlib.sha256(bytes.fromhex(c[2:])).hexdigest() for a,c in codes.items()}
            (d/'mapping.json').write_text(json.dumps({'resolution_records':[]}))
            (d/'family.json').write_text(json.dumps({'dataset':'vegeta-s4','native_code_families':{'cw20-base':{}},'profile_mappings':[
                {'ethereum_profile_family':fam[mapped],'native_code_family':'cw20-base','mapping_basis':'test'}]}))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/plan-vegeta-s4-semantic-coverage.py'),
                '--corpus',str(d/'corpus.jsonl'),'--code-cache',str(d/'code.json'),'--mapping-candidates',str(d/'mapping.json'),
                '--family-map',str(d/'family.json'),'--clusters-output',str(d/'clusters.json'),'--plan-output',str(d/'plan.json'),
                '--text-output',str(d/'plan.txt'),'--target-storage-access','0.9','--target-conflict','0.95'],check=True)
            plan=json.loads((d/'plan.json').read_text())
            self.assertEqual(plan['access_first_plan']['steps'][0]['runtime_code_family'],fam[access_owner])
            self.assertEqual(plan['conflict_first_plan']['steps'][0]['runtime_code_family'],fam[conflict_owner])
            self.assertEqual(plan['balanced_plan']['steps'][0]['runtime_code_family'],fam[conflict_owner])
            self.assertGreaterEqual(plan['balanced_plan']['steps'][0]['normalized_remaining_conflict_deficit_closed'],0.8)
            self.assertEqual(plan['balanced_plan']['steps'][1]['runtime_code_family'],fam[access_owner])
            self.assertTrue(plan['balanced_plan']['both_targets_reached'])
            text=(d/'plan.txt').read_text()
            self.assertIn('Balanced dual-gate plan',text)
            self.assertIn('Access-first plan',text)
            self.assertIn('Conflict-first plan',text)
            self.assertIn('NOT used for family-freeze ordering',text)

    def test_semantic_coverage_planner_does_not_double_count_overlapping_conflict_pairs(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            mapped='0x'+'11'*20; a='0x'+'22'*20; b='0x'+'33'*20
            slot_m='evm/'+'11'*20+'/'+'00'*32
            slot_a='evm/'+'22'*20+'/'+'00'*32
            slot_b='evm/'+'33'*20+'/'+'00'*32
            block={'schema_version':1,'block_number':7,'block_hash':'0xabc','timestamp':1,'transactions':[
                {'tx_index':0,'tx_hash':'0x01','from':mapped,'to':a,'selector':'0x','gas_used':10,'reads':[],'writes':[slot_m,slot_a,slot_b]},
                {'tx_index':1,'tx_hash':'0x02','from':mapped,'to':b,'selector':'0x','gas_used':10,'reads':[slot_a,slot_b],'writes':[]},
            ]}
            (d/'corpus.jsonl').write_text(json.dumps(block)+'\n')
            codes={mapped:'0x6000',a:'0x6001',b:'0x6002'}
            (d/'code.json').write_text(json.dumps({x:{'code':c} for x,c in codes.items()}))
            fam={x:hashlib.sha256(bytes.fromhex(c[2:])).hexdigest() for x,c in codes.items()}
            (d/'mapping.json').write_text(json.dumps({'resolution_records':[]}))
            (d/'family.json').write_text(json.dumps({'dataset':'vegeta-s4','native_code_families':{'cw20-base':{}},'profile_mappings':[
                {'ethereum_profile_family':fam[mapped],'native_code_family':'cw20-base','mapping_basis':'test'}]}))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/plan-vegeta-s4-semantic-coverage.py'),
                '--corpus',str(d/'corpus.jsonl'),'--code-cache',str(d/'code.json'),'--mapping-candidates',str(d/'mapping.json'),
                '--family-map',str(d/'family.json'),'--clusters-output',str(d/'clusters.json'),'--plan-output',str(d/'plan.json'),
                '--text-output',str(d/'plan.txt'),'--target-storage-access','0.99','--target-conflict','0.95'],check=True)
            plan=json.loads((d/'plan.json').read_text())
            self.assertEqual(plan['total_conflict_pairs'],1)
            steps=plan['conflict_first_plan']['steps']
            self.assertEqual(steps[0]['newly_covered_conflict_pairs'],1)
            self.assertEqual(steps[1]['newly_covered_conflict_pairs'],0)
            self.assertAlmostEqual(steps[0]['cumulative_projected_conflict_coverage'],1.0)
            self.assertAlmostEqual(steps[1]['cumulative_projected_conflict_coverage'],1.0)

    def test_checked_in_first_batch_installs_six_reviewed_mappings_and_custom_router_alias(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            base=json.loads((ROOT/'evaluation/vegeta/s1-native-family-map.v2.json').read_text())
            base['dataset']='vegeta-s4'; base['candidate_only']=True; base['profile_mappings']=[]
            (d/'base.json').write_text(json.dumps(base))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-first-batch.py'),
                '--review-base',str(d/'base.json'),
                '--family-extension',str(ROOT/'evaluation/vegeta/s4-first-batch-native-family-extension.v1.json'),
                '--reviewed-decisions',str(ROOT/'evaluation/vegeta/s4-first-batch-reviewed-decisions.v1.json'),
                '--workspace-decisions',str(d/'decisions.json')],check=True)
            installed=json.loads((d/'base.json').read_text())
            self.assertIn('custom-swap-router',installed['native_code_families'])
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-review-decisions.py'),
                '--base-map',str(d/'base.json'),'--decisions',str(d/'decisions.json'),'--output',str(d/'draft.json')],check=True)
            draft=json.loads((d/'draft.json').read_text())
            first={r['ethereum_profile_family']:r['native_code_family'] for r in draft['profile_mappings']}
            self.assertEqual(len(first),6)
            self.assertEqual(first['b554904137f2259dd2578d406fe4bc4c1327c918d1340c1a8f121f7f3262da63'],'custom-swap-router')
            self.assertEqual(first['9792a60e149086e16264936114044e08b07981f87f64293339df5cd659e92a29'],'cw20-base')
            self.assertEqual(first['e4774ce981798c23daad3f54c4b86ffc2ca58500d7268e52a4d4e696bf3cd69b'],'cw721-drop')
            self.assertEqual(first['76bcd86b160a3cd8740d07c985eedc4c3b044c0fb9123c8f90f49ac17185fba8'],'fiat-token-cw20')
            self.assertEqual(first['2bac97e30a3749ba06f5ea2f1e205ad49fe9118caf7d283a1492760dd558b21b'],'cw20-base')
            self.assertEqual(first['1fc5a4ab49d1d973bcb42d8287dc59cb7579b7e76548fc8d9c94551d3dd07ee5'],'cw20-base')
            self.assertTrue(draft['candidate_only'])
            impl=json.loads((ROOT/'evaluation/vegeta/s4-native-implementation-manifest.v1.json').read_text())
            aliases=[r for r in impl['families'] if r.get('native_code_family')=='custom-swap-router']
            self.assertEqual(len(aliases),1)
            self.assertEqual(aliases[0]['package'],'acg-benchmark-native-s3-marketplace-router')

    def test_checked_in_second_batch_merges_five_safe_aliases_and_retains_pending_system_rows(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            base=json.loads((ROOT/'evaluation/vegeta/s1-native-family-map.v2.json').read_text())
            base['dataset']='vegeta-s4'; base['candidate_only']=True; base['profile_mappings']=[]
            (d/'base.json').write_text(json.dumps(base))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-first-batch.py'),
                '--review-base',str(d/'base.json'),
                '--family-extension',str(ROOT/'evaluation/vegeta/s4-first-batch-native-family-extension.v1.json'),
                '--reviewed-decisions',str(ROOT/'evaluation/vegeta/s4-first-batch-reviewed-decisions.v1.json'),
                '--workspace-decisions',str(d/'decisions.json')],check=True)
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-second-batch.py'),
                '--review-base',str(d/'base.json'),'--workspace-decisions',str(d/'decisions.json'),
                '--second-batch',str(ROOT/'evaluation/vegeta/s4-second-batch-review-candidates.v1.json'),
                '--pending-output',str(d/'pending.json')],check=True)
            decisions=json.loads((d/'decisions.json').read_text())
            reviewed=[r for r in decisions['decisions'] if r.get('review_status')=='reviewed']
            self.assertEqual(len(reviewed),11)
            pending=json.loads((d/'pending.json').read_text())
            self.assertEqual(pending['pending_count'],7)
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-review-decisions.py'),
                '--base-map',str(d/'base.json'),'--decisions',str(d/'decisions.json'),'--output',str(d/'draft.json')],check=True)
            draft=json.loads((d/'draft.json').read_text())
            mapping={r['ethereum_profile_family']:r['native_code_family'] for r in draft['profile_mappings']}
            self.assertEqual(mapping['17afef0441761ab1fe4211750b1198bbf3a4e69ed6e1d927ff4daf6407573996'],'cw721-drop')
            self.assertEqual(mapping['33caa6820fde1f8d7de68cb310e876f454e23f45cc820fc6772bff1bb6cfff28'],'cw721-drop')
            self.assertEqual(mapping['5c925cc2dcd38717e4fad3901190a5ceebda33c77b1170548ff7f5f666716b4c'],'cw20-base')
            self.assertEqual(mapping['5c813da8be193a1a33a7533edc758e3ad29f1fa1730cbf2d8c9fc8a7f31c78f3'],'cw20-base')
            self.assertEqual(mapping['0e837f348745c0a4224f1f891ad0ae81163233cd31904b543fb15d334e013f3b'],'cw721-drop')
            self.assertNotIn('9c3952a6a60eb6294bc00efde1f3cd908d3726074e3528c34fec263e0a4b5a6a',mapping)

    def test_checked_in_third_batch_promotes_pending_token_rows_and_adds_v3_pool_alias(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            base=json.loads((ROOT/'evaluation/vegeta/s1-native-family-map.v2.json').read_text())
            base['dataset']='vegeta-s4'; base['candidate_only']=True; base['profile_mappings']=[]
            # Reproduce the real S4 review-base shape: the bootstrap candidate omits
            # fee-token-cw20 even though the implementation exists in the repo/manifest.
            base['native_code_families'].pop('fee-token-cw20', None)
            (d/'base.json').write_text(json.dumps(base))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-first-batch.py'),
                '--review-base',str(d/'base.json'),
                '--family-extension',str(ROOT/'evaluation/vegeta/s4-first-batch-native-family-extension.v1.json'),
                '--reviewed-decisions',str(ROOT/'evaluation/vegeta/s4-first-batch-reviewed-decisions.v1.json'),
                '--workspace-decisions',str(d/'decisions.json')],check=True)
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-second-batch.py'),
                '--review-base',str(d/'base.json'),'--workspace-decisions',str(d/'decisions.json'),
                '--second-batch',str(ROOT/'evaluation/vegeta/s4-second-batch-review-candidates.v1.json'),
                '--pending-output',str(d/'pending2.json')],check=True)
            before=json.loads((d/'decisions.json').read_text())
            self.assertEqual(next(r for r in before['decisions'] if r['priority']==17)['review_status'],'pending')
            self.assertEqual(next(r for r in before['decisions'] if r['priority']==18)['review_status'],'pending')

            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-third-batch.py'),
                '--review-base',str(d/'base.json'),'--workspace-decisions',str(d/'decisions.json'),
                '--family-extension',str(ROOT/'evaluation/vegeta/s4-third-batch-native-family-extension.v1.json'),
                '--third-batch',str(ROOT/'evaluation/vegeta/s4-third-batch-reviewed-decisions.v1.json'),
                '--pending-output',str(d/'pending3.json')],check=True)
            installed=json.loads((d/'base.json').read_text())
            self.assertIn('v3-pool-lock',installed['native_code_families'])
            self.assertIn('fee-token-cw20',installed['native_code_families'])
            decisions=json.loads((d/'decisions.json').read_text())
            reviewed=[r for r in decisions['decisions'] if r.get('review_status')=='reviewed']
            self.assertEqual(len(reviewed),21)
            self.assertEqual(next(r for r in decisions['decisions'] if r['priority']==17)['review_status'],'reviewed')
            self.assertEqual(next(r for r in decisions['decisions'] if r['priority']==18)['review_status'],'reviewed')
            pending=json.loads((d/'pending3.json').read_text())
            self.assertEqual(pending['pending_count'],5)

            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-review-decisions.py'),
                '--base-map',str(d/'base.json'),'--decisions',str(d/'decisions.json'),'--output',str(d/'draft.json')],check=True)
            draft=json.loads((d/'draft.json').read_text())
            mapping={r['ethereum_profile_family']:r['native_code_family'] for r in draft['profile_mappings']}
            self.assertEqual(mapping['39fc30b247bd5f4ffd99adffc0b8cb1d9775248075475088b054243f08a4a7b4'],'v3-pool-lock')
            self.assertEqual(mapping['5a4315b476318917253bb08310959df1de83fc6f1d0ffff9950fab1bbf5570b6'],'cw20-base')
            self.assertEqual(mapping['22b5c7e25fc0335550c67fd158ed9739d06aa126ee50126d5d6d1103f0a5c04e'],'cw20-base')
            self.assertEqual(mapping['ba7e918a2288ec52d2024fba5054224d962033ea64b880555cea18c0b2a3a562'],'fee-token-cw20')
            self.assertNotIn('9c3952a6a60eb6294bc00efde1f3cd908d3726074e3528c34fec263e0a4b5a6a',mapping)
            self.assertNotIn('e518057ee9772b6d5ad104f0c0cbae96d42e5543dcaaff3b39257089ab5fe699',mapping)

            impl=json.loads((ROOT/'evaluation/vegeta/s4-native-implementation-manifest.v1.json').read_text())
            aliases=[r for r in impl['families'] if r.get('native_code_family')=='v3-pool-lock']
            self.assertEqual(len(aliases),1)
            self.assertEqual(aliases[0]['package'],'acg-benchmark-native-s3-marketplace-router')
            planner=(ROOT/'tools/vegeta/build-native-s3-plan.py').read_text()
            self.assertIn('"v3-pool-lock"',planner)
            self.assertIn('"0x128acb08": ("execute::execute_route", [])',planner)
            execution=(ROOT/'tools/vegeta/prepare-native-s3-execution.py').read_text()
            self.assertIn("'v3-pool-lock'",execution)

    def test_checked_in_fourth_batch_promotes_p12_p16_with_evidence_and_aliases(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            base=json.loads((ROOT/'evaluation/vegeta/s1-native-family-map.v2.json').read_text())
            base['dataset']='vegeta-s4'; base['candidate_only']=True; base['profile_mappings']=[]
            base['native_code_families'].pop('fee-token-cw20', None)
            (d/'base.json').write_text(json.dumps(base))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-first-batch.py'),
                '--review-base',str(d/'base.json'),
                '--family-extension',str(ROOT/'evaluation/vegeta/s4-first-batch-native-family-extension.v1.json'),
                '--reviewed-decisions',str(ROOT/'evaluation/vegeta/s4-first-batch-reviewed-decisions.v1.json'),
                '--workspace-decisions',str(d/'decisions.json')],check=True)
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-second-batch.py'),
                '--review-base',str(d/'base.json'),'--workspace-decisions',str(d/'decisions.json'),
                '--second-batch',str(ROOT/'evaluation/vegeta/s4-second-batch-review-candidates.v1.json'),
                '--pending-output',str(d/'pending2.json')],check=True)
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-third-batch.py'),
                '--review-base',str(d/'base.json'),'--workspace-decisions',str(d/'decisions.json'),
                '--family-extension',str(ROOT/'evaluation/vegeta/s4-third-batch-native-family-extension.v1.json'),
                '--third-batch',str(ROOT/'evaluation/vegeta/s4-third-batch-reviewed-decisions.v1.json'),
                '--pending-output',str(d/'pending3.json')],check=True)
            before=json.loads((d/'pending3.json').read_text())
            self.assertEqual(before['pending_count'],5)

            fourth=[sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-fourth-batch.py'),
                '--review-base',str(d/'base.json'),'--workspace-decisions',str(d/'decisions.json'),
                '--family-extension',str(ROOT/'evaluation/vegeta/s4-fourth-batch-native-family-extension.v1.json'),
                '--fourth-batch',str(ROOT/'evaluation/vegeta/s4-fourth-batch-reviewed-decisions.v1.json'),
                '--review-evidence',str(ROOT/'evaluation/vegeta/s4-fourth-batch-review-evidence.v1.json'),
                '--pending-output',str(d/'pending4.json')]
            subprocess.run(fourth,check=True)
            # Exact re-run is idempotent: the evidence-backed decisions/native definitions are identical.
            subprocess.run(fourth,check=True)

            installed=json.loads((d/'base.json').read_text())
            for family in ('amp-partition-collateral-lock','linea-rollup-lock','zksync-l1-system-lock','arbitrum-bridge-lock'):
                self.assertIn(family,installed['native_code_families'])
            decisions=json.loads((d/'decisions.json').read_text())
            reviewed=[r for r in decisions['decisions'] if r.get('review_status')=='reviewed']
            self.assertEqual(len(reviewed),26)
            self.assertEqual(json.loads((d/'pending4.json').read_text())['pending_count'],0)

            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-review-decisions.py'),
                '--base-map',str(d/'base.json'),'--decisions',str(d/'decisions.json'),'--output',str(d/'draft.json')],check=True)
            draft=json.loads((d/'draft.json').read_text())
            mapping={r['ethereum_profile_family']:r['native_code_family'] for r in draft['profile_mappings']}
            self.assertEqual(mapping['9c3952a6a60eb6294bc00efde1f3cd908d3726074e3528c34fec263e0a4b5a6a'],'amp-partition-collateral-lock')
            self.assertEqual(mapping['e518057ee9772b6d5ad104f0c0cbae96d42e5543dcaaff3b39257089ab5fe699'],'linea-rollup-lock')
            self.assertEqual(mapping['b7d07cf451c70118754002caa25c35e6371e3c4e14fee774d0621bb718c32cdd'],'zksync-l1-system-lock')
            self.assertEqual(mapping['c2c49b1400159c76a8b41064333aa784bc108a11a5700d1f49a2c4fdc4a6fd78'],'arbitrum-bridge-lock')
            self.assertEqual(mapping['18c16c15c30c2696c7b126c4f4f0964e02d9020c9a09fad2661e218007530e9e'],'cw721-drop')

            evidence=json.loads((ROOT/'evaluation/vegeta/s4-fourth-batch-review-evidence.v1.json').read_text())
            self.assertEqual(len(evidence['records']),5)
            self.assertTrue(all(r.get('review_conclusion') and r.get('sources') for r in evidence['records']))
            impl=json.loads((ROOT/'evaluation/vegeta/s4-native-implementation-manifest.v1.json').read_text())
            impl_names={r.get('native_code_family') for r in impl['families']}
            for family in ('amp-partition-collateral-lock','linea-rollup-lock','zksync-l1-system-lock','arbitrum-bridge-lock'):
                self.assertIn(family,impl_names)
            planner=(ROOT/'tools/vegeta/build-native-s3-plan.py').read_text()
            self.assertIn('"0x4165d6dd": ("execute::execute_route", [])',planner)
            self.assertIn('"0xeb672419": ("execute::execute_route", [])',planner)
            execution=(ROOT/'tools/vegeta/prepare-native-s3-execution.py').read_text()
            self.assertIn("'linea-rollup-lock'",execution)
            self.assertIn("'zksync-l1-system-lock'",execution)

    def test_s4_batch_delta_uses_exact_integer_dual_gate_targets(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            before={
                'source_conflict_coverage':{'total_unique_conflict_pairs':101,'selected_family_unique_conflict_pairs':90,'coverage':90/101},
                'storage_access_coverage':{'total_access_records':101,'selected_family_access_records':40,'access_record_coverage':40/101},
            }
            after={
                'source_conflict_coverage':{'total_unique_conflict_pairs':101,'selected_family_unique_conflict_pairs':96,'coverage':96/101},
                'storage_access_coverage':{'total_access_records':101,'selected_family_access_records':50,'access_record_coverage':50/101},
            }
            (d/'before.json').write_text(json.dumps(before)); (d/'after.json').write_text(json.dumps(after))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/summarize-vegeta-s4-batch-delta.py'),
                '--before',str(d/'before.json'),'--after',str(d/'after.json'),
                '--output',str(d/'delta.json'),'--text-output',str(d/'delta.txt')],check=True)
            out=json.loads((d/'delta.json').read_text())
            self.assertEqual(out['targets']['conflict_pairs'],96)  # ceil(.95 * 101)
            self.assertEqual(out['targets']['storage_access_records'],91)  # ceil(.90 * 101)
            self.assertEqual(out['delta']['newly_mapped_conflict_pairs'],6)
            self.assertEqual(out['delta']['newly_mapped_storage_accesses'],10)
            self.assertTrue(out['publication_gates']['conflict'])
            self.assertFalse(out['publication_gates']['storage_access'])
            self.assertEqual(out['remaining']['storage_access_records'],41)

    def test_conflict_closure_report_tracks_dual_freeze_policy(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            coverage={
                'source_conflict_coverage':{'total_unique_conflict_pairs':100,'selected_family_unique_conflict_pairs':96,'coverage':0.96},
                'block_balanced_conflict_coverage':{'median_coverage':0.85},
                'conflict_relevant_storage_access_coverage':{'access_record_coverage':0.20},
                'storage_access_coverage':{'total_access_records':100,'selected_family_access_records':10,'access_record_coverage':0.10,'state_owner_occurrence_coverage':0.11},
                'gas_weighted_family_coverage':{'fully_selected_family_state_gas_coverage':0.05,'fully_selected_family_state_transaction_coverage':0.06},
                'top_unmapped_conflict_owners':[{'address':'0x'+'11'*20,'owner_pair_attributions':3,'access_records':7,'gas_attributions':9}],
            }
            readiness={'ready_to_freeze_family_map':False}
            decisions={'dataset':'vegeta-s4','decisions':[{'review_status':'reviewed','runtime_code_family':'aa'},{'review_status':'pending','runtime_code_family':'bb'}]}
            for name,obj in [('coverage',coverage),('ready',readiness),('decisions',decisions)]: (d/f'{name}.json').write_text(json.dumps(obj))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/summarize-vegeta-s4-conflict-closure.py'),
                '--coverage',str(d/'coverage.json'),'--readiness',str(d/'ready.json'),'--decisions',str(d/'decisions.json'),
                '--output',str(d/'out.json'),'--text-output',str(d/'out.txt')],check=True)
            out=json.loads((d/'out.json').read_text())
            self.assertFalse(out['ready_to_freeze_family_map'])
            self.assertEqual(out['conflict']['remaining_pairs_to_target'],0)
            self.assertEqual(out['storage_access']['remaining_records_to_target'],80)
            self.assertAlmostEqual(out['diagnostics']['conflict_relevant_storage_access_coverage'],0.20)
            self.assertIn('reported, not family-freeze gates',(d/'out.txt').read_text())

    def test_review_decisions_remain_candidate_until_gated_freeze(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            family='aa'*32
            base={'dataset':'vegeta-s4','candidate_only':True,'native_code_families':{'cw20-base':{}},'profile_mappings':[]}
            decisions={'dataset':'vegeta-s4','decisions':[
                {'priority':1,'address':'0x'+'11'*20,'runtime_code_family':family,'identity_hint':'token','review_status':'reviewed','reviewed_native_family':'cw20-base','mapping_basis':'reviewed test ABI','semantic_notes':'transfer'},
                {'priority':2,'address':'0x'+'22'*20,'runtime_code_family':'bb'*32,'review_status':'pending'},
            ]}
            (d/'base.json').write_text(json.dumps(base)); (d/'decisions.json').write_text(json.dumps(decisions))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-review-decisions.py'),
                '--base-map',str(d/'base.json'),'--decisions',str(d/'decisions.json'),'--output',str(d/'draft.json')],check=True)
            draft=json.loads((d/'draft.json').read_text())
            self.assertTrue(draft['candidate_only'])
            self.assertEqual(len(draft['profile_mappings']),1)
            self.assertEqual(draft['s4_pending_seed_runtime_families'],['bb'*32])

            coverage={'source_conflict_coverage':{'coverage':0.96},'block_balanced_conflict_coverage':{'median_coverage':0.85},
                'conflict_relevant_storage_access_coverage':{'access_record_coverage':0.91},
                'storage_access_coverage':{'access_record_coverage':0.50,'state_owner_occurrence_coverage':0.82},
                'gas_weighted_family_coverage':{'fully_selected_family_state_gas_coverage':0.10,'fully_selected_family_state_transaction_coverage':0.08}}
            provenance={'internal_integrity':{'pass':True}}
            (d/'coverage.json').write_text(json.dumps(coverage)); (d/'provenance.json').write_text(json.dumps(provenance))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/check-vegeta-s4-family-review-readiness.py'),
                '--family-map',str(d/'draft.json'),'--coverage',str(d/'coverage.json'),'--provenance',str(d/'provenance.json'),
                '--output',str(d/'gate.json'),'--text-output',str(d/'gate.txt'),'--allow-low'],check=True)
            gate=json.loads((d/'gate.json').read_text())
            self.assertFalse(gate['ready_to_freeze_family_map'])
            self.assertTrue(gate['gates']['conflict_coverage'])
            self.assertFalse(gate['gates']['storage_access_coverage'])

            coverage['storage_access_coverage']['access_record_coverage']=0.91
            (d/'coverage.json').write_text(json.dumps(coverage))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/check-vegeta-s4-family-review-readiness.py'),
                '--family-map',str(d/'draft.json'),'--coverage',str(d/'coverage.json'),'--provenance',str(d/'provenance.json'),
                '--output',str(d/'gate.json'),'--text-output',str(d/'gate.txt')],check=True)
            self.assertTrue(json.loads((d/'gate.json').read_text())['ready_to_freeze_family_map'])
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/freeze-vegeta-s4-family-map.py'),
                '--draft-map',str(d/'draft.json'),'--freeze-readiness',str(d/'gate.json'),'--coverage',str(d/'coverage.json'),
                '--provenance',str(d/'provenance.json'),'--output',str(d/'frozen.json'),'--reviewed'],check=True)
            frozen=json.loads((d/'frozen.json').read_text())
            self.assertFalse(frozen['candidate_only'])
            self.assertIn('freeze_evidence',frozen)

    def test_final_s4_readiness_gates_all_storage_but_keeps_gas_diagnostic(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            fmap={'dataset':'vegeta-s4','candidate_only':False,'freeze_evidence':{'x':'y'}}
            family={'source_conflict_coverage':{'coverage':0.96},'block_balanced_conflict_coverage':{'median_coverage':0.85},
                'conflict_relevant_storage_access_coverage':{'access_record_coverage':0.95},
                'storage_access_coverage':{'access_record_coverage':0.95,'state_owner_occurrence_coverage':0.90},
                'gas_weighted_family_coverage':{'fully_selected_family_state_gas_coverage':0.10}}
            trans={'implementation_readiness':{'native_execution_ready':True}}
            sem={'coverage':0.96,'block_balanced':{'median_coverage':0.85}}
            deficit={'denominators':{
                'all_source_transactions':{'successful_reviewed_state_coverage':0.9},
                'source_storage_access_transactions':{'successful_reviewed_state_gas_coverage':0.50},
                'source_conflict_participating_transactions':{'successful_reviewed_state_coverage':0.9}}}
            for name,obj in [('map',fmap),('family',family),('trans',trans),('sem',sem),('deficit',deficit)]: (d/f'{name}.json').write_text(json.dumps(obj))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/validate-vegeta-s4-readiness.py'),
                '--family-map',str(d/'map.json'),'--family-coverage',str(d/'family.json'),'--translation-coverage',str(d/'trans.json'),
                '--semantic-coverage',str(d/'sem.json'),'--transaction-deficit',str(d/'deficit.json'),
                '--output',str(d/'ready.json'),'--text-output',str(d/'ready.txt')],check=True)
            out=json.loads((d/'ready.json').read_text())
            self.assertTrue(out['ready'])
            self.assertNotIn('semantic_state_gas',out['gates'])
            self.assertNotIn('family_state_gas',out['gates'])
            self.assertNotIn('family_conflict_relevant_access',out['gates'])
            self.assertTrue(out['gates']['family_storage_access'])
            self.assertAlmostEqual(out['metrics']['family_conflict_relevant_access'],0.95)
            self.assertAlmostEqual(out['metrics']['family_storage_access'],0.95)
            self.assertAlmostEqual(out['metrics']['family_fully_mapped_state_gas_diagnostic'],0.10)
            self.assertAlmostEqual(out['metrics']['semantic_state_gas_diagnostic'],0.50)

    def test_final_s4_readiness_fails_when_conflict_passes_but_all_storage_is_low(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            fmap={'dataset':'vegeta-s4','candidate_only':False,'freeze_evidence':{'x':'y'}}
            family={'source_conflict_coverage':{'coverage':0.96},'block_balanced_conflict_coverage':{'median_coverage':0.85},
                'conflict_relevant_storage_access_coverage':{'access_record_coverage':0.95},
                'storage_access_coverage':{'access_record_coverage':0.50},
                'gas_weighted_family_coverage':{'fully_selected_family_state_gas_coverage':0.10}}
            trans={'implementation_readiness':{'native_execution_ready':True}}
            sem={'coverage':0.96,'block_balanced':{'median_coverage':0.85}}
            deficit={'denominators':{
                'all_source_transactions':{'successful_reviewed_state_coverage':0.9},
                'source_storage_access_transactions':{'successful_reviewed_state_gas_coverage':0.50},
                'source_conflict_participating_transactions':{'successful_reviewed_state_coverage':0.9}}}
            for name,obj in [('map',fmap),('family',family),('trans',trans),('sem',sem),('deficit',deficit)]: (d/f'{name}.json').write_text(json.dumps(obj))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/validate-vegeta-s4-readiness.py'),
                '--family-map',str(d/'map.json'),'--family-coverage',str(d/'family.json'),'--translation-coverage',str(d/'trans.json'),
                '--semantic-coverage',str(d/'sem.json'),'--transaction-deficit',str(d/'deficit.json'),
                '--output',str(d/'ready.json'),'--text-output',str(d/'ready.txt'),'--allow-low'],check=True)
            out=json.loads((d/'ready.json').read_text())
            self.assertFalse(out['ready'])
            self.assertTrue(out['gates']['family_conflict'])
            self.assertFalse(out['gates']['family_storage_access'])

    def test_final_s4_readiness_does_not_fail_low_conflict_relevant_access_diagnostic(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            fmap={'dataset':'vegeta-s4','candidate_only':False,'freeze_evidence':{'x':'y'}}
            family={'source_conflict_coverage':{'coverage':0.96},'block_balanced_conflict_coverage':{'median_coverage':0.85},
                'conflict_relevant_storage_access_coverage':{'access_record_coverage':0.10},
                'storage_access_coverage':{'access_record_coverage':0.95},
                'gas_weighted_family_coverage':{'fully_selected_family_state_gas_coverage':0.05}}
            trans={'implementation_readiness':{'native_execution_ready':True}}
            sem={'coverage':0.96,'block_balanced':{'median_coverage':0.85}}
            deficit={'denominators':{
                'all_source_transactions':{'successful_reviewed_state_coverage':0.9},
                'source_storage_access_transactions':{'successful_reviewed_state_gas_coverage':0.05},
                'source_conflict_participating_transactions':{'successful_reviewed_state_coverage':0.9}}}
            for name,obj in [('map',fmap),('family',family),('trans',trans),('sem',sem),('deficit',deficit)]: (d/f'{name}.json').write_text(json.dumps(obj))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/validate-vegeta-s4-readiness.py'),
                '--family-map',str(d/'map.json'),'--family-coverage',str(d/'family.json'),'--translation-coverage',str(d/'trans.json'),
                '--semantic-coverage',str(d/'sem.json'),'--transaction-deficit',str(d/'deficit.json'),
                '--output',str(d/'ready.json'),'--text-output',str(d/'ready.txt')],check=True)
            out=json.loads((d/'ready.json').read_text())
            self.assertTrue(out['ready'])
            self.assertNotIn('family_conflict_relevant_access',out['gates'])
            self.assertTrue(out['gates']['family_storage_access'])
            self.assertAlmostEqual(out['metrics']['family_conflict_relevant_access'],0.10)
            self.assertAlmostEqual(out['metrics']['family_storage_access'],0.95)

    def test_fifth_batch_scaffold_selects_minimum_conflict_prefix_and_keeps_rows_pending(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            fam_a='a'*64; fam_b='b'*64; fam_c='c'*64
            plan={
                'dataset':'vegeta-s4','current_storage_access_coverage':0.42,'current_conflict_coverage':0.9272,
                'target_storage_access_coverage':0.90,'target_conflict_coverage':0.95,
                'total_conflict_pairs':100000,'current_covered_conflict_pairs':92720,'target_conflict_pairs':95000,
                'remaining_storage_access_deficit_records':4300000,'remaining_conflict_deficit_pairs':2280,
                'conflict_first_plan':{'steps':[
                    {'runtime_code_family':fam_a,'blocker_id':'family:'+fam_a,'owners':['0x'+'11'*20],
                     'newly_covered_storage_access_records':100,'newly_covered_conflict_pairs':700,
                     'remaining_storage_access_deficit_records':4299900,'remaining_conflict_deficit_pairs':1580,
                     'cumulative_projected_storage_access_coverage':0.43,'cumulative_projected_conflict_coverage':0.9342},
                    {'runtime_code_family':fam_b,'blocker_id':'family:'+fam_b,'owners':['0x'+'22'*20],
                     'newly_covered_storage_access_records':200,'newly_covered_conflict_pairs':3500,
                     'remaining_storage_access_deficit_records':4299700,'remaining_conflict_deficit_pairs':0,
                     'cumulative_projected_storage_access_coverage':0.44,'cumulative_projected_conflict_coverage':0.9692},
                    {'runtime_code_family':fam_c,'blocker_id':'family:'+fam_c,'owners':['0x'+'33'*20],
                     'newly_covered_storage_access_records':300,'newly_covered_conflict_pairs':1,
                     'remaining_storage_access_deficit_records':4299400,'remaining_conflict_deficit_pairs':0,
                     'cumulative_projected_storage_access_coverage':0.45,'cumulative_projected_conflict_coverage':0.952},
                ]},
                'access_first_plan':{'steps':[
                    {'runtime_code_family':fam_c,'blocker_id':'family:'+fam_c,'owners':['0x'+'33'*20],
                     'newly_covered_storage_access_records':900,'newly_covered_conflict_pairs':1,
                     'remaining_storage_access_deficit_records':4299100,'remaining_conflict_deficit_pairs':9999,
                     'cumulative_projected_storage_access_coverage':0.50,'cumulative_projected_conflict_coverage':0.928},
                    {'runtime_code_family':fam_a,'blocker_id':'family:'+fam_a,'owners':['0x'+'11'*20],
                     'newly_covered_storage_access_records':100,'newly_covered_conflict_pairs':700,
                     'remaining_storage_access_deficit_records':4299000,'remaining_conflict_deficit_pairs':1580,
                     'cumulative_projected_storage_access_coverage':0.51,'cumulative_projected_conflict_coverage':0.943},
                ]},
            }
            (d/'plan.json').write_text(json.dumps(plan))
            (d/'queue.json').write_text(json.dumps({'dataset':'vegeta-s4','review_queue':[
                {'runtime_code_family':fam_a,'address':'0x'+'11'*20,'direct_selectors':[{'selector':'0x12345678','count':3}]}
            ]}))
            (d/'workspace.json').write_text(json.dumps({'dataset':'vegeta-s4','decisions':[]}))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/build-vegeta-s4-fifth-batch-review-scaffold.py'),
                '--plan',str(d/'plan.json'),'--review-queue',str(d/'queue.json'),'--workspace-decisions',str(d/'workspace.json'),
                '--output',str(d/'out.json'),'--text-output',str(d/'out.txt'),'--decisions-draft',str(d/'decisions.json'),
                '--access-tail-top','5'],check=True)
            out=json.loads((d/'out.json').read_text()); decisions=json.loads((d/'decisions.json').read_text())
            self.assertEqual(out['projected_conflict_closure_review_count'],2)
            self.assertEqual([r['runtime_code_family'] for r in out['projected_conflict_closure_shortlist']],[fam_a,fam_b])
            self.assertEqual([r['runtime_code_family'] for r in out['access_heavy_followup']],[fam_c])
            self.assertEqual(len(decisions['decisions']),2)
            self.assertTrue(all(r['review_status']=='pending' for r in decisions['decisions']))
            self.assertTrue(all(r['reviewed_native_family'] is None for r in decisions['decisions']))
            self.assertEqual(decisions['decisions'][0]['evidence_sources'],[])

    def test_fifth_batch_scaffold_skips_explicitly_unresolved_and_preserves_existing_draft(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td)
            unresolved='d'*64; candidate='a'*64
            plan={
                'dataset':'vegeta-s4','current_storage_access_coverage':0.4433,'current_conflict_coverage':444548/468252,
                'target_storage_access_coverage':0.90,'target_conflict_coverage':0.95,
                'total_conflict_pairs':468252,'current_covered_conflict_pairs':444548,'target_conflict_pairs':444840,
                'remaining_storage_access_deficit_records':4168364,'remaining_conflict_deficit_pairs':292,
                'conflict_first_plan':{'steps':[
                    {'runtime_code_family':unresolved,'blocker_id':'family:'+unresolved,'owners':['0x'+'11'*20],
                     'newly_covered_storage_access_records':2128,'newly_covered_conflict_pairs':465,
                     'remaining_storage_access_deficit_records':4166236,'remaining_conflict_deficit_pairs':0,
                     'cumulative_projected_storage_access_coverage':0.4436,'cumulative_projected_conflict_coverage':0.9504},
                    {'runtime_code_family':candidate,'blocker_id':'family:'+candidate,'owners':['0x'+'22'*20],
                     'newly_covered_storage_access_records':61179,'newly_covered_conflict_pairs':414,
                     'remaining_storage_access_deficit_records':4105057,'remaining_conflict_deficit_pairs':0,
                     'cumulative_projected_storage_access_coverage':0.4503,'cumulative_projected_conflict_coverage':0.9513},
                ]},
                'access_first_plan':{'steps':[
                    {'runtime_code_family':candidate,'blocker_id':'family:'+candidate,'owners':['0x'+'22'*20],
                     'newly_covered_storage_access_records':61179,'newly_covered_conflict_pairs':414,
                     'remaining_storage_access_deficit_records':4107185,'remaining_conflict_deficit_pairs':0,
                     'cumulative_projected_storage_access_coverage':0.4500,'cumulative_projected_conflict_coverage':0.9503},
                ]},
            }
            (d/'plan.json').write_text(json.dumps(plan))
            (d/'queue.json').write_text(json.dumps({'dataset':'vegeta-s4','review_queue':[]}))
            (d/'workspace.json').write_text(json.dumps({'dataset':'vegeta-s4','decisions':[
                {'runtime_code_family':unresolved,'review_status':'unresolved'}
            ]}))
            sentinel={'dataset':'vegeta-s4','decisions':[{'sentinel':True}]}
            (d/'decisions.json').write_text(json.dumps(sentinel))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/build-vegeta-s4-fifth-batch-review-scaffold.py'),
                '--plan',str(d/'plan.json'),'--review-queue',str(d/'queue.json'),'--workspace-decisions',str(d/'workspace.json'),
                '--output',str(d/'out.json'),'--text-output',str(d/'out.txt'),'--decisions-draft',str(d/'decisions.json')],check=True)
            out=json.loads((d/'out.json').read_text())
            self.assertEqual(out['explicitly_non_executable_families_skipped'],[unresolved])
            self.assertEqual(out['projected_conflict_closure_review_count'],1)
            self.assertEqual(out['projected_conflict_closure_shortlist'][0]['runtime_code_family'],candidate)
            self.assertEqual(out['projected_conflict_closure_shortlist'][0]['projected_remaining_conflict_deficit_pairs'],0)
            self.assertGreaterEqual(out['projected_conflict_closure_shortlist'][0]['projected_cumulative_conflict_coverage'],0.95)
            self.assertEqual(json.loads((d/'decisions.json').read_text()),sentinel)

    def test_fifth_batch_evidence_dossier_recovers_exact_selectors_delegate_target_and_cluster_context(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td); calls=d/'call-cache'; calls.mkdir()
            owner='0x'+'11'*20; impl='0x'+'22'*20; other='0x'+'33'*20
            proxy_runtime='363d3d373d3d3d363d73'+impl[2:]+'5af43d82803e903d91602b57fd5bf3'
            owner_family=hashlib.sha256(bytes.fromhex(proxy_runtime)).hexdigest()
            impl_family=hashlib.sha256(bytes.fromhex('6001')).hexdigest()
            slot='evm/'+owner[2:]+'/'+'00'*32
            block={'schema_version':1,'block_number':10,'block_hash':'0xabc','timestamp':1,'transactions':[
                {'tx_index':0,'tx_hash':'0x01','from':other,'to':owner,'selector':'0xa9059cbb','input':'0xa9059cbb','gas_used':123,
                 'reads':[slot],'writes':[slot]},
            ]}
            (d/'corpus.jsonl').write_text(json.dumps(block)+'\n')
            (calls/'10.json').write_text(json.dumps({'schema_version':1,'block_number':10,'transactions':[
                {'tx_hash':'0x01','result':{'type':'CALL','to':owner,'input':'0xa9059cbb','calls':[
                    {'type':'DELEGATECALL','to':impl,'input':'0x70a08231','calls':[]}
                ]}}
            ]}))
            (d/'code.json').write_text(json.dumps({
                owner:{'block_number':10,'code':'0x'+proxy_runtime},
                impl:{'block_number':10,'code':'0x6001'},
                other:{'block_number':10,'code':'0x'},
            }))
            candidates={'schema_version':1,'dataset':'vegeta-s4','current_storage_access_coverage':0.42,'current_conflict_coverage':0.9272,
                'remaining_storage_access_deficit_records':100,'remaining_conflict_deficit_pairs':10,
                'projected_conflict_closure_shortlist':[{
                    'runtime_code_family':owner_family,'owner_addresses':[owner],
                    'projected_new_storage_access_records':2,'projected_new_conflict_pairs':1,
                    'projected_cumulative_conflict_coverage':0.95,
                }]}
            (d/'candidates.json').write_text(json.dumps(candidates))
            family_summary={'dataset':'vegeta-s4','runtime_families':[{
                'runtime_code_family':owner_family,'storage_access_records':2,'conflict_owner_pair_attributions':1,
                'top_addresses':[{'address':owner,'runtime_code_family':owner_family,'storage_access_records':2}]
            }]}
            (d/'family-summary.json').write_text(json.dumps(family_summary))
            (d/'selector-summary.json').write_text(json.dumps({'dataset':'vegeta-s4','direct_destination_selectors':[],'call_frame_selectors':[]}))
            (d/'clusters.json').write_text(json.dumps({'dataset':'vegeta-s4','top_clusters':[{
                'blocker_ids':['family:'+owner_family,'family:'+'f'*64],'transactions':3,'gas_used':999,'unmapped_access_records':7
            }]}))
            (d/'coverage.json').write_text(json.dumps({'dataset':'vegeta-s4','top_unmapped_conflict_owners':[{
                'address':owner,'owner_pair_attributions':1,'access_records':2,'gas_attributions':123
            }],'top_unmapped_state_gas_owners':[]}))
            (d/'mapping.json').write_text(json.dumps({'resolution_records':[],'ambiguous_records':[]}))
            (d/'family-map.json').write_text(json.dumps({'dataset':'vegeta-s4','native_code_families':{'cw20-base':{}},'profile_mappings':[]}))
            (d/'workspace.json').write_text(json.dumps({'dataset':'vegeta-s4','decisions':[]}))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/build-vegeta-s4-fifth-batch-evidence.py'),
                '--candidates',str(d/'candidates.json'),'--corpus',str(d/'corpus.jsonl'),'--call-cache',str(calls),
                '--code-cache',str(d/'code.json'),'--family-summary',str(d/'family-summary.json'),'--selector-summary',str(d/'selector-summary.json'),
                '--clusters',str(d/'clusters.json'),'--coverage',str(d/'coverage.json'),'--mapping-candidates',str(d/'mapping.json'),
                '--family-map',str(d/'family-map.json'),'--workspace-decisions',str(d/'workspace.json'),
                '--output',str(d/'evidence.json'),'--text-output',str(d/'evidence.md')],check=True)
            out=json.loads((d/'evidence.json').read_text()); row=out['families'][0]
            self.assertTrue(out['call_cache_complete'])
            self.assertEqual(row['runtime_code_family'],owner_family)
            self.assertEqual(row['all_observed_storage_owners'],[owner])
            self.assertEqual(row['direct_selectors_exact'][0],{'value':'0xa9059cbb','count':1})
            self.assertEqual(row['call_frame_selectors_exact'][0],{'value':'0xa9059cbb','count':1})
            self.assertEqual(row['delegate_targets_exact'][0]['target_address'],impl)
            self.assertEqual(row['delegate_targets_exact'][0]['target_runtime_code_family'],impl_family)
            self.assertEqual(row['delegate_targets_exact'][0]['selector'],'0x70a08231')
            self.assertEqual(row['owner_evidence'][0]['canonical_eip1167_implementation'],impl)
            self.assertEqual(row['top_co_blockers'][0]['value'],'family:'+'f'*64)
            self.assertEqual(row['representative_transactions'][0]['tx_hash'],'0x01')
            self.assertEqual(row['storage_activity']['reads'],1)
            self.assertEqual(row['storage_activity']['writes'],1)

    def test_checked_in_fifth_batch_reviews_fifteen_keeps_one_unresolved_and_avoids_proxy_runtime_aliases(self):
        batch=json.loads((ROOT/'evaluation/vegeta/s4-fifth-batch-reviewed-decisions.v1.json').read_text())
        evidence=json.loads((ROOT/'evaluation/vegeta/s4-fifth-batch-review-evidence.v1.json').read_text())
        ext=json.loads((ROOT/'evaluation/vegeta/s4-fifth-batch-native-family-extension.v1.json').read_text())
        self.assertEqual(len(batch['decisions']),16)
        reviewed=[r for r in batch['decisions'] if r['review_status']=='reviewed']
        unresolved=[r for r in batch['decisions'] if r['review_status']!='reviewed']
        self.assertEqual(len(reviewed),15)
        self.assertEqual([r['runtime_code_family'] for r in unresolved],['d124079c81bccc739fb75764c300034a3ea8da46a1fea0b8a497e32e381cfdcf'])
        self.assertFalse(unresolved[0].get('reviewed_native_family'))
        by_priority={r['priority']:r for r in batch['decisions']}
        self.assertEqual(by_priority[29]['blocker_runtime_code_family'],'7421a58ad9ceb68c43df758b1d32821ba7d36b489789a778cb99936a3e1893ea')
        self.assertEqual(by_priority[29]['runtime_code_family'],'3b52a3f0be0f75537b6dadc9378a8db02e6eedee15b36f7374f510de285a3fed')
        self.assertEqual(by_priority[29]['reviewed_native_family'],'wrapped-native-token')
        self.assertEqual(by_priority[32]['blocker_runtime_code_family'],'dbc6bd2a5b33cdae1c0d0bca27ba9bde66832cca035e1f592c821e6d82179fb9')
        self.assertEqual(by_priority[32]['runtime_code_family'],'044d6332da1b79172c779e718e485de3fba43e367eb494fa1b456678ac0e6eec')
        self.assertEqual(by_priority[32]['reviewed_native_family'],'starknet-l1-system-lock')
        ids={r['evidence_id'] for r in evidence['records']}
        self.assertEqual(ids,{r['evidence_id'] for r in batch['decisions']})
        self.assertEqual(set(ext['native_code_families']),{'synthetix-system-lock','starknet-l1-system-lock'})
        manifest=json.loads((ROOT/'evaluation/vegeta/s4-native-implementation-manifest.v1.json').read_text())
        manifest_names={r['native_code_family'] for r in manifest['families']}
        self.assertTrue({'synthetix-system-lock','starknet-l1-system-lock'} <= manifest_names)

    def test_fifth_batch_apply_accepts_reviewed_subset_keeps_unresolved_and_validates_generated_evidence(self):
        with tempfile.TemporaryDirectory() as td:
            d=Path(td); blocker='d'*64; impl='e'*64; unresolved='f'*64; owner='0x'+'44'*20; unresolved_owner='0x'+'55'*20
            (d/'base.json').write_text(json.dumps({'dataset':'vegeta-s4','native_code_families':{'cw20-base':{}}}))
            (d/'workspace.json').write_text(json.dumps({'dataset':'vegeta-s4','decisions':[]}))
            (d/'ext.json').write_text(json.dumps({'dataset':'vegeta-s4','native_code_families':{'test-lock':{'implementation_alias_of':'cw20-base'}}}))
            evidence={'dataset':'vegeta-s4','records':[
                {'evidence_id':'reviewed','blocker_runtime_code_family':blocker,'decision_runtime_code_family':impl,
                 'storage_owner':owner,'delegate_target':'0x'+'66'*20,'verified_contract_identifiers':['Impl.sol:Impl'],
                 'direct_selectors':['0xa9059cbb'],'call_frame_selectors':['0xa9059cbb'],
                 'review_conclusion':'verified delegate implementation','sources':['generated']},
                {'evidence_id':'unresolved','blocker_runtime_code_family':unresolved,'decision_runtime_code_family':unresolved,
                 'storage_owner':unresolved_owner,'verified_contract_identifiers':[],
                 'direct_selectors':['0x3ccfd60b'],'call_frame_selectors':['0x3ccfd60b'],
                 'review_conclusion':'insufficient evidence','sources':['generated']},
            ]}
            (d/'evidence.json').write_text(json.dumps(evidence))
            generated={'dataset':'vegeta-s4','families':[
                {'runtime_code_family':blocker,'all_observed_storage_owners':[owner],
                 'direct_selectors_exact':[{'value':'0xa9059cbb','count':1}],
                 'call_frame_selectors_exact':[{'value':'0xa9059cbb','count':1}],
                 'owner_evidence':[{'verified_source':{'contract_identifier':None}}],
                 'delegate_targets_exact':[{'target_address':'0x'+'66'*20,'target_runtime_code_family':impl,
                                            'verified_source':{'contract_identifier':'Impl.sol:Impl'}}]},
                {'runtime_code_family':unresolved,'all_observed_storage_owners':[unresolved_owner],
                 'direct_selectors_exact':[{'value':'0x3ccfd60b','count':1}],
                 'call_frame_selectors_exact':[{'value':'0x3ccfd60b','count':1}],
                 'owner_evidence':[{'verified_source':{'status':'not-found'}}], 'delegate_targets_exact':[]},
            ]}
            (d/'generated.json').write_text(json.dumps(generated))
            batch={'dataset':'vegeta-s4','decisions':[
                {'priority':17,'address':owner,'runtime_code_family':impl,'blocker_runtime_code_family':blocker,
                 'review_status':'reviewed','reviewed_native_family':'test-lock','mapping_basis':'delegate implementation reviewed',
                 'review_conclusion':'safe owner scoped mapping','evidence_id':'reviewed','evidence_sources':['generated']},
                {'priority':18,'address':unresolved_owner,'runtime_code_family':unresolved,'blocker_runtime_code_family':unresolved,
                 'review_status':'unresolved','reviewed_native_family':None,'mapping_basis':'',
                 'review_conclusion':'insufficient evidence','evidence_id':'unresolved','evidence_sources':['generated']},
            ]}
            (d/'batch.json').write_text(json.dumps(batch))
            cmd=[sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-fifth-batch.py'),
                '--review-base',str(d/'base.json'),'--workspace-decisions',str(d/'workspace.json'),
                '--family-extension',str(d/'ext.json'),'--fifth-batch-decisions',str(d/'batch.json'),
                '--review-evidence',str(d/'evidence.json'),'--generated-evidence',str(d/'generated.json'),
                '--pending-output',str(d/'pending.json')]
            subprocess.run(cmd,check=True); subprocess.run(cmd,check=True)
            workspace=json.loads((d/'workspace.json').read_text())
            by_family={r['runtime_code_family']:r for r in workspace['decisions']}
            self.assertEqual(by_family[impl]['review_status'],'reviewed')
            self.assertEqual(by_family[unresolved]['review_status'],'unresolved')
            self.assertEqual(json.loads((d/'pending.json').read_text())['pending_count'],1)
            self.assertIn('test-lock',json.loads((d/'base.json').read_text())['native_code_families'])

            # The implementation profile can be scoped explicitly to the proxy storage owner.
            (d/'candidate.json').write_text(json.dumps({'dataset':'vegeta-s4','native_code_families':{'test-lock':{}},'profile_mappings':[]}))
            subprocess.run([sys.executable,str(ROOT/'tools/vegeta/apply-vegeta-s4-review-decisions.py'),
                '--base-map',str(d/'candidate.json'),'--decisions',str(d/'workspace.json'),'--output',str(d/'mapped.json')],check=True)
            mapped=json.loads((d/'mapped.json').read_text())
            row=next(r for r in mapped['profile_mappings'] if r['ethereum_profile_family']==impl)
            self.assertEqual(row['storage_owner_scope'],[owner])
            self.assertFalse(any(r.get('ethereum_profile_family')==unresolved for r in mapped['profile_mappings']))

            # Drift in the freshly generated delegate target must fail before changing reviewed state.
            broken=json.loads((d/'generated.json').read_text()); broken['families'][0]['delegate_targets_exact'][0]['target_address']='0x'+'77'*20
            (d/'broken-generated.json').write_text(json.dumps(broken))
            bad=cmd.copy(); bad[bad.index(str(d/'generated.json'))]=str(d/'broken-generated.json')
            self.assertNotEqual(subprocess.run(bad).returncode,0)

if __name__=='__main__': unittest.main()
