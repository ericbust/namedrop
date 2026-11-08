use super::*;

const GRAMMAR: &str = "terminal-relay/v1";

struct RelayAttempt {
    address_id: String,
    alias: String,
    message_id: String,
    thread_id: String,
}

impl StateTransaction<'_> {
    /// Whether the person sent the message an attempt delivers, so its final
    /// message goes back to them in the thread.
    pub(crate) fn relays_to_person(
        &self,
        namespace: &StateNamespace,
        attempt: &crate::DeliveryAttemptId,
    ) -> Result<bool, StoreError> {
        let message = self.transaction.query_row(
            "SELECT d.message_id FROM delivery_attempts a
             JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, delivery_id, address_id)
             WHERE a.session_socket_id=?1 AND a.worktree_id=?2 AND a.attempt_id=?3",
            params![namespace.session_socket_id, namespace.space_id.as_str(), attempt.as_str()],
            |row| row.get::<_, String>(0),
        )?;
        Ok(self
            .terminal_relay_source(namespace, &MessageId::new(message))?
            .is_some())
    }

    fn terminal_relay_source(
        &self,
        namespace: &StateNamespace,
        message: &MessageId,
    ) -> Result<Option<MessageId>, StoreError> {
        let source = match self.reminder_reply_target(namespace, message)? {
            Some(Some(original)) => original,
            Some(None) => return Ok(None),
            None => message.clone(),
        };
        let (human, author) = self.transaction.query_row(
            "SELECT author_kind='human', author_address_id
             FROM collaboration_messages WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?3",
            params![namespace.session_socket_id, namespace.space_id.as_str(), source.as_str()],
            |row| Ok((row.get::<_, bool>(0)?, row.get::<_, String>(1)?)),
        )?;
        Ok((human
            && self
                .operator_projection_addresses(namespace)?
                .contains(&author))
        .then_some(source))
    }

    /// Posts the final message of the turn that answered one attempt from the
    /// person, in that attempt's thread and in reply to their message, and
    /// closes the attempt without a reply. It posts nothing when the agent
    /// already replied in the thread during the turn.
    pub fn relay_final_message(
        &mut self,
        namespace: &StateNamespace,
        ending: &crate::TerminalRelayEnd,
        attribution: &MutationAttribution,
    ) -> Result<Option<MessageId>, StoreError> {
        let ns = namespace_params(namespace);
        let attempt = self
            .transaction
            .query_row(
                "SELECT a.address_id, r.alias_snapshot, m.message_id, m.thread_id
                 FROM delivery_attempts a
                 JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, delivery_id, address_id)
                 JOIN collaboration_messages m USING (session_socket_id, worktree_id, message_id)
                 JOIN message_recipients r ON r.session_socket_id=a.session_socket_id
                   AND r.worktree_id=a.worktree_id AND r.message_id=m.message_id AND r.address_id=a.address_id
                 JOIN delivery_attempt_state s USING (session_socket_id, worktree_id, attempt_id, address_id)
                 WHERE a.session_socket_id=?1 AND a.worktree_id=?2 AND a.attempt_id=?3
                   AND a.agent_session_id=?4
                   AND s.state IN ('submitted','acknowledged','settled')
                   AND NOT EXISTS (SELECT 1 FROM delivery_attempts newer
                       WHERE newer.session_socket_id=a.session_socket_id AND newer.worktree_id=a.worktree_id
                         AND newer.delivery_id=a.delivery_id AND newer.rowid>a.rowid)",
                params![
                    ns[0],
                    ns[1],
                    ending.attempt_id.as_str(),
                    ending.agent_session_id.as_str()
                ],
                |row| {
                    Ok(RelayAttempt {
                        address_id: row.get(0)?,
                        alias: row.get(1)?,
                        message_id: row.get(2)?,
                        thread_id: row.get(3)?,
                    })
                },
            )
            .optional()?;
        let text: String = ending
            .text
            .trim()
            .chars()
            .take(crate::TERMINAL_RELAY_MAX_CHARS)
            .collect();
        let Some(attempt) = attempt.filter(|_| !text.is_empty()) else {
            return Ok(None);
        };
        let Some(source) =
            self.terminal_relay_source(namespace, &MessageId::new(&attempt.message_id))?
        else {
            return Ok(None);
        };
        let ended_at = to_i64(ending.ended_at_ms)?;
        let at = MutationAttribution::new(
            attribution.actor_kind,
            &attribution.actor_id,
            ending.ended_at_ms,
        );
        let message_id = MessageId::new(format!("terminal-relay:{}", ending.attempt_id.as_str()));
        let replied = self.transaction.query_row(
            "SELECT EXISTS(SELECT 1 FROM collaboration_messages
               WHERE session_socket_id=?1 AND worktree_id=?2 AND message_id=?8)
             OR EXISTS(SELECT 1 FROM collaboration_messages reply
               WHERE reply.session_socket_id=?1 AND reply.worktree_id=?2 AND reply.thread_id=?3
                 AND reply.author_address_id=?4 AND reply.author_kind='agent_session'
                 AND reply.grammar_version NOT IN ('terminal-relay/v1','reminder/v1')
                 AND reply.created_at_ms >= ?5 AND reply.created_at_ms <= ?6)
             OR EXISTS(SELECT 1 FROM delivery_dispositions
               WHERE session_socket_id=?1 AND worktree_id=?2 AND attempt_id=?7 AND disposition_kind='reply')",
            params![ns[0], ns[1], attempt.thread_id, attempt.address_id,
                to_i64(ending.turn_started_at_ms)?, ended_at, ending.attempt_id.as_str(),
                message_id.as_str()],
            |row| row.get::<_, bool>(0),
        )?;
        if replied {
            return Ok(None);
        }
        let order = self.next_order(namespace, "thread_message", &attempt.thread_id)?;
        self.transaction.execute(
            "INSERT INTO collaboration_messages (
                session_socket_id, worktree_id, message_id, thread_id, thread_order, in_reply_to,
                author_address_id, author_alias_snapshot, author_kind, author_session_snapshot,
                request_id, authored_source, body, grammar_version, created_at_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'agent_session',?9,?3,?10,?10,?11,?12)",
            params![
                ns[0],
                ns[1],
                message_id.as_str(),
                attempt.thread_id,
                to_i64(order)?,
                source.as_str(),
                attempt.address_id,
                attempt.alias,
                ending.agent_session_id.as_str(),
                text,
                GRAMMAR,
                ended_at
            ],
        )?;
        self.append_transition(
            namespace,
            TransitionEntityKind::CollaborationMessage,
            message_id.as_str(),
            TransitionKind::Created,
            &at,
        )?;
        self.ensure_delivery_attempt_acknowledged(
            namespace,
            &ending.attempt_id,
            &ending.agent_session_id,
            &at,
        )?;
        if !self.delivery_disposition_exists(namespace, &ending.attempt_id)? {
            self.record_no_reply_disposition(
                namespace,
                &ending.attempt_id,
                &format!(
                    "{}{}",
                    AUTOMATIC_NO_REPLY_PREFIXES[0],
                    ending.attempt_id.as_str()
                ),
                &ending.agent_session_id,
                &at,
            )?;
        }
        let mut conversations = vec![(namespace.clone(), message_id.clone())];
        if let Some(copy) = self.mirror_terminal_relay(namespace, &source, &message_id, &at)? {
            conversations.push(copy);
        }
        for (conversation, relay) in conversations {
            self.open_attention_item(
                &conversation,
                &AttentionItemDraft {
                    id: crate::AttentionItemId::new(format!(
                        "terminal-relay:{}",
                        ending.attempt_id.as_str()
                    )),
                    source_kind: crate::AttentionSourceKind::TerminalRelay,
                    source_id: relay.as_str().to_owned(),
                    summary: format!(
                        "@{} answered in their terminal and may need your reply.",
                        attempt.alias
                    ),
                },
                &at,
            )?;
        }
        Ok(Some(message_id))
    }

    pub(super) fn resolve_terminal_relay_attention(
        &mut self,
        namespace: &StateNamespace,
        thread: &ThreadId,
        attribution: &MutationAttribution,
    ) -> Result<(), StoreError> {
        let mut conversations = self.paired_threads(namespace, thread)?;
        conversations.push((namespace.clone(), thread.clone()));
        for (namespace, thread) in conversations {
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
                    &namespace,
                    crate::AttentionSourceKind::TerminalRelay,
                    &id,
                    attribution,
                )?;
            }
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

    fn relay_attempt(
        store: &mut StateStore,
        ns: &StateNamespace,
        attempt: &str,
        session: &str,
        time: u64,
        text: &str,
    ) -> Option<MessageId> {
        store
            .transaction(|tx| {
                tx.relay_final_message(
                    ns,
                    &TerminalRelayEnd {
                        agent_session_id: AgentSessionId::new(session),
                        attempt_id: DeliveryAttemptId::new(attempt),
                        turn_started_at_ms: 10,
                        ended_at_ms: time,
                        text: text.to_owned(),
                    },
                    &at(time),
                )
            })
            .unwrap()
    }

    fn relay(
        store: &mut StateStore,
        ns: &StateNamespace,
        time: u64,
        text: &str,
    ) -> Option<MessageId> {
        relay_attempt(store, ns, "attempt", "session", time, text)
    }

    #[test]
    fn relays_operator_channel_and_direct_deliveries_without_recipient_mail() {
        for grammar in ["channel/v1", "mention/v1"] {
            let (mut store, ns) = fixture(grammar, true);
            assert_eq!(
                relay(&mut store, &ns, 20, "Which option?"),
                Some(MessageId::new("terminal-relay:attempt"))
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
    fn terminal_relay_stays_visible_but_out_of_model_context() {
        let (mut store, ns) = fixture("channel/v1", true);
        relay(&mut store, &ns, 20, "Decision: TERMINAL_PRIVATE_SENTINEL?");
        store
            .transaction(|tx| {
                let recap = tx.channel_recap(&ns, &MessageId::new("unrelated"))?;
                assert!(
                    !recap.contains("TERMINAL_PRIVATE_SENTINEL"),
                    "recap leaked: {recap}"
                );
                tx.ensure_thread_participant(
                    &ns,
                    &ThreadId::new("thread"),
                    &PaneAddressId::new("peer"),
                    true,
                    &at(30),
                )?;
                let context = tx.thread_context_snapshot(
                    &ns,
                    &ThreadId::new("thread"),
                    &PaneAddressId::new("peer"),
                )?;
                assert!(
                    !context
                        .payload
                        .to_string()
                        .contains("TERMINAL_PRIVATE_SENTINEL"),
                    "snapshot leaked: {}",
                    context.payload
                );
                let rows = tx.dashboard_rows(&ns)?;
                assert!(
                    rows.messages
                        .iter()
                        .any(|message| message.grammar_version == GRAMMAR
                            && message.body.contains("TERMINAL_PRIVATE_SENTINEL"))
                );
                Ok(())
            })
            .unwrap();
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
        assert!(relay(&mut store, &ns, 20, "Which one? ").is_some());
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
        assert!(relay(&mut store, &ns, 20, "Which one?").is_none());
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
        assert!(relay(&mut store, &ns, 20, "Which one?").is_some());
    }

    #[test]
    fn ignores_agent_and_native_human_sources_and_other_sessions() {
        let (mut store, ns) = fixture("mention/v1", false);
        assert!(relay(&mut store, &ns, 20, "Which one?").is_none());
        let (mut store, ns) = fixture("mention/v1", true);
        store
            .connection
            .execute_batch("DROP TRIGGER immutable_messages_update")
            .unwrap();
        store.connection.execute("UPDATE collaboration_messages SET author_address_id='peer' WHERE message_id='request'", []).unwrap();
        assert!(relay(&mut store, &ns, 20, "Which one?").is_none());
        let (mut store, ns) = fixture("mention/v1", true);
        assert!(
            relay_attempt(
                &mut store,
                &ns,
                "attempt",
                "other-session",
                20,
                "Other answer"
            )
            .is_none()
        );
        assert!(relay(&mut store, &ns, 20, "Which one?").is_some());
    }

    #[test]
    fn an_attempt_relays_once_and_never_with_empty_text() {
        let (mut store, ns) = fixture("mention/v1", true);
        assert!(relay(&mut store, &ns, 20, " ").is_none());
        assert!(relay(&mut store, &ns, 20, "Which one?").is_some());
        assert!(relay(&mut store, &ns, 21, "Different extraction").is_none());
        assert!(
            relay_attempt(&mut store, &ns, "unknown", "session", 21, "No such attempt").is_none()
        );
        let rows = store.transaction(|tx| tx.dashboard_rows(&ns)).unwrap();
        assert_eq!(
            rows.messages
                .iter()
                .filter(|m| m.grammar_version == GRAMMAR)
                .map(|m| m.body.as_str())
                .collect::<Vec<_>>(),
            ["Which one?"]
        );
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
            ),
            Some(MessageId::new("terminal-relay:attempt"))
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
        assert!(
            relay_attempt(
                &mut store,
                &ns,
                "followup-attempt",
                "session",
                40,
                "Another answer?"
            )
            .is_some()
        );
    }
}
