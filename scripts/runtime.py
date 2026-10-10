"""Publish a compatible runtime without removing tools used by existing sessions."""

import ctypes
import errno
import fcntl
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
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
    require(len(target.parts) == 2 and target.parts[0] == 'releases'
            and len(target.parts[1]) == 64 and all(c in '0123456789abcdef' for c in target.parts[1]),
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
                    require(time.monotonic() < deadline,
                            'Namedrop commands are still finishing. Retry the update shortly; agent sessions can stay open.')
                    time.sleep(.05)
    except BaseException:
        stream.close()
        raise
    return stream


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
    # ps preserves the launch path, which may include the now-switched current
    # symlink. Ask macOS for the executable vnode path before classifying it.
    process = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)
    process.proc_pidpath.argtypes = [ctypes.c_int, ctypes.c_void_p, ctypes.c_uint32]
    process.proc_pidpath.restype = ctypes.c_int
    candidates = paths | {str((runtime / 'current' / 'namedrop').resolve()),
                          str((runtime / 'namedrop').resolve())}
    deadline = time.monotonic() + timeout
    while True:
        # macOS is the supported host. Wide comm reports executable paths, not
        # user prompts/arguments. No PID is killed and no process-name matching.
        try:
            result = subprocess.run(['/bin/ps', '-axww', '-o', 'pid=,comm='],
                                    capture_output=True, text=True,
                                    timeout=max(.01, min(2, deadline - time.monotonic())), check=True)
        except subprocess.TimeoutExpired as error:
            raise RuntimeError('Could not finish inspecting older Namedrop commands. '
                               'Retry the update shortly; no agent was stopped.') from error
        running = []
        for line in result.stdout.splitlines():
            parts = line.split(maxsplit=1)
            if len(parts) != 2 or not parts[1].startswith('/') or str(Path(parts[1]).resolve()) not in candidates:
                continue
            executable = ctypes.create_string_buffer(4096)  # PROC_PIDPATHINFO_MAXSIZE
            if process.proc_pidpath(int(parts[0]), executable, len(executable)) <= 0:
                if ctypes.get_errno() == errno.ESRCH:  # Finished between the two observations.
                    continue
                raise RuntimeError('Could not identify an existing Namedrop command. '
                                   'Retry the update shortly; no agent was stopped.')
            if os.fsdecode(executable.value) in paths:
                running.append(parts[0])
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
        require(len(target.parts) == 2 and target.parts[0] == 'releases'
                and len(target.parts[1]) == 64
                and all(c in '0123456789abcdef' for c in target.parts[1]),
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
    with lock(runtime, 'commands.lock', timeout=20) as gate:
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
    with lock(runtime, 'commands.lock', timeout=20) as gate:
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
            write_owned(runtime / 'activation.json', json.dumps({'revision': digest}) + '\n', 0o600)
            result.stdout = json.dumps(payload) + '\n'
        print(result.stdout, end='')
        print(result.stderr, end='', file=sys.stderr)
        return result.returncode


if __name__ == '__main__':
    try:
        sys.exit(activate(Path(sys.argv[1]).resolve(), sys.argv[2:]))
    except (OSError, ValueError, KeyError, TypeError, AttributeError, RuntimeError, subprocess.SubprocessError) as error:
        print(f'Namedrop runtime activation stopped: {error}', file=sys.stderr)
        sys.exit(2)
