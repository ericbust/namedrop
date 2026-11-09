"""Real executable/SQLite migration races in disposable state; no agent sessions.

Gated predecessors (every Linux release, and macOS releases since the command
gate) run on every platform. Historical ungated predecessors exist only on
macOS, so their drain runs there; elsewhere they must be refused unchanged.
"""
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / 'scripts'))
import runtime


def wait_for(predicate, message):
    deadline = time.monotonic() + 8
    while not predicate():
        assert time.monotonic() < deadline, message
        time.sleep(.02)


# Stands in for an older release's command process. A gated release holds the
# shared command lock for the whole command, as installed_runtime::command_guard
# does; a historical ungated one never takes it.
SIMULATOR = '''const GATED: bool = {gated};
fn main() {{
    let _gate = GATED.then(|| {{
        let executable = std::fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
        let runtime = executable.parent().unwrap().parent().unwrap().parent().unwrap();
        let lock = std::fs::OpenOptions::new().create(true).append(true)
            .open(runtime.join("commands.lock")).unwrap();
        lock.lock_shared().unwrap();
        lock
    }});
    std::io::copy(&mut std::io::stdin().lock(), &mut std::io::stdout().lock()).unwrap();
}}
'''


def simulator(base, gated):
    path = base / ('gated-command' if gated else 'legacy-command')
    # Compile an ordinary native process. Copied Apple system executables
    # can be killed by code-signing policy on another macOS release.
    subprocess.run(['rustc', '--edition', '2021', '--crate-name', path.name.replace('-', '_'),
                    '-o', str(path), '-'], input=SIMULATOR.format(gated=str(gated).lower()),
                   text=True, check=True, capture_output=True)
    return path


def contracts(gated):
    declared = json.loads((REPO / 'runtime.json').read_text())
    old = {key: declared[key] for key in ('runtime_protocol', 'live_upgrade_epoch', 'collaboration_protocol')}
    old['state_schema'] = 3
    if gated:
        old['command_gate'] = 1
    return old, declared


def rows(state):
    with sqlite3.connect(state / 'namedrop.sqlite3') as db:
        tables = [r[0] for r in db.execute("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
                  if not r[0].startswith(('coordination', 'space_envoys', 'bridged_', 'reminder'))]
        return {table: db.execute('SELECT * FROM "' + table + '" ORDER BY rowid').fetchall()
                for table in tables}


class Fixture:
    def __init__(self, base, actual, simulator, gated):
        self.state = base / 'state'
        self.rt = self.state / 'runtime'
        self.rt.mkdir(parents=True)
        old, declared = contracts(gated)
        self.old = self.release(simulator, old)
        self.new = self.release(actual, declared)
        (self.rt / 'current').symlink_to(self.old)
        (self.rt / 'namedrop').write_text(runtime.LAUNCHER)
        (self.rt / 'namedrop').chmod(0o700)
        # Actual legacy table definitions; the production Rust migration adds
        # coordination tables and must preserve every existing row and pause.
        with sqlite3.connect(self.state / 'namedrop.sqlite3') as db:
            for name in ('src/store.rs', 'src/store/groups.rs', 'src/store/terminal.rs'):
                source = (REPO / name).read_text()
                schema = source.split('const SCHEMA: &str = r#"', 1)[1].split('"#;', 1)[0]
                db.executescript(schema)
            db.execute("INSERT INTO worktrees VALUES ('socket', 'w', 1, 1, 0, 0)")
            legacy = ['describe', 'register', 'ack', 'send', 'reply', 'complete.no-reply',
                      'delivery.inbox', 'snapshot.publish', 'context.inspect', 'context.read',
                      'handoff.status', 'handoff.results', 'handoff.preview', 'group.list']
            for address in ('lead', 'worker'):
                db.execute("INSERT INTO pane_roles VALUES ('socket','w',?,?, '')", (address, address))
                db.execute("INSERT INTO pane_addresses (session_socket_id,worktree_id,address_id,role_id,retired,collaboration_disabled,created_at_ms) VALUES ('socket','w',?,?,0,0,0)", (address, address))
                db.execute("INSERT INTO pane_bindings VALUES ('socket','w',?,?,?,1,'tab',0,NULL)",
                           (address+'-binding', address, address+'-pane'))
                db.execute("INSERT INTO integration_registrations VALUES ('socket','w',?,?,?,'codex','collab/v1',?,1)",
                           (address+'-registration', address, address+'-session', json.dumps(legacy, separators=(',', ':'))))
            db.execute("INSERT INTO collaboration_threads VALUES ('socket','w','thread','Pending work',0)")
            for order, status in enumerate(('held', 'queued'), 1):
                db.execute("""INSERT INTO collaboration_messages VALUES
                    ('socket','w',?,'thread',?,NULL,'lead','lead','agent_session','lead-session',?,
                     'source','unfinished work','delegated/v1',0)""", (status+'-message', order, status+'-request'))
                db.execute("INSERT INTO message_recipients VALUES ('socket','w',?,1,'worker','worker','worker-session')",
                           (status+'-message',))
                db.execute("INSERT INTO mailbox_deliveries VALUES ('socket','w',?,'worker',?,?,?,0)",
                           (status+'-delivery', order, status+'-message', status))
            db.execute("INSERT INTO delivery_attempts VALUES ('socket','w','held-attempt','held-delivery','worker','worker-binding','worker-session',NULL,0)")
            db.execute("INSERT INTO delivery_attempt_state VALUES ('socket','w','held-attempt','worker','held',0)")
            assert not db.execute('PRAGMA foreign_key_check').fetchall()
            db.execute('PRAGMA user_version=3')
        self.before = rows(self.state)
        self.children = []

    def release(self, source, contract):
        digest = hashlib.sha256(source.read_bytes()).hexdigest()
        target = Path('releases') / digest
        root = self.rt / target
        root.mkdir(parents=True)
        shutil.copyfile(source, root / 'namedrop')
        (root / 'namedrop').chmod(0o700)
        (root / 'runtime.json').write_text(json.dumps(contract))
        return target

    def spawn(self, args, **kwargs):
        env = {k: v for k, v in os.environ.items() if not k.startswith(('HERDR_', 'NAMEDROP_'))}
        child = subprocess.Popen(args, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                 env=env, **kwargs)
        self.children.append(child)
        return child

    def migration(self, timeout=20):
        # The timeout bounds both the gate wait and the legacy drain.
        code = ('import sys; from pathlib import Path; sys.path.insert(0,sys.argv[1]); import runtime; '
                'limit=float(sys.argv[5]); lock=runtime.lock; '
                'runtime.lock=lambda r,n,timeout=None: lock(r,n,None if timeout is None else min(timeout,limit)); '
                'runtime.drain_legacy_commands.__defaults__=(limit,); '
                'runtime.command_gate.__wrapped__.__defaults__=(limit, min(2, limit)); '
                'runtime.switch_and_migrate(Path(sys.argv[2]),Path(sys.argv[3]),Path(sys.argv[4])); '
                'print("migration completed")')
        return self.spawn([sys.executable, '-c', code, str(REPO / 'scripts'), str(self.rt),
                           str(self.old), str(self.new), str(timeout)])

    def command(self):
        return self.spawn([str(self.rt / 'namedrop'), '--plugin-state', str(self.state), 'describe', 'send'])

    def mark_interrupted(self):
        (self.rt / 'migration.json').write_text(json.dumps({'previous': str(self.old), 'target': str(self.new)}))
        runtime.switch(self.rt, self.new)

    def verify_rows(self, schema):
        assert runtime.state_schema(self.rt) == schema
        # Migration is additive: the same tables and rows survive with their
        # existing column values; only new trailing columns may appear.
        current = rows(self.state)
        assert set(current) == set(self.before), 'Legacy tables changed'
        for table, before in self.before.items():
            after = current[table]
            assert len(after) == len(before), f'Legacy rows changed in {table}'
            assert all(new[:len(old)] == old for old, new in zip(before, after)), \
                f'Legacy state or pause changed in {table}'

    def close(self):
        for child in self.children:
            if child.poll() is None:
                child.terminate()
            try:
                child.communicate(timeout=3)
            except subprocess.TimeoutExpired:
                child.kill()
                child.communicate(timeout=3)
        assert all(child.poll() is not None for child in self.children)


def start_old_commands(fixture):
    # Exercise the real launcher and a cached current path as well as the
    # immutable snapshot. ps comm retains the switched symlink.
    older = []
    for path in (fixture.rt / 'namedrop', fixture.rt / 'current' / 'namedrop',
                 fixture.rt / fixture.old / 'namedrop'):
        old = fixture.spawn([str(path)], stdin=subprocess.PIPE)
        old.stdin.write(b'old command alive\n')
        old.stdin.flush()
        assert old.stdout.readline() == b'old command alive\n'
        older.append(old)
    return older


def finish(old):
    old.stdin.close()
    old.stdin = None
    assert old.wait(timeout=2) == 0


def verify_completed(fixture, migration, pending):
    out, err = migration.communicate(timeout=8)
    assert migration.returncode == 0, (out, err)
    out, err = pending.communicate(timeout=8)
    assert pending.returncode == 0 and json.loads(out)['status'] == 'ok', (out, err)
    fixture.verify_rows(8)
    assert runtime.current(fixture.rt) == fixture.new
    assert not (fixture.rt / 'migration.json').exists()
    assert (fixture.rt / fixture.old / 'namedrop').is_file()


def legacy_success(fixture):
    """Historical macOS release: old commands drain after the pointer moves."""
    older = start_old_commands(fixture)
    migration = fixture.migration()
    wait_for(lambda: runtime.current(fixture.rt) == fixture.new, 'New pointer was not staged')
    pending = fixture.command()
    time.sleep(.2)
    assert all(old.poll() is None for old in older) and migration.poll() is None and pending.poll() is None
    fixture.verify_rows(3)
    # Finish the direct snapshot first; both symlink-launched commands
    # must independently keep the migration waiting.
    for old in reversed(older):
        assert migration.poll() is None, 'An old command was missed after pointer replacement'
        finish(old)
        if any(other.poll() is None for other in older):
            time.sleep(.15)
            assert migration.poll() is None and pending.poll() is None
            fixture.verify_rows(3)
    verify_completed(fixture, migration, pending)


def gated_success(fixture):
    """Gated release: nothing is published until every old command releases the gate."""
    older = start_old_commands(fixture)
    migration = fixture.migration()
    # Pause the migration child after it holds the gate, so a new command can
    # be shown waiting behind it rather than racing it.
    database = sqlite3.connect(fixture.state / 'namedrop.sqlite3')
    try:
        for old in reversed(older):
            time.sleep(.15)
            assert migration.poll() is None, 'Migration ran while an old command held the gate'
            assert runtime.current(fixture.rt) == fixture.old
            assert not (fixture.rt / 'migration.json').exists()
            fixture.verify_rows(3)
            if old is older[0]:
                database.execute('BEGIN IMMEDIATE')
            finish(old)
        wait_for(lambda: runtime.current(fixture.rt) == fixture.new, 'New pointer was not published')
        pending = fixture.command()
        time.sleep(.2)
        assert migration.poll() is None and pending.poll() is None, 'A command bypassed the migration gate'
        fixture.verify_rows(3)
    finally:
        database.rollback()
        database.close()
    verify_completed(fixture, migration, pending)


def timeout(fixture):
    old = fixture.spawn([str(fixture.rt / 'namedrop')], stdin=subprocess.PIPE)
    old.stdin.write(b'busy\n'); old.stdin.flush()
    assert old.stdout.readline() == b'busy\n'
    migration = fixture.migration(timeout=.3)
    out, err = migration.communicate(timeout=8)
    assert migration.returncode != 0, (out, err)
    assert b'PID %d (namedrop) holds the command lock' % old.pid in err, err
    assert b'kill %d' % old.pid in err and b'retry the update' in err and b'popup' not in err, err
    assert old.poll() is None, 'Updater stopped a running command'
    fixture.verify_rows(3)
    assert runtime.current(fixture.rt) == fixture.old
    assert not (fixture.rt / 'migration.json').exists()


FAKE_HERDR = """#!/bin/sh
touch "$(dirname "$0")/herdr-called"
printf '%s\\n' '{"error":{"code":"connection_refused","message":"host restarting"}}' >&2
exit 1
"""
HELPERS = (['hook', 'agent-status-worker'], ['--deferred-worker', '{state}', 'socket'],
           ['reminder-clock', '{state}', 'socket'])


def start_helper(fixture, executable, args, environment):
    helper = subprocess.Popen([str(executable), *[a.format(state=fixture.state) for a in args]],
                              stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                              env=environment)
    fixture.children.append(helper)
    helper.stdin.write(b'holding the gate\n')
    helper.stdin.flush()
    assert helper.stdout.readline() == b'holding the gate\n'
    return helper


def helper_environment(fixture):
    herdr = fixture.state.parent / 'herdr'
    herdr.write_text(FAKE_HERDR)
    herdr.chmod(0o700)
    environment = {k: v for k, v in os.environ.items() if not k.startswith(('HERDR_', 'NAMEDROP_'))}
    environment.update(HERDR_PLUGIN_STATE_DIR=str(fixture.state), HERDR_SOCKET_PATH='socket',
                       HERDR_WORKSPACE_ID='w', HERDR_BIN_PATH=str(herdr))
    return environment


def stop_runtime_processes(fixture):
    executables = runtime.retained_executables(fixture.rt)
    for pid in runtime.all_pids():
        found = runtime.process(pid)
        if found and os.path.realpath(found[0]) in executables and pid not in {c.pid for c in fixture.children}:
            os.kill(pid, signal.SIGKILL)


def stuck_helpers(fixture):
    """As on a Mac updating from 1.6.5: the release's status worker waits out
    retries while holding the command gate, beside its deferred worker and
    reminder clock. The update stops these helpers, migrates and starts them
    again on the new runtime, with no step for the person and no queued work lost."""
    environment = helper_environment(fixture)
    queue = fixture.state / 'agent-status-reconciliation' / hashlib.sha256(b'socket\0w').hexdigest()
    queue.mkdir(parents=True)
    marker = queue / 'pending-1'
    marker.write_text(json.dumps({'type': 'pane_agent_status_changed', 'pane_id': 'worker-pane',
                                  'workspace_id': 'w', 'agent_status': 'idle'}))
    helpers = [start_helper(fixture, fixture.rt / 'namedrop', args, environment) for args in HELPERS]
    try:
        migration = fixture.migration(timeout=10)
        out, err = migration.communicate(timeout=40)
        assert migration.returncode == 0 and b'migration completed' in out, (out, err)
        assert b'Restarted Namedrop helpers after the update: status worker' in err, err
        assert b'deferred worker' in err and b'reminder clock' in err, err
        for helper in helpers:
            assert helper.wait(timeout=5) == -signal.SIGTERM, helper.returncode
        fixture.verify_rows(8)
        assert runtime.current(fixture.rt) == fixture.new
        # The restarted status worker runs the new runtime against the same queue.
        wait_for(lambda: (fixture.state.parent / 'herdr-called').exists(),
                 'The restarted status worker never read its queue')
        assert marker.exists(), 'A queued status event was lost'
        with runtime.command_gate(fixture.rt, timeout=5, grace=5):
            pass
    finally:
        stop_runtime_processes(fixture)


def foreign_holder(fixture):
    """A process with a helper's arguments that is not one of this runtime's
    executables is never stopped: the update waits, then names it."""
    # The simulator locks the commands.lock three directories above itself, so
    # this copy holds the real gate without being a retained release.
    stranger = fixture.rt / 'foreign' / 'copy' / 'namedrop'
    stranger.parent.mkdir(parents=True)
    shutil.copyfile(fixture.rt / fixture.old / 'namedrop', stranger)
    stranger.chmod(0o700)
    holder = start_helper(fixture, stranger, HELPERS[0], helper_environment(fixture))
    migration = fixture.migration(timeout=3)
    out, err = migration.communicate(timeout=20)
    assert migration.returncode != 0 and b'PID %d (namedrop hook agent-status-worker)' % holder.pid in err, (out, err)
    assert holder.poll() is None, 'The update stopped a process that is not a Namedrop helper'
    assert b'Restarted' not in err
    fixture.verify_rows(3)
    assert runtime.current(fixture.rt) == fixture.old


def interrupted_publication(fixture):
    (fixture.rt / 'migration.json').write_text(json.dumps({'previous': str(fixture.old), 'target': str(fixture.new)}))
    # Interrupt the actual two-step pointer publication before replace.
    (fixture.rt / 'next').symlink_to(fixture.new)
    runtime.recover_migration(fixture.rt)
    assert runtime.current(fixture.rt) == fixture.old
    assert not (fixture.rt / 'next').is_symlink()
    assert not (fixture.rt / 'migration.json').exists()
    fixture.verify_rows(3)
    migration = fixture.migration()
    out, err = migration.communicate(timeout=8)
    assert migration.returncode == 0, (out, err)
    fixture.verify_rows(8)
    # Foreign publication entries are never removed by recovery.
    (fixture.rt / 'next').symlink_to('/foreign/runtime')
    try:
        runtime.switch(fixture.rt, fixture.new)
        raise AssertionError('Foreign publication entry accepted')
    except RuntimeError as error:
        assert 'foreign runtime publication' in str(error)
    assert os.readlink(fixture.rt / 'next') == '/foreign/runtime'


def orphaned_migration(fixture):
    # If the updater dies during the SQLite transaction, its migration child
    # must retain the exclusive lock until it exits. Waiting commands then
    # stop at the durable marker, ready for normal activation recovery.
    connection = sqlite3.connect(fixture.state / 'namedrop.sqlite3')
    connection.execute('BEGIN IMMEDIATE')
    try:
        migration = fixture.migration()
        child_pid = None
        # Launch arguments, not comm: Linux truncates comm to the file name.
        expected = f'{fixture.rt / fixture.new / "namedrop"} --migrate-runtime-state {fixture.state}'

        def migration_child():
            nonlocal child_pid
            output = subprocess.check_output(['/bin/ps', '-A', '-ww', '-o', 'pid=,ppid=,args='], text=True)
            for line in output.splitlines():
                parts = line.split(maxsplit=2)
                if len(parts) == 3 and parts[1] == str(migration.pid) and parts[2] == expected:
                    child_pid = int(parts[0])
                    return True
            return False

        wait_for(migration_child, 'Migration child was not observed')
        migration.kill()
        migration.wait(timeout=3)
        pending = fixture.command()
        time.sleep(.15)
        assert pending.poll() is None, 'Parent death released the migration lock before its child finished'
        connection.rollback()
        out, err = pending.communicate(timeout=6)
        assert pending.returncode != 0 and b'activation was interrupted' in err, (out, err)
        fixture.verify_rows(8)
        runtime.recover_migration(fixture.rt)
        assert not (fixture.rt / 'migration.json').exists()
        assert runtime.current(fixture.rt) == fixture.new
        assert child_pid is not None
    finally:
        connection.close()


def interrupted(fixture, committed):
    fixture.mark_interrupted()
    direct = fixture.spawn([str(fixture.rt / fixture.new / 'namedrop'),
                            '--migrate-runtime-state', str(fixture.state)])
    out, err = direct.communicate(timeout=5)
    assert direct.returncode != 0 and b'inherited command gate' in err, (out, err)
    fixture.verify_rows(3)
    if committed:
        with runtime.lock(fixture.rt, 'commands.lock') as gate:
            runtime.migrate_selected(fixture.rt, fixture.new, gate)
    pending = fixture.command()
    out, err = pending.communicate(timeout=5)
    assert pending.returncode != 0 and b'activation was interrupted' in err, (out, err)
    fixture.verify_rows(8 if committed else 3)
    runtime.recover_migration(fixture.rt)
    assert not (fixture.rt / 'migration.json').exists()
    assert runtime.current(fixture.rt) == (fixture.new if committed else fixture.old)
    fixture.verify_rows(8 if committed else 3)
    if committed:
        pending = fixture.command()
        out, err = pending.communicate(timeout=5)
        assert pending.returncode == 0 and json.loads(out)['status'] == 'ok', (out, err)


def ungated_refused(fixture):
    """No Linux release predates the gate, so an ungated one is never drained."""
    old = fixture.spawn([str(fixture.rt / 'namedrop')], stdin=subprocess.PIPE)
    old.stdin.write(b'busy\n'); old.stdin.flush()
    assert old.stdout.readline() == b'busy\n'
    migration = fixture.migration()
    out, err = migration.communicate(timeout=8)
    assert migration.returncode != 0 and b'command gate' in err, (out, err)
    assert old.poll() is None, 'Updater stopped a running command'
    fixture.verify_rows(3)
    assert runtime.current(fixture.rt) == fixture.old
    assert not (fixture.rt / 'migration.json').exists()


def run(base, name, actual, simulator, gated, case, *args):
    fixture = Fixture(base / name, actual, simulator, gated)
    try:
        case(fixture, *args)
    finally:
        fixture.close()


def helper_identity():
    """Activation stops only these exact helper commands of its own state."""
    raw = (2).to_bytes(4, sys.byteorder) + b'/r/releases/abc/namedrop\0\0\0\0' \
        + b'/r/current/namedrop\0hook\0HERDR_SOCKET_PATH=/s.sock\0A=b=c\0\0ptr_munge=\0'
    argv, environment = runtime.parse_procargs2(raw)
    assert argv == ['/r/current/namedrop', 'hook'], argv
    assert environment == {'HERDR_SOCKET_PATH': '/s.sock', 'A': 'b=c'}, environment
    kind = runtime.helper_kind
    state = Path('/var/state')
    assert kind(['n', 'hook', 'agent-status-worker'], state) == 'status worker'
    assert kind(['n', '--deferred-worker', '/var/state', '/s.sock'], state) == 'deferred worker'
    assert kind(['n', 'reminder-clock', '/var/state', '/s.sock'], state) == 'reminder clock'
    for other in (['n', 'reminder-clock', '/other/state', '/s.sock'], ['n', 'hook', 'agent-status-changed'],
                  ['n', 'hook', 'agent-status-worker', 'extra'], ['n', '--deferred-execute', '/var/state', 's', 'k'],
                  ['n', 'ui', 'run', 'tour'], ['n', 'reply'], ['claude'], ['n']):
        assert kind(other, state) is None, other


def busy_gate_advice():
    """A blocked update names each holder by its subcommand words only: a flag's
    value can hold a message, so it never reaches the person's screen."""
    words = runtime.command_words
    assert words(['/r/releases/abc/namedrop', 'hook', 'agent-status-worker']) == 'namedrop hook agent-status-worker'
    assert words(['/r/namedrop', '--plugin-state', '/s', 'reply', '--herdr-socket', '/s.sock',
                  '--input-json', '{"source":"@lead secret words"}']) == 'namedrop reply'
    assert words(['/r/namedrop', '--input-json={"a":1}', 'send', 'Secret Text']) == 'namedrop send'
    assert words(['/r/namedrop', 'ui', 'run', 'tour']) == 'namedrop ui run tour'
    holders = {'worker': [(41, 'namedrop hook agent-status-worker')],
               'popup': [(42, 'namedrop ui run tour')],
               'both': [(41, 'namedrop hook agent-status-worker'), (42, 'namedrop ui run tour')],
               'other': [(43, 'namedrop reply')], 'none': []}
    found = runtime.gate_holders
    try:
        messages = {}
        for name, listed in holders.items():
            runtime.gate_holders = lambda path, listed=listed: listed
            messages[name] = runtime.busy_gate_message(Path('/unused'))
    finally:
        runtime.gate_holders = found
    assert 'kill 41' in messages['worker'] and 'status worker' in messages['worker'] and 'popup' not in messages['worker']
    assert 'popup' in messages['popup'] and 'kill' not in messages['popup']
    assert 'popup' in messages['both'] and 'kill 41' in messages['both'] and 'kill 42' not in messages['both']
    assert 'kill 43' in messages['other'] and 'PID 43 (namedrop reply)' in messages['other']
    assert 'popup' in messages['none']
    assert all('retry the update' in message for message in messages.values())


def main():
    busy_gate_advice()
    helper_identity()
    actual = Path(os.environ['NAMEDROP_TEST_BINARY']).resolve()
    with tempfile.TemporaryDirectory(prefix='namedrop-live-schema-') as directory:
        base = Path(directory)
        simulators = {gated: simulator(base, gated) for gated in (True, False)}
        released_1_6_0 = {'runtime_protocol': 1, 'live_upgrade_epoch': 1, 'state_schema': 7,
                          'collaboration_protocol': 'collab/v1', 'command_gate': 1,
                          'live_migrate_from': [3, 4, 5, 6]}
        assert runtime.live_compatible(released_1_6_0, contracts(True)[1])
        for old, new in (contracts(True), contracts(False)):
            assert runtime.live_compatible(old, new)
            assert runtime.live_compatible(dict(new), new)
            for prior in (dict(old, state_schema=2), dict(old, state_schema=9),
                          dict(old, live_upgrade_epoch=2), dict(old, collaboration_protocol='unknown')):
                assert not runtime.live_compatible(prior, new)
            assert not runtime.live_compatible(old, dict(new, command_gate=0))
            assert not runtime.live_compatible(old, dict(new, live_migrate_from=[]))

        histories = {'gated': True}
        if sys.platform == 'darwin':
            histories['legacy_macos'] = False
        for history, gated in histories.items():
            def case(name, check, *args):
                run(base, f'{history}-{name}', actual, simulators[gated], gated, check, *args)
            case('success', gated_success if gated else legacy_success)
            case('timeout', timeout)
            if gated:
                case('stuck-helpers', stuck_helpers)
                case('foreign-holder', foreign_holder)
            case('interrupted-publication', interrupted_publication)
            case('orphaned-migration', orphaned_migration)
            for committed in (False, True):
                case(f'interrupted-{committed}', interrupted, committed)
            print(f'LIVE_SCHEMA_{history.upper()}_OK')
        if sys.platform != 'darwin':
            run(base, 'ungated-refused', actual, simulators[False], False, ungated_refused)
            print('LIVE_SCHEMA_UNGATED_REFUSED_OK')
    print('LIVE_SCHEMA_PROCESS_AND_RECOVERY_OK')


if __name__ == '__main__':
    main()
