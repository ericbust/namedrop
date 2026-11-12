# Terminal multiplexer port design

This document is the design for step 2 of the [Rex host plan](PLAN.md). It replaces `HerdrCommandExecutor` in `src/ports.rs`, which passes raw `herdr` arguments, with a port that speaks in panes, spaces, and prompts. A Herdr adapter implements the port in step 2. A Rex adapter implements it in step 4.

## Problem

Namedrop builds Herdr arguments and parses Herdr JSON inline in about 16 files. The dashboard creates its own `ProcessHerdr` and types into panes without the `PaneEffects` lock. Twenty-six test files fake Herdr by matching literal arguments. The design has to respect these constraints:

- **Herdr behavior does not change.** The `collab/v1` contract, the `HERDR_*` environment keys, the `--herdr-*` flags, stored `session_socket_id` values, and the Herdr commands that each discovery runs all stay the same. The one intended change is named under [Tradeoffs accepted](#tradeoffs-accepted).
- **"Host" is taken.** It means the agent CLI. The port is called `Mux`.
- **The two multiplexers deliver differently.** Herdr's `agent prompt` refuses a busy agent before it types anything, so a failure can be requeued. Rex accepts input at any time. Codex treats mid-turn input as a new instruction and drops its task. So for Rex, Namedrop must enforce the gate itself, and any failure after the first byte is ambiguous.
- **Rex reports no agent session ID,** and a Rex server restart keeps block IDs while it replaces the programs in them.

## Usage

```rust
// Discovery: one snapshot, then pure mapping.
let survey = mux.survey(Scope::Space(&space), Depth::Panes)?;
let (space, pane) = survey.pane_running(&session).ok_or(..)?;

// Delivery: one path for every multiplexer.
let effects = PaneEffects::acquire(state, mux.endpoint(), &pane.id, LockWait::Patient)?;
match effects.prompt(&mux, &owner, PromptRequest { expect, text, admit }) {
	InputOutcome::Delivered => submitted(),
	InputOutcome::Declined(retry) => requeue(retry),  // nothing was typed
	InputOutcome::Refused(_) => failed(),
	InputOutcome::Ambiguous(_) => held(),             // never sent again
}

// Dashboard keys: the same lock and the same door as delivery.
let effects = PaneEffects::acquire(state, mux.endpoint(), &pane, LockWait::Try)?;
effects.keys(&mux, &InputOwner::Dashboard, &[Key::CtrlC])?;

// Inbound: the Herdr hook now, the Rex bridge in step 5.
lifecycle::handle_event(runtime, &herdr::decode_plugin_event(op, hook_event, json)?)?;

// Tests: a world model, not argument strings.
let mux = FakeMux::world().space("workspace-1").pane("pane-author").claude().session("session-author").idle().build();
drain_mailboxes(&runtime_with(&mux))?;
assert_eq!(mux.sent_to("pane-reviewer").len(), 1);
```

## Shape

**Module map.**

| Path | Owns |
|---|---|
| `src/mux/model.rs` | Branded `PaneId`, `TabId`, and `TerminalId`. `Survey`, `SpaceView`, `PaneFacts`, `AgentState`, `AgentIdentity`, `SessionClaim`, screen and process types, `Key`, `Budget` |
| `src/mux/ports.rs` | The `MuxRead`, `MuxAct`, and `MuxInput` traits, `Mux` as their union, `MuxConnector`, and `MuxError` |
| `src/mux/effect.rs` | `Effect` and `Applied`: labels, badges, tab names, notifications, popups, and config reload |
| `src/mux/input.rs` | `InputKind`, `PromptRequest`, `Admit`, `InputOutcome`, and `Retry` |
| `src/mux/gate.rs` | The admission rule and `emulate_prompt`, the one implementation of "idle or done" and "ambiguous after the first byte" for multiplexers without a native gate |
| `src/mux/event.rs` | `MuxEvent`, the normalized lifecycle event |
| `src/mux/herdr/` | The only place that holds Herdr arguments, JSON, error codes, plugin events, and live-peek observation. Its transport is private to the module |
| `src/mux/fake.rs` | `FakeMux` and its world builder |
| `src/pane_effects.rs` | `PaneEffects` and `PaneInput`, which only this module can construct |

**Ports.** Ten methods across three traits hide the 22 Herdr operations.

```rust
pub trait MuxRead: Send + Sync {
	fn endpoint(&self) -> &Endpoint;
	fn survey(&self, scope: Scope<'_>, depth: Depth) -> Result<Survey, MuxError>;
	fn screen(&self, pane: &PaneId, request: ScreenRequest) -> Result<Screen, MuxError>;
	fn process(&self, pane: &PaneId) -> Result<PaneProcess, MuxError>;
	fn tab_label(&self, tab: &TabId) -> Result<Option<String>, MuxError>;
	fn integration_root(&self) -> Result<Option<PathBuf>, MuxError>;
}
pub trait MuxAct: MuxRead {
	fn apply(&self, effect: &Effect) -> Result<Applied, MuxError>;
}
pub trait MuxInput: MuxRead {
	fn deliver(&self, input: PaneInput<'_>) -> InputOutcome;
}
pub trait Mux: MuxAct + MuxInput {}
pub trait MuxConnector: Send + Sync {
	fn connect(&self, endpoint: &str, budget: Budget) -> Result<Arc<dyn Mux>, MuxError>;
}
```

Functions that only look, such as discovery, `verify_inherited_pane`, and health, take `&impl MuxRead`. The compiler then proves they cannot type into a pane. The budget (timeout and output cap) is fixed when a connection is made, so no method takes one and the `Deadline` and `BoundedReads` wrappers go away.

**Snapshots keep today's command counts.** Each `Scope` and `Depth` pair maps to exactly the Herdr commands that the code runs today. For example, `Scope::All` with `Depth::Panes` is one `pane list`, which serves `pane_workspace` and `locate_agent_session_pane`. Tests in `mux/herdr/` pin the arguments for every pair. The Herdr quirks stay in the adapter: `owned_worktree_index`, `not_git_worktree` becoming a space with no checkout, `workspace_not_found` becoming `MuxError::Gone`, and the `herdr-space:` synthetic worktree ID, byte for byte.

**Agent facts.** `PaneFacts.agent` is `Option<AgentIdentity { kind, label, session: SessionClaim }>`, and `PaneFacts.state` is `Option<AgentState>`. `agent` is `None` exactly when the multiplexer reports neither an agent kind nor a session, which is today's test for a free terminal pane. `AgentState` is `Idle | Working | Done | Blocked | Unknown(String)`, so an unknown Herdr status still prints the same text. `AgentState::accepts_prompt` replaces every `"idle" | "done"` literal. `SessionClaim` is `Reported(id)` for Herdr, `Unreported { incarnation }` for Rex, or `None`. One function, `resolve_session`, turns a claim into the `AgentSessionId` that `candidate_reach` compares. Step 2 ships only the `Reported` arm. Step 4 adds the record that Namedrop's SessionStart hook writes, keyed by the incarnation, so a replaced program loses its binding.

**Delivery.** The gate belongs to the port contract. `deliver` follows three rules for every adapter. It sends a prompt only if the pane is admitted at send time. It reports any failure after the first byte as `Ambiguous`. It lets no unclassified error escape.

- **Herdr enforces the gate itself.** The adapter maps Herdr's error codes exactly as `runtime.rs` does today.
- **Rex uses the shared driver.** The Rex adapter implements three small `gate::RawChannel` methods: `facts`, `tap`, and `paste_submit`. `gate::emulate_prompt` runs them in a fixed order. It opens a status tap, checks admission, declines if any status change has already arrived on the tap, pastes and presses Enter on one connection, then waits for a change to `Working` from the tap. The tap opens before the facts are read, so a change between the read and the paste declines the prompt, and a stale `working` state cannot confirm delivery. An adapter author cannot reorder these steps or skip the ambiguity rule, because the driver owns them.
- **Rex declines a between-turns prompt before it types.** On Herdr, Namedrop may deliver to a Claude Code session that reports `working` between turns. The driver declines that admission, because a later change to `Working` cannot confirm it. The message waits for `idle`, which is slower but never ambiguous.

`delivery.rs` keeps one `match` on `InputOutcome` and has no knowledge of the multiplexer. The readiness policy, the prompt-box draft check, and `clear_sent_prompt` stay in `delivery.rs`, because they are about agents rather than multiplexers.

**One door for input.** `PaneInput<'lock>` has private fields, a constructor private to `pane_effects.rs`, and a lifetime borrowed from the `PaneEffects` lock. `deliver` accepts nothing else. A `compile_fail` doctest replaces the source-grep test. The dashboard bypass closes because `ProcessHerdr` is no longer visible outside `mux/herdr/`. Dashboard code receives a `MuxConnector`, and it can type only through `PaneEffects`.

**Inbound events.** `MuxEvent` has the variants `PaneAppeared`, `PaneClosed`, `PaneMoved`, `TabClosed`, `AgentDetected`, `AgentStatusChanged`, and `Resync`. Each carries only IDs, because the existing handlers rediscover the space anyway. The `deny_unknown_fields` Herdr structs and the check that pairs a hook with its operation move into `herdr::decode_plugin_event`. The `event.*` operation names stay, because the plugin manifest calls them.

**Tests.** `FakeMux` is a world model of spaces, tabs, panes, screens, and processes. It records typed `Sent` and `Effect` logs, plays scripted outcomes, injects errors, and queues events. `FakeMux::emulated()` runs the real `gate::emulate_prompt`, so the Rex gate is tested before Rex exists. A conformance suite runs the same behavioral tests against `FakeMux` and against `HerdrMux` over a scripted transport, so the fake cannot drift from Herdr. The 21 tests that put a fake `herdr` script on `PATH` stay as they are and serve as the adapter's black-box tests.

## Synthesis decision

Two designs competed: one from Opus (A) and one from Sonnet (B). A Sonnet judge scored them on six criteria and recommended B, and I agree. Both converged on the name `Mux`, snapshot reads, a lock-bound input token, a four-way outcome, ID-only events, one fake, and a temporary bridge that the last commit deletes. That convergence is strong evidence for the shape.

B is the base for two reasons. Its shared `emulate_prompt` driver makes the Rex gate and the ambiguity rule code that is tested now, where A left them as a convention inside a future adapter. B also declines a between-turns prompt before it types, where A would type and then hold the message as ambiguous.

From A, I took these changes:

- **The budget moves to connect time.** This removes a parameter from every method and deletes the per-call `Budgeted` wrapper.
- **The `Capabilities` struct is gone.** B kept a hand-synced list of seven booleans. The only behavior that depended on it, between-turns admission, now lives inside the driver. A multiplexer that cannot show a notification returns `Applied::Unsupported`, and callers ignore it.
- **Argument-pinning tests and the conformance suite come from A.** They replace B's separate differential fake, because one suite over two implementations does the same job.
- **`agent: None` means a free terminal pane,** as in A.

I rejected B's `MuxEvents` trait for step 2. Its `observe` method exposed a raw file descriptor, and `subscribe` has no Herdr implementation. Live peek keeps its Herdr observer inside `mux/herdr/` until step 4 tests Rex's `data.connect`. The Rex event stream arrives with the bridge in step 5.

## Tradeoffs accepted

- We accept one behavior change in exchange for closing the bypass. Dashboard keys take the pane lock with `LockWait::Try` and record `InputOwner::Dashboard`. A key that collides with a delivery reports busy instead of landing in the middle of a prompt. This change gets its own commit so it can be reverted alone.
- We accept that on Rex, mail for a Claude Code session that is working between turns waits for `idle`, in exchange for never sending input that cannot be confirmed.
- We accept a closed `Effect` enum in exchange for exhaustive matches in every adapter. A new effect costs three edits, and the compiler lists them.
- We accept that `mux::ports` imports `PaneInput` from `pane_effects.rs`. Rust's module-private constructor is the only way to seal input at compile time.
- We accept that live peek stays Herdr-only in step 2, in exchange for no raw file descriptor on the port.
- We accept temporary duplicated parsing between the adapter commit and the commits that move callers, in exchange for every commit compiling and passing tests.

## Alternatives considered

- **One trait with a method per Herdr operation.** Callers would still combine `pane list`, `process-info`, and `agent prompt` themselves, and the gate would have no home. It loses on depth.
- **The gate in `delivery.rs` behind a capability check.** Delivery would branch on the multiplexer, and each new multiplexer would add a flag. It also cannot keep the check and the paste on one connection.
- **Typed helper functions over the existing trait.** The transport would stay public, and the dashboard bypass would stay one line away.
- **Enforce the input rule with a lint or a better grep.** Any new wrapper function routes around a text match. Types do not.

## Migration order

Each commit compiles and passes `cargo test`. Run `/verify` on real Herdr panes before commits 8, 11, 12, and 16.

1. **Model and contract.** Add `mux/{model,ports,effect,input,event,gate}.rs` and the branded IDs. Unit-test `AgentState` parsing, the admission rule, and `emulate_prompt` against a scripted `RawChannel`: a failure after the paste is ambiguous, a stale `working` cannot confirm, and an identity mismatch declines.
2. **`FakeMux` and its world builder.** Test the fake, including its emulated mode.
3. **`HerdrMux` and the legacy bridge.** Copy the argument builders, JSON parsing, error mapping, and event decoding into `mux/herdr/`. A temporary `impl<E: HerdrCommandExecutor> Mux for` the legacy executor makes every existing executor and fake a `Mux`. Add argument-pinning tests and the conformance suite. No caller changes.
4. **Rename `LifecycleRuntime.herdr` to `mux`** and change its bound to `M: Mux`.
5. **Discovery over `survey`.** Move `discover_*`, `pane_workspace`, `locate_agent_session_pane`, and `workspace_labels`. The existing tests pass through the bridge, which is the first proof that the adapter is faithful.
6. **The remaining reads.** Move `screen`, `process`, `tab_label`, and `integration_root`, and delete `Deadline` and `BoundedReads`.
7. **Typed status.** `DiscoveredPane` holds `AgentState` and `AgentIdentity`. Delete the `"idle" | "done"` literals. Add `SessionClaim` and `resolve_session` with the `Reported` arm only.
8. **Input.** Add `PaneInput` and `InputOutcome`, and make `delivery.rs` match on the outcome. Delete `PromptFailureKind`, `classify_prompt_failure`, and the grep test. Add the `compile_fail` doctest.
9. **Effects.** Metadata, badges, tab renames, notifications, popups, and config reload go through `apply(Effect)`.
10. **Events.** Add `MuxEvent` and `herdr::decode_plugin_event`. The handlers in `lifecycle.rs` take `&MuxEvent`.
11. **Dashboard connections.** `MuxConnector` replaces every inline `ProcessHerdr::default()`. Reads only, with no behavior change.
12. **Dashboard input through `PaneEffects`.** This is the one intended behavior change.
13. **Test wave A.** Move `tests/support::FakeHerdr` and the smallest test files to `FakeMux`.
14. **Test wave B.** Move the delivery, between-turns, handoff, final-reply, and terminal tests.
15. **Test wave C.** Move the remaining test files and the fakes inside `src/`. Tests that check Herdr wire behavior move to `mux/herdr/`.
16. **Delete.** Remove the bridge, `HerdrCommandExecutor`, `HerdrCommandOutput`, and the public `ProcessHerdr` export. Add a test that fails when a `herdr` argument or `HERDR_BIN_PATH` appears outside `src/mux/herdr/`, with an explicit allowlist for the `collab/v1` names that step 3 renames.

## Implementation reconciliation

Record each accepted deviation here, with its source, before the next commit starts.

**Commit 1** (`feat(mux): add the multiplexer port model and delivery gate`). The deviations below were accepted in review of that commit.

- **The tap opens before the facts are read.** The original order read the facts first, so a status change between the read and the paste went unseen. The driver now declines when the tap already holds a change. The [Delivery](#shape) text above is updated.
- **The status decides admission, not `Admit`.** A between-turns request reports `working`, so it is declined before any input. `Admit` stays on `PromptRequest` for the Herdr adapter, which honors between-turns delivery natively.
- **`PromptRequest.expect` is an `AgentIdentity`.** Admission compares its kind and its `SessionClaim`, so a replaced program, which has a new incarnation, is declined.
- **A failure before the paste is `Declined(Retry::AfterDelay)`, and any paste error is `Ambiguous`,** whatever error the adapter reports.
- **The module root is `src/mux.rs`,** beside `src/mux/`, which matches the rest of the crate.
- **`PaneInput` has no constructor yet.** An unused constructor fails `-D warnings`. Commit 8 adds it with the `compile_fail` doctest.
- **Deferred to the commits that use them:** `resolve_session`, the `Survey` lookup helpers, and `MuxEvent::space`.

**Commit 2** (`test(mux): add FakeMux and its world builder`). Each model change below is something today's Herdr discovery reports that the commit 1 types could not hold, so commit 5 could not rebuild `DiscoveredWorktree` without it.

- **`AgentKind::Unreported`.** Herdr can report a session with no agent kind. That pane is not free, so `agent` must be `Some`, and its kind needs a value.
- **`PaneFacts.idle_label`.** `DiscoveredPane.idle_state_label` (Herdr's `state_labels.idle`) decides whether a pane's identity metadata is republished.
- **`Checkout.repo_key` and `Checkout.repo_name`.** `workspace_labels` and the dashboard read them from the checkout Herdr stamps on a space.
- **`SpaceView.worktree: Option<WorktreeId>`.** The stored key, the checkout path or `herdr-space:<id>`, is chosen by the adapter so the synthetic key stays in `mux/herdr/`. It is `None` when a survey does not resolve it. `Scope` documents which facts each scope and depth read, because `Scope::All` with `Depth::Panes` is one `pane list` that names no labels or checkouts.
- **`PopupSpec.target` is a `PaneFacts`.** The Herdr popup command passes the target's space, terminal, and session, which a bare `PaneId` cannot supply without another `pane list`.
- **Reaching the fake from `tests/`.** `mux::fake` is a `#[doc(hidden)] pub mod`, always compiled, as `namedrop::mux::fake::FakeMux`. No feature flag.
- **Testing delivery before `PaneInput` exists.** `FakeMux::deliver` forwards to a crate-private `send(pane, kind)`, and the tests drive `send`. Commit 8 can test `deliver` itself.
- **The fake reads what Herdr reads.** A survey reports only the facts Herdr's commands for that scope and depth return. A checkout listing names no space label, number, or repository name, and a space listing names no checkout label or branch. A test then sees the same partial facts from either implementation, and the commit 3 conformance suite checks it.
- **Native gate.** By default the fake gates like Herdr: it declines a busy, blocked, or absent agent before typing, honors `Admit::BetweenTurns`, and checks no identity. `FakeMux::emulated()` runs `gate::emulate_prompt` instead. A prompted agent starts working unless `prompts_stay_settled()` is set.
- **Deferred:** the queued `MuxEvent`s that the design mentions. No port method consumes events until step 5.

**Commit 3** (`feat(mux): add the Herdr adapter and the legacy bridge`).

- **The survey pairs.** `Space`/`Panes` is `worktree list --workspace` and `pane list --workspace` (`discover_worktree_for_workspace`). `Space`/`Spaces` is `workspace get` (the dashboard label). `CheckoutOf`/`Panes` is `discover_worktree`: `worktree list --cwd`, `workspace list`, one `worktree list --workspace` per space without a stamped checkout, then `pane list --workspace`. `CheckoutOf`/`Spaces` is the same without the pane listing. `All`/`Spaces` is `workspace list` (`workspace_labels`). `All`/`Panes` is `pane list` (`pane_workspace`, `locate_agent_session_pane`). `herdr/tests.rs` pins each.
- **Lookup failures keep today's text.** A checkout Herdr does not resolve, does not have open, or has open in more than one space is `MuxError::Unavailable` with today's message, byte for byte. A listing without the requested space is `Protocol`. `pane_not_found` maps to `Gone(Pane)` beside `workspace_not_found`, and the raw error text is lost for both, so commits 5 and 6 must print their own text for `Gone`.
- **The bridge is a blanket `impl<E: HerdrCommandExecutor + Send + Sync>`** of `MuxRead`, `MuxAct`, and `MuxInput`. It keeps the executor's own `classify_prompt_failure`, because several test fakes override it. Executors that hold a `RefCell` are not `Sync` and are not bridged; commit 4 must change those fakes to a `Mutex` or migrate them.
- **The bridge's `endpoint()` has an empty key.** A legacy executor does not know its socket. Callers keep reading `session_socket_id` until commit 16, and commit 9 must not build the popup's `HERDR_SOCKET_PATH` from a bridged endpoint.
- **No per-call budgets.** The bridge calls `execute`, never `execute_bounded`, because no budget exists until a connector makes one. Commit 6 and commit 11 must connect with today's per-read limits (`plugin list` 2 s and 64 KiB, the dashboard reads).
- **`ProcessHerdr` is not copied.** It reaches the port through the bridge, so the process code is not duplicated. Commit 16 moves it into `mux/herdr/` as the private transport.
- **Not yet in the adapter:** `status server` (the socket fallback) and `terminal session observe` (live peek). Both move with their callers.

**Commit 4** (`refactor(mux): rename LifecycleRuntime.herdr to mux`).

- **The bound is `M: Mux + HerdrCommandExecutor`.** Callers that still build Herdr arguments call `execute` through the second bound. Each later commit drops `+ HerdrCommandExecutor` where its caller no longer needs it, and commit 16 deletes the rest.
- **Test fakes that held a `Cell` or `RefCell` hold `tests/support/sync_cell.rs` instead,** a `Mutex` with the same methods, so they are `Sync` and pass through the bridge. Their assertions are unchanged.

**Commit 5** (`refactor(mux): discover spaces and panes through survey`).

- **`MuxError` keeps the multiplexer's words.** `Gone`, `Timeout`, and `TooLarge` now carry the error text, and `Display` prints it unchanged. This reverses the commit 3 note that `Gone` loses its text: a discovery error still reaches the reminder clock, the status-event retry, and the user as Herdr's own JSON, byte for byte, and `tests/deferred_commands.rs` pins that. The conformance suite compares only the kind of a failure, because the fake and Herdr word it differently.
- **Only `discover_reminder_workspace` reads the error's type.** It fails a closed space's reminders on `Gone(Space)`. Every other discovery caller still receives a `String`, so `herdr_error_code` stays for the reminder clock until its callers take `MuxError`.
- **`DiscoveredPane.agent` still holds Herdr's word,** rebuilt from `AgentKind` in `lifecycle.rs`. Commit 7 deletes that mapping when `DiscoveredPane` holds an `AgentIdentity`.
- **Functions that only discover are bounded on `MuxRead`.** The agent status collector's `Deadline` wrapper reaches the port through the bridge, so its clock must be `Sync`, and it keeps today's per-read time and size limits.

**Commit 6** (`refactor(mux): move the remaining reads onto the port`).

- **The budget lives in the adapter, as a connection made with one.** `mux::herdr::bounded(executor, cancelled, budget)` returns a `MuxRead` whose every command runs through `execute_bounded` under the `Budget` that `budget` returns just before the command starts. It stands in for `MuxConnector::connect` until a connector exists, and it is deleted with the bridge in commit 16.
- **A budget is a function of time, not a fixed value.** The sketch fixes `Budget` at connect time. The agent status collector and the popup's identity recheck share one deadline across several commands, and each command gets what is left: the tests pin that the second discovery read gets less than the first, and that an excerpt read gets the collection budget less the 250 ms reserve on the injected clock. So `budget` is asked before each command, and its error is the read's error, word for word ("Reading this space took longer than its 2000 ms budget.", "confirming the invoking pane ran out of time"). The cancellation flag is fixed for the connection. A connector with a fixed `Budget` is still enough for the dashboard reads in commit 11.
- **What remains of `Deadline` is the collector's clock arithmetic.** `agent_status::Collection` holds the deadline and the reserve rule, and no longer wraps the executor. `BoundedReads` is gone outright.
- **Budgeted reads keep `HerdrCommandExecutor`.** `collect_agent_status`, `latest_identity`, and the health check's `plugin list` need `execute_bounded` until a connector supplies the budget. Every other reader moved here (`verify_inherited_pane`, `shell_is_available`, `execute_terminal`, `detect_pane_type`, the prompt box read, the context pane read, the tab label read, and the Codex in-process check) is bounded on `MuxRead` alone.
- **A process reply Herdr cannot parse still reads as "no shell".** `verify_inherited_pane` treats `MuxError::Protocol` as a missing shell, as the old parser's `None` did, so the two messages a person can see stay split exactly as before.
- **The dashboard reads stay inline.** `live_peek`, `http`, `hub`, and `skills_api` still call `execute_bounded` on their own `ProcessHerdr`, so `execute_bounded` and its `ProcessHerdr` tests stay until commit 11 moves them behind `MuxConnector`.

**Commit 7** (`refactor(mux): type agent status and identity on discovered panes`).

- **`SessionClaim` already existed.** Commit 1 added it with `AgentIdentity`, so this commit adds only `resolve_session`, a free function in `mux/model.rs` that returns the session of a `Reported` claim and `None` for the other two arms.
- **`DiscoveredPane` keeps no copy of the identity.** Its `agent_session_id` and `occupant_label` fields are gone. The methods `agent_session_id()` (through `resolve_session`), `occupant_label()`, and `accepts_prompt()` read them from `agent: Option<AgentIdentity>` and `agent_status: Option<AgentState>`. The field names `agent` and `agent_status` stay, so the diff stays at the readers.
- **A free terminal pane is `agent.is_none()`.** Every "no agent word and no session" check collapses to it, because the adapter returns `None` exactly when Herdr reports neither.
- **`AgentState` serializes as its word.** A hand-written `Serialize` prints `as_str()`, so the `agent_status` fields in the status, health, and handoff JSON are unchanged. `AgentWait::Status` holds an `AgentState`, and health matches `Blocked` and `Working` as variants.
- **The `"idle" | "done"` rule on stored statuses also goes through `accepts_prompt`.** The recipient outlook, persona readiness, and direct-message presence read `PaneAddressRecord.agent_status`, a stored string, so they call `AgentState::parse(status).accepts_prompt()`. The stored observation keeps Herdr's word.
- **The persona worker still needs a CLI name.** `naming_ui::persona_host` turns the identity into `claude`, `codex`, the reported word for any other agent, or an empty string for an unreported kind, which is what `agent.unwrap_or_default()` passed before. It is the one place outside the adapter that names an agent by word, and it names the CLI the worker runs, not Herdr's field.

**Commit 8** (`refactor(mux): deliver prompts through PaneInput`).

- **`Declined` keeps the multiplexer's words.** It is `Declined(Retry, String)`, like `Refused` and `Ambiguous`. Doctor's "Not sent, the agent did not accept it: {error}" prints Herdr's error text, and the interrupt warning prints whatever error the key press got, so a decline without its text would change what a person sees. The gate and the fake word their own declines. The conformance suite compares only the `Retry` of a decline, as it compares only the kind of a `MuxError`.
- **Every input `PaneEffects` sends goes through `deliver`.** Prompts, terminal lines, the Claude Code clear keys, and the interrupt's Ctrl+C are `InputKind`s, so no `agent prompt`, `send-keys`, or `pane run` argument is left outside `mux/herdr/` except the dashboard's live-peek input, which commit 12 moves. That input never matched the grep test, because it calls `send(&[..])` and not `.execute(&[..])`.
- **The legacy executor's classifier is `HerdrCommandExecutor::input_failure(String) -> InputOutcome`.** The bridge still needs a per-executor answer, because test fakes classify their own error strings. Its default is `Ambiguous`, as before. `ProcessHerdr` calls the adapter's one table, `mux::herdr::input_failure`, so the copy in `runtime.rs` is gone. It is deleted with `HerdrCommandExecutor` in commit 16.
- **A failed ownership record is `Ambiguous`.** Nothing was typed, but today's code reports it as an error that no classifier recognizes, so it is held. The text "could not record pane input ownership: …" is unchanged.
- **What the `compile_fail` doctests prove.** They compile as an outside crate, so they show that code outside the crate cannot fill in `PaneInput`'s fields or call its constructor. They fail if `PaneInput` and `PaneEffects` become public. Inside the crate, the private fields and the private `PaneInput::new` keep construction in `pane_effects.rs`, and the compiler enforces that. No doctest can test in-crate privacy. Stable rustdoc does not check error codes, so the doctests name none.
- **`PromptRequest` is filled honestly, though Herdr ignores it.** Delivery passes the pane's `AgentIdentity` from the second discovery and `Admit::BetweenTurns` when the pane does not report idle or done. Doctor and registration pass `Admit::Settled` and the identity they just checked.
- **Until commit 16, a raw `execute` can still type into a pane.** The grep test is deleted as planned. It caught only literal `.execute(&["agent", "prompt"` calls, and commit 16's test for Herdr arguments outside `mux/herdr/` replaces it.

**Commit 9** (`refactor(mux): apply metadata, notifications and popups as effects`).

- **`Applied::Opened(serde_json::Value)` carries the multiplexer's whole reply.** This answers the open question. Tests do read the popup JSON: `tests/tour.rs`, `tests/lifecycle.rs` (the tour report's `popup`), and `tests/naming_popup.rs` (the `ui.launch` data). The design expected a variant that carries the popup's pane, but a pane alone would change what `namedrop ui` prints, because real Herdr's reply holds more than a pane ID (`tests/update_lifecycle.py` answers `{"result":{"type":"plugin_pane_opened"}}`). `Applied` is no longer `Copy`. The fake answers `{"opened_for": <pane>}`.
- **An unreadable popup reply is `MuxError::Protocol` with serde's text.** The tour adds "Herdr returned an unreadable popup result: " to a `Protocol` error only, and `namedrop ui` prints the error unchanged, as before. A transport error never maps to `Protocol`, so the two messages stay split as before. A popup that returns `Done` or `Unsupported` reports `null`. Herdr returns neither.
- **The popup's socket comes from `mux::herdr::at_socket(executor, socket)`.** This bridge's endpoint holds the socket, so the commit 3 rule holds: no popup reads `HERDR_SOCKET_PATH` from the empty key of the plain bridge. `lifecycle::open_popup` (exported as `namedrop::open_popup`) is the one path, used by the tour and by `namedrop ui`. It stands in for a connector the same way `bounded` does, and commit 16 replaces it with `runtime.mux.apply`.
- **`tour::popup_open_args` is deleted.** The popup argv lives only in `mux/herdr/wire.rs`. The adapter test that compared the two copies is deleted with it. `each_effect_runs_today_s_command` and the black-box `tests/tour.rs`, `tests/lifecycle.rs`, and `tests/naming_popup.rs` still pin the argv byte for byte, the socket included.
- **`DiscoveredPane::facts()`** builds the `PopupSpec` target. `namedrop ui` keeps its space and pane IDs from the invoking runtime, as the old argv did.
- **`notification_shown` is true only for `Applied::Done`,** so a multiplexer that cannot notify reports `false`. Herdr's answer is unchanged.
- **Bounds.** A script removed all 76 `crate::mux::Mux + HerdrCommandExecutor` bounds, and the compiler named the 6 that still need the legacy trait. `agent_status::collect_agent_status` needs it for `bounded` reads. `health::diagnose_space` needs it for the `plugin list` read, and so do its callers `diagnose_collaboration` and `request_doctor_assistance`. `dashboard::open` needs it for its inline `workspace get`. `execute_lifecycle_operation` needs it only for the tour popup's `at_socket`. `activate_shortcuts` and `apply_managed_tab_label` take `&impl MuxAct`, and `tour::open_popup` and `show_for_first_agent` take `HerdrCommandExecutor + Sync`.

**Commit 10** (`refactor(mux): decode Herdr plugin events into MuxEvent`).

- **`decode_plugin_event` is split in two, and its name is gone.** `herdr::plugin_event_data` checks the envelope and pairs the hook with its operation, then returns the event's data unchanged. `herdr::decode_operation_input` turns that data into a `MuxEvent`. The status hook writes the raw data to its queue, and the worker decodes it later. A hook that decoded the data would reject a malformed event at once, but today that event is queued and the worker drops it on its `invalid operation input: ` prefix. The pane-lifecycle hook and `execute_lifecycle_operation` callers also pass raw data. So no caller wanted the one-step decode. `main.rs` now calls `plugin_event_data` in place of its own copy.
- **The data is decoded where it was parsed before.** `execute_lifecycle_operation` decodes after it discovers the invoking space and opens the store, as before. An event that is malformed and arrives while discovery fails still reports the discovery error. `lifecycle::handle_event` takes the store, the attribution, the runtime, and `&MuxEvent`, and it is private until the Rex bridge in step 5 calls it.
- **Each variant has its own handler.** These are `pane_appeared`, `pane_closed`, `tab_closed`, `pane_moved`, and `agent_status_changed`. Only `agent_status_changed` needs `HerdrCommandExecutor`, for the tour popup. The others take `M: Mux`. `execute_lifecycle_operation` keeps both bounds, so no bound could be dropped.
- **`MuxEvent::Resync` is deleted.** No Herdr event produces it, and no handler could run it without new behavior. Step 5 adds it with the handler the Rex stream needs.
- **A move with no destination tab is an error.** `PaneSlot.tab` is optional because a close names no tab. Herdr's `pane_moved` always names one, so the error "a moved pane event names no destination tab" cannot occur on Herdr.

**Commit 11** (`refactor(mux): connect the dashboard through MuxConnector`).

- **The dashboard holds one connector, and connects once per socket and read.** `run_server` makes one `Arc<dyn MuxConnector>` (`mux::herdr::HerdrConnector`) and gives it to `Front` and the hub. Each read connects to the socket it reads, the stored `session_socket_id`, with that read's limits, so the all-spaces discovery keeps one connection per Herdr socket. A Herdr connection is a `ProcessHerdr` with that socket. Each command it runs calls `execute_bounded` with the connection's `Budget`, so every time limit, size limit, and error text is the same as before.
- **`connect` takes a cancellation flag.** The hub's discovery and the live-peek history reader stop a running `herdr` when the flag is set. The signature is `connect(endpoint, budget, cancelled: Arc<AtomicBool>)`. A read that cannot be cancelled passes `Arc::default()`.
- **`Budget::ADAPTER_DEFAULT` replaces a plain `execute`.** It is `Duration::MAX` and `usize::MAX`. `ProcessHerdr` caps the timeout at `NAMEDROP_HERDR_TIMEOUT_MS`, so this budget runs exactly what `execute` ran. The direct-message presence read uses it.
- **`Scope::PanesOf(&SpaceId)` is `pane list --workspace` alone.** Live peek, the skills reply, and subagent maintenance listed one space's panes without its checkout. `Scope::Space` with `Depth::Panes` also runs `worktree list`, so a new scope keeps today's commands. Either depth runs the same single command. The fake, the conformance suite, and `herdr/tests.rs` cover it.
- **`PaneFacts` gains `viewport_rows` and `reports_session`.** Live peek sizes its observer from Herdr's `scroll.viewport_rows`. Subagent maintenance treats `agent_session: null` as an ended session and a missing field as unknown. The adapter's `Option<AgentSessionEntry>` mapped both to no session, so `reports_session` records whether the reply had the field. `DiscoveredPane::facts()` keeps neither fact, so it returns `None` and `false`.
- **An unreadable `shell_pid` reads as no shell instead of failing the whole process reply.** Live peek read the tty and ignored a malformed pid beside it, and `tests/dashboard_live_peek.rs` pins that. For `verify_inherited_pane` and `shell_is_available`, a malformed pid gave the same answer before (a `Protocol` error, then "no shell") as it does now (`None`). Every other `process-info` field stays strict.
- **Accepted gaps for replies Herdr does not send.** The adapter's pane listing requires `terminal_id`, `tab_id`, and `workspace_id` on every pane, and string-typed optional fields. Before this commit, the dashboard parsers needed fewer fields. Its `workspace list` and `workspace get` listings require `checkout_path` inside a stamped `worktree` and a non-negative `number`. Its process reply requires readable `foreground_*` fields. A reply that breaks one of these rules now fails the whole read, where before only the field was ignored: live peek and skills answer 503, subagent maintenance reports "unknown", and the label and repository are not recorded. Real Herdr always sends these fields, and the test fixtures, which copy Herdr's replies, pass unchanged.
- **`RepoRef::from_herdr_worktree` is now `RepoRef::of_checkout(Option<&Checkout>)`.** The hub's `HerdrWorkspaceList`, the dashboard's `HerdrWorkspaceGet`, and the inline pane structs in live peek and the skills API are deleted. `SkillRuntime::parse` is gone too, because the skills reply reads `AgentKind`.
- **`dashboard::open` reads its space through `survey(Scope::Space, Depth::Spaces)`.** That runs the same `workspace get`. Its bound is now `impl MuxRead`, so `+ HerdrCommandExecutor` is gone. `discover_worktree_for_workspace` takes `?Sized`, so the dashboard can pass a `&dyn Mux`.
- **Input stays raw until commit 12.** Live peek's `send` (`pane run`, `pane send-keys`) still uses its own `ProcessHerdr`. So do the runtimes that deliver or interrupt: the operator post, the direct message, the coordination review in `http.rs`, the channel drain, and the thread-control interrupt. The `/bin/ps` probe in live peek is not a Herdr read, and it keeps `ProcessHerdr::for_binary`. Commit 16 must give it its own runner when `ProcessHerdr` becomes private. The non-dashboard constructions in `main.rs`, `deferred_commands.rs`, and `reminder/clock.rs`, and `main.rs`'s `status server` fallback, build `LifecycleRuntime`s and are outside this commit.

**Review fixes** (`fix(mux): restore Herdr's agent-kind and process-info checks`). An independent review found two places where commits 6 and 7 changed Herdr behavior.

- **A pane that reports only a session is not a recognized agent.** Commit 7's adapter returns `Some(AgentIdentity { kind: AgentKind::Unreported, .. })` when Herdr reports a session without an agent word. Three readers had checked the agent word alone, and after commit 7 they read `agent.is_some()` as "word or session". `name` then refused such a pane with "the current pane must contain a recognized Codex or Claude session". `tab_label_owner` counted it as a second candidate, so the tab lost its managed label. The tour opened on it. These three now call `DiscoveredPane::reports_kind()`, which is true when the kind is not `Unreported`. The other `agent.is_some()` and `agent.is_none()` readers replaced a "word or session" check, so they keep `agent` alone.
- **A process reply needs both pids.** The old `terminal.rs` parser required `shell_pid` and `foreground_process_group_id`, so a reply with either null or missing read as no reply. Commit 6 made both optional in `PaneProcess`. `execute_terminal` then ran a command in a pane that names no shell, and `detect_pane_type` and the Codex in-process check read a reply without a foreground group. `verify_inherited_pane` also accepted a pane whose reply names no foreground group, where it had refused with "Herdr reports no shell". `terminal.rs` now converts the reply to a private `ShellProcess` with both pids, or `None`, and every terminal check reads that. Live peek's tty probe never needed either pid, so it keeps reading `PaneProcess`.
- **Three differences the review accepted.** Real Herdr replies do not reach any of them.
  - A pane that reports `display_agent` with no agent word and no session now has no occupant label. Commit 7 keeps the label inside `AgentIdentity`, which is `None` for such a pane. Main kept `display_agent.or(agent)` regardless.
  - `wire::process` rejects a whole reply with a malformed `foreground_process_group_id` or `foreground_processes`. Main's live-peek tty probe read only `tty` and `shell_pid` from raw JSON, so it ignored those fields. The terminal checks were already this strict on main.
  - Pids are `u32`. A `shell_pid` or group above `u32::MAX` now reads as no reply. Main parsed it as `u64`.
- **One audit entry needed a condition.** `PaneLabel::for_pane` replaced a check of the agent word alone. It matches main only because it also requires `agent_session_id().is_none()`, which excludes session-only panes.

## Open questions and risks

- Is a dashboard key that reports busy during a delivery acceptable, or should it wait a short fixed time? Commit 12 decides.
- `emulate_prompt` holds the pane lock while it waits for confirmation, for up to about 5 seconds. Is that acceptable for the Rex bridge, or should confirmation move to `observe_agent_attempt`?
- Can `rex block call process` report a start time, so that an incarnation is a process ID plus a start time? Step 4 must check this.
- When a pane has both `HERDR_PANE_ID` and `REX_BLOCK`, which multiplexer does `MuxConnector` choose? Step 3 decides.

## Next implementation step

Commit 1: add the `mux` model, ports, and gate modules with unit tests for `AgentState`, the admission rule, and `emulate_prompt`.
