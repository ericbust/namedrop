//! `relay::capture`, which runs after each drain in a Rex home on both
//! machines. It reads each new message to a relay author in a thread the
//! relay maps to a remote one. On B it keeps a reply for A's pull, and on A
//! it queues the answer to the B agent that wrote the reply it answers.
//! It is local database work only, and idempotent by message id.

use super::{
    store::{Outbound, RelayStore},
    wire::{self, Reply},
};
use crate::{StateNamespace, StateStore, ThreadId, mux::Endpoint, store::ToRelayAuthor};
use std::path::Path;

/// The line a cut reply ends with.
pub(crate) const CUT_NOTICE: &str = "\n\n[Namedrop cut this message to relay it.]";

/// Captures the messages of `endpoint` that are new since the last capture,
/// and calls `start` once for each label it queued an item for.
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
    for row in &rows {
        if let Some(label) = capture_one(&relay, &mut main, endpoint, row, now)?
            && !labels.contains(&label)
        {
            labels.push(label);
        }
    }
    if let Some(last) = rows.last() {
        relay
            .set_capture_cursor(&endpoint.key, last.rowid)
            .map_err(|error| error.to_string())?;
    }
    for label in &labels {
        start(label);
    }
    Ok(())
}

/// Keeps or queues one message, and returns the label it queued for.
fn capture_one(
    relay: &RelayStore,
    main: &mut StateStore,
    endpoint: &Endpoint,
    row: &ToRelayAuthor,
    now: i64,
) -> Result<Option<String>, String> {
    let error = |error: crate::StoreError| error.to_string();
    if row.author_address_id.starts_with("relay:") {
        return Ok(None);
    }
    if let Some((host_id, thread)) = relay
        .mapped_thread(&endpoint.key, &row.space, &row.thread_id)
        .map_err(error)?
    {
        let namespace = StateNamespace::for_space(&endpoint.key, &row.space);
        let mut answered = match &row.in_reply_to {
            Some(message) => relay.relayed_as(&host_id, message).map_err(error)?,
            None => None,
        };
        if answered.is_none()
            && let Some(latest) = main
                .transaction(|transaction| {
                    transaction.latest_relay_authored(
                        &namespace,
                        &ThreadId::new(&row.thread_id),
                        row.rowid,
                    )
                })
                .map_err(error)?
        {
            answered = relay.relayed_as(&host_id, &latest).map_err(error)?;
        }
        if let Some(re) = answered {
            relay
                .keep_reply(
                    &host_id,
                    &Reply {
                        seq: 0,
                        re,
                        thread,
                        from: row.author_alias.clone(),
                        message_id: row.message_id.clone(),
                        text: cut(&row.body, wire::TEXT_LIMIT),
                    },
                    now,
                )
                .map_err(error)?;
        }
        return Ok(None);
    }
    let Some(collected) = relay
        .collected_answer(
            &endpoint.key,
            &row.space,
            &row.thread_id,
            row.in_reply_to.as_deref(),
        )
        .map_err(error)?
    else {
        return Ok(None);
    };
    let text = fit(&row.author_alias, &collected.remote_alias, &row.body);
    relay
        .queue(&Outbound {
            label: &collected.label,
            message_id: &row.message_id,
            recipient: &collected.remote_alias,
            endpoint: &endpoint.key,
            space: &row.space,
            sender: &row.author_alias,
            thread: &row.thread_id,
            in_reply_to: Some(&collected.remote_message_id),
            text: &text,
            now_ms: now,
        })
        .map_err(error)?;
    Ok(Some(collected.label))
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
