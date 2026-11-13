#!/usr/bin/env python3
"""Check that a final message reaches its asker whatever names it holds, with real
agents, on a profile kept by `testbed/testbed test --keep` (Namedrop 1.6.3 and later).

Usage: final-message-mention-probe.py PROFILE_NAME [HELPER CASE]   (e.g. test-link-<checkout name>)
HELPER is claude or codex and CASE is relayed, real, opens or asker, to run that one exchange.

Each agent asks the other for a one-line answer that names an agent that isn't here, names
one that is, opens with a name, or opens with the asker's own name. The answer must arrive as the helper's reply, go only
to the asker, and leave the claim done. Exit 0 when every check passes.
"""

import glob
import json
import re
import shlex
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

ANSWER_S = 150
results = []


def check(what, ok, detail=''):
    results.append(ok)
    print(f"{'ok  ' if ok else 'FAIL'}  {what}{': ' + detail if detail else ''}", flush=True)


profile = Path.home() / '.namedrop-testbed/profiles' / sys.argv[1]
if 'claude_account' not in json.loads((profile / 'state.json').read_text()):
    sys.exit(f'{profile.name} has no accepted Claude Code account check; run '
             f'testbed/testbed up --profile {profile.name} first')
state = profile / 'state/herdr/plugins/namedrop'
db = state / 'namedrop.sqlite3'
env_line = re.search(r'env -i (.*) /bin/bash --noprofile', (profile / 'env.sh').read_text()).group(1)
base_env = ['env', '-i', *shlex.split(env_line)]
panes = json.loads((profile / 'state.json').read_text())['panes']


def herdr(*argv, timeout=180):
    return subprocess.run([*base_env, 'herdr', *argv], capture_output=True, text=True, timeout=timeout)


def rows(sql, **args):
    conn = sqlite3.connect(f'file:{db}?mode=ro', uri=True)
    try:
        return conn.execute(sql, args).fetchall()
    finally:
        conn.close()


def claim_for(attempt):
    for path in glob.glob(str(state / 'final-reply/*/claims.json')):
        for claim in json.load(open(path)):
            if claim['attempt_id'] == attempt:
                return claim
    return None


def clear_claude_input():
    """Claude Code puts a typed mention it blocked back in its input box, and a later
    delivery would pick that up as leftover input and keep explicit commands."""
    herdr('agent', 'send-keys', panes['claude'], 'esc')
    herdr('agent', 'send-keys', panes['claude'], 'esc')
    time.sleep(1)
    screen = herdr('pane', 'read', panes['claude'], '--source', 'visible').stdout
    if 'Restore the code and/or conversation' in screen:
        herdr('agent', 'send-keys', panes['claude'], 'esc')


def clear_codex_input():
    """Codex can keep a typed mention in its composer too, and the next typed mention
    then merges with it into one prompt that no hook reads as a mention."""
    screen = herdr('pane', 'read', panes['codex'], '--source', 'visible').stdout
    composer = [row for row in screen.splitlines() if row.startswith('\u203a ')]
    if composer and 'Ask Codex to do anything' not in composer[-1]:
        herdr('agent', 'send-keys', panes['codex'], 'c-c')
        time.sleep(1)


def wait_until_quiet():
    """An agent reads idle between the turn that ended and a reply still to dispatch, and
    text typed then waits in its composer."""
    deadline = time.time() + 60
    while time.time() < deadline and rows(
            """select 1 from mailbox_deliveries
               where state not in ('settled', 'failed', 'cancelled', 'held')"""):
        time.sleep(1)
    for pane in (panes['claude'], panes['codex']):
        herdr('agent', 'wait', pane, '--until', 'idle', '--until', 'done', '--timeout', '120000')


def exchange(asker, helper, token, line):
    """One typed ask and whatever came back. A typed mention keeps its body as written,
    so the ask can quote the names it wants repeated."""
    wait_until_quiet()
    herdr('agent', 'prompt', panes[asker],
          f'@{helper} Run no tools and no commands. Your whole final message is this line, '
          f'copied character for character with no backslash and no backticks: {line}',
          '--wait', '--until', 'idle', '--until', 'done', '--timeout', '120000')
    # Clearing before the ask has a row would wipe an ask still waiting to submit.
    deadline = time.time() + 20
    while time.time() < deadline and not rows(
            "select 1 from collaboration_messages where body like :t", t=f'%{token}%'):
        time.sleep(1)
    clear_claude_input() if asker == 'claude' else clear_codex_input()
    deadline = time.time() + ANSWER_S
    while time.time() < deadline:
        sent = rows("""select m.message_id, a.attempt_id from collaboration_messages m
                       join mailbox_deliveries d using (session_socket_id, worktree_id, message_id)
                       left join delivery_attempts a using (session_socket_id, worktree_id, delivery_id, address_id)
                       where m.body like :t and m.author_alias_snapshot = :a
                       order by a.created_at_ms desc""", t=f'%{token}%', a=asker)
        if sent and sent[0][1]:
            claim = claim_for(sent[0][1])
            ended = claim and claim['state'] != 'open'
            if ended and claim['state'] != {'left': 'held'}:
                break
        time.sleep(2)
    else:
        return None, None, []
    message, attempt = sent[0]
    replies = rows("""select m.message_id, m.author_alias_snapshot, m.body from collaboration_messages m
                      where m.in_reply_to = :m order by m.created_at_ms""", m=message)
    recipients = rows("select alias_snapshot from message_recipients where message_id = :m",
                      m=replies[0][0]) if replies else []
    return attempt, replies, [r[0] for r in recipients]


stamp = int(time.time())
CASES = [
    ('names an agent that is not here', 'relayed', '{token} from @ghost', '{token} from @ghost'),
    # Not last in the line: Codex's composer offers a completion for a name the cursor is on,
    # and Enter then accepts that instead of submitting.
    ('names an agent that is here', 'real', '{token} for @claude today', '{token} for @claude today'),
    ('opens with a name', 'opens', '@ghost said {token}', '\\@ghost said {token}'),
    ('opens with the asker\'s name', 'asker', '@{asker} is wrong about {token}',
     '\\@{asker} is wrong about {token}'),
]
for asker, helper in (('claude', 'codex'), ('codex', 'claude')):
    for label, slug, line, stored in CASES:
        if sys.argv[2:] and sys.argv[2:] != [helper, slug]:
            continue
        token = f'{slug}{helper}{stamp}'
        attempt, replies, recipients = exchange(
            asker, helper, token, line.format(token=token, asker=asker))
        claim = claim_for(attempt) if attempt else None
        disposition = rows("select disposition_kind from delivery_dispositions where attempt_id = :a",
                           a=attempt) if attempt else []
        warnings = rows("""select summary from attention_items where source_kind = 'agent_disposition'
                           and source_id = :a and resolved_at_ms is null""", a=attempt) if attempt else []
        body = replies[0][2] if replies else None
        check(f'{helper} answering {asker}, final message {label}',
              body == stored.format(token=token, asker=asker) and recipients == [asker]
              and claim and claim['state'] == {'done': 'replied'}
              and disposition == [('reply',)] and not warnings,
              f'body {body!r}; to {recipients}; claim {claim and claim["state"]}; '
              f'disposition {disposition}; warnings {warnings}')

print(f"{'PASS' if all(results) else 'FAIL'} {sum(results)}/{len(results)}")
sys.exit(0 if all(results) else 1)
