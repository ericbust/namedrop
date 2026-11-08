# Changelog

Each release is a section here, newest first. The GitHub release for a version uses its section as the release notes. See [releasing](docs/releasing.md) for how versions are chosen.

## [1.6.0] - 2026-11-07

### Changed

- Namedrop adds fewer tokens to agent sessions. The instructions that come with each delivery are 22–43% shorter, depending on the kind of delivery, and the note about other workspaces on each turn is a third shorter. Delivery prompts themselves are unchanged, so prompts sent before the upgrade still authenticate.
- `send`, `reply` and `complete` print a short result when an agent runs them. Pass `--json` for the complete result, for example in scripts.
- `send`, `reply` and `complete` return as soon as the message is saved, and a background worker delivers it. They take about 40 ms instead of about 900 ms, and Codex no longer has to poll or retry them. If the worker can't start, the command delivers the message itself, as before.
- The rule for reaching agents in other workspaces is now part of the policy each session gets at start. A session upgraded without a restart keeps its old policy until it restarts or compacts. Each turn still shows the `#label @name` form.
- A lead gets fewer check-ins while its worker is busy. A check-in waits until the worker has been quiet for 10 minutes, and for 10 minutes after the lead reads its results, but never more than 30 minutes after the worker last finished. Stalled work is still flagged.
- The workspace roster appears only when it changes, and again after a session starts or compacts.
- Codex no longer reads the Namedrop skill during a delivery.
- `address list` shows each agent's context size, or unknown after a compaction.
- After a tracked `send`, the lead is told to finish its other sends and end its turn rather than wait or poll.

## [1.5.0] - 2026-11-06

### Added

- Channels (Beta) ships in the normal installation. Open **Open Namedrop Channels** in Herdr or run `namedrop channels`. See your workspaces, messages, threads, agents, and requests that need you in one local browser page. Channels supports Markdown, paged history, repository groups, resizable sidebars, light and dark themes, and **All activity** across workspaces.
- Reminders let agents wake themselves later or on a schedule and answer where you asked, in a pane, thread, DM, or agent conversation. Pending reminders have Cancel buttons in Channels. Delays and intervals are at least 60 seconds, with at most 20 pending reminders per Agent Session and 100 total occurrences per repeating reminder. Saved schedules survive process restarts but do not transfer to a replacement agent session.
- Open Channels from a phone or Mac on your tailnet with `namedrop channels --tailscale`. Private HTTPS uses Tailscale Serve and checks this machine's owner identity on every request. Existing handlers stay in place. `namedrop channels --tailscale off` removes only Namedrop's handler.
- Direct messages let you talk to one agent across workspaces without adding the conversation to a channel. Workspace labels distinguish same-named agents. DMs stay out of channels, threads, and All activity.
- Live Peek shows an agent's terminal with colors and live updates. It keeps output that scrolls by while you watch, loads available terminal history, and offers Expand and Restore. **Type to agent** submits prompts and sends keys to the real pane. Viewing, scrolling, and resizing remain passive. **Text snapshot** keeps the sanitized text view and manual Refresh.
- Typing `/` or `$` in a Channels message box or Type to agent suggests the addressed agent's skills. Claude Code and Codex have separate skill lists. Member chips and Peek headers show running subagent counts.

- Channels handoff warnings explain blocked, cancelled, stalled, and unfinished work. Details shows the saved report. Ask to review and Review all send scoped review requests with one-use grants that expire after 15 minutes or when the dashboard session changes.

### Changed

- Thread replies stay in their thread. The channel shows the top message, reply count, and latest reply. With one eligible agent, reply without typing `@`. With several agents, choose who receives the reply.
- Your own message resets a thread's Automatic Reply Limit and lifts a pause caused by that limit. Agent exchanges still pause after ten replies. A manually paused or stopped thread stays that way.
- Plain channel notes without known agent mentions stay in the channel and make no model call. Embedded mentions use an isolated small-model router to select only the agents addressed. Leading mentions keep their existing addressing rules.
- The database upgrades in place to schema 7 and keeps names, history, and queued work. Doctor reports pending reminders and warns when their clock is missing.
- `namedrop channels` is the public command. `namedrop dashboard` remains a hidden compatibility alias.

### Fixed

- Type to agent submits prompts to Codex reliably instead of sometimes leaving an unsent draft.
- Agents address you as "you".
- Tailnet Channels sign-ins last seven days across browser restarts. Missing or expired sign-ins show the command to open a fresh HTTPS link.
- Live Peek shows the whole terminal in tall and wide panes, keeps working with older Herdr builds, and streams through replies after Type to agent.
- Agent status agrees across member chips, the DM sidebar, and DM and Peek headers. Pending permissions and questions, including those from DMs, show Needs you and clear when dismissed. DM sidebar rows show the alias once.
- One-time reminders show no repeat count. Shorter times keep the full date and time on hover.
- An agent's terminal-only answer to a Channels request now appears in the thread or DM, marked **From @name's terminal**, and the conversation is flagged as needing you. A normal Namedrop reply prevents duplicate relay.
- Live Peek follows the latest output through resizing, avoids blank flashes while Codex streams, and preserves watched history. Unrecoverable lines show a gap marker.
- Committed channel posts and queued terminal commands continue delivery after the browser receives its response or disconnects. Dashboard update handoffs retain the port and browser session, and schema checks retry bounded SQLite contention.
- Update checks fetch only Git metadata and version files, avoiding downloads of unrelated repository content and demo videos.
- Handoff review grants bind to the exact request and committed message. Replaying a request cannot renew a grant or attach it to another message.

Start new agent sessions after updating so they learn the reminder commands and use the updated instructions. Channels remains Beta. Its interface and workflows may change. Live terminal content can contain secrets; remove private text from screenshots and logs before reporting a problem.

## [1.4.4] - 2026-11-04

### Fixed

- Namedrop commands such as naming a pane or sending a message no longer fail with `database is locked` while agents are busy. A slow check that held the database for seconds now finishes quickly. Existing databases are repaired in place without a format change, so older versions can still open them.

## [1.4.3] - 2026-11-01

### Fixed

- Sandboxed Codex agents can send, reply, acknowledge and complete deliveries, and read the current workspace's roster when the sandbox blocks Herdr's socket. Commands pass through Namedrop's delivery spool to a worker outside the sandbox, without broader network permissions.
- Activation grants Codex write access only to Namedrop's delivery spool. The runtime and database remain outside the grant. Namedrop replaces its older broad grants and preserves user grants; unsafe broad user grants must be removed before activation. Deactivation removes only the grant Namedrop added. Start fresh Codex sessions after activation to load the grant.
- A corrupt queued command or result no longer stops the worker from processing other commands. Namedrop removes the invalid record and keeps processing the queue.
- Codex sandbox setup works through linked parent directories, including macOS's `/var` path. Linked configuration, ownership-record and lock files are still refused.

## [1.4.2] - 2026-10-28

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
