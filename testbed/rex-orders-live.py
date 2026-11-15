#!/usr/bin/env python3
"""UI.md's U13 on the testbed: Namedrop's two install orders, `install --remove`
after each, and the relay between two Rex lanes, on real Herdr, real Rex and
real agents.

usage: python3 -I testbed/rex-orders-live.py --link CHECKOUT --rex REX --out DIR [--keep] SCENARIO...
       python3 -I testbed/rex-orders-live.py --out DIR down

Scenarios, each in its own rex-u13-* profiles, made fresh:
  herdr-first     A profile Herdr installs the build from its release archive. The archive's
                  install.sh then adopts Herdr's runtime for a scratch Rex server in the same
                  HOME, and install --remove takes the Rex half away again.
  old-herdr       A profile Herdr runs Namedrop 1.11.1, from before step 6. install.sh refuses and
                  changes nothing. Herdr then updates to the build's archive, install.sh adopts,
                  and install --remove takes the Rex half away.
  rex-first       The Rex lane installs with install.sh, Claude Code and Codex bind in its blocks
                  and are named, and a profile Herdr in the same HOME then installs the archive,
                  which hands off. The old state's bridge stops, the module follows, the
                  standalone runtime refuses, Doctor's ND-INSTALL-001 counts and names the agents
                  before and after every Namedrop process is stopped, Codex's next turn starts no
                  helper of the old state, the warning is gone after a Rex server restart, and
                  install --remove takes the Rex half away.
  stopped-server  The Rex lane installs, its server stops, Herdr hands off and exits 0, and the
                  server's next start loads the module baked to Herdr's launcher. Then install
                  --remove.
  two-lanes       Lanes A and B install with install.sh only. B's server restarts with no session,
                  and B's bridge runs before any block does. A Codex then starts on B, is
                  activated and named from Rex's palette actions, a terminal on A mentions it
                  through A's `rex hosts` entry for B, which a pass-through forwarder serves, and
                  the reply comes back. Then install --remove on B, a standalone owner.

Every rex call names the lane's server with -S and --autostart=false, from the lane's own
environment. Claude Code runs only as the profile's token wrapper, started by the testbed's Rex
lane. Each scenario logs every command to OUT/<scenario>/commands.log, writes
OUT/<scenario>/evidence.json, prints one line per check, and brings its profiles down at the end
unless --keep. `down` brings every rex-u13-* profile down and keeps its files.
"""

import argparse
import contextlib
import datetime
import fcntl
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shlex
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import threading
import time

sys.dont_write_bytecode = True
KIT = Path(__file__).resolve().parent
sys.path.insert(0, str(KIT / "lib"))
import rex_live  # noqa: E402
import testbed  # noqa: E402

CONFIRMED = '{"confirmed":true}'
LINE = 'pcall(require, "namedrop");'
OLD_RELEASE = "v1.11.1"
PROFILES = {"herdr-first": ("rex-u13-h1",), "old-herdr": ("rex-u13-h0",), "rex-first": ("rex-u13-r1",),
            "stopped-server": ("rex-u13-r2",), "two-lanes": ("rex-u13-a", "rex-u13-b")}
REFUSED_AFTER_HANDOFF = ("ND-INSTALL-001: Namedrop on Rex moved to Herdr's install in {}. In each Rex session, "
                         "run Activate Namedrop again and name the panes again. Restart an agent there that shows "
                         "Hook failed or does not answer a mention.")
REFUSED_AFTER_REMOVAL = "Namedrop was removed from Rex. Run sh scripts/install.sh to install it again."
HELPER_WORDS = {("rex", "bridge"): "bridge", ("relay", "worker"): "relay worker",
                ("hook", "agent-status-worker"): "status worker", ("dashboard-server",): "dashboard server"}


def load_runtime():
    """scripts/runtime.py of this kit's checkout, for its process reads."""
    spec = importlib.util.spec_from_file_location("runtime_reads", KIT.parent / "scripts/runtime.py")
    module = importlib.util.module_from_spec(spec)
    sys.path.insert(0, str(KIT.parent / "scripts"))
    spec.loader.exec_module(module)
    return module


runtime = load_runtime()


class Failed(Exception):
    pass


class Run:
    """One scenario's command log, checks and observations."""

    def __init__(self, out, scenario):
        self.dir = out / scenario
        self.dir.mkdir(parents=True, exist_ok=True)
        self.log = (self.dir / "commands.log").open("a")
        self.checks, self.observed = [], {}
        self.note("scenario " + scenario)

    def note(self, text):
        self.log.write("[{}] {}\n".format(datetime.datetime.now().strftime("%H:%M:%S.%f")[:-3], text))
        self.log.flush()

    def run(self, environment, command, cwd=None, timeout=600, ok=True):
        started = time.monotonic()
        result = subprocess.run(testbed.env_command(environment, [str(word) for word in command]),
                                cwd=str(cwd) if cwd else None, text=True, stdin=subprocess.DEVNULL,
                                capture_output=True, timeout=timeout)
        self.note("$ {}\n[exit {} in {:.2f}s]\n{}{}".format(
            shlex.join(str(word) for word in command), result.returncode, time.monotonic() - started,
            result.stdout, result.stderr))
        if ok and result.returncode != 0:
            raise Failed("{} exited {}: {}{}".format(shlex.join(str(w) for w in command), result.returncode,
                                                     result.stdout[-2000:], result.stderr[-2000:]))
        return result

    def check(self, what, passed, detail=None):
        self.checks.append({"check": what, "ok": bool(passed), "detail": detail})
        self.note(("ok   " if passed else "FAIL ") + what + ("" if detail is None else ": " + json.dumps(detail, default=str)))
        print(("ok   " if passed else "FAIL ") + what + ("" if passed or detail is None else
                                                          ": " + json.dumps(detail, default=str)[:1500]), flush=True)
        self.save()
        if not passed:
            raise Failed(what)

    def observe(self, key, value):
        self.observed[key] = value
        self.note("observed {}: {}".format(key, json.dumps(value, default=str)))
        self.save()

    def save(self):
        (self.dir / "evidence.json").write_text(json.dumps(
            {"checks": self.checks, "observed": self.observed}, indent=2, default=str) + "\n")


def wait(read, accepts, what, within=30, step=0.2):
    until = time.monotonic() + within
    value = None
    while time.monotonic() < until:
        value = read()
        if accepts(value):
            return value
        time.sleep(step)
    raise Failed("{} within {}s: {!r}".format(what, within, value))


class Both(testbed.Profile):
    """A Rex lane profile that also runs a profile Herdr. Herdr's commands run in the profile's
    Herdr environment, and the Rex lane takes its own from it, so both hosts share one HOME."""

    @property
    def env(self):
        if "toolchain" not in self.state:
            testbed.fail("profile {} never finished setup".format(self.name))
        return testbed.clean_environment(self.dir, self.socket, self.state["toolchain"])


def fresh(kind, name, rex):
    profile = kind(name)
    if profile.dir.exists():
        profile.owned()
        down(profile)
        shutil.rmtree(str(profile.dir))
        profile = kind(name)
    profile.create()
    profile.state.update(purpose="rex", rex_bin=str(Path(rex).resolve()))
    profile.save()
    return profile


def herdr_state(profile):
    return profile.dir / "state/herdr/plugins/namedrop"


def lane_of(profile):
    return rex_live.Lane(profile, testbed.fail, testbed.say)


def rx(run, lane, *args, ok=True, timeout=60):
    return run.run(lane.environment(), [lane.binary(), "-S", lane.server, "--autostart=false", *args],
                   cwd=lane.profile.dir, timeout=timeout, ok=ok)


def nd(run, lane, launcher, *args, ok=True, environment=None, timeout=120):
    """A Namedrop command in the lane's environment, from outside any block."""
    return run.run(environment or lane.environment(), [launcher, *args], cwd=lane.profile.repo, timeout=timeout,
                   ok=ok)


def at(lane, session, block):
    return ["--endpoint", "rex:" + str(lane.socket), "--space", session, "--pane", block]


def about(run, lane):
    reply = json.loads(rx(run, lane, "do", "namedrop_about", "--json").stdout)
    return reply.get("result", reply) if isinstance(reply, dict) and "module" not in reply else reply


def actions(run, lane):
    listed = json.loads(rx(run, lane, "actions", "--json").stdout)
    items = listed if isinstance(listed, list) else listed.get("actions", listed)
    return sorted(item["name"] for item in items if item["name"].startswith("namedrop_"))


def registration(profile):
    path = profile.dir / "home/.config/namedrop/rex.json"
    return json.loads(path.read_text()) if path.exists() else None


def record(profile):
    path = profile.dir / "home/.config/namedrop/state-directory"
    return path.read_text().strip() if path.exists() else None


def shared(profile):
    """The shared slot's files, by path and digest: the packages, the permissions, Codex's
    settings, the shell command and the install record."""
    home = profile.dir / "home"
    roots = [home / ".agents/skills/namedrop", home / ".claude/skills/namedrop", home / ".claude/settings.json",
             profile.dir / "codex/config.toml", home / ".local/bin/namedrop", home / ".config/namedrop/state-directory"]
    return {str(path): hashlib.sha256(path.read_bytes()).hexdigest() for root in roots
            for path in (sorted(root.rglob("*")) if root.is_dir() else [root]) if path.is_file()}


def releases(state):
    directory = state / "runtime/releases"
    return sorted(path.name for path in directory.iterdir()) if directory.is_dir() else []


def helper_kind(argv, environment, state):
    """Which helper of `state` a process is, read as installed_runtime::helpers reads it."""
    rest = argv[1:]
    same = lambda path: path is not None and os.path.realpath(path) == os.path.realpath(state)
    if len(rest) == 3 and rest[0] in ("reminder-clock", "--deferred-worker") and same(rest[1]):
        return {"reminder-clock": "reminder clock", "--deferred-worker": "deferred worker"}[rest[0]]
    words, explicit, index = [], None, 0
    while index < len(rest):
        if rest[index] == "--plugin-state":
            explicit, index = rest[index + 1] if index + 1 < len(rest) else None, index + 2
        elif rest[index].startswith("--plugin-state="):
            explicit, index = rest[index].split("=", 1)[1], index + 1
        elif rest[index] in ("--endpoint", "--host", "--ui"):
            index += 2
        elif rest[index] == "--inherit-claim":
            index += 1
        else:
            words.append(rest[index])
            index += 1
    named = explicit or environment.get("NAMEDROP_STATE_DIR") or environment.get("HERDR_PLUGIN_STATE_DIR")
    return HELPER_WORDS.get(tuple(words)) if same(named) else None


def namedrop_processes(root):
    """{pid: (executable, argv, environment)} of every Namedrop binary run from a runtime under
    `root`."""
    found = {}
    for pid in runtime.all_pids():
        if pid == os.getpid():
            continue
        row = runtime.process(pid)
        if row and "/runtime/releases/" in os.path.realpath(row[0]) and \
                os.path.realpath(row[0]).startswith(os.path.realpath(root) + "/"):
            found[pid] = row
    return found


def helpers(state):
    """[(pid, kind, argv)] of every running helper of `state`."""
    rows = []
    for pid, (executable, argv, environment) in sorted(namedrop_processes(state).items()):
        kind = helper_kind(argv, environment, state)
        if kind:
            rows.append((pid, kind, argv[1:]))
    return rows


def bridges(state, lane):
    return [row for row in helpers(state)
            if row[1] == "bridge" and "rex:" + str(lane.socket) in row[2]]


def lock_holders(path):
    """The pids that have this lock file open, as lsof reads them."""
    shown = subprocess.run(["/usr/sbin/lsof", "-t", "--", str(path)], capture_output=True, text=True)
    return sorted(int(pid) for pid in shown.stdout.split())


def held(path):
    """True when another process holds an exclusive or shared flock on this file. The probe holds
    the lock only for the instant it takes, and only when no one else does."""
    with open(path) as handle:
        try:
            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError:
            return True
        fcntl.flock(handle, fcntl.LOCK_UN)
        return False


def stop_namedrop(root):
    """Stops every Namedrop process run from a runtime under `root`, by pid, and nothing else,
    and returns what it stopped."""
    stopped = []
    for attempt in range(40):
        rows = namedrop_processes(root)
        if not rows:
            return stopped
        for pid, (executable, argv, environment) in rows.items():
            if attempt == 0:
                stopped.append([pid] + argv[1:])
            with contextlib.suppress(ProcessLookupError):
                os.kill(pid, signal.SIGTERM if attempt < 20 else signal.SIGKILL)
        time.sleep(0.25)
    raise Failed("Namedrop processes still run under {}: {}".format(root, sorted(namedrop_processes(root))))


class Watch(threading.Thread):
    """Records every process whose command line names one of `needles`, polled every 30 ms."""

    def __init__(self, needles):
        super().__init__(daemon=True)
        self.needles, self.seen, self.done = needles, {}, threading.Event()

    def run(self):
        while not self.done.is_set():
            shown = subprocess.run(["/bin/ps", "-axww", "-o", "pid=,ppid=,command="], capture_output=True,
                                   text=True).stdout
            for line in shown.splitlines():
                parts = line.split(None, 2)
                if len(parts) == 3 and any(needle in parts[2] for needle in self.needles):
                    self.seen.setdefault((int(parts[0]), parts[2]), (int(parts[1]), time.time()))
            time.sleep(0.03)

    def stop(self):
        self.done.set()
        self.join(5)
        return [{"pid": pid, "ppid": ppid, "at": round(seen, 3), "command": command}
                for (pid, command), (ppid, seen) in sorted(self.seen.items(), key=lambda item: item[1][1])]


def watched_kind(command):
    for words, kind in (((" rex bridge ",), "bridge"), ((" reminder-clock ",), "reminder clock"),
                        ((" --deferred-worker ",), "deferred worker"), ((" relay worker",), "relay worker"),
                        ((" hook agent-status-worker",), "status worker"), ((" dashboard-server",), "dashboard server")):
        if any(word in command for word in words):
            return kind
    return None


def hook_runs(state):
    directory = state / "hook-runs"
    return {path.name: [path.read_text(), path.stat().st_mtime] for path in sorted(directory.iterdir())} \
        if directory.is_dir() else {}


# Herdr's side.

def herdr_up(run, profile):
    """Starts the profile Herdr with a workspace on the profile's repo, and returns its first pane."""
    profile.start_server()
    profile.seed()
    profile.workspace()
    panes = profile.herdr("pane", "list", "--workspace", profile.state["workspace"])["panes"]
    run.observe("herdr workspace", {"workspace": profile.state["workspace"], "pane": panes[0]["pane_id"],
                                    "socket": str(profile.socket)})
    return panes[0]["pane_id"]


def herdr_setup(run, profile, pane, root, label, timeout=900):
    """Runs `sh ROOT/scripts/setup.sh install --json` in a Herdr pane, as a person installing or
    updating Namedrop in Herdr does, and returns its exit code, report and stderr."""
    report, errors = profile.dir / "logs" / (label + ".json"), profile.dir / "logs" / (label + ".err")
    command = "sh {} install --json > {} 2> {}".format(shlex.quote(str(root / "scripts/setup.sh")),
                                                       shlex.quote(str(report)), shlex.quote(str(errors)))
    started = time.monotonic()
    code, _ = profile.in_pane(pane, command, label, timeout=timeout)
    text, stderr = report.read_text() if report.exists() else "", errors.read_text() if errors.exists() else ""
    run.note("herdr pane {}: {}\n[exit {} in {:.1f}s]\n{}\n{}".format(pane, command, code, time.monotonic() - started,
                                                                      text, stderr))
    try:
        payload = json.loads(text)
    except ValueError:
        payload = None
    return code, payload, stderr


def herdr_doctor(run, profile, drop=()):
    """Herdr's Doctor from Herdr's launcher, in a fresh process, and its ND-INSTALL-001 issues."""
    state = herdr_state(profile)
    environment = {key: value for key, value in profile.env.items() if key not in drop}
    result = run.run(environment, [state / "runtime/namedrop", "--plugin-state", state, "--herdr-workspace",
                                   profile.state["workspace"], "--json", "doctor"], cwd=profile.repo, ok=False)
    data = json.loads(result.stdout)["data"]
    return [issue for issue in data["issues"] if issue["code"] == "ND-INSTALL-001"], data


# The Rex lane's side.

def install_sh(run, lane, root, ok=True):
    """Runs the archive's install.sh --yes in the lane's environment with the lane's rex first on
    PATH, as the Rex lane does."""
    environment = dict(lane.environment())
    environment["PATH"] = os.pathsep.join([str(Path(lane.binary()).parent), environment["PATH"]])
    return run.run(environment, ["sh", root / "scripts/install.sh", "--yes"], cwd=lane.profile.dir, ok=ok,
                   timeout=900)


def remove(run, lane, launcher, state):
    result = nd(run, lane, launcher, "--plugin-state", state, "--json", "install", "--remove",
                "--input-json", CONFIRMED)
    return json.loads(result.stdout)["data"]


def server_stop(run, lane):
    """`rex server stop`, with the sessions left for the next start to restore."""
    rx(run, lane, "server", "stop", timeout=60)
    wait(lambda: lane.socket.exists(), lambda exists: not exists, "the Rex socket to go", 20)


def module_checks(run, lane, state, launcher, what):
    """The server loaded the module baked to `launcher` and `state`, and lists eleven actions."""
    shown = about(run, lane)
    run.check(what + ": namedrop_about names the launcher and state",
              shown.get("launcher") == str(launcher) and shown.get("state") == str(state), shown)
    names = actions(run, lane)
    run.check(what + ": rex actions lists eleven namedrop_ actions", len(names) == 11, names)
    return shown


def rex_half_checks(run, profile, lane, config_before, what):
    """install --remove left init.lua byte for byte, no module, no rex.json, and no action."""
    config = lane.dir / "config"
    run.check(what + ": init.lua is back byte for byte", (config / "init.lua").read_bytes() == config_before,
              (config / "init.lua").read_bytes().decode(errors="replace"))
    run.check(what + ": namedrop.lua and rex.json are gone",
              not (config / "namedrop.lua").exists() and registration(profile) is None,
              [str(path) for path in (config / "namedrop.lua", profile.dir / "home/.config/namedrop/rex.json")
               if path.exists()])
    run.check(what + ": the server lists no namedrop_ action", actions(run, lane) == [], actions(run, lane))


def build(run, lane):
    started = time.monotonic()
    root = rex_live.build_archive(lane)
    run.observe("archive", {"root": str(root), "seconds": round(time.monotonic() - started)})
    return root


def start_rex_only(run, profile, request, agents):
    """The Rex lane's install, with no herdr on PATH, as `testbed rex up` runs it."""
    lane = lane_of(profile)
    rex_live.install(lane, request, agents)
    rex_live.write_login_filters(profile)
    run.observe("login shells", rex_live.login_shells_find_no_herdr(lane.environment(), profile.dir))
    return lane


def add_herdr(profile, agents):
    """Links the host's herdr into the profile, so Herdr and Rex share one HOME."""
    profile.prepare(agents, herdr=True)


# Scenarios.

def herdr_installs_then_rex_adopts(run, profile, lane, root, what):
    """Herdr runs the build. install.sh adopts its runtime, and install --remove takes the Rex
    half away again."""
    state = herdr_state(profile)
    launcher = state / "runtime/namedrop"
    run.check(what + ": the install record names Herdr's state", record(profile) == str(state), record(profile))
    lane.start_server()
    config_before = (lane.dir / "config/init.lua").read_bytes()
    before = releases(state)
    result = install_sh(run, lane, root)
    run.check(what + ": install.sh adopts Herdr's state",
              "Namedrop Rex install: adopt\nState directory: {}\n".format(state) in result.stdout and
              "Herdr keeps its runtime and updates it. This archive publishes no runtime." in result.stdout,
              result.stdout)
    standalone = rex_live.state_directory(profile)
    run.check(what + ": one runtime: no standalone state, and Herdr's releases are unchanged",
              not standalone.exists() and releases(state) == before,
              {"standalone": standalone.exists(), "releases": releases(state), "before": before})
    entry = registration(profile)
    run.check(what + ": rex.json bakes Herdr's launcher and state",
              entry["launcher"] == str(launcher) and entry["state"] == str(state) and entry["line"] == "inserted" and
              entry["server"] == "rex:" + str(lane.socket), entry)
    module = (lane.dir / "config/namedrop.lua").read_text()
    run.check(what + ": namedrop.lua names Herdr's launcher",
              'local NAMEDROP = "{}"'.format(launcher) in module and 'local STATE = "{}"'.format(state) in module,
              module.splitlines()[:5])
    module_checks(run, lane, state, launcher, what)
    found = wait(lambda: bridges(state, lane), bool, "a bridge of Herdr's state for the Rex server", 20)
    run.check(what + ": Herdr's runtime runs the Rex server's bridge", bool(found), found)
    issues, _ = herdr_doctor(run, profile)
    run.check(what + ": Herdr's Doctor reports no ND-INSTALL-001", not issues, issues)
    kept = shared(profile)
    removed = remove(run, lane, launcher, state)
    run.observe(what + " removal", removed)
    run.check(what + ": install --remove removes only the Rex half, and says so",
              removed.get("removed") is True and removed.get("packages_removed") is False and
              "Removed only the Rex configuration" in (removed.get("message") or ""), removed)
    rex_half_checks(run, profile, lane, config_before, what)
    run.check(what + ": Herdr's packages and record stay as they were", shared(profile) == kept,
              sorted(set(shared(profile).items()) ^ set(kept.items())))
    run.check(what + ": Herdr's launcher still runs", nd(run, lane, launcher, "--version").returncode == 0)


def scenario_herdr_first(run, args, request):
    profile = fresh(Both, PROFILES["herdr-first"][0], args.rex)
    agents = ()
    profile.prepare(agents, herdr=True)
    profile.state["source"] = profile.resolve(request)
    profile.save()
    testbed.ensure_codex_settings(profile.dir / "codex/config.toml")
    profile.seed()
    lane = lane_of(profile)
    root = build(run, lane)
    pane = herdr_up(run, profile)
    code, report, stderr = herdr_setup(run, profile, pane, root, "herdr-setup")
    run.check("Herdr installs the build from its archive", code == 0 and report and report.get("status") == "ok",
              {"code": code, "stderr": stderr[-2000:]})
    herdr_installs_then_rex_adopts(run, profile, lane, root, "herdr-first")


def scenario_old_herdr(run, args, request):
    profile = fresh(Both, PROFILES["old-herdr"][0], args.rex)
    agents = ()
    profile.prepare(agents, herdr=True)
    profile.state["source"] = profile.resolve(request)
    profile.save()
    testbed.ensure_codex_settings(profile.dir / "codex/config.toml")
    profile.seed()
    lane = lane_of(profile)
    root = build(run, lane)
    old = profile.dir / "namedrop-old"
    run.run(profile.env, ["git", "clone", "--quiet", "--no-local", "--no-hardlinks", request["path"], old])
    run.run(profile.env, ["git", "-c", "advice.detachedHead=false", "checkout", "--quiet", OLD_RELEASE], cwd=old)
    pane = herdr_up(run, profile)
    code, report, stderr = herdr_setup(run, profile, pane, old, "herdr-setup-old", timeout=1200)
    state = herdr_state(profile)
    run.check("Herdr installs Namedrop " + OLD_RELEASE, code == 0 and report and report.get("status") == "ok" and
              (state / "runtime/current").exists() and not (state / "runtime/owner").exists(),
              {"code": code, "stderr": stderr[-2000:]})
    run.check(OLD_RELEASE + " writes no install record", record(profile) is None, record(profile))
    lane.start_server()
    config_before = (lane.dir / "config/init.lua").read_bytes()
    home_before = {str(path): path.read_bytes() for path in (profile.dir / "home").rglob("*") if path.is_file()}
    refused = install_sh(run, lane, root, ok=False)
    expected = ("Namedrop is already installed in Herdr ({}). Update Namedrop in Herdr first, then run sh "
                "scripts/install.sh again.").format(state)
    run.check("install.sh refuses beside " + OLD_RELEASE + " with the update message",
              refused.returncode != 0 and refused.stderr.strip().endswith(expected),
              {"code": refused.returncode, "stdout": refused.stdout, "stderr": refused.stderr})
    home_after = {str(path): path.read_bytes() for path in (profile.dir / "home").rglob("*") if path.is_file()}
    run.check("the refused install.sh changed nothing",
              home_after == home_before and (lane.dir / "config/init.lua").read_bytes() == config_before and
              not rex_live.state_directory(profile).exists() and actions(run, lane) == [],
              sorted(set(home_after) ^ set(home_before)))
    code, report, stderr = herdr_setup(run, profile, pane, root, "herdr-setup-update")
    run.check("Herdr updates from {} to the build's archive".format(OLD_RELEASE),
              code == 0 and report and report.get("status") == "ok", {"code": code, "stderr": stderr[-2000:]})
    herdr_installs_then_rex_adopts(run, profile, lane, root, "old-herdr")


def handoff_reported(handoff, old, status):
    """The activation report names one handoff from `old`, with the notice that the names stay
    there, and the module's confirmation."""
    return (isinstance(handoff, list) and len(handoff) == 1 and handoff[0].get("state") == str(old) and
            "The names made on Rex stay in {} and were not carried over.".format(old) in
            (handoff[0].get("notice") or "") and (handoff[0].get("module") or {}).get("status") == status and
            (handoff[0].get("module") or {}).get("sealed") is True)


def name_in_rex(run, lane, launcher, session, block, alias):
    result = nd(run, lane, launcher, *at(lane, session, block), "--json", "name", "@" + alias)
    return json.loads(result.stdout)["data"]


def bindings(state):
    directory = state / "rex/sessions"
    return sorted((json.loads(path.read_text()) for path in directory.glob("*.json")),
                  key=lambda b: b.get("block", "")) if directory.is_dir() else []


def rollouts(profile):
    """{rollout: task_complete count} of every Codex session file of the profile."""
    counts = {}
    for path in (profile.dir / "codex/sessions").rglob("*.jsonl"):
        counts[str(path)] = path.read_text(errors="replace").count('"type":"task_complete"')
    return counts


def codex_turn(run, lane, session, block, prompt, within=300):
    """Types a prompt into Codex's block and waits for the turn to complete in its rollout."""
    before = rollouts(lane.profile)
    rx(run, lane, "send", "-s", session, "-b", block, prompt)
    rx(run, lane, "send-key", "-s", session, "-b", block, "enter")
    started = time.time()
    changed = wait(lambda: {path: count for path, count in rollouts(lane.profile).items()
                            if count > before.get(path, 0)}, bool, "Codex to finish its turn", within, 1)
    return started, changed


def scenario_rex_first(run, args, request):
    profile = fresh(Both, PROFILES["rex-first"][0], args.rex)
    agents = ("claude", "codex")
    profile.state.update(agents=list(agents))
    profile.save()
    lane, session, blocks = rex_live.up(profile, request, agents)
    old = rex_live.state_directory(profile)
    state = herdr_state(profile)
    root = Path(profile.state["archive_root"])
    run.observe("rex session", {"session": session, "blocks": blocks, "socket": str(lane.socket)})
    config_before = b""  # the lane's server starts on an empty init.lua
    for alias in agents:
        named = name_in_rex(run, lane, old / "runtime/namedrop", session, blocks[alias], alias)
        run.observe("named @" + alias, named)
    bound = bindings(old)
    run.check("Claude Code and Codex are bound in the standalone state",
              sorted(b["block"] for b in bound) == sorted(blocks[a] for a in agents), bound)
    old_helpers = wait(lambda: helpers(old), lambda rows: any(row[1] == "bridge" for row in rows),
                       "the standalone state's bridge", 20)
    run.observe("standalone helpers before the handoff", old_helpers)
    cache = sorted(str(path) for path in (profile.dir / "codex/plugins/cache/namedrop/namedrop").glob("*"))
    run.observe("Codex plugin cache before the handoff", cache)
    old_runs = hook_runs(old)
    run.observe("standalone hook runs before the handoff", old_runs)

    add_herdr(profile, agents)
    pane = herdr_up(run, profile)
    code, report, stderr = herdr_setup(run, profile, pane, root, "herdr-handoff")
    handoff = (report or {}).get("data", {}).get("handoff")
    run.observe("handoff activation report", handoff)
    run.check("Herdr's activation hands off and exits 0", code == 0 and report and report.get("status") == "ok",
              {"code": code, "stderr": stderr[-3000:]})
    run.check("the activation report carries the names notice and the confirmed module",
              handoff_reported(handoff, old, "confirmed"), handoff)
    marker = json.loads((old / "runtime/retired.json").read_text())
    run.check("the standalone state is retired toward Herdr's",
              marker.get("reason") == "herdr_handoff" and marker.get("successor") == str(state), marker)
    left = helpers(old)
    run.check("the old state's bridge and every other helper stopped",
              not left and all(not os_alive(row[0]) for row in old_helpers), {"left": left, "before": old_helpers})
    entry = registration(profile)
    predecessors = entry.get("predecessors") or []
    run.check("rex.json bakes Herdr's launcher and keeps one sealed entry with both agents",
              entry["launcher"] == str(state / "runtime/namedrop") and entry["state"] == str(state) and
              len(predecessors) == 1 and predecessors[0]["state"] == str(old) and predecessors[0]["sealed"] is True
              and sorted(a["block"] for a in predecessors[0]["agents"]) == sorted(blocks[a] for a in agents), entry)
    module_checks(run, lane, state, state / "runtime/namedrop", "after the handoff")
    found = wait(lambda: bridges(state, lane), bool, "a bridge of Herdr's state for the Rex server", 20)
    run.check("the reloaded module started Herdr's bridge for the Rex server", bool(found), found)
    # The old package's commands name the old state with --plugin-state, and its module's actions
    # with NAMEDROP_STATE_DIR.
    named = dict(lane.environment(), NAMEDROP_STATE_DIR=str(old))
    for word, words, environment in (
            ("doctor", ["--plugin-state", old, "doctor"], None),
            ("name", [*at(lane, session, blocks["codex"]), "name", "@later"], named)):
        refused = nd(run, lane, old / "runtime/namedrop", "--json", *words, ok=False, environment=environment)
        run.check("the standalone runtime refuses `{}` with the refusal form".format(word),
                  refused.returncode == 69 and refused.stderr.strip() == REFUSED_AFTER_HANDOFF.format(state),
                  {"code": refused.returncode, "stdout": refused.stdout, "stderr": refused.stderr})
    run.check("the refusals started no helper of the old state", not helpers(old), helpers(old))

    def warning_names(issues, named):
        """One ND-INSTALL-001 that counts the agents of `named` and names their blocks, and no other's."""
        if len(issues) != 1:
            return False
        text = issues[0]["explanation"]
        count = "1 agent on Rex has" if len(named) == 1 else "{} agents on Rex have".format(len(named))
        return ("{} not run its hooks since the move from {}".format(count, old) in text and
                all(("{} on rex:{}".format(blocks[a], lane.socket) in text) == (a in named) for a in agents))

    issues, _ = herdr_doctor(run, profile)
    run.check("Herdr's Doctor: ND-INSTALL-001 counts 2 agents and names both blocks",
              warning_names(issues, agents), issues)

    # Codex's next turn, which runs Herdr's hooks from the package copy that replaced its own.
    cache_after = sorted(str(path) for path in (profile.dir / "codex/plugins/cache/namedrop/namedrop").glob("*"))
    run.observe("Codex plugin cache after the handoff", cache_after)
    watch = Watch([str(old) + "/", "/codex/plugins/cache/namedrop/", str(state) + "/"])
    watch.start()
    try:
        started, changed = codex_turn(run, lane, session, blocks["codex"], "Reply with exactly the word ok.")
        time.sleep(5)
    finally:
        seen = watch.stop()
    run.observe("processes during Codex's next turn", seen)
    run.observe("Codex rollouts that completed a turn", changed)
    screen = rx(run, lane, "capture", "-s", session, "-b", blocks["codex"], "--trim", ok=False).stdout
    run.observe("Codex screen after its turn", screen[-3000:])
    old_seen = [row for row in seen if str(old) + "/" in row["command"]]
    run.check("Codex's next turn started no helper of the old state",
              not [row for row in old_seen if watched_kind(row["command"])] and not helpers(old),
              {"old": old_seen, "helpers": helpers(old)})
    locks = [str(path) for name in ("rex-bridge", "relay", "deferred-commands", "agent-status-reconciliation")
             for path in (old / name).rglob("*.lock")] + [str(p) for p in [old / "reminder-clock.lock"] if p.exists()]
    run.check("no lock of the old state's bridge or reminder clock is held", not [p for p in locks if held(Path(p))],
              locks)
    run.check("the old state's hook runs did not change", hook_runs(old) == old_runs,
              {"before": old_runs, "after": hook_runs(old)})
    run.observe("Herdr state hook runs after Codex's turn", hook_runs(state))
    # Codex's turn ran Herdr's hooks, which bound it in Herdr's state, so it
    # runs the new hooks and Doctor counts only Claude Code (the U13 review's F1).
    codex_agent = next(a["agent"] for a in predecessors[0]["agents"] if a["block"] == blocks["codex"])
    run.check("Codex's turn bound its agent in Herdr's state",
              codex_agent in [b.get("agent") for b in bindings(state)], bindings(state))
    issues, _ = herdr_doctor(run, profile)
    run.check("Herdr's Doctor: after Codex's turn, ND-INSTALL-001 counts only Claude Code",
              warning_names(issues, ["claude"]), issues)

    # Doctor in a fresh process, with every Namedrop process stopped.
    run.observe("stopped every Namedrop process", stop_namedrop(profile.dir / "state"))
    issues, _ = herdr_doctor(run, profile, drop=("XDG_STATE_HOME", "NAMEDROP_STATE_DIR"))
    run.check("after every Namedrop process stopped, a fresh Doctor with no XDG_STATE_HOME still counts Claude Code",
              warning_names(issues, ["claude"]), issues)

    # A Rex server restart ends both agents.
    agent_pids = [int(a["agent"].split("@")[0]) for a in predecessors[0]["agents"]]
    server_stop(run, lane)
    lane.start_server()
    wait(lambda: [pid for pid in agent_pids if os_alive(pid)], lambda alive: not alive,
         "the old agents to end with the server", 30)
    issues, _ = herdr_doctor(run, profile)
    run.check("after a Rex server restart, Doctor reports no ND-INSTALL-001", not issues, issues)
    server_stop(run, lane)
    issues, _ = herdr_doctor(run, profile)
    run.check("with the Rex server stopped, Doctor still reports none", not issues, issues)
    lane.start_server()

    # install --remove after this order.
    kept = shared(profile)
    removed = remove(run, lane, state / "runtime/namedrop", state)
    run.observe("removal", removed)
    run.check("install --remove from Herdr's launcher removes only the Rex half, and says so",
              removed.get("removed") is True and removed.get("packages_removed") is False and
              "Removed only the Rex configuration" in (removed.get("message") or ""), removed)
    rex_half_checks(run, profile, lane, config_before, "rex-first")
    run.check("Herdr's packages and record stay as they were", shared(profile) == kept,
              sorted(set(shared(profile).items()) ^ set(kept.items())))


def os_alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def scenario_stopped_server(run, args, request):
    profile = fresh(Both, PROFILES["stopped-server"][0], args.rex)
    agents = ()
    lane = start_rex_only(run, profile, request, agents)
    old = rex_live.state_directory(profile)
    state = herdr_state(profile)
    root = Path(profile.state["archive_root"])
    module_checks(run, lane, old, old / "runtime/namedrop", "the standalone install")
    lane.stop()
    run.observe("standalone helpers with the server stopped", helpers(old))
    add_herdr(profile, agents)
    pane = herdr_up(run, profile)
    started = time.monotonic()
    code, report, stderr = herdr_setup(run, profile, pane, root, "herdr-handoff")
    handoff = (report or {}).get("data", {}).get("handoff")
    run.observe("handoff activation report", handoff)
    run.check("the activation report carries the names notice and the module sealed with no server",
              handoff_reported(handoff, old, "confirmed"), handoff)
    run.check("with the Rex server stopped, Herdr's activation hands off and exits 0",
              code == 0 and report and report.get("status") == "ok" and
              json.loads((old / "runtime/retired.json").read_text()).get("reason") == "herdr_handoff",
              {"code": code, "seconds": round(time.monotonic() - started), "stderr": stderr[-3000:]})
    entry = registration(profile)
    run.check("rex.json bakes Herdr's launcher and seals the entry with no server",
              entry["launcher"] == str(state / "runtime/namedrop") and
              [(p["state"], p["sealed"]) for p in entry.get("predecessors") or []] == [(str(old), True)], entry)
    run.check("the old state's helpers stopped", not helpers(old), helpers(old))
    module = (lane.dir / "config/namedrop.lua").read_text()
    run.check("namedrop.lua names Herdr's launcher", 'local NAMEDROP = "{}"'.format(state / "runtime/namedrop") in module,
              module.splitlines()[:5])
    lane.start_server()
    module_checks(run, lane, state, state / "runtime/namedrop", "the server's next start")
    found = wait(lambda: bridges(state, lane), bool, "a bridge of Herdr's state for the Rex server", 20)
    run.check("the module's load-time start ran Herdr's bridge, and the old state runs none",
              bool(found) and not helpers(old), {"herdr": found, "old": helpers(old)})
    kept = shared(profile)
    removed = remove(run, lane, state / "runtime/namedrop", state)
    run.observe("removal", removed)
    run.check("install --remove removes only the Rex half", removed.get("removed") is True and
              removed.get("packages_removed") is False, removed)
    rex_half_checks(run, profile, lane, b"", "stopped-server")
    run.check("Herdr's packages and record stay as they were", shared(profile) == kept,
              sorted(set(shared(profile).items()) ^ set(kept.items())))


class Forwarder(threading.Thread):
    """A pass-through Unix socket forwarder: A's `rex hosts` entry for B names it, so A reaches B's
    server through a socket that is not B's own, as through a tunnel."""

    def __init__(self, listen, target):
        super().__init__(daemon=True)
        self.listen, self.target = listen, target
        if listen.exists():
            listen.unlink()
        self.server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server.bind(str(listen))
        self.server.listen(16)
        self.connections = 0

    def pump(self, source, sink):
        try:
            while True:
                data = source.recv(65536)
                if not data:
                    break
                sink.sendall(data)
        except OSError:
            pass
        finally:
            for end in (source, sink):
                with contextlib.suppress(OSError):
                    end.shutdown(socket.SHUT_RDWR)

    def run(self):
        while True:
            try:
                client, _ = self.server.accept()
            except OSError:
                return
            self.connections += 1
            upstream = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
            try:
                upstream.connect(str(self.target))
            except OSError:
                client.close()
                continue
            threading.Thread(target=self.pump, args=(client, upstream), daemon=True).start()
            threading.Thread(target=self.pump, args=(upstream, client), daemon=True).start()

    def close(self):
        self.server.close()
        with contextlib.suppress(OSError):
            self.listen.unlink()


def layer_after(run, lane, session, block, action, within=20):
    """Runs a Namedrop palette action on the block and returns the floating layer it opened."""
    listed = lambda: {b["block_id"] for b in json.loads(rx(run, lane, "api", "call", "session.list_blocks",
                                                          json.dumps({"session_id": session})).stdout)["blocks"]}
    before = listed()
    done = rx(run, lane, "do", "-s", session, "-b", block, "--origin", "palette", action, ok=False, timeout=60)
    opened = wait(lambda: listed() - before, bool, action + " to open a layer", within)
    return done, next(iter(opened)), listed


def codex_ready(run, lane, session, blocks):
    """The Rex lane's first run for Codex. Codex's update notice can take the Enter that
    submits the warm-up, which then waits in its input, so a run that ends that way presses
    Enter once more and waits for the hook."""
    before = rex_live.hook_runs(lane.profile)
    try:
        return rex_live.first_run(lane, session, blocks, "codex", before)
    except RuntimeError:
        screen = rx(run, lane, "capture", "-s", session, "-b", blocks["codex"], "--trim", ok=False).stdout
        if "› " + rex_live.livetest.WARM_UP_PROMPT not in screen:
            raise
        run.observe("Codex kept its warm-up unsent", screen[-2000:])
        rx(run, lane, "send-key", "-s", session, "-b", blocks["codex"], "enter")
        evidence = wait(lambda: rex_live.hook_ran(lane.profile, "codex", before), bool,
                        "Codex's SessionStart hook after the warm-up was sent", 120, 1)
        return ["warm-up sent again"], evidence


def store_rows(state, query, *values):
    connection = sqlite3.connect("file:{}?mode=ro".format(state / "namedrop.sqlite3"), uri=True, timeout=5)
    try:
        return connection.execute(query, values).fetchall()
    finally:
        connection.close()


def scenario_two_lanes(run, args, request):
    a = fresh(testbed.Profile, PROFILES["two-lanes"][0], args.rex)
    b = fresh(testbed.Profile, PROFILES["two-lanes"][1], args.rex)
    lane_a = start_rex_only(run, a, request, ())
    lane_b = start_rex_only(run, b, request, ("codex",))
    state_a, state_b = rex_live.state_directory(a), rex_live.state_directory(b)
    run.check("B has no herdr on its lane's PATH", True, rex_live.require_no_herdr(lane_b))

    # B's server restarts with no session, and its bridge starts before any block runs.
    lane_b.stop()
    run.observe("B's Namedrop processes stopped with its server", stop_namedrop(b.dir / "state"))
    lane_b.start_server()
    listed = json.loads(rx(run, lane_b, "list", "--json").stdout).get("sessions") or []
    found = wait(lambda: bridges(state_b, lane_b), bool, "B's bridge after its server restart", 30)
    run.check("B's restarted server has no session, so no block ran a Namedrop command", not listed and
              not json.loads(rx(run, lane_b, "list", "--json").stdout).get("sessions"), listed)
    pid = found[0][0]
    row = runtime.process(pid)
    locks = [path for path in (state_b / "rex-bridge").rglob("*.lock")]
    holders = {str(path): lock_holders(path) for path in locks}
    run.check("B's bridge is `namedrop rex bridge --endpoint rex:<B's socket>`, and holds its lock",
              len(found) == 1 and row[1][1:] == ["rex", "bridge", "--endpoint", "rex:" + str(lane_b.socket)] and
              any(pid in pids for pids in holders.values()) and any(held(path) for path in locks),
              {"bridge": found, "argv": row[1], "holders": holders})
    run.observe("B's bridge process", {"pid": pid, "ppid": subprocess.run(
        ["/bin/ps", "-o", "ppid=,lstart=", "-p", str(pid)], capture_output=True, text=True).stdout.strip(),
        "NAMEDROP_STATE_DIR": row[2].get("NAMEDROP_STATE_DIR"), "REX_BLOCK": row[2].get("REX_BLOCK")})
    run.check("B's bridge carries the module's state, and no block existed to start it",
              row[2].get("NAMEDROP_STATE_DIR") == str(state_b) and not row[2].get("REX_BLOCK"),
              {key: row[2].get(key) for key in ("NAMEDROP_STATE_DIR", "REX_BLOCK", "REX_SESSION")})

    # A Codex starts on B, and Rex's palette actions activate and name it.
    session, blocks = rex_live.start_session(lane_b)
    rex_live.start_agent(lane_b, session, blocks, "codex")
    answered, evidence = codex_ready(run, lane_b, session, blocks)
    run.observe("B's Codex", {"session": session, "blocks": blocks, "answered": answered, "evidence": evidence})
    codex = blocks["codex"]
    done, layer, listed_blocks = layer_after(run, lane_b, session, codex, "namedrop_activate")
    shown = wait(lambda: rx(run, lane_b, "capture", "-s", session, "-b", layer, "--trim", ok=False).stdout,
                 lambda text: text.strip(), "the activation layer to paint", 20)
    run.observe("activation layer", shown[-3000:])
    for _ in range(6):
        if layer not in listed_blocks():
            break
        rx(run, lane_b, "send-key", "-s", session, "-b", layer, "escape", ok=False)
        time.sleep(1)
    run.check("Activate Namedrop from the palette opened one layer, which closed", layer not in listed_blocks(),
              {"exit": done.returncode, "stderr": done.stderr})
    done, layer, listed_blocks = layer_after(run, lane_b, session, codex, "namedrop_name")
    wait(lambda: rx(run, lane_b, "capture", "-s", session, "-b", layer, "--trim", ok=False).stdout,
         lambda text: "Name this pane" in text, "the name screen to paint", 20)
    rx(run, lane_b, "send", "-s", session, "-b", layer, "codex")
    rx(run, lane_b, "send-key", "-s", session, "-b", layer, "enter")
    # The name screen then asks the pane type, with agent as its default, and an agent's
    # persona follows, whose screen saves with Enter once its suggestion has settled. Enter
    # goes again only for a new screen, or one that stayed for 5 s.
    screens, pressed, pressed_at = [], None, 0.0
    until = time.monotonic() + 90
    while layer in listed_blocks() and time.monotonic() < until:
        time.sleep(1)
        shown = rx(run, lane_b, "capture", "-s", session, "-b", layer, "--trim", ok=False).stdout
        if shown not in screens:
            screens.append(shown)
        if ("Pane type:" in shown or "Enter Save" in shown) and (
                shown != pressed or time.monotonic() - pressed_at > 5):
            rx(run, lane_b, "send-key", "-s", session, "-b", layer, "enter", ok=False)
            pressed, pressed_at = shown, time.monotonic()
    run.observe("name and persona screens", screens)
    run.check("the name screen closed", layer not in listed_blocks(), screens[-1:])
    label = wait(lambda: {b["block_id"]: b for b in json.loads(rx(
        run, lane_b, "api", "call", "session.list_blocks", json.dumps({"session_id": session})).stdout)["blocks"]}
        [codex].get("label"), lambda value: value == "@codex", "B's Codex block to be labelled @codex", 20)
    run.check("Name this pane from the palette named B's Codex @codex", label == "@codex", label)
    doctor = lambda: json.loads(nd(run, lane_b, state_b / "runtime/namedrop", *at(lane_b, session, codex), "--json",
                                   "doctor", ok=False).stdout)["data"]
    status = wait(doctor, lambda data: data.get("available_agents") == 1, "B's Doctor to count @codex", 60, 2)
    run.check("B's Doctor counts one available agent", True,
              {key: status.get(key) for key in ("status", "available_agents", "issues")})

    # A mentions B's Codex through A's hosts entry for B, and the reply comes back.
    forward = Path("/tmp/nd-u13-fwd-{}.sock".format(os.getuid()))
    forwarder = Forwarder(forward, lane_b.socket)
    forwarder.start()
    try:
        rx(run, lane_a, "hosts", "add", "standin-b", "unix://" + str(forward))
        status = json.loads(run.run(lane_a.environment(), [lane_a.binary(), "-S", "standin-b", "--autostart=false",
                                                          "api", "call", "server.status"]).stdout)
        run.observe("standin-b answers as", status.get("host_id"))
        created = json.loads(rx(run, lane_a, "new", "nd-lead", "--shell", "none", "-c", str(a.repo), "--json", "--",
                                "/bin/zsh", "-f").stdout)
        a_session, lead = created["session_id"], created["initial_windows"][0]["block_ids"][0]
        # A block's PATH starts with its server's rex directory, which Namedrop's relay runs.
        block_env = dict(lane_a.environment(), REX_BIN_DIR=str(Path(lane_a.binary()).parent))
        block_env["PATH"] = os.pathsep.join([block_env["REX_BIN_DIR"], block_env["PATH"]])
        nd(run, lane_a, state_a / "runtime/namedrop", *at(lane_a, a_session, lead), "--json", "name", "@lead",
           "--type", "terminal", environment=block_env)
        sent = json.loads(nd(run, lane_a, state_a / "runtime/namedrop", *at(lane_a, a_session, lead), "--json", "send",
                             "--input-json", json.dumps({"request_id": "u13-pong", "source":
                                 "@codex@standin-b Reply to this message with exactly the word pong."}),
                             environment=block_env).stdout)
        message = sent.get("data", sent).get("message_id")
        thread = store_rows(state_a, "select thread_id from collaboration_messages where message_id = ?", message)[0][0]
        sent_at = time.time()
        reply = wait(lambda: store_rows(state_a, "select thread_order, author_alias_snapshot, body from "
                                        "collaboration_messages where thread_id = ? and author_alias_snapshot = ?",
                                        thread, "codex@standin-b"), bool, "Codex's reply on A", 420, 2)
        rows = store_rows(state_a, "select m.thread_order, m.author_alias_snapshot, coalesce(m.in_reply_to, '-'), "
                                   "m.body, m.grammar_version, d.state from collaboration_messages m join "
                                   "mailbox_deliveries d using (session_socket_id, worktree_id, message_id) where "
                                   "m.thread_id = ? order by m.thread_order", thread)
        run.observe("A's thread", rows)
        run.check("A's mention reached B's Codex and its reply came back to A's thread ({:.0f}s)".format(
            time.time() - sent_at), any("pong" in body.lower() for _, _, body in reply), reply)
        run.observe("forwarded connections", forwarder.connections)
    finally:
        forwarder.close()

    # install --remove on B, a standalone owner.
    config_before = b""  # the lane's server starts on an empty init.lua
    removed = remove(run, lane_b, state_b / "runtime/namedrop", state_b)
    run.observe("B removal", removed)
    marker = json.loads((state_b / "runtime/retired.json").read_text())
    run.check("install --remove on B retires its state and removes the packages",
              removed.get("removed") is True and removed.get("packages_removed") is True and
              marker.get("reason") == "removed", {"removed": removed, "marker": marker})
    run.check("B's helpers stopped", not helpers(state_b), helpers(state_b))
    rex_half_checks(run, b, lane_b, config_before, "B removal")
    home = b.dir / "home"
    left = [str(path) for path in (home / ".agents/skills/namedrop", home / ".claude/skills/namedrop",
                                   home / ".local/bin/namedrop", home / ".config/namedrop/state-directory")
            if path.exists()]
    run.check("B's packages, shell command and install record are gone", not left, left)
    run.observe("B's Codex config after removal", (b.dir / "codex/config.toml").read_text())
    settings = home / ".claude/settings.json"
    run.observe("B's Claude settings after removal", settings.read_text() if settings.exists() else None)
    refused = nd(run, lane_b, state_b / "runtime/namedrop", "--plugin-state", state_b, "--json", "doctor", ok=False)
    run.check("B's removed runtime refuses with the removal line",
              refused.returncode == 69 and refused.stderr.strip() == REFUSED_AFTER_REMOVAL,
              {"code": refused.returncode, "stderr": refused.stderr})
    # The Codex that B's install served takes one more turn on whatever hooks it still has.
    watch = Watch([str(state_b) + "/", "/codex/plugins/cache/namedrop/"])
    watch.start()
    try:
        codex_turn(run, lane_b, session, codex, "Reply with exactly the word ok.")
        time.sleep(5)
    finally:
        seen = watch.stop()
    run.observe("processes during B's Codex turn after removal", seen)
    run.check("after removal, B's Codex takes a turn that starts no helper of B's state",
              not [row for row in seen if watched_kind(row["command"])] and not helpers(state_b), seen)


def down(profile):
    """Stops the profile's Herdr and Rex servers, Codex's daemon, every Namedrop process and every
    process left under the profile, and keeps its files."""
    environment = testbed.clean_environment(profile.dir, profile.socket, profile.state["toolchain"]) \
        if "toolchain" in profile.state else None
    if environment and profile.socket.exists():
        subprocess.run(testbed.env_command(environment, ["herdr", "server", "stop"]), capture_output=True,
                       timeout=60)
        deadline = time.monotonic() + 10
        while profile.socket.exists() and time.monotonic() < deadline:
            time.sleep(0.1)
    if profile.state.get("rex_bin"):
        rex_live.Lane(profile, testbed.fail, lambda message: None).stop()
    if environment:
        subprocess.run(testbed.env_command(environment, ["codex", "app-server", "daemon", "stop"]),
                       capture_output=True, timeout=60)
    with contextlib.suppress(Failed):
        stop_namedrop(profile.dir)
    profile.kill_leftovers()
    profile.state.pop("rex_live_session", None)
    profile.state.pop("rex_live_blocks", None)
    profile.save()


def drop_builds(profile):
    for path in (profile.dir / "namedrop-source/target", profile.dir / "namedrop-old/target"):
        if path.exists():
            subprocess.run(["rm", "-rf", str(path)], check=True)


SCENARIOS = {"herdr-first": scenario_herdr_first, "old-herdr": scenario_old_herdr, "rex-first": scenario_rex_first,
             "stopped-server": scenario_stopped_server, "two-lanes": scenario_two_lanes}


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--link", type=Path)
    parser.add_argument("--rex")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--keep", action="store_true")
    parser.add_argument("scenarios", nargs="+", choices=sorted(SCENARIOS) + ["down"])
    args = parser.parse_args()
    if args.scenarios == ["down"]:
        for names in PROFILES.values():
            for name in names:
                profile = testbed.Profile(name)
                if profile.dir.exists():
                    down(profile)
                    drop_builds(profile)
                    print("down " + name, flush=True)
        return 0
    if not args.link or not args.rex:
        parser.error("--link and --rex name the checkout and the rex to run")
    request = testbed.requested_source(argparse.Namespace(ref=None, pr=None, link=str(args.link), repo=""))
    failed = []
    for scenario in args.scenarios:
        run = Run(args.out.resolve(), scenario)
        print("== " + scenario, flush=True)
        try:
            SCENARIOS[scenario](run, args, request)
        except (Failed, RuntimeError, subprocess.SubprocessError, OSError, KeyError, ValueError) as error:
            run.note("stopped: {!r}".format(error))
            print("stopped: {}".format(error), flush=True)
            failed.append(scenario)
        finally:
            run.save()
            if not args.keep:
                for name in PROFILES[scenario]:
                    profile = testbed.Profile(name)
                    if profile.dir.exists():
                        with contextlib.suppress(Exception):
                            down(profile)
                        drop_builds(profile)
    print("RESULT " + ("FAIL " + " ".join(failed) if failed else "PASS"), flush=True)
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main())
