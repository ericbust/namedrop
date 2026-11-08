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
use std::{
    fs,
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    thread,
    time::{Duration, Instant},
};

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

/// The bound `complete` as the Stop runs it: the same operation, recording
/// Namedrop's own no-reply so a later explicit reply can still follow up.
/// The CLI has no command for it.
pub const STOP_NO_REPLY: &str = "complete.final-reply";

/// What the hook promised for one delivery. It is the only thing the Stop
/// may do with it.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Armed {
    /// The final message becomes the bound reply. `template` is that reply's
    /// bound input, `<reply body>` placeholder included, so every rewrite the
    /// envelope made (reminder target, remote label, pre-bound outcome) holds.
    Reply { template: Value, tracked: bool },
    /// Namedrop records no reply; the final message is for the person, and
    /// for a delivery the person sent it goes back to their thread.
    NoReply(NoReplyCase),
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum NoReplyCase {
    /// The delivery answers a message this address wrote.
    AnswersOwn,
    ReportToHuman,
    ResultSet,
    /// The person sent it: the final message is posted in its thread, in
    /// reply to their message.
    Relayed,
}

/// Why a delivery keeps today's explicit commands.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Explicit {
    Kind(DeliveryKind),
    /// The person's draft rode in with the envelope, and the agent must
    /// also tell the person about it.
    LeftoverInput,
    /// The explicit block shows a line besides `reply` and
    /// `complete.no-reply`, such as an attached context read or a review's
    /// results, that the delivery asks the agent to run.
    OtherLine,
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
    /// The explicit block would show a line besides `reply` and
    /// `complete.no-reply`.
    pub other_line: bool,
    /// The delivered message answers one this address wrote.
    pub answers_own: bool,
    /// The person sent the delivered message, from their own address or an
    /// envoy of it.
    pub relayed: bool,
    pub leftover: bool,
}

/// The measured rule: a message from the person gets the final message back
/// in its thread, a tracked worker replies, a delivery that answers the
/// recipient's own message closes without one, and anything else replies,
/// because a wrong no-reply strands a sender while a wrong reply costs one
/// extra message.
pub(crate) fn plan(facts: &DeliveryFacts<'_>) -> Plan {
    if facts.leftover {
        return Plan::Explicit(Explicit::LeftoverInput);
    }
    if facts.other_line {
        return Plan::Explicit(Explicit::OtherLine);
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
        (DeliveryKind::Settle | DeliveryKind::Channel | DeliveryKind::DirectMessage, Some(_))
            if facts.relayed =>
        {
            Plan::Armed(Armed::NoReply(NoReplyCase::Relayed))
        }
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

/// The claim one authenticated delivery arms, or `None` when it keeps the
/// explicit commands: by its kind, or because the prompt's session is not the
/// one the attempt is pinned to.
pub(crate) fn claim_for(
    delivery: &crate::integration::AuthenticatedDelivery,
    session: &str,
    leftover: bool,
    turn: &HostTurn,
    now_ms: u64,
) -> Option<Claim> {
    if delivery.agent_session_id.as_str() != session {
        return None;
    }
    let control = &delivery.envelope["control"];
    let bound = |token: &str| {
        control["tool"]["operations"]
            .as_array()?
            .iter()
            .find(|operation| operation["token"] == token)
            .map(|operation| &operation["input"])
    };
    let Plan::Armed(plan) = plan(&DeliveryFacts {
        kind: delivery.kind,
        reply: bound("reply"),
        no_reply: bound("complete.no-reply").is_some(),
        other_line: crate::integration::line_tokens(control)?
            .iter()
            .any(|token| !matches!(*token, "reply" | "complete.no-reply")),
        answers_own: delivery.answers_own,
        relayed: delivery.relayed,
        leftover,
    }) else {
        return None;
    };
    let located = |key: &str| {
        control["tool"]["environment"][key]
            .as_str()
            .map(str::to_owned)
    };
    Some(Claim {
        attempt_id: crate::DeliveryAttemptId::new(control["attempt_id"].as_str()?),
        namespace: delivery.namespace.clone(),
        location: Location {
            state: located("HERDR_PLUGIN_STATE_DIR")?,
            socket: located("HERDR_SOCKET_PATH")?,
            workspace: located("HERDR_WORKSPACE_ID")?,
            pane: located("HERDR_PANE_ID")?,
        },
        turn: turn.clone(),
        plan,
        state: ClaimState::Open,
        armed_at_ms: now_ms,
        updated_at_ms: now_ms,
        reply_line: crate::integration::rendered_line(control, "reply"),
    })
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
    /// The turn this claim waits for.
    pub turn: HostTurn,
    pub plan: Armed,
    pub state: ClaimState,
    pub armed_at_ms: u64,
    pub updated_at_ms: u64,
    /// The delivery's bound reply line, shown again while a held claim's
    /// delivery stays open.
    #[serde(default)]
    pub reply_line: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ClaimState {
    Open,
    Done(Done),
    Left(LeaveReason),
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
    /// The turn ended with background work running or a helper still to
    /// answer, so its final message may not be the answer; the agent settles
    /// the delivery in a later turn.
    Held,
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
    pub disposition: Option<crate::DeliveryDisposition>,
    /// A sandboxed explicit reply or complete for the attempt is queued and
    /// has not committed yet.
    pub explicit_pending: bool,
    /// During the claim's turn the session asked a helper who has not
    /// answered yet, so its final message is a status, not the answer.
    pub waiting: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Verdict {
    Ignore,
    Leave(LeaveReason),
    Superseded,
    Settle(Settlement),
}

/// The bound operation that settles a claim, or the relay's post.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Settlement {
    Reply(Value),
    NoReply,
    /// The final message, trimmed, for the person's thread.
    Relay(String),
}

impl Settlement {
    pub(crate) fn done(&self) -> Done {
        match self {
            Self::Reply(_) => Done::Replied,
            Self::NoReply | Self::Relay(_) => Done::NoReplied,
        }
    }
}

/// One claim against one Stop of its session.
pub(crate) fn decide(claim: &Claim, stop: &StopFacts, settle: &SettleFacts) -> Verdict {
    match claim.state {
        ClaimState::Done(_) | ClaimState::Left(_) => Verdict::Ignore,
        ClaimState::Open if stop.turn.as_ref() != Some(&claim.turn) => {
            Verdict::Leave(LeaveReason::Interrupted)
        }
        ClaimState::Open if settle.explicit_pending || settle.disposition.is_some() => {
            Verdict::Superseded
        }
        ClaimState::Open => {
            let waiting = stop.background_running || settle.waiting;
            match &claim.plan {
                Armed::NoReply(NoReplyCase::Relayed) | Armed::Reply { .. } if waiting => {
                    Verdict::Leave(LeaveReason::Held)
                }
                Armed::NoReply(NoReplyCase::Relayed) => match stop.text.text.trim() {
                    "" => Verdict::Leave(LeaveReason::NoFinalText),
                    text => Verdict::Settle(Settlement::Relay(text.to_owned())),
                },
                Armed::NoReply(_) => Verdict::Settle(Settlement::NoReply),
                Armed::Reply { template, tracked } => {
                    match reply_input(template, *tracked, &stop.text) {
                        Ok(input) => Verdict::Settle(Settlement::Reply(input)),
                        Err(reason) => Verdict::Leave(reason),
                    }
                }
            }
        }
    }
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

/// How long a finished or abandoned claim is kept, for anyone checking what
/// a Stop did.
const KEEP_MS: u64 = 24 * 60 * 60 * 1000;
const MAX_CLAIMS_BYTES: u64 = 1024 * 1024;
/// A prompt hook that cannot take the lock arms nothing, so the agent gets
/// today's explicit commands; it never waits on a Stop that is settling.
const PROMPT_LOCK_WAIT: Duration = Duration::from_millis(500);
const STOP_LOCK_WAIT: Duration = Duration::from_secs(2);

/// The claims of one agent session, in one file only this module writes.
struct Claims {
    directory: PathBuf,
    claims: Vec<Claim>,
    _lock: fs::File,
}

fn session_directory(state: &Path, socket: &str, session: &str) -> PathBuf {
    let key = serde_json::to_vec(&(socket, session)).unwrap_or_default();
    state
        .join("final-reply")
        .join(crate::lower_hex(&Sha256::digest(key)))
}

fn read_claims(directory: &Path) -> Vec<Claim> {
    let Ok(file) = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join("claims.json"))
    else {
        return Vec::new();
    };
    let mut bytes = Vec::new();
    if !file.metadata().is_ok_and(|metadata| metadata.is_file())
        || file.take(MAX_CLAIMS_BYTES).read_to_end(&mut bytes).is_err()
    {
        return Vec::new();
    }
    serde_json::from_slice(&bytes).unwrap_or_default()
}

impl Claims {
    fn lock(directory: PathBuf, wait: Duration) -> Option<Self> {
        fs::create_dir_all(&directory).ok()?;
        let lock = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(directory.join("lock"))
            .ok()?;
        let until = Instant::now() + wait;
        while lock.try_lock().is_err() {
            if Instant::now() >= until {
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
        Some(Self {
            claims: read_claims(&directory),
            directory,
            _lock: lock,
        })
    }

    fn save(mut self, now_ms: u64) {
        self.claims
            .retain(|claim| claim.updated_at_ms.saturating_add(KEEP_MS) > now_ms);
        let next = self.directory.join("claims.next");
        let written = fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&next)
            .and_then(|mut file| {
                file.write_all(&serde_json::to_vec_pretty(&self.claims)?)?;
                file.sync_all()
            });
        if written.is_ok() {
            let _ = fs::rename(next, self.directory.join("claims.json"));
        }
    }
}

impl Claim {
    fn set(&mut self, state: ClaimState, now_ms: u64) {
        self.state = state;
        self.updated_at_ms = now_ms;
    }
}

/// What one prompt found in its session's claims.
#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct Observed {
    /// Whether `arm` was written: the only thing that decides what the hook
    /// tells the agent about this prompt's delivery.
    pub armed: bool,
    /// The bound reply line of each delivery a Stop held that is still open,
    /// so the agent can answer it in its thread.
    pub held: Vec<(crate::DeliveryAttemptId, String)>,
}

/// Runs for every prompt of a session that names its turn. It ends claims
/// that lost their turn, arms `arm` when the session holds no other open
/// claim, and finishes held claims whose deliveries were settled since.
pub(crate) fn observe_prompt(
    state: &Path,
    socket: &str,
    session: &str,
    turn: &HostTurn,
    arm: Option<Claim>,
    now_ms: u64,
) -> Observed {
    let directory = session_directory(state, socket, session);
    if arm.is_none() && !directory.join("claims.json").exists() {
        return Observed::default();
    }
    let Some(mut claims) = Claims::lock(directory, PROMPT_LOCK_WAIT) else {
        return Observed::default();
    };
    for claim in &mut claims.claims {
        if claim.state == ClaimState::Open && claim.turn != *turn {
            claim.set(ClaimState::Left(LeaveReason::Interrupted), now_ms);
        }
    }
    let open = claims
        .claims
        .iter()
        .any(|claim| claim.state == ClaimState::Open);
    let armed = match arm {
        Some(arm) if !open => {
            claims
                .claims
                .retain(|claim| claim.attempt_id != arm.attempt_id);
            claims.claims.push(arm);
            true
        }
        _ => false,
    };
    let mut held = Vec::new();
    for claim in &mut claims.claims {
        if claim.state != ClaimState::Left(LeaveReason::Held) {
            continue;
        }
        match (disposition(state, claim), &claim.reply_line) {
            (Some(_), _) => claim.set(ClaimState::Done(Done::Superseded), now_ms),
            (None, Some(line)) => held.push((claim.attempt_id.clone(), line.clone())),
            (None, None) => {}
        }
    }
    claims.save(now_ms);
    Observed { armed, held }
}

/// Whether a Stop of this session has anything to settle. It reads without
/// the lock, so a Stop with nothing to settle starts no worker.
pub fn has_live_claims(state: &Path, socket: &str, session: &str) -> bool {
    read_claims(&session_directory(state, socket, session))
        .iter()
        .any(|claim| claim.state == ClaimState::Open)
}

/// The bound operation the Stop runs for a claim: its token, its input, and
/// the location flags of the envelope's own tool.
pub struct BoundOperation {
    pub operation: &'static str,
    pub input: Value,
    pub state: PathBuf,
    pub socket: String,
    pub workspace: String,
    pub pane: String,
}

/// Decides every live claim of the stopping session and runs each settle
/// through `run`, the same pipeline the CLI runs, so every guard of `reply`
/// and `complete` applies. A failed settle leaves its delivery pending.
pub fn settle_at_stop(
    state: &Path,
    socket: &str,
    stop: &StopFacts,
    now_ms: u64,
    run: &mut dyn FnMut(BoundOperation) -> Result<Value, String>,
) {
    let directory = session_directory(state, socket, &stop.session);
    let Some(mut claims) = Claims::lock(directory, STOP_LOCK_WAIT) else {
        return;
    };
    for claim in &mut claims.claims {
        let current = claim.state == ClaimState::Open && stop.turn.as_ref() == Some(&claim.turn);
        let facts = if current {
            settle_facts(state, &stop.session, claim, now_ms)
        } else {
            SettleFacts::default()
        };
        let next = match decide(claim, stop, &facts) {
            Verdict::Ignore => continue,
            Verdict::Leave(reason) => ClaimState::Left(reason),
            Verdict::Superseded => ClaimState::Done(Done::Superseded),
            Verdict::Settle(Settlement::Relay(text)) => {
                match relay(state, &stop.session, claim, &text, now_ms) {
                    // The relay recorded the no-reply. The Stop's bound
                    // no-reply finds it, changes nothing, and wakes the
                    // mailboxes as the agent's own line would.
                    Ok(Some(_)) => {
                        let _ = run(claim.bound(&Settlement::NoReply));
                        ClaimState::Done(Done::NoReplied)
                    }
                    Ok(None) => ClaimState::Done(Done::Superseded),
                    Err(_) => ClaimState::Left(LeaveReason::OperationFailed),
                }
            }
            Verdict::Settle(settlement) => match run(claim.bound(&settlement)) {
                Ok(_) => ClaimState::Done(settlement.done()),
                Err(_) if disposition(state, claim).is_some() => ClaimState::Done(Done::Superseded),
                Err(_) => ClaimState::Left(LeaveReason::OperationFailed),
            },
        };
        claim.set(next, now_ms);
    }
    claims.save(now_ms);
}

impl Claim {
    /// The CLI operation for `settlement`, at the envelope's own location.
    fn bound(&self, settlement: &Settlement) -> BoundOperation {
        let (operation, input) = match settlement {
            Settlement::Reply(input) => ("reply", input.clone()),
            Settlement::NoReply | Settlement::Relay(_) => (
                STOP_NO_REPLY,
                serde_json::json!({"attempt_id": self.attempt_id}),
            ),
        };
        BoundOperation {
            operation,
            input,
            state: PathBuf::from(&self.location.state),
            socket: self.location.socket.clone(),
            workspace: self.location.workspace.clone(),
            pane: self.location.pane.clone(),
        }
    }
}

fn settle_facts(state: &Path, session: &str, claim: &Claim, now_ms: u64) -> SettleFacts {
    let (disposition, waiting) = crate::StateStore::open_read_only(state)
        .ok()
        .flatten()
        .and_then(|mut store| {
            store
                .transaction(|transaction| {
                    Ok((
                        transaction.delivery_disposition(&claim.namespace, &claim.attempt_id)?,
                        transaction.awaits_helper(
                            &claim.namespace,
                            &claim.attempt_id,
                            claim.armed_at_ms,
                            now_ms,
                        )?,
                    ))
                })
                .ok()
        })
        .unwrap_or_default();
    SettleFacts {
        disposition,
        explicit_pending: crate::deferred_commands::settle_queued(
            state,
            &claim.namespace.session_socket_id,
            session,
            claim.attempt_id.as_str(),
        ),
        waiting,
    }
}

fn disposition(state: &Path, claim: &Claim) -> Option<crate::DeliveryDisposition> {
    crate::StateStore::open_read_only(state)
        .ok()
        .flatten()
        .and_then(|mut store| {
            store
                .transaction(|transaction| {
                    transaction.delivery_disposition(&claim.namespace, &claim.attempt_id)
                })
                .ok()
        })
        .flatten()
}

/// Posts a Relayed claim's final message through the terminal relay.
fn relay(
    state: &Path,
    session: &str,
    claim: &Claim,
    text: &str,
    now_ms: u64,
) -> Result<Option<crate::MessageId>, crate::StoreError> {
    crate::StateStore::open(state)?.transaction(|transaction| {
        transaction.relay_final_message(
            &claim.namespace,
            &crate::TerminalRelayEnd {
                agent_session_id: crate::AgentSessionId::new(session),
                attempt_id: claim.attempt_id.clone(),
                turn_started_at_ms: claim.armed_at_ms,
                ended_at_ms: now_ms,
                text: text.to_owned(),
            },
            &crate::MutationAttribution::new(crate::ActorKind::Plugin, "namedrop", now_ms),
        )
    })
}

#[cfg(test)]
#[path = "final_reply_tests.rs"]
mod tests;
