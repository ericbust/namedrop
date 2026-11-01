use super::*;

impl StateTransaction<'_> {
    pub(crate) fn deferred_worker_active(&self, socket: &str) -> Result<bool, StoreError> {
        self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM worktrees WHERE session_socket_id = ?1 AND activated = 1 AND deactivated = 0)",
            [socket], |row| row.get(0),
        ).map_err(Into::into)
    }

    pub(crate) fn deferred_message_committed(
        &self,
        namespace: &StateNamespace,
        address: &PaneAddressId,
        session: &crate::AgentSessionId,
        request_id: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.transaction.query_row(
            "SELECT 1 FROM collaboration_messages WHERE session_socket_id = ?1 AND worktree_id = ?2
             AND author_address_id = ?3 AND author_session_snapshot = ?4 AND request_id = ?5",
            params![namespace.session_socket_id, namespace.space_id.as_str(), address.as_str(), session.as_str(), request_id],
            |_| Ok(()),
        ).optional()?.is_some())
    }

    pub(crate) fn deferred_reply_matches_attempt(
        &self,
        namespace: &StateNamespace,
        message: &crate::MessageId,
        attempt: &crate::DeliveryAttemptId,
        session: &crate::AgentSessionId,
    ) -> Result<bool, StoreError> {
        Ok(self.transaction.query_row(
            "SELECT 1 FROM delivery_attempts a JOIN delivery_dispositions d
             USING (session_socket_id, worktree_id, attempt_id)
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.attempt_id = ?3
               AND a.agent_session_id = ?4 AND d.disposition_kind = 'reply' AND d.disposition_id = ?5",
            params![namespace.session_socket_id, namespace.space_id.as_str(), attempt.as_str(), session.as_str(), message.as_str()],
            |_| Ok(()),
        ).optional()?.is_some())
    }

    pub(crate) fn deferred_attempt_committed(
        &self,
        namespace: &StateNamespace,
        attempt: &str,
        session: &crate::AgentSessionId,
        complete: bool,
    ) -> Result<bool, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT 1 FROM delivery_attempts a
             JOIN delivery_acknowledgements r USING (session_socket_id, worktree_id, attempt_id)
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.attempt_id = ?3
               AND a.agent_session_id = ?4 AND r.agent_session_id = ?4
               AND (?5 = 0 OR EXISTS (
                 SELECT 1 FROM delivery_dispositions d
                 WHERE d.session_socket_id = a.session_socket_id AND d.worktree_id = a.worktree_id
                   AND d.attempt_id = a.attempt_id AND d.disposition_kind = 'no_reply'))",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    attempt,
                    session.as_str(),
                    complete
                ],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }
}
