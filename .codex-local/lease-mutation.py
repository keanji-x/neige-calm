from pathlib import Path
import subprocess, json, hashlib, re
p=Path('crates/calm-server/src/operation/driver.rs')
original=p.read_bytes()
Path('.codex-local/lease-driver-before-mutation.rs.backup').write_bytes(original)
plan=json.loads(Path('.codex-local/lease-mutation-plan.json').read_text())
old=b'if !eligible {'; new=b'if false && !eligible {'
assert original.count(old)==1
command=['cargo','nextest','run','--locked','-p','calm-server','--test','mcp_integration_suite','candidate_lease_defers','--test-threads','8','--no-fail-fast']
record={'plan_sha256':hashlib.sha256(Path('.codex-local/lease-mutation-plan.json').read_bytes()).hexdigest(),'before_sha256':hashlib.sha256(original).hexdigest()}
try:
 p.write_bytes(original.replace(old,new))
 assert p.read_bytes().count(new)==1 and p.read_bytes()!=original
 record['mutation_applied_sha256']=hashlib.sha256(p.read_bytes()).hexdigest()
 with open('.codex-local/lease-mutation-red.log','w') as log:
  red=subprocess.run(command,stdout=log,stderr=subprocess.STDOUT)
 raw=Path('.codex-local/lease-mutation-red.log').read_text()
 actual=sorted(set(re.findall(r'FAIL\s+\[[^\]]+\]\s+\([^\)]+\) calm-server::mcp_integration_suite (\S+)',raw)))
 record.update(red_exit=red.returncode,actual_red_set=actual,expected_red_set=sorted(plan['complete_predicted_red_set']),audit_assertion_failures=raw.count('quiescent retained wait status must not compete with recovery claims'))
finally:
 p.write_bytes(original)
 assert p.read_bytes()==original
 record['restored_sha256']=hashlib.sha256(p.read_bytes()).hexdigest()
 with open('.codex-local/lease-mutation-restored-green.log','w') as log:
  green=subprocess.run(command,stdout=log,stderr=subprocess.STDOUT)
 record['restored_green_exit']=green.returncode
 Path('.codex-local/lease-mutation-result.json').write_text(json.dumps(record,indent=2)+'\n')
 print(json.dumps(record,indent=2),flush=True)
assert record['red_exit']==100
assert record['actual_red_set']==record['expected_red_set']
assert record['audit_assertion_failures']==2
assert record['restored_green_exit']==0
