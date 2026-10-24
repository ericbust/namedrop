# Drive a testbed profile by hand

For probes the harness does not cover. Everything below runs in the profile's own
environment, never in the real `~/.herdr`, `~/.claude` or `~/.codex`.

## Commands in the profile's environment

`~/.namedrop-testbed/profiles/NAME/env.sh` holds the `env -i ...` line. Reuse its
variables for one-off commands:

```sh
vars=$(sed -n 's/^ *env -i \(.*\) \/bin\/bash --noprofile.*/\1/p' ~/.namedrop-testbed/profiles/NAME/env.sh)
eval "env -i $vars herdr pane list"
```

Namedrop itself:

```sh
ST=~/.namedrop-testbed/profiles/NAME/state/herdr/plugins/namedrop
$ST/runtime/namedrop --plugin-state $ST --herdr-workspace w1 [--herdr-pane w1:p3] doctor --human
```

`--json` gives the structured result under `data`. Naming a pane again
(`name @codex --human --type agent`) is idempotent and shows its advice. Panes are
`w1:p1` `@shell`, `w1:p2` `@claude`, `w1:p3` `@codex` on a fresh profile; a rerun without
`--fresh` opens a new workspace, so read the ids from `herdr pane list`.

## Panes

- `herdr pane read PANE --source visible` or `--source recent --lines 80` prints plain
  text. `herdr pane wait-output PANE --regex RE --timeout MS` blocks until a line matches.
- `herdr agent send-keys PANE enter|down|esc|c-c` (`c-c` is Ctrl-C; `ctrl-c` is rejected).
- `herdr agent prompt PANE "text" --wait --until idle --timeout MS` submits a prompt and
  returns when the sender's own turn ends, not when a round trip ends. Slash commands work.
- `herdr agent wait PANE --until idle --timeout MS`; `herdr pane process-info --pane PANE`
  shows the foreground argv (look for `--no-daemon`).
- `herdr pane run w1:p1 "cmd > log 2>&1; printf '%s' \$? > log.exit"` runs a command in the
  shell pane; the activation helper is `sh PROFILE/namedrop-source/scripts/setup.sh activate`.
- Popups need their launcher's environment:
  `env NAMEDROP_TARGET_WORKSPACE=w1 NAMEDROP_TARGET_PANE=w1:p3 NAMEDROP_TARGET_TERMINAL=<terminal_id> NAMEDROP_TARGET_SESSION=<session id or empty> $ST/runtime/namedrop --plugin-state $ST ui run doctor`
  in the shell pane, then `send-keys a` or `esc`. Terminal ids come from `herdr pane list`.

## Codex

- Codex renders a hook's `systemMessage` only after the first prompt, as a `↳ Hook ·` row.
  Hooks are skipped until trusted: at the startup menu "Hooks need review" send `down`
  then `enter` for "Trust all and continue". Under plain `codex` then stop the shared
  server (`codex app-server daemon stop`) and relaunch, because a running server ignores
  new trust. Codex draws its composer before that menu appears; wait for the footer line
  (`<model> <effort> · <cwd>`) before typing.
- Codex registers its Herdr session only at its first turn. Send a short prompt first.
- Quit Codex with `c-c`, twice if the first one only interrupts. SIGTERM leaves its screen
  on the pane. After `codex app-server daemon stop` a served window shows
  `Reconnecting to server…` and one `c-c` quits it.
- `/new` opens a worktree picker; `enter` keeps the current one. It is a new session: the
  session-start hook runs again.
- Codex's log is `PROFILE/codex/logs_2.sqlite`, table `logs`, column `feedback_log_body`;
  `process_uuid` starts with `pid:<pid>`. Grep for `hook/started` and `rpc.transport=`.
  It is written with a delay. The server's pid file is
  `PROFILE/codex/app-server-daemon/daemon.pid`.
- Every Codex reply to Namedrop goes through Codex's approval reviewer: the sandboxed
  attempt fails with a read-only file system, then the reviewer decides. The testbed sets
  `approvals_reviewer = "auto_review"` so that needs no human.

## Claude Code

- The trust prompt highlights "No, exit"; send `down` then `enter`, or Claude exits and the
  pane returns to its shell. Relaunch with `herdr pane run w1:p2 claude`.
- Seeding `home/.claude.json` (`hasCompletedOnboarding`, `lastOnboardingVersion`,
  `projects[<repo>].hasTrustDialogAccepted`) and `theme` in `home/.claude/settings.json`
  skips the first-run screens; the harness does this.
- Its transcript is `home/.claude/projects/<encoded cwd>/<session>.jsonl`. Hook text sits
  in `attachment` entries; `Namedrop verification:` there proves an authenticated delivery.
  Codex never gets that string; on Codex the `Namedrop commands:` block proves it.

## Finish

`./testbed destroy --profile NAME --yes` stops the server, kills leftover Codex daemons
and deletes the profile.
