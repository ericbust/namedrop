#[path = "fixtures/hc/process.rs"]
mod process;
use namedrop::{
    HcHost, HerdrCommandExecutor, HerdrCommandOutput, Host, PaneScope, PromptFailureKind,
};
use serde_json::{Value, json};
use std::cell::RefCell;

struct Fixture {
    terminal: RefCell<Value>,
    calls: RefCell<Vec<Vec<String>>>,
}
impl Fixture {
    fn new(pid: u32, program: &str) -> Self {
        let mut terminal: Value =
            serde_json::from_str(include_str!("fixtures/hc/get-labeled.stdout")).unwrap();
        terminal["pid"] = json!(std::os::unix::process::parent_id());
        terminal["foreground"] = json!({"pid":pid,"name":program,"argv":[program]});
        terminal["labels"]["namedrop.session.pid"] = json!(pid.to_string());
        terminal["labels"]["namedrop.session.start"] = json!(process::start(pid));
        terminal["status"] = json!({"state":"idle","app":"codex"});
        terminal["reported"] = json!(true);
        Self {
            terminal: RefCell::new(terminal),
            calls: RefCell::default(),
        }
    }
    fn calls(&self, verb: &str) -> Vec<Vec<String>> {
        self.calls
            .borrow()
            .iter()
            .filter(|call| call[0] == verb)
            .cloned()
            .collect()
    }
}
impl HerdrCommandExecutor for Fixture {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|s| s.to_string()).collect());
        let stdout = match args {
            ["ls", "--json"] => json!({"terminals":[*self.terminal.borrow()]}).to_string(),
            ["get", "t1", "--json"] => self.terminal.borrow().to_string(),
            ["label", "t1", labels @ ..] => {
                for label in labels.iter().filter(|s| **s != "--json") {
                    let (key, value) = label.split_once('=').unwrap();
                    self.terminal.borrow_mut()["labels"][key] = json!(value);
                }
                "{}".into()
            }
            ["send", _, _, "--if", expected, "--json"]
                if self.terminal.borrow()["status"]["state"] != *expected =>
            {
                return Err(
                    json!({"error":{"kind":"status_mismatch","message":"condition failed"}})
                        .to_string(),
                );
            }
            ["send", ..] => "{}".into(),
            _ => panic!("unexpected argv {args:?}"),
        };
        Ok(HerdrCommandOutput { stdout })
    }
}

#[test]
fn stale_shell_owner_cannot_receive_agent_prompts() {
    let shell = process::AgentProcess::shell();
    let fixture = Fixture::new(shell.pid(), "bash");
    let host = HcHost::new(&fixture);
    let pane = host.panes(PaneScope::All).unwrap().remove(0);
    assert!(
        pane.agent.is_none(),
        "sticky status.app must not classify a shell as an agent"
    );
    assert!(pane.agent_session_id.is_none());
    fixture.calls.borrow_mut().clear();
    assert_eq!(
        host.prompt("t1", "hello").unwrap_err().kind,
        PromptFailureKind::DeferredUntilSettle {
            observed_status_ms: None
        }
    );
    assert_eq!(fixture.calls("get").len(), 1);
    assert!(fixture.calls("send").is_empty());
    assert_eq!(fixture.terminal.borrow()["labels"]["namedrop.session"], "");
    assert_eq!(
        fixture.terminal.borrow()["labels"]["namedrop.session.pid"],
        ""
    );
    fixture.terminal.borrow_mut()["pid"] = json!(shell.pid());
    let clock = namedrop::SystemClock;
    let ids = namedrop::ProcessIds::default();
    let runtime = namedrop::LifecycleRuntime {
        state_directory: std::path::Path::new("/unused"),
        home_directory: None,
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        workspace_id: Some("fixture-space"),
        current_pane_id: Some("t1"),
        session_socket_id: "hc:fixture",
        host,
        clock: &clock,
        ids: &ids,
    };
    assert_eq!(
        namedrop::detect_pane_type(&runtime).unwrap(),
        Some(namedrop::PaneType::Terminal)
    );
    host.run_in_pane("t1", "echo allowed").unwrap();
    assert_eq!(fixture.calls("send").len(), 1);
}

#[test]
fn live_owner_with_other_foreground_defers_but_retains_session() {
    let agent = process::AgentProcess::new();
    let fixture = Fixture::new(agent.pid(), "codex");
    fixture.terminal.borrow_mut()["foreground"] =
        json!({"pid":std::process::id(),"name":"vim","argv":["vim"]});
    let host = HcHost::new(&fixture);
    let pane = host.panes(PaneScope::All).unwrap().remove(0);
    assert_eq!(pane.agent.as_deref(), Some("codex"));
    assert!(pane.agent_session_id.is_some());
    assert_eq!(pane.agent_status.as_deref(), Some("working"));
    fixture.calls.borrow_mut().clear();
    assert_eq!(
        host.prompt("t1", "hello").unwrap_err().kind,
        PromptFailureKind::DeferredUntilSettle {
            observed_status_ms: None
        }
    );
    assert!(host.run_in_pane("t1", "echo denied").is_err());
    assert!(fixture.calls("send").is_empty());
    assert!(fixture.calls("label").is_empty());
}

#[test]
fn ready_agent_uses_one_fresh_read_and_status_specific_send() {
    let agent = process::AgentProcess::new();
    for status in ["idle", "done"] {
        let fixture = Fixture::new(agent.pid(), "codex");
        fixture.terminal.borrow_mut()["status"]["state"] = json!(status);
        let host = HcHost::new(&fixture);
        let pane = host.panes(PaneScope::All).unwrap().remove(0);
        assert_eq!(pane.agent_status.as_deref(), Some(status));
        fixture.calls.borrow_mut().clear();
        let result = host.prompt("t1", "hello");
        result.unwrap();
        assert_eq!(fixture.calls("get").len(), 1);
        let expected = if status == "idle" {
            vec!["send", "t1", "hello", "--if", "idle", "--json"]
        } else {
            vec!["send", "t1", "hello", "--json"]
        };
        assert_eq!(fixture.calls("send"), vec![expected]);
    }
}

#[test]
fn nonsettled_or_unbound_agent_never_receives_prompt() {
    let agent = process::AgentProcess::new();
    for status in [
        Some("working"),
        Some("blocked"),
        Some("error"),
        Some("future-status"),
        None,
    ] {
        let fixture = Fixture::new(agent.pid(), "codex");
        fixture.terminal.borrow_mut()["status"] = status
            .map(|state| json!({"state":state,"app":"codex"}))
            .unwrap_or(Value::Null);
        let host = HcHost::new(&fixture);
        assert!(host.prompt("t1", "hello").is_err());
        assert_eq!(fixture.calls("get").len(), 1);
        assert!(fixture.calls("send").is_empty());
    }
    let fixture = Fixture::new(agent.pid(), "codex");
    fixture.terminal.borrow_mut()["labels"]
        .as_object_mut()
        .unwrap()
        .remove("namedrop.session.pid");
    assert!(HcHost::new(&fixture).prompt("t1", "hello").is_err());
    assert!(fixture.calls("send").is_empty());
}

#[test]
fn exited_owner_is_cleared_even_when_hc_foreground_and_status_are_stale() {
    let agent = process::AgentProcess::new();
    let fixture = Fixture::new(agent.pid(), "codex");
    drop(agent);
    let host = HcHost::new(&fixture);
    let pane = host.panes(PaneScope::All).unwrap().remove(0);
    assert!(pane.agent_session_id.is_none());
    fixture.calls.borrow_mut().clear();
    assert!(host.prompt("t1", "hello").is_err());
    assert_eq!(fixture.calls("get").len(), 1);
    assert_eq!(fixture.calls("label").len(), 1);
    assert!(fixture.calls("send").is_empty());
}

#[test]
fn publish_binds_the_agent_ancestor_and_refuses_a_shell() {
    if process::in_agent_tree("publish_binds_the_agent_ancestor_and_refuses_a_shell") {
        return;
    }
    let owner = std::os::unix::process::parent_id();
    let fixture = Fixture::new(owner, "codex");
    let state = std::env::temp_dir().join(format!("hc-owner-test-{}", std::process::id()));
    let host = HcHost::new(&fixture);
    host.publish_session(&state, "t1", "new").unwrap();
    assert_eq!(
        fixture.terminal.borrow()["labels"]["namedrop.session.pid"],
        owner.to_string()
    );
    fixture.terminal.borrow_mut()["pid"] = json!(
        std::env::var("NAMEDROP_HC_TERMINAL_PID")
            .unwrap()
            .parse::<u32>()
            .unwrap()
    );
    host.publish_session(&state, "t1", "nested").unwrap();
    assert_eq!(
        fixture.terminal.borrow()["labels"]["namedrop.session.pid"],
        owner.to_string()
    );
    fixture.terminal.borrow_mut()["pid"] = json!(std::process::id());
    assert!(host.publish_session(&state, "t1", "wrong").is_err());
    assert_eq!(
        fixture.terminal.borrow()["labels"]["namedrop.session"],
        "nested"
    );
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn reused_pid_with_a_different_start_cannot_own_the_old_session() {
    let agent = process::AgentProcess::new();
    let fixture = Fixture::new(agent.pid(), "codex");
    fixture.terminal.borrow_mut()["labels"]["namedrop.session.start"] = json!("different-start");
    let host = HcHost::new(&fixture);
    assert!(
        host.panes(PaneScope::All).unwrap()[0]
            .agent_session_id
            .is_none()
    );
    fixture.calls.borrow_mut().clear();
    assert!(host.prompt("t1", "hello").is_err());
    assert_eq!(fixture.calls("get").len(), 1);
    assert!(fixture.calls("send").is_empty());
    assert_eq!(
        fixture.terminal.borrow()["labels"]["namedrop.session.start"],
        ""
    );
}

#[test]
fn publish_never_falls_back_to_the_terminal_shell() {
    if process::in_process_tree("publish_never_falls_back_to_the_terminal_shell", "bash") {
        return;
    }
    let fixture = Fixture::new(std::os::unix::process::parent_id(), "bash");
    let state = std::env::temp_dir().join(format!("hc-shell-owner-test-{}", std::process::id()));
    let error = HcHost::new(&fixture)
        .publish_session(&state, "t1", "wrong")
        .unwrap_err();
    assert!(error.message.contains("no recognized agent ancestor"));
    assert!(fixture.calls("label").is_empty());
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn missing_or_empty_start_cannot_own_a_session() {
    let agent = process::AgentProcess::new();
    for start in [None, Some("")] {
        let fixture = Fixture::new(agent.pid(), "codex");
        if let Some(start) = start {
            fixture.terminal.borrow_mut()["labels"]["namedrop.session.start"] = json!(start);
        } else {
            fixture.terminal.borrow_mut()["labels"]
                .as_object_mut()
                .unwrap()
                .remove("namedrop.session.start");
        }
        let host = HcHost::new(&fixture);
        assert!(
            host.panes(PaneScope::All).unwrap()[0]
                .agent_session_id
                .is_none(),
            "legacy session without start identity was accepted"
        );
        assert!(host.prompt("t1", "old session prompt").is_err());
        assert!(fixture.calls("send").is_empty());
        assert_eq!(fixture.terminal.borrow()["labels"]["namedrop.session"], "");
    }
}

#[test]
fn non_shell_terminal_root_never_receives_runner_text() {
    for program in ["vim", "python3"] {
        let process = process::AgentProcess::with_program(program);
        let fixture = Fixture::new(process.pid(), program);
        fixture.terminal.borrow_mut()["pid"] = json!(process.pid());
        let host = HcHost::new(&fixture);
        assert!(
            host.run_in_pane("t1", "namedrop terminal-execute attempt")
                .is_err(),
            "direct {program} terminal accepted shell text"
        );
        assert!(
            host.process_info("t1").is_none(),
            "direct {program} terminal advertised an available shell"
        );
        assert!(fixture.calls("send").is_empty());
    }
}

#[test]
fn lost_terminal_clears_even_a_live_session_owner() {
    let agent = process::AgentProcess::new();
    let fixture = Fixture::new(agent.pid(), "codex");
    fixture.terminal.borrow_mut()["state"] = json!("lost");
    let state = std::env::temp_dir().join(format!("hc-lost-owner-{}", std::process::id()));
    HcHost::new(&fixture)
        .invalidate_session(&state, "t1")
        .unwrap();
    assert_eq!(
        fixture.terminal.borrow()["labels"]["namedrop.session"],
        "",
        "lost terminal retained its live orphan's session"
    );
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn non_shell_root_does_not_create_delivery_attempts() {
    use namedrop::*;
    let program = process::AgentProcess::with_program("python3");
    let fixture = Fixture::new(program.pid(), "python3");
    fixture.terminal.borrow_mut()["pid"] = json!(program.pid());
    let state = std::env::temp_dir().join(format!("hc-nonshell-delivery-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&state);
    let clock = SystemClock;
    let ids = ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: &state,
        home_directory: Some(&state),
        agent_config: AgentConfigDirectories::default(),
        cwd: std::path::Path::new(env!("CARGO_MANIFEST_DIR")),
        workspace_id: Some("fixture-space"),
        current_pane_id: Some("t1"),
        session_socket_id: "hc:fixture",
        host: HcHost::new(&fixture),
        clock: &clock,
        ids: &ids,
    };
    execute_lifecycle_operation(
        "name",
        json!({"alias":"build","pane_type":"terminal"}),
        &runtime,
    )
    .unwrap();
    let ns = StateNamespace::for_space("hc:fixture", "fixture-space");
    let attribution = MutationAttribution::new(ActorKind::Human, "fixture", clock.now_unix_ms());
    StateStore::open(&state)
        .unwrap()
        .transaction(|tx| {
            let binding = tx.active_pane_bindings(&ns)?.remove(0);
            tx.create_thread(&ns, &ThreadId::new("thread"), "fixture", &attribution)?;
            tx.append_message(
                &ns,
                &MessageDraft {
                    thread_id: ThreadId::new("thread"),
                    message_id: MessageId::new("message"),
                    in_reply_to: None,
                    author_address_id: binding.address_id.clone(),
                    author_kind: ActorKind::Human,
                    author_alias_snapshot: "build".into(),
                    author_agent_session_snapshot: None,
                    request_id: "request".into(),
                    authored_source: "true".into(),
                    body: "true".into(),
                    grammar_version: "mention/v1".into(),
                    context_references: vec![],
                    recipients: vec![MessageRecipientDraft {
                        address_id: binding.address_id,
                        alias_snapshot: "build".into(),
                        agent_session_snapshot: None,
                        delivery_id: DeliveryId::new("delivery"),
                    }],
                },
                &attribution,
            )?;
            Ok(())
        })
        .unwrap();
    let result = drain_mailboxes(&runtime).unwrap();
    assert!(result.submitted.is_empty());
    assert!(fixture.calls("send").is_empty());
    let db = rusqlite::Connection::open(state.join("namedrop.sqlite3")).unwrap();
    for table in ["delivery_attempts", "terminal_executions"] {
        let count: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(count, 0, "non-shell root created a {table} row");
    }
    let shell = process::AgentProcess::shell();
    fixture.terminal.borrow_mut()["pid"] = json!(shell.pid());
    fixture.terminal.borrow_mut()["foreground"] =
        json!({"pid":shell.pid(),"name":"bash","argv":["bash"]});
    let result = drain_mailboxes(&runtime).unwrap();
    assert_eq!(
        result.submitted.len(),
        1,
        "real shell did not receive queued runner"
    );
    assert_eq!(fixture.calls("send").len(), 1);
    assert!(fixture.calls("send")[0][2].contains("terminal-execute"));
    drop(db);
    std::fs::remove_dir_all(state).unwrap();
}

#[test]
fn unknown_status_remains_discoverable_without_becoming_settled() {
    let agent = process::AgentProcess::new();
    let fixture = Fixture::new(agent.pid(), "codex");
    fixture.terminal.borrow_mut()["status"]["state"] = json!("future-status");
    let panes = HcHost::new(&fixture).panes(PaneScope::All).unwrap();
    assert_eq!(panes.len(), 1);
    assert_eq!(panes[0].agent_status.as_deref(), Some("unknown"));
    assert!(HcHost::new(&fixture).prompt("t1", "hello").is_err());
    assert!(fixture.calls("send").is_empty());
}
