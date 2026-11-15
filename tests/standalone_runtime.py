"""U8 publication contracts through isolated scripted hosts and real binaries."""
import contextlib
import fcntl
import hashlib
import io
import json
import os
from pathlib import Path
import shutil
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


def fixture(root, number=1, state_schema=8, failure=False):
    (root / 'bin').mkdir(parents=True, exist_ok=True)
    if not (root / 'scripts').exists():
        shutil.copytree(REPO / 'scripts', root / 'scripts')
    (root / 'herdr-plugin.toml').write_text(f'version = "1.7.{number}"\n')
    contract = dict(json.loads((REPO / 'runtime.json').read_text()), state_schema=state_schema,
                    live_migrate_from=[8] if state_schema == 9 else [3, 4, 5, 6, 7])
    (root / 'runtime.json').write_text(json.dumps(contract))
    executable = root / 'bin/namedrop'
    executable.write_text(f'''#!{sys.executable}
import json, os, pathlib, sqlite3, sys
args = sys.argv[1:]
state = pathlib.Path(os.environ['NAMEDROP_STATE_DIR'])
def emit(op, data): print(json.dumps(dict(status='ok', operation=op, data=data)))
if args == ['--runtime-info']:
    print({json.dumps(contract)!r})
elif args[:1] == ['--migrate-runtime-state']:
    if not (state/'namedrop.sqlite3').exists():
        sys.path.insert(0, {str(REPO / 'tests')!r})
        from runtime_activation import schema
        schema(state/'namedrop.sqlite3')
    with sqlite3.connect(state/'namedrop.sqlite3') as db: db.execute('PRAGMA user_version={state_schema}')
    print('{{"state_schema":{state_schema}}}')
elif 'sessions' in args:
    endpoint = args[args.index('--endpoint')+1] if '--endpoint' in args else os.environ['SURVEY_ENDPOINT']
    panes = json.loads(os.environ.get('SURVEY_PANES', '[]'))
    emit('rex.sessions', dict(endpoint=endpoint, panes=[p for p in panes if p['endpoint']==endpoint]))
else:
    assert 'install' in args and '--plugin-state' in args and '--input-json' in args, args
    assert args[args.index('--input-json')+1] == '{{"confirmed":true}}'
    if {failure!r}: sys.exit(7)
    home = pathlib.Path(os.environ['HOME'])
    module = home/'rex/namedrop.lua';module.parent.mkdir(exist_ok=True);module.write_text('-- managed fixture\\n')
    record = dict(state=str(state), launcher=str(state/'runtime/namedrop'), module=str(module),
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


class StandaloneChecks(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix='nd-u8-', dir='/tmp')
        self.addCleanup(self.directory.cleanup)
        self.base = Path(self.directory.name)
        self.env = environment(self.base)
        (self.base / 'tools/herdr').unlink()
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
        self.assertEqual(snapshot(selected.parent), before)

    def test_store_socket_failures_are_counted_except_missing_and_refused(self):
        self.publish()
        path = self.base / 'state/namedrop.sqlite3'
        path.unlink();schema(path)
        endpoints = ['rex:/tmp/u8-' + kind + '.sock' for kind in ('missing', 'refused', 'denied', 'timeout')]
        for i, endpoint in enumerate(endpoints):
            rex_agent(path, endpoint, str(i), alias='a'+str(i), runtime='codex', status='working')
        checks = dict(zip(endpoints, ('stopped', 'stopped', 'Permission denied', 'timed out')))
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

    def test_checkout_source_records_repository_origin_and_version(self):
        policy = Install(REPO, self.base / 'state', BINARY)
        source = policy.source()
        self.assertEqual(source['kind'], 'checkout')
        expected = (REPO / 'Cargo.toml').read_text().split('version = \"', 1)[1].split('\"', 1)[0]
        self.assertEqual(source['version'], expected)
        self.assertTrue(source['origin'])

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

    def test_same_schema_failure_rolls_back_source_and_runtime(self):
        selected = self.publish()
        before = os.readlink(selected / 'current')
        source = (selected / 'source.json').read_bytes()
        fixture(self.root, number=2, failure=True)
        result = invoke(self.root, self.env)
        self.assertEqual(result.returncode, 7)
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
        (self.base / 'state/namedrop.sqlite3').unlink()
        with mock.patch.dict(os.environ, self.env, clear=True), \
                mock.patch.object(runtime, 'migrate_selected', side_effect=RuntimeError('interrupted initialization')):
            with self.assertRaisesRegex(RuntimeError, 'interrupted initialization'):
                runtime.activate(self.root, [], verify='install')
        selected = self.base / 'state/runtime'
        self.assertEqual(json.loads((selected / 'migration.json').read_text())['previous'], None)
        self.assertEqual((selected / 'owner').read_text(), 'standalone\n')
        self.publish()
        self.assertFalse((selected / 'migration.json').exists())
        with contextlib.closing(sqlite3.connect(selected.parent / 'namedrop.sqlite3')) as db:
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


if __name__ == '__main__':
    unittest.main()
