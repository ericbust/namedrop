//! The notice a sender on A gets in its thread when an item for a remote
//! agent failed or expired, from that agent's relay author.

use super::{
    RemoteName,
    store::{RelayStore, Unsent},
};
use crate::{
    ActorKind, DeliveryId, IdGenerator, MessageId, MutationAttribution, SpaceId, StateNamespace,
    StateStore, mux::Endpoint, store::Relayed,
};
use std::path::Path;

/// Tells the sender of each item of `endpoint` that failed or expired, once,
/// and calls `wake` for each space it told. A fault leaves the rest for the
/// next call.
pub(crate) fn tell(state: &Path, endpoint: &Endpoint, wake: &mut dyn FnMut(&SpaceId)) {
    let now = super::now_ms();
    let Ok(store) = RelayStore::open(state, now) else {
        return;
    };
    let Ok(unsent) = store.unsent(&endpoint.key) else {
        return;
    };
    for item in &unsent {
        let Ok(gate) = crate::runtime_command_guard(state, None) else {
            return;
        };
        let told = notice(state, &endpoint.key, item, now);
        drop(gate);
        match told {
            Ok(inserted) => {
                if store.told(item, now).is_err() {
                    return;
                }
                if inserted {
                    wake(&SpaceId::new(&item.space));
                }
            }
            Err(_) => return,
        }
    }
}

/// Inserts the notice for `item` in its thread, in reply to its message,
/// to the message's sender. `false` when A can no longer take it, as when
/// the sender is gone.
fn notice(state: &Path, endpoint: &str, item: &Unsent, now: i64) -> Result<bool, String> {
    let namespace = StateNamespace::for_space(endpoint, &item.space);
    let name = RemoteName {
        alias: item.recipient.clone(),
        label: item.label.clone(),
    };
    let author = name.author();
    let message = MessageId::new(&item.message_id);
    let ids = crate::ProcessIds::default();
    let attribution = MutationAttribution::new(
        ActorKind::AgentSession,
        author.address_id.as_str(),
        u64::try_from(now).unwrap_or_default(),
    );
    let text = format!(
        "Namedrop could not relay this message to @{}@{}: {}.",
        item.recipient, item.label, item.reason
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
                request_id: format!(
                    "relay-unsent:{}:{}:{}",
                    item.label, item.message_id, item.recipient
                ),
                thread: &route.thread_id,
                message_id: MessageId::new(ids.next_id("message")),
                delivery_id: DeliveryId::new(ids.next_id("delivery")),
                in_reply_to: Some(&message),
                recipient: &sender,
                text: &text,
            },
            &attribution,
        )
    });
    match inserted {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == crate::StoreErrorKind::InvalidInput => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}
