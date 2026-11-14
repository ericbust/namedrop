//! `<state>/relay.sqlite3`: the relay's own state, apart from the main
//! database, whose schema a relay never changes. Only relay code opens it.

use super::wire::{Outcome, Reply};
use crate::StoreError;
use rusqlite::{Connection, OptionalExtension, params};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, time::Duration};

const FILE: &str = "relay.sqlite3";
const VERSION: u32 = 1;

/// An item A could not deliver within this fails.
pub(crate) const OUTBOUND_LIFETIME_MS: i64 = 24 * 60 * 60 * 1000;
/// B keeps a reply for A, and the key of each item A sent, this long. The
/// key outlasts A's retries, so every retry still finds it.
pub(crate) const OUTBOX_LIFETIME_MS: i64 = 7 * 24 * 60 * 60 * 1000;
pub(crate) const INBOUND_LIFETIME_MS: i64 = OUTBOX_LIFETIME_MS;

/// A holds `peers`, `outbound`, and `collected`. B holds `inbound`,
/// `outbox`, `remote_threads`, and the tally of the outbox items that
/// expired, which Doctor reports after the items themselves are gone.
const SCHEMA: &str = "
CREATE TABLE peers (
    label TEXT PRIMARY KEY,
    host_id TEXT NOT NULL,
    contacted_at_ms INTEGER NOT NULL
);
CREATE TABLE outbound (
    label TEXT NOT NULL,
    message_id TEXT NOT NULL,
    recipient TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    space TEXT NOT NULL,
    sender TEXT NOT NULL,
    thread TEXT NOT NULL,
    in_reply_to TEXT,
    text TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('queued', 'accepted', 'failed', 'expired')),
    host_id TEXT,
    reason TEXT,
    created_at_ms INTEGER NOT NULL,
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (label, message_id, recipient)
);
CREATE TABLE collected (
    host_id TEXT NOT NULL,
    seq INTEGER NOT NULL,
    label TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    space TEXT NOT NULL,
    message_id TEXT NOT NULL,
    remote_message_id TEXT NOT NULL,
    remote_alias TEXT NOT NULL,
    thread TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (host_id, seq)
);
CREATE TABLE inbound (
    host_id TEXT NOT NULL,
    message_id TEXT NOT NULL,
    recipient TEXT NOT NULL,
    outcome TEXT NOT NULL,
    local_message_id TEXT,
    endpoint TEXT,
    space TEXT,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (host_id, message_id, recipient)
);
CREATE TABLE outbox (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    host_id TEXT NOT NULL,
    re TEXT NOT NULL,
    thread TEXT NOT NULL,
    sender TEXT NOT NULL,
    message_id TEXT NOT NULL,
    text TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    UNIQUE (host_id, message_id)
);
CREATE TABLE remote_threads (
    host_id TEXT NOT NULL,
    thread TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    space TEXT NOT NULL,
    local_thread TEXT NOT NULL,
    sender TEXT NOT NULL,
    display_host TEXT NOT NULL,
    PRIMARY KEY (host_id, thread, space)
);
CREATE INDEX remote_threads_by_local ON remote_threads (endpoint, space, local_thread);
CREATE TABLE outbox_expired (
    host_id TEXT PRIMARY KEY,
    count INTEGER NOT NULL
);
";

pub(crate) struct RelayStore {
    connection: Connection,
}

/// An item's key on B: A's `host_id`, A's message id, and the recipient.
pub(crate) struct InboundKey<'a> {
    pub host_id: &'a str,
    pub message_id: &'a str,
    pub recipient: &'a str,
}

/// A's thread as one of B's spaces carries it, and who started it there.
pub(crate) struct RemoteThread<'a> {
    pub host_id: &'a str,
    pub thread: &'a str,
    pub endpoint: &'a str,
    pub space: &'a str,
    pub sender: &'a str,
    pub display_host: &'a str,
}

/// One item A sends: the A message `message_id` for `recipient` on `label`,
/// from `sender` in the space of `endpoint`.
pub(crate) struct Outbound<'a> {
    pub label: &'a str,
    pub message_id: &'a str,
    pub recipient: &'a str,
    pub endpoint: &'a str,
    pub space: &'a str,
    pub sender: &'a str,
    pub thread: &'a str,
    pub in_reply_to: Option<&'a str>,
    pub text: &'a str,
    pub now_ms: i64,
}

impl RelayStore {
    /// Opens the relay's database in `state`, creating it when it is missing,
    /// and expires whatever outlived its limit by `now_ms`.
    pub(crate) fn open(state: &Path, now_ms: i64) -> Result<Self, StoreError> {
        fs::create_dir_all(state)?;
        let path = state.join(FILE);
        let mut connection = Connection::open(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))?;
        connection.busy_timeout(Duration::from_secs(2))?;
        let transaction = connection.transaction()?;
        let version: u32 =
            transaction.pragma_query_value(None, "user_version", |row| row.get(0))?;
        match version {
            0 => {
                transaction.execute_batch(SCHEMA)?;
                transaction.pragma_update(None, "user_version", VERSION)?;
            }
            VERSION => {}
            newer => {
                return Err(StoreError::invalid_input(format!(
                    "{} has relay schema {newer}, and this Namedrop knows {VERSION}",
                    path.display()
                )));
            }
        }
        transaction.commit()?;
        let mut store = Self { connection };
        store.expire(now_ms)?;
        Ok(store)
    }

    /// Queues `item`, unless its key is already stored.
    pub(crate) fn queue(&self, item: &Outbound<'_>) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT OR IGNORE INTO outbound (label, message_id, recipient, endpoint, space,
                sender, thread, in_reply_to, text, state, created_at_ms, updated_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'queued', ?10, ?10)",
            params![
                item.label,
                item.message_id,
                item.recipient,
                item.endpoint,
                item.space,
                item.sender,
                item.thread,
                item.in_reply_to,
                item.text,
                item.now_ms
            ],
        )?;
        Ok(())
    }

    /// The outcome B stored for an item's key.
    pub(crate) fn inbound(&self, key: &InboundKey<'_>) -> Result<Option<Outcome>, StoreError> {
        let outcome: Option<String> = self
            .connection
            .query_row(
                "SELECT outcome FROM inbound
                 WHERE host_id = ?1 AND message_id = ?2 AND recipient = ?3",
                params![key.host_id, key.message_id, key.recipient],
                |row| row.get(0),
            )
            .optional()?;
        outcome
            .map(|outcome| {
                serde_json::from_str(&outcome)
                    .map_err(|error| StoreError::invalid_input(error.to_string()))
            })
            .transpose()
    }

    /// Stores B's outcome for an item's key, with where an accepted item
    /// went.
    pub(crate) fn record_inbound(
        &self,
        key: &InboundKey<'_>,
        outcome: &Outcome,
        placed: Option<(&str, &str, &str)>,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT OR IGNORE INTO inbound (host_id, message_id, recipient, outcome,
                local_message_id, endpoint, space, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                key.host_id,
                key.message_id,
                key.recipient,
                serde_json::to_string(outcome)
                    .map_err(|error| StoreError::invalid_input(error.to_string()))?,
                placed.map(|placed| placed.0),
                placed.map(|placed| placed.1),
                placed.map(|placed| placed.2),
                now_ms
            ],
        )?;
        Ok(())
    }

    /// B's thread for A's thread in one of B's spaces.
    pub(crate) fn remote_thread(
        &self,
        thread: &RemoteThread<'_>,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT local_thread FROM remote_threads
                 WHERE host_id = ?1 AND thread = ?2 AND space = ?3",
                params![thread.host_id, thread.thread, thread.space],
                |row| row.get(0),
            )
            .optional()?)
    }

    pub(crate) fn record_remote_thread(
        &self,
        thread: &RemoteThread<'_>,
        local_thread: &str,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT OR IGNORE INTO remote_threads (host_id, thread, endpoint, space,
                local_thread, sender, display_host)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                thread.host_id,
                thread.thread,
                thread.endpoint,
                thread.space,
                local_thread,
                thread.sender,
                thread.display_host
            ],
        )?;
        Ok(())
    }

    /// Whether B's `local_thread` in `space` carries a thread of `host_id`.
    pub(crate) fn maps_thread(
        &self,
        host_id: &str,
        endpoint: &str,
        space: &str,
        local_thread: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM remote_threads WHERE host_id = ?1 AND endpoint = ?2
                AND space = ?3 AND local_thread = ?4)",
            params![host_id, endpoint, space, local_thread],
            |row| row.get(0),
        )?)
    }

    /// Drops B's replies for `host_id` through `ack_through`, and returns the
    /// rest from the oldest, within `limit` encoded bytes, whether more
    /// remain, and how many B keeps for `host_id`.
    pub(crate) fn outbox_page(
        &self,
        host_id: &str,
        ack_through: i64,
        limit: usize,
    ) -> Result<(Vec<Reply>, bool, usize), StoreError> {
        self.connection.execute(
            "DELETE FROM outbox WHERE host_id = ?1 AND seq <= ?2",
            params![host_id, ack_through],
        )?;
        let mut statement = self.connection.prepare(
            "SELECT seq, re, thread, sender, message_id, text FROM outbox
             WHERE host_id = ?1 ORDER BY seq",
        )?;
        let replies = statement.query_map(params![host_id], |row| {
            Ok(Reply {
                seq: row.get(0)?,
                re: row.get(1)?,
                thread: row.get(2)?,
                from: row.get(3)?,
                message_id: row.get(4)?,
                text: row.get(5)?,
            })
        })?;
        let (mut page, mut used, mut kept, mut more) = (Vec::new(), 0, 0, false);
        for reply in replies {
            let reply = reply?;
            kept += 1;
            let size = serde_json::to_vec(&reply).map_or(usize::MAX, |encoded| encoded.len() + 1);
            if more || used + size > limit {
                more = true;
                continue;
            }
            used += size;
            page.push(reply);
        }
        Ok((page, more, kept))
    }

    /// Fails each queued item older than its lifetime, drops each outbox
    /// item and `inbound` key older than theirs, and counts the dropped
    /// outbox items per remote sender.
    fn expire(&mut self, now_ms: i64) -> Result<(), StoreError> {
        let transaction = self.connection.transaction()?;
        transaction.execute(
            "UPDATE outbound SET state = 'expired',
                reason = label || ' was unreachable for 24 hours', updated_at_ms = ?1
             WHERE state = 'queued' AND created_at_ms <= ?2",
            params![now_ms, now_ms - OUTBOUND_LIFETIME_MS],
        )?;
        let outbox_limit = now_ms - OUTBOX_LIFETIME_MS;
        transaction.execute(
            "INSERT INTO outbox_expired (host_id, count)
             SELECT host_id, COUNT(*) FROM outbox WHERE created_at_ms <= ?1 GROUP BY host_id
             ON CONFLICT (host_id) DO UPDATE SET count = count + excluded.count",
            params![outbox_limit],
        )?;
        transaction.execute(
            "DELETE FROM outbox WHERE created_at_ms <= ?1",
            params![outbox_limit],
        )?;
        transaction.execute(
            "DELETE FROM inbound WHERE created_at_ms <= ?1",
            params![now_ms - INBOUND_LIFETIME_MS],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
