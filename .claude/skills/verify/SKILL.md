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
in the thread, the pointer seen exactly once by the recipient with its proof marker);
`echo hello` on `@shell` with exit 0; nothing awaiting progress at the end. Each agent
settles a delivery by its final message: the request hop ends `disposition reply` and the
reply hop `no_reply`, each with `claim done:…`, `settle commands 0`, one model call per
delivery turn, a reply body equal to the target's final answer, and no live claims left. It does not
show the update notice (Namedrop 1.3.0 and later): a `--link` install never checks for
updates. Use the update notice probe under **Push on it** for that.

## Daily-use lane

The checklist run proves one ping and pong. Run this lane as well when a change touches
delivery, the hooks, final-message settling, Channels, handoffs, sessions or anything a
person meets during a working day:

```sh
testbed/testbed test --link . --fresh --yes --daily-use
```

It uses the same isolated profile, real Claude Code and Codex, report and evidence
directories as `test`, with Codex always `--no-daemon`. A fresh run takes 25 to 30 minutes, about
10 of them in session continuity, and has a 40-minute budget after the build. A failed setup step skips the rest. A
failed check doesn't stop the next one, because a run on a buggy build should report
everything it finds, and exit 1 means at least one check failed. After a failed check, a
`quiesced:` line says what the lane interrupted and closed before moving on. Closing clears
a delivery's item and Doctor's code, so the lane reads both first. Each answer it finds lost
gets a `lost answer` line that says `warned` or `SILENT`, and both fail the run. The final
backlog repeats those lines, because its own reading comes after the cleanup. It reads
Channels the way the page does: one person's session, the page's own routes, and one
`/api/events` stream recorded for the whole run. Every check reads attention and the label
over all its frames, because a person's next post clears a false item, and the rest of the
run's stretches fail on any item a check doesn't name. Two false alarms the code raises today
are named that way and printed at the end as `known false alarms`, so a passing reader still
sees them. The lane reads what the page derives its banner, "Needs you" rows and label from;
it doesn't render the page.

| Step | What it asserts |
| --- | --- |
| channels answers | Composer post, thread reply and DM to each agent, each told to answer by final message: one delivery, one attempt, one answer in the right thread, 1 model call, 0 settle commands, no new attention, no "not sent through Namedrop" label. |
| recap | An agent asked what the other answered gets it right. The answer is in its prompt and in no hook context. |
| real warnings | An interrupted delivery raises `agent_disposition` and Doctor's `ND-DELIVERY-003`, through the same judge read the other way. Closing it clears it. |
| tracked handoff | Lead, worker, `needs_continuation`, continuation with `expected_revision`, `completed`, `handoff.resolve`, one report shown once; then a nudge. |
| held turn | A turn that ends waiting on a helper posts nothing, later prompts show the way back, and the answer lands in the same thread. Claude sends its own request. For Codex the lane sends it during the turn, because a real Codex declines to `send` from a person's delivery. Each hold prompt that the lane scripts tells the agent to use the reply line Namedrop shows. |
| session continuity | `/compact` with a held delivery, exit and resume, and a crash mid-turn, on each agent. The lane sends each hold's `@shell` request during the turn. |
| other routes | `second-workspace.py` and `cross-workspace-probe.py`, and two reminders. |
| natural wording | A new session of each agent gets a natural question. One answer and no new attention are judged. Its model calls and settle commands are shown, not judged, and it runs last because a session that has run its reply line keeps doing so. A new Codex then takes a hold with no instructions on how to answer. The report says whether its answer landed, as a `FINDING` line if it didn't, without failing the lane. |
| backlog | Both workspaces: nothing awaiting, no live claims, no open attention, coordinations or reminders, Doctor healthy. |

A run on a different commit uses this kit against a detached worktree:
`git worktree add --detach <path> <commit>`, then `--link <path>`. The lane handles the whole
envelope prompts of 1.6.x and the pointer prompts of 1.7.0.

### Upgrade check

```sh
testbed/testbed test --link . --fresh --yes --upgrade-check
```

It installs the newest `v*` release below the checkout's version from GitHub, or
`--upgrade-from REF`, in a profile named
`test-upgrade-link-<checkout name>`. It leaves a delivery open on both agents mid-tool, links
and activates the build under test, about 13 minutes in all, and checks the version rose, the agents stayed up, the
open deliveries settle once and a fresh message works in the old sessions. The old sessions'
model calls are reported and never fail it. A new Codex session then shows whether starting
over recovers one the update stranded.

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
- When the change touches the Stop hook, final-message settling or how a message treats its
  names (Namedrop 1.6.3 and later), run the final message probe on a profile kept with
  `--keep`: `.claude/skills/verify/scripts/final-message-mention-probe.py test-link-<checkout name>`.
  Each agent asks the other for a one-line answer that names an agent that isn't here, names
  one that is, opens with a name, or opens with the asker's own name. It checks that each answer arrives as a reply to the asker
  alone and that its claim ends `done:replied`. It takes about 5 minutes.
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
- Tracked handoffs across workspaces, remote terminal commands and the dashboard (this branch
  only): the model-free native recipes in [native/README.md](native/README.md). The daily-use
  lane runs the cross-workspace scripts below; to run them on their own, for real agents, run
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
- Final message as the reply (`src/final_reply.rs`, `src/stop_hook.rs`): the daily-use lane
  covers a person's message answered by the final message and a turn held on a helper. Drive
  this one on a kept profile: a Stop that another Stop hook blocks (settles at the turn's
  second Stop, with the real final text).
- No route written yet: an ignored nudge and `ND-COORDINATION-001`, popups other than Doctor's (`src/naming_ui.rs`,
  `src/agent_status_ui.rs`), the tour (`src/tour.rs`), shared checkouts (`ND-INSPECT-001`), rename
  and personas. The native recipes for them, last reconciled at 1.0.2, are in git:
  `git show 1b1d32a:.agents/skills/verify/features/README.md`.

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

## Channels browser app

Use this driver for `experimental/channels` lanes. Run every testbed command from
this checkout. Set `CHANNELS_OUTPUT_DIR` to your lane's output directory. Its default
is `/tmp/swarm-${PR_ID:-ui1}/worker-${WORKER_N:-1}/`. Build the app, then boot an
isolated profile. `TESTBED_HOME` selects the testbed root.

```sh
export CHANNELS_OUTPUT_DIR="${CHANNELS_OUTPUT_DIR:-/tmp/swarm-${PR_ID:-ui1}/worker-${WORKER_N:-1}}"
mkdir -p "$CHANNELS_OUTPUT_DIR"
export CHANNELS_OUTPUT_DIR="$(cd "$CHANNELS_OUTPUT_DIR" && pwd)"
export TESTBED_HOME="${TESTBED_HOME:-$HOME/.namedrop-testbed}"
(cd experimental/channels && npm ci && npm run build)
testbed/testbed up --link "$PWD" --fresh --yes
```

Use `--no-launch` for lanes that do not prompt real agents. Do not use
`testbed/testbed attach`. The profile is `link-<checkout basename>`.
Read its generated `env.sh` to build the wrapper below. The socket assertion runs
before the first Herdr command. Keep all Herdr and Namedrop calls inside `tb`.

```sh
export CHANNELS_OUTPUT_DIR="${CHANNELS_OUTPUT_DIR:-/tmp/swarm-${PR_ID:-ui1}/worker-${WORKER_N:-1}}"
mkdir -p "$CHANNELS_OUTPUT_DIR"
export CHANNELS_OUTPUT_DIR="$(cd "$CHANNELS_OUTPUT_DIR" && pwd)"
export TESTBED_HOME="${TESTBED_HOME:-$HOME/.namedrop-testbed}"
tb_profile="$TESTBED_HOME/profiles/link-$(basename "$PWD")"
tb_vars=$(sed -n 's/^ *env -i \(.*\) \/bin\/bash --noprofile.*/\1/p' "$tb_profile/env.sh")
test -n "$tb_vars" || exit 1
tb() { eval "env -i $tb_vars \"\$@\""; }
tb_socket=$(tb printenv HERDR_SOCKET_PATH)
case "$tb_socket" in /tmp/nd-tb-*) ;; *) echo "Not a testbed socket" >&2; exit 1 ;; esac
tb herdr pane list
tb_workspace=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["workspace"])' "$tb_profile/state.json")
tb_state="$tb_profile/state/herdr/plugins/namedrop"
tb "$tb_state/runtime/namedrop" --plugin-state "$tb_state" --herdr-workspace "$tb_workspace" channels --no-open --json --ui "$PWD/experimental/channels/dist" > "$CHANNELS_OUTPUT_DIR/channels-dashboard.json"
```

Read `data.url` from `$CHANNELS_OUTPUT_DIR/channels-dashboard.json`. The nonce is single use.
Navigate once with a fresh browser context. The redirect redeems it for the session
cookie, which the app's same-origin EventSource uses automatically.

For three channels, create `alpha` and `beta` through the wrapper, each in its own
profile directory. Use `tb herdr workspace create --cwd <directory> --label alpha
--no-focus`, then repeat for `beta`. Name a member in each using Namedrop through
`tb` with that workspace and pane. For real agent coverage, use the existing
[second workspace helper](scripts/second-workspace.py) and
[manual drive notes](references/manual-drive.md), with the same socket assertion.
Use `tb herdr agent prompt <pane> <task>` for presence changes and `tb herdr
workspace close <workspace>` for removal. Never address the real Herdr socket.

Run the browser block from `experimental/channels`. Run the other shell blocks from
the repository root. Each block sets its own defaults and works in a fresh shell.
Use an absolute `CHANNELS_OUTPUT_DIR` to share artifacts between working directories.
Set `CHROME` to the installed Chrome or Chromium executable if it is absent from `PATH`.
This browser probe uses only the pinned `playwright-core` package. It saves a screenshot and performance
entries in `CHANNELS_OUTPUT_DIR`.

```sh
export CHANNELS_OUTPUT_DIR="${CHANNELS_OUTPUT_DIR:-/tmp/swarm-${PR_ID:-ui1}/worker-${WORKER_N:-1}}"
mkdir -p "$CHANNELS_OUTPUT_DIR"
export CHANNELS_OUTPUT_DIR="$(cd "$CHANNELS_OUTPUT_DIR" && pwd)"
export TESTBED_HOME="${TESTBED_HOME:-$HOME/.namedrop-testbed}"
CHANNELS_URL=$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["data"]["url"])' "$CHANNELS_OUTPUT_DIR/channels-dashboard.json")
export CHANNELS_URL
export CHROME="${CHROME:-$(command -v chromium || command -v google-chrome || command -v google-chrome-stable)}"
node --input-type=module <<'JS'
import assert from 'node:assert/strict';
import { writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { chromium } from 'playwright-core';
const output = process.env.CHANNELS_OUTPUT_DIR;
assert.ok(output, 'CHANNELS_OUTPUT_DIR must be exported');
const browser = await chromium.launch({executablePath: process.env.CHROME, headless: true, args: ['--no-sandbox']});
try {
  const context = await browser.newContext({viewport: {width: 1024, height: 700}});
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', error => errors.push(error.message));
  await page.goto(process.env.CHANNELS_URL);
  const channels = page.getByRole('navigation', {name: 'Channels', exact: true});
  await channels.getByRole('button').first().waitFor();
  const alpha = channels.getByRole('button').filter({has: page.locator('.row-name', {hasText: /^alpha$/})});
  await alpha.click();
  await page.getByRole('heading', {name: 'alpha', exact: true}).waitFor();
  await alpha.focus();
  await page.keyboard.press('End');
  await page.keyboard.press('Home');
  await page.waitForFunction(() => performance.getEntriesByName('namedrop:load-to-sidebar-paint').length > 0);
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await page.screenshot({path: join(output, 'ui1-channels.png'), fullPage: true});
  await page.setViewportSize({width: 1600, height: 1000});
  await page.emulateMedia({colorScheme: 'dark'});
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth > innerWidth), false);
  await page.screenshot({path: join(output, 'ui1-dark.png'), fullPage: true});
  const measures = await page.evaluate(() => performance.getEntriesByType('measure').map(entry => entry.toJSON()));
  await writeFile(join(output, 'ui1-measures.json'), JSON.stringify(measures, null, 2));
  assert.deepEqual(errors, []);
} finally {
  await browser.close();
}
JS
```

Use these selectors for later live lanes.

For UI-2, send the CHECKLIST Claude-to-Codex mention in the isolated profile before
opening the app. Wait for its reply in Namedrop's database, then open that channel.
The parent shows the reply in a preview button named **1 reply from @codex at
<time>. Open thread**. Click it and confirm the reply appears in
`getByRole('complementary', {name: 'Thread', exact: true})`. Send another reply
while the pane is open and confirm its text, the preview, and the reply count
update without a reload.
Save `ui2-exchange.png` and `ui2-thread-live.png`. Run the coordination exchange in
the plan's first lane and save its report card as `ui2-coordination.png`.

Seed 400 rows through the profile's Namedrop commands. Scroll `.timeline`, the
**Message history** region, to the top to load older history. Before each page, record the first visible
`article[data-message-id]` and its offset from the scroller's top. After the page,
require that same row to stay at the same offset within two pixels. Keep scrolling
until the first seeded row appears. Confirm each message ID appears once and that
switching channels ten times never mixes their rows. Save `ui2-history.png` and
`ui2-switch.png`, then repeat with the copied live database for `ui2-real.png`.

Send fenced code, an HTTP URL, a long body, and the literal body
`<img src=x onerror=alert(1)>`. Confirm code uses a monospace font, the URL has
`rel="noreferrer"`, **Show more** reveals the available long text, and no image or
dialog appears. A server-truncated body can reveal only the text the server sent.
Pause the testbed space with a mention queued and confirm its receipt says
**Held while paused** and the channel header says **Paused · deliveries held**. Save the format, long-message, hostile-text, and held-receipt screenshots
named in the plan. Keep all commands inside the `tb` wrapper above.

For UI-3, post through the composer only. Its box is
`getByRole('combobox', {name: 'Message #alpha'})`, or `Message <display name>` for a
channel without a mention label. In the thread pane it is named `Reply to @codex`, or
`Reply to @lead · #alpha` when the agent lives in another workspace, and `.composer-to`
shows the same recipients after "To". A post that names nobody stays in the box with
`.composer-guard` under it and never reaches the server.
Type with `pressSequentially` so each key fires an input event. `#` and `@` open
`getByRole('listbox', {name: 'Channels'})` or `{name: 'Members'}`. The combobox's
`aria-activedescendant` names the highlighted option. A disabled option carries
`aria-disabled="true"` and its reason in `title`. Each post shows as
`article.pending[data-request-id]` in the timeline, or at the end of the thread pane,
until its row arrives in a frame. A refused post shows its error as
`.composer [role="alert"]`. Read each posted row and its `in_reply_to` with
`sqlite3 -readonly` on the profile's `namedrop.sqlite3`. Two quick Enter presses must
leave one row with that text.

- `getByRole('navigation', {name: 'Channels', exact: true})` holds every channel.
  Channel buttons carry `data-channel-key` with the opaque key. `.row-name` holds
  the display name. Same-name buttons add `.row-suffix` with
  ` · workspace_id · full-space-key`. Each suffix stays fixed when more same-name
  channels arrive. The channel header's `.token` holds the `#token` or **No mention
  label**. The selected channel's URL is `/w/<key>`.
- `getByRole('navigation', {name: 'Needs you', exact: true})` holds channels with `health.level === "needs_you"`.
  It is absent when no channel needs you. Each row shows `.row-reason`, and its dot
  has the accessible name **Needs you**. `getByRole('region', {name: 'Attention
  needed'})` is the banner in the selected channel.
- `getByRole('list', {name: 'Members'})` holds member chips. Each `li` carries
  `data-address-id`, and its `.presence` badge carries `data-activity`. Activity
  values are `working`, `waiting`, `blocked`, `idle`, `offline`, and `unknown`.
  Retired members are hidden. The operator chip reads **You** and has no badge.
- `getByRole('status')` exposes the reconnect banner. Kill and restart only the
  profile's dashboard, then wait for this banner to disappear and the channel
  collection to reconcile at `ready` without reloading the page. A server without
  `ready` resumes live updates after a two-second fallback that keeps old channels.
  Delay `ready` past that interval and confirm it still removes omitted keys. Before
  first discovery, an empty fallback keeps the loading state until a channel commits
  or `ready` confirms the empty view. A first late `ready` applies the landing rule
  (URL key, first Needs-you channel, latest message) unless you selected a channel
  during fallback.
  Select a channel,
  focus its canonical Channels button, and create unread activity elsewhere before
  dropping the stream. Delay replay to check that the sidebar stays populated.
  After `ready`, confirm selection, unread marks, and focus survive for retained
  keys, then press an arrow key. A selected channel clears its unread mark only
  after 1.5 seconds on screen. Confirm omitted keys disappear and an empty ready
  set clears the view. The app opens exactly one `/api/events` EventSource per tab.
- `performance.getEntriesByName('namedrop:keystroke-to-popup', 'measure')` and
  `'namedrop:enter-to-sent'` time the composer. `experimental/channels/README.md`
  documents where each starts and ends. Each keeps at most 200 entries.
- `performance.getEntriesByName('namedrop:activity-derive', 'measure')` times the
  All activity derivation on every frame, and `'namedrop:click-to-peek-text'` times
  a peek from the opening click or key to the frame after its first screen text
  paints. `experimental/channels/README.md` documents where each starts and ends.
  Each keeps at most 200 entries.
- `performance.getEntriesByName('namedrop:frame-apply', 'measure')` measures reducer
  calls, including replay timeout reconciliation. `namedrop:load-to-sidebar-paint`
  measures navigation start to the first
  populated sidebar after two animation frames. The app retains at most 200
  reducer measures and one load measure per navigation.

For UI-2's performance lane, read
`performance.getEntriesByName('namedrop:frame-to-message-paint', 'measure')`.
`experimental/channels/README.md` documents its start and end. Use 20 interleaved trials with UI-1's sidebar
update as the baseline. Record a five-second scroll trace with 400 loaded rows.
Frame-to-message-paint p95 must stay below 100 ms, and the scroll trace must contain
no main-thread task longer than 50 ms. Record the exact app commit and database for
both measurements. Passing browser tests does not establish these budgets.

Save each lane's screenshots and measurements under `CHANNELS_OUTPUT_DIR`. Record the
channel count, member presence transition, duplicate-name suffixes, keyboard focus,
reconnect recovery, and dark surfaces against the plan's lanes. Use a fresh nonce
and context for each navigation trial. The performance lane reads both measures
across ten loads of 20 workspaces with 10 members. It compares the old dashboard's
first participant paint before checking the plan's p95 limits. Do not infer a
performance pass from the npm checks.

Destroy the profile when the lane finishes.

```sh
export CHANNELS_OUTPUT_DIR="${CHANNELS_OUTPUT_DIR:-/tmp/swarm-${PR_ID:-ui1}/worker-${WORKER_N:-1}}"
mkdir -p "$CHANNELS_OUTPUT_DIR"
export CHANNELS_OUTPUT_DIR="$(cd "$CHANNELS_OUTPUT_DIR" && pwd)"
export TESTBED_HOME="${TESTBED_HOME:-$HOME/.namedrop-testbed}"
testbed/testbed destroy --profile "link-$(basename "$PWD")" --yes
```
