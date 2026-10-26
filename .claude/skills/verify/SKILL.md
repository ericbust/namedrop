---
name: verify
description: Prove a Namedrop change on real Claude Code and Codex panes by running the maintainer's testbed checklist with one command and reporting its evidence. Use for /verify, "does this branch work with real agents", or before a pull request that changes runtime behavior merges.
---

# Verify Namedrop with real agents

Namedrop's runtime surface is Herdr panes with real agents in them. Unit tests and
`cargo test` are not evidence here. The handle is the testbed in this repository,
[testbed/](../../../testbed/README.md), a kit that builds the branch into an isolated Herdr
profile, starts Claude Code and Codex, answers their first-run screens, sends the
CHECKLIST mentions both ways, and checks every result in Namedrop's own database and the
agents' transcripts. Nothing it does touches the real `~/.herdr`, `~/.claude` or
`~/.codex`.

## Find the change

```sh
git log --oneline @{u}..        # or public/main..HEAD; count the commits
git diff @{u}.. --stat
```

The testbed builds the checkout's HEAD. Commit before you run it; uncommitted edits are
not tested. The kit warns about them only on a new or `--fresh` profile; a rerun of a
kept profile stays silent.

## Doctor

```sh
testbed/testbed check && testbed/testbed list
```

`check passed` means the tools, Python and socket path are fine and the kit's offline
self-test passes. `list` shows every profile and whether it is running; a leftover
`test-*` profile from a failed run or `--keep` is yours to inspect or destroy.

## Launch and drive

One command does `testbed/CHECKLIST.md` with no human. Run it, and every `testbed/testbed`
command below, from the root of the Namedrop checkout under verification:

```sh
testbed/testbed test --link . --fresh --yes
```

`--link` names the checkout to build; `.` is the one you are in. A fresh run takes about 3
minutes and a rerun of a kept profile about 1.5; the two mention round trips take the
longest and the build about a minute. After the build the run has 20 minutes, each
mention round trip 5, and a pane blocked for 30 seconds fails its step. The default is
plain `codex` on Namedrop 1.2.0 and later, which also proves the shared app-server
warning (`ND-SESSION-003`), runs the two commands Doctor gives as the fix, and checks that
the warning clears. `--codex no-daemon` skips that arc. `--keep` leaves the profile
running after a pass so you can probe further.

What the run asserts, step by step: build and names; first-run screens cleared; the Codex
server state (and the fix in plain mode); `namedrop address list` and `doctor` clean; the
hooks loaded (the policy text once per session and the roster on the first turn, in both
transcripts); Claude to Codex `pong` and Codex to Claude `ping`, each delivered once and
replied once (one delivery, one attempt, no retries, one receipt, settled, two messages
in the thread, the envelope seen exactly once by the recipient with its proof marker);
`echo hello` on `@shell` with exit 0; nothing awaiting progress at the end. It does not
show the update notice (Namedrop 1.3.0 and later): a `--link` install never checks for
updates. Use the update notice probe under **Push on it** for that.

## Read the result

- Exit 0 is a pass. The report ends with `PASS n/n in …. Report: <path>.txt` and the
  compatibility line that starts with `- **`; on the terminal one more line follows,
  `Deleted profile …` or, with `--keep`, `Kept profile …`. A JSON copy with the same name
  and a `.json` suffix sits beside that `.txt` under `~/.namedrop-testbed/reports/`; its
  `steps[].checks[]` carry `what`, `ok` and `detail`. Plain mode has ten steps, no-daemon
  mode nine.
- Exit 1 is a failure. The report names the failing check and its evidence directory
  (`~/.namedrop-testbed/profiles/<profile>/logs/test-<stamp>/NN-step/` with pane captures,
  doctor JSON, database rows and transcript summaries) and keeps the profile and its
  server. `report.txt` and `report.json` sit in `test-<stamp>/` itself; a failure is not
  copied to `reports/`. A step 1 (build) failure names `01-build/`, which does not exist;
  its whole story is in the report. The report prints `testbed/testbed attach`, which refuses
  to run inside a Herdr pane; from an agent use `testbed/testbed status --profile <profile>`
  and the manual drive notes. Read the evidence, then destroy.
- The profile is `test-link-<checkout name>`. `test` never drives or deletes any other
  profile.

## Push on it

The harness covers the happy path. Probe around the change at the same surface:

- Run it twice: `--fresh --keep` once, then again without `--fresh`. The second run must
  reset the panes into a new workspace and pass again. A pass without `--keep` deletes the
  profile, so the second run would only build from scratch. Do not commit between the two
  runs: when HEAD has moved since the build, a run without `--fresh` fails at step 1 with
  `was built from <old>; pass --fresh to test <new>`.
- Run `--codex no-daemon` as well as the default when the change touches Codex startup,
  hooks or doctor.
- When the change touches Doctor, the session-start hook or the update check (Namedrop
  1.3.0 and later), run the update notice probe on a profile kept with `--keep`:
  `.claude/skills/verify/scripts/update-notice-probe.py test-link-<checkout name>`. It
  writes an "update available" cache, then checks that Doctor shows `ND-UPDATE-001` as
  info and keeps its "checks passed" summary, that a new Claude session shows the line
  and keeps it out of the agent's context, that Codex stays quiet the same day and shows
  it the next, that invalid settings show `ND-UPDATE-002`, and that the opt-out turns it
  all off. It restores the cache and settings at the end and exits 0 when all eight
  checks pass. It takes about 30 seconds.
- When the change touches the prompt hook, the agent wrappers, delivery or the mailbox
  wake (Namedrop 1.3.3 and later), run the typed mention probe on a profile kept with
  `--keep`: `.claude/skills/verify/scripts/typed-mention-probe.py test-link-<checkout name>`.
  The checklist asks each agent to send its mention; this types a leading mention into
  the pane the way a person does. It checks that a mention typed into Codex gets a
  delivery attempt within 15 seconds and its reply settles, then that a mention typed
  into Claude Code is sent once and replied to once, with no repeat in the 45 seconds
  after. Other agent activity sends a stalled delivery and hides the stall, so it waits
  for both agents to be idle and prompts nothing else meanwhile. It leaves Claude's input
  box empty and exits 0 when both checks pass. It takes about 75 seconds when it passes.
- For anything the harness does not assert, pass `--keep` and drive the kept profile by
  hand with [references/manual-drive.md](references/manual-drive.md): pane reads, key
  presses, prompts, popups, Codex's log and pid file.

## What the harness does not reach

A change to any of these needs a drive on a kept profile beyond the checklist, with
Namedrop run through the profile's `env -i` line as the manual drive notes show:

- Stale runtime (`ND-SETUP-004`, `src/installed_runtime.rs`): replace
  `<plugin_root>/bin/namedrop` with a copy plus one trailing newline. Doctor shows
  `ND-SETUP-004` (notice) with `sh <plugin_root>/scripts/setup.sh activate --human` as the
  repair; run it with `herdr pane run` in the shell pane, since activation refuses to run
  outside a workspace pane, and the issue clears.
- Groups (`src/groups.rs`): `group set --input-json '{"alias":"pair","members":["claude","codex"]}'`,
  then `group list` and `group remove`, all with `--herdr-pane` set to the shell pane.
- Pause (`src/controls.rs`): `space pause --input-json '{}'` adds `ND-SPACE-001` to Doctor;
  `space resume` clears it.
- Cross-workspace mentions, tracked handoffs across workspaces, remote terminal commands
  and the dashboard (this branch only): the model-free native recipes in
  [native/README.md](native/README.md). For real agents, run
  `.claude/skills/verify/scripts/second-workspace.py test-link-<checkout name>` on a profile
  kept with `--keep`, then `.claude/skills/verify/scripts/cross-workspace-probe.py` with the
  same profile. The first adds a workspace labelled `beta` on its own repository with a
  real Claude Code `@lead` and a real Codex `@codex` in about 5 seconds; a rerun reuses
  them. The probe types four mentions as a person and checks Namedrop's rows in both
  workspaces: a bare `@lead` from the kept workspace's Claude persists nothing and the hook
  tells the agent `#beta @lead`; `#beta @lead` from Claude, with the blocked prompt left in
  its input box, is bridged once, answered once, comes back to Claude and is not repeated
  within 45 seconds; `#beta @lead` from Codex reaches beta within 15 seconds and its reply
  comes back; `#all @codex` from Claude brings both workspaces' `@codex` replies back to
  Claude. Every delivery must settle. It empties Claude's input box at the end, takes
  about 2.5 minutes and exits 0 when all six checks pass.
- No route written yet: tracked handoffs inside one workspace and the 30-second settle
  (`src/handoff.rs`, `src/coordination.rs`, `ND-DELIVERY-003`, `ND-COORDINATION-001`), popups other than
  Doctor's (`src/naming_ui.rs`, `src/agent_status_ui.rs`), the tour (`src/tour.rs`), shared
  checkouts (`ND-INSPECT-001`), rename and personas. The native recipes for them, last
  reconciled at 1.0.2, are in git: `git show 1b1d32a:.agents/skills/verify/features/README.md`.

## Report

Use this report shape: `## Verification: <what changed>`, **Verdict**,
**Claim**, **Method** (this skill, the `testbed/testbed test` command line and profile),
`### Steps` with one ✅ line per passed harness step quoting its `detail`, 🔍 lines for
the probes, then **Screenshot / sample** (the compatibility line and one pane capture)
and `### Findings`. Paste the step lines and the compatibility line inline; a path alone
is not evidence for a reader on another machine. A failed harness step is ❌ and the
verdict is FAIL until it passes or is explained.

## Cleanup

A pass destroys its profile unless `--keep`. After a failure or `--keep`:

```sh
testbed/testbed destroy --profile test-link-<name> --yes
```

Where the testbed cannot run, follow `testbed/CHECKLIST.md` by hand and use the manual
drive notes for the pane mechanics.
