//! The relay's commands run only beside a Rex server.

#[path = "support/binary.rs"]
mod binary;

use std::{fs, io::Write, process::Stdio};

/// `namedrop relay <command>` with `args`, for a Herdr endpoint, in a
/// fresh state directory: its exit code, and whether the relay's database
/// was made.
fn run(args: &[&str]) -> (Option<i32>, bool) {
    let state = binary::scratch_home().join("relay-state");
    let _ = fs::remove_dir_all(&state);
    fs::create_dir_all(&state).unwrap();
    let mut child = binary::namedrop()
        .arg("relay")
        .args(args)
        .args(["--endpoint", "/tmp/namedrop-relay-cli-herdr.sock"])
        .env("NAMEDROP_STATE_DIR", &state)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let request = r#"{"op":"collect","expect_host_id":"h","from":{"host_id":"a","name":"alice","host":"laptop"}}"#;
    let _ = child.stdin.take().unwrap().write_all(request.as_bytes());
    let code = child.wait().unwrap().code();
    (code, state.join("relay.sqlite3").exists())
}

#[test]
fn relay_accept_and_worker_refuse_an_endpoint_that_is_not_rex() {
    assert_eq!(run(&["accept", "--host-id", "h"]), (Some(64), false));
    assert_eq!(run(&["worker", "--host", "devbox"]), (Some(64), false));
}
