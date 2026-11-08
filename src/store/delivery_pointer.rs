//! The per-dispatch pointer a delivery prompt names. `delivery_attempts` is
//! immutable, so the nonce lives on the attempt's mutable state row: a lease
//! sets it, a requeue clears it, and every re-dispatch is a new attempt with a
//! new one.
use super::*;

const COLUMNS: [(&str, &str); 2] = [
    (
        "pointer_nonce",
        "ALTER TABLE delivery_attempt_state ADD COLUMN pointer_nonce TEXT",
    ),
    (
        "pointer_presented_at_ms",
        "ALTER TABLE delivery_attempt_state ADD COLUMN pointer_presented_at_ms INTEGER",
    ),
];

/// Adds only what is missing, so a retry after any interruption converges.
pub(super) fn add_columns(connection: &Connection) -> Result<(), StoreError> {
    for (name, ddl) in COLUMNS {
        let present: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('delivery_attempt_state') WHERE name = ?1)",
            [name],
            |row| row.get(0),
        )?;
        if !present {
            connection.execute_batch(ddl)?;
        }
    }
    Ok(())
}

/// What a lease writes for a new attempt's nonce. SQLite's randomness keeps the
/// draw inside the lease transaction; the nonce only selects, because the hook
/// still compares the whole prompt.
pub(super) const NEW_NONCE_SQL: &str = "lower(hex(randomblob(4)))";

/// The durable facts a pointer prompt is rendered from, beyond its envelope.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct DeliveryPointer {
    pub nonce: String,
    /// A prompt hook already attested this dispatch.
    pub presented: bool,
    /// The person wrote the message, from their own address or an envoy of it.
    pub from_person: bool,
}

impl StateTransaction<'_> {
    /// The attempt's pointer, or `None` when it has no nonce: it was leased
    /// before pointers existed, or it was requeued.
    pub(crate) fn delivery_pointer(
        &self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
    ) -> Result<Option<DeliveryPointer>, StoreError> {
        let row = self
            .transaction
            .query_row(
                "SELECT s.pointer_nonce, s.pointer_presented_at_ms IS NOT NULL, m.author_address_id
                 FROM delivery_attempt_state s
                 JOIN delivery_attempts a USING (
                     session_socket_id, worktree_id, attempt_id, address_id
                 )
                 JOIN mailbox_deliveries d USING (
                     session_socket_id, worktree_id, delivery_id, address_id
                 )
                 JOIN collaboration_messages m USING (
                     session_socket_id, worktree_id, message_id
                 )
                 WHERE s.session_socket_id = ?1 AND s.worktree_id = ?2 AND s.attempt_id = ?3
                   AND s.pointer_nonce IS NOT NULL",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    attempt_id.as_str()
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, bool>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((nonce, presented, author)) = row else {
            return Ok(None);
        };
        Ok(Some(DeliveryPointer {
            nonce,
            presented,
            from_person: self
                .operator_projection_addresses(namespace)?
                .contains(&author),
        }))
    }

    /// Records the one presentation a hook attests. Later ones find it set.
    pub(crate) fn mark_pointer_presented(
        &self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
        now_ms: u64,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "UPDATE delivery_attempt_state SET pointer_presented_at_ms = ?4
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND attempt_id = ?3
               AND pointer_presented_at_ms IS NULL",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                attempt_id.as_str(),
                to_i64(now_ms)?
            ],
        )?;
        Ok(())
    }
}
