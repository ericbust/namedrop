"""Behavioral tests for the detached worker; run by cargo test."""

import copy
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
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

    def tearDown(self):
        self.temporary.cleanup()

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

    def standalone(self, kind='archive', origin='/tmp/archive'):
        runtime = self.state / 'runtime'
        runtime.mkdir(parents=True)
        (runtime / 'owner').write_text('standalone\n')
        (runtime / 'source.json').write_text(json.dumps(
            dict(kind=kind, origin=origin, version='1.7.9')))

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
        with patch.dict(os.environ, PATH=str(self.root) + os.pathsep + os.environ['PATH']):
            self.check()
        self.assertEqual(self.calls, [])
        self.assertEqual(self.cache()['remote_version'], '9.10.0')
        self.assertEqual(self.cache()['remote_commit'], 'd' * 40)
        self.assertIsNone(self.cache()['last_error'])
        args = log.read_text().splitlines()
        self.assertEqual(args[-4:], ['ls-remote', '--tags', 'https://github.com/ericbust/namedrop.git', 'v*'])
        self.assertEqual(self.cache()['source']['installed_version'], '1.7.9')

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
        for owner in ('standalone', 'herdr'):
            (self.state / 'runtime/owner').write_text(owner)
            with patch.object(worker.sys, 'platform', 'darwin'), \
                    patch.dict(os.environ, PATH=str(tools) + ':/usr/bin:/bin'), \
                    patch.object(worker, 'run', side_effect=guarded_run) as run:
                self.assertEqual(worker.shutil.which('git'), '/usr/bin/git')
                self.check(now=(1 if owner == 'standalone' else 2) * worker.DAY_MS)
            self.assertEqual(run.call_args_list[0].args[0], ['xcode-select', '-p'])
            self.assertEqual(run.call_count, 1)
            self.assertEqual(self.cache()['status'], 'skipped')
            self.assertEqual(self.cache()['reason'], 'developer_tools_missing')
            self.assertIsNone(self.cache()['remote_version'])
            self.assertNotIn('git', self.calls)

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
