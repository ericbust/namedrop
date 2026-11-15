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
    assert_eq!(version, VERSION);
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
        .pragma_update(None, "user_version", VERSION + 1)
        .unwrap();
    let Err(error) = RelayStore::open(&state.0, NOW) else {
        panic!("a newer schema opened");
    };
    assert!(
        error
            .to_string()
            .contains(&format!("relay schema {}", VERSION + 1)),
        "{error}"
    );
}

/// A version 1 database, as the first relay made, opens as this version
/// with its rows, and takes what versions 2, 3 and 4 add.
#[test]
fn a_version_1_relay_database_is_brought_to_this_version_with_its_rows() {
    let state = State::new();
    state.made_by(VERSION_1_LAYOUTS[0].1, 1);
    Connection::open(state.0.join("relay.sqlite3"))
        .unwrap()
        .execute_batch(
            "INSERT INTO outbound (label, message_id, recipient, endpoint, space, sender,
                thread, text, state, created_at_ms, updated_at_ms)
             VALUES ('devbox', 'message-1', 'bob', 'rex:/tmp/a.sock', 'session:a', 'alice',
                'thread-1', 'hi', 'queued', 1800000000000, 1800000000000);
             INSERT INTO conditions (endpoint, label, other_label)
             VALUES ('rex:/tmp/a.sock', 'devbox', 1);",
        )
        .unwrap();
    let store = state.open(NOW);
    let version: u32 = store
        .connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, VERSION);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM outbound"), 1);
    store
        .count_other_label("rex:/tmp/a.sock", "devbox", NOW)
        .unwrap();
    assert_eq!(
        store.conditions("rex:/tmp/a.sock", NOW).unwrap()[0].other_label,
        1,
        "a count from before the version has no time, so it starts again"
    );
}

/// Opens that race on a database still to be made, on one at version 1,
/// or on one of an unreleased layout, wait their turn: each succeeds, and
/// the database is made, brought to this version, or set aside once.
#[test]
fn opens_that_race_to_make_or_migrate_the_database_each_succeed() {
    for version in [0, 1, 99] {
        for _ in 0..10 {
            let state = State::new();
            if version == 99 {
                state.made_by(VERSION_1_LAYOUTS[1].1, 1);
            }
            if version == 1 {
                state.made_by(VERSION_1_LAYOUTS[0].1, 1);
            }
            let barrier = std::sync::Barrier::new(8);
            let failed: Vec<String> = std::thread::scope(|scope| {
                let opens: Vec<_> = (0..8)
                    .map(|_| {
                        scope.spawn(|| {
                            barrier.wait();
                            RelayStore::read(&state.0)
                                .err()
                                .map(|error| error.to_string())
                        })
                    })
                    .collect();
                opens
                    .into_iter()
                    .filter_map(|open| open.join().unwrap())
                    .collect()
            });
            assert!(failed.is_empty(), "version {version}: {failed:?}");
            let store = state.open(NOW);
            let now: u32 = store
                .connection
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(now, VERSION);
            assert_eq!(count(&store, "SELECT COUNT(*) FROM outbox_epoch"), 1);
            assert_eq!(
                state.set_aside().len(),
                usize::from(version == 99),
                "an unreleased layout is set aside once"
            );
        }
    }
}

/// The 12 version 1 layouts the relay's unreleased builds made, each named
/// for a commit that made it, newest first. Only the newest is version 2
/// less what `TO_2` adds.
const VERSION_1_LAYOUTS: [(&str, &str); 12] = [
    ("1dbdfbd0", include_str!("fixtures/v1-1dbdfbd0.sql")),
    ("e4da0d71", include_str!("fixtures/v1-e4da0d71.sql")),
    ("d09d95ff", include_str!("fixtures/v1-d09d95ff.sql")),
    ("7da6330e", include_str!("fixtures/v1-7da6330e.sql")),
    ("8fe7e2e6", include_str!("fixtures/v1-8fe7e2e6.sql")),
    ("b47fc24a", include_str!("fixtures/v1-b47fc24a.sql")),
    ("7c7c897f", include_str!("fixtures/v1-7c7c897f.sql")),
    ("233aa8fb", include_str!("fixtures/v1-233aa8fb.sql")),
    ("1f0a8049", include_str!("fixtures/v1-1f0a8049.sql")),
    ("4e324deb", include_str!("fixtures/v1-4e324deb.sql")),
    ("79cbe1a7", include_str!("fixtures/v1-79cbe1a7.sql")),
    ("20ab6818", include_str!("fixtures/v1-20ab6818.sql")),
];

impl State {
    /// A relay database made by `schema`, stamped `version`, with one row
    /// in `peers`, which every layout has, and its outbox epoch when the
    /// layout has one.
    fn made_by(&self, schema: &str, version: u32) {
        let made = Connection::open(self.0.join("relay.sqlite3")).unwrap();
        made.execute_batch(schema).unwrap();
        made.execute(
            "INSERT INTO peers (label, host_id, contacted_at_ms) VALUES ('devbox', 'host-b', 1)",
            [],
        )
        .unwrap();
        let epochs: bool = made
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE name = 'outbox_epoch')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        if epochs {
            made.execute("INSERT INTO outbox_epoch (epoch) VALUES ('e0')", [])
                .unwrap();
        }
        made.pragma_update(None, "user_version", version).unwrap();
    }

    fn set_aside(&self) -> Vec<String> {
        set_aside_in(&self.0)
    }
}

fn version(store: &RelayStore) -> u32 {
    store
        .connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap()
}

/// Every call that reads a column some earlier layout lacked works.
fn usable(store: &RelayStore) {
    let endpoint = "rex:/tmp/a.sock";
    store.count_other_label(endpoint, "devbox", NOW).unwrap();
    store.conditions(endpoint, NOW).unwrap();
    store.unsent(endpoint).unwrap();
    store.dropped(endpoint).unwrap();
    store.held(endpoint, true).unwrap();
    store.outbox_epoch().unwrap();
    store.outbox_page("host-a", 0, 0, 1024).unwrap();
    store.expired_replies().unwrap();
    store.lost_for("host-a").unwrap();
    store.lost_untold(endpoint).unwrap();
}

/// Each version 1 layout an unreleased build made opens. The newest is
/// brought to this version in place with its rows. Each other one is set
/// aside, with its rows, and a new database is made, which every call can
/// use.
#[test]
fn each_version_1_layout_is_migrated_or_set_aside() {
    for (made_at, schema) in VERSION_1_LAYOUTS {
        let state = State::new();
        state.made_by(schema, 1);
        let store = state.open(NOW);
        assert_eq!(version(&store), VERSION, "{made_at}");
        assert_eq!(
            layout(&store.connection).unwrap(),
            *current_layout(),
            "{made_at}"
        );
        usable(&store);
        let peers = count(&store, "SELECT COUNT(*) FROM peers");
        let aside = state.set_aside();
        if made_at == "1dbdfbd0" {
            assert!(aside.is_empty(), "{made_at}: {aside:?}");
            assert_eq!(peers, 1, "{made_at} keeps its rows");
        } else {
            assert_eq!(aside.len(), 1, "{made_at}: {aside:?}");
            assert_eq!(peers, 0, "{made_at}");
            assert_eq!(count(&store, "SELECT COUNT(*) FROM outbox_epoch"), 1);
            let kept = Connection::open(state.0.join(&aside[0])).unwrap();
            let kept_peers: i64 = kept
                .query_row("SELECT COUNT(*) FROM peers", [], |row| row.get(0))
                .unwrap();
            assert_eq!(kept_peers, 1, "{made_at} is kept as it was");
        }
        drop(store);
        let store = state.open(NOW);
        assert_eq!(
            state.set_aside().len(),
            aside.len(),
            "{made_at} opens again as is"
        );
        usable(&store);
    }
}

/// A database of the `e4da0d71` era that a later build brought to version
/// 2 still lacks columns, so its layout, not its version, sets it aside.
#[test]
fn a_version_2_database_that_lacks_columns_is_set_aside() {
    let state = State::new();
    state.made_by(&format!("{}{TO_2}", VERSION_1_LAYOUTS[1].1), 2);
    let store = state.open(NOW);
    assert_eq!(state.set_aside().len(), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM peers"), 0);
    usable(&store);
}

/// A version 2 database, as the relay of `329c278b` made, is brought to
/// this version in place with its rows. Each cursor and reply is of the
/// first label that last found its server's `host_id`, or of its own label
/// when none did, and a cursor of no known server is of none.
#[test]
fn a_version_2_database_is_brought_to_this_version_with_its_rows() {
    let state = State::new();
    state.made_by(include_str!("fixtures/v2-6ca19369.sql"), 2);
    Connection::open(state.0.join("relay.sqlite3"))
        .unwrap()
        .execute_batch(
            "INSERT INTO peers (label, host_id, contacted_at_ms) VALUES ('aardvark', 'host-b', 1);
             INSERT INTO cursors (endpoint, host_id, epoch, seq) VALUES
                ('rex:/tmp/a.sock', 'host-b', 'e1', 7), ('rex:/tmp/a.sock', 'host-x', 'e2', 3);
             INSERT INTO collected (endpoint, host_id, epoch, seq, label, space, message_id,
                remote_message_id, remote_alias, thread, created_at_ms)
                VALUES ('rex:/tmp/a.sock', 'host-b', 'e1', 7, 'devbox', 'session:a', 'm-7',
                    'b-7', 'bob', 'thread-1', 1800000000000);
             INSERT INTO held (endpoint, host_id, remote_message_id, epoch, seq, label, space,
                thread, re, remote_alias, text, told, created_at_ms)
                VALUES ('rex:/tmp/a.sock', 'host-x', 'b-8', 'e2', 8, 'lab', 'session:a',
                    'thread-1', 'm-1', 'bob', 'held', 0, 1800000000000);
             INSERT INTO dropped (endpoint, host_id, remote_message_id, label, space, re,
                remote_alias, count, reason, created_at_ms)
                VALUES ('rex:/tmp/a.sock', 'host-b', 'b-9', 'devbox', 'session:a', 'm-1',
                    'bob', 1, 'refused', 1800000000000);
             INSERT INTO capture (endpoint, rowid) VALUES ('rex:/tmp/a.sock', 42);",
        )
        .unwrap();
    let store = state.open(NOW);
    assert_eq!(version(&store), VERSION);
    assert_eq!(
        store.capture_anchor("rex:/tmp/a.sock").unwrap(),
        Some((42, None)),
        "the cursor stays, with no message to check until the first capture"
    );
    assert_eq!(
        store.capture_marks("rex:/tmp/a.sock").unwrap(),
        [Mark {
            space: String::new(),
            place: Place {
                rowid: 42,
                ..Place::default()
            },
        }],
        "and it is a floor a capture never reads again before"
    );
    assert_eq!(store.capture_floor().unwrap(), Place::default());
    assert!(state.set_aside().is_empty());
    assert_eq!(count(&store, "SELECT COUNT(*) FROM peers"), 2);
    let endpoint = "rex:/tmp/a.sock";
    assert_eq!(
        store.ack_through(endpoint, "aardvark", "host-b").unwrap(),
        ("e1".to_owned(), 7)
    );
    assert_eq!(
        store.ack_through(endpoint, "", "host-x").unwrap(),
        ("e2".to_owned(), 3)
    );
    let peers = |table: &str| -> String {
        store
            .connection
            .query_row(&format!("SELECT peer FROM {table}"), [], |row| row.get(0))
            .unwrap()
    };
    assert_eq!(peers("collected"), "aardvark");
    assert_eq!(peers("held"), "lab");
    assert_eq!(peers("dropped"), "aardvark");
    assert_eq!(store.held(endpoint, false).unwrap()[0].peer, "lab");
    usable(&store);
}

/// A version 3 database, as the relay of `63d2a20a` made, is brought to
/// this version in place with its rows. Its floor and marks have no
/// message id or creation time yet: the first capture fills them in.
#[test]
fn a_version_3_database_is_brought_to_this_version_with_its_rows() {
    let state = State::new();
    state.made_by(include_str!("fixtures/v3-63d2a20a.sql"), 3);
    Connection::open(state.0.join("relay.sqlite3"))
        .unwrap()
        .execute_batch(
            "INSERT INTO capture_start (rowid) VALUES (17);
             INSERT INTO capture (endpoint, rowid, message_id)
                VALUES ('rex:/tmp/a.sock', 42, 'm-42');
             INSERT INTO capture_marks (endpoint, space, rowid, message_id)
                VALUES ('rex:/tmp/a.sock', 'session:a', 40, 'm-40'),
                    ('rex:/tmp/a.sock', '', 20, NULL);",
        )
        .unwrap();
    let store = state.open(NOW);
    assert_eq!(version(&store), VERSION);
    assert!(state.set_aside().is_empty());
    assert_eq!(
        store.capture_floor().unwrap(),
        Place {
            rowid: 17,
            ..Place::default()
        }
    );
    assert_eq!(
        store.capture_anchor("rex:/tmp/a.sock").unwrap(),
        Some((42, Some("m-42".to_owned())))
    );
    let mark = |space: &str, rowid, message_id: Option<&str>| Mark {
        space: space.to_owned(),
        place: Place {
            rowid,
            message_id: message_id.map(str::to_owned),
            created_at_ms: None,
        },
    };
    assert_eq!(
        store.capture_marks("rex:/tmp/a.sock").unwrap(),
        [mark("session:a", 40, Some("m-40")), mark("", 20, None)]
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM peers"), 1);
    usable(&store);
}

/// A migration that finds the disk full returns the error and leaves the
/// database as it was, and the next open migrates it.
#[test]
fn a_full_migration_returns_the_storage_error_and_keeps_the_database() {
    let state = State::new();
    state.made_by(include_str!("fixtures/v2-6ca19369.sql"), 2);
    let path = state.0.join("relay.sqlite3");
    let mut connection = Connection::open(&path).unwrap();
    let pages: i64 = connection
        .pragma_query_value(None, "page_count", |row| row.get(0))
        .unwrap();
    connection
        .pragma_update(None, "max_page_count", pages)
        .unwrap();
    let before = layout(&connection).unwrap();
    let error = upgrade(&mut connection, &path).unwrap_err();
    assert!(error.to_string().contains("full"), "{error}");
    assert_eq!(layout(&connection).unwrap(), before);
    assert_eq!(
        connection
            .pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .unwrap(),
        2
    );
    assert_eq!(
        connection
            .query_row("SELECT COUNT(*) FROM peers", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(state.set_aside().is_empty());
    drop(connection);
    let store = state.open(NOW);
    assert_eq!(version(&store), VERSION);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM peers"), 1);
    assert!(state.set_aside().is_empty());
}

/// A migration whose writes fail with an I/O error, past the file size
/// limit, keeps the database where it is.
#[test]
fn a_migration_io_error_never_sets_the_database_aside() {
    const CHILD_STATE: &str = "NAMEDROP_MIGRATION_IO_STATE";
    if let Some(path) = std::env::var_os(CHILD_STATE) {
        let path = PathBuf::from(path);
        assert!(RelayStore::read(&path).is_err());
        assert!(set_aside_in(&path).is_empty());
        return;
    }
    let state = State::new();
    state.made_by(include_str!("fixtures/v2-6ca19369.sql"), 2);
    let output = std::process::Command::new("/bin/sh")
        .args([
            "-c",
            "ulimit -f 4; trap '' XFSZ; exec \"$1\" --exact \"$2\"",
            "migration-io",
        ])
        .arg(std::env::current_exe().unwrap())
        .arg("relay::store::tests::a_migration_io_error_never_sets_the_database_aside")
        .env(CHILD_STATE, &state.0)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("1 passed"),
        "{stdout}{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(state.set_aside().is_empty());
    let store = state.open(NOW);
    assert_eq!(version(&store), VERSION);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM peers"), 1);
}

/// A database stamped this version whose layout is not this schema's, as
/// an earlier build of the same version made without the page's index, is
/// set aside by its layout.
#[test]
fn a_database_stamped_this_version_with_another_layout_is_set_aside() {
    let state = State::new();
    let store = state.open(NOW);
    queue(&store, "message-1", NOW);
    store
        .connection
        .execute_batch("DROP INDEX outbox_by_seq")
        .unwrap();
    drop(store);
    let store = state.open(NOW);
    assert_eq!(state.set_aside().len(), 1);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM outbound"), 0);
    assert_eq!(layout(&store.connection).unwrap(), *current_layout());
}

/// A capture's transaction takes the relay's write lock from its start, so
/// a capture that meets another relay writer waits for it rather than
/// failing.
#[test]
fn a_capture_waits_for_another_relay_writer() {
    let state = State::new();
    drop(state.open(NOW));
    let writer = Connection::open(state.0.join("relay.sqlite3")).unwrap();
    writer.busy_timeout(Duration::from_secs(5)).unwrap();
    writer
        .execute_batch(
            "BEGIN IMMEDIATE;
             INSERT INTO outbox_expired (sender, count) VALUES ('other', 1);",
        )
        .unwrap();
    let path = state.0.clone();
    let capture = std::thread::spawn(move || {
        let store = RelayStore::read(&path).unwrap();
        store.captured("rex:/tmp/a.sock", 1, ("message", "session:a"), || {
            store
                .connection
                .execute(
                    "INSERT INTO outbox_expired (sender, count) VALUES ('mine', 1)",
                    [],
                )
                .map_err(|error| error.to_string())?;
            Ok((vec!["devbox".to_owned()], Vec::new()))
        })
    });
    std::thread::sleep(Duration::from_millis(300));
    writer.execute_batch("COMMIT").unwrap();
    assert_eq!(capture.join().unwrap(), Ok(vec!["devbox".to_owned()]));
    assert_eq!(
        state.open(NOW).capture_cursor("rex:/tmp/a.sock").unwrap(),
        1
    );
}

/// A database of this version opens again as it is, and its rows stay.
#[test]
fn a_database_of_this_version_opens_again_with_its_rows() {
    let state = State::new();
    queue(&state.open(NOW), "message-1", NOW);
    for _ in 0..2 {
        let store = state.open(NOW);
        assert_eq!(version(&store), VERSION);
        assert_eq!(count(&store, "SELECT COUNT(*) FROM outbound"), 1);
    }
    assert!(state.set_aside().is_empty());
}

/// B counts, for each A message and B alias, the replies it discards for
/// an A host that it keeps 10,000 for and those that expire, and names the
/// latest 32 to that host.
#[test]
fn b_counts_what_it_lets_go_of_per_a_message() {
    let state = State::new();
    let store = state.open(NOW);
    store
        .connection
        .execute_batch(&format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < {OUTBOX_LIMIT})
             INSERT INTO outbox (host_id, re, thread, sender, message_id, text, created_at_ms)
             SELECT 'host-a', 'message-full', 't', 'bob', 'full-' || i, 'x', {NOW} FROM n"
        ))
        .unwrap();
    let reply = |message_id: &str, re: &str| Reply {
        seq: 0,
        re: re.to_owned(),
        thread: "t".to_owned(),
        from: "bob".to_owned(),
        message_id: message_id.to_owned(),
        text: "past the limit".to_owned(),
    };
    for (id, re) in [
        ("b-1", "message-1"),
        ("b-2", "message-1"),
        ("b-3", "message-2"),
    ] {
        assert!(!store.keep_reply("host-a", &reply(id, re), NOW + 1).unwrap());
    }
    assert!(
        store
            .keep_reply("host-c", &reply("c-1", "message-1"), NOW + 1)
            .unwrap()
    );
    let lost = store.lost_for("host-a").unwrap();
    let counts: Vec<_> = lost
        .iter()
        .map(|lost| (lost.re.as_str(), lost.discarded, lost.expired, lost.since))
        .collect();
    assert_eq!(
        counts,
        [("message-1", 2, 0, NOW + 1), ("message-2", 1, 0, NOW + 1)]
    );
    assert!(store.lost_for("host-c").unwrap().is_empty());
    drop(store);
    let later = NOW + OUTBOX_LIFETIME_MS;
    let store = state.open(later);
    let lost = store.lost_for("host-a").unwrap();
    assert_eq!(
        (
            lost[0].re.as_str(),
            lost[0].discarded,
            lost[0].expired,
            lost[0].since
        ),
        ("message-full", 0, OUTBOX_LIMIT, later),
        "the latest loss is named first"
    );
    assert_eq!(lost.len(), 3);
    store
        .connection
        .execute_batch(&format!(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 40)
             INSERT INTO outbox_lost (host_id, re, sender, since_ms, discarded, expired,
                updated_at_ms)
             SELECT 'host-a', 'm-' || i, 'bob', {later}, 1, 0, {later} - i FROM n"
        ))
        .unwrap();
    assert_eq!(store.lost_for("host-a").unwrap().len(), 32);
    drop(store);
    let store = state.open(later + OUTBOX_LIFETIME_MS);
    assert!(
        store.lost_for("host-a").unwrap().is_empty(),
        "a loss B has not added to for 7 days is forgotten"
    );
}

/// A reply to a message sent by two labels of one server goes to the
/// label sent to its agent, and otherwise to the first by name, whichever
/// of them collects it.
#[test]
fn a_reply_answers_one_label_whichever_label_collects_it() {
    let state = State::new();
    let store = state.open(NOW);
    for label in ["studio", "devbox"] {
        store
            .connection
            .execute(
                "INSERT INTO outbound (label, message_id, recipient, endpoint, space, sender,
                    thread, text, state, host_id, created_at_ms, updated_at_ms)
                 VALUES (?1, 'm1', 'bob', 'rex:/tmp/a.sock', 'session:a', 'alice',
                    'thread-1', 'hi', 'accepted', 'host-b', ?2, ?2)",
                params![label, NOW],
            )
            .unwrap();
    }
    let label = |from: &str, labels: [&str; 2]| {
        let labels = labels.map(str::to_owned);
        match store
            .sent_from("m1", "host-b", "rex:/tmp/a.sock", from, &labels)
            .unwrap()
        {
            Answers::Sent { label, .. } => label,
            _ => panic!("not sent"),
        }
    };
    for from in ["bob", "carol"] {
        assert_eq!(label(from, ["studio", "devbox"]), "devbox", "{from}");
        assert_eq!(label(from, ["devbox", "studio"]), "devbox", "{from}");
    }
}

/// A reply from a third agent to a message whose item for one recipient
/// is accepted and for another still queued goes to the accepted one,
/// whichever row comes first, and a recipient's own reply waits for its
/// own item.
#[test]
fn a_reply_answers_the_accepted_item_before_a_queued_one() {
    for order in [["accepted", "queued"], ["queued", "accepted"]] {
        let state = State::new();
        let store = state.open(NOW);
        for (recipient, state) in [("bob", order[0]), ("eve", order[1])] {
            store
                .connection
                .execute(
                    "INSERT INTO outbound (label, message_id, recipient, endpoint, space, sender,
                        thread, text, state, host_id, created_at_ms, updated_at_ms)
                     VALUES ('devbox', 'm1', ?1, 'rex:/tmp/a.sock', 'session:a', 'alice',
                        'thread-1', 'hi', ?2, 'host-b', ?3, ?3)",
                    params![recipient, state, NOW],
                )
                .unwrap();
        }
        let labels = ["devbox".to_owned()];
        let answers = |from: &str| match store
            .sent_from("m1", "host-b", "rex:/tmp/a.sock", from, &labels)
            .unwrap()
        {
            Answers::Sent { .. } => "sent",
            Answers::Unsettled { .. } => "unsettled",
            Answers::OtherLabel | Answers::Unknown => "nowhere",
        };
        let own = |recipient: &str| {
            if order[usize::from(recipient == "eve")] == "accepted" {
                "sent"
            } else {
                "unsettled"
            }
        };
        assert_eq!(answers("carol"), "sent", "{order:?}");
        assert_eq!(answers("bob"), own("bob"), "{order:?}");
        assert_eq!(answers("eve"), own("eve"), "{order:?}");
    }
}

/// B reads a page as a range of its `(host_id, seq)` index, so neither
/// another host's replies nor a sort of every later reply costs a read.
#[test]
fn b_reads_a_page_as_a_range_of_its_host_s_replies() {
    let state = State::new();
    let store = state.open(NOW);
    let plan: Vec<String> = store
        .connection
        .prepare(&format!("EXPLAIN QUERY PLAN {PAGE}"))
        .unwrap()
        .query_map(params!["host-a", 0], |row| row.get(3))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        plan,
        ["SEARCH outbox USING INDEX outbox_by_seq (host_id=? AND seq>?)"]
    );
}

/// Doctor counts the replies dropped for another host's message until
/// 7 days pass with none, and then counts from one again.
#[test]
fn replies_dropped_for_another_host_are_counted_until_7_quiet_days() {
    let state = State::new();
    let store = state.open(NOW);
    let endpoint = "rex:/tmp/a.sock";
    for at in [NOW, NOW + 1000] {
        store.count_other_label(endpoint, "devbox", at).unwrap();
    }
    let later = NOW + 1000 + OTHER_LABEL_LIFETIME_MS;
    assert_eq!(
        store.conditions(endpoint, later - 1).unwrap()[0].other_label,
        2
    );
    assert!(store.conditions(endpoint, later).unwrap().is_empty());
    store.count_other_label(endpoint, "devbox", later).unwrap();
    assert_eq!(store.conditions(endpoint, later).unwrap()[0].other_label, 1);
}

/// A refused reply's sender is told once: the record stays once told, so
/// the same reply refused again is not told again.
#[test]
fn a_refused_reply_is_listed_until_told_and_never_again() {
    let state = State::new();
    let store = state.open(NOW);
    let held = Held {
        endpoint: "rex:/tmp/a.sock".to_owned(),
        peer: "devbox".to_owned(),
        host_id: "host-b".to_owned(),
        epoch: "e1".to_owned(),
        label: "devbox".to_owned(),
        space: "session:a".to_owned(),
        thread: "thread-1".to_owned(),
        reply: Reply {
            seq: 1,
            re: "message-1".to_owned(),
            thread: "t".to_owned(),
            from: "bob".to_owned(),
            message_id: "b-1".to_owned(),
            text: "hi".to_owned(),
        },
    };
    store.refused(&held, "no", NOW).unwrap();
    let dropped = store.dropped(&held.endpoint).unwrap();
    assert_eq!(dropped.len(), 1);
    store.told_dropped(&held.endpoint, &dropped[0]).unwrap();
    store.refused(&held, "no", NOW + 1).unwrap();
    assert!(store.dropped(&held.endpoint).unwrap().is_empty());
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

/// An item B's Automatic Reply Limit paused that is still queued after 24
/// hours expires with that reason, not as one B never answered.
#[test]
fn a_paused_item_expires_with_its_thread_s_pause_as_the_reason() {
    let state = State::new();
    let store = state.open(NOW);
    queue(&store, "paused", NOW - OUTBOUND_LIFETIME_MS);
    let item = store.queued("devbox", "rex:/tmp/a.sock").unwrap().remove(0);
    store.set_paused("devbox", &item, true).unwrap();
    assert_eq!(store.paused_untold("rex:/tmp/a.sock").unwrap().len(), 1);
    drop(store);
    let store = state.open(NOW);
    assert!(store.paused_untold("rex:/tmp/a.sock").unwrap().is_empty());
    let unsent = store.unsent("rex:/tmp/a.sock").unwrap();
    assert_eq!(
        unsent[0].reason,
        "the Automatic Reply Limit kept its thread on devbox paused for 24 hours"
    );
}

/// A later message of a thread that waited behind a paused one expires for
/// its reason too. Another recipient of the paused message, another
/// thread, a thread whose first item B kept for another reason, and an item
/// before the paused one, did not wait behind it, and an item B no longer
/// says is paused is not.
#[test]
fn the_items_behind_a_paused_one_expire_with_its_reason() {
    let state = State::new();
    let store = state.open(NOW);
    let old = NOW - OUTBOUND_LIFETIME_MS;
    for (message, recipient, thread, at) in [
        ("paused", "bob", "thread-1", old - 3),
        ("paused", "carol", "thread-1", old - 3),
        ("behind", "bob", "thread-1", old - 2),
        ("elsewhere", "bob", "thread-2", old - 1),
        ("was-paused", "bob", "thread-3", old),
        ("retry-first", "bob", "thread-4", old - 1),
        ("retry-second", "bob", "thread-4", old),
        ("before", "bob", "thread-5", old - 2),
        ("paused-after", "bob", "thread-5", old - 1),
    ] {
        store
            .connection
            .execute(
                "INSERT INTO outbound (label, message_id, recipient, endpoint, space, sender,
                    thread, text, state, created_at_ms, updated_at_ms)
                 VALUES ('devbox', ?1, ?2, 'rex:/tmp/a.sock', 'session:a', 'alice', ?3, 'hi',
                    'queued', ?4, ?4)",
                params![message, recipient, thread, at],
            )
            .unwrap();
    }
    let item = |message: &str, recipient: &str| {
        store
            .queued("devbox", "rex:/tmp/a.sock")
            .unwrap()
            .into_iter()
            .find(|item| item.message_id == message && item.recipient == recipient)
            .unwrap()
    };
    for (message, recipient) in [("paused", "bob"), ("paused-after", "bob")] {
        store
            .set_paused("devbox", &item(message, recipient), true)
            .unwrap();
    }
    let was = item("was-paused", "bob");
    store.set_paused("devbox", &was, true).unwrap();
    store.set_paused("devbox", &was, false).unwrap();
    drop(store);
    let store = state.open(NOW);
    let reasons: Vec<(String, String, String)> = store
        .unsent("rex:/tmp/a.sock")
        .unwrap()
        .into_iter()
        .map(|unsent| (unsent.message_id, unsent.recipient, unsent.reason))
        .collect();
    let paused = "the Automatic Reply Limit kept its thread on devbox paused for 24 hours";
    let unreachable = "devbox was unreachable for 24 hours";
    let row = |message: &str, recipient: &str, reason: &str| {
        (message.to_owned(), recipient.to_owned(), reason.to_owned())
    };
    assert_eq!(
        reasons,
        [
            row("paused", "bob", paused),
            row("paused", "carol", unreachable),
            row("behind", "bob", paused),
            row("elsewhere", "bob", unreachable),
            row("was-paused", "bob", unreachable),
            row("retry-first", "bob", unreachable),
            row("retry-second", "bob", unreachable),
            row("before", "bob", unreachable),
            row("paused-after", "bob", paused),
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
                peer: "devbox",
                host_id: "host-b",
                epoch: "epoch-1",
                seq,
                label: "devbox",
                endpoint: "rex:/tmp/a.sock",
                space: "session:a",
                message_id: &format!("message-{seq}"),
                remote_message_id: "",
                remote_alias: "",
                thread: "",
                now_ms: at,
            })
            .unwrap();
        store
            .advance("rex:/tmp/a.sock", "devbox", "host-b", "epoch-1", seq)
            .unwrap();
    }
    drop(store);

    let store = state.open(NOW);
    assert_eq!(count(&store, "SELECT seq FROM collected"), 2);
    assert_eq!(
        store
            .ack_through("rex:/tmp/a.sock", "devbox", "host-b")
            .unwrap(),
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

/// A endpoint holds at most 100 replies of one label, whatever `host_id`
/// each came from, and at most 20 of one thread.
#[test]
fn a_label_holds_at_most_100_replies_and_a_thread_20() {
    let state = State::new();
    let store = state.open(NOW);
    let held = |label: &str, host_id: &str, thread: &str, n: usize| Held {
        endpoint: "rex:/tmp/a.sock".to_owned(),
        peer: label.to_owned(),
        host_id: host_id.to_owned(),
        epoch: "e1".to_owned(),
        label: label.to_owned(),
        space: "session:a".to_owned(),
        thread: thread.to_owned(),
        reply: Reply {
            seq: i64::try_from(n).unwrap(),
            re: "message-a".to_owned(),
            thread: "t".to_owned(),
            from: "bob".to_owned(),
            message_id: format!("{host_id}-{thread}-{n}"),
            text: "x".to_owned(),
        },
    };
    for thread in 0..5 {
        let host_id = format!("host-{thread}");
        for n in 0..20 {
            assert!(
                store
                    .hold(&held("devbox", &host_id, &format!("t{thread}"), n), NOW)
                    .unwrap()
            );
        }
        assert!(
            !store
                .hold(&held("devbox", &host_id, &format!("t{thread}"), 20), NOW)
                .unwrap(),
            "a thread holds 20"
        );
    }
    assert!(
        !store
            .hold(&held("devbox", "host-new", "t9", 0), NOW)
            .unwrap(),
        "a new host_id gets no more room"
    );
    assert!(
        store.hold(&held("devbox", "host-0", "t0", 3), NOW).unwrap(),
        "one already held stays held"
    );
    assert!(store.hold(&held("studio", "host-0", "t5", 0), NOW).unwrap());
}

/// A reply one server's labels hold or refused is no reply of another
/// server's that reports the same `host_id` and message id: the other's
/// is not found placed, and is held only when its own thread has room.
#[test]
fn what_one_server_held_or_refused_is_not_another_s() {
    let state = State::new();
    let store = state.open(NOW);
    let held = |peer: &str, thread: &str, message_id: String| Held {
        endpoint: "rex:/tmp/a.sock".to_owned(),
        peer: peer.to_owned(),
        host_id: "host-s".to_owned(),
        epoch: "e1".to_owned(),
        label: peer.to_owned(),
        space: "session:a".to_owned(),
        thread: thread.to_owned(),
        reply: Reply {
            seq: 1,
            re: "message-a".to_owned(),
            thread: "t".to_owned(),
            from: "eve".to_owned(),
            message_id,
            text: "x".to_owned(),
        },
    };
    for n in 0..HELD_PER_THREAD {
        assert!(
            store
                .hold(&held("studio", "full", format!("s-{n}")), NOW)
                .unwrap()
        );
    }
    assert!(
        store
            .hold(&held("devbox", "other", "x-1".to_owned()), NOW)
            .unwrap()
    );
    assert!(
        !store
            .hold(&held("studio", "full", "x-1".to_owned()), NOW)
            .unwrap(),
        "studio's thread has no room, whatever devbox holds"
    );
    store
        .refused(&held("devbox", "other", "x-2".to_owned()), "refused", NOW)
        .unwrap();
    assert!(
        store
            .placed(&held("devbox", "other", "x-2".to_owned()))
            .unwrap()
    );
    assert!(
        !store
            .placed(&held("studio", "full", "x-2".to_owned()))
            .unwrap()
    );
}

fn loss(re: &str, from: &str, since: i64) -> Lost {
    Lost {
        re: re.into(),
        from: from.into(),
        since,
        discarded: 1,
        expired: 0,
    }
}

#[test]
fn lost_reports_validate_authority_fields_and_bound_the_page() {
    let state = State::new();
    let store = state.open(NOW);
    queue(&store, "message", NOW);
    store
        .connection
        .execute("UPDATE outbound SET state='accepted', host_id='host-b'", [])
        .unwrap();
    let endpoint = "rex:/tmp/a.sock";
    for (label, host, report) in [
        ("studio", "host-b", loss("message", "bob", 1)),
        ("devbox", "other-host", loss("message", "bob", 2)),
        ("devbox", "host-b", loss("unknown", "bob", 3)),
        ("devbox", "host-b", loss("bad\nid", "bob", 4)),
        ("devbox", "host-b", loss("message", "bad\nalias", 5)),
        ("devbox", "host-b", loss("message", &"a".repeat(65), 6)),
    ] {
        store
            .note_lost(endpoint, label, label, host, "epoch", &[report], NOW)
            .unwrap();
    }
    for count in [-1, LOST_COUNT_LIMIT + 1, i64::MAX] {
        for expired in [false, true] {
            let mut report = loss("message", "bob", 7);
            if expired {
                report.expired = count;
            } else {
                report.discarded = count;
            }
            store
                .note_lost(
                    endpoint,
                    "devbox",
                    "devbox",
                    "host-b",
                    "epoch",
                    &[report],
                    NOW,
                )
                .unwrap();
        }
    }
    assert_eq!(count(&store, "SELECT COUNT(*) FROM lost"), 0);
    store
        .connection
        .execute("UPDATE outbound SET state='queued'", [])
        .unwrap();
    store
        .note_lost(
            endpoint,
            "devbox",
            "devbox",
            "host-b",
            "epoch",
            &[loss("message", "bob", 8)],
            NOW,
        )
        .unwrap();
    assert_eq!(count(&store, "SELECT COUNT(*) FROM lost"), 0);
    store
        .connection
        .execute("UPDATE outbound SET state='accepted'", [])
        .unwrap();
    let reports: Vec<_> = (0..100).map(|i| loss("message", "bob", i)).collect();
    store
        .note_lost(
            endpoint, "devbox", "devbox", "host-b", "epoch", &reports, NOW,
        )
        .unwrap();
    assert_eq!(count(&store, "SELECT COUNT(*) FROM lost"), LOST_LIMIT);
    assert_eq!(
        count(&store, "SELECT MAX(since_ms) FROM lost"),
        LOST_LIMIT - 1,
        "B names its latest first, so A keeps the first 32 of the answer"
    );
    let untold = store.lost_untold(endpoint).unwrap();
    assert_eq!(untold.len(), 1, "one alias and message are told once");
    assert_eq!(
        (untold[0].discarded, untold[0].expired),
        (LOST_LIMIT, 0),
        "of every count A keeps"
    );
}

/// A worker that notes a loss while another relay writer holds the write
/// lock waits for it, as a capture does, rather than failing the page.
#[test]
fn a_loss_noted_while_another_relay_writer_writes_waits_for_it() {
    let state = State::new();
    let store = state.open(NOW);
    queue(&store, "message", NOW);
    store
        .connection
        .execute("UPDATE outbound SET state='accepted', host_id='host-b'", [])
        .unwrap();
    let writer = Connection::open(state.0.join("relay.sqlite3")).unwrap();
    writer
        .execute_batch("BEGIN IMMEDIATE; UPDATE outbox_epoch SET epoch = epoch;")
        .unwrap();
    let committed = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        writer.execute_batch("COMMIT").unwrap();
    });
    let noted = store.note_lost(
        "rex:/tmp/a.sock",
        "devbox",
        "devbox",
        "host-b",
        "epoch",
        &[loss("message", "bob", 1)],
        NOW,
    );
    committed.join().unwrap();
    assert_eq!(noted.map_err(|error| error.to_string()), Ok(()));
    assert_eq!(count(&store, "SELECT COUNT(*) FROM lost"), 1);
}

/// A loss told stays told as long as A keeps the item it answers, so B can
/// never name it again to a sender who was told: the record goes only once
/// the item has.
#[test]
fn a_told_loss_stays_told_while_its_item_is_kept() {
    let state = State::new();
    let store = state.open(NOW);
    queue(&store, "message", NOW);
    store
        .connection
        .execute("UPDATE outbound SET state='accepted', host_id='host-b'", [])
        .unwrap();
    let endpoint = "rex:/tmp/a.sock";
    store
        .note_lost(
            endpoint,
            "devbox",
            "devbox",
            "host-b",
            "epoch",
            &[loss("message", "bob", 1)],
            NOW,
        )
        .unwrap();
    let untold = store.lost_untold(endpoint).unwrap();
    assert_eq!(untold.len(), 1);
    store.told_lost(endpoint, &untold[0], NOW).unwrap();
    drop(store);
    let kept = |store: &RelayStore| {
        (
            count(store, "SELECT COUNT(*) FROM outbound"),
            count(store, "SELECT COUNT(*) FROM lost_told"),
        )
    };
    let store = state.open(NOW + DONE_LIFETIME_MS - 1);
    assert_eq!(kept(&store), (1, 1));
    assert!(store.lost_untold(endpoint).unwrap().is_empty());
    drop(store);
    let store = state.open(NOW + DONE_LIFETIME_MS);
    assert_eq!(kept(&store), (0, 0), "the record goes with its item");
}

#[test]
fn lost_reports_roll_back_the_whole_page_on_write_failure() {
    let state = State::new();
    let store = state.open(NOW);
    queue(&store, "message", NOW);
    store
        .connection
        .execute("UPDATE outbound SET state='accepted', host_id='host-b'", [])
        .unwrap();
    store.connection.execute_batch("CREATE TRIGGER fail_second BEFORE INSERT ON lost WHEN NEW.since_ms = 2 BEGIN SELECT RAISE(ABORT, 'injected'); END;").unwrap();
    assert!(
        store
            .note_lost(
                "rex:/tmp/a.sock",
                "devbox",
                "devbox",
                "host-b",
                "epoch",
                &[loss("message", "bob", 1), loss("message", "bob", 2)],
                NOW
            )
            .is_err()
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM lost"), 0);
}

#[test]
fn lost_reports_doctor_saturates_legacy_counts_and_preserves_other_conditions() {
    let state = State::new();
    let store = state.open(NOW);
    store
        .set_can_reply("rex:/tmp/a.sock", "devbox", false)
        .unwrap();
    for since in [1, 2] {
        store.connection.execute("INSERT INTO lost (endpoint, host_id, epoch, re, remote_alias, since_ms, label, peer, discarded, expired, updated_at_ms) VALUES ('rex:/tmp/a.sock', 'host-b', 'epoch', 'message', 'bob', ?1, 'devbox', 'devbox', ?2, ?2, ?3)", params![since, i64::MAX, NOW]).unwrap();
    }
    let conditions = store.conditions("rex:/tmp/a.sock", NOW).unwrap();
    assert_eq!(conditions.len(), 1);
    assert_eq!(conditions[0].lost, i64::MAX);
    assert!(conditions[0].cannot_reply);
    drop(store);
    fs::write(state.0.join("relay.sqlite3"), b"not a SQLite database").unwrap();
    assert!(crate::relay::conditions(&state.0, "rex:/tmp/a.sock").is_err());
}

#[test]
fn lost_reports_retain_only_the_latest_page_per_peer_across_answers() {
    let state = State::new();
    let store = state.open(NOW);
    let endpoint = "rex:/tmp/a.sock";
    queue(&store, "message", NOW);
    store
        .connection
        .execute("UPDATE outbound SET state='accepted', host_id='host-b'", [])
        .unwrap();
    store.connection.execute("INSERT INTO outbound (label, message_id, recipient, endpoint, space, sender, thread, text, state, host_id, created_at_ms, updated_at_ms) VALUES ('studio', 'message', 'bob', 'rex:/tmp/a.sock', 'session:a', 'alice', 'thread-1', 'hi', 'accepted', 'host-c', ?1, ?1)", params![NOW]).unwrap();
    store
        .note_lost(
            endpoint,
            "studio",
            "studio",
            "host-c",
            "epoch",
            &[loss("message", "bob", 0)],
            NOW,
        )
        .unwrap();
    for page in 0..3 {
        let reports: Vec<_> = (0..LOST_LIMIT)
            .map(|i| loss("message", "bob", page * LOST_LIMIT + i))
            .collect();
        store
            .note_lost(
                endpoint,
                "devbox",
                "devbox",
                "host-b",
                &format!("epoch-{page}"),
                &reports,
                NOW + page,
            )
            .unwrap();
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM lost WHERE label='devbox'"),
            LOST_LIMIT
        );
        assert_eq!(
            count(
                &store,
                "SELECT MIN(since_ms) FROM lost WHERE label='devbox'"
            ),
            page * LOST_LIMIT
        );
        assert_eq!(
            count(&store, "SELECT COUNT(*) FROM lost WHERE label='studio'"),
            1
        );
        assert_eq!(
            store
                .lost_untold(endpoint)
                .unwrap()
                .iter()
                .map(|lost| (lost.label.as_str(), lost.discarded))
                .collect::<Vec<_>>(),
            [("studio", 1), ("devbox", LOST_LIMIT)]
        );
    }
}
