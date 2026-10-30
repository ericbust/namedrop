# Channel chat fix round 3 media (CH-fix-3)

These screenshots show Send anyway and Send to after fix round 3, where one
click asks the server to deliver the stored post itself. They come from the
built app, driven by headless Chromium against a fake server that answers the
agreed `deliver` route. The server side of the round is not pushed yet. The
script lives outside the repository, at
`~/.local/state/pstack/orchestrate/namedrop-ui/reports/CH-fix-3/evidence/scripts/shoot-fix3.mjs`.
It loads a fresh page for each scene.

Every state has four files: `<state>-<width>-<theme>.png`, at 1440 by 900 and
390 by 844, in the light and dark themes.

## Files

| State           | What it shows                                                                                                                                 |
| --------------- | --------------------------------------------------------------------------------------------------------------------------------------------- |
| `offer-idle`    | A post routed to @claude that also mentions @codex: "@claude · build question · Working on it · Mentioned @codex, not sent · Send anyway".    |
| `offer-sending` | After the click, while the server has not answered: the same button reads "Sending…" in `--text-muted`, and nothing else on the page changed. |
| `offer-sent`    | After the server answers: the button reads "Sent" until a frame shows the post reached @codex.                                                |
| `offer-failed`  | After a refusal: "Couldn't send to @codex." in `--failed-text`, then a "Try again" button whose tooltip is the server's message.              |
| `send-to-mixed` | A `router_failed` post after both offers: "Send to @codex Sent, Couldn't send to @claude. Try again", one state for each agent.               |

`shots-log.json` records, for each file, the route line's text, the page's
scroll position, and whether the page scrolled sideways. No file scrolls
sideways.
