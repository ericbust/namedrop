//! Which multiplexer timeouts a daemon keeps. A caller's
//! `NAMEDROP_HERDR_TIMEOUT_MS` and `NAMEDROP_REX_TIMEOUT_MS` are ones a person
//! set, so the daemons it starts keep them. The Stop hook's wrapper sets
//! 250 ms for the hook's own deadline, so the Stop worker and the reminder
//! clock the hook starts drop both.
//!
//! The reminder clock shows it: each command that may start the clock starts
//! `runtime/current/namedrop` while a reminder is pending and no clock holds
//! the lock, and here that executable only records the timeouts it got. The
//! Stop worker starts its own clock first thing, so the Stop hook records two
//! clocks: the hook's and its worker's.

#[path = "support/binary.rs"]
mod binary;

#[path = "support/executable.rs"]
mod executable;

use std::{
    fs,
    io::Write,
    path::PathBuf,
    process::Stdio,
    thread,
    time::{Duration, Instant},
};

struct Fixture(PathBuf);

impl Fixture {
    /// A state directory with one pending reminder and a recording clock.
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-daemon-timeouts-{name}-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("state/runtime/current")).unwrap();
        fs::create_dir_all(root.join("home")).unwrap();
        rusqlite::Connection::open(root.join("state/namedrop.sqlite3"))
            .unwrap()
            .execute_batch(
                "CREATE TABLE reminders (session_socket_id TEXT NOT NULL, worktree_id TEXT NOT NULL, state TEXT NOT NULL, due_at_ms INTEGER);\
                 CREATE INDEX reminders_pending ON reminders(due_at_ms) WHERE state IN ('scheduled','awaiting_delivery');\
                 INSERT INTO reminders VALUES ('/run/h.sock', 'w1', 'scheduled', 99999999999999);",
            )
            .unwrap();
        executable::write_executable(
            root.join("state/runtime/current/namedrop"),
            "#!/bin/sh\necho \"${NAMEDROP_HERDR_TIMEOUT_MS-unset} ${NAMEDROP_REX_TIMEOUT_MS-unset}\" >> \"$0.record\"\n",
        );
        Self(root)
    }

    /// The installed `hook` with the timeouts `herdr` and `rex`, fed `input`.
    fn hook(&self, hook: &[&str], herdr: &str, rex: &str, input: &str) {
        let mut child = binary::namedrop()
            .args(["hook"])
            .args(hook)
            .arg("--state-directory")
            .arg(self.0.join("state"))
            .env("HOME", self.0.join("home"))
            .env("TMPDIR", &self.0)
            .env("HERDR_BIN_PATH", "/usr/bin/false")
            .env("HERDR_SOCKET_PATH", "/run/h.sock")
            .env("NAMEDROP_HERDR_TIMEOUT_MS", herdr)
            .env("NAMEDROP_REX_TIMEOUT_MS", rex)
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_PANE_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        assert!(child.wait().unwrap().success());
    }

    /// The timeouts of each clock started so far, once `count` have started.
    fn clocks(&self, count: usize) -> Vec<String> {
        let record = self.0.join("state/runtime/current/namedrop.record");
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let lines: Vec<String> = fs::read_to_string(&record)
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect();
            if lines.len() >= count || Instant::now() > deadline {
                thread::sleep(Duration::from_millis(200));
                return fs::read_to_string(&record)
                    .unwrap_or_default()
                    .lines()
                    .map(str::to_owned)
                    .collect();
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn a_clock_keeps_the_timeouts_of_the_caller_that_started_it() {
    let fixture = Fixture::new("caller");
    fixture.hook(
        &["subagent", "--runtime", "claude-code"],
        "7000",
        "6000",
        "{}",
    );
    assert_eq!(fixture.clocks(1), ["7000 6000"]);
}

#[test]
fn the_stop_hook_s_clock_and_its_worker_drop_the_wrapper_s_timeouts() {
    let fixture = Fixture::new("stop");
    fixture.hook(
        &["stop"],
        "250",
        "250",
        r#"{"hook_event_name":"Stop","session_id":"session-1","prompt_id":"prompt-1","last_assistant_message":"done","background_tasks":[]}"#,
    );
    assert_eq!(fixture.clocks(2), ["unset unset", "unset unset"]);
}
