# Rex UI and installer design

This document is the design for step 6 of the [Rex host plan](PLAN.md): the Lua module, its palette actions and screens, and the installer. It also covers item 9 of PLAN's "Before the first release" and the install half of item 10. The step is done when a machine with Rex and no Herdr can install Namedrop, activate it, and name a pane from the palette.

The design is for the agent who implements step 6 and the person who reviews it. A person who uses Rex without Herdr gets what a Herdr user gets today: one install, an activation per space, and the naming, manage, Doctor, status, and tour screens from the palette and from keys. A person who runs Herdr inside Rex keeps one Namedrop, and both hosts use it.

The evidence comes from probes on 2026-11-14 with two Rex builds and two Namedrop builds. Rex `0.1.0-preview.14` (commit `7e460d0`, API revision 12) is the server the Rex app ran then. Rex `0.1.0-preview.18` (commit `baf79e0`, API revision 13, "supports 0 to 13") is the server the app bundle now ships, so the next server restart runs it. Namedrop ran as a debug build at `329c278b`, and as a scratch build of the same source whose handshake offers revisions 12 to 13. Every probe ran on a scratch Rex server with its own socket, state directory, and `HOME`, started under `env -i`. No probe touched the user's Rex server, `~/.config/rex`, or Herdr. [Evidence](#evidence) gives the commands. Each claim says whether it was measured, read in Rex's documentation, or inferred. A measured claim holds on both Rex builds unless it names one.

The design branch is `exp/rex-ui-design`, cut from `origin/experimental` at `329c278b`.

## What exists in Rex

Each row is a fact this design depends on, with the command or page that showed it.

| Question | Answer | Shown by |
|---|---|---|
| Where Rex reads `init.lua` | `REX_CONFIG` in the server's environment, else `$XDG_CONFIG_HOME/rex/init.lua`, else `~/.config/rex/init.lua`. The server reads it, so it lives on the server's machine | `rex config --help`. Measured: a server with a scratch `HOME` and no `--config` read `$HOME/.config/rex/init.lua` (`rex config check` printed that path) |
| Whether `require("namedrop")` loads a module | Yes. The config's `package.path` starts with `<config dir>/?.lua;<config dir>/?/init.lua`, then `./?.lua` and `/usr/local/share/lua/5.1/…`. `package.cpath` is empty, so C modules cannot load | Measured: an action returned `package.path`, and `debug.getinfo` named `<config dir>/namedrop.lua` as its source |
| Whether the module can live elsewhere | Yes. A line in `init.lua` that prefixes `package.path` makes `require` find a module in any folder, and `LUA_PATH` in the server's environment is merged in after the config dir | Measured: `require("ndpkg")` from `/tmp/ndui/pkg` after a prefix, and `require("lpmod")` through `LUA_PATH` |
| Where `require` looks when `init.lua` is a symlink | In the link's folder, not the target's. A file named by `REX_CONFIG` makes its own folder the config dir | Measured on preview.14 by the second review: a link `h/init.lua` to `g/my.lua` loaded `h/namedrop.lua` |
| What happens when `init.lua` has an error | Rex rejects the whole file and the server keeps the last configuration that loaded, actions and keys included. `rex config reload` prints `error init.lua:<line>: <message>` and exits 1. A first line that starts with `#!` or a byte-order mark is already an error | Measured: an earlier good file's action stayed listed after each failed reload. `#!/usr/bin/env lua` gave `syntax error near '#'`, and a BOM gave `Invalid token` |
| What a `pcall(require, "namedrop")` first line does | It loads the module, and a missing or broken `namedrop.lua` costs no error: the reload exits 0 and the module's actions are absent. A later error in the person's file still rejects the whole file. Without a trailing `;`, a file whose first line starts with `(` becomes a call of `pcall`'s result and fails with `attempt to call a non-function object`. With `;`, it loads | Measured: ten `init.lua` shapes, each reloaded before and after the line was inserted ([Evidence](#evidence)) |
| The Lua version | Lua 5.1 (`_VERSION`), so `unpack`, not `table.unpack`. `io.popen` and `os.execute` raise errors. `rex.exec` runs programs | Measured `_VERSION`. The rest from the [Lua API](https://www.superlogical.com/rex/docs/reference/lua.md) page |
| How to define a palette action | `rex.action{name, title, run, description, category, keywords, args}`. Names are lowercase letters, digits, and underscores, with no dot. `rex actions` lists each loaded action | Measured: three probe actions appeared in `rex actions` after `rex config reload` |
| Whether the actions appear in the app's palette | Unverified. The Rex app attaches only to the default server, so no probe could show it. Rex's docs say a custom action "appears in the command palette". `rex do --origin palette` runs an action as the palette would | Docs, [Custom Actions](https://www.superlogical.com/rex/docs/customize/actions.md). Measured: `--origin palette` set `ctx.origin` to `palette` |
| Whether the palette asks for an action's arguments | Undocumented. The design gives no palette action a required argument | Docs, `llms-full.txt` searched for palette argument prompts |
| How to bind a key | `rex.bind(key, action[, args])`. The newest binding of a key wins. `rex keymap` shows each binding with its source line, top of the stack first | Measured: `rex keymap` listed `shift+cmd+2 nd_probe_ctx namedrop.lua:34` and the sequence `ctrl+b n`. A person's `rex.bind("alt+shift+cmd+i", "mine")` after the module's binding was listed above it |
| What `rex.bindings{key}` sees at load | Only the bindings the config file has made so far. The app's own defaults sit under the file's bindings and are not visible: `cmd+d`, `shift+cmd+p`, and `ctrl+b` each returned 0 | Measured, and `rex keymap --help`: "the config file's bindings stacked over the bindings the asking client ships with" |
| How to read back what loaded | `rex actions --json` lists each action with `name`, `kind`, `title`, `category`, and `source` (`namedrop.lua:3`). The API's `client.keymap` returns `bindings`, `actions`, and `diagnostics`. `server.config_reload` reloads and returns `diagnostics`. Neither `rex actions`, `rex keymap`, nor `client.keymap` runs the file's load-time code | Measured: a load-time program that records each start ran under none of the three |
| Which keys the Mac app already uses | About 60 defaults, among them `cmd+d`, `cmd+k`, `shift+cmd+p` (the palette), and `alt+shift+cmd+{t,u,up,down,left,right,enter}`. `alt+shift+cmd+{i,m,h,s,o}` are free | Docs, [Mac Actions](https://www.superlogical.com/rex/docs/reference/mac-actions.md) |
| How a module learns the current block and session | `run(ctx, args)` gets `ctx.session_id`, `ctx.block_id` (usually the focused block), `ctx.client_id`, and `ctx.origin`. No `ctx.server` arrived, although the docs list one | Measured with `rex do -s S -b B nd_probe_ctx --json` |
| How a module learns the server | `rex.call("server.status")` inside an action returns `api_revision`, `pid`, `host_id`, and `listeners`, whose `primary` entry holds the Unix socket path | Measured |
| How the Lua API is versioned | The only version is the server's `api_revision`: 12 on preview.14, 13 on preview.18, from `rex version` and `server.status`. Lua has no version of its own. Each function is tagged by where it works, and a missing script-only function is `nil` | Measured `rex version` and `server.status`. Docs, Lua API, "Where Each Function Works" |
| How a client's revision is settled | The client's hello offers a range, and the server answers with the highest revision both support, or closes the connection. Written as (highest, lowest), preview.14 answers 12 to the offers (12, 12), (13, 12), and (14, 12), and closes on (13, 13). preview.18 answers 12 to (12, 12), and 13 to (13, 12), (13, 13), and (14, 12). Today's Namedrop offers (12, 12) and accepts only a welcome of 12 (`src/mux/rex/control.rs:13-14`, `:180`, `:202-203`) | Measured with a raw hello on each server |
| What changed from preview.14 to preview.18 | For Namedrop's purposes, the revision only. `rex --help`, `rex __internal skill`, the help for `config`, `actions`, `keymap`, and `do`, and the schemas of `server.status`, `session.new_layer`, `client.keymap`, and `server.config_reload` are byte-identical. `rex block types` gains one terminal action, `download_begin`, and `rex api list` gains its line | Measured: the outputs of both servers, diffed |
| Whether Namedrop works on preview.18 | Yes, on both handshakes. Today's binary (offering 12) and the scratch build (offering 12 to 13, so it ran at 13 on preview.18) gave the same results on both servers: Doctor's `ND-SETUP-001`, then a confirmed `activate`, then the naming screen in a layer, `@term1` set as the block's label and listed by `address list`, and a bridge and a deferred worker started. The fixture-free live tests (`ws_reads_the_welcome…`, `control_reads_the_server_status…`, `the_scratch_server_is_local…`, `the_bridge_stream_follows…`) passed on all four pairs. `mux_connect_surveys…` failed identically on all four, for want of its fixture sessions | Measured |
| What `rex.exec` in an action sees | The server's own environment (`HOME`, `USER`, `SHELL`, `PATH=/usr/bin:/bin` on the probe, and `REX_SERVER` when the server had it), merged with any `env` the call passes, and the server's working directory. A program must be named by its absolute path | Measured: an action ran `/usr/bin/env` and `/bin/pwd` |
| What an action's error looks like | `error(message, 0)` in `run` makes `rex do` print `Error: <message>` and exit 1. Today's Namedrop prints a failure as a JSON envelope on stdout, with stderr empty, and exits 65, so passing its output through would show raw JSON | Measured by the second review |
| Where `rex` is on a machine | Inside the app bundle. A plain login shell finds no `rex` on `PATH`. A Rex block has `REX_BIN_DIR`, and puts it first on `PATH` | Measured by the second review (`which rex`), and the layer probe below |
| How long an action may run | 30 s. Rex stops the command it started when `run` returns, but a child that the command backgrounds survives, reparented to launchd | Docs, Custom Actions. Measured: a `sh -c '… &'` child and a `setsid` child both outlived the action |
| How to open a floating layer | `rex.session.new_layer{bounds = {x, y, w, h}, focus, window_id, layout}`. `bounds` is required, in fractions of the window | Measured: `rex api describe session.new_layer` |
| When a layer closes | When its program exits, except after a quick failure. The terminal option `exit = {on_completion, quick_exit_threshold_ms}` defaults to `{true, 250}`: a program that fails within 250 ms leaves its layer open so the error can be read. A program that cannot start fails the `new_layer` call itself | Measured: `/usr/bin/false` left its layer open after 2 s; `sleep 1; exit 1`, `/usr/bin/true`, and `/usr/bin/false` with a threshold of 0 each closed theirs; `on_completion = false` kept a finished layer; `/nonexistent/namedrop` made `new_layer` fail with `posix_spawn … no such file or directory (status=500 code=internal)`. The schema's own text: "Left out, it closes after most exits but stays open after a quick failure" |
| What a layer's program inherits | `REX_SESSION`, `REX_BLOCK` for the layer's own block (not the block the action ran from), `REX_BIN_DIR`, and `PATH` with `REX_BIN_DIR` first. No `REX_SERVER` on a server started without it. With `login` left out, `/usr/bin/login` resets `HOME` to the account's home and prints a "Last login" banner. With `login = false`, `HOME` is the server's | Measured both ways |
| Whether a terminal block takes environment variables | No. The terminal options are `shell`, `command`, `args`, `login`, `interactive`, `cwd`, `path_prepend`, `size`, `theme`, `initial_input`, and `exit` | Measured: `rex block types --json` |
| Whether Namedrop's existing TUI runs in a layer | Yes. A layer running `/usr/bin/env NAMEDROP_TARGET_…=… namedrop --endpoint rex:<socket> ui run name` showed "Name this pane". Typing `term1`, Enter, and Enter named the block the layer was opened from `@term1`, set its label to `@term1`, and closed the layer | Measured, with today's binary |
| Whether an action can run Namedrop | Yes. An action read the socket from `server.status`, ran `namedrop --endpoint rex:<socket> --space <ctx.session_id> --pane <ctx.block_id> --json doctor`, and returned `healthy` with one terminal. `rex do --origin palette` took 0.03 s end to end | Measured |
| Whether a top-level `rex.exec` can start the bridge | It runs at every server start and every `rex config reload` (ADAPTER.md, Evidence, revision 1), and also at every `rex config check`, even a reload that then fails. Under `config check`, the detached child runs in the checker's environment | Measured: `rex.exec.start{detach = true}` at load wrote a new pid after `config reload` (parent the server) and after `config check` (parent 1) |
| Which parent a detached child sees, at once | The server, for a start at server start or reload and for a program an action runs. Parent 1 (launchd), for a start at `rex config check` and for one in a `rex do` script. The socket's peer pid is the server's whenever the child finds the socket. Without `REX_SERVER`, a child finds the server only at Rex's default socket, `$HOME/Library/Application Support/rex/server.sock` on macOS. Through a forwarding socket, the peer is the forwarder | Measured on four scratch servers: one at the scratch `HOME`'s default socket with no `REX_SERVER`, one with `REX_SERVER` naming its own socket, one with neither, and one whose `REX_SERVER` named a forwarder |
| Notifications | Rex has no notification API. A program's OSC 9, 99, or 777 becomes a `terminal.desktop_notification` event | Docs, Events and Program Status Protocol |
| Block plugins | `rex server run --block-dir DIR` loads executable block plugins. The docs give the flag only, and the app starts its own server | `rex server run --help` |
| Hosts | `rex hosts add LABEL ENDPOINT…` adds a host to the person's home. `rex home` shows or sets that home | `rex hosts --help`, `rex hosts add --help`, `rex home --help` |

Four facts about today's Namedrop on Rex matter as much as the Rex facts.

- **`activate` on a Rex endpoint writes Herdr's key bindings** (measured). It activated the session and installed both agent packages, and it also wrote five `[[keys.command]]` entries into `~/.config/herdr/config.toml` and reported `reloaded: true`. `activate` calls `shortcuts::activate_shortcuts` for every host (`src/lifecycle.rs:505-506`), and the Rex adapter answers `ReloadKeybindings` with `Applied::Unsupported`, which the shortcut code reads as success (`src/shortcuts.rs:209-215`, `src/mux/rex.rs:793-794`).
- **`ui launch` on Rex opens nothing.** The Rex adapter answers `OpenPopup` with `Unsupported` too, so `ui launch` returns `null` (`src/lifecycle.rs:1419-1422`). The first-agent tour then consumes its once-only marker and reports `shown: true` for a popup that never opened (`src/tour.rs:428-440`). The marker is claimed before the popup on purpose, so two agents found at once open one tour, and a failed open is not retried.
- **`scripts/runtime.py` calls `herdr` even where nothing else does** (measured by the second review). The first publish records the sessions of the one-time launcher migration through `update.session_panes()`, which runs `herdr session list` and reads `HERDR_SOCKET_PATH` (`scripts/runtime.py:94-108`, `:857`, `scripts/update.py:278-306`). An update that is not live or whose schema changes runs `update.preflight()`, which needs `HERDR_PANE_ID` and `HERDR_WORKSPACE_ID` (`scripts/runtime.py:820-825`, `scripts/update.py:309-338`). With a scratch state and no `herdr` on `PATH`, the script stops with `Namedrop runtime activation stopped: [Errno 2] No such file or directory: 'herdr'`, exit 2. That survey sees only Herdr servers, so Rex agents on a machine with both hosts are invisible to it.
- **Two runtimes can take turns owning the packages** (measured by the first review). Two copies of today's runtime, with one scratch `HOME` and the owner files the first draft proposed, activated A, then B, then A, and the install record followed each one. `package_guard()` locks and checks only the running runtime's own directory (`src/installed_runtime.rs:202-224`).

## How Herdr does it today

### The plugin manifest

`herdr-plugin.toml` declares Namedrop 1.7.9 for Herdr 0.7.5 or later, on macOS and Linux (`herdr-plugin.toml:1-6`). Herdr runs `[[build]]` (`scripts/build.sh`) after it clones the plugin, and `[[startup]]` (`run.sh reconcile`) when it starts. Every command below runs as `sh ./scripts/run.sh …`.

| Action | Title | Context | Runs | Opens |
|---|---|---|---|---|
| `activate` | Activate Namedrop | workspace | `activate --input-json {"confirmed":true}` | nothing, through `runtime.py` |
| `channels` | Open Namedrop Channels | workspace | `channels` | the browser |
| `name-agent` | Name this pane | pane | `ui launch name` | popup `naming`, 72x20 |
| `manage-addresses` | Manage Namedrop names and agents | pane | `ui launch manage` | popup `management`, 72x24 |
| `doctor` | Namedrop Doctor | pane | `ui launch doctor` | popup `doctor`, 72x24 |
| `check` | Check Namedrop health | workspace | `doctor` | nothing, a report |
| `agent-status` | Agent status | pane | `ui launch status` | popup `agent-status`, 84x24 |
| `tour` | Namedrop tour | pane | `ui launch tour` | popup `tour`, 72x24 |
| `deactivate`, `pause`, `resume` | Deactivate, Pause, Resume Namedrop | workspace | `deactivate`, `space pause`, `space resume` | nothing |
| `update` | Update Namedrop | workspace | `update.sh --open` | tab `update` |

Each popup is a manifest pane whose command is `ui run <mode>`. The tabs `setup` and `update` run `setup.sh activate --keep-open` and `update.sh`. Five lifecycle events run `event pane-lifecycle`, and `pane.agent_status_changed` and `pane.agent_detected` run `hook agent-status-changed` (`herdr-plugin.toml:133-163`). The manifest has no keys. Activation writes them into Herdr's own config.

### The scripts

- **`scripts/run.sh`** adds `--herdr-pane $HERDR_PANE_ID` when Herdr set it, sends `activate` to `runtime.py`, and otherwise runs the first of `$HERDR_PLUGIN_STATE_DIR/runtime/namedrop`, `bin/namedrop`, and the release build.
- **`scripts/build.sh`** builds `--release` for Darwin arm64 or Linux x86_64 and aarch64 and stages `bin/namedrop`. It contacts no host.
- **`scripts/setup.sh`** needs `HERDR_WORKSPACE_ID`, `HERDR_PANE_ID`, `HERDR_SOCKET_PATH`, `herdr`, and `python3` (`:28-39`). `install` links a local or archive checkout with `herdr plugin link`. `activate` invokes the `activate` action over Herdr's socket, polls the plugin log for 60 s, checks the space and the packages, and prints a human report.
- **`scripts/runtime.py`** publishes the runtime. Under `<state>/activation.lock` it checks `bin/namedrop --runtime-info` against `runtime.json`, copies the binary to `<state>/runtime/releases/<sha256>/namedrop`, writes the launcher `<state>/runtime/namedrop` that runs `current/namedrop`, switches the `current` link (migrating the store when the schema changes), runs `<launcher> activate`, checks that `space_id` equals `HERDR_WORKSPACE_ID`, runs `codex plugin marketplace add` and `codex plugin add namedrop@namedrop`, and restores the old runtime on any failure (`:807-921`). Its state is `HERDR_PLUGIN_STATE_DIR` (`:808`). It stops a helper only when that helper is a status worker of a retained release from before 1.7.0 that holds the command lock (`gate_helpers`, `:348-381`). Every later helper holds the gate only while it works, so activation waits for it and never stops it. The `HERDR_*` names in `helper_kind` and `queue_wake` (`:332`, `:433-438`) therefore matter only for those old workers, which never ran on Rex.
- **`scripts/update.py`** fetches the registered GitHub ref into a scratch checkout, validates its manifest with the launcher, waits for idle sessions, runs `herdr plugin install`, and runs the new `setup.sh activate`.
- **`scripts/check-update.py`**, embedded in the binary, asks `herdr plugin list` for the registration, runs `git ls-remote`, and reads the version from the manifest at that commit into `<state>/update-check.json`.
- **`scripts/package-release.sh`** builds the archive with `bin/namedrop`, the scripts, `runtime.json`, `SHA256SUMS`, and a `.tar.gz.sha256`. Nothing is signed.

### What a person does

1. `herdr integration install codex` and `herdr integration install claude`.
2. `herdr plugin install ericbust/namedrop --ref main`, which clones and builds. An archive instead runs `sh scripts/setup.sh install`.
3. Open the `setup` tab, which activates the workspace and waits for Enter.
4. Run `namedrop doctor` in a new tab, and start new agent sessions.

`activate` writes these files (`src/lifecycle.rs:452-524`, `src/install.rs`).

- `~/.local/bin/namedrop`, a script that runs the launcher with `--plugin-state <state>`, and a `PATH` line in the shell's startup file.
- `~/.claude/skills/namedrop/` and `~/.agents/skills/namedrop/`, each with `SKILL.md`, `tool.json`, both plugin manifests, `hooks/hooks.json`, the four hook wrappers, and an owner marker. Every wrapper bakes the launcher and `--state-directory <state>`.
- Claude Code allow rules in `settings.json`, Codex rules in `$CODEX_HOME/rules/namedrop.rules`, and the deferred-commands root in Codex's `config.toml`.
- The install record `~/.config/namedrop/state-directory`, which only a Rex home reads (`src/locator.rs:216-240`).
- Five `[[keys.command]]` bindings, `prefix+shift+{s,i,m,h,o}`, in Herdr's `config.toml`, then `herdr server reload-config`.

The update check runs at most once a day from `reconcile` and lifecycle events. A new version shows as Doctor's `ND-UPDATE-001` and as a once-a-day SessionStart message, both naming `herdr plugin action invoke update --plugin namedrop`.

### The screens

Each screen has two halves. `ui launch <mode>` runs in the person's pane, finds the target pane, and emits `Effect::OpenPopup`. Herdr renders that as `plugin pane open --placement popup` with `NAMEDROP_TARGET_WORKSPACE`, `_PANE`, `_TERMINAL`, `_SESSION`, `HERDR_PLUGIN_STATE_DIR`, and `HERDR_SOCKET_PATH` as `--env` values (`src/mux/herdr/wire.rs:682-732`). `ui run <mode>` is the popup's program. It reads the targets from its environment, refuses when the target pane's terminal changed, and runs a raw-terminal prompt through `/bin/stty` (`src/popup_input.rs`). The name, manage, Doctor, and tour screens are line prompts, and agent status is a live panel. None has a `--json` mode, because each is interactive. Their agent-facing equivalents are `name`, `address`, and `doctor`, which do. Channels is not a popup. It starts the dashboard server and opens the browser.

## Problem

Rex differs from Herdr in seven ways that shape this step.

- **Rex has no plugin manifest.** Actions and keys come only from the person's `init.lua`. Rex never runs a build, a startup command, or an event command for Namedrop.
- **Lua has no widgets.** A Rex action can run programs, call the API, and open blocks. It cannot draw text, read a line, or show a list, and the palette's handling of action arguments is undocumented. Every screen with input must be a program in a terminal.
- **A layer takes no environment.** Herdr's popup passes six values with `--env`. A Rex layer passes only argv and `path_prepend`.
- **An action runs in the server's environment.** It gets the server's `PATH`, not the person's shell, and a program in a layer gets a login `HOME` unless the layer sets `login = false`.
- **Nothing installs Namedrop.** No command puts the binary, the agent packages, or the Lua module on a machine without Herdr. The testbed's Rex lane activates inside a profile Herdr and then stops it (PORT.md, "Namedrop has no `integration install`").
- **One machine can run both hosts.** The measured machine runs Herdr inside a Rex block. Both hosts write the same agent packages, which bake one launcher and one state directory, and Claude Code and Codex load one set of hooks.
- **A remote host is a second install.** Each machine that runs agents runs its own Namedrop beside its own Rex server (ADAPTER.md, "Remote hosts").

These constraints carry over from steps 4 and 5. Herdr behavior does not change, except for text that names the host and the differences each [H] commit pins. Every existing test passes.

## Shapes considered

Four whole shapes could put the screens into Rex.

**A. Lua screens over `namedrop --json`.** The module would run `namedrop … --json`, read the result, and show it. Lua cannot show it: no Rex Lua call draws text or reads input, and a palette action cannot ask for an alias in any documented way. The shape would give Rex a list action and a fixed-argument name action for keys, and no naming from the palette. It fails the done-when.

**B. Lua opens layers that run Namedrop's TUI.** Each action calls `rex.session.new_layer` with a command that runs `namedrop ui run <mode>` and passes the targets through `/usr/bin/env` in argv. The probe proved this works with today's binary. The cost is a second copy of `PopupSpec` in Lua: the target terminal and agent session, which the module cannot read, the screen sizes, the state directory, and the target checks that `ui launch` does. The tour that opens after activation, and any future popup Namedrop starts itself, would still need a Rust path to `new_layer`.

**C. Lua runs `namedrop ui launch`, and the Rex adapter opens the layer.** Each action is one `rex.exec` of `namedrop rex action <name> --endpoint <server> --space <ctx.session_id> --pane <ctx.block_id>`, which runs `ui launch <mode>` for a screen. The Rex adapter renders `Effect::OpenPopup` as `session.new_layer`, with the same targets and checks Herdr gets, in argv instead of `--env`. The Lua module holds a table of action names, titles, and keys, and nothing else.

**D. A Rex block plugin.** A Namedrop block type could draw its own screens. It needs `--block-dir` on the server's command line, which the app's server does not take from the person, and the docs describe nothing beyond the flag.

**Chosen: C.** It gives Rex every screen with one code path for both hosts. `ui launch` already finds the target, checks it, and emits `OpenPopup`, and `ui run` already reads its targets and refuses a changed pane, so the only new Rust is one adapter arm and the argv form of the targets. The tour after activation opens through the same arm. The module stays a table of names, and each action calls one Rust entry point, so a change to an action or a screen never touches Lua, and a module loaded before an update keeps working after it. The binary that the module runs generates it, with the launcher and state directory written in. The cost over B is one process start per action, 0.03 s for the whole `rex do` in the probe (measured, debug build).

## Design

### The Lua module

`namedrop install` writes `namedrop.lua` beside the `init.lua` that Rex reads, and inserts one line as the first line of `init.lua`:

```lua
pcall(require, "namedrop");
```

`pcall` keeps a missing or broken module from rejecting the person's whole file. The `;` keeps a file whose first line starts with `(` from turning the line into a call of `pcall`'s result. Both were measured. A broken module is then silent at load, so the installer and Doctor read back what loaded instead of trusting the reload's exit status.

The module holds only what every Namedrop release reads the same way: the launcher, the state directory, a table of action names, titles, and keys, and an `endpoint()` helper. Each action runs one Rust entry point, `namedrop rex action <name> --endpoint … --space … --pane …`. An update that changes what an action does, or how a screen opens, changes the binary behind the launcher and never the loaded Lua. A server that loaded the module before an update keeps working after it, with no reload.

The generated module looks like this, with the paths written in at install.

```lua
-- Managed by Namedrop. `namedrop install` rewrites this file, so edits here are lost.
-- A key bound later in init.lua wins over the same key below.
local NAMEDROP = "/Users/me/.local/state/namedrop/runtime/namedrop"
local STATE = "/Users/me/.local/state/namedrop"

local function endpoint()
	local status, err = rex.call("server.status")
	if status == nil then error("Namedrop could not ask Rex for its server: " .. tostring(err), 0) end
	for _, listener in ipairs(status.listeners or {}) do
		if listener.primary and listener.transport == "unix" then return "rex:" .. listener.addresses[1] end
	end
	error("Namedrop needs a Rex server that listens on a Unix socket", 0)
end

local actions = {
	{ "activate", "Activate Namedrop" },
	{ "name", "Name this pane", "alt+shift+cmd+i" },
	{ "manage", "Manage Namedrop names and agents", "alt+shift+cmd+m" },
	{ "doctor", "Namedrop Doctor", "alt+shift+cmd+h" },
	{ "status", "Agent status", "alt+shift+cmd+s" },
	{ "tour", "Namedrop tour", "alt+shift+cmd+o" },
	{ "channels", "Open Namedrop Channels" },
	{ "pause", "Pause Namedrop" },
	{ "resume", "Resume Namedrop" },
	{ "deactivate", "Deactivate Namedrop" },
}

for _, a in ipairs(actions) do
	local name, title, key = a[1], a[2], a[3]
	rex.action{ name = "namedrop_" .. name, title = title, category = "Namedrop", keywords = "namedrop mention agent",
		run = function(ctx)
			if ctx.session_id == nil or ctx.block_id == nil then error("Run this from a Rex pane", 0) end
			local argv = { NAMEDROP, "rex", "action", name, "--endpoint", endpoint(),
				"--space", ctx.session_id, "--pane", ctx.block_id }
			local result, err = rex.exec(argv, { env = { NAMEDROP_STATE_DIR = STATE }, timeout = 25 })
			if result == nil then error(tostring(err), 0) end
			if not result.ok then error(result.stderr, 0) end
		end }
	if key ~= nil and #rex.bindings{ key = key } == 0 then rex.bind(key, "namedrop_" .. name) end
end

rex.exec.start({ NAMEDROP, "rex", "ensure", "--from-server" },
	{ env = { NAMEDROP_STATE_DIR = STATE }, detach = true, stdout = false, stderr = false })
```

Six choices in it need a reason.

- **One entry point.** `namedrop rex action <name>` maps the name to today's commands. `activate` runs a confirmed `activate`. `name`, `manage`, `doctor`, `status`, and `tour` run `ui launch <mode>`. `channels`, `pause`, `resume`, and `deactivate` run their commands. It prints a failure as one plain line on stderr, decoded from Namedrop's JSON envelope, and exits 1, so the palette shows `Error: <the line>` and never JSON. A name the binary does not know, as after a rollback to an older release, fails with `This Namedrop has no action <name>. Run sh scripts/install.sh again.` The module's text changes only when the list of actions does.
- **The endpoint comes from `server.status`.** Rex does not set `REX_SERVER` in every block, and an action's `ctx` carried no server (measured). The primary Unix listener is the canonical endpoint that `prove_local` already uses (ADAPTER.md, "The endpoint string carries the kind"). An explicit `--endpoint` decides the home with no socket probe, and the adapter still proves the server local before it types anything.
- **No revision check in Lua.** The Rust handshake is the only version gate. Its hello offers a range, and the server settles on the highest revision both support or closes the connection (measured), so every action and every CLI call is gated in one place, with one error. The tested range widens per release, after the probes of [What exists in Rex](#what-exists-in-rex) run on the new Rex build. Revision 13 joins it in U1: preview.18 differs from preview.14 only by the revision and one terminal action Namedrop does not use, and a build that offered 12 to 13 passed the same checks on both servers (measured).
- **Keys are bound by default, and a bound key is skipped.** The module binds `alt+shift+cmd+{i,m,h,s,o}`, Herdr's letters on the one modifier set the Mac app leaves free, unless `rex.bindings{ key = … }` already returns a binding for the key. That call sees only the bindings the file made before the module (measured), which on line 1 is none, unless the person moved the line down. A key the person binds later in `init.lua` wins, because Rex puts the newest binding on top (measured). Doctor reads `client.keymap` and reports each Namedrop key as in effect, or as shadowed with the source line that shadows it, and lists the keys skipped at load. The bindings carry action names, so `rex keymap` shows each one with its source.
- **No palette action takes an argument.** The palette's handling of arguments is undocumented, and every input is read by a screen.
- **The last line starts the bridge at server start, behind a guard.** A remote host needs its bridge running before any agent there runs a Namedrop command (PLAN.md, "With B's bridge stopped, A keeps the item"). [The load-time start](#the-load-time-start) gives the guard.

### The load-time start

`namedrop rex ensure --from-server` starts the bridge when config code runs inside the server, and otherwise exits doing nothing. A top-level `rex.exec.start` also runs at `rex config check`, in the checker's environment (measured), and the guard keeps that run from starting anything.

The guard runs first in `main`, before the runtime, the store, the locator, or any home probe exists, so a refused run writes no file and calls no host. It finds the socket as `rex` does: `REX_SERVER` when it names a Unix socket, else Rex's default socket, which is `$HOME/Library/Application Support/rex/server.sock` on macOS and `$XDG_RUNTIME_DIR/rex/server.sock` on Linux. That is the rule `rex_claim` already applies (`src/mux/home.rs:346-370`). It connects, reads the socket's peer pid as `prove_local` does, reads its own parent pid at once, and continues only when the two are equal. Then it runs today's `rex ensure` with the endpoint `rex:<socket>`, which is idempotent through the bridge lock.

The measured parents decide each case.

| Who ran the config code | Parent | Peer | Result |
|---|---|---|---|
| The server, at start or `config reload` | the server | the server | the bridge starts |
| `rex config check` | 1 | the server, or no socket | refused |
| A `rex do` script | 1 | the server, or no socket | refused |
| A server whose `REX_SERVER` names a forwarding socket | the server | the forwarder | refused |
| A server with no `REX_SERVER` and a socket that is not the default | the server | no socket | refused. The bridge then starts with the first Namedrop command in a block, as today |
| A program that an action runs | the server | the server | the bridge starts, or is already running |

This is a correctness guard, not a security boundary. Config code already runs programs as the same user, so a hostile `init.lua` needs no guard to start a bridge. The guard only keeps a checker's run, in the wrong environment, from starting one. On Linux, an orphan goes to the nearest subreaper rather than to pid 1. If the Rex server is a subreaper, a `config check` run inside a Rex block could see the server as its parent. That is measured before release ([Release checks](#release-checks)).

### Popups on Rex

The Rex adapter answers `Effect::OpenPopup` with `session.new_layer` and returns `Applied::Opened` with the reply (ADAPTER.md, answer 6).

```rust
// src/mux/rex.rs, apply
Effect::OpenPopup(popup) => {
	let request = json!({
		"session_id": popup.target.space.as_str(),
		"window_id": popup.target.tab.as_str(), // a Rex pane's tab is its window
		"bounds": popup.screen.bounds(),         // centered fractions, from the screen's cells
		"focus": true,
		"layout": {"block": {"flavor": "com.superlogical.terminal.shell", "options": {
			"shell": "none",
			"login": false,                       // keep the server's HOME, and no banner
			"size": {"columns": popup.screen.columns(), "rows": popup.screen.rows()},
			"command": popup_argv(&popup, &self.endpoint, launcher),
		}}},
	});
	connection.call("session.new_layer", &request).map(Applied::Opened)
}

// ["/usr/bin/env", "NAMEDROP_TARGET_WORKSPACE=…", "NAMEDROP_TARGET_PANE=…",
//  "NAMEDROP_TARGET_TERMINAL=…", "NAMEDROP_TARGET_SESSION=…", "NAMEDROP_STATE_DIR=<state>",
//  <launcher>, "--endpoint", "rex:<socket>", "ui", "run", <mode>]
fn popup_argv(popup: &PopupSpec, endpoint: &Endpoint, launcher: &Path) -> Vec<String> { todo!() }
```

`/usr/bin/env` carries the five values the Herdr popup gets with `--env`, so `ui run` reads them unchanged. The probe ran exactly this argv (measured). Its `--endpoint` keeps the popup's own `REX_BLOCK`, which names the layer, out of the home decision. `login = false` matters for the testbed, whose profile `HOME` differs from the account's, and it drops the "Last login" banner from the screen.

The layer keeps Rex's default `exit`. A screen that fails within 250 ms leaves its layer open with the error, as Rex intends (measured). A later failure would close the layer and take the error with it, so `ui run` on a Rex endpoint prints any refusal or error and waits for one key before it exits. A launcher that cannot start makes `new_layer` itself fail (measured), and `rex action` reports that error in the palette.

`ui launch` on Rex refuses two targets before it builds any request.

- **A block whose foreground program is `herdr`.** A person who runs Herdr inside Rex and opens Rex's palette over the Herdr block would otherwise name the Rex block that holds Herdr. The refusal says to use Herdr's own Name this pane.
- **A block whose foreground program is a Namedrop screen,** `namedrop … ui run`, read from the block's process facts (`foreground` with `name`, `path`, and `argv0`). `ctx.block_id` is usually the focused block, so a palette run from inside a layer would otherwise target the layer.

In a Rex session that is not active, the naming screen offers activation instead of refusing: `Namedrop is not active in this session. Press Enter to activate it, or Esc to cancel.` Enter runs the confirmed activation and continues to the name prompt. A Herdr space keeps today's behavior.

### Activation on Rex

`activate` stops writing Herdr's keys for a Rex home. It reports `shortcuts: {"status": "rex_module", "path": "<config dir>/namedrop.lua", "keys": […]}`, with the keys in effect, instead. `activate_shortcuts` returns an error for `Applied::Unsupported` rather than `reloaded: true`, so no host reports a reload that did not happen. `ui install-shortcuts` on a Rex home writes no Herdr file, and exits with an error that names `namedrop.lua` and Doctor's report of the keys.

Activation opens at most one layer. Today's `activate` already opens the first-agent tour when an agent is present and the tour's marker is unclaimed (`src/lifecycle.rs:509`). `namedrop rex action activate` opens Doctor only when no tour opened, and Doctor lists what remains, such as Codex's hook approval. Herdr's `activate` does not change.

The tour keeps Herdr's marker rule: it claims the marker before the popup, so two agents found at once open one tour, and a failed open keeps the marker. On a Rex home, an `Unsupported` result releases the marker, so the next detection tries again. With the adapter arm, Rex reports `Opened`, so the release applies only to a Rex server that cannot open layers.

### The installer

A person on a machine with Rex and no Herdr does four things.

1. Download the release archive and its `.sha256`, check it, and unpack it. Or clone the repository and run `sh scripts/build.sh`.
2. Run `sh scripts/install.sh` with Rex running. It shows what it will change and asks once.
3. Open the Rex palette in a pane and run **Activate Namedrop**. The tour or Doctor opens, and Doctor lists anything left, such as Codex's hook approval.
4. Run **Name this pane**, or press <kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>Cmd</kbd>+<kbd>I</kbd>, in each pane to name, and start new agent sessions.

`scripts/install.sh` checks the platform as `build.sh` does, and checks for `python3` and for `rex`: `rex` on `PATH`, else `$REX_BIN_DIR/rex`. A plain login shell has neither (measured), so outside a Rex block it stops and names both ways to supply `rex`. It then runs `python3 scripts/runtime.py <root> --state-dir <dir> --verify install`. The state directory is the one `locator::state_directory` gives a Rex home: `NAMEDROP_STATE_DIR`, the install record, then `$XDG_STATE_HOME/namedrop` or `~/.local/state/namedrop`. The runtime and the confirmed install then put these on the machine.

| What | Where | Written by |
|---|---|---|
| The runtime | `<state>/runtime/releases/<sha256>/namedrop`, the `current` link, and the launcher `<state>/runtime/namedrop` | `runtime.py`, as for Herdr |
| The owner and the install source | `<state>/runtime/owner` (`herdr` or `standalone`), and `<state>/runtime/source.json`: the version, and `archive` or `checkout` with its origin | `runtime.py` |
| The agent packages, permissions, Codex rules and sandbox root, `~/.local/bin/namedrop`, and the install record | As Herdr's activation writes them, baked to the launcher and the state directory | `namedrop install` |
| The Lua module | `namedrop.lua` beside the `init.lua` Rex reads, with the managed-file line first | `namedrop install`, and the owner's package refresh |
| The `init.lua` line | `pcall(require, "namedrop");` as line 1, under the guards below | `namedrop install` |
| The Rex install record | `~/.config/namedrop/rex.json`: the module's path, the `init.lua` path, the line's state (`inserted`, `printed`, or `removed`), and the keys skipped at load | `namedrop install`, and the owner's package refresh |
| The Codex plugin | `codex plugin marketplace add` and `codex plugin add namedrop@namedrop` | `runtime.py`, as for Herdr |
| The config reload | `server.config_reload` on the local default server | `namedrop install` |

`namedrop install` is a new command. It refuses without `--input-json {"confirmed":true}` and then lists every target, as `activate` does (`src/install.rs:203-226`). It shares `activate`'s package installer, which is already host-neutral, and it activates no space. It finds the path Rex reads by Rex's own rule, `REX_CONFIG`, then `$XDG_CONFIG_HOME/rex/init.lua`, then `~/.config/rex/init.lua`, and not with `rex config check`, which would run the file's load-time code. It refuses to replace a `namedrop.lua` whose first line is not its own. With `--print-config`, it writes `namedrop.lua`, prints the line and where it goes, and edits nothing else in Rex's config. `config reload` is refused over a remote connection (Rex's docs), so the installer reloads only the local default server.

**Editing `init.lua`.** The installer edits `init.lua` only under six guards, and otherwise prints the line, the reason, and where the line goes.

1. The path Rex reads must be a regular file, or absent, in which case the installer creates it with the line. For a symlink or any other kind of file, it prints the line. `namedrop.lua` still goes beside the path Rex reads, in the link's folder, because that is where `require` looks (measured).
2. The line counts as present only as a whole line of its own, with surrounding spaces ignored. A comment or a string that mentions `require("namedrop")` does not count. A present line is left where it is.
3. When `rex.json` records the line as `inserted` and the line is gone, the person removed it. The record becomes `removed`, and no later install or refresh adds the line again. Doctor reports it, with the line to restore.
4. The installer first reloads the unedited file through `server.config_reload`. When that reports an error, the file is already broken: it edits nothing, prints Rex's diagnostic and the line, and stops. When no local server answers, it edits nothing and asks the person to start Rex and run the install again.
5. It copies `init.lua` to `init.lua.namedrop-<UTC time>.bak`, writes the new file through a temporary file and a rename, and reloads. When the reload reports an error, it renames the backup back, reloads again, and reports Rex's diagnostic.
6. It confirms the install by reading `rex actions --json`, which runs no load-time code (measured). The ten `namedrop_` actions must be listed with the source `namedrop.lua`. A reload that exits 0 proves nothing here, because `pcall` hides a module error. A missing action fails the install and lists the actions found. The edit stays, because the line is harmless without a working module.

The edit, the reloads, and the readback all run under the shared package lock ([One Namedrop](#one-namedrop-on-a-machine-with-both-hosts)).

**`scripts/runtime.py` becomes host-neutral.** It takes `--state-dir` and `--verify activate|install`. `run.sh activate` passes `--state-dir "$HERDR_PLUGIN_STATE_DIR" --verify activate`, and that path keeps today's steps, its Herdr survey included: `record_legacy` through `herdr session list` on the first publish, and `preflight` before an update that is not live or that changes the schema. `--verify install` makes no `herdr` call. It takes the migration record and the update preflight from two sources: the agent sessions the store holds for each endpoint, and a survey of the Rex agents through the incoming runtime, `<launcher> rex sessions --json`, which lists each Rex block's agent and session as the adapter's survey sees them. It then runs `<launcher> install` and checks `state_directory`, the packages, and `rex.json` instead of the space. The first draft also taught `helper_kind` and `queue_wake` the neutral names, and stopped Rex helpers at publish. That half is dropped: a publish stops only status workers of releases before 1.7.0, which never ran on Rex, and waits for every later helper. The bridge and the relay worker already run the replacement runtime when they see one (ADAPTER.md, "The relay worker").

Herdr's update preflight in `scripts/update.py` also counts the Rex-bound agent sessions in the store, so an update from Herdr on a machine with both hosts waits for Rex agents as it waits for Herdr's. It reads them from the store, because its Herdr survey cannot see Rex.

A Rust port of `runtime.py` would drop the `python3` requirement, which Herdr has too. This design keeps the script, because it is the one piece that must run outside the binary it replaces.

### One Namedrop on a machine with both hosts

The agent packages are one slot: `~/.claude/skills/namedrop`, `~/.agents/skills/namedrop`, `~/.local/bin/namedrop`, the install record, the Codex rules, and the Codex plugin name each hold one launcher and one state directory, and Claude Code and Codex load one set of hooks. Two installs cannot each own a copy without firing every hook twice. So one machine has one Namedrop: one state directory, one runtime, one set of packages. Both hosts' spaces live in one store, kept apart by the endpoint kind in `session_socket_id`, and each process resolves its host through `mux::home`. The testbed's Rex lane already runs this way: it activates in a profile Herdr, and its Rex blocks reach Herdr's state directory through the install record (`testbed/README.md:335-339`).

**One lock and one owner.** Every writer of the shared packages takes one per-user lock, `~/.config/namedrop/packages.lock`, beside the install record. The writers are `activate`, `namedrop install` and its removal, the package refresh that lifecycle events and hooks run (`src/install.rs:898-907`, `:1116`), and the handoff below. Under the lock, a writer reads the install record and the `owner` file of the state directory the record names, and then follows four rules.

- A runtime whose state directory is the recorded one writes, as today.
- A standalone runtime never writes over a Herdr-owned record. It writes nothing and reports `ND-INSTALL-002`, which names Herdr's state directory.
- Herdr's activation over a standalone record takes the packages over, in the handoff below.
- A state directory with no `owner` file counts as Herdr-owned, because every runtime published before this step came from Herdr.

`package_guard()`'s lock stays: it still serializes publication with refresh inside one state directory, and the new lock serializes writers across state directories. A Herdr release from before this step takes neither lock. It can only write in Herdr's favor, which the rules allow, and the third case below covers what it misses.

**The module registration** lives in `~/.config/namedrop/rex.json`, beside the install record, and not in a state directory. So it outlives any one state directory, and the runtime that owns the packages also owns the module: its package refresh rewrites `namedrop.lua` at the recorded path with its own launcher and state directory. A machine with no `rex.json` never gets a module.

- **Herdr first, then Rex.** `install.sh` finds the install record naming Herdr's state, and adopts it. Before adopting, it asks the recorded launcher for its capabilities through `--runtime-info`, whose contract gains a `rex_ui` field in this step. A launcher without it is a Herdr release from before step 6, with no `install` and no `rex action` (the first review measured `install --help` failing with exit 64 on such a copy). The installer then refuses with `Update Namedrop in Herdr first, then run sh scripts/install.sh again.` With a capable launcher, it publishes no runtime, runs the adopted launcher's `install`, and writes the module baked to Herdr's launcher. Herdr's updates then update both hosts, and the module follows through the package refresh.
- **Rex first, then Herdr.** Herdr fixes its own state directory, so its activation cannot adopt Rex's. Its activation finds a standalone owner under the lock and hands off in four steps. It stops the standalone state's bridge and relay worker, found through their lock files and checked by argv and state directory before any signal. It rewrites the packages and the install record to Herdr's state directory. It rewrites `namedrop.lua` and `rex.json` to Herdr's launcher. It reloads Rex's config on the local default server. The old runtime stays on disk, and nothing names it any more. Names made on Rex before that stay in the old store. Doctor reports `ND-INSTALL-001`, naming the old directory and its count of live names, and the person names those panes again. Moving that store into Herdr's is deferred.
- **Rex first, then a Herdr release from before step 6.** That release rebakes the packages and the record without the lock, and knows nothing of `rex.json`, so the module still names the standalone runtime. Doctor on Rex sees the record naming another state directory and reports `ND-INSTALL-003`: `Update Namedrop in Herdr, then run Activate Namedrop there again.`

### Removing Namedrop from Rex

`namedrop install --remove` undoes the Rex half under the lock. It deletes line 1 of `init.lua` only when that line is exactly the line it inserted and `rex.json` says `inserted`, with the same backup, reload, and restore as the install. It deletes `namedrop.lua` only when the file's first line is the managed-file line, and then deletes `rex.json`. It removes the agent packages, the permissions, the Codex rules and plugin, `~/.local/bin/namedrop`, and the install record only when the owner is `standalone`. With a Herdr owner it leaves them, because Herdr still uses them, and says so. It never deletes a state directory. A full uninstall command for both hosts stays deferred.

### Updates

A standalone install has no Herdr registration, so `check-update.py` learns its source from `source.json`. For an archive or a GitHub checkout it lists the repository's `v*` tags with `git ls-remote --tags` and records the newest. Doctor's `ND-UPDATE-001` and the SessionStart notice then say `Update: download Namedrop <version> and run sh scripts/install.sh`, and Herdr installs keep today's text. The SessionStart wrapper accepts either text by exact pattern, as it accepts one today (`src/install.rs:1315-1357`).

A standalone install's update setting lives in `~/.config/namedrop/settings.json`, with the same `check_for_updates` key as Herdr's `~/.config/herdr/plugins/config/namedrop/settings.json` (`src/update_check.rs:27-54`). The owner decides which file is read, so a machine with both hosts has one setting, in the owner's place.

Running `install.sh` from the new archive publishes the new runtime, refreshes the packages and the module, and reloads Rex's config. An archive whose state schema differs migrates the store through `runtime.py`, as Herdr's updates do, behind the store-and-Rex preflight above, and a failure restores the previous runtime (`restore`, `scripts/runtime.py:640-655`). An update command that downloads and checks the archive itself is deferred.

### Remote hosts

On host B, the machine that receives relayed mentions, a person does this.

1. Install Namedrop on B as above, with B's Rex server running. On a Linux server the Rex config dir is `~/.config/rex` too, and `install.sh` reloads B's config only when it runs on B.
2. Start the agents on B in Rex blocks, activate each session, and name the agents, with `namedrop` in B's blocks, or from the palette of an app attached to B. That B's palette shows B's actions is inferred from Rex's docs and unverified.
3. To let A mention a terminal on B, create `<B's state>/relay/allow-terminals`. A terminal runs a relayed message as a shell command, so this is off by default. `namedrop doctor` on B says whether it is on.

On A, the person adds B with `rex hosts add devbox <endpoint>`, which goes to A's home, and checks it with `rex hosts check`. B needs A in its own hosts only to start new mentions to A. Replies flow back by A's pull. B's bridge must run for B to accept an item, and the module's guarded start runs it from B's server start. Doctor on A reports relay failures as `ND-RELAY-002` and `ND-RELAY-003`. PLAN's item 10 writes these steps into the user docs.

### Words

Text that names Herdr where the host is Rex changes, and a Herdr home keeps its Herdr wording where it gives a Herdr command. The first draft listed the texts it found: the packaged `SKILL.md` description ("inside Herdr"), the `activate`, `deactivate`, `address`, and `space` help ("this Herdr space"), Doctor's `ND-SESSION-002` repair for a Rex block (PORT.md, "Doctor's refusal line"), and the tour's "Herdr returned an unreadable popup result". The second review found more, in `src/naming_ui.rs:68` and `:71` and about ten strings in `src/health.rs`. So a test replaces the list. It runs every user-facing command and screen against the fake Rex host and fails on any `Herdr` or `herdr` in the output, except an allowlist of texts that are reached only on a Herdr home. Each allowlist entry names its file and line.

## Tradeoffs accepted

- **A process start per action.** Each action runs the binary once. The probe measured 0.03 s.
- **`python3` stays a requirement.** Herdr requires it too, and the installer checks for it.
- **Rex-first names are lost when Herdr is installed later.** Doctor says so and counts them.
- **The palette is proven only by `--origin palette` until the operator's check.** The app attaches only to the default server.
- **Popup bounds are fractions of the window.** A screen of 72 columns in a narrow window wraps, and the layer cannot be sized in cells.
- **A broken module is silent at load.** `pcall` keeps it from rejecting the person's file, so the install's readback and Doctor, not Rex, report it.
- **The person's own key wins.** A key bound later in `init.lua` shadows Namedrop's, and Doctor says which.
- **The installer needs Rex running.** It reloads and reads back on the local default server, and edits nothing without one.

## Implementation plan

Every commit compiles, passes `cargo fmt --check`, `cargo clippy --locked --all-targets -D warnings` with and without `--all-features`, and `cargo test --locked --lib`, and ends in a state you can check. Live Rex checks run on the testbed's Rex lane, or on a scratch server under `env -i` with its own `HOME`, state, and `REX_SERVER` naming itself, on preview.14 and preview.18 both.

**[H]** marks a commit that touches a path Herdr also runs: the shared UI, the packages, activation, updates, or the archive. Before an [H] commit merges, `/verify` passes on real Herdr, the Herdr-in-Rex lane included, and its done-when includes a **Herdr differential**. The differential runs one script on the Herdr lane twice, with the base commit's build and with the head's, and captures each command's output, each file the run writes under the profile, and each `herdr` call. The two captures must be equal except for the differences the commit pins. A commit that pins none says so.

The order puts each piece before the first commit that needs it: the lock and owner before the runtime that writes the owner, the layers and `rex action` before the install that reads them back, and the install before the archive.

U1. **The handshake accepts revisions 12 and 13.** `src/mux/rex/control.rs` offers `api_min_revision` 12 and `api_revision` 13, and accepts a welcome of 12 or 13. Tests: a scripted welcome of 12 and of 13 connects, and 11 and 14 are refused with the range in the error. Live: the four fixture-free live tests and the probe's doctor, activate, and naming pass on both builds, at revision 12 on preview.14 and 13 on preview.18, as the scratch build already did. Done when the live run passes on both.

U2. **Activation reports what happened** [H]. A Rex home's `activate` writes no Herdr keys and reports `rex_module`. `Applied::Unsupported` from a reload is an error in the shortcut report. On a Rex home, a tour whose popup is `Unsupported` releases its marker, and every other failure keeps it. `ui install-shortcuts` on a Rex home writes nothing and names `namedrop.lua`. Tests: a Rex activation in a temporary `HOME` leaves no `.config/herdr/config.toml` (today's binary writes it, measured), a Herdr activation writes today's file byte for byte, a Rex tour whose popup is `Unsupported` is shown on the next detection, a Herdr tour whose open fails keeps its marker, and `ui install-shortcuts` on a Rex home writes no file. Herdr differential: nothing pinned. Done when the probe's activation, run again, leaves no Herdr config and the differential is clean.

U3. **One package lock and one owner** [H]. `~/.config/namedrop/packages.lock` at every package writer (`activate`, the refresh in events and hooks), the owner read under it, a missing `owner` read as Herdr's, `ND-INSTALL-002` and `ND-INSTALL-003`, and `rex_ui` in `--runtime-info`. Tests: the first review's probe as a test, with a standalone and a Herdr runtime in one temporary `HOME` activated standalone, Herdr, standalone, leaves the packages and the record with Herdr; a standalone event refresh under a Herdr record writes nothing; two writers in two processes serialize on the lock; a state with no `owner` file is Herdr's. Herdr differential: pinned, the lock file appears and `--runtime-info` gains `rex_ui`. Done when the tests pass and the differential shows only the pinned changes.

U4. **`runtime.py` takes its state directory** [H]. `--state-dir`, `--verify activate|install`, `owner`, `source.json`, the store-and-Rex preflight and migration record for `--verify install` with `namedrop rex sessions --json`, and Rex-bound sessions in `update.py`'s preflight. `run.sh` passes Herdr's values. Tests: `tests/update_lifecycle.py` and `tests/seamless_native.py` pass unchanged; `--verify install` with no `herdr` on `PATH` publishes and records its migration from the store and the Rex survey; a schema change with a working Rex agent stops with the preflight message; `--verify install` refuses a Herdr-owned directory; Herdr's preflight counts a Rex-bound session from a fixture store. Two new lanes:
- **The upgrade lane.** `tests/update_lifecycle.py --upgrade-from <archive or ref>` publishes the old release's `runtime.py` and binary into a scratch state, then the new ones, once through `--verify activate` with a fake `herdr` and once through `--verify install` with none on `PATH`, and checks that the names and the store survive.
- **The fixture differential.** The old and the new `runtime.py` run on the same fixtures (a first publish, a live update, a schema update, and a failed activation that rolls back) with a fake `herdr` that logs its argv. The files each writes and the `herdr` calls each makes must match, except `owner` and `source.json`.

Herdr differential: pinned, `owner` (`herdr`) and `source.json` appear, and the `herdr` call logs are equal. Done when `/verify` passes on Herdr, the upgrade lane passes, and both differentials show only the pinned files.

U5. **Popups as layers** [H]. The Rex arm of `OpenPopup`, `popup_argv`, the screens' bounds, `ui run`'s key wait after a late error on Rex, the two refusals (a `herdr` foreground and a Namedrop screen's), and the activation offer in an inactive Rex session. Tests: a scripted wire pins the `session.new_layer` request for each screen; its argv parses back into the five targets; each refusal comes before any request; an inactive Rex session's name screen offers activation, and Enter activates and continues. Startup failures: a missing launcher makes `new_layer` fail and `ui launch` report it; a refusal within 250 ms leaves the layer open with its message; a late error waits for a key. The live check runs `ui launch name --endpoint --space --pane` from a Rex-lane block, sees "Name this pane" in the layer's capture, types a name and Enter twice, and sees the block named and the layer closed, then runs each startup failure. Herdr differential: nothing pinned; the naming screen's Herdr text and behavior are unchanged. Done when every screen opens in a layer on the lane, on both Rex builds.

U6. **The module, `rex action`, and the guarded start** [H]. The generated text, written by the owner's package refresh where `rex.json` points; `namedrop rex action` with its name table and its decoded error line; `rex ensure --from-server` with the guard first in `main`; activation's single layer; and Doctor's key and action report through `client.keymap`. Tests: the module's text is pinned; `rex config check` of the module with a stub launcher reports ten actions and no error; each name maps to its command, and an unknown name fails with the reinstall message; a failure envelope becomes one stderr line; a guard with a parent that is not the server, a forwarded socket, or no socket exits 0, creates no state directory, and logs no call on a fake `herdr`; activation that opens the tour opens no Doctor. The live check, on both builds: a server start runs one bridge, `config check` and a second `config reload` start no second one, `rex do namedrop_name --origin palette` opens the name screen, and a failing action prints `Error: <line>`. Herdr differential: nothing pinned; a machine with no `rex.json` gets no module. Done when the live check passes on both builds.

U7. **`namedrop install` and its removal** [H]. The confirmation, adoption with the `rex_ui` check, the path Rex reads, the six `init.lua` guards, `--print-config`, `rex.json`, the readback, the handoff in Herdr's activation, and `install --remove`. Tests in a temporary `HOME`, against a scratch server:
- A second run changes no file, and an unowned `namedrop.lua` is refused.
- A comment or a string naming `require("namedrop")` does not count as the line.
- A symlinked `init.lua` is printed, not edited, and the module lands in the link's folder.
- A removed line is never added again.
- A file that fails before the edit is not edited; a reload failure after the edit restores the backup byte for byte.
- `--print-config` leaves `init.lua` unchanged.
- A missing action in the readback fails the install.
- Adopting a launcher without `rex_ui` is refused with the update message.
- The handoff stops the old state's helpers and rebakes the packages, the record, the module, and `rex.json`.
- `--remove` with a standalone owner removes all it wrote, and with a Herdr owner only the Rex half.
- The ten `init.lua` shapes of [Evidence](#the-revisions-probes) are fixtures.

Herdr differential: nothing pinned; Herdr's activation on a machine with no `rex.json` and no standalone owner is unchanged. Done when an install into a scratch `HOME` makes `rex actions --json` list the ten actions from `namedrop.lua` on both builds, and `--remove` returns `init.lua` byte for byte.

U8. **`scripts/install.sh` and the archive** [H]. The script, its checks, its `rex` lookup, and its place in `package-release.sh` and `tests/release.rs`. Herdr differential: pinned, the archive gains `install.sh` and nothing else changes for Herdr's `setup.sh install`. Done when an archive built by `package-release.sh` installs into a scratch profile with no `herdr` on `PATH`, `install.sh` outside a Rex block with no `rex` on `PATH` stops and names both ways, and Herdr's `setup.sh install` from the same archive passes `/verify`.

U9. **Standalone updates** [H]. `check-update.py` reads `source.json`, the standalone settings file, the standalone notice text, and the wrapper's second exact pattern. Tests: a scripted `git ls-remote` with a newer tag yields `ND-UPDATE-001` with the standalone text; the wrapper forwards both texts and nothing else; the owner picks the settings file; a Herdr install's notice and settings path are unchanged. The live check, on a scratch standalone install: an update is seen from a fixture tag; installing the newer fixture archive, whose state schema differs, migrates the store and keeps the names; a fixture archive whose activation fails restores the previous runtime, and the module still runs it. Herdr differential: nothing pinned. Done when the update and the rollback pass.

U10. **Words, by test** [H]. The sweep test of [Words](#words), with its allowlist, and the texts it finds changed. Herdr differential: nothing pinned. Done when the sweep passes and the differential is clean.

U11. **The testbed's Rex lane installs without Herdr.** `testbed rex up` runs the build's `install.sh` into the profile instead of activating in a profile Herdr, and checks that no `herdr` is on the profile's `PATH`. Done when the step 4 and step 5 live checks pass on the new lane.

U12. **Install orders, removal, and the relay, live.** On the lanes:
- Herdr first, then Rex: `install.sh` adopts with one runtime and a module baked to Herdr's launcher, and refuses with a Herdr build from before this step.
- Rex first, then Herdr: the handoff stops the old bridge, the module follows, and Doctor's `ND-INSTALL-001` counts the old names.
- `install --remove` after each order.
- Two lanes: B is installed by `install.sh` only, and B's server is restarted. No Namedrop command runs on B, so B's bridge starts only from the module's load-time start. A mentions a named agent on B, and the reply comes back.

Done when all four pass.

U13. **Live check for step 6.** On the lane: install, `rex do namedrop_activate --origin palette` (one layer opens), then `rex do namedrop_name --origin palette` in a Claude Code block and in a Codex block, typing the names into each layer. The two agents then mention each other with no manual command, as in step 4's check. Then an operator step: **Operator:** in a disposable macOS account whose default Rex server is a test server, with no other Rex config, install from the archive, open the Rex app, and run Activate Namedrop and Name this pane from the palette and by key. Record whether the palette lists the ten actions, whether the key opens the name screen, and the app server's `HOME` and `XDG_*` from Doctor. Record the evidence in PLAN.md and PORT.md. This meets PLAN's step 6 done-when.

## Release checks

These are measured before the first release and are not commits.

- **Linux.** On a Linux Rex server, whether a `rex config check` run inside a Rex block hands its detached child to the server as a subreaper (the server's `PR_GET_CHILD_SUBREAPER`, and the child's parent at once), and `install.sh` end to end. This is deferred to PLAN's "Before the first release" #11, which already holds the Linux B.
- **TLS and the operator's own hosts.** PLAN's #11 as written.

## Deferred

A full uninstall command for both hosts, moving a Rex-first store into Herdr's, an update command that downloads the archive, a Rust port of `runtime.py`, palette arguments, notifications through OSC 777, popup sizes in cells, and a Linux run of the installer, which PLAN's "Before the first release" #11 holds with the Linux B.

## Decisions

The first draft left four calls to the operator. They are made.

- **No Lua revision gate.** The Rust handshake is the only version gate, and its tested range widens per release. The module carries no revision, so it cannot refuse a server that the binary accepts.
- **A version-stable module.** The module holds names, titles, keys, and `endpoint()`, and every action calls `namedrop rex action`. A binary update cannot break a loaded module.
- **`init.lua` is edited, under guards.** `pcall(require, "namedrop");` goes in as line 1, under the six guards of [Editing `init.lua`](#the-installer), and `--print-config` edits nothing.
- **Keys are bound by default.** A key already bound is skipped, and Doctor reports the skipped keys and the keys in effect.
- **The palette check runs in a disposable macOS account** whose default Rex server is a test server. U13 writes it as an operator step.

## Open questions and risks

- **The palette may treat custom actions differently from the docs.** If the actions are missing from the palette, the keys and `rex do` still work, and the done-when is not met until Rex fixes it.
- **Rex is a preview.** Each release can change the API. The handshake refuses a revision outside its tested range with one error, and the range widens only after the probes of [What exists in Rex](#what-exists-in-rex) run again on the new release.
- **The app's server environment is unmeasured.** The probe's server had `PATH=/usr/bin:/bin`. The module names every program by its absolute path, so the app's `PATH` does not matter, but `HOME` and `XDG_*` in the app's server decide where `rex.exec` children look for the install record. The module passes `NAMEDROP_STATE_DIR`, so the record is not read there. U13's operator step records them.
- **A Herdr release from before step 6 takes no lock.** It can still rebake the packages over a standalone install. Doctor reports `ND-INSTALL-003` and names the fix, but nothing prevents it.
- **On Linux, a subreaper may adopt the checker's child.** The guard's parent test is measured only on macOS ([Release checks](#release-checks)).

## Review history

Two reviews of the first draft (`ad6883bc`) said to revise before building, and both kept shape C. The operator then made the four open calls ([Decisions](#decisions)). This revision applies them.

- **The first review** (Codex) found that ownership did not protect the shared packages, since two runtimes took turns owning them, so one per-user lock and one owner now cover every writer. It found that the module's registration could not follow a handoff, so it moved to `~/.config/namedrop/rex.json`, and the handoff is spelled out. It found that appending the `require` line could break a valid `init.lua`, which led to the line-1 `pcall` and its guards. It asked for a capability check before adopting a Herdr launcher (`rex_ui`), for [H] on every shared commit with a Herdr differential, and for update success and rollback checks. It found that a layer does not close after a quick failure, so the wording is corrected and U5 tests startup failures. It also asked that the load-time guard run before any side effect, with its socket discovery spelled out and called a correctness guard, and suggested the disposable account for the palette check, refusing targets inside Namedrop layers, offering activation from the naming screen, and opening one layer on activation.
- **The second review** (Opus) found that the Lua revision gate refuses preview.18, which the app now ships, so the gate is gone and the module calls one Rust entry point. It found `runtime.py`'s `herdr` calls on first publish and in the preflight (H2), so `--verify install` uses the store and a Rex survey, and the helper-stop half of the first draft, which nothing reaches, is dropped (M2). For `init.lua` (H3) it measured the append failures, the symlink rule, and that a failed reload keeps the last good config, which set the six guards. Ownership (H4) gained the missing-owner rule and Rex sessions in Herdr's update preflight. It kept Herdr's tour marker and limited the release to `Unsupported` on Rex (M3), reordered the commits so each piece precedes its first use (M5), and added the `rex actions` readback (M6). Its lows became the decoded error line, the `rex` lookup through `PATH` or `REX_BIN_DIR`, a single layer on activation, the Words test, `ui install-shortcuts` on a Rex home, and the standalone update setting.
- **This revision's own probes** measured every Rex fact again on preview.18, found the `;` that a first line starting with `(` needs, found that `#!` and BOM files are already errors, measured the guard's parent and peer in six contexts, and loaded the generated module verbatim on both builds.

## Evidence

The probes ran with `X=/tmp/ndui`, a link `$X/rex` to the bundled `rex`, and the wrapper `$X/rx`, which runs `env -i HOME=$X/home PATH=/usr/bin:/bin $X/rex --autostart=false -S unix://$X/s.sock "$@"`. The first server ran as `env -i HOME=$X/home USER SHELL PATH=/usr/bin:/bin $X/rex server run --listen unix://$X/s.sock --state-dir $X/state`, with no `--config`, and `rex config check` printed `path: /tmp/ndui/home/.config/rex/init.lua`. Before any Namedrop command the server ran again with `REX_SERVER=unix://$X/s.sock`, so no block's Namedrop probed the default socket. Namedrop ran as `env -i HOME=$X/home PATH=$X/bin:/usr/bin:/bin NAMEDROP_STATE_DIR=$X/nd-state target/debug/namedrop`, with no `herdr` on that `PATH`.

```sh
rex do -s "$S" -b "$B" nd_probe_ctx --json
# "ctx": {"block_id": "block:…", "client_id": "client:…", "origin": "api", "session_id": "session:…"}
# "package_path": "/tmp/ndui/pkg/?.lua;/tmp/ndui/home/.config/rex/?.lua;/tmp/ndui/home/.config/rex/?/init.lua;./?.lua;/usr/local/share/lua/5.1/?.lua;/usr/local/share/lua/5.1/?/init.lua"
# "cpath": "", "version": "Lua 5.1", "src": "/tmp/ndui/home/.config/rex/namedrop.lua", "ndpkg": true

rex do -s "$S" -b "$B" nd_probe_version --json
# "status": {"api_revision": 12, "listeners": [{"addresses": ["/tmp/ndui/s.sock"], "endpoint": "unix:///tmp/ndui/s.sock", "primary": true, "transport": "unix"}], "pid": 54393, …}

rex do -s "$S" -b "$B" nd_probe_layer        # a layer whose program reads a line and writes its env
rex send -s "$S" -b "$LAYER" alpha; rex send-key -s "$S" -b "$LAYER" enter
# layer.txt: alpha block:<the layer's block> session:<S>
# env: REX_SESSION, REX_BLOCK=<the layer's block>, REX_BIN_DIR, HOME=<the account's home>, no REX_SERVER
# The block count fell back to 1 when the program exited. With login = false, HOME was /tmp/ndui/home.

rex do -s "$S" -b "$B" layer-name.lua        # new_layer with shell none, login false, and the argv below
#   /usr/bin/env NAMEDROP_TARGET_WORKSPACE=$S NAMEDROP_TARGET_PANE=$B NAMEDROP_TARGET_TERMINAL=$B
#   NAMEDROP_TARGET_SESSION= NAMEDROP_STATE_DIR=$X/nd-state namedrop --endpoint rex:/tmp/ndui/s.sock ui run name
rex capture -s "$S" -b "$LAYER" --trim
#  Name this pane
#  Letters, digits and single hyphens. Esc cancels without saving.
#  Mention name: @
rex send -s "$S" -b "$LAYER" term1; rex send-key -s "$S" -b "$LAYER" enter; rex send-key -s "$S" -b "$LAYER" enter
namedrop --endpoint rex:/tmp/ndui/s.sock --space "$S" --pane "$B" --json address list
# {"alias": "term1", "pane_id": "block:…", "pane_label": "@term1", "pane_type": "terminal", …}

rex do -s "$S" -b "$B" --origin palette nd_probe_doctor --json
# {"endpoint": "rex:/tmp/ndui/s.sock", "ok": true, "status": "healthy", "agents": 1}   real 0.03

# init.lua: rex.exec.start({"/bin/sh", "-c", "echo $$ > loadstart.pid; exec sleep 600"}, {detach = true})
rex config reload     # a second sleep, parent 74599 (the server)
rex config check      # a third sleep, parent 1, with the checker's environment
```

`namedrop activate --input-json '{"confirmed":true}'` with the Rex endpoint returned `activated: true` and `shortcuts.path: /tmp/ndui/home/.config/herdr/config.toml` with five bindings and `reloaded: true`, and the file held them. The commands started `namedrop rex bridge --endpoint rex:/tmp/ndui/s.sock` on their own. Afterwards the session was killed, the bridge and the server were stopped by pid, every probe `sleep` was killed by pid, and the user's `~/Library/Application Support/rex/servers` held no entry newer than the probe.

### The revision's probes

The second round ran each probe on preview.14 and again on preview.18, on a fresh scratch server per probe, started as `env -i HOME=$X/home USER=$USER SHELL=/bin/sh PATH=/usr/bin:/bin [REX_SERVER=unix://$X/s.sock] rex server run --listen unix://$X/s.sock --state-dir $X/state --restore=false`, with `X` under `/tmp` and every client call made as `rex --autostart=false -S unix://$X/s.sock`. The results matched on both builds except where a row names one.

```sh
# init.lua shapes: reload the unedited file, insert `pcall(require, "namedrop")` as line 1, reload again
rex config reload        # good file:                 generation: 7, exit 0, actions mine and namedrop_t
rex config reload        # error("mine broke") later:  error init.lua:3: mine broke, exit 1, actions still the last good file's
rex config reload        # first line `#!…` or a BOM: already an error before the edit
rex config reload        # first line `(function() … end)()`: attempt to call a non-function object; with `;` after the line: exit 0
rex config reload        # namedrop.lua missing, or a syntax error in it: exit 0, no namedrop_ action
rex do -s "$S" -b "$B" namedrop_t --json   # rex.bindings{key} at line 1: 0 for alt+shift+cmd+i, cmd+d, shift+cmd+p, ctrl+b

# A load-time rex.exec.start of a program that connects to REX_SERVER, else the default socket, and compares
# the socket's peer pid with its own parent pid, at once:
#   context              parent   peer     verdict
#   server start/reload  server   server   accept
#   rex config check     1        server   refuse    (no socket found when REX_SERVER is unset and the socket is not the default)
#   rex do <script>      1        server   refuse
#   an action's rex.exec server   server   accept
#   REX_SERVER=forwarder server   forwarder refuse
# rex actions, rex keymap, and rex api call client.keymap started none.

rex api call session.new_layer '{… "options": {"shell": "none", "login": false, "command": ["/usr/bin/false"]}}'
rex block inspect -s "$S" "$LAYER"   # after 2 s: still there. sleep 1; exit 1 → gone. /usr/bin/true → gone.
                                     # exit = {quick_exit_threshold_ms = 0} → gone. /nonexistent/namedrop → new_layer fails, status 500

# Revision settling, a raw hello on /api/control:  offer (high, low) → welcome api_revision
#   preview.14: (12,12) 12  (13,12) 12  (13,13) closed  (14,12) 12
#   preview.18: (12,12) 12  (13,12) 13  (13,13) 13      (14,12) 13

namedrop --endpoint rex:$X/s.sock --space "$S" --pane "$B" --json doctor        # ND-SETUP-001, both builds, both servers
namedrop --endpoint rex:$X/s.sock --space "$S" --pane "$B" activate --input-json '{"confirmed":true}'   # activated: true
# then the naming layer as above, `term1`, Enter, Enter: label @term1, address list [term1, terminal]
NAMEDROP_REX_LIVE=unix://$X/s.sock NAMEDROP_REX_LIVE_HOME=$X/home <test binary> mux::rex::live::<test> --ignored --test-threads=1
```

The same `rex` outputs were saved from both servers and diffed: `--help`, `__internal skill`, `config --help`, `actions --help`, `keymap --help`, `do --help`, and `api describe` for `server.status`, `session.new_layer`, `client.keymap`, and `server.config_reload` were identical. The generated module in [The Lua module](#the-lua-module) was then loaded verbatim on both builds, behind the line, with a stub launcher that logs its parent pid and argv and fails `doctor` with one stderr line.

```sh
rex actions --json   # namedrop_activate … namedrop_tour, ten actions, each with source namedrop.lua
rex keymap           # alt+shift+cmd+{i,h,s,o} namedrop.lua:39; alt+shift+cmd+m mine init.lua:3 above namedrop_manage
rex do -s "$S" -b "$B" --origin palette namedrop_name     # exit 0; stub: <server pid> rex action name --endpoint rex:$X/s.sock --space $S --pane $B
rex do -s "$S" -b "$B" --origin palette namedrop_doctor   # Error: Namedrop is not active in this session. Run Activate Namedrop.  exit 1
# stub log: "<server pid> rex ensure --from-server" at server start, "1 rex ensure --from-server" at rex config check
```

Each probe's server, bridge, deferred worker, and load-time program was stopped by pid, and none was left running.
