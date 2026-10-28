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
name. Duplicate names include the full workspace ID. Arrow keys, Home, and End
select and focus channel buttons inside each region. Tab moves between regions.
The first arriving channel opens automatically. Closing the selected workspace
leaves **Choose a channel** when other channels remain. OS dark mode controls all
surface and text tokens.

## State and connection behavior

`wire.ts` validates retained fields before constructing branded `SpaceKey` and
`AddressId` values. Unknown wire versions and malformed snapshots are ignored.
`store.ts` owns `channels: Map<SpaceKey, Channel>` and a pure reducer. Snapshots
replace channels atomically. Equal retained content returns the same state.
Message IDs are retained only for unread activity. Message bodies and history
belong to a later unit.

The app opens exactly one same-origin `EventSource('/api/events')` per tab. Native
EventSource retry handles transient failures. A closed source is closed again
before a one-second timer creates its replacement. Unmount closes the source
and clears the timer.

During an outage, the app keeps the last received channels and shows a reconnect
banner. Each `open` resets channels, selection, and unread flags before full replay.
This removes workspaces whose `gone` event was missed offline. The stream has no
initial-set completion marker, so replay briefly shows a partial collection and
selects the first replayed workspace. Selection and unread flags do not survive
that reset.

New channels establish a read baseline. New message IDs or changes to membership,
activity, or attention mark a background channel unread. Messages leaving the live
window update retained IDs without creating unread activity. Changes to names, roles,
or health counts alone do not. A selected channel stays read while the document
is visible. Returning to a visible document clears the selected channel's unread
flag. **Needs you** derives directly from the server's attention list.

## Performance entries

The browser's standard Performance API exposes these measures.

- `namedrop:frame-apply` measures the actual pure reducer call for each accepted
  `space` or `gone` event. Parsing and React rendering are outside this interval.
  The app clears this measure after 200 samples, then starts a new batch.
- `namedrop:load-to-sidebar-paint` starts at the navigation time origin, `0`, and
  ends after two animation frames following a committed sidebar with at least
  one channel. It emits once per page load. It includes loading and connection
  time. An empty collection does not emit it.

Temporary marks are cleared after measurement. Read the entries with
`performance.getEntriesByName(name, 'measure')`. The live performance lane owns
latency conclusions and comparisons with the embedded dashboard.
