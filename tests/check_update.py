"""Behavioral tests for the detached worker; run by cargo test."""

import copy
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
# `git ls-remote --tags` of ericbust/namedrop when U10 was reviewed: its
# release tags, of which none ships scripts/install.sh.
REPOSITORY_LISTING = ''.join(line + '\n' for line in [
    '6f4fa61ad3cb8cccd459b6cab0a0d15d0e7831e5\trefs/tags/v1.0.0',
    '8b464a4d58ae8e605848bb894f7efc9877b09c50\trefs/tags/v1.0.1',
    'a178aef833c915317a46d016a1ebf15376a21800\trefs/tags/v1.0.2',
    '448298bdb625480d86591fbf9c1a17244a33d996\trefs/tags/v1.1.0',
    '3b83b710b60518748d1a2b039ebc8d08a38dc44f\trefs/tags/v1.1.1',
    'f70164e7498237f19a9148df090db7e792443962\trefs/tags/v1.10.0',
    '6f6daf01a2a0eedcd590d0cab2fd4be0f396fc59\trefs/tags/v1.11.0',
    'eb5b50231c36aca7374840fa7141518403d7b1dd\trefs/tags/v1.11.1',
    'bc70992ea1c42013be3b6c242ccc6cf626b090a8\trefs/tags/v1.2.0',
    '3f95a3f1ae59221f3895edacdf67578fcfeba6fb\trefs/tags/v1.3.0',
    '78b677f41c0f2175d42f1bc1d05cab4e57fb24db\trefs/tags/v1.3.1',
    'b1275567d355fabd57354827503be99ae53105ac\trefs/tags/v1.3.2',
    '12f16b548e21f822772149f97825d9c1946a2922\trefs/tags/v1.3.3',
    'b52c23faadd508c532f3733051ea43c08fd6c167\trefs/tags/v1.4.1',
    '3864c16f8778fa88e4a1be90cbb6d808e97de1ac\trefs/tags/v1.4.2',
    'be57d5871f1221975e2d17cc4e3cee5e95630856\trefs/tags/v1.4.3',
    'b28ef08a69811bafc937418dd04185b24dbbc5b8\trefs/tags/v1.4.4',
    '22caabff69a6f95b65c110311f3c34de47c1e421\trefs/tags/v1.5.0',
    'caf657d1e6fe233e7847ca720d8b260784ca0033\trefs/tags/v1.6.0',
    'eebd07c00aaae0aba4d27d4c5578edb8690c023f\trefs/tags/v1.6.1',
    '1fbdaf0bf6705e6581b167f0b6953593a2e6eaf7\trefs/tags/v1.6.2',
    'caa56f1c918e2d644807da27003cb9c1dce938a9\trefs/tags/v1.6.3',
    'ce571c6c663295152e65778f05008028fc193614\trefs/tags/v1.6.4',
    'd533eeefdfaa84a2afd120e4bf127fe3aae53621\trefs/tags/v1.6.5',
    '3d89e56a96f15c8fa9fcc78b5559ae7c98a2b80a\trefs/tags/v1.6.6',
    'd1a30880a1bbb3215f3b1cd9461de932f3a1db6e\trefs/tags/v1.7.0',
    '08187b73b240c0dad310ec67d6f0feba3e56518c\trefs/tags/v1.7.1',
    '667e8da2085e03d4f9b9406919c75f61c1d90092\trefs/tags/v1.7.3',
    '4232e7cb8969b5a7a8fd35c4916aee60653853aa\trefs/tags/v1.7.4',
    'c834d2fce4b2c60b69031a9df9665c5a3627b4e5\trefs/tags/v1.7.5',
    '609a3b9d7581a335cc61172d501634e63919e939\trefs/tags/v1.7.6',
    '1d781b0cbe4947a9b24a2f50922e951cd3599494\trefs/tags/v1.7.7',
    'a3f2ebc665677d09bdc4eba747a9232f6782896c\trefs/tags/v1.7.8',
    '9bf44304fd0ab98edd23ea6c13801b04b7a4cb65\trefs/tags/v1.7.9',
    'c12e55d715137b08bf8ab5d3e2acc7a7e348ed33\trefs/tags/v1.8.0',
    '5b9b2899236c1649feb592aee457444655d8f16e\trefs/tags/v1.8.1',
    '1f2d3ffbbcacb87f4ab8454e02915adf019ca14d\trefs/tags/v1.9.0',
    'e41ce07422fd50e0bdefe79f37189d7882badb22\trefs/tags/v1.9.1',
])
# What a real `git ls-remote --tags <repository> 'v*'` printed for a scratch
# repository with an annotated pre-release, an annotated release, and refs
# that are no release.
SCRATCH_LISTING = ('0d99cae9ee776f96c5daa73d198aab3aebb6197a\trefs/tags/v1.0.0\n'
                   '0530d7b902aabf5f046c5975429f98152dd3164c\trefs/tags/v1.10.0-exp.3\n'
                   'fd548568e5c38381cbaf1c00632d774212e81a30\trefs/tags/v1.10.0-exp.3^{}\n'
                   'd0810793e7894111789f60bd56af70dd17bcf2d7\trefs/tags/v1.2.0\n'
                   'fd548568e5c38381cbaf1c00632d774212e81a30\trefs/tags/v1.2.0^{}\n'
                   'fd548568e5c38381cbaf1c00632d774212e81a30\trefs/tags/v2\n'
                   'fd548568e5c38381cbaf1c00632d774212e81a30\trefs/tags/vv8.8.8\n'
                   'fd548568e5c38381cbaf1c00632d774212e81a30\trefs/tags/x/v9.9.9\n')
spec = importlib.util.spec_from_file_location('check_update', Path(__file__).resolve().parents[1] / 'scripts/check-update.py')
worker = importlib.util.module_from_spec(spec)
spec.loader.exec_module(worker)


class UpdateCheck(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix='namedrop-check-')
        self.root = Path(self.temporary.name)
        self.state = self.root / 'state'
        self.settings = self.root / 'settings.json'
        self.value = dict(result=dict(type='plugin_list', plugins=[dict(
            plugin_id='namedrop', plugin_root=str(self.root), source=dict(
                kind='github', owner='owner', repo='repo', requested_ref='main',
                resolved_commit='a' * 40, managed_path=str(self.root)))]))
        self.calls = []
        self.commit = 'b' * 40
        self.remote = '1.10.0'
        # A test runs `git` only as a stub it wrote under its own root. Any
        # other `git` would reach GitHub, so the test fails instead (the U10
        # review's F4).
        self.unstubbed = []
        run = worker.run

        def guarded(command, timeout, **options):
            if command[0] == 'git':
                found = shutil.which('git', path=(options.get('env') or os.environ).get('PATH'))
                if not found or self.root.resolve() not in Path(found).resolve().parents:
                    self.unstubbed.append(command)
                    raise AssertionError(f'an unstubbed git would run: {found} {command}')
            return run(command, timeout, **options)
        patcher = patch.object(worker, 'run', guarded)
        patcher.start()
        self.addCleanup(patcher.stop)

    def tearDown(self):
        self.temporary.cleanup()
        self.assertEqual(self.unstubbed, [], 'an unstubbed git would have run')

    def registration(self):
        self.calls.append('registration')
        return self.value

    def lookup_commit(self, source):
        self.calls.append('git')
        return self.commit

    def lookup_version(self, source, commit):
        self.calls.append('manifest')
        return self.remote

    def check(self, now=100_000):
        worker.check(self.state, self.settings, now, self.registration,
                     self.lookup_commit, self.lookup_version)

    def cache(self):
        return json.loads((self.state / 'update-check.json').read_text())

    def standalone(self, kind='archive', origin='/tmp/archive', version='1.7.9'):
        runtime = self.state / 'runtime'
        runtime.mkdir(parents=True, exist_ok=True)
        (runtime / 'owner').write_text('standalone\n')
        (runtime / 'source.json').write_text(json.dumps(
            dict(kind=kind, origin=origin, version=version)))

    def offered(self, installed, releases, listing=None):
        """The version a standalone install of `installed` is offered when
        the repository's tags are `releases`, and the cache it leaves."""
        self.standalone(version=installed)
        listing = listing or ''.join(f'{index:040x}\trefs/tags/v{release}\n'
                                     for index, release in enumerate(releases))
        self.now = getattr(self, 'now', 0) + worker.DAY_MS
        with patch.object(worker, 'run', return_value=subprocess.CompletedProcess([], 0, listing, '')):
            self.check(now=self.now)
        self.assertEqual(self.calls, [], 'a standalone check consulted Herdr')
        return self.cache()['remote_version'], self.cache()

    def test_standalone_archive_uses_scripted_tags_without_herdr_or_manifest(self):
        self.standalone()
        git = self.root / 'git'
        log = self.root / 'git-args'
        git.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "' + str(log) + '"\n'
                       + 'printf "%s\\n" "' + 'a' * 40 + '\trefs/tags/v9.9.0" "'
                       + 'b' * 40 + '\trefs/tags/v9.10.0-exp.2" "'
                       + 'c' * 40 + '\trefs/tags/v9.10.0" "'
                       + 'd' * 40 + '\trefs/tags/v9.10.0^{}" "'
                       + 'e' * 40 + '\trefs/tags/v99.0.0-beta"\n')
        git.chmod(0o700)
        with patch.dict(os.environ, PATH=str(self.root) + os.pathsep + os.environ['PATH']), \
                patch.object(worker, 'run', wraps=worker.run) as run:
            self.check()
        self.assertEqual(self.calls, [])
        self.assertEqual(self.cache()['remote_version'], '9.10.0')
        self.assertEqual(self.cache()['remote_commit'], 'd' * 40)
        self.assertIsNone(self.cache()['last_error'])
        command = ['git', '-c', 'credential.helper=', '-c', 'core.hooksPath=/dev/null',
                   'ls-remote', '--tags', 'https://github.com/ericbust/namedrop.git', 'v*']
        self.assertEqual(log.read_text().splitlines(), command[1:])
        run.assert_called_once()
        self.assertEqual(run.call_args.args, (command,))
        with patch.dict(os.environ, PATH=str(self.root) + os.pathsep + os.environ['PATH']):
            self.assertEqual(run.call_args.kwargs, dict(text=True, timeout=8, env=dict(os.environ, GIT_TERMINAL_PROMPT='0')))
        self.assertEqual(self.cache()['source']['installed_version'], '1.7.9')

    def test_only_a_release_with_an_installer_on_the_installs_channel_is_offered(self):
        self.assertEqual(worker.INSTALLER_FLOOR, '2.0.0', 'move the floor only with docs/releasing.md')
        # Today's repository: every tag is below the floor, so nothing is
        # offered and nothing is an error. 1.11.1 is newer than 1.7.9 but
        # has no installer.
        for installed in ('1.7.9', '1.7.9-exp.1', '1.0.0'):
            remote, cache = self.offered(installed, None, REPOSITORY_LISTING)
            self.assertIsNone(remote, installed)
            self.assertIsNone(cache['remote_commit'])
            self.assertIsNone(cache['last_error'])
            self.assertEqual(cache['last_success_at'], self.now)
        # A stable install is never offered a pre-release, here the scratch
        # repository's v1.10.0-exp.3, which would be the newest otherwise.
        self.assertIsNone(self.offered('1.2.0', None, SCRATCH_LISTING)[0])
        self.assertIsNone(self.offered('1.0.0', None, SCRATCH_LISTING)[0])
        # At the floor and above it.
        self.assertEqual(self.offered('1.7.9', None, REPOSITORY_LISTING + 'f' * 40 + '\trefs/tags/v2.0.0\n')[0], '2.0.0')
        self.assertEqual(self.offered('1.7.9', ['1.99.99', '2.0.0-exp.4'])[0], None)
        self.assertEqual(self.offered('1.7.9', ['2.0.0', '2.0.1', '1.11.1'])[0], '2.0.1')
        for installed, releases, expected in [
                # Stable: only stable releases, never a pre-release, however new.
                ('2.0.0', ['2.0.1', '2.1.0-exp.3', '3.0.0-exp.1'], '2.0.1'),
                ('2.0.0', ['2.1.0-exp.3', '1.11.1'], None),
                ('2.0.1', ['2.0.0', '2.0.1'], None),
                # A pre-release: later pre-releases of its own version, and
                # newer stable releases, never another version's pre-release.
                ('2.1.0-exp.1', ['2.1.0-exp.3', '2.2.0-exp.1', '2.0.1'], '2.1.0-exp.3'),
                ('2.1.0-exp.1', ['2.1.0-exp.3', '2.1.0'], '2.1.0'),
                ('2.1.0-exp.3', ['2.1.0-exp.2', '2.1.0-exp.3', '3.0.0-exp.1', '12.1.0-exp.9'], None),
                ('2.1.0-exp.1', ['2.1.0-exp.10', '2.1.0-exp.9'], '2.1.0-exp.10'),
                ('2.1.0-exp.1', ['2.0.5', '2.1.1'], '2.1.1')]:
            with self.subTest(installed=installed, releases=releases):
                self.assertEqual(self.offered(installed, releases)[0], expected)

    def test_checkout_uses_only_its_github_origin(self):
        self.standalone(kind='checkout')
        metadata = self.state / 'runtime/source.json'
        for origin in ['https://github.com/team/project.git', 'git@github.com:team/project.git',
                       'ssh://git@github.com/team/project.git']:
            metadata.write_text(json.dumps(dict(kind='checkout', origin=origin, version='1.7.9')))
            source = worker.standalone_source(self.state)
            self.assertEqual((source['owner'], source['repo']), ('team', 'project'))
        for origin in ['/tmp/local', 'https://github.com.evil/team/project',
                       'https://github.com/team/project?token=secret']:
            metadata.write_text(json.dumps(dict(kind='checkout', origin=origin, version='1.7.9')))
            with patch.object(worker, 'run') as run:
                self.check(now=len(origin) * worker.DAY_MS)
                run.assert_not_called()
                self.assertIsNone(self.cache()['source'])

    def test_apple_git_stub_is_never_started_without_developer_tools(self):
        self.standalone()
        tools = self.root / 'tools'
        tools.mkdir()
        xcode = tools / 'xcode-select'
        xcode.write_text('#!/bin/sh\nexit 1\n')
        xcode.chmod(0o700)
        original_run = worker.run
        def guarded_run(command, **kwargs):
            self.assertEqual(command, ['xcode-select', '-p'], 'Git must never start')
            return original_run(command, **kwargs)
        def checked(owner, now):
            (self.state / 'runtime/owner').write_text(owner)
            with patch.object(worker.sys, 'platform', 'darwin'), \
                    patch.dict(os.environ, PATH=str(tools) + ':/usr/bin:/bin'), \
                    patch.object(worker, 'run', side_effect=guarded_run) as run:
                self.assertEqual(worker.shutil.which('git'), '/usr/bin/git')
                self.check(now=now)
            return run
        run = checked('standalone', worker.DAY_MS)
        self.assertEqual(run.call_args_list[0].args[0], ['xcode-select', '-p'])
        self.assertEqual(run.call_count, 1)
        self.assertEqual(self.cache()['status'], 'skipped')
        self.assertEqual(self.cache()['reason'], 'developer_tools_missing')
        self.assertIsNone(self.cache()['remote_version'])
        self.assertNotIn('git', self.calls)
        # Herdr's check is as it was before standalone installs: it never
        # runs xcode-select, and asks Git as always.
        run = checked('herdr', 2 * worker.DAY_MS)
        run.assert_not_called()
        self.assertEqual(self.calls, ['registration', 'git', 'manifest'])
        self.assertNotIn('status', self.cache())
        self.assertNotIn('reason', self.cache())
        self.assertEqual(self.cache()['remote_version'], self.remote)

    def test_a_missing_or_tampered_source_runs_no_git_and_records_a_bounded_error(self):
        self.standalone()
        metadata = self.state / 'runtime/source.json'
        archive = dict(kind='archive', origin='/tmp/archive', version='1.7.9')
        for label, contents in [
                ('missing', None),
                ('not JSON', b'{'),
                ('a list', b'[]'),
                ('a string', b'"archive"'),
                # Valid JSON one byte past the limit, so only the limit refuses it.
                ('oversized', json.dumps(dict(archive, pad='x' * (worker.LIMIT + 1 - len(json.dumps(dict(archive, pad='')))))).encode()),
                ('no version', json.dumps(dict(kind='archive', origin='/tmp/archive')).encode()),
                ('a short version', json.dumps(dict(archive, version='1.7')).encode()),
                ('a numeric version', json.dumps(dict(archive, version=179)).encode()),
                ('a list version', json.dumps(dict(archive, version=['1.7.9'])).encode()),
                ('a version with a newline', json.dumps(dict(archive, version='1.7.9\n')).encode()),
                ('a hostile version', json.dumps(dict(archive, version='1.7.9-exp.$(id)')).encode()),
                ('an unknown kind', json.dumps(dict(archive, kind='tarball')).encode()),
                ('no kind', json.dumps(dict(origin='/tmp/archive', version='1.7.9')).encode()),
                ('a null kind', json.dumps(dict(archive, kind=None)).encode()),
                ('a checkout with no origin', json.dumps(dict(kind='checkout', version='1.7.9')).encode()),
                ('a checkout with a numeric origin', json.dumps(dict(kind='checkout', origin=7, version='1.7.9')).encode())]:
            with self.subTest(label):
                metadata.unlink(missing_ok=True)
                if contents is not None:
                    metadata.write_bytes(contents)
                self.now = getattr(self, 'now', 0) + worker.DAY_MS
                with patch.object(worker, 'run') as run:
                    self.check(now=self.now)
                run.assert_not_called()
                self.assertEqual(self.calls, [], 'a standalone source fell back to Herdr')
                cache = self.cache()
                self.assertIsNone(cache['source'])
                self.assertIsNone(cache['remote_version'])
                self.assertTrue(cache['last_error'])
                self.assertLessEqual(len(cache['last_error']), 512)
                self.assertTrue(cache['last_error'].isprintable())

    def test_only_a_standalone_owner_reads_its_source_and_any_other_is_checked_as_herdr(self):
        self.standalone()
        owner = self.state / 'runtime/owner'
        for label, write in [
                ('Herdr', lambda: owner.write_text('Herdr\n')),
                ('empty', lambda: owner.write_text('')),
                ('standalone-ish', lambda: owner.write_text('standalone-ish\n')),
                ('not UTF-8', lambda: owner.write_bytes(b'\xffstandalone')),
                ('a directory', lambda: owner.mkdir()),
                ('herdr', lambda: owner.write_text('herdr\n')),
                ('missing', lambda: None)]:
            with self.subTest(label):
                if owner.is_dir():
                    owner.rmdir()
                owner.unlink(missing_ok=True)
                write()
                self.calls.clear()
                (self.state / 'update-check.json').unlink(missing_ok=True)
                self.now = getattr(self, 'now', 0) + worker.DAY_MS
                with patch.object(worker, 'latest_tag', side_effect=AssertionError('the tag listing ran')), \
                        patch.object(worker, 'developer_tools_missing', side_effect=AssertionError('xcode-select ran')):
                    self.check(now=self.now)
                self.assertEqual(self.calls, ['registration', 'git', 'manifest'])
                self.assertEqual(self.cache()['source']['requested_ref'], 'main')
                self.assertEqual(self.cache()['remote_version'], self.remote)
                self.assertIsNone(self.cache()['last_error'])
        if owner.is_dir():
            owner.rmdir()
        owner.write_text(' standalone \n')
        self.calls.clear()
        with patch.object(worker, 'run', return_value=subprocess.CompletedProcess([], 0, 'a' * 40 + '\trefs/tags/v2.0.0\n', '')):
            self.check(now=self.now + worker.DAY_MS)
        self.assertEqual(self.calls, [])
        self.assertEqual(self.cache()['source']['kind'], 'archive')
        self.assertEqual(self.cache()['remote_version'], '2.0.0')

    def test_other_git_binaries_and_working_developer_tools_are_allowed(self):
        with patch.object(worker.sys, 'platform', 'darwin'), \
                patch.object(worker.shutil, 'which', return_value='/opt/homebrew/bin/git'), \
                patch.object(worker, 'run') as run:
            self.assertFalse(worker.developer_tools_missing())
            run.assert_not_called()
        with patch.object(worker.sys, 'platform', 'darwin'), \
                patch.object(worker.shutil, 'which', return_value='/usr/bin/git'), \
                patch.object(worker, 'run') as run:
            self.assertFalse(worker.developer_tools_missing())
            run.assert_called_once_with(['xcode-select', '-p'], timeout=5)

    def test_herdr_registration_and_git_scripts_keep_the_existing_update_flow(self):
        tools = self.root / 'tools'
        tools.mkdir()
        herdr = tools / 'herdr'
        registration = self.root / 'registration.json'
        registration.write_text(json.dumps(self.value))
        calls = self.root / 'herdr-args'
        herdr.write_text('#!/bin/sh\nprintf "%s\\n" "$@" > "' + str(calls)
                         + '"\ncat "' + str(registration) + '"\n')
        herdr.chmod(0o700)
        git = tools / 'git'
        git.write_text('#!/bin/sh\nprintf "%s\\n" "' + 'b' * 40 + '\trefs/heads/main"\n')
        git.chmod(0o700)
        with patch.dict(os.environ, PATH=str(tools) + ':/usr/bin:/bin', HERDR_BIN_PATH=str(herdr)):
            worker.check(self.state, self.settings, now=100_000, lookup_version=self.lookup_version)
        self.assertEqual(calls.read_text().splitlines(), ['plugin', 'list', '--plugin', 'namedrop', '--json'])
        self.assertEqual(self.calls, ['manifest'])
        self.assertEqual(self.cache()['remote_version'], self.remote)
        self.assertEqual(self.cache()['source']['requested_ref'], 'main')
        self.assertNotIn('kind', self.cache()['source'])

    def test_experimental_versions_are_cached_and_reused(self):
        self.remote = '1.5.0-exp.2'
        self.check()
        self.assertEqual(self.cache()['remote_version'], '1.5.0-exp.2')
        self.check(100_000 + worker.DAY_MS)
        self.assertEqual(self.calls, ['registration', 'git', 'manifest', 'registration', 'git'])
        self.assertIsNone(self.cache()['last_error'])

    def test_daily_throttle_and_only_changed_commits_fetch_the_manifest(self):
        self.check()
        self.assertEqual(self.cache()['remote_version'], '1.10.0')
        self.check(100_001)
        self.assertEqual(self.calls, ['registration', 'git', 'manifest'])
        self.check(100_000 + worker.DAY_MS)
        self.assertEqual(self.calls, ['registration', 'git', 'manifest', 'registration', 'git'])
        self.commit = 'c' * 40
        self.check(100_000 + 2 * worker.DAY_MS)
        self.assertEqual(self.calls[-3:], ['registration', 'git', 'manifest'])
        self.assertEqual(self.cache()['remote_commit'], self.commit)

    def test_linked_archive_pinned_and_invalid_registration_never_use_network(self):
        original = copy.deepcopy(self.value)
        for kind, ref in [('local', 'main'), ('archive', 'main'), ('github', 'a' * 40), ('github', 'a' * 64)]:
            with self.subTest(kind=kind, ref=ref):
                self.value = copy.deepcopy(original)
                self.value['result']['plugins'][0]['source'].update(kind=kind, requested_ref=ref)
                self.check(len(self.calls) * worker.DAY_MS)
                self.assertIsNone(self.cache()['source'])
                self.assertNotIn('git', self.calls)
                self.assertNotIn('manifest', self.calls)
        for source in [None, {}, {'kind': 'github'}, {'kind': 'github', 'owner': '../host'}]:
            self.value = copy.deepcopy(original)
            self.value['result']['plugins'][0]['source'] = source
            self.check(len(self.calls) * worker.DAY_MS)
            self.assertIsNone(self.cache()['remote_version'])
            self.assertNotIn('git', self.calls)

    def test_opt_out_and_invalid_settings_do_not_even_inspect_registration(self):
        for settings in ['{"check_for_updates":false}', '{', '[]', '{"check_for_updates":"false"}', '{"check_for_update":false}']:
            self.settings.write_text(settings)
            self.check()
            self.assertEqual(self.calls, [])
            self.assertFalse(self.state.exists())
        self.settings.write_text('{"check_for_updates":true}')
        self.check()
        self.assertIn('git', self.calls)

    def test_worker_lock_deduplicates_concurrent_checks(self):
        self.state.mkdir()
        with (self.state / 'update-check.lock').open('a') as lock:
            fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            self.check()
            self.assertEqual(self.calls, [])
        self.check()
        self.assertEqual(self.calls, ['registration', 'git', 'manifest'])

    def test_offline_error_is_cached_throttled_and_keeps_last_success(self):
        self.check()
        def offline(source):
            raise subprocess.TimeoutExpired('git', 8)
        worker.check(self.state, self.settings, 100_000 + worker.DAY_MS,
                     self.registration, offline, self.lookup_version)
        cache = self.cache()
        self.assertIn('timed out', cache['last_error'])
        self.assertEqual(cache['last_success_at'], 100_000)
        self.assertEqual(cache['remote_version'], '1.10.0')
        calls = self.calls[:]
        self.check(100_001 + worker.DAY_MS)
        self.assertEqual(self.calls, calls)

    def test_corrupt_cache_recovers_and_hostile_versions_are_never_saved(self):
        self.state.mkdir()
        (self.state / 'update-check.json').write_text('{')
        self.check()
        self.assertEqual(self.cache()['remote_version'], self.remote)
        for index, value in enumerate([
                '1.2.3\nmalice', '1.2.3"', '\x1b[31m1.2.3', '9' * 10000, '01.2.3',
                '1.2.3-beta', '1.2.3+build', '1.5.0-exp.', '1.5.0-exp.01',
                '1.5.0-exp.18446744073709551616', '1.5.0-exp.1.extra', '1.5.0-exp.$(id)']):
            self.commit = str(index) * 40
            self.remote = value
            self.check(100_000 + (index + 1) * worker.DAY_MS)
            self.assertNotEqual(self.cache()['remote_version'], value)
            self.assertEqual(self.cache()['remote_commit'], 'b' * 40)
            self.assertIsNotNone(self.cache()['last_error'])

    def test_manifest_is_bounded_and_only_numeric_metadata_is_used(self):
        source = worker.source_from_registration(self.value)
        class Response:
            def __enter__(self):
                return self
            def __exit__(self, *args):
                pass
            def read(self, size):
                return self.data[:size]
        response = Response()
        for data, expected in [(b'id = "namedrop"\nversion = "1.10.0"\n[[build]]\ncommand = ["malice"]', '1.10.0'),
                               (b'version = "1.5.0-exp.2"', '1.5.0-exp.2'),
                               (b'version = "1.2.3\\nmalice"', None),
                               (b'version = "1.2.3"\nversion = "1.3.0"', None),
                               (b'x' * (worker.LIMIT + 1), None)]:
            response.data = data
            with patch.object(worker.urllib.request, 'urlopen', return_value=response) as fetch:
                if expected:
                    self.assertEqual(worker.remote_version(source, 'b' * 40), expected)
                else:
                    with self.assertRaises(ValueError):
                        worker.remote_version(source, 'b' * 40)
                self.assertIn('/' + 'b' * 40 + '/herdr-plugin.toml', fetch.call_args.args[0])
                self.assertEqual(fetch.call_args.kwargs['timeout'], 8)

    def test_git_ref_resolution_uses_numeric_commit_and_peels_annotated_tags(self):
        source = worker.source_from_registration(self.value)
        source['requested_ref'] = 'v1.2.0'
        with patch.object(worker, 'run', return_value=subprocess.CompletedProcess([], 0,
                          'a' * 40 + '\trefs/tags/v1.2.0\n' + 'b' * 40 + '\trefs/tags/v1.2.0^{}\n')) as git:
            self.assertEqual(worker.remote_commit(source), 'b' * 40)
            self.assertEqual(git.call_args.kwargs['timeout'], 8)
            self.assertIn('ls-remote', git.call_args.args[0])


if __name__ == '__main__':
    unittest.main()
