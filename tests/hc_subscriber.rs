use namedrop::{HcHost, HerdrCommandExecutor, HerdrCommandOutput};
use serde_json::{Value, json};
use std::{cell::RefCell, path::Path};

struct Fixture {
    terminals: RefCell<Vec<Value>>,
    calls: RefCell<Vec<Vec<String>>>,
    send_error: Option<String>,
    get_failure: RefCell<Option<String>>,
    fail_ls_at: RefCell<Option<usize>>,
    read_text: Option<String>,
    clear_error: RefCell<Option<String>>,
    rename_error: RefCell<bool>,
}
impl Default for Fixture {
    fn default() -> Self {
        Self {
            terminals: RefCell::new(vec![
                serde_json::from_str(include_str!("fixtures/hc/get-labeled.stdout")).unwrap(),
            ]),
            calls: RefCell::new(Vec::new()),
            send_error: None,
            get_failure: RefCell::new(None),
            fail_ls_at: RefCell::new(None),
            read_text: None,
            clear_error: RefCell::new(None),
            rename_error: RefCell::new(false),
        }
    }
}
impl HerdrCommandExecutor for Fixture {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|arg| (*arg).to_owned()).collect());
        let stdout = match args {
            ["ls", "--json"]
                if self.fail_ls_at.borrow().is_some_and(|limit| {
                    self.calls
                        .borrow()
                        .iter()
                        .filter(|call| call[0] == "ls")
                        .count()
                        >= limit
                }) =>
            {
                return Err("postcommit discovery failed".into());
            }
            ["ls", "--json"] => json!({"terminals": *self.terminals.borrow()}).to_string(),
            ["get", id, "--json"] if self.get_failure.borrow().as_deref() == Some(id) => {
                return Err("terminal inspection failed".into());
            }
            ["get", id, "--json"] => self
                .terminals
                .borrow()
                .iter()
                .find(|terminal| terminal["address"] == *id)
                .ok_or_else(|| include_str!("fixtures/hc/not-found.stderr").to_owned())?
                .to_string(),
            ["read", ..] => self
                .read_text
                .as_ref()
                .map(|text| json!({"text":text}).to_string())
                .unwrap_or_else(|| include_str!("fixtures/hc/read.stdout").to_owned()),
            ["send", ..] => {
                if let Some(error) = &self.send_error {
                    return Err(error.clone());
                }
                include_str!("fixtures/hc/send.stdout").to_owned()
            }
            ["server", "status", "--json"] => {
                include_str!("fixtures/hc/server-status.stdout").to_owned()
            }
            ["--version"] => "herdr-core 0.1.14\n".to_owned(),
            ["label", id, labels @ ..] => {
                if *self.rename_error.borrow()
                    && (labels.contains(&"namedrop.name=bob")
                        || (*id == "t2" && labels.contains(&"namedrop.name=alice")))
                {
                    return Err("label replacement failed".into());
                }
                if labels.contains(&"namedrop.name=")
                    && let Some(error) = self.clear_error.borrow().as_ref()
                {
                    return Err(error.clone());
                }
                let mut terminals = self.terminals.borrow_mut();
                let terminal = terminals
                    .iter_mut()
                    .find(|terminal| terminal["address"] == *id)
                    .unwrap();
                for label in labels.iter().filter(|label| **label != "--json") {
                    let (key, value) = label.split_once('=').unwrap();
                    terminal["labels"][key] = json!(value);
                }
                "{}".into()
            }
            ["interrupt", ..] => "{}".into(),
            other => panic!("unexpected hc argv {other:?}"),
        };
        Ok(HerdrCommandOutput { stdout })
    }
}

fn check(
    name: &str,
    run: impl FnOnce(
        &Fixture,
        &namedrop::LifecycleRuntime<
            '_,
            HcHost<&Fixture>,
            namedrop::SystemClock,
            namedrop::ProcessIds,
        >,
    ),
) {
    let fixture = Fixture::default();
    let state =
        std::env::temp_dir().join(format!("namedrop-subscriber-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    let home = state.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let clock = namedrop::SystemClock;
    let ids = namedrop::ProcessIds::default();
    let runtime = namedrop::LifecycleRuntime {
        state_directory: &state,
        home_directory: Some(&home),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new(env!("CARGO_MANIFEST_DIR")),
        workspace_id: Some("fixture-space"),
        current_pane_id: Some("t1"),
        session_socket_id: "hc:fixture",
        host: HcHost::new(&fixture),
        clock: &clock,
        ids: &ids,
    };
    namedrop::execute_lifecycle_operation(
        "name",
        json!({"alias":"alice", "pane_type":"terminal"}),
        &runtime,
    )
    .unwrap();
    run(&fixture, &runtime);
    std::fs::remove_dir_all(state).unwrap();
}
fn labels() -> String {
    json!({"method":"event", "params":{"type":"labels","terminal":"t1"}}).to_string()
}
#[test]
fn rejected_name_is_correlated_and_returned() {
    check("rejected", |fixture, runtime| {
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.name"] = json!("bad name");
        let result =
            namedrop::host::hc::events::Subscriber::default().handle_line(&labels(), runtime);
        assert!(
            result.is_err(),
            "name rejection must be returned, got {result:?}"
        );
        let terminal = &fixture.terminals.borrow()[0];
        let error: Value =
            serde_json::from_str(terminal["labels"]["namedrop.name.error"].as_str().unwrap())
                .unwrap();
        assert_eq!(error["requested"], "bad name");
        assert!(!error["message"].as_str().unwrap().is_empty());
        assert_eq!(terminal["labels"]["namedrop.name"], "alice");
    });
}
#[test]
fn missed_reconciles_healthy_terminal_after_failed_get() {
    check("partial", |fixture, runtime| {
        let mut second = fixture.terminals.borrow()[0].clone();
        second["address"] = json!("t2");
        second["labels"]["namedrop.name"] = json!("bob");
        fixture.terminals.borrow_mut().push(second);
        *fixture.get_failure.borrow_mut() = Some("t1".into());
        let result = namedrop::host::hc::events::Subscriber::default()
            .handle_line(r#"{"method":"missed","params":{}}"#, runtime);
        assert!(result.is_err());
        let addresses = namedrop::StateStore::open(runtime.state_directory)
            .unwrap()
            .transaction(|tx| {
                tx.list_pane_addresses(&namedrop::StateNamespace::for_space(
                    "hc:fixture",
                    "fixture-space",
                ))
            })
            .unwrap();
        assert!(
            addresses
                .iter()
                .any(|a| a.alias == "bob" && a.pane_id.as_deref() == Some("t2")),
            "healthy terminal was skipped: {addresses:?}"
        );
    });
}
#[test]
fn restarted_subscriber_moves_durable_binding() {
    for trigger in ["labels", "snapshot", "missed"] {
        check(&format!("move-{trigger}"), |fixture, runtime| {
            fixture.terminals.borrow_mut()[0]["labels"]["herdr.space"] = json!("destination");
            let destination = namedrop::LifecycleRuntime {
                workspace_id: Some("destination"),
                ..*runtime
            };
            namedrop::execute_lifecycle_operation(
                "activate",
                json!({"confirmed":true}),
                &destination,
            )
            .unwrap();
            let line = match trigger {
                "snapshot" => json!({"terminals":*fixture.terminals.borrow()}).to_string(),
                "missed" => json!({"method":"missed","params":{}}).to_string(),
                _ => labels(),
            };
            namedrop::host::hc::events::Subscriber::default()
                .handle_line(&line, runtime)
                .unwrap();
            let mut store = namedrop::StateStore::open(runtime.state_directory).unwrap();
            let old = store
                .transaction(|tx| {
                    tx.list_pane_addresses(&namedrop::StateNamespace::for_space(
                        "hc:fixture",
                        "fixture-space",
                    ))
                })
                .unwrap();
            assert!(
                old.iter().all(|a| a.pane_id.is_none()),
                "old binding survived move: {old:?}"
            );
            let new = store
                .transaction(|tx| {
                    tx.list_pane_addresses(&namedrop::StateNamespace::for_space(
                        "hc:fixture",
                        "destination",
                    ))
                })
                .unwrap();
            assert_eq!(
                new.iter()
                    .filter(|a| a.pane_id.as_deref() == Some("t1"))
                    .count(),
                1
            );
        });
    }
}

#[path = "support/hc_socket.rs"]
mod hc_socket;

#[test]
fn detached_subscriber_process_lifecycle() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("namedrop-detached-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("home")).unwrap();
    let socket = root.join("core.sock");
    let _socket = hc_socket::HcSocket::bind(&socket).unwrap();
    let hc = root.join("hc");
    std::fs::write(
        &hc,
        format!(
            r#"#!/usr/bin/python3
import json, os, sys, time
with open(os.environ['HC_TEST_LOG'], 'a') as log:
    log.write(json.dumps({{'args':sys.argv[1:], 'env':dict(os.environ)}})+'\n')
if sys.argv[1:] == ['server', 'status', '--json']:
    print({status:?})
elif sys.argv[1:] == ['events', '--json']:
    while not os.path.exists(os.environ['SUBSCRIBER_TEST_ROOT']+'/disconnect'):
        print('{{"method":"missed","params":{{}}}}', flush=True)
        time.sleep(.05)
elif sys.argv[1:] == ['ls', '--json']:
    print('{{"terminals":[]}}')
else:
    raise SystemExit('unexpected hc command '+str(sys.argv))
"#,
            status = include_str!("fixtures/hc/server-status.stdout")
        ),
    )
    .unwrap();
    std::fs::set_permissions(&hc, std::fs::Permissions::from_mode(0o700)).unwrap();
    let binary = root.join("namedrop");
    std::fs::copy(env!("CARGO_BIN_EXE_namedrop"), &binary).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "detached_subscriber_child", "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("xdg"))
        .env("XDG_RUNTIME_DIR", &root)
        .env("CODEX_HOME", root.join("codex"))
        .env("CLAUDE_CONFIG_DIR", root.join("claude"))
        .env("HC_BIN_PATH", &hc)
        .env("HC_TERMINAL_SOCKET", &socket)
        .env("HC_TERMINAL_ID", "forbidden")
        .env("HERDR_SOCKET_PATH", "forbidden")
        .env("HERDR_CUSTOM", "forbidden")
        .env("HC_TEST_LOG", root.join("calls.jsonl"))
        .env("SUBSCRIBER_TEST_ROOT", &root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let calls = std::fs::read_to_string(root.join("calls.jsonl")).unwrap();
    let events: Vec<Value> = calls
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|call| call["args"] == json!(["events", "--json"]))
        .collect();
    assert!(!events.is_empty());
    for call in events {
        let env = call["env"].as_object().unwrap();
        assert!(env.keys().all(|key| !key.starts_with("HERDR_")));
        assert!(!env.contains_key("HC_TERMINAL_ID"));
        assert!(!env.contains_key("HC_TERMINAL_SOCKET"));
        assert_eq!(env["HC_SOCKET"], socket.to_str().unwrap());
        assert_eq!(env["HOME"], root.join("home").to_str().unwrap());
        assert_eq!(env["CODEX_HOME"], root.join("codex").to_str().unwrap());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn detached_subscriber_child() {
    let Some(root) = std::env::var_os("SUBSCRIBER_TEST_ROOT").map(std::path::PathBuf::from) else {
        return;
    };
    let fixture = Fixture::default();
    let host = HcHost::new(&fixture);
    let state = root.join("state");
    let binary = root.join("namedrop");
    std::fs::create_dir_all(state.join("runtime")).unwrap();
    namedrop::host::hc::events::start(&host, &binary, &state).unwrap();
    let directory = std::fs::read_dir(state.join("hc-subscribers"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let record = || {
        serde_json::from_slice::<Value>(&std::fs::read(directory.join("subscriber.json")).unwrap())
            .unwrap()
    };
    let first = record();
    let pid = first["pid"].as_u64().unwrap() as i32;
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ =
                namedrop::host::hc::events::stop(&self.0, "hc:628bcf8129aa037b1fa5aad3504926a6");
        }
    }
    let _cleanup = Cleanup(state.clone());
    std::fs::create_dir_all(state.join("runtime")).unwrap();
    let gate = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(state.join("runtime/commands.lock"))
        .unwrap();
    gate.lock().unwrap();
    let namespace =
        namedrop::StateNamespace::for_space("hc:628bcf8129aa037b1fa5aad3504926a6", "fixture-space");
    namedrop::StateStore::open(&state)
        .unwrap()
        .transaction(|tx| {
            tx.activate_worktree(
                &namespace,
                &namedrop::MutationAttribution::new(namedrop::ActorKind::Human, "fixture", 0),
            )
        })
        .unwrap();
    let database = state.join("namedrop.sqlite3");
    let epoch = || {
        rusqlite::Connection::open(&database)
            .unwrap()
            .query_row("SELECT epoch FROM reconciliation_epochs", [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap()
    };
    std::thread::sleep(std::time::Duration::from_millis(350));
    assert_eq!(
        epoch(),
        0,
        "subscriber wrote while the update guard was exclusive"
    );
    drop(gate);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while epoch() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "subscriber dropped queued event after waiting for update guard"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    namedrop::host::hc::events::start(&host, &binary, &state).unwrap();
    assert_eq!(record()["pid"], first["pid"]);
    let starters: Vec<_> = (0..3)
        .map(|_| {
            let state = state.clone();
            let binary = binary.clone();
            std::thread::spawn(move || {
                let fixture = Fixture::default();
                namedrop::host::hc::events::start(&HcHost::new(&fixture), &binary, &state).unwrap();
            })
        })
        .collect();
    for starter in starters {
        starter.join().unwrap();
    }
    assert_eq!(
        record()["pid"],
        first["pid"],
        "concurrent activation created duplicate subscribers"
    );
    assert_eq!(
        unsafe { libc::getsid(pid) },
        pid,
        "subscriber was not detached"
    );
    let replacement = root.join("replacement");
    std::fs::copy(&binary, &replacement).unwrap();
    std::fs::rename(replacement, &binary).unwrap();
    namedrop::host::hc::events::start(&host, &binary, &state).unwrap();
    let second = record();
    let second_pid = second["pid"].as_u64().unwrap() as i32;
    assert_ne!(
        second["pid"], first["pid"],
        "activation retained stale subscriber"
    );
    let replacement = root.join("replacement");
    std::fs::copy(&binary, &replacement).unwrap();
    std::fs::rename(replacement, &binary).unwrap();
    let wait_exit = |pid: i32| {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let exited =
                std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
                    stat.rsplit_once(')')
                        .unwrap()
                        .1
                        .trim_start()
                        .starts_with('Z')
                });
            if exited {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "subscriber did not exit after replacement or stopped core"
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    };
    wait_exit(second_pid);
    let mut recycled = second;
    recycled["pid"] = json!(std::process::id());
    recycled["process_start"] = json!("recycled-pid");
    std::fs::write(directory.join("subscriber.json"), recycled.to_string()).unwrap();
    std::fs::create_dir_all(state.join("runtime/current")).unwrap();
    std::os::unix::fs::symlink(&binary, state.join("runtime/current/namedrop")).unwrap();
    let launcher = state.join("runtime/namedrop");
    std::fs::write(
        &launcher,
        "#!/bin/sh\nexec \"$(dirname \"$0\")/current/namedrop\" \"$@\"\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o700)).unwrap();
    namedrop::host::hc::events::start(&host, &launcher, &state).unwrap();
    let stopped_pid = record()["pid"].as_u64().unwrap() as i32;
    namedrop::host::hc::events::stop(&state, "hc:628bcf8129aa037b1fa5aad3504926a6").unwrap();
    wait_exit(stopped_pid);
    namedrop::host::hc::events::start(&host, &launcher, &state).unwrap();
    let selected_pid = record()["pid"].as_u64().unwrap() as i32;
    let successor = root.join("successor");
    std::fs::copy(&binary, &successor).unwrap();
    std::fs::remove_file(state.join("runtime/current/namedrop")).unwrap();
    std::os::unix::fs::symlink(&successor, state.join("runtime/current/namedrop")).unwrap();
    wait_exit(selected_pid);
    namedrop::host::hc::events::start(&host, &launcher, &state).unwrap();
    let final_pid = record()["pid"].as_u64().unwrap() as i32;
    std::fs::remove_file(std::env::var_os("HC_TERMINAL_SOCKET").unwrap()).unwrap();
    std::fs::write(root.join("disconnect"), "").unwrap();
    wait_exit(final_pid);
}

#[test]
fn failed_error_publication_preserves_requested_name() {
    check("publish-failed", |fixture, runtime| {
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.name"] = json!("bad name");
        *fixture.rename_error.borrow_mut() = true;
        fixture.terminals.borrow_mut()[0]["address"] = json!("t2");
        let event =
            json!({"method":"event", "params":{"type":"labels","terminal":"t2"}}).to_string();
        *fixture.clear_error.borrow_mut() = Some("response publication failed".into());
        let result = namedrop::host::hc::events::Subscriber::default().handle_line(&event, runtime);
        assert!(
            result
                .unwrap_err()
                .contains("could not publish name rejection")
        );
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.name"],
            "bad name"
        );
    });
}

#[test]
fn successful_retry_clears_error_and_generic_labels_do_not_rename() {
    check("retry", |fixture, runtime| {
        let mut subscriber = namedrop::host::hc::events::Subscriber::default();
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.name"] = json!("bad name");
        assert!(subscriber.handle_line(&labels(), runtime).is_err());
        fixture.calls.borrow_mut().clear();
        subscriber.handle_line(&labels(), runtime).unwrap();
        assert!(fixture.calls.borrow().iter().all(|call| call[0] != "label"));
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.name"] = json!("bob");
        subscriber.handle_line(&labels(), runtime).unwrap();
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.name.error"],
            ""
        );
    });
}

#[test]
fn doctor_reports_partial_discovery_and_activation_repair_without_writes() {
    check("doctor", |fixture, runtime| {
        *fixture.get_failure.borrow_mut() = Some("t1".into());
        fixture.calls.borrow_mut().clear();
        let mut report = namedrop::diagnose_collaboration(runtime);
        namedrop::append_hc_diagnostics(&mut report, runtime, &runtime.host);
        let warning = report
            .issues
            .iter()
            .find(|issue| issue.code == "ND-HC-DISCOVERY-001")
            .unwrap();
        assert_eq!(warning.diagnostic["terminal"], "t1");
        assert_eq!(warning.diagnostic["message"], "terminal inspection failed");
        let subscriber = report
            .issues
            .iter()
            .find(|issue| issue.code == "ND-HC-SUBSCRIBER-001")
            .unwrap();
        assert_eq!(subscriber.repair.operation.as_deref(), Some("activate"));
        assert!(
            fixture
                .calls
                .borrow()
                .iter()
                .all(|call| !matches!(call[0].as_str(), "label" | "run" | "send"))
        );
        assert!(!runtime.state_directory.join("hc-subscribers").exists());
    });
}

#[test]
fn postcommit_failure_preserves_the_committed_alias() {
    check("postcommit", |fixture, runtime| {
        *fixture.fail_ls_at.borrow_mut() = Some(
            fixture
                .calls
                .borrow()
                .iter()
                .filter(|call| call[0] == "ls")
                .count()
                + 2,
        );
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.name"] = json!("bob");
        let result =
            namedrop::host::hc::events::Subscriber::default().handle_line(&labels(), runtime);
        assert!(result.is_err(), "postcommit failure was not exercised");
        let addresses = namedrop::StateStore::open(runtime.state_directory)
            .unwrap()
            .transaction(|tx| {
                tx.list_pane_addresses(&namedrop::StateNamespace::for_space(
                    "hc:fixture",
                    "fixture-space",
                ))
            })
            .unwrap();
        assert_eq!(addresses[0].alias, "bob");
        let terminal = &fixture.terminals.borrow()[0];
        assert_eq!(terminal["labels"]["namedrop.name"], "bob");
        let error: Value =
            serde_json::from_str(terminal["labels"]["namedrop.name.error"].as_str().unwrap())
                .unwrap();
        assert_eq!(error["requested"], "bob");
        assert!(
            error["message"]
                .as_str()
                .unwrap()
                .contains("postcommit discovery failed")
        );
    });
}

#[test]
fn stopped_core_error_survives_doctor_composition() {
    let root = std::env::temp_dir().join(format!("namedrop-stopped-doctor-{}", std::process::id()));
    std::fs::create_dir_all(root.join("home")).unwrap();
    for inherited in [None, Some("stale-terminal")] {
        let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_namedrop"));
        command
            .args(["--host", "hc", "--plugin-state"])
            .arg(root.join("state"))
            .args(["--hc-space", "fixture-space", "doctor"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root.join("home"))
            .env("XDG_CONFIG_HOME", root.join("config"))
            .env("XDG_STATE_HOME", root.join("xdg"))
            .env("XDG_RUNTIME_DIR", &root)
            .env("CODEX_HOME", root.join("codex"))
            .env("CLAUDE_CONFIG_DIR", root.join("claude"))
            .env("HC_SOCKET", root.join("missing.sock"))
            .env("HC_BIN_PATH", root.join("hc-must-not-run"));
        if let Some(pane) = inherited {
            command.env("HC_TERMINAL_ID", pane);
        }
        let output = command.output().unwrap();
        let response = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!output.status.success());
        assert!(
            response.contains("hc core is unavailable"),
            "lost core preflight error: {response}"
        );
        assert!(response.contains("missing.sock"));
        assert!(!root.join("missing.sock").exists());
    }
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn unreadable_agent_is_unknown_in_doctor_not_disconnected() {
    check("unknown-doctor", |fixture, runtime| {
        namedrop::execute_lifecycle_operation(
            "name",
            json!({"alias":"alice","pane_type":"agent"}),
            runtime,
        )
        .unwrap();
        *fixture.get_failure.borrow_mut() = Some("t1".into());
        let report = namedrop::diagnose_collaboration(runtime);
        assert!(
            report
                .issues
                .iter()
                .all(|issue| !matches!(issue.code.as_str(), "ND-SESSION-001" | "ND-SETUP-003")),
            "unknown inspection became a missing-agent diagnosis: {:?}",
            report.issues
        );
        assert_eq!(report.available_agents, 0);
    });
}
