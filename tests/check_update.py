"""Behavioral tests for the detached worker; run by cargo test."""

import copy
import fcntl
import importlib.util
import json
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
        for index, value in enumerate(['1.2.3\nmalice', '1.2.3"', '\x1b[31m1.2.3', '9' * 10000, '01.2.3', '1.2.3-beta', '1.2.3+build']):
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
