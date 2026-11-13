# Rex adapter and bridge design

This document is the design for steps 4 and 5 of the [Rex host plan](PLAN.md). Step 4 adds a Rex adapter behind the `Mux` port that [PORT.md](PORT.md) built in step 2. Step 5 adds the bridge, one long-lived process per Rex server that turns Rex events into Namedrop's lifecycle work. Nothing here is implemented yet.

The design is for the agent who implements steps 4 and 5, and for the person who reviews them. An agent in a Rex block gets the same promise an agent in a Herdr pane has today. Namedrop types into it only when the agent Namedrop verified is the live program and reports `idle` or `done`, and it never sends a prompt twice.

The evidence comes from live probes on 2026-11-12 with Rex `0.1.0-preview.14` (API revision 12), Claude Code 2.1.323 on Haiku, and Codex CLI on its default model, all on macOS. Each probe used scratch Rex sessions or a separate scratch server. Every scratch session was killed and the scratch server was stopped afterwards. [Evidence](#evidence) gives the commands and the trimmed output. Each claim below says whether it was measured, inferred, or guessed.

The design branch is `exp/rex-adapter-design`, cut from `origin/exp/mux-delete-bridge` (`013c129`) because PR #191 had not merged.

## Problem

Rex runs agents in blocks and reports their status, but it differs from Herdr in five ways that shape this design.

- **Rex reports no agent session** (measured). Namedrop's own SessionStart hook is the only witness to which session runs in a block. `SessionClaim::Unreported` has no producer yet, and `resolve_session` answers `None` for it.
- **A status record can outlive its program** (measured). After `kill -9` of Claude Code under a shell, Rex kept `{state: idle, owner: {pid: <dead pid>}}` while zsh held the terminal. The OSC 7501 protocol says `done` must survive a process exit and `idle` may. A gate that trusts the record would paste a mention into zsh and press Enter.
- **The gate needs one connection with an order** (measured). The tap, the facts, the paste, and the Enter must share one `rex.control.v1` connection, whose `seq` numbers order events against responses. A `rex` CLI process per call costs about 20 ms, and a `rex events` tap carries no `seq`.
- **Rex has no plugin host** (measured). Lifecycle events reach Namedrop only through a process that holds the event stream open.
- **Rex passes Herdr's variables through** (measured), and Herdr likely passes Rex's (inferred). A Rex server passes an inherited `HERDR_PANE_ID` into its blocks, and it passes an inherited `REX_SERVER` that can name a different server. On the machine this was measured on, the everyday Herdr runs inside a Rex block, so every Herdr pane descends from an outer Rex block too.

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

The runtime builder asks `mux::home` which multiplexer this process runs in before today's body runs. Without a verified Rex claim, today's Herdr body runs unchanged. `main.rs` names no Rex type.

```rust
// main.rs
fn with_runtime_options<T>(options: &RuntimeOptions, action: impl FnOnce(&Runtime<'_>) -> Result<T, String>) -> Result<T, String> {
	let state = state_directory(options)?; // step 3's neutral state directory
	match mux::home::resolve(&state, &options.home())? {
		Home::Herdr { inherited_pane } => with_herdr_runtime(options, &state, inherited_pane, action), // today's body
		Home::Placed(place) => {
			let mux = mux::connect(&state, &place.endpoint, Budget::ADAPTER_DEFAULT, Arc::default())?;
			action(&LifecycleRuntime {
				workspace_id: Some(place.space.as_str()),
				current_pane_id: place.pane.as_ref().map(PaneId::as_str),
				session_socket_id: &place.endpoint, // "rex:/Users/me/Library/Application Support/rex/server.sock"
				mux: &mux,
				..common_runtime_parts(&state)
			})
		}
	}
}
```

Every place that rebuilds a connection from a stored `session_socket_id` calls one function instead of `herdr::connect`.

```rust
// deferred_commands.rs, reminder/clock.rs
let mux = crate::mux::connect(state, &namespace.session_socket_id, Budget::ADAPTER_DEFAULT, Arc::default())?;
// dashboard.rs and its services
let connector: Arc<dyn MuxConnector> = Arc::new(mux::Connector::new(state));
```

The context hook binds the session before it builds the runtime, so the runtime's first survey already reports it.

```rust
// main.rs run_context_hook, for SessionStart and UserPromptSubmit
let home = mux::home::resolve(state, &HomeOptions::inherited())?;
if let Some(session) = hook_session_id(&input) { // stdin `session_id`
	home.bind_session(state, &session); // a no-op for Herdr, silent on failure like every hook path
}
with_home_runtime(home, state, |runtime| Ok(execute(&input, runtime)))
```

The bridge translates and enqueues. It never calls lifecycle or drain code. The existing status worker does that, as it does for Herdr.

```rust
// rex_bridge.rs, for each coalesced change
status_queue::enqueue(state, endpoint, &space, &Marker::Event(event))?;
status_queue::spawn_worker(state, endpoint, &space)?; // the spawn the Herdr hook uses today

// main.rs, the status worker's one dispatch
match Marker::parse(&input)? {
	Marker::Event(event) => namedrop::handle_mux_event(runtime, &event),
	Marker::Wake => namedrop::drain_mailboxes(runtime).map(to_value),
	Marker::DraftRecheck(held) => namedrop::recheck_drafts(runtime, held).map(to_value),
	Marker::Herdr(data) => run_lifecycle("event.agent-status-changed", data, ..), // today's raw shape
}
```

Adapter tests script a private `Wire`, as `herdr/tests.rs` scripts `Transport`, and pin the requests each port call sends.

```rust
#[test]
fn a_prompt_reads_the_facts_on_the_tapped_connection_then_pastes_and_presses_enter() {
	let rex = scripted(|call, events| match call.method {
		"target.resolve" => Ok(block_in("session:s", "block:b")),
		"session.attach" => Ok(json!({})),
		"program_status" => Ok(json!({"records": [root("idle", "claude-code", 41, "2.1.323")]})),
		"process" => Ok(json!({"child": proc(40, "login"), "foreground": proc(41, "2.1.323")})),
		"block.event" if is_enter(&call) => { events.status("block:b", "working"); Ok(json!({})) }
		"block.event" => Ok(json!({})),
		_ => Err(rex_error("not_found")),
	})
	.started(41, 1_700_000)
	.bound("block:b", "41@1700000", "session-1");
	assert_eq!(rex.send(&PaneId::new("block:b"), prompt("hello", &claude("session-1"))), InputOutcome::Delivered);
	assert_eq!(calls(&rex), ["target.resolve block:b", "session.attach session:s", "program_status block:b",
		"process block:b", "block.event paste hello", "block.event key enter"]);
}
```

## Shape

### Module map

| Path | Owns |
|---|---|
| `src/mux/rex.rs` | `RexMux`, `connect`, the `MuxRead`, `MuxAct`, and `MuxInput` impls, and the effect and key mapping. The public surface listed under [Interface depth](#interface-depth) |
| `src/mux/rex/ws.rs` | A client-side WebSocket over `std::os::unix::net::UnixStream`: the HTTP upgrade, masked frames, 7-, 16-, and 64-bit lengths, fragments, ping and pong, and close. No compression and no new crate |
| `src/mux/rex/control.rs` | The private `Wire` and `Control` traits and their production impl. Hello and welcome, request ids, the event buffer, the per-operation budget and byte count, cancellation |
| `src/mux/rex/wire.rs` | Rex JSON parsed into domain types, Rex error codes mapped to `MuxError`, method names, and the canonical socket path |
| `src/mux/rex/agent.rs` | `pane_agent`, the pure liveness rule that the survey and the gate both call |
| `src/mux/rex/channel.rs` | `RexChannel` and `RexTap`, the `gate::RawChannel` and `gate::StatusTap` impls |
| `src/mux/rex/binding.rs` | Session bindings under `<state>/rex/sessions/`, `bind_session`, and the incarnation read |
| `src/mux/rex/caller.rs` | `RexClaim`, `RexCaller`, and `locate_caller`. Finds the server that owns `REX_BLOCK` and proves this process runs inside it |
| `src/mux/home.rs` | `Home` and `resolve`. Which multiplexer this process runs in, from flags, the environment, and ancestry. `Home::bind_session` |
| `src/mux/rex/events.rs` | `RexEvents`, the bridge's stream, and `classify`, the pure decode table from a Rex event to a `MuxEvent` |
| `src/mux/rex/tests.rs` | Scripted-`Wire` tests that pin the requests for every port call |
| `src/mux/rex/conformance.rs` | FakeMux's world rendered as Rex replies, so the shared behavioral suite runs against `RexMux` |
| `src/rex_bridge.rs` | The bridge loop, its lock, coalescing, `ensure`, and `run` |

Changes outside these files:

| Path | Change |
|---|---|
| `src/mux/model.rs` | `Endpoint::parse`, `Endpoint::unix_socket`, and `REX_SCHEME`. `Budget::tighter` and `narrowest`, moved from `herdr::transport` |
| `src/mux.rs` | `connect`, `Connector`, and `innermost` |
| `src/mux/gate.rs` | `TapChange`. A program change declines before the paste and is `Ambiguous` at once after it |
| `src/mux/input.rs` | `Admit` and `PromptRequest.admit` deleted |
| `src/mux/event.rs` | `MuxEvent::Resync(SpaceId)` and serde for every variant |
| `src/lifecycle.rs` | `pub fn handle_mux_event` over the private `handle_event`. Attribution names the endpoint's multiplexer. The `Resync` handler |
| `src/status_queue.rs` | `Marker`, `enqueue`, and `spawn_worker`, moved out of `main.rs` |
| `src/terminal.rs` | `process_start(pid)` beside `parent_pid`, from the same `proc_pidinfo` call, or `/proc/<pid>/stat` on Linux |
| `src/integration.rs` | `tool_environment` writes the `HERDR_*` keys only for a Herdr endpoint |
| `src/main.rs` | The runtime builder and the context hook call `mux::home`. The `rex bridge` subcommand. The worker's `Marker` dispatch, with its scope read from step 3's neutral names |
| `src/deferred_commands.rs`, `src/reminder/clock.rs`, `src/dashboard*` | `mux::connect` or `mux::Connector` in place of `herdr::connect` and `HerdrConnector`. The four `UnixStream::connect(socket)` liveness probes in `deferred_commands.rs` (lines 354, 441, 468, and 489) connect to `Endpoint::unix_socket`. Live peek refuses a Rex binding |

### The endpoint string carries the kind

A stored endpoint is a `session_socket_id`. A Rex endpoint is `rex:` followed by the percent-decoded absolute path of the server's Unix socket. No stored Herdr value starts with `rex:`, because they are absolute paths, `default`, or empty. So every stored Herdr value parses as Herdr, byte for byte, and no migration runs.

```rust
// src/mux/model.rs
pub const REX_SCHEME: &str = "rex:";

impl Endpoint {
	/// The one reader of a stored endpoint: a session_socket_id, an envelope's
	/// NAMEDROP_ENDPOINT, or an --endpoint flag. `key` stays the stored string.
	pub fn parse(stored: &str) -> Self {
		let kind = if stored.starts_with(REX_SCHEME) { MuxKind::Rex } else { MuxKind::Herdr };
		Self { kind, key: stored.to_owned() }
	}

	/// The socket a liveness probe connects to: the Herdr path, or the path after `rex:`.
	pub fn unix_socket(&self) -> Option<&Path> { todo!() }
}

// src/mux.rs
pub fn connect(state: &Path, endpoint: &str, budget: Budget, cancelled: Arc<AtomicBool>) -> Result<Arc<dyn Mux>, MuxError> {
	match Endpoint::parse(endpoint).kind {
		MuxKind::Herdr => Ok(herdr::connect(Some(endpoint), budget, cancelled)),
		MuxKind::Rex => rex::connect(state, endpoint, budget, cancelled),
	}
}

/// `connect` as a `MuxConnector`, for the dashboard and its services.
pub struct Connector { state: PathBuf }

/// The multiplexer whose anchor process is the nearest ancestor of this one.
/// An unverified claim never wins over a verified one. With neither verified
/// the answer is Herdr, which is today's path.
pub fn innermost(herdr_depth: Option<usize>, rex_depth: Option<usize>) -> MuxKind {
	match (herdr_depth, rex_depth) {
		(Some(herdr), Some(rex)) if rex < herdr => MuxKind::Rex,
		(None, Some(_)) => MuxKind::Rex,
		_ => MuxKind::Herdr,
	}
}
```

The canonical form is lexical. The adapter percent-decodes the path from `REX_SERVER` or the default socket and normalizes it, but it does not resolve symlinks, because that would give a different string while the server is down and split the store namespace.

### One module decides where a process runs

```rust
// src/mux/home.rs
pub enum Home {
	/// Today's path. The Herdr runtime builder runs unchanged, with its own
	/// --herdr-* flags, HERDR_* variables, and verify_inherited_pane. When
	/// `resolve` already read the inherited pane's process to compare depths,
	/// it passes that reply on, so the builder does not ask Herdr twice.
	Herdr { inherited_pane: Option<PaneProcess> },
	/// A verified or asserted place on a multiplexer that is not Herdr.
	Placed(Place),
}

pub struct Place {
	pub endpoint: String, // the canonical stored form, "rex:/…"
	pub space: SpaceId,
	pub pane: Option<PaneId>,
	/// Present only when this process descends from the pane's foreground program.
	caller: Option<RexCaller>,
}

pub struct HomeOptions { endpoint: Option<String>, space: Option<String>, pane: Option<String>, herdr_flags: bool }

/// 1. An explicit --endpoint decides, with --space and --pane trusted as --herdr-pane is today.
/// 2. Any --herdr-* flag means Herdr.
/// 3. Otherwise a REX_BLOCK claim must verify through `rex::locate_caller`, and it
///    wins only when `innermost` puts its anchor nearer than a verified HERDR_PANE_ID.
/// 4. Anything else is `Herdr`, so a Rex failure never becomes a new Herdr error.
pub fn resolve(state: &Path, options: &HomeOptions) -> Result<Home, String> { todo!() }

impl Home {
	/// Records the hook's session for the pane's foreground program. A no-op
	/// for Herdr, which reports sessions itself, and for an asserted place.
	pub fn bind_session(&self, state: &Path, session: &AgentSessionId) { todo!() }
}
```

With no `REX_BLOCK`, `resolve` returns `Herdr` without a request. With both claims present, it reads the Herdr pane's `shell_pid` through the same `process` call that `verify_inherited_pane` makes, and hands the reply on, so a Herdr command pays only for the Rex probe.

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
pub use binding::bind_session;
pub use caller::{RexCaller, RexClaim, locate_caller};
pub use events::RexEvents;

/// Opens nothing. Each port call opens one control connection, runs, and closes it.
pub fn connect(state: &Path, endpoint: &str, budget: Budget, cancelled: Arc<AtomicBool>) -> Result<Arc<dyn Mux>, MuxError> { todo!() }

pub(crate) struct RexMux<W> {
	wire: W,
	endpoint: Endpoint,
	bindings: Bindings,
	/// Start times of local processes. Scripted in tests.
	local: Box<dyn LocalProcesses>,
}

impl<W: Wire> RexMux<W> {
	/// `MuxInput::deliver` minus the lock token, for tests, as `HerdrMux::send` is.
	pub(crate) fn send(&self, pane: &PaneId, kind: InputKind<'_>) -> InputOutcome { todo!() }
}

// src/mux/rex/control.rs
/// Opens one connection: connect, upgrade, hello, welcome. A scripted impl replaces it in tests.
pub(super) trait Wire: Send + Sync {
	fn open(&self, limits: Limits<'_>) -> Result<Box<dyn Control>, Fault>;
}
pub(super) trait Control {
	/// One request and its response. `Reply.seq` is the last event queued before the request started.
	fn request(&mut self, method: &str, payload: Value) -> Result<Reply, Fault>;
	/// The next buffered or arriving event, or `None` when `within` passes.
	fn next_event(&mut self, within: Duration) -> Result<Option<Event>, Fault>;
}
pub(super) struct Limits<'a> { pub budget: Budget, pub cancelled: Option<&'a AtomicBool> }
pub(super) struct Reply { pub seq: u64, pub payload: Value }
pub(super) struct Event { pub seq: u64, pub body: Value }
pub(super) enum Fault {
	Rex { code: String, message: String },
	Timeout(Duration),
	TooLarge,
	Closed(String),
	Cancelled,
	/// The allowance refused before any request was sent.
	Allowance(String),
}
```

**One connection per port call.** `survey`, `screen`, `process`, `tab_label`, `apply`, and `deliver` each open a connection, run their requests, and drop it, which sends close 1000. A fresh connection with hello, attach, and one method took 0.8 ms, and a request on a held one took 0.1 ms (measured). A held connection would save 0.7 ms per call at the price of a reader thread, a pending-request map, reconnect logic, and a stale socket after a server restart. The bridge is the only holder of a long-lived connection.

**The liveness rule is one pure function.** The survey and the gate's facts both call it, so they cannot disagree.

```rust
// src/mux/rex/agent.rs
pub(super) struct Observed<'a> {
	pub record: Option<&'a StatusRecord>,      // the root record, `id` absent or ""
	pub foreground: Option<&'a Proc>,          // `process.foreground`
	pub block_is_local: bool,                  // the block's host_id is the server's host_id
	pub incarnation: Option<&'a Incarnation>,  // the foreground's, read locally
	pub binding: Option<&'a Binding>,
}
pub(super) struct PaneAgent { pub agent: Option<AgentIdentity>, pub state: Option<AgentState>, pub reports_session: bool }

/// A record speaks for the block only while its owner is the live foreground
/// process, pid and name both. A session is reported only while the binding's
/// incarnation is the foreground's, so a replaced program loses it.
pub(super) fn pane_agent(observed: Observed<'_>) -> PaneAgent { todo!() }
```

| Record owner is the live foreground, and `app` is `claude-code` or `codex` | Binding incarnation equals the foreground's | `agent` | `state` |
|---|---|---|---|
| yes | yes | `Known(kind)`, `Reported(session)` | the record's state |
| yes | no binding, or another incarnation | `Known(kind)`, `Unreported { incarnation }` | the record's state |
| yes | start time unreadable, or the block is not local | `Known(kind)`, `SessionClaim::None` | the record's state |
| no record yet | yes | `AgentKind::Unreported`, `Reported(session)` | `None` |
| no, the record is stale, or `app` is another program | any other case | `None`, a free terminal | `None` |

`app` must be `claude-code` or `codex` (both measured). Any other program that reports OSC 7501, such as `cargo`, leaves the block a free terminal, because Namedrop can address only those two agents. The state goes through `AgentState::parse`, so `error` and unknown words become `Unknown` and are never admitted. The protocol says to treat an unknown state as `idle`, and Namedrop is stricter on purpose. `PaneFacts.reports_session` is `true` when the pane reports a bound session or no agent at all, and `false` when an agent runs whose session is unknown (`Unreported` or `None`). Subagent maintenance reads a reported but missing session as ended and an unreported one as unknown, so an agent whose hook has not run yet keeps its subagent records.

**The gate channel.** `deliver` for a prompt opens one connection and runs `gate::emulate_prompt` over it.

```rust
// src/mux/rex/channel.rs
pub(super) struct RexChannel<'a> {
	control: RefCell<Box<dyn Control>>,
	block: BlockRef, // the block, its session, and its window, from one target.resolve
	local: &'a dyn LocalProcesses,
	bindings: &'a Bindings,
	/// The lowest response seq of the facts reads, and the foreground pid they saw.
	read: Cell<Option<(u64, u32)>>,
}

impl gate::RawChannel for RexChannel<'_> {
	type Tap = RexTap;
	/// session.attach for the block's session. From here the session's events buffer on the connection.
	fn tap(&self, pane: &PaneId) -> Result<RexTap, ChannelError> { todo!() }
	/// program_status, then process, then the local start time and the binding file. `pane_agent` answers.
	fn facts(&self, pane: &PaneId) -> Result<(Option<AgentIdentity>, Option<AgentState>), ChannelError> { todo!() }
	/// block.event paste {text}, wait for its ack, then block.event key Enter, wait for its ack. No sleep.
	fn paste_submit(&self, pane: &PaneId, text: &str) -> Result<(), ChannelError> { todo!() }
}

impl gate::StatusTap for RexTap {
	fn next_change(&mut self, within: Duration) -> Option<TapChange> {
		// Skip events with seq <= the facts' seq, because the facts include them, and events for other blocks.
		// Root program_status_changed       => Status(AgentState::parse(record.state)).
		// Root program_status_removed       => Program.
		// child_exited                      => Program.
		// process_changed                   => Program only when its foreground pid differs from the facts' pid,
		//                                      because Rex also sends it whenever a client starts reading the terminal.
		// title_changed and every other event are skipped.
		todo!()
	}
}
```

`deliver` for `Line` and `Keys` sends `block.event` paste and key events with no confirmation. A Rex error on the first request is `Refused`, because nothing was typed. A fault after the first request was written is `Ambiguous`. `Recipient::Agent` and `Recipient::Pane` type the same way on Rex.

**The binding.** One JSON file per block, written only by `bind_session`, and deleted only by `RexEvents` when it decodes `block_closed`. Both writers live in the adapter. Block IDs are never reused and survive a server restart (measured), so the block ID alone is the key. The endpoint stays out of the key on purpose, because two processes can spell one server two ways.

```rust
// src/mux/rex/binding.rs
/// `<pid>@<start>`. Start is microseconds since the epoch on macOS
/// (pbi_start_tvsec and pbi_start_tvusec) and clock ticks since boot on Linux
/// (/proc/<pid>/stat field 22). Only equality matters, and the hook and the
/// survey read it on the same machine.
pub(super) fn incarnation(local: &dyn LocalProcesses, pid: u32) -> Option<Incarnation> { todo!() }

/// Records that `session` runs in the caller's block, bound to the
/// incarnation of the block's foreground program. Refuses a caller whose
/// block foreground is not one of its ancestors, and one whose start time
/// cannot be read. Writes only a change, so UserPromptSubmit can call it on
/// every prompt.
pub fn bind_session(state: &Path, caller: &RexCaller, session: &AgentSessionId) -> Result<(), String> { todo!() }

#[derive(Serialize, Deserialize, Eq, PartialEq)]
pub(super) struct Binding {
	pub block: String,       // "block:<uuid>"
	pub incarnation: String, // "93037@1794528253006243"
	pub session: String,     // the hook's stdin session_id
	pub bound_at_ms: u64,
}

pub(super) struct Bindings { directory: PathBuf } // <state>/rex/sessions, mode 0700
impl Bindings {
	/// A missing, unreadable, or malformed file reads as no binding.
	pub(super) fn get(&self, block: &PaneId) -> Option<Binding> { todo!() }
	/// A temp file and a rename, mode 0600, so a reader never sees half a record.
	fn put(&self, binding: &Binding) -> io::Result<()> { todo!() }
	pub(super) fn forget(&self, block: &PaneId) { todo!() }
}
```

**The caller.**

```rust
// src/mux/rex/caller.rs
/// What the environment claims. REX_SERVER can name a server other than the block's owner (measured).
pub struct RexClaim { server: Option<String>, session: SpaceId, block: PaneId }
impl RexClaim {
	/// `None` unless both REX_SESSION and REX_BLOCK are set.
	pub fn from_env() -> Option<Self> { todo!() }
}

pub struct RexCaller {
	/// The canonical `rex:` endpoint, which becomes the runtime's session_socket_id.
	pub endpoint: String,
	pub space: SpaceId,
	pub pane: PaneId,
	/// How many parents up this process the block's `process.child` is.
	pub depth: usize,
	/// The block's foreground program, when it is an ancestor of this process. Only such a caller can bind.
	agent: Option<Incarnation>,
}

impl RexCaller {
	/// From --endpoint, --space, and --pane, which a collab/v1 envelope wrote. Trusted like --herdr-pane today. Binds nothing.
	pub fn asserted(endpoint: &str, space: &str, pane: &str) -> Result<Self, String> { todo!() }
}

/// Tries REX_SERVER, then the platform's default socket. Skips a server that
/// does not know the block. On the server that knows it, the claim verifies
/// when the block's `process.child` is an ancestor of this process. Failing
/// that, it verifies when CLAUDE_CODE_SESSION_ID or CODEX_SESSION_ID equals
/// the block's live binding, the second proof `verify_inherited_pane` already
/// accepts for Herdr. Each try is one connection and a few requests.
pub fn locate_caller(claim: &RexClaim, bindings: &Bindings, budget: Budget) -> Result<RexCaller, String> { todo!() }
```

### The bridge, the queue, and the worker

```rust
// src/mux/event.rs
#[derive(Serialize, Deserialize)]
pub enum MuxEvent {
	// the six existing variants
	/// Events for this space may have been missed, as when the Rex bridge (re)connects.
	Resync(SpaceId),
}

// src/lifecycle.rs
/// Runs the handler for one multiplexer event. The Herdr event operations and
/// the status worker's Rex markers both enter here. The attribution actor is
/// the endpoint kind's word, "herdr" or "rex".
pub fn handle_mux_event<M: Mux, C: Clock, I: IdGenerator>(runtime: &LifecycleRuntime<'_, M, C, I>, event: &MuxEvent) -> Result<Value, String> { todo!() }
// MuxEvent::Resync(space) => discover the space, require_activated, then
// reconcile_worktree(.., after_restart = false). See answer 8 for why false is right.

// src/status_queue.rs
pub enum Marker {
	Wake,                         // {"operation":"mailbox-wake"}, as today
	DraftRecheck(Vec<DraftHold>), // {"operation":"draft-recheck","held":[..]}, as today
	Event(MuxEvent),              // {"operation":"mux-event","event":{..}}, new
	Herdr(Value),                 // today's raw Herdr status data, decoded later by design (PORT.md commit 10)
}
impl Marker { pub fn parse(input: &Value) -> Result<Self, String> { todo!() } }
pub fn enqueue(state: &Path, endpoint: &str, space: &SpaceId, marker: &Marker) -> io::Result<()> { todo!() } // queued-*, fsync, rename to pending-*
pub fn spawn_worker(state: &Path, endpoint: &str, space: &SpaceId) -> Result<(), String> { todo!() }       // `hook agent-status-worker` with the neutral names

// src/rex_bridge.rs
/// One bridge per Rex server. Holds try_lock on <state>/rex-bridge/<sha256(endpoint)>.lock for its life.
pub fn run(state: &Path, endpoint: &str) -> ExitCode { todo!() }
/// Spawns `namedrop rex bridge` detached unless the lock is held.
pub fn ensure(state: &Path, endpoint: &str) -> Result<(), String> { todo!() }
```

```text
run(state, endpoint):
	try_lock <state>/rex-bridge/<sha256(endpoint)>.lock, or exit 0
	stop = a flag that SIGTERM and SIGINT set
	away = backoff 100 ms, 250 ms, 500 ms, 1 s, then 5 s; give up after 120 s without a server
	while not stop:
		stream = RexEvents::open(endpoint), else wait on `away`, or exit 0 when it gives up
		log server.status instance_id for the doctor
		for each activated space of this endpoint in the store: pending.push(Resync(space))
		loop:
			flush when 50 ms pass with no event, or 250 ms after the first pending change:
				take runtime_command_guard, call runtime_reexec_if_replaced
				for each (space, marker) in pending: status_queue::enqueue
				for each space with a new marker: status_queue::spawn_worker
				release the gate
			match stream.next(time to the next flush):
				Some(event) => pending.note(classify(event)) // coalesced, see below
				None => {}
				Ended => break, then reconnect. In-flight mail is untouched.
```

| Rex event (inside `session_event` unless noted) | Marker |
|---|---|
| `block_created` | `PaneAppeared` |
| `block_closed` | `PaneClosed`, after `RexEvents` deletes the block's binding file |
| `window_closed` | `TabClosed { space, tab: window_id }` |
| `block_event program_status_changed`, root record | `AgentStatusChanged` |
| `block_event program_status_removed`, root record | `AgentStatusChanged` |
| `block_event process_changed` | `AgentStatusChanged`. The first block of a new session sends this and no `block_created` (measured) |
| `block_event child_exited` | `AgentStatusChanged`. `block_closed` follows when the program's exit closes the block (measured) |
| `session_created`, `session_destroyed`, `title_changed`, `pwd_changed`, `bell`, `size_changed`, `client_*`, `session_view_changed`, anything unknown | Nothing. `rex kill` sends `block_closed` for every block before `session_destroyed` (measured) |

`pending` holds at most one `AgentStatusChanged` per pane. A `Resync` absorbs the queued `AgentStatusChanged` markers for its space. `PaneAppeared`, `PaneClosed`, and `TabClosed` keep their order. So the queue is bounded by the number of blocks on the server. The bridge flushes only for spaces with an activated worktree, which a read-only store query lists.

`AgentDetected` is never produced in steps 4 and 5. On Herdr it differs from a status change only by opening the tour popup, and Rex popups arrive in step 6.

### Interface depth

The adapter's surface is `connect`, `RexClaim`, `RexCaller`, `locate_caller`, `bind_session`, and `RexEvents`, all `pub(crate)`. Only `mux::connect`, `mux::home`, and `rex_bridge` call them, so the rest of the crate reaches Rex through the `Mux` traits and `Home`. Behind it sit a WebSocket client, the `seq` ordering rule, the stale-record rule, incarnations, the binding files, server discovery with an ancestry proof, and the event decode table. No Rex wire type crosses that surface. `MuxEvent`, `SpaceId`, `PaneId`, `AgentSessionId`, and `Incarnation` are domain types the core already has.

The core sees three additions. `handle_mux_event` is a public entry over the private `handle_event`. It opens the store and builds the attribution itself, as `execute_lifecycle_operation` does, because `handle_event` (`src/lifecycle.rs:813`) also takes those two. `MuxEvent::Resync` adds one handler that reuses `reconcile_worktree`. `Marker` names the status queue's four input shapes, three of which exist today as untyped JSON. The session model in `model.rs` does not move, and `resolve_session`, `locate_agent_session_pane`, the popup target, and `candidate_reach` do not change.

## Synthesis decision

The design came from an arena of two candidates, each told to take a different whole shape. Candidate 1 (Opus) put the binding, the transport, and its own scheduler inside a Rex-only adapter and bridge. Candidate 2 (Sonnet) kept the adapter thin, moved the binding into the core through a new `SessionClaim::Bound` variant and a decorator, and made the bridge a translator into the existing status queue. A Sonnet cross-judge scored both (see [Cross-judge verdict](#cross-judge-verdict)).

Candidate 1 is the base. Its binding keeps the core's session model whole. The survey reports `Reported` exactly when Namedrop's own hook bound the live program, so no core reader of sessions learns that a session can come from two places. Candidate 2's `Bound` variant needed a decorator on every read path, a `same_program` comparison in the gate, and a change to `locate_agent_session_pane`, and it admitted that its binding's correctness spanned three modules.

From candidate 2 the design takes these parts.

- **One connection per port call.** Candidate 1 held one connection per `RexMux`, with a reader thread, a pending-request map, subscriber fan-out, and reopen logic. The measured 0.7 ms per call does not pay for that state.
- **The bridge as a translator into the existing per-space status queue.** Candidate 1 moved the worker's retry policy into a shared `drain_schedule.rs`, ran its own drains, and woke itself through Rex `client.perform`. It named "two event loops" as its own weakness. Reusing the worker avoids a second scheduler. It brings the worker's backoff, draft rechecks, `DrainReport` timers, and `failing.json` health reporting with it, and a wake is the existing `schedule_mailbox_workers` path. Drains already run from many callers (`drain_mailboxes` has ten call sites), and the pane lock, not `worker.lock`, keeps two of them from typing into one pane.
- **Only `claude-code` and `codex` are agents, and a block on another host is not.** Candidate 1 mapped any other `app` word to `AgentKind::Other`, which would make a `cargo` build an agent.
- **One module chooses the multiplexer.** Candidate 1 put the choice in `main.rs`, so the composition root read `REX_BLOCK` and called the adapter's binding. `mux::home` holds it now, with candidate 2's session-vouched fallback for a process whose ancestry does not reach the block.
- **The bridge flushes only for activated spaces**, so a `process_changed` in a space Namedrop does not manage costs no survey.
- **A prompt whose budget cannot cover the confirm window is declined before any byte.**

Both candidates agreed on the `rex:` endpoint string, the stale-record rule, the incarnation string, deleting `Admit`, keeping `Applied::Opened`, leaving the lock key alone, keeping live peek on Herdr, and leaving remote hosts out. That convergence is strong evidence for those parts.

The design also refines one thing neither candidate had. Candidate 1's tap reported every `process_changed` as a program change. Rex also sends that event whenever a client starts reading the terminal, so a dashboard opening a data connection after the paste would turn a delivered prompt into a held one. The tap reports `Program` only when the event's foreground pid differs from the one the facts saw.

On restart detection the design keeps candidate 1's answer over candidate 2's. Candidate 2 stored the server's `instance_id` and ran a restart reconcile when it changed. Candidate 1 runs the same idempotent `Resync` on every connect and lets the incarnation prove which programs were replaced. `observe_agent_attempt` (`src/delivery.rs:351`) holds any in-flight attempt whose pane no longer reports its session, before it looks at the restart flag, so the stored marker adds nothing.

## Tradeoffs accepted

- We accept session bindings in per-block JSON files outside SQLite, in exchange for no change to `resolve_session`, `locate_agent_session_pane`, the popup target, or `candidate_reach`. A store reset does not clear them. That is harmless, because a binding without a registration does nothing.
- We accept that the Rex adapter holds a path to Namedrop's state directory, which the Herdr adapter never needed, in exchange for keeping incarnations and bindings out of the core.
- We accept that an agent can register only after its first SessionStart or UserPromptSubmit hook, in exchange for a binding that only the agent's own descendant can write.
- We accept one Rex probe, a connection and two requests, on every Herdr command that inherits `REX_BLOCK`. On the measured machine that is every Herdr command. In exchange the innermost multiplexer is chosen correctly. The probe also shows on the Rex server as one short-lived client.
- We accept that `CONFIRM_WINDOW` holds the pane lock for up to 5 s on Rex, in exchange for a prompt that is either confirmed or held before the lock is released. A dashboard key during that time answers busy, as PORT.md accepted.
- We accept a hand-rolled WebSocket client of about 250 lines that skips the `Sec-WebSocket-Accept` check, in exchange for no new crate. It speaks only to a local Unix socket, and the check needs SHA-1, which no current dependency provides.
- We accept that `error` and unknown states never admit a prompt, although the protocol says to treat an unknown state as `idle`.
- We accept that badges, notifications, popups, keybinding reloads, and live peek are unsupported on Rex until step 6 or later.
- We accept a residual window of about a millisecond between the facts read and the paste. A person who submits a prompt by hand inside it would make the agent report `working`, and that change could confirm a mention typed into a busy agent. Closing it needs an atomic paste-if-idle from Rex.
- We accept two marker shapes in the status queue. `Marker::Herdr` is Herdr's raw status data, which the worker decodes late on purpose, and `Marker::Event` is Namedrop's own event. Moving Herdr's hook to decoded markers would change when a malformed Herdr event fails, so it is not part of these steps.

## Alternatives considered

- **The core owns the binding** (candidate 2's shape). A `SessionClaim::Bound` variant, made by a decorator around every Rex connection, resolves `Unreported` from a store. It keeps the adapter thin, but the gate then compares incarnations while the caller verified a session, every read path carries hidden state, and `locate_agent_session_pane` changes. More of the system learns about incarnations. It loses on depth.
- **A per-call `rex` CLI transport.** It is the simplest to write and it matches Herdr's process transport. It costs 19.5 to 26.9 ms per call (median), and a CLI tap has no `seq`, so the gate's order cannot be proven (measured). It cannot meet the gate contract.
- **A held control connection per `RexMux`.** It saves 0.7 ms per call. It costs a reader thread, a pending-request map, reconnect logic, a stale socket after a server restart, and a ping duty.
- **The bridge runs lifecycle and drain itself** (candidate 1's shape). It adds a second scheduler and a second drain caller per space beside `worker.lock`, and its wake needs a channel of its own.
- **Restart detection by a stored `instance_id`.** A crash can leave the stored value wrong, it needs a first-start case, and it would hold mail to programs that survived, if any could. The incarnation already proves which programs were replaced.
- **`tungstenite`.** It is complete and well tested, but it brings an HTTP stack and a random-number crate for one fixed local protocol. Switch to it if Rex ever needs TLS or mandatory compression.
- **Storing the binding in Rex.** Rex has no per-block key-value store, and a block label is visible and editable by people.

## Answers to the questions

### 1. `emulate_prompt`'s order holds on Rex

Rex's `program_status_changed` events reach a subscriber in the order the gate needs, when the tap, the facts, the paste, and the Enter share one control connection. `gate_probe.py` ran that order against live agents.

- **Order.** The tap is `session.attach`. The facts are `block.method program_status` and `process`. The paste and the Enter are two `block.event` requests, sent one after the other with no sleep. Their acks came 0.2 to 0.7 ms apart (measured). Every facts response carried `seq 1`, which was the probe's own `client_connected`, and every `working` event carried `seq 2` or `3` (measured). So a `seq` above the facts' `seq` is the boundary the tap measures from.
- **Latency.** Claude Code reported root `working` 17.3 to 140.7 ms after the Enter ack, median 21.6 ms, in 9 of 9 runs. Codex took 18.8 to 403.1 ms, median 25.2 ms, in 5 of 5 runs (measured). The latency is the agent's own time to submit the prompt and emit OSC 7501, not Rex's, because the request round trips were under a millisecond. Codex's 403 ms was its first prompt after start. `CONFIRM_WINDOW` stays 5 s, about 12 times the slowest run.
- **Busy agent.** Codex reported `working` for 33 s on a story. A second gate run during that time declined at the facts, and the word it would have sent never appeared on Codex's screen (measured).
- **Blocked agent.** Claude Code at a permission prompt reported `{state: blocked, kind: permission, msg: "approve Bash: touch blocked-probe.txt"}`. The gate declined at the facts. Escape returned it to `idle` (measured).
- **Tool calls.** While Claude Code ran `sleep 12` and Codex ran `sleep 20` as tool calls, every 0.5 s sample showed `process.foreground.pid` equal to the root record's `owner.pid` (measured). The stale-record rule does not flap during a turn.
- **Stale record.** The rule in `pane_agent` treats such a block as a free terminal, so the survey shows no agent, `candidate_reach` answers that the recipient is gone, and the gate would decline even if reached.
- **Between turns.** Claude Code reported `done` 0.73 to 1.01 s after the Enter for each one-word reply, and 13.5 s after it for the `sleep 12` turn (measured). It never stayed `working` after a turn, so PORT.md's accepted cost, mail waiting for `idle` after a between-turns `working`, did not show up on Rex.
- **Confirmation stays in the gate.** PORT.md asked whether confirmation should move to `observe_agent_attempt`. It should not. A prompt the tap cannot confirm must be held before the lock is released, and the measured confirmations release the lock in well under a second.

### 2. The incarnation is a pid and a local start time

Rex reports no start time. `process` returns `child` and `foreground` with `argv0`, `cwd`, `invoked_path`, `name`, `path`, `pid`, `script`, `user`, and `user_id`, and status records carry `owner {pid, name}` (measured). Claude Code's process name is its version, `2.1.323`, so a name does not identify a program across upgrades.

The incarnation is `<pid>@<start>`, for the block's `process.foreground` pid. Namedrop reads the start locally. On macOS, `proc_pidinfo(PROC_PIDTBSDINFO)` gives `pbi_start_tvsec` and `pbi_start_tvusec`, which were stable across reads (measured, `93037@1794528253006243` for the Claude Code agent). `src/terminal.rs` already calls `proc_pidinfo` with `PROC_PIDTBSDINFO` for `parent_pid`, so `process_start` sits beside it. On Linux the source is field 22 of `/proc/<pid>/stat` (inferred, not run).

When the start cannot be read, there is no incarnation and no fallback. `bind_session` refuses to write, and the survey reports `SessionClaim::None`, so the agent is visible but cannot register, and the doctor can say why. A pid-only fallback would let a reused pid inherit a session. The realistic causes are a process of another user, a process that exited between reads, and a block on another host. The adapter checks the last one first. A block whose `host_id` differs from the server's `host_id` is not local, and its pid names nothing on this machine. On the measured server, every block's `host_id` equalled `server.status.host_id` (measured).

### 3. The session record is a per-block file the hook writes

A Claude Code SessionStart hook ran as a child of the `claude` process. That pid was the block's `process.foreground.pid` and the status record's `owner.pid`. The hook's environment had `REX_BLOCK`, `REX_SESSION`, `REX_SERVER`, `TERM_PROGRAM=rex`, and `CLAUDE_CODE_SESSION_ID`, and its stdin had `session_id` and `source: "startup"` (measured). Namedrop's `execute_session_start_hook` (`src/hook.rs:42`) writes nothing today.

- **Shape.** `{block, incarnation, session, bound_at_ms}` as JSON, at `<state>/rex/sessions/<block uuid>.json`.
- **Writer.** Only `bind_session`, from `run_context_hook` for SessionStart and UserPromptSubmit. It first proves that the block's foreground program is one of the hook's ancestors, then reads that program's incarnation. It writes through a temp file and a rename, and it skips an unchanged record, so a repeat is harmless and a crash leaves the old record or the new one. Binding on UserPromptSubmit too covers an agent that started before Namedrop was installed and a SessionStart hook that failed.
- **Replacement.** `/clear` and an in-place resume run SessionStart with a new `session_id` in the same process (inferred from Claude Code's hook contract, not run on Rex), so the incarnation stays and the session changes. A new program has a new incarnation. Until its first hook runs, the old binding does not match, so the survey reports `Unreported`, and the old session is gone from the block. That is PORT.md's "a replaced program loses its binding" contract.
- **Deletion.** `RexEvents` calls `Bindings::forget` when it decodes `block_closed`, before the bridge enqueues `PaneClosed`. A file left behind while no bridge ran is harmless, because block IDs are never reused and its incarnation can never match again.
- **The producer of `Unreported`.** The Rex survey, through `pane_agent`, for a live agent whose incarnation matches no binding.
- **Resolution.** It happens in the adapter. The survey reports `Reported(session)` exactly when the binding's incarnation is the live foreground's. `resolve_session` keeps its pure arms. `locate_agent_session_pane` (`src/lifecycle.rs:1837`) and the popup target match `Reported` directly, and that is now right for Rex too, so neither changes.

### 4. The budget is 5 s per request and 4 MiB per call

- **Timeout.** Each request is held to the lower of the budget's timeout and `NAMEDROP_REX_TIMEOUT_MS`, which defaults to 5 s and is capped at 60 s, mirroring `NAMEDROP_HERDR_TIMEOUT_MS`. Opening the connection, from `connect` to the welcome, counts as one request. Requests measured 0.1 ms on an open connection and 0.8 ms with the open (measured), so 5 s catches only a wedged server. The `rex` CLI's own default is 10 s (`rex --help`).
- **Output cap.** The adapter caps every port call at 4 MiB of received bytes, as Herdr's process transport caps its timeout. `Budget::ADAPTER_DEFAULT` itself stays `Duration::MAX` and `usize::MAX`, so on Rex it means 5 s per request and 4 MiB per call. The server's own message limit is 1 MiB (the welcome's `limits.max_message_bytes`, measured), and a response larger than either limit is `MuxError::TooLarge`. Every measured `program_status` and `process` reply was well under 1 KiB (measured), so 4 MiB covers a 100-block survey with room to spare (inferred).
- **Screen reads.** `format` returns the whole scrollback, up to about 64 MiB per terminal, and has no line-count argument (measured). `screen` therefore calls `format` with a small `max_bytes` to learn the result's `id` and `total`, then `blob_read` with an `offset` near `total` for the tail, widening backward in 960 KiB windows until it has the requested lines or reaches the cap.
- **The confirm window.** A prompt needs up to 5 s after the paste. `deliver` declines a prompt with `Declined(AfterDelay)` before any byte when the budget's timeout is shorter than `CONFIRM_WINDOW` plus 1 s, so the gate never outlives its caller's budget.
- **`within`.** It returns a `RexMux` over `Within { wire: &dyn Wire, allowance }`, as Herdr's does. The allowance is asked before every request, its error is the read's error, and no request is sent after it refuses. `Budget::tighter` and `narrowest` move from `herdr::transport` to `mux::model`, so one copy serves both adapters. Cancellation is checked before each request and every 25 ms while a response is awaited.

### 5. Namedrop speaks `rex.control.v1` directly, with no new crate

The Rex server answers HTTP on its Unix socket and documents its own API at `GET /llms.txt` (measured). The control connection is a WebSocket at `GET /api/control` with subprotocol `rex.control.v1`. Each message is one JSON object in one text frame. The client sends `hello` within 5 s and gets `welcome` with the API revision and the message limit. Requests carry an `id`, responses carry the same `id` and a `seq`, and events carry a gap-free `seq`. `block.event` requests run in arrival order per connection, and block methods run concurrently (all from the server's documentation, the order measured).

- **Cost.** 30 calls each, with a load average near 7 (measured).

	| Path | Median | p90 |
	|---|---|---|
	| `rex version`, the process start alone | 16.0 to 22.9 ms | 17.4 to 26.0 ms |
	| `rex block call program_status` | 19.5 to 26.9 ms | 20.9 to 30.7 ms |
	| Fresh WebSocket: connect, hello, `session.attach`, `block.method` | 0.8 ms | 1.1 ms |
	| `block.method` on a held connection | 0.1 ms | 0.2 ms |

	The `rex` process start, a 42 MB Go binary, explains almost all of the CLI cost. Every call checked its reply for an error, and the two runs were sequential, not interleaved. The roughly 25-fold gap is far larger than the run-to-run spread. The direct numbers come from a Python client, so a Rust client should be no slower (inferred). A survey of a 10-block session needs about 22 requests, which would be about half a second through the CLI and a few milliseconds over one connection (inferred from the medians).
- **The client.** A stdlib-only Python client of about 120 lines did the upgrade, masked text frames, ping to pong, and every request in these probes (`rexws.py`, measured). The Rust `ws.rs` is the same on `UnixStream`, about 250 lines (inferred). Masking keys come from `libc::getentropy`.
- **Ownership.** A port call opens its connection inside the call and drops it at the end. The bridge holds one connection for its life.
- **Tests.** `rex/tests.rs` scripts `Wire` and `Control`. The script records each method with a short payload summary, answers from a closure, and pushes events with chosen `seq` values. `ws.rs` and `control.rs` get their own tests over `UnixStream::pair` and a scripted server thread on a temp socket.

### 6. The four Herdr-shaped leftovers

- **`Applied::Opened(serde_json::Value)` stays.** Its readers print the multiplexer's reply unchanged, and a typed reply would change what `namedrop ui` prints on Herdr. Rex answers `Applied::Unsupported` for `OpenPopup` in steps 4 and 5, which `open_popup` already prints as `null`. Step 6 returns `Opened` with the `session.new_layer` reply.
- **`Admit` is deleted**, with `PromptRequest.admit` and its three setters at `src/delivery.rs:774`, `src/integration.rs:736`, and `src/health.rs:767`. Nothing reads it. Herdr types to a `working` agent whatever it says, and the gate declines a `working` agent by status.
- **The pane lock key stays** `sha256(json [session_socket_id, pane])`. A Rex `session_socket_id` starts with `rex:` and a Herdr one never does, so the two kinds cannot share a lock file. Adding the kind to the key would rename every Herdr lock file, so an old and a new Namedrop could lock different files during an upgrade.
- **Live peek stays Herdr-only.** `data.connect` works. It gives a ticket, the data WebSocket answers HELLO with WELCOME, and the stream starts with a binary Ghostty snapshot (`GHOSTSNP`) followed by raw VT output (measured). Rendering that needs a terminal emulator. The live-peek entry answers "Live peek is not available on Rex yet" for a Rex binding. The dashboard's history view keeps working, because it reads `screen` through the port.

The effects map as follows. `Identify` is `session.set_block_label "@alias"`. `RenameTab` is `session.set_window_label`. `Badges`, `Notify`, `OpenPopup`, and `ReloadKeybindings` are `Unsupported`.

### 7. The innermost verified multiplexer wins

Rex passes the other multiplexer's variables through (measured). A scratch Rex server started with `HERDR_PANE_ID` and `HERDR_SOCKET_PATH` set passed both into its blocks unchanged, and it passed through an inherited `REX_SERVER` that named the default server, not itself. On the measured machine the everyday Herdr runs inside a Rex block. The process chain is the Rex server, `/usr/bin/login`, `-zsh`, the `herdr` client, the `herdr` server, and then each pane's shell (measured with `ps`). macOS `ps` cannot show another process's environment, so whether Herdr passes the outer `REX_BLOCK` into its panes was not seen. Herdr has no reason to remove it (inferred), and if it does not, a Herdr pane verifies both an inherited `REX_BLOCK` claim and its own `HERDR_PANE_ID` claim.

`mux::home::resolve` applies the rule, so the runtime builder and the context hook share it. The rule has three parts.

1. An explicit flag decides. That is step 3's `--endpoint`, or today's `--herdr-socket` and `--herdr-pane`. `scripts/run.sh` already passes `--herdr-pane` for Herdr actions and popups.
2. Otherwise each inherited claim is verified by ancestry. Herdr's anchor is the pane's `shell_pid`, as `verify_inherited_pane` already checks. Rex's anchor is the block's `process.child`. The claim whose anchor is the nearest ancestor wins (`mux::innermost`). A Rex claim whose ancestry fails still verifies when the agent's session variable (`CLAUDE_CODE_SESSION_ID` or `CODEX_SESSION_ID`) equals the block's live binding, the same second proof `verify_inherited_pane` accepts for Herdr.
3. With neither verified, today's Herdr body runs with its own errors. A Rex failure never becomes a new Herdr error.

How the cases come out:

- Herdr inside Rex, the measured setup, verifies both claims. The Herdr pane shell is nearer, so Herdr wins, and the runtime is today's.
- Rex inside Herdr, a Rex server started in a Herdr pane, makes the Rex block nearer, so Rex wins. Today that process would act as the Herdr pane that runs the server, which is an identity error (inferred).
- A stale `HERDR_PANE_ID` inside a Rex block fails Herdr's ancestry check, so Rex wins.
- A stale `REX_SERVER` is handled by `locate_caller`, which tries `REX_SERVER`, then the default socket (`~/Library/Application Support/rex/server.sock` on macOS), and skips a server that does not know the block.

**What steps 4 and 5 need from step 3.** `NAMEDROP_ENDPOINT` and `--endpoint` for the endpoint string, `NAMEDROP_SPACE` and `--space`, `NAMEDROP_PANE` and `--pane`, and the neutral state directory, each with a fallback to its `HERDR_*` name. Four things must hold.

- `--endpoint` goes through `Endpoint::parse`, never straight to Herdr.
- `--space` and `--pane` with a Rex endpoint are trusted, as `--herdr-pane` is today.
- `tool_environment` writes the `HERDR_*` keys only for a Herdr endpoint, so an older reader never runs `herdr` against a `rex:` string.
- The status worker reads its state directory, endpoint, and space from the neutral names, because the bridge spawns it with no `HERDR_*` variables.

**How the kind travels.** The `rex:` prefix is part of the stored string. As the `session_socket_id` it scopes the store namespace, the status queue directory, and the pane lock. As `NAMEDROP_ENDPOINT` in the collab/v1 envelope, `located_command` turns it back into `--endpoint`. `mux::connect` and `mux::Connector` parse it to pick the adapter. `Endpoint::parse` is the only place that reads the kind.

### 8. One bridge per server feeds the existing worker

- **One process per server.** `namedrop rex bridge --endpoint rex:/…` holds `try_lock` on `<state>/rex-bridge/<sha256(endpoint)>.lock` for its whole life, so a second start exits 0.
- **The stream.** One connection with `session.attach_all {event_scope: "all"}`. On the scratch server that delivered `program_status_changed` (root `id` is `""`), `process_changed`, `child_exited`, `block_created`, `block_closed`, and `session_destroyed` (measured). The decode table in [the bridge section](#the-bridge-the-queue-and-the-worker) drops the noisy events early. Codex sent 29 `title_changed` events in one 3 s turn, and the `idle` event that ended a 33 s turn carried `seq 338` (measured). Every `rex` call or port connection also shows up as a `client_connected` event.
- **How it calls lifecycle and drain.** It does not. It enqueues `Marker::Event` into the per-space status queue that the Herdr status hook already uses, `<state>/agent-status-reconciliation/<sha256(endpoint \0 space)>/`, and spawns the status worker for that space. The worker runs `handle_mux_event`, then drains, with its existing `worker.lock`, backoff, draft rechecks, `DrainReport` timers, and `failing.json`. A CLI wake reaches it the same way, through `schedule_mailbox_workers`. The bridge takes the shared command gate only while it enqueues, and calls `runtime_reexec_if_replaced` each time, so an update between flushes hands the stream to the new runtime.
- **Reconnect and reconcile.** When the stream ends, the bridge reconnects with backoff and queues `Resync` for every activated space on the endpoint. The handler runs `reconcile_worktree(.., after_restart = false)`, so a mere reconnect holds no mail. Events lost during the gap are covered by the resync.
- **Restart detection.** A Rex server restart ended the control connection with no close frame, killed each block's program, and on restart the same block IDs answered with a fresh `login` child, a new pid, and no status records. `instance_id` changed and `host_id` stayed (measured). So every program has a new incarnation, the survey reports no session for any of them, and `observe_agent_attempt` holds each in-flight attempt through its `!same_session` arm, which runs before the restart flag is read (`src/delivery.rs:351`). The same `Resync` that serves a reconnect serves a restart, and it converges however many times it runs. The bridge logs `instance_id` for the doctor and decides nothing from it.
- **Starting with no plugin host.** `rex_bridge::ensure` is a `try_lock` probe and a detached spawn with `process_group(0)` and null stdio, like `deferred_commands::ensure_worker`. The Rex runtime builder calls it, so any Namedrop command or hook in a Rex block starts the bridge. Step 6's Lua module adds a top-level `rex.exec` that calls it. On the scratch server a top-level `rex.exec` in `init.lua` ran at every server start and every `rex config reload`, and a child it backgrounded survived, reparented to launchd (measured). So the bridge also comes back with the server.
- **Stopping.** SIGTERM or SIGINT stops it between flushes. A server that stays unreachable for 120 s ends it with exit 0, and the next `ensure` or server start brings it back. A replaced runtime re-execs between flushes. The bridge keeps no state that a `kill -9` could corrupt. The lock dies with it, and every marker it writes is idempotent.
- **Liveness.** The bridge pings after 120 s of silence and ends the stream after 60 s without a pong, matching the server's own schedule.

### 9. Remote Rex hosts are out of scope for the first release

This machine has no Rex hosts (`rex hosts` printed "No hosts"), so remote addressing could not be tested. Three facts put it out of scope. The incarnation needs a start time read on the machine that runs the program. Rex ignores `REX_SESSION` and `REX_BLOCK` under `-S`. The hooks of a remote agent run on the remote machine with its own state directory. Each machine should run its own Namedrop against its own local server. `Endpoint::parse` accepts only `rex:` with an absolute path, and `locate_caller` accepts only a `unix://` `REX_SERVER`, so any other address fails with "remote Rex servers are not supported yet". A block on another host inside a local session reads as unlocal, as answer 2 describes.

## Implementation plan

Each commit compiles, passes `cargo fmt --check`, `cargo clippy --locked --all-targets -D warnings`, and `cargo test --locked --lib`, and ends in a state you can check. Run `/verify` on real Herdr panes before commits 9, 10, 13, and 14, which touch shared paths. Live Rex checks run on a scratch server with its own socket, state directory, and empty `init.lua`, started under `env -i` as in the spike, never on the user's server.

### Step 4. The adapter

1. **Delete `Admit`.** Remove `PromptRequest.admit` and its three setters. Rename the gate test `a_between_turns_admission_is_declined_before_any_input` to cover a `working` agent. Done when `grep -rnw Admit src` finds nothing. Tests are the existing gate and delivery suites.
2. **Move the budget helpers.** `tighter` and `narrowest` move from the inline `mod transport` in `src/mux/herdr.rs` to `mux::model`. Done when the Herdr process tests pass unchanged.
3. **`TapChange` in the gate.** `FakeMux`'s tap wraps its states in `Status`. New gate tests cover a program change after the facts (declined, nothing pasted) and after the paste (`Ambiguous` before the window ends, under a clock bound of 1 s). Done when the old and new gate tests pass.
4. **`rex/ws.rs`.** Done when tests over `UnixStream::pair` pass for the upgrade, masking, a 70 KiB message with a 64-bit length, a fragmented message, ping to pong, close, and an oversize frame. The live check reads a `welcome` from the scratch server.
5. **`rex/control.rs`.** Done when tests against a scripted server thread on a temp socket pass for hello and welcome, request ids, an event buffered while a request waits, a timeout, cancellation within 50 ms, the 4 MiB byte count, and a server that closes mid-request.
6. **Reads.** `rex/wire.rs`, `rex/agent.rs`, the read side of `rex/binding.rs`, `terminal::process_start`, and `RexMux`'s `MuxRead`. `rex/tests.rs` pins the requests for each `Scope` and `Depth` pair (the table below) and checks `pane_agent` row by row. The rows include the stale record (owner 41 dead, foreground zsh 52, so no agent), a pid reused under another name, a binding to an older incarnation (`Unreported`), an unreadable start (`SessionClaim::None`), a `cargo` record (free terminal), and a block on another host. `screen` tests cover a tail across two `blob_read` windows and the cap. The live check surveys the scratch session. It shows the agent, and its incarnation's start matches `ps -o lstart` to the second.
7. **Input and effects.** `rex/channel.rs`, `Line`, `Keys`, and `apply`. Tests pin the order (`target.resolve`, `session.attach`, `program_status`, `process`, paste, Enter), skip an event at or below the facts' `seq`, decline on one above it, deliver on `working` after the Enter, and treat a paste error as `Ambiguous`. A `process_changed` after the paste with a new pid is `Ambiguous` at once, and one with the same pid is ignored. Further tests pin each key payload, `set_block_label` for `Identify`, `set_window_label` for `RenameTab`, `Unsupported` for the rest, and `Declined(AfterDelay)` for a prompt with a budget under 6 s. The live check captures the key payloads for Escape, Up, and Down before the tests pin them, because only Enter, Ctrl-C, and Ctrl-U were measured.
8. **Rex conformance.** `rex/conformance.rs` renders FakeMux's world as Rex replies and runs the multiplexer-neutral part of the shared suite (survey shape, `Gone` mapping, input outcomes) against `RexMux`. Done when the same assertions pass for both.
9. **One connector.** `Endpoint::parse`, `Endpoint::unix_socket`, `mux::connect`, and `mux::Connector`. The deferred worker's probe and connect, the reminder clock, and the dashboard move to them. Live peek refuses Rex. `tool_environment` writes `HERDR_*` only for Herdr. A test pins `""`, `"default"`, and `"/tmp/h.sock"` as Herdr and `"rex:/tmp/r.sock"` as Rex, and asserts that their lock keys differ. Done when every test passes and `grep -rn "herdr::connect(Some" src` finds only `src/mux.rs`.
10. **The caller.** `rex/caller.rs`, `mux::innermost`, `mux::home`, and the Rex runtime. The runtime builder and the context hook call `mux::home::resolve`. A table test covers `innermost` for Herdr in Rex, Rex in Herdr, a stale `HERDR_PANE_ID`, and a stale `REX_BLOCK`. A scripted-server test covers a `REX_SERVER` that does not own the block, so the default socket wins. Done when a CLI inside a Rex block resolves its block, and a Herdr pane inside Rex still resolves Herdr. Run `/verify` here, because every Herdr command on the measured machine takes the new path.
11. **The binding from hooks.** `bind_session` and the call in `run_context_hook`. Tests cover a hook under a scripted foreground ancestor (binds), an identical second call (no rewrite, same mtime), `/clear` with a new session (replaced), a caller whose foreground is not an ancestor (refused), and a Herdr choice (no file). The live check binds Claude Code and Codex, each in a scratch block. Codex's hook ancestry is unmeasured, and if its hooks do not descend from the `codex` process, this commit stops and the design comes back for review.
12. **Live check for step 4.** On the scratch server, start a Claude Code block and a Codex block in one session. Name each through the CLI, mention one from the other, and see the reply. Then `kill -9` the Claude Code process, queue a mention to it, and confirm that nothing is typed into zsh. Record the evidence in PLAN.md and PORT.md. This meets PLAN's step 4 done-when.

The survey requests that commit 6 pins:

| Port call | Rex requests on one connection |
|---|---|
| `survey(All, Spaces)` | `session.list`. Space IDs are session IDs, and `number` is the list position |
| `survey(Space, Spaces)` | `session.list`, for the label |
| `survey(Space or PanesOf, Panes)` | `session.list` for `Space` only, `server.status` for `host_id`, `session.attach`, `session.list_blocks`, then `program_status` and `process` for each block whose `creator_name` is `com.superlogical.terminal` |
| `survey(All, Panes)` | `session.list`, `server.status`, then the row above for each session |
| `survey(CheckoutOf, _)` | None. `MuxError::Unavailable("a Rex session is not a checkout")` |
| `screen` | `target.resolve`, then `format` with a small `max_bytes`, then `blob_read` of the tail. `Style::Ansi` is `format: "vt"`, which keeps SGR styling (measured) |
| `process`, `tab_label` | `target.resolve`, then `process` or the window's label from `session.view` |

A Rex pane maps as follows. `id` and `terminal` are the block ID. `tab` is the window ID. `space` is the session ID. `cwd` is `process.foreground.cwd`. `title` is the block label. `idle_label` and `viewport_rows` are `None`. `worktree` is `rex-space:<session id>` with no checkout, which the core already supports for a Herdr space outside Git. `PaneProcess` is `shell_pid = child.pid`, `foreground_pids` = the members of the foreground's process group, read locally, and `foreground_group` = the foreground's pgid, read locally with `getpgid`, because Rex reports no pgid (measured). `tty` is `None`.

### Step 5. The bridge

13. **The status queue's `Marker`.** `Marker`, `enqueue`, and `spawn_worker` move from `main.rs` to `status_queue.rs`, and the worker dispatches on `Marker::parse`. `MuxEvent` gains serde. The worker reads its scope from step 3's neutral names. Done when the worker's tests pass unchanged and a `Marker` round-trip test passes for all four shapes.
14. **`handle_mux_event` and `MuxEvent::Resync`.** Attribution follows the endpoint kind. FakeMux tests check that a `Marker::Event(PaneClosed)` retires a name, that `Resync` keeps a `Submitted` attempt whose session still runs, holds one whose program was replaced, and completes reconciliation so a later drain runs. The Herdr `event.*` tests pass unchanged.
15. **`rex/events.rs`.** Scripted-wire tests cover each row of the decode table, using lines captured by the probes as fixtures. `title_changed` yields nothing, coalescing keeps one status change per pane, and a missing pong ends the stream.
16. **`src/rex_bridge.rs`, `namedrop rex bridge`, and `ensure`.** The loop takes a private stream trait, so a test can script it over FakeMux and a temp state directory. Tests check that a second `run` exits at once, that each connect queues one `Resync` per activated space, that a flush enqueues and spawns once per space, and that SIGTERM stops it between flushes. The Rex runtime builder calls `ensure`. Done when a scripted `block_closed` retires a name through the real worker.
17. **Live check for step 5.** On the scratch server, close a named block and see the name go with no manual command. Queue a mention to a working Codex and see it drain after `idle`. Restart the server and see in-flight mail to the replaced programs held, then bind a new agent in the same block and name it again. Record the evidence in PLAN.md. This meets PLAN's step 5 done-when.

## What only the operator can decide

- **Remote hosts.** This design leaves `rex -S` out of the first release. Only the operator can say whether cross-host mentions are needed soon.
- **Registration through hooks.** On Rex an agent can register only after a Namedrop hook ran in it, so an agent started before the install needs one prompt first. That is a product call.
- **The Rex app.** PLAN's open questions about restarting the app and seeing the actions in the palette need the app attached to the operator's real server, which no agent should drive.
- **Requests to Superlogical.** An atomic paste-if-idle would close the last race in answer 1, and a pgid in `process` would remove one local read.

A Herdr probe needs isolation that changing `HERDR_HOME` does not give. A `herdr` client started with only `HERDR_HOME` changed still attached to the default server at `~/.config/herdr/herdr.sock`. A future probe must set `HERDR_SOCKET_PATH` and a separate config as well, and check `herdr status server` from that environment before it starts a client.

## Open questions and risks

- Do Codex hooks run as descendants of the block's `codex` process, with `REX_*` in their environment? Only Claude Code was measured. If not, `bind_session` refuses and Codex cannot register on Rex. Commit 11 checks it first.
- Does Codex run SessionStart for a sub-session in the same process? If it does, the sub-session would take the block's binding. The hook could skip a SessionStart whose `source` marks a sub-session.
- Does `delivery.rs`'s prompt-box parser read Rex's `vt` output the way it reads Herdr's ANSI lines? Claude Code sometimes fills an empty box with a grey suggestion of the next prompt, which a plain-text read cannot tell from a draft. `vt` keeps the grey SGR (measured), but it may also carry cursor moves. Commit 7 pins a captured box with a suggestion and one with a typed draft.
- What is the default Rex socket on Linux, and does the `/proc` start time behave? Only macOS was measured.
- Rex is a preview. Should the adapter refuse a welcome whose `api_revision` is not 12 until a newer revision is checked? This design says yes, and the doctor reports it.

## Cross-judge verdict

A read-only Sonnet judge scored both candidates on six criteria and called it close. Candidate 1 scored 23 and candidate 2 scored 24.

| Criterion | Candidate 1 | Candidate 2 |
|---|---|---|
| Delivery safety | 4 | 4 |
| Interface depth and leakage | 3 | 4 |
| Fit with the existing code | 4 | 4 |
| Operational robustness | 3 | 4 |
| Testability and sequencing | 4 | 4 |
| Evidence use | 5 | 4 |

The judge recommended candidate 2 as the base, with candidate 1's `TapChange::Program`, its owner-name check, its ancestry-proven binding, and its conformance suite grafted in. Its case for candidate 2 rested on three weaknesses of candidate 1: Rex knowledge in `main.rs`, a second event loop, and a held connection with a reader thread. The synthesis removes all three by grafting candidate 2's parts into candidate 1, so the remaining difference is where the binding resolves. On that point this design disagrees with the judge. Candidate 2's core stays Rex-unaware only by name. Its gate compares incarnations, its core holds a `Bound` claim and a decorator on every read, and `locate_agent_session_pane` changes. Candidate 1 keeps every core reader of sessions as it is.

The judge also found these factual errors, and the design corrects each.

- Candidate 1 moved the deferred worker's connect but missed its four `UnixStream::connect(socket)` probes (`src/deferred_commands.rs:354`, 441, 468, 489), which cannot connect to a `rex:` string. They use `Endpoint::unix_socket` here.
- Candidate 1 called `handle_mux_event` a function "of the same shape" as `handle_event`, which also takes the store and the attribution.
- Candidate 1 wrote that `Budget::ADAPTER_DEFAULT` "means 5 s". The constant stays unbounded, and the adapter applies its own caps.
- Candidate 2 claimed the status worker is the only drain caller per space. `drain_mailboxes` has ten call sites, and the pane lock serializes typing.
- Candidate 2 called its restart reconcile idempotent. It holds every Dispatching and Submitted attempt, so a replay after new dispatches would hold fresh mail. This design has no restart reconcile.
- Candidate 2 named a `herdr/transport.rs` file. `tighter` and `narrowest` live in the inline `mod transport` in `src/mux/herdr.rs`.

## Evidence

The probe programs lived in the session scratch directory and are not kept. This section keeps the commands and the trimmed output.

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
     1.1 ms tap open, attach response seq 0
     1.7 ms facts response seq 1 root {"state": "done", "app": "claude-code", "owner": {"pid": 93037, "name": "2.1.323"}} foreground 93037 2.1.323
     2.2 ms paste ack seq 1 err None, enter ack seq 1 err None, paste->enter ack 0.5 ms
  +    58.2 ms seq=2 id=<root> state=working app=claude-code owner=93037/2.1.323
  CONFIRMED working 58.2 ms after the Enter ack
  +   936.0 ms seq=3 id=<root> state=done app=claude-code owner=93037/2.1.323
```

Codex, first prompt:

```text
     1.7 ms facts response seq 1 root {"state": "idle", "app": "codex", "owner": {"pid": 2336, "name": "codex"}} foreground 2336 codex
     2.5 ms paste ack seq 1 err None, enter ack seq 1 err None, paste->enter ack 0.7 ms
  +   403.1 ms seq=3 id=<root> state=working app=codex owner=2336/codex
  +  2696.9 ms seq=26 id=<root> state=idle app=codex owner=2336/codex
```

Confirm latencies, all runs, in ms: Claude Code 58.2, 22.4, 20.4, 17.3, 21.0, 19.8, 56.9, 140.7, 21.6. Codex 403.1, 20.3, 18.8, 25.2, 113.2.

Busy Codex and blocked Claude Code:

```text
     1.6 ms facts response seq 1 root {"state": "working", "app": "codex", ...}
     1.6 ms DECLINED: state 'working' cannot take a prompt
     1.9 ms facts response seq 1 root {"state": "blocked", "kind": "permission", "app": "claude-code", "msg": "approve Bash: touch blocked-probe.txt", ...}
     1.9 ms DECLINED: state 'blocked' cannot take a prompt
```

The key events that `block.event` accepted. Enter submitted a pasted shell line and every agent prompt. On a fresh zsh block, Ctrl-C stopped `sleep 30`, and Ctrl-U cleared a pasted line before Enter.

```json
{"type": "key", "payload": {"key": "enter", "mods": "", "consumed_mods": "", "text": "\r", "unshifted_codepoint": 13, "composing": false, "action": "press"}}
{"type": "key", "payload": {"key": "key_c", "mods": "ctrl", "consumed_mods": "", "text": "", "unshifted_codepoint": 99, "composing": false, "action": "press"}}
{"type": "key", "payload": {"key": "key_u", "mods": "ctrl", "consumed_mods": "", "text": "", "unshifted_codepoint": 117, "composing": false, "action": "press"}}
```

```text
➜  s4 sleep 30
^C
➜  s4 echo ctrl-u-cleared
ctrl-u-cleared
```

The same Ctrl-C did not stop `sleep` in the block where Claude Code had been killed with `kill -9`. That block was still on the alternate screen with the agent's keypad and cursor-key modes set, and `rex send-key ctrl+c` failed there too. A crashed agent leaves its terminal modes behind, so a terminal line or key sent to such a block may not act as it would in a clean shell. Herdr panes have the same exposure (inferred).

**The stale record.** Claude Code under zsh in a block, then `kill -9` of its pid:

```text
owner 70790
after kill -9:
{"state":"idle","app":"claude-code","updated_at":"2026-11-12T19:09:06.520073-05:00","owner":{"pid":70790,"name":"2.1.323"}}
{"foreground":{"pid":39372,"name":"zsh"}}
(5 s and one shell command later, the same record)
```

After a clean `/exit` the records were `[]` and the foreground was zsh.

**Start times** (`starttime.py`, `proc_pidinfo(PROC_PIDTBSDINFO)` through ctypes):

```text
58351 ppid=39372 start_us=1794528500677340 comm=2.1.323 lstart='Thu Nov 12 19:08:20 2026'
93037 ppid=1661 start_us=1794528253006243 comm=2.1.323 lstart='Thu Nov 12 19:04:13 2026'
99999 no such process ''
58351 ppid=39372 start_us=1794528500677340 comm=2.1.323 lstart='Thu Nov 12 19:08:20 2026'
```

**A SessionStart hook in a Rex block** (project `.claude/settings.json` running `hookprobe.py`):

```text
stdin_keys: cwd, hook_event_name, model, scratchpad_dir, session_id, source, transcript_path ; source: startup
env: TERM_PROGRAM=rex, REX_SESSION, REX_BLOCK, REX_SERVER, REX_BIN_DIR, CLAUDE_CODE_SESSION_ID
ancestors: 57472 Python (the hook) -> 57374 ~/.local/bin/claude -> 39372 zsh -> 1661 rex server
block process: {"child":39372,"fg":{"pid":57374,"name":"2.1.323"}}; status owner 57374
```

**Environment leaks.** A scratch server started with `HERDR_PANE_ID=outer-herdr-pane HERDR_SOCKET_PATH=/nonexistent/outer.sock rex server run --listen unix:///…/r.sock`:

```text
HERDR_PANE_ID=outer-herdr-pane
HERDR_SOCKET_PATH=/nonexistent/outer.sock
REX_BLOCK=block:01a1d23f-215a-7586-b3d0-6857a3ae043a
REX_SERVER=unix:///Users/me/Library/Application%20Support/rex/server.sock   (not the scratch server)
REX_SESSION=session:01a1d23f-215a-73fb-982e-5916a8f1eb48
TERM_PROGRAM=rex
```

The real Herdr's ancestry, from `ps -o pid=,ppid=,comm=`:

```text
3525  1661 /usr/bin/login      (1661 is the Rex server)
3526  3525 -zsh
3595  3526 herdr
3596  3595 ~/.local/bin/herdr
3598  3596 -zsh                 (one per Herdr pane)
```

**Restart** on the scratch server. Before, the block's child was zsh 14593. SIGTERM to the server:

```text
attach_all False sessions 1
+0.98s stream ended after 0 messages: ConnectionError: closed
14593 gone
{"child":{"pid":29283,"name":"login"}}
{"records":[]}
{"instance_id":"c1dee400-…","started_at":"2026-11-13T00:12:51.597962Z","host_id":"dd8fe7c3-…"}   (was f4790dfc-…, same host_id)
```

**Lifecycle events** on `attach_all {event_scope: "all"}` (`lifecycle_probe.py`):

```text
new: session_created {"session": {"session_id": "session:…", "label": "nd-s4-lifecycle"}}
new: block_event:process_changed {"block_id": "block:…", "payload": {"generation": 2, "foreground": {"pid": 81151, …}}}
split: block_created {"block_id": "block:…"}
split: block_event:process_changed {"payload": {"generation": 1, "foreground": null}}
split: block_event:process_changed {"payload": {"generation": 2, "foreground": {"pid": 81523, …}}}
exit-in-split: block_event:child_exited {"payload": {"exit_code": 0}}
exit-in-split: block_closed {"block_id": "block:…"}
kill: block_closed {"block_id": "block:…"}
kill: session_destroyed {"session_id": "session:…"}
```

**Starting the bridge from `init.lua`.** A scratch `init.lua` whose top level ran `rex.exec({"/bin/sh", "-c", "date >> …; ( exec /bin/sleep 287 ) > /dev/null 2>&1 < /dev/null &"})`. After a server start and after `rex config reload` the file gained a line, and `sleep 287` was alive with parent pid 1.

**Live peek.** `data.connect` with `data_ref_id: "terminal"` and `subprotocol: "rex.data.terminal.v1"`, then `GET /api/data`:

```text
ticket True 60000
upgrade HTTP/1.1 101 Switching Protocols
welcome 0x81 len 32
first stream bytes b'GHOSTSNP\x01\x00\x01\x00…'
```

### Reproduce the probes

Run these from a Rex terminal, in a folder that Claude Code and Codex trust. Write the stdlib WebSocket client first, as [Answer 5](#5-namedrop-speaks-rexcontrolv1-directly-with-no-new-crate) describes. The server's own `curl --unix-socket "$HOME/Library/Application Support/rex/server.sock" http://localhost/llms.txt` documents every message.

```sh
created=$(rex new nd-probe -c "$PWD" --json -- claude --model haiku)
S=$(printf '%s' "$created" | jq -er .session_id)
B=$(printf '%s' "$created" | jq -er '.initial_windows[0].block_ids[0]')
# On one control connection: session.attach {session_id: S},
# block.method {session_id: S, block_id: B, method: "program_status"}, then "process",
# block.event paste {text}, block.event key (the Enter payload above),
# then read events until a root program_status_changed reports "working".
rex block call -s "$S" -b "$B" program_status | jq -c '.records[] | {id, state, owner}'
rex kill "$S"
```
