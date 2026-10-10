"""Codex setup decisions, subprocess failures, and cache verification."""
import copy
import json
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / 'scripts'))
import runtime


class CodexActivation(unittest.TestCase):
    def setUp(self):
        self.package = "/tmp/home with spaces/.agents/skills/namedrop"
        self.payload = {'data': {
            'integration_packages': {'codex': {'installed': True, 'path': self.package}},
            'agent_integration_hook_activation': {'codex': {
                'cache_current': False, 'steps': [
                    f"codex plugin marketplace add '{self.package}' --json",
                    'codex plugin add namedrop@namedrop --json',
                    'Start a new session and approve /hooks.']}}}}
        self.verified = copy.deepcopy(self.payload)
        self.verified['data']['agent_integration_hook_activation']['codex'].update(
            cache_current=True, steps=['Start a new session and approve /hooks.'])

    def install(self, payload=None, verified=None, result=None):
        with patch.object(runtime.shutil, 'which', return_value='/bin/codex'), \
             patch.object(runtime.subprocess, 'run', return_value=result or
                          subprocess.CompletedProcess([], 0, '{}', '')) as run:
            value, code = runtime.activate_codex_plugin(
                payload or self.payload, lambda: verified or self.verified)
        return value, code, run

    def test_first_install_verifies_and_preserves_literal_path(self):
        value, code, run = self.install()
        self.assertEqual(code, 0)
        self.assertEqual(value, self.verified)
        self.assertEqual(run.call_args_list[0].args[0],
                         ['/bin/codex', 'plugin', 'marketplace', 'add', self.package, '--json'])
        self.assertEqual(run.call_count, 2)
        for call in run.call_args_list:
            self.assertEqual(call.kwargs['stdin'], subprocess.DEVNULL)
            self.assertLessEqual(call.kwargs['timeout'], 30)

    def test_current_cache_does_no_work(self):
        _, code, run = self.install(self.verified)
        self.assertEqual(code, 0)
        run.assert_not_called()

    def test_upgrade_only_refreshes_plugin(self):
        self.payload['data']['agent_integration_hook_activation']['codex']['steps'].pop(0)
        _, code, run = self.install()
        self.assertEqual(code, 0)
        self.assertEqual(run.call_count, 1)
        self.assertEqual(run.call_args.args[0],
                         ['/bin/codex', 'plugin', 'add', 'namedrop@namedrop', '--json'])

    def test_conflict_is_preserved(self):
        self.payload['data']['agent_integration_hook_activation']['codex']['steps'].insert(
            0, 'codex plugin marketplace remove namedrop --json')
        value, code, run = self.install()
        self.assertEqual(code, 1)
        run.assert_not_called()
        self.assertIn('conflicts', json.dumps(value))

    def test_missing_cli_allows_other_runtime_setup(self):
        with patch.object(runtime.shutil, 'which', return_value=None), \
             patch.object(runtime.subprocess, 'run') as run:
            value, code = runtime.activate_codex_plugin(self.payload, lambda: self.fail())
        self.assertEqual(code, 0)
        run.assert_not_called()
        self.assertIn('not on PATH', json.dumps(value))

    def test_failed_command_stops_and_keeps_repair_steps(self):
        value, code, run = self.install(result=subprocess.CompletedProcess([], 1, '', 'CLI failure'))
        self.assertEqual(code, 1)
        self.assertEqual(run.call_count, 1)
        self.assertIn('CLI failure', json.dumps(value))
        self.assertIn('codex plugin add namedrop@namedrop --json', json.dumps(value))

    def test_successful_command_with_stale_cache_fails(self):
        _, code, _ = self.install(verified=copy.deepcopy(self.payload))
        self.assertEqual(code, 1)

    def test_timeout_reports_failure_without_refresh(self):
        with patch.object(runtime.shutil, 'which', return_value='/bin/codex'), \
             patch.object(runtime.subprocess, 'run', side_effect=subprocess.TimeoutExpired('codex', 30)):
            value, code = runtime.activate_codex_plugin(self.payload, lambda: self.fail())
        self.assertEqual(code, 1)
        self.assertIn('timed out', json.dumps(value))

    def test_refresh_failure_retains_diagnostics(self):
        with patch.object(runtime.shutil, 'which', return_value='/bin/codex'), \
             patch.object(runtime.subprocess, 'run', return_value=subprocess.CompletedProcess([], 0, '', '')):
            def refresh():
                raise subprocess.CalledProcessError(1, ['namedrop', 'activate'])
            value, code = runtime.activate_codex_plugin(self.payload, refresh)
        self.assertEqual(code, 1)
        self.assertEqual(value['data']['agent_integration_hook_activation']['codex']['status'], 'setup_failed')


if __name__ == '__main__':
    unittest.main()
