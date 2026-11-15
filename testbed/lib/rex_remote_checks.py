"""The remote-host lab's checks, numbered as PLAN #11's lab checks in experimental/rex/PORT.md.
Each check prints `ok`, `FAIL`, or `known` (a caveat PORT.md records) and the evidence it read,
and every result goes to LAB/reports/check-<time>.json. The checks need no model and no network
beyond the lab. They run in the order below, because 5 retires B's names and names them again,
and 6 and 7 restart B's server with an anonymous http listener."""

import sys

sys.dont_write_bytecode = True

import json
import re
import sqlite3
import statistics
import threading
import time

import rex_remote
from rex_remote import B_HOME, B_SOCKET, B_STATE, LABEL, HTTP_LABEL, fail, say

ZERO_HOST = "00000000-0000-0000-0000-000000000000"


class Check:
    def __init__(self, lab):
        self.lab = lab
        self.tag = time.strftime("%H%M%S")
        self.lines = []
        self.ok = True
        self.caveat = None

    def note(self, line):
        self.lines.append(line)

    def expect(self, condition, line):
        self.lines.append(("" if condition else "UNEXPECTED ") + line)
        self.ok = self.ok and bool(condition)

    def known(self, condition, line, caveat):
        """A behavior PORT.md records as a caveat: it does not fail the run, and the check says
        `known` until the behavior changes."""
        self.lines.append(("" if condition else "KNOWN ") + line)
        if not condition:
            self.caveat = caveat

    # Reads.

    def a_db(self, sql, *args, db="namedrop.sqlite3"):
        path = self.lab.path("a-namedrop", db)
        if not path.exists():
            return []
        connection = sqlite3.connect("file:{}?mode=ro".format(path), uri=True, timeout=10)
        try:
            return connection.execute(sql, args).fetchall()
        finally:
            connection.close()

    def b_db(self, sql, db="namedrop.sqlite3"):
        shown = self.lab.bx("sqlite3", "-readonly", "-json", "{}/{}".format(B_STATE, db), sql, check=False)
        if shown.returncode != 0:
            return None
        return json.loads(shown.stdout or "[]")

    def a_names(self):
        return sorted(row[0] for row in self.a_db("select alias from alias_claims where claim_state = 'active'"))

    def b_names(self):
        rows = self.b_db("select alias from alias_claims where claim_state = 'active'")
        return None if rows is None else sorted(row["alias"] for row in rows)

    def outbound(self, message):
        rows = self.a_db("select label, state, reason, host_id from outbound where message_id = ?", message,
                         db="relay.sqlite3")
        return rows[0] if rows else None

    def b_screen(self, block):
        return self.lab.b_rex("capture", "-b", block, "--trim", check=False).stdout

    def printed(self, block, token):
        """How many lines of the block's screen are exactly `token`."""
        return sum(1 for line in self.b_screen(block).splitlines() if line.strip() == token)

    # Calls.

    def timed(self, *args, server=LABEL, timeout="10s"):
        started = time.monotonic()
        result = self.lab.ar(*args, server=server, timeout=timeout)
        return result, int((time.monotonic() - started) * 1000)

    def host_id(self, server=LABEL):
        return json.loads(self.lab.ar("api", "call", "server.status", server=server, check=True).stdout)["host_id"]

    def bridge_client(self):
        resolved = json.loads(self.lab.ar("api", "call", "target.resolve",
                                          json.dumps({"queries": ["namedrop"], "kinds": ["client"]}),
                                          server=LABEL, check=True).stdout)
        for result in resolved.get("results") or []:
            if result.get("status") == "resolved":
                return result["target"]["client_id"]
        fail("B's bridge is not a client of B's server: {}".format(resolved))

    def perform(self, item, to, text, host=None, client=None, timeout="15s"):
        """One relay call as A's worker makes it, by hand: rc, the relay's answer or the error,
        and the time it took."""
        request = {"client_id": client or self.bridge_client(), "action": "namedrop.relay",
                   "args": {"op": "deliver", "expect_host_id": host or self.host_id(),
                            "from": {"host_id": "host-lab-probe", "name": "probe", "host": "probe-host"},
                            "item": {"id": item, "to": to, "thread": "thread-lab-" + self.tag,
                                     "in_reply_to": None, "text": text},
                            "ack_through": 0}}
        started = time.monotonic()
        result = self.lab.ar("api", "call", "client.perform", "-", server=LABEL, timeout=timeout,
                             stdin=json.dumps(request))
        elapsed = int((time.monotonic() - started) * 1000)
        if result.returncode != 0:
            return result.returncode, (result.stderr or result.stdout).strip().splitlines()[:1], elapsed
        return 0, answer_of(json.loads(result.stdout)), elapsed

    def a_send(self, source):
        a = self.lab.state["a"]
        sent = self.lab.a_namedrop("send", "--json", "--space", a["session"], "--pane", a["lead"],
                                   "--input-json", json.dumps({"request_id": "lab-" + source[-40:] + self.tag,
                                                               "source": source}))
        return message_id_of(json.loads(sent.stdout))

    def wait(self, what, seconds, step=1.0):
        deadline = time.monotonic() + seconds
        while True:
            value = what()
            if value or time.monotonic() >= deadline:
                return value
            time.sleep(step)


def answer_of(payload):
    """The relay's own answer inside a `client.perform` result."""
    while isinstance(payload, dict) and "status" not in payload and "result" in payload:
        payload = payload["result"]
    return payload


def message_id_of(payload):
    found = re.search(r'"message_id":\s*"([^"]+)"', json.dumps(payload))
    if not found:
        fail("no message id in {}".format(payload))
    return found.group(1)


def status(answer):
    return answer.get("status") if isinstance(answer, dict) else None


# 1. Rex over ssh.

def rex_over_ssh(check):
    lab = check.lab
    listed, ms = check.timed("hosts", "check", "--json", server=None)
    check.expect(listed.returncode == 0 and LABEL in listed.stdout, "hosts check rc {} in {} ms: {}".format(
        listed.returncode, ms, " ".join(listed.stdout.split())[:240]))
    who = json.loads(lab.ar("whoami", "--json", server=LABEL, check=True).stdout)
    principal = who.get("principal") or who
    uid = lab.bx("id", "-u").stdout.strip()
    check.expect(principal.get("kind") == "peer" and str(principal.get("uid")) == uid,
                 "whoami on {}: {}".format(LABEL, json.dumps(principal, sort_keys=True)))
    b = json.loads(lab.ar("api", "call", "server.status", server=LABEL, check=True).stdout)
    a = json.loads(lab.ar("api", "call", "server.status", check=True).stdout)
    check.note("A: Rex {} api {}; B: Rex {} api {}; B's host_id {}".format(
        lab.state["a_version"], a.get("api_revision"), lab.state["b_version"], b.get("api_revision"),
        b.get("host_id")))


# 2. The relay's answers over ssh.

def hand_check(check):
    lab = check.lab
    b = lab.state["b"]
    token = "relayed-{}".format(check.tag)
    text = "echo {}-$((40+2))".format(token)
    item = "m-{}-1".format(check.tag)
    for label, to, item_id, host, want in (
            ("accepted", "bterm", item, None, "accepted"),
            ("duplicate", "bterm", item, None, "duplicate"),
            ("unknown", "nobody", "m-{}-2".format(check.tag), None, "refused"),
            ("wrong host", "bterm", "m-{}-3".format(check.tag), ZERO_HOST, "refused")):
        rc, answer, ms = check.perform(item_id, to, text if to == "bterm" else "echo never", host=host)
        check.expect(rc == 0 and status(answer) == want, "{}: {} in {} ms".format(label, json.dumps(answer), ms))
    time.sleep(2)
    check.expect(check.printed(b["bterm"], token + "-42") == 1, "@bterm printed {}-42 once".format(token))
    # Two activated spaces of B that both hold @bdup.
    for index in (1, 2):
        key = "bdup{}".format(index)
        if key not in b:
            session, block = lab.new_b_session("nd-bdup{}".format(index), ["/bin/bash"])
            time.sleep(1)
            lab.b_namedrop("name", "@bdup", "--type", "terminal", "--space", session, "--pane", block, "--human")
            b[key] = block
            lab.save()
    rc, answer, ms = check.perform("m-{}-4".format(check.tag), "bdup", "echo never-ambiguous")
    check.expect(rc == 0 and status(answer) == "ambiguous", "two sessions with @bdup: {} in {} ms".format(
        json.dumps(answer), ms))
    lab.bx("rm", "-f", B_STATE + "/relay/allow-terminals")
    try:
        rc, answer, ms = check.perform("m-{}-5".format(check.tag), "bterm", "echo never-without-opt-in")
        check.expect(rc == 0 and status(answer) == "refused",
                     "terminal without relay/allow-terminals: {} in {} ms".format(json.dumps(answer), ms))
    finally:
        lab.bx("touch", B_STATE + "/relay/allow-terminals")
    rows = check.b_db("select message_id, recipient, outcome from inbound where host_id = 'host-lab-probe' "
                      "and message_id like 'm-{}-%' order by message_id".format(check.tag), db="relay.sqlite3")
    check.note("B's inbound rows: {}".format(json.dumps(rows)))


# 3. A full exchange, A's terminal to B's terminal and back.

def exchange(check):
    lab = check.lab
    b = lab.state["b"]
    token = "exchange-{}".format(check.tag)
    sent = check.a_send("@bterm@{} echo {}-$((40+2))".format(LABEL, token))
    started = time.monotonic()
    accepted = check.wait(lambda: (check.outbound(sent) or [None, None])[1] == "accepted", 60)
    check.expect(accepted, "A's item {} accepted by B after {:.1f} s".format(sent, time.monotonic() - started))
    check.expect(check.wait(lambda: check.printed(b["bterm"], token + "-42") == 1, 20),
                 "@bterm on B ran it once")
    local = check.wait(lambda: next((row["local_message_id"] for row in check.b_db(
        "select local_message_id from inbound where message_id = '{}'".format(sent), db="relay.sqlite3") or []
        if row["local_message_id"]), None), 20)
    check.expect(local, "B's inbound row names the message B made of it")
    sender = check.b_db("select author_alias_snapshot from collaboration_messages where message_id = '{}'".format(local))
    sender = sender[0]["author_alias_snapshot"] if sender else None
    check.note("on B the message is {} from @{}".format(local, sender))
    reply = "pong-{}".format(check.tag)
    lab.b_namedrop("reply", "--json", "--space", b["session"], "--pane", b["bterm"], "--input-json",
                   json.dumps({"request_id": "lab-reply-" + check.tag, "in_reply_to": local,
                               "source": "@{} {}".format(sender, reply)}))
    replied = time.monotonic()
    found = check.wait(lambda: check.a_db("select author_alias_snapshot, thread_id from collaboration_messages "
                                          "where body like ?", "%" + reply + "%"), 75)
    check.expect(found, "A collected the reply {:.1f} s after B wrote it: {}".format(time.monotonic() - replied, found))
    thread = check.a_db("select thread_id from collaboration_messages where message_id = ?", sent)
    check.expect(found and thread and found[0][1] == thread[0][0], "the reply is in the thread A started")
    check.state = {"sent": sent, "thread": thread[0][0] if thread else None}
    lab.state["exchange"] = {"sent": sent, "local": local, "sender": sender}
    lab.save()


# 4. B goes away with a thread open and an item queued.

AWAY = ("server stopped", "container stopped", "paused", "restarted")


def go_away(lab, mode):
    if mode == "server stopped":
        lab.stop_b()
    elif mode == "container stopped":
        lab.docker("stop", "-t", "5", lab.container)
    elif mode == "paused":
        lab.docker("pause", lab.container)
    elif mode == "restarted":
        lab.docker("restart", "-t", "5", lab.container)


def come_back(lab, mode, restore=True):
    if mode == "paused":
        lab.docker("unpause", lab.container)
        return
    if mode == "container stopped":
        lab.docker("start", lab.container)
    lab.wait_sshd()
    lab.start_b(restore=restore)
    lab.start_b_bridge()


def probes(check, line):
    """The relay's read calls against an away B: each must fail with rc 4, never `not_found`."""
    b = check.lab.state["b"]
    for args in (("api", "call", "server.status"),
                 ("api", "call", "target.resolve", json.dumps({"queries": [b["bterm"]], "kinds": ["block"]})),
                 ("api", "call", "target.resolve", json.dumps({"queries": ["namedrop"], "kinds": ["client"]}))):
        result, ms = check.timed(*args)
        shown = " ".join((result.stderr + result.stdout).split())
        check.expect(result.returncode == 4 and "not_found" not in shown,
                     "{} {}: rc {} in {} ms: {}".format(line, args[2], result.returncode, ms, shown[:160]))


def away(check):
    lab = check.lab
    b = lab.state["b"]
    a_before, b_before = check.a_names(), check.b_names()
    check.note("names before: A {}, B {}".format(a_before, b_before))
    for mode in AWAY:
        token = "away-{}-{}".format(mode.split()[0], check.tag)
        go_away(lab, mode)
        if mode == "restarted":
            # A restarted container comes back with sshd and no Rex server.
            lab.wait_sshd()
        probes(check, mode)
        sent = check.a_send("@bterm@{} echo {}-$((40+2))".format(LABEL, token))
        time.sleep(15)
        held = check.outbound(sent)
        check.expect(held and held[1] == "queued", "{}: A's item stays queued after 15 s: {}".format(mode, held))
        check.expect(check.a_names() == a_before, "{}: A's names unchanged".format(mode))
        returned = time.monotonic()
        come_back(lab, mode)
        accepted = check.wait(lambda: (check.outbound(sent) or [None, None])[1] == "accepted", 120)
        check.expect(accepted, "{}: accepted {:.1f} s after B came back".format(mode, time.monotonic() - returned))
        check.wait(lambda: check.printed(b["bterm"], token + "-42") >= 1, 15)
        time.sleep(2)
        check.expect(check.printed(b["bterm"], token + "-42") == 1, "{}: @bterm ran it once".format(mode))
        check.expect(check.b_names() == b_before, "{}: B's names unchanged: {}".format(mode, check.b_names()))
    check.expect(check.a_names() == a_before, "A's names after the matrix: {}".format(check.a_names()))


# 5. B's server started without restore.

NO_RESTORE = ("a B server started with --restore=false answers session_not_found for B's sessions, so B's "
              "bridge retires their names, and the next normal start restores the blocks without them "
              "(PORT.md, \"The remote-host lab\")")


def no_restore(check):
    lab = check.lab
    b = lab.state["b"]
    before = check.b_names()
    lab.stop_b()
    lab.start_b(restore=False)
    lab.start_b_bridge()
    status_b = json.loads(lab.b_rex("api", "call", "server.status").stdout)
    check.note("server.status without restore: session_count {}, keys {}".format(
        status_b.get("session_count"), sorted(status_b)))
    resolved = lab.ar("api", "call", "target.resolve", json.dumps({"queries": [b["bterm"]], "kinds": ["block"]}),
                      server=LABEL)
    check.note("A asks B for @bterm's block: rc {} {}".format(resolved.returncode,
                                                             " ".join(resolved.stdout.split())[:160]))
    time.sleep(20)
    during = check.b_names()
    check.known(during == before, "B's names after 20 s of a server without restore: {} (before {})".format(
        during, before), NO_RESTORE)
    lab.stop_b()
    come_back(lab, "server stopped")
    time.sleep(10)
    after = check.b_names()
    resolved = lab.b_rex("api", "call", "target.resolve", json.dumps({"queries": [b["bterm"]], "kinds": ["block"]}))
    check.note("after the next normal start: @bterm's block {}".format(" ".join(resolved.stdout.split())[:160]))
    check.known(after == before, "B's names after the next normal start: {}".format(after), NO_RESTORE)
    if after != before:
        rename_b(check)


def rename_b(check):
    """Names @bterm and both @bdup again after a check that lost them, so later checks have them."""
    lab = check.lab
    b = lab.state["b"]
    labels = {session["label"]: session["session_id"]
              for session in json.loads(lab.b_rex("list", "--json").stdout).get("sessions") or []}
    for alias, label, block in (("bterm", "nd-bterm", b["bterm"]), ("bdup", "nd-bdup1", b.get("bdup1")),
                                ("bdup", "nd-bdup2", b.get("bdup2"))):
        if block and label in labels:
            lab.b_namedrop("name", "@" + alias, "--type", "terminal", "--space", labels[label],
                           "--pane", block, "--human")
    check.note("named again on B: {}".format(check.b_names()))


# 6 and 7. An anonymous http:// label, then two labels for one server.

def http_label(check, isolated):
    lab = check.lab
    lab.stop_b()
    lab.start_b(http=True)
    lab.start_b_bridge()
    listed = lab.ar("hosts", "list", "--json").stdout
    if HTTP_LABEL not in listed:
        lab.ar("hosts", "add", HTTP_LABEL, "http://127.0.0.1:{}".format(lab.state["http_port"]), check=True)
    if isolated:
        lab.ar("hosts", "remove", LABEL, check=True)
    try:
        who = lab.ar("whoami", "--json", server=HTTP_LABEL)
        check.note("whoami on {}: {}".format(HTTP_LABEL, " ".join(who.stdout.split())[:200]))
        token = "never-http-{}-{}".format("alone" if isolated else "both", check.tag)
        sent = check.a_send("@bterm@{} echo {}".format(HTTP_LABEL, token))
        settled = check.wait(lambda: (check.outbound(sent) or [None, None])[1] not in (None, "queued"), 60)
        row = check.outbound(sent)
        check.note("A's item: {}".format(row))
        notices = check.a_db("select body from collaboration_messages where thread_id = "
                             "(select thread_id from collaboration_messages where message_id = ?) "
                             "and message_id != ?", sent, sent)
        bodies = [body for (body,) in notices]
        check.note("A's thread got: {}".format(bodies))
        check.expect(settled and row[1] == "failed", "the item failed")
        check.expect(check.printed(lab.state["b"]["bterm"], token) == 0, "@bterm never ran it")
        return row, bodies
    finally:
        if isolated:
            lab.ar("hosts", "add", LABEL, "ssh://{}@127.0.0.1:{}".format(rex_remote.B_USER, lab.state["ssh_port"]),
                   check=True)


def anonymous(check):
    row, bodies = http_label(check, isolated=True)
    text = " ".join(bodies + [row[2] or "" if row else ""])
    check.expect("anonymous" in text and "a anonymous" not in text, "the notice names the anonymous caller")


def two_labels(check):
    row, bodies = http_label(check, isolated=False)
    text = " ".join(bodies + [row[2] or "" if row else ""])
    check.expect(LABEL in text, "the refusal names the other label {}".format(LABEL))
    lab = check.lab
    a = lab.state["a"]
    doctor = json.loads(lab.a_namedrop("doctor", "--json", "--space", a["session"], "--pane", a["lead"],
                                       check=False).stdout)
    issues = {issue["code"]: issue.get("summary", "") + " " + issue.get("explanation", "")
              for issue in (doctor.get("data") or {}).get("issues") or []}
    relay = issues.get("ND-RELAY-003", "")
    check.expect(LABEL in relay and HTTP_LABEL in relay, "A's Doctor ND-RELAY-003: {}".format(relay[:300]))
    check.note("A's Doctor codes: {}".format(sorted(issues)))
    lab.ar("hosts", "remove", HTTP_LABEL, check=True)
    lab.stop_b()
    come_back(lab, "server stopped")


# 8. A call that times out while B is paused, then the retry.

def timeout_retry(check):
    lab = check.lab
    b = lab.state["b"]
    host, client = check.host_id(), check.bridge_client()
    for delay in (0.0, 0.08, 0.1, 0.11, 0.12, 0.13, 0.14, 0.6):
        token = "retry-{}-{}".format(int(delay * 1000), check.tag)
        item = "m-{}-r{}".format(check.tag, int(delay * 1000))
        text = "echo {}-$((40+2))".format(token)
        timer = threading.Timer(delay, lambda: lab.docker("pause", lab.container))
        timer.start()
        rc, first, ms = check.perform(item, "bterm", text, host=host, client=client, timeout="5s")
        timer.join()
        lab.docker("unpause", lab.container)
        back = check.wait(lambda: lab.ar("api", "call", "server.status", server=LABEL, timeout="5s").returncode == 0, 30)
        check.expect(back, "pause after {} s: B answers again after the unpause".format(delay))
        rc2, second, ms2 = check.perform(item, "bterm", text, host=host, client=client)
        rc3, third, ms3 = check.perform(item, "bterm", text, host=host, client=client)
        time.sleep(2)
        once = check.printed(b["bterm"], token + "-42")
        check.expect(status(second) in ("accepted", "duplicate") and status(third) == "duplicate" and once == 1,
                     "pause after {} s: first rc {} {} in {} ms; retry {}; again {}; ran {} time(s)".format(
                         delay, rc, json.dumps(first)[:120], ms, status(second), status(third), once))


# 9. Linux reads on B: the lib's /proc tests, and a Codex stand-in's hook through dash and busybox.

STANDIN = r'''
import json, subprocess, sys, time
# A Codex stand-in. It runs as ~/lab/codex, a copy of python3, so its executable is not a
# shell, and runs Namedrop's SessionStart hook through the shell it is given, as Codex runs an
# installed hook wrapper through /bin/sh.
shell, session, namedrop, state = sys.argv[1:5]
command = "NAMEDROP_HOOK_RUNTIME=codex {} hook session-start --state-directory {} >/dev/null 2>&1; echo hook-exit=$?"
command = command.format(namedrop, state)
run = ["busybox", "sh", "-c", command] if shell == "busybox" else [shell, "-c", command]
done = subprocess.run(run, input=json.dumps({"session_id": session}), text=True, capture_output=True)
print(done.stdout.strip(), flush=True)
time.sleep(600)
'''
WRAPPER = '#!/bin/bash\n"$HOME/lab/codex" "$HOME/lab/standin.py" "$@"\n'
# The live tests that need no agent. The others need a Claude Code or Codex block.
LIVE = ("ws_reads_the_welcome_of_a_real_server", "control_reads_the_server_status_and_a_typed_error",
        "the_scratch_server_is_local_and_a_socket_forwarded_to_it_is_not",
        "a_line_typed_into_a_scratch_shell_runs_there", "the_bridge_stream_follows_a_real_session_from_start_to_kill")


def lib_tests(lab, *args, env=()):
    # The tests run below a shell, because a process that `docker exec` starts has no parent
    # inside the container, and one test reads its parent.
    command = " ".join(["{}/namedrop-lib-tests".format(B_HOME)] + list(args)) + " 2>&1; echo rc=$?"
    ran = lab.bx("env", *env, "sh", "-c", command, check=False).stdout
    summary = [line for line in ran.splitlines() if line.startswith("test result") or line.endswith("FAILED")]
    return ran.rstrip().endswith("rc=0"), summary


def linux_reads(check):
    lab = check.lab
    b = lab.state["b"]
    test_binary = lab.linux_test_binary()
    check.expect(test_binary, "the Linux lib test binary: {}".format(test_binary and test_binary.name))
    if test_binary:
        lab.docker("cp", str(test_binary), lab.container + ":" + B_HOME + "/namedrop-lib-tests")
        lab.docker("exec", lab.container, "chown", "nd:nd", B_HOME + "/namedrop-lib-tests")
        for filter_ in ("mux::processes::", "mux::rex::binding::"):
            passed, summary = lib_tests(lab, filter_, "--test-threads=1")
            check.expect(passed, "lib tests {} on B: {}".format(filter_, summary))
        for name in LIVE:
            passed, summary = lib_tests(lab, "mux::rex::live::" + name, "--exact", "--ignored", "--test-threads=1",
                                        env=("NAMEDROP_REX_LIVE=unix://" + B_SOCKET, "NAMEDROP_REX_LIVE_HOME=" + B_HOME,
                                             "NAMEDROP_REX_LIVE_SHELL=" + b["bterm"]))
            check.expect(passed, "live {} against B's server: {}".format(name, summary[-1:]))
    lab.bx("sh", "-c", "mkdir -p ~/lab && cp /usr/bin/python3 ~/lab/codex && cat > ~/lab/standin.py", stdin=STANDIN)
    lab.bx("sh", "-c", "cat > ~/lab/codex-wrapper && chmod +x ~/lab/codex-wrapper", stdin=WRAPPER)
    for shell in ("/bin/sh", "busybox"):
        session = "standin-{}-{}".format(shell.strip("/").replace("/", "-"), check.tag)
        space, block = lab.new_b_session("nd-" + session, ["/bin/bash", B_HOME + "/lab/codex-wrapper", shell, session,
                                                           rex_remote.B_NAMEDROP, B_STATE])
        check.wait(lambda: "hook-exit=" in check.b_screen(block), 20)
        screen = " ".join(check.b_screen(block).split())[-40:]
        binding = lab.bx("sh", "-c", "cat {}/rex/sessions/* 2>/dev/null | grep -F -- '{}'".format(B_STATE, session),
                         check=False).stdout
        refusal = lab.bx("sh", "-c", "cat {}/rex/refusals/* 2>/dev/null | grep -F -- '{}'".format(B_STATE, session),
                         check=False).stdout
        check.expect(binding and not refusal, "bash wrapper, Codex stand-in, {}, hook ({}): bound {}{}".format(
            shell, screen, " ".join(binding.split())[:260], " refused " + refusal[:200] if refusal else ""))
        lab.b_rex("kill", space, check=False)


# 10. What a relay call over ssh costs.

def latency(check):
    lab = check.lab
    calls = lab.path("logs/ssh-calls.log")
    times = []
    before = len(calls.read_text().splitlines()) if calls.exists() else 0
    for _ in range(10):
        result, ms = check.timed("api", "call", "server.status")
        times.append(ms)
    after = len(calls.read_text().splitlines())
    local = []
    for _ in range(10):
        started = time.monotonic()
        lab.ar("api", "call", "server.status", check=True)
        local.append(int((time.monotonic() - started) * 1000))
    check.note("rex -S {} server.status: median {} ms, max {} ms; ssh runs per call: {}; A's own socket: median {} ms".format(
        LABEL, statistics.median(times), max(times), (after - before) / 10, statistics.median(local)))
    started = time.monotonic()
    rc, answer, ms = check.perform("m-{}-lat".format(check.tag), "nobody", "echo never")
    check.note("a hand relay call: client.perform {} ms; with its server.status and target.resolve, 3 rex "
               "processes and 3 tunnels, {} ms".format(ms, int((time.monotonic() - started) * 1000)))
    start = len(calls.read_text().splitlines())
    time.sleep(65)
    lines = calls.read_text().splitlines()[start:]
    tunnels = sum(1 for line in lines if " -N " in line)
    seconds = [line.split()[0] for line in lines]
    check.note("A's worker over 65 s with a thread open: {} ssh runs, {} of them tunnels, at {}".format(
        len(lines), tunnels, " ".join(seconds)))
    check.expect(all(ms < 5000 for ms in times), "every call under 5 s")


CHECKS = (
    (1, "Rex over ssh: hosts check, whoami, versions", rex_over_ssh),
    (2, "the relay's answers over ssh", hand_check),
    (3, "full exchange, A's terminal to B's terminal and back", exchange),
    (4, "away matrix with a thread open and an item queued", away),
    (5, "B's server started with --restore=false", no_restore),
    (8, "timeout during client.perform, then the retry", timeout_retry),
    (9, "Linux reads on B", linux_reads),
    (10, "latency over ssh", latency),
    (6, "anonymous http:// label refused with a notice", anonymous),
    (7, "two labels for one server", two_labels),
)


def run(lab, only):
    wanted = {int(number) for number in only.split(",")} if only else None
    results = []
    passed = True
    for number, title, body in CHECKS:
        if wanted and number not in wanted:
            continue
        check = Check(lab)
        started = time.monotonic()
        try:
            body(check)
        except Exception as error:  # A check that raises fails and the next one still runs.
            check.expect(False, "raised {}: {}".format(type(error).__name__, error))
        seconds = round(time.monotonic() - started, 1)
        verdict = "FAIL" if not check.ok else "known" if check.caveat else "ok"
        say("{:<5} {:>2}. {} ({} s)".format(verdict, number, title, seconds))
        for line in check.lines:
            say("        " + line)
        if check.caveat:
            say("        caveat: " + check.caveat)
        results.append({"number": number, "title": title, "verdict": verdict, "seconds": seconds,
                        "lines": check.lines, "caveat": check.caveat})
        passed = passed and check.ok
    report = lab.path("reports", "check-{}.json".format(time.strftime("%Y%m%d-%H%M%S")))
    report.parent.mkdir(parents=True, exist_ok=True)
    report.write_text(json.dumps({"lab": lab.state, "results": results}, indent=2, sort_keys=True) + "\n")
    say("Report: {}".format(report))
    return passed
