use super::snapshot::SpaceKey;
use crate::{ActorKind, DeliveryAttemptId, MutationAttribution, StateStore, StoreErrorKind};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

/// What the person confirms by pressing Dismiss on a lost answer's card.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DismissPost {
    #[serde(default)]
    confirmed: bool,
}

pub(super) enum Error {
    NotFound,
    /// The warning can't be dismissed now, with the reason to show.
    Refused(String),
    Unavailable,
}

/// Dismisses the warning that a request's answer can never arrive, as
/// `delivery acknowledge` does: by the person, only once nobody can answer
/// it, recording no disposition and resending nothing.
pub(super) fn dismiss(
    state: &Path,
    key: &SpaceKey,
    attempt: &str,
    post: &DismissPost,
    now: u64,
) -> Result<Value, Error> {
    if !post.confirmed {
        return Err(Error::Refused(
            "Confirm that nobody can answer this request before you dismiss it.".to_owned(),
        ));
    }
    let mut store = StateStore::open(state).map_err(|_| Error::Unavailable)?;
    let namespace = store
        .dashboard_spaces()
        .map_err(|_| Error::Unavailable)?
        .into_iter()
        .find(|space| SpaceKey::of(&space.namespace) == *key)
        .map(|space| space.namespace)
        .ok_or(Error::NotFound)?;
    let attempt = DeliveryAttemptId::new(attempt);
    let by = MutationAttribution::new(ActorKind::Human, "local-human", now);
    let changed = store
        .transaction(|tx| tx.acknowledge_disposition_attention(&namespace, &attempt, &by))
        .map_err(|error| match error.kind() {
            StoreErrorKind::InvalidInput
                if error.message() == "delivery attempt does not exist in this workspace" =>
            {
                Error::NotFound
            }
            StoreErrorKind::InvalidInput => Error::Refused(refusal(error.message())),
            _ => Error::Unavailable,
        })?;
    Ok(json!({"attempt_id": attempt, "acknowledged": true, "changed": changed}))
}

/// The person's words for why the store refused, which speaks to agents.
fn refusal(message: &str) -> String {
    if message.starts_with("the recipient address is not retired") {
        "The agent can still answer this request, so there's nothing to dismiss.".to_owned()
    } else {
        "There's no warning to dismiss on this request.".to_owned()
    }
}
