# Namedrop in Herdr app

Namedrop shows named terminals in a roster, with the agent each one is waiting on. Press Alt+N to name a terminal and Alt+Shift+S to open the roster. A reply or new blocked status posts a notification. Select it to focus the affected terminal.

![Namedrop](preview.png)

## What you see

The roster is a 300 px window over the current layout. Rows show the alias, program icon, and space or tab when known. An open request replaces the location line with `waiting on @name`. A status dot shows the program's status. An empty roster prompts you to name a terminal. A read failure shows its message and a **Retry** button.

The 360 px native Name dialog captures the selected terminal when it opens. Saving calls the Namedrop binary with that terminal and its `herdr.space` label, or `headless` when the label is absent. It shows the binary's error message if naming fails. Only when both binary locations are missing does it use the old core label path.

## How it behaves

The roster merges live terminals with `namedrop --host hc roster --json`. The binary reads the message store without changing it and supplies each agent's `waiting_on`, attention count, and five most recent delivered replies. The mod watches the state directory returned by the binary for `namedrop.sqlite3*` changes. It reads at most twice per second during a burst. If the watcher fails, it retries. The first read seeds notification history, so existing replies do not trigger notifications.

The mod probes `${home()}/.local/bin/namedrop-app` with `--version`, then tries `namedrop` on the app session's `PATH` if the launcher is absent. Activation owns `namedrop-app`. Do not put a release binary at that path before activation. A desktop-launched app may have a different `PATH` from your shell.

The mod uses `exec` for the binary, `watchFolder` for store changes, `notify` for replies and errors, and `hc run` for terminal-backed popups. The binary's hidden `events --ensure` starts the existing subscriber when a space is activated. It returns inactive without starting anything for an unactivated space. The mod tries ensure at startup and after read failures, no more often than once every 30 seconds.

| Program status | Appearance |
| --- | --- |
| `blocked` or `error` | Blocked-color dot. |
| `working` | Working-color dot. |
| `done` or `idle` | The corresponding theme dot. |
| No report | No dot. |

Click a row to focus a terminal with a pane. A terminal without a pane opens in a peek dialog. The roster window stays open until closed. The app keeps its position per layout.

## Built from

This build targets Herdr app **0.1.9** and herdr-core **0.1.14**. Copy these six source folders into your app home's `mods/` directory:

- `mods/namedrop/` is the roster window.
- `mods/namedrop-name/` is the native Name dialog.
- `mods/namedrop-management/`, `mods/namedrop-doctor/`, `mods/namedrop-agent-status/`, and `mods/namedrop-tour/` are four identical terminal launchers apart from their popup identifier.

The launcher for `management` calls `ui run manage`; `agent-status` calls `ui run status`. Doctor and tour use `ui run doctor` and `ui run tour`. Each launcher runs `hc run --bg --json --rm` for its selected terminal and opens the returned terminal in an app window. A taken name reopens the existing window. The native Name dialog does not launch a terminal.

Merge these `keys.json` entries into the existing `global` object. Keep unrelated bindings:

| Linux chord | Action |
| --- | --- |
| `alt-n` | `mod.open:namedrop-name` |
| `alt-shift-s` | `mod.open:namedrop` |
| `alt-shift-m` | `mod.open:namedrop-management` |
| `alt-shift-d` | `mod.open:namedrop-doctor` |
| `alt-shift-o` | `mod.open:namedrop-tour` |

On macOS, Option plus a letter can type a character or start an accent. Option+N starts tilde composition on common layouts. If you need those input chords, rebind Name and roster to unused Command chords, and use `cmd-alt-m`, `cmd-alt-d`, and `cmd-alt-o` for management, Doctor, and tour. Check each chord against your existing bindings and the app's `option_as_alt` setting before editing `keys.json`.

## Watch out

The binary release archive contains no mods or keys. Obtain the matching source archive or checkout for those files. Keep the extracted release directory in a stable location; activation's launcher points to its binary. The app's installation and activation steps are in [README.md](README.md#install-in-herdr-app).

The roster's kept window and optional layout float belong to the app. To add a float, clone a layout with `herdr-app do layout.clone '{"name":"main","as":"namedrop-layout"}'`, then add a `floating` entry whose content is `{ "mod": "namedrop" }` to the cloned layout. The float has no Close button; remove that entry to remove it.

The hc host's own notification method remains a no-op. The app mod provides the reply and blocked-status notifications. The heavier per-pane Host read and the subscriber's existing scope remain tracked as #94 and #97.

## Done when

Run `node --test mods/*/*.test.js` and check all six folders on the target app version:

```sh
herdr-app check mods/namedrop
herdr-app check mods/namedrop-name
herdr-app check mods/namedrop-management
herdr-app check mods/namedrop-doctor
herdr-app check mods/namedrop-agent-status
herdr-app check mods/namedrop-tour
```

The final compositor checks should name two agent terminals, show a waiting line during a request, deliver one reply notification, open each terminal-backed popup, and save a name through the native dialog. The program's final verification lane owns those app and live checks.
