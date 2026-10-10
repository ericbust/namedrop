---
name: verify
description: Drive Namedrop's Rust CLI and terminal UI in disposable native Herdr panes, capture evidence, and verify naming, personas, groups, controls, tours, the dashboard, and collaboration behavior after changes.
---

# Verify Namedrop

Read [the feature index](features/README.md), then the matching recipe. The primary user interface is the Namedrop CLI and Herdr terminal popups. Agent hooks and authenticated Codex/Claude sessions are additional entry points. Installation success, hook output, and an agent round trip are separate proofs.

## Launch

From the repository root:

```sh
cargo build --locked --bin namedrop
.agents/skills/verify/scripts/control-namedrop.py --feature names
```

Requires Rust 1.97+, a C compiler, Python 3.9+, Bash, Git, and native Herdr on PATH. The documented distribution build is `sh scripts/build.sh`; verification uses the debug binary to avoid changing an installed runtime. Pass `--binary bin/namedrop` to check a distribution build instead.

The helper starts `herdr server` with an isolated home, configuration, state, data, runtime directory, and Unix socket under `/tmp/nd-verify-*`. It creates a disposable Git workspace and two native Bash panes. Readiness requires the owned server to remain alive, its socket to exist, and each pane's prompt marker to appear. It activates Namedrop through its public CLI within that fixture. Activation writes integration packages and shell configuration only to the fixture home. It does not start a model or install the Codex plugin into your profile.

Each invocation owns its profile, socket, panes, and state. Concurrent runs are supported. Never replace the helper's environment with the caller's Herdr context or link this checkout into the user's running session for verification. No TCP port, credentials, or seed database are required.

Teardown runs automatically in `finally`, on both success and failure. The helper terminates and waits for its own server process, escalates only that child if necessary, and verifies that its socket and owned pane shells are gone.

## Doctor

Every helper run performs one read-only instance check before the feature drive: verify the owned server and pane identities, record both executable versions and the Namedrop binary SHA-256, then run `namedrop doctor --json` with explicit fixture state/socket/workspace/pane arguments. It compares database contents before and after Doctor to detect writes.

Exit `0` means healthy, `1` means needs attention, and `2` means unavailable. The model-free fixture reports `needs_attention` with exactly one issue, `ND-SETUP-003`, because no supported agent is open. The helper requires exactly that result. That expected limitation permits the CLI recipes, but does not establish hook or delivery readiness. Inspect `doctor.json`, `instance.json`, and `server.log` whenever a run behaves unexpectedly. Do not automatically execute Doctor's suggested repairs.

## Drive

```sh
.agents/skills/verify/scripts/control-namedrop.py --feature names
.agents/skills/verify/scripts/control-namedrop.py --feature groups
.agents/skills/verify/scripts/control-namedrop.py --feature controls
.agents/skills/verify/scripts/control-namedrop.py --feature tour
.agents/skills/verify/scripts/control-namedrop.py --feature dashboard
.agents/skills/verify/scripts/control-namedrop.py --feature handoffs
```

The helper sends public CLI commands through `herdr pane run` into real terminal panes. Each command has its own stdout, stderr, exit-status capture, and terminal transcript. It reads saved values through public list commands or a read-only SQLite connection. The tour recipe sends actual `right` and `esc` keys and captures both pages. It runs the tour renderer in the owned terminal; floating-popup actions and prefix shortcuts require a separately linked disposable plugin instance. The dashboard recipe starts the dashboard server from an owned pane and reads it over loopback HTTP. The handoffs recipe proves the model-free rejections only.

The existing native collaboration check is currently blocked by stale inherited pane context:

```sh
python3 tests/coordination_native.py --binary target/debug/namedrop
```

This command reproduces the blocker; it is not a passing handoff drive. Both this check and `tests/personas_native.py` invoke the candidate from Python with only an inherited `HERDR_PANE_ID`. The current CLI correctly rejects that provenance. Repair those invocations to use explicit fixture pane/socket/workspace arguments, or execute them in the owned pane, before using their intended envelope and hook checks as proof. Read [handoffs](features/handoffs.md) for the intended drives and limits. The helper's recipes already use explicit context and run inside the native panes, except the one handoffs step that deliberately runs outside them.

## Evidence

The helper prints the retained directory, `.scratch/verify/nd-verify-*/`, on success or failure. It contains:

- `commands.jsonl`: actual argument arrays, stdout, stderr, and exit codes, including user actions and resulting reads.
- Numbered `*-terminal.txt` files, plus `tour-first.txt` and `tour-second.txt` for the tour.
- `dashboard-snapshot.json` and `dashboard.json` for the dashboard, and `handoffs.json` for the handoff rejections.
- `instance.json`, `doctor.json`, `server.log`, and feature-specific saved values.
- `state.sqlite3` after a successful drive, for independent inspection of persisted side effects.
- `summary.json`: feature, commit, binary identity, limitations, and pass status. The commit alone does not identify uncommitted code; retain the relevant diff with proof of such changes.
- `cleanup.json`: server exit, owned shell PIDs, the dashboard server PID and whether it outlived the run, remaining sockets, and cleanup verdict.

Require the real user action and its observable result. A final list or screenshot alone is insufficient. Verify stored mutations through a second read, and delivery through receipt and final disposition. Only substitute an external system at an existing production boundary, and label what the substitution cannot prove.

Preview is a read-only operation, but verify that claim: compare the fixture database, Git refs/objects, recipient transcripts, and relevant files before and after. Observe host requests when network activity matters. A preview notice or a flag named dry-run is not proof of absent side effects. No authenticated preview or message send runs in the default helper.

## Cleanup

The helper deletes fixture state only after its own server and shell processes have exited and its sockets have disappeared. A dashboard server runs detached from Herdr, so the helper stops it by the PID in `server.json` after checking that its command line names the fixture state. Evidence lives outside the fixture and survives deletion. An incomplete cleanup retains the fixture and returns an error; inspect `cleanup.json` and the recorded PIDs before taking further action. Never kill by process name or stop the user's Herdr server.

After a run, inspect the printed directory's `summary.json` and `cleanup.json`, then confirm `commands.jsonl` and the feature evidence still exist. A passed feature with incomplete cleanup is a failed verification run. Native collaboration has its own cleanup and evidence path described in the feature map.

## Helpers

`scripts/control-namedrop.py` is executable. Run it from the repository root as shown above; `--help` lists its six recipes and `--binary` option. It uses only Python's standard library. Add new drives at public CLI or terminal entry points, rather than setting application state internally.

Use `/maintain-verification-skill` to reconcile the feature map and recipes as Namedrop changes.
