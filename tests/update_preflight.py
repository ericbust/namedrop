"""Inspect real Git refs without downloading unrelated repository contents."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(REPO / 'scripts'))
from update import prepare_target


def git(root, *args):
    return subprocess.check_output(['git', '-C', str(root), *args], text=True).strip()


with tempfile.TemporaryDirectory(prefix='namedrop-update-preflight-') as directory:
    base = Path(directory)
    source = base / 'source'
    source.mkdir()
    remote = base / 'origin.git'
    os.environ.update(GIT_CONFIG_NOSYSTEM='1', GIT_CONFIG_GLOBAL=os.devnull,
                      GIT_CONFIG_COUNT='1', GIT_ALLOW_PROTOCOL='file',
                      GIT_CONFIG_KEY_0=f'url.{remote.as_uri()}.insteadOf',
                      GIT_CONFIG_VALUE_0='https://github.com/fixture/source.git',
                      HERDR_PLUGIN_STATE_DIR=str(base / 'state'))
    git(source, 'init', '--quiet', '--initial-branch=experimental')
    git(source, 'config', 'user.name', 'Update fixture')
    git(source, 'config', 'user.email', 'fixture@example.invalid')
    package = source / 'plugins/namedrop'
    (package / 'scripts').mkdir(parents=True)
    for name in ('runtime.json', 'herdr-plugin.toml', 'scripts/run.sh'):
        shutil.copyfile(REPO / name, package / name)
    (source / 'unrelated.bin').write_bytes(os.urandom(1024 * 1024))
    git(source, 'add', '.')
    git(source, 'commit', '--quiet', '-m', 'Fixture source')
    git(source, 'tag', 'fixture-tag')
    revision = git(source, 'rev-parse', 'HEAD')
    unrelated = git(source, 'hash-object', 'unrelated.bin')
    git(source, 'clone', '--quiet', '--bare', str(source), str(remote))
    git(remote, 'config', 'uploadpack.allowFilter', 'true')
    git(remote, 'config', 'uploadpack.allowAnySHA1InWant', 'true')
    for index, ref in enumerate(('experimental', 'fixture-tag', revision)):
        inspection = base / f'inspection-{index}'
        inspection.mkdir()
        target = prepare_target({
            'plugin_root': str(package),
            'source': {'owner': 'fixture', 'repo': 'source', 'requested_ref': ref,
                       'managed_path': str(source)},
        }, inspection)
        assert target['revision'] == revision
        assert target['contract'] == json.loads((REPO / 'runtime.json').read_text())
        checkout = inspection / 'inspect'
        for name in ('runtime.json', 'herdr-plugin.toml', 'scripts/run.sh'):
            assert git(checkout, 'show', f'{revision}:plugins/namedrop/{name}') == (REPO / name).read_text().strip()
        present = git(checkout, 'cat-file', '--batch-all-objects', '--batch-check=%(objectname)').splitlines()
        assert unrelated not in present, 'version inspection downloaded an unrelated repository blob'
        assert target['environment']['NAMEDROP_UPDATE_REVISION'] == revision
        assert target['environment']['NAMEDROP_UPDATE_SOURCE'] == 'https://github.com/fixture/source.git'

print('update preflight skips unrelated blobs for branches, tags, and pinned commits')
