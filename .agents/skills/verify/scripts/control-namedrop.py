#!/usr/bin/env python3
"""Drive public Namedrop commands in disposable native Herdr terminal panes."""
import argparse
import hashlib
import http.client
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.parse


REPO = Path(__file__).resolve().parents[4]

# Herdr's Codex detector reads the pane title and body.
RENDERER = '''import pathlib, sys, time
surface, title = pathlib.Path(sys.argv[1]), sys.argv[2]
previous = None
while True:
    state = surface.read_text()
    if state != previous:
        prefix = '\\u280b ' if state == 'working' else ''
        body = '\\u2022 Working (esc to interrupt)' if state == 'working' else '\\u203a '
        sys.stdout.write('\\x1b]0;' + prefix + title + '\\x07\\x1b[2J\\x1b[H' + body + '\\n')
        sys.stdout.flush()
        previous = state
    time.sleep(.05)
'''


def wait(predicate, label, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.1)
    raise RuntimeError(f"Timed out waiting for {label}")


def now_ms():
    return time.time_ns() // 1_000_000


def compile_simulator(path):
    subprocess.run(["rustc", "--edition", "2021", "-o", str(path), "-"], check=True, capture_output=True, text=True,
                   input="fn main() { std::io::copy(&mut std::io::stdin().lock(), &mut std::io::stdout().lock()).unwrap(); }")


def read_envelopes(path):
    values = []
    if path.exists():
        for line in path.read_text(errors="replace").splitlines():
            try:
                value = json.loads(line.replace("\x1b[200~", "").replace("\x1b[201~", "").strip())
            except ValueError:
                continue
            if isinstance(value, dict) and value.get("control", {}).get("attempt_id"):
                values.append(value)
    return values


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--feature", choices=["names", "groups", "controls", "tour", "dashboard", "handoffs",
                                              "shared-checkout", "stuck-wake", "idle-worker", "stale-runtime",
                                              "cross-workspace"],
                        default="names")
    parser.add_argument("--binary", type=Path, default=REPO / "target/debug/namedrop")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    herdr = shutil.which("herdr")
    if not herdr:
        raise RuntimeError("Native Herdr must be on PATH")
    base = Path(tempfile.mkdtemp(prefix="nd-verify-", dir=os.path.realpath("/tmp")))
    evidence = REPO / ".scratch/verify" / base.name
    evidence.mkdir(parents=True)
    for name in ("home", "config", "state", "data", "run", "workspace", "io"):
        (base / name).mkdir()
    env = {key: os.environ[key] for key in ("PATH", "LANG", "LC_ALL", "TERM") if key in os.environ}
    env.update(HOME=str(base / "home"), XDG_CONFIG_HOME=str(base / "config"),
               XDG_STATE_HOME=str(base / "state"), XDG_DATA_HOME=str(base / "data"),
               XDG_RUNTIME_DIR=str(base / "run"), CODEX_HOME=str(base / "home/.codex"),
               CLAUDE_CONFIG_DIR=str(base / "home/.claude"), GIT_CONFIG_NOSYSTEM="1",
               GIT_CONFIG_GLOBAL=str(base / "home/.gitconfig"),
               HERDR_SOCKET_PATH=str(base / "run/herdr.sock"), SHELL=shutil.which("bash") or "/bin/bash")
    (base / "home/.bashrc").write_text('PROMPT_COMMAND=\': > "$HOME/ready-$HERDR_PANE_ID"\'\n')
    (base / "home/.bash_profile").write_text('. "$HOME/.bashrc"\n')
    links_plugin = args.feature in ("stuck-wake", "idle-worker", "stale-runtime", "cross-workspace")
    state = base / ("state/herdr/plugins/namedrop" if links_plugin else "state/namedrop")
    server = None
    dashboard_pid = None
    shell_pids = []
    success = False
    sequence = 0
    summary = {"feature": args.feature, "fixture": str(base), "binary": str(binary),
               "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
               "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip(),
               "limits": ("Hook-driven reconciliation with simulated Codex sessions; no model, no real agent hooks."
                          if links_plugin else
                          "No model sessions; proves CLI and native terminal behavior only.")}

    def save(name, value):
        (evidence / name).write_text(json.dumps(value, indent=2) + "\n")

    def record(row):
        with (evidence / "commands.jsonl").open("a") as log:
            log.write(json.dumps(row) + "\n")

    def command(*argv, check=True, context=None, timeout=30, kind="host", cwd=None, stdin=None):
        result = subprocess.run([str(a) for a in argv], env=dict(env, **(context or {})), cwd=cwd or base / "workspace",
                                capture_output=True, text=True, timeout=timeout, input=stdin)
        record({"kind": kind, "argv": [str(a) for a in argv], **({"stdin": stdin} if stdin is not None else {}),
                "exit": result.returncode, "stdout": result.stdout, "stderr": result.stderr})
        if check and result.returncode:
            raise RuntimeError(result.stderr or result.stdout)
        return result

    def host(*argv, context=None):
        result = command(herdr, *argv, context=context)
        return json.loads(result.stdout)["result"] if result.stdout.strip() else None

    def cli(*argv, pane=None, workspace=None, allowed=(0,)):
        nonlocal sequence
        sequence += 1
        pane = pane or panes["lead"]
        workspace = workspace or spaces["lead"]
        prefix = base / "io" / str(sequence)
        out, err, code = [Path(str(prefix) + suffix) for suffix in (".out", ".err", ".exit")]
        invocation = [str(binary), "--plugin-state", str(state), "--herdr-socket", env["HERDR_SOCKET_PATH"],
                      "--herdr-workspace", workspace, "--herdr-pane", pane, *map(str, argv)]
        script = prefix.with_suffix(".sh")
        script.write_text(shlex.join(invocation) + " > " + shlex.quote(str(out)) + " 2> " + shlex.quote(str(err))
                          + "\nresult=$?\ncat " + shlex.quote(str(out)) + "\ncat " + shlex.quote(str(err))
                          + " >&2\nprintf '%s\\n' \"$result\" > " + shlex.quote(str(code)) + "\n")
        ready = base / "home" / f"ready-{pane}"
        ready.unlink(missing_ok=True)
        host("pane", "run", pane, shlex.join(["sh", str(script)]))
        wait(lambda: code.exists() and ready.exists(), "CLI exit and idle shell")
        row = {"kind": "cli", "feature": args.feature, "pane": pane, "argv": invocation,
               "exit": int(code.read_text()), "stdout": out.read_text(), "stderr": err.read_text()}
        record(row)
        transcript = command(herdr, "pane", "read", pane, "--source", "recent-unwrapped", "--lines", "120").stdout
        (evidence / f"{sequence:02d}-terminal.txt").write_text(transcript)
        if row["exit"] not in allowed:
            raise RuntimeError(f"CLI failed: {row}")
        return json.loads(row["stdout"]) if row["stdout"].strip().startswith("{") else row["stdout"]

    def body(*argv, pane=None, workspace=None, **payload):
        return cli(*argv, "--input-json", json.dumps(payload), pane=pane, workspace=workspace)["data"]

    def database_snapshot():
        with sqlite3.connect(state.joinpath("namedrop.sqlite3").as_uri() + "?mode=ro", uri=True) as db:
            return "\n".join(db.iterdump())

    def rows(sql, parameters=()):
        with sqlite3.connect(state.joinpath("namedrop.sqlite3").as_uri() + "?mode=ro", uri=True) as db:
            return db.execute(sql, parameters).fetchall()

    def backup_database():
        with sqlite3.connect(state.joinpath("namedrop.sqlite3").as_uri() + "?mode=ro", uri=True) as source:
            with sqlite3.connect(evidence / "state.sqlite3") as destination:
                source.backup(destination)


    def dashboard_alive():
        try:
            arguments = Path(f"/proc/{dashboard_pid}/cmdline").read_bytes().split(b"\0")
        except FileNotFoundError:
            return False
        return b"dashboard-server" in arguments and str(state).encode() in arguments

    if args.feature == "dashboard":
        # Panes inherit this, so the server the CLI starts there idles out within the run.
        env["NAMEDROP_DASHBOARD_IDLE_MS"] = "10000"
    log = (evidence / "server.log").open("w")
    try:
        command("git", "init", "--quiet")
        summary["herdr_version"] = command(herdr, "--version").stdout.strip()
        summary["namedrop_version"] = command(binary, "--version").stdout.strip()
        server = subprocess.Popen([herdr, "server"], env=env, cwd=base / "workspace",
                                  stdout=log, stderr=log, start_new_session=True)
        wait(lambda: server.poll() is not None or Path(env["HERDR_SOCKET_PATH"]).exists(), "owned server socket")
        if server.poll() is not None:
            raise RuntimeError("Disposable server failed; see server.log")
        created = host("workspace", "create", "--cwd", str(base / "workspace"), "--label",
                       "alpha" if args.feature == "cross-workspace" else "Namedrop verification")
        spaces = {"lead": created["workspace"]["workspace_id"]}
        panes = {"lead": created["root_pane"]["pane_id"]}
        panes["worker"] = host("tab", "create", "--workspace", spaces["lead"], "--cwd", str(base / "workspace"),
                               "--label", "Verification worker")["root_pane"]["pane_id"]
        for pane in panes.values():
            wait(lambda: (base / "home" / f"ready-{pane}").exists(), "native bash prompt")
            shell_pids.append(host("pane", "process-info", "--pane", pane)["process_info"]["shell_pid"])
        save("instance.json", {"server_pid": server.pid, "socket": env["HERDR_SOCKET_PATH"],
                              "state": str(state), "workspace": spaces["lead"], "panes": panes})
        if links_plugin:
            root = base / "plugin"
            for name in ("scripts", "bin"):
                (root / name).mkdir(parents=True)
            for name in ("setup.sh", "run.sh", "runtime.py", "update.py"):
                shutil.copyfile(REPO / "scripts" / name, root / "scripts" / name)
            for name in ("herdr-plugin.toml", "runtime.json"):
                shutil.copyfile(REPO / name, root / name)
            shutil.copyfile(binary, root / "bin/namedrop")
            (root / "bin/namedrop").chmod(0o700)
            context = dict(HERDR_ENV="1", HERDR_WORKSPACE_ID=spaces["lead"], HERDR_PANE_ID=panes["lead"],
                           HERDR_PLUGIN_STATE_DIR=str(state))
            host("plugin", "link", str(root), context=context)
            activated = json.loads(command("sh", root / "scripts/setup.sh", "activate", "--json", context=context,
                                           timeout=120).stdout)["data"]
            launcher = state / "runtime/namedrop"
            assert launcher.is_file(), f"missing stable launcher {launcher}"
            assert hashlib.sha256((state / "runtime/current/namedrop").read_bytes()).hexdigest() == summary["sha256"], \
                "installed runtime is not the candidate binary"
        else:
            activated = body("activate", confirmed=True)

        # Doctor reads state; the unnamed fixture may legitimately need attention.
        before = database_snapshot()
        live = host("pane", "list")["panes"]
        assert server.poll() is None and all(any(p["pane_id"] == pane for p in live) for pane in panes.values())
        report = cli("doctor", "--json", allowed=(1,))
        assert report["data"]["status"] == "needs_attention", report
        assert [issue["code"] for issue in report["data"]["issues"]] == ["ND-SETUP-003"], report
        assert before == database_snapshot(), "Doctor changed stored state"
        save("doctor.json", report)

        if args.feature == "names":
            cli("name", "@build", "--type", "terminal")
            listed = cli("address", "list", "--json")["data"]
            saved = next(a for a in listed if a["alias"] == "build")
            assert saved["pane_type"] == "terminal", saved
            body("address", "rename", address_id=saved["id"], alias="verify-build")
            listed = cli("address", "list", "--json")["data"]
            assert any(a["alias"] == "verify-build" and a["id"] == saved["id"] for a in listed), listed
            assert not any(a["alias"] == "build" for a in listed), listed
            save("names.json", listed)
            cli("name", "@reviewer", "--type", "agent", "--persona", "Reviews Rust changes.", pane=panes["worker"])
            cli("name", "@reviewer", "--type", "agent", pane=panes["worker"])
            roster = cli("address", "list", "--json")["data"]
            assert next(a for a in roster if a["alias"] == "reviewer")["persona"] == "Reviews Rust changes."
            save("persona-saved.json", roster)
            cli("name", "@reviewer", "--type", "agent", "--generic", pane=panes["worker"])
            roster = cli("address", "list", "--json")["data"]
            assert next(a for a in roster if a["alias"] == "reviewer")["persona"] is None
            save("persona-cleared.json", roster)
            refused = cli("name", "@verify-build", "--type", "terminal", "--persona", "Probe.", allowed=range(1, 256))
            assert refused["error"]["message"] == "a named terminal cannot hold a persona", refused
            roster = cli("address", "list", "--json")["data"]
            assert next(a for a in roster if a["alias"] == "verify-build")["persona"] is None, roster
        elif args.feature == "groups":
            cli("name", "@reviewer", "--type", "agent", "--persona", "Reviews Rust changes.")
            cli("name", "@tester", "--type", "agent", "--generic", pane=panes["worker"])
            body("group", "set", alias="review-team", members=["reviewer", "tester"])
            listed = cli("group", "list", "--json")["data"]["groups"]
            assert len(listed) == 1 and listed[0]["alias"] == "review-team", listed
            assert {m["alias"] for m in listed[0]["members"]} == {"reviewer", "tester"}, listed
            save("groups.json", listed)
            assert body("group", "remove", alias="review-team")["removed"] is True
            assert cli("group", "list", "--json")["data"]["groups"] == []
            kept = {a["alias"] for a in cli("address", "list", "--json")["data"]}
            assert {"reviewer", "tester"} <= kept, kept
            empty = cli("group", "set", "--input-json", json.dumps({"alias": "nobody", "members": []}), allowed=range(1, 256))
            assert empty["status"] == "rejected", empty
            cli("name", "@build", "--type", "terminal", pane=panes["worker"])
            probe = cli("group", "set", "--input-json", json.dumps({"alias": "ops", "members": ["build"]}), allowed=range(256))
            body("group", "set", alias="solo", members=["reviewer"])
            before = database_snapshot()
            only_author = cli("send", "--input-json", json.dumps({"request_id": "solo-self", "source": "@solo Note to self."}),
                              allowed=range(1, 256))
            assert "addressed only to its author" in only_author["error"]["message"], only_author
            assert before == database_snapshot(), "A send addressed only to its author changed stored state"
            save("group-probes.json", {"empty_members": empty, "terminal_member": probe, "only_author": only_author})
        elif args.feature == "controls":
            body("space", "pause")
            with sqlite3.connect(state.joinpath("namedrop.sqlite3").as_uri() + "?mode=ro", uri=True) as db:
                paused = db.execute("SELECT paused FROM worktrees").fetchall()
            assert paused == [(1,)], paused
            body("space", "resume")
            with sqlite3.connect(state.joinpath("namedrop.sqlite3").as_uri() + "?mode=ro", uri=True) as db:
                resumed = db.execute("SELECT paused FROM worktrees").fetchall()
            assert resumed == [(0,)], resumed
            save("controls.json", {"paused": paused[0][0], "resumed": resumed[0][0]})
        elif args.feature == "tour":
            origin = next(p for p in live if p["pane_id"] == panes["lead"])
            invocation = ["env", f"NAMEDROP_TARGET_WORKSPACE={spaces['lead']}", f"NAMEDROP_TARGET_PANE={panes['lead']}",
                          f"NAMEDROP_TARGET_TERMINAL={origin['terminal_id']}", str(binary), "--plugin-state", str(state),
                          "--herdr-socket", env["HERDR_SOCKET_PATH"], "--herdr-pane", panes["lead"], "ui", "run", "tour"]
            ready = base / "home" / f"ready-{panes['lead']}"
            ready.unlink(missing_ok=True)
            host("pane", "run", panes["lead"], shlex.join(invocation))

            def page(*texts):
                # The footer can draw after the title, so wait for every expected line.
                result = command(herdr, "pane", "read", panes["lead"], "--source", "visible").stdout
                return result if all(text in result for text in texts) else None

            (evidence / "tour-first.txt").write_text(wait(lambda: page("Welcome to Namedrop", "Page 1/4"), "first tour page"))
            host("pane", "send-keys", panes["lead"], "right")
            (evidence / "tour-second.txt").write_text(wait(lambda: page("Mention an agent", "Page 2/4"), "second tour page"))
            host("pane", "send-keys", panes["lead"], "esc")
            wait(ready.exists, "shell prompt after closing tour")
            assert cli("address", "list", "--json")["data"] == []
            first_page, second_page = (evidence / "tour-first.txt").read_text(), (evidence / "tour-second.txt").read_text()
            assert "Page 1/4" in first_page and "Name this pane" in first_page, first_page
            assert "Page 2/4" in second_page and "Name this pane" not in second_page, second_page
            marker = state / "tours-shown/welcome-1"
            assert marker.exists(), "Opening the tour did not record it as shown"
            cleared = body("ui", "reset-tour")
            assert cleared == {"presentation_id": "welcome-1", "cleared": True} and not marker.exists(), cleared
            assert body("ui", "reset-tour")["cleared"] is False
            save("tour.json", {"marker_written": True, "reset": cleared})
        elif args.feature == "handoffs":
            before = database_snapshot()
            # Run outside the pane's process tree with only the inherited pane variable, as a stale caller would.
            inherited = command("env", f"HERDR_PANE_ID={panes['lead']}", f"HERDR_WORKSPACE_ID={spaces['lead']}", binary,
                                "--plugin-state", state, "handoff", "status", "--input-json", "{}", check=False)
            assert inherited.returncode == 65 and "Pass --herdr-pane" in inherited.stdout + inherited.stderr, inherited
            assert before == database_snapshot(), "A rejected status read changed stored state"
            cli("name", "@lead", "--type", "agent")
            cli("name", "@build", "--type", "terminal", pane=panes["worker"])
            before = database_snapshot()
            anonymous = cli("handoff", "preview", "--input-json", json.dumps({"source": "@build Check this."}),
                            allowed=(65,))
            assert "Agent Session" in anonymous["error"]["message"], anonymous
            assert before == database_snapshot(), "A rejected handoff changed stored state"
            self_send = cli("send", "--input-json", json.dumps({"request_id": "self-only", "source": "@lead Note to self."}),
                            allowed=range(1, 256))
            assert "addressed only to its author" in self_send["error"]["message"], self_send
            assert before == database_snapshot(), "A send addressed only to its author changed stored state"
            save("handoffs.json", {"inherited_exit": inherited.returncode, "preview": anonymous, "self_send": self_send})
        elif args.feature == "dashboard":
            cli("name", "@build", "--type", "terminal")
            first = cli("dashboard", "--no-open")["data"]
            info = json.loads((state / "dashboard/server.json").read_text())
            dashboard_pid = info["pid"]
            assert first["server"] == "started" and first["opened"] is False, first
            assert first["workspace_id"] == spaces["lead"], first
            url = urllib.parse.urlsplit(first["url"])
            assert (url.hostname, url.port) == ("127.0.0.1", info["port"]), first

            def get(path, cookie=None):
                connection = http.client.HTTPConnection("127.0.0.1", info["port"], timeout=10)
                connection.request("GET", path, headers={"Cookie": cookie} if cookie else {})
                response = connection.getresponse()
                record({"kind": "http", "path": path.split("?")[0], "status": response.status})
                return connection, response

            def snapshot(path, cookie, until=lambda frame: True):
                # A new stream starts with the cached frame; later frames follow each committed change.
                connection, response = get(path + "/events", cookie)
                assert response.status == 200, response.status
                event, frames = None, []
                try:
                    while True:
                        line = response.readline().decode().rstrip("\n")
                        assert line or not response.closed, "event stream ended"
                        if line.startswith("event: "):
                            event = line[len("event: "):]
                        elif line.startswith("data: ") and event == "snapshot":
                            frames.append(json.loads(line[len("data: "):]))
                            if until(frames[-1]):
                                return frames[-1]
                except TimeoutError:
                    raise AssertionError(f"no matching snapshot on {path}: {frames[-1:]}")
                finally:
                    connection.close()

            entry = f"{url.path}?{url.query}"
            _, redeemed = get(entry)
            assert redeemed.status == 303 and redeemed.getheader("Location") == url.path, redeemed.status
            cookie = redeemed.getheader("Set-Cookie").split(";")[0]
            assert get(entry)[1].status == 401, "A link opened the dashboard twice"
            assert get(url.path)[1].status == 401, "The dashboard opened without a session"
            page = get(url.path, cookie)[1]
            assert page.status == 200 and page.getheader("Content-Type").startswith("text/html"), page.status
            own = snapshot(url.path, cookie)
            assert own["space"]["workspace_id"] == spaces["lead"] and own["space"]["state"] == "active", own["space"]
            assert [p["alias"] for p in own["participants"] if p["kind"]["type"] == "terminal"] == ["@build"], own
            save("dashboard-snapshot.json", own)
            again = cli("dashboard", "--no-open")["data"]
            assert again["server"] == "running" and again["space_key"] == first["space_key"], again
            assert urllib.parse.urlsplit(again["url"]).port == info["port"], again
            command("git", "init", "--quiet", str(base / "workspace-b"))
            other = host("workspace", "create", "--cwd", str(base / "workspace-b"), "--label", "Namedrop verification B")
            panes["other"] = other["root_pane"]["pane_id"]
            wait(lambda: (base / "home" / f"ready-{panes['other']}").exists(), "second workspace prompt")
            shell_pids.append(host("pane", "process-info", "--pane", panes["other"])["process_info"]["shell_pid"])
            second = cli("dashboard", "--no-open", pane=panes["other"], workspace=other["workspace"]["workspace_id"])["data"]
            assert second["server"] == "running" and second["space_key"] != first["space_key"], second
            assert urllib.parse.urlsplit(second["url"]).port == info["port"], second
            other_page = urllib.parse.urlsplit(second["url"]).path
            unactivated = snapshot(other_page, cookie)
            assert unactivated["space"]["state"] == "unknown", unactivated["space"]
            assert unactivated["space"]["label"]["text"] == "Namedrop verification B", unactivated["space"]
            assert unactivated["participants"] == [], unactivated
            cli("activate", "--input-json", json.dumps({"confirmed": True}),
                pane=panes["other"], workspace=other["workspace"]["workspace_id"])
            neighbour = snapshot(other_page, cookie, until=lambda frame: frame["space"]["state"] != "unknown")
            assert neighbour["space"]["workspace_id"] == other["workspace"]["workspace_id"], neighbour["space"]
            assert neighbour["space"]["state"] == "active", neighbour["space"]
            assert all(p["alias"] != "@build" for p in neighbour["participants"]), neighbour
            connection, listing = get("/api/spaces", cookie)
            listed = json.loads(listing.read())
            connection.close()
            assert {first["space_key"], second["space_key"]} <= {space["key"] for space in listed}, listed
            assert (state / "dashboard/session").exists()
            # The server sees closed streams at its next keepalive, then waits the idle period.
            wait(lambda: not dashboard_alive(), "dashboard idle exit", timeout=60)
            assert "dashboard: stopping: idle" in (state / "dashboard/server.log").read_text()
            assert not (state / "dashboard/session").exists(), "Idle exit kept the browser session"
            save("dashboard.json", {"first": first, "again": again, "second": second, "server": info,
                                    "other_unactivated": unactivated["space"], "other_active": neighbour["space"],
                                    "spaces": listed, "idle_exit": True, "session_rotated": True})
        elif args.feature == "shared-checkout":
            twin = host("workspace", "create", "--cwd", str(base / "workspace"), "--label", "Namedrop verification twin")
            spaces["twin"] = twin["workspace"]["workspace_id"]
            panes["twin"] = twin["root_pane"]["pane_id"]
            wait(lambda: (base / "home" / f"ready-{panes['twin']}").exists(), "twin native bash prompt")
            shell_pids.append(host("pane", "process-info", "--pane", panes["twin"])["process_info"]["shell_pid"])
            listing = host("worktree", "list", "--workspace", spaces["twin"], "--json")
            matches = [w for w in listing["worktrees"] if w["path"] == str(base / "workspace")]
            assert len(matches) == 1, listing
            checkout = matches[0]
            assert checkout["open_workspace_id"] == spaces["lead"], listing
            assert listing["source"]["source_workspace_id"] == spaces["twin"], listing
            twin_report = cli("doctor", "--json", pane=panes["twin"], workspace=spaces["twin"], allowed=(0, 1))
            assert twin_report["data"]["status"] in ("healthy", "needs_attention"), twin_report
            assert not any(issue["code"] == "ND-INSPECT-001" for issue in twin_report["data"]["issues"]), twin_report
            twin_activated = body("activate", pane=panes["twin"], workspace=spaces["twin"], confirmed=True)
            assert twin_activated["space_id"] == spaces["twin"], twin_activated
            assert twin_activated["worktree_id"] == str(base / "workspace") == activated["worktree_id"], \
                (activated, twin_activated)
            assert activated["space_id"] == spaces["lead"], activated
            cli("name", "@twin-reviewer", "--type", "terminal", pane=panes["twin"], workspace=spaces["twin"])
            cli("name", "@verify-build", "--type", "terminal")
            rosters = {space: cli("address", "list", "--json", pane=panes[space], workspace=spaces[space])["data"]
                       for space in ("lead", "twin")}
            assert {a["alias"] for a in rosters["lead"]} == {"verify-build"}, rosters
            assert {a["alias"] for a in rosters["twin"]} == {"twin-reviewer"}, rosters
            reports = {space: cli("doctor", "--json", pane=panes[space], workspace=spaces[space], allowed=(0, 1))
                       for space in ("lead", "twin")}
            assert not any(issue["code"] == "ND-INSPECT-001" for report in reports.values()
                           for issue in report["data"]["issues"]), reports
            workspaces = host("workspace", "list")["workspaces"]
            by_path = host("worktree", "list", "--cwd", str(base / "workspace"), "--json")
            unscoped = {}
            for case, scope in (("pane", ["--herdr-pane", panes["twin"]]), ("cwd", [])):
                result = command(binary, "--plugin-state", state, "--herdr-socket", env["HERDR_SOCKET_PATH"], *scope,
                                 "address", "list", "--json", check=False)
                unscoped[case] = {"exit": result.returncode, "stdout": result.stdout, "stderr": result.stderr}
            save("shared-checkout.json", {"worktrees": listing, "activate": {"lead": activated, "twin": twin_activated},
                                          "rosters": rosters, "doctor": reports, "workspaces": workspaces,
                                          "worktrees_by_path": by_path, "unscoped": unscoped})
            by_pane = unscoped["pane"]
            aliases = {a["alias"] for a in json.loads(by_pane["stdout"])["data"]} if by_pane["exit"] == 0 else None
            assert aliases == {"twin-reviewer"}, (f"--herdr-pane {panes['twin']} without a workspace id read roster "
                                                  f"{aliases}, not twin workspace {spaces['twin']}'s {{'twin-reviewer'}}: "
                                                  f"exit {by_pane['exit']} {by_pane['stderr'].strip()}")
            by_cwd = unscoped["cwd"]
            refusal = by_cwd["stdout"] + by_cwd["stderr"]
            assert by_cwd["exit"] == 65 and all(token in refusal for token in (spaces["lead"], spaces["twin"],
                                                                                "--herdr-workspace")), \
                (f"a caller with only the shared checkout cwd got exit {by_cwd['exit']} instead of a refusal naming "
                 f"{spaces['lead']}, {spaces['twin']} and --herdr-workspace: {refusal.strip()}")
            linked = base / "linked"
            command("git", "-c", "user.name=verify", "-c", "user.email=verify@example.invalid", "commit", "--quiet",
                    "--allow-empty", "-m", "fixture")
            command("git", "worktree", "add", "--quiet", "-b", "verify-linked", linked)
            opened = host("worktree", "open", "--workspace", spaces["lead"], "--path", linked, "--label",
                          "Namedrop verification linked")
            second = host("workspace", "create", "--cwd", linked, "--label", "Namedrop verification linked twin")
            for role, created in (("linked", opened), ("linked-twin", second)):
                spaces[role] = created["workspace"]["workspace_id"]
                panes[role] = created["root_pane"]["pane_id"]
                wait(lambda: (base / "home" / f"ready-{panes[role]}").exists(), f"{role} native bash prompt")
                shell_pids.append(host("pane", "process-info", "--pane", panes[role])["process_info"]["shell_pid"])
            linked_listing = host("worktree", "list", "--workspace", spaces["linked-twin"], "--json")
            linked_workspaces = host("workspace", "list")["workspaces"]
            body("activate", pane=panes["linked"], workspace=spaces["linked"], confirmed=True)
            cli("name", "@linked-build", "--type", "terminal", pane=panes["linked"], workspace=spaces["linked"])
            linked_roster = cli("address", "list", "--json", pane=panes["linked"], workspace=spaces["linked"])["data"]
            assert {a["alias"] for a in linked_roster} == {"linked-build"}, linked_roster
            twin_refused = cli("activate", "--input-json", json.dumps({"confirmed": True}), pane=panes["linked-twin"],
                               workspace=spaces["linked-twin"], allowed=(65,))
            linked_unscoped = {}
            for case, scope, cwd in (("pane", ["--herdr-pane", panes["linked-twin"]], linked), ("cwd", [], linked),
                                     ("root", [], base / "workspace")):
                result = command(binary, "--plugin-state", state, "--herdr-socket", env["HERDR_SOCKET_PATH"], *scope,
                                 "address", "list", "--json", check=False, cwd=cwd)
                linked_unscoped[case] = {"exit": result.returncode, "stdout": result.stdout, "stderr": result.stderr}
            save("shared-linked-checkout.json", {"opened": opened, "worktrees": linked_listing,
                                                 "workspaces": linked_workspaces, "roster": linked_roster,
                                                 "twin_activate": twin_refused, "unscoped": linked_unscoped})
            assert f"does not contain space {spaces['linked-twin']}" in twin_refused["error"]["message"], twin_refused
            by_pane = linked_unscoped["pane"]
            assert by_pane["exit"] == 65 and "linked-build" not in by_pane["stdout"], \
                (f"--herdr-pane {panes['linked-twin']} from the linked checkout got exit {by_pane['exit']} instead of "
                 f"failing closed: {by_pane['stdout'].strip()} {by_pane['stderr'].strip()}")
            by_cwd = linked_unscoped["cwd"]
            refusal = by_cwd["stdout"] + by_cwd["stderr"]
            assert by_cwd["exit"] == 65 and all(token in refusal for token in (spaces["linked"], spaces["linked-twin"],
                                                                                "--herdr-workspace")), \
                (f"a caller with only the linked checkout cwd got exit {by_cwd['exit']} instead of a refusal naming "
                 f"{spaces['linked']}, {spaces['linked-twin']} and --herdr-workspace: {refusal.strip()}")
            by_root = linked_unscoped["root"]
            refusal = json.loads(by_root["stdout"])["error"]["message"] if by_root["exit"] == 65 else by_root["stdout"]
            assert by_root["exit"] == 65 and spaces["lead"] in refusal and spaces["twin"] in refusal and not any(
                f"{spaces[role]}," in refusal or f"{spaces[role]})" in refusal for role in ("linked", "linked-twin")), \
                (f"a caller with only the root checkout cwd got exit {by_root['exit']} instead of a refusal naming "
                 f"exactly {spaces['lead']} and {spaces['twin']}: {refusal.strip()}")
        elif args.feature == "stale-runtime":
            # A bare `herdr plugin install` rebuilds <root>/bin/namedrop and never
            # activates. A trailing newline changes the digest and leaves the ELF
            # program headers intact, so the staged file still runs.
            built = root / "bin/namedrop"
            staged = root / "bin/.namedrop.reinstalled"
            staged.write_bytes(binary.read_bytes() + b"\n")
            staged.chmod(0o700)
            staged.replace(built)
            reinstalled = hashlib.sha256(built.read_bytes()).hexdigest()

            def active_revision():
                return os.readlink(state / "runtime/current").rsplit("/", 1)[-1]

            def require(condition, requirement, observed):
                if not condition:
                    summary["outcome"] = requirement
                    raise RuntimeError(f"stale runtime: {requirement}; observed {observed}")

            trace = {"candidate": summary["sha256"], "reinstalled": reinstalled, "active_before_doctor": active_revision()}
            try:
                require(reinstalled != summary["sha256"] and active_revision() == summary["sha256"],
                        "the reinstalled executable differs from the active runtime", trace)
                version = command(built, "--version").stdout.strip()
                require(version.startswith("namedrop "), "the reinstalled executable runs", version)
                stale = trace["doctor_stale"] = cli("doctor", "--json", allowed=(0, 1))["data"]
                flagged = [issue for issue in stale["issues"] if issue["code"] == "ND-SETUP-004"]
                require(flagged, "doctor flags the stale runtime as ND-SETUP-004 while bin/ and current/ differ",
                        [issue["code"] for issue in stale["issues"]])
                repair = flagged[0]["repair"]["command"]
                require(repair == ["sh", str(root / "scripts/setup.sh"), "activate", "--human"],
                        "the repair activates through the registered root's setup helper", repair)
                repaired = command(*repair, check=False, context=context, timeout=120, kind="repair")
                trace["repair"] = {"exit": repaired.returncode, "stdout": repaired.stdout, "stderr": repaired.stderr}
                require(repaired.returncode == 0, "the repair command exits 0", trace["repair"])
                require(active_revision() == reinstalled, "the repair switches the active runtime to the reinstalled executable",
                        {"active": active_revision(), "reinstalled": reinstalled})
                after = trace["doctor_repaired"] = cli("doctor", "--json", allowed=(0, 1))["data"]
                require([issue["code"] for issue in after["issues"]] == ["ND-SETUP-003"],
                        "doctor shows only ND-SETUP-003 after the repair", [issue["code"] for issue in after["issues"]])
                summary["outcome"] = "passed"
            finally:
                save("stale-runtime.json", trace)
        elif links_plugin:
            directories = {"lead": base / "workspace"}

            def open_tab(key, space, label):
                panes[key] = host("tab", "create", "--workspace", spaces[space], "--cwd", str(directories[space]),
                                  "--label", label)["root_pane"]["pane_id"]
                wait(lambda: (base / "home" / f"ready-{panes[key]}").exists(), f"{key} native bash prompt")
                shell_pids.append(host("pane", "process-info", "--pane", panes[key])["process_info"]["shell_pid"])

            if args.feature == "cross-workspace":
                for space, label in (("beta", "Beta Space"), ("gamma", "gamma")):
                    directories[space] = base / space
                    directories[space].mkdir()
                    command("git", "init", "--quiet", cwd=directories[space])
                    created = host("workspace", "create", "--cwd", str(directories[space]), "--label", label)
                    spaces[space] = created["workspace"]["workspace_id"]
                    panes[space] = created["root_pane"]["pane_id"]
                    wait(lambda: (base / "home" / f"ready-{panes[space]}").exists(), f"{space} native bash prompt")
                    shell_pids.append(host("pane", "process-info", "--pane", panes[space])["process_info"]["shell_pid"])
                    command("sh", root / "scripts/setup.sh", "activate", "--json", timeout=120, cwd=directories[space],
                            context=dict(context, HERDR_WORKSPACE_ID=spaces[space], HERDR_PANE_ID=panes[space]))
                # The repository's main checkout is labelled after the repo and never activated; Herdr labels each
                # linked worktree's workspace after the worktree, so only the repo name ties them together.
                stack = directories["stack"] = base / "stack"
                stack.mkdir()
                command("git", "init", "--quiet", "-b", "main", cwd=stack)
                command("git", "-c", "user.name=verify", "-c", "user.email=verify@example.invalid", "commit", "--quiet",
                        "--allow-empty", "-m", "fixture", cwd=stack)
                created = host("workspace", "create", "--cwd", str(stack), "--label", "stack")
                for space, label in (("stack", None), ("wt-one", "wt-one"), ("wt-two", "wt-two")):
                    if label is not None:
                        directories[space] = base / label
                        command("git", "worktree", "add", "--quiet", "-b", label, str(directories[space]), cwd=stack)
                        created = host("worktree", "open", "--workspace", spaces["stack"], "--path",
                                       str(directories[space]), "--label", label)
                    spaces[space] = created["workspace"]["workspace_id"]
                    panes[space] = created["root_pane"]["pane_id"]
                    wait(lambda: (base / "home" / f"ready-{panes[space]}").exists(), f"{space} native bash prompt")
                    shell_pids.append(host("pane", "process-info", "--pane", panes[space])["process_info"]["shell_pid"])
                    if label is not None:
                        command("sh", root / "scripts/setup.sh", "activate", "--json", timeout=120,
                                cwd=directories[space],
                                context=dict(context, HERDR_WORKSPACE_ID=spaces[space], HERDR_PANE_ID=panes[space]))
                open_tab("alpha-lead", "lead", "Verification alpha lead")
                open_tab("beta-reviewer", "beta", "Verification beta reviewer")
                open_tab("wt-one-twin", "wt-one", "Verification wt-one twin")
                cast = (("alpha-lead", "lead", "alpha-lead", "lead"), ("alpha-helper", "lead", "worker", "helper"),
                        ("beta-lead", "beta", "beta", "lead"), ("beta-reviewer", "beta", "beta-reviewer", "reviewer"),
                        ("gamma-lead", "gamma", "gamma", "lead"), ("wt-one-builder", "wt-one", "wt-one", "builder"),
                        ("wt-one-twin", "wt-one", "wt-one-twin", "twin"), ("wt-two-twin", "wt-two", "wt-two", "twin"))
            else:
                open_tab("lead-agent", "lead", "Verification lead agent")
                cast = (("lead", "lead", "lead-agent", "lead"), ("worker", "lead", "worker", "worker"))
            simulator = base / "codex"
            compile_simulator(simulator)
            renderer = base / "render.py"
            renderer.write_text(RENDERER)
            agents = {role: {"pane": panes[key], "space": spaces[space], "cwd": directories[space], "alias": alias,
                             "session": f"{index:08d}-1111-4111-8111-{index:012d}",
                             "surface": base / f"{role}-surface", "envelopes": base / f"{role}-envelopes.jsonl",
                             "pids": base / f"{role}-pids"}
                      for index, (role, space, key, alias) in enumerate(cast, 1)}
            space_of = {agent["pane"]: agent["space"] for agent in agents.values()}

            def pane_row(role):
                return next((p for p in host("pane", "list")["panes"] if p["pane_id"] == agents[role]["pane"]), {})

            def settled(status):
                # Herdr reports the first prompt as idle and a prompt after work as done.
                return status in ("idle", "done")

            def reported(role, status):
                row = pane_row(role)
                matches = settled(row.get("agent_status")) if settled(status) else row.get("agent_status") == status
                return row if matches and (row.get("agent_session") or {}).get("value") == agents[role]["session"] else None

            def plugin_log():
                return host("plugin", "log", "list", "--plugin", "namedrop", "--limit", "500")["logs"]

            def hook_logged_since(started_ms):
                return min((row["started_unix_ms"] for row in plugin_log() if row.get("event") == "pane.agent_status_changed"
                            and row.get("exit_code") == 0 and row["started_unix_ms"] >= started_ms), default=None)

            def events_between(start_ms, end_ms):
                return [row for row in plugin_log() if start_ms < row["started_unix_ms"] < end_ms]

            def observe(role, status):
                previous = pane_row(role).get("agent_status")
                requested_ms = now_ms()
                staged = agents[role]["surface"].with_suffix(".next")
                staged.write_text(status)
                staged.replace(agents[role]["surface"])
                observed = wait(lambda: reported(role, status), f"Herdr to report {role} {status} for session {agents[role]['session']}")
                observed_ms = now_ms()
                hook_started_ms = None
                if settled(previous) != settled(status):
                    hook_started_ms = wait(lambda: hook_logged_since(requested_ms),
                                           f"the pane.agent_status_changed hook after {role} {status}")
                    wait(lambda: not list((state / "agent-status-reconciliation").glob("*/pending-*")),
                         "the status reconciliation directory to hold no pending markers")
                record({"kind": "status", "role": role, "previous": previous, "surface": status,
                        "observed": observed["agent_status"], "requested_ms": requested_ms, "observed_ms": observed_ms,
                        "hook_started_ms": hook_started_ms})
                return {"observed_ms": observed_ms, "hook_started_ms": hook_started_ms}

            def tool_environment(pane):
                return {"HERDR_PLUGIN_STATE_DIR": str(state), "HERDR_SOCKET_PATH": env["HERDR_SOCKET_PATH"],
                        "HERDR_WORKSPACE_ID": space_of[pane], "HERDR_PANE_ID": pane}

            def agent_call(*argv, pane, **payload):
                invocation = [str(launcher), "--plugin-state", str(state), "--herdr-socket", env["HERDR_SOCKET_PATH"],
                              "--herdr-workspace", space_of[pane], "--herdr-pane", pane, *map(str, argv)]
                if payload:
                    invocation += ["--input-json", json.dumps(payload)]
                result = command(*invocation, context=tool_environment(pane), check=False, kind="agent-cli")
                response = json.loads(result.stdout) if result.stdout.strip().startswith("{") else {}
                return invocation, result, response

            def as_agent(*argv, pane, **payload):
                invocation, result, response = agent_call(*argv, pane=pane, **payload)
                assert result.returncode == 0 and response.get("status") == "ok", (invocation, result.stdout, result.stderr)
                return response["data"]

            def attempt_state(attempt):
                found = rows("SELECT state FROM delivery_attempt_state WHERE attempt_id = ?", (attempt,))
                return found[0][0] if found else None

            def disposition(attempt):
                found = rows("SELECT disposition_kind FROM delivery_dispositions WHERE attempt_id = ?", (attempt,))
                return found[0][0] if found else None

            def bound(envelope, token, outcome=None, **authored):
                tool = envelope["control"]["tool"]
                operation = next(op for op in tool["operations"] if op["token"] == token
                                 and (outcome is None or op["input"].get("outcome") == outcome))
                bound_env = tool["environment"]
                assert bound_env["HERDR_SOCKET_PATH"] == env["HERDR_SOCKET_PATH"], bound_env
                assert space_of.get(bound_env["HERDR_PANE_ID"]) == bound_env["HERDR_WORKSPACE_ID"], bound_env
                assert bound_env["HERDR_PLUGIN_STATE_DIR"] == str(state), bound_env
                payload = dict(operation["input"], **authored)
                for field in ("attempt_id", "in_reply_to", "coordination_id", "assignment_id", "expected_revision", "request_id"):
                    assert field not in operation["input"] or payload[field] == operation["input"][field], field
                attempt = envelope["control"]["attempt_id"]
                wait(lambda: attempt_state(attempt) in ("submitted", "acknowledged", "settled"),
                     f"attempt {attempt} to leave dispatching before its bound {token}")
                invocation = [tool["executable"], "--plugin-state", bound_env["HERDR_PLUGIN_STATE_DIR"], *operation["argv"],
                              tool["input_flag"], json.dumps(payload), "--herdr-socket", bound_env["HERDR_SOCKET_PATH"],
                              "--herdr-workspace", bound_env["HERDR_WORKSPACE_ID"], "--herdr-pane", bound_env["HERDR_PANE_ID"]]
                response = json.loads(command(*invocation, context=bound_env, kind="bound").stdout)
                assert response.get("status") == "ok", response
                return response["data"]

            def envelope(role, predicate, label):
                return wait(lambda: next((e for e in read_envelopes(agents[role]["envelopes"]) if predicate(e)), None), label)

            for role, agent in agents.items():
                agent["surface"].write_text("idle")
                script = base / f"start-{role}.sh"
                script.write_text("#!/bin/sh\nstty -icanon -echo min 1 time 0\n"
                                  + shlex.join([sys.executable, str(renderer), str(agent["surface"]), f"Synthetic {role}"])
                                  + " &\nprintf '%s\\n' \"$$\" \"$!\" > " + shlex.quote(str(agent["pids"])) + "\nexec "
                                  + shlex.quote(str(simulator)) + " >> " + shlex.quote(str(agent["envelopes"])) + "\n")
                host("pane", "run", agent["pane"], shlex.join(["sh", str(script)]))
                wait(lambda: agent["pids"].exists() and len(agent["pids"].read_text().splitlines()) == 2,
                     f"{role} simulator and renderer PID file")
                shell_pids.extend(int(pid) for pid in agent["pids"].read_text().split())
                wait(lambda: pane_row(role).get("agent") == "codex", f"Herdr to detect the {role} simulator as codex")
                host("pane", "report-agent-session", agent["pane"], "--source", "herdr:codex", "--agent", "codex",
                     "--agent-session-id", agent["session"])
                observe(role, "idle")
            for agent in agents.values():
                as_agent("name", agent["alias"], "--type", "agent", pane=agent["pane"])
            for role in agents:
                observe(role, "working")
                observe(role, "idle")
            expected_sessions = {agent["session"] for agent in agents.values()}
            def active_registrations():
                found = rows("SELECT address_id, agent_session_id FROM integration_registrations WHERE active = 1")
                return found if {session for _, session in found} == expected_sessions else None

            registrations = wait(active_registrations, "active registrations for every simulated session")
            roster = cli("doctor", "--json", allowed=(0, 1))
            assert roster["data"]["available_agents"] == sum(a["space"] == spaces["lead"] for a in agents.values()), roster

            if args.feature == "stuck-wake":
                begun = as_agent("handoff", "begin", pane=agents["lead"]["pane"], request_id="stuck-wake-begin",
                                 deliverable="Report the synthetic checksum once the worker replies.")
                coordination = begun["coordination_id"]
                sent = as_agent("send", pane=agents["lead"]["pane"], request_id="stuck-wake-send", author_kind="agent_session",
                                source="@worker Compute the synthetic checksum and reply with it.", coordination_id=coordination)
                request = envelope("worker", lambda e: e["control"]["message_id"] == sent["message_id"], "the worker's request envelope")
                replied = bound(request, "reply", source="@lead Checksum 42.", work_outcome="completed")
                receipt = envelope("lead", lambda e: e["control"]["message_id"] == replied["message_id"], "the lead's reply envelope")
                bound(receipt, "complete.no-reply")
                wake = envelope("lead", lambda e: e["control"].get("coordination_nudge") is True, "the coordination nudge envelope")
                wake_attempt = wake["control"]["attempt_id"]
                t0 = rows("SELECT created_at_ms FROM delivery_attempts WHERE attempt_id = ?", (wake_attempt,))[0][0]

                observe("lead", "working")
                resolved = bound(wake, "handoff.resolve", outcome="completed", report="Synthetic checksum 42 reported.")
                assert resolved["report_saved"] is True and resolved["report_message_id"], resolved
                report_message = resolved["report_message_id"]
                idle = observe("lead", "idle")
                idle_at_ms = idle["observed_ms"]
                if idle_at_ms - t0 >= 25_000:
                    summary["outcome"] = "inconclusive"
                    raise RuntimeError(f"inconclusive: the nudge turn ended {idle_at_ms - t0} ms after the wake lease, "
                                       "outside the 25 s budget that keeps it inside the 30 s acknowledgement grace")

                def ledger():
                    report_state = rows("SELECT state FROM mailbox_deliveries WHERE message_id = ? ORDER BY mailbox_order",
                                        (report_message,))
                    arrived = next((e for e in read_envelopes(agents["lead"]["envelopes"])
                                    if e["control"]["message_id"] == report_message), None)
                    return {"at_ms": now_ms(), "wake_state": attempt_state(wake_attempt),
                            "disposition": disposition(wake_attempt),
                            "report_state": ",".join(row[0] for row in report_state) or None, "report_envelope": arrived}

                def save_wake_evidence(polls, events_in_window, **extra):
                    save("stuck-wake.json", {
                        "panes": panes, "sessions": {role: agent["session"] for role, agent in agents.items()},
                        "simulator_pids": {role: agent["pids"].read_text().split() for role, agent in agents.items()},
                        "registrations": registrations, "coordination_id": coordination, "wake_attempt_id": wake_attempt,
                        "t0_ms": t0, "idle_at_ms": idle_at_ms, "idle_hook_started_ms": idle["hook_started_ms"],
                        "wake_row": rows("SELECT attempt_id, state, active FROM delivery_attempt_state WHERE attempt_id = ?",
                                         (wake_attempt,)),
                        "dispositions": rows("SELECT disposition_id, disposition_kind, occurred_at_ms FROM delivery_dispositions "
                                             "WHERE attempt_id = ?", (wake_attempt,)),
                        "report_delivery_state": ledger()["report_state"], "events_in_window": events_in_window,
                        "polls": polls, **extra})
                    for role, agent in agents.items():
                        save(f"{role}-envelopes.json", read_envelopes(agent["envelopes"]))

                def await_unprompted_settlement():
                    polls = []
                    deadline = time.monotonic() + 50
                    while True:
                        seen = ledger()
                        polls.append({**{key: value for key, value in seen.items() if key != "report_envelope"},
                                      "report_arrived": seen["report_envelope"] is not None})
                        if seen["report_envelope"] is not None:
                            return seen, polls
                        if time.monotonic() >= deadline:
                            break
                        time.sleep(0.5)
                    stalled_at_ms = seen["at_ms"]
                    events_in_window = events_between(idle["hook_started_ms"], stalled_at_ms)
                    doctor_at_stall = cli("doctor", "--json", allowed=(0, 1))["data"]
                    backup_database()
                    summary["outcome"] = "stalled"
                    save_wake_evidence(polls, events_in_window, stalled=True, stalled_at_ms=stalled_at_ms, arrived_at_ms=None,
                                       settled_at_ms=next((poll["at_ms"] for poll in polls if poll["wake_state"] == "settled"), None),
                                       doctor_at_stall=doctor_at_stall)
                    raise RuntimeError(f"stuck wake: attempt {wake_attempt} still {seen['wake_state']} with disposition "
                                       f"{seen['disposition'] or 'none'} {(stalled_at_ms - idle_at_ms) // 1000} s after idle; "
                                       f"report delivery {seen['report_state']}; no report envelope reached the lead")

                seen, polls = await_unprompted_settlement()
                arrived_at_ms = seen["at_ms"]
                assert seen["wake_state"] == "settled" and seen["disposition"] == "no_reply", seen
                settled_at_ms = next(poll["at_ms"] for poll in polls if poll["wake_state"] == "settled")
                final_report = seen["report_envelope"]
                assert final_report["control"]["reply_mode"] == "report_to_human", final_report["control"]
                events_in_window = events_between(idle["hook_started_ms"], arrived_at_ms)
                assert events_in_window == [], events_in_window
                doctor_after_settle = cli("doctor", "--json", allowed=(0, 1))["data"]
                assert not any(issue["code"] == "ND-DELIVERY-003" for issue in doctor_after_settle["issues"]), doctor_after_settle
                bound(final_report, "complete.no-reply")
                doctor_after_report = cli("doctor", "--json", allowed=(0, 1))["data"]
                issues = doctor_after_report["issues"]
                assert not any(issue["code"] == "ND-DELIVERY-003" for issue in issues), issues
                assert not any(issue["code"] == "ND-COORDINATION-001" and issue.get("diagnostic", {}).get("source_id") == coordination
                               for issue in issues), issues
                summary["outcome"] = "passed"
                save_wake_evidence(polls, events_in_window, stalled=False, settled_at_ms=settled_at_ms, arrived_at_ms=arrived_at_ms,
                                   settled_after_ms=settled_at_ms - t0, arrived_after_ms=arrived_at_ms - t0,
                                   doctor_after_settle=doctor_after_settle, doctor_after_report=doctor_after_report)
            elif args.feature == "idle-worker":
                lead_pane = agents["lead"]["pane"]
                trace = {"panes": panes, "sessions": {role: agent["session"] for role, agent in agents.items()},
                         "simulator_pids": {role: agent["pids"].read_text().split() for role, agent in agents.items()},
                         "registrations": registrations}

                def require(condition, requirement, observed):
                    if not condition:
                        summary["outcome"] = requirement
                        raise RuntimeError(f"idle worker: {requirement}; observed {observed}")

                def doctor_read(case, name):
                    report = case[name] = cli("doctor", "--json", allowed=(0, 1))["data"]
                    flagged = [issue for issue in report["issues"] if issue["code"] == "ND-DELIVERY-003"]
                    require(not flagged, f"{name} shows no ND-DELIVERY-003",
                            f"{len(flagged)} ND-DELIVERY-003 issue(s) with source_id "
                            f"{[issue.get('diagnostic', {}).get('source_id') for issue in flagged]}: {json.dumps(flagged)}")
                    return report

                def names_coordination(report, coordination):
                    return any(issue["code"] == "ND-COORDINATION-001"
                               and issue.get("diagnostic", {}).get("source_id") == coordination for issue in report["issues"])

                def nudge_for(coordination, excluding=None):
                    return envelope("lead", lambda e: e["control"].get("coordination_nudge") is True
                                    and e["control"].get("coordination_id") == coordination
                                    and e["control"]["message_id"] != excluding, f"a coordination nudge for {coordination}")

                def idle_assignment(label, task, doctor_name):
                    case = trace[label] = {}
                    begun = as_agent("handoff", "begin", pane=lead_pane, request_id=f"idle-worker-{label}-begin",
                                     deliverable=f"Report the {label} synthetic checksum once the worker replies.")
                    coordination = case["coordination_id"] = begun["coordination_id"]
                    sent = as_agent("send", pane=lead_pane, request_id=f"idle-worker-{label}-send", author_kind="agent_session",
                                    source=f"@worker {task}", coordination_id=coordination)
                    request = envelope("worker", lambda e: e["control"]["message_id"] == sent["message_id"],
                                       f"the worker's {label} request envelope")
                    attempt = request["control"]["attempt_id"]
                    wait(lambda: attempt_state(attempt) in ("submitted", "acknowledged", "settled"),
                         f"attempt {attempt} to leave dispatching")
                    t0 = rows("SELECT created_at_ms FROM delivery_attempts WHERE attempt_id = ?", (attempt,))[0][0]
                    case.update(assignment=request["control"]["assignments"][0], worker_attempt_id=attempt, request=request,
                                t0_ms=t0, working_at_ms=observe("worker", "working")["observed_ms"])
                    time.sleep(max(0, (t0 + 5_000 - now_ms()) / 1000))
                    idle = observe("worker", "idle")
                    case.update(idle_at_ms=idle["observed_ms"], idle_hook_started_ms=idle["hook_started_ms"])
                    if idle["observed_ms"] - t0 >= 25_000:
                        summary["outcome"] = "inconclusive"
                        raise RuntimeError(f"inconclusive: the {label} worker went idle {idle['observed_ms'] - t0} ms after its "
                                           "attempt was created, outside the 25 s budget that keeps it inside the 30 s grace")
                    polls = []
                    deadline = time.monotonic() + 50
                    while True:
                        seen = {"at_ms": now_ms(), "state": attempt_state(attempt), "disposition": disposition(attempt)}
                        polls.append(seen)
                        if seen["state"] == "settled" or time.monotonic() >= deadline:
                            break
                        time.sleep(0.5)
                    settled = seen["state"] == "settled"
                    case.update(polls=polls, settled_at_ms=seen["at_ms"] if settled else None,
                                settled_after_ms=seen["at_ms"] - t0 if settled else None,
                                events_in_window=events_between(idle["hook_started_ms"], seen["at_ms"]))
                    require(settled, f"the {label} worker attempt settles by observation after the grace", seen)
                    require(seen["disposition"] is None, f"the {label} worker attempt settles without a disposition", seen)
                    doctor_read(case, doctor_name)
                    nudge = nudge_for(coordination)
                    case.update(nudge=nudge, nudge_completed=bound(nudge, "complete.no-reply"))
                    return case

                try:
                    positive = idle_assignment("positive", "Compute the synthetic checksum and reply with it.", "doctor_after_settle")
                    attempt = positive["worker_attempt_id"]
                    time.sleep(max(0, (positive["settled_at_ms"] + 10_000 - now_ms()) / 1000))
                    try:
                        replied = bound(positive["request"], "reply", source="@lead Checksum 42.", work_outcome="completed")
                    except RuntimeError as refusal:
                        require(False, "the worker's late reply exits 0", refusal)
                    replied_at_ms = now_ms()
                    positive.update(replied_at_ms=replied_at_ms, replied_after_settle_ms=replied_at_ms - positive["settled_at_ms"],
                                    replied=replied, dispositions=rows("SELECT disposition_kind, occurred_at_ms FROM delivery_dispositions "
                                                                       "WHERE attempt_id = ?", (attempt,)))
                    require(disposition(attempt) == "reply", "the worker attempt records disposition reply after the late reply",
                            positive["dispositions"])
                    doctor_read(positive, "doctor_after_reply")
                    result_envelope = envelope("lead", lambda e: e["control"]["message_id"] == replied["message_id"],
                                               "the lead's result envelope")
                    positive.update(result_envelope=result_envelope, result_completed=bound(result_envelope, "complete.no-reply"))
                    second_nudge = nudge_for(positive["coordination_id"], excluding=positive["nudge"]["control"]["message_id"])
                    resolved = bound(second_nudge, "handoff.resolve", outcome="completed",
                                     report="Synthetic checksum 42 reported after the worker's late reply.")
                    positive.update(second_nudge=second_nudge, resolved=resolved,
                                    second_nudge_completed=bound(second_nudge, "complete.no-reply"))
                    report_envelope = envelope("lead", lambda e: e["control"]["message_id"] == resolved["report_message_id"],
                                               "the completed report envelope")
                    positive.update(report_envelope=report_envelope, report_completed=bound(report_envelope, "complete.no-reply"))
                    doctor_read(positive, "doctor_after_resolve")
                    require(not names_coordination(positive["doctor_after_resolve"], positive["coordination_id"]),
                            "doctor_after_resolve shows no ND-COORDINATION-001 for the completed coordination",
                            positive["doctor_after_resolve"]["issues"])

                    negative = idle_assignment("negative", "Compute the second synthetic checksum and reply with it.",
                                               "doctor_after_second_settle")
                    attempt = negative["worker_attempt_id"]
                    cancelled = as_agent("handoff", "resolve", pane=lead_pane, request_id="idle-worker-negative-cancel",
                                         coordination_id=negative["coordination_id"], outcome="cancelled",
                                         reason="The worker never replied to its assignment.",
                                         report="Cancelled: the second synthetic checksum was never reported.")
                    negative.update(cancelled_at_ms=now_ms(), cancelled=cancelled, doctor_polls=[])
                    deadline = time.monotonic() + 20
                    while True:
                        doctor = cli("doctor", "--json", allowed=(0, 1))["data"]
                        flagged = [issue for issue in doctor["issues"] if issue["code"] == "ND-DELIVERY-003"
                                   and issue.get("diagnostic", {}).get("source_id") == attempt]
                        negative["doctor_polls"].append({"at_ms": now_ms(), "issues": [
                            [issue["code"], issue.get("diagnostic", {}).get("source_id")] for issue in doctor["issues"]]})
                        if flagged or time.monotonic() >= deadline:
                            break
                        time.sleep(1)
                    negative.update(doctor_after_cancel=doctor,
                                    coordination_flagged=names_coordination(doctor, negative["coordination_id"]))
                    require(flagged, "doctor_after_cancel shows ND-DELIVERY-003 for the second worker attempt once its coordination "
                            "is cancelled unanswered", negative["doctor_polls"][-1]["issues"])
                    report_envelope = envelope("lead", lambda e: e["control"]["message_id"] == cancelled["report_message_id"],
                                               "the cancelled report envelope")
                    negative.update(report_envelope=report_envelope, report_completed=bound(report_envelope, "complete.no-reply"))
                    summary["outcome"] = "passed"
                finally:
                    if summary.get("outcome") != "passed":
                        backup_database()
                    save("idle-worker.json", trace)
                    for role, agent in agents.items():
                        save(f"{role}-envelopes.json", read_envelopes(agent["envelopes"]))
            else:
                summary["limits"] += (" Each prompt hook runs as a host process that Herdr vouches for through the pane's"
                                      " CODEX_SESSION_ID, not as a child of a real agent process. Herdr cannot list"
                                      " notifications, so the hook's \"Sent to\" banner is unobserved; the relay is"
                                      " observed through the reply's receipt and the source pane's metadata. No person"
                                      " answers the workspace picker.")
                hook_executable = [launcher, "hook", "user-prompt-submit", "--state-directory", state]
                wrappers = {runtime: base / "home" / directory / "skills/namedrop/scripts/user-prompt-submit"
                            for runtime, directory in (("codex", ".agents"), ("claude", ".claude"))}
                blocked = {"decision": "block", "reason": "Collaboration message sent."}
                expected = dict.fromkeys(agents, 0)
                trace = {"panes": panes, "spaces": spaces, "sessions": {role: agent["session"] for role, agent in agents.items()},
                         "registrations": registrations, "requirements": {}}

                def require(condition, requirement, observed):
                    trace["requirements"][requirement] = {"passed": bool(condition), "observed": observed}
                    if not condition:
                        summary["outcome"] = requirement
                        raise RuntimeError(f"cross-workspace: {requirement}; observed {observed}")

                def hook(role, prompt, executable=hook_executable):
                    agent = agents[role]
                    payload = {"hook_event_name": "UserPromptSubmit", "session_id": agent["session"],
                               "cwd": str(agent["cwd"]), "prompt": prompt}
                    result = command(*executable, stdin=json.dumps(payload), check=False, kind="hook", cwd=agent["cwd"],
                                     context=dict(tool_environment(agent["pane"]), CODEX_SESSION_ID=agent["session"]))
                    try:
                        output = json.loads(result.stdout)
                    except ValueError:
                        output = None
                    return {"role": role, "prompt": prompt, "exit": result.returncode, "stdout": result.stdout,
                            "stderr": result.stderr, "output": output}

                def wake(role):
                    # The generated wrapper runs this after a block; a bare hook call never drains.
                    agent = agents[role]
                    command(launcher, "hook", "mailbox-wake", "--state-directory", state, stdin="", kind="wake",
                            cwd=agent["cwd"], context=dict(tool_environment(agent["pane"]), CODEX_SESSION_ID=agent["session"]))

                def counts():
                    return {role: len(read_envelopes(agent["envelopes"])) for role, agent in agents.items()}

                def deliveries(step, recipients):
                    for role in recipients:
                        expected[role] += 1
                    try:
                        wait(lambda: counts() == expected, f"{step} envelopes")
                    except RuntimeError:
                        pass
                    time.sleep(3)
                    observed = counts()
                    require(observed == expected, f"{step}: exactly {recipients or 'nobody'} receive a new envelope", observed)
                    return {role: read_envelopes(agents[role]["envelopes"])[-1] for role in recipients}

                def message_state():
                    return {table: rows(f"SELECT COUNT(*) FROM {table}")[0][0] for table in
                            ("collaboration_messages", "mailbox_deliveries", "delivery_attempts", "bridged_messages",
                             "space_envoys")}

                def sender_workspace(envelope):
                    return envelope["control"]["sender"].get("workspace")

                def relay_token(role):
                    return host("pane", "get", agents[role]["pane"])["pane"].get("tokens", {}).get("namedrop-relay")

                try:
                    listed = trace["workspace_list"] = host("workspace", "list")
                    labels = {w["workspace_id"]: w.get("label") for w in listed["workspaces"]}
                    require(labels == {spaces["lead"]: "alpha", spaces["beta"]: "Beta Space", spaces["gamma"]: "gamma",
                                       spaces["stack"]: "stack", spaces["wt-one"]: "wt-one", spaces["wt-two"]: "wt-two"},
                            "g: herdr workspace list reports the labels alpha, Beta Space, gamma, stack, wt-one and wt-two",
                            labels)
                    repos = {w["workspace_id"]: (w.get("worktree") or {}).get("repo_name") for w in listed["workspaces"]}
                    require(repos[spaces["wt-one"]] == "stack" and repos[spaces["wt-two"]] == "stack",
                            "g: Herdr stamps both linked worktrees with the repository name stack", repos)
                    for runtime, wrapper in wrappers.items():
                        require(wrapper.is_file(), f"h: activation installed the {runtime} wrapper", str(wrapper))
                        (evidence / f"{runtime}-user-prompt-submit.sh").write_text(wrapper.read_text())

                    sent = trace["a"] = hook("alpha-lead", "#beta-space @lead hi")
                    require(sent["exit"] == 0 and sent["output"] == blocked,
                            "a: the hook blocks with the literal Collaboration message sent.", sent["stdout"])
                    wake("alpha-lead")
                    arrived = deliveries("a", ["beta-lead"])["beta-lead"]
                    require(arrived["untrusted_body"].endswith("hi"), "a: Beta Space's lead receives the prompt",
                            arrived["untrusted_body"])
                    require(sender_workspace(arrived) == "alpha", "a: the envelope's sender names workspace alpha",
                            arrived["control"]["sender"])
                    inbox = as_agent("delivery", "inbox", pane=agents["beta-lead"]["pane"])["envelopes"]
                    require(arrived in inbox, "a: Beta Space's lead's delivery inbox holds the received envelope",
                            [e["control"]["attempt_id"] for e in inbox])
                    inbox = as_agent("delivery", "inbox", pane=agents["alpha-lead"]["pane"])["envelopes"]
                    require(inbox == [], "a: alpha's lead owes no delivery", [e["control"]["attempt_id"] for e in inbox])
                    bridged = trace["bridged_after_a"] = rows("SELECT worktree_id, origin_space_id, origin_label_snapshot "
                                                              "FROM bridged_messages")
                    require([row[:2] for row in bridged] == [(spaces["beta"], spaces["lead"])],
                            "g: #beta-space forwarded into the workspace labelled Beta Space", bridged)

                    operation = next(op for op in arrived["control"]["tool"]["operations"] if op["token"] == "reply")
                    prefill = operation["input"].get("source", "")
                    require(prefill == "@lead <reply body>", "b: the bound reply addresses the sender without a workspace",
                            prefill)
                    replied = trace["b"] = bound(arrived, "reply", source=prefill.replace("<reply body>", "Beta lead here."))
                    relay = replied.get("relay") or {}
                    require(relay.get("notification_shown") is True and relay.get("metadata_error") is None,
                            "b: the reply shows the human relay notification and reports pane metadata", relay)
                    returned = deliveries("b", ["alpha-lead"])["alpha-lead"]
                    require(returned["untrusted_body"] == "Beta lead here." and
                            returned["control"]["reply_mode"] == "report_to_human",
                            "b: alpha's lead receives the reply as a report to the human",
                            {"body": returned["untrusted_body"], "reply_mode": returned["control"]["reply_mode"]})
                    targets = trace["relay_tokens"] = {role: relay_token(role) for role in ("alpha-lead", "beta-lead")}
                    require(targets == {"alpha-lead": "@lead: Beta lead here.", "beta-lead": None},
                            "b: the relay metadata targets alpha's lead pane only", targets)
                    bound(returned, "complete.no-reply")

                    wrapped = trace["h_send"] = hook("alpha-lead", "#beta-space @lead hi from the wrapper",
                                                     [wrappers["codex"]])
                    require(wrapped["exit"] == 0 and wrapped["output"] == blocked,
                            "h: the installed codex wrapper passes the block through", wrapped["stdout"])
                    forwarded = deliveries("h-send", ["beta-lead"])["beta-lead"]
                    require(forwarded["untrusted_body"].endswith("hi from the wrapper") and sender_workspace(forwarded) == "alpha",
                            "h: the wrapper's own mailbox wake delivers to Beta Space's lead",
                            {"body": forwarded["untrusted_body"], "sender": forwarded["control"]["sender"]})
                    bound(forwarded, "complete.no-reply")

                    local = trace["c"] = hook("alpha-helper", "@lead local check")
                    require(local["output"] == blocked, "c: the hook blocks an unqualified local send", local["stdout"])
                    wake("alpha-helper")
                    stayed = deliveries("c", ["alpha-lead"])["alpha-lead"]
                    require(stayed["untrusted_body"].endswith("local check") and sender_workspace(stayed) is None,
                            "c: alpha's own lead receives it with a local sender",
                            {"body": stayed["untrusted_body"], "sender": stayed["control"]["sender"]})
                    bound(stayed, "complete.no-reply")

                    remote = trace["d"] = hook("alpha-helper", "@reviewer please review")
                    require(remote["output"] == blocked, "d: the hook blocks an unqualified send to a remote-only name",
                            remote["stdout"])
                    wake("alpha-helper")
                    reviewed = deliveries("d", ["beta-reviewer"])["beta-reviewer"]
                    require(reviewed["untrusted_body"].endswith("please review") and sender_workspace(reviewed) == "alpha",
                            "d: Beta Space's reviewer receives it from alpha",
                            {"body": reviewed["untrusted_body"], "sender": reviewed["control"]["sender"]})
                    bound(reviewed, "complete.no-reply")

                    everyone = trace["f"] = hook("alpha-helper", "#all @lead status")
                    require(everyone["output"] == blocked, "f: the hook blocks #all", everyone["stdout"])
                    wake("alpha-helper")
                    leads = deliveries("f", ["alpha-lead", "beta-lead", "gamma-lead"])
                    require(all(e["untrusted_body"].endswith("status") for e in leads.values()),
                            "f: every workspace's lead receives the status prompt",
                            {role: e["untrusted_body"] for role, e in leads.items()})
                    for envelope in leads.values():
                        bound(envelope, "complete.no-reply")

                    built = trace["i"] = hook("alpha-helper", "#stack @builder build it")
                    require(built["output"] == blocked,
                            "i: the hook blocks #stack, which names the repository, not a workspace with the name",
                            built["stdout"])
                    wake("alpha-helper")
                    arrived = deliveries("i", ["wt-one-builder"])["wt-one-builder"]
                    require(arrived["untrusted_body"].endswith("build it") and sender_workspace(arrived) == "alpha",
                            "i: wt-one's builder receives it from alpha",
                            {"body": arrived["untrusted_body"], "sender": arrived["control"]["sender"]})
                    bound(arrived, "complete.no-reply")

                    before_twin = message_state()
                    twin = trace["j"] = hook("alpha-helper", "#stack @twin which one?")
                    twin_text = ((twin["output"] or {}).get("hookSpecificOutput") or {}).get("additionalContext", "")
                    require(twin_text.startswith("Namedrop workspace roster.")
                            and "@twin is named in more than one stack worktree: #wt-one, #wt-two." in twin_text
                            and "Nothing was sent" in twin_text,
                            "j: the picker names the stack repository and offers #wt-one and #wt-two", twin_text)
                    wake("alpha-helper")
                    deliveries("j", [])
                    require(message_state() == before_twin, "j: the picker persists nothing", message_state())

                    for step, prompt, offered in (
                            ("k", "#stack run the tests", "#wt-one @builder, #wt-one @twin, #wt-two @twin"),
                            ("k-one", "#wt-two run the tests", "#wt-two @twin")):
                        asked = trace[step] = hook("alpha-helper", prompt)
                        asked_text = ((asked["output"] or {}).get("hookSpecificOutput") or {}).get("additionalContext", "")
                        scope = prompt.split()[0]
                        require(asked_text.startswith("Namedrop workspace roster.")
                                and f"It names {scope} but no agent there. Its agents: {offered}. Nothing was sent"
                                in asked_text,
                                f"{step}: {prompt!r} with no Mention asks who, offering {offered}", asked_text)
                        wake("alpha-helper")
                        deliveries(step, [])
                        require(message_state() == before_twin, f"{step}: the recipient picker persists nothing",
                                message_state())

                    mixed_prompt = "#stack @builder @twin both of you"
                    mixed = trace["l"] = hook("alpha-helper", mixed_prompt)
                    mixed_text = ((mixed["output"] or {}).get("hookSpecificOutput") or {}).get("additionalContext", "")
                    require(mixed_text.startswith("Namedrop workspace roster.")
                            and "@twin is named in more than one stack worktree: #wt-one, #wt-two. These names are "
                                "already resolved: #wt-one @builder. Nothing was sent" in mixed_text
                            and "once per workspace, never with #all" in mixed_text,
                            "l: the picker asks only about @twin and keeps @builder resolved in #wt-one", mixed_text)
                    wake("alpha-helper")
                    deliveries("l", [])
                    require(message_state() == before_twin, "l: the mixed picker persists nothing", message_state())
                    # "All of them", resent the way the picker prescribes: one send per workspace, each with every
                    # name bound there and the rest of the prompt after its last leading @name.
                    text = mixed_prompt.split(" ", 3)[3]
                    resends = trace["l_resend"] = [f"#wt-one @builder @twin {text}", f"#wt-two @twin {text}"]
                    for index, source in enumerate(resends):
                        as_agent("send", pane=agents["alpha-helper"]["pane"], request_id=f"xws-mixed-{index}",
                                 author_kind="agent_session", source=source)
                    wake("alpha-helper")
                    resent = deliveries("l-resend", ["wt-one-builder", "wt-one-twin", "wt-two-twin"])
                    require(all(e["untrusted_body"].endswith(text) and sender_workspace(e) == "alpha"
                                for e in resent.values()),
                            "l: the guided resend reaches wt-one's builder and both twins from alpha",
                            {role: {"body": e["untrusted_body"], "sender": e["control"]["sender"]}
                             for role, e in resent.items()})
                    for envelope in resent.values():
                        bound(envelope, "complete.no-reply")
                    # The #wt-one send addressed two names, so their answers come home as one result set.
                    wake("alpha-helper")
                    results = deliveries("l-result-set", ["alpha-helper"])["alpha-helper"]
                    outcomes = sorted((o["target"]["alias"], o["status"])
                                      for o in (results["control"].get("result_set") or {}).get("outcomes", []))
                    require(outcomes == [("builder", "no_reply"), ("twin", "no_reply")],
                            "l: alpha's helper gets one result set for the #wt-one send", outcomes)
                    bound(results, "complete.no-reply")

                    before_bare = message_state()
                    for step, source in (("m", "#wt-two"), ("m-indented", "  #wt-two run the tests")):
                        _, result, response = agent_call("send", pane=agents["alpha-helper"]["pane"],
                                                         request_id=f"xws-{step}", author_kind="agent_session",
                                                         source=source)
                        message = (response.get("error") or {}).get("message", "")
                        trace[step] = {"source": source, "exit": result.returncode, "stdout": result.stdout,
                                       "stderr": result.stderr}
                        require(result.returncode == 65 and response.get("status") == "rejected"
                                and "#wt-two has these named agents: #wt-two @twin" in message,
                                f"{step}: agent send {source!r} is refused with #wt-two's names", trace[step])
                    deliveries("m", [])
                    require(message_state() == before_bare, "m: the refused agent sends persist nothing",
                            message_state())

                    asked_review = as_agent("send", pane=agents["alpha-helper"]["pane"], request_id="xws-review",
                                            author_kind="agent_session", source="#beta-space @reviewer review the patch")
                    wake("alpha-helper")
                    review = deliveries("n-send", ["beta-reviewer"])["beta-reviewer"]
                    reply_op = next(op for op in review["control"]["tool"]["operations"] if op["token"] == "reply")
                    before_reply = message_state()
                    _, result, response = agent_call(*reply_op["argv"], pane=agents["beta-reviewer"]["pane"],
                                                     **dict(reply_op["input"], source="@twin Reviewed."))
                    message = (response.get("error") or {}).get("message", "")
                    trace["n"] = {"exit": result.returncode, "stdout": result.stdout, "stderr": result.stderr}
                    require(result.returncode == 65 and response.get("status") == "rejected"
                            and "@twin is not named in this workspace but is named in #wt-one and #wt-two" in message
                            and "nothing was sent and the reply's delivery attempt stays open" in message
                            and "except the source, which is @helper, a space, and the message text" in message
                            and "then tell the person the reply left out @twin" in message,
                            "n: the ambiguous reply is refused with a retry addressed to its sender", trace["n"])
                    deliveries("n-refused", [])
                    require(message_state() == before_reply, "n: the refused reply persists nothing", message_state())
                    trace["n_reply"] = bound(review, "reply", source="@helper Reviewed.")
                    as_agent("send", pane=agents["beta-reviewer"]["pane"], request_id="xws-review-twin",
                             author_kind="agent_session", source="#wt-one @twin Reviewed.")
                    wake("beta-reviewer")
                    recovered = deliveries("n-recovered", ["alpha-helper", "wt-one-twin"])
                    answered = recovered["alpha-helper"]
                    require(answered["untrusted_body"] == "Reviewed."
                            and answered["control"]["thread_id"] == asked_review["thread_id"],
                            "n: alpha's helper hears the reply on the thread it started",
                            {"body": answered["untrusted_body"], "thread": answered["control"]["thread_id"],
                             "started": asked_review["thread_id"]})
                    require(recovered["wt-one-twin"]["untrusted_body"] == "Reviewed.",
                            "n: wt-one's twin receives the send", recovered["wt-one-twin"]["untrusted_body"])
                    settled_as = disposition(review["control"]["attempt_id"])
                    require(settled_as == "reply", "n: the original attempt settles as a reply", settled_as)
                    for envelope in recovered.values():
                        bound(envelope, "complete.no-reply")

                    named = cli("address", "list", "--json")["data"]
                    lead_address = next(a for a in named if a["alias"] == "lead")
                    removed = trace["e_remove"] = body("address", "remove", address_id=lead_address["id"])
                    remaining = sorted(a["alias"] for a in cli("address", "list", "--json")["data"])
                    require(removed["removed"] is True and "lead" not in remaining,
                            "e: alpha's lead name is removed", {"removed": removed, "remaining": remaining})
                    before = trace["e_state_before"] = message_state()
                    picked = trace["e"] = hook("alpha-helper", "@lead who should take this?")
                    context_text = ((picked["output"] or {}).get("hookSpecificOutput") or {}).get("additionalContext", "")
                    require(context_text.startswith("Namedrop workspace roster."),
                            "e: the hook answers with roster-marked additionalContext", picked["stdout"])
                    require("@lead is not named in this workspace. It is named in these workspaces: #beta-space, #gamma."
                            in context_text and "Nothing was sent" in context_text,
                            "e: the picker lists Beta Space and gamma and says nothing was sent", context_text)
                    wake("alpha-helper")
                    deliveries("e", [])
                    require(message_state() == before, "e: the picker persists nothing", message_state())
                    for runtime, wrapper in wrappers.items():
                        wrapped = trace[f"h_picker_{runtime}"] = hook("alpha-helper", "@lead who should take this?", [wrapper])
                        require(wrapped["exit"] == 0 and wrapped["stdout"] == picked["stdout"],
                                f"h: the installed {runtime} wrapper passes the picker through unchanged", wrapped["stdout"])
                    deliveries("h-picker", [])
                    require(message_state() == before, "h: the wrapped pickers persist nothing", message_state())
                    summary["outcome"] = "passed"
                finally:
                    if summary.get("outcome") != "passed":
                        backup_database()
                    save("cross-workspace.json", trace)
                    for role, agent in agents.items():
                        save(f"{role}-envelopes.json", read_envelopes(agent["envelopes"]))
        backup_database()
        success = True
    finally:
        if dashboard_pid is not None and dashboard_alive():
            os.kill(dashboard_pid, 15)
            deadline = time.monotonic() + 5
            while dashboard_alive() and time.monotonic() < deadline:
                time.sleep(0.1)
        if server is not None:
            if server.poll() is None:
                server.terminate()
            try:
                server.wait(timeout=10)
            except subprocess.TimeoutExpired:
                server.kill()
                server.wait(timeout=5)
        log.close()
        def live_shells():
            live_pids = []
            for pid in shell_pids:
                try:
                    os.kill(pid, 0)
                    live_pids.append(pid)
                except ProcessLookupError:
                    pass
            return live_pids

        def owned_processes():
            found = []
            for entry in Path("/proc").iterdir():
                if not entry.name.isdigit() or int(entry.name) == os.getpid():
                    continue
                try:
                    exe = os.readlink(entry / "exe")
                except OSError:
                    exe = ""
                try:
                    argv = (entry / "cmdline").read_bytes().decode(errors="replace").split("\0")
                except OSError:
                    continue
                if str(base) in exe or any(str(base) in argument for argument in argv):
                    found.append({"pid": int(entry.name), "exe": exe, "argv": [a for a in argv if a]})
            return found

        remaining = list((base / "run").glob("*.sock"))
        shells = live_shells()
        if server is not None:
            deadline = time.monotonic() + 5
            while (remaining or shells) and time.monotonic() < deadline:
                time.sleep(0.1)
                remaining = list((base / "run").glob("*.sock"))
                shells = live_shells()
        dashboard_left = dashboard_pid is not None and dashboard_alive()
        owned = owned_processes()
        after_server_exit = owned
        deadline = time.monotonic() + 15
        while owned and time.monotonic() < deadline:
            time.sleep(0.5)
            owned = owned_processes()
        terminated = [process["pid"] for process in owned]
        for pid in terminated:
            try:
                os.kill(pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
        deadline = time.monotonic() + 5
        while owned and time.monotonic() < deadline:
            time.sleep(0.5)
            owned = owned_processes()
        clean = (server is None or server.poll() is not None) and not (remaining or shells or owned or terminated or dashboard_left)
        save("cleanup.json", {"verified": clean, "server_pid": server.pid if server else None,
                              "server_exit": server.returncode if server else None,
                              "dashboard_pid": dashboard_pid, "dashboard_alive": dashboard_left,
                              "owned_shell_pids": shell_pids, "live_shell_pids": shells,
                              "remaining_sockets": [str(p) for p in remaining],
                              "owned_processes_after_server_exit": after_server_exit,
                              "terminated_owned_pids": terminated, "live_owned_processes": owned})
        summary.update(passed=success and clean, cleanup_verified=clean)
        save("summary.json", summary)
        if clean:
            shutil.rmtree(base)
        else:
            print(f"Fixture retained for cleanup: {base}")
        assert (evidence / "commands.jsonl").exists() and (evidence / "cleanup.json").exists()
        print(f"Evidence: {evidence}")
        if not clean:
            raise RuntimeError("Owned server cleanup incomplete; see cleanup.json")


if __name__ == "__main__":
    main()
