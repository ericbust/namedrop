//! `namedrop relay worker`, on A: one per `rex hosts` label. It delivers the
//! label's queued items to B's bridge, collects B's replies while a thread
//! is open, and exits when nothing is queued and no thread is open. So a
//! slow network call never holds a space's local mail.

use super::{
    Hosts,
    rex::{self, Failure},
    store::{Answers, Collected, Held, Queued, RelayStore, Settled},
    wire::{self, Answer, Item, Op, Perform, Reply, Request, Sender, Status},
};
use crate::{
    ActorKind, DeliveryId, IdGenerator, MessageId, MutationAttribution, SpaceId, StateNamespace,
    StateStore,
    mux::Endpoint,
    store::{RelayAuthor, RelayOrigin, Relayed},
};
use serde_json::{Value, json};
use std::{cell::RefCell, path::Path, time::Duration};

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
/// and `wake` starts the mailbox worker of a space that got mail. `woken`
/// holds the spaces of a round to wake after it, which a gate that hands
/// the worker over to another runtime wakes first.
pub(crate) struct Run<'a> {
    pub state: &'a Path,
    pub endpoint: &'a Endpoint,
    pub label: &'a str,
    pub gate: Gate<'a>,
    pub pause: &'a mut dyn FnMut(Duration) -> bool,
    pub woken: &'a RefCell<Vec<SpaceId>>,
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
    let mut reading = Reading::default();
    loop {
        // An update while the worker waited selects another runtime, which
        // takes the worker's place before it does anything more.
        drop((run.gate)());
        let wait = {
            let woken = run.woken;
            let mut once = |space: &SpaceId| {
                let mut woken = woken.borrow_mut();
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
                woken,
                wake: &mut once,
            };
            let wait = round(&mut round_run, &mut reading);
            release(run.state, run.endpoint, run.gate, round_run.wake);
            super::notice::tell(run.state, run.endpoint, run.gate, round_run.wake);
            wait
        };
        for space in run.woken.take() {
            (run.wake)(&space);
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
    /// An item was kept, a call failed, or a reply stays on B for want of
    /// room, so the next round waits longer.
    Kept,
}

/// One round: the label's queued items, then a collect. B's `host_id` is
/// read again each round, so a new item is pinned to the server that
/// answers now.
fn round(run: &mut Run<'_>, reading: &mut Reading) -> Round {
    let now = super::now_ms();
    reading.pages = 0;
    reading.left = !reading.waiting.is_empty();
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
    let (hosts, labels) = match Hosts::read() {
        Ok(hosts) if hosts.labels.iter().any(|listed| listed == run.label) => {
            let labels = hosts.same_host(run.label);
            (hosts, labels)
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
    // A's name for the server these labels reach: the first of them.
    let name = labels.iter().min().map_or(run.label, String::as_str);
    let Some(own) = crate::rex_bridge::known_host_id(run.state, run.endpoint) else {
        return Round::Kept;
    };
    let Ok(peer_id) = status(run.label) else {
        return Round::Kept;
    };
    match answered_as_elsewhere(&store, &hosts, &labels, &peer_id, now) {
        Ok(None) => {
            let _ = store.set_same_server(endpoint, run.label, None);
            let _ = store.set_peer(run.label, &peer_id, now);
        }
        Ok(Some(other)) => {
            let _ = store.set_same_server(endpoint, run.label, Some(&other));
            let reason = format!(
                "{}'s Rex server answers as the one {other} reaches, which your Rex hosts list \
                 gives other addresses",
                run.label
            );
            for item in queued.iter().filter(|item| item.host_id.is_none()) {
                let _ = store.settle(run.label, item, Settled::Failed, Some(&reason), now);
            }
            return Round::Kept;
        }
        Err(_) => return Round::Kept,
    }
    let mut kept = false;
    let silent = || {
        store
            .cannot_reply(&run.endpoint.key, run.label)
            .is_ok_and(|no| no)
    };
    // Whether B said in its latest page this round that it keeps no more
    // replies for this side. Only that fails an item, and only one never
    // sent, since B may have accepted one sent before and still answers
    // for it.
    let mut full = false;
    // The threads of the items B has not taken yet, each with the message
    // that waits. A later message of such a thread waits behind it, so B
    // takes a thread's messages in order.
    let mut waiting: Vec<(&str, &str)> = Vec::new();
    if !queued.is_empty() && silent() {
        // B's last page said it keeps no more replies for this side, so a
        // collect first tells whether that still holds.
        let peer = Peer {
            name,
            host_id: &peer_id,
            labels: &labels,
        };
        let pages = collect(run, &store, &own, &peer, reading);
        kept = !pages.answered;
        full = pages.read && silent();
    }
    for item in &queued {
        if waiting
            .iter()
            .any(|(thread, message)| *thread == item.thread && *message != item.message_id)
        {
            kept = true;
            continue;
        }
        if full && item.host_id.is_none() {
            let reason = format!(
                "{} keeps no more replies for this machine until this machine collects them",
                run.label
            );
            let _ = store.settle(run.label, item, Settled::Failed, Some(&reason), now);
            continue;
        }
        let host_id = item.host_id.clone().unwrap_or_else(|| peer_id.clone());
        if store.pin(run.label, item, &host_id).is_err() {
            return Round::Kept;
        }
        let peer = Peer {
            name,
            host_id: &host_id,
            labels: &labels,
        };
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
            ..acked(&store, &run.endpoint.key, &peer, reading)
        };
        match perform(run.label, &request) {
            Call::Answered(answer) => {
                let more = answer.more;
                if decide(
                    run.label,
                    &store,
                    item,
                    answer.result.as_ref(),
                    now,
                    &mut kept,
                ) {
                    waiting.push((item.thread.as_str(), item.message_id.as_str()));
                }
                let page = collect_replies(run, &store, &peer, &answer, reading);
                let pages = (page != Page::Fault && more)
                    .then(|| collect(run, &store, &own, &peer, reading));
                // Each page stores whether B has room, so the last one read
                // says it.
                if page == Page::Read || pages.as_ref().is_some_and(|pages| pages.read) {
                    full = silent();
                }
                if page == Page::Fault || pages.is_some_and(|pages| !pages.answered) {
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
        name,
        host_id: &peer_id,
        labels: &labels,
    };
    if queued.is_empty() && !collect(run, &store, &own, &peer, reading).answered {
        kept = true;
    }
    if kept || full || reading.left {
        Round::Kept
    } else {
        Round::Collected
    }
}

/// The label outside `labels`, and listed in `hosts`, whose server last
/// reported `host_id` and still does when asked again, so the server
/// behind `labels` answers as one this machine reaches by other
/// addresses. One that now reports another `host_id` is recorded so, and
/// one that does not answer counts.
fn answered_as_elsewhere(
    store: &RelayStore,
    hosts: &Hosts,
    labels: &[String],
    host_id: &str,
    now: i64,
) -> Result<Option<String>, crate::StoreError> {
    for other in store.answering_as(host_id)? {
        if labels.contains(&other) || !hosts.labels.contains(&other) {
            continue;
        }
        match status(&other) {
            Ok(theirs) if theirs != host_id => store.set_peer(&other, &theirs, now)?,
            _ => return Ok(Some(other)),
        }
    }
    Ok(None)
}

/// Settles `item` by B's outcome for it, and says whether B has not taken
/// it yet, so it stays queued.
fn decide(
    label: &str,
    store: &RelayStore,
    item: &Queued,
    outcome: Option<&super::wire::Outcome>,
    now: i64,
    kept: &mut bool,
) -> bool {
    let Some(outcome) = outcome else {
        *kept = true;
        return true;
    };
    let printable: String = outcome
        .reason
        .as_deref()
        .unwrap_or("refused")
        .chars()
        .map(|c| if shown(c) { c } else { ' ' })
        .collect();
    let reason = wire::cut(&printable, wire::REASON_LIMIT);
    let reason = reason.as_str();
    let settled = match outcome.status {
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
            return true;
        }
        Status::Paused => {
            *kept = true;
            let _ = store.paused(label, item);
            return true;
        }
    };
    // The item stays queued, and B keeps each reply to it until a later
    // round settles it by B's stored outcome.
    if settled.is_err() {
        *kept = true;
    }
    false
}

/// What a collect found.
struct Pages {
    /// B answered with at least one page.
    read: bool,
    /// Every call was answered and every page stored.
    answered: bool,
}

/// Whether a terminal shows `c` as itself: not a control character, nor
/// one that formats or breaks the text around it, as U+202E reverses it.
/// Those are Unicode 16's General_Category Cf, the rest of the tag block,
/// and the line and paragraph separators.
fn shown(c: char) -> bool {
    !(c.is_control()
        || matches!(
            c,
            '\u{00AD}'
                | '\u{0600}'..='\u{0605}'
                | '\u{061C}'
                | '\u{06DD}'
                | '\u{070F}'
                | '\u{0890}'..='\u{0891}'
                | '\u{08E2}'
                | '\u{180E}'
                | '\u{200B}'..='\u{200F}'
                | '\u{2028}'..='\u{202E}'
                | '\u{2060}'..='\u{2064}'
                | '\u{2066}'..='\u{206F}'
                | '\u{FEFF}'
                | '\u{FFF9}'..='\u{FFFB}'
                | '\u{110BD}'
                | '\u{110CD}'
                | '\u{13430}'..='\u{1343F}'
                | '\u{1BCA0}'..='\u{1BCA3}'
                | '\u{1D173}'..='\u{1D17A}'
                | '\u{E0000}'..='\u{E007F}'
        ))
}

/// Collects B's replies for this side until B has no more, or for at most
/// `PAGES` pages a round. The next round reads on from where it stopped.
fn collect(
    run: &mut Run<'_>,
    store: &RelayStore,
    own: &str,
    peer: &Peer<'_>,
    reading: &mut Reading,
) -> Pages {
    let mut read = false;
    while reading.pages < PAGES {
        reading.pages += 1;
        let request = Request {
            op: Op::Collect,
            expect_host_id: peer.host_id.to_owned(),
            from: Sender {
                host_id: own.to_owned(),
                name: "namedrop".to_owned(),
                host: super::machine_name(),
            },
            ..acked(store, &run.endpoint.key, peer, reading)
        };
        let Call::Answered(answer) = perform(run.label, &request) else {
            return Pages {
                read,
                answered: false,
            };
        };
        if answer.result.is_some() || !wire::valid_id(&answer.epoch) {
            // B refused the call itself, as for another server's host_id,
            // or could not page, as its answer to a fault of its own says,
            // so the next try backs off.
            return Pages {
                read,
                answered: false,
            };
        }
        match collect_replies(run, store, peer, &answer, reading) {
            Page::None => {}
            Page::Read => read = true,
            Page::Fault => {
                return Pages {
                    read,
                    answered: false,
                };
            }
        }
        if !answer.more || answer.outbox.is_empty() {
            break;
        }
    }
    Pages {
        read,
        answered: true,
    }
}

/// The parts of a request that ack what `endpoint` has taken from `peer`,
/// and ask for the replies after those `reading` read.
fn acked(store: &RelayStore, endpoint: &str, peer: &Peer<'_>, reading: &Reading) -> Request {
    let (epoch, ack_through) = store
        .ack_through(endpoint, peer.name, peer.host_id)
        .unwrap_or_default();
    let after = if reading.host_id == peer.host_id && reading.epoch == epoch {
        reading.after
    } else {
        0
    };
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
        after,
        relay: wire::VERSION,
    }
}

/// The server a worker collects from: A's name for it, the first by name
/// of the labels that reach it by this machine's host list, the `host_id`
/// it reports, and those labels, the worker's own first.
struct Peer<'a> {
    name: &'a str,
    host_id: &'a str,
    labels: &'a [String],
}

/// How far a worker has read B's outbox for one server, apart from what
/// it acks. A pass reads from the ack to B's last reply, over as many
/// rounds as it takes. A reply A has no room for stays on B, as does one
/// to an item A has not settled, and so does every later one, since an
/// ack covers all before it. The pass still takes the later replies of
/// other threads, while those of the reply's own thread wait for it, so
/// it reads in order on a later pass. So no thread waits for another
/// one's room.
#[derive(Default)]
struct Reading {
    host_id: String,
    epoch: String,
    /// The last `seq` this pass read.
    after: i64,
    /// The threads with a reply this pass left on B.
    waiting: Vec<(String, String)>,
    /// Whether a reply this pass left on B found no room in A's hold.
    full: bool,
    /// The pages collected this round, or `PAGES` once a B that ignores
    /// `after` gave one.
    pages: usize,
    /// Whether this round left a reply on B, or read on in a pass that
    /// did.
    left: bool,
}

impl Reading {
    /// Starts a pass over `host_id`'s outbox of `epoch` when this one reads
    /// another.
    fn follow(&mut self, host_id: &str, epoch: &str) {
        if self.host_id != host_id || self.epoch != epoch {
            host_id.clone_into(&mut self.host_id);
            epoch.clone_into(&mut self.epoch);
            self.restart();
        }
    }

    /// Starts the next pass from the ack.
    fn restart(&mut self) {
        self.after = 0;
        self.waiting.clear();
        self.full = false;
    }

    fn leave(&mut self, thread: (String, String)) {
        self.left = true;
        if !self.waiting.contains(&thread) {
            self.waiting.push(thread);
        }
    }
}

/// Whether an answer brought a page of replies, and whether A stored what
/// it found in it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Page {
    /// The answer brought no page, as a refusal or a retry does.
    None,
    Read,
    /// A store failed, so B keeps the reply it failed on and every later
    /// one, and this pass reads on from the one before it.
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
/// thread, so they reach it in order once it takes replies again. Acks
/// stop at a reply A has no room to hold, or one that answers an item A
/// has not settled, as `Reading` tells. A reply read again finds what A
/// made of it before: it is taken or refused once.
fn collect_replies(
    run: &mut Run<'_>,
    store: &RelayStore,
    peer: &Peer<'_>,
    answer: &Answer,
    reading: &mut Reading,
) -> Page {
    let (endpoint, host_id) = (&run.endpoint.key, peer.host_id);
    if !wire::valid_id(&answer.epoch) {
        return Page::None;
    }
    if store
        .set_can_reply(endpoint, run.label, answer.can_reply)
        .is_err()
        || store
            .note_lost(
                endpoint,
                run.label,
                host_id,
                &answer.epoch,
                &answer.lost,
                super::now_ms(),
            )
            .is_err()
    {
        return Page::Fault;
    }
    let Ok((epoch, through)) = store.ack_through(endpoint, peer.name, host_id) else {
        return Page::Fault;
    };
    // A new epoch starts the cursor at 0, so the next request names the
    // epoch the pass reads, even before A acks a reply of it.
    if epoch != answer.epoch
        && store
            .advance(endpoint, peer.name, host_id, &answer.epoch, 0)
            .is_err()
    {
        return Page::Fault;
    }
    let through = if epoch == answer.epoch { through } else { 0 };
    reading.follow(host_id, &answer.epoch);
    // A B built before `after` pages from the ack, so its page starts at or
    // before the last reply this pass read, and its next page would be the
    // same. The pass starts again with this page, and reads no further this
    // round, so a thread with room again takes its replies.
    if answer
        .outbox
        .first()
        .is_some_and(|reply| reply.seq <= reading.after)
    {
        reading.restart();
        reading.pages = PAGES;
    }
    for reply in &answer.outbox {
        if reply.seq <= through {
            continue;
        }
        // A dropped reply is counted once, when it is acked, since it is
        // read again on each pass until then.
        let acked = |other_label: bool| {
            !reading.waiting.is_empty()
                || (!other_label
                    || store
                        .count_other_label(endpoint, run.label, super::now_ms())
                        .is_ok())
                    && store
                        .advance(endpoint, peer.name, host_id, &answer.epoch, reply.seq)
                        .is_ok()
        };
        match place(run, store, peer, answer, reply, reading) {
            Ok(Placed::Done) => {
                if !acked(false) {
                    return Page::Fault;
                }
            }
            Ok(Placed::Dropped { other_label }) => {
                if !acked(other_label) {
                    return Page::Fault;
                }
            }
            Ok(Placed::Left(thread)) => reading.leave(thread),
            Ok(Placed::Full(thread)) => {
                reading.full = true;
                reading.leave(thread);
            }
            Err(()) => return Page::Fault,
        }
        reading.after = reading.after.max(reply.seq);
    }
    let held_full = reading.full;
    if !answer.more {
        reading.restart();
    }
    if store.set_held_full(endpoint, run.label, held_full).is_err() {
        return Page::Fault;
    }
    Page::Read
}

/// What A made of one reply.
enum Placed {
    /// It is taken, held, or refused, so B may let it go.
    Done,
    /// It answers nothing of this endpoint's, or with `other_label` a
    /// message sent to another host, so B may let it go.
    Dropped { other_label: bool },
    /// It stays on B behind an earlier reply of its thread, or until A
    /// settles the item it answers.
    Left((String, String)),
    /// It stays on B, as one A has no room to hold.
    Full((String, String)),
}

/// Places `reply` of `answer`: dropped when it answers nothing of this
/// endpoint's, left on B when it answers an item A has not settled or
/// follows a reply of its thread this pass left, found as A placed it
/// before, held behind its thread's held replies or by the Automatic
/// Reply Limit, or else taken. An error is a store fault.
fn place(
    run: &mut Run<'_>,
    store: &RelayStore,
    peer: &Peer<'_>,
    answer: &Answer,
    reply: &Reply,
    reading: &Reading,
) -> Result<Placed, ()> {
    let (endpoint, host_id) = (&run.endpoint.key, peer.host_id);
    let sent = if trusted(reply) {
        store
            .sent_from(&reply.re, host_id, endpoint, &reply.from, peer.labels)
            .map_err(drop)?
    } else {
        Answers::Unknown
    };
    let (label, space, thread) = match sent {
        Answers::Sent {
            label,
            space,
            thread,
        } => (label, space, thread),
        Answers::Unsettled { space, thread } => return Ok(Placed::Left((space, thread))),
        Answers::OtherLabel => return Ok(Placed::Dropped { other_label: true }),
        Answers::Unknown => return Ok(Placed::Dropped { other_label: false }),
    };
    let thread = (space, thread);
    if reading.waiting.contains(&thread) {
        return Ok(Placed::Left(thread));
    }
    let held = Held {
        endpoint: endpoint.clone(),
        peer: peer.name.to_owned(),
        host_id: host_id.to_owned(),
        epoch: answer.epoch.clone(),
        label,
        space: thread.0.clone(),
        thread: thread.1.clone(),
        reply: reply.clone(),
    };
    if store.placed(&held).map_err(drop)? {
        return Ok(Placed::Done);
    }
    let taken = if store
        .holding(endpoint, &held.space, &held.thread)
        .map_err(drop)?
    {
        Taken::Limited
    } else {
        take(run.state, run.gate, &held).map_err(drop)?
    };
    match taken {
        Taken::Inserted(message_id) => {
            if collected(store, &held, &message_id).map_err(drop)? {
                (run.wake)(&SpaceId::new(&held.space));
            }
        }
        Taken::Limited => {
            if !store.hold(&held, super::now_ms()).map_err(drop)? {
                return Ok(Placed::Full(thread));
            }
        }
        Taken::Refused(reason) => {
            store
                .refused(&held, &reason, super::now_ms())
                .map_err(drop)?;
        }
    }
    Ok(Placed::Done)
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
            Ok(Taken::Refused(reason)) => {
                if store
                    .refused(held, &reason, super::now_ms())
                    .and_then(|()| store.release(held))
                    .is_err()
                {
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
    /// A can never take it, for this reason, as when its sender is gone or
    /// its thread was stopped.
    Refused(String),
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
        peer: &held.peer,
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
                self_named_host: None,
            },
            &attribution,
        )
    });
    match inserted {
        Ok(receipt) => Ok(Taken::Inserted(receipt.message_id.as_str().to_owned())),
        Err(error) if error.is_reply_limit() => Ok(Taken::Limited),
        Err(error) if error.kind() == crate::StoreErrorKind::InvalidInput => {
            Ok(Taken::Refused(error.message().to_owned()))
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
            Err(_) => Call::Failed(unread(label, &performed["result"])),
        },
        Err(failure) => failed(label, &failure),
    }
}

/// Why A fails an item whose answer it cannot read: B runs a newer relay
/// that answered in a shape this one does not know, or none at all.
fn unread(label: &str, result: &Value) -> String {
    match result["relay"].as_u64() {
        Some(theirs) if theirs > u64::from(wire::VERSION) => format!(
            "{label} runs a newer relay (version {theirs}) than this Namedrop's (version {})",
            wire::VERSION
        ),
        _ => format!("{label} runs a Namedrop without this relay"),
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
