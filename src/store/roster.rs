use super::*;
use std::collections::HashSet;

#[derive(Serialize)]
struct RosterSnapshot {
    core: Option<String>,
    rows: Vec<RosterRow>,
    state_directory: PathBuf,
}

#[derive(Serialize)]
struct RosterRow {
    alias: String,
    terminal: Option<String>,
    space: String,
    lifecycle: crate::PaneAddressLifecycle,
    agent_status: Option<String>,
    waiting_on: Vec<WaitingOn>,
    attention: u64,
    recent_replies: Vec<RecentReply>,
}

#[derive(Serialize)]
struct WaitingOn {
    alias: String,
    message_id: MessageId,
}

#[derive(Serialize)]
struct RecentReply {
    message_id: MessageId,
    from: String,
    at_ms: u64,
}

impl StateStore {
    pub(crate) fn roster_snapshot(
        state: &Path,
        raw_core: Option<&str>,
    ) -> Result<impl Serialize, StoreError> {
        let mut snapshot = RosterSnapshot {
            core: raw_core.map(str::to_owned),
            rows: Vec::new(),
            state_directory: state.to_path_buf(),
        };
        let Some(raw_core) = raw_core else {
            return Ok(snapshot);
        };
        let database = state.join(DATABASE_FILE_NAME);
        match fs::metadata(&database) {
            Ok(_) => (),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(snapshot),
            Err(error) => return Err(error.into()),
        }
        let connection = Connection::open_with_flags(&database, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        connection.busy_timeout(Duration::from_millis(250))?;
        let mut store = Self {
            connection,
            state_directory: state.to_path_buf(),
            read_only: true,
        };
        snapshot.rows = store.transaction(|tx| collect_rows(tx, &format!("hc:{raw_core}")))?;
        Ok(snapshot)
    }
}

fn collect_rows(tx: &mut StateTransaction<'_>, core: &str) -> Result<Vec<RosterRow>, StoreError> {
    let version: u32 = tx
        .transaction
        .pragma_query_value(None, "user_version", |row| row.get(0))?;
    if version != SCHEMA_VERSION {
        return Err(StoreError::unsupported_schema(format!(
            "Unsupported state schema version {version}"
        )));
    }
    let spaces = {
        let mut statement = tx.transaction.prepare(
            "SELECT worktree_id FROM worktrees WHERE session_socket_id = ?1 ORDER BY worktree_id",
        )?;
        statement
            .query_map([core], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut rows = Vec::new();
    for space in spaces {
        let namespace = StateNamespace::for_space(core, &space);
        let attention: i64 = tx.transaction.query_row(
            "SELECT COUNT(*) FROM attention_items WHERE session_socket_id = ?1 AND worktree_id = ?2 AND resolved_at_ms IS NULL",
            params![core, space],
            |row| row.get(0),
        )?;
        for address in tx.list_pane_addresses(&namespace)? {
            let waiting_on = collect_waits(tx, &namespace, &address)?;
            let recent_replies = collect_replies(tx, &namespace, &address)?;
            rows.push(RosterRow {
                alias: address.alias,
                terminal: address.pane_id,
                space: space.clone(),
                lifecycle: address.lifecycle,
                agent_status: address.agent_status,
                waiting_on,
                attention: attention as u64,
                recent_replies,
            });
        }
    }
    rows.sort_by(|a, b| (&a.space, &a.alias, &a.terminal).cmp(&(&b.space, &b.alias, &b.terminal)));
    Ok(rows)
}

fn collect_waits(
    tx: &StateTransaction<'_>,
    namespace: &StateNamespace,
    address: &PaneAddressRecord,
) -> Result<Vec<WaitingOn>, StoreError> {
    let author = RoutingAddressSnapshot {
        address_id: address.id.clone(),
        alias: address.alias.clone(),
        pane_binding_id: None,
        agent_session_id: address.agent_session_id.clone(),
    };
    let messages = if author.agent_session_id.is_some() {
        tx.handoff_message_ids(namespace, &author, false, 50, 0)?
    } else {
        // SQLite's equality operator does not match a NULL author session.
        let mut statement = tx.transaction.prepare(
            "SELECT m.message_id FROM collaboration_messages m
             WHERE m.session_socket_id = ?1 AND m.worktree_id = ?2
               AND m.author_address_id = ?3 AND m.author_session_snapshot IS NULL
               AND m.in_reply_to IS NULL AND EXISTS (
                 SELECT 1 FROM mailbox_deliveries d
                 JOIN delivery_seats s USING (session_socket_id, worktree_id, delivery_id)
                 WHERE d.session_socket_id = m.session_socket_id AND d.worktree_id = m.worktree_id
                   AND d.message_id = m.message_id AND s.progress NOT IN ('failed', 'cancelled')
                   AND NOT EXISTS (
                     SELECT 1 FROM delivery_attempts a
                     JOIN delivery_dispositions p USING (session_socket_id, worktree_id, attempt_id)
                     WHERE a.session_socket_id = d.session_socket_id AND a.worktree_id = d.worktree_id
                       AND a.delivery_id = d.delivery_id))
             ORDER BY m.rowid DESC LIMIT 50",
        )?;
        statement
            .query_map(
                params![
                    namespace.session_socket_id,
                    namespace.space_id.as_str(),
                    address.id.as_str()
                ],
                |row| row.get::<_, String>(0).map(MessageId::new),
            )?
            .collect::<Result<Vec<_>, _>>()?
    };
    let mut waits = Vec::new();
    let mut seen = HashSet::new();
    for message in messages {
        let mut recipients = if author.agent_session_id.is_some() {
            let report = tx.handoff_report(namespace, &author, &message, 0, false)?;
            report["recipients"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|recipient| {
                    recipient["result_status"].is_null() || recipient["needs_attention"] == true
                })
                .filter_map(|recipient| recipient["target"]["alias"].as_str().map(str::to_owned))
                .collect::<Vec<_>>()
        } else {
            tx.handoff_recipients(namespace, &message)?
                .into_iter()
                .filter(|recipient| {
                    recipient.result_status().is_none()
                        || matches!(recipient.state, DeliveryState::Held | DeliveryState::Failed)
                })
                .map(|recipient| recipient.target.alias)
                .collect::<Vec<_>>()
        };
        recipients.sort();
        for alias in recipients {
            if seen.insert((alias.clone(), message.clone())) {
                waits.push(WaitingOn {
                    alias,
                    message_id: message.clone(),
                });
                if waits.len() == 50 {
                    return Ok(waits);
                }
            }
        }
    }
    Ok(waits)
}

fn collect_replies(
    tx: &StateTransaction<'_>,
    namespace: &StateNamespace,
    address: &PaneAddressRecord,
) -> Result<Vec<RecentReply>, StoreError> {
    let mut statement = tx.transaction.prepare(
        "SELECT m.message_id, m.author_alias_snapshot, m.created_at_ms
         FROM collaboration_messages m
         JOIN message_recipients r USING (session_socket_id, worktree_id, message_id)
         JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, message_id, address_id)
         WHERE m.session_socket_id = ?1 AND m.worktree_id = ?2 AND r.address_id = ?3
           AND r.session_snapshot IS ?4 AND m.in_reply_to IS NOT NULL
           AND d.state IN ('submitted', 'acknowledged', 'settled')
         ORDER BY m.created_at_ms DESC, m.message_id DESC LIMIT 5",
    )?;
    statement
        .query_map(
            params![
                namespace.session_socket_id,
                namespace.space_id.as_str(),
                address.id.as_str(),
                address.agent_session_id.as_ref().map(|id| id.as_str())
            ],
            |row| {
                Ok(RecentReply {
                    message_id: MessageId::new(row.get::<_, String>(0)?),
                    from: row.get(1)?,
                    at_ms: from_i64(row.get(2)?)?,
                })
            },
        )?
        .collect::<Result<Vec<_>, _>>()
        .map_err(StoreError::from)
}
