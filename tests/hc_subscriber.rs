use namedrop::{
    HcHost, HerdrCommandExecutor, HerdrCommandOutput,
};
use serde_json::{Value, json};
use std::{cell::RefCell, path::Path};

struct Fixture {
    terminals: RefCell<Vec<Value>>,
    calls: RefCell<Vec<Vec<String>>>,
    send_error: Option<String>,
    get_failure: RefCell<Option<String>>,
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
            ["ls", "--json"] => json!({"terminals": *self.terminals.borrow()}).to_string(),
            ["get", id, "--json"] if self.get_failure.borrow().as_deref() == Some(id) => return Err("terminal inspection failed".into()),
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

fn check(name: &str, run: impl FnOnce(&Fixture, &namedrop::LifecycleRuntime<'_, HcHost<&Fixture>, namedrop::SystemClock, namedrop::ProcessIds>)) {
    let fixture = Fixture::default();
    let state = std::env::temp_dir().join(format!("namedrop-subscriber-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    let home = state.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let clock = namedrop::SystemClock;
    let ids = namedrop::ProcessIds::default();
    let runtime = namedrop::LifecycleRuntime {
        state_directory: &state, home_directory: Some(&home),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new(env!("CARGO_MANIFEST_DIR")), workspace_id: Some("fixture-space"),
        current_pane_id: Some("t1"), session_socket_id: "hc:fixture",
        host: HcHost::new(&fixture), clock: &clock, ids: &ids,
    };
    namedrop::execute_lifecycle_operation("name", json!({"alias":"alice", "pane_type":"terminal"}), &runtime).unwrap();
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
        let result = namedrop::host::hc::events::Subscriber::default().handle_line(&labels(), runtime);
        assert!(result.is_err(), "name rejection must be returned, got {result:?}");
        let terminal = &fixture.terminals.borrow()[0];
        let error: Value = serde_json::from_str(terminal["labels"]["namedrop.name.error"].as_str().unwrap()).unwrap();
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
        let result = namedrop::host::hc::events::Subscriber::default().handle_line(r#"{"method":"missed","params":{}}"#, runtime);
        assert!(result.is_err());
        let addresses = namedrop::StateStore::open(runtime.state_directory).unwrap().transaction(|tx| tx.list_pane_addresses(&namedrop::StateNamespace::for_space("hc:fixture","fixture-space"))).unwrap();
        assert!(addresses.iter().any(|a| a.alias == "bob" && a.pane_id.as_deref() == Some("t2")), "healthy terminal was skipped: {addresses:?}");
    });
}
#[test]
fn restarted_subscriber_moves_durable_binding() {
    check("move", |fixture, runtime| {
        fixture.terminals.borrow_mut()[0]["labels"]["herdr.space"] = json!("destination");
        let destination = namedrop::LifecycleRuntime { workspace_id: Some("destination"), ..*runtime };
        namedrop::execute_lifecycle_operation("activate", json!({"confirmed":true}), &destination).unwrap();
        namedrop::host::hc::events::Subscriber::default().handle_line(&labels(), runtime).unwrap();
        let mut store = namedrop::StateStore::open(runtime.state_directory).unwrap();
        let old = store.transaction(|tx| tx.list_pane_addresses(&namedrop::StateNamespace::for_space("hc:fixture","fixture-space"))).unwrap();
        assert!(old.iter().all(|a| a.pane_id.is_none()), "old binding survived move: {old:?}");
        let new = store.transaction(|tx| tx.list_pane_addresses(&namedrop::StateNamespace::for_space("hc:fixture","destination"))).unwrap();
        assert_eq!(new.iter().filter(|a| a.pane_id.as_deref() == Some("t1")).count(), 1);
    });
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
    std::fs::write(&hc, format!(r#"#!/usr/bin/python3
import json, os, sys, time
with open(os.environ['HC_TEST_LOG'], 'a') as log:
    log.write(json.dumps({{'args':sys.argv[1:], 'env':dict(os.environ)}})+'\n')
if sys.argv[1:] == ['server', 'status', '--json']:
    print({status:?})
elif sys.argv[1:] == ['events', '--json']:
    while True:
        print('{{"method":"heartbeat","params":{{}}}}', flush=True)
        time.sleep(.05)
elif sys.argv[1:] == ['ls', '--json']:
    print('{{"terminals":[]}}')
else:
    raise SystemExit('unexpected hc command '+str(sys.argv))
"#, status=include_str!("fixtures/hc/server-status.stdout"))).unwrap();
    std::fs::set_permissions(&hc, std::fs::Permissions::from_mode(0o700)).unwrap();
    let binary = root.join("namedrop");
    std::fs::copy(env!("CARGO_BIN_EXE_namedrop"), &binary).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "detached_subscriber_child", "--nocapture"])
        .env_clear().env("PATH", "/usr/bin:/bin").env("HOME", root.join("home"))
        .env("XDG_CONFIG_HOME", root.join("config")).env("XDG_STATE_HOME", root.join("xdg"))
        .env("XDG_RUNTIME_DIR", &root).env("CODEX_HOME", root.join("codex"))
        .env("CLAUDE_CONFIG_DIR", root.join("claude")).env("HC_BIN_PATH", &hc)
        .env("HC_TERMINAL_SOCKET", &socket).env("HC_TERMINAL_ID", "forbidden")
        .env("HERDR_SOCKET_PATH", "forbidden").env("HERDR_CUSTOM", "forbidden")
        .env("HC_TEST_LOG", root.join("calls.jsonl")).env("SUBSCRIBER_TEST_ROOT", &root)
        .output().unwrap();
    assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    let calls = std::fs::read_to_string(root.join("calls.jsonl")).unwrap();
    let events: Vec<Value> = calls.lines().map(|line| serde_json::from_str::<Value>(line).unwrap())
        .filter(|call| call["args"] == json!(["events", "--json"])).collect();
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
    let Some(root) = std::env::var_os("SUBSCRIBER_TEST_ROOT").map(std::path::PathBuf::from) else { return; };
    let fixture = Fixture::default();
    let host = HcHost::new(&fixture);
    let state = root.join("state");
    let binary = root.join("namedrop");
    namedrop::host::hc::events::start(&host, &binary, &state).unwrap();
    let directory = std::fs::read_dir(state.join("hc-subscribers")).unwrap().next().unwrap().unwrap().path();
    let record = || serde_json::from_slice::<Value>(&std::fs::read(directory.join("subscriber.json")).unwrap()).unwrap();
    let first = record();
    let pid = first["pid"].as_u64().unwrap() as i32;
    struct Cleanup(Vec<i32>);
    impl Drop for Cleanup {
        fn drop(&mut self) { for pid in &self.0 { unsafe { libc::kill(*pid, libc::SIGTERM); } } }
    }
    let mut cleanup = Cleanup(vec![pid]);
    namedrop::host::hc::events::start(&host, &binary, &state).unwrap();
    assert_eq!(record()["pid"], first["pid"]);
    assert_eq!(unsafe { libc::getsid(pid) }, pid, "subscriber was not detached");
    let replacement = root.join("replacement");
    std::fs::copy(&binary, &replacement).unwrap();
    std::fs::rename(replacement, &binary).unwrap();
    namedrop::host::hc::events::start(&host, &binary, &state).unwrap();
    let second = record();
    let second_pid = second["pid"].as_u64().unwrap() as i32;
    cleanup.0.push(second_pid);
    assert_ne!(second["pid"], first["pid"], "activation retained stale subscriber");
    let replacement = root.join("replacement");
    std::fs::copy(&binary, &replacement).unwrap();
    std::fs::rename(replacement, &binary).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let exited = std::fs::read_to_string(format!("/proc/{second_pid}/stat")).map_or(true, |stat| stat.rsplit_once(')').unwrap().1.trim_start().starts_with('Z'));
        if exited { break; }
        assert!(std::time::Instant::now() < deadline, "subscriber did not exit after binary replacement");
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
}
