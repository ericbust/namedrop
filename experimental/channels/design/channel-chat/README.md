# Channel chat review media (CH-2)

These files are the fixture review of CH-2. The design owner (`@chn-design`)
captured them from the built app against a fake server. That server sends the
wire contract in `docs/channel-chat-plan.md`, because the server side (CH-1) is
still being built. The scripts live outside the repository in
`~/.cache/chn-design/ch2/scripts/`: `review-server.mjs` holds the scene,
`shots.mjs` takes the screenshots, `video.mjs` records the video, and
`overflow.mjs` probes for page scroll.

## Files

| File                                                  | What it shows                                                                                                                                                                                                                                                                                                                                                                                                                                      |
| ----------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `walkthrough.mp4`                                     | 69 seconds at 1440 by 900. A post mentions @claude and routes to @claude, and @claude pulls in @codex. A plain post reaches no one. A passing mention offers Send anyway, which opens the thread filled in. A thread reply with no mention goes to @codex. A leading `@codex` request gets its answer box.                                                                                                                                         |
| `chat-{1440,1024}-{light,dark}.png`, `chat-top-*.png` | The whole scene, with every route state, in both themes and at both widths.                                                                                                                                                                                                                                                                                                                                                                        |
| `state-<id>-{light,dark}.png`                         | One message per state. m1 is routed with a reason and a reply count. r1 woke @codex, so it reads "pulled in". r2 is a reply to the operator, with no line. a1 and a2 are an agent conversation. m2 is a plain post, with no line. m3 is a request. m7 is a passing mention with Send anyway. m4 is router_failed. r3 has a reply reference. m8 is routed and also mentions an agent it did not reach. m5 has two recipients. m6 is still checking. |
| `composer-{plain,mention,leading}-*.png`              | The three channel composer hints.                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `sendto-*.png`                                        | After Send anyway: the thread is open, the box is filled and focused with the caret at the end, and no member list shows.                                                                                                                                                                                                                                                                                                                          |
| `thread-*.png`                                        | A channel thread in the pane. It has route lines, the composer reads "To @claude, @codex", and a reply with no mention can be sent.                                                                                                                                                                                                                                                                                                                |
| `pending-mention-*.png`                               | A post that mentions an agent, pending with "Checking who this is for…".                                                                                                                                                                                                                                                                                                                                                                           |
| `page-fill-{before,after}-*.png`, `page-fill-*.json`  | The page-scroll bug before and after the fix, at 1440, at 640, and at 125% zoom.                                                                                                                                                                                                                                                                                                                                                                   |
| `shots-log.json`, `video-log.json`                    | The checks behind the screenshots, and the video's timeline marks.                                                                                                                                                                                                                                                                                                                                                                                 |

## Design verdict: PASS with follow-ups

Every row of the route table in DESIGN.md "Channel chat" renders as designed, in
both themes, at 1440 and 1024. No layout scrolls the page or scrolls sideways.
Contrast passes, and `impeccable detect` finds nothing in the changed UI files.
The channel reads as one conversation. Replies sit in time order, and a
reference line ties back any reply that is not next to its parent. Who got each
message, and why, sits in one quiet line under it.

Fixed during review:

- An agent conversation's first message showed a bordered "1 reply" box right
  above the reply it counted. It now ends with a small "1 reply" link.
- A post that missed several mentioned agents named each of them twice. It now
  reads "Mentioned 2 agents, not sent · Send to …".
- Send anyway kept offering an agent after the thread had already reached it.
  The offer now goes once a later message reaches the agent.

Follow-ups, none blocking:

1. A routed post with several recipients names them twice while receipts show,
   as in "@claude, @codex · review request · @claude Working on it · @codex
   Delivered". It is rare, because the router prefers one agent.
2. In the 400px thread pane, the composer's "To" label clips two names
   ("To @claude, @co…"). The accessible name lists everyone. The clipping comes
   from the label's existing width.
3. The follow-up that Send anyway posts repeats the post's text right under the
   post. It is honest, but it would read better if the server could add a
   recipient to the existing post.
4. Answered on CH-1's head (`f148c8e`, merged): the server sends operator legs
   as `human`; `channel` carries `state`, `to`, `mentioned`, `reason`, and
   `router`, with bare aliases; `woke` is a list of bare aliases; the server
   never sends `routing`, because it routes before it answers the post; and a
   thread reply with no mention reaches the thread's agents. Send anyway now
   offers the agents in `channel.mentioned`.

## Live on CH-1's head

`live/` holds the run on a testbed with real Claude Code 2.1.310 and Codex
0.164.0. The testbed was made by `testbed test --link . --keep`, which passed all
10 of its checks at f9c79c4. `live/live-log.json` holds every post's receipt.

| Scenario                     | What happened                                                                                                                                                                                       | Shot                 |
| ---------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | -------------------- |
| Plain post, no mention       | `unrouted`, nothing mentioned, no deliveries, and no route line. Neither agent answered in 20 s.                                                                                                    | `live-plain.png`     |
| Post that mentions an agent  | Routed to @claude, which answered in the room. The line then read "@claude · reason · 1 reply".                                                                                                     | `live-mention-*.png` |
| Thread reply with no mention | The text was sent as typed, with `in_reply_to`. The label read "To @claude", and @claude answered.                                                                                                  | `live-thread-*.png`  |
| An agent pulls another in    | @claude's reply mentioned @codex and read "@codex · pulled in · Working on it". @codex answered "pong" in the room.                                                                                 | `live-pullin-*.png`  |
| Passing mention              | `unrouted` with `mentioned: ["codex"]`, which read "Mentioned @codex, not sent · Send anyway". Send anyway filled the box with no member list. @codex answered, and the offer went away.            | `live-passing*.png`  |
| Router failure               | The dashboard ran with a router stub that exits 1. The post was kept as `router_failed`, and the line read "Couldn't check who this is for · Send to @claude" in the failed tone, with its tooltip. | `live-failed.png`    |

`live-walkthrough.mp4` (66 s) is the live run. The 20 s wait after the plain
post plays at three times speed. `live-channel-{1024,1440}-{light,dark}.png`
show the finished channel, and the page did not scroll at any of those sizes.

Live verdict: **PASS with follow-ups**. The room reads as one conversation with
real agents, and every state renders as designed. One fix landed from the live
run: the "2 replies" link's focus ring covered the line above it after a thread
closed. It now hugs the text.

Follow-ups from the live run:

5. **Router reasons**: they read like tickets and repeat the agent's name, as in
   "@claude · Post asks @claude to provide information". The plan's form is a
   short topic, such as "build question". This belongs to CH-1's router prompt
   round: ask for two to four words that name the topic, without the agent or
   "Post asks". The page shows the reason as the server sends it.

## Final integration with the cleanup round

`final/` holds the smoke run after merging exp/channel-core 4b12a26, which adds
the router fixes and the cleanup round (#70 to #76). The testbed was made by
`testbed test --link . --keep`, which passed all 10 of its checks at 34b5fce,
with real Claude Code 2.1.310 and Codex 0.164.0. `final/live-log.json` holds
every post's receipt.

| Scenario                     | What happened                                                                                                                                                                      | Shot                         |
| ---------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------- |
| Plain post, no mention       | No route line, and neither agent answered in 20 s.                                                                                                                                 | `final-1-plain.png`          |
| Post that mentions @claude   | Routed by claude-haiku-4-5 with the topic "repository description". The line read "@claude · repository description · Queued", and @claude answered in the room.                   | `final-2-*`, `final-3-*`     |
| Post that mentions @codex    | Topic "definition request". @codex answered in the room.                                                                                                                           | `final-4-codex-answered`     |
| Thread reply with no mention | The label read "To @claude". The text was sent as typed with `in_reply_to`, and @claude answered.                                                                                  | `final-5-thread-answered`    |
| Needs-you item               | "@claude didn't answer a request" opened after an interrupted turn. Show message opened the thread on the tinted request. Respond prefilled "@claude ", and @claude answered "ok". | `final-6-*` to `final-9-*`   |
| Recently closed member       | Closing @codex's pane turned its chip into "@codex Retired". Its peek read "Pane closed", and its earlier post kept "@codex · definition request · 1 reply".                       | `final-11-*` to `final-13-*` |

`final-14-{1440,390}-{light,dark}.png` show the finished channel. The page did
not scroll at either size.

The driver's wait for @claude's "ok" timed out because it looked only in the
main timeline, where a request-thread reply shows as a preview card. The reply
itself arrived within seconds; `live-log.json` records the step as the driver
saw it and the facts from the receipts and screenshots.

Verdict: **PASS with follow-ups**. Channel chat and the cleanup features work
together. Router topics now read as short anonymous labels, which closes
follow-up 5. When the model names no topic, the server sends "General topic";
the page reads that as no reason and shows the names alone.

Two fixes landed from an independent review of the merge:

- Send anyway inside a Needs-you conversation fills that pane's reply box. It
  used to close the pane when the thread was outside the loaded timeline.
- Respond's composer label names every agent that a plain reply reaches. The
  prefill still targets the responsible agent.

Follow-ups for the design polish PR, after DESIGN-R2:

6. **Reply preview at phone width**: below 600 px, the preview's time wraps
   under "2 replies" and the two overlap (`final-14-390-dark.png`).
7. **Receipt on an unanswered request**: the request reads "Done" while the
   Needs-you item says it was not answered. It should say the agent ended
   without replying.
8. **Item after a Respond answer**: the item stays open after the operator
   responds and the agent answers, because the interrupted request still has
   no disposition. Resolving it on that answer is a server change, for the
   cleanup owner.
