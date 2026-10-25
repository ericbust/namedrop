#!/usr/bin/env python3
"""Drive Namedrop's update notice (1.3.0+) in a profile kept by `./testbed test --keep`.

Usage: update-notice-probe.py PROFILE_NAME   (e.g. test-link-<checkout name>)

Seeds an "update available" cache, then checks Doctor, a new Claude Code session, a
same-day and a next-day Codex session, invalid settings and the opt-out. Restores the
cache and the settings file at the end. Exit 0 when every check passes.
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


def update_issues(code='ND-UPDATE-001'):
    data = json.loads(doctor('--json'))['data']
    blocking = [i['code'] for i in data['issues'] if i.get('severity') != 'info']
    return data['status'], [i for i in data['issues'] if i.get('code') == code], blocking


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
    answered = herdr('agent', 'prompt', codex, 'Reply with only the word ok.', '--wait', '--until',
                     'idle', '--until', 'done', '--timeout', '120000').returncode == 0
    return pane_text(codex), answered


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
original_settings = settings.read_bytes() if settings.exists() else None
now = int(time.time() * 1000)
try:
    cache.write_text(json.dumps({
        'schema_version': 1, 'checked_at': now, 'last_success_at': now, 'last_error': None,
        'source': {'owner': 'ericbust', 'repo': 'namedrop', 'requested_ref': 'main',
                   'resolved_commit': 'a' * 40, 'subdir': ''},
        'remote_commit': 'b' * 40, 'remote_version': REMOTE}))
    notice.unlink(missing_ok=True)

    settings.unlink(missing_ok=True)
    status, issues, blocking = update_issues()
    human = doctor('--human')
    check('doctor finding', status == 'healthy' and len(issues) == 1 and issues[0]['severity'] == 'info',
          f'status {status}, ND-UPDATE-001 {[i["severity"] for i in issues]}, not info {blocking}')
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

    screen, answered = new_codex_turn()
    check('codex same day stays quiet', answered and not LINE.search(screen),
          f'turn answered {answered}, notice shown < 24h ago')
    # A marker a day and a millisecond old: the once-a-day window has passed.
    notice.write_text(json.dumps({'version': REMOTE, 'shown_at': now - 86_400_001}))
    screen, answered = new_codex_turn()
    check('codex next day shows the line', answered and bool(LINE.search(screen)),
          f'turn answered {answered}, ↳ Hook row')

    settings.parent.mkdir(parents=True, exist_ok=True)
    settings.write_text('{"check_for_updates":"no"}\n')
    status, invalid, blocking = update_issues('ND-UPDATE-002')
    check('invalid settings', status == 'healthy' and [i['severity'] for i in invalid] == ['info'],
          f'ND-UPDATE-002 {[i["severity"] for i in invalid]}, status {status}, not info {blocking}')

    settings.write_text('{"check_for_updates":false}\n')
    notice.unlink(missing_ok=True)
    _, issues, _ = update_issues()
    _, invalid, _ = update_issues('ND-UPDATE-002')
    screen = new_claude_session()
    check('opt-out', not issues and not invalid and not LINE.search(screen) and not notice.exists(),
          f'doctor findings {len(issues) + len(invalid)}, line on screen {bool(LINE.search(screen))}, '
          f'notice file {notice.exists()}')
finally:
    if original_settings is None:
        settings.unlink(missing_ok=True)
    else:
        settings.write_bytes(original_settings)
    notice.unlink(missing_ok=True)
    if original_cache is None:
        cache.unlink(missing_ok=True)
    else:
        cache.write_bytes(original_cache)

print(f"{'PASS' if all(results) else 'FAIL'} {sum(results)}/{len(results)}")
sys.exit(0 if all(results) else 1)
