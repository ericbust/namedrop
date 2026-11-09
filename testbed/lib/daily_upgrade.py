"""The upgrade check: install the previous release, leave deliveries open with agents running,
activate the build under test, and show what a person's running agents go through.

`lane(request, from_ref)` is what `./testbed test --upgrade-check` runs.
"""

from collections import namedtuple
import os
from pathlib import Path
import random
import re
import shlex
import subprocess

from livetest import AGENTS, Check, TIMEOUTS, fields
import livetest as live
import testbed
import dailyuse as du
import daily_channels as dc
import daily_continuity as dcn
import daily_coordination as dco

TAG = re.compile(r"refs/tags/v(\d+)\.(\d+)\.(\d+)$")
BUDGET = 25 * 60
OPEN_SECONDS = 60
"""How long each open delivery's tool runs. It has to outlast the activation, which takes about a
second, and the wait for the second agent's tool to start."""
ACTIVATE_WAIT = 120
OPEN_COMMAND = "time.sleep({})".format(OPEN_SECONDS)
OPEN_PROMPT = ("Run this shell command in the foreground, not in the background, and wait for it to finish: "
               "python3 -c 'import time; time.sleep(" + str(OPEN_SECONDS) + ")'. Then say the single word "
               "{word} in your final message, not with a Namedrop command.")


def newest_tag(ls_remote, below):
    """The highest vMAJOR.MINOR.PATCH under the version `below` in `git ls-remote --tags` output, or
    None. Local tags stop at 1.0.x, so the remote is the only list of releases."""
    versions = [tuple(int(part) for part in found.groups())
                for found in map(TAG.search, ls_remote.splitlines()) if found]
    older = [version for version in versions if version < below]
    return "v{}.{}.{}".format(*max(older)) if older else None


def previous_release(repo, below):
    listing = subprocess.run(["git", "ls-remote", "--tags", "https://github.com/{}.git".format(repo), "v*"],
                             text=True, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=60)
    tag = newest_tag(listing.stdout, below)
    if listing.returncode or not tag:
        testbed.fail("cannot list releases of {}: {}".format(repo, listing.stdout[-300:]))
    return tag


def step_skip_tour(r):
    """The first-run tour holds Namedrop's command lock while it shows, and that blocks activation."""
    shown = r.profile.dir / "state/herdr/plugins/namedrop/tours-shown"
    shown.mkdir(parents=True, exist_ok=True)
    (shown / "welcome-1").touch()
    return [Check("tour", True, "the first-run tour is marked as shown")]


def build_target(r):
    """Build the checkout under test beside the installed release, the way `link_from_clone` does.
    Done before any delivery opens, so the build's minutes aren't part of the open window."""
    profile, path = r.profile, r.request["path"]
    commit = profile.run(["git", "-C", path, "rev-parse", "--verify", "HEAD^{commit}"]).stdout.strip()
    local = profile.dir / "namedrop-next"
    profile.run(["git", "clone", "--quiet", "--no-local", "--no-hardlinks", path, str(local)], capture=False)
    profile.run(["git", "-c", "advice.detachedHead=false", "checkout", "--quiet", commit], cwd=str(local), capture=False)
    profile.run(["sh", "scripts/build.sh"], cwd=str(local), capture=False)
    r.target = local
    return commit


def step_build_target(r):
    return [Check("build under test", True, "built {} beside {}".format(build_target(r)[:7],
                                                                       live.version_text(r.feats)))]


Standing = namedtuple("Standing", "session pids")
OpenDelivery = namedtuple("OpenDelivery", "agent word sent attempt")


def standing(r):
    panes = r.panes()
    return {alias: Standing(panes[alias].session, tuple(sorted(dcn.agent_pids(r, alias)))) for alias in AGENTS}


def judge_standing(before, after):
    return [fields(alias + " stayed up", [("session", after[alias].session, before[alias].session),
                                          ("pids", after[alias].pids, before[alias].pids),
                                          ("running", bool(after[alias].pids), True)]) for alias in AGENTS]


def open_delivery(r, agent, rng):
    word = dco.marker(rng)
    sent = r.channels.post("@{} {}".format(agent, OPEN_PROMPT.format(word=word)))
    hop = r.settle(lambda: du.hop_for(r.store, sent.message_id), lambda found: found and found.attempt_id,
                   TIMEOUTS.label, watch=()) or du.NO_HOP
    return OpenDelivery(agent, word, sent, hop.attempt_id)


def tool_started(r, delivery):
    if not delivery.attempt:
        return False
    key = du.prompt_key(r.store, delivery.attempt)
    return any(OPEN_COMMAND in command for turn in live.delivery_turns(r.transcript(delivery.agent), key)
               for command in turn.commands)


def installed_version(r):
    """What the stable launcher runs, which is what a running agent's hooks call."""
    state = r.profile.dir / "state/herdr/plugins/namedrop"
    result = r.profile.run([str(state / "runtime/namedrop"), "--version"], check=False)
    return live.features(result.stdout.strip().split()[-1]) if result.returncode == 0 else None


def still_open(r, delivery, claims):
    """True only when the delivery's turn is running: this is the state the check has to activate in."""
    hop = du.hop_for(r.store, delivery.sent.message_id) or du.NO_HOP
    return hop.active == 1 and claims.get(delivery.attempt) == "open"


def open_terminal_relay_items(store):
    return sum(kind == "terminal_relay" for kind, _ in du.open_items(store))


def step_activate(r):
    """Activate with both agents in the middle of a delivery's tool call."""
    rng = random.Random()
    ask = r.channels.post("@claude " + dc.NEXT_PROMPT + dc.FINAL_MESSAGE)
    du.wait_closed(r, ask.message_id)
    r.wait_agent("claude", ("idle", "done"), TIMEOUTS.idle)
    r.flags_before = open_terminal_relay_items(r.store)
    r.deliveries = tuple(open_delivery(r, agent, rng) for agent in AGENTS)
    started = {delivery.agent: r.settle(lambda: tool_started(r, delivery), bool, dc.MID_TOOL_WAIT)
               for delivery in r.deliveries}
    r.before = standing(r)
    claims_before = r.store.claims()
    open_before = {delivery.agent: still_open(r, delivery, claims_before) for delivery in r.deliveries}
    r.save("open.json", {"deliveries": [row._asdict() for row in r.deliveries],
                         "claims": {row.attempt: claims_before.get(row.attempt) for row in r.deliveries},
                         "standing": {alias: row._asdict() for alias, row in r.before.items()}})
    r.profile.herdr("plugin", "link", str(r.target))
    code, text = r.profile.in_pane(r.pane("shell"), "sh {} activate".format(
        shlex.quote(str(r.target / "scripts/setup.sh"))), "upgrade-activate", timeout=ACTIVATE_WAIT)
    r.save("activate.txt", text)
    r.profile.state["plugin_root"] = str(r.target.resolve())
    r.profile.state["source"] = dict(r.request, commit=r.profile.run(
        ["git", "-C", str(r.target), "rev-parse", "HEAD"]).stdout.strip())
    r.profile.save()
    r.feats_after = installed_version(r)
    r.channels.stop()
    r.server_after = du.open_channels(r).get("server")
    after = standing(r)
    return [judge_activation(code, r.feats, r.feats_after, started, open_before, r.server_after)
            ] + judge_standing(r.before, after)


def judge_activation(code, before, after, started, open_before, server):
    """It proves an upgrade only if the version rose and both deliveries were mid-tool when it ran."""
    rose = bool(after) and before.version < after.version
    return fields("activate", [("exit", code, 0),
                               ("version", "{} to {}".format(live.version_text(before),
                                                             live.version_text(after) if after else "unknown"), None),
                               ("newer build", rose, True),
                               ("tool started", " ".join(sorted(key for key, value in started.items() if value)),
                                "claude codex"),
                               ("open when it ran", " ".join(sorted(key for key, value in open_before.items() if value)),
                                "claude codex"),
                               ("channels after it", server, None)])


def judge_open_delivery(label, hop, answers, word, claim):
    answer = answers[0] if answers else du.parse_message({})
    return fields(label, [("deliveries", hop.deliveries, 1), ("attempts", hop.attempts, 1),
                          ("retries", hop.retries, 0),
                          ("state", "{}/{}".format(hop.state, hop.active), "settled/0"),
                          ("disposition", hop.disposition, frozenset({"no_reply", "reply"})),
                          ("answers", len(answers), 1), ("word in the answer", word in answer.body, True),
                          ("claim", claim, None)])


def step_open_settle(r):
    checks = []
    for delivery in r.deliveries:
        hop = du.wait_closed(r, delivery.sent.message_id)
        answers = dc.settled_answers(r, delivery.sent, dc.PUBLIC, r.channels.address(delivery.agent), delivery.agent)
        checks.append(judge_open_delivery(delivery.agent + " open delivery", hop, answers, delivery.word,
                                          r.store.claims().get(delivery.attempt)))
    return checks + judge_standing(r.before, standing(r))


def calls_note(transcript, attempt_key, label):
    """Reported, never failed: the session started on the old release pays for its stale hooks."""
    turns = live.delivery_turns(transcript, attempt_key)
    return Check(label + " cost", True, "{}: model calls {}, settle commands {}, hook contexts {}".format(
        label, "+".join(str(turn.calls) for turn in turns) or "0",
        sum(live.settle_commands(turn) for turn in turns),
        sum(len(turn.contexts) for turn in turns)))


def step_fresh(r):
    checks = [Check("old flags", True, "open terminal-reply items: {} before activation, {} after Channels reopened"
                    .format(r.flags_before, open_terminal_relay_items(r.store)))]
    for agent in AGENTS:
        sent = r.channels.post("@{} {}{}".format(agent, dc.NEXT_PROMPT, dc.FINAL_MESSAGE))
        hop = du.wait_closed(r, sent.message_id)
        answers = dc.settled_answers(r, sent, dc.PUBLIC, r.channels.address(agent), agent)
        checks.append(fields(agent + " fresh message", [("deliveries", hop.deliveries, 1), ("attempts", hop.attempts, 1),
                                                        ("state", "{}/{}".format(hop.state, hop.active), "settled/0"),
                                                        ("answers", len(answers), 1)]))
        if hop.attempt_id:
            key = du.prompt_key(r.store, hop.attempt_id)
            transcript = r.settle(lambda: r.transcript(agent), lambda found: live.answered(found, key),
                                  TIMEOUTS.idle, watch=())
            checks.append(calls_note(transcript, key, "old {} session".format(agent)))
    return checks + judge_standing(r.before, standing(r))


def step_new_session(r):
    """What a person does once an update has stranded a Codex session: start a new one and approve
    its changed hooks. The screen table answers that review."""
    dcn.stop_agent(r, "codex")
    r.profile.launch("codex", live.launch_command("codex", r.mode, os.environ))
    first = r.settle_screens("codex")
    if first.restart:
        r.restart_codex(stop_daemon=False)
    r.warm_up("codex")
    sent = r.channels.post("@codex {}{}".format(dc.NEXT_PROMPT, dc.FINAL_MESSAGE))
    hop = du.wait_closed(r, sent.message_id)
    answers = dc.settled_answers(r, sent, dc.PUBLIC, r.channels.address("codex"), "codex")
    return [fields("new codex session", [("hook review on start", "hooks-review" in first.answered, None),
                                         ("state", "{}/{}".format(hop.state, hop.active), "settled/0"),
                                         ("disposition", hop.disposition, "no_reply"),
                                         ("answers", len(answers), 1)])]


def step_clean(r):
    doctor = r.doctor("after-upgrade")
    return dcn.check_backlog(r) + [fields("setup", [("doctor ND-SETUP-004", any(issue.code == "ND-SETUP-004"
                                                                                for issue in doctor.issues), False)])]


SETUP = (live.Step("tour", "tour", "", step_skip_tour, ""),
         live.Step("first-run", "first run", "", live.step_first_run, ""),
         live.Step("names", "names", "", du.step_names, ""),
         live.Step("hooks", "hooks", "", live.step_hooks, ""),
         live.Step("channels", "channels", "", du.step_channels, ""),
         live.Step("build-target", "build under test", "", step_build_target, ""))

CHECKS = (
    du.check_step("activate", "activate", step_activate,
                  "Activating the build with deliveries open kept every agent running.", restarts_channels=True),
    du.check_step("open-delivery", "open delivery", step_open_settle, "The open deliveries settled once."),
    du.check_step("fresh-message", "fresh message", step_fresh, "A new message worked in each old session."),
    du.check_step("new-session", "new codex session", step_new_session, "A new Codex session answered."),
    du.check_step("clean", "backlog", step_clean, ""),
)


def lane(from_ref, repo, target):
    """target: the checkout under test, whose version decides which release came before it."""
    manifest = Path(target).expanduser() / "herdr-plugin.toml"
    found = re.search(r'(?m)^version\s*=\s*"(\d+)\.(\d+)\.(\d+)', manifest.read_text())
    if not found:
        testbed.fail("cannot read the version of the checkout under test: " + str(manifest))
    tag = from_ref or previous_release(repo, tuple(int(part) for part in found.groups()))
    return du.Lane("upgrade", SETUP, CHECKS, BUDGET, {"kind": "ref", "repo": repo, "ref": tag})
