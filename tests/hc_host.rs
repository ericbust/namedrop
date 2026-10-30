use namedrop::{
    CallerClaim, HcHost, HerdrCommandExecutor, HerdrCommandOutput, Host, PaneMetadata, PaneScope,
    PromptFailureKind, SpaceLocator,
};
use serde_json::{Value, json};
use std::{cell::RefCell, path::Path};

struct Fixture {
    terminals: RefCell<Vec<Value>>,
    calls: RefCell<Vec<Vec<String>>>,
    send_error: Option<String>,
}
impl Default for Fixture {
    fn default() -> Self {
        Self {
            terminals: RefCell::new(vec![
                serde_json::from_str(include_str!("fixtures/hc/get-labeled.stdout")).unwrap(),
            ]),
            calls: RefCell::new(Vec::new()),
            send_error: None,
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
            ["get", id, "--json"] => self
                .terminals
                .borrow()
                .iter()
                .find(|terminal| terminal["address"] == *id)
                .ok_or_else(|| include_str!("fixtures/hc/not-found.stderr").to_owned())?
                .to_string(),
            ["read", ..] => include_str!("fixtures/hc/read.stdout").to_owned(),
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
impl Fixture {
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
    assert_eq!(
        pane.agent_session_id.as_ref().unwrap().as_str(),
        "fixture-session"
    );
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
    host.prompt("t1", "hello").unwrap();
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
            vec!["send", "t1", "hello", "--json"],
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
            vec!["label", "t1", "namedrop.name=alice", "--json"],
        ]
    );
}

#[test]
fn status_words_and_program_fallback_keep_unknown_distinct() {
    let fixture = Fixture::default();
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
        ("blocked", "", PromptFailureKind::TemporaryPreInvocation),
        (
            "status_mismatch",
            "",
            PromptFailureKind::TemporaryPreInvocation,
        ),
        (
            "unavailable",
            "nothing was typed",
            PromptFailureKind::TemporaryPreInvocation,
        ),
        (
            "unavailable",
            "the program was lost",
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
            send_error: Some(
                json!({"error":{"kind":kind,"message":"rejected","hint":hint}}).to_string(),
            ),
            ..Fixture::default()
        };
        let failure = HcHost::new(&fixture).prompt("t1", "hello").unwrap_err();
        assert_eq!(failure.kind, expected, "{kind} {hint}");
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
fn unknown_programs_and_reporting_shells_are_not_shell_recipients() {
    let fixture = Fixture::default();
    fixture.terminals.borrow_mut()[0]["foreground"]["argv"] = json!(["vim"]);
    assert_eq!(
        HcHost::new(&fixture).panes(PaneScope::All).unwrap()[0]
            .agent
            .as_deref(),
        Some("vim")
    );
    fixture.terminals.borrow_mut()[0]["foreground"]["argv"] = json!(["bash"]);
    fixture.terminals.borrow_mut()[0]["reported"] = json!(true);
    assert_eq!(
        HcHost::new(&fixture).panes(PaneScope::All).unwrap()[0]
            .agent
            .as_deref(),
        Some("bash")
    );
}

#[test]
fn events_reconcile_names_preserve_lost_aliases_and_clear_sessions() {
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
    subscriber.handle_line(&event("labels"), &runtime).unwrap();
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
fn subscriber_start_is_idempotent_only_for_name_taken() {
    struct StartTransport {
        error: Option<&'static str>,
    }
    impl HerdrCommandExecutor for StartTransport {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            assert_eq!(
                args,
                [
                    "run",
                    "--bg",
                    "--no-parent",
                    "--rm",
                    "--name",
                    "namedrop-events",
                    "--",
                    "/namedrop",
                    "--host",
                    "hc",
                    "--plugin-state",
                    "/state",
                    "events"
                ]
            );
            match self.error {
                Some(kind) => Err(json!({"error":{"kind":kind,"message":"rejected"}}).to_string()),
                None => Ok(HerdrCommandOutput {
                    stdout: "{}".into(),
                }),
            }
        }
    }
    for (kind, success) in [
        (None, true),
        (Some("name_taken"), true),
        (Some("invalid_params"), false),
    ] {
        assert_eq!(
            namedrop::host::hc::events::start(
                &HcHost::new(StartTransport { error: kind }),
                Path::new("/namedrop"),
                Path::new("/state")
            )
            .is_ok(),
            success
        );
    }
}
