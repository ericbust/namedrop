# Channel chat fix round 2 media (CH-fix-2)

These screenshots show the new states of fix round 2. Round 3 replaced Send
anyway's note in the thread box with a direct delivery, so its screenshots moved
to `../fix-3/`. They come from the built
app, driven by headless Chromium against a fake server that sends the agreed
wire contract. The server side of the round is not pushed yet. The script lives
outside the repository, at `~/.cache/r2-ch-fix-2/scripts/shoot-fix2.mjs`. It
loads a fresh page for each scene.

Every state has four files: `<state>-<width>-<theme>.png`, at 1440 by 900 and
390 by 844, in the light and dark themes.

## Files

| State                 | What it shows                                                                                                                                                                 |
| --------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `routing`             | A stored post that mentions @claude while the router runs: the clock and "Checking who this is for…".                                                                         |
| `routed`              | The same row after a frame routes it: "@claude · build question · Working on it". The row and its line update in place.                                                       |
| `reply-limit-idle`    | A reply-limit item with its thread and actions: the hint "The agents paused after 10 replies in a row.", then Allow 10 more replies and Stop thread.                          |
| `reply-limit-confirm` | After Stop thread: "Stop this thread for good?", a Stop button in `--failed-text`, and Cancel.                                                                                |
| `reply-limit-pending` | After Allow 10 more replies, while the item waits on the frame: "Allowing…", with every action on the row disabled.                                                           |
| `reply-limit-failed`  | After a 500: "Couldn't allow more replies. Try again." under the row, and the buttons back.                                                                                   |
| `pull-in-loop`        | The pull-in loop in the room. @claude's ask reads "@codex · pulled in". @codex's answer reads "@claude", with a reference line to the ask. @claude's report back has no line. |

`shots-log.json` records, for each file, the page's scroll position and whether
the page scrolled sideways. No file scrolls sideways.
