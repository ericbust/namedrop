use super::*;
use crate::PaneRoleId;
use sha2::{Digest, Sha256};

pub(super) const ADDRESS_PREFIX: &str = "relay:";
pub(super) const ROLE_PREFIX: &str = "role-relay:";
const DESCRIPTION: &str = "Paneless relay author.";

/// Where a remote agent is, as one machine knows it: on the recipient's
/// machine by the `host_id` the sender's server reported, and on the
/// sender's machine by the `rex hosts` label it sends to, because its first
/// message to the agent is stored before any call has read that host's
/// `host_id`.
#[derive(Clone, Copy, Debug)]
pub(crate) enum RelayOrigin<'a> {
    Host(&'a str),
    Label(&'a str),
}

impl RelayOrigin<'_> {
    fn key(self) -> String {
        match self {
            Self::Host(host_id) => format!("host:{host_id}"),
            Self::Label(label) => format!("label:{label}"),
        }
    }
}

/// One remote agent's paneless author in a space: `relay:` and 16 hex of
/// the digest of its origin and alias. It has no alias claim, binding,
/// observation, or registration, and its role is named for `display`, as
/// `bob@devbox`, which every message it writes shows as its sender.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RelayAuthor {
    pub address_id: PaneAddressId,
    pub role_id: PaneRoleId,
    pub display: String,
}

impl RelayAuthor {
    pub(crate) fn new(origin: RelayOrigin<'_>, alias: &str, display: String) -> Self {
        let digest = crate::lower_hex(&Sha256::digest(format!("{}\0{alias}", origin.key())));
        Self {
            address_id: PaneAddressId::new(format!("{ADDRESS_PREFIX}{}", &digest[..16])),
            role_id: PaneRoleId::new(format!("{ROLE_PREFIX}{}", &digest[..16])),
            display,
        }
    }

    fn snapshot(&self) -> RoutingAddressSnapshot {
        RoutingAddressSnapshot {
            address_id: self.address_id.clone(),
            alias: self.display.clone(),
            pane_binding_id: None,
            agent_session_id: None,
        }
    }
}

impl StateTransaction<'_> {
    pub(crate) fn ensure_relay_author(
        &mut self,
        namespace: &StateNamespace,
        author: &RelayAuthor,
        attribution: &MutationAttribution,
    ) -> Result<RoutingAddressSnapshot, StoreError> {
        self.transaction.execute(
            "INSERT OR IGNORE INTO pane_roles (
                session_socket_id, worktree_id, role_id, name, description
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                author.role_id.as_str(),
                author.display,
                DESCRIPTION
            ],
        )?;
        self.transaction.execute(
            "INSERT OR IGNORE INTO pane_addresses (
                session_socket_id, worktree_id, address_id, role_id,
                retired, collaboration_disabled, created_at_ms
             ) VALUES (?1, ?2, ?3, ?4, 0, 0, ?5)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                author.address_id.as_str(),
                author.role_id.as_str(),
                to_i64(attribution.occurred_at_ms)?
            ],
        )?;
        if !self.is_relay_author(namespace, &author.address_id)? {
            return Err(StoreError::internal(
                "a remote agent's reserved address conflicts with existing state",
            ));
        }
        Ok(author.snapshot())
    }

    pub(crate) fn is_relay_author(
        &self,
        namespace: &StateNamespace,
        address: &PaneAddressId,
    ) -> Result<bool, StoreError> {
        let Some(digest) = address.as_str().strip_prefix(ADDRESS_PREFIX) else {
            return Ok(false);
        };
        self.transaction
            .query_row(
                "SELECT EXISTS (
                    SELECT 1 FROM pane_addresses a
                    JOIN pane_roles r USING (session_socket_id, worktree_id, role_id)
                    WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
                      AND a.address_id = ?3 AND a.role_id = ?4 AND a.retired = 0
                      AND r.description = ?5
                      AND a.collaboration_disabled = 0 AND a.pane_type = 'agent'
                      AND a.remote_commands_terminal_id IS NULL AND a.persona IS NULL
                      AND NOT EXISTS (
                        SELECT 1 FROM pane_addresses WHERE session_socket_id = ?1
                          AND worktree_id = ?2 AND role_id = ?4 AND address_id <> ?3
                        UNION ALL
                        SELECT 1 FROM alias_claims WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3
                        UNION ALL
                        SELECT 1 FROM pane_bindings WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3
                        UNION ALL
                        SELECT 1 FROM agent_session_observations WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3
                        UNION ALL
                        SELECT 1 FROM integration_registrations WHERE session_socket_id = ?1 AND worktree_id = ?2 AND address_id = ?3
                      )
                 )",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    address.as_str(),
                    format!("{ROLE_PREFIX}{digest}"),
                    DESCRIPTION
                ],
                |row| row.get(0),
            )
            .map_err(StoreError::from)
    }

    /// A relay author's snapshot, which names it by its display alias.
    pub(crate) fn relay_author_snapshot(
        &self,
        namespace: &StateNamespace,
        address: &PaneAddressId,
    ) -> Result<Option<RoutingAddressSnapshot>, StoreError> {
        if !self.is_relay_author(namespace, address)? {
            return Ok(None);
        }
        let display: String = self.transaction.query_row(
            "SELECT r.name FROM pane_addresses a
             JOIN pane_roles r USING (session_socket_id, worktree_id, role_id)
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2 AND a.address_id = ?3",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                address.as_str()
            ],
            |row| row.get(0),
        )?;
        Ok(Some(RoutingAddressSnapshot {
            address_id: address.clone(),
            alias: display,
            pane_binding_id: None,
            agent_session_id: None,
        }))
    }

    /// The display alias of each relay author that takes part in the thread
    /// of `message`, among the spaces of `session_socket_id`.
    pub(crate) fn relay_participants(
        &self,
        session_socket_id: &str,
        message: &MessageId,
    ) -> Result<Vec<String>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT p.worktree_id, p.address_id FROM collaboration_messages m
             JOIN thread_participants p USING (session_socket_id, worktree_id, thread_id)
             WHERE m.session_socket_id = ?1 AND m.message_id = ?2 AND p.address_id LIKE 'relay:%'
             ORDER BY p.participant_order",
        )?;
        let participants = statement
            .query_map(params![session_socket_id, message.as_str()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut aliases = Vec::new();
        for (space, address) in participants {
            let namespace = StateNamespace::for_space(session_socket_id, space);
            if let Some(author) =
                self.relay_author_snapshot(&namespace, &PaneAddressId::new(address))?
            {
                aliases.push(author.alias);
            }
        }
        Ok(aliases)
    }

    /// The messages of `session_socket_id` after `rowid` that a relay author
    /// receives, oldest first.
    pub(crate) fn relay_recipient_messages(
        &self,
        session_socket_id: &str,
        after: i64,
    ) -> Result<Vec<ToRelayAuthor>, StoreError> {
        let mut statement = self.transaction.prepare(
            "SELECT m.rowid, m.worktree_id, m.message_id, m.thread_id, m.in_reply_to,
                m.author_alias_snapshot, m.author_address_id, m.body
             FROM collaboration_messages m
             WHERE m.session_socket_id = ?1 AND m.rowid > ?2 AND EXISTS (
                SELECT 1 FROM message_recipients r WHERE r.session_socket_id = m.session_socket_id
                  AND r.worktree_id = m.worktree_id AND r.message_id = m.message_id
                  AND r.address_id LIKE 'relay:%')
             ORDER BY m.rowid",
        )?;
        let rows = statement
            .query_map(params![session_socket_id, after], |row| {
                Ok(ToRelayAuthor {
                    rowid: row.get(0)?,
                    space: row.get(1)?,
                    message_id: row.get(2)?,
                    thread_id: row.get(3)?,
                    in_reply_to: row.get(4)?,
                    author_alias: row.get(5)?,
                    author_address_id: row.get(6)?,
                    body: row.get(7)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows)
    }

    /// The latest message in `thread` before `rowid` that a relay author
    /// wrote.
    pub(crate) fn latest_relay_authored(
        &self,
        namespace: &StateNamespace,
        thread: &ThreadId,
        before: i64,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT message_id FROM collaboration_messages
                 WHERE session_socket_id = ?1 AND worktree_id = ?2 AND thread_id = ?3
                   AND rowid < ?4 AND author_address_id LIKE 'relay:%'
                 ORDER BY rowid DESC LIMIT 1",
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    thread.as_str(),
                    before
                ],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// The space and thread of `message` among the spaces of
    /// `session_socket_id`.
    pub(crate) fn message_place(
        &self,
        session_socket_id: &str,
        message: &MessageId,
    ) -> Result<Option<(crate::SpaceId, ThreadId)>, StoreError> {
        Ok(self
            .transaction
            .query_row(
                "SELECT worktree_id, thread_id FROM collaboration_messages
                 WHERE session_socket_id = ?1 AND message_id = ?2",
                params![session_socket_id, message.as_str()],
                |row| {
                    Ok((
                        crate::SpaceId::new(row.get::<_, String>(0)?),
                        ThreadId::new(row.get::<_, String>(1)?),
                    ))
                },
            )
            .optional()?)
    }

    /// Inserts a message a remote agent wrote, from its relay author, to
    /// one local recipient. `relayed.request_id` names the item, so a retry
    /// after a crash finds the message the first try committed.
    pub(crate) fn insert_relayed_message(
        &mut self,
        namespace: &StateNamespace,
        relayed: &Relayed<'_>,
        attribution: &MutationAttribution,
    ) -> Result<MessageReceipt, StoreError> {
        let author = self.ensure_relay_author(namespace, relayed.author, attribution)?;
        if let Some(replayed) = self.replay_message(
            namespace,
            &author.address_id,
            &relayed.request_id,
            relayed.text,
            relayed.in_reply_to,
            ActorKind::AgentSession,
        )? {
            return Ok(replayed.receipt);
        }
        let exists: bool = self.transaction.query_row(
            "SELECT EXISTS (SELECT 1 FROM collaboration_threads
                WHERE session_socket_id = ?1 AND worktree_id = ?2 AND thread_id = ?3)",
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                relayed.thread.as_str()
            ],
            |row| row.get(0),
        )?;
        if !exists {
            let title = relayed
                .text
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("")
                .trim();
            let title: String = title.chars().take(80).collect();
            self.create_thread(namespace, relayed.thread, &title, attribution)?;
        }
        let draft = MessageDraft {
            thread_id: relayed.thread.clone(),
            message_id: relayed.message_id.clone(),
            in_reply_to: relayed.in_reply_to.cloned(),
            author_address_id: author.address_id,
            author_kind: ActorKind::AgentSession,
            author_alias_snapshot: author.alias,
            author_agent_session_snapshot: None,
            request_id: relayed.request_id.clone(),
            authored_source: relayed.text.to_owned(),
            body: relayed.text.to_owned(),
            grammar_version: "mention/v1".to_owned(),
            recipients: vec![crate::MessageRecipientDraft {
                address_id: relayed.recipient.address_id.clone(),
                alias_snapshot: relayed.recipient.alias.clone(),
                agent_session_snapshot: relayed.recipient.agent_session_id.clone(),
                delivery_id: relayed.delivery_id.clone(),
            }],
            context_references: Vec::new(),
        };
        self.append_message(namespace, &draft, attribution)
    }
}

/// A remote agent's message as B stores it.
pub(crate) struct Relayed<'a> {
    pub author: &'a RelayAuthor,
    pub request_id: String,
    pub thread: &'a ThreadId,
    pub message_id: MessageId,
    pub delivery_id: crate::DeliveryId,
    pub in_reply_to: Option<&'a MessageId>,
    pub recipient: &'a RoutingAddressSnapshot,
    pub text: &'a str,
}

/// A message one relay author receives.
pub(crate) struct ToRelayAuthor {
    pub rowid: i64,
    pub space: String,
    pub message_id: String,
    pub thread_id: String,
    pub in_reply_to: Option<String>,
    pub author_alias: String,
    pub author_address_id: String,
    pub body: String,
}
