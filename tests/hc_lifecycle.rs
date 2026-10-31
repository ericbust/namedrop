use namedrop::{
    ActorKind, AgentConfigDirectories, HcHost, HerdrCommandExecutor, HerdrCommandOutput,
    LifecycleRuntime, MutationAttribution, PaneAddressId, ProcessIds, StateNamespace, StateStore,
    SystemClock, execute_lifecycle_operation,
};
use serde_json::{Value, json};
use std::{cell::RefCell, fs, path::PathBuf};

struct Fixture {
    terminal: RefCell<Value>,
    label_error: RefCell<bool>,
}

impl Fixture {
    fn new() -> Self {
        let mut terminal: Value =
            serde_json::from_str(include_str!("fixtures/hc/get-labeled.stdout")).unwrap();
        terminal["labels"]["namedrop.name"] = json!("");
        Self {
            terminal: RefCell::new(terminal),
            label_error: RefCell::new(false),
        }
    }
}

impl HerdrCommandExecutor for Fixture {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let stdout = match args {
            ["ls", "--json"] => json!({"terminals": [&*self.terminal.borrow()]}).to_string(),
            ["get", "t1", "--json"] => self.terminal.borrow().to_string(),
            ["label", "t1", labels @ ..] => {
                if *self.label_error.borrow() {
                    return Err("label write rejected".into());
                }
                for label in labels.iter().filter(|label| **label != "--json") {
                    let (key, value) = label.split_once('=').unwrap();
                    self.terminal.borrow_mut()["labels"][key] = json!(value);
                }
                "{}".into()
            }
            args => panic!("unexpected hc call {args:?}"),
        };
        Ok(HerdrCommandOutput { stdout })
    }
}

fn root(case: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/hc-lifecycle")
        .join(format!("{case}-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    root
}

#[test]
fn create_and_retire_update_the_bound_pane_label() {
    address_labels(false);
}

#[test]
fn rejected_create_and_retire_labels_roll_back_addresses_and_bindings() {
    address_labels(true);
}

fn address_labels(reject: bool) {
    let root = root(if reject { "rollback" } else { "labels" });
    let state = root.join("state");
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap();
    let fixture = Fixture::new();
    let clock = SystemClock;
    let ids = ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: &state,
        home_directory: Some(&home),
        agent_config: AgentConfigDirectories::default(),
        cwd: &root,
        workspace_id: Some("fixture-space"),
        current_pane_id: Some("t1"),
        session_socket_id: "hc:lifecycle",
        host: HcHost::new(&fixture),
        clock: &clock,
        ids: &ids,
    };
    let namespace = StateNamespace::for_space("hc:lifecycle", "fixture-space");
    let mut store = StateStore::open(&state).unwrap();
    store
        .transaction(|tx| {
            tx.activate_worktree(
                &namespace,
                &MutationAttribution::new(ActorKind::Human, "test", 1),
            )
        })
        .unwrap();
    let create = || {
        execute_lifecycle_operation(
            "address.create",
            json!({"role_name":"Planner", "role_description":"Plan work", "alias":"planner"}),
            &runtime,
        )
    };
    if reject {
        *fixture.label_error.borrow_mut() = true;
        assert!(create().unwrap_err().contains("label write rejected"));
        store
            .transaction(|tx| {
                assert!(tx.list_pane_addresses(&namespace)?.is_empty());
                assert!(tx.active_pane_bindings(&namespace)?.is_empty());
                Ok(())
            })
            .unwrap();
        assert_eq!(fixture.terminal.borrow()["labels"]["namedrop.name"], "");
        *fixture.label_error.borrow_mut() = false;
    }
    let created = create().unwrap();
    let address_id = PaneAddressId::new(created["id"].as_str().unwrap());
    assert_eq!(
        fixture.terminal.borrow()["labels"]["namedrop.name"],
        "planner"
    );
    let retire = || {
        execute_lifecycle_operation(
            "address.retire",
            json!({"address_id": address_id}),
            &runtime,
        )
    };
    if reject {
        *fixture.label_error.borrow_mut() = true;
        assert!(retire().unwrap_err().contains("label write rejected"));
        store
            .transaction(|tx| {
                let addresses = tx.list_pane_addresses(&namespace)?;
                assert_eq!(addresses.len(), 1);
                assert!(!addresses[0].retired);
                assert_eq!(addresses[0].pane_id.as_deref(), Some("t1"));
                assert_eq!(tx.active_pane_bindings(&namespace)?.len(), 1);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            fixture.terminal.borrow()["labels"]["namedrop.name"],
            "planner"
        );
        *fixture.label_error.borrow_mut() = false;
    }
    retire().unwrap();
    assert_eq!(fixture.terminal.borrow()["labels"]["namedrop.name"], "");
    store
        .transaction(|tx| {
            assert!(
                tx.list_pane_addresses(&namespace)?
                    .iter()
                    .all(|address| address.retired)
            );
            assert!(tx.active_pane_bindings(&namespace)?.is_empty());
            Ok(())
        })
        .unwrap();
    fs::remove_dir_all(root).unwrap();
}
