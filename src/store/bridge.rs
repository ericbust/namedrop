//! Cross-workspace messages. A message is always written in its author's
//! space; a recipient in another space is an envoy there, a claim-less local
//! stand-in address. Appending a message that names envoys writes one copy per
//! remote space, and a copy's outcome is reflected back onto the envoy's
//! delivery so the author's own joins see it.

use super::*;
use crate::{SpaceId, WorkspaceLabel};

pub(super) const SCHEMA: &str = r#"
CREATE TABLE space_envoys (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    address_id TEXT NOT NULL,
    origin_space_id TEXT NOT NULL,
    origin_address_id TEXT NOT NULL,
    origin_alias TEXT NOT NULL,
    origin_label TEXT NOT NULL,
    host_label TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, address_id),
    UNIQUE (session_socket_id, worktree_id, origin_space_id, origin_address_id),
    FOREIGN KEY (session_socket_id, worktree_id, address_id)
        REFERENCES pane_addresses (session_socket_id, worktree_id, address_id)
);
CREATE TABLE bridged_messages (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    origin_space_id TEXT NOT NULL,
    origin_message_id TEXT NOT NULL,
    origin_label_snapshot TEXT NOT NULL,
    label_snapshot TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, message_id),
    UNIQUE (session_socket_id, worktree_id, origin_space_id, origin_message_id),
    FOREIGN KEY (session_socket_id, worktree_id, message_id)
        REFERENCES collaboration_messages (session_socket_id, worktree_id, message_id)
);
CREATE TABLE bridged_threads (
    session_socket_id TEXT NOT NULL,
    worktree_id TEXT NOT NULL,
    thread_id TEXT NOT NULL,
    peer_space_id TEXT NOT NULL,
    peer_thread_id TEXT NOT NULL,
    PRIMARY KEY (session_socket_id, worktree_id, thread_id, peer_space_id),
    UNIQUE (session_socket_id, worktree_id, peer_space_id, peer_thread_id),
    FOREIGN KEY (session_socket_id, worktree_id, thread_id)
        REFERENCES collaboration_threads (session_socket_id, worktree_id, thread_id)
);
"#;

pub(super) const DELIVERY_SEATS_SCHEMA: &str = r#"
-- An envoy delivery settles when its copy is forwarded; the copy's own
-- delivery carries the real progress. Every other delivery is its own seat.
CREATE VIEW delivery_seats AS
SELECT d.session_socket_id, d.worktree_id, d.delivery_id,
       COALESCE(c.state, d.state) AS progress
FROM mailbox_deliveries d
LEFT JOIN space_envoys e
  ON e.session_socket_id = d.session_socket_id AND e.worktree_id = d.worktree_id
 AND e.address_id = d.address_id
LEFT JOIN bridged_messages b
  ON b.session_socket_id = d.session_socket_id AND b.worktree_id = e.origin_space_id
 AND b.origin_space_id = d.worktree_id AND b.origin_message_id = d.message_id
LEFT JOIN mailbox_deliveries c
  ON c.session_socket_id = b.session_socket_id AND c.worktree_id = b.worktree_id
 AND c.message_id = b.message_id AND c.address_id = e.origin_address_id;
"#;

const ENVOY_ROLE_ID: &str = "role-envoy";

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct EnvoyOrigin {
    pub space: SpaceId,
    pub address_id: PaneAddressId,
    pub alias: String,
    pub origin_label: WorkspaceLabel,
    pub host_label: WorkspaceLabel,
}

pub(crate) struct SpaceNames {
    pub space: SpaceId,
    pub activated: bool,
    pub names: std::collections::BTreeSet<String>,
    pub retired: std::collections::BTreeSet<String>,
}

pub(super) struct BridgeOrigin<'a> {
    space: &'a SpaceId,
    message_id: &'a MessageId,
    label: &'a WorkspaceLabel,
    holder_label: &'a WorkspaceLabel,
}

/// What one forwarded copy did, read back from the ledger, so a fresh receipt
/// and a replayed one agree.
#[derive(Clone, Debug, Serialize)]
pub(crate) struct ForwardedCopy {
    pub workspace: WorkspaceLabel,
    #[serde(skip)]
    pub space: SpaceId,
    pub message_id: MessageId,
    pub thread_id: ThreadId,
    pub deliveries: Vec<ForwardedDelivery>,
}

#[derive(Clone, Debug, Serialize)]
pub(crate) struct ForwardedDelivery {
    pub alias: String,
    pub delivery_id: crate::DeliveryId,
}

pub(super) fn bridged_id(space: &SpaceId, id: &str) -> String {
    format!("bridge:{}:{id}", space.as_str())
}

impl StateTransaction<'_> {
    pub(crate) fn envoy_for(
        &mut self,
        host: &StateNamespace,
        origin: &EnvoyOrigin,
        occurred_at_ms: u64,
    ) -> Result<RoutingAddressSnapshot, StoreError> {
        let origin_namespace =
            StateNamespace::new(host.session_socket_id.clone(), origin.space.clone());
        if self
            .envoy_origin(&origin_namespace, &origin.address_id)?
            .is_some()
        {
            return Err(StoreError::invalid_input(
                "an envoy cannot stand for another envoy",
            ));
        }
        let address_id = PaneAddressId::new(format!(
            "envoy:{}:{}",
            origin.space.as_str(),
            origin.address_id.as_str()
        ));
        let params = params![host.session_socket_id, host.space_id.as_str()];
        self.transaction.execute(
            "INSERT OR IGNORE INTO pane_roles (session_socket_id, worktree_id, role_id, name, description)
             VALUES (?1, ?2, 'role-envoy', 'envoy',
                     'Stands in for a named agent in another Herdr workspace.')",
            params,
        )?;
        let created = self.transaction.execute(
            "INSERT OR IGNORE INTO pane_addresses (
                session_socket_id, worktree_id, address_id, role_id, retired,
                collaboration_disabled, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, 0, 0, ?5)",
            params![
                host.session_socket_id,
                host.space_id.as_str(),
                address_id.as_str(),
                ENVOY_ROLE_ID,
                to_i64(occurred_at_ms)?
            ],
        )? == 1;
        if created {
            // Never active, so no pane lookup sees it. It exists because a
            // reflected Delivery Attempt must name a binding.
            self.transaction.execute(
                "INSERT INTO pane_bindings (
                    session_socket_id, worktree_id, binding_id, address_id, pane_id,
                    active, bound_at_ms, unbound_at_ms
                 ) VALUES (?1, ?2, ?3, ?3, '', 0, ?4, ?4)",
                params![
                    host.session_socket_id,
                    host.space_id.as_str(),
                    address_id.as_str(),
                    to_i64(occurred_at_ms)?
                ],
            )?;
        }
        self.transaction.execute(
            "INSERT INTO space_envoys (
                session_socket_id, worktree_id, address_id, origin_space_id,
                origin_address_id, origin_alias, origin_label, host_label
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (session_socket_id, worktree_id, address_id) DO UPDATE SET
                origin_alias = excluded.origin_alias,
                origin_label = excluded.origin_label,
                host_label = excluded.host_label",
            params![
                host.session_socket_id,
                host.space_id.as_str(),
                address_id.as_str(),
                origin.space.as_str(),
                origin.address_id.as_str(),
                origin.alias,
                origin.origin_label.as_str(),
                origin.host_label.as_str()
            ],
        )?;
        Ok(RoutingAddressSnapshot {
            address_id,
            alias: origin.alias.clone(),
            pane_binding_id: None,
            agent_session_id: None,
        })
    }

    pub(crate) fn envoy_origin(
        &self,
        host: &StateNamespace,
        address_id: &PaneAddressId,
    ) -> Result<Option<EnvoyOrigin>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT origin_space_id, origin_address_id, origin_alias, origin_label, host_label
                 FROM space_envoys
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3",
                params![
                    host.session_socket_id,
                    host.space_id.as_str(),
                    address_id.as_str()
                ],
                |row| {
                    Ok(EnvoyOrigin {
                        space: SpaceId::new(row.get::<_, String>(0)?),
                        address_id: PaneAddressId::new(row.get::<_, String>(1)?),
                        alias: row.get(2)?,
                        origin_label: WorkspaceLabel::from_herdr(&row.get::<_, String>(3)?),
                        host_label: WorkspaceLabel::from_herdr(&row.get::<_, String>(4)?),
                    })
                },
            )
            .optional()?)
    }

    pub(super) fn envoy_standing_for(
        &self,
        host: &StateNamespace,
        origin_space: &SpaceId,
        origin_address: &PaneAddressId,
    ) -> Result<Option<PaneAddressId>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT address_id FROM space_envoys
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND origin_space_id = ?3 AND origin_address_id = ?4",
                params![
                    host.session_socket_id,
                    host.space_id.as_str(),
                    origin_space.as_str(),
                    origin_address.as_str()
                ],
                |row| row.get::<_, String>(0).map(PaneAddressId::new),
            )
            .optional()?)
    }

    fn stand_in(
        &mut self,
        target: &StateNamespace,
        source: &StateNamespace,
        address_id: &PaneAddressId,
        alias: &str,
        labels: (&WorkspaceLabel, &WorkspaceLabel),
        occurred_at_ms: u64,
    ) -> Result<RoutingAddressSnapshot, StoreError> {
        match self.envoy_origin(source, address_id)? {
            Some(origin) if origin.space == target.space_id => {
                self.routing_address_by_id(target, &origin.address_id)
            }
            Some(_) => Err(StoreError::invalid_input(
                "an envoy cannot stand for another envoy",
            )),
            None => {
                let (source_label, target_label) = labels;
                self.envoy_for(
                    target,
                    &EnvoyOrigin {
                        space: source.space_id.clone(),
                        address_id: address_id.clone(),
                        alias: alias.to_owned(),
                        origin_label: source_label.clone(),
                        host_label: target_label.clone(),
                    },
                    occurred_at_ms,
                )
            }
        }
    }

    /// One copy of `draft` in each space its envoy recipients stand for. This
    /// space's envoy deliveries settle because the copy now carries them.
    pub(super) fn forward_to_envoys(
        &mut self,
        namespace: &StateNamespace,
        draft: &MessageDraft,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let mut targets: Vec<(
            EnvoyOrigin,
            Vec<(&crate::MessageRecipientDraft, EnvoyOrigin)>,
        )> = Vec::new();
        for recipient in &draft.recipients {
            let Some(origin) = self.envoy_origin(namespace, &recipient.address_id)? else {
                continue;
            };
            match targets
                .iter_mut()
                .find(|(first, _)| first.space == origin.space)
            {
                Some((_, recipients)) => recipients.push((recipient, origin)),
                None => targets.push((origin.clone(), vec![(recipient, origin)])),
            }
        }
        if targets.is_empty() {
            return Ok(());
        }
        if self.pane_type(namespace, &draft.author_address_id)? == crate::PaneType::Terminal
            && draft.grammar_version != "terminal-result/v1"
        {
            return Err(StoreError::invalid_input(
                "a named terminal cannot send to other workspaces; only its command results \
                 travel home",
            ));
        }
        let mut unshared = Vec::new();
        for (envoy, recipients) in &targets {
            let target =
                StateNamespace::new(namespace.session_socket_id.clone(), envoy.space.clone());
            for (_, origin) in recipients {
                // A result is saved, never run, when its author has since become a terminal.
                if draft.grammar_version == "terminal-result/v1"
                    || self.pane_type(&target, &origin.address_id)? != crate::PaneType::Terminal
                {
                    continue;
                }
                if !super::terminal::terminal_runs(
                    &draft.grammar_version,
                    draft.in_reply_to.is_some(),
                ) {
                    return Err(StoreError::invalid_input(
                        "terminal recipients accept new shell command requests without agent \
                         context attachments",
                    ));
                }
                let grant = self.remote_command_grant(&target, &origin.address_id)?;
                if super::terminal::admit_remote_command(grant.as_deref(), None, true).is_err() {
                    unshared.push((origin.alias.clone(), envoy.origin_label.clone()));
                }
            }
        }
        if !unshared.is_empty() {
            return Err(StoreError::invalid_input(
                crate::spaces::RouteRefusal::TerminalNotShared {
                    terminals: unshared,
                }
                .to_string(),
            ));
        }
        for (envoy, recipients) in targets {
            let target = StateNamespace::new(namespace.session_socket_id.clone(), envoy.space);
            if !self
                .worktree_state(&target)?
                .is_some_and(|state| state.activated && !state.deactivated)
            {
                return Err(StoreError::invalid_input(format!(
                    "Namedrop is not activated in {}; activate it there before sending",
                    envoy.origin_label
                )));
            }
            let (source_label, target_label) = (&envoy.host_label, &envoy.origin_label);
            let author = self.stand_in(
                &target,
                namespace,
                &draft.author_address_id,
                &draft.author_alias_snapshot,
                (source_label, target_label),
                attribution.occurred_at_ms,
            )?;
            let thread_id =
                self.bridged_thread(namespace, &draft.thread_id, &target, attribution)?;
            let in_reply_to = match &draft.in_reply_to {
                Some(message_id) => self.counterpart_message(namespace, message_id, &target)?,
                None => None,
            };
            let mut copy_recipients = Vec::new();
            for (recipient, origin) in &recipients {
                let current = self.routing_address_by_id(&target, &origin.address_id)?;
                copy_recipients.push(crate::MessageRecipientDraft {
                    address_id: current.address_id,
                    alias_snapshot: current.alias,
                    agent_session_snapshot: current.agent_session_id,
                    delivery_id: crate::DeliveryId::new(bridged_id(
                        &namespace.space_id,
                        recipient.delivery_id.as_str(),
                    )),
                });
            }
            let copy_id = bridged_id(&namespace.space_id, draft.message_id.as_str());
            self.append_message_from(
                &target,
                &MessageDraft {
                    thread_id,
                    message_id: MessageId::new(copy_id.clone()),
                    in_reply_to,
                    author_address_id: author.address_id,
                    author_kind: draft.author_kind,
                    author_alias_snapshot: draft.author_alias_snapshot.clone(),
                    author_agent_session_snapshot: draft.author_agent_session_snapshot.clone(),
                    request_id: copy_id,
                    authored_source: draft.authored_source.clone(),
                    body: draft.body.clone(),
                    grammar_version: draft.grammar_version.clone(),
                    recipients: copy_recipients,
                    context_references: Vec::new(),
                },
                attribution,
                Some(&BridgeOrigin {
                    space: &namespace.space_id,
                    message_id: &draft.message_id,
                    label: source_label,
                    holder_label: target_label,
                }),
            )?;
            for (recipient, _) in recipients {
                self.transaction.execute(
                    "UPDATE mailbox_deliveries SET state = 'settled'
                     WHERE session_socket_id = ?1 AND worktree_id = ?2 AND delivery_id = ?3",
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        recipient.delivery_id.as_str()
                    ],
                )?;
                self.append_transition(
                    namespace,
                    TransitionEntityKind::MailboxDelivery,
                    recipient.delivery_id.as_str(),
                    TransitionKind::Settled,
                    attribution,
                )?;
            }
        }
        Ok(())
    }

    pub(super) fn record_bridged_message(
        &mut self,
        namespace: &StateNamespace,
        message_id: &MessageId,
        origin: &BridgeOrigin<'_>,
    ) -> Result<(), StoreError> {
        self.transaction.execute(
            "INSERT INTO bridged_messages (
                session_socket_id, worktree_id, message_id, origin_space_id,
                origin_message_id, origin_label_snapshot, label_snapshot
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                message_id.as_str(),
                origin.space.as_str(),
                origin.message_id.as_str(),
                origin.label.as_str(),
                origin.holder_label.as_str()
            ],
        )?;
        Ok(())
    }

    fn bridged_thread(
        &mut self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
        target: &StateNamespace,
        attribution: &MutationAttribution,
    ) -> Result<ThreadId, StoreError> {
        let paired = self
            .transaction
            .query_row(
                "SELECT peer_thread_id FROM bridged_threads
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND thread_id = ?3 AND peer_space_id = ?4",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    thread_id.as_str(),
                    target.space_id.as_str()
                ],
                |row| row.get::<_, String>(0).map(ThreadId::new),
            )
            .optional()?;
        if let Some(peer) = paired {
            return Ok(peer);
        }
        let title: String = self.transaction.query_row(
            "SELECT title FROM collaboration_threads
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND thread_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                thread_id.as_str()
            ],
            |row| row.get(0),
        )?;
        let peer = ThreadId::new(bridged_id(&namespace.space_id, thread_id.as_str()));
        self.create_thread(target, &peer, &title, attribution)?;
        // One conversation has one set of controls, so the new side starts
        // where the existing side stands.
        self.transaction.execute(
            "UPDATE thread_controls SET (paused, stopped, reply_allowance) = (
                SELECT paused, stopped, reply_allowance FROM thread_controls
                WHERE session_socket_id = ?1 AND worktree_id = ?2 AND thread_id = ?3
             )
             WHERE session_socket_id = ?1 AND worktree_id = ?4 AND thread_id = ?5",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                thread_id.as_str(),
                target.space_id.as_str(),
                peer.as_str()
            ],
        )?;
        for (side, thread, other, other_thread) in [
            (namespace, thread_id, target, &peer),
            (target, &peer, namespace, thread_id),
        ] {
            self.transaction.execute(
                "INSERT INTO bridged_threads (
                    session_socket_id, worktree_id, thread_id, peer_space_id, peer_thread_id
                 ) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    side.session_socket_id,
                    side.space_id.as_str(),
                    thread.as_str(),
                    other.space_id.as_str(),
                    other_thread.as_str()
                ],
            )?;
        }
        Ok(peer)
    }

    pub(super) fn paired_threads(
        &self,
        namespace: &StateNamespace,
        thread_id: &ThreadId,
    ) -> Result<Vec<(StateNamespace, ThreadId)>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT peer_space_id, peer_thread_id FROM bridged_threads
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND thread_id = ?3",
        )?;
        let start = (namespace.space_id.clone(), thread_id.clone());
        let mut seen = vec![start.clone()];
        let mut pending = vec![start];
        while let Some((space, thread)) = pending.pop() {
            let peers = statement
                .query_map(
                    params![namespace.session_socket_id, space.as_str(), thread.as_str()],
                    |row| {
                        Ok((
                            SpaceId::new(row.get::<_, String>(0)?),
                            ThreadId::new(row.get::<_, String>(1)?),
                        ))
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?;
            for peer in peers {
                if !seen.contains(&peer) {
                    seen.push(peer.clone());
                    pending.push(peer);
                }
            }
        }
        Ok(seen
            .into_iter()
            .skip(1)
            .map(|(space, thread)| {
                (
                    StateNamespace::new(namespace.session_socket_id.clone(), space),
                    thread,
                )
            })
            .collect())
    }

    fn counterpart_message(
        &self,
        namespace: &StateNamespace,
        message_id: &MessageId,
        target: &StateNamespace,
    ) -> Result<Option<MessageId>, StoreError> {
        if let Some((space, origin)) = self.copy_origin(namespace, message_id)?
            && space == target.space_id
        {
            return Ok(Some(origin));
        }
        self.copy_of(target, &namespace.space_id, message_id)
    }

    pub(super) fn copy_origin(
        &self,
        namespace: &StateNamespace,
        message_id: &MessageId,
    ) -> Result<Option<(SpaceId, MessageId)>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT origin_space_id, origin_message_id FROM bridged_messages
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    message_id.as_str()
                ],
                |row| {
                    Ok((
                        SpaceId::new(row.get::<_, String>(0)?),
                        MessageId::new(row.get::<_, String>(1)?),
                    ))
                },
            )
            .optional()?)
    }

    pub(super) fn copy_of(
        &self,
        holder: &StateNamespace,
        origin_space: &SpaceId,
        origin_message: &MessageId,
    ) -> Result<Option<MessageId>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT message_id FROM bridged_messages
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND origin_space_id = ?3 AND origin_message_id = ?4",
                params![
                    holder.session_socket_id,
                    holder.space_id.as_str(),
                    origin_space.as_str(),
                    origin_message.as_str()
                ],
                |row| row.get::<_, String>(0).map(MessageId::new),
            )
            .optional()?)
    }

    /// The attempt in the recipient's own workspace that an envoy delivery's
    /// copy received: the latest one, as reflection picks it.
    pub(crate) fn copy_attempt(
        &self,
        namespace: &StateNamespace,
        envoy: &PaneAddressId,
        message_id: &MessageId,
    ) -> Result<Option<(StateNamespace, crate::DeliveryAttemptId)>, StoreError> {
        let Some(origin) = self.envoy_origin(namespace, envoy)? else {
            return Ok(None);
        };
        let holder = StateNamespace::new(namespace.session_socket_id.clone(), origin.space);
        let Some(copy) = self.copy_of(&holder, &namespace.space_id, message_id)? else {
            return Ok(None);
        };
        let attempt = self
            .transaction
            .query_row(
                "SELECT a.attempt_id FROM mailbox_deliveries d
                 JOIN delivery_attempts a USING (session_socket_id, worktree_id, delivery_id)
                 WHERE d.session_socket_id = ?1 AND d.worktree_id = ?2
                   AND d.message_id = ?3 AND d.address_id = ?4
                 ORDER BY a.created_at_ms DESC, a.rowid DESC LIMIT 1",
                params![
                    holder.session_socket_id,
                    holder.space_id.as_str(),
                    copy.as_str(),
                    origin.address_id.as_str()
                ],
                |row| row.get::<_, String>(0).map(crate::DeliveryAttemptId::new),
            )
            .optional()?;
        Ok(attempt.map(|attempt| (holder, attempt)))
    }

    pub(crate) fn bridged_sender_label(
        &self,
        namespace: &StateNamespace,
        message_id: &MessageId,
    ) -> Result<Option<WorkspaceLabel>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT origin_label_snapshot FROM bridged_messages
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    message_id.as_str()
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(|label| WorkspaceLabel::from_herdr(&label)))
    }

    /// Named agents bound to a live pane in this session's other activated
    /// workspaces, and how many workspaces hold them. Read from durable state
    /// only, so a roster costs no Herdr command.
    pub(crate) fn other_space_agents(
        &self,
        socket: &str,
        here: &SpaceId,
    ) -> Result<(usize, Vec<String>), StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT c.worktree_id, c.alias
             FROM alias_claims c
             JOIN pane_addresses a USING (session_socket_id, worktree_id, address_id)
             JOIN worktrees w USING (session_socket_id, worktree_id)
             WHERE c.session_socket_id = ?1 AND c.worktree_id <> ?2
               AND c.claim_state = 'active' AND a.retired = 0 AND a.pane_type = 'agent'
               AND w.activated = 1 AND w.deactivated = 0
               AND EXISTS (
                    SELECT 1 FROM pane_bindings b
                    WHERE b.session_socket_id = c.session_socket_id
                      AND b.worktree_id = c.worktree_id
                      AND b.address_id = c.address_id AND b.active = 1
               )
             ORDER BY c.alias",
        )?;
        let rows = statement
            .query_map(params![socket, here.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let spaces: std::collections::BTreeSet<&str> =
            rows.iter().map(|(space, _)| space.as_str()).collect();
        let mut aliases: Vec<String> = rows.iter().map(|(_, alias)| alias.clone()).collect();
        aliases.dedup();
        Ok((spaces.len(), aliases))
    }

    pub(crate) fn forwarded_copies(
        &self,
        namespace: &StateNamespace,
        message_id: &MessageId,
    ) -> Result<Vec<ForwardedCopy>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT b.worktree_id, b.message_id, m.thread_id, b.label_snapshot
             FROM bridged_messages b
             JOIN collaboration_messages m USING (session_socket_id, worktree_id, message_id)
             WHERE b.session_socket_id = ?1 AND b.origin_space_id = ?2
               AND b.origin_message_id = ?3
             ORDER BY b.worktree_id",
        )?;
        let copies = statement
            .query_map(
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    message_id.as_str()
                ],
                |row| {
                    Ok((
                        SpaceId::new(row.get::<_, String>(0)?),
                        MessageId::new(row.get::<_, String>(1)?),
                        ThreadId::new(row.get::<_, String>(2)?),
                        WorkspaceLabel::from_herdr(&row.get::<_, String>(3)?),
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        copies
            .into_iter()
            .map(|(space, message_id, thread_id, workspace)| {
                let holder =
                    StateNamespace::new(namespace.session_socket_id.clone(), space.clone());
                let deliveries = self
                    .message_routing_snapshots(
                        &holder,
                        &message_id,
                        "message_recipients",
                        "recipient_order",
                        false,
                    )?
                    .into_iter()
                    .map(|recipient| {
                        let delivery_id = self.transaction.query_row(
                            "SELECT delivery_id FROM mailbox_deliveries
                             WHERE session_socket_id = ?1 AND worktree_id = ?2
                               AND message_id = ?3 AND address_id = ?4",
                            params![
                                holder.session_socket_id,
                                holder.space_id.as_str(),
                                message_id.as_str(),
                                recipient.address_id.as_str()
                            ],
                            |row| row.get::<_, String>(0).map(crate::DeliveryId::new),
                        )?;
                        Ok(ForwardedDelivery {
                            alias: recipient.alias,
                            delivery_id,
                        })
                    })
                    .collect::<Result<Vec<_>, StoreError>>()?;
                Ok(ForwardedCopy {
                    workspace,
                    space,
                    message_id,
                    thread_id,
                    deliveries,
                })
            })
            .collect()
    }

    /// Reflects each settled outcome of a copy's deliveries onto the envoy
    /// delivery it came from, so joins in the author's space see real results.
    /// A settled attempt is reflected when it settles, and its disposition
    /// when one arrives, so the author's space can judge an unanswered one.
    /// Idempotent: an outcome already reflected is left alone.
    pub(super) fn reflect_copy_outcomes(
        &mut self,
        namespace: &StateNamespace,
        copy_id: &MessageId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let Some((origin_space, origin_message)) = self.copy_origin(namespace, copy_id)? else {
            return Ok(());
        };
        let origin = StateNamespace::new(namespace.session_socket_id.clone(), origin_space);
        let mut statement = self.transaction.prepare(
            "SELECT d.address_id, d.state, a.attempt_id, a.agent_session_id, a.created_at_ms,
                    p.disposition_id, p.disposition_kind, p.occurred_at_ms
             FROM mailbox_deliveries d
             LEFT JOIN delivery_attempts a ON a.rowid = (
                 SELECT latest.rowid FROM delivery_attempts latest
                 WHERE latest.session_socket_id = d.session_socket_id
                   AND latest.worktree_id = d.worktree_id
                   AND latest.delivery_id = d.delivery_id
                 ORDER BY latest.created_at_ms DESC, latest.rowid DESC LIMIT 1
             )
             LEFT JOIN delivery_dispositions p USING (session_socket_id, worktree_id, attempt_id)
             WHERE d.session_socket_id = ?1 AND d.worktree_id = ?2 AND d.message_id = ?3",
        )?;
        let outcomes = statement
            .query_map(
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    copy_id.as_str()
                ],
                |row| {
                    Ok((
                        PaneAddressId::new(row.get::<_, String>(0)?),
                        parse_delivery_state(row.get(1)?)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<i64>>(4)?,
                        row.get::<_, Option<String>>(5)?,
                        row.get::<_, Option<String>>(6)?,
                        row.get::<_, Option<i64>>(7)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        let mut changed = false;
        for (address, state, attempt, session, attempt_at, disposition, kind, disposed_at) in
            outcomes
        {
            let Some(envoy) = self.envoy_standing_for(&origin, &namespace.space_id, &address)?
            else {
                continue;
            };
            let delivery_id: String = self.transaction.query_row(
                "SELECT delivery_id FROM mailbox_deliveries
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND message_id = ?3 AND address_id = ?4",
                params![
                    origin.session_socket_id,
                    origin.space_id.as_str(),
                    origin_message.as_str(),
                    envoy.as_str()
                ],
                |row| row.get(0),
            )?;
            if let (Some(attempt), Some(session)) = (&attempt, &session)
                && (disposition.is_some() || state == DeliveryState::Settled)
            {
                let reflected_attempt = bridged_id(&namespace.space_id, attempt);
                let inserted = self.transaction.execute(
                    "INSERT OR IGNORE INTO delivery_attempts (
                        session_socket_id, worktree_id, attempt_id, delivery_id, address_id,
                        binding_id, agent_session_id, created_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?5, ?6, ?7)",
                    params![
                        origin.session_socket_id,
                        origin.space_id.as_str(),
                        reflected_attempt,
                        delivery_id,
                        envoy.as_str(),
                        session,
                        attempt_at.unwrap_or_default()
                    ],
                )? == 1;
                if inserted {
                    self.transaction.execute(
                        "INSERT INTO delivery_attempt_state (
                            session_socket_id, worktree_id, attempt_id, address_id, state, active
                         ) VALUES (?1, ?2, ?3, ?4, 'settled', 0)",
                        params![
                            origin.session_socket_id,
                            origin.space_id.as_str(),
                            reflected_attempt,
                            envoy.as_str()
                        ],
                    )?;
                }
                let (Some(disposition), Some(kind)) = (disposition, kind) else {
                    continue;
                };
                // A reply's disposition id is its reply message, so the
                // reflected one names the reply's copy on this side. A reply
                // that stayed in the copy's space has no copy here, so the
                // author sees that nothing came back.
                let reply_copy = match kind.as_str() {
                    "reply" => self.copy_of(
                        &origin,
                        &namespace.space_id,
                        &MessageId::new(disposition.clone()),
                    )?,
                    _ => None,
                };
                let (reflected_disposition, kind) = match reply_copy {
                    Some(message) => (message.as_str().to_owned(), kind),
                    None if kind == "reply" => (
                        bridged_id(&namespace.space_id, &disposition),
                        crate::DeliveryDisposition::NoReply.as_str().to_owned(),
                    ),
                    None => (bridged_id(&namespace.space_id, &disposition), kind),
                };
                let disposed = self.transaction.execute(
                    "INSERT OR IGNORE INTO delivery_dispositions (
                        session_socket_id, worktree_id, disposition_id, attempt_id,
                        disposition_kind, occurred_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        origin.session_socket_id,
                        origin.space_id.as_str(),
                        reflected_disposition,
                        reflected_attempt,
                        kind,
                        disposed_at.unwrap_or_default()
                    ],
                )? == 1;
                if !disposed {
                    continue;
                }
                self.append_transition(
                    &origin,
                    TransitionEntityKind::AgentDisposition,
                    &reflected_disposition,
                    if kind == "reply" {
                        TransitionKind::CompletedReply
                    } else {
                        TransitionKind::CompletedNoReply
                    },
                    attribution,
                )?;
                self.resolve_attention_source(
                    &origin,
                    crate::AttentionSourceKind::AgentDisposition,
                    &reflected_attempt,
                    attribution,
                )?;
                changed = true;
            } else if matches!(state, DeliveryState::Failed | DeliveryState::Cancelled) {
                changed |= self.end_envoy_delivery(&origin, &delivery_id, state, attribution)?;
            }
        }
        if changed {
            self.complete_result_set_if_ready(&origin, &origin_message, attribution)?;
        }
        Ok(())
    }

    fn end_envoy_delivery(
        &mut self,
        origin: &StateNamespace,
        delivery_id: &str,
        state: DeliveryState,
        attribution: &MutationAttribution,
    ) -> Result<bool, StoreError> {
        let updated = self.transaction.execute(
            "UPDATE mailbox_deliveries SET state = ?4
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND delivery_id = ?3
               AND state = 'settled'",
            params![
                origin.session_socket_id,
                origin.space_id.as_str(),
                delivery_id,
                delivery_state_name(state)
            ],
        )? == 1;
        if updated {
            self.append_transition(
                origin,
                TransitionEntityKind::MailboxDelivery,
                delivery_id,
                delivery_transition_kind(state),
                attribution,
            )?;
        }
        Ok(updated)
    }

    /// Purging a workspace deletes the copies forwarded into it. Every
    /// delivery still waiting on one of them in the author's space is
    /// cancelled first, so no join or coordination waits on work that is gone.
    pub(super) fn cancel_purged_copies(
        &mut self,
        namespace: &StateNamespace,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT b.origin_space_id, b.origin_message_id, e.address_id, o.delivery_id
             FROM bridged_messages b
             JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, message_id)
             JOIN space_envoys e
               ON e.session_socket_id = b.session_socket_id
              AND e.worktree_id = b.origin_space_id
              AND e.origin_space_id = b.worktree_id
              AND e.origin_address_id = d.address_id
             JOIN mailbox_deliveries o
               ON o.session_socket_id = b.session_socket_id
              AND o.worktree_id = b.origin_space_id
              AND o.message_id = b.origin_message_id
              AND o.address_id = e.address_id
             WHERE b.session_socket_id = ?1 AND b.worktree_id = ?2
               AND NOT EXISTS (
                   SELECT 1 FROM delivery_attempts a JOIN delivery_dispositions p
                     USING (session_socket_id, worktree_id, attempt_id)
                   WHERE a.session_socket_id = o.session_socket_id
                     AND a.worktree_id = o.worktree_id AND a.delivery_id = o.delivery_id
               )
             ORDER BY b.rowid, d.rowid",
        )?;
        let waiting = statement
            .query_map(
                params![namespace.session_socket_id, namespace.space_id.as_str()],
                |row| {
                    Ok((
                        SpaceId::new(row.get::<_, String>(0)?),
                        MessageId::new(row.get::<_, String>(1)?),
                        row.get::<_, String>(3)?,
                    ))
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        drop(statement);
        for (space, message, delivery) in waiting {
            let origin = StateNamespace::new(namespace.session_socket_id.clone(), space);
            if self.end_envoy_delivery(&origin, &delivery, DeliveryState::Cancelled, attribution)? {
                self.complete_result_set_if_ready(&origin, &message, attribution)?;
            }
        }
        Ok(())
    }

    /// Spaces holding a queued delivery in a conversation that spans
    /// workspaces: a forwarded copy, or anything another workspace's activity
    /// queued there, such as a result set. Only a wake from that other
    /// workspace reaches it promptly.
    pub(crate) fn spaces_awaiting_dispatch(
        &self,
        socket: &str,
    ) -> Result<Vec<SpaceId>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT DISTINCT d.worktree_id FROM mailbox_deliveries d
             JOIN collaboration_messages m USING (session_socket_id, worktree_id, message_id)
             WHERE d.session_socket_id = ?1 AND d.state = 'queued'
               AND EXISTS (
                   SELECT 1 FROM bridged_threads t
                   WHERE t.session_socket_id = m.session_socket_id
                     AND t.worktree_id = m.worktree_id AND t.thread_id = m.thread_id
               )
             ORDER BY d.worktree_id",
        )?;
        statement
            .query_map(params![socket], |row| {
                row.get::<_, String>(0).map(SpaceId::new)
            })?
            .collect::<Result<Vec<_>, _>>()
            .map_err(StoreError::from)
    }

    pub(crate) fn alias_index(&self, socket: &str) -> Result<Vec<SpaceNames>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT worktree_id, activated AND NOT deactivated FROM worktrees
             WHERE session_socket_id = ?1 ORDER BY worktree_id",
        )?;
        let mut spaces = statement
            .query_map(params![socket], |row| {
                Ok(SpaceNames {
                    space: SpaceId::new(row.get::<_, String>(0)?),
                    activated: row.get(1)?,
                    names: std::collections::BTreeSet::new(),
                    retired: std::collections::BTreeSet::new(),
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut statement = self.transaction.prepare(
            "SELECT c.worktree_id, c.alias, c.claim_state = 'active' AND a.retired = 0
             FROM alias_claims c
             JOIN pane_addresses a USING (session_socket_id, worktree_id, address_id)
             WHERE c.session_socket_id = ?1
             UNION SELECT worktree_id, alias, 1 FROM mention_groups WHERE session_socket_id = ?1",
        )?;
        for name in statement.query_map(params![socket], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, bool>(2)?,
            ))
        })? {
            let (space, alias, live) = name?;
            if let Some(entry) = spaces
                .iter_mut()
                .find(|entry| entry.space.as_str() == space)
            {
                if live {
                    entry.names.insert(alias);
                } else {
                    entry.retired.insert(alias);
                }
            }
        }
        Ok(spaces)
    }

    pub(crate) fn is_envoy(
        &self,
        namespace: &StateNamespace,
        address_id: &PaneAddressId,
    ) -> Result<bool, StoreError> {
        Ok(self.envoy_origin(namespace, address_id)?.is_some())
    }
}
