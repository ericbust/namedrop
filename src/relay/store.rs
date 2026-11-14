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

/// A holds `peers`, `outbound`, `collected`, and `cursors`, the last
/// collected `seq` of each B and the outbox epoch it counts in. B holds
/// `inbound`, `outbox`, its `outbox_epoch`, `remote_threads`, and the tally
/// of the replies that expired, which Doctor reports after the items
/// themselves are gone. Both hold `capture`, the last main-database message
/// each endpoint's capture read.
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
    notified_at_ms INTEGER,
    PRIMARY KEY (label, message_id, recipient)
);
CREATE TABLE collected (
    host_id TEXT NOT NULL,
    epoch TEXT NOT NULL,
    seq INTEGER NOT NULL,
    label TEXT NOT NULL,
    endpoint TEXT NOT NULL,
    space TEXT NOT NULL,
    message_id TEXT NOT NULL,
    remote_message_id TEXT NOT NULL,
    remote_alias TEXT NOT NULL,
    thread TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (host_id, epoch, seq)
);
CREATE TABLE cursors (
    host_id TEXT PRIMARY KEY,
    epoch TEXT NOT NULL,
    seq INTEGER NOT NULL
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
    sender TEXT PRIMARY KEY,
    count INTEGER NOT NULL
);
CREATE TABLE capture (
    endpoint TEXT PRIMARY KEY,
    rowid INTEGER NOT NULL
);
CREATE TABLE outbox_epoch (
    epoch TEXT NOT NULL
);
";

pub(crate) struct RelayStore {
    connection: Connection,
}

/// An item queued on A for one label.
#[derive(Clone, Debug)]
pub(crate) struct Queued {
    pub message_id: String,
    pub recipient: String,
    pub sender: String,
    pub thread: String,
    pub in_reply_to: Option<String>,
    pub text: String,
    /// The `host_id` the item was first sent to.
    pub host_id: Option<String>,
}

/// An item that failed or expired and whose sender has not been told.
pub(crate) struct Unsent {
    pub label: String,
    pub message_id: String,
    pub recipient: String,
    pub space: String,
    pub reason: String,
}

/// How a queued item ends.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Settled {
    Accepted,
    Failed,
}

/// A reply A collected from B and inserted as `message_id`.
pub(crate) struct Collected<'a> {
    pub host_id: &'a str,
    pub epoch: &'a str,
    pub seq: i64,
    pub label: &'a str,
    pub endpoint: &'a str,
    pub space: &'a str,
    pub message_id: &'a str,
    pub remote_message_id: &'a str,
    pub remote_alias: &'a str,
    pub thread: &'a str,
    pub now_ms: i64,
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
        let mut store = Self::read(state)?;
        store.expire(now_ms)?;
        Ok(store)
    }

    /// Opens the relay's database in `state` as `open` does, without the
    /// expiry, for a reader that looks often.
    pub(crate) fn read(state: &Path) -> Result<Self, StoreError> {
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
                transaction.execute(
                    "INSERT INTO outbox_epoch (epoch) VALUES (?1)",
                    params![crate::IdGenerator::next_id(
                        &crate::ProcessIds::default(),
                        "outbox"
                    )],
                )?;
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
        Ok(Self { connection })
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

    /// The rowid of the last main-database message capture read for
    /// `endpoint`.
    pub(crate) fn capture_cursor(&self, endpoint: &str) -> Result<i64, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT rowid FROM capture WHERE endpoint = ?1",
                params![endpoint],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0))
    }

    pub(crate) fn set_capture_cursor(&self, endpoint: &str, rowid: i64) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO capture (endpoint, rowid) VALUES (?1, ?2)
             ON CONFLICT (endpoint) DO UPDATE SET rowid = MAX(rowid, excluded.rowid)",
            params![endpoint, rowid],
        )?;
        Ok(())
    }

    /// A's thread and `host_id` for B's `local_thread` in `space`.
    pub(crate) fn mapped_thread(
        &self,
        endpoint: &str,
        space: &str,
        local_thread: &str,
    ) -> Result<Option<(String, String)>, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT host_id, thread FROM remote_threads
                 WHERE endpoint = ?1 AND space = ?2 AND local_thread = ?3 LIMIT 1",
                params![endpoint, space, local_thread],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    /// A's message id for the item B stored as `local_message_id`, when it
    /// came from `host_id`.
    pub(crate) fn relayed_as(
        &self,
        host_id: &str,
        local_message_id: &str,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT message_id FROM inbound WHERE host_id = ?1 AND local_message_id = ?2",
                params![host_id, local_message_id],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Keeps a reply for `host_id`, once per B message, cut so that its
    /// text encodes in `TEXT_LIMIT` bytes and every page holds at least one
    /// reply.
    pub(crate) fn keep_reply(
        &self,
        host_id: &str,
        reply: &Reply,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT OR IGNORE INTO outbox (host_id, re, thread, sender, message_id, text,
                created_at_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                host_id,
                reply.re,
                reply.thread,
                reply.from,
                reply.message_id,
                super::wire::cut(&reply.text, super::wire::TEXT_LIMIT),
                now_ms
            ],
        )?;
        Ok(())
    }

    /// B's id for the reply of `alias` on `label` that an answer in A's
    /// `thread` of `space` goes back to: the one A stored as `in_reply_to`
    /// when it is that agent's, or else that agent's latest one there.
    pub(crate) fn answered_reply(
        &self,
        endpoint: &str,
        space: &str,
        thread: &str,
        in_reply_to: Option<&str>,
        label: &str,
        alias: &str,
    ) -> Result<Option<String>, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT remote_message_id FROM collected
                 WHERE endpoint = ?1 AND space = ?2 AND thread = ?3 AND label = ?5
                   AND remote_alias = ?6 AND message_id <> ''
                 ORDER BY message_id = ?4 DESC, created_at_ms DESC, seq DESC LIMIT 1",
                params![endpoint, space, thread, in_reply_to, label, alias],
                |row| row.get(0),
            )
            .optional()?)
    }

    /// Counts a reply to the remote sender `display` that B could not keep,
    /// because the A message it answers is no longer known, with the replies
    /// that expired.
    pub(crate) fn count_unrouted(&self, display: &str) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO outbox_expired (sender, count) VALUES (?1, 1)
             ON CONFLICT (sender) DO UPDATE SET count = count + 1",
            params![display],
        )?;
        Ok(())
    }

    /// Each remote sender whose replies expired on B, by its display name,
    /// with how many expired.
    pub(crate) fn expired_replies(&self) -> Result<Vec<(String, i64)>, StoreError> {
        let mut statement = self
            .connection
            .prepare("SELECT sender, count FROM outbox_expired WHERE count > 0 ORDER BY sender")?;
        let expired = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<Result<_, _>>()?;
        Ok(expired)
    }

    /// The items `endpoint` queued for `label`, oldest first.
    pub(crate) fn queued(&self, label: &str, endpoint: &str) -> Result<Vec<Queued>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT message_id, recipient, sender, thread, in_reply_to, text, host_id
             FROM outbound WHERE label = ?1 AND endpoint = ?2 AND state = 'queued'
             ORDER BY created_at_ms, rowid",
        )?;
        let queued = statement
            .query_map(params![label, endpoint], |row| {
                Ok(Queued {
                    message_id: row.get(0)?,
                    recipient: row.get(1)?,
                    sender: row.get(2)?,
                    thread: row.get(3)?,
                    in_reply_to: row.get(4)?,
                    text: row.get(5)?,
                    host_id: row.get(6)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(queued)
    }

    /// The labels of `endpoint` with an item queued or a thread open at
    /// `now_ms`, each of which needs its worker.
    pub(crate) fn active_labels(
        &self,
        endpoint: &str,
        now_ms: i64,
    ) -> Result<Vec<String>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT label FROM outbound WHERE endpoint = ?1 AND (state = 'queued'
                OR (state = 'accepted' AND updated_at_ms > ?2))
             UNION SELECT label FROM collected WHERE endpoint = ?1 AND created_at_ms > ?2
             ORDER BY label",
        )?;
        let labels = statement
            .query_map(params![endpoint, now_ms - OUTBOUND_LIFETIME_MS], |row| {
                row.get(0)
            })?
            .collect::<Result<_, _>>()?;
        Ok(labels)
    }

    /// The items of `endpoint` that failed or expired and whose sender has
    /// not been told, oldest first.
    pub(crate) fn unsent(&self, endpoint: &str) -> Result<Vec<Unsent>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT label, message_id, recipient, space, COALESCE(reason, state) FROM outbound
             WHERE endpoint = ?1 AND state IN ('failed', 'expired') AND notified_at_ms IS NULL
             ORDER BY updated_at_ms, rowid",
        )?;
        let unsent = statement
            .query_map(params![endpoint], |row| {
                Ok(Unsent {
                    label: row.get(0)?,
                    message_id: row.get(1)?,
                    recipient: row.get(2)?,
                    space: row.get(3)?,
                    reason: row.get(4)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(unsent)
    }

    pub(crate) fn told(&self, unsent: &Unsent, now_ms: i64) -> Result<(), StoreError> {
        self.connection.execute(
            "UPDATE outbound SET notified_at_ms = ?4
             WHERE label = ?1 AND message_id = ?2 AND recipient = ?3",
            params![unsent.label, unsent.message_id, unsent.recipient, now_ms],
        )?;
        Ok(())
    }

    /// Each label of `endpoint` with items that failed or expired, with how
    /// many of each.
    pub(crate) fn unsent_by_label(
        &self,
        endpoint: &str,
    ) -> Result<Vec<(String, i64, i64)>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT label, SUM(state = 'failed'), SUM(state = 'expired') FROM outbound
             WHERE endpoint = ?1 AND state IN ('failed', 'expired')
             GROUP BY label ORDER BY label",
        )?;
        let counts = statement
            .query_map(params![endpoint], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })?
            .collect::<Result<_, _>>()?;
        Ok(counts)
    }

    /// Records the `host_id` an item was first sent to.
    pub(crate) fn pin(&self, label: &str, item: &Queued, host_id: &str) -> Result<(), StoreError> {
        self.connection.execute(
            "UPDATE outbound SET host_id = ?4
             WHERE label = ?1 AND message_id = ?2 AND recipient = ?3 AND host_id IS NULL",
            params![label, item.message_id, item.recipient, host_id],
        )?;
        Ok(())
    }

    /// Ends a queued item as `state`, with `reason` for a failure.
    pub(crate) fn settle(
        &self,
        label: &str,
        item: &Queued,
        state: Settled,
        reason: Option<&str>,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "UPDATE outbound SET state = ?4, reason = ?5, updated_at_ms = ?6
             WHERE label = ?1 AND message_id = ?2 AND recipient = ?3 AND state = 'queued'",
            params![
                label,
                item.message_id,
                item.recipient,
                match state {
                    Settled::Accepted => "accepted",
                    Settled::Failed => "failed",
                },
                reason,
                now_ms
            ],
        )?;
        Ok(())
    }

    pub(crate) fn set_peer(
        &self,
        label: &str,
        host_id: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO peers (label, host_id, contacted_at_ms) VALUES (?1, ?2, ?3)
             ON CONFLICT (label) DO UPDATE SET host_id = excluded.host_id,
                contacted_at_ms = excluded.contacted_at_ms",
            params![label, host_id, now_ms],
        )?;
        Ok(())
    }

    /// Whether `endpoint` has a thread open with `label`: an item accepted,
    /// or a reply collected, within 24 hours of `now_ms`.
    pub(crate) fn thread_open(
        &self,
        label: &str,
        endpoint: &str,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        Ok(self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM outbound WHERE label = ?1 AND endpoint = ?2
                    AND state = 'accepted' AND updated_at_ms > ?3
                UNION ALL SELECT 1 FROM collected WHERE label = ?1 AND endpoint = ?2
                    AND created_at_ms > ?3)",
            params![label, endpoint, now_ms - OUTBOUND_LIFETIME_MS],
            |row| row.get(0),
        )?)
    }

    /// The outbox epoch of `host_id` that A last collected in, and the
    /// highest `seq` of it that A has stored.
    pub(crate) fn ack_through(&self, host_id: &str) -> Result<(String, i64), StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT epoch, seq FROM cursors WHERE host_id = ?1",
                params![host_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .unwrap_or_default())
    }

    /// B's outbox epoch, which A's acks name.
    pub(crate) fn outbox_epoch(&self) -> Result<String, StoreError> {
        Ok(self
            .connection
            .query_row("SELECT epoch FROM outbox_epoch", [], |row| row.get(0))?)
    }

    /// The space and thread of the A message `message_id`, when `endpoint`
    /// sent it to `label` and the server `host_id` accepted it there, so B
    /// can answer only what was sent to it.
    pub(crate) fn sent_from(
        &self,
        message_id: &str,
        label: &str,
        host_id: &str,
        endpoint: &str,
    ) -> Result<Option<(String, String)>, StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT space, thread FROM outbound WHERE message_id = ?1 AND label = ?2
                    AND host_id = ?3 AND endpoint = ?4 AND state = 'accepted' LIMIT 1",
                params![message_id, label, host_id, endpoint],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?)
    }

    pub(crate) fn record_collected(&self, collected: &Collected<'_>) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT OR IGNORE INTO collected (host_id, epoch, seq, label, endpoint, space,
                message_id, remote_message_id, remote_alias, thread, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                collected.host_id,
                collected.epoch,
                collected.seq,
                collected.label,
                collected.endpoint,
                collected.space,
                collected.message_id,
                collected.remote_message_id,
                collected.remote_alias,
                collected.thread,
                collected.now_ms
            ],
        )?;
        self.connection.execute(
            "INSERT INTO cursors (host_id, epoch, seq) VALUES (?1, ?2, ?3)
             ON CONFLICT (host_id) DO UPDATE SET seq = CASE
                WHEN epoch = excluded.epoch THEN MAX(seq, excluded.seq) ELSE excluded.seq END,
                epoch = excluded.epoch",
            params![collected.host_id, collected.epoch, collected.seq],
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
    /// item and `inbound` key older than theirs and each reply A collected
    /// as long ago, and counts the dropped outbox items per remote sender.
    /// `cursors` keeps what A acks.
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
            "INSERT INTO outbox_expired (sender, count)
             SELECT COALESCE((SELECT sender || '@' || display_host FROM remote_threads t
                    WHERE t.host_id = o.host_id LIMIT 1), o.host_id), COUNT(*)
             FROM outbox o WHERE created_at_ms <= ?1 GROUP BY 1
             ON CONFLICT (sender) DO UPDATE SET count = count + excluded.count",
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
        transaction.execute(
            "DELETE FROM collected WHERE created_at_ms <= ?1",
            params![outbox_limit],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
