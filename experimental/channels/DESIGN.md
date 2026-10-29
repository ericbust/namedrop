# Channels design direction

This file is the design contract for the channels app. A builder follows it. The
design owner (`@chn-design`) changes it. The tokens live in
[`design/mockup/tokens.css`](design/mockup/tokens.css), and the target screens
live in [`design/mockup/index.html`](design/mockup/index.html). Copy the tokens
into `src/style.css` verbatim. When this file and the mockup disagree, this file
wins.

## Feel

The channels app is a quiet control room for your agents. You open it to read
what your agents said to each other, to see who is working, and to find the one
thing that needs you. Four rules shape every screen.

1. **Messages first.** The conversation gets the space. The channel header,
   members, and alerts stay thin, so the timeline holds at least 70 percent of
   the window height at 1440 by 900 while the Needs-you details are collapsed.
2. **Quiet until it needs you.** Neutral surfaces carry the page. Color means
   state, and red means you. A calm channel shows no red and no amber.
3. **Plain words.** Every state reads as something a person would say. Database
   words never reach the screen: no `settled`, `no_reply`, `disposition`,
   `delivery.inbox`, `ND-` codes, or raw runtime ids such as `claude-code`.
4. **Familiar.** Use Slack-grade conventions: a sidebar, a channel header, a
   timeline, a thread pane on the right. Invent nothing a chat user would have
   to learn.

Keep the current layout: sidebar, channel header with members, timeline, and a
thread pane. Raise the craft inside it.

## Decisions

The operator delegated design decisions to the design owner on 2026-10-28.
These are settled. Do not reopen them without a new decision here.

- **Answers show inline.** Each request shows its latest reply under it. The
  thread pane holds the full conversation. Slack-style collapsed threads are
  rejected, because nearly every agent request gets exactly one reply.
- **Members are a chip strip.** The facepile in the mockup's "Variant B" view is
  rejected. It hides the status words the operator reads at a glance.
- **The brand accent comes in.** The accent is Namedrop's periwinkle, and the
  dark theme is the brand's indigo night. The accent stays reserved for
  selection, links, and focus.
- **The sidebar stays alphabetical.** The Needs-you section carries urgency, and a
  stable order keeps channels findable.
- **A channel is read after 1.5 seconds on screen.** Passing over it with the
  arrow keys keeps its unread mark. This changes UI-1's behavior on purpose.

## Tokens

Use only tokens from `tokens.css`. No raw hex, rgb, or pixel font sizes in
component CSS. `node design/contrast.mjs` checks every text and state pair in
both themes and must exit 0 after any token change.

### Type

The page cannot load web fonts. The CSP sets `default-src 'none'` with no
`font-src`, so use the system stack in `--font-ui`. On the operator's Linux
machine it resolves to Adwaita Sans, on macOS to SF Pro, and on Windows to
Segoe UI Variable.

| Role                                               | Size                  | Line height             | Weight          |
| -------------------------------------------------- | --------------------- | ----------------------- | --------------- |
| Channel title                                      | `--text-lg` (18px)    | 24px                    | 600             |
| Empty-state title                                  | `--text-xl` (22px)    | 28px                    | 600             |
| Author name, member alias                          | `--text-base` (14px)  | 20px                    | 600             |
| Message body                                       | `--text-base` (14px)  | 21px (`--leading-body`) | 400             |
| Sidebar row                                        | `--text-base` (14px)  | 20px                    | 400, unread 600 |
| Thread title, card title                           | `--text-md` (15px)    | 20px                    | 600             |
| Meta: time, receipts, status words, section labels | `--text-xs` (12px)    | 16px                    | 400, labels 600 |
| Code                                               | `--font-mono` at 13px | 19px                    | 400             |

No readable text is smaller than 12px. The 11px letters inside 20px avatars
are decoration and stay `aria-hidden`. Use weights 400, 500, 600, and 700 only.
Timestamps and counts use `font-variant-numeric: tabular-nums`.

### Space and density

Use the 4px scale `--space-1` through `--space-10`. The density is comfortable,
not airy.

- Sidebar rows are 32px tall. Section gaps are 20px.
- The channel header is 56px. The member strip under it is 36px per line, at
  most two lines.
- A message group has 16px above it. Messages inside a group have 2px between
  them.
- The timeline and thread pane have 24px side padding. The thread pane is
  `--thread-width` (400px) wide.
- Message text never runs wider than `--measure` (72ch).

### Color

Light and dark use the same token names. Dark is the brand's own night: a
neutral with a faint indigo cast (hue 278), as in `docs/assets/social-preview.png`.

- **Accent** (`--accent`, `--accent-text`): periwinkle-indigo. Use it for the
  selected channel, links, the thread link, focus rings, and the primary action.
  Nothing else.
- **State** (`--working`, `--waiting`, `--blocked`, `--idle`, `--offline`): only
  on presence dots, receipt icons, and the Needs-you surface.
- **Attention** (`--attention-bg`, `--attention-line`, `--attention-text`): only
  on the Needs-you banner and failed receipts.
- **Identity** (`--id-0` through `--id-5`, `--id-terminal`, `--id-operator`):
  only on avatars. Pick the hue with `hash(alias) % 6`, where `hash` is the sum
  of the alias's UTF-16 code units. Terminals always use `--id-terminal`. The
  operator always uses `--id-operator`.
- Never use color alone. Every dot has a word next to it or an accessible name.

## Elements

### App shell

A grid of `--sidebar-width`, then the main column, then the thread pane when a
thread is open. The main column stacks the channel header, the member strip,
the Needs-you banner when present, and the timeline. Only the timeline and the
thread pane scroll.

### Landing and navigation

- With no channel in the URL, open the first channel that needs you. Else open
  the channel with the latest message.
- Keep the selected channel in the URL as `/w/<key>`, and push a history entry
  on each selection, so reload and Back work.
- The tab title is "Namedrop" when nothing needs you, and "(2) Needs you ·
  Namedrop" when two channels need you.
- Switching channels closes the thread pane.
- Arrow keys still move the selection, as UI-1 specified. A channel counts as
  read only after it has been shown for 1.5 seconds, so passing over it with the
  arrow keys keeps its unread mark.

### Sidebar

- **Brand row**, 52px: a 14px brand mark with a 4px radius, filled with the
  brand gradient `linear-gradient(135deg, oklch(0.74 0.12 275), oklch(0.82 0.11
  175) 55%, oklch(0.8 0.11 318))`, then **Namedrop** at 15px 600. The wordmark
  text is never a gradient.
- **Needs you** section: one row per channel that needs you, two lines tall. The
  first line is the channel name. The second line is the reason in 12px
  `--attention-text`: the first item's title from the Needs-you copy table
  below, such as "Unanswered request", plus "· 1 more" when there are more. A
  red dot sits at the right. When the
  section is empty, hide it entirely. Do not show "Nothing needs your attention."
- **Channels** section: one 32px row per channel. A muted `#` icon, the name in
  one line with an ellipsis, and a right slot. The right slot shows a red dot when
  the channel needs you, else a small accent dot when it has unread activity. The
  `#token` does not appear in the row. Same-name channels add the workspace id as
  a muted suffix, as in "namedrop · wB".
- Only the Channels row of the selected channel gets the selected background.
  Its Needs-you row stays unselected.
- **Footer**, 40px: the connection dot and "Live", "Connecting…", or
  "Reconnecting…". Keep "Read only" at the right until UI-3 adds the composer.
- Section labels are 12px 600 `--text-muted`, sentence case, with the count in
  tabular numerals. They are not headings. Each `nav` carries the section name
  as its `aria-label`, so the channel title is the page's first heading.

### Channel header

One 56px row: a muted `#` icon, the channel name at 18px 600, then the mention
token as a muted 12px chip, such as `#alpha`. When the channel has no token, the
chip reads "No mention label" and its tooltip says "Two workspaces share this
label, or it has none. Rename one in Herdr to mention it." The right side shows
the health summary in 12px muted text from the table below. When the channel
needs you, the summary is hidden, because the banner says it.

| `health.level` | Summary                                                                                   |
| -------------- | ----------------------------------------------------------------------------------------- |
| `calm`         | All quiet                                                                                 |
| `busy`         | "2 working · 3 pending", leaving out a part that is zero. "All quiet" when both are zero. |
| `paused`       | A drawn pause icon and "Paused · deliveries held", in `--held-text`                       |
| `inactive`     | Not active                                                                                |
| `unknown`      | nothing                                                                                   |

"In flight" never reaches the screen. A pending count is the number of messages
that have not settled yet.

### Member strip

Under the header, one wrapping line of member chips. Each chip is 28px tall: a
20px avatar with its presence dot as a badge, the alias at 13px 500, and the
status word at 12px muted. Order: needs you, working, waiting, idle, offline.
The operator chip reads "You", shows no presence, and comes last. The chip's
tooltip names the runtime in words: "Claude Code", "Codex", "OpenCode", "Pi",
or "Terminal". An unknown runtime id is shown with its first letter capitalized. After two lines, end with a
"+N more" chip that expands the strip. Remove the "Members" and "Status"
headings. In UI-4 a chip opens the peek drawer.

### Presence

| Activity | Dot                                                          | Word      |
| -------- | ------------------------------------------------------------ | --------- |
| working  | filled `--working`, a slow 2.4s opacity pulse from 1 to 0.55 | Working   |
| waiting  | filled `--waiting`                                           | Waiting   |
| blocked  | filled `--blocked`                                           | Needs you |
| idle     | 1.5px ring `--idle`, hollow                                  | Idle      |
| offline  | filled `--offline`, chip text `--text-muted`                 | Offline   |
| retired  | not shown in the strip                                       | Retired   |
| unknown  | 1.5px dashed ring `--idle`                                   | Unknown   |

The dot is 8px, or 10px with a 2px `--bg` ring when it is a badge on an avatar.

### Avatar

A 32px rounded square (`--radius-md`) in the identity hue, with the alias's first
letter in white at 14px 600. Terminals show a drawn `>_` prompt icon instead of a
letter. The operator shows "Y" in `--id-operator`. In the member strip and the
reply preview, avatars are 20px with 11px letters and `--radius-sm`.

### Message

```
[avatar]  @claude  → @codex  Handoff  11:02 PM
          please reply with the single word pong.
          ┌──────────────────────────────────────────────┐
          │ [av] @codex 11:03 PM                 1 reply │
          │      pong                                    │
          └──────────────────────────────────────────────┘
```

A request and its answer are the unit of this timeline. Nearly every agent
message gets exactly one reply, so the answer shows under the request without a
click.

- **Header line**: the author at 14px 600, then the recipients in 13px muted
  after a drawn 12px arrow icon, as "@codex" or "@codex, @shell", then the kind tag, then the time in 12px
  muted. Recipients come from the unique `legs[].to`. Center the items
  vertically.
- **Kind tag**: "Handoff" is a 12px chip on `--bg-hover` with `--radius-sm` and
  no border. Mentions and replies carry no tag.
- **Grouping**: consecutive messages by the same author within five minutes drop
  the avatar, author, and time. The time shows in the avatar column on hover.
  When a grouped message has different recipients or a different kind from the
  one above it, it keeps a short "→ @shell" line above its body.
- **To a terminal**: a message whose only recipient is a terminal member shows
  its body as a command chip: `--bg-raised`, a 1px `--line` border,
  `--radius-sm`, `--font-mono` at 13px, and a drawn prompt icon.
- **From a terminal**: a terminal's reply shows "Exit 0" with a check-circle in
  `--working` (or "Exit 1" with an alert in `--failed-text`) after the author.
  Its output renders as a code block. Omit the empty `stderr:` section.
- **Body**: 14px with 21px line height, at most `--measure` wide. Links use
  `--accent-text` with a 3px underline offset. Code blocks use `--bg-raised`, a 1px
  `--line` border, `--radius-md`, 10px by 12px padding, `--font-mono` at 13px, and
  `white-space: pre` with horizontal scroll. Bodies over 480 characters collapse
  behind a **Show more** text button.
- **Receipt line**: one 12px muted line under the body, with a 12px drawn icon per
  recipient. With one recipient, show only the state, because the header already
  names the recipient. With several, show "@codex Done · @shell Working on it".
  Use the receipt copy table below. Hide the line when every recipient replied
  and the reply preview shows, because the preview already says so.
- **Reply preview**: when the message has replies, show the latest reply in a
  box under the body: a 1px `--line` border, `--radius-md`, 8px by 10px padding,
  at most `--measure` wide. It holds a 20px avatar, the author at 13px 600, the
  time, and the reply body clamped to two lines at 13px. Terminal output uses
  `--font-mono` at 12.5px. "1 reply" or "3 replies" sits at the right in 12px 600
  `--accent-text`. The whole box is one button that opens the thread pane. Its
  accessible name is "1 reply from @codex at 11:03 PM. Open thread". On hover it
  gets `--bg-hover` and a `--line-strong` border.
- **Hover**: the row background becomes `--bg-hover` at 50 percent.
- **Arrival**: a message that arrives live fades in and rises 4px over
  `--duration` with `--ease-out`. History pages and channel switches do not
  animate.

### Receipt copy

| Wire stage                   | Shown as              | Icon               | Tone                       |
| ---------------------------- | --------------------- | ------------------ | -------------------------- |
| `queued`                     | Queued                | clock              | muted                      |
| `delivered`                  | Delivered             | check              | muted                      |
| `working`                    | Working on it         | half-filled circle | `--working` icon           |
| `replied`                    | Replied               | reply arrow        | muted, links to the thread |
| `completed`, kind `settled`  | Done                  | check-circle       | muted                      |
| `completed`, kind `no_reply` | Done, no reply needed | check-circle       | muted                      |
| `completed`, any other kind  | Done                  | check-circle       | muted, raw kind in `title` |
| `held`                       | Held while paused     | pause              | `--held-text`              |
| `failed`                     | Not delivered         | alert              | `--failed-text`            |
| `cancelled`                  | Cancelled             | slash-circle       | muted                      |
| `unknown`                    | Status unknown        | dashed circle      | muted                      |

### Report card

A `result_set` renders as one card: `--bg-raised`, a 1px `--line` border,
`--radius-lg`, and 12px by 16px padding, at most `--measure` wide. Its title row
has a drawn checklist icon and "Coordination report" at 15px 600, as an `h2`
so the heading levels never skip. The body follows
the message body rules. Never nest a card inside it.

### System rows

A nudge or hook is one 13px muted line. The avatar column holds a 16px drawn icon:
a bell for a nudge, a plug for a hook. The text reads "Nudge" or "Hook" in 600,
then the body on the same line. System rows have no receipts unless one failed.

### Needs-you banner

When the channel needs you, show a banner between the member strip and the
timeline, 24px from the sides. It uses `--attention-bg`, a 1px `--attention-line`
border, and `--radius-lg`. Each item is one row: a 16px drawn alert icon in
`--blocked`, the title at 14px 600 in `--text`, the hint in 13px muted, and the
age at the right, such as "4 min". A **Details** text button reveals the server's
full summary in 13px muted text. Show three items, then a "Show N more" button.
Announce new items through a polite live region.

| `source`                                | Title                                                                                                | Hint                                                 |
| --------------------------------------- | ---------------------------------------------------------------------------------------------------- | ---------------------------------------------------- |
| `agent_status`                          | the server summary, which is already short, such as "@claude is waiting on an approval or question." | Open its pane in Herdr to answer.                    |
| `agent_disposition`, `delivery_attempt` | Unanswered request                                                                                   | An agent ended its turn without replying.            |
| `automatic_reply_limit`                 | Reply limit reached                                                                                  | Continue or stop this thread from its pane.          |
| `coordination`                          | Coordination needs a decision                                                                        | Review the report and close it from the lead's pane. |
| `context_grant`                         | Context request waiting                                                                              | Approve or decline it from the pane.                 |
| `pane_binding`                          | An agent's pane changed                                                                              | Name the pane again to reconnect it.                 |
| `integration_registration`              | Agent setup incomplete                                                                               | Start a new session in that pane.                    |
| `unknown`                               | Something needs you                                                                                  | Open Details for the full message.                   |

Never cut the server's summary mid-word into the visible line. When an
`agent_status` summary is longer than 90 characters, use the `unknown` title and
put the summary under **Details**.

### Thread pane

400px wide, with a 1px `--line` left border. The header is 52px: "Thread" at 15px
600, then "in #alpha" in 12px muted, and a 32px icon button with a drawn X at the
right. Its accessible name is "Close thread", and Escape closes it. Below it come
the parent message, a divider that reads "1 reply" with hairlines on both sides,
and the replies. Replies use the message component without a reply preview. The
pane slides in from 16px right with a fade over `--duration`.

## Motion

Motion shows state and nothing else.

- A live message arrives with a 4px rise and fade over 180ms.
- A presence dot changes color over 180ms. A working dot pulses slowly.
- The thread pane enters over 180ms.
- The Needs-you banner expands and fades over 180ms when it appears.
- Under `prefers-reduced-motion: reduce`, remove the pulse and every movement,
  and keep only color changes.

## States

| State                         | What shows                                                                                                                         |
| ----------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| First connection, under 400ms | Nothing new. Keep the shell blank to avoid a flash.                                                                                |
| First connection, over 400ms  | Three skeleton sidebar rows and three skeleton messages, in `--bg-hover`, with no shimmer.                                         |
| No workspaces                 | Title "No workspaces yet". Body "Open a workspace in Herdr and name an agent with `namedrop name`. It shows up here as a channel." |
| Channel with no messages      | Title "No messages yet". Body "When agents in this workspace mention each other, the conversation shows up here."                  |
| Loading older messages        | A 12px muted "Loading earlier messages…" line at the top of the timeline.                                                          |
| History error                 | "Couldn't load earlier messages." and a **Try again** button, in the same line.                                                    |
| Missing range                 | A hairline divider with "Some messages are missing here" and a **Load them** button.                                               |
| Reconnecting                  | A 36px bar at the top of the main column in `--warning-bg` and `--warning-text`: "Reconnecting… Showing updates up to 10:42 PM."   |
| Server-shortened text         | A 12px muted line "Shortened by Namedrop."                                                                                         |

## Accessibility

- Text contrast is at least 4.5:1, and dots and icons at least 3:1, in both
  themes. `design/contrast.mjs` enforces it.
- Focus rings are 2px `--focus` with a 2px offset on every focusable element.
  Never remove an outline without replacing it.
- The sidebar keeps its arrow-key navigation. The timeline scroller stays
  focusable. Escape closes the thread pane and returns focus to the thread button.
- Every presence dot, receipt icon, and avatar has a text equivalent. Decorative
  icons are `aria-hidden`.
- Hit targets are at least 32 by 32px.
- The layout holds at 200 percent zoom and at 1024 by 700 without horizontal
  scroll.
- Theme the browser surfaces: `::selection` uses `--bg-selected`, the scrollbar
  uses `--line-strong` on transparent, and the caret uses `--accent`.

## Icons

Draw icons as inline SVG in React, 1.5px stroke on a 16px grid, `currentColor`,
round caps and joins. Do not use emoji or Unicode glyphs as icons. Do not add an
icon package for the 15 icons this app needs.

## Design review gate

`@chn-design` reviews every UI PR for design before it merges. The operator does
not review design. Send these with the PR head SHA:

- Screenshots at 1440 by 900 and 1024 by 700, in light and dark, of every screen
  and state the PR changes, with real testbed content.
- A 20 to 60 second video of each changed interaction.
- The output of `node design/contrast.mjs` when tokens changed, and of
  `impeccable detect --json` on the changed `src/ui` files.

The verdict is PASS, PASS with follow-ups, or CHANGES with a list of required
edits. A follow-up never blocks a merge. A broken layout, unreadable text, a
contrast failure, a database word on screen, or a state with no design blocks
it.

## Coming next

- **UI-3 composer**: docks at the bottom of the timeline and of the thread pane.
  The input has `--bg`, a 1px `--line-strong` border, `--radius-lg`, and 12px
  padding, with the placeholder "Message #alpha". The `#` and `@` popup uses
  `--bg`, `--shadow-pop`, and `--radius-md`, with 32px rows that each hold an
  avatar, the alias, and the presence word. The highlighted row uses
  `--bg-selected`. A pending message renders at 60 percent opacity with a
  "Sending…" receipt.
- **UI-4 All activity**: a sidebar row above Needs you with a drawn pulse icon.
  The view lists member rows in the member-chip vocabulary, grouped Needs you,
  Waiting, Working, with the channel and "waiting on @codex" as the second line.
- **UI-4 Peek drawer**: takes the thread pane's slot and width plus 80px. Text is
  `--font-mono` at 12.5px on `--bg-raised`, with a **Refresh** button and a
  "Read at 10:42 PM" stamp in the header.
