use super::*;
use crate::PaneType;

pub(super) const SCHEMA: &str = r#"
ALTER TABLE pane_addresses ADD COLUMN pane_type TEXT NOT NULL DEFAULT 'agent' CHECK(pane_type IN ('agent','terminal'));
CREATE TABLE terminal_executions (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    attempt_id TEXT NOT NULL,
    terminal_id TEXT NOT NULL,
    command TEXT NOT NULL,
    started_at_ms INTEGER,
    completed_at_ms INTEGER,
    exit_code INTEGER,
    output TEXT,
    PRIMARY KEY(session_socket_id, worktree_id, attempt_id),
    FOREIGN KEY(session_socket_id, worktree_id, attempt_id)
        REFERENCES delivery_attempts(session_socket_id, worktree_id, attempt_id)
);
CREATE TRIGGER terminal_command_immutable BEFORE UPDATE OF terminal_id,command ON terminal_executions
BEGIN SELECT RAISE(ABORT,'terminal command identity is immutable'); END;
CREATE TRIGGER terminal_result_immutable BEFORE UPDATE OF completed_at_ms,exit_code,output ON terminal_executions
WHEN OLD.completed_at_ms IS NOT NULL
BEGIN SELECT RAISE(ABORT,'terminal result is immutable'); END;
"#;

impl StateTransaction<'_> {
    pub(crate) fn terminal_result(
        &self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
    ) -> Result<Option<(i32, String)>, StoreError> {
        self.transaction.query_row("SELECT exit_code,output FROM terminal_executions WHERE session_socket_id=?1 AND worktree_id=?2 AND attempt_id=?3 AND completed_at_ms IS NOT NULL",
            params![ns.session_socket_id,ns.space_id.as_str(),attempt.as_str()],|r|Ok((r.get(0)?,r.get(1)?))).optional().map_err(StoreError::from)
    }
    pub(crate) fn prepare_terminal_execution(
        &mut self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
        terminal_id: &str,
    ) -> Result<(), StoreError> {
        let (body, grammar, reply): (String, String, Option<String>) = self.transaction.query_row(
            "SELECT m.body,m.grammar_version,m.in_reply_to FROM delivery_attempts a
             JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,delivery_id,address_id)
             JOIN collaboration_messages m USING(session_socket_id,worktree_id,message_id)
             WHERE a.session_socket_id=?1 AND a.worktree_id=?2 AND a.attempt_id=?3",
            params![ns.session_socket_id, ns.space_id.as_str(), attempt.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if reply.is_some()
            || !matches!(
                grammar.as_str(),
                "mention/v1" | "mention-hook/v1" | "delegated/v1"
            )
        {
            return Err(StoreError::invalid_input(
                "terminal recipients execute new command requests only, never replies or result reports",
            ));
        }
        let command = if grammar == "mention-hook/v1" {
            crate::parse_mentions(&body)
                .map_err(StoreError::invalid_input)?
                .body
        } else {
            body
        };
        self.transaction.execute("INSERT INTO terminal_executions(session_socket_id,worktree_id,attempt_id,terminal_id,command) VALUES(?1,?2,?3,?4,?5)",
            params![ns.session_socket_id,ns.space_id.as_str(),attempt.as_str(),terminal_id,command])?;
        Ok(())
    }

    pub(crate) fn terminal_execution(
        &self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
    ) -> Result<Option<(String, Option<u64>)>, StoreError> {
        self.transaction.query_row("SELECT terminal_id,started_at_ms FROM terminal_executions WHERE session_socket_id=?1 AND worktree_id=?2 AND attempt_id=?3",
            params![ns.session_socket_id,ns.space_id.as_str(),attempt.as_str()],|r|Ok((r.get(0)?,r.get::<_,Option<i64>>(1)?.map(from_i64).transpose()?))).optional().map_err(StoreError::from)
    }

    pub(crate) fn claim_terminal_execution(
        &mut self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
        pane: &crate::DiscoveredPane,
        at: &MutationAttribution,
    ) -> Result<String, StoreError> {
        // The helper may start before pane.run returns. Both dispatching and
        // submitted are claimable; a replay, a held attempt, or changed binding is not.
        let command = self.transaction.query_row(
            "SELECT t.command FROM terminal_executions t JOIN delivery_attempts a USING(session_socket_id,worktree_id,attempt_id)
             JOIN delivery_attempt_state s USING(session_socket_id,worktree_id,attempt_id,address_id)
             JOIN pane_bindings b USING(session_socket_id,worktree_id,binding_id,address_id)
             JOIN pane_addresses p USING(session_socket_id,worktree_id,address_id)
             JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,delivery_id,address_id)
             JOIN collaboration_messages m USING(session_socket_id,worktree_id,message_id)
             JOIN thread_controls c USING(session_socket_id,worktree_id,thread_id)
             JOIN worktrees w USING(session_socket_id,worktree_id)
             WHERE t.session_socket_id=?1 AND t.worktree_id=?2 AND t.attempt_id=?3
                AND t.terminal_id=?4 AND b.pane_id=?5 AND b.active=1 AND p.pane_type='terminal'
                AND t.started_at_ms IS NULL AND s.state IN ('dispatching','submitted')
                AND p.collaboration_disabled=0 AND p.retired=0 AND c.paused=0 AND c.stopped=0
                AND w.activated=1 AND w.paused=0 AND w.deactivated=0",
            params![ns.session_socket_id,ns.space_id.as_str(),attempt.as_str(),pane.terminal_id,pane.pane_id],|r|r.get::<_,String>(0))
            .optional()?.ok_or_else(||StoreError::invalid_input("terminal execution is no longer claimable for this pane"))?;
        self.transaction.execute("UPDATE terminal_executions SET started_at_ms=?4 WHERE session_socket_id=?1 AND worktree_id=?2 AND attempt_id=?3",
            params![ns.session_socket_id,ns.space_id.as_str(),attempt.as_str(),to_i64(at.occurred_at_ms)?])?;
        if self.delivery_attempt_state(ns, attempt)? == DeliveryState::Dispatching {
            self.transition_delivery_attempt(ns, attempt, DeliveryState::Submitted, at)?;
        }
        self.ensure_delivery_attempt_acknowledged(
            ns,
            attempt,
            &crate::AgentSessionId::new(""),
            at,
        )?;
        Ok(command)
    }

    pub(crate) fn record_terminal_result(
        &mut self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
        code: i32,
        output: &str,
        at: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let updated=self.transaction.execute("UPDATE terminal_executions SET completed_at_ms=?4,exit_code=?5,output=?6 WHERE session_socket_id=?1 AND worktree_id=?2 AND attempt_id=?3 AND started_at_ms IS NOT NULL AND completed_at_ms IS NULL",
            params![ns.session_socket_id,ns.space_id.as_str(),attempt.as_str(),to_i64(at.occurred_at_ms)?,code,output])?;
        if updated == 0 {
            return Err(StoreError::invalid_input(
                "terminal execution is not running",
            ));
        }
        Ok(())
    }

    pub(crate) fn finish_terminal_execution(
        &mut self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
        code: i32,
        output: &str,
        at: &MutationAttribution,
    ) -> Result<(), StoreError> {
        if !matches!(
            self.delivery_attempt_state(ns, attempt)?,
            DeliveryState::Acknowledged
        ) {
            return Ok(());
        }
        let (thread,message,source,source_alias,source_session,recipient,alias):(String,String,String,String,Option<String>,String,String)=self.transaction.query_row(
            "SELECT m.thread_id,m.message_id,m.author_address_id,m.author_alias_snapshot,m.author_session_snapshot,a.address_id,r.alias_snapshot
             FROM delivery_attempts a JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,delivery_id,address_id)
             JOIN collaboration_messages m USING(session_socket_id,worktree_id,message_id)
             JOIN message_recipients r USING(session_socket_id,worktree_id,message_id,address_id)
             WHERE a.session_socket_id=?1 AND a.worktree_id=?2 AND a.attempt_id=?3",
            params![ns.session_socket_id,ns.space_id.as_str(),attempt.as_str()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
        let body = format!("@{alias} command finished with exit code {code}.\n{output}");
        let reply_id = MessageId::new(format!("terminal-result:{}", attempt.as_str()));
        let receipt = self.append_message(
            ns,
            &MessageDraft {
                thread_id: ThreadId::new(thread),
                message_id: reply_id.clone(),
                in_reply_to: Some(MessageId::new(message)),
                author_address_id: PaneAddressId::new(recipient),
                author_kind: ActorKind::AgentSession,
                author_alias_snapshot: alias,
                author_agent_session_snapshot: None,
                request_id: reply_id.as_str().to_owned(),
                authored_source: body.clone(),
                body,
                // Router-owned completion follows the existing report-to-human path.
                grammar_version: "terminal-result/v1".to_owned(),
                recipients: vec![crate::MessageRecipientDraft {
                    address_id: PaneAddressId::new(source),
                    alias_snapshot: source_alias,
                    agent_session_snapshot: source_session.map(crate::AgentSessionId::new),
                    delivery_id: crate::DeliveryId::new(format!(
                        "terminal-result-delivery:{}",
                        attempt.as_str()
                    )),
                }],
                context_references: vec![],
            },
            at,
        )?;
        let grouped:bool=self.transaction.query_row(
            "SELECT m.grammar_version='delegated/v1' AND (SELECT COUNT(*) FROM message_recipients r WHERE r.session_socket_id=m.session_socket_id AND r.worktree_id=m.worktree_id AND r.message_id=m.message_id)>1
             FROM collaboration_messages m WHERE m.session_socket_id=?1 AND m.worktree_id=?2 AND m.message_id=(SELECT in_reply_to FROM collaboration_messages WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3)",
            params![ns.session_socket_id,ns.space_id.as_str(),receipt.message_id.as_str()],|r|r.get(0))?;
        if grouped {
            // Joined results own group presentation.
            let changed=self.transaction.execute("UPDATE mailbox_deliveries SET state='settled' WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3 AND state='queued'",
                params![ns.session_socket_id,ns.space_id.as_str(),receipt.message_id.as_str()])?;
            if changed > 0 {
                self.append_transition(
                    ns,
                    TransitionEntityKind::MailboxDelivery,
                    &format!("terminal-result-delivery:{}", attempt.as_str()),
                    TransitionKind::Settled,
                    at,
                )?;
            }
        }
        self.record_reply_disposition(
            ns,
            attempt,
            &receipt.message_id,
            &crate::AgentSessionId::new(""),
            at,
        )
    }

    pub(crate) fn pane_type(
        &self,
        ns: &StateNamespace,
        id: &PaneAddressId,
    ) -> Result<PaneType, StoreError> {
        self.transaction.query_row("SELECT pane_type FROM pane_addresses WHERE session_socket_id=?1 AND worktree_id=?2 AND address_id=?3",
            params![ns.session_socket_id,ns.space_id.as_str(),id.as_str()], |r| {
                Ok(if r.get::<_,String>(0)? == "terminal" {PaneType::Terminal} else {PaneType::Agent})
            }).map_err(StoreError::from)
    }

    pub(crate) fn set_pane_type(
        &mut self,
        ns: &StateNamespace,
        id: &PaneAddressId,
        kind: PaneType,
    ) -> Result<(), StoreError> {
        if self.pane_type(ns, id)? != kind {
            let pending:bool = self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM mailbox_deliveries WHERE session_socket_id=?1 AND worktree_id=?2 AND address_id=?3 AND state NOT IN ('settled','failed','cancelled'))",
                params![ns.session_socket_id,ns.space_id.as_str(),id.as_str()],|r|r.get(0))?;
            if pending {
                return Err(StoreError::invalid_input(
                    "finish or cancel pending deliveries before changing pane type",
                ));
            }
        }
        self.transaction.execute("UPDATE pane_addresses SET pane_type=?4 WHERE session_socket_id=?1 AND worktree_id=?2 AND address_id=?3",
            params![ns.session_socket_id,ns.space_id.as_str(),id.as_str(),kind.as_str()])?;
        Ok(())
    }
}
