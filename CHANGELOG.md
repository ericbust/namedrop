# Changelog

Each release is a section here, newest first. The GitHub release for a version uses its section as the release notes. See [releasing](docs/releasing.md) for how versions are chosen.

## [1.5.0] - 2026-10-28

### Removed

- The `namedrop dashboard` command is removed. Namedrop 1.4.0 shipped this browser dashboard without announcing it, because that release was cut from the `experimental` branch. Its code is kept on the `exp/dashboard` branch. The README also described a `herdr plugin action invoke dashboard` action, which no release registered. That text is removed.

## [1.4.1] - 2026-10-26

### Fixed

- Namedrop 1.4.0 could not activate over state written by the public `experimental` branch before the release. That branch had already marked the state as schema 6 when the release added the `remote_commands_terminal_id` column and the `delivery_seats` view to schema 6, so 1.4.0 took the number at its word, skipped both, and activation stopped at `no such column: a.remote_commands_terminal_id` with the previous release still selected. State at schema 6 now receives whichever of the two it lacks the first time 1.4.1 opens it, every record is kept, and activation completes. Until that open, Doctor names the pre-release schema instead of failing on a query. Installs upgraded from 1.3.3 or earlier were never affected.
- Namedrop could misread another process's command line while that process was starting, because it read `/proc/<pid>/cmdline` in pieces that could straddle the process's `exec`. Doctor's check for a running Codex shared app-server could then miss a live server for a moment. It now reads the command line in one call.

## [1.4.0] - 2026-10-26

### Added

- Mention agents in other Herdr workspaces. Put a workspace label before the mentions, as in `#exp-dashboard @lead Status?`, name several labels, or use `#all @lead` to reach that name in every workspace. Labels ignore case, a space matches a hyphen, and every workspace also answers to its Herdr id, such as `#w12`.
- A repository's name reaches its worktrees. `#namestack @claude` goes to the one `namestack` worktree that has `@claude`, even though Herdr labels each worktree's workspace after the worktree. A workspace labelled exactly `namestack` that has the name still wins, and if two worktrees have it your agent asks which one you meant.
- A workspace label with no mention, as in `#catchup-again run the tests`, sends nothing. Your agent lists that workspace's named agents and asks who you meant, even when there is only one.
- A mention without a label never leaves the current workspace, so where it goes doesn't depend on which names other workspaces have. If the name isn't in this workspace, nothing is sent, and you're told the exact label to use, such as `@lead is not named in this workspace. It is named in: #beta. Use #beta @lead to reach it.` Agents' `namedrop send` follows the same rule.
- Replies come back to the workspace that sent the message, and the recipient sees which workspace it came from. Stopping or pausing the conversation on either side applies to both.
- The prompt roster lists the named agents in your other workspaces, and `address list` with `{"workspaces":"all"}` shows every workspace's names and groups.
- Tracked handoffs reach agents in other workspaces. After `handoff begin`, send to `#label @name`, a group in that workspace, or `#all @name` with the `coordination_id`. The worker replies with its work outcome from its own workspace, and continuations, `handoff results`, `status`, `resolve` and `adopt`, nudges and the unanswered-request warning all work as they do in one workspace. The coordination stays in the lead's workspace; nothing is copied into the worker's.
- `handoff results` and `handoff status` show where a recipient in another workspace really stands: queued, held, settled or answered, as its own workspace records it, with that workspace's name and its delivery outlook. Before, such a recipient showed as settled from the moment of the send.
- Purging a workspace cancels the deliveries that other workspaces were still waiting on there, so their results and coordinations stop waiting.
- Agents in other workspaces can run commands in a named terminal once you allow it with `namedrop address allow-remote`, and the result comes back to the agent that sent the command. The terminal must be named with its workspace label, as in `#build-box @build make test`; an unlabelled name or `#all` never reaches another workspace's shell. `address deny-remote` takes it back and fails the commands still waiting, and a rebind or a new shell ends the grant.

### Changed

- Namedrop's state moves to schema 6 the first time the new release opens it. Context handoffs still stay inside one workspace.
- Namedrop's Codex hooks changed, so Codex asks once to trust them again in `/hooks`. A Codex session started before the update keeps receiving and answering messages, but a mention typed into it is not sent until you start a new Codex session and approve the hooks.
- Namedrop 1.3.3 and earlier refuse to open state at schema 6, so going back to one of them after this update needs a [state backup](docs/operations.md#back-up-and-restore-state) taken before it.

## [1.3.3] - 2026-10-26

### Fixed

- A mention you typed into a Claude Code pane no longer repeats itself. Claude Code puts a prompt the hook blocked back into the input box, and the reply was pasted in behind it, so Namedrop read the whole submission as a new mention and sent it again on every reply. A delivery that arrives behind text already in the input, whether that restored mention or a half-typed draft, is now handled as the delivery. The leftover text is never sent, and the agent tells you it came in with the delivery.

## [1.3.2] - 2026-10-26

### Fixed

- A mention you typed into a Codex pane was accepted, but its delivery could wait minutes, until some other activity in the workspace sent it. The wake that sends a typed mention runs after its hook has exited, and it refused the pane identity it inherited because Codex gives hooks no session ID to vouch for it. It now wakes the workspace directly, as the status worker already does, and reports a failure instead of staying silent. Mentions typed into Codex now arrive within seconds, as they do from Claude Code.

## [1.3.1] - 2026-10-25

### Fixed

- Codex on `gpt-6-sol` closed ordinary requests without answering them. Since 1.1.1 an ordinary delivery's instruction carried a separate sentence telling a reviewed worker report to finish with `complete.no-reply`, and that model read it as an order for every delivery, so the sender waited for a reply that never came. The sentence is gone: an ordinary delivery offers `reply` and `complete.no-reply` as equal choices again. Replies to your own requests still close without a loop.

## [1.3.0] - 2026-10-24

### Added

- GitHub-managed installations check for a newer version in the background once a day. Doctor reports the installed and available versions as ND-UPDATE-001 without affecting health status. Claude Code and Codex show a person-visible session-start line at most once a day for each available version, shared across panes. The line adds no agent context. Local links, archives and full commit pins make no update requests. Set `check_for_updates` to `false` in `settings.json` under `herdr plugin config-dir namedrop` to disable checks and notices.

### Changed

- The session-start wrapper forwards the update line alongside the existing selection policy and Codex shared app-server notice. Update Namedrop refreshes it during activation; restart agent sessions to load it. The installed hook command and `hooks.json` are unchanged, but Codex asks once to trust the refreshed hooks again in `/hooks`.

## [1.2.0] - 2026-10-23

### Added

- Namedrop now tells you when Codex 0.162 or later runs a Codex pane's session through its shared app-server. That server keeps the environment of wherever it started, so Herdr cannot tell which pane the session is in: mentions to that agent stay queued, a second Codex pane's session can land on the first, and Namedrop's own Codex hooks go silent. `namedrop doctor` reports it once as ND-SESSION-003, and stops giving the first-prompt and reinstall advice that cannot help here. Naming a Codex pane, activation and the start of an affected Codex session say the same thing. Each explains what is wrong, what the fix does and costs, the commands to run, and how to undo it. Namedrop never changes the Codex setting itself. See [the shared app-server](docs/compatibility.md#codex-0162-and-later-the-shared-app-server).

### Changed

- A Codex pane that the shared app-server serves shows "@name: see namedrop doctor" while idle instead of "send a prompt to connect".
- The Codex session-start hook changed, so Codex asks once to trust Namedrop's hooks again in `/hooks`. Restart Codex sessions after updating to load it.

## [1.1.1] - 2026-10-22

### Changed

- Each delivery envelope carries only the rules for the kind of delivery it is: a context request, a result set, a report to the person, a coordination nudge or an ordinary message. The whole rulebook used to ride along on every delivery; the instruction text per envelope is now 57% to 79% shorter. Nothing changes in what an envelope authorizes.

## [1.1.0] - 2026-10-22

### Fixed

- A delivery that Claude Code hands to the hook as marked pasted text, which it does for any prompt over 800 characters, now gets its verification context. Before, a long envelope arrived without it, so the agent could not tell a routed delivery from text the person pasted.

### Changed

- The selection policy arrives once per session through a new SessionStart hook on Claude Code and Codex, and again after a compaction. Each prompt now carries only the roster rows, about 95% less text than before. Restart agent sessions after updating so they load the new hook; a session started before the update keeps working and sees the roster rows without the policy until it restarts. Codex asks once to trust the new hook definition.
- A delivery that needs no visible answer ends with one status line instead of silence, which saves the extra model call Claude Code spends continuing a turn that printed nothing.
- Shorter command guidance in delivery envelopes. Nothing changes in what an envelope authorizes.

## [1.0.2] - 2026-10-12

### Changed

- Updated the bundled dependencies: rusqlite 0.40 (with its bundled SQLite), sha2 0.11, toml_edit 0.25, clap 4.6.7 and libc 0.2.190. Nothing changes in how Namedrop behaves, and no state migration runs.

## [1.0.1] - 2026-10-12

### Fixed

- A message never goes to its own author. Naming yourself among the leading mentions labels the message instead of addressing it, so a worker's tracked reply that starts with its own name now reaches the lead instead of being refused.
- A second Herdr workspace on a checkout that another workspace already has open now attaches to that checkout. Doctor no longer reports ND-INSPECT-001 for it.
- A delivery received by an idle agent settles when its grace period ends, so the messages queued behind it move on without a human prompting the agent. A coordination nudge settles as no reply instead of raising ND-DELIVERY-003.
- An unanswered tracked request raises ND-DELIVERY-003 only when its work has ended without the worker's answer, not while the assignment still awaits that worker.
- A command that names no Herdr workspace no longer reads or writes another workspace's state when two workspaces share a checkout. It takes the workspace from the caller's pane, and it refuses with exit 65 when it can't tell which workspace owns the checkout.
- Doctor reports ND-SETUP-004 when the active release isn't the staged build, such as after a bare `herdr plugin install`, and offers the command that activates it.

### Added

- Versioned releases. Every merge to `main` raises the version, adds a section to this changelog, and publishes a tagged GitHub release with archives for each platform.

## [1.0.0] - 2026-09-24

Initial public release.
