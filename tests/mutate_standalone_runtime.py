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
    ('retired-refusal', 'scripts/runtime.py', "require(json.loads(retired.read_text()).get('reason') != 'herdr_handoff',", 'require(True,', 'test_herdr_owner_and_handoff_are_refused'),
    ('preflight', 'scripts/runtime.py', 'require(ready(state_before, agents),', 'require(True,', 'test_schema_change_refuses_survey_agent_and_preserves_store'),
    ('socket-liveness', 'scripts/update.py', "if checked[endpoint] != 'stopped'", "if checked[endpoint] is None", 'test_store_socket_failures_are_counted_except_missing_and_refused'),
    ('marker-clear', 'scripts/runtime.py', "                retired.unlink()", '                pass', 'test_removed_marker_is_cleared_under_both_locks_before_install'),
    ('registration', 'scripts/runtime_install.py', "record.get('state') == str(self.state)", 'True', 'test_install_verification_checks_packages_state_and_registration'),
    ('rollback', 'scripts/runtime.py', '            if result.returncode:\n                restore(runtime, previous, incoming)', '            if False:\n                restore(runtime, previous, incoming)', 'test_same_schema_failure_rolls_back_source_and_runtime'),
    ('schema-rollback', 'scripts/runtime.py', "require(schema == contract(runtime / previous)['state_schema'], 'State has already migrated.')", "require(True, 'State has already migrated.')", 'test_failed_install_after_migration_keeps_new_schema_and_source'),
    ('recovery', 'scripts/runtime.py', '        recover_migration(runtime)', '        pass', 'test_initial_store_migration_recovers_after_interruption'),
    ('incoming-binary', 'src/main.rs', 'if cli.is_rex_sessions() {', 'if false && cli.is_rex_sessions() {', 'test_real_incoming_survey_bypasses_current_and_gate_without_writing'),
    ('first-store', 'scripts/runtime.py', "or installation and previous is None", "or False and previous is None", 'test_real_install_reinstall_and_cached_hook_resume'),
    ('removal-recheck', 'src/rex_ui/install.rs', '    if standalone {\n        match installed_runtime::retirement(state)? {', '    if false && standalone {\n        match installed_runtime::retirement(state)? {', 'test_removal_paused_after_marker_stops_after_runtime_reinstall'),
    ('tour-gate', 'scripts/runtime.py', "if ((previous is not None and incoming.get('command_gate') == 1", "if ((previous is not None and incoming.get('command_gate') == 0", 'test_schema_gate_times_out_with_tour_open_and_same_schema_passes'),
    ('survey-session', 'src/rex_ui/sessions.rs', 'mux::resolve_session(&identity.session)', 'mux::resolve_session(&mux::SessionClaim::None)', None),
]
results = []
for name, filename, old, new, case in mutations:
    if len(sys.argv) > 2 and name not in sys.argv[2:]:
        continue
    path = repo / filename
    before = path.read_bytes()
    text = before.decode()
    assert old in text, (name, 'missing mutation anchor')
    command = fixture if case else ['cargo', 'test', '--locked', '--lib', 'rex_ui::sessions::tests::survey_report_preserves_blocks_kinds_sessions_and_states', '--', '--exact']
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
