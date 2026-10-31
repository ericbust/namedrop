use super::*;
use std::collections::BTreeMap;

struct LegacyMailboxPause {
    namespace: StateNamespace,
    delivery_id: String,
    recipient_key: String,
    order: u64,
}

impl StateTransaction<'_> {
    pub(super) fn migrate_mailbox_deferrals(&mut self) -> Result<(), StoreError> {
        let pauses = {
            let mut statement = self.transaction.prepare(
                "SELECT paused.session_socket_id, paused.worktree_id, a.delivery_id,
                        paused.entity_id, paused.worktree_order
                 FROM transition_facts paused
                 JOIN transition_facts observed
                   ON observed.session_socket_id = paused.session_socket_id AND observed.worktree_id = paused.worktree_id
                   AND observed.worktree_order = paused.worktree_order - 1
                   AND observed.entity_kind = paused.entity_kind AND observed.entity_id = paused.entity_id
                   AND observed.transition_kind = 'observed'
                 JOIN transition_facts queued
                   ON queued.session_socket_id = paused.session_socket_id AND queued.worktree_id = paused.worktree_id
                   AND queued.worktree_order = paused.worktree_order - 2
                   AND queued.entity_kind = 'mailbox_delivery' AND queued.transition_kind = 'queued'
                 JOIN transition_facts cancelled
                   ON cancelled.session_socket_id = paused.session_socket_id AND cancelled.worktree_id = paused.worktree_id
                   AND cancelled.worktree_order = paused.worktree_order - 3
                   AND cancelled.entity_kind = 'delivery_attempt' AND cancelled.transition_kind = 'cancelled'
                 JOIN delivery_attempts a
                   ON a.session_socket_id = cancelled.session_socket_id AND a.worktree_id = cancelled.worktree_id
                   AND a.attempt_id = cancelled.entity_id AND a.delivery_id = queued.entity_id
                 JOIN delivery_attempt_state s USING (session_socket_id, worktree_id, attempt_id, address_id)
                 JOIN pane_bindings b USING (session_socket_id, worktree_id, binding_id, address_id)
                 WHERE paused.entity_kind = 'prompt_recipient' AND paused.transition_kind = 'paused'
                   AND paused.entity_id = json_array(b.pane_id, a.agent_session_id)
                   AND s.state = 'cancelled' AND s.active = 0
                   AND NOT EXISTS (
                       SELECT 1 FROM transition_facts sent
                       WHERE sent.session_socket_id = a.session_socket_id AND sent.worktree_id = a.worktree_id
                         AND sent.entity_kind = 'delivery_attempt' AND sent.entity_id = a.attempt_id
                         AND sent.transition_kind IN ('submitted','invoked','acknowledged','settled','held','failed')
                   )
                 ORDER BY paused.worktree_order",
            )?;
            statement
                .query_map([], |row| {
                    Ok(LegacyMailboxPause {
                        namespace: StateNamespace::for_space(
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                        ),
                        delivery_id: row.get(2)?,
                        recipient_key: row.get(3)?,
                        order: from_i64(row.get(4)?)?,
                    })
                })?
                .collect::<Result<Vec<_>, _>>()?
        };
        if pauses.is_empty() {
            return Ok(());
        }
        let trigger: String = self.transaction.query_row(
            "SELECT sql FROM sqlite_schema WHERE name = 'immutable_transitions_update'",
            [],
            |row| row.get(0),
        )?;
        self.transaction
            .execute_batch("DROP TRIGGER immutable_transitions_update;")?;
        let mut latest = BTreeMap::new();
        for pause in pauses {
            self.transaction.execute(
                "UPDATE transition_facts SET entity_kind = 'mailbox_delivery', entity_id = ?3
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND worktree_order IN (?4, ?5)",
                params![
                    pause.namespace.session_socket_id,
                    pause.namespace.space_id.as_str(),
                    pause.delivery_id,
                    to_i64(pause.order - 1)?,
                    to_i64(pause.order)?
                ],
            )?;
            latest.insert(
                (
                    pause.namespace.session_socket_id.clone(),
                    pause.namespace.space_id.clone(),
                    pause.delivery_id.clone(),
                ),
                pause,
            );
        }
        self.transaction.execute_batch(&trigger)?;
        for pause in latest.into_values() {
            let resumed = self
                .transaction
                .query_row(
                    "SELECT actor_kind, actor_id, occurred_at_ms FROM transition_facts
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND entity_kind = 'prompt_recipient' AND entity_id = ?3
                   AND transition_kind = 'resumed' AND worktree_order > ?4
                 ORDER BY worktree_order DESC LIMIT 1",
                    params![
                        pause.namespace.session_socket_id,
                        pause.namespace.space_id.as_str(),
                        pause.recipient_key,
                        to_i64(pause.order)?
                    ],
                    |row| {
                        Ok(MutationAttribution::new(
                            parse_actor_kind(row.get(0)?)?,
                            row.get::<_, String>(1)?,
                            from_i64(row.get(2)?)?,
                        ))
                    },
                )
                .optional()?;
            if let Some(attribution) = resumed {
                self.append_transition(
                    &pause.namespace,
                    TransitionEntityKind::MailboxDelivery,
                    &pause.delivery_id,
                    TransitionKind::Resumed,
                    &attribution,
                )?;
            }
        }
        Ok(())
    }
}
