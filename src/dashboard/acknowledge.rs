use super::snapshot::SpaceKey;
use crate::{ActorKind, MutationAttribution, StateStore};
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

/// What the person confirms by pressing Dismiss on any card, or Dismiss all.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DismissItemsPost {
    #[serde(default)]
    confirmed: bool,
    /// The ids of the cards the person saw, so one that arrives while they
    /// decide is not dismissed unseen.
    ids: Vec<String>,
}

/// More cards than one space ever shows.
const MAX_DISMISSED_AT_ONCE: usize = 500;

pub(super) enum Error {
    NotFound,
    /// None of the named cards is there any more: each was dismissed
    /// already, or its source closed it.
    NothingToDismiss,
    /// The post can't be acted on, with the reason to show.
    Refused(String),
    Unavailable,
}

/// Dismisses the attention items the person names, and says how many there
/// were. Each is recorded as theirs. Nothing is resent, no disposition is
/// recorded, and an agent that can still answer still can.
pub(super) fn dismiss_items(
    state: &Path,
    key: &SpaceKey,
    post: &DismissItemsPost,
    now: u64,
) -> Result<Value, Error> {
    if !post.confirmed {
        return Err(Error::Refused(
            "Confirm before you dismiss a warning.".to_owned(),
        ));
    }
    if post.ids.is_empty() || post.ids.len() > MAX_DISMISSED_AT_ONCE {
        return Err(Error::Refused(format!(
            "Name between 1 and {MAX_DISMISSED_AT_ONCE} warnings to dismiss."
        )));
    }
    let mut store = StateStore::open(state).map_err(|_| Error::Unavailable)?;
    let namespace = store
        .dashboard_spaces()
        .map_err(|_| Error::Unavailable)?
        .into_iter()
        .find(|space| SpaceKey::of(&space.namespace) == *key)
        .map(|space| space.namespace)
        .ok_or(Error::NotFound)?;
    let by = MutationAttribution::new(ActorKind::Human, "local-human", now);
    let dismissed = store
        .transaction(|tx| {
            let mut dismissed = 0;
            for id in &post.ids {
                if tx.dismiss_attention_item(&namespace, id, &by)? {
                    dismissed += 1;
                }
            }
            Ok(dismissed)
        })
        .map_err(|_| Error::Unavailable)?;
    if dismissed == 0 {
        return Err(Error::NothingToDismiss);
    }
    Ok(json!({"dismissed": dismissed}))
}
