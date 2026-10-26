#!/usr/bin/env python3
"""Type cross-workspace mentions as a person into real agents of a kept testbed profile.

Usage: cross-workspace-probe.py PROFILE_NAME   (after second-workspace.py PROFILE_NAME)

Alpha is the workspace `./testbed test --keep` left; beta is the one second-workspace.py
added. Each check types one mention, waits for every delivery to settle and every agent to
go idle, and judges the rows Namedrop wrote. Exit 0 when every check passes.
"""

import glob
import json
import random
import re
import shlex
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

results = []


def check(what, ok, detail=''):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {what}{': ' + detail if detail else ''}", flush=True)


profile = Path.home() / '.namedrop-testbed/profiles' / sys.argv[1]
env_line = re.search(r'env -i (.*) /bin/bash --noprofile', (profile / 'env.sh').read_text()).group(1)
base_env = ['env', '-i', *shlex.split(env_line)]
database = profile / 'state/herdr/plugins/namedrop/namedrop.sqlite3'
alpha, beta = json.loads((profile / 'state.json').read_text()), json.loads((profile / 'beta.json').read_text())
SPACES = {alpha['workspace']: 'alpha', beta['workspace']: 'beta'}
claude, codex = alpha['panes']['claude'], alpha['panes']['codex']
AGENT_PANES = (claude, codex, beta['panes']['lead'], beta['panes']['codex'])


def herdr(*argv, timeout=180):
    return subprocess.run([*base_env, 'herdr', *argv], capture_output=True, text=True, timeout=timeout)


def now_ms():
    return int(time.time() * 1000)


def query(sql, *args):
    connection = sqlite3.connect(f'file:{database}?mode=ro', uri=True, timeout=5)
    try:
        return connection.execute(sql, args).fetchall()
    finally:
        connection.close()


def messages(since):
    """One dict per collaboration message written after `since`, each workspace's copy apart."""
    rows = []
    for space, message, author, kind, reply, origin, body in query("""
            SELECT m.worktree_id, m.message_id, m.author_alias_snapshot, m.author_kind,
                   m.in_reply_to IS NOT NULL, b.origin_space_id, m.body
            FROM collaboration_messages m LEFT JOIN bridged_messages b USING (session_socket_id, worktree_id, message_id)
            WHERE m.created_at_ms > ? ORDER BY m.created_at_ms""", since):
        deliveries = query("""
            SELECT coalesce((SELECT alias FROM alias_claims c WHERE c.worktree_id = d.worktree_id
                             AND c.address_id = d.address_id AND c.claim_state = 'active'),
                            (SELECT '#' || e.origin_label || ' @' || e.origin_alias FROM space_envoys e
                             WHERE e.worktree_id = d.worktree_id AND e.address_id = d.address_id)),
                   d.state,
                   (SELECT count(*) FROM delivery_attempts t
                    WHERE t.worktree_id = d.worktree_id AND t.delivery_id = d.delivery_id),
                   (SELECT min(t.created_at_ms) FROM delivery_attempts t
                    WHERE t.worktree_id = d.worktree_id AND t.delivery_id = d.delivery_id),
                   (SELECT group_concat(p.disposition_kind) FROM delivery_attempts t JOIN delivery_dispositions p
                    USING (session_socket_id, worktree_id, attempt_id)
                    WHERE t.worktree_id = d.worktree_id AND t.delivery_id = d.delivery_id)
            FROM mailbox_deliveries d WHERE d.worktree_id = ? AND d.message_id = ?""", space, message)
        rows.append({'space': SPACES.get(space, space), 'author': author, 'kind': kind, 'reply': bool(reply),
                     'bridged': SPACES.get(origin, origin), 'body': body,
                     'to': [{'alias': a, 'state': s, 'attempts': n, 'first_attempt': at, 'disposition': p}
                            for a, s, n, at, p in deliveries]})
    return rows


def show(rows):
    return '; '.join(
        f"{r['space']} {r['author']}({'reply' if r['reply'] else r['kind']}"
        f"{', from ' + r['bridged'] if r['bridged'] else ''}) {r['body'][:40]!r} -> "
        f"{', '.join(f'''{d['alias']} {d['state']}/{d['attempts']} {d['disposition'] or ''}'''.strip() for d in r['to']) or 'none'}"
        for r in rows) or 'no messages'


def idle():
    states = {p['pane_id']: p.get('agent_status') for p in
              json.loads(herdr('pane', 'list').stdout)['result']['panes']}
    return all(states.get(pane) in ('idle', 'done') for pane in AGENT_PANES)


def settle(since, seconds=300):
    """Rows once every delivery is settled, every agent idle and nothing changed for 10 s."""
    deadline, last, quiet_since = time.monotonic() + seconds, None, time.monotonic()
    while time.monotonic() < deadline:
        rows = messages(since)
        if rows != last:
            last, quiet_since = rows, time.monotonic()
        elif (time.monotonic() - quiet_since >= 10 and idle()
              and all(d['state'] == 'settled' for r in rows for d in r['to'])):
            return rows
        time.sleep(1)
    return last


def type_into(pane, text):
    since = query('SELECT coalesce(max(created_at_ms), 0) FROM collaboration_messages')[0][0]
    herdr('agent', 'prompt', pane, text, '--wait', '--until', 'idle', '--until', 'done', '--timeout', '180000',
          timeout=200)
    return since


def composer(pane):
    """Text in Claude's input box. Its prompt suggestion is drawn dim and left out."""
    screen = herdr('pane', 'read', pane, '--source', 'visible', '--ansi').stdout
    plain = re.sub(r'\x1b\[[0-9;]*m', '', re.sub(r'\x1b\[2m.*?(?:\x1b\[0m|$)', '', screen, flags=re.M))
    lines = [line for line in plain.splitlines() if line.startswith('❯')]
    return lines[-1][1:].strip('  \r') if lines else ''


def hook_context(word):
    """additionalContext the hooks gave Claude for the prompt that holds `word`."""
    context = []
    for path in glob.glob(str(profile / 'home/.claude/projects/*/*.jsonl')):
        after_prompt = False
        for entry in map(json.loads, open(path)):
            content = (entry.get('message') or {}).get('content')
            if entry.get('type') == 'user' and isinstance(content, str):
                after_prompt = word in content
            attachment = entry.get('attachment') or {}
            if after_prompt and attachment.get('type') == 'hook_additional_context':
                context.extend(attachment.get('content') or [])
    return ' '.join(map(str, context))


def word():
    return random.choice(('amber', 'cobalt', 'fennel', 'juniper', 'saffron', 'tundra')) + str(random.randint(100, 999))


def where(rows, space, author=None, reply=None, to=None, bridged=...):
    return [r for r in rows if r['space'] == space and (author is None or r['author'] == author)
            and (reply is None or r['reply'] == reply) and (to is None or any(d['alias'] == to for d in r['to']))
            and (bridged is ... or r['bridged'] == bridged)]


def all_settled(rows):
    return bool(rows) and all(d['state'] == 'settled' and d['attempts'] <= 1 for r in rows for d in r['to'])


for pane in AGENT_PANES:
    herdr('agent', 'wait', pane, '--until', 'idle', '--until', 'done', '--timeout', '120000', timeout=130)

t1 = word()
since = type_into(claude, f'@lead reply with only the word {t1}')
rows, context = settle(since, 60), hook_context(t1)
sentence = re.search(r'@lead is not named in this workspace.*?Nothing was sent\.', context)
check('bare @lead stays in alpha', not rows and '#beta @lead' in context,
      f"{show(rows)}; hook context: {sentence.group(0) if sentence else context[:160]!r}")

t2 = word()
since = type_into(claude, f'#beta @lead reply with only the word {t2}')
kept = composer(claude)
rows = settle(since)
sent, copy = where(rows, 'alpha', reply=False), where(rows, 'beta', reply=False)
answer, back = where(rows, 'beta', 'lead', True), where(rows, 'alpha', reply=True, to='claude')
check('#beta @lead from claude',
      len(sent) == 1 and sent[0]['kind'] == 'human' and len(copy) == 1 and copy[0]['bridged'] == 'alpha'
      and [d['alias'] for d in copy[0]['to']] == ['lead'] and len(answer) == 1 and len(back) == 1
      and back[0]['bridged'] == 'beta' and t2 in back[0]['body'] and all_settled(rows),
      f'input box kept {kept[:50]!r}; {show(rows)}')
time.sleep(45)
again = messages(since)
check('no repeat within 45 s', again == rows, f'{len(rows)} messages before, {len(again)} after')

t3 = word()
typed = now_ms()
since = type_into(codex, f'#beta @lead reply with only the word {t3}')
rows = settle(since)
copy = where(rows, 'beta', reply=False, to='lead')
first = copy[0]['to'][0]['first_attempt'] if copy else None
lag = (first - typed) / 1000 if first else None
back = where(rows, 'alpha', reply=True, to='codex')
check('#beta @lead from codex',
      lag is not None and lag <= 15 and len(where(rows, 'beta', 'lead', True)) == 1 and len(back) == 1
      and t3 in back[0]['body'] and all_settled(rows),
      f"delivered to beta {f'{lag:.1f} s' if lag is not None else 'never'} after typing; {show(rows)}")

t4 = word()
since = type_into(claude, f'#all @codex reply with only the word {t4}')
rows = settle(since)
local, remote = where(rows, 'alpha', 'codex', True, bridged=None), where(rows, 'beta', 'codex', True)
back = where(rows, 'alpha', reply=True, to='claude')
check('#all @codex from claude',
      len(local) == 1 and len(remote) == 1 and len(back) == 2 and all(t4 in r['body'] for r in back)
      and all_settled(rows), show(rows))

if composer(claude):
    herdr('agent', 'send-keys', claude, 'esc', 'esc')
    time.sleep(1)
check('claude input box empty', not composer(claude), repr(composer(claude)))

print(f"{'PASS' if all(results) else 'FAIL'} {sum(results)}/{len(results)}")
sys.exit(0 if all(results) else 1)
