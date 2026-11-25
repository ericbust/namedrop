"""U9's archive and installer contract, using only scratch homes and stub hosts."""
import hashlib
import json
import os
from pathlib import Path
import pty
import select
import shutil
import signal
import subprocess
import sys
import tarfile
import tempfile
import time
import unittest
from unittest.mock import patch

REPO = Path(__file__).resolve().parents[1]
VERSION = (REPO / 'Cargo.toml').read_text().split('version = "', 1)[1].split('"', 1)[0]
CONTRACT = json.loads((REPO / 'runtime.json').read_text())
# macOS's /usr/bin/python3 caches bytecode under HOME. The python3 on the
# stub PATH does the same, so a write that would land there is seen.
CACHE = 'Library/Caches/com.apple.python'
# macOS's own Python, which the script runs under on a Mac with the
# developer tools and nothing else, and the one running these tests.
SYSTEM_PYTHON = '/usr/bin/python3'
PYTHONS = [sys.executable] + [SYSTEM_PYTHON] * (sys.platform == 'darwin' and subprocess.run(
    [SYSTEM_PYTHON, '-c', 'import sys'], env={'PATH': '/usr/bin:/bin'}, capture_output=True).returncode == 0)
# The Words test (tests/words.rs) reads what each run printed from here.
CAPTURE = os.environ.get('NAMEDROP_WORDS_CAPTURE')


def capture(surface, text):
    if CAPTURE and text:
        with open(CAPTURE, 'a') as log:
            log.write(json.dumps({'surface': surface, 'text': text}) + '\n')

# A launcher from before 1.12.0, as 1.11.1 answers: its contract, and exit
# 64 with an invalid request for any argument it does not know.
OLD_LAUNCHER = f'''#!/bin/sh
[ "$*" = --runtime-info ] && {{ echo '{json.dumps(CONTRACT, separators=(',', ':'))}'; exit 0; }}
echo '{{"status":"error","error":{{"code":"invalid_request"}}}}'
exit 64
'''

STUB = '''#!{python} -B
import json, os, shutil, sqlite3, sys
from pathlib import Path
args = sys.argv[1:]
mode = os.environ.get('STUB_MODE', '')
home = Path(os.environ['HOME'])
with open(os.environ['CALLS'], 'a') as log:
    log.write(json.dumps([sys.argv[0], *args]) + '\\n')
config = home / '.config/rex/init.lua'
module = config.with_name('namedrop.lua')
targets = [dict(what='Rex module', path=str(module)), dict(what='Shell command', path=str(home / '.local/bin/namedrop')),
           dict(what='PATH lines', path=str(home / '.zshrc')),
           dict(what='Claude Code package', path=str(home / '.claude/skills/namedrop'))]
if args == ['--version']:
    print('namedrop {version}')
elif args == ['--runtime-info']:
    print({contract!r})
elif args == ['--capabilities']:
    print('{{"rex_ui":1}}')
elif args == ['--install-preview']:
    if mode == 'no-server':
        print('Namedrop found no running Rex server: could not reach Rex. Start Rex, or set REX_SERVER to its '
              'Unix socket, then run sh scripts/install.sh again.', file=sys.stderr)
        sys.exit(69)
    print(json.dumps(dict(server='rex:/tmp/u9-stub-only.sock', config=str(config), module=str(module), targets=targets)))
elif args[:1] == ['--migrate-runtime-state']:
    sys.path.insert(0, {tests!r})
    from runtime_activation import schema
    schema(Path(args[1]) / 'namedrop.sqlite3')
    with sqlite3.connect(str(Path(args[1]) / 'namedrop.sqlite3')) as db:
        db.execute('PRAGMA user_version={schema}')
    print(json.dumps(dict(state_schema={schema})))
elif 'install' in args:
    assert args[args.index('--input-json') + 1] == '{{"confirmed":true}}'
    with open(os.environ['CALLS'], 'a') as log:
        log.write(json.dumps(['rex on PATH', shutil.which('rex')]) + '\\n')
    print('stub progress line', file=sys.stderr, flush=True)
    state = Path(args[args.index('--plugin-state') + 1])
    launcher = str(state / 'runtime/namedrop')
    config.parent.mkdir(parents=True, exist_ok=True)
    module.write_text(launcher)
    config.write_text('pcall(require, "namedrop");\\n')
    record = dict(state=str(state), launcher=launcher, module=str(module), config=str(config),
                  line='inserted', server='rex:/tmp/u9-stub-only.sock')
    directory = home / '.config/namedrop'
    directory.mkdir(parents=True, exist_ok=True)
    (directory / 'rex.json').write_text(json.dumps(record))
    (directory / 'state-directory').write_text(str(state) + '\\n')
    data = dict(state_directory=str(state), module=str(module), config=str(config), line='inserted',
                keys=[dict(key='alt+shift+cmd+i', action='namedrop_name', status='in_effect'),
                      dict(key='alt+shift+cmd+m', action='namedrop_manage', status='shadowed',
                           effective_action='my_action', source='init.lua:4')],
                confirmed=True, targets=targets,
                shell_command=dict(path=str(home / '.local/bin/namedrop'), shell_startup_files=[str(home / '.zshrc')],
                                   current_shell='export PATH="$HOME/.local/bin:$PATH"',
                                   next_step='Open a new terminal, or run the current_shell command in this terminal.'),
                integration_packages=dict(codex=dict(installed=True)),
                agent_integration_hook_activation=dict(codex=dict(cache_current=True, migration_ready=True,
                    steps=['Approve the Namedrop hooks in Codex.'])))
    if mode == 'manual':
        data.update(manual_config=True, reason='Namedrop could not edit init.lua: denied.',
                    instruction='Put the line in init.lua yourself.')
    operation = 'activate' if mode == 'bad-report' else 'install'
    print(json.dumps(dict(status='ok', operation=operation, data=data)))
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
        for name in ('dirname', 'awk', 'mkdir', 'cp', 'chmod', 'mktemp', 'tar', 'gzip', 'rm', 'ln', 'shasum',
                     'sha256sum'):
            tool = shutil.which(name)
            if tool:
                (self.tools / name).symlink_to(tool)
        self.script('uname', '#!/bin/sh\ncase "$1" in -s) echo Darwin;; -m) echo arm64;; esac\n')
        self.script('file', '#!/bin/sh\necho "Mach-O 64-bit executable arm64"\n')
        self.script('rex', '#!/bin/sh\nexit 97\n')
        self.python(sys.executable)
        self.env = {k: v for k, v in os.environ.items() if k not in {
            'REX_BLOCK', 'REX_SESSION', 'REX_SERVER', 'REX_CLIENT', 'REX_BIN_DIR',
            'XDG_CONFIG_HOME', 'XDG_STATE_HOME', 'NAMEDROP_STATE_DIR', 'CODEX_HOME',
            'CLAUDE_CONFIG_DIR', 'HERDR_PLUGIN_STATE_DIR', 'PYTHONPATH', 'PYTHONHOME',
            'PYTHONDONTWRITEBYTECODE', 'PYTHONPYCACHEPREFIX', 'STUB_MODE'}}
        self.env.update(HOME=str(self.home), PATH=str(self.tools), CALLS=str(self.root / 'calls'),
                        XDG_RUNTIME_DIR=str(self.root / 'run'))
        self.cwd = None
        self.binary = self.root / 'namedrop'
        self.binary.write_text(STUB.format(python=sys.executable, version=VERSION,
                                          contract=json.dumps(CONTRACT), schema=CONTRACT['state_schema'],
                                          tests=str(REPO / 'tests')))
        self.binary.chmod(0o755)
        # A built checkout: the scripts under test and the stub as its binary.
        self.checkout = self.root / 'checkout'
        shutil.copytree(REPO / 'scripts', self.checkout / 'scripts',
                        ignore=shutil.ignore_patterns('__pycache__'))
        for name in ('runtime.json', 'herdr-plugin.toml'):
            shutil.copy2(REPO / name, self.checkout / name)
        (self.checkout / 'bin').mkdir()
        shutil.copy2(self.binary, self.checkout / 'bin/namedrop')

    def script(self, name, text):
        path = self.tools / name
        path.write_text(text)
        path.chmod(0o755)
        return path

    def python(self, executable):
        self.script('python3', f'#!/bin/sh\nexec "{executable}" -X pycache_prefix="$HOME/{CACHE}" "$@"\n')

    def run_install(self, *args, package=None, answer='', stdin=None):
        package = package or self.checkout
        result = subprocess.run(['/bin/sh', str(package / 'scripts/install.sh'), *args],
                                input=answer if stdin is None else None, stdin=stdin,
                                env=self.env, cwd=self.cwd, capture_output=True, text=True)
        capture('install.sh', result.stdout)
        capture('install.sh', result.stderr)
        return result

    def ok(self, result):
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result.stdout

    def snapshot(self, root=None):
        root = root or self.home
        return {str(p.relative_to(root)): ('link', os.readlink(p)) if p.is_symlink()
                else ('file', p.read_bytes(), p.stat().st_mode) if p.is_file() else ('dir',)
                for p in root.rglob('*')}

    def unchanged(self, before):
        after = self.snapshot(self.root)
        after.pop('calls', None)
        before = dict(before)
        before.pop('calls', None)
        self.assertEqual(after, before)

    def calls(self):
        path = self.root / 'calls'
        return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

    def no_cache(self):
        self.assertFalse((self.home / CACHE).exists(), 'Python cached bytecode under HOME')
        self.assertEqual(list(self.root.rglob('__pycache__')), [], 'Python cached bytecode beside a script')

    def record(self, owner='standalone', capable=True, legacy=False, state=None):
        """A record naming a state of `owner`. Herdr lists Namedrop for a
        state of its own, as it does until `herdr plugin uninstall`."""
        state = state or self.root / 'recorded state'
        if owner == 'herdr' or legacy:
            self.registry(['namedrop'])
        runtime = state / 'runtime'
        runtime.mkdir(parents=True)
        launcher = runtime / 'namedrop'
        if capable:
            shutil.copy2(self.binary, launcher)
        else:
            launcher.write_text(OLD_LAUNCHER)
            launcher.chmod(0o755)
        if legacy:
            (runtime / 'current').mkdir()
        else:
            (runtime / 'owner').write_text(owner + '\n')
        directory = self.home / '.config/namedrop'
        directory.mkdir(parents=True, exist_ok=True)
        (directory / 'state-directory').write_text(str(state) + '\n')
        return state

    def archive(self):
        # The packager runs on the maintainer's machine, not in this HOME.
        packager = self.root / 'packager home'
        packager.mkdir(exist_ok=True)
        result = subprocess.run(['/bin/sh', str(REPO / 'scripts/package-release.sh'),
                                 '--target', 'aarch64-apple-darwin', '--binary', str(self.binary),
                                 '--output', str(self.root / 'dist')], env={**self.env, 'HOME': str(packager)},
                                capture_output=True, text=True)
        self.ok(result)
        (self.root / 'calls').unlink(missing_ok=True)
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
        (self.tools / 'uname').unlink()
        result = self.run_install('--yes')
        self.assertEqual(result.returncode, 69)
        self.assertIn('reports no platform (uname did not run).', result.stderr)
        self.assertEqual(self.snapshot(), {})

    def test_confirmation_is_once_and_read_only(self):
        state = self.record('herdr')
        before = self.snapshot(self.root)
        output = self.ok(self.run_install(answer='n\n'))
        self.assertIn('install: adopt', output)
        self.assertEqual(output.count('Install Namedrop for Rex?'), 1)
        self.assertIn('Installation cancelled; no files changed.', output)
        # The beta notice comes before the question.
        notice = ('Rex support is beta and under active development. '
                  'docs/compatibility.md lists the Rex versions it was tested with.\n')
        self.assertIn(notice, output.split('Install Namedrop for Rex?')[0])
        # The confirmation lists what the binary says the install writes.
        for line in ('Rex module: ', 'PATH lines: ' + str(self.home / '.zshrc'),
                     'Claude Code package: ' + str(self.home / '.claude/skills/namedrop')):
            self.assertIn(line, output)
        self.unchanged(before)
        self.no_cache()
        self.assertEqual([call[1:] for call in self.calls()], [['--capabilities'], ['--install-preview']])
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

    def registry(self, plugins, config=None):
        """Herdr's plugin registry, in `config` or ~/.config, holding
        `plugins`: a list of plugin IDs, or bytes written as they are."""
        path = (config or self.home / '.config') / 'herdr/plugins.json'
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(plugins if isinstance(plugins, bytes) else json.dumps(
            [dict(plugin_id=plugin, name=plugin, enabled=True) for plugin in plugins]).encode())
        return path

    def refused(self, *expected):
        before = self.snapshot(self.root)
        result = self.run_install('--yes')
        self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
        for text in expected:
            self.assertIn(text, result.stderr)
        self.assertNotIn('Errno', result.stderr)
        self.assertNotIn('Namedrop Rex install:', result.stdout)
        self.unchanged(before)
        return result.stderr

    @staticmethod
    def unlisted(state, registry, relative=False):
        """The refusal for a recorded Herdr state that the list read does not name."""
        return (f"Namedrop's install record names Herdr's Namedrop state ({state}), but Herdr's plugin list in "
                f'{registry} does not name Namedrop.{Installer.elsewhere(relative)} Otherwise install Namedrop '
                'in Herdr again, or move that state directory away, then run sh scripts/install.sh again.')

    @staticmethod
    def unreadable(herdr, registry, relative=False):
        """The refusal for a Herdr plugin list it cannot read."""
        return (f"Namedrop cannot read Herdr's plugin list in {registry}, so it cannot tell whether Namedrop is "
                f'still installed in Herdr ({herdr}).{Installer.elsewhere(relative)} Otherwise, if Namedrop is '
                'not installed in Herdr, move that state directory away, then run sh scripts/install.sh again.')

    @staticmethod
    def elsewhere(relative):
        """Each list refusal's first way out: Herdr may read another list."""
        return ((" XDG_CONFIG_HOME is relative, so Namedrop read that list from this working directory, which "
                 "may not be Herdr's." if relative else '') +
                " If Herdr uses another config directory, run sh scripts/install.sh again with Herdr's "
                'XDG_CONFIG_HOME.')

    def test_a_recorded_herdr_state_is_adopted_only_while_herdr_lists_namedrop(self):
        # `herdr plugin uninstall` keeps the state and the record that names
        # it, and nothing would update its runtime (round A's review, F1).
        herdr = self.record('herdr')
        self.assertIn('install: adopt', self.ok(self.run_install(answer='n\n')))
        registry = self.home / '.config/herdr/plugins.json'
        for label, plugins in [('another plugin', ['other']), ('no plugins', []), ('no registry', None)]:
            with self.subTest(label):
                if plugins is None:
                    registry.unlink()
                else:
                    self.registry(plugins)
                (self.root / 'calls').unlink(missing_ok=True)
                self.refused(self.unlisted(herdr, registry))
                self.assertEqual([call for call in self.calls() if 'install' in call], [])
        registry = self.registry(b'[')
        self.refused(self.unreadable(herdr, registry))
        # The refusal names the list it read, and the way out for a Herdr
        # that reads another one, before moving its state away (round C's
        # review, F2): this shell's XDG_CONFIG_HOME is not Herdr's.
        self.registry(['namedrop'])
        config = self.root / 'xdg config'
        self.env['XDG_CONFIG_HOME'] = str(config)
        self.refused(self.unlisted(herdr, self.registry(['other'], config)))
        # A relative one is read from this working directory, which may not
        # be Herdr's (round C's review, F3).
        self.cwd = self.root / 'working directory'
        self.cwd.mkdir()
        self.env['XDG_CONFIG_HOME'] = 'relative config'
        listed = self.registry(['other'], self.cwd / 'relative config')
        read = Path(os.path.realpath(self.cwd)) / 'relative config/herdr/plugins.json'
        self.refused(self.unlisted(herdr, read, relative=True))
        listed.unlink()
        self.refused(self.unreadable(herdr, read, relative=True))
        self.registry(['namedrop'], self.cwd / 'relative config')
        self.assertIn('install: adopt', self.ok(self.run_install(answer='n\n')))
        self.cwd = None
        del self.env['XDG_CONFIG_HOME']
        # Moved away, the state leaves a stale record, so the archive makes
        # an install of its own.
        self.registry([])
        shutil.move(herdr, self.root / 'moved away')
        self.assertIn('install: new', self.ok(self.run_install(answer='n\n')))

    def test_herdr_without_a_record_is_refused(self):
        # Herdr's Namedrop before step 6 writes no record, only its state.
        for xdg in (None, self.root / 'xdg state'):
            with self.subTest(xdg=xdg):
                if xdg:
                    self.env['XDG_STATE_HOME'] = str(xdg)
                herdr = (xdg or self.home / '.local/state') / 'herdr/plugins/namedrop'
                (herdr / 'runtime/current').mkdir(parents=True)
                self.registry(['other', 'namedrop'])
                self.refused(f'Namedrop is already installed in Herdr ({herdr}). '
                             'Update Namedrop in Herdr first, then run sh scripts/install.sh again.')
                shutil.rmtree(herdr)
        # The home default is not Herdr's when XDG_STATE_HOME moves it.
        (self.home / '.local/state/herdr/plugins/namedrop/runtime/current').mkdir(parents=True)
        self.assertIn('install: new', self.ok(self.run_install(answer='n\n')))

    def test_herdr_state_counts_only_while_herdr_lists_namedrop(self):
        # `herdr plugin uninstall` keeps the plugin's state (the U9 review's N1).
        herdr = self.home / '.local/state/herdr/plugins/namedrop'
        (herdr / 'runtime/current').mkdir(parents=True)
        update = 'Update Namedrop in Herdr first, then run sh scripts/install.sh again.'
        for label, plugins in [('no registry', None), ('no plugins', []), ('another plugin', ['other'])]:
            with self.subTest(label):
                if plugins is not None:
                    self.registry(plugins)
                self.assertIn('install: new', self.ok(self.run_install(answer='n\n')))
        # Herdr's own rule for its config directory (herdr's
        # config::config_dir): XDG_CONFIG_HOME as it is whenever it is set,
        # else ~/.config. Herdr reads an empty or relative one from its own
        # working directory, so a list missing from this one says nothing.
        config = self.root / 'xdg config'
        self.registry(['namedrop'], config)
        self.env['XDG_CONFIG_HOME'] = str(config)
        self.refused(update)
        self.registry(['namedrop'])
        self.cwd = self.root / 'working directory'
        self.cwd.mkdir()
        for value, config in [('', self.cwd), ('relative config', self.cwd / 'relative config')]:
            with self.subTest(XDG_CONFIG_HOME=value):
                self.env['XDG_CONFIG_HOME'] = value
                registry = self.registry(['other'], config)
                self.assertIn('install: new', self.ok(self.run_install(answer='n\n')))
                self.registry(['namedrop'], config)
                self.refused(update)
                registry.unlink()
                # The list it looked for, by the path that names it.
                looked = Path(os.path.realpath(self.cwd)) / registry.relative_to(self.cwd)
                self.refused(self.unreadable(herdr, looked, relative=True))
        self.cwd = None
        del self.env['XDG_CONFIG_HOME']
        # A registry it cannot read names the way out, under each Python the
        # script may run with: macOS's own parses deep nesting by recursion.
        for python in PYTHONS:
            self.python(python)
            for label, contents in [('not JSON', b'['), ('an object', b'{"plugin_id": "namedrop"}'),
                                    ('nested too deep', b'[' * 100000)]:
                with self.subTest(label, python=python):
                    registry = self.registry(contents)
                    stderr = self.refused(self.unreadable(herdr, registry))
                    self.assertNotIn('Traceback', stderr)
        self.python(sys.executable)
        registry.unlink()
        registry.mkdir()
        self.refused(f"Namedrop cannot read Herdr's plugin list in {registry},")
        shutil.rmtree(herdr)
        self.assertIn('install: new', self.ok(self.run_install(answer='n\n')))

    def test_a_current_herdr_whose_record_was_lost_is_told_to_activate(self):
        herdr = self.home / '.local/state/herdr/plugins/namedrop'
        (herdr / 'runtime').mkdir(parents=True)
        (herdr / 'runtime/owner').write_text('herdr\n')
        shutil.copy2(self.binary, herdr / 'runtime/namedrop')
        self.registry(['namedrop'])
        self.refused(f'Namedrop is already installed in Herdr ({herdr}). '
                     'Run Activate Namedrop in Herdr first, then run sh scripts/install.sh again.')
        self.assertEqual([call[1:] for call in self.calls()], [['--capabilities']])
        # A release before 1.12.0, or no launcher, is a release to update.
        (herdr / 'runtime/namedrop').write_text(OLD_LAUNCHER)
        self.refused('Update Namedrop in Herdr first, then run sh scripts/install.sh again.')
        (herdr / 'runtime/namedrop').unlink()
        self.refused('Update Namedrop in Herdr first, then run sh scripts/install.sh again.')

    def test_an_owner_that_cannot_be_read_is_named(self):
        herdr = self.home / '.local/state/herdr/plugins/namedrop'
        (herdr / 'runtime/owner').mkdir(parents=True)
        self.registry(['namedrop'])
        message = (f"Namedrop cannot read the owner of Herdr's Namedrop state in {herdr / 'runtime/owner'}: "
                   'it is not a readable file. Inspect that file, then run sh scripts/install.sh again.')
        self.refused(message)
        (herdr / 'runtime/owner').rmdir()
        (herdr / 'runtime/owner').write_bytes(b'\xffherdr\n')
        self.refused(message)
        if os.geteuid():
            (herdr / 'runtime/owner').write_text('herdr\n')
            before = self.snapshot(self.root)
            (herdr / 'runtime/owner').chmod(0)
            result = self.run_install('--yes')
            (herdr / 'runtime/owner').chmod(0o644)
            self.assertEqual(result.returncode, 2, result.stdout + result.stderr)
            self.assertIn(message, result.stderr)
            self.unchanged(before)
        shutil.rmtree(herdr)
        state = self.root / 'selected state'
        (state / 'runtime/owner').mkdir(parents=True)
        self.env['NAMEDROP_STATE_DIR'] = str(state)
        self.refused(f"Namedrop cannot read the owner of this Namedrop state in {state / 'runtime/owner'}: "
                     'it is not a readable file.')

    def test_herdr_adoption_publishes_nothing(self):
        state = self.record('herdr')
        package = self.archive()
        before = self.snapshot(state)
        output = self.ok(self.run_install('--yes', package=package))
        self.assertIn('install: adopt', output)
        # The report is lines a person reads, not JSON.
        self.assertNotIn('{', output)
        self.assertIn('  alt+shift+cmd+i: namedrop_name (in effect)\n', output)
        self.assertIn('  alt+shift+cmd+m: namedrop_manage (shadowed by my_action from init.lua:4)\n', output)
        self.assertIn('Agent hooks:\n  Codex:\n    Approve the Namedrop hooks in Codex.\n', output)
        self.assertIn('Shell command: ' + str(self.home / '.local/bin/namedrop')
                      + '\n  Open a new terminal, or run the current_shell command in this terminal.\n'
                      '  current_shell: export PATH="$HOME/.local/bin:$PATH"\n', output)
        report = output.split('Installed or updated:\n', 1)[1]
        self.assertIn('  PATH lines: ' + str(self.home / '.zshrc') + '\n', report)
        self.assertEqual(self.snapshot(state), before)
        self.assertFalse((self.home / '.local/state/namedrop').exists())
        self.assertEqual((self.home / '.config/rex/namedrop.lua').read_text(), str(state / 'runtime/namedrop'))
        installs = [call for call in self.calls() if 'install' in call]
        self.assertEqual(installs, [[str(state / 'runtime/namedrop'), '--plugin-state', str(state),
                                    '--json', 'install', '--input-json', '{"confirmed":true}']])

    def test_report_is_validated_and_manual_config_is_printed(self):
        self.record('herdr')
        self.env['STUB_MODE'] = 'bad-report'
        result = self.run_install('--yes')
        self.assertEqual(result.returncode, 2)
        self.assertIn('Namedrop did not return an installation report.', result.stderr)
        self.assertNotIn('In the Rex palette', result.stdout)
        self.env['STUB_MODE'] = 'manual'
        self.assertIn('\nNamedrop could not edit init.lua: denied.\nPut the line in init.lua yourself.\n',
                      self.ok(self.run_install('--yes')))

    def test_retirement_precedes_owner_and_default_target_is_guarded(self):
        state = self.record('invalid')
        (state / 'runtime/retired.json').write_text(json.dumps(dict(reason='herdr_handoff')))
        before = self.snapshot(self.root)
        result = self.run_install('--yes')
        self.assertIn('Namedrop is moving to Herdr. Run Activate Namedrop in Herdr again.', result.stderr)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.snapshot(self.root), before)
        (state / 'runtime/retired.json').write_text(json.dumps(dict(reason='unknown')))
        result = self.run_install('--yes')
        self.assertEqual(result.returncode, 2)
        self.assertIn('Invalid Namedrop retirement marker in ' + str(state), result.stderr)
        (self.home / '.config/namedrop/state-directory').unlink()
        (state / 'runtime/retired.json').unlink()
        (state / 'runtime/owner').write_text('herdr\n')
        self.env['NAMEDROP_STATE_DIR'] = str(state)
        before = self.snapshot(self.root)
        result = self.run_install('--yes')
        self.assertEqual(result.returncode, 2)
        self.assertIn('ND-INSTALL-002', result.stderr)
        # install.sh refuses before it lists or asks anything.
        self.assertNotIn('Namedrop Rex install:', result.stdout)
        self.assertEqual(self.calls(), [])
        self.unchanged(before)
        (state / 'runtime/retired.json').write_text(json.dumps(dict(reason='herdr_handoff')))
        self.assertIn('Namedrop is moving to Herdr.', self.run_install('--yes').stderr)

    def test_current_directory_is_not_imported(self):
        planted = self.root / 'planted'
        planted.mkdir()
        (planted / 'json.py').write_text('raise SystemExit("planted json.py ran")\n')
        result = subprocess.run(['/bin/sh', str(self.checkout / 'scripts/install.sh')], input='n\n', cwd=planted,
                                env=self.env, capture_output=True, text=True)
        self.assertNotIn('planted', result.stdout + result.stderr)
        self.assertIn('Installation cancelled; no files changed.', self.ok(result))

    def test_record_is_read_as_the_binary_reads_it(self):
        state = self.record()
        record = self.home / '.config/namedrop/state-directory'
        launcher = state / 'runtime/namedrop'
        executable = self.root / 'elsewhere'
        shutil.copy2(launcher, executable)

        def decision():
            output = self.ok(self.run_install(answer='n\n'))
            return output.split('Namedrop Rex install: ', 1)[1].split('\n', 1)[0]

        self.assertEqual(decision(), 'update')
        record.write_text(str(state))
        self.assertEqual(decision(), 'update', 'the trailing newline is optional')
        record.write_text(str(state) + '\n\n')
        self.assertEqual(decision(), 'new', 'only one trailing newline is stripped')
        record.write_bytes(b'\xff' + str(state).encode() + b'\n')
        self.assertEqual(decision(), 'new', 'a record that is not UTF-8 is no record')
        record.write_text(str(state) + '\n')
        launcher.chmod(0o644)
        self.assertEqual(decision(), 'new', 'a launcher that cannot run is stale')
        launcher.unlink()
        launcher.symlink_to(executable)
        self.assertEqual(decision(), 'new', 'a linked launcher is stale, as symlink_metadata sees it')

    def test_home_must_be_set_and_absolute(self):
        before = self.snapshot(self.root)
        self.env['HOME'] = 'relative home'
        result = self.run_install('--yes')
        self.assertEqual(result.returncode, 2)
        self.assertIn('Namedrop install needs an absolute HOME.', result.stderr)
        del self.env['HOME']
        result = self.run_install('--yes')
        self.assertEqual(result.returncode, 2)
        self.assertIn('Namedrop install needs HOME set to your home directory.', result.stderr)
        self.assertNotIn('Traceback', result.stderr)
        self.unchanged(before)

    def test_no_rex_server_stops_before_any_write(self):
        package = self.archive()
        self.env['STUB_MODE'] = 'no-server'
        before = self.snapshot(self.root)
        result = self.run_install('--yes', package=package)
        self.assertEqual(result.returncode, 69)
        self.assertIn('Start Rex', result.stderr)
        self.assertIn('then run sh scripts/install.sh again.', result.stderr)
        self.assertNotIn('{', result.stderr)
        self.assertNotIn('Namedrop Rex install:', result.stdout)
        self.unchanged(before)
        self.assertEqual([call[1:] for call in self.calls()], [['--install-preview']])

    def test_closed_stdin(self):
        self.record('herdr')
        before = self.snapshot(self.root)
        result = subprocess.run(
            ['/bin/sh', '-c', 'exec /bin/sh "$0" <&-', str(self.checkout / 'scripts/install.sh')],
            env=self.env, capture_output=True, text=True)
        self.assertIn('Installation cancelled; no files changed.', self.ok(result))
        self.unchanged(before)
        result = subprocess.run(
            ['/bin/sh', '-c', 'exec /bin/sh "$0" --yes <&-', str(self.checkout / 'scripts/install.sh')],
            env=self.env, capture_output=True, text=True)
        self.assertIn('In the Rex palette', self.ok(result))

    def test_interrupt_at_the_prompt(self):
        self.record('herdr')
        before = self.snapshot(self.root)
        pid, terminal = pty.fork()
        if pid == 0:
            # A terminal's session starts with SIGINT at its default. A test
            # run from a background job inherits it ignored, and then no
            # Ctrl-C reaches the installer.
            signal.signal(signal.SIGINT, signal.SIG_DFL)
            os.execve('/bin/sh', ['/bin/sh', str(self.checkout / 'scripts/install.sh')], self.env)
        output = b''
        deadline = time.monotonic() + 30
        sent = False
        # Python acts on a SIGINT only between steps of its own: one that
        # lands after the prompt is written and before the installer's read
        # starts waits until the read returns. So the Ctrl-C comes once the
        # prompt has been still for a moment, as a person's does, and once
        # more if nothing answers it, when the read is surely waiting.
        quiet = None
        while time.monotonic() < deadline:
            if select.select([terminal], [], [], 0.05)[0]:
                try:
                    data = os.read(terminal, 4096)
                except OSError:
                    break
                if not data:
                    break
                output += data
                quiet = time.monotonic()
            if not sent and b'[y/N]' in output and time.monotonic() - quiet >= 0.2:
                os.write(terminal, b'\x03')
                sent = time.monotonic()
                # Cancellation gets its own observation window after startup.
                deadline = time.monotonic() + 30
            elif sent and output.endswith(b'^C') and time.monotonic() - sent >= 5:
                os.write(terminal, b'\x03')
                sent = time.monotonic()
        os.close(terminal)
        os.waitpid(pid, 0)
        text = output.decode(errors='replace')
        self.assertTrue(sent, text)
        self.assertIn('Installation cancelled; no files changed.', text)
        self.assertNotIn('Traceback', text)
        self.unchanged(before)

    def test_interrupt_near_startup_deadline(self):
        monotonic, write = time.monotonic, os.write
        elapsed = 0

        def clock():
            return monotonic() + elapsed

        def interrupt(terminal, data):
            nonlocal elapsed
            result = write(terminal, data)
            if data == b'\x03':
                elapsed += 31
            return result

        with patch.object(time, 'monotonic', clock), patch.object(os, 'write', interrupt):
            self.test_interrupt_at_the_prompt()

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
        result = self.run_install('--yes', package=package)
        output = self.ok(result)
        self.assertIn('install: new', output)
        # Progress reaches the terminal while the runtime is published.
        self.assertIn('Publishing the Namedrop runtime', output)
        self.assertIn('stub progress line', result.stderr)
        # What install.sh runs finds the rex it found.
        self.assertIn(['rex on PATH', str(rex_bin / 'rex')], self.calls())
        self.no_cache()
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
        self.no_cache()

    def test_new_confirmation_default_home_and_publisher_failure(self):
        before = self.snapshot(self.root)
        self.ok(self.run_install(answer=''))
        self.unchanged(before)
        package = self.archive()
        output = self.ok(self.run_install(package=package, answer='y\n'))
        self.assertIn('install: new', output)
        self.assertEqual(output.count('Install Namedrop for Rex?'), 1)
        self.assertEqual(output.count('Rex support is beta and under active development.'), 1)
        self.assertTrue((package / 'docs/compatibility.md').is_file())
        installed = self.home / '.local/state/namedrop'
        self.assertTrue((installed / 'runtime/current').is_symlink())
        shutil.rmtree(installed)
        # The preview answers; the publish fails.
        shutil.copy2(package / 'bin/namedrop', self.root / 'previewer')
        (package / 'bin/namedrop').write_text(
            f'#!/bin/sh\n[ "$1" = --install-preview ] && exec "{self.root / "previewer"}" "$@"\n'
            'echo fixture-publish-failed >&2\nexit 23\n')
        result = self.run_install('--yes', package=package)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn('non-zero exit status 23', result.stderr)
        self.assertNotIn('In the Rex palette', result.stdout)

    @unittest.skipUnless(SYSTEM_PYTHON in PYTHONS, "needs macOS's own Python")
    def test_system_python_writes_nothing_before_confirmation(self):
        self.python('/usr/bin/python3')
        state = self.record('herdr')
        before = self.snapshot(self.root)
        self.ok(self.run_install(answer='n\n'))
        self.unchanged(before)
        (state / 'runtime/retired.json').write_text(json.dumps(dict(reason='herdr_handoff')))
        self.assertEqual(self.run_install('--yes').returncode, 2)
        self.no_cache()


if __name__ == '__main__':
    unittest.main(verbosity=2)
