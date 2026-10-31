#[path = "support/app_popup.rs"]
mod app_popup;
#[path = "support/hc_socket.rs"]
mod hc_socket;
#[path = "fixtures/hc/process.rs"]
mod process;
use namedrop::{
    CallerClaim, HcHost, HerdrCommandExecutor, HerdrCommandOutput, Host, PaneMetadata, PaneScope,
    PromptFailureKind, SpaceLocator,
};
use serde_json::{Value, json};
use std::{cell::RefCell, path::Path};

struct Fixture {
    environment: namedrop::environment::EnvironmentSnapshot,
    _shell: process::AgentProcess,
    terminals: RefCell<Vec<Value>>,
    calls: RefCell<Vec<Vec<String>>>,
    send_error: RefCell<Option<String>>,
    read_text: Option<String>,
    on_send: RefCell<Option<Box<dyn Fn()>>>,
    retire_when_leased: RefCell<Option<std::path::PathBuf>>,
    clear_error: RefCell<Option<String>>,
    rename_error: RefCell<bool>,
}
impl Default for Fixture {
    fn default() -> Self {
        let shell = process::AgentProcess::shell();
        let mut terminal: Value =
            serde_json::from_str(include_str!("fixtures/hc/get-labeled.stdout")).unwrap();
        terminal["pid"] = json!(shell.pid());
        terminal["foreground"]["pid"] = json!(shell.pid());
        Self {
            environment: namedrop::environment::EnvironmentSnapshot::from_variables([(
                "XDG_STATE_HOME".into(),
                std::env::temp_dir()
                    .join(format!("hc-host-fixture-{}", std::process::id()))
                    .into_os_string(),
            )]),
            _shell: shell,
            terminals: RefCell::new(vec![terminal]),
            calls: RefCell::new(Vec::new()),
            send_error: RefCell::new(None),
            read_text: None,
            on_send: RefCell::new(None),
            retire_when_leased: RefCell::new(None),
            clear_error: RefCell::new(None),
            rename_error: RefCell::new(false),
        }
    }
}
impl HerdrCommandExecutor for Fixture {
    fn open_hc_popup(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let environment = namedrop::environment::EnvironmentSnapshot::capture();
        if environment.var_os("HERDR_APP_BIN_PATH").is_none() {
            return Err("test app override missing".into());
        }
        namedrop::ProcessHerdr::from_snapshot(namedrop::runtime::HostKind::Hc, &environment)
            .open_hc_popup(args)
    }
    fn environment_snapshot(&self) -> Option<&namedrop::environment::EnvironmentSnapshot> {
        Some(&self.environment)
    }
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|arg| (*arg).to_owned()).collect());
        let stdout = match args {
            ["ls", "--json"] => json!({"terminals": *self.terminals.borrow()}).to_string(),
            ["get", id, "--json"] => {
                if let Some(state) = self.retire_when_leased.borrow().as_ref() {
                    let database =
                        rusqlite::Connection::open(state.join("namedrop.sqlite3")).unwrap();
                    let leased: bool = database
                        .query_row("SELECT EXISTS(SELECT 1 FROM leases)", [], |row| row.get(0))
                        .unwrap();
                    if leased {
                        self.terminals.borrow_mut()[0]["labels"]["namedrop.session.pid"] =
                            json!("4000000000");
                    }
                }
                self.terminals
                    .borrow()
                    .iter()
                    .find(|terminal| terminal["address"] == *id)
                    .ok_or_else(|| include_str!("fixtures/hc/not-found.stderr").to_owned())?
                    .to_string()
            }
            ["read", ..] => self
                .read_text
                .as_ref()
                .map(|text| json!({"text":text}).to_string())
                .unwrap_or_else(|| include_str!("fixtures/hc/read.stdout").to_owned()),
            ["send", ..] => {
                if let Some(callback) = self.on_send.borrow_mut().take() {
                    callback();
                }
                if let Some(error) = self.send_error.borrow().as_ref() {
                    return Err(error.clone());
                }
                include_str!("fixtures/hc/send.stdout").to_owned()
            }
            ["server", "status", "--json"] => {
                include_str!("fixtures/hc/server-status.stdout").to_owned()
            }
            ["--version"] => "herdr-core 0.1.14\n".to_owned(),
            ["label", id, labels @ ..] => {
                let mut terminals = self.terminals.borrow_mut();
                let terminal = terminals
                    .iter_mut()
                    .find(|terminal| terminal["address"] == *id)
                    .unwrap();
                for label in labels.iter().filter(|label| **label != "--json") {
                    let (key, value) = label.split_once('=').unwrap();
                    terminal["labels"][key] = json!(value);
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
                "{}".into()
            }
            ["interrupt", ..] => "{}".into(),
            other => panic!("unexpected hc argv {other:?}"),
        };
        Ok(HerdrCommandOutput { stdout })
    }
}
impl Fixture {
    fn agent(&self, pid: u32) {
        let mut terminals = self.terminals.borrow_mut();
        terminals[0]["labels"]["namedrop.session.pid"] = json!(pid.to_string());
        terminals[0]["labels"]["namedrop.session.start"] = json!(process::start(pid));
        terminals[0]["foreground"] = json!({"pid":pid,"argv":["codex"],"name":"codex"});
        terminals[0]["status"] = json!({"state":"idle","app":"codex"});
    }
    fn take_calls(&self) -> Vec<Vec<String>> {
        std::mem::take(&mut *self.calls.borrow_mut())
    }
}

#[test]
fn recorded_terminal_discovers_labels_namespace_and_processes() {
    let fixture = Fixture::default();
    let host = HcHost::new(&fixture);
    let panes = host.panes(PaneScope::All).unwrap();
    let pane = &panes[0];
    assert_eq!(pane.pane_id, "t1");
    assert_eq!(pane.terminal_id, "t1");
    assert_eq!(pane.workspace_id, "fixture-space");
    assert_eq!(pane.tab_id, "fixture-tab");
    assert_eq!(pane.agent, None);
    assert_eq!(pane.status_reported, Some(false));
    assert_eq!(pane.agent_status, None);
    assert_eq!(pane.pane_label.as_deref(), Some("@fixture-name"));
    assert!(pane.agent_session_id.is_none());
    let spaces = host.spaces().unwrap();
    assert_eq!(spaces[0].label, "Fixture Space");
    assert_eq!(
        host.socket_id().as_deref(),
        Some("hc:628bcf8129aa037b1fa5aad3504926a6")
    );
    let process = host.process_info("t1").unwrap();
    assert_eq!(process.shell_pid, process.foreground_process_group_id);
    assert_eq!(
        u64::from(process.foreground_processes[0].pid),
        process.shell_pid
    );
    assert!(
        host.panes(PaneScope::Workspace("other"))
            .unwrap()
            .is_empty()
    );
    let space = host
        .space(SpaceLocator::Workspace("fixture-space"), None)
        .unwrap();
    assert_eq!(space.panes.len(), 1);
    assert!(host.plugin_root().unwrap().is_absolute());
    assert_eq!(host.version().unwrap(), "herdr-core 0.1.14");
}

#[test]
fn golden_argv_and_idempotent_metadata() {
    let fixture = Fixture::default();
    let host = HcHost::new(&fixture);
    host.read("t1", 40, None).unwrap();
    host.read("t1", 1000, None).unwrap();
    assert!(host.prompt("t1", "hello").is_err());
    host.run_in_pane("t1", "true").unwrap();
    host.interrupt("t1").unwrap();
    host.report_metadata(
        "t1",
        &PaneMetadata::Counters {
            awaiting: 2,
            attention: 3,
        },
    )
    .unwrap();
    host.report_metadata(
        "t1",
        &PaneMetadata::Counters {
            awaiting: 2,
            attention: 3,
        },
    )
    .unwrap();
    fixture.terminals.borrow_mut()[0]["labels"]["namedrop.name.error"] = json!("stale failure");
    host.report_metadata(
        "t1",
        &PaneMetadata::Identity {
            title: "@alice".into(),
            ready: "".into(),
            working: "".into(),
            blocked: "".into(),
            unknown: "".into(),
        },
    )
    .unwrap();
    host.report_metadata(
        "t1",
        &PaneMetadata::Relay {
            text: "ignored".into(),
            ttl_ms: 1,
        },
    )
    .unwrap();
    host.notify("title", "body", true).unwrap();
    host.reload_config().unwrap();
    host.rename_tab("tab", "name").unwrap();
    assert_eq!(host.tab_label("tab"), None);
    assert_eq!(
        fixture.take_calls(),
        vec![
            vec!["get", "t1", "--json"],
            vec!["read", "t1", "--json"],
            vec!["get", "t1", "--json"],
            vec!["read", "t1", "--lines", "1000", "--json"],
            vec!["get", "t1", "--json"],
            vec!["get", "t1", "--json"],
            vec!["label", "t1", "namedrop.session=", "--json"],
            vec!["get", "t1", "--json"],
            vec!["send", "t1", "true", "--json"],
            vec!["interrupt", "t1", "--timeout", "2", "--json"],
            vec!["get", "t1", "--json"],
            vec![
                "label",
                "t1",
                "namedrop.awaiting=2",
                "namedrop.attention=3",
                "--json"
            ],
            vec!["get", "t1", "--json"],
            vec!["get", "t1", "--json"],
            vec![
                "label",
                "t1",
                "namedrop.name=alice",
                "namedrop.name.error=",
                "--json"
            ],
        ]
    );
}

#[test]
fn status_words_and_program_fallback_keep_unknown_distinct() {
    let claude = process::AgentProcess::with_program("claude");
    let codex = process::AgentProcess::new();
    let fixture = Fixture::default();
    fixture.terminals.borrow_mut()[0]["foreground"]["argv"] = json!(["claude"]);
    fixture.terminals.borrow_mut()[0]["foreground"]["pid"] = json!(claude.pid());
    let host = HcHost::new(&fixture);
    for (word, expected) in [
        ("idle", "idle"),
        ("working", "working"),
        ("done", "done"),
        ("blocked", "blocked"),
        ("error", "blocked"),
    ] {
        fixture.terminals.borrow_mut()[0]["status"] = json!({"state":word,"app":"claude-code"});
        fixture.terminals.borrow_mut()[0]["reported"] = json!(true);
        let pane = host.panes(PaneScope::All).unwrap().remove(0);
        assert_eq!(pane.agent_status.as_deref(), Some(expected));
        assert_eq!(pane.agent.as_deref(), Some("claude"));
        assert_eq!(pane.status_reported, Some(true));
    }
    fixture.terminals.borrow_mut()[0]["status"] = Value::Null;
    fixture.terminals.borrow_mut()[0]["reported"] = json!(false);
    fixture.terminals.borrow_mut()[0]["foreground"]["argv"] = json!(["/bin/codex"]);
    fixture.terminals.borrow_mut()[0]["foreground"]["pid"] = json!(codex.pid());
    let pane = host.panes(PaneScope::All).unwrap().remove(0);
    assert_eq!(pane.agent_status, None);
    assert_eq!(pane.agent.as_deref(), Some("codex"));
    assert_eq!(pane.status_reported, Some(false));
}

#[test]
fn errors_classify_by_kind_and_pretyping_hint() {
    for (kind, hint, expected) in [
        ("not_found", "", PromptFailureKind::TemporaryPreInvocation),
        ("finished", "", PromptFailureKind::TemporaryPreInvocation),
        (
            "blocked",
            "",
            PromptFailureKind::DeferredUntilSettle {
                observed_status_ms: None,
            },
        ),
        (
            "status_mismatch",
            "",
            PromptFailureKind::DeferredUntilSettle {
                observed_status_ms: None,
            },
        ),
        (
            "unavailable",
            "nothing was typed",
            PromptFailureKind::TemporaryPreInvocation,
        ),
        (
            "unavailable",
            "try again",
            PromptFailureKind::AmbiguousInvocation,
        ),
        ("usage", "", PromptFailureKind::DefinitiveNonRetryable),
        (
            "invalid_params",
            "",
            PromptFailureKind::DefinitiveNonRetryable,
        ),
        ("config", "", PromptFailureKind::DefinitiveNonRetryable),
        (
            "machine_unknown",
            "",
            PromptFailureKind::DefinitiveNonRetryable,
        ),
        ("timeout", "", PromptFailureKind::AmbiguousInvocation),
    ] {
        let fixture = Fixture {
            send_error: RefCell::new(Some(
                json!({"error":{"kind":kind,"message":"rejected","hint":hint}}).to_string(),
            )),
            ..Fixture::default()
        };
        let agent = process::AgentProcess::new();
        fixture.agent(agent.pid());
        let failure = HcHost::new(&fixture).prompt("t1", "hello").unwrap_err();
        assert_eq!(failure.kind, expected, "{kind} {hint}: {}", failure.message);
        assert!(failure.message.contains(hint));
    }
}

#[test]
fn identity_requires_ancestry_or_one_exact_live_session() {
    let fixture = Fixture::default();
    let host = HcHost::new(&fixture);
    let mut claim = CallerClaim {
        workspace: None,
        pane: None,
        inherited_pane: Some("t1".into()),
        agent_sessions: vec![],
    };
    fixture.terminals.borrow_mut()[0]["pid"] = json!(4_000_000_000u32);
    assert!(host.caller_identity(&claim).is_err());
    let agent = process::AgentProcess::new();
    fixture.agent(agent.pid());
    claim.agent_sessions.push("fixture-session".into());
    assert_eq!(
        host.caller_identity(&claim).unwrap().pane.as_deref(),
        Some("t1")
    );
    let mut duplicate = fixture.terminals.borrow()[0].clone();
    duplicate["address"] = json!("t2");
    fixture.terminals.borrow_mut().push(duplicate);
    assert!(host.caller_identity(&claim).is_err());
    fixture.terminals.borrow_mut()[1]["state"] = json!("finished");
    assert!(host.caller_identity(&claim).is_ok());
    claim.agent_sessions.clear();
    fixture.terminals.borrow_mut()[0]["pid"] = json!(std::os::unix::process::parent_id());
    assert!(host.caller_identity(&claim).is_ok());
    claim.pane = Some("t2".into());
    claim.workspace = Some("explicit-space".into());
    assert_eq!(
        host.caller_identity(&claim).unwrap().workspace.as_deref(),
        Some("explicit-space")
    );
}

#[test]
fn checkout_ownership_uses_all_roots_and_rejects_multiple_spaces() {
    let fixture = Fixture::default();
    let host = HcHost::new(&fixture);
    let cwd = env!("CARGO_MANIFEST_DIR");
    fixture.terminals.borrow_mut()[0]["foreground"]["cwd"] = json!(cwd);
    let space = host
        .space(SpaceLocator::Checkout(Path::new(cwd)), None)
        .unwrap();
    assert!(space.worktree_attached);
    let mut other = fixture.terminals.borrow()[0].clone();
    other["address"] = json!("t2");
    other["labels"]["herdr.space"] = json!("second");
    fixture.terminals.borrow_mut().push(other);
    assert!(
        host.space(SpaceLocator::Checkout(Path::new(cwd)), None)
            .unwrap_err()
            .message
            .contains("--hc-space")
    );
    fixture.terminals.borrow_mut()[1]["labels"]["herdr.space"] = json!("fixture-space");
    fixture.terminals.borrow_mut()[1]["foreground"]["cwd"] = json!("/not-a-checkout");
    let space = host
        .space(SpaceLocator::Workspace("fixture-space"), None)
        .unwrap();
    assert!(!space.worktree_attached);
    assert_eq!(space.worktree_id.as_str(), "hc-space:fixture-space");
}

fn check_name_mutation(operation: &str, failure: bool) {
    check_name_mutation_failure(operation, failure, false);
}

fn check_name_mutation_failure(operation: &str, failure: bool, target_failure: bool) {
    let fixture = Fixture::default();
    let state = std::env::temp_dir().join(format!(
        "namedrop-hc-mutation-{}-{operation}-{failure}-{target_failure}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
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
        json!({"alias":"alice","pane_type":"terminal"}),
        &runtime,
    )
    .unwrap();
    let names = || {
        namedrop::StateStore::open(&state)
            .unwrap()
            .transaction(|tx| {
                tx.list_pane_addresses(&namedrop::StateNamespace::for_space(
                    "hc:fixture",
                    "fixture-space",
                ))
            })
            .unwrap()
    };
    let address_id = names()[0].id.clone();
    let mut other = fixture.terminals.borrow()[0].clone();
    other["address"] = json!("t2");
    other["labels"]["namedrop.name"] = json!("");
    fixture.terminals.borrow_mut().push(other);
    let rename = matches!(operation, "name" | "address.rename");
    if failure {
        if rename || target_failure {
            *fixture.rename_error.borrow_mut() = true;
        } else {
            *fixture.clear_error.borrow_mut() = Some("label clear failed".into());
        }
    }
    let input = match operation {
        "name" => json!({"alias":"bob","pane_type":"terminal"}),
        "address.rename" => json!({"address_id":address_id,"alias":"bob"}),
        "address.bind" => json!({"address_id":address_id,"pane_id":"t2"}),
        "address.unbind" => json!({"address_id":address_id}),
        _ => unreachable!(),
    };
    let labels_event = || {
        for pane in ["t1", "t2"] {
            let event = json!({"jsonrpc":"2.0","method":"event","params":{"type":"labels","terminal":pane,"data":{}}}).to_string();
            namedrop::host::hc::events::Subscriber::default()
                .handle_line(&event, &runtime)
                .unwrap();
        }
    };
    let result = namedrop::execute_lifecycle_operation(operation, input.clone(), &runtime);
    if failure {
        assert!(
            result.is_err(),
            "{operation} must report the failed label write"
        );
        assert_eq!(names()[0].alias, "alice", "failed rename must not commit");
        assert_eq!(names()[0].pane_id.as_deref(), Some("t1"));
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.name"],
            "alice"
        );
        assert_eq!(fixture.terminals.borrow()[1]["labels"]["namedrop.name"], "");
        labels_event();
        assert_eq!(names().len(), 1);
        assert_eq!(names()[0].alias, "alice");
        assert_eq!(names()[0].pane_id.as_deref(), Some("t1"));
        *fixture.rename_error.borrow_mut() = false;
        *fixture.clear_error.borrow_mut() = None;
        namedrop::execute_lifecycle_operation(operation, input, &runtime).unwrap();
    } else {
        result.unwrap();
    }
    assert_eq!(
        fixture.terminals.borrow()[0]["labels"]["namedrop.name"],
        if rename { "bob" } else { "" }
    );
    assert_eq!(names()[0].alias, if rename { "bob" } else { "alice" });
    labels_event();
    assert_eq!(
        names().len(),
        1,
        "queued label events must not resurrect an old binding"
    );
    assert_eq!(names()[0].alias, if rename { "bob" } else { "alice" });
    assert_eq!(
        names()[0].pane_id.as_deref(),
        match operation {
            "address.bind" => Some("t2"),
            "address.unbind" => None,
            _ => Some("t1"),
        }
    );
    assert_eq!(
        fixture.terminals.borrow()[1]["labels"]["namedrop.name"],
        if operation == "address.bind" {
            "alice"
        } else {
            ""
        }
    );
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn existing_name_replacement_rolls_back_on_label_failure() {
    check_name_mutation("name", true);
}
#[test]
fn address_rename_rolls_back_on_label_failure() {
    check_name_mutation("address.rename", true);
}
#[test]
fn unbind_clears_the_old_pane_name() {
    check_name_mutation("address.unbind", false);
}
#[test]
fn unbind_rolls_back_on_label_clear_failure() {
    check_name_mutation("address.unbind", true);
}
#[test]
fn rebind_clears_the_old_pane_name() {
    check_name_mutation("address.bind", false);
}
#[test]
fn rebind_rolls_back_on_label_clear_failure() {
    check_name_mutation("address.bind", true);
}

#[test]
fn rebind_restores_the_old_label_if_the_new_label_fails() {
    check_name_mutation_failure("address.bind", true, true);
}

#[test]
fn release_and_removal_clear_name_mirrors_after_store_mutation() {
    let fixture = Fixture::default();
    let state = std::env::temp_dir().join(format!("namedrop-hc-release-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
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
    for remove in [true, false] {
        let named = namedrop::execute_lifecycle_operation(
            "name",
            json!({"alias":"alice", "pane_type":"terminal"}),
            &runtime,
        )
        .unwrap();
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.name"],
            "alice"
        );
        assert_eq!(named["tab_label"]["status"], "unchanged");
        let address_id = namedrop::StateStore::open(&state)
            .unwrap()
            .transaction(|transaction| {
                Ok(transaction
                    .list_pane_addresses(&namedrop::StateNamespace::for_space(
                        "hc:fixture",
                        "fixture-space",
                    ))?
                    .into_iter()
                    .find(|address| address.alias == "alice")
                    .unwrap()
                    .id)
            })
            .unwrap();
        if remove {
            *fixture.clear_error.borrow_mut() = Some("label clear failed".into());
            assert!(
                namedrop::execute_lifecycle_operation(
                    "address.remove",
                    json!({"address_id":address_id}),
                    &runtime
                )
                .is_err()
            );
            let still_named = namedrop::StateStore::open(&state)
                .unwrap()
                .transaction(|transaction| {
                    transaction.list_pane_addresses(&namedrop::StateNamespace::for_space(
                        "hc:fixture",
                        "fixture-space",
                    ))
                })
                .unwrap();
            assert_eq!(
                still_named[0].alias, "alice",
                "failed label clear must roll back removal"
            );
            assert_eq!(
                fixture.terminals.borrow()[0]["labels"]["namedrop.name"],
                "alice"
            );
            *fixture.clear_error.borrow_mut() = None;
            namedrop::execute_lifecycle_operation(
                "address.remove",
                json!({"address_id":address_id}),
                &runtime,
            )
            .unwrap();
        } else {
            namedrop::execute_lifecycle_operation(
                "address.retire",
                json!({"address_id":address_id}),
                &runtime,
            )
            .unwrap();
            namedrop::execute_lifecycle_operation(
                "address.release-alias",
                json!({"alias":"alice"}),
                &runtime,
            )
            .unwrap();
        }
        assert_eq!(fixture.terminals.borrow()[0]["labels"]["namedrop.name"], "");
    }
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn events_reconcile_names_preserve_lost_aliases_and_clear_sessions() {
    if process::in_agent_tree("events_reconcile_names_preserve_lost_aliases_and_clear_sessions") {
        return;
    }
    let fixture = Fixture::default();
    let state = std::env::temp_dir().join(format!("namedrop-hc-events-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
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
    let mut subscriber = namedrop::host::hc::events::Subscriber::default();
    for line in include_str!("fixtures/hc/events.jsonl").lines() {
        subscriber.handle_line(line, &runtime).unwrap();
    }
    let event = |kind: &str| {
        json!({"jsonrpc":"2.0", "method":"event", "params":{"type":kind,"terminal":"t1", "data":{}}}).to_string()
    };
    fixture.terminals.borrow_mut()[0]["labels"]["namedrop.name"] = json!("bad name");
    assert!(
        subscriber
            .handle_line(&event("labels"), &runtime)
            .unwrap_err()
            .contains("alias must")
    );
    assert_eq!(
        fixture.terminals.borrow()[0]["labels"]["namedrop.name"],
        "alice"
    );
    fixture.terminals.borrow_mut()[0]["labels"]["namedrop.name"] = json!("bob");
    subscriber.handle_line(&event("labels"), &runtime).unwrap();
    let names = || {
        namedrop::StateStore::open(&state)
            .unwrap()
            .transaction(|transaction| {
                transaction.list_pane_addresses(&namedrop::StateNamespace::for_space(
                    "hc:fixture",
                    "fixture-space",
                ))
            })
            .unwrap()
    };
    assert_eq!(names()[0].alias, "bob");
    for kind in ["lost", "restarted"] {
        fixture.terminals.borrow_mut()[0]["labels"]["namedrop.session"] = json!("stale-session");
        subscriber.handle_line(&event(kind), &runtime).unwrap();
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
            ""
        );
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.name"],
            "bob"
        );
        assert_eq!(names()[0].alias, "bob");
    }
    fixture.terminals.borrow_mut()[0]["pid"] = json!(std::os::unix::process::parent_id());
    runtime
        .host
        .publish_session(&state, "t1", "fresh-session")
        .unwrap();
    for kind in ["lost", "restarted"] {
        subscriber.handle_line(&event(kind), &runtime).unwrap();
        assert_eq!(
            fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
            "fresh-session"
        );
        assert_eq!(names()[0].alias, "bob");
    }
    for key in [
        "namedrop.session",
        "namedrop.session.pid",
        "namedrop.session.start",
    ] {
        fixture.terminals.borrow_mut()[0]["labels"]
            .as_object_mut()
            .unwrap()
            .remove(key);
    }
    for kind in [
        "created",
        "status",
        "turn",
        "foreground",
        "size",
        "title",
        "doc",
        "keep",
    ] {
        subscriber.handle_line(&event(kind), &runtime).unwrap();
    }
    subscriber
        .handle_line(r#"{"method":"heartbeat","params":{"seq":9}}"#, &runtime)
        .unwrap();
    subscriber
        .handle_line(
            r#"{"method":"missed","params":{"seq":9,"terminals":[]}}"#,
            &runtime,
        )
        .unwrap();
    let address_id = names()[0].id.clone();
    namedrop::execute_lifecycle_operation(
        "address.remove",
        json!({"address_id":address_id}),
        &runtime,
    )
    .unwrap();
    subscriber.handle_line(&event("labels"), &runtime).unwrap();
    assert!(
        names().is_empty(),
        "a queued labels event must not resurrect a released name"
    );
    namedrop::execute_lifecycle_operation(
        "name",
        json!({"alias":"final", "pane_type":"terminal"}),
        &runtime,
    )
    .unwrap();
    fixture.terminals.borrow_mut()[0]["state"] = json!("finished");
    subscriber
        .handle_line(&event("finished"), &runtime)
        .unwrap();
    assert!(names().is_empty());
    fixture.terminals.borrow_mut().clear();
    subscriber.handle_line(&event("removed"), &runtime).unwrap();
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn subscriber_start_propagates_core_preflight_failure_before_launch() {
    struct StartTransport;
    impl HerdrCommandExecutor for StartTransport {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            assert_eq!(args, ["server", "status", "--json"]);
            Err(
                json!({"error":{"kind":"unavailable","message":"fixture core unavailable"}})
                    .to_string(),
            )
        }
    }
    let root = std::env::temp_dir().join(format!("hc-preflight-no-launch-{}", std::process::id()));
    let error = namedrop::host::hc::events::start_with_environment(
        &HcHost::new(StartTransport),
        &root.join("missing-binary"),
        &root,
        &namedrop::environment::EnvironmentSnapshot::from_variables([]),
    )
    .unwrap_err();
    assert!(error.message.contains("fixture core unavailable"));
    assert!(
        !root.exists(),
        "failed core preflight created subscriber state"
    );
}

#[test]
fn doctor_reports_only_observed_unreported_codex_and_uses_hc_repairs() {
    let agent = process::AgentProcess::new();
    let fixture = Fixture::default();
    let state = std::env::temp_dir().join(format!("namedrop-hc-doctor-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    std::fs::create_dir_all(&state).unwrap();
    let clock = namedrop::SystemClock;
    let ids = namedrop::ProcessIds::default();
    let runtime = namedrop::LifecycleRuntime {
        state_directory: &state,
        home_directory: Some(&state),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new(env!("CARGO_MANIFEST_DIR")),
        workspace_id: Some("fixture-space"),
        current_pane_id: Some("t1"),
        session_socket_id: "hc:fixture",
        host: HcHost::new(&fixture),
        clock: &clock,
        ids: &ids,
    };
    namedrop::StateStore::open(&state)
        .unwrap()
        .transaction(|transaction| {
            transaction.activate_worktree(
                &namedrop::StateNamespace::for_space("hc:fixture", "fixture-space"),
                &namedrop::MutationAttribution::new(namedrop::ActorKind::Human, "test", 1),
            )
        })
        .unwrap();
    fixture.terminals.borrow_mut()[0]["foreground"]["argv"] = json!(["codex"]);
    fixture.terminals.borrow_mut()[0]["foreground"]["pid"] = json!(agent.pid());
    for (reported, status, note) in [
        (false, Value::Null, true),
        (true, Value::Null, false),
        (true, json!({"app":"codex","state":"idle"}), false),
    ] {
        fixture.terminals.borrow_mut()[0]["reported"] = json!(reported);
        fixture.terminals.borrow_mut()[0]["status"] = status;
        let report = namedrop::diagnose_collaboration(&runtime);
        assert_eq!(report.host, "herdr-core");
        assert_eq!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "ND-HC-CODEX-001"),
            note
        );
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "ND-HC-SPACE-001")
        );
        let mut state_commands = 0;
        for command in report
            .issues
            .iter()
            .filter_map(|issue| issue.repair.command.as_ref())
        {
            if command.iter().any(|arg| arg == "--plugin-state") {
                state_commands += 1;
                assert!(
                    command.windows(2).any(|pair| pair == ["--host", "hc"]),
                    "{command:?}"
                );
            }
            assert!(
                !command.iter().any(|arg| arg.starts_with("--herdr-")),
                "{command:?}"
            );
        }
        assert!(
            state_commands > 0,
            "doctor must provide an executable state repair"
        );
    }
    std::fs::remove_dir_all(state).unwrap();
}

struct ConcurrentFixture {
    environment: namedrop::environment::EnvironmentSnapshot,
    record: std::sync::Mutex<Value>,
    get_count: std::sync::atomic::AtomicUsize,
    pause_get: std::sync::atomic::AtomicUsize,
    gate: std::sync::Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
}
impl ConcurrentFixture {
    fn new() -> Self {
        let mut record: Value =
            serde_json::from_str(include_str!("fixtures/hc/get-labeled.stdout")).unwrap();
        record["labels"]
            .as_object_mut()
            .unwrap()
            .remove("namedrop.session");
        Self {
            environment: namedrop::environment::EnvironmentSnapshot::from_variables([(
                "XDG_STATE_HOME".into(),
                std::env::temp_dir()
                    .join(format!("hc-prompt-race-{}", std::process::id()))
                    .into_os_string(),
            )]),
            record: std::sync::Mutex::new(record),
            get_count: std::sync::atomic::AtomicUsize::new(0),
            pause_get: std::sync::atomic::AtomicUsize::new(usize::MAX),
            gate: std::sync::Mutex::new(None),
        }
    }
    fn pause_on_get(
        &self,
        count: usize,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        *self.gate.lock().unwrap() = Some((entered_tx, resume_rx));
        self.get_count.store(0, std::sync::atomic::Ordering::SeqCst);
        self.pause_get
            .store(count, std::sync::atomic::Ordering::SeqCst);
        (entered_rx, resume_tx)
    }
}
impl HerdrCommandExecutor for ConcurrentFixture {
    fn environment_snapshot(&self) -> Option<&namedrop::environment::EnvironmentSnapshot> {
        Some(&self.environment)
    }
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let stdout = match args {
            ["ls", "--json"] => json!({"terminals":[*self.record.lock().unwrap()]}).to_string(),
            ["get", _, "--json"] => {
                let snapshot = self.record.lock().unwrap().to_string();
                if self
                    .get_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1
                    == self.pause_get.load(std::sync::atomic::Ordering::SeqCst)
                {
                    let (entered, resume) = self.gate.lock().unwrap().take().unwrap();
                    entered.send(()).unwrap();
                    resume
                        .recv_timeout(std::time::Duration::from_secs(5))
                        .unwrap();
                }
                snapshot
            }
            ["label", _, labels @ ..] => {
                let mut record = self.record.lock().unwrap();
                for label in labels.iter().filter(|label| **label != "--json") {
                    let (key, value) = label.split_once('=').unwrap();
                    if value.is_empty() {
                        record["labels"].as_object_mut().unwrap().remove(key);
                    } else {
                        record["labels"][key] = json!(value);
                    }
                }
                record.to_string()
            }
            args => panic!("unexpected hc call {args:?}"),
        };
        Ok(HerdrCommandOutput { stdout })
    }
}

#[test]
fn delayed_invalidations_and_interleaved_publication_preserve_the_new_session() {
    if process::in_agent_tree(
        "delayed_invalidations_and_interleaved_publication_preserve_the_new_session",
    ) {
        return;
    }
    let fixture = ConcurrentFixture::new();
    let state =
        std::env::temp_dir().join(format!("namedrop-hc-session-race-{}", std::process::id()));
    let host = HcHost::new(&fixture);
    fixture.record.lock().unwrap()["pid"] = json!(std::os::unix::process::parent_id());
    host.publish_session(&state, "t1", "old").unwrap();
    fixture.record.lock().unwrap()["pid"] = json!(std::os::unix::process::parent_id());
    host.publish_session(&state, "t1", "new").unwrap();
    host.invalidate_session(&state, "t1").unwrap();
    assert_eq!(
        fixture.record.lock().unwrap()["labels"]["namedrop.session"],
        "new",
        "a delayed event must retain a session published for the current PID"
    );

    fixture.record.lock().unwrap()["labels"]["namedrop.session.pid"] = json!("4000000000");
    let (entered, resume) = fixture.pause_on_get(1);
    std::thread::scope(|scope| {
        let invalidation = scope.spawn(|| host.invalidate_session(&state, "t1"));
        entered
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        fixture.record.lock().unwrap()["pid"] = json!(std::os::unix::process::parent_id());
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let state = &state;
        let publication = scope.spawn(move || {
            let result = host.publish_session(state, "t1", "newest");
            done_tx.send(()).unwrap();
            result
        });
        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_millis(75))
                .is_err(),
            "publication must serialize with the whole invalidation decision"
        );
        resume.send(()).unwrap();
        invalidation.join().unwrap().unwrap();
        publication.join().unwrap().unwrap();
    });
    assert_eq!(
        fixture.record.lock().unwrap()["labels"]["namedrop.session"],
        "newest"
    );
    assert_eq!(
        fixture.record.lock().unwrap()["labels"]["namedrop.session.pid"],
        std::os::unix::process::parent_id().to_string()
    );
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn removal_serializes_with_a_name_request_already_being_read() {
    let fixture = ConcurrentFixture::new();
    let state = std::env::temp_dir().join(format!("namedrop-hc-name-race-{}", std::process::id()));
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
        json!({"alias":"alice","pane_type":"terminal"}),
        &runtime,
    )
    .unwrap();
    let namespace = namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
    let address = namedrop::StateStore::open(&state)
        .unwrap()
        .transaction(|transaction| transaction.list_pane_addresses(&namespace))
        .unwrap()
        .remove(0);
    let (entered, resume) = fixture.pause_on_get(2);
    std::thread::scope(|scope| {
        let event = scope.spawn(|| {
            namedrop::host::hc::events::Subscriber::default().handle_line(
                r#"{"method":"event","params":{"type":"labels","terminal":"t1","data":{}}}"#,
                &runtime,
            )
        });
        entered
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let runtime = &runtime;
        let removal = scope.spawn(move || {
            let result = namedrop::execute_lifecycle_operation(
                "address.remove",
                json!({"address_id":address.id}),
                runtime,
            );
            done_tx.send(()).unwrap();
            result
        });
        assert!(
            done_rx
                .recv_timeout(std::time::Duration::from_millis(75))
                .is_err(),
            "removal must wait for naming's read and decision"
        );
        resume.send(()).unwrap();
        event.join().unwrap().unwrap();
        removal.join().unwrap().unwrap();
    });
    assert!(
        namedrop::StateStore::open(&state)
            .unwrap()
            .transaction(|transaction| transaction.list_pane_addresses(&namespace))
            .unwrap()
            .is_empty()
    );
    assert!(fixture.record.lock().unwrap()["labels"]["namedrop.name"].is_null());
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn session_generation_is_checked_by_discovery_and_caller_identity() {
    let fixture = Fixture::default();
    let host = HcHost::new(&fixture);
    let claim = CallerClaim {
        workspace: None,
        pane: None,
        inherited_pane: Some("stale-terminal".into()),
        agent_sessions: vec!["fixture-session".into()],
    };
    let agent = process::AgentProcess::new();
    let owner = agent.pid().to_string();
    fixture.terminals.borrow_mut()[0]["labels"]["namedrop.session.start"] =
        json!(process::start(agent.pid()));
    for (companion, valid) in [
        (None, false),
        (Some("4000000000"), false),
        (Some(owner.as_str()), true),
    ] {
        let mut terminals = fixture.terminals.borrow_mut();
        terminals[0]["pid"] = json!(456);
        if let Some(pid) = companion {
            terminals[0]["labels"]["namedrop.session.pid"] = json!(pid);
        } else {
            terminals[0]["labels"]
                .as_object_mut()
                .unwrap()
                .remove("namedrop.session.pid");
        }
        drop(terminals);
        assert_eq!(
            host.panes(PaneScope::All).unwrap()[0]
                .agent_session_id
                .is_some(),
            valid,
            "{companion:?}"
        );
        assert_eq!(host.caller_identity(&claim).is_ok(), valid, "{companion:?}");
    }
}

#[test]
fn snapshots_and_missed_events_clear_stale_sessions_before_naming() {
    if process::in_agent_tree("snapshots_and_missed_events_clear_stale_sessions_before_naming") {
        return;
    }
    let fixture = Fixture::default();
    let state = std::env::temp_dir().join(format!("namedrop-hc-resnapshot-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    let clock = namedrop::SystemClock;
    let ids = namedrop::ProcessIds::default();
    let runtime = namedrop::LifecycleRuntime {
        state_directory: &state,
        home_directory: Some(&state),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new(env!("CARGO_MANIFEST_DIR")),
        workspace_id: Some("fixture-space"),
        current_pane_id: Some("t1"),
        session_socket_id: "hc:fixture",
        host: HcHost::new(&fixture),
        clock: &clock,
        ids: &ids,
    };
    let namespace = namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
    namedrop::StateStore::open(&state)
        .unwrap()
        .transaction(|transaction| {
            transaction.activate_worktree(
                &namespace,
                &namedrop::MutationAttribution::new(namedrop::ActorKind::Human, "test", 1),
            )
        })
        .unwrap();
    fixture.terminals.borrow_mut()[0]["foreground"]["argv"] = json!(["codex"]);
    fixture.terminals.borrow_mut()[0]["foreground"]["pid"] =
        json!(std::os::unix::process::parent_id());
    let mut subscriber = namedrop::host::hc::events::Subscriber::default();
    for snapshot in [true, false] {
        for (session, companion) in [
            ("stale", Some("123")),
            ("fresh", Some("456")),
            ("legacy", None),
        ] {
            {
                let mut terminals = fixture.terminals.borrow_mut();
                terminals[0]["pid"] = json!(std::os::unix::process::parent_id());
                terminals[0]["labels"]["namedrop.session"] = json!(session);
                terminals[0]["labels"]["namedrop.name"] =
                    json!(format!("name-{snapshot}-{session}"));
                if let Some(pid) = companion {
                    terminals[0]["labels"]["namedrop.session.pid"] = json!(pid);
                } else {
                    terminals[0]["labels"]
                        .as_object_mut()
                        .unwrap()
                        .remove("namedrop.session.pid");
                }
            }
            if session == "fresh" {
                let mut terminals = fixture.terminals.borrow_mut();
                terminals[0]["labels"]["namedrop.session"] = json!("old");
                terminals[0]["labels"]["namedrop.session.pid"] = json!("123");
            }
            let line = if snapshot {
                json!({"terminals": *fixture.terminals.borrow()}).to_string()
            } else {
                json!({"method":"missed", "params":{"seq":99}}).to_string()
            };
            if session == "fresh" {
                runtime.host.publish_session(&state, "t1", session).unwrap();
            }
            subscriber.handle_line(&line, &runtime).unwrap();
            let expected = if session != "fresh" {
                None
            } else {
                Some(session)
            };
            assert_eq!(
                fixture.terminals.borrow()[0]["labels"]["namedrop.session"]
                    .as_str()
                    .filter(|s| !s.is_empty()),
                if session == "legacy" {
                    Some(session)
                } else {
                    expected
                },
                "snapshot={snapshot}, session={session}"
            );
            let addresses = namedrop::StateStore::open(&state)
                .unwrap()
                .transaction(|transaction| transaction.list_pane_addresses(&namespace))
                .unwrap();
            let address = addresses
                .iter()
                .find(|address| address.alias == format!("name-{snapshot}-{session}"))
                .unwrap();
            assert_eq!(
                address.agent_session_id.as_ref().map(|id| id.as_str()),
                expected
            );
        }
    }
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn removing_or_releasing_a_gone_terminal_succeeds_but_other_clear_errors_roll_back() {
    for remove in [true, false] {
        for kind in ["gone", "not_found", "finished", "unavailable", "transport"] {
            let fixture = Fixture::default();
            let state = std::env::temp_dir().join(format!(
                "namedrop-hc-absent-{}-{remove}-{kind}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&state);
            let clock = namedrop::SystemClock;
            let ids = namedrop::ProcessIds::default();
            let runtime = namedrop::LifecycleRuntime {
                state_directory: &state,
                home_directory: Some(&state),
                agent_config: namedrop::AgentConfigDirectories::default(),
                cwd: Path::new(env!("CARGO_MANIFEST_DIR")),
                workspace_id: Some("fixture-space"),
                current_pane_id: Some("t1"),
                session_socket_id: "hc:fixture",
                host: HcHost::new(&fixture),
                clock: &clock,
                ids: &ids,
            };
            let namespace = namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
            namedrop::execute_lifecycle_operation(
                "name",
                json!({"alias":"alice", "pane_type":"terminal"}),
                &runtime,
            )
            .unwrap();
            let addresses = || {
                namedrop::StateStore::open(&state)
                    .unwrap()
                    .transaction(|transaction| transaction.list_pane_addresses(&namespace))
                    .unwrap()
            };
            let address_id = addresses()[0].id.clone();
            if !remove {
                namedrop::execute_lifecycle_operation(
                    "address.retire",
                    json!({"address_id":address_id}),
                    &runtime,
                )
                .unwrap();
                fixture.terminals.borrow_mut()[0]["labels"]["namedrop.name"] = json!("alice");
            }
            if kind == "gone" {
                fixture.terminals.borrow_mut().clear();
            } else {
                *fixture.clear_error.borrow_mut() = Some(if kind == "transport" {
                    "connection refused".into()
                } else {
                    json!({"error":{"kind":kind,"message":"clear rejected","hint":""}}).to_string()
                });
            }
            let before = addresses();
            let (operation, input) = if remove {
                ("address.remove", json!({"address_id":address_id}))
            } else {
                ("address.release-alias", json!({"alias":"alice"}))
            };
            let result = namedrop::execute_lifecycle_operation(operation, input, &runtime);
            let absent = matches!(kind, "gone" | "not_found" | "finished");
            assert_eq!(result.is_ok(), absent, "{operation} {kind}: {result:?}");
            if absent {
                assert!(addresses().iter().all(|address| address.alias != "alice"));
            } else {
                assert_eq!(addresses(), before, "{operation} {kind} must roll back");
            }
            std::fs::remove_dir_all(state).unwrap();
        }
    }
}

#[test]
fn routine_events_preserve_another_dispatchers_live_lease() {
    for event in ["created", "restarted", "snapshot", "missed"] {
        check_dispatch_lease(event, false);
    }
}

#[test]
fn subscriber_holds_abandoned_dispatch_lease_at_deadline() {
    check_dispatch_lease("snapshot", true);
}

#[test]
fn explicit_reconcile_holds_live_dispatch_lease() {
    check_dispatch_lease("reconcile", false);
}

fn check_dispatch_lease(event: &str, expires: bool) {
    use namedrop::{
        ActorKind, AgentSessionId, Clock, DeliveryAttemptId, DeliveryId, DeliveryState, LeaseDraft,
        MessageDraft, MessageId, MessageRecipientDraft, MutationAttribution, StateStore, ThreadId,
    };
    let fixture = Fixture::default();
    let agent = process::AgentProcess::new();
    fixture.agent(agent.pid());
    {
        let mut terminals = fixture.terminals.borrow_mut();
        terminals[0]["foreground"]["argv"] = json!(["codex"]);
        terminals[0]["status"] = json!({"state":"done", "app":"claude-code"});
        let mut other = terminals[0].clone();
        other["address"] = json!("t2");
        other["labels"] = json!({"herdr.space":"fixture-space"});
        terminals.push(other);
    }
    let state = std::env::temp_dir().join(format!(
        "namedrop-hc-live-lease-{}-{event}-{expires}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&state);
    struct TestClock(std::cell::Cell<u64>);
    impl Clock for TestClock {
        fn now_unix_ms(&self) -> u64 {
            self.0.get()
        }
    }
    let clock = TestClock(std::cell::Cell::new(1_000));
    let ids = namedrop::ProcessIds::default();
    let runtime = namedrop::LifecycleRuntime {
        state_directory: &state,
        home_directory: Some(&state),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new(env!("CARGO_MANIFEST_DIR")),
        workspace_id: Some("fixture-space"),
        current_pane_id: Some("t1"),
        session_socket_id: "hc:fixture",
        host: HcHost::new(&fixture),
        clock: &clock,
        ids: &ids,
    };
    namedrop::execute_lifecycle_operation("name", json!({"alias":"alice"}), &runtime).unwrap();
    let namespace = namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
    let mut store = StateStore::open(&state).unwrap();
    let now = clock.now_unix_ms();
    let attribution = MutationAttribution::new(ActorKind::Plugin, "test-dispatcher", now);
    let attempt_id = DeliveryAttemptId::new("active-dispatch");
    store
        .transaction(|tx| {
            let binding = tx
                .active_pane_bindings(&namespace)?
                .into_iter()
                .find(|binding| binding.pane_id == "t1")
                .unwrap();
            let session = AgentSessionId::new("fixture-session");
            let thread = ThreadId::new("test-thread");
            tx.create_thread(&namespace, &thread, "test", &attribution)?;
            tx.append_message(
                &namespace,
                &MessageDraft {
                    thread_id: thread,
                    message_id: MessageId::new("test-message"),
                    in_reply_to: None,
                    author_address_id: binding.address_id.clone(),
                    author_kind: ActorKind::Human,
                    author_alias_snapshot: "alice".into(),
                    author_agent_session_snapshot: None,
                    request_id: "test-request".into(),
                    authored_source: "@alice test".into(),
                    body: "test".into(),
                    grammar_version: "mention/v1".into(),
                    recipients: vec![MessageRecipientDraft {
                        address_id: binding.address_id.clone(),
                        alias_snapshot: "alice".into(),
                        agent_session_snapshot: Some(session.clone()),
                        delivery_id: DeliveryId::new("test-delivery"),
                    }],
                    context_references: Vec::new(),
                },
                &attribution,
            )?;
            assert!(
                tx.lease_mailbox_head(
                    &namespace,
                    &binding.address_id,
                    &LeaseDraft {
                        attempt_id: attempt_id.clone(),
                        binding_id: binding.id.clone(),
                        agent_session_id: session,
                        retry_of_attempt_id: None,
                        owner: "other-process".into(),
                        acquired_at_ms: now,
                        expires_at_ms: now + 30_000,
                    },
                    &attribution
                )?
                .is_some()
            );
            Ok(())
        })
        .unwrap();
    if event == "reconcile" {
        namedrop::execute_lifecycle_operation("reconcile", json!({}), &runtime).unwrap();
        assert_eq!(
            store
                .transaction(|tx| tx.delivery_attempt_state(&namespace, &attempt_id))
                .unwrap(),
            DeliveryState::Held
        );
        std::fs::remove_dir_all(state).unwrap();
        return;
    }
    let report = namedrop::drain_mailboxes(&runtime).unwrap();
    assert!(report.pending_grace.is_empty());
    assert_eq!(report.next_lease_due_ms, Some(now + 30_000));
    let line = match event {
        "snapshot" => json!({"terminals": *fixture.terminals.borrow()}),
        "missed" => json!({"method":"missed", "params":{"seq":99}}),
        _ => json!({"method":"event", "params":{"type":event, "terminal":"t2"}}),
    }
    .to_string();
    let mut subscriber = namedrop::host::hc::events::Subscriber::default();
    subscriber.handle_line(&line, &runtime).unwrap();
    assert_eq!(
        store
            .transaction(|tx| tx.delivery_attempt_state(&namespace, &attempt_id))
            .unwrap(),
        DeliveryState::Dispatching,
        "{event}"
    );
    clock.0.set(now + 29_999);
    subscriber.tick(&runtime).unwrap();
    assert_eq!(
        store
            .transaction(|tx| tx.delivery_attempt_state(&namespace, &attempt_id))
            .unwrap(),
        DeliveryState::Dispatching,
        "before lease expiry"
    );
    if expires {
        clock.0.set(now + 30_000);
        subscriber.tick(&runtime).unwrap();
        assert_eq!(
            store
                .transaction(|tx| tx.delivery_attempt_state(&namespace, &attempt_id))
                .unwrap(),
            DeliveryState::Held,
            "at lease expiry"
        );
        assert!(
            !fixture
                .take_calls()
                .iter()
                .any(|args| args.first().is_some_and(|arg| arg == "send"))
        );
        std::fs::remove_dir_all(state).unwrap();
        return;
    }
    let attribution =
        MutationAttribution::new(ActorKind::Plugin, "test-dispatcher", clock.now_unix_ms());
    store
        .transaction(|tx| {
            assert_eq!(
                tx.delivery_attempt_state(&namespace, &attempt_id)?,
                DeliveryState::Dispatching,
                "{event}"
            );
            tx.transition_delivery_attempt(
                &namespace,
                &attempt_id,
                DeliveryState::Submitted,
                &attribution,
            )?;
            tx.acknowledge_delivery_attempt(
                &namespace,
                &attempt_id,
                "test-receipt",
                &AgentSessionId::new("fixture-session"),
                &attribution,
            )
        })
        .unwrap();
    assert_eq!(
        store
            .transaction(|tx| tx.delivery_attempt_state(&namespace, &attempt_id))
            .unwrap(),
        DeliveryState::Acknowledged
    );
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn lost_terminal_send_is_pre_invocation() {
    let fixture = Fixture {
        send_error: RefCell::new(Some(
            include_str!("fixtures/hc/lost-send.stderr").to_owned(),
        )),
        ..Fixture::default()
    };
    for result in [
        HcHost::new(&fixture).prompt("t2", "hello"),
        HcHost::new(&fixture).run_in_pane("t2", "echo hello"),
    ] {
        assert_eq!(
            result.unwrap_err().kind,
            PromptFailureKind::TemporaryPreInvocation
        );
    }
}

#[test]
fn screen_reads_return_only_the_requested_tail() {
    for (text, lines, expected) in [
        ("first\nsecond\nthird", 2, "second\nthird"),
        ("first\nsecond\nthird\n", 2, "second\nthird\n"),
        ("first\nsecond\nthird\n", 0, ""),
        ("first\nsecond\nthird", 0, ""),
        ("first\n\n", 1, "\n"),
        ("first\n", 5, "first\n"),
        ("", 5, ""),
    ] {
        let fixture = Fixture {
            read_text: Some(text.to_owned()),
            ..Fixture::default()
        };
        assert_eq!(
            HcHost::new(&fixture).read("t1", lines, None).unwrap(),
            expected,
            "lines={lines} text={text:?}"
        );
    }
}

#[test]
fn foreground_programs_are_agents_only_when_recognized() {
    for (program, app, expected) in [
        ("vim", None, None),
        ("npm", None, None),
        ("python", None, None),
        ("bash", None, None),
        ("/bin/codex", None, Some("codex")),
        ("/bin/claude", None, Some("claude")),
        ("claude-code", None, Some("claude")),
        ("vim", Some("codex"), None),
        ("python", Some("custom-agent"), None),
        ("namedrop", Some("custom-agent"), None),
    ] {
        let process = process::AgentProcess::with_program(program);
        for (reported, argv) in [(false, true), (true, true), (false, false)] {
            let fixture = Fixture::default();
            {
                let mut terminals = fixture.terminals.borrow_mut();
                terminals[0]["foreground"]["argv"] = if argv {
                    json!([program, "terminal-execute"])
                } else {
                    json!([])
                };
                terminals[0]["foreground"]["name"] = json!(program);
                terminals[0]["foreground"]["pid"] = json!(process.pid());
                terminals[0]["status"] =
                    app.map_or(Value::Null, |app| json!({"state":"idle", "app":app}));
                terminals[0]["reported"] = json!(reported);
            }
            let pane = HcHost::new(&fixture)
                .panes(PaneScope::All)
                .unwrap()
                .remove(0);
            assert_eq!(
                pane.agent.as_deref(),
                expected,
                "program={program} app={app:?} reported={reported}"
            );
        }
    }
}

#[test]
fn hc_process_info_reports_the_group_leader_without_enumerating_members() {
    let fixture = Fixture::default();
    let group = unsafe { libc::getpgid(0) };
    fixture.terminals.borrow_mut()[0]["foreground"]["pid"] = json!(group);
    let mut member = std::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let info = HcHost::new(&fixture).process_info("t1");
    drop(member.stdin.take());
    let status = member.wait().unwrap();
    assert!(status.success());
    let info = info.unwrap();
    assert_eq!(info.foreground_process_group_id, group as u64);
    assert_eq!(
        info.foreground_processes
            .iter()
            .map(|process| process.pid)
            .collect::<Vec<_>>(),
        vec![group as u32]
    );
}

#[test]
fn hc_naming_recognizes_its_process_group_without_pid_membership() {
    const TEST_NAME: &str = "hc_naming_recognizes_its_process_group_without_pid_membership";
    if std::env::var("NAMEDROP_NAMING_TEST_CHILD").as_deref() != Ok(TEST_NAME) {
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST_NAME, "--nocapture"])
            .env("NAMEDROP_NAMING_TEST_CHILD", TEST_NAME)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let fixture = Fixture::default();
    let group = unsafe { libc::getpgid(0) };
    assert_ne!(group as u32, std::process::id());
    {
        let mut terminals = fixture.terminals.borrow_mut();
        terminals[0]["address"] = json!("t1");
        terminals[0]["labels"]
            .as_object_mut()
            .unwrap()
            .remove("namedrop.session");
        assert_ne!(terminals[0]["pid"], json!(group));
        terminals[0]["foreground"]["pid"] = json!(group);
        terminals[0]["foreground"]["argv"] = json!(["namedrop", "terminal-execute"]);
    }
    let clock = namedrop::SystemClock;
    let ids = namedrop::ProcessIds::default();
    let runtime = namedrop::LifecycleRuntime {
        state_directory: Path::new("/unused"),
        home_directory: None,
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new(env!("CARGO_MANIFEST_DIR")),
        workspace_id: Some("fixture-space"),
        current_pane_id: Some("t1"),
        session_socket_id: "hc:fixture",
        host: HcHost::new(&fixture),
        clock: &clock,
        ids: &ids,
    };
    assert_eq!(
        namedrop::detect_pane_type(&runtime).unwrap(),
        Some(namedrop::PaneType::Terminal)
    );
}

#[test]
fn screen_and_history_reads_share_the_remaining_budget() {
    use std::{cell::Cell, sync::atomic::AtomicBool, time::Duration};
    struct ReadBudget {
        remaining: Cell<u64>,
        cancelled: AtomicBool,
    }
    impl namedrop::Budget for ReadBudget {
        fn remaining(&self) -> Result<Duration, String> {
            let remaining = self.remaining.get();
            self.remaining.set(remaining - 10);
            Ok(Duration::from_millis(remaining))
        }
        fn cancelled(&self) -> &AtomicBool {
            &self.cancelled
        }
        fn max_output_bytes(&self) -> usize {
            4096
        }
    }
    struct Bounded<'a> {
        fixture: &'a Fixture,
        budget: &'a ReadBudget,
        calls: Cell<u64>,
    }
    impl HerdrCommandExecutor for Bounded<'_> {
        fn execute(&self, _: &[&str]) -> Result<HerdrCommandOutput, String> {
            panic!("read lost its budget")
        }
        fn execute_bounded(
            &self,
            args: &[&str],
            timeout: Duration,
            cancelled: &AtomicBool,
            limit: usize,
        ) -> Result<HerdrCommandOutput, String> {
            let call = self.calls.get();
            self.calls.set(call + 1);
            assert_eq!(timeout, Duration::from_millis(100 - call * 10));
            assert!(std::ptr::eq(cancelled, &self.budget.cancelled));
            assert_eq!(limit, 4096);
            self.fixture.execute(args)
        }
    }
    for lines in [2, 130] {
        let fixture = Fixture {
            read_text: Some("one\ntwo\nthree\n".into()),
            ..Fixture::default()
        };
        let budget = ReadBudget {
            remaining: Cell::new(100),
            cancelled: AtomicBool::new(false),
        };
        let executor = Bounded {
            fixture: &fixture,
            budget: &budget,
            calls: Cell::new(0),
        };
        let result = HcHost::new(&executor)
            .read("t1", lines, Some(&budget))
            .unwrap();
        assert_eq!(
            result,
            if lines == 2 {
                "two\nthree\n"
            } else {
                "one\ntwo\nthree\n"
            }
        );
        let calls = fixture.take_calls();
        assert_eq!(calls[0], ["get", "t1", "--json"]);
        assert_eq!(
            calls[1],
            if lines == 2 {
                vec!["read", "t1", "--json"]
            } else {
                vec!["read", "t1", "--lines", "130", "--json"]
            }
        );
    }
}

#[test]
fn hc_bootstrap_first_name_installs_the_shared_command() {
    check_hc_bootstrap(None);
}

#[test]
fn hc_bootstrap_first_name_refuses_unowned_command_before_registration_challenges() {
    check_hc_bootstrap(Some("unowned-first-name"));
}

#[test]
fn hc_bootstrap_doctor_detects_deleted_command() {
    check_hc_bootstrap(Some("deleted"));
}

#[test]
fn hc_bootstrap_doctor_detects_corrupt_command() {
    check_hc_bootstrap(Some("corrupt"));
}

#[test]
fn hc_bootstrap_doctor_detects_nonexecutable_command() {
    check_hc_bootstrap(Some("nonexecutable"));
}

#[test]
fn hc_bootstrap_doctor_detects_unowned_command() {
    check_hc_bootstrap(Some("unowned"));
}

#[test]
fn hc_bootstrap_doctor_detects_symlink_command() {
    check_hc_bootstrap(Some("symlink"));
}

#[test]
fn hc_bootstrap_doctor_detects_symlink_parent() {
    check_hc_bootstrap(Some("symlink-parent"));
}

#[test]
fn hc_bootstrap_doctor_detects_missing_fallback() {
    check_hc_bootstrap(Some("missing-fallback"));
}

#[test]
fn hc_bootstrap_stale_activation_does_not_replace_the_current_fallback() {
    const CHILD: &str = "NAMEDROP_BOOTSTRAP_STALE_ACTIVATION";
    if std::env::var_os(CHILD).is_some() {
        check_hc_bootstrap(Some("stale-activation"));
        return;
    }
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/bootstrap-checks")
        .join(format!("stale-activation-runtime-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let old = root.join("releases").join("a".repeat(64));
    let current = root.join("releases").join("b".repeat(64));
    std::fs::create_dir_all(&old).unwrap();
    std::fs::create_dir_all(&current).unwrap();
    std::fs::hard_link(std::env::current_exe().unwrap(), old.join("namedrop")).unwrap();
    std::fs::write(old.join("runtime.json"), include_str!("../runtime.json")).unwrap();
    std::fs::write(current.join("namedrop"), "active runtime").unwrap();
    std::fs::write(root.join("namedrop"), "stable launcher").unwrap();
    std::os::unix::fs::symlink(
        Path::new("releases").join("b".repeat(64)),
        root.join("current"),
    )
    .unwrap();
    let fake = app_popup::fake(&root);
    let output = std::process::Command::new(old.join("namedrop"))
        .args([
            "--exact",
            "hc_bootstrap_stale_activation_does_not_replace_the_current_fallback",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .env("NAMEDROP_BOOTSTRAP_CASE", "stale-activation")
        .env("HERDR_APP_BIN_PATH", fake)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    app_popup::assert_one_test(&output);
    app_popup::assert_tour(&root, 1);
    std::fs::remove_dir_all(root).unwrap();
}

fn check_hc_bootstrap(damage: Option<&str>) {
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };
    let case = damage.unwrap_or("fresh-name");
    if std::env::var("NAMEDROP_BOOTSTRAP_CASE").as_deref() != Ok(case) {
        let root =
            std::env::temp_dir().join(format!("namedrop-popup-{case}-{}", std::process::id()));
        let fake = app_popup::fake(&root);
        let thread = std::thread::current();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", thread.name().unwrap(), "--nocapture"])
            .env("NAMEDROP_BOOTSTRAP_CASE", case)
            .env("HERDR_APP_BIN_PATH", fake)
            .output()
            .unwrap();
        app_popup::assert_one_test(&output);
        app_popup::assert_tour(
            &root,
            usize::from(damage.is_some() && damage != Some("unowned-first-name")),
        );
        fs::remove_dir_all(root).unwrap();
        return;
    }
    let claude = process::AgentProcess::with_program("claude");
    let fixture = Fixture::default();
    fixture.terminals.borrow_mut()[0]["foreground"]["argv"] = json!(["claude"]);
    fixture.terminals.borrow_mut()[0]["foreground"]["pid"] = json!(claude.pid());
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/bootstrap-checks")
        .join(format!(
            "{}-{}",
            damage.unwrap_or("fresh-name"),
            std::process::id()
        ));
    let _ = fs::remove_dir_all(&root);
    let home = root.join("home");
    let state = root.join("state");
    fs::create_dir_all(&home).unwrap();
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
    let command = home.join(".local/bin/namedrop-app");
    if damage == Some("unowned-first-name") {
        fs::create_dir_all(command.parent().unwrap()).unwrap();
        let unowned = "#!/bin/sh\nexit 41\n";
        fs::write(&command, unowned).unwrap();
        fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).unwrap();
        let result = namedrop::execute_lifecycle_operation(
            "name",
            json!({"alias":"alice","pane_type":"agent"}),
            &runtime,
        )
        .unwrap();
        for agent in ["codex", "claude-code"] {
            assert_eq!(
                result["integration_packages"][agent]["installed"], false,
                "{result}"
            );
        }
        assert_eq!(result["ready"], false);
        assert!(
            !fixture
                .take_calls()
                .iter()
                .any(|args| args.first().is_some_and(|arg| arg == "send")),
            "unowned bootstrap must block registration challenges"
        );
        assert_eq!(fs::read_to_string(&command).unwrap(), unowned);
    } else if damage == Some("stale-activation") {
        fs::create_dir_all(command.parent().unwrap()).unwrap();
        let script = "#!/bin/sh\n# Managed by Namedrop: shell command.\nexec '/bin/sh' \"$@\"\n";
        fs::write(&command, script).unwrap();
        fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).unwrap();
        namedrop::execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime)
            .unwrap();
        assert_eq!(
            fs::read_to_string(&command).unwrap(),
            script,
            "stale activation overwrote the current bootstrap"
        );
    } else if let Some(damage) = damage {
        namedrop::execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime)
            .unwrap();
        let baseline = namedrop::diagnose_collaboration(&runtime);
        assert!(
            !baseline
                .issues
                .iter()
                .any(|issue| issue.code == "ND-INTEGRATION-001"),
            "{baseline:?}"
        );
        let original = fs::read(&command).unwrap();
        match damage {
            "deleted" => fs::remove_file(&command).unwrap(),
            "missing-fallback" => fs::write(
                &command,
                format!(
                    "#!/bin/sh\n# Managed by Namedrop: shell command.\nexec '{}' \"$@\"\n",
                    root.join("missing-executable").display()
                ),
            )
            .unwrap(),
            "corrupt" => fs::write(
                &command,
                "#!/bin/sh\n# Managed by Namedrop: shell command.\nexit 41\n",
            )
            .unwrap(),
            "nonexecutable" => {
                fs::set_permissions(&command, fs::Permissions::from_mode(0o644)).unwrap()
            }
            "unowned" => fs::write(&command, "#!/bin/sh\nexit 41\n").unwrap(),
            "symlink" => {
                fs::rename(&command, root.join("outside-command")).unwrap();
                symlink(root.join("outside-command"), &command).unwrap();
            }
            "symlink-parent" => {
                fs::rename(home.join(".local/bin"), root.join("outside-bin")).unwrap();
                symlink(root.join("outside-bin"), home.join(".local/bin")).unwrap();
            }
            _ => unreachable!(),
        }
        let before = fs::read(&command).ok();
        let mode = fs::symlink_metadata(&command)
            .ok()
            .map(|m| m.permissions().mode());
        let report = namedrop::diagnose_collaboration(&runtime);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "ND-INTEGRATION-001"),
            "doctor accepted {damage} required bootstrap: {report:?}"
        );
        assert_eq!(
            fs::read(&command).ok(),
            before,
            "doctor must not repair the command"
        );
        assert_eq!(
            fs::symlink_metadata(&command)
                .ok()
                .map(|m| m.permissions().mode()),
            mode
        );
        let result = namedrop::execute_lifecycle_operation(
            "name",
            json!({"alias":"alice","pane_type":"agent"}),
            &runtime,
        )
        .unwrap();
        if matches!(damage, "unowned" | "symlink" | "symlink-parent") {
            for agent in ["codex", "claude-code"] {
                assert_eq!(
                    result["integration_packages"][agent]["installed"], false,
                    "{result}"
                );
            }
            assert_eq!(result["ready"], false);
            let attention = namedrop::StateStore::open(&state)
                .unwrap()
                .transaction(|tx| {
                    tx.unresolved_attention_items(&namedrop::StateNamespace::for_space(
                        "hc:fixture",
                        "fixture-space",
                    ))
                })
                .unwrap();
            assert_eq!(
                attention
                    .iter()
                    .filter(|item| matches!(
                        item.source_id.as_str(),
                        "integration-package-codex" | "integration-package-claude-code"
                    ))
                    .count(),
                2
            );
            assert_eq!(fs::read(&command).ok(), before);
        } else {
            assert_eq!(
                fs::read(&command).unwrap(),
                original,
                "common refresh repairs owned bootstrap"
            );
            assert_eq!(
                fs::metadata(&command).unwrap().permissions().mode() & 0o777,
                0o755
            );
            assert!(
                !namedrop::diagnose_collaboration(&runtime)
                    .issues
                    .iter()
                    .any(|issue| issue.code == "ND-INTEGRATION-001")
            );
        }
    } else {
        let result = namedrop::execute_lifecycle_operation(
            "name",
            json!({"alias":"alice","pane_type":"agent"}),
            &runtime,
        )
        .unwrap();
        assert_eq!(result["activation_was_needed"], true);
        assert!(
            command.is_file(),
            "first HC name must install its required bootstrap"
        );
        assert_ne!(
            fs::metadata(&command).unwrap().permissions().mode() & 0o111,
            0
        );
        assert!(!home.join(".local/bin/namedrop").exists());
        assert!(!home.join(".bashrc").exists());
        assert!(!home.join(".profile").exists());
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn hc_bootstrap_popups_use_only_the_test_app() {
    app_popup::guard(
        &[
            "hc_bootstrap_doctor_detects_corrupt_command",
            "hc_bootstrap_doctor_detects_deleted_command",
            "hc_bootstrap_doctor_detects_missing_fallback",
            "hc_bootstrap_doctor_detects_nonexecutable_command",
            "hc_bootstrap_doctor_detects_symlink_command",
            "hc_bootstrap_doctor_detects_symlink_parent",
            "hc_bootstrap_doctor_detects_unowned_command",
            "hc_bootstrap_stale_activation_does_not_replace_the_current_fallback",
            "refused_hc_delivery_waits_for_new_target_settle_without_failure",
            "settle_before_refusal_recording_resumes_delivery",
            "inactive_namespace_preserves_settle_for_reactivation",
            "expired_deferred_delivery_can_be_cancelled_with_confirmation",
            "cancelled_attempt_requires_exact_unused_deferral_provenance",
            "onboarding_and_doctor_refusals_wait_for_new_settle",
            "no_input_refusal_resets_speculatively_invoked_challenge",
            #[cfg(target_os = "linux")]
            "prompt_race::settle_projection_waits_for_each_prompt_callers_no_input_commit",
        ],
        "bootstrap",
    );
}

#[test]
fn hc_popup_uses_native_override_and_scrubs_environment() {
    use std::os::unix::ffi::OsStringExt;
    let root = std::env::temp_dir().join(format!("namedrop-popup-native-{}", std::process::id()));
    let fake = app_popup::fake(&root);
    let native_path = root.join(std::ffi::OsString::from_vec(
        b"app with spaces-\xff".to_vec(),
    ));
    std::fs::rename(fake, &native_path).unwrap();
    let path = app_popup::tripwire(&root);
    let socket = root.join("core.sock");
    let _core = hc_socket::HcSocket::bind(&socket).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "hc_popup_child", "--nocapture"])
        .env_clear()
        .env("PATH", path)
        .env("HERDR_APP_BIN_PATH", native_path)
        .env("HERDR_SOCKET_PATH", "inherited-socket")
        .env("HERDR_PANE_ID", "inherited-pane")
        .env("HERDR_UNKNOWN", "line one\nline two")
        .env(
            std::ffi::OsString::from_vec(b"HERDR_nonutf8_\xff".to_vec()),
            "bytes",
        )
        .env("HC_TERMINAL_ID", "inherited-terminal")
        .env("HC_TERMINAL_SOCKET", "inherited-terminal-socket")
        .env("HC_SESSION", "kept-session")
        .env("HC_SOCKET", &socket)
        .env("UNRELATED", "kept-value")
        .output()
        .unwrap();
    app_popup::assert_one_test(&output);
    let calls = app_popup::calls(&root);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert_eq!(
        calls[0]["argv"],
        json!([
            "do",
            "mod.open",
            "--name",
            "namedrop-entry 'quoted'\n$(touch never) *"
        ])
    );
    let env = calls[0]["env"].as_object().unwrap();
    assert!(env.keys().all(|key| !key.starts_with("HERDR_")), "{env:?}");
    assert!(!env.contains_key("HC_TERMINAL_ID"));
    assert!(!env.contains_key("HC_TERMINAL_SOCKET"));
    assert_eq!(env["HC_SESSION"], "kept-session");
    assert_eq!(env["HC_SOCKET"], socket.to_str().unwrap());
    assert_eq!(env["UNRELATED"], "kept-value");
    assert!(!root.join("path/unexpected-app.log").exists());
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore]
fn hc_popup_child() {
    let result = HcHost::new(&Fixture::default())
        .open_popup(&namedrop::Popup {
            entrypoint: "entry 'quoted'\n$(touch never) *",
            width: "80",
            height: "20",
            env: &[],
        })
        .unwrap();
    assert_eq!(result, json!({"opened": true}));
}

#[test]
fn refused_hc_delivery_waits_for_new_target_settle_without_failure() {
    assert_refused_hc_delivery("blocked", "status");
    assert_refused_hc_delivery("status_mismatch", "snapshot");
}

#[test]
fn settle_before_refusal_recording_resumes_delivery() {
    assert_refused_hc_delivery("blocked", "earlier-settle");
}

#[test]
fn inactive_namespace_preserves_settle_for_reactivation() {
    if in_recorded_app_fixture(2) {
        return;
    }
    assert_refused_hc_delivery("blocked", "inactive-status");
    assert_refused_hc_delivery("status_mismatch", "inactive-snapshot");
}

#[test]
fn ordinary_refusals_share_the_mailbox_return_transition() {
    let delivery = include_str!("../src/delivery.rs");
    assert!(
        !delivery.contains(".defer_delivery_attempt_until_settle("),
        "ordinary refusals must use the mailbox return transition"
    );
    assert!(
        !include_str!("../src/store.rs").contains("\nmod prompt_deferral;"),
        "recipient eligibility and challenge rollback belong with their existing owners"
    );
}

#[test]
fn expired_deferred_delivery_can_be_cancelled_with_confirmation() {
    assert_refused_hc_delivery("blocked", "cancel-held");
}

#[test]
fn cancelled_attempt_requires_exact_unused_deferral_provenance() {
    for proof in ["missing-pause", "wrong-recipient", "active", "submitted"] {
        assert_refused_hc_delivery("blocked", &format!("cancel-held-{proof}"));
    }
}

fn assert_refused_hc_delivery(refusal: &str, recovery: &str) {
    use namedrop::{
        ActorKind, AgentSessionId, Clock, DeliveryId, DeliveryState, MessageDraft, MessageId,
        MessageRecipientDraft, MutationAttribution, StateStore, ThreadId,
    };
    {
        let fixture = Fixture::default();
        let agent = process::AgentProcess::new();
        fixture.agent(agent.pid());
        fixture.terminals.borrow_mut()[0]["status_ms"] = json!(900);
        let mut other = fixture.terminals.borrow()[0].clone();
        other["address"] = json!("t2");
        other["labels"]["namedrop.session"] = json!("other-session");
        other["labels"]["namedrop.name"] = json!("");
        fixture.terminals.borrow_mut().push(other);
        let state = std::env::temp_dir().join(format!(
            "namedrop-hc-deferred-{}-{refusal}-{recovery}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&state);
        struct TestClock(std::cell::Cell<u64>);
        impl Clock for TestClock {
            fn now_unix_ms(&self) -> u64 {
                self.0.get()
            }
            fn wait_ms(&self, _: u64) {}
        }
        let clock = TestClock(std::cell::Cell::new(1_000));
        let ids = namedrop::ProcessIds::default();
        let runtime = namedrop::LifecycleRuntime {
            state_directory: &state,
            home_directory: Some(&state),
            agent_config: namedrop::AgentConfigDirectories::default(),
            cwd: Path::new(env!("CARGO_MANIFEST_DIR")),
            workspace_id: Some("fixture-space"),
            current_pane_id: Some("t1"),
            session_socket_id: "hc:fixture",
            host: HcHost::new(&fixture),
            clock: &clock,
            ids: &ids,
        };
        namedrop::execute_lifecycle_operation("name", json!({"alias":"alice"}), &runtime).unwrap();
        let namespace = namedrop::StateNamespace::for_space("hc:fixture", "fixture-space");
        let mut store = StateStore::open(&state).unwrap();
        let binding = store
            .transaction(|tx| tx.active_pane_bindings(&namespace))
            .unwrap()
            .remove(0);
        namedrop::execute_lifecycle_operation(
            "name",
            json!({"alias":"bob"}),
            &namedrop::LifecycleRuntime {
                current_pane_id: Some("t2"),
                ..runtime
            },
        )
        .unwrap();
        let other_binding = store
            .transaction(|tx| tx.active_pane_bindings(&namespace))
            .unwrap()
            .into_iter()
            .find(|binding| binding.pane_id == "t2")
            .unwrap();
        let attribution = MutationAttribution::new(ActorKind::Human, "test", 1_000);
        store
            .transaction(|tx| {
                let thread = ThreadId::new("test-thread");
                tx.create_thread(&namespace, &thread, "test", &attribution)?;
                tx.append_message(
                    &namespace,
                    &MessageDraft {
                        thread_id: thread,
                        message_id: MessageId::new("test-message"),
                        in_reply_to: None,
                        author_address_id: binding.address_id.clone(),
                        author_kind: ActorKind::Human,
                        author_alias_snapshot: "alice".into(),
                        author_agent_session_snapshot: None,
                        request_id: "test-request".into(),
                        authored_source: "@alice test".into(),
                        body: "test".into(),
                        grammar_version: "mention/v1".into(),
                        recipients: vec![MessageRecipientDraft {
                            address_id: binding.address_id.clone(),
                            alias_snapshot: "alice".into(),
                            agent_session_snapshot: Some(AgentSessionId::new("fixture-session")),
                            delivery_id: DeliveryId::new("test-delivery"),
                        }],
                        context_references: Vec::new(),
                    },
                    &attribution,
                )?;
                Ok(())
            })
            .unwrap();
        let attention_before = store
            .transaction(|tx| tx.unresolved_attention_items(&namespace))
            .unwrap()
            .len();
        fixture.take_calls();
        *fixture.send_error.borrow_mut() =
            Some(json!({"error":{"kind":refusal,"message":"not sent"}}).to_string());
        if recovery == "dead-owner" {
            *fixture.retire_when_leased.borrow_mut() = Some(state.clone());
        }
        let report = namedrop::drain_mailboxes(&runtime).unwrap();
        if recovery == "dead-owner" {
            assert_eq!(report.requeued.len(), 1, "{report:?}");
            assert!(
                store
                    .transaction(|tx| tx.transition_facts(&namespace))
                    .unwrap()
                    .iter()
                    .any(|fact| fact.transition_kind == namedrop::TransitionKind::Failed)
            );
            assert!(
                store
                    .transaction(|tx| tx.unresolved_attention_items(&namespace))
                    .unwrap()
                    .len()
                    > attention_before
            );
            assert!(!fixture.calls.borrow().iter().any(|call| call[0] == "send"));
            assert_eq!(
                fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
                ""
            );
            std::fs::remove_dir_all(state).unwrap();
            return;
        }
        assert!(
            report.requeued.is_empty(),
            "settle deferral must not schedule retry: {report:?}"
        );
        assert!(!report.awaits_retry());
        assert!(report.failed.is_empty());
        assert_eq!(
            store
                .transaction(|tx| tx.unresolved_attention_items(&namespace))
                .unwrap()
                .len(),
            attention_before
        );
        let database = rusqlite::Connection::open(state.join("namedrop.sqlite3")).unwrap();
        assert_eq!(
            database
                .query_row(
                    "SELECT state FROM mailbox_deliveries WHERE delivery_id = 'test-delivery'",
                    [],
                    |row| row.get::<_, String>(0)
                )
                .unwrap(),
            "queued"
        );
        assert_eq!(
            database
                .query_row("SELECT COUNT(*) FROM leases", [], |row| row
                    .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let facts = store
            .transaction(|tx| tx.transition_facts(&namespace))
            .unwrap();
        assert!(
            !facts
                .iter()
                .any(|fact| fact.transition_kind == namedrop::TransitionKind::Failed)
        );
        let attempt = facts
            .iter()
            .find(|fact| fact.entity_kind == namedrop::TransitionEntityKind::DeliveryAttempt)
            .unwrap();
        assert_eq!(
            store
                .transaction(|tx| tx.delivery_attempt_state(
                    &namespace,
                    &namedrop::DeliveryAttemptId::new(&attempt.entity_id)
                ))
                .unwrap(),
            DeliveryState::Cancelled
        );
        assert_eq!(
            fixture
                .calls
                .borrow()
                .iter()
                .filter(|call| call[0] == "send" && call[1] == "t1")
                .count(),
            1
        );
        *fixture.send_error.borrow_mut() = None;
        if recovery.starts_with("cancel-held") {
            clock.0.set(86_401_001);
            assert_eq!(
                namedrop::drain_mailboxes(&runtime).unwrap().expired,
                [DeliveryId::new("test-delivery")]
            );
            if let Some(proof) = recovery.strip_prefix("cancel-held-") {
                match proof {
                    "missing-pause" => {
                        database.execute("DELETE FROM transition_facts WHERE entity_kind='mailbox_delivery' AND transition_kind='paused'", []).unwrap();
                    }
                    "wrong-recipient" => {
                        database.execute("DELETE FROM transition_facts WHERE entity_kind='mailbox_delivery' AND transition_kind='paused'", []).unwrap();
                        database.execute("INSERT INTO transition_facts
                            (session_socket_id,worktree_id,worktree_order,entity_kind,entity_id,transition_kind,actor_kind,actor_id,occurred_at_ms)
                            SELECT session_socket_id,worktree_id,worktree_order+1,entity_kind,'other-delivery','paused',actor_kind,actor_id,1000
                            FROM transition_facts WHERE entity_kind='mailbox_delivery' AND transition_kind='observed'", []).unwrap();
                    }
                    "active" => {
                        database
                            .execute("UPDATE delivery_attempt_state SET active=1", [])
                            .unwrap();
                    }
                    "submitted" => {
                        database.execute("INSERT INTO transition_facts
                            (session_socket_id,worktree_id,worktree_order,entity_kind,entity_id,transition_kind,actor_kind,actor_id,occurred_at_ms)
                            SELECT session_socket_id,worktree_id,(SELECT MAX(worktree_order)+1 FROM transition_facts),entity_kind,entity_id,'submitted',actor_kind,actor_id,occurred_at_ms
                            FROM transition_facts WHERE entity_kind='delivery_attempt' AND transition_kind='cancelled'", []).unwrap();
                        database.execute("UPDATE order_counters SET next_order=next_order+1 WHERE stream_kind='worktree_transition'", []).unwrap();
                    }
                    _ => unreachable!(),
                }
                assert!(
                    store
                        .transaction(|tx| tx.cancel_held_delivery(
                            &namespace,
                            &DeliveryId::new("test-delivery"),
                            &attribution
                        ))
                        .is_err(),
                    "accepted invalid cancellation proof: {proof}"
                );
                std::fs::remove_dir_all(state).unwrap();
                return;
            }
            assert!(
                namedrop::execute_control_operation(
                    "delivery.cancel-held",
                    json!({"delivery_id":"test-delivery","confirmed":false}),
                    &runtime
                )
                .is_err()
            );
            let cancelled = namedrop::execute_control_operation(
                "delivery.cancel-held",
                json!({"delivery_id":"test-delivery","confirmed":true}),
                &runtime,
            )
            .unwrap();
            assert_eq!(cancelled["state"], "cancelled");
            std::fs::remove_dir_all(state).unwrap();
            return;
        }
        store
            .transaction(|tx| {
                tx.append_message(
                    &namespace,
                    &MessageDraft {
                        thread_id: ThreadId::new("test-thread"),
                        message_id: MessageId::new("other-message"),
                        in_reply_to: None,
                        author_address_id: binding.address_id.clone(),
                        author_kind: ActorKind::Human,
                        author_alias_snapshot: "alice".into(),
                        author_agent_session_snapshot: None,
                        request_id: "other-request".into(),
                        authored_source: "@bob other work".into(),
                        body: "other work".into(),
                        grammar_version: "mention/v1".into(),
                        recipients: vec![
                            MessageRecipientDraft {
                                address_id: other_binding.address_id.clone(),
                                alias_snapshot: "bob".into(),
                                agent_session_snapshot: Some(AgentSessionId::new("other-session")),
                                delivery_id: DeliveryId::new("other-delivery"),
                            },
                            MessageRecipientDraft {
                                address_id: binding.address_id.clone(),
                                alias_snapshot: "alice".into(),
                                agent_session_snapshot: Some(AgentSessionId::new(
                                    "fixture-session",
                                )),
                                delivery_id: DeliveryId::new("followup-delivery"),
                            },
                        ],
                        context_references: Vec::new(),
                    },
                    &attribution,
                )?;
                Ok(())
            })
            .unwrap();
        let other = namedrop::drain_mailboxes(&runtime).unwrap();
        assert_eq!(other.submitted.len(), 1);
        let mut timer = namedrop::host::hc::events::Subscriber::default();
        timer
            .handle_line(
                &json!({"method":"event","params":{"type":"status","terminal":"t2"}}).to_string(),
                &runtime,
            )
            .unwrap();
        clock.0.set(31_000);
        timer.tick(&runtime).unwrap();
        assert_eq!(
            store
                .transaction(|tx| tx.delivery_attempt_state(&namespace, &other.submitted[0]))
                .unwrap(),
            DeliveryState::Settled
        );
        assert_eq!(
            fixture
                .calls
                .borrow()
                .iter()
                .filter(|call| call[0] == "send" && call[1] == "t1")
                .count(),
            1
        );
        for operation in ["thread.pause", "thread.resume"] {
            namedrop::execute_control_operation(
                operation,
                json!({"thread_id":"test-thread"}),
                &runtime,
            )
            .unwrap();
        }
        for event in ["labels", "status", "turn"] {
            let mut subscriber = namedrop::host::hc::events::Subscriber::default();
            for pane in ["t1", "t2"] {
                subscriber
                    .handle_line(
                        &json!({"method":"event","params":{"type":event,"terminal":pane}})
                            .to_string(),
                        &runtime,
                    )
                    .unwrap();
            }
            clock.0.set(clock.0.get() + 1_000);
            subscriber.tick(&runtime).unwrap();
            subscriber
                .handle_line(
                    &json!({"terminals":*fixture.terminals.borrow()}).to_string(),
                    &runtime,
                )
                .unwrap();
            subscriber
                .handle_line(r#"{"method":"missed","params":{}}"#, &runtime)
                .unwrap();
            namedrop::execute_lifecycle_operation("reconcile", json!({}), &runtime).unwrap();
            namedrop::drain_mailboxes(&runtime).unwrap();
            assert_eq!(
                fixture
                    .calls
                    .borrow()
                    .iter()
                    .filter(|call| call[0] == "send" && call[1] == "t1")
                    .count(),
                1,
                "{event} retried deferred work"
            );
        }
        fixture.terminals.borrow_mut()[0]["status"] = json!({"state":"done"});
        fixture.terminals.borrow_mut()[0]["status_ms"] = json!(if recovery == "earlier-settle" {
            950
        } else {
            clock.0.get()
        });
        if recovery.starts_with("inactive-") {
            store
                .transaction(|tx| tx.deactivate_worktree(&namespace, &attribution))
                .unwrap();
        }
        let recovered = if recovery.ends_with("snapshot") {
            json!({"terminals":*fixture.terminals.borrow()})
        } else {
            json!({"method":"event","params":{"type":"status","terminal":"t1"}})
        };
        namedrop::host::hc::events::Subscriber::default()
            .handle_line(&recovered.to_string(), &runtime)
            .unwrap();
        if recovery.starts_with("inactive-") {
            assert_eq!(
                fixture
                    .calls
                    .borrow()
                    .iter()
                    .filter(|call| call[0] == "send" && call[1] == "t1")
                    .count(),
                1
            );
            namedrop::execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime)
                .unwrap();
        }
        namedrop::drain_mailboxes(&runtime).unwrap();
        let calls = fixture.calls.borrow();
        let sends: Vec<_> = calls
            .iter()
            .filter(|call| call[0] == "send" && call[1] == "t1")
            .collect();
        assert_eq!(sends.len(), 2);
        assert_eq!(&sends[1][3..], &["--json"]);
        std::fs::remove_dir_all(state).unwrap();
    }
}

#[test]
fn onboarding_and_doctor_refusals_wait_for_new_settle() {
    if in_recorded_app_fixture(1) {
        return;
    }
    assert_onboarding_and_doctor_refusals(false);
}

#[test]
fn no_input_refusal_resets_speculatively_invoked_challenge() {
    if in_recorded_app_fixture(1) {
        return;
    }
    assert_onboarding_and_doctor_refusals(true);
}

fn in_recorded_app_fixture(expected_tours: usize) -> bool {
    let thread = std::thread::current();
    let test = thread.name().unwrap();
    if std::env::var("NAMEDROP_HC_POPUP_TEST").as_deref() == Ok(test) {
        return false;
    }
    let root = std::env::temp_dir().join(format!(
        "namedrop-refusal-popup-{}-{test}",
        std::process::id()
    ));
    let fake = app_popup::fake(&root);
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", test, "--nocapture"])
        .env("NAMEDROP_HC_POPUP_TEST", test)
        .env("HERDR_APP_BIN_PATH", fake)
        .output()
        .unwrap();
    app_popup::assert_one_test(&output);
    app_popup::assert_tour(&root, expected_tours);
    std::fs::remove_dir_all(root).unwrap();
    true
}

fn assert_onboarding_and_doctor_refusals(mark_invoked: bool) {
    use namedrop::{Clock, StateNamespace, StateStore, TransitionKind};
    for (caller, refusal) in [
        ("onboarding", "blocked"),
        ("doctor", "status_mismatch"),
        ("unnamed-doctor", "blocked"),
    ] {
        let fixture = Fixture::default();
        let agent = process::AgentProcess::new();
        fixture.agent(agent.pid());
        fixture.terminals.borrow_mut()[0]["status_ms"] = json!(900);
        let state = std::env::temp_dir().join(format!(
            "namedrop-hc-{caller}-deferred-{}-{mark_invoked}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&state);
        struct TestClock(std::cell::Cell<u64>);
        impl Clock for TestClock {
            fn now_unix_ms(&self) -> u64 {
                self.0.get()
            }
            fn wait_ms(&self, _: u64) {}
        }
        let clock = TestClock(std::cell::Cell::new(1_000));
        let ids = namedrop::ProcessIds::default();
        let runtime = namedrop::LifecycleRuntime {
            state_directory: &state,
            home_directory: Some(&state),
            agent_config: namedrop::AgentConfigDirectories::default(),
            cwd: Path::new(env!("CARGO_MANIFEST_DIR")),
            workspace_id: Some("fixture-space"),
            current_pane_id: Some("t1"),
            session_socket_id: "hc:fixture",
            host: HcHost::new(&fixture),
            clock: &clock,
            ids: &ids,
        };
        if caller == "unnamed-doctor" {
            namedrop::execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime)
                .unwrap();
        } else {
            namedrop::execute_lifecycle_operation("name", json!({"alias":"alice"}), &runtime)
                .unwrap();
        }
        let namespace = StateNamespace::for_space("hc:fixture", "fixture-space");
        let mut store = StateStore::open(&state).unwrap();
        if caller == "onboarding" {
            let db = rusqlite::Connection::open(state.join("namedrop.sqlite3")).unwrap();
            db.execute(
                "UPDATE pane_roles SET description = 'Manually configured collaborator'",
                [],
            )
            .unwrap();
            db.execute("UPDATE integration_registrations SET active = 0", [])
                .unwrap();
        } else {
            namedrop::execute_control_operation("worktree.pause", json!({}), &runtime).unwrap();
        }
        let invoke = || {
            if caller == "onboarding" {
                namedrop::execute_lifecycle_operation("reconcile", json!({}), &runtime).unwrap();
            } else {
                let error =
                    namedrop::request_doctor_assistance(&runtime, "fixture-session").unwrap_err();
                assert!(error.starts_with("Not sent"), "{error}");
            }
        };
        fixture.take_calls();
        *fixture.send_error.borrow_mut() =
            Some(json!({"error":{"kind":refusal,"message":"not sent"}}).to_string());
        if mark_invoked && caller == "onboarding" {
            let path = state.join("namedrop.sqlite3");
            *fixture.on_send.borrow_mut() = Some(Box::new(move || {
                let db = rusqlite::Connection::open(&path).unwrap();
                db.execute("INSERT INTO transition_facts
                    (session_socket_id,worktree_id,worktree_order,entity_kind,entity_id,transition_kind,actor_kind,actor_id,occurred_at_ms)
                    SELECT session_socket_id,worktree_id,
                        (SELECT MAX(worktree_order)+1 FROM transition_facts),entity_kind,entity_id,'invoked',actor_kind,actor_id,occurred_at_ms
                    FROM transition_facts WHERE entity_kind='integration_registration'
                    AND transition_kind='dispatching' ORDER BY worktree_order DESC LIMIT 1", []).unwrap();
                db.execute(
                    "UPDATE order_counters SET next_order=next_order+1
                    WHERE stream_kind='worktree_transition'",
                    [],
                )
                .unwrap();
            }));
        }
        invoke();
        assert_eq!(
            fixture
                .calls
                .borrow()
                .iter()
                .filter(|call| call[0] == "send")
                .count(),
            1,
            "{caller}"
        );
        *fixture.send_error.borrow_mut() = None;
        invoke();
        let mut subscriber = namedrop::host::hc::events::Subscriber::default();
        for status in ["idle", "working"] {
            fixture.terminals.borrow_mut()[0]["status"] = json!({"state":status});
            subscriber
                .handle_line(
                    &json!({"method":"event","params":{"type":"status","terminal":"t1"}})
                        .to_string(),
                    &runtime,
                )
                .unwrap();
            invoke();
        }
        assert_eq!(
            fixture
                .calls
                .borrow()
                .iter()
                .filter(|call| call[0] == "send")
                .count(),
            1
        );
        assert!(
            !store
                .transaction(|tx| tx.transition_facts(&namespace))
                .unwrap()
                .iter()
                .any(|fact| fact.transition_kind == TransitionKind::Failed)
        );
        clock.0.set(2_000);
        fixture.terminals.borrow_mut()[0]["status"] = json!({"state":"done"});
        fixture.terminals.borrow_mut()[0]["status_ms"] = json!(950);
        subscriber
            .handle_line(
                &json!({"method":"event","params":{"type":"status","terminal":"t1"}}).to_string(),
                &runtime,
            )
            .unwrap();
        if caller != "onboarding" {
            namedrop::request_doctor_assistance(&runtime, "fixture-session").unwrap();
        }
        assert_eq!(
            fixture
                .calls
                .borrow()
                .iter()
                .filter(|call| call[0] == "send")
                .count(),
            2,
            "{caller}"
        );
        std::fs::remove_dir_all(state).unwrap();
    }
}

#[cfg(target_os = "linux")]
#[path = "support/hc_prompt_race.rs"]
mod prompt_race;

#[test]
fn dead_session_owner_is_a_preinvocation_failure() {
    let fixture = Fixture::default();
    fixture.terminals.borrow_mut()[0]["labels"]["namedrop.session.pid"] = json!("4000000000");
    fixture.terminals.borrow_mut()[0]["labels"]["namedrop.session.start"] = json!("gone");
    let result = HcHost::new(&fixture)
        .prompt("t1", "must not type")
        .unwrap_err();
    assert_eq!(result.kind, PromptFailureKind::TemporaryPreInvocation);
    assert_eq!(
        fixture.terminals.borrow()[0]["labels"]["namedrop.session"],
        ""
    );
    assert!(!fixture.calls.borrow().iter().any(|call| call[0] == "send"));
}

#[test]
fn stale_prompt_does_not_erase_a_replacement_session() {
    if process::in_agent_tree("stale_prompt_does_not_erase_a_replacement_session") {
        return;
    }
    let fixture = ConcurrentFixture::new();
    let state = fixture.environment.hc_state_directory().unwrap();
    let host = HcHost::new(&fixture);
    fixture.record.lock().unwrap()["pid"] = json!(std::os::unix::process::parent_id());
    host.publish_session(&state, "t1", "old").unwrap();
    fixture.record.lock().unwrap()["labels"]["namedrop.session.pid"] = json!("4000000000");
    let (entered, resume) = fixture.pause_on_get(1);
    std::thread::scope(|scope| {
        let sending = scope.spawn(|| host.prompt("t1", "must not type"));
        entered
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        host.publish_session(&state, "t1", "replacement").unwrap();
        resume.send(()).unwrap();
        assert!(sending.join().unwrap().is_err());
    });
    assert_eq!(
        fixture.record.lock().unwrap()["labels"]["namedrop.session"],
        "replacement"
    );
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn dead_owner_after_lease_records_failed_attention_and_requeues() {
    assert_refused_hc_delivery("blocked", "dead-owner");
}
