#[allow(dead_code)]
#[path = "fixtures/hc/process.rs"]
mod process;

use namedrop::*;
use serde_json::{Value, json};
use std::{
    cell::{Cell, RefCell},
    path::Path,
};

struct Fixture {
    terminals: RefCell<Vec<Value>>,
    unavailable: Cell<bool>,
    sends: RefCell<Vec<String>>,
}
impl Fixture {
    fn new(pid: u32) -> Self {
        let mut agent: Value =
            serde_json::from_str(include_str!("fixtures/hc/get-labeled.stdout")).unwrap();
        agent["labels"]["namedrop.session.pid"] = json!(pid.to_string());
        agent["foreground"] = json!({"pid":pid,"argv":["codex"],"name":"codex"});
        agent["status"] = json!({"state":"idle"});
        let mut shell: Value =
            serde_json::from_str(include_str!("fixtures/hc/get-labeled.stdout")).unwrap();
        shell["address"] = json!("t2");
        shell["labels"] = json!({"herdr.space":"fixture-space","herdr.tab":"healthy-tab"});
        Self {
            terminals: RefCell::new(vec![agent, shell]),
            unavailable: Cell::new(false),
            sends: RefCell::default(),
        }
    }
}
impl HerdrCommandExecutor for Fixture {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let stdout = match args {
            ["ls", "--json"] => json!({"terminals": *self.terminals.borrow()}).to_string(),
            ["get", "t1", "--json"] if self.unavailable.get() => {
                return Err("temporary get failure".into());
            }
            ["get", id, "--json"] => self
                .terminals
                .borrow()
                .iter()
                .find(|t| t["address"] == *id)
                .unwrap()
                .to_string(),
            ["label", id, labels @ ..] => {
                let mut terminals = self.terminals.borrow_mut();
                let terminal = terminals.iter_mut().find(|t| t["address"] == *id).unwrap();
                for label in labels.iter().filter(|label| **label != "--json") {
                    let (key, value) = label.split_once('=').unwrap();
                    terminal["labels"][key] = json!(value);
                }
                "{}".into()
            }
            ["send", id, ..] => {
                self.sends.borrow_mut().push(id.to_string());
                "{}".into()
            }
            other => panic!("unexpected hc call {other:?}"),
        };
        Ok(HerdrCommandOutput { stdout })
    }
}
struct TestClock;
impl Clock for TestClock {
    fn now_unix_ms(&self) -> u64 {
        1000
    }
    fn wait_ms(&self, _: u64) {}
}

#[test]
fn partial_discovery_preserves_binding_and_inflight_while_healthy_pane_progresses() {
    check_partial("event.reconcile");
}
#[test]
fn naming_healthy_pane_does_not_remove_unavailable_neighbor() {
    check_partial("event.name");
}
#[test]
fn unavailable_binding_is_not_compacted_onto_another_pane() {
    check_partial("compaction");
}
#[test]
fn explicit_close_still_removes_an_unavailable_pane() {
    check_partial("pane_closed");
}
#[test]
fn explicit_exit_still_removes_an_unavailable_pane() {
    check_partial("pane_exited");
}
#[test]
fn explicit_unbind_still_holds_an_unavailable_panes_inflight_delivery() {
    check_partial("unbind");
}
fn check_partial(operation: &str) {
    let agent = process::AgentProcess::new();
    let fixture = Fixture::new(agent.pid());
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target/hc-partial")
        .join(format!("{operation}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let ids = ProcessIds::default();
    let clock = TestClock;
    let runtime = LifecycleRuntime {
        state_directory: &root,
        home_directory: Some(&root),
        agent_config: AgentConfigDirectories::default(),
        cwd: Path::new(env!("CARGO_MANIFEST_DIR")),
        workspace_id: Some("fixture-space"),
        current_pane_id: Some("t1"),
        session_socket_id: "hc:partial",
        host: HcHost::new(&fixture),
        clock: &clock,
        ids: &ids,
    };
    execute_lifecycle_operation("name", json!({"alias":"alice"}), &runtime).unwrap();
    let healthy = LifecycleRuntime {
        current_pane_id: Some("t2"),
        ..runtime
    };
    execute_lifecycle_operation(
        "name",
        json!({"alias":"shell","pane_type":"terminal"}),
        &healthy,
    )
    .unwrap();
    let namespace = StateNamespace::for_space("hc:partial", "fixture-space");
    let attribution = MutationAttribution::new(ActorKind::Human, "test", 1000);
    let attempt = DeliveryAttemptId::new("inflight");
    let mut store = StateStore::open(&root).unwrap();
    store
        .transaction(|tx| {
            let bindings = tx.active_pane_bindings(&namespace)?;
            let first = bindings.iter().find(|b| b.pane_id == "t1").unwrap();
            let second = bindings.iter().find(|b| b.pane_id == "t2").unwrap();
            tx.create_thread(&namespace, &ThreadId::new("thread"), "test", &attribution)?;
            for (n, binding, session) in [
                (1, first, Some(AgentSessionId::new("fixture-session"))),
                (2, second, None),
            ] {
                tx.append_message(
                    &namespace,
                    &MessageDraft {
                        thread_id: ThreadId::new("thread"),
                        message_id: MessageId::new(format!("m{n}")),
                        in_reply_to: None,
                        author_address_id: first.address_id.clone(),
                        author_kind: ActorKind::Human,
                        author_alias_snapshot: "alice".into(),
                        author_agent_session_snapshot: None,
                        request_id: format!("r{n}"),
                        authored_source: "true".into(),
                        body: "true".into(),
                        grammar_version: "mention/v1".into(),
                        context_references: vec![],
                        recipients: vec![MessageRecipientDraft {
                            address_id: binding.address_id.clone(),
                            alias_snapshot: if n == 1 { "alice" } else { "shell" }.into(),
                            agent_session_snapshot: session,
                            delivery_id: DeliveryId::new(format!("d{n}")),
                        }],
                    },
                    &attribution,
                )?;
            }
            assert!(
                tx.lease_mailbox_head(
                    &namespace,
                    &first.address_id,
                    &LeaseDraft {
                        attempt_id: attempt.clone(),
                        binding_id: first.id.clone(),
                        agent_session_id: AgentSessionId::new("fixture-session"),
                        retry_of_attempt_id: None,
                        owner: "other".into(),
                        acquired_at_ms: 1000,
                        expires_at_ms: 31000,
                    },
                    &attribution
                )?
                .is_some()
            );
            Ok(())
        })
        .unwrap();
    fixture.unavailable.set(true);
    if operation == "compaction" {
        let mut duplicate = fixture.terminals.borrow()[0].clone();
        duplicate["address"] = json!("t3");
        fixture.terminals.borrow_mut().push(duplicate);
    }
    fixture.terminals.borrow_mut()[1]["labels"]["herdr.tab"] = json!("moved-tab");
    let input = if operation == "event.name" {
        json!({"alias":"shell"})
    } else {
        json!({})
    };
    let reconcile_operation = if operation == "event.name" {
        operation
    } else {
        "event.reconcile"
    };
    execute_lifecycle_operation(reconcile_operation, input, &healthy).unwrap();
    store
        .transaction(|tx| {
            let bindings = tx.active_pane_bindings(&namespace)?;
            assert!(
                bindings.iter().any(|b| b.pane_id == "t1"),
                "unavailable pane lost its binding"
            );
            assert_eq!(
                tx.delivery_attempt_state(&namespace, &attempt)?,
                DeliveryState::Dispatching
            );
            assert_eq!(
                bindings
                    .iter()
                    .find(|b| b.pane_id == "t2")
                    .unwrap()
                    .tab_id
                    .as_deref(),
                Some("moved-tab")
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(*fixture.sends.borrow(), ["t2"]);
    if operation == "unbind" {
        store
            .transaction(|tx| {
                let binding = tx
                    .active_pane_bindings(&namespace)?
                    .into_iter()
                    .find(|b| b.pane_id == "t1")
                    .unwrap();
                tx.unbind_pane(&namespace, &binding.address_id, &attribution)
            })
            .unwrap();
        drain_mailboxes(&healthy).unwrap();
        assert_eq!(
            store
                .transaction(|tx| tx.delivery_attempt_state(&namespace, &attempt))
                .unwrap(),
            DeliveryState::Held
        );
        std::fs::remove_dir_all(root).unwrap();
        return;
    }
    if matches!(operation, "pane_closed" | "pane_exited") {
        execute_lifecycle_operation(
            "event.pane-lifecycle",
            json!({
                "type": operation, "pane_id":"t1", "workspace_id":"fixture-space"
            }),
            &healthy,
        )
        .unwrap();
        store
            .transaction(|tx| {
                assert!(
                    !tx.active_pane_bindings(&namespace)?
                        .iter()
                        .any(|b| b.pane_id == "t1")
                );
                assert_eq!(
                    tx.delivery_attempt_state(&namespace, &attempt)?,
                    DeliveryState::Cancelled
                );
                Ok(())
            })
            .unwrap();
        std::fs::remove_dir_all(root).unwrap();
        return;
    }
    fixture.unavailable.set(false);
    fixture.terminals.borrow_mut().truncate(2);
    fixture.terminals.borrow_mut()[0]["labels"]["namedrop.session"] = json!("recovered-session");
    fixture.terminals.borrow_mut()[0]["status"] = json!({"state":"working"});
    execute_lifecycle_operation("event.reconcile", json!({}), &healthy).unwrap();
    store
        .transaction(|tx| {
            let addresses = tx.list_pane_addresses(&namespace)?;
            let recovered = addresses
                .iter()
                .find(|a| a.pane_id.as_deref() == Some("t1"))
                .unwrap();
            assert_eq!(
                recovered.agent_session_id.as_ref().map(|s| s.as_str()),
                Some("recovered-session")
            );
            assert_eq!(
                tx.delivery_attempt_state(&namespace, &attempt)?,
                DeliveryState::Held
            );
            Ok(())
        })
        .unwrap();
    fixture.terminals.borrow_mut().remove(0);
    execute_lifecycle_operation("event.reconcile", json!({}), &healthy).unwrap();
    store
        .transaction(|tx| {
            assert!(
                !tx.active_pane_bindings(&namespace)?
                    .iter()
                    .any(|b| b.pane_id == "t1")
            );
            assert_eq!(
                tx.delivery_attempt_state(&namespace, &attempt)?,
                DeliveryState::Held
            );
            Ok(())
        })
        .unwrap();
    assert_eq!(*fixture.sends.borrow(), ["t2"]);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn dead_untagged_foreground_is_not_an_agent() {
    let agent = process::AgentProcess::new();
    let fixture = Fixture::new(agent.pid());
    fixture.terminals.borrow_mut()[0]["labels"]
        .as_object_mut()
        .unwrap()
        .remove("namedrop.session.pid");
    drop(agent);
    let panes = HcHost::new(&fixture).panes(PaneScope::All).unwrap();
    assert_eq!(panes[0].agent, None);
    assert_eq!(panes[0].agent_session_id, None);
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("target/hc-partial/dead-foreground");
    let ids = ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: &root,
        home_directory: Some(&root),
        agent_config: AgentConfigDirectories::default(),
        cwd: Path::new(env!("CARGO_MANIFEST_DIR")),
        workspace_id: Some("fixture-space"),
        current_pane_id: Some("t1"),
        session_socket_id: "hc:partial",
        host: HcHost::new(&fixture),
        clock: &TestClock,
        ids: &ids,
    };
    assert_ne!(detect_pane_type(&runtime).unwrap(), Some(PaneType::Agent));
}

#[test]
fn concrete_core_identity_preserves_transport_failure() {
    struct Unavailable;
    impl HerdrCommandExecutor for Unavailable {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            assert_eq!(args, ["server", "status", "--json"]);
            Err("hc core is unavailable; start it explicitly".into())
        }
    }
    let host = HcHost::new(Unavailable);
    assert_eq!(
        host.core_identity().unwrap_err().message,
        "hc core is unavailable; start it explicitly"
    );
    assert_eq!(host.socket_id(), None);
}

#[test]
fn discovery_retains_failed_terminal_ids_without_stale_observations() {
    let agent = process::AgentProcess::new();
    let fixture = Fixture::new(agent.pid());
    fixture.unavailable.set(true);
    let discovered = HcHost::new(&fixture)
        .space(SpaceLocator::Workspace("fixture-space"), None)
        .unwrap();
    assert_eq!(discovered.unavailable_panes, ["t1"]);
    assert_eq!(
        discovered
            .panes
            .iter()
            .map(|p| p.pane_id.as_str())
            .collect::<Vec<_>>(),
        ["t2"]
    );
}
