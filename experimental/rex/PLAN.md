# Rex host support: feasibility and plan

Namedrop runs only inside Herdr today. This document records whether Namedrop can also run inside [Rex](https://www.superlogical.com/rex/docs), what the work costs, and the order to do it in. The work happens on the `experimental` branch.

The research dates from 2026-11-11 on macOS. It used Rex `0.1.0-preview.14` (commit `7e460d0`, API revision 12), Claude Code 2.1.322, and Codex CLI 0.163.0. Rex is an invite-only preview, so check every Rex fact here against the version you run.

This document calls Herdr and Rex *terminal hosts*. The code already uses `host` for the agent CLI, so the new port needs a different name.

## Rex can host Namedrop

Rex exposes every capability that the mention loop needs. A program can list panes, identify its own pane, send a prompt to another pane, read a screen, and learn when an agent starts or stops work. Rex has no plugin system, so Namedrop must bring its own installer, its own state directory, and a long-lived process that reads Rex events.

Most of the cost is in Namedrop. The code has no terminal-host abstraction. Herdr commands, Herdr JSON, and Herdr names run through about half of the non-test source and almost every test.

Rex support fits beside Herdr in one binary with two adapters. Ship it as experimental until Rex states an API stability policy. Rex has no public changelog and no published licence yet.

## What the live check proved

The check ran one Claude Code pane and one Codex pane in a scratch Rex session.

- **Prompt delivery works.** A two-line prompt, sent as one `paste` event and followed by an Enter key, arrived as one draft and submitted in both agents.
- **Agents report their own status.** Claude Code and Codex both emit the Rex program status sequence (OSC 7501) with no integration installed. Claude Code reported `idle`, `working`, then `done`. Codex reported `idle`, `working`, then `idle`.
- **Status changes arrive as events.** Each change appeared on `rex events` as `program_status_changed`.
- **Pane identity is stable.** A block keeps its ID when it moves between windows and when the server restarts. A process in a block gets `REX_BLOCK`, `REX_SESSION`, and `REX_SERVER`.

## How each Herdr seam maps to Rex

A Rex *session* holds *windows* (tabs), and a window holds *blocks* (panes).

| Namedrop needs | Herdr today | Rex | Fit |
|---|---|---|---|
| Pane identity | `pane_id`, which can change, plus `terminal_id` | `block:<uuid>`, never reused | Better than Herdr |
| Space | Workspace | Session. Rex has no workspace, and session labels change | Good. Key on the session ID |
| Agent kind and status | Herdr's agent integration reports `agent` and `agent_status` | The agent reports `app` and `state` through OSC 7501 | Verified |
| Status events | Plugin manifest runs one process for each event | `program_status_changed` on the event stream | Verified |
| Agent session for each pane | `pane list` reports `agent_session` | Not exposed | Gap. Namedrop's own SessionStart hook must record the block and the session |
| Prompt delivery | `agent prompt`, which rejects a busy agent before any input | `block.event` paste, then a key, on one API connection. No busy rejection | Works. Namedrop owns the readiness gate |
| Terminal pane delivery | `pane run` | `rex send`, then Enter | Works |
| Screen read | `pane read --lines N` | `rex capture`, which returns the whole scrollback | Works. Trim on the Namedrop side |
| Pane lifecycle events | Manifest events for created, moved, closed, and exited | `block_created`, `block_closed`, and `child_exited` on the stream. No moved event, and none is needed | Gap. Needs a long-lived bridge process, because `rex.on` does not run in `init.lua` |
| Popups | `plugin pane open --placement popup --env` | Floating layer through `session.new_layer`. No `env` option | Works. Pass values in the command |
| Palette actions and keys | Manifest actions, plus `keys.command` in Herdr's `config.toml` | `rex.action` and `rex.bind` in the user's `init.lua` | Documented, not yet tested |
| Pane title and badges | `pane report-metadata` with a label for each status | One block label | Partial. Keep the `@alias` label. Rex shows agent status itself |
| Tab label | `tab rename` | `rex window rename` | Works |
| Notifications | `notification show` | No API | Gap. Minor |
| Install, update, and state directory | `herdr plugin install` and `HERDR_PLUGIN_STATE_DIR` | Nothing | Gap. Namedrop needs its own installer and state directory |
| Startup reconcile | Manifest `startup` | Nothing | The bridge process does the reconcile when it starts |
| Live peek for Channels | `terminal session observe` | `rex block stream`, not yet tested | Unknown |

## Where the cost is in Namedrop

The only seam is `HerdrCommandExecutor` in `src/ports.rs`. It passes raw `herdr` arguments and returns raw output, so each caller builds Herdr commands and parses Herdr JSON itself.

Namedrop calls these 19 `herdr` subcommands: `worktree list`, `workspace list`, `workspace get`, `pane list`, `pane process-info`, `pane read`, `pane run`, `pane send-keys`, `pane report-metadata`, `agent prompt`, `agent send-keys`, `tab get`, `tab rename`, `notification show`, `plugin pane open`, `plugin list`, `server reload-config`, `status server`, and `terminal session observe`.

- **Call sites are spread out.** The heaviest files are `src/lifecycle.rs`, `src/delivery.rs`, `src/message.rs`, `src/context.rs`, `src/health.rs`, and `src/main.rs`. Only `src/runtime.rs` and `src/pane_effects.rs` are close to thin adapters.
- **The dashboard bypasses the seam.** Files under `src/dashboard/` construct `ProcessHerdr` directly, and `src/dashboard/observer_guard.rs` spawns `herdr` with a raw `Command`.
- **Tests are the larger cost.** Fifty of the 56 files in `tests/*.rs` mention Herdr (`grep -li herdr tests/*.rs | wc -l`). They fake Herdr in two ways. Some implement `HerdrCommandExecutor` and match literal arguments. Others put a fake `herdr` script on `HERDR_BIN_PATH`. No shared fake exists.
- **Herdr names are in the wire contract.** `tool_environment` in `src/integration.rs` writes `HERDR_*` keys into every `collab/v1` delivery envelope. The CLI flags are `--herdr-socket`, `--herdr-workspace`, and `--herdr-pane`. The installed skill text and the Claude permission rules repeat them. A rename is a protocol change.
- **Herdr supplies agent facts.** Delivery authorization compares the stored agent session with the session that Herdr reports for the pane. Readiness reads Herdr's `agent_status`.
- **The store needs no migration.** Pane, tab, and terminal IDs are plain `TEXT` with no format check, so `block:<uuid>` fits. The `session_socket_id` column already separates one terminal host from another.

## Plan

Each step ends in a state that you can check before the next step starts.

1. **Build a spike in `experimental/rex/`.** Write a small program, outside `src/`, that names two Rex blocks and delivers a gated mention from a Claude Code block to a Codex block. Use it to answer the questions under [What is not yet verified](#what-is-not-yet-verified). The step is done when this file records a go or a no-go.
2. **Replace the argument-level trait with a semantic port.** Define operations such as list panes, prompt an agent, read a screen, and set a label. Move every Herdr command and every Herdr JSON shape into one Herdr adapter. Route the dashboard through the port. Give the tests one shared fake. Behavior does not change. The step is done when the existing tests pass and no `herdr` argument appears outside the adapter. This step is the largest, and it also limits the damage when Herdr changes its CLI.
3. **Add neutral names.** Accept `NAMEDROP_*` environment variables and flags beside the `HERDR_*` ones. Make the state directory independent of `HERDR_PLUGIN_STATE_DIR`. The step is done when an agent with no `HERDR_*` variables can register and reply, and when `collab/v1` readers accept both sets of keys.
4. **Write the Rex adapter.** Cover discovery, delivery, status, and labels. Take identity from `REX_BLOCK` and verify it against the process ancestors that `rex block call process` reports. Record the agent session from Namedrop's own SessionStart hook. Gate delivery on the root `program_status` record: send only on `idle` or `done`, send the paste and the Enter key on one API connection, then confirm that the state changes to `working`. The step is done when two agents in one Rex session can name each other, mention, and reply through the CLI.
5. **Write the Rex bridge.** Run one process for each Rex server that reads `rex events --all --json` and calls the existing lifecycle and drain operations. Guard it with a lock so that a second start does nothing. The step is done when a closed block loses its name, and when a queued mention drains after the recipient reports `idle`, both without a manual command.
6. **Add the UI and the installer.** Ship a Lua module that the user loads from `init.lua` with one `require` line. The module defines the palette actions and the key bindings, and it opens the naming, manage, doctor, status, and tour screens in floating layers. The step is done when a machine with Rex and no Herdr can install Namedrop, activate it, and name a pane from the palette.
7. **Add verification and documentation.** Write a Rex driver for the testbed and a Rex checklist for `/verify`. State the supported Rex API revision in `docs/compatibility.md`. The step is done when `/verify` passes on Rex.

## What is not yet verified

- What Claude Code and Codex do with a prompt that arrives in the middle of a turn, or while a permission prompt is open.
- Whether block IDs and the event stream survive a restart of the Rex app.
- Whether an action defined in `init.lua` appears in the command palette, and what context it receives.
- Whether `rex block stream` can feed live peek in Channels.
- How block addresses behave across remote Rex hosts (`rex -S`).
- How to find the `rex` binary outside a Rex terminal. Rex puts the binary on `PATH` only inside its own terminals, through `REX_BIN_DIR`.
- Which adapter wins when one pane has both `HERDR_PANE_ID` and `REX_BLOCK`, for example Herdr running inside a Rex block.

## Risks

- **Rex is a preview.** Public testing began on 2026-10-05, and builds change often. Gate on the API revision that `rex version` reports, and expect to update the adapter.
- **Delivery safety moves into Namedrop.** Herdr's `agent prompt` fails before it sends any input, so Namedrop can requeue without a duplicate. Rex accepts input at any time. A failure after the paste is ambiguous, and Namedrop must hold the message instead of sending it again.
- **Session labels are not identity.** Rex's own SessionStart hook tells the agent to rename the session. Key every space on the session ID.
- **Hook order matters for Codex.** Codex approves hooks by position in `hooks.json`. Rex appends its hook last. Namedrop must also append, or the user must approve the Rex hook again.
- **The install is manual.** With no plugin system, the user edits `init.lua` and a background process must start without a host to start it.

## Reproduce the live check

Run these commands inside a Rex terminal, from a folder that Claude Code already trusts. They start Claude Code in a scratch session, paste a prompt, submit it, and print the status changes. Rex does not show the scratch session in the app.

```sh
created=$(rex new namedrop-rex-live -c "$PWD" --json -- claude)
S=$(printf '%s' "$created" | jq -er '.session_id')
B=$(printf '%s' "$created" | jq -er '.initial_windows[0].block_ids[0]')
rex events -s "$S" --json > events.jsonl &

# Wait until this prints "idle", then continue.
rex block call -s "$S" -b "$B" program_status | jq -r '.records[0].state'

payload=$(jq -nc --arg b "$B" --arg t 'Reply with exactly the word pong.' \
	'{block_id:$b,event:{type:"paste",payload:{text:$t}}}')
rex api call -s "$S" block.event "$payload"
rex send-key -s "$S" -b "$B" enter

# After the reply appears:
grep program_status_changed events.jsonl |
	jq -c '.payload.event.payload.payload.record.state'
rex kill "$S"
kill %1
```

The last `jq` command prints `"idle"`, `"working"`, and `"done"` for Claude Code.
