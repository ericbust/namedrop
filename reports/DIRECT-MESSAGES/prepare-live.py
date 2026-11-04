import copy
import json
import os
from pathlib import Path
import sys

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "testbed/lib"))
import testbed
import livetest

name = sys.argv[1] if len(sys.argv) > 1 else "test-direct-messages"
if not name.startswith("test-"):
    raise RuntimeError("Live DM verification requires a test- profile")
profile = testbed.Profile(name)
output = Path(os.environ.get("DM_LIVE_OUTPUT", str(ROOT / "reports/DIRECT-MESSAGES/live")))
output.mkdir(parents=True, exist_ok=True)
mode = livetest.choose_mode("no-daemon", livetest.features(profile.namedrop_version()))
run = livetest.Run(profile, mode, output)
checks = livetest.step_first_run(run)
for check in checks:
    if not check.ok:
        raise RuntimeError(str(check))
primary = copy.deepcopy(profile.state)
primary_repo = profile.repo
secondary_repo = profile.dir / "repo-two"
if not secondary_repo.exists():
    profile.run(["git", "worktree", "add", "--detach", str(secondary_repo), "HEAD"], cwd=str(primary_repo))
secondary = profile.herdr(
    "worktree", "open", "--cwd", str(secondary_repo), "--path", str(secondary_repo),
    "--label", "dm-second-workspace", "--no-focus",
)["workspace"]["workspace_id"]
try:
    profile.repo = secondary_repo
    profile.clear_panes()
    profile.state["workspace"] = secondary
    profile.save()
    profile.panes(("claude",), launch=False)
    livetest.write_json(profile.dir / "home/.claude.json", lambda state: livetest.claude_state_seed(
        state, str(secondary_repo.resolve()), profile.tool_version("claude")))
    profile.launch("claude", "claude")
    second_run = livetest.Run(profile, mode, output)
    second_run.settle_screens("claude")
    if second_run.warm_up("claude") is None:
        raise RuntimeError("Second workspace Claude session did not register")
    secondary_panes = copy.deepcopy(profile.state["panes"])
finally:
    profile.repo = primary_repo
    profile.state = primary
    profile.save()
receipt = profile.namedrop("channels", "--no-open")
if receipt.returncode:
    raise RuntimeError(receipt.stdout)
url_file = profile.dir / "dm-channels-url.json"
url_file.write_text(receipt.stdout)
summary = {
    "profile": name,
    "tested_commit": profile.state["source"]["commit"],
    "primary_workspace": primary["workspace"],
    "primary_panes": primary["panes"],
    "secondary_workspace": secondary,
    "secondary_panes": secondary_panes,
    "isolated_home": profile.env["HOME"],
    "isolated_config": profile.env["XDG_CONFIG_HOME"],
    "socket": str(profile.socket),
}
(output / "profile.json").write_text(json.dumps(summary, indent=2) + "\n")
print("Real agents ready in two isolated testbed workspaces.")
print("Channels URL receipt saved at " + str(url_file))
