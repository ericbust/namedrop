# Changelog

Each release is a section here, newest first. The GitHub release for a version uses its section as the release notes. See [releasing](docs/releasing.md) for how versions are chosen.

## [1.8.1] - 2026-11-12

### Fixed

- The Channels link on your phone comes back by itself after the machine restarts. With tailnet access on, the Channels server was gone after a restart, a Herdr restart or a crash, and the link failed until someone ran `namedrop channels` again. Herdr now starts the server again when Herdr starts, and when a pane opens, moves or closes. It comes back on the same port, and a device that was signed in stays signed in. Herdr has to be running, so after a restart with nobody logged in the link stays down until you open Herdr.
- `namedrop channels` no longer says the server stops after 10 minutes when it doesn't. With tailnet access on, the server has kept running with no tab open since 1.5.0, but the command still printed "The server stops 10 minutes after the last tab closes". It now prints the rule in force, such as "The server keeps running while Tailscale access is on."
- Doctor reports a tailnet link whose server is not running (ND-TAILSCALE-001), and gives the command that starts it, `namedrop channels --no-open`. Before, it reported the link as configured while it was down. If another program has taken the server's port, Doctor names the port, and Herdr tries to start the server once a minute, not on every pane event.
- With tailnet access on, `namedrop channels --ui <dir>` switches a running server to another Channels app. It used to refuse and tell you to kill the server. The server now restarts on the same port with the app you asked for, and signed-in browsers reconnect. Without tailnet access, the command still names the running app and the `kill` command.

### Added

- `namedrop channels --idle <minutes>` sets how long the server keeps running after the last tab closes, and `--idle never` keeps it running. The setting is saved and applies until you change it, across restarts and updates. Without it, the server stops after 10 minutes, as before. The setting has no effect while tailnet access is on, where the server never stops for idleness. Doctor shows a saved setting as ND-CHANNELS-001, and reports one it cannot read.

## [1.8.0] - 2026-11-11

### Added

- You can send images to an agent from Channels. Paste an image into the message box, drop it there, or pick it with the new image button beside Send. Each image shows as a small thumbnail above your text, with a button to remove it. After you send, the thumbnails show under the message in the thread, and a click opens the full image. This works in channels, threads and direct messages, on a phone, and over the tailnet link.
- A message takes up to 4 images of up to 10 MB each. PNG, JPEG, GIF and WebP are accepted. Channels reads the type from the file's bytes, not its name, and refuses anything else. A message with images still needs some words, and Send stays off until you type them.
- The agent gets one line per image at the end of your message, `Image: ` and the file's absolute path. Claude Code and Codex open it with their ordinary file tools. No image data travels in the message, so an image costs the agent's prompt one short line until it opens the file.
- Claude Code opens the image without asking. It normally asks before it reads a file outside its working directory, so activation now adds one read rule for the image directory to Claude Code's settings, beside the command permissions Namedrop already manages. Deactivating the last worktree removes it. If Claude Code asks anyway, as it can right after an update, answer in its pane once and reactivate Namedrop.
- Images are stored under `channel-images/` in Namedrop's state directory, readable by you only, and named by the SHA-256 of their bytes. The same image sent twice is stored once. An image is deleted 30 days after it was last attached, when Channels starts or when the next image is stored. A thread then shows "Image no longer stored" in its place.
- A message with no image is sent and shown as before.

## [1.7.9] - 2026-11-11

### Added

- `namedrop channels --tailscale --no-open` now draws the one-time tailnet link as a QR code in your terminal, with the link under it. Scan the code with your phone to open Channels there, with no link to copy. The code is black on white, so it scans on dark and light terminal themes. It appears only when you run the command yourself in a terminal: output to a file or a pipe, JSON output, and an agent running the command get the link alone. A terminal too narrow for the code gets the link and the width to use.

## [1.7.8] - 2026-11-10

### Fixed

- A mention you type in a Claude Code pane gets its answer again. Claude Code puts a prompt a hook blocked back into the prompt box, and a mention Namedrop sends is such a prompt. Since 1.7.7, Namedrop read that leftover mention as a draft you hadn't sent, so the answer and every later message to that agent waited until you cleared the box, and Doctor reported a draft in the way (ND-DELIVERY-005).
- Namedrop now deletes a mention it already sent from the box before it types the next message, so the message arrives as a prompt of its own. Before 1.7.7, the message was typed behind the leftover mention, and the agent got both as one prompt. Namedrop deletes the text only while the box holds exactly the mention it sent and you have sent nothing since. If you change that text or type after it, it is your draft and the message waits as before. A mention too long for the box to show in full is also left alone, and the message waits until you clear the box.
- The mention is deleted with Ctrl+U, the key that deletes a row of Claude Code's prompt box. Namedrop reads the box before each few presses and stops as soon as it holds anything but the mention, so words you start typing meanwhile stay. If the mention stays, Namedrop types nothing, and Doctor tells you to clear the box (ND-DELIVERY-005, `held_by: sent_prompt`).

## [1.7.7] - 2026-11-10

### Fixed

- A message to a Claude Code agent no longer waits while background agents it started are still running. Once the agent's turn ends, Namedrop types the message in within seconds. Before, Herdr reported such an agent as working until its background agents finished, so the message could wait 10 minutes or more while the agent sat ready for input. A turn that is still running still gets nothing until it ends. A mention the agent's prompt box sends starts no turn and doesn't hold the message. A prompt another plugin's hook blocks or stops counts as a started turn, so the message waits for the agent's next turn to end or for its background agents to finish. When another plugin's Stop hook runs longer than 3 seconds, the message waits for the background agents as before.
- Namedrop no longer types a message into a Claude Code or Codex prompt box that holds a draft you haven't sent. Before, a message to an idle agent was typed in at your cursor, and the agent got your draft split around the message as one prompt. The message now waits, and goes in within about 10 seconds after you send or clear the draft, without waiting for the agent's next turn or for its background agents to finish. A box you switched to shell mode by typing `!` counts as a draft.
- Doctor now explains a message that has waited more than a minute for its agent (ND-DELIVERY-005). It says what Herdr reports for the agent, or that a draft in its prompt box is in the way.

## [1.7.6] - 2026-11-10

### Fixed

- Channels no longer says an agent didn't answer your message while Namedrop holds its answer, not even for a moment. Namedrop holds an answer while background work the agent started is still running, or while the agent waits on a helper it asked. Before, the agent going idle raised "@name didn't answer a request" about 30 seconds after its turn ended, and sometimes for a moment right as the turn ended, even though the answer came once the work finished. Your message now reads "Still working. Its answer will follow." until the answer comes.
- When you sent the message yourself, you now see what the agent said at once. Before, a held answer meant nothing in the thread until the work finished. Now the agent's last words from that turn appear in the thread right away, in reply to your message, marked "Progress update. Not the final answer." The note doesn't close your message or count as a reply, and the real answer still follows in the same thread. Messages between agents get no note.
- You're warned only when a held answer can never arrive, and the warning says why. Channels shows "@name can't answer this anymore" when you closed its pane, the agent was removed, or a new session took its name, and "@name stopped before answering" when the agent exited in its pane or its pane has been gone for 10 minutes, as when Herdr restarted and the agent wasn't resumed. If that same session comes back, for example when you resume it, the warning goes away and the agent can still answer. After you review the warning, `delivery acknowledge` clears it.
- If the turn that should carry a held answer ends without one, you get the ordinary warning that the agent didn't answer.
- A request is now flagged as unanswered even when the agent's turn ended without the hook that records its end, as when you interrupt the agent with Esc or an API error ends its turn. Before, Channels showed nothing until the agent's next prompt. Now Namedrop waits 5 seconds after the agent goes idle for that hook, then shows the ordinary warning. If the hook ran but Namedrop's worker never finished with it, the warning comes 30 seconds after the hook.
- A pane Namedrop loses track of for a moment, as while Herdr restarts, no longer flashes a warning that the agent can't answer, and no notification goes out for it. If the pane is still gone 10 minutes later, the warning comes then, with no further event needed. If your computer was asleep then, the warning comes when Channels next starts. Closing that pane later doesn't remove the name, so its aliases and messages stay. A pane you move to another workspace keeps its session, which can still answer a message from you, so no warning comes while it stays there. If you close it there, the warning comes 10 minutes later.
- A lost answer's card in Channels has a Dismiss button. It asks once, then clears the warning as `delivery acknowledge` does, and your message then reads "Ended without an answer". If the warning was already cleared, because you dismissed it elsewhere or the answer came, the card says so and goes away on the next update.
- Agents no longer read progress notes. The recap in a later delivery, a thread's context snapshot and the channel router leave them out, which also saves tokens. The real answer still reaches them.
- A "didn't answer" warning that an earlier version raised on an answer that is still held is cleared when Channels starts. If that answer can no longer arrive, the warning is reworded to say why.
- `delivery acknowledge` now also clears the warning for a delivery whose agent exited in its pane, or whose pane has been gone for 10 minutes. Before, it was refused until a new session appeared on that name.

## [1.7.5] - 2026-11-10

### Fixed

- A reminder's clock no longer quits when Doctor checks on it at the moment it starts. Before, the reminder then waited with no clock until your next Namedrop command, so it could arrive late.

## [1.7.4] - 2026-11-10

### Fixed

- Updates no longer pile up old copies of Namedrop. Each activation kept every version it had installed, under `runtime/releases/` in the state directory, so a machine that updated often collected dozens. After a successful activation, Namedrop now deletes all but the active version, the one before it, and any version a running Namedrop process still uses. Updating to 1.7.4 removes the old copies that nothing uses.

## [1.7.3] - 2026-11-10

### Fixed

- An agent's waiting line is no longer sent as its answer when its subagent stops with background work of its own still running. Claude Code reports that stop in a notice it marks as possibly interim, and Namedrop took the notice for the subagent finishing, so the next final message closed your message and the real report had no way back. The answer now stays held through such a notice and comes from the final message of the turn that carries the subagent's final report.

## [1.7.1] - 2026-11-10

### Fixed

- An agent that still owes answers to many messages from earlier turns is now shown the answer line for three of them, oldest first, then how many more are open and the `delivery inbox` line that prints them. Before, each one added its own line of about 500 characters to every turn, so around 20 of them pushed hook context past the 10,000 characters Claude Code keeps whole.

## [1.7.0] - 2026-11-09

### Changed

- A delivery now types a short pointer into the pane instead of its whole envelope: `Namedrop message <id> from <sender>`, a line to run if no details arrive, and the message as one JSON string. The prompt hook supplies the rest. In the real-agent test, each delivery added 39–46% fewer context tokens than in 1.6.5, about 320 to 700 fewer. Live Peek shows the short prompt.
- Your own posts read `from the person` in that header, not by an internal name.
- A pointer is good for one dispatch. Its id is random, a re-dispatch gets a new one, and the hook adds details only the first time it sees it and only when the whole prompt matches. A whole envelope authenticates only for a delivery from before this upgrade, so pasting what `delivery inbox` prints does not re-open one.
- The roster gives way to keep hook context under the 10,000 characters Claude Code keeps whole. A delivery's details and commands always come through. The note about other workspaces is dropped first, then the roster rows, and anything dropped comes again on a later turn. Details and commands that are themselves over the limit still go to Claude Code's preview and file.
- `delivery inbox` works from inside the Codex sandbox without an approval, so a Codex session whose hook didn't run can read a delivery's details directly. It also says when a delivery is held because Namedrop couldn't confirm it reached the pane.
- After compaction, resume or clear, a session is told which deliveries it still owes and how to print them.
- A coordination nudge with many assignments keeps its settling lines and says when some lines didn't fit. The rest stay available through `delivery inbox`.

### Fixed

- An update no longer waits on Namedrop's status worker. The worker held the command lock for as long as it ran, and one that kept retrying a status event could run for days, so the update gave up after 20 seconds and blamed a popup. It now holds the lock only while it handles an event, and after an update it hands its queue to the new version.
- An update now stops a status worker from 1.6.5 or earlier that holds the command lock, because that worker held it for as long as it ran, retries included, and starts it again on the new version with a wake, so a retry it was waiting on still happens. Its queued work stays on disk, so nothing is lost and you don't have to do anything. Helpers of this version hold the lock only while they work, so the update waits for them instead. It never stops an agent or any other process.
- When something else holds the command lock, the update now names it by PID and Namedrop command and says how to free it, instead of always pointing at a popup.
- On macOS, an update no longer fails with "Could not finish inspecting older Namedrop commands" on a busy machine. It now reads running processes directly instead of waiting on `ps`.
- Namedrop's status worker no longer gets stuck on one event. A worker runs in the directory of the hook that started it, and once that directory was removed, for example a plugin checkout replaced by an update, every event failed the same way and was retried every few seconds for as long as the worker lived, with every later event waiting behind it. The worker no longer depends on where it started. An event that keeps failing through 10 minutes of retrying, not counting time the machine slept, is now set aside so later events run, because a later event checks the same pane again, and the mailboxes drain once more afterwards. A wake that carries a delivery's retry is never set aside.
- Doctor now shows a status event the worker is still failing on, with its error (ND-STATUS-001), and one it set aside in the last week (ND-STATUS-002). Before, the worker's errors were recorded nowhere.

### Upgrade

- This release raises the state schema to 8. The update migrates it live behind the command lock, so agent sessions and pending deliveries carry on. Keep the matching executable and a database backup together in case you roll back.
- Close any open Namedrop popup, such as the tour, before you update. An open popup makes the migration wait, then ask you to retry.
- Codex sessions started before the update lose their hooks, so restart them. Until you do, such a session reads each delivery through the prompt's `delivery inbox` line and can answer with the reply line it prints, which takes about 3 model calls instead of 1. An answer it gives only as its final message is lost, and ND-DELIVERY-003 tells you so.

## [1.6.6] - 2026-11-09

### Fixed

- An agent's answer in its final message is no longer held when the session has background work its turn didn't start, such as a long-running command or monitor from earlier, or Claude Code's own memory upkeep. Before, every such answer was held, ND-DELIVERY-003 opened, and the answer never reached Channels.
- When an agent starts background work for your message and ends its turn, its answer now comes from its final message in the turn that work's notice starts. If the notice arrives while the agent works on something else, or another message reaches the turn it starts, the agent is shown the line to answer your message, naming the work it waits on, and the final message of a turn about something else is never sent as the answer. Before, Namedrop sent nothing until the agent ran its reply line.
- An agent's answer to one of your messages no longer stops its final-message answer to another of your messages in the same conversation. Before, when the agent answered an earlier message during a later one's turn, the later answer was dropped.

## [1.6.5] - 2026-11-09

### Fixed

- A release that changes nothing a running session uses no longer changes any byte of the installed plugin. Codex refuses a running session's hooks when any byte differs, so these updates no longer strand Codex sessions or ask you to approve the hooks again. Upgrading to this version changes the plugin once, so restart your running Codex sessions this one time. Until you do, Doctor can't name them, and tracked work there raises no warning.
- When Codex refuses a session's hooks, tracked work now warns you too. Before, an answer lost this way raised no warning while the handoff stayed open.
- ND-DELIVERY-003 now says when Codex refused the session's hooks and tells you to start a new session.
- A reply or close that a sandboxed agent queued now warns you if the outside worker later refuses it. Before, the agent saw `pending` and nothing reported the refusal.

### Added

- Doctor shows ND-SETUP-005 and names the agent when Codex is refusing a running session's hooks, before any answer is lost. It knows this only for a session that has run a hook on this version or later, so it can't name the sessions already running when you upgrade to this version.

## [1.6.4] - 2026-11-09

### Fixed

- Agents can now see what another agent said when you ask them. If you start a post with a mention and name another agent later, for example `@codex what did @claude say?`, the agent you asked gets recent channel history with your post. That history includes the named agent's earlier conversations with you. Before, the agent could tell you it saw nothing even though the answer was on your screen.
- Channel history no longer shows agents a post you withdrew, a post still waiting to reach its recipient, or a reminder. Before, those could appear in the history an agent got with a channel post.

### Changed

- The named agent's latest answers now come first in that history, so channel chatter can't push them out. A line that was cut ends with `…`.

A mention that names nobody else adds nothing to the prompt, so it costs no extra tokens.

## [1.6.3] - 2026-11-08

### Fixed

- An agent's answer now arrives when it names an agent that isn't here, or one that was retired. Before, its final message stayed unsent and you got ND-DELIVERY-003 saying the agent ended without a reply.
- An agent's answer now arrives when its final message opens with a name, as in `@codex said pong`. It goes only to whoever asked, and every word stays, even when the name is the asker's. You see the name as plain text in Channels and notifications. An agent's prompt shows `\@codex`, the way Namedrop writes a name that wakes nobody.
- When a final message still can't be sent, the warning says why, and so does the agent's next prompt, along with the line that lets it answer. This now covers tracked work too, which used to show nothing, including when Namedrop's Stop worker dies, runs out of time or can't read its claims. A warning no longer opens on a delivery nobody can answer, and one you acknowledged stays closed.

### Changed

- A reply from an agent that names someone who isn't here now keeps that name as plain text instead of being refused. This covers replies an agent runs itself as well as its final message. A new message an agent sends still refuses a name it can't find, so a typo there still shows.
- A `reply` an agent runs itself that opens with names keeps the usual addressing rules. Each is a recipient and an unknown one is refused, so the agent sees the error and nothing is lost.

## [1.6.2] - 2026-11-08

### Fixed

- An agent's final message now shows in Channels as a normal reply. It no longer opens a red "terminal reply needs you" item for every answer, and it's no longer labelled as coming from the agent's terminal.
- Other agents in the conversation can read those answers now. Before, an agent asked what another agent said could tell you it saw no reply.
- The red items that 1.6.0 and 1.6.1 left on answers clear on their own the next time Channels starts, which includes the restart after an update.
- Your message now reads "Replied" when an agent's final message answered it, in DMs, in threads and when you asked an agent in another workspace. Before, it read "Done, no reply needed" right above the answer.

Text that a release before 1.6.0 copied from an agent's terminal keeps its label and its item, and stays hidden from other agents. Warnings about undelivered or unanswered messages work as before.

### After you update

- Restart any Codex session that was already running. Codex refuses to run a plugin's hooks once the plugin's files have changed, so that session can lose its hooks, answer only in its terminal and raise ND-DELIVERY-003. Its screen shows `plugin contents changed; hook approval required`. Start a new session and trust the Namedrop hooks at **Hooks need review**.

## [1.6.1] - 2026-11-08

### Fixed

- If your browser is signed in to Channels, re-opening a used or expired link now opens the page instead of showing a 401. A browser that isn't signed in still gets the 401, and a new link still signs in only once.
- Channels over Tailscale no longer signs you out on day seven while you keep using it. The sign-in now lasts seven days after you last opened or reconnected Channels.
- The sign-in error now says your agents and messages are fine. It also gives you ways to sign in when you have no terminal nearby.

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
- An agent's final message is now its answer to a delivery. Agents no longer run a reply or close command after each delivery, which cuts each delivery turn from two model calls to one. A turn that ends while waiting on a helper or background work sends nothing, and the agent is reminded on later prompts how to answer.
- Your messages in Channels get the agent's final message as the answer. The old terminal relay's time window is gone, so a status line written while waiting is no longer posted as the answer.
- A `reply` after Namedrop closed a delivery on its own is sent as a follow-up in the same thread. Other conflicts get a plain sentence instead of a database error.

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
