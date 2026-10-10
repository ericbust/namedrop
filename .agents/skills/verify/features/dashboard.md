# Dashboard

Users open a live, read-only browser view of one workspace's agents, messages, mailboxes, handoffs and coordinations. One loopback server serves every workspace's page. The first request starts it, later requests reuse it, and it exits ten minutes after its last open stream.

## Sub-features

- `dashboard-open` starts the server when none answers, and prints a one-time link to the invoking workspace's page.
- `dashboard-reuse` returns `server: running` with the same port on later calls, from the same or another workspace.
- `dashboard-entry` redeems the link once for a session cookie. A replayed link and a request without the cookie get `401`.
- `dashboard-live` streams `snapshot` frames over `/w/<key>/events` and sends a new frame after each committed change.
- `dashboard-spaces` lists every stored space with its label and state at `/`, through `/api/spaces`.
- `dashboard-idle` exits the server after the idle period, and rotates the session so old tabs must reopen through the CLI.

## How to get to it (user POV)

- Run `namedrop dashboard` in a workspace. It opens a browser when it can, and always prints the URL. `--no-open` only prints.
- Run `herdr plugin action invoke dashboard --plugin namedrop`.
- Open the printed `http://127.0.0.1:<port>/w/<key>?n=<nonce>` link. The browser lands on `/w/<key>` with the cookie set.

## Driving it with control-namedrop

Preconditions:

- The helper owns the server, two workspaces with separate Git checkouts, and the Namedrop state. No browser is needed.

- Run `.agents/skills/verify/scripts/control-namedrop.py --feature dashboard`.
- It names `@build` as a terminal, then runs `dashboard --no-open` in the first workspace's pane. The response must report `server: started`, `opened: false`, the fixture workspace, and a loopback URL on the port in `<state>/dashboard/server.json`.
- It redeems the link and requires `303` to `/w/<key>` with a `Set-Cookie`. Replaying the link and requesting the page without the cookie must both return `401`. With the cookie, the page returns `200` HTML.
- It reads the first `snapshot` frame from `/w/<key>/events`. The space must be the fixture workspace in state `active`, and the only terminal participant must be `@build`. The frame is saved as `dashboard-snapshot.json`.
- A second `dashboard --no-open` must report `server: running` with the same port and space key.
- It creates workspace B and runs `dashboard --no-open` from B's pane. The response must report `server: running`, the same port, and a different space key. Before activation, B's stream reports state `unknown` with B's label and no participants. After `activate` in B, the stream must deliver a frame with B's workspace ID in state `active`. `dashboard.json` keeps the responses and both space states.
- Teardown stops the dashboard server by the PID recorded in `server.json`, after checking that its command line names `dashboard-server` and the fixture state. `cleanup.json` records `dashboard_pid` and `dashboard_alive`.
- The page's rendering is a separate check. Open the printed URL in a browser, or run `python3 scripts/dashboard-preview.py --check` for the fixture snapshots.

## Gotchas

- A new stream first receives the frame the server already cached for that space. A change committed just before the stream opened arrives in the following frame. Wait for the expected frame rather than asserting on the first.
- A workspace that was never activated has no stored space, so its page reports `unknown` with an empty `workspace_id`. It changes to `active` once the workspace activates while the page is open.
- The server starts with the caller's state directory and runs detached in its own process group. Stopping the Herdr server does not stop it. Stop it by its recorded PID, never by process name.
- A development binary run against a state directory that has `runtime/current` re-executes into the installed runtime. Verify with the helper's isolated state, not the user's live state.
- The page never shows context payloads, evidence indexes or terminal output. A snapshot containing them is a defect.
