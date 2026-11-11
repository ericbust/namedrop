# Namedrop Channels (Beta)

Channels is a **Beta** feature included in the normal Namedrop installation on `main`.
The binary serves the embedded app without Node or npm. This directory holds its frontend source.

The app shows live Herdr workspaces as channels, with messages, threads, members,
presence, direct messages, reminders, and attention requests. You can post to a channel or reply in a thread as the
operator. It needs the dashboard's all-workspace stream, its operator post route, and
`--ui` support. It does not change a workspace's panes or labels.

## Check and build the app

Use Node 26.7.0 and npm 11.19.0. From `experimental/channels`, run these commands.

```sh
npm ci
npm run lint
npm run typecheck
npm test
npm run build
npm run bundle:check
```

`npm test` uses Node's built-in test runner with TypeScript stripping. It runs in
one process so every test registers in the same runner. `npm run build` emits
`dist/index.html` and external JavaScript and CSS. Fonts come from the system.
Scripts and bundled styles load from the dashboard. xterm also creates inline
styles, which the dashboard permits with `style-src 'self' 'unsafe-inline'`.
Scripts remain restricted to `script-src 'self'`. No remote assets are required.

Run the committed browser regressions from the same app directory. They build the
actual app and serve an isolated local SSE fixture under the dashboard CSP.

```sh
CHROME=/opt/google/chrome/chrome npm run test:browser
```

Set `CHROME` to your installed Chrome or Chromium executable. The headless runner
uses `--no-sandbox` and needs permission to bind a local test port. It covers focus
restoration, late `ready`, slow discovery, replay mismatches, old servers without
`ready`, and replay removal. It also covers the composer against a local fake of
`POST /w/<key>/messages` that checks the session cookie and CSRF header, keeps request
IDs idempotent, and returns the server's error codes.

After changing the app, run `npm run bundle` to regenerate the embedded assets in
`src/dashboard/channels` and `src/dashboard/channels_bundle.rs`. Commit these
files with the frontend change.
`npm run bundle:check` checks that the committed assets match the build.

The optional, path-filtered **Channels experiment (optional)** workflow runs these checks.
It adds no required-check configuration. Cargo excludes `/experimental/**` from
the package.

## Serve and drive the app

From the repository root, use the isolated profile procedure in the
[channels driver](../../.agents/skills/verify/SKILL.md#channels-browser-app).
Build the app first, then serve it from that profile.

```sh
namedrop channels --no-open --json --ui /absolute/path/to/experimental/channels/dist
```

Run that command through the driver's `env -i` wrapper. Use the returned nonce URL
in headless Chrome through the pinned `playwright-core` package. Vite's development
server can serve assets with `npm run dev`, but it does not provide the authenticated
Namedrop API.

[`DESIGN.md`](DESIGN.md) is the design contract, and `src/style.css` starts with
its tokens. The **Channel list** and **Needs you** navigation regions expose buttons
by workspace name. Display names have no mention prefix, and sidebar rows do not
repeat the `#token`. The channel header shows the exact `#token`, or **No mention
label**. Duplicate names add a muted suffix with the workspace ID and the full
20-character space key. These suffixes stay fixed when more same-name channels
arrive. The **Needs you** section lists each channel that needs you with the
reason, and it is hidden when nothing needs you.
Arrow keys, Home, and End select and focus channel buttons inside each region. Tab
moves between regions. The app opens the channel in the URL (`/w/<key>`), else
the first channel that needs you in sidebar order, else the channel with the
latest message. Until you pick a channel, each newly arrived channel re-runs that
rule, so the app moves to the URL's channel or to Needs you as soon as it
appears. An update to a channel already present never moves the selection. A
click pushes a history entry for `/w/<key>` and Back selects the previous
channel. An automatic selection never rewrites the URL. The tab title reads
**(N) Needs you · Namedrop** while channels need you.
Closing the selected workspace leaves **Choose a channel** when other
channels remain. OS dark mode switches every token to the indigo night theme.

## Read messages and threads

The timeline groups consecutive messages by author within five minutes and
inserts day dividers. A channel message with replies shows a reply count and the latest reply in a
preview box. Replies stay in their thread when the parent message is loaded.
The preview and reply count open the **Thread** pane. Replies whose parent is
not loaded remain visible as **Earlier thread** rows.
Under each channel-chat message a route line says who got it and why, with the
receipts until they answer, and offers **Send anyway** for a mentioned agent the
post did not reach. `routeLine()` in `src/route.ts` holds that line's rules. The
preview, the reference, and a post's reply count open the **Thread** pane, which
updates when replies arrive. Opening a
thread moves focus to **Close thread** once, and switching channels closes the
pane. When none of the thread's messages are loaded any more, after Jump to
latest or when the history cap evicts them, the pane closes for good and focus
moves to the timeline. The preview exposes its expanded state and pane ID.
**Close thread** restores focus to that preview. Escape does the same from
inside the pane, or from the page body when no dialog is open; other focused
controls keep their own Escape behavior. On narrow screens the pane sits below
the timeline. Replies whose parent is not loaded remain visible as **Earlier
thread** rows.

Scroll to the top or select **Load earlier messages** to fetch history. The first
visible message keeps its position across prepends, live updates, and channel
switches. Disjoint live windows show a gap and load missing pages automatically.
The visible window holds at most 600 messages per channel: four live windows or
twelve history pages, preserving the established 400-message history case. Live
arrivals discard the oldest edge and keep it reachable through **Load earlier
messages**. Paging past the cap discards the newest edge and enters browsing
mode. While browsing, live frames refresh overlapping receipts and the latest
reset source without restoring discarded rows or creating an artificial gap.
**Jump to latest** resets to the latest snapshot and scrolls to the bottom.
Failed history requests offer **Try again**. The member strip above the timeline
wraps to two lines and folds the rest behind **+N more**.

Message bodies render supported Markdown, including emphasis, headings, lists,
tables, and fenced code. Collaborator text never becomes arbitrary HTML. HTTP and
HTTPS URLs become links with `rel="noreferrer"`. Bodies over 480 characters have
**Show more**, which can load the full sanitized body up to 64 Ki characters.
Preview boundaries preserve surrogate pairs. Links are recognized
before shortening, so a shortened label retains its complete destination and the
ellipsis stays outside the link. A server truncation note identifies text omitted
before delivery. A message to a single terminal renders as a command.
`terminalResult` in `wire.ts` parses a terminal's reply into its exit code,
stdout, and stderr, so the reply shows stdout as monospace output, a non-empty
stderr under a **stderr** label, or **No output**. The reply preview shows the
first two lines. **Exit N** uses a terminal run whose `message_id` matches the
request, else the parsed exit code. A reply that does not parse renders as
monospace text with no **Exit N**. Receipts use the plain words in
`src/copy.ts` and hide once every recipient has replied.
Unknown message kinds and delivery stages use neutral labels.

## See all activity and peek at agents

**All activity** sits above **Needs you** in the sidebar, outside the channel
regions' arrow keys, with the number of busy agents. It has its own address,
`/?view=activity`, so reload and Back return to it, and a URL that names it ends
landing. It lists every working, waiting, or blocked agent across channels in the
groups **Needs you**, **Waiting**, and **Working**. Each row's second line shows the
member's attention item, who it waits on, or its oldest mailbox item, and the row's
channel button opens that channel at its newest message. Every channel collects
unread marks while the view is shown.

Member chips, names in message headers, and activity rows open the peek drawer in
the thread pane's slot, so the drawer and a thread never show together. The drawer
opens in **Live** mode. An agent in another workspace is viewed in its home channel
under its own address. Escape closes the drawer and returns focus to the control
that opened it.

Live shows a passive terminal projection with colors, cursor motion, and TUI
redraws. Its grid follows the source terminal, including source resizes, when its
PTY dimensions are available. Reading those dimensions does not change the source.
If only source row metadata is available, Live follows those rows. Older Herdr
builds that reject size flags use their default projection. Scroll the projection
locally to see a wider grid. Opening
or resizing Peek sends no terminal input, source resize, focus, or takeover command.
The browser uses pinned xterm 6.0.0. **Type to agent** is a separate, explicit
input control for prompts, Esc, Ctrl-C, arrows, and Enter. Input requires a
current live continuity value and agent binding. It goes to the real pane.
Live terminal paints can contain secrets. Text snapshots and fetched history
redact recognized secrets, but redaction cannot catch every secret.

Unusable geometry hints fall back to source row metadata, then observer defaults.
Positive sizes clamp to 512 columns, 256 rows, and 65,536 cells. A watch caches its
discovered TTY path, reads dimensions without retaining an open descriptor, and
retries failed path discovery at most once per second.

Live displays terminal content rather than the Text snapshot route's redacted
text. Select **Text snapshot** for that sanitized plain-text alternative. Text
reads `GET /w/<key>/participants/<address>/screen` on selection and on **Refresh**
or **Try again**, never on a timer. It retains the existing bounded snapshot
cache, with one read at a time.

Live retries eligible interruptions after 1, 2, and 4 seconds, at most three times
per open observation. Each retry starts with a complete paint and retains the
same continuity value. A confirmed unsupported Live capability before the first
paint selects Text snapshot automatically. After Live establishes continuity,
errors never trigger a silent snapshot read or follow a replacement target.
In Live mode, **Try again** starts a deliberate new observation. Switching away
from Live and back, or reopening Peek, also starts a new observation. A dashboard
session failure or replacement clears the live terminal. An ended stream says
**Live view ended**, without asserting that the agent exited.

Live opens at the latest text or cursor row in the projected screen, above any
unused blank tail. Mouse wheel, pixel trackpad, keyboard scrolling, and both
scrollbars move the browser view locally. Scrolling
up pauses following while output arrives. Returning to the latest output resumes
following. Full paints and reconnects preserve your local position where possible;
a new target or reopened Peek starts at the latest output.

The projection contains the source terminal's current grid. The server also
passively reads available `recent-unwrapped` history, with up to 2,200 lines and
1 MiB of sanitized text, and refreshes it while observing. The browser retains
output that scrolls by while you watch. **Watching since** shows when that
history began. A quick reopen retains it for five minutes. Unrecoverable text
shows a gap marker. Local scrolling does not scroll the source application.

## Update Channels

Use [Update Namedrop](../../README.md#update-namedrop) for the installation's
chosen ref. Normal installations follow `main`. Refresh or reopen Channels
after an update. If the dashboard session ended, run `namedrop channels` for
a fresh one-time link. Start fresh agent sessions to load new operations such
as reminders. Existing sessions keep their loaded instructions.

The strict `npm run test:live-peek` browser check runs Chromium headlessly with
visible native scrollbars. On Linux it runs official Playwright Firefox on a
private Weston headless display, because Firefox's headless test agent hides
scrollbars. The fixture requires an existing `weston` executable. It creates and
removes its own display socket, HOME, and browser profile without connecting to
your desktop. `CHROME`, `FIREFOX`, and `PLAYWRIGHT_BROWSERS_PATH` select existing
browser installations. `LIVE_PEEK_OUTPUT` selects the evidence directory.

`npm run test:source-rows` drives an existing isolated testbed profile with an idle
Codex pane, which it sizes to 80 rows. Set `PEEK_SOURCE_PROFILE` to its `test-*` profile
directory and `PEEK_SOURCE_OUTPUT` to a screenshot directory. The test compares the
native prompt with Live Peek in Chromium and WebKit at scales 1 and 2, then grows
and shrinks the source terminal. It controls only that test profile's terminal.
`PEEK_SOURCE_TEXT` overrides the expected Codex prompt text.
Set `PEEK_SOURCE_ALIAS=shell` to test the shell pane. A prepared marker at column
190 can be checked with `PEEK_SOURCE_COLUMNS=200`, `PEEK_SOURCE_TEXT=COL190S`, and
`PEEK_SOURCE_COLUMN_MARKER=COL190S`. The regression also grows and shrinks the source
width while preserving the marker's column.

## Live terminal stream v1

The browser calls `observePeek({target, signal, retainedContinuity, apply, onState})` from
`src/livePeek.ts`. That operation owns fetch parsing, continuity, retries, the
30-second stream watchdog, and abort. `apply` receives reset and data updates as
`Uint8Array` bytes, or a history update with sanitized text. It resolves after
xterm drains each update. A large reset uses
sequential writes of at most 64 KiB. The view becomes Live after the complete
paint drains. Target changes, mode changes, and close abort the observation and
dispose the emulator. Live bytes do not enter React or the Channels store.

The endpoint is:

```text
GET /w/<key>/participants/<address>/live[?continuity=<opaque-value>]
```

It uses the dashboard session cookie and existing Host and provenance checks.
The only query key is `continuity`; the endpoint accepts no body or terminal
control commands. It returns `application/x-ndjson` with no-store and the
dashboard security headers. Blank lines are heartbeats and have no sequence
number. Each JSON record has `v: 1` and a positive, contiguous, response-local
`seq` starting at 1.

| `kind`        | Other fields                          | Meaning                                                                                                                                                                   |
| ------------- | ------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `reset`       | `continuity`, `cols`, `rows`, `bytes` | Replace all emulator and decoder state with the complete base64 ANSI paint at this viewport size. A later full paint produces another reset, even at the same dimensions. |
| `history`     | `text`                                | Add sanitized available history, limited to 2,200 lines and 1 MiB.                                                                                                        |
| `data`        | `bytes`                               | Apply ordered base64 ANSI bytes after a reset. Data may split UTF-8 or escape sequences across records.                                                                   |
| `closed`      | `reason`                              | End with `ended`, `replaced`, or `missing`. Native diagnostic strings are not part of the contract.                                                                       |
| `unavailable` | `reason`                              | End with `unsupported`, `busy`, `source_lost`, `invalid_source`, `slow_viewer`, or `server_stopping`.                                                                     |

The viewport limits are 1 through 512 columns, 1 through 256 rows, and at most
65,536 cells.
Reset bytes decode to at most 1 MiB; data bytes decode to at most 64 KiB. An
encoded record is at most 2 MiB. The server caps pending serialized output at
2 MiB per viewer and admits at most eight viewers. It requires startup within
five seconds, ends a pending write after two seconds without progress, and emits
blank heartbeats every ten seconds when no content is pending. These are policy
bounds, not memory or latency measurements.

EOF without a final record is an interruption, even with a successful native
exit status. Retryable conditions are an interruption, startup HTTP 503,
`source_lost`, `slow_viewer`, and `server_stopping`. HTTP 401 or 403 ends the
dashboard session path, 404 means the member is missing, and 410 refuses the
current identity or continuity. HTTP 410, closure, unsupported capability,
invalid source data, and an `unavailable` record with `busy` stop automatic
retries. There is no replay cursor. A reconnect starts at sequence 1 and requires
a fresh full paint.

Continuity compares the authenticated namespace, address, active binding, native
terminal identity, optional known sessions, and dashboard instance. It is an
equality precondition, not authentication. The backend attaches only to the
resolved immutable terminal ID and rechecks recorded and native identity.
Observed changes end the view and discard unsent content. An automatic retry
cannot discard continuity to follow a replacement or survive a dashboard restart.
Reopening is an explicit read of the member's current terminal.

This guarantee is terminal-scoped. A new process or agent session can reuse the
same terminal before metadata reports the change. The stream does not provide
atomic isolation between agent sessions, and already delivered bytes cannot be
recalled.
Missing optional session metadata alone does not prevent viewing a bound terminal.

The current backend uses the Herdr TUI observer. Its private adapter validates native
frames and converts full paints, ordered deltas, and endings into this browser
contract. It owns observer processes and bounded I/O. A private compiled guard
cleans up the observer even after dashboard parent death on a quiet terminal.
A future herdr-core adapter can normalize its framed observer transport into the
same browser API. This release implements the TUI adapter.

Rust serialization owns the conformance samples in
[`tests/fixtures/live_peek/streams.json`](../../tests/fixtures/live_peek/streams.json).
Browser tests consume those records unchanged. They cover every ending and a
reset/data/reset stream; parser tests enforce versions, ordering, and bounds.

## Post messages

The composer sits under the timeline and at the bottom of the thread pane. Enter
sends, and Shift+Enter starts a new line. Typing `#` or `@` at the start of the box
or after a space opens a list of channels or members. The members come from the
channel named by the last `#token` before the caret, else the open channel. The
operator and retired members never appear. Up and Down move the highlight, Tab or
Enter accepts, and Escape closes the list. A channel without a mention label is
listed but cannot be chosen. Frames carry no groups, so `@` lists only members.
`complete()` and `accept()` in `src/complete.ts` hold these rules as pure functions.

At startup the app reads its CSRF token from `GET /api/session`. A send posts
`{request_id, source}`, plus `in_reply_to` for a thread reply, to
`POST /w/<key>/messages` with the `X-Namedrop-Csrf` header. After a 403 the app reads
the token again and retries the same request once. The store's `outbox` holds each
post by request ID. A post shows as a pending row with **Sending…**, then **Sent**
after the server accepts it. A channel post that mentions agents reads **Checking who
this is for…** while it sends, because the server asks its router before it answers.
The row that a later frame carries for that message ID replaces it. A refused post keeps its text and shows the error under the box. Sending
the same text again reuses its request ID, so the server never writes it twice.

Text that starts with `@name`, `#channel @name`, or `#all @name` goes only to the
members it names. Any other text in the channel composer is a channel post, stored
exactly as typed. A post that mentions no agent reaches no one. A post that mentions
agents goes to the ones the server's router decides it is for, and only among them.
The hint under the box says which of these Enter does. A thread reply without leading mentions is sent only when there is one eligible
agent. The app adds that agent's recipient prefix before posting. With several
agents, the hint asks you to choose and the draft stays unsent until you select
recipients with `@name`. A terminal always requires an explicit leading mention.
`src/thread-recipients.ts` holds the one-agent and multiple-agent guidance.
`postSource()` and `composerHint()` in `src/complete.ts` apply it.

Typing `/` or `$` offers skills for the addressed agent. Selecting one inserts
`/name` for Claude Code or `$name` for Codex. Their skill lists stay separate.
Skill suggestions also work in **Type to agent**.

A paste, a drop or the image button attaches up to 4 images of up to 10 MB each.
A paste that also carries text stays a text paste. The composer uploads each image
at once with `POST /api/images`, raw bytes and the `X-Namedrop-Csrf` header, and
the answer is `{name, line}`. The line is `Image: ` and the stored file's absolute
path. A draft keeps its images as such lines at the end of its text, after a blank
line, so a kept or refused draft keeps them and the post's `source` already carries
them. `splitImages()` and `withImages()` in `src/images.ts` hold that form. The
box shows only the typed text, and a chip per image from `GET /api/images/<name>`.
A frame's message row lists its images in `images` and leaves their lines out of
`body`. A message with images needs words too, so Send stays off until there are
some.

## Direct messages and reminders

**Direct messages** lists Namedrop agents across workspaces, with a channel label
beside each name. Selecting an agent opens a private operator-agent conversation.
No `@` is needed, and body mentions remain text. DMs stay out of channel timelines,
thread lists, and All activity. They do not use the Automatic Reply Limit.

Pending reminders appear in their thread or DM with a **Cancel** button. The
list uses `GET /w/<key>/threads/<thread-id>/reminders` or
`GET /w/<key>/direct-messages/<address-id>/reminders`. Cancel posts to the matching
path plus `/<reminder-id>/cancel`, with the session and CSRF checks used for
other mutations. Reminder operations, limits, clock files, and exact-session
ownership are documented in [operations](../../docs/operations.md#reminders).

An agent's final message answers your Channels message and appears in the
originating thread or DM as an ordinary reply. A reply the agent already sent in
that turn prevents a duplicate. Older text that a release before 1.6.0 copied
from a terminal stays marked **From @name's terminal**, and its conversation
needs you. Running subagent counts appear on member chips and Peek headers.

A person's new message resets a thread's Automatic Reply Limit to ten and lifts
a pause caused by that limit. Agent exchanges still consume the allowance.
A stopped thread stays stopped, and an ordinary manual pause stays paused.

## State and connection behavior

`wire.ts` validates retained fields before constructing branded `SpaceKey` and
`AddressId` values. Unknown wire versions and malformed snapshots are ignored.
New nonblank activity, health, member-kind, and attention-source strings retain
the snapshot as explicit unknown variants. Unknown status values use neutral labels.
Attention summaries remain visible only for known `needs_you` health, including
`agent_status` and future attention sources. Optional attention `subject_alias`
and `message_id` fields and the frame's `terminal_runs` parse tolerantly: a missing
or malformed value becomes null or an empty list and never drops the frame.
`store.ts` owns `channels: Map<SpaceKey, Channel>` and a pure reducer. Snapshots replace channel metadata atomically and accumulate full messages by ID.
`Channel.latestMessages` retains only the newest 150 snapshot rows as the reset
source, separate from the bounded visible `messages` and `runs`. `viewMode` is
`following` until history evicts the newest edge, then `browsing` until the explicit
`latest` event. These structures retain at most 750 message references per channel.
Equal incoming rows reuse their objects; unchanged maps, members and runs retain
identity. Equality and unread detection inspect frame rows and small metadata,
without serializing accumulated messages. A changed visible map can copy up to
600 entries per copy. Thread grouping is memoized and shared with timeline construction;
message components receive stable members, row references and thread callbacks.
Live rows update receipts; older history fills only missing rows. Contiguous ordered runs preserve server ordering
for equal timestamps and expose gaps between disjoint live windows.

The app opens exactly one same-origin `EventSource('/api/events')` per tab. Native
EventSource retry handles transient failures. A closed source is closed again
before a one-second timer creates its replacement. Unmount closes the source
and clears the timer.

During an outage and replay, the app keeps the last received channels and shows a
reconnect banner. Each `open` starts a pending snapshot collection. The server sends
exactly one `ready` event with `{ "keys": [...] }` after all initial `space` frames,
including an empty list when no workspace is live. A validated `ready` merges parsed
snapshots into the visible collection and removes only omitted keys. A listed key
with a rejected snapshot keeps its last good channel when one exists. Selection, unread
flags, and focused channel buttons survive for retained keys. Omitted keys disappear,
and a removed selection falls back to the landing rule above. An
interrupted replay is discarded. Duplicate keys in `ready` are normalized. Subsets
and supersets also end replay. `gone` removes committed and pending snapshots in
stream order, and a later snapshot can recreate the channel.

Each open starts one two-second replay timer. If `ready` never arrives, the app
merges parsed snapshots without pruning existing channels, then resumes live updates.
This compatibility interval gives ordinary replay time to complete. It does not
infer which channels an old server omitted. The first `ready` on that connection
still removes omitted keys from the current collection after fallback. Later
`ready` events on the same connection are ignored. A `ready` keeps the channel
you picked. It keeps an automatic selection too, unless the replay brought a
channel the app had not seen, which re-runs the landing rule.
Ready, errors, restarting, a new open, and unmount cancel the current timer.

Before any channel is committed or an authoritative `ready` arrives, the page keeps
its loading state: skeleton rows after 400 ms under the heading **Connecting to
Namedrop** for assistive technology. An empty timeout cannot confirm that no
workspaces exist. This loading state survives retries that discard staged
snapshots. An empty `ready` or removal of the last committed channel shows **No
workspaces yet**. Reconnects preserve that knowledge.

New channels establish a read baseline. New message IDs or changes to membership,
activity, or attention mark a background channel unread. Messages leaving the live
window stay loaded up to the visible cap without creating unread activity.
Changes to names, roles, or health counts alone do not. Selecting a channel keeps
its unread mark. The app marks it read after it has been selected and the document
visible for 1.5 seconds, so arrow keys can pass over channels without reading
them. New activity in the selected, visible channel does not mark it unread. **Needs you** follows the server's `health.level === "needs_you"`, so paused
and inactive channels do not show attention shortcuts or summaries.

## Performance entries

The browser's standard Performance API exposes these measures.

- `namedrop:frame-apply` measures the actual pure reducer call for each accepted
  `space`, `gone`, `ready`, or replay timeout event, including the replay commit. Parsing and
  React rendering are outside this interval.
  The app clears this measure after 200 samples, then starts a new batch.
- `namedrop:frame-to-message-paint` starts at `performance.now()` on each accepted
  `space` SSE callback, before parsing, and ends after two animation frames
  following the React commit that renders the selected channel state. It
  includes parsing, reducer work, React rendering, and the next paint opportunity.
  Frames received during replay wait for the replay commit or fallback. Frames
  for background channels do not emit samples. Later commits do not cancel a
  scheduled sample. A sample is recorded only if the same channel is selected at
  its paint opportunity. Batched frames share an end time and keep their own arrival times.
  Pending arrivals, animation callbacks, and measure entries are each bounded at
  200. It emits no per-message marks. Background tabs can delay animation frames.
- `namedrop:load-to-sidebar-paint` starts at the navigation time origin, `0`, and
  ends after two animation frames following a committed sidebar with at least
  one channel. It emits once per page load. It includes loading and connection
  time. An empty collection does not emit it.

- `namedrop:keystroke-to-popup` starts at the `timeStamp` of the composer's input
  event and ends in the layout effect after the commit that renders the open `#` or
  `@` list, once a forced layout read of the list returns. It covers completion,
  React rendering, style, and layout, but not paint. Input that leaves the list closed
  records nothing. The app clears this measure after 200 samples, then starts a new batch.
- `namedrop:enter-to-sent` starts at the `timeStamp` of the Enter keydown, or of the
  **Send** click, and ends when the post route answers with a 2xx and its receipt
  parses. It includes a token refresh and the retry after a 403. A refused or failed
  post records nothing. The app clears this measure after 200 samples, then starts a
  new batch.
- `namedrop:activity-derive` measures each call of the All activity derivation.
  The app derives the list whenever the channel map changes, so once per accepted
  frame, whether or not All activity is shown, because the sidebar count reads it.
  It covers the pure derivation only, not React rendering. The app clears this
  measure after 200 samples, then starts a new batch.
- `namedrop:click-to-peek-text` starts at the `timeStamp` of the click or key event
  that opens the peek drawer for a member and ends after two animation frames
  following the commit that paints that member's first screen text. It includes the
  screen route's latency. It records once per opening: a Refresh, reopening the
  member already shown, an empty screen, and a failed read record nothing. The app
  clears this measure after 200 samples, then starts a new batch.

Temporary marks are cleared after measurement. Read the entries with
`performance.getEntriesByName(name, 'measure')`. The live performance lane owns
latency conclusions and comparisons with the embedded dashboard.

Run the reducer comparison from this directory. The baseline directory must
contain `store.ts`, `model.ts`, and `wire.ts` from the baseline commit.

```sh
node --experimental-strip-types test/reduce-bench.mjs --before /path/to/baseline/src --after src --rounds 5 --max-p50-ms 5 --out /path/to/results.json
```

The benchmark alternates both versions, verifies each appended message, and
records source hashes and actual loaded counts. The new cap applies during
warmup, so a large preload retains 600 rows in the changed app. JSON parsing,
fixture setup, and assertions are outside the reducer timing.
