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

#[test]
fn hc_background_lifecycle_preserves_packages_until_a_user_refreshes_them() {
    use std::{collections::BTreeMap, os::unix::fs::PermissionsExt, path::Path};

    fn snapshot(root: &Path) -> BTreeMap<PathBuf, (u32, Vec<u8>)> {
        fn visit(root: &Path, path: &Path, result: &mut BTreeMap<PathBuf, (u32, Vec<u8>)>) {
            let metadata = fs::metadata(path).unwrap();
            result.insert(
                path.strip_prefix(root).unwrap().to_owned(),
                (
                    metadata.permissions().mode(),
                    if metadata.is_file() {
                        fs::read(path).unwrap()
                    } else {
                        vec![]
                    },
                ),
            );
            if metadata.is_dir() {
                for entry in fs::read_dir(path).unwrap() {
                    visit(root, &entry.unwrap().path(), result);
                }
            }
        }
        let mut result = BTreeMap::new();
        visit(root, root, &mut result);
        result
    }

    let root = root("background-packages");
    let home = root.join("foreign-home");
    let claude = root.join("foreign-claude-config");
    let codex = root.join("foreign-codex-config");
    let state = root.join("state");
    let fixture = Fixture::new();
    let clock = SystemClock;
    let ids = ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: &state,
        home_directory: Some(&home),
        agent_config: AgentConfigDirectories {
            claude: Some(&claude),
            codex: Some(&codex),
        },
        cwd: &root,
        workspace_id: Some("fixture-space"),
        current_pane_id: Some("t1"),
        session_socket_id: "hc:lifecycle",
        host: HcHost::new(&fixture),
        clock: &clock,
        ids: &ids,
    };
    let activation =
        execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime).unwrap();
    for agent in ["codex", "claude-code"] {
        assert_eq!(
            activation["integration_packages"][agent]["installed"], true,
            "{activation}"
        );
    }
    let roots = [&home, &claude, &codex];
    let installed = roots.map(|path| snapshot(path));
    let command = home.join(".local/bin/namedrop-app");
    fs::write(
        &command,
        "#!/bin/sh\n# Managed by Namedrop: shell command.\ncorrupt bootstrap\n",
    )
    .unwrap();
    fs::set_permissions(&command, fs::Permissions::from_mode(0o400)).unwrap();
    let damaged = roots.map(|path| snapshot(path));
    for (operation, input) in [
        (
            "event.name",
            json!({"alias":"background", "pane_type":"terminal"}),
        ),
        ("event.reconcile", json!({})),
        ("reconcile", json!({})),
    ] {
        execute_lifecycle_operation(operation, input, &runtime).unwrap();
        assert_eq!(
            roots.map(|path| snapshot(path)),
            damaged,
            "{operation} changed packages"
        );
    }
    let namespace = StateNamespace::for_space("hc:lifecycle", "fixture-space");
    let mut store = StateStore::open(&state).unwrap();
    let addresses = store
        .transaction(|tx| tx.list_pane_addresses(&namespace))
        .unwrap();
    assert_eq!(addresses[0].alias, "background");
    let attention = store
        .transaction(|tx| tx.unresolved_attention_items(&namespace))
        .unwrap();
    assert_eq!(
        attention
            .iter()
            .filter(|item| item.source_id.starts_with("integration-package"))
            .count(),
        2
    );
    execute_lifecycle_operation(
        "address.remove",
        json!({"address_id": addresses[0].id}),
        &runtime,
    )
    .unwrap();
    execute_lifecycle_operation(
        "address.create",
        json!({"role_name":"Planner", "role_description":"Plan work", "alias":"planner"}),
        &runtime,
    )
    .unwrap();
    assert!(
        roots.map(|path| snapshot(path)) == damaged,
        "address.create changed packages"
    );
    execute_lifecycle_operation(
        "name",
        json!({"alias":"user", "pane_type":"terminal"}),
        &runtime,
    )
    .unwrap();
    assert!(
        roots.map(|path| snapshot(path)) == installed,
        "user name must repair owned package bytes and modes"
    );
    fs::remove_dir_all(root).unwrap();
}
