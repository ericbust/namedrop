//! The dashboard's read model: plain rows for one space, read in one short
//! deferred transaction. Context payloads, evidence and terminal output are
//! never selected, so no later layer can render them.
use super::*;

pub const MESSAGE_WINDOW: i64 = 150;
pub const TERMINAL_RUN_WINDOW: i64 = 20;
pub const COORDINATION_WINDOW: i64 = 20;

pub struct DashboardRows {
    pub namespace: StateNamespace,
    pub worktree: Option<crate::WorktreeState>,
    /// Includes retired addresses, so old messages keep their author's lane.
    pub addresses: Vec<AddressRow>,
    pub registrations: Vec<RegistrationRow>,
    /// The newest `MESSAGE_WINDOW` messages, oldest first.
    pub messages: Vec<MessageRow>,
    /// One per recipient of a windowed message, in recipient order.
    pub legs: Vec<LegRow>,
    /// Every inbound delivery still awaiting progress or held, in mailbox order.
    pub mailbox: Vec<MailboxRow>,
    pub threads: Vec<ThreadRow>,
    pub coordinations: Vec<CoordinationRow>,
    pub assignments: Vec<AssignmentRow>,
    pub terminal_runs: Vec<TerminalRunRow>,
    pub attention: Vec<crate::AttentionItemRecord>,
}

/// A history page and its recipient state from one database snapshot.
pub struct HistoryRows {
    pub messages: Vec<MessageRow>,
    pub legs: Vec<LegRow>,
}

pub struct AddressRow {
    pub address_id: String,
    pub alias: String,
    pub role_name: String,
    pub persona: Option<String>,
    pub pane_type: crate::PaneType,
    pub retired: bool,
    pub bound: bool,
    pub occupied: bool,
    pub agent_status: Option<String>,
    pub blocked_since_ms: Option<i64>,
}

pub struct RegistrationRow {
    pub address_id: String,
    pub runtime: String,
}

pub struct MessageRow {
    pub message_id: String,
    pub thread_id: String,
    pub in_reply_to: Option<String>,
    pub created_at_ms: i64,
    pub author_address_id: String,
    pub author_is_human: bool,
    pub grammar_version: String,
    pub body: String,
}

pub struct LegRow {
    pub message_id: String,
    pub address_id: String,
    /// `None` when the recipient has no mailbox delivery row yet.
    pub state: Option<DeliveryState>,
    pub acknowledged_at_ms: Option<i64>,
    pub disposition_kind: Option<String>,
    pub disposition_id: Option<String>,
}

pub struct MailboxRow {
    pub address_id: String,
    pub delivery_id: String,
    pub message_id: String,
    pub state: DeliveryState,
    pub author_address_id: String,
    pub author_is_human: bool,
    pub author_alias: String,
    pub in_reply_to: Option<String>,
    pub grammar_version: String,
    pub body: String,
    pub since_ms: i64,
    pub attempts: u32,
}

pub struct ThreadRow {
    pub thread_id: String,
    pub title: String,
    pub message_count: u32,
}

pub struct CoordinationRow {
    pub coordination_id: String,
    pub owner_address_id: String,
    pub deliverable: String,
    pub outcome: String,
    pub updated_at_ms: i64,
}

pub struct AssignmentRow {
    pub coordination_id: String,
    pub assignee_address_id: String,
    pub work_outcome: String,
    pub blocked_reason: Option<String>,
    pub waiting_on_address_id: Option<String>,
}

pub struct TerminalRunRow {
    pub address_id: String,
    pub command: String,
    pub started_at_ms: i64,
    pub exit_code: Option<i32>,
}

pub struct SpaceRow {
    pub namespace: StateNamespace,
    pub state: crate::WorktreeState,
}

const WINDOW_MESSAGE_IDS: &str = "SELECT message_id FROM collaboration_messages
     WHERE session_socket_id = ?1 AND worktree_id = ?2
     ORDER BY created_at_ms DESC, rowid DESC LIMIT ?3";

const LATEST_ATTEMPT: &str = "a.rowid = (
     SELECT latest.rowid FROM delivery_attempts latest
     WHERE latest.session_socket_id = d.session_socket_id
       AND latest.worktree_id = d.worktree_id AND latest.delivery_id = d.delivery_id
     ORDER BY latest.created_at_ms DESC, latest.rowid DESC LIMIT 1)";

impl StateStore {
    pub fn dashboard_rows(
        &mut self,
        namespace: &StateNamespace,
    ) -> Result<DashboardRows, StoreError> {
        self.transaction(|transaction| transaction.dashboard_rows(namespace))
    }

    pub fn dashboard_spaces(&mut self) -> Result<Vec<SpaceRow>, StoreError> {
        self.transaction(|transaction| transaction.dashboard_spaces())
    }

    pub fn dashboard_history(
        &mut self,
        namespace: &StateNamespace,
        before: Option<&str>,
        limit: i64,
    ) -> Result<Option<HistoryRows>, StoreError> {
        self.transaction(|transaction| {
            transaction.dashboard_history(namespace, before, limit.clamp(1, 100))
        })
    }

    /// Changes whenever another connection commits; the dashboard's cheap
    /// "anything changed" gate.
    pub fn data_version(&self) -> Result<i64, StoreError> {
        Ok(self
            .connection
            .pragma_query_value(None, "data_version", |row| row.get(0))?)
    }

    pub fn set_busy_timeout(&self, timeout: Duration) -> Result<(), StoreError> {
        Ok(self.connection.busy_timeout(timeout)?)
    }

    #[cfg(test)]
    pub(crate) fn connection(&self) -> &Connection {
        &self.connection
    }
}

impl StateTransaction<'_> {
    fn dashboard_history(
        &self,
        namespace: &StateNamespace,
        before: Option<&str>,
        limit: i64,
    ) -> Result<Option<HistoryRows>, StoreError> {
        let ns = namespace_params(namespace);
        let cursor = match before {
            Some(id) => {
                let mut statement = self.transaction.prepare(
                    "SELECT created_at_ms, rowid FROM collaboration_messages
                     WHERE session_socket_id = ?1 AND worktree_id = ?2 AND message_id = ?3",
                )?;
                match statement
                    .query_row(params![ns[0], ns[1], id], |row| {
                        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
                    })
                    .optional()?
                {
                    Some(cursor) => Some(cursor),
                    None => return Ok(None),
                }
            }
            None => None,
        };
        let predicate = if cursor.is_some() {
            "AND (created_at_ms, rowid) < (?3, ?4)"
        } else {
            ""
        };
        let sql = format!(
            "SELECT message_id, thread_id, in_reply_to, created_at_ms, author_address_id,
                    author_kind, grammar_version, body
             FROM collaboration_messages
             WHERE session_socket_id = ?1 AND worktree_id = ?2 {predicate}
             ORDER BY created_at_ms DESC, rowid DESC LIMIT {}",
            if cursor.is_some() { "?5" } else { "?3" }
        );
        let args: Vec<rusqlite::types::Value> = match cursor {
            Some((at, rowid)) => vec![
                ns[0].to_owned().into(),
                ns[1].to_owned().into(),
                at.into(),
                rowid.into(),
                limit.into(),
            ],
            None => vec![
                ns[0].to_owned().into(),
                ns[1].to_owned().into(),
                limit.into(),
            ],
        };
        let messages = self.rows(&sql, rusqlite::params_from_iter(args), |row| {
            Ok(MessageRow {
                message_id: row.get(0)?,
                thread_id: row.get(1)?,
                in_reply_to: row.get(2)?,
                created_at_ms: row.get(3)?,
                author_address_id: row.get(4)?,
                author_is_human: row.get::<_, String>(5)? == "human",
                grammar_version: row.get(6)?,
                body: row.get(7)?,
            })
        })?;
        if messages.is_empty() {
            return Ok(Some(HistoryRows {
                messages,
                legs: Vec::new(),
            }));
        }
        let placeholders = (3..messages.len() + 3)
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(",");
        let sql = format!(
            "WITH page_recipients AS MATERIALIZED (
                 SELECT session_socket_id, worktree_id, message_id, address_id, recipient_order
                 FROM message_recipients
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND message_id IN ({placeholders})
             ), page_addresses AS MATERIALIZED (
                 SELECT DISTINCT session_socket_id, worktree_id, address_id FROM page_recipients
             ), page_deliveries AS MATERIALIZED (
                 SELECT d.* FROM page_addresses addresses CROSS JOIN mailbox_deliveries d
                 WHERE d.session_socket_id = addresses.session_socket_id
                   AND d.worktree_id = addresses.worktree_id
                   AND d.address_id = addresses.address_id
                   AND d.message_id IN (SELECT message_id FROM page_recipients)
             )
             SELECT r.message_id, r.address_id, d.state, ack.acknowledged_at_ms,
                    p.disposition_kind, p.disposition_id
             FROM page_recipients r
             LEFT JOIN page_deliveries d USING (session_socket_id, worktree_id, message_id, address_id)
             LEFT JOIN delivery_attempts a ON {LATEST_ATTEMPT}
             LEFT JOIN delivery_acknowledgements ack USING (session_socket_id, worktree_id, attempt_id)
             LEFT JOIN delivery_dispositions p USING (session_socket_id, worktree_id, attempt_id)
             ORDER BY r.message_id, r.recipient_order"
        );
        let values = std::iter::once(ns[0].to_owned())
            .chain(std::iter::once(ns[1].to_owned()))
            .chain(messages.iter().map(|message| message.message_id.clone()));
        let legs = self.rows(&sql, rusqlite::params_from_iter(values), |row| {
            Ok(LegRow {
                message_id: row.get(0)?,
                address_id: row.get(1)?,
                state: row
                    .get::<_, Option<String>>(2)?
                    .map(parse_delivery_state)
                    .transpose()?,
                acknowledged_at_ms: row.get(3)?,
                disposition_kind: row.get(4)?,
                disposition_id: row.get(5)?,
            })
        })?;
        Ok(Some(HistoryRows { messages, legs }))
    }

    fn dashboard_rows(&self, namespace: &StateNamespace) -> Result<DashboardRows, StoreError> {
        let ns = namespace_params(namespace);
        let windowed = params![ns[0], ns[1], MESSAGE_WINDOW];
        let mut messages = self.rows(
            "SELECT message_id, thread_id, in_reply_to, created_at_ms, author_address_id,
                    author_kind, grammar_version, body
             FROM collaboration_messages
             WHERE session_socket_id = ?1 AND worktree_id = ?2
             ORDER BY created_at_ms DESC, rowid DESC LIMIT ?3",
            windowed,
            |row| {
                Ok(MessageRow {
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
        )?;
        messages.reverse();
        let legs = self.rows(
            &format!(
                "SELECT r.message_id, r.address_id, d.state, ack.acknowledged_at_ms,
                        p.disposition_kind, p.disposition_id
                 FROM message_recipients r
                 LEFT JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, message_id, address_id)
                 LEFT JOIN delivery_attempts a ON {LATEST_ATTEMPT}
                 LEFT JOIN delivery_acknowledgements ack USING (session_socket_id, worktree_id, attempt_id)
                 LEFT JOIN delivery_dispositions p USING (session_socket_id, worktree_id, attempt_id)
                 WHERE r.session_socket_id = ?1 AND r.worktree_id = ?2
                   AND r.message_id IN ({WINDOW_MESSAGE_IDS})
                 ORDER BY r.message_id, r.recipient_order"
            ),
            windowed,
            |row| {
                Ok(LegRow {
                    message_id: row.get(0)?,
                    address_id: row.get(1)?,
                    state: row
                        .get::<_, Option<String>>(2)?
                        .map(parse_delivery_state)
                        .transpose()?,
                    acknowledged_at_ms: row.get(3)?,
                    disposition_kind: row.get(4)?,
                    disposition_id: row.get(5)?,
                })
            },
        )?;
        let mailbox = self.rows(
            &format!(
                "SELECT d.address_id, d.delivery_id, d.message_id, d.state,
                        m.author_address_id, m.author_kind, m.author_alias_snapshot, m.body,
                        m.in_reply_to, m.grammar_version,
                        COALESCE(ack.acknowledged_at_ms, a.created_at_ms, d.created_at_ms),
                        (SELECT COUNT(*) FROM delivery_attempts x
                         WHERE x.session_socket_id = d.session_socket_id
                           AND x.worktree_id = d.worktree_id AND x.delivery_id = d.delivery_id)
                 FROM mailbox_deliveries d
                 JOIN collaboration_messages m USING (session_socket_id, worktree_id, message_id)
                 LEFT JOIN delivery_attempts a ON {LATEST_ATTEMPT}
                 LEFT JOIN delivery_acknowledgements ack USING (session_socket_id, worktree_id, attempt_id)
                 WHERE d.session_socket_id = ?1 AND d.worktree_id = ?2
                   AND d.state IN ('queued', 'dispatching', 'submitted', 'acknowledged', 'held')
                 ORDER BY d.address_id, d.mailbox_order"
            ),
            ns,
            |row| {
                Ok(MailboxRow {
                    address_id: row.get(0)?,
                    delivery_id: row.get(1)?,
                    message_id: row.get(2)?,
                    state: parse_delivery_state(row.get(3)?)?,
                    author_address_id: row.get(4)?,
                    author_is_human: row.get::<_, String>(5)? == "human",
                    author_alias: row.get(6)?,
                    body: row.get(7)?,
                    in_reply_to: row.get(8)?,
                    grammar_version: row.get(9)?,
                    since_ms: row.get(10)?,
                    attempts: row.get(11)?,
                })
            },
        )?;
        let threads = self.rows(
            &format!(
                "SELECT t.thread_id, t.title,
                        (SELECT COUNT(*) FROM collaboration_messages c
                         WHERE c.session_socket_id = t.session_socket_id
                           AND c.worktree_id = t.worktree_id AND c.thread_id = t.thread_id)
                 FROM collaboration_threads t
                 WHERE t.session_socket_id = ?1 AND t.worktree_id = ?2
                   AND t.thread_id IN (
                       SELECT thread_id FROM collaboration_messages
                       WHERE session_socket_id = ?1 AND worktree_id = ?2
                         AND message_id IN ({WINDOW_MESSAGE_IDS}))"
            ),
            windowed,
            |row| {
                Ok(ThreadRow {
                    thread_id: row.get(0)?,
                    title: row.get(1)?,
                    message_count: row.get(2)?,
                })
            },
        )?;
        const WINDOW_COORDINATIONS: &str = "SELECT coordination_id FROM coordinations
             WHERE session_socket_id = ?1 AND worktree_id = ?2
             ORDER BY outcome = 'open' DESC, updated_at_ms DESC, coordination_id LIMIT ?3";
        let coordination_window = params![ns[0], ns[1], COORDINATION_WINDOW];
        let coordinations = self.rows(
            &format!(
                "SELECT coordination_id, owner_address_id, deliverable, outcome, updated_at_ms
                 FROM coordinations
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                   AND coordination_id IN ({WINDOW_COORDINATIONS})
                 ORDER BY outcome = 'open' DESC, updated_at_ms DESC, coordination_id"
            ),
            coordination_window,
            |row| {
                Ok(CoordinationRow {
                    coordination_id: row.get(0)?,
                    owner_address_id: row.get(1)?,
                    deliverable: row.get(2)?,
                    outcome: row.get(3)?,
                    updated_at_ms: row.get(4)?,
                })
            },
        )?;
        let assignments = self.rows(
            &format!(
                "SELECT a.coordination_id, a.recipient_address_id, a.work_outcome,
                        a.blocked_reason, w.recipient_address_id
                 FROM coordination_assignments a
                 LEFT JOIN coordination_assignments w
                   ON w.session_socket_id = a.session_socket_id AND w.worktree_id = a.worktree_id
                  AND w.assignment_id = a.waiting_on_assignment_id
                 WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
                   AND a.coordination_id IN ({WINDOW_COORDINATIONS})
                 ORDER BY a.coordination_id, a.rowid"
            ),
            coordination_window,
            |row| {
                Ok(AssignmentRow {
                    coordination_id: row.get(0)?,
                    assignee_address_id: row.get(1)?,
                    work_outcome: row.get(2)?,
                    blocked_reason: row.get(3)?,
                    waiting_on_address_id: row.get(4)?,
                })
            },
        )?;
        let terminal_runs = self.rows(
            "SELECT a.address_id, t.command, t.started_at_ms, t.exit_code
             FROM terminal_executions t
             JOIN delivery_attempts a USING (session_socket_id, worktree_id, attempt_id)
             WHERE t.session_socket_id = ?1 AND t.worktree_id = ?2
               AND t.started_at_ms IS NOT NULL
             ORDER BY t.started_at_ms DESC, t.rowid DESC LIMIT ?3",
            params![ns[0], ns[1], TERMINAL_RUN_WINDOW],
            |row| {
                Ok(TerminalRunRow {
                    address_id: row.get(0)?,
                    command: row.get(1)?,
                    started_at_ms: row.get(2)?,
                    exit_code: row.get(3)?,
                })
            },
        )?;
        // Order observations once, then find blocked runs in grouped passes.
        // Correlated history lookups would rescan every observation per address.
        let addresses = self.rows(
            "WITH observations AS (
                 SELECT o.address_id, o.agent_session_id, o.status, o.observed_at_ms,
                        ROW_NUMBER() OVER (
                            PARTITION BY o.address_id
                            ORDER BY o.observed_at_ms DESC, o.rowid DESC) AS recency
                 FROM agent_session_observations o
                 JOIN pane_bindings b
                   ON b.session_socket_id = o.session_socket_id AND b.worktree_id = o.worktree_id
                  AND b.address_id = o.address_id AND b.active = 1
                 WHERE o.session_socket_id = ?1 AND o.worktree_id = ?2
                   AND o.observed_at_ms >= b.bound_at_ms),
             boundaries AS (
                 SELECT address_id, MIN(recency) AS last_nonblocked
                 FROM observations WHERE status <> 'blocked'
                 GROUP BY address_id),
             blocked_runs AS (
                 SELECT o.address_id, MIN(o.observed_at_ms) AS blocked_since_ms
                 FROM observations o
                 LEFT JOIN boundaries b ON b.address_id = o.address_id
                 WHERE o.status = 'blocked'
                   AND (b.last_nonblocked IS NULL OR o.recency < b.last_nonblocked)
                 GROUP BY o.address_id),
             released AS (
                 SELECT address_id, alias, MAX(history_order)
                 FROM alias_history
                 WHERE session_socket_id = ?1 AND worktree_id = ?2
                 GROUP BY address_id)
             SELECT a.address_id, COALESCE(c.alias, h.alias, ltrim(r.name, '@')), r.name, a.persona, a.pane_type, a.retired,
                    b.pane_id IS NOT NULL,
                    COALESCE(o.agent_session_id, '') <> '', o.status, br.blocked_since_ms
             FROM pane_addresses a
             JOIN pane_roles r USING (session_socket_id, worktree_id, role_id)
             -- A retired agent's alias can be released for reuse; its lane keeps the last one.
             LEFT JOIN alias_claims c
               ON c.session_socket_id = a.session_socket_id AND c.worktree_id = a.worktree_id
              AND c.address_id = a.address_id AND c.claim_state = 'active'
             LEFT JOIN released h ON h.address_id = a.address_id
             LEFT JOIN pane_bindings b
               ON b.session_socket_id = a.session_socket_id AND b.worktree_id = a.worktree_id
              AND b.address_id = a.address_id AND b.active = 1
             LEFT JOIN observations o ON o.address_id = a.address_id AND o.recency = 1
             LEFT JOIN blocked_runs br ON br.address_id = a.address_id
             WHERE a.session_socket_id = ?1 AND a.worktree_id = ?2
               -- An envoy stands in for an agent in another workspace, not a lane here.
               AND NOT EXISTS (
                   SELECT 1 FROM space_envoys e
                   WHERE e.session_socket_id = a.session_socket_id
                     AND e.worktree_id = a.worktree_id AND e.address_id = a.address_id)
             ORDER BY a.address_id",
            ns,
            |row| {
                Ok(AddressRow {
                    address_id: row.get(0)?,
                    alias: row.get(1)?,
                    role_name: row.get(2)?,
                    persona: row.get(3)?,
                    pane_type: if row.get::<_, String>(4)? == "terminal" {
                        crate::PaneType::Terminal
                    } else {
                        crate::PaneType::Agent
                    },
                    retired: row.get(5)?,
                    bound: row.get(6)?,
                    occupied: row.get(7)?,
                    agent_status: row.get(8)?,
                    blocked_since_ms: row.get(9)?,
                })
            },
        )?;
        let registrations = self.rows(
            "SELECT address_id, runtime FROM integration_registrations
             WHERE session_socket_id = ?1 AND worktree_id = ?2 AND active = 1",
            ns,
            |row| {
                Ok(RegistrationRow {
                    address_id: row.get(0)?,
                    runtime: row.get(1)?,
                })
            },
        )?;
        Ok(DashboardRows {
            namespace: namespace.clone(),
            worktree: self.worktree_state(namespace)?,
            addresses,
            registrations,
            messages,
            legs,
            mailbox,
            threads,
            coordinations,
            assignments,
            terminal_runs,
            attention: self.unresolved_attention_items(namespace)?,
        })
    }

    fn dashboard_spaces(&self) -> Result<Vec<SpaceRow>, StoreError> {
        self.rows(
            "SELECT session_socket_id, worktree_id, activated, paused, deactivated
             FROM worktrees ORDER BY created_at_ms, worktree_id",
            [],
            |row| {
                Ok(SpaceRow {
                    namespace: StateNamespace::for_space(
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                    ),
                    state: crate::WorktreeState {
                        activated: row.get(2)?,
                        paused: row.get(3)?,
                        deactivated: row.get(4)?,
                    },
                })
            },
        )
    }

    fn rows<T>(
        &self,
        sql: &str,
        params: impl rusqlite::Params,
        map: impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>,
    ) -> Result<Vec<T>, StoreError> {
        let mut statement = self.transaction.prepare(sql)?;
        let rows = statement
            .query_map(params, map)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(rows)
    }
}
