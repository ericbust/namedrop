//! The notices a sender on A gets in its thread from a remote agent's relay
//! author: that an item for that agent failed or expired, that the
//! Automatic Reply Limit holds that agent's replies, or that A refused one
//! or let held ones expire.

use super::{
    RemoteName,
    store::{Dropped, Held, RelayStore, Why},
};
use crate::{
    ActorKind, DeliveryId, IdGenerator, MessageId, MutationAttribution, SpaceId, StateNamespace,
    StateStore, mux::Endpoint, store::Relayed,
};
use std::path::Path;

/// One notice: from the relay author of `alias` on `label`, in reply to
/// the A message `message` in `space`, to that message's sender.
struct Notice<'a> {
    space: &'a str,
    label: &'a str,
    alias: &'a str,
    message: &'a str,
    request_id: String,
    text: String,
}

/// Tells the sender of each item of `endpoint` that failed or expired,
/// of each thread whose replies are held, of each reply A refused, and of
/// each thread whose held replies expired, once, and calls `wake` for
/// each space it told. A fault leaves the rest for the next call.
pub(crate) fn tell(
    state: &Path,
    endpoint: &Endpoint,
    gate: super::worker::Gate<'_>,
    wake: &mut dyn FnMut(&SpaceId),
) {
    let now = super::now_ms();
    let Ok(store) = RelayStore::open(state, now) else {
        return;
    };
    let (Ok(unsent), Ok(held), Ok(dropped)) = (
        store.unsent(&endpoint.key),
        store.held(&endpoint.key, true),
        store.dropped(&endpoint.key),
    ) else {
        return;
    };
    for item in &unsent {
        let told = notice(
            state,
            gate,
            &endpoint.key,
            &Notice {
                space: &item.space,
                label: &item.label,
                alias: &item.recipient,
                message: &item.message_id,
                request_id: format!(
                    "relay-unsent:{}:{}:{}",
                    item.label, item.message_id, item.recipient
                ),
                text: format!(
                    "Namedrop could not relay this message to @{}@{}: {}.",
                    item.recipient, item.label, item.reason
                ),
            },
        );
        if !settle(told, &item.space, wake, || store.told(item, now)) {
            return;
        }
    }
    for held in &held {
        let told = notice(state, gate, &endpoint.key, &held_notice(held));
        if !settle(told, &held.space, wake, || store.told_held(held)) {
            return;
        }
    }
    for dropped in &dropped {
        let told = notice(state, gate, &endpoint.key, &dropped_notice(dropped));
        if !settle(told, &dropped.space, wake, || {
            store.told_dropped(&endpoint.key, dropped)
        }) {
            return;
        }
    }
}

/// The notice that A refused a reply, or that the held replies of a
/// thread expired.
fn dropped_notice(dropped: &Dropped) -> Notice<'_> {
    let (alias, label) = (&dropped.remote_alias, &dropped.label);
    Notice {
        space: &dropped.space,
        label,
        alias,
        message: &dropped.re,
        request_id: format!(
            "relay-dropped:{label}:{}:{}",
            dropped.host_id, dropped.remote_message_id
        ),
        text: match &dropped.why {
            Why::Refused(reason) => {
                format!("Namedrop could not take a reply from @{alias}@{label}: {reason}.")
            }
            Why::Expired(count) => format!(
                "Namedrop let go of {} from @{alias}@{label} that it held for 7 days while the \
                 Automatic Reply Limit paused this thread.",
                if *count == 1 {
                    "1 reply".to_owned()
                } else {
                    format!("{count} replies")
                }
            ),
        },
    }
}

/// The notice that the Automatic Reply Limit holds `held`'s replies.
fn held_notice(held: &Held) -> Notice<'_> {
    Notice {
        space: &held.space,
        label: &held.label,
        alias: &held.reply.from,
        message: &held.reply.re,
        request_id: format!(
            "relay-held:{}:{}:{}",
            held.label, held.host_id, held.reply.message_id
        ),
        text: format!(
            "The Automatic Reply Limit paused this thread, so Namedrop is holding the \
             replies from @{}@{}. A person's message in this thread, or Continue, resumes \
             it, and the held replies follow.",
            held.reply.from, held.label
        ),
    }
}

/// Records a notice's outcome with `told` and wakes its space when it was
/// inserted, and says whether to go on.
fn settle(
    told: Result<bool, String>,
    space: &str,
    wake: &mut dyn FnMut(&SpaceId),
    record: impl FnOnce() -> Result<(), crate::StoreError>,
) -> bool {
    let Ok(inserted) = told else {
        return false;
    };
    if record().is_err() {
        return false;
    }
    if inserted {
        wake(&SpaceId::new(space));
    }
    true
}

/// Inserts `notice` under the command gate. `false` when A can no longer
/// take it, as when the sender is gone.
fn notice(
    state: &Path,
    gate: super::worker::Gate<'_>,
    endpoint: &str,
    notice: &Notice<'_>,
) -> Result<bool, String> {
    let gate = gate()?;
    let namespace = StateNamespace::for_space(endpoint, notice.space);
    let author = RemoteName {
        alias: notice.alias.to_owned(),
        label: notice.label.to_owned(),
    }
    .author();
    let message = MessageId::new(notice.message);
    let ids = crate::ProcessIds::default();
    let attribution = MutationAttribution::new(
        ActorKind::AgentSession,
        author.address_id.as_str(),
        u64::try_from(super::now_ms()).unwrap_or_default(),
    );
    let mut main = StateStore::open(state).map_err(|error| error.to_string())?;
    let inserted = main.transaction(|transaction| {
        let route = transaction.message_route(&namespace, &message)?;
        let sender =
            transaction.routing_address_by_id(&namespace, &route.participants[0].address_id)?;
        transaction.insert_relayed_message(
            &namespace,
            &Relayed {
                author: &author,
                request_id: notice.request_id.clone(),
                thread: &route.thread_id,
                message_id: MessageId::new(ids.next_id("message")),
                delivery_id: DeliveryId::new(ids.next_id("delivery")),
                in_reply_to: Some(&message),
                recipient: &sender,
                text: &notice.text,
                notice: true,
            },
            &attribution,
        )
    });
    drop(gate);
    match inserted {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == crate::StoreErrorKind::InvalidInput => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}
