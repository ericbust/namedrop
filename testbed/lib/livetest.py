"""Run CHECKLIST.md against real Claude Code and Codex in a testbed profile.

`run()` is the entry point.
"""

import sys

sys.dont_write_bytecode = True

from collections import namedtuple
import datetime
import json
import os
from pathlib import Path
import re
import shlex
import shutil
import sqlite3
import time
import urllib.parse

import source as sources
import testbed

AGENTS = ("claude", "codex")

Timeouts = namedtuple("Timeouts", "screen exit warm_up banner label round_trip terminal idle "
                                  "blocked run")
TIMEOUTS = Timeouts(screen=45, exit=20, warm_up=150, banner=30, label=10, round_trip=300,
                    terminal=240, idle=60, blocked=30, run=20 * 60)

ANSWER, READY, FATAL, EXITED = "answer", "ready", "fatal", "exited"
"""Screen outcomes. EXITED is fatal only when Herdr sees no agent in the pane: the same
prompt above a running CLI is stale scrollback."""
RESTART_CODEX = "restart-codex"

Screen = namedtuple("Screen", "agent name pattern keys outcome after message")
"""One recognisable screen. Order in SCREENS is priority: a dialog wins over the composer it
covers. Each row is answered at most once per launch. Herdr's wait-output runs the patterns
too, one line at a time, so they stay line-local and in the subset Python `re` and Rust
`regex` share."""

KEYS = frozenset(("enter", "down", "esc", "c-c"))


def shell_prompt_pattern(ps1):
    """The profile's bash prompt as a line pattern. In a narrow pane the prompt wraps, and a
    line-local pattern sees only its tail, "/repo $"."""
    head, tail = ps1.split("{}")
    before_cwd, after_cwd = tail.split("\\w")
    def literal(text):
        return re.escape(text).replace("\\ ", " ")  # Rust's regex has no escaped space

    return "^{}[A-Za-z0-9._-]+{}[^$]*{} *$|/repo {} *$".format(
        literal(head), literal(before_cwd), literal(after_cwd.strip()), literal(after_cwd.strip()))


SHELL_PROMPT = shell_prompt_pattern(testbed.SHELL_PS1)

SCREENS = (
    # Claude Code 2.1.296. The seeds skip theme and trust; their rows catch a Claude that
    # ignores a seed.
    Screen("claude", "exited", SHELL_PROMPT, (), EXITED, None,
           "Claude exited to its shell; see the capture"),
    Screen("claude", "login", r"Select login method|Paste code here", (), FATAL, None,
           "Claude is not logged in; run ./testbed shell --profile {profile}, then claude auth login"),
    Screen("claude", "theme", r"Choose the text style that looks best", ("enter",), ANSWER, None, ""),
    Screen("claude", "security-notes", r"Security notes:", ("enter",), ANSWER, None, ""),
    Screen("claude", "terminal-setup", r"Use Claude Code's terminal setup\?", ("esc",), ANSWER,
           None, ""),
    # "No, exit" has the focus; a bare enter quits Claude.
    Screen("claude", "folder-trust", r"Quick safety check", ("down", "enter"), ANSWER, None, ""),
    # Claude draws a no-break space after the composer's arrow.
    Screen("claude", "ready", "^❯[ \u00a0]", (), READY, None, ""),
    # Codex 0.162.1.
    Screen("codex", "channel-closed", r"remote app-server worker channel is closed", (), FATAL,
           None, "Codex's shared app-server restarted under the window (it updates itself); "
                 "run the test again"),
    Screen("codex", "folder-trust", r"› ?(1\. )?Trust and continue", ("enter",), ANSWER, None, ""),
    Screen("codex", "folder-trust-unfocused", r"Trust this folder\?", (), FATAL, None,
           "Codex asks to trust the folder although the seed trusts it, and 'Trust and "
           "continue' is not focused"),
    Screen("codex", "hooks-review", r"Hooks need review", ("down", "enter"), ANSWER,
           RESTART_CODEX, ""),
    Screen("codex", "exited", SHELL_PROMPT, (), EXITED, None, "Codex exited to its shell; see the capture"),
    # The composer shows while Codex still loads, and its hook review can follow it; the
    # footer's "<model> <effort> · <cwd>" line appears only once startup is done.
    Screen("codex", "ready", r"^  \S.* · [/~]", (), READY, None, ""),
)

# Constructs one of the two engines lacks or reads differently, inline flags (Python refuses
# global ones inside the union's groups), and anything that spans lines.
UNPORTABLE_REGEX = re.compile(r"\(\?<?[=!]|\(\?P?<|\\[ZzGbAn]|\\[1-9]|\\[pPN]\{|\\[ux]\{|"
                              r"\(\?[a-zA-Z-]|\[\\s\\S\]|\[\\S\\s\]")


def screens_for(agent):
    return tuple(row for row in SCREENS if row.agent == agent)


def union_pattern(rows):
    return "|".join("(?:{})".format(row.pattern) for row in rows)


def classify(text, rows):
    lines = text.splitlines()
    return next((row for row in rows if any(re.search(row.pattern, line) for line in lines)), None)


FirstRun = namedtuple("FirstRun", "agent answered ready_row restart")


SERVED, IN_PROCESS = "served", "in-process"

CodexMode = namedtuple("CodexMode", "name daemon_flag journey")
"""journey: the server states a run passes through, in order. The only transition, served to
in-process, is "apply Namedrop's fix and restart Codex"."""

MODES = {
    "plain": CodexMode("plain", False, (SERVED, IN_PROCESS)),
    "no-daemon": CodexMode("no-daemon", True, (IN_PROCESS,)),
}

ServerExpect = namedtuple("ServerExpect", "served issue_003 label banner registered auto_start")
"""What one state must show. None means "not asserted in this state"."""

EXPECT = {
    SERVED: ServerExpect(served=True, issue_003=True, label="@codex: see namedrop doctor",
                         banner=True, registered=None, auto_start=True),
    # The served session's banner stays in the pane's scrollback, so it is not asserted here.
    IN_PROCESS: ServerExpect(served=False, issue_003=False, label="@codex ready",
                             banner=None, registered=True, auto_start=None),
}

ServerView = namedtuple("ServerView", "daemon_pid pane_argv auto_start issue_003 issue_panes "
                                      "fix_steps label banner registered transport")
"""One observation. daemon_pid, pane_argv and auto_start are the oracle, read independently of
Namedrop; issue_003, issue_panes, fix_steps, label and banner are Namedrop's claims; transport
(Codex's own log) is evidence only."""

FIX_COMMANDS = ("codex features disable daemon_auto_start", "codex app-server daemon stop")
UNDO_COMMAND = "codex features enable daemon_auto_start"
STOP_COMMAND = FIX_COMMANDS[1]
BANNER = "Namedrop: Codex runs through its shared app-server"
NO_DAEMON = "--no-daemon"


def served(view):
    return view.daemon_pid is not None and NO_DAEMON not in view.pane_argv


def codex_command(base, mode):
    words = [word for word in shlex.split(base) if word != NO_DAEMON]
    if mode.daemon_flag:
        words.insert(1, NO_DAEMON)
    return shlex.join(words)


def launch_command(alias, mode, environ):
    if alias == "claude":
        return environ.get("TESTBED_CLAUDE_CMD") or "claude"
    return codex_command(environ.get("TESTBED_CODEX_CMD") or "codex", mode)


Features = namedtuple("Features", "version hook_markers server_warning")


def features(version_text):
    found = re.match(r"(\d+)\.(\d+)\.(\d+)", version_text)
    if not found:
        testbed.fail("cannot read the installed Namedrop version: " + version_text)
    version = tuple(int(part) for part in found.groups())
    return Features(version, version >= (1, 1, 0), version >= (1, 2, 0))


def version_text(feats):
    return ".".join(map(str, feats.version))


def choose_mode(requested, feats):
    if not feats.hook_markers:
        testbed.fail("Namedrop {} predates the hook markers of 1.1.0 that prove the hooks "
                     "loaded; follow CHECKLIST.md by hand".format(version_text(feats)))
    name = requested or ("plain" if feats.server_warning else "no-daemon")
    if name == "plain" and not feats.server_warning:
        testbed.fail("--codex plain needs Namedrop 1.2.0 or later: {} has no shared app-server "
                     "warning to assert, and mentions to a served Codex stay queued".format(
                         version_text(feats)))
    return MODES[name]


Mention = namedtuple("Mention", "sender target word prompt")
MENTIONS = (
    Mention("claude", "codex", "pong", "Ask @codex to reply with the single word pong."),
    Mention("codex", "claude", "ping", "Ask @claude to reply with the single word ping."),
)
TERMINAL_PROMPT = "Run `echo hello` on @shell and tell me its output and exit code."
# Neutral on purpose: Codex's approval reviewer reads the last human prompt, and a warm-up
# that asks for one word made it reject a later Namedrop reply as off-request.
WARM_UP_PROMPT = "hi"


Check = namedtuple("Check", "what ok detail")


def expect(what, observed, expected):
    detail = "{} {}".format(what, observed)
    return Check(what, observed == expected,
                 detail if observed == expected else detail + " (expected {})".format(expected))


def fields(what, pairs):
    """One Check over (label, observed, expected) pairs. Expected None is only shown; a
    frozenset expected means any one of its members."""
    parts, ok = [], True
    for label, observed, wanted in pairs:
        part = "{} {}".format(label, observed)
        if isinstance(wanted, frozenset):
            if observed not in wanted:
                ok = False
                part += " (expected {})".format(" or ".join(sorted(wanted)))
        elif wanted is not None and observed != wanted:
            ok = False
            part += " (expected {})".format(wanted)
        parts.append(part)
    return Check(what, ok, "{}: {}".format(what, ", ".join(parts)))


class StepFailed(Exception):
    def __init__(self, check, checks=()):
        Exception.__init__(self, check.detail)
        self.check = check
        self.checks = tuple(checks)


Step = namedtuple("Step", "key title checklist action outcome")
"""key names the evidence subdirectory; checklist names the CHECKLIST.md steps it replaces;
outcome is the docs fragment the step contributes when it passes."""


class StepResult(namedtuple("StepResult", "step checks seconds")):
    @property
    def status(self):
        if self.checks is None:
            return "skip"
        return "ok" if self.checks and all(check.ok for check in self.checks) else "FAIL"


class Report(namedtuple("Report", "profile mode feats results compat evidence archived")):
    """compat is None unless every step passed: a failed run is never recorded as tested."""

    @property
    def passed(self):
        return bool(self.results) and all(result.status == "ok" for result in self.results)


def outcome_sentence(results):
    return " ".join(result.step.outcome for result in results
                    if result.status == "ok" and result.step.outcome)


def duration(seconds):
    return "{}m{:02d}s".format(int(seconds) // 60, int(seconds) % 60)


def report_json(report):
    return {
        "passed": report.passed,
        "profile": report.profile,
        "mode": report.mode.name if report.mode else None,
        "namedrop": version_text(report.feats) if report.feats else None,
        "compat": report.compat,
        "evidence": str(report.evidence),
        "archived": str(report.archived) if report.archived else None,
        "steps": [{"key": result.step.key, "title": result.step.title,
                   "checklist": result.step.checklist, "status": result.status,
                   "seconds": round(result.seconds, 1),
                   "checks": [check._asdict() for check in result.checks or ()]}
                  for result in report.results],
    }


def step_dir(evidence, number, step):
    return Path(evidence) / "{:02d}-{}".format(number, step.key)


def render(report):
    width = max(len(result.step.title) for result in report.results)
    lines = ["./testbed test: {}, Namedrop {}, Codex {}".format(
        report.profile, version_text(report.feats) if report.feats else "unknown",
        report.mode.name if report.mode else "unknown")]
    indent = " " * (10 + width)
    for number, result in enumerate(report.results, 1):
        head = "{:<4} {:>2} {:<{}}  ".format(result.status, number, result.step.title, width)
        if result.status == "skip":
            lines.append(head.rstrip())
        elif result.status == "ok":
            lines.append(head + "; ".join(check.detail for check in result.checks))
        else:
            failing = [check.detail for check in result.checks if not check.ok] or ["no checks ran"]
            lines.append(head + failing[0])
            lines.extend(indent + detail for detail in failing[1:])
            lines.append(indent + "evidence: {}/".format(step_dir(report.evidence, number, result.step)))
    total = duration(sum(result.seconds for result in report.results))
    done = sum(result.status == "ok" for result in report.results)
    if report.passed:
        lines.append("PASS {}/{} in {}. Report: {}".format(done, len(report.results), total,
                                                          report.archived))
        lines.append(report.compat)
    else:
        failed = next((n for n, r in enumerate(report.results, 1) if r.status == "FAIL"),
                      len(report.results))
        lines.append("FAIL {}/{} after {}. Evidence: {}/".format(failed, len(report.results),
                                                                total, report.evidence))
        lines.append("Profile kept: ./testbed attach --profile {0}, then ./testbed destroy "
                     "--profile {0} --yes".format(report.profile))
    return "\n".join(lines)


def claude_state_seed(state, repo, claude_version):
    seeded = dict(state, hasCompletedOnboarding=True, lastOnboardingVersion=claude_version)
    projects = dict(state.get("projects") or {})
    projects[repo] = dict(projects.get(repo) or {}, hasTrustDialogAccepted=True)
    seeded["projects"] = projects
    return seeded


def claude_settings_seed(settings):
    return settings if "theme" in settings else dict(settings, theme="dark")


def codex_trust_seed(text, repo):
    header = '[projects."{}"]'.format(repo)
    if re.search(r"(?m)^[ \t]*" + re.escape(header), text):
        return text
    separator = "" if not text or text.endswith("\n") else "\n"
    return text + separator + header + '\ntrust_level = "trusted"\n'


def toml_bool(text, table, key, default):
    current = None
    for line in text.splitlines():
        stripped = line.strip()
        if stripped.startswith("["):
            current = stripped.strip("[]").strip()
            continue
        found = re.match(r"{}\s*=\s*(true|false)\b".format(re.escape(key)), stripped)
        if current == table and found:
            return found.group(1) == "true"
    return default


MARKERS = {
    "policy": "Namedrop selection policy.",
    "roster": "Namedrop workspace roster. Current named agents",
    "verified": "Namedrop verification:",
    "commands": "Namedrop commands:",
}
# Codex never receives the attestation (Namedrop src/hook.rs), only the commands block that
# is rendered after the envelope authenticated.
PROOF = {"claude": "verified", "codex": "commands"}

Turn = namedtuple("Turn", "prompt contexts answers")
"""prompt: the submitted text, a human prompt or a pasted envelope. contexts: the hook texts
attached to it. answers: the assistant texts until the next prompt."""

Transcript = namedtuple("Transcript", "agent session path session_contexts turns models")
"""session_contexts: hook texts before the first prompt, which is where SessionStart lands."""


def markers(text):
    return frozenset(key for key, marker in MARKERS.items()
                     if re.search("(?m)^" + re.escape(marker), text))


def json_lines(lines):
    for line in lines:
        try:
            entry = json.loads(line)
        except ValueError:
            continue
        if isinstance(entry, dict):
            yield entry


class TranscriptBuilder:
    def __init__(self):
        self.session_contexts, self.turns, self.models = [], [], set()

    def prompt(self, text):
        self.turns.append(Turn(text, [], []))

    def context(self, text):
        (self.turns[-1].contexts if self.turns else self.session_contexts).append(text)

    def answer(self, text):
        if self.turns and text:
            self.turns[-1].answers.append(text)

    def build(self, agent, session, path):
        turns = tuple(Turn(t.prompt, tuple(t.contexts), tuple(t.answers)) for t in self.turns)
        return Transcript(agent, session, path, tuple(self.session_contexts), turns,
                          frozenset(self.models))


def claude_prompt_text(entry):
    """The text of a submitted prompt, or None for tool results and injected skill text."""
    if entry.get("isMeta") or "toolUseResult" in entry:
        return None
    content = (entry.get("message") or {}).get("content")
    if isinstance(content, str):
        return content
    if isinstance(content, list) and content and all(
            isinstance(block, dict) and block.get("type") == "text" for block in content):
        return "\n".join(block.get("text") or "" for block in content)
    return None


def parse_claude(lines, session, path):
    built = TranscriptBuilder()
    for entry in json_lines(lines):
        kind = entry.get("type")
        if kind == "attachment":
            attachment = entry.get("attachment") or {}
            if attachment.get("type") == "hook_additional_context":
                for text in attachment.get("content") or ():
                    if isinstance(text, str):
                        built.context(text)
        elif kind == "user":
            text = claude_prompt_text(entry)
            if text is not None:
                built.prompt(text)
        elif kind == "assistant":
            message = entry.get("message") or {}
            if message.get("model") and not message["model"].startswith("<"):
                built.models.add(message["model"])
            content = message.get("content")
            if isinstance(content, list):
                built.answer("".join(block.get("text") or "" for block in content
                                     if isinstance(block, dict) and block.get("type") == "text"))
    return built.build("claude", session, path)


def parse_codex(lines, session, path):
    """User messages before the first turn_context are Codex's own preamble (AGENTS.md and the
    environment), not prompts."""
    built = TranscriptBuilder()
    in_turns = False
    for entry in json_lines(lines):
        payload = entry.get("payload") or {}
        if entry.get("type") == "turn_context":
            in_turns = True
            if payload.get("model"):
                built.models.add(payload["model"])
        elif entry.get("type") == "response_item" and payload.get("type") == "message":
            text = "\n".join(item.get("text") or "" for item in payload.get("content") or ()
                             if isinstance(item, dict))
            role = payload.get("role")
            if role == "user" and in_turns:
                built.prompt(text)
            elif role == "developer":
                built.context(text)
            elif role == "assistant":
                built.answer(text)
    return built.build("codex", session, path)


def strip_markdown(text):
    return re.sub(r"[*`_\"'“”‘’]", "", text)


def last_answer(transcript):
    for turn in reversed(transcript.turns):
        if turn.answers:
            return turn.answers[-1]
    return ""


def has_word(word, text):
    return re.search(r"(?i)\b" + re.escape(word) + r"\b", strip_markdown(text)) is not None


def judge_hooks(agent, transcript):
    policy = sum("policy" in markers(text) for text in transcript.session_contexts)
    first = transcript.turns[0] if transcript.turns else Turn("", (), ())
    you = "- @{} (you)".format(agent)
    roster = any("roster" in markers(text) and you in text for text in first.contexts)
    return [fields("{} hooks".format(agent),
                   [("policy", policy, 1), ("first-turn roster with (you)", roster, True)])]


Hop = namedtuple("Hop", "message_id author recipient in_reply_to deliveries attempts retries "
                        "acks attempt_id state active disposition")
"""One message of a thread with its raw delivery counts. attempt_id, state, active and
disposition belong to its first attempt; disposition is None until the recipient settles it."""

Exchange = namedtuple("Exchange", "root_count hops")

TerminalRun = namedtuple("TerminalRun", "command exit_code output completed")
Backlog = namedtuple("Backlog", "awaiting open_attention active_attempts")
TERMINAL_STATES = ("settled", "failed", "cancelled")


def exchange_done(exchange):
    return (exchange.root_count == 1 and len(exchange.hops) >= 2 and
            all(hop.attempt_id and hop.state in TERMINAL_STATES and hop.active == 0
                for hop in exchange.hops))


def seen(transcript, attempt_id, proof):
    turns = [turn for turn in transcript.turns if attempt_id and attempt_id in turn.prompt]
    proven = sum(proof in markers("\n".join(turn.contexts)) for turn in turns)
    return len(turns), proven


def answered(transcript, attempt_id):
    """True once the turn that carried attempt_id has an answer. The store settles when the
    agent runs its reply or complete command, which is before its closing line reaches the
    transcript, so a judge that reads the last answer right after settlement sees the turn
    before."""
    return any(attempt_id and attempt_id in turn.prompt and turn.answers
               for turn in transcript.turns)


def judge_hop(label, hop, author, recipient, parent, dispositions):
    return fields(label, [("from", hop.author, author), ("to", hop.recipient, recipient),
                          ("in reply to", hop.in_reply_to, parent),
                          ("deliveries", hop.deliveries, 1), ("attempts", hop.attempts, 1),
                          ("retries", hop.retries, 0), ("acks", hop.acks, 1),
                          ("state", "{}/{}".format(hop.state, hop.active), "settled/0"),
                          ("disposition", hop.disposition, dispositions)])


def judge_exchange(mention, exchange, sender_tx, target_tx):
    sender, target = mention.sender, mention.target
    checks = [fields("thread", [("roots", exchange.root_count, 1),
                                ("messages", len(exchange.hops), 2)])]
    if exchange.root_count != 1 or len(exchange.hops) < 2:
        return checks
    request, reply = exchange.hops[0], exchange.hops[1]
    checks.append(judge_hop("request", request, sender, target, None, frozenset({"reply"})))
    checks.append(judge_hop("reply", reply, target, sender, request.message_id,
                            frozenset({"reply", "no_reply"})))
    for label, transcript, hop in (("{} saw the request".format(target), target_tx, request),
                                   ("{} saw the reply".format(sender), sender_tx, reply)):
        times, proven = seen(transcript, hop.attempt_id, PROOF[transcript.agent])
        checks.append(fields(label, [("times", times, 1),
                                     (MARKERS[PROOF[transcript.agent]], proven, 1)]))
    answer = last_answer(sender_tx)
    checks.append(Check("{}'s answer".format(sender), has_word(mention.word, answer),
                        '{} answered "{}"'.format(sender, " ".join(answer.split())[:120]) +
                        ("" if has_word(mention.word, answer) else
                         " (expected the word {})".format(mention.word))))
    return checks


def judge_terminal(runs, claude_tx):
    run = runs[0] if len(runs) == 1 else TerminalRun(None, None, "", False)
    checks = [fields("terminal", [("runs", len(runs), 1), ("completed", run.completed, True),
                                  ("exit", run.exit_code, 0),
                                  ("hello in output", "hello" in (run.output or ""), True)])]
    answer = last_answer(claude_tx)
    ok = has_word("hello", answer) and has_word("0", answer)
    checks.append(Check("claude's answer", ok, 'claude answered "{}"'.format(
        " ".join(answer.split())[:120]) + ("" if ok else " (expected hello and 0)")))
    return checks


def judge_settled(backlog, doctor, panes):
    awaiting = sorted("{} {}".format(pane.alias, pane.tokens.get("namedrop-awaiting"))
                      for pane in panes.values() if pane.tokens.get("namedrop-awaiting") != "0")
    states = {alias: panes[alias].status for alias in AGENTS}
    return [fields("backlog", [("awaiting", backlog.awaiting, 0),
                               ("attention", backlog.open_attention, 0),
                               ("active", backlog.active_attempts, 0),
                               ("doctor waiting", doctor.waiting, 0),
                               ("panes awaiting", ", ".join(awaiting) or "none", "none")]),
            fields("agents", [(alias, status, frozenset({"idle", "done"}))
                              for alias, status in states.items()])]


def judge_server(view, expect_row, codex_pane):
    oracle = served(view)
    pairs = [("served", oracle, expect_row.served),
             ("doctor ND-SESSION-003", view.issue_003, oracle)]
    if expect_row.issue_003:
        pairs += [("003 panes", view.issue_panes, (codex_pane,)),
                  ("003 first steps", view.fix_steps[:2], FIX_COMMANDS)]
    for label, observed, wanted in (("label", view.label, expect_row.label),
                                    ("banner", view.banner, expect_row.banner),
                                    ("registered", view.registered, expect_row.registered),
                                    ("daemon_auto_start", view.auto_start, expect_row.auto_start)):
        if wanted is not None:
            pairs.append((label, observed, wanted))
    pairs.append(("transport", view.transport, None))
    return [fields("codex server", pairs)]


Issue = namedtuple("Issue", "code severity diagnostic")
Doctor = namedtuple("Doctor", "exit status issues waiting")
"""exit: 0 healthy, 1 needs attention, 2 unavailable. The outer JSON "status" is "ok" even when
attention is needed, so it is not kept."""

Pane = namedtuple("Pane", "alias pane_id agent status session label tokens")
"""agent None when Herdr detects no CLI; label is state_labels["idle"], where Namedrop
publishes its hint (the title is always @alias)."""

Address = namedtuple("Address", "alias pane_type registered session selection")


def last_json(output):
    for line in reversed((output or "").splitlines()):
        if line.startswith("{"):
            try:
                return json.loads(line)
            except ValueError:
                break
    testbed.fail("expected a JSON line from Namedrop, got: " + (output or "")[-500:])


def parse_doctor(exit_code, output):
    data = last_json(output).get("data") or {}
    issues = tuple(Issue(issue.get("code"), issue.get("severity"), issue.get("diagnostic") or {})
                   for issue in data.get("issues") or ())
    return Doctor(exit_code, data.get("status"), issues, data.get("waiting_messages"))


def parse_panes(result, aliases):
    rows = {row.get("pane_id"): row for row in (result or {}).get("panes") or ()}
    panes = {}
    for alias, pane_id in aliases.items():
        row = rows.get(pane_id)
        if row is None:
            panes[alias] = Pane(alias, pane_id, None, "missing", None, None, {})
            continue
        panes[alias] = Pane(alias, pane_id, row.get("agent"), row.get("agent_status"),
                            (row.get("agent_session") or {}).get("value"),
                            (row.get("state_labels") or {}).get("idle"), row.get("tokens") or {})
    return panes


def parse_addresses(output):
    return {row["alias"]: Address(row["alias"], row.get("pane_type"), row.get("registered"),
                                  row.get("agent_session_id"), row.get("selection"))
            for row in last_json(output).get("data") or ()}


class StoreBusy(RuntimeError):
    pass


HOP_SQL = """
SELECT m.message_id, m.author_alias_snapshot,
  (SELECT r.alias_snapshot FROM message_recipients r WHERE r.session_socket_id = m.session_socket_id
     AND r.worktree_id = m.worktree_id AND r.message_id = m.message_id ORDER BY r.recipient_order LIMIT 1),
  m.in_reply_to,
  (SELECT count(*) FROM mailbox_deliveries d WHERE d.session_socket_id = m.session_socket_id
     AND d.worktree_id = m.worktree_id AND d.message_id = m.message_id),
  (SELECT count(*) FROM delivery_attempts a JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, delivery_id)
     WHERE d.session_socket_id = m.session_socket_id AND d.worktree_id = m.worktree_id AND d.message_id = m.message_id),
  (SELECT count(*) FROM delivery_attempts a JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, delivery_id)
     WHERE d.session_socket_id = m.session_socket_id AND d.worktree_id = m.worktree_id AND d.message_id = m.message_id
     AND a.retry_of_attempt_id IS NOT NULL),
  (SELECT count(*) FROM delivery_acknowledgements k JOIN delivery_attempts a USING (session_socket_id, worktree_id, attempt_id)
     JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, delivery_id)
     WHERE d.session_socket_id = m.session_socket_id AND d.worktree_id = m.worktree_id AND d.message_id = m.message_id),
  f.attempt_id, s.state, s.active, p.disposition_kind
FROM collaboration_messages m
LEFT JOIN delivery_attempts f ON f.attempt_id = (
  SELECT a.attempt_id FROM delivery_attempts a JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, delivery_id)
  WHERE d.session_socket_id = m.session_socket_id AND d.worktree_id = m.worktree_id AND d.message_id = m.message_id
    AND a.retry_of_attempt_id IS NULL ORDER BY a.created_at_ms LIMIT 1)
  AND f.session_socket_id = m.session_socket_id AND f.worktree_id = m.worktree_id
LEFT JOIN delivery_attempt_state s ON s.session_socket_id = f.session_socket_id
  AND s.worktree_id = f.worktree_id AND s.attempt_id = f.attempt_id
LEFT JOIN delivery_dispositions p ON p.session_socket_id = f.session_socket_id
  AND p.worktree_id = f.worktree_id AND p.attempt_id = f.attempt_id
WHERE m.session_socket_id = ? AND m.worktree_id = ?
  AND m.thread_id = (SELECT thread_id FROM collaboration_messages WHERE session_socket_id = ?
                     AND worktree_id = ? AND message_id = ?)
ORDER BY m.thread_order
"""


class Store:
    """Read-only view of Namedrop's SQLite store for one workspace of one profile.

    The store uses a rollback journal and its writer waits only 2 s for a lock, so every read
    opens its own `mode=ro` connection, runs one statement outside any transaction and closes.
    A busy store or a hot journal is retried, then raised as StoreBusy, never read as zeros.
    """

    def __init__(self, path, socket_id, workspace):
        self.path = path
        self.socket_id = socket_id
        self.workspace = workspace

    def _rows(self, sql, params=()):
        uri = "file:{}?mode=ro".format(urllib.parse.quote(str(self.path)))
        last = None
        for attempt in range(4):
            if attempt:
                time.sleep(0.5)
            try:
                connection = sqlite3.connect(uri, uri=True, isolation_level=None, timeout=0.5)
                try:
                    return connection.execute(sql, params).fetchall()
                finally:
                    connection.close()
            except sqlite3.OperationalError as error:
                last = error
        raise StoreBusy("Namedrop's store stayed unreadable: {}".format(last))

    def exchange(self, sender, since_ms):
        scope = (self.socket_id, self.workspace)
        roots = [row[0] for row in self._rows(
            "SELECT message_id FROM collaboration_messages WHERE session_socket_id = ? AND "
            "worktree_id = ? AND author_alias_snapshot = ? AND in_reply_to IS NULL AND "
            "created_at_ms >= ? ORDER BY created_at_ms", scope + (sender, since_ms))]
        if len(roots) != 1:
            return Exchange(len(roots), ())
        return Exchange(1, tuple(Hop(*row) for row in self._rows(HOP_SQL, scope + scope + (roots[0],))))

    def terminal_runs(self, since_ms):
        return tuple(TerminalRun(command, exit_code, output, completed is not None)
                     for command, exit_code, output, completed in self._rows(
            "SELECT t.command, t.exit_code, t.output, t.completed_at_ms FROM terminal_executions t "
            "JOIN delivery_attempts a USING (session_socket_id, worktree_id, attempt_id) "
            "WHERE t.session_socket_id = ? AND t.worktree_id = ? AND a.created_at_ms >= ? "
            "ORDER BY a.created_at_ms", (self.socket_id, self.workspace, since_ms)))

    def backlog(self):
        scope = (self.socket_id, self.workspace)
        return Backlog(*self._rows(
            "SELECT (SELECT count(*) FROM mailbox_deliveries WHERE session_socket_id = ?1 AND "
            "worktree_id = ?2 AND state IN ('queued', 'dispatching', 'submitted', 'acknowledged')), "
            "(SELECT count(*) FROM attention_items WHERE session_socket_id = ?1 AND worktree_id = ?2 "
            "AND resolved_at_ms IS NULL), "
            "(SELECT count(*) FROM delivery_attempt_state WHERE session_socket_id = ?1 AND "
            "worktree_id = ?2 AND active = 1)", scope)[0])


def now_ms():
    return int(time.time() * 1000)


class Run:
    """One test run. Owns the evidence directory and every Herdr interaction; everything runs
    through Profile.run, Profile.herdr and Profile.namedrop, so the testbed's isolation holds.
    Pane ids are read from profile.state["panes"], their single owner."""

    def __init__(self, profile, mode, evidence):
        self.profile = profile
        self.mode = mode
        self.evidence = evidence
        self.step_dir = evidence
        self.deadline = time.monotonic() + TIMEOUTS.run
        self.store = Store(profile.dir / "state/herdr/plugins/namedrop/namedrop.sqlite3",
                           str(profile.socket), profile.state["workspace"])

    def save(self, name, content):
        path = self.step_dir / name
        path.write_text(content if isinstance(content, str) else
                        json.dumps(content, indent=2, default=str) + "\n")

    def budget(self, seconds):
        left = self.deadline - time.monotonic()
        if left <= 0:
            raise StepFailed(Check("run time", False, "the run used up its {} after the "
                                   "build".format(duration(TIMEOUTS.run))))
        return min(seconds, left)

    def pane(self, alias):
        return self.profile.state["panes"][alias]

    def panes(self):
        return parse_panes(self.profile.herdr("pane", "list", "--workspace",
                                              self.profile.state["workspace"]),
                           self.profile.state["panes"])

    def text(self, alias, source="visible", lines=None):
        command = ["herdr", "pane", "read", self.pane(alias), "--source", source]
        return self.profile.run(command + (["--lines", str(lines)] if lines else []),
                                check=False).stdout

    def herdr_result(self, *args):
        """(result, None) or (None, error code). Herdr reports failures as JSON with a code."""
        output = self.profile.run(["herdr"] + list(args), check=False).stdout
        try:
            reply = json.loads(output)
        except ValueError:
            return None, "unparsable output: " + output.strip()[-300:]
        if "error" in reply:
            return None, (reply["error"] or {}).get("code") or "error"
        return reply.get("result"), None

    def wait_text(self, alias, regex, seconds, source="visible"):
        _, code = self.herdr_result("pane", "wait-output", self.pane(alias), "--regex", regex,
                                    "--source", source,
                                    "--timeout", str(int(self.budget(seconds) * 1000)))
        if code not in (None, "timeout"):
            testbed.fail("herdr pane wait-output failed ({}) for {}".format(code, regex))
        return code is None

    def keys(self, alias, keys):
        if not set(keys) <= KEYS:
            testbed.fail("unproven key names: " + ", ".join(sorted(set(keys) - KEYS)))
        _, code = self.herdr_result("agent", "send-keys", self.pane(alias), *keys)
        if code:
            testbed.fail("herdr agent send-keys failed ({})".format(code))

    def prompt(self, alias, text, seconds):
        """Submit and wait for the agent's own turn to end; never re-submits, because a timeout
        does not prove the prompt was not delivered. A turn that ends within Herdr's first
        status poll reports agent_prompt_stalled, so stalled is left to the step's own evidence
        (a session, a DB row, a transcript turn) to judge."""
        _, code = self.herdr_result("agent", "prompt", self.pane(alias), text, "--wait",
                                    "--until", "idle", "--until", "done",
                                    "--timeout", str(int(self.budget(seconds) * 1000)))
        if code == "agent_prompt_stalled":
            with (self.step_dir / "prompts.txt").open("a") as log:
                log.write("{} {!r}: herdr saw no working state (agent_prompt_stalled)\n".format(alias, text))
        elif code:
            raise StepFailed(Check(alias + " prompt", False, "{} prompt {!r}: herdr returned {}".format(
                alias, text[:60], code)))

    def wait_agent(self, alias, states, seconds):
        args = []
        for state in states:
            args += ["--until", state]
        _, code = self.herdr_result("agent", "wait", self.pane(alias), *args,
                                    "--timeout", str(int(self.budget(seconds) * 1000)))
        return code is None

    def settle_screens(self, alias):
        rows = screens_for(alias)
        answered, dropped = [], set()
        for _ in range(len(rows) + 2):
            pending = [row for row in rows if row.name not in answered and row.name not in dropped]
            if not self.wait_text(alias, union_pattern(pending), TIMEOUTS.screen):
                if self.panes()[alias].agent is None:
                    raise StepFailed(Check(alias + " first run", False,
                                           alias + " exited to its shell"))
                screen = "\n".join(self.text(alias).rstrip().splitlines()[-15:])
                raise StepFailed(Check(alias + " first run", False,
                                       "unrecognised screen after {}:\n{}".format(
                                           ", ".join(answered) or "launch", screen)))
            row = classify(self.text(alias), pending)
            if row is None:
                continue
            if row.outcome == READY:
                return FirstRun(alias, tuple(answered), row.name,
                                any(r.after == RESTART_CODEX for r in rows if r.name in answered))
            if row.outcome == EXITED and self.panes()[alias].agent is not None:
                dropped.add(row.name)
                continue
            if row.outcome in (FATAL, EXITED):
                raise StepFailed(Check(alias + " first run", False, "{} ({})".format(
                    row.message.format(profile=self.profile.name), row.name)))
            self.keys(alias, row.keys)
            answered.append(row.name)
        raise StepFailed(Check(alias + " first run", False,
                               "first run did not reach the composer after " + ", ".join(answered)))

    def warm_up(self, alias):
        """One short turn so the hooks run; Codex registers its session only at its first turn."""
        self.prompt(alias, WARM_UP_PROMPT, TIMEOUTS.warm_up)
        return self.settle(lambda: self.panes()[alias].session, bool, TIMEOUTS.label, watch=())

    def codex(self, command):
        result = self.profile.run(shlex.split(command), cwd=str(self.profile.repo), check=False)
        with (self.step_dir / "codex-commands.txt").open("a") as log:
            log.write("$ {}\n{}[exit {}]\n".format(command, result.stdout, result.returncode))
        return result.returncode, result.stdout

    def daemon_pid(self):
        pid_file = self.profile.dir / "codex/app-server-daemon/daemon.pid"
        try:
            pid = int(json.loads(pid_file.read_text())["pid"])
        except (OSError, ValueError, KeyError, TypeError):
            return None
        command = self.profile.run(["ps", "-o", "command=", "-p", str(pid)], check=False).stdout
        return pid if "packages/app-server-daemon/releases/" in command else None

    def stop_daemon(self):
        code, output = self.codex(STOP_COMMAND)
        if code and "failed to connect" not in output:
            raise StepFailed(Check("daemon stop", False, "{} exited {}: {}".format(
                STOP_COMMAND, code, output.strip()[-200:])))
        pid = self.daemon_pid()
        if pid is not None:
            raise StepFailed(Check("daemon stop", False,
                                   "Codex's app-server still runs as pid {} after {}".format(
                                       pid, STOP_COMMAND)))

    def foreground(self, alias):
        info = self.profile.herdr("pane", "process-info", "--pane", self.pane(alias))
        return (info.get("process_info") or {}).get("foreground_processes") or []

    def restart_codex(self, stop_daemon):
        if stop_daemon:
            self.stop_daemon()
        # Codex quits on ctrl-c, but one that is still settling (just after its hook trust)
        # takes the first ctrl-c as an interrupt and quits on the second.
        for _ in range(2):
            if self.panes()["codex"].agent:
                self.keys("codex", ("c-c",))
            if self.settle(lambda: self.panes()["codex"].agent, lambda agent: agent is None,
                           TIMEOUTS.exit / 2, watch=()) is None:
                break
        else:
            raise StepFailed(Check("codex restart", False, "Codex still runs after two ctrl-c"))
        self.profile.launch("codex", launch_command("codex", self.mode, os.environ))
        first = self.settle_screens("codex")
        if first.restart:
            raise StepFailed(Check("codex hook trust", False,
                                   "Codex asked to review its hooks again after the restart, so "
                                   "the trust did not persist"))
        return first

    def transport(self, pid):
        uri = "file:{}?mode=ro".format(urllib.parse.quote(str(self.profile.dir / "codex/logs_2.sqlite")))
        try:
            connection = sqlite3.connect(uri, uri=True, isolation_level=None, timeout=0.5)
            try:
                row = connection.execute(
                    "SELECT feedback_log_body FROM logs WHERE process_uuid LIKE ? AND "
                    "feedback_log_body LIKE '%rpc.transport=%' ORDER BY id DESC LIMIT 1",
                    ("pid:{}:%".format(pid),)).fetchone()
            finally:
                connection.close()
        except sqlite3.Error:
            return None
        found = re.search(r'rpc\.transport="?([\w-]+)', row[0]) if row else None
        return found.group(1) if found else None

    def observe_server(self, look_for_banner):
        doctor = self.doctor("server")
        issue = next((issue for issue in doctor.issues if issue.code == "ND-SESSION-003"), None)
        processes = [process for process in self.foreground("codex")
                     if Path((process.get("argv") or [""])[0]).name == "codex"]
        argv = tuple(processes[0].get("argv") or ()) if processes else ()
        config = self.profile.dir / "codex/config.toml"
        view = ServerView(
            daemon_pid=self.daemon_pid(), pane_argv=argv,
            auto_start=toml_bool(config.read_text() if config.exists() else "", "features",
                                 "daemon_auto_start", True),
            issue_003=issue is not None,
            issue_panes=tuple(issue.diagnostic.get("pane_ids") or ()) if issue else (),
            fix_steps=tuple(issue.diagnostic.get("steps") or ()) if issue else (),
            label=self.panes()["codex"].label,
            banner=self.wait_text("codex", BANNER, TIMEOUTS.banner, source="recent-unwrapped")
            if look_for_banner else None,
            registered=(self.addresses().get("codex") or Address("codex", None, None, None, None)).registered,
            transport=self.transport(processes[0]["pid"]) if processes else None)
        self.save("server.json", view._asdict())
        return view

    def apply_fix(self):
        doctor = self.doctor("before-fix")
        issue = next((issue for issue in doctor.issues if issue.code == "ND-SESSION-003"), None)
        steps = tuple(issue.diagnostic.get("steps") or ()) if issue else ()
        pairs = [("doctor's first two steps", steps[:2] == FIX_COMMANDS, True)]
        if steps[:2] != FIX_COMMANDS:
            raise StepFailed(fields("fix", pairs + [("steps", steps[:2], None)]))
        for command in FIX_COMMANDS:
            code, _ = self.codex(command)
            pairs.append(("exit of " + command, code, 0))
            if code:
                raise StepFailed(fields("fix", pairs))
        self.restart_codex(stop_daemon=False)
        session = self.warm_up("codex")
        pairs.append(("new codex session", session is not None, True))
        return [fields("fix", pairs)]

    def doctor(self, label):
        """Call only at checkpoints: every run integrity-checks the whole store."""
        result = self.profile.namedrop("doctor", "--json")
        self.save("doctor-{}.json".format(label), result.stdout)
        return parse_doctor(result.returncode, result.stdout)

    def addresses(self):
        result = self.profile.namedrop("address", "list", "--json")
        self.save("addresses.json", result.stdout)
        return parse_addresses(result.stdout)

    def transcript(self, alias):
        session = self.panes()[alias].session
        if not session:
            raise StepFailed(Check(alias + " transcript", False, alias + " has no session in Herdr"))
        if alias == "claude":
            found = sorted((self.profile.dir / "home/.claude/projects").glob("*/{}.jsonl".format(session)))
        else:
            found = sorted((self.profile.dir / "codex/sessions").glob(
                "*/*/*/rollout-*-{}.jsonl".format(session)))
        if len(found) != 1:
            raise StepFailed(Check(alias + " transcript", False, "{} transcripts for {} session {}: {}".format(
                len(found), alias, session, ", ".join(map(str, found)) or "none")))
        parse = parse_claude if alias == "claude" else parse_codex
        transcript = parse(found[0].read_text(errors="replace").splitlines(), session, found[0])
        self.save("transcript-{}.json".format(alias), {
            "path": str(found[0]), "session": session, "models": sorted(transcript.models),
            "session_contexts": [sorted(markers(text)) for text in transcript.session_contexts],
            "turns": [{"prompt": turn.prompt[:300], "contexts": [sorted(markers(t)) for t in turn.contexts],
                       "answer": turn.answers[-1][:300] if turn.answers else None}
                      for turn in transcript.turns]})
        return transcript

    def settle(self, read, done, seconds, watch=AGENTS):
        """The only poll loop: SQLite has no wait primitive. A watched pane that stays blocked
        for TIMEOUTS.blocked fails the step, because the driver never answers a tool prompt
        on an agent's behalf."""
        deadline = time.monotonic() + self.budget(seconds)
        blocked = {}
        while True:
            value = read()
            if done(value) or time.monotonic() >= deadline:
                return value
            if watch:
                panes = self.panes()
                for alias in watch:
                    if panes[alias].status != "blocked":
                        blocked.pop(alias, None)
                    elif time.monotonic() - blocked.setdefault(alias, time.monotonic()) > TIMEOUTS.blocked:
                        screen = "\n".join(self.text(alias).rstrip().splitlines()[-15:])
                        raise StepFailed(Check(alias + " blocked", False,
                                               "{} stayed blocked for {} s:\n{}".format(
                                                   alias, TIMEOUTS.blocked, screen)))
            time.sleep(1)

    def capture(self):
        for alias in self.profile.state["panes"]:
            self.save(alias + ".txt", self.text(alias, "recent-unwrapped", lines=600))

    def execute(self, step, number):
        self.step_dir = step_dir(self.evidence, number, step)
        self.step_dir.mkdir(parents=True, exist_ok=True)
        testbed.say("..  {:>2} {}".format(number, step.title))
        start = time.monotonic()
        try:
            checks = tuple(step.action(self))
        except StepFailed as failed:
            checks = failed.checks + (failed.check,)
        except (RuntimeError, OSError, KeyError, ValueError) as error:
            checks = (Check(step.title, False, "{}: {}".format(type(error).__name__, error)),)
        seconds = time.monotonic() - start
        try:
            self.capture()
        except (RuntimeError, OSError) as error:
            checks += (Check("capture", False, "pane capture failed: {}".format(error)),)
        self.save("checks.json", [check._asdict() for check in checks])
        return StepResult(step, checks, seconds)


BUILD = Step("build", "build", "1-2", None, "")


def step_build(profile, request, codex):
    start = time.monotonic()
    mode = feats = None
    try:
        saved = profile.state.get("source")
        reused = "seed_commit" in profile.state
        if reused and saved and saved["kind"] == "link":
            head = profile.run(["git", "-C", saved["path"], "rev-parse", "HEAD"]).stdout.strip()
            if head != saved["commit"]:
                testbed.fail("profile {} was built from {}; pass --fresh to test {}".format(
                    profile.name, saved["commit"][:7], head[:7]))
        profile.up(request, AGENTS, launch=False)
        if reused:
            profile.reset()
        feats = features(profile.namedrop_version())
        mode = choose_mode(codex, feats)
        named = [alias for alias in ("shell",) + AGENTS if profile.state.get("named_" + alias)]
        checks = (Check("namedrop", True, "Namedrop {} from {}".format(
                      version_text(feats), sources.describe(profile.state["source"]))),
                  expect("named", " ".join("@" + alias for alias in named), "@shell @claude @codex"),
                  expect("activated", profile.state.get("activated"), True))
    except (RuntimeError, OSError, KeyError, ValueError) as error:
        checks = (Check("build", False, str(error)),)
    return StepResult(BUILD, checks, time.monotonic() - start), mode, feats


def write_json(path, transform):
    current = json.loads(path.read_text()) if path.exists() else {}
    testbed.write_if_changed(path, json.dumps(transform(current), indent=2) + "\n")


def step_first_run(r):
    r.stop_daemon()
    if r.mode.name == "plain":
        code, output = r.codex(UNDO_COMMAND)
        if code:
            raise StepFailed(Check("undo", False, "{} exited {}".format(UNDO_COMMAND, code)))
    home, repo = r.profile.dir / "home", str(r.profile.repo.resolve())
    version = r.profile.tool_version("claude")
    write_json(home / ".claude.json", lambda state: claude_state_seed(state, repo, version))
    write_json(home / ".claude/settings.json", claude_settings_seed)
    config = r.profile.dir / "codex/config.toml"
    testbed.write_if_changed(config, codex_trust_seed(config.read_text() if config.exists() else "", repo))
    checks = []
    for alias in AGENTS:
        r.profile.launch(alias, launch_command(alias, r.mode, os.environ))
        first = r.settle_screens(alias)
        note = ""
        if first.restart:
            r.restart_codex(stop_daemon=True)
            note = ", restarted for trust"
        checks.append(Check(alias + " first run", True,
                            "{}: {}{}".format(alias, ", ".join(first.answered) or "no screens", note)))
    sessions = {alias: r.warm_up(alias) for alias in AGENTS}
    needed = AGENTS if r.mode.journey[0] != SERVED else ("claude",)
    for alias in needed:
        checks.append(Check(alias + " session", sessions[alias] is not None,
                            "{} session {}".format(alias, sessions[alias] or "missing after warm-up")))
    return checks


def step_codex_state(state, fix_first):
    def action(r):
        checks = r.apply_fix() if fix_first else []
        wanted = EXPECT[state]
        r.settle(lambda: r.panes()["codex"].label, lambda label: label == wanted.label,
                 TIMEOUTS.label, watch=())
        view = r.observe_server(look_for_banner=wanted.banner is True)
        checks += judge_server(view, wanted, r.pane("codex"))
        checks.append(expect("codex argv has --no-daemon", NO_DAEMON in view.pane_argv,
                             r.mode.daemon_flag))
        return checks
    return action


def step_names(r):
    addresses = r.addresses()
    doctor = r.doctor("names")
    rows = [("aliases", " ".join(sorted(addresses)), "claude codex shell"),
            ("shell type", (addresses.get("shell") or Address(*[None] * 5)).pane_type, "terminal")]
    for alias in AGENTS:
        address = addresses.get(alias) or Address(*[None] * 5)
        rows += [(alias + " registered", address.registered, True),
                 (alias + " selection", address.selection, "available now")]
    return [fields("names", rows),
            fields("doctor", [("exit", doctor.exit, 0), ("status", doctor.status, "healthy"),
                              ("issues", ", ".join(issue.code for issue in doctor.issues) or "none",
                               "none")])]


def step_hooks(r):
    """The hooks loaded, which doctor cannot show."""
    return [check for alias in AGENTS for check in judge_hooks(alias, r.transcript(alias))]


def step_mention(mention):
    def action(r):
        since = now_ms()
        r.prompt(mention.sender, mention.prompt, TIMEOUTS.warm_up)
        exchange = r.settle(lambda: r.store.exchange(mention.sender, since), exchange_done,
                            TIMEOUTS.round_trip)
        r.save("exchange.json", [hop._asdict() for hop in exchange.hops] or {"roots": exchange.root_count})
        idle = [alias for alias in AGENTS
                if not r.wait_agent(alias, ("idle", "done"), TIMEOUTS.idle)]
        if len(exchange.hops) > 1:
            r.settle(lambda: r.transcript(mention.sender),
                     lambda transcript: answered(transcript, exchange.hops[1].attempt_id),
                     TIMEOUTS.idle, watch=())
        checks = judge_exchange(mention, exchange, r.transcript(mention.sender),
                                r.transcript(mention.target))
        return checks + [expect("still working", ", ".join(idle) or "none", "none")]
    return action


def step_terminal(r):
    since = now_ms()
    r.prompt("claude", TERMINAL_PROMPT, TIMEOUTS.warm_up)
    runs, _ = r.settle(lambda: (r.store.terminal_runs(since), r.store.backlog()),
                       lambda value: any(run.completed for run in value[0]) and
                       value[1] == Backlog(0, 0, 0), TIMEOUTS.terminal)
    r.save("terminal.json", [run._asdict() for run in runs])
    r.wait_agent("claude", ("idle", "done"), TIMEOUTS.idle)
    return judge_terminal(runs, r.transcript("claude"))


def step_settled(r):
    return judge_settled(r.store.backlog(), r.doctor("final"), r.panes())


TITLES = {SERVED: "codex warning", IN_PROCESS: "codex in-process"}
OUTCOME_FOR = {"plain": "Codex's shared app-server warning appeared and cleared after its fix.",
               "no-daemon": "Codex ran with --no-daemon."}


def plan(mode):
    steps = [Step("first-run", "first run", "4-5", step_first_run, "")]
    for index, state in enumerate(mode.journey):
        title = "codex fix" if index else TITLES[state]
        steps.append(Step("codex-" + ("fix" if index else state), title, "6",
                          step_codex_state(state, fix_first=index > 0),
                          OUTCOME_FOR[mode.name] if index == len(mode.journey) - 1 else ""))
    return tuple(steps + [
        Step("names", "names", "6", step_names, ""),
        Step("hooks", "hooks", "", step_hooks, ""),
        Step("claude-to-codex", "claude to codex", "7", step_mention(MENTIONS[0]),
             "Mentions went both ways between Claude and Codex, and each reply arrived once."),
        Step("codex-to-claude", "codex to claude", "8", step_mention(MENTIONS[1]), ""),
        Step("terminal", "terminal", "9", step_terminal, "@shell ran a command for Claude."),
        Step("settled", "settled", "10", step_settled, ""),
    ])


def models_seen(r):
    models = {}
    for alias in AGENTS:
        try:
            found = r.transcript(alias).models
        except (StepFailed, RuntimeError, OSError):
            continue
        if found:
            models[alias] = " and ".join(sorted(found))
    return models


def run(profile, request, codex, keep):
    """Build, drive every step, report. True on a pass. A pass deletes a profile `test`
    created unless keep; a failure keeps the profile and its server for ./testbed attach."""
    stamp = datetime.datetime.now(datetime.timezone.utc).strftime("%Y%m%dT%H%M%SZ")
    evidence = profile.dir / "logs" / ("test-" + stamp)
    with profile.locked():
        evidence.mkdir(parents=True)
        build, mode, feats = step_build(profile, request, codex)
        results = [build]
        r = None
        if mode is not None:
            r = Run(profile, mode, evidence)
            try:
                for number, step in enumerate(plan(mode), 2):
                    if results[-1].status != "ok":
                        results.append(StepResult(step, None, 0))
                    else:
                        results.append(r.execute(step, number))
            except KeyboardInterrupt:
                results.append(StepResult(Step("interrupted", "interrupted", "", None, ""),
                                          (Check("run", False, "interrupted with Ctrl-C"),), 0))
        report = Report(profile.name, mode, feats, tuple(results), None, evidence, None)
        if report.passed:
            report = report._replace(
                compat=testbed.compat_line(profile, models_seen(r)) + " " + outcome_sentence(results),
                archived=testbed.testbed_home() / "reports" / "{}-{}.txt".format(profile.name, stamp))
        text = render(report)
        testbed.say(text)
        (evidence / "report.txt").write_text(text + "\n")
        (evidence / "report.json").write_text(json.dumps(report_json(report), indent=2) + "\n")
        if report.passed:
            report.archived.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(str(evidence / "report.txt"), str(report.archived))
            shutil.copyfile(str(evidence / "report.json"), str(report.archived.with_suffix(".json")))
            if not keep and profile.state.get("purpose") == "test":
                profile.destroy()
                testbed.say("Deleted profile {} (pass --keep to keep it).".format(profile.name))
            else:
                testbed.say("Kept profile {}: ./testbed destroy --profile {} --yes deletes it.".format(
                    profile.name, profile.name))
    return report.passed
