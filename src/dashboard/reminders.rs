use super::snapshot::{SafeText, SpaceKey};
use crate::reminder::{Reminder, ReminderState, Repeat};
use crate::{PaneAddressId, StateNamespace, StateStore, StateTransaction, StoreError, ThreadId};
use serde::Serialize;
use serde_json::Value;
use std::path::Path;

pub(super) enum Conversation {
    Thread(String),
    Direct(String),
}

#[derive(Debug)]
pub(super) enum Error {
    NotFound,
    Unavailable,
}

#[derive(Serialize)]
pub(super) struct PendingReminder {
    reminder_id: String,
    owner: PaneAddressId,
    body: SafeText,
    due_at_ms: Option<u64>,
    repeats_left: u32,
    repeat_until_ms: Option<u64>,
}

impl From<Reminder> for PendingReminder {
    fn from(reminder: Reminder) -> Self {
        let (due_at_ms, ordinal) = match reminder.state {
            ReminderState::Scheduled { due_at_ms, ordinal } => (Some(due_at_ms), ordinal),
            ReminderState::AwaitingDelivery { ordinal, .. } => (None, ordinal),
            _ => unreachable!("pending query excludes terminal reminder states"),
        };
        let (repeats_left, repeat_until_ms) = match reminder.spec.repeat {
            Repeat::Once => (0, None),
            Repeat::Count { total, .. } => (total.saturating_sub(ordinal), None),
            Repeat::Until { until_ms, .. } => (100 - ordinal, Some(until_ms)),
        };
        Self {
            reminder_id: reminder.reminder_id,
            owner: reminder.owner.address,
            body: SafeText::from_untrusted(&reminder.spec.body, 2000),
            due_at_ms,
            repeats_left,
            repeat_until_ms,
        }
    }
}

fn namespace(store: &mut StateStore, key: &SpaceKey) -> Result<StateNamespace, Error> {
    store
        .dashboard_spaces()
        .map_err(|_| Error::Unavailable)?
        .into_iter()
        .find(|space| SpaceKey::of(&space.namespace) == *key)
        .map(|space| space.namespace)
        .ok_or(Error::NotFound)
}

fn scoped_thread(
    tx: &StateTransaction<'_>,
    ns: &StateNamespace,
    conversation: &Conversation,
) -> Result<Option<ThreadId>, StoreError> {
    let (thread, peer) = match conversation {
        Conversation::Thread(id) => (ThreadId::new(id), None),
        Conversation::Direct(id) => {
            let peer = PaneAddressId::new(id);
            (crate::store::direct_messages::thread_id(&peer), Some(peer))
        }
    };
    let direct = tx.direct_message(ns, &thread)?;
    Ok(match (peer, direct) {
        (None, None) => Some(thread),
        (Some(peer), Some(dm)) if dm.peer == peer => Some(thread),
        _ => None,
    })
}

pub(super) fn list(
    state: &Path,
    key: &SpaceKey,
    conversation: &Conversation,
) -> Result<Vec<PendingReminder>, Error> {
    let mut store = StateStore::open_read_only(state)
        .map_err(|_| Error::Unavailable)?
        .ok_or(Error::NotFound)?;
    let ns = namespace(&mut store, key)?;
    store
        .transaction(|tx| {
            let Some(thread) = scoped_thread(tx, &ns, conversation)? else {
                return Ok(None);
            };
            tx.conversation_reminders(&ns, &thread).map(Some)
        })
        .map_err(|_| Error::Unavailable)?
        .ok_or(Error::NotFound)
        .map(|rows| rows.into_iter().map(PendingReminder::from).collect())
}

pub(super) fn cancel(
    state: &Path,
    key: &SpaceKey,
    conversation: &Conversation,
    reminder: &str,
    now: u64,
) -> Result<Value, Error> {
    let mut store = StateStore::open(state).map_err(|_| Error::Unavailable)?;
    let ns = namespace(&mut store, key)?;
    store
        .transaction(|tx| {
            let Some(thread) = scoped_thread(tx, &ns, conversation)? else {
                return Ok(None);
            };
            tx.cancel_conversation_reminder(&ns, &thread, reminder, now)
        })
        .map_err(|_| Error::Unavailable)?
        .ok_or(Error::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reminder::{ReminderOwner, ReminderSpec};
    use crate::{
        ActorKind, AgentSessionId, DeliveryAttemptId, IdGenerator, LeaseDraft, MutationAttribution,
        PaneAddressDraft, PaneBindingId, PaneRoleId, ProcessIds,
    };

    #[test]
    fn operator_cancel_is_conversation_scoped_private_and_idempotent() {
        let state = std::env::temp_dir().join(ProcessIds::default().next_id("dashboard-reminders"));
        let ns = StateNamespace::for_space("test-socket", "test-space");
        let key = SpaceKey::of(&ns);
        let owner = ReminderOwner {
            address: PaneAddressId::new("agent"),
            session: AgentSessionId::new("session"),
            binding: PaneBindingId::new("binding"),
            pane: "pane".into(),
        };
        let at = MutationAttribution::new(ActorKind::Human, "test", 1);
        let mut store = StateStore::open(&state).unwrap();
        store
            .transaction(|tx| {
                tx.activate_worktree(&ns, &at)?;
                tx.create_pane_address(
                    &ns,
                    &PaneAddressDraft {
                        id: owner.address.clone(),
                        role_id: PaneRoleId::new("role"),
                        role_name: "agent".into(),
                        role_description: "fixture".into(),
                        alias: "codex".into(),
                    },
                    &at,
                )?;
                tx.bind_pane(&ns, &owner.binding, &owner.address, &owner.pane, &at)?;
                tx.observe_agent_session(
                    &ns,
                    "observation",
                    &owner.address,
                    &owner.session,
                    "idle",
                    &at,
                )?;
                let receipt = tx.send_direct_message(
                    &ns,
                    &owner.address,
                    "question",
                    "Check later",
                    &ProcessIds::default(),
                    &at,
                )?;
                tx.lease_mailbox_head(
                    &ns,
                    &owner.address,
                    &LeaseDraft {
                        attempt_id: DeliveryAttemptId::new("attempt"),
                        binding_id: owner.binding.clone(),
                        agent_session_id: owner.session.clone(),
                        retry_of_attempt_id: None,
                        owner: "test".into(),
                        acquired_at_ms: 2,
                        expires_at_ms: 1000,
                    },
                    &at,
                )?;
                tx.set_reminder(
                    &ns,
                    "reminder".into(),
                    owner.clone(),
                    ReminderSpec {
                        body: "Check the build".into(),
                        first_due_at_ms: 60_000,
                        in_reply_to: Some(receipt.receipt.message_id),
                        repeat: Repeat::Count {
                            interval_ms: 60_000,
                            total: 3,
                        },
                    },
                    "set",
                    "set",
                )?;
                Ok(())
            })
            .unwrap();
        let direct = Conversation::Direct("agent".into());
        let rows = list(&state, &key, &direct).unwrap_or_else(|_| panic!("list failed"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].owner, owner.address);
        assert_eq!(rows[0].body.text, "Check the build");
        assert_eq!(rows[0].due_at_ms, Some(60_000));
        assert_eq!(rows[0].repeats_left, 2);
        for wrong in [
            Conversation::Thread("another-thread".into()),
            Conversation::Thread("direct-message:agent".into()),
            Conversation::Direct("another-agent".into()),
        ] {
            assert!(matches!(
                cancel(&state, &key, &wrong, "reminder", 3),
                Err(Error::NotFound)
            ));
        }
        assert!(matches!(
            list(
                &state,
                &key,
                &Conversation::Thread("direct-message:agent".into())
            ),
            Err(Error::NotFound)
        ));
        let wrong_key = SpaceKey::of(&StateNamespace::for_space("other-socket", "test-space"));
        assert!(matches!(
            cancel(&state, &wrong_key, &direct, "reminder", 3),
            Err(Error::NotFound)
        ));
        assert_eq!(
            list(&state, &key, &direct)
                .unwrap_or_else(|_| panic!("list failed"))
                .len(),
            1
        );
        for _ in 0..2 {
            let receipt = cancel(&state, &key, &direct, "reminder", 3)
                .unwrap_or_else(|_| panic!("cancel failed"));
            assert_eq!(receipt["state"], "cancelled");
        }
        assert!(
            list(&state, &key, &direct)
                .unwrap_or_else(|_| panic!("list failed"))
                .is_empty()
        );
        std::fs::remove_dir_all(state).unwrap();
    }
}
