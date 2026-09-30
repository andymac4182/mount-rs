"""Offline current filesystem verification; historical RustFS is a separate reference."""
import copy,hashlib,json,sys,types
from pathlib import Path
FS_SHA='27ef362db1c1778eb18cc51cd373258551276e42fad277cfe854c5f9ed41bf02'
V_SHA='aac3f50339ece6b4d718bd94bdd54dc9cd39bfa5b3415c227062241b84572d17'
def load_module(ref,name,pin):
 p=Path(ref['path']);b=p.read_bytes()
 if ref['sha256']!=pin or hashlib.sha256(b).hexdigest()!=pin:raise ValueError('program source pin')
 m=types.ModuleType(name);m.__file__=str(p);exec(compile(b,str(p),'exec'),m.__dict__);return m

def main():
 if len(sys.argv)!=3:raise ValueError('usage')
 b=Path(sys.argv[1]).read_bytes()
 if len(b)>65536 or hashlib.sha256(b).hexdigest()!=sys.argv[2]:raise ValueError('input pin')
 spec=json.loads(b)
 if set(spec)!={'schema','filesystem_public','filesystem_input','filesystem_extractor','arithmetic_verifier','historical_rustfs_public'} or spec['schema']!='mount-rs.filesystem-standalone-verification-input.v1':raise ValueError('closed input schema')
 v=load_module(spec['arithmetic_verifier'],'pinned_arithmetic',V_SHA)
 fs=load_module(spec['filesystem_extractor'],'pinned_filesystem',FS_SHA)
 public=v.load(spec['filesystem_public']);historical=v.load(spec['historical_rustfs_public']);private=v.load(spec['filesystem_input'])
 v.need(public['schema']=='mount-rs-native-filesystem-public-observations-v1' and public['extractor_sha256']==FS_SHA and public['input_spec_sha256']==spec['filesystem_input']['sha256'] and len(public['runs'])==1 and len(public['tables'])==16,'current filesystem binding')
 v.need('matched_comparison' not in public and 'matched_rustfs' not in private,'standalone cannot claim current pair')
 current=public['runs'][0];cells=v.verify_run(current,public['tables'],True)
 v.need(len(historical['runs'])==1 and historical['schema']=='mount-rs-native-public-observations-v2','historical schema')
 previous=historical['runs'][0];v.verify_run(previous,historical['tables'],False)
 v.need(current['source_revision']=='bf6062cb4fc0b3dcbfde4532bba1b8c46266ab2f' and previous['source_revision']=='7f4da840b815b24cc65ab37c93d7146a43f72775','closed source identities')
 item=private['run'];v.need(item['pins']['terminal']==current['terminal_sha256'] and item['label']==current['label'],'negative-control input binding')
 root=Path(item['owner_root'])/'combined-target';terminal,_=fs.read_json(root/'terminal.json',current['terminal_sha256'])
 seqs=terminal['stages'][0]['metric_sequences'][:2];rows={r['sequence']:r for r in terminal['phase_metrics']['boundaries'] if 'sequence' in r}
 pair=[fs.read_json(root/rows[s]['controller']['file'],rows[s]['controller']['sha256'])[0] for s in seqs]
 fs.checked_delta(*pair);fs.verify_oracles(root,terminal,10)
 passed=[]
 def reject(name,operation):
  try:operation()
  except fs.ExtractionError:passed.append(name)
  else:raise v.Rejected('negative control accepted: '+name)
 changed=copy.deepcopy(pair[1]);changed['delta_from_previous']['counters']['allocations']['counters']['rust_allocations']+=1
 reject('allocation_delta_tamper',lambda:fs.checked_delta(pair[0],changed))
 oracle_path=root/'oracle-receipts/initial.json';witness,witness_sha=fs.read_json(oracle_path,terminal['fresh_oracle_passes'][0]['receipt']['sha256']);bad=copy.deepcopy(witness);bad['completed_files']-=1;original=fs.read_json
 try:
  fs.read_json=lambda path,pin=None:(bad,witness_sha) if Path(path)==oracle_path else original(path,pin)
  reject('incomplete_full_oracle',lambda:fs.verify_oracles(root,copy.deepcopy(terminal),10))
 finally:fs.read_json=original
 reject('historical_control_refused_as_current_pair',lambda:fs.matched_rustfs_summary(dict(spec['historical_rustfs_public'],label=previous['label']),current))
 v.need(len(passed)==3,'negative controls incomplete')
 result={'schema':'mount-rs.filesystem-standalone-with-historical-verification.v1','qualified_current_filesystem':True,'same_source_pair_available':False,'current_cells_verified':len(cells),'historical_cells_arithmetic_checked':16,'input_spec_sha256':sys.argv[2],'filesystem_public_sha256':spec['filesystem_public']['sha256'],'historical_rustfs_public_sha256':spec['historical_rustfs_public']['sha256'],'filesystem_extractor_sha256':FS_SHA,'arithmetic_verifier_sha256':V_SHA,'standalone_verifier_sha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'current_source':current['source_revision'],'historical_source':previous['source_revision'],'negative_controls_rejected':passed,'scope':'offline current filesystem arithmetic/full-oracle consistency; historical RustFS reference explicitly rejected as a current matched control; no new workload or production/saturation/physical-IOPS proof'}
 print(json.dumps(result,sort_keys=True,separators=(',',':'),allow_nan=False))
if __name__=='__main__':main()
