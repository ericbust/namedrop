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
/// A keeps an item it is done with this long, which outlasts a reply B
/// keeps for a week after a week of quiet.
pub(crate) const DONE_LIFETIME_MS: i64 = 2 * OUTBOX_LIFETIME_MS;
/// A holds at most this many replies of one server for one endpoint. B
/// stops its own agents after 10 replies in a thread with no human turn,
/// so an honest B sends far fewer while A's thread is paused.
pub(crate) const HELD_LIMIT: i64 = 100;
/// B keeps at most this many replies for one A host. One more is counted
/// with the expired replies, as one that can never reach A.
pub(crate) const OUTBOX_LIMIT: i64 = 10_000;

/// A holds `peers`, `outbound`, `collected`, `cursors`, the last `seq`
/// each local endpoint took from each B and the outbox epoch it counts in,
/// because each endpoint reads its own outbox on B, `held`, the replies
/// the Automatic Reply Limit kept out of their threads, and `conditions`,
/// what Doctor reports for each label. B holds
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
    endpoint TEXT NOT NULL,
    host_id TEXT NOT NULL,
    epoch TEXT NOT NULL,
    seq INTEGER NOT NULL,
    label TEXT NOT NULL,
    space TEXT NOT NULL,
    message_id TEXT NOT NULL,
    remote_message_id TEXT NOT NULL,
    remote_alias TEXT NOT NULL,
    thread TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (endpoint, host_id, epoch, seq)
);
CREATE INDEX collected_by_message ON collected (endpoint, message_id);
CREATE TABLE cursors (
    endpoint TEXT NOT NULL,
    host_id TEXT NOT NULL,
    epoch TEXT NOT NULL,
    seq INTEGER NOT NULL,
    PRIMARY KEY (endpoint, host_id)
);
CREATE TABLE held (
    endpoint TEXT NOT NULL,
    host_id TEXT NOT NULL,
    remote_message_id TEXT NOT NULL,
    epoch TEXT NOT NULL,
    seq INTEGER NOT NULL,
    label TEXT NOT NULL,
    space TEXT NOT NULL,
    thread TEXT NOT NULL,
    re TEXT NOT NULL,
    remote_alias TEXT NOT NULL,
    text TEXT NOT NULL,
    told INTEGER NOT NULL,
    created_at_ms INTEGER NOT NULL,
    PRIMARY KEY (endpoint, host_id, remote_message_id)
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
    used_at_ms INTEGER NOT NULL,
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
CREATE TABLE conditions (
    endpoint TEXT NOT NULL,
    label TEXT NOT NULL,
    other_label INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (endpoint, label)
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

/// A reply A took from the server `host_id` for `endpoint` and holds,
/// because the Automatic Reply Limit paused the A thread it answers.
#[derive(Clone, Debug)]
pub(crate) struct Held {
    pub endpoint: String,
    pub host_id: String,
    pub epoch: String,
    pub label: String,
    pub space: String,
    pub thread: String,
    pub reply: Reply,
}

/// Where a reply B made up goes on A.
pub(crate) enum Answers {
    /// To the thread of `space` that the label sent its message from.
    Sent {
        label: String,
        space: String,
        thread: String,
    },
    /// Nowhere: its message was sent to another host, which this server
    /// cannot answer for.
    OtherLabel,
    /// Nowhere: A sent this server no such message.
    Unknown,
}

/// What Doctor reports on A for one label: how many replies its worker
/// dropped because they answered a message sent to another host.
#[derive(Debug, Eq, PartialEq)]
pub(crate) struct Conditions {
    pub label: String,
    pub other_label: i64,
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
    /// expiry, for a reader that looks often. A state directory that is gone
    /// is an error, and is not made again.
    pub(crate) fn read(state: &Path) -> Result<Self, StoreError> {
        if !state.is_dir() {
            return Err(StoreError::invalid_input(format!(
                "{} is not a directory",
                state.display()
            )));
        }
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

    /// Passes `endpoint`'s capture cursor to `rowid`, and counts with the
    /// expired replies, in the same transaction, one reply for each remote
    /// sender of `unkept`, whose reply from that message B could not keep.
    pub(crate) fn captured(
        &self,
        endpoint: &str,
        rowid: i64,
        unkept: &[String],
    ) -> Result<(), StoreError> {
        let transaction = self.connection.unchecked_transaction()?;
        for display in unkept {
            transaction.execute(
                "INSERT INTO outbox_expired (sender, count) VALUES (?1, 1)
                 ON CONFLICT (sender) DO UPDATE SET count = count + 1",
                params![display],
            )?;
        }
        transaction.execute(
            "INSERT INTO capture (endpoint, rowid) VALUES (?1, ?2)
             ON CONFLICT (endpoint) DO UPDATE SET rowid = MAX(rowid, excluded.rowid)",
            params![endpoint, rowid],
        )?;
        transaction.commit()?;
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
    /// reply, and says whether it is kept, which it is not while B keeps
    /// `OUTBOX_LIMIT` replies for `host_id`.
    pub(crate) fn keep_reply(
        &self,
        host_id: &str,
        reply: &Reply,
        now_ms: i64,
    ) -> Result<bool, StoreError> {
        self.connection.execute(
            "INSERT OR IGNORE INTO outbox (host_id, re, thread, sender, message_id, text,
                created_at_ms)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7
             WHERE (SELECT COUNT(*) FROM outbox WHERE host_id = ?1) < ?8",
            params![
                host_id,
                reply.re,
                reply.thread,
                reply.from,
                reply.message_id,
                super::wire::cut(&reply.text, super::wire::TEXT_LIMIT),
                now_ms,
                OUTBOX_LIMIT
            ],
        )?;
        Ok(self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM outbox WHERE host_id = ?1 AND message_id = ?2)",
            params![host_id, reply.message_id],
            |row| row.get(0),
        )?)
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
                   AND remote_alias = ?6
                 ORDER BY message_id = ?4 DESC, created_at_ms DESC, seq DESC LIMIT 1",
                params![endpoint, space, thread, in_reply_to, label, alias],
                |row| row.get(0),
            )
            .optional()?)
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

    /// The outbox epoch of `host_id` that `endpoint` last collected in, and
    /// the highest `seq` of it that `endpoint` has taken.
    pub(crate) fn ack_through(
        &self,
        endpoint: &str,
        host_id: &str,
    ) -> Result<(String, i64), StoreError> {
        Ok(self
            .connection
            .query_row(
                "SELECT epoch, seq FROM cursors WHERE endpoint = ?1 AND host_id = ?2",
                params![endpoint, host_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?
            .unwrap_or_default())
    }

    /// Records that `endpoint` has taken B's `seq` of `epoch` from
    /// `host_id`, so its next call acks it. A new epoch starts the count
    /// again.
    pub(crate) fn advance(
        &self,
        endpoint: &str,
        host_id: &str,
        epoch: &str,
        seq: i64,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO cursors (endpoint, host_id, epoch, seq) VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (endpoint, host_id) DO UPDATE SET seq = CASE
                WHEN epoch = excluded.epoch THEN MAX(seq, excluded.seq) ELSE excluded.seq END,
                epoch = excluded.epoch",
            params![endpoint, host_id, epoch, seq],
        )?;
        Ok(())
    }

    /// Holds `held` for its thread, cut as A would insert it, and says
    /// whether it is held, which it is not when the server already has
    /// `HELD_LIMIT` replies held for this endpoint.
    pub(crate) fn hold(&self, held: &Held, now_ms: i64) -> Result<bool, StoreError> {
        let changed = self.connection.execute(
            "INSERT OR IGNORE INTO held (endpoint, host_id, remote_message_id, epoch, seq,
                label, space, thread, re, remote_alias, text, told, created_at_ms)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, ?12
             WHERE (SELECT COUNT(*) FROM held WHERE endpoint = ?1 AND host_id = ?2) < ?13",
            params![
                held.endpoint,
                held.host_id,
                held.reply.message_id,
                held.epoch,
                held.reply.seq,
                held.label,
                held.space,
                held.thread,
                held.reply.re,
                held.reply.from,
                super::wire::cut(&held.reply.text, super::wire::TEXT_LIMIT),
                now_ms,
                HELD_LIMIT
            ],
        )?;
        Ok(changed == 1
            || self.connection.query_row(
                "SELECT EXISTS (SELECT 1 FROM held WHERE endpoint = ?1 AND host_id = ?2
                    AND remote_message_id = ?3)",
                params![held.endpoint, held.host_id, held.reply.message_id],
                |row| row.get(0),
            )?)
    }

    /// Whether `endpoint` holds a reply for A's `thread` in `space`, so a
    /// new one waits behind it.
    pub(crate) fn holding(
        &self,
        endpoint: &str,
        space: &str,
        thread: &str,
    ) -> Result<bool, StoreError> {
        Ok(self.connection.query_row(
            "SELECT EXISTS (SELECT 1 FROM held WHERE endpoint = ?1 AND space = ?2
                AND thread = ?3)",
            params![endpoint, space, thread],
            |row| row.get(0),
        )?)
    }

    /// The replies `endpoint` holds, oldest first, or with `untold` only the
    /// oldest of each thread, when its sender has not been told about it.
    /// So a thread is told once each time the limit pauses it.
    pub(crate) fn held(&self, endpoint: &str, untold: bool) -> Result<Vec<Held>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT host_id, epoch, label, space, thread, seq, re, remote_alias,
                remote_message_id, text FROM held h
             WHERE endpoint = ?1 AND (?2 = 0 OR (told = 0 AND rowid = (SELECT MIN(rowid)
                FROM held o WHERE o.endpoint = h.endpoint AND o.space = h.space
                    AND o.thread = h.thread)))
             ORDER BY rowid",
        )?;
        let held = statement
            .query_map(params![endpoint, untold], |row| {
                Ok(Held {
                    endpoint: endpoint.to_owned(),
                    host_id: row.get(0)?,
                    epoch: row.get(1)?,
                    label: row.get(2)?,
                    space: row.get(3)?,
                    thread: row.get(4)?,
                    reply: Reply {
                        seq: row.get(5)?,
                        re: row.get(6)?,
                        from: row.get(7)?,
                        message_id: row.get(8)?,
                        text: row.get(9)?,
                        thread: String::new(),
                    },
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(held)
    }

    /// Records that the sender of `held`'s thread was told its replies are
    /// held.
    pub(crate) fn told_held(&self, held: &Held) -> Result<(), StoreError> {
        self.connection.execute(
            "UPDATE held SET told = 1
             WHERE endpoint = ?1 AND host_id = ?2 AND remote_message_id = ?3",
            params![held.endpoint, held.host_id, held.reply.message_id],
        )?;
        Ok(())
    }

    /// Lets go of `held`, which its thread took or can never take.
    pub(crate) fn release(&self, held: &Held) -> Result<(), StoreError> {
        self.connection.execute(
            "DELETE FROM held WHERE endpoint = ?1 AND host_id = ?2 AND remote_message_id = ?3",
            params![held.endpoint, held.host_id, held.reply.message_id],
        )?;
        Ok(())
    }

    /// B's outbox epoch, which A's acks name.
    pub(crate) fn outbox_epoch(&self) -> Result<String, StoreError> {
        Ok(self
            .connection
            .query_row("SELECT epoch FROM outbox_epoch", [], |row| row.get(0))?)
    }

    /// Where a reply to the A message `message_id` goes, when `endpoint`
    /// sent it to one of `labels` for the server `host_id`, which accepted
    /// it, so B answers only what was sent to it. `labels` are the
    /// collecting worker's own, first, and those that dial its addresses,
    /// which share its server's outbox, so a reply goes to the label its
    /// message was sent by: the one sent to `from` first, and then the
    /// worker's own.
    pub(crate) fn sent_from(
        &self,
        message_id: &str,
        host_id: &str,
        endpoint: &str,
        from: &str,
        labels: &[String],
    ) -> Result<Answers, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT label, recipient, space, thread, host_id IS ?3 FROM outbound
             WHERE message_id = ?1 AND endpoint = ?2 AND state = 'accepted'",
        )?;
        let sent: Vec<(String, String, String, String, bool)> = statement
            .query_map(params![message_id, endpoint, host_id], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        let best = sent
            .iter()
            .filter(|(label, _, _, _, pinned)| *pinned && labels.contains(label))
            .max_by_key(|(label, recipient, ..)| (recipient == from, *label == labels[0]));
        Ok(match best {
            Some((label, _, space, thread, _)) => Answers::Sent {
                label: label.clone(),
                space: space.clone(),
                thread: thread.clone(),
            },
            None if sent.iter().any(|(label, ..)| !labels.contains(label)) => Answers::OtherLabel,
            None => Answers::Unknown,
        })
    }

    /// Counts a reply that `endpoint`'s worker for `label` collected and
    /// dropped because it answered a message sent to another host.
    pub(crate) fn count_other_label(&self, endpoint: &str, label: &str) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO conditions (endpoint, label, other_label) VALUES (?1, ?2, 1)
             ON CONFLICT (endpoint, label) DO UPDATE SET other_label = other_label + 1",
            params![endpoint, label],
        )?;
        Ok(())
    }

    /// What `endpoint`'s Doctor reports for each label.
    pub(crate) fn conditions(&self, endpoint: &str) -> Result<Vec<Conditions>, StoreError> {
        let mut statement = self.connection.prepare(
            "SELECT label, other_label FROM conditions
             WHERE endpoint = ?1 AND other_label > 0 ORDER BY label",
        )?;
        let conditions = statement
            .query_map(params![endpoint], |row| {
                Ok(Conditions {
                    label: row.get(0)?,
                    other_label: row.get(1)?,
                })
            })?
            .collect::<Result<_, _>>()?;
        Ok(conditions)
    }

    /// Records a reply A inserted as `collected.message_id`, unless that A
    /// message is already recorded, and says whether it was not.
    pub(crate) fn record_collected(&self, collected: &Collected<'_>) -> Result<bool, StoreError> {
        let recorded = self.connection.execute(
            "INSERT OR IGNORE INTO collected (endpoint, host_id, epoch, seq, label, space,
                message_id, remote_message_id, remote_alias, thread, created_at_ms)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11
             WHERE NOT EXISTS (SELECT 1 FROM collected WHERE endpoint = ?1
                AND message_id = ?7)",
            params![
                collected.endpoint,
                collected.host_id,
                collected.epoch,
                collected.seq,
                collected.label,
                collected.space,
                collected.message_id,
                collected.remote_message_id,
                collected.remote_alias,
                collected.thread,
                collected.now_ms
            ],
        )?;
        Ok(recorded == 1)
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

    /// Records B's `local_thread` for A's thread, or that the thread it
    /// already has was used at `now_ms`.
    pub(crate) fn record_remote_thread(
        &self,
        thread: &RemoteThread<'_>,
        local_thread: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "INSERT INTO remote_threads (host_id, thread, endpoint, space, local_thread,
                sender, display_host, used_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (host_id, thread, space) DO UPDATE SET used_at_ms = excluded.used_at_ms",
            params![
                thread.host_id,
                thread.thread,
                thread.endpoint,
                thread.space,
                local_thread,
                thread.sender,
                thread.display_host,
                now_ms
            ],
        )?;
        Ok(())
    }

    /// Records that B's `local_thread` in `space` carried a reply at
    /// `now_ms`.
    pub(crate) fn touch_thread(
        &self,
        endpoint: &str,
        space: &str,
        local_thread: &str,
        now_ms: i64,
    ) -> Result<(), StoreError> {
        self.connection.execute(
            "UPDATE remote_threads SET used_at_ms = ?4
             WHERE endpoint = ?1 AND space = ?2 AND local_thread = ?3",
            params![endpoint, space, local_thread, now_ms],
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
    /// item and `inbound` key older than theirs, each reply A collected or
    /// held as long ago, each B thread mapping unused as long, and each item A was
    /// done with two weeks ago, and counts the dropped outbox items per
    /// remote sender. `cursors` keeps what A acks.
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
        transaction.execute(
            "DELETE FROM held WHERE created_at_ms <= ?1",
            params![outbox_limit],
        )?;
        transaction.execute(
            "DELETE FROM remote_threads WHERE used_at_ms <= ?1",
            params![outbox_limit],
        )?;
        transaction.execute(
            "DELETE FROM outbound WHERE state <> 'queued' AND updated_at_ms <= ?1",
            params![now_ms - DONE_LIFETIME_MS],
        )?;
        transaction.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests;
