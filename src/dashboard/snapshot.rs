//! The wire shape the browser renders, and the one pure function that derives
//! it from store rows. Every dashboard rule (activity, leg stage, waiting-on,
//! health, lane order, redaction) lives here so the browser never re-derives.
use crate::dashboard::rows::{self, DashboardRows};
use crate::{DeliveryState, PaneType, StateNamespace, WorktreeState};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::ops::Range;

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
/// unsafe controls and formatting (bidi overrides, zero-width spaces, tags)
/// removed, length capped. HTML escaping is the browser's
/// job (it renders through `textContent` only).
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SafeText {
    pub text: String,
    pub truncated: bool,
}

impl SafeText {
    pub fn from_untrusted(raw: &str, max_chars: usize) -> Self {
        let redacted = redact_for_display(raw);
        let mut chars = redacted.chars();
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

struct Shadow {
    text: String,
    /// Each byte maps to its full original character, including UTF-8 boundaries.
    source: Vec<Range<usize>>,
}

impl Shadow {
    fn push(&mut self, character: char, start: usize) {
        self.text.push(character);
        let range = start..start + character.len_utf8();
        self.source
            .extend(std::iter::repeat_n(range, character.len_utf8()));
    }

    fn project(&self, range: Range<usize>) -> Range<usize> {
        if range.is_empty() {
            let boundary = self.source.get(range.start).map_or_else(
                || self.source.last().map_or(0, |range| range.end),
                |range| range.start,
            );
            boundary..boundary
        } else {
            self.source[range.start].start..self.source[range.end - 1].end
        }
    }
}

#[derive(Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
enum MaskKind {
    Aws,
    Token,
    Env,
    PrivateKey,
}

impl MaskKind {
    fn marker(self) -> &'static str {
        match self {
            Self::Aws => "[REDACTED AWS ACCESS KEY]",
            Self::Token => "[REDACTED TOKEN]",
            Self::Env => "[REDACTED ENV VALUE]",
            Self::PrivateKey => "[REDACTED PRIVATE KEY]",
        }
    }
}

struct Mask {
    source: Range<usize>,
    kind: MaskKind,
}

fn is_token_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-')
}

fn is_display_character(character: char) -> bool {
    (matches!(character, '\n' | '\t') || !character.is_control())
        && !crate::domain::is_invisible_format(character)
}

/// Unicode 18 default ignorables, line/paragraph separators and annotations.
/// This wider set belongs only to secret detection, never display sanitation.
fn is_secret_ignorable(character: char) -> bool {
    matches!(
        character,
        '\u{00AD}'
            | '\u{034F}'
            | '\u{061C}'
            | '\u{115F}'..='\u{1160}'
            | '\u{17B4}'..='\u{17B5}'
            | '\u{180B}'..='\u{180F}'
            | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}'
            | '\u{2060}'..='\u{206F}'
            | '\u{3164}'
            | '\u{FE00}'..='\u{FE0F}'
            | '\u{FEFF}'
            | '\u{FFA0}'
            | '\u{FFF0}'..='\u{FFFB}'
            | '\u{1BCA0}'..='\u{1BCA3}'
            | '\u{1D173}'..='\u{1D17A}'
            | '\u{E0000}'..='\u{E0FFF}'
    )
}

fn secret_shadow(source: &str, canonical: bool) -> Shadow {
    enum State {
        Ground,
        Escape,
        EscapeIntermediate,
        Csi,
        ControlString { osc: bool },
        StringEscape { osc: bool },
    }
    let mut state = State::Ground;
    let mut shadow = Shadow {
        text: String::with_capacity(source.len()),
        source: Vec::with_capacity(source.len()),
    };
    for (start, character) in source.char_indices() {
        if !canonical {
            shadow.push(character, start);
            continue;
        }
        if is_secret_ignorable(character) {
            continue;
        }
        state = match state {
            State::Ground => match character {
                '\u{1b}' => State::Escape,
                '\u{9b}' => State::Csi,
                '\u{9d}' => State::ControlString { osc: true },
                '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => State::ControlString { osc: false },
                _ => {
                    if character == '\n' || !character.is_control() {
                        shadow.push(character, start);
                    }
                    State::Ground
                }
            },
            State::Escape => match character {
                '\n' => State::Ground,
                '[' => State::Csi,
                ']' => State::ControlString { osc: true },
                'P' | 'X' | '^' | '_' => State::ControlString { osc: false },
                '\u{1b}' => State::Escape,
                '\u{21}'..='\u{2f}' => State::EscapeIntermediate,
                _ if character.is_control() => State::Escape,
                _ => {
                    shadow.push(character, start);
                    State::Ground
                }
            },
            State::EscapeIntermediate => match character {
                '\u{30}'..='\u{7e}' => State::Ground,
                '\u{1b}' => State::Escape,
                '\n' => State::Ground,
                _ => State::EscapeIntermediate,
            },
            State::Csi => match character {
                '\u{40}'..='\u{7e}' => State::Ground,
                '\u{1b}' => State::Escape,
                _ => State::Csi,
            },
            State::ControlString { osc } => match character {
                '\u{9c}' => State::Ground,
                '\u{7}' if osc => State::Ground,
                '\u{1b}' => State::StringEscape { osc },
                _ => State::ControlString { osc },
            },
            State::StringEscape { osc } => match character {
                '\\' | '\u{9c}' => State::Ground,
                '\u{7}' if osc => State::Ground,
                '\u{1b}' => State::StringEscape { osc },
                _ => State::ControlString { osc },
            },
        };
    }
    shadow
}

fn token_masks(shadow: &Shadow, masks: &mut Vec<Mask>) {
    let bytes = shadow.text.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if !is_token_byte(bytes[cursor]) {
            cursor += 1;
            continue;
        }
        let start = cursor;
        while cursor < bytes.len() && is_token_byte(bytes[cursor]) {
            cursor += 1;
        }
        let end = cursor;
        let redacted = crate::context::redact_disclosure_text(&shadow.text[start..end]);
        let mut input = start;
        let mut output = redacted.as_str();
        while !output.is_empty() {
            if output.starts_with(MaskKind::Token.marker()) {
                // Ordered prefix passes can emit adjacent markers for one run.
                masks.push(Mask {
                    source: shadow.project(input..end),
                    kind: MaskKind::Token,
                });
                input = end;
                break;
            }
            if let Some(rest) = output.strip_prefix(MaskKind::Aws.marker()) {
                assert!(input + 20 <= end, "AWS oracle marker exceeds token run");
                masks.push(Mask {
                    source: shadow.project(input..input + 20),
                    kind: MaskKind::Aws,
                });
                input += 20;
                output = rest;
            } else {
                assert!(input < end, "token oracle output exceeds token run");
                assert_eq!(bytes[input], output.as_bytes()[0], "token oracle grammar");
                input += 1;
                output = &output[1..];
            }
        }
        assert_eq!(input, end, "token oracle output omitted an unmasked suffix");
    }
}

fn policy_masks(shadow: &Shadow, source_len: usize, masks: &mut Vec<Mask>) {
    let mut offset = 0;
    let mut private_start = None;
    for terminated in shadow.text.split_inclusive('\n') {
        let line = terminated
            .strip_suffix('\n')
            .map_or(terminated, |line| line.strip_suffix('\r').unwrap_or(line));
        let range = shadow.project(offset..offset + line.len());
        if private_start.is_none()
            && crate::context::redact_disclosure_text(&format!("{line} !"))
                == MaskKind::PrivateKey.marker()
        {
            private_start = Some(range.start);
        }
        if let Some(start) = private_start {
            let closing_probe = format!("-----BEGIN PRIVATE KEY-----\n{line}\n!");
            if crate::context::redact_disclosure_text(&closing_probe).ends_with("\n!") {
                masks.push(Mask {
                    source: start..range.end,
                    kind: MaskKind::PrivateKey,
                });
                private_start = None;
            }
        } else if let Some((name, _)) = line.split_once('=') {
            let witness = crate::context::redact_disclosure_text(&format!("{name}=!"));
            if witness.ends_with("=[REDACTED ENV VALUE]") {
                let start = shadow.source[offset + name.len()].end;
                masks.push(Mask {
                    source: start..range.end,
                    kind: MaskKind::Env,
                });
            }
        }
        offset += terminated.len();
    }
    if let Some(start) = private_start {
        masks.push(Mask {
            source: start..source_len,
            kind: MaskKind::PrivateKey,
        });
    }
}

fn apply_masks(source: &str, mut masks: Vec<Mask>) -> String {
    masks.sort_unstable_by_key(|mask| (mask.source.start, mask.source.end));
    let mut merged: Vec<Mask> = Vec::new();
    for mask in masks {
        if let Some(previous) = merged.last_mut()
            && (mask.source.start < previous.source.end
                || mask.source == previous.source
                || mask.source.is_empty() && mask.source.start == previous.source.end)
        {
            previous.source.end = previous.source.end.max(mask.source.end);
            previous.kind = previous.kind.max(mask.kind);
        } else {
            merged.push(mask);
        }
    }
    let mut output = String::with_capacity(source.len());
    let mut cursor = 0;
    for mask in merged {
        output.push_str(&source[cursor..mask.source.start]);
        output.push_str(mask.kind.marker());
        cursor = mask.source.end;
    }
    output.push_str(&source[cursor..]);
    output
}

fn redact_for_display(source: &str) -> String {
    let mut masks = Vec::new();
    for canonical in [false, true] {
        let shadow = secret_shadow(source, canonical);
        token_masks(&shadow, &mut masks);
        policy_masks(&shadow, source.len(), &mut masks);
    }
    let masked = apply_masks(source, masks);
    let clean: String = masked
        .chars()
        .filter(|c| is_display_character(*c))
        .collect();
    // Preserve the shared redactor's line formatting without reinterpreting markers.
    clean.lines().collect::<Vec<_>>().join("\n")
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
        if human || address_id == HUMAN {
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
        .filter(|a| {
            a.address_id != HUMAN && (!a.retired || referenced.contains(a.address_id.as_str()))
        })
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
    if rows.messages.iter().any(|m| m.author_is_human)
        || rows
            .addresses
            .iter()
            .any(|address| address.address_id == HUMAN)
    {
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

#[cfg(test)]
mod redaction_regression_tests {
    use super::SafeText;

    #[test]
    fn pem_joiners_next_to_header_spaces_are_redacted() {
        let mut failures = Vec::new();
        for joiner in ['\u{200d}', '\u{2060}', '\u{200c}', '\u{fe0f}'] {
            for header in [
                format!("-----BEGIN{joiner} PRIVATE KEY-----"),
                format!("-----BEGIN {joiner}RSA PRIVATE KEY-----"),
                format!("-----BEGIN RSA PRIVATE{joiner} KEY-----"),
                format!("-----BEGIN RSA PRIVATE {joiner}KEY-----"),
            ] {
                let raw = format!(
                    "👩‍💻 before\n{header}\nprivate-body\n-----END RSA PRIVATE KEY-----\nمی‌خواهم after"
                );
                let actual = SafeText::from_untrusted(&raw, 2000).text;
                if actual != "👩‍💻 before\n[REDACTED PRIVATE KEY]\nمی‌خواهم after" {
                    failures.push((raw, actual));
                }
            }
        }
        assert!(failures.is_empty(), "PEM joiner bypasses: {failures:#?}");
    }

    #[test]
    fn ansi_inside_tokens_and_pem_headers_is_redacted() {
        let mut failures = Vec::new();
        for raw in [
            "s\u{1b}[31mk-abcdefghijklmnopqrstuvwxyz123456",
            "ghp_abcdefgh\u{1b}[32mijklmnopqrstuvwxyz123456",
            "AKIAABCD\u{9b}31mEFGHIJKLMNOP",
            "-----BEGIN\u{1b}[31m PRIVATE KEY-----\nprivate-body\n-----END PRIVATE KEY-----",
            "-----BEGIN\u{1b} PRIVATE KEY-----\nprivate-body\n-----END PRIVATE KEY-----",
        ] {
            let expected = if raw.starts_with("-----") {
                "[REDACTED PRIVATE KEY]"
            } else if raw.starts_with("AKIA") {
                "[REDACTED AWS ACCESS KEY]"
            } else {
                "[REDACTED TOKEN]"
            };
            let actual = SafeText::from_untrusted(raw, 2000).text;
            if actual != expected {
                failures.push((raw, actual, expected));
            }
        }
        assert!(failures.is_empty(), "ANSI secret bypasses: {failures:#?}");
    }

    #[test]
    fn displayed_terminal_payloads_keep_split_secrets_redacted() {
        for (open, close, expected) in [
            ("\u{1b}]0;", "\u{7}", "👩‍💻 ]0;[REDACTED TOKEN] می‌خواهم"),
            ("\u{1b}]0;", "\u{1b}\\", "👩‍💻 ]0;[REDACTED TOKEN]\\ می‌خواهم"),
            ("\u{1b}P", "\u{1b}\\", "👩‍💻 P[REDACTED TOKEN]\\ می‌خواهم"),
            ("\u{90}", "\u{9c}", "👩‍💻 [REDACTED TOKEN] می‌خواهم"),
        ] {
            let raw =
                format!("👩‍💻 {open}s\u{200d}k-abcdefghijklmnopqrstuvwxyz123456{close} می‌خواهم");
            assert_eq!(
                SafeText::from_untrusted(&raw, 2000).text,
                expected,
                "{raw:?}"
            );
        }
    }

    #[test]
    fn terminal_sequence_families_inside_secrets_are_redacted() {
        for sequence in [
            "\u{1b}[31m",
            "\u{9b}31m",
            "\u{1b}]0;title\u{7}",
            "\u{1b}]0;title\u{1b}\\",
            "\u{9d}0;title\u{9c}",
            "\u{1b}Pprivate\u{1b}\\",
            "\u{1b}Xprivate\u{1b}\\",
            "\u{1b}^private\u{1b}\\",
            "\u{1b}_private\u{1b}\\",
            "\u{90}private\u{9c}",
            "\u{98}private\u{9c}",
            "\u{9e}private\u{9c}",
            "\u{9f}private\u{9c}",
            "\u{1b}(B",
            "\u{1b}",
            "\u{1b}\n",
            "\u{1b}\r\n",
            "\t",
        ] {
            for (secret, marker) in [
                (
                    format!("s{sequence}k-abcdefghijklmnopqrstuvwxyz123456"),
                    "[REDACTED TOKEN]",
                ),
                (
                    format!(
                        "-----BE{sequence}GIN PRIVATE KEY-----\nbody\n-----END PRIVATE KEY-----"
                    ),
                    "[REDACTED PRIVATE KEY]",
                ),
            ] {
                let raw = format!("👩‍💻 before\n{secret}\nمی‌خواهم after");
                assert_eq!(
                    SafeText::from_untrusted(&raw, 2000).text,
                    format!("👩‍💻 before\n{marker}\nمی‌خواهم after"),
                    "{sequence:?} in {secret:?}"
                );
            }
        }
    }

    #[test]
    fn nonsecret_unicode_and_coloured_cli_output_keep_their_display_text() {
        for text in ["👨‍👩‍👧‍👦 🇺🇸 🏳️‍🌈", "می‌خواهم", "❤️ ✈︎ ✈️"]
        {
            assert_eq!(SafeText::from_untrusted(text, 2000).text, text);
        }
        assert_eq!(
            SafeText::from_untrusted("\u{1b}[32mReady\u{1b}[0m\t2 passed", 2000).text,
            "[32mReady[0m\t2 passed"
        );
    }

    #[test]
    fn legitimate_unicode_round_trips_through_safe_text_and_persona_display() {
        for text in [
            "👨‍👩‍👧",
            "❤️",
            "می‌خواهم",
            "क्‍ष क्‌ष",
            "안녕하세요 ㅤ",
            "Builds UI ❤️ with 👩‍💻, writes می‌خواهم",
        ] {
            assert_eq!(SafeText::from_untrusted(text, 2000).text, text);
            assert_eq!(crate::persona::displayed(Some(text), 2000), text);
            assert_eq!(
                SafeText::from_untrusted(
                    &format!("{text} sk-abcdefghijklmnopqrstuvwxyz123456"),
                    2000
                )
                .text,
                format!("{text} [REDACTED TOKEN]")
            );
        }
    }

    #[test]
    fn original_tabbed_env_names_keep_their_values_redacted() {
        for name in ["API\tKEY", "PRIVATE\tKEY"] {
            let raw = format!("{name}=hunter2-plain-secret ghp_\tabcdefghijklmnopqrstuvwxyz123456");
            assert_eq!(
                SafeText::from_untrusted(&raw, 2000).text,
                format!("{name}=[REDACTED ENV VALUE]")
            );
        }
    }

    #[test]
    fn projected_tokens_cover_the_whole_secret_and_preserve_surrounding_joiners() {
        for (raw, expected) in [
            (
                "sk-abcdefghijklmnopqrstuvw\u{200d}SECRET_TAIL_123456",
                "[REDACTED TOKEN]",
            ),
            (
                "sk-abcdefghijklmnopqrstgithub_pat_abcdefghijklmnopqrst",
                "[REDACTED TOKEN]",
            ),
            (
                "xoxb-abcdefghijklmnopqrstgithub_pat_abcdefghijklmnopqrst",
                "[REDACTED TOKEN]",
            ),
            (
                "AKIAABCDEFGHIJKLMNOPsk-abcdefghijklmnopqrst",
                "[REDACTED AWS ACCESS KEY][REDACTED TOKEN]",
            ),
            (
                "AKIAABCDEFGHIJKLMNOP\u{200d}safe_tail",
                "[REDACTED AWS ACCESS KEY]\u{200d}safe_tail",
            ),
            (
                "é👩‍💻 foo\u{200d}bar_sk-abcdefghijklmnopqrstuvw",
                "é👩‍💻 foo\u{200d}bar_[REDACTED TOKEN]",
            ),
            ("foo\u{200d}bar\tname\nnext", "foo\u{200d}bar\tname\nnext"),
            ("s\0k-abcdefghijklmnopqrstuvw", "[REDACTED TOKEN]"),
        ] {
            assert_eq!(
                SafeText::from_untrusted(raw, 2000).text,
                expected,
                "{raw:?}"
            );
        }
    }

    #[test]
    fn env_policy_uses_both_unmasked_names_and_deduplicates_empty_values() {
        for (raw, expected) in [
            (
                "sk-abcdefghijklmnopqrstTOKEN=hunter2-plain-secret",
                "[REDACTED TOKEN]=[REDACTED ENV VALUE]",
            ),
            (
                "ghp_abcdefghijklmnopqrst=visible",
                "[REDACTED TOKEN]=visible",
            ),
            ("TOK\tEN=hunter2", "TOK\tEN=[REDACTED ENV VALUE]"),
            (
                "TOK\u{200d}EN=hunter2",
                "TOK\u{200d}EN=[REDACTED ENV VALUE]",
            ),
            ("API\u{200b}KEY=hunter2", "APIKEY=[REDACTED ENV VALUE]"),
            ("SECRET=", "SECRET=[REDACTED ENV VALUE]"),
            ("API\tKEY=", "API\tKEY=[REDACTED ENV VALUE]"),
            (
                "API\tKEY=hunter2\n  name\tvalue",
                "API\tKEY=[REDACTED ENV VALUE]\n  name\tvalue",
            ),
            (
                "[REDACTED TOKEN]=hunter2",
                "[REDACTED TOKEN]=[REDACTED ENV VALUE]",
            ),
            (
                "plain[REDACTED ENV VALUE]=visible",
                "plain[REDACTED ENV VALUE]=visible",
            ),
        ] {
            assert_eq!(
                SafeText::from_untrusted(raw, 2000).text,
                expected,
                "{raw:?}"
            );
        }
    }

    #[test]
    fn private_key_witnesses_preserve_block_state_without_reading_literal_markers() {
        for raw in [
            "before\n-----BEGIN PRIVATE KEY-----\nbody\n-----END PRIVATE KEY-----\nafter",
            "before\n-----BE\tGIN PRIVATE KEY-----\nbody\n-----E\tND PRIVATE KEY-----\nafter",
            "before\n-----BE\tGIN PRIVATE K\tEY-----\nbody\n-----END PRIVATE K\tEY-----\nafter",
            "before\n-----BEGIN\u{200b} PRIVATE KEY-----\nbody\n-----END PRIVATE KEY-----\nafter",
            "before\n-----BEGIN PRIVATE KEY----- -----END PRIVATE KEY-----\nafter",
            "before\n-----BE\tGIN PRIVATE KEY----- -----E\tND PRIVATE KEY-----\nafter",
            "before\n-----BEGIN PRIVATE KEY-----\nbody\n-----BEGIN PRIVATE KEY-----\nmore\n-----END PRIVATE KEY-----\nafter",
            "before\n-----BE\tGIN PRIVATE KEY-----\nbody\n-----BE\tGIN PRIVATE KEY-----\nmore\n-----E\tND PRIVATE KEY-----\nafter",
        ] {
            assert_eq!(
                SafeText::from_untrusted(raw, 2000).text,
                "before\n[REDACTED PRIVATE KEY]\nafter",
                "{raw:?}"
            );
        }
        for raw in [
            "before\n-----BEGIN PRIVATE KEY-----\nbody\n",
            "before\n-----BE\tGIN PRIVATE KEY-----\nbody\n",
            "before\n-----BE\tGIN PRIVATE KEY-----\nSECRET=",
            "before\n-----BEGIN PRIVATE KEY-----\nbody\n-----E\tND PRIVATE KEY-----\nafter",
        ] {
            // The original unclosed block's coverage survives a canonical close.
            assert_eq!(
                SafeText::from_untrusted(raw, 2000).text,
                "before\n[REDACTED PRIVATE KEY]",
                "{raw:?}"
            );
        }
    }

    #[test]
    fn literal_markers_and_nonsecret_whitespace_keep_their_display_text() {
        for text in [
            "[REDACTED TOKEN]",
            "[REDACTED AWS ACCESS KEY]",
            "[REDACTED ENV VALUE]",
            "[REDACTED PRIVATE KEY]",
            "[REDACTED PRIVATE KEY]\nvisible after a literal marker",
            "  first\tcolumn\n\tsecond\tcolumn\nlast",
            "a\u{3164}b foo\u{200d}bar ❤️",
        ] {
            assert_eq!(SafeText::from_untrusted(text, 2000).text, text);
        }
        assert_eq!(
            SafeText::from_untrusted("  first\tcolumn\r\n\tsecond\tcolumn\r\nlast\r", 2000).text,
            "  first\tcolumn\n\tsecond\tcolumn\nlast"
        );
        assert_eq!(SafeText::from_untrusted("a\u{1b}\nZ", 2000).text, "a\nZ");
        assert_eq!(SafeText::from_untrusted("a\u{1b}\r\nZ", 2000).text, "a\nZ");
        assert_eq!(
            SafeText::from_untrusted("one\ntwo\n", 2000).text,
            "one\ntwo"
        );
    }
}
