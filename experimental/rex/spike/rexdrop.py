#!/usr/bin/env python3
"""Throwaway Rex spike: name Rex blocks and deliver gated mentions between agents.

Not production code. See ../PLAN.md, step 1.
"""

import argparse
import contextlib
import fcntl
import json
import os
import subprocess
import sys
import time
import uuid

REX_BUNDLE = os.path.expanduser(
    "~/Library/Application Support/com.superlogical.rex/RexServer/Versions/Current/Rex Server.app/Contents/MacOS/rex"
)
OPEN_STATES = {"idle", "done"}


def rex_bin():
    if os.environ.get("REX_BIN_DIR"):
        return os.path.join(os.environ["REX_BIN_DIR"], "rex")
    return REX_BUNDLE


def rex(*args, server=None, check=True, stdin=None):
    server = server or os.environ.get("RD_SERVER")
    cmd = [rex_bin()] + (["-S", server] if server else []) + list(args)
    p = subprocess.run(cmd, capture_output=True, text=True, input=stdin)
    if check and p.returncode != 0:
        raise RuntimeError(f"rex {' '.join(args)}: {p.stderr.strip()}")
    return p


def rex_json(*args, **kw):
    out = rex(*args, **kw).stdout.strip()
    return json.loads(out) if out else None


def state_dir():
    d = os.environ.get("RD_STATE") or os.path.expanduser("~/.cache/rexdrop-spike")
    os.makedirs(d, exist_ok=True)
    return d


@contextlib.contextmanager
def locked_state():
    d = state_dir()
    with open(os.path.join(d, "state.lock"), "w") as lock:
        fcntl.flock(lock, fcntl.LOCK_EX)
        path = os.path.join(d, "state.json")
        try:
            with open(path) as f:
                st = json.load(f)
        except FileNotFoundError:
            st = {"aliases": {}, "queue": [], "log": []}
        yield st
        tmp = path + ".tmp"
        with open(tmp, "w") as f:
            json.dump(st, f, indent=1)
        os.replace(tmp, path)


def log(st, **entry):
    entry["at"] = time.strftime("%H:%M:%S")
    st["log"].append(entry)
    print(json.dumps(entry), flush=True)


def ancestors(pid):
    chain = []
    while pid > 1:
        chain.append(pid)
        out = subprocess.run(["ps", "-o", "ppid=", "-p", str(pid)], capture_output=True, text=True).stdout.strip()
        if not out:
            break
        pid = int(out)
    return chain


def verify_identity(session, block):
    """Prove the caller runs inside BLOCK: the block's child process must be our ancestor."""
    proc = json.loads(rex("block", "call", "-s", session, "-b", block, "process").stdout)
    child = (proc.get("child") or {}).get("pid")
    return child in ancestors(os.getpid()), child


def status(session, block):
    data = json.loads(rex("block", "call", "-s", session, "-b", block, "program_status").stdout)
    roots = [r.get("record", r) for r in data.get("records", []) if not r.get("id")]
    if not roots:
        return None, None
    return roots[0].get("state"), roots[0].get("kind")


def deliver(target, text):
    s, b = target["session"], target["block"]
    payload = json.dumps({"block_id": b, "event": {"type": "paste", "payload": {"text": text}}})
    rex("api", "call", "-s", s, "block.event", payload)
    time.sleep(0.3)
    rex("send-key", "-s", s, "-b", b, "enter")
    deadline = time.time() + 15
    while time.time() < deadline:
        state, _ = status(s, b)
        if state == "working":
            return "delivered"
        time.sleep(0.25)
    return "ambiguous"


def drain(force=False):
    with locked_state() as st:
        keep = []
        for msg in st["queue"]:
            target = st["aliases"].get(msg["to"])
            if not target:
                log(st, event="dropped", id=msg["id"], reason="unknown alias")
                continue
            state, kind = status(target["session"], target["block"])
            if not force and state not in OPEN_STATES:
                log(st, event="held", id=msg["id"], to=msg["to"], state=state, kind=kind)
                keep.append(msg)
                continue
            body = f"[@{msg['from']} via rexdrop] {msg['text']}"
            result = deliver(target, body)
            log(st, event=result, id=msg["id"], to=msg["to"], state_before=state)
        st["queue"] = keep


def cmd_name(a):
    session = a.session or os.environ["REX_SESSION"]
    block = a.block or os.environ["REX_BLOCK"]
    if not a.block:
        ok, child = verify_identity(session, block)
        if not ok:
            sys.exit(f"REX_BLOCK {block} is not this process's block (block child pid {child})")
    rex("block", "rename", "-s", session, block, f"@{a.alias}")
    with locked_state() as st:
        st["aliases"][a.alias] = {"session": session, "block": block}
        log(st, event="named", alias=a.alias, block=block)


def cmd_send(a):
    with locked_state() as st:
        st["queue"].append({"id": uuid.uuid4().hex[:8], "to": a.to, "from": a.sender, "text": a.text})
    drain(force=a.force)


def cmd_bridge(a):
    cmd = [rex_bin()] + (["-S", os.environ["RD_SERVER"]] if os.environ.get("RD_SERVER") else []) + ["events", "--all", "--json"]
    lockf = open(os.path.join(state_dir(), "bridge.lock"), "w")
    try:
        fcntl.flock(lockf, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        sys.exit("bridge already running")
    proc = subprocess.Popen(cmd, stdout=subprocess.PIPE, text=True)
    print("bridge listening", flush=True)
    for line in proc.stdout:
        ev = json.loads(line)
        if ev.get("type") != "session_event":
            continue
        inner = ev["payload"]["event"]
        if inner["type"] == "block_closed":
            block = inner["payload"].get("block_id")
            with locked_state() as st:
                for alias, t in list(st["aliases"].items()):
                    if t["block"] == block:
                        del st["aliases"][alias]
                        log(st, event="unnamed", alias=alias, reason="block_closed")
        elif inner["type"] == "block_event" and inner["payload"].get("name") == "program_status_changed":
            rec = inner["payload"]["payload"]
            if not rec.get("id") and rec.get("record", {}).get("state") in OPEN_STATES:
                drain()


def cmd_ls(a):
    with locked_state() as st:
        print(json.dumps({k: st[k] for k in ("aliases", "queue")}, indent=1))


def main():
    p = argparse.ArgumentParser()
    sub = p.add_subparsers(required=True)
    n = sub.add_parser("name")
    n.add_argument("alias")
    n.add_argument("--session")
    n.add_argument("--block")
    n.set_defaults(fn=cmd_name)
    s = sub.add_parser("send")
    s.add_argument("to")
    s.add_argument("text")
    s.add_argument("--from", dest="sender", default="operator")
    s.add_argument("--force", action="store_true")
    s.set_defaults(fn=cmd_send)
    sub.add_parser("drain").set_defaults(fn=lambda a: drain())
    sub.add_parser("bridge").set_defaults(fn=cmd_bridge)
    sub.add_parser("ls").set_defaults(fn=cmd_ls)
    a = p.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
