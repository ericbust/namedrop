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
