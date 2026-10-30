# Namedrop

Namedrop adds a roster of named terminals and a dialog for naming the terminal you are using. Press Alt+N to name a terminal and Alt+Shift+S to open the roster. Each row shows an alias, its program icon, and its space and tab when known. A status dot shows what the program reports.

![Namedrop](preview.png)

## What you see

The roster opens in a 300 px window over the current layout. A Namedrop heading and a Close button sit above a vertical list. Rows use 13 px text for the alias and 11 px muted text for the space and tab. Program icons are 18 px. Status dots are 7 px. The list scrolls after 420 px. The app supplies the frame, and the active theme supplies every color.

An empty roster explains how to name a terminal. If the Namedrop binary cannot start, a footer shows "Install the Namedrop binary to send mentions", the releases URL, and an Open releases button.

The 360 px Name dialog has a terminal address, an alias input, a normalization hint, and Cancel and Save name buttons. A failed save leaves the dialog open with its error. Without a focused terminal, the dialog explains how to select one and shows a Close button.

## How it behaves

The roster includes live terminals with a nonempty `namedrop.name` label. It reads raw OSC 7501 status words from the terminal, including terminals without a pane.

| Program status | Appearance | Sort group |
| --- | --- | --- |
| `blocked` | A dot in the theme's `blocked` color. | First. |
| `error` | Shown as `blocked`, with the same dot. | First. |
| `working` | A dot in the theme's `working` color. | Second. |
| `done` | A dot in the theme's `done` color. | Third, together with idle. |
| `idle` | A dot in the theme's `idle` color. | Third, together with done. |
| No report | No dot. | Last. |

Aliases sort alphabetically inside each group. A `done` report stays done until the program changes it. Shells and programs that never report a status, including Codex in the intended setup, still appear when named. Finished terminals disappear.

Click a row to focus its terminal when it has a pane, including a pane in another tab. A terminal without a pane opens in a peek dialog. Hover highlights the row. Native buttons support keyboard activation. The roster uses live app data without polling.

Alt+Shift+S opens the roster on any layout. Opening it again replaces the window with the same key and preserves its position. Drag its heading or unused space to move it. The window stays open until closed and reopens with the layout where you opened it, including after restart. Opening it on a different layout keeps a roster for that layout too. Close removes that layout's kept window.

Alt+N opens the Name dialog on any layout. It captures the focused terminal when it opens, so a later focus change does not retarget a save. Enter and Save name use the same action. While saving, the input becomes a static display of the submitted alias. `Code Reviewer` becomes `code-reviewer`. ASCII letters and digits are accepted. ASCII spaces, tabs, line breaks, form feeds, carriage returns, hyphens, and underscores become single separators. Leading and trailing separators disappear. Empty names and other characters show the Rust alias error text.

Another live terminal with that alias in the same space prevents saving. Two terminals without a space label share that duplicate check. Other spaces and finished terminals do not reserve the alias. Saving writes `namedrop.name=<alias>` through core. A nonzero result shows its stderr. A successful save closes the dialog. Cancel and Escape close without starting a save. An already started core command can still finish after the dialog closes.

## Built from

This build targets Herdr app **0.1.9** at `c235143` and herdr-core **0.1.14**. It has two independent mods.

- `mods/namedrop/mod.json` sets `place` to `none` and requests a `window` overlay with key `namedrop-roster`, `keep: true`, and width 300.
- `mods/namedrop/roster.js` exports `roster(terminals, panes, spaces)`. It returns `{ alias, address, status, program, shown, where }` rows, joins pane visibility and space and tab names, and sorts by status and alias. Remote terminal addresses qualify the machine-local space and tab labels for the name lookup.
- `mods/namedrop/main.js` renders those rows with `View`, `div`, `Button`, `h_flex`, and `v_flex`. Icons come from each program's `icon` and `tint` fields through `svg` or `image`. The root reports its size with `on_size_change`. A floating copy puts its status dot on the edge named by `placement()?.edge`.
- `mods/namedrop-name/mod.json` sets `place` to `none` and requests a `dialog` overlay of width 360.
- `mods/namedrop-name/alias.js` exports `normalizeAlias(input)` and `aliasTaken(terminals, alias, address)`. The first ports `normalize_alias` and `validate_alias` from Namedrop's `src/domain.rs`. Both pure modules import nothing.
- `mods/namedrop-name/main.js` captures `focused()?.terminal` in `init`, owns an `InputState`, and renders `Input` and native buttons. It checks a fresh terminal snapshot before saving. Its editing and saving states prevent duplicate submission while a save is pending.
- `keys.json` binds `alt-n` to `mod.open:namedrop-name` and `alt-shift-s` to `mod.open:namedrop` in `global`.
- `.github/workflows/mod.yml` runs `node --test mods/*/*.test.js` on Node 22 for pushes and pull requests that change `mods/**` or the workflow.

The roster calls `terminals()`, `panes()`, `spaces()`, `theme()`, `placement()`, and `props()`. It runs `pane.focus` or `terminal.peek` through `run`. The Name dialog calls `focused()`, `terminals()`, `props()`, `hc()`, and `closeOverlay()`. The roster's Close button also calls `closeOverlay()`. Each copy probes `exec("namedrop", ["--version"])` once in `init`. The releases button calls `cx.open_url("https://github.com/ericbust/namedrop/releases")`. The only core label either mod writes is `namedrop.name`, with `hc(["label", address, "namedrop.name=" + alias])`. There are no bundled icons or shipped mods to copy.

## Watch out

Copy both mod folders into your app home's `mods/` folder, normally `~/.config/herdr-app/mods/`. Merge the two global bindings into your existing `keys.json`. Check an existing binding before replacing it.

Option plus a letter types a character or starts an accent on many Mac keyboards. Binding `alt-n` can take Option+N away from typing, including tilde composition on layouts that use it. The app's `option_as_alt` setting and keyboard layout affect whether Option reaches the shortcut. To preserve Option input, remove `alt-n` from your app home's `keys.json` and bind an unused chord such as `cmd-shift-n` to `mod.open:namedrop-name`. Check that chord against your existing bindings first. The same consideration applies to the roster's Option shortcut.

A desktop-launched app on Linux sees the desktop session's PATH. A `namedrop` installed in `~/.cargo/bin` can therefore read as missing even when it works in your interactive shell. Add that directory to the session PATH and restart the app. The version probe runs once per mod copy, so reopen or reload the roster after changing the installation.

The roster window is kept per layout. It does not become a peek drawer. For an optional persistent layout float, clone a shipped layout first:

```sh
herdr-app do layout.clone '{"name":"main","as":"namedrop-layout"}'
```

In your app home's `layouts/namedrop-layout.json`, append this entry to the root's `floating` list. Create that list if absent. Preserve the layout's other fields and existing floating entries:

```json
{
  "id": "namedrop-roster",
  "content": { "mod": "namedrop" },
  "edge": "right",
  "align": "center",
  "peek": 36
}
```

Close the roster window if you want only the float. Side edges support `peek`; hover reveals the whole roster and leaving it tucks it back. The status dot sits on the edge that peeks. Dragging the float changes its edge and alignment. A floating copy has no Close button. Remove its layout entry to remove it.

This shell assigns core labels and shows status. Sending mentions still requires the Namedrop binary's app-host integration. Core labels do not enforce alias uniqueness atomically, so simultaneous Name dialogs can race. No terminal text, Namedrop database, or other mod state is changed by these mods. App-owned window persistence belongs to Herdr.

The app, core CLI, and Weston were unavailable during this build. The source and pure functions were checked locally. Live rendering, focus retention when opening Name, remote interaction, hot reload, and performance await the app checks below. `preview.png` is intentionally absent until a real screenshot can be captured.

## Done when

Run these checks on the target app and core versions:

1. `node --test mods/*/*.test.js` passes. Both `main.js` files pass `node --check`.
2. `herdr-app check mods/namedrop` and `herdr-app check mods/namedrop-name` pass. `herdr-app status` lists no errors for shown copies.
3. On a shipped layout, Alt+Shift+S opens the roster and Alt+N opens Name for the focused terminal. Save `coder` with Enter. `hc ls --json` shows its label, and the roster updates within two seconds. Missing focus, duplicate names, and core failures show an explanation without a successful save.
4. Label a Claude terminal and a `cat` terminal. The roster lists both aliases. The Claude row shows its reported status color, and `cat` shows no dot. Clicking a terminal with a pane focuses it. Clicking a terminal without a pane peeks at it.
5. Reopen the roster, drag it, switch layouts, and restart. Its position and kept state match the behavior above. In the optional layout float, move it between edges and confirm that the status dot stays on the peeking edge.
6. Start without `namedrop` on the app's PATH. The install text and releases URL appear, and Open releases opens that page. Break `main.js` during the reload lane. Status reports the error while the last good roster remains visible.
7. Measure the app's idle CPU with four labelled terminals against the same app without the mod, interleaving two 60-second runs each. The increase is at most one percentage point. The longest roster render in the sandbox log is at most 20 milliseconds.
8. Save a real running-app screenshot as `preview.png`. Capture the roster and Name dialog screenshots and a 30 to 60 second review video for the operator's review before merge.
