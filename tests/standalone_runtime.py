"""U8 publication contracts through isolated scripted hosts and real binaries."""
import contextlib
import fcntl
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
from unittest import mock

BINARY = Path(sys.argv.pop(1)).resolve()
REPO = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(REPO / 'tests'))
from runtime_activation import environment, schema, rex_agent, rex_server
sys.path.insert(0, str(REPO / 'scripts'))
import runtime
import update
import runtime_install
from runtime_install import Install
from support.rex_control import Server


def fixture(root, number=1, state_schema=8, failure=False, live=True):
    """A release whose `namedrop` is a script. Each FIXTURE_* variable makes
    one of its steps act as a failure or a concurrent writer would."""
    (root / 'bin').mkdir(parents=True, exist_ok=True)
    if not (root / 'scripts').exists():
        shutil.copytree(REPO / 'scripts', root / 'scripts')
    (root / 'herdr-plugin.toml').write_text(f'version = "1.7.{number}"\n')
    contract = dict(json.loads((REPO / 'runtime.json').read_text()), state_schema=state_schema,
                    live_migrate_from=([8] if live else []) if state_schema == 9 else [3, 4, 5, 6, 7])
    (root / 'runtime.json').write_text(json.dumps(contract))
    executable = root / 'bin/namedrop'
    executable.write_text(f'''#!{sys.executable}
import fcntl, json, os, pathlib, signal, sqlite3, sys, time
args = sys.argv[1:]
state = pathlib.Path(os.environ['NAMEDROP_STATE_DIR'])
knob = os.environ.get
if knob('FIXTURE_LOG'):
    # Which step ran, and whether the state was retired while it did.
    step = 'sessions' if 'sessions' in args else 'install' if 'install' in args else args[0]
    with open(knob('FIXTURE_LOG'), 'a') as log:
        log.write(step + (' retired' if (state/'runtime/retired.json').exists() else ' live') + '\\n')
def emit(op, data): print(json.dumps(dict(status='ok', operation=op, data=data)))
def refuse_retired():
    # As `main` does before any command but --runtime-info and the migration.
    if (state/'runtime/retired.json').exists():
        sys.exit('Namedrop was removed from Rex. Run sh scripts/install.sh to install it again.')
def initialize():
    sys.path.insert(0, {str(REPO / 'tests')!r})
    from runtime_activation import schema
    schema(state/'namedrop.sqlite3')
if args == ['--runtime-info']:
    print({json.dumps(contract)!r})
elif args[:1] == ['--migrate-runtime-state']:
    if knob('FIXTURE_MIGRATE_FAIL') and pathlib.Path(knob('FIXTURE_MIGRATE_FAIL')).exists():
        pathlib.Path(knob('FIXTURE_MIGRATE_FAIL')).unlink()
        initialize()
        sys.exit('Runtime state migration failed: fixture stopped after creating the store')
    if not (state/'namedrop.sqlite3').exists():
        initialize()
    with sqlite3.connect(state/'namedrop.sqlite3') as db:
        found = db.execute('PRAGMA user_version').fetchone()[0]
        if found > {state_schema}:
            sys.exit(f'Runtime state migration failed: database schema {{found}} is not supported; expected {state_schema}')
        db.execute('PRAGMA user_version={state_schema}')
    print('{{"state_schema":{state_schema}}}')
elif 'sessions' in args:
    refuse_retired()
    if knob('FIXTURE_FAIL') == 'survey':
        sys.exit('handshake closed')
    endpoint = args[args.index('--endpoint')+1] if '--endpoint' in args else os.environ['SURVEY_ENDPOINT']
    panes = json.loads(os.environ.get('SURVEY_PANES', '[]'))
    emit('rex.sessions', dict(endpoint=endpoint, panes=[p for p in panes if p['endpoint']==endpoint]))
else:
    assert 'install' in args and '--plugin-state' in args and '--input-json' in args, args
    assert args[args.index('--input-json')+1] == '{{"confirmed":true}}'
    refuse_retired()
    if knob('FIXTURE_RETIRE'):
        (state/'runtime/.retire').write_text(knob('FIXTURE_RETIRE'))
        (state/'runtime/.retire').rename(state/'runtime/retired.json')
    if knob('FIXTURE_SIGNAL'):
        os.kill(os.getppid(), getattr(signal, 'SIG' + knob('FIXTURE_SIGNAL')))
        time.sleep(30)
    if knob('FIXTURE_HOLD_GATE'):
        release = pathlib.Path(knob('FIXTURE_HOLD_GATE'))
        if os.fork() == 0:
            gate = open(state/'runtime/commands.lock', 'a+')
            fcntl.flock(gate, fcntl.LOCK_SH)
            quiet = os.open(os.devnull, os.O_RDWR)
            for stream in (0, 1, 2): os.dup2(quiet, stream)
            release.with_suffix('.pid').write_text(str(os.getpid()))
            deadline = time.monotonic() + 30
            while not release.exists() and time.monotonic() < deadline: time.sleep(.05)
            os._exit(0)
        while not release.with_suffix('.pid').exists(): time.sleep(.01)
    if {failure!r}: sys.exit(7)
    home = pathlib.Path(os.environ['HOME'])
    module = home/'rex/namedrop.lua';module.parent.mkdir(exist_ok=True);module.write_text('-- managed fixture\\n')
    record = dict(state='/foreign' if knob('FIXTURE_FAIL') == 'record' else str(state),
                  launcher=str(state/'runtime/namedrop'), module=str(module),
                  config=str(home/'rex/init.lua'), line='inserted', server=os.environ['SURVEY_ENDPOINT'])
    path = home/'.config/namedrop/rex.json';path.parent.mkdir(parents=True, exist_ok=True);path.write_text(json.dumps(record))
    emit('install', dict(state_directory=str(state), module=str(module), config=record['config'], line=record['line'], confirmed=True,
                        integration_packages={{'codex':{{'installed':True}}, 'claude-code':{{'installed':True}}}},
                        agent_integration_hook_activation={{'codex':{{'cache_current':True,'migration_ready':True}}}}))
# fixture {number}
''')
    executable.chmod(0o700)


def invoke(root, env, *extra):
    return subprocess.run([sys.executable, str(root / 'scripts/runtime.py'), str(root), '--state-dir',
                           env['HERDR_PLUGIN_STATE_DIR'], '--verify', 'install', *extra],
                          env=env, capture_output=True, text=True, timeout=55)


def snapshot(path):
    return {str(p.relative_to(path)): os.readlink(p) if p.is_symlink() else hashlib.sha256(p.read_bytes()).hexdigest()
            for p in path.rglob('*') if p.is_file() or p.is_symlink()}


def unpacked(state, root):
    """The release a refused install leaves unpacked: only it may differ."""
    return 'runtime/releases/' + hashlib.sha256((root / 'bin/namedrop').read_bytes()).hexdigest() + '/'


def apart(files, prefix):
    return {name: digest for name, digest in files.items() if not name.startswith(prefix)}


# A herdr on PATH that records every call and fails it, so a call shows even
# where its caller ignores the failure.
TRAP = '''#!/bin/sh
printf '%s\\n' "$*" >> "$FIXTURE/calls"
echo "herdr: no Herdr here" >&2
exit 127
'''
REMOVED = '{"at": "2026-11-15T00:00:00.123456789Z", "reason": "removed", "successor": null}\n'


class StandaloneChecks(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='nd-u8-', dir='/tmp')
        self.addCleanup(self.directory.cleanup)
        self.base = Path(self.directory.name)
        self.env = environment(self.base)
        (self.base / 'tools/herdr').write_text(TRAP)
        for name in list(self.env):
            if name.startswith('HERDR_') and name != 'HERDR_PLUGIN_STATE_DIR':
                del self.env[name]
        self.env['NAMEDROP_STATE_DIR'] = self.env['HERDR_PLUGIN_STATE_DIR']
        sock = self.enterContext(rex_server())
        self.env['SURVEY_ENDPOINT'] = 'rex:' + sock
        self.env['REX_SERVER'] = 'unix://' + sock
        self.root = self.base / 'checkout'
        fixture(self.root)
        schema(self.base / 'state/namedrop.sqlite3')

    def publish(self):
        result = invoke(self.root, self.env)
        self.assertEqual(result.returncode, 0, (result.stdout, result.stderr))
        return self.base / 'state/runtime'

    def retire(self, selected):
        marker = selected / 'retired.json'
        marker.write_text(REMOVED)
        return marker

    def assertRetired(self, selected, result, marker=REMOVED):
        self.assertEqual((selected / 'retired.json').read_text(), marker, result and (result.stdout, result.stderr))
        self.assertFalse((selected / 'retired.aside.json').exists())

    def real_release(self):
        """The built binary as the release, with the control-protocol fixture as Rex."""
        shutil.copy2(BINARY, self.root / 'bin/namedrop')
        shutil.copy2(REPO / 'runtime.json', self.root / 'runtime.json')
        (self.base / 'state/namedrop.sqlite3').unlink()
        config = self.base / 'home/.config/rex/init.lua'
        config.parent.mkdir(parents=True)
        config.write_text('-- personal config\n')
        server = Server(self.base, config)
        self.addCleanup(server.close)
        self.env['REX_SERVER'] = 'unix://' + str(server.socket)
        self.env['SURVEY_ENDPOINT'] = 'rex:' + str(server.socket)
        return server

    def remove(self, selected):
        env = dict(self.env, NAMEDROP_STATE_DIR=str(selected.parent))
        removed = subprocess.run([str(selected / 'namedrop'), '--json', 'install', '--remove', '--input-json',
                                  '{"confirmed":true}'], env=env, capture_output=True, text=True, timeout=15)
        self.assertEqual(removed.returncode, 0, (removed.stdout, removed.stderr))
        return (selected / 'retired.json').read_text()

    def assertRemoved(self, selected):
        env = dict(self.env, NAMEDROP_STATE_DIR=str(selected.parent))
        described = subprocess.run([str(selected / 'namedrop'), '--json', 'describe'], env=env,
                                   capture_output=True, text=True, timeout=15)
        self.assertEqual((described.returncode, described.stderr),
                         (69, 'Namedrop was removed from Rex. Run sh scripts/install.sh to install it again.\n'))

    def test_no_herdr_records_store_and_survey_and_source(self):
        path = self.base / 'state/namedrop.sqlite3'
        with rex_server() as sock:
            endpoint = 'rex:' + sock
            rex_agent(path, endpoint, 'stored', alias='survivor', runtime='codex', status='working')
            self.env['SURVEY_ENDPOINT'] = endpoint
            self.env['REX_SERVER'] = 'unix://' + sock
            self.env['SURVEY_PANES'] = json.dumps([dict(endpoint=endpoint, space='s', block='new',
                                                      agent='claude-code', session='survey-session', status='working')])
            selected = self.publish()
        record = json.loads((selected / 'legacy-sessions.json').read_text())
        self.assertEqual(record, dict(sessions=[[endpoint, 'stored-session'], [endpoint, 'survey-session']],
                                      codex_ready=True, packages_ready=True))
        self.assertEqual((selected / 'owner').read_text(), 'standalone\n')
        self.assertEqual((selected / 'owner').stat().st_mode & 0o777, 0o600)
        self.assertEqual(json.loads((selected / 'source.json').read_text()),
                         dict(version='1.7.1', kind='archive', origin=str(self.root.resolve())))
        self.assertFalse((self.base / 'calls').exists())

    def test_mistyped_explicit_call_cannot_publish_herdr_state(self):
        for words in (['--verify', 'install', '--state-dir', str(self.base / 'other')], ['install'], ['--verifiy', 'activate'], []):
            with self.subTest(words=words):
                before = snapshot(self.base / 'state')
                result = subprocess.run([sys.executable, str(self.root / 'scripts/runtime.py'), str(self.root), *words],
                                        env=self.env, capture_output=True, text=True)
                self.assertEqual(result.returncode, 2)
                self.assertIn('Expected activate or --herdr-pane', result.stderr)
                self.assertEqual(snapshot(self.base / 'state'), before)

    def test_herdr_owner_and_handoff_are_refused(self):
        selected = self.publish()
        for explicit_owner in (True, False):
            owner = selected / 'owner'
            owner.write_text('herdr\n') if explicit_owner else owner.unlink()
            before = snapshot(selected)
            result = invoke(self.root, self.env)
            self.assertEqual(result.returncode, 2)
            self.assertIn('ND-INSTALL-002', result.stderr)
            self.assertEqual(snapshot(selected), before)
        (selected / 'owner').write_text('standalone\n')
        (selected / 'retired.json').write_text(json.dumps(dict(reason='herdr_handoff', successor='/new', at='now')))
        for mode in ('install', 'activate'):
            before = snapshot(selected)
            args = [sys.executable, str(self.root / 'scripts/runtime.py'), str(self.root), '--state-dir',
                    str(selected.parent), '--verify', mode, *(['activate'] if mode == 'activate' else [])]
            result = subprocess.run(args, env=self.env, capture_output=True, text=True)
            self.assertEqual(result.returncode, 2)
            self.assertIn('Namedrop is moving to Herdr', result.stderr)
            self.assertEqual(snapshot(selected), before)

    def test_schema_change_refuses_survey_agent_and_preserves_store(self):
        selected = self.publish()
        before = snapshot(selected.parent)
        endpoint = self.env['SURVEY_ENDPOINT']
        self.env['SURVEY_PANES'] = json.dumps([dict(endpoint=endpoint, space='s', block='busy',
                                                  agent='codex', session=None, status='working')])
        fixture(self.root, number=2, state_schema=9)
        result = invoke(self.root, self.env)
        self.assertEqual(result.returncode, 2, result.stdout)
        self.assertIn('Some Rex sessions still use pre-launcher tools', result.stderr)
        self.assertIn(f'An agent (codex, working) in block busy on {endpoint}', result.stderr)
        self.assertEqual(apart(snapshot(selected.parent), unpacked(selected.parent, self.root)), before)

    def test_store_socket_failures_are_counted_except_missing_and_refused(self):
        self.publish()
        path = self.base / 'state/namedrop.sqlite3'
        path.unlink();schema(path)
        # The last is a Herdr server's: a standalone state counts every endpoint's sessions.
        endpoints = ['rex:/tmp/u8-' + kind + '.sock' for kind in ('missing', 'refused', 'denied', 'timeout')] + ['/tmp/u8-herdr.sock']
        for i, endpoint in enumerate(endpoints):
            rex_agent(path, endpoint, str(i), alias='a'+str(i), runtime='codex', status='working')
        checks = dict(zip(endpoints, ('stopped', 'stopped', 'Permission denied', 'timed out', None)))
        with mock.patch.dict(os.environ, self.env, clear=True), mock.patch.object(update, 'rex_server_check', side_effect=checks.get):
            policy = Install(self.root, self.base / 'state', self.root / 'bin/namedrop')
            state, agents = policy.preflight()
            self.assertEqual({p['_socket'] for p in agents}, set(endpoints[2:]))
            self.assertEqual(state['stopped_rex_servers'], endpoints[:2])
            notices = '\n'.join(update.rex_notices(state))
            self.assertIn('Permission denied', notices);self.assertIn('timed out', notices)
            self.assertFalse(update.ready(state, agents))

    def test_removed_marker_is_cleared_under_both_locks_before_install(self):
        selected = self.publish()
        marker = selected / 'retired.json'
        marker.write_text('{"reason":"removed","successor":null,"at":"now"}')
        real_gate = runtime.command_gate
        saw = []
        @contextlib.contextmanager
        def checked_gate(directory, *args, **kwargs):
            with real_gate(directory, *args, **kwargs) as gate:
                self.assertTrue(marker.exists())
                for name in ('activation.lock', 'commands.lock'):
                    with (selected / name).open('r+') as other:
                        with self.assertRaises(BlockingIOError):
                            fcntl.flock(other, fcntl.LOCK_EX | fcntl.LOCK_NB)
                yield gate
                self.assertFalse(marker.exists())
                saw.append(True)
        with mock.patch.dict(os.environ, self.env, clear=True), mock.patch.object(runtime, 'command_gate', checked_gate), \
                contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(runtime.activate(self.root, [], verify='install'), 0)
        self.assertEqual(saw, [True])
        self.assertFalse(marker.exists())
        self.assertFalse((selected / 'retired.aside.json').exists())

    def test_install_verification_checks_packages_state_and_registration(self):
        result = invoke(self.root, self.env)
        self.assertEqual(result.returncode, 0, result.stderr)
        payload = json.loads(result.stdout)
        path = self.base / 'home/.config/namedrop/rex.json'
        record = json.loads(path.read_text())
        policy = Install(self.root, self.base / 'state', self.root / 'bin/namedrop')
        with mock.patch.dict(os.environ, self.env, clear=True):
            policy.verify(payload)
            for key, bad in (('state', '/foreign'), ('launcher', '/foreign/namedrop'),
                             ('module', '/foreign/module.lua'), ('config', '/foreign/init.lua'),
                             ('line', 'printed'), ('server', 'not-rex')):
                path.write_text(json.dumps(dict(record, **{key: bad})))
                with self.subTest(record=key), self.assertRaises(RuntimeError):
                    policy.verify(payload)
            path.write_text(json.dumps(record))
            for key, bad in (('state_directory', '/foreign'), ('integration_packages', {}), ('confirmed', False)):
                changed = dict(payload, data=dict(payload['data'], **{key: bad}))
                with self.subTest(payload=key), self.assertRaises(RuntimeError):
                    policy.verify(changed)
            changed = json.loads(json.dumps(payload))
            changed['data']['integration_packages']['codex']['installed'] = False
            with self.assertRaises(RuntimeError):
                policy.verify(changed)

    @unittest.skipUnless(shutil.which('git'), 'needs git')
    def test_checkout_source_records_repository_origin_and_version(self):
        checkout = self.base / 'clone'
        fixture(checkout, number=5)
        for origin, public in (('https://ericbust:ghp_secret@github.com/o/namedrop.git', 'https://github.com/o/namedrop.git'),
                               ('ssh://git:secret@github.com/o/namedrop.git', 'ssh://git@github.com/o/namedrop.git'),
                               ('git@github.com:o/namedrop.git', 'git@github.com:o/namedrop.git')):
            with self.subTest(origin=origin):
                shutil.rmtree(checkout / '.git', ignore_errors=True)
                for command in (['init', '-q'], ['remote', 'add', 'origin', origin]):
                    subprocess.run(['git', '-C', str(checkout), *command], env=self.env, check=True, capture_output=True)
                with mock.patch.dict(os.environ, self.env, clear=True):
                    source = Install(checkout, self.base / 'state', checkout / 'bin/namedrop').source()
                self.assertEqual(source, dict(version='1.7.5', kind='checkout', origin=public))

    def test_stale_registration_skips_its_missing_server_and_surveys_the_target(self):
        server = Server(self.base, self.base / 'home/init.lua')
        self.addCleanup(server.close)
        self.env['REX_SERVER'] = 'unix://' + str(server.socket)
        path = self.base / 'home/.config/namedrop/rex.json'
        path.parent.mkdir(parents=True)
        missing = 'rex:' + str(self.base / 'missing.sock')
        path.write_text(json.dumps(dict(server=missing)))
        policy = Install(REPO, self.base / 'state', BINARY)
        shown = io.StringIO()
        with mock.patch.dict(os.environ, self.env, clear=True), contextlib.redirect_stderr(shown):
            panes, unknown = policy.session_panes()
        self.assertEqual((panes, unknown), ([], []))
        self.assertIn(missing, shown.getvalue())
        self.assertIn('not running', shown.getvalue())
        self.assertEqual(server.calls, ['server.status', 'session.list'])

    def test_uncheckable_registered_server_without_store_rows_remains_unverified(self):
        path = self.base / 'home/.config/namedrop/rex.json'
        path.parent.mkdir(parents=True)
        endpoint = 'rex:' + str(self.base / 'denied.sock')
        path.write_text(json.dumps(dict(server=endpoint)))
        policy = Install(self.root, self.base / 'state', self.root / 'bin/namedrop')
        with mock.patch.dict(os.environ, self.env, clear=True), \
                mock.patch.object(runtime_install, 'rex_server_check', side_effect=lambda e: 'Permission denied' if e == endpoint else None):
            state, agents = policy.preflight()
        self.assertEqual(agents, [])
        self.assertEqual(state['other_servers'], [endpoint])
        self.assertIn('Permission denied', '\n'.join(update.rex_notices(state)))
        self.assertFalse(update.ready(state, agents))
        self.assertIn(f'The Rex server at {endpoint}, which could not be checked', Install.refusal(state, agents))
        with mock.patch.dict(os.environ, self.env, clear=True), contextlib.redirect_stderr(io.StringIO()), \
                mock.patch.object(runtime_install, 'rex_server_check', side_effect=lambda e: 'Permission denied' if e == endpoint else None), \
                self.assertRaisesRegex(RuntimeError, 'Could not check the Rex server at ' + endpoint):
            policy.session_panes()

    def test_same_schema_failure_rolls_back_source_and_runtime(self):
        selected = self.publish()
        before = os.readlink(selected / 'current')
        source = (selected / 'source.json').read_bytes()
        fixture(self.root, number=2, failure=True)
        result = invoke(self.root, self.env)
        self.assertEqual(result.returncode, 7)
        self.assertIn('Installation did not complete. The previous runtime remains selected', result.stderr)
        self.assertEqual(os.readlink(selected / 'current'), before)
        self.assertEqual((selected / 'source.json').read_bytes(), source)

    def test_source_write_failure_restores_previous_same_schema_runtime(self):
        selected = self.publish()
        before = os.readlink(selected / 'current')
        foreign = self.base / 'foreign-source'
        foreign.write_text('preserve')
        source = selected / 'source.json'
        source.unlink();source.symlink_to(foreign)
        fixture(self.root, number=2)
        result = invoke(self.root, self.env)
        self.assertEqual(result.returncode, 2)
        self.assertIn('Runtime file has an unexpected type', result.stderr)
        self.assertEqual(os.readlink(selected / 'current'), before)
        self.assertEqual(foreign.read_text(), 'preserve')

    def test_failed_install_after_migration_keeps_new_schema_and_source(self):
        selected = self.publish()
        before = os.readlink(selected / 'current')
        fixture(self.root, number=2, state_schema=9, failure=True)
        result = invoke(self.root, self.env)
        self.assertEqual(result.returncode, 7, (result.stdout, result.stderr))
        self.assertIn('new runtime remains selected', result.stderr)
        self.assertNotEqual(os.readlink(selected / 'current'), before)
        with contextlib.closing(sqlite3.connect(selected.parent / 'namedrop.sqlite3')) as db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], 9)
        self.assertEqual(json.loads((selected / 'source.json').read_text())['version'], '1.7.2')

    def test_initial_store_migration_recovers_after_interruption(self):
        selected = self.base / 'state/runtime'
        store = self.base / 'state/namedrop.sqlite3'
        store.unlink()
        # A migration that failed and left no store is withdrawn: nothing is selected.
        with mock.patch.dict(os.environ, self.env, clear=True), \
                mock.patch.object(runtime, 'migrate_selected', side_effect=RuntimeError('interrupted initialization')):
            with self.assertRaisesRegex(RuntimeError, 'interrupted initialization'):
                runtime.activate(self.root, [], verify='install')
        self.assertFalse((selected / 'migration.json').exists())
        self.assertFalse((selected / 'current').is_symlink())
        self.assertFalse(store.exists())
        self.assertEqual((selected / 'owner').read_text(), 'standalone\n')
        # One that changed the store keeps its record, and the next run finishes it.
        flag = self.base / 'fail-migration'
        flag.touch()
        result = invoke(self.root, dict(self.env, FIXTURE_MIGRATE_FAIL=str(flag)))
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn('fixture stopped after creating the store', result.stderr)
        self.assertEqual(json.loads((selected / 'migration.json').read_text())['previous'], None)
        self.publish()
        self.assertFalse((selected / 'migration.json').exists())
        with contextlib.closing(sqlite3.connect(store)) as db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], 8)

    def test_real_incoming_survey_bypasses_current_and_gate_without_writing(self):
        selected = self.base / 'state/runtime'
        (selected / 'releases/old').mkdir(parents=True)
        (selected / 'current').symlink_to('releases/old')
        (selected / 'namedrop').write_text('#!/bin/sh\nexit 77\n')
        (selected / 'namedrop').chmod(0o700)
        config = self.base / 'home/init.lua'
        server = Server(self.base, config)
        self.addCleanup(server.close)
        endpoint = 'rex:' + str(server.socket)
        before = snapshot(self.base / 'state')
        with (selected / 'commands.lock').open('w+') as gate:
            fcntl.flock(gate, fcntl.LOCK_EX)
            before = snapshot(self.base / 'state')
            result = subprocess.run([str(BINARY), '--plugin-state', str(selected.parent), '--endpoint', endpoint,
                                     'rex', 'sessions', '--json'], env=self.env, capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, (result.stdout, result.stderr))
        self.assertEqual(json.loads(result.stdout), dict(protocol_version='collab/v1', status='ok', operation='rex.sessions',
                         data=dict(endpoint=endpoint, panes=[])))
        self.assertEqual(snapshot(self.base / 'state'), before)
        self.assertEqual(server.calls, ['server.status', 'session.list'])
        self.assertFalse((self.base / 'calls').exists())

    def test_real_install_reinstall_and_cached_hook_resume(self):
        shutil.copy2(BINARY, self.root / 'bin/namedrop')
        shutil.copy2(REPO / 'runtime.json', self.root / 'runtime.json')
        (self.base / 'state/namedrop.sqlite3').unlink()
        config = self.base / 'home/.config/rex/init.lua'
        config.parent.mkdir(parents=True)
        config.write_text('-- personal config\n')
        server = Server(self.base, config)
        self.addCleanup(server.close)
        self.env['REX_SERVER'] = 'unix://' + str(server.socket)
        self.env['SURVEY_ENDPOINT'] = 'rex:' + str(server.socket)
        selected = self.publish()
        with contextlib.closing(sqlite3.connect(selected.parent / 'namedrop.sqlite3')) as db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], 8)
        self.assertFalse((selected / 'migration.json').exists())
        cached = self.base / 'cached-session-start.sh'
        package = self.base / 'home/.agents/skills/namedrop'
        wrapper = package / 'scripts/session-start'
        shutil.copy2(wrapper, cached)
        env = dict(self.env, NAMEDROP_STATE_DIR=str(selected.parent))
        removed = subprocess.run([str(selected / 'namedrop'), '--json', 'install', '--remove',
                                  '--input-json', '{"confirmed":true}'], env=env, capture_output=True, text=True, timeout=15)
        self.assertEqual(removed.returncode, 0, (removed.stdout, removed.stderr))
        self.assertTrue((selected / 'retired.json').exists())
        quiet = subprocess.run(['sh', str(cached)], input='{}', env=env, capture_output=True, text=True, timeout=5)
        self.assertEqual((quiet.returncode, quiet.stdout, quiet.stderr), (0, '', ''))
        self.publish()
        self.assertFalse((selected / 'retired.json').exists())
        survey = subprocess.run([str(selected / 'namedrop'), '--endpoint', self.env['SURVEY_ENDPOINT'],
                                 'rex', 'sessions', '--json'], env=env, capture_output=True, text=True, timeout=5)
        self.assertEqual(survey.returncode, 0, (survey.stdout, survey.stderr))
        self.assertFalse((self.base / 'calls').exists())

    @unittest.skipUnless(sys.platform == 'darwin', 'uses the existing macOS flock interposer')
    def test_removal_paused_after_marker_stops_after_runtime_reinstall(self):
        shutil.copy2(BINARY, self.root / 'bin/namedrop')
        shutil.copy2(REPO / 'runtime.json', self.root / 'runtime.json')
        config = self.base / 'home/.config/rex/init.lua'
        config.parent.mkdir(parents=True)
        config.write_text('-- personal config\n')
        server = Server(self.base, config)
        self.addCleanup(server.close)
        self.env['REX_SERVER'] = 'unix://' + str(server.socket)
        self.env['SURVEY_ENDPOINT'] = 'rex:' + str(server.socket)
        selected = self.publish()
        library = self.base / 'pause.dylib'
        subprocess.run(['cc', '-dynamiclib', str(REPO / 'tests/support/pause_lock.c'), '-o', str(library)], check=True)
        pause = self.base / 'pause'
        pause.touch()
        env = dict(self.env, DYLD_INSERT_LIBRARIES=str(library),
                   ND_TEST_PAUSE=f'/runtime/commands.lock:SH:{pause}')
        child = subprocess.Popen([str((selected / 'current/namedrop').resolve()), '--json', 'install', '--remove',
                                  '--input-json', '{"confirmed":true}'], env=env, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, text=True)
        if os.environ.get('NAMEDROP_U8_PID_LOG'):
            with open(os.environ['NAMEDROP_U8_PID_LOG'], 'a') as log:
                log.write(json.dumps(dict(pid=child.pid, kind='paused removal', scratch=str(self.base))) + '\n')
        try:
            deadline = time.monotonic() + 8
            while not pause.with_suffix('.arrived').exists() and child.poll() is None and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertTrue(pause.with_suffix('.arrived').exists(), 'removal never reached its shared gate')
            self.assertTrue((selected / 'retired.json').exists())
            self.publish()
            before = snapshot(selected.parent), snapshot(self.base / 'home')
            calls = list(server.calls)
        finally:
            pause.unlink(missing_ok=True)
            stdout, stderr = child.communicate(timeout=10)
        self.assertEqual(child.returncode, 0, (stdout, stderr))
        self.assertIn('Namedrop was installed again. Removal stopped.', stdout)
        self.assertEqual((snapshot(selected.parent), snapshot(self.base / 'home')), before)
        self.assertEqual(server.calls, calls)

    def test_schema_gate_times_out_with_tour_open_and_same_schema_passes(self):
        selected = self.publish()
        before = os.readlink(selected / 'current')
        store = (selected.parent / 'namedrop.sqlite3').read_bytes()
        with (selected / 'commands.lock').open('w+') as tour:
            fcntl.flock(tour, fcntl.LOCK_SH)
            fixture(self.root, number=2, state_schema=9)
            with mock.patch.dict(os.environ, self.env, clear=True):
                started = time.monotonic()
                with self.assertRaisesRegex(RuntimeError, '[Cc]lose .*Namedrop popup.*tour'):
                    runtime.activate(self.root, [], verify='install')
                self.assertGreaterEqual(time.monotonic() - started, 20)
            self.assertEqual(os.readlink(selected / 'current'), before)
            self.assertEqual((selected.parent / 'namedrop.sqlite3').read_bytes(), store)
            fixture(self.root, number=3)
            self.publish()
            self.assertNotEqual(os.readlink(selected / 'current'), before)
            self.assertEqual((selected.parent / 'namedrop.sqlite3').read_bytes(), store)

    def test_reinstall_that_does_not_complete_keeps_the_removed_marker(self):
        selected = self.publish()
        self.retire(selected)
        endpoint = self.env['SURVEY_ENDPOINT']
        busy = json.dumps([dict(endpoint=endpoint, space='s', block='busy', agent='codex', session=None, status='working')])
        # The steps that run with the marker aside are the survey, the
        # migration that follows it, and the launcher's `install`.
        surveyed = ['--runtime-info retired', 'sessions live']
        installed = ['--runtime-info retired', 'install live']
        cases = [('refused preflight', dict(state_schema=9), dict(SURVEY_PANES=busy), 'Some Rex sessions still use', surveyed),
                 ('failed survey', dict(state_schema=9), dict(FIXTURE_FAIL='survey'), 'Could not survey Rex agents', surveyed),
                 ('failed launcher', dict(failure=True), {}, 'Installation did not complete. Namedrop stays removed from Rex.',
                  installed),
                 ('failed verification', {}, dict(FIXTURE_FAIL='record'), 'Installation did not verify the Rex registration',
                  installed),
                 ('failed launcher after the migration', dict(state_schema=9, failure=True), {},
                  'The new runtime remains selected', [*surveyed, '--migrate-runtime-state live', 'install live'])]
        for number, (name, release, knobs, words, steps) in enumerate(cases, start=2):
            with self.subTest(name):
                fixture(self.root, number=number, **release)
                log = self.base / f'steps-{number}'
                result = invoke(self.root, dict(self.env, FIXTURE_LOG=str(log), **knobs))
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(words, result.stderr)
                self.assertIn('Namedrop stays removed from Rex. Run sh scripts/install.sh to install it again.', result.stderr)
                self.assertRetired(selected, result)
                self.assertEqual(log.read_text().splitlines(), steps)
        with contextlib.closing(sqlite3.connect(selected.parent / 'namedrop.sqlite3')) as db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], 9)

    def test_marker_written_during_the_reinstall_wins(self):
        selected = self.publish()
        self.retire(selected)
        again = json.dumps(dict(reason='removed', successor=None, at='again')) + '\n'
        later = json.dumps(dict(reason='removed', successor=None, at='later')) + '\n'
        handoff = json.dumps(dict(reason='herdr_handoff', successor='/herdr/state', at='later')) + '\n'
        cases = [(again, True, 'Namedrop was removed from Rex again meanwhile.'), (later, False, None),
                 (handoff, True, 'Namedrop is moving to Herdr. Run Activate Namedrop in Herdr again.')]
        for number, (marker, failure, words) in enumerate(cases, start=2):
            with self.subTest(marker=marker, failure=failure):
                fixture(self.root, number=number, failure=failure)
                result = invoke(self.root, dict(self.env, FIXTURE_RETIRE=marker))
                self.assertEqual(result.returncode, 7 if failure else 0, result.stderr)
                if words:
                    self.assertIn('Installation did not complete. ' + words, result.stderr)
                self.assertRetired(selected, result, marker)

    def test_signal_during_the_reinstall_puts_the_marker_back(self):
        selected = self.publish()
        self.retire(selected)
        before = os.readlink(selected / 'current')
        for number, name in enumerate(('TERM', 'HUP'), start=2):
            with self.subTest(name):
                fixture(self.root, number=number)
                result = invoke(self.root, dict(self.env, FIXTURE_SIGNAL=name))
                self.assertEqual(result.returncode, 128 + getattr(signal, 'SIG' + name), result.stderr)
                self.assertIn('Namedrop stays removed from Rex.', result.stderr)
                self.assertRetired(selected, result)
                self.assertEqual(os.readlink(selected / 'current'), before)

    def test_marker_left_aside_goes_back_on_the_next_run(self):
        selected = self.publish()
        aside = selected / 'retired.aside.json'
        endpoint = self.env['SURVEY_ENDPOINT']
        busy = json.dumps([dict(endpoint=endpoint, space='s', block='busy', agent='codex', session=None, status='working')])
        fixture(self.root, number=2, state_schema=9)
        # As a reinstall killed while it held the marker aside leaves it.
        aside.write_text(REMOVED)
        result = invoke(self.root, dict(self.env, SURVEY_PANES=busy))
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertRetired(selected, result)
        # Beside a marker written since, the one left aside is dropped.
        later = json.dumps(dict(reason='removed', successor=None, at='later')) + '\n'
        (selected / 'retired.json').write_text(later)
        aside.write_text(REMOVED)
        result = invoke(self.root, dict(self.env, SURVEY_PANES=busy))
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertRetired(selected, result, later)

    def test_busy_gate_at_put_back_writes_the_marker_anyway(self):
        selected = self.publish()
        self.retire(selected)
        release = self.base / 'release-gate'
        def stop_holder():
            release.touch()
            with contextlib.suppress(OSError, ValueError):
                pid = int(release.with_suffix('.pid').read_text())
                deadline = time.monotonic() + 5
                while time.monotonic() < deadline:
                    os.kill(pid, 0)
                    time.sleep(.05)
                os.kill(pid, signal.SIGKILL)
        self.addCleanup(stop_holder)
        fixture(self.root, number=2, failure=True)
        started = time.monotonic()
        result = invoke(self.root, dict(self.env, FIXTURE_HOLD_GATE=str(release)))
        self.assertGreaterEqual(time.monotonic() - started, runtime.RemovedMarker.WAIT)
        self.assertEqual(result.returncode, 7, result.stderr)
        self.assertIn('Namedrop stays removed from Rex. A Namedrop command that started during the install may still finish.',
                      result.stderr)
        self.assertRetired(selected, result)

    def test_marker_that_cannot_go_back_is_named_and_goes_back_next_run(self):
        selected = self.publish()
        self.retire(selected)
        fixture(self.root, number=2, failure=True)
        shown = io.StringIO()
        with mock.patch.dict(os.environ, self.env, clear=True), contextlib.redirect_stderr(shown), \
                contextlib.redirect_stdout(io.StringIO()), \
                mock.patch.object(runtime.os, 'link', side_effect=PermissionError(13, 'Permission denied')):
            self.assertEqual(runtime.activate(self.root, [], verify='install'), 7)
        self.assertIn(f'Installation did not complete. Namedrop could not mark {selected.parent} as removed again '
                      '([Errno 13] Permission denied)', shown.getvalue())
        self.assertFalse((selected / 'retired.json').exists())
        self.assertEqual((selected / 'retired.aside.json').read_text(), REMOVED)
        endpoint = self.env['SURVEY_ENDPOINT']
        busy = json.dumps([dict(endpoint=endpoint, space='s', block='busy', agent='codex', session=None, status='working')])
        fixture(self.root, number=3, state_schema=9)
        result = invoke(self.root, dict(self.env, SURVEY_PANES=busy))
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertRetired(selected, result)

    def test_handoff_before_the_gate_stops_the_reinstall(self):
        selected = self.publish()
        marker = self.retire(selected)
        handoff = json.dumps(dict(reason='herdr_handoff', successor='/herdr/state', at='later')) + '\n'
        real_gate = runtime.command_gate
        def gate(directory, *args, **kwargs):
            # Herdr's activation retires the state between the checks and the gate.
            if json.loads(marker.read_text())['reason'] == 'removed':
                marker.write_text(handoff)
            return real_gate(directory, *args, **kwargs)
        with mock.patch.dict(os.environ, self.env, clear=True), mock.patch.object(runtime, 'command_gate', gate), \
                contextlib.redirect_stdout(io.StringIO()), self.assertRaisesRegex(RuntimeError, 'Namedrop is moving to Herdr'):
            runtime.activate(self.root, [], verify='install')
        self.assertRetired(selected, None, handoff)

    def test_owner_taken_before_the_lock_stops_the_reinstall(self):
        selected = self.publish()
        before = os.readlink(selected / 'current')
        fixture(self.root, number=2)
        real_lock = runtime.lock
        def lock(directory, name, *args, **kwargs):
            # Herdr's install takes the state between the first check and the lock.
            if name == 'activation.lock':
                (selected / 'owner').write_text('herdr\n')
            return real_lock(directory, name, *args, **kwargs)
        with mock.patch.dict(os.environ, self.env, clear=True), mock.patch.object(runtime, 'lock', lock), \
                contextlib.redirect_stdout(io.StringIO()), self.assertRaisesRegex(RuntimeError, 'ND-INSTALL-002'):
            runtime.activate(self.root, [], verify='install')
        self.assertEqual((os.readlink(selected / 'current'), (selected / 'owner').read_text()), (before, 'herdr\n'))

    def test_first_install_leaves_out_agents_without_a_session(self):
        endpoint = self.env['SURVEY_ENDPOINT']
        # Claude Code and Codex report their kind through OSC 7501 with no Namedrop hook.
        self.env['SURVEY_PANES'] = json.dumps([
            dict(endpoint=endpoint, space='s', block='claude', agent='claude-code', session=None, status='idle'),
            dict(endpoint=endpoint, space='s', block='codex', agent='codex', session=None, status='working'),
            dict(endpoint=endpoint, space='s', block='bound', agent='codex', session='bound-session', status='idle')])
        selected = self.publish()
        self.assertEqual(json.loads((selected / 'legacy-sessions.json').read_text())['sessions'],
                         [[endpoint, 'bound-session']])
        # A later live update still waits for them, as for any agent from before the install.
        fixture(self.root, number=2, state_schema=9)
        result = invoke(self.root, self.env)
        self.assertEqual(result.returncode, 2, result.stderr)
        for line in ('An agent (claude-code, idle) in block claude', 'An agent (codex, working) in block codex'):
            self.assertIn(line, result.stderr)

    def test_first_publish_over_a_store_it_cannot_migrate_is_withdrawn(self):
        store = self.base / 'state/namedrop.sqlite3'
        with contextlib.closing(sqlite3.connect(store)) as db:
            db.execute('PRAGMA user_version=9')
            db.commit()
        selected = self.base / 'state/runtime'
        digest = hashlib.sha256((self.root / 'bin/namedrop').read_bytes()).hexdigest()
        for attempt in ('first', 'again', 'recovery'):
            with self.subTest(attempt=attempt):
                if attempt == 'recovery':
                    # What a first publish killed during its migration leaves.
                    (selected / 'current').symlink_to('releases/' + digest)
                    (selected / 'migration.json').write_text(json.dumps(dict(previous=None, target='releases/' + digest)))
                result = invoke(self.root, self.env)
                self.assertEqual(result.returncode, 2, result.stderr)
                self.assertIn('database schema 9 is not supported; expected 8', result.stderr)
                self.assertFalse((selected / 'current').is_symlink())
                self.assertFalse((selected / 'migration.json').exists())
                self.assertEqual((selected / 'owner').read_text(), 'standalone\n')
        fixture(self.root, number=2, state_schema=9)
        self.publish()
        digest = hashlib.sha256((self.root / 'bin/namedrop').read_bytes()).hexdigest()
        self.assertEqual(os.readlink(selected / 'current'), 'releases/' + digest)
        with contextlib.closing(sqlite3.connect(store)) as db:
            self.assertEqual(db.execute('PRAGMA user_version').fetchone()[0], 9)

    def test_session_refresh_reinstall_waits_for_every_open_session(self):
        selected = self.publish()
        endpoint = self.env['SURVEY_ENDPOINT']
        # A session from after the install: no update that runs live waits for it.
        rex_agent(selected.parent / 'namedrop.sqlite3', endpoint, 'later', alias='worker', runtime='codex', status='idle')
        before = snapshot(selected.parent)
        fixture(self.root, number=2, state_schema=9, live=False)
        result = invoke(self.root, self.env)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn('This update needs a session refresh. Finish the work in each Rex session below', result.stderr)
        self.assertIn(f'- @worker (codex, idle) in block later on {endpoint}', result.stderr)
        self.assertEqual(apart(snapshot(selected.parent), unpacked(selected.parent, self.root)), before)
        fixture(self.root, number=3, state_schema=9)
        self.publish()

    def test_default_socket_is_surveyed_without_rex_server(self):
        del self.env['REX_SERVER']
        default = (Path(self.env['HOME']) / 'Library/Application Support/rex/server.sock' if sys.platform == 'darwin'
                   else Path(self.env['XDG_RUNTIME_DIR']) / 'rex/server.sock')
        default.parent.mkdir(parents=True)
        listener = self.enterContext(contextlib.closing(socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)))
        listener.bind(str(default))
        listener.listen(4)
        endpoint = 'rex:' + str(default)
        self.env['SURVEY_PANES'] = json.dumps([dict(endpoint=endpoint, space='s', block='b', agent='codex',
                                                    session='default-session', status='idle')])
        policy = Install(self.root, self.base / 'state', self.root / 'bin/namedrop')
        with mock.patch.dict(os.environ, self.env, clear=True), contextlib.redirect_stderr(io.StringIO()):
            panes, unknown = policy.session_panes()
        self.assertEqual(([(p['_socket'], p['agent_session']['value']) for p in panes], unknown),
                         ([(endpoint, 'default-session')], []))
        # Linux with neither REX_SERVER nor XDG_RUNTIME_DIR has no socket to look at.
        bare = {key: value for key, value in self.env.items() if key != 'XDG_RUNTIME_DIR'}
        with mock.patch.dict(os.environ, bare, clear=True), mock.patch.object(runtime_install.sys, 'platform', 'linux'), \
                self.assertRaisesRegex(RuntimeError, r'^Start Rex, or set REX_SERVER to its Unix socket\.$'):
            policy.survey()

    def test_real_reinstall_refused_after_removal_keeps_the_state_retired(self):
        server = self.real_release()
        selected = self.publish()
        endpoint = 'rex:' + str(server.socket)
        # Removal leaves the store's bindings, so a reinstall brings the sessions back.
        rex_agent(selected.parent / 'namedrop.sqlite3', endpoint, 'gone', alias='gone', runtime='claude-code', status='idle')
        marker = self.remove(selected)
        fixture(self.root, number=2, state_schema=9, live=False)
        result = invoke(self.root, self.env)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn(f'- @gone (claude, idle) in block gone on {endpoint}', result.stderr)
        self.assertRetired(selected, result, marker)
        self.assertRemoved(selected)

    def test_real_reinstall_after_herdr_took_the_packages_leaves_the_state_retired(self):
        self.real_release()
        selected = self.publish()
        marker = self.remove(selected)
        # Herdr's activation then took the packages, through the lease's `removed` rule.
        herdr = self.base / 'herdr-state'
        shutil.copytree(selected, herdr / 'runtime', symlinks=True,
                        ignore=shutil.ignore_patterns('retired.json', '*.lock', 'source.json'))
        (herdr / 'runtime/owner').write_text('herdr\n')
        (self.base / 'home/.config/namedrop/state-directory').write_text(str(herdr) + '\n')
        result = invoke(self.root, self.env)
        self.assertEqual(result.returncode, 65, (result.stdout, result.stderr))
        self.assertIn('ND-INSTALL-002', result.stdout)
        self.assertIn('Installation did not complete. Namedrop stays removed from Rex.', result.stderr)
        self.assertRetired(selected, result, marker)
        self.assertRemoved(selected)

    @unittest.skipUnless(sys.platform == 'darwin', 'uses the existing macOS flock interposer')
    def test_removal_paused_after_marker_completes_after_failed_reinstall(self):
        server = self.real_release()
        selected = self.publish()
        library = self.base / 'pause.dylib'
        subprocess.run(['cc', '-dynamiclib', str(REPO / 'tests/support/pause_lock.c'), '-o', str(library)], check=True)
        pause = self.base / 'pause'
        pause.touch()
        env = dict(self.env, DYLD_INSERT_LIBRARIES=str(library), ND_TEST_PAUSE=f'/runtime/commands.lock:SH:{pause}')
        child = subprocess.Popen([str((selected / 'current/namedrop').resolve()), '--json', 'install', '--remove',
                                  '--input-json', '{"confirmed":true}'], env=env, stdout=subprocess.PIPE,
                                 stderr=subprocess.PIPE, text=True)
        try:
            deadline = time.monotonic() + 8
            while not pause.with_suffix('.arrived').exists() and child.poll() is None and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertTrue(pause.with_suffix('.arrived').exists(), 'removal never reached its shared gate')
            marker = (selected / 'retired.json').read_text()
            server.fail_before = True
            result = invoke(self.root, self.env)
            self.assertNotEqual(result.returncode, 0, result.stderr)
            self.assertRetired(selected, result, marker)
            server.fail_before = False
        finally:
            pause.unlink(missing_ok=True)
            stdout, stderr = child.communicate(timeout=20)
        self.assertEqual(child.returncode, 0, (stdout, stderr))
        report = json.loads(stdout)['data']
        self.assertEqual((report['removed'], report['packages_removed']), (True, True), report)
        self.assertRetired(selected, None, marker)
        self.assertFalse((self.base / 'home/.config/namedrop/rex.json').exists())


if __name__ == '__main__':
    unittest.main()
