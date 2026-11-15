"""Drive U5 on a scratch Rex server. No agents or user configuration are used."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time


def wait_for(read, accepts, description):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        value = read()
        if accepts(value):
            return value
        time.sleep(0.1)
    raise AssertionError(description + ": " + repr(value))


def main():
    parser = argparse.ArgumentParser(__doc__)
    parser.add_argument("--namedrop", type=Path, required=True)
    parser.add_argument("--rex", type=Path, required=True)
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    args.out.mkdir(parents=True, exist_ok=True)
    root = Path(tempfile.mkdtemp(prefix="nd-u5-", dir="/tmp"))
    home, state = root / "home", root / "state"
    home.mkdir()
    endpoint = "rex:" + str(root / "s.sock")
    env = {"HOME": str(home), "PATH": "/usr/bin:/bin", "USER": "ebusta",
           "SHELL": "/bin/sh", "NAMEDROP_STATE_DIR": str(state)}
    runtime = state / "runtime"
    digest = hashlib.sha256(args.namedrop.read_bytes()).hexdigest()
    release = runtime / "releases" / digest
    release.mkdir(parents=True)
    shutil.copy2(args.namedrop, release / "namedrop")
    contract = Path(__file__).resolve().parent.parent / "runtime.json"
    shutil.copy2(contract, release / "runtime.json")
    (runtime / "current").symlink_to(Path("releases") / digest)
    launcher = runtime / "namedrop"
    launcher.write_text('#!/bin/sh\ncase " $* " in *" rex bridge "*|*" agent-status-worker "*|*" --deferred-worker "*|*" reminder-clock "*|*" relay worker "*) echo $$ >> "' + str(root / "helper-pids") + '";; esac\nexec "$(dirname "$0")/current/namedrop" "$@"\n')
    launcher.chmod(0o700)
    evidence = {"root": str(root), "binary_sha256": digest, "checks": []}
    server_log = (args.out / "server.log").open("w")
    server = subprocess.Popen([
        str(args.rex.resolve()), "server", "run", "--listen", "unix://" + str(root / "s.sock"),
        "--state-dir", str(root / "rex-state"), "--restore=false",
    ], env={**env, "REX_SERVER": "unix://" + str(root / "s.sock")},
        stdout=server_log, stderr=subprocess.STDOUT)

    def rex(*argv, check=True):
        result = subprocess.run([str(args.rex.resolve()), "--autostart=false", "-S",
                                 "unix://" + str(root / "s.sock"), *argv],
                                env=env, text=True, capture_output=True, timeout=20)
        if check and result.returncode:
            raise AssertionError((argv, result.stdout, result.stderr))
        return result

    def api(method, data):
        return json.loads(rex("api", "call", method, json.dumps(data)).stdout)

    def nd(*argv, check=True):
        result = subprocess.run([str(launcher), "--endpoint", endpoint, "--space", session,
                                 "--pane", pane, "--json", *argv],
                                env=env, text=True, capture_output=True, timeout=20)
        if check and result.returncode:
            raise AssertionError((argv, result.stdout, result.stderr))
        return result

    def capture(block):
        return rex("capture", "-s", session, "-b", block, "--trim", check=False).stdout

    def send_key(block, key):
        rex("send-key", "-s", session, "-b", block, key)

    def blocks():
        return {b["block_id"] for b in api("session.list_blocks", {"session_id": session})["blocks"]}

    def launch(mode):
        result = json.loads(nd("ui", "launch", mode).stdout)
        return result["data"]["block_ids"][0]

    try:
        wait_for(lambda: (root / "s.sock").exists() or server.poll() is not None,
                 bool, "server did not start")
        if server.poll() is not None:
            raise AssertionError("scratch Rex server exited; see server.log")
        evidence["version"] = rex("version").stdout
        created = json.loads(rex("new", "u5", "--shell", "none", "--json", "--", "/bin/sh", "-i").stdout)
        session = created["session_id"]
        pane = created["initial_windows"][0]["block_ids"][0]
        window = created["initial_windows"][0]["window_id"]
        layer = launch("name")
        wait_for(lambda: capture(layer), lambda text: "Press Enter to activate it" in text,
                 "inactive naming did not offer activation")
        send_key(layer, "escape")
        wait_for(blocks, lambda ids: layer not in ids, "cancelled naming layer stayed open")
        assert not (state / "namedrop.sqlite3").exists()
        layer = launch("name")
        wait_for(lambda: capture(layer), lambda text: "Press Enter to activate it" in text, "activation offer")
        send_key(layer, "enter")
        wait_for(lambda: capture(layer), lambda text: "Name this pane" in text, "activation did not continue naming")
        assert blocks() == {pane, layer}, "activation opened another layer"
        assert not (state / "tours-shown" / "welcome-1").exists(), "activation claimed the tour"
        rex("send", "-s", session, "-b", layer, "u5-terminal")
        send_key(layer, "enter")
        send_key(layer, "enter")
        wait_for(blocks, lambda ids: layer not in ids, "successful naming layer stayed open")
        addresses = json.loads(nd("address", "list").stdout)["data"]
        assert any(a["alias"] == "u5-terminal" and a["pane_id"] == pane for a in addresses), addresses
        evidence["checks"].append("inactive offer, Esc cancels, Enter activates without tour, names original block, layer closes")
        for mode, needle in [("name", "Name this pane"), ("manage", "u5-terminal"),
                             ("doctor", "Namedrop"), ("tour", "Namedrop"), ("status", "Agent")]:
            layer = launch(mode)
            text = wait_for(lambda: capture(layer), lambda text: needle.lower() in text.lower(), mode + " did not paint")
            (args.out / (mode + ".txt")).write_text(text)
            send_key(layer, "escape")
            wait_for(blocks, lambda ids: layer not in ids, mode + " layer did not close")
            evidence["checks"].append(mode + " opens, paints and closes")
        launcher.rename(runtime / "saved-launcher")
        result = subprocess.run([str(release / "namedrop"), "--endpoint", endpoint,
                                 "--space", session, "--pane", pane, "ui", "launch", "name"],
                                env=env, text=True, capture_output=True, timeout=20)
        assert result.returncode and "Run sh scripts/install.sh again." in result.stdout + result.stderr, result
        assert blocks() == {pane}, "missing launcher opened a layer"
        (runtime / "saved-launcher").rename(launcher)
        evidence["checks"].append("missing launcher refuses before opening a layer")
        for delay in [0, 1]:
            failer = root / "refuse"
            failer.write_text('#!/bin/sh\nsleep ' + str(delay) + '\nexec "' + str(launcher) + '" --endpoint "' + endpoint + '" ui run name\n')
            failer.chmod(0o700)
            reply = api("session.new_layer", {"session_id": session, "window_id": window,
                "bounds": [0.2, 0.2, 0.6, 0.6], "layout": {"block": {
                    "flavor": "com.superlogical.terminal.shell", "options": {
                        "shell": "none", "login": False, "command": [str(failer)]}}}})
            layer = reply["block_ids"][0]
            wait_for(lambda: capture(layer), lambda text: "Press any key to close" in text, "refusal did not wait")
            assert layer in blocks()
            send_key(layer, "x")
            wait_for(blocks, lambda ids: layer not in ids, "refusal did not close on one key")
            evidence["checks"].append("refusal after " + str(delay) + "s waits for one key")
        missing = root / "removed-launcher"
        missing.write_text("#!/bin/sh\nexit 0\n")
        missing.chmod(0o700)
        assert missing.is_file() and os.access(missing, os.X_OK)
        missing.unlink()
        reply = api("session.new_layer", {"session_id": session, "window_id": window,
            "bounds": [0.2, 0.2, 0.6, 0.6], "layout": {"block": {
                "flavor": "com.superlogical.terminal.shell", "options": {
                    "shell": "none", "login": False, "command": ["/usr/bin/env", str(missing)]}}}})
        layer = reply["block_ids"][0]
        facts = wait_for(lambda: api("com.superlogical.terminal.process", {
            "session_id": session, "block_id": layer, "args": {}}),
            lambda facts: facts.get("last_exit") is not None, "env did not report missing launcher")
        assert facts["last_exit"]["exit_code"] == 127, facts
        evidence["checks"].append("launcher absent at spawn leaves env layer with exit 127")
        evidence["passed"] = True
    finally:
        if server.poll() is None:
            server.terminate()
            server.wait(timeout=10)
        server_log.close()
        recorded = root / "helper-pids"
        evidence["helper_pids"] = sorted({int(pid) for pid in recorded.read_text().split()}) if recorded.exists() else []
        for pid in evidence["helper_pids"]:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        (args.out / "report.json").write_text(json.dumps(evidence, indent=2) + "\n")
    print(json.dumps(evidence, indent=2))


if __name__ == "__main__":
    main()
