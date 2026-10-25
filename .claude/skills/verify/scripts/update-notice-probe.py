#!/usr/bin/env python3
"""Drive Namedrop's update notice (1.3.0+) in a profile kept by `./testbed test --keep`.

Usage: update-notice-probe.py PROFILE_NAME   (e.g. test-link-<checkout name>)

Seeds an "update available" cache, then checks Doctor, a new Claude Code session, a
same-day and a next-day Codex session, invalid settings and the opt-out. Restores the
cache and removes the settings file at the end. Exit 0 when every check passes.
"""

import glob
import json
import re
import shlex
import subprocess
import sys
import time
from pathlib import Path

REMOTE = '9.9.9'
LINE = re.compile(rf'Namedrop {re.escape(REMOTE)} is available \(you have [0-9.]+\)')
results = []


def check(what, ok, detail=''):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {what}{': ' + detail if detail else ''}", flush=True)


profile = Path.home() / '.namedrop-testbed/profiles' / sys.argv[1]
state = profile / 'state/herdr/plugins/namedrop'
env_line = re.search(r'env -i (.*) /bin/bash --noprofile', (profile / 'env.sh').read_text()).group(1)
base_env = ['env', '-i', *shlex.split(env_line)]


def run(*argv, timeout=180):
    return subprocess.run([*base_env, *argv], capture_output=True, text=True, timeout=timeout)


def herdr(*argv, timeout=180):
    return run('herdr', *argv, timeout=timeout)


def pane_text(pane, lines=200):
    out = herdr('pane', 'read', pane, '--source', 'recent', '--lines', str(lines)).stdout
    return re.sub(r'\s+', ' ', out)


saved = json.loads((profile / 'state.json').read_text())
claude, codex, workspace = saved['panes']['claude'], saved['panes']['codex'], saved['workspace']


def doctor(*mode):
    return run(str(state / 'runtime/namedrop'), '--plugin-state', str(state),
               '--herdr-workspace', workspace, 'doctor', *mode).stdout


def update_issues():
    data = json.loads(doctor('--json'))['data']
    return data['status'], [i for i in data['issues'] if i.get('code') == 'ND-UPDATE-001']


def new_claude_session():
    herdr('agent', 'prompt', claude, '/exit')
    herdr('pane', 'wait-output', claude, '--regex', r'\$ *$', '--timeout', '20000')
    herdr('pane', 'run', claude, 'claude')
    herdr('pane', 'wait-output', claude, '--regex', '/effort', '--timeout', '60000')
    time.sleep(4)
    return pane_text(claude, 40)


def new_codex_turn():
    herdr('agent', 'prompt', codex, '/new')
    time.sleep(3)
    herdr('agent', 'send-keys', codex, 'enter')
    time.sleep(3)
    herdr('agent', 'prompt', codex, 'Reply with only the word ok.', '--wait', '--until', 'idle',
          '--until', 'done', '--timeout', '120000')
    return pane_text(codex)


def latest_claude_transcript():
    files = glob.glob(str(profile / 'home/.claude/projects/*/*.jsonl'))
    entries = [json.loads(line) for line in open(max(files, key=lambda f: Path(f).stat().st_mtime))]
    shown, context = [], []
    for entry in entries:
        attachment = entry.get('attachment') or {}
        if attachment.get('type') == 'hook_system_message':
            shown.append(attachment['content'])
        if attachment.get('type') == 'hook_success' and attachment.get('hookEvent') == 'SessionStart':
            output = json.loads(attachment['stdout'] or '{}')
            context.append(output.get('hookSpecificOutput', {}).get('additionalContext', ''))
    return shown, context


cache, notice = state / 'update-check.json', state / 'update-notice.json'
settings = Path(herdr('plugin', 'config-dir', 'namedrop').stdout.strip()) / 'settings.json'
original_cache = cache.read_bytes() if cache.exists() else None
now = int(time.time() * 1000)
try:
    cache.write_text(json.dumps({
        'schema_version': 1, 'checked_at': now, 'last_success_at': now, 'last_error': None,
        'source': {'owner': 'ericbust', 'repo': 'namedrop', 'requested_ref': 'main',
                   'resolved_commit': 'a' * 40, 'subdir': ''},
        'remote_commit': 'b' * 40, 'remote_version': REMOTE}))
    notice.unlink(missing_ok=True)

    status, issues = update_issues()
    human = doctor('--human')
    check('doctor finding', status == 'healthy' and len(issues) == 1 and issues[0]['severity'] == 'info',
          f'status {status}, ND-UPDATE-001 {[i["severity"] for i in issues]}')
    check('doctor summary kept', human.startswith('Namedrop checks passed') and REMOTE in human,
          human.splitlines()[0])

    screen = new_claude_session()
    # Claude Code writes the transcript at the session's first prompt.
    herdr('agent', 'prompt', claude, 'Reply with only the word ok.', '--wait', '--until', 'idle',
          '--until', 'done', '--timeout', '120000')
    shown, context = latest_claude_transcript()
    check('claude shows the line', bool(LINE.search(screen)), 'startup screen')
    check('claude keeps it from the agent',
          any(LINE.search(s) for s in shown) and context and not any(REMOTE in c for c in context),
          f'hook_system_message {len(shown)}, additionalContext with {REMOTE}: '
          f'{sum(REMOTE in c for c in context)}')

    screen = new_codex_turn()
    check('codex same day stays quiet', '• ok' in screen and not LINE.search(screen),
          f'turn answered {"• ok" in screen}, notice shown < 24h ago')
    notice.unlink()
    check('codex next day shows the line', bool(LINE.search(new_codex_turn())), '↳ Hook row')

    settings.parent.mkdir(parents=True, exist_ok=True)
    settings.write_text('{"check_for_updates":"no"}\n')
    data = json.loads(doctor('--json'))['data']
    invalid = any(i.get('code') == 'ND-UPDATE-002' for i in data['issues'])
    check('invalid settings', invalid and data['status'] == 'healthy',
          f"ND-UPDATE-002 {invalid}, status {data['status']}")

    settings.write_text('{"check_for_updates":false}\n')
    notice.unlink(missing_ok=True)
    status, issues = update_issues()
    screen = new_claude_session()
    check('opt-out', not issues and not LINE.search(screen) and not notice.exists(),
          f'doctor findings {len(issues)}, line on screen {bool(LINE.search(screen))}, '
          f'notice file {notice.exists()}')
finally:
    settings.unlink(missing_ok=True)
    notice.unlink(missing_ok=True)
    if original_cache is None:
        cache.unlink(missing_ok=True)
    else:
        cache.write_bytes(original_cache)

print(f"{'PASS' if all(results) else 'FAIL'} {sum(results)}/{len(results)}")
sys.exit(0 if all(results) else 1)
