#![cfg(target_os = "linux")]

#[path = "support/cleanup.rs"]
mod cleanup;

#[path = "support/xws.rs"]
mod xws;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Worker(Child);

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn dropping_a_fixture_stops_its_mailbox_and_status_workers() {
    use sha2::{Digest, Sha256};
    let f = xws::Fixture::new();
    let host = f.root.join("failing-herdr");
    fs::write(
        &host,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$PPID\" >> '{}/worker-pids'\nexit 1\n",
            f.root.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&host, fs::Permissions::from_mode(0o700)).unwrap();
    let mut workers = Vec::new();
    for (operation, space) in [("mailbox-wake", "w1"), ("agent-status-worker", "w2")] {
        let scope = namedrop::lower_hex(&Sha256::digest(format!("socket-a\0{space}")));
        let queue = f.state.join("agent-status-reconciliation").join(scope);
        fs::create_dir_all(&queue).unwrap();
        fs::write(
            queue.join("pending-test"),
            b"{\"operation\":\"mailbox-wake\"}",
        )
        .unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
        command
            .arg("--plugin-state")
            .arg(&f.state)
            .args(["hook", operation]);
        if operation == "mailbox-wake" {
            command.arg("--state-directory").arg(&f.state);
        }
        workers.push(Worker(
            command
                .env("HERDR_PLUGIN_STATE_DIR", &f.state)
                .env("HERDR_SOCKET_PATH", "socket-a")
                .env("HERDR_WORKSPACE_ID", space)
                .env("HERDR_BIN_PATH", &host)
                .env_remove("HERDR_PANE_ID")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        ));
    }
    let deadline = Instant::now() + Duration::from_secs(10);
    let status_pids = loop {
        let pids = fs::read_to_string(f.root.join("worker-pids"))
            .unwrap_or_default()
            .lines()
            .map(|pid| pid.parse::<i32>().unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        if pids.len() >= 2 {
            break pids;
        }
        assert!(Instant::now() < deadline, "both workers must reach Herdr");
        thread::sleep(Duration::from_millis(10));
    };
    let owned_pids: Vec<_> = status_pids
        .into_iter()
        .chain(workers.iter().map(|worker| worker.0.id() as i32))
        .collect();
    drop(f);
    let live: Vec<_> = owned_pids
        .iter()
        .copied()
        .filter(|pid| unsafe { libc::kill(*pid, 0) } == 0)
        .collect();
    for pid in &live {
        unsafe { libc::kill(*pid, libc::SIGKILL) };
    }
    drop(workers);
    assert!(live.is_empty(), "fixture drop left workers alive: {live:?}");
}

#[test]
fn a_worker_does_not_recreate_a_deleted_state_directory() {
    let f = xws::Fixture::new();
    let state = f.state.clone();
    cleanup::remove_fixture(&state).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_namedrop"))
        .args(["hook", "agent-status-worker"])
        .env("HERDR_PLUGIN_STATE_DIR", &state)
        .env("HERDR_SOCKET_PATH", "socket-a")
        .env("HERDR_WORKSPACE_ID", "w1")
        .env_remove("HERDR_PANE_ID")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(
        !state.exists(),
        "a stale worker must leave the deleted state directory absent"
    );
}
