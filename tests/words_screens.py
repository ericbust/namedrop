"""The Words test's binary half (tests/words.rs): each interactive screen
against a scripted Rex host, page by page, its refusals, an installed
runtime's refusal of Herdr's packages, and runtime.py's standalone failures.
Each output goes to NAMEDROP_WORDS_CAPTURE as a JSON line of {surface, text}
for the sweep to check."""
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

CAPTURE = os.environ['NAMEDROP_WORDS_CAPTURE']
BINARY = Path(sys.argv[1])
RIGHT, ESC = '\x1b[C', '\x1b'


def capture(surface, text):
    if text:
        with open(CAPTURE, 'a') as log:
            log.write(json.dumps({'surface': surface, 'text': text}) + '\n')


def run(argv, *, env, input=None, timeout=20):
    child = subprocess.Popen(argv, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    print("child pid " + str(child.pid), flush=True)
    stdout, stderr = child.communicate(input, timeout=timeout)
    return subprocess.CompletedProcess(argv, child.returncode, stdout, stderr)


spec = importlib.util.spec_from_file_location("scripted_rex", Path(__file__).parent / "support/scripted_rex.py")
scripted = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scripted)
script = Path(__file__).resolve().parents[1] / "scripts/runtime.py"
python = [sys.executable, "-I", "-c", "import sys,runpy; from pathlib import Path; script=sys.argv.pop(1); sys.path.insert(0,str(Path(script).parent)); sys.argv[0]=script; runpy.run_path(script,run_name='__main__')", str(script)]


def installed(state, owner):
    """`state` with the binary published as its selected runtime."""
    runtime = state / 'runtime'
    digest = hashlib.sha256(BINARY.read_bytes()).hexdigest()
    release = runtime / 'releases' / digest
    release.mkdir(parents=True)
    shutil.copy2(BINARY, release / 'namedrop')
    shutil.copy2(Path(__file__).resolve().parents[1] / 'runtime.json', release / 'runtime.json')
    (runtime / 'current').symlink_to('releases/' + digest)
    (runtime / 'owner').write_text(owner + '\n')
    launcher = runtime / 'namedrop'
    launcher.write_text('#!/bin/sh\nexec "$(dirname "$0")/current/namedrop" "$@"\n')
    launcher.chmod(0o755)
    return runtime / 'current/namedrop'


with tempfile.TemporaryDirectory(prefix="nd-words-", dir="/tmp") as folder:
    root = Path(folder)
    home, state = root / "home", root / "state"
    home.mkdir()
    (state / "runtime").mkdir(parents=True)
    (state / "runtime/owner").write_text("standalone\n")
    targets = {"NAMEDROP_TARGET_WORKSPACE": "s1", "NAMEDROP_TARGET_PANE": "p1",
               "NAMEDROP_TARGET_TERMINAL": "p1", "NAMEDROP_TARGET_SESSION": ""}
    env = {"HOME": str(home), "PATH": "/usr/bin:/bin", "NAMEDROP_STATE_DIR": str(state), **targets}
    sock = root / "r.sock"
    endpoint = ["--endpoint", "rex:" + str(sock), "--space", "s1", "--pane", "p1"]
    foreground_herdr = False

    def answer(method):
        if method == "server.status":
            return {"pid": os.getpid(), "listeners": [{"primary": True, "transport": "unix", "addresses": [str(sock)]}], "host_id": "local"}
        if method == "session.list":
            return {"sessions": [{"session_id": "s1", "label": "words"}]}
        if method in ("session.attach", "session.attach_all", "client.set_label"):
            return {"view": {"windows": []}}
        if method == "session.list_blocks":
            return {"blocks": [{"block_id": "p1", "window_id": "window:w", "creator_name": "com.superlogical.terminal"}]}
        if method == "com.superlogical.terminal.program_status":
            return {"records": []}
        if method == "com.superlogical.terminal.process":
            return {"foreground": {"pid": os.getpid(), "name": "herdr"}} if foreground_herdr else {}
        if method == "com.superlogical.terminal.format":
            return {"content": ""}
        if method == "target.resolve":
            return {"results": [{"status": "resolved", "target": {"kind": "block", "id": "p1", "session_id": "s1"}}]}
        raise AssertionError("unhandled scripted method " + method)
    held = []

    def hold_bridge(at):
        """Holds `at`'s bridge lock, so its commands start no bridge. A
        bridge would register its relay with `client.keymap` and keep the
        scripted server, which serves one connection at a time, from the
        command under test."""
        bridge = at / "rex-bridge"
        bridge.mkdir()
        lock = (bridge / (hashlib.sha256(("rex:" + str(sock)).encode()).hexdigest() + ".lock")).open("w")
        fcntl.flock(lock, fcntl.LOCK_EX)
        held.append(lock)
    hold_bridge(state)
    server = scripted.Server(sock, answer)
    print("scripted server pid " + str(os.getpid()))
    try:
        # Every page of each screen: the arrows page Doctor and the tour.
        for mode, expected, keys in [("name", "Namedrop is not active", ESC), ("manage", "Manage names", ESC),
                                     ("doctor", "Namedrop needs attention.", RIGHT * 3 + ESC),
                                     ("status", "Facts", ESC), ("tour", "Welcome to Namedrop", RIGHT * 4 + ESC)]:
            out = run([str(BINARY), *endpoint, "ui", "run", mode], env=env, input=keys, timeout=20)
            text = out.stdout + out.stderr
            assert out.returncode == 0, (mode, out.returncode, text)
            assert expected in text, (mode, text)
            capture("ui run " + mode, text)
            print(mode + " screen passed")
        out = run([str(BINARY), *endpoint, "ui", "run", "tour"], env=env, input=RIGHT * 4 + ESC, timeout=20)
        for page in ("Names belong to this Rex session.", "Run Agent status from Rex's palette to see what each agent is doing.",
                     "Run Namedrop Doctor from Rex's palette. It explains", "Run Namedrop tour from Rex's palette to read these pages again."):
            assert page in out.stdout.replace("\n", " ").replace("  ", " "), (page, out.stdout)
        # The refusals each screen gives a changed, closed or unnamed target.
        bare = {key: value for key, value in env.items() if not key.startswith("NAMEDROP_TARGET_")}
        for mode in ("name", "manage", "doctor", "tour", "status"):
            for label, changed in [("no targets", {}), ("no pane", dict(targets, NAMEDROP_TARGET_PANE=None)),
                                   ("no terminal", dict(targets, NAMEDROP_TARGET_TERMINAL=None)),
                                   ("closed pane", dict(targets, NAMEDROP_TARGET_PANE="p9")),
                                   ("replaced terminal", dict(targets, NAMEDROP_TARGET_TERMINAL="t9")),
                                   ("new session", dict(targets, NAMEDROP_TARGET_SESSION="agent-9"))]:
                changed = {key: value for key, value in changed.items() if value is not None}
                # A status panel rereads its pane on each pass; R starts one.
                keys = "rrr" + ESC if mode == "status" else "x" * 8 + ESC
                out = run([str(BINARY), *endpoint, "ui", "run", mode], env=dict(bare, **changed), input=keys, timeout=20)
                capture(f"ui run {mode} ({label})", out.stdout + out.stderr)
            for args in (["--endpoint", "rex:" + str(sock)], ["--endpoint", "rex:" + str(sock), "--space", "s1"]):
                out = run([str(BINARY), *args, "ui", "launch", mode], env=bare, timeout=20)
                capture(f"ui launch {mode} (no target)", out.stdout + out.stderr)
        foreground_herdr = True
        for mode, title in [("name", "Name this pane"), ("manage", "Manage Namedrop names and agents"),
                            ("doctor", "Namedrop Doctor"), ("status", "Agent status"), ("tour", "Namedrop tour")]:
            out = run([str(BINARY), *endpoint, "ui", "launch", mode], env=env, timeout=20)
            refusal = "This pane is running Herdr. Use Herdr's own " + title + "."
            assert out.returncode != 0 and refusal in out.stdout + out.stderr, (mode, out)
            capture(f"ui launch {mode} (Herdr in the foreground)", out.stdout + out.stderr)
            print(refusal)
        foreground_herdr = False
        # An installed Rex runtime under a HOME whose packages Herdr's install owns.
        herdr_state, rex_state = root / "herdr-state", root / "rex-state"
        installed(herdr_state, "herdr")
        selected = installed(rex_state, "standalone")
        hold_bridge(rex_state)
        record = home / ".config/namedrop/state-directory"
        record.parent.mkdir(parents=True)
        record.write_text(str(herdr_state) + "\n")
        rex_env = dict(env, NAMEDROP_STATE_DIR=str(rex_state))
        for args in (["--json", "activate", "--input-json", '{"confirmed":true}'], ["activate", "--input-json", '{"confirmed":true}'],
                     ["name", "words", "--type", "terminal"]):
            out = run([str(selected), "--plugin-state", str(rex_state), *endpoint, *args], env=rex_env, timeout=20)
            assert "ND-INSTALL-002" in out.stdout + out.stderr, (args, out)
            capture("activate (Herdr owns the packages)", out.stdout + out.stderr)
        print("installed refusal passed")
        # A Rex state that Herdr's install took over.
        retired = root / "retired-state"
        (retired / "runtime").mkdir(parents=True)
        (retired / "runtime/owner").write_text("standalone\n")
        (retired / "runtime/retired.json").write_text(json.dumps(
            {"reason": "herdr_handoff", "successor": str(herdr_state), "at": "2026-11-15T00:00:00Z"}))
        for args in (["doctor"], ["--json", "doctor"], ["activate", "--input-json", '{"confirmed":true}'], ["name", "words"]):
            out = run([str(BINARY), "--plugin-state", str(retired), *endpoint, *args], env=dict(env, NAMEDROP_STATE_DIR=str(retired)), timeout=20)
            assert "ND-INSTALL-001" in out.stdout + out.stderr, (args, out)
            capture("doctor (Herdr took the state over)", out.stdout + out.stderr)
        print("retired state passed")
    finally:
        server.close()
    # runtime.py's failures on a standalone state, and its busy gate.
    out = run([*python, str(root), "--state-dir", str(root / "missing"), "--verify", "install"], env=env, timeout=10)
    assert out.returncode == 2, out
    assert "Namedrop runtime installation stopped:" in out.stderr, out
    assert "activation" not in out.stderr.lower(), out.stderr
    capture("runtime.py install", out.stderr)
    print("standalone failure passed")
    out = run([*python, str(root), "--state-dir", str(root / "missing"), "--verify", "activate"], env=env, timeout=10)
    assert out.returncode == 2, out
    assert out.stderr == "Namedrop runtime activation stopped: Activation needs an absolute, existing state directory.\n", out.stderr
    print("original runtime failure passed")
    sys.path.insert(0, str(script.parent))
    import runtime
    found = runtime.gate_holders
    try:
        for holders in ([], [(42, 'namedrop ui run tour', True)], [(42, 'namedrop ui run tour', True), (43, 'namedrop reply', True)]):
            runtime.gate_holders = lambda path, holders=holders: holders
            for owner in ("standalone", "herdr"):
                (state / "runtime/owner").write_text(owner + "\n")
                message = runtime.busy_gate_message(state / "runtime/commands.lock")
                if owner == "standalone":
                    assert "Namedrop screen, such as the tour" in message, message
                    capture("runtime.py busy gate", message)
                else:
                    assert "Namedrop popup, such as the tour" in message, message
    finally:
        runtime.gate_holders = found
    print("busy gate passed")
