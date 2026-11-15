"""U9's archive and installer contract, using only scratch homes and stub hosts."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest

REPO = Path(__file__).resolve().parents[1]
VERSION = (REPO / 'Cargo.toml').read_text().split('version = "', 1)[1].split('"', 1)[0]
CONTRACT = json.loads((REPO / 'runtime.json').read_text())

STUB = '''#!{python}
import json, os, sqlite3, sys
from pathlib import Path
args = sys.argv[1:]
with open(os.environ['CALLS'], 'a') as log:
    log.write(json.dumps([sys.argv[0], *args]) + '\\n')
if args == ['--version']:
    print('namedrop {version}')
elif args == ['--runtime-info']:
    print({contract!r})
elif args[:1] == ['--migrate-runtime-state']:
    sys.path.insert(0, {tests!r})
    from runtime_activation import schema
    schema(Path(args[1]) / 'namedrop.sqlite3')
    with sqlite3.connect(str(Path(args[1]) / 'namedrop.sqlite3')) as db:
        db.execute('PRAGMA user_version={schema}')
    print(json.dumps(dict(state_schema={schema})))
elif 'install' in args:
    assert args[args.index('--input-json') + 1] == '{{"confirmed":true}}'
    state = Path(args[args.index('--plugin-state') + 1])
    home = Path(os.environ['HOME'])
    launcher = str(state / 'runtime/namedrop')
    config = home / '.config/rex/init.lua'
    config.parent.mkdir(parents=True, exist_ok=True)
    module = config.with_name('namedrop.lua')
    module.write_text(launcher)
    config.write_text('pcall(require, "namedrop");\\n')
    record = dict(state=str(state), launcher=launcher, module=str(module), config=str(config),
                  line='inserted', server='rex:/tmp/u9-stub-only.sock')
    directory = home / '.config/namedrop'
    directory.mkdir(parents=True, exist_ok=True)
    (directory / 'rex.json').write_text(json.dumps(record))
    (directory / 'state-directory').write_text(str(state) + '\\n')
    data = dict(state_directory=str(state), module=str(module), config=str(config), line='inserted',
                keys=[dict(key='alt+shift+cmd+i', action='namedrop_name', in_effect=True)], confirmed=True,
                integration_packages=dict(codex=dict(installed=True)),
                agent_integration_hook_activation=dict(codex=dict(cache_current=True, migration_ready=True,
                    steps=['Approve the Namedrop hooks in Codex.'])))
    print(json.dumps(dict(status='ok', operation='install', data=data)))
else:
    raise SystemExit('unexpected Namedrop invocation: ' + repr(args))
'''


class Installer(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='nd-u9-', dir='/tmp')
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.home = self.root / 'scratch home'
        self.home.mkdir()
        self.tools = self.root / 'tools'
        self.tools.mkdir()
        for name in ('dirname', 'awk', 'mkdir', 'cp', 'chmod', 'mktemp', 'tar', 'gzip', 'rm', 'ln', 'shasum', 'sha256sum'):
            tool = shutil.which(name)
            if tool:
                (self.tools / name).symlink_to(tool)
        self.script('uname', '#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n')
        self.script('file', '#!/bin/sh\necho "Mach-O 64-bit executable arm64"\n')
        self.script('rex', '#!/bin/sh\nexit 97\n')
        self.script('python3', f'#!/bin/sh\nexec "{sys.executable}" "$@"\n')
        self.env = {k: v for k, v in os.environ.items() if k not in {
            'REX_BLOCK', 'REX_SESSION', 'REX_SERVER', 'REX_CLIENT', 'REX_BIN_DIR',
            'XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'NAMEDROP_STATE_DIR', 'CODEX_HOME',
            'CLAUDE_CONFIG_DIR', 'HERDR_PLUGIN_STATE_DIR', 'PYTHONPATH', 'PYTHONHOME'}}
        self.env.update(HOME=str(self.home), PATH=str(self.tools), CALLS=str(self.root / 'calls'),
                        XDG_RUNTIME_DIR=str(self.root / 'run'), PYTHONDONTWRITEBYTECODE='1')
        self.binary = self.root / 'namedrop'
        self.binary.write_text(STUB.format(python=sys.executable, version=VERSION,
                                          contract=json.dumps(CONTRACT), schema=CONTRACT['state_schema'],
                                          tests=str(REPO / 'tests')))
        self.binary.chmod(0o755)

    def script(self, name, text):
        path = self.tools / name
        path.write_text(text)
        path.chmod(0o755)
        return path

    def run_install(self, *args, package=REPO, answer=''):
        result = subprocess.run(['/bin/sh', str(package / 'scripts/install.sh'), *args],
                                input=answer, env=self.env, capture_output=True, text=True)
        return result

    def ok(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result.stdout

    def snapshot(self, root=None):
        root = root or self.home
        return {str(p.relative_to(root)): ('link', os.readlink(p)) if p.is_symlink()
                else ('file', p.read_bytes(), p.stat().st_mode) if p.is_file() else ('dir',)
                for p in root.rglob('*')}

    def record(self, owner='standalone', capable=True, legacy=False):
        state = self.root / 'recorded state'
        runtime = state / 'runtime'
        runtime.mkdir(parents=True)
        launcher = runtime / 'namedrop'
        if capable:
            shutil.copy2(self.binary, launcher)
        else:
            launcher.write_text('#!/bin/sh\necho \'{"runtime_protocol":1}\'\n')
            launcher.chmod(0o755)
        if legacy:
            (runtime / 'current').mkdir()
        else:
            (runtime / 'owner').write_text(owner + '\n')
        directory = self.home / '.config/namedrop'
        directory.mkdir(parents=True)
        (directory / 'state-directory').write_text(str(state) + '\n')
        return state

    def archive(self):
        result = subprocess.run(['/bin/sh', str(REPO / 'scripts/package-release.sh'),
                                 '--target', 'aarch64-apple-darwin', '--binary', str(self.binary),
                                 '--output', str(self.root / 'dist')], env=self.env, capture_output=True, text=True)
        self.ok(result)
        archive = Path(result.stdout.splitlines()[0])
        checksum = Path(str(archive) + '.sha256').read_text().split()[0]
        self.assertEqual(hashlib.sha256(archive.read_bytes()).hexdigest(), checksum)
        with tarfile.open(archive) as packed:
            packed.extractall(self.root / 'release with spaces', filter='data')
        package = self.root / 'release with spaces' / archive.name.removesuffix('.tar.gz')
        self.assertTrue(os.access(package / 'scripts/install.sh', os.X_OK))
        listed = set()
        for line in (package / 'SHA256SUMS').read_text().splitlines():
            digest, name = line.split('  ', 1)
            listed.add(name)
            self.assertEqual(hashlib.sha256((package / name).read_bytes()).hexdigest(), digest)
        self.assertEqual(listed, {str(p.relative_to(package)) for p in package.rglob('*')
                                  if p.is_file() and p.name != 'SHA256SUMS'})
        return package

    def test_python_stub_failure(self):
        self.script('python3', '#!/bin/sh\nexit 1\n')
        before = self.snapshot(self.root)
        result = self.run_install('--yes')
        self.assertEqual(result.returncode, 69)
        self.assertIn('xcode-select --install', result.stderr)
        self.assertIn('Namedrop needs Python 3.', result.stderr)
        self.assertEqual(self.snapshot(self.root), before)

    def test_missing_rex(self):
        (self.tools / 'rex').unlink()
        result = self.run_install('--yes')
        self.assertEqual(result.returncode, 69)
        self.assertIn('PATH', result.stderr)
        self.assertIn('REX_BIN_DIR', result.stderr)
        self.assertEqual(self.snapshot(), {})

    def test_platform_and_argument_refusal(self):
        self.assertEqual(self.run_install('--unknown').returncode, 64)
        self.assertIn('Usage:', self.ok(self.run_install('--help')))
        self.script('uname', '#!/bin/sh\necho unsupported\n')
        result = self.run_install('--yes')
        self.assertEqual(result.returncode, 69)
        self.assertIn('supports macOS', result.stderr)
        self.assertEqual(self.snapshot(), {})

    def test_confirmation_is_once_and_read_only(self):
        state = self.record('herdr')
        before = self.snapshot(self.root)
        output = self.ok(self.run_install(answer='n\n'))
        self.assertIn('install: adopt', output)
        self.assertEqual(output.count('Install Namedrop for Rex?'), 1)
        after = self.snapshot(self.root)
        after.pop('calls', None)
        self.assertEqual(after, before)
        calls = [json.loads(line)[1:] for line in (self.root / 'calls').read_text().splitlines()]
        self.assertEqual(calls, [['--runtime-info']])
        (self.root / 'calls').unlink()
        self.env['NAMEDROP_STATE_DIR'] = str(self.root / 'must not create')
        output = self.ok(self.run_install(answer='yes\n'))
        self.assertIn('init.lua line: inserted', output)
        self.assertFalse(Path(self.env['NAMEDROP_STATE_DIR']).exists())
        self.assertEqual((self.home / '.config/rex/namedrop.lua').read_text(), str(state / 'runtime/namedrop'))

    def test_old_herdr_refused_including_implicit_owner(self):
        state = self.record('herdr', capable=False, legacy=True)
        before = self.snapshot(self.root)
        result = self.run_install('--yes')
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('Update Namedrop in Herdr first', result.stderr)
        self.assertEqual(self.snapshot(self.root), before)
        self.assertTrue((state / 'runtime/current').is_dir())

    def test_herdr_adoption_publishes_nothing(self):
        state = self.record('herdr')
        package = self.archive()
        before = self.snapshot(state)
        output = self.ok(self.run_install('--yes', package=package))
        self.assertIn('install: adopt', output)
        self.assertIn('Keys in effect:', output)
        self.assertIn('Approve the Namedrop hooks in Codex.', output)
        self.assertEqual(self.snapshot(state), before)
        self.assertFalse((self.home / '.local/state/namedrop').exists())
        self.assertEqual((self.home / '.config/rex/namedrop.lua').read_text(), str(state / 'runtime/namedrop'))
        calls = [json.loads(line) for line in (self.root / 'calls').read_text().splitlines()]
        installs = [call for call in calls if 'install' in call]
        self.assertEqual(installs, [[str(state / 'runtime/namedrop'), '--plugin-state', str(state),
                                    '--json', 'install', '--input-json', '{"confirmed":true}']])

    def test_retirement_precedes_owner_and_default_target_is_guarded(self):
        state = self.record('invalid')
        (state / 'runtime/retired.json').write_text(json.dumps(dict(reason='herdr_handoff')))
        before = self.snapshot(self.root)
        result = self.run_install('--yes')
        self.assertIn('Namedrop is moving to Herdr. Run Activate Namedrop in Herdr again.', result.stderr)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.snapshot(self.root), before)
        (self.home / '.config/namedrop/state-directory').unlink()
        (state / 'runtime/retired.json').unlink()
        (state / 'runtime/owner').write_text('herdr\n')
        self.env['NAMEDROP_STATE_DIR'] = str(state)
        result = self.run_install('--yes')
        self.assertIn('ND-INSTALL-002', result.stderr)
        (state / 'runtime/retired.json').write_text(json.dumps(dict(reason='herdr_handoff')))
        self.assertIn('Namedrop is moving to Herdr.', self.run_install('--yes').stderr)

    def test_archive_new_install_stale_record_and_rex_bin_dir(self):
        package = self.archive()
        self.assertIsNone(shutil.which('herdr', path=self.env['PATH']))
        state = self.record()
        (state / 'runtime/namedrop').unlink()
        self.env['XDG_STATE_HOME'] = str(self.root / 'xdg state')
        rex_bin = self.root / 'Rex helpers'
        rex_bin.mkdir()
        (self.tools / 'rex').rename(rex_bin / 'rex')
        self.env['REX_BIN_DIR'] = str(rex_bin)
        output = self.ok(self.run_install('--yes', package=package))
        self.assertIn('install: new', output)
        installed = Path(self.env['XDG_STATE_HOME']) / 'namedrop'
        self.assertTrue((installed / 'runtime/current').is_symlink())
        self.assertEqual((installed / 'runtime/owner').read_text(), 'standalone\n')
        source = json.loads((installed / 'runtime/source.json').read_text())
        self.assertEqual(source, dict(version=VERSION, kind='archive', origin=str(package.resolve())))
        self.assertEqual((self.home / '.config/rex/namedrop.lua').read_text(), str(installed / 'runtime/namedrop'))
        self.assertEqual((self.home / '.config/namedrop/state-directory').read_text(), str(installed) + '\n')
        self.assertFalse((state / 'runtime/current').exists())
        self.assertNotIn('Install Namedrop for Rex?', output)
        self.env['NAMEDROP_STATE_DIR'] = str(self.root / 'ignored default')
        self.assertIn('install: update', self.ok(self.run_install('--yes', package=package)))
        self.assertFalse(Path(self.env['NAMEDROP_STATE_DIR']).exists())
        (installed / 'runtime/retired.json').write_text(json.dumps(dict(reason='removed')))
        self.ok(self.run_install('--yes', package=package))
        self.assertFalse((installed / 'runtime/retired.json').exists())

    def test_new_confirmation_default_home_and_publisher_failure(self):
        before = self.snapshot(self.root)
        self.ok(self.run_install(answer=''))
        self.assertEqual(self.snapshot(self.root), before)
        package = self.archive()
        output = self.ok(self.run_install(package=package, answer='y\n'))
        self.assertIn('install: new', output)
        self.assertEqual(output.count('Install Namedrop for Rex?'), 1)
        installed = self.home / '.local/state/namedrop'
        self.assertTrue((installed / 'runtime/current').is_symlink())
        shutil.rmtree(installed)
        (package / 'bin/namedrop').write_text('#!/bin/sh\necho fixture-publish-failed >&2\nexit 23\n')
        result = self.run_install('--yes', package=package)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('non-zero exit status 23', result.stderr)
        self.assertNotIn('In the Rex palette', result.stdout)


if __name__ == '__main__':
    unittest.main(verbosity=2)
