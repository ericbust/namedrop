use super::*;
use crate::StateStore;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

const NOW: i64 = 1_800_000_000_000;

struct State(PathBuf);

impl State {
    /// A state directory whose main database already exists, as every
    /// relay's does.
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "namedrop-relay-store-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        StateStore::open(&path).unwrap();
        Self(path)
    }

    fn open(&self, now_ms: i64) -> RelayStore {
        let store = RelayStore::open(&self.0, now_ms).unwrap();
        let main = Connection::open(self.0.join("namedrop.sqlite3")).unwrap();
        let version: u32 = main
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, 8, "the main database keeps its schema");
        store
    }
}

impl Drop for State {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn count(store: &RelayStore, sql: &str) -> i64 {
    store
        .connection
        .query_row(sql, [], |row| row.get(0))
        .unwrap()
}

fn queue(store: &RelayStore, message: &str, created_at_ms: i64) {
    store
        .connection
        .execute(
            "INSERT INTO outbound (label, message_id, recipient, endpoint, space, sender,
                thread, text, state, created_at_ms, updated_at_ms)
             VALUES ('devbox', ?1, 'bob', 'rex:/tmp/a.sock', 'session:a', 'alice',
                'thread-1', 'hi', 'queued', ?2, ?2)",
            params![message, created_at_ms],
        )
        .unwrap();
}

fn reply(store: &RelayStore, host_id: &str, message: &str, created_at_ms: i64) {
    store
        .connection
        .execute(
            "INSERT INTO outbox (host_id, re, thread, sender, message_id, text, created_at_ms)
             VALUES (?1, 'message-a', 'thread-a', 'bob', ?2, 'done', ?3)",
            params![host_id, message, created_at_ms],
        )
        .unwrap();
}

fn key(store: &RelayStore, message: &str, created_at_ms: i64) {
    store
        .connection
        .execute(
            "INSERT INTO inbound (host_id, message_id, recipient, outcome, created_at_ms)
             VALUES ('host-a', ?1, 'bob', '{\"status\":\"accepted\"}', ?2)",
            params![message, created_at_ms],
        )
        .unwrap();
}

#[test]
fn the_relay_database_is_created_from_nothing_and_reopened_with_its_rows() {
    let state = State::new();
    assert!(!state.0.join("relay.sqlite3").exists());
    let store = state.open(NOW);
    let version: u32 = store
        .connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 1);
    for table in [
        "peers",
        "outbound",
        "collected",
        "inbound",
        "outbox",
        "remote_threads",
        "outbox_expired",
    ] {
        assert_eq!(count(&store, &format!("SELECT COUNT(*) FROM {table}")), 0);
    }
    queue(&store, "message-1", NOW);
    drop(store);

    let store = state.open(NOW);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM outbound"), 1);
    let mode = fs::metadata(state.0.join("relay.sqlite3"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
}

#[test]
fn a_newer_relay_schema_is_refused() {
    let state = State::new();
    drop(state.open(NOW));
    Connection::open(state.0.join("relay.sqlite3"))
        .unwrap()
        .pragma_update(None, "user_version", 2)
        .unwrap();
    let Err(error) = RelayStore::open(&state.0, NOW) else {
        panic!("a newer schema opened");
    };
    assert!(error.to_string().contains("relay schema 2"), "{error}");
}

#[test]
fn a_queued_item_expires_24_hours_after_it_was_queued() {
    let state = State::new();
    let store = state.open(NOW);
    queue(&store, "old", NOW - OUTBOUND_LIFETIME_MS);
    queue(&store, "young", NOW - OUTBOUND_LIFETIME_MS + 1);
    store
        .connection
        .execute(
            "INSERT INTO outbound (label, message_id, recipient, endpoint, space, sender,
                thread, text, state, created_at_ms, updated_at_ms)
             VALUES ('devbox', 'done', 'bob', 'rex:/tmp/a.sock', 'session:a', 'alice',
                'thread-1', 'hi', 'accepted', 0, ?1)",
            [NOW],
        )
        .unwrap();
    drop(store);

    let store = state.open(NOW);
    let states: Vec<(String, String, Option<String>)> = store
        .connection
        .prepare("SELECT message_id, state, reason FROM outbound ORDER BY message_id")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        states,
        [
            ("done".to_owned(), "accepted".to_owned(), None),
            (
                "old".to_owned(),
                "expired".to_owned(),
                Some("devbox was unreachable for 24 hours".to_owned())
            ),
            ("young".to_owned(), "queued".to_owned(), None),
        ]
    );
}

#[test]
fn an_outbox_item_expires_after_7_days_and_is_counted_per_remote_sender() {
    let state = State::new();
    let store = state.open(NOW);
    reply(&store, "host-a", "m1", NOW - OUTBOX_LIFETIME_MS);
    reply(&store, "host-a", "m2", NOW - OUTBOX_LIFETIME_MS - 5);
    reply(&store, "host-a", "m3", NOW - OUTBOX_LIFETIME_MS + 1);
    reply(&store, "host-c", "m4", NOW - OUTBOX_LIFETIME_MS);
    drop(store);

    let store = state.open(NOW);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM outbox"), 1);
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM outbox WHERE message_id = 'm3'"
        ),
        1
    );
    let tally = |store: &RelayStore| -> Vec<(String, i64)> {
        store
            .connection
            .prepare("SELECT sender, count FROM outbox_expired ORDER BY sender")
            .unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(
        tally(&store),
        [("host-a".to_owned(), 2), ("host-c".to_owned(), 1)]
    );
    reply(&store, "host-a", "m5", NOW - OUTBOX_LIFETIME_MS);
    drop(store);

    let store = state.open(NOW);
    assert_eq!(
        tally(&store),
        [("host-a".to_owned(), 3), ("host-c".to_owned(), 1)]
    );
}

#[test]
fn an_inbound_key_expires_after_7_days() {
    let state = State::new();
    let store = state.open(NOW);
    key(&store, "old", NOW - INBOUND_LIFETIME_MS);
    key(&store, "young", NOW - INBOUND_LIFETIME_MS + 1);
    drop(store);

    let store = state.open(NOW);
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM inbound WHERE message_id = 'young'"
        ),
        1
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM inbound"), 1);
}

#[test]
fn a_collected_reply_is_dropped_after_7_days_and_its_cursor_stays() {
    let state = State::new();
    let store = state.open(NOW);
    for (seq, at) in [
        (1, NOW - OUTBOX_LIFETIME_MS),
        (2, NOW - OUTBOX_LIFETIME_MS + 1),
    ] {
        store
            .record_collected(&Collected {
                host_id: "host-b",
                epoch: "epoch-1",
                seq,
                label: "devbox",
                endpoint: "rex:/tmp/a.sock",
                space: "session:a",
                message_id: "",
                remote_message_id: "",
                remote_alias: "",
                thread: "",
                now_ms: at,
            })
            .unwrap();
        store
            .advance("rex:/tmp/a.sock", "host-b", "epoch-1", seq)
            .unwrap();
    }
    drop(store);

    let store = state.open(NOW);
    assert_eq!(count(&store, "SELECT seq FROM collected"), 2);
    assert_eq!(
        store.ack_through("rex:/tmp/a.sock", "host-b").unwrap(),
        ("epoch-1".to_owned(), 2)
    );
}

#[test]
fn a_done_item_is_dropped_after_14_days_and_a_thread_mapping_after_7_unused() {
    assert_eq!(DONE_LIFETIME_MS, 14 * 24 * 60 * 60 * 1000);
    let state = State::new();
    let store = state.open(NOW);
    for (message, item_state, at) in [
        ("old-accepted", "accepted", NOW - DONE_LIFETIME_MS),
        ("old-failed", "failed", NOW - DONE_LIFETIME_MS),
        ("young-accepted", "accepted", NOW - DONE_LIFETIME_MS + 1),
    ] {
        store
            .connection
            .execute(
                "INSERT INTO outbound (label, message_id, recipient, endpoint, space, sender,
                    thread, text, state, created_at_ms, updated_at_ms)
                 VALUES ('devbox', ?1, 'bob', 'rex:/tmp/a.sock', 'session:a', 'alice',
                    'thread-1', 'hi', ?2, ?3, ?3)",
                params![message, item_state, at],
            )
            .unwrap();
    }
    for (thread, at) in [
        ("old", NOW - OUTBOX_LIFETIME_MS),
        ("young", NOW - OUTBOX_LIFETIME_MS + 1),
    ] {
        store
            .record_remote_thread(
                &RemoteThread {
                    host_id: "host-a",
                    thread,
                    endpoint: "rex:/tmp/b.sock",
                    space: "session:b",
                    sender: "alice",
                    display_host: "laptop",
                },
                &format!("b-{thread}"),
                at,
            )
            .unwrap();
    }
    drop(store);

    let store = state.open(NOW);
    let left = |sql: &str| -> Vec<String> {
        store
            .connection
            .prepare(sql)
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert_eq!(left("SELECT message_id FROM outbound"), ["young-accepted"]);
    assert_eq!(left("SELECT thread FROM remote_threads"), ["young"]);
    store
        .touch_thread(
            "rex:/tmp/b.sock",
            "session:b",
            "b-young",
            NOW + OUTBOX_LIFETIME_MS,
        )
        .unwrap();
    drop(store);
    let store = state.open(NOW + OUTBOX_LIFETIME_MS + 2);
    assert_eq!(
        count(&store, "SELECT COUNT(*) FROM remote_threads"),
        1,
        "a used mapping stays"
    );
}
