# Operations and recovery

Run Herdr actions from a terminal in the intended workspace. Keep `HERDR_SOCKET_PATH`, `HERDR_WORKSPACE_ID` and `HERDR_PANE_ID` from that terminal. `sh scripts/setup.sh` refuses to choose a workspace when these are absent.

## Setup helper

The README uses Herdr's built-in plugin tabs to show setup progress and keep the result visible:

```sh
herdr plugin pane open --plugin namedrop --entrypoint setup
```

This runs `scripts/setup.sh activate --keep-open` from the installed package. It activates the workspace without rebuilding or relinking the plugin. Read the result and runtime instructions, then press Enter.

Activation also sets up Codex. It registers Namedrop's local Codex marketplace, installs or refreshes the plugin, and checks the cached copy. If everything is already current, it skips this. It doesn't approve hooks for you, so start a new Codex session and review `/hooks` if asked. Without `codex` on PATH, setup tells you how to finish later and carries on with Claude. If another marketplace is in the way, setup leaves it alone and reports it. If a command fails or setup can't check the cache, activation fails and says how to fix it. Run **Activate Namedrop** again afterwards. Running the low-level `namedrop activate` yourself only prints these Codex commands. The Herdr action runs them.

Activation installs scoped command permissions in `$CODEX_HOME/rules/namedrop.rules` (default `~/.codex/rules/namedrop.rules`), including `address list` and collaboration operations. A fresh installation creates the file, and activation fills in an existing empty file. Rules bind the installed executable and state directory, preserve unrelated policies, and load when Codex starts a new session.

Claude Code gets the same command prefixes in `permissions.allow` in `$CLAUDE_CONFIG_DIR/settings.json`, defaulting to `~/.claude/settings.json`. Agents no longer need to load the Namedrop skill to get permission to run these commands. Activation preserves other settings, key order and user entries. It records only entries it added in `claude-permissions.json` in Namedrop's state directory. Reactivation replaces stale managed prefixes. Deactivation removes only those owned entries, and only when no worktree in that Namedrop state directory remains activated. An identical rule that existed before activation remains a user rule. Claude rules are global to the selected profile, like Codex's rules file. Deleting a managed rule is temporary, because the next activation or integration refresh adds it again. Deactivate all worktrees in that state directory to opt out. Activation records pending ownership, entry changes and SHA-256 hashes in a journal before publication and recovers them on refresh. The journal does not copy other settings. A later user edit or a replay serialization mismatch discards the pending update, retains only previous ownership still present, and warns that leftover rules may need review. Activation leaves some settings files untouched and reports them without failing. This happens when the file has invalid JSON, duplicate keys at the top level or inside `permissions`, or a non-array `permissions.allow`. It also happens when the file is unsafe, such as a symlink. The installed skill grants no permissions in this case either. Activation and Doctor warn that Claude agents get permission prompts until you fix the settings and reactivate Namedrop. Fix the reported settings conflict, then run Activate Namedrop again. Doctor reports missing or stale rules and gives the exact activation command to repair them. Loading the skill cannot repair permissions.

Two limits apply. Namedrop selects the config directory from its own process environment, not the Claude pane's environment. Set `CLAUDE_CONFIG_DIR` consistently in both. Activation rows left by dead sessions keep the rules installed until those rows are purged.

To run setup inline in the current terminal instead, locate the installed package and run the same helper:

```sh
cd "$(herdr plugin list --plugin namedrop --json | python3 -c 'import json,sys; print(json.load(sys.stdin)["result"]["plugins"][0]["plugin_root"])')"
sh scripts/setup.sh activate
```

The helper waits for the action to finish and checks its result. Then open a new terminal tab inside Herdr and run `namedrop doctor`. Running `herdr plugin action invoke activate --plugin namedrop` directly only tells you Herdr started the action, not that setup finished.

## Shell command

Activation installs `~/.local/bin/namedrop`, a launcher for the stable runtime command and its state directory. It preserves the calling terminal's Herdr workspace and pane. Reactivation refreshes the launcher when you upgrade or switch installations. Agent permission rules use the exact stable runtime command path.

For zsh, activation adds an idempotent PATH entry to `.zshrc` (honoring `ZDOTDIR` inside your home directory). For bash, it updates `.bashrc` and the existing login profile, choosing `.profile` if none exists. Activation preserves existing settings. Open a new terminal, or run `export PATH="$HOME/.local/bin:$PATH"` in the current one. For other shells or a `ZDOTDIR` outside your home directory, add `~/.local/bin` to PATH in your shell configuration. Activation reports that manual step.

An unrelated `~/.local/bin/namedrop` or a symlinked install or startup path stops command installation before activation changes those files. Move the conflicting command or use a regular startup file, then activate again. Activation reports the launcher path and the exact startup files it configured.

## Command output

Naming, activation, health checks, name and group management, and space and thread controls show readable results when stdout is a terminal. Lists show names and targets. Confirmations omit empty delivery records and unrelated setup details. Warnings and required actions remain visible. Errors use stderr and keep their original exit codes.

```sh
namedrop address list
namedrop group list
namedrop doctor
namedrop address list --json
namedrop doctor --human > health.txt 2>&1
sh scripts/setup.sh check
sh scripts/setup.sh check --json
```

Direct CLI commands keep JSON when stdout is redirected or piped. `--json` forces the full `collab/v1` response even in a terminal. `--human` forces readable output when capturing it. These flags choose output. `--input-json` and `--input-json-file` still supply command input.

Activation exits with `1` if it couldn't install a shared agent integration, even when the workspace itself was activated. The report keeps each package's error and setup steps. Fix those before you retry. Exit `0` doesn't mean the agents have loaded the hooks.

Commands that agents use (`describe`, registration, messaging, inbox, handoffs and context) print JSON by default, even in a terminal. Hooks and terminal execution have their own formats. Scripts that run inside a terminal should pass `--json` explicitly.

The setup helper prints readable output even when captured. With `--json`, it prints the raw action response on stdout and progress on stderr. The checks and exit status are the same either way. Formatting never runs a repair or repeats an action. The setup tab passes `--keep-open` so it waits for Enter. The helper waits up to 60 seconds for the action. If it times out, the action may still be running, so read the log it names before trying again.

## Popups

**Prefix**, then **Shift-H** (`prefix+shift+h`, action `namedrop.doctor`) opens **Namedrop Doctor** with the workspace's health and repair steps. `>` and `<`, or the arrow keys, page. `R` (or `r`) refreshes, and Enter, Esc or `q` closes. Keys act without Enter. The panel never repairs anything itself. It sends something only when you press `A`, described below. While the workspace has no names, `T` (or `t`) pages the [tour](#tour-presentations) inside the panel, **N** on its first page names the pane, and Esc returns to the diagnosis. Reading the tour this way doesn't mark it as shown.

When Doctor lists issues, `A` (or `a`), **Ask agent to help**, sends them to the agent in the pane that opened the panel, named or not, without asking again. It prompts only the session that was in that pane when the panel opened, so it never prompts a replacement session. It doesn't add a mention name. Success means only that Herdr accepted the prompt. The agent may not have read it yet, nothing is fixed, and it isn't a Namedrop delivery. Go back to that pane to see what the agent did and what it needs from you. Some issues need your decision. If the pane has no supported agent, its session changed, or Herdr refuses the prompt, nothing is sent and the panel shows why. If the pane itself changed, the panel closes and asks you to open it again. For scripts, use `namedrop doctor` or the background `check` action. `sh scripts/setup.sh check` waits for that action's result.

**Prefix**, then **Shift-I** (`prefix+shift+i`, action `namedrop.name-agent`) opens **Name this pane**. Type a mention name, then press Enter to accept the suggested terminal or agent type, or type `t` or `a` to change it. Esc at either field closes without saving. The popup names the pane that opened it, not itself. If that pane was closed or replaced, the popup refuses to name it. Errors stay on screen until you dismiss them.

**Prefix**, then **Shift-M** (`prefix+shift+m`, action `namedrop.manage-addresses`) opens **Manage names** for the current space. Use **↑** and **↓** to select a name and **Enter** to manage it: rename, edit the persona of an agent name, attach or detach, enable or disable delivery, or remove. Menu letters act immediately. Name fields and yes-or-no confirmations require Enter. Attach, detach and removal ask for a yes-or-no confirmation, and Enter defaults to No. Removing a name keeps its history and refuses while deliveries remain unfinished. These actions leave panes and agent sessions open. **N** names the invoking pane, **T** pages the [tour](#tour-presentations) while the space has no names, **R** refreshes the list, and **Esc** closes or goes back. Enter also goes back from the action menu. Use **<** and **>** to page through longer lists. If the shortcut is not configured, run `herdr plugin action invoke manage-addresses --plugin namedrop`.

To open the same dialog from the shell, run `herdr plugin action invoke name-agent --plugin namedrop`.

## Terminal panes

Run the installed command from the pane being named:

```sh
namedrop name @build
namedrop name @logs --type terminal
namedrop name @reviewer --type agent
```

In a terminal, naming asks `terminal / agent`, and Enter accepts the detected type. That is terminal for a shell, or agent for a detected agent even if its session hasn't registered yet. If Namedrop can't tell what's in the pane, you have to choose. Renaming keeps the type, and both types follow the same naming rules. Names saved before terminal support stay agents after an upgrade. Run `name <alias> --type terminal` to convert one. Changing the type waits until the name's pending deliveries finish or are cancelled.

Send `@build cargo test` from an agent, or use `send` with that source. A terminal runs the message body as a shell command. It takes new requests only, without context attachments, and never runs replies or result reports. The shell must be available when the command is dispatched, and nothing runs if an agent has started in the pane or the terminal was replaced. Each request runs once, as `/bin/sh -c` in the pane's current directory and environment. Each command gets its own child shell, so `cd`, aliases, functions and shell variables don't carry over. Put related commands in one request, such as `cd project && make test`.

Output shows in the pane. The result sent back has the exit code and up to 32 KiB each of stdout and stderr, marked when truncated, with recognized secrets redacted. A nonzero exit is a finished command, and Namedrop doesn't retry it. A group send waits for all its results. If the helper is interrupted, or Namedrop can't tell whether the command started, the attempt is held and later commands wait until you review it. Pausing or cancelling doesn't kill a running process. Saved results show in `handoff.results`. If a delivery wake fails, run `reconcile` to recover them.

When a terminal sends a request, Namedrop saves the reply but doesn't type it into that terminal's shell. Read the output in the target pane. Agents get the normal result. Once a newer release has migrated the state, older releases can't read it, so keep the executable and state together (see [schema and migrations](#schema-and-migrations)).

## Personas and automatic collaborator selection

Each named agent can have one optional persona: a short description of what it's good at. An agent without one is **Generic**. It has no specialty, but coordinators still pick it for ordinary work. The persona belongs to the name in that workspace, so it survives renames, rebinding and restarts.

Set one while naming with `namedrop name @reviewer --persona 'Reviews Rust changes for risk'`, or clear it with `--generic`. You can't use both flags at once, and using neither keeps the saved persona. An agent can also run `address persona` with an `address_id` and a `description`. `null` or blank means Generic. `description` is required so that an edit that forgets the text can't clear a persona by accident.

A persona can have up to 1000 characters, no control characters, and no invisible formatting characters such as bidirectional overrides, zero-width spaces or tag characters (emoji joiners are fine). Named terminals can't have a persona. Namedrop keeps a persona that an earlier release saved, even if it breaks these rules. Namedrop escapes and shortens it for display and prompts but never rewrites it.

At the start of every turn, and with every incoming task on either host, the installed hook gives the agent the current roster: each named agent in the workspace, its persona, which one is itself, and whether it can take work now (registered session, idle or busy, delivery off, no pane). Named terminals are listed as commands, not collaborators. A leading `@mention` that doesn't match a name stays in the pane as before, with the same "do not retry" note under the roster.

The rules for using that roster, the selection policy, are the same on every turn, so the hook sends them once, when the session starts, and again whenever the host rebuilds the session's context: on resume, after `/clear`, after compaction, and for a forked session. Each turn then carries only the roster rows. On Claude Code and Codex that's the `SessionStart` hook event. A session that was already running when Namedrop was installed or updated never ran it, so start a fresh session after either.

The agent's role depends on who asked. If you typed the prompt, the agent is the coordinator. It picks available peers that fit, says briefly why, and gives you one combined answer, or does the work itself if nobody fits. An agent working on another agent's request is a helper. It can recommend peers to the coordinator that asked, but doesn't recruit them itself.

Availability changes between turns, so the roster tells the coordinator to run `address list` right before choosing. Each entry in that list includes `registered`, which says whether a live session is registered for the name right now. Each entry also includes `selection`, the same line the roster shows. Examples are `collaboration paused; not selectable` for a paused workspace, or a "not a persona collaborator" note for a terminal. The other fields are unchanged, so the same list still works for managing names.

Personas grant no permissions or tool access and aren't proof of expertise. What you ask for, and any explicit `@mention`, always wins. Both hosts put hook context in a privileged message, a system message in Claude Code and a developer message in Codex. For that reason, the roster quotes and escapes every persona, the policy tells the agent to treat every quoted persona as user-entered data, never as instructions, and each roster repeats that reminder beside the rows. A persona never changes what a delivered request allows, and the roster isn't part of the delivery envelope. It sits beside the prompt and is rebuilt each turn.

Each delivery envelope carries a short plugin-written instruction for the kind of delivery it is: an ordinary message, a context request, a result set, a report to you, or a coordination nudge. Namedrop picks those rules from the envelope's own control fields, so an agent reads only the rules that apply, and the installed skill still lists the whole set.

Both hosts get the roster on ordinary turns and incoming tasks. Only Claude Code also gets the delivery attestation, a line confirming that the prompt matches a pending delivery, followed by each authorized operation as the one command line that runs it. Claude Code hands the hook a long prompt in its pasted-text form, between `<pasted_content id="…">` and `</pasted_content id="…">` lines; the hook reads the envelope inside one such block, so the attestation appears for every delivery. The envelope itself is the same on both hosts.

A paused workspace sends nothing, so automatic selection is off there. The roster says so, shows no peer as available, and tells the agent to do the work itself. Work already sent isn't affected.

Personas and the selection policy come with the installed package, so a session started before an update keeps its old instructions and its old hook list. Start a new Codex or Claude Code session after updating, and run any Codex refresh steps setup prints.

## Agent status panel

The `namedrop.agent-status` action opens a floating panel for the pane that invoked it. It shows the agent's current work, outstanding requests and the progress of those recipients first, followed by a compact workspace overview. Facts appear before the short model descriptions. **R** refreshes, **↑** and **↓** scroll, and **Esc** closes. None of these keys needs Enter.

During activation, normal setup and updates configure **Prefix**, then **Shift-S** (`prefix+shift+s`) for this panel. They also configure Name this pane (`prefix+shift+i`), Manage names (`prefix+shift+m`), Namedrop Doctor (`prefix+shift+h`) and Tour (`prefix+shift+o`), then reload Herdr's configuration. Doctor avoids `prefix+shift+d`, which is Herdr's default for closing a workspace. Tour avoids `prefix+shift+t`, which renames a tab. No separate command or restart is needed. Setup preserves an existing binding for each action, including a custom key. If a key is already bound to anything else, setup leaves it untouched, reports it, and still adds the remaining shortcuts. That action stays available without a shortcut. Setup also reports configuration errors and reload failures, and reopening setup retries them.

Activation uses `HERDR_CONFIG_PATH` when set, then `$XDG_CONFIG_HOME/herdr/config.toml` when `XDG_CONFIG_HOME` is an absolute path, as Herdr does, otherwise `~/.config/herdr/config.toml`. It creates a missing configuration and backs up an existing file before changing it. Activation refuses symlinked configuration files. For manual repair or a different configuration file, run `namedrop ui install-shortcuts --config PATH`, then run `herdr server reload-config`. The install command exits `1` if a key needs attention.

To open the panel directly:

```sh
herdr plugin action invoke agent-status --plugin namedrop
```

The panel reads recent output from panes in the workspace and asks the invoking agent's host to describe it. Claude Code uses `claude-haiku-4-5`, and Codex uses `gpt-5.6-luna`. These requests run separately from your session and use your existing sign-in. Codex runs sandboxed. On macOS it uses `sandbox-exec`. On Linux it uses Landlock (ABI 1 or newer) and works only with a static native Codex binary run directly rather than through Node, such as the musl binary in the official `@openai/codex` npm package. Without the sandbox, the request fails with an error saying why, and Doctor explains it too. A clean Doctor doesn't prove the sandbox works. The panel waits for a description until it arrives or you close the panel. A slow response alone doesn't fail it. Until then it shows "Preparing summary…" placeholders next to agent states and outstanding requests, never raw terminal output. If the request errors, returns invalid output or too much output, the panel shows labeled recent output and the reason. Descriptions don't affect delivery state and don't prove an agent is stuck.

The panel reuses a description for up to 30 seconds if nothing changed, and a new terminal or agent session clears it. Closing the panel cancels any request still running. Refreshing during a request waits for that request instead of starting another. A request that stays pending may be stuck at the provider, even if its process is running. Close the panel to cancel it. Reading panes and checking sessions have short time limits of their own. If Namedrop can't read its state, the panel shows outstanding requests as unknown. Before a checkout replaces an active installation, its collaboration schema must match the installed one.

## Tour presentations

Tour presentations live in `tours/<id>.md`, and `src/tour.rs` compiles them into the binary with `include_str!`. The header contains `id: <id>` and `title: <title>`, followed by a line containing only `---`. The id must match `^[a-z0-9][a-z0-9-]{0,63}$`, and titles must be non-empty. The parser rejects unknown header keys.

Pages are separated by `---` lines, and each starts with `# <title>`. An optional `action: name` line offers naming the current pane. The tour removes that line from the displayed body. No other action is supported. For example:

```text
id: welcome-1
title: Welcome to Namedrop
---
# Name this pane
Give this pane a name so other agents can mention it.
action: name
---
# Mention an agent
Use @name in a prompt to ask a named agent for help.
```

Bodies wrap at 68 columns, preserving paragraph breaks. A page body may not exceed 16 wrapped lines. The parser rejects an oversized page, and `cargo test --test tour` catches it. Keep each page short enough to fit the popup.

`CURRENT_ID` in `src/tour.rs` selects the presentation that shows automatically. To ship a new one for a major update, add a file with a new id, include it in `src/tour.rs`, and point `CURRENT_ID` at it. Namedrop shows it once per machine when it first sees an agent pane in an activated space. The marker is `<state directory>/tours-shown/<id>` and survives updates and reinstalls. Keep ids stable, because a downgrade never reshows an older id whose marker is present. `namedrop ui reset-tour` clears the current presentation's marker so it can show again.

Activation and each agent-detection event report a `tour` object. When the popup opens, `shown` is `true` and `pane_id` names the pane it opened for. Otherwise `shown` is `false`, and `reason` is `no_agent_pane`, `already_shown`, `presentation_error`, `marker_error` or `open_failed`. The last three also carry an `error`. Namedrop claims the marker before the popup opens, so two agents detected at the same moment open one tour. A popup that fails to open keeps the marker rather than retrying on every later event. Reopen it with the shortcut, or clear the marker with `namedrop ui reset-tour`.

The `tour` action runs `namedrop ui launch tour` to open the `tour` popup pane, which runs `namedrop ui run tour`. Readers can reopen it with **Prefix**, then **Shift-O**, or with `herdr plugin action invoke tour --plugin namedrop`, even when its marker is present. Opening it that way records the marker too. While a space has no names, **Manage names** and **Doctor** page the same presentation in place with **T** without recording it. A page with `action: name` shows **N**, which runs the ordinary Name this pane prompt for the pane the tour opened over and then returns to that page.

## When something does not work

```sh
sh scripts/setup.sh check
herdr integration status
herdr plugin log list --plugin namedrop --limit 10
```

The helper waits for the check to finish and prints the report. It exits with Doctor's code: `0` healthy, `1` needs attention, `2` unavailable. It also returns `2` if the action log is lost or unreadable, or the wait times out, and names the log when it can. Read that log before retrying, because the action may still be running. `herdr plugin action invoke` on its own only confirms the action was scheduled, so its exit code says nothing about health. Installation returns `1` if any runtime package failed and keeps the full setup output. When you report a problem, include the issue codes, source IDs, causes and suggested commands. Doctor only reads, and it never runs a repair for you.

| Symptom | Check and remedy |
|---|---|
| Naming returns a missing or null session or `registered:false` | A new Codex session can show its composer before running `SessionStart`. Submit the first prompt in that pane, then check registration. Queued mentions wait until the session registers. If it's still missing after a turn, check `python3 --version`, install or update the native integration with `herdr integration install codex` or `herdr integration install claude`, restart and approve its hooks. Seeing "Codex" or "Claude" in the pane isn't enough. The session has to register. If Doctor reports ND-SESSION-003, follow it instead: a second prompt or reinstall will not help. |
| Doctor reports ND-SESSION-003, mentions to a Codex agent stay queued, or one Codex pane shows another's session | Codex 0.162 and later runs its sessions through a shared app-server that keeps the environment of wherever it started, so Herdr cannot tell which pane a session is in. Run `codex features disable daemon_auto_start`, then `codex app-server daemon stop`, then restart Codex in each of its Herdr panes. The stop disconnects every open Codex window that uses the server. While the server is off, the `codex agents` session browser and Codex remote control are unavailable. To fix one session only, start it with `codex --no-daemon`. To undo the fix, run `codex features enable daemon_auto_start`. Namedrop never changes the setting for you. See [the shared app-server](compatibility.md#codex-0162-and-later-the-shared-app-server). |
| Codex ignores a leading mention | Run activation's marketplace and plugin steps, start a new session, then review both Namedrop and Herdr in `/hooks`. New or changed hooks require fresh trust. Check that an older collaboration plugin is not also handling prompts. |
| A setup, naming or handoff command asks permission | Approve only that exact intended command. Name management is deliberately outside automatic collaboration permissions. Runtime policy may also request approval for bound handoff commands even when local permission checks allow them. Unattended operation is not guaranteed. |
| Permission denied during a handoff | Use the bound command exactly, with `--plugin-state` before the operation and explicit context arguments. Restart after permission-file changes. Report restrictive policy. Do not install broad shell permissions or retry indefinitely. |
| `HERDR_PANE_ID=… was inherited from the environment, but this process … is not running inside pane …`, or `could not verify the inherited HERDR_PANE_ID=…` | The command took its pane from an inherited `HERDR_PANE_ID`. The calling process neither runs under that pane's shell nor belongs to an agent session that Herdr reports in that pane. Namedrop refused rather than act as the wrong agent. A background agent host such as Claude Code's `bg-pty-host` keeps the variables of the pane where it first started, even when another pane drives its conversation. A shell under tmux or another multiplexer is not a descendant of the pane's shell either. A Claude Code or Codex command whose `CLAUDE_CODE_SESSION_ID` or `CODEX_SESSION_ID` names the session running in the inherited pane is accepted even outside the shell's process tree. Pass `--herdr-pane <pane-id>` naming the pane the command acts for, or run the command from a shell or agent inside that pane. When the message carries Herdr's own error, such as a timeout or `host unavailable`, restore the host connection first. Actions, popups and events that Herdr starts pass the pane explicitly and are not affected. The same holds for the terminal runner that Namedrop dispatches through `pane run`. A hook that runs outside its pane's process tree does nothing instead of attesting a delivery or registering the wrong pane. The one exception is a Codex session whose hooks run inside Codex's shared app-server: its session-start hook shows the ND-SESSION-003 notice. |
| Ownership conflict installing a skill or rules file | Inspect the reported exact path and owner. Move a genuinely obsolete conflicting artifact to a backup location, then reactivate. Do not delete a shared skill directory or unrelated rules. |
| "Not connected" after a restart | Confirm the new native Agent Session ID, load runtime integrations, then ask the agent to name or reconnect itself. Doctor compares live sessions rather than trusting an old registration. |
| Messages wait while paused or busy | Wait for the recipient to finish, or resume the named agent or workspace. A queued message isn't done yet. |
| A delivery is held or uncertain | Look at its `attempt_id` and the reason Doctor gives. You can run the report's `delivery cancel-held` command with that attempt and `confirmed:true`. Don't resend it, because the work may already have run. Cancelling doesn't stop or undo work. |
| An agent response ended without a clear result | Doctor shows ND-DELIVERY-003 with the attempt ID as `source_id`, and its explanation says what happened. An untracked message is flagged as soon as it settles without a reply or no-reply. A tracked request is flagged only when its work ended without the worker's answer, because the coordination was resolved or the worker's exact session or name is gone. A request its open coordination still awaits is not flagged; the lead's nudge covers it. A request the lead superseded with a follow-up is never flagged, because the follow-up's own delivery carries the obligation. The worker's exact session can still answer or complete it from `delivery.inbox`. Once that agent is retired, or its exact session was replaced on a live name, and you've reviewed it, you can run `delivery acknowledge` with that attempt and `confirmed:true`. It clears only the warning, records no disposition and resends nothing. It's refused while no newer session has been seen on that name, because the exact session can still record the disposition itself. A coordination nudge never raises this warning. It settles as no-reply on its own. |
| A handoff warns that its lead is retired | Review its saved report as described in [Recover a report after owner loss](#recover-a-report-after-owner-loss). You can run `handoff acknowledge` with its coordination ID and `confirmed:true` to clear the warning. The outcome and deliveries don't change. It's refused while linked deliveries are unfinished. If the lead's address is still live and a new session is registered on it, that session adopts the handoff instead. See [Adopt a handoff after a lead restart](#adopt-a-handoff-after-a-lead-restart). |
| A completed handoff keeps warning after you cancelled its held report delivery | Doctor shows ND-COORDINATION-001 saying the saved report's delivery is cancelled. Read the report with `handoff results`, then run `handoff acknowledge` with its coordination ID and `confirmed:true`. It clears only the warning. The outcome, the report and the cancelled delivery stay as they are. It's refused while the report delivery is queued, in flight or held. |
| A delivery expired before dispatch | Look at the reported `delivery_id` and why it expired. You can run `delivery cancel-held` with that delivery ID and `confirmed:true` to free the queue. It refuses attempts that may already have started. Use the attempt ID for those. Pass either `delivery_id` or `attempt_id`, not both. Cancelling never resends the request. |
| A thread reached its human checkpoint | Review the thread identified by the diagnostic `source_id`, then explicitly run the proposed `thread continue` action if appropriate. |
| A delivery prompt was lost | The registered recipient may use `delivery inbox` to recover envelopes owed by that exact session. A replacement session cannot settle the previous session's work. |
| `handoff` is unknown or the new reads are unavailable | Refresh the installed package and runtime integrations using [Upgrade](#upgrade), then start fresh registered sessions. A rebuilt checkout alone does not update an installed binary or loaded skill. |
| Doctor says Namedrop is still running the previous build | Doctor shows ND-SETUP-004 when the plugin root's `bin/namedrop` differs from the release that `runtime/current` runs. A bare `herdr plugin install` rebuilds `bin/namedrop` but does not switch to it. Run the issue's repair command, `sh <plugin_root>/scripts/setup.sh activate --human`, in the Herdr workspace. `diagnostic.path` names the new build. On a GitHub install, Update Namedrop builds and switches in one step, so use it instead of a bare reinstall. |
| A handoff is "not owned by this exact Agent Session" | Run the read from the session that sent the request. A new session on the same lead address can take over an open coordination with `handoff adopt`, which covers its results by `coordination_id`, new assignments, continuations and resolve. A read keyed by a `message_id` the old session sent stays with that session. Renaming, or a session on another address, cannot take it over. Use `doctor` for broader diagnostics after a restart. |
| A restarted lead's handoff belongs to another session of this address | The lead's agent session restarted in the same pane and the coordination still records the old session. From the new session, run `handoff status`, then `handoff adopt` with each `coordination_id` listed under `adoptable`. Adoption cancels worker replies still queued for the old session and the next drain retires its queued nudge. A nudge or reply already dispatched or held for it stays as it is and needs `delivery cancel-held` with `confirmed:true`. |
| A group cannot be saved or sent | List groups and agents. Use a free group name and 1 to 64 existing agent aliases. Replace unavailable members. Namedrop rejects nested groups and names reserved by agents. An unresolved member prevents the whole new request from routing. |
| A send fails with "addressed only to its author" | Every leading mention, after group expansion, was the sender. Mention at least one other agent. A message never goes to its author. |
| A tracked reply is refused because it "also addresses" another agent | A tracked reply goes only to the lead. Start the source with the lead's mention alone, and write any other name without the `@`. |
| A review's freshness is `unknown` | Inspect `reason` or `unavailable_paths`. Send a new review with explicit, allowed `diff` paths and Git capture enabled when evidence was absent or never pinned. Use a smaller evidence scope for oversized Git metadata. The per-file byte cap still applies. `checkout_unavailable` means the reviewed checkout was moved, removed or replaced. `checkout_reason` says which. Send a new review from the checkout's current location. |
| A `checkout.path` is refused | Pass the absolute root of a worktree that `git worktree list` reports for the workspace's repository, or a symlink to it. The workspace itself must be attached to a Git worktree. |
| Context read denied | Confirm the read comes from the recipient's pane after its current session registered, and that the sender's session is still live. Shared context belongs to the recipient's named pane, not to one run of its agent. After a restart, the agent can read nothing until its new session registers, and then it can read context shared with that pane earlier. A share ends when it's revoked or the sender's session restarts. Reads are limited to granted evidence. Credential directories, symlink aliases and historical credential paths remain protected. Only an exact human file reveal can override path blocking. |
| Evidence exceeds `1048576 bytes` or line bounds | Select a smaller file or narrower Git path and request 1 to 200 lines. The byte cap applies before pagination. Namedrop rejects huge files even when you request only one line. |
| Doctor is unavailable | Its diagnostic cause distinguishes host connection, state access and schema problems. Check the socket and reported state path. Preserve the database and logs before repair. |

### Local return replies and automatic approval

Activation installs permissions for the same 18 collaboration command prefixes on both hosts. They cover the operation catalog plus management operations marked `automatic_permission`, which is currently `address list`. Each prefix pins the installed executable and plugin-state path. Codex gets `rules/namedrop.rules`, which it loads in the next session. Claude gets the same 18 rules in managed settings. Both hosts register with the installed prefix followed by `register --herdr-socket <socket> --herdr-workspace <workspace> --herdr-pane <pane> --input-json <json>`. The existing managed `register` rule covers the challenge without environment assignments or permission wildcards for environment values. The installed skill grants no permissions. Reactivation or an integration refresh during an update rewrites older skills without `allowed-tools`, removes any owned environment-prefixed bootstrap rule from earlier builds, and preserves user-owned entries. If Doctor reports missing or stale permissions, repair any settings conflict and reactivate Namedrop. Name management, `delivery.cancel-held` and unrelated shell commands remain outside these permissions. Claude installation refuses paths containing `*` because they would become permission wildcards.

Namedrop only talks locally, through a Unix socket and a local state directory. A reply goes back to whoever asked, in the same Herdr workspace. That doesn't mean an agent may put anything in the reply. Your sharing rules still apply. Sending to new recipients, other workspaces or anywhere outside needs your permission.

If you're happy for agents to send replies back within the workspace, including private research, say so in your agent configuration. For Claude, [auto-mode configuration](https://code.claude.com/docs/en/auto-mode-config) supports `autoMode.environment` for the local service and `autoMode.allow` for the scope you allow, in `~/.claude/settings.json`. Keep `"$defaults"` in both arrays. Current Claude versions don't read these fields from project settings. Check with `claude auto-mode config`. For Codex, write the allowed scope in `~/.codex/AGENTS.md` and start a new session. Namedrop activation never changes either setting for you.

For long replies, agents write the input to a file in a separate step and pass `--input-json-file`. JSON parse and schema errors from file inputs omit their contents, including unknown field names and invalid values. Operational errors, such as unknown recipients, request ID conflicts and approval refusals, retain their diagnostics. Wrapping the call in `export`, command substitution or a pipeline hides the exact command and destination from permission review.

If auto review still blocks a reply, keep the report file and the pending reply, and look at the reason. A settled delivery doesn't prove the reply was sent. Check the reply's outcome and what the requester received. Don't get around a content-policy block with broad Bash allow rules or by relaying the text by hand.

Doctor prints IDs, paths, repair inputs and setup commands exactly, even when they look like secrets, so you can copy them. Its explanations still redact recognized secrets, and control characters are escaped. Check the report before sharing it, because the IDs and paths aren't scrubbed.

For a bug report, include the Namedrop version or commit, the OS and architecture, `herdr --version`, `codex --version` or `claude --version`, the relevant health issue, the commands, and the observed and expected results. Remove sensitive values yourself before posting. Do not share tokens, full credentials, or private collaboration bodies.

## Handoffs and groups

Agents run these commands, but you can run them yourself. Use the executable and state paths from the installed Namedrop skill. `describe <operation>` prints each operation's input schema and an example, including `group.set` and `group.remove`, which agents only run when you ask.

Operation names use dots in `describe` (`handoff.results`) and spaces on the command line (`handoff results`). A full results command, run from the requesting agent's Herdr terminal, looks like this. Fill in the installed paths and the message ID that `send` or `handoff status` returned:

```sh
"/absolute/path/to/namedrop" --plugin-state "/absolute/path/to/state" \
  handoff results \
  --herdr-socket "$HERDR_SOCKET_PATH" \
  --herdr-workspace "$HERDR_WORKSPACE_ID" \
  --herdr-pane "$HERDR_PANE_ID" \
  --input-json '{"message_id":"<request message_id>"}'
```

For longer input, pass `--input-json-file /absolute/path/to/input.json` instead. The result is under `data`. Check the outer `status` first. Commands included in a delivered request already carry the right arguments, and you must run them exactly as given.

| Command suffix | Input JSON | Result |
|---|---|---|
| `handoff status` | `{}` | This exact registered session's outstanding outgoing requests, recipient states and elapsed time. |
| `handoff status` | `{"include_completed":true,"limit":50,"offset":0}` | Include finished requests. Follow `next_offset` while `has_more` is true. |
| `handoff results` | `{"message_id":"<request id>"}` | Completed and expected counts, pending recipients, available replies and review freshness. |
| `handoff adopt` | `{"request_id":"adopt-1","coordination_id":"<coordination id>"}` | Transfers an open coordination from an earlier session of this same address to this session. Returns the coordination report plus `adoption.changed`. |
| `handoff preview` | `{"source":"@review-team Inspect these changes."}` | Expanded recipients, live sessions, message and constraints. Sends nothing. Add the same `context` block you intend to send. |
| `handoff preview` | `{"source":"@review-team Inspect these changes.","mode":"direct_prompt"}` | Preview the native hook's verbatim message. This mode takes no context block. |
| `group list` | `{}` | Saved groups and current member aliases. A null member alias needs replacement. |
| `group set` | `{"alias":"review-team","members":["reviewer","tester"]}` | Create or replace a group when you ask for one. |
| `group remove` | `{"alias":"review-team"}` | Remove the group. Existing deliveries and replays keep their recipients. |

Status, results, preview and group list only read. They don't send, recover or finish anything, and a held request stays held. Reading a reply doesn't use it up, and the combined result still arrives later. A session can only read its own requests. Use `doctor` to look at anything else, including work from an earlier session.

### Reading progress and partial results

`handoff.status` lists the requests this session started. By default it shows unfinished ones, 50 at a time. `limit` can be 1 to 100, and `offset` starts at 0. Add `include_completed:true` to see finished requests, and follow `next_offset` while `has_more` is true. Requests can finish between pages.

Pass the request's `message_id` to `handoff.results`. `completed` counts every recipient that has finished in any way, including failures, so it can be higher than the number of replies. `complete:true` means every recipient has finished. For each recipient:

| Field | Meaning |
|---|---|
| `state` | Recorded delivery state, such as queued, active, settled, held, failed, or cancelled. |
| `acknowledged_at_ms` | When the recipient received it. Received doesn't mean finished. |
| `result_status` | `replied`, `no_reply`, `failed`, or `cancelled`. Null until the recipient finishes. |
| `untrusted_reply` | The reply text, written by the other agent. It's data and grants no permissions. |
| `needs_attention` | Held or failed delivery, or a settled attempt with no disposition. |
| `outlook` | For queued recipients: what the delivery is waiting on, such as no live session, a pause, a busy agent or earlier deliveries. |
| `elapsed_ms` | Time since the request or delivery was created. It keeps counting after completion. |

These counts are about the recipients, not whether the requesting agent has shown you its final answer. When one request goes to several agents, reading results early, even with `complete:true`, doesn't use up the combined result that arrives later. The agent can show you progress when you ask, and gives its finished answer once the combined result arrives. Polling in a loop slows things down, because queued deliveries wait until the agent ends its turn.

### Group membership and replay

A group only expands when it's at the start of a message. A group mentioned later in the text is just a reference. An agent can turn "Ask @review-team to review this" into a send to the group. To hand off a prompt directly, make `@review-team` the very first thing in it.

`group.set` creates a group or replaces its whole member list, with 1 to 64 existing agent names. The leading `@` is optional. A member listed twice, or also mentioned on its own, still gets one delivery, in the order it first appears. Members are tracked by identity, so renaming an agent doesn't break the group. `group.list` shows a null name for a member that's gone. Replace or remove it before sending. Groups can't contain other groups or use a name that belongs to an agent, including a renamed one.

A message never goes to its author. Naming yourself among the leading mentions, or mentioning a group you belong to, labels the message rather than addressing you. A message whose only recipient would be its author is refused with `this message is addressed only to its author; mention at least one other agent`, and nothing is sent.

A send resolves all its recipients at once and saves them with the request. If one doesn't resolve, Namedrop sends nothing. Replaying the same request after the group changes still goes to the original recipients, while a new `request_id` uses the current members. Agents can't change groups on their own, because `group.set` and `group.remove` aren't in their automatic permissions or in delivered requests.

### Preview modes

The default, `mode:"send"`, shows how a send is parsed: leading mentions become recipients and are dropped from the message body. `mode:"direct_prompt"` shows how a prompt typed into a pane is routed, with the whole prompt kept as the message. It takes no context block. Preview fails on unknown recipients or context references, or a message addressed only to its author, and sends nothing. Its recipients never include the author.

The preview shows `authored_source`, `message`, the resolved recipients and their live sessions, context references, the redacted context that would be sent, and anything that would hold the delivery. It reflects this moment only. Namedrop checks the session again when it actually sends the message. Preview reserves nothing, grants no access and creates no Git objects or refs. Send separately when you're ready.

### Review context and freshness

To make a review checkable, attach a `context.snapshot` when you send it and list the files to review, relative to the worktree, as `diff` evidence. This `send` input is an example. Fill the snapshot with what the reviewer needs:

```json
{
  "request_id": "review-retry-001",
  "source": "@reviewer Review the retry logic and report correctness issues.",
  "author_kind": "agent_session",
  "context": {
    "capture_git_state": true,
    "snapshot": {
      "objective": "Review the retry logic",
      "applicable_user_instructions": [],
      "decisions": [],
      "completed_work": [],
      "current_work": ["Retry changes are ready for review"],
      "blockers": [],
      "evidence": [{"kind": "diff", "locator": "src/retry.rs"}]
    }
  }
}
```

Attaching context captures Git state by default, which needs a commit at `HEAD`. Git capture and file, diff or commit evidence only work when the workspace is attached to a Git worktree. Namedrop never falls back to the directory the command runs in. Without a worktree, it refuses before touching any file or Git object. With `capture_git_state: false`, snapshots with only text or pane output still work without a worktree. The pin records the whole worktree: tracked changes and untracked files that aren't ignored. Capture runs the repository's clean filters, as `git add` does, so tools such as Git LFS and git-crypt store what they normally store. It never runs filesystem monitor hooks. The evidence paths limit what the reviewer can read and what the freshness check compares. A plain direct prompt attaches no context. To preview this example, pass just its `source` and `context` to `handoff.preview`.

#### Share context from another worktree

By default, context comes from the worktree attached to the workspace. To share context from another worktree of the same repository, such as a worker's checkout, add `checkout` next to `snapshot` in the `context` block:

```json
"context": {
  "checkout": {"path": "/home/me/work/retry-worker"},
  "snapshot": {
    "objective": "Review the retry logic",
    "applicable_user_instructions": [],
    "decisions": [],
    "completed_work": [],
    "current_work": [],
    "blockers": [],
    "evidence": [{"kind": "diff", "locator": "src/retry.rs"}]
  }
}
```

`snapshot.publish` takes the same `checkout` at the top level, next to `snapshot`. The path must be the absolute root of a worktree that `git worktree list` reports for the workspace's repository, or a symlink to that root. Namedrop refuses a relative path, a subdirectory, a bare repository, an unregistered copy, a checkout of another repository, and any `checkout` in a workspace without a worktree. It refuses before it pins or shares anything. It never picks a checkout from the directory a command runs in.

Namedrop records the checkout with the snapshot and uses it for every later read and freshness check, even after the workspace is reattached elsewhere. The snapshot read, `context.inspect` and `handoff.results` show it as `checkout`, or `reviewed_checkout` in results. That field holds the checkout's `root`, `label`, `layout`, and `selection`, which is `explicit` or `space_default`. With `capture_git_state: false`, Namedrop records the checkout but pins nothing.

The pin doesn't change. A `git_state` read shows the work as it was captured. A `file` or `diff` read shows the checkout's current files.

Namedrop doesn't follow a checkout that is moved, removed or replaced, for example by `git worktree remove` and `add` at the same path, and never reads another worktree in its place. File and Git reads refuse with the reason, the snapshot text stays readable, and freshness is `unknown` with `reason: checkout_unavailable` and a `checkout_reason`. `context.inspect` shows every path locator as `[BLOCKED PATH]`, because Namedrop can no longer check it against the protected-path rules. The snapshot's evidence list still shows each locator as it was redacted at publish time. Publish a new snapshot from the checkout's current location. Namedrop tells a replacement directory from the original by its inode and creation time. On a filesystem that doesn't report creation times, it compares only the inode. Snapshots published before Namedrop recorded checkouts keep using the workspace's current worktree.

After a recipient replies, `handoff.results` adds `reviewed_git_state` and `freshness` for that recipient. Namedrop evaluates freshness when you read results, so another read after an edit can give a different answer.

| Freshness | Interpretation |
|---|---|
| `fresh` | All compared, allowed evidence matches the pinned version. |
| `stale` | At least one compared path changed. Inspect `changed_paths` and any `unavailable_paths`. |
| `unknown` | Namedrop couldn't make the comparison. Inspect `reason` or `unavailable_paths`. |

The check compares each `diff` path with the pinned Git tree, including content, executable bit, new files and deletions. A directory covers the allowed files inside it. Changes to other files don't affect the result. A listed path that isn't in the pin, such as an ignored file Git didn't capture, gives `unknown`. Protected paths stay protected, and the check never runs custom Git content filters or filesystem monitor hooks. Each file or Git metadata read is capped at 1 MiB. A returned review is tied to the request it answers, so the reviewer's reply text can't point the check at something else.

You get `unknown` when there's no context or pin, the reviewed checkout is unavailable, there are no `diff` paths, a protected file was swapped in, a custom content filter applies, or the evidence is too big or unsupported. If any path is known to have changed, the result is `stale`, and unavailable paths are still listed. Freshness says nothing about how good the review was or about files outside the evidence. Ask for a new review when the code has changed.

## Tracked coordination

Tracked coordination is for a lead agent that splits one piece of work you asked for into separate assignments. It only applies when the lead starts it. Ordinary messages and prompts you hand off directly work as before. The lead runs `handoff begin` from its own registered session:

```json
{"request_id":"review-begin-1","deliverable":"Review the implementation and tests; give one answer with remaining defects."}
```

It keeps the returned `coordination_id` and sends each worker its own request with that ID:

```json
{"request_id":"implementation-1","author_kind":"agent_session","coordination_id":"<coordination id>","source":"@reviewer Inspect implementation correctness and return evidence."}
```

A second `send` could ask another worker to check the tests. Each request becomes its own assignment. `handoff status` keeps showing an open coordination even when no messages are pending. `handoff results` takes either `message_id` or `coordination_id`, not both. With a `coordination_id` it returns the deliverable, the current revision, the assignments, the wake ID and the saved report, even for a blocked or cancelled coordination with no assignments. Reading doesn't start any work.

Workers reply using the IDs from the request they received, and say where their work stands:

| Reply field | Meaning |
|---|---|
| `work_outcome: needs_continuation` | Progress only. The worker is ending its turn and needs another request to continue. |
| `work_outcome: completed` | The worker says it's done. The lead still reviews it and reports to you. |
| `work_outcome: blocked` | Needs a `blocked_reason`, and can name a `waiting_on_assignment_id` in the same coordination. A dependency that doesn't exist, belongs to another coordination or loops back is rejected. |
| Outcome omitted | The work stays `unclassified`. Receipt, `ack` and `complete.no-reply` only confirm delivery. |

The lead reads the results, then continues an assignment, adds a new assignment or closes the coordination. A follow-up `send` includes `coordination_id`, `assignment_id` and the current `expected_revision`, goes only to that assignment's original worker, and bumps the revision. Worker sessions are fixed, and renaming never hands over a role. The lead session is fixed too, until a new session on the same lead address takes the coordination over with `handoff adopt`. An old reply can't overwrite newer work. Sending the same request ID with the same input again is harmless. Namedrop rejects the same ID with a different outcome, dependency, revision or report.

Use these follow-ups for corrections and review notes on an assignment. An ordinary `send` starts a separate, untracked conversation, and closing the coordination won't clear that conversation's queued replies. A message isn't part of a coordination just because the names or text match.

```json
{"request_id":"implementation-2","author_kind":"agent_session","coordination_id":"<coordination id>","assignment_id":"<assignment id>","expected_revision":1,"source":"@reviewer Correct the missing boundary check and report the result."}
```

A current lead nudge or tracked result set includes bound actions for the next phase. Run its `address.list` action immediately before choosing a worker. Preview the intended `source` and optional `context` with its `handoff.preview` action. Preview checks routing and context only. It accepts no request, coordination, assignment or revision fields and sends nothing. The actual send enforces coordination ownership, recipient sessions and assignment revisions.

Use the new-assignment `send` to add work after a helper returns. It binds the original coordination and request-ID prefix, leaving the recipient, task and `<new-assignment-key>` for the lead to author. Replace that placeholder with a unique stable key per independently dispatched slice, retaining the attempt prefix and the variant suffix. Plain IDs end in `-plain` and context IDs end in `-context`, so keys such as `slice` and `slice-context` cannot collide across variants. It omits `assignment_id` and `expected_revision`, so it creates an assignment at revision 1 without changing earlier assignments. Its source may name multiple recipients, creating one assignment per recipient. Recipients of one send share its checkout. To dispatch writers in separate checkouts from the same callback, use a distinct key and context for each slice. Do not reuse an authored request ID for a different source. Fixed continuation actions still bind the original assignment, current revision and recipient. A retired or replaced worker cannot inherit a continuation. The lead can explicitly assign new work to an eligible replacement.

Both sends have context variants. Author their snapshot and evidence, select an optional registered checkout, and choose whether to capture Git state. A plain send and its context variant have different request IDs. Choose one variant per intent. After an uncertain dispatch, inspect results before considering the alternate variant, because sending both can create duplicate work. Keep the selected ID and input unchanged when retrying. Current replay matching does not compare context. The same request ID and source with different context replays the original message and snapshot. Do not reuse a plain send's ID to attach new context. The context goes through the same worktree, protected-path and grant checks as any other send. Only the coordination's owner session can add or continue tracked assignments. After `handoff adopt`, that is the adopting session. Workers receive no coordinator actions, and stale nudges authorize only results and completion.

### Continuation and final reports

A worker that says "I'll continue" and then ends its turn won't carry on by itself. So when a coordination has unfinished work but nothing is running, Namedrop checks that the sessions are still live and sends the lead one nudge, marked `control.coordination_nudge: true`, per revision. The lead reads the results, sends a follow-up or resolves, and completes the nudge. If the lead's turn ends without completing the nudge, Namedrop settles it as `no_reply` itself 30 seconds after the nudge was submitted, if the lead is idle by then, because the coordination already records what the lead did. Nothing is resent. More idle events don't send another nudge for the same revision, and a nudge the lead ignores is flagged for attention instead of repeating forever.

Namedrop doesn't nudge an agent that's still working. Instead of nudging, it flags attention when the host status is unknown or blocked, the lead's session is gone, a name is disabled or retired, the workspace is paused or stopped, or a delivery is held or uncertain. An assignment waiting on another one only delays the nudge while that other assignment has work running or ready to send. Check the reason Doctor gives before using its repair. Resuming doesn't redo a worker's side effects. A replacement session on the lead's address gets the lead's role only when it runs `handoff adopt`.

To finish, the lead session that owns the coordination calls `handoff resolve` with a report:

```json
{"request_id":"review-final-1","coordination_id":"<coordination id>","outcome":"completed","report":"Implementation and tests reviewed; the boundary correction passed.","reconciled_assignments":[{"assignment_id":"<first id>","revision":2},{"assignment_id":"<second id>","revision":1}]}
```

`completed` needs every assignment marked completed, with one `reconciled_assignments` entry for each at its current revision. `blocked` or `cancelled` needs a `reason` instead, is never reported as success, and can explain why nothing was assigned. A blank report, or a missing, duplicate, foreign or out-of-date entry, is rejected before anything changes.

A `completed` resolve also clears worker replies still queued for the lead, up to each reconciled revision, in the same step that saves the report. Those deliveries become `cancelled`, but their text, links and history stay. Reading results doesn't clear anything. Untracked conversations, other coordinations, and deliveries that already started or are held aren't touched, and `blocked` or `cancelled` resolves don't clear anything.

An unfinished delivery blocks later messages to the same agent, including the final report. The agent that received it can recover it with `delivery.inbox` from its own session. Don't try to finish it from another pane or mark an uncertain delivery complete. A delivery the agent received but never completed settles on its own 30 seconds after it was submitted, if the agent is idle by then. It is then flagged for review unless it was a nudge, or a tracked request its coordination still awaits. Such a request is flagged only when the coordination ends or the worker's exact session or name is gone before it answers. A request the lead superseded with a follow-up is never flagged; the follow-up's own delivery carries the obligation. Doctor's explanation names which happened.

| Report field | Meaning |
|---|---|
| `report_saved` and `report` | The report text was saved together with the message that carries it. |
| `report_delivery_state` and `report_delivery_settled` | Delivery state of that message. Settled doesn't mean you saw it. |
| `presentation: unknown` | Namedrop can't know whether the answer was shown to you. The saved text is still there, and this alone isn't a problem. |

The attention flag clears once the outcome is `completed`, the report is saved and its delivery is settled. That also clears warnings saved by earlier versions, without deleting their history. Blocked or cancelled outcomes, and reports that are missing or not yet delivered, keep the flag with a reason. Doctor only reads these flags. After an upgrade, older warnings go away the next time Namedrop reconciles.

To dismiss the warning on an old blocked or cancelled handoff once you've reviewed it, on an open handoff whose lead was retired, or on a completed handoff whose report delivery you cancelled:

```sh
namedrop handoff acknowledge --input-json '{"coordination_id":"coordination-1","confirmed":true}'
namedrop doctor
```

Use the coordination ID Doctor shows. This saves your acknowledgement and clears only that warning, and it stays cleared. The outcome, reason, report and deliveries don't change, and nothing is completed, resumed or cancelled. It's also accepted for an open handoff whose lead address was retired, for example because the lead's pane closed, since that lead can never come back to resolve it. Acknowledging such an open handoff also stops it from blocking an update that needs a session refresh. A completed handoff whose report is saved and whose report delivery you cancelled with `delivery cancel-held` is accepted too, since that delivery never settles on its own. It's refused for open handoffs with a live lead, for missing or undelivered reports on blocked or cancelled outcomes, for completed handoffs unless their report delivery was cancelled, and for unfinished deliveries. It's outside agents' automatic permissions, and an agent should only run it when you ask. Doctor's **Ask agent to help** doesn't count as asking.

To dismiss the warning on a settled delivery whose recipient was retired, or whose exact session was replaced, before it recorded a reply or no-reply disposition (Doctor shows it as ND-DELIVERY-003 with the attempt ID as `source_id`, and its explanation says why the delivery went unanswered):

```sh
namedrop delivery acknowledge --input-json '{"attempt_id":"attempt-1","confirmed":true}'
namedrop doctor
```

This saves your acknowledgement and clears only that warning. The delivery, its history and the thread don't change, no disposition is invented and nothing is resent. It's refused while the recipient isn't retired and no newer session has been seen on its name, because that session can still record the disposition itself. It's outside agents' automatic permissions, and an agent should only run it when you ask. Doctor's **Ask agent to help** doesn't count as asking.

### Adopt a handoff after a lead restart

When a lead's agent session exits and a new session starts in the same pane, the pane address keeps its name and the new session registers, but the open coordination still records the old session as its owner. The new session cannot list, read, continue or resolve it, and `handoff acknowledge` refuses because the address is not retired. `handoff status` lists such coordinations under `adoptable`. The new session takes one over with:

```sh
namedrop handoff adopt --input-json '{"request_id":"adopt-1","coordination_id":"coordination-1"}'
```

Adoption moves the owner session and cancels the worker replies still queued for the old one. Assignments, worker sessions and the saved report don't change. After it, the new session reads the coordination's results by `coordination_id`, sends new assignments and continuations, and resolves the coordination exactly as the original session could. The old session can no longer act on it. A `handoff results` read keyed by a `message_id` the old session sent stays with that session. The response is the coordination report plus an `adoption` block with `changed`, `owner_session_id` and `previous_owner_session_id`. Repeating the command from the same session with the same request ID is harmless and returns `changed:false`. Namedrop records each transfer as an immutable transition fact that names both sessions.

Adoption is refused for a session on another address, for the original session while it still owns the coordination, for a request ID this session already used for another coordination, and for a coordination that is already resolved. From the session that already adopted, the same request ID replays the response with `changed:false` and a different request ID is refused. A retired address has no current session, so nothing can adopt its coordinations. A human reviews and acknowledges those instead.

Adoption cancels worker replies that were still queued for the old session. Their work outcomes are already recorded on their assignments, so `handoff results` shows them, and the replies' text and history stay. A nudge still queued for the old session is retired at the next drain. A nudge or reply that was already dispatched or held for the old session stays as it is. Run `delivery cancel-held` for it with `confirmed:true`.

`handoff adopt` is in agents' automatic permissions. It can only move a coordination between sessions of the caller's own live, registered address in the same workspace. It grants no permission. The worker replies it cancels could never be delivered, and they keep their text and recorded outcomes. The restarted lead needs it without a human in the loop.

### Capabilities and upgrading

Agents register with the operations they support. The current set has 17, adding `handoff.adopt` to the previous 16, which added `handoff.begin` and `handoff.resolve` to the older 14. Sessions registered with the 16 or the 14 keep working and are not re-challenged. A session registered with the 14 can still send ordinary messages, but Namedrop refuses tracked coordination if the lead or any worker has it. A session registered with the 16 can run `handoff adopt`, because adoption needs only the `handoff.begin` and `handoff.resolve` capabilities. Refresh the installed package and start new sessions before using it. Registering doesn't reload an agent's instructions or prove its hooks work. The describable `address.list` management entry opts into automatic command permission with `automatic_permission: true`. Management entries default to false. Group changes, persona edits, and handoff and delivery acknowledgements remain outside automatic permissions. This metadata does not add registration capabilities or delivery authority.

Schema 5 migrates earlier databases and keeps names, bindings, sessions, registrations, pauses, messages and queued or held deliveries. Upgrades from schema 3 and 4 run live behind the command lock, so agent sessions and pending deliveries carry on. Keep the matching executable and a database backup together in case you roll back. If the lead's session is gone, an unresolved coordination or an undelivered report still blocks an incompatible update. An open coordination whose lead address is retired stops blocking once you [acknowledge it](#continuation-and-final-reports). Its unfinished deliveries still block. An open coordination whose lead restarted in the same pane stops blocking once the new session [adopts](#adopt-a-handoff-after-a-lead-restart) and resolves it. Once a report delivery has finished, its saved text and any warning stay recoverable and no longer block updates.

### Recover a report after owner loss

Only the coordination's owner session can read `handoff.results`. If the coordination is still open and the lead's address is live with a new session registered on it, that session should run `handoff adopt` as described in [Adopt a handoff after a lead restart](#adopt-a-handoff-after-a-lead-restart). Adoption applies only to an open coordination. For a resolved one, or when the address is retired or unbound, this read-only query is the way to read the saved report without changing ownership or settling anything. Find the state directory with `namedrop doctor`, then run this read-only query with the socket, workspace and coordination IDs Doctor reports:

```sh
python3 - /absolute/state/namedrop.sqlite3 '<socket id>' '<workspace id>' '<coordination id>' <<'PYTHON'
import json, sqlite3, sys
from pathlib import Path
path, socket, space, coordination = sys.argv[1:]
with sqlite3.connect(Path(path).resolve().as_uri() + '?mode=ro', uri=True) as db:
    db.row_factory = sqlite3.Row
    rows = db.execute("""SELECT c.coordination_id,c.owner_address_id,c.owner_session_id,
      c.deliverable,c.outcome,c.report,c.reason,c.report_message_id,
      d.state AS report_delivery_state,'unknown' AS presentation
      FROM coordinations c LEFT JOIN mailbox_deliveries d
      ON d.session_socket_id=c.session_socket_id AND d.worktree_id=c.worktree_id
      AND d.message_id=c.report_message_id
      WHERE c.session_socket_id=? AND c.worktree_id=? AND c.coordination_id=?""",
      (socket, space, coordination)).fetchall()
    print(json.dumps([dict(row) for row in rows], indent=2))
PYTHON
```

The query only reads and sends nothing. Treat the report text as private. Check Doctor before deciding how to redo any unfinished work.

## What installation, activation and packages each cover

Maintenance is safe only if you know which of these you are changing. They have different scopes, live in different places, and different commands remove them.

| Layer | Scope | Where it lives | What changes or removes it |
|---|---|---|---|
| Herdr plugin registration | Your user on this machine, every workspace | `herdr plugin list`. A `local` link records the path you linked, and a `github` installation records a checkout Herdr owns | `herdr plugin link` or `unlink` for a local link, `herdr plugin install` or `uninstall` for a managed one |
| Namedrop activation | One Herdr workspace | Rows in the state database for that workspace | The `activate` and `deactivate` actions |
| Shared runtime packages | Your user, shared by every workspace | `~/.agents/skills/namedrop`, `~/.claude/skills/namedrop`, `$CODEX_HOME/rules/namedrop.rules` | Reactivation refreshes them. Only you remove them |
| Claude collaboration permissions | Your Claude profile, shared by every workspace | `$CLAUDE_CONFIG_DIR/settings.json` or `~/.claude/settings.json`; ownership in `claude-permissions.json` in Namedrop state | Activation adds or refreshes the managed entries. The last active worktree in the state directory to deactivate removes only those entries |
| Shell command | Your user, shared by every workspace | `~/.local/bin/namedrop` and the `# Namedrop CLI` PATH block in shell startup files | Activation installs or refreshes them. Remove the launcher explicitly when uninstalling |
| Durable collaboration state | Your user, partitioned per space | `namedrop.sqlite3` in the `state_directory` activation reports | Nothing implicit. Only a restore or an explicit `purge` |
| Tour markers | Your user on this machine, every workspace | `tours-shown/<id>` in the same state directory | `namedrop ui reset-tour` clears the current one. Installs, updates and uninstalls leave them |
| Plugin configuration | The plugin id `namedrop` | `herdr plugin config-dir namedrop` | Survives linking, unlinking, installing and uninstalling |

Registration is global, activation is per workspace, and the runtime packages are shared. Registering Namedrop does not enable collaboration anywhere. Activating one workspace does not activate the others. Refreshing the shared packages affects every workspace at once.

Configuration and durable state deliberately live outside the installation directory, for both installation routes. Activation reports the `state_directory`, and `herdr plugin config-dir namedrop` reports the configuration directory. Neither is inside the linked package or inside Herdr's managed checkout. That makes an installation replaceable. You can delete and replace the package directory without touching names, the panes they point at, saved recipient types, groups, history or queued deliveries. Do not move the database into a package directory, and do not treat `herdr plugin config-dir namedrop` as a way to find it.

## Upgrade

The **Update Namedrop** action guides a GitHub-managed installation through replacement and activation. It uses Herdr's existing `plugin install` command. There is no separate `herdr plugin upgrade` or `herdr plugin update`.

### Guided update

The guided updater requires Python 3.9 or newer on `PATH`. From a plain shell tab in the workspace you want to activate:

```sh
herdr plugin action invoke update --plugin namedrop
```

The tab shows the installed source and ref and follows **Check update → Install → Activate**. It preserves the origin, repository subdirectory and requested ref, including tag or commit pins. Local links and archives use the manual steps below.

For a compatible release, agents stay open and existing tools and handoffs keep working. Answer **y** at **Install update now? [y/N]** to go ahead. Declining, EOF or Ctrl+C before installation postpones it. The updater never closes panes, stops agents, cancels deliveries, changes pause settings or makes up replies, and it doesn't retry held or uncertain work. Herdr does briefly swap the checkout it uses to run host events, so a wake that lands in that moment can be delayed until the next reconciliation.

Activation installs a stable command at `<state_directory>/runtime/namedrop` that points at a saved copy of the executable, kept outside Herdr's checkout. Agent tools, cached hooks and permission rules all use that path, so Herdr can replace the checkout while they keep working. Switching to a new copy happens in one step, and commands already running finish on the old one. Old commands can't overwrite the new version's shared packages. Namedrop keeps old copies, and nothing deletes them automatically.

**One-time migration.** Sessions started before the stable command existed may still point at the old checkout. The first activation records which sessions were open. The updater lists any that are still open and offers **Wait here** or **Later**. Finish them when it suits you and exit back to the shell. The panes and workspaces can stay open. Sessions started after setup can keep running. Also finish agents running outside Herdr or in other profiles, because Herdr can't see them all. If a session can't be identified or the Herdr server can't be reached, the updater won't treat the migration as done.

Codex keeps its own versioned copy of the plugin. If setup prints Codex refresh commands, run them and open **Activate Namedrop** again before starting new Codex sessions. Until activation has checked that copy, Codex sessions stay on the wait list. After that, only sessions that could have loaded the old copy do. If the Codex plugin isn't installed or is disabled, no refresh is needed. Later compatible releases may print a refresh command for new instructions. Existing sessions keep working with the ones they loaded.

**Breaking releases.** `runtime.json` declares the launcher protocol, the live-upgrade epoch, the collaboration protocol and the state schema. An update runs live only when the protocols and epoch match, the host entrypoints are unchanged, and the schema is either the same or a declared live migration with `command_gate: 1`. This release declares `live_migrate_from: [3, 4]` for schema 5. Anything else, including releases that don't declare these, waits for all sessions, terminal commands and unfinished handoffs to finish. Namedrop keeps held or uncertain work, and you review it as usual. The updater asks again once work finishes. A session ending doesn't trigger an install by itself. While a fallback install runs, don't start new agents or handoffs. For a schema this release doesn't know, follow that release's recovery steps.

The updater inspects the new source without running any of its code. During the install, a temporary Git checkout hook checks that the commit is the one inspected before Herdr builds or replaces anything. If the branch moved in the meantime, the install stops and the current version keeps running. The hook only lasts for that install, and no Git configuration is changed. `herdr plugin install` still records the original source and ref.

Build output and activation results go to `last-update.log` in the state directory. A failed install leaves the running version alone. If activation fails, the updater switches back to the previous copy only if that copy can still read the database. Herdr's registration may already point at the new release. If activation already migrated the state, it keeps the new version and prints recovery steps, because the older one may not read the migrated state. Check the log and the registration before retrying. Reopening the updater retries activation when the installed source and the running version differ, including for a fixed pin. Nothing rolls back or retries on its own.

**Live schema migration.** Activation takes an exclusive lock and switches to the new executable while holding it. New commands wait for the lock, then check which version to run. Namedrop finds commands from releases older than the lock by their executable paths and lets them finish. It stops no agent. The migration itself is one SQLite transaction. If a command runs long, or Namedrop can't inspect running processes, the switch waits and the old schema stays as it is. The migration process holds the lock itself, so interrupting the updater can't let waiting commands through mid-migration.

If a switch is interrupted, a marker on disk stops ordinary commands from running. Reopening the updater goes back to the previous version if the old schema is still there, or finishes the switch if the migration already committed. It never rolls back the database. The updater never reports a release that was downloaded but not fully activated as up to date. It shows Installed and Active separately, and goes straight to activation, without rebuilding, when the downloaded branch already matches.

Agents registered with the older set of operations can still send ordinary messages after the update. Updating doesn't change an agent's conversation or give it operations it didn't register. Tracked coordination needs the current set, so follow setup's package steps and start new sessions for it. You don't have to close existing sessions to finish the update.

The registration and shared agent packages apply to every workspace, but setup only activates the workspace where you ran the update. Other workspaces stay activated. Compatible sessions keep their loaded instructions and tools, and new sessions load the new ones. `namedrop doctor` checks setup. Only a real mention and reply shows the agent's hooks work. Names and groups keep working.

### Manual upgrade

Keep Herdr running throughout the upgrade. For the remove-and-reinstall sequence, see [reinstalling](../README.md#reinstall-an-existing-installation). A GitHub-managed installation can also be updated in place using the steps below.

1. Stop initiating handoffs and let work finish. Pause Namedrop in each active workspace with `herdr plugin action invoke pause --plugin namedrop`. An empty pane doesn't mean the work is complete. `sh scripts/setup.sh check` reports held and expired deliveries, and `handoff status` from each originating session lists that session's outstanding requests. Once work has finished, exit Claude and Codex sessions before replacing the executable.
2. Record activation's exact `state_directory` and the installed executable, then [back up the state](#back-up-and-restore-state). For a local or archive installation, keep the old package directory, because it is your rollback. A managed installation has no such artifact. Reinstalling replaces Herdr's checkout in place, so record the current `resolved_commit` from `herdr plugin list --plugin namedrop --json` instead, because reinstalling that ref is the only way back.
3. Register the replacement. Extract the verified new archive to a **new** versioned directory and run `sh scripts/setup.sh` from it, or update a clean source checkout and run the same command there. For a managed installation, first run `cd "$HOME"` so the shell is outside the checkout Herdr replaces, then run `herdr plugin install ericbust/namedrop --ref <revision>`, substituting your installed origin if different. Use `main` for the latest code or a tag or commit to pin the version. Open setup with `herdr plugin pane open --plugin namedrop --entrypoint setup`.
4. Read the setup result and follow the runtime instructions it prints, then press Enter to dismiss the setup tab. Open a **new terminal tab** and run `namedrop doctor`. Start **fresh** sessions in both runtimes and review changed hook entries. Name or reconnect agents, resume with `namedrop space resume` if you paused the workspace, and verify a round trip in both directions. Existing workspaces keep their activation. Run `namedrop doctor` there and repeat setup if it reports attention or you want to enable another workspace.

The plugin id stays `namedrop`, so the new installation gets the same configuration directory, state and names. Current releases run through a stable command outside the checkout, so a compatible manual activation can also leave sessions running. The careful steps above are for legacy and breaking upgrades. Activation rewrites the shared runtime packages and permission rules to point at the executable and state directory of the installation that is now registered. Those packages are shared, so upgrading in one workspace updates them for all. Activation also overwrites any hand edits inside `~/.agents/skills/namedrop`, `~/.claude/skills/namedrop` or `namedrop.rules`. Ownership markers protect other tools' directories, not your changes inside Namedrop's. Repair commands keep the same path across compatible updates of stable-runtime installations. A command copied from a legacy installation may point at a removed checkout. Run `sh scripts/setup.sh check` again for a current command with the same attempt and recipient IDs.

Updating packages doesn't reload a running session's instructions or hooks. Compatible releases keep working with what sessions already loaded, and new sessions load the new instructions. First-time setup and breaking changes still need the printed setup steps. Namedrop never resends uncertain work. After activation, Namedrop dispatches nothing until it reconciles. Held work stays held until you deal with it, and deliveries queued behind it keep waiting.

Don't rebuild or delete a runtime copy while commands are using it. The updater keeps them for this reason. Keep old package directories until their sessions have finished and you've checked that the new version works.

Agents' automatic permissions include the read commands `handoff.status`, `handoff.results`, `handoff.preview` and `group.list`. Changing groups needs your request. Refresh the packages and start new sessions to pick these up. Requests keep their original owner session across the migration.

## Migrate a local link to a managed installation

Herdr registers one plugin per id, and what happens when you register a second one depends on which command you use and what is already there. Run `herdr plugin list --plugin namedrop` before any of them.

| Already registered | Command | What Herdr does |
|---|---|---|
| a `local` link | `herdr plugin install …` | **Refuses.** Exits `1` with `plugin namedrop is already linked from a local path; uninstall/unlink it before installing from GitHub`. The link is untouched. |
| a `github` installation | `herdr plugin install … --ref <other>` | **Replaces it in place.** Exits `0` and keeps the same managed directory, with a new ref and resolved commit. The previous revision's files are gone. |
| a `local` link | `herdr plugin link <other path>` | **Silently repoints.** Exits `0` with no warning. The old directory's files are left alone. |
| a `github` installation | `herdr plugin link <path>` | **Silently converts it to a local link.** Exits `0` with no warning. Herdr's managed checkout is orphaned on disk and `herdr plugin install` then refuses until you unlink. |

Only `install` over a local link refuses. Every other combination changes your registration without asking. The refusal happens **after** the clone, because Herdr reads the manifest to learn the plugin id. It happens **before** any of the manifest's build commands run, so a refused install costs a clone and nothing more.

The last row is the riskiest. `herdr plugin link` quietly turns a managed installation into a local link. `sh scripts/setup.sh` doesn't, because it detects a `github` registration and only activates it. The bare Herdr command has no such guard.

Keep Herdr running, finish active handoffs, and exit Claude and Codex sessions before changing the registration.

```sh
# 1. Back up state and confirm what is registered now.
herdr plugin list --plugin namedrop --json
# 2. Remove only the registration. Files, state and configuration all stay.
cd "$HOME"
namedrop deactivate
herdr plugin disable namedrop
herdr plugin unlink namedrop
# 3. Install the managed origin. Use a tag or commit instead of main to pin it.
herdr plugin install ericbust/namedrop --ref main
# 4. Activate through the setup tab.
herdr plugin pane open --plugin namedrop --entrypoint setup
# 5. Verify the managed origin.
herdr plugin list --plugin namedrop --json
```

The commands in this section name `ericbust/namedrop` because that is Namedrop's own origin. If you installed from a fork, substitute your own `OWNER/REPO`. A private fork needs GitHub credentials with read access and a `git` that can clone it without prompting. Source installation, same-ref reinstall and local-to-managed migration were verified at a pinned commit. See [compatibility](compatibility.md).

Step 5 should report `source.kind` `github` with the `owner`, `repo`, `requested_ref` and `resolved_commit` you asked for, and a `plugin_root` inside Herdr's own plugin directory rather than your checkout. Record the `resolved_commit`, because it makes the installation reproducible. After migrating, `sh scripts/setup.sh` detects the managed installation and only activates it. It neither rebuilds nor relinks, so a later `herdr plugin install` still refreshes from the same origin.

Read the setup result and complete its runtime instructions. Open a new terminal tab for `namedrop doctor`, then start fresh agents and [verify a mention and reply](../README.md#connect-your-agents). Herdr itself does not need a restart.

Keep the old package until step 5 passes. Unlinking removes the registration only, so relinking it with `herdr plugin link <old path> --enabled` restores the previous installation. Check `herdr plugin list --plugin namedrop` first, because `link` repoints whatever is registered without asking. You need that fallback because nothing is registered between steps 2 and 3. A managed install that fails registers nothing, leaves no managed directory behind, and does not restore your local link.

Recovering a *managed* installation is different. `herdr plugin uninstall namedrop` deletes the checkout Herdr owned, so there is no directory left to relink. You recover it by installing the same `--ref` again. That is the reason to record the resolved commit before you change anything.

## Back up and restore state

Namedrop keeps its durable state in one SQLite database, `namedrop.sqlite3`, in the `state_directory` activation reports. Queued agent-status hook events sit beside it, as `pending-*` files under `agent-status-reconciliation/`, so a complete backup covers the directory and not only the database. `claude-permissions.json` records which Claude allow entries Namedrop added. Keep that record with the settings profile it describes rather than replacing it with a database backup. The once-only tour markers under `tours-shown/` live there too. The blocks below leave them out, and a restore without them only lets the current tour open once more. `sqlite3` ships with macOS at `/usr/bin/sqlite3`. On Linux it comes from the distribution's SQLite package (`sqlite` on Arch).

The destination holds collaboration bodies, so restrict it *before* anything sensitive lands in it, rather than hardening it after the copy. `umask 077` and `chmod 700` come first so nothing is ever written into a readable directory. The closing `chmod -R go-rwx` is a backstop for a file that arrived with its own explicit mode. Run the block as a whole. `umask` applies to the rest of that shell session, so use a throwaway terminal or reset it with `umask 022` afterwards:

```sh
umask 077
STATE="<the state_directory activation reported>"
BACKUP="$HOME/namedrop-backup-$(date +%Y%m%d%H%M%S)"
mkdir -p "$BACKUP"
chmod 700 "$BACKUP"
sqlite3 "$STATE/namedrop.sqlite3" ".backup '$BACKUP/namedrop.sqlite3'"
sqlite3 "$BACKUP/namedrop.sqlite3" "PRAGMA integrity_check; PRAGMA user_version;"
if [ -d "$STATE/agent-status-reconciliation" ]; then
  cp -R "$STATE/agent-status-reconciliation" "$BACKUP/"
fi
chmod -R go-rwx "$BACKUP"
```

Take the database through SQLite rather than copying the file. `.backup` makes a consistent copy while the database is in use, and `VACUUM INTO '<path>'` is an equivalent alternative. A plain `cp` is not equivalent. This database uses SQLite's default rollback journal, so a copy taken during a write transaction can capture a partly applied change without the `namedrop.sqlite3-journal` file that would undo it. Namedrop never enables WAL, so there is no durable `-wal` and `-shm` pair to copy alongside the database. The journal file exists only for the length of a transaction. Verify every backup with `integrity_check`, and record the `user_version` it reports. That is the schema you must restore the backup against.

What this gives you is a consistent database snapshot plus a separate copy of the markers, **not** an atomic image of the whole directory. For a mutually consistent backup, stop activity first. Run `herdr plugin disable namedrop`, or run `herdr plugin action invoke deactivate --plugin namedrop` in every workspace using it. Then confirm that no `pending-*` file remains under `agent-status-reconciliation/` before running the block above.

If Namedrop keeps running, the two steps drift, because producers and the reconciliation worker keep working between them. A hook can queue a new `pending-*` marker after `.backup`, and the copy then includes it. The worker can apply and delete a marker after `.backup`, so the copy no longer contains it. No command body is at risk either way. A marker only records *that* a runtime's agent status should be reconciled. Reconciliation inspects current host state when it runs, so the message and delivery records it acts on are all inside the database the snapshot already captured. What can drift is one reconciliation pass, and the next status event reconciles against live state regardless.

Restore only with every Namedrop process stopped. Clear the old state first rather than copying over it:

```sh
rm -f "$STATE/namedrop.sqlite3" "$STATE/namedrop.sqlite3-journal"
rm -rf "$STATE/agent-status-reconciliation"
cp "$BACKUP/namedrop.sqlite3" "$STATE/namedrop.sqlite3"
chmod 600 "$STATE/namedrop.sqlite3"
if [ -d "$BACKUP/agent-status-reconciliation" ]; then
  cp -R "$BACKUP/agent-status-reconciliation" "$STATE/"
fi
```

Both removals matter. A restore usually follows a crash, and a crash can leave a hot `namedrop.sqlite3-journal` behind. SQLite would roll that stale journal back over the database you just restored. Leftover `pending-*` markers are hook events queued *after* the backup, and replaying them against rolled-back state reapplies observations the restored database never saw.

Restoring discards everything recorded after the backup was taken, including names created and deliveries settled since. A restore is a rollback of collaboration history, not a repair, so take a fresh backup of the current state before you overwrite it. Even a state you believe is broken is the only copy of the work done since.

## Schema and migrations

Namedrop uses schema **5**. Opening state for writing migrates it forward in place. Namedrop initializes a fresh directory at schema 5. Schema 1 gains group tables and alias-conflict guards, schema 2 gains terminal execution and saved recipient type, schema 3 gains coordination, assignment and message-link tables, and schema 4 gains the persona column. Migration keeps existing addresses, messages, delivery records and configuration. Addresses saved before the type existed stay agent recipients. Run `name <alias> --type terminal` to convert one. Upgrading to 5 keeps a user-chosen role description as that name's persona, whether the person typed it or it came with a role preset they explicitly chose. A name registered automatically by `name` has no description of its own and starts Generic. Queued and held deliveries keep their identities and state. Upgrades from schema 3 and 4 run live. Any other schema change goes through the session-refresh path.

Migration happens when the new executable first opens writable state, during setup or activation. Read-only commands do not migrate. For example, `doctor` reports `unavailable` against a schema it does not support instead of upgrading it. Because migration is one-way, take the backup **before** running the new executable, and keep it with a note of the `user_version` it holds.

## Roll back

Rolling back means restoring the executable *and* the state that executable can read.

1. Stop collaboration. Run `herdr plugin action invoke deactivate --plugin namedrop` in each active workspace, then confirm no Namedrop process is running.
2. Register the previous installation again. Check `herdr plugin list --plugin namedrop` first, then `herdr plugin link <old path> --enabled` for a local package, or `herdr plugin install ericbust/namedrop --ref <previous tag>` for a managed one, substituting the origin you installed from if it differs. Reinstalling an earlier ref over a managed installation works and is the managed rollback. Running `link` against one would convert it to a local link instead.
3. Restore the backup whose `user_version` that executable supports, as above.
4. Reactivate with `sh scripts/setup.sh activate`, start fresh sessions, and re-check health.

Within one schema, steps 2 and 4 are enough and the state carries over untouched. A snapshot's recorded checkout needs no schema change, so an earlier executable reads such a snapshot as it read every snapshot: against the workspace's current worktree. Across schemas they are not enough, and there is no code-only path. An executable refuses state that a newer release already migrated rather than downgrading it. Namedrop rejects a writable operation with `database schema <found> is not supported; expected <supported>`, and `doctor` reports `unavailable` with `state has schema <found>; expected <supported>` under `ND-INSPECT-001`. It fails closed and changes nothing. That is why the matching backup is the only way back. If you have no backup at that schema, you cannot roll back past a migration. Stay on the newer executable and report the problem instead.

## Deactivate or remove

Deactivation stops collaboration in a workspace. Uninstalling removes Herdr's registration and managed checkout. The shared agent integrations and shell command need separate cleanup. These steps preserve your saved names, groups, messages, and queued deliveries. Take a [state backup](#back-up-and-restore-state) first.

### Stop collaboration temporarily

To stop collaboration in one workspace while preserving its data:

```sh
namedrop deactivate
```

Deactivation reports `state_preserved: true`. The registration, the installed files, the shared runtime packages and every saved name, group, message and queued delivery all stay. Deactivation keeps shared packages and rules on purpose, because other workspaces may still use them.

To stop the plugin running at all, without removing anything:

```sh
herdr plugin disable namedrop
```

Do this before you move any installed files. A registration stays invocable while it is enabled, so disabling first stops Herdr running startup, event or action commands against a directory you are in the middle of moving. `herdr plugin enable namedrop` reverses it.

### Uninstall from Herdr

Keep Herdr running. Finish active handoffs and exit Claude and Codex sessions. Run `namedrop deactivate` in every workspace using Namedrop before disabling it globally. Check `herdr plugin list --plugin namedrop --json` and record the source and `resolved_commit` for rollback.

For a GitHub-managed installation (`source.kind` is `github`):

```sh
cd "$HOME"
namedrop deactivate
herdr plugin disable namedrop
herdr plugin uninstall namedrop
```

For a local checkout or extracted archive (`source.kind` is `local`), use `herdr plugin unlink namedrop` in place of the last command. It removes the registration and leaves the local directory intact. Changing to your home directory first keeps your shell out of any directory being removed.

Check the registration afterward:

```sh
herdr plugin list --plugin namedrop --json
```

The `result.plugins` list should be empty. `uninstall` removes Herdr's managed checkout from disk. Recover that revision with `herdr plugin install` and the recorded commit. `unlink` leaves the directory available to relink.

Both routes preserve the state directory, plugin configuration, shared agent integrations, shell launcher, and PATH entries. A launcher pointing at a removed executable cannot run until you reinstall. If you are [reinstalling](../README.md#reinstall-an-existing-installation), setup refreshes those integrations and the launcher. Keep them and Herdr's native runtime integrations in place.

### Remove agent integrations and shell command

Use these additional steps when removing Namedrop entirely. Deactivate every workspace using it and exit agent sessions before uninstalling, as above.

If Codex was registered, remove its plugin entry before you move the packages. Skip this step if it never was:

```sh
codex plugin remove namedrop@namedrop --json
codex plugin marketplace remove namedrop --json
```

Move **only** these Namedrop-owned artifacts into a backup folder:

- `~/.agents/skills/namedrop` and `~/.claude/skills/namedrop`, each identified by a `.namedrop-package.json` containing `"owner":"namedrop"`. A directory without that marker is not Namedrop's, so leave it.
- `$CODEX_HOME/rules/namedrop.rules` (default `~/.codex/rules/namedrop.rules`), identified by its `Managed by Namedrop` header.
- `~/.local/bin/namedrop`, identified by its `Managed by Namedrop: shell command.` header. Remove only the `# Namedrop CLI` block from the startup files reported by activation. Leave any other PATH entries intact.
- The extracted or source package directory, once it is unregistered and no runtime refers to it.

Open a new terminal tab and start fresh agent sessions to discard the old hooks and shell configuration. Herdr itself can stay running.

Preserve unrelated skills, unrelated rules files, runtime settings and Herdr's native integrations. Check the owner marker or the managed header before moving anything, and never delete a shared skill directory to remove one skill.

### Erase collaboration history

If you intend to erase history, do this before uninstalling or moving the executable.

None of this erases collaboration history. Removing the integration does not remove the SQLite state directory, and no removal step implies permission to purge. If you deliberately want to erase one space's history, first [back up the state](#back-up-and-restore-state) again. Purge is irreversible, and no gate protects settled history or saved names. Then use the installed prefix to run `purge` with no input. It reports the exact space id, the affected record count, the number of nonterminal deliveries and `confirmation_required: true`, and erases nothing. Only then supply that same space id as `typed_space_id` through `--input-json`. Purge requires deactivation and refuses while any delivery is still nonterminal, so a purge you ran to tidy up cannot destroy pending and held work. It is not forensic erasure.
