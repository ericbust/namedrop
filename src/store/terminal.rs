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

/// The physical terminal a person allowed other workspaces to command.
pub(super) const REMOTE_SCHEMA: &str =
    "ALTER TABLE pane_addresses ADD COLUMN remote_commands_terminal_id TEXT;";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RemoteCommandRefusal {
    NotShared,
    OtherTerminal,
    AuthorSpaceInactive,
}

impl std::fmt::Display for RemoteCommandRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NotShared => "this terminal does not accept commands from other workspaces",
            Self::OtherTerminal => {
                "commands from other workspaces were allowed for a different shell"
            }
            Self::AuthorSpaceInactive => "the sending workspace is no longer activated",
        })
    }
}

/// `live_terminal` is `None` at send time, when no pane was discovered.
pub(crate) fn admit_remote_command(
    grant: Option<&str>,
    live_terminal: Option<&str>,
    author_space_live: bool,
) -> Result<(), RemoteCommandRefusal> {
    let grant = grant.ok_or(RemoteCommandRefusal::NotShared)?;
    if live_terminal.is_some_and(|live| live != grant) {
        return Err(RemoteCommandRefusal::OtherTerminal);
    }
    if !author_space_live {
        return Err(RemoteCommandRefusal::AuthorSpaceInactive);
    }
    Ok(())
}

pub(crate) fn terminal_runs(grammar: &str, replies: bool) -> bool {
    !replies && matches!(grammar, "mention/v1" | "mention-hook/v1" | "delegated/v1")
}

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
        at: &MutationAttribution,
    ) -> Result<Result<(), RemoteCommandRefusal>, StoreError> {
        let (body, grammar, reply): (String, String, Option<String>) = self.transaction.query_row(
            "SELECT m.body,m.grammar_version,m.in_reply_to FROM delivery_attempts a
             JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,delivery_id,address_id)
             JOIN collaboration_messages m USING(session_socket_id,worktree_id,message_id)
             WHERE a.session_socket_id=?1 AND a.worktree_id=?2 AND a.attempt_id=?3",
            params![ns.session_socket_id, ns.space_id.as_str(), attempt.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        if !terminal_runs(&grammar, reply.is_some()) {
            return Err(StoreError::invalid_input(
                "terminal recipients execute new command requests only, never replies or result reports",
            ));
        }
        if let Err(refusal) = self.remote_command_admission(ns, attempt, terminal_id)? {
            self.refuse_remote_command(ns, attempt, at)?;
            return Ok(Err(refusal));
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
        Ok(Ok(()))
    }

    fn remote_command_admission(
        &self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
        live_terminal: &str,
    ) -> Result<Result<(), RemoteCommandRefusal>, StoreError> {
        let (author, grant): (String, Option<String>) = self.transaction.query_row(
            "SELECT m.author_address_id,p.remote_commands_terminal_id FROM delivery_attempts a
             JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,delivery_id,address_id)
             JOIN collaboration_messages m USING(session_socket_id,worktree_id,message_id)
             JOIN pane_addresses p ON p.session_socket_id=a.session_socket_id AND p.worktree_id=a.worktree_id AND p.address_id=a.address_id
             WHERE a.session_socket_id=?1 AND a.worktree_id=?2 AND a.attempt_id=?3",
            params![ns.session_socket_id, ns.space_id.as_str(), attempt.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let Some(origin) = self.envoy_origin(ns, &PaneAddressId::new(author))? else {
            return Ok(Ok(()));
        };
        let home = StateNamespace::new(ns.session_socket_id.clone(), origin.space);
        let live = self
            .worktree_state(&home)?
            .is_some_and(|state| state.activated && !state.deactivated);
        Ok(admit_remote_command(
            grant.as_deref(),
            Some(live_terminal),
            live,
        ))
    }

    /// Failed and Cancelled are the outcomes reflection carries home, so the
    /// author's join completes instead of waiting on a command that never runs.
    fn refuse_remote_command(
        &mut self,
        ns: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
        at: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let next = match self.delivery_attempt_state(ns, attempt)? {
            DeliveryState::Dispatching => DeliveryState::Failed,
            _ => DeliveryState::Cancelled,
        };
        self.transition_delivery_attempt(ns, attempt, next, at)
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
    ) -> Result<Result<String, RemoteCommandRefusal>, StoreError> {
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
        if let Err(refusal) = self.remote_command_admission(ns, attempt, &pane.terminal_id)? {
            self.refuse_remote_command(ns, attempt, at)?;
            return Ok(Err(refusal));
        }
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
        Ok(Ok(command))
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
        if let Some(origin) = self.envoy_origin(ns, &PaneAddressId::new(source.clone()))?
            && !self
                .worktree_state(&StateNamespace::new(
                    ns.session_socket_id.clone(),
                    origin.space,
                ))?
                .is_some_and(|state| state.activated && !state.deactivated)
        {
            // The result waits, acknowledged, until its workspace returns;
            // reconciliation finishes it then.
            return Ok(());
        }
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
        self.settle_grouped_terminal_result(ns, &receipt.message_id, at)?;
        for copy in self.forwarded_copies(ns, &receipt.message_id)? {
            let holder = StateNamespace::new(ns.session_socket_id.clone(), copy.space);
            self.settle_grouped_terminal_result(&holder, &copy.message_id, at)?;
        }
        self.record_reply_disposition(
            ns,
            attempt,
            &receipt.message_id,
            &crate::AgentSessionId::new(""),
            at,
        )
    }

    /// Each workspace judges grouping against its own copy of the request: a
    /// copy that alone has one recipient can belong to the author's group.
    fn settle_grouped_terminal_result(
        &mut self,
        ns: &StateNamespace,
        result: &MessageId,
        at: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let grouped:bool=self.transaction.query_row(
            "SELECT m.grammar_version='delegated/v1' AND (SELECT COUNT(*) FROM message_recipients r WHERE r.session_socket_id=m.session_socket_id AND r.worktree_id=m.worktree_id AND r.message_id=m.message_id)>1
             FROM collaboration_messages m WHERE m.session_socket_id=?1 AND m.worktree_id=?2 AND m.message_id=(SELECT in_reply_to FROM collaboration_messages WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3)",
            params![ns.session_socket_id,ns.space_id.as_str(),result.as_str()],|r|r.get(0))?;
        if !grouped {
            return Ok(());
        }
        // Joined results own group presentation.
        let queued = self
            .transaction
            .prepare("SELECT delivery_id FROM mailbox_deliveries WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3 AND state='queued'")?
            .query_map(params![ns.session_socket_id,ns.space_id.as_str(),result.as_str()],|r|r.get::<_,String>(0))?
            .collect::<Result<Vec<_>,_>>()?;
        for delivery in queued {
            self.transaction.execute("UPDATE mailbox_deliveries SET state='settled' WHERE session_socket_id=?1 AND worktree_id=?2 AND delivery_id=?3",
                params![ns.session_socket_id,ns.space_id.as_str(),delivery])?;
            self.append_transition(
                ns,
                TransitionEntityKind::MailboxDelivery,
                &delivery,
                TransitionKind::Settled,
                at,
            )?;
        }
        Ok(())
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
        at: &MutationAttribution,
    ) -> Result<(), StoreError> {
        if self.pane_type(ns, id)? != kind {
            self.clear_remote_commands(ns, id, at)?;
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

    /// Allows other workspaces to command the shell the address is bound to
    /// right now. Never from a named terminal or a running command, so a
    /// command delivered to a terminal cannot grant itself.
    pub(crate) fn allow_remote_commands(
        &mut self,
        ns: &StateNamespace,
        id: &PaneAddressId,
        invoker: &crate::DiscoveredPane,
        panes: &[crate::DiscoveredPane],
        at: &MutationAttribution,
    ) -> Result<PaneAddressRecord, StoreError> {
        let invoker_commands:bool = self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM pane_bindings b JOIN pane_addresses p USING(session_socket_id,worktree_id,address_id)
                 WHERE b.session_socket_id=?1 AND b.worktree_id=?2 AND b.pane_id=?3 AND b.active=1 AND p.pane_type='terminal')
             OR EXISTS(SELECT 1 FROM terminal_executions WHERE session_socket_id=?1 AND terminal_id=?4
                 AND started_at_ms IS NOT NULL AND completed_at_ms IS NULL)",
            params![ns.session_socket_id,ns.space_id.as_str(),invoker.pane_id,invoker.terminal_id],|r|r.get(0))?;
        if invoker_commands {
            return Err(StoreError::invalid_input(
                "allow-remote cannot run from a named terminal; run it from another pane in this workspace",
            ));
        }
        let record = self.active_address(ns, id)?;
        if record.pane_type != PaneType::Terminal {
            return Err(StoreError::invalid_input(format!(
                "@{} is not a named terminal; only terminals run commands",
                record.alias
            )));
        }
        let pane = record
            .pane_id
            .as_deref()
            .and_then(|pane_id| panes.iter().find(|pane| pane.pane_id == pane_id))
            .filter(|pane| pane.agent.is_none() && pane.agent_session_id.is_none())
            .ok_or_else(|| {
                StoreError::invalid_input(format!(
                    "@{} is not bound to a live shell pane",
                    record.alias
                ))
            })?;
        let changed = self.transaction.execute(
            "UPDATE pane_addresses SET remote_commands_terminal_id=?4 WHERE session_socket_id=?1 AND worktree_id=?2 AND address_id=?3 AND remote_commands_terminal_id IS NOT ?4",
            params![ns.session_socket_id,ns.space_id.as_str(),id.as_str(),pane.terminal_id])?;
        if changed == 1 {
            self.append_transition(
                ns,
                TransitionEntityKind::PaneAddress,
                id.as_str(),
                TransitionKind::RemoteCommandsAllowed,
                at,
            )?;
        }
        self.active_address(ns, id)
    }

    pub(crate) fn deny_remote_commands(
        &mut self,
        ns: &StateNamespace,
        id: &PaneAddressId,
        at: &MutationAttribution,
    ) -> Result<PaneAddressRecord, StoreError> {
        self.active_address(ns, id)?;
        self.clear_remote_commands(ns, id, at)?;
        let queued = self
            .transaction
            .prepare(
                "SELECT d.delivery_id,d.message_id FROM mailbox_deliveries d
                 JOIN collaboration_messages m USING(session_socket_id,worktree_id,message_id)
                 JOIN space_envoys e ON e.session_socket_id=m.session_socket_id AND e.worktree_id=m.worktree_id AND e.address_id=m.author_address_id
                 WHERE d.session_socket_id=?1 AND d.worktree_id=?2 AND d.address_id=?3 AND d.state='queued'",
            )?
            .query_map(params![ns.session_socket_id,ns.space_id.as_str(),id.as_str()],|r|Ok((r.get::<_,String>(0)?,MessageId::new(r.get::<_,String>(1)?))))?
            .collect::<Result<Vec<_>,_>>()?;
        for (delivery, message) in queued {
            self.transaction.execute("UPDATE mailbox_deliveries SET state='failed' WHERE session_socket_id=?1 AND worktree_id=?2 AND delivery_id=?3",
                params![ns.session_socket_id,ns.space_id.as_str(),delivery])?;
            self.append_transition(
                ns,
                TransitionEntityKind::MailboxDelivery,
                &delivery,
                TransitionKind::Failed,
                at,
            )?;
            self.complete_result_set_if_ready(ns, &message, at)?;
        }
        self.active_address(ns, id)
    }

    pub(super) fn clear_remote_commands(
        &mut self,
        ns: &StateNamespace,
        id: &PaneAddressId,
        at: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let changed = self.transaction.execute(
            "UPDATE pane_addresses SET remote_commands_terminal_id=NULL WHERE session_socket_id=?1 AND worktree_id=?2 AND address_id=?3 AND remote_commands_terminal_id IS NOT NULL",
            params![ns.session_socket_id,ns.space_id.as_str(),id.as_str()])?;
        if changed == 1 {
            self.append_transition(
                ns,
                TransitionEntityKind::PaneAddress,
                id.as_str(),
                TransitionKind::RemoteCommandsRevoked,
                at,
            )?;
        }
        Ok(())
    }

    fn active_address(
        &self,
        ns: &StateNamespace,
        id: &PaneAddressId,
    ) -> Result<PaneAddressRecord, StoreError> {
        self.list_pane_addresses(ns)?
            .into_iter()
            .find(|record| &record.id == id)
            .ok_or_else(|| StoreError::invalid_input("Pane Address is not active"))
    }
}
