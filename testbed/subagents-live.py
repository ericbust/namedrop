#!/usr/bin/env python3
"""Capture subagent presence from a kept testbed profile, never a normal workspace."""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import time

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent / "lib"))
import livetest
import testbed


def contexts_in(records):
    contexts = []
    for record in records:
        attachment = record.get("attachment") or {}
        if attachment.get("type") == "hook_additional_context":
            contexts.extend(text for text in attachment.get("content", []) if isinstance(text, str))
        payload = record.get("payload") or {}
        if record.get("type") == "response_item" and payload.get("type") == "message" and payload.get("role") == "developer":
            contexts.append("\n".join(part.get("text", "") for part in payload.get("content", []) if isinstance(part, dict)))
    return contexts


def normalized_context(text):
    return re.sub(
        r"available now|busy; work queues until its session settles|no registered Agent Session; work queues until one registers",
        "<readiness>", text,
    )


def unexpected_contexts_in(records, expected, *, codex_child_version=None):
    native = set()
    if codex_child_version == "0.164.0":
        fixture = Path(__file__).parent / "fixtures/codex-0.164.0-child-context.json"
        native = set(json.loads(fixture.read_text())["contexts"])
    return [text for text in contexts_in(records) if normalized_context(text) not in expected and text not in native]


def records_in(path):
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


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
    before_transcript = run.transcript(args.agent)
    before_records = records_in(Path(before_transcript.path))
    expected_contexts = {normalized_context(text) for text in contexts_in(before_records)}
    codex_version = profile.tool_version("codex") if args.agent == "codex" else None
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
        if browser.returncode or result.returncode:
            raise RuntimeError(f"live probe failed with browser exit {browser.returncode} and prompt exit {result.returncode}; inspect browser.log and prompt-result.txt")
        transcript = run.transcript(args.agent)
        path = Path(transcript.path)
        records = records_in(path)
        hooks = []
        for record in records:
            attachment = record.get("attachment")
            if not isinstance(attachment, dict):
                continue
            if any(event in str(attachment.get("hookName", attachment.get("hook_name", "")))
                   for event in ("SubagentStart", "SubagentStop")):
                hooks.append(attachment)
        evidence_records = records[len(before_records):]
        browser_result = json.loads((output / "browser-result.json").read_text())
        child_ids = set()
        for event in browser_result["frames"]:
            for member in event["value"]["frame"]["participants"]:
                if member["alias"].lstrip("@") == args.agent:
                    child_ids.update(child["id"] for child in member.get("subagents", []))
        if len(child_ids) < 2:
            raise RuntimeError("fewer than two host child identities were observed")
        child_root = profile.dir / ("home/.claude/projects" if args.agent == "claude" else "codex/sessions")
        child_paths = [child for child in child_root.rglob("*.jsonl") if any(
            child.name == f"agent-{identity}.jsonl" or child.name.endswith(f"-{identity}.jsonl")
            for identity in child_ids
        )]
        if len(child_paths) != len(child_ids):
            raise RuntimeError("every observed child needs its own transcript")
        transcript_output = output / "transcripts"
        transcript_output.mkdir(exist_ok=True)
        for original in [path, *child_paths]:
            shutil.copyfile(original, transcript_output / original.name)
        contexts = contexts_in(evidence_records)
        unexpected_contexts = unexpected_contexts_in(evidence_records, expected_contexts)
        for child in child_paths:
            child_records = records_in(child)
            contexts.extend(contexts_in(child_records))
            unexpected_contexts.extend(unexpected_contexts_in(
                child_records, expected_contexts, codex_child_version=codex_version,
            ))
        (output / "hook-transcript.json").write_text(json.dumps({
            "path": str(path), "session": before.session,
            "codex_version": codex_version,
            "lifecycle_hook_attachments": hooks,
            "child_transcripts": [str(child) for child in child_paths],
            "transcript_copies": [str(transcript_output / original.name) for original in [path, *child_paths]],
            "unexpected_model_contexts": unexpected_contexts,
            "model_contexts_checked": contexts,
            "note": "Silent lifecycle hooks should produce no model-context attachment. The live count proves the hooks ran.",
        }, indent=2) + "\n")
        if any(hook.get("content") or hook.get("additionalContext") or hook.get("stdout") for hook in hooks):
            raise RuntimeError("lifecycle hook output reached the transcript")
        if unexpected_contexts:
            raise RuntimeError("unexpected model context after subagent hooks; inspect hook-transcript.json")
        print(f"PASS {args.agent}: live count rose and returned to zero. Evidence: {output}")
    finally:
        if browser.poll() is None:
            browser.terminate()
            browser.wait(timeout=10)
        browser_log.close()


if __name__ == "__main__":
    main()
