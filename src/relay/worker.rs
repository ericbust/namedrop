//! `namedrop relay worker`, on A: one per `rex hosts` label. It delivers the
//! label's queued items to B's bridge, collects B's replies while a thread
//! is open, and exits when nothing is queued and no thread is open. So a
//! slow network call never holds a space's local mail.

use super::{
    Hosts, RemoteHosts,
    rex::{self, Failure},
    store::{Answers, Collected, Held, Queued, RelayStore, Settled},
    wire::{self, Answer, Item, Op, Perform, Reply, Request, Sender, Status},
};
use crate::{
    ActorKind, DeliveryId, IdGenerator, MessageId, MutationAttribution, SpaceId, StateNamespace,
    StateStore, ThreadId,
    mux::Endpoint,
    store::{RelayAuthor, RelayOrigin, Relayed},
};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

/// How often the worker collects while a thread is open and nothing is
/// queued.
pub(crate) const COLLECT_EVERY: Duration = Duration::from_secs(30);
/// The waits before each new try after a fault, the last repeating.
const BACKOFF: [Duration; 5] = [
    Duration::from_secs(1),
    Duration::from_secs(2),
    Duration::from_secs(5),
    Duration::from_secs(10),
    Duration::from_secs(30),
];
/// Takes the command gate. A relay worker's also runs the runtime an
/// update selected in its place, right after it takes the gate.
pub(crate) type Gate<'a> = &'a dyn Fn() -> Result<Option<std::fs::File>, String>;

/// The client label B's bridge sets.
const BRIDGE: &str = "namedrop";
/// The most outbox pages one collect reads, so a B that always has more
/// cannot hold the worker.
const PAGES: usize = 16;

/// One worker's place and its effects: `gate` takes the command gate for
/// a write to the main database, `pause` waits and says whether to go on,
/// and `wake` starts the mailbox worker of a space that got mail.
pub(crate) struct Run<'a> {
    pub state: &'a Path,
    pub endpoint: &'a Endpoint,
    pub label: &'a str,
    pub gate: Gate<'a>,
    pub pause: &'a mut dyn FnMut(Duration) -> bool,
    pub wake: &'a mut dyn FnMut(&SpaceId),
}

/// What became of one call.
enum Call {
    Answered(Answer),
    /// Kept, and sent again after the next wait.
    Kept,
    /// Failed for good, with this reason.
    Failed(String),
}

/// The worker's loop, until nothing is queued and no thread is open, or
/// `pause` says stop. Each space that got mail in a round is woken once,
/// after it.
pub(crate) fn work(run: &mut Run<'_>) {
    let mut faults = 0;
    loop {
        // An update while the worker waited selects another runtime, which
        // takes the worker's place before it does anything more.
        drop((run.gate)());
        let mut woken: Vec<SpaceId> = Vec::new();
        let wait = {
            let mut once = |space: &SpaceId| {
                if !woken.contains(space) {
                    woken.push(space.clone());
                }
            };
            let mut round_run = Run {
                state: run.state,
                endpoint: run.endpoint,
                label: run.label,
                gate: run.gate,
                pause: &mut *run.pause,
                wake: &mut once,
            };
            let wait = round(&mut round_run);
            release(run.state, run.endpoint, run.gate, round_run.wake);
            super::notice::tell(run.state, run.endpoint, run.gate, round_run.wake);
            wait
        };
        for space in &woken {
            (run.wake)(space);
        }
        let wait = match wait {
            Round::Done => return,
            Round::Collected if finished(run) => return,
            Round::Collected => {
                faults = 0;
                COLLECT_EVERY
            }
            Round::Kept => {
                faults += 1;
                BACKOFF[(faults - 1).min(BACKOFF.len() - 1)]
            }
        };
        if !(run.pause)(wait) {
            return;
        }
    }
}

/// Whether nothing is queued for the label and no thread of it is open.
fn finished(run: &Run<'_>) -> bool {
    let now = super::now_ms();
    RelayStore::open(run.state, now).is_ok_and(|store| {
        store
            .queued(run.label, &run.endpoint.key)
            .is_ok_and(|queued| queued.is_empty())
            && store
                .thread_open(run.label, &run.endpoint.key, now)
                .is_ok_and(|open| !open)
    })
}

enum Round {
    /// Nothing is queued and no thread is open.
    Done,
    /// Every queued item was decided, and the label was collected.
    Collected,
    /// An item was kept or a call failed, so the next round waits longer.
    Kept,
}

/// One round: the label's queued items, then a collect. B's `host_id` is
/// read again each round, so a new item is pinned to the server that
/// answers now.
fn round(run: &mut Run<'_>) -> Round {
    let now = super::now_ms();
    let Ok(store) = RelayStore::open(run.state, now) else {
        return Round::Kept;
    };
    let endpoint = &run.endpoint.key;
    let (Ok(queued), Ok(open)) = (
        store.queued(run.label, endpoint),
        store.thread_open(run.label, endpoint, now),
    ) else {
        return Round::Kept;
    };
    if queued.is_empty() && !open {
        return Round::Done;
    }
    let labels = match Hosts::read() {
        Ok(hosts)
            if hosts
                .listed()
                .is_some_and(|listed| listed.iter().any(|l| l == run.label)) =>
        {
            hosts.same_host(run.label)
        }
        Ok(_) => {
            let reason = format!("{} is no longer in your Rex hosts", run.label);
            for item in &queued {
                let _ = store.settle(run.label, item, Settled::Failed, Some(&reason), now);
            }
            return Round::Done;
        }
        Err(_) => return Round::Kept,
    };
    let Some(own) = crate::rex_bridge::known_host_id(run.state, run.endpoint) else {
        return Round::Kept;
    };
    let peer_id = match status(run.label) {
        Ok(host_id) => {
            let _ = store.set_peer(run.label, &host_id, now);
            host_id
        }
        Err(_) => return Round::Kept,
    };
    let mut kept = false;
    for item in &queued {
        let host_id = item.host_id.clone().unwrap_or_else(|| peer_id.clone());
        if store.pin(run.label, item, &host_id).is_err() {
            return Round::Kept;
        }
        let request = Request {
            op: Op::Deliver,
            expect_host_id: host_id.clone(),
            from: Sender {
                host_id: own.clone(),
                name: item.sender.clone(),
                host: super::machine_name(),
            },
            item: Some(Item {
                id: item.message_id.clone(),
                to: item.recipient.clone(),
                thread: item.thread.clone(),
                in_reply_to: item.in_reply_to.clone(),
                text: item.text.clone(),
            }),
            ..acked(&store, &run.endpoint.key, &host_id)
        };
        match perform(run.label, &request) {
            Call::Answered(answer) => {
                let more = answer.more;
                decide(
                    run.label,
                    &store,
                    item,
                    answer.result.as_ref(),
                    now,
                    &mut kept,
                );
                let peer = Peer {
                    host_id: &host_id,
                    labels: &labels,
                };
                if collect_replies(run, &store, &peer, &answer) != Page::Taken
                    || more && !collect(run, &store, &own, &peer)
                {
                    kept = true;
                }
            }
            Call::Kept => {
                kept = true;
                break;
            }
            Call::Failed(reason) => {
                let _ = store.settle(run.label, item, Settled::Failed, Some(&reason), now);
            }
        }
    }
    let peer = Peer {
        host_id: &peer_id,
        labels: &labels,
    };
    if queued.is_empty() && !collect(run, &store, &own, &peer) {
        kept = true;
    }
    if kept { Round::Kept } else { Round::Collected }
}

/// Settles `item` by B's outcome for it.
fn decide(
    label: &str,
    store: &RelayStore,
    item: &Queued,
    outcome: Option<&super::wire::Outcome>,
    now: i64,
    kept: &mut bool,
) {
    let Some(outcome) = outcome else {
        *kept = true;
        return;
    };
    let reason = wire::cut(
        outcome.reason.as_deref().unwrap_or("refused"),
        wire::REASON_LIMIT,
    );
    let reason = reason.as_str();
    let _ = match outcome.status {
        Status::Accepted | Status::Duplicate => {
            store.settle(label, item, Settled::Accepted, None, now)
        }
        Status::Refused if reason == "wrong_host" => store.settle(
            label,
            item,
            Settled::Failed,
            Some(&format!("{label} now answers as another Rex server")),
            now,
        ),
        Status::Refused | Status::Ambiguous => {
            store.settle(label, item, Settled::Failed, Some(reason), now)
        }
        Status::Retry => {
            *kept = true;
            Ok(())
        }
    };
}

/// Collects B's replies for this side until B has no more, or for at most
/// `PAGES` pages, and says whether every call was answered and every reply
/// taken. The next round collects what remains.
fn collect(run: &mut Run<'_>, store: &RelayStore, own: &str, peer: &Peer<'_>) -> bool {
    for _ in 0..PAGES {
        let request = Request {
            op: Op::Collect,
            expect_host_id: peer.host_id.to_owned(),
            from: Sender {
                host_id: own.to_owned(),
                name: "namedrop".to_owned(),
                host: super::machine_name(),
            },
            ..acked(store, &run.endpoint.key, peer.host_id)
        };
        let Call::Answered(answer) = perform(run.label, &request) else {
            return false;
        };
        if answer
            .result
            .as_ref()
            .is_some_and(|result| result.status != Status::Retry)
        {
            // B refused the call itself, as for another server's host_id.
            return false;
        }
        if collect_replies(run, store, peer, &answer) != Page::Taken {
            return false;
        }
        if !answer.more || answer.outbox.is_empty() {
            return true;
        }
    }
    true
}

/// The parts of a request that ack what `endpoint` has taken from
/// `host_id`.
fn acked(store: &RelayStore, endpoint: &str, host_id: &str) -> Request {
    let (epoch, ack_through) = store.ack_through(endpoint, host_id).unwrap_or_default();
    Request {
        op: Op::Collect,
        expect_host_id: String::new(),
        from: Sender {
            host_id: String::new(),
            name: String::new(),
            host: String::new(),
        },
        item: None,
        ack_through,
        epoch,
    }
}

/// The server a worker collects from: the `host_id` it reports, and the
/// labels that reach it by this machine's host list, the worker's own
/// first.
struct Peer<'a> {
    host_id: &'a str,
    labels: &'a [String],
}

/// Whether a page of replies was all taken.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Page {
    Taken,
    /// A had no room to hold a reply, so B keeps it and every later one,
    /// and they come again.
    Full,
    /// A store failed, so B keeps the reply it failed on and every later
    /// one.
    Fault,
}

/// Inserts each reply of `answer` that A has not collected into the space
/// of the A message it answers, from the relay author of the B agent that
/// wrote it on the label that message was sent by, and wakes that space.
/// A new outbox epoch means B's `seq` started again, so every reply of it
/// is new. A reply is B's to make up, so one with a bad field, or one that
/// answers a message this endpoint did not send to `peer`, is acked and
/// dropped, and A keeps nothing of it but a count for Doctor of those that
/// answer a message sent to another host. A reply whose thread the
/// Automatic Reply Limit paused is held, as is each later one of that
/// thread, so they reach it in order once it takes replies again. A reply
/// A has no room to hold is not acked, nor is any later one, and the
/// later replies of its thread wait for it, while those of other threads
/// are taken: taking a reply again finds what the first take made.
fn collect_replies(
    run: &mut Run<'_>,
    store: &RelayStore,
    peer: &Peer<'_>,
    answer: &Answer,
) -> Page {
    let (endpoint, host_id) = (&run.endpoint.key, peer.host_id);
    if !wire::valid_id(&answer.epoch) {
        // A refusal or a retry brings no page.
        return Page::Taken;
    }
    let Ok((epoch, through)) = store.ack_through(endpoint, host_id) else {
        return Page::Fault;
    };
    let through = if epoch == answer.epoch { through } else { 0 };
    let mut full: Vec<(String, String)> = Vec::new();
    // Acks `seq` unless B keeps an earlier reply, and says whether that
    // went well.
    let ack = |seq: i64, full: &[(String, String)]| {
        !full.is_empty() || store.advance(endpoint, host_id, &answer.epoch, seq).is_ok()
    };
    for reply in &answer.outbox {
        if reply.seq <= through {
            continue;
        }
        let sent = if trusted(reply) {
            store.sent_from(&reply.re, host_id, endpoint, &reply.from, peer.labels)
        } else {
            Ok(Answers::Unknown)
        };
        let (label, space, thread) = match sent {
            Ok(Answers::Sent {
                label,
                space,
                thread,
            }) => (label, space, thread),
            Ok(dropped) => {
                if matches!(dropped, Answers::OtherLabel)
                    && store.count_other_label(endpoint, run.label).is_err()
                    || !ack(reply.seq, &full)
                {
                    return Page::Fault;
                }
                continue;
            }
            Err(_) => return Page::Fault,
        };
        if full.contains(&(space.clone(), thread.clone())) {
            continue;
        }
        let held = Held {
            endpoint: endpoint.clone(),
            host_id: host_id.to_owned(),
            epoch: answer.epoch.clone(),
            label,
            space,
            thread,
            reply: reply.clone(),
        };
        let taken = match store.holding(endpoint, &held.space, &held.thread) {
            Ok(true) => Ok(Taken::Limited),
            Ok(false) => take(run.state, run.gate, &held),
            Err(error) => Err(error.to_string()),
        };
        match taken {
            Ok(Taken::Inserted(message_id)) => match collected(store, &held, &message_id) {
                Ok(true) => (run.wake)(&SpaceId::new(&held.space)),
                Ok(false) => {}
                Err(_) => return Page::Fault,
            },
            Ok(Taken::Limited) => match store.hold(&held, super::now_ms()) {
                Ok(true) => {}
                Ok(false) => {
                    full.push((held.space, held.thread));
                    continue;
                }
                Err(_) => return Page::Fault,
            },
            Ok(Taken::Refused) => {}
            Err(_) => return Page::Fault,
        }
        if !ack(reply.seq, &full) {
            return Page::Fault;
        }
    }
    if store
        .set_held_full(endpoint, run.label, !full.is_empty())
        .is_err()
    {
        return Page::Fault;
    }
    if full.is_empty() {
        Page::Taken
    } else {
        Page::Full
    }
}

/// Inserts each reply `endpoint` holds whose thread takes replies again,
/// oldest first, wakes its space, and returns each label it inserted a
/// reply of. A thread still paused keeps the rest of its replies, and one
/// that can never take them, as a stopped one, lets them go.
pub(crate) fn release(
    state: &Path,
    endpoint: &Endpoint,
    gate: Gate<'_>,
    wake: &mut dyn FnMut(&SpaceId),
) -> Vec<String> {
    let mut labels: Vec<String> = Vec::new();
    let Ok(store) = RelayStore::open(state, super::now_ms()) else {
        return labels;
    };
    let Ok(held) = store.held(&endpoint.key, false) else {
        return labels;
    };
    let mut paused: Vec<(String, String)> = Vec::new();
    for held in &held {
        let thread = (held.space.clone(), held.thread.clone());
        if paused.contains(&thread) {
            continue;
        }
        match take(state, gate, held) {
            Ok(Taken::Inserted(message_id)) => {
                if collected(&store, held, &message_id)
                    .and_then(|_| store.release(held))
                    .is_err()
                {
                    return labels;
                }
                wake(&SpaceId::new(&held.space));
                if !labels.contains(&held.label) {
                    labels.push(held.label.clone());
                }
            }
            Ok(Taken::Limited) => paused.push(thread),
            Ok(Taken::Refused) => {
                if store.release(held).is_err() {
                    return labels;
                }
            }
            Err(_) => return labels,
        }
    }
    labels
}

/// What became of a reply A tried to insert.
enum Taken {
    /// It is the A message with this id.
    Inserted(String),
    /// Its thread takes no agent reply until a person's message or
    /// Continue, so A holds it.
    Limited,
    /// A can never take it, as when its sender is gone or its thread was
    /// stopped.
    Refused,
}

/// Inserts `held`'s reply under the command gate.
fn take(state: &Path, gate: Gate<'_>, held: &Held) -> Result<Taken, String> {
    let gate = gate()?;
    let taken = insert_reply(state, held);
    drop(gate);
    taken
}

/// Records that `held`'s reply is the A message `message_id`, so an answer
/// to it goes back to its B agent, and says whether A had no record of it.
/// A copy of a reply B sends again under a new `seq` is the same A
/// message, so it records nothing, and neither wakes its space nor keeps
/// its thread open.
fn collected(store: &RelayStore, held: &Held, message_id: &str) -> Result<bool, crate::StoreError> {
    store.record_collected(&Collected {
        host_id: &held.host_id,
        epoch: &held.epoch,
        seq: held.reply.seq,
        label: &held.label,
        endpoint: &held.endpoint,
        space: &held.space,
        message_id,
        remote_message_id: &held.reply.message_id,
        remote_alias: &held.reply.from,
        thread: &held.thread,
        now_ms: super::now_ms(),
    })
}

/// Whether each field of `reply` that A stores or shows passes the rule
/// B holds A's own fields to.
fn trusted(reply: &Reply) -> bool {
    super::valid_name(&reply.from)
        && [&reply.re, &reply.message_id, &reply.thread]
            .into_iter()
            .all(|id| wire::valid_id(id))
}

/// Inserts `held`'s reply in its A thread, or says why A does not, or
/// gives an error to try again later.
fn insert_reply(state: &Path, held: &Held) -> Result<Taken, String> {
    let (label, reply) = (&held.label, &held.reply);
    let namespace = StateNamespace::for_space(&held.endpoint, &held.space);
    let author = RelayAuthor::new(
        RelayOrigin::Label(label),
        &reply.from,
        format!("{}@{label}", reply.from),
    );
    let ids = crate::ProcessIds::default();
    let re = MessageId::new(&reply.re);
    let attribution = MutationAttribution::new(
        ActorKind::AgentSession,
        author.address_id.as_str(),
        u64::try_from(super::now_ms()).unwrap_or_default(),
    );
    let mut main = StateStore::open(state).map_err(|error| error.to_string())?;
    let inserted = main.transaction(|transaction| {
        let route = transaction.message_route(&namespace, &re)?;
        let sender =
            transaction.routing_address_by_id(&namespace, &route.participants[0].address_id)?;
        transaction.insert_relayed_message(
            &namespace,
            &Relayed {
                author: &author,
                request_id: format!("relay-reply:{}:{}", held.host_id, reply.message_id),
                thread: &route.thread_id,
                message_id: MessageId::new(ids.next_id("message")),
                delivery_id: DeliveryId::new(ids.next_id("delivery")),
                in_reply_to: Some(&re),
                recipient: &sender,
                text: &wire::cut(&reply.text, wire::TEXT_LIMIT),
                notice: false,
            },
            &attribution,
        )
    });
    match inserted {
        Ok(receipt) => Ok(Taken::Inserted(receipt.message_id.as_str().to_owned())),
        Err(error) if error.kind() == crate::StoreErrorKind::InvalidInput => {
            let thread = ThreadId::new(&held.thread);
            let limited = main
                .transaction(|transaction| transaction.thread_control_state(&namespace, &thread))
                .is_ok_and(|controls| controls.reply_allowance == 0 && !controls.stopped);
            Ok(if limited {
                Taken::Limited
            } else {
                Taken::Refused
            })
        }
        Err(error) => Err(error.to_string()),
    }
}

/// The `host_id` B's server reports, which pins each new item to who B is.
fn status(label: &str) -> Result<String, Failure> {
    let status = api(label, &["server.status"], None)?;
    status["host_id"]
        .as_str()
        .filter(|host_id| wire::valid_id(host_id))
        .map(str::to_owned)
        .ok_or_else(|| Failure::Exit("server.status gave no valid host_id".to_owned()))
}

/// One `namedrop.relay` call: `target.resolve` for B's bridge, then
/// `client.perform`, mapped by the outcome table.
fn perform(label: &str, request: &Request) -> Call {
    let query = json!({"queries": [BRIDGE], "kinds": ["client"]}).to_string();
    let resolved = api(label, &["target.resolve", &query], None);
    let client_id = match resolved {
        Ok(resolved) => {
            let result = &resolved["results"][0];
            match (
                result["status"].as_str(),
                result["target"]["client_id"].as_str(),
            ) {
                (Some("resolved"), Some(client_id)) => client_id.to_owned(),
                _ => return Call::Kept,
            }
        }
        Err(failure) => return failed(label, &failure),
    };
    let call = Perform {
        client_id,
        action: crate::mux::rex::events::RELAY_ACTION.to_owned(),
        args: request.clone(),
    };
    let input = match serde_json::to_vec(&call) {
        Ok(input) => input,
        Err(error) => return Call::Failed(error.to_string()),
    };
    match api(label, &["client.perform", "-"], Some(&input)) {
        Ok(performed) => match serde_json::from_value::<Answer>(performed["result"].clone()) {
            Ok(answer) => Call::Answered(bounded(answer)),
            Err(_) => Call::Failed(format!("{label} runs a Namedrop without this relay")),
        },
        Err(failure) => failed(label, &failure),
    }
}

/// `answer` without its replies when its epoch is not an id, because A
/// stores the epoch B makes up with each reply it takes.
fn bounded(mut answer: Answer) -> Answer {
    if !wire::valid_id(&answer.epoch) {
        answer.outbox.clear();
        answer.more = false;
    }
    answer
}

/// The outcome table's rows for a failed `rex` call.
fn failed(label: &str, failure: &Failure) -> Call {
    let Failure::Exit(stderr) = failure else {
        return Call::Kept;
    };
    if stderr.contains("control message too large") {
        return Call::Failed("the message is too large to relay".to_owned());
    }
    let code = stderr
        .split("code=")
        .nth(1)
        .map(|rest| {
            rest.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                .next()
                .unwrap_or_default()
        })
        .unwrap_or_default();
    match code {
        "bad_request" | "not_supported" => {
            Call::Failed(format!("{label} runs a Namedrop without this relay"))
        }
        "too_large" => Call::Failed("the message is too large to relay".to_owned()),
        _ => Call::Kept,
    }
}

/// `rex -S <label> api call <args>`, its stdout as JSON.
fn api(label: &str, args: &[&str], input: Option<&[u8]>) -> Result<Value, Failure> {
    let mut call = vec!["-S", label, "--timeout", "15s", "api", "call"];
    call.extend_from_slice(args);
    let output = rex::run(&call, input, rex::CALL)?;
    serde_json::from_slice(&output)
        .map_err(|error| Failure::Exit(format!("rex gave no JSON: {error}")))
}

#[cfg(test)]
mod tests;
