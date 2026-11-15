#[path = "support/binary.rs"]
mod binary;

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
        } else if args[2] == "future" {
            assert_eq!(out.status.code(), Some(1));
            assert_eq!(
                String::from_utf8(out.stderr).unwrap(),
                "This Namedrop has no action future. Run sh scripts/install.sh again.\n"
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
