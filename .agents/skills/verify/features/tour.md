# Namedrop tour

Users page through a terminal introduction and can name their invoking pane from the first page. Herdr opens the tour once per presentation after an agent appears in an activated workspace.

## Sub-features

- `tour-pages` renders the welcome and mention pages and navigates between them.
- `tour-close` returns to the shell with Esc or Enter.
- `tour-name` opens naming from the first page's N action.
- `tour-once` opens automatically once and persists the presentation marker.
- `tour-reset` clears the marker so automatic opening can happen again.

## How to get to it (user POV)

- Activate Namedrop, then start an agent in the workspace for automatic opening.
- Press Prefix, release, Shift-O, or run `herdr plugin action invoke tour --plugin namedrop`.
- With no saved names, press T in Manage names or Doctor to page the tour in place.
- Run `namedrop ui reset-tour` to clear the current presentation's marker.

## Driving it with control-namedrop

Preconditions:

- The helper owns the server, workspace, and terminal IDs.
- Floating popup and shortcut drives require the plugin to be linked only in a disposable profile.

- Run `.agents/skills/verify/scripts/control-namedrop.py --feature tour`.
- The helper starts `namedrop ui run tour` in its native terminal with the exact invoking workspace, pane, and terminal environment. It captures `Welcome to Namedrop` in `tour-first.txt`.
- It sends `right` through `herdr pane send-keys` and requires `Mention an agent` in `tour-second.txt`. The key action is recorded in `commands.jsonl`.
- It sends `esc`, then runs `address list --json` in the same pane to prove the shell can accept another command. No names are created by paging.
- To cover the normal popup path, invoke `herdr plugin action invoke tour --plugin namedrop` in an owned linked profile, discover the new popup ID through `pane list`, and drive the same keys there. Record that path separately.
- For automatic opening, inspect the activation/event response's `tour` object and the fixture state's `tours-shown/welcome-1` marker. A second event must report `already_shown`; `ui reset-tour` clears the marker. The renderer helper does not prove event-driven opening.
- For in-place access, open the empty-name manager and Doctor separately, press T, then capture the same presentation without a new once-only marker.

## Gotchas

- `ui run tour` is the renderer behind the popup and requires `NAMEDROP_TARGET_WORKSPACE`, `NAMEDROP_TARGET_PANE`, and `NAMEDROP_TARGET_TERMINAL`. The helper supplies real IDs.
- The default drive proves the native renderer and keys, not the floating frame, prefix bindings, or automatic opening.
- Popup N names the invoking pane. Read the list afterward to prove it did not name the popup itself.
- Reset changes fixture state. Keep it scoped to the owned profile.
