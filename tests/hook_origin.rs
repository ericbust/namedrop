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

#[test]
fn explicit_hook_selection_precedes_origin_state_and_host_io() {
    let root = std::env::temp_dir().join(format!("namedrop-hook-selection-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let marker = root.join("runtime-contacted");
    let host_marker = root.join("host-contacted");
    let host = root.join("host");
    fs::write(
        &host,
        format!(
            "#!/bin/sh\nprintf host > '{}'\nexit 97\n",
            host_marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&host, fs::Permissions::from_mode(0o700)).unwrap();
    for name in ["baked", "hc", "plugin"] {
        let state = root.join(name);
        fs::create_dir_all(state.join("runtime")).unwrap();
        let launcher = state.join("runtime/namedrop");
        fs::write(
            &launcher,
            format!(
                "#!/bin/sh\nprintf %s {name} > '{}'\nexit 98\n",
                marker.display()
            ),
        )
        .unwrap();
        fs::set_permissions(launcher, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut failures = Vec::new();
    for event in ["session-start", "user-prompt-submit", "mailbox-wake"] {
        for (name, flags, hc_origin, hc_ambient, expected) in [
            (
                "tui overrides hc ambient",
                vec!["--host", "tui"],
                false,
                true,
                Some("baked"),
            ),
            (
                "hc overrides tui ambient",
                vec!["--host", "hc"],
                true,
                false,
                Some("hc"),
            ),
            (
                "hc origin follows explicit tui state",
                vec!["--host", "tui"],
                true,
                false,
                Some("baked"),
            ),
            (
                "tui origin follows explicit hc state",
                vec!["--host", "hc"],
                false,
                true,
                Some("hc"),
            ),
            (
                "tui rejects hc flags",
                vec!["--host", "tui", "--hc-terminal", "terminal"],
                true,
                false,
                None,
            ),
            (
                "hc rejects tui flags",
                vec!["--host", "hc", "--herdr-pane", "pane"],
                false,
                true,
                None,
            ),
            (
                "both flag groups reject explicit hc",
                vec![
                    "--host",
                    "hc",
                    "--herdr-pane",
                    "pane",
                    "--hc-space",
                    "space",
                ],
                true,
                false,
                None,
            ),
            (
                "both flag groups reject without host",
                vec!["--herdr-pane", "pane", "--hc-space", "space"],
                false,
                true,
                None,
            ),
        ] {
            let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
            command
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", root.join("home"))
                .env("NAMEDROP_STATE_DIR", root.join("hc"))
                .env("HERDR_PLUGIN_STATE_DIR", root.join("unused-tui"))
                .env("HERDR_BIN_PATH", &host)
                .env("HC_BIN_PATH", &host)
                .args(&flags)
                .arg("--json")
                .stdin(Stdio::null());
            if hc_origin {
                command.arg("--hc-hook");
            }
            if hc_ambient {
                command.env("HC_TERMINAL_ID", "terminal");
            } else {
                command.env("HERDR_SOCKET_PATH", "socket");
            }
            let output = command
                .args(["hook", event, "--state-directory"])
                .arg(root.join("baked"))
                .output()
                .unwrap();
            let valid = if let Some(state) = expected {
                output.status.code() == Some(98)
                    && fs::read_to_string(&marker).ok().as_deref() == Some(state)
            } else {
                let body: serde_json::Value =
                    serde_json::from_slice(&output.stdout).unwrap_or_default();
                output.status.code() == Some(64)
                    && body.to_string().contains("invalid_request")
                    && body.to_string().contains("conflict")
                    && !marker.exists()
            };
            if !valid {
                failures.push(format!(
                    "{event}: {name}: {:?} stdout={} stderr={} marker={:?}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                    fs::read_to_string(&marker).ok()
                ));
            }
            assert!(!host_marker.exists(), "{event}: {name} accessed host");
            let _ = fs::remove_file(&marker);
        }
    }
    fs::remove_dir_all(root).unwrap();
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
