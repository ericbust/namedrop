#!/usr/bin/env python3
"""Cross-session mentions and Channels on a Rex lane, with real Claude Code and Codex.

usage: python3 -I testbed/rex-cross-session-live.py --profile rex-NAME --out DIR

Run it on a lane that `./testbed rex up --fresh` just installed. It runs step 6's palette
checks for the lane's session when nothing is named yet, so that session, A, has @claude and
@codex. It then:

1. Renames A to `alpha` and starts session B, labelled `beta`, on the same lane server: its own
   Git directory, a shell block, Claude Code as PROFILE/bin/claude and `codex --no-daemon`,
   started and first-run handled as `rex up` starts the lane's agents. B is activated from the
   palette and its agents are named @claude and @codex from the palette, the same names as A's.
2. Types cross-session mentions into the agents and judges the store and the screens: A's
   Claude Code to `#beta @codex`, B's Claude Code to `#alpha @codex`, A's Codex to
   `#all @claude`, and A's Claude Code to `#nosuch @codex`, a label no session has. Each send
   must reach only the named agent, once, as a bridged copy in the recipient's session, and
   the reply must come back to the sender in the same thread. The unknown label typed as a
   prompt must stay the person's prompt and send nothing, and Claude Code's own `send` of it
   must be refused in Rex's words.
3. Reads what the agents were shown, the roster line `Other Rex sessions:` among it, from
   their transcripts, and checks that nothing is left awaiting and Doctor is clean in both
   sessions.
4. Runs `namedrop channels --no-open` in A's shell block, opens the printed link as a person
   does, and reads the page's own routes: the page loads, both sessions and their agents show,
   and a composer post to an agent in A and one in B each get one answer in their thread. The
   Rex limits the docs record are written down, not failed on.

Every rex call names the lane's server with -S and --autostart=false, from the lane's own
environment. Claude Code runs only as the profile's token wrapper. The driver logs every
command to DIR/commands.log, writes DIR/evidence.json, prints one line per check, and leaves
the lane up for `./testbed rex down`. Unlike step 6, a failed check does not stop the run.
"""

import argparse
import copy
import importlib.util
import json
from pathlib import Path
import re
import shlex
import sqlite3
import subprocess
import sys
import time
import urllib.error

sys.dont_write_bytecode = True
HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE / "lib"))
import dailyuse  # noqa: E402
import livetest  # noqa: E402
import rex_live  # noqa: E402

_spec = importlib.util.spec_from_file_location("step6", HERE / "rex-step6-live.py")
step6 = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(step6)
Failed, wait, clock = step6.Failed, step6.wait, step6.clock

ALPHA, BETA, UNKNOWN = "alpha", "beta", "nosuch"
# A delivery prompt (src/pointer.rs): its nonce, and its sender, which a bridged copy prefixes
# with the sender's session label.
BANNER = re.compile(r"Namedrop\s+message\s+([0-9a-f]{8})\s+from\s+(?:#([\w-]+)\s+)?@([\w-]+)")
SETTLED = ("acknowledged", "settled")


class Run(step6.Run):
    """step6's Run, except a failed check is recorded and the run goes on."""

    def check(self, what, passed, detail=None):
        try:
            super().check(what, passed, detail)
        except Failed:
            pass


def lane_for(a, session, blocks):
    other = copy.copy(a)
    other.session, other.blocks = session, dict(blocks)
    return other


def store_rows(a, query, *values):
    return a.rows(query, *values)


def latest_ms(a):
    return store_rows(a, "select coalesce(max(created_at_ms), 0) from collaboration_messages")[0][0]


def messages(a, since, spaces):
    """Every collaboration message written after `since`, one per session's copy, with its
    deliveries, as the Herdr cross-workspace probe reads them."""
    rows = []
    for space, message, thread, author, kind, reply, origin, body in store_rows(a, """
            select m.worktree_id, m.message_id, m.thread_id, m.author_alias_snapshot,
                   m.author_kind, m.in_reply_to, b.origin_space_id, m.body
            from collaboration_messages m left join bridged_messages b
              using (session_socket_id, worktree_id, message_id)
            where m.created_at_ms > ? order by m.created_at_ms""", since):
        deliveries = store_rows(a, """
            select coalesce((select alias from alias_claims c where c.worktree_id = d.worktree_id
                             and c.address_id = d.address_id and c.claim_state = 'active'),
                            (select '#' || e.origin_label || ' @' || e.origin_alias
                             from space_envoys e where e.worktree_id = d.worktree_id
                             and e.address_id = d.address_id)),
                   d.state,
                   (select count(*) from delivery_attempts t where t.worktree_id = d.worktree_id
                    and t.delivery_id = d.delivery_id),
                   (select group_concat(p.disposition_kind) from delivery_attempts t
                    join delivery_dispositions p using (session_socket_id, worktree_id, attempt_id)
                    where t.worktree_id = d.worktree_id and t.delivery_id = d.delivery_id)
            from mailbox_deliveries d where d.worktree_id = ? and d.message_id = ?""",
            space, message)
        rows.append({"space": spaces.get(space, space), "message": message, "thread": thread,
                     "author": author.lstrip("@"), "kind": kind, "in_reply_to": reply,
                     "bridged": spaces.get(origin, origin), "body": body,
                     "to": [{"alias": alias, "state": state, "attempts": attempts,
                             "disposition": disposition}
                            for alias, state, attempts, disposition in deliveries]})
    return rows


def show(rows):
    return ["{} {}({}{}) {!r} -> {}".format(
        row["space"], row["author"], "reply" if row["in_reply_to"] else row["kind"],
        ", from " + row["bridged"] if row["bridged"] else "", row["body"][:60],
        ", ".join("{} {}/{}{}".format(d["alias"], d["state"], d["attempts"],
                                      " " + d["disposition"] if d["disposition"] else "")
                  for d in row["to"]) or "none") for row in rows]


def where(rows, space, author=None, reply=None, to=None, bridged=...):
    return [row for row in rows if row["space"] == space and
            (author is None or row["author"] == author) and
            (reply is None or bool(row["in_reply_to"]) == reply) and
            (to is None or any(d["alias"] == to for d in row["to"])) and
            (bridged is ... or row["bridged"] == bridged)]


def nonces(a, message):
    return {row[0] for row in store_rows(a, """
        select s.pointer_nonce from delivery_attempt_state s join delivery_attempts t
          using (session_socket_id, worktree_id, attempt_id, address_id)
        join mailbox_deliveries d using (session_socket_id, worktree_id, delivery_id, address_id)
        where d.message_id = ? and s.pointer_nonce is not null""", message)}


def banners(text):
    found = list(BANNER.finditer(text))
    return [(match.group(1), match.group(2), match.group(3),
             text[match.end():found[index + 1].start() if index + 1 < len(found) else len(text)])
            for index, match in enumerate(found)]


def new_banner(lane, block, before, drawn):
    return next(((nonce, label, sender, after) for nonce, label, sender, after in
                 banners(lane.screen(block)) if nonce not in before and nonce in drawn), None)


def word(index):
    return ("amber", "cobalt", "fennel", "juniper", "saffron", "tundra", "quartz")[index] + str(
        int(time.time()) % 900 + 100)


def type_prompt(run, lane, block, text, landed, what):
    """Types the prompt and waits for `landed`. A paste Codex did not submit gets one more
    Enter, which the run records."""
    lane.type_line(block, text)
    try:
        return wait(landed, bool, what, 45, 0.5)
    except Failed:
        screen = lane.screen(block)
        run.observe("retry: {} not seen in 45 s; screen".format(what), screen[-2000:])
        lane.key(block, "enter")
        return wait(landed, bool, what + " after one retried Enter", 90, 0.5)


def settle(a, since, spaces, quiet=8, within=240):
    """The rows once every delivery is acknowledged or settled and nothing changed for `quiet`
    seconds."""
    until, last, still = time.monotonic() + within, None, time.monotonic()
    while time.monotonic() < until:
        rows = messages(a, since, spaces)
        if rows != last:
            last, still = rows, time.monotonic()
        elif time.monotonic() - still >= quiet and rows and all(
                d["state"] in SETTLED for row in rows for d in row["to"]):
            return rows
        time.sleep(1)
    return last


def cross(run, a, spaces, sender, recipient, label, text_word, others):
    """A `#label @name` mention typed into the sender's agent reaches only that agent in the
    labelled session, once, and its reply comes back to the sender in the same thread.
    sender and recipient: (lane, space name, agent). others: every agent block that must not
    receive it, as (lane, block)."""
    s_lane, s_space, s_agent = sender
    r_lane, r_space, r_agent = recipient
    prompt = "#{} @{} Please reply with exactly the word {}.".format(label, r_agent, text_word)
    what = "#{} @{} from {}'s @{}".format(label, r_agent, s_space, s_agent)
    to_block, from_block = r_lane.blocks[r_agent], s_lane.blocks[s_agent]
    stale_to = {b[0] for b in banners(r_lane.screen(to_block))}
    stale_from = {b[0] for b in banners(s_lane.screen(from_block))}
    stale_others = {block: {b[0] for b in banners(lane.screen(block))} for lane, block in others}
    since = latest_ms(a)
    typed = time.time()
    type_prompt(run, s_lane, from_block, prompt, lambda: where(
        messages(a, since, spaces), s_space, s_agent, False), what + " in the store")
    copies = wait(lambda: where(messages(a, since, spaces), r_space, reply=False, to=r_agent),
                  bool, what + "'s copy in " + r_space, 120, 0.5)
    shown = wait(lambda: new_banner(r_lane, to_block, stale_to, nonces(a, copies[0]["message"])),
                 bool, what + "'s prompt on the recipient's screen", 240, 0.5)
    reached = time.time()

    def back():
        rows = messages(a, since, spaces)
        replies = [row for row in where(rows, s_space, reply=True, to=s_agent)
                   if text_word in row["body"].lower()]
        return replies and rows

    rows = wait(back, bool, what + "'s reply back in " + s_space, 240, 0.5)
    answered = time.time()
    reply = [row for row in where(rows, s_space, reply=True, to=s_agent)
             if text_word in row["body"].lower()][0]
    shown_back = wait(lambda: (lambda found: found if found and text_word in found[3].lower()
                               else None)(new_banner(s_lane, from_block, stale_from,
                                                     nonces(a, reply["message"]))),
                      bool, what + "'s reply on the sender's screen", 90, 0.5)
    rows = settle(a, since, spaces)
    time.sleep(15)
    again = messages(a, since, spaces)
    stray = {block: [b[:3] for b in banners(lane.screen(block)) if b[0] not in before]
             for (lane, block), before in zip(others, stale_others.values())}
    stray = {block: found for block, found in stray.items() if found}
    original, copy_, answer = (where(rows, s_space, s_agent, False), where(
        rows, r_space, reply=False, bridged=s_space), where(rows, r_space, r_agent, True))
    detail = {"typed": clock(typed), "reached": round(reached - typed, 1),
              "answered": round(answered - typed, 1), "rows": show(rows),
              "recipient prompt": shown[:3], "sender prompt": shown_back[:3],
              "stray prompts": stray, "rows after 15 s": len(again)}
    run.observe(what, dict(detail, raw=rows, recipient_screen=r_lane.screen(to_block)[-2500:],
                           sender_screen=s_lane.screen(from_block)[-2500:]))
    run.check(what + " reaches " + r_space + "'s @" + r_agent + " once",
              len(original) == 1 and len(copy_) == 1 and
              [d["alias"] for d in copy_[0]["to"]] == [r_agent] and
              copy_[0]["to"][0]["attempts"] == 1 and shown[1] == s_space and
              shown[2] == s_agent and not stray, detail)
    run.check(what + ": the reply comes back to the sender in the same thread",
              len(answer) == 1 and reply["bridged"] == r_space and
              reply["thread"] == original[0]["thread"] if original else False, detail)
    run.check(what + ": every delivery settled and nothing repeated in 15 s",
              all(d["state"] in SETTLED for row in rows for d in row["to"]) and again == rows,
              detail)


def broadcast(run, a, spaces, sender, agent, text_word, targets, others):
    """`#all @agent` from the sender reaches the agent in every session that has it, the
    sender's own included, and each reply comes back to the sender. targets: (space, lane)."""
    s_lane, s_space, s_agent = sender
    prompt = "#all @{} Please reply with exactly the word {}.".format(agent, text_word)
    what = "#all @{} from {}'s @{}".format(agent, s_space, s_agent)
    from_block = s_lane.blocks[s_agent]
    stale = {space: {b[0] for b in banners(lane.screen(lane.blocks[agent]))}
             for space, lane in targets}
    stale_others = [{b[0] for b in banners(lane.screen(block))} for lane, block in others]
    since = latest_ms(a)
    type_prompt(run, s_lane, from_block, prompt, lambda: where(
        messages(a, since, spaces), s_space, s_agent, False), what + " in the store")

    def back():
        rows = messages(a, since, spaces)
        replies = [row for row in where(rows, s_space, reply=True, to=s_agent)
                   if text_word in row["body"].lower()]
        return len(replies) >= len(targets) and rows

    rows = wait(back, bool, what + ": a reply from each session", 300, 0.5)
    rows = settle(a, since, spaces)
    time.sleep(10)
    again = messages(a, since, spaces)
    reached = {}
    for space, lane in targets:
        sent = where(rows, space, reply=False, to=agent)
        drawn = set().union(*(nonces(a, row["message"]) for row in sent)) if sent else set()
        reached[space] = [b[:3] for b in banners(lane.screen(lane.blocks[agent]))
                          if b[0] not in stale[space] and b[0] in drawn]
    stray = [[b[:3] for b in banners(lane.screen(block)) if b[0] not in before]
             for (lane, block), before in zip(others, stale_others)]
    replies = [row for row in where(rows, s_space, reply=True, to=s_agent)
               if text_word in row["body"].lower()]
    detail = {"rows": show(rows), "prompts on each target's screen": reached,
              "stray prompts": stray, "rows after 10 s": len(again)}
    run.observe(what, dict(detail, raw=rows))
    run.check(what + " reaches @{} in {}".format(agent, ", ".join(s for s, _ in targets)),
              all(len(where(rows, space, reply=False, to=agent)) == 1 and reached[space]
                  for space, _ in targets) and not any(stray), detail)
    run.check(what + ": one reply from each comes back, settled, nothing repeated",
              len(replies) == len(targets) and
              sorted(row["bridged"] or s_space for row in replies) ==
              sorted(space for space, _ in targets) and
              all(d["state"] in SETTLED for row in rows for d in row["to"]) and again == rows,
              detail)
    return rows


def store_counts(a):
    return store_rows(a, "select (select count(*) from collaboration_messages), "
                         "(select count(*) from mailbox_deliveries), "
                         "(select count(*) from bridged_messages)")[0]


def unknown_label(run, a, spaces, sender, session_id):
    """A label no session has. Typed at the start of a prompt, `#nosuch @codex` names no
    session, so it stays the person's own prompt: the hook adds nothing and nothing is sent
    (README, "A `#` word that names no workspace ... stays part of your prompt";
    src/message.rs's NotAMention). An agent `send` of the same source is refused with Rex's
    wording, which the driver asks the real Claude Code to run and reads from its transcript."""
    s_lane, s_space, s_agent = sender
    block = s_lane.blocks[s_agent]
    marker = word(6)
    prompt = "#{} @codex Please reply with exactly the word {}.".format(UNKNOWN, marker)
    since, before = latest_ms(a), store_counts(a)
    s_lane.type_line(block, prompt)
    entries = wait(lambda: claude_turn(a.profile, session_id, marker), lambda found: any(
        kind == "text" and role == "assistant" for role, kind, _ in found),
        "Claude Code's answer to the #nosuch prompt", 120, 1)
    time.sleep(10)
    after = store_counts(a)
    added = [entry for entry in entries if entry[1] == "attachment"]
    run.observe("#nosuch prompt", {"turn": entries, "counts": [before, after]})
    run.check("#{} @codex typed as a prompt stays the person's prompt: the hook adds nothing "
              "and nothing is sent".format(UNKNOWN),
              not any("Namedrop" in text for _, _, text in added) and before == after and
              not messages(a, since, spaces), {"hook context": added, "counts": [before, after],
                                               "answer": [e for e in entries if e[1] == "text"]})
    marker = word(3)
    source = "#{} @codex hello {}".format(UNKNOWN, marker)
    ask = ("Please use the Namedrop skill to run its send operation with author_kind "
           "agent_session and source exactly \"{}\", then quote any error it prints word for "
           "word.").format(source)
    since, before = latest_ms(a), store_counts(a)
    s_lane.type_line(block, ask)
    entries = wait(lambda: claude_turn(a.profile, session_id, marker), lambda found: any(
        kind == "tool_result" and "#" + UNKNOWN in text for _, kind, text in found) and found[-1][
        :2] == ("assistant", "text"), "Claude Code's send of " + source, 180, 1)
    time.sleep(5)
    after = store_counts(a)
    results = [text for _, kind, text in entries if kind == "tool_result" and "#" + UNKNOWN in text]
    expected = "no Rex session is labelled #{}; activated Rex sessions: #{}, #{}".format(
        UNKNOWN, ALPHA, BETA)
    run.observe("#nosuch agent send", {"turn": entries, "counts": [before, after]})
    run.check("an agent send to #{} @codex is refused: {}".format(UNKNOWN, expected),
              any(expected in text and "rejected" in text for text in results) and
              before == after and not messages(a, since, spaces),
              {"tool results": results, "counts": [before, after]})


def claude_turn(profile, session_id, marker):
    """(role, kind, text) for each entry from the prompt that holds `marker` on: the hook's
    attachments, the tool calls and results, and the answers."""
    found, after = [], False
    for path in transcript_files(profile, "claude", session_id):
        for line in path.read_text(errors="replace").splitlines():
            try:
                entry = json.loads(line)
            except ValueError:
                continue
            message = entry.get("message") or {}
            content = message.get("content")
            if entry.get("type") == "user" and isinstance(content, str):
                after = marker in content
                continue
            if not after or entry.get("type") not in ("user", "assistant", "attachment"):
                continue
            if entry.get("attachment"):
                found.append((entry["type"], "attachment", json.dumps(entry["attachment"])[:3000]))
            for part in content if isinstance(content, list) else ():
                kind = part.get("type")
                if kind in ("text", "tool_use", "tool_result"):
                    text = part.get("text") or part.get("input") or part.get("content")
                    found.append((message.get("role") or entry["type"], kind,
                                  text if isinstance(text, str) else json.dumps(text)[:3000]))
    return found


# B's session, started the way rex up starts the lane's.

def find_session(a, label):
    return next((row["session_id"] for row in a.json("list", "--json").get("sessions") or []
                 if row.get("label") == label), None)


def beta_repo(run, a):
    repo = a.profile.dir / BETA
    if not (repo / ".git").exists():
        repo.mkdir(exist_ok=True)
        for name in ("AGENTS.md", "CLAUDE.md", "README.md"):
            source = a.profile.repo / name
            if source.exists():
                (repo / name).write_bytes(source.read_bytes())
        for command in (["git", "init", "-q"], ["git", "add", "."],
                        ["git", "-c", "user.name=testbed", "-c", "user.email=testbed@localhost",
                         "commit", "-qm", "beta seed"]):
            run.run(a.lane.environment(), command, repo)
    return repo


def blocks_of(a, session):
    listed = a.json("api", "call", "session.list_blocks", json.dumps({"session_id": session}))
    found = {}
    for block in listed["blocks"]:
        label = (block.get("label") or "").lstrip("@")
        found[label if label in ("claude", "codex") else "shell"] = block["block_id"]
    return found


def hook_log_b(profile):
    return profile.dir / "logs/claude-hooks-beta.log"


def start_agent_b(a, session, blocks, agent, repo):
    command = rex_live.agent_command(a.profile, agent)
    if agent == "claude":
        if hook_log_b(a.profile).exists():
            hook_log_b(a.profile).unlink()
        command += ("--debug=hooks", "--debug-file", str(hook_log_b(a.profile)))
    created = a.json("split", "-s", session, "-b", blocks["shell"], "--label", agent, "--shell",
                     "none", "--focus=false", "-c", str(repo), "--json", "--", *command)
    blocks[agent] = created["block_ids"][0]
    return blocks[agent]


def hook_ran_b(profile, agent, before):
    if agent == "codex":
        roots = [root for name, root in rex_live.hook_runs(profile).items()
                 if before.get(name) != root and "/codex/plugins/" in root]
        return roots and "a hook run from " + roots[0]
    log = hook_log_b(profile)
    lines = log.read_text(errors="replace").splitlines() if log.exists() else []
    return any("SessionStart:" in line and "/scripts/session-start" in line and
               "finished with status 0" in line for line in lines) and "SessionStart in " + str(log)


def first_run_b(run, a, session, blocks, agent, repo, before):
    """rex_live.first_run for B's blocks, which the profile's state does not hold."""
    rows, answered, text = livetest.screens_for(agent), [], ""
    until = time.monotonic() + rex_live.FIRST_RUN_SECONDS
    while time.monotonic() < until:
        evidence = hook_ran_b(a.profile, agent, before)
        if evidence:
            return answered, evidence
        text = a.rex("capture", "-s", session, "-b", blocks[agent], "--trim", ok=False).stdout
        row = livetest.classify(text, rows)
        if row and row.outcome == livetest.ANSWER:
            a.rex("send-key", "-s", session, "-b", blocks[agent],
                  *[rex_live.KEYS[key] for key in row.keys])
            answered.append(row.name)
            time.sleep(2)
            if row.after == livetest.RESTART_CODEX:
                a.rex("block", "close", "-s", session, blocks.pop(agent))
                start_agent_b(a, session, blocks, agent, repo)
                answered.append("restarted in " + blocks[agent])
            continue
        if row and row.outcome == livetest.READY and agent == "codex" and \
                "warm-up" not in answered:
            a.rex("send", "-s", session, "-b", blocks[agent], livetest.WARM_UP_PROMPT)
            a.rex("send-key", "-s", session, "-b", blocks[agent], "enter")
            answered.append("warm-up")
        if row and row.outcome in (livetest.FATAL, livetest.EXITED):
            raise Failed("{} in B: {}\n{}".format(agent, row.name, text[-1500:]))
        time.sleep(0.5)
    raise Failed("{} in B ran no SessionStart hook; answered {}; screen:\n{}".format(
        agent, answered, text[-1500:]))


def start_beta(run, a):
    session = find_session(a, BETA)
    if session:
        blocks = blocks_of(a, session)
        if {"shell", "claude", "codex"} <= set(blocks):
            run.observe("session B reused", {"session": session, "blocks": blocks})
            return session, blocks
        raise Failed("session {} is labelled {} but lacks blocks: {}".format(session, BETA, blocks))
    repo = beta_repo(run, a)
    shell = ("/bin/bash", "--noprofile", "--rcfile", str(a.profile.dir / "home/.bashrc"), "-i")
    created = a.json("new", BETA, "--shell", "none", "-c", str(repo), "--window", "agents",
                     "--json", "--", *shell)
    session = created["session_id"]
    blocks = {"shell": created["initial_windows"][0]["block_ids"][0]}
    for agent in ("claude", "codex"):
        before = rex_live.hook_runs(a.profile)
        start_agent_b(a, session, blocks, agent, repo)
        answered, evidence = first_run_b(run, a, session, blocks, agent, repo, before)
        run.observe("B's " + agent, {"block": blocks[agent], "answered": answered,
                                     "evidence": evidence})
    return session, blocks


def named(lane):
    return {alias for alias, in lane.rows(
        "select c.alias from alias_claims c join pane_addresses p using (session_socket_id, "
        "worktree_id, address_id) where c.worktree_id = ? and c.claim_state = 'active' and "
        "p.retired = 0", lane.session)}


# What the agents were shown.

def agent_sessions(a, session):
    return dict(a.rows("select c.alias, r.agent_session_id from integration_registrations r join "
                       "alias_claims c using (session_socket_id, worktree_id, address_id) where "
                       "r.worktree_id = ? and r.active = 1 and c.claim_state = 'active'", session))


def transcript_files(profile, agent, session_id):
    if agent == "claude":
        return sorted((profile.dir / "home/.claude/projects").glob("*/{}.jsonl".format(session_id)))
    return sorted(path for path in (profile.dir / "codex/sessions").rglob("*.jsonl")
                  if session_id in path.name)


def other_sessions_lines(profile, agent, session_id):
    lines = set()
    for path in transcript_files(profile, agent, session_id):
        text = path.read_text(errors="replace").replace("\\n", "\n").replace('\\"', '"')
        lines.update(re.findall(r"Other Rex sessions: \d+ other Rex sessions? ha(?:s|ve) named "
                                r"agents \([^)]*\)", text))
        lines.update(re.findall(r"Other workspaces: [^.]*\.", text))
    return sorted(lines)


def doctor(run, a, session, block):
    launcher = rex_live.state_directory(a.profile) / "runtime/namedrop"
    result = run.run(a.lane.environment(), [launcher, "--endpoint", "rex:" + str(a.lane.socket),
                                            "--space", session, "--pane", block, "--json",
                                            "doctor"], a.profile.repo, ok=False)
    try:
        data = livetest.last_json(result.stdout).get("data") or {}
    except SystemExit:
        data = {"unparsed": result.stdout[-1500:]}
    return result.returncode, data


def backlog(a, session):
    return a.rows(
        "select (select count(*) from mailbox_deliveries where worktree_id = ?1 and state in "
        "('queued', 'dispatching', 'submitted', 'acknowledged')), (select count(*) from "
        "attention_items where worktree_id = ?1 and resolved_at_ms is null), (select count(*) "
        "from delivery_attempt_state where worktree_id = ?1 and active = 1)", session)[0]


# Channels.

class Page(dailyuse.Channels):
    """dailyuse's person at the Channels page, recording every session's frames."""

    def __init__(self, url):
        self.by_key = {}
        super().__init__(url)

    def _read(self, stream):
        name = None
        for raw in stream:
            line = raw.decode(errors="replace").rstrip("\n")
            if line.startswith("event: "):
                name = line[7:]
            elif line.startswith("data: ") and name == "space":
                event = json.loads(line[6:])
                self.by_key.setdefault(event.get("key"), []).append(
                    dailyuse.parse_frame(event["frame"]))
                if event.get("key") == self.key:
                    self.raw.append(dailyuse.small_frame(event["frame"]))
                    self.frames.append(dailyuse.parse_frame(event["frame"]))

    def history_of(self, key):
        rows = self.get("/w/{}/messages?limit=100".format(key))
        return tuple(dailyuse.parse_message(row) for row in rows)

    def post_to(self, key, source):
        receipt = self._post("/w/{}/messages".format(key), {"request_id": str(time.time_ns()),
                                                             "source": source})
        return dailyuse.Sent(receipt["message_id"], receipt["thread_id"])

    def raw_get(self, path):
        with self.opener.open(self.base + path, timeout=10) as response:
            return response.status, response.read().decode(errors="replace")

    def raw_post(self, path, body):
        try:
            return 200, self._post(path, body)
        except dailyuse.ChannelsError as error:
            return None, str(error)


def channels_link(run, a):
    """Runs `namedrop channels --no-open` in A's shell block and returns the printed link."""
    log = a.profile.dir / "logs/xsession-channels.txt"
    done = a.profile.dir / "logs/xsession-channels.exit"
    for path in (log, done):
        if path.exists():
            path.unlink()
    shell = a.blocks["shell"]
    a.key(shell, "ctrl+c")
    a.rex("send", "-s", a.session, "-b", shell, "namedrop channels --no-open 2>&1 | tee {}; "
          "echo ${{PIPESTATUS[0]}} > {}".format(shlex.quote(str(log)), shlex.quote(str(done))))
    a.key(shell, "enter")
    wait(lambda: done.exists() and done.read_text().strip(), bool, "namedrop channels to exit", 90)
    printed = log.read_text(errors="replace")
    link = re.search(r"https?://127\.0\.0\.1:\d+/[^\s\"]+", printed)
    run.observe("namedrop channels --no-open", {"exit": done.read_text().strip(),
                                                 "printed": printed[-3000:],
                                                 "screen": a.screen(shell)[-2000:]})
    return done.read_text().strip(), printed, link.group(0) if link else None


def channel_answer(run, page, key, agent, label, text_word):
    """A composer post to @agent in one session's channel gets one answer in its thread."""
    prompt = "@{} Please reply with exactly the word {}. Answer in your final message, not " \
             "with a command.".format(agent, text_word)
    address = next((row.address_id for row in (page.by_key.get(key) or [None])[-1].participants
                    if row.alias == agent), None) if page.by_key.get(key) else None
    sent = page.post_to(key, prompt)

    def answers():
        return [row for row in page.history_of(key)
                if row.thread_id == sent.thread_id and row.message_id != sent.message_id and
                not row.progress_note and (address is None or row.author == address)]

    try:
        found = wait(answers, bool, "@{}'s answer in {}'s channel".format(agent, label), 240, 1)
    except Failed as error:
        found = []
        run.note(str(error))
    time.sleep(15)
    final = answers()
    history = [row._asdict() for row in page.history_of(key) if row.thread_id == sent.thread_id]
    run.observe("Channels post to {}'s @{}".format(label, agent),
                {"sent": sent._asdict(), "address": address, "thread": history})
    run.check("Channels: a composer post to @{} in {} gets one answer in its thread".format(
        agent, label), len(final) == 1 and text_word in final[0].body.lower() and
        final[0].in_reply_to == sent.message_id,
        {"answers": [row._asdict() for row in final], "found first": len(found)})


def channels_smoke(run, a, b, spaces_by_session):
    code, printed, link = channels_link(run, a)
    run.check("namedrop channels --no-open in A's shell block exits 0 and prints a link",
              code == "0" and bool(link), {"exit": code, "printed": printed[-600:]})
    if not link:
        return None
    try:
        page = Page(link)
    except urllib.error.HTTPError as error:
        # Recorded, then retried once with a new link from the same shell block.
        run.check("Channels: the printed link signs in", False,
                  {"link": link.split("?")[0], "error": repr(error),
                   "body": error.read().decode(errors="replace")[:300]})
        code, printed, link = channels_link(run, a)
        page = Page(link)
    run.check("Channels: a link signs in", True, link.split("?")[0])
    status, html = page.raw_get("/")
    run.check("Channels: the page loads", status == 200 and "<html" in html.lower(),
              {"status": status, "length": len(html)})
    spaces = page.get("/api/spaces")
    keys = {}
    for row in spaces:
        name = spaces_by_session.get(row.get("workspace_id"))
        if name:
            keys[name] = row.get("key")
    wait(lambda: all(page.by_key.get(key) for key in keys.values()), bool,
         "a frame for each session", 30, 0.5)
    members = {name: sorted(row.alias for row in page.by_key[key][-1].participants)
               for name, key in keys.items() if page.by_key.get(key)}
    run.observe("Channels spaces", {"spaces": spaces, "keys": keys, "members": members,
                                    "session": page.get("/api/session")})
    # The page learns live labels from the host once its stream subscribes, so a first read
    # can lack them; a person's sidebar refreshes the same way.
    first = spaces

    def labelled():
        rows = page.get("/api/spaces")
        names = {spaces_by_session.get(row.get("workspace_id")): (row.get("label") or {}).get(
            "text") for row in rows}
        return rows if all(names.get(name) == name for name in (ALPHA, BETA)) else None

    try:
        spaces = wait(labelled, bool, "both labels in /api/spaces", 30, 1)
    except Failed:
        spaces = page.get("/api/spaces")
    run.observe("Channels /api/spaces, first read and after the stream subscribed",
                {"first": first, "later": spaces})
    run.check("Channels: /api/spaces lists both Rex sessions with their labels",
              set(keys) == {ALPHA, BETA} and labelled() is not None, {"first": first,
                                                                        "later": spaces})
    run.check("Channels: each session's channel shows its @claude and @codex",
              all({"claude", "codex"} <= set(members.get(name, ())) for name in (ALPHA, BETA)),
              members)
    if set(keys) == {ALPHA, BETA}:
        channel_answer(run, page, keys[ALPHA], "claude", ALPHA, word(4))
        channel_answer(run, page, keys[BETA], "codex", BETA, word(5))
    limits = channels_limits(run, page, keys, a)
    page.stop()
    return limits


def channels_limits(run, page, keys, a):
    """The Rex limits the docs record, read from the page's own routes, as notes."""
    notes = {}
    key = keys.get(ALPHA)
    if not key:
        return notes
    # The bodies the page's New channel and Add agent forms send
    # (src/dashboard/channel_create.rs).
    notes["New channel"] = page.raw_post("/w/{}/channels".format(key), {
        "name": "xs-probe", "folder": str(a.profile.repo)})
    notes["Add agent"] = page.raw_post("/w/{}/agents".format(key), {
        "kind": "claude", "name": "xsprobe"})
    frame = (page.by_key.get(key) or [None])[-1]
    notes["health level"] = frame.level if frame else None
    wire = json.dumps(page.raw[-1] if page.raw else {})
    notes["latest frame mentions Herdr"] = sorted(set(re.findall(
        r"[^.\"]{0,80}Herdr[^.\"]{0,60}", wire)))[:8]
    run.observe("Channels Rex limits", notes)
    return notes


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--skip-channels", action="store_true")
    parser.add_argument("--channels-only", action="store_true",
                        help="skip the mentions, on a lane a full run already set up")
    args = parser.parse_args()
    if not args.profile.startswith("rex-"):
        parser.error("--profile names a rex- profile")
    run = Run(args.out.resolve())
    started = time.monotonic()
    try:
        a = step6.Lane(run, args.profile)
        version = a.rex("version").stdout.splitlines()
        print("Rex {} on {}, session A {}".format(version[0] if version else "?", a.lane.server,
                                                  a.session), flush=True)
        if named(a) != {"claude", "codex"}:
            step6.check_install(run, a)
            step6.activate(run, a)
            for agent in ("claude", "codex"):
                step6.name_from_palette(run, a, agent)
        a.rex("session", "rename", a.session, ALPHA)
        session_b, blocks_b = start_beta(run, a)
        b = lane_for(a, session_b, blocks_b)
        if named(b) != {"claude", "codex"}:
            step6.activate(run, b)
            for agent in ("claude", "codex"):
                step6.name_from_palette(run, b, agent)
        labels = {row["session_id"]: row.get("label")
                  for row in a.json("list", "--json").get("sessions") or []}
        run.observe("sessions", {"labels": labels, "A": [a.session, a.blocks],
                                 "B": [b.session, b.blocks]})
        run.check("A is labelled alpha and B beta, both activated, each with @claude and @codex",
                  labels.get(a.session) == ALPHA and labels.get(b.session) == BETA and
                  named(a) == named(b) == {"claude", "codex"}, labels)
        spaces = {a.session: ALPHA, b.session: BETA}
        lanes = {ALPHA: a, BETA: b}
        every = [(lane, lane.blocks[agent]) for lane in (a, b) for agent in ("claude", "codex")]

        def others(*keep):
            return [(lane, block) for lane, block in every
                    if (spaces[lane.session], next(k for k, v in lane.blocks.items()
                                                   if v == block)) not in keep]

        steps = [] if args.channels_only else [
            ("A to B", lambda: cross(run, a, spaces, (a, ALPHA, "claude"), (b, BETA, "codex"),
                                     BETA, word(0), others((ALPHA, "claude"), (BETA, "codex")))),
            ("B to A", lambda: cross(run, a, spaces, (b, BETA, "claude"), (a, ALPHA, "codex"),
                                     ALPHA, word(1), others((BETA, "claude"), (ALPHA, "codex")))),
            ("#all", lambda: broadcast(run, a, spaces, (a, ALPHA, "codex"), "claude", word(2),
                                       [(ALPHA, a), (BETA, b)],
                                       others((ALPHA, "codex"), (ALPHA, "claude"),
                                              (BETA, "claude")))),
        ]
        for name, step in steps:
            try:
                step()
            except (Failed, subprocess.SubprocessError, OSError, ValueError, KeyError,
                    IndexError, sqlite3.Error) as error:
                run.note("{} stopped: {!r}".format(name, error))
                run.check(name + " ran to its end", False, str(error)[:1500])
        try:
            if not args.channels_only:
                unknown_label(run, a, spaces, (a, ALPHA, "claude"),
                              agent_sessions(a, a.session)["claude"])
        except Failed as error:
            run.check("#nosuch @codex ran to its end", False, str(error)[:1500])
        shown = {}
        for space, lane in lanes.items():
            for agent, session_id in agent_sessions(a, lane.session).items():
                shown["{} @{}".format(space, agent)] = {
                    "session": session_id,
                    "files": [str(p) for p in transcript_files(a.profile, agent, session_id)],
                    "lines": other_sessions_lines(a.profile, agent, session_id)}
        run.observe("other-sessions lines the agents were shown", shown)
        run.check("A's Claude Code was shown an `Other Rex sessions:` line naming B's agents",
                  any("1 other Rex session has named agents (@claude, @codex)" in line
                      for line in shown.get(ALPHA + " @claude", {}).get("lines", [])),
                  shown.get(ALPHA + " @claude"))
        run.check("B's Claude Code was shown an `Other Rex sessions:` line naming A's agents",
                  any("1 other Rex session has named agents (@claude, @codex)" in line
                      for line in shown.get(BETA + " @claude", {}).get("lines", [])),
                  shown.get(BETA + " @claude"))
        run.check("no agent was shown Herdr's `Other workspaces:` wording",
                  not any(line.startswith("Other workspaces") for row in shown.values()
                          for line in row["lines"]), shown)
        address_list = run.run(a.lane.environment(), [
            rex_live.state_directory(a.profile) / "runtime/namedrop", "--endpoint",
            "rex:" + str(a.lane.socket), "--space", a.session, "--pane", a.blocks["claude"],
            "--json", "address", "list", "--input-json", json.dumps({"workspaces": "all"})],
            a.profile.repo, ok=False)
        run.observe("address list workspaces all from A's @claude",
                    {"exit": address_list.returncode, "stdout": address_list.stdout[-4000:],
                     "stderr": address_list.stderr[-1000:]})
        if not args.skip_channels:
            try:
                limits = channels_smoke(run, a, b, spaces)
                run.observe("Channels limits recorded", limits)
            except (Failed, OSError, ValueError, KeyError, dailyuse.ChannelsError) as error:
                run.check("Channels smoke ran to its end", False, repr(error)[:1500])
        time.sleep(10)
        for space, lane in lanes.items():
            left = backlog(a, lane.session)
            run.check("nothing left awaiting in " + space, tuple(left) == (0, 0, 0),
                      {"open deliveries, open attention, active attempts": left})
            code, data = doctor(run, a, lane.session, lane.blocks["claude"])
            issues = [(issue.get("code"), issue.get("severity"), issue.get("summary"))
                      for issue in data.get("issues") or []]
            problems = [issue for issue in issues if issue[1] != "info"]
            run.observe("doctor in " + space, data)
            run.check("Doctor is clean in " + space + " (info items allowed)", code == 0 and not problems and
                      data.get("status") in ("healthy", "ok", None),
                      {"exit": code, "status": data.get("status"), "issues": issues,
                       "waiting": data.get("waiting_messages")})
    except (Failed, RuntimeError, subprocess.SubprocessError, OSError, KeyError, ValueError,
            sqlite3.Error) as error:
        run.note("stopped: {!r}".format(error))
        print("stopped: {}".format(error), flush=True)
        run.checks.append({"check": "the run reached its end", "ok": False, "detail": str(error)})
    finally:
        run.observe("seconds", round(time.monotonic() - started, 1))
        run.save()
    failed = [check["check"] for check in run.checks if not check["ok"]]
    print("RESULT {} ({} checks, {} failed, {:.0f}s)".format(
        "FAIL" if failed else "PASS", len(run.checks), len(failed), time.monotonic() - started),
        flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
