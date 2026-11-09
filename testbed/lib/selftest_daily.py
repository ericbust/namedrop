"""Offline tests for the daily-use lane's parsers and judges. Each defect test names a real bug the
judge exists to catch; fixtures in ../fixtures were saved from real runs."""

import http.server
import json
from pathlib import Path
import sqlite3
import threading

import livetest as live
import dailyuse as du
import daily_channels as dc
import daily_continuity as dcn
import daily_coordination as dco
import daily_upgrade as dup

FIXTURES = Path(__file__).resolve().parent.parent / "fixtures"


def frame_fixture(name):
    return json.loads((FIXTURES / name).read_text())


def check_frames():
    good = du.parse_frame(frame_fixture("channels-frame-1.6.2.json"))
    bad = du.parse_frame(frame_fixture("channels-frame-1.6.1.json"))
    assert good.level in ("calm", "busy") and not good.attention
    assert any(row.kind == "reply" and row.message_id.startswith("terminal-relay:") for row in good.messages)
    assert bad.level == "needs_you"
    assert {row.source_kind for row in bad.attention} == {"terminal_relay"}
    assert any(row.kind == "terminal_relay" for row in bad.messages)
    assert all(row.alias and not row.alias.startswith("@") for row in good.participants)
    leg = du.parse_message({"legs": [{"stage": {"stage": "completed", "kind": "no_reply"}}, {"stage": {"stage": "held"}}]})
    assert leg.legs == ("completed/no_reply", "held"), "the legs judge reads these"
    cut = du.small_frame({"messages": [{"body": {"text": "x" * 500}}, {"body": {"text": "y"}}]}, 10)
    assert cut["messages"][0]["body"]["text"] == "x" * 10 + "…" and cut["messages"][1]["body"]["text"] == "y"
    return "daily frames: a saved 1.6.1 frame reads as a red item with the label, a 1.6.2 frame as neither"


def empty_baseline():
    return dc.baseline_of(None, ())


def check_quiet_judge():
    good = du.parse_frame(frame_fixture("channels-frame-1.6.2.json"))
    bad = du.parse_frame(frame_fixture("channels-frame-1.6.1.json"))
    quiet = dc.judge_quiet("claude composer", "claude", dc.attention_view((good,), empty_baseline(), (), ()))
    assert [check.ok for check in quiet] == [True]
    loud = dc.judge_quiet("claude composer", "claude", dc.attention_view((bad,), empty_baseline(), (), ()))
    details = " ".join(check.detail for check in loud)
    assert not any(check.ok for check in loud)
    assert "not sent through Namedrop" in details and "terminal reply needs you" in details and "banner" in details
    stored = dc.attention_view((good,), empty_baseline(), (("i", "terminal_relay", "s"),), ())
    assert not dc.judge_quiet("x", "claude", stored)[0].ok, "a flicker only SQLite saw still fails"
    dot = dc.attention_view((), empty_baseline(), (), (du.Peer("a", "claude", 1),))
    assert "attention_count 1" in dc.judge_quiet("x", "claude", dot)[0].detail
    status = good._replace(attention=(du.AttentionRow("agent-status-a", "agent_status", "a", "codex", 1, "s"),))
    assert dc.judge_quiet("x", "codex", dc.attention_view((status,), empty_baseline(), (), ()))[0].ok
    banner = good._replace(level="needs_you")
    assert not dc.judge_quiet("x", "claude", dc.attention_view((banner,), empty_baseline(), (), ()))[0].ok
    card = banner._replace(attention=status.attention)
    assert dc.judge_quiet("x", "codex", dc.attention_view((card,), empty_baseline(), (), ()))[0].ok, "an approval prompt"
    open_item = bad.attention[0]
    older = dc.Baseline(frozenset({open_item.attention_id}), frozenset(), {})
    assert dc.judge_quiet("x", "claude", dc.attention_view((banner._replace(attention=bad.attention),), older, (), ()))[0].ok
    old = bad._replace(level="calm")
    known = dc.Baseline(frozenset(row.attention_id for row in bad.attention),
                        frozenset(row.message_id for row in bad.messages), {})
    assert dc.judge_quiet("x", "claude", dc.attention_view((old,), known, (), ()))[0].ok, "only new items count"
    return "daily quiet judge: red item, label, banner, DM dot and a stored-only flicker each fail"


def check_warning_judge():
    item = du.AttentionRow("i", "agent_disposition", "attempt-1", "claude", 1, "s")
    base = du.parse_frame(frame_fixture("channels-frame-1.6.2.json"))
    loud = base._replace(level="needs_you", attention=(item,))
    assert dc.judge_warning("w", "claude", "attempt-1", dc.attention_view((loud,), empty_baseline(), (), ())).ok
    older = item._replace(attention_id="old", source_id="attempt-0")
    known = dc.Baseline(frozenset({"old"}), frozenset(), {})
    leftover = dc.attention_view((loud._replace(attention=(older, item)),), known, (), ())
    assert dc.judge_warning("w", "claude", "attempt-1", leftover).ok, "an earlier failed check's item doesn't hide this one"
    silenced = dc.attention_view((base,), empty_baseline(), (), ())
    assert not dc.judge_warning("w", "claude", "attempt-1", silenced).ok, "a build that silences everything fails"
    stored_only = dc.attention_view((loud._replace(attention=()),), empty_baseline(),
                                    (("i", "agent_disposition", "attempt-1"),), ())
    assert not dc.judge_warning("w", "claude", "attempt-1", stored_only).ok, "the page has to show it"
    other = loud._replace(attention=(item, item._replace(attention_id="j", source_kind="terminal_relay")))
    assert not dc.judge_warning("w", "claude", "attempt-1", dc.attention_view((other,), empty_baseline(), (), ())).ok
    return "daily warning judge: check 1's reading inverted fails a build that silences everything"


def answered_case(**changes):
    sent = du.Sent("m", "t")
    answer = du.parse_message({"message_id": "terminal-relay:a", "thread_id": "t", "in_reply_to": "m",
                               "kind": "reply", "body": {"text": "The sky scatters blue."}, "legs": []})
    hop = live.Hop("m", "person", "claude", None, 1, 1, 0, 1, "a", "settled", 0, "no_reply", "q")
    turn = live.Turn("a", (), ("The sky scatters blue.",), (), 1)
    base = dict(label="claude composer", scope=dc.PUBLIC, sent=sent, hop=hop, dispositions=1,
                claim="done:no_replied", answers=(answer,), leaked=False, turns=(turn,))
    base.update(changes)
    return dc.Answered(**base)


def check_answer_judge():
    assert dc.judge_answered(answered_case()).ok
    answer = answered_case().answers[0]
    hop = answered_case().hop
    turn = answered_case().turns[0]
    defects = {
        "doubled": dict(answers=(answer, answer)),
        "no answer": dict(answers=()),
        "wrong thread": dict(answers=(answer._replace(thread_id="other"),)),
        "labelled": dict(answers=(answer._replace(kind="terminal_relay"),)),
        "retried": dict(hop=hop._replace(attempts=2, retries=1)),
        "unsettled": dict(hop=hop._replace(state="acknowledged", active=1)),
        "two model calls": dict(turns=(turn._replace(calls=2),)),
        "settle command": dict(turns=(turn._replace(commands=("'namedrop' reply --input-json {}",)),)),
        "body differs": dict(turns=(turn._replace(answers=("Something else.",)),)),
        "claim open": dict(claim="open"),
        "two turns": dict(turns=(turn, turn)),
        "two deliveries": dict(hop=hop._replace(deliveries=2)),
        "no disposition": dict(dispositions=0),
        "legs": dict(answers=(answer._replace(legs=("completed/settled",)),)),
    }
    for name, change in defects.items():
        assert not dc.judge_answered(answered_case(**change)).ok, name
    assert not dc.judge_answered(answered_case(scope=dc.DIRECT, leaked=True)).ok
    assert dc.judge_answered(answered_case(scope=dc.DIRECT)).ok
    return "daily answer judge: a doubled, misthreaded, labelled, retried or costly answer each fail"


def check_natural_judge():
    hop = answered_case().hop
    answer = answered_case().answers[0]
    costly = live.Turn("a", (), (), ("'namedrop' reply --input-json {}",), 2)
    judged = dc.judge_natural("n", hop, (answer,), (costly,))
    assert all(check.ok for check in judged) and "2 model calls, 1 settle commands" in judged[1].detail
    assert not dc.judge_natural("n", hop, (), (costly,))[0].ok, "a lost answer still fails"
    assert not dc.judge_natural("n", hop._replace(attempts=2), (answer,), (costly,))[0].ok
    return "daily natural wording: the cost is shown, a lost or retried answer still fails"


def check_recap_judge():
    turn = live.Turn("Earlier: claude: VWXYZ", (), ("VWXYZ",), (), 1)
    transcript = live.Transcript("codex", "s", "p", (), (turn,), frozenset())
    assert all(check.ok for check in dc.judge_recap("claude to codex", "VWXYZ", (turn,), (transcript,), "VWXYZ"))
    missing = dc.judge_recap("claude to codex", "VWXYZ", (turn._replace(prompt="no recap"),), (transcript,), "none")
    assert not missing[0].ok and "recap missing" in missing[0].detail
    hooked = transcript._replace(turns=(turn._replace(contexts=("claude: VWXYZ",)),))
    assert not dc.judge_recap("claude to codex", "VWXYZ", (turn,), (hooked,), "VWXYZ")[0].ok, "a hook context holding it"
    session = transcript._replace(session_contexts=("VWXYZ",))
    assert not dc.judge_recap("claude to codex", "VWXYZ", (turn,), (session,), "VWXYZ")[0].ok
    assert not dc.judge_recap("claude to codex", "VWXYZ", (turn,), (transcript,), "I see no reply")[0].ok
    assert dc.sentinel_in("It is KFRZTMB.", "kfrztmb") == "KFRZTMB"
    assert dc.sentinel_in("It is kfrztmb.", "kfrztmb") is None, "the question's own letters are not an answer"
    return "daily recap judge: a missing recap, a recap in hook context and a lost answer each fail"


def key_store(path, nonce_column):
    columns = "session_socket_id, worktree_id, attempt_id" + (", pointer_nonce" if nonce_column else "")
    connection = sqlite3.connect(str(path))
    connection.execute("CREATE TABLE delivery_attempt_state ({})".format(columns))
    values = ("s", "w", "attempt-1") + (("ab12cd34",) if nonce_column else ())
    connection.execute("INSERT INTO delivery_attempt_state VALUES ({})".format(",".join("?" * len(values))), values)
    connection.commit()
    connection.close()
    return live.Store(path, "s", "w")


def check_prompt_key(scratch):
    assert du.prompt_key(key_store(scratch / "schema7.sqlite3", False), "attempt-1") == "attempt-1"
    assert du.prompt_key(key_store(scratch / "schema8.sqlite3", True), "attempt-1") == "Namedrop message ab12cd34 from"
    return "daily prompt key: the attempt id on schema 7, the pointer's words on schema 8"


class FakePage(http.server.BaseHTTPRequestHandler):
    """The sign-in and CSRF rules channels.md measured on the real server."""
    def log_message(self, *args):
        pass

    def _send(self, status, body=b"{}", headers=()):
        self.send_response(status)
        for name, value in headers:
            self.send_header(name, value)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        if self.path.startswith("/w/key?n="):
            return self._send(303, headers=(("Location", "/w/key"), ("Set-Cookie", "session=1; Path=/")))
        if "session=1" not in self.headers.get("Cookie", ""):
            return self._send(401)
        if self.path == "/api/session":
            return self._send(200, json.dumps({"csrf_token": "token"}).encode())
        if self.path == "/api/events":
            frame = {"generated_at_ms": 1, "health": {"level": "calm"}, "space": {"workspace_id": "w"}}
            line = "event: space\ndata: " + json.dumps({"key": "key", "frame": frame}) + "\n\n"
            return self._send(200, line.encode(), headers=(("Content-Type", "text/event-stream"),))
        return self._send(200)

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        if self.headers.get("X-Namedrop-Csrf") != "token":
            return self._send(403, b"csrf")
        if body.get("source") == "refused":
            return self._send(400, b'{"error":"refused"}')
        self._send(200, json.dumps({"message_id": "m", "thread_id": "t"}).encode())


def check_client():
    server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), FakePage)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    try:
        base = "http://127.0.0.1:{}".format(server.server_port)
        client = du.Channels(base + "/w/key?n=nonce")
        assert client.post("hello") == du.Sent("m", "t")
        try:
            client.post("refused")
            raise AssertionError("a refused post must raise")
        except du.ChannelsError as error:
            assert "400" in str(error)
        for _ in range(200):
            if client.frames:
                break
            threading.Event().wait(0.01)
        assert client.latest().level == "calm" and client.level_of_workspace("w") == "calm"
    finally:
        server.shutdown()
    return "daily Channels client: signs in with a cookie, sends the CSRF header, records space frames"


def tracked_case(**changes):
    links = (dco.Link("a", 1, "request", None, None), dco.Link("b", 1, "reply", "needs_continuation", None),
             dco.Link("c", 2, "request", None, 1), dco.Link("d", 2, "reply", "completed", None))
    base = dict(coordination=dco.Coordination("c", "completed", "report amber1", "r", None),
                assignments=(dco.Assignment("x", "codex", 2, "completed"),), links=links, nudges=(),
                reports=(dco.Mail("r", "coordination-report/v1", "b", "settled", "no_reply", "ra"),),
                report_replies=())
    base.update(changes)
    return dco.Tracked(**base)


def check_tracked_judge():
    report = live.Turn("ra", (), ("Report: amber1",), (), 1)
    transcript = live.Transcript("claude", "s", "p", (), (report,), frozenset())
    judged = dco.judge_tracked("t", tracked_case(), "amber1", (report,), transcript)
    assert all(check.ok for check in judged), [check.detail for check in judged if not check.ok]
    links = tracked_case().links
    defects = {
        "never continued": dict(links=links[:2]),
        "no expected_revision": dict(links=links[:2] + (links[2]._replace(expected_revision=None), links[3])),
        "still blocked": dict(assignments=(dco.Assignment("x", "codex", 2, "blocked"),)),
        "two reports": dict(reports=tracked_case().reports * 2),
        "report unsettled": dict(reports=(tracked_case().reports[0]._replace(state="queued"),)),
        "replied to": dict(report_replies=("z",)),
    }
    for name, change in defects.items():
        result = dco.judge_tracked("t", tracked_case(**change), "amber1", (report,), transcript)
        assert not all(check.ok for check in result), name
    assert not all(c.ok for c in dco.judge_tracked("t", tracked_case(), "amber1", (report, report), transcript)), "shown twice"
    assert not all(c.ok for c in dco.judge_tracked("t", tracked_case(), "amber1", (), transcript)), "never shown"
    assert not dco.judge_tracked("t", tracked_case(coordination=None), "amber1", (), transcript)[0].ok
    assert dco.tracked_done(tracked_case()) and not dco.tracked_done(tracked_case(reports=()))
    return "daily tracked judge: a missing continuation, a doubled or unshown report each fail"


def check_hold_judges():
    held = dco.Held("left:held", 0, 0, 0, 1)
    assert dco.judge_hold("h", held).ok
    assert not dco.judge_hold("h", held._replace(relays=1)).ok, "posted while held"
    assert not dco.judge_hold("h", held._replace(dispositions=1)).ok
    assert not dco.judge_hold("h", held._replace(posts=1)).ok
    assert not dco.judge_hold("h", held._replace(helper_requests=0)).ok
    won = dco.judge_hold("h", held._replace(claim="done:no_replied"))
    assert not won.ok and "inconclusive" in won.detail, "a helper that won the race proves nothing"
    assert "inconclusive" in dco.judge_hold("h", held._replace(claim="left:no_final_text")).detail
    open_turn = live.Turn("p", ("Namedrop: delivery a1 from an earlier turn is still open; to answer it",), (), (), 1)
    transcript = live.Transcript("claude", "s", "p", (), (live.Turn("p", (), (), (), 1), open_turn), frozenset())
    assert dco.way_back_turns(transcript, "a1") == (open_turn,) and not dco.way_back_turns(transcript, "a2")
    answer = du.parse_message({"message_id": "m2", "thread_id": "t", "in_reply_to": "m", "kind": "reply",
                               "body": {"text": "amber1"}, "legs": []})
    hop = live.Hop("m", "person", "claude", None, 1, 1, 0, 1, "a1", "settled", 0, "reply", "q")
    ok = dco.judge_late_answer("h", "amber1", hop, (answer,), "t", (open_turn,), "agent_disposition", True)
    assert all(check.ok for check in ok)
    for changed in (dco.judge_late_answer("h", "amber1", hop, (), "t", (open_turn,), "", True),
                    dco.judge_late_answer("h", "amber1", hop, (answer, answer), "t", (open_turn,), "", True),
                    dco.judge_late_answer("h", "amber1", hop, (answer,), "other", (open_turn,), "", True),
                    dco.judge_late_answer("h", "amber1", hop, (answer,), "t", (), "", True),
                    dco.judge_late_answer("h", "amber1", hop._replace(attempts=2), (answer,), "t", (open_turn,), "", True),
                    dco.judge_late_answer("h", "amber1", hop, (answer,), "t", (open_turn,), "agent_disposition", False)):
        assert not all(check.ok for check in changed)
    return "daily hold judges: a post during the hold, a lost way-back line or a doubled answer each fail"


def check_uncoached_report():
    fine = [live.Check("a", True, "a")]
    assert "landed" in dco.report_uncoached("u", fine)[0].detail and dco.report_uncoached("u", fine)[0].ok
    lost = dco.report_uncoached("u", fine + [live.Check("b", False, "answers 0 (expected 1)")])
    assert lost[0].ok and "FINDING" in lost[0].detail and "answers 0" in lost[0].detail, "shown, not judged, and named"
    return "daily uncoached hold: a lost answer is shown as a finding without failing the lane"


def check_continuity_judges():
    assert dcn.compactions("claude", ['{"type":"system","subtype":"compact_boundary"}', "{}"]) == 1
    assert dcn.compactions("codex", ['{"type": "compacted"}', '{"type":"compacted"}']) == 2
    assert dcn.judge_compaction("c", 0, 1, "left:held", 0).ok
    assert not dcn.judge_compaction("c", 1, 1, "left:held", 0).ok, "no compaction proves nothing"
    assert not dcn.judge_compaction("c", 0, 1, "done:no_replied", 1).ok
    mode = live.MODES["no-daemon"]
    assert dcn.resume_command("claude", mode, "abc", {}) == "claude --resume abc"
    assert dcn.resume_command("codex", mode, "abc", {}) == "codex --no-daemon resume abc"
    next_hop = live.Hop("m", "person", "claude", None, 1, 1, 0, 1, "a", "settled", 0, "no_reply", "q")
    held = ("held", 0, "held")
    assert dcn.judge_crash("x", held, 1, 0, next_hop, "delivery_attempt").ok
    assert not dcn.judge_crash("x", held, 2, 0, next_hop, "delivery_attempt").ok, "doubled by a retry"
    assert not dcn.judge_crash("x", held, 1, 1, next_hop, "delivery_attempt").ok
    assert not dcn.judge_crash("x", held, 1, 0, next_hop, None).ok, "lost without a word to the person"
    assert not dcn.judge_crash("x", held, 1, 0, next_hop._replace(state="queued", active=None), "delivery_attempt").ok
    return "daily continuity judges: a missed compaction, a retried crash or a silent loss each fail"


def check_probe_parsing():
    text = ("ok    bare @lead stays in alpha: no messages\nnoise\n"
            "FAIL  #beta @lead from claude: alpha claude\nPASS 1/2\n")
    checks = dcn.parse_probe(text)
    assert [(check.what, check.ok) for check in checks] == [("bare @lead stays in alpha", True),
                                                            ("#beta @lead from claude", False)]
    assert not dcn.parse_probe("Traceback\n")[0].ok, "a script that prints no checks has failed"
    env = dcn.script_env({"HERDR_SOCKET_PATH": "/real", "PATH": "/bin"}, "/home")
    assert env == {"PATH": "/bin", "TESTBED_HOME": "/home"}, "the real socket never reaches a script"
    return "daily probe parsing: ok and FAIL lines become checks, no lines is a failure, no Herdr env leaks"


def check_backlog_judge():
    class Quiet:
        def __init__(self, backlog):
            self.result = backlog

        def backlog(self):
            return self.result

    clean = live.Doctor(0, "healthy", (), 0)
    space = dcn.Space("alpha", Quiet(live.Backlog(0, 0, 0)), clean, "calm")
    assert dcn.judge_backlog(space, 0, 0, 0)[0].ok
    defects = (dcn.judge_backlog(space._replace(store=Quiet(live.Backlog(1, 0, 0))), 0, 0, 0),
               dcn.judge_backlog(space._replace(store=Quiet(live.Backlog(0, 1, 0))), 0, 0, 0),
               dcn.judge_backlog(space._replace(store=Quiet(live.Backlog(0, 0, 1))), 0, 0, 0),
               dcn.judge_backlog(space, 1, 0, 0), dcn.judge_backlog(space, 0, 1, 0), dcn.judge_backlog(space, 0, 0, 1),
               dcn.judge_backlog(space._replace(level="needs_you"), 0, 0, 0),
               dcn.judge_backlog(space._replace(doctor=live.Doctor(1, "attention", (live.Issue("ND-X", "warn", {}),), 0)), 0, 0, 0))
    assert not any(result[0].ok for result in defects)
    return "daily backlog judge: anything awaiting, open or unhealthy fails, with no exceptions"


def check_lane_plan():
    lane = du.lane()
    keys = [step.key for step in lane.setup + lane.checks]
    assert keys[:4] == ["first-run", "names", "hooks", "channels"]
    assert keys[4:] == ["answers", "recap", "warnings", "tracked", "held", "continuity", "routes", "natural", "backlog"]
    ran, results = [], []

    class Fake:
        def execute(self, step, number):
            ran.append(step.key)
            return live.StepResult(step, (live.Check(step.key, step.key != "a", ""),), 0)

    a, b = live.Step("a", "a", "", None, ""), live.Step("b", "b", "", None, "")
    live.drive(Fake(), (), (a, b), results)
    assert ran == ["a", "b"], "a failed check doesn't stop the next"
    ran.clear()
    results.clear()
    live.drive(Fake(), (a,), (b,), results)
    assert ran == ["a"] and results[-1].status == "skip", "a failed setup step skips the rest"
    return "daily plan: checks run in order, a failed check runs on, a failed setup step skips the rest"


class FakeChannels:
    def __init__(self, gaps=()):
        self.gaps = list(gaps)
        self.frames = []

    def mark(self):
        return 0

    def save_frames(self, path, mark):
        self.saved = mark


class FakeRun:
    def __init__(self, channels):
        self.channels = channels
        self.step_dir = FIXTURES
        self.known = set()


def check_step_boundary():
    channels = FakeChannels()

    def opens_a_gap(r):
        channels.gaps.append("closed")
        return [live.Check("x", True, "x")]

    def crashes(r):
        raise AttributeError("a crash")

    try:
        original, du.quiesce = du.quiesce, lambda r: [live.Check("quiesce", False, "quiesced: closed a1")]
        gapped = du.check_step("k", "t", opens_a_gap).action(FakeRun(channels))
        assert any(check.what == "event stream" and not check.ok for check in gapped), "a gap fails the check"
        assert any(check.what == "quiesce" for check in gapped), "a failed check says what it closed"
        assert all(check.ok for check in du.check_step("k", "t", opens_a_gap, restarts_channels=True).action(
            FakeRun(FakeChannels())))
        crashed = du.check_step("k", "t", crashes).action(FakeRun(FakeChannels()))
        assert "AttributeError: a crash" in crashed[0].detail, "a crash stays in the report"
    finally:
        du.quiesce = original
    return "daily step boundary: a stream gap fails, a crash is reported, quiesce says what it closed"


def check_stretch_judge():
    good = du.parse_frame(frame_fixture("channels-frame-1.6.2.json"))

    class Store:
        socket_id, workspace = "s", "w"

        def _rows(self, sql, params=()):
            return ()

    class Run:
        store, known = Store(), set()

    run = Run()
    run.channels = FakeChannels()
    item = du.AttentionRow("i", "agent_disposition", "a", "claude", 1, "s")
    run.channels.peers = lambda: ()
    run.channels.frames_since = lambda mark: (good._replace(level="needs_you", attention=(item,)),)
    why = ("agent_disposition", "a hold")
    assert not dc.judge_stretch(run, "h", "claude", 0, empty_baseline(), 0)[0].ok, "no allowance, it fails"
    assert dc.judge_stretch(run, "h", "claude", 0, empty_baseline(), 0, (why,))[0].ok and run.known == {"agent_disposition: a hold"}
    other = item._replace(attention_id="j", source_kind="terminal_relay")
    run.channels.frames_since = lambda mark: (good._replace(level="needs_you", attention=(item, other)),)
    assert not all(c.ok for c in dc.judge_stretch(run, "h", "claude", 0, empty_baseline(), 0, (why,))), "only that kind"
    assert "a hold" in dc.judge_known(run)[0].detail
    real = ("agent_disposition", None)
    run.known = set()
    run.channels.frames_since = lambda mark: (good._replace(level="needs_you", attention=(item,)),)
    assert dc.judge_stretch(run, "h", "claude", 0, empty_baseline(), 0, (real,))[0].ok and not run.known, "a real item isn't a false alarm"
    return "daily stretch judge: an allowance names one kind, any other still fails, the report names it"


def check_interrupted_judge():
    stopped = live.Hop("m", "person", "claude", None, 1, 1, 0, 1, "a", "settled", 0, None, "q")
    good = lambda **changes: dict(dict(activity="working", key="esc", ended=True, stopped=stopped, legs=("completed/settled",),
                                       answers=0, opened_after="24 s", doctor_codes={"ND-DELIVERY-003"}), **changes)
    assert dc.judge_interrupted("i", **good()).ok
    for change in (dict(activity="idle"), dict(ended=False), dict(stopped=stopped._replace(disposition="no_reply")),
                   dict(answers=1), dict(doctor_codes=set()), dict(legs=("completed/no_reply",))):
        assert not dc.judge_interrupted("i", **good(**change)).ok, change
    return "daily interrupted judge: an answer, a disposition or a silent Doctor each fail"


def check_reminder_judge():
    hop = live.Hop("m", "namedrop", "codex", None, 1, 1, 0, 1, "a", "settled", 0, "no_reply", "q")
    turn = live.Turn("a", (), ("amber1",), (), 1)
    assert dcn.judge_reminder("r", 1, hop, (turn,), "amber1", None).ok
    assert dcn.judge_reminder("r", 1, hop, (turn,), "amber1", [("reply", True)]).ok
    assert not dcn.judge_reminder("r", 1, hop, (turn,), "amber1", [("terminal_relay", True)]).ok, "the label on an answer"
    assert not dcn.judge_reminder("r", 1, hop, (turn,), "amber1", [("reply", True), ("reply", True)]).ok, "doubled"
    assert not dcn.judge_reminder("r", 1, hop, (turn,), "amber1", [("reply", False)]).ok, "another thread"
    assert not dcn.judge_reminder("r", 2, hop, (turn,), "amber1", None).ok
    assert not dcn.judge_reminder("r", 1, hop, (), "amber1", None).ok
    return "daily reminder judge: a doubled, labelled or misthreaded answer each fail"


def check_upgrade_judges():
    listing = ("a\trefs/tags/v1.0.2\nb\trefs/tags/v1.6.1\nc\trefs/tags/v1.10.0\n"
               "d\trefs/tags/v2.0.0-exp.1\ne\trefs/tags/archive/v9.9.9\n")
    assert dup.newest_tag(listing, (9, 0, 0)) == "v1.10.0", "versions compare as numbers, and pre-releases don't count"
    assert dup.newest_tag(listing, (1, 10, 0)) == "v1.6.1", "the release before the build under test, never itself"
    assert dup.newest_tag(listing, (1, 0, 0)) is None and dup.newest_tag("", (9, 0, 0)) is None
    old, new = live.features("1.6.1"), live.features("1.6.2")
    both = {"claude": True, "codex": True}
    assert dup.judge_activation(0, old, new, both, both, "running").ok
    assert not dup.judge_activation(0, new, new, both, both, "running").ok, "1.6.2 over 1.6.2 proves nothing"
    assert not dup.judge_activation(0, new, old, both, both, "running").ok, "a downgrade"
    assert not dup.judge_activation(0, old, None, both, both, "running").ok
    assert not dup.judge_activation(1, old, new, both, both, "running").ok
    assert not dup.judge_activation(0, old, new, both, dict(both, codex=False), "running").ok, "not open when it ran"
    before = {"claude": dup.Standing("s1", (1,)), "codex": dup.Standing("s2", (2,))}
    assert all(check.ok for check in dup.judge_standing(before, before))
    restarted = dict(before, codex=dup.Standing("s2", (3,)))
    assert not all(check.ok for check in dup.judge_standing(before, restarted)), "a restarted agent didn't stay up"
    gone = dict(before, claude=dup.Standing(None, ()))
    assert not all(check.ok for check in dup.judge_standing(before, gone))
    hop = live.Hop("m", "person", "codex", None, 1, 1, 0, 1, "a", "settled", 0, "no_reply", "q")
    answer = du.parse_message({"message_id": "x", "thread_id": "t", "body": {"text": "amber1"}})
    assert dup.judge_open_delivery("o", hop, (answer,), "amber1", "done:no_replied").ok
    assert not dup.judge_open_delivery("o", hop, (), "amber1", "open").ok, "settled by observation, answer lost"
    assert not dup.judge_open_delivery("o", hop._replace(disposition=None), (answer,), "amber1", None).ok
    assert not dup.judge_open_delivery("o", hop._replace(attempts=2), (answer,), "amber1", None).ok
    return "daily upgrade judges: newest tag, a restarted agent, a lost or doubled open delivery"


def checks(scratch):
    return [("daily frames", check_frames), ("daily quiet judge", check_quiet_judge),
            ("daily warning judge", check_warning_judge), ("daily answer judge", check_answer_judge),
            ("daily natural wording", check_natural_judge), ("daily recap judge", check_recap_judge), ("daily prompt key", lambda: check_prompt_key(scratch)),
            ("daily Channels client", check_client), ("daily tracked judge", check_tracked_judge),
            ("daily hold judges", check_hold_judges), ("daily uncoached hold", check_uncoached_report), ("daily continuity judges", check_continuity_judges),
            ("daily probe parsing", check_probe_parsing), ("daily backlog judge", check_backlog_judge),
            ("daily step boundary", check_step_boundary), ("daily stretch judge", check_stretch_judge),
            ("daily interrupted judge", check_interrupted_judge), ("daily reminder judge", check_reminder_judge),
            ("daily upgrade judges", check_upgrade_judges), ("daily plan", check_lane_plan)]
