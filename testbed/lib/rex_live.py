"""The Rex live lane: Claude Code and Codex in blocks of a scratch Rex server, with Namedrop
installed into the profile by the build's own install.sh, with no herdr on the profile's PATH,
and every hook baked to its state directory. The Rex adapter's live checks run against it."""

import sys

sys.dont_write_bytecode = True

import json
import os
from pathlib import Path
import shlex
import shutil
import subprocess
import time

import livetest
import rex_lane
import source as sources
import testbed

SESSION = "testbed-rex"
AGENTS = {"claude": ("claude",), "codex": ("codex", "--no-daemon")}
KEYS = {"enter": "enter", "down": "down", "esc": "escape", "c-c": "ctrl+c", "ctrl+u": "ctrl+u"}
FIRST_RUN_SECONDS = 180


def without_herdr(path, profile_dir):
    """PATH without the host's directories that hold a herdr. The profile's own directories
    stay, so a herdr linked into them is still found."""
    return os.pathsep.join(entry for entry in path.split(os.pathsep) if entry and (
        Path(entry) == profile_dir or profile_dir in Path(entry).parents
        or not shutil.which("herdr", path=entry)))


class Lane(rex_lane.Lane):
    """The profile's clean environment without its Herdr names or any herdr on PATH, so a
    block's only location names are the REX_* its server gives it. The server is also every
    block's Rex home, so a `rex` that Namedrop runs from a block or a daemon reads the lane's
    own hosts."""

    def environment(self):
        environment = {key: value for key, value in self.profile.env.items()
                       if not key.startswith("HERDR_")}
        environment["PATH"] = without_herdr(environment["PATH"], self.profile.dir)
        environment["REX_SERVER"] = self.server
        environment["REX_HOME"] = self.server
        return environment

    def command_line(self, *args):
        """A `rex` command for a person. A block takes its environment from the server, so the
        client needs only the profile's HOME."""
        client = {"HOME": self.profile.env["HOME"], "PATH": "/usr/bin:/bin"}
        return shlex.join(testbed.env_command(client, [
            self.binary(), "-S", self.server, "--autostart=false"] + list(args)))

    def run(self, command, log, cwd, extra=None):
        """Runs a command in the lane's environment, its stderr appended to the log, and
        returns its stdout. A failure stops the run with the log's tail."""
        environment = dict(self.environment(), **(extra or {}))
        with log.open("a") as errors:
            result = subprocess.run(testbed.env_command(environment, command), cwd=str(cwd),
                                    text=True, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                                    stderr=errors)
        if result.returncode != 0:
            testbed.fail("{} exited {}\n{}{}".format(shlex.join(command), result.returncode,
                                                     result.stdout, log.read_text()[-4000:]))
        return result.stdout


def state_directory(profile):
    """Where install.sh puts Namedrop's state when nothing names one: XDG_STATE_HOME/namedrop."""
    return profile.dir / "state/namedrop"


def require_no_herdr(lane):
    path = lane.environment()["PATH"]
    found = shutil.which("herdr", path=path)
    if found:
        testbed.fail("the Rex lane's PATH finds a herdr: " + found)
    return "No herdr on the lane's PATH: " + path


def build_archive(lane):
    """Builds the source's release archive with its own package-release.sh, checks its
    checksum, and unpacks it into the profile. Returns the unpacked archive's root."""
    profile = lane.profile
    source = profile.clone(sources.clone_plan(profile.state["source"]))
    for stale in ("dist", "namedrop-archive"):
        if (profile.dir / stale).exists():
            shutil.rmtree(str(profile.dir / stale))
    log = profile.dir / "logs/package-release.log"
    testbed.say("Building the release archive from {}. A release build can take several "
                "minutes.".format(source))
    printed = lane.run(["sh", "scripts/package-release.sh", "--output", str(profile.dir / "dist")],
                       log, source).splitlines()
    archive, checksum = Path(printed[-2]), Path(printed[-1])
    lane.run(["shasum", "-a", "256", "-c", checksum.name], log, checksum.parent)
    unpacked = profile.dir / "namedrop-archive"
    unpacked.mkdir(mode=0o700)
    lane.run(["tar", "-xzf", str(archive), "-C", str(unpacked)], log, profile.dir)
    root = unpacked / archive.name[:-len(".tar.gz")]
    if not (root / "scripts/install.sh").is_file():
        testbed.fail("the archive has no scripts/install.sh: " + str(archive))
    testbed.say("Built " + archive.name)
    return root


def install(lane, request, agents):
    """Builds the source's release archive and runs its install.sh --yes into the profile, with
    no herdr on PATH and the lane's server running, so the install edits that server's config.
    The install puts the agents' packages into the profile with every hook baked to the
    profile's state directory. A profile installs once."""
    profile = lane.profile
    if profile.state.get("activated"):
        testbed.fail("profile {} was activated in a profile Herdr by an older testbed; pass "
                     "--fresh".format(profile.name))
    profile.prepare(agents, herdr=False)
    if "source" not in profile.state:
        profile.state["source"] = profile.resolve(request)
        profile.save()
    testbed.ensure_codex_settings(profile.dir / "codex/config.toml")
    testbed.say(require_no_herdr(lane))
    lane.start_server()
    if profile.state.get("installed"):
        return
    profile.seed()
    root = build_archive(lane)
    log = profile.dir / "logs/install.log"
    report = lane.run(["sh", str(root / "scripts/install.sh"), "--yes"], log, profile.dir,
                      {"REX_BIN_DIR": str(Path(lane.binary()).parent)})
    with log.open("a") as kept:
        kept.write(report)
    print(report, end="", flush=True)
    state = state_directory(profile)
    if "State directory: {}\n".format(state) not in report:
        testbed.fail("install.sh did not install into {}; see {}".format(state, log))
    profile.state.update(installed=True, archive_root=str(root))
    profile.save()


def hook_runs(profile):
    """What Namedrop's hooks recorded, one file per agent session: the plugin root they ran from."""
    directory = state_directory(profile) / "hook-runs"
    if not directory.is_dir():
        return {}
    return {path.name: path.read_text() for path in directory.iterdir()}


def session_blocks(lane):
    session = lane.profile.state.get("rex_live_session")
    if not session or session not in lane.sessions(every=True):
        return None, {}
    return session, dict(lane.profile.state.get("rex_live_blocks") or {})


def start_session(lane):
    session, blocks = session_blocks(lane)
    if session:
        return session, blocks
    # A login shell runs path_helper, which can put a herdr back on PATH.
    shell = ("/bin/bash", "--noprofile", "--rcfile", str(lane.profile.dir / "home/.bashrc"), "-i")
    created = json.loads(lane.rex("new", SESSION, "--shell", "none", "-c", str(lane.profile.repo),
                                  "--window", "agents", "--json", "--", *shell).stdout)
    session = created["session_id"]
    blocks = {"shell": created["initial_windows"][0]["block_ids"][0]}
    lane.profile.state.update(rex_live_session=session, rex_live_blocks=blocks)
    lane.profile.save()
    return session, blocks


def shell_finds_no_herdr(lane, session, blocks, timeout=20):
    """Types a herdr lookup into the shell block, which writes what it found to a file."""
    found = lane.profile.dir / "logs/shell-herdr.txt"
    if found.exists():
        found.unlink()
    target = shlex.quote(str(found))
    lane.rex("send", "-s", session, "-b", blocks["shell"],
             "command -v herdr > {0} || echo none > {0}".format(target))
    lane.rex("send-key", "-s", session, "-b", blocks["shell"], "enter")
    deadline = time.monotonic() + timeout
    while not (found.exists() and found.read_text().endswith("\n")):
        if time.monotonic() >= deadline:
            testbed.fail("the shell block {} did not run the herdr lookup".format(blocks["shell"]))
        time.sleep(0.2)
    if found.read_text() != "none\n":
        testbed.fail("the shell block {} finds a herdr: {}".format(blocks["shell"], found.read_text()))
    return "The shell block {} finds no herdr".format(blocks["shell"])


def hook_log(profile):
    return profile.dir / "logs/claude-hooks.log"


def agent_command(profile, agent):
    """The agent's command, with the profile's claude named in full, because a block's shell can
    put another claude ahead of the profile's bin."""
    command = AGENTS[agent]
    if agent == "claude":
        return (str(profile.dir / "bin/claude"),) + command[1:]
    return command


def start_agent(lane, session, blocks, agent):
    if agent in blocks:
        return blocks[agent]
    # Claude Code passes its plugin hooks no PLUGIN_ROOT, so Namedrop records no hook run for
    # them; its own hook log shows they ran.
    if agent == "claude" and hook_log(lane.profile).exists():
        hook_log(lane.profile).unlink()
    command = agent_command(lane.profile, agent)
    if agent == "claude":
        command += ("--debug=hooks", "--debug-file", str(hook_log(lane.profile)))
    created = json.loads(lane.rex("split", "-s", session, "-b", blocks["shell"], "--label", agent,
                                  "--shell", "none", "--focus=false", "-c", str(lane.profile.repo),
                                  "--json", "--", *command).stdout)
    blocks[agent] = created["block_ids"][0]
    lane.profile.state["rex_live_blocks"] = blocks
    lane.profile.save()
    return blocks[agent]


def hook_ran(profile, agent, before):
    """How this agent's Namedrop SessionStart hook is known to have run, from a script baked to
    the profile's state directory, or None while it has not."""
    if agent == "codex":
        roots = [root for name, root in hook_runs(profile).items()
                 if before.get(name) != root and "/codex/plugins/" in root]
        if not roots:
            return None
        script, evidence = Path(roots[0]) / "scripts/session-start", "a hook run from " + roots[0]
    else:
        log = hook_log(profile)
        lines = log.read_text(errors="replace").splitlines() if log.exists() else []
        if not any("SessionStart:" in line and "/scripts/session-start" in line and
                   "finished with status 0" in line for line in lines):
            return None
        script = profile.dir / "home/.claude/skills/namedrop/scripts/session-start"
        evidence = "its SessionStart hook finished, as " + str(log) + " shows"
    baked = "--state-directory '{}'".format(state_directory(profile))
    if baked not in script.read_text():
        testbed.fail("{} does not pass {}".format(script, baked))
    return evidence


def first_run(lane, session, blocks, agent, before):
    """Answers the agent's first-run prompts as the Herdr lane's screen table names them, until its
    Namedrop SessionStart hook has run. A Codex that just trusted its hooks runs none of them in
    that session, so it starts again in a new block, as the Herdr lane restarts it. Codex runs
    SessionStart only with its first prompt, so a ready Codex gets the Herdr lane's warm-up."""
    rows = livetest.screens_for(agent)
    block = blocks[agent]
    answered = []
    deadline = time.monotonic() + FIRST_RUN_SECONDS
    text = ""
    while time.monotonic() < deadline:
        evidence = hook_ran(lane.profile, agent, before)
        if evidence:
            return answered, evidence
        text = lane.rex("capture", "-s", session, "-b", block, "--trim").stdout
        row = livetest.classify(text, rows)
        if row and row.outcome == livetest.ANSWER:
            lane.rex("send-key", "-s", session, "-b", block, *[KEYS[key] for key in row.keys])
            answered.append(row.name)
            time.sleep(2)
            if row.after == livetest.RESTART_CODEX:
                lane.rex("block", "close", "-s", session, block)
                del blocks[agent]
                block = start_agent(lane, session, blocks, agent)
                answered.append("restarted in " + block)
            continue
        if row and row.outcome == livetest.READY and agent == "codex" and "warm-up" not in answered:
            lane.rex("send", "-s", session, "-b", block, livetest.WARM_UP_PROMPT)
            lane.rex("send-key", "-s", session, "-b", block, "enter")
            answered.append("warm-up")
        if row and row.outcome in (livetest.FATAL, livetest.EXITED):
            testbed.fail("{} in Rex block {}: {}\n{}".format(
                agent, block, row.message.format(profile=lane.profile.name), text))
        time.sleep(0.5)
    testbed.fail("{} in Rex block {} ran no Namedrop SessionStart hook in {}s; answered {}. "
                 "Screen:\n{}".format(agent, block, FIRST_RUN_SECONDS, answered or "nothing", text))


def up(profile, request, agents):
    """Every agent block it reports was started and proved by this call: a block saved from an
    earlier `up` is closed and started again, because only a fresh start shows its hook runs. A
    failure stops everything `up` started, as `down` does."""
    lane = Lane(profile, testbed.fail, testbed.say)
    try:
        install(lane, request, agents)
        lane.start_server()
        session, blocks = start_session(lane)
        testbed.say(shell_finds_no_herdr(lane, session, blocks))
        for agent in agents:
            if agent in blocks:
                lane.rex("block", "close", "-s", session, blocks.pop(agent), check=False)
            before = hook_runs(profile)
            start_agent(lane, session, blocks, agent)
            answered, evidence = first_run(lane, session, blocks, agent, before)
            testbed.say("{} runs in Rex block {}; answered {}; {}".format(
                agent, blocks[agent], ", ".join(answered) or "no prompts", evidence))
    except BaseException:
        try:
            down(profile)
        except (RuntimeError, OSError, subprocess.SubprocessError) as error:
            testbed.say("Cleaning up after the failed rex up failed too: {}".format(error))
        raise
    return lane, session, blocks


def down(profile):
    """Stops the scratch Rex server with its sessions and Codex's daemon, and forgets the
    session."""
    lane = Lane(profile, testbed.fail, testbed.say)
    try:
        lane.stop()
    finally:
        profile.run(["codex", "app-server", "daemon", "stop"], check=False)
        profile.kill_leftovers()
    profile.state.pop("rex_live_session", None)
    profile.state.pop("rex_live_blocks", None)
    profile.save()
