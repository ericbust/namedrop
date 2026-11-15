"""Prove U8 contracts by one reversible source mutation per test."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tarfile
import tempfile

repo = Path(__file__).resolve().parents[1]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
fixture = ['cargo', 'test', '--locked', '--test', 'standalone_runtime', 'standalone_publication_contracts', '--', '--exact']
mutations = [
    ('store-and-survey', 'scripts/runtime.py', 'survey=installation.session_panes if installation else None', 'survey=None', 'test_no_herdr_records_store_and_survey_and_source'),
    ('source-version', 'scripts/runtime_install.py', 'return dict(version=version[1], kind=kind, origin=origin)', "return dict(version='0.0.0', kind=kind, origin=origin)", 'test_no_herdr_records_store_and_survey_and_source'),
    ('checkout-source', 'scripts/runtime_install.py', "kind = 'checkout'", "kind = 'archive'", 'test_checkout_source_records_repository_origin_and_version'),
    ('stale-registration', 'scripts/runtime_install.py', 'checked[endpoint] = rex_server_check(endpoint)', 'checked[endpoint] = None', 'test_stale_registration_skips_its_missing_server_and_surveys_the_target'),
    ('unknown-registration', 'scripts/runtime_install.py', "self.unknown_servers = [endpoint for endpoint, _ in unchecked if endpoint not in known]", 'self.unknown_servers = []', 'test_uncheckable_registered_server_without_store_rows_remains_unverified'),
    ('source-rollback', 'scripts/runtime.py', '            record_source()\n        except BaseException:\n            restore(runtime, previous, incoming)\n            record_source()\n            raise', '        except BaseException:\n            restore(runtime, previous, incoming)\n            raise\n        record_source()', 'test_source_write_failure_restores_previous_same_schema_runtime'),
    ('argv-gate', 'scripts/runtime.py', "require(arguments[:1] in (['activate'], ['--herdr-pane']),", 'require(True,', 'test_mistyped_explicit_call_cannot_publish_herdr_state'),
    ('owner-refusal', 'scripts/runtime_install.py', "require(value != 'herdr',", 'require(True,', 'test_herdr_owner_and_handoff_are_refused'),
    ('retired-refusal', 'scripts/runtime.py', "require(json.loads(marker.read_text()).get('reason') != 'herdr_handoff', MOVING_TO_HERDR)", 'require(True, MOVING_TO_HERDR)', 'test_herdr_owner_and_handoff_are_refused'),
    ('preflight', 'scripts/runtime.py', 'require(ready(state_before, agents),', 'require(True,', 'test_schema_change_refuses_survey_agent_and_preserves_store'),
    ('socket-liveness', 'scripts/update.py', "if checked[endpoint] != 'stopped'", "if checked[endpoint] is None", 'test_store_socket_failures_are_counted_except_missing_and_refused'),
    ('marker-clear', 'scripts/runtime.py', '            os.rename(self.marker, self.aside)', '            shutil.copy2(self.marker, self.aside)', 'test_removed_marker_is_cleared_under_both_locks_before_install'),
    ('marker-restore', 'scripts/runtime.py', '                outcome = self.restore()', "                outcome = 'restored'", 'test_reinstall_that_does_not_complete_keeps_the_removed_marker'),
    ('window-shrink', 'scripts/runtime.py', 'digest = stage(root, runtime, incoming) if installation else None', 'digest = None', 'test_reinstall_that_does_not_complete_keeps_the_removed_marker'),
    ('marker-superseded', 'scripts/runtime.py', '                os.link(self.aside, self.marker)', '                os.replace(self.aside, self.marker)', 'test_marker_written_during_the_reinstall_wins'),
    ('install-signals', 'scripts/runtime.py', 'previous = {number: signal.signal(number, stop) for number in (signal.SIGTERM, signal.SIGHUP)}', 'previous = {}', 'test_signal_during_the_reinstall_puts_the_marker_back'),
    ('aside-recovery', 'scripts/runtime.py', '        if self.aside.exists() or self.aside.is_symlink():', '        if False:', 'test_marker_left_aside_goes_back_on_the_next_run'),
    ('unfenced-restore', 'scripts/runtime.py', '        gate = exclusive_within(self.runtime, self.WAIT)\n', "        gate = exclusive_within(self.runtime, self.WAIT)\n        if gate is None:\n            raise RuntimeError(busy_gate_message(self.runtime / 'commands.lock'))\n", 'test_busy_gate_at_put_back_writes_the_marker_anyway'),
    ('unwritable-marker', 'scripts/runtime.py', "            except OSError as error:\n                return (f'Namedrop could not mark", "            except KeyboardInterrupt as error:\n                return (f'Namedrop could not mark", 'test_marker_that_cannot_go_back_is_named_and_goes_back_next_run'),
    ('marker-reason', 'scripts/runtime.py', "            require(reason != 'herdr_handoff', MOVING_TO_HERDR)\n            require(reason == 'removed',", '            require(True, MOVING_TO_HERDR)\n            require(True,', 'test_handoff_before_the_gate_stops_the_reinstall'),
    ('inlock-owner', 'scripts/runtime.py', '            installation.check_owner()\n            source = installation.source()', '            source = installation.source()', 'test_owner_taken_before_the_lock_stops_the_reinstall'),
    ('install-wording', 'scripts/runtime.py', "print('Installation did not complete. '", "print('Activation did not complete. '", 'test_same_schema_failure_rolls_back_source_and_runtime'),
    ('herdr-took-over', 'scripts/runtime.py', '                outcome = self.restore()', "                outcome = 'restored'", 'test_real_reinstall_after_herdr_took_the_packages_leaves_the_state_retired'),
    ('sessionless-agents', 'scripts/runtime_install.py', "return [pane for pane in panes if pane.get('agent_session')], []", 'return panes, []', 'test_first_install_leaves_out_agents_without_a_session'),
    ('first-store-schema', 'scripts/runtime.py', "(not (state / 'namedrop.sqlite3').exists() or state_schema(runtime) != incoming['state_schema'])", "(not (state / 'namedrop.sqlite3').exists())", 'test_first_publish_over_a_store_it_cannot_migrate_is_withdrawn'),
    ('withdraw', 'scripts/runtime.py', '            elif previous is None:\n                withdraw_first_publish(runtime, found)', '            elif False:\n                withdraw_first_publish(runtime, found)', 'test_first_publish_over_a_store_it_cannot_migrate_is_withdrawn'),
    ('withdraw-recovery', 'scripts/runtime.py', '            except BaseException:\n                withdraw_first_publish(runtime, found)\n                raise\n            (runtime', '            except BaseException:\n                raise\n            (runtime', 'test_first_publish_over_a_store_it_cannot_migrate_is_withdrawn'),
    ('withdraw-unchanged', 'scripts/runtime.py', '    if unchanged:\n        switch(runtime, None)', '    if True:\n        switch(runtime, None)', 'test_initial_store_migration_recovers_after_interruption'),
    ('nonlive-preflight', 'scripts/runtime.py', 'state_before, agents = check(live=True) if live else check()', 'state_before, agents = check(live=True)', 'test_session_refresh_reinstall_waits_for_every_open_session'),
    ('refusal-names', 'scripts/runtime_install.py', r"return '\n  '.join(safe(line) for line in lines)", 'return safe(lines[0])', 'test_session_refresh_reinstall_waits_for_every_open_session'),
    ('removed-bindings', 'scripts/runtime_install.py', r"return '\n  '.join(safe(line) for line in lines)", 'return safe(lines[0])', 'test_real_reinstall_refused_after_removal_keeps_the_state_retired'),
    ('all-endpoints', 'scripts/runtime_install.py', 'rex_session_panes(all_endpoints=True)', 'rex_session_panes(all_endpoints=False)', 'test_store_socket_failures_are_counted_except_missing_and_refused'),
    ('herdr-call', 'scripts/runtime_install.py', "        kind, origin = 'archive', str(self.root)", "        subprocess.run(['herdr', 'status'], capture_output=True)\n        kind, origin = 'archive', str(self.root)", 'test_no_herdr_records_store_and_survey_and_source'),
    ('origin-credentials', 'scripts/runtime_install.py', 'origin = public(result.stdout.strip())', 'origin = result.stdout.strip()', 'test_checkout_source_records_repository_origin_and_version'),
    ('default-socket', 'scripts/runtime_install.py', "'Library/Application Support/rex/server.sock'", "'Library/Application Support/rex/other.sock'", 'test_default_socket_is_surveyed_without_rex_server'),
    ('xdg-message', 'scripts/runtime_install.py', "target = str(Path(directory) / 'rex/server.sock') if directory else ''", "target = str(Path(os.environ['XDG_RUNTIME_DIR']) / 'rex/server.sock')", 'test_default_socket_is_surveyed_without_rex_server'),
    ('unknown-server-wording', 'scripts/runtime_install.py', '        require(not self.unknown_servers,', '        require(True,', 'test_uncheckable_registered_server_without_store_rows_remains_unverified'),
    ('registration', 'scripts/runtime_install.py', "record.get('state') == str(self.state)", 'True', 'test_install_verification_checks_packages_state_and_registration'),
    ('rollback', 'scripts/runtime.py', '            if result.returncode:\n                restore(runtime, previous, incoming)', '            if False:\n                restore(runtime, previous, incoming)', 'test_same_schema_failure_rolls_back_source_and_runtime'),
    ('schema-rollback', 'scripts/runtime.py', "require(schema == contract(runtime / previous)['state_schema'], 'State has already migrated.')", "require(True, 'State has already migrated.')", 'test_failed_install_after_migration_keeps_new_schema_and_source'),
    ('recovery', 'scripts/runtime.py', '        recover_migration(runtime)', '        pass', 'test_initial_store_migration_recovers_after_interruption'),
    ('incoming-binary', 'src/main.rs', 'if cli.is_rex_sessions() {', 'if false && cli.is_rex_sessions() {', 'test_real_incoming_survey_bypasses_current_and_gate_without_writing'),
    ('first-store', 'scripts/runtime.py', "or installation and previous is None", "or False and previous is None", 'test_real_install_reinstall_and_cached_hook_resume'),
    ('removal-recheck', 'src/rex_ui/install.rs', '    if standalone {\n        match installed_runtime::retirement(state)? {', '    if false && standalone {\n        match installed_runtime::retirement(state)? {', 'test_removal_paused_after_marker_stops_after_runtime_reinstall'),
    ('tour-gate', 'scripts/runtime.py', "if ((previous is not None and incoming.get('command_gate') == 1", "if ((previous is not None and incoming.get('command_gate') == 0", 'test_schema_gate_times_out_with_tour_open_and_same_schema_passes'),
    ('survey-session', 'src/rex_ui/sessions.rs', 'mux::resolve_session(&identity.session)', 'mux::resolve_session(&mux::SessionClaim::None)', None),
    ('interrupted-wording', 'src/installed_runtime.rs', 'if retirement::standalone(state).unwrap_or(false) {', 'if false {', None),
]
# Mutations of Rust code that no fixture case reaches run one library test.
LIBRARY = {'survey-session': 'rex_ui::sessions::tests::survey_report_preserves_blocks_kinds_sessions_and_states',
           'interrupted-wording': 'installed_runtime::retirement::tests::an_unfinished_migration_is_refused_in_the_owner_s_words'}
results = []
for name, filename, old, new, case in mutations:
    if len(sys.argv) > 2 and name not in sys.argv[2:]:
        continue
    path = repo / filename
    before = path.read_bytes()
    text = before.decode()
    assert old in text, (name, 'missing mutation anchor')
    command = fixture if case else ['cargo', 'test', '--locked', '--lib', LIBRARY[name], '--', '--exact']
    env = dict(os.environ)
    for key in ('REX_BLOCK', 'REX_SESSION', 'REX_SERVER', 'REX_CLIENT'):
        env.pop(key, None)
    if case:
        env['NAMEDROP_U8_TEST'] = 'StandaloneChecks.' + case
    try:
        path.write_text(text.replace(old, new) if name == 'retired-refusal' else text.replace(old, new, 1))
        with (out / (name + '.log')).open('w') as log:
            child = subprocess.Popen(command, cwd=repo, env=env, stdout=log, stderr=subprocess.STDOUT)
            with (out / 'pids.jsonl').open('a') as ledger:
                ledger.write(json.dumps(dict(mutation=name, pid=child.pid, command=command)) + '\n')
            code = child.wait()
        output = (out / (name + '.log')).read_text()
        killed = code != 0 and 'test result: FAILED' in output and 'could not compile' not in output
        results.append(dict(mutation=name, test=case, command=command, exit=code, killed=killed))
        (out / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
        print(name, 'killed' if killed else 'SURVIVED OR INVALID', flush=True)
        if not killed:
            raise SystemExit(1)
    finally:
        path.write_bytes(before)

if os.environ.get('NAMEDROP_TEST_ARCHIVE') and (len(sys.argv) == 2 or 'archive-migration' in sys.argv[2:]):
    with tempfile.TemporaryDirectory(prefix='nd-u8-mut-') as directory:
        folder = Path(directory)
        with tarfile.open(os.environ['NAMEDROP_TEST_ARCHIVE']) as archive:
            archive.extractall(folder, filter='data')
        package = next(folder.iterdir())
        path = package / 'scripts/runtime.py'
        before = path.read_text()
        old = "if ((previous is not None and incoming.get('command_gate') == 1"
        assert old in before
        path.write_text(before.replace(old, old.replace('== 1', '== 0'), 1))
        mutant = out / 'archive-migration.tar.gz'
        with tarfile.open(mutant, 'w:gz') as archive:
            archive.add(package, arcname=package.name)
        env = dict(os.environ, NAMEDROP_TEST_ARCHIVE=str(mutant))
        command = ['cargo', 'test', '--locked', '--test', 'standalone_runtime', 'standalone_archive_upgrade_lane', '--', '--ignored', '--exact']
        with (out / 'archive-migration.log').open('w') as log:
            child = subprocess.Popen(command, cwd=repo, env=env, stdout=log, stderr=subprocess.STDOUT)
            with (out / 'pids.jsonl').open('a') as ledger:
                ledger.write(json.dumps(dict(mutation='archive-migration', pid=child.pid, command=command)) + '\n')
            code = child.wait()
        output = (out / 'archive-migration.log').read_text()
        killed = code != 0 and 'test result: FAILED' in output and 'could not compile' not in output
        results.append(dict(mutation='archive-migration', command=command, exit=code, killed=killed))
        (out / 'results.json').write_text(json.dumps(results, indent=2) + '\n')
        print('archive-migration', 'killed' if killed else 'SURVIVED OR INVALID', flush=True)
        if not killed:
            raise SystemExit(1)
