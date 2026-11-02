#[path = "support/cleanup.rs"]
mod cleanup;

use namedrop::{StateStore, StoreErrorKind};
use rusqlite::Connection;
use std::{
    fs,
    path::PathBuf,
    thread,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);

impl Fixture {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "namedrop-store-locking-{label}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        drop(StateStore::open(&path).unwrap());
        Self(path)
    }

    fn database(&self) -> Connection {
        Connection::open(self.0.join("namedrop.sqlite3")).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        cleanup::remove_fixture(&self.0).unwrap();
    }
}

/// Hooks of every live agent open the store while a command writes. A
/// current schema needs no migration, so opening must not queue behind that
/// writer's lock.
#[test]
fn open_with_a_current_schema_does_not_wait_for_another_writer() {
    let fixture = Fixture::new("open");
    let writer = fixture.database();
    writer.execute_batch("BEGIN IMMEDIATE").unwrap();

    let started = Instant::now();
    let opened = StateStore::open(&fixture.0);
    let elapsed = started.elapsed();

    assert!(
        opened.is_ok(),
        "open must not need the write lock: {:?}",
        opened.err().map(|error| (error.kind(), error.to_string()))
    );
    assert!(
        elapsed < Duration::from_secs(1),
        "open waited {elapsed:?} for a writer it did not need"
    );
}

/// A command opens the store a dozen times, and so does every hook of every
/// live agent. With the rollback journal a committing writer waits for every
/// reader, so an open must not read the whole file. On a 44 MB state file
/// that read takes seconds under load, and every writer waiting on it gives
/// up with "database is locked".
#[test]
fn opening_a_large_state_file_is_quick_and_does_not_block_a_commit() {
    let fixture = Fixture::new("scan");
    let padding = fixture.database();
    padding
        .execute_batch(
            "CREATE TABLE padding (id INTEGER PRIMARY KEY, a TEXT NOT NULL, b TEXT NOT NULL);
             CREATE INDEX padding_a_b ON padding (a, b);
             INSERT INTO padding (a, b)
               WITH RECURSIVE n (i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 200000)
               SELECT hex(randomblob(40)), hex(randomblob(40)) FROM n;",
        )
        .unwrap();
    let writer = fixture.database();
    writer.busy_timeout(Duration::ZERO).unwrap();

    let directory = fixture.0.clone();
    let opener = thread::spawn(move || {
        let started = Instant::now();
        StateStore::open(&directory).map(|_| started.elapsed())
    });
    thread::sleep(Duration::from_millis(40));
    let committed = writer
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO padding (a, b) VALUES ('a', 'b'); COMMIT");
    let open_took = opener.join().unwrap().unwrap();

    assert!(
        committed.is_ok(),
        "a commit was blocked by a store being opened: {committed:?}"
    );
    assert!(
        open_took < Duration::from_millis(150),
        "open read the whole file: {open_took:?}"
    );
}

/// Without a scan at open, damage surfaces from the first statement that
/// reads it, and that error carries the backup guidance.
#[test]
fn a_damaged_state_file_still_reports_backup_guidance() {
    let fixture = Fixture::new("damaged");
    let path = fixture.0.join("namedrop.sqlite3");
    let mut bytes = fs::read(&path).unwrap();
    bytes[..16].copy_from_slice(b"not a database!!");
    fs::write(&path, bytes).unwrap();

    let error = StateStore::open(&fixture.0)
        .err()
        .expect("damaged file must not open");

    assert_eq!(error.kind(), StoreErrorKind::Storage);
    let read_only = StateStore::open_read_only(&fixture.0)
        .err()
        .expect("damaged file must not open read-only");
    for message in [error.to_string(), read_only.to_string()] {
        assert!(
            message.contains("Back up the file before replacing or removing it"),
            "{message}"
        );
        assert!(
            message.contains(&path.display().to_string()),
            "names the file to back up: {message}"
        );
    }
}
