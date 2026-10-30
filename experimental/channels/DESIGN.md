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
- **The channel is a room.** A post that does not start with `@name` goes to
  the channel and reaches only the agents it mentions and means. Channel posts
  and agent conversations show every reply as its own message in time order, as
  in a group chat. A request the operator starts with `@name` keeps the request
  and answer unit above. Decided 2026-10-29 for the operator's channel-chat
  request and north star (preferences lines 21, 23 and 25).

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
- A message group has 16px above it. Messages inside a group sit about 6px
  apart, which keeps the hover padding.
- The timeline scroller fades its top 12px with a `mask-image` gradient, so a
  half-scrolled message never sits flush under the header or banner.
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
  only on avatars. Pick the hue with `hash(name) % 6`, where `hash` is the sum
  of the UTF-16 code units of the name as displayed, with its `@`, such as
  "@claude". Terminals always use `--id-terminal`. The
  operator always uses `--id-operator`.
- Never use color alone. Every dot has a word next to it or an accessible name.
- Mix overlay colors in sRGB, as in `color-mix(in srgb, var(--bg-hover) 50%,
  transparent)`. Mixing with `transparent` in OKLCH swings the hue toward red.

## Elements

### App shell

A grid of `--sidebar-width`, then the main column, then the thread pane when a
thread is open. The main column stacks the channel header, the member strip,
the Needs-you banner when present, and the timeline. The shell fills the window
exactly, and the page itself never scrolls. Only the panes inside it scroll: the
sidebar list, the Needs-you details, the timeline, the thread, the activity list,
and the screen text. Each of them is the containing block of what it scrolls, so
hidden text inside it never stretches the page.

Below 860px of window width, an open thread pane or peek drawer stacks under
the main column, and each gets half the window's height. Nothing in the main
column paints outside it. The Needs-you banner takes at most 40 percent of the
main column. When the column is short, the timeline shrinks first, down to
nothing, and then the banner, which scrolls inside itself. The composer always
stays whole. Side by side, from 860px up, the banner's cap is 40 percent of the
window's height.

### Landing and navigation

- Open the channel the URL names. With no channel in the URL, or one that has
  not arrived, open the first channel that needs you, else the channel with the
  latest message.
- Landing runs only while channels are being discovered. Discovery ends when
  the server says `ready`. A server that never says `ready` ends it 2 seconds
  after the first channel arrives. During discovery each newly arrived channel
  re-runs this rule, so the app moves to Needs you as soon as it appears. The
  URL's own channel is the exception: it is chosen whenever it arrives, even
  after discovery, because the reader asked for it.
- Any input from the reader stops all of this: a click, a key press, a scroll,
  or a channel pick. From then on the app never moves by itself. A workspace
  that opens later, or a reconnect that replays channels, never changes the open
  channel or closes its thread.
- When you pick a channel, its URL becomes `/w/<key>` with a new history entry,
  so reload and Back work. An automatic choice never rewrites the URL.
- The tab title is "Namedrop" when nothing needs you, and "(2) Needs you ·
  Namedrop" when two channels need you.
- Switching channels closes the thread pane.
- When none of an open thread's messages are loaded any more, for example after
  Jump to latest, the thread closes for good. It does not reopen when history
  loads it again. Focus moves to the timeline, never to the page body.
- **All activity** has its own address, `/?view=activity`, because the server
  serves the page only at `/` and `/w/<key>`. Picking it pushes that URL, so
  reload and Back work. A URL that names it opens it at once and ends landing,
  because the reader asked for it.
- Arrow keys still move the selection, as UI-1 specified. A channel counts as
  read only after it has been shown for 1.5 seconds, so passing over it with the
  arrow keys keeps its unread mark.

### Sidebar

- **Brand row**, 52px: a 14px brand mark with a 4px radius, filled with the
  brand gradient `linear-gradient(135deg, oklch(0.74 0.12 275), oklch(0.82 0.11
  175) 55%, oklch(0.8 0.11 318))`, then **Namedrop** at 15px 600. The wordmark
  text is never a gradient.
- **All activity** row, 32px, above Needs you: a drawn pulse icon, "All
  activity", and in the right slot the number of agents that are working,
  waiting, or need you, in muted tabular numerals. The number hides at zero.
  When selected, the row gets the selected background like a channel row, and
  no channel row is selected. The row is reached with Tab. It is not part of
  the channel list's arrow-key navigation.
- **Needs you** section: one row per channel that needs you, two lines tall. The
  first line is the channel name. The second line is the reason in 12px
  `--attention-text`: the first item's title from the Needs-you copy table
  below, such as "Unanswered request", plus "· 1 more" when there are more. A
  red dot sits at the right. When the
  section is empty, hide it entirely. Do not show "Nothing needs your attention."
- **Channels** section: one 32px row per channel. A muted `#` icon, the name in
  one line with an ellipsis, and a right slot. The right slot shows a red dot when
  the channel needs you, else a small accent dot when it has unread activity. The
  `#token` does not appear in the row. Same-name channels add a muted suffix with
  the workspace id and the full key, because two spaces can share both a name
  and a workspace id.
- Only the Channels row of the selected channel gets the selected background.
  Its Needs-you row stays unselected.
- **Footer**, 40px: the connection dot and "Live", "Connecting…", or
  "Reconnecting…".
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
status word at 12px muted. Order: needs you, working, waiting, idle, offline,
unknown, pane closed. The operator chip reads "You", shows no presence, and
comes last. The chip's
tooltip names the runtime in words: "Claude Code", "Codex", "OpenCode", "Pi",
or "Terminal". An unknown runtime id is shown with its first letter capitalized. After two lines, end with a
"+N more" chip that expands the strip. Remove the "Members" and "Status"
headings. Each member chip is a button that opens the peek drawer for that
member. The operator chip is not a button.

### Presence

| Activity | Dot                                                          | Word        |
| -------- | ------------------------------------------------------------ | ----------- |
| working  | filled `--working`, a slow 2.4s opacity pulse from 1 to 0.55 | Working     |
| waiting  | filled `--waiting`                                           | Waiting     |
| blocked  | filled `--blocked`                                           | Needs you   |
| idle     | 1.5px ring `--idle`, hollow                                  | Idle        |
| offline  | filled `--offline`, chip text `--text-muted`                 | Offline     |
| retired  | none, chip text `--text-muted`                               | Pane closed |
| unknown  | 1.5px dashed ring `--idle`                                   | Unknown     |

The dot is 8px, or 10px with a 2px `--bg` ring when it is a badge on an avatar.
A retired member shows in the strip for 5 minutes after its pane closes, with
the word "Pane closed", then leaves it. Its messages keep its name.

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
- **Names open the screen**: the author and each named recipient in a header
  are buttons that open the peek drawer for that member. They keep their text
  style, underline on hover, and show the focus ring. The operator, "+N", and
  ids the page cannot place stay plain text.
- **Kind tag**: "Handoff" is a 12px chip on `--bg-hover` with `--radius-sm` and
  no border. Mentions and replies carry no tag.
- **Grouping**: consecutive messages by the same author within five minutes drop
  the avatar, author, and time. The time shows in the avatar column on hover.
  When a grouped message has different recipients or a different kind from the
  one above it, it keeps a short "→ @shell" line above its body.
- **To a terminal**: a message whose only recipient is a terminal member shows
  its body as a command chip: `--bg-raised`, a 1px `--line` border,
  `--radius-sm`, `--font-mono` at 13px, and a drawn prompt icon.
- **From a terminal**: Namedrop writes a terminal's reply as
  `@shell command finished with exit code N.` followed by `stdout:` and
  `stderr:` sections. Parse that once, for terminal authors only. The reply shows
  "Exit 0" with a check-circle in `--working` (or "Exit 1" with an alert in
  `--failed-text`) after the author, then stdout as a code block, then stderr
  under a small "stderr" label only when it is not empty. With neither, show
  "No output" in muted text. A body that does not parse renders as it is. Each
  reply shows its own run: of the `TerminalRun`s whose `message_id` names the
  request, the latest one that started at or before the reply. That run's code
  overrides the parsed one. The request row shows the newest run. The server
  sends runs newest first, so never let list order pick the run.
- **Body**: 14px with 21px line height, at most `--measure` wide. Links use
  `--accent-text` with a 3px underline offset. Code blocks use `--bg-raised`, a 1px
  `--line` border, `--radius-md`, 10px by 12px padding, `--font-mono` at 13px, and
  `white-space: pre` with horizontal scroll. Drop one newline right after a
  closing fence and right before an opening fence, so no blank line sits next to
  a code block. Bodies over 480 characters collapse behind a **Show more** text
  button.
- **Receipt line**: one 12px muted line under the body, with a 12px drawn icon per
  recipient. With one recipient, show only the state, because the header already
  names the recipient. With several, show "@codex Done · @shell Working on it".
  Use the receipt copy table below. Hide the line when every recipient replied
  and the reply preview shows, because the preview already says so.
- **Reply preview**: when a request has replies, show the latest reply in a
  box under the body. A channel post or an agent conversation never
  gets one, as Channel chat says.
  The box has a 1px `--line` border, `--radius-md`, 8px by 10px padding, and is
  at most `--measure` wide. It holds a 20px avatar, the author at 13px 600, the
  time, and the reply body clamped to two lines at 13px. Terminal output uses
  `--font-mono` at 12.5px. "1 reply" or "3 replies" sits at the right in 12px 600
  `--accent-text`. The whole box is one button that opens the thread pane. Its
  accessible name is "1 reply from @codex at 11:03 PM. Open thread", or "3
  replies, latest from @codex at 11:03 PM. Open thread". A terminal reply's
  preview shows its Exit chip and the first two lines of stdout. On hover it
  gets `--bg-hover` and a `--line-strong` border.
- **Narrow previews**: the reply count never shrinks and nothing covers it.
  The author, time and reply text take the width left beside it, the author
  and text ending in an ellipsis. When less than 120px is left for them, as in
  a 390px window, the box shows the count alone and hugs it, the look of a
  request whose latest reply is not loaded.
- **Hover**: the row background becomes `--bg-hover` at 50 percent.
- **Arrival**: a message that arrives live fades in and rises 4px over
  `--duration` with `--ease-out`. History pages and channel switches do not
  animate.

### Receipt copy

| Wire stage                                 | Shown as                | Icon               | Tone                       |
| ------------------------------------------ | ----------------------- | ------------------ | -------------------------- |
| `queued`                                   | Queued                  | clock              | muted                      |
| `delivered`                                | Delivered               | check              | muted                      |
| `working`                                  | Working on it           | half-filled circle | `--working` icon           |
| `replied`                                  | Replied                 | reply arrow        | muted, links to the thread |
| `completed`, kind `settled`, to an agent   | Ended without an answer | dashed circle      | muted                      |
| `completed`, kind `settled`, to a terminal | Done                    | check-circle       | muted                      |
| `completed`, kind `no_reply`               | Done, no reply needed   | check-circle       | muted                      |
| `completed`, any other kind                | Done                    | check-circle       | muted, raw kind in `title` |
| `held`                                     | Held while paused       | pause              | `--held-text`              |
| `failed`                                   | Not delivered           | alert              | `--failed-text`            |
| `cancelled`                                | Cancelled               | slash-circle       | muted                      |
| `unknown`                                  | Status unknown          | dashed circle      | muted                      |

`settled` means the recipient's turn ended without closing the request. For an
agent that is an interruption or a forgotten answer, which Needs-you reports as
"didn't answer a request", so the receipt never says Done. For a terminal the
command finished. An agent in another workspace, or an id the page cannot
place, counts as an agent; the operator and a member of a kind the page does
not know read Done.

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

A nudge never shows a raw id. "Coordination coordination-… (revision 3)
remains open." reads "This coordination (revision 3) remains open.", and each
"Assignment assignment-… revision 4: needs_continuation." reads "Assignment 1,
revision 4: needs continuation.", numbered in order of appearance. Any other
id (`coordination-`, `assignment-`, `message-`, `thread-`, `delivery-`,
`attempt-`, `delivery-attempt-` or `request-`, then three digit groups) is
dropped with the space around it. The server's text is the body's tooltip.

### Needs-you banner

When the channel needs you, show a banner between the member strip and the
timeline, 24px from the sides. It uses `--attention-bg`, a 1px `--attention-line`
border, and `--radius-lg`. Each item is one row: a 16px drawn alert icon in
`--blocked`, the title at 14px 600 in `--text`, the hint in 13px muted, and the
age at the right, such as "4 min". Under the hint, the item's actions sit on one
wrapping row of 13px text buttons, 12px apart, such as "Show message · Reply to
@quiet · Details". **Details** is always on that row; it reveals the server's
full summary in 13px muted text. On a narrow window the row wraps between
buttons, never inside a label. Show three items, then a "Show N more" button.
The banner takes at most 40 percent of the main column's height and scrolls
inside (see App shell for the stacked layout).
Announce new items through a polite live region.

| `source`                                | Title                                                                                                | Hint                                                |
| --------------------------------------- | ---------------------------------------------------------------------------------------------------- | --------------------------------------------------- |
| `agent_status`                          | the server summary, which is already short, such as "@claude is waiting on an approval or question." | Answer it in its pane in Herdr.                     |
| `agent_disposition`, `delivery_attempt` | Unanswered request                                                                                   | An agent ended its turn without replying.           |
| `automatic_reply_limit`                 | Reply limit reached                                                                                  | Continue or stop this thread from its pane.         |
| `coordination`                          | Coordination needs a decision                                                                        | Read the report, then answer the lead.              |
| `context_grant`                         | Context request waiting                                                                              | Approve or decline it in the agent's pane in Herdr. |
| `pane_binding`                          | An agent's pane changed                                                                              | Name the pane again to reconnect it.                |
| `integration_registration`              | Agent setup incomplete                                                                               | Start a new session in that pane.                   |
| `unknown`                               | Something needs you                                                                                  | Open Details for the full message.                  |

Never cut the server's summary mid-word into the visible line. When an
`agent_status` summary is longer than 90 characters, use the `unknown` title and
put the summary under **Details**.

When the item names its agent (`subject_alias`), the title names it too. For an
unanswered request that reads "@codex didn't answer a request", with the hint
"It ended its turn without replying." When the item names a message
(`message_id`) that is loaded in the timeline, a **Show message** text button
starts the action row. It scrolls the message to the middle of the timeline, tints it
with `--bg-selected` for 1.5 seconds, and opens its thread. When that message is
not loaded, the button is absent. A conversation the item opens in the thread
pane marks its target the same way: `--bg-selected` for 1.5 seconds, then it
fades. Under reduced motion the tint holds still and leaves after 1.5 seconds.
The tint never stays.

| `source`                                   | Title when the item names its agent                                          |
| ------------------------------------------ | ---------------------------------------------------------------------------- |
| `agent_disposition`, `delivery_attempt`    | "@codex didn't answer a request", hint "It ended its turn without replying." |
| `agent_status`, summary over 90 characters | "@claude needs you"                                                          |
| `automatic_reply_limit`                    | "@codex hit the reply limit"                                                 |
| `coordination`                             | "@claude's coordination needs a decision"                                    |
| `context_grant`                            | "@codex is asking for context"                                               |
| `pane_binding`                             | "@codex's pane changed"                                                      |
| `integration_registration`                 | "@codex isn't set up yet"                                                    |
| `unknown`                                  | "@codex needs you"                                                           |

A short `agent_status` summary already names the agent, so it stays as the
server wrote it. Hints keep their wording from the first table, except where
the item's response decides them below.

**Response.** An item's `response` names the most direct answer the page can
offer, and its button names the action and the person:

| `response` | Button                 | What it does                                                         |
| ---------- | ---------------------- | -------------------------------------------------------------------- |
| `reply`    | "Reply to @quiet"      | Opens the conversation with the reply box addressed to that agent.   |
| `message`  | "Message @claude"      | Opens the conversation and prepares a channel message to that agent. |
| `pane`     | "See @worker's screen" | Opens the peek drawer for that agent.                                |
| `local`    | none                   | Its setup step becomes the hint.                                     |

A `pane` response is answered in Herdr, so its hint names the agent's pane,
with the pane label, such as `w1:worker`, as the hint's tooltip. A `local`
response's hint reads "Open this workspace in Herdr and choose Namedrop setup
from the command palette to repair the integration." for
`integration_registration`, and "Open this workspace in Herdr and choose
Namedrop Doctor to review this item." otherwise.

**Which hint wins.** The first row that fits the item gives its hint:

| Item                                                  | Hint                                                | Tooltip        |
| ----------------------------------------------------- | --------------------------------------------------- | -------------- |
| A `local` response                                    | Its setup step, as above                            | none           |
| `automatic_reply_limit` with Continue or Stop on page | "The agents paused after 10 replies in a row."      | none           |
| `context_grant` with a `pane` response                | "Approve or decline it in @worker's pane in Herdr." | the pane label |
| Any other source with a `pane` response               | "Answer it in @worker's pane in Herdr."             | the pane label |
| `agent_status` whose agent is known                   | "Answer it in @claude's pane in Herdr."             | none           |
| Anything else, or no agent known                      | The first table's hint, or the named hint above     | none           |

**Reply limit.** A reply-limit item that names its thread (`thread_id`) and
lists the actions the browser may take (`actions`) can be continued or stopped
from the page. Its hint reads "The agents paused after 10 replies in a row."
Two 13px text buttons follow Show message on the item's action row, each only
when its action is listed:

| Button                  | Tooltip                                                   | Sends                                                |
| ----------------------- | --------------------------------------------------------- | ---------------------------------------------------- |
| "Allow 10 more replies" | "Lets the agents send 10 more replies in this thread."    | `POST /w/<key>/threads/<id>/continue`                |
| "Stop thread"           | "Ends agent replies in this thread. You can't undo this." | `POST /w/<key>/threads/<id>/stop`, after the confirm |
- Stop asks once, inline. The two buttons give way to "Stop this thread for
  good?", then a "Stop" text button in `--failed-text` and a "Cancel" text
  button. Focus moves to Stop. Cancel or Escape returns to the row and focuses
  Stop thread. Only the confirm's Stop sends.
- A press reads "Allowing…" or "Stopping…" on the pressed button, and every
  action on the row is disabled. The item stays pending after the server
  answers, until a frame drops it. When the item leaves first, the page
  cancels the request, so a slow server never holds a connection.
- A refusal puts a 12px line in `--failed-text` under the row, with
  `role="alert"` and the HTTP status as its tooltip: "Couldn't allow more
  replies. Try again." or "Couldn't stop the thread. Try again." For 404 it
  reads "This thread is no longer in this workspace." The buttons come back.
- The same row shows in All activity. An item without a thread or actions
  keeps the hint "Continue or stop this thread from its pane."

**Prepared message.** A channel message prepared for a tracked item never puts
a raw id where the operator writes. The first line is plain words and the
caret ends it; the id the agent needs sits on a last line of its own, so typing
goes above it:

| Anchor       | First line                                 | Last line             |
| ------------ | ------------------------------------------ | --------------------- |
| coordination | "@claude About your coordination report: " | "Ref: coordination-…" |
| message      | "@codex About your message at 5:54 PM: "   | "Ref: message-…"      |

The time uses the page's local time format, as message headers do.

### Thread pane

400px wide, with a 1px `--line` left border. The header is 52px: "Thread" at 15px
600, then "in #alpha" in 12px muted, and a 32px icon button with a drawn X at the
right. Its accessible name is "Close thread", and Escape closes it. Below it come
the parent message, a divider that reads "1 reply" with hairlines on both sides,
and the replies. When the thread has no replies and the server knows of none, the
divider reads "No replies yet". Replies use the message component without a
reply preview. The pane slides in from 16px right with a fade over `--duration`.
Below 860px of window width, the pane stacks under the timeline instead of
beside it.

A Needs-you conversation that is answered somewhere else says where, in the
composer's place: one 12px muted line, 20px from the sides and 12px from the
bottom. Only one place offers an action at a time.

| The item's answer               | The line                                                                          |
| ------------------------------- | --------------------------------------------------------------------------------- |
| A channel message, ready        | "Answer tracked work in the channel. Your message to @claude is ready there."     |
| A channel message, not prepared | "Answer tracked work in the channel." then a **Message @claude** text button      |
| The agent's pane                | "Answer it in @worker's pane in Herdr.", with the pane label as its tooltip       |
| A reply to an agent now gone    | "The responsible agent is no longer available. Use its pane in Herdr to respond." |

When a reply's thread already holds a draft, the composer stays and its status
row reads, 12px muted: "Your existing draft was kept. Send or clear it before
preparing this reply."

### History window

The timeline keeps at most 600 loaded rows. A channel is in one of two modes.

- **Following**: the window ends at the newest message, and live messages
  append to it. When a live message would push the window past 600 rows, the
  oldest rows drop only while the reader is at the bottom, within 24px. If the
  reader has scrolled up, the channel switches to browsing instead. No row in or
  near the viewport ever disappears, and the view never jumps.
- **Browsing**: the window stays where the reader left it. Live updates still
  refresh rows already shown. New messages wait outside the window.

The way back is always at the bottom.

- While the channel is browsing, or the reader is more than one screen above the
  newest message, a pill floats 16px above the bottom edge of the timeline,
  centered. It holds a drawn arrow-down icon and "Jump to latest" at 13px 500 in
  `--accent-text`, on `--bg` with a 1px `--line-strong` border, a full radius,
  `--shadow-pop`, and a 32px height. Its accessible name is "Jump to latest
  messages". Activating it returns to the newest window and scrolls to the
  bottom.
- While browsing, the last row of the timeline is a hairline divider that reads
  "Newer messages aren't loaded", with a **Jump to latest** text button, so
  keyboard and screen reader users meet the gap at the end of the list.
- The top history control only loads older messages. It never holds Jump to
  latest, because auto-load fires there first.
- Scrolling down does not load newer pages. The history route only pages
  backward.

### Composer

The composer is how the operator talks to agents. It docks at the bottom of the
main column, under the timeline, and at the bottom of the thread pane.

- **Box**: 24px from the column's sides and 12px from its bottom. `--bg`, a 1px
  `--line-strong` border, `--radius-lg`, 10px by 12px padding. The text area is
  14px with 21px line height. It starts at one line, grows to eight, then
  scrolls. While focused, the border turns `--accent` with a 3px ring of
  `--accent` at 20 percent, mixed in sRGB.
- **Placeholder**: "Message #alpha" for an addressable channel, else "Message
  testbed ui2-live". In the thread pane, where the "To" label already names the
  recipients: "Write a reply".
- **Send button**: a 32px square at the right, `--radius-md`, with a drawn
  arrow-up icon. It is `--accent` with `--on-accent` when there is text, and
  muted and disabled when the box is empty. Its accessible name is "Send".
- **Keys**: Enter sends, and Shift+Enter starts a new line. While the popup is
  open, Enter and Tab accept the highlighted suggestion instead.
- **Hint**: while the box has focus, a 12px muted line under it says where Enter
  sends the text. In the thread pane it reads "Enter to send · Shift+Enter for
  a new line". The channel composer's hints are in Channel chat below.
- **Kept draft**: when a Needs-you **Message @claude** finds the channel box
  already holding other text, the text stays and the status row under the box
  reads, in 12px `--text` like the recipient guard: "Your draft is still here.
  Send or clear it, then press Message @claude again.", with the pressed
  button's own label. There is no link in it; the card's button is the retry.
  The message that button prepared is not another draft, so pressing it again
  never warns. When the agent is gone, the same row reads "The responsible
  agent is no longer available. Use its pane in Herdr to respond."
- The Jump to latest pill floats 16px above the composer, not above the
  timeline's bottom edge.

### Sending

- Enter locks the box, which keeps its text, and shows "Sending…" in place of
  the hint. A second Enter while sending does nothing, so a double Enter writes
  one row.
- At once, a pending row appears at the bottom of the timeline: the operator's
  message at 60 percent opacity, with a clock icon and "Sending…" as its
  receipt. In the thread pane it appears at the end of the replies.
- When the server accepts the post, the box clears and unlocks, and the pending
  receipt reads "Sent". When the real row arrives in a frame, it replaces the
  pending row with no jump.
- When the server refuses the post, the pending row goes away. The box keeps
  the text, unlocks, and shows the error under it in 13px `--failed-text` with a
  drawn alert icon. For 400, 404 and 409 the error is the server's own message,
  such as "Unknown recipient @ghost". For 403 it reads "Your session expired.
  Reload the page to keep posting." For 413 it reads "This message is too
  long." For 401 it reads the same as 403. A server message shows with its first
  letter capitalized and a final period. Anything else reads "Namedrop couldn't
  send this. Try again." Editing the text clears the error.
- A thread reply needs someone to get it: the thread's agents, or the members
  its text starts with. When the thread has no agents and the text does not
  start with `@name`, `#channel @name`, or `#all @name`, Enter does not send,
  and the Send button stays disabled. Only after Enter or a click on Send does
  the hint line read "Start with @name to choose who gets this." in `--text`.
  Typing clears it, so it never nags while a recipient is still being typed. A
  channel post always sends, as Channel chat says.
- Names with no message after them, as in `@codex` or `#alpha @codex`, never
  send in any composer, because the server refuses them. After Enter the hint
  line reads "Add a message after @codex." in `--text`, and typing clears it.
- A refused draft remembers its latest attempt: the request id and the exact
  text and reply target it tried. Sending that same text again reuses the id,
  even after edits that end back at the same text, so retrying an attempt whose
  outcome is unknown can never post twice. Different text gets a new id, and a
  new refusal replaces the remembered attempt. The attempt is forgotten only
  after a success. Ids never move between drafts.
- Every composer keeps its own draft for as long as the page is open: one per
  channel and one per thread. Switching channels or closing a thread keeps the
  text.
- A post refused while you are elsewhere is never lost. Its text and its error
  go back into the draft of the composer that sent it, and show when you return.
  While any draft in a channel holds a refused post, the channel's sidebar row
  shows a 14px alert icon in
  `--failed-text` after its name, with the accessible name "A post wasn't sent".
  A refused thread reply also shows "Reply not sent · Open thread" in 12px
  `--failed-text` under its parent message, as a button that opens the thread.

### Thread replies

A reply in the thread pane goes to the thread's agents, the way typing in an
agent's pane reaches that agent.

- The recipients are every agent who wrote in the thread or was sent a message
  in it, except terminals and retired members. Never the operator. The server
  applies the same rule. The page only shows it.
- The thread composer shows them as a muted label at the left of the box: "To
  @codex, @claude", where an agent from another workspace reads "@lead ·
  #alpha".
- Text that does not start with mentions is sent exactly as typed. Text that
  starts with its own mentions goes to those members instead, and the label
  hides.
- When the thread has no such agents, as when its only other member is a
  terminal, the reply must start with `@name`, so a terminal never runs text by
  accident. The box reads "Start with @name to choose who gets this."
- The post carries `in_reply_to` with the message Respond targets, else the
  latest message in the thread.

### Channel chat

Talk in a channel the way you talk in a Herdr pane with Namedrop. Write
normally. A post reaches an agent you mention when it is meant for them, agents
bring each other in by name, and the whole conversation stays in view. It should
read like a room you talk in, not a ticket queue.

```
[Y]  You  11:02 PM
     can @claude check why the mac build fails on main?
     → @claude · build question

[c]  @claude  11:02 PM
     The arm64 link step fails. @codex, is the CI cache stale?
     → @codex · pulled in · ◐ Working on it

[x]  @codex  11:04 PM
     Yes. I cleared it, and the build passes now.

[Y]  You  11:20 PM
     heads up: I'm rotating the staging API keys at 3pm

[Y]  You  11:22 PM
     nice catch by @codex on the cache
     ▢ Mentioned @codex, not sent · Send anyway

     ↩ @codex: Yes. I cleared it, and the build passes now.
[c]  @claude  11:31 PM
     I added a CI check so a toolchain bump clears the cache.
```

**What the channel composer sends.**

- Text that starts with `@name`, `#channel @name`, or `#all @name` goes only to
  the people it names. It is a request, and it shows as one.
- Any other text is a channel post. The server stores it exactly as typed.
  When it mentions no agent, it is a message to the room: no one gets it and no
  model runs. When it mentions agents, the server asks a small model which of
  them the post is for, and only those agents get it. The model picks only among
  the mentioned agents and never changes the text.
- The agents a post mentions are the channel's agent members that are not
  retired, in the order the text first names them. Terminals never count,
  because a terminal runs its text as a command. A mention follows the
  server's grammar in `src/message.rs`:
  - A code span, a run of backticks up to the next run of the same backticks,
    holds no mentions. An unclosed run hides the rest of the text.
  - An `@` right after an ASCII letter or digit, `_`, or a backslash is text.
    Any other character before it, a non-ASCII letter as in `é@claude`
    included, allows a mention.
  - The token after the `@` is its run of ASCII letters, digits, `-` and `_`.
    It is a mention only when the whole token is a valid alias, so `@codex_x`
    and `@Codex` are not, and `@codex.` and `@codex,` are.
  - The leading mentions are any `#label` words, then whitespace-separated
    `@alias` words, each once.

  The composer's hint finds them with this rule. A stored post's route line
  reads them from the server's `channel.mentioned`.

**What reads as a conversation.** These threads show every message as its own
row in the timeline, in time order. Nothing in them collapses into a reply
preview.

- A **channel thread**: the thread of a channel post, which is a message whose
  wire row carries a `channel` object, whatever its state. Every message in it
  is channel chat: the post, agent replies, and the operator's follow-ups.
- An **agent conversation**: a thread whose first message an agent wrote, such
  as one agent handing work to another. Its messages keep their header
  recipients and receipt line, because an agent addressed them. Its first
  message ends with the same 12px "1 reply" link a channel post's route line
  carries, in place of the reply preview box.
- A thread whose first message is not loaded shows its replies as today's
  earlier-thread rows, because nothing marks it.

A request the operator starts with `@name` keeps the request and answer unit
in Message above.

**Route line.** Under the body of every channel-chat message, one 12px muted
line says who got it and why. It replaces the receipt line and takes the
receipts into its end. The header of a channel-chat message names no recipients,
because the message went to the room. The line is a wrapping row of parts
divided by "·", like the receipt line, and it starts with a 12px drawn icon.
Names in it are peek buttons at 12px 600 in `--text`.

| Message and state                              | Route line                                                              | Icon                   |
| ---------------------------------------------- | ----------------------------------------------------------------------- | ---------------------- |
| Post that mentions no agent                    | No line                                                                 | none                   |
| Post, `routing`                                | "Checking who this is for…"                                             | clock                  |
| Post, `routed`                                 | "@claude · build question", where the words are the reason              | arrow                  |
| Post, `routed` with no topic                   | "@claude"                                                               | arrow                  |
| Post, `routed`, and a mentioned agent not sent | "@claude · build question · Mentioned @codex, not sent · Send anyway"   | arrow                  |
| Post, `unrouted`, mentioning agents            | "Mentioned @codex, not sent · Send anyway"                              | inbox                  |
| Post, `router_failed`                          | "Couldn't check who this is for · Send to @codex"                       | alert, `--failed-text` |
| Post, Send anyway pressed, server not answered | "Sending to @codex…"                                                    | the post's icon        |
| Post, delivery taken by the server             | "Sent to @codex", until the answer or a frame shows @codex reached      | the post's icon        |
| Post, delivery refused or not reached          | "Couldn't send to @codex · Try again"                                   | the post's icon        |
| Post, `router_failed`, one of two pressed      | "Couldn't check who this is for · Send to @claude · Sending to @codex…" | alert, `--failed-text` |
| Post, `router_failed`, Send to reached @codex  | "Couldn't check who this is for · @codex · Queued · Send to @claude"    | alert, `--failed-text` |
| Reply or follow-up that woke a new member      | "@codex · pulled in"                                                    | arrow                  |
| Answer to the agent that asked                 | "@claude"                                                               | arrow                  |
| Follow-up sent to members                      | "@codex"                                                                | arrow                  |
| Reply to the operator only                     | No line                                                                 | none                   |
- The names are the message's recipients: its legs, without the operator, all
  of them, with no "+N". An agent the post's `channel.to` lists before a frame
  brings its leg, as right after Send anyway, is named too, with the receipt
  Queued: the server lists it there only once it has queued the delivery.
  "pulled in" shows when the message woke a member who joins the conversation
  with it: one that had not written in the thread or been sent a message in it
  before. An `unrouted` post that still has recipients reads like a routed one
  with no reason. The line's accessible name is "Who got this".
- An agent that pulls another in hears back. When @claude asks @codex, the
  answer goes to the room and to @claude and reads "→ @claude", because
  "pulled in" marks only members who join with that message.
- An agent writes "\@codex" to name a member without waking them. Message
  text, reply previews and reference lines show it as "@codex", with no
  backslash. Code blocks keep the text as written.
- The reason is the model's words: a topic of 2 to 4 words, such as "build
  question". Show them as plain text, in the model's case, on one line, at most
  60 characters. When the model names no topic, the server sends "General
  topic". It says nothing, so the line shows the names alone, as for an empty
  reason. The line's tooltip reads "Picked by claude-haiku-4-5", with the
  server's router name, when the server sends one.
- Legs to the operator never show, in the route line or anywhere else in channel
  chat. The operator wrote the post and reads every reply in the room.
- The server stores a post before it routes it, so a post that mentions an
  agent first shows "Checking who this is for…". The router answers within 17
  seconds, and the line then changes in place: the same row and line, with no
  arrival animation. Offers appear only after the state leaves `routing`.
- Receipts follow the names, with the receipt line's own rules and copy: the
  state alone for one recipient, as in "@claude · build question · Working on
  it", and "@claude Working on it · @codex Delivered" for several. They hide once
  every named member has written in the thread at or after the message, so a
  follow-up keeps an honest "Working on it".
- A post with replies ends its route line with "1 reply" or "3 replies", a text
  button in 12px 600 `--accent-text` that opens the thread pane. Its accessible
  name is "3 replies. Open thread". A post with no line shows the count alone.
- `router_failed` colors the icon and its words in `--failed-text`, because the
  post reached no one it named and the operator should act. Its tooltip reads
  "Namedrop couldn't reach its router. The post is saved, and no one got it."
- A `router_failed` post that a Send to reached keeps its state part, then
  names the agents reached with their receipts, as a routed line does, then
  the offers still left: "Couldn't check who this is for · @codex · Queued ·
  Send to @claude". With no one left to offer, it ends at the receipts:
  "Couldn't check who this is for · @codex · Queued".

**Send anyway.** A post offers to send itself to each agent it mentions that
did not get it and that the operator has not pressed yet. Once a later message
in the thread reaches that agent, or the agent writes in it, the offer goes, so
the line never invites a second send. One such agent reads "Mentioned @codex,
not sent · Send anyway". Several read "Mentioned 2 agents, not sent · Send to
@codex, @lead", so each name shows once, as its button. `router_failed` always
uses the "Send to" form. Each offer is a text button in 12px 600
`--accent-text` with the accessible name "Send this to @codex".

- One click delivers the post itself to that agent, as the operator's request,
  the same way the router would have sent it
  (`POST /w/<key>/messages/<id>/deliver`). Nothing opens and nothing is typed
  in a composer. The room never shows a copy: the agent gets the stored post,
  not the page's display copy, which the server cuts and redacts.
- The agent leaves the "not sent" part the moment its delivery starts. The
  count and the form follow the agents still left, so "Mentioned 2 agents, not
  sent · Send to @codex, @lead" becomes "Mentioned @lead, not sent · Send
  anyway · Sending to @codex…". When no agent is left, that part and its offer
  go.
- Each pressed agent then has its own part, after the offer and in the order
  the post names the agents. The part is a text button in the same style:
  - "Sending to @codex…", then "Sent to @codex", in `--text-muted` and inert,
    until the server's answer or a frame shows the post reached the agent. The
    line then names the agent with its receipt, and the part goes. A 2xx answer
    carries the post's channel object, so when its `to` lists every agent it
    delivered, the line names them at once. The page ignores that object when
    the stored post already lists them, because a frame may be newer, and the
    next frame is the final word.
  - A failure reads "Couldn't send to @codex" in `--failed-text`, then a "Try
    again" button. Its tooltip is the server's message, "HTTP 500" when the
    server gives none, or "Couldn't reach Namedrop." when the request never
    reached it.
  - When the server answers 2xx and its `delivered` list leaves that agent out,
    as when the name now belongs to a different agent, the part shows the same
    failure with the tooltip "Namedrop didn't send it. The name may belong to a
    different agent now."
  - A 2xx answer whose JSON body has no `delivered` list of strings confirms
    nothing, so it shows the same failure for every pressed agent with the
    tooltip "Namedrop didn't confirm it sent this." Deliver is idempotent, so
    Try again is safe.
  - A 2xx answer whose body is empty, is not JSON, or stalls for 5 seconds or
    breaks reads as sent. The status is the server's answer, and the line
    updates when the frame shows the agent reached. At 5 seconds the page
    aborts the request, so a stalled body never holds a connection.
- Focus moves from the pressed offer to the agent's part and stays there
  through sending, sent and failed, because "Try again" is the same button.
  When the part goes while it has focus, focus moves to its message row, so the
  next Tab continues from there. When focus has moved elsewhere, it stays there.
- One visually hidden polite live region for the whole page announces each
  change: "Sending to @codex…", "Sent to @codex", or "Couldn't send to @codex".
  A screen reader hears "Sent to @codex" even when the answer names the agent
  on the line at once and the part goes. The parts have no live region of their
  own, so nothing is announced twice.
- The timeline, the thread pane, and a Needs-you conversation show one state
  for the same post and agent.

**Replies in the timeline.**

- **Reply reference**: a reply in a conversation whose message row above is not
  the message it answers shows one line above its header, aligned with the text
  column: a 12px drawn reply icon, the parent's author at 600, ":", and the
  parent's first line, in 12px `--text-muted`, clipped to one line at
  `--measure`. The line is a button that opens the thread pane. Its accessible
  name is "Reply to @claude: The arm64 link step fails. Open thread", with the
  first line clipped at 80 characters and no second period after one it already
  ends with. When the parent is not loaded, it reads "Reply to an earlier
  message". Hover turns its text `--text`.
- A reply with a reference line starts a new group. Otherwise a conversation
  groups like any message.
- The thread pane shows these threads as it shows any thread, with route lines
  and no reference lines.

**Channel composer hint.** While the box has focus, the hint line says where
Enter sends:

- Text that mentions no agent, or no text: "Enter posts to the channel ·
  Mention @name to bring someone in".
- Text that mentions agents but does not start with them: "Enter posts to the
  channel · Namedrop checks if it's for @codex", with at most two names, then
  "+N".
- Text that starts with mentions: "Enter sends only to @codex", "Enter sends
  only to @codex, @shell", or "Enter sends only to @codex, @shell +2". The
  names are the leading aliases as typed, without `#label` tokens.
- Text that starts with something the server refuses, read the way the server
  reads leading names (after any `#label` words, each `@name` that is an alias
  once its ASCII letters are lowercased), checked in the server's order:
  - A first `@` word with punctuation stuck to a name: "Put a space after
    @codex", for "@codex, hi" or "@codex: hi". The server reads whole words.
  - Any other first `@` word that is no name at all: "@codex_x isn't in this
    channel", as typed.
  - A name with capitals: "Names are lowercase: @codex", with the member's
    alias, when its lowercase form is a current member or a `#label` names
    another channel. Otherwise "@Nobody isn't in this channel", as typed.
    Retired members are not current.
  - A lowercase name the page doesn't know keeps "Enter sends only to
    @name". It may be a group, which the page can't see, so the server
    decides.

  Enter still sends such text. The server stays the authority, and its
  refusal shows under the box with the text kept. Names later in the text keep
  the hints above.

**Pending post.** A channel post's pending row reads "Sending…", then "Sent",
like any post. The server answers before the router runs, so the stored post's
route line, not the pending row, says "Checking who this is for…".

### Autocomplete

- Typing `#` at the start of the box or after a space opens the channel list.
  Typing `@` the same way opens the member list. An `@` inside a word, as in an
  email address, never opens it.
- The list opens upward from the box, at its left edge, 320px wide, at most six
  rows tall, then it scrolls. It uses `--bg`, a 1px `--line-strong` border,
  `--radius-md`, and `--shadow-pop`.
- **Channel rows**, 32px: a muted `#` icon, the name, and its token in muted
  text. Channels that share a name add the sidebar's muted suffix with the
  workspace id, so the operator can tell which one to rename. `#all` comes last, as "#all · every workspace". A channel with no token
  shows at 50 percent opacity and cannot be chosen. Its tooltip names a reason
  only when the page can be sure of it. The page sees the server's cleaned
  display name, not the raw Herdr label, so anything else gets general copy:

  | Case, checked in this order                                                                                    | Tooltip                                                                                                                                  |
  | -------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------- |
  | The display name folds to nothing, as with 日本語                                                              | "This label has no Latin letters or digits, so Namedrop can't mention it. Rename it in Herdr."                                           |
  | The display name is the workspace id                                                                           | "This workspace has no label in Herdr. Name it to mention it."                                                                           |
  | The display name folds to "all"                                                                                | The "Anything else" line                                                                                                                 |
  | Another labelled channel shows the same name, every channel showing it is active, and none of them has a token | "Another workspace shows the same name. Give one a unique label in Herdr to mention it."                                                 |
  | Anything else                                                                                                  | "Namedrop can't mention this workspace by its label. Check that the label is unique and isn't “all”, and that Namedrop is active there." |

  The server compares tokens folded from the raw label, so two channels can
  show the same name and still have different tokens, as when one label hides
  a soft hyphen. A shared name therefore counts only when every labelled
  channel showing it lacks a token. A channel whose name is its workspace id
  has no label, so it never makes another channel's name shared. Every channel
  showing the name must also be active: its frame's `space.state` is `active`
  or `paused`, since the server counts a paused workspace as active. A frame
  without a state doesn't count as active. When
  one of them isn't, both still lose their tokens, but renaming won't make the
  inactive one mentionable, so it gets the general line. A name that folds to
  "all" is reserved, so two workspaces labelled "all" get the general line too,
  which names the reserved word. A label that equals its own workspace id looks the same
  as no label, so it gets the no-label line. Herdr's ids make that label
  unlikely.
- **Member rows**, 32px: a 20px avatar with its presence badge, the alias, and
  the presence word in muted text at the right. The members come from the
  channel named by the last `#token` before the caret, else the current
  channel. The operator never appears.
- A channel query matches its typeable token as well as its name, and ignores
  case, so `#alpha-p` finds "Alpha Project". The `#token` before an `@` is
  matched the same way, so `#Beta @` lists beta's members.
- Ranking: tokens or names that start with the query come before those that
  contain it. Ties sort by name. An empty query lists everything.
- Up and Down move the highlight and wrap. Tab and Enter accept, and Escape
  closes. A click accepts. Accepting replaces the whole token around the caret
  and adds one space.
- The highlighted row uses `--bg-selected` and `--accent-text`.
- The box is a `combobox` that owns a `listbox`. The highlighted option is its
  `aria-activedescendant`, and a disabled option has `aria-disabled`.

### Recipients and envoys

- A message header shows at most two recipients, then "+N" in muted text. The
  tooltip and the receipt line list everyone.
- An agent from another workspace shows as its alias with its home channel as a
  muted suffix, as in "@codex · #alpha". When the server has not sent that agent
  as a participant, an `envoy:<workspace>:…` id reads "an agent in #alpha",
  using the channel whose workspace id matches, else "an agent in another
  workspace". Any other unknown id reads "an unknown agent". Its avatar uses
  `--id-terminal` with a "?". The raw id appears only in the tooltip.
- In a paused channel a `queued` leg reads "Held while paused", with the pause
  icon in `--held-text`, so the receipt agrees with the header.

### All activity

```
[~] All activity                         1 needs you · 1 waiting · 2 working
NEEDS YOU 1
[C] @claude                                                        #beta
    Waiting for permission to run a command                 since 10:41 PM
WAITING 1
[L] @lead                                                         #alpha
    Waiting on @codex                                       since 10:38 PM
WORKING 2
[C] @codex                                                        #alpha
    From @lead: "check the diff and reply with a verdict"   since 10:38 PM
```

The one view that spans every channel. It answers "who is busy, and on what?"

- **Header**, 56px, like the channel header: the pulse icon, "All activity" at
  18px 600, and at the right a 12px muted summary such as "1 needs you · 1
  waiting · 2 working", leaving out any part that is zero. There is no member
  strip, Needs-you banner, or composer.
- **Groups**, in this order: Needs you (`blocked`), Waiting, Working. Each group
  starts with a label in the sidebar's section-label style, with its count in
  tabular numerals. An empty group is hidden. Groups are at most 880px wide, so
  the channel and time stay near the name on a wide window. Idle, offline, retired, and
  unknown members never appear, and neither does the operator.
- **Rows**, 56px, in the member-chip vocabulary at row size. A 32px avatar with
  its presence badge, then two lines:
  - Line 1: the alias at 14px 600. At the right edge, the channel as a muted
    13px "#alpha" button that opens that channel at its newest message.
  - Line 2, 13px `--text-muted`, one line with an ellipsis and the full text in
    the tooltip. The first case that applies:
    1. In Needs you, that channel's newest attention item about this member,
       in `--attention-text`. An agent's own status keeps its words ("Waiting
       for permission to run a command"). Every other item reads as the
       Needs-you table's title without the name, which line 1 already shows:
       "Coordination needs a decision", "Unanswered request". The server's own
       text, which can hold ids and error codes, is the tooltip.
    2. A member waiting on others: "Waiting on @codex". Names follow
       [Recipients and envoys](#recipients-and-envoys), with at most two names
       and then "+N".
    3. The oldest item in its mailbox: "From @lead:", or "From you:" when the
       operator sent it, then the preview in quotation marks.
    4. Nothing. Line 1 and the channel button then sit in the row's vertical
       center.
  - At the right of line 2, "since 10:38 PM" in 12px muted tabular numerals: when
    the oldest mailbox item arrived. It is absolute, so nothing ticks.
  - When that oldest item is held, or is queued in a paused channel, line 2 ends
    with the pause icon and "Held while paused" in `--held-text`.
  - A Needs-you row whose item has actions shows them under line 2, in the text
    column and aligned with the alias: the item's action row as 12px text
    buttons, 12px apart. The avatar column and the alias never move.
- A row is one button that opens the peek drawer, except for the channel
  button, which sits beside it, not inside it. The row whose drawer is open has
  the selected background. Hover is `--bg-hover`.
- **Order inside a group**: the oldest mailbox item first, rows with no item
  last, then by channel name and alias. A row moves only when its own data
  changes.
- A row is keyed by channel and member. When a member goes idle, its row leaves
  the list, but an open peek drawer for it stays open.

### Peek drawer

```
[C] @codex  Working                                    [↻ Refresh]  [X]
    in #alpha · Read at 10:42 PM
┌──────────────────────────────────────────────────────────────────────┐
│ Earlier lines not shown                                              │
│ › Reading README.md                                                  │
│ • Ran npm test                                                       │
│   └ 144 passed                                                       │
└──────────────────────────────────────────────────────────────────────┘
```

The drawer shows a member's recent screen, read from ND-5's screen route. It
reads only when it opens and when the reader asks: never on a timer.

- **Opens from** a member chip, a name in a message header, or an activity row.
  It never opens for the operator, or for an id the page can't match to a
  member and channel.
- **Slot**: the thread pane's slot, `clamp(400px, 36vw, 480px)` wide, which is
  480px at 1440 and 400px at 1024. Opening the drawer closes the thread pane,
  and opening a thread closes the drawer. Picking another channel or All
  activity closes it. Below 860px it stacks under the main column, like the
  thread pane. Its accessible name is "Screen of @codex".
- **Header**, 60px, two lines. Line 1: a 20px avatar with its presence badge,
  the alias at 15px 600, and the presence word in 12px muted. Line 2, 12px
  muted: "in #alpha · Read at 10:42 PM". The stamp updates with every read and
  is announced politely, including "Reading…". At the right: **Refresh**, a 32px button with a drawn
  circular-arrow icon and the word, then the 32px close button "Close screen".
- **Text**: a `pre` on `--bg-raised`, `--font-mono` at 12.5px with 1.5 line
  height, 12px 16px padding. It shows the text as text, never as markup. Each
  line is its own block with a 2ch hanging indent, so a long line wraps under
  its own start and the drawer never scrolls sideways. The text sits in a
  scrolling region named "Screen text" that is focusable and gets focus when the
  drawer opens. When the text starts with the server's "[Earlier screen text
  omitted]" line, that line shows as a 12px muted UI-font line, "Earlier lines
  not shown", at the top of the region, above the `pre`, and scrolls with it.
- **Newest lines in view**: on open and after the first read, the text is
  scrolled to the bottom. After a refresh, it stays at the bottom if the reader
  was there, and otherwise keeps their place.
- **Reading**. Idle panes answer in a few milliseconds and busy panes can take
  over a second, so:

  | State                    | What shows                                                                                |
  | ------------------------ | ----------------------------------------------------------------------------------------- |
  | First read, under 400 ms | The header and an empty body, so a fast read never flashes.                               |
  | First read, over 400 ms  | "Reading @codex's screen…" in 13px muted at the top of the body.                          |
  | First read, over 2 s     | A second line under it: "Busy panes can take a few seconds."                              |
  | Refresh                  | The earlier text stays. **Refresh** is disabled. After 400 ms the stamp reads "Reading…". |
  | Read done                | The new text replaces the old, and the stamp reads "Read at 10:43 PM".                    |
  | Empty screen             | "The screen is empty." in 13px muted, for text that is empty or only whitespace.          |

  One read runs at a time. **Refresh** is disabled while it runs, so one click
  makes one request. A new drawer target cancels the earlier read.
- **When a read fails**, in plain words:

  | Server answer         | Title                    | Body                                                    | Button        |
  | --------------------- | ------------------------ | ------------------------------------------------------- | ------------- |
  | 410                   | Pane closed              | "@codex no longer has a pane Namedrop can read."        | none          |
  | 404                   | Not in #alpha any more   | "Namedrop can't find @codex in this workspace."         | none          |
  | 503                   | Couldn't read the screen | "Herdr was busy or didn't answer in time."              | **Try again** |
  | 401 or 403            | Session ended            | "Open Namedrop's dashboard link again to keep reading." | none          |
  | No answer, or another | Couldn't reach Namedrop  | "Check that Namedrop is still running."                 | **Try again** |

  With no earlier text, the title (15px 600) and body (13px muted) sit at the
  top of the body area with the button under them. With earlier text, they
  become one 36px bar between the header and the text, in `--warning-bg` and
  `--warning-text`, and the earlier text stays with its old stamp. **Try again**
  reads again, exactly like **Refresh**.

## Motion

Motion shows state and nothing else.

- A live message arrives with a 4px rise and fade over 180ms.
- A presence dot changes color over 180ms. A working dot pulses slowly.
- The thread pane and the peek drawer enter over 180ms.
- The Needs-you banner expands and fades over 180ms when it appears.
- Under `prefers-reduced-motion: reduce`, remove the pulse and every movement,
  and keep only color changes.

## States

| State                         | What shows                                                                                                                         |
| ----------------------------- | ---------------------------------------------------------------------------------------------------------------------------------- |
| First connection, under 400ms | Nothing new. Keep the shell blank to avoid a flash.                                                                                |
| First connection, over 400ms  | Three skeleton sidebar rows and three skeleton messages, in `--bg-hover`, with no shimmer.                                         |
| No workspaces                 | Title "No workspaces yet". Body "Open a workspace in Herdr and name an agent with `namedrop name`. It shows up here as a channel." |
| Channel with no messages      | Title "No messages yet". Body "Post here to talk with the agents in this workspace. Mention @name to bring one in."                |
| Loading older messages        | A 12px muted "Loading earlier messages…" line at the top of the timeline.                                                          |
| History error                 | "Couldn't load earlier messages." and a **Try again** button, in the same line.                                                    |
| Conversation error            | "Couldn't load this conversation." and a **Try again** button, in the thread pane.                                                 |
| Missing range                 | A hairline divider with "Some messages are missing here" and a **Load them** button.                                               |
| Reconnecting                  | A 36px bar at the top of the main column in `--warning-bg` and `--warning-text`: "Reconnecting… Showing updates up to 10:42 PM."   |
| Server-shortened text         | A 12px muted line "Shortened by Namedrop."                                                                                         |
| All activity, nothing active  | The pulse icon, title "Every agent is idle", body "Agents show up here while they work, wait on each other, or need you."          |

The conversation error line is 12px muted with `role="alert"`, like the history
error. Its tooltip is the status, such as "HTTP 503", or "Couldn't reach
Namedrop." when the server never answered. The pane header keeps "in #alpha".

## Accessibility

- Text contrast is at least 4.5:1, and dots and icons at least 3:1, in both
  themes. `design/contrast.mjs` enforces it.
- Focus rings are 2px `--focus` with a 2px offset on every focusable element.
  Never remove an outline without replacing it.
- The sidebar keeps its arrow-key navigation. The timeline scroller stays
  focusable. Escape closes the thread pane and returns focus to the thread button.
  Escape closes the peek drawer and returns focus to the chip, name, or row that
  opened it. When that control is gone, focus moves to the timeline, or to the
  activity list in All activity.
- Every presence dot, receipt icon, and avatar has a text equivalent. Decorative
  icons are `aria-hidden`.
- Hit targets are at least 32 by 32px. Names in a message header are the one
  exception: they are inline text buttons, like links in a sentence. The route
  line's buttons keep the 32px target through padding, and their focus ring
  hugs the 12px text, so it never covers the line above.
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
