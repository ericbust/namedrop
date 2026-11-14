-- VERSION 2 schema 74a144fe

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
    held_full INTEGER NOT NULL DEFAULT 0,
    cannot_reply INTEGER NOT NULL DEFAULT 0,
    other_label_at_ms INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (endpoint, label)
);
CREATE TABLE dropped (
    endpoint TEXT NOT NULL,
    host_id TEXT NOT NULL,
    remote_message_id TEXT NOT NULL,
    label TEXT NOT NULL,
    space TEXT NOT NULL,
    re TEXT NOT NULL,
    remote_alias TEXT NOT NULL,
    count INTEGER NOT NULL,
    reason TEXT,
    created_at_ms INTEGER NOT NULL,
    told INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (endpoint, host_id, remote_message_id)
);

