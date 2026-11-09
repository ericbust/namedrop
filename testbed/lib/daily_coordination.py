"""Daily-use checks 4 and 5: tracked work from handoff to report, and a turn held on a helper."""

from collections import namedtuple
import json
import random
import uuid

from livetest import AGENTS, Check, StepFailed, TIMEOUTS, fields
import livetest as live
import dailyuse as du
import daily_channels as dc

WORDS = ("amber", "cobalt", "fennel", "juniper", "saffron", "tundra", "walnut", "indigo")


def marker(rng=random):
    return "{}{}".format(rng.choice(WORDS), rng.randint(100, 999))


Coordination = namedtuple("Coordination", "coordination_id outcome report report_message_id wake_message_id")
Assignment = namedtuple("Assignment", "assignment_id recipient revision work_outcome")
Link = namedtuple("Link", "message_id revision kind work_outcome expected_revision")
Mail = namedtuple("Mail", "message_id grammar body state disposition attempt_id")
Tracked = namedtuple("Tracked", "coordination assignments links nudges reports report_replies")
"""One coordination's durable state. nudges and reports: Mail rows. report_replies: messages that
answer the report, which nothing should."""


def _rows(store, sql, *args):
    return store._rows(sql, (store.socket_id, store.workspace) + args)


def coordinations_since(store, since_ms):
    return tuple(row[0] for row in _rows(
        store, "SELECT coordination_id FROM coordinations WHERE session_socket_id = ? AND worktree_id = ? "
               "AND created_at_ms >= ? ORDER BY created_at_ms", since_ms))


MAIL_SQL = ("SELECT m.message_id, m.grammar_version, m.body, d.state, p.disposition_kind, a.attempt_id "
            "FROM collaboration_messages m "
            "LEFT JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, message_id) "
            "LEFT JOIN delivery_attempts a USING (session_socket_id, worktree_id, delivery_id) "
            "LEFT JOIN delivery_dispositions p USING (session_socket_id, worktree_id, attempt_id) "
            "WHERE m.session_socket_id = ? AND m.worktree_id = ? AND ")


def tracked_view(store, coordination_id):
    head = _rows(store, "SELECT coordination_id, outcome, report, report_message_id, wake_message_id "
                        "FROM coordinations WHERE session_socket_id = ? AND worktree_id = ? AND "
                        "coordination_id = ?", coordination_id)
    assignments = _rows(store, "SELECT assignment_id, recipient_address_id, revision, work_outcome "
                               "FROM coordination_assignments WHERE session_socket_id = ? AND "
                               "worktree_id = ? AND coordination_id = ? ORDER BY rowid", coordination_id)
    links = _rows(store, "SELECT l.message_id, l.revision, l.kind, "
                         "json_extract(l.input_json, '$.work_outcome'), "
                         "json_extract(l.input_json, '$.expected_revision') "
                         "FROM coordination_links l JOIN coordination_assignments a "
                         "USING (session_socket_id, worktree_id, assignment_id) "
                         "WHERE l.session_socket_id = ? AND l.worktree_id = ? AND a.coordination_id = ? "
                         "ORDER BY l.rowid", coordination_id)
    nudges = _rows(store, MAIL_SQL + "m.message_id LIKE ? AND m.grammar_version = 'coordination-nudge/v1'",
                   "coordination-wake:{}:%".format(coordination_id))
    reports = _rows(store, MAIL_SQL + "m.message_id = ?", "coordination-report:{}".format(coordination_id))
    replies = _rows(store, "SELECT message_id FROM collaboration_messages WHERE session_socket_id = ? AND "
                           "worktree_id = ? AND in_reply_to = ?",
                    "coordination-report:{}".format(coordination_id))
    return Tracked(Coordination(*head[0]) if head else None,
                   tuple(Assignment(*row) for row in assignments), tuple(Link(*row) for row in links),
                   tuple(Mail(*row) for row in nudges), tuple(Mail(*row) for row in reports),
                   tuple(row[0] for row in replies))


TRACKED_RED = ("coordination", "a tracked handoff's report reads as a red item until its delivery settles")
HOLD_RED = ("agent_disposition", "a turn held past 30 s reads as an agent that didn't answer until its answer lands")


def tracked_done(view):
    """Resolved, and the report's delivery has reached a final state."""
    return bool(view.coordination and view.coordination.outcome != "open" and view.reports and
                all(mail.state in live.TERMINAL_STATES for mail in view.reports))


def judge_tracked(label, view, word, report_turns, transcript):
    """report_turns: the lead's transcript turns that carried the report."""
    if view.coordination is None:
        return [Check(label, False, "{}: the lead never began tracked work".format(label))]
    links = [(link.kind, link.revision, link.work_outcome) for link in view.links]
    wanted = [("request", 1, None), ("reply", 1, "needs_continuation"),
              ("request", 2, None), ("reply", 2, "completed")]
    expected = next((link.expected_revision for link in view.links
                     if link.kind == "request" and link.revision == 2), None)
    assignment = view.assignments[0] if view.assignments else Assignment(*[None] * 4)
    final = report_turns[-1].answers[-1] if report_turns and report_turns[-1].answers else ""
    earlier = sum(word in answer for turn in transcript.turns if turn not in report_turns
                  for answer in turn.answers)
    return [fields(label, [("outcome", view.coordination.outcome, "completed"),
                           ("assignments", len(view.assignments), 1),
                           ("assignment revision", assignment.revision, 2),
                           ("work outcome", assignment.work_outcome, "completed"),
                           ("links", links, wanted),
                           ("continued with expected_revision", expected, 1),
                           ("report in the coordination", word in (view.coordination.report or ""), True)]),
            judge_report(label, view, word, report_turns, final),
            Check(label + " marker", True, "{}: the marker appears in {} earlier lead answers".format(
                label, earlier))]


def judge_report(label, view, word, report_turns, final):
    mail = view.reports[0] if view.reports else Mail(*[None] * 6)
    return fields(label + " report", [("rows", len(view.reports), 1),
                                      ("grammar", mail.grammar, "coordination-report/v1"),
                                      ("delivery", mail.state, "settled"),
                                      ("disposition", mail.disposition, "no_reply"),
                                      ("replies to it", len(view.report_replies), 0),
                                      ("shown in turns", len(report_turns), 1),
                                      ("marker in the shown report", word in final, True)])


def report_turns(r, alias, view):
    """The lead's turns that carried the report, once the host wrote their closing line."""
    attempt = view.reports[0].attempt_id if view.reports else None
    if not attempt:
        return ()
    key = du.prompt_key(r.store, attempt)
    transcript = r.settle(lambda: r.transcript(alias), lambda found: live.answered(found, key),
                          TIMEOUTS.idle, watch=())
    return tuple(live.delivery_turns(transcript, key))


TRACKED_WAIT = 420
TRACKED_PROMPT = (
    "Use Namedrop tracked coordination for this: run handoff.begin, then a tracked send to @codex. "
    "Deliverable: the word {word}. First assignment to @codex: say you have started, and end your "
    "final message with the line `work_outcome: needs_continuation`. When @codex replies, continue "
    "that same assignment with a tracked send that carries expected_revision: ask it for the word "
    "{word}, ending its final message with the line `work_outcome: completed`. When it replies, run "
    "handoff.resolve as completed with a report containing {word}, then show me that report once.")


def run_tracked(r, rng):
    word, since = marker(rng), live.now_ms()
    r.prompt("claude", TRACKED_PROMPT.format(word=word), TIMEOUTS.warm_up)
    found = r.settle(lambda: coordinations_since(r.store, since), bool, TIMEOUTS.idle)
    if not found:
        return [Check("tracked", False, "tracked: the lead began no coordination; it said: {}".format(
            " ".join(live.last_answer(r.transcript("claude")).split())[:200]))]
    view = r.settle(lambda: tracked_view(r.store, found[0]), tracked_done, TRACKED_WAIT)
    r.save("tracked.json", {"word": word, "view": view._asdict()})
    idle = [alias for alias in AGENTS if not r.wait_agent(alias, ("idle", "done"), TIMEOUTS.idle)]
    turns = report_turns(r, "claude", view)
    return judge_tracked("tracked", view, word, turns, r.transcript("claude")) + [
        live.expect("still working", ", ".join(idle) or "none", "none")]


NUDGE_BRIEF = (
    "For the next tracked work you lead in this session: when the agent named codex replies, do not "
    "read results and do not resolve; end that turn with the single word noted. Resolve only when "
    "Namedrop sends you a coordination nudge, as completed, with a report containing {word}. Nothing "
    "to do yet, just say ok.")
NUDGE_ASSIGNMENT = "@codex Reply with the single word {word}. End your final message with the line `work_outcome: completed`."


def as_pane(r, alias, operation, payload):
    """Run a Namedrop operation as the agent in that pane. The lane reads SQLite instead of `handoff
    results`, because a lead's read holds its nudge for 10 minutes."""
    result = r.profile.namedrop("--herdr-pane", r.pane(alias), "--json", *operation,
                                "--input-json", json.dumps(payload))
    if result.returncode:
        raise StepFailed(Check(" ".join(operation), False, "{} exited {}: {}".format(
            " ".join(operation), result.returncode, result.stdout[-300:])))
    return live.last_json(result.stdout).get("data") or {}


def run_nudge(r, rng):
    """The script begins the work, so the nudge's own turn is what the real lead is judged on."""
    word = marker(rng)
    r.prompt("claude", NUDGE_BRIEF.format(word=word), TIMEOUTS.warm_up)
    begun = as_pane(r, "claude", ("handoff", "begin"),
                    {"request_id": str(uuid.uuid4()), "deliverable": "The word {}.".format(word)})
    coordination = begun["coordination_id"]
    as_pane(r, "claude", ("send",), {"request_id": str(uuid.uuid4()), "author_kind": "agent_session",
                                     "coordination_id": coordination,
                                     "source": NUDGE_ASSIGNMENT.format(word=word)})
    view = r.settle(lambda: tracked_view(r.store, coordination), tracked_done, TRACKED_WAIT)
    r.save("nudge.json", {"word": word, "view": view._asdict()})
    idle = [alias for alias in AGENTS if not r.wait_agent(alias, ("idle", "done"), TIMEOUTS.idle)]
    turns = report_turns(r, "claude", view)
    final = turns[-1].answers[-1] if turns and turns[-1].answers else ""
    nudge = view.nudges[0] if view.nudges else Mail(*[None] * 6)
    return [fields("nudge", [("nudges", len(view.nudges), 1),
                             ("delivery", nudge.state, "settled"),
                             ("disposition", nudge.disposition, None),
                             ("outcome", view.coordination.outcome, "completed"),
                             ("work outcome", view.assignments[0].work_outcome if view.assignments else None,
                              "completed")]),
            judge_report("nudge", view, word, turns, final),
            live.expect("still working", ", ".join(idle) or "none", "none")]


def nothing_stuck(r, items_before):
    """Coordinations, mail and new attention still open after the tracked flows ended. Items from
    an earlier failed check are that check's finding. A report's own flag clears a moment after its
    delivery settles, so this waits for that."""
    store = r.store
    r.settle(lambda: set(du.open_items(store)) - set(items_before), lambda items: not items, TIMEOUTS.idle, watch=())
    work = _rows(store, "SELECT count(*) FROM coordinations WHERE session_socket_id = ? AND worktree_id = ? "
                        "AND outcome = 'open'")[0][0]
    return work, store.backlog(), len(set(du.open_items(store)) - set(items_before))


def check_tracked(r):
    """Check 4: lead, worker, continuation, resolve, report, and a nudge."""
    rng = random.Random()
    checks, items_before = [], du.open_items(r.store)
    baseline = dc.baseline_of(r.channels.latest(), r.channels.peers())
    mark, since = r.channels.mark(), live.now_ms()
    for run in (run_tracked, run_nudge):
        try:
            checks += run(r, rng)
        except du.RECOVERABLE as error:
            checks.append(Check(run.__name__, False, str(error)))
            checks += du.quiesce(r)
    open_work, backlog, new_items = nothing_stuck(r, items_before)
    checks += dc.judge_stretch(r, "tracked", "claude", mark, baseline, since, (TRACKED_RED,))
    return checks + [fields("tracked backlog", [("open coordinations", open_work, 0),
                                                ("awaiting", backlog.awaiting, 0),
                                                ("active", backlog.active_attempts, 0),
                                                ("new attention", new_items, 0)])]


Held = namedtuple("Held", "claim dispositions relays posts helper_requests")
"""What the store shows while the turn is held. posts: the holder's rows in the person's thread."""

HOLD_WAIT = 150
ANSWER_WAIT = 240
HOLD_PROMPT = ("Ask the agent named {helper}, using a new `namedrop send` and not your reply line, to run "
               "`sleep 30` in its shell and then reply with the single word {word}. End this turn with a "
               "one-line waiting status; do not poll or wait. When it answers, tell me the word.")
UNCOACHED_HOLD_PROMPT = (
    "Run this shell command in the foreground and wait for it: python3 -c 'import time; time.sleep(15)'. Then "
    "end this turn at once with a one-line waiting status. Don't wait or poll for anything else in this turn. "
    "A helper's answer holding a word arrives in a later turn; when it does, tell me the word.")
SCRIPTED_HOLD_PROMPT = (UNCOACHED_HOLD_PROMPT + " Tell me by running the reply line Namedrop shows you for this "
                        "delivery, with the word as its body.")
"""Told to use the way-back line, because a real Codex sometimes writes the answer as its final
message instead, which never reaches the person. check_uncoached_hold reports how that went."""
SCRIPTED_HELP = "@{helper} Run `sleep 60` in your shell, then reply with the single word {word}."
TOOL_WAIT = 60


def send_during_turn(r, agent, attempt, source):
    """Send as the agent while its delivery's turn runs a tool. A real Codex declines to `send` from a
    person's delivery, because `send` isn't in the envelope's permitted operations, so the store's
    hold has to be reached this way to test what Codex does next."""
    key = du.prompt_key(r.store, attempt)
    started = r.settle(lambda: any(turn.commands for turn in live.delivery_turns(r.transcript(agent), key)),
                       bool, TOOL_WAIT)
    if not started:
        raise StepFailed(Check(agent + " tool", False, "{} did not start its tool within {} s".format(agent, TOOL_WAIT)))
    as_pane(r, agent, ("send",), {"request_id": str(uuid.uuid4()), "author_kind": "agent_session", "source": source})


def held_snapshot(store, attempt, address, thread, helper_address, since):
    claim = store.claims().get(attempt)
    return Held(claim, du.disposition_count(store, attempt),
                _rows(store, "SELECT count(*) FROM collaboration_messages WHERE session_socket_id = ? AND "
                             "worktree_id = ? AND message_id = ?", "terminal-relay:" + attempt)[0][0],
                _rows(store, "SELECT count(*) FROM collaboration_messages WHERE session_socket_id = ? AND "
                             "worktree_id = ? AND thread_id = ? AND author_address_id = ?",
                      thread, address)[0][0],
                _rows(store, "SELECT count(*) FROM collaboration_messages m JOIN message_recipients t USING "
                             "(session_socket_id, worktree_id, message_id) WHERE m.session_socket_id = ? AND "
                             "m.worktree_id = ? AND m.author_address_id = ? AND t.address_id = ? AND "
                             "m.created_at_ms >= ? AND m.in_reply_to IS NULL",
                      address, helper_address, since)[0][0])


def judge_hold(label, held):
    """While the turn is held: a request went out, and the person's thread stays silent."""
    if held.claim is not None and (held.claim.startswith("done:") or held.claim == "left:no_final_text"):
        return Check(label + " hold", False, "{} hold: inconclusive, the turn was never held: claim {}, {} relay, "
                     "{} helper requests".format(label, held.claim, held.relays, held.helper_requests))
    return fields(label + " hold", [("claim", held.claim, "left:held"),
                                    ("dispositions", held.dispositions, 0),
                                    ("posted answer", held.relays, 0),
                                    ("posts in the thread", held.posts, 0),
                                    ("helper requests", held.helper_requests, 1)])


def way_back_turns(transcript, attempt):
    return tuple(turn for turn in transcript.turns
                 if any("is still open" in text and attempt in text for text in turn.contexts))


def judge_late_answer(label, word, hop, answers, thread, way_back, interim, resolved):
    """interim is shown, not judged: whether a person sees a warning during a long hold is a
    product question this lane reports."""
    answer = answers[0] if answers else du.parse_message({})
    return [fields(label + " answer", [("way-back prompts", len(way_back) > 0, True),
                                       ("answers", len(answers), 1),
                                       ("same thread", answer.thread_id == thread, True),
                                       ("wire kind", answer.kind, "reply"),
                                       ("word in the answer", word in answer.body, True),
                                       ("attempts", hop.attempts, 1), ("retries", hop.retries, 0),
                                       ("disposition", hop.disposition, "reply"),
                                       ("warning open at the end", resolved is False, False)]),
            Check(label + " warning", True, "{}: warning during the hold: {}".format(label, interim or "none"))]


def run_hold(r, holder, helper, scripted, rng, prompt=SCRIPTED_HOLD_PROMPT):
    channels, word = r.channels, marker(rng)
    label = "{} held on {}".format(holder, helper)
    address, helper_address = channels.address(holder), channels.address(helper)
    baseline = dc.baseline_of(channels.latest(), channels.peers())
    mark, since = channels.mark(), live.now_ms()
    sent = channels.post("@{} {}".format(holder, prompt if scripted else HOLD_PROMPT.format(helper=helper, word=word)))
    hop = r.settle(lambda: du.hop_for(r.store, sent.message_id), lambda found: found and found.attempt_id,
                   TIMEOUTS.label, watch=()) or du.NO_HOP
    attempt = hop.attempt_id
    if scripted:
        send_during_turn(r, holder, attempt, SCRIPTED_HELP.format(helper=helper, word=word))
    r.settle(lambda: r.store.claims().get(attempt), lambda claim: claim not in (None, "open"), HOLD_WAIT)
    held = held_snapshot(r.store, attempt, address, sent.thread_id, helper_address, since)
    r.save("{}-hold.json".format(holder), {"word": word, "held": held._asdict(), "attempt": attempt})
    hold = judge_hold(label, held)
    if not hold.ok:
        return [hold]
    r.settle(lambda: dc.replies_to(channels.latest().messages, address, sent.message_id), bool, ANSWER_WAIT)
    idle = [alias for alias in AGENTS if not r.wait_agent(alias, ("idle", "done"), TIMEOUTS.idle)]
    dc.listen(r, dc.QUIET)
    answers = dc.replies_to(channels.latest().messages, address, sent.message_id)
    final = du.hop_for(r.store, sent.message_id) or du.NO_HOP
    transcript = r.settle(lambda: r.transcript(holder), lambda found: way_back_turns(found, attempt),
                          TIMEOUTS.idle, watch=())
    frames = channels.frames_since(mark)
    interim = sorted({row.source_kind for frame in frames for row in frame.attention if row.source_id == attempt})
    open_now = any(row.source_id == attempt for row in channels.latest().attention)
    r.save("{}-answer.json".format(holder), {"hop": final._asdict(), "answers": [row._asdict() for row in answers],
                                             "interim": interim})
    return [hold] + judge_late_answer(label, word, final, answers, sent.thread_id,
                                      way_back_turns(transcript, attempt), ", ".join(interim),
                                      not open_now) + dc.judge_stretch(
        r, label, holder, mark, baseline, since, (HOLD_RED,)) + [
        live.expect("still working", ", ".join(idle) or "none", "none")]


def check_held(r):
    """Check 5: a turn ends waiting on a helper, and its answer lands later in the same thread."""
    rng = random.Random()
    checks = []
    for holder, helper, scripted in (("claude", "codex", False), ("codex", "claude", True)):
        try:
            checks += run_hold(r, holder, helper, scripted, rng)
        except du.RECOVERABLE as error:
            checks.append(Check(holder + " held", False, str(error)))
            checks += du.quiesce(r)
    return checks


def report_uncoached(label, checks):
    """A held turn's answer with no instructions on how to give it: shown, not judged."""
    lost = [check.detail for check in checks if not check.ok]
    if lost:
        return [Check(label, True, "{}: FINDING, the answer did not land: {}".format(label, lost[0][:200]))]
    return [Check(label, True, "{}: the answer landed through the way-back line".format(label))]


def run_uncoached_hold(r, rng):
    label = "codex uncoached hold"
    checks = run_hold(r, "codex", "claude", True, rng, UNCOACHED_HOLD_PROMPT)
    if not all(check.ok for check in checks):
        attempts = [row[0] for row in du.open_attempts(r.store) if row[2] == "settled"]
        for attempt in attempts:
            du.close_no_reply(r, "codex", attempt)
    return report_uncoached(label, checks)


STEPS = (
    du.check_step("tracked", "tracked handoff", check_tracked,
                  "Tracked work ran from handoff through continuation and nudge to one shown report."),
    du.check_step("held", "held turn", check_held,
                  "A turn held on a helper posted nothing, then answered once in the same thread."),
)
