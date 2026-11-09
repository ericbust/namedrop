"""Daily-use checks 6 to 8: a session compacted or restarted with a delivery open, the other
routes into a pane, and an empty backlog at the end."""

from collections import namedtuple
import json
import os
from pathlib import Path
import random
import re
import signal
import subprocess
import sys
import uuid

from livetest import AGENTS, Check, StepFailed, TIMEOUTS, fields
import livetest as live
import testbed
import dailyuse as du
import daily_channels as dc
import daily_coordination as dco

COMPACT_MARKERS = {"claude": '"subtype":"compact_boundary"', "codex": '"type":"compacted"'}
COMPACT_WAIT = 180
HELD_WAIT = 150
ANSWER_WAIT = 300
SHELL_SECONDS = {"compact": 100, "resume": 75}
"""Long enough for the turn to end and the step under test to finish before the shell answers: a
compaction, or the 30 s settle plus an exit and a resume. The clock starts mid-turn."""


def compactions(agent, lines):
    """How many compactions a host's transcript records."""
    return sum(COMPACT_MARKERS[agent] in line.replace(" ", "") for line in lines)


def transcript_lines(r, agent):
    return Path(r.transcript(agent).path).read_text(errors="replace").splitlines()


Hold = namedtuple("Hold", "agent word sent address attempt mark since baseline")


def start_hold(r, agent, purpose, rng):
    """A person asks the agent something it can only answer after a slow terminal command. Returns
    with the agent idle and its claim left:held, the one state where nothing is mid-turn and the
    delivery is still open."""
    channels, word = r.channels, dco.marker(rng)
    baseline = dc.baseline_of(channels.latest(), channels.peers())
    mark, since = channels.mark(), live.now_ms()
    sent = channels.post("@{} {}".format(agent, dco.SCRIPTED_HOLD_PROMPT))
    hop = r.settle(lambda: du.hop_for(r.store, sent.message_id), lambda found: found and found.attempt_id,
                   TIMEOUTS.label, watch=()) or du.NO_HOP
    if not hop.attempt_id:
        raise StepFailed(Check(agent + " hold", False, "{} hold: no delivery attempt for the post".format(agent)))
    dco.send_during_turn(r, agent, hop.attempt_id, "@shell sleep {}; echo {}".format(SHELL_SECONDS[purpose], word))
    claim = r.settle(lambda: r.store.claims().get(hop.attempt_id), lambda found: found not in (None, "open"),
                     HELD_WAIT)
    if claim != "left:held":
        raise StepFailed(Check(agent + " hold", False, "{} hold: inconclusive, claim {} instead of left:held".format(
            agent, claim)))
    return Hold(agent, word, sent, channels.address(agent), hop.attempt_id, mark, since, baseline)


def finish_hold(r, label, hold):
    """The helper answered: the way-back line shows, and the delivery settles once in its thread."""
    channels = r.channels
    r.settle(lambda: dc.replies_to(channels.latest().messages, hold.address, hold.sent.message_id),
             bool, ANSWER_WAIT)
    idle = r.wait_agent(hold.agent, ("idle", "done"), TIMEOUTS.idle)
    dc.listen(r, dc.QUIET)
    answers = dc.replies_to(channels.latest().messages, hold.address, hold.sent.message_id)
    final = du.hop_for(r.store, hold.sent.message_id) or du.NO_HOP
    transcript = r.settle(lambda: r.transcript(hold.agent),
                          lambda found: dco.way_back_turns(found, hold.attempt), TIMEOUTS.idle, watch=())
    interim = sorted({row.source_kind for frame in channels.frames_since(hold.mark) for row in frame.attention
                      if row.source_id == hold.attempt})
    open_now = any(row.source_id == hold.attempt for row in channels.latest().attention)
    r.save("{}.json".format(label.replace(" ", "-")),
           {"word": hold.word, "attempt": hold.attempt, "hop": final._asdict(),
            "answers": [row._asdict() for row in answers], "interim": interim})
    return dco.judge_late_answer(label, hold.word, final, answers, hold.sent.thread_id,
                                 dco.way_back_turns(transcript, hold.attempt), ", ".join(interim),
                                 not open_now) + dc.judge_stretch(
        r, label, hold.agent, hold.mark, hold.baseline, hold.since, (dco.HOLD_RED,)) + [
        live.expect(label + " idle", idle, True)]


def judge_compaction(label, before, after, claim_after, dispositions):
    return fields(label, [("compactions", after - before, 1), ("claim after it", claim_after, "left:held"),
                          ("dispositions", dispositions, 0)])


def run_compact(r, agent, rng):
    hold = start_hold(r, agent, "compact", rng)
    before = compactions(agent, transcript_lines(r, agent))
    # No --wait: a slash command may never show a working state, and the transcript is the proof.
    _, code = r.herdr_result("agent", "prompt", r.pane(agent), "/compact")
    if code:
        return [Check(agent + " compact", False, "{} compact: herdr returned {}".format(agent, code))]
    after = r.settle(lambda: compactions(agent, transcript_lines(r, agent)), lambda count: count > before,
                     COMPACT_WAIT, watch=())
    label = agent + " compacted"
    dc.listen(r, dc.QUIET)
    checks = [judge_compaction(label, before, after, r.store.claims().get(hold.attempt),
                               du.disposition_count(r.store, hold.attempt))]
    return checks + finish_hold(r, label, hold)


def agent_pids(r, agent):
    return [process["pid"] for process in r.foreground(agent)
            if Path((process.get("argv") or [""])[0]).name == agent]


def stop_agent(r, agent):
    """End the session as a person does, and wait for the pane's shell."""
    if agent == "claude":
        r.herdr_result("agent", "prompt", r.pane(agent), "/exit")
    else:
        r.keys(agent, ("c-c",))
    gone = r.settle(lambda: r.panes()[agent].agent, lambda found: found is None, TIMEOUTS.exit / 2, watch=())
    if gone is not None and agent == "codex":
        r.keys(agent, ("c-c",))
        gone = r.settle(lambda: r.panes()[agent].agent, lambda found: found is None, TIMEOUTS.exit / 2, watch=())
    if gone is not None:
        raise StepFailed(Check(agent + " exit", False, "{} still runs after exit".format(agent)))


def kill_agent(r, agent):
    """Kill the session with no chance to run a hook, as a crash does."""
    for pid in agent_pids(r, agent):
        os.kill(pid, signal.SIGKILL)
    gone = r.settle(lambda: r.panes()[agent].agent, lambda found: found is None, TIMEOUTS.exit, watch=())
    if gone is not None:
        raise StepFailed(Check(agent + " kill", False, "{} still runs after SIGKILL".format(agent)))


def resume_command(agent, mode, session, environ):
    base = live.launch_command(agent, mode, environ)
    return "{} --resume {}".format(base, session) if agent == "claude" else "{} resume {}".format(base, session)


def resume_agent(r, agent, session):
    r.profile.launch(agent, resume_command(agent, r.mode, session, os.environ))
    r.settle_screens(agent)
    # Codex tells Herdr its session only at its first turn, and nothing is delivered before that.
    if agent == "codex":
        r.prompt(agent, live.WARM_UP_PROMPT, TIMEOUTS.warm_up)
    return r.settle(lambda: r.panes()[agent].session, bool, TIMEOUTS.label, watch=())


def attempt_state(store, attempt):
    rows = store._rows("SELECT s.state, s.active, d.state FROM delivery_attempt_state s "
                       "JOIN delivery_attempts a USING (session_socket_id, worktree_id, attempt_id) "
                       "JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, delivery_id) "
                       "WHERE s.session_socket_id = ? AND s.worktree_id = ? AND s.attempt_id = ?",
                       (store.socket_id, store.workspace, attempt))
    return rows[0] if rows else (None, None, None)


def run_resume(r, agent, rng):
    """The clean exit: the attempt already settled, so the same session can still answer it."""
    hold = start_hold(r, agent, "resume", rng)
    r.settle(lambda: attempt_state(r.store, hold.attempt)[0], lambda state: state == "settled",
             TIMEOUTS.idle, watch=())
    session = r.panes()[agent].session
    stop_agent(r, agent)
    resumed = resume_agent(r, agent, session)
    label = agent + " resumed"
    checks = [fields(label, [("same session", resumed == session, True),
                             ("attempt at exit", attempt_state(r.store, hold.attempt)[0], "settled")])]
    return checks + finish_hold(r, label, hold)


CRASH_PROMPT = dc.SLOW_PROMPT


def judge_crash(label, held_state, attempts, answers, next_hop, item_kind):
    """A crash mid-turn holds the delivery. A resume can't answer it, so nothing doubles, the person
    sees it, and cancelling it releases the mailbox."""
    return fields(label, [("attempt after the crash", held_state[0], "held"),
                          ("mailbox after the crash", held_state[2], "held"),
                          ("attempts", attempts, 1), ("answers", answers, 0),
                          ("what the person sees", item_kind, "delivery_attempt"),
                          ("next message", "{}/{}".format(next_hop.state, next_hop.active), "settled/0")])


def run_crash(r, agent):
    channels = r.channels
    baseline = dc.baseline_of(channels.latest(), channels.peers())
    mark, since = channels.mark(), live.now_ms()
    sent = channels.post("@{} {}".format(agent, CRASH_PROMPT))
    hop = r.settle(lambda: du.hop_for(r.store, sent.message_id), lambda found: found and found.attempt_id,
                   TIMEOUTS.label, watch=()) or du.NO_HOP
    dc.wait_slow_tool(r, agent, hop.attempt_id)
    session = r.panes()[agent].session
    kill_agent(r, agent)
    state = r.settle(lambda: attempt_state(r.store, hop.attempt_id), lambda found: found[0] == "held",
                     TIMEOUTS.idle, watch=())
    items = du.open_items(r.store)
    item_kind = next((kind for kind, source in items if source == hop.attempt_id), None)
    resumed = resume_agent(r, agent, session)
    next_message = channels.post("@{} {}".format(agent, dc.NEXT_PROMPT))
    waiting = du.hop_for(r.store, next_message.message_id) or du.NO_HOP
    result = r.profile.namedrop("delivery", "cancel-held", "--input-json",
                                json.dumps({"attempt_id": hop.attempt_id, "confirmed": True}))
    after = du.wait_closed(r, next_message.message_id)
    r.wait_agent(agent, ("idle", "done"), TIMEOUTS.idle)
    answers = dc.replies_to(channels.latest().messages, channels.address(agent), sent.message_id)
    final = du.hop_for(r.store, sent.message_id) or du.NO_HOP
    r.save(agent + "-crash.json", {"held": list(state), "items": list(items), "waiting": waiting._asdict(),
                                   "cancel": result.stdout[-400:], "after": after._asdict(), "final": final._asdict()})
    label = agent + " crashed mid-turn"
    checks = dc.judge_stretch(r, label, agent, mark, baseline, since,
                              (("delivery_attempt", "a crash mid-turn is real, and the person has to cancel it"),))
    return checks + [judge_crash(label, state, final.attempts, len(answers), after, item_kind),
            fields(label + " resume", [("same session", resumed == session, True),
                                       ("cancel-held exit", result.returncode, 0),
                                       ("delivery after cancel", attempt_state(r.store, hop.attempt_id)[0],
                                        "cancelled"),
                                       ("attention left", sum(source == hop.attempt_id
                                                              for _, source in du.open_items(r.store)), 0)])]


def check_continuity(r):
    """Check 6: compact with a delivery open, exit and resume, crash and resume."""
    rng = random.Random()
    checks = []
    scenarios = [(run_compact, agent, (rng,)) for agent in AGENTS]
    scenarios += [(run_resume, agent, (rng,)) for agent in AGENTS]
    scenarios += [(run_crash, agent, ()) for agent in AGENTS]
    for run, agent, extra in scenarios:
        try:
            checks += run(r, agent, *extra)
        except du.RECOVERABLE as error:
            checks.append(Check("{} {}".format(agent, run.__name__), False, str(error)))
            checks += du.quiesce(r)
    return checks


STEPS = (
    du.check_step("continuity", "session continuity", check_continuity,
                  "A compacted session and an exited and resumed session each settled their open delivery once."),
)
