"""Archive-to-archive standalone upgrade against the local protocol fixture."""
import contextlib
import hashlib
import json
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tarfile
import tempfile

from runtime_activation import environment, rex_agent
from support.rex_control import Server


def unpack(archive, destination):
    with tarfile.open(archive) as source:
        source.extractall(destination, filter='data')
    roots = list(destination.iterdir())
    assert len(roots) == 1 and roots[0].is_dir(), roots
    return roots[0]


def upgrade_from(baseline):
    incoming = Path(os.environ['NAMEDROP_TEST_ARCHIVE']).resolve()
    with tempfile.TemporaryDirectory(prefix='nd-u8-upgrade-', dir='/tmp') as directory:
        base = Path(directory)
        env = environment(base)
        (base / 'tools/herdr').unlink()
        for key in list(env):
            if key.startswith('HERDR_') and key != 'HERDR_PLUGIN_STATE_DIR':
                del env[key]
        env['NAMEDROP_STATE_DIR'] = str(base / 'state')
        assert shutil.which('herdr', path=env['PATH']) is None
        old = unpack(baseline, base / 'old')
        new = unpack(incoming, base / 'new')
        contracts = [json.loads((root / 'runtime.json').read_text()) for root in (old, new)]
        assert contracts[0]['state_schema'] != contracts[1]['state_schema'], contracts
        assert contracts[0]['state_schema'] in contracts[1]['live_migrate_from'], contracts
        config = base / 'home/.config/rex/init.lua'
        config.parent.mkdir(parents=True)
        config.write_text('-- upgrade fixture\n')
        server = Server(base, config)
        try:
            endpoint = 'rex:' + str(server.socket)
            env['REX_SERVER'] = 'unix://' + str(server.socket)
            def publish(root):
                result = subprocess.run([sys.executable, str(root / 'scripts/runtime.py'), str(root),
                                         '--state-dir', str(base / 'state'), '--verify', 'install'],
                                        env=env, capture_output=True, text=True, timeout=60)
                assert result.returncode == 0, (result.stdout, result.stderr)
                return json.loads(result.stdout)['data']
            publish(old)
            store = base / 'state/namedrop.sqlite3'
            rex_agent(store, endpoint, 'surviving-block', alias='survivor', runtime='claude-code', status='idle')
            def rows():
                with contextlib.closing(sqlite3.connect(store)) as db:
                    return {table: db.execute(f'SELECT * FROM {table} ORDER BY 1,2,3').fetchall()
                            for table in ('pane_addresses', 'pane_bindings', 'alias_claims', 'agent_session_observations')}
            before = rows()
            result = publish(new)
            assert result['runtime']['live_update'] is True
            assert rows() == before
            assert 'survivor' in str(before['alias_claims'])
            with contextlib.closing(sqlite3.connect(store)) as db:
                assert db.execute('PRAGMA user_version').fetchone()[0] == contracts[1]['state_schema']
                assert db.execute("SELECT name FROM sqlite_master WHERE name='u8_upgrade_fixture'").fetchone()
            selected = base / 'state/runtime'
            digest = hashlib.sha256((new / 'bin/namedrop').read_bytes()).hexdigest()
            assert os.readlink(selected / 'current') == 'releases/' + digest
            assert (selected / 'owner').read_text() == 'standalone\n'
            assert json.loads((selected / 'source.json').read_text())['kind'] == 'archive'
            print('standalone archive upgrade: names and store rows preserved, schema '
                  f"{contracts[0]['state_schema']} -> {contracts[1]['state_schema']}, owner=standalone, no herdr on PATH")
        finally:
            server.close()
