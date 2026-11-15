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

/// Captures the messages of `endpoint` that are new since the last capture,
/// and calls `start` once for each label it queued an item for. The cursor
/// stops before a message it could not capture, so the next capture tries
/// that message again, and it passes each message in one transaction with
/// the count of the replies of it that B could not keep, so a retry never
/// counts one twice. Once every message is captured, the cursor moves past
/// the endpoint's last message, so the next capture reads only those made
/// since. Everything it needs of the main database is read first, in one
/// transaction, so a relay transaction never waits on the main database.
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
    let (rows, last) = main
        .transaction(|transaction| {
            let (rows, last) = transaction.relay_recipient_messages(&endpoint.key, cursor)?;
            let rows = rows
                .into_iter()
                .map(|row| {
                    let answered = answered(transaction, endpoint, &row)?;
                    Ok((row, answered))
                })
                .collect::<Result<Vec<_>, crate::StoreError>>()?;
            Ok((rows, last))
        })
        .map_err(|error| error.to_string())?;
    drop(main);
    let mut labels: Vec<String> = Vec::new();
    let mut failure = None;
    for (row, answered) in &rows {
        let captured = relay.captured(&endpoint.key, row.rowid, || {
            capture_one(&relay, endpoint, row, answered, now)
        });
        match captured {
            Ok(queued) => {
                for label in queued {
                    if !labels.contains(&label) {
                        labels.push(label);
                    }
                }
            }
            Err(error) => {
                failure = Some(error);
                break;
            }
        }
    }
    if failure.is_none()
        && let Some(last) = last
        && let Err(error) = relay.capture_through(&endpoint.key, last)
    {
        failure = Some(error.to_string());
    }
    for label in &labels {
        start(label);
    }
    failure.map_or(Ok(()), Err)
}

/// For each relay author `row` addresses, in order, the message of that
/// author's in `row`'s thread that `row` answers, which B keeps a reply
/// against, or `None` for an author of a label, as A has.
fn answered(
    transaction: &crate::store::StateTransaction<'_>,
    endpoint: &Endpoint,
    row: &ToRelayAuthor,
) -> Result<Vec<Option<String>>, crate::StoreError> {
    let namespace = StateNamespace::for_space(&endpoint.key, &row.space);
    row.relay_recipients
        .iter()
        .map(|recipient| match recipient.display.rsplit_once('@') {
            Some((alias, label)) if !is(recipient, RelayOrigin::Label(label), alias) => transaction
                .relay_answered(
                    &namespace,
                    &ThreadId::new(&row.thread_id),
                    row.rowid,
                    &recipient.address_id,
                    row.in_reply_to.as_deref(),
                ),
            _ => Ok(None),
        })
        .collect()
}

/// Keeps or queues one message for each relay author it addresses, and
/// returns each label it queued for, and the display name of each relay
/// author whose reply B could not keep. `answered` is what `answered` read
/// for it.
fn capture_one(
    relay: &RelayStore,
    endpoint: &Endpoint,
    row: &ToRelayAuthor,
    answered: &[Option<String>],
    now: i64,
) -> Result<(Vec<String>, Vec<String>), String> {
    let error = |error: crate::StoreError| error.to_string();
    if row.author_address_id.starts_with("relay:") {
        return Ok((Vec::new(), Vec::new()));
    }
    let mapped = relay
        .mapped_thread(&endpoint.key, &row.space, &row.thread_id)
        .map_err(error)?;
    let (mut labels, mut unkept) = (Vec::new(), Vec::new());
    for (recipient, answered) in row.relay_recipients.iter().zip(answered) {
        let Some((alias, origin)) = recipient.display.rsplit_once('@') else {
            return Err(format!("relay author {} has no host", recipient.address_id));
        };
        if is(recipient, RelayOrigin::Label(origin), alias) {
            queue_to(relay, endpoint, row, origin, alias, now)?;
            labels.push(origin.to_owned());
            continue;
        }
        let kept = match &mapped {
            Some((host_id, thread)) if is(recipient, RelayOrigin::Host(host_id), alias) => {
                keep_for(
                    relay,
                    endpoint,
                    row,
                    answered.as_deref(),
                    host_id,
                    thread,
                    now,
                )?
            }
            // Its thread's mapping expired, so no reply can reach it.
            _ => false,
        };
        if !kept {
            unkept.push(recipient.display.clone());
        }
    }
    Ok((labels, unkept))
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

/// On B: keeps `row` for the A host `host_id`, in reply to the A message
/// that relayed `answered`, the message of its recipient's it answers, and
/// says whether B kept it, which it does not once that A message is no
/// longer known or B's outbox for A is full.
fn keep_for(
    relay: &RelayStore,
    endpoint: &Endpoint,
    row: &ToRelayAuthor,
    answered: Option<&str>,
    host_id: &str,
    thread: &str,
    now: i64,
) -> Result<bool, String> {
    let error = |error: crate::StoreError| error.to_string();
    let re = match answered {
        Some(message) => relay.relayed_as(host_id, message).map_err(error)?,
        None => None,
    };
    let Some(re) = re else {
        return Ok(false);
    };
    let kept = relay
        .keep_reply(
            host_id,
            &Reply {
                seq: 0,
                re,
                thread: thread.to_owned(),
                from: row.author_alias.clone(),
                message_id: row.message_id.clone(),
                text: row.body.clone(),
            },
            now,
        )
        .map_err(error)?;
    relay
        .touch_thread(&endpoint.key, &row.space, &row.thread_id, now)
        .map_err(error)?;
    Ok(kept)
}

/// `text` cut so that its `deliver` request fits what the `rex` CLI sends.
fn fit(sender: &str, to: &str, text: &str) -> String {
    let mut limit = wire::TEXT_LIMIT;
    loop {
        let fitted = wire::cut(text, limit);
        if wire::deliver_size(sender, to, &fitted) <= wire::REQUEST_LIMIT || limit < 2048 {
            return fitted;
        }
        limit -= 1024;
    }
}
