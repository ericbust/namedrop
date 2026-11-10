"""Runtime publication with real subprocesses and isolated, disposable state."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / 'scripts'))
import runtime


def tree(path):
    """Every path under `path`, without following links, with its contents."""
    found = {}
    for directory, directories, files in os.walk(path):
        for name in directories + files:
            entry = Path(directory) / name
            found[str(entry.relative_to(path))] = (
                ('link', os.readlink(entry)) if entry.is_symlink()
                else ('dir',) if entry.is_dir() else ('file', entry.read_bytes()))
    return found


def check_pruning(base, release):
    state = base / 'prune-state'
    state.mkdir()
    os.environ['HERDR_PLUGIN_STATE_DIR'] = str(state)
    rt = state / 'runtime'
    releases = rt / 'releases'
    def activate(root):
        assert runtime.activate(root, ['activate']) == 0
        return runtime.current(rt).name
    def retained():
        return {entry.name for entry in releases.iterdir() if runtime.RELEASE.fullmatch(entry.name)
                and entry.is_dir() and not entry.is_symlink()}

    activate(release('prune-one'))
    outside = base / 'prune-outside'
    (outside / 'release').mkdir(parents=True)
    (outside / 'release/namedrop').write_text('outside')
    (outside / 'file').write_text('outside')
    (rt / 'sentinel').write_text('keep')
    (rt / 'sentinel-dir').mkdir()
    (rt / 'sentinel-dir/file').write_text('keep')
    (rt / 'sentinel-link').symlink_to(outside / 'file')
    (releases / 'not-a-release').mkdir()
    (releases / 'not-a-release/namedrop').write_text('keep')
    (releases / ('A' * 64)).mkdir()
    (releases / ('d' * 64)).write_text('a file, not a release')
    (releases / ('f' * 64)).symlink_to(outside / 'release')
    sentinels = ('sentinel', 'sentinel-dir', 'sentinel-link', 'releases/not-a-release',
                 'releases/' + 'A' * 64, 'releases/' + 'd' * 64, 'releases/' + 'f' * 64)
    def unrelated():
        return {name: value for name, value in tree(rt).items()
                if any(name == item or name.startswith(item + '/') for item in sentinels)}
    unrelated_before, outside_before = unrelated(), tree(outside)
    assert len(unrelated_before) == 9

    two = activate(release('prune-two'))
    three_root = release('prune-three')
    three = activate(three_root)
    assert retained() == {two, three}, 'activation keeps only the current and previous releases'
    assert json.loads((rt / 'activation.json').read_text()) == {'revision': three, 'previous': two}
    assert activate(three_root) == three and retained() == {two, three}, 'reactivation keeps the earlier previous'
    assert json.loads((rt / 'activation.json').read_text()) == {'revision': three, 'previous': two}

    sleep = Path(shutil.which('sleep')).resolve()
    held = hashlib.sha256(sleep.read_bytes()).hexdigest()
    (releases / held).mkdir()
    shutil.copy(sleep, releases / held / 'namedrop')
    shutil.copy(releases / three / 'runtime.json', releases / held / 'runtime.json')
    sleeper = subprocess.Popen(['sleep', '120'], executable=releases / held / 'namedrop')
    try:
        four = activate(release('prune-four'))
        five = activate(release('prune-five'))
        assert retained() == {four, five, held}, 'a release a running process uses is kept'
    finally:
        sleeper.kill()
        sleeper.wait()
    listing = runtime.all_pids
    def unlisted():
        raise RuntimeError('Could not list processes')
    runtime.all_pids = unlisted
    try:
        six_root = release('prune-six')
        six = activate(six_root)
        assert retained() == {four, five, six, held}, 'without a process list nothing is removed'
    finally:
        runtime.all_pids = listing
    assert activate(six_root) == six and retained() == {five, six}, 'a release is removed once no process runs from it'

    (rt / '.pruning').rmdir()
    (rt / '.pruning').symlink_to(outside)
    seven_root = release('prune-seven')
    seven = activate(seven_root)
    assert retained() == {five, six, seven}, 'a prune that cannot use its own directory removes nothing'
    (rt / '.pruning').unlink()

    real_rmtree = shutil.rmtree
    def interrupted(*_args, **_kwargs):
        raise KeyboardInterrupt
    runtime.shutil.rmtree = interrupted
    try:
        runtime.activate(seven_root, ['activate'])
        raise AssertionError('the interrupt did not reach the prune')
    except KeyboardInterrupt:
        pass
    finally:
        runtime.shutil.rmtree = real_rmtree
    assert retained() == {six, seven}
    assert all((releases / name / 'namedrop').is_file() and (releases / name / 'runtime.json').is_file()
               for name in retained()), 'an interrupted prune leaves only whole releases'
    assert (rt / '.pruning' / five).is_dir()
    partial = rt / '.pruning' / ('c' * 64)
    (partial / 'nested').mkdir(parents=True)
    (partial / 'runtime.json').write_text('{}')
    (rt / '.pruning' / 'link').symlink_to(outside)
    assert activate(seven_root) == seven
    assert retained() == {six, seven} and not any((rt / '.pruning').iterdir()), 'the next prune clears what was left'
    settled = tree(rt)
    assert activate(seven_root) == seven and tree(rt) == settled, 'pruning again changes nothing'
    assert unrelated() == unrelated_before, 'nothing but old releases is touched'
    assert tree(outside) == outside_before, 'symbolic link targets are untouched'

    (rt / 'migration.json').write_text('{}')
    try:
        runtime.prune_releases(rt, {seven})
        assert retained() == {six, seven}, 'an unfinished migration prunes nothing'
    finally:
        (rt / 'migration.json').unlink()


def main():
    actual = Path(os.environ['NAMEDROP_TEST_BINARY'])
    declared = json.loads((REPO / 'runtime.json').read_text())
    assert json.loads(subprocess.check_output([actual, '--runtime-info'])) == declared
    assert declared['state_schema'] == 8 and declared['collaboration_protocol'] == 'collab/v1'
    subprocess.run([actual, '--validate-update-manifest', REPO / 'herdr-plugin.toml'], check=True)
    with tempfile.TemporaryDirectory(prefix='namedrop runtime ') as directory:
        base = Path(directory)
        os.environ.update(HOME=str(base/'home'), CLAUDE_CONFIG_DIR=str(base/'home/.claude'), CODEX_HOME=str(base/'home/.codex'))
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
        check_pruning(base, release)
    print('runtime lifecycle checks passed')


if __name__ == '__main__':
    main()
