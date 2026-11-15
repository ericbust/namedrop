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
bins=['cargo','test','--locked','--all-features','--bin','namedrop']
rex_ui=['cargo','test','--locked','--all-features','--test','rex_ui']
health=['cargo','test','--locked','--all-features','--test','health']
installer=integration+['installer_round_trips_guarded_config_and_rolls_back_rpc_errors']
mutations=[
 ('removed-line-refresh','src/rex_ui.rs','if !matches!(record["line"].as_str(), Some("inserted" | "removed")) {','if !matches!(record["line"].as_str(), Some("present" | "removed")) {',unit+['refresh_remembers_a_removed_managed_line_without_restoring_it']),
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
 # The fix round for the U7 and U6 reviews (fix-u7b).
 ('u7f1-failed-read','src/rex_ui.rs','        Err(_) => return,\n    };\n    record["line"]','        Err(_) => false,\n    };\n    record["line"]',unit+['the_line_state_is_read_from_init_lua_each_time']),
 ('u7f1-line-put-back','src/rex_ui.rs','record["line"] = json!(if present { "inserted" } else { "removed" });','if !present {\n        record["line"] = json!("removed");\n    }',unit+['the_line_state_is_read_from_init_lua_each_time']),
 ('u7f1-doctor-trusts-record','src/rex_ui.rs','    reinspect_line(&mut record);\n    Some(report(&record)','    Some(report(&record)',unit+['the_line_state_is_read_from_init_lua_each_time']),
 ('u7f1-install-put-back','src/rex_ui/install.rs','return if matches!(previous, Some("inserted" | "removed")) {','return if previous == Some("inserted") {',unit+['whole_lines_and_prior_ownership_decide_the_edit']),
 ('u7f6-present-stays-out','src/rex_ui/install.rs','    if matches!(previous, Some("inserted" | "removed")) {\n        return Line::Removed;','    if matches!(previous, Some("inserted" | "removed" | "present")) {\n        return Line::Removed;',unit+['whole_lines_and_prior_ownership_decide_the_edit']),
 ('u7f2-utf8','src/rex_ui/install.rs','        use std::os::unix::fs::OpenOptionsExt;\n        let _held = lease;','        use std::os::unix::fs::OpenOptionsExt;\n        let _held = lease;\n        std::str::from_utf8(&after).map_err(|e| e.to_string())?;',installer),
 ('u7f2-edit-failure-aborts','src/rex_ui/install.rs','            Err(error) => {\n                line = Line::Printed;\n                reason = Some(format!(\n                    "Namedrop could not edit {}: {error}",\n                    config.display()\n                ));\n                None\n            }','            Err(error) => return Err(error),',installer),
 ('u7-backups-kept','src/rex_ui/install.rs','    fn keep_only_its_backup(&self) {','    fn keep_only_its_backup(&self) {\n        if self.after.is_empty() || !self.after.is_empty() {\n            return;\n        }',installer),
 ('u7-baseline-before-lease','src/rex_ui/install.rs','    installed_runtime::package_guard(Some(home), state, Writer::Activation)?.require_write()?;\n','',installer),
 ('u7f5-removal-baseline','src/rex_ui/install.rs','            && failed_before.is_none()\n','',installer),
 ('u7f5-removal-no-edit','src/rex_ui/install.rs','            && edit.is_some()\n','',installer),
 ('u7f4-seal-without-call','src/rex_ui/handoff.rs','let seal_without_call = matches!(record["line"].as_str(), Some("printed" | "removed"))\n            || record["server"].as_str().is_none();','let seal_without_call = record["server"].as_str().is_none();',unit+['confirmation_seals_without_a_module_or_server']),
 ('u7-seal-entry-gone','src/rex_ui/handoff.rs','    if !sealed {\n        return Ok(json!({"status":"removed"}));\n    }\n','',unit+['a_seal_never_reports_sealed']),
 ('u7-seal-record-gone','src/rex_ui/handoff.rs','    let Some(mut record) = registration(home)? else {\n        return Ok(json!({"status":"removed"}));','    let Some(mut record) = registration(home)? else {\n        return Ok(json!({"status":"confirmed","sealed":true}));',unit+['a_seal_never_reports_sealed']),
 ('u7f7-deferred-gate-error','src/deferred_commands.rs','    let Ok(initial_gate) = crate::runtime_command_guard(state, None) else {\n        return Ok(());\n    };','    let initial_gate = crate::runtime_command_guard(state, None)?;',integration+['helpers_wait_for_the_gate']),
 ('u7f7-hooks-take-gate','src/main.rs','    if let Some((state, host)) = cli.subagent_hook() {','    let _hook_gate = selected_state\n        .as_deref()\n        .map(|state| namedrop::runtime_command_guard(state, None));\n    if let Some((state, host)) = cli.subagent_hook() {',integration+['helpers_wait_for_the_gate']),
 ('u7f7-helper-no-wait','src/reminder/clock.rs','    let initial_gate = match crate::runtime_command_guard(state, None) {','    let initial_gate = match crate::runtime_command_guard(state, Some(Duration::ZERO)) {',integration+['helpers_wait_for_the_gate']),
 ('u6f6-palette-clock','src/main.rs','    if let Some(state) = selected_state.as_deref() {\n        let socket = cli\n            .runtime','    if let Some(state) = selected_state.as_deref().filter(|_| cli.rex_action().is_none()) {\n        let socket = cli\n            .runtime',rex_ui+['a_palette_action_starts_the_reminder_clock']),
 ('u6f3-guard-parent','src/main.rs','let parent = unsafe { libc::getppid() } as u32;','let parent = unsafe { libc::getpid() } as u32;',rex_ui+['the_server_started_load_time_start']),
 ('u6f1-bridge-key','src/main.rs','            namedrop::rex_ui::local_endpoint,','            |socket: &Path| Some(namedrop::mux::Endpoint::parse(&format!("rex:{}", socket.display()))),',rex_ui+['the_server_started_load_time_start']),
 ('u6f3-undecoded','src/main.rs','eprintln!("{}", namedrop::rex_ui::error_line(&error));','eprintln!("{error}");',rex_ui+['refused_config_start_and_unknown_action']),
 ('u6f3-pause-resume','src/main.rs','        Action::Pause => execute_control_operation("worktree.pause", empty, runtime),\n        Action::Resume => execute_control_operation("worktree.resume", empty, runtime),','        Action::Pause => execute_control_operation("worktree.resume", empty, runtime),\n        Action::Resume => execute_control_operation("worktree.pause", empty, runtime),',bins+['palette_actions_run_their_own_commands']),
 ('u6f3-incomplete-setup','src/main.rs','    if !activation_complete(&report) {','    if false && !activation_complete(&report) {',bins+['palette_actions_run_their_own_commands']),
 ('u6f3-doctor-notice','src/health.rs','        if confirmed {\n            HealthSeverity::Info\n        } else {\n            HealthSeverity::Notice\n        },','        HealthSeverity::Info,',health+['doctor_reports_an_unconfirmed_rex_module']),
 ('u6f3-doctor-escalation','src/health.rs','        if check.severity != HealthSeverity::Info && report.status == HealthStatus::Healthy {\n            report.status = HealthStatus::NeedsAttention;\n        }\n','',health+['doctor_reports_an_unconfirmed_rex_module']),
 ('u6f3-doctor-on-herdr','src/health.rs','    if runtime.mux.endpoint().kind == crate::mux::MuxKind::Rex\n        && let Some(inspected) =','    if let Some(inspected) =',health+['doctor_reports_an_unconfirmed_rex_module']),
 ('u6f3-unconfirmed-keys','src/rex_ui.rs','        Some(Ok(report)) if report["confirmed"] == true => {','        Some(Ok(report)) if report["confirmed"] == true || report["stale"] != true => {',unit+['shortcuts_name_keys_only_for_a_confirmed_module']),
 ('u6f5-undefined-top','src/mux/rex/ui.rs','            let top = on_key.into_iter().next();','            let top = on_key.into_iter().find(|b| {\n                b["action"]\n                    .as_str()\n                    .is_some_and(|name| name.contains(\'.\') || present(name))\n            });',unit+['ui_keys_distinguish_effective_shadowed']),
 ('u6f7-stale','src/rex_ui.rs','        } else if report["confirmed"] != true && stale(&record, &report) {','        } else if false && stale(&record, &report) {',unit+['shortcuts_name_keys_only_for_a_confirmed_module']),
 ('u6f7-stale-repair','src/health.rs','    } else if stale {\n        format!(','    } else if false && stale {\n        format!(',unit+['an_unconfirmed_rex_module_names_a_reload']),
 ('u6f7-path-identity','src/mux/rex/ui.rs','    loaded == registered\n        || matches!(','    loaded == registered\n        || false && matches!(',unit+['ui_readback_requires_all_identity_fields']),
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
