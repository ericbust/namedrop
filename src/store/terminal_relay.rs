use super::*;

const GRAMMAR: &str = "terminal-relay/v1";

struct RelayAttempt {
    attempt_id: String,
    address_id: String,
    alias: String,
    message_id: String,
    thread_id: String,
    started_at_ms: u64,
}

impl StateStore {
    pub fn open_for_terminal_relay(
        state_directory: impl AsRef<Path>,
    ) -> Result<Option<Self>, StoreError> {
        let state_directory = state_directory.as_ref();
        let database_path = state_directory.join(DATABASE_FILE_NAME);
        if !database_path.is_file() {
            return Ok(None);
        }
        let connection =
            Connection::open_with_flags(&database_path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        connection.busy_timeout(Duration::from_millis(25))?;
        connection.pragma_update(None, "foreign_keys", true)?;
        check_read_only_schema(&connection)?;
        Ok(Some(Self {
            connection,
            state_directory: state_directory.to_owned(),
            read_only: false,
        }))
    }
}

impl StateTransaction<'_> {
    pub(super) fn arm_terminal_relay(
        &mut self,
        namespace: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
        message: &MessageId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let operator_source = self.transaction.query_row(
            "SELECT author_kind='human' AND author_address_id='operator:local'
             FROM collaboration_messages WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3",
            params![namespace.session_socket_id, namespace.space_id.as_str(), message.as_str()],
            |row| row.get::<_, bool>(0),
        )?;
        if operator_source
            && self.is_operator_address(namespace, &PaneAddressId::new("operator:local"))?
        {
            self.append_transition(
                namespace,
                TransitionEntityKind::TerminalRelayAttempt,
                attempt.as_str(),
                TransitionKind::Activated,
                attribution,
            )?;
        }
        Ok(())
    }

    pub fn relay_terminal_ending(
        &mut self,
        namespace: &StateNamespace,
        ending: &crate::TerminalRelayEnd,
        attribution: &MutationAttribution,
    ) -> Result<Vec<MessageId>, StoreError> {
        let ns = namespace_params(namespace);
        let previous_end: Option<i64> = self.transaction.query_row(
            "SELECT MAX(occurred_at_ms) FROM transition_facts
             WHERE session_socket_id=?1 AND worktree_id=?2
               AND entity_kind='terminal_relay_session' AND entity_id=?3
               AND transition_kind='stopped'",
            params![ns[0], ns[1], ending.agent_session_id.as_str()],
            |row| row.get(0),
        )?;
        let ended_at = to_i64(ending.ended_at_ms)?;
        if previous_end.is_some_and(|previous| previous >= ended_at) {
            return Ok(Vec::new());
        }
        let attempts = {
            let mut statement = self.transaction.prepare(
                "SELECT a.attempt_id, a.address_id, r.alias_snapshot, m.message_id,
                        m.thread_id, arm.occurred_at_ms
                 FROM delivery_attempts a
                 JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, delivery_id, address_id)
                 JOIN collaboration_messages m USING (session_socket_id, worktree_id, message_id)
                 JOIN message_recipients r ON r.session_socket_id=a.session_socket_id
                   AND r.worktree_id=a.worktree_id AND r.message_id=m.message_id AND r.address_id=a.address_id
                 JOIN delivery_attempt_state s USING (session_socket_id, worktree_id, attempt_id, address_id)
                 JOIN transition_facts arm ON arm.session_socket_id=a.session_socket_id
                   AND arm.worktree_id=a.worktree_id AND arm.entity_kind='terminal_relay_attempt'
                   AND arm.entity_id=a.attempt_id AND arm.transition_kind='activated'
                 WHERE a.session_socket_id=?1 AND a.worktree_id=?2 AND a.agent_session_id=?3
                   AND arm.occurred_at_ms > ?4 AND arm.occurred_at_ms <= ?5
                   AND s.state IN ('submitted','acknowledged','settled')
                   AND m.author_kind='human' AND m.author_address_id='operator:local'
                   AND NOT EXISTS (SELECT 1 FROM delivery_attempts newer
                       WHERE newer.session_socket_id=a.session_socket_id AND newer.worktree_id=a.worktree_id
                         AND newer.delivery_id=a.delivery_id AND newer.rowid>a.rowid)
                 ORDER BY arm.occurred_at_ms, a.rowid",
            )?;
            statement
                .query_map(
                    params![
                        ns[0],
                        ns[1],
                        ending.agent_session_id.as_str(),
                        previous_end.unwrap_or(-1),
                        ended_at
                    ],
                    |row| {
                        Ok(RelayAttempt {
                            attempt_id: row.get(0)?,
                            address_id: row.get(1)?,
                            alias: row.get(2)?,
                            message_id: row.get(3)?,
                            thread_id: row.get(4)?,
                            started_at_ms: from_i64(row.get(5)?)?,
                        })
                    },
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        let text: String = ending
            .text
            .trim()
            .chars()
            .take(crate::TERMINAL_RELAY_MAX_CHARS)
            .collect();
        let at = MutationAttribution::new(
            attribution.actor_kind,
            &attribution.actor_id,
            ending.ended_at_ms,
        );
        let mut messages = Vec::new();
        if !text.is_empty()
            && self.is_operator_address(namespace, &PaneAddressId::new("operator:local"))?
        {
            for attempt in attempts {
                let message_id = MessageId::new(format!("terminal-relay:{}", attempt.attempt_id));
                let exists = self.transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM collaboration_messages
                     WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3)",
                    params![ns[0], ns[1], message_id.as_str()],
                    |row| row.get::<_, bool>(0),
                )?;
                if exists {
                    continue;
                }
                let replied = self.transaction.query_row(
                    "SELECT EXISTS(SELECT 1 FROM collaboration_messages reply
                       WHERE reply.session_socket_id=?1 AND reply.worktree_id=?2 AND reply.thread_id=?3
                         AND reply.author_address_id=?4 AND reply.author_kind='agent_session'
                         AND reply.grammar_version <> 'terminal-relay/v1'
                         AND reply.created_at_ms >= ?5 AND reply.created_at_ms <= ?6)
                     OR EXISTS(SELECT 1 FROM delivery_dispositions
                       WHERE session_socket_id=?1 AND worktree_id=?2 AND attempt_id=?7 AND disposition_kind='reply')",
                    params![ns[0], ns[1], attempt.thread_id, attempt.address_id,
                        to_i64(attempt.started_at_ms)?, ended_at, attempt.attempt_id],
                    |row| row.get::<_, bool>(0),
                )?;
                if replied {
                    continue;
                }
                let order = self.next_order(namespace, "thread_message", &attempt.thread_id)?;
                self.transaction.execute(
                    "INSERT INTO collaboration_messages (
                        session_socket_id, worktree_id, message_id, thread_id, thread_order, in_reply_to,
                        author_address_id, author_alias_snapshot, author_kind, author_session_snapshot,
                        request_id, authored_source, body, grammar_version, created_at_ms)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'agent_session',?9,?3,?10,?10,?11,?12)",
                    params![ns[0], ns[1], message_id.as_str(), attempt.thread_id, to_i64(order)?,
                        attempt.message_id, attempt.address_id, attempt.alias,
                        ending.agent_session_id.as_str(), text, GRAMMAR, ended_at],
                )?;
                self.append_transition(
                    namespace,
                    TransitionEntityKind::CollaborationMessage,
                    message_id.as_str(),
                    TransitionKind::Created,
                    &at,
                )?;
                let attempt_id = crate::DeliveryAttemptId::new(&attempt.attempt_id);
                self.ensure_delivery_attempt_acknowledged(
                    namespace,
                    &attempt_id,
                    &ending.agent_session_id,
                    &at,
                )?;
                if !self.delivery_disposition_exists(namespace, &attempt_id)? {
                    self.record_no_reply_disposition(
                        namespace,
                        &attempt_id,
                        &format!("terminal-relay-no-reply:{}", attempt.attempt_id),
                        &ending.agent_session_id,
                        &at,
                    )?;
                }
                self.open_attention_item(
                    namespace,
                    &AttentionItemDraft {
                        id: crate::AttentionItemId::new(format!(
                            "terminal-relay:{}",
                            attempt.attempt_id
                        )),
                        source_kind: crate::AttentionSourceKind::TerminalRelay,
                        source_id: message_id.as_str().to_owned(),
                        summary: format!(
                            "@{} answered in their terminal and may need your reply.",
                            attempt.alias
                        ),
                    },
                    &at,
                )?;
                messages.push(message_id);
            }
        }
        self.append_transition(
            namespace,
            TransitionEntityKind::TerminalRelaySession,
            ending.agent_session_id.as_str(),
            TransitionKind::Stopped,
            &at,
        )?;
        Ok(messages)
    }

    pub(super) fn resolve_terminal_relay_attention(
        &mut self,
        namespace: &StateNamespace,
        thread: &ThreadId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let ids = {
            let mut statement = self.transaction.prepare(
                "SELECT i.source_id FROM attention_items i JOIN collaboration_messages m
                 ON m.session_socket_id=i.session_socket_id AND m.worktree_id=i.worktree_id
                   AND m.message_id=i.source_id
                 WHERE i.session_socket_id=?1 AND i.worktree_id=?2 AND i.source_kind='terminal_relay'
                   AND i.resolved_at_ms IS NULL AND m.thread_id=?3",
            )?;
            statement
                .query_map(
                    params![
                        namespace.session_socket_id,
                        namespace.space_id.as_str(),
                        thread.as_str()
                    ],
                    |row| row.get::<_, String>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?
        };
        for id in ids {
            self.resolve_attention_source(
                namespace,
                crate::AttentionSourceKind::TerminalRelay,
                &id,
                attribution,
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        AgentSessionId, DeliveryAttemptId, DeliveryId, MessageRecipientDraft, PaneBindingId,
        PaneRoleId, TerminalRelayEnd,
    };

    fn at(time: u64) -> MutationAttribution {
        MutationAttribution::new(ActorKind::Plugin, "test-relay", time)
    }

    fn fixture(grammar: &str, operator: bool) -> (StateStore, StateNamespace) {
        let mut connection = Connection::open_in_memory().unwrap();
        connection
            .pragma_update(None, "foreign_keys", true)
            .unwrap();
        initialize_schema(&mut connection, Path::new("unused")).unwrap();
        let mut store = StateStore {
            connection,
            state_directory: PathBuf::new(),
            read_only: false,
        };
        let ns = StateNamespace::for_space("test-socket", "test-relay");
        store
            .transaction(|tx| {
                tx.activate_worktree(&ns, &at(1))?;
                tx.ensure_operator_author(&ns, &at(1))?;
                for alias in ["agent", "peer"] {
                    tx.create_pane_address(
                        &ns,
                        &PaneAddressDraft {
                            id: PaneAddressId::new(alias),
                            role_id: PaneRoleId::new(format!("role-{alias}")),
                            role_name: alias.to_owned(),
                            role_description: "test agent".to_owned(),
                            alias: alias.to_owned(),
                        },
                        &at(1),
                    )?;
                }
                tx.bind_pane(
                    &ns,
                    &PaneBindingId::new("binding"),
                    &PaneAddressId::new("agent"),
                    "pane",
                    &at(1),
                )?;
                tx.observe_agent_session(
                    &ns,
                    "observation",
                    &PaneAddressId::new("agent"),
                    &AgentSessionId::new("session"),
                    "idle",
                    &at(1),
                )?;
                tx.create_thread(&ns, &ThreadId::new("thread"), "A question", &at(1))?;
                tx.append_message(
                    &ns,
                    &draft(
                        "request",
                        if operator { "operator:local" } else { "peer" },
                        if operator {
                            ActorKind::Human
                        } else {
                            ActorKind::AgentSession
                        },
                        grammar,
                        None,
                        "agent",
                    ),
                    &at(2),
                )?;
                tx.lease_mailbox_head(
                    &ns,
                    &PaneAddressId::new("agent"),
                    &LeaseDraft {
                        attempt_id: DeliveryAttemptId::new("attempt"),
                        binding_id: PaneBindingId::new("binding"),
                        agent_session_id: AgentSessionId::new("session"),
                        retry_of_attempt_id: None,
                        owner: "test".to_owned(),
                        acquired_at_ms: 10,
                        expires_at_ms: 1000,
                    },
                    &at(10),
                )?
                .expect("lease request");
                tx.transition_delivery_attempt(
                    &ns,
                    &DeliveryAttemptId::new("attempt"),
                    DeliveryState::Submitted,
                    &at(11),
                )
            })
            .unwrap();
        (store, ns)
    }

    fn draft(
        id: &str,
        author: &str,
        kind: ActorKind,
        grammar: &str,
        parent: Option<&str>,
        to: &str,
    ) -> MessageDraft {
        MessageDraft {
            thread_id: ThreadId::new("thread"),
            message_id: MessageId::new(id),
            in_reply_to: parent.map(MessageId::new),
            author_address_id: PaneAddressId::new(author),
            author_kind: kind,
            author_alias_snapshot: author.to_owned(),
            author_agent_session_snapshot: (kind == ActorKind::AgentSession)
                .then(|| AgentSessionId::new("session")),
            request_id: id.to_owned(),
            authored_source: format!("@{to} Please choose."),
            body: "Please choose.".to_owned(),
            grammar_version: grammar.to_owned(),
            recipients: vec![MessageRecipientDraft {
                address_id: PaneAddressId::new(to),
                alias_snapshot: to.to_owned(),
                agent_session_snapshot: None,
                delivery_id: DeliveryId::new(format!("delivery-{id}")),
            }],
            context_references: Vec::new(),
        }
    }

    fn relay(store: &mut StateStore, ns: &StateNamespace, time: u64, text: &str) -> Vec<MessageId> {
        store
            .transaction(|tx| {
                tx.relay_terminal_ending(
                    ns,
                    &TerminalRelayEnd {
                        agent_session_id: AgentSessionId::new("session"),
                        ended_at_ms: time,
                        text: text.to_owned(),
                    },
                    &at(time),
                )
            })
            .unwrap()
    }

    #[test]
    fn relays_operator_channel_and_direct_deliveries_without_recipient_mail() {
        for grammar in ["channel/v1", "mention/v1"] {
            let (mut store, ns) = fixture(grammar, true);
            assert_eq!(
                relay(&mut store, &ns, 20, "Which option?"),
                [MessageId::new("terminal-relay:attempt")]
            );
            store.transaction(|tx| {
                assert_eq!(tx.delivery_attempt_state(&ns, &DeliveryAttemptId::new("attempt"))?, DeliveryState::Settled);
                let rows = tx.dashboard_rows(&ns)?;
                let snapshot = crate::dashboard::snapshot::build(&rows, None, 20);
                let wire = serde_json::to_value(snapshot).unwrap();
                let message = wire["messages"].as_array().unwrap().iter().find(|m|m["kind"] == "terminal_relay").unwrap();
                assert_eq!(message["author"], "agent");
                assert_eq!(message["body"]["text"], "Which option?");
                assert!(message["legs"].as_array().unwrap().is_empty());
                let card = wire["attention"].as_array().unwrap().iter().find(|a|a["source_kind"] == "terminal_relay").unwrap();
                assert_eq!(card["response"]["kind"], "reply");
                assert_eq!(card["response"]["message_id"], "terminal-relay:attempt");
                assert_eq!(card["response"]["address_id"], "agent");
                assert_eq!(wire["health"]["level"], "needs_you");
                assert_eq!(tx.transaction.query_row("SELECT COUNT(*) FROM message_recipients WHERE message_id='terminal-relay:attempt'", [], |row|row.get::<_, i64>(0))?, 0);
                assert_eq!(tx.thread_control_state(&ns, &ThreadId::new("thread"))?.reply_allowance, 10);
                Ok(())
            }).unwrap();
        }
    }

    #[test]
    fn explicit_no_reply_still_relays() {
        let (mut store, ns) = fixture("mention/v1", true);
        store
            .transaction(|tx| {
                let attempt = DeliveryAttemptId::new("attempt");
                tx.ensure_delivery_attempt_acknowledged(
                    &ns,
                    &attempt,
                    &AgentSessionId::new("session"),
                    &at(12),
                )?;
                tx.record_no_reply_disposition(
                    &ns,
                    &attempt,
                    "no-reply",
                    &AgentSessionId::new("session"),
                    &at(15),
                )
            })
            .unwrap();
        assert_eq!(relay(&mut store, &ns, 20, "Which one? ").len(), 1);
    }

    #[test]
    fn real_reply_in_turn_suppresses_even_without_bound_disposition() {
        let (mut store, ns) = fixture("mention/v1", true);
        store
            .transaction(|tx| {
                tx.append_message(
                    &ns,
                    &draft(
                        "reply",
                        "agent",
                        ActorKind::AgentSession,
                        "mention/v1",
                        Some("request"),
                        "operator:local",
                    ),
                    &at(15),
                )
            })
            .unwrap();
        assert!(relay(&mut store, &ns, 20, "Which one?").is_empty());
    }

    #[test]
    fn other_threads_and_old_replies_do_not_suppress() {
        let (mut store, ns) = fixture("mention/v1", true);
        store
            .transaction(|tx| {
                tx.append_message(
                    &ns,
                    &draft(
                        "old",
                        "agent",
                        ActorKind::AgentSession,
                        "mention/v1",
                        Some("request"),
                        "operator:local",
                    ),
                    &at(5),
                )
            })
            .unwrap();
        assert_eq!(relay(&mut store, &ns, 20, "Which one?").len(), 1);
    }

    #[test]
    fn ignores_agent_and_native_human_sources_and_other_sessions() {
        let (mut store, ns) = fixture("mention/v1", false);
        assert!(relay(&mut store, &ns, 20, "Which one?").is_empty());
        let (mut store, ns) = fixture("mention/v1", true);
        store
            .connection
            .execute_batch("DROP TRIGGER immutable_messages_update")
            .unwrap();
        store.connection.execute("UPDATE collaboration_messages SET author_address_id='peer' WHERE message_id='request'", []).unwrap();
        assert!(relay(&mut store, &ns, 20, "Which one?").is_empty());
        let (mut store, ns) = fixture("mention/v1", true);
        let results = store
            .transaction(|tx| {
                tx.relay_terminal_ending(
                    &ns,
                    &TerminalRelayEnd {
                        agent_session_id: AgentSessionId::new("other-session"),
                        ended_at_ms: 20,
                        text: "Other answer".to_owned(),
                    },
                    &at(20),
                )
            })
            .unwrap();
        assert!(results.is_empty());
        assert_eq!(relay(&mut store, &ns, 20, "Which one?").len(), 1);
    }

    #[test]
    fn stop_watermark_and_attempt_identity_prevent_duplicates_and_stale_relay() {
        let (mut store, ns) = fixture("mention/v1", true);
        assert_eq!(relay(&mut store, &ns, 20, "Which one?").len(), 1);
        assert!(relay(&mut store, &ns, 20, "Different extraction").is_empty());
        assert!(relay(&mut store, &ns, 21, "Different extraction").is_empty());
        assert!(relay(&mut store, &ns, 19, "Stale stop").is_empty());
        let (mut store, ns) = fixture("mention/v1", true);
        assert!(relay(&mut store, &ns, 9, "Before delivery").is_empty());
        assert_eq!(relay(&mut store, &ns, 20, "Actual delivered turn").len(), 1);
        let (mut store, ns) = fixture("mention/v1", true);
        assert!(relay(&mut store, &ns, 20, " ").is_empty());
        assert!(relay(&mut store, &ns, 30, "Unrelated later turn").is_empty());
    }

    #[test]
    fn text_is_bounded_with_utf8_intact_and_snapshot_keeps_expandable_body() {
        let (mut store, ns) = fixture("mention/v1", true);
        assert_eq!(
            relay(
                &mut store,
                &ns,
                20,
                &"界".repeat(crate::TERMINAL_RELAY_MAX_CHARS + 5)
            )
            .len(),
            1
        );
        let rows = store.transaction(|tx| tx.dashboard_rows(&ns)).unwrap();
        let snapshot = crate::dashboard::snapshot::build(&rows, None, 20);
        let message = snapshot
            .messages
            .iter()
            .find(|m| m.message_id == "terminal-relay:attempt")
            .unwrap();
        assert_eq!(
            message.body.text.chars().count(),
            crate::TERMINAL_RELAY_MAX_CHARS
        );
    }

    #[test]
    fn human_reply_clears_attention_and_can_be_dispatched_to_relay_author() {
        let (mut store, ns) = fixture("mention/v1", true);
        relay(&mut store, &ns, 20, "Which one?");
        store
            .transaction(|tx| {
                let route = tx.message_route(&ns, &MessageId::new("terminal-relay:attempt"))?;
                assert!(
                    route
                        .participants
                        .iter()
                        .any(|p| p.address_id.as_str() == "agent")
                );
                tx.append_message(
                    &ns,
                    &draft(
                        "human-answer",
                        "operator:local",
                        ActorKind::Human,
                        "mention/v1",
                        Some("terminal-relay:attempt"),
                        "agent",
                    ),
                    &at(30),
                )?;
                assert!(
                    !tx.unresolved_attention_items(&ns)?
                        .iter()
                        .any(|item| item.source_kind == crate::AttentionSourceKind::TerminalRelay)
                );
                assert!(
                    tx.lease_mailbox_head(
                        &ns,
                        &PaneAddressId::new("agent"),
                        &LeaseDraft {
                            attempt_id: DeliveryAttemptId::new("followup-attempt"),
                            binding_id: PaneBindingId::new("binding"),
                            agent_session_id: AgentSessionId::new("session"),
                            retry_of_attempt_id: None,
                            owner: "test".to_owned(),
                            acquired_at_ms: 31,
                            expires_at_ms: 1000,
                        },
                        &at(31)
                    )?
                    .is_some()
                );
                tx.transition_delivery_attempt(
                    &ns,
                    &DeliveryAttemptId::new("followup-attempt"),
                    DeliveryState::Submitted,
                    &at(32),
                )
            })
            .unwrap();
        assert_eq!(relay(&mut store, &ns, 40, "Another answer?").len(), 1);
    }
    #[test]
    fn persisted_stop_and_relay_survive_hook_safe_reopen() {
        let directory = std::env::temp_dir().join(format!(
            "namedrop-terminal-relay-{}",
            crate::IdGenerator::next_id(&crate::ProcessIds::default(), "test")
        ));
        assert!(
            StateStore::open_for_terminal_relay(&directory)
                .unwrap()
                .is_none()
        );
        assert!(!directory.exists());
        fs::create_dir(&directory).unwrap();
        let (mut store, ns) = fixture("mention/v1", true);
        relay(&mut store, &ns, 20, "Which one?");
        store
            .connection
            .execute(
                "VACUUM INTO ?1",
                [directory.join(DATABASE_FILE_NAME).to_str().unwrap()],
            )
            .unwrap();
        drop(store);
        let mut reopened = StateStore::open_for_terminal_relay(&directory)
            .unwrap()
            .unwrap();
        assert_eq!(
            reopened
                .connection
                .pragma_query_value(None, "busy_timeout", |row| row.get::<_, i64>(0))
                .unwrap(),
            25
        );
        assert!(relay(&mut reopened, &ns, 20, "Different extraction").is_empty());
        assert!(relay(&mut reopened, &ns, 21, "Later unrelated answer").is_empty());
        reopened
            .transaction(|tx| {
                let rows = tx.dashboard_rows(&ns)?;
                assert_eq!(
                    rows.messages
                        .iter()
                        .filter(|m| m.grammar_version == GRAMMAR)
                        .count(),
                    1
                );
                assert_eq!(
                    rows.attention
                        .iter()
                        .filter(|item| item.item.source_kind
                            == crate::AttentionSourceKind::TerminalRelay)
                        .count(),
                    1
                );
                Ok(())
            })
            .unwrap();
        drop(reopened);
        fs::remove_dir_all(directory).unwrap();
    }
    #[test]
    fn legacy_submitted_attempt_without_activation_is_not_relayed() {
        let (mut store, ns) = fixture("mention/v1", true);
        store
            .connection
            .execute(
                "DELETE FROM transition_facts WHERE entity_kind='terminal_relay_attempt'",
                [],
            )
            .unwrap();
        assert!(relay(&mut store, &ns, 20, "Unrelated later answer").is_empty());
        let (mut store, ns) = fixture("mention/v1", true);
        assert_eq!(
            relay(&mut store, &ns, 20, "Current delivered turn?").len(),
            1
        );
    }
}
