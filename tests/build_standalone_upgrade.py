"""Build disposable U8 fixture archives from this working tree; never edit it."""
import json
from pathlib import Path
import shutil
import subprocess
import sys


def build(output):
    repo = Path(__file__).resolve().parents[1]
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    source = output / 'source'
    shutil.copytree(repo, source, ignore=shutil.ignore_patterns('.git', '.claude', '.agents', 'target', 'node_modules', '__pycache__'))
    original_version = (source / 'Cargo.toml').read_text().split('version = "', 1)[1].split('"', 1)[0]
    archives = []
    try:
        for revision in (1, 2):
            version = f'{original_version}-exp.{revision}'
            for name in ('Cargo.toml', 'herdr-plugin.toml', 'Cargo.lock'):
                path = source / name
                before = path.read_text()
                if name == 'Cargo.lock':
                    before = before.replace(f'name = "namedrop"\nversion = "{original_version}"', f'name = "namedrop"\nversion = "{version}"', 1)
                else:
                    before = before.replace(f'version = "{original_version}"', f'version = "{version}"', 1)
                path.write_text(before)
            if revision == 2:
                contract = source / 'runtime.json'
                value = json.loads(contract.read_text())
                previous = value['state_schema']
                value.update(state_schema=previous + 1, live_migrate_from=[*value['live_migrate_from'], previous])
                contract.write_text(json.dumps(value) + '\n')
                store = source / 'src/store.rs'
                text = store.read_text().replace(f'const SCHEMA_VERSION: u32 = {previous};', f'const SCHEMA_VERSION: u32 = {previous + 1};', 1)
                text = text.replace('    if version < SCHEMA_VERSION {',
                    '    if version < SCHEMA_VERSION {\n        transaction.execute_batch("CREATE TABLE IF NOT EXISTS u8_upgrade_fixture (id INTEGER PRIMARY KEY)")?;', 1)
                store.write_text(text)
            target = output / 'build'
            subprocess.run(['cargo', 'build', '--locked', '--release', '--manifest-path', str(source / 'Cargo.toml'),
                            '--target-dir', str(target)], check=True)
            dist = output / f'archive-{revision}'
            subprocess.run(['sh', str(source / 'scripts/package-release.sh'), '--binary', str(target / 'release/namedrop'),
                            '--output', str(dist)], check=True)
            archives.append(str(next(dist.glob('*.tar.gz'))))
            for name in ('Cargo.toml', 'herdr-plugin.toml', 'Cargo.lock'):
                path = source / name
                path.write_text(path.read_text().replace(version, original_version))
        result = dict(baseline=archives[0], incoming=archives[1])
        (output / 'archives.json').write_text(json.dumps(result, indent=2) + '\n')
        print(json.dumps(result))
    finally:
        shutil.rmtree(source)
        shutil.rmtree(output / 'build', ignore_errors=True)


if __name__ == '__main__':
    build(Path(sys.argv[1]))
