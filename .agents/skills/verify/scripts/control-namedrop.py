#!/usr/bin/env python3
"""Drive public Namedrop commands in disposable native Herdr terminal panes."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shlex
import shutil
import sqlite3
import subprocess
import tempfile
import time


REPO = Path(__file__).resolve().parents[4]


def wait(predicate, label, timeout=30):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = predicate()
        if value:
            return value
        time.sleep(0.1)
    raise RuntimeError(f"Timed out waiting for {label}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--feature", choices=["names", "groups", "controls", "tour", "handoffs", "shared-checkout"], default="names")
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
    state = base / "state/namedrop"
    server = None
    shell_pids = []
    success = False
    sequence = 0
    summary = {"feature": args.feature, "fixture": str(base), "binary": str(binary),
               "sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
               "commit": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=REPO, text=True).strip(),
               "limits": "No model sessions; proves CLI and native terminal behavior only."}

    def save(name, value):
        (evidence / name).write_text(json.dumps(value, indent=2) + "\n")

    def record(row):
        with (evidence / "commands.jsonl").open("a") as log:
            log.write(json.dumps(row) + "\n")

    def command(*argv, check=True):
        result = subprocess.run([str(a) for a in argv], env=env, cwd=base / "workspace",
                                capture_output=True, text=True, timeout=30)
        record({"kind": "host", "argv": [str(a) for a in argv], "exit": result.returncode,
                "stdout": result.stdout, "stderr": result.stderr})
        if check and result.returncode:
            raise RuntimeError(result.stderr or result.stdout)
        return result

    def host(*argv):
        result = command(herdr, *argv)
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
        created = host("workspace", "create", "--cwd", str(base / "workspace"), "--label", "Namedrop verification")
        spaces = {"lead": created["workspace"]["workspace_id"]}
        panes = {"lead": created["root_pane"]["pane_id"]}
        panes["worker"] = host("tab", "create", "--workspace", spaces["lead"], "--cwd", str(base / "workspace"),
                               "--label", "Verification worker")["root_pane"]["pane_id"]
        for pane in panes.values():
            wait(lambda: (base / "home" / f"ready-{pane}").exists(), "native bash prompt")
            shell_pids.append(host("pane", "process-info", "--pane", pane)["process_info"]["shell_pid"])
        save("instance.json", {"server_pid": server.pid, "socket": env["HERDR_SOCKET_PATH"],
                              "state": str(state), "workspace": spaces["lead"], "panes": panes})
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
        elif args.feature == "shared-checkout":
            twin = host("workspace", "create", "--cwd", str(base / "workspace"), "--label", "Namedrop verification twin")
            spaces["twin"] = twin["workspace"]["workspace_id"]
            panes["twin"] = twin["root_pane"]["pane_id"]
            wait(lambda: (base / "home" / f"ready-{panes['twin']}").exists(), "twin native bash prompt")
            shell_pids.append(host("pane", "process-info", "--pane", panes["twin"])["process_info"]["shell_pid"])
            listing = host("worktree", "list", "--workspace", spaces["twin"], "--json")
            checkout = next(w for w in listing["worktrees"] if w["path"] == str(base / "workspace"))
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
            save("shared-checkout.json", {"worktrees": listing, "activate": {"lead": activated, "twin": twin_activated},
                                          "rosters": rosters, "doctor": reports})
        with sqlite3.connect(state.joinpath("namedrop.sqlite3").as_uri() + "?mode=ro", uri=True) as source:
            with sqlite3.connect(evidence / "state.sqlite3") as destination:
                source.backup(destination)
        success = True
    finally:
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

        remaining = list((base / "run").glob("*.sock"))
        shells = live_shells()
        if server is not None:
            deadline = time.monotonic() + 5
            while (remaining or shells) and time.monotonic() < deadline:
                time.sleep(0.1)
                remaining = list((base / "run").glob("*.sock"))
                shells = live_shells()
        clean = (server is None or server.poll() is not None) and not (remaining or shells)
        save("cleanup.json", {"verified": clean, "server_pid": server.pid if server else None,
                              "server_exit": server.returncode if server else None,
                              "owned_shell_pids": shell_pids, "live_shell_pids": shells,
                              "remaining_sockets": [str(p) for p in remaining]})
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
