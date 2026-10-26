#!/usr/bin/env python3
"""Type a leading mention into the Claude Code and Codex panes of a profile kept by
`testbed/testbed test --keep`, the way a person does (Namedrop 1.3.3 and later).

Usage: typed-mention-probe.py PROFILE_NAME   (e.g. test-link-<checkout name>)

Codex: the mention gets a delivery attempt within seconds and its reply settles.
Claude: the mention is sent once and replied to once, with no repeat after the reply
lands behind the blocked prompt Claude Code puts back in its input box. Leaves Claude's
input box empty. Exit 0 when both checks pass.
"""

import json
import re
import shlex
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

SETTLE_S, WATCH_S, ATTEMPT_S = 150, 45, 15
results = []


def check(what, ok, detail=''):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {what}{': ' + detail if detail else ''}", flush=True)


profile = Path.home() / '.namedrop-testbed/profiles' / sys.argv[1]
db = profile / 'state/herdr/plugins/namedrop/namedrop.sqlite3'
env_line = re.search(r'env -i (.*) /bin/bash --noprofile', (profile / 'env.sh').read_text()).group(1)
base_env = ['env', '-i', *shlex.split(env_line)]
saved = json.loads((profile / 'state.json').read_text())
claude, codex = saved['panes']['claude'], saved['panes']['codex']


def herdr(*argv, timeout=180):
    return subprocess.run([*base_env, 'herdr', *argv], capture_output=True, text=True, timeout=timeout)


def prompt(pane, text):
    for agent in (claude, codex):
        herdr('agent', 'wait', agent, '--until', 'idle', '--until', 'done', '--timeout', '120000')
    start = time.time()
    herdr('agent', 'prompt', pane, text, '--wait', '--until', 'idle', '--until', 'done',
          '--timeout', '120000')
    return start, time.time()


def clear_claude_input():
    herdr('agent', 'send-keys', claude, 'esc')
    herdr('agent', 'send-keys', claude, 'esc')
    time.sleep(1)
    # Esc twice on an empty input box opens Claude Code's Rewind menu instead.
    screen = herdr('pane', 'read', claude, '--source', 'visible').stdout
    if 'Restore the code and/or conversation' in screen:
        herdr('agent', 'send-keys', claude, 'esc')


def thread(token):
    """Human messages carrying the token, and the replies to them, oldest first."""
    conn = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
    rows = conn.execute("""
        select m.message_id, m.author_alias_snapshot, m.author_kind, m.body, m.in_reply_to,
               m.created_at_ms,
               (select group_concat(d.state) from mailbox_deliveries d
                 where d.session_socket_id = m.session_socket_id
                   and d.worktree_id = m.worktree_id and d.message_id = m.message_id),
               (select count(*) from mailbox_deliveries d join delivery_attempts a
                   on a.session_socket_id = d.session_socket_id
                  and a.worktree_id = d.worktree_id and a.delivery_id = d.delivery_id
                 where d.session_socket_id = m.session_socket_id
                   and d.worktree_id = m.worktree_id and d.message_id = m.message_id)
          from collaboration_messages m
         where (m.author_kind = 'human' and m.body like :t)
            or m.in_reply_to in (select message_id from collaboration_messages
                                  where author_kind = 'human' and body like :t)
         order by m.created_at_ms""", {'t': f'%{token}%'}).fetchall()
    conn.close()
    keys = ('id', 'alias', 'kind', 'body', 'reply_to', 'at', 'states', 'attempts')
    rows = [dict(zip(keys, row)) for row in rows]
    return [r for r in rows if r['kind'] == 'human'], [r for r in rows if r['reply_to']]


def settled(sent, replies):
    return bool(sent and replies) and all(r['states'] and set(r['states'].split(',')) == {'settled'}
                                          for r in sent + replies)


def describe(sent, replies, since):
    rows = sorted(sent + replies, key=lambda r: r['at'])
    return '; '.join(f"{'reply' if r['reply_to'] else 'sent'} by {r['alias']} "
                     f"+{r['at'] / 1000 - since:.0f}s {r['body'][:40]!r} "
                     f"[{r['states'] or 'no delivery'}] attempts {r['attempts']}"
                     for r in rows) or 'no rows'


def wait_settled(token, since, limit):
    while time.time() < since + limit and not settled(*thread(token)):
        time.sleep(2)
    return settled(*thread(token))


stamp = int(time.time())
try:
    # Codex first, on a quiet workspace: any agent's activity sends a stalled delivery.
    token = f'tmcodex{stamp}'
    start, returned = prompt(codex, f'@claude reply with only the word {token}')
    while time.time() < returned + ATTEMPT_S and not any(r['attempts'] for r in thread(token)[0]):
        time.sleep(1)
    sent, _ = thread(token)
    attempted = bool(sent) and all(r['attempts'] for r in sent)
    detail, settled_once = f'within {ATTEMPT_S}s: {describe(sent, [], start)}', False
    if attempted:
        settled_once = wait_settled(token, returned, SETTLE_S)
        sent, replies = thread(token)
        detail += f'; settled within {SETTLE_S}s {settled_once}: {describe(sent, replies, start)}'
    check('codex typed mention delivered promptly, reply settles',
          settled_once and [r['alias'] for r in sent] == ['codex'], detail)

    token = f'tmclaude{stamp}'
    start, returned = prompt(claude, f'@codex reply with only the word {token}')
    settled_once = wait_settled(token, returned, SETTLE_S)
    time.sleep(WATCH_S)
    sent, replies = thread(token)
    check('claude typed mention sent once, replied once',
          settled_once and settled(sent, replies) and len(replies) == 1
          and [r['alias'] for r in sent] == ['claude'],
          f'settled within {SETTLE_S}s {settled_once}, after {WATCH_S}s more: '
          f'{describe(sent, replies, start)}')
finally:
    clear_claude_input()

print(f"{'PASS' if all(results) else 'FAIL'} {sum(results)}/{len(results)}")
sys.exit(0 if all(results) else 1)
