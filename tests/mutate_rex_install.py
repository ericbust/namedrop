"""Prove each U7 test detects a relevant defect, restoring every source in finally.

Usage: python3 -I tests/mutate_rex_install.py OUT
Run with no other builder or suite writing this checkout.
"""
import json
import os
from pathlib import Path
import subprocess
import sys

repo=Path(__file__).resolve().parents[1]
out=Path(sys.argv[1]).resolve();out.mkdir(parents=True,exist_ok=True)
env={k:v for k,v in os.environ.items() if k not in {'REX_BLOCK','REX_SESSION','REX_SERVER','REX_CLIENT'}}
unit=['cargo','test','--locked','--all-features','--lib']
integration=['cargo','test','--locked','--all-features','--test','rex_install']
mutations=[
 ('removed-line-refresh','src/rex_ui.rs','record["line"] == "inserted"','record["line"] == "present"',unit+['refresh_remembers_a_removed_managed_line_without_restoring_it']),
 ('removed-line-doctor','src/health.rs','if diagnostic["line"] == "removed" {','if diagnostic["line"] == "present" {',unit+['rex_module_explanation_is_one_line_with_each_key']),
 ('line-ownership','src/rex_ui/install.rs','if print || !regular {','if print && !regular {',unit+['whole_lines_and_prior_ownership_decide_the_edit']),
 ('helper-state','src/installed_runtime/helpers.rs','state_matches\n        && matches!(', '(!state_matches)\n        && matches!(',unit+['only_helpers_of_the_exact_state_can_be_stopped']),
 ('removed-exception','src/installed_runtime/retirement.rs','if allow_removed => Ok(())','if !allow_removed => Ok(())',unit+['a_retired_state_creates_no_command_lock_and_removed_has_one_exception']),
 ('drain-mode','src/installed_runtime/retirement.rs','match file.try_lock() {','match file.try_lock_shared() {',unit+['an_exclusive_drain_waits_without_mutating_the_state_and_times_out']),
 ('fallback','src/installed_runtime/packages.rs','&& shared_failure {','&& !shared_failure {',unit+['removed_records_admit_a_new_owner_and_herdr_falls_back_only_on_shared_lock_failures']),
 ('writer-kind','src/installed_runtime/packages.rs','"activate" | "install" => Self::Activation,','"activate" | "install" => Self::Command,',unit+['lifecycle_operations_derive_one_lease_kind']),
 ('seal','src/rex_ui/handoff.rs','entry["sealed"] = json!(true);','entry["sealed"] = json!(false);',unit+['confirmation_seals_without_a_module_or_server_and_never_recreates_a_removed_record']),
 ('confirmation-budget','src/rex_ui/handoff.rs','let deadline = Instant::now() + Duration::from_secs(5);','let deadline = Instant::now() + Duration::from_secs(7);',unit+['confirmation_caps_a_hung_server_at_five_seconds_even_with_a_sixty_second_request_limit']),
 ('early-retirement','src/main.rs','if let Some(error) = refused {','if let Some(error) = refused.filter(|_| false) {',integration+['retirement_precedes_every_dispatch_form_and_all_cached_hook_forms']),
 ('doctor-incarnation','src/rex_ui/handoff.rs','if p.incarnation.0 == a.agent','if p.incarnation.0 != a.agent',integration+['takeover_resumes_predecessors_and_doctor_reads_liveness_without_the_old_store']),
 ('installer-rollback','src/rex_ui/install.rs','fs::rename(backup, &edit.path).map_err(|e| e.to_string())?;','let _ = backup;',integration+['installer_round_trips_guarded_config_and_rolls_back_rpc_errors']),
 ('queued-hook','src/main.rs','if cli.quiet_retirement()\n                && selected_state','if !cli.quiet_retirement()\n                && selected_state',integration+['a_binding_hook_queued_behind_the_handoff_gate_stays_quiet']),
]
results=[]
for name,path,old,new,command in mutations:
 if len(sys.argv)>2 and name not in sys.argv[2:]:continue
 target=repo/path;before=target.read_bytes();text=before.decode();assert old in text,(name,'mutation anchor missing')
 try:
  target.write_text(text.replace(old,new,1))
  with (out/(name+'.log')).open('w') as log:
   p=subprocess.Popen(command,cwd=repo,env=env,stdout=log,stderr=subprocess.STDOUT)
   with (out/'pids.jsonl').open('a') as f:f.write(json.dumps({'name':name,'pid':p.pid,'command':command})+'\n')
   code=p.wait()
  output=(out/(name+'.log')).read_text()
  killed=code!=0 and 'test result: FAILED' in output and 'could not compile' not in output
  results.append({'mutation':name,'path':path,'test_command':command,'exit':code,'killed':killed})
  (out/'results.json').write_text(json.dumps(results,indent=2)+'\n')
  print(name,'killed' if killed else 'SURVIVED OR INVALID',flush=True)
  if not killed:raise SystemExit(1)
 finally:target.write_bytes(before)
