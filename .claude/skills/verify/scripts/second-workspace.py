#!/usr/bin/env python3
"""Add a second Namedrop workspace, `beta`, to a profile kept by `./testbed test --keep`.

Usage: second-workspace.py PROFILE_NAME   (e.g. test-link-<checkout name>)

Creates PROFILE/beta, its own Git repository, opens it as the Herdr workspace labelled
`beta`, activates Namedrop there, names a real Claude Code `@lead` and a real Codex
`@codex`, and writes the workspace and pane ids to PROFILE/beta.json. A rerun reuses
whatever already exists and starts only what is missing.
"""

import json
import re
import shlex
import subprocess
import sys
import time
from pathlib import Path

profile = Path.home() / '.namedrop-testbed/profiles' / sys.argv[1]
env_line = re.search(r'env -i (.*) /bin/bash --noprofile', (profile / 'env.sh').read_text()).group(1)
base_env = ['env', '-i', *shlex.split(env_line)]
saved = json.loads((profile / 'state.json').read_text())
setup_sh = Path(saved['plugin_root']) / 'scripts/setup.sh'
repo = profile / 'beta'
# Under `--codex no-daemon` a plain `codex` would start the shared app-server and add ND-SESSION-003.
AGENTS = {'lead': 'claude', 'codex': 'codex --no-daemon'}


def run(*argv, cwd=None):
    result = subprocess.run([*base_env, *argv], capture_output=True, text=True, cwd=cwd, timeout=180)
    if result.returncode:
        sys.exit(f'{shlex.join(argv)} exited {result.returncode}: {result.stdout}{result.stderr}')
    return result.stdout


def herdr(*argv):
    return json.loads(run('herdr', *argv))['result']


def panes(workspace):
    return {pane['pane_id']: pane for pane in herdr('pane', 'list', '--workspace', workspace)['panes']}


def screen(pane):
    return run('herdr', 'pane', 'read', pane, '--source', 'visible')


def wait_for(pane, pattern, seconds=90):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        if re.search(pattern, screen(pane), re.M):
            return screen(pane)
        time.sleep(1)
    sys.exit(f'{pane}: no {pattern!r} within {seconds}s:\n{screen(pane)[-1500:]}')


def in_shell(pane, command, label):
    log, done = profile / 'logs' / f'{label}.log', profile / 'logs' / f'{label}.exit'
    done.unlink(missing_ok=True)
    run('herdr', 'pane', 'run', pane,
        f'{{ {command}; }} > {shlex.quote(str(log))} 2>&1; printf %s $? > {shlex.quote(str(done))}')
    deadline = time.monotonic() + 120
    while not done.exists() and time.monotonic() < deadline:
        time.sleep(0.2)
    if not done.exists() or done.read_text() != '0':
        sys.exit(f'{label} failed: {log.read_text() if log.exists() else "no output"}')


if not (repo / '.git').exists():
    repo.mkdir(exist_ok=True)
    for name in ('AGENTS.md', 'CLAUDE.md', 'README.md'):
        (repo / name).write_bytes((profile / 'repo' / name).read_bytes())
    for argv in (['git', 'init', '-q'], ['git', 'add', '.'],
                 ['git', '-c', 'user.name=testbed', '-c', 'user.email=testbed@localhost',
                  'commit', '-qm', 'beta seed']):
        run(*argv, cwd=str(repo))

claude_json = profile / 'home/.claude.json'
claude_state = json.loads(claude_json.read_text())
project = claude_state.setdefault('projects', {}).setdefault(str(repo), {})
if not project.get('hasTrustDialogAccepted'):
    project['hasTrustDialogAccepted'] = True
    claude_json.write_text(json.dumps(claude_state, indent=2) + '\n')
codex_config = profile / 'codex/config.toml'
toml = codex_config.read_text()
if f'[projects."{repo}"]' not in toml:
    codex_config.write_text(f'{toml.rstrip()}\n[projects."{repo}"]\ntrust_level = "trusted"\n')

workspace = next((w['workspace_id'] for w in herdr('workspace', 'list')['workspaces']
                  if w.get('label') == 'beta'), None)
if workspace is None:
    workspace = herdr('workspace', 'create', '--cwd', str(repo), '--label', 'beta',
                      '--no-focus')['workspace']['workspace_id']
live = panes(workspace)
ids = {pane['title'][1:]: pane_id for pane_id, pane in live.items() if pane.get('title') in ('@lead', '@codex')}
ids['shell'] = next(pane_id for pane_id in live if pane_id not in ids.values())
in_shell(ids['shell'], f'sh {shlex.quote(str(setup_sh))} activate', 'beta-activate')

for alias, parent, direction in (('lead', 'shell', 'right'), ('codex', 'lead', 'down')):
    if alias not in ids:
        ids[alias] = herdr('pane', 'split', ids[parent], '--direction', direction,
                           '--cwd', str(repo), '--no-focus')['pane']['pane_id']
live = panes(workspace)
for alias, command in AGENTS.items():
    pane = ids[alias]
    if live[pane].get('agent') is None:
        in_shell(pane, f'namedrop name @{alias} --human --type agent', f'beta-name-{alias}')
        run('herdr', 'pane', 'run', pane, command)

if 'Hooks need review' in wait_for(ids['codex'], r'Hooks need review|^  \S.* · [/~]'):
    run('herdr', 'agent', 'send-keys', ids['codex'], 'down', 'enter')
    wait_for(ids['codex'], r'^  \S.* · [/~]')
wait_for(ids['lead'], '^❯')

for alias in AGENTS:
    if not panes(workspace)[ids[alias]].get('agent_session'):
        subprocess.run([*base_env, 'herdr', 'agent', 'prompt', ids[alias], 'hi',
                        '--wait', '--until', 'idle', '--until', 'done', '--timeout', '180000'],
                       capture_output=True, timeout=200)
(profile / 'beta.json').write_text(json.dumps({'workspace': workspace, 'panes': ids}, indent=2) + '\n')
print(json.dumps({'workspace': workspace, 'panes': ids}))
