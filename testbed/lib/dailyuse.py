"""The daily-use lane: what a person's day with Namedrop looks like, on real Claude and Codex.

`lane()` is what `./testbed test --daily-use` runs. This module owns the Lane shape, the Channels
HTTP and SSE client, the wire parser and the transcript key; the checks live in daily_*.py.
"""

import sys

sys.dont_write_bytecode = True

from collections import namedtuple
import http.cookiejar
import json
import subprocess
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid

import livetest as live

Lane = namedtuple("Lane", "name setup checks budget install", defaults=(None,))
"""setup: Steps whose failure skips everything after them. checks: Steps that run even after an
earlier check failed, because a run on a buggy build should report everything it finds. install:
the source to install instead of the requested one, which then stays the build under test."""

Frame = namedtuple("Frame", "at_ms level attention messages participants")
AttentionRow = namedtuple("AttentionRow", "attention_id source_kind source_id subject_alias "
                                          "opened_at_ms summary")
WireMessage = namedtuple("WireMessage", "message_id thread_id in_reply_to author kind body legs channel")
"""legs: one "stage" or "stage/kind" per recipient, as in "completed/no_reply". channel: the
routing state of a plain post, or None."""
Participant = namedtuple("Participant", "address_id alias activity")
"""alias without the wire's leading @."""

Peer = namedtuple("Peer", "address_id alias attention_count")

WARNING_FLOOR = 35
"""Seconds after an attempt starts by which Namedrop has judged an idle agent's silence. It waits
30 s from the attempt, then flags on its next tick."""


def _body(value):
    return (value or {}).get("text") or ""


def _stage(leg):
    stage = leg.get("stage") or {}
    return "/".join(str(stage[key]) for key in ("stage", "kind") if stage.get(key))


def parse_message(wire):
    return WireMessage(wire.get("message_id"), wire.get("thread_id"), wire.get("in_reply_to"),
                       wire.get("author"), wire.get("kind"), _body(wire.get("body")),
                       tuple(_stage(leg) for leg in wire.get("legs") or ()), wire.get("channel"))


def parse_attention(wire):
    return AttentionRow(wire.get("attention_id"), wire.get("source_kind"), wire.get("source_id"),
                        wire.get("subject_alias"), wire.get("opened_at_ms"), _body(wire.get("summary")))


def parse_frame(wire):
    """One `space` frame's `frame` object as the lane's judges read it."""
    return Frame(wire.get("generated_at_ms"), (wire.get("health") or {}).get("level"),
                 tuple(parse_attention(row) for row in wire.get("attention") or ()),
                 tuple(parse_message(row) for row in wire.get("messages") or ()),
                 tuple(Participant(row.get("address_id"), (row.get("alias") or "").lstrip("@"),
                                   row.get("activity"))
                       for row in wire.get("participants") or ()))


def small_frame(wire, limit=200):
    """A frame for the evidence directory: long bodies cut, every other field as sent."""
    def cut(row):
        text = (row.get("body") or {}).get("text") or ""
        return dict(row, body=dict(row["body"], text=text[:limit] + "…")) if len(text) > limit else row
    return dict(wire, messages=[cut(row) for row in wire.get("messages") or ()])


def quoted(segment):
    """terminal-relay:<attempt> holds a colon."""
    return urllib.parse.quote(segment, safe="")


class ChannelsError(RuntimeError):
    pass


RECOVERABLE = (live.StepFailed, RuntimeError, OSError, ValueError, KeyError, subprocess.SubprocessError)
"""What one scenario can raise without taking the next scenario down: urllib's errors are OSErrors
and StoreBusy is a RuntimeError."""


Sent = namedtuple("Sent", "message_id thread_id")


class Channels:
    """A person at the Channels page: one session, one stream, the page's own routes.

    One thread reads /api/events for the whole run. A person's next post resolves a 1.6.1 red
    item, so only the frames in between show it, and the server exits after 10 minutes with no
    subscriber.
    """

    def __init__(self, url):
        parts = urllib.parse.urlsplit(url)
        self.base = "{}://{}".format(parts.scheme, parts.netloc)
        self.key = parts.path.rsplit("/", 1)[-1]
        self.opener = urllib.request.build_opener(
            urllib.request.ProxyHandler({}),
            urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
        self.opener.open(url, timeout=10).read()
        self.csrf = self.get("/api/session")["csrf_token"]
        self.frames, self.raw, self.gaps = [], [], []
        self.levels = {}
        self.stopped = threading.Event()
        threading.Thread(target=self._record, daemon=True).start()

    def get(self, path):
        with self.opener.open(self.base + path, timeout=10) as response:
            return json.load(response)

    def _post(self, path, body):
        request = urllib.request.Request(
            self.base + path, data=json.dumps(body).encode(), method="POST",
            headers={"Content-Type": "application/json", "X-Namedrop-Csrf": self.csrf})
        try:
            with self.opener.open(request, timeout=30) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            raise ChannelsError("POST {} answered {}: {}".format(
                path, error.code, error.read().decode(errors="replace")[:300]))

    def _record(self):
        while not self.stopped.is_set():
            try:
                with self.opener.open(self.base + "/api/events", timeout=40) as stream:
                    self._read(stream)
                self.gaps.append("the event stream closed at {}".format(live.now_ms()))
            except (OSError, ValueError) as error:
                self.gaps.append("the event stream failed at {}: {}".format(live.now_ms(), error))
            time.sleep(1)

    def stop(self):
        self.stopped.set()

    def _read(self, stream):
        name = None
        for raw in stream:
            line = raw.decode(errors="replace").rstrip("\n")
            if line.startswith("event: "):
                name = line[7:]
            elif line.startswith("data: ") and name == "space":
                event = json.loads(line[6:])
                frame = event["frame"]
                self.levels[(frame.get("space") or {}).get("workspace_id")] = (frame.get("health") or {}).get("level")
                if event.get("key") == self.key:
                    self.raw.append(small_frame(frame))
                    self.frames.append(parse_frame(frame))

    def latest(self):
        return self.frames[-1] if self.frames else None

    def level_of_workspace(self, workspace):
        return self.levels.get(workspace)

    def mark(self):
        """A position in the recording; frames_since(mark) is what arrived after it."""
        return len(self.frames)

    def frames_since(self, mark):
        return tuple(self.frames[mark:])

    def save_frames(self, path, mark):
        with path.open("w") as out:
            for wire in self.raw[mark:]:
                out.write(json.dumps(wire) + "\n")

    def address(self, alias):
        for row in self.latest().participants:
            if row.alias == alias:
                return row.address_id
        raise ChannelsError("@{} is not a member of this channel".format(alias))

    def post(self, source, in_reply_to=None):
        """Composer post or thread reply, as the page sends them."""
        body = {"request_id": str(uuid.uuid4()), "source": source}
        if in_reply_to:
            body["in_reply_to"] = in_reply_to
        receipt = self._post("/w/{}/messages".format(self.key), body)
        return Sent(receipt["message_id"], receipt["thread_id"])

    def deliver(self, message_id, aliases):
        return self._post("/w/{}/messages/{}/deliver".format(self.key, quoted(message_id)),
                          {"to": list(aliases)})

    def direct_post(self, address, source):
        receipt = self._post("/w/{}/direct-messages/{}/messages".format(self.key, quoted(address)),
                             {"request_id": str(uuid.uuid4()), "source": source})
        return Sent(receipt["message_id"], receipt["thread_id"])

    def direct_history(self, address):
        rows = self.get("/w/{}/direct-messages/{}/messages?limit=100".format(self.key, quoted(address)))
        return tuple(parse_message(row) for row in rows.get("messages") or ())

    def history(self):
        rows = self.get("/w/{}/messages?limit=100".format(self.key))
        return tuple(parse_message(row) for row in rows)

    def peers(self):
        return tuple(Peer(row.get("address_id"), row.get("alias"), row.get("attention_count"))
                     for row in self.get("/api/direct-messages").get("peers") or ()
                     if row.get("space_key") == self.key)


def prompt_key(store, attempt_id):
    """The text that identifies a delivery in a transcript prompt: the attempt id while the whole
    envelope is typed (1.6.x), the pointer's first words once only a pointer is (1.7.0). The
    column is looked up first because a missing one reads as StoreBusy."""
    columns = {row[1] for row in store._rows("PRAGMA table_info(delivery_attempt_state)")}
    if "pointer_nonce" not in columns:
        return attempt_id
    rows = store._rows("SELECT pointer_nonce FROM delivery_attempt_state WHERE session_socket_id = ? "
                       "AND worktree_id = ? AND attempt_id = ?",
                       (store.socket_id, store.workspace, attempt_id))
    return "Namedrop message {} from".format(rows[0][0]) if rows and rows[0][0] else attempt_id


def hop_for(store, message_id):
    """The Hop of one message, or None. Its answer's row is a Hop too, with no attempt."""
    scope = (store.socket_id, store.workspace)
    rows = [live.Hop(*row) for row in store._rows(live.HOP_SQL, scope + scope + (message_id,))]
    return next((hop for hop in rows if hop.message_id == message_id), None)


NO_HOP = live.Hop(*[None] * len(live.Hop._fields))


def closed(hop):
    return bool(hop and hop.attempt_id and hop.state in live.TERMINAL_STATES and hop.active == 0)


def wait_closed(r, message_id, seconds=live.TIMEOUTS.round_trip):
    return r.settle(lambda: hop_for(r.store, message_id), closed, seconds) or NO_HOP


def disposition_count(store, attempt_id):
    return store._rows("SELECT count(*) FROM delivery_dispositions WHERE session_socket_id = ? AND "
                       "worktree_id = ? AND attempt_id = ?",
                       (store.socket_id, store.workspace, attempt_id))[0][0]


INTERRUPT_KEY = {"claude": "esc", "codex": "esc"}
"""Measured on Claude Code 2.1.319 and Codex 0.167.0, each in the middle of a shell command."""


def close_no_reply(r, alias, attempt_id):
    """Run the attempt's bound `complete --no-reply` as the agent's pane, as the agent would."""
    return r.profile.namedrop("--herdr-pane", r.pane(alias), "complete", "--no-reply",
                              "--input-json", json.dumps({"attempt_id": attempt_id}))


def open_attempts(store):
    """(attempt, address, state) of every attempt still running, or settled with no answer."""
    return store._rows(
        "SELECT a.attempt_id, a.address_id, s.state FROM delivery_attempts a "
        "JOIN delivery_attempt_state s USING (session_socket_id, worktree_id, attempt_id) "
        "LEFT JOIN delivery_dispositions p USING (session_socket_id, worktree_id, attempt_id) "
        "WHERE a.session_socket_id = ? AND a.worktree_id = ? AND "
        "(s.active = 1 OR (s.state = 'settled' AND p.disposition_kind IS NULL))",
        (store.socket_id, store.workspace))


def open_items(store):
    return store._rows("SELECT source_kind, source_id FROM attention_items WHERE session_socket_id = ? "
                       "AND worktree_id = ? AND resolved_at_ms IS NULL",
                       (store.socket_id, store.workspace))


def quiesce(r):
    """After a failed check: stop what still runs and close what it left open, so the next check
    starts clean. A Check, never silent, and not ok so the report shows it under the failure."""
    notes = []
    panes = r.panes()
    for alias in live.AGENTS:
        if panes[alias].status in ("working", "blocked"):
            r.keys(alias, (INTERRUPT_KEY[alias],))
            ended = r.wait_agent(alias, ("idle", "done"), live.TIMEOUTS.idle)
            notes.append("interrupted {} ({})".format(alias, "idle" if ended else "still working"))
    aliases = {row.address_id: row.alias for row in r.channels.latest().participants}
    for attempt, address, state in open_attempts(r.store):
        alias = aliases.get(address)
        if alias in live.AGENTS:
            result = close_no_reply(r, alias, attempt)
            notes.append("closed {} on {} from {} (exit {})".format(attempt, alias, state, result.returncode))
        else:
            notes.append("left {} on {} {}".format(attempt, alias or address, state))
    items = open_items(r.store)
    if items:
        notes.append("{} attention items stay open: {}".format(len(items), ", ".join(
            "{} {}".format(*item) for item in items)))
    return [live.Check("quiesce", False, "quiesced: " + "; ".join(notes))] if notes else []


def check_step(key, title, action, outcome="", restarts_channels=False):
    """A lane check: its frames land in the evidence directory, a gap in the recording fails it,
    and a failure quiesces so the next check still means something. A step that restarts Channels
    on purpose opens its own recording, so its gaps are the restart."""
    def run(r):
        channels = r.channels
        mark, gaps = channels.mark(), len(channels.gaps)
        try:
            checks = list(action(r))
        except live.StepFailed as failed:
            checks = list(failed.checks) + [failed.check]
        # A check that crashes must not take the report and the later checks with it.
        except Exception as error:
            checks = [live.Check(title, False, "{}: {}".format(type(error).__name__, error))]
        recording = r.channels
        missed = channels.gaps[gaps:]
        if missed and not restarts_channels:
            checks.append(live.Check("event stream", False, "the recording has gaps: " + "; ".join(missed)))
        if not all(check.ok for check in checks):
            try:
                checks += quiesce(r)
            except Exception as error:
                checks.append(live.Check("quiesce", False, "quiesce failed: {}: {}".format(type(error).__name__, error)))
        recording.save_frames(r.step_dir / "frames.jsonl", 0 if recording is not channels else mark)
        return checks
    return live.Step(key, title, "", run, outcome)


def open_channels(r):
    """Sign in as a person and start a recording. Returns what `namedrop channels` printed."""
    result = r.profile.namedrop("channels", "--no-open", "--json")
    data = live.last_json(result.stdout).get("data") or {}
    r.channels = Channels(data["url"])
    r.settle(r.channels.latest, bool, live.TIMEOUTS.label, watch=())
    return data


def step_channels(r):
    """The one recording every check reads."""
    data = open_channels(r)
    latest = r.channels.latest()
    if latest is None:
        return [live.Check("channels", False, "no frame on the event stream after {} s".format(live.TIMEOUTS.label))]
    return [live.fields("channels", [("server", data.get("server"), frozenset({"started", "running"})),
                                     ("space", r.channels.raw[-1]["space"]["state"], "active"),
                                     ("level", latest.level, "calm"),
                                     ("members", " ".join(sorted(row.alias for row in latest.participants
                                                                 if row.alias != "you")),
                                      "claude codex shell")])]


SETUP = (live.Step("first-run", "first run", "", live.step_first_run, ""),
         live.Step("names", "names", "", live.step_names, ""),
         live.Step("hooks", "hooks", "", live.step_hooks, ""),
         live.Step("channels", "channels", "", step_channels, ""))

BUDGET = 40 * 60


def lane():
    import daily_channels
    import daily_continuity
    import daily_coordination
    return Lane("daily-use", SETUP,
                daily_channels.STEPS + daily_coordination.STEPS + daily_continuity.STEPS, BUDGET)
