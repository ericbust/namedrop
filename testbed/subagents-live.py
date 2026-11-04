#!/usr/bin/env python3
"""Capture subagent presence from a kept testbed profile, never a normal workspace."""

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
import livetest
import testbed


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--agent", choices=("claude", "codex"), required=True)
    args = parser.parse_args()
    if not args.profile.startswith("test-"):
        parser.error("only test- profiles are allowed")
    profile = testbed.Profile(args.profile)
    profile.owned()
    if profile.env["HOME"] == str(Path.home()) or profile.env["XDG_CONFIG_HOME"] == os.environ.get("XDG_CONFIG_HOME"):
        raise RuntimeError("profile isolation failed")
    if not profile.socket.name.startswith("nd-tb-"):
        raise RuntimeError("refusing a non-testbed socket")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    (output / "browser-ready").unlink(missing_ok=True)
    run = livetest.Run(profile, livetest.MODES["no-daemon"], output)
    before = run.panes()[args.agent]
    if before.session is None:
        raise RuntimeError("warm up the agent with testbed/testbed test --keep first")
    shell = profile.state["panes"]["shell"]
    opened = profile.namedrop("--herdr-pane", shell, "channels", "--no-open", "--json")
    if opened.returncode:
        raise RuntimeError(opened.stdout)
    channels = json.loads(opened.stdout)
    (output / "channels.json").write_text(json.dumps(channels, indent=2) + "\n")
    (output / "profile.json").write_text(json.dumps({
        "name": profile.name, "home": profile.env["HOME"],
        "config": profile.env["XDG_CONFIG_HOME"], "socket": str(profile.socket),
        "workspace": profile.state["workspace"], "pane": run.pane(args.agent),
        "session": before.session, "source": profile.state.get("source"),
    }, indent=2) + "\n")
    watcher = Path(__file__).with_name("subagents-live.mjs")
    browser_log = (output / "browser.log").open("w")
    browser = subprocess.Popen([
        "node", str(watcher), channels["data"]["url"], str(output), args.agent,
    ], stdout=browser_log, stderr=subprocess.STDOUT)
    try:
        deadline = time.monotonic() + 30
        while not (output / "browser-ready").exists():
            if browser.poll() is not None or time.monotonic() >= deadline:
                raise RuntimeError("browser did not become ready; see browser.log")
            time.sleep(0.2)
        spawn = "Agent tool" if args.agent == "claude" else "spawn_agent tool"
        prompt = (
            f"This is an authorized test of subagent presence. Use the {spawn} to launch "
            "exactly THREE subagents concurrently, before waiting for any. Give them distinct "
            "names table-a, table-b, table-c. Each must independently derive and print a "
            "complete 15 by 15 multiplication table, then check every row and explain three "
            "checks. Children must not use tools, spawn more agents, read or edit files, or "
            "send messages. Wait for all three children to finish, close them if your runtime "
            "requires closing completed agents, then reply only SUBAGENTS_FINISHED. Do not "
            "do the table work yourself."
        )
        (output / "prompt.txt").write_text(prompt + "\n")
        result = profile.run([
            "herdr", "agent", "prompt", run.pane(args.agent), prompt, "--wait",
            "--until", "idle", "--until", "done", "--timeout", "240000",
        ], check=False)
        (output / "prompt-result.txt").write_text(result.stdout)
        browser.wait(timeout=300)
        run.capture()
        transcript = run.transcript(args.agent)
        path = Path(transcript.path)
        records = [json.loads(line) for line in path.read_text().splitlines() if line.strip()]
        hooks = []
        for record in records:
            attachment = record.get("attachment")
            if not isinstance(attachment, dict):
                continue
            if any(event in str(attachment.get("hookName", attachment.get("hook_name", "")))
                   for event in ("SubagentStart", "SubagentStop")):
                hooks.append(attachment)
        contexts = list(transcript.session_contexts)
        for turn in transcript.turns:
            if "authorized test of subagent presence" in turn.prompt:
                contexts.extend(turn.contexts)
        lifecycle_contexts = [text for text in contexts if any(
            marker in text for marker in ("SubagentStart", "SubagentStop", "scripts/subagent")
        )]
        (output / "hook-transcript.json").write_text(json.dumps({
            "path": str(path), "session": before.session,
            "lifecycle_hook_attachments": hooks,
            "lifecycle_model_contexts": lifecycle_contexts,
            "model_contexts_checked": contexts,
            "note": "Silent lifecycle hooks should produce no model-context attachment. The live count proves the hooks ran.",
        }, indent=2) + "\n")
        if browser.returncode or result.returncode:
            raise RuntimeError("live probe failed; inspect browser.log and prompt-result.txt")
        if any(hook.get("content") or hook.get("additionalContext") or hook.get("stdout") for hook in hooks):
            raise RuntimeError("lifecycle hook output reached the transcript")
        if lifecycle_contexts:
            raise RuntimeError("lifecycle hook context found in model messages")
        print(f"PASS {args.agent}: live count rose and returned to zero. Evidence: {output}")
    finally:
        if browser.poll() is None:
            browser.terminate()
            browser.wait(timeout=10)
        browser_log.close()


if __name__ == "__main__":
    main()
