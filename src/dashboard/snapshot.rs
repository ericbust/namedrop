//! The wire shape the browser renders, and the one pure function that derives
//! it from store rows. Every dashboard rule (activity, leg stage, waiting-on,
//! health, lane order, redaction) lives here so the browser never re-derives.
use crate::dashboard::rows::{self, DashboardRows};
use crate::{DeliveryState, PaneType, StateNamespace, WorktreeState};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};

pub const WIRE_VERSION: u8 = 1;
const BODY_CHARS: usize = 2000;
const PREVIEW_CHARS: usize = 80;
const SHORT_CHARS: usize = 200;
const MAILBOX_LIMIT: usize = 50;
pub const HUMAN: &str = "human";

/// Opaque, URL-safe identity of one Herdr workspace: the first 20 hex chars
/// of `sha256(socket \0 workspace)`, so URLs never reveal the socket path.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize)]
pub struct SpaceKey(String);

impl SpaceKey {
    pub fn of(namespace: &StateNamespace) -> Self {
        let digest = Sha256::digest(format!(
            "{}\0{}",
            namespace.session_socket_id,
            namespace.space_id.as_str()
        ));
        Self(crate::lower_hex(&digest)[..20].to_owned())
    }

    pub fn parse(segment: &str) -> Option<Self> {
        (segment.len() == 20
            && segment
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then(|| Self(segment.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Untrusted collaborator text made safe for the wire: secrets redacted,
/// control and invisible formatting characters (bidi overrides, zero-width,
/// tags) removed, length capped. HTML escaping is the browser's
/// job (it renders through `textContent` only).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SafeText {
    pub text: String,
    pub truncated: bool,
}

impl SafeText {
    pub fn from_untrusted(raw: &str, max_chars: usize) -> Self {
        let redacted = crate::context::redact_disclosure_text(raw);
        let mut chars = redacted.chars().filter(|c| {
            (*c == '\n' || *c == '\t' || !c.is_control()) && !crate::domain::is_invisible_format(*c)
        });
        let text: String = chars.by_ref().take(max_chars).collect();
        Self {
            text,
            truncated: chars.next().is_some(),
        }
    }

    fn optional(raw: Option<&str>, max_chars: usize) -> Option<Self> {
        raw.filter(|raw| !raw.trim().is_empty())
            .map(|raw| Self::from_untrusted(raw, max_chars))
    }
}

#[derive(Debug, Serialize)]
pub struct DashboardSnapshot {
    pub wire: u8,
    pub generated_at_ms: i64,
    pub space: SpaceHeader,
    pub health: SpaceHealth,
    /// Lane order: the human first, then by first appearance in `messages`.
    pub participants: Vec<Participant>,
    /// The newest window of messages, oldest first.
    pub messages: Vec<MessageRow>,
    pub threads: Vec<ThreadChip>,
    pub coordinations: Vec<CoordinationCard>,
    /// Command and exit code only; terminal output never leaves the store.
    pub terminal_runs: Vec<TerminalRun>,
    pub attention: Vec<AttentionCard>,
}

#[derive(Debug, Serialize)]
pub struct SpaceHeader {
    pub key: SpaceKey,
    pub workspace_id: String,
    pub label: Option<SafeText>,
    pub display_name: String,
    pub addressable_label: Option<String>,
    pub state: SpaceState,
}

impl SpaceHeader {
    pub(crate) fn set_live_label(
        &mut self,
        workspace_id: &str,
        label: Option<&str>,
        addressable_label: Option<String>,
    ) {
        let label = SafeText::optional(label, SHORT_CHARS).map(|label| label.text);
        self.display_name = label.unwrap_or_else(|| workspace_id.to_owned());
        self.addressable_label = addressable_label
            .filter(|token| SafeText::from_untrusted(token, SHORT_CHARS).text == *token);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SpaceState {
    Unknown,
    NotActivated,
    Active,
    Paused,
    Deactivated,
}

impl SpaceState {
    pub fn of(worktree: Option<&WorktreeState>) -> Self {
        match worktree {
            None => Self::NotActivated,
            Some(state) if state.deactivated => Self::Deactivated,
            Some(state) if !state.activated => Self::NotActivated,
            Some(state) if state.paused => Self::Paused,
            Some(_) => Self::Active,
        }
    }
}

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "level", rename_all = "snake_case")]
pub enum SpaceHealth {
    Calm,
    Busy { in_flight: u32, working: u32 },
    NeedsYou { items: u32 },
    Paused,
    Inactive,
}

#[derive(Debug, Serialize)]
pub struct Participant {
    pub address_id: String,
    pub alias: String,
    pub kind: ParticipantKind,
    pub role: Option<SafeText>,
    pub persona: Option<SafeText>,
    pub pane_label: Option<SafeText>,
    pub activity: Activity,
    /// Inbound work not yet finished, oldest first.
    pub mailbox: Vec<MailboxItem>,
    /// Aliases whose answer this participant is waiting for.
    pub waiting_on: Vec<String>,
}

#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ParticipantKind {
    Human,
    Agent { runtime: Option<String> },
    Terminal,
}

/// Mostly first-match top to bottom, except that acknowledged work in
/// progress outranks a held delivery (which the tray and the attention
/// ribbon already show); a blocked assignment or agent status still wins.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Retired,
    Offline,
    Blocked,
    Working,
    Waiting,
    Idle,
}

#[derive(Debug, Serialize)]
pub struct MailboxItem {
    pub delivery_id: String,
    pub message_id: String,
    pub from: String,
    pub preview: SafeText,
    pub state: DeliveryState,
    pub since_ms: i64,
    pub attempts: u32,
}

#[derive(Debug, Serialize)]
pub struct MessageRow {
    pub message_id: String,
    pub thread_id: String,
    pub in_reply_to: Option<String>,
    pub at_ms: i64,
    /// A participant's `address_id`, or `"human"`.
    pub author: String,
    pub kind: MessageKind,
    pub body: SafeText,
    pub legs: Vec<DeliveryLeg>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    Mention,
    Reply,
    Handoff,
    ResultSet,
    Nudge,
    Hook,
}

impl MessageKind {
    fn of(grammar_version: &str, is_reply: bool) -> Self {
        match grammar_version {
            "delegated/v1" => Self::Handoff,
            "result-set/v1" | "coordination-report/v1" => Self::ResultSet,
            "coordination-nudge/v1" => Self::Nudge,
            "mention-hook/v1" => Self::Hook,
            "terminal-result/v1" => Self::Reply,
            _ if is_reply => Self::Reply,
            _ => Self::Mention,
        }
    }

    fn opens_request(self) -> bool {
        matches!(self, Self::Mention | Self::Handoff | Self::Hook)
    }
}

#[derive(Debug, Serialize)]
pub struct DeliveryLeg {
    pub to: String,
    pub stage: LegStage,
}

/// The delivery state collapsed with its disposition, so the browser draws
/// one glyph per arrowhead.
#[derive(Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "stage", rename_all = "snake_case")]
pub enum LegStage {
    Queued,
    Delivered,
    Working { acked_at_ms: i64 },
    Replied { reply_id: String },
    Completed { kind: String },
    Held,
    Failed,
    Cancelled,
}

impl LegStage {
    fn of(leg: &rows::LegRow) -> Self {
        match (leg.disposition_kind.as_deref(), &leg.disposition_id) {
            (Some("reply"), Some(reply_id)) => {
                return Self::Replied {
                    reply_id: reply_id.clone(),
                };
            }
            (Some(kind), _) => {
                return Self::Completed {
                    kind: kind.to_owned(),
                };
            }
            (None, _) => {}
        }
        match leg.state.unwrap_or(DeliveryState::Queued) {
            DeliveryState::Queued | DeliveryState::Dispatching => Self::Queued,
            DeliveryState::Submitted => Self::Delivered,
            DeliveryState::Acknowledged => Self::Working {
                acked_at_ms: leg.acknowledged_at_ms.unwrap_or_default(),
            },
            DeliveryState::Settled => Self::Completed {
                kind: "settled".to_owned(),
            },
            DeliveryState::Held => Self::Held,
            DeliveryState::Failed => Self::Failed,
            DeliveryState::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ThreadChip {
    pub thread_id: String,
    pub title: SafeText,
    pub message_count: u32,
}

#[derive(Debug, Serialize)]
pub struct CoordinationCard {
    pub id: String,
    pub owner: String,
    pub deliverable: SafeText,
    pub outcome: String,
    pub updated_at_ms: i64,
    pub assignments: Vec<AssignmentChip>,
}

#[derive(Debug, Serialize)]
pub struct AssignmentChip {
    pub assignee: String,
    pub work_outcome: String,
    pub blocked_reason: Option<SafeText>,
    pub waiting_on: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TerminalRun {
    pub terminal: String,
    pub command: SafeText,
    pub started_at_ms: i64,
    pub exit_code: Option<i32>,
    pub message_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AttentionCard {
    pub source_kind: AttentionSourceKind,
    pub summary: SafeText,
    pub opened_at_ms: i64,
    pub subject_alias: Option<String>,
    pub message_id: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionSourceKind {
    AgentStatus,
    #[serde(untagged)]
    Stored(crate::AttentionSourceKind),
}

/// One entry of the space index at `/`: labels only, never content.
#[derive(Debug, Serialize)]
pub struct SpaceSummary {
    pub key: SpaceKey,
    pub workspace_id: String,
    pub label: Option<SafeText>,
    pub state: SpaceState,
}

impl SpaceSummary {
    pub fn of(space: &rows::SpaceRow, label: Option<&str>) -> Self {
        Self {
            key: SpaceKey::of(&space.namespace),
            workspace_id: space.namespace.space_id.as_str().to_owned(),
            label: SafeText::optional(label, SHORT_CHARS),
            state: SpaceState::of(Some(&space.state)),
        }
    }
}

impl DashboardSnapshot {
    pub fn unknown(key: SpaceKey, label: Option<&str>, now_ms: i64) -> Self {
        Self {
            wire: WIRE_VERSION,
            generated_at_ms: now_ms,
            space: SpaceHeader {
                key,
                workspace_id: String::new(),
                label: SafeText::optional(label, SHORT_CHARS),
                display_name: String::new(),
                addressable_label: None,
                state: SpaceState::Unknown,
            },
            health: SpaceHealth::Inactive,
            participants: Vec::new(),
            messages: Vec::new(),
            threads: Vec::new(),
            coordinations: Vec::new(),
            terminal_runs: Vec::new(),
            attention: Vec::new(),
        }
    }
}

pub fn build(rows: &DashboardRows, label: Option<&str>, now_ms: i64) -> DashboardSnapshot {
    let state = SpaceState::of(rows.worktree.as_ref());
    let messages: Vec<MessageRow> = rows
        .messages
        .iter()
        .map(|m| message(m, &rows.legs))
        .collect();
    let participants = participants(rows, &messages);
    let in_flight = rows
        .mailbox
        .iter()
        .filter(|entry| entry.state.awaits_progress())
        .count();
    let working = participants
        .iter()
        .filter(|p| p.activity == Activity::Working)
        .count();
    let mut attention: Vec<AttentionCard> = rows
        .attention
        .iter()
        .map(|item| AttentionCard {
            source_kind: AttentionSourceKind::Stored(item.item.source_kind),
            summary: SafeText::from_untrusted(&item.item.summary, SHORT_CHARS),
            opened_at_ms: i64::try_from(item.item.opened_at_ms).unwrap_or(i64::MAX),
            subject_alias: item.subject_alias.clone(),
            message_id: item.message_id.clone(),
        })
        .collect();
    attention.extend(rows.addresses.iter().filter_map(|address| {
        blocked_on_user_at(address).map(|opened_at_ms| AttentionCard {
            source_kind: AttentionSourceKind::AgentStatus,
            summary: SafeText::from_untrusted(
                &format!("@{} is waiting on an approval or question.", address.alias),
                SHORT_CHARS,
            ),
            opened_at_ms,
            subject_alias: Some(address.alias.clone()),
            message_id: address.blocked_message_id.clone(),
        })
    }));
    DashboardSnapshot {
        wire: WIRE_VERSION,
        generated_at_ms: now_ms,
        space: SpaceHeader {
            key: SpaceKey::of(&rows.namespace),
            workspace_id: rows.namespace.space_id.as_str().to_owned(),
            label: SafeText::optional(label, SHORT_CHARS),
            display_name: rows.namespace.space_id.as_str().to_owned(),
            addressable_label: None,
            state,
        },
        health: health(state, attention.len(), in_flight, working),
        participants,
        threads: threads(rows, &messages),
        messages,
        coordinations: rows
            .coordinations
            .iter()
            .map(|c| CoordinationCard {
                id: c.coordination_id.clone(),
                owner: c.owner_address_id.clone(),
                deliverable: SafeText::from_untrusted(&c.deliverable, SHORT_CHARS),
                outcome: c.outcome.clone(),
                updated_at_ms: c.updated_at_ms,
                assignments: rows
                    .assignments
                    .iter()
                    .filter(|a| a.coordination_id == c.coordination_id)
                    .map(|a| AssignmentChip {
                        assignee: a.assignee_address_id.clone(),
                        work_outcome: a.work_outcome.clone(),
                        blocked_reason: SafeText::optional(
                            a.blocked_reason.as_deref(),
                            SHORT_CHARS,
                        ),
                        waiting_on: a.waiting_on_address_id.clone(),
                    })
                    .collect(),
            })
            .collect(),
        terminal_runs: rows
            .terminal_runs
            .iter()
            .map(|run| TerminalRun {
                terminal: run.address_id.clone(),
                command: SafeText::from_untrusted(&run.command, SHORT_CHARS),
                started_at_ms: run.started_at_ms,
                exit_code: run.exit_code,
                message_id: run.message_id.clone(),
            })
            .collect(),
        attention,
    }
}

fn health(state: SpaceState, attention: usize, in_flight: usize, working: usize) -> SpaceHealth {
    let count = |n: usize| u32::try_from(n).unwrap_or(u32::MAX);
    match state {
        SpaceState::Paused => SpaceHealth::Paused,
        SpaceState::Active if attention > 0 => SpaceHealth::NeedsYou {
            items: count(attention),
        },
        SpaceState::Active if in_flight > 0 || working > 0 => SpaceHealth::Busy {
            in_flight: count(in_flight),
            working: count(working),
        },
        SpaceState::Active => SpaceHealth::Calm,
        SpaceState::Unknown | SpaceState::NotActivated | SpaceState::Deactivated => {
            SpaceHealth::Inactive
        }
    }
}

pub(crate) fn message(row: &rows::MessageRow, legs: &[rows::LegRow]) -> MessageRow {
    MessageRow {
        message_id: row.message_id.clone(),
        thread_id: row.thread_id.clone(),
        in_reply_to: row.in_reply_to.clone(),
        at_ms: row.created_at_ms,
        author: if row.author_is_human {
            HUMAN.to_owned()
        } else {
            row.author_address_id.clone()
        },
        kind: MessageKind::of(&row.grammar_version, row.in_reply_to.is_some()),
        body: SafeText::from_untrusted(&row.body, BODY_CHARS),
        legs: legs
            .iter()
            .filter(|leg| leg.message_id == row.message_id)
            .map(|leg| DeliveryLeg {
                to: leg.address_id.clone(),
                stage: LegStage::of(leg),
            })
            .collect(),
    }
}

fn threads(rows: &DashboardRows, messages: &[MessageRow]) -> Vec<ThreadChip> {
    let mut seen = HashSet::new();
    messages
        .iter()
        .filter(|m| seen.insert(m.thread_id.as_str()))
        .filter_map(|m| rows.threads.iter().find(|t| t.thread_id == m.thread_id))
        .map(|t| ThreadChip {
            thread_id: t.thread_id.clone(),
            title: SafeText::from_untrusted(&t.title, SHORT_CHARS),
            message_count: t.message_count,
        })
        .collect()
}

fn participants(rows: &DashboardRows, messages: &[MessageRow]) -> Vec<Participant> {
    let alias_of: HashMap<&str, &str> = rows
        .addresses
        .iter()
        .map(|a| (a.address_id.as_str(), a.alias.as_str()))
        .collect();
    let at = |address_id: &str, human: bool| {
        if human {
            "you".to_owned()
        } else {
            format!(
                "@{}",
                alias_of.get(address_id).copied().unwrap_or(address_id)
            )
        }
    };

    let mut order: Vec<&str> = Vec::new();
    for m in messages {
        for id in std::iter::once(m.author.as_str()).chain(m.legs.iter().map(|l| l.to.as_str())) {
            if !order.contains(&id) {
                order.push(id);
            }
        }
    }
    let referenced: HashSet<&str> = order
        .iter()
        .copied()
        .chain(rows.mailbox.iter().map(|e| e.address_id.as_str()))
        .chain(
            rows.coordinations
                .iter()
                .map(|c| c.owner_address_id.as_str()),
        )
        .chain(
            rows.assignments
                .iter()
                .map(|a| a.assignee_address_id.as_str()),
        )
        .chain(rows.terminal_runs.iter().map(|r| r.address_id.as_str()))
        .collect();
    let mut addresses: Vec<&rows::AddressRow> = rows
        .addresses
        .iter()
        .filter(|a| !a.retired || referenced.contains(a.address_id.as_str()))
        .collect();
    addresses.sort_by_key(|a| {
        (
            order
                .iter()
                .position(|id| *id == a.address_id.as_str())
                .unwrap_or(usize::MAX),
            a.alias.clone(),
        )
    });

    let waiting_on = |author: &str, human: bool| -> Vec<String> {
        let mut aliases: Vec<String> = Vec::new();
        for entry in rows.mailbox.iter().filter(|e| {
            e.state.awaits_progress()
                && MessageKind::of(&e.grammar_version, e.in_reply_to.is_some()).opens_request()
                && e.author_is_human == human
                && (human || e.author_address_id == author)
                && e.address_id != author
        }) {
            let alias = at(&entry.address_id, false);
            if !aliases.contains(&alias) {
                aliases.push(alias);
            }
        }
        aliases
    };

    let mut lanes = Vec::new();
    if rows.messages.iter().any(|m| m.author_is_human) {
        let waiting_on = waiting_on(HUMAN, true);
        lanes.push(Participant {
            address_id: HUMAN.to_owned(),
            alias: "you".to_owned(),
            kind: ParticipantKind::Human,
            role: None,
            persona: None,
            pane_label: None,
            activity: if waiting_on.is_empty() {
                Activity::Idle
            } else {
                Activity::Waiting
            },
            mailbox: Vec::new(),
            waiting_on,
        });
    }
    for address in addresses {
        let id = address.address_id.as_str();
        let inbound: Vec<&rows::MailboxRow> =
            rows.mailbox.iter().filter(|e| e.address_id == id).collect();
        let waiting_on = waiting_on(id, false);
        let blocked_assignment = rows.assignments.iter().any(|a| {
            a.assignee_address_id == id
                && a.work_outcome == "blocked"
                && rows
                    .coordinations
                    .iter()
                    .any(|c| c.coordination_id == a.coordination_id && c.outcome == "open")
        });
        lanes.push(Participant {
            address_id: id.to_owned(),
            alias: format!("@{}", address.alias),
            kind: match address.pane_type {
                PaneType::Terminal => ParticipantKind::Terminal,
                PaneType::Agent => ParticipantKind::Agent {
                    runtime: rows
                        .registrations
                        .iter()
                        .find(|r| r.address_id == id)
                        .map(|r| r.runtime.clone()),
                },
            },
            // Default roles are named `@alias`, which only repeats the lane title.
            role: (address.role_name.trim_start_matches('@') != address.alias)
                .then(|| SafeText::from_untrusted(&address.role_name, SHORT_CHARS)),
            persona: SafeText::optional(address.persona.as_deref(), SHORT_CHARS),
            pane_label: None,
            activity: activity(
                address,
                &inbound,
                blocked_assignment,
                !waiting_on.is_empty(),
            ),
            mailbox: inbound
                .iter()
                .take(MAILBOX_LIMIT)
                .map(|entry| MailboxItem {
                    delivery_id: entry.delivery_id.clone(),
                    message_id: entry.message_id.clone(),
                    from: at(&entry.author_address_id, entry.author_is_human),
                    preview: SafeText::from_untrusted(&entry.body, PREVIEW_CHARS),
                    state: entry.state,
                    since_ms: entry.since_ms,
                    attempts: entry.attempts,
                })
                .collect(),
            waiting_on,
        });
    }
    lanes
}

fn blocked_on_user_at(address: &rows::AddressRow) -> Option<i64> {
    if address.pane_type == PaneType::Agent
        && !address.retired
        && address.bound
        && address.occupied
        && address.agent_status.as_deref() == Some("blocked")
    {
        address.blocked_since_ms
    } else {
        None
    }
}

fn activity(
    address: &rows::AddressRow,
    inbound: &[&rows::MailboxRow],
    blocked_assignment: bool,
    waiting: bool,
) -> Activity {
    let unoccupied = address.pane_type == PaneType::Agent && !address.occupied;
    if address.retired {
        Activity::Retired
    } else if !address.bound || unoccupied {
        Activity::Offline
    } else if blocked_assignment || blocked_on_user_at(address).is_some() {
        Activity::Blocked
    } else if address.agent_status.as_deref() == Some("working")
        || inbound
            .iter()
            .any(|e| e.state == DeliveryState::Acknowledged)
    {
        Activity::Working
    } else if inbound.iter().any(|e| e.state == DeliveryState::Held) {
        Activity::Blocked
    } else if waiting {
        Activity::Waiting
    } else {
        Activity::Idle
    }
}
