"""Migrate frozen, populated main and experimental schema 6 stores through the CLI."""

import argparse
import fcntl
import hashlib
import json
import os
from pathlib import Path
import sqlite3
import subprocess
import tempfile

FIXTURES = Path(__file__).resolve().parent / 'fixtures/schema6'


def snapshot(database):
    tables = database.execute(
        "SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name"
    ).fetchall()
    return {
        table: {
            'columns': [row[1] for row in database.execute(f'PRAGMA table_info("{table}")')],
            'rows': sorted(database.execute(f'SELECT * FROM "{table}"').fetchall(), key=repr),
        }
        for (table,) in tables
    }


def subagent_files(state):
    return {
        str(path.relative_to(state)): path.read_bytes()
        for path in sorted((state / 'subagents').rglob('*')) if path.is_file()
    }


def prepare(state, experimental):
    state.mkdir(parents=True)
    with sqlite3.connect(state / 'namedrop.sqlite3') as database:
        database.execute('PRAGMA foreign_keys=ON')
        for name in ['schema.sql', 'common.sql'] + (['experimental.sql'] if experimental else []):
            database.executescript((FIXTURES / name).read_text())
        assert database.execute('PRAGMA user_version').fetchone() == (6,)
        assert database.execute('PRAGMA foreign_key_check').fetchall() == []
    if experimental:
        parent = dict(socket='test-schema-socket', workspace='home', pane='worker-pane',
                      session='worker-session')
        identity = b''.join(len(part.encode()).to_bytes(8, 'little') + part.encode()
                            for part in parent.values())
        directory = state / 'subagents/v1' / hashlib.sha256(identity).hexdigest()
        directory.mkdir(parents=True)
        for child, stopped in [('running-child', False), ('finished-child', True)]:
            key = hashlib.sha256((child + '\0turn-generation').encode()).hexdigest()
            events = [('start', 214, {'Start': dict(id=child, generation='turn-generation',
                                                  kind='explorer', description='Keep this child')})]
            if stopped:
                events.append(('stop', 215, {'Stop': dict(id=child, generation='turn-generation')}))
            for suffix, at_ms, event in events:
                fact = dict(parent=parent, at_ms=at_ms, event=event)
                (directory / f'{key}.{suffix}.json').write_text(
                    json.dumps(fact, separators=(',', ':')))


def migrate(binary, state, home, schema=7):
    runtime = state / 'runtime'
    runtime.mkdir(exist_ok=True)
    selected = runtime / 'current'
    if not selected.exists():
        selected.mkdir()
        (selected / 'namedrop').symlink_to(binary)
    (runtime / 'migration.json').write_text('{"previous":"fixture","target":"fixture"}')
    env = {key: value for key, value in os.environ.items()
           if not key.startswith(('NAMEDROP_', 'HERDR_', 'CLAUDE_', 'CODEX_', 'XDG_'))
           and key != 'HOME'}
    env.update(HOME=str(home), XDG_CONFIG_HOME=str(home / '.config'),
               XDG_STATE_HOME=str(home / '.local/state'), XDG_CACHE_HOME=str(home / '.cache'))
    with (runtime / 'commands.lock').open('a') as gate:
        fcntl.flock(gate, fcntl.LOCK_EX)
        env['NAMEDROP_MIGRATION_GATE_FD'] = str(gate.fileno())
        result = subprocess.run([str(binary), '--migrate-runtime-state', str(state)],
                                env=env, pass_fds=(gate.fileno(),), capture_output=True,
                                text=True, timeout=15)
    assert result.returncode == 0, (result.stdout, result.stderr)
    assert json.loads(result.stdout) == {'state_schema': schema}, result.stdout


def verify(state, before, files):
    with sqlite3.connect(state / 'namedrop.sqlite3') as database:
        assert database.execute('PRAGMA user_version').fetchone() == (7,)
        after = snapshot(database)
        assert set(after) == set(before) | {'reminders', 'reminder_fires'}
        for table, original in before.items():
            columns = ','.join('"' + column + '"' for column in original['columns'])
            values = sorted(database.execute(f'SELECT {columns} FROM "{table}"').fetchall(), key=repr)
            assert values == original['rows'], f'Original values changed in {table}'
        assert after['reminders']['rows'] == []
        assert after['reminder_fires']['rows'] == []
        assert database.execute('PRAGMA foreign_key_check').fetchall() == []
        assert database.execute('PRAGMA integrity_check').fetchall() == [('ok',)]
        assert database.execute(
            "SELECT count(*) FROM pragma_table_info('pane_addresses') "
            "WHERE name='remote_commands_terminal_id'"
        ).fetchone() == (1,)
        assert database.execute('SELECT count(*) FROM delivery_seats').fetchone()[0] == len(
            before['mailbox_deliveries']['rows'])
    assert subagent_files(state) == files, 'Subagent lifecycle files changed'


def run(binary, root):
    evidence = {}
    for history, experimental in [('main-1.4.4', False), ('exp.13', True), ('exp.13-incomplete', True)]:
        state = root / history
        prepare(state, experimental)
        with sqlite3.connect(state / 'namedrop.sqlite3') as database:
            if history.endswith('incomplete'):
                database.execute('DROP VIEW delivery_seats')
                database.execute('ALTER TABLE pane_addresses DROP COLUMN remote_commands_terminal_id')
            before = snapshot(database)
        files = subagent_files(state)
        for _ in range(2):
            migrate(binary, state, root / 'isolated-home')
            verify(state, before, files)
        evidence[history] = {
            'baseline_schema': 6,
            'migrated_schema': 7,
            'cli_opens': 2,
            'original_tables': {
                table: {'rows': len(value['rows']), 'sha256': hashlib.sha256(
                    json.dumps(value['rows'], separators=(',', ':')).encode()).hexdigest()}
                for table, value in before.items()
            },
            'subagent_files': {path: hashlib.sha256(value).hexdigest() for path, value in files.items()},
            'original_values_unchanged': True,
            'foreign_key_check': [],
            'integrity_check': 'ok',
        }
        print(f'SCHEMA6_MIGRATION_OK {history}')
    return evidence


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', type=Path, default=os.environ.get('NAMEDROP_TEST_BINARY'))
    parser.add_argument('--evidence', type=Path)
    args = parser.parse_args()
    assert args.binary, 'Pass --binary or NAMEDROP_TEST_BINARY'
    with tempfile.TemporaryDirectory(prefix='namedrop-schema6-') as directory:
        evidence = run(args.binary.resolve(), Path(directory))
    if args.evidence:
        args.evidence.write_text(json.dumps(evidence, indent=2) + '\n')


if __name__ == '__main__':
    main()
