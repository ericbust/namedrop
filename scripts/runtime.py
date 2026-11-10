"""Publish a compatible runtime without removing tools used by existing sessions."""

import contextlib
import ctypes
import fcntl
import hashlib
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import signal
import stat
import sqlite3
import subprocess
import sys
import tempfile
import time

from update import require, session_panes, preflight, ready, is_codex, built_executable

LAUNCHER = '''#!/bin/sh
# Managed by Namedrop: runtime launcher v1.
set -eu
namedrop_runtime=$(CDPATH= cd -- "${0%/*}" && pwd -P)
exec "$namedrop_runtime/current/namedrop" "$@"
'''


RELEASE = re.compile(r'[0-9a-f]{64}')


def contract(root):
    value = json.loads((root / 'runtime.json').read_text())
    require(isinstance(value, dict) and type(value.get('runtime_protocol')) is int and value['runtime_protocol'] == 1
            and type(value.get('live_upgrade_epoch')) is int and value['live_upgrade_epoch'] > 0
            and type(value.get('state_schema')) is int and value['state_schema'] > 0
            and isinstance(value.get('collaboration_protocol'), str),
            'This release does not support the installed runtime launcher.')
    require(value.get('command_gate', 0) in (0, 1)
            and type(value.get('command_gate', 0)) is int
            and isinstance(value.get('live_migrate_from', []), list)
            and all(type(schema) is int and 0 < schema < value['state_schema']
                    for schema in value.get('live_migrate_from', [])),
            'Invalid live migration compatibility declaration.')
    return value


def live_compatible(previous, incoming):
    """Only a release's explicitly supported forward migrations can run live."""
    keys = ('runtime_protocol', 'live_upgrade_epoch', 'collaboration_protocol')
    if any(previous[key] != incoming[key] for key in keys):
        return False
    return (previous['state_schema'] == incoming['state_schema']
            or incoming.get('command_gate') == 1
            and previous['state_schema'] in incoming.get('live_migrate_from', []))


def owned_directory(path):
    # The state directory is trusted; everything claimed below it must be local.
    if not path.exists():
        path.mkdir(mode=0o700)
    require(not path.is_symlink() and path.is_dir(), f'Runtime path is not a regular directory: {path}')


def current(runtime):
    link = runtime / 'current'
    if not link.is_symlink():
        require(not link.exists(), 'Runtime current pointer is not a symbolic link.')
        return None
    target = Path(os.readlink(link))
    require(len(target.parts) == 2 and target.parts[0] == 'releases' and RELEASE.fullmatch(target.parts[1]),
            'Runtime current pointer has an invalid target.')
    release = runtime / target
    require(not (runtime/'releases').is_symlink() and not release.is_symlink() and release.is_dir(), 'The active runtime is missing.')
    contract(release)
    return target


def write_owned(path, contents, mode):
    require(not path.is_symlink() and (not path.exists() or path.is_file()),
            f'Runtime file has an unexpected type: {path}')
    descriptor, temporary = tempfile.mkstemp(prefix='.write-', dir=path.parent)
    try:
        with os.fdopen(descriptor, 'w') as output:
            output.write(contents)
            os.fchmod(output.fileno(), mode)
        os.replace(temporary, path)
    finally:
        Path(temporary).unlink(missing_ok=True)


def record_legacy(runtime, *, first=False, codex_ready=False, packages_ready=False):
    panes, unverified = session_panes()
    sessions = [[p['_socket'], (p.get('agent_session') or {}).get('value')]
                for p in panes if (p.get('agent') or p.get('agent_session')) and (first or is_codex(p))]
    require(not unverified and all(item[1] for item in sessions),
            'Could not identify existing sessions for the one-time runtime migration. Retry activation when Herdr can report their identities.')
    path = runtime/'legacy-sessions.json'
    legacy = json.loads(path.read_text()) if path.exists() else {'sessions': []}
    legacy['sessions'] = sorted({tuple(item) for item in legacy['sessions'] + sessions})
    legacy['codex_ready'] = codex_ready
    legacy['packages_ready'] = packages_ready
    write_owned(path, json.dumps(legacy) + '\n', 0o600)


WORD = re.compile(r'[a-z][a-z0-9.-]{0,40}')


def command_words(argv):
    """What a Namedrop process is doing, from its executable name and leading
    subcommand words. Flag values can hold message text, so they are skipped and
    any word that is not a plain lowercase token ends the description."""
    words = [os.path.basename(argv[0])] if argv else []
    rest = iter(argv[1:])
    for word in rest:
        if word.startswith('--'):
            if '=' not in word:
                next(rest, None)
            continue
        if not WORD.fullmatch(word) or len(words) > 3:
            break
        words.append(word)
    return ' '.join(words) or 'unknown'


def gate_holders(path):
    """PIDs other than this one with the command lock open, and what each is."""
    target, me = os.path.realpath(path), os.getpid()
    if sys.platform == 'darwin':
        # Namedrop's own executables are asked about first, cheaply. Only when
        # none holds the lock does lsof scan every process; this is the error path.
        def listed(arguments):
            try:
                found = subprocess.run(['/usr/sbin/lsof', '-t', *arguments, '--', target],
                                       capture_output=True, text=True, timeout=20)
            except (OSError, subprocess.SubprocessError):
                return set()
            return {int(pid) for pid in found.stdout.split() if pid.isdigit()} - {me}
        try:
            executables = retained_executables(Path(path).parent)
            candidates = [pid for pid in all_pids() if pid != me
                          and process(pid, executables)]
        except (OSError, RuntimeError):
            executables, candidates = {}, []
        holding = listed(['-a', '-p', ','.join(map(str, candidates))]) if candidates else set()
        holding = holding or listed([])
        holders = []
        for pid in sorted(holding):
            found = process(pid)
            holders.append((pid, command_words(found[1]) if found and found[1] else 'unknown',
                            executables.get(os.path.realpath(found[0])) if found else None))
        return holders
    try:
        executables = retained_executables(Path(path).parent)
    except OSError:
        executables = {}
    holders = []
    for entry in os.listdir('/proc'):
        if not entry.isdigit() or int(entry) == me or not opens(int(entry), target):
            continue
        found = process(int(entry))
        if found and found[1]:
            holders.append((int(entry), command_words(found[1]), executables.get(os.path.realpath(found[0]))))
    return sorted(holders)


def busy_gate_message(path):
    """Each holder is (PID, Namedrop subcommand, whether its release holds the
    gate only per job: True, False before 1.7.0, None when not a release)."""
    holders = gate_holders(path)
    if not holders:
        return ('Namedrop commands are still finishing. Close any open Namedrop popup, such as the tour, '
                'and retry the update; agent sessions can stay open.')
    named = ' '.join(f'PID {pid} ({what}) holds the command lock.' for pid, what, _ in holders)
    popups = [pid for pid, what, _ in holders if ' ui ' in f' {what} ']
    # A status worker from before 1.7.0 holds the lock between jobs too; any
    # other holder is in the middle of a job, such as a delivery, so it is
    # waited for rather than stopped.
    old_workers = [pid for pid, what, per_job in holders
                   if pid not in popups and 'agent-status-worker' in what and per_job is False]
    working = [pid for pid, _, _ in holders if pid not in popups + old_workers]
    steps = []
    if popups:
        steps.append('close the open Namedrop popup, such as the tour')
    if old_workers:
        steps.append("stop the status worker from the earlier version with `kill "
                     + ' '.join(map(str, old_workers)) + '`, which loses nothing because its events stay queued')
    if working:
        steps.append('wait a moment for the command that is still working to finish')
    return (f'Namedrop commands are still finishing. {named} To continue, ' + '; '.join(steps)
            + '. Then retry the update; agent sessions can stay open.')


def lock(runtime, name, timeout=None):
    descriptor = os.open(runtime / name, os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    stream = os.fdopen(descriptor, 'r+')
    require(stat.S_ISREG(os.fstat(descriptor).st_mode), 'Runtime lock must be a regular file.')
    try:
        if timeout is None:
            fcntl.flock(stream, fcntl.LOCK_EX)
        else:
            deadline = time.monotonic() + timeout
            while True:
                try:
                    fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    break
                except BlockingIOError:
                    if time.monotonic() >= deadline:
                        raise RuntimeError(busy_gate_message(runtime / name)) from None
                    time.sleep(.05)
    except BaseException:
        stream.close()
        raise
    return stream


# macOS process inspection goes through libproc and sysctl, not ps or lsof
# scans of every process, which can take seconds on a busy machine.
CTL_KERN, KERN_ARGMAX, KERN_PROCARGS2 = 1, 8, 49


def darwin_library():
    library = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
    library.proc_listallpids.argtypes = [ctypes.c_void_p, ctypes.c_int]
    library.proc_listallpids.restype = ctypes.c_int
    library.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
    library.proc_pidpath.restype = ctypes.c_int
    return library


def darwin_pids(library):
    count = library.proc_listallpids(None, 0)
    if count > 0:
        buffer = (ctypes.c_int * (count + 256))()
        count = library.proc_listallpids(buffer, ctypes.sizeof(buffer))
    if count <= 0:
        raise RuntimeError('Could not list processes to finish the update. Retry it; no agent was stopped.')
    return [pid for pid in buffer[:count] if pid > 0]


def darwin_path(library, pid):
    executable = ctypes.create_string_buffer(4096)  # PROC_PIDPATHINFO_MAXSIZE
    if library.proc_pidpath(pid, executable, len(executable)) <= 0:
        return None
    return os.fsdecode(executable.value)


def parse_procargs2(data):
    """argv and environment from macOS KERN_PROCARGS2: argc, the exec path,
    padding, then argc NUL-ended arguments and NUL-ended environment strings
    up to an empty one."""
    argc = int.from_bytes(data[:4], sys.byteorder)
    rest = data[4:]
    rest = rest[rest.index(b'\0'):].lstrip(b'\0')
    strings = rest.split(b'\0')
    argv = [os.fsdecode(item) for item in strings[:argc]]
    environment = {}
    for item in strings[argc:]:
        if not item:
            break
        name, _, value = os.fsdecode(item).partition('=')
        environment[name] = value
    return argv, environment


def darwin_arguments(pid):
    libc = ctypes.CDLL(None, use_errno=True)
    argmax = ctypes.c_int(0)
    size = ctypes.c_size_t(ctypes.sizeof(argmax))
    if libc.sysctl((ctypes.c_int * 2)(CTL_KERN, KERN_ARGMAX), 2, ctypes.byref(argmax), ctypes.byref(size),
                   None, ctypes.c_size_t(0)) != 0 or argmax.value <= 0:
        return None
    buffer = ctypes.create_string_buffer(argmax.value)
    size = ctypes.c_size_t(argmax.value)
    if libc.sysctl((ctypes.c_int * 3)(CTL_KERN, KERN_PROCARGS2, pid), 3, buffer, ctypes.byref(size),
                   None, ctypes.c_size_t(0)) != 0:
        return None
    return parse_procargs2(buffer.raw[:size.value])


def process(pid, executables=None):
    """(executable, argv, environment) of a process of this user, or None. With
    `executables`, a process running anything else is skipped before its
    arguments are read."""
    try:
        if sys.platform == 'darwin':
            executable = darwin_path(darwin_library(), pid)
            if executable is None or executables is not None and os.path.realpath(executable) not in executables:
                return None
            arguments = darwin_arguments(pid)
            return None if arguments is None else (executable, *arguments)
        executable = os.readlink(f'/proc/{pid}/exe')
        if executables is not None and os.path.realpath(executable) not in executables:
            return None
        argv = [os.fsdecode(a) for a in Path(f'/proc/{pid}/cmdline').read_bytes().split(b'\0') if a]
        environment = {}
        for item in Path(f'/proc/{pid}/environ').read_bytes().split(b'\0'):
            name, _, value = os.fsdecode(item).partition('=')
            if name:
                environment[name] = value
        return executable, argv, environment
    except (OSError, ValueError, RuntimeError, IndexError):
        return None


def all_pids():
    if sys.platform == 'darwin':
        return darwin_pids(darwin_library())
    return [int(entry) for entry in os.listdir('/proc') if entry.isdigit()]


def retained_executables(runtime):
    """Each retained release's executable, mapped to whether its helpers hold
    the command gate only while they work (`helper_gate`, from 1.7.0)."""
    found = {}
    for release in (runtime / 'releases').iterdir():
        if release.is_dir() and not release.is_symlink() and (release / 'namedrop').is_file():
            try:
                per_job = json.loads((release / 'runtime.json').read_text()).get('helper_gate') == 1
            except (OSError, ValueError, AttributeError):
                per_job = False
            found[str((release / 'namedrop').resolve())] = per_job
    return found


def helper_kind(argv, environment, state):
    """Which Namedrop background helper of `state` this is, or None."""
    rest = argv[1:]
    ours = lambda path: path is not None and os.path.realpath(path) == os.path.realpath(state)
    if rest == ['hook', 'agent-status-worker'] and ours(environment.get('HERDR_PLUGIN_STATE_DIR')):
        return 'status worker'
    if len(rest) == 3 and ours(rest[1]) and rest[0] == '--deferred-worker':
        return 'deferred worker'
    if len(rest) == 3 and ours(rest[1]) and rest[0] == 'reminder-clock':
        return 'reminder clock'
    return None


def opens(pid, path):
    try:
        return any(os.readlink(f'/proc/{pid}/fd/{fd}') == path for fd in os.listdir(f'/proc/{pid}/fd'))
    except OSError:
        return False


def gate_helpers(runtime):
    """Helpers activation may stop: status workers of this state, run by a
    retained release from before 1.7.0, that have the command lock open. Those
    hold the gate for their whole life, waiting out retries included. Every
    other helper holds it only while it works, so it is waited for, never
    stopped, and agents and every other process are never included."""
    try:
        stoppable = {path for path, per_job in retained_executables(runtime).items() if not per_job}
        if not stoppable:
            return []
        target = os.path.realpath(runtime / 'commands.lock')
        helpers = []
        for pid in all_pids():
            if pid == os.getpid():
                continue
            found = process(pid, stoppable)
            if found and helper_kind(found[1], found[2], runtime.parent) == 'status worker':
                helpers.append({'pid': pid, 'kind': 'status worker', 'executable': found[0],
                                'argv': found[1], 'environment': found[2]})
    except (OSError, RuntimeError, ValueError):
        return []
    if sys.platform != 'darwin':
        return [helper for helper in helpers if opens(helper['pid'], target)]
    if not helpers:
        return []
    try:
        listed = subprocess.run(['/usr/sbin/lsof', '-a', '-t', '-p', ','.join(str(h['pid']) for h in helpers),
                                 '--', target], capture_output=True, text=True, timeout=5)
    except (OSError, subprocess.SubprocessError):
        # Without proof that it holds the lock, a helper is left alone.
        return []
    holding = {int(pid) for pid in listed.stdout.split() if pid.isdigit()}
    return [helper for helper in helpers if helper['pid'] in holding]


def gone(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return True
    try:
        return Path(f'/proc/{pid}/stat').read_text().rsplit(')', 1)[1].split()[0] == 'Z'
    except (OSError, IndexError):
        return False


def same_helper(helper):
    """Whether the PID still runs this helper, not a process that reused it."""
    found = process(helper['pid'])
    return found is not None and found[0] == helper['executable'] and found[1] == helper['argv']


def signal_helper(helper, number):
    if not same_helper(helper):
        return False
    try:
        os.kill(helper['pid'], number)
    except ProcessLookupError:
        return False
    return True


def stop_helpers(helpers, stopped):
    """Signals each helper, recording it in `stopped` first, so an interrupt
    partway through still restarts every helper already signalled."""
    signalled = []
    for helper in helpers:
        stopped.append(helper)
        if signal_helper(helper, signal.SIGTERM):
            signalled.append(helper)
        else:
            stopped.remove(helper)
    deadline = time.monotonic() + 3
    while not all(gone(helper['pid']) for helper in signalled) and time.monotonic() < deadline:
        time.sleep(.05)
    for helper in signalled:
        if not gone(helper['pid']):
            signal_helper(helper, signal.SIGKILL)
    return signalled


def queue_wake(helper):
    """A status worker keeps its next timed retry only in memory, so its
    replacement is given a wake, which drains and schedules that retry again."""
    environment = helper['environment']
    state, socket, workspace = (environment.get(key) for key in
                                ('HERDR_PLUGIN_STATE_DIR', 'HERDR_SOCKET_PATH', 'HERDR_WORKSPACE_ID'))
    if not (state and socket and workspace):
        return
    queue = (Path(state) / 'agent-status-reconciliation'
             / hashlib.sha256(f'{socket}\0{workspace}'.encode()).hexdigest())
    queue.mkdir(parents=True, exist_ok=True, mode=0o700)
    name = f'activation-{time.time_ns()}'
    queued = queue / f'queued-{name}'
    queued.write_text(json.dumps({'operation': 'mailbox-wake'}))
    queued.chmod(0o600)
    queued.rename(queue / f'pending-{name}')


def restart_helpers(runtime, helpers):
    """Start each stopped helper again through the launcher, so it runs the
    selected runtime with the arguments and environment it had. A helper that
    can't be restarted is reported, never raised: the update itself is done,
    and the helper starts again on its next event."""
    restarted, failed = [], []
    # Process listing order differs between Linux and macOS; the report doesn't.
    for helper in sorted(helpers, key=lambda helper: (helper['kind'], helper['pid'])):
        if helper['kind'] == 'status worker':
            try:
                queue_wake(helper)
            except (OSError, ValueError) as error:
                print(f"Could not queue a wake for Namedrop's status worker (was PID {helper['pid']}): {error}. "
                      'A timed retry it was waiting on runs at its next event.', file=sys.stderr)
        try:
            subprocess.Popen([str(runtime / 'namedrop'), *helper['argv'][1:]], env=helper['environment'],
                             cwd='/', stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                             stderr=subprocess.DEVNULL, start_new_session=True, close_fds=True)
            restarted.append(helper)
        except (OSError, ValueError, subprocess.SubprocessError) as error:
            failed.append((helper, error))
    if restarted:
        print('Restarted Namedrop helpers after the update: '
              + ', '.join(f"{helper['kind']} (was PID {helper['pid']})" for helper in restarted) + '.',
              file=sys.stderr)
    for helper, error in failed:
        print(f"Could not restart Namedrop's {helper['kind']} (was PID {helper['pid']}): {error}. "
              'It starts again on its next event.', file=sys.stderr)


@contextlib.contextmanager
def command_gate(runtime, timeout=20, grace=2):
    """The exclusive command gate. A command, popup or helper in the middle of
    its work is waited for. A status worker from before 1.7.0 still holding the
    gate after `grace` is stopped, because it holds the gate for its whole life,
    and it starts again once the gate is released."""
    descriptor = os.open(runtime / 'commands.lock', os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW, 0o600)
    stream = os.fdopen(descriptor, 'r+')
    stopped = []
    try:
        require(stat.S_ISREG(os.fstat(descriptor).st_mode), 'Runtime lock must be a regular file.')
        deadline = time.monotonic() + timeout
        check_at = time.monotonic() + grace
        while True:
            try:
                fcntl.flock(stream, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                now = time.monotonic()
                if now >= deadline:
                    raise RuntimeError(busy_gate_message(runtime / 'commands.lock')) from None
                if now >= check_at:
                    stop_helpers(gate_helpers(runtime), stopped)
                    check_at = time.monotonic() + grace
                time.sleep(.05)
    except BaseException:
        stream.close()
        restart_helpers(runtime, stopped)
        raise
    try:
        with stream:
            yield stream
    finally:
        restart_helpers(runtime, stopped)


def state_schema(runtime):
    path = runtime.parent / 'namedrop.sqlite3'
    with sqlite3.connect(path.as_uri() + '?mode=ro', uri=True, timeout=2) as db:
        return db.execute('PRAGMA user_version').fetchone()[0]


def drain_legacy_commands(runtime, timeout=20):
    """Bridge older releases that did not participate in the command gate.

    The new pointer is already published behind an exclusive gate. Old entrypoints
    forward to it, while old commands which passed that check may finish once.
    Only retained, exact Namedrop executable paths are observed; never stop agents.
    """
    paths = set()
    for release in (runtime / 'releases').iterdir():
        if release.is_dir() and not release.is_symlink() and contract(release).get('command_gate') != 1:
            paths.add(str((release / 'namedrop').resolve()))
    if not paths:
        return
    # Only macOS releases predate the command gate; no Linux release lacks it.
    require(sys.platform == 'darwin',
            'Found a runtime release without the command gate, which only older macOS releases have. '
            'Inspect the runtime directory; no agent was stopped.')
    # libproc reports each process's executable vnode path, not its arguments
    # and not the launch path through the switched current symlink, and needs
    # no subprocess, so a busy machine cannot time it out. No PID is killed.
    library = darwin_library()
    deadline = time.monotonic() + timeout
    while True:
        running = [str(pid) for pid in sorted(darwin_pids(library))
                   if pid != os.getpid() and darwin_path(library, pid) in paths]
        if not running:
            return
        require(time.monotonic() < deadline,
                'Older Namedrop commands are still finishing (PIDs ' + ', '.join(running)
                + '). Retry the update shortly; no agent was stopped.')
        time.sleep(.05)


def migrate_selected(runtime, target, gate):
    result = subprocess.run([str(runtime / target / 'namedrop'),
                             '--migrate-runtime-state', str(runtime.parent)],
                            capture_output=True, text=True, timeout=15,
                            env=dict(os.environ, NAMEDROP_MIGRATION_GATE_FD=str(gate.fileno())),
                            pass_fds=(gate.fileno(),))
    require(result.returncode == 0,
            'State migration did not finish: ' + result.stderr.strip())
    expected = contract(runtime / target)['state_schema']
    require(json.loads(result.stdout) == {'state_schema': expected}
            and state_schema(runtime) == expected, 'State migration was not verified.')


def migration_record(runtime):
    path = runtime / 'migration.json'
    require(not path.is_symlink() and path.is_file(), 'Invalid runtime migration record.')
    value = json.loads(path.read_text())
    require(isinstance(value, dict) and set(value) == {'previous', 'target'},
            'Invalid runtime migration record.')
    for name in ('previous', 'target'):
        target = Path(value[name])
        require(len(target.parts) == 2 and target.parts[0] == 'releases' and RELEASE.fullmatch(target.parts[1]),
                'Invalid runtime migration target.')
        release = runtime / target
        require(not release.is_symlink() and not (release / 'namedrop').is_symlink()
                and hashlib.sha256((release / 'namedrop').read_bytes()).hexdigest() == target.name,
                'The retained migration executable failed verification.')
        contract(release)
    return Path(value['previous']), Path(value['target'])


def recover_migration(runtime):
    """A normal activation retry repairs an interrupted switch without data rollback."""
    if not (runtime / 'migration.json').exists() and not (runtime / 'migration.json').is_symlink():
        return
    with command_gate(runtime) as gate:
        previous, target = migration_record(runtime)
        require(current(runtime) in (previous, target), 'Runtime changed during an unfinished migration.')
        schema = state_schema(runtime)
        if schema == contract(runtime / previous)['state_schema']:
            switch(runtime, previous)
        else:
            require(schema == contract(runtime / target)['state_schema'],
                    'Interrupted migration has an unknown database schema; no runtime was restored.')
            switch(runtime, target)
            drain_legacy_commands(runtime)
            migrate_selected(runtime, target, gate)
        (runtime / 'migration.json').unlink()


def switch_and_migrate(runtime, previous, target):
    with command_gate(runtime) as gate:
        record = runtime / 'migration.json'
        require(not record.exists() and not record.is_symlink(), 'An unfinished migration needs recovery.')
        write_owned(record, json.dumps({'previous': str(previous), 'target': str(target)}) + '\n', 0o600)
        try:
            switch(runtime, target)
            drain_legacy_commands(runtime)
            migrate_selected(runtime, target, gate)
        except BaseException:
            # An atomic migration either left the old schema or committed the new
            # one. Never publish an old executable over newer state. Keep the
            # marker after an uncertain/committed failure for normal retry recovery.
            if state_schema(runtime) == contract(runtime / previous)['state_schema']:
                switch(runtime, previous)
                record.unlink()
            raise
        record.unlink()


def switch(runtime, target):
    # Package refresh checks this same lock before writing shared integrations.
    with lock(runtime, 'packages.lock'):
        temporary = runtime / 'next'
        if temporary.exists() or temporary.is_symlink():
            allowed = {target, current(runtime)}
            if (runtime / 'migration.json').exists():
                allowed.update(migration_record(runtime))
            require(temporary.is_symlink() and Path(os.readlink(temporary)) in allowed,
                    'A foreign runtime publication entry needs inspection; nothing was removed.')
            temporary.unlink()
        if target is None:
            (runtime / 'current').unlink(missing_ok=True)
        else:
            temporary.symlink_to(target)
            temporary.replace(runtime / 'current')


def restore(runtime, previous, incoming):
    if previous is None:
        # First setup may already have written wrappers pointing here. Keep the
        # validated executable available for diagnostics and activation retry.
        return
    if previous is not None and contract(runtime / previous)['state_schema'] != incoming['state_schema']:
        try:
            path = runtime.parent/'namedrop.sqlite3'
            with sqlite3.connect(path.as_uri() + '?mode=ro', uri=True) as db:
                schema = db.execute('PRAGMA user_version').fetchone()[0]
            require(schema == contract(runtime / previous)['state_schema'], 'State has already migrated.')
        except (OSError, RuntimeError, sqlite3.Error):
            print('Activation failed after a possible state migration. The new runtime remains selected; inspect setup and restore a matching state backup before rolling back.', file=sys.stderr)
            return
    switch(runtime, previous)


def recorded_previous(runtime, digest):
    """The release before `digest` when activation selects `digest` again: the
    one activation.json recorded for it, or the release it recorded last."""
    try:
        record = json.loads((runtime / 'activation.json').read_text())
    except (OSError, ValueError):
        return None
    if not isinstance(record, dict):
        return None
    name = record.get('previous') if record.get('revision') == digest else record.get('revision')
    return name if isinstance(name, str) and RELEASE.fullmatch(name) else None


def running_executables():
    """The resolved executable of every process this user can inspect. A
    process that exits or can't be read is skipped; failing to list processes
    raises."""
    if sys.platform == 'darwin':
        library = darwin_library()
        def path(pid):
            return darwin_path(library, pid)
    else:
        def path(pid):
            try:
                return os.readlink(f'/proc/{pid}/exe')
            except OSError:
                return None
    return {os.path.realpath(found) for found in map(path, all_pids()) if found}


def prune_releases(runtime, keep):
    """Delete every release outside `keep` that no process runs from. Only
    lowercase 64-hex directories in releases/ count as releases. Each leaves
    releases/ in one rename into .pruning/ before it is removed, so an
    interrupted prune never leaves part of a release there, and the next prune
    removes whatever .pruning/ still holds."""
    if (runtime / 'migration.json').exists() or (runtime / 'migration.json').is_symlink():
        return
    trash = runtime / '.pruning'
    owned_directory(trash)
    for entry in trash.iterdir():
        if entry.is_dir() and not entry.is_symlink():
            shutil.rmtree(entry)
        else:
            entry.unlink()
    releases = runtime / 'releases'
    candidates = {entry.name: os.path.realpath(entry / 'namedrop') for entry in releases.iterdir()
                  if RELEASE.fullmatch(entry.name) and entry.name not in keep
                  and stat.S_ISDIR(entry.lstat().st_mode)}
    if not candidates:
        return
    running = running_executables()
    for name, executable in sorted(candidates.items()):
        if executable not in running:
            os.rename(releases / name, trash / name)
            shutil.rmtree(trash / name)


def activate_codex_plugin(payload, refresh):
    """Register the local package during explicit setup, then verify via activation.

    Only the two known installation commands are accepted. A conflicting source
    needs inspection; setup never removes a marketplace or writes hook trust.
    """
    data = payload['data']
    hook = data.get('agent_integration_hook_activation', {}).get('codex', {})
    if hook.get('cache_current') is True:
        return payload, 0
    steps = hook.get('steps', [])
    commands = [shlex.split(step) for step in steps if step.startswith('codex ')]
    if not commands:
        return payload, 0
    package = data['integration_packages']['codex'].get('path')
    allowed = [['codex', 'plugin', 'marketplace', 'add', package, '--json'],
               ['codex', 'plugin', 'add', 'namedrop@namedrop', '--json']]
    try:
        require(package and Path(package).is_absolute()
                and all(command in allowed for command in commands),
                'Codex marketplace configuration conflicts with Namedrop. Review the reported repair steps, then run setup again.')
        codex = shutil.which('codex')
        if codex is None:
            hook['status'] = 'setup_required'
            hook['steps'] = ['Codex is not on PATH. If you use Codex, install it and run Activate Namedrop again.']
            return payload, 0
        print('Installing the Namedrop Codex plugin…', file=sys.stderr)
        deadline = time.monotonic() + 30
        for command in commands:
            result = subprocess.run([codex, *command[1:]], stdin=subprocess.DEVNULL,
                                    capture_output=True, text=True,
                                    timeout=max(0.01, deadline - time.monotonic()))
            require(result.returncode == 0,
                    f'{shlex.join(command)} failed: {result.stderr.strip() or result.stdout.strip()}')
        verified = refresh()
        require(verified['data'].get('agent_integration_hook_activation', {})
                .get('codex', {}).get('cache_current') is True,
                'Codex installation finished but its installed plugin cache could not be verified.')
        return verified, 0
    except (OSError, RuntimeError, ValueError, KeyError, subprocess.SubprocessError) as error:
        hook['status'] = 'setup_failed'
        hook['error'] = str(error)
        hook['steps'] = [str(error), 'Resolve the issue and run Activate Namedrop again.', *steps]
        return payload, 1


def activate(root, args):
    state = Path(os.environ['HERDR_PLUGIN_STATE_DIR'])
    require(state.is_absolute() and state.is_dir(), 'Activation needs an absolute, existing state directory.')
    runtime = state / 'runtime'
    owned_directory(runtime)
    owned_directory(runtime / 'releases')
    with lock(runtime, 'activation.lock'):
        recover_migration(runtime)
        previous = current(runtime)
        incoming = contract(root)
        live = previous is not None and live_compatible(contract(runtime / previous), incoming)
        if previous is not None:
            if not live or contract(runtime / previous)['state_schema'] != incoming['state_schema']:
                state_before, agents = preflight(live=True) if live else preflight()
                require(ready(state_before, agents),
                        'Some sessions still use pre-launcher tools or incomplete setup. Open Update Namedrop for the specific blockers; nothing was stopped.' if live else
                        'This update needs a session refresh. Open Update Namedrop to install when current work is finished; nothing was stopped.')
        executable = built_executable(root)
        require(executable.is_file(), 'No built Namedrop executable was found. Rebuild with scripts/build.sh, or reinstall Namedrop.')
        reported = subprocess.run([str(executable), '--runtime-info'], capture_output=True, text=True, check=True)
        require(json.loads(reported.stdout) == incoming, 'Built runtime does not match the release compatibility declaration.')
        contents = executable.read_bytes()
        digest = hashlib.sha256(contents).hexdigest()
        target = Path('releases') / digest
        release = runtime / target
        if previous == target:
            before = recorded_previous(runtime, digest)
        else:
            before = previous.name if previous is not None else None
        if release.exists():
            require(not release.is_symlink() and not (release/'namedrop').is_symlink()
                    and not (release/'runtime.json').is_symlink() and contract(release) == incoming
                    and hashlib.sha256((release / 'namedrop').read_bytes()).hexdigest() == digest,
                    'An existing runtime snapshot is inconsistent; no files were overwritten.')
        else:
            with tempfile.TemporaryDirectory(prefix='.prepare-', dir=runtime) as temporary:
                prepared = Path(temporary) / digest
                prepared.mkdir(mode=0o700)
                (prepared/'namedrop').write_bytes(contents)
                (prepared / 'namedrop').chmod(0o700)
                (prepared / 'runtime.json').write_text(json.dumps(incoming) + '\n')
                prepared.rename(release)
        launcher = runtime / 'namedrop'
        if launcher.exists() or launcher.is_symlink():
            require(not launcher.is_symlink() and launcher.is_file() and launcher.read_text() == LAUNCHER,
                    'Runtime launcher is not owned by this Namedrop release.')
        else:
            # Record sessions which may still cache checkout paths. Subsequent
            # activations never reclassify new sessions as legacy sessions.
            require(not (runtime/'legacy-sessions.json').exists() and not (runtime/'legacy-sessions.json').is_symlink(),
                    'An existing runtime migration record needs inspection; nothing was overwritten.')
            record_legacy(runtime, first=True)
            write_owned(launcher, LAUNCHER, 0o700)
        if (previous is not None and incoming.get('command_gate') == 1
                and contract(runtime / previous)['state_schema'] != incoming['state_schema']):
            switch_and_migrate(runtime, previous, target)
        else:
            switch(runtime, target)
        payload = None
        try:
            result = subprocess.run([str(launcher), *args], capture_output=True, text=True)
            if result.returncode == 0:
                payload = json.loads(result.stdout)
                data = payload.get('data', {})
                packages = data.get('integration_packages', {})
                require(payload.get('status') == 'ok' and payload.get('operation') == 'activate'
                        and data.get('activated') is True
                        and data.get('space_id') == os.environ['HERDR_WORKSPACE_ID']
                        and data.get('state_directory') == str(state)
                        and isinstance(packages, dict) and packages
                        and all(package.get('installed') is True for package in packages.values()),
                        'Activation did not verify this workspace, state and integration packages.')
                def refresh():
                    refreshed = subprocess.run([str(launcher), *args], capture_output=True,
                                               text=True, timeout=20, check=True)
                    value = json.loads(refreshed.stdout)
                    require(value.get('status') == 'ok' and value.get('operation') == 'activate'
                            and value['data'].get('space_id') == data['space_id']
                            and value['data'].get('state_directory') == str(state)
                            and value['data'].get('activated') is True
                            and all(p.get('installed') is True for p in
                                    value['data']['integration_packages'].values()),
                            'Could not verify activation after Codex installation.')
                    return value
                payload, result.returncode = activate_codex_plugin(payload, refresh)
                data = payload['data']
                migration = json.loads((runtime/'legacy-sessions.json').read_text())
                if result.returncode == 0 and (not migration['packages_ready'] or not migration['codex_ready']):
                    # Freeze Codex's cutoff only after its enabled host cache is
                    # verified, including sessions opened during cache refresh.
                    cache_ready = data.get('agent_integration_hook_activation', {}).get('codex', {}).get('migration_ready') is True
                    record_legacy(runtime, first=not migration['packages_ready'], codex_ready=cache_ready, packages_ready=True)
        except BaseException:
            restore(runtime, previous, incoming)
            raise
        if result.returncode:
            restore(runtime, previous, incoming)
            print('Activation did not complete. The previous runtime remains selected; existing sessions can continue.'
                  if previous is not None and current(runtime) == previous else 'Inspect setup before starting sessions.', file=sys.stderr)
        if payload is not None:
            payload['data']['runtime'] = {'launcher': str(launcher), 'revision': digest,
                                         'live_update': live,
                                         'codex_migration_pending': not json.loads((runtime/'legacy-sessions.json').read_text())['codex_ready']}
            write_owned(runtime / 'activation.json', json.dumps({'revision': digest, 'previous': before}) + '\n', 0o600)
            result.stdout = json.dumps(payload) + '\n'
        print(result.stdout, end='')
        print(result.stderr, end='', file=sys.stderr)
        if result.returncode == 0:
            try:
                prune_releases(runtime, {digest, before})
            except Exception as error:
                print(f'Could not remove older Namedrop releases: {error}. The next update tries again.', file=sys.stderr)
        return result.returncode


if __name__ == '__main__':
    try:
        sys.exit(activate(Path(sys.argv[1]).resolve(), sys.argv[2:]))
    except (OSError, ValueError, KeyError, TypeError, AttributeError, RuntimeError, subprocess.SubprocessError) as error:
        print(f'Namedrop runtime activation stopped: {error}', file=sys.stderr)
        sys.exit(2)
