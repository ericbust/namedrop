# Rex adapter and bridge design

This document is the design for steps 4 and 5 of the [Rex host plan](PLAN.md), and for the cross-host relay that follows them. Step 4 adds a Rex adapter behind the `Mux` port that [PORT.md](PORT.md) built in step 2. Step 5 adds the bridge, one long-lived process per Rex server that turns Rex events into Namedrop's lifecycle work. The relay carries a mention from a Namedrop on one machine to a Namedrop on another, and it ships later as its own PR series ([Part 2](#part-2-the-relay)). Commits 0a, 0b, 1, 2, 4, 5, and 6 of the plan merged into `experimental` in PR #196 (`280779db`). Nothing else is implemented yet.

The design is for the agent who implements these steps, and for the person who reviews them. An agent in a Rex block gets the same promise an agent in a Herdr pane has today. Namedrop types into it only when the agent Namedrop verified is the live program and reports `idle` or `done`, and it never sends a prompt twice. Herdr behaves exactly as it does today, including Herdr running inside a Rex block.

This is revision 4, patched after a fourth review. Revision 3 (`487f760`) went to a third adversarial review, which asked for a narrow revision first. Revision 4 (`5901edb`) went to a fourth, which found it ready to implement once nine items were patched in, with no further design round. [Review 4 and how it was answered](#review-4-and-how-it-was-answered) and [Review 3 and how it was answered](#review-3-and-how-it-was-answered) map each finding to its change. The tables for reviews 3, 2, and 1 keep the earlier answers and mark the ones a later revision replaced.

The evidence comes from live probes on 2026-11-12 and 2026-11-13 with Rex `0.1.0-preview.14` (API revision 12), Herdr 0.9.5, Claude Code 2.1.323 on Haiku, Codex CLI 0.163.0 (native, with and without `--no-daemon`), Codex CLI 0.142.2 through its npm launcher, and Docker images of Debian and Alpine. Probes used scratch Rex sessions, scratch Rex servers, a scratch `CODEX_HOME`, and an isolated Herdr server with its own socket, config, and home. Every scratch session was killed, every scratch server and Codex app server was stopped, and nothing started `herdr` against the user's server. [Evidence](#evidence) gives the commands and the trimmed output. Each claim says whether it was measured, inferred, or guessed.

The design branch is `exp/rex-adapter-design`, cut from `origin/exp/mux-delete-bridge` (`013c129`). Step 3 is final at `e68deeb`, whose last code change is `17c580d`, and it merged into `experimental` in PR #195 (`f71ca69`). This revision designs against that head. Commits 0a to 6 merged after it in PR #196 (`280779db`), so commit 6a and every later commit start from `experimental`, which holds both.

## Review 4 and how it was answered

The fourth review found revision 4 ready to implement once these items were patched in. Its measures are its own, summarized in [Evidence](#review-4s-measures), and this patch ran no new probes.

| # | Finding | Answer |
|---|---|---|
| 1 | High. The test suite run inside a Rex block resolves a Rex home against the developer's real server | Fixed in the plan. The test suite spawns the binary at 81 sites in 29 files, and most keep the parent's environment with a fake `HERDR_SOCKET_PATH` (`tests/inherited_pane.rs:115`, `tests/naming_popup.rs:131`, `tests/runtime_reliability.rs:141`). That path is `ENOENT` on an explicit path, so the home table's third row answers Rex. New commit 13a routes every test spawn of the binary through one helper that clears `REX_BLOCK`, `REX_SESSION`, `REX_CLIENT`, and `REX_SERVER`, with a boundary test. In commit 14, unit tests reach `ThisProcess` only through a scripted `Inherited`, and the done-when runs the full suite once from a Rex block |
| 2 | High. A zombie agent counts as alive | Fixed. Alive means present and not a zombie, defined once in `mux/processes.rs`, which reads a zombie as no process. Linux's `/proc/<pid>/stat` keeps a zombie's group and start, so the state field decides. macOS reads with `proc_pidinfo`'s `arg` 0, which fails on a zombie, where `arg` 1 returns it. Commit 9 adds a `pane_agent` row test and a Docker test with a real zombie, and commit 10 a re-read test |
| 3 | Medium. A nested agent reached through shells only, with another executable path, still binds | Fixed. Condition 5 of `bind_session`: when the hook's agent is not the foreground, the hook refuses if the block's binding already names the foreground's incarnation as its own agent. A hook whose agent is the foreground always replaces the binding. Condition 6 refuses a Claude Code hook whose `CLAUDE_CODE_ENTRYPOINT` is not `cli`. Lands in commit 17 |
| 4 | Medium-low. "`port_ansi` `None` makes delivery read `Unseen` instead of guessing" is wrong, because `Unseen` with `idle` or `done` delivers | Corrected, and decided. A Rex screen fault (a read error, or `port_ansi` answering `None`) holds the mail and retries instead of delivering. Herdr's rule does not change. Lands in commit 11. That an agent can report `done` over a dialog Namedrop does not recognize is shared with Herdr and recorded under [Open questions and risks](#open-questions-and-risks) |
| 5 | Low. `shell_of` accepts a non-interactive shell | Fixed. When the candidate shell is the foreground, `shell_of` reports no shell if the foreground's process group has any other member |
| 6 | Low. A stale record refuses the bind | Fixed. A record whose owner is not the foreground counts as absent in condition 3, as the survey reads it |
| 7 | Doc facts | Corrected. Codex 0.163.0 has no `mcp-server` command. A record existed at SessionStart in 5 of 6 of review 4's runs, so the text says a record may not exist. The probe recipe links `Library/Keychains` into the scratch `HOME` for Claude Code on macOS. Rex requires `updated_at` on a status record, and the fixtures carry it |
| 8 | Plan wording | Corrected. Commit 15 tests that a Herdr set of names ignores `NAMEDROP_STATE_DIR`. Commit 16's live check uses a terminal recipient, because an agent recipient needs commit 17's binding |
| 9 | Part 2, record only. The pointer's `@bob@laptop` can name another machine on B | Recorded under [Open questions and risks](#open-questions-and-risks) |

None of the findings changes commits 0a to 6, which merged in PR #196.

## Review 3 and how it was answered

Rows marked † were revised again in answer to review 4.

| # | Finding | Answer |
|---|---|---|
| 1 † | A launcher that outlives its agent defeats the stale-record rule | Fixed. The binding holds the hook agent's own `<pid>@<start>` beside the foreground's. The survey, the gate's facts, and the re-read after the paste each require that process alive, with that start time, in the foreground's process group. Otherwise the block is a free terminal whose shell does not hold the foreground, so nothing is typed. Re-probed with Claude Code behind a `bash` wrapper that does not `exec`. The gate still delivers to the live agent, a `kill -9` before the facts declines, and a `kill -9` between the facts and the paste presses no Enter. The line then sat unrun at zsh's prompt after the wrapper ended, and its marker file never appeared |
| 2 † | The group test binds a nested agent started by an MCP server | Fixed. When the hook agent is not the foreground itself, the foreground must be its ancestor, every process between them must be a shell, and the foreground's executable must differ from the agent's. The hook wrapper passes its runtime (`--runtime claude-code` or `codex`), and a record that exists must name that `app`. Re-probed with three MCP servers under Claude Code: a Python server, a Python server that runs `sh -c`, and a server that is itself `sh -c`. Every nested `claude -p` hook was refused, and every outer agent kept its binding. `codex mcp-server` under Claude Code, which only the runtime test refuses, is inferred |
| 3 | A sandboxed Codex's reply, ack, and complete do not defer | Fixed. For an explicit Rex endpoint, `try_defer` keeps today's body. It connects to `Endpoint::unix_socket` and defers on `EPERM` or `EACCES` with the flags' space and pane. Commit 18's live check adds a rendered `reply` |
| 4 | Relay keys collide in the main database | Fixed. The main database's request id is `relay:<A host_id>:<A message id>:<recipient>`. `remote_threads` is keyed by (A's `host_id`, A's thread, B's space) |
| 5 | Relay text reaches the terminal unvalidated, and the paste rule covers only `\n` | Fixed. A Rex paste turns every control character into a space. A terminal `Line` that holds `\n` or `\r` is `Refused` with nothing typed. B validates `from.name` and `to` with `validate_alias`, `from.host` with the label rule, bounds `id`, `thread`, and `in_reply_to`, and caps `text` itself |
| 6 | The mailbox wake and the Stop worker lose their ancestry and get no location | Fixed. The UserPromptSubmit hook, which proves its home while its agent runs, starts the wake through `daemon::command` with `Scope::Space`, as the dashboard does. The installer stops backgrounding the wake in the wrapper. The Stop hook writes its space into `CapturedStop`. `space_for_wake` and the `REX_SESSION` exception are deleted |
| 7 | `relay:remote` is more than rows | Answered with the smaller shape, a paneless relay author per remote sender, recognized by one predicate, with four changed store sites, all in [H] commit R3. [Relayed messages have a paneless author](#relayed-messages-have-a-paneless-author) lists every operator site and why each other one does not change. Routing through the operator or through envoys was read and rejected |
| 8 | A's follow-up has no producer | Specified. Both sides run one `relay::capture`. On B it turns a message to the relay author into an outbox reply. On A it turns one into an outbound item to B, in reply to the B message it answers. So a conversation continues across hosts while only A can reach B |
| 9 | One commit behind step 3 | Fixed. A Herdr home reads the state directory exactly as `17c580d` does. It reads `HERDR_PLUGIN_STATE_DIR` and never `NAMEDROP_STATE_DIR` |
| 10 | Relay hardening (Low) | Fixed. The bridge checks the principal before anything else and stores nothing for a refused one. It refuses unknown principal kinds. Anonymous principals need a file in B's state directory, not an inherited variable. A sends B's expected `host_id`, and a different server refuses. `inbound` rows expire after 7 days. The one-spawn fallback after a removed label and the reach of a self-reported `host_id` are stated |
| 11 | Plan | Fixed. Commits 23 and R2 to R5 are [H]. `handle_mux_event` (commit 20) lands before the dispatch that calls it (commit 21). Every module whose first caller lands later stays `#[cfg(test)]` until then. Commit 16 migrates `src/main.rs:1140-1146`, and its boundary test reads production code only. Commit 13 lists the clock's re-exec |
| 12 | Which leading-run readers take the host list | Answered. Only `send`'s parse takes it, through `parse_mentions_in`. `parse_prompt_mentions`, `carries_leading_mentions`, `literal_leading_mentions`, and every reply path keep today's grammar. In any home, a body's `@bob@devbox` is a context mention of local `bob`, as today |
| 13 | A live Herdr at a guessed default path that is not an ancestor yields Rex | Fixed. On a guessed default path, a peer that is not an ancestor is `Doubt`, so the home is Herdr |
| Deferral list | Could be deferred | Applied in full. Delivery acks, `namedrop relay list`, request batching, the two-tier poll, the cached bridge client id, `prove_local`'s file check, `scripts/runtime.py` (to step 6), and Doctor's wording for a changed `host_id` are deferred, and `space_for_wake` is gone. The relay is [Part 2](#part-2-the-relay), its own PR series after commit 24. The home table, `daemon::command`, the install record, `RexDeferred`, `port_ansi`, and coalescing stay |

None of the findings changes commits 0a, 0b, 1, 2, 4, 5, or 6. The paste rule (finding 5) and the re-read (finding 1) change commit 10's `paste_submit`, and the binding's new fields change commit 9's read side and commit 17. Each lands after commit 6. The review of `exp/rex-adapter-1` adds commit 6a, which fixes `rex/control.rs` and the `--inside-rex` lane before commit 7, and a `seq` rule in commit 10. A status change at or below the paste ack's `seq` happened before the paste, so it stops the Enter and never confirms.

## Review 2 and how it was answered

Rows marked † were revised again in answer to review 3.

| # | Finding | Answer |
|---|---|---|
| 1 | The relay hands an unvalidated host to `rex -S` | Fixed. A remote recipient's host must equal, byte for byte, a label that `rex hosts list --json` prints in the same environment as the relay's later calls, and it may hold only ASCII letters, digits, `-`, and `_`. Any other host fails the send at once, with an error that names the label and lists the known ones. Every relay `rex` call passes `--autostart=false` and `-S <label>`, never an address. Measured: `-S nosuchlabel` dials three addresses, a label hides an address of the same spelling, `-S` matches labels ignoring case, and `-S host:<id>` is refused as a bad address |
| 2 | Replies cannot route by `host_id`, and a laptop gets no replies from a devbox that cannot reach it | Redesigned. B never calls A. B keeps replies and delivery acks for A in a durable outbox keyed by A's `host_id`, and A collects them over its own outbound calls, in the result of every send and by a bounded poll while it has open remote threads. A learns B's `host_id` from a live `server.status` and pins it on every item it sends. The accept result carries `can_reply`. Outbox items expire after 7 days, and Doctor reports the expired ones. Measured on the stand-in, with B listing no host for A |
| 3 † | Codex's default sandbox cannot reach Rex, and the deferral path is not migrated | Fixed. A denied peer probe (`EPERM` or `EACCES`) is its own home row. With no Herdr name, the home is `Home::RexDeferred`, a place that only `try_defer` accepts, proved by the block's session binding: the bound incarnation must be an ancestor with the same start time. `try_defer` and `agent_status.rs:137` take the home. A boundary test fails when anything outside `mux::home` and `locator` reads the three location names. Measured under Codex 0.163's default sandbox: the socket connect fails `EPERM`, and the ancestry and the ancestors' start times read |
| 4 | `hook_agent` refuses every hook on Linux | Fixed. A process is a shell by the basename of its executable (`sh`, `bash`, `zsh`, `dash`, `ksh`, `fish`, or `busybox`), never by `comm`. A Linux incarnation is `<pid>@<boot id>:<start ticks>`, so a binding cannot survive a reboot. Measured in Docker: a `#!/bin/sh` wrapper has `comm` `session-start-w`, and its executable is `dash` on Debian and `busybox` on Alpine. A Docker test of the real process reads lands with the binding |
| 5 | `shell_of` calls a non-shell program the shell | Fixed. `shell_of` returns a pid only when the candidate's executable is a shell, so a block made with `-- python3 -i`, an editor, or `ssh` has no shell and never gets a terminal line |
| 6 † | "The paste then sits in the shell's line unsubmitted" is false without bracketed paste | Fixed, and the claim is corrected. A Rex paste turns each newline into a space. Measured: the one-line text stayed unrun at the prompt of bash 3.2, `sh`, zsh, and dash with no Enter. Claude Code's and Codex's UserPromptSubmit hooks received the pasted line byte for byte, and `pointer::strip_matching`'s rule matched it to the three-line prompt |
| 7 | `daemon::command` is under-specified | Specified. Each daemon has a scope kind. The clear list is the 13 `AGENT_SESSION_MARKERS`, `CLAUDE_SESSION_ID`, `REX_BLOCK`, `REX_SESSION`, `REX_CLIENT`, `REX_SERVER`, and both timeout overrides. An endpoint-scoped daemon gets no space or pane name under either set. The boundary test checks every self-spawn of the Namedrop executable, not `process_group(0)`, and lists its exceptions with reasons |
| 8 † | Herdr-visible changes are ungated or avoidable | Fixed. `mux::home` walks processes itself, so `terminal::parent_pid` and `verify_inherited_pane` do not change, and revision 2's commit 3 is dropped. `ECONNREFUSED` is doubt, as is `ENOENT` on a guessed default path (measured: a live listener with a full queue refuses at once). Every commit that touches a Herdr path is [H], including `handle_mux_event` and the grammar. Relay state lives in its own `relay.sqlite3` beside the main database, so the main schema and older runtimes are untouched. `@name@host` is a recipient only in a Rex home and only for a listed host. Under Herdr, `@a@b` reads exactly as today, and a test pins both of today's readings. The state directory claim is corrected |
| 9 † | Relay protocol gaps | Fixed. The dedup key is (sender `host_id`, message id, recipient), measured with two recipients. A name that two of B's spaces hold is refused as ambiguous. Every Namedrop outcome travels in the action's result, because a client can answer only `not_supported`, `refused`, or `internal` as an error (Rex's docs), and the outcome table covers every Rex code. Dedup runs before any refusal and replays the stored outcome. A slow accept answers `retry`. The request goes on stdin. A per-host relay worker sends, not the status worker. Both queues expire |
| 10 † | Binding refuses an agent behind a launcher, and default Codex is unmeasured | Fixed and measured. A hook binds when its nearest non-shell ancestor is in the block's foreground process group. The npm launcher (`node`) is Rex's child and foreground, and the native `codex` and its hooks share the launcher's group. Claude Code's tool shell, and a `claude -p` it runs, have their own group. Codex's default mode runs every session's hooks in a background app server with its own group, so it never binds. It stays unsupported, as on Herdr, where Namedrop's existing notice already tells the person what to do |
| 11 | Plan | Rewritten. Revision 2's commit 11 is now five commits (13 to 16, and 18). A live-check lane (commit 7) comes before the first live check that needs agents with hooks. The Rex modules stay out of the library build until a production caller exists. `mux::connect` gets its Rex arm in commit 11, after `RexMux` implements `Mux` |
| Low † | Five items | The tap's boundary is the first facts response's `seq`. A `Resync` against a server whose `host_id` changed retires nothing. Home probes are non-blocking and bounded at 20 ms each. A hold after the paste leaves the pointer in the agent's box, and the next mail waits as a draft. `port_ansi` is an allowlist |

None of the findings changes commits 0a, 0b, 1, 2, 4, 5, or 6. Revision 3 narrows commit 0a's text to what is being built (the `vt` captures and the `prompt_box` test). The other fixtures land with the commit that first reads them.

## Review 1 and how it was answered

Rows marked † were revised again in answer to review 2.

| # | Finding | Answer |
|---|---|---|
| 1 † | A Herdr-spawned process with no pane picks Rex, so Herdr's reconcile runs against the outer Rex session | Fixed at the root. `mux::home` picks the multiplexer whose *server* is the nearest ancestor, proved by the peer pid of each claimed socket, with no request. Any doubt about Herdr picks Herdr. Measured on an isolated Herdr inside a scratch Rex block: a pane, a workspace action, a pane action, and the startup command all resolve Herdr. The testbed gets a Herdr-in-Rex lane. Revision 3 gives `mux::home` its own process walk instead of changing `parent_pid`, and treats `ECONNREFUSED` as doubt |
| 2 † | `bind_session` accepts a nested agent's hook | Fixed. Revision 3's rule binds a hook only when its nearest non-shell ancestor is in the block's foreground process group. A nested `claude -p` runs in its tool shell's group and is refused (measured). Revision 4 adds the launcher and runtime conditions, because an MCP server's nested agent shares the outer agent's group (review 3) |
| 3 | Rex `vt` output breaks the Codex prompt-box read | Fixed in `RexMux::screen`, which hands the port the same shape Herdr does. The real parser on six captures misreads two raw and reads all six right once each `ESC[nC` is n spaces |
| 4 | The kind does not travel in the value | Fixed. Every endpoint value, from any flag spelling, variable, or envelope, goes through `Endpoint::parse`. `Locator::pass` writes the set that matches the kind. Rex lines and envelopes use the neutral names |
| 5 † | Daemons inherit a pane and a session | Fixed. Every daemon starts through `daemon::command`, which [Daemons start with no identity](#daemons-start-with-no-identity) specifies. Rex has no session-vouched path at all |
| 6 | `locate_caller` cannot find a non-default server | Answered. Namedrop finds a server the way the `rex` CLI does, `REX_SERVER` then the default socket, and proves it by ancestry. A server that does not export `REX_SERVER` to its blocks also misroutes Rex's own CLI, so Namedrop refuses and names the variable |
| 7 † | `shell_pid = child.pid` is wrong | Fixed. The shell is the process just below `login` in `foreground` plus `ancestors`. Revision 3 counts the candidate only when its executable is a shell |
| 8 † | Hooks write one state directory and the CLI reads another | Fixed. A Rex home reads `NAMEDROP_STATE_DIR`, then the install record, which holds the directory the installer bakes into the hook wrappers, then a default |
| 9 | A killed session keeps its names, and Rex errors retry for 10 minutes | Fixed. `session_destroyed` becomes `MuxEvent::SpaceClosed`. A `Resync` that finds the space gone retires it too. Rex markers classify failures by `MuxError`, not by Herdr's JSON codes |
| 10 † | Hooks that never reach `mux::home` | Fixed. Revision 3 counts eleven reader sites, listed under [Every identity site takes the home](#every-identity-site-takes-the-home), and a boundary test keeps the list closed |
| 11 † | Claims | Corrected. The race window is the agent's own submit latency. `ps -Eww` does show environments. A data connection sends `process_changed`, and `capture` and `format` do not. Revision 3 measured Codex's default mode instead of inferring it |
| Shape | Transport, deferrals, fixtures, base, manual wakes, live entry points | `ws.rs` writes over `Read + Write`. The conformance suite and backward `blob_read` windowing are deferred. Step 4's live checks use `reconcile` as the manual wake |
| Remote | In scope | Typing stays on the block's machine. The relay uses Rex's own authenticated connection through `client.perform`. Revision 3 makes replies a pull |

## Problem

Rex runs agents in blocks and reports their status, but it differs from Herdr in eight ways that shape this design.

- **Rex reports no agent session** (measured). Namedrop's own SessionStart hook is the only witness to which session runs in a block. `SessionClaim::Unreported` has no producer yet, and `resolve_session` answers `None` for it.
- **A status record can outlive its program** (measured). After `kill -9` of Claude Code under a shell, Rex kept `{state: idle, owner: {pid: <dead pid>}}` while zsh held the terminal. Behind a launcher that does not `exec`, the record names the launcher, which is still alive. A gate that trusts the record would paste a mention into a terminal nobody reads and press Enter.
- **The gate needs one connection with an order** (measured). The tap, the facts, the paste, and the Enter must share one `rex.control.v1` connection, whose `seq` numbers order events against responses. A `rex` CLI process per call costs about 20 ms, and a `rex events` tap carries no `seq`.
- **Rex has no plugin host** (measured). Lifecycle events reach Namedrop only through a process that holds the event stream open.
- **Each multiplexer passes the other's variables through** (measured). A Rex server passes an inherited `HERDR_PANE_ID` into its blocks. A Herdr server passes the outer `REX_BLOCK` into its panes and its plugin processes. On the measured machine the everyday Herdr runs inside a Rex block, so every Herdr process carries a Rex claim that verifies.
- **A Rex block's shell runs under a root-owned `login`** (measured). On macOS, the call `terminal::parent_pid` uses fails on `login`, so a walk with it never reaches the Rex server.
- **A sandboxed agent has no names to defer with** (measured). Codex's default sandbox denies a tool's connect to the Rex socket, as it denies Herdr's. Herdr's deferral takes the location from Herdr's names, and a Rex block has none.
- **A Rex server can be on another machine** (from Rex's docs and the stand-in). One server can be reached through a Unix socket, a forwarded socket, TCP, or the tailnet, and a block's program runs on the server's machine. Its pids mean nothing anywhere else.

These constraints carry over from step 2. Herdr behavior does not change, and every existing test passes. No heavy dependency is added. The port traits in `src/mux/ports.rs` stay. `gate::emulate_prompt` owns the gate order and the ambiguity rule. `PaneInput` stays the only door for input.

## Usage

Delivery does not change. `delivery.rs` keeps its one `match` on `InputOutcome`, and Rex is invisible to it. The candidate check still compares the stored session with `pane.agent_session_id()`, and the Rex survey decides whether that comparison holds.

```rust
// delivery.rs, unchanged apart from the deleted `admit` field.
let expect = pane.agent.as_ref().expect("live candidate checked");
match effects.prompt(runtime.mux, &owner, PromptRequest { expect, text: &prompt }) {
	InputOutcome::Delivered => submitted(),
	InputOutcome::Declined(retry, _) => requeue(retry), // nothing was typed
	InputOutcome::Refused(_) => failed(),
	InputOutcome::Ambiguous(_) => held(),               // never sent again
}
```

The runtime builder asks `mux::home` which multiplexer this process runs in before it reads any location name. A Herdr home runs today's body with step 3's Herdr set. `main.rs` names no Rex type.

```rust
// main.rs
fn with_runtime_options<T>(options: &RuntimeOptions, action: impl FnOnce(&Runtime<'_>) -> Result<T, String>) -> Result<T, String> {
	match mux::home::resolve(&options.flags(), &mux::home::ThisProcess)? {
		Home::Herdr => with_herdr_runtime(options, action), // today's body, step 3's names
		Home::Rex(place) => {
			let state = locator::state_directory(options.plugin_state.clone(), MuxKind::Rex).ok_or_else(state_required)?;
			let mux = mux::connect(&state, &place.endpoint, Budget::ADAPTER_DEFAULT, Arc::default())?;
			action(&LifecycleRuntime {
				workspace_id: place.space.as_ref().map(SpaceId::as_str),
				current_pane_id: place.pane.as_ref().map(PaneId::as_str),
				session_socket_id: place.endpoint.key(), // "rex:/Users/me/Library/Application Support/rex/server.sock"
				mux: &mux,
				..common_runtime_parts(&state)
			})
		}
		Home::RexDeferred(_) => Err("the agent's sandbox denies the Rex socket, and only bound commands can wait for the deferred worker".into()),
	}
}
```

Every hook that today reads its location from the environment takes it from the home instead.

```rust
// stop_hook.rs, prompt_hook.rs, agent_status.rs, and main.rs's subagent hook
let Ok(Some(here)) = mux::home::resolve(&Flags::none(), &mux::home::ThisProcess).map(|home| home.location()) else {
	return; // silent, as every hook path is
};
// here.endpoint, here.space, here.pane replace Locator::Endpoint.env(), ::Space.env(), ::Pane.env()
```

The context hook binds the session before it builds the runtime, so the runtime's first survey already reports it. The installer writes the wrapper's runtime into its command line (`hook session-start --state-directory <dir> --runtime claude-code`), because a nested agent's hook cannot be told apart by its environment, which it inherits.

```rust
// main.rs run_context_hook, for SessionStart and UserPromptSubmit
let home = mux::home::resolve(&Flags::none(), &mux::home::ThisProcess)?;
if let (Home::Rex(place), Some(session), Some(runtime)) = (&home, hook_session_id(&input), args.runtime) {
	let _ = place.bind_session(state, &session, runtime); // refuses unless this hook's agent is the block's foreground program
}
// After a UserPromptSubmit that sent a mention, the hook starts the wake itself,
// because it can still prove its home: daemon::command(.., Scope::Space(endpoint, space)).
```

The bridge translates and enqueues. It never calls lifecycle or drain code. The existing status worker does that, as it does for Herdr.

```rust
// rex_bridge.rs, for each coalesced change
status_queue::enqueue(state, &endpoint, &space, &Marker::Event(event))?;
status_queue::spawn_worker(state, &endpoint, &space)?; // daemon::command with Scope::Space

// main.rs, the status worker's one dispatch
match Marker::parse(&input)? {
	Marker::Event(event) => namedrop::handle_mux_event(runtime, &event), // typed failures
	Marker::Wake => namedrop::drain_mailboxes(runtime).map(to_value),
	Marker::DraftRecheck(held) => namedrop::recheck_drafts(runtime, held).map(to_value),
	Marker::Herdr(data) => run_lifecycle("event.agent-status-changed", data, ..), // today's shape and classifier
}
```

Adapter tests script a private `Wire`, as `herdr/tests.rs` scripts `Transport`, and pin the requests each port call sends.

```rust
#[test]
fn a_prompt_reads_the_facts_on_the_tapped_connection_then_pastes_rechecks_and_presses_enter() {
	let rex = scripted(|call, events| match call.method {
		"server.status" => Ok(status(4680, "/tmp/r.sock")),
		"target.resolve" => Ok(block_in("session:s", "block:b")),
		"session.attach" => Ok(json!({})),
		"program_status" => Ok(json!({"records": [root("idle", "claude-code", 41, "2.1.323")]})),
		"process" => Ok(fixture("process/claude-foreground.json")),
		"block.event" if is_enter(&call) => { events.status("block:b", "working"); Ok(json!({})) }
		"block.event" => Ok(json!({})),
		_ => Err(rex_error("not_found")),
	})
	.peer(4680)
	.started(41, 1_700_000)
	.bound("block:b", bound_to("41@1700000").agent("41@1700000").runtime("claude-code").session("session-1"));
	assert_eq!(rex.send(&PaneId::new("block:b"), prompt("hello\nworld", &claude("session-1"))), InputOutcome::Delivered);
	assert_eq!(calls(&rex), ["server.status", "target.resolve block:b", "session.attach session:s",
		"program_status block:b", "process block:b", "block.event paste hello world", "process block:b", "block.event key enter"]);
}
```

## Shape

### Module map

| Path | Owns |
|---|---|
| `src/mux/home.rs` | `Home`, `Place`, `Deferral`, `Location`, and `resolve`. Which multiplexer this process runs in, from flags, the claimed sockets, and ancestry. `Place::bind_session` |
| `src/mux/processes.rs` | `LocalProcesses` and its real impl: the parent, process group, executable, and start time of a local process, and `is_shell`. macOS uses `PROC_PIDT_SHORTBSDINFO` and `proc_pidpath`, Linux `/proc` and the boot id. A zombie reads as no process. `mux::home` and `rex/binding.rs` read through it. `terminal.rs` is untouched |
| `src/mux/rex.rs` | `RexMux`, `connect`, the `MuxRead`, `MuxAct`, and `MuxInput` impls, and the effect and key mapping |
| `src/mux/rex/ws.rs` | A client-side WebSocket over any `Read + Write`: the HTTP upgrade, masked frames, 7-, 16-, and 64-bit lengths, fragments, ping and pong, and close. No compression and no new crate |
| `src/mux/rex/control.rs` | The private `Wire` and `Control` traits and their production impl. Hello and welcome, request ids, the event buffer, the per-operation budget and byte count, cancellation |
| `src/mux/rex/local.rs` | `prove_local`: the peer pid of a socket, `server.status`, and the canonical endpoint. The only code that decides whether a server is on this machine |
| `src/mux/rex/wire.rs` | Rex JSON parsed into domain types, Rex error codes mapped to `MuxError`, method names, and `port_ansi` |
| `src/mux/rex/agent.rs` | `pane_agent`, the pure liveness rule that the survey and the gate both call, and `shell_of`, the shell from a `process` reply |
| `src/mux/rex/channel.rs` | `RexChannel` and `RexTap`, the `gate::RawChannel` and `gate::StatusTap` impls |
| `src/mux/rex/binding.rs` | Session bindings under `<state>/rex/sessions/`, the incarnation read, `hook_agent`, and the launcher rule |
| `src/mux/rex/events.rs` | `RexEvents`, the bridge's stream, and `classify`, the pure decode table from a Rex event to a `MuxEvent` |
| `src/mux/rex/fixtures/` | Captured `vt` screens (commit 0a), then `process` replies, hook chains with process groups, event lines, and the home table, each added by the commit that first reads it |
| `src/mux/rex/tests.rs` | Scripted-`Wire` tests that pin the requests for every port call |
| `src/mux/rex/live.rs` | `#[ignore]` tests that drive `RexMux` against the scratch server named by `NAMEDROP_REX_LIVE` |
| `src/rex_bridge.rs` | The bridge loop, its lock, coalescing, `ensure`, `run`, and the relay action it declares |
| `src/relay.rs` | Part 2. `@name@host` validation against `rex hosts list`, `relay accept` on B, the per-host relay worker on A, the capture step on both sides, and outcome mapping. Rex homes only |
| `src/relay/store.rs` | Part 2. `relay.sqlite3`: peers, outbound items, collected items, inbound dedup keys, B's outbox, and remote threads, with its own schema version |
| `src/daemon.rs` | `daemon::command` and `daemon::scrub`, through which every background Namedrop process starts |

Changes outside these files:

| Path | Change |
|---|---|
| `src/mux/model.rs` | `Endpoint::parse`, `Endpoint::unix_socket`, and `REX_SCHEME`. `Budget::tighter` and `narrowest`, moved from `herdr::transport` |
| `src/mux.rs` | `connect` and `Connector`. `mod rex` is test-only until commit 11 |
| `src/mux/gate.rs` | `TapChange`. A program change declines before the paste and is `Ambiguous` at once after it |
| `src/mux/input.rs` | `Admit` and `PromptRequest.admit` deleted |
| `src/mux/event.rs` | `MuxEvent::Resync(SpaceId)`, `MuxEvent::SpaceClosed(SpaceId)`, and serde for every variant |
| `src/lifecycle.rs` | `pub fn handle_mux_event`, its `EventError`, and the `Resync` and `SpaceClosed` handlers. Attribution names the endpoint's multiplexer |
| `src/status_queue.rs` | `Marker`, `enqueue`, and `spawn_worker`, moved out of `main.rs` |
| `src/locator.rs` (step 3) | `state_directory` takes the home's kind. `Locator::pass` takes the endpoint's kind. The install record |
| `src/install.rs` | Writes the install record with the directory it bakes into the hook wrappers. The SessionStart and UserPromptSubmit wrappers pass `--runtime`, and the UserPromptSubmit wrapper no longer backgrounds `hook mailbox-wake`. The Stop wrapper also sets `NAMEDROP_REX_TIMEOUT_MS=250` |
| `src/integration.rs`, `src/health.rs`, `src/terminal.rs` | `tool_environment`, `registration_command`, Doctor's repair line, and `runner_command` write the neutral names for a Rex endpoint. Herdr bytes do not change. `terminal::parent_pid` does not change |
| `src/main.rs` | The runtime builder and the identity sites listed under [Every identity site takes the home](#every-identity-site-takes-the-home). The context hook's binding. The `rex bridge` and `relay` subcommands. The worker's `Marker` dispatch. Every self-spawn moves to `daemon` |
| `src/stop_hook.rs`, `src/prompt_hook.rs`, `src/agent_status.rs` | Their endpoint, space, and pane come from the home. `CapturedStop` carries the Stop hook's space to its worker |
| `src/deferred_commands.rs` | `try_defer` takes the home and accepts `Home::RexDeferred`. `execute_request` and the four liveness probes use `mux::connect` and `Endpoint::unix_socket` |
| `src/reminder/clock.rs`, `src/dashboard*` | `mux::connect` or `mux::Connector` in place of `herdr::connect` and `HerdrConnector`. Live peek refuses a Rex binding |
| `src/agent_status_model.rs` | The summary worker's scrub drops `REX_*` beside `HERDR_*` and `NAMEDROP_*` |
| `src/codex_app_server.rs` | The shared app-server notice names Rex when the server inherited Rex names and no Herdr name. Herdr's text is unchanged |
| `src/message.rs` | Part 2. `parse_mentions_in`, which reads `@name@host` in the leading run when `send` in a Rex home passes its host list. `parse_mentions` keeps today's behavior for every caller |
| `src/store/relay_author.rs`, `src/store.rs`, `src/store/operator.rs` | Part 2. `ensure_relay_author`, `is_relay_author`, and `insert_relayed_message`, following `src/store/operator.rs`, and the two changed sites in [Relayed messages have a paneless author](#relayed-messages-have-a-paneless-author). New rows, no schema change |
| `testbed/` | The Herdr-in-Rex lane (commit 0b) and the Rex live lane (commit 7) |

### The endpoint string carries the kind

A stored endpoint is a `session_socket_id`. A Rex endpoint is `rex:` followed by the absolute path of the server's primary Unix socket, as the server itself reports it. No stored Herdr value starts with `rex:`, because Herdr values are absolute paths, `default`, or empty. So every stored Herdr value parses as Herdr, byte for byte, and no migration runs.

```rust
// src/mux/model.rs
pub const REX_SCHEME: &str = "rex:";

impl Endpoint {
	/// The one reader of an endpoint value: a session_socket_id, an envelope's
	/// NAMEDROP_ENDPOINT or HERDR_SOCKET_PATH, or the --endpoint field, which
	/// --herdr-socket also fills. The name that carried the value never decides.
	pub fn parse(value: &str) -> Self {
		let kind = if value.starts_with(REX_SCHEME) { MuxKind::Rex } else { MuxKind::Herdr };
		Self { kind, key: value.to_owned() }
	}

	/// The socket a liveness probe connects to: the Herdr path, or the path after `rex:`.
	pub fn unix_socket(&self) -> Option<&Path> { todo!() }
}

// src/mux.rs, from commit 11. Commit 8 answers MuxError::Unavailable for MuxKind::Rex.
pub fn connect(state: &Path, endpoint: &Endpoint, budget: Budget, cancelled: Arc<AtomicBool>) -> Result<Arc<dyn Mux>, MuxError> {
	match endpoint.kind {
		MuxKind::Herdr => Ok(herdr::connect(Some(endpoint.key()), budget, cancelled)),
		MuxKind::Rex => rex::connect(state, endpoint, budget, cancelled),
	}
}
```

**The canonical form comes from the server, not from the address that reached it.** One server has several spellings on one machine: `REX_SERVER`'s percent-encoded URL, the default path, `/tmp` and `/private/tmp`, a symlinked directory, and a forwarded socket. Each spelling would split the store namespace, the status queue directory, the pane lock, and the bridge lock. So the endpoint is the address of the listener that `server.status` marks `primary`, and every spelling that reaches the server converges on it. A forwarded socket and the server's own socket reported the same primary listener and the same `instance_id` (measured).

```rust
// src/mux/rex/local.rs
pub(crate) struct LocalServer { pub endpoint: Endpoint, pub pid: u32, pub host_id: Option<String> }

/// Proves that `dial` is served by a Rex server on this machine, and names it canonically.
/// 1. The peer pid of the connection (LOCAL_PEERPID on macOS, SO_PEERCRED on Linux) equals
///    `server.status.pid`. A forwarder's peer is the forwarder (measured).
/// 2. The primary listener is a Unix socket, and its address becomes the endpoint.
/// Anything else is "not a local Rex server", and Namedrop types nothing through it.
/// Matching `dial` to a listener by device and inode is deferred: the peer pid already
/// rejects every forwarder measured.
pub(crate) fn prove_local(dial: &Path, control: &mut dyn Control, peer: u32) -> Result<LocalServer, Fault> { todo!() }
```

Each port call checks the peer pid of its fresh connection, which costs nothing. It asks `server.status` only when that pid differs from the last proven one, as after a restart.

### One module decides where a process runs

```rust
// src/mux/home.rs
pub enum Home {
	/// Today's path. The Herdr runtime builder runs unchanged, with step 3's
	/// Herdr set of names and verify_inherited_pane.
	Herdr,
	Rex(Place),
	/// An agent's sandbox denied the Rex socket, and the block's binding proved
	/// the caller. Only deferred_commands::try_defer accepts it.
	RexDeferred(Deferral),
}

pub struct Place {
	pub endpoint: Endpoint, // canonical, from prove_local
	pub space: Option<SpaceId>,
	pub pane: Option<PaneId>,
	/// The block's process facts from the verifying `process` read. Present only
	/// when an inherited REX_BLOCK verified, so only then can a hook bind.
	caller: Option<Caller>,
}

/// A place read from a binding file. It can queue a deferred command and nothing else.
pub struct Deferral { pub endpoint: Endpoint, pub space: SpaceId, pub pane: PaneId }

/// Where a hook records its turn: endpoint, space, and pane, from either home.
pub struct Location { pub endpoint: Endpoint, pub space: Option<SpaceId>, pub pane: Option<PaneId> }

/// What this process inherited: its environment, its ancestors (through
/// mux::processes), and the result of probing a socket. `ThisProcess` reads the
/// real ones, and tests script them.
pub trait Inherited { /* var, processes, probe */ }

impl Home {
	/// Herdr: step 3's Herdr set, read as the hooks read it today. Rex: the place.
	/// `None` when the home names no endpoint, and for RexDeferred.
	pub fn location(&self) -> Option<Location> { todo!() }
}

/// 1. An explicit endpoint value decides by Endpoint::parse, with no probe. With a Rex
///    endpoint, --space and --pane are trusted as --herdr-pane is today, and the place
///    has no caller, so it cannot bind. try_defer probes its socket itself.
/// 2. With REX_BLOCK and REX_SESSION set, the nearest server ancestor decides (below).
///    A Rex home reads its place from the verified claim and ignores every HERDR_*
///    and NAMEDROP_* location name.
/// 3. Otherwise step 3's locator::home runs: a HERDR_* location name means Herdr, and
///    else the neutral set, whose NAMEDROP_ENDPOINT value is parsed for its kind.
pub fn resolve(flags: &Flags, inherited: &dyn Inherited) -> Result<Home, String> { todo!() }
```

**The nearest server ancestor.** Each claim names a socket. Rex's is `REX_SERVER`, then Rex's default socket. Herdr's is `HERDR_SOCKET_PATH`, then Herdr's default, and Herdr is asked only when a `HERDR_*` location name is set. For each socket, Namedrop makes one non-blocking connect, reads the peer pid, and closes, with no request. A probe has four results.

| Probe result | When |
|---|---|
| `Peer(pid)` | The connect succeeded. The walk then says whether `pid` is an ancestor, and how many parents up. On a guessed default path, a `pid` that is not an ancestor is `Doubt` instead |
| `NoServer` | `ENOENT` on a path that `HERDR_SOCKET_PATH` or `REX_SERVER` named explicitly |
| `Doubt` | `ECONNREFUSED`, `EAGAIN`, `ENOENT` on a guessed default path, a live server on a guessed default path that is not an ancestor, a path too long for a Unix socket, 20 ms without completing, or any other error |
| `Denied` | `EPERM` or `EACCES`, as an agent's sandbox answers |

`ECONNREFUSED` is doubt because a live listener whose queue is full refuses at once. On macOS, a listener with a backlog of one that never accepts refused five more connects in under 0.01 ms each (measured). The peer probe took 5 µs at the median and 53 µs at most over 50 tries, and neither server logged anything for it (measured), so the 20 ms bound is never reached in practice and leaves the Stop hook's 250 ms for its own work.

| Rex probe | Herdr | Home |
|---|---|---|
| `Peer`, an ancestor at depth r | No `HERDR_*` name | Rex |
| `Peer`, an ancestor at depth r | `Peer`, an ancestor at depth h | Rex when r < h, else Herdr |
| `Peer`, an ancestor at depth r | `Peer` that is not an ancestor on a path `HERDR_SOCKET_PATH` named, or `NoServer` | Rex |
| `Peer`, an ancestor | `Doubt` or `Denied` | Herdr |
| `Peer` that is not an ancestor, `NoServer`, or `Doubt` | A `HERDR_*` name is set | Herdr, today's path |
| `Peer` that is not an ancestor, `NoServer`, or `Doubt` | No `HERDR_*` name | An error that names `REX_BLOCK` and `REX_SERVER`. Never Herdr, so a Rex process never runs `herdr` |
| `Denied` | A `HERDR_*` name is set | Herdr, today's path, which defers through Herdr's names |
| `Denied` | No `HERDR_*` name | `RexDeferred` when the block's binding proves the caller (see [Sandboxed commands defer through the binding](#sandboxed-commands-defer-through-the-binding)), else an error that says the sandbox denied the socket |

So any doubt about Herdr is resolved as Herdr, which keeps every Herdr process on today's path. The third row also reaches Namedrop's own test suite. Most integration tests spawn the binary with the parent's environment and a fake `HERDR_SOCKET_PATH` such as `test-socket` (`tests/runtime_reliability.rs:141`), which is `ENOENT` on an explicit path. Run from a Rex block, such a test inherits a verifying `REX_BLOCK`, and its binary would resolve a Rex home and attach to the developer's real session, and from commit 23 `ensure` would start a bridge there. Commit 13a clears the four `REX_*` names from every test spawn before commit 14 reads them. The prototype took 0.07 to 0.10 ms per decision in Python, with no request sent (measured). Measured with an isolated Herdr server inside a scratch Rex block, with Rex blocks that carried stale Herdr names, and with a Codex tool under its default sandbox:

| Process | Rex server | Herdr server | Home |
|---|---|---|---|
| A shell in a Herdr pane | depth 2 | depth 1 | Herdr |
| A Herdr workspace action | depth 1 | depth 0 | Herdr |
| A Herdr pane action | depth 1 | depth 0 | Herdr |
| A Herdr startup command | depth 1 | depth 0 | Herdr |
| A block of a Rex server started from a Herdr pane | depth 1 | live, not an ancestor | Rex |
| A Rex block whose `HERDR_*` names a live Herdr elsewhere | depth 1 | live, not an ancestor | Rex |
| A Rex block whose `HERDR_SOCKET_PATH` names no socket | depth 1 | `NoServer` | Rex |
| A plain Rex block | depth 1 | no name | Rex |
| A Codex tool in a plain Rex block, default sandbox | `Denied` (`EPERM`) | no name | `RexDeferred` (the rule's answer for the measured denial) |

**Verifying the block.** When the table says Rex, one connection runs `server.status` for the local proof, `session.attach`, and `process` for `REX_BLOCK`. The claim verifies when the block's `child` is an ancestor of this process. Otherwise the answer is an error naming the block, as `verify_inherited_pane` refuses an unverified Herdr pane today. That is one fresh connection with three requests, about 1 ms (inferred from the 0.8 ms measured for a connection with two).

**`mux::home` walks processes itself.** In a default Rex block the chain is the Rex server, `/usr/bin/login` running as root, the login shell, and the program. `proc_pidinfo(PROC_PIDTBSDINFO)`, which `terminal::parent_pid` calls, fails on the root-owned `login`. `PROC_PIDT_SHORTBSDINFO` reads it and also gives the process group (measured). `mux::processes` uses the short call, and `terminal::parent_pid` keeps the old one, because the short call reads 217 more processes on this machine and would let `verify_inherited_pane` accept chains through root-owned parents (review 2's measure). On Linux, `/proc/<pid>/stat` already reads root processes.

**Rex finds servers as `rex` does.** A block on a server started without `REX_SERVER` in its environment gets no `REX_SERVER`, and the `rex` CLI in that block talks to the default server (measured in the spike). Namedrop does the same lookup, so in that block it finds no ancestor server and refuses with "REX_BLOCK=… is on a Rex server this process cannot find; start that server with REX_SERVER set". A server started with `REX_SERVER` naming itself passes it to every block (measured).

**Kind-scoped names.** The home is chosen before any location name is read. A Rex home takes its place from the verified claim, from the flags, from a binding, or, for a daemon, from the neutral set Namedrop gave it. It never reads a `HERDR_*` location name, so a stale outer Herdr cannot redirect it. Nothing puts `NAMEDROP_*` names into a block's environment, so in a block the inherited identity comes only from `REX_*`.

**Work that outlives its hook gets its place from the hook.** Two processes run after the hook that started them has exited, so they cannot prove a block by ancestry. Each now takes its place from a hook that proved it while its agent ran.

- **The mailbox wake.** Today the UserPromptSubmit wrapper backgrounds `hook mailbox-wake` with `&` and exits (`src/install.rs:1183`). Instead, the UserPromptSubmit hook starts the wake itself after it sent a mention, through `daemon::command` with `Scope::Space(endpoint, space)`, as the dashboard does (`src/dashboard/http.rs:2290`). The installer stops writing the backgrounded line. `hook mailbox-wake` reads its home like any process. A Herdr home runs today's body. A Rex place with a space runs `schedule_mailbox_workers`. A wrapper installed before this change still backgrounds its own wake, which does the same under Herdr, so Herdr wakes once or twice and drains the same space either way.
- **The Stop worker.** `stop_hook::run` reads its endpoint and space from the home and writes both into `CapturedStop`. `wake_waiting_mail` takes the space from there instead of `Locator::Space.env()` (`src/stop_hook.rs:236`). Under Herdr both values come from the Herdr set the hook and its worker share today, so they do not change.

**The state directory.** Step 3 leaves the default to step 4, for a Rex home only.

```rust
// src/locator.rs (step 3), with the home's kind
pub fn state_directory(flag: Option<PathBuf>, home: MuxKind) -> Option<PathBuf> {
	// flag, then:
	// Herdr: HERDR_PLUGIN_STATE_DIR, and no default. Step 3's body at 17c580d, unchanged: a Herdr
	//        set never reads NAMEDROP_STATE_DIR.
	// Rex:   NAMEDROP_STATE_DIR, then the install record, then $XDG_STATE_HOME/namedrop
	//        (or $HOME/.local/state/namedrop). A Rex home never reads HERDR_PLUGIN_STATE_DIR.
	todo!()
}
```

The install record is `$XDG_CONFIG_HOME/namedrop/state-directory` (or `~/.config/namedrop/state-directory`), one line holding the same absolute path the installer bakes into the hook wrappers with `--state-directory`. `integration install` writes it beside the wrappers, through a temp file and a rename. So the hook that binds a session and the CLI that surveys the block open one directory, spelled the same way. Doctor reports a record that differs from the wrappers' directory. A Rex daemon gets the directory as `NAMEDROP_STATE_DIR`, because `Locator::pass` writes the set of the endpoint's kind for the state directory too. Rex status workers share that directory with Herdr's. Teaching `scripts/runtime.py` to stop and wake them by their neutral names moves to step 6, with the installer that first puts a Rex worker on a machine.

### Daemons start with no identity

Every background Namedrop process starts through `daemon::command`, so none of them can take an identity from the process that started it.

```rust
// src/daemon.rs
pub enum Scope<'a> {
	/// The bridge, the deferred worker and executor, the reminder clock, the relay worker, and `relay accept`.
	Endpoint(&'a Endpoint),
	/// The status worker, the dashboard's mailbox wake, and the prompt hook's mailbox wake.
	Space(&'a Endpoint, &'a SpaceId),
	/// The dashboard server.
	None,
}

/// A detached background Namedrop process: process_group(0), null stdio, `scrub`,
/// and `scope` written with Locator::pass for the endpoint's kind. Every location
/// name the scope leaves out is cleared under both sets with Locator::clear.
pub fn command(executable: &Path, scope: Scope<'_>) -> Command { todo!() }

/// The scrub alone, for the Stop worker, which stays in its hook's process group.
/// Clears REX_BLOCK, REX_SESSION, REX_CLIENT, REX_SERVER, the 13 AGENT_SESSION_MARKERS
/// (src/agent_status_model.rs:795), CLAUDE_SESSION_ID, NAMEDROP_HERDR_TIMEOUT_MS, and
/// NAMEDROP_REX_TIMEOUT_MS. Keeps REX_BIN_DIR, REX_HOME, and REX_RUNTIME_DIR, which say
/// how to run `rex`, not where this process runs.
pub fn scrub(command: &mut Command) { todo!() }
```

| Daemon | Spawned today at | Scope | Set after the scrub |
|---|---|---|---|
| Status worker | `src/main.rs:1114` | `Space` | `NAMEDROP_HERDR_TIMEOUT_MS=10000`, as today |
| Deferred worker and executor | `src/deferred_commands.rs:446`, `:527` | `Endpoint`, which their arguments also carry | Nothing |
| Reminder clock | `src/reminder/clock.rs:149` | `Endpoint`, also in its arguments | Nothing |
| Dashboard server | `src/dashboard/server_files.rs:533` | `None` | Nothing |
| Dashboard mailbox wake | `src/dashboard/http.rs:2290` | `Space` | Nothing |
| Prompt hook's mailbox wake | New, in place of the wrapper's `&` at `src/install.rs:1183` | `Space` | Nothing |
| Stop worker | `src/stop_hook.rs:115` | `scrub` only. Its endpoint and space travel in `CapturedStop` | Nothing, as today it drops `NAMEDROP_HERDR_TIMEOUT_MS` |
| Bridge, relay worker, `relay accept` | New | `Endpoint` | Nothing |

A daemon born under the Stop wrapper therefore never keeps its 250 ms `NAMEDROP_REX_TIMEOUT_MS`, which would decline every prompt with `Declined(AfterDelay)`. Herdr daemons lose only names they never read. No Herdr daemon reads `REX_*`. The only reader of the agent markers in a runtime is `with_runtime_options` (`src/main.rs:928`, `:949`), whose session path runs only when no space is known, and every space-scoped daemon has its space. The `/verify` run on both lanes before the commit proves it ([H]).

A boundary test lists every production `Command::new` of Namedrop's own executable (`current_exe`, `runtime_executable`, `selected_executable`, `launcher`, and the dashboard's `executable`) and fails when one does not pass through `daemon::command` or `daemon::scrub`. It follows the registry style of `src/mux/herdr/boundary.rs` and `DASHBOARD_SPAWNS` in `src/pane_effects.rs`. Four exceptions are listed in the test with their reasons. The runtime re-exec (`src/main.rs:228`, `src/installed_runtime.rs:83`) and the reminder clock's re-exec into a replaced runtime (`src/reminder/clock.rs:219`) replace the process and keep its identity on purpose. The dashboard's `--help` capability probe (`src/dashboard/server_files.rs:88`) exits at once and reads no location. Live peek's observer guard (`src/dashboard/live_peek.rs:568`) is Herdr adapter plumbing and is refused on Rex. `process_group(0)` stays where it is for `herdr`, `git`, `tailscale`, `python3`, the browser, and agent summary workers, so the test does not look for it. The installer no longer writes a backgrounded `hook mailbox-wake`, so no shell text starts a daemon.

### The gate learns that a program can change

The stale record shows that a status record can outlive its program. The tap reports that as its own kind of change.

```rust
// src/mux/gate.rs
pub enum TapChange {
	/// The pane's agent reported this state.
	Status(AgentState),
	/// The program the facts named may no longer run in the pane.
	Program,
}

pub trait StatusTap {
	/// The next change after the facts were read, or `None` when none arrives within `within`.
	fn next_change(&mut self, within: Duration) -> Option<TapChange>;
}

// emulate_prompt's pre-paste check is unchanged: a change of either kind declines.
// Its confirm loop becomes:
//   Some(TapChange::Status(AgentState::Working)) => return InputOutcome::Delivered,
//   Some(TapChange::Program) => return InputOutcome::Ambiguous(format!("the program in pane {pane} changed after the prompt")),
//   Some(TapChange::Status(_)) => {}
//   None => break,
```

`FakeMux::emulated()` wraps its states in `TapChange::Status`, so its tests keep their meaning.

### The adapter

```rust
// src/mux/rex.rs
/// Opens nothing. Each port call opens one control connection, proves it local, runs, and closes it.
pub fn connect(state: &Path, endpoint: &Endpoint, budget: Budget, cancelled: Arc<AtomicBool>) -> Result<Arc<dyn Mux>, MuxError> { todo!() }

pub(crate) struct RexMux<W> {
	wire: W,
	endpoint: Endpoint,
	bindings: Bindings,
	/// Parents, groups, executables, and start times of local processes. Scripted in tests.
	local: Box<dyn LocalProcesses>,
	proven: Cell<Option<u32>>, // the server pid the last local proof saw
}

// src/mux/rex/control.rs
/// Opens one connection over any Read + Write: upgrade, hello, welcome. A scripted impl replaces it in tests.
pub(super) trait Wire: Send + Sync {
	fn open(&self, limits: Limits<'_>) -> Result<(Box<dyn Control>, u32 /* peer pid */), Fault>;
}
pub(super) trait Control {
	/// One request and its response. `Reply.seq` is the last event queued before the request started.
	fn request(&mut self, method: &str, payload: Value) -> Result<Reply, Fault>;
	/// The next buffered or arriving event, or `None` when `within` passes.
	fn next_event(&mut self, within: Duration) -> Result<Option<Event>, Fault>;
}
pub(super) enum Fault { Rex { code: String, message: String }, Timeout(Duration), TooLarge, Closed(String), Cancelled, Allowance(String), NotLocal(String) }
```

**One connection per port call.** `survey`, `screen`, `process`, `tab_label`, `apply`, and `deliver` each open a connection, run their requests, and drop it, which sends close 1000. A fresh connection with hello, attach, and one method took 0.8 ms, and a request on a held one took 0.1 ms (measured). The bridge is the only holder of a long-lived connection.

**The liveness rule is one pure function.** The survey and the gate's facts both call it, so they cannot disagree.

```rust
// src/mux/rex/agent.rs
pub(super) struct Observed<'a> {
	pub record: Option<&'a StatusRecord>,      // the root record, `id` absent or ""
	pub foreground: Option<&'a Proc>,          // `process.foreground`
	pub block_is_local: bool,                  // the block's host_id equals the proven server's host_id
	pub incarnation: Option<&'a Incarnation>,  // the foreground's, read locally
	pub binding: Option<&'a Binding>,
	/// The binding's agent pid, read locally: its start time and process group, or None when it is gone or a zombie.
	pub bound_agent: Option<&'a LocalProc>,
}
pub(super) struct PaneAgent { pub agent: Option<AgentIdentity>, pub state: Option<AgentState>, pub reports_session: bool }

/// A record speaks for the block only while its owner is the live foreground
/// process, pid and name both. A session is reported only while the binding names
/// the foreground's incarnation, the binding's agent is alive (present and not a
/// zombie) with its bound start time in the foreground's process group, and the binding's runtime is the
/// record's `app`. So a replaced program loses the session, and a launcher whose
/// agent died stops speaking for it.
pub(super) fn pane_agent(observed: Observed<'_>) -> PaneAgent { todo!() }

/// The block's shell, or None. The candidate is `child`, or, when `child` is
/// `login` (macOS), the process just below it in `[foreground] ++ ancestors`.
/// The candidate counts only when its executable (`path`) is a shell by
/// processes::is_shell. A block made with `-- python3 -i`, an editor, or `ssh`
/// therefore has no shell. When the candidate is the foreground itself, it counts
/// only when `foreground_group`, the members of its process group read locally,
/// holds no other process. A non-interactive shell runs its commands in its own
/// group, so `-- bash -c 'sleep 120; true'`, whose child and foreground are both
/// bash with `sleep` in bash's group, has no shell. `None` also when Rex leaves
/// `ancestors` out (more than 16, or one exited), for that read only.
pub(super) fn shell_of(process: &ProcessReply, foreground_group: &[u32]) -> Option<u32> { todo!() }
```

| Record owner is the live foreground, and `app` is `claude-code` or `codex` | The binding | `agent` | `state` |
|---|---|---|---|
| yes | names the foreground's incarnation, its agent lives with its start time in the foreground's group, and its runtime is `app` | `Known(kind)`, `Reported(session)` | the record's state |
| yes | none, or it names another foreground incarnation | `Known(kind)`, `Unreported { incarnation }` | the record's state |
| yes | names the foreground's incarnation, but its agent is gone or a zombie, restarted, or out of the group, or its runtime is not `app` | `None`, a free terminal, because the record is stale behind a launcher | `None` |
| yes | start time unreadable, or the block is not local | `Known(kind)`, `SessionClaim::None` | the record's state |
| no record yet | names the foreground's incarnation, and its agent lives in the group | `AgentKind::Unreported`, `Reported(session)` | `None` |
| no, the record is stale, or `app` is another program | any other case | `None`, a free terminal | `None` |

The third row is review 3's finding 1. Claude Code behind a `bash` wrapper that does not `exec` was killed with `kill -9`, and Rex kept `{state: done, app: claude-code, owner: bash}` with the wrapper as foreground. The bound agent was gone, so the row said free terminal. The block's shell (zsh, group 75489) did not hold the foreground group (82428), so `shell_is_available` was false and no terminal line could be typed either (re-probed, [Evidence](#revision-4)). When the launcher starts a new agent, that agent's first hook binds it, and the first row applies again. When the agent is the foreground itself, the agent check and the foreground check are one check.

**Alive means not a zombie.** A launcher that does not reap its children keeps a dead agent as a zombie. Review 4 ran Claude Code behind a Python launcher that never calls `wait`, sent it `kill -9`, and saw `Z+` in `ps`, while Rex kept `{idle, claude-code, owner Python}`, and a read that returns zombies admitted the prompt. Linux's `/proc/<pid>/stat` keeps a zombie's process group and start time, and macOS's `proc_pidinfo` returns one when its `arg` is 1. So `mux/processes.rs` reads a zombie as no process. On Linux it reads the state field of the same `/proc/<pid>/stat` line, and `Z` or `X` is no process. On macOS it passes `arg` 0, which fails on a zombie. Every check in this design that a process is alive, the foreground's and the bound agent's in the survey, the facts, the re-read, and `bind_session`, goes through that one read. The read tells `Gone` (no such pid, or a zombie) apart from `Unreadable` (any other error). A foreground that is `Gone` is not the live foreground, so its record is stale and the block is a free terminal (the last row). The row for an unreadable start time applies only to `Unreadable`.

`app` must be `claude-code` or `codex` (both measured). Any other program that reports OSC 7501, such as `cargo`, leaves the block a free terminal. The state goes through `AgentState::parse`, so `error` and unknown words become `Unknown` and are never admitted. `PaneFacts.reports_session` is `true` when the pane reports a bound session or no agent at all, and `false` when an agent runs whose session is unknown.

A block whose `host_id` differs from its server's runs on another machine, so its pids name nothing here. It is never typed into, by the gate or as a terminal. Its server's own machine owns it, and the relay reaches it there.

**The process facts.** `PaneProcess` is `shell_pid = shell_of(reply, foreground_pids)`, `foreground_pids` = the members of the foreground's process group, read locally, and `foreground_group` = the foreground's process group, read locally, because Rex reports no process group. In an idle block the reply was `child login 92189, foreground zsh 92197, ancestors [login]`, so the shell is 92197, whose group is 92197, and `shell_is_available` holds. With `sleep 30` running it was `foreground sleep 92786, ancestors [zsh 92197, login]`, so the shell is still 92197 and the group is 92786 (measured). A block made with `-- python3 -i` reports child and foreground both `Python` and no ancestors (review 2's measure), so it has no shell.

**The screen speaks the port's ANSI.** Herdr's ANSI rendering, which `delivery.rs` parses, is text with SGR styling. Rex's `vt` format also writes a run of n blank cells, a tab among them, as `ESC[nC`, and `cells()` drops every CSI but SGR, so `codex_box` loses the space after `›`. `RexMux::screen` converts at the boundary, and refuses what it does not know.

```rust
// src/mux/rex/wire.rs
/// Rex's `vt` output in the port's ANSI shape, or None. Each ESC[nC becomes n
/// spaces (one when n is absent). SGR (ESC[…m) and OSC 8 links pass through,
/// because cells() parses both. Any other escape sequence returns None, and
/// `screen` answers MuxError::Unavailable naming it, a screen fault.
pub(super) fn port_ansi(vt: &str) -> Option<String> { todo!() }
```

The fix belongs in the adapter, not in `cells()`, because the port promises one shape and Herdr's path then stays untouched. With delivery's real parser, the raw captures read an idle Codex with an empty box as `Draft`, and a fresh Codex draft as `Unseen`. After the conversion, all six captures read as their screens show (measured, [Evidence](#evidence)).

**A Rex screen fault holds the mail.** Today delivery turns a failed screen read into `PromptBox::Unseen` (`src/delivery.rs:1223-1225`), and `Unseen` while the agent reports `idle` or `done` delivers (`:1151`), so that Herdr's idle stays the proof a prompt may go in. Under that rule a Rex screen fault would deliver without the box read that `port_ansi` refuses to guess. So on a Rex endpoint a screen fault, a read error or `port_ansi` answering `None`, is `PromptBox::Unread`, which waits as `AgentWait::ScreenUnread` whatever the agent's state, and the draft recheck reads the screen again until a read succeeds. On a Herdr endpoint a failed read stays `Unseen`, and Herdr's rule does not change. A screen that is read but shows no box Namedrop knows stays `Unseen` on both, which delivers on `idle` or `done` (see [Open questions and risks](#open-questions-and-risks)).

**The gate channel.** `deliver` for a prompt opens one connection and runs `gate::emulate_prompt` over it.

```rust
// src/mux/rex/channel.rs
impl gate::RawChannel for RexChannel<'_> {
	type Tap = RexTap;
	/// session.attach for the block's session. From here the session's events buffer on the connection.
	fn tap(&self, pane: &PaneId) -> Result<RexTap, ChannelError> { todo!() }
	/// program_status, then process, then the local start time and the binding file. `pane_agent` answers.
	/// The tap's boundary is the `seq` of the program_status response, the first of the two.
	fn facts(&self, pane: &PaneId) -> Result<(Option<AgentIdentity>, Option<AgentState>), ChannelError> { todo!() }
	/// block.event paste of `text` with every control character turned into ' ', and its ack.
	/// Then one non-blocking read of the connection and process again. Each of these stops
	/// here with an error, so the gate holds the prompt and Enter is never pressed:
	/// - a status change for the block above the facts' boundary and at or below the paste
	///   ack's `seq`, which Rex queued before the paste started (the agent was already busy);
	/// - a foreground pid other than the facts' pid;
	/// - a bound agent that is gone or a zombie, has another start time, or left the foreground's group.
	/// Then block.event key Enter and its ack. No sleep.
	fn paste_submit(&self, pane: &PaneId, text: &str) -> Result<(), ChannelError> { todo!() }
}

impl gate::StatusTap for RexTap {
	fn next_change(&mut self, within: Duration) -> Option<TapChange> {
		// Skip events with seq <= the boundary and events for other blocks. A status
		// change that lands between the two facts requests has a higher seq, so it declines.
		// A zero `within` still makes one non-blocking read of the socket, so the gate's
		// pre-paste check sees an event that has arrived but was not yet read.
		// After the paste, a status change with seq <= the paste ack's seq happened before the
		// paste started, so it is Program, which the confirm loop holds as Ambiguous. It never
		// confirms as Delivered.
		// Root program_status_changed       => Status(AgentState::parse(record.state)).
		// Root program_status_removed       => Program.
		// child_exited                      => Program.
		// process_changed                   => Program only when its foreground pid differs from the facts' pid,
		//                                      because opening a data connection to the block also sends it (measured).
		// title_changed and every other event are skipped.
		todo!()
	}
}
```

**A paste never runs without Enter.** The typed prompt is three or four lines (`src/pointer.rs:21-34`). Without bracketed paste, bash 3.2 and dash run each pasted line that ends in a newline (review 2's measure), and a `\r` ran with no Enter in bash 3.2 and dash (review 3's measure). Rex pastes therefore turn every control character (U+0000 to U+001F and U+007F) into a space. Rex already blanks ESC (review 3's measure), and the rule does not depend on it. A prompt is rewritten that way. A terminal `Line` that holds `\n` or `\r` is `Refused` before any byte, because rewriting a multi-line command into one line would run something other than what was asked. The one-line text stayed at the prompt, unrun, in bash 3.2, `sh`, zsh, and dash with no Enter (measured). The newlines carry nothing for the readers. `pointer::strip_matching` and `looks_like_pointer` compare with whitespace runs collapsed (`src/pointer.rs:80-90`), and an envelope typed with no pointer is compact JSON, which has no newline. Claude Code's and Codex's UserPromptSubmit hooks received the one-line paste byte for byte, and the strip rule matched it to the three-line prompt (measured).

**The re-read after the paste.** The second `process` read, with a local read of the bound agent, closes most of the window in which an agent exits between the facts and the Enter, for one 0.1 ms request. When the foreground changed or the bound agent is gone or a zombie, no Enter is pressed and the attempt is held. Behind a launcher the foreground stays the launcher, so only the bound agent's check sees the death (re-probed, [Evidence](#revision-4)). The line then sits in the input of whatever reads the terminal next, which is a shell when the agent died. A person who presses Enter in that shell runs it, `$(…)` in the JSON-quoted body included, because JSON quotes with double quotes. Namedrop presses no key to clear the line, because the gate no longer knows what program has it. One window stays open. An agent that dies after the re-read and before the Enter, about 0.1 ms, gets the Enter too, and the shell that reads the terminal next runs the line.

**A hold after the paste.** When a fault follows the paste, or the confirm window ends without `working`, the attempt is held and Namedrop's pointer stays in the agent's box. The next mail to that agent then waits as a draft, as it waits for text the person typed, and the draft recheck drains it once the box is empty. The person sees the pointer in the box, and Doctor lists the held attempt. Namedrop presses no key to clear it, because that would type again into an agent whose last input the gate could not confirm. Herdr leaves the same state after a fault that follows its `agent prompt` paste (inferred).

`deliver` for `Line` and `Keys` sends `block.event` paste and key events with no confirmation. A `Line` with `\n` or `\r` is `Refused`, and its other control characters become spaces. A Rex error on the first request is `Refused`, because nothing was typed. A fault after the first request was written is `Ambiguous`. `Recipient::Agent` and `Recipient::Pane` type the same way on Rex.

**The binding.** One JSON file per block, written only by `bind_session` and deleted only by `RexEvents` when it decodes `block_closed`. Block IDs are never reused and survive a server restart (measured), so the block ID alone is the key.

```rust
// src/mux/rex/binding.rs
/// `<pid>@<start>`. macOS: microseconds since the epoch (pbi_start_tvsec and
/// pbi_start_tvusec). Linux: `<boot id>:<ticks>`, from /proc/sys/kernel/random/boot_id
/// and field 22 of /proc/<pid>/stat, so a binding cannot outlive a reboot.
/// Only equality matters, and the hook and the survey read it on the same machine.
pub(super) fn incarnation(local: &dyn LocalProcesses, pid: u32) -> Option<Incarnation> { todo!() }

/// The process whose hook this is: the nearest ancestor whose executable is not
/// a shell by processes::is_shell (sh, bash, zsh, dash, ksh, fish, or busybox,
/// by the executable's basename). Never by comm, which Linux sets to a script's
/// truncated name.
pub(super) fn hook_agent(local: &dyn LocalProcesses) -> Option<LocalProc> { todo!() }

impl Place {
	/// Records that `session` runs in the caller's block. Let F be `process.foreground`
	/// and H be hook_agent(). Refuses unless all of these hold:
	/// 1. H is in F's process group.
	/// 2. H is F, or F is an ancestor of H, every process between them is a shell by
	///    processes::is_shell, and F's executable path differs from H's (the launcher rule).
	/// 3. A root record whose owner is F (pid and name) has `app` equal to `runtime`, the
	///    wrapper's `--runtime`. A record whose owner is not F is stale, as the survey
	///    reads it, and counts as absent, as no record yet does.
	/// 4. The start times of F and H read.
	/// 5. When H is not F, the block's binding does not already name F's incarnation as
	///    its `agent`. When H is F, a hook that meets the other conditions replaces any binding.
	/// 6. For `runtime` claude-code, `entrypoint`, the hook's inherited
	///    CLAUDE_CODE_ENTRYPOINT, is `cli`. Unset or any other value refuses.
	/// Writes only a change, so UserPromptSubmit can call it on every prompt.
	pub fn bind_session(&self, state: &Path, session: &AgentSessionId, runtime: AgentRuntime, entrypoint: Option<&OsStr>) -> Result<(), String> { todo!() }
}

#[derive(Serialize, Deserialize, Eq, PartialEq)]
pub(super) struct Binding {
	pub block: String,      // "block:<uuid>"
	pub endpoint: String,   // the canonical endpoint, for a sandboxed deferral
	pub space: String,      // "session:<uuid>", for a sandboxed deferral
	pub foreground: String, // F's incarnation, "82428@1794550330414769"
	pub agent: String,      // H's incarnation, "82429@1794550330417141". Equal to `foreground` when H is F
	pub runtime: String,    // "claude-code" or "codex", which the record's `app` must equal
	pub session: String,    // the hook's stdin session_id
	pub bound_at_ms: u64,
}
```

A terminal gives its foreground group to the program the shell started, and an agent's tools run in a group of their own with no controlling terminal. So the group test (condition 1) passes a launcher that stays as the agent's parent and fails an agent run as a tool. It does not fail an agent that an MCP server starts, because MCP servers run in the agent's group (review 3's measure). Condition 2 closes most of that. Between a launcher and its agent there is nothing, or only shells, and a launcher is another program than its agent. An MCP server is a non-shell process between the outer agent and the nested one, and a shell that an agent itself runs usually leaves the agent's own executable as the foreground. Condition 3 refuses an agent of the other runtime once a record names F.

Conditions 5 and 6 close what the executable test misses. Claude Code keeps several versions side by side and names each executable for its version (four were installed on the review machine), so after an auto-update a nested `claude -p` can run another executable than the outer agent. Review 4 measured it with an MCP server `sh -c "<versions/2.1.322> -p …"` under Claude Code 2.1.323. Only shells lay between the nested agent and the foreground, the executables differed, and the nested hook bound under conditions 1 to 4. The outer agent's binding already names the foreground as its own agent, so condition 5 refuses that hook. A nested Claude Code's hook sees `CLAUDE_CODE_ENTRYPOINT=sdk-cli`, and the outer agent's sees `cli` (measured in every MCP run of revision 4, and by review 4), so condition 6 refuses it too. A Codex started by an MCP server that is itself `sh -c` has only shells between it and Claude Code, so condition 2 passes it, and condition 5 refuses it once the outer agent is bound (inferred, not run). Codex 0.163.0 has no `mcp-server` command, so the case review 3 named does not arise with it. Before the outer agent's first hook binds, condition 5 has nothing to compare, so such a nested Codex could bind, and the outer agent's next hook replaces that binding, because its agent is the foreground (inferred). Measured:

| Agent and hook | Block foreground (its group) | The hook's agent (its group), and what lies between | Rule |
|---|---|---|---|
| Claude Code 2.1.323, SessionStart and UserPromptSubmit | `claude` 26048 (26048) | `claude` 26048 (26048), the foreground itself | Bind |
| `claude -p` run as a tool by that Claude Code | `claude` 26048 (26048) | nested `claude` 46007 (45976, its tool shell's group) | Refuse, condition 1 |
| Codex 0.163 with `--no-daemon` | `codex` 50380 (50380) | `codex` 50380 (50380), the foreground itself | Bind |
| Codex 0.142.2 through its npm launcher | `node` 82223 (82223) | `codex` 82278 (82223), nothing between, `node` is not `codex` | Bind |
| Claude Code behind `bash wrap.sh`, which does not `exec` | `bash` 82428 (82428) | `claude` 82429 (82428), nothing between, `bash` is not Claude's executable | Bind (revision 4) |
| `claude -p` started by a Python MCP server of Claude Code | `claude` 46300 (46300) | nested `claude` 46638 (46300), with the server `Python` 46626 between | Refuse, condition 2 (revision 4) |
| `claude -p` through `sh -c` from a Python MCP server | `claude` 46326 (46326) | nested `claude` 46719 (46326), with `Python` 46709 and `bash` 46717 between | Refuse, condition 2 (revision 4) |
| `claude -p` from an MCP server that is itself `sh -c` | `claude` 59404 (59404) | nested `claude` 59612 (59404), with `bash` 59609 between, and the foreground runs Claude's own executable | Refuse, condition 2 (revision 4) |
| `versions/2.1.322 -p` from an MCP server that is `sh -c`, under Claude Code 2.1.323 | `claude` 2.1.323 | nested `claude` 2.1.322 in the foreground's group, with only shells between, and another executable | Refuse, conditions 5 and 6 (review 4's measure, which revision 4's rule bound) |
| Codex 0.163 in its default mode, the block that started its app server | `codex` 80440 (80440) | the app server's `codex` 81243 (81243) | Refuse, condition 1 |
| Codex 0.163 in its default mode, a second block on the same `CODEX_HOME` | the hook claims the first block, `codex` 80440 (80440) | the app server's `codex` 81243 (81243) | Refuse, condition 1 |

In every revision 4 run the outer agent's own binding survived its nested agents' refused hooks. A record may not exist at SessionStart. Behind the `bash` wrapper it did not ([Evidence](#revision-4)), and review 4 found one at SessionStart in 5 of 6 runs, so the order is a race. Condition 3 therefore checks a record only when there is one whose owner is F. After a crash, the new agent's SessionStart can beat its own first report while the dead agent's record still stands, and that stale record counts as absent instead of refusing the bind. The survey then checks the runtime against `app` on every read (the first row of the liveness table). Revision 3's rule (condition 1 alone) bound the nested agents of the MCP rows. Revision 2's rule ("the hook's agent is the foreground pid") refused the launcher rows. Review 1's rule ("the foreground is an ancestor") would have bound the second daemon-mode block's session to the first block. With a launcher, Rex reports the launcher as the record's owner (measured with `bash`, and review 2's measure with a Python launcher), and the binding's agent incarnation is what tells a live agent from a dead one. Codex 0.142.2 sent Rex no status record at all, so its block stays a free terminal until a Codex that reports runs behind the launcher.

The runtime comes from the wrapper because nothing in a hook's own process tells the runtimes apart reliably. A nested agent inherits the outer agent's environment, and Claude Code's executable is named for its version. `integration install` already writes one wrapper per runtime (`src/install.rs`, `AgentRuntime`), whose `serde` names are exactly Rex's `app` words. A wrapper installed before this change passes no `--runtime`, so its hooks never bind on Rex, and Doctor says to run `integration install` again. Herdr ignores the flag.

**Codex's default mode is unsupported, as on Herdr.** Codex 0.162 and later start one background app server for each `CODEX_HOME` and run every session's hooks and tools in it, with the environment of the block that first started it (measured: the second block's hooks carried the first block's `REX_BLOCK`). No hook runs in a block's foreground group, so the agent never binds and cannot register. What the person sees is what Herdr users see today. Namedrop's SessionStart hook detects the app server without a runtime (`execute_codex_app_server_hook`, `src/hook.rs:72`) and puts its notice in Codex's warning row, and Doctor's issue 003 gives the fix: `codex features disable daemon_auto_start` and `codex app-server daemon stop`, or `codex --no-daemon` for one session. The notice names Herdr today. On Rex its words name Rex when the server inherited Rex names and no Herdr name, and Herdr's bytes do not change. Any `-c`, `--enable`, `--disable`, or `--search` option also puts Codex in embedded mode (measured, Codex's own startup warning), so a wrapper that passes `-c` binds.

### Sandboxed commands defer through the binding

Codex's default sandbox denies a tool's connect to the Rex socket with `EPERM`, and it still lets the tool read its ancestry, the start times of its ancestors, and files outside the workspace (measured with Codex 0.163). Herdr has the same denial, and today's answer is the deferral. `try_defer` writes the command under `<state>/deferred-commands/<sha256(endpoint)>/`, a writable root in Codex's config, and the deferred worker outside the sandbox runs it as the stored caller (`src/deferred_commands.rs:345`). Codex's hooks ran outside the sandbox and reached the Rex socket in every run (measured), so the hook has already written the block's binding by the time a tool runs.

```rust
// src/mux/home.rs, the `Denied` row with no Herdr name
// 1. Read <state>/rex/sessions/<uuid of REX_BLOCK>.json, with the state directory
//    from --plugin-state, NAMEDROP_STATE_DIR, or the install record.
// 2. Its agent incarnation `pid@start` must be an ancestor of this process, with that start time.
// 3. Home::RexDeferred(Deferral { endpoint: binding.endpoint, space: binding.space, pane: binding.block }).
// Otherwise: "the agent's sandbox denies the Rex socket, and no Namedrop hook has bound this block yet".

// src/deferred_commands.rs
pub fn try_defer(operation: &str, input: &Value, options: &RuntimeOptions, home: &Home) -> Option<Result<Value, String>> {
	// Home::Herdr: today's body, with the location from home.location() in place of Locator::*.env().
	// Home::Rex(place) from explicit flags (rule 1, no caller): today's body too. Connect to
	//   place.endpoint.unix_socket(), and on EPERM or EACCES defer under
	//   StateNamespace::for_space(endpoint.key(), --space) with --pane. Any other result is None.
	// Home::Rex(place) from a verified REX_BLOCK: None, because its socket already answered.
	// Home::RexDeferred(d): the request names StateNamespace::for_space(d.endpoint.key(), d.space) and d.pane.
	todo!()
}
```

A sandboxed Codex sends its replies, acks, and completes through command lines that Namedrop rendered with `--endpoint`, `--space`, and `--pane` (`src/integration.rs:1992-2011`). Rule 1 resolves those to a Rex place with no probe, so `try_defer` probes the flag's socket itself, as today's body does for `--herdr-socket` (`src/deferred_commands.rs:345-358`). Without that probe the command would fail with `EPERM` on its first connect.

The deferred worker checks the caller against the live survey, as it does today (`validate_live_caller`), so a stale binding cannot act. The survey reports the session only while the binding matches the live foreground and its agent. Every Rex hook run ensures the worker, as `src/main.rs:994` does for Herdr. `execute_request` builds its runtime with `mux::connect`, so it serves either kind.

### The bridge, the queue, and the worker

```rust
// src/mux/event.rs
#[derive(Serialize, Deserialize)]
pub enum MuxEvent {
	// the six existing variants
	/// Events for this space may have been missed, as when the Rex bridge (re)connects.
	Resync(SpaceId),
	/// The space and every pane in it are gone for good.
	SpaceClosed(SpaceId),
}

// src/lifecycle.rs
/// Runs the handler for one multiplexer event. The status worker's Rex markers
/// enter here. The attribution actor is the endpoint kind's word, "herdr" or "rex".
pub fn handle_mux_event<M: Mux, C: Clock, I: IdGenerator>(runtime: &LifecycleRuntime<'_, M, C, I>, event: &MuxEvent) -> Result<Value, EventError> { todo!() }

/// How the worker treats a failure. Decided from MuxError, never from the text.
pub enum EventError {
	/// Gone, or a rejected input. Retrying cannot change it, so the marker is dropped.
	Definitive(String),
	/// Timeout, unavailable, storage. The marker stays and the worker backs off.
	Retry(String),
}
// MuxEvent::Resync(space)      => discover the space; Gone(Space) runs the SpaceClosed handler;
//                                 else require_activated, then reconcile_worktree(.., after_restart = false).
// MuxEvent::SpaceClosed(space) => in the stored namespace for (endpoint, space): end its moves,
//                                 cancel and remove every pane address, and fail its reminders.

// src/status_queue.rs
pub enum Marker {
	Wake,                         // {"operation":"mailbox-wake"}, as today
	DraftRecheck(Vec<DraftHold>), // {"operation":"draft-recheck","held":[..]}, as today
	Event(MuxEvent),              // {"operation":"mux-event","event":{..}}, new
	Herdr(Value),                 // today's raw Herdr status data, decoded late by design (PORT.md commit 10)
}
pub fn enqueue(state: &Path, endpoint: &Endpoint, space: &SpaceId, marker: &Marker) -> io::Result<()> { todo!() } // queued-*, fsync, rename to pending-*
pub fn spawn_worker(state: &Path, endpoint: &Endpoint, space: &SpaceId) -> Result<(), String> { todo!() }       // daemon::command, Scope::Space

// src/rex_bridge.rs
/// One bridge per Rex server. Holds try_lock on <state>/rex-bridge/<sha256(endpoint)>.lock for its life.
pub fn run(state: &Path, endpoint: &Endpoint) -> ExitCode { todo!() }
/// Spawns `namedrop rex bridge` through daemon::command unless the lock is held.
pub fn ensure(state: &Path, endpoint: &Endpoint) -> Result<(), String> { todo!() }
```

`Marker::Herdr` keeps `retryable_agent_status_reconciliation`, which reads Herdr's JSON codes, so Herdr's worker behaves as today. A shared `pane_closed` is not changed, so Herdr's pane close keeps its behavior too. A Rex `PaneClosed` whose space is already gone fails `Definitive`, and the `SpaceClosed` that follows it retires the space.

```text
run(state, endpoint):
	try_lock <state>/rex-bridge/<sha256(endpoint)>.lock, or exit 0
	stop = a flag that SIGTERM and SIGINT set
	away = backoff 100 ms, 250 ms, 500 ms, 1 s, then 5 s; give up after 120 s without a server
	while not stop:
		stream = RexEvents::open(endpoint) (prove_local first), else wait on `away`, or exit 0 when it gives up
		compare server.status host_id with <state>/rex/servers/<sha256(endpoint)>.json; write it only when none is stored
		from Part 2 on, declare the relay: client.set_label "namedrop", client.keymap with namedrop.relay
		unless the host_id changed: for each activated space of this endpoint in the store, pending.push(Resync(space))
		loop:
			flush when 50 ms pass with no event, or 250 ms after the first pending change:
				take runtime_command_guard, call runtime_reexec_if_replaced
				for each (space, marker) in pending: status_queue::enqueue
				for each space with a new marker: status_queue::spawn_worker
				release the gate
			match stream.next(time to the next flush):
				Some(Event(event)) => pending.note(classify(event)) // coalesced, see below
				Some(Perform(request)) => relay::receive(request)   // Part 2
				None => {}
				Ended => break, then reconnect. In-flight mail is untouched.
```

| Rex event (inside `session_event` unless noted) | Marker |
|---|---|
| `block_created` | `PaneAppeared` |
| `block_closed` | `PaneClosed`, after `RexEvents` deletes the block's binding file |
| `window_closed` | `TabClosed { space, tab: window_id }` |
| `session_destroyed` (top level) | `SpaceClosed`. `rex kill` sends `block_closed` for each block about 6 ms before it (review 1's measure) |
| `block_event program_status_changed`, root record | `AgentStatusChanged` |
| `block_event program_status_removed`, root record | `AgentStatusChanged` |
| `block_event process_changed` | `AgentStatusChanged`. The first block of a new session sends this and no `block_created` (measured) |
| `block_event child_exited` | `AgentStatusChanged`. `block_closed` follows when the program's exit closes the block (measured) |
| `session_created`, `title_changed`, `pwd_changed`, `bell`, `size_changed`, `client_*`, `session_view_changed`, anything unknown | Nothing |

`pending` holds at most one `AgentStatusChanged` per pane. A `Resync` absorbs the queued `AgentStatusChanged` markers for its space, and a `SpaceClosed` absorbs every marker for its space. `PaneAppeared`, `PaneClosed`, and `TabClosed` keep their order. So the queue is bounded by the number of blocks on the server. The bridge flushes only for spaces with an activated worktree, which a read-only store query lists.

A gone space is not a restart artifact. After SIGTERM and a restart of a scratch server, the first connection listed the saved session and attached to it, and the block answered `process` with a fresh `login` (measured). So `Resync` may retire a space that answers `Gone`. A changed `host_id` behind the same endpoint means another server, with other state, now answers at that socket. Then the bridge queues no `Resync`, so it retires nothing and keeps the stored `host_id`. Doctor's wording for that case is deferred, and the guard is not. `namedrop reconcile` in that server's blocks records the new `host_id` and retires what is gone, once the person has looked.

`AgentDetected` is never produced in steps 4 and 5. On Herdr it differs from a status change only by opening the tour popup, and Rex popups arrive in step 6.

### Every identity site takes the home

These are the production readers of the endpoint, space, or pane names at step 3's final head. Each takes the home, and a boundary test fails when anything outside `src/mux/home.rs` and `src/locator.rs` reads `Locator::Endpoint`, `::Space`, or `::Pane` with `env`, `env_os`, or `inherited`, or reads one of the six names with `env::var` or `var_os`. The test reads source with whitespace removed, as `boundary::compact` does, so a call split over lines (`src/main.rs:1140-1146`) still counts. It reads production code only and skips `tests.rs` files and `#[cfg(test)]` modules, so the Herdr adapter's test that builds its expected call from the inherited socket (`src/mux/herdr/process.rs:257`) is not a reader. The test exempts `src/dashboard/observer_guard.rs:23`, which reads the `HERDR_SOCKET_PATH` that live peek gave it and is refused on Rex. Writers (`Locator::pass`, `Locator::clear`, and `Command::env`) are not readers.

| Site | Reads | Takes |
|---|---|---|
| `src/main.rs:198` | The status worker's endpoint for `ensure_clock` | The home's location |
| `src/main.rs:250` | The CLI's endpoint for `ensure_clock` | The home's location |
| `src/main.rs:526-529` | The subagent hook's parent identity | The home's location |
| `src/main.rs:920-926` | The runtime builder's space and inherited pane | The home (rule 2 for Rex, today's body for Herdr) |
| `src/main.rs:1028`, `:1037` | `session_socket_id` and `connection_endpoint` | The home's endpoint |
| `src/main.rs:1059` | The mailbox wake's space | The home. A Herdr home keeps today's body, and a Rex place has the space its `Scope::Space` gave it |
| `src/main.rs:1140-1146` | The status worker's state directory, endpoint, and space for its reconciliation directory | The home's location and `state_directory`. It moves here from commit 21, so commit 16's boundary test passes |
| `src/stop_hook.rs:73`, `:236` | The Stop hook's endpoint and the Stop worker's space | The home's location in the hook, carried to the worker in `CapturedStop` |
| `src/prompt_hook.rs:42` | The prompt hook's endpoint | The home's location |
| `src/agent_status.rs:137` | The status panel's observing pane | The home's location |
| `src/deferred_commands.rs:354-370` | The deferral's endpoint, space, and pane | The home, including `RexDeferred` |

### Remote hosts: typing stays local, mentions relay

The relay ships as [Part 2](#part-2-the-relay), its own PR series after step 5's live check. This section is its design, cut to what Part 2's live check needs.

Each machine that runs agents runs its own Namedrop beside its own Rex server, because the agents' hooks run there and need Namedrop there. A Namedrop types into, locks, binds, and reads incarnations of blocks only on servers that `prove_local` accepts. So the gate, the incarnation, the binding, and the pane lock never cross a machine. A mention to an agent on another machine goes from one Namedrop to the other, and the receiving Namedrop delivers it locally. Below, A is the sender's machine and B the recipient's.

How each element of review 1's remote table comes out:

| Element | Behavior |
|---|---|
| Endpoint string | Only proven-local servers become endpoints, spelled as their primary Unix listener. A remote server is never an endpoint, a namespace, a lock, or a status queue. The relay names it by a `rex hosts` label and pins its `host_id` |
| One server at several addresses | Every local spelling converges on the primary listener. A forwarded socket, an `ssh -L` socket, or TCP fails the local proof, because the peer is not the server's pid (measured with a forwarder) |
| Incarnation | Read only for blocks of a proven-local server whose `host_id` is the server's |
| `host_id` guard | Replaced by the peer proof for the server. `host_id` still marks a block in a local session that runs elsewhere, and such a block is never typed into |
| Hook binding | Written on the agent's machine into that machine's state directory, under the same `REX_SERVER` rule |
| Bridge and pane lock | A block's server is on one machine, and only that machine's Namedrop can prove it local. So two machines never type into one block, and no lock has to span machines |
| `-S` | Every `rex` call the relay makes passes `--autostart=false` and `-S <label>` for a label it found in `rex hosts list`, and runs through `daemon::command`, so no inherited `REX_SESSION`, `REX_BLOCK`, or `REX_CLIENT` can pick a target |
| Transport | Typing uses the local Unix socket. The relay runs `rex`, so tailnet TLS and Rex's admission stay Rex's code. About 40 ms per `rex` process on the stand-in (measured). A call is two processes, `target.resolve` and `client.perform`, because caching the bridge's client id is deferred |

**Which hosts a mention may name.** A remote recipient's host must be a label that `rex hosts list --json` prints, byte for byte, in the environment the relay's later calls use. That list joins the home's hosts and the hosts of this machine's own server (Rex's help), and it is the list `-S` resolves labels from, so the check and the call agree. The label may hold only ASCII letters, digits, `-`, and `_`, 1 to 64 of them. Rex itself accepts labels with dots, colons, and spaces (measured). Namedrop does not, because `-S` reads a value with a port as an address, and a mention token ends at whitespace, so a label with a space could never be written. A host that fails either test fails the send at once with `devbx is not in your Rex hosts (rex hosts list): studio, devbox`, and nothing is queued. The relay worker checks the label again before each call, and an item whose label is gone fails with `devbox is no longer in your Rex hosts`. The checks exist because `-S` falls back to an address. `-S nosuchlabel` dialed `https://nosuchlabel`, `https://nosuchlabel:7297`, and `http://nosuchlabel:7297`, a label `localhost` took the place of the address `localhost`, `-S STANDIN-B` reached `standin-b`, and `-S host:<id>` failed as a bad address (all measured). Namedrop's exact match is stricter than Rex's case-insensitive one. `rex --help` offers no `-S` form that accepts only a label, so a label removed after the worker's check and before its `rex` process starts falls back to an address of the same spelling for that one process. Whatever answers there sees one request, and a Rex server other than B refuses it by the `host_id` pin below.

**Who B is.** Before its first call in a run, A's relay worker asks `rex -S <label> --autostart=false api call server.status` for B's `host_id`, which is Rex's own answer, and stores it in `peers`. Every item A queues for that label records the `host_id` seen at its first attempt, and A sends it as `expect_host_id` in each call. B's bridge compares it with its own server's `host_id` before anything else and answers `refused` with `wrong_host`, storing nothing, when they differ. A then fails the item with `devbox now answers as another Rex server`. `rex hosts add` stores no `host_id`, and `hosts check` does not add one (review 2's measure, confirmed in `rex hosts list --json`), so Namedrop never takes a `host_id` from the host list.

**The action.** B's bridge is already a client of B's server, so it declares one action, and A calls it with `client.perform` through `rex -S <label> api call client.perform -`, with the request on stdin. A call carries at most one item, because batching is deferred.

```rust
// src/rex_bridge.rs, on B
// client.set_label "namedrop"; client.keymap {actions: [{name: "namedrop.relay", kind: "client", args: <schema>}], server_actions: "none"}
// args: {
//   op: "deliver" | "collect",
//   expect_host_id: <B's host_id, as A pinned it>,
//   from: {host_id: <A's server.status host_id>, name: <the sender's alias>, host: <A's machine name, for display>},
//   item: {id: <A's message id>, to: <a name on B>, thread: <A's thread id>, in_reply_to: <B's message id or null>, text}, // deliver only
//   ack_through: <the highest outbox seq A has stored>
// }
// result: {
//   host_id: <B's>, can_reply: bool,
//   result: {id, to, status: "accepted" | "duplicate" | "refused" | "ambiguous" | "retry", reason?}, // deliver only
//   outbox: [{seq, re: <the A message id it answers>, thread: <A's thread id>, from: <B alias>, message_id: <B's>, text}],
//   more: bool
// }
fn receive(request: PerformRequest) {
	// 1. The principal: `peer` with B's uid, or `tailscale`. `anonymous` only while
	//    <state>/relay/allow-anonymous exists. Any other kind is refused. A refused
	//    principal gets client.reply `refused`, and nothing is stored or started.
	// 2. expect_host_id must equal this server's host_id, else `refused: wrong_host`, and nothing is stored.
	// 3. Start `namedrop relay accept` through daemon::command with the args on stdin, and keep the child with a 5 s deadline.
	//    The event loop does not wait. When the child exits, client.reply with its stdout. When the deadline passes first,
	//    client.reply with status "retry", and let the child finish its commit.
}
```

`namedrop relay accept`, on B, does six things in order.

1. It validates every field, because a direct client is not held to A's CLI. `from.name` and `item.to` must pass `validate_alias`, and `from.host` the label rule above. `from.host_id`, `item.id`, `item.thread`, and `item.in_reply_to` must each be 1 to 128 printable ASCII characters with no space. `text` may be at most 60 KiB. A bad field refuses the item with the field's name, and nothing is stored.
2. It looks up the item's key (A's `host_id`, item id, `to`) in `inbound`. A known key returns its stored outcome, which is `duplicate` for an accepted item and the stored refusal for a refused one. So dedup runs before any refusal that depends on state, and a retry never changes the answer.
3. It refuses an item for a paused space.
4. It resolves `to`. With `in_reply_to` naming a message B relayed, `to` must hold a live alias claim in that message's space. Otherwise it resolves among the activated spaces of B's proven-local endpoints. One live claim accepts the item into that space. None refuses it with `no @bob on devbox`. More than one answers `ambiguous` with the session labels, because a remote address has no space part yet.
5. For an accepted item, in one main-database transaction, it inserts the message with the sender's relay author ([below](#relayed-messages-have-a-paneless-author)), with the request id `relay:<A host_id>:<item id>:<to>`, into the B thread that `remote_threads` holds for (A's `host_id`, A's thread, B's space), or a new one. Then it records the key, the outcome, B's message id, and the thread in `relay.sqlite3`, and schedules the space's mailbox worker. A crash between the two writes is found on the retry by the request id. The request id names the recipient, so the second recipient of one A message in the same B space gets its own message, not a fingerprint conflict on `idempotency_records` (`src/store.rs:3073`, `:3088`).
6. It drops B's outbox items for A with `seq <= ack_through`, and returns at most 256 KiB of the rest, with `more` when some remain.

`can_reply` is `true` when B will keep replies for A's `host_id`, which holds unless B's outbox for A holds 10,000 items.

**Replies come back by pull, in both directions.** B never calls A. One `relay::capture` runs in the status worker after each drain in a Rex home, on both machines. It reads each new message addressed to a relay author of the space, which settled when it was inserted, in a thread that `relay.sqlite3` maps to a remote one. It is local database work only, and idempotent by message id. What a message answers is its `in_reply_to` when that names a relayed or collected message, and otherwise the latest such message in its thread.

- On B, it writes an outbox `reply` for A's `host_id`, naming the A message it answers and A's thread. A reply over 60 KiB keeps its first 60 KiB and ends with a line saying Namedrop cut it.
- On A, it writes an `outbound` item to the label, addressed to the B alias that wrote the message it answers, with `in_reply_to` set to that message's B id and `thread` set to A's thread. Then it starts the label's relay worker.

So when A's agent answers a collected reply, its answer goes back to the B agent in the same B thread, and a conversation continues across hosts while only A can reach B. A new mention from B's agent to A's agent is a send from B, so B must list A and reach it.

A inserts each collected `reply` into the sender's space as a message from the relay author for that B agent, in reply to the A message it answers. Its key in `collected` is (B's `host_id`, `seq`), so a reply collected twice is inserted once. The row also keeps A's new message id, B's message id, and B's alias, which A's capture reads.

On A, the relay worker for a label collects while that label has an open thread. A thread is open for 24 hours after its last accepted item or collected reply. The worker collects every 30 s while a thread is open, and every `deliver` also collects. It exits when no item is queued and no thread is open, and a new send starts it again. A reply that B writes after A's threads closed waits in B's outbox until A's next send to B collects it, or until it expires.

| What A sees | What it means | A's item |
|---|---|---|
| `accepted` or `duplicate` | B owns the item | Done. Its thread stays open for replies |
| `refused`, with B's reason | An unknown name, a paused space, a bad field, a refused principal, or `wrong_host` | Failed, with the reason |
| `ambiguous`, with B's session labels | The name is in more than one of B's spaces | Failed, with the labels |
| `retry` | B is still deciding | Kept, and sent again on the next collect |
| Rex `timeout` | B did not answer in time and may still act (measured) | Kept, sent again with the same id |
| Rex `client_unavailable`, `not_found`, or a resolve that is `not_found` or `ambiguous` | No single Namedrop bridge serves B. Two bridges resolve as `ambiguous` (measured) | Kept, with backoff |
| Rex `bad_request` or `not_supported` | B's Namedrop speaks another relay | Failed with `devbox runs a Namedrop without this relay` |
| Rex `too_large`, or the CLI's request limit | The request is too big | Failed. The send refuses such an item before it is queued |
| Rex `refused` or `internal`, a failed `rex`, no route | A transient fault | Kept, with backoff |
| The label is gone from `rex hosts list` | The person removed the host | Failed |

Retries are safe because B deduplicates per recipient. A perform that timed out at A still ran on B, and the retry with the same id came back `duplicate` (measured). So the relay is at-least-once on the wire and exactly-once into B's mailbox. B's own delivery keeps the gate's at-most-once typing. Whether B delivered the item to its agent is not reported back to A, because delivery acks are deferred.

**Limits and expiry.** The `rex` CLI refuses a request over 64 KiB before it sends it (measured: `control message too large: encoded request is 70269 bytes, limit is 65536`), although the server accepts 1 MiB. So the send refuses an item whose encoded request would pass 60 KiB, and B caps `text` at 60 KiB itself. B caps a result at 256 KiB, because a reply over the server's 1 MiB limit broke B's own connection (measured: a 1.1 MB reply, after which A got `client_unavailable`). An item A cannot deliver within 24 hours fails with `devbox was unreachable for 24 hours`. An outbox item on B expires 7 days after B wrote it, and B's Doctor reports how many expired for each remote sender. An `inbound` row expires 7 days after B wrote it, which outlasts A's 24-hour retry, so every retry still finds its key.

**Where relay state lives.** The main database keeps its schema (`SCHEMA_VERSION` 8), because a runtime refuses a database whose `user_version` is newer than its own (`src/store.rs:8023`). The relay keeps its own state in `<state>/relay.sqlite3`, with its own `user_version`, opened only by relay code.

| Table | On | Key | Holds |
|---|---|---|---|
| `peers` | A | label | The `host_id` from the last `server.status`, the last contact |
| `outbound` | A | (label, A's message id, recipient) | The item, its state (queued, accepted, failed, expired), B's `host_id`, the reason |
| `collected` | A | (B's `host_id`, `seq`) | A's inserted message id, B's message id, B's alias, A's thread |
| `inbound` | B | (A's `host_id`, A's message id, recipient) | The stored outcome, B's message id and space, when B wrote it |
| `outbox` | B | (A's `host_id`, `seq`) | Replies for A, and when B wrote them |
| `remote_threads` | B | (A's `host_id`, A's thread id, B's space) | B's thread, and the remote sender's name and display host |

#### Relayed messages have a paneless author

On each machine, each remote agent that talks to a space gets one paneless author there. Its address is `relay:<16 hex of sha256(host_id, alias)>`, and its role is its own, named for the display alias (`bob@laptop` on B, `alice@devbox` on A). It has `pane_type` agent and no alias claim, binding, observation, or registration. `ensure_relay_author` creates it the way `ensure_operator_author` creates `operator:local` (`src/store/operator.rs`). A relayed message has author kind `AgentSession`, because the v1 schema allows only human and agent-session authors (`src/store.rs:6016`), and `author_alias_snapshot` is the display alias, so the pointer names its sender `@bob@laptop` with no change to `pointer::prompt` (inferred from `pointer::sender_label`, which prints the envelope's sender alias). These are rows, not schema.

The operator is special-cased on 49 production lines in 8 files (`grep -rn "is_operator_address\|operator:local" src`, tests left out). Read one by one against a relay author, four sites must change, and the rest already treat it correctly. `is_relay_author` recognizes it with the same query shape as `is_operator_address` (`src/store/operator.rs:72`).

| Site | What it does for the operator | For a relay author |
|---|---|---|
| `src/store.rs:3350` | A delivery to the operator settles at insert | Changed: it settles too. Otherwise a reply or a final reply to the relayed sender stays queued, and the drain looks for a pane it never has |
| `src/store.rs:1062` (`optional_routing_address_by_id`) | Answers the operator's snapshot, which has no alias claim | Changed: answers the relay author's snapshot with its display alias. Without it, a reply to a relayed message cannot name its author |
| `src/spaces.rs:690` (`local_participant`) | Resolves the operator by id, not by alias | Changed: resolves a relay author by id. Without it, a reply's route fails with "no longer reaches the participant it named" |
| `src/store/operator.rs` `refuse_reserved_creation`, `refuse_pane_mutation` | No other path creates or binds the operator's address or role | Changed: they also refuse the `relay:` prefix and relay roles, so no registration or pane action takes a relay author |
| `src/store.rs:2646` (`thread_agent_participants`) | Skips the operator | Unchanged. The relay author stays a participant, so a reply to the whole thread reaches it and capture relays it |
| `src/store.rs:2696` (the routing roster) | Leaves out `operator:local` by name | Unchanged. The roster joins `alias_claims`, and a relay author holds none, so no local agent can name it |
| `src/store.rs:2731`, `:2756`, `:2845`, `:2965`, `:3097`, `:3227`, `src/store/channel_routing.rs`, `src/store/direct_messages.rs`, `src/store/coordination_review.rs:119` | Channel posts, operator selection, recipient-less posts, terminal-relay attention, direct messages, and review requests | Unchanged. Each also requires `author_kind = 'human'` or the literal `operator:local`, and a relayed message is `AgentSession` from another address |
| `src/message.rs:354`, `:366` (send outlooks) | Skips the operator | Unchanged. A relay author's alias is in no projection, and `stored_recipient_outlooks` leaves out an alias it does not list |

Each changed site is a shared store path, so commit R3 is [H]. Older runtimes open the same main database and see a relay author as one more paneless address in a `rex:` namespace, which no Herdr code path reads.

**Names.** A remote recipient is `@name@host` in the leading run of `namedrop send`. Names stay local to their Namedrop. Only `send`'s parse takes the host list, through `parse_mentions_in(source, &hosts)` in a Rex home. A leading token `@<alias>@<label>`, with `<alias>` valid by `validate_alias` and `<label>` exactly a listed host, is a remote recipient. With any other label it fails the send with the error above. Every other reader keeps `parse_mentions`, which is `parse_mentions_in(source, &NoRemoteHosts)`: the prompt shorthand's `parse_prompt_mentions` and `carries_leading_mentions`, which must agree with each other, `literal_leading_mentions`, which escapes a reply body's leading mentions, the final reply, the dashboard, the terminal store, and the handoff preview. So `@bob@devbox hi` typed into an agent's prompt stays literal, as today, and no reply body can add a remote recipient. Remote recipients in the prompt shorthand are deferred. Under `parse_mentions`, `@a@b` reads exactly as today. In the leading run the token is not a mention, so it ends the run, and alone it is today's `MISSING_RECIPIENTS` error. In a body, in every home, `mention_token_end` stops at the second `@` (`src/message.rs:2346`, `:2404`), so `@bob@devbox` is a context mention of local `bob`. A table test pins these readings through `parse_mentions` and through `parse_mentions_in` with `NoRemoteHosts`.

**Who may relay.** A principal that B's server admits can already type into any of B's blocks with `block.event`, so the relay grants nothing the connection does not. It does let a message arrive under a sender's name, and it lets the caller collect replies for the `host_id` it names. B accepts `peer` principals with B's own uid and `tailscale` principals, which Rex admits only for the device owner unless the owner widened it (Rex's docs for `allow_tailnet_members` and `--tailnet-allow-identified`). B refuses every other kind. It accepts `anonymous` principals, which a `--allow-anonymous` TCP listener admits, only while `<state>/relay/allow-anonymous` exists, a file only the stand-in creates. An environment variable would pass to every daemon the bridge starts. A principal can name any `host_id` as its own, and then collect, and with `ack_through` drop, the replies B keeps for that `host_id`. The delivered envelope names the sender's display host and the principal's display name.

**Workers.** On A, `namedrop relay worker --endpoint <A> --host <label>` runs one per label, holding `<state>/relay/<sha256(endpoint, label)>.lock`, through `daemon::command`. The send and A's capture start it after they commit the `outbound` row, and the bridge starts it at connect when items are queued. It runs the `rex` calls, writes outcomes to `relay.sqlite3`, inserts collected replies, and schedules the sender's mailbox worker. So a slow network call never holds a space's local mail, as it would inside the status worker. On B, the bridge runs `relay accept` per call. On both, the status worker runs `relay::capture` after each drain. Each worker writes its own rows (a label's rows, a call's keys, a message's relay item), and SQLite's transactions order them.

**Testing without a second machine.** The stand-in runs two scratch servers on one machine, each with a scratch `HOME`, state directory, and config. Server A listens on a Unix socket. Server B listens on a Unix socket and on `--extra-listen 127.0.0.1:<port> --allow-anonymous`, and B's state directory holds `relay/allow-anonymous`. Each exports `REX_SERVER` naming itself, and each Namedrop has its own state directory. A's `rex` runs with `REX_HOME=unix://<A's socket>` and a scratch `HOME`, because the `rex` CLI caches its home's host list under `HOME` (measured), and `rex hosts add standin-b 127.0.0.1:<port>` then makes `-S standin-b` reach B (measured). B lists no host for A, so every reply must come back by A's pull. A delay-and-reset forwarder between A's `rex` and B's port exercises timeouts and drops. A `LocalProcesses` double that answers nothing for B's pids, and a forwarded socket to B, show that A refuses to type into B even though B is on the same machine. TLS to the user's real server needs the user's approval and is not tested. Namedrop's TLS path is the `rex` CLI's, unchanged. The stand-in is macOS to macOS, so a Linux B (`SO_PEERCRED`, `/proc`, its default socket) gets no live check before release.

### Interface depth

The adapter's surface is `connect`, `Place`, `Deferral`, `RexEvents`, and the relay's `receive`, all `pub(crate)`. Only `mux::connect`, `mux::home`, `deferred_commands`, `rex_bridge`, and `relay` call them, so the rest of the crate reaches Rex through the `Mux` traits and `Home`. Behind it sit a WebSocket client, the local proof, the `seq` ordering rule, the stale-record rule, incarnations, the binding files, the nearest-server rule, and the event decode table. No Rex wire type crosses that surface.

The core sees five additions. `handle_mux_event` is a public entry over the private `handle_event`, with a typed error. `Resync` and `SpaceClosed` add two handlers that reuse `reconcile_worktree` and the existing address removal. `Marker` names the status queue's four input shapes. `daemon::command` replaces each spawn's own environment handling. `parse_mentions_in` takes a host list that only `send` in a Rex home passes. In Part 2, a relay author is a second paneless author beside the operator, recognized at four store sites. The session model in `model.rs` does not move, and `resolve_session`, `locate_agent_session_pane`, the popup target, and `candidate_reach` do not change.

## Synthesis decision

The design came from an arena of two candidates, each told to take a different whole shape. Candidate 1 (Opus) put the binding, the transport, and its own scheduler inside a Rex-only adapter and bridge. Candidate 2 (Sonnet) kept the adapter thin, moved the binding into the core through a new `SessionClaim::Bound` variant and a decorator, and made the bridge a translator into the existing status queue. A Sonnet cross-judge scored both (see [Cross-judge verdict](#cross-judge-verdict)).

Candidate 1 is the base. Its binding keeps the core's session model whole. The survey reports `Reported` exactly when Namedrop's own hook bound the live program, so no core reader of sessions learns that a session can come from two places.

From candidate 2 the design takes one connection per port call, the bridge as a translator into the existing per-space status queue, the rule that only `claude-code` and `codex` are agents, one module that chooses the multiplexer, flushing only for activated spaces, and declining a prompt whose budget cannot cover the confirm window.

Revision 2 drops candidate 2's session-vouched fallback for Rex. On Rex, a binding needs the hook's ancestry, so a process whose ancestry fails has no binding to vouch with, and the fallback only let a daemon pass as the block it started in.

On restart detection the design keeps candidate 1's answer. It runs the same idempotent `Resync` on every connect and lets the incarnation prove which programs were replaced. `observe_agent_attempt` (`src/delivery.rs:351`) holds any in-flight attempt whose pane no longer reports its session, before it looks at the restart flag.

## Tradeoffs accepted

- We accept session bindings in per-block JSON files outside SQLite, in exchange for no change to `resolve_session`, `locate_agent_session_pane`, the popup target, or `candidate_reach`. A store reset does not clear them. That is harmless, because a binding without a registration does nothing.
- We accept that the Rex adapter holds a path to Namedrop's state directory, which the Herdr adapter never needed, in exchange for keeping incarnations and bindings out of the core.
- We accept that an agent can register only after its first SessionStart or UserPromptSubmit hook, and only when that hook's agent is the block's foreground program or sits behind a launcher made only of shells and another executable, in exchange for a binding that only the agent's own hook can write. Behind a launcher, the first prompt may have to come from the person, because SessionStart can run before any status record exists (measured).
- We accept that a hook wrapper installed before revision 4's commit 17 never binds on Rex, because it passes no `--runtime`, and Doctor says to run `integration install` again.
- We accept that Codex in its default daemon mode cannot register on Rex, as on Herdr, with the same notice and fix.
- We accept two non-blocking peer probes and a process walk, about 0.1 ms in total, on every Namedrop process that inherits `REX_BLOCK`, which on the measured machine is every Herdr process. No request is sent. In exchange the nearest server is chosen correctly.
- We accept that a Rex server must export `REX_SERVER` for Namedrop to work in its blocks, as Rex's own CLI needs.
- We accept that `CONFIRM_WINDOW` holds the pane lock for up to 5 s on Rex, in exchange for a prompt that is either confirmed or held before the lock is released.
- We accept a hand-rolled WebSocket client of about 250 lines that skips the `Sec-WebSocket-Accept` check, in exchange for no new crate. It speaks only to a proven-local Unix socket, and the check needs SHA-1, which no current dependency provides.
- We accept that `error` and unknown states never admit a prompt, although the protocol says to treat an unknown state as `idle`.
- We accept that badges, notifications, popups, keybinding reloads, and live peek are unsupported on Rex until step 6 or later.
- We accept a residual window between the facts read and the agent's own `working` report. The record read `idle` 220 times in the 35 ms after an Enter ack (review 1's measure), because the window is the agent's submit-to-`working` latency, 17 to 403 ms here. A person who submits a prompt by hand inside it makes the agent report `working`, and that change could confirm a mention typed into a busy agent. Herdr has the same exposure. Closing it needs an atomic paste-if-idle from Rex.
- We accept that a line pasted after its agent died, between the facts and the re-read, stays in the editor of the shell that reads the terminal next, where a person's Enter would run it.
- We accept that an agent that dies in the 0.1 ms between the re-read and the Enter gets the Enter too, so the shell that reads the terminal next runs the line. Closing it needs the same atomic paste-if-idle from Rex.
- We accept two marker shapes in the status queue, and two failure classifiers, one per shape, so Herdr's worker keeps its exact behavior.
- We accept polling for replies, two `rex` processes every 30 s per label with an open thread, for up to 24 hours after the last activity. That is at most 5,760 processes a day per label, about 40 ms each (measured cost). A reply written later waits at B until A's next send to B, or expires after 7 days.
- We accept a 60 KiB limit on one relayed message, set by the `rex` CLI's 64 KiB request limit.
- We accept that a host can be named in a mention only when its label is made of ASCII letters, digits, `-`, and `_`, although Rex accepts more.
- We accept that B believes the `host_id` A reports for itself, so a principal B admits can collect, and with `ack_through` drop, the replies B keeps for another sender's `host_id`. Any such principal can already type into B's blocks.
- We accept that a label removed between the worker's check and its `rex` process falls back to an address of that spelling for one process, because `-S` has no label-only form.
- We accept that `@bob@devbox` in a message body is a context mention of local `bob` in every home, as it is today.
- We accept a relay author as a second paneless author special-cased at four store sites, in exchange for replies that use the store's own reply, final-reply, and thread paths.
- We accept two `rex` processes per relay call, about 80 ms locally plus the network round trip, in exchange for Rex owning tailnet TLS and admission.

## Alternatives considered

- **The core owns the binding** (candidate 2's shape). A `SessionClaim::Bound` variant, made by a decorator around every Rex connection, resolves `Unreported` from a store. More of the system learns about incarnations. It loses on depth.
- **A per-call `rex` CLI transport.** It costs 19.5 to 26.9 ms per call (median), and a CLI tap has no `seq`, so the gate's order cannot be proven (measured). It cannot meet the gate contract.
- **A held control connection per `RexMux`.** It saves 0.7 ms per call. It costs a reader thread, a pending-request map, reconnect logic, a stale socket after a server restart, and a ping duty.
- **Choosing by the nearest *pane* anchor** (revision 1). A Herdr process with no pane has no Herdr anchor, so the Rex claim won. A server is the one ancestor every process of a multiplexer has.
- **`terminal::parent_pid` through `PROC_PIDT_SHORTBSDINFO`** (revision 2's commit 3). It changes what `verify_inherited_pane` accepts for Herdr. `mux::home` walks with its own reader instead.
- **Recognizing servers by executable name.** The peer pid of the claimed socket proves the same thing without trusting a name.
- **Finding a server from its process's open sockets** (`proc_pidfdinfo` on macOS, `/proc/net/unix` on Linux). It would find a server that exports no `REX_SERVER`, at about 100 lines of platform code. Rex's own CLI misroutes in such a block, so the case is a Rex misconfiguration, and a clear refusal is enough.
- **Trying every endpoint already in the store** (review 1's suggestion for finding 6). It opens the database on every hook in a Rex block, and it still fails on a fresh install.
- **A session-vouched fallback for Rex** (revision 1). See the synthesis decision.
- **Binding when the foreground is any ancestor** (revision 1). A nested agent's hooks pass, and so do a second daemon-mode Codex block's hooks for the first block.
- **Binding when the hook's agent is the foreground pid** (revision 2). It refuses every agent behind a launcher that stays as its parent (measured with the npm launcher).
- **Binding any hook agent in the foreground's process group, with only the foreground's incarnation** (revision 3). An agent that an MCP server starts shares the group and binds, and a launcher that outlives its agent keeps the binding true after the agent died (review 3, re-probed).
- **Telling runtimes apart by the hook's environment or executable.** A nested agent inherits the outer agent's environment, and Claude Code's executable is named for its version. The wrapper knows its runtime because the installer wrote it.
- **The `REX_SESSION` exception for the mailbox wake** (revision 3). A process with both `HERDR_*` and `REX_SESSION` had no rule for which claim wins, and the hook that sent the mention can prove its home and start the wake itself.
- **Relayed messages authored by the operator.** It needs no new author, but B's agent would read a remote agent's text as the person's, with the person's authority, and B's person would see it as their own in the dashboard.
- **Relayed messages authored by envoys.** An envoy stands for an address in another space of the same endpoint (`src/store/bridge.rs`). Sending to one writes a copy into that space, with bridged threads, labels, and reflected outcomes that assume a live multiplexer space, which a remote sender does not have.
- **One relay author per space.** A snapshot by id has one alias, so one author could not name each remote sender.
- **Normalizing `ESC[nC` in `delivery.rs`'s `cells()`.** It would change Herdr's parser for a Rex shape. The adapter owns the port's contract.
- **Retiring a gone space inside the shared `pane_closed`.** It would change Herdr's pane close. `SpaceClosed` is produced only by the Rex bridge.
- **Restart detection by a stored `instance_id`.** The incarnation already proves which programs were replaced.
- **`tungstenite`.** It brings an HTTP stack and a random-number crate for one fixed local protocol.
- **Storing the binding in Rex.** Rex has no per-block key-value store, and a block label is visible and editable by people.
- **Clearing a held pointer with Ctrl-U.** It types into a program the gate no longer recognizes, or into an agent whose last input it could not confirm.
- **Relay through an `init.lua` action that runs `rex.exec`.** It needs Namedrop's Lua in the remote config first, it shares the server's 30 s action budget, and a config reload fails it mid-call (Rex's docs).
- **Relay through `rex -S <host> run --wait -- namedrop …`.** Each message would create a visible block in a new window on the remote server.
- **Relay over Namedrop's own network protocol.** It adds a listener, authentication, and TLS that Rex already provides.
- **B pushes replies through its own `rex hosts`** (revision 2). It fails when only A can reach B, and when B lists no host for A, and the host list stores no `host_id` to match (measured).
- **Passing the host's address, or `host:<id>`, to `-S`.** An address is what finding 1 forbids, and `-S` refuses a host id (measured).
- **Sending from the status worker** (revision 2). A 35 s call would hold that space's local mail.
- **Relay tables in the main database.** They need a schema bump, and a runtime refuses a newer schema (`src/store.rs:8023`), so every older runtime that shares the state directory would stop.
- **Typing into the remote block over `-S`.** The decided model forbids it, because the gate's facts, the incarnation, and the lock would cross machines.

## Answers to the questions

### 1. `emulate_prompt`'s order holds on Rex

Rex's `program_status_changed` events reach a subscriber in the order the gate needs, when the tap, the facts, the paste, and the Enter share one control connection. `gate_probe.py` ran that order against live agents.

- **Order.** The tap is `session.attach`. The facts are `block.method program_status` and `process`. The paste and the Enter are two `block.event` requests. Their acks came 0.2 to 0.7 ms apart (measured). Every facts response carried `seq 1`, and every `working` event carried `seq 2` or `3` (measured). So a `seq` above the first facts response's `seq` is the boundary the tap measures from. Revision 2 added a `process` read between the paste ack and the Enter.
- **Latency.** Claude Code reported root `working` 17.3 to 140.7 ms after the Enter ack, median 21.6 ms, in 9 of 9 runs. Codex took 18.8 to 403.1 ms, median 25.2 ms, in 5 of 5 runs (measured). The latency is the agent's own. `CONFIRM_WINDOW` stays 5 s, about 12 times the slowest run.
- **Busy agent.** Codex reported `working` for 33 s on a story. A second gate run during that time declined at the facts, and the word it would have sent never appeared on Codex's screen (measured).
- **Blocked agent.** Claude Code at a permission prompt reported `{state: blocked, kind: permission}`. The gate declined at the facts (measured).
- **Tool calls.** While Claude Code and Codex ran `sleep` as tool calls, every 0.5 s sample showed `process.foreground.pid` equal to the root record's `owner.pid` (measured). The stale-record rule does not flap during a turn.
- **Stale record.** `kill -9` left a null foreground 0.34 ms later, and `process_changed` lagged about 110 ms (review 1's measure). The facts decline, and `pane_agent` treats the block as a free terminal.
- **Between turns.** Claude Code reported `done` 0.73 to 1.01 s after the Enter for each one-word reply (measured). It never stayed `working` after a turn.
- **Confirmation stays in the gate.** A prompt the tap cannot confirm must be held before the lock is released.

### 2. The incarnation is a pid and a local start time

Rex reports no start time. `process` returns `child`, `foreground`, and `ancestors` with `argv0`, `cwd`, `invoked_path`, `name`, `path`, `pid`, `script`, `user`, and `user_id`, and status records carry `owner {pid, name}` (measured). Claude Code's process name is its version, and so is its executable's basename (`2.1.323`, measured), so a name does not identify a program across upgrades.

The incarnation is `<pid>@<start>`, for the block's `process.foreground` pid, read locally. On macOS, `proc_pidinfo(PROC_PIDTBSDINFO)` gives `pbi_start_tvsec` and `pbi_start_tvusec`, which were stable across reads and readable from inside Codex's sandbox (measured). On Linux the start is `<boot id>:<ticks>`, from `/proc/sys/kernel/random/boot_id` and field 22 of `/proc/<pid>/stat` (measured in Docker).

Namedrop reads it only for a block of a proven-local server whose `host_id` is the server's. When the start cannot be read, there is no incarnation and no fallback. `bind_session` refuses to write, and the survey reports `SessionClaim::None`, so the agent is visible but cannot register, and Doctor can say why.

### 3. The session record is a per-block file the hook writes

A Claude Code SessionStart hook ran as a child of the `claude` process, which was the block's foreground and the record's owner. Its environment had `REX_BLOCK`, `REX_SESSION`, `REX_SERVER`, and `CLAUDE_CODE_SESSION_ID`, and its stdin had `session_id` and `source: "startup"` (measured). Codex 0.163's hooks ran as children of `codex`, and through the npm launcher as children of the native `codex` in the launcher's group (measured).

- **Shape.** `{block, endpoint, space, foreground, agent, runtime, session, bound_at_ms}` as JSON, at `<state>/rex/sessions/<block uuid>.json`, in the install's state directory.
- **Writer.** Only `Place::bind_session`, from `run_context_hook` for SessionStart and UserPromptSubmit, under the group, launcher, and runtime rule. A temp file and a rename, and an unchanged record is not rewritten.
- **Readers.** The survey and the gate, through `pane_agent`, and a sandboxed command's home, through the `Denied` row.
- **Replacement.** `/clear` and an in-place resume run SessionStart with a new `session_id` in the same process (inferred from Claude Code's hook contract), so the incarnation stays and the session changes. A new program has a new incarnation, and the old binding stops matching.
- **Deletion.** `RexEvents` calls `Bindings::forget` when it decodes `block_closed`. A file left behind while no bridge ran is harmless, because block IDs are never reused.
- **The producer of `Unreported`.** The Rex survey, through `pane_agent`, for a live agent whose incarnation matches no binding.

### 4. The budget is 5 s per request and 4 MiB per call

- **Timeout.** Each request is held to the lower of the budget's timeout and `NAMEDROP_REX_TIMEOUT_MS`, which defaults to 5 s and is capped at 60 s. Opening the connection counts as one request. The Stop wrapper sets 250 ms, as it does for Herdr, and `daemon::scrub` removes it from every daemon. The peer probes in `mux::home` are non-blocking connects bounded at 20 ms each.
- **Output cap.** 4 MiB of received bytes per port call. The server's own message limit is 1 MiB (the welcome's `limits.max_message_bytes`, measured).
- **Screen reads.** `format` returns the whole scrollback and has no line-count argument (measured). `screen` calls `format` with a small `max_bytes` to learn the result's `id` and `total`, then one `blob_read` of the last 960 KiB, and takes the requested lines from it. Widening backward when one window holds too few lines is deferred, because a 60-line prompt-box read does not need it (inferred).
- **The confirm window.** `deliver` declines a prompt with `Declined(AfterDelay)` before any byte when the budget's timeout is shorter than `CONFIRM_WINDOW` plus 1 s.
- **`within`.** A `RexMux` over `Within { wire, allowance }`, as Herdr's does. `Budget::tighter` and `narrowest` move to `mux::model`.

### 5. Namedrop speaks `rex.control.v1` directly, with no new crate

The Rex server answers HTTP on its Unix socket and documents its own API at `GET /llms.txt` (measured). The control connection is a WebSocket at `GET /api/control` with subprotocol `rex.control.v1`, one JSON object per text frame. Requests carry an `id`, responses carry the same `id` and a `seq`, and events carry a gap-free `seq`.

| Path | Median | p90 |
|---|---|---|
| `rex version`, the process start alone | 16.0 to 22.9 ms | 17.4 to 26.0 ms |
| `rex block call program_status` | 19.5 to 26.9 ms | 20.9 to 30.7 ms |
| Fresh WebSocket: connect, hello, `session.attach`, `block.method` | 0.8 ms | 1.1 ms |
| `block.method` on a held connection | 0.1 ms | 0.2 ms |

A stdlib-only Python client of about 120 lines did every request in these probes (measured). The Rust `ws.rs` writes over any `Read + Write`, so its tests run over in-memory pipes and `UnixStream::pair`. Masking keys come from `libc::getentropy`.

### 6. The four Herdr-shaped leftovers

- **`Applied::Opened(serde_json::Value)` stays.** Rex answers `Applied::Unsupported` for `OpenPopup` in steps 4 and 5. Step 6 returns `Opened` with the `session.new_layer` reply.
- **`Admit` is deleted**, with `PromptRequest.admit` and its three setters at `src/delivery.rs:774`, `src/integration.rs:736`, and `src/health.rs:767`. Nothing reads it.
- **The pane lock key stays** `sha256(json [session_socket_id, pane])`. A Rex `session_socket_id` starts with `rex:` and is canonical per server, so the two kinds cannot share a lock file, and two spellings of one server cannot split one.
- **Live peek stays Herdr-only.** `data.connect` works, and its stream starts with a binary Ghostty snapshot followed by raw VT output (measured). Rendering that needs a terminal emulator. Opening one also sends `process_changed` (measured).

The effects map as follows. `Identify` is `session.set_block_label "@alias"`. `RenameTab` is `session.set_window_label`. `Badges`, `Notify`, `OpenPopup`, and `ReloadKeybindings` are `Unsupported`.

### 7. The nearest multiplexer server wins

See [One module decides where a process runs](#one-module-decides-where-a-process-runs) for the rule and its measured table. Herdr passes an outer `REX_BLOCK` into its panes (measured in the isolated Herdr's pane) and into its plugin processes (review 1's `ps -Eww`). Rex passes inherited Herdr names into its blocks (measured). The rule needs no pane on either side, any doubt about Herdr keeps a process on today's Herdr path, and a sandbox's denial is its own row.

### 8. One bridge per server feeds the existing worker

- **One process per server.** `namedrop rex bridge --endpoint rex:/…` holds `try_lock` on `<state>/rex-bridge/<sha256(endpoint)>.lock` for its whole life, so a second start exits 0. The endpoint is canonical, so two spellings of one server cannot start two bridges.
- **The stream.** One connection with `session.attach_all {event_scope: "all"}`. It delivered every event in the decode table (measured).
- **How it calls lifecycle and drain.** It does not. It enqueues `Marker::Event` into the per-space status queue and spawns the status worker, which runs `handle_mux_event`, then drains, with its existing `worker.lock`, backoff, draft rechecks, `DrainReport` timers, and `failing.json`.
- **Reconnect and reconcile.** When the stream ends, the bridge reconnects with backoff and queues `Resync` for every activated space on the endpoint, unless the server's `host_id` changed. A space that answers `Gone` is retired.
- **Restart detection.** A restart ended the control connection with no close frame, killed each block's program, and kept the block IDs with a fresh `login` child and no status records (measured). Every program has a new incarnation, so the survey reports no session for any of them, and `observe_agent_attempt` holds each in-flight attempt.
- **The relay.** The bridge is also the relay's receiving client, because it already holds a connection to its server.
- **Starting with no plugin host.** `rex_bridge::ensure` is a `try_lock` probe and a spawn through `daemon::command`. The Rex runtime builder calls it, so any Namedrop command or hook in a Rex block starts the bridge. Step 6's Lua module adds a top-level `rex.exec` that calls it, which runs at every server start and `rex config reload` (measured).
- **Stopping.** SIGTERM or SIGINT stops it between flushes. A server unreachable for 120 s ends it with exit 0. A replaced runtime re-execs between flushes. The lock dies with it, and every marker it writes is idempotent.
- **Liveness.** The bridge pings after 120 s of silence and ends the stream after 60 s without a pong.

### 9. Remote Rex hosts are in the first release

The decision overturns revision 1's answer. Typing stays on the block's machine, and mentions relay between Namedrops over `client.perform`, with replies pulled by the sender's machine. See [Remote hosts](#remote-hosts-typing-stays-local-mentions-relay). The relay is not impossible or unsafe on the evidence, so no other transport is proposed.

## What steps 4 and 5 need from step 3

Step 3 is final at `e68deeb`, and its last code change is `17c580d`. `locator::home` is the set predicate. It picks Herdr's set when any of `HERDR_SOCKET_PATH`, `HERDR_WORKSPACE_ID`, or `HERDR_PANE_ID` is set, even empty, and the neutral set otherwise. An explicit flag wins for its own value. `state_directory` reads the flag, then `HERDR_PLUGIN_STATE_DIR` when it is set, and otherwise the state name of the set `home` chose, with no default. So a Herdr set never reads `NAMEDROP_STATE_DIR` (`17c580d`). `Locator::pass` writes Herdr's names for any value. Step 3 already gives this design four of the seven things it needs.

1. **The set predicate as one function.** `locator::home` exists. `mux::home::resolve` calls it as its last branch, after the Rex test.
2. **Reads of one set at a time.** `Locator::resolve` reads the endpoint, space, and pane from one set.
3. **Set-scoped envelope readers.** `Locator::in_envelope` reads a `collab/v1` tool environment by the same rule.
4. **One field per location.** Each `--herdr-*` flag is a clap alias of its neutral flag, so step 4 parses the endpoint field's value for its kind, whatever spelling filled it.

Step 4 makes the other three changes itself, in the commits named.

5. **`Locator::pass` takes the endpoint's kind** (commit 12). Herdr writes the Herdr set and clears the neutral one, as today. Rex writes the neutral set and clears the Herdr one, so a Rex endpoint never becomes `HERDR_SOCKET_PATH`.
6. **`state_directory` takes the home's kind** (commit 15), with the Rex order and the install record above. Under Herdr it stays step 3's.
7. **Nothing writes `NAMEDROP_*` into a block's environment.** It holds at `e68deeb`, because `tool_environment` writes only the Herdr keys. Step 4's Rex writer writes neutral keys into envelopes and command lines, never into a block's environment.

## Implementation plan

Every commit compiles, passes `cargo fmt --check`, `cargo clippy --locked --all-targets -D warnings` with and without `--all-features`, and `cargo test --locked --lib`, and ends in a state you can check.

- **[H]** marks a commit that touches a Herdr path, meaning any file Herdr's runtime runs, shared modules included. Run `/verify` on real Herdr before it, which from commit 0b on includes the Herdr-in-Rex lane.
- **Dead code.** Until commit 11, the Rex modules stay out of the library build, for example with `#[cfg(test)] mod rex;` in `src/mux.rs`, so their only callers are tests and no `allow(dead_code)` is needed. Commit 11 makes the module unconditional when `mux::connect` calls `rex::connect`. `mux::processes` joins the build with it, and `mux::home` joins in commit 14. The same rule holds later. A module whose first production caller lands in a later commit stays `#[cfg(test)]` until that commit: `rex/events.rs` (commit 22) until commit 23, and `relay/store.rs` (commit R1) until commit R2.
- **Live Rex checks** run on a scratch server with its own socket, state directory, empty `init.lua`, and scratch `HOME`, started under `env -i` with `REX_SERVER` naming itself, never on the user's server. Under the real `HOME`, a scratch server writes `servers/<sha256 of its listen URL>/start.lock` into the user's Rex directory (found in the review of `exp/rex-adapter-1`, and confirmed by revision 4's own probe server, whose entry was then removed). Commits 5 and 6 start it by hand and run `NAMEDROP_REX_LIVE=unix://<socket> cargo test --lib mux::rex::live -- --ignored --test-threads=1`. From commit 7 on, the testbed's Rex lane starts it, with agents whose hooks are baked to a scratch state directory.
- **Until the bridge (commit 23)**, mail declined with `Declined(OnEvent)` and a registration completed while the agent is idle wait for an event that nothing sends. Step 4's live checks run `namedrop --plugin-state <scratch> reconcile` as the manual wake.

### Step 4. The adapter

Commits 0a, 0b, 1, 2, 4, 5, and 6 were built on `exp/rex-adapter-1` (`95d38fb`) and merged into `experimental` in PR #196 (`280779db`). Step 3 merged before them in PR #195. Revision 4 keeps their content. Commit 6a is the next to build. It fixes what their review found before commit 7 builds on them.

0a. **Fixtures.** The six real `vt` captures under the module's test data, with the box state each screen shows, scrubbed of private paths and names. A `delivery.rs` test reads each capture with blank runs as spaces and asserts its box state, and the same test fed the raw captures fails on `codex-empty` and `codex-draft-fresh`, as the commit message quotes.
0b. **The Herdr-in-Rex testbed lane.** `testbed test --inside-rex` starts a scratch Rex server under `env -i` with `REX_SERVER` naming itself, runs the profile's `herdr server` inside a scratch block of it with that block's `REX_*` kept, runs the normal checklist, and stops both. `clean_environment` drops `REX_*` today, so this lane is the only one that sees a Rex claim. Done when the lane passes at the base, before any step 4 change.
1. **Delete `Admit`** [H]. Remove `PromptRequest.admit` and its three setters. Done when `grep -rnw Admit src` finds nothing and the gate and delivery suites pass.
2. **Move the budget helpers** [H]. `tighter` and `narrowest` move to `mux::model`. Done when the Herdr process tests pass unchanged.
3. **Dropped.** Revision 2 changed `terminal::parent_pid` here. `mux::processes` (commit 9) walks instead, and `parent_pid` stays.
4. **`TapChange` in the gate.** New gate tests cover a program change after the facts (declined, nothing pasted) and after the paste (`Ambiguous` at once). Done when the old and new gate tests pass.
5. **`rex/ws.rs`.** Tests over in-memory pipes cover the upgrade, masking, a 70 KiB message with a 64-bit length, a fragmented message, ping to pong, close, and an oversize frame. The live check reads a `welcome` from the scratch server.
6. **`rex/control.rs`.** Tests against a scripted server thread cover hello and welcome, request ids, an event buffered while a request waits, a timeout, cancellation within 50 ms, the 4 MiB byte count, and a server that closes mid-request.
6a. **Control and lane fixes.** From the review of `exp/rex-adapter-1`. In `rex/control.rs`, `next_event` with a zero wait makes one non-blocking read of the socket instead of none (`:153-163`, and the deadline filter at `:215-219`), so the gate's pre-paste check (`src/mux/gate.rs:83`) sees a `working` event that has already arrived. `next_event(Duration::MAX)` adds with `checked_add` and waits with no deadline instead of panicking (`:157`). `open` runs under the same budget and cancellation as a request (`:28-35`, `:84-87`), so a cancel during the upgrade is `Cancelled`, the whole open fits in one timeout, and a zero timeout fails at once with `Timeout`. Any fault other than `Fault::Rex` marks the connection dead, so a write timeout in the middle of a frame (`:191-194`, `ws.rs:274`) leaves no half-written frame in front of a later request. An `EINTR` read is retried, not turned into `Fault::Closed` (`:232-233`, `:284`). A `ws.rs` test pins that an oversize frame is refused by its header before any payload byte is read (`:223-228`). In the testbed, the `--inside-rex` lane stops its scratch server and removes its profile when a run fails, and before it starts it removes a stale `/tmp/nd-tb-rex-*.sock` that refuses connections (`testbed/lib/rex_lane.py:71`, `:130`), so the claim in `.claude/skills/verify/SKILL.md:132-133` that it always stops holds. Tests: a scripted-server test for each control fix (an event queued before a zero wait, a `Duration::MAX` wait, a cancel during the upgrade, a zero-timeout open, a request after a write timeout that fails `Closed`, and a read interrupted by a signal), and a lane selftest that fails a run mid-checklist and then finds no server, socket, or profile left. Done when they pass and the Herdr-in-Rex lane passes.
7. **The testbed's Rex lane.** `testbed rex up --link <worktree>` makes a profile with its own `HOME`, `XDG_*`, `CODEX_HOME`, and Claude Code config, with the login links that `auth_links` already makes. It runs the build's `integration install` into the profile, so every hook wrapper bakes the profile's state directory. It starts a scratch Rex server under `env -i` with `REX_SERVER` naming itself and the profile's environment, and prints the `rex -S … new … -- claude` and `-- codex --no-daemon` lines. Codex's model settings go in the profile's `config.toml`, because a `-c` option forces Codex's embedded mode (measured), and the daemon-mode check must start Codex with no option. The lane answers each agent's first-run prompts with `rex send-key` as measured: Claude Code's folder trust (down, Enter), Codex's folder trust (Enter), and Codex's hook review (down, Enter). `testbed rex down` kills every session and stops the server. No Rust change. Done when `up`, a Claude Code block and a Codex block whose Namedrop SessionStart hooks each leave a hook-run record in the profile's state directory, and `down` leave no server or session behind.
8. **The endpoint and the connector, with Rex refused** [H]. `Endpoint::parse`, `Endpoint::unix_socket`, `rex/local.rs`, `mux::connect`, and `mux::Connector`, where a Rex endpoint answers `MuxError::Unavailable("the Rex adapter is not built yet")`. The deferred worker's `execute_request` and its four probes, the reminder clock, and the dashboard move to them. Live peek refuses Rex. Tests pin `""`, `"default"`, and `"/tmp/h.sock"` as Herdr and `"rex:/tmp/r.sock"` as Rex, assert that their lock keys differ, and run `prove_local` against a scripted server and a forwarder thread, which it refuses. The live check proves the scratch server local and refuses a forwarded socket to it. Done when `grep -rn "herdr::connect(Some" src` finds only `src/mux.rs`.
9. **Reads.** `mux/processes.rs` (parent, group, executable, start, `is_shell`, the boot id on Linux, and a zombie read as no process), `rex/wire.rs` with the `port_ansi` allowlist, `rex/agent.rs` with `pane_agent` and `shell_of`, the read side of `rex/binding.rs` with the binding's `foreground`, `agent`, and `runtime`, and `RexMux`'s `MuxRead`. Fixtures: `process` replies for idle and busy `login` blocks, a `-- python3 -i` block, a killed agent's stale record, revision 4's `bash` wrapper block before and after its agent's `kill -9`, and review 4's non-reaping launcher block whose agent is a zombie. Every scripted status record carries `updated_at`, which Rex requires. Commit 0a's test switches to `port_ansi`. `rex/tests.rs` pins the requests for each `Scope` and `Depth` pair (the table below), checks `pane_agent` row by row, including a binding whose agent is gone behind a live launcher, a binding whose agent is a zombie behind a live launcher (a free terminal), and a binding whose runtime is not the record's `app`, checks `shell_of` on the idle, busy, `python3 -i`, and `bash -c 'sleep 120; true'` replies, and checks `port_ansi` on the six captures, a tab run, an OSC 8 link, and an unknown CSI, which returns `None`. A Docker test reads the real `/proc` for a process's parent, group, executable, and boot id, and reads a real zombie, a child its parent does not reap, as no process. The live check surveys a scratch session. It shows the agent, its incarnation's start matches `ps -o lstart`, a terminal block reports its shell as available, and a `python3 -i` block reports none.
10. **Input and effects.** `rex/channel.rs`, `Line`, `Keys`, and `apply`. Tests pin the order (proof, `target.resolve`, `session.attach`, `program_status`, `process`, paste, `process`, Enter), skip an event at or below the first facts response's `seq`, decline on a status event whose `seq` falls between the two facts responses, decline on one above it, deliver on `working` after the Enter, hold without an Enter when the second `process` names another pid, treat a paste error as `Ambiguous`, send a prompt's newlines, `\r`, and tabs as spaces, refuse a `Line` with `\n` or `\r` before any byte, hold without an Enter when the bound agent is gone at the re-read while the foreground stays (the launcher case), hold without an Enter when the bound agent is a zombie at the re-read, and hold without an Enter when a status change arrives with a `seq` above the facts' boundary and at or below the paste ack's `seq`. That last test covers the case the review of `exp/rex-adapter-1` found. A `working` event that Rex queued before the paste and Namedrop reads after it is never read as `Delivered`, because the tap reports it as `Program`, so the attempt is held. Further tests pin each key payload and each effect. The live check types into a scratch zsh and prompts a scratch Claude Code on Haiku. It captures the payloads for Escape, Up, and Down before the tests pin them, because only Enter, Ctrl-C, and Ctrl-U were measured. Done when `RexMux` implements `Mux`.
11. **The Rex arm of `mux::connect`** [H]. `mux::connect` calls `rex::connect`, and the Rex modules join the library build. The Herdr arm does not change. Delivery gets `PromptBox::Unread` and `AgentWait::ScreenUnread` for a Rex screen fault, Doctor's label for it, and the draft recheck's retry. Tests: on a Rex endpoint a screen read error and a `port_ansi` `None` each hold the mail while the agent reports `idle`, and a later clean read delivers it. On a Herdr endpoint a failed read with `idle` delivers as today. Done when the ignored live test surveys the scratch session through `mux::connect` with a `rex:` endpoint and `/verify` passes on both lanes.
12. **Kind-aware writers** [H]. `Locator::pass` with the endpoint's kind, `tool_environment` with neutral keys for Rex, `registration_command`, `runner_command`, and Doctor's repair line. `Locator::pass` covers the state directory too. Tests: every existing Herdr line and envelope is byte for byte the same, the Rex ones use `--endpoint`, `--space`, and `--pane`, and a Rex daemon gets `NAMEDROP_STATE_DIR`. `scripts/runtime.py` moves to step 6.
13. **`daemon::command`** [H]. `src/daemon.rs`, every self-spawn in the daemon table, the summary worker's `REX_*` scrub, and the self-spawn boundary test with its four exceptions, the clock's re-exec (`src/reminder/clock.rs:219`) among them. Tests: each daemon's child environment holds today's Herdr names byte for byte, minus the scrub. A spawn of the executable added outside `daemon.rs` fails the boundary test (mutation). Done when `/verify` passes on both lanes.
13a. **One door for test spawns.** `tests/support/mod.rs` gets `namedrop()`, the only `Command` for `CARGO_BIN_EXE_namedrop`, which removes `REX_BLOCK`, `REX_SESSION`, `REX_CLIENT`, and `REX_SERVER`, and `without_rex(&mut Command)` for a spawned program that starts the binary itself, such as a script or the test's own re-exec. All 81 spawns of the binary in 29 test files move to it. Tests that clear the environment first (`tests/final_reply.rs:963`) move too, so the rule has no exceptions. A boundary test reads every file under `tests/` and fails when `CARGO_BIN_EXE_namedrop` appears outside the helper (mutation: a spawn added in a test file fails it). No production code changes. Done when `cargo test --locked` passes and the boundary test fails red as stated.
14. **The home** [H]. `mux::home` joins the build, with `resolve`, the non-blocking probes, and the full table, and the runtime builder's Rex arm. `Home::RexDeferred` exists, and nothing builds it until commit 18. A table test runs every row, including `ECONNREFUSED`, `ENOENT` on a guessed default path, `ENOENT` on an explicit path, a live server on a guessed default Herdr path that is not an ancestor (Herdr), and both `Denied` rows, over scripted processes and probes. A mutation that restores revision 1's rule (a Rex claim wins when Herdr has no pane) must fail that test and the Herdr-in-Rex lane. `ThisProcess` is built only at the binary's entry points, and every function below them takes `&dyn Inherited`, so a unit test passes a scripted `Inherited` and never reads the real environment or sockets. A boundary test fails when `ThisProcess` is named in a test module. Done when both run green and red as stated, the CLI in a Rex-lane block resolves its block, `/verify` passes on both lanes, and the full `cargo test --locked`, integration tests included, passes once when run from a Rex block.
15. **The Rex state directory and the install record** [H]. `state_directory` with the home's kind, the install record, Doctor's check of it, and the Stop wrapper's `NAMEDROP_REX_TIMEOUT_MS=250`. Tests: Herdr's order is step 3's at `17c580d` with no default, and a Herdr set of names ignores an inherited `NAMEDROP_STATE_DIR` (`src/locator.rs:72-83`), Rex's order is the one above, the record round-trips, and Doctor reports a record that differs from the wrappers. Done when `/verify` passes and the CLI in a Rex-lane block finds the profile's state directory with no flag.
16. **The identity sites** [H]. Every site in [Every identity site takes the home](#every-identity-site-takes-the-home), `src/main.rs:1140-1146` among them, and the location-name boundary test, which reads compacted production code only. The UserPromptSubmit hook starts the mailbox wake through `daemon::command` with `Scope::Space`, the installer stops backgrounding it, and `hook mailbox-wake` reads its home. `CapturedStop` carries the Stop hook's space. Tests: each site reads its location from a scripted home, a reader added outside `mux::home` and `locator` fails the boundary test (mutation), a reader split over lines fails it too, the Herdr adapter's test module does not, the UserPromptSubmit wrapper's text is today's minus the backgrounded line, a Herdr wake drains the space today's wake drains, and a Stop worker with stale Herdr names in its environment wakes the space its hook proved. The live check runs the prompt hook in a Rex-lane block, sees its turn phase under the `rex:` endpoint, and sees a mention typed there to a terminal recipient drain through the hook's wake with no `reconcile`. The recipient is a terminal because an agent recipient needs commit 17's binding. Done when `/verify` passes on both lanes.
17. **The binding from hooks** [H]. `Place::bind_session` with its six conditions, `hook_agent`, `Binding` with `foreground`, `agent`, and `runtime`, the call in `run_context_hook` with the hook's `CLAUDE_CODE_ENTRYPOINT`, the wrappers' `--runtime`, and Doctor's line for a wrapper without it. Fixtures: the hook chains with process groups, executables, and entrypoints from revisions 3 and 4 and review 4 (Claude Code, a nested `claude -p` from a tool, Codex with `--no-daemon`, the npm launcher, both daemon-mode blocks, the `bash` wrapper, the three MCP servers, and the `sh -c` MCP server that runs another Claude Code version) and the Linux wrapper's `comm` and executable. Tests: each fixture binds or refuses as the table says, a hook whose runtime differs from the record's `app` refuses, a SessionStart with no record yet binds, a SessionStart whose block holds a stale record of a dead agent binds, a hook whose agent is not the foreground refuses when the binding names the foreground's incarnation as its agent, a hook whose agent is the foreground replaces a binding that names another agent, a Claude Code hook whose `CLAUDE_CODE_ENTRYPOINT` is `sdk-cli` or unset refuses, an identical second call does not rewrite the file, `/clear` with a new session replaces it, and a Herdr home writes nothing. A Docker test runs `hook_agent` through a real `#!/bin/sh` wrapper. The live check binds Claude Code and Codex with `--no-daemon`, binds Claude Code behind a `bash` wrapper and then sees a `kill -9` of the agent leave the block a free terminal with nothing typed, refuses a `claude -p` that a tool or an MCP server starts, and shows that Codex in its default mode never binds and shows the shared app-server notice.
18. **Sandboxed commands defer through the binding** [H]. The `Denied` row's binding proof, `try_defer` for `Home::RexDeferred` and for an explicit Rex endpoint, `ensure_worker` from Rex hooks, and the notice's Rex wording. Tests: a scripted denial with a matching binding is `RexDeferred`, a binding whose incarnation is not an ancestor is an error, `try_defer` writes the request under the endpoint key with the binding's space and block, a command with `--endpoint rex:…`, `--space`, and `--pane` whose socket is denied defers with those flags, and Herdr's `try_defer` is unchanged. The live check runs a `namedrop … send` from a Codex tool under its default sandbox in a Rex-lane block and gets the worker's receipt within the 2 s wait, then runs the rendered `reply` line from a delivered pointer the same way and gets its receipt.
19. **Live check for step 4.** On the Rex lane, start a Claude Code block and a Codex block in one session. Name each through the CLI, mention one from the other, wake with `reconcile`, and see the reply. Then `kill -9` the Claude Code process, queue a mention to it, and confirm that nothing is typed into zsh. Do the same behind a `bash` wrapper that does not `exec`. Record the evidence in PLAN.md and PORT.md. This meets PLAN's step 4 done-when.

The survey requests that commit 9 pins:

| Port call | Rex requests on one connection |
|---|---|
| `survey(All, Spaces)` | `session.list`. Space IDs are session IDs, and `number` is the list position |
| `survey(Space, Spaces)` | `session.list`, for the label |
| `survey(Space or PanesOf, Panes)` | `session.list` for `Space` only, `session.attach`, `session.list_blocks`, then `program_status` and `process` for each block whose `creator_name` is `com.superlogical.terminal` |
| `survey(All, Panes)` | `session.list`, then the row above for each session |
| `survey(CheckoutOf, _)` | None. `MuxError::Unavailable("a Rex session is not a checkout")` |
| `screen` | `target.resolve`, then `format` with a small `max_bytes`, then one `blob_read` of the tail. `Style::Ansi` is `format: "vt"` through `port_ansi` |
| `process`, `tab_label` | `target.resolve`, then `process` or the window's label from `session.view` |

Every row starts with the local proof. Its `server.status`, kept with the proven pid, gives the `host_id` the survey compares. A Rex pane maps as follows. `id` and `terminal` are the block ID. `tab` is the window ID. `space` is the session ID. `cwd` is `process.foreground.cwd`. `title` is the block label. `worktree` is `rex-space:<session id>` with no checkout. `tty` is `None`.

### Step 5. The bridge

20. **`handle_mux_event`, `Resync`, and `SpaceClosed`** [H]. FakeMux tests: a `PaneClosed` retires a name, a `SpaceClosed` retires every name in its space and fails its reminders, a `Resync` that finds the space gone does the same, a `Resync` keeps a `Submitted` attempt whose session still runs and holds one whose program was replaced, and a `Gone` failure is `Definitive` while a timeout is `Retry`. The Herdr `event.*` tests pass unchanged, and the attribution test pins `herdr` for a Herdr endpoint.
21. **The status queue's `Marker`** [H]. `Marker`, `enqueue`, and `spawn_worker` move to `status_queue.rs`, and the worker dispatches on `Marker::parse`, whose `Event` arm calls commit 20's `handle_mux_event`. `MuxEvent` gains serde. Done when the worker's tests pass unchanged and a round-trip test passes for all four shapes.
22. **`rex/events.rs`.** It stays `#[cfg(test)]` until commit 23 calls it. Scripted-wire tests cover each row of the decode table with fixture lines. `title_changed` yields nothing, coalescing keeps one status change per pane, `SpaceClosed` absorbs its space's markers, and a missing pong ends the stream.
23. **`src/rex_bridge.rs`, `namedrop rex bridge`, and `ensure`** [H]. It calls the status queue and the runtime command guard, and the Rex runtime builder calls `ensure`. The loop takes a private stream trait, so a test can script it over FakeMux and a temp state directory. Tests: a second `run` exits at once, each connect queues one `Resync` per activated space, a connect after the `host_id` changed queues none, a flush enqueues and spawns once per space, and SIGTERM stops it between flushes. Done when a scripted `block_closed` retires a name through the real worker.
24. **Live check for step 5.** On the Rex lane, close a named block and see the name go with no manual command. `rex kill` a session with two named blocks and see both names go. Queue a mention to a working Codex and see it drain after `idle`. Restart the server and see in-flight mail to the replaced programs held, then bind a new agent in the same block and name it again. Record the evidence in PLAN.md.

## Part 2: the relay

Part 2 ships as its own PR series, after commit 24's live check and on top of it. Same-host Namedrop works on any host once step 5 lands. A real remote host also needs Namedrop installed there without Herdr, which is PLAN step 6. The stand-in tests the relay before that, with each side's Namedrop run from the build with explicit flags. Part 2 holds only what commit R6's live check needs, and the rest is under [Deferred](#deferred). The rules above for [H], dead code, and live checks apply.

R1. **`relay.sqlite3`.** `src/relay/store.rs` with the six tables, its own `user_version`, and the expiry queries. It stays `#[cfg(test)]` until commit R2 calls it. Tests: it creates from nothing and reopens, outbound items, outbox items, and `inbound` rows expire at their limits, and the main database's `user_version` is 8 after every relay operation.
R2. **Remote recipients in `send`** [H]. `parse_mentions_in` and `NoRemoteHosts`, the host list read from `rex hosts list --json`, and a Rex `send` that validates and queues one `outbound` item per remote recipient. Tests: under `parse_mentions` and `NoRemoteHosts`, `@a@b` alone is `MISSING_RECIPIENTS`, after a valid mention it ends the run, and in a body it is a context mention of `a`, all as today. In a Rex home, `send` reads an exact label as a remote recipient, and a case mismatch, an unlisted label, and a label with `.` or `:` fail with the error that lists the labels. `@bob@devbox hi` as a prompt stays literal, `carries_leading_mentions` is false for it, and a reply body that opens with `@bob@devbox` adds no recipient. A body whose encoded request passes 60 KiB is refused.
R3. **Receiving on B** [H]. The bridge declares `namedrop.relay`. `receive` checks the principal and `expect_host_id` first. `namedrop relay accept` validates, deduplicates, resolves, and inserts with the request id `relay:<A host_id>:<id>:<to>` into the thread `remote_threads` keys by (A's `host_id`, A's thread, B's space), with the 5 s `retry` and the 256 KiB page. The store gets `ensure_relay_author`, `is_relay_author`, `insert_relayed_message`, and the four changed sites in [Relayed messages have a paneless author](#relayed-messages-have-a-paneless-author). Tests script `client_perform_requested` events. Each recipient is accepted once, and two recipients of one A message in one B space each get a message. One A thread that reaches two B spaces gets a B thread in each. A repeat replays the stored outcome, a refusal included. A name in two spaces is `ambiguous`, and an unknown name is refused. An anonymous principal is refused without the allow file and accepted with it, and an unknown principal kind and a wrong `expect_host_id` are refused with nothing stored. Each bad field is refused: a `from.host` with `\r`, an alias with a space, a 129-character id, and 61 KiB of text. A slow accept answers `retry` and then `duplicate`, and the outbox page stops at 256 KiB with `more`. Store tests: a reply and a final reply to a relay author settle at insert, its snapshot by id carries its display alias, a reply's route resolves it, a registration or a pane action cannot take a `relay:` address or role, and the operator's tests pass unchanged. The live check calls `client.perform` on the stand-in with `rex -S standin-b` and sees each result shape.
R4. **The relay worker on A** [H]. One worker per label with its lock, the `server.status` pin sent as `expect_host_id`, `target.resolve`, `deliver`, `collect`, collected replies inserted from the B agent's relay author with their `collected` rows, one 30 s collect interval, and the 24-hour expiry. Tests run a script `rex` on `PATH` that answers each row of the outcome table, and check `outbound`, `collected`, and the sender's mailbox after each. The worker exits when nothing is queued and no thread is open.
R5. **Capture on both sides** [H]. `relay::capture` after each drain in a Rex home, the outbox expiry, and B's Doctor line for expired items. Tests: on B, a reply to a relay author becomes one outbox `reply` naming A's message and thread, and capture run twice writes it once. On A, a reply to a collected message becomes one `outbound` item to that B alias, with `in_reply_to` set to the B message id, and its worker starts. A reply over 60 KiB is cut with the notice. Expired items are counted per remote sender.
R6. **Live check for the relay.** On the stand-in, with B listing no host for A, a mention from A's Claude Code to B's Codex arrives, and B's reply returns to A by A's pull. A's Claude Code answers that reply, the answer reaches B's Codex in the same B thread, and B's second reply returns too. Through the delay-and-reset forwarder, a timed-out send is retried and accepted once. With B's bridge stopped, A keeps the item, and it arrives after the bridge starts. A refuses to type into B's blocks. Record the evidence in PLAN.md.

## Deferred

The Rex conformance suite (revision 1's commit 8), backward `blob_read` windowing, `AgentDetected`, a `namedrop.roster` action for remote name completion, a space part in remote addresses, new mentions from B to a machine B cannot reach, and a live TLS test against the user's real server. From review 3's list: relay delivery acks (the `ack` outbox items, the ack half of `relay::capture`, and `namedrop relay list`), request batching, a second, faster poll tier after activity, a cached bridge client id, matching `prove_local`'s dialed socket to a listener by device and inode, `scripts/runtime.py` finding Rex workers by their neutral names (moved to step 6 with the installer), and Doctor's wording for a changed `host_id`. Also deferred: remote recipients in the prompt shorthand, and a body reader that skips `@name@<listed host>`. The stand-in is macOS to macOS, so a Linux B gets no live check before release.

## What only the operator can decide

- **TLS to the real server.** The relay's live test reaches only the stand-in. A test over the user's tailnet needs the user's approval.
- **Who may relay.** This design accepts same-uid and tailnet principals and refuses anonymous ones. Only the operator can widen that.
- **Registration through hooks.** On Rex an agent can register only after a Namedrop hook ran in it, so an agent started before the install needs one prompt first, and so may an agent behind a launcher, whose SessionStart can run before its status record exists. Codex in its default daemon mode cannot register on Rex, as on Herdr. Those are product calls.
- **The Rex app.** PLAN's open questions about restarting the app and seeing the actions in the palette need the app attached to the operator's real server.
- **Requests to Superlogical.** An atomic paste-if-idle would close the last race in answer 1. A process group in `process` would remove one local read. A server that sets `REX_SERVER` for its own blocks would remove the export the stand-in needs. A `host_id` stored with each host, a `-S` form that refuses anything but a listed label, and a CLI request limit as high as the server's would each simplify the relay.

## Open questions and risks

- Does Codex run SessionStart for a sub-session in the same process? If it does, the sub-session would take the block's binding, because no other process group lies between them. The hook could skip a SessionStart whose `source` marks a sub-session.
- What is the default Rex socket on Linux, and does `SO_PEERCRED` behave? Only macOS was measured live, and the Linux process reads only in Docker.
- Rex attributes a status record from an agent behind a launcher that stays to the launcher: `bash` for Claude Code (measured in revision 4) and Python in review 2. Codex 0.142.2 behind its npm launcher sent no record at all, so the npm case with a Codex that reports is unmeasured.
- Codex 0.163.0 has no `mcp-server` command. A Codex started by an MCP server of Claude Code that is itself `sh -c` passes condition 2 and is refused by condition 5 once the outer agent is bound. That case was not run (inferred from the chain the rule would see).
- An agent can report `done` while a dialog Namedrop does not recognize holds the screen, and a screen that reads `Unseen` then delivers. Review 4 saw Claude Code report `done` with a modal dialog showing whose Enter confirms its option 1. Herdr has the same rule, so this is not a regression. Only a Rex screen fault holds the mail.
- On B, the pointer names a relayed sender `@bob@laptop`, with `laptop` taken from A's self-reported `from.host`. B's agent may copy that into a `send`. It parses as a remote recipient only when B lists a host labelled `laptop`, and that label may name another machine, which would then receive the mention. It stays open for Part 2.
- A Linux B is unmeasured live. The stand-in is macOS to macOS.
- Rex is a preview. The adapter refuses a welcome whose `api_revision` is not 12 until a newer revision is checked, and Doctor reports it.
- A crashed agent leaves its terminal modes behind, so a terminal line or key sent to such a block may not act as it would in a clean shell. Herdr panes have the same exposure (inferred).
- Does Doctor's issue 003 find a daemon-mode Codex in a Rex block through the port? It reads the panes the survey reports (inferred), and commit 17's live check confirms it.

## Cross-judge verdict

A read-only Sonnet judge scored revision 1's two candidates on six criteria and called it close. Candidate 1 scored 23 and candidate 2 scored 24.

| Criterion | Candidate 1 | Candidate 2 |
|---|---|---|
| Delivery safety | 4 | 4 |
| Interface depth and leakage | 3 | 4 |
| Fit with the existing code | 4 | 4 |
| Operational robustness | 3 | 4 |
| Testability and sequencing | 4 | 4 |
| Evidence use | 5 | 4 |

The judge recommended candidate 2 as the base. Its case rested on three weaknesses of candidate 1: Rex knowledge in `main.rs`, a second event loop, and a held connection with a reader thread. The synthesis removes all three, so the remaining difference is where the binding resolves, and on that point this design disagrees with the judge. The judge's factual corrections still stand: the four deferred-worker probes, `handle_mux_event`'s extra arguments, the unbounded `ADAPTER_DEFAULT`, ten `drain_mailboxes` callers, and the inline `mod transport`.

## Evidence

The probe programs lived in the session scratch directory and are not kept. This section keeps the commands and the trimmed output, with private paths shortened. Revision 1's evidence comes first, then revisions 2, 3, and 4, then review 4's measures.

### Revision 1

**Per-call cost** (`cost.py`, 30 calls each, two runs):

```text
rex version (process start only): median 16.0 ms, p90 17.4, min 14.8, max 18.0 (n=30)
rex block call program_status:    median 19.5 ms, p90 20.9, min 18.2, max 22.5 (n=30)
fresh ws: connect+hello+attach+method: median 0.8 ms, p90 1.1, min 0.4, max 1.3 (n=30)
held ws: block.method:            median 0.1 ms, p90 0.2, min 0.1, max 0.7 (n=30)
19:02  up  9:59, 3 users, load averages: 7.13 8.59 11.67
```

**The gate order on one connection**, Claude Code (`gate_probe.py SESSION BLOCK "Reply with exactly the word pong."`):

```text
     1.7 ms facts response seq 1 root {"state": "done", "app": "claude-code", "owner": {"pid": 93037, "name": "2.1.323"}} foreground 93037 2.1.323
     2.2 ms paste ack seq 1 err None, enter ack seq 1 err None, paste->enter ack 0.5 ms
  +    58.2 ms seq=2 id=<root> state=working app=claude-code owner=93037/2.1.323
  CONFIRMED working 58.2 ms after the Enter ack
```

Confirm latencies, all runs, in ms: Claude Code 58.2, 22.4, 20.4, 17.3, 21.0, 19.8, 56.9, 140.7, 21.6. Codex 403.1, 20.3, 18.8, 25.2, 113.2.

The key events that `block.event` accepted. Enter submitted a pasted shell line and every agent prompt. On a fresh zsh block, Ctrl-C stopped `sleep 30`, and Ctrl-U cleared a pasted line before Enter.

```json
{"type": "key", "payload": {"key": "enter", "mods": "", "consumed_mods": "", "text": "\r", "unshifted_codepoint": 13, "composing": false, "action": "press"}}
{"type": "key", "payload": {"key": "key_c", "mods": "ctrl", "consumed_mods": "", "text": "", "unshifted_codepoint": 99, "composing": false, "action": "press"}}
{"type": "key", "payload": {"key": "key_u", "mods": "ctrl", "consumed_mods": "", "text": "", "unshifted_codepoint": 117, "composing": false, "action": "press"}}
```

**The stale record.** Claude Code under zsh, then `kill -9` of its pid:

```text
after kill -9:
{"state":"idle","app":"claude-code","owner":{"pid":70790,"name":"2.1.323"}}
{"foreground":{"pid":39372,"name":"zsh"}}
(5 s and one shell command later, the same record)
```

**A SessionStart hook in a Rex block:**

```text
env: TERM_PROGRAM=rex, REX_SESSION, REX_BLOCK, REX_SERVER, REX_BIN_DIR, CLAUDE_CODE_SESSION_ID
ancestors: 57472 Python (the hook) -> 57374 ~/.local/bin/claude -> 39372 zsh -> 1661 rex server
block process: {"child":39372,"fg":{"pid":57374,"name":"2.1.323"}}; status owner 57374
```

**Environment leaks.** A scratch server started with `HERDR_PANE_ID=outer-herdr-pane HERDR_SOCKET_PATH=/nonexistent/outer.sock` passed both into its blocks, with a `REX_SERVER` naming the default server, not itself.

**Restart** on a scratch server. The stream ended with no close frame, and the old child was gone. The block answered with a fresh `login` child and `{"records":[]}`. `instance_id` changed, and `host_id` stayed.

**Lifecycle events** on `attach_all {event_scope: "all"}`:

```text
new: session_created
new: block_event:process_changed {"payload": {"generation": 2, "foreground": {"pid": 81151, …}}}
split: block_created
exit-in-split: block_event:child_exited {"payload": {"exit_code": 0}}
exit-in-split: block_closed
kill: block_closed
kill: session_destroyed
```

**Starting the bridge from `init.lua`.** A top-level `rex.exec` in a scratch `init.lua` ran at every server start and every `rex config reload`, and a child it backgrounded survived, reparented to launchd.

### Revision 2

Scratch servers ran as `env -i HOME USER SHELL PATH REX_SERVER=unix://<sock> rex server run --listen unix://<sock> --state-dir <dir> --config <empty init.lua>`, with `--extra-listen 127.0.0.1:47298 --allow-anonymous` for host B. They ran with the real `HOME`, which revision 4 found writes into the user's Rex directory.

**The peer pid is the server** (`peer.py`: connect, `getsockopt(SOL_LOCAL, LOCAL_PEERPID)`, then `server.status` on a second connection):

```text
default server   peer 1661  status pid 1661  equal  start 69.2 ms before started_at  probe median 0.005 ms, max 0.053 ms (n=50)
scratch A        peer 6563  status pid 6563  equal  start 24.9 ms before started_at
scratch B        peer 6587  status pid 6587  equal  listeners [unix b.s primary, tcp 127.0.0.1:47298 anonymous]
forward to B     peer 12973 (Python forwarder)  status pid 6587  not equal  same primary listener and instance_id as B
```

The scratch Rex server's log gained no line for 50 bare connects, and the isolated Herdr server's log held no warning or error after the prototype's probes.

**The walk stops at `login`** (`ppid_probe.py`, from a Claude Code tool shell in a Rex block):

```text
1679 bsdinfo (1674, zsh)   short (1674, zsh)    sysctl 1674
1674 bsdinfo fail          short (1661, login)  sysctl 1661
1661 bsdinfo (1, rex)      short (1, rex)       sysctl 1
```

**Herdr inside Rex** (`home.py`, the prototype of `resolve`). An isolated Herdr server (`HERDR_SOCKET_PATH`, `HERDR_CONFIG_PATH`, `HOME`, and every `XDG_*` in a scratch directory, after `herdr status server` named the isolated socket) ran as the program of a block on scratch server A. A scratch plugin linked into it ran the prototype from its startup command and two actions, and `herdr pane run` ran it in a pane.

```text
herdr-pane              chain zsh,herdr,rex    rex depth 2  herdr depth 1           home herdr   0.097 ms
herdr-workspace-action  chain herdr,rex        rex depth 1  herdr depth 0           home herdr   0.083 ms
herdr-pane-action       chain herdr,rex        rex depth 1  herdr depth 0           home herdr   0.076 ms
herdr-startup           chain herdr,rex        rex depth 1  herdr depth 0           home herdr   0.072 ms
rex-in-herdr            chain bash,rex         rex depth 1  herdr live, not ancestor home rex
stale-herdr-live        chain bash,rex         rex depth 1  herdr live, not ancestor home rex
stale-herdr-dead        chain bash,rex         rex depth 1  herdr ENOENT             home rex
plain-rex-block         chain bash,rex         rex depth 1  no Herdr name            home rex
```

The pane's environment held `REX_BLOCK` from the outer block, so Herdr passes it through. In every Rex row, `server.status.pid` equalled the peer pid and the block's `child` was an ancestor.

**`process` in a `login` block:**

```text
idle     {"child":{"pid":92189,"name":"login"},"foreground":{"pid":92197,"name":"zsh"},"ancestors":[{"pid":92189,"name":"login"}]}
running  {"child":{"pid":92189,"name":"login"},"foreground":{"pid":92786,"name":"sleep"},"ancestors":[{"pid":92197,"name":"zsh"},{"pid":92189,"name":"login"}]}
ps       92189 ppid 6563 pgid 92189 root /usr/bin/login; 92197 ppid 92189 pgid 92197 -zsh; 92786 ppid 92197 pgid 92786 sleep
```

**The prompt box from Rex captures.** A test added to a scratch copy of step 3's `src/delivery.rs` ran `prompt_box` on the last 60 lines of each of review 1's captures, raw and with each `ESC[nC` as n spaces. The captures hold only SGR (`m`) and cursor-forward (`C`) sequences.

```text
claude-draft.vt:       raw=Draft  spaces=Draft
claude-empty.vt:       raw=Empty  spaces=Empty
claude-placeholder.vt: raw=Empty  spaces=Empty
codex-draft-fresh.vt:  raw=Unseen spaces=Draft
codex-draft.vt:        raw=Draft  spaces=Draft
codex-empty.vt:        raw=Draft  spaces=Empty
test result: ok. 1 passed
```

**Revision 2's nearest-agent rule on captured hook chains** (review 1's `hooklog` and Codex samples), kept for the record. Revision 3's group rule replaces it.

```text
claude SessionStart       cli      fg 40546  nearest non-shell 40546  bind    (old rule: accept)
claude UserPromptSubmit   cli      fg 40546  nearest non-shell 40546  bind    (old rule: accept), twice
claude SessionStart       sdk-cli  fg 40546  nearest non-shell 59269  refuse  (old rule: accept)
claude UserPromptSubmit   sdk-cli  fg 40546  nearest non-shell 59269  refuse  (old rule: accept)
claude SessionStart       cli      fg 22934  nearest non-shell 22934  bind
claude SessionStart       cli      fg 59503  nearest non-shell 59503  bind
codex  namedrop,bash,bash,codex  nearest non-shell codex
codex  namedrop,bash,codex       nearest non-shell codex, twice
```

**A gone space after a restart** (`restart_probe.py`: SIGTERM to scratch server B, restart, connect as soon as the socket accepts):

```text
first connect 73 ms after restart began; sessions ['nd-s4v2-restart']
attach error None; process child login (fresh pid), foreground login
```

**What sends `process_changed`** (`tap9.py`, a tap attached to a scratch session while another client reads the block):

```text
baseline 1.5s        ['client_connected']
data.connect stream  ['client_connected', 'block_event:process_changed', 'client_disconnected']
rex capture          ['client_connected', 'client_disconnected']
format vt            ['client_connected', 'client_disconnected']
```

**The relay on the stand-in.** `relay_server.py` connected to B's Unix socket, set the label `namedrop-relay`, and declared `namedrop.relay` with an argument schema through `client.keymap`. It answered each `client_perform_requested` with whether the message `id` was new. Host A's `rex` ran with a scratch `HOME` and `REX_HOME=unix://<A>`.

```text
$ rex hosts add standin-b 127.0.0.1:47298
Added standin-b to a.s.
(the CLI wrote its cache to <scratch HOME>/Library/Application Support/rex/homes/endpoint-…/hosts/everywhere.json)
$ rex -S standin-b api call target.resolve '{"queries":["namedrop-relay"],"kinds":["client"]}'
{"results":[{"query":"namedrop-relay","status":"resolved","target":{"kind":"client","client_id":"client:01a1d2bb-…","label":"namedrop-relay"}}]}
$ rex -S standin-b api call client.perform '{"client_id":…,"action":"namedrop.relay","args":{"id":"msg-1","to":"@bob","text":"hello"}}'
{"result":{"accepted":true,"duplicate":false,"principal":{"kind":"anonymous","transport":"tcp"}},"performed_by":"client:…"}   45.9 ms
{"result":{"accepted":false,"duplicate":true,…}}   41.0 ms, then 40.2 ms
args without id and text  -> missing required properties "id", "text" (status=400 code=bad_request)
action namedrop.other     -> "namedrop-relay" does not offer "namedrop.other" (status=400 code=not_supported)
slow reply, timeout 1000  -> "namedrop-relay" did not answer within 1s (status=504 code=timeout)
retry of that id later    -> {"accepted":false,"duplicate":true}   (the timed-out request had still run)
over B's Unix socket      -> principal {"kind":"peer","transport":"unix","display":"uid 501","uid":501}
client crashed mid-call   -> "namedrop-relay" disconnected while the request waited (status=409 code=client_unavailable)
no such client            -> client client:… not found (status=404 code=not_found)
```

### Revision 3

Scratch servers A and B ran as in revision 2, with B on `--extra-listen 127.0.0.1:47331 --allow-anonymous`. A's `rex` ran with a scratch `HOME` and `REX_HOME=unix://<A>`. A's and B's `host_id` differed (`6606bd95-…` and `0b0d3018-…`), as two state directories do.

**Host labels and `-S`:**

```text
$ rex hosts add 'bad.label' 127.0.0.1:47331      Added bad.label to a.s.   (also 'a:b', 'UpperCase', 'with space', 'unix:x')
$ rex hosts add <65 characters> 127.0.0.1:47331  Error: host record label: is longer than 64 characters
$ rex hosts list --json
{"home":{"label":"a.s","endpoint":"unix://<A>","this_machine":false,"reached":true},
 "hosts":[{"host":{"id":"host:01a1d337-…","label":"standin-b","endpoints":["127.0.0.1:47331"],"added_from":"<machine>","updated_at":"…"},"from":"home","held":false}], …}
(no host_id in the entry)
$ rex -S standin-b --autostart=false api call server.status    pid 45597 host_id 0b0d3018-…   36 ms
$ rex -S STANDIN-B --autostart=false api call server.status    the same server
$ rex -S host:01a1d337-… --autostart=false api call server.status
Error: parsing server endpoint "host:01a1d337-…": parse "https://host:01a1d337-…": invalid port …
$ rex -S nosuchlabel --autostart=false --timeout 2s api call server.status
Error: connecting to Rex server: probing https://nosuchlabel: … lookup nosuchlabel: no such host
probing https://nosuchlabel:7297: … no such host
probing http://nosuchlabel:7297: … no such host
$ rex hosts add localhost 127.0.0.1:47331; rex -S localhost --autostart=false api call server.status
{"pid":45597,…}   (server B: the label took the place of the address)
```

**The pull on the stand-in.** `relay_b.py` on B's Unix socket declared `namedrop.relay` with `op` `deliver` or `collect`, kept per-recipient keys and an outbox per sender `host_id`, and wrote a reply into the outbox for each new item. A ran `rex -S standin-b api call client.perform -` with the request on stdin. B listed no host for A.

```text
deliver m1 to bob and carol   results accepted, accepted; can_reply true; host_id 0b0d3018-…; outbox [seq 1 reply from bob, seq 2 reply from carol]
deliver m1 to bob again       results duplicate; outbox unchanged
collect ack_through 1         outbox [seq 2]
collect ack_through 2         outbox []
a result of 900,000 bytes     returned through the CLI
a result of 1,100,000 bytes   Error: "namedrop" disconnected while the request waited (status=409 code=client_unavailable); the stand-in's socket broke (BrokenPipeError)
a request of 65,416 bytes     accepted
a request of 70,216 bytes     Error: control message too large: encoded request is 70269 bytes, limit is 65536   (the rex CLI, before it sent anything)
two clients labelled namedrop target.resolve status "ambiguous", two candidates
```

**Process groups.** `hookprobe3.py` ran as each agent's SessionStart and UserPromptSubmit hook and logged its chain from `ps` (pid, parent, process group, controlling terminal's group) with each executable from `proc_pidpath`. Claude Code got its hooks through `--settings <file>`. Codex got them from a scratch `CODEX_HOME` with `auth.json` linked to the account's and the probe in `hooks.json`, trusted once through Codex's hook review. Each line reads pid (group) executable, from the hook up.

```text
Claude Code 2.1.323, block child and foreground claude 26048 (26048)
  hook  33279 (33279) Python <- 26048 (26048, tty group 26048) 2.1.323 <- rex
  the tool shell: 45976 ppid 26048 pgid 45976 tpgid 0 zsh
  nested claude -p, SessionStart and UserPromptSubmit:
  hook  46166 (46166) Python <- 46007 (45976) 2.1.323 <- 45976 (45976) zsh <- 26048 (26048) 2.1.323 <- rex
Codex 0.142.2 through ~/.hermes/node/bin/codex, block child and foreground node 82223 (82223), program_status {"records":[]}
  hook  96092 (82223) Python <- 82278 (82223) codex <- 82223 (82223) node <- rex
Codex 0.163.0 --no-daemon, block child and foreground codex 50380 (50380), record owner 50380 app codex
  hook  54133 (54133) Python <- 50380 (50380) codex <- rex
  tool  54849 (54849) Python <- 50380 (50380) codex <- rex
Codex 0.163.0 default mode, first block, foreground codex 80440 (80440)
  app server 81243 (81243, tty group 0), parent 80440, environment REX_BLOCK=<first block>
  hook  91415 (91415) Python <- 81243 (81243) codex <- 80440 (80440) codex <- rex
  tool  92655 (92655) Python <- 81243 (81243) codex <- 80440 (80440) codex <- rex
Codex 0.163.0 default mode, second block, foreground codex 99989
  hook  5236 (5236) Python <- 81243 (81243) codex <- 80440 (80440) codex <- rex; the hook's REX_BLOCK names the first block
Codex 0.163.0 started with -c model_reasoning_effort="low":
  "Running without the shared background server: command-line configuration overrides (-c, --enable, --disable, or --search) requires embedded mode."
```

`codex app-server daemon stop` with the scratch `CODEX_HOME` stopped the app server afterwards.

**The sandbox.** `sbxprobe.py` ran as a Codex 0.163.0 tool under its default sandbox, in the `--no-daemon` session above. The same session's hooks connected (`ok peer 45533`).

```text
rex socket: errno 1 Operation not permitted
ancestors: (54849 Python pgid 54849 start 1794545668.725997), (50380 codex pgid 50380 start 1794545644.691360), (45533 rex pgid 45416 start 1794545005.766954)
```

**A one-line paste** (`paste1.sh`: `block.event` paste of `echo PASTED-ONE > /tmp/nd3-one\necho PASTED-TWO\necho PASTED-THREE` with each newline as a space, and no Enter). `/tmp/nd3-one` never appeared.

```text
bash 3.2   bash-3.2$ echo PASTED-ONE > /tmp/nd3-one echo PASTED-TWO echo PASTED-THREE
sh         sh-3.2$ echo PASTED-ONE > /tmp/nd3-one echo PASTED-TWO echo PASTED-THREE
zsh -f     % echo PASTED-ONE > /tmp/nd3-one echo PASTED-TWO echo PASTED-THREE
dash       $ echo PASTED-ONE > /tmp/nd3-one echo PASTED-TWO echo PASTED-THREE
```

**The pointer through the prompt hooks.** A pointer-shaped prompt with three lines (opening, body rule, and a JSON-quoted body with doubled spaces and `$(echo not-run)`) was pasted with each newline as a space, then Enter.

```text
Claude Code (rex send)          hook prompt == pasted line: True; strip_matching's rule matches the three-line prompt: True; newlines in the hook's prompt: 0
Codex 0.163 (block.event paste) hook prompt == pasted line: True; strip_matching's rule matches the three-line prompt: True; newlines in the hook's prompt: 0
```

**A shell wrapper on Linux** (Docker, a `#!/bin/sh` script named `session-start-wrapper`):

```text
rust:1.97.0 (Debian)   wrapper comm=session-start-w exe=dash      boot_id=c33e6b97-… start_ticks=933178
alpine:latest          wrapper comm=session-start-w exe=busybox   boot_id=c33e6b97-… start_ticks=933219
```

**A full listen queue** (a Unix listener with a backlog of one that never accepts, then non-blocking connects):

```text
live listener, backlog 1, never accepting: [ok 0.005 ms, ECONNREFUSED 0.006, ECONNREFUSED 0.005, ECONNREFUSED 0.003, ECONNREFUSED 0.005, ECONNREFUSED 0.005]
no socket file: ENOENT
```

### Revision 4

One scratch server ran as in revision 2, with the real `HOME`. It wrote `servers/<sha256 of unix://<socket>>/start.lock` into the user's Rex directory. That entry was removed after the server stopped, and the recipe below now gives each scratch server its own `HOME`. `rule4.py` implemented the revision 4 rules over real local process reads (`PROC_PIDT_SHORTBSDINFO`, `PROC_PIDTBSDINFO`, and `proc_pidpath`). `hookprobe4.py` ran as Claude Code's SessionStart and UserPromptSubmit hook through `--settings`, with `--runtime claude-code`, and wrote a prototype binding. `gate4.py` ran the gate by hand on one control connection. Each chain line reads pid, process group, and executable, from the hook up.

**A launcher that outlives its agent** (review 3's finding 1). A `zsh -f` block ran `WRAP_LINGER=40 bash wrap.sh`, which runs `claude --model haiku --settings …`, then `sleep`.

```text
SessionStart       chain [hook (own group), claude 82429 (82428), bash 82428 (82428), zsh 75489 (75489), rex]
                   foreground bash 82428, record none yet -> refuse (record checked only once it exists; rule relaxed to that)
UserPromptSubmit   record {state: working, app: claude-code, owner: bash 82428}
                   -> bind {foreground: 82428@1794550330414769, agent: 82429@1794550330417141, runtime: claude-code}
gate prompt        facts record {done, claude-code, owner bash 82428}, foreground bash 82428
                   pane_agent Known, Reported(86a18c52-…) -> admits; re-read: same foreground, bound agent alive -> Enter
                   +46.5 ms seq 2 working, CONFIRMED working; the screen showed "pong"
kill -9 82429      record {done, claude-code, owner bash 82428}, foreground bash 82428, block child zsh 75489
                   pane_agent: free terminal, the bound agent 82429@… is gone -> declines, nothing typed
                   zsh's group 75489 is not the foreground group 82428, so no terminal line either
```

The race ran in a second wrapper block (`WRAP_LINGER=20`). The facts admitted, then `kill -9` stopped the bound agent, then the gate pasted a line holding `touch …/ran-race; echo "$(touch …/ran-race)"`.

```text
pane_agent Known, Reported(284b4722-…) -> admits
kill -9 22121
paste err None | re-read: the bound agent 22121@1794550462331328 is gone | no Enter, held
28 s later:   WRAP: done
              Erics-MacBook-Pro% touch /tmp/…/ran-race; echo "$(touch /tmp/…/ran-race)"     (at zsh's prompt, unrun)
              ls ran-race: No such file or directory; foreground zsh 21811
```

**Nested agents started by an MCP server** (review 3's finding 2). Each block ran `claude --model haiku --settings … --mcp-config <file>`. The stand-in MCP server answered `initialize` and `tools/list`, and started `claude -p 'Reply with exactly the word nested.'` with a default spawn. Each nested agent answered `nested`.

```text
Python MCP server
  outer SessionStart  cli      chain [hook, claude 46300 (46300), rex]                                -> bind
  nested SessionStart sdk-cli  chain [hook, claude 46638 (46300), Python 46626 (46300), claude 46300] -> refuse: non-shell Python between
  nested UserPromptSubmit      the same chain                                                         -> refuse: non-shell Python between
Python MCP server running sh -c 'claude -p …; true'
  outer SessionStart  cli      chain [hook, claude 46326 (46326), rex]                                -> bind
  nested SessionStart sdk-cli  chain [hook, claude 46719 (46326), bash 46717 (46326), Python 46709 (46326), claude 46326]
                                                                                                       -> refuse: non-shell Python between
MCP server that is itself /bin/sh -c "claude -p …; python3 fakemcp.py none"
  outer SessionStart  cli      chain [hook, claude 59404 (59404), rex]                                -> bind
  nested SessionStart sdk-cli  chain [hook, claude 59612 (59404), bash 59609 (59404), claude 59404]
                               record {idle, claude-code, owner 59404}                                -> refuse: the foreground runs the agent's own executable (2.1.323)
  nested UserPromptSubmit      chain [hook, claude 59612 (59404)], its shell parent gone              -> refuse: the foreground is not an ancestor
bindings after all runs: each block's file names its outer session (d798a1e4, f5848adb, 42c2d23c)
```

Every scratch session was killed and the scratch server stopped. The nested agents ended on their own.

### Review 4's measures

Review 4 ran its own probes, and this patch did not re-run them. The last item was counted again for this patch with `git grep` on `origin/experimental`. As review 4 reported them:

- A zombie agent. Claude Code behind a Python launcher that does not reap its children, then `kill -9` of Claude Code. `ps` showed the agent as `Z+`, Rex kept `{idle, claude-code, owner Python}`, and a process read that returns zombies admitted the prompt. Linux's `/proc/<pid>/stat` keeps a zombie's group and start, and macOS's `proc_pidinfo` returns a zombie with `arg` 1 and fails with `arg` 0.
- Version skew. An MCP server `sh -c "<versions/2.1.322> -p …"` under Claude Code 2.1.323, with four Claude Code versions installed. The nested hook passed revision 4's conditions 1 to 4. Nested hooks saw `CLAUDE_CODE_ENTRYPOINT=sdk-cli` and the outer agent's `cli`.
- A non-interactive shell. A block made with `-- bash -c 'sleep 120; true'` reported child and foreground both `bash`, with `sleep` in bash's process group.
- The record at SessionStart. A root record existed at SessionStart in 5 of 6 runs.
- A dialog under `done`. Claude Code reported `done` while a modal dialog showed whose Enter confirms its option 1.
- Codex 0.163.0 has no `mcp-server` command.
- The test suite. Integration tests spawn the binary at 81 sites in 29 files, and no test file names `REX_`. Six of the files call `env_clear` for some of their spawns. The rest of the spawns keep the parent's environment, most with a fake `HERDR_SOCKET_PATH`.

### Reproduce the probes

Run these from a Rex terminal. The server's own `curl --unix-socket "$HOME/Library/Application Support/rex/server.sock" http://localhost/llms.txt` documents every message. Keep scratch socket paths under 104 bytes, and Herdr's shorter still. Give every scratch server its own `HOME`. Under the real `HOME`, a scratch server writes `servers/<sha256 of its listen URL>/start.lock` into the user's Rex directory. An agent in a scratch server's block then needs its login passed explicitly, as `CLAUDE_CONFIG_DIR` or `CODEX_HOME`. Claude Code on macOS also keeps its login in the login keychain, so link `Library/Keychains` into the scratch `HOME`, as the testbed's `auth_links` does (`testbed/lib/testbed.py:318-323`).

```sh
# A scratch server whose blocks know it.
X=$(mktemp -d /tmp/ndrex.XXXX); mkdir -p "$X/cfg" "$X/home"; : > "$X/cfg/init.lua"
[ "$(uname)" = Darwin ] && mkdir -p "$X/home/Library" && ln -s "$HOME/Library/Keychains" "$X/home/Library/Keychains"
env -i HOME="$X/home" USER="$USER" SHELL=/bin/zsh PATH=/usr/bin:/bin REX_SERVER="unix://$X/s.sock" \
	CLAUDE_CONFIG_DIR="${CLAUDE_CONFIG_DIR:-$HOME/.claude}" \
	"$(command -v rex)" server run --listen "unix://$X/s.sock" --state-dir "$X/state" --config "$X/cfg/init.lua" &
created=$(rex -S "unix://$X/s.sock" --autostart=false new nd-probe -c "$PWD" --json -- claude --model haiku)
S=$(printf '%s' "$created" | jq -er .session_id)
B=$(printf '%s' "$created" | jq -er '.initial_windows[0].block_ids[0]')
rex -S "unix://$X/s.sock" --autostart=false block call -s "$S" -b "$B" process | jq -c '{child, foreground, ancestors}'
rex -S "unix://$X/s.sock" --autostart=false kill "$S"; kill %1

# A Codex with hooks of its own: a scratch CODEX_HOME with the account's login linked in.
mkdir -p "$X/codex"; ln -s "$CODEX_HOME/auth.json" "$X/codex/auth.json"
printf 'model = "<model>"\ncli_auth_credentials_store = "file"\n' > "$X/codex/config.toml"   # no -c, which forces embedded mode
# Put the probe hook in "$X/codex/hooks.json", start `env CODEX_HOME="$X/codex" codex` in a scratch block,
# answer the folder trust with `rex send-key enter` and the hook review with `rex send-key down` then `enter`,
# and stop its app server afterwards with `env CODEX_HOME="$X/codex" codex app-server daemon stop`.

# An isolated Herdr for a nested probe. Check the socket before starting anything.
export HOME="$X/home" XDG_CONFIG_HOME="$X/config" XDG_STATE_HOME="$X/state-h" HERDR_SOCKET_PATH="$X/h.s" HERDR_CONFIG_PATH="$X/config/herdr/config.toml"
herdr status server   # must print "socket: $X/h.s" and "not running"
```
