#!/usr/bin/env python3
"""Preview the `namedrop dashboard` browser UI without the Rust server.

Serves the embedded assets at their real paths and an SSE stream that replays
tests/fixtures/dashboard_snapshots.json, so the UI can be iterated on and
screenshotted in isolation.

    python3 scripts/dashboard-preview.py [--port 8765] [--interval 4]

Pages:
    /                      space index (from /api/spaces)
    /w/9f2c41d07ab3e815c2d4 replays every snapshot, looping
    /w/step-N              holds snapshot N (1-based)
    /w/empty               an active space with no messages yet
    /w/not-activated       a space where Namedrop is not activated
    /w/restarting          one snapshot, then a `restarting` frame, then silence

Every fixture is checked against the frozen wire contract at startup.
Timestamps are shifted so each frame's generated_at_ms is "now".
"""

import argparse
import copy
import json
import re
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "src" / "dashboard" / "assets"
FIXTURES = ROOT / "tests" / "fixtures" / "dashboard_snapshots.json"
CSP = "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"

SPACE_STATES = {"unknown", "not_activated", "active", "paused", "deactivated"}
ACTIVITIES = {"retired", "offline", "blocked", "working", "waiting", "idle"}
DELIVERY_STATES = {"queued", "dispatching", "submitted", "acknowledged", "settled", "held", "failed", "cancelled"}
MESSAGE_KINDS = {"mention", "reply", "handoff", "result_set", "nudge", "hook"}
LEG_STAGES = {"queued": set(), "delivered": set(), "working": {"acked_at_ms"}, "replied": {"reply_id"},
              "completed": {"kind"}, "held": set(), "failed": set(), "cancelled": set()}
HEALTH = {"calm": set(), "busy": {"in_flight", "working"}, "needs_you": {"items"}, "paused": set(), "inactive": set()}
OUTCOMES = {"open", "completed", "blocked", "cancelled"}
WORK_OUTCOMES = {"pending", "unclassified", "completed", "needs_continuation", "blocked"}


def check(cond, where):
    if not cond:
        raise SystemExit(f"fixture violates wire contract at {where}")


def keys(obj, expected, where):
    check(isinstance(obj, dict) and set(obj) == set(expected), f"{where}: keys {sorted(obj) if isinstance(obj, dict) else obj}")


def safe(v, where, nullable=False):
    if v is None and nullable:
        return
    keys(v, {"text", "truncated"}, where)
    check(isinstance(v["text"], str) and isinstance(v["truncated"], bool), where)


def validate(snap, i):
    w = f"snapshot[{i}]"
    keys(snap, {"wire", "generated_at_ms", "space", "health", "participants", "messages", "threads",
                "coordinations", "terminal_runs", "attention"}, w)
    check(snap["wire"] == 1, f"{w}.wire")
    keys(snap["space"], {"key", "workspace_id", "label", "state"}, f"{w}.space")
    safe(snap["space"]["label"], f"{w}.space.label", nullable=True)
    check(snap["space"]["state"] in SPACE_STATES, f"{w}.space.state")
    h = snap["health"]
    check(h.get("level") in HEALTH, f"{w}.health")
    keys(h, {"level"} | HEALTH[h["level"]], f"{w}.health")
    ids = set()
    for p in snap["participants"]:
        pw = f"{w}.participants[{p.get('address_id')}]"
        keys(p, {"address_id", "alias", "kind", "role", "persona", "pane_label", "activity", "mailbox", "waiting_on"}, pw)
        ids.add(p["address_id"])
        kind = p["kind"]
        check(kind.get("type") in {"human", "agent", "terminal"}, f"{pw}.kind")
        keys(kind, {"type", "runtime"} if kind["type"] == "agent" else {"type"}, f"{pw}.kind")
        for f in ("role", "persona", "pane_label"):
            safe(p[f], f"{pw}.{f}", nullable=True)
        check(p["activity"] in ACTIVITIES, f"{pw}.activity")
        for m in p["mailbox"]:
            keys(m, {"delivery_id", "message_id", "from", "preview", "state", "since_ms", "attempts"}, f"{pw}.mailbox")
            safe(m["preview"], f"{pw}.mailbox.preview")
            check(m["state"] in DELIVERY_STATES, f"{pw}.mailbox.state")
        check(all(isinstance(a, str) for a in p["waiting_on"]), f"{pw}.waiting_on")
    check("human" in ids, f"{w}: human participant")
    mids = [m["message_id"] for m in snap["messages"]]
    check([m["at_ms"] for m in snap["messages"]] == sorted(m["at_ms"] for m in snap["messages"]), f"{w}.messages ascending")
    for m in snap["messages"]:
        mw = f"{w}.messages[{m.get('message_id')}]"
        keys(m, {"message_id", "thread_id", "in_reply_to", "at_ms", "author", "kind", "body", "legs"}, mw)
        check(m["author"] in ids, f"{mw}.author")
        check(m["kind"] in MESSAGE_KINDS, f"{mw}.kind")
        safe(m["body"], f"{mw}.body")
        for leg in m["legs"]:
            keys(leg, {"to", "stage"}, f"{mw}.legs")
            check(leg["to"] in ids, f"{mw}.legs.to")
            stage = leg["stage"].get("stage")
            check(stage in LEG_STAGES, f"{mw}.legs.stage")
            keys(leg["stage"], {"stage"} | LEG_STAGES[stage], f"{mw}.legs.stage")
            if stage == "replied":
                check(leg["stage"]["reply_id"] in mids, f"{mw}.legs.reply_id")
    for t in snap["threads"]:
        keys(t, {"thread_id", "title", "message_count"}, f"{w}.threads")
        safe(t["title"], f"{w}.threads.title")
    for c in snap["coordinations"]:
        cw = f"{w}.coordinations[{c.get('id')}]"
        keys(c, {"id", "owner", "deliverable", "outcome", "updated_at_ms", "assignments"}, cw)
        check(c["owner"] in ids and c["outcome"] in OUTCOMES, cw)
        safe(c["deliverable"], f"{cw}.deliverable")
        for a in c["assignments"]:
            keys(a, {"assignee", "work_outcome", "blocked_reason", "waiting_on"}, f"{cw}.assignments")
            check(a["assignee"] in ids and a["work_outcome"] in WORK_OUTCOMES, f"{cw}.assignments")
            check(a["waiting_on"] is None or a["waiting_on"] in ids, f"{cw}.assignments.waiting_on")
            safe(a["blocked_reason"], f"{cw}.blocked_reason", nullable=True)
    for r in snap["terminal_runs"]:
        keys(r, {"terminal", "command", "started_at_ms", "exit_code"}, f"{w}.terminal_runs")
        check(r["terminal"] in ids, f"{w}.terminal_runs.terminal")
        safe(r["command"], f"{w}.terminal_runs.command")
    for a in snap["attention"]:
        keys(a, {"source_kind", "summary", "opened_at_ms"}, f"{w}.attention")
        safe(a["summary"], f"{w}.attention.summary")


def shifted(value, delta):
    if isinstance(value, dict):
        return {k: (v + delta if k.endswith("_ms") and isinstance(v, int) else shifted(v, delta)) for k, v in value.items()}
    if isinstance(value, list):
        return [shifted(v, delta) for v in value]
    return value


def live(snap):
    return shifted(snap, int(time.time() * 1000) - snap["generated_at_ms"])


def variant(snaps, name):
    s = copy.deepcopy(snaps[0])
    s.update(messages=[], threads=[], coordinations=[], terminal_runs=[], attention=[], health={"level": "calm"})
    for p in s["participants"]:
        p.update(activity="idle", mailbox=[], waiting_on=[])
    if name == "not-activated":
        s["space"]["state"] = "not_activated"
        s["health"] = {"level": "inactive"}
        s["participants"] = [p for p in s["participants"] if p["address_id"] == "human"]
    return s


class Handler(BaseHTTPRequestHandler):
    snaps = []
    interval = 4.0

    def log_message(self, fmt, *args):
        sys.stderr.write("preview: " + fmt % args + "\n")

    def send(self, status, ctype, body):
        self.send_response(status)
        self.send_header("Content-Type", ctype)
        self.send_header("Content-Security-Policy", CSP)
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self):
        path = self.path.split("?", 1)[0]
        asset = {"/assets/app.js": "application/javascript", "/assets/app.css": "text/css"}
        if path in asset:
            return self.send(200, asset[path] + "; charset=utf-8", (ASSETS / path.rsplit("/", 1)[1]).read_bytes())
        if path == "/" or re.fullmatch(r"/w/[A-Za-z0-9-]+", path):
            return self.send(200, "text/html; charset=utf-8", (ASSETS / "index.html").read_bytes())
        if path == "/api/spaces":
            spaces = [{k: s["space"][k] for k in ("key", "workspace_id", "label", "state")} for s in self.snaps[:1]]
            spaces.append({"key": "3b71d0e9c24a5f6810aa", "workspace_id": "w7", "label": {"text": "herdr/main", "truncated": False}, "state": "paused"})
            spaces.append({"key": "c0de5a1e7b2f49d3e811", "workspace_id": "w9", "label": None, "state": "not_activated"})
            return self.send(200, "application/json", json.dumps(spaces).encode())
        m = re.fullmatch(r"/w/([A-Za-z0-9-]+)/events", path)
        if m:
            return self.stream(m.group(1))
        self.send(404, "text/plain", b"not found")

    def frame(self, event, data):
        self.wfile.write(f"event: {event}\ndata: {data}\n\n".encode())
        self.wfile.flush()

    def hold(self, snap):
        self.frame("snapshot", json.dumps(live(snap)))
        while True:
            time.sleep(15)
            self.wfile.write(b": ping\n\n")
            self.wfile.flush()

    def stream(self, key):
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        self.wfile.write(b"retry: 1000\n\n")
        try:
            step = re.fullmatch(r"step-(\d+)", key)
            if step:
                return self.hold(self.snaps[int(step.group(1)) - 1])
            if key in ("empty", "not-activated"):
                return self.hold(variant(self.snaps, key))
            if key == "restarting":
                self.frame("snapshot", json.dumps(live(self.snaps[-1])))
                self.frame("restarting", "{}")
                time.sleep(3600)
                return
            while True:
                for snap in self.snaps:
                    self.frame("snapshot", json.dumps(live(snap)))
                    time.sleep(self.interval)
        except (BrokenPipeError, ConnectionResetError):
            return


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("--interval", type=float, default=4.0, help="seconds between replayed snapshots")
    ap.add_argument("--check", action="store_true", help="validate fixtures against the wire contract and exit")
    args = ap.parse_args()
    snaps = json.loads(FIXTURES.read_text())
    for i, snap in enumerate(snaps):
        validate(snap, i)
    if args.check:
        print(f"{len(snaps)} snapshots match the wire contract")
        return
    Handler.snaps = snaps
    Handler.interval = args.interval
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.daemon_threads = True
    print(f"dashboard preview on http://127.0.0.1:{args.port}/w/{snaps[0]['space']['key']}")
    server.serve_forever()


if __name__ == "__main__":
    main()
