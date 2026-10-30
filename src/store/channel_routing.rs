use super::*;
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelRoutingState {
    Routing,
    Routed,
    Unrouted,
    RouterFailed,
}

fn state(kind: &str) -> ChannelRoutingState {
    match kind {
        "queued" | "dispatching" => ChannelRoutingState::Routing,
        "settled" => ChannelRoutingState::Routed,
        "empty" => ChannelRoutingState::Unrouted,
        _ => ChannelRoutingState::RouterFailed,
    }
}

const ACTOR: &str = "channel-router/v1";
const FACTS: &str = "entity_kind = 'collaboration_message'
    AND actor_kind = 'human' AND actor_id = 'channel-router/v1'
    AND transition_kind IN ('queued','dispatching','settled','empty','failed')";

#[derive(serde::Serialize)]
pub(crate) struct ChannelProjection {
    state: ChannelRoutingState,
    to: Vec<String>,
    mentioned: Vec<String>,
}

pub(crate) struct DeliveredChannel {
    pub channel: ChannelProjection,
    pub delivered: Vec<String>,
}

pub(crate) enum ChannelDelivery {
    Delivered(DeliveredChannel),
    Refused(ChannelDeliveryRefusal),
}

pub(crate) enum ChannelDeliveryRefusal {
    NotFound,
    InvalidMember,
    Routing,
    Stopped,
}

pub(crate) struct ChannelClaim {
    pub namespace: StateNamespace,
    pub draft: MessageDraft,
    pub token: u64,
    pub candidates: Vec<RoutingAddressSnapshot>,
    pub participants: Vec<RoutingAddressSnapshot>,
    pub host: Option<AgentRuntime>,
}

fn attribution(now: u64) -> MutationAttribution {
    MutationAttribution {
        actor_kind: ActorKind::Human,
        actor_id: ACTOR.to_owned(),
        occurred_at_ms: now,
    }
}

impl StateStore {
    pub(crate) fn channel_context(
        &mut self,
        claim: &ChannelClaim,
    ) -> Result<Option<dashboard::DashboardRows>, StoreError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Deferred)?;
        let tx = StateTransaction { transaction };
        let current = tx.channel_fact(&claim.namespace, &claim.draft.message_id)?
            == Some((claim.token, "dispatching".to_owned()));
        let rows = if current
            && !tx
                .thread_control_state(&claim.namespace, &claim.draft.thread_id)?
                .stopped
        {
            Some(tx.dashboard_rows(&claim.namespace)?)
        } else {
            None
        };
        tx.transaction.commit()?;
        Ok(rows)
    }
}

impl StateTransaction<'_> {
    pub(crate) fn deliver_channel_message(
        &mut self,
        ns: &StateNamespace,
        message: &MessageId,
        aliases: &[String],
        actor: &MutationAttribution,
    ) -> Result<ChannelDelivery, StoreError> {
        use ChannelDelivery::{Delivered, Refused};
        use ChannelDeliveryRefusal::{InvalidMember, NotFound, Routing, Stopped};
        let stored = self
            .transaction
            .query_row(
                "SELECT thread_id, thread_order, author_address_id, author_kind,
                    grammar_version, in_reply_to IS NULL
             FROM collaboration_messages
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3",
                params![ns.session_socket_id, ns.space_id.as_str(), message.as_str()],
                |row| {
                    Ok((
                        ThreadId::new(row.get::<_, String>(0)?),
                        from_i64(row.get(1)?)?,
                        PaneAddressId::new(row.get::<_, String>(2)?),
                        row.get::<_, String>(3)?,
                        row.get::<_, String>(4)?,
                        row.get::<_, bool>(5)?,
                    ))
                },
            )
            .optional()?;
        let Some((thread, order, author, kind, grammar, root)) = stored else {
            return Ok(Refused(NotFound));
        };
        let routing = self.channel_routing_state(ns, message)?;
        if kind != ActorKind::Human.as_str()
            || !self.is_operator_address(ns, &author)?
            || !(root && grammar == "channel/v1" || routing.is_some())
        {
            return Ok(Refused(NotFound));
        }
        if routing == Some(ChannelRoutingState::Routing) {
            return Ok(Refused(Routing));
        }
        if aliases.is_empty() {
            return Ok(Refused(InvalidMember));
        }
        let members = self.channel_members(ns)?;
        let mut selected: Vec<&RoutingAddressSnapshot> = Vec::new();
        for alias in aliases {
            let Some(member) = members
                .iter()
                .find(|member| member.alias == *alias && alias != "operator")
            else {
                return Ok(Refused(InvalidMember));
            };
            if !selected
                .iter()
                .any(|previous| previous.address_id == member.address_id)
            {
                selected.push(member);
            }
        }
        let existing = self.message_routing_snapshots(
            ns,
            message,
            "message_recipients",
            "recipient_order",
            false,
        )?;
        let delivered = selected.iter().map(|member| member.alias.clone()).collect();
        selected.retain(|member| {
            !existing
                .iter()
                .any(|old| old.address_id == member.address_id)
        });
        if !selected.is_empty() {
            if self.thread_control_state(ns, &thread)?.stopped {
                return Ok(Refused(Stopped));
            }
            let recipients: Vec<_> = selected
                .into_iter()
                .map(|member| crate::MessageRecipientDraft {
                    delivery_id: crate::DeliveryId::new(crate::IdGenerator::next_id(
                        &crate::ProcessIds::default(),
                        "delivery",
                    )),
                    address_id: member.address_id.clone(),
                    alias_snapshot: member.alias.clone(),
                    agent_session_snapshot: member.agent_session_id.clone(),
                })
                .collect();
            self.insert_recipient_deliveries(ns, message, &grammar, &recipients, actor)?;
            for recipient in recipients {
                self.ensure_thread_participant(
                    ns,
                    &thread,
                    &recipient.address_id,
                    order > 1,
                    actor,
                )?;
            }
        }
        Ok(Delivered(DeliveredChannel {
            channel: self.channel_projection(ns, message)?.ok_or_else(|| {
                StoreError::invalid_input("eligible channel post has no projection")
            })?,
            delivered,
        }))
    }

    pub(crate) fn append_pending_channel(
        &mut self,
        namespace: &StateNamespace,
        draft: &MessageDraft,
        actor: &MutationAttribution,
    ) -> Result<MessageReceipt, StoreError> {
        if draft.author_kind != ActorKind::Human
            || !self.is_operator_address(namespace, &draft.author_address_id)?
            || !draft.recipients.is_empty()
        {
            return Err(StoreError::invalid_input(
                "pending channel posts require the local operator and no recipients",
            ));
        }
        let receipt = self.append_message_mode(namespace, draft, actor, None, true)?;
        if !receipt.idempotent_replay {
            self.append_transition(
                namespace,
                TransitionEntityKind::CollaborationMessage,
                draft.message_id.as_str(),
                TransitionKind::Queued,
                &attribution(actor.occurred_at_ms),
            )?;
        }
        Ok(receipt)
    }

    pub(crate) fn channel_projection(
        &self,
        ns: &StateNamespace,
        message: &MessageId,
    ) -> Result<Option<ChannelProjection>, StoreError> {
        let state = self.channel_routing_state(ns, message)?;
        let (source, root) = self.transaction.query_row(
            "SELECT authored_source, grammar_version = 'channel/v1' AND in_reply_to IS NULL
             FROM collaboration_messages
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3",
            params![ns.session_socket_id, ns.space_id.as_str(), message.as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
        )?;
        if state.is_none() && !root {
            return Ok(None);
        }
        let to = self
            .message_routing_snapshots(ns, message, "message_recipients", "recipient_order", false)?
            .into_iter()
            .filter(|m| m.alias != "operator")
            .map(|m| m.alias)
            .collect::<Vec<_>>();
        Ok(Some(ChannelProjection {
            state: state.unwrap_or(if to.is_empty() {
                ChannelRoutingState::Unrouted
            } else {
                ChannelRoutingState::Routed
            }),
            to,
            mentioned: self.channel_mentioned(ns, &source)?,
        }))
    }

    pub(crate) fn channel_routing_state(
        &self,
        ns: &StateNamespace,
        message: &MessageId,
    ) -> Result<Option<ChannelRoutingState>, StoreError> {
        Ok(self
            .channel_fact(ns, message)?
            .map(|(_, kind)| state(&kind)))
    }

    pub(crate) fn channel_routing_states(
        &self,
        ns: &StateNamespace,
        ids: &str,
    ) -> Result<std::collections::HashMap<String, ChannelRoutingState>, StoreError> {
        let mut statement = self.transaction.prepare(&format!(
            "SELECT entity_id, transition_kind FROM (
                SELECT entity_id, transition_kind,
                    ROW_NUMBER() OVER (PARTITION BY entity_id ORDER BY worktree_order DESC) rank
                FROM transition_facts WHERE session_socket_id = ?1 AND worktree_id = ?2
                  AND entity_id IN (SELECT value FROM json_each(?3)) AND {FACTS}
             ) WHERE rank = 1"
        ))?;
        Ok(statement
            .query_map(
                params![ns.session_socket_id, ns.space_id.as_str(), ids],
                |row| Ok((row.get::<_, String>(0)?, state(&row.get::<_, String>(1)?))),
            )?
            .collect::<Result<_, _>>()?)
    }

    fn channel_fact(
        &self,
        ns: &StateNamespace,
        message: &MessageId,
    ) -> Result<Option<(u64, String)>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                &format!(
                    "SELECT worktree_order, transition_kind FROM transition_facts
                WHERE session_socket_id = ?1 AND worktree_id = ?2 AND entity_id = ?3 AND {FACTS}
                ORDER BY worktree_order DESC LIMIT 1"
                ),
                params![ns.session_socket_id, ns.space_id.as_str(), message.as_str()],
                |row| Ok((from_i64(row.get(0)?)?, row.get(1)?)),
            )
            .optional()?)
    }

    fn pending_channel(
        &self,
        kind: &str,
    ) -> Result<Option<(StateNamespace, MessageId)>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                &format!(
                    "SELECT session_socket_id, worktree_id, entity_id FROM (
                SELECT session_socket_id, worktree_id, entity_id, transition_kind, worktree_order,
                    ROW_NUMBER() OVER (
                        PARTITION BY session_socket_id, worktree_id, entity_id
                        ORDER BY worktree_order DESC) rank
                FROM transition_facts WHERE {FACTS}
             ) WHERE rank = 1 AND transition_kind = ?1
             ORDER BY worktree_order, session_socket_id, worktree_id LIMIT 1"
                ),
                [kind],
                |row| {
                    Ok((
                        StateNamespace::for_space(
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                        ),
                        MessageId::new(row.get::<_, String>(2)?),
                    ))
                },
            )
            .optional()?)
    }

    #[cfg(test)]
    pub(crate) fn has_pending_channel(&self) -> Result<bool, StoreError> {
        Ok(self.pending_channel("queued")?.is_some())
    }

    pub(crate) fn claim_channel(&mut self, now: u64) -> Result<Option<ChannelClaim>, StoreError> {
        while let Some((ns, message)) = self.pending_channel("queued")? {
            self.append_transition(
                &ns,
                TransitionEntityKind::CollaborationMessage,
                message.as_str(),
                TransitionKind::Dispatching,
                &attribution(now),
            )?;
            let claim = self.channel_claim(ns, message)?;
            if self
                .thread_control_state(&claim.namespace, &claim.draft.thread_id)?
                .stopped
            {
                self.finish_channel(&claim, &[], true, now)?;
                continue;
            }
            return Ok(Some(claim));
        }
        Ok(None)
    }

    fn channel_claim(
        &self,
        ns: StateNamespace,
        message: MessageId,
    ) -> Result<ChannelClaim, StoreError> {
        let draft = self.transaction.query_row(
            "SELECT thread_id, in_reply_to, author_address_id, author_kind,
                    author_alias_snapshot, author_session_snapshot, request_id,
                    authored_source, body, grammar_version
             FROM collaboration_messages
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3",
            params![ns.session_socket_id, ns.space_id.as_str(), message.as_str()],
            |row| {
                Ok(MessageDraft {
                    message_id: message.clone(),
                    thread_id: ThreadId::new(row.get::<_, String>(0)?),
                    in_reply_to: row.get::<_, Option<String>>(1)?.map(MessageId::new),
                    author_address_id: PaneAddressId::new(row.get::<_, String>(2)?),
                    author_kind: parse_actor_kind(row.get(3)?)?,
                    author_alias_snapshot: row.get(4)?,
                    author_agent_session_snapshot: row
                        .get::<_, Option<String>>(5)?
                        .map(crate::AgentSessionId::new),
                    request_id: row.get(6)?,
                    authored_source: row.get(7)?,
                    body: row.get(8)?,
                    grammar_version: row.get(9)?,
                    recipients: Vec::new(),
                    context_references: Vec::new(),
                })
            },
        )?;
        let members = self.channel_members(&ns)?;
        let participants = if draft.in_reply_to.is_some() {
            self.thread_agent_participants(&ns, &draft.thread_id)?
        } else {
            Vec::new()
        };
        let mentioned = self.channel_mentioned(&ns, &draft.authored_source)?;
        let candidates = members
            .into_iter()
            .filter(|m| {
                mentioned.contains(&m.alias)
                    && !participants.iter().any(|p| p.address_id == m.address_id)
            })
            .collect();
        let token = self
            .channel_fact(&ns, &message)?
            .ok_or_else(|| StoreError::invalid_input("missing channel claim"))?
            .0;
        Ok(ChannelClaim {
            host: self.preferred_router_host()?,
            namespace: ns,
            draft,
            token,
            candidates,
            participants,
        })
    }

    pub(crate) fn finish_channel(
        &mut self,
        claim: &ChannelClaim,
        selected: &[String],
        failed: bool,
        now: u64,
    ) -> Result<bool, StoreError> {
        if self.channel_fact(&claim.namespace, &claim.draft.message_id)?
            != Some((claim.token, "dispatching".to_owned()))
        {
            return Ok(false);
        }
        if self
            .thread_control_state(&claim.namespace, &claim.draft.thread_id)?
            .stopped
        {
            self.append_transition(
                &claim.namespace,
                TransitionEntityKind::CollaborationMessage,
                claim.draft.message_id.as_str(),
                TransitionKind::Failed,
                &attribution(now),
            )?;
            return Ok(true);
        }
        let current = self.channel_members(&claim.namespace)?;
        let valid = |old: &RoutingAddressSnapshot| {
            current
                .iter()
                .find(|m| m.address_id == old.address_id && m.alias == old.alias)
                .cloned()
        };
        let picked: Vec<_> = if failed {
            Vec::new()
        } else {
            selected
                .iter()
                .filter_map(|alias| claim.candidates.iter().find(|m| &m.alias == alias))
                .filter_map(valid)
                .collect()
        };
        let kind = if failed {
            TransitionKind::Failed
        } else if picked.is_empty() {
            TransitionKind::Empty
        } else {
            TransitionKind::Settled
        };
        let baseline = self.thread_agent_participants(&claim.namespace, &claim.draft.thread_id)?;
        let mut recipients: Vec<_> = claim
            .participants
            .iter()
            .filter_map(|old| {
                baseline
                    .iter()
                    .find(|m| m.address_id == old.address_id && m.alias == old.alias)
                    .cloned()
            })
            .collect();
        for member in picked {
            if !recipients.iter().any(|m| m.address_id == member.address_id) {
                recipients.push(member);
            }
        }
        let mut draft = claim.draft.clone();
        draft.recipients = recipients
            .into_iter()
            .map(|m| crate::MessageRecipientDraft {
                delivery_id: crate::DeliveryId::new(crate::IdGenerator::next_id(
                    &crate::ProcessIds::default(),
                    "delivery",
                )),
                address_id: m.address_id,
                alias_snapshot: m.alias,
                agent_session_snapshot: m.agent_session_id,
            })
            .collect();
        let order = self
            .message_receipt(&claim.namespace, &draft.message_id, false)?
            .thread_order;
        self.insert_message_recipients(&claim.namespace, &draft, order, &attribution(now))?;
        self.append_transition(
            &claim.namespace,
            TransitionEntityKind::CollaborationMessage,
            draft.message_id.as_str(),
            kind,
            &attribution(now),
        )?;
        Ok(true)
    }

    pub(crate) fn fail_abandoned_channels(
        &mut self,
        now: u64,
    ) -> Result<Vec<StateNamespace>, StoreError> {
        let mut namespaces = Vec::new();
        while let Some((ns, message)) = self.pending_channel("dispatching")? {
            let claim = self.channel_claim(ns, message)?;
            self.finish_channel(&claim, &[], true, now)?;
            if !namespaces.contains(&claim.namespace) {
                namespaces.push(claim.namespace);
            }
        }
        Ok(namespaces)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn queued_fixture(count: usize) -> (PathBuf, StateStore, StateNamespace) {
        let root = std::env::temp_dir().join(format!(
            "channel-context-{}",
            crate::IdGenerator::next_id(&crate::ProcessIds::default(), "test")
        ));
        let mut store = StateStore::open(&root).unwrap();
        let ns = StateNamespace::for_space("synthetic", "w1");
        let actor = MutationAttribution::new(ActorKind::Human, "test", 1);
        store
            .transaction(|tx| {
                tx.activate_worktree(&ns, &actor)?;
                tx.ensure_operator_author(&ns, &actor)?;
                let thread = ThreadId::new("thread");
                tx.create_thread(&ns, &thread, "test", &actor)?;
                for index in 0..count {
                    tx.append_pending_channel(
                        &ns,
                        &MessageDraft {
                            thread_id: thread.clone(),
                            message_id: MessageId::new(format!("message-{index}")),
                            in_reply_to: None,
                            author_address_id: PaneAddressId::new("operator:local"),
                            author_kind: ActorKind::Human,
                            author_alias_snapshot: "operator".to_owned(),
                            author_agent_session_snapshot: None,
                            request_id: format!("request-{index}"),
                            authored_source: "Hello @planner".to_owned(),
                            body: "Hello @planner".to_owned(),
                            grammar_version: "channel/v1".to_owned(),
                            recipients: Vec::new(),
                            context_references: Vec::new(),
                        },
                        &actor,
                    )?;
                }
                Ok(())
            })
            .unwrap();
        (root, store, ns)
    }

    #[test]
    fn context_rechecks_stop_and_obsolete_claims() {
        for stop in [false, true] {
            let (root, mut store, ns) = queued_fixture(1);
            let claim = store
                .transaction(|tx| tx.claim_channel(2))
                .unwrap()
                .unwrap();
            assert_eq!(
                store
                    .channel_context(&claim)
                    .unwrap()
                    .unwrap()
                    .messages
                    .len(),
                1
            );
            store
                .transaction(|tx| {
                    if stop {
                        tx.stop_thread(&ns, &claim.draft.thread_id, &attribution(3))?;
                    } else {
                        tx.fail_abandoned_channels(3)?;
                    }
                    Ok(())
                })
                .unwrap();
            assert!(store.channel_context(&claim).unwrap().is_none());
            let settled = store
                .transaction(|tx| tx.finish_channel(&claim, &[], true, 4))
                .unwrap();
            assert_eq!(settled, stop);
            assert_eq!(
                store
                    .transaction(|tx| tx.channel_routing_state(&ns, &claim.draft.message_id))
                    .unwrap(),
                Some(ChannelRoutingState::RouterFailed)
            );
            drop(store);
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn context_projection_releases_writer_ownership_after_claim_commit() {
        use std::{
            ffi::{CStr, c_char, c_int, c_void},
            sync::mpsc,
            thread,
        };
        struct Gate {
            entered: mpsc::Sender<()>,
            resume: mpsc::Receiver<()>,
            paused: bool,
        }
        unsafe extern "C" fn pause_projection(
            pointer: *mut c_void,
            action: c_int,
            table: *const c_char,
            _: *const c_char,
            _: *const c_char,
            _: *const c_char,
        ) -> c_int {
            let gate = unsafe { &mut *pointer.cast::<Gate>() };
            if !gate.paused
                && action == rusqlite::ffi::SQLITE_READ
                && !table.is_null()
                && unsafe { CStr::from_ptr(table) }.to_bytes() == b"pane_addresses"
            {
                gate.paused = true;
                if gate.entered.send(()).is_err()
                    || gate.resume.recv_timeout(Duration::from_secs(3)).is_err()
                {
                    return rusqlite::ffi::SQLITE_DENY;
                }
            }
            rusqlite::ffi::SQLITE_OK
        }
        for wal in [false, true] {
            let (root, mut store, _) = queued_fixture(151);
            if wal {
                store
                    .connection
                    .execute_batch("PRAGMA journal_mode=WAL")
                    .unwrap();
            }
            let claim = store
                .transaction(|tx| tx.claim_channel(2))
                .unwrap()
                .unwrap();
            let mut writer = StateStore::open(&root).unwrap();
            writer.connection.busy_timeout(Duration::ZERO).unwrap();
            let (entered, paused) = mpsc::channel();
            let (resume, resumed) = mpsc::channel();
            let reader = thread::spawn(move || {
                let mut gate = Gate {
                    entered,
                    resume: resumed,
                    paused: false,
                };
                unsafe {
                    rusqlite::ffi::sqlite3_set_authorizer(
                        store.connection.handle(),
                        Some(pause_projection),
                        (&mut gate as *mut Gate).cast(),
                    );
                }
                let rows = store.channel_context(&claim);
                unsafe {
                    rusqlite::ffi::sqlite3_set_authorizer(
                        store.connection.handle(),
                        None,
                        std::ptr::null_mut(),
                    );
                }
                assert_eq!(rows.unwrap().unwrap().messages.len(), 150);
                store
            });
            paused.recv_timeout(Duration::from_secs(3)).unwrap();
            let outcome = match writer
                .connection
                .transaction_with_behavior(TransactionBehavior::Immediate)
            {
                Ok(transaction) => {
                    if wal {
                        let write = transaction
                            .execute("UPDATE worktrees SET created_at_ms=created_at_ms+1", [])
                            .map(|_| ());
                        write.and_then(|()| transaction.commit())
                    } else {
                        transaction.rollback()
                    }
                }
                Err(error) => Err(error),
            };
            resume.send(()).unwrap();
            let store = reader.join().unwrap();
            outcome.expect("context projection must allow a writer reservation, and WAL must also allow its commit");
            drop((store, writer));
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn pending_commit_is_atomic_and_claim_fences_late_completions() {
        let root = std::env::temp_dir().join(format!(
            "channel-claim-{}-{}",
            std::process::id(),
            crate::IdGenerator::next_id(&crate::ProcessIds::default(), "test")
        ));
        let mut store = StateStore::open(&root).unwrap();
        assert!(
            !StateStore::open_read_only(&root)
                .unwrap()
                .unwrap()
                .transaction(|tx| tx.has_pending_channel())
                .unwrap()
        );
        let ns = StateNamespace::for_space("synthetic", "w1");
        let actor = MutationAttribution::new(ActorKind::Human, "test", 1);
        let draft = MessageDraft {
            thread_id: ThreadId::new("thread"),
            message_id: MessageId::new("message"),
            in_reply_to: None,
            author_address_id: PaneAddressId::new("operator:local"),
            author_kind: ActorKind::Human,
            author_alias_snapshot: "operator".to_owned(),
            author_agent_session_snapshot: None,
            request_id: "request".to_owned(),
            authored_source: "Hello @planner".to_owned(),
            body: "Hello @planner".to_owned(),
            grammar_version: "channel/v1".to_owned(),
            recipients: Vec::new(),
            context_references: Vec::new(),
        };
        store.connection.execute_batch("CREATE TRIGGER reject_queue BEFORE INSERT ON transition_facts WHEN NEW.actor_id='channel-router/v1' AND NEW.transition_kind='queued' BEGIN SELECT RAISE(ABORT,'synthetic queue failure'); END;").unwrap();
        assert!(
            store
                .transaction(|tx| {
                    tx.activate_worktree(&ns, &actor)?;
                    tx.ensure_operator_author(&ns, &actor)?;
                    tx.create_thread(&ns, &draft.thread_id, "test", &actor)?;
                    tx.append_pending_channel(&ns, &draft, &actor)
                })
                .is_err()
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM collaboration_messages", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM idempotency_records", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        store
            .connection
            .execute_batch("DROP TRIGGER reject_queue;")
            .unwrap();
        store
            .transaction(|tx| {
                tx.activate_worktree(&ns, &actor)?;
                tx.ensure_operator_author(&ns, &actor)?;
                tx.create_thread(&ns, &draft.thread_id, "test", &actor)?;
                tx.append_pending_channel(&ns, &draft, &actor)?;
                assert!(
                    tx.append_pending_channel(&ns, &draft, &actor)?
                        .idempotent_replay
                );
                Ok(())
            })
            .unwrap();
        assert!(
            StateStore::open_read_only(&root)
                .unwrap()
                .unwrap()
                .transaction(|tx| tx.has_pending_channel())
                .unwrap()
        );
        let claim = store
            .transaction(|tx| tx.claim_channel(2))
            .unwrap()
            .unwrap();
        assert!(
            !StateStore::open_read_only(&root)
                .unwrap()
                .unwrap()
                .transaction(|tx| tx.has_pending_channel())
                .unwrap()
        );
        assert!(
            store
                .transaction(|tx| tx.claim_channel(3))
                .unwrap()
                .is_none()
        );
        store
            .transaction(|tx| tx.fail_abandoned_channels(4))
            .unwrap();
        assert!(
            !store
                .transaction(|tx| tx.finish_channel(&claim, &["planner".to_owned()], false, 5))
                .unwrap()
        );
        assert_eq!(
            store
                .transaction(|tx| tx.channel_routing_state(&ns, &draft.message_id))
                .unwrap(),
            Some(ChannelRoutingState::RouterFailed)
        );
        assert_eq!(store.connection.query_row("SELECT COUNT(*) FROM transition_facts WHERE actor_id='channel-router/v1' AND transition_kind='queued'",[],|row|row.get::<_,i64>(0)).unwrap(),1);
        assert_eq!(
            store
                .connection
                .query_row("SELECT COUNT(*) FROM message_recipients", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(store);
        fs::remove_dir_all(root).unwrap();
    }
}
