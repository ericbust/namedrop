use std::{
    fs,
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[test]
fn cross_host_hooks_decline_before_locked_state_runtime_or_socket_access() {
    let root = std::env::temp_dir().join(format!("namedrop-hook-origin-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let state = root.join("state");
    fs::create_dir_all(state.join("runtime")).unwrap();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(state.join("runtime/commands.lock"))
        .unwrap();
    lock.lock().unwrap();
    let tripwire = root.join("tripwire");
    let marker = root.join("contacted");
    fs::write(
        &tripwire,
        format!(
            "#!/bin/sh\nprintf contacted > '{}'\nexit 98\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&tripwire, fs::Permissions::from_mode(0o755)).unwrap();
    fs::copy(&tripwire, state.join("runtime/namedrop")).unwrap();
    let binary = std::env::var_os("NAMEDROP_TEST_BASE_BINARY")
        .unwrap_or_else(|| env!("CARGO_BIN_EXE_namedrop").into());
    let mut failures = Vec::new();
    for hc_origin in [false, true] {
        for event in ["session-start", "user-prompt-submit", "mailbox-wake"] {
            let mut command = Command::new(&binary);
            command
                .env_clear()
                .env("HOME", root.join("home"))
                .env("PATH", "/usr/bin:/bin")
                .env("HC_BIN_PATH", &tripwire)
                .env("HERDR_BIN_PATH", &tripwire)
                .env("NAMEDROP_STATE_DIR", &state)
                .env("HERDR_PLUGIN_STATE_DIR", &state)
                .args([
                    "--host",
                    if hc_origin { "hc" } else { "tui" },
                    "--plugin-state",
                ])
                .arg(&state)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            if hc_origin {
                command
                    .arg("--hc-hook")
                    .env("HERDR_SOCKET_PATH", "tui-tripwire")
                    .env("HC_TERMINAL_ID", "hc-inherited");
            } else {
                command.env("HC_TERMINAL_ID", "hc-inherited");
            }
            command
                .args(["hook", event, "--state-directory"])
                .arg(&state);
            let mut child = command.spawn().unwrap();
            let deadline = Instant::now() + Duration::from_secs(2);
            while child.try_wait().unwrap().is_none() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if child.try_wait().unwrap().is_none() {
                child.kill().unwrap();
                failures.push(format!(
                    "hc_origin={hc_origin} {event} waited behind the command gate"
                ));
            }
            let output = child.wait_with_output().unwrap();
            if !output.status.success() || !output.stdout.is_empty() || !output.stderr.is_empty() {
                failures.push(format!(
                    "hc_origin={hc_origin} {event} did not quietly decline: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }
    }
    assert!(!marker.exists(), "cross-host hook contacted runtime/socket");
    assert!(
        !state.join("namedrop.sqlite3").exists(),
        "cross-host hook opened state"
    );
    drop(lock);
    fs::remove_dir_all(root).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
