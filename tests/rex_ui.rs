#[path = "support/binary.rs"]
mod binary;
#[path = "support/executable.rs"]
mod executable;

use std::{fs, os::unix::fs::PermissionsExt};

#[test]
fn refused_config_start_and_unknown_action_write_nothing_and_call_no_host() {
    let root = std::env::temp_dir().join(format!("nd-u6-cli-{}", std::process::id()));
    fs::create_dir_all(root.join("home")).unwrap();
    let herdr = root.join("herdr");
    let called = root.join("host-called");
    fs::write(
        &herdr,
        format!("#!/bin/sh\n/usr/bin/touch '{}'\nexit 1\n", called.display()),
    )
    .unwrap();
    fs::set_permissions(&herdr, fs::Permissions::from_mode(0o700)).unwrap();
    for args in [
        vec!["rex", "ensure", "--from-server"],
        vec!["rex", "action", "future"],
        vec!["rex", "action", "two\nlines"],
        vec!["rex", "action", "name"],
    ] {
        let out = binary::namedrop()
            .env_clear()
            .env("HOME", root.join("home"))
            .env("PATH", "/usr/bin:/bin")
            .env("NAMEDROP_STATE_DIR", root.join("state"))
            .env("HERDR_BIN_PATH", &herdr)
            .env(
                "REX_SERVER",
                format!("unix://{}", root.join("no-server.sock").display()),
            )
            .args(&args)
            .output()
            .unwrap();
        assert!(out.stdout.is_empty(), "{out:?}");
        if args[1] == "ensure" {
            assert!(out.status.success());
            assert!(out.stderr.is_empty());
        } else if args[2] != "name" {
            // The palette shows one plain line, whatever the error holds.
            assert_eq!(out.status.code(), Some(1));
            assert_eq!(
                String::from_utf8(out.stderr).unwrap(),
                format!(
                    "This Namedrop has no action {}. Run sh scripts/install.sh again.\n",
                    args[2].replace('\n', " ")
                )
            );
        } else {
            assert_eq!(out.status.code(), Some(1));
            assert_eq!(
                String::from_utf8(out.stderr).unwrap(),
                "Namedrop Rex actions need a rex: --endpoint\n"
            );
        }
        assert!(!root.join("state").exists());
        assert!(!called.exists());
        assert!(fs::read_dir(root.join("home")).unwrap().next().is_none());
    }
    fs::remove_dir_all(root).unwrap();
}

/// The U6 review's F6: a palette action starts the reminder clock before it
/// runs, as every other command does. The state holds one pending reminder,
/// and its selected runtime only records that it was started.
#[test]
fn a_palette_action_starts_the_reminder_clock_as_every_command_does() {
    let root = std::env::temp_dir().join(format!("nd-u6-clock-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let state = root.join("state");
    fs::create_dir_all(state.join("runtime/current")).unwrap();
    fs::create_dir_all(root.join("home")).unwrap();
    let endpoint = "rex:/tmp/u6-clock-never-listens.sock";
    rusqlite::Connection::open(state.join("namedrop.sqlite3"))
        .unwrap()
        .execute_batch(&format!(
            "CREATE TABLE reminders (session_socket_id TEXT NOT NULL, worktree_id TEXT NOT NULL, state TEXT NOT NULL, due_at_ms INTEGER);\
             CREATE INDEX reminders_pending ON reminders(due_at_ms) WHERE state IN ('scheduled','awaiting_delivery');\
             INSERT INTO reminders VALUES ('{endpoint}', 'w1', 'scheduled', 99999999999999);"
        ))
        .unwrap();
    let clock = state.join("runtime/current/namedrop");
    executable::write_executable(&clock, "#!/bin/sh\necho \"$*\" >> \"$0.record\"\n");
    let out = binary::namedrop()
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.join("home"))
        .env("NAMEDROP_STATE_DIR", &state)
        .args(["rex", "action", "name", "--endpoint", endpoint])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let record = state.join("runtime/current/namedrop.record");
    let started = format!("reminder-clock {} {endpoint}\n", state.display());
    let until = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while fs::read_to_string(&record).ok().as_deref() != Some(started.as_str()) {
        assert!(
            std::time::Instant::now() < until,
            "no reminder clock started: {:?}",
            fs::read_to_string(&record)
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    // The clock the action started may still be writing into the state when
    // the record appears, so a removal can race it. The folder is scratch.
    for _ in 0..50 {
        if fs::remove_dir_all(&root).is_ok() || !root.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// The U6 review's F1 and F3: the guard's accept path, at the binary, with
/// a scripted server that starts the command as Rex's server does.
#[test]
fn the_server_started_load_time_start_runs_one_bridge_keyed_by_the_proven_endpoint() {
    let output = binary::command("python3")
        .args([
            "-I",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rex_ensure.py"),
            binary::namedrop_path(),
        ])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[cfg(feature = "test-support")]
#[test]
fn owner_activation_refreshes_only_a_registered_module_and_keeps_config_bytes() {
    use namedrop::{
        AgentConfigDirectories, LifecycleRuntime, ProcessIds, SystemClock,
        execute_lifecycle_operation, mux::fake::FakeMux,
    };
    use serde_json::json;
    let root = std::env::temp_dir().join(format!("nd-u6-refresh-{}", std::process::id()));
    let home = root.join("home");
    let state = root.join("state");
    let module = root.join("namedrop.lua");
    fs::create_dir_all(home.join(".config/namedrop")).unwrap();
    let config = root.join("init.lua");
    fs::write(&config, "-- person's config\n").unwrap();
    let record = home.join(".config/namedrop/rex.json");
    let mux = FakeMux::world()
        .endpoint("/tmp/u6-herdr.sock")
        .space("s1")
        .pane("p1")
        .build();
    let ids = ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: &state,
        home_directory: Some(&home),
        agent_config: AgentConfigDirectories::default(),
        cwd: &root,
        workspace_id: Some("s1"),
        current_pane_id: Some("p1"),
        session_socket_id: "/tmp/u6-herdr.sock",
        mux: &mux,
        clock: &SystemClock,
        ids: &ids,
    };
    execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime).unwrap();
    assert!(!module.exists());
    fs::write(
        &record,
        json!({"module":module,"config":config,"line":"removed","predecessors":[{"state":"/old"}]})
            .to_string(),
    )
    .unwrap();
    execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&fs::read(&record).unwrap()).unwrap();
    assert_eq!(saved["state"], json!(state));
    assert_eq!(saved["line"], "removed");
    assert_eq!(saved["predecessors"], json!([{"state":"/old"}]));
    assert_eq!(
        fs::read_to_string(&module).unwrap(),
        namedrop::rex_ui::module_text(
            std::path::Path::new(saved["launcher"].as_str().unwrap()),
            &state
        )
        .unwrap()
    );
    assert_eq!(fs::read_to_string(config).unwrap(), "-- person's config\n");
    fs::remove_dir_all(root).unwrap();
}
