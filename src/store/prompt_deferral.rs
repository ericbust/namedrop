use super::*;
use crate::{AgentSessionId, DeliveryAttemptId, PaneBindingId};

fn recipient_key(pane: &str, session: &AgentSessionId) -> String {
    serde_json::to_string(&(pane, session.as_str())).expect("string pair serializes")
}

impl StateTransaction<'_> {
    fn prompt_deferral(
        &self,
        namespace: &StateNamespace,
        pane: &str,
        session: &AgentSessionId,
    ) -> Result<Option<u64>, StoreError> {
        let latest = self
            .transaction
            .query_row(
                "SELECT transition_kind, occurred_at_ms FROM transition_facts
             WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND entity_kind = 'prompt_recipient' AND entity_id = ?3
             ORDER BY worktree_order DESC LIMIT 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    recipient_key(pane, session)
                ],
                |row| Ok((row.get::<_, String>(0)?, from_i64(row.get(1)?)?)),
            )
            .optional()?;
        Ok(latest.and_then(|(kind, time)| (kind == "paused").then_some(time)))
    }

    pub(super) fn binding_prompt_is_deferred(
        &self,
        namespace: &StateNamespace,
        binding: &PaneBindingId,
        session: &AgentSessionId,
    ) -> Result<bool, StoreError> {
        let pane = self.transaction.query_row(
            "SELECT pane_id FROM pane_bindings WHERE session_socket_id = ?1 AND worktree_id = ?2
               AND binding_id = ?3 AND active = 1",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                binding.as_str()
            ],
            |row| row.get::<_, String>(0),
        )?;
        self.prompt_is_deferred(namespace, &pane, session)
    }

    pub(crate) fn prompt_is_deferred(
        &self,
        namespace: &StateNamespace,
        pane: &str,
        session: &AgentSessionId,
    ) -> Result<bool, StoreError> {
        Ok(self.prompt_deferral(namespace, pane, session)?.is_some())
    }

    pub(crate) fn defer_prompt_until_settle(
        &mut self,
        namespace: &StateNamespace,
        pane: &str,
        session: &AgentSessionId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        self.append_transition(
            namespace,
            TransitionEntityKind::PromptRecipient,
            &recipient_key(pane, session),
            TransitionKind::Paused,
            attribution,
        )
    }

    pub(crate) fn resume_prompt_after_settle(
        &mut self,
        namespace: &StateNamespace,
        pane: &str,
        session: &AgentSessionId,
        status_ms: u64,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        if self
            .prompt_deferral(namespace, pane, session)?
            .is_some_and(|deferred| status_ms > deferred)
        {
            self.append_transition(
                namespace,
                TransitionEntityKind::PromptRecipient,
                &recipient_key(pane, session),
                TransitionKind::Resumed,
                attribution,
            )?;
        }
        Ok(())
    }

    pub(crate) fn defer_delivery_attempt_until_settle(
        &mut self,
        namespace: &StateNamespace,
        attempt: &DeliveryAttemptId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let (delivery, pane, session, state) = self.transaction.query_row(
            "SELECT a.delivery_id, b.pane_id, a.agent_session_id, s.state
             FROM delivery_attempts a JOIN delivery_attempt_state s
               USING (session_socket_id, worktree_id, attempt_id, address_id)
             JOIN pane_bindings b USING (session_socket_id, worktree_id, binding_id, address_id)
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.attempt_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                attempt.as_str()
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    AgentSessionId::new(row.get::<_, String>(2)?),
                    row.get::<_, String>(3)?,
                ))
            },
        )?;
        if state != "dispatching" {
            return Err(StoreError::invalid_input(
                "only an unused dispatch reservation can wait for settle",
            ));
        }
        self.transaction.execute(
            "UPDATE delivery_attempt_state SET state = 'cancelled', active = 0
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND attempt_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                attempt.as_str()
            ],
        )?;
        self.transaction.execute(
            "UPDATE mailbox_deliveries SET state = 'queued'
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND delivery_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                delivery
            ],
        )?;
        self.transaction.execute(
            "DELETE FROM leases WHERE session_socket_id = ?1 AND worktree_id = ?2 AND attempt_id = ?3",
            params![namespace.session_socket_id, namespace.space_id.as_str(), attempt.as_str()],
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::DeliveryAttempt,
            attempt.as_str(),
            TransitionKind::Cancelled,
            attribution,
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::MailboxDelivery,
            &delivery,
            TransitionKind::Queued,
            attribution,
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::PromptRecipient,
            &recipient_key(&pane, &session),
            TransitionKind::Paused,
            attribution,
        )
    }
}
