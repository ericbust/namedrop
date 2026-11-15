"""Every pane command here carries a caller-set NAMEDROP_HERDR_TIMEOUT_MS=7000 and
NAMEDROP_REX_TIMEOUT_MS=6000; the Stop hook runs as its wrapper does, with
NAMEDROP_HERDR_TIMEOUT_MS=250.

One Herdr session driven end to end through a Namedrop binary over the script
`herdr`, for a differential run of two builds.

usage: python3 -I flow.py BINARY OUT [STEP...]
Writes OUT/report: every step's exit, stdout and stderr, every `herdr` call
with its location-shaped environment and arguments (typed text included), and
every process started through the runtime launcher with its arguments and
environment. Paths, ids, nonces and times are normalized so two builds compare
by text."""
import tempfile
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import time

TOOLS = os.path.dirname(os.path.abspath(__file__))
binary, out = os.path.abspath(sys.argv[1]), os.path.abspath(sys.argv[2])
subprocess.run(["/bin/ps", "-axo", "pid=,command="], capture_output=True, check=True)
only = set(sys.argv[3:])
shutil.rmtree(out, ignore_errors=True)
root = out
for d in ("home", "repo", "repo2", "state"):
    os.makedirs(os.path.join(root, d))
root_real = os.path.realpath(root)
state = os.path.join(root_real, "state")
checkout = os.path.join(root_real, "repo")

digest = hashlib.sha256(open(binary, "rb").read()).hexdigest()
release = os.path.join(state, "runtime", "releases", digest)
os.makedirs(release)
shutil.copy2(binary, os.path.join(release, "namedrop"))
shutil.copy2(os.path.join(os.path.dirname(binary), "..", "..", "runtime.json"), os.path.join(release, "runtime.json"))
os.symlink(os.path.join("releases", digest), os.path.join(state, "runtime", "current"))
shutil.copy2(os.path.join(TOOLS, "launcher"), os.path.join(state, "runtime", "namedrop"))
os.chmod(os.path.join(state, "runtime", "namedrop"), 0o755)
cli = os.path.join(release, "namedrop")

os.makedirs(os.path.join(root_real, "bin"))
with open(os.path.join(root_real, "bin", "open"), "w") as opener:
    opener.write('#!/bin/sh\necho "open $*" >> "$ROOT/opened"\n')
os.chmod(os.path.join(root_real, "bin", "open"), 0o755)
BASE = {
    "PATH": os.path.join(root_real, "bin") + ":/usr/bin:/bin",
    "HOME": os.path.join(root_real, "home"),
    "TMPDIR": root_real,
    "ROOT": root_real,
    "CHECKOUT": checkout,
    "SHELLPID": str(os.getpid()),
    "HERDR_BIN_PATH": os.path.join(TOOLS, "herdr"),
    "XDG_CONFIG_HOME": os.path.join(root_real, "home", ".config"),
}
SESSION = {"p1": ("CODEX_SESSION_ID", "session-1"), "p2": ("CLAUDE_CODE_SESSION_ID", "session-2"),
           "p4": ("CLAUDE_CODE_SESSION_ID", "session-4"), "p5": ("CODEX_SESSION_ID", "session-5")}


def pane(p, **extra):
    env = {"HERDR_SOCKET_PATH": SOCK, "HERDR_WORKSPACE_ID": "w1", "HERDR_PANE_ID": p}
    env.update(NOISE)
    if p in SESSION:
        env[SESSION[p][0]] = SESSION[p][1]
    env.update(extra)
    return env


def plugin(p, **extra):
    return pane(p, HERDR_PLUGIN_STATE_DIR=state, HERDR_PLUGIN_ID="namedrop", **extra)


report = []
STEP_FAILURES = []
# Each build refuses these remote-shaped sends by design; any other exit fails the flow.
EXPECTED_EXIT = {"send-remote-shaped-0": 64, "send-remote-shaped-2": 65, "send-remote-shaped-3": 64}

import socket as _socket
import threading
LINK = tempfile.mkdtemp(prefix="nd-diff-", dir="/tmp")
os.rmdir(LINK)
if os.path.islink(LINK):
    os.unlink(LINK)
os.symlink(root_real, LINK)
SOCK = LINK + "/h.sock"
_listener = _socket.socket(_socket.AF_UNIX)
_listener.bind(SOCK)
_listener.listen(64)
def _accept():
    while True:
        try:
            c, _ = _listener.accept(); c.close()
        except OSError:
            return
threading.Thread(target=_accept, daemon=True).start()
BASE["SOCK"] = SOCK
NOISE = {"REX_BLOCK": "block:noise", "REX_SESSION": "session:noise", "REX_SERVER": "unix:///nonexistent/rex.sock",
         "REX_CLIENT": "client:noise", "REX_BIN_DIR": "/noise/bin", "CLAUDECODE": "1", "CODEX_THREAD_ID": "thread-noise",
         "NAMEDROP_HERDR_TIMEOUT_MS": "7000", "NAMEDROP_REX_TIMEOUT_MS": "6000"}


def norm(text):
    text = text.replace(SOCK, "<SOCK>").replace(LINK, "<LINK>").replace(release, "<RELEASE>").replace(state, "<STATE>").replace(root_real, "<ROOT>")
    text = text.replace(digest, "<DIGEST>")
    text = re.sub(r"[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}", "<UUID>", text)
    text = re.sub(r"\b[0-9a-f]{16,}\b", "<HEX>", text)
    text = re.sub(r"<HEX>-[0-9]+-", "<HEX>-<N>-", text)
    text = re.sub(r"Namedrop message [0-9a-f]{8}\b", "Namedrop message <NONCE>", text)
    text = re.sub(r"\b1[0-9]{9,12}\b", "<TIME>", text)
    text = re.sub(r"\b[0-9]{4,}-[0-9]{6,}\b", "<PIDTIME>", text)
    text = re.sub(r"\"(pid|shell_pid|foreground_process_group_id)\": ?[0-9]+", r'"\1":<PID>', text)
    text = re.sub(r"(attempt|thread|message|delivery|observation|registration|binding|address|nonce|reminder|grant|claim|request|snapshot|batch|role|challenge)[-_][0-9a-z_-]{4,}", r"\1-<ID>", text)
    return text


def run(label, env, args, stdin="", settle=0.5):
    if only and label not in only:
        return ""
    full = dict(BASE)
    full.update(env)
    try:
        p = subprocess.run([cli, *args], env=full, cwd=checkout, input=stdin,
                           capture_output=True, text=True, timeout=60)
        code, stdout, stderr = p.returncode, p.stdout, p.stderr
    except subprocess.TimeoutExpired:
        code, stdout, stderr = "TIMEOUT", "", ""
    time.sleep(settle)
    if label != "doctor" and code != EXPECTED_EXIT.get(label, 0):
        STEP_FAILURES.append((label, code))
    report.append(f"## {label}\nexit={code}\n--stdout\n{norm(stdout)}\n--stderr\n{norm(stderr)}")
    return stdout


def calls_since(mark):
    path = os.path.join(root_real, "calls")
    lines = open(path).read().splitlines() if os.path.exists(path) else []
    return lines[mark:], len(lines)


ST = ["--plugin-state", state]
run("activate", pane("p1"), ST + ["--json", "activate", "--input-json", '{"confirmed":true}'])
run("ui-launch-name", pane("p1"), ST + ["--json", "ui", "launch", "name"])
run("ui-launch-doctor", pane("p1"), ST + ["--json", "ui", "launch", "doctor"])
run("name-p1", pane("p1"), ST + ["--json", "name", "alpha"])
run("name-p2", pane("p2"), ST + ["--json", "name", "beta"])
run("name-p3", pane("p3"), ST + ["--json", "name", "term", "--type", "terminal"])
for p, alias in (("p4", "rev"), ("p5", "cx")):
    run(f"create-{p}", pane("p1"), ST + ["--json", "address", "create", "--input-json",
        json.dumps({"role_name": "Reviewer", "role_description": "Review code", "alias": alias, "pane_id": p})], settle=1)
run("reconcile", plugin("p1"), ["--json", "reconcile"], settle=2)

prompts = []
for line in calls_since(0)[0]:
    call = json.loads(line)
    if call["args"][:2] == ["agent", "prompt"]:
        prompts.append(call["args"])
registrations = {}
for args in prompts:
    text = args[-1]
    match = re.search(r"Run this exact command, verify it returns structured status ok, then stop this turn: (.*)$", text, re.S)
    if match:
        target = args[2] if len(args) > 2 else "?"
        registrations[target] = match.group(1).strip()
for target, command in sorted(registrations.items()):
    p = re.search(r"--herdr-pane '([^']+)'|--pane '([^']+)'", command)
    pane_id = next(g for g in p.groups() if g)
    full = dict(BASE)
    full.update(pane(pane_id))
    rest = command.split(" ", 1)[1]
    proc = subprocess.run(["/bin/sh", "-c", f'"$0" {rest}', cli], env=full, cwd=checkout,
                          capture_output=True, text=True, timeout=60)
    if proc.returncode: STEP_FAILURES.append(("register-" + pane_id, proc.returncode))
    report.append(f"## register-{pane_id}\nexit={proc.returncode}\n--stdout\n{norm(proc.stdout)}\n--stderr\n{norm(proc.stderr)}")
    time.sleep(0.5)

run("send-beta", pane("p1"), ST + ["--json", "send", "--input-json", json.dumps({"author_kind": "agent_session", "request_id": "send-1", "source": "@beta are the tests green?"})], settle=3)
run("inbox-p2", pane("p2"), ST + ["--json", "delivery", "inbox", "--input-json", "{}"])
for n, source in enumerate(["@beta@devbox are you there?", "@beta @beta@devbox hi", "@beta see @gamma@devbox now", "@beta@DevBox, @beta: x"]):
    run(f"send-remote-shaped-{n}", pane("p1"), ST + ["--json", "send", "--input-json", json.dumps({"author_kind": "agent_session", "request_id": f"send-r{n}", "source": source})], settle=1)
run("prompt-remote-shaped", pane("p1"), ["hook", "user-prompt-submit", "--state-directory", state],
    stdin=json.dumps({"hook_event_name": "UserPromptSubmit", "session_id": "session-1", "prompt": "@beta@devbox hook ping"}), settle=2)
typed = [json.loads(line)["args"][-1] for line in calls_since(0)[0]
         if json.loads(line)["args"][:3] == ["agent", "prompt", "p2"]]
run("prompt-hook-p2", pane("p2"), ["hook", "user-prompt-submit", "--state-directory", state],
    stdin=json.dumps({"hook_event_name": "UserPromptSubmit", "session_id": "session-2", "prompt": typed[-1] if typed else "none typed"}), settle=2)
run("prompt-mention-p1", pane("p1"), ["hook", "user-prompt-submit", "--state-directory", state],
    stdin=json.dumps({"hook_event_name": "UserPromptSubmit", "session_id": "session-1", "prompt": "@beta hook ping"}), settle=3)
for runtime_dir in (".claude/skills/namedrop", ".agents/skills/namedrop"):
    for script in ("user-prompt-submit", "session-start"):
        wrapper = os.path.join(BASE["HOME"], runtime_dir, "scripts", script)
        if os.path.exists(wrapper):
            report.append(f"## wrapper {runtime_dir} {script}\n" + norm(open(wrapper).read()))
claude_wrapper = os.path.join(BASE["HOME"], ".claude/skills/namedrop/scripts/user-prompt-submit")
if os.path.exists(claude_wrapper):
    full = dict(BASE); full.update(pane("p4"))
    proc = subprocess.run(["/bin/sh", claude_wrapper], env=full, cwd=checkout, capture_output=True, text=True, timeout=60,
                          input=json.dumps({"hook_event_name": "UserPromptSubmit", "session_id": "session-4", "prompt_id": "prompt-4", "prompt": "@beta wrapper ping"}))
    if proc.returncode: STEP_FAILURES.append(("prompt-wrapper-p4", proc.returncode))
    report.append(f"## prompt-wrapper-p4\nexit={proc.returncode}\n--stdout\n{norm(proc.stdout)}\n--stderr\n{norm(proc.stderr)}")
    time.sleep(3)
run("send-term", pane("p1"), ST + ["--json", "send", "--input-json", json.dumps({"author_kind": "agent_session", "request_id": "send-2", "source": "@term ls"})], settle=3)
if True:
    profile = '(version 1)(allow default)(deny network-outbound (remote unix-socket (path-literal "%s")))(deny network-outbound (remote unix-socket (path-literal "%s")))' % (SOCK, os.path.join(root_real, "h.sock"))
    full = dict(BASE); full.update(pane("p1"))
    for label, argv in (("deferred-send", ST + ["--json", "send", "--input-json", json.dumps({"author_kind": "agent_session", "request_id": "send-3", "source": "@beta deferred hello"})]),
                        ("deferred-inbox", ST + ["--json", "delivery", "inbox", "--input-json", "{}"])):
        proc = subprocess.run(["/usr/bin/sandbox-exec", "-p", profile, cli, *argv], env=full, cwd=checkout, capture_output=True, text=True, timeout=60)
        if proc.returncode: STEP_FAILURES.append((label, proc.returncode))
        report.append(f"## {label}\nexit={proc.returncode}\n--stdout\n{norm(proc.stdout)}\n--stderr\n{norm(proc.stderr)}")
        time.sleep(4)
run("reminder", pane("p1"), ST + ["--json", "reminder", "set", "--input-json", json.dumps({"in": "1h", "body": "check", "request_id": "reminder-1"})], settle=2)
run("pause", pane("p1"), ST + ["--json", "space", "pause"])
run("doctor", pane("p1"), ST + ["--json", "doctor"])
run("resume", pane("p1"), ST + ["--json", "space", "resume"])
run("status-hook", plugin("p2", HERDR_PLUGIN_EVENT="pane.agent_status_changed",
                          HERDR_PLUGIN_EVENT_JSON=json.dumps({"event": "pane_agent_status_changed", "data": {"type": "pane_agent_status_changed", "pane_id": "p2", "workspace_id": "w1", "agent_status": "idle"}})),
    ["hook", "agent-status-changed"], settle=3)
for line in subprocess.run(["ps", "-axo", "pid=,command="], capture_output=True, text=True).stdout.splitlines():
    pid, _, command = line.strip().partition(" ")
    if command.startswith(root_real) and " reminder-clock " in command:
        subprocess.run(["kill", pid]); report.append("## killed the clock")
time.sleep(1)
run("stop-hook-p2", pane("p2", NAMEDROP_HERDR_TIMEOUT_MS="250"), ["hook", "stop", "--state-directory", state],
    stdin=json.dumps({"hook_event_name": "Stop", "session_id": "session-2", "last_assistant_message": "Yes, green.", "background_tasks": []}), settle=3)
run("mailbox-wake", pane("p1"), ["hook", "mailbox-wake", "--state-directory", state], settle=3)
run("channels", pane("p1"), ST + ["channels"], settle=3)

run("deactivate", pane("p1"), ST + ["--json", "deactivate"])
run("activate-again", pane("p1"), ST + ["--json", "activate", "--input-json", '{"confirmed":true}'])
run("name-inactive-space", dict(pane("p6"), HERDR_WORKSPACE_ID="w2"), ST + ["--json", "name", "inactive-term", "--type", "terminal"])

time.sleep(2)
live = []
mine = []
for line in subprocess.run(["ps", "-axo", "pid=,command="], capture_output=True, text=True).stdout.splitlines():
    pid, _, command = line.strip().partition(" ")
    if command.startswith(root_real):
        mine.append(pid)
        environment = subprocess.run(["ps", "eww", "-o", "command=", "-p", pid], capture_output=True, text=True).stdout
        names = sorted(set(re.findall(r"\b((?:HERDR|NAMEDROP|REX|CLAUDE[A-Z_]*|CODEX)_?[A-Z_]*=\S*)", environment[len(command):])))
        live.append(command + "  ENV " + " ".join(n for n in names if not n.startswith("HERDR_BIN_PATH")))
for pid in mine:
    subprocess.run(["kill", pid], stderr=subprocess.DEVNULL)
calls, _ = calls_since(0)
spawns = open(os.path.join(root_real, "spawns")).read().splitlines() if os.path.exists(os.path.join(root_real, "spawns")) else []


def canon(lines):
    return sorted(set(norm(line) for line in lines))


def files():
    rows = []
    for base in (BASE["HOME"], state):
        for folder, dirs, names in os.walk(base):
            links = [name for name in dirs if os.path.islink(os.path.join(folder, name))]
            for name in sorted(names + links):
                path = os.path.join(folder, name)
                rel = norm(path)
                if os.path.islink(path):
                    text = "link " + norm(os.readlink(path))
                elif name.startswith("namedrop.sqlite3"):
                    text = "<STORE>"
                else:
                    data = open(path, "rb").read()
                    try:
                        text = norm(data.decode("utf-8")) if b"\0" not in data else "<BINARY>"
                    except UnicodeDecodeError:
                        text = "<BINARY>"
                rows.append(json.dumps({"path": rel, "text": text}, sort_keys=True))
    return sorted(rows)


with open(os.path.join(out, "report"), "w") as f:
    f.write("\n".join(report))
    f.write("\n## herdr calls (set)\n" + "\n".join(canon(calls)))
    f.write("\n## spawns (set)\n" + "\n".join(canon(spawns)) + "\n")
    f.write("## live at the end (set)\n" + "\n".join(canon(live)) + "\n")
    f.write("## files (set)\n" + "\n".join(files()) + "\n")
_listener.close()
os.unlink(LINK)
print(out, "steps", len(report), "calls", len(calls), "spawns", len(spawns))

if STEP_FAILURES:
    raise SystemExit("Flow steps failed: " + repr(STEP_FAILURES))
