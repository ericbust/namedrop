"""U11 renders each interactive screen against a scripted Rex host, with no agents."""
import fcntl
import hashlib
import importlib.util
import os
from pathlib import Path
import subprocess
import sys
import tempfile

def run(argv, *, env, input=None, timeout=20):
    child = subprocess.Popen(argv, env=env, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    print("child pid " + str(child.pid), flush=True)
    stdout, stderr = child.communicate(input, timeout=timeout)
    return subprocess.CompletedProcess(argv, child.returncode, stdout, stderr)


spec = importlib.util.spec_from_file_location("scripted_rex", Path(__file__).parent / "support/scripted_rex.py")
scripted = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scripted)

with tempfile.TemporaryDirectory(prefix="nd-words-", dir="/tmp") as folder:
    root = Path(folder)
    home, state = root / "home", root / "state"
    home.mkdir()
    (state / "runtime").mkdir(parents=True)
    (state / "runtime/owner").write_text("standalone\n")
    env = {"HOME": str(home), "PATH": "/usr/bin:/bin", "NAMEDROP_STATE_DIR": str(state),
           "NAMEDROP_TARGET_WORKSPACE": "s1", "NAMEDROP_TARGET_PANE": "p1",
           "NAMEDROP_TARGET_TERMINAL": "p1", "NAMEDROP_TARGET_SESSION": ""}
    sock = root / "r.sock"
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
    # Hold the bridge's lock so screen commands start no background process.
    bridge = state / "rex-bridge"
    bridge.mkdir()
    lock = (bridge / (hashlib.sha256(("rex:" + str(sock)).encode()).hexdigest() + ".lock")).open("w")
    fcntl.flock(lock, fcntl.LOCK_EX)
    server = scripted.Server(sock, answer)
    print("scripted server pid " + str(os.getpid()))
    try:
        for mode, expected in [("name", "Namedrop is not active"), ("manage", "Manage names"),
                               ("doctor", "Namedrop needs attention."), ("status", "Facts"),
                               ("tour", "Welcome to Namedrop")]:
            out = run([sys.argv[1], "--endpoint", "rex:" + str(sock), "--space", "s1", "--pane", "p1", "ui", "run", mode],
                                 env=env, input="\x1b", timeout=20)
            text = out.stdout + out.stderr
            assert out.returncode == 0, (mode, out.returncode, text)
            assert expected in text, (mode, text)
            assert "herdr" not in text.lower(), (mode, text)
            print(mode + " screen passed")
        foreground_herdr = True
        for mode, title in [("name", "Name this pane"), ("manage", "Manage Namedrop names and agents"),
                            ("doctor", "Namedrop Doctor"), ("status", "Agent status"), ("tour", "Namedrop tour")]:
            out = run([sys.argv[1], "--endpoint", "rex:" + str(sock), "--space", "s1", "--pane", "p1", "ui", "launch", mode],
                                 env=env, timeout=20)
            refusal = "This pane is running Herdr. Use Herdr's own " + title + "."
            assert out.returncode != 0 and refusal in out.stdout + out.stderr, (mode, out)
            assert "herdr" not in (out.stdout + out.stderr).replace(refusal, "").lower(), out
            print(refusal)
    finally:
        server.close()
    script = Path(__file__).resolve().parents[1] / "scripts/runtime.py"
    python = [sys.executable, "-I", "-c", "import sys,runpy; from pathlib import Path; script=sys.argv.pop(1); sys.path.insert(0,str(Path(script).parent)); sys.argv[0]=script; runpy.run_path(script,run_name='__main__')", str(script)]
    out = run([*python, str(root), "--state-dir", str(root / "missing"), "--verify", "install"],
                         env=env, timeout=10)
    assert out.returncode == 2, out
    assert "Namedrop runtime installation stopped:" in out.stderr, out
    assert "activation" not in out.stderr.lower(), out.stderr
    print("standalone failure passed")
    out = run([*python, str(root), "--state-dir", str(root / "missing"), "--verify", "activate"],
                         env=env, timeout=10)
    assert out.returncode == 2, out
    assert out.stderr == "Namedrop runtime activation stopped: Activation needs an absolute, existing state directory.\n", out.stderr
    print("original runtime failure passed")
