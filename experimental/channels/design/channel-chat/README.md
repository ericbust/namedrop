# Channel chat review media (CH-2)

These files are the fixture review of CH-2. The design owner (`@chn-design`)
captured them from the built app against a fake server. That server sends the
wire contract in `docs/channel-chat-plan.md`, because the server side (CH-1) is
still being built. The scripts live outside the repository in
`~/.cache/chn-design/ch2/scripts/`: `review-server.mjs` holds the scene,
`shots.mjs` takes the screenshots, `video.mjs` records the video, and
`overflow.mjs` probes for page scroll.

## Files

| File | What it shows |
| --- | --- |
| `walkthrough.mp4` | 69 seconds at 1440 by 900. A post mentions @claude and routes to @claude, and @claude pulls in @codex. A plain post reaches no one. A passing mention offers Send anyway, which opens the thread filled in. A thread reply with no mention goes to @codex. A leading `@codex` request gets its answer box. |
| `chat-{1440,1024}-{light,dark}.png`, `chat-top-*.png` | The whole scene, with every route state, in both themes and at both widths. |
| `state-<id>-{light,dark}.png` | One message per state. m1 is routed with a reason and a reply count. r1 woke @codex, so it reads "pulled in". r2 is a reply to the operator, with no line. a1 and a2 are an agent conversation. m2 is a plain post, with no line. m3 is a request. m7 is a passing mention with Send anyway. m4 is router_failed. r3 has a reply reference. m8 is routed and also mentions an agent it did not reach. m5 has two recipients. m6 is still checking. |
| `composer-{plain,mention,leading}-*.png` | The three channel composer hints. |
| `sendto-*.png` | After Send anyway: the thread is open, the box is filled and focused with the caret at the end, and no member list shows. |
| `thread-*.png` | A channel thread in the pane. It has route lines, the composer reads "To @claude, @codex", and a reply with no mention can be sent. |
| `pending-mention-*.png` | A post that mentions an agent, pending with "Checking who this is for…". |
| `page-fill-{before,after}-*.png`, `page-fill-*.json` | The page-scroll bug before and after the fix, at 1440, at 640, and at 125% zoom. |
| `shots-log.json`, `video-log.json` | The checks behind the screenshots, and the video's timeline marks. |

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
4. Check on CH-1's head: how the operator leg id looks, whether `channel` and
   `woke` match the contract, whether the `routing` state can ever show, and
   whether a thread reply with no mention reaches the thread's agents. The page
   no longer adds mentions to such a reply, so a server without D11 refuses it.
