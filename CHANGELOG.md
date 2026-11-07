# Changelog

Each release is a section here, newest first. The GitHub release for a version uses its section as the release notes. See [releasing](docs/releasing.md) for how versions are chosen.

## [1.5.0-exp.20] - 2026-11-06

### Fixed

- Agents no longer call you "the operator". Start new sessions to pick up the change.

## [1.5.0-exp.19] - 2026-11-06

### Fixed

- Type to agent submits prompts in Codex automatically instead of sometimes leaving an unsent draft.

## [1.5.0-exp.18] - 2026-11-06

### Fixed

- Pending permissions and questions from direct messages show Needs you across Channels and clear when dismissed.

## [1.5.0-exp.17] - 2026-11-06

### Added

- Open Channels from your phone or Mac with `namedrop channels --tailscale`. Private HTTPS access uses Tailscale Serve and accepts only this machine's Tailscale owner. Existing Serve handlers stay in place. Run `namedrop channels --tailscale off` to remove Namedrop's handler.

### Fixed

- Live Peek keeps streaming after you send a prompt with Type to agent and the agent replies.
- Agent status agrees across channel members, direct messages, and Peek. Pending permissions and questions show Needs you everywhere. DM sidebar rows show the agent name once.
- One-time reminders no longer show a repeat count. Reminder times are shorter, with the full date and time available on hover.

## [1.5.0-exp.16] - 2026-11-06

### Fixed

- Live Peek shows the whole terminal in tall and wide panes. It previously showed only the top 40 rows and first 120 columns, which could hide the agent's prompt on a Mac.
- Live Peek keeps working with older Herdr builds.

## [1.5.0-exp.15] - 2026-11-06

### Fixed

- Thread replies go to the only agent without typing `@`. With several agents, Channels asks you to pick who receives the reply.

## [1.5.0-exp.14] - 2026-11-05

### Added

- Agents can remind themselves. Ask an agent to do something later ("check the build in 10 minutes and tell me") or on a schedule, and it wakes up when it is time and answers where you asked: in the DM, the thread, or back to the agent that asked. Works for Claude and Codex and survives restarts. Pending reminders appear in Channels with a Cancel button.

Start a new agent session after updating so it learns the reminder commands. The database upgrades in place to schema 7.

## [1.5.0-exp.13] - 2026-11-04

### Added

- Direct messages. A "Direct messages" section in the sidebar lists every Namedrop agent in every workspace, with the workspace's channel next to each name so same-named agents are easy to tell apart. DMs stay out of channels, threads, and All activity.
- You can see when an agent runs subagents. Its member chip and Peek header show how many are running.
- If an agent answers a Channels message only in its own terminal, that answer now appears in the thread or DM, marked "From @name's terminal", and the conversation is flagged as needing you. Typed Mentions in panes work as before.

## [1.5.0-exp.12] - 2026-11-04

### Changed

- Thread replies now appear only in their thread. The channel shows the top message with a reply count and the latest reply.
- Your own message in a thread resets the Automatic Reply Limit and lifts a pause it caused. Agent-to-agent exchanges still pause after 10 replies.

### Fixed

- Live Peek fills its frame. It grows text to use the full width, especially in Expand, and keeps the newest line visible after resizing.
- Live Peek no longer flashes blank while Codex streams, and its watch history no longer repeats or drops lines. Lines that truly can't be recovered show a visible gap marker.

## [1.5.0-exp.11] - 2026-11-04

### Added

- Typing `/` or `$` in the Channels message box or Peek's Type to agent box suggests the addressed agent's skills. Claude and Codex have separate lists, and choosing a skill inserts it the way that agent expects.

## [1.5.0-exp.10] - 2026-11-03

### Added

- Live Peek keeps text that scrolls by while you watch. “Watching since” shows when that history started, and a quick close and reopen keeps it.
- Live Peek passively loads available shell history, fits the drawer width, and has Expand and Restore controls for more room. Type to agent stays available in either size.

## [1.5.0-exp.9] - 2026-11-03

### Fixed

- Namedrop commands no longer fail with "database is locked" while agents are busy.

### Added

- "Type to agent" in Live Peek lets you submit prompts and send Esc, Ctrl-C, arrow keys, and Enter.

### Changed

- Open Channels with `namedrop channels`. The Herdr action uses this command. `namedrop dashboard` remains a hidden compatibility alias with the same flags and output.

## [1.5.0-exp.8] - 2026-11-03

### Fixed

- Live Peek opens at the latest text or cursor row, above unused blank rows. Mouse wheel, trackpad, keyboard, and scrollbars move the local view, including wide and tall terminal projections.
- Scrolling up pauses following while output arrives. Returning to the latest output resumes following across full paints, source resizes, and reconnects.
- Keyboard scrolling stays responsive while live output arrives.

Live shows the source terminal's current projected screen. It does not recover earlier host scrollback. Update Namedrop, then refresh Channels and reopen Peek. Existing agent sessions can continue.

## [1.5.0-exp.7] - 2026-11-03

### Added

- Peek opens a live, read-only terminal projection with colors, cursor motion, and TUI redraws. Text snapshot keeps the sanitized text view and manual Refresh. Live sends no terminal input and does not resize the source terminal.
- Live reconnects retain target continuity and stop on observed binding or terminal changes. Closing Peek, disconnecting, or stopping the dashboard cleans up its passive observer, including quiet terminals.

Existing experimental installations can use Update Namedrop, then refresh or reopen Channels and Peek. This update does not require new agent sessions.

## [1.5.0-exp.6] - 2026-11-02

### Added

- Channels and conversation sidebars resize by dragging or keyboard, keep the channel readable, and remember their widths locally.
- Thread replies start with editable participant mentions. Choose one or several agents, or invite another channel agent into the same thread. Removing every recipient keeps the reply unsent until someone is selected.

### Fixed

- Rows inside a thread no longer offer Reply in thread. Timeline rows keep the action, and Show more still expands messages inside the thread.

## [1.5.0-exp.5] - 2026-11-02

### Fixed

- The updater's version check fetches only Git metadata and the version files it needs. It no longer downloads unrelated repository contents, including demo videos, which could exhaust its two-minute timeout on slower connections.

## [1.5.0-exp.4] - 2026-11-02

### Fixed

- Review grants bind to the exact review request and committed message. Replaying a request cannot renew its grants or attach them to an unrelated operator message.

### Added

- Handoff warnings in Channels say in plain words what happened and what to do next: blocked, cancelled, stuck, lost its lead, or finished without a delivered report. Details shows the saved report as Markdown.
- Ask to review sends one handoff to its lead, or to an agent you pick when the lead's pane is closed. Review all sends every open handoff to one agent. Each press grants that agent's current session one acknowledgement of each named handoff. A grant is used once, expires after 15 minutes, and ends when the dashboard session changes. The agent clears a handoff with `namedrop handoff acknowledge-granted`, and Channels shows who cleared it.
- Handoff warnings share one collapsible row, "N handoffs need a decision", which starts closed when there are more than two. Your choice to open or close it is kept per channel, and the sidebar uses the same wording.

## [1.5.0-exp.3] - 2026-11-02

### Added

- Channels group workspaces by repository in the sidebar. The server sends each space a repository id and name, never a path.
- Show more loads a shortened message's whole body, up to 64 Ki characters, from the authenticated `GET /w/<key>/messages/<id>/body`, with the same redaction as frames. Every message offers Reply in thread, which answers that message and keeps it as the target until the reply is sent.

### Fixed

- `namedrop name`, `address list` and agent hooks no longer fail with `state_busy: database is locked` on a large state file while other agents' hooks run. Opening the state no longer scans the whole file or takes the write lock when the schema is current, so hooks of live agents cannot hold up a command's write. A damaged file is still reported with backup guidance by the first command that reads it.

## [1.5.0-exp.2] - 2026-11-02

- Agents receive session guidance for Markdown in reply and send bodies, including file input for formatted JSON. Channels render Markdown in those messages.

## [1.5.0-exp.1] - 2026-11-02

Experimental Channels build for Linux and macOS testers.

### Fixed

- Dashboard entry links keep their query parameters and browser fragments after redemption. Channels use server-provided addressability reasons and alias-free member attention text. Recently closed members remain available for Peek for five minutes, with existing history references retaining them afterward.
- Queued terminal commands continue after early dashboard responses, including back-to-back posts and bursts, without an unrelated drain. Forwarded mailbox workers start before the source worker is awaited.
- Dashboard schema checks retry only SQLite Busy or Locked errors for a bounded five-second budget and log contention. Other read failures retain the exit or handoff behavior.
- Operator dashboard posts return after the message and request ID commit, before agent delivery finishes. Delivery continues in the source and forwarded workspaces even if the client disconnects.
- Dashboard delivery and unanswered-request warnings follow the recipient's current alias after a rename. Attention links load in one query instead of preparing a query for each warning.
- Channel routing treats code, quoted instructions, and multi-step operator requests as normal post content. Measured routing policy uses plain JSON for Claude, structured output for Codex, and a 17-second deadline. A separate bounded dashboard journal records routing attempts and available usage, including failed calls.
- Reopening Peek in the channels app shows the last successful screen and its original read time while a fresh full read runs. Retained screens stay in memory for the current app instance. Missing, closed, and session-ended failures replace earlier screen text with their notice.

### Added

- The Channels app opens from the workspace action **Open Namedrop Channels** or `namedrop dashboard`. Installed builds include the app, so using it does not require Node or npm.
- Dashboard channels accept plain top-level posts. An isolated small-model router parses embedded mentions and chooses only the members addressed, without changing the text. Posts without known mentions make no model call. Plain operator thread follow-ups reach the thread's agents, while terminals need an explicit leading mention. Channel replies can wake local agents through embedded mentions, with bounded context and the existing reply allowance. Set `NAMEDROP_CHANNEL_ROUTER=off` to keep plain root posts unrouted. Routing explanations use a dashboard sidecar so the ledger stays at schema 6.
- Channels attention items link to their conversations and offer a prefilled response or a named pane to inspect. Historical conversation links load their target directly, including requests outside the live message window.
- The dashboard serves sanitized recent pane output at authenticated `GET /w/<key>/participants/<address_id>/screen` for agents and terminals. Concurrent requests for one pane share a bounded Herdr read. Retired or closed panes return 410.
- Screen reads use unwrapped output and redact keys split by invisible characters or tabs. Oversized screens keep the newest text with an omission marker. Followers stop waiting after four seconds, and a 16-request peek limit reserves capacity for other dashboard routes.
- Secret redaction preserves emoji and script formatting outside secrets, and keeps ENV values protected when a tab splits the variable name.
- Message and Peek redaction recognize private-key headers with invisible characters next to spaces, and tokens split by ANSI sequences.

- The dashboard can post as the local operator through authenticated `POST /w/<key>/messages`, with session-bound CSRF protection and optional thread follow-ups. Agent replies settle without an operator pane and appear in one persistent human lane. `GET /api/session` supplies the CSRF token for custom apps.

- Dashboard attention entries include nullable `subject_alias` and `message_id` fields, and terminal runs include the request's nullable `message_id`. Links remain available for requests older than the live frame window.

- The dashboard serves authenticated, paged channel history at `/w/<key>/messages`, including messages older than the live frame window.

- `namedrop dashboard --ui <dir>` serves a custom browser app from a directory under the dashboard's existing security headers. Rebuilt directories are read fresh, large assets stream with bounded memory, dotfiles stay private, and aliases of the app shell require the session cookie. Ancestors need only search permission; the UI directory needs read and search permission. The shared server keeps its UI setting through upgrades, supports handoff to older releases without claim inheritance, falls back to their embedded UI when they lack `--ui` with a logged explanation, and spawns the exact executable it probed. During a handoff, matching UI requests wait up to five seconds for a ready successor and apply its UI setting before returning a URL. Longer startups return restart guidance without a URL, initially different UI requests receive restart guidance immediately, and updates and package refreshes can proceed while the successor starts.

- The dashboard's `/api/events` endpoint streams every live Herdr workspace over one authenticated connection. It sends changed workspaces as `space` events and closed workspaces as `gone` events. Dashboard frames now include a live `display_name` and a nullable `addressable_label` that identifies a workspace only when its label is unique.
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
