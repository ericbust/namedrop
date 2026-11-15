"""U4 activation contracts, fixture differential, and release upgrade lane.

Run directly for the local checks. The upgrade lane is exposed through
update_lifecycle.py --upgrade-from TAG and uses NAMEDROP_TEST_PREVIOUS_BINARY
and NAMEDROP_TEST_BINARY built from that tag and this checkout respectively.
"""
import contextlib
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import socket as socketlib
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import unittest
from unittest import mock

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / 'scripts'))
import runtime
import update

HOST = '''#!{python}
import json, os, pathlib, sys
args = sys.argv[1:]
base = pathlib.Path(os.environ['FIXTURE'])
with (base/'calls').open('a') as log: log.write(json.dumps(args)+'\\n')
def out(value): print(json.dumps({{'result':value}}))
pane = dict(pane_id='p', workspace_id='w', tab_id='t', terminal_id='terminal')
key = args[:2]
if key == ['session','list']: print(json.dumps({{'sessions':[]}}))
elif key == ['pane','list']: out({{'panes':[pane]}})
elif key == ['pane','process-info']:
    out({{'process_info':dict(pane_id='p',shell_pid=int(os.environ['SHELLPID']),
          foreground_process_group_id=int(os.environ['SHELLPID']),foreground_processes=[])}})
elif key == ['worktree','list']: out({{'worktrees':[dict(path=str(base/'workspace'),open_workspace_id='w')]}})
elif key == ['workspace','list']: out({{'workspaces':[dict(workspace_id='w',label='fixture')]}})
elif key == ['workspace','get']: out({{'workspace':dict(workspace_id='w',label='fixture')}})
elif key == ['tab','get']: out({{'tab':dict(tab_id='t',label='fixture')}})
elif key == ['status','server']: print('running: yes\\nsocket: '+os.environ['HERDR_SOCKET_PATH'])
else: print('{{}}')
'''


def environment(base):
    env = {k: v for k, v in os.environ.items()
           if not k.startswith(('HERDR_', 'REX_', 'CODEX_', 'CLAUDE', 'NAMEDROP_', 'GIT_CONFIG_'))}
    tools = base / 'tools'
    tools.mkdir()
    host = tools / 'herdr'
    host.write_text(HOST.format(python=sys.executable))
    host.chmod(0o700)
    (base / 'home').mkdir()
    (base / 'state').mkdir()
    (base / 'workspace').mkdir()
    env.update(HOME=str(base / 'home'), PATH=str(tools) + ':/usr/bin:/bin',
               XDG_CONFIG_HOME=str(base / 'home/.config'), XDG_STATE_HOME=str(base / 'home/.local/state'),
               XDG_RUNTIME_DIR=str(base / 'run'), CODEX_HOME=str(base / 'home/.codex'),
               CLAUDE_CONFIG_DIR=str(base / 'home/.claude'), FIXTURE=str(base), SHELLPID=str(os.getpid()),
               HERDR_BIN_PATH=str(host), HERDR_SOCKET_PATH=str(base / 'herdr.sock'),
               HERDR_PANE_ID='p', HERDR_WORKSPACE_ID='w', HERDR_PLUGIN_STATE_DIR=str(base / 'state'))
    return env


def schema(path):
    with sqlite3.connect(path) as db:
        for file in ('src/store.rs', 'src/store/coordination.rs'):
            sql = (REPO / file).read_text().split('const SCHEMA: &str = r#"', 1)[1].split('"#;', 1)[0]
            db.executescript(sql)
        db.execute('PRAGMA user_version=8')


@contextlib.contextmanager
def rex_server():
    """A listening Unix socket, as a running Rex server's, under /tmp so the
    path fits sun_path."""
    directory = tempfile.mkdtemp(prefix='nd-u4-', dir='/tmp')
    path = os.path.join(directory, 'rex.sock')
    try:
        with contextlib.closing(socketlib.socket(socketlib.AF_UNIX, socketlib.SOCK_STREAM)) as listener:
            listener.bind(path)
            listener.listen(16)
            yield path
    finally:
        shutil.rmtree(directory)


def rex_agent(path, endpoint, pane, *, alias, runtime, status):
    """An active binding of `pane` on `endpoint` with an alias, a registration
    and an observed session, the rows an agent's hooks leave in the store."""
    address, session = pane + '-address', pane + '-session'
    with contextlib.closing(sqlite3.connect(path)) as db, db:
        db.execute("INSERT INTO pane_addresses VALUES (?, 's', ?, 'role', 0, 0, 0)", (endpoint, address))
        db.execute("INSERT INTO pane_bindings VALUES (?, 's', ?, ?, ?, 1, 'tab', 5, NULL)",
                   (endpoint, pane + '-binding', address, pane))
        db.execute("INSERT INTO alias_claims VALUES (?, 's', ?, ?, 'active', NULL)", (endpoint, alias, address))
        db.execute("INSERT INTO integration_registrations VALUES (?, 's', ?, ?, ?, ?, 'v', 'p', 1)",
                   (endpoint, pane + '-registration', address, session, runtime))
        db.execute("INSERT INTO agent_session_observations VALUES (?, 's', ?, ?, ?, ?, 5)",
                   (endpoint, pane + '-observation', address, session, status))


def archive(ref, destination):
    repository = os.environ.get('NAMEDROP_TEST_REPOSITORY', str(REPO))
    contents = subprocess.check_output(['git', '-C', repository, 'archive', ref])
    with tarfile.open(fileobj=io.BytesIO(contents)) as source:
        source.extractall(destination, filter='data')


def release_tags():
    repository = os.environ.get('NAMEDROP_TEST_REPOSITORY', str(REPO))
    result = subprocess.run(['git', '-C', repository, 'tag', '--list', 'v*', '--sort=-v:refname'],
                            capture_output=True, text=True)
    return result.stdout.split() if result.returncode == 0 else []


# Run in its own interpreter: the release's update.py and runtime.py, with an
# installed checkout of that release, a published runtime, and a target ref
# that holds the release's contract and manifest and the given run.sh.
RELEASED_UPDATER = r'''
import json, os, shutil, subprocess, sys
from pathlib import Path
old, run_sh, work = map(Path, sys.argv[1:4])
def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()
installed = work / 'installed'
shutil.copytree(old / 'scripts', installed / 'scripts')
for name in ('runtime.json', 'herdr-plugin.toml'):
    shutil.copyfile(old / name, installed / name)
sys.path.insert(0, str(installed / 'scripts'))
import update, runtime
state = Path(os.environ['HERDR_PLUGIN_STATE_DIR'])
release = state / 'runtime/releases' / ('a' * 64)
release.mkdir(parents=True)
shutil.copyfile(old / 'runtime.json', release / 'runtime.json')
(release / 'namedrop').write_text('#!/bin/sh\nexit 0\n')
(release / 'namedrop').chmod(0o700)
(state / 'runtime/current').symlink_to(Path('releases') / ('a' * 64))
(state / 'runtime/namedrop').write_text(runtime.LAUNCHER)
(state / 'runtime/namedrop').chmod(0o700)
source = work / 'source'
(source / 'scripts').mkdir(parents=True)
git(source, 'init', '--quiet')
for name in ('runtime.json', 'herdr-plugin.toml'):
    shutil.copyfile(old / name, source / name)
shutil.copyfile(run_sh, source / 'scripts/run.sh')
git(source, 'add', '.')
git(source, '-c', 'user.name=fixture', '-c', 'user.email=fixture@example.invalid', 'commit', '--quiet', '-m', 'target')
git(source, 'tag', 'target')
git(work, 'clone', '--quiet', '--bare', str(source), str(work / 'origin.git'))
git(work / 'origin.git', 'config', 'uploadpack.allowFilter', 'true')
(work / 'inspect').mkdir()
target = update.prepare_target({'plugin_root': str(installed), 'source': {
    'owner': 'fixture', 'repo': 'source', 'requested_ref': 'target', 'managed_path': str(installed)}},
    work / 'inspect')
print(json.dumps(target['live']))
'''


def invoke(root, env, *, explicit=True):
    args = [sys.executable, str(root / 'scripts/runtime.py'), str(root)]
    if explicit:
        args += ['--state-dir', env['HERDR_PLUGIN_STATE_DIR'], '--verify', 'activate']
    return subprocess.run([*args, 'activate', '--input-json', '{"confirmed":true}'],
                          env=env, cwd=Path(env['FIXTURE']) / 'workspace', capture_output=True, text=True, timeout=60)


def fixture_release(root, *, number=1, state_schema=8, failure=False):
    (root / 'bin').mkdir(exist_ok=True)
    contract = dict(json.loads((REPO / 'runtime.json').read_text()), state_schema=state_schema,
                    command_gate=0, live_migrate_from=[])
    (root / 'runtime.json').write_text(json.dumps(contract))
    executable = root / 'bin/namedrop'
    executable.write_text(f'''#!{sys.executable}
import json, os, sqlite3, sys
if sys.argv[1:] == ['--runtime-info']:
    print({json.dumps(contract)!r})
else:
    with sqlite3.connect(os.environ['HERDR_PLUGIN_STATE_DIR']+'/namedrop.sqlite3') as db:
        db.execute('PRAGMA user_version={state_schema}')
    print(json.dumps(dict(status='ok',operation='activate',data=dict(activated=True,
        space_id='w',state_directory=os.environ['HERDR_PLUGIN_STATE_DIR'],
        integration_packages={{'codex':{{'installed':True}}}},
        agent_integration_hook_activation={{'codex':{{'cache_current':True,'migration_ready':True}}}}))))
    sys.exit({7 if failure else 0})
# release {number}
''')
    executable.chmod(0o700)


class ActivationChecks(unittest.TestCase):
    def test_explicit_state_and_owner(self):
        with tempfile.TemporaryDirectory(prefix='namedrop-u4-') as directory:
            base = Path(directory)
            env = environment(base)
            root = base / 'checkout'
            shutil.copytree(REPO / 'scripts', root / 'scripts')
            fixture_release(root)
            chosen = base / 'chosen'
            chosen.mkdir()
            args = [sys.executable, str(root / 'scripts/runtime.py'), str(root),
                    '--state-dir', str(chosen), '--verify', 'activate', 'activate']
            result = subprocess.run(args, env=env, cwd=base, capture_output=True, text=True, timeout=60)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)['data']['state_directory'], str(chosen))
            self.assertEqual((chosen / 'runtime/owner').read_text(), 'herdr\n')
            self.assertFalse(any((base / 'state').iterdir()))
            selected = os.readlink(chosen / 'runtime/current')
            fixture_release(root, number=2)
            foreign = base / 'foreign-owner'
            foreign.write_text('preserve')
            owner = chosen / 'runtime/owner'
            owner.unlink()
            owner.symlink_to(foreign)
            failed = subprocess.run(args, env=env, cwd=base, capture_output=True, text=True, timeout=60)
            self.assertEqual(failed.returncode, 2, failed.stderr)
            self.assertIn('Runtime file has an unexpected type', failed.stderr)
            self.assertEqual(os.readlink(chosen / 'runtime/current'), selected)
            self.assertEqual(foreign.read_text(), 'preserve')
            owner.unlink()
            args[args.index('activate')] = 'install'
            rejected = subprocess.run(args, env=env, cwd=base, capture_output=True, text=True, timeout=60)
            self.assertEqual((rejected.returncode, rejected.stdout, rejected.stderr),
                             (2, '', 'Namedrop runtime activation stopped: Unsupported runtime verification: install\n'),
                             '--verify install belongs to U8')
            self.assertEqual(os.readlink(chosen / 'runtime/current'), selected)
            self.assertFalse(owner.exists())

    def test_run_sh_shape_means_herdr_state_and_activate(self):
        with tempfile.TemporaryDirectory(prefix='namedrop-u4-') as directory:
            base = Path(directory)
            env = environment(base)
            root = base / 'checkout'
            shutil.copytree(REPO / 'scripts', root / 'scripts')
            fixture_release(root)
            result = invoke(root, env, explicit=False)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)['data']['state_directory'], env['HERDR_PLUGIN_STATE_DIR'])
            self.assertEqual((base / 'state/runtime/owner').read_text(), 'herdr\n')

    def test_run_sh_forwards_state_and_pane(self):
        with tempfile.TemporaryDirectory(prefix='namedrop-u4-') as directory:
            base = Path(directory)
            env = environment(base)
            capture = base / 'tools/python3'
            capture.write_text(f'#!{sys.executable}\nimport json,sys\nprint(json.dumps(sys.argv[1:]))\n')
            capture.chmod(0o700)
            (base / 'tools/dirname').symlink_to('/usr/bin/dirname')
            result = subprocess.run(['/bin/sh', str(REPO / 'scripts/run.sh'), 'activate', '--input-json',
                                     '{"confirmed":true}'], env=dict(env, PATH=str(base / 'tools')),
                                    capture_output=True, text=True, timeout=60)
            self.assertEqual(result.returncode, 0, result.stderr)
            args = json.loads(result.stdout)
            # The shape every release's run.sh passes; runtime.py reads it as
            # --state-dir "$HERDR_PLUGIN_STATE_DIR" --verify activate.
            self.assertEqual(args[1:], [str(REPO), '--herdr-pane', 'p', 'activate', '--input-json',
                                        '{"confirmed":true}'])

    def test_released_updaters_call_this_update_live(self):
        # A released updater calls an update live only when the target's
        # run.sh is byte for byte the installed one (scripts/update.py's
        # prepare_target), so run.sh must not change.
        refs = os.environ.get('NAMEDROP_TEST_RELEASE_REFS', '').split() or release_tags()[:1]
        if not refs:
            self.skipTest('no release tag in this clone')
        for ref in refs:
            with self.subTest(ref=ref), tempfile.TemporaryDirectory(prefix='namedrop-u4-live-') as directory:
                base = Path(directory)
                old = base / 'old'
                archive(ref, old)
                changed = base / 'changed-run.sh'
                changed.write_text((REPO / 'scripts/run.sh').read_text() + '# changed\n')
                verdicts = {}
                for name, run_sh in (('head', REPO / 'scripts/run.sh'), ('changed', changed)):
                    work = base / name
                    profile = base / (name + '-profile')
                    profile.mkdir()
                    env = environment(profile)
                    remote = work / 'origin.git'
                    env.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull, GIT_ALLOW_PROTOCOL='file',
                               GIT_CONFIG_COUNT='1', GIT_CONFIG_KEY_0=f'url.{remote.as_uri()}.insteadOf',
                               GIT_CONFIG_VALUE_0='https://github.com/fixture/source.git')
                    result = subprocess.run([sys.executable, '-I', '-c', RELEASED_UPDATER, str(old), str(run_sh),
                                             str(work)], env=env, capture_output=True, text=True, timeout=120)
                    self.assertEqual(result.returncode, 0, result.stderr)
                    verdicts[name] = json.loads(result.stdout)
                self.assertEqual(verdicts, {'head': True, 'changed': False})

    def test_rex_preflight(self):
        with tempfile.TemporaryDirectory(prefix='namedrop-u4-') as directory, rex_server() as listening:
            base = Path(directory)
            env = environment(base)
            path = base / 'state/namedrop.sqlite3'
            with mock.patch.dict(os.environ, env, clear=True):
                with self.assertRaisesRegex(RuntimeError, 'Cannot verify durable collaboration state'):
                    update.preflight()
            schema(path)
            socket = 'rex:' + listening
            rex_agent(path, socket, 'block', alias='alpha', runtime='claude-code', status='working')
            # A Herdr binding with an observed session: Herdr's own survey
            # reports it, so the store's copy must not count a second time.
            rex_agent(path, listening, 'herdr-pane', alias='beta', runtime='claude-code', status='idle')
            with mock.patch.dict(os.environ, env, clear=True):
                state, agents = update.preflight()
                self.assertEqual(state['session_count'], 1)
                self.assertEqual(state['stopped_rex_servers'], [])
                self.assertEqual([(p['_socket'], p['agent_session']['value'], p['label'], p['agent'], p['agent_status'])
                                  for p in agents], [(socket, 'block-session', 'alpha', 'claude', 'working')])
                self.assertFalse(update.ready(state, agents))
                with mock.patch.object(update, 'structured', return_value={'result': {'workspaces': [], 'tabs': []}}) as host:
                    metadata = update.labels(agents)
                    self.assertEqual([call.kwargs['socket'] for call in host.call_args_list],
                                     [env['HERDR_SOCKET_PATH']] * 2)
                shown = io.StringIO()
                with contextlib.redirect_stdout(shown):
                    update.show_agents(agents, metadata)
                self.assertIn(f'→ alpha (claude, working; block) [Rex: {socket}]', shown.getvalue())
                rt = base / 'state/runtime'
                rt.mkdir()
                migration = rt / 'legacy-sessions.json'
                migration.write_text(json.dumps(dict(sessions=[], codex_ready=True, packages_ready=True)))
                live_state, live_agents = update.preflight(live=True)
                self.assertEqual(live_state['session_count'], 1)
                self.assertTrue(update.ready(live_state, live_agents))
                migration.write_text(json.dumps(dict(sessions=[[socket, 'block-session']],
                                                     codex_ready=True, packages_ready=True)))
                self.assertFalse(update.ready(*update.preflight(live=True)))
                # Codex's setup still pending blocks only Codex sessions.
                migration.write_text(json.dumps(dict(sessions=[], codex_ready=False, packages_ready=True)))
                self.assertTrue(update.ready(*update.preflight(live=True)))
                with sqlite3.connect(path) as db:
                    db.execute("UPDATE integration_registrations SET runtime='codex'")
                self.assertFalse(update.ready(*update.preflight(live=True)))
                with sqlite3.connect(path) as db:
                    db.execute("INSERT INTO agent_session_observations VALUES (?, 's','ended','block-address','','unknown',5)", (socket,))
                self.assertTrue(update.ready(*update.preflight()))
                with sqlite3.connect(path) as db:
                    db.execute("DELETE FROM agent_session_observations WHERE observation_id='ended'")
                    db.execute('UPDATE pane_bindings SET active=0')
                self.assertTrue(update.ready(*update.preflight()))
                with sqlite3.connect(path) as db:
                    db.execute('UPDATE pane_bindings SET active=1, bound_at_ms=6')
                self.assertTrue(update.ready(*update.preflight()))
                with sqlite3.connect(path) as db:
                    db.execute('UPDATE pane_bindings SET bound_at_ms=5')
                    db.execute('UPDATE pane_addresses SET retired=1')
                self.assertTrue(update.ready(*update.preflight()))

    def test_rex_preflight_leaves_out_stopped_servers(self):
        with tempfile.TemporaryDirectory(prefix='namedrop-u4-') as directory, rex_server() as listening:
            base = Path(directory)
            env = environment(base)
            path = base / 'state/namedrop.sqlite3'
            schema(path)
            refused = str(Path(listening).with_name('stopped.sock'))
            with contextlib.closing(socketlib.socket(socketlib.AF_UNIX, socketlib.SOCK_STREAM)) as stopped:
                stopped.bind(refused)  # the file stays, and nothing listens on it
            missing = str(Path(listening).with_name('missing.sock'))
            for endpoint in (refused, missing):
                rex_agent(path, 'rex:' + endpoint, 'block', alias='alpha', runtime='claude-code', status='idle')
            with mock.patch.dict(os.environ, env, clear=True):
                state, agents = update.preflight()
                self.assertEqual((state['session_count'], agents), (0, []))
                self.assertEqual(state['stopped_rex_servers'], sorted(['rex:' + refused, 'rex:' + missing]))
                self.assertTrue(update.ready(state, agents))
                rex_agent(path, 'rex:' + listening, 'block', alias='alpha', runtime='claude-code', status='idle')
                state, agents = update.preflight()
                self.assertEqual([p['_socket'] for p in agents], ['rex:' + listening])
                self.assertFalse(update.ready(state, agents))

    def test_unknown_store_schema_keeps_its_message(self):
        with tempfile.TemporaryDirectory(prefix='namedrop-u4-') as directory:
            base = Path(directory)
            env = environment(base)
            root = base / 'checkout'
            shutil.copytree(REPO / 'scripts', root / 'scripts')
            fixture_release(root)
            self.assertEqual(invoke(root, env, explicit=False).returncode, 0)
            (base / 'state/namedrop.sqlite3').write_bytes(b'')  # a store file with no tables
            fixture_release(root, number=2, state_schema=9)
            result = invoke(root, env, explicit=False)
            self.assertEqual(result.returncode, 2, result.stderr)
            self.assertEqual(result.stderr, 'Namedrop runtime activation stopped: Unknown collaboration database '
                             'schema; use the matching Namedrop release to inspect unfinished work.\n')

    def test_fixture_differential(self):
        ref = os.environ.get('NAMEDROP_TEST_BASE_REF', 'cca00814')
        with tempfile.TemporaryDirectory(prefix='namedrop-u4-diff-') as directory:
            base = Path(directory)
            old = base / 'old'
            archive(ref, old)
            fixture = base / 'fixture'
            results = []
            # Herdr runs run.sh's shape; the explicit options must mean the same.
            for source, explicit in ((old, False), (REPO, False), (REPO, True)):
                fixture.mkdir()
                env = environment(fixture)
                root = fixture / 'checkout'
                shutil.copytree(source / 'scripts', root / 'scripts')
                schema(fixture / 'state/namedrop.sqlite3')
                snapshots = []
                for number, version, failure in ((1, 8, False), (2, 8, False), (3, 9, False), (4, 9, True)):
                    fixture_release(root, number=number, state_schema=version, failure=failure)
                    result = invoke(root, env, explicit=explicit)
                    self.assertEqual(result.returncode, 7 if failure else 0, result.stderr)
                    files = {}
                    for path in sorted((fixture / 'state').rglob('*')):
                        if path.is_symlink():
                            contents = os.readlink(path)
                        elif path.is_file():
                            if path.name == 'owner':
                                self.assertIs(source, REPO)
                                self.assertEqual(path.read_text(), 'herdr\n')
                                continue
                            contents = path.read_bytes()
                        else:
                            continue
                        files[str(path.relative_to(fixture))] = contents
                    if source is REPO:
                        self.assertEqual((fixture / 'state/runtime/owner').read_text(), 'herdr\n')
                    snapshots.append((result.returncode, result.stdout, result.stderr, files,
                                      (fixture / 'calls').read_bytes()))
                results.append(snapshots)
                shutil.rmtree(fixture)
            self.assertEqual(results[0], results[1], 'publish/live/schema/rollback differ beyond owner')
            self.assertEqual(results[1], results[2], 'the explicit options differ from run.sh\'s shape')


def upgrade_from(ref):
    binary = Path(os.environ['NAMEDROP_TEST_BINARY']).resolve()
    previous = Path(os.environ['NAMEDROP_TEST_PREVIOUS_BINARY']).resolve()
    with tempfile.TemporaryDirectory(prefix='namedrop-u4-upgrade-') as directory:
        base = Path(directory)
        old = base / 'old'
        archive(ref, old)
        (old / 'bin').mkdir(exist_ok=True)
        shutil.copy2(previous, old / 'bin/namedrop')
        new = base / 'new'
        shutil.copytree(REPO / 'scripts', new / 'scripts')
        shutil.copy2(REPO / 'runtime.json', new / 'runtime.json')
        (new / 'bin').mkdir()
        shutil.copy2(binary, new / 'bin/namedrop')
        env = environment(base)
        old_contract = json.loads(subprocess.check_output([previous, '--runtime-info'], env=env))
        assert old_contract == json.loads((old / 'runtime.json').read_text())
        assert old_contract['state_schema'] == 8
        first = invoke(old, env, explicit=False)
        assert first.returncode == 0, (first.stdout, first.stderr)
        launcher = base / 'state/runtime/namedrop'
        args = [str(launcher), '--herdr-socket', env['HERDR_SOCKET_PATH'], '--herdr-workspace', 'w',
                '--herdr-pane', 'p', '--json']
        named = subprocess.run([*args, 'name', 'survivor', '--type', 'terminal'], env=env, cwd=base / 'workspace',
                               capture_output=True, text=True, timeout=60)
        assert named.returncode == 0, (named.stdout, named.stderr)
        def names():
            result = subprocess.run([*args, 'address', 'list'], env=env, cwd=base / 'workspace',
                                    capture_output=True, text=True, timeout=60)
            assert result.returncode == 0, (result.stdout, result.stderr)
            return json.loads(result.stdout)['data']
        before = names()
        assert 'survivor' in json.dumps(before)
        with sqlite3.connect(base / 'state/namedrop.sqlite3') as db:
            rows = db.execute('SELECT * FROM pane_addresses ORDER BY address_id').fetchall()
            claims = db.execute('SELECT * FROM alias_claims ORDER BY alias').fetchall()
        result = invoke(new, env, explicit=False)
        assert result.returncode == 0, (result.stdout, result.stderr)
        expected = 'releases/' + hashlib.sha256(binary.read_bytes()).hexdigest()
        assert os.readlink(base / 'state/runtime/current') == expected
        assert json.loads(result.stdout)['data']['runtime']['live_update'] is True
        assert names() == before
        with sqlite3.connect(base / 'state/namedrop.sqlite3') as db:
            assert db.execute('PRAGMA user_version').fetchone()[0] == 8
            assert db.execute('SELECT * FROM pane_addresses ORDER BY address_id').fetchall() == rows
            assert db.execute('SELECT * FROM alias_claims ORDER BY alias').fetchall() == claims
        assert (base / 'state/runtime/owner').read_text() == 'herdr\n'
        print(f'upgrade from {ref}: names and schema-8 store preserved; owner=herdr')


if __name__ == '__main__':
    unittest.main()
