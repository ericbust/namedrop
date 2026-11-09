"""Daily-use checks 1 to 3: answers in Channels raise no false alarm, recaps carry them, and the
real warning still fires."""

from collections import namedtuple
import random
import re

from livetest import AGENTS, Check, StepFailed, TIMEOUTS, fields
import livetest as live
import dailyuse as du

PUBLIC, DIRECT = "public", "direct"
ANSWER_WAIT = 10
QUIET = 2
"""Seconds to keep listening after an answer. The hub polls SQLite every 250 ms."""

Surface = namedtuple("Surface", "key post answer_scope prompt")


def _composer(channels, agent, address, prompt, parent):
    return channels.post("@{} {}".format(agent, prompt))


def _thread(channels, agent, address, prompt, parent):
    return channels.post("@{} {}".format(agent, prompt), in_reply_to=parent)


def _direct(channels, agent, address, prompt, parent):
    return channels.direct_post(address, prompt)


FINAL_MESSAGE = " Answer in your final message, not with a command."
"""With natural wording alone a real Claude runs the bound reply on every person's message, and Codex
on some, so the answer never takes the final-message path whose bugs this lane exists to catch.
check_natural reports what natural wording costs."""

SURFACES = (
    Surface("composer", _composer, PUBLIC, "In one sentence, why does the sky look blue?" + FINAL_MESSAGE),
    Surface("thread", _thread, PUBLIC, "In one sentence, why do sunsets look red?" + FINAL_MESSAGE),
    Surface("dm", _direct, DIRECT, "Which planet is the largest? One short sentence, please." + FINAL_MESSAGE),
)
NATURAL_PROMPT = "What is 2+2?"

ALARM_TITLES = {"terminal_relay": "@{}'s terminal reply needs you",
                "agent_disposition": "@{} didn't answer a request",
                "delivery_attempt": "@{} didn't answer a request"}

Alarm = namedtuple("Alarm", "attention_id source_kind source_id subject place")
"""One attention item a person would see. place: "wire", or "stored" when only SQLite saw it."""

AttentionView = namedtuple("AttentionView", "alarms needs_you dots labelled")
"""What a stretch of frames and rows raised beyond its baseline. needs_you: frames at that level
that no earlier item or approval card explains. dots: (alias, new attention_count) of DM peers.
labelled: rows that read as terminal_relay."""

Baseline = namedtuple("Baseline", "attention messages peers")


def baseline_of(frame, peers):
    return Baseline(frozenset(row.attention_id for row in frame.attention) if frame else frozenset(),
                    frozenset(row.message_id for row in frame.messages) if frame else frozenset(),
                    {peer.alias: peer.attention_count for peer in peers})


def attention_view(frames, baseline, stored, peers, direct_rows=()):
    """The one reading of attention both ways round: check 1 demands it empty, check 3 demands it
    hold the warning. stored: (id, kind, source, opened) rows from attention_items."""
    alarms = {}
    for frame in frames:
        for row in frame.attention:
            if row.attention_id not in baseline.attention and row.source_kind != "agent_status":
                alarms.setdefault(row.attention_id, Alarm(row.attention_id, row.source_kind, row.source_id,
                                                          row.subject_alias, "wire"))
    for attention_id, kind, source in stored:
        if attention_id not in baseline.attention:
            alarms.setdefault(attention_id, Alarm(attention_id, kind, source, None, "stored"))
    labelled = {row.message_id for frame in frames for row in frame.messages
                if row.kind == "terminal_relay" and row.message_id not in baseline.messages}
    labelled |= {row.message_id for row in direct_rows
                 if row.kind == "terminal_relay" and row.message_id not in baseline.messages}
    dots = tuple((peer.alias, peer.attention_count) for peer in peers
                 if peer.attention_count > baseline.peers.get(peer.alias, 0))
    return AttentionView(tuple(alarms.values()), sum(unexplained_red(frame, baseline) for frame in frames),
                         dots, tuple(sorted(labelled)))


def unexplained_red(frame, baseline):
    """A frame at needs_you whose level an older item or an approval card doesn't account for: a
    short approval prompt reddens the page too, and that's not what these checks judge."""
    explained = any(row.source_kind == "agent_status" or row.attention_id in baseline.attention
                    for row in frame.attention)
    return frame.level == "needs_you" and not explained


def alarm_text(alarm, agent):
    title = ALARM_TITLES.get(alarm.source_kind, "@{} needs you").format(alarm.subject or agent)
    return '"Needs you" red item "{}" ({} {}{})'.format(
        title, alarm.source_kind, alarm.source_id,
        ", stored only, the stream never showed it" if alarm.place == "stored" else "")


def judge_quiet(label, agent, view):
    """Zero new attention, no red level, no DM dot, no label."""
    problems = [alarm_text(alarm, agent) for alarm in view.alarms]
    if view.needs_you:
        problems.append('"Attention needed" banner: health.level needs_you in {} frames'.format(view.needs_you))
    problems += ['"Needs you" dot on the DM with @{}: attention_count {}'.format(*dot) for dot in view.dots]
    problems += ['"not sent through Namedrop" label on {} (wire kind terminal_relay)'.format(row)
                 for row in view.labelled]
    if problems:
        return [Check(label + " quiet", False, "{}: {}".format(label, problem)) for problem in problems]
    return [Check(label + " quiet", True, "{}: no attention, no label".format(label))]


def judge_stretch(r, label, agent, mark, baseline, since, allowed=()):
    """check 1's reading over any other stretch of a run. allowed: (source_kind, why) a build under
    test is known to raise here, which doesn't fail the check and is named in the report."""
    view = attention_view(r.channels.frames_since(mark), baseline, stored_attention(r.store, since), r.channels.peers())
    why = dict(allowed)
    known = [alarm for alarm in view.alarms if alarm.source_kind in why]
    for alarm in known:
        r.known.add("{}: {}".format(alarm.source_kind, why[alarm.source_kind]))
    quiet = view._replace(alarms=tuple(alarm for alarm in view.alarms if alarm.source_kind not in why),
                          needs_you=0 if known else view.needs_you)
    return judge_quiet(label, agent, quiet)


def judge_known(r):
    """Reported at the end of a run, so a passing reader still sees what a person would."""
    return [Check("known false alarms", True, "known false alarms: {}".format("; ".join(sorted(r.known)) or "none"))]


def judge_warning(label, agent, attempt_id, view):
    """check 1 inverted: this one attempt must raise the real item, and nothing else may."""
    wanted = [alarm for alarm in view.alarms
              if alarm.source_kind == "agent_disposition" and alarm.source_id == attempt_id]
    others = [alarm for alarm in view.alarms if alarm not in wanted]
    pairs = [("agent_disposition item on the wire", any(alarm.place == "wire" for alarm in wanted), True),
             ("frames at needs_you", view.needs_you > 0, True),
             ("other items", ", ".join(alarm_text(alarm, agent) for alarm in others) or "none", "none"),
             ("labelled rows", ", ".join(view.labelled) or "none", "none")]
    return fields(label + " warning", pairs)


Answered = namedtuple("Answered", "label scope sent hop dispositions claim answers leaked turns")
"""What one scenario left behind. answers: the agent's rows replying to the post. leaked: a DM
answer also shows in the channel feed. turns: the transcript turns that carried the delivery."""


def judge_answered(ev):
    answer = ev.answers[0] if ev.answers else du.parse_message({})
    final = ev.turns[-1].answers[-1].strip() if ev.turns and ev.turns[-1].answers else ""
    pairs = [("deliveries", ev.hop.deliveries, 1), ("attempts", ev.hop.attempts, 1),
             ("retries", ev.hop.retries, 0),
             ("state", "{}/{}".format(ev.hop.state, ev.hop.active), "settled/0"),
             ("dispositions", ev.dispositions, 1),
             ("claim", ev.claim, frozenset({"done:no_replied", "done:replied"})),
             ("answers", len(ev.answers), 1),
             ("same thread", answer.thread_id == ev.sent.thread_id, True),
             ("wire kind", answer.kind, "reply"), ("legs", list(answer.legs), []),
             ("delivery turns", len(ev.turns), 1),
             ("model calls", ev.turns[-1].calls if ev.turns else 0, 1),
             ("settle commands", live.settle_commands(ev.turns[-1]) if ev.turns else 0, 0),
             ("body is the final answer", bool(final) and answer.body.strip() == final, True)]
    if ev.scope == DIRECT:
        pairs.append(("in the channel feed", ev.leaked, False))
    return fields(ev.label, pairs)


def stored_attention(store, since_ms):
    return store._rows("SELECT attention_item_id, source_kind, source_id FROM attention_items WHERE "
                       "session_socket_id = ? AND worktree_id = ? AND opened_at_ms >= ?",
                       (store.socket_id, store.workspace, since_ms))


def read_rows(r, sent, scope, address):
    """The rows a person would read after this post: the DM history, or the channel feed."""
    return r.channels.direct_history(address) if scope == DIRECT else r.channels.latest().messages


def replies_to(rows, address, message_id):
    return tuple(row for row in rows if row.author == address and row.in_reply_to == message_id)


def wait_answer(r, sent, scope, address):
    return r.settle(lambda: replies_to(read_rows(r, sent, scope, address), address, sent.message_id),
                    bool, ANSWER_WAIT, watch=())


def settled_answers(r, sent, scope, address, agent):
    """Every answer once the agent has gone idle and the page has caught up. The first answer closes
    the wait, and a second can land seconds later, when a Stop relays what a reply already said."""
    wait_answer(r, sent, scope, address)
    r.wait_agent(agent, ("idle", "done"), TIMEOUTS.idle)
    listen(r, QUIET)
    return replies_to(read_rows(r, sent, scope, address), address, sent.message_id)


def delivery_turns(r, agent, hop, body=None):
    """The delivery's transcript turns, once the host wrote the closing line: it can land after
    the Stop that settled the store. With body, wait for that closing line rather than the first
    interim text, which answers too."""
    if not hop.attempt_id:
        return ()
    key = du.prompt_key(r.store, hop.attempt_id)

    def closed(found):
        turns = live.delivery_turns(found, key)
        if body is None or not turns or not turns[-1].answers:
            return live.answered(found, key)
        return turns[-1].answers[-1].strip() == body.strip()

    return tuple(live.delivery_turns(r.settle(lambda: r.transcript(agent), closed, TIMEOUTS.exit, watch=()), key))


def listen(r, seconds):
    r.settle(lambda: None, lambda _: False, seconds, watch=())


def run_scenario(r, agent, surface, parent):
    channels = r.channels
    address = channels.address(agent)
    label = "{} {}".format(agent, surface.key)
    baseline = baseline_of(channels.latest(), channels.peers())
    mark, since = channels.mark(), live.now_ms()
    sent = surface.post(channels, agent, address, surface.prompt, parent)
    hop = du.wait_closed(r, sent.message_id)
    answers = settled_answers(r, sent, surface.answer_scope, address, agent) if du.closed(hop) else ()
    turns = delivery_turns(r, agent, hop, answers[0].body if answers else None)
    feed = channels.history()
    leaked = any(row.message_id in {answer.message_id for answer in answers}
                 for row in feed + channels.latest().messages)
    direct_rows = channels.direct_history(address)
    evidence = Answered(label, surface.answer_scope, sent, hop,
                        du.disposition_count(r.store, hop.attempt_id) if hop.attempt_id else 0,
                        r.store.claims().get(hop.attempt_id), answers, leaked, turns)
    view = attention_view(channels.frames_since(mark), baseline, stored_attention(r.store, since),
                          channels.peers(), direct_rows)
    r.save("{}-{}.json".format(agent, surface.key),
           {"sent": sent._asdict(), "hop": hop._asdict(), "claim": evidence.claim,
            "answers": [row._asdict() for row in answers], "view": view._asdict()})
    checks = [judge_answered(evidence)] + judge_quiet(label, agent, view)
    return checks, answers[0].message_id if answers else None, view, since


def judge_natural(label, hop, answers, turns):
    """The answer is judged and its cost is only shown: running the reply line is the model's choice,
    and it's a finding rather than a failure until Namedrop's instructions change."""
    cost = "{} model calls, {} settle commands".format("+".join(str(turn.calls) for turn in turns) or "0",
                                                       sum(live.settle_commands(turn) for turn in turns))
    return [fields(label, [("deliveries", hop.deliveries, 1), ("attempts", hop.attempts, 1),
                           ("retries", hop.retries, 0), ("state", "{}/{}".format(hop.state, hop.active), "settled/0"),
                           ("answers", len(answers), 1)]),
            Check(label + " cost", True, "{}: {}".format(label, cost))]


def run_natural(r, agent):
    """A person's natural wording, by either path the model picks."""
    channels = r.channels
    label = agent + " natural wording"
    address = channels.address(agent)
    baseline = baseline_of(channels.latest(), channels.peers())
    mark, since = channels.mark(), live.now_ms()
    sent = channels.post("@{} {}".format(agent, NATURAL_PROMPT))
    hop = du.wait_closed(r, sent.message_id)
    answers = settled_answers(r, sent, PUBLIC, address, agent) if du.closed(hop) else ()
    turns = delivery_turns(r, agent, hop)
    view = attention_view(channels.frames_since(mark), baseline, stored_attention(r.store, since), channels.peers())
    return judge_natural(label, hop, answers, turns) + judge_quiet(label, agent, view)


def check_answers(r):
    """Check 1: a person asks each agent on each surface, and only an answer comes back."""
    checks, seen, first_since = [], set(), live.now_ms()
    before = baseline_of(r.channels.latest(), r.channels.peers())
    mark = r.channels.mark()
    for agent in AGENTS:
        # First, because a session's earlier answers teach it how to answer, and this measures a new one.
        checks += run_natural(r, agent)
        parent = None
        for surface in SURFACES:
            label = "{} {}".format(agent, surface.key)
            if surface.key == "thread" and not parent:
                checks.append(Check(label, False, "{}: no composer answer to reply to".format(label)))
                continue
            try:
                found, answer, view, _ = run_scenario(r, agent, surface, parent)
            except du.RECOVERABLE as error:
                checks.append(Check(label, False, "{}: {}".format(label, error)))
                checks += du.quiesce(r)
                continue
            checks += found
            seen.update(alarm.attention_id for alarm in view.alarms)
            parent = answer if surface.key == "composer" else parent
    listen(r, du.WARNING_FLOOR)
    late = attention_view(r.channels.frames_since(mark),
                          before._replace(attention=before.attention | seen, messages=frozenset(
                              row.message_id for frame in r.channels.frames_since(mark)
                              for row in frame.messages)),
                          stored_attention(r.store, first_since), r.channels.peers())
    return checks + judge_quiet("{} s later".format(du.WARNING_FLOOR), "an agent",
                                late._replace(needs_you=0, dots=()))


STEPS = (
    du.check_step("answers", "channels answers", check_answers,
                  "A person's Channels questions to Claude and Codex came back as ordinary replies "
                  "with no false warning."),
)
