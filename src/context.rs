use crate::{
    ActorKind, AgentSessionId, Clock, ContextGrantDraft, ContextGrantId, ContextGrantRecord,
    ContextGrantState, ContextSnapshotId, EvidenceKind, EvidencePointer, HerdrCommandExecutor,
    IdGenerator, LifecycleRuntime, MessageId, MutationAttribution, PaneAddressId, StateNamespace,
    StateStore, StoreError, StoreErrorKind, ThreadId, discover_worktree,
    discover_worktree_for_workspace,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Component, Path},
    process::{Command, Stdio},
};

const MAX_EVIDENCE_LINES: usize = 200;
const MAX_SNAPSHOT_BYTES: usize = 64 * 1024;
const MAX_EVIDENCE_BYTES: usize = 1024 * 1024;

mod checkout;
mod freshness;
pub(crate) use checkout::resolve_saved_checkout;
use checkout::{
    CheckoutAccess, CheckoutBinding, CheckoutInput, CheckoutSelector, bind_checkout,
    public_snapshot,
};
pub(crate) use freshness::review_freshness;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GrantInput {
    thread_id: String,
    source_address_id: String,
    grantee_address_ids: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeInput {
    grant_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SnapshotPublishInput {
    #[serde(default)]
    grant_ids: Vec<String>,
    thread_id: Option<String>,
    #[serde(default)]
    grantee_address_ids: Vec<String>,
    /// Pin the sender's branch, HEAD, merge-base, and uncommitted worktree+index
    /// as a `git_state` block on the published snapshot. Off by default because
    /// the pin writes objects and one `refs/herdr-context/*` ref into the repo.
    #[serde(default)]
    capture_git_state: bool,
    checkout: Option<CheckoutInput>,
    snapshot: WorkingContextSnapshot,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkingContextSnapshot {
    objective: String,
    applicable_user_instructions: Vec<String>,
    decisions: Vec<String>,
    completed_work: Vec<String>,
    current_work: Vec<String>,
    blockers: Vec<String>,
    evidence: Vec<EvidencePointer>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InspectInput {
    /// Context Reference mode: the Collaboration Message that lists the reference.
    message_id: Option<String>,
    /// Context Reference mode: one Pane Address listed in that message's `context_references`.
    reference_address_id: Option<String>,
    /// Thread Context Snapshot mode: the Collaboration Thread whose snapshot is selected.
    thread_id: Option<String>,
}

/// The one sentence that names both selection modes of `context.inspect`. Every refusal that
/// rejects a mode selection quotes it, and the operation catalog may quote it as the field
/// documentation of `context.inspect`.
const CONTEXT_INSPECT_MODES: &str = "context inspect selects exactly one mode: thread_id selects \
the Thread Context Snapshot held for the calling participant of one Collaboration Thread, and \
message_id plus reference_address_id selects one Context Reference listed in a Collaboration \
Message together with the Working Context Snapshot Context Grant it may lead to";

/// Which of the two `context.inspect` modes a request selected. The operation keeps one name;
/// the mode is chosen by the fields the input carries and is reported back as `mode`.
enum InspectSelection {
    ThreadContextSnapshot {
        thread_id: ThreadId,
    },
    ContextReference {
        message_id: MessageId,
        reference_address_id: PaneAddressId,
    },
}

impl InspectSelection {
    const THREAD_CONTEXT_SNAPSHOT: &'static str = "thread_context_snapshot";
    const CONTEXT_REFERENCE: &'static str = "context_reference";

    fn from_input(input: InspectInput) -> Result<Self, String> {
        match (
            input.thread_id,
            input.message_id,
            input.reference_address_id,
        ) {
            (Some(thread_id), None, None) => Ok(Self::ThreadContextSnapshot {
                thread_id: ThreadId::new(required(thread_id, "thread_id")?),
            }),
            (None, Some(message_id), Some(reference_address_id)) => Ok(Self::ContextReference {
                message_id: MessageId::new(required(message_id, "message_id")?),
                reference_address_id: PaneAddressId::new(required(
                    reference_address_id,
                    "reference_address_id",
                )?),
            }),
            (Some(_), _, _) => Err(format!(
                "{CONTEXT_INSPECT_MODES}; thread_id selects the Thread Context Snapshot mode and \
                 cannot be combined with message_id or reference_address_id"
            )),
            (None, Some(_), None) => Err(format!(
                "{CONTEXT_INSPECT_MODES}; the Context Reference mode also requires \
                 reference_address_id"
            )),
            (None, None, Some(_)) => Err(format!(
                "{CONTEXT_INSPECT_MODES}; the Context Reference mode also requires message_id"
            )),
            (None, None, None) => Err(format!(
                "{CONTEXT_INSPECT_MODES}; this request selected neither"
            )),
        }
    }

    fn mode(&self) -> &'static str {
        match self {
            Self::ThreadContextSnapshot { .. } => Self::THREAD_CONTEXT_SNAPSHOT,
            Self::ContextReference { .. } => Self::CONTEXT_REFERENCE,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    grant_id: String,
    selection: EvidenceSelection,
    reveal_blocked_item: Option<String>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum EvidenceSelection {
    Snapshot,
    PaneOutput {
        locator: String,
        max_lines: usize,
    },
    File {
        locator: String,
        start_line: usize,
        max_lines: usize,
    },
    Diff {
        locator: String,
        max_lines: usize,
    },
    Commit {
        locator: String,
        max_lines: usize,
    },
    /// The pinned handoff diff of a snapshot published with `capture_git_state`:
    /// `git diff <merge_base> <pinned_commit>`, resolved live against the shared
    /// repository and paged like a file read. The shas come from the stored
    /// `git_state` block, never from this selection.
    GitState {
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        stat: bool,
        start_line: usize,
        max_lines: usize,
    },
}

impl EvidenceSelection {
    fn pointer(&self) -> Option<EvidencePointer> {
        match self {
            Self::Snapshot | Self::GitState { .. } => None,
            Self::PaneOutput { locator, .. } => Some(EvidencePointer {
                kind: EvidenceKind::PaneOutput,
                locator: locator.clone(),
            }),
            Self::File { locator, .. } => Some(EvidencePointer {
                kind: EvidenceKind::File,
                locator: locator.clone(),
            }),
            Self::Diff { locator, .. } => Some(EvidencePointer {
                kind: EvidenceKind::Diff,
                locator: locator.clone(),
            }),
            Self::Commit { locator, .. } => Some(EvidencePointer {
                kind: EvidenceKind::Commit,
                locator: locator.clone(),
            }),
        }
    }

    fn locator(&self) -> &str {
        match self {
            Self::Snapshot => "working-context-snapshot",
            Self::GitState { path, .. } => path.as_deref().unwrap_or("git-state"),
            Self::PaneOutput { locator, .. }
            | Self::File { locator, .. }
            | Self::Diff { locator, .. }
            | Self::Commit { locator, .. } => locator,
        }
    }

    fn max_lines(&self) -> usize {
        match self {
            Self::Snapshot => MAX_EVIDENCE_LINES,
            Self::PaneOutput { max_lines, .. }
            | Self::File { max_lines, .. }
            | Self::Diff { max_lines, .. }
            | Self::Commit { max_lines, .. }
            | Self::GitState { max_lines, .. } => *max_lines,
        }
    }

    fn path_locator(&self) -> Option<&str> {
        match self {
            Self::File { locator, .. } | Self::Diff { locator, .. } => Some(locator),
            Self::Commit { locator, .. } => locator.split_once(':').map(|(_, path)| path),
            Self::GitState { path, .. } => path.as_deref(),
            Self::Snapshot | Self::PaneOutput { .. } => None,
        }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum ContextInvocation {
    AgentSession,
    Human,
}

pub fn execute_context_operation<E, C, I>(
    operation: &str,
    input: Value,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Value, String>
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    execute_context_operation_as(ContextInvocation::AgentSession, operation, input, runtime)
}

pub fn execute_human_context_operation<E, C, I>(
    operation: &str,
    input: Value,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Value, String>
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    execute_context_operation_as(ContextInvocation::Human, operation, input, runtime)
}

fn execute_context_operation_as<E, C, I>(
    invocation: ContextInvocation,
    operation: &str,
    input: Value,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Value, String>
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    match operation {
        "context.grant" if invocation == ContextInvocation::Human => {
            create_human_grant(input, runtime)
        }
        "context.revoke" if invocation == ContextInvocation::Human => {
            revoke_human_grant(input, runtime)
        }
        "context.grant" | "context.revoke" => {
            Err(format!("{operation} requires a Herdr-managed human action"))
        }
        "snapshot.publish" if invocation == ContextInvocation::AgentSession => {
            publish_snapshot(input, runtime)
        }
        "snapshot.publish" => {
            Err("snapshot.publish requires the exact registered Agent Session".to_owned())
        }
        "context.inspect" => inspect_context(input, invocation, runtime),
        "context.read" => read_context(input, invocation, runtime),
        _ => Err(format!("{operation} is not a context operation")),
    }
}

fn create_human_grant<E, C, I>(
    input: Value,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Value, String>
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    let input: GrantInput = serde_json::from_value(input).map_err(crate::input_schema_error)?;
    let thread_id = ThreadId::new(required(input.thread_id, "thread_id")?);
    let source_address_id =
        PaneAddressId::new(required(input.source_address_id, "source_address_id")?);
    let grantees = required_ids(input.grantee_address_ids, "grantee_address_ids")?;
    let (namespace, _) = discover(runtime)?;
    let attribution =
        MutationAttribution::new(ActorKind::Human, "local-human", runtime.clock.now_unix_ms());
    let mut store = StateStore::open(runtime.state_directory).map_err(|error| error.to_string())?;
    let grants = store
        .transaction(|transaction| {
            let source = transaction.routing_address_by_id(&namespace, &source_address_id)?;
            let binding_id = source.pane_binding_id.ok_or_else(|| {
                StoreError::invalid_input("Context Grant source must have an active pane binding")
            })?;
            let agent_session_id = source.agent_session_id.ok_or_else(|| {
                StoreError::invalid_input("Context Grant source must have a current Agent Session")
            })?;
            grantees
                .iter()
                .map(|grantee| {
                    transaction.create_context_grant(
                        &namespace,
                        &ContextGrantDraft {
                            id: ContextGrantId::new(runtime.ids.next_id("context-grant")),
                            thread_id: thread_id.clone(),
                            source_address_id: source_address_id.clone(),
                            grantee_address_id: grantee.clone(),
                            pane_binding_id: binding_id.clone(),
                            agent_session_id: agent_session_id.clone(),
                        },
                        &attribution,
                    )
                })
                .collect::<Result<Vec<_>, _>>()
        })
        .map_err(|error| error.to_string())?;
    Ok(json!({"grants": grants}))
}

fn revoke_human_grant<E, C, I>(
    input: Value,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Value, String>
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    let input: RevokeInput = serde_json::from_value(input).map_err(crate::input_schema_error)?;
    let grant_id = ContextGrantId::new(required(input.grant_id, "grant_id")?);
    let (namespace, _) = discover(runtime)?;
    let mut store = StateStore::open(runtime.state_directory).map_err(|error| error.to_string())?;
    serde_json::to_value(
        store
            .transaction(|transaction| {
                transaction.revoke_context_grant(
                    &namespace,
                    &grant_id,
                    &MutationAttribution::new(
                        ActorKind::Human,
                        "local-human",
                        runtime.clock.now_unix_ms(),
                    ),
                )
            })
            .map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())
}

fn publish_snapshot<E, C, I>(
    input: Value,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Value, String>
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    let grant_ids = input
        .get("grant_ids")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let input: SnapshotPublishInput = match serde_json::from_value(input) {
        Ok(input) => input,
        Err(error) => {
            let error = crate::input_schema_error(error);
            record_snapshot_activation_failure(&grant_ids, &error, runtime);
            return Err(error);
        }
    };
    if let Err(error) = validate_snapshot(&input.snapshot) {
        record_snapshot_activation_failure(&grant_ids, &error, runtime);
        return Err(error);
    }
    let (namespace, discovered) = discover(runtime)?;
    let (mut payload, checkout) = match bound_payload(
        &input.snapshot,
        input.capture_git_state,
        input.checkout,
        &discovered,
    ) {
        Ok(bound) => bound,
        Err(error) => {
            record_snapshot_activation_failure(&grant_ids, &error, runtime);
            return Err(error);
        }
    };
    let pane_id = runtime
        .current_pane_id
        .ok_or_else(|| "the current Herdr pane is required for snapshot publication".to_owned())?;
    let attribution = MutationAttribution::new(
        ActorKind::AgentSession,
        "current-agent-session",
        runtime.clock.now_unix_ms(),
    );
    let mut store = StateStore::open(runtime.state_directory).map_err(|error| error.to_string())?;
    store
        .transaction(|transaction| {
            registered_current_address(transaction, &namespace, pane_id, &discovered).map(|_| ())
        })
        .map_err(|error| error.to_string())?;
    if input.capture_git_state
        && let Err(error) = capture_into_payload(
            &mut payload,
            &checkout,
            &runtime.ids.next_id("context-pin"),
            &mut store,
        )
    {
        record_snapshot_activation_failure(&grant_ids, &error, runtime);
        return Err(error);
    }
    let grants = match store.transaction(|transaction| {
        let source = registered_current_address(transaction, &namespace, pane_id, &discovered)?;
        let source_session = source.agent_session_id.clone().ok_or_else(|| {
            StoreError::invalid_input("snapshot publication requires a current Agent Session")
        })?;
        let grant_ids = if input.grant_ids.is_empty() {
            let thread_id = input.thread_id.as_ref().ok_or_else(|| {
                StoreError::invalid_input(
                    "thread_id is required when snapshot publication creates a Context Grant",
                )
            })?;
            let grantees = required_ids(input.grantee_address_ids.clone(), "grantee_address_ids")
                .map_err(StoreError::invalid_input)?;
            let binding_id = source.pane_binding_id.clone().ok_or_else(|| {
                StoreError::invalid_input("snapshot publication requires an active pane binding")
            })?;
            grantees
                .into_iter()
                .map(|grantee| {
                    let grant_id = ContextGrantId::new(runtime.ids.next_id("context-grant"));
                    transaction.create_context_grant(
                        &namespace,
                        &ContextGrantDraft {
                            id: grant_id.clone(),
                            thread_id: ThreadId::new(thread_id),
                            source_address_id: source.address_id.clone(),
                            grantee_address_id: grantee,
                            pane_binding_id: binding_id.clone(),
                            agent_session_id: source_session.clone(),
                        },
                        &attribution,
                    )?;
                    Ok(grant_id)
                })
                .collect::<Result<Vec<_>, StoreError>>()?
        } else {
            if input.thread_id.is_some() || !input.grantee_address_ids.is_empty() {
                return Err(StoreError::invalid_input(
                    "grant_ids cannot be combined with thread_id or grantee_address_ids",
                ));
            }
            input
                .grant_ids
                .iter()
                .map(|grant_id| {
                    required(grant_id.clone(), "grant_id")
                        .map(ContextGrantId::new)
                        .map_err(StoreError::invalid_input)
                })
                .collect::<Result<Vec<_>, _>>()?
        };
        grant_ids
            .iter()
            .map(|grant_id| {
                transaction.activate_context_grant(
                    &namespace,
                    grant_id,
                    &source.address_id,
                    &source_session,
                    &ContextSnapshotId::new(runtime.ids.next_id("working-context-snapshot")),
                    &payload,
                    &input.snapshot.evidence,
                    &MutationAttribution::new(
                        ActorKind::AgentSession,
                        source_session.as_str(),
                        runtime.clock.now_unix_ms(),
                    ),
                )
            })
            .collect::<Result<Vec<_>, _>>()
    }) {
        Ok(grants) => grants,
        Err(error) => {
            let error = error.to_string();
            record_snapshot_activation_failure(&grant_ids, &error, runtime);
            return Err(error);
        }
    };
    Ok(json!({"grants": grants, "checkout": checkout.public_summary()}))
}

fn bound_payload(
    snapshot: &WorkingContextSnapshot,
    capture_git_state: bool,
    checkout: Option<CheckoutInput>,
    space: &crate::DiscoveredWorktree,
) -> Result<(Value, CheckoutAccess), String> {
    let binding = bind_checkout(
        CheckoutSelector::from_input(checkout)?,
        snapshot,
        capture_git_state,
        space,
    )?;
    let record = binding.to_json();
    let checkout = CheckoutAccess::from(binding);
    let mut payload = redact_snapshot_payload(snapshot, &checkout)?;
    payload[CheckoutBinding::FIELD] = record;
    Ok((payload, checkout))
}

fn capture_into_payload(
    payload: &mut Value,
    checkout: &CheckoutAccess,
    pin_id: &str,
    store: &mut StateStore,
) -> Result<(), String> {
    let root = checkout.root()?;
    sweep_context_pin_refs(root, store);
    payload["git_state"] = git_state_json(&capture_git_state_pin(root, pin_id)?);
    Ok(())
}

fn git_state_json(pin: &CapturedGitState) -> Value {
    json!({
        "pin_id": pin.pin_id,
        "branch": pin.branch.as_deref().map(redact_disclosure_text),
        "head": pin.head,
        "merge_base": pin.merge_base,
        "pinned_commit": pin.pinned_commit,
        "dirty": pin.dirty,
    })
}

/// The inline `context` block an agent-authored `send` or `reply` may carry:
/// the full Working Context Snapshot payload `snapshot.publish` accepts, with
/// grants created for every recipient by the routing transaction itself.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct MessageContextInput {
    /// Like `SnapshotPublishInput::capture_git_state`, but **on** by default:
    /// a context handoff is presumed to be about the sender's working tree, so
    /// skipping the pin is the explicit choice.
    #[serde(default = "default_capture_git_state")]
    capture_git_state: bool,
    /// Activate-mode: fulfill exactly these pending Context Grants — the ones
    /// `request_context` replies created — instead of creating one grant per
    /// recipient. Empty means create-mode.
    #[serde(default)]
    grant_ids: Vec<String>,
    checkout: Option<CheckoutInput>,
    snapshot: WorkingContextSnapshot,
}

fn default_capture_git_state() -> bool {
    true
}

pub(crate) fn preview_message_context(
    input: MessageContextInput,
    discovered: &crate::DiscoveredWorktree,
) -> Result<Value, String> {
    if !input.grant_ids.is_empty() {
        return Err("context grant_ids require a reply, not a new send".to_owned());
    }
    validate_snapshot(&input.snapshot)?;
    let (payload, checkout) = bound_payload(
        &input.snapshot,
        input.capture_git_state,
        input.checkout,
        discovered,
    )?;
    Ok(
        json!({"snapshot":public_snapshot(payload, &checkout),"capture_git_state":input.capture_git_state,
        "git_scope":if input.capture_git_state {Some("whole_worktree")} else {None}}),
    )
}

impl MessageContextInput {
    /// The raw activate-mode grant ids, read before preparation so a failed
    /// fulfillment can still record its activation failure against them.
    pub(crate) fn fulfillment_grant_ids(&self) -> Vec<String> {
        self.grant_ids.clone()
    }
}

/// A context block validated, redacted, and (when requested) git-pinned before
/// the routing transaction opens, so a failed send leaves at most one orphaned
/// pin ref for the lazy sweep.
pub(crate) struct PreparedMessageContext {
    pub payload: Value,
    pub evidence: Vec<EvidencePointer>,
    /// The exact pending grants an activate-mode reply fulfills; empty for
    /// create-mode.
    pub grant_ids: Vec<ContextGrantId>,
}

pub(crate) fn prepare_message_context<I>(
    input: MessageContextInput,
    discovered: &crate::DiscoveredWorktree,
    ids: &I,
    store: &mut StateStore,
) -> Result<PreparedMessageContext, String>
where
    I: IdGenerator,
{
    let grant_ids = input
        .grant_ids
        .iter()
        .map(|grant_id| required(grant_id.clone(), "grant_id").map(ContextGrantId::new))
        .collect::<Result<Vec<_>, _>>()?;
    validate_snapshot(&input.snapshot)?;
    let (mut payload, checkout) = bound_payload(
        &input.snapshot,
        input.capture_git_state,
        input.checkout,
        discovered,
    )?;
    if input.capture_git_state {
        capture_into_payload(&mut payload, &checkout, &ids.next_id("context-pin"), store)?;
    }
    Ok(PreparedMessageContext {
        payload,
        evidence: input.snapshot.evidence,
        grant_ids,
    })
}

pub(crate) fn record_snapshot_activation_failure<E, C, I>(
    grant_ids: &[String],
    error: &str,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    if grant_ids.is_empty() {
        return;
    }
    let Ok((namespace, _)) = discover(runtime) else {
        return;
    };
    let Ok(mut store) = StateStore::open(runtime.state_directory) else {
        return;
    };
    let attribution = MutationAttribution::new(
        ActorKind::Plugin,
        "namedrop-context",
        runtime.clock.now_unix_ms(),
    );
    for grant_id in grant_ids {
        let _ = store.transaction(|transaction| {
            transaction.record_context_grant_activation_failure(
                &namespace,
                &ContextGrantId::new(grant_id),
                &format!("Shared context failed: {error}. Ask the sender to fix the problem and share the context again."),
                &attribution,
            )?;
            Ok(())
        });
    }
}

fn inspect_context<E, C, I>(
    input: Value,
    invocation: ContextInvocation,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Value, String>
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    let input: InspectInput = serde_json::from_value(input).map_err(crate::input_schema_error)?;
    let selection = InspectSelection::from_input(input)?;
    let mode = selection.mode();
    let (namespace, discovered) = discover(runtime)?;
    let mut store = StateStore::open(runtime.state_directory).map_err(|error| error.to_string())?;
    store
        .transaction(|transaction| {
            let reader = if invocation == ContextInvocation::AgentSession {
                let pane_id = runtime.current_pane_id.ok_or_else(|| {
                    StoreError::invalid_input(
                        "the current Herdr pane is required for context inspection",
                    )
                })?;
                Some(registered_current_address(
                    transaction,
                    &namespace,
                    pane_id,
                    &discovered,
                )?)
            } else {
                None
            };
            let (message_id, source_address_id) = match &selection {
                InspectSelection::ThreadContextSnapshot { thread_id } => {
                    let participant_address_id = &reader
                        .as_ref()
                        .ok_or_else(|| {
                            StoreError::invalid_input(
                                "the Thread Context Snapshot mode of context inspect selects the \
                                 snapshot held for the calling participant, so it requires a \
                                 registered Agent Session",
                            )
                        })?
                        .address_id;
                    let snapshot = transaction
                        .thread_context_snapshot(&namespace, thread_id, participant_address_id)
                        .map_err(|error| {
                            missing_thread_context_snapshot(
                                error,
                                thread_id,
                                participant_address_id,
                            )
                        })?;
                    return Ok(json!({
                        "mode": mode,
                        "thread_context_snapshot": snapshot,
                        "grants": [],
                    }));
                }
                InspectSelection::ContextReference {
                    message_id,
                    reference_address_id,
                } => (message_id.clone(), reference_address_id.clone()),
            };
            let reference = transaction
                .context_reference(&namespace, &message_id, &source_address_id)
                .map_err(|error| {
                    absent_context_reference(error, &message_id, &source_address_id)
                })?;
            if let Some(reader) = &reader
                && !transaction.is_thread_participant(
                    &namespace,
                    &reference.thread_id,
                    &reader.address_id,
                )?
            {
                return Err(StoreError::invalid_input(
                    "context inspection is limited to Collaboration Thread participants",
                ));
            }
            let current = transaction.routing_address_by_id(&namespace, &source_address_id)?;
            let grant = if let Some(reader) = &reader {
                transaction.latest_context_grant(
                    &namespace,
                    &reference.thread_id,
                    &source_address_id,
                    &reader.address_id,
                )?
            } else {
                transaction.latest_context_grant_for_source(
                    &namespace,
                    &reference.thread_id,
                    &source_address_id,
                )?
            };
            let (evidence, checkout) = if grant
                .as_ref()
                .is_some_and(|grant| grant.state == ContextGrantState::Active)
            {
                let grant = grant.as_ref().expect("active grant");
                require_live_grant_source(transaction, &namespace, grant, &discovered)?;
                let snapshot_id = grant
                    .snapshot_id
                    .as_ref()
                    .expect("active Context Grant has a snapshot");
                let checkout = resolve_saved_checkout(
                    &transaction.working_context_snapshot(&namespace, snapshot_id)?,
                    &discovered,
                );
                let evidence = transaction
                    .evidence_index(&namespace, snapshot_id)?
                    .into_iter()
                    .map(|pointer| evidence_index_entry(&checkout, pointer))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(StoreError::invalid_input)?;
                (evidence, Some(checkout.public_summary()))
            } else {
                (Vec::new(), None)
            };
            let authorized = grant
                .as_ref()
                .is_some_and(|grant| grant.state == ContextGrantState::Active);
            Ok(json!({
                "mode": mode,
                "authorized": authorized,
                "authorized_snapshot": authorized.then_some("working_context_snapshot"),
                "grant_state": grant
                    .as_ref()
                    .map_or("none", |grant| grant.state.as_str()),
                "grant_id": grant.as_ref().map(|grant| &grant.grant_id),
                "send_time": {
                    "address_id": reference.source_address_id,
                    "alias": reference.alias_snapshot,
                    "pane_binding_id": reference.pane_binding_snapshot,
                    "agent_session_id": reference.agent_session_snapshot,
                },
                "current": {
                    "address_id": current.address_id,
                    "alias": current.alias,
                    "pane_binding_id": current.pane_binding_id,
                    "agent_session_id": current.agent_session_id,
                    "drift": current.pane_binding_id != reference.pane_binding_snapshot
                        || current.agent_session_id != reference.agent_session_snapshot,
                },
                "evidence": evidence,
                "checkout": checkout,
            }))
        })
        .map_err(|error| error.to_string())
}

fn read_context<E, C, I>(
    input: Value,
    invocation: ContextInvocation,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Value, String>
where
    E: HerdrCommandExecutor,
    C: Clock,
    I: IdGenerator,
{
    let input: ReadInput = serde_json::from_value(input).map_err(crate::input_schema_error)?;
    validate_line_bound(input.selection.max_lines())?;
    let grant_id = ContextGrantId::new(required(input.grant_id, "grant_id")?);
    let (namespace, discovered) = discover(runtime)?;
    let mut store = StateStore::open(runtime.state_directory).map_err(|error| error.to_string())?;
    let (grant, reader, snapshot, pointers) = store
        .transaction(|transaction| {
            let reader = if invocation == ContextInvocation::AgentSession {
                let pane_id = runtime.current_pane_id.ok_or_else(|| {
                    StoreError::invalid_input(
                        "the current Herdr pane is required for Agent Session context reads",
                    )
                })?;
                Some(registered_current_address(
                    transaction,
                    &namespace,
                    pane_id,
                    &discovered,
                )?)
            } else {
                None
            };
            let grant = transaction.refresh_context_grant(&namespace, &grant_id)?;
            if grant.state != ContextGrantState::Active {
                return Err(StoreError::invalid_input(
                    "context read requires an active Context Grant",
                ));
            }
            require_live_grant_source(transaction, &namespace, &grant, &discovered)?;
            if invocation == ContextInvocation::AgentSession {
                let reader_address_id = &reader
                    .as_ref()
                    .expect("Agent Session context read has a reader")
                    .address_id;
                if grant.grantee_address_id != *reader_address_id {
                    return Err(StoreError::invalid_input(unauthorized_context_read(
                        &grant,
                        reader_address_id,
                    )));
                }
            }
            let snapshot_id = grant
                .snapshot_id
                .as_ref()
                .expect("active Context Grant has a snapshot")
                .clone();
            Ok((
                grant,
                reader,
                transaction.working_context_snapshot(&namespace, &snapshot_id)?,
                transaction.evidence_index(&namespace, &snapshot_id)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let checkout = resolve_saved_checkout(&snapshot, &discovered);

    if let Some(pointer) = input.selection.pointer()
        && !pointers.contains(&pointer)
    {
        return Err("the requested item is not in this grant's bounded evidence index".to_owned());
    }
    let human_reveal = input.reveal_blocked_item.as_deref() == Some(input.selection.locator())
        && invocation == ContextInvocation::Human;
    if input.reveal_blocked_item.is_some() && !human_reveal {
        return Err("only a human may reveal exactly one named blocked evidence item".to_owned());
    }
    if !human_reveal && let Some(locator) = input.selection.path_locator() {
        let root = checkout.root()?;
        if let Some(reason) = blocked_path_reason(root, locator) {
            return Err(format!("evidence path is blocked: {reason}"));
        }
    }

    let content = match &input.selection {
        EvidenceSelection::Snapshot => public_snapshot(snapshot, &checkout),
        EvidenceSelection::PaneOutput { max_lines, .. } => {
            let source = StateStore::open(runtime.state_directory)
                .map_err(|error| error.to_string())?
                .transaction(|transaction| {
                    transaction
                        .list_pane_addresses(&namespace)?
                        .into_iter()
                        .find(|address| address.id == grant.source_address_id)
                        .and_then(|address| address.pane_id)
                        .ok_or_else(|| {
                            StoreError::invalid_input(
                                "Context Grant source no longer has a readable pane",
                            )
                        })
                })
                .map_err(|error| error.to_string())?;
            let lines = max_lines.to_string();
            let output = runtime.herdr.execute(&[
                "pane",
                "read",
                &source,
                "--source",
                "recent-unwrapped",
                "--lines",
                &lines,
            ])?;
            Value::String(redact_disclosure_text(&output.stdout))
        }
        EvidenceSelection::File {
            locator,
            start_line,
            max_lines,
        } => Value::String(read_file(
            checkout.root()?,
            locator,
            *start_line,
            *max_lines,
            human_reveal,
        )?),
        EvidenceSelection::Diff { locator, max_lines } => {
            let root = checkout.root()?;
            let mut args = vec!["diff".to_owned(), "--".to_owned()];
            args.extend(git_evidence_paths(
                root,
                Some(locator),
                human_reveal,
                &[""],
                true,
            )?);
            Value::String(read_git(root, &args, 1, *max_lines)?)
        }
        EvidenceSelection::GitState {
            path,
            stat,
            start_line,
            max_lines,
        } => {
            if *start_line == 0 {
                return Err("git state start_line must be at least 1".to_owned());
            }
            let root = checkout.root()?;
            let git_state = snapshot
                .get("git_state")
                .and_then(Value::as_object)
                .ok_or_else(|| {
                    "this Working Context Snapshot carries no git state; it was published \
                     without capture_git_state"
                        .to_owned()
                })?;
            let pinned = git_state
                .get("pinned_commit")
                .and_then(Value::as_str)
                .ok_or_else(|| "the stored git_state block names no pinned commit".to_owned())?;
            let base = git_state
                .get("merge_base")
                .and_then(Value::as_str)
                .or_else(|| git_state.get("head").and_then(Value::as_str))
                .ok_or_else(|| "the stored git_state block names no diff base".to_owned())?;
            let mut args = vec!["diff".to_owned()];
            if *stat {
                args.push("--stat".to_owned());
            }
            args.extend([base.to_owned(), pinned.to_owned(), "--".to_owned()]);
            args.extend(git_evidence_paths(
                root,
                path.as_deref(),
                human_reveal,
                &[base, pinned],
                false,
            )?);
            Value::String(read_git(root, &args, *start_line, *max_lines)?)
        }
        EvidenceSelection::Commit { locator, max_lines } => {
            let root = checkout.root()?;
            let (commit, path) = locator.split_once(':').ok_or_else(|| {
                "commit evidence must name one path as <commit>:<path>".to_owned()
            })?;
            if commit.len() < 7
                || commit.len() > 64
                || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err(
                    "commit evidence locator must begin with a 7-64 digit hex id".to_owned(),
                );
            }
            let mut args = vec![
                "show".to_owned(),
                "--format=fuller".to_owned(),
                commit.to_owned(),
                "--".to_owned(),
            ];
            if path.is_empty() {
                return Err("commit evidence must name one non-empty path".to_owned());
            }
            args.extend(git_evidence_paths(
                root,
                Some(path),
                human_reveal,
                &[commit, &format!("{commit}^")],
                false,
            )?);
            Value::String(read_git(root, &args, 1, *max_lines)?)
        }
    };
    let attribution = MutationAttribution::new(
        if invocation == ContextInvocation::Human {
            ActorKind::Human
        } else {
            ActorKind::AgentSession
        },
        if invocation == ContextInvocation::Human {
            "local-human".to_owned()
        } else {
            reader
                .as_ref()
                .expect("Agent Session context read has a reader")
                .agent_session_id
                .as_ref()
                .map_or("agent-session", AgentSessionId::as_str)
                .to_owned()
        },
        runtime.clock.now_unix_ms(),
    );
    let audit_order = StateStore::open(runtime.state_directory)
        .map_err(|error| error.to_string())?
        .transaction(|transaction| {
            transaction.record_context_read(
                &namespace,
                &grant_id,
                reader.as_ref().map(|reader| &reader.address_id),
                input.selection.locator(),
                &attribution,
            )
        })
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "grant_id": grant_id,
        "selection": input.selection.locator(),
        "content": content,
        "scope": match input.selection {
            EvidenceSelection::Diff { .. }
            | EvidenceSelection::Commit { .. }
            | EvidenceSelection::GitState { .. } => "shared_worktree",
            EvidenceSelection::PaneOutput { .. } => "source_pane",
            EvidenceSelection::Snapshot | EvidenceSelection::File { .. } => "source_address",
        },
        "human_reveal": human_reveal,
        "audit_order": audit_order,
    }))
}

/// Names the authorization rule an unauthorized `context.read` actually violated. Section 9
/// authorizes the named grantee's pull only, so the source of a Context Grant and an unrelated
/// participant are refused for two different reasons and must be told which one applies.
fn unauthorized_context_read(grant: &ContextGrantRecord, reader: &PaneAddressId) -> String {
    let grant_id = grant.grant_id.as_str();
    let grantee = grant.grantee_address_id.as_str();
    let reader = reader.as_str();
    if grant.source_address_id.as_str() == reader {
        format!(
            "Pane Address {reader} is the source of Context Grant {grant_id}, not its named \
             grantee {grantee}. A Context Grant authorizes its named grantee to pull the source's \
             Working Context Snapshot, and no operation returns a published Working Context \
             Snapshot to its source."
        )
    } else {
        format!(
            "Context Grant {grant_id} names Pane Address {grantee} as its grantee and is \
             non-delegable, so Pane Address {reader} cannot read the Working Context Snapshot it \
             authorizes."
        )
    }
}

/// States which snapshot concept the Thread Context Snapshot mode queried, and what writes one,
/// so a participant that just published a Working Context Snapshot is not told its publication
/// does not exist.
fn missing_thread_context_snapshot(
    error: StoreError,
    thread_id: &ThreadId,
    participant_address_id: &PaneAddressId,
) -> StoreError {
    if error.kind() != StoreErrorKind::InvalidInput {
        return error;
    }
    let thread_id = thread_id.as_str();
    let participant = participant_address_id.as_str();
    StoreError::invalid_input(format!(
        "no Thread Context Snapshot exists for Pane Address {participant} in Collaboration Thread \
         {thread_id}. A Thread Context Snapshot is written only when a Pane Address newly joins an \
         existing Collaboration Thread; a participant of that thread's first Collaboration Message \
         has none. This mode never reads a Working Context Snapshot: a published Working Context \
         Snapshot is reached through its Context Grant, so inspect the Context Reference with \
         message_id plus reference_address_id and then read that grant with context read."
    ))
}

/// States where valid `reference_address_id` values come from, because the sender of a
/// Collaboration Message is not implicitly one of its Context References.
fn absent_context_reference(
    error: StoreError,
    message_id: &MessageId,
    reference_address_id: &PaneAddressId,
) -> StoreError {
    if error.kind() != StoreErrorKind::InvalidInput {
        return error;
    }
    let message_id = message_id.as_str();
    let reference = reference_address_id.as_str();
    StoreError::invalid_input(format!(
        "Collaboration Message {message_id} does not list Pane Address {reference} in its \
         context_references. Only the Pane Address ids listed in that message's \
         context_references, delivered as control.context_references in its Delivery Envelope, are \
         valid reference_address_id values, and the message's sender is not implicitly a Context \
         Reference."
    ))
}

fn discover<E, C, I>(
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<(StateNamespace, crate::DiscoveredWorktree), String>
where
    E: HerdrCommandExecutor,
{
    let discovered = match runtime.workspace_id {
        Some(workspace_id) => discover_worktree_for_workspace(runtime.herdr, workspace_id)?,
        None => discover_worktree(runtime.herdr, runtime.cwd)?,
    };
    Ok((discovered.namespace(runtime.session_socket_id), discovered))
}

pub(crate) fn registered_current_address(
    transaction: &crate::StateTransaction<'_>,
    namespace: &StateNamespace,
    pane_id: &str,
    discovered: &crate::DiscoveredWorktree,
) -> Result<crate::RoutingAddressSnapshot, StoreError> {
    let address = transaction.routing_author_for_pane(namespace, pane_id)?;
    let session = address
        .agent_session_id
        .as_ref()
        .ok_or_else(|| StoreError::invalid_input("operation requires a current Agent Session"))?;
    require_live_context_session(discovered, pane_id, session)?;
    if transaction
        .active_integration_registration(namespace, &address.address_id, session)?
        .is_none()
    {
        return Err(StoreError::invalid_input(
            "operation requires an active exact-session Integration Registration",
        ));
    }
    Ok(address)
}

fn require_live_context_session(
    discovered: &crate::DiscoveredWorktree,
    pane_id: &str,
    session: &AgentSessionId,
) -> Result<(), StoreError> {
    if discovered
        .panes
        .iter()
        .find(|pane| pane.pane_id == pane_id)
        .and_then(|pane| pane.agent_session_id.as_ref())
        != Some(session)
    {
        return Err(StoreError::invalid_input(
            "context operation does not match the live native Agent Session",
        ));
    }
    Ok(())
}

fn require_live_grant_source(
    transaction: &crate::StateTransaction<'_>,
    namespace: &StateNamespace,
    grant: &ContextGrantRecord,
    discovered: &crate::DiscoveredWorktree,
) -> Result<(), StoreError> {
    let binding = transaction
        .active_pane_bindings(namespace)?
        .into_iter()
        .find(|binding| Some(&binding.id) == grant.pane_binding_id.as_ref())
        .ok_or_else(|| {
            StoreError::invalid_input(
                "Context Grant source has no live native Agent Session binding",
            )
        })?;
    require_live_context_session(discovered, &binding.pane_id, &grant.agent_session_id)
}

fn validate_snapshot(snapshot: &WorkingContextSnapshot) -> Result<(), String> {
    required(snapshot.objective.clone(), "objective")?;
    if serde_json::to_vec(snapshot)
        .map_err(|error| error.to_string())?
        .len()
        > MAX_SNAPSHOT_BYTES
    {
        return Err(format!(
            "a Working Context Snapshot may contain at most {MAX_SNAPSHOT_BYTES} bytes"
        ));
    }
    if snapshot.evidence.len() > 64 {
        return Err("a Working Context Snapshot may index at most 64 evidence items".to_owned());
    }
    for pointer in &snapshot.evidence {
        required(pointer.locator.clone(), "evidence locator")?;
        validate_evidence_pointer(pointer)?;
    }
    Ok(())
}

fn validate_evidence_pointer(pointer: &EvidencePointer) -> Result<(), String> {
    match pointer.kind {
        EvidenceKind::File | EvidenceKind::Diff => validate_single_path(&pointer.locator),
        EvidenceKind::Commit => {
            let (commit, path) = pointer.locator.split_once(':').ok_or_else(|| {
                "commit evidence must name one path as <commit>:<path>".to_owned()
            })?;
            if commit.len() < 7
                || commit.len() > 64
                || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Err("commit evidence requires a 7-64 digit hex id".to_owned());
            }
            if path.is_empty() {
                return Err("commit evidence must name one non-empty path".to_owned());
            }
            validate_single_path(path)
        }
        EvidenceKind::PaneOutput => Ok(()),
    }
}

fn validate_single_path(locator: &str) -> Result<(), String> {
    if locator.is_empty()
        || locator.starts_with(':')
        || locator.contains('*')
        || locator.contains('?')
        || locator.contains('\0')
    {
        return Err("evidence must name one literal Worktree-relative path".to_owned());
    }
    Ok(())
}

fn required(value: String, field: &str) -> Result<String, String> {
    if value.trim().is_empty() {
        Err(format!("{field} is required"))
    } else {
        Ok(value)
    }
}

fn required_ids(values: Vec<String>, field: &str) -> Result<Vec<PaneAddressId>, String> {
    if values.is_empty() {
        return Err(format!("{field} requires at least one Pane Address"));
    }
    let mut ids = Vec::new();
    for value in values {
        let id = PaneAddressId::new(required(value, field)?);
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    Ok(ids)
}

fn validate_line_bound(max_lines: usize) -> Result<(), String> {
    if (1..=MAX_EVIDENCE_LINES).contains(&max_lines) {
        Ok(())
    } else {
        Err(format!(
            "each evidence read must request 1-{MAX_EVIDENCE_LINES} lines"
        ))
    }
}

fn evidence_index_entry(
    checkout: &CheckoutAccess,
    pointer: EvidencePointer,
) -> Result<Value, String> {
    let mut entry = json!({
        "kind": pointer.kind,
        "locator": &pointer.locator,
        "blocked": false,
    });
    let Some(locator) = evidence_pointer_path_locator(&pointer) else {
        return Ok(entry);
    };
    let blocked = match checkout {
        CheckoutAccess::Unavailable(reason, _) => {
            entry["available"] = json!(false);
            entry["checkout_reason"] = json!(reason.as_str());
            Some("protection cannot be verified while the checkout is unavailable")
        }
        _ => blocked_path_reason(checkout.root()?, locator),
    };
    if let Some(reason) = blocked {
        entry["locator"] = json!("[BLOCKED PATH]");
        entry["blocked"] = json!(true);
        entry["reason"] = json!(reason);
    }
    Ok(entry)
}

fn evidence_pointer_path_locator(pointer: &EvidencePointer) -> Option<&str> {
    match pointer.kind {
        EvidenceKind::Commit => pointer.locator.split_once(':').map(|(_, path)| path),
        EvidenceKind::File | EvidenceKind::Diff => Some(pointer.locator.as_str()),
        EvidenceKind::PaneOutput => None,
    }
}

/// The credential path components `blocked_path_reason` refuses to disclose,
/// shared with every Git evidence pathspec exclusion.
const CREDENTIAL_COMPONENT_NAMES: [&str; 7] = [
    ".ssh", ".aws", ".gnupg", ".netrc", ".npmrc", ".pypirc", ".kube",
];

/// The blocked-path policy expressed as git pathspec excludes, so a directory
/// read cannot disclose content `blocked_path_reason` would refuse to read.
/// The `.env*`, gcloud, and docker patterns mirror the inline rules of
/// `blocked_path_reason` and must change together with them.
fn blocked_pathspec_excludes() -> Vec<String> {
    let mut excludes = Vec::new();
    for name in CREDENTIAL_COMPONENT_NAMES.into_iter().chain([
        ".env",
        ".env.*",
        ".config/gcloud",
        ".docker/config.json",
    ]) {
        for pattern in [name.to_owned(), format!("{name}/**")] {
            excludes.push(format!(":(glob,icase,exclude){pattern}"));
            excludes.push(format!(":(glob,icase,exclude)**/{pattern}"));
        }
    }
    excludes
}

fn blocked_path_reason(worktree: &Path, locator: &str) -> Option<&'static str> {
    let locator_path = Path::new(locator);
    if let Some(reason) = lexical_path_block(locator_path) {
        return Some(reason);
    }
    if let Ok(canonical) = worktree.join(locator_path).canonicalize()
        && let Ok(root) = worktree.canonicalize()
    {
        let Ok(relative) = canonical.strip_prefix(root) else {
            return Some("outside-worktree paths are not disclosed");
        };
        return lexical_path_block(relative);
    }
    None
}

fn lexical_path_block(locator_path: &Path) -> Option<&'static str> {
    if locator_path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Some("outside-worktree paths are not disclosed");
    }
    if locator_path.components().any(|component| {
        let Component::Normal(name) = component else {
            return false;
        };
        let name = name.to_string_lossy().to_ascii_lowercase();
        if name == ".env" || name.starts_with(".env.") {
            return true;
        }
        CREDENTIAL_COMPONENT_NAMES
            .iter()
            .any(|blocked| name == *blocked)
    }) {
        return Some("known credential locations are not disclosed");
    }
    let components = locator_path
        .components()
        .filter_map(|component| match component {
            Component::Normal(name) => Some(name.to_string_lossy().to_ascii_lowercase()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if components.windows(2).any(|pair| {
        (pair[0] == ".config" && pair[1] == "gcloud")
            || (pair[0] == ".docker" && pair[1] == "config.json")
    }) {
        return Some("known credential locations are not disclosed");
    }
    None
}

/// All Git selectors are literal paths. A directory expands only after the
/// credential exclusions are applied; human approval can lift them for one
/// exact file, never an entire protected directory (including historical trees).
fn git_evidence_paths(
    worktree: &Path,
    path: Option<&str>,
    human_reveal: bool,
    revisions: &[&str],
    include_worktree: bool,
) -> Result<Vec<String>, String> {
    let mut paths = Vec::new();
    if let Some(path) = path {
        validate_single_path(path)?;
        paths.push(format!(":(literal){path}"));
    }
    let exact_reveal = human_reveal
        && path.is_some_and(|path| {
            if blocked_path_reason(worktree, path).is_none() {
                return false;
            }
            let mut file = false;
            if include_worktree && let Ok(metadata) = fs::symlink_metadata(worktree.join(path)) {
                if metadata.is_dir() {
                    return false;
                }
                file = metadata.is_file() || metadata.file_type().is_symlink();
            }
            for revision in revisions {
                match git_output(worktree, &["cat-file", "-t", &format!("{revision}:{path}")])
                    .as_deref()
                {
                    Ok("tree") => return false,
                    Ok("blob") => file = true,
                    _ => {}
                }
            }
            file
        });
    if !exact_reveal {
        paths.extend(blocked_pathspec_excludes());
    }
    Ok(paths)
}

fn read_file(
    worktree: &Path,
    locator: &str,
    start_line: usize,
    max_lines: usize,
    human_reveal: bool,
) -> Result<String, String> {
    if start_line == 0 {
        return Err("file evidence start_line must be at least 1".to_owned());
    }
    if !human_reveal && let Some(reason) = blocked_path_reason(worktree, locator) {
        return Err(format!("evidence path is blocked: {reason}"));
    }
    let requested_path = worktree.join(locator);
    let path = if human_reveal {
        requested_path
    } else {
        requested_path.canonicalize().map_err(|error| {
            format!(
                "failed to resolve evidence file {}: {error}",
                requested_path.display()
            )
        })?
    };
    if !human_reveal {
        let root = worktree.canonicalize().map_err(|error| error.to_string())?;
        let relative = path.strip_prefix(root).map_err(|_| {
            "evidence path is blocked: outside-worktree paths are not disclosed".to_owned()
        })?;
        if let Some(reason) = lexical_path_block(relative) {
            return Err(format!("evidence path is blocked: {reason}"));
        }
    }
    if !fs::metadata(&path)
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("file evidence must name one regular file".to_owned());
    }
    let file = fs::File::open(&path)
        .map_err(|error| format!("failed to read evidence file {}: {error}", path.display()))?;
    let content =
        String::from_utf8(read_evidence_bytes(file)?).map_err(|error| error.to_string())?;
    Ok(redact_disclosure_window(
        &content,
        start_line - 1,
        max_lines,
    ))
}

fn read_evidence_bytes(reader: impl Read) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    reader
        .take(MAX_EVIDENCE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("failed to read evidence: {error}"))?;
    if bytes.len() > MAX_EVIDENCE_BYTES {
        return Err(format!(
            "evidence exceeds {MAX_EVIDENCE_BYTES} bytes; select a smaller evidence file or Git path"
        ));
    }
    Ok(bytes)
}

fn read_git(
    worktree: &Path,
    args: &[String],
    start_line: usize,
    max_lines: usize,
) -> Result<String, String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["-c", "core.fsmonitor=false"])
        .args(&args[..1])
        .args(["--no-ext-diff", "--no-textconv", "--no-color"])
        .args(&args[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("failed to read Git evidence: {error}"))?;
    // Drain stderr concurrently so a diagnostic cannot fill its pipe while
    // stdout is being bounded. Stop the producer as soon as stdout exceeds the
    // cap; neither an enormous diff nor one enormous line is buffered in full.
    let stderr = child.stderr.take().expect("Git stderr was piped");
    let stderr_reader = std::thread::spawn(move || read_evidence_bytes(stderr));
    let output = read_evidence_bytes(child.stdout.take().expect("Git stdout was piped"));
    if output.is_err() {
        let _ = child.kill();
    }
    let status = child
        .wait()
        .map_err(|error| format!("failed to wait for Git evidence: {error}"));
    let stderr = stderr_reader
        .join()
        .map_err(|_| "failed to collect Git diagnostics".to_owned())?;
    let output = output?;
    if !status?.success() {
        return Err(String::from_utf8_lossy(&stderr?).trim().to_owned());
    }
    let output = String::from_utf8(output).map_err(|error| error.to_string())?;
    Ok(redact_disclosure_window(
        &output,
        start_line.saturating_sub(1),
        max_lines,
    ))
}

const CONTEXT_PIN_REF_PREFIX: &str = "refs/herdr-context/";

/// The plugin-captured `git_state` block of one capture-publish. The pointers
/// name the pin; `context.read` resolves them live against the shared repo.
struct CapturedGitState {
    pin_id: String,
    branch: Option<String>,
    head: String,
    merge_base: Option<String>,
    pinned_commit: String,
    dirty: bool,
}

/// Pins the sender's handoff-time work in git itself: a dangling commit of the
/// worktree — tracked modifications, the index, and untracked files that are
/// not ignored (HEAD when all of those are absent) — anchored at
/// `refs/herdr-context/<pin_id>` so gc cannot prune it. The pin is immutable
/// no matter what the sender does next.
fn capture_git_state_pin(worktree: &Path, pin_id: &str) -> Result<CapturedGitState, String> {
    let head = git_output(worktree, &["rev-parse", "HEAD"])
        .map_err(|error| format!("git state capture requires a commit at HEAD: {error}"))?;
    let branch = git_output(worktree, &["symbolic-ref", "--short", "-q", "HEAD"])
        .ok()
        .filter(|name| !name.is_empty());
    let merge_base = [
        "@{upstream}",
        "origin/HEAD",
        "origin/main",
        "origin/master",
        "main",
        "master",
    ]
    .iter()
    .find_map(|candidate| {
        git_output(worktree, &["merge-base", "HEAD", candidate])
            .ok()
            .filter(|base| !base.is_empty())
    });
    let head_tree = git_output(worktree, &["rev-parse", "HEAD^{tree}"])
        .map_err(|error| format!("git state capture could not read the HEAD tree: {error}"))?;
    let pinned_tree = pin_worktree_tree(worktree, pin_id)
        .map_err(|error| format!("git state capture could not pin uncommitted work: {error}"))?;
    let (pinned_commit, dirty) = if pinned_tree == head_tree {
        (head.clone(), false)
    } else {
        let commit = git_output(
            worktree,
            &[
                "-c",
                "user.name=herdr-context",
                "-c",
                "user.email=context@herdr.invalid",
                "commit-tree",
                &pinned_tree,
                "-p",
                "HEAD",
                "-m",
                "herdr context pin",
            ],
        )
        .map_err(|error| format!("git state capture could not pin uncommitted work: {error}"))?;
        (commit, true)
    };
    git_output(
        worktree,
        &[
            "update-ref",
            &format!("{CONTEXT_PIN_REF_PREFIX}{pin_id}"),
            &pinned_commit,
        ],
    )
    .map_err(|error| format!("git state capture could not anchor the pin ref: {error}"))?;
    Ok(CapturedGitState {
        pin_id: pin_id.to_owned(),
        branch,
        head,
        merge_base,
        pinned_commit,
        dirty,
    })
}

/// Snapshots the worktree content — tracked modifications plus untracked files
/// that are not ignored — as a tree object, through a throwaway index so the
/// user's real index is never touched. The index file name carries a
/// process-wide sequence because pin ids are only unique per id generator.
fn pin_worktree_tree(worktree: &Path, pin_id: &str) -> Result<String, String> {
    static PIN_INDEX_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = PIN_INDEX_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let index = std::env::temp_dir().join(format!(
        "herdr-context-pin-{}-{sequence}-{pin_id}.index",
        std::process::id()
    ));
    let tree = (|| {
        run_git(worktree, Some(&index), &["read-tree", "HEAD"])?;
        run_git(worktree, Some(&index), &["add", "-A"])?;
        run_git(worktree, Some(&index), &["write-tree"])
    })();
    let _ = fs::remove_file(&index);
    tree
}

/// Deletes pin refs whose Context Grants are all terminal, or that no stored
/// snapshot knows (the leftovers of failed publication transactions).
/// Best-effort: a sweep failure never blocks the publish that triggered it,
/// and an unjudgeable ref is kept.
fn sweep_context_pin_refs(worktree: &Path, store: &mut StateStore) {
    let Ok(refs) = git_output(
        worktree,
        &["for-each-ref", "--format=%(refname)", "refs/herdr-context"],
    ) else {
        return;
    };
    for name in refs.lines() {
        let Some(pin_id) = name.strip_prefix(CONTEXT_PIN_REF_PREFIX) else {
            continue;
        };
        let Ok(states) =
            store.transaction(|transaction| transaction.refresh_context_grants_for_git_pin(pin_id))
        else {
            continue;
        };
        if !states.iter().all(|state| {
            matches!(
                state,
                ContextGrantState::Revoked | ContextGrantState::Expired
            )
        }) {
            continue;
        }
        if states.is_empty() && unknown_pin_ref_may_be_in_flight(worktree, name) {
            continue;
        }
        let _ = git_output(worktree, &["update-ref", "-d", name]);
    }
}

/// How long an unknown pin ref is presumed to belong to a publish still in
/// flight. A pin ref is written before the publish's store transaction
/// commits, so a fresh ref with no grants is indistinguishable from live work;
/// sweeping it would leave an active grant pointing at a gc-prunable commit.
const CONTEXT_PIN_SWEEP_GRACE: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// True when a pin ref with no stored grants is young enough to belong to an
/// uncommitted publish. Judged by the loose ref file's age; a missing loose
/// file means git packed the ref long ago, so it is safely past the grace.
fn unknown_pin_ref_may_be_in_flight(worktree: &Path, refname: &str) -> bool {
    let Ok(relative) = git_output(worktree, &["rev-parse", "--git-path", refname]) else {
        return true;
    };
    let path = Path::new(&relative);
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        worktree.join(path)
    };
    match fs::metadata(&path).and_then(|metadata| metadata.modified()) {
        Ok(modified) => modified
            .elapsed()
            .map_or(true, |age| age < CONTEXT_PIN_SWEEP_GRACE),
        Err(_) => false,
    }
}

fn git_output(worktree: &Path, args: &[&str]) -> Result<String, String> {
    run_git(worktree, None, args)
}

fn run_git(worktree: &Path, index_file: Option<&Path>, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(worktree)
        .args(["-c", "core.fsmonitor=false"])
        .args(args);
    if let Some(index_file) = index_file {
        command.env("GIT_INDEX_FILE", index_file);
    }
    let output = command
        .output()
        .map_err(|error| format!("failed to run git: {error}"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(if stderr.is_empty() {
            format!("git {} failed", args.first().unwrap_or(&""))
        } else {
            stderr
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn redact_json(mut value: Value) -> Value {
    match &mut value {
        Value::String(text) => *text = redact_disclosure_text(text),
        Value::Array(values) => {
            for value in values {
                *value = redact_json(value.take());
            }
        }
        Value::Object(values) => {
            for value in values.values_mut() {
                *value = redact_json(value.take());
            }
        }
        _ => {}
    }
    value
}

fn redact_snapshot_payload(
    snapshot: &WorkingContextSnapshot,
    checkout: &CheckoutAccess,
) -> Result<Value, String> {
    let mut payload =
        redact_json(serde_json::to_value(snapshot).map_err(|error| error.to_string())?);
    let entries = payload
        .get_mut("evidence")
        .and_then(Value::as_array_mut)
        .expect("serialized Working Context Snapshot has an evidence array");
    for (entry, pointer) in entries.iter_mut().zip(&snapshot.evidence) {
        if let Some(locator) = evidence_pointer_path_locator(pointer)
            && blocked_path_reason(checkout.root()?, locator).is_some()
        {
            entry["locator"] = Value::String("[BLOCKED PATH]".to_owned());
        }
    }
    Ok(payload)
}

pub(crate) fn redact_disclosure_text(text: &str) -> String {
    redact_disclosure_window(text, 0, usize::MAX)
}

fn redact_disclosure_window(text: &str, skip_lines: usize, max_lines: usize) -> String {
    let mut redacted = Vec::new();
    let mut in_private_key = false;
    let mut key_marker_emitted = false;
    for (index, line) in text
        .lines()
        .enumerate()
        .take(skip_lines.saturating_add(max_lines))
    {
        let selected = index >= skip_lines;
        if line.contains("-----BEGIN ") && line.contains("PRIVATE KEY-----") {
            in_private_key = true;
            key_marker_emitted = false;
        }
        if in_private_key {
            if selected && !key_marker_emitted {
                redacted.push("[REDACTED PRIVATE KEY]".to_owned());
                key_marker_emitted = true;
            }
            if line.contains("-----END ") && line.contains("PRIVATE KEY-----") {
                in_private_key = false;
            }
            continue;
        }
        if !selected {
            continue;
        }
        let line = redact_environment_value(line);
        redacted.push(redact_fixed_tokens(&line));
    }
    redacted.join("\n")
}

fn redact_environment_value(line: &str) -> String {
    let Some((name, _value)) = line.split_once('=') else {
        return line.to_owned();
    };
    let normalized = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect::<String>();
    if [
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASS",
        "API_KEY",
        "PRIVATE_KEY",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
    {
        format!("{name}=[REDACTED ENV VALUE]")
    } else {
        line.to_owned()
    }
}

fn redact_fixed_tokens(text: &str) -> String {
    // Fixed policy: GitHub classic/fine-grained, OpenAI-style, Slack, and AWS access keys.
    let mut output = text.to_owned();
    for prefix in ["github_pat_", "ghp_", "sk-", "xoxb-", "xoxp-", "xoxa-"] {
        output = redact_prefixed_tokens(&output, prefix, 20);
    }
    redact_aws_access_keys(&output)
}

fn redact_prefixed_tokens(text: &str, prefix: &str, minimum_length: usize) -> String {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(relative) = text[cursor..].find(prefix) {
        let start = cursor + relative;
        output.push_str(&text[cursor..start]);
        let end = text[start..]
            .char_indices()
            .take_while(|(_, character)| {
                character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
            })
            .last()
            .map_or(start, |(index, character)| {
                start + index + character.len_utf8()
            });
        if end - start >= minimum_length {
            output.push_str("[REDACTED TOKEN]");
        } else {
            output.push_str(&text[start..end]);
        }
        cursor = end.max(start + prefix.len());
    }
    output.push_str(&text[cursor..]);
    output
}

fn redact_aws_access_keys(text: &str) -> String {
    let mut output = String::with_capacity(text.len());
    let mut cursor = 0;
    while let Some(relative) = text[cursor..].find("AKIA") {
        let start = cursor + relative;
        output.push_str(&text[cursor..start]);
        let candidate = text.get(start..start + 20);
        if candidate.is_some_and(|candidate| {
            candidate.is_ascii()
                && candidate[4..]
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit())
        }) {
            output.push_str("[REDACTED AWS ACCESS KEY]");
            cursor = start + 20;
        } else {
            output.push_str("AKIA");
            cursor = start + 4;
        }
    }
    output.push_str(&text[cursor..]);
    output
}
