//! `namedrop relay worker`, on A: one per `rex hosts` label. It delivers the
//! label's queued items to B's bridge, collects B's replies while a thread
//! is open, and exits when nothing is queued and no thread is open. So a
//! slow network call never holds a space's local mail.

use super::{
    Hosts, RemoteHosts,
    rex::{self, Failure},
    store::{Collected, Queued, RelayStore, Settled},
    wire::{self, Answer, Item, Op, Perform, Reply, Request, Sender, Status},
};
use crate::{
    ActorKind, DeliveryId, IdGenerator, MessageId, MutationAttribution, SpaceId, StateNamespace,
    StateStore,
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
/// The client label B's bridge sets.
const BRIDGE: &str = "namedrop";
/// The most outbox pages one collect reads, so a B that always has more
/// cannot hold the worker.
const PAGES: usize = 16;

/// One worker's place and its two effects: `pause` waits and says whether
/// to go on, and `wake` starts the mailbox worker of a space that got mail.
pub(crate) struct Run<'a> {
    pub state: &'a Path,
    pub endpoint: &'a Endpoint,
    pub label: &'a str,
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
/// `pause` says stop.
pub(crate) fn work(run: &mut Run<'_>) {
    let mut faults = 0;
    loop {
        let wait = round(run);
        super::notice::tell(run.state, run.endpoint, run.wake);
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
    match Hosts::read() {
        Ok(hosts)
            if hosts
                .listed()
                .is_some_and(|listed| listed.iter().any(|l| l == run.label)) => {}
        Ok(_) => {
            let reason = format!("{} is no longer in your Rex hosts", run.label);
            for item in &queued {
                let _ = store.settle(run.label, item, Settled::Failed, Some(&reason), now);
            }
            return Round::Done;
        }
        Err(_) => return Round::Kept,
    }
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
                collect_replies(run, &store, &host_id, &answer);
                if more && !collect(run, &store, &own, &host_id) {
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
    if queued.is_empty() && !collect(run, &store, &own, &peer_id) {
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
    let reason = outcome.reason.as_deref().unwrap_or("refused");
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
/// `PAGES` pages, and says whether every call was answered. The next round
/// collects what remains.
fn collect(run: &mut Run<'_>, store: &RelayStore, own: &str, host_id: &str) -> bool {
    for _ in 0..PAGES {
        let request = Request {
            op: Op::Collect,
            expect_host_id: host_id.to_owned(),
            from: Sender {
                host_id: own.to_owned(),
                name: "namedrop".to_owned(),
                host: super::machine_name(),
            },
            ..acked(store, &run.endpoint.key, host_id)
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
        collect_replies(run, store, host_id, &answer);
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

/// Inserts each reply of `answer` that A has not collected into the space
/// of the A message it answers, from the relay author of the B agent that
/// wrote it, and wakes that space. A new outbox epoch means B's `seq`
/// started again, so every reply of it is new. A reply is B's to make up,
/// so one with a bad field, or one that answers a message this endpoint
/// did not send to B, is acked and dropped.
fn collect_replies(run: &mut Run<'_>, store: &RelayStore, host_id: &str, answer: &Answer) {
    let endpoint = &run.endpoint.key;
    let Ok((epoch, through)) = store.ack_through(endpoint, host_id) else {
        return;
    };
    let through = if epoch == answer.epoch { through } else { 0 };
    for reply in &answer.outbox {
        if reply.seq <= through {
            continue;
        }
        let sent = if trusted(reply) {
            store.sent_from(&reply.re, run.label, host_id, endpoint)
        } else {
            Ok(None)
        };
        let Ok(Some((space, thread))) = sent else {
            let _ = store.record_collected(&Collected {
                host_id,
                epoch: &answer.epoch,
                seq: reply.seq,
                label: run.label,
                endpoint,
                space: "",
                message_id: "",
                remote_message_id: "",
                remote_alias: "",
                thread: "",
                now_ms: super::now_ms(),
            });
            let _ = store.advance(endpoint, host_id, &answer.epoch, reply.seq);
            continue;
        };
        let Ok(gate) = crate::runtime_command_guard(run.state, None) else {
            return;
        };
        let inserted = insert_reply(run.state, run.label, host_id, reply, endpoint, &space);
        drop(gate);
        let message_id = match inserted {
            Ok(Some(message)) => message,
            Ok(None) => String::new(),
            Err(_) => return,
        };
        let _ = store.record_collected(&Collected {
            host_id,
            epoch: &answer.epoch,
            seq: reply.seq,
            label: run.label,
            endpoint,
            space: &space,
            message_id: &message_id,
            remote_message_id: &reply.message_id,
            remote_alias: &reply.from,
            thread: &thread,
            now_ms: super::now_ms(),
        });
        let _ = store.advance(endpoint, host_id, &answer.epoch, reply.seq);
        if !message_id.is_empty() {
            (run.wake)(&SpaceId::new(&space));
        }
    }
}

/// Whether each field of `reply` that A stores or shows passes the rule
/// B holds A's own fields to.
fn trusted(reply: &Reply) -> bool {
    super::valid_name(&reply.from)
        && [&reply.re, &reply.message_id, &reply.thread]
            .into_iter()
            .all(|id| wire::valid_id(id))
}

/// The new A message for `reply`, `None` when A can no longer take it, as
/// when its sender is gone, or an error to try again later.
fn insert_reply(
    state: &Path,
    label: &str,
    host_id: &str,
    reply: &Reply,
    endpoint: &str,
    space: &str,
) -> Result<Option<String>, String> {
    let namespace = StateNamespace::for_space(endpoint, space);
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
                request_id: format!("relay-reply:{host_id}:{}", reply.message_id),
                thread: &route.thread_id,
                message_id: MessageId::new(ids.next_id("message")),
                delivery_id: DeliveryId::new(ids.next_id("delivery")),
                in_reply_to: Some(&re),
                recipient: &sender,
                text: &wire::cut(&reply.text, wire::TEXT_LIMIT),
            },
            &attribution,
        )
    });
    match inserted {
        Ok(receipt) => Ok(Some(receipt.message_id.as_str().to_owned())),
        Err(error) if error.kind() == crate::StoreErrorKind::InvalidInput => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

/// The `host_id` B's server reports, which pins each new item to who B is.
fn status(label: &str) -> Result<String, Failure> {
    let status = api(label, &["server.status"], None)?;
    status["host_id"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| Failure::Exit("server.status gave no host_id".to_owned()))
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
            Ok(answer) => Call::Answered(answer),
            Err(_) => Call::Failed(format!("{label} runs a Namedrop without this relay")),
        },
        Err(failure) => failed(label, &failure),
    }
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
