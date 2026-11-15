"""The update check's start on Rex, at the binary (the U10 review's F1).
Nothing on Rex runs `reconcile` or a lifecycle event, so a standalone
install's bridge starts the check once it connects to its server. A bridge
on any other state never does, nor one whose settings opt out or whose
check ran within the day.

Each case runs `namedrop rex bridge` against its own scripted server. A
`python3` first on the bridge's PATH stands in for the check and records
its arguments, so nothing runs `git`.

Usage: python3 -I tests/update_bridge.py BINARY
"""
import contextlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import sys
import tempfile
import time

spec = importlib.util.spec_from_file_location('scripted_rex', Path(__file__).resolve().parent / 'support/scripted_rex.py')
scripted_rex = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scripted_rex)
BINARY = Path(sys.argv[1]).resolve()
root = Path(tempfile.mkdtemp(prefix='nd-u10-bridge-', dir='/tmp')).resolve()
tools = root / 'tools'
tools.mkdir()
(tools / 'python3').write_text('#!/bin/sh\nprintf "%s\\n" "$1" "$3" "$4" > "$3/check-started"\n')
(tools / 'python3').chmod(0o700)
(tools / 'git').write_text('#!/bin/sh\necho "git ran" >&2\nexit 1\n')
(tools / 'git').chmod(0o700)
DAY_MS = 86_400_000


def answers(sock):
    """A server at `sock` that holds a bridge's stream open, with no events."""
    def answer(method):
        if method == 'server.status':
            return {'pid': os.getpid(), 'listeners': [{'primary': True, 'transport': 'unix', 'addresses': [str(sock)]}]}
        if method in ('session.attach_all', 'client.set_label', 'client.keymap'):
            return {}
        raise EOFError
    return answer


def ours():
    """This test's processes, by pid: each names a path under `root`."""
    rows = subprocess.run(['/bin/ps', '-axo', 'pid=,command='], capture_output=True, text=True).stdout.splitlines()
    return [int(row.split(None, 1)[0]) for row in rows if str(root) in row and int(row.split(None, 1)[0]) != os.getpid()]


def case(name, owner, settings=None, checked_at=None):
    place = root / name
    home, state = place / 'home', place / 'state'
    (state / 'runtime').mkdir(parents=True)
    home.mkdir()
    if isinstance(owner, bytes):
        (state / 'runtime/owner').write_bytes(owner)
    elif owner == 'directory':
        (state / 'runtime/owner').mkdir()
    if settings is not None:
        (home / '.config/namedrop').mkdir(parents=True)
        (home / '.config/namedrop/settings.json').write_text(settings)
    if checked_at is not None:
        (state / 'update-check.json').write_text(json.dumps(dict(
            schema_version=1, checked_at=checked_at, source=None, remote_commit=None,
            remote_version=None, last_success_at=None, last_error=None)))
    sock = place / 'r.sock'
    server = scripted_rex.Server(sock, answers(sock))
    bridge = subprocess.Popen([str(BINARY), 'rex', 'bridge', '--endpoint', 'rex:' + str(sock)],
                              env={'HOME': str(home), 'PATH': str(tools) + ':/usr/bin:/bin',
                                   'NAMEDROP_STATE_DIR': str(state)},
                              stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
    return dict(name=name, home=home, state=state, server=server, bridge=bridge,
                started=state / 'check-started')


def wait(condition, description, within=15):
    until = time.monotonic() + within
    while not condition():
        assert time.monotonic() < until, description
        time.sleep(.05)


now_ms = int(time.time() * 1000)
cases = []
try:
    cases = [case('standalone', b'standalone\n'),
             case('herdr', b'herdr\n'),
             case('unowned', None),
             case('invalid', b'Herdr\n'),
             case('a-directory', 'directory'),
             case('opted-out', b'standalone\n', settings='{"check_for_updates": false}'),
             case('checked-today', b'standalone\n', checked_at=now_ms - 60_000)]
    for each in cases:
        wait(lambda: 'session.attach_all' in each['server'].calls or each['bridge'].poll() is not None,
             each['name'] + ': the bridge connects')
        assert each['bridge'].poll() is None, (each['name'], each['bridge'].stderr.read())
    standalone = cases[0]
    wait(standalone['started'].exists, 'the standalone bridge starts the update check')
    flag, state, settings = standalone['started'].read_text().splitlines()
    assert (flag, state, settings) == ('-c', str(standalone['state']),
                                       str(standalone['home'] / '.config/namedrop/settings.json')), (flag, state, settings)
    # A bridge starts the check as soon as it connects, before its first
    # wait, so each other bridge had its chance by now.
    time.sleep(2)
    for each in cases[1:]:
        assert not each['started'].exists(), each['name'] + ': the bridge started the update check'
    # The bridge outlives its check, so it reaps the check as it exits.
    rows = subprocess.run(['/bin/ps', '-axo', 'ppid=,stat='], capture_output=True, text=True).stdout.splitlines()
    zombies = [row for row in rows if row.split()[0] == str(standalone['bridge'].pid) and 'Z' in row.split()[1]]
    assert not zombies, ('the bridge left its check unreaped', zombies)
    print('update check: started by a standalone bridge only')
finally:
    for each in cases:
        if each['bridge'].poll() is None:
            each['bridge'].send_signal(signal.SIGTERM)
        with contextlib.suppress(subprocess.TimeoutExpired):
            each['bridge'].wait(10)
    for _ in range(3):
        for pid in ours():
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, signal.SIGTERM)
        if not ours():
            break
        time.sleep(1)
    left = ours()
    for each in cases:
        each['server'].close()
    shutil.rmtree(root, ignore_errors=True)
    assert not left, ('processes left', left)
