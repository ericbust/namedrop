//! `relay::capture`, which runs after each drain in a Rex home on both
//! machines. It reads each new message to a relay author. For each relay
//! author the message addresses, on B it keeps a reply for A's pull, and on
//! A it queues the message to that remote agent. It is local database work
//! only, and idempotent by message id.

use super::{
    store::{Outbound, RelayStore},
    wire::{self, Reply},
};
use crate::{
    StateNamespace, StateStore, ThreadId,
    mux::Endpoint,
    store::{RelayAuthor, RelayOrigin, RelayRecipient, ToRelayAuthor},
};
use std::path::Path;

/// The line a cut reply ends with.
pub(crate) const CUT_NOTICE: &str = "\n\n[Namedrop cut this message to relay it.]";

/// Captures the messages of `endpoint` that are new since the last capture,
/// and calls `start` once for each label it queued an item for. The cursor
/// stops before a message it could not capture, so the next capture tries
/// that message again.
pub(crate) fn capture(
    state: &Path,
    endpoint: &Endpoint,
    start: &mut dyn FnMut(&str),
) -> Result<(), String> {
    let now = super::now_ms();
    let relay = RelayStore::open(state, now).map_err(|error| error.to_string())?;
    let mut main = StateStore::open(state).map_err(|error| error.to_string())?;
    let cursor = relay
        .capture_cursor(&endpoint.key)
        .map_err(|error| error.to_string())?;
    let rows = main
        .transaction(|transaction| transaction.relay_recipient_messages(&endpoint.key, cursor))
        .map_err(|error| error.to_string())?;
    let mut labels: Vec<String> = Vec::new();
    let mut captured = None;
    let mut failure = None;
    for row in &rows {
        match capture_one(&relay, &mut main, endpoint, row, now) {
            Ok(queued) => {
                for label in queued {
                    if !labels.contains(&label) {
                        labels.push(label);
                    }
                }
                captured = Some(row.rowid);
            }
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    if let Some(rowid) = captured {
        relay
            .set_capture_cursor(&endpoint.key, rowid)
            .map_err(|error| error.to_string())?;
    }
    for label in &labels {
        start(label);
    }
    failure.map_or(Ok(()), Err)
}

/// Keeps or queues one message for each relay author it addresses, and
/// returns each label it queued for.
fn capture_one(
    relay: &RelayStore,
    main: &mut StateStore,
    endpoint: &Endpoint,
    row: &ToRelayAuthor,
    now: i64,
) -> Result<Vec<String>, String> {
    let error = |error: crate::StoreError| error.to_string();
    if row.author_address_id.starts_with("relay:") {
        return Ok(Vec::new());
    }
    let mapped = relay
        .mapped_thread(&endpoint.key, &row.space, &row.thread_id)
        .map_err(error)?;
    let mut labels = Vec::new();
    for recipient in &row.relay_recipients {
        let Some((alias, origin)) = recipient.display.rsplit_once('@') else {
            return Err(format!("relay author {} has no host", recipient.address_id));
        };
        if is(recipient, RelayOrigin::Label(origin), alias) {
            queue_to(relay, endpoint, row, origin, alias, now)?;
            labels.push(origin.to_owned());
            continue;
        }
        match &mapped {
            Some((host_id, thread)) if is(recipient, RelayOrigin::Host(host_id), alias) => {
                keep_for(relay, main, endpoint, row, recipient, host_id, thread, now)?;
            }
            // Its thread's mapping expired, so no reply can reach it.
            _ => relay.count_unrouted(&recipient.display).map_err(error)?,
        }
    }
    Ok(labels)
}

/// Whether `recipient` is the relay author of `alias` from `origin`.
fn is(recipient: &RelayRecipient, origin: RelayOrigin<'_>, alias: &str) -> bool {
    RelayAuthor::new(origin, alias, String::new())
        .address_id
        .as_str()
        == recipient.address_id
}

/// On A: queues `row` to `alias` on `label`, in reply to the reply of that
/// agent's that it answers, or to none, which B maps by A's thread.
fn queue_to(
    relay: &RelayStore,
    endpoint: &Endpoint,
    row: &ToRelayAuthor,
    label: &str,
    alias: &str,
    now: i64,
) -> Result<(), String> {
    let in_reply_to = relay
        .answered_reply(
            &endpoint.key,
            &row.space,
            &row.thread_id,
            row.in_reply_to.as_deref(),
            label,
            alias,
        )
        .map_err(|error| error.to_string())?;
    let text = fit(&row.author_alias, alias, &row.body);
    relay
        .queue(&Outbound {
            label,
            message_id: &row.message_id,
            recipient: alias,
            endpoint: &endpoint.key,
            space: &row.space,
            sender: &row.author_alias,
            thread: &row.thread_id,
            in_reply_to: in_reply_to.as_deref(),
            text: &text,
            now_ms: now,
        })
        .map_err(|error| error.to_string())
}

/// On B: keeps `row` for the A host `host_id`, in reply to the A message of
/// `recipient`'s that it answers.
#[allow(clippy::too_many_arguments)]
fn keep_for(
    relay: &RelayStore,
    main: &mut StateStore,
    endpoint: &Endpoint,
    row: &ToRelayAuthor,
    recipient: &RelayRecipient,
    host_id: &str,
    thread: &str,
    now: i64,
) -> Result<(), String> {
    let error = |error: crate::StoreError| error.to_string();
    let namespace = StateNamespace::for_space(&endpoint.key, &row.space);
    let answered = main
        .transaction(|transaction| {
            transaction.relay_answered(
                &namespace,
                &ThreadId::new(&row.thread_id),
                row.rowid,
                &recipient.address_id,
                row.in_reply_to.as_deref(),
            )
        })
        .map_err(error)?;
    let re = match answered {
        Some(message) => relay.relayed_as(host_id, &message).map_err(error)?,
        None => None,
    };
    let Some(re) = re else {
        // The A message it answers is no longer known here.
        return relay.count_unrouted(&recipient.display).map_err(error);
    };
    relay
        .keep_reply(
            host_id,
            &Reply {
                seq: 0,
                re,
                thread: thread.to_owned(),
                from: row.author_alias.clone(),
                message_id: row.message_id.clone(),
                text: cut(&row.body, wire::TEXT_LIMIT),
            },
            now,
        )
        .map_err(error)
}

/// `text`, or its first `limit` bytes and the notice that Namedrop cut it.
pub(crate) fn cut(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit.saturating_sub(CUT_NOTICE.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}{CUT_NOTICE}", &text[..end])
}

/// `text` cut so that its `deliver` request fits what the `rex` CLI sends.
fn fit(sender: &str, to: &str, text: &str) -> String {
    let mut limit = wire::TEXT_LIMIT;
    loop {
        let fitted = cut(text, limit);
        if wire::deliver_size(sender, to, &fitted) <= wire::REQUEST_LIMIT || limit < 2048 {
            return fitted;
        }
        limit -= 1024;
    }
}
