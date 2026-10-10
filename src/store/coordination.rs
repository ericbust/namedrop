use super::*;
use crate::{
    AssignmentId, CoordinationId, CoordinationLink, CoordinationOutcome, ReconciledAssignment,
};
use serde_json::{Value, json};

struct CoordinationWakeState {
    outcome: String,
    revision: i64,
    wake_revision: Option<i64>,
    wake_message: Option<String>,
    owner_address: String,
    owner_session: String,
    deliverable: String,
}

pub(super) const SCHEMA: &str = r#"
CREATE TABLE coordinations (
    session_socket_id TEXT NOT NULL, worktree_id TEXT NOT NULL,
    coordination_id TEXT NOT NULL, request_id TEXT NOT NULL,
    owner_address_id TEXT NOT NULL, owner_session_id TEXT NOT NULL,
    deliverable TEXT NOT NULL, revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    outcome TEXT NOT NULL DEFAULT 'open' CHECK (outcome IN ('open','completed','blocked','cancelled')),
    report TEXT, reason TEXT, resolve_request_id TEXT, resolve_input_json TEXT,
    report_message_id TEXT, wake_revision INTEGER, wake_message_id TEXT,
    created_at_ms INTEGER NOT NULL, updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, coordination_id),
    UNIQUE (session_socket_id, worktree_id, owner_address_id, owner_session_id, request_id),
    FOREIGN KEY (session_socket_id, worktree_id, owner_address_id)
        REFERENCES pane_addresses(session_socket_id, worktree_id, address_id)
);
CREATE TABLE coordination_assignments (
    session_socket_id TEXT NOT NULL, worktree_id TEXT NOT NULL,
    assignment_id TEXT NOT NULL, coordination_id TEXT NOT NULL,
    recipient_address_id TEXT NOT NULL, recipient_session_id TEXT NOT NULL,
    original_message_id TEXT NOT NULL, current_message_id TEXT NOT NULL,
    revision INTEGER NOT NULL CHECK (revision > 0),
    work_outcome TEXT NOT NULL CHECK (work_outcome IN ('pending','unclassified','completed','needs_continuation','blocked')),
    blocked_reason TEXT, waiting_on_assignment_id TEXT, last_reply_message_id TEXT,
    PRIMARY KEY (session_socket_id, worktree_id, assignment_id),
    FOREIGN KEY (session_socket_id, worktree_id, coordination_id)
        REFERENCES coordinations(session_socket_id, worktree_id, coordination_id),
    FOREIGN KEY (session_socket_id, worktree_id, recipient_address_id)
        REFERENCES pane_addresses(session_socket_id, worktree_id, address_id),
    FOREIGN KEY (session_socket_id, worktree_id, current_message_id)
        REFERENCES collaboration_messages(session_socket_id, worktree_id, message_id)
);
CREATE INDEX coordination_assignment_parent ON coordination_assignments(session_socket_id, worktree_id, coordination_id);
CREATE TABLE coordination_links (
    session_socket_id TEXT NOT NULL, worktree_id TEXT NOT NULL,
    message_id TEXT NOT NULL, assignment_id TEXT NOT NULL, revision INTEGER NOT NULL,
    kind TEXT NOT NULL CHECK (kind IN ('request','reply')), input_json TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, message_id, assignment_id),
    FOREIGN KEY (session_socket_id, worktree_id, message_id)
        REFERENCES collaboration_messages(session_socket_id, worktree_id, message_id),
    FOREIGN KEY (session_socket_id, worktree_id, assignment_id)
        REFERENCES coordination_assignments(session_socket_id, worktree_id, assignment_id)
);
"#;

impl StateTransaction<'_> {
    pub(crate) fn coordination_delivery_allowed(
        &self,
        ns: &StateNamespace,
        message: &MessageId,
        recipient: &PaneAddressId,
        session: &crate::AgentSessionId,
    ) -> Result<bool, StoreError> {
        let tracked=self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM coordination_links WHERE session_socket_id=?1 AND worktree_id=?2 AND (message_id=?3 OR message_id=(SELECT in_reply_to FROM collaboration_messages WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3 AND grammar_version='result-set/v1'))) OR EXISTS(SELECT 1 FROM coordinations WHERE session_socket_id=?1 AND worktree_id=?2 AND report_message_id=?3) OR EXISTS(SELECT 1 FROM collaboration_messages WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3 AND grammar_version='coordination-nudge/v1')",params![ns.session_socket_id,ns.space_id.as_str(),message.as_str()],|r|r.get::<_,bool>(0))?;
        if !tracked {
            return Ok(true);
        }
        if !self.coordination_wake_current(ns, message)? {
            return Ok(false);
        }
        let pinned=self.transaction.query_row("SELECT session_snapshot FROM message_recipients WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3 AND address_id=?4",params![ns.session_socket_id,ns.space_id.as_str(),message.as_str(),recipient.as_str()],|r|r.get::<_,Option<String>>(0))?;
        if pinned.as_deref() != Some(session.as_str()) {
            return Ok(false);
        }
        let Some(recipient) = self.optional_routing_address_by_id(ns, recipient)? else {
            return Ok(false);
        };
        if recipient.agent_session_id.as_ref() != Some(session) {
            return Ok(false);
        }
        Ok(self.require_coordination_capability(ns, &recipient).is_ok())
    }

    pub(crate) fn coordination_owner(
        &self,
        ns: &StateNamespace,
        id: &CoordinationId,
    ) -> Result<RoutingAddressSnapshot, StoreError> {
        let row = self.coordination_owner_row(ns, id)?.ok_or_else(|| {
            StoreError::invalid_input("coordination does not exist in this workspace")
        })?;
        let mut owner = self.routing_address_by_id(ns, &row.address)?;
        owner.agent_session_id = Some(row.session);
        Ok(owner)
    }

    pub(crate) fn require_assignment_attempt(
        &self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
        message: &MessageId,
        author: &RoutingAddressSnapshot,
    ) -> Result<(), StoreError> {
        let matches=self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM delivery_attempts a JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,delivery_id,address_id) WHERE a.session_socket_id=?1 AND a.worktree_id=?2 AND a.attempt_id=?3 AND d.message_id=?4 AND a.address_id=?5 AND a.agent_session_id=?6 AND NOT EXISTS(SELECT 1 FROM delivery_dispositions p WHERE p.session_socket_id=a.session_socket_id AND p.worktree_id=a.worktree_id AND p.attempt_id=a.attempt_id))",params![ns.session_socket_id,ns.space_id.as_str(),attempt.as_str(),message.as_str(),author.address_id.as_str(),author.agent_session_id.as_ref().map(|s|s.as_str())],|r|r.get::<_,bool>(0))?;
        if !matches {
            return Err(StoreError::invalid_input(
                "work outcome attempt must match its exact assignment message, address, and session",
            ));
        }
        Ok(())
    }

    pub(crate) fn require_coordination_capability(
        &self,
        ns: &StateNamespace,
        address: &RoutingAddressSnapshot,
    ) -> Result<(), StoreError> {
        let registration = address
            .agent_session_id
            .as_ref()
            .map(|session| self.active_integration_registration(ns, &address.address_id, session))
            .transpose()?
            .flatten();
        if !registration.is_some_and(|r| {
            ["handoff.begin", "handoff.resolve"]
                .iter()
                .all(|token| r.capabilities.iter().any(|c| c == token))
        }) {
            return Err(StoreError::invalid_input(
                "tracked coordination requires a fresh exact-session registration with handoff.begin and handoff.resolve capabilities",
            ));
        }
        Ok(())
    }

    pub(crate) fn begin_coordination(
        &mut self,
        ns: &StateNamespace,
        owner: &RoutingAddressSnapshot,
        request_id: &str,
        deliverable: &str,
        id: &CoordinationId,
        now: u64,
    ) -> Result<CoordinationId, StoreError> {
        self.require_coordination_capability(ns, owner)?;
        let session = owner
            .agent_session_id
            .as_ref()
            .expect("capability requires a session");
        let old = self.transaction.query_row(
            "SELECT coordination_id,deliverable FROM coordinations WHERE session_socket_id=?1 AND worktree_id=?2
             AND owner_address_id=?3 AND owner_session_id=?4 AND request_id=?5",
            params![ns.session_socket_id,ns.space_id.as_str(),owner.address_id.as_str(),session.as_str(),request_id],
            |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional()?;
        if let Some((id, old_deliverable)) = old {
            let id = CoordinationId::new(id);
            if self
                .previous_owner_adopted_from(ns, &id, session)?
                .is_some()
            {
                return Err(StoreError::invalid_input(
                    "request id was already used to adopt a coordination",
                ));
            }
            if old_deliverable != deliverable {
                return Err(StoreError::invalid_input(
                    "request id was already used for another deliverable",
                ));
            }
            return Ok(id);
        }
        self.transaction.execute(
            "INSERT INTO coordinations(session_socket_id,worktree_id,coordination_id,request_id,owner_address_id,owner_session_id,deliverable,created_at_ms,updated_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?8)",
            params![ns.session_socket_id,ns.space_id.as_str(),id.as_str(),request_id,owner.address_id.as_str(),session.as_str(),deliverable,to_i64(now)?])?;
        Ok(id.clone())
    }

    pub(crate) fn coordination_links(
        &self,
        ns: &StateNamespace,
        message: &MessageId,
    ) -> Result<Vec<CoordinationLink>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT a.coordination_id,l.assignment_id,l.revision FROM coordination_links l JOIN coordination_assignments a
             USING(session_socket_id,worktree_id,assignment_id) WHERE l.session_socket_id=?1 AND l.worktree_id=?2 AND l.message_id=?3 ORDER BY l.rowid")?;
        statement
            .query_map(
                params![ns.session_socket_id, ns.space_id.as_str(), message.as_str()],
                |r| {
                    Ok(CoordinationLink {
                        coordination_id: CoordinationId::new(r.get::<_, String>(0)?),
                        assignment_id: AssignmentId::new(r.get::<_, String>(1)?),
                        revision: from_i64(r.get(2)?)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn coordination_assignment(
        &self,
        ns: &StateNamespace,
        id: &AssignmentId,
    ) -> Result<Value, StoreError> {
        let mut assignment = self.transaction.query_row(
            "SELECT coordination_id,recipient_address_id,recipient_session_id,original_message_id,current_message_id,revision,
             work_outcome,blocked_reason,waiting_on_assignment_id,last_reply_message_id FROM coordination_assignments
             WHERE session_socket_id=?1 AND worktree_id=?2 AND assignment_id=?3",
            params![ns.session_socket_id,ns.space_id.as_str(),id.as_str()], |r|Ok(json!({
                "assignment_id":id,"coordination_id":r.get::<_,String>(0)?,"recipient_address_id":r.get::<_,String>(1)?,
                "recipient_session_id":r.get::<_,String>(2)?,"original_message_id":r.get::<_,String>(3)?,"current_message_id":r.get::<_,String>(4)?,
                "revision":from_i64(r.get(5)?)?,"work_outcome":r.get::<_,String>(6)?,"blocked_reason":r.get::<_,Option<String>>(7)?,
                "waiting_on_assignment_id":r.get::<_,Option<String>>(8)?,"last_reply_message_id":r.get::<_,Option<String>>(9)?,
            }))).optional()?.ok_or_else(||StoreError::invalid_input("assignment does not exist in this workspace"))?;
        assignment["delivery"] = self.transaction.query_row("SELECT d.delivery_id,d.state,(SELECT attempt_id FROM delivery_attempts a WHERE a.session_socket_id=d.session_socket_id AND a.worktree_id=d.worktree_id AND a.delivery_id=d.delivery_id ORDER BY a.rowid DESC LIMIT 1) FROM mailbox_deliveries d WHERE d.session_socket_id=?1 AND d.worktree_id=?2 AND d.message_id=?3 AND d.address_id=?4", params![ns.session_socket_id,ns.space_id.as_str(),assignment["current_message_id"].as_str(),assignment["recipient_address_id"].as_str()], |r|Ok(json!({"delivery_id":r.get::<_,String>(0)?,"state":r.get::<_,String>(1)?,"attempt_id":r.get::<_,Option<String>>(2)?})))?;
        Ok(assignment)
    }

    pub(crate) fn coordination_report(
        &self,
        ns: &StateNamespace,
        id: &CoordinationId,
        owner: &RoutingAddressSnapshot,
    ) -> Result<Value, StoreError> {
        self.require_coordination_owner(ns, id, owner, false)?;
        let mut report = self.transaction.query_row(
            "SELECT owner_address_id,owner_session_id,deliverable,revision,outcome,report,reason,report_message_id,wake_revision,wake_message_id
             FROM coordinations WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3",
            params![ns.session_socket_id,ns.space_id.as_str(),id.as_str()],|r|Ok(json!({
                "coordination_id":id,"owner_address_id":r.get::<_,String>(0)?,"owner_session_id":r.get::<_,String>(1)?,
                "deliverable":r.get::<_,String>(2)?,"revision":from_i64(r.get(3)?)?,"outcome":r.get::<_,String>(4)?,
                "report":r.get::<_,Option<String>>(5)?,"reason":r.get::<_,Option<String>>(6)?,"report_message_id":r.get::<_,Option<String>>(7)?,
                "wake_revision":r.get::<_,Option<i64>>(8)?,"wake_message_id":r.get::<_,Option<String>>(9)?,
                "presentation":"unknown",
            })))?;
        let mut statement = self.transaction.prepare("SELECT assignment_id FROM coordination_assignments WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3 ORDER BY rowid")?;
        let ids = statement
            .query_map(
                params![ns.session_socket_id, ns.space_id.as_str(), id.as_str()],
                |r| r.get::<_, String>(0).map(AssignmentId::new),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        report["assignments"] = json!(
            ids.iter()
                .map(|id| self.coordination_assignment(ns, id))
                .collect::<Result<Vec<_>, _>>()?
        );
        report["report_saved"] = json!(!report["report"].is_null());
        report["report_delivery_state"] = match report["report_message_id"].as_str() {
            None => Value::Null,
            Some(message) => self.transaction.query_row("SELECT state FROM mailbox_deliveries WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),message],|r|r.get::<_,String>(0)).optional()?.map_or(Value::Null,Value::String),
        };
        report["report_delivery_settled"] = json!(report["report_delivery_state"] == "settled");
        Ok(report)
    }

    fn coordination_attention(
        &mut self,
        ns: &StateNamespace,
        id: &CoordinationId,
        summary: &str,
        by: &MutationAttribution,
    ) -> Result<(), StoreError> {
        self.open_attention_item(
            ns,
            &crate::AttentionItemDraft {
                id: crate::AttentionItemId::new(format!("coordination-attention:{}", id.as_str())),
                source_kind: crate::AttentionSourceKind::Coordination,
                source_id: id.as_str().to_owned(),
                summary: summary.to_owned(),
            },
            by,
        )?;
        Ok(())
    }

    // Share the safety condition between acknowledgement and reconciliation:
    // a historical outcome cannot conceal transport that still needs action.
    fn coordination_attention_can_be_acknowledged(
        &self,
        ns: &StateNamespace,
        id: &CoordinationId,
    ) -> Result<bool, StoreError> {
        Ok(self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM coordinations c
             WHERE c.session_socket_id=?1 AND c.worktree_id=?2 AND c.coordination_id=?3
               AND ((c.outcome IN ('blocked','cancelled') AND c.report IS NOT NULL
                   AND EXISTS(SELECT 1 FROM mailbox_deliveries d
                       WHERE d.session_socket_id=c.session_socket_id AND d.worktree_id=c.worktree_id
                         AND d.message_id=c.report_message_id AND d.state='settled'))
                 OR (c.outcome='completed' AND c.report IS NOT NULL
                   AND EXISTS(SELECT 1 FROM mailbox_deliveries d
                       WHERE d.session_socket_id=c.session_socket_id AND d.worktree_id=c.worktree_id
                         AND d.message_id=c.report_message_id AND d.state='cancelled'))
                 OR (c.outcome='open' AND EXISTS(SELECT 1 FROM pane_addresses p
                       WHERE p.session_socket_id=c.session_socket_id AND p.worktree_id=c.worktree_id
                         AND p.address_id=c.owner_address_id AND p.retired=1)))
               AND NOT EXISTS(SELECT 1 FROM mailbox_deliveries d
                   WHERE d.session_socket_id=c.session_socket_id AND d.worktree_id=c.worktree_id
                     AND d.state NOT IN ('settled','failed','cancelled')
                     AND (d.message_id=c.report_message_id OR d.message_id=c.wake_message_id
                       OR d.message_id IN (SELECT l.message_id FROM coordination_links l
                         JOIN coordination_assignments a USING(session_socket_id,worktree_id,assignment_id)
                         WHERE a.session_socket_id=c.session_socket_id AND a.worktree_id=c.worktree_id
                           AND a.coordination_id=c.coordination_id))))",
            params![ns.session_socket_id, ns.space_id.as_str(), id.as_str()],
            |row| row.get(0),
        )?)
    }

    fn coordination_attention_acknowledged(
        &self,
        ns: &StateNamespace,
        id: &CoordinationId,
    ) -> Result<bool, StoreError> {
        Ok(self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM attention_items a JOIN transition_facts f
               ON f.session_socket_id=a.session_socket_id AND f.worktree_id=a.worktree_id
                 AND f.entity_id=a.attention_item_id
             WHERE a.session_socket_id=?1 AND a.worktree_id=?2
               AND a.source_kind='coordination' AND a.source_id=?3
               AND f.entity_kind='attention_item' AND f.transition_kind='acknowledged'
               AND f.actor_kind='human')",
            params![ns.session_socket_id, ns.space_id.as_str(), id.as_str()],
            |row| row.get(0),
        )?)
    }

    pub fn acknowledge_coordination_attention(
        &mut self,
        ns: &StateNamespace,
        id: &CoordinationId,
        by: &MutationAttribution,
    ) -> Result<bool, StoreError> {
        let (outcome, owner_retired, report_delivery): (String, bool, Option<String>) = self.transaction.query_row(
            "SELECT c.outcome, p.retired,
                    (SELECT d.state FROM mailbox_deliveries d
                     WHERE d.session_socket_id=c.session_socket_id AND d.worktree_id=c.worktree_id
                       AND d.message_id=c.report_message_id)
             FROM coordinations c JOIN pane_addresses p
               ON p.session_socket_id=c.session_socket_id AND p.worktree_id=c.worktree_id AND p.address_id=c.owner_address_id
             WHERE c.session_socket_id=?1 AND c.worktree_id=?2 AND c.coordination_id=?3",
            params![ns.session_socket_id, ns.space_id.as_str(), id.as_str()], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional()?.ok_or_else(|| StoreError::invalid_input("coordination does not exist in this workspace"))?;
        let acknowledgeable = match outcome.as_str() {
            "blocked" | "cancelled" => true,
            "open" => owner_retired,
            "completed" => report_delivery.as_deref() == Some("cancelled"),
            _ => false,
        };
        if by.actor_kind != ActorKind::Human || !acknowledgeable {
            return Err(StoreError::invalid_input(
                "only a human may acknowledge a blocked or cancelled coordination, an open one whose lead address is retired, or a completed one whose report delivery was cancelled",
            ));
        }
        if !self.coordination_attention_can_be_acknowledged(ns, id)? {
            return Err(StoreError::invalid_input(
                "handoff has unfinished deliveries, or lacks a saved, settled report; review it before acknowledging",
            ));
        }
        let acknowledged = self.coordination_attention_acknowledged(ns, id)?;
        if !acknowledged {
            let attention_id: String = self.transaction.query_row(
                "SELECT attention_item_id FROM attention_items WHERE session_socket_id=?1 AND worktree_id=?2 AND source_kind='coordination' AND source_id=?3 ORDER BY opened_at_ms DESC LIMIT 1",
                params![ns.session_socket_id, ns.space_id.as_str(), id.as_str()], |r| r.get(0),
            ).optional()?.ok_or_else(|| StoreError::invalid_input("coordination has no warning to acknowledge"))?;
            self.append_transition(
                ns,
                TransitionEntityKind::AttentionItem,
                &attention_id,
                TransitionKind::Acknowledged,
                by,
            )?;
        }
        let resolved = self.resolve_attention_source(
            ns,
            crate::AttentionSourceKind::Coordination,
            id.as_str(),
            by,
        )?;
        Ok(!acknowledged || resolved)
    }

    /// A finished coordination is worth a human's attention only while
    /// something is still actionable: an outcome that stopped short of
    /// completion, a report that was never saved, or a report delivery that has
    /// not settled. Once a completed coordination's report is saved and its
    /// delivery has settled there is nothing left to ask anyone for, so a stale
    /// item is resolved rather than raised again by every reconciliation.
    /// Blocked/cancelled outcomes may instead be explicitly acknowledged by a
    /// human, but only with a settled report and no unfinished linked deliveries.
    ///
    /// Resolving it claims nothing about a human having read the report.
    /// Presentation stays unknown; settled transport is not consumption. This
    /// only stops Namedrop from repeating a warning that carries no action.
    fn reconcile_resolved_coordination(
        &mut self,
        ns: &StateNamespace,
        id: &CoordinationId,
        outcome: &str,
        by: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let (report,reason,report_message)=self.transaction.query_row("SELECT report,reason,report_message_id FROM coordinations WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),id.as_str()],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<String>>(2)?)))?;
        if self.coordination_attention_acknowledged(ns, id)?
            && self.coordination_attention_can_be_acknowledged(ns, id)?
        {
            self.resolve_attention_source(
                ns,
                crate::AttentionSourceKind::Coordination,
                id.as_str(),
                by,
            )?;
            return Ok(());
        }
        if outcome != "completed" {
            // The reason the lead recorded is the actionable part, so it is
            // carried through rather than summarized away.
            let reason = reason
                .as_deref()
                .map(|reason| reason.trim().trim_end_matches('.').trim_end())
                .filter(|reason| !reason.is_empty())
                .unwrap_or("none was recorded");
            return self.coordination_attention(ns,id,&format!("This coordination was resolved {outcome} rather than completed. Recorded reason: {reason}. Review its saved report and decide what happens next; Namedrop will not continue it on its own."),by);
        }
        // A missing report and a missing report message are different facts, and
        // neither may be reported as the other.
        if report.is_none() {
            return self.coordination_attention(ns,id,"This coordination completed without a saved report. Review the assignment replies directly; there is no report to read.",by);
        }
        let Some(message) = report_message.as_deref() else {
            return self.coordination_attention(ns,id,"The coordination report is saved but no report message was recorded for it. Read it through handoff.results; there is no delivery to review.",by);
        };
        let state=self.transaction.query_row("SELECT state FROM mailbox_deliveries WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),message],|r|r.get::<_,String>(0)).optional()?;
        match state.as_deref() {
            Some("settled") => {
                self.resolve_attention_source(
                    ns,
                    crate::AttentionSourceKind::Coordination,
                    id.as_str(),
                    by,
                )?;
                Ok(())
            }
            // Named states only where the delivery has stopped making progress
            // by itself. An in-flight delivery is progress, not a problem, so
            // its summary stays fixed rather than being rewritten at every
            // reconciliation. None of these say whether the report reached its
            // lead: an acknowledged delivery records receipt, and a held one
            // may have been invoked ambiguously, so only settlement and the
            // state itself are claimed.
            Some(state @ ("held" | "failed" | "cancelled")) => self.coordination_attention(ns,id,&format!("The saved coordination report's delivery is {state} and will not progress on its own. Review that delivery; the report itself is saved."),by),
            Some(_) => self.coordination_attention(ns,id,"The coordination report is saved but its delivery has not settled. Review that delivery if it stops making progress; the report itself is saved.",by),
            // A missing row proves there is no record, not that nothing was
            // ever queued: a delivery can be disposed of or pruned.
            None => self.coordination_attention(ns,id,"The saved coordination report has no delivery record. Read it through handoff.results; its delivery state cannot be established here.",by),
        }
    }

    /// Run after result-set reconciliation. The immediate SQLite transaction
    /// contains both the semantic wake claim and its mailbox row.
    pub(crate) fn reconcile_coordinations(
        &mut self,
        ns: &StateNamespace,
        discovered: &crate::DiscoveredWorktree,
        by: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let mut statement = self.transaction.prepare("SELECT coordination_id FROM coordinations WHERE session_socket_id=?1 AND worktree_id=?2 ORDER BY rowid")?;
        let ids = statement
            .query_map(params![ns.session_socket_id, ns.space_id.as_str()], |r| {
                r.get::<_, String>(0).map(CoordinationId::new)
            })?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for id in ids {
            let CoordinationWakeState {outcome,revision,wake_revision,wake_message,owner_address,owner_session,deliverable} = self.transaction.query_row(
                "SELECT outcome,revision,wake_revision,wake_message_id,owner_address_id,owner_session_id,deliverable FROM coordinations WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3",
                params![ns.session_socket_id,ns.space_id.as_str(),id.as_str()], |r|Ok(CoordinationWakeState {outcome:r.get(0)?,revision:r.get(1)?,wake_revision:r.get(2)?,wake_message:r.get(3)?,owner_address:r.get(4)?,owner_session:r.get(5)?,deliverable:r.get(6)?}))?;
            if let Some(message) = &wake_message
                && (outcome != "open" || wake_revision != Some(revision))
            {
                // A queued item is unsent, including one returned after a
                // proven pre-invocation failure. Invoked/ambiguous work is
                // never silently cancelled or retried here.
                let delivery=self.transaction.query_row("SELECT delivery_id FROM mailbox_deliveries WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3 AND state='queued'",params![ns.session_socket_id,ns.space_id.as_str(),message],|r|r.get::<_,String>(0)).optional()?;
                if let Some(delivery) = delivery {
                    self.transaction.execute("UPDATE mailbox_deliveries SET state='cancelled' WHERE session_socket_id=?1 AND worktree_id=?2 AND delivery_id=?3 AND state='queued'",params![ns.session_socket_id,ns.space_id.as_str(),delivery])?;
                    self.append_transition(
                        ns,
                        TransitionEntityKind::MailboxDelivery,
                        &delivery,
                        TransitionKind::Cancelled,
                        by,
                    )?;
                }
            }
            if outcome != "open" {
                self.reconcile_resolved_coordination(ns, &id, &outcome, by)?;
                continue;
            }
            // For an open coordination the predicate holds only once its lead
            // address is retired, which can never bind again to resolve it; a
            // live lead's warning stays actionable.
            if self.coordination_attention_acknowledged(ns, &id)?
                && self.coordination_attention_can_be_acknowledged(ns, &id)?
            {
                self.resolve_attention_source(
                    ns,
                    crate::AttentionSourceKind::Coordination,
                    id.as_str(),
                    by,
                )?;
                continue;
            }
            if wake_revision == Some(revision) {
                let state = wake_message.as_ref().map(|message| self.transaction.query_row("SELECT state FROM mailbox_deliveries WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),message],|r|r.get::<_,String>(0))).transpose()?;
                if state
                    .as_deref()
                    .is_some_and(|s| matches!(s, "settled" | "held" | "failed" | "cancelled"))
                {
                    self.coordination_attention(ns, &id, "The one continuation for this revision ended or is held without a new work decision. Review handoff.results and act explicitly; Namedrop will not repeat the prompt.", by)?;
                } else if state.as_deref() == Some("queued") {
                    let message = wake_message.as_ref().expect("queued wake has message");
                    let owner_binding = self
                        .active_pane_bindings(ns)?
                        .into_iter()
                        .find(|b| b.address_id.as_str() == owner_address);
                    let owner_live = owner_binding.as_ref().is_some_and(|b| {
                        discovered.panes.iter().any(|p| {
                            p.pane_id == b.pane_id
                                && p.agent_session_id.as_ref().map(|s| s.as_str())
                                    == Some(owner_session.as_str())
                        })
                    });
                    if !owner_live {
                        self.coordination_attention(ns,&id,"The queued continuation lost its exact lead session. The current session on this address can take over this coordination with handoff adopt.",by)?;
                        continue;
                    }
                    if self.coordination_paused_assignment(ns, &id)? {
                        self.coordination_attention(ns,&id,"Tracked assignment work is paused or stopped. Resume that thread explicitly before the queued continuation can run.",by)?;
                        continue;
                    }
                    let held_ahead=self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM mailbox_deliveries d JOIN mailbox_deliveries earlier ON earlier.session_socket_id=d.session_socket_id AND earlier.worktree_id=d.worktree_id AND earlier.address_id=d.address_id AND earlier.mailbox_order<d.mailbox_order AND earlier.state='held' WHERE d.session_socket_id=?1 AND d.worktree_id=?2 AND d.message_id=?3)",params![ns.session_socket_id,ns.space_id.as_str(),message],|r|r.get::<_,bool>(0))?;
                    if held_ahead {
                        self.coordination_attention(ns,&id,"The continuation is queued behind a held delivery. Review the earlier exact attempt; automatic prompting is stopped by mailbox order.",by)?;
                    }
                }
                continue;
            }
            let (retired,disabled):(bool,bool)=self.transaction.query_row("SELECT retired,collaboration_disabled FROM pane_addresses WHERE session_socket_id=?1 AND worktree_id=?2 AND address_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),owner_address],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if retired || disabled {
                self.coordination_attention(ns,&id,"The original lead session is missing, retired, disabled, or lacks its current registration. Recover the saved coordination without transferring mutation rights.",by)?;
                continue;
            }
            let Some(owner) =
                self.optional_routing_address_by_id(ns, &crate::PaneAddressId::new(owner_address))?
            else {
                self.coordination_attention(ns,&id,"The original lead address is no longer routable. Recover the saved coordination without transferring mutation rights.",by)?;
                continue;
            };
            if let Some(current) = &owner.agent_session_id
                && current.as_str() != owner_session
            {
                self.coordination_attention(ns,&id,"The original lead session was replaced. The current session on this address can take over this coordination with handoff adopt; handoff status lists it under adoptable.",by)?;
                continue;
            }
            if self.require_coordination_capability(ns, &owner).is_err() {
                self.coordination_attention(ns,&id,"The original lead session is missing, retired, disabled, or lacks its current registration. Recover the saved coordination without transferring mutation rights.",by)?;
                continue;
            }
            let binding = self
                .active_pane_bindings(ns)?
                .into_iter()
                .find(|b| b.address_id == owner.address_id);
            if !binding.as_ref().is_some_and(|b| {
                discovered.panes.iter().any(|p| {
                    p.pane_id == b.pane_id
                        && p.agent_session_id.as_ref().map(|s| s.as_str())
                            == Some(owner_session.as_str())
                })
            }) {
                self.coordination_attention(ns,&id,"The original lead Agent Session is absent from its live pane. Review the saved coordination; a replacement session cannot mutate it.",by)?;
                continue;
            }
            let live_status = binding
                .as_ref()
                .and_then(|b| discovered.panes.iter().find(|p| p.pane_id == b.pane_id))
                .and_then(|p| p.agent_status.as_deref());
            if matches!(live_status, None | Some("unknown" | "blocked")) {
                self.coordination_attention(ns,&id,"The original lead is blocked or its live status is unknown. Wait for a settled idle observation or review the saved coordination.",by)?;
                continue;
            }
            let owner_mail = self.transaction.query_row("SELECT d.state,c.paused,c.stopped FROM mailbox_deliveries d JOIN collaboration_messages m USING(session_socket_id,worktree_id,message_id) JOIN thread_controls c USING(session_socket_id,worktree_id,thread_id) WHERE d.session_socket_id=?1 AND d.worktree_id=?2 AND d.address_id=?3 AND d.state NOT IN ('settled','failed','cancelled') ORDER BY d.mailbox_order LIMIT 1",params![ns.session_socket_id,ns.space_id.as_str(),owner.address_id.as_str()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,bool>(1)?,r.get::<_,bool>(2)?))).optional()?;
            if owner_mail
                .as_ref()
                .is_some_and(|(_, paused, stopped)| *paused || *stopped)
            {
                self.coordination_attention(ns,&id,"The lead's queued reply is in a paused or stopped thread. Resume or review that thread; automatic continuation will not cross the constraint.",by)?;
                continue;
            }
            if owner_mail
                .as_ref()
                .is_some_and(|(state, _, _)| state == "held")
            {
                self.coordination_attention(ns,&id,"The lead mailbox has a held or ambiguous delivery. Review its exact attempt before continuing; no worker work will be resent.",by)?;
                continue;
            }
            if owner_mail.is_some() {
                continue;
            }
            if self.coordination_paused_assignment(ns, &id)? {
                self.coordination_attention(ns,&id,"Tracked assignment work is paused or stopped. Resume that thread explicitly before continuing.",by)?;
                continue;
            }
            let has_assignments=self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM coordination_assignments WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3)",params![ns.session_socket_id,ns.space_id.as_str(),id.as_str()],|r|r.get::<_,bool>(0))?;
            if !has_assignments && matches!(live_status, Some("idle" | "done")) {
                self.coordination_attention(ns,&id,"The lead is idle with an open coordination and no assignment or saved report. Review the deliverable, assign work, or resolve an explicit blocker.",by)?;
                continue;
            }
            self.resolve_attention_source(
                ns,
                crate::AttentionSourceKind::Coordination,
                id.as_str(),
                by,
            )?;
            if !self.coordination_wake_eligible(ns, &id, discovered)? {
                continue;
            }
            let message =
                crate::MessageId::new(format!("coordination-wake:{}:{revision}", id.as_str()));
            let thread = crate::ThreadId::new(format!(
                "coordination-wake-thread:{}:{revision}",
                id.as_str()
            ));
            self.create_thread(ns, &thread, "Tracked handoff continuation", by)?;
            let mut body = format!(
                "Coordination {} (revision {revision}) remains open. Deliverable: {deliverable}. Review the current assignments and take an explicit next action or save a final report.",
                id.as_str()
            );
            let mut assignments = self.transaction.prepare("SELECT assignment_id,work_outcome,revision FROM coordination_assignments WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3 ORDER BY rowid")?;
            let rows = assignments
                .query_map(
                    params![ns.session_socket_id, ns.space_id.as_str(), id.as_str()],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, i64>(2)?,
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            drop(assignments);
            for (assignment, outcome, version) in rows {
                body.push_str(&format!(
                    "\nAssignment {assignment} revision {version}: {outcome}."
                ));
            }
            self.append_message(
                ns,
                &crate::MessageDraft {
                    thread_id: thread,
                    message_id: message.clone(),
                    in_reply_to: None,
                    author_address_id: owner.address_id.clone(),
                    author_kind: crate::ActorKind::AgentSession,
                    author_alias_snapshot: "namedrop".to_owned(),
                    author_agent_session_snapshot: owner.agent_session_id.clone(),
                    request_id: format!("coordination-wake:{}:{revision}", id.as_str()),
                    authored_source: body.clone(),
                    body,
                    grammar_version: "coordination-nudge/v1".to_owned(),
                    recipients: vec![crate::MessageRecipientDraft {
                        address_id: owner.address_id.clone(),
                        alias_snapshot: owner.alias.clone(),
                        agent_session_snapshot: owner.agent_session_id.clone(),
                        delivery_id: crate::DeliveryId::new(format!(
                            "coordination-wake-delivery:{}:{revision}",
                            id.as_str()
                        )),
                    }],
                    context_references: vec![],
                },
                by,
            )?;
            self.transaction.execute("UPDATE coordinations SET wake_revision=?4,wake_message_id=?5 WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3 AND outcome='open' AND revision=?4",params![ns.session_socket_id,ns.space_id.as_str(),id.as_str(),revision,message.as_str()])?;
            self.resolve_attention_source(
                ns,
                crate::AttentionSourceKind::Coordination,
                id.as_str(),
                by,
            )?;
        }
        Ok(())
    }

    pub(crate) fn coordination_wake_eligible(
        &self,
        ns: &StateNamespace,
        id: &CoordinationId,
        discovered: &crate::DiscoveredWorktree,
    ) -> Result<bool, StoreError> {
        let (address,session,outcome):(String,String,String)=self.transaction.query_row("SELECT owner_address_id,owner_session_id,outcome FROM coordinations WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),id.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
        if outcome != "open"
            || !self
                .worktree_state(ns)?
                .is_some_and(|w| w.activated && !w.paused && !w.deactivated)
        {
            return Ok(false);
        }
        if self.coordination_paused_assignment(ns, id)? {
            return Ok(false);
        }
        let Some(owner) =
            self.optional_routing_address_by_id(ns, &crate::PaneAddressId::new(address))?
        else {
            return Ok(false);
        };
        let disabled: bool = self.transaction.query_row("SELECT collaboration_disabled FROM pane_addresses WHERE session_socket_id=?1 AND worktree_id=?2 AND address_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),owner.address_id.as_str()],|r|r.get(0))?;
        if disabled
            || owner.agent_session_id.as_ref().map(|s| s.as_str()) != Some(session.as_str())
            || self.require_coordination_capability(ns, &owner).is_err()
        {
            return Ok(false);
        }
        let binding = self
            .active_pane_bindings(ns)?
            .into_iter()
            .find(|b| b.address_id == owner.address_id);
        let Some(binding) = binding else {
            return Ok(false);
        };
        if !discovered.panes.iter().any(|p| {
            p.pane_id == binding.pane_id
                && p.agent_session_id.as_ref().map(|s| s.as_str()) == Some(session.as_str())
                && matches!(p.agent_status.as_deref(), Some("idle" | "done"))
        }) {
            return Ok(false);
        }
        let mut statement=self.transaction.prepare("SELECT assignment_id,recipient_address_id,recipient_session_id,work_outcome,waiting_on_assignment_id,current_message_id FROM coordination_assignments WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3")?;
        let assignments = statement
            .query_map(
                params![ns.session_socket_id, ns.space_id.as_str(), id.as_str()],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<String>>(4)?,
                        r.get::<_, String>(5)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        // Beginning a coordination records an obligation; the lead can still
        // be preparing its first assignment. A wake needs actual delegated work.
        if assignments.is_empty() {
            return Ok(false);
        }
        for (_, address, session, outcome, _, message) in &assignments {
            let binding = self
                .active_pane_bindings(ns)?
                .into_iter()
                .find(|b| b.address_id.as_str() == address);
            let live = binding
                .as_ref()
                .and_then(|b| discovered.panes.iter().find(|p| p.pane_id == b.pane_id));
            let worker_working = live.is_some_and(|p| {
                p.agent_session_id.as_ref().map(|s| s.as_str()) == Some(session.as_str())
                    && p.agent_status.as_deref() == Some("working")
            });
            if matches!(
                outcome.as_str(),
                "needs_continuation" | "blocked" | "unclassified"
            ) && worker_working
            {
                return Ok(false);
            }
            if outcome != "pending" {
                continue;
            }
            let delivery=self.transaction.query_row("SELECT state FROM mailbox_deliveries WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3 AND address_id=?4",params![ns.session_socket_id,ns.space_id.as_str(),message,address],|r|r.get::<_,String>(0))?;
            if matches!(
                delivery.as_str(),
                "dispatching" | "submitted" | "acknowledged"
            ) {
                return Ok(false);
            }
            if delivery == "queued"
                && live.is_some_and(|p| {
                    p.agent_session_id.as_ref().map(|s| s.as_str()) == Some(session.as_str())
                        && matches!(p.agent_status.as_deref(), Some("working" | "idle" | "done"))
                })
            {
                let (retired,disabled):(bool,bool)=self.transaction.query_row("SELECT retired,collaboration_disabled FROM pane_addresses WHERE session_socket_id=?1 AND worktree_id=?2 AND address_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),address],|r|Ok((r.get(0)?,r.get(1)?)))?;
                let head=self.transaction.query_row("SELECT state FROM mailbox_deliveries WHERE session_socket_id=?1 AND worktree_id=?2 AND address_id=?3 AND state NOT IN ('settled','failed','cancelled') ORDER BY mailbox_order LIMIT 1",params![ns.session_socket_id,ns.space_id.as_str(),address],|r|r.get::<_,String>(0)).optional()?;
                if !retired
                    && !disabled
                    && head.as_deref().is_some_and(|s| {
                        matches!(s, "queued" | "dispatching" | "submitted" | "acknowledged")
                    })
                    && self
                        .dispatch_candidate(ns, &crate::PaneAddressId::new(address.clone()))?
                        .is_some()
                {
                    return Ok(false);
                }
                // An earlier active FIFO item is progress too: this assignment
                // becomes dispatchable after its ordinary disposition.
                if !retired
                    && !disabled
                    && head
                        .as_deref()
                        .is_some_and(|s| matches!(s, "dispatching" | "submitted" | "acknowledged"))
                {
                    return Ok(false);
                }
            }
            if delivery == "settled" && worker_working {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn coordination_paused_assignment(
        &self,
        ns: &StateNamespace,
        id: &CoordinationId,
    ) -> Result<bool, StoreError> {
        self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM coordination_assignments a JOIN collaboration_messages m ON m.session_socket_id=a.session_socket_id AND m.worktree_id=a.worktree_id AND m.message_id=a.current_message_id JOIN thread_controls c ON c.session_socket_id=m.session_socket_id AND c.worktree_id=m.worktree_id AND c.thread_id=m.thread_id WHERE a.session_socket_id=?1 AND a.worktree_id=?2 AND a.coordination_id=?3 AND (c.paused=1 OR c.stopped=1))",params![ns.session_socket_id,ns.space_id.as_str(),id.as_str()],|r|r.get::<_,bool>(0)).map_err(StoreError::from)
    }

    pub(crate) fn coordination_wake_message_eligible(
        &self,
        ns: &StateNamespace,
        message: &MessageId,
        discovered: &crate::DiscoveredWorktree,
    ) -> Result<bool, StoreError> {
        if !self.coordination_wake_current(ns, message)? {
            return Ok(false);
        }
        let Some((id, _)) = self.wake_identity(ns, message)? else {
            return Ok(true);
        };
        self.coordination_wake_eligible(ns, &id, discovered)
    }

    pub(crate) fn wake_identity(
        &self,
        ns: &StateNamespace,
        message: &MessageId,
    ) -> Result<Option<(CoordinationId, i64)>, StoreError> {
        let grammar=self.transaction.query_row("SELECT grammar_version FROM collaboration_messages WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),message.as_str()],|r|r.get::<_,String>(0))?;
        if grammar != "coordination-nudge/v1" {
            return Ok(None);
        }
        let (id, revision) = message
            .as_str()
            .strip_prefix("coordination-wake:")
            .and_then(|s| s.rsplit_once(':'))
            .ok_or_else(|| StoreError::invalid_input("invalid saved continuation identity"))?;
        let revision = revision
            .parse::<i64>()
            .map_err(|_| StoreError::invalid_input("invalid saved continuation revision"))?;
        Ok(Some((CoordinationId::new(id), revision)))
    }

    pub(crate) fn coordination_wake_current(
        &self,
        ns: &StateNamespace,
        message: &MessageId,
    ) -> Result<bool, StoreError> {
        let Some((id, revision)) = self.wake_identity(ns, message)? else {
            return Ok(true);
        };
        let current=self.transaction.query_row("SELECT outcome='open' AND revision=?4 AND wake_revision=?4 AND wake_message_id=?5 FROM coordinations WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),id.as_str(),revision,message.as_str()],|r|r.get::<_,bool>(0)).optional()?;
        Ok(current == Some(true))
    }

    pub(crate) fn coordination_reports(
        &self,
        ns: &StateNamespace,
        owner: &RoutingAddressSnapshot,
        include_completed: bool,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<Value>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT coordination_id FROM coordinations WHERE session_socket_id=?1 AND worktree_id=?2 AND owner_address_id=?3 AND owner_session_id=?4
             AND (?5 OR outcome='open' OR report IS NOT NULL) ORDER BY rowid DESC LIMIT ?6 OFFSET ?7")?;
        let ids = statement
            .query_map(
                params![
                    ns.session_socket_id,
                    ns.space_id.as_str(),
                    owner.address_id.as_str(),
                    owner.agent_session_id.as_ref().map(|s| s.as_str()),
                    include_completed,
                    limit,
                    offset
                ],
                |r| r.get::<_, String>(0).map(CoordinationId::new),
            )?
            .collect::<Result<Vec<_>, _>>()?;
        ids.iter()
            .map(|id| self.coordination_report(ns, id, owner))
            .collect()
    }

    pub(crate) fn check_coordination_replay(
        &self,
        ns: &StateNamespace,
        message: &MessageId,
        input: &Value,
        author: &RoutingAddressSnapshot,
    ) -> Result<(), StoreError> {
        let stored=self.transaction.query_row("SELECT input_json FROM coordination_links WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3 LIMIT 1",
            params![ns.session_socket_id,ns.space_id.as_str(),message.as_str()],|r|r.get::<_,String>(0)).optional()?;
        if let Some(stored) = stored {
            self.require_coordination_capability(ns, author)?;
            let session=self.transaction.query_row("SELECT author_session_snapshot FROM collaboration_messages WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),message.as_str()],|r|r.get::<_,Option<String>>(0))?;
            if session.as_deref() != author.agent_session_id.as_ref().map(|s| s.as_str()) {
                return Err(StoreError::invalid_input(
                    "tracked replay requires the original exact author session",
                ));
            }
            let expected = input.to_string();
            if stored != expected {
                return Err(StoreError::invalid_input(
                    "request id was already used with different coordination fields",
                ));
            }
        } else if input
            .as_object()
            .is_some_and(|m| m.values().any(|v| !v.is_null()))
        {
            return Err(StoreError::invalid_input(
                "request id was already used for an untracked message",
            ));
        }
        Ok(())
    }

    pub(crate) fn link_coordination_message(
        &mut self,
        ns: &StateNamespace,
        message: &MessageId,
        link: &CoordinationLink,
        kind: &str,
        input: &Value,
    ) -> Result<(), StoreError> {
        self.transaction.execute("INSERT INTO coordination_links(session_socket_id,worktree_id,message_id,assignment_id,revision,kind,input_json) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![ns.session_socket_id,ns.space_id.as_str(),message.as_str(),link.assignment_id.as_str(),to_i64(link.revision)?,kind,input.to_string()])?;
        Ok(())
    }

    pub(crate) fn dispatch_assignment(
        &mut self,
        ns: &StateNamespace,
        link: &CoordinationLink,
        draft: &MessageDraft,
        recipient: &crate::MessageRecipientDraft,
        input: &Value,
    ) -> Result<(), StoreError> {
        let session = recipient.agent_session_snapshot.as_ref().ok_or_else(|| {
            StoreError::invalid_input("tracked recipients require an exact session")
        })?;
        if link.revision == 1 {
            self.transaction.execute("INSERT INTO coordination_assignments(session_socket_id,worktree_id,assignment_id,coordination_id,recipient_address_id,recipient_session_id,original_message_id,current_message_id,revision,work_outcome)
                VALUES (?1,?2,?3,?4,?5,?6,?7,?7,1,'pending')",params![ns.session_socket_id,ns.space_id.as_str(),link.assignment_id.as_str(),link.coordination_id.as_str(),recipient.address_id.as_str(),session.as_str(),draft.message_id.as_str()])?;
        } else {
            self.transaction.execute("UPDATE coordination_assignments SET current_message_id=?4,revision=?5,work_outcome='pending',blocked_reason=NULL,waiting_on_assignment_id=NULL,last_reply_message_id=NULL
                WHERE session_socket_id=?1 AND worktree_id=?2 AND assignment_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),link.assignment_id.as_str(),draft.message_id.as_str(),to_i64(link.revision)?])?;
        }
        self.link_coordination_message(ns, &draft.message_id, link, "request", input)
    }

    pub(crate) fn record_assignment_outcome(
        &mut self,
        ns: &StateNamespace,
        link: &CoordinationLink,
        draft: &MessageDraft,
        input: &crate::coordination::TrackingInput,
        now: u64,
    ) -> Result<(), StoreError> {
        self.validate_assignment_wait(ns, link, input.waiting_on_assignment_id.as_ref())?;
        let assignment = self.coordination_assignment(ns, &link.assignment_id)?;
        // An older revision can finish its transport, but cannot rewrite current work.
        let open=self.transaction.query_row("SELECT outcome='open' FROM coordinations WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),link.coordination_id.as_str()],|r|r.get::<_,bool>(0))?;
        if open && assignment["revision"].as_u64() == Some(link.revision) {
            let outcome = input
                .work_outcome
                .map(|v| serde_json::to_value(v).expect("enum serializes"))
                .unwrap_or(json!("unclassified"));
            self.transaction.execute("UPDATE coordination_assignments SET work_outcome=?4,blocked_reason=?5,waiting_on_assignment_id=?6,last_reply_message_id=?7
                WHERE session_socket_id=?1 AND worktree_id=?2 AND assignment_id=?3 AND revision=?8",
                params![ns.session_socket_id,ns.space_id.as_str(),link.assignment_id.as_str(),outcome.as_str(),input.blocked_reason,input.waiting_on_assignment_id.as_ref().map(|v|v.as_str()),draft.message_id.as_str(),to_i64(link.revision)?])?;
            self.advance_coordination(ns, &link.coordination_id, now)?;
        }
        self.link_coordination_message(ns, &draft.message_id, link, "reply", &input.fingerprint())
    }

    fn validate_assignment_wait(
        &self,
        ns: &StateNamespace,
        link: &CoordinationLink,
        wait: Option<&AssignmentId>,
    ) -> Result<(), StoreError> {
        let mut next = wait.cloned();
        let mut seen = std::collections::BTreeSet::from([link.assignment_id.clone()]);
        while let Some(id) = next {
            if !seen.insert(id.clone()) {
                return Err(StoreError::invalid_input(
                    "assignment wait cycle is not allowed",
                ));
            }
            let assignment = self.coordination_assignment(ns, &id)?;
            if assignment["coordination_id"].as_str() != Some(link.coordination_id.as_str()) {
                return Err(StoreError::invalid_input(
                    "waiting_on_assignment_id must belong to this coordination",
                ));
            }
            next = assignment["waiting_on_assignment_id"]
                .as_str()
                .map(AssignmentId::new);
        }
        Ok(())
    }

    pub(crate) fn advance_coordination(
        &mut self,
        ns: &StateNamespace,
        id: &CoordinationId,
        now: u64,
    ) -> Result<(), StoreError> {
        self.transaction.execute("UPDATE coordinations SET revision=revision+1,updated_at_ms=?4 WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3 AND outcome='open'",params![ns.session_socket_id,ns.space_id.as_str(),id.as_str(),to_i64(now)?])?;
        Ok(())
    }

    pub(super) fn cancel_queued_assignment_replies(
        &mut self,
        ns: &StateNamespace,
        id: &CoordinationId,
        recipient: &PaneAddressId,
        session: &crate::AgentSessionId,
        current_revisions_only: bool,
        by: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let revision_filter = if current_revisions_only {
            "AND l.revision<=a.revision"
        } else {
            ""
        };
        let cancelled = {
            let mut statement = self.transaction.prepare(&format!(
                "SELECT d.delivery_id FROM coordination_links l
                 JOIN coordination_assignments a USING(session_socket_id,worktree_id,assignment_id)
                 JOIN collaboration_messages m USING(session_socket_id,worktree_id,message_id)
                 JOIN message_recipients r USING(session_socket_id,worktree_id,message_id)
                 JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,message_id,address_id)
                 WHERE l.session_socket_id=?1 AND l.worktree_id=?2 AND a.coordination_id=?3
                   AND l.kind='reply' {revision_filter}
                   AND m.author_kind='agent_session' AND m.author_address_id=a.recipient_address_id
                   AND m.author_session_snapshot=a.recipient_session_id
                   AND r.address_id=?4 AND r.session_snapshot=?5
                   AND d.address_id=?4 AND d.state='queued'"
            ))?;
            statement
                .query_map(
                    params![
                        ns.session_socket_id,
                        ns.space_id.as_str(),
                        id.as_str(),
                        recipient.as_str(),
                        session.as_str(),
                    ],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        for delivery in cancelled {
            self.transaction.execute(
                "UPDATE mailbox_deliveries SET state='cancelled'
                 WHERE session_socket_id=?1 AND worktree_id=?2 AND delivery_id=?3 AND state='queued'",
                params![ns.session_socket_id, ns.space_id.as_str(), delivery],
            )?;
            self.append_transition(
                ns,
                TransitionEntityKind::MailboxDelivery,
                &delivery,
                TransitionKind::Cancelled,
                by,
            )?;
        }
        Ok(())
    }

    pub(crate) fn resolve_coordination(
        &mut self,
        ns: &StateNamespace,
        owner: &RoutingAddressSnapshot,
        input: &crate::coordination::ResolveInput,
        now: u64,
    ) -> Result<Value, StoreError> {
        self.require_coordination_owner(ns, &input.coordination_id, owner, false)?;
        let old=self.transaction.query_row("SELECT resolve_input_json FROM coordinations WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),input.coordination_id.as_str()],|r|r.get::<_,Option<String>>(0))?;
        let fingerprint =
            serde_json::to_string(input).map_err(|e| StoreError::invalid_input(e.to_string()))?;
        if let Some(old) = old {
            if old != fingerprint {
                return Err(StoreError::invalid_input(
                    "coordination was already resolved with a different report",
                ));
            }
            return self.coordination_report(ns, &input.coordination_id, owner);
        }
        let report = self.coordination_report(ns, &input.coordination_id, owner)?;
        let attribution = MutationAttribution::new(ActorKind::Plugin, "namedrop-coordination", now);
        if input.outcome == CoordinationOutcome::Completed {
            let assignments = report["assignments"].as_array().expect("array");
            let required = assignments
                .iter()
                .map(|a| ReconciledAssignment {
                    assignment_id: AssignmentId::new(a["assignment_id"].as_str().unwrap()),
                    revision: a["revision"].as_u64().unwrap(),
                })
                .collect::<Vec<_>>();
            if input.reconciled_assignments.len() != required.len()
                || required
                    .iter()
                    .any(|r| !input.reconciled_assignments.contains(r))
                || assignments.iter().any(|a| a["work_outcome"] != "completed")
            {
                return Err(StoreError::invalid_input(
                    "completed resolution requires every completed assignment reconciled at its current revision",
                ));
            }
            self.cancel_queued_assignment_replies(
                ns,
                &input.coordination_id,
                &owner.address_id,
                owner
                    .agent_session_id
                    .as_ref()
                    .expect("capability requires a session"),
                true,
                &attribution,
            )?;
        }
        let id = input.coordination_id.as_str();
        let thread = ThreadId::new(format!("coordination-report-thread:{id}"));
        let message = MessageId::new(format!("coordination-report:{id}"));
        self.create_thread(ns, &thread, "Saved coordination report", &attribution)?;
        self.append_message(
            ns,
            &MessageDraft {
                thread_id: thread,
                message_id: message.clone(),
                in_reply_to: None,
                author_address_id: owner.address_id.clone(),
                author_kind: ActorKind::AgentSession,
                author_alias_snapshot: "namedrop".to_owned(),
                author_agent_session_snapshot: owner.agent_session_id.clone(),
                request_id: format!("coordination-report:{id}"),
                authored_source: input.report.clone(),
                body: input.report.clone(),
                grammar_version: "coordination-report/v1".to_owned(),
                recipients: vec![crate::MessageRecipientDraft {
                    address_id: owner.address_id.clone(),
                    alias_snapshot: owner.alias.clone(),
                    agent_session_snapshot: owner.agent_session_id.clone(),
                    delivery_id: crate::DeliveryId::new(format!(
                        "coordination-report-delivery:{id}"
                    )),
                }],
                context_references: vec![],
            },
            &attribution,
        )?;
        let outcome = serde_json::to_value(input.outcome).expect("enum serializes");
        self.transaction.execute("UPDATE coordinations SET outcome=?4,report=?5,reason=?6,resolve_request_id=?7,resolve_input_json=?8,report_message_id=?9,revision=revision+1,updated_at_ms=?10
            WHERE session_socket_id=?1 AND worktree_id=?2 AND coordination_id=?3",params![ns.session_socket_id,ns.space_id.as_str(),id,outcome.as_str(),input.report,input.reason,input.request_id,fingerprint,message.as_str(),to_i64(now)?])?;
        self.coordination_report(ns, &input.coordination_id, owner)
    }
}
