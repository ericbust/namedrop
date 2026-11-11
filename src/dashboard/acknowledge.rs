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
    /// The request has no warning any more: it was dismissed already, or its
    /// answer came or can come again.
    NothingToDismiss,
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
    store
        .transaction(|tx| {
            Ok(
                match tx.acknowledge_disposition_attention(&namespace, &attempt, &by) {
                    Ok(true) => Ok(()),
                    Ok(false) => Err(Error::NothingToDismiss),
                    Err(error) if error.kind() != StoreErrorKind::InvalidInput => {
                        return Err(error);
                    }
                    Err(error)
                        if error.message()
                            == "delivery attempt does not exist in this workspace" =>
                    {
                        Err(Error::NotFound)
                    }
                    Err(_) if !tx.disposition_warning_open(&namespace, &attempt)? => {
                        Err(Error::NothingToDismiss)
                    }
                    Err(_) => Err(Error::Refused(
                        "The agent can still answer this request, so there's nothing to dismiss."
                            .to_owned(),
                    )),
                },
            )
        })
        .map_err(|_| Error::Unavailable)??;
    Ok(json!({"attempt_id": attempt, "acknowledged": true}))
}
