# Remote terminal commands

An agent in one Herdr workspace makes a named terminal in another workspace run a command. The terminal takes commands only from its own workspace until a person there runs `address allow-remote`. The result comes home to the agent that sent the command, and `address deny-remote` takes the grant back.

## Sub-features

- `rt-refused` refuses `#beta-space @build …` from alpha while `@build` has no grant, and persists nothing.
- `rt-label` refuses an unlabelled `@build …` with the general rule that a bare name never leaves its workspace, and refuses `#all @build …` with the terminal rule that names the `#beta-space` label to put before the mention.
- `rt-self-grant` refuses `address allow-remote` run inside the terminal it would open.
- `rt-run` runs the exact command in Beta Space's shell after the grant, delivers the result envelope to alpha's lead without a manual drain, and shows the exit code and output in alpha's `handoff results`.
- `rt-deny` refuses the next send after `address deny-remote`.
- `rt-queued-deny` fails a command queued behind a busy shell in both workspaces the moment the person denies, and never runs it after a later allow.

## How to get to it (user POV)

- Activate Namedrop in two workspaces. In one, name a shell pane with `namedrop name @build --type terminal`.
- From another pane in that workspace, run `namedrop address allow-remote --input-json '{"address_id":"<id>"}'` with the `id` from `address list`.
- From an agent in the other workspace, send `#<label> @build <command>`.

## Driving it with control-namedrop

Preconditions: the same as [cross-workspace mentions](cross-workspace.md), whose setup this recipe shares.

- Run `.claude/skills/verify/native/control-namedrop.py --feature remote-terminal --binary <candidate>`.
- Setup. The cross-workspace fixture, plus two plain Bash tabs in Beta Space: `beta-build`, named `@build --type terminal` from inside itself, and `beta-shell`, where the helper types the person's CLI commands. `address list` must show `@build` as a terminal without `remote_commands`.
- before. Alpha's lead sends `#beta-space @build touch <marker>` through the CLI. The send must fail naming `allow-remote`, and the `collaboration_messages` count must not change.
- unlabelled. `@build touch <marker>` must fail with `Use #beta-space @build to reach it`, and persist nothing. `#all @build touch <marker>` must fail with `put #beta-space before the mention`, and persist nothing.
- self-grant. `address allow-remote` typed into `beta-build` must fail naming the named terminal.
- allow. `address allow-remote` typed into `beta-shell` must return `remote_commands: true`.
- allowed. `#beta-space @build touch <marker> && echo remote-ok` must be accepted. The marker must appear, and alpha's lead must receive an envelope from `@build` whose body has `exit code 0` and `remote-ok`, with no wake from the helper. Alpha's `handoff results` for the message must be `complete` with `terminal_result.exit_code` 0 and `remote-ok` in its output.
- deny. `address deny-remote` must drop `remote_commands`, and the next send must fail naming `allow-remote`.
- queued. After a new allow, the helper runs `sleep 8` in `beta-build` and sends again. The copy's delivery in Beta Space must be `queued`. `address deny-remote` must leave it `failed` in Beta Space and the author's delivery `failed` in alpha at once. After the sleep and another allow, the queued marker must still be absent: only the allowed command ever ran.
- Every assertion after setup is a `require` that names its step and the observed value. `summary.json` records `outcome`. `remote-terminal.json` keeps the panes, workspaces, every send with its response, alpha's `handoff results`, and every requirement. `<role>-envelopes.json` keeps every envelope each simulator received.

## Limits

- The author is a simulated Codex session, and the helper runs its CLI calls and bound operations for it. No model decides to send the command.
- The person is the helper typing into a Bash pane. The host permission prompt that stops an agent's own `allow-remote` is not exercised; `tests/cross_workspace.rs` pins that neither operation is in the agent catalog.
- The pause, rebind and respawn refusals at execution time are covered by `tests/cross_workspace.rs`, not here.
