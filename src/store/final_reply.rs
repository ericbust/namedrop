use super::*;

impl StateTransaction<'_> {
    /// The disposition an attempt has, if any.
    pub(crate) fn delivery_disposition(
        &self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
    ) -> Result<Option<crate::DeliveryDisposition>, StoreError> {
        let kind = self
            .transaction
            .query_row(
                "SELECT disposition_kind FROM delivery_dispositions
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND attempt_id = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    attempt_id.as_str()
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        Ok(kind.map(|kind| match kind.as_str() {
            "reply" => crate::DeliveryDisposition::Reply,
            _ => crate::DeliveryDisposition::NoReply,
        }))
    }

    /// Whether the message an attempt delivers answers one its own recipient
    /// address wrote: the reply to a request this agent sent.
    pub(crate) fn answers_own_message(
        &self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
    ) -> Result<bool, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT 1
                 FROM delivery_attempts a
                 JOIN mailbox_deliveries d USING (
                     session_socket_id, worktree_id, delivery_id, address_id
                 )
                 JOIN collaboration_messages m
                   ON m.session_socket_id = d.session_socket_id
                  AND m.worktree_id = d.worktree_id
                  AND m.message_id = d.message_id
                 JOIN collaboration_messages parent
                   ON parent.session_socket_id = m.session_socket_id
                  AND parent.worktree_id = m.worktree_id
                  AND parent.message_id = m.in_reply_to
                 WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.attempt_id = ?3
                   AND parent.author_address_id = a.address_id",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    attempt_id.as_str()
                ],
                |_| Ok(()),
            )
            .optional()?
            .is_some())
    }

    /// Whether the attempt's recipient, in its own session, sent a message
    /// between `since_ms` and `until_ms` that asks someone besides the person
    /// and has no answer yet: the turn ended waiting on a helper. Its reply to
    /// the attempt's own message is not such a message.
    pub(crate) fn awaits_helper(
        &self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
        since_ms: u64,
        until_ms: u64,
    ) -> Result<bool, StoreError> {
        let unanswered = {
            let mut statement = self.transaction.prepare(
                "SELECT r.address_id
                 FROM delivery_attempts a
                 JOIN mailbox_deliveries d USING (
                     session_socket_id, worktree_id, delivery_id, address_id
                 )
                 JOIN collaboration_messages m
                   ON m.session_socket_id = a.session_socket_id
                  AND m.worktree_id = a.worktree_id
                  AND m.author_address_id = a.address_id
                  AND m.author_session_snapshot = a.agent_session_id
                  AND m.author_kind = 'agent_session'
                  AND m.created_at_ms >= ?4 AND m.created_at_ms <= ?5
                  AND m.in_reply_to IS NOT d.message_id
                 JOIN message_recipients r
                   ON r.session_socket_id = m.session_socket_id
                  AND r.worktree_id = m.worktree_id
                  AND r.message_id = m.message_id
                  AND r.address_id <> a.address_id
                 WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.attempt_id = ?3
                   AND NOT EXISTS (
                       SELECT 1 FROM collaboration_messages answer
                       WHERE answer.session_socket_id = m.session_socket_id
                         AND answer.worktree_id = m.worktree_id
                         AND answer.in_reply_to = m.message_id
                         AND answer.author_address_id = r.address_id)
                   AND NOT EXISTS (
                       SELECT 1 FROM mailbox_deliveries helper
                       JOIN delivery_attempts tried USING (
                           session_socket_id, worktree_id, delivery_id, address_id
                       )
                       JOIN delivery_dispositions closed USING (
                           session_socket_id, worktree_id, attempt_id
                       )
                       WHERE helper.session_socket_id = m.session_socket_id
                         AND helper.worktree_id = m.worktree_id
                         AND helper.message_id = m.message_id
                         AND helper.address_id = r.address_id)",
            )?;
            statement
                .query_map(
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        attempt_id.as_str(),
                        to_i64(since_ms)?,
                        to_i64(until_ms)?
                    ],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        if unanswered.is_empty() {
            return Ok(false);
        }
        let person = self.operator_projection_addresses(namespace)?;
        Ok(unanswered.iter().any(|address| !person.contains(address)))
    }
}
