//! Settling a delivery from the agent's final message.
//!
//! The UserPromptSubmit hook arms a claim when it tells the agent that its
//! final message settles the delivery, and the Stop hook settles only armed
//! claims, only with the text of the turn the claim was armed on. Without a
//! claim nothing is automatic: the delivery stays pending and recoverable.
use crate::integration::DeliveryKind;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// One host turn, by the id both of its hooks carry: Claude Code's
/// `prompt_id` or Codex's `turn_id`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub struct HostTurn(String);

impl HostTurn {
    /// `None` unless exactly one of the two ids is present and non-blank.
    pub fn from_ids(prompt_id: Option<&str>, turn_id: Option<&str>) -> Option<Self> {
        let identity = match (prompt_id, turn_id) {
            (Some(id), None) if !id.trim().is_empty() => format!("claude:{id}"),
            (None, Some(id)) if !id.trim().is_empty() => format!("codex:{id}"),
            _ => return None,
        };
        Some(Self(crate::lower_hex(&Sha256::digest(identity))))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The text a turn ended with, already cut to the relay limit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FinalText {
    pub text: String,
    pub truncated: bool,
}

impl FinalText {
    pub fn bounded(text: &str) -> Self {
        let mut chars = text.chars();
        let text: String = chars
            .by_ref()
            .take(crate::TERMINAL_RELAY_MAX_CHARS)
            .collect();
        Self {
            text,
            truncated: chars.next().is_some(),
        }
    }
}

const CUT_NOTICE: &str = "[Cut at 16,000 characters by Namedrop.]";

/// What the hook promised for one delivery. It is the only thing the Stop
/// may do with it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Armed {
    /// The final message becomes the bound reply. `template` is that reply's
    /// bound input, `<reply body>` placeholder included, so every rewrite the
    /// envelope made (reminder target, remote label, pre-bound outcome) holds.
    Reply { template: Value, tracked: bool },
    /// Namedrop records no reply; the final message is for the person.
    NoReply(NoReplyCase),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoReplyCase {
    /// The delivery answers a message this address wrote.
    AnswersOwn,
    ReportToHuman,
    ResultSet,
}

/// Why a delivery keeps today's explicit commands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Explicit {
    Kind(DeliveryKind),
    /// The person's draft rode in with the envelope, and the agent must
    /// also tell the person about it.
    LeftoverInput,
    /// The envelope binds no reply line with a body placeholder, or no
    /// complete.no-reply line.
    NoBoundLine,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Plan {
    Explicit(Explicit),
    Armed(Armed),
}

/// What the hook knows about one authenticated delivery.
pub(crate) struct DeliveryFacts<'a> {
    pub kind: DeliveryKind,
    /// The bound `reply` input, when the envelope permits one.
    pub reply: Option<&'a Value>,
    pub no_reply: bool,
    /// The delivered message answers one this address wrote.
    pub answers_own: bool,
    pub leftover: bool,
}

/// The measured rule: a tracked worker replies, a delivery that answers the
/// recipient's own message closes without one, and anything else replies,
/// because a wrong no-reply strands a sender while a wrong reply costs one
/// extra message.
pub(crate) fn plan(facts: &DeliveryFacts<'_>) -> Plan {
    if facts.leftover {
        return Plan::Explicit(Explicit::LeftoverInput);
    }
    if !facts.no_reply {
        return Plan::Explicit(Explicit::NoBoundLine);
    }
    let reply = facts.reply.filter(|reply| {
        reply["source"]
            .as_str()
            .is_some_and(|source| source.ends_with(crate::REPLY_BODY_PLACEHOLDER))
    });
    match (facts.kind, reply) {
        (DeliveryKind::ReportToHuman, _) => Plan::Armed(Armed::NoReply(NoReplyCase::ReportToHuman)),
        (DeliveryKind::ResultSet, _) => Plan::Armed(Armed::NoReply(NoReplyCase::ResultSet)),
        (DeliveryKind::Settle, Some(template)) => {
            let tracked = template.get("work_outcome").is_some();
            Plan::Armed(if !tracked && facts.answers_own {
                Armed::NoReply(NoReplyCase::AnswersOwn)
            } else {
                Armed::Reply {
                    template: template.clone(),
                    tracked,
                }
            })
        }
        (DeliveryKind::Settle, None) => Plan::Explicit(Explicit::NoBoundLine),
        (kind, _) => Plan::Explicit(Explicit::Kind(kind)),
    }
}

/// Where the bound operations of one delivery run: the envelope's own tool
/// location, so the Stop runs exactly the line the agent was shown.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Location {
    pub state: String,
    pub socket: String,
    pub workspace: String,
    pub pane: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Claim {
    pub attempt_id: crate::DeliveryAttemptId,
    pub namespace: crate::StateNamespace,
    pub location: Location,
    /// The turn this claim waits for. A carried claim takes the turn of the
    /// continuation that re-opens it.
    pub turn: HostTurn,
    pub plan: Armed,
    pub state: ClaimState,
    pub armed_at_ms: u64,
    pub updated_at_ms: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClaimState {
    Open,
    /// The turn ended with background work running; the continuation that
    /// reports it decides.
    Carried,
    Done(Done),
    Left(LeaveReason),
}

impl ClaimState {
    pub(crate) fn is_live(self) -> bool {
        matches!(self, Self::Open | Self::Carried)
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Done {
    Replied,
    NoReplied,
    /// The agent settled it with an explicit command.
    Superseded,
}

/// Why a claim ended without settling; the delivery stays pending.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeaveReason {
    /// Another turn began or stopped before this claim's turn stopped. Neither
    /// host runs Stop for an interrupted turn.
    Interrupted,
    NoFinalText,
    /// The final message leads with Mentions other than the addressee.
    AddressesOthers,
    OperationFailed,
}

/// What the Stop hook knows about the turn that ended.
pub struct StopFacts {
    pub session: String,
    pub turn: Option<HostTurn>,
    /// Claude Code reported background work still running or pending.
    pub background_running: bool,
    pub text: FinalText,
}

/// What durable state says about the claim's attempt when the Stop decides.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct SettleFacts {
    pub disposed: bool,
    /// A sandboxed explicit reply or complete for the attempt is queued and
    /// has not committed yet.
    pub explicit_pending: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Verdict {
    Ignore,
    Carry,
    Leave(LeaveReason),
    Superseded,
    Settle(Settlement),
}

/// The bound operation that settles a claim.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Settlement {
    Reply(Value),
    NoReply,
}

impl Settlement {
    pub(crate) fn operation(&self, attempt_id: &crate::DeliveryAttemptId) -> (&'static str, Value) {
        match self {
            Self::Reply(input) => ("reply", input.clone()),
            Self::NoReply => ("complete", serde_json::json!({"attempt_id": attempt_id})),
        }
    }

    pub(crate) fn done(&self) -> Done {
        match self {
            Self::Reply(_) => Done::Replied,
            Self::NoReply => Done::NoReplied,
        }
    }
}

/// One claim against one Stop of its session.
pub(crate) fn decide(claim: &Claim, stop: &StopFacts, settle: &SettleFacts) -> Verdict {
    match claim.state {
        ClaimState::Done(_) | ClaimState::Left(_) | ClaimState::Carried => Verdict::Ignore,
        ClaimState::Open if stop.turn.as_ref() != Some(&claim.turn) => {
            Verdict::Leave(LeaveReason::Interrupted)
        }
        ClaimState::Open if settle.disposed || settle.explicit_pending => Verdict::Superseded,
        ClaimState::Open => match &claim.plan {
            Armed::NoReply(_) => Verdict::Settle(Settlement::NoReply),
            Armed::Reply { .. } if stop.background_running => Verdict::Carry,
            Armed::Reply { template, tracked } => {
                match reply_input(template, *tracked, &stop.text) {
                    Ok(input) => Verdict::Settle(Settlement::Reply(input)),
                    Err(reason) => Verdict::Leave(reason),
                }
            }
        },
    }
}

/// The new state for a claim when another prompt of its session begins on
/// `turn`, or `None` when it stays. A claim still open on an earlier turn lost
/// that turn; a carried claim waits for the continuation that reports its
/// background work, and only that prompt re-opens it.
pub(crate) fn on_prompt(
    state: ClaimState,
    claim_turn: &HostTurn,
    turn: &HostTurn,
    continuation: bool,
) -> Option<ClaimState> {
    match state {
        ClaimState::Open if claim_turn != turn => Some(ClaimState::Left(LeaveReason::Interrupted)),
        ClaimState::Carried if continuation => Some(ClaimState::Open),
        _ => None,
    }
}

/// A Claude Code background task reports back as a prompt of its own.
pub(crate) fn is_continuation(prompt: &str) -> bool {
    prompt.starts_with("<task-notification>")
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum TrackedOutcome {
    Completed,
    NeedsContinuation,
    Blocked { reason: String },
}

/// Splits a strict closing line off a tracked worker's message:
/// `work_outcome: completed`, `work_outcome: needs_continuation` or
/// `work_outcome: blocked: <reason>`. Without one the body stays as written and
/// the outcome is the one the bound line pre-binds, needs_continuation.
pub(crate) fn split_outcome_trailer(body: &str) -> (&str, TrackedOutcome) {
    let body = body.trim_end();
    let (head, last) = body.rsplit_once('\n').unwrap_or(("", body));
    let outcome = match last.trim().strip_prefix("work_outcome: ") {
        Some("completed") => Some(TrackedOutcome::Completed),
        Some("needs_continuation") => Some(TrackedOutcome::NeedsContinuation),
        Some(rest) => rest
            .strip_prefix("blocked: ")
            .map(str::trim)
            .filter(|reason| !reason.is_empty())
            .map(|reason| TrackedOutcome::Blocked {
                reason: reason.to_owned(),
            }),
        None => None,
    };
    match outcome {
        Some(outcome) => (head.trim_end(), outcome),
        None => (body, TrackedOutcome::NeedsContinuation),
    }
}

/// The bound reply input that carries `text`. A leading Mention of the
/// addressee is dropped by the reply's own grammar, so it is allowed; a
/// leading Mention of anyone else would add a recipient, so it is refused.
fn reply_input(template: &Value, tracked: bool, text: &FinalText) -> Result<Value, LeaveReason> {
    let source = template["source"].as_str().unwrap_or_default();
    let (body, outcome) = if tracked {
        let (body, outcome) = split_outcome_trailer(&text.text);
        (body, Some(outcome))
    } else {
        (text.text.as_str(), None)
    };
    let body = body.trim();
    if body.is_empty() {
        return Err(LeaveReason::NoFinalText);
    }
    let body = if text.truncated {
        format!("{body}\n\n{CUT_NOTICE}")
    } else {
        body.to_owned()
    };
    let recipients = |source: &str| crate::parse_mentions(source).ok().map(|m| m.recipients);
    let filled = source.replacen(crate::REPLY_BODY_PLACEHOLDER, &body, 1);
    let expected = recipients(&source.replacen(crate::REPLY_BODY_PLACEHOLDER, "body", 1));
    if expected.is_none() || recipients(&filled) != expected {
        return Err(LeaveReason::AddressesOthers);
    }
    let mut input = template.clone();
    input["source"] = Value::String(filled);
    match outcome {
        Some(TrackedOutcome::Completed) => input["work_outcome"] = "completed".into(),
        Some(TrackedOutcome::NeedsContinuation) => {
            input["work_outcome"] = "needs_continuation".into();
        }
        Some(TrackedOutcome::Blocked { reason }) => {
            input["work_outcome"] = "blocked".into();
            input["blocked_reason"] = reason.into();
        }
        None => {}
    }
    Ok(input)
}

#[cfg(test)]
#[path = "final_reply_tests.rs"]
mod tests;
