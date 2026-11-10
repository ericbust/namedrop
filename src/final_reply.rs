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
    io::{Read, Seek, Write},
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
        no_reply_line: crate::integration::rendered_line(control, "complete.no-reply"),
        failure: None,
        awaits: Vec::new(),
        first_armed_at_ms: None,
        promised_turn: None,
    })
}

/// What one Claude Code turn's transcript records say about its background
/// work. A turn's records are those that carry its `promptId`, and those with
/// none that follow one of them before another turn's record.
#[derive(Debug, Default, Eq, PartialEq)]
pub struct TurnTasks {
    /// The tasks the turn started, or `None` when its first record is not in
    /// what was read, so a start may be missing.
    pub started: Option<Vec<String>>,
    /// The tasks whose notices reached the turn: as its own prompt, or queued
    /// into it while it ran.
    pub notified: Vec<String>,
}

/// Reads one turn from the end of a transcript. `whole` says the bytes start
/// at the transcript's beginning.
pub fn turn_tasks(transcript: &[u8], whole: bool, prompt_id: &str) -> TurnTasks {
    let mut turn = TurnTasks::default();
    let (mut started, mut found, mut earlier_turn) = (Vec::new(), false, whole);
    // Records with no `promptId` belong to the turn whose record came last.
    let mut in_turn = false;
    let own_key = format!(r#""promptId":"{prompt_id}""#);
    let (own_key, any_key) = (
        memchr::memmem::Finder::new(own_key.as_bytes()),
        memchr::memmem::Finder::new(br#""promptId":""#),
    );
    let queued = memchr::memmem::Finder::new(br#""queued_command""#);
    for line in transcript.split(|byte| *byte == b'\n') {
        // Only this turn's own records and the queued notices inside it are
        // parsed; any other line only says whose turn follows.
        let carries_id = any_key.find(line).is_some();
        let maybe_own = !prompt_id.is_empty() && own_key.find(line).is_some();
        if !maybe_own && !(in_turn && queued.find(line).is_some()) {
            if carries_id {
                in_turn = false;
                earlier_turn |= !found;
            }
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        let own = !prompt_id.is_empty() && record["promptId"] == prompt_id;
        if record["promptId"].is_string() {
            in_turn = own;
            earlier_turn |= !own && !found;
        }
        if !in_turn {
            continue;
        }
        found = true;
        let notice = match (
            record["type"].as_str(),
            record["attachment"]["type"].as_str(),
        ) {
            (Some("user"), _) if own => record["message"]["content"].as_str(),
            (Some("attachment"), Some("queued_command")) => record["attachment"]["prompt"].as_str(),
            _ => None,
        };
        turn.notified
            .extend(notice.and_then(notice_task).map(str::to_owned));
        if own {
            let result = &record["toolUseResult"];
            for key in ["backgroundTaskId", "agentId", "resumedAgentId", "taskId"] {
                started.extend(result[key].as_str().map(str::to_owned));
            }
        }
    }
    turn.started = (found && earlier_turn).then_some(started);
    turn
}

/// The task a Claude Code task notice is about: the `task-id` element that
/// opens it, never one quoted in its result.
pub(crate) fn notice_task(text: &str) -> Option<&str> {
    let (id, _) = text
        .trim_start()
        .strip_prefix("<task-notification>")?
        .trim_start()
        .strip_prefix("<task-id>")?
        .split_once("</task-id>")?;
    Some(id.trim()).filter(|id| !id.is_empty())
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
    /// The delivery's bound reply and no-reply lines, the way back shown
    /// while a claim that ended unsettled leaves its delivery open.
    #[serde(default)]
    pub reply_line: Option<String>,
    #[serde(default)]
    pub no_reply_line: Option<String>,
    /// The error that stopped the settle, for the agent's later prompts.
    #[serde(default)]
    pub failure: Option<String>,
    /// The background tasks a held claim waits for.
    #[serde(default)]
    pub awaits: Vec<String>,
    /// When a claim that a task's notice woke was first armed.
    #[serde(default)]
    pub first_armed_at_ms: Option<u64>,
    /// The turn whose prompt carried a notice of this held claim's work and
    /// so left out its way back. That turn's Stop wakes it, even when it can't
    /// read the notice itself.
    #[serde(default)]
    pub promised_turn: Option<HostTurn>,
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
    /// The bound operation failed or its worker died. Claims a release before
    /// 1.6.3 left because the final message led with other Mentions read as
    /// this, since one unknown variant makes `read_claims` drop the whole file.
    #[serde(alias = "addresses_others")]
    OperationFailed,
}

/// What the Stop hook knows about the turn that ended.
pub struct StopFacts {
    pub session: String,
    pub turn: Option<HostTurn>,
    /// The background tasks Claude Code listed as running or pending.
    pub running_tasks: Vec<String>,
    /// The tasks this turn put in the background, from its transcript.
    pub started_tasks: Vec<String>,
    /// The tasks whose notices reached this turn, from its transcript.
    pub notified_tasks: Vec<String>,
    pub text: FinalText,
}

impl StopFacts {
    /// The running work a claim's final message may be waiting for: what its
    /// turn started, and what it was already held for. Work started earlier,
    /// and Claude Code's own background tasks, never answer this delivery.
    fn awaited(&self, claim: &Claim) -> Vec<String> {
        self.running_tasks
            .iter()
            .filter(|id| self.started_tasks.contains(id) || claim.awaits.contains(id))
            .cloned()
            .collect()
    }
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
            let waiting = !stop.awaited(claim).is_empty() || settle.waiting;
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
                        Some(input) => Verdict::Settle(Settlement::Reply(input)),
                        None => Verdict::Leave(LeaveReason::NoFinalText),
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

/// The bound reply input that carries `text`, or `None` when it has no text.
/// However it opens, it reaches only the template's addressee.
fn reply_input(template: &Value, tracked: bool, text: &FinalText) -> Option<Value> {
    let source = template["source"].as_str().unwrap_or_default();
    let (body, outcome) = if tracked {
        let (body, outcome) = split_outcome_trailer(&text.text);
        (body, Some(outcome))
    } else {
        (text.text.as_str(), None)
    };
    let body = body.trim();
    if body.is_empty() {
        return None;
    }
    let body = if text.truncated {
        format!("{body}\n\n{CUT_NOTICE}")
    } else {
        body.to_owned()
    };
    let body = crate::message::literal_leading_mentions(&body);
    let mut input = template.clone();
    input["source"] = Value::String(source.replacen(crate::REPLY_BODY_PLACEHOLDER, &body, 1));
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
    Some(input)
}

/// How long a finished or abandoned claim is kept, for anyone checking what
/// a Stop did.
const KEEP_MS: u64 = 24 * 60 * 60 * 1000;
const MAX_CLAIMS_BYTES: u64 = 1024 * 1024;
/// A prompt hook that cannot take the lock arms nothing, so the agent gets
/// today's explicit commands; it never waits on a Stop that is settling.
const PROMPT_LOCK_WAIT: Duration = Duration::from_millis(500);
const STOP_LOCK_WAIT: Duration = Duration::from_secs(2);
/// How long after its Stop a claim's worker may still settle it: the
/// worker's lock wait and deadline, with room to spare. A claim still open
/// after that lost its worker.
const STOPPED_GRACE_MS: u64 = 30_000;
/// How many of a session's latest turns keep a record while a claim is live.
const RECENT_TURNS: usize = 8;
const WORKER_LOST: &str = "Namedrop's Stop worker ended before it settled this delivery";
const LOCK_MISSED: &str = "the Stop could not read this session's claims within 2 seconds";
const WORKER_TIMED_OUT: &str = "Namedrop's Stop worker ran out of its 10 seconds";

/// What a session's prompt hook saw of one turn: when its first prompt came,
/// and whether a prompt of it carried a delivery, armed or explicit.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct TurnSeen {
    started_at_ms: u64,
    delivery: bool,
}

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

fn stopped_at(directory: &Path, turn: &HostTurn) -> Option<u64> {
    let mut text = String::new();
    fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join("stops").join(turn.as_str()))
        .ok()?
        .take(32)
        .read_to_string(&mut text)
        .ok()?;
    text.trim().parse().ok()
}

/// Records that a Stop of `turn` is handing its claims to a worker, before
/// the worker starts, so the session's next prompt leaves them to it. It
/// takes no lock: the Stop hook must not wait on a worker that is settling.
pub fn mark_stopped(state: &Path, socket: &str, session: &str, turn: &HostTurn, now_ms: u64) {
    let directory = session_directory(state, socket, session).join("stops");
    if fs::create_dir_all(&directory).is_err() {
        return;
    }
    let next = directory.join(format!("{}.next", turn.as_str()));
    let written = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&next)
        .and_then(|mut file| file.write_all(now_ms.to_string().as_bytes()));
    if written.is_ok() {
        let _ = fs::rename(next, directory.join(turn.as_str()));
    }
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

    /// The held claim that notices of `tasks` reaching `turn` wake: the
    /// oldest still outstanding that waits for one of them. A turn that
    /// carried a delivery of its own wakes none, since its final message is
    /// about that delivery.
    fn woken(&self, state: &Path, turn: &HostTurn, tasks: &[String]) -> Option<usize> {
        self.oldest_held(state, turn, |claim| claim.waits_for_any(tasks))
    }

    /// The held claim a prompt of `turn` promised that turn's Stop.
    fn promised(&self, state: &Path, turn: &HostTurn) -> Option<usize> {
        self.oldest_held(state, turn, |claim| {
            claim.promised_turn.as_ref() == Some(turn)
        })
    }

    /// The oldest held claim still outstanding that `matches`, when `turn`
    /// carries no delivery of its own.
    fn oldest_held(
        &self,
        state: &Path,
        turn: &HostTurn,
        matches: impl Fn(&Claim) -> bool,
    ) -> Option<usize> {
        if self.claims.iter().any(|claim| claim.turn == *turn)
            || self.turn_seen(turn).is_some_and(|seen| seen.delivery)
        {
            return None;
        }
        self.claims
            .iter()
            .enumerate()
            .filter(|(_, claim)| {
                claim.state == ClaimState::Left(LeaveReason::Held)
                    && matches(claim)
                    && outstanding(state, claim) != Some(false)
            })
            .min_by_key(|(_, claim)| claim.helpers_since_ms())
            .map(|(index, _)| index)
    }

    /// When a Stop of `turn` handed its claims to a worker, if one did.
    fn stopped_at(&self, turn: &HostTurn) -> Option<u64> {
        stopped_at(&self.directory, turn)
    }

    /// Keeps the records of the session's latest turns only while a claim
    /// may still wake or settle on one of them.
    fn prune_turns(&self, now_ms: u64) {
        let Ok(entries) = fs::read_dir(self.directory.join("turns")) else {
            return;
        };
        let live = self.claims.iter().any(|claim| match claim.state {
            ClaimState::Open => true,
            ClaimState::Left(LeaveReason::Held) => !claim.awaits.is_empty(),
            _ => false,
        });
        let mut kept = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let seen = (!name.ends_with(".next"))
                .then(|| self.turn_seen(&HostTurn(name)))
                .flatten();
            match seen {
                Some(seen) if live && seen.started_at_ms.saturating_add(KEEP_MS) > now_ms => {
                    kept.push((seen.started_at_ms, entry.path()));
                }
                // A record being written is its prompt hook's own.
                None if live && entry.file_name().to_string_lossy().ends_with(".next") => {}
                _ => {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
        kept.sort_by_key(|(started_at_ms, _)| std::cmp::Reverse(*started_at_ms));
        for (_, path) in kept.into_iter().skip(RECENT_TURNS) {
            let _ = fs::remove_file(path);
        }
    }

    fn turn_seen(&self, turn: &HostTurn) -> Option<TurnSeen> {
        let mut bytes = Vec::new();
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(self.directory.join("turns").join(turn.as_str()))
            .ok()?
            .take(256)
            .read_to_end(&mut bytes)
            .ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Records a prompt of `turn`: the first one's time, and a delivery.
    fn see_turn(&self, turn: &HostTurn, delivery: bool, now_ms: u64) {
        let seen = match self.turn_seen(turn) {
            Some(seen) if seen.delivery || !delivery => return,
            Some(seen) => TurnSeen { delivery, ..seen },
            None => TurnSeen {
                started_at_ms: now_ms,
                delivery,
            },
        };
        let directory = self.directory.join("turns");
        let next = directory.join(format!("{}.next", turn.as_str()));
        let written = fs::create_dir_all(&directory).and_then(|()| {
            fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                .open(&next)?
                .write_all(&serde_json::to_vec(&seen)?)
        });
        if written.is_ok() {
            let _ = fs::rename(next, directory.join(turn.as_str()));
        }
    }

    /// What a claim armed on another turn becomes when this session moves
    /// on: still open while its Stop's worker may settle it, otherwise left.
    fn moved_on(&self, claim: &Claim, now_ms: u64) -> Option<ClaimState> {
        match self.stopped_at(&claim.turn) {
            Some(at) if now_ms < at.saturating_add(STOPPED_GRACE_MS) => None,
            Some(_) => Some(ClaimState::Left(LeaveReason::OperationFailed)),
            None => Some(ClaimState::Left(LeaveReason::Interrupted)),
        }
    }

    fn save(mut self, now_ms: u64) {
        self.claims
            .retain(|claim| claim.updated_at_ms.saturating_add(KEEP_MS) > now_ms);
        self.prune_turns(now_ms);
        if let Ok(entries) = fs::read_dir(self.directory.join("stops")) {
            for entry in entries.flatten() {
                if entry
                    .path()
                    .extension()
                    .is_some_and(|extension| extension == "next")
                {
                    continue;
                }
                let live = self.claims.iter().any(|claim| {
                    claim.state == ClaimState::Open
                        && entry.file_name().to_str() == Some(claim.turn.as_str())
                });
                if !live {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
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
    /// Since when a helper the session asked still owes this claim's answer.
    fn helpers_since_ms(&self) -> u64 {
        self.first_armed_at_ms.unwrap_or(self.armed_at_ms)
    }

    /// Moves a held claim to the turn a notice of its work reached, so that
    /// turn's final message is the answer.
    fn wake_on(&mut self, turn: HostTurn, at_ms: u64) {
        self.first_armed_at_ms.get_or_insert(self.armed_at_ms);
        self.turn = turn;
        self.armed_at_ms = at_ms;
        self.set(ClaimState::Open, at_ms);
    }

    fn waits_for_any(&self, tasks: &[impl AsRef<str>]) -> bool {
        tasks
            .iter()
            .any(|task| self.awaits.iter().any(|id| id == task.as_ref()))
    }

    fn set(&mut self, state: ClaimState, now_ms: u64) {
        self.state = state;
        self.updated_at_ms = now_ms;
    }

    /// Whether the final message was to answer the delivery, rather than
    /// Namedrop closing it without a reply.
    fn answers(&self) -> bool {
        matches!(
            self.plan,
            Armed::Reply { .. } | Armed::NoReply(NoReplyCase::Relayed)
        )
    }

    /// The line that settles this claim's delivery the way the hook said it
    /// would: the reply when the final message was to answer, otherwise the
    /// no-reply.
    fn way_back(&self) -> Option<WayBack> {
        let answers = self.answers();
        let line = if answers {
            &self.reply_line
        } else {
            &self.no_reply_line
        };
        Some(WayBack {
            attempt_id: self.attempt_id.clone(),
            line: line.clone()?,
            answers,
            failure: self.failure.clone(),
        })
    }

    /// Moves a claim its session left behind to `next`. One whose Stop worker
    /// never finished is a failed settle like any other.
    fn move_on(&mut self, state: &Path, next: ClaimState, now_ms: u64) {
        let next = if next == ClaimState::Left(LeaveReason::OperationFailed) {
            self.settle_failed(state, WORKER_LOST.to_owned(), now_ms)
        } else {
            next
        };
        self.set(next, now_ms);
    }

    /// Keeps the error that stopped the settle and warns the person.
    fn settle_failed(&mut self, state: &Path, failure: String, now_ms: u64) -> ClaimState {
        warn_settle_failed(state, self, &failure, now_ms);
        self.failure = Some(failure);
        ClaimState::Left(LeaveReason::OperationFailed)
    }
}

/// The claim still saves, and still tells the agent why, if this write fails.
fn warn_settle_failed(state: &Path, claim: &Claim, failure: &str, now_ms: u64) {
    if !matches!(crate::StateStore::open_read_only(state), Ok(Some(_))) {
        return;
    }
    let by = crate::MutationAttribution::new(crate::ActorKind::Plugin, "namedrop", now_ms);
    let _ = crate::StateStore::open(state).and_then(|mut store| {
        store.transaction(|transaction| {
            transaction.flag_failed_settle(
                &claim.namespace,
                &claim.attempt_id,
                if claim.answers() {
                    crate::store::LostSettle::FinalMessage
                } else {
                    crate::store::LostSettle::TurnEnd
                },
                failure,
                &by,
            )
        })
    });
}

/// Warns about the turn's claims when nothing can save them, reading without
/// the lock. A settle that still commits clears the warning.
fn warn_open_claims(
    state: &Path,
    socket: &str,
    session: &str,
    turn: Option<&HostTurn>,
    reason: &str,
    now_ms: u64,
) {
    read_claims(&session_directory(state, socket, session))
        .iter()
        .filter(|claim| claim.state == ClaimState::Open && Some(&claim.turn) == turn)
        .for_each(|claim| warn_settle_failed(state, claim, reason, now_ms));
}

/// For the Stop worker's deadline, which ends it with no chance to save.
pub fn warn_stop_timed_out(
    state: &Path,
    socket: &str,
    session: &str,
    turn: Option<&HostTurn>,
    now_ms: u64,
) {
    warn_open_claims(state, socket, session, turn, WORKER_TIMED_OUT, now_ms);
}

/// How the agent settles a delivery whose claim ended without settling it.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct WayBack {
    pub attempt_id: crate::DeliveryAttemptId,
    pub line: String,
    /// The line is the reply, with a body to write; otherwise it closes the
    /// delivery without one.
    pub answers: bool,
    /// The error that stopped the Stop from settling it.
    pub failure: Option<String>,
}

/// What a prompt carried, beyond the delivery it may arm.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct PromptFacts<'a> {
    /// The tasks its Claude Code notice is about.
    pub notified: &'a [String],
    /// It authenticated a delivery for this session, armed or explicit.
    pub delivery: bool,
}

/// What one prompt found in its session's claims.
#[derive(Debug, Default, Eq, PartialEq)]
pub(crate) struct Observed {
    /// Whether `arm` was written: the only thing that decides what the hook
    /// tells the agent about this prompt's delivery.
    pub armed: bool,
    /// The way back to each delivery a claim left open, so the agent can
    /// still settle it: in its thread, for a reply.
    pub left_open: Vec<WayBack>,
}

/// Runs for every prompt of a session that names its turn. It ends claims
/// that lost their turn, arms `arm` when the session holds no other open
/// claim, and finishes left claims whose deliveries are no longer
/// outstanding. A claim it shows stays fresh, so pruning never hides it.
pub(crate) fn observe_prompt(
    state: &Path,
    socket: &str,
    session: &str,
    turn: &HostTurn,
    arm: Option<Claim>,
    prompt: PromptFacts<'_>,
    now_ms: u64,
) -> Observed {
    let PromptFacts { notified, delivery } = prompt;
    let directory = session_directory(state, socket, session);
    if arm.is_none() && !directory.join("claims.json").exists() {
        return Observed::default();
    }
    let Some(mut claims) = Claims::lock(directory, PROMPT_LOCK_WAIT) else {
        return Observed::default();
    };
    claims.see_turn(turn, delivery || arm.is_some(), now_ms);
    let moved: Vec<_> = claims
        .claims
        .iter()
        .map(|claim| {
            (claim.state == ClaimState::Open && claim.turn != *turn)
                .then(|| claims.moved_on(claim, now_ms))
                .flatten()
        })
        .collect();
    for (claim, next) in claims.claims.iter_mut().zip(moved) {
        if let Some(next) = next {
            claim.move_on(state, next, now_ms);
        }
    }
    // A claim whose Stop already ran belongs to that Stop's worker; only one
    // still waiting for its own turn's Stop keeps this delivery explicit.
    let open = claims
        .claims
        .iter()
        .any(|claim| claim.state == ClaimState::Open && claims.stopped_at(&claim.turn).is_none());
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
    // The claim this prompt's notice wakes answers at this turn's Stop. A
    // turn promises one claim; a later notice in it shows its claim's way back.
    let promised = claims
        .claims
        .iter()
        .any(|claim| claim.promised_turn.as_ref() == Some(turn));
    if !armed
        && !promised
        && let Some(index) = claims.woken(state, turn, notified)
    {
        claims.claims[index].promised_turn = Some(turn.clone());
    }
    let mut left_open = Vec::new();
    for claim in &mut claims.claims {
        if !matches!(claim.state, ClaimState::Left(_)) {
            continue;
        }
        if claim.promised_turn.as_ref() == Some(turn) {
            claim.updated_at_ms = now_ms;
        } else if outstanding(state, claim) == Some(false) {
            claim.set(ClaimState::Done(Done::Superseded), now_ms);
        } else if let Some(way_back) = claim.way_back() {
            left_open.push(way_back);
            claim.updated_at_ms = now_ms;
        }
    }
    claims.save(now_ms);
    Observed { armed, left_open }
}

/// Whether a Stop of `turn` has anything to settle: an open claim, or a held
/// one this turn's prompt promised or whose work's notice the transcript
/// names, as `names_notice` says for the held tasks. When it can't tell, the
/// worker starts. It reads without the lock, so a Stop with nothing to settle
/// starts no worker.
pub fn stop_has_work(
    state: &Path,
    socket: &str,
    session: &str,
    turn: Option<&HostTurn>,
    names_notice: impl FnOnce(&[String]) -> Option<bool>,
) -> bool {
    let claims = read_claims(&session_directory(state, socket, session));
    let held: Vec<&Claim> = claims
        .iter()
        .filter(|claim| {
            claim.state == ClaimState::Left(LeaveReason::Held) && !claim.awaits.is_empty()
        })
        .collect();
    if claims.iter().any(|claim| claim.state == ClaimState::Open)
        || held
            .iter()
            .any(|claim| turn.is_some() && claim.promised_turn.as_ref() == turn)
    {
        return true;
    }
    if held.is_empty() {
        return false;
    }
    let tasks: Vec<String> = held
        .iter()
        .flat_map(|claim| claim.awaits.iter().cloned())
        .collect();
    names_notice(&tasks).unwrap_or(true)
}

/// Whether the last `limit` bytes of the transcript at `path` name a notice of
/// one of `tasks`. It reads back from the end a chunk at a time, and is `None`
/// when it can't read the file or can't finish within `budget`.
pub fn names_notice(path: &Path, tasks: &[String], limit: u64, budget: Duration) -> Option<bool> {
    const CHUNK: u64 = 1024 * 1024;
    let started = Instant::now();
    let needles: Vec<String> = tasks
        .iter()
        .map(|id| format!("<task-id>{id}</task-id>"))
        .collect();
    let finders: Vec<_> = needles
        .iter()
        .map(|needle| memchr::memmem::Finder::new(needle.as_bytes()))
        .collect();
    let overlap = needles.iter().map(String::len).max().unwrap_or(0) as u64;
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() {
        return None;
    }
    let end = metadata.len();
    let floor = end.saturating_sub(limit);
    let (mut high, mut bytes) = (end, Vec::new());
    while high > floor {
        if started.elapsed() >= budget {
            return None;
        }
        let low = high.saturating_sub(CHUNK).max(floor);
        // Each chunk reaches into the one after it, so a notice across the
        // boundary is still found.
        let until = high.saturating_add(overlap).min(end);
        bytes.clear();
        file.seek(std::io::SeekFrom::Start(low)).ok()?;
        (&mut file).take(until - low).read_to_end(&mut bytes).ok()?;
        if finders.iter().any(|finder| finder.find(&bytes).is_some()) {
            return Some(true);
        }
        high = low;
    }
    Some(false)
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
/// and `complete` applies. A failed settle leaves its delivery pending, keeps
/// the error on the claim and warns the person. `now_ms` is when the Stop
/// ended, however late its worker runs.
pub fn settle_at_stop(
    state: &Path,
    socket: &str,
    stop: &StopFacts,
    now_ms: u64,
    run: &mut dyn FnMut(BoundOperation) -> Result<Value, String>,
) {
    let directory = session_directory(state, socket, &stop.session);
    let Some(mut claims) = Claims::lock(directory, STOP_LOCK_WAIT) else {
        warn_open_claims(
            state,
            socket,
            &stop.session,
            stop.turn.as_ref(),
            LOCK_MISSED,
            now_ms,
        );
        return;
    };
    if let Some(turn) = &stop.turn {
        // A claim this turn's prompt promised comes first: the agent was shown
        // no other way to answer it.
        let woken = claims
            .promised(state, turn)
            .or_else(|| claims.woken(state, turn, &stop.notified_tasks));
        if let Some(index) = woken {
            // The relay looks for an answer the agent already posted this
            // turn from the turn's start.
            // With no record of the turn, from this Stop only: a duplicate
            // answer is the safer failure than one taken for answered.
            let started_at_ms = claims
                .turn_seen(turn)
                .map_or(now_ms, |seen| seen.started_at_ms);
            claims.claims[index].wake_on(turn.clone(), started_at_ms);
        }
        // A promise this Stop didn't keep ends here, so later prompts show
        // that claim's way back.
        for claim in &mut claims.claims {
            if claim.promised_turn.as_ref() == Some(turn) {
                claim.promised_turn = None;
            }
        }
    }
    // A claim of another turn armed after this Stop ended belongs to a turn
    // that started after it: a late worker leaves it to that turn.
    let moved: Vec<_> = claims
        .claims
        .iter()
        .map(|claim| {
            (claim.state == ClaimState::Open && stop.turn.as_ref() != Some(&claim.turn)).then(
                || {
                    (claim.armed_at_ms <= now_ms)
                        .then(|| claims.moved_on(claim, now_ms))
                        .flatten()
                },
            )
        })
        .collect();
    for (claim, moved) in claims.claims.iter_mut().zip(moved) {
        match moved {
            Some(Some(next)) => {
                claim.move_on(state, next, now_ms);
                continue;
            }
            Some(None) => continue,
            None => {}
        }
        let current = claim.state == ClaimState::Open && stop.turn.as_ref() == Some(&claim.turn);
        let facts = if current {
            settle_facts(state, &stop.session, claim, now_ms)
        } else {
            SettleFacts::default()
        };
        let next = match decide(claim, stop, &facts) {
            Verdict::Ignore => continue,
            Verdict::Leave(LeaveReason::Held) => {
                claim.awaits = stop.awaited(claim);
                ClaimState::Left(LeaveReason::Held)
            }
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
                    Err(error) => claim.settle_failed(state, error.to_string(), now_ms),
                }
            }
            Verdict::Settle(settlement) => match run(claim.bound(&settlement)) {
                Ok(_) => ClaimState::Done(settlement.done()),
                Err(_) if disposition(state, claim).is_some() => ClaimState::Done(Done::Superseded),
                Err(error) => claim.settle_failed(state, error, now_ms),
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
                            claim.helpers_since_ms(),
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

/// Whether the claim's attempt is still in its session's delivery inbox, or
/// `None` when the store cannot say; then the way back is shown, since a
/// line that no longer applies is refused in a plain sentence.
fn outstanding(state: &Path, claim: &Claim) -> Option<bool> {
    crate::StateStore::open_read_only(state)
        .ok()
        .flatten()?
        .transaction(|transaction| {
            transaction.attempt_outstanding(&claim.namespace, &claim.attempt_id)
        })
        .ok()
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
