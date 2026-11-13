"""The Rex live lane: Claude Code and Codex in blocks of a scratch Rex server, with Namedrop's
hooks installed into the profile and baked to its state directory. The Rex adapter's live
checks run against it."""

import sys

sys.dont_write_bytecode = True

import json
from pathlib import Path
import shlex
import subprocess
import time

import livetest
import rex_lane
import testbed

SESSION = "testbed-rex"
AGENTS = {"claude": ("claude",), "codex": ("codex", "--no-daemon")}
KEYS = {"enter": "enter", "down": "down", "esc": "escape", "c-c": "ctrl+c", "ctrl+u": "ctrl+u"}
FIRST_RUN_SECONDS = 180


class Lane(rex_lane.Lane):
    """The profile's clean environment without its Herdr names, so a block's only location
    names are the REX_* its server gives it."""

    def environment(self):
        environment = {key: value for key, value in self.profile.env.items()
                       if not key.startswith("HERDR_")}
        environment["REX_SERVER"] = self.server
        return environment

    def command_line(self, *args):
        """A `rex` command for a person. A block takes its environment from the server, so the
        client needs only the profile's HOME."""
        client = {"HOME": self.profile.env["HOME"], "PATH": "/usr/bin:/bin"}
        return shlex.join(testbed.env_command(client, [
            self.binary(), "-S", self.server, "--autostart=false"] + list(args)))


def state_directory(profile):
    return profile.dir / "state/herdr/plugins/namedrop"


def install(profile, request, agents):
    """Builds the source and activates it in the profile's own Herdr, which installs the
    agents' Namedrop packages with every hook baked to the profile's state directory. That
    Herdr is stopped before any Rex block starts."""
    profile.prepare(agents)
    if "source" not in profile.state:
        profile.state["source"] = profile.resolve(request)
        profile.save()
    testbed.ensure_codex_settings(profile.dir / "codex/config.toml")
    if profile.state.get("activated"):
        return
    profile.start_server()
    profile.install_plugin(profile.state["source"])
    profile.seed()
    profile.workspace()
    shell = profile.herdr("pane", "list", "--workspace", profile.state["workspace"])["panes"][0]
    profile.activate(shell["pane_id"])
    profile.stop_server()


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
    created = json.loads(lane.rex("new", SESSION, "-c", str(lane.profile.repo), "--window", "agents",
                                  "--json").stdout)
    session = created["session_id"]
    blocks = {"shell": created["initial_windows"][0]["block_ids"][0]}
    lane.profile.state.update(rex_live_session=session, rex_live_blocks=blocks)
    lane.profile.save()
    return session, blocks


def hook_log(profile):
    return profile.dir / "logs/claude-hooks.log"


def start_agent(lane, session, blocks, agent):
    if agent in blocks:
        return blocks[agent]
    # Claude Code passes its plugin hooks no PLUGIN_ROOT, so Namedrop records no hook run for
    # them; its own hook log shows they ran.
    if agent == "claude" and hook_log(lane.profile).exists():
        hook_log(lane.profile).unlink()
    command = AGENTS[agent] + (("--debug=hooks", "--debug-file", str(hook_log(lane.profile)))
                               if agent == "claude" else ())
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
        install(profile, request, agents)
        lane.start_server()
        session, blocks = start_session(lane)
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
    """Stops the scratch Rex server with its sessions, the profile's own Herdr, and Codex's
    daemon, and forgets the session."""
    lane = Lane(profile, testbed.fail, testbed.say)
    try:
        lane.stop()
    finally:
        profile.stop_server()
        profile.run(["codex", "app-server", "daemon", "stop"], check=False)
        profile.kill_leftovers()
    profile.state.pop("rex_live_session", None)
    profile.state.pop("rex_live_blocks", None)
    profile.save()
