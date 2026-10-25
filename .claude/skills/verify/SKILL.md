---
name: verify
description: Prove a Namedrop change on real Claude Code and Codex panes by running the maintainer's testbed checklist with one command and reporting its evidence. Use for /verify, "does this branch work with real agents", or before a pull request that changes runtime behavior merges.
---

# Verify Namedrop with real agents

Namedrop's runtime surface is Herdr panes with real agents in them. Unit tests and
`cargo test` are not evidence here. The handle is the maintainer's testbed at
`~/Projects/namedrop-testbed`, a local kit that builds the branch into an isolated Herdr
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
not tested, and the kit says so in a warning.

## Doctor

```sh
cd ~/Projects/namedrop-testbed && ./testbed check && ./testbed list
```

`check passed` means the tools, Python and socket path are fine and the kit's offline
self-test passes. `list` shows every profile and whether it is running; a leftover
`test-*` profile from a failed run or `--keep` is yours to inspect or destroy.

## Launch and drive

One command does CHECKLIST.md with no human:

```sh
cd ~/Projects/namedrop-testbed && ./testbed test --link "$WORKTREE" --fresh --yes
```

`$WORKTREE` is the Namedrop checkout under verification. A fresh run takes about 3
minutes and a rerun of a kept profile about 1.5; the two mention round trips take the
longest, the build about a minute, and the run gives up after 20 minutes. The default is
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
  (`<profile>/logs/test-<stamp>/NN-step/` with pane captures, doctor JSON, database rows
  and transcript summaries), keeps the profile and its server, and prints the
  `./testbed attach` and `./testbed destroy` commands. Read the evidence, then destroy.
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
- For anything the harness does not assert, pass `--keep` and drive the kept profile by
  hand with [references/manual-drive.md](references/manual-drive.md): pane reads, key
  presses, prompts, popups, Codex's log and pid file.

## Report

Use the bundled `/verify` report shape: `## Verification: <what changed>`, **Verdict**,
**Claim**, **Method** (this skill, the `./testbed test` command line and profile),
`### Steps` with one ✅ line per passed harness step quoting its `detail`, 🔍 lines for
the probes, then **Screenshot / sample** (the compatibility line and one pane capture)
and `### Findings`. Paste the step lines and the compatibility line inline; a path alone
is not evidence for a reader on another machine. A failed harness step is ❌ and the
verdict is FAIL until it passes or is explained.

## Cleanup

A pass destroys its profile unless `--keep`. After a failure or `--keep`:

```sh
cd ~/Projects/namedrop-testbed && ./testbed destroy --profile test-link-<name> --yes
```

Without the testbed on this machine, follow `CHECKLIST.md` in that repository by hand
and use the manual drive notes for the pane mechanics.
