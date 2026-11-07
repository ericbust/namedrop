use super::*;

impl StateTransaction<'_> {
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
}
