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
| How a client's revision is settled | The client's hello offers a range, and the server answers with the highest revision both support, or closes the connection. preview.14 answers 12 to the offers 12–12, 12–13, and 12–14, and closes on 13–13. preview.18 answers 12 to 12–12 and 13 to 12–13, 13–13, and 12–14. Today's Namedrop offers 12–12 and accepts only a welcome of 12 (`src/mux/rex/control.rs:13-14`, `:180`, `:202-203`) | Measured with a raw hello on each server |
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

These constraints carry over from steps 4 and 5. Herdr behavior does not change, except for text that names the host. Every existing test passes.

## Shapes considered

Four whole shapes could put the screens into Rex.

**A. Lua screens over `namedrop --json`.** The module would run `namedrop … --json`, read the result, and show it. Lua cannot show it: no Rex Lua call draws text or reads input, and a palette action cannot ask for an alias in any documented way. The shape would give Rex a list action and a fixed-argument name action for keys, and no naming from the palette. It fails the done-when.

**B. Lua opens layers that run Namedrop's TUI.** Each action calls `rex.session.new_layer` with a command that runs `namedrop ui run <mode>` and passes the targets through `/usr/bin/env` in argv. The probe proved this works with today's binary. The cost is a second copy of `PopupSpec` in Lua: the target terminal and agent session, which the module cannot read, the screen sizes, the state directory, and the target checks that `ui launch` does. The tour that opens after activation, and any future popup Namedrop starts itself, would still need a Rust path to `new_layer`.

**C. Lua runs `namedrop ui launch`, and the Rex adapter opens the layer.** Each action is one `rex.exec` of `namedrop --endpoint <server> --space <ctx.session_id> --pane <ctx.block_id> ui launch <mode>`. The Rex adapter renders `Effect::OpenPopup` as `session.new_layer`, with the same targets and checks Herdr gets, in argv instead of `--env`. The Lua module holds a table of actions and keys and nothing else.

**D. A Rex block plugin.** A Namedrop block type could draw its own screens. It needs `--block-dir` on the server's command line, which the app's server does not take from the person, and the docs describe nothing beyond the flag.

**Chosen: C.** It gives Rex every screen with one code path for both hosts. `ui launch` already finds the target, checks it, and emits `OpenPopup`, and `ui run` already reads its targets and refuses a changed pane, so the only new Rust is one adapter arm and the argv form of the targets. The tour after activation opens through the same arm. The module stays a table, so a change to a screen never touches Lua, and the module can be generated by the binary that it runs, with the launcher and state directory written in. The cost over B is one process start per action, 0.03 s for the whole `rex do` in the probe (measured, debug build).

## Design

### The Lua module

`namedrop install` writes `namedrop.lua` beside `init.lua` and adds one line to `init.lua`:

```lua
require("namedrop")
```

The generated module looks like this, with the paths written in at install.

```lua
-- Managed by Namedrop 1.8.0. `namedrop install` rewrites this file, so edits here are lost.
-- Remove a key after the require line with rex.unbind("alt+shift+cmd+i").
local NAMEDROP = "/Users/me/.local/state/namedrop/runtime/namedrop"
local STATE = "/Users/me/.local/state/namedrop"
local REVISIONS = { low = 12, high = 12 }

local function endpoint()
	local status, err = rex.call("server.status")
	if status == nil then error("Namedrop could not ask Rex for its server: " .. err, 0) end
	if status.api_revision < REVISIONS.low or status.api_revision > REVISIONS.high then
		error(string.format("Namedrop supports Rex API revision %d to %d, and this server is %d. Update Namedrop.",
			REVISIONS.low, REVISIONS.high, status.api_revision), 0)
	end
	for _, listener in ipairs(status.listeners or {}) do
		if listener.primary and listener.transport == "unix" then return "rex:" .. listener.addresses[1] end
	end
	error("Namedrop needs a Rex server that listens on a Unix socket", 0)
end

local function namedrop(ctx, ...)
	if ctx.session_id == nil or ctx.block_id == nil then error("Run this from a Rex pane", 0) end
	local argv = { NAMEDROP, "--endpoint", endpoint(), "--space", ctx.session_id, "--pane", ctx.block_id, ... }
	local result, err = rex.exec(argv, { env = { NAMEDROP_STATE_DIR = STATE }, timeout = 25 })
	if result == nil then error(err, 0) end
	if not result.ok then error(result.stderr ~= "" and result.stderr or result.stdout, 0) end
end

local actions = {
	{ "namedrop_activate", "Activate Namedrop", { "activate", "--input-json", '{"confirmed":true}', "--open", "doctor" } },
	{ "namedrop_name", "Name this pane", { "ui", "launch", "name" }, "alt+shift+cmd+i" },
	{ "namedrop_manage", "Manage Namedrop names and agents", { "ui", "launch", "manage" }, "alt+shift+cmd+m" },
	{ "namedrop_doctor", "Namedrop Doctor", { "ui", "launch", "doctor" }, "alt+shift+cmd+h" },
	{ "namedrop_status", "Agent status", { "ui", "launch", "status" }, "alt+shift+cmd+s" },
	{ "namedrop_tour", "Namedrop tour", { "ui", "launch", "tour" }, "alt+shift+cmd+o" },
	{ "namedrop_channels", "Open Namedrop Channels", { "channels" } },
	{ "namedrop_pause", "Pause Namedrop", { "space", "pause" } },
	{ "namedrop_resume", "Resume Namedrop", { "space", "resume" } },
	{ "namedrop_deactivate", "Deactivate Namedrop", { "deactivate" } },
}

for _, a in ipairs(actions) do
	local name, title, argv, key = a[1], a[2], a[3], a[4]
	rex.action{ name = name, title = title, category = "Namedrop", keywords = "namedrop mention agent",
		run = function(ctx) namedrop(ctx, unpack(argv)) end }
	if key ~= nil and #rex.bindings{ key = key } == 0 then rex.bind(key, name) end
end

rex.exec.start({ NAMEDROP, "rex", "ensure", "--from-server-start" },
	{ env = { NAMEDROP_STATE_DIR = STATE }, detach = true, stdout = false, stderr = false })
```

Five choices in it need a reason.

- **The endpoint comes from `server.status`.** Rex does not set `REX_SERVER` in every block, and an action's `ctx` carried no server (measured). The primary Unix listener is the canonical endpoint that `prove_local` already uses (ADAPTER.md, "The endpoint string carries the kind"). An explicit `--endpoint` decides the home with no socket probe, and the adapter still proves the server local before it types anything.
- **The revision gate runs in the action.** At load, `rex.call` fails, so the module cannot read the revision then. Each action checks it first and names both revisions in its error. The Rust adapter keeps its own check, so the CLI is gated too.
- **Keys bind only where this file has none.** `rex.bindings{ key = … }` returns the bindings the file has made so far, so a key the person bound above the `require` line keeps that binding, and a binding below it wins as it would anyway. The keys are Herdr's letters on the one modifier set the Mac app leaves free. The bindings carry action names, so `rex keymap` shows each one with its source.
- **No palette action takes an argument.** The palette's handling of arguments is undocumented, and every input is read by a screen.
- **The last line starts the bridge at server start, behind a guard.** A top-level `rex.exec` also runs at `rex config check`, in the checker's environment (measured). `namedrop rex ensure --from-server-start` proves that its parent process is a local Rex server, by peer pid on the server's socket as `prove_local` does, and otherwise exits doing nothing. So a server restart starts the bridge before any agent runs, which a remote host needs to accept relayed mentions (PLAN.md, "With B's bridge stopped, A keeps the item"), and a `config check` starts nothing. `ensure` is already idempotent through the bridge lock.

Activation is the one action with no screen of its own. `activate --open doctor` activates the session and then opens the Doctor screen, which shows what remains, such as Codex's hook approval. Herdr's `activate` does not take `--open`.

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

`ui launch` on Rex refuses a target block whose foreground program is `herdr`. A person who runs Herdr inside Rex and opens Rex's palette over the Herdr block would otherwise name the Rex block that holds Herdr. The refusal says to use Herdr's own Name this pane.

### Activation on Rex

`activate` stops writing Herdr's keys for a Rex home, and reports `shortcuts: {"status": "rex_module", "path": "<config dir>/namedrop.lua"}` instead. `activate_shortcuts` returns an error for `Applied::Unsupported` rather than `reloaded: true`, so no host reports a reload that did not happen. The tour consumes its marker only when the popup reports `Opened`. With the adapter arm above, the tour after activation opens in a layer on Rex as it does in a Herdr popup.

### The installer

A person on a machine with Rex and no Herdr does four things.

1. Download the release archive and its `.sha256`, check it, and unpack it. Or clone the repository and run `sh scripts/build.sh`.
2. Run `sh scripts/install.sh`. It shows what it will change and asks once.
3. Open the Rex palette in a pane and run **Activate Namedrop**. Doctor opens and lists anything left, such as Codex's hook approval.
4. Run **Name this pane**, or press <kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>Cmd</kbd>+<kbd>I</kbd>, in each pane to name, and start new agent sessions.

`scripts/install.sh` checks the platform as `build.sh` does, checks for `python3` and `rex`, and runs `python3 scripts/runtime.py <root> --state-dir <dir> --verify install`. The state directory is the one `locator::state_directory` gives a Rex home: `NAMEDROP_STATE_DIR`, the install record, then `$XDG_STATE_HOME/namedrop` or `~/.local/state/namedrop`. The runtime and the confirmed install then put these on the machine.

| What | Where | Written by |
|---|---|---|
| The runtime | `<state>/runtime/releases/<sha256>/namedrop`, the `current` link, and the launcher `<state>/runtime/namedrop` | `runtime.py`, as for Herdr |
| The install source | `<state>/runtime/source.json`: the version, and `archive` or `checkout` with its origin | `runtime.py` |
| The agent packages, permissions, Codex rules and sandbox root, `~/.local/bin/namedrop`, and the install record | As Herdr's activation writes them, baked to the launcher and the state directory | `namedrop install` |
| The Lua module | `<Rex config dir>/namedrop.lua`, with the managed-file line first | `namedrop install` |
| The `require` line | Appended to `init.lua`, or `init.lua` created with it, unless a `require("namedrop")` line is there | `namedrop install`, after a backup `init.lua.namedrop-<pid>.bak` |
| The Codex plugin | `codex plugin marketplace add` and `codex plugin add namedrop@namedrop` | `runtime.py`, as for Herdr |
| The config reload | `rex --autostart=false config reload` | `namedrop install` |

`namedrop install` is a new command. It refuses without `--input-json {"confirmed":true}` and then lists every target, as `activate` does (`src/install.rs:203-226`). It shares `activate`'s package installer, which is already host-neutral, and it activates no space. It finds the Rex config dir by Rex's own rule, `REX_CONFIG` then `$XDG_CONFIG_HOME/rex` then `~/.config/rex`, and not with `rex config check`, which would run the file's load-time code. It refuses to replace a `namedrop.lua` whose first line is not its own. It records the module's path in `<state>/rex/module`, and each later package refresh (`src/integration.rs:545-548`) rewrites the module at that path only, so the module's paths follow every update and a machine with no Rex install never gets one. `config reload` is refused over a remote connection (Rex's docs), so the installer runs it only against the local default server, and otherwise prints the command.

**`scripts/runtime.py` becomes host-neutral.** It takes `--state-dir` and `--verify activate|install`. `run.sh activate` passes `--state-dir "$HERDR_PLUGIN_STATE_DIR" --verify activate`, so Herdr's path runs the same steps with the same values. `--verify install` runs `<launcher> install` and checks `state_directory` and the packages instead of the space. `helper_kind` and `queue_wake` also recognize a worker by `NAMEDROP_STATE_DIR`, `NAMEDROP_ENDPOINT`, and `NAMEDROP_SPACE_ID` (PORT.md, "Finding status workers"), and the publish stops a Rex bridge, relay worker, and deferred worker of the same state directory the way it stops Herdr's helpers. The bridge and the relay worker already run the replacement runtime when they see one (ADAPTER.md, "The relay worker"), so a stop is needed only when the schema migrates. A Rust port of `runtime.py` would drop the `python3` requirement, which Herdr has too. This design keeps the script, because it is the one piece that must run outside the binary it replaces.

### One Namedrop on a machine with both hosts

The agent packages are one slot: `~/.claude/skills/namedrop`, `~/.agents/skills/namedrop`, `~/.local/bin/namedrop`, the install record, the Codex rules, and the Codex plugin name each hold one launcher and one state directory, and Claude Code and Codex load one set of hooks. Two installs cannot each own a copy without firing every hook twice. So one machine has one Namedrop: one state directory, one runtime, one set of packages. Both hosts' spaces live in one store, kept apart by the endpoint kind in `session_socket_id`, and each process resolves its host through `mux::home`. The testbed's Rex lane already runs this way: it activates in a profile Herdr, and its Rex blocks reach Herdr's state directory through the install record (`testbed/README.md:335-339`).

- **Herdr first, then Rex.** `install.sh` finds the install record, or the state directory baked into the existing packages, naming Herdr's plugin state, and adopts it. It installs no second runtime and changes no package path. It writes the Lua module and the `require` line, baked to Herdr's launcher. Herdr's updates then update both hosts, and the module follows through the package refresh.
- **Rex first, then Herdr.** Herdr fixes its own state directory, so its activation cannot adopt Rex's. Its package install rebakes every package and the record to Herdr's directory, and the module follows. Names made on Rex before that stay in the old directory. Doctor reports `ND-INSTALL-001`, naming the old directory and its count of live names, and the person names those panes again. Moving that store into Herdr's is deferred.
- **Ownership.** `runtime.py` writes `<state>/runtime/owner`, `herdr` or `standalone`. `install.sh` with a `herdr` owner adopts, and `--verify install` never publishes a runtime into a Herdr-owned directory, so a standalone install cannot flip the packages back.

### Updates

A standalone install has no Herdr registration, so `check-update.py` learns its source from `source.json`. For an archive or a GitHub checkout it lists the repository's `v*` tags with `git ls-remote --tags` and records the newest. Doctor's `ND-UPDATE-001` and the SessionStart notice then say `Update: download Namedrop <version> and run sh scripts/install.sh`, and Herdr installs keep today's text. The SessionStart wrapper accepts either text by exact pattern, as it accepts one today (`src/install.rs:1315-1357`). Running `install.sh` from the new archive publishes the new runtime, refreshes the packages and the module, and reloads Rex's config. An update command that downloads and checks the archive itself is deferred.

### Remote hosts

On host B, the machine that receives relayed mentions, a person does this.

1. Install Namedrop on B as above. On a Linux server the Rex config dir is `~/.config/rex` too, and `install.sh` reloads B's config only when it runs on B.
2. Start the agents on B in Rex blocks, activate each session, and name the agents, with `namedrop` in B's blocks, or from the palette of an app attached to B. That B's palette shows B's actions is inferred from Rex's docs and unverified.
3. To let A mention a terminal on B, create `<B's state>/relay/allow-terminals`. A terminal runs a relayed message as a shell command, so this is off by default. `namedrop doctor` on B says whether it is on.

On A, the person adds B with `rex hosts add devbox <endpoint>`, which goes to A's home, and checks it with `rex hosts check`. B needs A in its own hosts only to start new mentions to A. Replies flow back by A's pull. B's bridge must run for B to accept an item, and the module's guarded start runs it from B's server start. Doctor on A reports relay failures as `ND-RELAY-002` and `ND-RELAY-003`. PLAN's item 10 writes these steps into the user docs.

### Words

Text that names Herdr where the host is Rex changes: the packaged `SKILL.md` description ("inside Herdr"), the `activate`, `deactivate`, `address`, and `space` help ("this Herdr space"), Doctor's `ND-SESSION-002` repair for a Rex block (PORT.md, "Doctor's refusal line"), and the tour's "Herdr returned an unreadable popup result". A Herdr home keeps its Herdr wording where it gives a Herdr command.

## Tradeoffs accepted

- **A process start per action.** Each action runs the binary once. The probe measured 0.03 s.
- **`python3` stays a requirement.** Herdr requires it too, and the installer checks for it.
- **Rex-first names are lost when Herdr is installed later.** Doctor says so and counts them.
- **The palette is proven only by `--origin palette` until the operator's check.** The app attaches only to the default server.
- **Popup bounds are fractions of the window.** A screen of 72 columns in a narrow window wraps, and the layer cannot be sized in cells.

## Implementation plan

Every commit compiles, passes `cargo fmt --check`, `cargo clippy --locked --all-targets -D warnings` with and without `--all-features`, and `cargo test --locked --lib`, and ends in a state you can check. **[H]** marks a commit that touches a Herdr path. Run `/verify` on real Herdr before it, including the Herdr-in-Rex lane. Live Rex checks run on the testbed's Rex lane, or on a scratch server under `env -i` with its own `HOME`, state, and `REX_SERVER` naming itself.

U1. **Activation reports what happened** [H]. A Rex home's `activate` writes no Herdr keys and reports `rex_module`. `Applied::Unsupported` from a reload is an error in the shortcut report. The tour keeps its marker unless the popup reports `Opened`. Tests: a Rex activation in a temporary `HOME` leaves no `.config/herdr/config.toml` (today's binary writes it, measured), a Herdr activation writes today's file byte for byte, and a tour whose popup is `Unsupported` is shown on the next detection. Done when the probe's activation, run again, leaves no Herdr config.

U2. **Popups as layers.** The Rex arm of `OpenPopup`, `popup_argv`, the screens' bounds, and the refusal for a block whose foreground is `herdr`. Tests: a scripted wire pins the `session.new_layer` request for each screen, its argv parses back into the five targets, and a `herdr` foreground refuses before any request. The live check runs `ui launch name --endpoint --space --pane` from a Rex-lane block, sees "Name this pane" in the layer's capture, types a name and Enter twice, and sees the block named and the layer closed. Done when every screen opens in a layer on the lane, and the tour opens after activation.

U3. **`runtime.py` takes its state directory** [H]. `--state-dir`, `--verify`, `owner`, `source.json`, and the neutral names in `helper_kind` and `queue_wake`. `run.sh` passes Herdr's values. Tests: `tests/update_lifecycle.py` and `tests/seamless_native.py` pass unchanged, a publish into a scratch standalone state stops and wakes a status worker started under the neutral names (it is missed today), and `--verify install` refuses a Herdr-owned directory. Done when `/verify` passes on Herdr.

U4. **`namedrop install`.** The confirmation, the shared package installer, adoption, the Rex config dir, the module and the `require` line with the backup, `<state>/rex/module`, the refresh rule, and the local reload. Tests in a temporary `HOME`: a second run changes no file, an unowned `namedrop.lua` is refused, an existing `require("namedrop")` line is not added again, a Herdr-owned record is adopted with no runtime published, and a refresh rewrites the module only where `<state>/rex/module` points. Done when an install into a scratch `HOME` makes `rex config check` on the scratch server report ten actions and no error.

U5. **The module and the guarded start.** The generated text, `namedrop rex ensure --from-server-start`, `activate --open doctor`, and Doctor's check that the server has loaded `namedrop_name`, through `client.keymap`. Tests: the module's text is pinned, `rex config check <module>` reports no error, a scripted parent that is not a Rex server makes `ensure` exit without a bridge, and the revision gate's error names both revisions. The live check: a scratch server start runs one bridge, `config check` and a second `config reload` start no second one, and `rex do namedrop_name --origin palette` opens the name screen. Done when the live check passes.

U6. **`scripts/install.sh` and the archive.** The script, its checks, and its place in `package-release.sh` and `tests/release.rs`. Done when an archive built by `package-release.sh` installs into a scratch profile with no `herdr` on `PATH`.

U7. **Standalone updates** [H]. `check-update.py` reads `source.json`, the standalone notice text, and the wrapper's second exact pattern. Tests: a scripted `git ls-remote` with a newer tag yields `ND-UPDATE-001` with the standalone text, the wrapper forwards both texts and nothing else, and a Herdr install's notice is unchanged. Done when a scratch standalone install sees an update from a fixture tag.

U8. **Words** [H]. The texts in [Words](#words). Tests: a Rex Doctor's `ND-SESSION-002` names no Herdr command, and Herdr's output is unchanged where it gives a Herdr command. Done when `/verify` passes on Herdr.

U9. **The testbed's Rex lane installs without Herdr.** `testbed rex up` runs the build's `install.sh` into the profile instead of activating in a profile Herdr, and checks that no `herdr` is on the profile's `PATH`. Done when the step 4 and step 5 live checks pass on the new lane.

U10. **Live check for step 6.** On the lane: install, `rex config reload`, `rex do namedrop_activate --origin palette`, then `rex do namedrop_name --origin palette` in a Claude Code block and in a Codex block, typing the names into each layer. The two agents then mention each other with no manual command, as in step 4's check. Then the operator runs the same steps from the Rex app's palette on a machine or account whose default server is a test server. Record the evidence in PLAN.md and PORT.md. This meets PLAN's step 6 done-when.

## Deferred

An uninstall command, moving a Rex-first store into Herdr's, an update command that downloads the archive, a Rust port of `runtime.py`, palette arguments, notifications through OSC 777, and popup sizes in cells.

## What only the operator can decide

- **The palette check.** The Rex app shows only its default server's actions. U10's last step needs a machine or macOS account whose default Rex server is a test server, or the operator's consent to install on their own server.
- **Editing `init.lua`.** The installer appends one line after a backup and lists it in the confirmation. The other choice is to print the line and let the person add it.
- **The default keys.** `alt+shift+cmd+{i,m,h,s,o}` follow Herdr's letters. The operator may prefer no keys by default.

## Open questions and risks

- **The palette may treat custom actions differently from the docs.** If the actions are missing from the palette, the keys and `rex do` still work, and the done-when is not met until Rex fixes it.
- **Rex is a preview.** The module gates on revision 12 only. Each Rex update needs the probe of [What exists in Rex](#what-exists-in-rex) again before the range widens.
- **The app's server environment is unmeasured.** The probe's server had `PATH=/usr/bin:/bin`. The module names every program by its absolute path, so the app's `PATH` does not matter, but `HOME` and `XDG_*` in the app's server decide where `rex.exec` children look for the install record. The module passes `NAMEDROP_STATE_DIR`, so the record is not read there.
- **Naming in an inactive session.** The probe named a pane after activation. Whether the name screen should offer activation, as Doctor's `ND-SETUP-001` does, is a call for U2.
- **`ctx.block_id` is "usually the focused" block.** A palette run from a Namedrop layer would target the layer. U2 should make `ui launch` refuse a block whose foreground process is `namedrop ui run`, which it can read from the block's process facts.

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
#   rex do <script>      1        —        refuse
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

The same `rex` outputs were saved from both servers and diffed: `--help`, `__internal skill`, `config --help`, `actions --help`, `keymap --help`, `do --help`, and `api describe` for `server.status`, `session.new_layer`, `client.keymap`, and `server.config_reload` were identical. Each probe's server, bridge, deferred worker, and load-time program was stopped by pid, and none was left running.
