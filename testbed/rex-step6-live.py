#!/usr/bin/env python3
"""UI.md's U14 on the testbed: PLAN's step 6 live check on a Rex lane that
`./testbed rex up --fresh` just installed with the archive's install.sh and no Herdr.

usage: python3 -I testbed/rex-step6-live.py --profile rex-NAME --out DIR

1. The lane's server lists the module's eleven actions, and About Namedrop names the module,
   launcher and state that install.sh wrote.
2. `rex do namedrop_activate --origin palette` in the Claude Code block opens exactly one layer,
   which Esc closes, and activates the session.
3. `rex do namedrop_name --origin palette` in the Claude Code block and in the Codex block opens
   the name screen, the driver types `claude` and `codex` into it and saves, and each agent is
   then named and bound.
4. A mention typed into Claude Code reaches Codex and Codex's reply comes back, then a mention
   typed into Codex reaches Claude Code and its reply comes back. The driver runs no Namedrop
   command during the exchange, which it checks from its own command list: it types into the
   agents, reads their screens and reads the store read-only, and the agents' Namedrop hooks
   carry each mention and reply. Each screen check needs a delivery prompt that was not on the
   screen before the typing and whose nonce the store drew for that exchange's message.

Every rex call names the lane's server with -S and --autostart=false, from the lane's own
environment. Claude Code runs only as the profile's token wrapper, which the lane started.
The driver logs every command to DIR/commands.log, writes DIR/evidence.json, prints one line per
check, and leaves the lane up for `./testbed rex down`.
"""

import argparse
import contextlib
import datetime
import json
import os
from pathlib import Path
import re
import shlex
import sqlite3
import subprocess
import sys
import time

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
import rex_live  # noqa: E402
import testbed  # noqa: E402

ACTIONS = sorted("namedrop_" + name for name in (
    "activate", "name", "manage", "doctor", "status", "tour", "channels", "pause", "resume",
    "deactivate", "about"))
# The prompt a delivery types into the recipient's pane opens with its attempt's pointer nonce
# and the sender (src/pointer.rs). A host may wrap it, so any whitespace separates the words.
BANNER = re.compile(r"Namedrop\s+message\s+([0-9a-f]{8})\s+from\s+@([\w-]+)")


class Failed(Exception):
    pass


def clock(seconds=None):
    return datetime.datetime.fromtimestamp(time.time() if seconds is None else seconds).strftime(
        "%H:%M:%S.%f")[:-3]


class Run:
    def __init__(self, out):
        out.mkdir(parents=True, exist_ok=True)
        self.out, self.log = out, (out / "commands.log").open("a")
        self.checks, self.observed, self.commands = [], {}, []

    def note(self, text):
        self.log.write("[{}] {}\n".format(clock(), text))
        self.log.flush()

    def run(self, environment, command, cwd, timeout=120, ok=True):
        started = time.monotonic()
        self.commands.append([str(word) for word in command])
        result = subprocess.run(testbed.env_command(environment, [str(word) for word in command]),
                                cwd=str(cwd), text=True, stdin=subprocess.DEVNULL,
                                capture_output=True, timeout=timeout)
        self.note("$ {}\n[exit {} in {:.2f}s]\n{}{}".format(
            shlex.join(str(word) for word in command), result.returncode,
            time.monotonic() - started, result.stdout, result.stderr))
        if ok and result.returncode != 0:
            raise Failed("{} exited {}: {}{}".format(shlex.join(str(w) for w in command),
                                                     result.returncode, result.stdout[-2000:],
                                                     result.stderr[-2000:]))
        return result

    def check(self, what, passed, detail=None):
        self.checks.append({"check": what, "ok": bool(passed), "detail": detail})
        line = ("ok   " if passed else "FAIL ") + what
        self.note(line + ": " + json.dumps(detail, default=str))
        print(line + ("" if passed else ": " + json.dumps(detail, default=str)[:1500]), flush=True)
        self.save()
        if not passed:
            raise Failed(what)

    def observe(self, key, value):
        self.observed[key] = value
        self.note("observed {}: {}".format(key, json.dumps(value, default=str)))
        self.save()

    def save(self):
        (self.out / "evidence.json").write_text(json.dumps(
            {"checks": self.checks, "observed": self.observed}, indent=2, default=str) + "\n")


def wait(read, accepts, what, within=30, step=0.3):
    until = time.monotonic() + within
    value = None
    while time.monotonic() < until:
        value = read()
        if accepts(value):
            return value
        time.sleep(step)
    raise Failed("{} within {}s: {!r}".format(what, within, value))


class Lane:
    """The lane `rex up` started, read from its profile, with the calls the check makes."""

    def __init__(self, run, name):
        with open(os.devnull, "w") as quiet, contextlib.redirect_stdout(quiet):
            self.profile = testbed.Profile(name)
            self.lane = rex_live.Lane(self.profile, testbed.fail, lambda message: None)
        state = self.profile.state
        if state.get("purpose") != "rex" or not state.get("installed"):
            raise Failed("profile {} is not a Rex lane that install.sh installed".format(name))
        self.run, self.session = run, state.get("rex_live_session")
        self.blocks = dict(state.get("rex_live_blocks") or {})
        if not self.session or not {"claude", "codex"} <= set(self.blocks):
            raise Failed("profile {} has no lane session with Claude Code and Codex; run "
                         "./testbed rex up --fresh first".format(name))
        self.state = rex_live.state_directory(self.profile)

    def rex(self, *args, ok=True, timeout=60):
        return self.run.run(self.lane.environment(),
                            [self.lane.binary(), "-S", self.lane.server, "--autostart=false", *args],
                            self.profile.dir, timeout=timeout, ok=ok)

    def json(self, *args):
        return json.loads(self.rex(*args).stdout)

    def every_block(self):
        """Every block of every session on the lane's server, so a layer opened anywhere counts."""
        sessions = self.json("list", "--json").get("sessions") or []
        return {block["block_id"]: dict(block, session=session["session_id"])
                for session in sessions for block in self.json(
                    "api", "call", "session.list_blocks",
                    json.dumps({"session_id": session["session_id"]}))["blocks"]}

    def layers(self):
        """The lane session's layers that are not a window's tiled one, each with its blocks."""
        def blocks(node):
            if "block_id" in node:
                return [node["block_id"]]
            split = node.get("split") or {}
            return blocks(split.get("before") or {}) + blocks(split.get("after") or {}) \
                if split else []
        view = self.json("api", "call", "session.view", json.dumps({"session_id": self.session}))
        return {layer["layer_id"]: {"kind": layer.get("kind"), "window": window["window_id"],
                                    "blocks": blocks(layer.get("layout") or {})}
                for window in view.get("windows") or [] for layer in window.get("layers") or []
                if layer.get("kind") != "tiled"}

    def screen(self, block):
        return self.rex("capture", "-s", self.session, "-b", block, "--trim", ok=False).stdout

    def key(self, block, key):
        self.rex("send-key", "-s", self.session, "-b", block, key, ok=False)

    def type_line(self, block, text):
        self.rex("send", "-s", self.session, "-b", block, text)
        time.sleep(0.5)
        self.key(block, "enter")

    def rows(self, query, *values):
        connection = sqlite3.connect("file:{}?mode=ro".format(self.state / "namedrop.sqlite3"),
                                     uri=True, timeout=5)
        try:
            return connection.execute(query, values).fetchall()
        finally:
            connection.close()


def palette(lane, block, action, agent, settle=3.0):
    """Runs a palette action on the block and returns what it opened, after waiting `settle`
    seconds more for any second one: the new blocks of every session, and the lane session's
    new floating layers."""
    before, layers_before = lane.every_block(), lane.layers()
    done = lane.rex("do", "-s", lane.session, "-b", block, "--origin", "palette", action, ok=False)
    opened = wait(lambda: set(lane.every_block()) - set(before), bool, action + " to open a layer",
                  20)
    time.sleep(settle)
    after = lane.every_block()
    new = {block_id: after[block_id] for block_id in set(after) - set(before)}
    layers = {layer: value for layer, value in lane.layers().items() if layer not in layers_before}
    lane.run.observe("{} opened in the {} block".format(action, agent), {"exit": done.returncode, "stdout": done.stdout,
                                          "stderr": done.stderr, "first seen": sorted(opened),
                                          "new blocks": new, "new layers": layers})
    one = len(new) == 1 and len(layers) == 1 and [
        value for value in layers.values() if value["kind"] == "floating" and
        value["blocks"] == list(new)]
    return done, new, bool(one)


def check_install(run, lane):
    listed = lane.json("actions", "--json")
    items = listed if isinstance(listed, list) else listed.get("actions", listed)
    names = sorted(item["name"] for item in items if item["name"].startswith("namedrop_"))
    run.observe("namedrop actions", [{key: item.get(key) for key in ("name", "title", "category")}
                                     for item in items if item["name"].startswith("namedrop_")])
    run.check("the server lists the module's eleven actions", names == ACTIONS, names)
    reply = lane.json("do", "namedrop_about", "--json")
    about = reply.get("result", reply) if "module" not in reply else reply
    run.observe("namedrop_about", about)
    run.check("About Namedrop names the installed module, launcher and state",
              about.get("module", "").lstrip("@") == str(lane.lane.dir / "config/namedrop.lua") and
              about.get("state") == str(lane.state) and
              Path(about.get("launcher", "")).parent == lane.state / "runtime", about)
    bound = lane.rows("select count(*) from pane_addresses where retired = 0")[0][0]
    run.check("no pane is named before the palette names them", bound == 0, bound)
    activated = lane.rows("select activated, deactivated from worktrees where worktree_id = ?",
                          lane.session)
    run.check("the session is not activated before the palette activates it",
              not [row for row in activated if row[0]], activated)


def activate(run, lane):
    claude = lane.blocks["claude"]
    done, new, one = palette(lane, claude, "namedrop_activate", "claude")
    run.check("Activate Namedrop from the palette exits 0 and opens exactly one floating layer, "
              "and no other block", done.returncode == 0 and one,
              {"exit": done.returncode, "stderr": done.stderr, "new": new})
    layer, = new
    shown = wait(lambda: lane.screen(layer), lambda text: text.strip(), "the layer to paint", 20)
    run.observe("activation layer", shown[-3000:])
    run.check("the layer is in the lane's session", new[layer]["session"] == lane.session, new)
    activated = lane.rows("select activated, deactivated from worktrees where worktree_id = ?",
                          lane.session)
    run.check("the session is activated", activated == [(1, 0)], activated)
    for _ in range(6):
        if layer not in lane.every_block():
            break
        lane.key(layer, "escape")
        time.sleep(1)
    run.check("Esc closed the layer", layer not in lane.every_block() and not lane.layers(), layer)


def name_from_palette(run, lane, agent):
    block = lane.blocks[agent]
    done, new, one = palette(lane, block, "namedrop_name", agent)
    run.check("Name this pane in the {} block opens exactly one floating layer".format(agent),
              done.returncode == 0 and one, {"exit": done.returncode, "stderr": done.stderr,
                                             "new": new})
    layer, = new
    wait(lambda: lane.screen(layer), lambda text: "Name this pane" in text,
         "the name screen to paint", 20)
    lane.rex("send", "-s", lane.session, "-b", layer, agent)
    time.sleep(0.5)
    lane.key(layer, "enter")
    # An agent's name screen asks its pane type, with agent the default, and then its persona,
    # whose screen saves with Enter once its suggestion settled. Enter goes again only for a
    # new screen, or one that stayed for 5 s.
    screens, pressed, pressed_at = [], None, 0.0
    until = time.monotonic() + 90
    while layer in lane.every_block() and time.monotonic() < until:
        time.sleep(1)
        shown = lane.screen(layer)
        if shown not in screens:
            screens.append(shown)
        if ("Pane type:" in shown or "Enter Save" in shown) and (
                shown != pressed or time.monotonic() - pressed_at > 5):
            lane.key(layer, "enter")
            pressed, pressed_at = shown, time.monotonic()
    run.observe("{} name screens".format(agent), screens)
    run.check("the {} name screen closed".format(agent), layer not in lane.every_block(),
              screens[-1:])
    label = wait(lambda: lane.every_block()[block].get("label"), lambda value: value == "@" + agent,
                 "the {} block to be labelled @{}".format(agent, agent), 20)
    row = wait(lambda: lane.rows(
        "select a.pane_type, r.runtime, r.agent_session_id from pane_addresses a join "
        "pane_bindings b using (session_socket_id, worktree_id, address_id) join alias_claims c "
        "using (session_socket_id, worktree_id, address_id) join integration_registrations r "
        "using (session_socket_id, worktree_id, address_id) where a.worktree_id = ? and "
        "b.pane_id = ? and a.retired = 0 and b.active = 1 and c.claim_state = 'active' and "
        "c.alias = ? and r.active = 1", lane.session, block, agent), bool,
        "@{}'s binding".format(agent), 20)
    runtime = {"claude": "claude-code", "codex": "codex"}[agent]
    run.check("Name this pane named the {} block @{}, bound to its agent session".format(
        agent, agent), label == "@" + agent and row == [("agent", runtime, row[0][2])] and
        row[0][2], {"label": label, "binding": row})


def thread_of(lane, author, words):
    """The latest message `author` wrote whose body holds `words`, with its thread's rows."""
    found = lane.rows("select thread_id, message_id, created_at_ms from collaboration_messages "
                      "where author_alias_snapshot in (?, ?) and body like ? order by created_at_ms "
                      "desc limit 1", author, "@" + author, "%" + words + "%")
    if not found:
        return None
    thread = found[0][0]
    return found[0], lane.rows(
        "select m.thread_order, m.author_alias_snapshot, coalesce(m.in_reply_to, '-'), m.body, "
        "m.grammar_version, group_concat(d.state), m.message_id from collaboration_messages m "
        "left join mailbox_deliveries d using (session_socket_id, worktree_id, message_id) where "
        "m.thread_id = ? group by m.message_id order by m.thread_order", thread)


def nonces(lane, message):
    """The pointer nonces the store drew for the message's delivery attempts."""
    return {row[0] for row in lane.rows(
        "select s.pointer_nonce from delivery_attempt_state s join delivery_attempts a using "
        "(session_socket_id, worktree_id, attempt_id, address_id) join mailbox_deliveries d using "
        "(session_socket_id, worktree_id, delivery_id, address_id) where d.message_id = ? and "
        "s.pointer_nonce is not null", message)}


def banners(text, sender):
    """Each delivery prompt from @sender on a screen: its nonce and the text after it."""
    found = [match for match in BANNER.finditer(text) if match.group(2) == sender]
    return [(match.group(1), text[match.end():found[index + 1].start()
                                   if index + 1 < len(found) else len(text)])
            for index, match in enumerate(found)]


def new_banner(lane, block, sender, message, before):
    """The nonce of a delivery prompt on the block's screen that was not there before and that
    the store drew for `message`, with the text after it, or None."""
    shown = [(nonce, after) for nonce, after in banners(lane.screen(block), sender)
             if nonce not in before]
    if not shown:
        return None
    drawn = nonces(lane, message)
    return next(((nonce, after) for nonce, after in shown if nonce in drawn), None)


def exchange(run, lane, sender, recipient, word, within=240):
    """A mention typed into the sender's agent reaches the recipient, whose reply comes back.
    Each screen check needs a delivery prompt that was not on the screen before the typing and
    whose nonce the store drew for this exchange's message, so an earlier exchange's prompt
    never counts."""
    prompt = "@{} Please reply with exactly the word {}.".format(recipient, word)
    to_block, from_block = lane.blocks[recipient], lane.blocks[sender]
    on_recipient = {nonce for nonce, _ in banners(lane.screen(to_block), sender)}
    on_sender = {nonce for nonce, _ in banners(lane.screen(from_block), recipient)}
    lane.type_line(from_block, prompt)
    typed = time.time()
    request = wait(lambda: thread_of(lane, sender, prompt[len(recipient) + 2:]), bool,
                   "@{}'s mention in the store".format(sender), 120, 0.5)
    mention = request[0][1]
    reached_banner = wait(lambda: new_banner(lane, to_block, sender, mention, on_recipient), bool,
                          "the mention's prompt on @{}'s screen".format(recipient), within, 0.5)
    reached = time.time()

    def replied():
        rows = thread_of(lane, sender, prompt[len(recipient) + 2:])[1]
        return [row for row in rows if row[1].lstrip("@") == recipient and
                word in row[3].lower()] and rows

    rows = wait(replied, bool, "@{}'s reply in the thread".format(recipient), within, 0.5)
    answered = time.time()
    reply = next(row[6] for row in rows if row[1].lstrip("@") == recipient and
                 word in row[3].lower())
    shown_banner = wait(
        lambda: (lambda found: found if found and word in found[1].lower() else None)(
            new_banner(lane, from_block, recipient, reply, on_sender)),
        bool, "@{}'s reply on @{}'s screen".format(recipient, sender), 60, 0.5)
    # The reply's delivery to the sender is acknowledged once the sender's agent took it.
    rows = wait(lambda: thread_of(lane, sender, prompt[len(recipient) + 2:])[1], lambda found: any(
        row[1].lstrip("@") == recipient and word in row[3].lower() and row[5] in (
            "acknowledged", "settled") for row in found), "the reply's delivery to be acknowledged",
        120, 0.5)
    run.observe("@{} to @{} thread".format(sender, recipient), rows)
    run.observe("@{} screen after the reply".format(sender), lane.screen(from_block)[-2500:])
    run.check("@{}'s mention reached @{} ({:.1f}s) and the {} reply came back ({:.1f}s)".format(
        sender, recipient, reached - typed, word, answered - typed),
        rows[0][1].lstrip("@") == sender and rows[0][6] == mention and
        rows[0][4] == "mention-hook/v1" and len(rows) >= 2, {
            "typed": clock(typed), "mention": mention, "reply": reply,
            "stale prompts on @{}'s screen".format(recipient): sorted(on_recipient),
            "mention's prompt": reached_banner[0], "on recipient's screen": clock(reached),
            "reply in thread": clock(answered),
            "stale prompts on @{}'s screen".format(sender): sorted(on_sender),
            "reply's prompt": shown_banner[0], "thread": rows})


def check_exchange_commands(run, lane, commands):
    """The driver's own commands during the exchanges: the lane's rex, and only capture, send and
    send-key. The mentions and replies travel through the agents' Namedrop hooks."""
    prefix = [str(lane.lane.binary()), "-S", lane.lane.server, "--autostart=false"]
    kinds = sorted({command[len(prefix)] if command[:len(prefix)] == prefix else
                    shlex.join(command) for command in commands})
    run.check("the driver ran no Namedrop command during the exchanges, only rex capture, send "
              "and send-key", set(kinds) <= {"capture", "send", "send-key"},
              {"commands": len(commands), "kinds": kinds})


def main():
    parser = argparse.ArgumentParser(description=__doc__,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    if not args.profile.startswith("rex-"):
        parser.error("--profile names a rex- profile")
    run = Run(args.out.resolve())
    try:
        lane = Lane(run, args.profile)
        version = lane.rex("version").stdout.splitlines()
        run.observe("lane", {"rex": lane.lane.binary(), "version": version, "server": lane.lane.server,
                             "session": lane.session, "blocks": lane.blocks,
                             "state": str(lane.state)})
        print("Rex {} on {}, session {}".format(version[0] if version else "?", lane.lane.server,
                                                lane.session), flush=True)
        check_install(run, lane)
        activate(run, lane)
        for agent in ("claude", "codex"):
            name_from_palette(run, lane, agent)
        first = len(run.commands)
        exchange(run, lane, "claude", "codex", "pong")
        exchange(run, lane, "codex", "claude", "ping")
        check_exchange_commands(run, lane, run.commands[first:])
    except (Failed, RuntimeError, subprocess.SubprocessError, OSError, KeyError, ValueError,
            sqlite3.Error) as error:
        run.note("stopped: {!r}".format(error))
        print("stopped: {}".format(error), flush=True)
        print("RESULT FAIL", flush=True)
        return 1
    finally:
        run.save()
    print("RESULT PASS ({} checks)".format(len(run.checks)), flush=True)
    return 0


if __name__ == "__main__":
    sys.exit(main())
