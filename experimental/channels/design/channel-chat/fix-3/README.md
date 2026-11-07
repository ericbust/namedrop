# Channel chat fix round 3 media (CH-fix-3)

These screenshots show Send anyway and Send to after fix round 3 and its first
design revision. One click asks the server to deliver the stored post itself,
and the pressed agent then reads as its own part of the route line. They come
from the built app, driven by headless Chromium against a fake server that
answers the agreed `deliver` route. The server side of the round is not pushed
yet. The script lives outside the repository, at
`~/.local/state/pstack/orchestrate/namedrop-ui/reports/CH-fix-3/evidence/scripts/shoot-fix3.mjs`.
It loads a fresh page for each scene.

Every state has four files: `<state>-<width>-<theme>.png`, at 1440 by 900 and
390 by 844, in the light and dark themes.

## Files

| State           | What it shows                                                                                                                                                          |
| --------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `offer-idle`    | A post routed to @claude that also mentions @codex: "@claude · build question · Working on it · Mentioned @codex, not sent · Send anyway".                             |
| `offer-sending` | After the click, while the server has not answered: the not-sent part and its offer go, and @codex's own part reads "Sending to @codex…" in `--text-muted`.            |
| `offer-sent`    | After the server answers: the part reads "Sent to @codex" until a frame shows the post reached @codex.                                                                 |
| `offer-failed`  | After a refusal: "Couldn't send to @codex" in `--failed-text`, then a "Try again" button whose tooltip is the server's message.                                        |
| `send-to-mixed` | A `router_failed` post after both offers: "Couldn't check who this is for · Sent to @codex · Couldn't send to @claude · Try again", one part for each agent, in order. |

`shots-log.json` records, for each step, the route line's text as it reads on
screen, with the dots between parts, the `data-offer` of the focused element,
the page's scroll position, and whether the page scrolled sideways. It also
records three steps with no screenshot: `offer-reached`, the line after the
frame that shows @codex got the post; `send-to-idle`, the `router_failed` line
before any press; and `send-to-pressed`, that line while @codex's delivery waits
for the server. Focus sits on the pressed agent's part from `offer-sending` on.
No file scrolls sideways.
