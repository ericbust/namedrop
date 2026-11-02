use super::*;
use crate::{
    ActorKind, AgentRuntime, HerdrCommandExecutor, HerdrCommandOutput, MutationAttribution,
};
use std::cell::Cell;

struct Fixture {
    state: PathBuf,
    rotate_on_workspace_list: Cell<bool>,
    ids: ProcessIds,
}

impl Fixture {
    fn new() -> Self {
        let state = std::env::temp_dir().join(ProcessIds::default().next_id("caller-race"));
        let fixture = Self {
            state,
            rotate_on_workspace_list: Cell::new(false),
            ids: ProcessIds::default(),
        };
        StateStore::open(&fixture.state)
            .unwrap()
            .transaction(|tx| {
                let namespace = namespace();
                let attribution = attribution();
                tx.activate_worktree(&namespace, &attribution)?;
                for name in ["author", "reviewer"] {
                    tx.create_pane_address(
                        &namespace,
                        &crate::PaneAddressDraft {
                            id: PaneAddressId::new(name),
                            role_id: crate::PaneRoleId::new(name),
                            role_name: name.into(),
                            role_description: name.into(),
                            alias: name.into(),
                        },
                        &attribution,
                    )?;
                }
                tx.bind_pane(
                    &namespace,
                    &crate::PaneBindingId::new("binding"),
                    &PaneAddressId::new("author"),
                    "pane-author",
                    &attribution,
                )?;
                tx.observe_agent_session(
                    &namespace,
                    "initial",
                    &PaneAddressId::new("author"),
                    &AgentSessionId::new("session-author"),
                    "working",
                    &attribution,
                )?;
                register(tx, "initial-registration")
            })
            .unwrap();
        fixture
    }

    fn runtime(&self) -> LifecycleRuntime<'_, Self, SystemClock, ProcessIds> {
        LifecycleRuntime {
            state_directory: &self.state,
            home_directory: None,
            agent_config: Default::default(),
            cwd: Path::new("/repo"),
            workspace_id: Some("workspace-1"),
            current_pane_id: Some("pane-author"),
            session_socket_id: "socket",
            herdr: self,
            clock: &SystemClock,
            ids: &self.ids,
        }
    }

    fn validated_caller(&self) -> Caller {
        let caller = stored_caller(&self.state, namespace(), "pane-author".into()).unwrap();
        validate_live_caller(&self.state, &caller, self).unwrap();
        caller
    }

    fn rotate_session(&self) {
        StateStore::open(&self.state)
            .unwrap()
            .transaction(|tx| {
                // Vacating the pane expires its registration. Returning the original
                // session isolates registration freshness from ordinary session checks.
                for session in ["", "replacement-session", "session-author"] {
                    tx.observe_agent_session(
                        &namespace(),
                        session,
                        &PaneAddressId::new("author"),
                        &AgentSessionId::new(session),
                        "working",
                        &attribution(),
                    )?;
                }
                register(tx, "replacement-registration")
            })
            .unwrap();
    }

    fn send(&self, input: Value, caller: Option<&Caller>) -> Result<Value, String> {
        crate::message::execute_message_operation_with_caller(
            "send",
            input,
            &self.runtime(),
            caller,
        )
    }

    fn count(&self, table: &str) -> i64 {
        rusqlite::Connection::open(self.state.join("namedrop.sqlite3"))
            .unwrap()
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.state).unwrap();
    }
}

impl HerdrCommandExecutor for Fixture {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let value = match args {
            ["worktree", "list", ..] => {
                json!({"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}})
            }
            ["workspace", "list"] => {
                if self.rotate_on_workspace_list.replace(false) {
                    self.rotate_session();
                }
                json!({"result":{"workspaces":[{"workspace_id":"workspace-1","label":"local"}]}})
            }
            ["pane", "list", ..] => {
                json!({"result":{"panes":[{"pane_id":"pane-author","terminal_id":"terminal-author","tab_id":"tab-1","workspace_id":"workspace-1","agent_session":{"value":"session-author"},"agent_status":"working"}]}})
            }
            _ => return Err(format!("unexpected Herdr command {args:?}")),
        };
        Ok(HerdrCommandOutput {
            stdout: value.to_string(),
        })
    }
}

fn namespace() -> StateNamespace {
    StateNamespace::for_space("socket", "workspace-1")
}
fn attribution() -> MutationAttribution {
    MutationAttribution::new(ActorKind::Human, "fixture", 1)
}
fn register(tx: &mut crate::StateTransaction<'_>, id: &str) -> Result<(), StoreError> {
    tx.issue_integration_challenge(
        &namespace(),
        id,
        &PaneAddressId::new("author"),
        AgentRuntime::Codex,
        id,
        &attribution(),
    )?;
    tx.complete_integration_registration(
        &namespace(),
        &PaneAddressId::new("author"),
        &AgentSessionId::new("session-author"),
        AgentRuntime::Codex,
        "collab/v1",
        id,
        crate::complete_capability_profile(),
        &attribution(),
    )?;
    Ok(())
}
fn send_input(source: &str) -> Value {
    json!({"request_id":"race-message","source":source,"author_kind":"agent_session"})
}
fn assert_stale(result: Result<Value, String>) {
    assert!(
        result
            .as_ref()
            .is_err_and(|error| error.contains("registration or Agent Session is stale")),
        "stale caller must be rejected, got {result:?}"
    );
}

#[test]
fn replay_rechecks_caller_after_live_validation() {
    let fixture = Fixture::new();
    let input = send_input("@reviewer hello");
    fixture.send(input.clone(), None).unwrap();
    let caller = fixture.validated_caller();
    fixture.rotate_session();
    assert_stale(fixture.send(input.clone(), Some(&caller)));
    assert_eq!(fixture.count("collaboration_messages"), 1);
    let current = fixture.validated_caller();
    assert_ne!(current.registration, caller.registration);
    assert_eq!(current.session, caller.session);
    fixture.send(input, Some(&current)).unwrap();
    assert_eq!(fixture.count("collaboration_messages"), 1);
}

#[test]
fn send_rechecks_caller_after_routing_discovery() {
    let fixture = Fixture::new();
    let caller = fixture.validated_caller();
    fixture.rotate_on_workspace_list.set(true);
    let input = send_input("#local @reviewer hello");
    assert_stale(fixture.send(input.clone(), Some(&caller)));
    assert!(!fixture.rotate_on_workspace_list.get());
    assert_eq!(fixture.count("collaboration_messages"), 0);
    assert_eq!(fixture.count("mailbox_deliveries"), 0);
    fixture
        .send(input, Some(&fixture.validated_caller()))
        .unwrap();
    assert_eq!(fixture.count("collaboration_messages"), 1);
}

#[test]
fn complete_rechecks_caller_after_live_validation() {
    let fixture = Fixture::new();
    StateStore::open(&fixture.state)
        .unwrap()
        .transaction(|tx| {
            tx.create_thread(
                &namespace(),
                &crate::ThreadId::new("thread"),
                "task",
                &attribution(),
            )?;
            tx.append_message(
                &namespace(),
                &crate::MessageDraft {
                    thread_id: crate::ThreadId::new("thread"),
                    message_id: crate::MessageId::new("task"),
                    in_reply_to: None,
                    author_address_id: PaneAddressId::new("reviewer"),
                    author_kind: ActorKind::Human,
                    author_alias_snapshot: "reviewer".into(),
                    author_agent_session_snapshot: None,
                    request_id: "task".into(),
                    authored_source: "@author task".into(),
                    body: "task".into(),
                    grammar_version: "mention/v1".into(),
                    recipients: vec![crate::MessageRecipientDraft {
                        address_id: PaneAddressId::new("author"),
                        alias_snapshot: "author".into(),
                        agent_session_snapshot: Some(AgentSessionId::new("session-author")),
                        delivery_id: crate::DeliveryId::new("delivery"),
                    }],
                    context_references: Vec::new(),
                },
                &attribution(),
            )?;
            Ok(())
        })
        .unwrap();
    StateStore::open(&fixture.state)
        .unwrap()
        .transaction(|tx| {
            let leased = tx.lease_mailbox_head(
                &namespace(),
                &PaneAddressId::new("author"),
                &crate::LeaseDraft {
                    attempt_id: crate::DeliveryAttemptId::new("attempt"),
                    binding_id: crate::PaneBindingId::new("binding"),
                    agent_session_id: AgentSessionId::new("session-author"),
                    retry_of_attempt_id: None,
                    owner: "fixture".into(),
                    acquired_at_ms: 2,
                    expires_at_ms: u64::MAX / 2,
                },
                &attribution(),
            )?;
            assert!(leased.is_some());
            tx.transition_delivery_attempt(
                &namespace(),
                &crate::DeliveryAttemptId::new("attempt"),
                crate::DeliveryState::Submitted,
                &attribution(),
            )?;
            Ok(())
        })
        .unwrap();
    let caller = fixture.validated_caller();
    fixture.rotate_session();
    let input = json!({"attempt_id":"attempt"});
    assert_stale(crate::integration::execute_attempt_operation(
        "complete",
        input.clone(),
        &fixture.runtime(),
        Some(&caller),
    ));
    assert_eq!(fixture.count("delivery_acknowledgements"), 0);
    assert_eq!(fixture.count("delivery_dispositions"), 0);
    assert_eq!(fixture.count("collaboration_messages"), 1);
    crate::integration::execute_attempt_operation(
        "complete",
        input,
        &fixture.runtime(),
        Some(&fixture.validated_caller()),
    )
    .unwrap();
    assert_eq!(fixture.count("delivery_acknowledgements"), 1);
    assert_eq!(fixture.count("delivery_dispositions"), 1);
}
