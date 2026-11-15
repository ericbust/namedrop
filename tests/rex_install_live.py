"""U7 on disposable Rex servers: `namedrop install`, the handoff, removal,
and the palette's error lines.

Usage: python3 -I tests/rex_install_live.py BINARY REX_BINARY OUT [SECTION...]

Sections, all by default: shapes lines handoff drain palette helpers tour
doctor step6 races fallback. The optional runtime and updates sections need
NAMEDROP_TEST_STANDALONE_BASELINE and NAMEDROP_TEST_ARCHIVE from
build_standalone_upgrade.py. Each fixture is a folder of OUT with its own HOME, its
own Rex server on a socket under /tmp, and `--autostart=false -S unix://...`
on every Rex call. Herdr is the Herdr differential's script `herdr`. Agents
are Python stand-ins whose real SessionStart hook binds them in Rex blocks.
Claude and Codex are never started. Cleanup signals by pid only the processes
whose executable or argv lies under the fixture's folder, and the server.

`races` loads tests/support/pause_lock.c into one Namedrop process through
DYLD_INSERT_LIBRARIES, to hold it at a named lock, so it runs on macOS only.
"""
import contextlib
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shutil
import signal
import socket
import subprocess
import sys
import tarfile
import threading
import time

binary, rex, out = (Path(p).resolve() for p in sys.argv[1:4])
SECTIONS = ['shapes', 'lines', 'handoff', 'drain', 'palette', 'helpers', 'tour', 'doctor', 'step6', 'races', 'fallback', 'runtime', 'updates']
chosen = sys.argv[4:] or [section for section in SECTIONS if section not in ('runtime', 'updates')]
assert set(chosen) <= set(SECTIONS), chosen
repo = Path(__file__).resolve().parents[1]
spec = importlib.util.spec_from_file_location('runtime_cleanup', repo / 'scripts/runtime.py')
runtime = importlib.util.module_from_spec(spec)
sys.path.insert(0, str(repo / 'scripts'))
spec.loader.exec_module(runtime)
out.mkdir(parents=True, exist_ok=True)
LINE = 'pcall(require, "namedrop");'
CONFIRMED = '{"confirmed":true}'
TAG = str(os.getpid())
BUSY = 'Namedrop on Rex is busy. Close any Namedrop screen that is open in Rex, such as the tour, then run Activate Namedrop again.'


def say(*words):
    print(*words, flush=True)


def wait(read, accepts, description, within=15):
    until = time.monotonic() + within
    value = None
    while time.monotonic() < until:
        value = read()
        if accepts(value):
            return value
        time.sleep(.05)
    raise AssertionError(description + ': ' + repr(value))


def started_at(pid):
    """The process's start time, which with its pid names one incarnation."""
    p = subprocess.run(['/bin/ps', '-o', 'lstart=', '-p', str(pid)], capture_output=True, text=True)
    return p.stdout.strip() or None


def identity(pid):
    found = runtime.process(pid)
    start = started_at(pid)
    return None if found is None or start is None else (start, os.path.realpath(found[0]), tuple(found[1]))


def processes_under(root):
    """Every process of this user whose executable or argv names `root`."""
    rows = []
    for pid in runtime.all_pids():
        if pid == os.getpid():
            continue
        found = runtime.process(pid)
        if found and (os.path.realpath(found[0]).startswith(str(root) + '/') or any(str(root) in a for a in found[1])):
            rows.append((pid, *found))
    return rows


def helper_kind(argv, environment, state):
    """Which helper of `state` a process is, read as installed_runtime::helpers reads it."""
    rest = argv[1:]
    same = lambda path: path is not None and os.path.realpath(path) == os.path.realpath(state)
    if len(rest) == 3 and rest[0] in ('reminder-clock', '--deferred-worker') and same(rest[1]):
        return {'reminder-clock': 'reminder clock', '--deferred-worker': 'deferred worker'}[rest[0]]
    words, explicit, i = [], None, 0
    while i < len(rest):
        if rest[i] == '--plugin-state':
            explicit, i = rest[i + 1] if i + 1 < len(rest) else None, i + 2
        elif rest[i].startswith('--plugin-state='):
            explicit, i = rest[i].split('=', 1)[1], i + 1
        elif rest[i] in ('--endpoint', '--host', '--ui'):
            i += 2
        elif rest[i] == '--inherit-claim':
            i += 1
        else:
            words.append(rest[i])
            i += 1
    named = explicit or environment.get('NAMEDROP_STATE_DIR') or environment.get('HERDR_PLUGIN_STATE_DIR')
    kinds = {('rex', 'bridge'): 'bridge', ('relay', 'worker'): 'relay worker',
             ('hook', 'agent-status-worker'): 'status worker', ('dashboard-server',): 'dashboard server'}
    return kinds.get(tuple(words)) if same(named) else None


def helpers(state):
    """{pid: (kind, identity)} of every running helper of `state`."""
    found = {}
    for pid in runtime.all_pids():
        row = runtime.process(pid)
        if row and os.path.realpath(row[0]).startswith(os.path.realpath(state) + '/runtime/releases/'):
            kind = helper_kind(row[1], row[2], state)
            if kind:
                found[pid] = (kind, identity(pid))
    return found


def held_locks(state):
    """Helper lock files of `state` that some process holds."""
    paths = [p for name in ('rex-bridge', 'relay', 'deferred-commands', 'agent-status-reconciliation', 'dashboard')
             for p in (state / name).rglob('*.lock')] + [p for p in [state / 'reminder-clock.lock'] if p.is_file()]
    held = []
    for path in paths:
        with open(path, 'r') as f:
            try:
                fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                held.append(str(path))
    return held


def lua(path):
    return '"' + str(path) + '"'


class Fixture:
    """A standalone install's state, HOME and its own scratch Rex server."""

    def __init__(self, name, contents, custom=False, different_home=False, start=True):
        self.name = name
        self.root = out / name
        if self.root.exists():
            raise RuntimeError('Use a fresh OUT directory: ' + str(self.root))
        self.root.mkdir()
        self.home = self.root / 'home'
        self.home.mkdir()
        self.server_home = self.root / 'server-home' if different_home else self.home
        self.server_home.mkdir(exist_ok=True)
        self.config = self.root / 'custom/config.lua' if custom else self.server_home / '.config/rex/init.lua'
        self.config.parent.mkdir(parents=True)
        if contents is not None:
            self.config.write_bytes(contents)
        self.state = self.root / 'standalone'
        release = self.state / 'runtime/releases' / hashlib.sha256(binary.read_bytes()).hexdigest()
        release.mkdir(parents=True)
        shutil.copy2(binary, release / 'namedrop')
        shutil.copy2(repo / 'runtime.json', release / 'runtime.json')
        (self.state / 'runtime/current').symlink_to('releases/' + release.name)
        (self.state / 'runtime/owner').write_text('standalone\n')
        self.launcher = self.state / 'runtime/namedrop'
        self.launcher.write_text('#!' + sys.executable + ' -IB\nimport json,os,sys\nwith open(' + repr(str(self.root / 'pids.jsonl'))
                                 + ',"a") as f:f.write(json.dumps({"pid":os.getpid(),"argv":sys.argv})+"\\n")\nos.execv('
                                 + repr(str(self.state / 'runtime/current/namedrop')) + ',sys.argv)\n')
        self.launcher.chmod(0o700)
        # Short socket paths fit sockaddr_un even when OUT is a long scratch path.
        self.socket = Path('/tmp') / ('nd-u7-' + TAG + '-' + name + '.sock')
        self.endpoint = 'rex:' + str(self.socket)
        self.env = {'HOME': str(self.home), 'PATH': '/usr/bin:/bin', 'SHELL': '/bin/none',
                    'REX_SERVER': 'unix://' + str(self.socket), 'NAMEDROP_STATE_DIR': str(self.state)}
        self.server_env = dict(self.env, HOME=str(self.server_home))
        self.args = [str(rex), '--autostart=false', '-S', 'unix://' + str(self.socket), 'server', 'run', '--listen',
                     'unix://' + str(self.socket), '--state-dir', str(self.root / 'rex-state'), '--restore=false']
        if custom:
            self.args += ['--config', str(self.config)]
        self.server = None
        self.log = (self.root / 'rex.log').open('a')
        if start:
            self.start_server()

    def start_server(self):
        self.socket.unlink(missing_ok=True)
        self.server = subprocess.Popen(self.args, env=self.server_env, stdout=self.log, stderr=subprocess.STDOUT)
        with (self.root / 'server.pid').open('a') as f:
            f.write(str(self.server.pid) + '\n')
        wait(lambda: self.socket.exists() or self.server.poll() is not None, bool, 'scratch Rex start', 10)
        assert self.server.poll() is None, 'Scratch Rex failed: ' + str(self.root / 'rex.log')

    def stop_server(self):
        if self.server is not None and self.server.poll() is None:
            self.server.terminate()
            try:
                self.server.wait(10)
            except subprocess.TimeoutExpired:
                self.server.kill()
                self.server.wait()

    def rx(self, *args, ok=True, timeout=30):
        p = subprocess.run([str(rex), '--autostart=false', '-S', 'unix://' + str(self.socket), *args], env=self.env,
                           capture_output=True, text=True, timeout=timeout)
        if ok:
            assert p.returncode == 0, (args, p.stdout, p.stderr)
        return p

    def api(self, method, data):
        return json.loads(self.rx('api', 'call', method, json.dumps(data)).stdout)

    def session(self, name, *command):
        created = json.loads(self.rx('new', name, '--shell', 'none', '--json', '--', *(command or ['/bin/sh', '-i'])).stdout)
        return created['session_id'], created['initial_windows'][0]['block_ids'][0]

    def blocks(self, session):
        return {b['block_id']: b for b in self.api('session.list_blocks', {'session_id': session})['blocks']}

    def nd(self, *args, ok=True, env=None, timeout=75):
        p = subprocess.run([str(self.launcher), '--json', *args], env=env or self.env, input='', capture_output=True,
                           text=True, timeout=timeout)
        with (self.root / 'commands.jsonl').open('a') as f:
            f.write(json.dumps({'args': args, 'code': p.returncode, 'stdout': p.stdout, 'stderr': p.stderr}) + '\n')
        if ok:
            assert p.returncode == 0, (args, p.returncode, p.stdout, p.stderr)
        elif ok is False:
            assert p.returncode != 0, (args, p.stdout, p.stderr)
        return p

    def in_block(self, session, block, *args, ok=True, env=None):
        return self.nd('--endpoint', self.endpoint, '--space', session, '--pane', block, *args, ok=ok, env=env)

    def install(self, *args, ok=True):
        return self.nd('install', *args, '--input-json', CONFIRMED, ok=ok)

    def close(self):
        self.stop_server()
        self.log.close()
        reap(self.root)
        self.socket.unlink(missing_ok=True)


def reap(root):
    """Stops every process left under `root`, by pid, after rereading it."""
    for _ in range(3):
        rows = processes_under(root)
        if not rows:
            return
        for pid, *_ in rows:
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, signal.SIGTERM)
        time.sleep(1)
    for pid, executable, argv, _ in processes_under(root):
        if os.path.realpath(executable).startswith(str(root) + '/') or any(str(root) in a for a in argv):
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, signal.SIGKILL)


def stop_namedrop(root):
    """Stops every Namedrop process under `root`, and nothing else."""
    def running():
        return [r for r in processes_under(root) if os.path.realpath(r[1]).startswith(str(root) + '/')]
    for attempt in range(40):
        rows = running()
        if not rows:
            return
        for pid, *_ in rows:
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, signal.SIGTERM if attempt < 20 else signal.SIGKILL)
        time.sleep(.25)
    assert not running(), running()


def registration(f):
    return json.loads((f.home / '.config/namedrop/rex.json').read_text())


def shared(f):
    roots = [f.home / '.agents/skills/namedrop', f.home / '.claude/skills/namedrop', f.home / '.claude/settings.json',
             f.home / '.codex', f.home / '.local/bin/namedrop', f.home / '.config/namedrop/state-directory']
    return {str(p): p.read_bytes() for root in roots for p in (root.rglob('*') if root.is_dir() else [root]) if p.is_file()}


def snapshot(f):
    paths = [p for p in f.home.rglob('*') if p.is_file()]
    paths += [p for p in f.config.parent.glob('*') if p.is_file()]
    return {str(p): p.read_bytes() for p in paths}


def about(f):
    reply = json.loads(f.rx('do', 'namedrop_about', '--json').stdout)
    return reply.get('result', reply)


class Herdr:
    """Herdr's state beside a standalone fixture, with Herdr's launcher and its
    differential `herdr` script, on a socket that accepts and closes."""

    def __init__(self, f):
        self.f = f
        self.state = f.root / 'herdr'
        shutil.copytree(f.state / 'runtime', self.state / 'runtime', symlinks=True,
                        ignore=shutil.ignore_patterns('retired.json', '*.lock'))
        (self.state / 'runtime/owner').write_text('herdr\n')
        self.launcher = self.state / 'runtime/namedrop'
        self.launcher.write_text(f.launcher.read_text().replace(str(f.state), str(self.state)))
        self.launcher.chmod(0o700)
        self.checkout = f.root / 'repo'
        self.checkout.mkdir()
        (f.root / 'repo2').mkdir()
        self.socket = Path('/tmp') / ('nd-u7-h-' + TAG + '-' + f.name + '.sock')
        self.listener = socket.socket(socket.AF_UNIX)
        self.listener.bind(str(self.socket))
        self.listener.listen()
        self.done = threading.Event()
        threading.Thread(target=self.accept, daemon=True).start()
        self.env = {'HOME': str(f.home), 'PATH': '/usr/bin:/bin', 'ROOT': str(f.root), 'CHECKOUT': str(self.checkout),
                    'SHELLPID': str(os.getpid()), 'SOCK': str(self.socket),
                    'HERDR_BIN_PATH': str(repo / 'tests/differential/herdr'), 'HERDR_PLUGIN_STATE_DIR': str(self.state),
                    'HERDR_SOCKET_PATH': str(self.socket), 'HERDR_WORKSPACE_ID': 'w1', 'HERDR_PANE_ID': 'p1'}

    def accept(self):
        while not self.done.is_set():
            try:
                connection, _ = self.listener.accept()
                connection.close()
            except OSError:
                return

    def command(self, *args):
        return [str(self.launcher), '--endpoint', str(self.socket), '--space', 'w1', '--pane', 'p1', '--json', *args]

    def run(self, *args, ok=True, env=None, timeout=90):
        p = subprocess.run(self.command(*args), env=env or self.env, cwd=self.checkout, input='', capture_output=True,
                           text=True, timeout=timeout)
        with (self.f.root / 'herdr-commands.jsonl').open('a') as log:
            log.write(json.dumps({'args': args, 'code': p.returncode, 'stdout': p.stdout, 'stderr': p.stderr}) + '\n')
        if ok:
            assert p.returncode == 0, (args, p.returncode, p.stdout, p.stderr)
        return p

    def start(self, *args):
        return subprocess.Popen(self.command(*args), env=self.env, cwd=self.checkout, stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    def activate(self, ok=True):
        return self.run('activate', '--input-json', CONFIRMED, ok=ok)

    def warnings(self):
        report = self.run('doctor', ok=None)
        return [i for i in json.loads(report.stdout)['data']['issues'] if i['code'] == 'ND-INSTALL-001']

    def close(self):
        self.done.set()
        self.listener.close()
        self.socket.unlink(missing_ok=True)


def handoff_fixture(name, contents=b'-- config\n', install=True, **fixture):
    f = Fixture(name, contents, **fixture)
    if install:
        f.install()
    return f, Herdr(f)


AGENT = '''import json,os,subprocess,sys,time
launcher,state,ended=sys.argv[1:]
env=dict(os.environ,NAMEDROP_HOOK_RUNTIME="codex")
p=subprocess.run([launcher,"hook","session-start","--state-directory",state],input=json.dumps({"session_id":"u7-"+str(os.getpid())}),env=env,capture_output=True,text=True)
if p.returncode:raise SystemExit(p.stderr)
while not os.path.exists(ended):time.sleep(.05)
'''


# Claude Code and Codex report their kind through OSC 7501 with no
# integration installed, so Rex sees an agent that no Namedrop hook bound.
UNHOOKED = '''import os,sys,time
sys.stdout.write("\\x1b]7501;state=idle:app=" + sys.argv[1] + "\\x1b\\\\")
sys.stdout.flush()
while not os.path.exists(sys.argv[2]):time.sleep(.05)
'''


def bind_agents(f, count=2, first=0):
    script = f.root / 'standin.py'
    script.write_text(AGENT)
    sessions, blocks = [], []
    for i in range(first, first + count):
        session, block = f.session('u7-agent-' + str(i), sys.executable, '-I', str(script), str(f.launcher), str(f.state),
                                   str(f.root / ('end-' + str(i))))
        sessions.append(session)
        blocks.append(block)
    bound = lambda: list((f.state / 'rex/sessions').glob('*.json')) if (f.state / 'rex/sessions').exists() else []
    wait(bound, lambda entries: len(entries) == first + count, 'stand-in SessionStart bindings')
    return sessions, blocks


def bindings(f):
    return sorted(({k: v[k] for k in ['endpoint', 'block', 'agent']} for p in (f.state / 'rex/sessions').glob('*.json')
                   for v in [json.loads(p.read_text())]), key=lambda a: (a['endpoint'], a['block'], a['agent']))


def cached_hooks_are_quiet(f, cache):
    for script in ['session-start', 'user-prompt-submit', 'stop', 'subagent']:
        env = dict(f.env, NAMEDROP_HOOK_RUNTIME='codex')
        p = subprocess.run(['/bin/sh', str(cache / 'scripts' / script)], env=env, input='{}', capture_output=True, text=True,
                           timeout=10)
        assert p.returncode == 0 and not p.stdout and not p.stderr, (script, p)
    p = subprocess.run([str(f.launcher), 'hook', 'mailbox-wake', '--state-directory', str(f.state)], env=f.env,
                       input='{}', capture_output=True, text=True, timeout=10)
    assert p.returncode == 0 and not p.stdout and not p.stderr, ('mailbox-wake', p)


def section_shapes():
    shapes = [('absent', None), ('return', b'return {}\n'), ('comment', b'-- no newline'), ('runtime', b'error("mine broke")\n'),
              ('syntax', b'local = 1\n'), ('shebang', b'#!/usr/bin/env lua\n'), ('bom', b'\xef\xbb\xbf-- config\n'),
              ('paren', b'(function() return 1 end)()\n'),
              ('binding', b'rex.action{name="mine",title="Mine",run=function()end}\nrex.bind("alt+shift+cmd+i","mine")\n'),
              ('crlf', b'-- config\r\n')]
    for name, contents in shapes:
        broken = name in {'runtime', 'syntax', 'shebang', 'bom'}
        # Rex may reject a broken file at startup, so break it only after startup.
        f = Fixture(name, b'-- initial\n' if broken else contents, custom=name == 'return', different_home=name == 'binding')
        try:
            if broken:
                f.config.write_bytes(contents)
            f.install(ok=not broken)
            if broken:
                assert f.config.read_bytes() == contents
                say(name, 'refused before any edit, file unchanged')
                continue
            record = registration(f)
            assert record['config'] == str(f.config) and record['module'] == str(f.config.parent / 'namedrop.lua'), record
            readback = about(f)
            assert readback['launcher'] == str(f.launcher) and readback['state'] == str(f.state), readback
            assert readback['module'].removeprefix('@') == record['module'], readback
            actions = json.loads(f.rx('actions', '--json').stdout)
            actions = actions.get('actions', []) if isinstance(actions, dict) else actions
            assert len([a for a in actions if a['name'].startswith('namedrop_')]) == 11
            before = snapshot(f)
            f.install()
            assert snapshot(f) == before, 'second install changed a file'
            cached = f.root / 'cached-codex'
            shutil.copytree(f.home / '.agents/skills/namedrop', cached)
            f.install('--remove')
            assert f.config.read_bytes() == (contents or b''), f.config.read_bytes()
            before = {str(p.relative_to(f.state)): p.read_bytes() for p in f.state.rglob('*') if p.is_file()}
            cached_hooks_are_quiet(f, cached)
            assert {str(p.relative_to(f.state)): p.read_bytes() for p in f.state.rglob('*') if p.is_file()} == before
            say(name, 'passed')
        finally:
            f.close()
    say('U7 real Rex install/config/readback/remove/cached-hook checks passed')


def rex_module(f, session, block):
    report = json.loads(f.in_block(session, block, 'doctor', ok=None).stdout)['data']
    return next(i for i in report['issues'] if i['code'] == 'ND-REX-001')


def section_lines():
    """The U7 review's F1, F2, F5 and F6, and the backups, on a real server."""
    # F1: an unreadable moment is no removal, and the line put back clears it.
    f = Fixture('lines-read', b'-- mine\n')
    try:
        f.install()
        session, block = f.session('lines')
        f.config.chmod(0)
        try:
            f.in_block(session, block, 'activate', '--input-json', CONFIRMED)
        finally:
            f.config.chmod(0o600)
        assert registration(f)['line'] == 'inserted', registration(f)
        assert rex_module(f, session, block)['diagnostic']['confirmed'] is True
        edited = f.config.read_bytes()
        f.config.write_bytes(b'-- mine\n')
        f.api('server.config_reload', {})
        f.in_block(session, block, 'activate', '--input-json', CONFIRMED)
        assert registration(f)['line'] == 'removed', registration(f)
        issue = rex_module(f, session, block)
        assert issue['diagnostic']['confirmed'] is False and 'was removed' in issue['explanation'], issue
        f.config.write_bytes(edited)
        f.api('server.config_reload', {})
        # Doctor reads the file itself, with no refresh in between.
        issue = rex_module(f, session, block)
        assert issue['diagnostic']['confirmed'] is True, issue
        f.in_block(session, block, 'activate', '--input-json', CONFIRMED)
        assert registration(f)['line'] == 'inserted', registration(f)
        f.install('--remove')
        assert f.config.read_bytes() == b'-- mine\n', f.config.read_bytes()
        say('lines: an unreadable init.lua is no removal; the line put back is confirmed, and --remove deletes it')
    finally:
        f.config.chmod(0o600)
        f.close()
    # F2: a file Rex reads that is not UTF-8 is edited, and comes back byte
    # for byte, with only the newest backup kept.
    body = b'-- caf\xe9 config\nlocal x = 1\n'
    f = Fixture('lines-latin1', body)
    try:
        f.install()
        assert f.config.read_bytes() == LINE.encode() + b'\n' + body, f.config.read_bytes()
        assert about(f)['launcher'] == str(f.launcher)
        f.install('--remove')
        assert f.config.read_bytes() == body, f.config.read_bytes()
        backups = sorted(f.config.parent.glob('init.lua.namedrop-*.bak'))
        assert [b.read_bytes() for b in backups] == [LINE.encode() + b'\n' + body], backups
        say('lines: a Latin-1 init.lua is installed, confirmed and removed byte for byte, with one backup left')
    finally:
        f.close()
    # F5: removal finishes over an error the person made after the install.
    f = Fixture('lines-broken', b'-- mine\n')
    try:
        f.install()
        f.config.write_bytes(f.config.read_bytes() + b'local = 1\n')
        data = json.loads(f.install('--remove').stdout)['data']
        assert data['removed'] is True and 'config_error' in data, data
        assert f.config.read_bytes() == b'-- mine\nlocal = 1\n', f.config.read_bytes()
        assert not (f.config.parent / 'namedrop.lua').exists() and not (f.home / '.config/namedrop/rex.json').exists()
        say('lines: --remove finished over an init.lua the person broke, and named the error')
    finally:
        f.close()
    # F6: the person's own line, deleted later, is inserted by the next install.
    f = Fixture('lines-present', (LINE + '\n-- mine\n').encode())
    try:
        f.install()
        assert registration(f)['line'] == 'present'
        f.config.write_bytes(b'-- mine\n')
        f.install()
        assert f.config.read_bytes() == (LINE + '\n-- mine\n').encode() and registration(f)['line'] == 'inserted'
        say('lines: a present line the person deleted is inserted by the next install')
    finally:
        f.close()


def section_handoff():
    for scenario in ['takeover', 'stopped-before-marker', 'stopped-after-marker', 'writer-after-marker',
                     'partial-package-write', 'rex-half-removed', 'server-stopped', 'line-printed', 'line-removed']:
        f, h = handoff_fixture(scenario)
        try:
            sessions, blocks = bind_agents(f)
            cache = f.root / 'cached-codex'
            shutil.copytree(f.home / '.agents/skills/namedrop', cache)
            old_bindings = bindings(f)
            record = registration(f)
            if scenario.startswith('stopped-') or scenario in {'writer-after-marker', 'partial-package-write', 'rex-half-removed'}:
                record['predecessors'] = [{'state': str(f.state), 'sealed': False, 'agents': old_bindings}]
                (f.home / '.config/namedrop/rex.json').write_text(json.dumps(record))
                if scenario == 'stopped-before-marker':
                    before = shared(f)
                    assert not h.warnings()
                    assert shared(f) == before
                    # An agent that binds before the next run is in its one entry.
                    bind_agents(f, 1, first=2)
                    old_bindings = bindings(f)
                    assert len(old_bindings) == 3
                else:
                    (f.state / 'runtime/retired.json').write_text(json.dumps(
                        {'reason': 'herdr_handoff', 'successor': str(h.state), 'at': '2026-11-15T00:00:00Z'}))
            if scenario == 'writer-after-marker':
                h.run('event', 'agent-status-changed', '--input-json', json.dumps(
                    {'type': 'pane_agent_status_changed', 'pane_id': 'p1', 'workspace_id': 'w1', 'agent_status': 'idle'}), ok=None)
                assert h.warnings()
            if scenario == 'partial-package-write':
                (f.home / '.config/namedrop/state-directory').write_text(str(h.state) + '\n')
                (f.home / '.agents/skills/namedrop/SKILL.md').write_text('partial old package\n')
            if scenario == 'rex-half-removed':
                h.run('install', '--remove', '--input-json', CONFIRMED)
            if scenario == 'server-stopped':
                f.stop_server()
            if scenario in {'line-printed', 'line-removed'}:
                record['line'] = scenario.removeprefix('line-')
                (f.home / '.config/namedrop/rex.json').write_text(json.dumps(record))
            if scenario == 'line-removed':
                # The line state is read from init.lua, so the person removes it.
                f.config.write_bytes(f.config.read_bytes().split(b'\n', 1)[1])
            report = json.loads(h.activate().stdout)['data']
            assert report['handoff'] and 'were not carried over' in report['handoff'][0]['notice'], report.get('handoff')
            assert (f.state / 'runtime/retired.json').exists()
            assert (f.home / '.config/namedrop/state-directory').read_text() == str(h.state) + '\n'
            if scenario == 'rex-half-removed':
                assert not (f.home / '.config/namedrop/rex.json').exists() and not h.warnings()
                say(scenario, 'passed')
                continue
            record = registration(f)
            assert len(record['predecessors']) == 1 and record['predecessors'][0]['sealed'] is True, record['predecessors']
            entry = sorted(record['predecessors'][0]['agents'], key=lambda a: (a['endpoint'], a['block'], a['agent']))
            assert entry == old_bindings, (entry, old_bindings)
            if scenario == 'server-stopped':
                # The stopped server's next start reads the new namedrop.lua.
                f.start_server()
                readback = wait(lambda: about(f) if f.rx('actions', ok=False).returncode == 0 else None, bool,
                                'restarted server answers namedrop_about')
                assert readback['launcher'] == str(h.launcher) and readback['state'] == str(h.state), readback
                say('server-stopped: the next start loaded the module baked to Herdr\'s launcher')
            elif scenario not in {'line-printed', 'line-removed'}:
                assert about(f)['launcher'] == str(h.launcher)
            before = shared(f)
            p = f.nd('activate', '--input-json', CONFIRMED, ok=False)
            assert p.returncode == 69 and 'ND-INSTALL-001' in p.stderr and shared(f) == before, p
            cached_hooks_are_quiet(f, cache)
            if scenario == 'takeover':
                warn = h.warnings()
                assert len(warn) == 1 and '2 agents on Rex still run' in json.dumps(warn), warn
                assert all(block in json.dumps(warn) for block in blocks)
                (f.root / 'end-0').touch()
                wait(lambda: json.dumps(h.warnings()), lambda text: '1 agent on Rex still runs' in text, 'one old agent remains')
                h.run('install', '--remove', '--input-json', CONFIRMED)
                assert not h.warnings()
            say(scenario, 'passed')
        finally:
            h.close()
            f.close()
    say('U7 real Rex handoff replay, takeover, cached hooks and fresh Doctor checks passed')


def section_drain():
    for operation in ['handoff', 'removal']:
        for timeout in [False, True]:
            f, h = handoff_fixture(operation + ('-timeout' if timeout else '-drain'))
            try:
                gate = (f.state / 'runtime/commands.lock').open('r+')
                fcntl.flock(gate, fcntl.LOCK_SH)
                before = shared(f)
                started = time.monotonic()
                if operation == 'handoff':
                    p = h.start('activate', '--input-json', CONFIRMED)
                else:
                    p = subprocess.Popen([str(f.launcher), '--json', 'install', '--remove', '--input-json', CONFIRMED],
                                         env=f.env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                         text=True)
                wait(lambda: runtime.gate_holders(f.state / 'runtime/commands.lock'),
                     lambda holders: any(row[0] == p.pid for row in holders), 'retiring command reached the held gate')
                assert not (f.state / 'runtime/retired.json').exists() and shared(f) == before
                if not timeout:
                    gate.close()
                stdout, stderr = p.communicate(timeout=90)
                if timeout:
                    assert p.returncode != 0 and time.monotonic() - started >= 59, (p.returncode, stdout, stderr)
                    assert 'Namedrop on Rex is busy. Close any Namedrop screen' in stdout + stderr, (stdout, stderr)
                    assert not (f.state / 'runtime/retired.json').exists() and shared(f) == before
                    gate.close()
                else:
                    assert p.returncode == 0, (stdout, stderr)
                    assert (f.state / 'runtime/retired.json').exists()
                    refused = f.nd('doctor', ok=False)
                    assert refused.returncode == 69
                say(operation, 'timeout' if timeout else 'drain', 'passed')
            finally:
                h.close()
                f.close()


def section_palette():
    """The U6 review's F4: each start or run failure the module words itself."""
    f = Fixture('palette', b'-- config\n')
    try:
        f.install()
        session, block = f.session('u7-palette')
        module = Path(registration(f)['module'])
        good = module.read_text()
        stubs = f.root / 'stubs'
        stubs.mkdir()

        def stub(name, text, mode=0o700):
            path = stubs / name
            path.write_text(text)
            path.chmod(mode)
            return path
        cases = [
            ('missing', stubs / 'gone/namedrop', "Error: Namedrop's launcher {p} is missing. Run sh scripts/install.sh again."),
            ('not-executable', stub('noexec', '#!/bin/sh\nexit 0\n', 0o600),
             "Error: Namedrop's launcher {p} could not start: {p}: posix_spawn {p}: permission denied. Run sh scripts/install.sh again."),
            ('silent', stub('silent', '#!/bin/sh\nexit 3\n'), 'Error: Name this pane exited with status 3 and printed no reason.'),
            ('signal', stub('signal', '#!/bin/sh\nkill -KILL $$\n'), 'Error: Name this pane stopped on SIGKILL before it finished.'),
            ('timeout', stub('timeout', '#!/bin/sh\nexec /bin/sleep 40\n'),
             'Error: Name this pane did not finish within 25 s, so Rex stopped it. If Namedrop is updating, try again when the update is done.'),
        ]
        for name, launcher, expected in cases:
            text = good.replace('local NAMEDROP = ' + lua(f.launcher), 'local NAMEDROP = ' + lua(launcher), 1)
            assert text != good
            module.write_text(text)
            f.rx('config', 'reload')
            began = time.monotonic()
            p = f.rx('do', '-s', session, '-b', block, '--origin', 'palette', 'namedrop_name', ok=False, timeout=60)
            message = (p.stdout + p.stderr).strip()
            assert p.returncode != 0 and message == expected.format(p=launcher), (name, p.returncode, message)
            assert name != 'timeout' or time.monotonic() - began >= 24
            say('palette', name, '->', message)
        module.write_text(good)
        f.rx('config', 'reload')
        say('U7 palette error lines passed')
    finally:
        f.close()


def start_helpers(f, session, block, first=0):
    """Runs Namedrop where it starts each helper of the standalone state."""
    f.in_block(session, block, 'activate', '--input-json', CONFIRMED)
    f.in_block(session, block, 'dashboard', '--no-open')
    # A bound stand-in in a second active space. The reminder clock is not
    # started: a reminder needs an agent session with a completed
    # registration, which a stand-in does not perform.
    sessions, blocks = bind_agents(f, 1, first=first)
    f.in_block(sessions[0], blocks[0], 'activate', '--input-json', CONFIRMED)
    relay = subprocess.Popen([str(f.launcher), 'relay', 'worker', '--host', 'u7', '--endpoint', f.endpoint], env=f.env,
                             stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                             start_new_session=True)
    threading.Thread(target=relay.wait, daemon=True).start()
    wanted = {'bridge', 'deferred worker', 'dashboard server'}
    found = wait(lambda: helpers(f.state), lambda h: wanted <= {kind for kind, _ in h.values()},
                 'helpers of the standalone state', 20)
    time.sleep(1)
    return helpers(f.state) or found


def section_helpers():
    for operation in ['takeover', 'removal']:
        f, h = handoff_fixture('helpers-' + operation)
        try:
            session, block = f.session('u7-helpers')
            running = start_helpers(f, session, block)
            kinds = sorted(kind for kind, _ in running.values())
            say('helpers', operation, 'running before:', kinds)
            cache = f.root / 'cached-codex'
            shutil.copytree(f.home / '.agents/skills/namedrop', cache)
            if operation == 'takeover':
                h.activate()
            else:
                f.install('--remove')

            def back():
                return {pid for pid, (_, before) in running.items() if identity(pid) == before}
            wait(back, lambda alive: not alive, 'every old helper ended', 10)
            cached_hooks_are_quiet(f, cache)
            f.in_block(session, block, 'doctor', ok=False)
            time.sleep(3)
            assert not back(), back()
            assert not helpers(f.state), helpers(f.state)
            assert not held_locks(f.state), held_locks(f.state)
            say('helpers', operation, 'passed: none of', len(running), 'helpers came back')
        finally:
            h.close()
            f.close()


def section_tour():
    f, h = handoff_fixture('tour')
    try:
        session, block = f.session('u7-tour')
        f.in_block(session, block, 'activate', '--input-json', CONFIRMED)
        before_blocks = set(f.blocks(session))
        f.in_block(session, block, 'ui', 'launch', 'tour')
        layer = next(iter(wait(lambda: set(f.blocks(session)) - before_blocks, bool, 'the tour layer opened')))
        gate = f.state / 'runtime/commands.lock'
        wait(lambda: runtime.gate_holders(gate), bool, 'the tour holds the old state\'s gate')
        record = (f.home / '.config/namedrop/rex.json').read_bytes()
        before = shared(f)
        began = time.monotonic()
        p = h.activate(ok=False)
        assert p.returncode != 0 and time.monotonic() - began >= 59, (p.returncode, p.stdout, p.stderr)
        assert BUSY in p.stdout + p.stderr, (p.stdout, p.stderr)
        assert not (f.state / 'runtime/retired.json').exists()
        assert shared(f) == before and (f.home / '.config/namedrop/rex.json').read_bytes() == record
        assert layer in f.blocks(session), 'the handoff closed the tour'
        say('tour: busy after', round(time.monotonic() - began), 's, layer still open, nothing changed')
        f.rx('send-key', '-s', session, '-b', layer, 'escape')
        wait(lambda: f.blocks(session), lambda blocks: layer not in blocks, 'the tour layer closed', 20)
        report = json.loads(h.activate().stdout)['data']
        assert report['handoff'] and (f.state / 'runtime/retired.json').exists()
        say('tour passed: the next activation handed off')
    finally:
        h.close()
        f.close()


def section_doctor():
    f, h = handoff_fixture('doctor')
    try:
        sessions, blocks = bind_agents(f)
        h.activate()
        # Every Namedrop process stops; the server and both agents keep running.
        stop_namedrop(f.root)
        agents = {block: next(a for a in bindings(f) if a['block'] == block) for block in blocks}
        assert all(identity(int(a['agent'].split('@')[0])) for a in agents.values()), 'both stand-ins still run'
        env = {k: v for k, v in h.env.items() if k not in ('NAMEDROP_STATE_DIR', 'XDG_STATE_HOME')}

        def warned():
            report = h.run('doctor', ok=None, env=env)
            return [i for i in json.loads(report.stdout)['data']['issues'] if i['code'] == 'ND-INSTALL-001']
        two = warned()
        text = json.dumps(two)
        assert len(two) == 1 and '2 agents on Rex still run' in text and str(f.state) in text, two
        assert all(block + ' on ' + f.endpoint in text for block in blocks), two
        (f.root / 'end-0').touch()
        one = wait(warned, lambda w: '1 agent on Rex still runs' in json.dumps(w), 'one old agent remains')
        assert blocks[1] in json.dumps(one) and blocks[0] not in json.dumps(one), one
        f.stop_server()
        f.start_server()
        wait(lambda: identity(int(agents[blocks[1]]['agent'].split('@')[0])), lambda alive: alive is None,
             'the Rex restart ended the second agent')
        assert not warned()
        f.stop_server()
        # The restarted server's module started a bridge for Herdr's state,
        # which would reconnect; stop it so only Doctor could call.
        stop_namedrop(f.root)
        f.socket.unlink(missing_ok=True)
        calls = []
        listener = socket.socket(socket.AF_UNIX)
        listener.bind(str(f.socket))
        listener.listen()
        listener.settimeout(.2)

        def count():
            while listener.fileno() >= 0:
                try:
                    connection, _ = listener.accept()
                    try:
                        calls.append(connection.getsockopt(0, 2))  # SOL_LOCAL, LOCAL_PEERPID
                    except OSError:
                        calls.append('a peer that already closed')
                    connection.close()
                except (socket.timeout, OSError):
                    if listener.fileno() < 0:
                        return
        threading.Thread(target=count, daemon=True).start()
        assert not warned()
        time.sleep(.5)
        listener.close()
        assert not calls, ('Doctor called the stopped server for liveness', calls)
        say('doctor passed: 2, then 1, then 0 after the Rex restart, and 0 with the server stopped and no call')
    finally:
        h.close()
        f.close()


def section_step6():
    """Step 6 through runtime.py: a module that names another launcher."""
    f, h = handoff_fixture('step6')
    try:
        def staged(name, sign=None):
            source = f.root / name
            shutil.copytree(repo / 'scripts', source / 'scripts')
            shutil.copy2(repo / 'runtime.json', source / 'runtime.json')
            (source / 'bin').mkdir()
            shutil.copy2(binary, source / 'bin/namedrop')
            if sign:
                subprocess.run(['/usr/bin/codesign', '--force', '--sign', '-', '--identifier', sign,
                                str(source / 'bin/namedrop')], check=True, capture_output=True)
            return source

        tools = f.root / 'bin'
        tools.mkdir()
        shutil.copy2(repo / 'tests/differential/herdr', tools / 'herdr')

        def publish(source, home=None):
            # runtime.py surveys Herdr through the `herdr` on PATH.
            env = dict(h.env, HOME=str(home or f.home), PATH=str(tools) + ':/usr/bin:/bin')
            p = subprocess.run(['/bin/sh', str(source / 'scripts/run.sh'), 'activate', '--input-json', CONFIRMED],
                               env=env, cwd=h.checkout, capture_output=True, text=True, timeout=120)
            with (f.root / 'run-sh.jsonl').open('a') as log:
                log.write(json.dumps({'home': env['HOME'], 'code': p.returncode, 'stdout': p.stdout, 'stderr': p.stderr}) + '\n')
            assert p.returncode == 0, (p.returncode, p.stdout, p.stderr)
            return json.loads(p.stdout)['data']
        # Herdr's state gets its runtime from runtime.py, in a HOME of its
        # own, so this machine's packages and record stay the standalone's.
        shutil.rmtree(h.state)
        h.state.mkdir()
        bootstrap = f.root / 'bootstrap-home'
        bootstrap.mkdir()
        publish(staged('source-previous'), bootstrap)
        h.launcher = h.state / 'runtime/namedrop'
        previous = os.readlink(h.state / 'runtime/current')
        source = staged('source', 'namedrop-u7-step6')
        incoming = 'releases/' + hashlib.sha256((source / 'bin/namedrop').read_bytes()).hexdigest()
        assert incoming != previous
        assert (f.home / '.config/namedrop/state-directory').read_text() == str(f.state) + '\n'
        override = ('rex.action{ name = "namedrop_about", title = "About Namedrop", run = function() return { module = '
                    + lua(f.config.parent / 'namedrop.lua') + ', launcher = "/elsewhere/namedrop", state = '
                    + lua(h.state) + ' } end }\n')
        original = f.config.read_text()
        f.config.write_text(original + override)
        for attempt in (1, 2):
            data = publish(source)
            assert os.readlink(h.state / 'runtime/current') == incoming, os.readlink(h.state / 'runtime/current')
            entry = data['handoff'][0]
            assert 'were not carried over' in entry['notice'] and entry['module']['status'] == 'not_confirmed', entry
            assert '/elsewhere/namedrop' in json.dumps(entry['module']), entry
            assert registration(f)['predecessors'][0]['sealed'] is False
            say('step6 run', attempt, ': exit 0, the new runtime selected, not confirmed, notice repeated, unsealed')
        f.config.write_text(original)
        data = publish(source)
        assert data['handoff'][0]['module'] == {'sealed': True, 'status': 'confirmed'}, data['handoff']
        assert registration(f)['predecessors'][0]['sealed'] is True
        assert about(f)['launcher'] == str(h.launcher)
        assert 'handoff' not in publish(source)
        say('step6 passed: the fixed module sealed on the next Activate, and the one after says nothing')
    finally:
        h.close()
        f.close()


PAUSE = repo / 'tests/support/pause_lock.c'


def pause_library():
    built = out / 'pause_lock.dylib'
    if not built.exists():
        subprocess.run(['/usr/bin/cc', '-dynamiclib', '-O2', '-o', str(built), str(PAUSE)], check=True)
    return built


def paused(f, suffix, kind, *args):
    """Starts the standalone launcher held at its first blocking `kind` flock of `suffix`."""
    hold = f.root / ('pause-' + kind)
    hold.touch()
    env = dict(f.env, DYLD_INSERT_LIBRARIES=str(pause_library()), ND_TEST_PAUSE=suffix + ':' + kind + ':' + str(hold))
    p = subprocess.Popen([str(f.launcher), '--json', *args], env=env, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                         stderr=subprocess.PIPE, text=True)
    wait(lambda: Path(str(hold) + '.arrived').exists() or p.poll() is not None, bool, 'the paused command arrived', 75)
    assert p.poll() is None, p.communicate()
    return p, hold


def section_races():
    # Removal against a handoff: stopped once `removed` is durable, before its lease.
    f, h = handoff_fixture('race-handoff')
    try:
        bind_agents(f, 1)
        init = f.config.read_bytes()
        p, _ = paused(f, '/.config/namedrop/packages.lock', 'EX', 'install', '--remove', '--input-json', CONFIRMED)
        assert json.loads((f.state / 'runtime/retired.json').read_text())['reason'] == 'removed'
        p.kill()
        p.communicate()
        report = json.loads(h.activate().stdout)['data']
        assert not report.get('handoff'), report.get('handoff')
        assert 'predecessors' not in registration(f), registration(f)
        assert (f.home / '.config/namedrop/state-directory').read_text() == str(h.state) + '\n'
        herdr_files = shared(f)
        assert str(h.launcher) in (f.home / '.agents/skills/namedrop/scripts/session-start').read_text()
        again = json.loads(f.nd('install', '--remove', '--input-json', CONFIRMED).stdout)['data']
        assert again['packages_removed'] is False and 'Removed only the Rex configuration' in again['message'], again
        assert shared(f) == herdr_files, 'removal touched Herdr\'s packages or record'
        assert not (f.home / '.config/namedrop/rex.json').exists() and not (f.config.parent / 'namedrop.lua').exists()
        assert f.config.read_bytes() == init[len(LINE) + 1:], f.config.read_bytes()
        say('races: removal against a handoff passed')
    finally:
        h.close()
        f.close()
    # Removal against a reinstall: paused after its marker, before its shared gate.
    f = Fixture('race-reinstall', b'-- config\n')
    try:
        f.install()
        session, block = f.session('u7-race')
        p, hold = paused(f, '/runtime/commands.lock', 'SH', 'install', '--remove', '--input-json', CONFIRMED)
        assert json.loads((f.state / 'runtime/retired.json').read_text())['reason'] == 'removed'
        with open(f.state / 'runtime/activation.lock', 'a+') as activation:
            fcntl.flock(activation, fcntl.LOCK_EX)
            with open(f.state / 'runtime/commands.lock', 'r+') as gate:
                fcntl.flock(gate, fcntl.LOCK_EX)
                (f.state / 'runtime/retired.json').unlink()
            f.install()
        running = start_helpers(f, session, block)
        files = shared(f)
        module = (f.config.parent / 'namedrop.lua').read_bytes()
        hold.unlink()
        stdout, stderr = p.communicate(timeout=60)
        assert p.returncode == 0, (p.returncode, stdout, stderr)
        assert 'Namedrop was installed again. Removal stopped.' in stdout, stdout
        assert {pid: identity(pid) for pid in running} == {pid: before for pid, (_, before) in running.items()}, 'a helper was signalled'
        assert shared(f) == files and (f.config.parent / 'namedrop.lua').read_bytes() == module
        assert not (f.state / 'runtime/retired.json').exists() and registration(f)['state'] == str(f.state)
        say('races: removal against a reinstall passed;', len(running), 'helpers kept their pids')
    finally:
        f.close()


def section_fallback():
    """The per-user lock that cannot be opened, three ways."""
    conditions = ['read-only-config', 'config-is-a-file', 'lock-is-a-directory']
    lock_error = 'Namedrop could not open its package lock '
    for condition in conditions:
        # Herdr alone: every writer falls back to the per-state lock and writes.
        # No Rex here, and the server's config lives outside HOME/.config.
        f, h = handoff_fixture('fallback-herdr-' + condition, install=False, custom=True, start=False)
        try:
            config = f.home / '.config'
            if condition == 'read-only-config':
                config.mkdir()
                config.chmod(0o555)
            elif condition == 'config-is-a-file':
                config.write_text('a file\n')
            else:
                (config / 'namedrop/packages.lock').mkdir(parents=True)
            lock = config / 'namedrop/packages.lock'
            event = json.dumps({'type': 'pane_agent_status_changed', 'pane_id': 'p1', 'workspace_id': 'w1', 'agent_status': 'idle'})
            for operation, args in [('activate', ['activate', '--input-json', CONFIRMED]), ('name', ['name', 'alpha']),
                                    ('event', ['event', 'agent-status-changed', '--input-json', event]),
                                    ('reconcile', ['reconcile']), ('deactivate', ['deactivate'])]:
                p = h.run(*args, ok=None)
                assert p.returncode == 0, (condition, operation, p.stdout, p.stderr)
                assert lock_error + str(lock) in p.stderr and 'It wrote under the lock of ' + str(h.state) + ' only.' in p.stderr, (condition, operation, p.stderr)
                if operation == 'activate':
                    assert str(h.launcher) in (f.home / '.agents/skills/namedrop/scripts/session-start').read_text()
                if operation == 'deactivate':
                    data = json.loads(p.stdout)['data']
                    assert all(data[key]['error'] is None for key in ('codex_sandbox', 'subagent_hooks', 'claude_permissions')), data
            say('fallback', condition, 'Herdr writers wrote under the per-state lock and named', lock)
        finally:
            if (f.home / '.config').is_dir():
                (f.home / '.config').chmod(0o755)
            h.close()
            f.close()
        if condition == 'config-is-a-file':
            continue  # No standalone record can exist without ~/.config/namedrop.
        # Under a standalone record.
        f, h = handoff_fixture('fallback-standalone-' + condition)
        try:
            session, block = f.session('u7-fallback')
            directory = f.home / '.config/namedrop'
            lock = directory / 'packages.lock'
            lock.unlink()
            if condition == 'read-only-config':
                directory.chmod(0o555)
            else:
                lock.mkdir()
            record = (directory / 'rex.json').read_bytes()
            before = shared(f)
            event = json.dumps({'type': 'pane_agent_status_changed', 'pane_id': 'p1', 'workspace_id': 'w1', 'agent_status': 'idle'})
            for operation, args in [('name', ['name', 'alpha']), ('event', ['event', 'agent-status-changed', '--input-json', event]),
                                    ('reconcile', ['reconcile']), ('deactivate', ['deactivate'])]:
                p = h.run(*args, ok=None)
                assert p.returncode == 0, (condition, operation, p.stdout, p.stderr)
                assert shared(f) == before, (condition, operation)
            p = h.activate(ok=False)
            assert p.returncode != 0 and lock_error + str(lock) in p.stdout + p.stderr, (p.stdout, p.stderr)
            assert not (f.state / 'runtime/retired.json').exists() and shared(f) == before
            assert (directory / 'rex.json').read_bytes() == record
            p = f.in_block(session, block, 'activate', '--input-json', CONFIRMED, ok=False)
            assert lock_error + str(lock) in p.stdout + p.stderr, (p.stdout, p.stderr)
            assert shared(f) == before and (directory / 'rex.json').read_bytes() == record
            say('fallback', condition, 'under a standalone record: Herdr wrote nothing, activations failed naming', lock)
        finally:
            (f.home / '.config/namedrop').chmod(0o755)
            h.close()
            f.close()
    say('U7 lock fallback matrix passed')


def section_runtime():
    """U8 with the fixture archives: no Herdr, real agents, tour and reinstall."""
    f = Fixture('runtime-u8', b'-- config\n')
    try:
        shutil.rmtree(f.state)
        f.state.mkdir()
        assert shutil.which('herdr', path=f.env['PATH']) is None
        roots = []
        for key, label in [('NAMEDROP_TEST_STANDALONE_BASELINE', 'baseline'), ('NAMEDROP_TEST_ARCHIVE', 'incoming')]:
            directory = f.root / label
            with tarfile.open(os.environ[key]) as archive:
                archive.extractall(directory, filter='data')
            roots.append(next(directory.iterdir()))
        old, new = roots

        def publish(source, ok=True):
            p = subprocess.run([sys.executable, str(source / 'scripts/runtime.py'), str(source), '--state-dir',
                                str(f.state), '--verify', 'install'], env=f.env, input='', capture_output=True,
                               text=True, timeout=90)
            with (f.root / 'runtime-publish.jsonl').open('a') as log:
                log.write(json.dumps(dict(source=str(source), code=p.returncode, stdout=p.stdout, stderr=p.stderr)) + '\n')
            assert (p.returncode == 0) == ok, (p.returncode, p.stdout, p.stderr)
            return p

        def store():
            import sqlite3
            with contextlib.closing(sqlite3.connect(f.state / 'namedrop.sqlite3')) as db:
                return '\n'.join(db.iterdump())

        def settled():
            # The bridge records a block's observations a moment after the
            # block changes, so the snapshot waits for those writes to end.
            last = store()
            for _ in range(15):
                time.sleep(1)
                now = store()
                if now == last:
                    return now
                last = now
            raise AssertionError('the store kept changing before the publish')

        def unchanged(before):
            import difflib
            after = store()
            assert after == before, '\n'.join(difflib.unified_diff(before.splitlines(), after.splitlines(), lineterm=''))

        script = f.root / 'unhooked.py'
        script.write_text(UNHOOKED)
        unhooked = {f.session('u8-unhooked-' + app, sys.executable, '-I', str(script), app, str(f.root / ('end-' + app)))[1]: app
                    for app in ('claude-code', 'codex')}

        def surveyed():
            p = subprocess.run([str(old / 'bin/namedrop'), '--plugin-state', str(f.state), '--endpoint', f.endpoint, 'rex',
                                'sessions', '--json'], env=f.env, capture_output=True, text=True, timeout=30)
            assert p.returncode == 0, (p.stdout, p.stderr)
            return {pane['block']: (pane['agent'], pane['session']) for pane in json.loads(p.stdout)['data']['panes']}
        wait(surveyed, lambda found: all(found.get(block) == (app, None) for block, app in unhooked.items()),
             'the unhooked agents reported their kinds')
        publish(old)
        assert (f.state / 'runtime/owner').read_text() == 'standalone\n'
        assert json.loads((f.state / 'runtime/source.json').read_text())['kind'] == 'archive'
        migration = f.state / 'runtime/legacy-sessions.json'
        assert json.loads(migration.read_text())['sessions'] == [], migration.read_text()
        selected, before = os.readlink(f.state / 'runtime/current'), settled()
        refused = publish(new, ok=False)
        assert all(f'in block {block} on {f.endpoint}' in refused.stderr for block in unhooked), refused.stderr
        assert os.readlink(f.state / 'runtime/current') == selected
        unchanged(before)
        for app in unhooked.values():
            (f.root / ('end-' + app)).touch()
        wait(surveyed, lambda found: not any(found.get(block, (None,))[0] for block in unhooked), 'the unhooked agents ended')
        say('U8 first install with unhooked Claude Code and Codex agents passed; a live update waited for them')
        sessions, blocks = bind_agents(f, 1)
        cutoff = json.loads(migration.read_text())
        survey = json.loads(f.nd('rex', 'sessions', '--endpoint', f.endpoint).stdout)['data']['panes']
        agent = next(p for p in survey if p['block'] == blocks[0])
        # A stand-in writes no status record, so the adapter reports its bound
        # session with no agent kind (`unreported_kind` in src/mux/rex/agent.rs).
        assert agent['session'] and agent['reports_session'], agent
        cutoff['sessions'].append([f.endpoint, agent['session']])
        migration.write_text(json.dumps(cutoff))
        selected, before = os.readlink(f.state / 'runtime/current'), settled()
        refused = publish(new, ok=False)
        assert 'Some Rex sessions still use pre-launcher tools' in refused.stderr
        assert os.readlink(f.state / 'runtime/current') == selected
        unchanged(before)
        agent_pid = int(bindings(f)[0]['agent'].split('@')[0])
        (f.root / 'end-0').touch()
        wait(lambda: identity(agent_pid), lambda alive: alive is None, 'the working agent ended')
        cutoff['sessions'] = []
        cutoff['codex_ready'] = True
        migration.write_text(json.dumps(cutoff))

        session, block = f.session('u8-runtime-tour')
        f.in_block(session, block, 'name', 'survivor', '--type', 'terminal')
        before_blocks = set(f.blocks(session))
        f.in_block(session, block, 'ui', 'launch', 'tour')
        layer = next(iter(wait(lambda: set(f.blocks(session)) - before_blocks, bool, 'the U8 tour opened')))
        wait(lambda: runtime.gate_holders(f.state / 'runtime/commands.lock'), bool, 'the U8 tour holds its gate')
        selected, before = os.readlink(f.state / 'runtime/current'), settled()
        started = time.monotonic()
        refused = publish(new, ok=False)
        assert time.monotonic() - started >= 20
        assert 'close' in refused.stderr and 'tour' in refused.stderr
        assert os.readlink(f.state / 'runtime/current') == selected
        unchanged(before)
        assert layer in f.blocks(session)

        same = f.root / 'same-schema'
        shutil.copytree(repo / 'scripts', same / 'scripts')
        for filename in ('runtime.json', 'herdr-plugin.toml'):
            shutil.copy2(repo / filename, same / filename)
        (same / 'bin').mkdir()
        shutil.copy2(binary, same / 'bin/namedrop')
        publish(same)
        assert os.readlink(f.state / 'runtime/current') != selected
        old_runtime = os.path.realpath(f.state / 'runtime' / selected / 'namedrop')
        assert any(os.path.realpath(row[1]) == old_runtime and 'ui' in row[2] and 'run' in row[2]
                   for row in processes_under(f.root)), ('tour changed runtimes', processes_under(f.root))
        assert layer in f.blocks(session)
        f.rx('send-key', '-s', session, '-b', layer, 'escape')
        wait(lambda: f.blocks(session), lambda b: layer not in b, 'the U8 tour closed')
        publish(new)
        assert 'survivor' in store()

        cache = f.root / 'u8-cached-codex'
        shutil.copytree(f.home / '.agents/skills/namedrop', cache)
        f.install('--remove')
        cached_hooks_are_quiet(f, cache)
        publish(new)
        assert not (f.state / 'runtime/retired.json').exists()
        script = f.root / 'cached-agent.py'
        script.write_text(AGENT.replace('[launcher,"hook","session-start","--state-directory",state]',
                                        '["/bin/sh",launcher]'))
        before_bindings = set((f.state / 'rex/sessions').glob('*.json'))
        f.session('u8-cached-agent', sys.executable, '-I', str(script), str(cache / 'scripts/session-start'),
                  str(f.state), str(f.root / 'end-cached'))
        wait(lambda: set((f.state / 'rex/sessions').glob('*.json')) - before_bindings, bool,
             'the cached hook binds again after reinstall')

        if sys.platform == 'darwin':
            stable = f.launcher
            f.launcher = (f.state / 'runtime/current/namedrop').resolve()
            try:
                removal, hold = paused(f, '/runtime/commands.lock', 'SH', 'install', '--remove', '--input-json', CONFIRMED)
            finally:
                f.launcher = stable
            publish(new)
            running = start_helpers(f, session, block, first=len(list((f.state / 'rex/sessions').glob('*.json'))))
            files, module = shared(f), (f.config.parent / 'namedrop.lua').read_bytes()
            hold.unlink()
            stdout, stderr = removal.communicate(timeout=60)
            assert removal.returncode == 0 and 'Namedrop was installed again. Removal stopped.' in stdout, (stdout, stderr)
            assert {pid: identity(pid) for pid in running} == {pid: before for pid, (_, before) in running.items()}
            assert shared(f) == files and (f.config.parent / 'namedrop.lua').read_bytes() == module
        say('U8 real Rex runtime preflight, tour, upgrade, cached-hook reinstall and removal race passed')
    finally:
        f.close()



def section_updates():
    import sqlite3
    for mode in ('same', 'migrated', 'interrupted'):
        migrated_failure = mode == 'migrated'
        f = Fixture('updates-' + mode, b'-- U10 config\n')
        try:
            shutil.rmtree(f.state)
            f.state.mkdir()
            tools = f.root / 'tools'
            tools.mkdir()
            (tools / 'rex').symlink_to(rex)
            f.env['PATH'] = str(tools) + ':/usr/bin:/bin'
            roots = []
            for key, label in [('NAMEDROP_TEST_STANDALONE_BASELINE', 'baseline'), ('NAMEDROP_TEST_ARCHIVE', 'incoming')]:
                folder = f.root / label
                with tarfile.open(os.environ[key]) as archive:
                    archive.extractall(folder, filter='data')
                roots.append(next(folder.iterdir()))
            old, new = roots
            schemas = [json.loads((root / 'runtime.json').read_text())['state_schema'] for root in roots]
            assert schemas[0] != schemas[1], schemas

            def publish(source, code=0):
                result = subprocess.run(['/bin/sh', str(source / 'scripts/install.sh'), '--yes'],
                                        env=f.env, input='', capture_output=True, text=True, timeout=90)
                with (f.root / 'updates.jsonl').open('a') as log:
                    log.write(json.dumps(dict(source=str(source), code=result.returncode,
                                              stdout=result.stdout, stderr=result.stderr)) + '\n')
                assert result.returncode == code, (result.returncode, result.stdout, result.stderr)
                return result

            def store():
                with contextlib.closing(sqlite3.connect(f.state / 'namedrop.sqlite3')) as db:
                    return (db.execute('PRAGMA user_version').fetchone()[0],
                            db.execute('SELECT * FROM alias_claims ORDER BY 1,2,3').fetchall())

            def doctor_runs_selected(session, block):
                existing = {row[0] for row in processes_under(f.root)}
                before = set(f.blocks(session))
                f.rx('do', '-s', session, '-b', block, 'namedrop_doctor')
                layer = next(iter(wait(lambda: set(f.blocks(session)) - before, bool, 'Doctor opens')))
                executable = str((f.state / 'runtime/current/namedrop').resolve())
                wait(lambda: processes_under(f.root),
                     lambda rows: any(row[0] not in existing and os.path.realpath(row[1]) == executable
                                      and all(word in row[2] for word in ('ui', 'run', 'doctor'))
                                      for row in rows), 'Doctor runs the selected runtime')
                f.rx('send-key', '-s', session, '-b', layer, 'escape')
                wait(lambda: f.blocks(session), lambda rows: layer not in rows, 'Doctor closes')

            publish(old)
            session, block = f.session('u10-survivor')
            f.in_block(session, block, 'name', 'survivor', '--type', 'terminal')
            before = store()
            assert 'survivor' in str(before[1]), before
            if mode == 'interrupted':
                publisher = new / 'scripts/runtime.py'
                original = publisher.read_text()
                anchor = 'def migrate_selected(runtime, target, gate):\n'
                assert anchor in original
                publisher.write_text(original.replace(anchor, anchor +
                    "    if os.environ.get('NAMEDROP_U10_PAUSE'):\n"
                    "        Path(os.environ['NAMEDROP_U10_PAUSE']).write_text(str(os.getpid()))\n"
                    "        while True: time.sleep(1)\n", 1))
                marker = f.root / 'migration-paused'
                with (f.root / 'interrupted.log').open('w') as log:
                    pending = subprocess.Popen([sys.executable, str(publisher), str(new), '--state-dir',
                                                str(f.state), '--verify', 'install'],
                                               env=dict(f.env, NAMEDROP_U10_PAUSE=str(marker)),
                                               stdout=log, stderr=subprocess.STDOUT)
                    with (f.root / 'pids.jsonl').open('a') as ledger:
                        ledger.write(json.dumps(dict(pid=pending.pid, purpose='interrupted migration')) + '\n')
                    try:
                        wait(lambda: marker.exists() or pending.poll() is not None, bool, 'migration pauses')
                        assert marker.exists() and pending.poll() is None
                    finally:
                        if pending.poll() is None:
                            pending.kill()
                        pending.wait(timeout=10)
                publisher.write_text(original)
                assert (f.state / 'runtime/migration.json').is_file()
                refused = f.in_block(session, block, 'doctor', ok=False)
                assert refused.returncode == 69 and 'Namedrop activation was interrupted.' in (refused.stdout + refused.stderr)
                assert store() == before
                publish(new)
                assert not (f.state / 'runtime/migration.json').exists()
                assert store() == (schemas[1], before[1]), store()
                doctor_runs_selected(session, block)
                say('U10 interrupted migration recovery passed')
                continue
            if not migrated_failure:
                selected = os.readlink(f.state / 'runtime/current')
                invalid = f.root / 'invalid-contract'
                shutil.copytree(new, invalid)
                contract = invalid / 'runtime.json'
                value = json.loads(contract.read_text())
                value['state_schema'] += 1
                contract.write_text(json.dumps(value))
                refused = publish(invalid, code=2)
                assert 'Built runtime does not match' in refused.stderr
                assert os.readlink(f.state / 'runtime/current') == selected and store() == before
                version = re.search(r'^version\s*=\s*"([^"]+)"', (new / 'herdr-plugin.toml').read_text(), re.M)[1]
                git = tools / 'git'
                git.write_text('#!/bin/sh\nprintf "%s\\n" "' + 'b' * 40 + '\trefs/tags/v' + version + '"\n')
                git.chmod(0o700)
                checked = subprocess.run([sys.executable, str(repo / 'scripts/check-update.py'), str(f.state),
                                          str(f.home / '.config/namedrop/settings.json')], env=f.env,
                                         capture_output=True, text=True, timeout=20)
                assert checked.returncode == 0, checked.stderr
                notice = f.in_block(session, block, 'doctor').stdout
                assert 'ND-UPDATE-001' in notice and ('download Namedrop ' + version) in notice, notice
                publish(new)
                assert store() == (schemas[1], before[1]), store()
                before = store()
                candidate = f.root / 'same-schema'
                shutil.copytree(new, candidate)
                executable = candidate / 'bin/namedrop'
                if sys.platform == 'darwin':
                    subprocess.run(['/usr/bin/codesign', '--force', '--sign', '-', '--identifier',
                                    'namedrop-u10-failed-install', str(executable)], check=True, capture_output=True)
                else:
                    with executable.open('ab') as stream:
                        stream.write(b'\nnamedrop-u10-failed-install\n')
            else:
                candidate = new
            selected = os.readlink(f.state / 'runtime/current')
            module = Path(registration(f)['module'])
            contents = module.read_bytes()
            module.write_text('-- foreign module: fixture install must refuse\n')
            failed = publish(candidate, code=1)
            module.write_bytes(contents)
            after = os.readlink(f.state / 'runtime/current')
            if migrated_failure:
                assert after != selected
                assert 'Activation failed after a possible state migration. The new runtime remains selected;' in failed.stderr
                assert store() == (schemas[1], before[1]), store()
            else:
                assert after == selected
                assert 'The previous runtime remains selected; existing sessions can continue.' in failed.stderr
                assert store() == before
            doctor_runs_selected(session, block)
            say('U10 migrated failure' if migrated_failure else 'U10 detection, migration and same-schema rollback', 'passed')
        finally:
            f.close()


for section in chosen:
    say('== ' + section)
    globals()['section_' + section]()
say('passed:', ' '.join(chosen))
