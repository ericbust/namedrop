# Rex adapter and bridge design

This document is the design for steps 4 and 5 of the [Rex host plan](PLAN.md), and for the cross-host relay that follows them. Step 4 adds a Rex adapter behind the `Mux` port that [PORT.md](PORT.md) built in step 2. Step 5 adds the bridge, one long-lived process per Rex server that turns Rex events into Namedrop's lifecycle work. The relay carries a mention from a Namedrop on one machine to a Namedrop on another. Nothing here is implemented yet.

The design is for the agent who implements these steps, and for the person who reviews them. An agent in a Rex block gets the same promise an agent in a Herdr pane has today. Namedrop types into it only when the agent Namedrop verified is the live program and reports `idle` or `done`, and it never sends a prompt twice. Herdr behaves exactly as it does today, including Herdr running inside a Rex block.

This is revision 2. Revision 1 (`31c9c34`) went to an adversarial review, which asked for a revision first. [Review 1 and how it was answered](#review-1-and-how-it-was-answered) maps each finding to its change. Since that review, two decisions changed the scope. Remote Rex hosts are in the first release, and a Namedrop types only into blocks on its own machine.

The evidence comes from live probes on 2026-11-12 with Rex `0.1.0-preview.14` (API revision 12), Herdr 0.9.5, Claude Code 2.1.323 on Haiku, and Codex CLI on its default model, all on macOS. Probes used scratch Rex sessions, scratch Rex servers, and an isolated Herdr server with its own socket, config, and home. Every scratch session was killed and every scratch server was stopped afterwards. [Evidence](#evidence) gives the commands and the trimmed output. Each claim says whether it was measured, inferred, or guessed.

The design branch is `exp/rex-adapter-design`, cut from `origin/exp/mux-delete-bridge` (`013c129`). Step 3 (`exp/neutral-names`) is not in this base. Step 4's implementation starts from step 3's final head.

## Review 1 and how it was answered

| # | Finding | Answer |
|---|---|---|
| 1 | A Herdr-spawned process with no pane picks Rex, so Herdr's reconcile runs against the outer Rex session | Fixed at the root. `mux::home` picks the multiplexer whose *server* is the nearest ancestor, proved by the peer pid of each claimed socket, with no request. Any doubt about Herdr picks Herdr. Measured on an isolated Herdr inside a scratch Rex block: a pane, a workspace action, a pane action, and the startup command all resolve Herdr. The probe also found that macOS `parent_pid` stops at the root-owned `login`, so no walk could reach a Rex server. It moves to `PROC_PIDT_SHORTBSDINFO`. The testbed gets a Herdr-in-Rex lane |
| 2 | `bind_session` accepts a nested agent's hook | Fixed. A hook binds only when its nearest ancestor that is not a shell is the block's foreground program. On the review's captured chains, the outer hooks bind, both nested `claude -p` hooks are refused, and Codex binds at 2 and 3 hops |
| 3 | Rex `vt` output breaks the Codex prompt-box read | Fixed in `RexMux::screen`, which hands the port the same shape Herdr does. The real parser on six captures misreads two raw and reads all six right once each `ESC[nC` is n spaces |
| 4 | The kind does not travel in the value | Fixed. Every endpoint value, from any flag spelling, variable, or envelope, goes through `Endpoint::parse`. `Locator::pass` writes the set that matches the kind. Rex lines and envelopes use the neutral names. The names now match step 3 |
| 5 | Daemons inherit a pane and a session | Fixed. Every daemon gets its scope explicitly and starts through one `daemon::command`, which clears `REX_BLOCK`, `REX_SESSION`, `REX_CLIENT`, and the agent session variables. Rex has no session-vouched path at all |
| 6 | `locate_caller` cannot find a non-default server | Answered. Namedrop finds a server the way the `rex` CLI does, `REX_SERVER` then the default socket, and proves it by ancestry. A server that does not export `REX_SERVER` to its blocks also misroutes Rex's own CLI, so Namedrop refuses and names the variable. The stand-in and the testbed export it, and blocks inherit it (measured) |
| 7 | `shell_pid = child.pid` is wrong | Fixed. The shell is the process just below `login` in `foreground` plus `ancestors`, as Rex documents. Measured in an idle and a busy block |
| 8 | Hooks write one state directory and the CLI reads another | Fixed. A Rex home's default state directory is the install's. The installer records the directory it bakes into the hook wrappers, and a Rex home reads that record before any default |
| 9 | A killed session keeps its names, and Rex errors retry for 10 minutes | Fixed. `session_destroyed` becomes `MuxEvent::SpaceClosed`. A `Resync` that finds the space gone retires it too. Rex markers classify failures by `MuxError`, not by Herdr's JSON codes. A restart probe saw the saved session at the first connection, so a gone space is not a restart artifact (one run) |
| 10 | Hooks that never reach `mux::home` | Fixed. All seven identity sites take the home: `src/main.rs:926`, `:525-529`, `:1028`, `:1060`, `src/stop_hook.rs:73` and `:236`, and `src/prompt_hook.rs:42` (step 3 line numbers) |
| 11 | Claims | Corrected. The race window is the agent's own submit latency. `ps -Eww` does show environments. A data connection sends `process_changed`, and `capture` and `format` do not (measured). Codex daemon mode fails closed |
| Shape | Transport, deferrals, fixtures, base, manual wakes, live entry points | `ws.rs` writes over `Read + Write`. The conformance suite and backward `blob_read` windowing are deferred. Commit 0 holds the fixtures. Step 4 starts from step 3. Step 4's live checks use `reconcile` as the manual wake, and an ignored live test is the entry point until the CLI can reach Rex |
| Remote | In scope | Typing stays on the block's machine. The relay rides Rex's own authenticated connection through `client.perform`, measured end to end on a stand-in |

## Problem

Rex runs agents in blocks and reports their status, but it differs from Herdr in seven ways that shape this design.

- **Rex reports no agent session** (measured). Namedrop's own SessionStart hook is the only witness to which session runs in a block. `SessionClaim::Unreported` has no producer yet, and `resolve_session` answers `None` for it.
- **A status record can outlive its program** (measured). After `kill -9` of Claude Code under a shell, Rex kept `{state: idle, owner: {pid: <dead pid>}}` while zsh held the terminal. A gate that trusts the record would paste a mention into zsh and press Enter.
- **The gate needs one connection with an order** (measured). The tap, the facts, the paste, and the Enter must share one `rex.control.v1` connection, whose `seq` numbers order events against responses. A `rex` CLI process per call costs about 20 ms, and a `rex events` tap carries no `seq`.
- **Rex has no plugin host** (measured). Lifecycle events reach Namedrop only through a process that holds the event stream open.
- **Each multiplexer passes the other's variables through** (measured). A Rex server passes an inherited `HERDR_PANE_ID` into its blocks. A Herdr server passes the outer `REX_BLOCK` into its panes and its plugin processes. On the measured machine the everyday Herdr runs inside a Rex block, so every Herdr process carries a Rex claim that verifies.
- **A Rex block's shell runs under a root-owned `login`** (measured). On macOS a process can read its own ancestry only up to `login` with the call Namedrop uses today, so the Rex server is invisible to it.
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
			let state = locator::state_directory(options.plugin_state.clone(), MuxKind::Rex)?;
			let mux = mux::connect(&state, &place.endpoint, Budget::ADAPTER_DEFAULT, Arc::default())?;
			action(&LifecycleRuntime {
				workspace_id: place.space.as_ref().map(SpaceId::as_str),
				current_pane_id: place.pane.as_ref().map(PaneId::as_str),
				session_socket_id: place.endpoint.key(), // "rex:/Users/me/Library/Application Support/rex/server.sock"
				mux: &mux,
				..common_runtime_parts(&state)
			})
		}
	}
}
```

Every hook that today reads its location from the environment takes it from the home instead.

```rust
// stop_hook.rs, prompt_hook.rs, and main.rs's subagent and mailbox-wake hooks
let Ok(Some(here)) = mux::home::resolve(&Flags::none(), &mux::home::ThisProcess).map(Home::location) else {
	return; // silent, as every hook path is
};
// here.endpoint, here.space, here.pane replace Locator::Endpoint.env(), ::Space.env(), ::Pane.env()
```

The context hook binds the session before it builds the runtime, so the runtime's first survey already reports it.

```rust
// main.rs run_context_hook, for SessionStart and UserPromptSubmit
let home = mux::home::resolve(&Flags::none(), &mux::home::ThisProcess)?;
if let (Home::Rex(place), Some(session)) = (&home, hook_session_id(&input)) {
	let _ = place.bind_session(state, &session); // refuses unless this hook's agent is the block's foreground
}
```

The bridge translates and enqueues. It never calls lifecycle or drain code. The existing status worker does that, as it does for Herdr.

```rust
// rex_bridge.rs, for each coalesced change
status_queue::enqueue(state, &endpoint, &space, &Marker::Event(event))?;
status_queue::spawn_worker(state, &endpoint, &space)?; // daemon::command with the Rex set of names

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
	.bound("block:b", "41@1700000", "session-1");
	assert_eq!(rex.send(&PaneId::new("block:b"), prompt("hello", &claude("session-1"))), InputOutcome::Delivered);
	assert_eq!(calls(&rex), ["server.status", "target.resolve block:b", "session.attach session:s",
		"program_status block:b", "process block:b", "block.event paste hello", "process block:b", "block.event key enter"]);
}
```

## Shape

### Module map

| Path | Owns |
|---|---|
| `src/mux/home.rs` | `Home`, `Place`, `Location`, and `resolve`. Which multiplexer this process runs in, from flags, the claimed sockets, and ancestry. `Place::bind_session` |
| `src/mux/rex.rs` | `RexMux`, `connect`, the `MuxRead`, `MuxAct`, and `MuxInput` impls, and the effect and key mapping |
| `src/mux/rex/ws.rs` | A client-side WebSocket over any `Read + Write`: the HTTP upgrade, masked frames, 7-, 16-, and 64-bit lengths, fragments, ping and pong, and close. No compression and no new crate |
| `src/mux/rex/control.rs` | The private `Wire` and `Control` traits and their production impl. Hello and welcome, request ids, the event buffer, the per-operation budget and byte count, cancellation |
| `src/mux/rex/local.rs` | `prove_local`: the peer pid of a socket, `server.status`, and the canonical endpoint. The only code that decides whether a server is on this machine |
| `src/mux/rex/wire.rs` | Rex JSON parsed into domain types, Rex error codes mapped to `MuxError`, method names, and `port_ansi` |
| `src/mux/rex/agent.rs` | `pane_agent`, the pure liveness rule that the survey and the gate both call, and `shell_of`, the shell from a `process` reply |
| `src/mux/rex/channel.rs` | `RexChannel` and `RexTap`, the `gate::RawChannel` and `gate::StatusTap` impls |
| `src/mux/rex/binding.rs` | Session bindings under `<state>/rex/sessions/`, the incarnation read, and `hook_agent` |
| `src/mux/rex/events.rs` | `RexEvents`, the bridge's stream, and `classify`, the pure decode table from a Rex event to a `MuxEvent` |
| `src/mux/rex/fixtures/` | Captured `vt` screens, `process` replies, hook chains, event lines, and the home table. Commit 0 |
| `src/mux/rex/tests.rs` | Scripted-`Wire` tests that pin the requests for every port call |
| `src/mux/rex/live.rs` | `#[ignore]` tests that drive `RexMux` against the scratch server named by `NAMEDROP_REX_LIVE` |
| `src/rex_bridge.rs` | The bridge loop, its lock, coalescing, `ensure`, `run`, and the relay's receiving end |
| `src/relay.rs` | The relay's sending end: the outbox, `rex -S` calls, and outcome mapping |
| `src/daemon.rs` | `daemon::command`, the one constructor for every background process Namedrop starts |

Changes outside these files:

| Path | Change |
|---|---|
| `src/mux/model.rs` | `Endpoint::parse`, `Endpoint::unix_socket`, and `REX_SCHEME`. `Budget::tighter` and `narrowest`, moved from `herdr::transport` |
| `src/mux.rs` | `connect` and `Connector` |
| `src/mux/gate.rs` | `TapChange`. A program change declines before the paste and is `Ambiguous` at once after it |
| `src/mux/input.rs` | `Admit` and `PromptRequest.admit` deleted |
| `src/mux/event.rs` | `MuxEvent::Resync(SpaceId)`, `MuxEvent::SpaceClosed(SpaceId)`, and serde for every variant |
| `src/lifecycle.rs` | `pub fn handle_mux_event`, its `EventError`, and the `Resync` and `SpaceClosed` handlers. Attribution names the endpoint's multiplexer |
| `src/status_queue.rs` | `Marker`, `enqueue`, and `spawn_worker`, moved out of `main.rs` |
| `src/terminal.rs` | `parent_pid` through `PROC_PIDT_SHORTBSDINFO` on macOS. `process_start(pid)` and `process_name(pid)` beside it |
| `src/locator.rs` (step 3) | `state_directory` takes the home's kind. `Locator::pass` takes the endpoint's kind. The install record |
| `src/install.rs` | Writes the install record with the directory it bakes into the hook wrappers. The Stop wrapper also sets `NAMEDROP_REX_TIMEOUT_MS=250` |
| `src/integration.rs`, `src/health.rs`, `src/terminal.rs` | `tool_environment`, `registration_command`, Doctor's repair line, and `runner_command` write the neutral names for a Rex endpoint. Herdr bytes do not change |
| `src/main.rs` | The runtime builder, the context hook, the subagent hook (`:525-529`), `session_socket_id` (`:1028`), and the mailbox wake (`:1060`) take the home. The `rex bridge` subcommand. The worker's `Marker` dispatch. Every spawn moves to `daemon::command` |
| `src/stop_hook.rs`, `src/prompt_hook.rs` | The endpoint (`stop_hook.rs:73`, `prompt_hook.rs:42`) and the space (`stop_hook.rs:236`) come from the home |
| `src/deferred_commands.rs`, `src/reminder/clock.rs`, `src/dashboard*` | `mux::connect` or `mux::Connector` in place of `herdr::connect` and `HerdrConnector`. The four liveness probes connect to `Endpoint::unix_socket`. Live peek refuses a Rex binding |
| `scripts/runtime.py` | `helper_kind` and `queue_wake` (`:332`, `:434`) find a status worker by either name set, with the same set rule as the locator |
| `testbed/` | The Herdr-in-Rex lane (`testbed test --inside-rex`) |

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

// src/mux.rs
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
/// 2. `dial` is the same file (device and inode) as one of `server.status.listeners[].addresses`.
/// 3. The primary listener is a Unix socket, and its address becomes the endpoint.
/// Anything else is "not a local Rex server", and Namedrop types nothing through it.
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
}

pub struct Place {
	pub endpoint: Endpoint, // canonical, from prove_local
	pub space: Option<SpaceId>,
	pub pane: Option<PaneId>,
	/// The block's child and foreground pids from the verifying `process` read.
	/// Present only when an inherited REX_BLOCK verified, so only then can a hook bind.
	caller: Option<Caller>,
}

/// Where a hook records its turn: endpoint, space, and pane, from either home.
pub struct Location { pub endpoint: Endpoint, pub space: Option<SpaceId>, pub pane: Option<PaneId> }

/// What this process inherited: its environment, its ancestors, and the peer pid
/// of a socket. `ThisProcess` reads the real ones, and tests script them.
pub trait Inherited { /* var, ancestors, peer_pid */ }

impl Home {
	/// Herdr: step 3's Herdr set, read as the hooks read it today. Rex: the place.
	/// `None` when the home names no endpoint.
	pub fn location(&self) -> Option<Location> { todo!() }
}

/// 1. An explicit endpoint value decides by Endpoint::parse. With a Rex endpoint,
///    --space and --pane are trusted as --herdr-pane is today.
/// 2. With REX_BLOCK and REX_SESSION set, the nearest server ancestor decides (below).
///    A Rex home reads its place from the verified claim and ignores every HERDR_*
///    and NAMEDROP_* location name.
/// 3. Otherwise step 3's set predicate runs: a HERDR_* location name means Herdr, and
///    else the neutral set, whose NAMEDROP_ENDPOINT value is parsed for its kind.
pub fn resolve(flags: &Flags, inherited: &dyn Inherited) -> Result<Home, String> { todo!() }
```

**The nearest server ancestor.** Each claim names a socket. Rex's is `REX_SERVER`, then Rex's default socket. Herdr's is `HERDR_SOCKET_PATH`, then Herdr's default, and Herdr is asked only when a `HERDR_*` location name is set. For each socket, Namedrop connects, reads the peer pid, and closes, with no request. The peer probe took 5 µs at the median and 53 µs at most over 50 tries, and neither server logged anything for it (measured). The ancestor walk then says how many parents up each server is.

| Rex server | Herdr server | Home |
|---|---|---|
| Not an ancestor | No `HERDR_*` name | An error that names `REX_BLOCK` and `REX_SERVER`. Never Herdr, so a Rex process never runs `herdr` |
| Not an ancestor | A `HERDR_*` name is set | Herdr, today's path |
| An ancestor at depth r | No `HERDR_*` name, no server at the socket (`ENOENT` or `ECONNREFUSED`), or a server that is not an ancestor | Rex |
| An ancestor at depth r | An ancestor at depth h | Rex when r < h, else Herdr |
| An ancestor | The probe could not tell (a timeout, a path too long, or any other error) | Herdr |

So any doubt about Herdr is resolved as Herdr, which keeps every Herdr process on today's path. The prototype took 0.07 to 0.10 ms per decision in Python, with no request sent (measured). Measured with an isolated Herdr server inside a scratch Rex block, and with Rex blocks that carried stale Herdr names:

| Process | Rex server depth | Herdr server | Home |
|---|---|---|---|
| A shell in a Herdr pane | 2 | depth 1 | Herdr |
| A Herdr workspace action | 1 | depth 0 | Herdr |
| A Herdr pane action | 1 | depth 0 | Herdr |
| A Herdr startup command | 1 | depth 0 | Herdr |
| A block of a Rex server started from a Herdr pane | 1 | live, not an ancestor | Rex |
| A Rex block whose `HERDR_*` names a live Herdr elsewhere | 1 | live, not an ancestor | Rex |
| A Rex block whose `HERDR_SOCKET_PATH` names no socket | 1 | `ENOENT` | Rex |
| A plain Rex block | 1 | no name | Rex |

**Verifying the block.** When the table says Rex, one connection runs `server.status` for the local proof, `session.attach`, and `process` for `REX_BLOCK`. The claim verifies when the block's `child` is an ancestor of this process. Otherwise the answer is an error naming the block, as `verify_inherited_pane` refuses an unverified Herdr pane today. That is one fresh connection with three requests, about 1 ms (inferred from the 0.8 ms measured for a connection with two).

**The ancestor walk crosses `login`.** In a default Rex block the chain is the Rex server, `/usr/bin/login` running as root, the login shell, and the program. `proc_pidinfo(PROC_PIDTBSDINFO)`, which `terminal::parent_pid` calls today, fails on the root-owned `login`, so the walk stops one step short of the server. `PROC_PIDT_SHORTBSDINFO` and `sysctl(KERN_PROC_PID)` both read it (measured). `parent_pid` moves to the short info, which gives the same parent for every process the old call could read. On Linux, `/proc/<pid>/stat` already reads root processes.

**Rex finds servers as `rex` does.** A block on a server started without `REX_SERVER` in its environment gets no `REX_SERVER`, and the `rex` CLI in that block talks to the default server (measured in the spike). Namedrop does the same lookup, so in that block it finds no ancestor server and refuses with "REX_BLOCK=… is on a Rex server this process cannot find; start that server with REX_SERVER set". A server started with `REX_SERVER` naming itself passes it to every block (measured).

**Kind-scoped names.** The home is chosen before any location name is read. A Rex home takes its place from the verified claim, or from the flags, or, for a daemon, from the neutral set Namedrop gave it. It never reads a `HERDR_*` location name, so a stale outer Herdr cannot redirect it. Nothing puts `NAMEDROP_*` names into a block's environment, so in a block the inherited identity comes only from `REX_*`.

**The state directory.** Step 3's outcome leaves the default to step 4, for a Rex home only.

```rust
// src/locator.rs (step 3), with the home's kind
pub fn state_directory(flag: Option<PathBuf>, home: MuxKind) -> Option<PathBuf> {
	// flag, then:
	// Herdr: HERDR_PLUGIN_STATE_DIR, else None, exactly as step 3.
	// Rex:   NAMEDROP_STATE_DIR, then the install record, then $XDG_STATE_HOME/namedrop
	//        (or $HOME/.local/state/namedrop).
	todo!()
}
```

The install record is `$XDG_CONFIG_HOME/namedrop/state-directory` (or `~/.config/namedrop/state-directory`), one line holding the same absolute path the installer bakes into the hook wrappers with `--state-directory`. `integration install` writes it beside the wrappers, through a temp file and a rename. So the hook that binds a session and the CLI that surveys the block open one directory, spelled the same way. Doctor reports a record that differs from the wrappers' directory. Rex status workers share that directory with Herdr's, so `scripts/runtime.py` must find them by their neutral names when it stops and wakes helpers on an update.

### Daemons start with no identity

The bridge, the status worker, the deferred worker, the reminder clock, the dashboard server, the mailbox wake, and the relay's `rex` calls all start through one constructor.

```rust
// src/daemon.rs
/// A background process with the scope it is given and no identity it inherited.
/// Clears REX_BLOCK, REX_SESSION, REX_SERVER, REX_CLIENT, CLAUDE_CODE_SESSION_ID,
/// CODEX_SESSION_ID, and both pane names. Writes `scope` with Locator::pass for its
/// endpoint's kind. Keeps REX_BIN_DIR, which finds `rex`. process_group(0), null stdio.
pub fn command(executable: &Path, scope: Scope<'_>) -> Command { todo!() }
```

A boundary test fails when `process_group(0)` appears outside `daemon.rs`, so a new spawn cannot skip the scrub. With the scrub, a daemon has no `REX_BLOCK` to claim, and Rex has no session-vouched path for it to fall into.

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
	/// Parents, names, groups, and start times of local processes. Scripted in tests.
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
}
pub(super) struct PaneAgent { pub agent: Option<AgentIdentity>, pub state: Option<AgentState>, pub reports_session: bool }

/// A record speaks for the block only while its owner is the live foreground
/// process, pid and name both. A session is reported only while the binding's
/// incarnation is the foreground's, so a replaced program loses it.
pub(super) fn pane_agent(observed: Observed<'_>) -> PaneAgent { todo!() }

/// The block's shell. `child` is the shell, except on macOS where a login shell
/// runs under `login` and `child` describes login. Then the shell is the process
/// just below `child` in `[foreground] ++ ancestors`. `None` when Rex leaves
/// `ancestors` out (more than 16, or one exited), which makes the shell unavailable
/// for that read only.
pub(super) fn shell_of(process: &ProcessReply) -> Option<u32> { todo!() }
```

| Record owner is the live foreground, and `app` is `claude-code` or `codex` | Binding incarnation equals the foreground's | `agent` | `state` |
|---|---|---|---|
| yes | yes | `Known(kind)`, `Reported(session)` | the record's state |
| yes | no binding, or another incarnation | `Known(kind)`, `Unreported { incarnation }` | the record's state |
| yes | start time unreadable, or the block is not local | `Known(kind)`, `SessionClaim::None` | the record's state |
| no record yet | yes | `AgentKind::Unreported`, `Reported(session)` | `None` |
| no, the record is stale, or `app` is another program | any other case | `None`, a free terminal | `None` |

`app` must be `claude-code` or `codex` (both measured). Any other program that reports OSC 7501, such as `cargo`, leaves the block a free terminal. The state goes through `AgentState::parse`, so `error` and unknown words become `Unknown` and are never admitted. `PaneFacts.reports_session` is `true` when the pane reports a bound session or no agent at all, and `false` when an agent runs whose session is unknown.

A block whose `host_id` differs from its server's runs on another machine, so its pids name nothing here. It is never typed into, by the gate or as a terminal. Its server's own machine owns it, and the relay reaches it there.

**The process facts.** `PaneProcess` is `shell_pid = shell_of(reply)`, `foreground_pids` = the members of the foreground's process group, read locally, and `foreground_group` = the foreground's pgid, read locally, because Rex reports no pgid. In an idle block the reply was `child login 92189, foreground zsh 92197, ancestors [login]`, so the shell is 92197, whose group is 92197, and `shell_is_available` holds. With `sleep 30` running it was `foreground sleep 92786, ancestors [zsh 92197, login]`, so the shell is still 92197 and the group is 92786 (measured).

**The screen speaks the port's ANSI.** Herdr's ANSI rendering, which `delivery.rs` parses, is text with SGR styling. Rex's `vt` format also writes a run of n blank cells as `ESC[nC`, and `cells()` drops every CSI but SGR, so `codex_box` loses the space after `›`. `RexMux::screen` converts at the boundary.

```rust
// src/mux/rex/wire.rs
/// Rex's `vt` output in the port's ANSI shape: each ESC[nC becomes n spaces
/// (one when n is absent). SGR passes through. Six captures held no other CSI.
pub(super) fn port_ansi(vt: &str) -> String { todo!() }
```

The fix belongs in the adapter, not in `cells()`, because the port promises one shape and Herdr's path then stays untouched. With delivery's real parser, the raw captures read an idle Codex with an empty box as `Draft`, and a fresh Codex draft as `Unseen`. After `port_ansi`, all six captures read as their screens show (measured, [Evidence](#evidence)).

**The gate channel.** `deliver` for a prompt opens one connection and runs `gate::emulate_prompt` over it.

```rust
// src/mux/rex/channel.rs
impl gate::RawChannel for RexChannel<'_> {
	type Tap = RexTap;
	/// session.attach for the block's session. From here the session's events buffer on the connection.
	fn tap(&self, pane: &PaneId) -> Result<RexTap, ChannelError> { todo!() }
	/// program_status, then process, then the local start time and the binding file. `pane_agent` answers.
	fn facts(&self, pane: &PaneId) -> Result<(Option<AgentIdentity>, Option<AgentState>), ChannelError> { todo!() }
	/// block.event paste, its ack, then process again: a foreground pid other than the facts'
	/// pid stops here with an error, so the gate holds the prompt and Enter is never pressed.
	/// Then block.event key Enter and its ack. No sleep.
	fn paste_submit(&self, pane: &PaneId, text: &str) -> Result<(), ChannelError> { todo!() }
}

impl gate::StatusTap for RexTap {
	fn next_change(&mut self, within: Duration) -> Option<TapChange> {
		// Skip events with seq <= the facts' seq, because the facts include them, and events for other blocks.
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

The re-read after the paste closes most of the window in which an agent exits between the facts and the Enter. The paste then sits in the shell's line unsubmitted, and the attempt is held. It costs one 0.1 ms request.

`deliver` for `Line` and `Keys` sends `block.event` paste and key events with no confirmation. A Rex error on the first request is `Refused`, because nothing was typed. A fault after the first request was written is `Ambiguous`. `Recipient::Agent` and `Recipient::Pane` type the same way on Rex.

**The binding.** One JSON file per block, written only by `bind_session` and deleted only by `RexEvents` when it decodes `block_closed`. Block IDs are never reused and survive a server restart (measured), so the block ID alone is the key.

```rust
// src/mux/rex/binding.rs
/// `<pid>@<start>`. Start is microseconds since the epoch on macOS
/// (pbi_start_tvsec and pbi_start_tvusec) and clock ticks since boot on Linux
/// (/proc/<pid>/stat field 22). Only equality matters, and the hook and the
/// survey read it on the same machine.
pub(super) fn incarnation(local: &dyn LocalProcesses, pid: u32) -> Option<Incarnation> { todo!() }

/// The agent that ran this hook: the nearest ancestor that is not a shell (sh,
/// bash, zsh, dash, ksh, or fish, with or without a login dash). An agent's hook
/// runner and Namedrop's wrapper put only shells between the agent and the hook.
/// A nested agent, such as `claude -p` run as a tool, is itself the nearest
/// non-shell ancestor of its own hooks, so they never reach the outer agent.
pub(super) fn hook_agent(local: &dyn LocalProcesses) -> Option<u32> { todo!() }

impl Place {
	/// Records that `session` runs in the caller's block, bound to the incarnation
	/// of the block's foreground program. Refuses unless hook_agent() is the
	/// block's foreground pid and the root record's owner, and when the start time
	/// cannot be read. Writes only a change, so UserPromptSubmit can call it on every prompt.
	pub fn bind_session(&self, state: &Path, session: &AgentSessionId) -> Result<(), String> { todo!() }
}

#[derive(Serialize, Deserialize, Eq, PartialEq)]
pub(super) struct Binding {
	pub block: String,       // "block:<uuid>"
	pub incarnation: String, // "93037@1794528253006243"
	pub session: String,     // the hook's stdin session_id
	pub bound_at_ms: u64,
}
```

On the review's captured chains, the rule binds every outer Claude Code hook (Python, then `claude`, the foreground), refuses both hooks of a nested `claude -p` (whose nearest non-shell ancestor is the nested `claude`, three hops below the foreground), and binds Codex's Namedrop hook through one or two `bash` processes (a script ran the rule over those captures, [Evidence](#evidence)). The old rule, "the foreground is an ancestor", accepted all seven Claude Code hooks.

A wrapper that forks the agent instead of `exec`-ing it puts a non-shell between them, so its hooks are refused and the agent cannot register. Rex already reports a wrapper's command as `foreground` (Rex's docs), so that refusal is narrow, and nothing is typed in error (inferred).

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
pub fn spawn_worker(state: &Path, endpoint: &Endpoint, space: &SpaceId) -> Result<(), String> { todo!() }       // daemon::command

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
		log server.status instance_id for the doctor
		from step R on, declare the relay: client.set_label "namedrop", client.keymap with namedrop.relay
		for each activated space of this endpoint in the store: pending.push(Resync(space))
		loop:
			flush when 50 ms pass with no event, or 250 ms after the first pending change:
				take runtime_command_guard, call runtime_reexec_if_replaced
				for each (space, marker) in pending: status_queue::enqueue
				for each space with a new marker: status_queue::spawn_worker
				release the gate
			match stream.next(time to the next flush):
				Some(Event(event)) => pending.note(classify(event)) // coalesced, see below
				Some(Perform(request)) => receive(request)          // step R, see the relay
				None => {}
				Ended => break, then reconnect. In-flight mail is untouched.
```

| Rex event (inside `session_event` unless noted) | Marker |
|---|---|
| `block_created` | `PaneAppeared` |
| `block_closed` | `PaneClosed`, after `RexEvents` deletes the block's binding file |
| `window_closed` | `TabClosed { space, tab: window_id }` |
| `session_destroyed` (top level) | `SpaceClosed`. `rex kill` sends `block_closed` for each block about 6 ms before it (review's measure) |
| `block_event program_status_changed`, root record | `AgentStatusChanged` |
| `block_event program_status_removed`, root record | `AgentStatusChanged` |
| `block_event process_changed` | `AgentStatusChanged`. The first block of a new session sends this and no `block_created` (measured) |
| `block_event child_exited` | `AgentStatusChanged`. `block_closed` follows when the program's exit closes the block (measured) |
| `session_created`, `title_changed`, `pwd_changed`, `bell`, `size_changed`, `client_*`, `session_view_changed`, anything unknown | Nothing |

`pending` holds at most one `AgentStatusChanged` per pane. A `Resync` absorbs the queued `AgentStatusChanged` markers for its space, and a `SpaceClosed` absorbs every marker for its space. `PaneAppeared`, `PaneClosed`, and `TabClosed` keep their order. So the queue is bounded by the number of blocks on the server. The bridge flushes only for spaces with an activated worktree, which a read-only store query lists.

A gone space is not a restart artifact. After SIGTERM and a restart of a scratch server, the first connection listed the saved session and attached to it, and the block answered `process` with a fresh `login` (measured). So `Resync` may retire a space that answers `Gone`.

`AgentDetected` is never produced in steps 4 and 5. On Herdr it differs from a status change only by opening the tour popup, and Rex popups arrive in step 6.

### Remote hosts: typing stays local, mentions relay

Each machine that runs agents runs its own Namedrop beside its own Rex server, because the agents' hooks run there and need Namedrop there. A Namedrop types into, locks, binds, and reads incarnations of blocks only on servers that `prove_local` accepts. So the gate, the incarnation, the binding, and the pane lock never cross a machine. A mention to an agent on another machine goes from one Namedrop to the other, and the receiving Namedrop delivers it locally.

How each element of the review's remote table comes out:

| Element | Behavior |
|---|---|
| Endpoint string | Only proven-local servers become endpoints, spelled as their primary Unix listener. A remote server is never an endpoint, a namespace, a lock, or a status queue. The relay names it by a `rex hosts` label |
| One server at several addresses | Every local spelling converges on the primary listener. A forwarded socket, an `ssh -L` socket, or TCP fails the local proof, because the peer is not the server's pid or the dialed file is not one of its listeners (measured with a forwarder) |
| Incarnation | Read only for blocks of a proven-local server whose `host_id` is the server's |
| `host_id` guard | Replaced by the peer proof for the server. `host_id` still marks a block in a local session that runs elsewhere, and such a block is never typed into |
| Hook binding | Written on the agent's machine into that machine's state directory, under the same `REX_SERVER` rule |
| Bridge and pane lock | A block's server is on one machine, and only that machine's Namedrop can prove it local. So two machines never type into one block, and no lock has to span machines |
| `-S` | Every `rex` call the relay makes passes `-S <label>` and runs through `daemon::command`, so no inherited `REX_SESSION`, `REX_BLOCK`, or `REX_CLIENT` can pick a target |
| Transport | Typing uses the local Unix socket. The relay shells out to `rex`, so tailnet TLS and Rex's admission stay Rex's code. About 40 ms per call on the stand-in, one `rex` process each (measured) |

**The relay rides Rex's own connection.** Rex lets a client declare actions and lets another client call them. `client.perform` sends the named client a `client_perform_requested` event that carries a `requester` principal the server fills in, and the server waits up to `timeout_ms` (at most 30 s) for `client.reply`. The server checks the action name and the arguments against the declared schema before the client sees anything. Requests to one client wait in line (all from Rex's docs). On the stand-in, a Namedrop-shaped client on host B declared `namedrop.relay`, and a `rex -S standin-b api call client.perform …` from host A reached it and returned its result in 40 to 46 ms (measured).

```rust
// src/rex_bridge.rs, on host B. The bridge is already a client of B's server.
// client.set_label "namedrop"; client.keymap {actions: [{name: "namedrop.relay", kind: "client",
//   args: {id, from: {name, host_id}, to, text}}], server_actions: "none"}
fn receive(request: PerformRequest) {
	// principal: peer with this uid, or tailscale; anonymous only with NAMEDROP_RELAY_ALLOW_ANONYMOUS=1
	// run `namedrop relay accept` through daemon::command, args on stdin, held to 5 s
	// client.reply with its result, or error {code: "refused", message} when it refused
}

// `namedrop relay accept`, on host B
// insert the inbound message keyed (from.host_id, id), unique
//   new       => {"accepted": true}, then schedule_mailbox_workers for the recipient's space
//   duplicate => {"duplicate": true}, nothing changes
//   no such name, space paused, principal refused => refused, with the reason

// src/relay.rs, on host A, run by the status worker for its outbox
fn send(outbox: &Outbound) -> RelayOutcome {
	// rex -S <label> --timeout 10s api call target.resolve {"queries":["namedrop"],"kinds":["client"]}
	// rex -S <label> --timeout 35s api call client.perform {client_id, action: "namedrop.relay", args, timeout_ms: 30000}
	todo!()
}
```

| Outcome of a send | A's outbox |
|---|---|
| `accepted` or `duplicate` | Done. B owns the message |
| `refused` | Bounced to the sender as failed, with B's reason |
| `timeout`, `client_unavailable`, `not_found` (no Namedrop client on B), no route, or a failed `rex` | Kept, retried with the same `id` on the worker's backoff |

Retries are safe because B deduplicates. A perform that timed out at A still ran on B, and the retry with the same `id` came back `duplicate` (measured). So the relay is at-least-once on the wire and exactly-once into B's mailbox. B's own delivery keeps the gate's at-most-once typing.

**Addresses.** A remote recipient is `@name@host`, where `host` is a label from the sender's `rex hosts`. Names stay local to their Namedrop. A request carries the sender's name and its server's `host_id`. B replies through the host in its own `rex hosts` whose recorded `host_id` matches (Rex keeps the `host_id` each host's server reported when it was added). When B has no such host, the reply waits at B and Doctor says which host to add.

**Who may relay.** A principal that B's server admits can already type into any of B's blocks with `block.event`, so the relay grants nothing the connection does not. It does let a message arrive under a sender's name, so B accepts `peer` principals with B's own uid and `tailscale` principals, which Rex admits only for the device owner unless the owner widened it (Rex's docs for `allow_tailnet_members` and `--tailnet-allow-identified`). B refuses `anonymous` principals, which a `--allow-anonymous` TCP listener admits, unless `NAMEDROP_RELAY_ALLOW_ANONYMOUS=1` is set for a stand-in. The delivered envelope names the sender's host and the principal's display name.

**Testing without a second machine.** The stand-in runs two scratch servers on one machine. Server A listens on a Unix socket. Server B listens on a Unix socket and on `--extra-listen 127.0.0.1:<port> --allow-anonymous`. Each exports `REX_SERVER` naming itself, and each Namedrop has its own state directory. Host labels come from a scratch home. A-side `rex` runs with `REX_HOME=unix://<A's socket>` and a scratch `HOME`, because the `rex` CLI caches its home's host list under `HOME` (measured), and `rex hosts add standin-b 127.0.0.1:<port>` then makes `-S standin-b` reach B (measured). A delay-and-reset forwarder between A's `rex` and B's port exercises timeouts and drops. A `LocalProcesses` double that answers nothing for B's pids, and a forwarded socket to B, show that A refuses to type into B even though B is on the same machine. TLS to the user's real server needs the user's approval and is not tested. Namedrop's TLS path is the `rex` CLI's, unchanged.

### Interface depth

The adapter's surface is `connect`, `Place`, `RexEvents`, and the relay's receive, all `pub(crate)`. Only `mux::connect`, `mux::home`, and `rex_bridge` call them, so the rest of the crate reaches Rex through the `Mux` traits and `Home`. Behind it sit a WebSocket client, the local proof, the `seq` ordering rule, the stale-record rule, incarnations, the binding files, the nearest-server rule, and the event decode table. No Rex wire type crosses that surface.

The core sees four additions. `handle_mux_event` is a public entry over the private `handle_event`, with a typed error. `Resync` and `SpaceClosed` add two handlers that reuse `reconcile_worktree` and the existing address removal. `Marker` names the status queue's four input shapes. `daemon::command` replaces each spawn's own environment handling. The session model in `model.rs` does not move, and `resolve_session`, `locate_agent_session_pane`, the popup target, and `candidate_reach` do not change.

## Synthesis decision

The design came from an arena of two candidates, each told to take a different whole shape. Candidate 1 (Opus) put the binding, the transport, and its own scheduler inside a Rex-only adapter and bridge. Candidate 2 (Sonnet) kept the adapter thin, moved the binding into the core through a new `SessionClaim::Bound` variant and a decorator, and made the bridge a translator into the existing status queue. A Sonnet cross-judge scored both (see [Cross-judge verdict](#cross-judge-verdict)).

Candidate 1 is the base. Its binding keeps the core's session model whole. The survey reports `Reported` exactly when Namedrop's own hook bound the live program, so no core reader of sessions learns that a session can come from two places.

From candidate 2 the design takes one connection per port call, the bridge as a translator into the existing per-space status queue, the rule that only `claude-code` and `codex` are agents, one module that chooses the multiplexer, flushing only for activated spaces, and declining a prompt whose budget cannot cover the confirm window.

Revision 2 drops candidate 2's session-vouched fallback for Rex. On Rex, a binding needs the hook's ancestry, so a process whose ancestry fails has no binding to vouch with, and the fallback only let a daemon pass as the block it started in (finding 5).

On restart detection the design keeps candidate 1's answer. It runs the same idempotent `Resync` on every connect and lets the incarnation prove which programs were replaced. `observe_agent_attempt` (`src/delivery.rs:351`) holds any in-flight attempt whose pane no longer reports its session, before it looks at the restart flag.

## Tradeoffs accepted

- We accept session bindings in per-block JSON files outside SQLite, in exchange for no change to `resolve_session`, `locate_agent_session_pane`, the popup target, or `candidate_reach`. A store reset does not clear them. That is harmless, because a binding without a registration does nothing.
- We accept that the Rex adapter holds a path to Namedrop's state directory, which the Herdr adapter never needed, in exchange for keeping incarnations and bindings out of the core.
- We accept that an agent can register only after its first SessionStart or UserPromptSubmit hook, and only when no non-shell process sits between it and its hooks, in exchange for a binding that only the agent's own hook can write.
- We accept two peer probes and an ancestor walk, about 0.1 ms in total, on every Namedrop process that inherits `REX_BLOCK`, which on the measured machine is every Herdr process. No request is sent. In exchange the nearest server is chosen correctly.
- We accept that a Rex server must export `REX_SERVER` for Namedrop to work in its blocks, as Rex's own CLI needs.
- We accept that `CONFIRM_WINDOW` holds the pane lock for up to 5 s on Rex, in exchange for a prompt that is either confirmed or held before the lock is released.
- We accept a hand-rolled WebSocket client of about 250 lines that skips the `Sec-WebSocket-Accept` check, in exchange for no new crate. It speaks only to a proven-local Unix socket, and the check needs SHA-1, which no current dependency provides.
- We accept that `error` and unknown states never admit a prompt, although the protocol says to treat an unknown state as `idle`.
- We accept that badges, notifications, popups, keybinding reloads, and live peek are unsupported on Rex until step 6 or later.
- We accept a residual window between the facts read and the agent's own `working` report. The record read `idle` 220 times in the 35 ms after an Enter ack (review's measure), because the window is the agent's submit-to-`working` latency, 17 to 403 ms here. A person who submits a prompt by hand inside it makes the agent report `working`, and that change could confirm a mention typed into a busy agent. Herdr has the same exposure. Closing it needs an atomic paste-if-idle from Rex.
- We accept two marker shapes in the status queue, and two failure classifiers, one per shape, so Herdr's worker keeps its exact behavior.
- We accept that a cross-host reply needs the recipient's machine to list the sender's machine in `rex hosts`. Until it does, the reply waits and Doctor says so.
- We accept a `rex` process per relay call, about 40 ms locally plus the network round trip, in exchange for Rex owning tailnet TLS and admission.

## Alternatives considered

- **The core owns the binding** (candidate 2's shape). A `SessionClaim::Bound` variant, made by a decorator around every Rex connection, resolves `Unreported` from a store. More of the system learns about incarnations. It loses on depth.
- **A per-call `rex` CLI transport.** It costs 19.5 to 26.9 ms per call (median), and a CLI tap has no `seq`, so the gate's order cannot be proven (measured). It cannot meet the gate contract.
- **A held control connection per `RexMux`.** It saves 0.7 ms per call. It costs a reader thread, a pending-request map, reconnect logic, a stale socket after a server restart, and a ping duty.
- **Choosing by the nearest *pane* anchor** (revision 1). A Herdr process with no pane has no Herdr anchor, so the Rex claim won (finding 1). A server is the one ancestor every process of a multiplexer has.
- **Recognizing servers by executable name.** The peer pid of the claimed socket proves the same thing without trusting a name.
- **Finding a server from its process's open sockets** (`proc_pidfdinfo` on macOS, `/proc/net/unix` on Linux). It would find a server that exports no `REX_SERVER`, at about 100 lines of platform code. Rex's own CLI misroutes in such a block, so the case is a Rex misconfiguration, and a clear refusal is enough.
- **Trying every endpoint already in the store** (the review's suggestion for finding 6). It opens the database on every hook in a Rex block, and it still fails on a fresh install.
- **A session-vouched fallback for Rex** (revision 1). See the synthesis decision.
- **Binding when the foreground is any ancestor** (revision 1). A nested agent's hooks pass (finding 2).
- **Normalizing `ESC[nC` in `delivery.rs`'s `cells()`.** It would change Herdr's parser for a Rex shape. The adapter owns the port's contract.
- **Retiring a gone space inside the shared `pane_closed`.** It would change Herdr's pane close. `SpaceClosed` is produced only by the Rex bridge.
- **Restart detection by a stored `instance_id`.** The incarnation already proves which programs were replaced.
- **`tungstenite`.** It brings an HTTP stack and a random-number crate for one fixed local protocol.
- **Storing the binding in Rex.** Rex has no per-block key-value store, and a block label is visible and editable by people.
- **Relay through an `init.lua` action that runs `rex.exec`.** It needs Namedrop's Lua in the remote config first, it shares the server's 30 s action budget, and a config reload fails it mid-call (Rex's docs).
- **Relay through `rex -S <host> run --wait -- namedrop …`.** Each message would create a visible block in a new window on the remote server.
- **Relay over Namedrop's own network protocol.** It adds a listener, authentication, and TLS that Rex already provides.
- **Typing into the remote block over `-S`.** The decided model forbids it, because the gate's facts, the incarnation, and the lock would cross machines.

## Answers to the questions

### 1. `emulate_prompt`'s order holds on Rex

Rex's `program_status_changed` events reach a subscriber in the order the gate needs, when the tap, the facts, the paste, and the Enter share one control connection. `gate_probe.py` ran that order against live agents.

- **Order.** The tap is `session.attach`. The facts are `block.method program_status` and `process`. The paste and the Enter are two `block.event` requests. Their acks came 0.2 to 0.7 ms apart (measured). Every facts response carried `seq 1`, and every `working` event carried `seq 2` or `3` (measured). So a `seq` above the facts' `seq` is the boundary the tap measures from. Revision 2 adds a `process` read between the paste ack and the Enter.
- **Latency.** Claude Code reported root `working` 17.3 to 140.7 ms after the Enter ack, median 21.6 ms, in 9 of 9 runs. Codex took 18.8 to 403.1 ms, median 25.2 ms, in 5 of 5 runs (measured). The latency is the agent's own. `CONFIRM_WINDOW` stays 5 s, about 12 times the slowest run.
- **Busy agent.** Codex reported `working` for 33 s on a story. A second gate run during that time declined at the facts, and the word it would have sent never appeared on Codex's screen (measured).
- **Blocked agent.** Claude Code at a permission prompt reported `{state: blocked, kind: permission}`. The gate declined at the facts (measured).
- **Tool calls.** While Claude Code and Codex ran `sleep` as tool calls, every 0.5 s sample showed `process.foreground.pid` equal to the root record's `owner.pid` (measured). The stale-record rule does not flap during a turn.
- **Stale record.** `kill -9` left a null foreground 0.34 ms later, and `process_changed` lagged about 110 ms (review's measure). The facts decline, and `pane_agent` treats the block as a free terminal.
- **Between turns.** Claude Code reported `done` 0.73 to 1.01 s after the Enter for each one-word reply (measured). It never stayed `working` after a turn.
- **Confirmation stays in the gate.** A prompt the tap cannot confirm must be held before the lock is released.

### 2. The incarnation is a pid and a local start time

Rex reports no start time. `process` returns `child`, `foreground`, and `ancestors` with `argv0`, `cwd`, `invoked_path`, `name`, `path`, `pid`, `script`, `user`, and `user_id`, and status records carry `owner {pid, name}` (measured). Claude Code's process name is its version, so a name does not identify a program across upgrades.

The incarnation is `<pid>@<start>`, for the block's `process.foreground` pid, read locally. On macOS, `proc_pidinfo(PROC_PIDTBSDINFO)` gives `pbi_start_tvsec` and `pbi_start_tvusec`, which were stable across reads (measured). On Linux the source is field 22 of `/proc/<pid>/stat` (inferred, not run).

Namedrop reads it only for a block of a proven-local server whose `host_id` is the server's. When the start cannot be read, there is no incarnation and no fallback. `bind_session` refuses to write, and the survey reports `SessionClaim::None`, so the agent is visible but cannot register, and Doctor can say why.

### 3. The session record is a per-block file the hook writes

A Claude Code SessionStart hook ran as a child of the `claude` process, which was the block's foreground and the record's owner. Its environment had `REX_BLOCK`, `REX_SESSION`, `REX_SERVER`, and `CLAUDE_CODE_SESSION_ID`, and its stdin had `session_id` and `source: "startup"` (measured). Codex's Namedrop hook ran one or two `bash` processes below `codex` (review's capture).

- **Shape.** `{block, incarnation, session, bound_at_ms}` as JSON, at `<state>/rex/sessions/<block uuid>.json`, in the install's state directory.
- **Writer.** Only `Place::bind_session`, from `run_context_hook` for SessionStart and UserPromptSubmit, under the nearest-agent rule. A temp file and a rename, and an unchanged record is not rewritten.
- **Replacement.** `/clear` and an in-place resume run SessionStart with a new `session_id` in the same process (inferred from Claude Code's hook contract), so the incarnation stays and the session changes. A new program has a new incarnation, and the old binding stops matching.
- **Deletion.** `RexEvents` calls `Bindings::forget` when it decodes `block_closed`. A file left behind while no bridge ran is harmless, because block IDs are never reused.
- **The producer of `Unreported`.** The Rex survey, through `pane_agent`, for a live agent whose incarnation matches no binding.

### 4. The budget is 5 s per request and 4 MiB per call

- **Timeout.** Each request is held to the lower of the budget's timeout and `NAMEDROP_REX_TIMEOUT_MS`, which defaults to 5 s and is capped at 60 s. Opening the connection counts as one request. The Stop wrapper sets 250 ms, as it does for Herdr. The peer probes in `mux::home` connect with a 250 ms limit, and a Unix connect returns at once unless the listen queue is full.
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

See [One module decides where a process runs](#one-module-decides-where-a-process-runs) for the rule and its measured table. Herdr passes an outer `REX_BLOCK` into its panes (measured in the isolated Herdr's pane) and into its plugin processes (review's `ps -Eww`). Rex passes inherited Herdr names into its blocks (measured). The rule needs no pane on either side, and any doubt about Herdr keeps a process on today's Herdr path.

### 8. One bridge per server feeds the existing worker

- **One process per server.** `namedrop rex bridge --endpoint rex:/…` holds `try_lock` on `<state>/rex-bridge/<sha256(endpoint)>.lock` for its whole life, so a second start exits 0. The endpoint is canonical, so two spellings of one server cannot start two bridges.
- **The stream.** One connection with `session.attach_all {event_scope: "all"}`. It delivered every event in the decode table (measured).
- **How it calls lifecycle and drain.** It does not. It enqueues `Marker::Event` into the per-space status queue and spawns the status worker, which runs `handle_mux_event`, then drains, with its existing `worker.lock`, backoff, draft rechecks, `DrainReport` timers, and `failing.json`.
- **Reconnect and reconcile.** When the stream ends, the bridge reconnects with backoff and queues `Resync` for every activated space on the endpoint. A space that answers `Gone` is retired.
- **Restart detection.** A restart ended the control connection with no close frame, killed each block's program, and kept the block IDs with a fresh `login` child and no status records (measured). Every program has a new incarnation, so the survey reports no session for any of them, and `observe_agent_attempt` holds each in-flight attempt.
- **The relay.** The bridge is also the relay's receiving client, because it already holds a connection to its server.
- **Starting with no plugin host.** `rex_bridge::ensure` is a `try_lock` probe and a spawn through `daemon::command`. The Rex runtime builder calls it, so any Namedrop command or hook in a Rex block starts the bridge. Step 6's Lua module adds a top-level `rex.exec` that calls it, which runs at every server start and `rex config reload` (measured).
- **Stopping.** SIGTERM or SIGINT stops it between flushes. A server unreachable for 120 s ends it with exit 0. A replaced runtime re-execs between flushes. The lock dies with it, and every marker it writes is idempotent.
- **Liveness.** The bridge pings after 120 s of silence and ends the stream after 60 s without a pong.

### 9. Remote Rex hosts are in the first release

The decision overturns revision 1's answer. Typing stays on the block's machine, and mentions relay between Namedrops over `client.perform`. See [Remote hosts](#remote-hosts-typing-stays-local-mentions-relay). The relay is not impossible or unsafe on the evidence, so no other transport is proposed.

## What steps 4 and 5 need from step 3

Step 3's review moved it to set-scoped names (flag per value, then the Herdr set when any of `HERDR_SOCKET_PATH`, `HERDR_WORKSPACE_ID`, or `HERDR_PANE_ID` is set, else the neutral set). The names are `NAMEDROP_ENDPOINT` and `--endpoint`, `NAMEDROP_SPACE_ID` and `--space`, `NAMEDROP_PANE_ID` and `--pane`, and `NAMEDROP_STATE_DIR`, with the `--herdr-*` flags as clap aliases of the same fields. This design needs seven things from it, and none depends on which set wins inside step 3's predicate.

1. **The set predicate as one function** that `mux::home::resolve` calls as its last branch, after the Rex test. Step 3's review outcome already shapes it that way.
2. **Reads of one set at a time.** A Rex home from neutral names reads only the neutral set.
3. **Set-scoped envelope readers.** A Rex writer puts only neutral keys in `control.tool.environment`.
4. **`Locator::pass` takes the endpoint's kind.** Herdr writes the Herdr set and clears the neutral one, as today. Rex writes the neutral set and clears the Herdr one, so a Rex endpoint never becomes `HERDR_SOCKET_PATH`. Step 4 may make this change itself if step 3 does not.
5. **`state_directory` takes the home's kind**, with the Rex default and the install record above. Under Herdr it stays step 3's.
6. **One field per location.** Step 4 parses the endpoint field's value for its kind, whatever flag spelling filled it.
7. **Nothing writes `NAMEDROP_*` into a block's environment.** Step 4 relies on it, because inherited identity in a Rex block comes only from `REX_*`.

## Implementation plan

Every commit compiles, passes `cargo fmt --check`, `cargo clippy --locked --all-targets -D warnings`, and `cargo test --locked --lib`, and ends in a state you can check. Step 4 starts from step 3's final head.

- **[H]** marks a commit that touches a Herdr path. Run `/verify` on real Herdr before it, which from commit 0b on includes the Herdr-in-Rex lane.
- **Live Rex checks** run on a scratch server with its own socket, state directory, and empty `init.lua`, started under `env -i` with `REX_SERVER` naming itself, never on the user's server. Until commit 11, their entry point is `NAMEDROP_REX_LIVE=unix://<socket> cargo test --lib mux::rex::live -- --ignored --test-threads=1`. From commit 11 on, the CLI runs inside a scratch block.
- **Until the bridge (commit 17)**, mail declined with `Declined(OnEvent)` and a registration completed while the agent is idle wait for an event that nothing sends. Step 4's live checks run `namedrop --plugin-state <scratch> reconcile` as the manual wake.

### Step 4. The adapter

0a. **Fixtures.** `src/mux/rex/fixtures/`: the six `vt` captures with the box state each screen shows, `process` replies (idle and busy `login` blocks, a command block, a killed agent's stale record), the hook chains (outer Claude Code, nested `claude -p`, Codex at two and three hops), event lines (lifecycle, kill order, restart), and the home table's eight rows. A `delivery.rs` test reads each capture with blank runs as spaces and asserts the box state its screen shows. Done when that test passes, and when the same test fed the raw captures fails on `codex-empty` and `codex-draft-fresh`, as the commit message quotes.
0b. **The Herdr-in-Rex testbed lane.** `testbed test --inside-rex` starts a scratch Rex server under `env -i` with `REX_SERVER` naming itself, runs the profile's `herdr server` inside a scratch block of it with that block's `REX_*` kept, runs the normal checklist, and stops both. `clean_environment` drops `REX_*` today, so this lane is the only one that sees a Rex claim. Done when the lane passes at the base, before any step 4 change.
1. **Delete `Admit`** [H]. Remove `PromptRequest.admit` and its three setters. Done when `grep -rnw Admit src` finds nothing and the gate and delivery suites pass.
2. **Move the budget helpers** [H]. `tighter` and `narrowest` move to `mux::model`. Done when the Herdr process tests pass unchanged.
3. **Walk past root-owned parents** [H]. `terminal::parent_pid` uses `PROC_PIDT_SHORTBSDINFO` on macOS. A test reads the parent of a root-owned process from `ps -axo pid,user`, which the old call cannot. Done when it passes and `verify_inherited_pane`'s tests pass unchanged.
4. **`TapChange` in the gate.** New gate tests cover a program change after the facts (declined, nothing pasted) and after the paste (`Ambiguous` at once). Done when the old and new gate tests pass.
5. **`rex/ws.rs`.** Tests over in-memory pipes cover the upgrade, masking, a 70 KiB message with a 64-bit length, a fragmented message, ping to pong, close, and an oversize frame. The live check reads a `welcome` from the scratch server.
6. **`rex/control.rs`.** Tests against a scripted server thread cover hello and welcome, request ids, an event buffered while a request waits, a timeout, cancellation within 50 ms, the 4 MiB byte count, and a server that closes mid-request.
7. **One connector and the local proof** [H]. `Endpoint::parse`, `Endpoint::unix_socket`, `rex/local.rs`, `mux::connect`, and `mux::Connector`. The deferred worker's probes and connect, the reminder clock, and the dashboard move to them. Live peek refuses Rex. Tests pin `""`, `"default"`, and `"/tmp/h.sock"` as Herdr and `"rex:/tmp/r.sock"` as Rex, assert that their lock keys differ, and run `prove_local` against a scripted server and a forwarder thread, which it refuses. The live check proves the scratch server local and refuses a forwarded socket to it. Done when `grep -rn "herdr::connect(Some" src` finds only `src/mux.rs`.
8. **Reads.** `rex/wire.rs` with `port_ansi`, `rex/agent.rs` with `pane_agent` and `shell_of`, the read side of `rex/binding.rs`, `terminal::process_start`, and `RexMux`'s `MuxRead`. Commit 0a's test switches to `port_ansi`. `rex/tests.rs` pins the requests for each `Scope` and `Depth` pair (the table below), checks `pane_agent` row by row from the fixtures, and checks `shell_of` on the idle and busy replies. The live check surveys a scratch session. It shows the agent, its incarnation's start matches `ps -o lstart`, and a terminal block reports its shell as available.
9. **Input and effects.** `rex/channel.rs`, `Line`, `Keys`, and `apply`. Tests pin the order (proof, `target.resolve`, `session.attach`, `program_status`, `process`, paste, `process`, Enter), skip an event at or below the facts' `seq`, decline on one above it, deliver on `working` after the Enter, hold without an Enter when the second `process` names another pid, and treat a paste error as `Ambiguous`. Further tests pin each key payload and each effect. The live check types into a scratch zsh and prompts a scratch Claude Code on Haiku. It captures the payloads for Escape, Up, and Down before the tests pin them, because only Enter, Ctrl-C, and Ctrl-U were measured.
10. **Kind-aware writers** [H]. `Locator::pass` with the endpoint's kind, `tool_environment` with neutral keys for Rex, `registration_command`, `runner_command`, and Doctor's repair line, and `scripts/runtime.py`. Tests: every existing Herdr line and envelope is byte for byte the same, the Rex ones use `--endpoint`, `--space`, and `--pane`, and the runtime script's selftest finds and wakes a worker that has only neutral names.
11. **The home** [H]. `mux::home::resolve` in place of step 3's predicate, the Rex state directory with the install record, the runtime builder, the seven identity sites, `daemon::command` and its boundary test, and the Stop wrapper's `NAMEDROP_REX_TIMEOUT_MS`. A table test runs the fixtures' eight home rows over a scripted `LocalProcesses` and scripted peer pids. A mutation that restores revision 1's rule (a Rex claim wins when Herdr has no pane) must fail that test and the Herdr-in-Rex lane. Done when both run green and red as stated, the CLI in a scratch block resolves its block, and `/verify` passes on both lanes.
12. **The binding from hooks** [H]. `Place::bind_session`, `hook_agent`, and the call in `run_context_hook`. Tests from the fixture chains: the outer Claude Code hooks bind, the nested `claude -p` hooks are refused, Codex binds at two and three hops, an identical second call does not rewrite the file, `/clear` with a new session replaces it, and a Herdr home writes nothing. The live check binds Claude Code and Codex, each in a scratch block, and refuses a `claude -p` run as a tool.
13. **Live check for step 4.** On the scratch server, start a Claude Code block and a Codex block in one session. Name each through the CLI, mention one from the other, wake with `reconcile`, and see the reply. Then `kill -9` the Claude Code process, queue a mention to it, and confirm that nothing is typed into zsh. Record the evidence in PLAN.md and PORT.md. This meets PLAN's step 4 done-when.

The survey requests that commit 8 pins:

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

14. **The status queue's `Marker`** [H]. `Marker`, `enqueue`, and `spawn_worker` move to `status_queue.rs`, the worker dispatches on `Marker::parse`, and it reads its scope from the home. `MuxEvent` gains serde. Done when the worker's tests pass unchanged and a round-trip test passes for all four shapes.
15. **`handle_mux_event`, `Resync`, and `SpaceClosed`.** FakeMux tests: a `PaneClosed` retires a name, a `SpaceClosed` retires every name in its space and fails its reminders, a `Resync` that finds the space gone does the same, a `Resync` keeps a `Submitted` attempt whose session still runs and holds one whose program was replaced, and a `Gone` failure is `Definitive` while a timeout is `Retry`. The Herdr `event.*` tests pass unchanged.
16. **`rex/events.rs`.** Scripted-wire tests cover each row of the decode table with the fixture lines. `title_changed` yields nothing, coalescing keeps one status change per pane, `SpaceClosed` absorbs its space's markers, and a missing pong ends the stream.
17. **`src/rex_bridge.rs`, `namedrop rex bridge`, and `ensure`.** The loop takes a private stream trait, so a test can script it over FakeMux and a temp state directory. Tests: a second `run` exits at once, each connect queues one `Resync` per activated space, a flush enqueues and spawns once per space, and SIGTERM stops it between flushes. Done when a scripted `block_closed` retires a name through the real worker.
18. **Live check for step 5.** On the scratch server, close a named block and see the name go with no manual command. `rex kill` a session with two named blocks and see both names go. Queue a mention to a working Codex and see it drain after `idle`. Restart the server and see in-flight mail to the replaced programs held, then bind a new agent in the same block and name it again. Record the evidence in PLAN.md.

### Step R. The relay

Same-host Namedrop works on any host once step 5 lands. A real remote host also needs Namedrop installed there without Herdr, which is PLAN step 6. The stand-in tests the relay before that, with B's Namedrop run from the build with explicit flags.

19. **Receiving.** The bridge declares `namedrop.relay`, `namedrop relay accept` inserts with its unique key, and the principal policy. Tests script `client_perform_requested` events: a new message is accepted once, a repeat is a duplicate, an unknown name and an anonymous principal are refused, and a slow accept still replies within 5 s. The live check calls `client.perform` on the stand-in with `rex -S standin-b` and sees each reply shape.
20. **Sending.** `@name@host` addresses, the outbox, the two `rex -S` calls through `daemon::command`, the outcome table, retries, and reply routing by `host_id`. Tests run a script `rex` on `PATH` that answers each outcome, and check the outbox after each.
21. **Live check for the relay.** On the stand-in, a mention from A's Claude Code to B's Codex arrives and B's reply returns to A. Through the delay-and-reset forwarder, a timed-out send is retried and arrives once. With B's bridge stopped, A keeps the message, and it arrives after the bridge starts. A refuses to type into B's blocks. Record the evidence in PLAN.md.

### Deferred

The Rex conformance suite (revision 1's commit 8), backward `blob_read` windowing, `AgentDetected`, a `namedrop.roster` action for remote name completion, and a live TLS test against the user's real server.

## What only the operator can decide

- **TLS to the real server.** The relay's live test reaches only the stand-in. A test over the user's tailnet needs the user's approval.
- **Who may relay.** This design accepts same-uid and tailnet principals and refuses anonymous ones. Only the operator can widen that.
- **Registration through hooks.** On Rex an agent can register only after a Namedrop hook ran in it, so an agent started before the install needs one prompt first. Codex in daemon mode, and any agent behind a forking wrapper, cannot register on Rex. Those are product calls.
- **The Rex app.** PLAN's open questions about restarting the app and seeing the actions in the palette need the app attached to the operator's real server.
- **Requests to Superlogical.** An atomic paste-if-idle would close the last race in answer 1. A pgid in `process` would remove one local read. A server that sets `REX_SERVER` for its own blocks would remove the export the stand-in needs.

## Open questions and risks

- Does Codex run SessionStart for a sub-session in the same process? If it does, the sub-session would take the block's binding, because no process lies between them. The hook could skip a SessionStart whose `source` marks a sub-session.
- What is the default Rex socket on Linux, and do `SO_PEERCRED` and the `/proc` start time behave? Only macOS was measured.
- Rex is a preview. The adapter refuses a welcome whose `api_revision` is not 12 until a newer revision is checked, and Doctor reports it.
- A crashed agent leaves its terminal modes behind, so a terminal line or key sent to such a block may not act as it would in a clean shell. Herdr panes have the same exposure (inferred).
- A reply between hosts needs both machines to list each other in `rex hosts`. How often that holds is unknown.

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

The probe programs lived in the session scratch directory and are not kept. This section keeps the commands and the trimmed output. Revision 1's evidence comes first, then revision 2's.

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

Scratch servers ran as `env -i HOME USER SHELL PATH REX_SERVER=unix://<sock> rex server run --listen unix://<sock> --state-dir <dir> --config <empty init.lua>`, with `--extra-listen 127.0.0.1:47298 --allow-anonymous` for host B.

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

**The prompt box from Rex captures.** A test added to a scratch copy of step 3's `src/delivery.rs` ran `prompt_box` on the last 60 lines of each of the review's captures, raw and with each `ESC[nC` as n spaces. The captures hold only SGR (`m`) and cursor-forward (`C`) sequences.

```text
claude-draft.vt:       raw=Draft  spaces=Draft
claude-empty.vt:       raw=Empty  spaces=Empty
claude-placeholder.vt: raw=Empty  spaces=Empty
codex-draft-fresh.vt:  raw=Unseen spaces=Draft
codex-draft.vt:        raw=Draft  spaces=Draft
codex-empty.vt:        raw=Draft  spaces=Empty
test result: ok. 1 passed
```

**The nearest-agent rule on captured hook chains** (the review's `hooklog` and Codex samples):

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

### Reproduce the probes

Run these from a Rex terminal. The server's own `curl --unix-socket "$HOME/Library/Application Support/rex/server.sock" http://localhost/llms.txt` documents every message. Keep scratch socket paths under 104 bytes, and Herdr's shorter still.

```sh
# A scratch server whose blocks know it.
X=$(mktemp -d /tmp/ndrex.XXXX); mkdir -p "$X/cfg"; : > "$X/cfg/init.lua"
env -i HOME="$HOME" USER="$USER" SHELL=/bin/zsh PATH=/usr/bin:/bin REX_SERVER="unix://$X/s.sock" \
	"$(command -v rex)" server run --listen "unix://$X/s.sock" --state-dir "$X/state" --config "$X/cfg/init.lua" &
created=$(rex -S "unix://$X/s.sock" --autostart=false new nd-probe -c "$PWD" --json -- claude --model haiku)
S=$(printf '%s' "$created" | jq -er .session_id)
B=$(printf '%s' "$created" | jq -er '.initial_windows[0].block_ids[0]')
rex -S "unix://$X/s.sock" --autostart=false block call -s "$S" -b "$B" process | jq -c '{child, foreground, ancestors}'
rex -S "unix://$X/s.sock" --autostart=false kill "$S"; kill %1

# An isolated Herdr for a nested probe. Check the socket before starting anything.
export HOME="$X/home" XDG_CONFIG_HOME="$X/config" XDG_STATE_HOME="$X/state-h" HERDR_SOCKET_PATH="$X/h.s" HERDR_CONFIG_PATH="$X/config/herdr/config.toml"
herdr status server   # must print "socket: $X/h.s" and "not running"
```
