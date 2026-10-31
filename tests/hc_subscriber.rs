#[path = "support/environment.rs"]
mod environment;
use environment::CommandHome;

#[allow(dead_code)]
#[path = "fixtures/hc/process.rs"]
mod process;
use namedrop::{HcHost, HerdrCommandExecutor, HerdrCommandOutput};
use serde_json::{Value, json};
use std::{cell::RefCell, path::Path};

struct Fixture {
    _shell: process::AgentProcess,
    terminals: RefCell<Vec<Value>>,
    calls: RefCell<Vec<Vec<String>>>,
    send_error: Option<String>,
    get_failure: RefCell<Option<String>>,
    fail_ls_at: RefCell<Option<usize>>,
    read_text: Option<String>,
    clear_error: RefCell<Option<String>>,
    rename_error: RefCell<bool>,
    name_write_failure: RefCell<Option<bool>>,
    session_clear_failure: RefCell<bool>,
    session_clear_state: RefCell<Option<std::path::PathBuf>>,
    replacement_on_get: RefCell<Option<(usize, Value)>>,
    counter_failure: RefCell<bool>,
}
impl Default for Fixture {
    fn default() -> Self {
        let shell = process::AgentProcess::shell();
        let mut terminal: Value =
            serde_json::from_str(include_str!("fixtures/hc/get-labeled.stdout")).unwrap();
        terminal["pid"] = json!(shell.pid());
        terminal["foreground"]["pid"] = json!(shell.pid());
        Self {
            _shell: shell,
            terminals: RefCell::new(vec![terminal]),
            calls: RefCell::new(Vec::new()),
            send_error: None,
            get_failure: RefCell::new(None),
            fail_ls_at: RefCell::new(None),
            read_text: None,
            clear_error: RefCell::new(None),
            rename_error: RefCell::new(false),
            name_write_failure: RefCell::new(None),
            session_clear_failure: RefCell::new(false),
            session_clear_state: RefCell::new(None),
            replacement_on_get: RefCell::new(None),
            counter_failure: RefCell::new(false),
        }
    }
}
impl HerdrCommandExecutor for Fixture {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|arg| (*arg).to_owned()).collect());
        if matches!(args, ["get", "t1", "--json"]) {
            let mut replacement = self.replacement_on_get.borrow_mut();
            if let Some((remaining, terminal)) = replacement.as_mut() {
                *remaining -= 1;
                if *remaining == 0 {
                    self.terminals.borrow_mut()[0] = terminal.clone();
                    *replacement = None;
                }
            }
        }
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
                if labels.contains(&"namedrop.session=")
                    && let Some(state) = self.session_clear_state.borrow().as_ref()
                {
                    let namespace =
                        namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
                    let addresses = namedrop::StateStore::open(state)
                        .unwrap()
                        .transaction(|tx| tx.list_pane_addresses(&namespace))
                        .unwrap();
                    let address = addresses
                        .iter()
                        .find(|address| address.alias == "alice")
                        .unwrap();
                    assert_eq!(
                        address.agent_session_id.as_ref().map(|id| id.as_str()),
                        Some("original-session"),
                        "the session must remain bound until its labels clear"
                    );
                }
                if labels.contains(&"namedrop.session=") && *self.session_clear_failure.borrow() {
                    return Err("session cleanup failed".into());
                }
                if *self.counter_failure.borrow()
                    && labels
                        .iter()
                        .any(|label| label.starts_with("namedrop.awaiting="))
                {
                    return Err("postcommit counter publication failed".into());
                }
                let failure = if labels
                    .iter()
                    .any(|label| label.starts_with("namedrop.name="))
                {
                    self.name_write_failure.borrow_mut().take()
                } else {
                    None
                };
                if failure == Some(false) {
                    return Err("transient name write failed before applying".into());
                }

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
                if failure == Some(true) {
                    return Err("transient name write failed after applying".into());
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
        result.unwrap();
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
fn restarted_subscriber_clears_moved_alias_without_implicit_destination_claim() {
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
            assert_eq!(fixture.terminals.borrow()[0]["labels"]["namedrop.name"], "");
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
                0
            );
        });
    }
}

#[path = "support/hc_socket.rs"]
mod hc_socket;

#[test]
fn detached_subscriber_process_lifecycle() {
    use std::os::unix::fs::PermissionsExt;
    let root = hc_socket::fixture::short_socket_root();
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
    with open(os.environ['SUBSCRIBER_TEST_ROOT']+'/stream-ended', 'w') as ended:
        ended.write(str(os.getpid()))
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
        .isolated_env(root.join("home"))
        .env("PATH", "/usr/bin:/bin")
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_STATE_HOME", root.join("xdg"))
        .env("XDG_RUNTIME_DIR", &root)
        .env("CODEX_HOME", root.join("codex"))
        .env("CLAUDE_CONFIG_DIR", root.join("claude"))
        .env("HC_BIN_PATH", &hc)
        .env("HC_SOCKET", &socket)
        .env("HC_TERMINAL_SOCKET", "unused")
        .env("HC_SCOPE", "terminal-lifetime-token")
        .env("HC_RUNTIME", "terminal-runtime-token")
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
    let environment = namedrop::environment::EnvironmentSnapshot::capture();
    let fixture = Fixture::default();
    let host = HcHost::new(&fixture);
    let state = root.join("state");
    let binary = root.join("namedrop");
    std::fs::create_dir_all(state.join("runtime")).unwrap();
    namedrop::host::hc::events::start_with_environment(&host, &binary, &state, &environment)
        .unwrap();
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
    let child_environment = std::fs::read(format!("/proc/{pid}/environ")).unwrap();
    for token in [b"HC_SCOPE=".as_slice(), b"HC_RUNTIME=", b"HC_TERMINAL_"] {
        assert!(
            !child_environment
                .split(|byte| *byte == 0)
                .any(|entry| entry.starts_with(token))
        );
    }

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
    namedrop::host::hc::events::start_with_environment(&host, &binary, &state, &environment)
        .unwrap();
    assert_eq!(record()["pid"], first["pid"]);
    let starters: Vec<_> = (0..3)
        .map(|_| {
            let state = state.clone();
            let binary = binary.clone();
            std::thread::spawn(move || {
                let fixture = Fixture::default();
                namedrop::host::hc::events::start_with_environment(
                    &HcHost::new(&fixture),
                    &binary,
                    &state,
                    &namedrop::environment::EnvironmentSnapshot::capture(),
                )
                .unwrap();
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
    let doctor = std::process::Command::new(&binary)
        .env_remove("HC_TERMINAL_ID")
        .args(["--host", "hc", "--plugin-state"])
        .arg(&state)
        .args(["--hc-space", "fixture-space", "doctor"])
        .output()
        .unwrap();
    assert!(
        !doctor.stdout.is_empty(),
        "{}",
        String::from_utf8_lossy(&doctor.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&doctor.stdout).contains("ND-HC-SUBSCRIBER-001"),
        "{}",
        String::from_utf8_lossy(&doctor.stdout)
    );
    let second = record();
    let second_pid = second["pid"].as_u64().unwrap() as i32;
    assert_ne!(
        second["pid"], first["pid"],
        "doctor retained stale subscriber"
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
                "subscriber did not exit after replacement or verified stop"
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
    namedrop::host::hc::events::start_with_environment(&host, &launcher, &state, &environment)
        .unwrap();
    let cooperative = record();
    let mut stale_request = cooperative.clone();
    stale_request["process_start"] = json!("recycled-pid");
    std::fs::write(
        directory.join("stop-request.json"),
        stale_request.to_string(),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(600));
    namedrop::host::hc::events::status(&state, "hc:628bcf8129aa037b1fa5aad3504926a6").unwrap();
    let mut wrong_binary = cooperative.clone();
    wrong_binary["binary"]["inode"] = json!(0);
    std::fs::write(
        directory.join("stop-request.json"),
        wrong_binary.to_string(),
    )
    .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(600));
    namedrop::host::hc::events::status(&state, "hc:628bcf8129aa037b1fa5aad3504926a6").unwrap();
    std::fs::write(directory.join("stop-request.json"), cooperative.to_string()).unwrap();
    wait_exit(cooperative["pid"].as_u64().unwrap() as i32);
    namedrop::host::hc::events::start_with_environment(&host, &launcher, &state, &environment)
        .unwrap();
    let stopped_pid = record()["pid"].as_u64().unwrap() as i32;
    namedrop::host::hc::events::stop(&state, "hc:628bcf8129aa037b1fa5aad3504926a6").unwrap();
    wait_exit(stopped_pid);
    namedrop::host::hc::events::start_with_environment(&host, &launcher, &state, &environment)
        .unwrap();
    let selected_pid = record()["pid"].as_u64().unwrap() as i32;
    let successor = root.join("successor");
    std::fs::copy(&binary, &successor).unwrap();
    std::fs::remove_file(state.join("runtime/current/namedrop")).unwrap();
    std::os::unix::fs::symlink(&successor, state.join("runtime/current/namedrop")).unwrap();
    wait_exit(selected_pid);
    let before_restart_epoch = epoch();
    namedrop::host::hc::events::start_with_environment(&host, &launcher, &state, &environment)
        .unwrap();
    let final_pid = record()["pid"].as_u64().unwrap() as i32;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while epoch() <= before_restart_epoch {
        assert!(
            std::time::Instant::now() < deadline,
            "final subscriber stream did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let socket = std::path::PathBuf::from(std::env::var_os("HC_SOCKET").unwrap());
    std::fs::remove_file(&socket).unwrap();
    std::fs::write(root.join("disconnect"), "").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !root.join("stream-ended").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "fake stream did not disconnect"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    std::thread::sleep(std::time::Duration::from_millis(500));
    let calls = || std::fs::read_to_string(root.join("calls.jsonl")).unwrap();
    let absent_calls = calls();
    let absent_epoch = epoch();
    std::thread::sleep(std::time::Duration::from_millis(750));
    namedrop::host::hc::events::status(&state, "hc:628bcf8129aa037b1fa5aad3504926a6")
        .expect("subscriber exited instead of waiting for the stopped core");
    assert_eq!(record()["pid"], json!(final_pid));
    assert_eq!(
        calls(),
        absent_calls,
        "subscriber invoked hc while the socket was absent"
    );
    std::fs::remove_file(root.join("disconnect")).unwrap();
    let _reconnected = hc_socket::HcSocket::bind(&socket).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while epoch() <= absent_epoch {
        assert!(
            std::time::Instant::now() < deadline,
            "reconnected stream did not reconcile its missed event"
        );
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert_eq!(
        record()["pid"],
        json!(final_pid),
        "reconnect replaced the subscriber"
    );
    assert!(
        calls()[absent_calls.len()..].lines().any(|line| {
            serde_json::from_str::<Value>(line).unwrap()["args"] == json!(["events", "--json"])
        }),
        "subscriber did not reopen the stream"
    );
    std::fs::remove_file(&socket).unwrap();
    std::fs::remove_file(root.join("stream-ended")).unwrap();
    std::fs::write(root.join("disconnect"), "").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !root.join("stream-ended").exists() {
        assert!(std::time::Instant::now() < deadline);
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    std::fs::write(directory.join("stop-request.json"), record().to_string()).unwrap();
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
        *fixture.counter_failure.borrow_mut() = true;
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.awaiting"] = json!("-1");
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
        assert!(
            result
                .unwrap_err()
                .contains("postcommit counter publication failed")
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
            .isolated_env(root.join("home"))
            .env("PATH", "/usr/bin:/bin")
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

#[test]
fn snapshot_repairs_a_missing_name_mirror() {
    check("mirror", |fixture, runtime| {
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.name"] = json!("");
        namedrop::host::hc::events::Subscriber::default()
            .handle_line(r#"{"method":"missed","params":{}}"#, runtime)
            .unwrap();
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.name"],
            "alice"
        );
    });
}

#[test]
fn failed_store_commit_restores_the_applied_name() {
    check("commit-failure", |fixture, runtime| {
        let connection =
            rusqlite::Connection::open(runtime.state_directory.join("namedrop.sqlite3")).unwrap();
        connection.execute_batch("CREATE TABLE failure_parent (id INTEGER PRIMARY KEY);
            CREATE TABLE failure_child (id INTEGER REFERENCES failure_parent(id) DEFERRABLE INITIALLY DEFERRED);
            CREATE TRIGGER fail_rename_commit AFTER INSERT ON alias_claims WHEN NEW.alias='bob'
            BEGIN INSERT INTO failure_child VALUES(1); END;").unwrap();
        let error = namedrop::execute_lifecycle_operation(
            "name",
            json!({"alias":"bob", "pane_type":"terminal"}),
            runtime,
        )
        .unwrap_err();
        assert!(error.contains("FOREIGN KEY"), "{error}");
        assert!(
            fixture
                .calls
                .borrow()
                .iter()
                .any(|call| call.iter().any(|arg| arg == "namedrop.name=bob"))
        );
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.name"],
            "alice"
        );
        namedrop::host::hc::events::Subscriber::default()
            .handle_line(&labels(), runtime)
            .unwrap();
        let alias: String = connection
            .query_row(
                "SELECT alias FROM alias_claims WHERE claim_state='active'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(alias, "alice");
    });
}

#[test]
fn doctor_repair_uses_the_discovered_checkout_space() {
    check("doctor-space", |fixture, runtime| {
        fixture.terminals.borrow_mut()[0]["foreground"]["cwd"] = json!(env!("CARGO_MANIFEST_DIR"));
        let runtime = namedrop::LifecycleRuntime {
            workspace_id: None,
            current_pane_id: None,
            ..*runtime
        };
        let mut report = namedrop::diagnose_collaboration(&runtime);
        namedrop::append_hc_diagnostics(&mut report, &runtime, &runtime.host);
        let repair = &report
            .issues
            .iter()
            .find(|issue| issue.code == "ND-HC-SUBSCRIBER-001")
            .unwrap()
            .repair;
        let command = repair.command.as_ref().unwrap();
        assert!(
            command
                .windows(2)
                .any(|part| part == ["--hc-space", "fixture-space"]),
            "{command:?}"
        );
    });
}

#[test]
fn moved_terminal_mirrors_its_existing_destination_alias() {
    check("move-existing", |fixture, runtime| {
        fixture.terminals.borrow_mut()[0]["labels"]["herdr.space"] = json!("destination");
        let destination = namedrop::LifecycleRuntime {
            workspace_id: Some("destination"),
            ..*runtime
        };
        namedrop::execute_lifecycle_operation(
            "name",
            json!({"alias":"destination-name", "pane_type":"terminal"}),
            &destination,
        )
        .unwrap();
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.name"] = json!("alice");
        namedrop::host::hc::events::Subscriber::default()
            .handle_line(&labels(), runtime)
            .unwrap();
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.name"],
            "destination-name"
        );
    });
}

#[test]
fn naming_without_a_tab_works_for_parked_and_headless_terminals() {
    for space in ["fixture-space", "headless"] {
        check(&format!("no-tab-{space}"), |fixture, runtime| {
            fixture.terminals.borrow_mut()[0]["labels"]
                .as_object_mut()
                .unwrap()
                .remove("herdr.tab");
            if space == "headless" {
                fixture.terminals.borrow_mut()[0]["labels"]
                    .as_object_mut()
                    .unwrap()
                    .remove("herdr.space");
            }
            let runtime = namedrop::LifecycleRuntime {
                workspace_id: Some(space),
                ..*runtime
            };
            namedrop::execute_lifecycle_operation(
                "name",
                json!({"alias":"parked", "pane_type":"terminal"}),
                &runtime,
            )
            .unwrap();
            assert_eq!(
                fixture.terminals.borrow()[0]["labels"]["namedrop.name"],
                "parked"
            );
        });
    }
}

#[test]
fn move_label_failure_preserves_source_proof_until_fresh_subscriber_repairs_mirror() {
    for applied in [false, true] {
        for destination_alias in [None, Some("destination-name")] {
            for first in ["labels", "snapshot", "missed"] {
                for retry in ["labels", "snapshot", "missed"] {
                    check(
                        &format!(
                            "move-failure-{applied}-{}-{first}-{retry}",
                            destination_alias.unwrap_or("empty")
                        ),
                        |fixture, runtime| {
                            let mut healthy = fixture.terminals.borrow()[0].clone();
                            healthy["address"] = json!("t2");
                            healthy["labels"]["namedrop.name"] = json!("healthy");
                            fixture.terminals.borrow_mut().push(healthy);
                            fixture.terminals.borrow_mut()[0]["labels"]["herdr.space"] =
                                json!("destination");
                            let destination = namedrop::LifecycleRuntime {
                                workspace_id: Some("destination"),
                                ..*runtime
                            };
                            if let Some(alias) = destination_alias {
                                namedrop::execute_lifecycle_operation(
                                    "name",
                                    json!({"alias":alias, "pane_type":"terminal"}),
                                    &destination,
                                )
                                .unwrap();
                            } else {
                                namedrop::execute_lifecycle_operation(
                                    "activate",
                                    json!({"confirmed":true}),
                                    &destination,
                                )
                                .unwrap();
                            }
                            fixture.terminals.borrow_mut()[0]["labels"]["namedrop.name"] =
                                json!("alice");
                            let event = |kind| match kind {
                                "snapshot" => {
                                    json!({"terminals":*fixture.terminals.borrow()}).to_string()
                                }
                                "missed" => json!({"method":"missed","params":{}}).to_string(),
                                _ => labels(),
                            };
                            *fixture.name_write_failure.borrow_mut() = Some(applied);
                            let error = namedrop::host::hc::events::Subscriber::default()
                                .handle_line(&event(first), runtime)
                                .unwrap_err();
                            assert!(error.contains("transient name write failed"), "{error}");
                            if first != "labels" {
                                let names = namedrop::StateStore::open(runtime.state_directory)
                                    .unwrap()
                                    .transaction(|transaction| {
                                        transaction.list_pane_addresses(
                                            &namedrop::StateNamespace::for_space(
                                                "hc:fixture",
                                                "fixture-space",
                                            ),
                                        )
                                    })
                                    .unwrap();
                                assert!(
                                    names.iter().any(|address| address.alias == "healthy"
                                        && address.pane_id.as_deref() == Some("t2")),
                                    "failed move prevented healthy source naming"
                                );
                            }
                            namedrop::execute_lifecycle_operation(
                                "name",
                                json!({"alias":"healthy", "pane_type":"terminal"}),
                                &namedrop::LifecycleRuntime {
                                    current_pane_id: Some("t2"),
                                    ..*runtime
                                },
                            )
                            .unwrap();
                            namedrop::execute_lifecycle_operation("reconcile", json!({}), runtime)
                                .unwrap();
                            let mut store =
                                namedrop::StateStore::open(runtime.state_directory).unwrap();
                            let source =
                                namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
                            let target =
                                namedrop::StateNamespace::for_space("hc:fixture", "destination");
                            assert_eq!(
                                store
                                    .transaction(|tx| tx.active_pane_bindings(&source))
                                    .unwrap()
                                    .iter()
                                    .filter(|binding| binding.pane_id == "t1")
                                    .count(),
                                1,
                                "failed mirror write lost durable source binding: applied={applied}, first={first}, retry={retry}"
                            );
                            namedrop::host::hc::events::Subscriber::default()
                                .handle_line(&event(retry), runtime)
                                .unwrap();
                            assert!(
                                store
                                    .transaction(|tx| tx.active_pane_bindings(&source))
                                    .unwrap()
                                    .iter()
                                    .all(|binding| binding.pane_id != "t1")
                            );
                            let destination_addresses = store
                                .transaction(|tx| tx.list_pane_addresses(&target))
                                .unwrap();
                            assert!(
                                !destination_addresses
                                    .iter()
                                    .any(|address| address.alias == "alice")
                            );
                            assert_eq!(
                                destination_addresses
                                    .iter()
                                    .find(|address| address.pane_id.as_deref() == Some("t1"))
                                    .map(|address| address.alias.as_str()),
                                destination_alias
                            );
                            assert_eq!(
                                fixture.terminals.borrow()[0]["labels"]["namedrop.name"],
                                destination_alias.unwrap_or("")
                            );
                        },
                    );
                }
            }
        }
    }
}

fn session_event(kind: &str, fixture: &Fixture) -> String {
    match kind {
        "snapshot" => json!({"terminals": *fixture.terminals.borrow()}).to_string(),
        "missed" => json!({"method":"missed","params":{}}).to_string(),
        _ => json!({"method":"event","params":{"type":kind,"terminal":"t1"}}).to_string(),
    }
}

fn queued_agent(
    fixture: &Fixture,
    runtime: &namedrop::LifecycleRuntime<
        '_,
        HcHost<&Fixture>,
        namedrop::SystemClock,
        namedrop::ProcessIds,
    >,
    pid: u32,
) {
    use namedrop::*;
    {
        let mut terminals = fixture.terminals.borrow_mut();
        let terminal = &mut terminals[0];
        terminal["labels"]["namedrop.session"] = json!("original-session");
        terminal["labels"]["namedrop.session.pid"] = json!(pid.to_string());
        terminal["labels"]["namedrop.session.start"] = json!(process::start(pid));
        terminal["foreground"] = json!({"pid":pid,"name":"codex","argv":["codex"]});
        terminal["status"] = json!({"state":"working"});
    }
    execute_lifecycle_operation(
        "name",
        json!({"alias":"alice","pane_type":"agent"}),
        runtime,
    )
    .unwrap();
    let namespace = StateNamespace::for_space("hc:fixture", "fixture-space");
    let attribution =
        MutationAttribution::new(ActorKind::Human, "test", runtime.clock.now_unix_ms());
    StateStore::open(runtime.state_directory)
        .unwrap()
        .transaction(|tx| {
            let binding = tx.active_pane_bindings(&namespace)?.remove(0);
            let thread = ThreadId::new("death-thread");
            tx.create_thread(&namespace, &thread, "test", &attribution)?;
            tx.append_message(
                &namespace,
                &MessageDraft {
                    thread_id: thread,
                    message_id: MessageId::new("death-message"),
                    in_reply_to: None,
                    author_address_id: binding.address_id.clone(),
                    author_kind: ActorKind::Human,
                    author_alias_snapshot: "alice".into(),
                    author_agent_session_snapshot: None,
                    request_id: "death-request".into(),
                    authored_source: "@alice test".into(),
                    body: "test".into(),
                    grammar_version: "mention/v1".into(),
                    recipients: vec![MessageRecipientDraft {
                        address_id: binding.address_id,
                        alias_snapshot: "alice".into(),
                        agent_session_snapshot: Some(AgentSessionId::new("original-session")),
                        delivery_id: DeliveryId::new("death-delivery"),
                    }],
                    context_references: vec![],
                },
                &attribution,
            )?;
            Ok(())
        })
        .unwrap();
    fixture.calls.borrow_mut().clear();
}

#[test]
fn dead_queued_session_clears_labels_and_keeps_base_empty_mailbox_behavior() {
    for kind in [
        "status",
        "turn",
        "labels",
        "snapshot",
        "missed",
        "lost",
        "restarted",
    ] {
        check(&format!("dead-queued-{kind}"), |fixture, runtime| {
            let agent = process::AgentProcess::new();
            queued_agent(fixture, runtime, agent.pid());
            drop(agent);
            namedrop::host::hc::events::Subscriber::default()
                .handle_line(&session_event(kind, fixture), runtime)
                .unwrap();
            for key in [
                "namedrop.session",
                "namedrop.session.pid",
                "namedrop.session.start",
            ] {
                assert_eq!(
                    fixture.terminals.borrow()[0]["labels"][key],
                    "",
                    "stale {key} after {kind}"
                );
            }
            let db = rusqlite::Connection::open(runtime.state_directory.join("namedrop.sqlite3"))
                .unwrap();
            assert_eq!(
                db.query_row(
                    "SELECT state FROM mailbox_deliveries WHERE delivery_id='death-delivery'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
                "queued"
            );
            assert_eq!(
                db.query_row("SELECT COUNT(*) FROM delivery_attempts", [], |row| row
                    .get::<_, i64>(0))
                    .unwrap(),
                0
            );
            let namespace = namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
            let addresses = namedrop::StateStore::open(runtime.state_directory)
                .unwrap()
                .transaction(|tx| tx.list_pane_addresses(&namespace))
                .unwrap();
            let address = addresses
                .iter()
                .find(|address| address.alias == "alice")
                .unwrap();
            assert_eq!(
                address.lifecycle,
                namedrop::PaneAddressLifecycle::Empty,
                "{kind}"
            );
            let outlook = namedrop::recipient_outlooks(&["alice".into()], &addresses);
            assert!(
                outlook[0]
                    .statement
                    .contains("this message queues normally with nobody there to receive it")
            );
            let calls = fixture.calls.borrow();
            assert!(calls.iter().all(|call| call[0] != "send"));
            let clears: Vec<_> = calls
                .iter()
                .filter(|call| call.iter().any(|arg| arg == "namedrop.session="))
                .collect();
            assert_eq!(clears.len(), 1);
            assert!(clears[0].contains(&"namedrop.session.pid=".to_owned()));
            assert!(clears[0].contains(&"namedrop.session.start=".to_owned()));
        });
    }
}

#[test]
fn stale_session_cleanup_failure_stops_reconciliation_and_sends() {
    for kind in [
        "status",
        "turn",
        "labels",
        "snapshot",
        "missed",
        "lost",
        "restarted",
    ] {
        check(&format!("cleanup-failure-{kind}"), |fixture, runtime| {
            let agent = process::AgentProcess::new();
            queued_agent(fixture, runtime, agent.pid());
            drop(agent);
            *fixture.session_clear_failure.borrow_mut() = true;
            let error = namedrop::host::hc::events::Subscriber::default()
                .handle_line(&session_event(kind, fixture), runtime)
                .unwrap_err();
            assert!(error.contains("session cleanup failed"), "{error}");
            assert_eq!(
                fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
                "original-session"
            );
            let namespace = namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
            let addresses = namedrop::StateStore::open(runtime.state_directory)
                .unwrap()
                .transaction(|tx| tx.list_pane_addresses(&namespace))
                .unwrap();
            assert_eq!(
                addresses[0].agent_session_id.as_ref().map(|id| id.as_str()),
                Some("original-session")
            );
            assert!(fixture.calls.borrow().iter().all(|call| call[0] != "send"));
        });
    }
}

fn cached_session_after_other_pane_event(cleanup_failure: bool) {
    for kind in ["created", "labels"] {
        for cross_space in [false, true] {
            check(
                &format!("cached-session-{kind}-{cross_space}-{cleanup_failure}"),
                |fixture, runtime| {
                    let agent = process::AgentProcess::new();
                    queued_agent(fixture, runtime, agent.pid());
                    let mut subscriber = namedrop::host::hc::events::Subscriber::default();
                    subscriber
                        .handle_line(&session_event("snapshot", fixture), runtime)
                        .unwrap();
                    let mut second = fixture.terminals.borrow()[0].clone();
                    second["address"] = json!("t2");
                    second["labels"] = json!({"herdr.space": if cross_space { "other-space" } else { "fixture-space" }});
                    second["foreground"] =
                        json!({"pid":fixture._shell.pid(),"name":"bash","argv":["bash"]});
                    second["status"] = json!({"state":"idle"});
                    fixture.terminals.borrow_mut().push(second);
                    if cross_space {
                        let namespace =
                            namedrop::StateNamespace::for_space("hc:fixture", "other-space");
                        let attribution = namedrop::MutationAttribution::new(
                            namedrop::ActorKind::Human,
                            "test",
                            1000,
                        );
                        namedrop::StateStore::open(runtime.state_directory)
                            .unwrap()
                            .transaction(|tx| tx.activate_worktree(&namespace, &attribution))
                            .unwrap();
                    }
                    *fixture.session_clear_state.borrow_mut() =
                        Some(runtime.state_directory.to_owned());
                    fixture.calls.borrow_mut().clear();
                    drop(agent);
                    *fixture.session_clear_failure.borrow_mut() = cleanup_failure;
                    let event = json!({"method":"event","params":{"type":kind,"terminal":"t2"}})
                        .to_string();
                    let result = subscriber.handle_line(&event, runtime);
                    if cleanup_failure {
                        assert!(result.unwrap_err().contains("session cleanup failed"));
                        let namespace =
                            namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
                        let addresses = namedrop::StateStore::open(runtime.state_directory)
                            .unwrap()
                            .transaction(|tx| tx.list_pane_addresses(&namespace))
                            .unwrap();
                        let address = addresses
                            .iter()
                            .find(|address| address.alias == "alice")
                            .unwrap();
                        assert_eq!(
                            address.agent_session_id.as_ref().map(|id| id.as_str()),
                            Some("original-session")
                        );
                        assert_eq!(
                            fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
                            "original-session"
                        );
                        assert!(fixture.calls.borrow().iter().all(|call| call[0] != "send"));
                        *fixture.session_clear_failure.borrow_mut() = false;
                        subscriber.handle_line(&event, runtime).unwrap();
                    } else {
                        result.unwrap();
                    }
                    for key in [
                        "namedrop.session",
                        "namedrop.session.pid",
                        "namedrop.session.start",
                    ] {
                        assert_eq!(
                            fixture.terminals.borrow()[0]["labels"][key],
                            "",
                            "stale cached {key} after {kind} on t2"
                        );
                    }
                    let namespace =
                        namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
                    let addresses = namedrop::StateStore::open(runtime.state_directory)
                        .unwrap()
                        .transaction(|tx| tx.list_pane_addresses(&namespace))
                        .unwrap();
                    let address = addresses
                        .iter()
                        .find(|address| address.alias == "alice")
                        .unwrap();
                    assert_eq!(address.lifecycle, namedrop::PaneAddressLifecycle::Empty);
                    assert!(
                        namedrop::recipient_outlooks(&["alice".into()], &addresses)[0]
                            .statement
                            .contains(
                                "this message queues normally with nobody there to receive it"
                            )
                    );
                    let db = rusqlite::Connection::open(
                        runtime.state_directory.join("namedrop.sqlite3"),
                    )
                    .unwrap();
                    assert_eq!(db.query_row("SELECT state FROM mailbox_deliveries WHERE delivery_id='death-delivery'", [], |row| row.get::<_, String>(0)).unwrap(), "queued");
                    assert_eq!(
                        db.query_row("SELECT COUNT(*) FROM delivery_attempts", [], |row| row
                            .get::<_, i64>(0))
                            .unwrap(),
                        0
                    );
                    let calls = fixture.calls.borrow();
                    assert!(
                        calls
                            .iter()
                            .all(|call| call[0] != "send" && call[0] != "ls")
                    );
                    if !cleanup_failure {
                        assert_eq!(
                            calls.len(),
                            3,
                            "only t2 refresh and t1 cleanup are needed: {calls:?}"
                        );
                    }
                },
            );
        }
    }
}

#[test]
fn cached_dead_session_clears_on_other_pane_event() {
    cached_session_after_other_pane_event(false);
}

#[test]
fn cached_dead_session_cleanup_failure_preserves_binding() {
    cached_session_after_other_pane_event(true);
}

#[test]
fn reconciliation_validates_pid_start_and_preserves_fresh_session() {
    for valid_start in [true, false] {
        check(&format!("pid-start-{valid_start}"), |fixture, runtime| {
            let agent = process::AgentProcess::new();
            queued_agent(fixture, runtime, agent.pid());
            if !valid_start {
                fixture.terminals.borrow_mut()[0]["labels"]["namedrop.session.start"] =
                    json!("recycled-pid");
            }
            namedrop::host::hc::events::Subscriber::default()
                .handle_line(&session_event("status", fixture), runtime)
                .unwrap();
            assert_eq!(
                fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
                if valid_start { "original-session" } else { "" }
            );
            assert!(fixture.calls.borrow().iter().all(|call| call[0] != "send"));
        });
    }
}

#[test]
fn stale_observation_refresh_preserves_replacement_session() {
    check("replacement-refresh", |fixture, runtime| {
        let old = process::AgentProcess::new();
        queued_agent(fixture, runtime, old.pid());
        let fresh = process::AgentProcess::new();
        let mut replacement = fixture.terminals.borrow()[0].clone();
        replacement["labels"]["namedrop.session"] = json!("replacement-session");
        replacement["labels"]["namedrop.session.pid"] = json!(fresh.pid().to_string());
        replacement["labels"]["namedrop.session.start"] = json!(process::start(fresh.pid()));
        replacement["foreground"]["pid"] = json!(fresh.pid());
        drop(old);
        *fixture.replacement_on_get.borrow_mut() = Some((2, replacement));
        namedrop::host::hc::events::Subscriber::default()
            .handle_line(&session_event("status", fixture), runtime)
            .unwrap();
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
            "replacement-session"
        );
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.session.pid"],
            fresh.pid().to_string()
        );
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.session.start"],
            process::start(fresh.pid())
        );
        assert!(fixture.calls.borrow().iter().all(|call| call[0] != "send"
            && !call.iter().any(|arg| arg == "namedrop.session=")));
    });
}

#[test]
fn stale_session_refresh_preserves_labels_after_move_to_inactive_space() {
    for kind in [
        "status",
        "turn",
        "labels",
        "snapshot",
        "missed",
        "lost",
        "restarted",
    ] {
        for deactivated in [false, true] {
            check(
                &format!("session-move-inactive-{deactivated}-{kind}"),
                |fixture, runtime| {
                    let mut subscriber = namedrop::host::hc::events::Subscriber::default();
                    subscriber
                        .handle_line(&session_event("snapshot", fixture), runtime)
                        .unwrap();
                    let old = process::AgentProcess::new();
                    queued_agent(fixture, runtime, old.pid());
                    let destination = "inactive-destination";
                    let namespace = namedrop::StateNamespace::for_space("hc:fixture", destination);
                    if deactivated {
                        let attribution = namedrop::MutationAttribution::new(
                            namedrop::ActorKind::Human,
                            "test",
                            1000,
                        );
                        namedrop::StateStore::open(runtime.state_directory)
                            .unwrap()
                            .transaction(|tx| {
                                tx.activate_worktree(&namespace, &attribution)?;
                                tx.deactivate_worktree(&namespace, &attribution)
                            })
                            .unwrap();
                    }
                    let mut moved = fixture.terminals.borrow()[0].clone();
                    moved["labels"]["herdr.space"] = json!(destination);
                    let session_labels: Vec<_> = [
                        "namedrop.session",
                        "namedrop.session.pid",
                        "namedrop.session.start",
                    ]
                    .into_iter()
                    .map(|key| (key, moved["labels"][key].clone()))
                    .collect();
                    drop(old);
                    *fixture.replacement_on_get.borrow_mut() =
                        Some((if kind == "snapshot" { 1 } else { 2 }, moved));
                    subscriber
                        .handle_line(&session_event(kind, fixture), runtime)
                        .unwrap();
                    for (key, expected) in session_labels {
                        assert_eq!(
                            fixture.terminals.borrow()[0]["labels"][key],
                            expected,
                            "cleanup changed {key} after moving into an inactive space"
                        );
                    }
                    assert!(fixture.calls.borrow().iter().all(|call| call[0] != "send"
                        && !call.iter().any(|arg| arg == "namedrop.session=")));
                },
            );
        }
    }
}

#[test]
fn dead_queued_tui_session_has_the_same_empty_mailbox_outcome() {
    #[derive(Clone, Copy)]
    struct DeadTui;
    impl HerdrCommandExecutor for DeadTui {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["worktree", "list", "--workspace", "fixture-space", "--json"] => {
                    json!({"result":{"worktrees":[{"path":env!("CARGO_MANIFEST_DIR"),"open_workspace_id":"fixture-space"}]}}).to_string()
                }
                ["pane", "list", "--workspace", "fixture-space"] => {
                    json!({"result":{"panes":[{"pane_id":"t1","terminal_id":"t1","tab_id":"tab-1","workspace_id":"fixture-space"}]}}).to_string()
                }
                ["pane", "report-metadata", ..] => "{}".into(),
                other => panic!("unexpected TUI command after agent exit {other:?}"),
            };
            Ok(HerdrCommandOutput { stdout })
        }
    }
    check("tui-dead-queued", |fixture, runtime| {
        let agent = process::AgentProcess::new();
        queued_agent(fixture, runtime, agent.pid());
        drop(agent);
        let tui = namedrop::LifecycleRuntime {
            host: namedrop::TuiHost::new(DeadTui),
            state_directory: runtime.state_directory,
            home_directory: runtime.home_directory,
            agent_config: runtime.agent_config,
            cwd: runtime.cwd,
            workspace_id: runtime.workspace_id,
            current_pane_id: runtime.current_pane_id,
            session_socket_id: runtime.session_socket_id,
            clock: runtime.clock,
            ids: runtime.ids,
        };
        namedrop::execute_lifecycle_operation(
            "event.agent-status-changed",
            json!({"type":"pane_agent_status_changed","pane_id":"t1","workspace_id":"fixture-space"}),
            &tui,
        ).unwrap();
        let db =
            rusqlite::Connection::open(runtime.state_directory.join("namedrop.sqlite3")).unwrap();
        assert_eq!(
            db.query_row(
                "SELECT state FROM mailbox_deliveries WHERE delivery_id='death-delivery'",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "queued"
        );
        assert_eq!(
            db.query_row("SELECT COUNT(*) FROM delivery_attempts", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let namespace = namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
        let addresses = namedrop::StateStore::open(runtime.state_directory)
            .unwrap()
            .transaction(|tx| tx.list_pane_addresses(&namespace))
            .unwrap();
        assert_eq!(
            addresses[0].lifecycle,
            namedrop::PaneAddressLifecycle::Empty
        );
        assert!(
            namedrop::recipient_outlooks(&["alice".into()], &addresses)[0]
                .statement
                .contains("this message queues normally with nobody there to receive it")
        );
    });
}

#[test]
fn inactive_space_session_labels_are_not_reconciled() {
    for kind in [
        "status",
        "turn",
        "labels",
        "snapshot",
        "missed",
        "lost",
        "restarted",
    ] {
        check(&format!("inactive-session-{kind}"), |fixture, runtime| {
            let agent = process::AgentProcess::new();
            queued_agent(fixture, runtime, agent.pid());
            drop(agent);
            fixture.terminals.borrow_mut()[0]["state"] = json!("lost");
            let namespace = namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
            namedrop::StateStore::open(runtime.state_directory)
                .unwrap()
                .transaction(|tx| {
                    tx.deactivate_worktree(
                        &namespace,
                        &namedrop::MutationAttribution::new(
                            namedrop::ActorKind::Human,
                            "test",
                            1000,
                        ),
                    )
                })
                .unwrap();
            namedrop::host::hc::events::Subscriber::default()
                .handle_line(&session_event(kind, fixture), runtime)
                .unwrap();
            assert_eq!(
                fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
                "original-session"
            );
            assert!(
                fixture
                    .calls
                    .borrow()
                    .iter()
                    .all(|call| !matches!(call[0].as_str(), "label" | "send"))
            );
        });
    }
}

#[test]
fn subscriber_snapshot_and_created_events_do_not_rescan_other_terminals() {
    check("bounded-created-discovery", |fixture, runtime| {
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.session"] = json!("");
        let mut subscriber = namedrop::host::hc::events::Subscriber::default();
        fixture.calls.borrow_mut().clear();
        subscriber
            .handle_line(
                &json!({"terminals":*fixture.terminals.borrow()}).to_string(),
                runtime,
            )
            .unwrap();
        assert!(
            fixture
                .calls
                .borrow()
                .iter()
                .all(|call| call[0] != "ls" && call[0] != "get"),
            "snapshot discarded its terminal records: {:?}",
            fixture.calls.borrow()
        );
        for index in 2..=11 {
            let pane = format!("t{index}");
            let mut terminal = fixture.terminals.borrow()[0].clone();
            terminal["address"] = json!(pane);
            terminal["labels"]["namedrop.name"] = json!("");
            fixture.terminals.borrow_mut().push(terminal);
            fixture.calls.borrow_mut().clear();
            subscriber
                .handle_line(
                    &json!({"method":"event","params":{"type":"created","terminal":pane}})
                        .to_string(),
                    runtime,
                )
                .unwrap();
            let calls = fixture.calls.borrow();
            assert!(
                calls.len() <= 3,
                "created {pane} made {} hc calls: {calls:?}",
                calls.len()
            );
            assert!(
                calls
                    .iter()
                    .all(|call| call[0] != "ls" && (call[0] != "get" || call[1] == pane)),
                "created {pane} touched another terminal: {calls:?}"
            );
        }
    });
}

#[test]
fn subscriber_coalesces_queued_events_and_missed_notifications() {
    check("coalesced-discovery", |fixture, runtime| {
        let mut subscriber = namedrop::host::hc::events::Subscriber::default();
        subscriber
            .handle_line(
                &json!({"terminals":*fixture.terminals.borrow()}).to_string(),
                runtime,
            )
            .unwrap();
        let epoch = || {
            rusqlite::Connection::open(runtime.state_directory.join("namedrop.sqlite3"))
                .unwrap()
                .query_row("SELECT epoch FROM reconciliation_epochs", [], |row| {
                    row.get::<_, i64>(0)
                })
                .unwrap()
        };
        let mut lines = Vec::new();
        for index in 2..=11 {
            let pane = format!("t{index}");
            let mut terminal = fixture.terminals.borrow()[0].clone();
            terminal["address"] = json!(pane);
            terminal["labels"]["namedrop.name"] = json!("");
            fixture.terminals.borrow_mut().push(terminal);
            for kind in ["created", "labels", "status", "turn", "labels"] {
                lines.push(
                    json!({"method":"event","params":{"type":kind,"terminal":pane}}).to_string(),
                );
            }
        }
        fixture.calls.borrow_mut().clear();
        let before = epoch();
        subscriber
            .handle_lines(lines.iter().map(String::as_str), runtime)
            .unwrap();
        assert_eq!(
            epoch(),
            before + 1,
            "queued events must share one reconciliation"
        );
        assert_eq!(
            fixture.calls.borrow().len(),
            10,
            "one fresh get per changed terminal: {:?}",
            fixture.calls.borrow()
        );
        assert!(
            fixture
                .calls
                .borrow()
                .iter()
                .all(|call| call[0] == "get" && call[1] != "t1")
        );
        fixture.calls.borrow_mut().clear();
        let missed = r#"{"method":"missed","params":{}}"#;
        let before = epoch();
        subscriber
            .handle_lines([missed, missed, missed], runtime)
            .unwrap();
        assert_eq!(epoch(), before + 1);
        assert_eq!(
            fixture
                .calls
                .borrow()
                .iter()
                .filter(|call| call[0] == "ls")
                .count(),
            1
        );
        assert_eq!(
            fixture
                .calls
                .borrow()
                .iter()
                .filter(|call| call[0] == "get")
                .count(),
            11
        );
    });
}

#[test]
fn queued_snapshot_does_not_resurrect_released_alias() {
    check("snapshot-after-release", |fixture, runtime| {
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.session"] = json!("");
        let snapshot = json!({"terminals":*fixture.terminals.borrow()}).to_string();
        let mut subscriber = namedrop::host::hc::events::Subscriber::default();
        subscriber.handle_line(&snapshot, runtime).unwrap();
        let namespace = namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
        let addresses = || {
            namedrop::StateStore::open(runtime.state_directory)
                .unwrap()
                .transaction(|tx| tx.list_pane_addresses(&namespace))
                .unwrap()
        };
        namedrop::execute_lifecycle_operation(
            "address.retire",
            json!({"address_id":addresses()[0].id}),
            runtime,
        )
        .unwrap();
        namedrop::execute_lifecycle_operation(
            "address.release-alias",
            json!({"alias":"alice"}),
            runtime,
        )
        .unwrap();
        fixture.calls.borrow_mut().clear();
        subscriber.handle_line(&snapshot, runtime).unwrap();
        assert!(
            addresses().iter().all(|address| address.retired),
            "stale snapshot resurrected a released alias"
        );
        assert_eq!(fixture.terminals.borrow()[0]["labels"]["namedrop.name"], "");
    });
}

#[test]
fn queued_snapshot_does_not_revert_committed_rename() {
    check("snapshot-after-rename", |fixture, runtime| {
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.session"] = json!("");
        let snapshot = json!({"terminals":*fixture.terminals.borrow()}).to_string();
        let mut subscriber = namedrop::host::hc::events::Subscriber::default();
        subscriber.handle_line(&snapshot, runtime).unwrap();
        namedrop::execute_lifecycle_operation(
            "name",
            json!({"alias":"bob", "pane_type":"terminal"}),
            runtime,
        )
        .unwrap();
        fixture.calls.borrow_mut().clear();
        subscriber.handle_line(&snapshot, runtime).unwrap();
        let namespace = namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
        let addresses = namedrop::StateStore::open(runtime.state_directory)
            .unwrap()
            .transaction(|tx| tx.list_pane_addresses(&namespace))
            .unwrap();
        assert_eq!(
            addresses[0].alias, "bob",
            "stale snapshot reverted a committed rename"
        );
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.name"],
            "bob"
        );
    });
}

#[test]
fn coalesced_session_loss_and_removal_close_the_pane_without_retrying() {
    for kind in ["lost", "restarted"] {
        check(&format!("coalesced-{kind}-removed"), |fixture, runtime| {
            fixture.terminals.borrow_mut()[0]["labels"]["namedrop.session"] = json!("");
            let mut subscriber = namedrop::host::hc::events::Subscriber::default();
            subscriber
                .handle_line(
                    &json!({"terminals":*fixture.terminals.borrow()}).to_string(),
                    runtime,
                )
                .unwrap();
            fixture.terminals.borrow_mut().clear();
            fixture.calls.borrow_mut().clear();
            let events = [kind, "removed"].map(|kind| {
                json!({"method":"event", "params":{"type":kind,"terminal":"t1"}}).to_string()
            });
            subscriber
                .handle_lines(events.iter().map(String::as_str), runtime)
                .unwrap();
            let namespace = namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
            let bindings = namedrop::StateStore::open(runtime.state_directory)
                .unwrap()
                .transaction(|tx| tx.active_pane_bindings(&namespace))
                .unwrap();
            assert!(bindings.is_empty(), "removed pane kept an active binding");
            assert_eq!(
                fixture
                    .calls
                    .borrow()
                    .iter()
                    .filter(|call| call[0] == "get")
                    .count(),
                1
            );
            fixture.calls.borrow_mut().clear();
            subscriber
                .handle_line(r#"{"method":"heartbeat","params":{}}"#, runtime)
                .unwrap();
            assert!(
                fixture.calls.borrow().is_empty(),
                "removed pane remained pending for retry"
            );
        });
    }
}

#[derive(Clone, Copy)]
struct RetryClock(u64);
impl namedrop::Clock for RetryClock {
    fn now_unix_ms(&self) -> u64 {
        self.0
    }
}

#[test]
fn removed_binding_survives_failed_close_until_tick_retry() {
    for initialized in [false, true] {
        check(
            &format!("removed-retry-{initialized}"),
            |fixture, runtime| {
                fixture.terminals.borrow_mut()[0]["labels"]["namedrop.session"] = json!("");
                let clock = RetryClock(10_000);
                let runtime = namedrop::LifecycleRuntime {
                    clock: &clock,
                    state_directory: runtime.state_directory,
                    home_directory: runtime.home_directory,
                    agent_config: runtime.agent_config,
                    cwd: runtime.cwd,
                    workspace_id: runtime.workspace_id,
                    current_pane_id: runtime.current_pane_id,
                    session_socket_id: runtime.session_socket_id,
                    host: runtime.host,
                    ids: runtime.ids,
                };
                let mut subscriber = namedrop::host::hc::events::Subscriber::default();
                subscriber
                    .handle_line(&session_event("snapshot", fixture), &runtime)
                    .unwrap();
                let pane = if initialized { "t1" } else { "t2" };
                if !initialized {
                    let mut second = fixture.terminals.borrow()[0].clone();
                    second["address"] = json!(pane);
                    second["labels"]["namedrop.name"] = json!("");
                    fixture.terminals.borrow_mut().push(second);
                    let second_runtime = namedrop::LifecycleRuntime {
                        current_pane_id: Some(pane),
                        ..runtime
                    };
                    namedrop::execute_lifecycle_operation(
                        "name",
                        json!({"alias":"bob", "pane_type":"terminal"}),
                        &second_runtime,
                    )
                    .unwrap();
                }
                let connection =
                    rusqlite::Connection::open(runtime.state_directory.join("namedrop.sqlite3"))
                        .unwrap();
                connection.execute_batch("CREATE TRIGGER fail_unbind BEFORE UPDATE OF active ON pane_bindings WHEN NEW.active=0 BEGIN SELECT RAISE(ABORT, 'injected unbind failure'); END;").unwrap();
                fixture
                    .terminals
                    .borrow_mut()
                    .retain(|terminal| terminal["address"] != pane);
                let events = ["created", "lost", "removed"].map(|kind| {
                    json!({"method":"event","params":{"type":kind,"terminal":pane}}).to_string()
                });
                let result = subscriber.handle_lines(events.iter().map(String::as_str), &runtime);
                assert!(
                    result.is_err(),
                    "removal must attempt closing the committed binding: {result:?}"
                );
                connection
                    .execute_batch("DROP TRIGGER fail_unbind")
                    .unwrap();
                let retry = namedrop::LifecycleRuntime {
                    clock: &RetryClock(10_500),
                    ..runtime
                };
                subscriber.tick(&retry).unwrap();
                let active: i64 = connection
                    .query_row(
                        "SELECT count(*) FROM pane_bindings WHERE active=1 AND pane_id=?1",
                        [pane],
                        |row| row.get(0),
                    )
                    .unwrap();
                assert_eq!(active, 0, "retry lost the removed pane's binding");
            },
        );
    }
}

#[test]
fn pending_event_survives_transient_store_read_failure() {
    check("pending-store-failure", |fixture, runtime| {
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.session"] = json!("");
        let runtime = namedrop::LifecycleRuntime {
            clock: &RetryClock(10_000),
            state_directory: runtime.state_directory,
            home_directory: runtime.home_directory,
            agent_config: runtime.agent_config,
            cwd: runtime.cwd,
            workspace_id: runtime.workspace_id,
            current_pane_id: runtime.current_pane_id,
            session_socket_id: runtime.session_socket_id,
            host: runtime.host,
            ids: runtime.ids,
        };
        let mut subscriber = namedrop::host::hc::events::Subscriber::default();
        subscriber
            .handle_line(&session_event("snapshot", fixture), &runtime)
            .unwrap();
        let connection =
            rusqlite::Connection::open(runtime.state_directory.join("namedrop.sqlite3")).unwrap();
        connection
            .execute_batch("ALTER TABLE worktrees RENAME TO unavailable_worktrees")
            .unwrap();
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.name"] = json!("bob");
        assert!(subscriber.handle_line(&labels(), &runtime).is_err());
        fixture.calls.borrow_mut().clear();
        subscriber.tick(&runtime).unwrap();
        assert!(
            fixture.calls.borrow().is_empty(),
            "retry must wait for its deadline"
        );
        let retry = namedrop::LifecycleRuntime {
            clock: &RetryClock(10_500),
            ..runtime
        };
        assert!(
            subscriber.tick(&retry).is_err(),
            "store outage must remain scheduled"
        );
        connection
            .execute_batch("ALTER TABLE unavailable_worktrees RENAME TO worktrees")
            .unwrap();
        let retry = namedrop::LifecycleRuntime {
            clock: &RetryClock(11_000),
            ..runtime
        };
        subscriber.tick(&retry).unwrap();
        let alias: String = connection
            .query_row(
                "SELECT alias FROM alias_claims WHERE claim_state='active'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(alias, "bob", "failed batch lost its pending label event");
    });
}

#[test]
fn queued_snapshot_preserves_newer_bound_terminal() {
    for unavailable in [false, true] {
        check(
            &format!("snapshot-new-binding-{unavailable}"),
            |fixture, runtime| {
                fixture.terminals.borrow_mut()[0]["labels"]["namedrop.session"] = json!("");
                let snapshot = session_event("snapshot", fixture);
                let mut subscriber = namedrop::host::hc::events::Subscriber::default();
                subscriber.handle_line(&snapshot, runtime).unwrap();
                let mut second = fixture.terminals.borrow()[0].clone();
                second["address"] = json!("t2");
                second["labels"]["namedrop.name"] = json!("");
                fixture.terminals.borrow_mut().push(second);
                let second_runtime = namedrop::LifecycleRuntime {
                    current_pane_id: Some("t2"),
                    ..*runtime
                };
                namedrop::execute_lifecycle_operation(
                    "name",
                    json!({"alias":"bob", "pane_type":"terminal"}),
                    &second_runtime,
                )
                .unwrap();
                if unavailable {
                    *fixture.get_failure.borrow_mut() = Some("t2".into());
                }
                let result = subscriber.handle_line(&snapshot, runtime);
                result.unwrap();
                let namespace = namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
                let bindings = namedrop::StateStore::open(runtime.state_directory)
                    .unwrap()
                    .transaction(|tx| tx.active_pane_bindings(&namespace))
                    .unwrap();
                assert!(
                    bindings.iter().any(|binding| binding.pane_id == "t2"),
                    "old snapshot removed a newer binding"
                );
                *fixture.get_failure.borrow_mut() = None;
                subscriber
                    .handle_line(
                        r#"{"method":"event","params":{"type":"created","terminal":"t2"}}"#,
                        runtime,
                    )
                    .unwrap();
                assert_eq!(
                    fixture.terminals.borrow()[1]["labels"]["namedrop.name"],
                    "bob"
                );
            },
        );
    }
}

#[test]
fn queued_snapshot_does_not_reverse_committed_move() {
    check("snapshot-committed-move", |fixture, runtime| {
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.session"] = json!("");
        let snapshot = session_event("snapshot", fixture);
        let mut subscriber = namedrop::host::hc::events::Subscriber::default();
        subscriber.handle_line(&snapshot, runtime).unwrap();
        fixture.terminals.borrow_mut()[0]["labels"]["herdr.space"] = json!("destination");
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.name"] = json!("");
        let destination = namedrop::LifecycleRuntime {
            workspace_id: Some("destination"),
            ..*runtime
        };
        namedrop::execute_lifecycle_operation(
            "name",
            json!({"alias":"bob", "pane_type":"terminal"}),
            &destination,
        )
        .unwrap();
        subscriber.handle_line(&snapshot, runtime).unwrap();
        let namespace = namedrop::StateNamespace::for_space("hc:fixture", "destination");
        let bindings = namedrop::StateStore::open(runtime.state_directory)
            .unwrap()
            .transaction(|tx| tx.active_pane_bindings(&namespace))
            .unwrap();
        assert!(
            bindings.iter().any(|binding| binding.pane_id == "t1"),
            "old snapshot reversed a committed move"
        );
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.name"],
            "bob"
        );
    });
}

#[test]
fn unavailable_terminal_clears_session_with_still_running_owner() {
    for state in ["lost", "finished"] {
        for kind in ["lost", "restarted"] {
            check(
                &format!("unavailable-owner-{state}-{kind}"),
                |fixture, runtime| {
                    let mut subscriber = namedrop::host::hc::events::Subscriber::default();
                    subscriber
                        .handle_line(&session_event("snapshot", fixture), runtime)
                        .unwrap();
                    let agent = process::AgentProcess::new();
                    queued_agent(fixture, runtime, agent.pid());
                    fixture.terminals.borrow_mut()[0]["state"] = json!(state);
                    subscriber
                        .handle_line(&session_event(kind, fixture), runtime)
                        .unwrap();
                    for key in [
                        "namedrop.session",
                        "namedrop.session.pid",
                        "namedrop.session.start",
                    ] {
                        assert_eq!(
                            fixture.terminals.borrow()[0]["labels"][key],
                            "",
                            "{state}/{kind} kept {key}"
                        );
                    }
                    assert!(fixture.calls.borrow().iter().all(|call| call[0] != "send"));
                },
            );
        }
    }
}

#[test]
fn remote_events_do_not_schedule_heartbeat_retries() {
    check("remote-heartbeats", |fixture, runtime| {
        let mut remote = fixture.terminals.borrow()[0].clone();
        remote["address"] = json!("remote/t9");
        remote["labels"]["namedrop.name"] = json!("");
        fixture.terminals.borrow_mut().push(remote);
        let mut subscriber = namedrop::host::hc::events::Subscriber::default();
        let snapshot_result = subscriber.handle_line(
            &json!({"terminals":*fixture.terminals.borrow()}).to_string(),
            runtime,
        );
        fixture.calls.borrow_mut().clear();
        for kind in ["created", "status", "removed"] {
            let _ = subscriber.handle_line(
                &json!({"method":"event","params":{"type":kind,"terminal":"remote/t9"}})
                    .to_string(),
                runtime,
            );
        }
        for _ in 0..10 {
            let _ = subscriber.handle_line(r#"{"method":"heartbeat","params":{}}"#, runtime);
        }
        assert!(
            fixture.calls.borrow().is_empty(),
            "remote events scheduled hc work: {:?}",
            fixture.calls.borrow()
        );
        snapshot_result.unwrap();
    });
}

#[test]
fn failed_get_is_idle_on_heartbeats_and_preserves_only_unknown_recipients() {
    check("unavailable-heartbeats", |fixture, runtime| {
        let mut other = fixture.terminals.borrow()[0].clone();
        other["address"] = json!("t2");
        other["labels"]["namedrop.name"] = json!("bob");
        fixture.terminals.borrow_mut().push(other);
        let mut subscriber = namedrop::host::hc::events::Subscriber::default();
        subscriber
            .handle_line(
                &json!({"terminals":*fixture.terminals.borrow()}).to_string(),
                runtime,
            )
            .unwrap();
        let connection =
            rusqlite::Connection::open(runtime.state_directory.join("namedrop.sqlite3")).unwrap();
        for pane in ["t1", "t2", "never-readable"] {
            connection.execute("INSERT INTO transition_facts SELECT 'hc:fixture', 'fixture-space', MAX(worktree_order)+1, 'prompt_recipient', ?1, 'paused', 'plugin', 'fixture', 1 FROM transition_facts WHERE session_socket_id='hc:fixture' AND worktree_id='fixture-space'", [json!([pane,"dead-session"]).to_string()]).unwrap();
        }
        connection.execute("UPDATE order_counters SET next_order = (SELECT MAX(worktree_order)+1 FROM transition_facts WHERE session_socket_id='hc:fixture' AND worktree_id='fixture-space') WHERE session_socket_id='hc:fixture' AND worktree_id='fixture-space' AND stream_kind='worktree_transition' AND stream_id=''", []).unwrap();
        *fixture.get_failure.borrow_mut() = Some("t1".into());
        let event_result = subscriber.handle_lines(
            [
                labels().as_str(),
                r#"{"method":"event","params":{"type":"labels","terminal":"never-readable"}}"#,
            ],
            runtime,
        );
        fixture.calls.borrow_mut().clear();
        for _ in 0..10 {
            let _ = subscriber.handle_line(r#"{"method":"heartbeat","params":{}}"#, runtime);
        }
        assert!(
            fixture.calls.borrow().is_empty(),
            "failed get was retried: {:?}",
            fixture.calls.borrow()
        );
        event_result.unwrap();
        fixture.terminals.borrow_mut()[1]["labels"]["namedrop.name"] = json!("carol");
        subscriber
            .handle_line(
                r#"{"method":"event","params":{"type":"labels","terminal":"t2"}}"#,
                runtime,
            )
            .unwrap();
        let namespace = namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
        let addresses = namedrop::StateStore::open(runtime.state_directory)
            .unwrap()
            .transaction(|tx| tx.list_pane_addresses(&namespace))
            .unwrap();
        assert!(addresses.iter().any(|address| address.alias == "carol"));
        assert!(addresses.iter().any(|address| address.alias == "alice"));
        for (pane, expected) in [
            ("t1", "paused"),
            ("t2", "resumed"),
            ("never-readable", "paused"),
        ] {
            let state: String = connection.query_row("SELECT transition_kind FROM transition_facts WHERE entity_kind='prompt_recipient' AND entity_id=?1 ORDER BY worktree_order DESC LIMIT 1", [json!([pane,"dead-session"]).to_string()], |row| row.get(0)).unwrap();
            assert_eq!(state, expected, "recipient {pane}");
        }
        let mut report = namedrop::diagnose_collaboration(runtime);
        namedrop::append_hc_diagnostics(&mut report, runtime, &runtime.host);
        assert!(report.issues.iter().any(
            |issue| issue.code == "ND-HC-DISCOVERY-001" && issue.diagnostic["terminal"] == "t1"
        ));
    });
}

#[test]
fn unavailable_neighbor_does_not_block_healthy_mail() {
    check("unavailable-healthy-mail", |fixture, runtime| {
        let agent = process::AgentProcess::new();
        queued_agent(fixture, runtime, agent.pid());
        let mut other = fixture.terminals.borrow()[0].clone();
        other["address"] = json!("t2");
        other["labels"]["namedrop.name"] = json!("");
        fixture.terminals.borrow_mut().push(other);
        let mut subscriber = namedrop::host::hc::events::Subscriber::default();
        subscriber
            .handle_line(&session_event("snapshot", fixture), runtime)
            .unwrap();
        *fixture.get_failure.borrow_mut() = Some("t2".into());
        let failed = r#"{"method":"event","params":{"type":"labels","terminal":"t2"}}"#;
        subscriber.handle_line(failed, runtime).unwrap();
        fixture.terminals.borrow_mut()[0]["status"] = json!({"state":"done"});
        subscriber
            .handle_line(&session_event("status", fixture), runtime)
            .unwrap();
        assert_eq!(
            fixture
                .calls
                .borrow()
                .iter()
                .filter(|call| call[0] == "send" && call[1] == "t1")
                .count(),
            1
        );
    });
}

#[test]
fn due_mailbox_tick_does_not_retry_an_unavailable_dead_owner() {
    use namedrop::*;
    check("unavailable-due-tick", |fixture, runtime| {
        let dead_owner = process::AgentProcess::new();
        queued_agent(fixture, runtime, dead_owner.pid());
        let live_owner = process::AgentProcess::new();
        let mut other = fixture.terminals.borrow()[0].clone();
        other["address"] = json!("t2");
        other["labels"]["namedrop.name"] = json!("bob");
        other["labels"]["namedrop.session"] = json!("live-session");
        other["labels"]["namedrop.session.pid"] = json!(live_owner.pid().to_string());
        other["labels"]["namedrop.session.start"] = json!(process::start(live_owner.pid()));
        other["foreground"]["pid"] = json!(live_owner.pid());
        other["status"] = json!({"state":"done"});
        fixture.terminals.borrow_mut().push(other);
        let runtime = LifecycleRuntime {
            clock: &RetryClock(10_000),
            state_directory: runtime.state_directory,
            home_directory: runtime.home_directory,
            agent_config: runtime.agent_config,
            cwd: runtime.cwd,
            workspace_id: runtime.workspace_id,
            current_pane_id: runtime.current_pane_id,
            session_socket_id: runtime.session_socket_id,
            host: runtime.host,
            ids: runtime.ids,
        };
        let mut subscriber = host::hc::events::Subscriber::default();
        subscriber
            .handle_line(&session_event("snapshot", fixture), &runtime)
            .unwrap();
        let namespace = StateNamespace::for_space("hc:fixture", "fixture-space");
        let attribution = MutationAttribution::new(ActorKind::Human, "test", 10_000);
        let mut store = StateStore::open(runtime.state_directory).unwrap();
        store
            .transaction(|tx| {
                let binding = tx
                    .active_pane_bindings(&namespace)?
                    .into_iter()
                    .find(|binding| binding.pane_id == "t2")
                    .unwrap();
                tx.append_message(
                    &namespace,
                    &MessageDraft {
                        thread_id: ThreadId::new("death-thread"),
                        message_id: MessageId::new("live-message"),
                        in_reply_to: None,
                        author_address_id: binding.address_id.clone(),
                        author_kind: ActorKind::Human,
                        author_alias_snapshot: "bob".into(),
                        author_agent_session_snapshot: None,
                        request_id: "live-request".into(),
                        authored_source: "@bob test".into(),
                        body: "test".into(),
                        grammar_version: "mention/v1".into(),
                        recipients: vec![MessageRecipientDraft {
                            address_id: binding.address_id,
                            alias_snapshot: "bob".into(),
                            agent_session_snapshot: Some(AgentSessionId::new("live-session")),
                            delivery_id: DeliveryId::new("live-delivery"),
                        }],
                        context_references: vec![],
                    },
                    &attribution,
                )?;
                Ok(())
            })
            .unwrap();
        subscriber
            .handle_line(
                r#"{"method":"event","params":{"type":"status","terminal":"t2"}}"#,
                &runtime,
            )
            .unwrap();
        assert_eq!(
            fixture
                .calls
                .borrow()
                .iter()
                .filter(|call| call[0] == "send" && call[1] == "t2")
                .count(),
            1
        );
        drop(dead_owner);
        *fixture.get_failure.borrow_mut() = Some("t1".into());
        subscriber.handle_line(&labels(), &runtime).unwrap();
        let connection =
            rusqlite::Connection::open(runtime.state_directory.join("namedrop.sqlite3")).unwrap();
        let attempt: String = connection
            .query_row(
                "SELECT attempt_id FROM delivery_attempts WHERE delivery_id='live-delivery'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            store
                .transaction(|tx| tx
                    .delivery_attempt_state(&namespace, &DeliveryAttemptId::new(attempt.clone())))
                .unwrap(),
            DeliveryState::Acknowledged
        );
        fixture.calls.borrow_mut().clear();
        let due = LifecycleRuntime {
            clock: &RetryClock(40_000),
            ..runtime
        };
        let result = subscriber.tick(&due);
        assert_eq!(
            fixture
                .calls
                .borrow()
                .iter()
                .filter(|call| call[0] == "get" && call[1] == "t1")
                .count(),
            0,
            "due tick retried unavailable pane: {:?}",
            fixture.calls.borrow()
        );
        result.unwrap();
        let state = store
            .transaction(|tx| {
                tx.delivery_attempt_state(&namespace, &DeliveryAttemptId::new(attempt))
            })
            .unwrap();
        assert_eq!(
            state,
            DeliveryState::Settled,
            "independent delivery grace must still run"
        );
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
            "original-session"
        );
    });
}
