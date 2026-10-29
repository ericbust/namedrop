# Namedrop channels experiment

This read-only app shows live Herdr workspaces as channels, with their members,
presence, and attention requests. It needs the dashboard's all-workspace stream
and `--ui` support. It does not send messages or change a workspace.

## Check and build the app

Use Node 26.7.0 and npm 11.19.0. From `experimental/channels`, run these commands.

```sh
npm ci
npm run lint
npm run typecheck
npm test
npm run build
```

`npm test` uses Node's built-in test runner with TypeScript stripping. It runs in
one process so every test registers in the same runner. `npm run build` emits
`dist/index.html` and external JavaScript and CSS. Fonts come from the system.
No inline scripts, inline styles, or remote assets are needed under the dashboard CSP.

Run the committed browser regressions from the same app directory. They build the
actual app and serve an isolated local SSE fixture under the dashboard CSP.

```sh
CHROME=/opt/google/chrome/chrome npm run test:browser
```

Set `CHROME` to your installed Chrome or Chromium executable. The headless runner
uses `--no-sandbox` and needs permission to bind a local test port. It covers focus
restoration, late `ready`, slow discovery, replay mismatches, old servers without
`ready`, and replay removal.

The optional, path-filtered **Channels experiment** workflow runs these checks.
It adds no required-check configuration. Cargo excludes `/experimental/**` from
the package.

## Serve and drive the app

From the repository root, use the isolated profile procedure in the
[channels driver](../../.claude/skills/verify/SKILL.md#channels-browser-app).
Build the app first, then serve it from that profile.

```sh
namedrop dashboard --no-open --json --ui /absolute/path/to/experimental/channels/dist
```

Run that command through the driver's `env -i` wrapper. Use the returned nonce URL
in headless Chrome through the pinned `playwright-core` package. Vite's development
server can serve assets with `npm run dev`, but it does not provide the authenticated
Namedrop API.

The **Channels** and **Needs you** navigation regions expose buttons by workspace
name. Display names have no mention prefix. Each row and channel header shows
its exact `#token` separately, or **Not addressable**. Duplicate names include the
full workspace ID and full 20-character space key. These suffixes stay fixed when
more same-name channels arrive.
Arrow keys, Home, and End select and focus channel buttons inside each region. Tab
moves between regions. The first channel in sidebar order opens when initial replay
completes. Closing the selected workspace leaves **Choose a channel** when other
channels remain. OS dark mode controls all
surface and text tokens.

## State and connection behavior

`wire.ts` validates retained fields before constructing branded `SpaceKey` and
`AddressId` values. Unknown wire versions and malformed snapshots are ignored.
New nonblank activity, health, member-kind, and attention-source strings retain
the snapshot as explicit unknown variants. Unknown status values use neutral labels.
Attention summaries remain visible only for known `needs_you` health, including
`agent_status` and future attention sources.
`store.ts` owns `channels: Map<SpaceKey, Channel>` and a pure reducer. Snapshots
replace channels atomically. Equal retained content returns the same state.
Message IDs are retained only for unread activity. Message bodies and history
belong to a later unit.

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
and a removed selection falls back to the first channel in sidebar order. An
interrupted replay is discarded. Duplicate keys in `ready` are normalized. Subsets
and supersets also end replay. `gone` removes committed and pending snapshots in
stream order, and a later snapshot can recreate the channel.

Each open starts one two-second replay timer. If `ready` never arrives, the app
merges parsed snapshots without pruning existing channels, then resumes live updates.
This compatibility interval gives ordinary replay time to complete. It does not
infer which channels an old server omitted. The first `ready` on that connection
still removes omitted keys from the current collection after fallback. Later
`ready` events on the same connection are ignored. On first load, that first
`ready` selects the first channel in sidebar order unless you selected one during
fallback.
Ready, errors, restarting, a new open, and unmount cancel the current timer.

Before any channel is committed or an authoritative `ready` arrives, the page keeps
showing **Connecting to Namedrop** and **Waiting for workspaces**. An empty timeout
cannot confirm that no workspaces exist. This loading state survives retries that
discard staged snapshots. An empty `ready` or removal of the last committed channel
shows **No live workspaces**. Reconnects preserve that knowledge.

New channels establish a read baseline. New message IDs or changes to membership,
activity, or attention mark a background channel unread. Messages leaving the live
window update retained IDs without creating unread activity. Changes to names, roles,
or health counts alone do not. A selected channel stays read while the document
is visible. Returning to a visible document clears the selected channel's unread
flag. **Needs you** follows the server's `health.level === "needs_you"`, so paused
and inactive channels do not show attention shortcuts or summaries.

## Performance entries

The browser's standard Performance API exposes these measures.

- `namedrop:frame-apply` measures the actual pure reducer call for each accepted
  `space`, `gone`, `ready`, or replay timeout event, including the replay commit. Parsing and
  React rendering are outside this interval.
  The app clears this measure after 200 samples, then starts a new batch.
- `namedrop:load-to-sidebar-paint` starts at the navigation time origin, `0`, and
  ends after two animation frames following a committed sidebar with at least
  one channel. It emits once per page load. It includes loading and connection
  time. An empty collection does not emit it.

Temporary marks are cleared after measurement. Read the entries with
`performance.getEntriesByName(name, 'measure')`. The live performance lane owns
latency conclusions and comparisons with the embedded dashboard.
