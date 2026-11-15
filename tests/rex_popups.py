"""Drive U5 on a scratch Rex server. No agents or user configuration are used."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import pwd
import re
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


# What the probe's runtime may leave running; anything else under the root is a stray.
HELPERS = (" rex bridge ", " hook agent-status-worker", " --deferred-worker ", " reminder-clock ",
           " relay worker ")


def probe_processes(root):
    """Every live process whose command lies under the probe root, with its start time."""
    listing = subprocess.run(["/bin/ps", "-axo", "pid=,lstart=,command="], capture_output=True,
                             text=True, check=True).stdout
    prefixes = tuple({str(root) + "/", str(root.resolve()) + "/"})
    found = []
    for line in listing.splitlines():
        fields = line.split(None, 6)
        if len(fields) == 7 and fields[6].startswith(prefixes) and int(fields[0]) != os.getpid():
            found.append({"pid": int(fields[0]), "start": " ".join(fields[1:6]), "command": fields[6] + " "})
    return found


def stop_probe_processes(root, quiet=0):
    """SIGTERM the probe's processes, each only while its pid still names the
    same process: the same start time and a command under the root. With
    `quiet`, keep scanning until that many seconds pass with none found."""
    stopped, last_seen, deadline = [], time.monotonic(), time.monotonic() + 60
    while True:
        found = probe_processes(root)
        for process in found:
            again = subprocess.run(["/bin/ps", "-p", str(process["pid"]), "-o", "lstart=,command="],
                                   capture_output=True, text=True).stdout.split(None, 5)
            if len(again) == 6 and " ".join(again[:5]) == process["start"] \
                    and (again[5].rstrip("\n") + " ") == process["command"]:
                try:
                    os.kill(process["pid"], signal.SIGTERM)
                except ProcessLookupError:
                    continue
                if process not in stopped:
                    stopped.append(process)
        if found:
            last_seen = time.monotonic()
        if time.monotonic() - last_seen >= quiet or time.monotonic() > deadline:
            return stopped
        time.sleep(0.5)


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
    env = {"HOME": str(home), "PATH": "/usr/bin:/bin", "USER": pwd.getpwuid(os.getuid()).pw_name,
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

    def open_layer(command):
        return api("session.new_layer", {"session_id": session, "window_id": window,
            "bounds": [0.2, 0.2, 0.6, 0.6], "layout": {"block": {
                "flavor": "com.superlogical.terminal.shell", "options": {
                    "shell": "none", "login": False, "command": command}}}})["block_ids"][0]

    def painted(block, needles, description):
        """The screen's own text, which no refusal prints."""
        text = wait_for(lambda: capture(block), lambda text: all(n in text for n in needles)
                        or "Press any key to close" in text, description)
        if "Press any key to close" in text or "Namedrop: " in text:
            raise AssertionError(description + ", refused instead: " + repr(text))
        return text

    def refused_until_key(block, refusal, description):
        text = wait_for(lambda: capture(block), lambda text: "Press any key to close" in text, description)
        assert refusal in text, (description, text)
        time.sleep(1)
        assert block in blocks(), description + ": closed before a key"
        send_key(block, "x")
        wait_for(blocks, lambda ids: block not in ids, description + ": did not close on one key")

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
        text = painted(layer, ["Name this pane", "Mention name: @"], "activation did not continue naming")
        assert "Press Enter to activate it" not in text, "the offer stayed above naming: " + repr(text)
        # Rust tests pin that this activation opens no tour; this session has no agent for one.
        assert blocks() == {pane, layer}, "activation opened another layer"
        rex("send", "-s", session, "-b", layer, "u5-terminal")
        send_key(layer, "enter")
        send_key(layer, "enter")
        wait_for(blocks, lambda ids: layer not in ids, "successful naming layer stayed open")
        addresses = json.loads(nd("address", "list").stdout)["data"]
        assert any(a["alias"] == "u5-terminal" and a["pane_id"] == pane for a in addresses), addresses
        evidence["checks"].append("inactive offer: Esc cancels; Enter activates, clears the offer, opens no other layer, names the original block, layer closes")
        screens = [
            ("name", ["Name this pane", "Mention name: @"]),
            ("manage", ["Manage names", "@u5-terminal", "Esc Close"]),
            ("doctor", ["agents are available", "R Refresh", "Enter or Esc closes"]),
            ("tour", ["Welcome to Namedrop", "Page 1/", "Enter or Esc closes"]),
            ("status", ["@u5-terminal", "Facts ", "Elsewhere in this workspace", "Esc Close"]),
        ]
        for mode, needles in screens:
            layer = launch(mode)
            text = painted(layer, needles, mode + " did not paint")
            (args.out / (mode + ".txt")).write_text(text)
            if mode == "status":
                # The pane and this layer are the only blocks, and the layer is not a pane to describe.
                elsewhere = text.split("Elsewhere in this workspace", 1)[1].strip().splitlines()[0]
                assert elsewhere.startswith("No other agents"), "status listed its own layer: " + repr(text)
            send_key(layer, "escape")
            wait_for(blocks, lambda ids: layer not in ids, mode + " layer did not close")
            evidence["checks"].append(mode + " opens, paints its own screen and closes")
        before = blocks()
        rex("send", "-s", session, "-b", pane, str(launcher) + " ui launch doctor; echo U5-RC=$?")
        send_key(pane, "enter")
        typed = wait_for(lambda: capture(pane), lambda text: re.search(r"^U5-RC=\d+$", text, re.M),
                         "typed launch did not finish")
        assert re.search(r"^U5-RC=0$", typed, re.M), typed
        opened = blocks() - before
        assert len(opened) == 1, opened
        layer = opened.pop()
        painted(layer, ["agents are available"], "typed launch did not paint Doctor")
        send_key(layer, "escape")
        wait_for(blocks, lambda ids: layer not in ids, "typed launch layer did not close")
        evidence["checks"].append("ui launch typed in the target pane opens its layer")
        detached = api("session.new_block", {"session_id": session, "flavor": "com.superlogical.terminal.shell",
                                             "options": {"shell": "none", "command": ["/bin/sh", "-i"]}})["block_id"]
        before = blocks()
        result = subprocess.run([str(launcher), "--endpoint", endpoint, "--space", session, "--pane", detached,
                                 "ui", "launch", "name"], env=env, text=True, capture_output=True, timeout=20)
        assert result.returncode and "This block is not in a window" in result.stdout + result.stderr, result
        assert blocks() == before, "a detached target opened a layer"
        rex("block", "close", "-s", session, detached)
        wait_for(blocks, lambda ids: detached not in ids, "detached block did not close")
        evidence["checks"].append("a detached target is refused before any layer")
        migration = runtime / "migration.json"
        migration.write_text("{}")
        try:
            layer = open_layer(["/usr/bin/env", "NAMEDROP_TARGET_WORKSPACE=" + session,
                                       "NAMEDROP_TARGET_PANE=" + pane, "NAMEDROP_TARGET_TERMINAL=" + pane,
                                       "NAMEDROP_TARGET_SESSION=", "NAMEDROP_STATE_DIR=" + str(state),
                                       str(launcher), "--endpoint", endpoint, "ui", "run", "doctor"])
            refused_until_key(layer, "Namedrop activation was interrupted", "gate refusal")
        finally:
            migration.unlink()
        evidence["checks"].append("a command gate refusal waits for one key")
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
            refused_until_key(open_layer([str(failer)]), "This popup is missing its invoking space",
                              "refusal after " + str(delay) + "s")
            evidence["checks"].append("refusal after " + str(delay) + "s waits for one key")
        missing = root / "removed-launcher"
        missing.write_text("#!/bin/sh\nexit 0\n")
        missing.chmod(0o700)
        assert missing.is_file() and os.access(missing, os.X_OK)
        missing.unlink()
        layer = open_layer(["/usr/bin/env", str(missing)])
        facts = wait_for(lambda: api("com.superlogical.terminal.process", {
            "session_id": session, "block_id": layer, "args": {}}),
            lambda facts: facts.get("last_exit") is not None, "env did not report missing launcher")
        assert facts["last_exit"]["exit_code"] == 127, facts
        evidence["checks"].append("launcher absent at spawn leaves env layer with exit 127")
        evidence["passed"] = True
    finally:
        recorded = root / "helper-pids"
        evidence["helper_pids"] = sorted({int(pid) for pid in recorded.read_text().split()}) if recorded.exists() else []
        # The helpers first, so no bridge is left to start a worker once the
        # server stops, then the server, then whatever appears late.
        stopped = stop_probe_processes(root)
        if server.poll() is None:
            server.terminate()
            server.wait(timeout=10)
        server_log.close()
        stopped += [p for p in stop_probe_processes(root, quiet=5) if p not in stopped]
        evidence["stopped"] = stopped
        unexpected = [p for p in stopped if not any(word in p["command"] for word in HELPERS)]
        left = probe_processes(root)
        evidence["stray"] = unexpected + left
        if evidence["stray"]:
            evidence["passed"] = False
        if not left:
            shutil.rmtree(root)
        (args.out / "report.json").write_text(json.dumps(evidence, indent=2) + "\n")
    print(json.dumps(evidence, indent=2))
    if not evidence.get("passed"):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
