use super::snapshot::SpaceKey;
use crate::store::channel_routing::{ChannelDelivery, ChannelDeliveryRefusal};
use crate::{ActorKind, MessageId, MutationAttribution, StateNamespace, StateStore};
use serde_json::{Value, json};
use std::path::Path;

pub(super) enum DeliverError {
    Refused(ChannelDeliveryRefusal),
    Unavailable,
}

pub(super) fn apply(
    state: &Path,
    key: &SpaceKey,
    message: &str,
    aliases: &[String],
) -> Result<(StateNamespace, Value), DeliverError> {
    let namespace = StateStore::open_read_only(state)
        .and_then(|store| store.map(|mut store| store.dashboard_spaces()).transpose())
        .map_err(|_| DeliverError::Unavailable)?
        .and_then(|spaces| {
            spaces
                .into_iter()
                .find(|space| SpaceKey::of(&space.namespace) == *key)
                .map(|space| space.namespace)
        })
        .ok_or(DeliverError::Refused(ChannelDeliveryRefusal::NotFound))?;
    let actor = MutationAttribution::new(
        ActorKind::Human,
        "local-human",
        crate::Clock::now_unix_ms(&crate::SystemClock),
    );
    let mut store = StateStore::open(state).map_err(|_| DeliverError::Unavailable)?;
    let result = store
        .transaction(|tx| {
            tx.deliver_channel_message(&namespace, &MessageId::new(message), aliases, &actor)
        })
        .map_err(|_| DeliverError::Unavailable)?;
    match result {
        ChannelDelivery::Delivered(channel) => {
            let mut receipt = json!({"message_id": message, "channel": channel});
            super::snapshot::channel_routing::finish_post(state, &namespace, &mut receipt);
            Ok((namespace, receipt))
        }
        ChannelDelivery::Refused(reason) => Err(DeliverError::Refused(reason)),
    }
}
