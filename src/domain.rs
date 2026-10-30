use serde::{Deserialize, Serialize};

macro_rules! string_id {
    ($name:ident) => {
        #[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Self {
                Self(value.into())
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

string_id!(WorktreeId);
string_id!(SpaceId);
string_id!(PaneRoleId);
string_id!(PaneAddressId);
string_id!(PaneBindingId);
string_id!(AgentSessionId);
string_id!(ThreadId);
string_id!(MessageId);
string_id!(DeliveryId);
string_id!(DeliveryAttemptId);
string_id!(AttentionItemId);
string_id!(ContextGrantId);
string_id!(ContextSnapshotId);
string_id!(CoordinationId);
string_id!(AssignmentId);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkOutcome {
    Completed,
    NeedsContinuation,
    Blocked,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CoordinationOutcome {
    Completed,
    Blocked,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReconciledAssignment {
    pub assignment_id: AssignmentId,
    pub revision: u64,
}

/// The immutable relationship carried by a request, reply, or joined result.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct CoordinationLink {
    pub coordination_id: CoordinationId,
    pub assignment_id: AssignmentId,
    pub revision: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneRolePreset {
    Planner,
    Implementer,
    Reviewer,
    Researcher,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PaneRoleDefinition {
    pub name: &'static str,
    pub description: &'static str,
    pub suggested_alias: &'static str,
}

impl PaneRolePreset {
    /// Every preset, in the order a setup offer proposes them.
    pub const ALL: [Self; 4] = [
        Self::Planner,
        Self::Implementer,
        Self::Reviewer,
        Self::Researcher,
    ];

    pub fn role(self) -> PaneRoleDefinition {
        match self {
            Self::Planner => PaneRoleDefinition {
                name: "Planner",
                description: "Turns goals and constraints into decisions and ordered, checkable plans.",
                suggested_alias: "@planner",
            },
            Self::Implementer => PaneRoleDefinition {
                name: "Implementer",
                description: "Carries out agreed work in small, verifiable steps and reports the results.",
                suggested_alias: "@implementer",
            },
            Self::Reviewer => PaneRoleDefinition {
                name: "Reviewer",
                description: "Independently checks proposed or completed work for correctness, intent, and risk.",
                suggested_alias: "@reviewer",
            },
            Self::Researcher => PaneRoleDefinition {
                name: "Researcher",
                description: "Investigates repository and connected knowledge sources, cross-checks evidence, and prepares durable findings for human-approved publication.",
                suggested_alias: "@researcher",
            },
        }
    }
}

pub fn normalize_alias(role_name: &str) -> Result<String, String> {
    let mut alias = String::new();
    let mut pending_separator = false;
    for character in role_name.chars() {
        if character.is_ascii_alphanumeric() {
            if pending_separator && !alias.is_empty() {
                alias.push('-');
            }
            alias.push(character.to_ascii_lowercase());
            pending_separator = false;
        } else if character.is_ascii_whitespace() || character == '-' || character == '_' {
            pending_separator = !alias.is_empty();
        } else {
            return Err(
                "aliases can contain only ASCII letters, digits, spaces, hyphens, or underscores"
                    .to_owned(),
            );
        }
    }
    validate_alias(&alias)?;
    Ok(alias)
}

pub fn validate_alias(alias: &str) -> Result<(), String> {
    if alias.is_empty()
        || alias.starts_with('-')
        || alias.ends_with('-')
        || alias
            .bytes()
            .any(|byte| !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'))
        || alias.as_bytes().windows(2).any(|pair| pair == b"--")
    {
        return Err(
            "alias must be lowercase ASCII kebab-case with no leading or trailing hyphen"
                .to_owned(),
        );
    }
    Ok(())
}

pub const MAX_PERSONA_CHARS: usize = 1_000;

/// Formatting excluded from displayed text and saved personas.
pub(crate) fn is_invisible_format(character: char) -> bool {
    matches!(
        character,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'
            | '\u{200E}'
            | '\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
            | '\u{E0000}'..='\u{E007F}'
    )
}

/// Bounds a saved persona while allowing existing emoji and script formatting.
pub fn validate_persona(description: &str) -> Result<(), String> {
    if description.chars().any(char::is_control) {
        return Err("persona cannot contain control characters".to_owned());
    }
    if description.chars().any(is_invisible_format) {
        return Err(
            "persona cannot contain invisible formatting characters such as bidirectional overrides"
                .to_owned(),
        );
    }
    if description.chars().count() > MAX_PERSONA_CHARS {
        return Err(format!(
            "persona cannot exceed {MAX_PERSONA_CHARS} characters"
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StateNamespace {
    pub session_socket_id: String,
    #[serde(alias = "worktree_id")]
    pub space_id: SpaceId,
}

impl StateNamespace {
    pub fn new(session_socket_id: impl Into<String>, space_id: SpaceId) -> Self {
        Self {
            session_socket_id: session_socket_id.into(),
            space_id,
        }
    }

    pub fn for_space(
        session_socket_id: impl Into<String>,
        workspace_id: impl Into<String>,
    ) -> Self {
        Self::new(session_socket_id, SpaceId::new(workspace_id))
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WorktreeState {
    pub activated: bool,
    pub paused: bool,
    pub deactivated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ThreadControlState {
    pub paused: bool,
    pub stopped: bool,
    pub reply_allowance: u64,
    pub reply_limit_paused: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ActiveThreadResponse {
    pub attempt_id: DeliveryAttemptId,
    pub binding_id: PaneBindingId,
    pub pane_id: String,
    pub agent_session_id: AgentSessionId,
    pub binding_active: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ThreadStopResult {
    pub state: ThreadControlState,
    pub cancelled_delivery_ids: Vec<DeliveryId>,
    pub active_responses: Vec<ActiveThreadResponse>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActorKind {
    Human,
    AgentSession,
    Plugin,
    System,
}

impl ActorKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::AgentSession => "agent_session",
            Self::Plugin => "plugin",
            Self::System => "system",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MutationAttribution {
    pub actor_kind: ActorKind,
    pub actor_id: String,
    pub occurred_at_ms: u64,
}

impl MutationAttribution {
    pub fn new(actor_kind: ActorKind, actor_id: impl Into<String>, occurred_at_ms: u64) -> Self {
        Self {
            actor_kind,
            actor_id: actor_id.into(),
            occurred_at_ms,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PaneAddressDraft {
    pub id: PaneAddressId,
    pub role_id: PaneRoleId,
    pub role_name: String,
    pub role_description: String,
    pub alias: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum PaneType {
    #[default]
    Agent,
    Terminal,
}

impl PaneType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Terminal => "terminal",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PaneAddressRecord {
    #[serde(default)]
    pub pane_type: PaneType,
    pub id: PaneAddressId,
    pub role_id: PaneRoleId,
    pub role_name: String,
    pub role_description: String,
    /// The editable specialization shown to the user and to collaborators.
    /// `None` is Generic: no specialization, still eligible for ordinary work.
    /// It is independent of `role_description`, which the native naming path
    /// overwrites with its own auto-registration marker.
    #[serde(default)]
    pub persona: Option<String>,
    pub alias: String,
    pub lifecycle: PaneAddressLifecycle,
    pub pane_id: Option<String>,
    pub pane_label: Option<String>,
    pub agent_session_id: Option<AgentSessionId>,
    pub occupant_label: Option<String>,
    pub agent_status: Option<String>,
    pub retired: bool,
    pub collaboration_disabled: bool,
    /// Other workspaces may command this terminal's current shell.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub remote_commands: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PaneBindingRecord {
    pub id: PaneBindingId,
    pub address_id: PaneAddressId,
    pub pane_id: String,
    pub tab_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaneAddressLifecycle {
    Occupied,
    Empty,
    Unbound,
    Retired,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextReferenceDraft {
    pub address_id: PaneAddressId,
    pub alias_snapshot: String,
    pub pane_binding_snapshot: Option<PaneBindingId>,
    pub agent_session_snapshot: Option<AgentSessionId>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextGrantState {
    Pending,
    Active,
    Revoked,
    Expired,
}

impl ContextGrantState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Active => "active",
            Self::Revoked => "revoked",
            Self::Expired => "expired",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextGrantDraft {
    pub id: ContextGrantId,
    pub thread_id: ThreadId,
    pub source_address_id: PaneAddressId,
    pub grantee_address_id: PaneAddressId,
    pub pane_binding_id: PaneBindingId,
    pub agent_session_id: AgentSessionId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContextGrantRecord {
    pub grant_id: ContextGrantId,
    pub thread_id: ThreadId,
    pub source_address_id: PaneAddressId,
    pub grantee_address_id: PaneAddressId,
    pub pane_binding_id: Option<PaneBindingId>,
    pub agent_session_id: AgentSessionId,
    pub state: ContextGrantState,
    pub snapshot_id: Option<ContextSnapshotId>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    PaneOutput,
    File,
    Diff,
    Commit,
}

impl EvidenceKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::PaneOutput => "pane_output",
            Self::File => "file",
            Self::Diff => "diff",
            Self::Commit => "commit",
        }
    }

    pub(crate) fn parse(value: &str) -> Result<Self, String> {
        match value {
            "pane_output" => Ok(Self::PaneOutput),
            "file" => Ok(Self::File),
            "diff" => Ok(Self::Diff),
            "commit" => Ok(Self::Commit),
            _ => Err(format!("unknown evidence kind {value}")),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct EvidencePointer {
    pub kind: EvidenceKind,
    pub locator: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ContextReferenceRecord {
    pub message_id: MessageId,
    pub thread_id: ThreadId,
    pub source_address_id: PaneAddressId,
    pub alias_snapshot: String,
    pub pane_binding_snapshot: Option<PaneBindingId>,
    pub agent_session_snapshot: Option<AgentSessionId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ThreadContextSnapshotRecord {
    pub snapshot_id: ContextSnapshotId,
    pub thread_id: ThreadId,
    pub participant_address_id: PaneAddressId,
    pub payload: serde_json::Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MessageRecipientDraft {
    pub address_id: PaneAddressId,
    pub alias_snapshot: String,
    pub agent_session_snapshot: Option<AgentSessionId>,
    pub delivery_id: DeliveryId,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MessageDraft {
    pub thread_id: ThreadId,
    pub message_id: MessageId,
    pub in_reply_to: Option<MessageId>,
    pub author_address_id: PaneAddressId,
    pub author_kind: ActorKind,
    pub author_alias_snapshot: String,
    pub author_agent_session_snapshot: Option<AgentSessionId>,
    pub request_id: String,
    pub authored_source: String,
    pub body: String,
    pub grammar_version: String,
    pub recipients: Vec<MessageRecipientDraft>,
    pub context_references: Vec<ContextReferenceDraft>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MessageReceipt {
    pub message_id: MessageId,
    pub thread_id: ThreadId,
    pub thread_order: u64,
    pub deliveries: Vec<MailboxDeliveryRecord>,
    pub idempotent_replay: bool,
}

/// How a delivered message should finish after one Agent Session handles it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryReplyMode {
    AgentThread,
    Channel,
    ReturnToSource,
    ReportToHuman,
}

impl DeliveryReplyMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentThread => "agent_thread",
            Self::Channel => "channel",
            Self::ReturnToSource => "return_to_source",
            Self::ReportToHuman => "report_to_human",
        }
    }

    pub(crate) fn for_message_grammar(grammar_version: &str) -> Self {
        match grammar_version {
            "channel/v1" => Self::Channel,
            "mention-hook/v1" => Self::ReturnToSource,
            "coordination-report/v1"
            | "mention-return/v1"
            | "result-set/v1"
            | "terminal-result/v1" => Self::ReportToHuman,
            _ => Self::AgentThread,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RoutingAddressSnapshot {
    pub address_id: PaneAddressId,
    pub alias: String,
    pub pane_binding_id: Option<PaneBindingId>,
    pub agent_session_id: Option<AgentSessionId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct MessageRoute {
    pub thread_id: ThreadId,
    pub participants: Vec<RoutingAddressSnapshot>,
    pub reply_mode: DeliveryReplyMode,
    /// Namedrop wrote the message itself, under the recipient's own address
    /// and the sender name `namedrop`, so its sender is no one to reply to.
    pub from_namedrop: bool,
    asker: RoutingAddressSnapshot,
    asked_addresses: Vec<PaneAddressId>,
}

impl MessageRoute {
    pub(crate) fn new(
        thread_id: ThreadId,
        asker: RoutingAddressSnapshot,
        recipients: Vec<RoutingAddressSnapshot>,
        reply_mode: DeliveryReplyMode,
        from_namedrop: bool,
        asked_addresses: Vec<PaneAddressId>,
    ) -> Self {
        let mut participants = vec![asker.clone()];
        for recipient in recipients {
            if !participants
                .iter()
                .any(|p| p.address_id == recipient.address_id)
            {
                participants.push(recipient);
            }
        }
        Self {
            thread_id,
            participants,
            reply_mode,
            from_namedrop,
            asker,
            asked_addresses,
        }
    }

    pub(crate) fn asker_for(&self, replier: &PaneAddressId) -> Option<&RoutingAddressSnapshot> {
        self.asked_addresses
            .contains(replier)
            .then_some(&self.asker)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RoutedMessageReceipt {
    pub receipt: MessageReceipt,
    pub recipients: Vec<RoutingAddressSnapshot>,
    pub context_references: Vec<RoutingAddressSnapshot>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MailboxDeliveryRecord {
    pub delivery_id: DeliveryId,
    pub address_id: PaneAddressId,
    pub mailbox_order: u64,
    pub message_id: MessageId,
    pub state: DeliveryState,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryState {
    Queued,
    Dispatching,
    Submitted,
    Acknowledged,
    Settled,
    Held,
    Failed,
    Cancelled,
}

impl DeliveryState {
    /// The states specification section 11 classifies as Awaiting Progress rather than
    /// Attention Items: automatic progress is still expected, so no human choice is offered
    /// and no retry or cancel action is valid.
    pub fn awaits_progress(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Dispatching | Self::Submitted | Self::Acknowledged
        )
    }

    /// The Awaiting Progress states where the Collaboration Message has not been submitted into
    /// the recipient's Agent Session, so nothing has been delivered and the wait is not on an
    /// answer. The complement inside `awaits_progress` is the delivered case. This splits
    /// section 11's Awaiting Progress classification for presentation and changes none of it.
    pub fn awaits_submission(self) -> bool {
        self.awaits_progress() && matches!(self, Self::Queued | Self::Dispatching)
    }

    pub fn can_attempt_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Dispatching, Self::Submitted)
                | (Self::Dispatching, Self::Held)
                | (Self::Dispatching, Self::Failed)
                | (Self::Dispatching, Self::Cancelled)
                | (Self::Submitted, Self::Acknowledged)
                | (Self::Submitted, Self::Held)
                | (Self::Submitted, Self::Cancelled)
                | (Self::Acknowledged, Self::Settled)
                | (Self::Acknowledged, Self::Held)
                | (Self::Acknowledged, Self::Cancelled)
                | (Self::Held, Self::Cancelled)
        )
    }

    pub fn can_mailbox_transition_to(self, next: Self) -> bool {
        matches!(
            (self, next),
            (Self::Queued, Self::Dispatching)
                | (Self::Queued, Self::Held)
                | (Self::Queued, Self::Failed)
                | (Self::Queued, Self::Cancelled)
                | (Self::Dispatching, Self::Queued)
                | (Self::Dispatching, Self::Submitted)
                | (Self::Dispatching, Self::Held)
                | (Self::Dispatching, Self::Failed)
                | (Self::Dispatching, Self::Cancelled)
                | (Self::Submitted, Self::Acknowledged)
                | (Self::Submitted, Self::Held)
                | (Self::Submitted, Self::Cancelled)
                | (Self::Acknowledged, Self::Settled)
                | (Self::Acknowledged, Self::Held)
                | (Self::Acknowledged, Self::Cancelled)
                | (Self::Held, Self::Cancelled)
        )
    }
}

/// The terminal outcome an Agent Session recorded for a Delivery Attempt. The durable
/// `delivery_dispositions` row remains the truth; this is the vocabulary terminal
/// receipts use to name it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryDisposition {
    Reply,
    NoReply,
}

/// Whether a Delivery Attempt with an unresolved missing-disposition review can
/// still be finished by its exact pinned Agent Session.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryDispositionRecovery {
    /// The pinned Agent Session is still current and actively registered.
    Recoverable,
    /// The pinned Agent Session is current but must renew its Integration
    /// Registration before it can finish.
    RegistrationRequired,
    /// The pinned Agent Session is no longer current for the Pane Address.
    Orphaned,
}

impl DeliveryDisposition {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Reply => "reply",
            Self::NoReply => "no_reply",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeaseDraft {
    pub attempt_id: DeliveryAttemptId,
    pub binding_id: PaneBindingId,
    pub agent_session_id: AgentSessionId,
    pub retry_of_attempt_id: Option<DeliveryAttemptId>,
    pub owner: String,
    pub acquired_at_ms: u64,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LeasedDelivery {
    pub delivery: MailboxDeliveryRecord,
    pub attempt_id: DeliveryAttemptId,
    pub binding_id: PaneBindingId,
    pub agent_session_id: AgentSessionId,
    pub owner: String,
    pub expires_at_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DispatchCandidate {
    pub delivery_id: DeliveryId,
    pub pane_type: PaneType,
    pub address_id: PaneAddressId,
    pub binding_id: PaneBindingId,
    pub pane_id: String,
    pub agent_session_id: AgentSessionId,
    pub registration_id: String,
    pub message_id: MessageId,
    pub retry_of_attempt_id: Option<DeliveryAttemptId>,
    pub settled_delay_ms: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InflightDeliveryAttempt {
    pub attempt_id: DeliveryAttemptId,
    pub address_id: PaneAddressId,
    pub binding_id: PaneBindingId,
    pub state: DeliveryState,
    pub binding_active: bool,
    pub pane_id: String,
    pub agent_session_id: AgentSessionId,
    pub created_at_ms: u64,
    pub lease_expires_at_ms: Option<u64>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionEntityKind {
    Worktree,
    PaneAddress,
    IntegrationRegistration,
    CollaborationThread,
    CollaborationMessage,
    MailboxDelivery,
    DeliveryAttempt,
    AgentDisposition,
    AttentionItem,
    /// One session's tenure over one coordination. The entity id is
    /// `{coordination_id}:{owner_session_id}`.
    CoordinationOwnership,
}

impl TransitionEntityKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Worktree => "worktree",
            Self::PaneAddress => "pane_address",
            Self::IntegrationRegistration => "integration_registration",
            Self::CollaborationThread => "collaboration_thread",
            Self::CollaborationMessage => "collaboration_message",
            Self::MailboxDelivery => "mailbox_delivery",
            Self::DeliveryAttempt => "delivery_attempt",
            Self::AgentDisposition => "agent_disposition",
            Self::AttentionItem => "attention_item",
            Self::CoordinationOwnership => "coordination_ownership",
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransitionKind {
    Activated,
    Deactivated,
    Created,
    Bound,
    Unbound,
    Occupied,
    Empty,
    Renamed,
    Released,
    Retired,
    Challenged,
    Registered,
    Expired,
    Queued,
    Dispatching,
    Submitted,
    Invoked,
    Acknowledged,
    Settled,
    Held,
    Failed,
    Cancelled,
    CompletedReply,
    CompletedNoReply,
    Paused,
    Resumed,
    Stopped,
    Continued,
    Enabled,
    Disabled,
    RemoteCommandsAllowed,
    RemoteCommandsRevoked,
    Opened,
    Resolved,
    Adopted,
}

impl TransitionKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Activated => "activated",
            Self::Deactivated => "deactivated",
            Self::Created => "created",
            Self::Bound => "bound",
            Self::Unbound => "unbound",
            Self::Occupied => "occupied",
            Self::Empty => "empty",
            Self::Renamed => "renamed",
            Self::Released => "released",
            Self::Retired => "retired",
            Self::Challenged => "challenged",
            Self::Registered => "registered",
            Self::Expired => "expired",
            Self::Queued => "queued",
            Self::Dispatching => "dispatching",
            Self::Submitted => "submitted",
            Self::Invoked => "invoked",
            Self::Acknowledged => "acknowledged",
            Self::Settled => "settled",
            Self::Held => "held",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::CompletedReply => "completed_reply",
            Self::CompletedNoReply => "completed_no_reply",
            Self::Paused => "paused",
            Self::Resumed => "resumed",
            Self::Stopped => "stopped",
            Self::Continued => "continued",
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
            Self::RemoteCommandsAllowed => "remote_commands_allowed",
            Self::RemoteCommandsRevoked => "remote_commands_revoked",
            Self::Opened => "opened",
            Self::Resolved => "resolved",
            Self::Adopted => "adopted",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TransitionFact {
    pub worktree_order: u64,
    pub entity_kind: TransitionEntityKind,
    pub entity_id: String,
    pub transition_kind: TransitionKind,
    pub actor_kind: ActorKind,
    pub actor_id: String,
    pub occurred_at_ms: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionSourceKind {
    DeliveryAttempt,
    AutomaticReplyLimit,
    PaneBinding,
    IntegrationRegistration,
    AgentDisposition,
    ContextGrant,
    Coordination,
}

impl AttentionSourceKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::DeliveryAttempt => "delivery_attempt",
            Self::AutomaticReplyLimit => "automatic_reply_limit",
            Self::PaneBinding => "pane_binding",
            Self::IntegrationRegistration => "integration_registration",
            Self::AgentDisposition => "agent_disposition",
            Self::ContextGrant => "context_grant",
            Self::Coordination => "coordination",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AttentionItemDraft {
    pub id: AttentionItemId,
    pub source_kind: AttentionSourceKind,
    pub source_id: String,
    pub summary: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AttentionItemRecord {
    pub id: AttentionItemId,
    pub source_kind: AttentionSourceKind,
    pub source_id: String,
    pub summary: String,
    pub opened_at_ms: u64,
    pub resolved_at_ms: Option<u64>,
}
