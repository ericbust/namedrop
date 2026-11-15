"""The load-time start, `namedrop rex ensure --from-server`, at the binary,
against a scripted server that stands in for Rex and starts the command
itself, as Rex's server does at config load.

A start whose parent is the socket's peer starts one bridge, keyed by the
endpoint the server names itself by, however REX_SERVER spells the socket
(the U6 review's F1). A start with any other parent, here a shell between
the server and the command, starts nothing and writes nothing.

Usage: python3 -I tests/rex_ensure.py BINARY
"""
import contextlib
import fcntl
import hashlib
import importlib.util
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
root = Path(tempfile.mkdtemp(prefix='nd-u6-ensure-', dir='/tmp')).resolve()
real = root / 'real'
real.mkdir()
alias = root / 'alias'
alias.symlink_to(real)
socket_path = real / 'r.sock'


def answer(method):
    if method == 'server.status':
        return {'pid': os.getpid(), 'listeners': [{'primary': True, 'transport': 'unix', 'addresses': [str(socket_path)]}]}
    # The bridge's stream opens with these, then waits for events, of which
    # none come.
    if method in ('session.attach_all', 'client.set_label', 'client.keymap'):
        return {}
    raise EOFError


def held(path):
    with open(path) as f:
        try:
            fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return True
    return False


def ours():
    """This test's processes, by pid: each names a path under `root`."""
    rows = subprocess.run(['/bin/ps', '-axo', 'pid=,command='], capture_output=True, text=True).stdout.splitlines()
    return [int(row.split(None, 1)[0]) for row in rows if str(root) in row and int(row.split(None, 1)[0]) != os.getpid()]


server = scripted_rex.Server(socket_path, answer)
try:
    for case in ['accepted', 'through-a-shell']:
        home, state = root / case / 'home', root / case / 'state'
        home.mkdir(parents=True)
        env = {'HOME': str(home), 'PATH': '/usr/bin:/bin', 'NAMEDROP_STATE_DIR': str(state),
               'REX_SERVER': 'unix://' + str(alias / 'r.sock')}
        argv = [str(BINARY), 'rex', 'ensure', '--from-server']
        if case == 'through-a-shell':
            argv = ['/bin/sh', '-c', '"$@"; exit $?', 'sh', *argv]
        calls = len(server.calls)
        p = subprocess.run(argv, env=env, capture_output=True, timeout=30)
        assert p.returncode == 0 and not p.stdout and not p.stderr, (case, p)
        if case == 'through-a-shell':
            assert not state.exists() and not any(home.iterdir()), case
            assert len(server.calls) == calls, 'a refused start called the server'
            continue
        lock = state / 'rex-bridge' / (hashlib.sha256(('rex:' + str(socket_path)).encode()).hexdigest() + '.lock')
        until = time.monotonic() + 15
        while not (lock.is_file() and held(lock)):
            assert time.monotonic() < until, ('no bridge holds the proven endpoint\'s lock', sorted(os.listdir(state / 'rex-bridge')) if (state / 'rex-bridge').exists() else None)
            time.sleep(.05)
        assert os.listdir(state / 'rex-bridge') == [lock.name], 'a bridge keyed by another spelling'
    print('load-time start: accepted from the server, keyed by its listener, refused through a shell')
finally:
    for _ in range(3):
        for pid in ours():
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, signal.SIGTERM)
        if not ours():
            break
        time.sleep(1)
    left = ours()
    server.close()
    shutil.rmtree(root, ignore_errors=True)
    assert not left, ('processes left', left)
