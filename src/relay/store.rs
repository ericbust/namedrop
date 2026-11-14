//! `<state>/relay.sqlite3`: the relay's own state, apart from the main
//! database, whose schema a relay never changes. Only relay code opens it.

use crate::StoreError;
use rusqlite::{Connection, params};
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
