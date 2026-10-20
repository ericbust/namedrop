use super::*;

const ITEM_ID_PREFIX: &str = "attention-agent-disposition-";

fn item_id(attempt: &crate::DeliveryAttemptId) -> crate::AttentionItemId {
    crate::AttentionItemId::new(format!("{ITEM_ID_PREFIX}{}", attempt.as_str()))
}

struct MissingDispositionFacts {
    disposed: bool,
    recipient_session: crate::AgentSessionId,
    recipient_retired: bool,
    recipient_session_now: RecipientSession,
    tracked: Option<TrackedRequest>,
}

pub(super) enum RecipientSession {
    Unknown,
    Observed(crate::AgentSessionId),
}

impl RecipientSession {
    pub(super) fn replaced(&self, pinned: &crate::AgentSessionId) -> bool {
        matches!(self, Self::Observed(current) if current != pinned)
    }
}

struct TrackedRequest {
    coordination: CoordinationState,
    is_current_request: bool,
}

enum CoordinationState {
    Open,
    Resolved(String),
}

impl From<String> for CoordinationState {
    fn from(outcome: String) -> Self {
        if outcome == "open" {
            Self::Open
        } else {
            Self::Resolved(outcome)
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum Unanswered {
    Untracked,
    RecipientRetired,
    RecipientSessionReplaced,
    CoordinationResolved { outcome: String },
}

fn unanswered(facts: &MissingDispositionFacts) -> Option<Unanswered> {
    if facts.disposed {
        return None;
    }
    let Some(tracked) = &facts.tracked else {
        return Some(Unanswered::Untracked);
    };
    if !tracked.is_current_request {
        return None;
    }
    if facts.recipient_retired {
        return Some(Unanswered::RecipientRetired);
    }
    if facts
        .recipient_session_now
        .replaced(&facts.recipient_session)
    {
        return Some(Unanswered::RecipientSessionReplaced);
    }
    if let CoordinationState::Resolved(outcome) = &tracked.coordination {
        return Some(Unanswered::CoordinationResolved {
            outcome: outcome.clone(),
        });
    }
    None
}

impl Unanswered {
    fn summary(&self) -> String {
        match self {
            Self::Untracked => "The agent received this message and ended its turn without a reply or a no-reply. Its exact session can still answer it from delivery.inbox. Once that agent is retired or its session was replaced, delivery acknowledge clears this warning.".to_owned(),
            Self::RecipientRetired => "The agent that received this tracked request was retired before it answered. Nothing can answer it now. Review it, then delivery acknowledge clears this warning.".to_owned(),
            Self::RecipientSessionReplaced => "The agent session that received this tracked request is gone and a new session is registered on its name. The new session cannot answer for it. Review it, then delivery acknowledge clears this warning.".to_owned(),
            Self::CoordinationResolved { outcome } => format!("This tracked request was still unanswered when its coordination was resolved {outcome}. The worker's exact session can still answer it from delivery.inbox. Once that agent is retired or its session was replaced, delivery acknowledge clears this warning."),
        }
    }
}

impl StateTransaction<'_> {
    /// Opens ND-DELIVERY-003 for a settled attempt that is owed to a human
    /// now. A warning already raised, or closed by a disposition or a human
    /// acknowledgement, is never raised again or reworded.
    pub(super) fn flag_missing_disposition(
        &mut self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
        by: &MutationAttribution,
    ) -> Result<(), StoreError> {
        if self.missing_disposition_judged(ns, attempt)? {
            return Ok(());
        }
        let facts = self.missing_disposition_facts(ns, attempt)?;
        let Some(reason) = unanswered(&facts) else {
            return Ok(());
        };
        self.open_attention_item(
            ns,
            &AttentionItemDraft {
                id: item_id(attempt),
                source_kind: crate::AttentionSourceKind::AgentDisposition,
                source_id: attempt.as_str().to_owned(),
                summary: reason.summary(),
            },
            by,
        )?;
        Ok(())
    }

    pub(super) fn reconcile_missing_dispositions(
        &mut self,
        ns: &StateNamespace,
        by: &MutationAttribution,
    ) -> Result<(), StoreError> {
        for attempt in self.unflagged_settled_requests(ns)? {
            self.flag_missing_disposition(ns, &attempt, by)?;
        }
        Ok(())
    }

    fn missing_disposition_judged(
        &self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
    ) -> Result<bool, StoreError> {
        Ok(self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM attention_items
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND attention_item_id = ?3)",
            params![
                ns.session_socket_id,
                ns.space_id.as_str(),
                item_id(attempt).as_str()
            ],
            |row| row.get(0),
        )?)
    }

    pub(super) fn recipient_session_now(
        &self,
        ns: &StateNamespace,
        address: &PaneAddressId,
    ) -> Result<RecipientSession, StoreError> {
        Ok(self
            .optional_routing_address_by_id(ns, address)?
            .and_then(|routing| routing.agent_session_id)
            .map_or(RecipientSession::Unknown, RecipientSession::Observed))
    }

    fn missing_disposition_facts(
        &self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
    ) -> Result<MissingDispositionFacts, StoreError> {
        let (address, session, retired, tracked) = self.transaction.query_row(
            "SELECT a.address_id, a.agent_session_id, p.retired, c.outcome,
                    t.current_message_id = d.message_id, t.coordination_id
             FROM delivery_attempts a
             JOIN mailbox_deliveries d USING (
                 session_socket_id, worktree_id, delivery_id, address_id
             )
             JOIN pane_addresses p USING (session_socket_id, worktree_id, address_id)
             LEFT JOIN (
                 SELECT l.session_socket_id, l.worktree_id, l.message_id,
                        g.recipient_address_id, g.recipient_session_id,
                        g.coordination_id, g.current_message_id
                 FROM coordination_links l
                 JOIN coordination_assignments g USING (
                     session_socket_id, worktree_id, assignment_id
                 )
                 WHERE l.kind = 'request'
             ) t ON t.session_socket_id = a.session_socket_id
                AND t.worktree_id = a.worktree_id
                AND t.message_id = d.message_id
                AND t.recipient_address_id = a.address_id
                AND t.recipient_session_id = a.agent_session_id
             LEFT JOIN coordinations c
               ON c.session_socket_id = t.session_socket_id
              AND c.worktree_id = t.worktree_id
              AND c.coordination_id = t.coordination_id
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.attempt_id = ?3",
            params![ns.session_socket_id, ns.space_id.as_str(), attempt.as_str()],
            |row| {
                let tracked = row
                    .get::<_, Option<String>>(5)?
                    .map(|_coordination_id| {
                        Ok::<_, rusqlite::Error>(TrackedRequest {
                            coordination: row.get::<_, String>(3)?.into(),
                            is_current_request: row.get(4)?,
                        })
                    })
                    .transpose()?;
                Ok((
                    PaneAddressId::new(row.get::<_, String>(0)?),
                    crate::AgentSessionId::new(row.get::<_, String>(1)?),
                    row.get::<_, bool>(2)?,
                    tracked,
                ))
            },
        )?;
        Ok(MissingDispositionFacts {
            disposed: self.delivery_disposition_exists(ns, attempt)?,
            recipient_session: session,
            recipient_retired: retired,
            recipient_session_now: self.recipient_session_now(ns, &address)?,
            tracked,
        })
    }

    /// Untracked attempts are judged once, at settle.
    fn unflagged_settled_requests(
        &self,
        ns: &StateNamespace,
    ) -> Result<Vec<crate::DeliveryAttemptId>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT a.attempt_id
             FROM delivery_attempts a
             JOIN delivery_attempt_state s USING (
                 session_socket_id, worktree_id, attempt_id, address_id
             )
             JOIN mailbox_deliveries d USING (
                 session_socket_id, worktree_id, delivery_id, address_id
             )
             JOIN coordination_links l
               ON l.session_socket_id = a.session_socket_id
              AND l.worktree_id = a.worktree_id
              AND l.message_id = d.message_id
              AND l.kind = 'request'
             JOIN coordination_assignments g
               ON g.session_socket_id = l.session_socket_id
              AND g.worktree_id = l.worktree_id
              AND g.assignment_id = l.assignment_id
              AND g.recipient_address_id = a.address_id
              AND g.recipient_session_id = a.agent_session_id
              AND g.current_message_id = d.message_id
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
               AND s.state = 'settled'
               AND NOT EXISTS (
                   SELECT 1 FROM delivery_dispositions p
                   WHERE p.session_socket_id = a.session_socket_id
                     AND p.worktree_id = a.worktree_id
                     AND p.attempt_id = a.attempt_id
               )
               AND NOT EXISTS (
                   SELECT 1 FROM attention_items i
                   WHERE i.session_socket_id = a.session_socket_id
                     AND i.worktree_id = a.worktree_id
                     AND i.attention_item_id = ?3 || a.attempt_id
               )
             ORDER BY a.created_at_ms, a.rowid",
        )?;
        statement
            .query_map(
                params![ns.session_socket_id, ns.space_id.as_str(), ITEM_ID_PREFIX],
                |row| Ok(crate::DeliveryAttemptId::new(row.get::<_, String>(0)?)),
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(id: &str) -> crate::AgentSessionId {
        crate::AgentSessionId::new(id)
    }

    fn awaited() -> TrackedRequest {
        TrackedRequest {
            coordination: CoordinationState::Open,
            is_current_request: true,
        }
    }

    fn facts(tracked: Option<TrackedRequest>) -> MissingDispositionFacts {
        MissingDispositionFacts {
            disposed: false,
            recipient_session: session("session-worker"),
            recipient_retired: false,
            recipient_session_now: RecipientSession::Observed(session("session-worker")),
            tracked,
        }
    }

    #[test]
    fn nothing_is_owed_while_an_open_assignment_awaits_the_request() {
        assert_eq!(unanswered(&facts(Some(awaited()))), None);
    }

    #[test]
    fn a_disposed_attempt_is_never_owed() {
        let mut disposed = facts(None);
        disposed.disposed = true;
        assert_eq!(unanswered(&disposed), None);
    }

    #[test]
    fn an_untracked_attempt_is_owed_at_settle() {
        assert_eq!(unanswered(&facts(None)), Some(Unanswered::Untracked));
    }

    #[test]
    fn an_unroutable_address_is_not_a_replaced_session() {
        let mut unroutable = facts(Some(awaited()));
        unroutable.recipient_session_now = RecipientSession::Unknown;
        assert_eq!(unanswered(&unroutable), None);
    }

    #[test]
    fn the_first_reason_that_applies_names_the_action() {
        let mut facts = facts(Some(TrackedRequest {
            coordination: CoordinationState::Resolved("cancelled".to_owned()),
            is_current_request: false,
        }));
        facts.recipient_retired = true;
        facts.recipient_session_now = RecipientSession::Observed(session("session-worker-2"));
        assert_eq!(
            unanswered(&facts),
            None,
            "a superseded request is never owed, whatever else is true"
        );
        facts.tracked.as_mut().unwrap().is_current_request = true;
        assert_eq!(unanswered(&facts), Some(Unanswered::RecipientRetired));
        facts.recipient_retired = false;
        assert_eq!(
            unanswered(&facts),
            Some(Unanswered::RecipientSessionReplaced)
        );
        facts.recipient_session_now = RecipientSession::Observed(session("session-worker"));
        assert_eq!(
            unanswered(&facts),
            Some(Unanswered::CoordinationResolved {
                outcome: "cancelled".to_owned()
            })
        );
        facts.tracked.as_mut().unwrap().coordination = CoordinationState::Open;
        assert_eq!(unanswered(&facts), None);
    }
}
