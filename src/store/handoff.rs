use super::*;
use serde_json::{Value, json};

impl StateTransaction<'_> {
    pub(crate) fn returned_review_source(
        &self,
        namespace: &StateNamespace,
        attempt_id: &crate::DeliveryAttemptId,
    ) -> Result<Option<MessageId>, StoreError> {
        self.transaction.query_row(
            "SELECT source.message_id FROM delivery_attempts a
             JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,delivery_id,address_id)
             JOIN collaboration_messages reply USING(session_socket_id,worktree_id,message_id)
             JOIN collaboration_messages source ON source.session_socket_id=reply.session_socket_id
                AND source.worktree_id=reply.worktree_id AND source.message_id=reply.in_reply_to
             WHERE a.session_socket_id=?1 AND a.worktree_id=?2 AND a.attempt_id=?3
                AND source.author_address_id=a.address_id AND source.author_session_snapshot=a.agent_session_id
                AND EXISTS(SELECT 1 FROM message_context_attachments c WHERE c.session_socket_id=source.session_socket_id
                    AND c.worktree_id=source.worktree_id AND c.message_id=source.message_id)",
            params![namespace.session_socket_id,namespace.space_id.as_str(),attempt_id.as_str()],|r|r.get::<_,String>(0).map(MessageId::new)).optional().map_err(StoreError::from)
    }

    pub(crate) fn handoff_review_snapshot(
        &self,
        namespace: &StateNamespace,
        message_id: &MessageId,
        recipient: &PaneAddressId,
    ) -> Result<Option<Value>, StoreError> {
        let snapshot = self.transaction.query_row(
            "SELECT s.payload_json FROM message_context_attachments a
             JOIN working_context_snapshots s USING(session_socket_id,worktree_id,grant_id)
             WHERE a.session_socket_id=?1 AND a.worktree_id=?2 AND a.message_id=?3 AND a.address_id=?4",
            params![namespace.session_socket_id,namespace.space_id.as_str(),message_id.as_str(),recipient.as_str()],|r|r.get::<_,String>(0)).optional()?;
        snapshot
            .map(|s| serde_json::from_str(&s).map_err(|e| StoreError::invalid_input(e.to_string())))
            .transpose()
    }

    pub(crate) fn recipient_preview(
        &self,
        namespace: &StateNamespace,
        discovered: &crate::DiscoveredWorktree,
        address_id: &PaneAddressId,
        delivery_id: Option<&str>,
    ) -> Result<Value, StoreError> {
        let address = self
            .list_pane_addresses(namespace)?
            .into_iter()
            .find(|a| &a.id == address_id);
        let Some(address) = address else {
            return Ok(json!({"address_id":address_id,"constraints":["recipient_removed"]}));
        };
        let live = address
            .pane_id
            .as_ref()
            .and_then(|id| discovered.panes.iter().find(|p| &p.pane_id == id));
        let mut constraints = Vec::new();
        match self.worktree_state(namespace)? {
            Some(s) if s.activated && !s.deactivated => {
                if s.paused {
                    constraints.push("space_paused");
                }
            }
            _ => constraints.push("space_inactive"),
        }
        if !self.reconciliation_completed(namespace)? {
            constraints.push("reconciliation_required");
        }
        if address.collaboration_disabled {
            constraints.push("recipient_disabled");
        }
        if address.pane_id.is_none() {
            constraints.push("recipient_unbound");
        }
        if address.pane_type == crate::PaneType::Terminal {
            if live.is_none() {
                constraints.push("recipient_disconnected");
            } else if live.is_some_and(|p| p.agent.is_some() || p.agent_session_id.is_some()) {
                constraints.push("terminal_contains_agent");
            }
        } else {
            match live.and_then(|p| p.agent_session_id.as_ref()) {
                None => constraints.push("recipient_disconnected"),
                Some(session) if address.agent_session_id.as_ref() != Some(session) => {
                    constraints.push("session_changed")
                }
                Some(session) => {
                    match self.active_integration_registration(namespace, address_id, session)? {
                        None => constraints.push("registration_required"),
                        Some(r) if self.integration_package_failed(namespace, r.runtime)? => {
                            constraints.push("integration_repair_required")
                        }
                        Some(_) => (),
                    }
                    if !matches!(
                        live.and_then(|p| p.agent_status.as_deref()),
                        Some("idle" | "done")
                    ) {
                        constraints.push("recipient_busy_or_unknown");
                    }
                }
            }
        }
        let ahead: u64 = self.transaction.query_row(
            "SELECT COUNT(*) FROM mailbox_deliveries WHERE session_socket_id=?1 AND worktree_id=?2 AND address_id=?3
             AND state NOT IN ('settled','failed','cancelled') AND (?4 IS NULL OR mailbox_order < (
                SELECT mailbox_order FROM mailbox_deliveries WHERE session_socket_id=?1 AND worktree_id=?2 AND delivery_id=?4))",
            params![namespace.session_socket_id,namespace.space_id.as_str(),address_id.as_str(),delivery_id],|r|from_i64(r.get(0)?))?;
        if ahead > 0 {
            constraints.push("earlier_delivery_pending");
        }
        Ok(
            json!({"address_id":address_id,"alias":address.alias,"pane_id":address.pane_id,
            "pane_type":address.pane_type,
            "agent_session_id":live.and_then(|p|p.agent_session_id.as_ref()),"agent_status":live.and_then(|p|p.agent_status.as_ref()),
            "constraints":constraints,"earlier_deliveries":ahead,
            "delivery_expectation":if !constraints.is_empty(){"queued until constraints clear"}else if address.pane_type==crate::PaneType::Terminal {"command executes after dispatch verifies an available shell"}else{"eligible after dispatch revalidates a settled session"}}),
        )
    }

    pub(crate) fn add_handoff_outlooks(
        &self,
        namespace: &StateNamespace,
        live: &crate::spaces::LiveSpaces,
        report: &mut Value,
    ) -> Result<(), StoreError> {
        let thread = self.thread_control_state(
            namespace,
            &ThreadId::new(report["thread_id"].as_str().unwrap()),
        )?;
        for recipient in report["recipients"].as_array_mut().unwrap() {
            if recipient["state"] == "queued" {
                let address =
                    PaneAddressId::new(recipient["target"]["address_id"].as_str().unwrap());
                let delivery = recipient["delivery_id"].as_str().unwrap();
                let principal = self.principal(namespace, &address)?;
                let seat_delivery = if principal.ns == *namespace {
                    delivery.to_owned()
                } else {
                    super::bridge::bridged_id(&namespace.space_id, delivery)
                };
                let mut outlook = match live.of(&principal.ns.space_id) {
                    Some(discovered) => self.recipient_preview(
                        &principal.ns,
                        discovered,
                        &principal.address_id,
                        Some(&seat_delivery),
                    )?,
                    None => json!({"address_id":address,"constraints":["remote_status_unknown"],
                        "delivery_expectation":"queued until constraints clear"}),
                };
                if thread.paused {
                    outlook["constraints"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!("thread_paused"));
                }
                if thread.stopped {
                    outlook["constraints"]
                        .as_array_mut()
                        .unwrap()
                        .push(json!("thread_stopped"));
                }
                if thread.paused || thread.stopped {
                    outlook["delivery_expectation"] = json!("queued until constraints clear");
                }
                recipient["outlook"] = outlook;
            }
        }
        Ok(())
    }

    pub(crate) fn handoff_message_ids(
        &self,
        namespace: &StateNamespace,
        author: &RoutingAddressSnapshot,
        include_completed: bool,
        limit: u32,
        offset: u32,
    ) -> Result<Vec<MessageId>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT m.message_id FROM collaboration_messages m
             WHERE m.session_socket_id = ?1 AND m.worktree_id = ?2
               AND m.author_address_id = ?3 AND m.author_session_snapshot = ?4
               AND m.in_reply_to IS NULL
               AND (?5 OR EXISTS (
                   SELECT 1 FROM mailbox_deliveries d
                   JOIN delivery_seats s USING (session_socket_id, worktree_id, delivery_id)
                   WHERE d.session_socket_id = m.session_socket_id AND d.worktree_id = m.worktree_id
                     AND d.message_id = m.message_id AND s.progress NOT IN ('failed', 'cancelled')
                     AND NOT EXISTS (
                         SELECT 1 FROM delivery_attempts a JOIN delivery_dispositions p USING (session_socket_id, worktree_id, attempt_id)
                         WHERE a.session_socket_id = d.session_socket_id AND a.worktree_id = d.worktree_id AND a.delivery_id = d.delivery_id
                     )
               )) ORDER BY m.rowid DESC LIMIT ?6 OFFSET ?7")?;
        statement
            .query_map(
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    author.address_id.as_str(),
                    author.agent_session_id.as_ref().map(|s| s.as_str()),
                    include_completed,
                    limit,
                    offset
                ],
                |row| row.get::<_, String>(0).map(MessageId::new),
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn handoff_report(
        &self,
        namespace: &StateNamespace,
        author: &RoutingAddressSnapshot,
        message_id: &MessageId,
        now: u64,
        include_replies: bool,
    ) -> Result<Value, StoreError> {
        let message = self
            .transaction
            .query_row(
                "SELECT thread_id, body, created_at_ms FROM collaboration_messages
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3
               AND author_address_id = ?4 AND author_session_snapshot = ?5
               AND grammar_version != 'result-set/v1'",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    message_id.as_str(),
                    author.address_id.as_str(),
                    author.agent_session_id.as_ref().map(|s| s.as_str())
                ],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        from_i64(row.get(2)?)?,
                    ))
                },
            )
            .optional()?
            .ok_or_else(|| {
                StoreError::invalid_input("handoff is not owned by this exact Agent Session")
            })?;
        let progress = self.handoff_recipients(namespace, message_id)?;
        let complete = progress.iter().all(|p| p.result_status().is_some());
        let expected = progress.len();
        let completed = progress
            .iter()
            .filter(|p| p.result_status().is_some())
            .count();
        let recipients = progress.into_iter().map(|p| {
            let mut value = serde_json::to_value(&p).map_err(|e| StoreError::invalid_input(e.to_string()))?;
            value["elapsed_ms"] = json!(now.saturating_sub(p.created_at_ms));
            value["result_status"] = json!(p.result_status());
            if let Some(attempt)=p.attempt_id.as_ref()
                && let Some((code,output))=self.terminal_result(namespace,attempt)? {
                value["terminal_result"]=json!({"exit_code":code,"output":output});
            }
            value["needs_attention"] = json!(matches!(p.state, DeliveryState::Held | DeliveryState::Failed) || (p.state == DeliveryState::Settled && p.result_status().is_none()));
            if include_replies && let Some(reply_id) = p.reply_message_id {
                let body: String = self.transaction.query_row(
                    "SELECT body FROM collaboration_messages WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3",
                    params![namespace.session_socket_id, namespace.space_id.as_str(), reply_id.as_str()], |r| r.get(0))?;
                value["untrusted_reply"] = json!(body);
            }
            Ok(value)
        }).collect::<Result<Vec<_>,StoreError>>()?;
        Ok(
            json!({"message_id":message_id,"thread_id":message.0,"request":message.1,"created_at_ms":message.2,
            "elapsed_ms":now.saturating_sub(message.2),"complete":complete,"expected":expected,"completed":completed,"recipients":recipients}),
        )
    }

    pub(crate) fn handoff_recipients(
        &self,
        namespace: &StateNamespace,
        message_id: &MessageId,
    ) -> Result<Vec<HandoffRecipient>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT r.address_id, r.alias_snapshot, d.delivery_id, s.progress, d.created_at_ms,
                    a.attempt_id, ack.acknowledged_at_ms, p.disposition_kind,
                    CASE WHEN p.disposition_kind = 'reply' THEN p.disposition_id END,
                    COALESCE(p.occurred_at_ms, ack.acknowledged_at_ms, a.created_at_ms, d.created_at_ms),
                    e.origin_label
             FROM message_recipients r JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, message_id, address_id)
             JOIN delivery_seats s USING (session_socket_id, worktree_id, delivery_id)
             LEFT JOIN space_envoys e USING (session_socket_id, worktree_id, address_id)
             LEFT JOIN delivery_attempts a ON a.rowid = (
                 SELECT latest.rowid FROM delivery_attempts latest
                 WHERE latest.session_socket_id = d.session_socket_id AND latest.worktree_id = d.worktree_id AND latest.delivery_id = d.delivery_id
                 ORDER BY latest.created_at_ms DESC, latest.rowid DESC LIMIT 1
             )
             LEFT JOIN delivery_acknowledgements ack USING (session_socket_id, worktree_id, attempt_id)
             LEFT JOIN delivery_dispositions p USING (session_socket_id, worktree_id, attempt_id)
             WHERE r.session_socket_id = ?1 AND r.worktree_id = ?2 AND r.message_id = ?3 ORDER BY r.recipient_order")?;
        statement
            .query_map(
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    message_id.as_str()
                ],
                |r| {
                    Ok(HandoffRecipient {
                        target: DeliveryAddress {
                            workspace: r
                                .get::<_, Option<String>>(10)?
                                .map(|label| crate::WorkspaceLabel::from_herdr(&label)),
                            ..DeliveryAddress::new(
                                PaneAddressId::new(r.get::<_, String>(0)?),
                                r.get::<_, String>(1)?,
                            )
                        },
                        delivery_id: crate::DeliveryId::new(r.get::<_, String>(2)?),
                        state: parse_delivery_state(r.get(3)?)?,
                        created_at_ms: from_i64(r.get(4)?)?,
                        attempt_id: r
                            .get::<_, Option<String>>(5)?
                            .map(crate::DeliveryAttemptId::new),
                        acknowledged_at_ms: r
                            .get::<_, Option<i64>>(6)?
                            .map(from_i64)
                            .transpose()?,
                        disposition: r.get(7)?,
                        reply_message_id: r.get::<_, Option<String>>(8)?.map(MessageId::new),
                        last_activity_at_ms: from_i64(r.get(9)?)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }
}

#[derive(Serialize)]
pub(crate) struct HandoffRecipient {
    pub target: DeliveryAddress,
    pub delivery_id: crate::DeliveryId,
    pub state: DeliveryState,
    pub created_at_ms: u64,
    pub attempt_id: Option<crate::DeliveryAttemptId>,
    pub acknowledged_at_ms: Option<u64>,
    pub last_activity_at_ms: u64,
    pub disposition: Option<String>,
    pub reply_message_id: Option<MessageId>,
}

impl HandoffRecipient {
    pub fn result_status(&self) -> Option<crate::ResultSetStatus> {
        match (self.disposition.as_deref(), self.state) {
            (Some("reply"), _) => Some(crate::ResultSetStatus::Replied),
            (Some("no_reply"), _) => Some(crate::ResultSetStatus::NoReply),
            (_, DeliveryState::Failed) => Some(crate::ResultSetStatus::Failed),
            (_, DeliveryState::Cancelled) => Some(crate::ResultSetStatus::Cancelled),
            _ => None,
        }
    }
}
