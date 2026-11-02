//! The operator's review grant. Pressing "Ask to review" in Channels is the
//! human's confirmation for one coordination: it lets one agent session
//! acknowledge that coordination's warning once, within a short window, while
//! the dashboard session that pressed it is still the current one.
//!
//! A grant is facts, not a table: `opened` when the operator presses the
//! button, `resolved` when the agent consumes it. The latest fact on the
//! entity `{coordination_id}:{agent_session_id}` is its state, and its expiry
//! is derived from the `opened` time.
use super::*;
use crate::{AgentSessionId, CoordinationId};
use sha2::{Digest, Sha256};

pub const REVIEW_GRANT_TTL_MS: u64 = 15 * 60 * 1000;

const GRANT_NOT_ACKNOWLEDGEABLE: &str = "this handoff can't be acknowledged in its current state: only a blocked or cancelled handoff, an open one whose lead address is retired, or a completed one whose report delivery was cancelled can be. Continue or resolve it instead";

/// The dashboard session that issued a grant, as a fingerprint of its secret.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OperatorSession(String);

impl OperatorSession {
    pub fn of_secret(secret: &str) -> Self {
        let mut digest = Sha256::new();
        digest.update(b"namedrop-review-grant/v1\0");
        digest.update(secret.as_bytes());
        Self(crate::lower_hex(&digest.finalize()[..8]))
    }

    fn actor_id(&self) -> String {
        format!("dashboard:{}", self.0)
    }
}

/// A grant still waiting for its agent, as the dashboard shows it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OpenReviewGrant {
    pub coordination_id: String,
    pub grantee_address_id: String,
    pub granted_at_ms: u64,
    pub expires_at_ms: u64,
}

/// A coordination warning an agent cleared with a grant.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GrantedClear {
    pub coordination_id: String,
    pub deliverable: String,
    pub address_id: String,
    pub at_ms: u64,
}

/// Why a coordination warning is open, in the terms the operator acts on.
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningCause {
    Blocked,
    Cancelled,
    /// Open with a live lead: its continuation ended, is held or is paused.
    Stalled,
    LeadRetired,
    /// Completed, but its report is missing or its delivery did not settle.
    Report,
}

impl WarningCause {
    fn of(outcome: &str, lead_retired: bool) -> Self {
        match outcome {
            "blocked" => Self::Blocked,
            "cancelled" => Self::Cancelled,
            "open" if lead_retired => Self::LeadRetired,
            "open" => Self::Stalled,
            _ => Self::Report,
        }
    }
}

/// One open coordination warning with what the operator needs to decide.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoordinationWarning {
    pub coordination_id: CoordinationId,
    pub cause: WarningCause,
    pub deliverable: String,
    pub reason: Option<String>,
    pub report: Option<String>,
    pub summary: String,
    pub lead_address_id: PaneAddressId,
    pub lead_alias: String,
    pub lead_retired: bool,
}

/// The alias an address `p` goes by, or went by before it was retired.
const ALIAS_OF_P: &str = "COALESCE(
    (SELECT c.alias FROM alias_claims c WHERE c.session_socket_id=p.session_socket_id
       AND c.worktree_id=p.worktree_id AND c.address_id=p.address_id AND c.claim_state='active'),
    (SELECT h.alias FROM alias_history h WHERE h.session_socket_id=p.session_socket_id
       AND h.worktree_id=p.worktree_id AND h.address_id=p.address_id AND h.transition_kind='claimed'
     ORDER BY h.history_order DESC LIMIT 1),
    (SELECT ltrim(r.name,'@') FROM pane_roles r WHERE r.session_socket_id=p.session_socket_id
       AND r.worktree_id=p.worktree_id AND r.role_id=p.role_id))";

fn grant_entity(id: &CoordinationId, session: &AgentSessionId) -> String {
    format!("{}:{}", id.as_str(), session.as_str())
}

impl StateTransaction<'_> {
    pub(crate) fn coordination_review_requested(
        &self,
        ns: &StateNamespace,
        request_id: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM transition_facts
             WHERE session_socket_id=?1 AND worktree_id=?2
               AND entity_kind='coordination_review_request'
               AND json_extract(entity_id, '$[0]')=?3)
             OR EXISTS(SELECT 1 FROM collaboration_messages
               WHERE session_socket_id=?1 AND worktree_id=?2
                 AND author_address_id='operator:local' AND request_id=?3)",
            params![ns.session_socket_id, ns.space_id.as_str(), request_id],
            |row| row.get(0),
        )?)
    }

    /// Bind only after the review message committed, before its first drain.
    pub fn bind_coordination_review_message(
        &mut self,
        ns: &StateNamespace,
        request_id: &str,
        message_id: &crate::MessageId,
        operator: &OperatorSession,
        now_ms: u64,
    ) -> Result<(), StoreError> {
        self.append_transition(
            ns,
            TransitionEntityKind::CoordinationReviewMessage,
            &serde_json::json!([request_id, message_id.as_str()]).to_string(),
            TransitionKind::Bound,
            &MutationAttribution::new(ActorKind::Human, operator.actor_id(), now_ms),
        )
    }

    /// Every open coordination warning in the space, oldest first.
    pub fn coordination_warnings(
        &self,
        ns: &StateNamespace,
    ) -> Result<Vec<CoordinationWarning>, StoreError> {
        let mut statement = self.transaction.prepare(&format!(
            "SELECT c.coordination_id, c.outcome, c.deliverable, c.reason, c.report, i.summary,
                    c.owner_address_id, {ALIAS_OF_P}, p.retired
             FROM attention_items i
             JOIN coordinations c ON c.session_socket_id=i.session_socket_id
               AND c.worktree_id=i.worktree_id AND c.coordination_id=i.source_id
             JOIN pane_addresses p ON p.session_socket_id=c.session_socket_id
               AND p.worktree_id=c.worktree_id AND p.address_id=c.owner_address_id
             WHERE i.session_socket_id=?1 AND i.worktree_id=?2 AND i.source_kind='coordination'
               AND i.resolved_at_ms IS NULL
             ORDER BY i.opened_at_ms, i.attention_item_id"
        ))?;
        let rows = statement
            .query_map(params![ns.session_socket_id, ns.space_id.as_str()], |row| {
                let outcome: String = row.get(1)?;
                let lead_retired: bool = row.get(8)?;
                Ok(CoordinationWarning {
                    coordination_id: CoordinationId::new(row.get::<_, String>(0)?),
                    cause: WarningCause::of(&outcome, lead_retired),
                    deliverable: row.get(2)?,
                    reason: row.get(3)?,
                    report: row.get(4)?,
                    summary: row.get(5)?,
                    lead_address_id: PaneAddressId::new(row.get::<_, String>(6)?),
                    lead_alias: row.get(7)?,
                    lead_retired,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }

    /// Opens, or re-opens with a fresh window, the grant for the agent session
    /// currently registered on `grantee`. Returns the grantee's alias and the
    /// expiry.
    /// Refused unless the coordination has an open warning and the grantee is
    /// a live, registered agent.
    pub fn grant_coordination_review(
        &mut self,
        ns: &StateNamespace,
        id: &CoordinationId,
        grantee: &PaneAddressId,
        operator: &OperatorSession,
        request_id: &str,
        now_ms: u64,
    ) -> Result<(String, u64), StoreError> {
        let warned: bool = self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM attention_items WHERE session_socket_id=?1 AND worktree_id=?2
               AND source_kind='coordination' AND source_id=?3 AND resolved_at_ms IS NULL)",
            params![ns.session_socket_id, ns.space_id.as_str(), id.as_str()],
            |row| row.get(0),
        )?;
        if !warned {
            return Err(StoreError::invalid_input(
                "this handoff has no warning to review",
            ));
        }
        let (session, alias) = self
            .transaction
            .query_row(
                &format!(
                    "SELECT r.agent_session_id, {ALIAS_OF_P} FROM integration_registrations r
                 JOIN pane_addresses p ON p.session_socket_id=r.session_socket_id
                   AND p.worktree_id=r.worktree_id AND p.address_id=r.address_id
                 WHERE r.session_socket_id=?1 AND r.worktree_id=?2 AND r.address_id=?3
                   AND r.active=1 AND p.retired=0
                 ORDER BY r.rowid DESC LIMIT 1"
                ),
                params![ns.session_socket_id, ns.space_id.as_str(), grantee.as_str()],
                |row| {
                    Ok((
                        AgentSessionId::new(row.get::<_, String>(0)?),
                        row.get::<_, String>(1)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::invalid_input("the reviewer must be a live, registered agent")
            })?;
        let entity = grant_entity(id, &session);
        let by = MutationAttribution::new(ActorKind::Human, operator.actor_id(), now_ms);
        self.append_transition(
            ns,
            TransitionEntityKind::CoordinationReviewGrant,
            &entity,
            TransitionKind::Opened,
            &by,
        )?;
        let grant_order: i64 = self.transaction.query_row(
            "SELECT MAX(worktree_order) FROM transition_facts
             WHERE session_socket_id=?1 AND worktree_id=?2",
            params![ns.session_socket_id, ns.space_id.as_str()],
            |row| row.get(0),
        )?;
        self.append_transition(
            ns,
            TransitionEntityKind::CoordinationReviewRequest,
            &serde_json::json!([request_id, grant_order]).to_string(),
            TransitionKind::Created,
            &by,
        )?;
        Ok((alias, now_ms + REVIEW_GRANT_TTL_MS))
    }

    /// Consumes the caller's grant and acknowledges the warning in the
    /// caller's transaction, so a refusal anywhere writes neither. `operator`
    /// is the dashboard session current now; `None` means none is.
    pub fn acknowledge_with_review_grant(
        &mut self,
        ns: &StateNamespace,
        id: &CoordinationId,
        caller: &AgentSessionId,
        operator: Option<&OperatorSession>,
        now_ms: u64,
    ) -> Result<bool, StoreError> {
        let entity = grant_entity(id, caller);
        let latest: Option<(String, String, i64)> = self
            .transaction
            .query_row(
                "SELECT transition_kind, actor_id, occurred_at_ms FROM transition_facts
                 WHERE session_socket_id=?1 AND worktree_id=?2 AND entity_kind=?3 AND entity_id=?4
                 ORDER BY worktree_order DESC LIMIT 1",
                params![
                    ns.session_socket_id,
                    ns.space_id.as_str(),
                    TransitionEntityKind::CoordinationReviewGrant.as_str(),
                    entity
                ],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((kind, issuer, granted_at)) = latest else {
            return Err(StoreError::invalid_input(
                "the operator has not asked you to review this handoff; only the operator's Ask to review button grants this",
            ));
        };
        if kind != TransitionKind::Opened.as_str() {
            return Err(StoreError::invalid_input(
                "your review grant for this handoff was already used; ask the operator to press Ask to review again",
            ));
        }
        if now_ms >= from_i64(granted_at)?.saturating_add(REVIEW_GRANT_TTL_MS) {
            return Err(StoreError::invalid_input(
                "your review grant for this handoff expired; ask the operator to press Ask to review again",
            ));
        }
        if operator.map(OperatorSession::actor_id).as_deref() != Some(issuer.as_str()) {
            return Err(StoreError::invalid_input(
                "the operator session that granted this review has ended; ask the operator to press Ask to review again",
            ));
        }
        let by = MutationAttribution::new(ActorKind::AgentSession, caller.as_str(), now_ms);
        self.append_transition(
            ns,
            TransitionEntityKind::CoordinationReviewGrant,
            &entity,
            TransitionKind::Resolved,
            &by,
        )?;
        self.record_coordination_acknowledgement(ns, id, &by, GRANT_NOT_ACKNOWLEDGEABLE)
    }

    /// The coordinations whose grants the operator issued for this attempt's
    /// exact session with this operator request, still unused, still warning.
    pub(crate) fn review_grants_for_attempt(
        &self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
    ) -> Result<Vec<CoordinationId>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT i.source_id
             FROM delivery_attempts a
             JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, delivery_id, address_id)
             JOIN collaboration_messages m USING (session_socket_id, worktree_id, message_id)
             JOIN attention_items i ON i.session_socket_id=a.session_socket_id
               AND i.worktree_id=a.worktree_id AND i.source_kind='coordination' AND i.resolved_at_ms IS NULL
             JOIN transition_facts f ON f.session_socket_id=a.session_socket_id AND f.worktree_id=a.worktree_id
               AND f.entity_kind='coordination_review_grant'
               AND f.entity_id = i.source_id || ':' || a.agent_session_id
               AND f.transition_kind='opened' AND f.actor_kind='human'
             JOIN transition_facts request ON request.session_socket_id=f.session_socket_id
               AND request.worktree_id=f.worktree_id
               AND request.entity_kind='coordination_review_request'
               AND json_extract(request.entity_id, '$[0]')=m.request_id
               AND json_extract(request.entity_id, '$[1]')=f.worktree_order
             JOIN transition_facts bound ON bound.session_socket_id=f.session_socket_id
               AND bound.worktree_id=f.worktree_id
               AND bound.entity_kind='coordination_review_message'
               AND json_extract(bound.entity_id, '$[0]')=m.request_id
               AND json_extract(bound.entity_id, '$[1]')=m.message_id
             WHERE a.session_socket_id=?1 AND a.worktree_id=?2 AND a.attempt_id=?3
               AND m.author_kind='human'
               AND NOT EXISTS(SELECT 1 FROM transition_facts later
                   WHERE later.session_socket_id=f.session_socket_id AND later.worktree_id=f.worktree_id
                     AND later.entity_kind=f.entity_kind AND later.entity_id=f.entity_id
                     AND later.worktree_order > f.worktree_order)
             ORDER BY i.opened_at_ms, i.rowid",
        )?;
        let ids = statement
            .query_map(
                params![ns.session_socket_id, ns.space_id.as_str(), attempt.as_str()],
                |row| row.get::<_, String>(0).map(CoordinationId::new),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(ids)
    }

    /// The newest unused grant per coordination with a warning. Expired ones
    /// are included; the reader compares `expires_at_ms` with its clock.
    pub fn open_review_grants(
        &self,
        ns: &StateNamespace,
    ) -> Result<Vec<OpenReviewGrant>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT i.source_id, r.address_id, f.occurred_at_ms
             FROM attention_items i
             JOIN transition_facts f ON f.session_socket_id=i.session_socket_id
               AND f.worktree_id=i.worktree_id AND f.entity_kind='coordination_review_grant'
               AND f.transition_kind='opened'
             JOIN integration_registrations r ON r.session_socket_id=f.session_socket_id
               AND r.worktree_id=f.worktree_id
               AND f.entity_id = i.source_id || ':' || r.agent_session_id
             WHERE i.session_socket_id=?1 AND i.worktree_id=?2 AND i.source_kind='coordination'
               AND i.resolved_at_ms IS NULL
               AND NOT EXISTS(SELECT 1 FROM transition_facts later
                   WHERE later.session_socket_id=f.session_socket_id AND later.worktree_id=f.worktree_id
                     AND later.entity_kind=f.entity_kind AND later.entity_id=f.entity_id
                     AND later.worktree_order > f.worktree_order)
             ORDER BY f.worktree_order DESC",
        )?;
        let rows = statement
            .query_map(params![ns.session_socket_id, ns.space_id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    from_i64(row.get(2)?)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut seen = std::collections::HashSet::new();
        Ok(rows
            .into_iter()
            .filter(|(id, _, _)| seen.insert(id.clone()))
            .map(
                |(coordination_id, grantee_address_id, granted_at_ms)| OpenReviewGrant {
                    coordination_id,
                    grantee_address_id,
                    granted_at_ms,
                    expires_at_ms: granted_at_ms + REVIEW_GRANT_TTL_MS,
                },
            )
            .collect())
    }

    /// The latest warnings agents cleared with a grant, newest first.
    pub fn granted_clears(&self, ns: &StateNamespace) -> Result<Vec<GrantedClear>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT c.coordination_id, c.deliverable, r.address_id, f.occurred_at_ms
             FROM transition_facts f
             JOIN coordinations c ON c.session_socket_id=f.session_socket_id
               AND c.worktree_id=f.worktree_id
               AND f.entity_id = c.coordination_id || ':' || f.actor_id
             JOIN integration_registrations r ON r.session_socket_id=f.session_socket_id
               AND r.worktree_id=f.worktree_id AND r.agent_session_id=f.actor_id
             WHERE f.session_socket_id=?1 AND f.worktree_id=?2
               AND f.entity_kind='coordination_review_grant' AND f.transition_kind='resolved'
             GROUP BY f.worktree_order
             ORDER BY f.worktree_order DESC LIMIT 20",
        )?;
        let rows = statement
            .query_map(params![ns.session_socket_id, ns.space_id.as_str()], |row| {
                Ok(GrantedClear {
                    coordination_id: row.get(0)?,
                    deliverable: row.get(1)?,
                    address_id: row.get(2)?,
                    at_ms: from_i64(row.get(3)?)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
