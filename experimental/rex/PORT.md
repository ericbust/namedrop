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

## Open questions and risks

- Is a dashboard key that reports busy during a delivery acceptable, or should it wait a short fixed time? Commit 12 decides.
- Does any test read the JSON that a Herdr popup command prints? If one does, `Applied` needs a variant that carries the popup's pane. Commit 9 decides.
- `emulate_prompt` holds the pane lock while it waits for confirmation, for up to about 5 seconds. Is that acceptable for the Rex bridge, or should confirmation move to `observe_agent_attempt`?
- Can `rex block call process` report a start time, so that an incarnation is a process ID plus a start time? Step 4 must check this.
- When a pane has both `HERDR_PANE_ID` and `REX_BLOCK`, which multiplexer does `MuxConnector` choose? Step 3 decides.

## Next implementation step

Commit 1: add the `mux` model, ports, and gate modules with unit tests for `AgentState`, the admission rule, and `emulate_prompt`.
