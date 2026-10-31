"""Runtime publication with real subprocesses and isolated, disposable state."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

from support.environment import isolated_home

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / 'scripts'))
import runtime


def main():
    actual = Path(os.environ['NAMEDROP_TEST_BINARY'])
    declared = json.loads((REPO / 'runtime.json').read_text())
    assert json.loads(subprocess.check_output([actual, '--runtime-info'])) == declared
    assert declared['state_schema'] == 6 and declared['collaboration_protocol'] == 'collab/v1'
    subprocess.run([actual, '--validate-update-manifest', REPO / 'herdr-plugin.toml'], check=True)
    with tempfile.TemporaryDirectory(prefix='namedrop runtime ') as directory:
        base = Path(directory)
        os.environ.update(isolated_home(base / 'home'))
        os.environ.update(CLAUDE_CONFIG_DIR=str(base/'home/.claude'), CODEX_HOME=str(base/'home/.codex'))
        invalid = base / 'invalid.toml'
        invalid.write_text((REPO / 'herdr-plugin.toml').read_text().replace(
            '"sh", "./scripts/run.sh", "reconcile"', '"./bin/namedrop", "reconcile"'))
        assert subprocess.run([actual, '--validate-update-manifest', invalid], capture_output=True).returncode != 0
        state = base / 'state'
        state.mkdir()
        os.environ['HERDR_PLUGIN_STATE_DIR'] = str(state)
        os.environ['HERDR_WORKSPACE_ID'] = 'fixture'
        runtime.session_panes = lambda: ([{'_socket': 'fixture', 'agent_session': {'value':'legacy'}}], [])
        def release(name, *, failure=False, epoch=1, activated=True, schema=5, codex_ready=True, path='bin/namedrop'):
            root = base / name
            (root / path).parent.mkdir(parents=True)
            value = dict(declared, live_upgrade_epoch=epoch, state_schema=schema)
            value['live_migrate_from'] = [n for n in value.get('live_migrate_from', []) if n < schema]
            (root / 'runtime.json').write_text(json.dumps(value))
            binary = root / path
            binary.write_text(f'''#!{sys.executable}
import json, os, sqlite3, sys
if sys.argv[1:] == ['--runtime-info']:
    print({json.dumps(value)!r})
elif sys.argv[1] == '--migrate-runtime-state':
    with sqlite3.connect(os.path.join(sys.argv[2], 'namedrop.sqlite3')) as db:
        db.execute('PRAGMA user_version={schema}')
    print(json.dumps({{"state_schema":{schema}}}))
elif sys.argv[1] == 'activate':
    with sqlite3.connect(os.path.join(os.environ['HERDR_PLUGIN_STATE_DIR'], 'namedrop.sqlite3')) as db:
        db.execute('PRAGMA user_version={schema}')
    print(json.dumps({{"status":"ok", "operation":"activate", "data": {{"space_id":"fixture", "state_directory":os.environ['HERDR_PLUGIN_STATE_DIR'], "activated": {activated!r}, "integration_packages": {{"codex": {{"installed": True}}}}, "agent_integration_hook_activation": {{"codex": {{"migration_ready": {codex_ready!r}}}}}}}}}))
    sys.exit({7 if failure else 0})
elif sys.argv[1] == 'work':
    print('started', flush=True)
    sys.stdin.readline()
    print({name!r} + ': finished once', flush=True)
else:
    print({name!r})
''')
            binary.chmod(0o700)
            return root
        bootstrap = base/'bootstrap-state'
        bootstrap.mkdir()
        os.environ['HERDR_PLUGIN_STATE_DIR'] = str(bootstrap)
        assert runtime.activate(release('first-failure', failure=True, codex_ready=False), ['activate']) == 7
        assert subprocess.check_output([bootstrap/'runtime/namedrop', 'tool'], text=True).strip() == 'first-failure'
        runtime.session_panes = lambda: ([{'_socket':'fixture', 'agent':'claude', 'agent_session':{'value':'late-legacy'}}], [])
        assert runtime.activate(release('cache-ready'), ['activate']) == 0
        migration = json.loads((bootstrap/'runtime/legacy-sessions.json').read_text())
        assert migration == {'sessions':[['fixture','late-legacy'], ['fixture','legacy']], 'codex_ready':True, 'packages_ready':True}
        runtime.session_panes = lambda: ([{'_socket':'fixture', 'agent':'codex', 'agent_session':{'value':'new-session'}}], [])
        assert runtime.activate(release('new-instructions', codex_ready=False), ['activate']) == 0
        assert json.loads((bootstrap/'runtime/legacy-sessions.json').read_text()) == migration
        runtime.session_panes = lambda: ([{'_socket':'fixture', 'agent_session':{'value':'legacy'}}], [])
        schema_state = base / 'schema-state'
        schema_state.mkdir()
        os.environ['HERDR_PLUGIN_STATE_DIR'] = str(schema_state)
        assert runtime.activate(release('schema-three', schema=3), ['activate']) == 0
        previous_schema = runtime.current(schema_state / 'runtime')
        live_checks = []
        def live_preflight(live=False):
            live_checks.append(live)
            return {'other_servers': [], 'unfinished':1, 'joins':0, 'live_update':live}, [{}]
        runtime.preflight = live_preflight
        incoming = release('schema-five')
        try:
            runtime.activate(incoming, ['activate'])
            raise AssertionError('Direct activation bypassed legacy-session protection')
        except RuntimeError as error:
            assert 'pre-launcher tools' in str(error)
        assert live_checks == [True]
        assert runtime.current(schema_state / 'runtime') == previous_schema
        runtime.preflight = lambda live=False: ({'other_servers': [], 'unfinished':1, 'joins':0, 'live_update':live}, [])
        assert runtime.activate(incoming, ['activate']) == 0
        assert runtime.state_schema(schema_state / 'runtime') == 5
        # Checkouts built before bin/namedrop staging keep activating from
        # Cargo's Apple silicon output; with neither, activation says so.
        fallback_state = base / 'fallback-state'
        fallback_state.mkdir()
        os.environ['HERDR_PLUGIN_STATE_DIR'] = str(fallback_state)
        assert runtime.activate(release('mac-fallback', path='target/aarch64-apple-darwin/release/namedrop'), ['activate']) == 0
        assert subprocess.check_output([fallback_state/'runtime/namedrop', 'tool'], text=True).strip() == 'mac-fallback'
        unbuilt = base / 'unbuilt'
        unbuilt.mkdir()
        (unbuilt / 'runtime.json').write_text(json.dumps(declared))
        try:
            runtime.activate(unbuilt, ['activate'])
            raise AssertionError('Activation accepted a checkout without an executable')
        except RuntimeError as error:
            assert 'No built Namedrop executable' in str(error)
        assert subprocess.check_output([fallback_state/'runtime/namedrop', 'tool'], text=True).strip() == 'mac-fallback'
        # Only older macOS releases lack the command gate. Elsewhere the drain
        # stops before loading macOS process APIs rather than guessing.
        legacy_runtime = base / 'legacy-runtime'
        ungated = legacy_runtime / 'releases' / ('b' * 64)
        ungated.mkdir(parents=True)
        (ungated / 'runtime.json').write_text(json.dumps(
            {key: value for key, value in declared.items() if key not in ('command_gate', 'live_migrate_from')}))
        (ungated / 'namedrop').write_text('')
        platform, sys.platform = sys.platform, 'linux'
        try:
            runtime.drain_legacy_commands(legacy_runtime)
            raise AssertionError('A non-macOS drain inspected an ungated release')
        except RuntimeError as error:
            assert 'only older macOS releases' in str(error)
        finally:
            sys.platform = platform
        os.environ['HERDR_PLUGIN_STATE_DIR'] = str(state)
        first = release('first')
        assert runtime.activate(first, ['activate']) == 0
        launcher = state / 'runtime/namedrop'
        cached_command = [str(launcher), 'tool']
        legacy_before = (state / 'runtime/legacy-sessions.json').read_bytes()
        first_snapshot = launcher.parent / runtime.current(launcher.parent)
        assert subprocess.check_output(cached_command, text=True).strip() == 'first'
        worker = subprocess.Popen([launcher, 'work'], stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        try:
            assert worker.stdout.readline().strip() == 'started'
            second = release('second')
            assert runtime.activate(second, ['activate']) == 0
            shutil.rmtree(first)
            shutil.rmtree(second)
            assert subprocess.check_output(cached_command, text=True).strip() == 'second'
            # A retained raw checkout command follows the selected runtime too;
            # it cannot republish checkout paths into the shared packages.
            assert subprocess.check_output([actual, 'describe', 'send'], text=True).strip() == 'second'
            output, _ = worker.communicate('\n', timeout=5)
            assert worker.returncode == 0 and output == 'first: finished once\n', output
            assert first_snapshot.is_dir(), 'An in-use runtime must never be pruned by an update'
            assert (state / 'runtime/legacy-sessions.json').read_bytes() == legacy_before
            active_before = runtime.current(launcher.parent)
            failed = release('failed', failure=True)
            assert runtime.activate(failed, ['activate']) == 7
            assert runtime.current(launcher.parent) == active_before
            assert subprocess.check_output(cached_command, text=True).strip() == 'second'
            partial = release('partial', activated=False)
            try:
                runtime.activate(partial, ['activate'])
                raise AssertionError('Zero-exit partial activation accepted')
            except RuntimeError as error:
                assert 'did not verify' in str(error)
            assert runtime.current(launcher.parent) == active_before
            assert subprocess.check_output(cached_command, text=True).strip() == 'second'
            broken = release('breaking', epoch=2)
            runtime.preflight = lambda: ({'other_servers': [], 'unfinished':1, 'joins':0}, [])
            try:
                runtime.activate(broken, ['activate'])
                raise AssertionError('Breaking update accepted unfinished work')
            except RuntimeError as error:
                assert 'session refresh' in str(error)
            assert runtime.current(launcher.parent) == active_before
            assert subprocess.check_output(cached_command, text=True).strip() == 'second'
            runtime.preflight = lambda: ({'other_servers': [], 'unfinished':0, 'joins':0}, [])
            future = release('future', failure=True, epoch=2, schema=6)
            assert runtime.activate(future, ['activate']) == 7
            assert runtime.current(launcher.parent) != active_before
            assert subprocess.check_output(cached_command, text=True).strip() == 'future'
            assert (launcher.parent/active_before).is_dir()
            foreign = base / 'foreign'
            foreign.write_text('do not change')
            launcher.unlink()
            launcher.symlink_to(foreign)
            try:
                runtime.activate(failed, ['activate'])
                raise AssertionError('Foreign launcher accepted')
            except RuntimeError as error:
                assert 'not owned' in str(error)
            assert foreign.read_text() == 'do not change'
        finally:
            if worker.poll() is None:
                worker.kill()
                worker.wait()
    print('runtime lifecycle checks passed')


if __name__ == '__main__':
    main()
