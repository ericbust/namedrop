#[path = "support/binary.rs"]
mod binary;

use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "nd-u7-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(path.join("home")).unwrap();
        fs::create_dir_all(path.join("state/runtime")).unwrap();
        Self(path)
    }
    fn run(&self, args: &[&str]) -> std::process::Output {
        binary::namedrop()
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.0.join("home"))
            .env("NAMEDROP_STATE_DIR", self.0.join("state"))
            .env("NAMEDROP_ENDPOINT", "rex:/tmp/u7-must-never-connect.sock")
            .env("HERDR_BIN_PATH", self.0.join("host"))
            .args(args)
            .output()
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn retirement_precedes_every_dispatch_form_and_all_cached_hook_forms() {
    let f = Fixture::new();
    let state = f.0.join("state");
    let state_arg = state.to_str().unwrap();
    let called = f.0.join("host-called");
    fs::write(
        f.0.join("host"),
        format!("#!/bin/sh\ntouch '{}'\nexit 1\n", called.display()),
    )
    .unwrap();
    fs::set_permissions(f.0.join("host"), fs::Permissions::from_mode(0o700)).unwrap();
    for reason in ["removed", "herdr_handoff"] {
        let marker = json!({"reason":reason,"successor":if reason == "removed" {Value::Null} else {json!("/new-herdr")},"at":"2026-11-15T00:00:00Z"});
        fs::write(state.join("runtime/retired.json"), marker.to_string()).unwrap();
        let before = fs::read(state.join("runtime/retired.json")).unwrap();
        for args in [
            vec!["doctor"],
            vec!["activate"],
            vec!["name", "x"],
            vec!["reconcile"],
            vec!["event", "pane-lifecycle"],
            vec!["address", "list"],
            vec!["space", "pause"],
            vec!["send"],
            vec!["install"],
            vec!["rex", "action", "name"],
            vec!["rex", "sessions", "--json"],
            vec!["ui", "launch", "name"],
            vec!["--runtime-info"],
            vec!["--validate-update-manifest", "/does-not-exist"],
            vec!["describe"],
            vec!["snapshot", "publish"],
            vec!["context", "inspect"],
            vec!["handoff", "status"],
            vec!["group", "list"],
            vec!["reminder", "list"],
            vec!["thread", "pause"],
            vec!["delivery", "inbox"],
            vec!["channels"],
            vec!["relay", "accept", "--host-id", "h"],
            vec!["terminal-execute", "attempt-1"],
            vec!["--migrate-runtime-state", state_arg],
        ] {
            let output = f.run(&args);
            assert_eq!(
                output.status.code(),
                Some(69),
                "{reason} {args:?}: {output:?}"
            );
            assert!(output.stdout.is_empty(), "{args:?}");
            let message = String::from_utf8(output.stderr).unwrap();
            assert_eq!(message.lines().count(), 1, "{args:?}: {message}");
            assert!(
                message.contains(if reason == "removed" {
                    "Namedrop was removed from Rex."
                } else {
                    "ND-INSTALL-001: Namedrop on Rex moved to Herdr's install in /new-herdr."
                }),
                "{args:?}: {message}"
            );
        }
        for args in [
            vec!["hook", "session-start", "--state-directory", state_arg],
            vec!["hook", "user-prompt-submit", "--state-directory", state_arg],
            vec!["hook", "stop", "--state-directory", state_arg],
            vec![
                "hook",
                "subagent",
                "--state-directory",
                state_arg,
                "--runtime",
                "codex",
            ],
            vec!["hook", "mailbox-wake", "--state-directory", state_arg],
            vec!["hook", "stop-worker", "--state-directory", state_arg],
            vec!["hook", "agent-status-changed"],
            vec!["hook", "agent-status-worker"],
            vec!["rex", "bridge"],
            vec!["rex", "ensure", "--from-server"],
            vec!["relay", "worker", "--host", "b"],
            vec!["reminder-clock", state_arg, "rex:/tmp/never.sock"],
            vec!["--deferred-worker", state_arg, "rex:/tmp/never.sock"],
            vec![
                "--deferred-execute",
                state_arg,
                "rex:/tmp/never.sock",
                "key",
            ],
            vec!["dashboard-server"],
        ] {
            let output = f.run(&args);
            assert!(
                output.status.success() && output.stdout.is_empty() && output.stderr.is_empty(),
                "{reason} {args:?}: {output:?}"
            );
        }
        assert_eq!(
            fs::read(state.join("runtime/retired.json")).unwrap(),
            before
        );
        assert_eq!(fs::read_dir(state.join("runtime")).unwrap().count(), 1);
        assert_eq!(fs::read_dir(&state).unwrap().count(), 1);
        assert!(!called.exists());
        let removal = f.run(&["install", "--remove"]);
        if reason == "herdr_handoff" {
            assert_eq!(removal.status.code(), Some(69));
        } else {
            assert_ne!(removal.status.code(), Some(69));
            assert!(
                String::from_utf8_lossy(&removal.stdout).contains("removal requires confirmation")
            );
        }
    }
}

#[cfg(feature = "test-support")]
#[test]
fn takeover_resumes_predecessors_and_doctor_reads_liveness_without_the_old_store() {
    use namedrop::{
        AgentConfigDirectories, Clock, LifecycleRuntime, ProcessIds, SystemClock,
        execute_lifecycle_operation, mux::fake::FakeMux,
    };
    let f = Fixture::new();
    let home = f.0.join("home");
    let old = f.0.join("non-default-old");
    let successor = f.0.join("successor");
    fs::create_dir_all(old.join("runtime")).unwrap();
    fs::write(old.join("runtime/owner"), "standalone\n").unwrap();
    fs::write(old.join("runtime/namedrop"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        old.join("runtime/namedrop"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    fs::create_dir_all(home.join(".config/namedrop")).unwrap();
    fs::write(
        home.join(".config/namedrop/state-directory"),
        format!("{}\n", old.display()),
    )
    .unwrap();
    let agent = namedrop::test_process_incarnation(std::process::id()).unwrap();
    fs::create_dir_all(old.join("rex/sessions")).unwrap();
    for block in ["block:first", "block:second"] {
        fs::write(
            old.join(format!("rex/sessions/{}.json", block.replace(':', "-"))),
            json!({"endpoint":"rex:/tmp/u7-down.sock","block":block,"agent":agent}).to_string(),
        )
        .unwrap();
    }
    fs::write(old.join("rex/sessions/broken.json"), "{").unwrap();
    fs::write(
        old.join("namedrop.sqlite3"),
        b"an old incompatible store must never be opened",
    )
    .unwrap();
    let mux = FakeMux::world()
        .endpoint("/tmp/u7-herdr.sock")
        .space("s1")
        .pane("p1")
        .build();
    let ids = ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: &successor,
        home_directory: Some(&home),
        agent_config: AgentConfigDirectories::default(),
        cwd: &f.0,
        workspace_id: Some("s1"),
        current_pane_id: Some("p1"),
        session_socket_id: "/tmp/u7-herdr.sock",
        mux: &mux,
        clock: &SystemClock,
        ids: &ids,
    };
    let report =
        execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime).unwrap();
    assert_eq!(report["handoff"].as_array().unwrap().len(), 1);
    assert!(
        report["handoff"][0]["notice"]
            .as_str()
            .unwrap()
            .contains("were not carried over")
    );
    assert_eq!(
        report["handoff"][0]["invalid_bindings"],
        json!([old.join("rex/sessions/broken.json")])
    );
    let registration = home.join(".config/namedrop/rex.json");
    let saved: Value = serde_json::from_slice(&fs::read(&registration).unwrap()).unwrap();
    assert_eq!(saved["predecessors"].as_array().unwrap().len(), 1);
    assert_eq!(saved["predecessors"][0]["sealed"], true);
    assert_eq!(
        saved["predecessors"][0]["agents"].as_array().unwrap().len(),
        2
    );
    let record = fs::read_to_string(home.join(".config/namedrop/state-directory")).unwrap();
    assert_eq!(record, format!("{}\n", successor.display()));
    let health = namedrop::diagnose_collaboration(&runtime);
    let warning = health
        .issues
        .iter()
        .find(|i| i.code == "ND-INSTALL-001")
        .unwrap();
    let text = serde_json::to_string(warning).unwrap();
    assert!(
        text.contains("2 agents on Rex still run")
            && text.contains("block:first")
            && text.contains("block:second")
            && text.contains(old.to_str().unwrap())
    );
    assert_eq!(
        fs::read(old.join("namedrop.sqlite3")).unwrap(),
        b"an old incompatible store must never be opened"
    );
    let again =
        execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime).unwrap();
    assert!(again.get("handoff").is_none());
    // An unsealed marker must resume even after a writer took the install record.
    let mut unsealed = saved.clone();
    unsealed["predecessors"][0]["sealed"] = json!(false);
    fs::write(&registration, unsealed.to_string()).unwrap();
    let resumed =
        execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime).unwrap();
    assert!(
        resumed["handoff"][0]["notice"]
            .as_str()
            .unwrap()
            .contains("were not carried over")
    );
    let resumed: Value = serde_json::from_slice(&fs::read(&registration).unwrap()).unwrap();
    assert_eq!(resumed["predecessors"].as_array().unwrap().len(), 1);
    assert_eq!(resumed["predecessors"][0]["sealed"], true);
    for marker in [
        json!({"reason":"removed","successor":null,"at":"now"}),
        json!({"reason":"herdr_handoff","successor":"/another","at":"now"}),
    ] {
        fs::write(old.join("runtime/retired.json"), marker.to_string()).unwrap();
        assert!(
            !namedrop::diagnose_collaboration(&runtime)
                .issues
                .iter()
                .any(|i| i.code == "ND-INSTALL-001")
        );
    }
    fs::remove_file(old.join("runtime/retired.json")).unwrap();
    assert!(
        !namedrop::diagnose_collaboration(&runtime)
            .issues
            .iter()
            .any(|i| i.code == "ND-INSTALL-001")
    );
    fs::write(old.join("runtime/retired.json"),json!({"reason":"herdr_handoff","successor":successor,"at":SystemClock.now_unix_ms().to_string()}).to_string()).unwrap();
    let mut ended = saved;
    for a in ended["predecessors"][0]["agents"].as_array_mut().unwrap() {
        a["agent"] = json!("1@999999999999");
    }
    fs::write(&registration, ended.to_string()).unwrap();
    assert!(
        !namedrop::diagnose_collaboration(&runtime)
            .issues
            .iter()
            .any(|i| i.code == "ND-INSTALL-001")
    );
}

/// The U6 review's F2: the Rex module is no agent package, so a module the
/// owner's refresh cannot write is reported on its own, on Herdr and on Rex,
/// and leaves both packages installed and activation successful.
#[cfg(feature = "test-support")]
#[test]
fn a_module_the_refresh_cannot_write_is_reported_alone_and_leaves_both_packages() {
    use namedrop::{
        AgentConfigDirectories, LifecycleRuntime, ProcessIds, SystemClock,
        execute_lifecycle_operation, mux::fake::FakeMux,
    };
    for endpoint in ["/tmp/u7-f2-herdr.sock", "rex:/tmp/u7-f2-never.sock"] {
        for shape in [
            "symlink",
            "unparsable",
            "unreadable",
            "missing-folder",
            "relative",
        ] {
            let f = Fixture::new();
            let home = f.0.join("home");
            let state = f.0.join("state");
            let config = f.0.join("cfg/init.lua");
            fs::create_dir_all(config.parent().unwrap()).unwrap();
            fs::write(&config, "-- person's config\n").unwrap();
            fs::create_dir_all(home.join(".config/namedrop")).unwrap();
            let registration = home.join(".config/namedrop/rex.json");
            let record = |module: &str| {
                fs::write(
                    &registration,
                    json!({"module":module,"config":config,"line":"inserted"}).to_string(),
                )
                .unwrap()
            };
            let linked = f.0.join("cfg/namedrop.lua");
            match shape {
                "symlink" => {
                    let target = f.0.join("dotfiles-namedrop.lua");
                    fs::write(&target, format!("{}\n", namedrop::rex_ui::MANAGED_LINE)).unwrap();
                    std::os::unix::fs::symlink(&target, &linked).unwrap();
                    record(linked.to_str().unwrap());
                }
                "unparsable" => fs::write(&registration, "{not json").unwrap(),
                "unreadable" => {
                    record(linked.to_str().unwrap());
                    fs::set_permissions(&registration, fs::Permissions::from_mode(0o000)).unwrap();
                }
                "missing-folder" => record(f.0.join("gone/namedrop.lua").to_str().unwrap()),
                _ => record("namedrop.lua"),
            }
            let mux = FakeMux::world()
                .endpoint(endpoint)
                .space("s1")
                .pane("p1")
                .build();
            let ids = ProcessIds::default();
            let runtime = LifecycleRuntime {
                state_directory: &state,
                home_directory: Some(&home),
                agent_config: AgentConfigDirectories::default(),
                cwd: &f.0,
                workspace_id: Some("s1"),
                current_pane_id: Some("p1"),
                session_socket_id: endpoint,
                mux: &mux,
                clock: &SystemClock,
                ids: &ids,
            };
            let report =
                execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime)
                    .unwrap_or_else(|error| panic!("{endpoint} {shape}: {error}"));
            for agent in ["codex", "claude-code"] {
                let package = &report["integration_packages"][agent];
                assert_eq!(
                    package["installed"], true,
                    "{endpoint} {shape} {agent}: {report}"
                );
                assert_eq!(package["error"], Value::Null, "{endpoint} {shape} {agent}");
                assert_ne!(package["configuration"], "installation_failed");
                assert_ne!(
                    report["agent_integration_hook_activation"][agent]["status"],
                    "installation_failed",
                    "{endpoint} {shape} {agent}"
                );
            }
            assert_eq!(report["rex_module"]["written"], false, "{endpoint} {shape}");
            assert!(
                report["rex_module"]["error"]
                    .as_str()
                    .is_some_and(|error| !error.is_empty()),
                "{endpoint} {shape}: {report}"
            );
            assert_eq!(fs::read_to_string(&config).unwrap(), "-- person's config\n");
            if shape == "unreadable" {
                fs::set_permissions(&registration, fs::Permissions::from_mode(0o600)).unwrap();
            }
        }
    }
}

/// A handoff whose step 5 cannot rewrite the module leaves it naming the old
/// launcher, so step 6 never seals the entry, even where it would seal with
/// no Rex call, and the next activation tries again.
#[cfg(feature = "test-support")]
#[test]
fn a_handoff_that_cannot_rewrite_the_module_stays_unsealed_and_succeeds() {
    use namedrop::{
        AgentConfigDirectories, LifecycleRuntime, ProcessIds, SystemClock,
        execute_lifecycle_operation, mux::fake::FakeMux,
    };
    let f = Fixture::new();
    let home = f.0.join("home");
    let old = f.0.join("old");
    let successor = f.0.join("successor");
    fs::create_dir_all(old.join("runtime")).unwrap();
    fs::write(old.join("runtime/owner"), "standalone\n").unwrap();
    fs::write(old.join("runtime/namedrop"), "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(
        old.join("runtime/namedrop"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    fs::create_dir_all(home.join(".config/namedrop")).unwrap();
    fs::write(
        home.join(".config/namedrop/state-directory"),
        format!("{}\n", old.display()),
    )
    .unwrap();
    let target = f.0.join("dotfiles-namedrop.lua");
    fs::write(&target, format!("{}\n", namedrop::rex_ui::MANAGED_LINE)).unwrap();
    let module = f.0.join("cfg/namedrop.lua");
    fs::create_dir_all(module.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, &module).unwrap();
    let registration = home.join(".config/namedrop/rex.json");
    // No server: a step 6 that ran would seal at once without a Rex call.
    fs::write(
        &registration,
        json!({"module":module,"config":f.0.join("cfg/init.lua"),"line":"inserted"}).to_string(),
    )
    .unwrap();
    let mux = FakeMux::world()
        .endpoint("/tmp/u7-step5-herdr.sock")
        .space("s1")
        .pane("p1")
        .build();
    let ids = ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: &successor,
        home_directory: Some(&home),
        agent_config: AgentConfigDirectories::default(),
        cwd: &f.0,
        workspace_id: Some("s1"),
        current_pane_id: Some("p1"),
        session_socket_id: "/tmp/u7-step5-herdr.sock",
        mux: &mux,
        clock: &SystemClock,
        ids: &ids,
    };
    for _ in 0..2 {
        let report =
            execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime).unwrap();
        let module_report = &report["handoff"][0]["module"];
        assert_eq!(module_report["status"], "not_confirmed", "{report}");
        assert_eq!(module_report["sealed"], false);
        assert!(
            module_report["error"]
                .as_str()
                .is_some_and(|e| e.contains("symbolic link")),
            "{module_report}"
        );
        let saved: Value = serde_json::from_slice(&fs::read(&registration).unwrap()).unwrap();
        assert_eq!(saved["predecessors"][0]["sealed"], false, "{saved}");
        assert_eq!(report["integration_packages"]["codex"]["installed"], true);
    }
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        format!("{}\n", namedrop::rex_ui::MANAGED_LINE)
    );
}

#[test]
fn installer_round_trips_guarded_config_and_rolls_back_rpc_errors() {
    let output = binary::command("python3")
        .args([
            "-I",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/rex_installer.py"),
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

#[test]
fn a_binding_hook_queued_behind_the_handoff_gate_stays_quiet() {
    use std::{io::Write, time::Duration};
    let f = Fixture::new();
    let state = f.0.join("state");
    let gate = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(state.join("runtime/commands.lock"))
        .unwrap();
    gate.lock().unwrap();
    let mut child = binary::namedrop()
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", f.0.join("home"))
        .args([
            "hook",
            "session-start",
            "--state-directory",
            state.to_str().unwrap(),
        ])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"{}").unwrap();
    let gate_path = state.join("runtime/commands.lock");
    let until = std::time::Instant::now() + Duration::from_secs(10);
    while !child_opens_gate(child.id(), &gate_path) {
        assert!(
            child.try_wait().unwrap().is_none(),
            "hook ended before reaching the gate"
        );
        if std::time::Instant::now() >= until {
            let pid = child.id();
            drop(gate);
            let output = child.wait_with_output().unwrap();
            panic!("hook PID {pid} never opened the gate: {output:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    fs::write(
        state.join("runtime/retired.json"),
        json!({"reason":"herdr_handoff","successor":"/herdr","at":"now"}).to_string(),
    )
    .unwrap();
    drop(gate);
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success() && result.stdout.is_empty() && result.stderr.is_empty(),
        "{result:?}"
    );
    assert_eq!(fs::read_dir(&state).unwrap().count(), 1);
}

fn child_opens_gate(pid: u32, path: &std::path::Path) -> bool {
    namedrop::test_process_opens(pid, path)
}
