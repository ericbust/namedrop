use super::*;

pub(crate) const GRAMMAR: &str = "direct-message/v1";

pub(crate) fn thread_id(peer: &PaneAddressId) -> ThreadId {
    ThreadId::new(format!("direct-message:{}", peer.as_str()))
}

pub(crate) fn private_thread_sql(alias: &str) -> String {
    format!(
        "EXISTS (SELECT 1 FROM collaboration_messages dm_root
        JOIN message_recipients dm_peer USING (session_socket_id, worktree_id, message_id)
        WHERE dm_root.session_socket_id = {alias}.session_socket_id
          AND dm_root.worktree_id = {alias}.worktree_id AND dm_root.thread_id = {alias}.thread_id
          AND dm_root.thread_order = 1 AND dm_root.in_reply_to IS NULL AND dm_root.grammar_version = 'direct-message/v1'
          AND dm_root.author_kind = 'human' AND dm_root.author_address_id = 'operator:local'
          AND dm_root.thread_id = 'direct-message:' || dm_peer.address_id
          AND (SELECT COUNT(*) FROM message_recipients dm_count
               WHERE dm_count.session_socket_id = dm_root.session_socket_id
                 AND dm_count.worktree_id = dm_root.worktree_id
                 AND dm_count.message_id = dm_root.message_id) = 1)"
    )
}

pub(crate) struct DirectMessage {
    pub thread_id: ThreadId,
    pub peer: PaneAddressId,
}

impl DirectMessage {
    pub fn counterpart(&self, author: &PaneAddressId) -> Result<PaneAddressId, StoreError> {
        if author.as_str() == "operator:local" {
            Ok(self.peer.clone())
        } else if author == &self.peer {
            Ok(PaneAddressId::new("operator:local"))
        } else {
            Err(StoreError::invalid_input(
                "only the operator and the addressed agent can participate in a direct message",
            ))
        }
    }
}

impl StateTransaction<'_> {
    pub(crate) fn direct_message(
        &self,
        ns: &StateNamespace,
        thread: &ThreadId,
    ) -> Result<Option<DirectMessage>, StoreError> {
        let sql = format!(
            "SELECT r.address_id FROM collaboration_messages m
            JOIN message_recipients r USING (session_socket_id, worktree_id, message_id)
            WHERE m.session_socket_id = ?1 AND m.worktree_id = ?2 AND m.thread_id = ?3
              AND m.thread_order=1 AND m.in_reply_to IS NULL AND m.grammar_version='direct-message/v1' AND {}",
            private_thread_sql("m")
        );
        Ok(self
            .transaction
            .query_row(
                &sql,
                params![ns.session_socket_id, ns.space_id.as_str(), thread.as_str()],
                |row| {
                    Ok(DirectMessage {
                        thread_id: thread.clone(),
                        peer: PaneAddressId::new(row.get::<_, String>(0)?),
                    })
                },
            )
            .optional()?)
    }

    pub(crate) fn validate_direct_message(
        &self,
        ns: &StateNamespace,
        draft: &MessageDraft,
        reminder_wake: bool,
    ) -> Result<bool, StoreError> {
        let existing = self.direct_message(ns, &draft.thread_id)?;
        let creating = draft.grammar_version == GRAMMAR && draft.in_reply_to.is_none();
        let dm = if let Some(dm) = existing {
            if draft.in_reply_to.is_none() {
                return Err(StoreError::invalid_input(
                    "a direct message already has its immutable root",
                ));
            }
            dm
        } else if creating {
            if draft.author_kind != ActorKind::Human
                || !self.is_operator_address(ns, &draft.author_address_id)?
                || draft.recipients.len() != 1
            {
                return Err(StoreError::invalid_input(
                    "a direct message root requires the operator and exactly one agent",
                ));
            }
            let peer = draft.recipients[0].address_id.clone();
            if thread_id(&peer) != draft.thread_id
                || peer == draft.author_address_id
                || self.pane_type(ns, &peer)? != crate::PaneType::Agent
                || self.envoy_origin(ns, &peer)?.is_some()
            {
                return Err(StoreError::invalid_input(
                    "a direct message must identify one agent in its home workspace",
                ));
            }
            let count: i64 = self.transaction.query_row(
                "SELECT COUNT(*) FROM collaboration_messages
                WHERE session_socket_id=?1 AND worktree_id=?2 AND thread_id=?3",
                params![
                    ns.session_socket_id,
                    ns.space_id.as_str(),
                    draft.thread_id.as_str()
                ],
                |row| row.get(0),
            )?;
            if count != 0 {
                return Err(StoreError::invalid_input(
                    "a public thread cannot become a direct message",
                ));
            }
            DirectMessage {
                thread_id: draft.thread_id.clone(),
                peer,
            }
        } else {
            if draft.grammar_version == GRAMMAR {
                return Err(StoreError::invalid_input(
                    "a direct message reply requires a canonical private root",
                ));
            }
            return Ok(false);
        };
        let counterpart = if reminder_wake
            && draft.grammar_version == "reminder/v1"
            && draft.author_address_id == dm.peer
            && draft.author_alias_snapshot == "namedrop"
        {
            dm.peer.clone()
        } else {
            dm.counterpart(&draft.author_address_id)?
        };
        if draft.recipients.len() != 1
            || draft.recipients[0].address_id != counterpart
            || !draft.context_references.is_empty()
            || (draft.author_address_id.as_str() == "operator:local"
                && draft.author_kind != ActorKind::Human)
            || (draft.author_address_id == dm.peer && draft.author_kind != ActorKind::AgentSession)
        {
            return Err(StoreError::invalid_input(
                "a direct message cannot expand its participants",
            ));
        }
        if let Some(parent) = &draft.in_reply_to {
            let same_thread: bool = self.transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM collaboration_messages
                WHERE session_socket_id=?1 AND worktree_id=?2 AND thread_id=?3 AND message_id=?4)",
                params![
                    ns.session_socket_id,
                    ns.space_id.as_str(),
                    dm.thread_id.as_str(),
                    parent.as_str()
                ],
                |row| row.get(0),
            )?;
            if !same_thread {
                return Err(StoreError::invalid_input(
                    "a direct message reply must stay in its conversation",
                ));
            }
        }
        Ok(true)
    }
}

#[derive(Serialize)]
pub(crate) struct DirectMessagePeer {
    pub space_key: crate::dashboard::snapshot::SpaceKey,
    pub address_id: String,
    pub alias: String,
    pub workspace_label: String,
    pub pane_label: Option<crate::dashboard::snapshot::SafeText>,
    pub runtime: String,
    pub presence: String,
    pub closed: bool,
    pub thread_id: Option<String>,
    pub last_activity_ms: Option<i64>,
    pub last_message_id: Option<String>,
    pub last_agent_message_id: Option<String>,
    pub attention_count: i64,
    #[serde(skip)]
    pub namespace: StateNamespace,
    #[serde(skip)]
    pub pane_id: Option<String>,
}

pub(crate) struct DirectMessageHistory {
    pub thread_id: Option<String>,
    pub history: dashboard::HistoryRows,
    pub earlier_omitted: bool,
}

impl StateStore {
    pub(crate) fn direct_message_peers(&mut self) -> Result<Vec<DirectMessagePeer>, StoreError> {
        self.transaction(|tx| tx.direct_message_peers())
    }

    pub(crate) fn direct_message_body(
        &mut self,
        ns: &StateNamespace,
        peer: &PaneAddressId,
        message_id: &str,
        max_chars: usize,
    ) -> Result<Option<dashboard::BodyPrefix>, StoreError> {
        self.transaction(|tx| {
            let thread = thread_id(peer);
            if tx.direct_message(ns, &thread)?.is_none() {
                return Ok(None);
            }
            Ok(tx
                .transaction
                .query_row(
                    "SELECT substr(body, 1, ?5), length(body) > ?5 FROM collaboration_messages
                 WHERE session_socket_id=?1 AND worktree_id=?2 AND thread_id=?3 AND message_id=?4",
                    params![
                        ns.session_socket_id,
                        ns.space_id.as_str(),
                        thread.as_str(),
                        message_id,
                        i64::try_from(max_chars).unwrap_or(i64::MAX)
                    ],
                    |row| {
                        Ok(dashboard::BodyPrefix {
                            text: row.get(0)?,
                            cut: row.get(1)?,
                        })
                    },
                )
                .optional()?)
        })
    }

    pub(crate) fn direct_message_history(
        &mut self,
        ns: &StateNamespace,
        peer: &PaneAddressId,
        before: Option<&str>,
        limit: i64,
    ) -> Result<Option<DirectMessageHistory>, StoreError> {
        self.transaction(|tx| tx.direct_message_history(ns, peer, before, limit.clamp(1, 100)))
    }
}

impl StateTransaction<'_> {
    fn direct_message_peers(&self) -> Result<Vec<DirectMessagePeer>, StoreError> {
        let sql = format!("SELECT a.session_socket_id, a.worktree_id, a.address_id,
            COALESCE((SELECT alias FROM alias_claims c WHERE c.session_socket_id=a.session_socket_id
                AND c.worktree_id=a.worktree_id AND c.address_id=a.address_id AND c.claim_state='active'),
                (SELECT alias FROM alias_history h WHERE h.session_socket_id=a.session_socket_id
                 AND h.worktree_id=a.worktree_id AND h.address_id=a.address_id AND h.transition_kind='claimed'
                 ORDER BY history_order DESC LIMIT 1), r.name),
            a.retired OR b.pane_id IS NULL OR NOT w.activated OR w.deactivated, b.pane_id,
            (SELECT runtime FROM integration_registrations i WHERE i.session_socket_id=a.session_socket_id
                AND i.worktree_id=a.worktree_id AND i.address_id=a.address_id ORDER BY i.rowid DESC LIMIT 1),
            (SELECT status FROM agent_session_observations o WHERE o.session_socket_id=a.session_socket_id
                AND o.worktree_id=a.worktree_id AND o.address_id=a.address_id AND o.observed_at_ms>=b.bound_at_ms
                ORDER BY o.observed_at_ms DESC, o.rowid DESC LIMIT 1)
            FROM pane_addresses a JOIN pane_roles r USING(session_socket_id,worktree_id,role_id)
            JOIN worktrees w USING(session_socket_id,worktree_id)
            LEFT JOIN pane_bindings b ON b.session_socket_id=a.session_socket_id AND b.worktree_id=a.worktree_id
                AND b.address_id=a.address_id AND b.active=1
            WHERE a.pane_type='agent' AND a.address_id<>'operator:local'
                AND (a.retired=0 OR EXISTS(SELECT 1 FROM collaboration_messages m
                    JOIN message_recipients r USING(session_socket_id,worktree_id,message_id)
                    WHERE m.session_socket_id=a.session_socket_id AND m.worktree_id=a.worktree_id
                      AND r.address_id=a.address_id AND {private}))
                AND NOT EXISTS(SELECT 1 FROM space_envoys e WHERE e.session_socket_id=a.session_socket_id
                    AND e.worktree_id=a.worktree_id AND e.address_id=a.address_id)
            ORDER BY a.worktree_id,a.address_id", private=private_thread_sql("m"));
        let mut statement = self.transaction.prepare(&sql)?;
        let mut peers = statement
            .query_map([], |row| {
                let namespace =
                    StateNamespace::for_space(row.get::<_, String>(0)?, row.get::<_, String>(1)?);
                let closed = row.get::<_, bool>(4)?;
                let status: Option<String> = row.get(7)?;
                Ok(DirectMessagePeer {
                    space_key: crate::dashboard::snapshot::SpaceKey::of(&namespace),
                    workspace_label: namespace.space_id.as_str().to_owned(),
                    namespace,
                    address_id: row.get(2)?,
                    alias: row.get(3)?,
                    closed,
                    pane_id: row.get(5)?,
                    pane_label: None,
                    runtime: row
                        .get::<_, Option<String>>(6)?
                        .unwrap_or_else(|| "unknown".to_owned()),
                    presence: if closed {
                        "gone"
                    } else if matches!(status.as_deref(), Some("idle" | "done")) {
                        "idle"
                    } else {
                        "working"
                    }
                    .to_owned(),
                    thread_id: None,
                    last_activity_ms: None,
                    last_message_id: None,
                    last_agent_message_id: None,
                    attention_count: 0,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        for peer in &mut peers {
            let thread = thread_id(&PaneAddressId::new(&peer.address_id));
            if self.direct_message(&peer.namespace, &thread)?.is_none() {
                continue;
            }
            peer.thread_id = Some(thread.as_str().to_owned());
            let latest: (String,i64) = self.transaction.query_row("SELECT message_id,created_at_ms
                FROM collaboration_messages WHERE session_socket_id=?1 AND worktree_id=?2 AND thread_id=?3
                ORDER BY created_at_ms DESC,rowid DESC LIMIT 1",params![peer.namespace.session_socket_id,
                peer.namespace.space_id.as_str(),thread.as_str()],|row|Ok((row.get(0)?,row.get(1)?)))?;
            peer.last_message_id = Some(latest.0);
            peer.last_activity_ms = Some(latest.1);
            peer.last_agent_message_id = self.transaction.query_row("SELECT message_id FROM collaboration_messages
                WHERE session_socket_id=?1 AND worktree_id=?2 AND thread_id=?3 AND author_kind='agent_session' AND grammar_version <> 'reminder/v1'
                ORDER BY created_at_ms DESC,rowid DESC LIMIT 1",params![peer.namespace.session_socket_id,
                peer.namespace.space_id.as_str(),thread.as_str()],|row|row.get(0)).optional()?;
            peer.attention_count=self.transaction.query_row("SELECT COUNT(*) FROM attention_items i
                LEFT JOIN delivery_attempts a ON a.session_socket_id=i.session_socket_id AND a.worktree_id=i.worktree_id
                    AND a.attempt_id=i.source_id AND i.source_kind IN ('delivery_attempt','agent_disposition')
                LEFT JOIN mailbox_deliveries d ON d.session_socket_id=a.session_socket_id AND d.worktree_id=a.worktree_id
                    AND d.delivery_id=a.delivery_id
                LEFT JOIN collaboration_messages m ON m.session_socket_id=d.session_socket_id AND m.worktree_id=d.worktree_id
                    AND m.message_id=d.message_id
                LEFT JOIN context_grants g ON g.session_socket_id=i.session_socket_id AND g.worktree_id=i.worktree_id
                    AND g.grant_id=i.source_id AND i.source_kind='context_grant'
                LEFT JOIN collaboration_messages relay ON relay.session_socket_id=i.session_socket_id
                    AND relay.worktree_id=i.worktree_id AND relay.message_id=i.source_id AND i.source_kind='terminal_relay'
                WHERE i.session_socket_id=?1 AND i.worktree_id=?2 AND i.resolved_at_ms IS NULL
                    AND (m.thread_id=?3 OR g.thread_id=?3 OR relay.thread_id=?3
                         OR (i.source_kind='automatic_reply_limit' AND i.source_id=?3))",
                params![peer.namespace.session_socket_id,peer.namespace.space_id.as_str(),thread.as_str()],|row|row.get(0))?;
        }

        peers.sort_by(|a, b| {
            b.last_activity_ms
                .cmp(&a.last_activity_ms)
                .then_with(|| a.workspace_label.cmp(&b.workspace_label))
                .then_with(|| a.address_id.cmp(&b.address_id))
        });
        Ok(peers)
    }

    fn direct_message_history(
        &self,
        ns: &StateNamespace,
        peer: &PaneAddressId,
        before: Option<&str>,
        limit: i64,
    ) -> Result<Option<DirectMessageHistory>, StoreError> {
        let exists: bool=self.transaction.query_row("SELECT EXISTS(SELECT 1 FROM pane_addresses a
            WHERE session_socket_id=?1 AND worktree_id=?2 AND address_id=?3 AND pane_type='agent'
                AND address_id<>'operator:local' AND NOT EXISTS(SELECT 1 FROM space_envoys e
                    WHERE e.session_socket_id=a.session_socket_id AND e.worktree_id=a.worktree_id AND e.address_id=a.address_id))",
            params![ns.session_socket_id,ns.space_id.as_str(),peer.as_str()],|row|row.get(0))?;
        if !exists {
            return Ok(None);
        }
        let thread = thread_id(peer);
        if self.direct_message(ns, &thread)?.is_none() {
            if before.is_some() {
                return Ok(None);
            }
            return Ok(Some(DirectMessageHistory {
                thread_id: None,
                history: dashboard::HistoryRows {
                    messages: Vec::new(),
                    legs: Vec::new(),
                },
                earlier_omitted: false,
            }));
        }
        let cursor = before
            .map(|id| {
                self.transaction
                    .query_row(
                        "SELECT thread_order FROM collaboration_messages
            WHERE session_socket_id=?1 AND worktree_id=?2 AND thread_id=?3 AND message_id=?4",
                        params![
                            ns.session_socket_id,
                            ns.space_id.as_str(),
                            thread.as_str(),
                            id
                        ],
                        |row| row.get::<_, i64>(0),
                    )
                    .optional()
            })
            .transpose()?;
        if matches!(cursor, Some(None)) {
            return Ok(None);
        }
        let mut statement=self.transaction.prepare("SELECT message_id,thread_id,in_reply_to,created_at_ms,author_address_id,author_kind,grammar_version,body
            FROM collaboration_messages WHERE session_socket_id=?1 AND worktree_id=?2 AND thread_id=?3
                AND (?4 IS NULL OR thread_order<?4) ORDER BY thread_order DESC LIMIT ?5")?;
        let mut messages = statement
            .query_map(
                params![
                    ns.session_socket_id,
                    ns.space_id.as_str(),
                    thread.as_str(),
                    cursor.flatten(),
                    limit + 1
                ],
                |row| {
                    Ok(dashboard::MessageRow {
                        channel_to: None,
                        channel_state: None,
                        channel_mentioned: None,
                        woke: Vec::new(),
                        message_id: row.get(0)?,
                        thread_id: row.get(1)?,
                        in_reply_to: row.get(2)?,
                        created_at_ms: row.get(3)?,
                        author_address_id: row.get(4)?,
                        author_is_human: row.get::<_, String>(5)? == "human",
                        grammar_version: row.get(6)?,
                        body: row.get(7)?,
                    })
                },
            )?
            .collect::<Result<Vec<_>, _>>()?;
        let earlier_omitted = messages.len() > limit as usize;
        messages.truncate(limit as usize);
        messages.reverse();
        let legs = self.dashboard_message_legs(ns, &messages)?;
        Ok(Some(DirectMessageHistory {
            thread_id: Some(thread.as_str().to_owned()),
            history: dashboard::HistoryRows { messages, legs },
            earlier_omitted,
        }))
    }

    pub(crate) fn replay_direct_message(
        &mut self,
        ns: &StateNamespace,
        peer: &PaneAddressId,
        request_id: &str,
        source: &str,
        at: &MutationAttribution,
    ) -> Result<Option<RoutedMessageReceipt>, StoreError> {
        let operator = self.ensure_operator_author(ns, at)?;
        let thread = thread_id(peer);
        let existing = self
            .transaction
            .query_row(
                "SELECT message_id FROM idempotency_records WHERE session_socket_id=?1
            AND worktree_id=?2 AND author_address_id=?3 AND request_id=?4",
                params![
                    ns.session_socket_id,
                    ns.space_id.as_str(),
                    operator.address_id.as_str(),
                    request_id
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(message) = existing {
            let route = self.message_route(ns, &MessageId::new(&message))?;
            if route.thread_id != thread
                || route.reply_mode != crate::DeliveryReplyMode::DirectMessage
            {
                return Err(StoreError::invalid_input(
                    "request id was already used in a different conversation",
                ));
            }
            let parent=self.transaction.query_row("SELECT in_reply_to FROM collaboration_messages WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3",
                params![ns.session_socket_id,ns.space_id.as_str(),message],|row|row.get::<_,Option<String>>(0))?.map(MessageId::new);
            return self
                .replay_message(
                    ns,
                    &operator.address_id,
                    request_id,
                    source,
                    parent.as_ref(),
                    ActorKind::Human,
                )?
                .map(Some)
                .ok_or_else(|| StoreError::internal("direct message replay disappeared"));
        }
        Ok(None)
    }

    pub(crate) fn send_direct_message(
        &mut self,
        ns: &StateNamespace,
        peer: &PaneAddressId,
        request_id: &str,
        source: &str,
        ids: &impl crate::IdGenerator,
        at: &MutationAttribution,
    ) -> Result<RoutedMessageReceipt, StoreError> {
        if let Some(receipt) = self.replay_direct_message(ns, peer, request_id, source, at)? {
            return Ok(receipt);
        }
        let operator = self.ensure_operator_author(ns, at)?;
        let thread = thread_id(peer);
        let peer_snapshot = self.routing_address_by_id(ns, peer)?;
        if peer_snapshot.pane_binding_id.is_none()
            || self.pane_type(ns, peer)? != crate::PaneType::Agent
            || self.envoy_origin(ns, peer)?.is_some()
            || peer == &operator.address_id
            || self
                .worktree_state(ns)?
                .is_none_or(|state| !state.activated || state.deactivated)
        {
            return Err(StoreError::invalid_input(
                "this direct message conversation is closed",
            ));
        }
        let parent = if self.direct_message(ns, &thread)?.is_some() {
            Some(self.transaction.query_row("SELECT message_id FROM collaboration_messages WHERE session_socket_id=?1 AND worktree_id=?2
                AND thread_id=?3 ORDER BY thread_order DESC LIMIT 1",params![ns.session_socket_id,ns.space_id.as_str(),thread.as_str()],|row|row.get::<_,String>(0)).map(MessageId::new)?)
        } else {
            self.create_thread(ns, &thread, "Direct message", at)?;
            None
        };
        let draft = MessageDraft {
            thread_id: thread,
            message_id: MessageId::new(ids.next_id("message")),
            in_reply_to: parent,
            author_address_id: operator.address_id,
            author_kind: ActorKind::Human,
            author_alias_snapshot: operator.alias,
            author_agent_session_snapshot: None,
            request_id: request_id.to_owned(),
            authored_source: source.to_owned(),
            body: source.to_owned(),
            grammar_version: GRAMMAR.to_owned(),
            recipients: vec![crate::MessageRecipientDraft {
                address_id: peer.clone(),
                alias_snapshot: peer_snapshot.alias.clone(),
                agent_session_snapshot: peer_snapshot.agent_session_id.clone(),
                delivery_id: crate::DeliveryId::new(ids.next_id("delivery")),
            }],
            context_references: Vec::new(),
        };
        let receipt =
            self.append_message_mode(ns, &draft, at, None, MessageAppendMode::DirectMessage)?;
        Ok(RoutedMessageReceipt {
            receipt,
            recipients: vec![peer_snapshot],
            context_references: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DeliveryId, MessageRecipientDraft, PaneBindingId, PaneRoleId};

    struct Fixture {
        path: PathBuf,
        store: StateStore,
        ns: StateNamespace,
        peer: PaneAddressId,
        at: MutationAttribution,
    }
    impl Fixture {
        fn new() -> Self {
            let path = std::env::temp_dir().join(crate::IdGenerator::next_id(
                &crate::ProcessIds::default(),
                "namedrop-dm-test",
            ));
            let mut store = StateStore::open(&path).unwrap();
            let ns = StateNamespace::for_space("dm-test-socket", "one");
            let at = MutationAttribution::new(ActorKind::Human, "test", 1);
            let peer = PaneAddressId::new("peer");
            store
                .transaction(|tx| {
                    tx.activate_worktree(&ns, &at)?;
                    add_peer(tx, &ns, &peer, "claude", &at)
                })
                .unwrap();
            Self {
                path,
                store,
                ns,
                peer,
                at,
            }
        }
        fn send(&mut self, request: &str, source: &str) -> RoutedMessageReceipt {
            self.store
                .transaction(|tx| {
                    tx.send_direct_message(
                        &self.ns,
                        &self.peer,
                        request,
                        source,
                        &crate::ProcessIds::default(),
                        &self.at,
                    )
                })
                .unwrap()
        }
        fn reply(&mut self, parent: &MessageId, id: &str) -> MessageReceipt {
            let draft = MessageDraft {
                thread_id: thread_id(&self.peer),
                message_id: MessageId::new(id),
                in_reply_to: Some(parent.clone()),
                author_address_id: self.peer.clone(),
                author_kind: ActorKind::AgentSession,
                author_alias_snapshot: "claude".to_owned(),
                author_agent_session_snapshot: None,
                request_id: id.to_owned(),
                authored_source: "@operator Private response mentioning @outsider".to_owned(),
                body: "Private response mentioning @outsider".to_owned(),
                grammar_version: "mention/v1".to_owned(),
                recipients: vec![MessageRecipientDraft {
                    address_id: PaneAddressId::new("operator:local"),
                    alias_snapshot: "operator".to_owned(),
                    agent_session_snapshot: None,
                    delivery_id: DeliveryId::new(format!("delivery-{id}")),
                }],
                context_references: Vec::new(),
            };
            self.store
                .transaction(|tx| tx.append_message(&self.ns, &draft, &self.at))
                .unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn add_peer(
        tx: &mut StateTransaction<'_>,
        ns: &StateNamespace,
        id: &PaneAddressId,
        alias: &str,
        at: &MutationAttribution,
    ) -> Result<(), StoreError> {
        tx.create_pane_address(
            ns,
            &PaneAddressDraft {
                id: id.clone(),
                role_id: PaneRoleId::new(id.as_str()),
                role_name: alias.to_owned(),
                role_description: alias.to_owned(),
                alias: alias.to_owned(),
            },
            at,
        )?;
        tx.bind_pane(
            ns,
            &PaneBindingId::new(format!("binding-{}", id.as_str())),
            id,
            &format!("pane-{}", id.as_str()),
            at,
        )
    }

    #[test]
    fn direct_messages_are_private_before_channel_pagination_and_keep_agent_replies() {
        let mut f = Fixture::new();
        let public = MessageDraft {
            thread_id: ThreadId::new("public"),
            message_id: MessageId::new("public-message"),
            in_reply_to: None,
            author_address_id: PaneAddressId::new("operator:local"),
            author_kind: ActorKind::Human,
            author_alias_snapshot: "operator".to_owned(),
            author_agent_session_snapshot: None,
            request_id: "public".to_owned(),
            authored_source: "Public conversation".to_owned(),
            body: "Public conversation".to_owned(),
            grammar_version: "channel/v1".to_owned(),
            recipients: Vec::new(),
            context_references: Vec::new(),
        };
        f.store
            .transaction(|tx| {
                tx.ensure_operator_author(&f.ns, &f.at)?;
                tx.create_thread(&f.ns, &public.thread_id, "public", &f.at)?;
                tx.append_message(&f.ns, &public, &f.at)
            })
            .unwrap();
        let root = f.send("dm-root", "Private question @outsider");
        let reply = f.reply(&root.receipt.message_id, "private-reply");
        for index in 0..155 {
            f.send(&format!("private-{index}"), "Private follow-up");
        }
        let rows = f.store.dashboard_rows(&f.ns).unwrap();
        assert_eq!(rows.messages.len(), 1);
        assert_eq!(rows.messages[0].message_id, "public-message");
        assert_eq!(rows.threads.len(), 1);
        assert!(rows.mailbox.is_empty());
        let page = f.store.dashboard_history(&f.ns, None, 1).unwrap().unwrap();
        assert_eq!(page.messages[0].message_id, "public-message");
        assert!(
            f.store
                .dashboard_history(&f.ns, Some(reply.message_id.as_str()), 1)
                .unwrap()
                .is_none()
        );
        assert!(
            f.store
                .dashboard_message_body(&f.ns, reply.message_id.as_str(), 500)
                .unwrap()
                .is_none()
        );
        assert!(
            f.store
                .dashboard_thread_context(&f.ns, reply.message_id.as_str())
                .unwrap()
                .is_none()
        );
        let body = f
            .store
            .direct_message_body(&f.ns, &f.peer, reply.message_id.as_str(), 7)
            .unwrap()
            .unwrap();
        assert_eq!(body.text, "Private");
        assert!(body.cut);
        let full = f
            .store
            .direct_message_body(&f.ns, &f.peer, root.receipt.message_id.as_str(), 500)
            .unwrap()
            .unwrap();
        assert_eq!(full.text, "Private question @outsider");
        assert!(!full.cut);
        f.store
            .transaction(|tx| {
                let outsider = PaneAddressId::new("outsider");
                add_peer(tx, &f.ns, &outsider, "other", &f.at)?;
                tx.send_direct_message(
                    &f.ns,
                    &outsider,
                    "another-dm",
                    "Another private conversation",
                    &crate::ProcessIds::default(),
                    &f.at,
                )
            })
            .unwrap();
        for (ns, peer, message) in [
            (f.ns.clone(), f.peer.clone(), "public-message"),
            (
                f.ns.clone(),
                PaneAddressId::new("outsider"),
                reply.message_id.as_str(),
            ),
            (
                StateNamespace::for_space("dm-test-socket", "other"),
                f.peer.clone(),
                reply.message_id.as_str(),
            ),
        ] {
            assert!(
                f.store
                    .direct_message_body(&ns, &peer, message, 500)
                    .unwrap()
                    .is_none()
            );
        }
        let history = f
            .store
            .direct_message_history(&f.ns, &f.peer, None, 100)
            .unwrap()
            .unwrap();
        assert_eq!(history.history.messages.len(), 100);
        assert!(history.earlier_omitted);
        let first = history.history.messages.first().unwrap().message_id.clone();
        let older = f
            .store
            .direct_message_history(&f.ns, &f.peer, Some(&first), 100)
            .unwrap()
            .unwrap();
        assert!(
            older
                .history
                .messages
                .iter()
                .any(|message| message.message_id == reply.message_id.as_str())
        );
        assert!(
            f.store
                .transaction(|tx| tx.message_route(&f.ns, &reply.message_id))
                .unwrap()
                .reply_mode
                == crate::DeliveryReplyMode::DirectMessage
        );
    }

    #[test]
    fn direct_messages_never_spend_or_require_automatic_reply_allowance() {
        let mut f = Fixture::new();
        let root = f.send("question", "Question");
        for index in 0..15 {
            f.reply(&root.receipt.message_id, &format!("reply-{index}"));
        }
        let controls = f
            .store
            .transaction(|tx| tx.thread_control_state(&f.ns, &root.receipt.thread_id))
            .unwrap();
        assert_eq!(controls.reply_allowance, 10);
        assert!(!controls.paused);
        f.store
            .transaction(|tx| {
                tx.transaction.execute(
                    "UPDATE thread_controls SET paused=1,reply_allowance=0 WHERE thread_id=?1",
                    [root.receipt.thread_id.as_str()],
                )?;
                Ok(())
            })
            .unwrap();
        f.reply(&root.receipt.message_id, "reply-with-zero-allowance");
        f.send("operator-with-zero-allowance", "Another question");
        let rows = f.store.dashboard_rows(&f.ns).unwrap();
        assert!(rows.attention.is_empty());
    }

    #[test]
    fn direct_message_identity_survives_alias_collision_and_closed_workspace() {
        let mut f = Fixture::new();
        let first = f.send("same-request", "First workspace");
        let other = StateNamespace::for_space("dm-test-socket", "two");
        let second = f
            .store
            .transaction(|tx| {
                tx.activate_worktree(&other, &f.at)?;
                add_peer(tx, &other, &f.peer, "claude", &f.at)?;
                tx.send_direct_message(
                    &other,
                    &f.peer,
                    "same-request",
                    "Second workspace",
                    &crate::ProcessIds::default(),
                    &f.at,
                )
            })
            .unwrap();
        assert_ne!(first.receipt.message_id, second.receipt.message_id);
        let peers = f.store.direct_message_peers().unwrap();
        assert_eq!(peers.len(), 2);
        assert_ne!(peers[0].space_key, peers[1].space_key);
        assert_eq!(peers[0].alias, peers[1].alias);
        f.store
            .transaction(|tx| {
                tx.transaction.execute("UPDATE mailbox_deliveries SET state='settled' WHERE session_socket_id=?1 AND worktree_id=?2",params![f.ns.session_socket_id,f.ns.space_id.as_str()])?;
                tx.retire_pane_address(&f.ns, &f.peer, &f.at)?;
                tx.deactivate_worktree(&f.ns, &f.at)?;
                Ok(())
            })
            .unwrap();
        let peers = f.store.direct_message_peers().unwrap();
        assert_eq!(peers.len(), 2);
        assert!(peers.iter().any(|peer| peer.closed
            && peer.last_message_id.as_deref() == Some(first.receipt.message_id.as_str())));
        assert_eq!(
            f.store
                .direct_message_history(&f.ns, &f.peer, None, 100)
                .unwrap()
                .unwrap()
                .history
                .messages[0]
                .body,
            "First workspace"
        );
        let replay = f.send("same-request", "First workspace");
        assert!(replay.receipt.idempotent_replay);
        assert!(
            f.store
                .transaction(|tx| tx.send_direct_message(
                    &f.ns,
                    &f.peer,
                    "new-request",
                    "Closed",
                    &crate::ProcessIds::default(),
                    &f.at
                ))
                .is_err()
        );
    }

    #[test]
    fn terminal_ending_stays_in_direct_message_with_private_attention_and_unread_metadata() {
        let mut f = Fixture::new();
        let root = f.send("dm-terminal-request", "Which option should we use?");
        let session = crate::AgentSessionId::new("session-peer");
        let attempt = crate::DeliveryAttemptId::new("dm-terminal-attempt");
        let ending = crate::TerminalRelayEnd {
            agent_session_id: session.clone(),
            ended_at_ms: 20,
            previous_end_ms: None,
            text: "Do you prefer option A or option B?".to_owned(),
        };
        let relay = f
            .store
            .transaction(|tx| {
                tx.observe_agent_session(&f.ns, "observed-peer", &f.peer, &session, "idle", &f.at)?;
                let leased = tx.lease_mailbox_head(
                    &f.ns,
                    &f.peer,
                    &crate::LeaseDraft {
                        attempt_id: attempt.clone(),
                        binding_id: PaneBindingId::new("binding-peer"),
                        agent_session_id: session.clone(),
                        retry_of_attempt_id: None,
                        owner: "test".to_owned(),
                        acquired_at_ms: 2,
                        expires_at_ms: 1000,
                    },
                    &MutationAttribution::new(ActorKind::Plugin, "test", 2),
                )?;
                assert!(leased.is_some());
                tx.transition_delivery_attempt(
                    &f.ns,
                    &attempt,
                    crate::DeliveryState::Submitted,
                    &MutationAttribution::new(ActorKind::Plugin, "test", 3),
                )?;
                let relayed = tx.relay_terminal_ending(
                    &f.ns,
                    &ending,
                    &MutationAttribution::new(ActorKind::Plugin, "test", 20),
                )?;
                assert_eq!(relayed.len(), 1);
                assert_eq!(
                    tx.delivery_attempt_state(&f.ns, &attempt)?,
                    crate::DeliveryState::Settled
                );
                Ok(relayed[0].clone())
            })
            .unwrap();
        let history = f
            .store
            .direct_message_history(&f.ns, &f.peer, None, 100)
            .unwrap()
            .unwrap();
        assert_eq!(history.history.messages.len(), 2);
        let message = &history.history.messages[1];
        assert_eq!(message.message_id, relay.as_str());
        assert_eq!(message.thread_id, root.receipt.thread_id.as_str());
        assert_eq!(
            message.in_reply_to.as_deref(),
            Some(root.receipt.message_id.as_str())
        );
        assert_eq!(message.author_address_id, f.peer.as_str());
        assert_eq!(message.grammar_version, "terminal-relay/v1");
        assert_eq!(message.body, ending.text);
        assert!(
            history
                .history
                .legs
                .iter()
                .all(|leg| leg.message_id != relay.as_str())
        );
        let peers = f.store.direct_message_peers().unwrap();
        assert_eq!(peers.len(), 1);
        let peer = &peers[0];
        assert_eq!(
            peer.thread_id.as_deref(),
            Some(root.receipt.thread_id.as_str())
        );
        assert_eq!(peer.last_message_id.as_deref(), Some(relay.as_str()));
        assert_eq!(peer.last_agent_message_id.as_deref(), Some(relay.as_str()));
        assert_eq!(
            peer.attention_count, 1,
            "DM terminal reply must need the operator"
        );
        let rows = f.store.dashboard_rows(&f.ns).unwrap();
        assert!(
            rows.messages.is_empty(),
            "DM reply leaked into channel timeline"
        );
        assert!(
            rows.threads.is_empty(),
            "DM reply leaked into channel threads"
        );
        assert!(
            rows.attention.is_empty(),
            "DM attention leaked into All activity"
        );
        assert!(rows.mailbox.is_empty());
        for (label, absent) in [
            (
                "body",
                f.store
                    .dashboard_message_body(&f.ns, relay.as_str(), 500)
                    .unwrap()
                    .is_none(),
            ),
            (
                "thread",
                f.store
                    .dashboard_thread_context(&f.ns, relay.as_str())
                    .unwrap()
                    .is_none(),
            ),
            (
                "history",
                f.store
                    .dashboard_history(&f.ns, Some(relay.as_str()), 100)
                    .unwrap()
                    .is_none(),
            ),
        ] {
            assert!(absent, "private terminal reply appeared in public {label}");
        }
        f.store
            .transaction(|tx| {
                assert!(tx.relay_terminal_ending(&f.ns, &ending, &f.at)?.is_empty());
                Ok(())
            })
            .unwrap();
        f.at = MutationAttribution::new(ActorKind::Human, "test", 30);
        f.send("dm-follow-up", "Option A");
        let peers = f.store.direct_message_peers().unwrap();
        assert_eq!(peers[0].attention_count, 0);
    }

    #[test]
    fn acknowledged_direct_message_permission_status_is_public_without_private_context() {
        use crate::dashboard::snapshot::{
            Activity, AttentionResponse, AttentionSourceKind, SpaceHealth, build,
        };

        let mut f = Fixture::new();
        let private_body = "Private request requiring Bash permission";
        let root = f.send("dm-permission", private_body);
        let session = crate::AgentSessionId::new("session-peer");
        let attempt = crate::DeliveryAttemptId::new("dm-permission-attempt");
        f.store
            .transaction(|tx| {
                tx.observe_agent_session(&f.ns, "initial-peer", &f.peer, &session, "idle", &f.at)?;
                let leased = tx.lease_mailbox_head(
                    &f.ns,
                    &f.peer,
                    &crate::LeaseDraft {
                        attempt_id: attempt.clone(),
                        binding_id: PaneBindingId::new("binding-peer"),
                        agent_session_id: session.clone(),
                        retry_of_attempt_id: None,
                        owner: "test".to_owned(),
                        acquired_at_ms: 2,
                        expires_at_ms: 1000,
                    },
                    &MutationAttribution::new(ActorKind::Plugin, "test", 2),
                )?;
                assert!(leased.is_some());
                tx.transition_delivery_attempt(
                    &f.ns,
                    &attempt,
                    crate::DeliveryState::Submitted,
                    &MutationAttribution::new(ActorKind::Plugin, "test", 3),
                )?;
                tx.ensure_delivery_attempt_acknowledged(
                    &f.ns,
                    &attempt,
                    &session,
                    &MutationAttribution::new(ActorKind::Plugin, "test", 4),
                )?;
                assert_eq!(
                    tx.delivery_attempt_state(&f.ns, &attempt)?,
                    crate::DeliveryState::Acknowledged
                );
                Ok(())
            })
            .unwrap();

        for (status, at_ms, activity, health) in [
            (
                "blocked",
                10,
                Activity::Blocked,
                SpaceHealth::NeedsYou { items: 1 },
            ),
            ("idle", 20, Activity::Idle, SpaceHealth::Calm),
        ] {
            f.store
                .transaction(|tx| {
                    tx.observe_agent_session(
                        &f.ns,
                        &format!("peer-{status}"),
                        &f.peer,
                        &session,
                        status,
                        &MutationAttribution::new(ActorKind::Plugin, "test", at_ms),
                    )
                })
                .unwrap();
            let rows = f.store.dashboard_rows(&f.ns).unwrap();
            assert!(rows.messages.is_empty());
            assert!(rows.threads.is_empty());
            assert!(rows.mailbox.is_empty());
            let snapshot = build(&rows, None, at_ms as i64);
            let wire = serde_json::to_string(&snapshot).unwrap();
            for private in [
                private_body,
                root.receipt.message_id.as_str(),
                root.receipt.thread_id.as_str(),
            ] {
                assert!(
                    !wire.contains(private),
                    "private DM context leaked: {private}"
                );
            }
            let member = snapshot
                .participants
                .iter()
                .find(|member| member.address_id == f.peer.as_str())
                .unwrap();
            assert!(member.mailbox.is_empty());
            assert_eq!(member.activity, activity, "DM peer observed as {status}");
            assert_eq!(snapshot.health, health);
            if status == "blocked" {
                assert_eq!(snapshot.attention.len(), 1);
                let attention = &snapshot.attention[0];
                assert_eq!(attention.source_kind, AttentionSourceKind::AgentStatus);
                assert_eq!(attention.opened_at_ms, 10);
                assert!(matches!(
                    &attention.response,
                    AttentionResponse::Pane { address_id } if address_id == f.peer.as_str()
                ));
                assert!(attention.message_id.is_none());
                assert!(attention.thread_id.is_none());
                assert!(attention.conversation.is_none());
            } else {
                assert!(snapshot.attention.is_empty());
            }
        }
    }

    #[test]
    fn direct_message_delivery_attention_belongs_to_the_private_peer() {
        let mut f = Fixture::new();
        f.send("dm-attention", "Private request");
        f.store
            .transaction(|tx| {
                let session = crate::AgentSessionId::new("session-peer");
                tx.observe_agent_session(&f.ns, "observed-peer", &f.peer, &session, "idle", &f.at)?;
                let attempt = crate::DeliveryAttemptId::new("dm-attempt");
                let leased = tx.lease_mailbox_head(
                    &f.ns,
                    &f.peer,
                    &crate::LeaseDraft {
                        attempt_id: attempt.clone(),
                        binding_id: PaneBindingId::new("binding-peer"),
                        agent_session_id: session,
                        retry_of_attempt_id: None,
                        owner: "test".to_owned(),
                        acquired_at_ms: 2,
                        expires_at_ms: 1000,
                    },
                    &f.at,
                )?;
                assert!(leased.is_some());
                tx.open_attention_item(
                    &f.ns,
                    &crate::AttentionItemDraft {
                        id: crate::AttentionItemId::new("private-attention"),
                        source_kind: crate::AttentionSourceKind::AgentDisposition,
                        source_id: attempt.as_str().to_owned(),
                        summary: "Private request needs a response".to_owned(),
                    },
                    &f.at,
                )?;
                tx.open_attention_item(
                    &f.ns,
                    &crate::AttentionItemDraft {
                        id: crate::AttentionItemId::new("private-automatic-attention"),
                        source_kind: crate::AttentionSourceKind::AutomaticReplyLimit,
                        source_id: thread_id(&f.peer).as_str().to_owned(),
                        summary: "Private thread attention".to_owned(),
                    },
                    &f.at,
                )
            })
            .unwrap();
        let rows = f.store.dashboard_rows(&f.ns).unwrap();
        assert!(rows.attention.is_empty());
        assert!(rows.mailbox.is_empty());
        let peers = f.store.direct_message_peers().unwrap();
        assert_eq!(peers[0].attention_count, 2);
    }

    #[test]
    fn direct_message_append_rejects_outsiders_expansion_and_forged_roots() {
        let mut f = Fixture::new();
        let root = f.send("question", "Question");
        f.store
            .transaction(|tx| {
                add_peer(
                    tx,
                    &f.ns,
                    &PaneAddressId::new("outsider"),
                    "outsider",
                    &f.at,
                )
            })
            .unwrap();
        let base = MessageDraft {
            thread_id: root.receipt.thread_id.clone(),
            message_id: MessageId::new("invalid"),
            in_reply_to: Some(root.receipt.message_id.clone()),
            author_address_id: PaneAddressId::new("outsider"),
            author_kind: ActorKind::AgentSession,
            author_alias_snapshot: "outsider".to_owned(),
            author_agent_session_snapshot: None,
            request_id: "invalid".to_owned(),
            authored_source: "@operator Leaked".to_owned(),
            body: "Leaked".to_owned(),
            grammar_version: "mention/v1".to_owned(),
            recipients: vec![MessageRecipientDraft {
                address_id: PaneAddressId::new("operator:local"),
                alias_snapshot: "operator".to_owned(),
                agent_session_snapshot: None,
                delivery_id: DeliveryId::new("invalid"),
            }],
            context_references: Vec::new(),
        };
        assert!(
            f.store
                .transaction(|tx| tx.append_message(&f.ns, &base, &f.at))
                .is_err()
        );
        let mut expanded = base.clone();
        expanded.author_address_id = f.peer.clone();
        expanded.recipients.push(MessageRecipientDraft {
            address_id: PaneAddressId::new("outsider"),
            alias_snapshot: "outsider".to_owned(),
            agent_session_snapshot: None,
            delivery_id: DeliveryId::new("extra"),
        });
        assert!(
            f.store
                .transaction(|tx| tx.append_message(&f.ns, &expanded, &f.at))
                .is_err()
        );
        let mut forged = base;
        forged.grammar_version = GRAMMAR.to_owned();
        forged.in_reply_to = None;
        forged.author_address_id = PaneAddressId::new("operator:local");
        forged.author_kind = ActorKind::Human;
        assert!(
            f.store
                .transaction(|tx| tx.append_message(&f.ns, &forged, &f.at))
                .unwrap_err()
                .to_string()
                .contains("private send operation")
        );
        assert_eq!(
            f.store
                .direct_message_history(&f.ns, &f.peer, None, 100)
                .unwrap()
                .unwrap()
                .history
                .messages
                .len(),
            1
        );
    }
}
