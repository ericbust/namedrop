#[path = "support/codex_app_server.rs"]
mod codex_app_server;
use clap::Parser;
use namedrop::{
    ActorKind, AgentRuntime, AgentSessionId, Clock, DeliveryAttemptId, DeliveryId, DeliveryState,
    HerdrCommandExecutor, HerdrCommandOutput, IdGenerator, LeaseDraft, LifecycleRuntime,
    MessageDraft, MessageId, MessageRecipientDraft, MutationAttribution, PaneAddressDraft,
    PaneAddressId, PaneAddressLifecycle, PaneBindingId, PaneRoleId, PaneRolePreset, StateNamespace,
    StateStore, StoreErrorKind, ThreadId, WorktreeId, execute_lifecycle_operation,
    execute_message_operation, normalize_alias, package_directory, reconcile_discovered_panes,
    validate_alias,
};
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

struct TestDirectory {
    path: PathBuf,
    home: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        let sequence = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "namedrop-lifecycle-{}-{sequence}",
            std::process::id()
        ));
        fs::create_dir(&path).unwrap();
        let home = path.join("home");
        fs::create_dir(&home).unwrap();
        Self { path, home }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Stands in for the user's home directory, so no test installs an Agent
    /// Integration package into the developer's real user-level skill roots.
    fn home(&self) -> &Path {
        &self.home
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.path).unwrap();
    }
}

fn namespace() -> StateNamespace {
    StateNamespace::for_space("socket-a", "workspace-1")
}

fn attribution(at: u64) -> MutationAttribution {
    MutationAttribution::new(ActorKind::Human, "human", at)
}

fn address_with(id: &str, alias: &str) -> PaneAddressDraft {
    PaneAddressDraft {
        id: PaneAddressId::new(id),
        role_id: PaneRoleId::new(format!("role-{id}")),
        role_name: "Planner".to_owned(),
        role_description:
            "Turns goals and constraints into decisions and ordered, checkable plans.".to_owned(),
        alias: alias.to_owned(),
    }
}

fn address() -> PaneAddressDraft {
    address_with("address-planner", "planner")
}

#[test]
fn name_is_one_positional_cli_command() {
    let cli = namedrop::cli::Cli::try_parse_from(["namedrop", "name", "@reviewer"]).unwrap();

    assert_eq!(cli.operation(), "name");
    assert_eq!(cli.agent_name(), Some("@reviewer"));
}

#[test]
fn name_activates_binds_registers_and_labels_the_current_native_pane() {
    #[derive(Default)]
    struct NameHerdr {
        commands: std::sync::Mutex<Vec<Vec<String>>>,
    }
    impl HerdrCommandExecutor for NameHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
                        .to_owned()
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"] => {
                    r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                        .to_owned()
                }
                ["pane", "list", "--workspace", "workspace-1"] => serde_json::json!({
                    "result": {"panes": [{
                        "pane_id": "pane-1",
                        "terminal_id": "terminal-1",
                        "tab_id": "tab-1",
                        "workspace_id": "workspace-1",
                        "agent": "codex",
                        "display_agent": "Codex",
                        "agent_session": {"value": "session-1"},
                        "agent_status": "idle"
                    }]}
                })
                .to_string(),
                ["pane", "report-metadata", "pane-1", ..] => {
                    self.commands.lock().unwrap().push(
                        args.iter().map(|argument| (*argument).to_owned()).collect(),
                    );
                    "{}".to_owned()
                }
                ["tab", "get", "tab-1"] => {
                    r#"{"result":{"label":"Main"}}"#.to_owned()
                }
                ["tab", "rename", "tab-1", "@reviewer"] => {
                    self.commands.lock().unwrap().push(
                        args.iter().map(|argument| (*argument).to_owned()).collect(),
                    );
                    "{}".to_owned()
                }
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput { stdout })
        }
    }
    struct NameIds(AtomicU64);
    impl IdGenerator for NameIds {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
        }
    }
    struct NameClock;
    impl Clock for NameClock {
        fn now_unix_ms(&self) -> u64 {
            100
        }
        fn wait_ms(&self, _duration_ms: u64) {}
    }

    let directory = TestDirectory::new();
    let herdr = NameHerdr::default();
    let ids = NameIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-1"),
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&herdr),
        clock: &NameClock,
        ids: &ids,
    };

    let result =
        execute_lifecycle_operation("name", serde_json::json!({"alias": "@reviewer"}), &runtime)
            .unwrap();

    assert_eq!(result["alias"], "@reviewer");
    assert_eq!(result["ready"], true);
    assert_eq!(result["registered"], true);
    assert_eq!(
        StateStore::open(directory.path())
            .unwrap()
            .transaction(|transaction| transaction.list_pane_addresses(&namespace()))
            .unwrap()[0]
            .alias,
        "reviewer"
    );
    let commands = herdr.commands.lock().unwrap();
    assert!(commands.iter().any(|command| {
        command.iter().any(|argument| argument == "@reviewer")
            && command
                .iter()
                .any(|argument| argument == "working=@reviewer working")
    }));
    assert!(
        commands
            .iter()
            .any(|command| { command == &["tab", "rename", "tab-1", "@reviewer"] })
    );
}

#[test]
fn name_leaves_a_tab_shared_by_two_agents_unchanged() {
    struct MultiPaneHerdr;
    impl HerdrCommandExecutor for MultiPaneHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
                        .to_owned()
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"] => {
                    r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                        .to_owned()
                }
                ["pane", "list", "--workspace", "workspace-1"] => serde_json::json!({
                    "result": {"panes": [
                        {
                            "pane_id": "pane-1",
                            "terminal_id": "terminal-1",
                            "tab_id": "tab-1",
                            "workspace_id": "workspace-1",
                            "agent": "codex",
                            "agent_session": {"value": "session-1"},
                            "agent_status": "idle"
                        },
                        {
                            "pane_id": "pane-2",
                            "terminal_id": "terminal-2",
                            "tab_id": "tab-1",
                            "workspace_id": "workspace-1",
                            "agent": "claude",
                            "agent_status": "idle"
                        }
                    ]}
                })
                .to_string(),
                ["pane", "report-metadata", "pane-1", ..] => "{}".to_owned(),
                ["tab", "rename", ..] => panic!("a tab shared by two agents must keep its label"),
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput { stdout })
        }
    }
    struct MultiPaneIds(AtomicU64);
    impl IdGenerator for MultiPaneIds {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
        }
    }
    struct MultiPaneClock;
    impl Clock for MultiPaneClock {
        fn now_unix_ms(&self) -> u64 {
            100
        }
        fn wait_ms(&self, _duration_ms: u64) {}
    }

    let directory = TestDirectory::new();
    let ids = MultiPaneIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-1"),
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&MultiPaneHerdr),
        clock: &MultiPaneClock,
        ids: &ids,
    };

    let result =
        execute_lifecycle_operation("name", serde_json::json!({"alias": "reviewer"}), &runtime)
            .unwrap();

    assert_eq!(result["alias"], "@reviewer");
}

#[test]
fn name_succeeds_with_a_warning_when_the_cosmetic_tab_label_cannot_be_applied() {
    struct RenameFailureHerdr {
        database_path: PathBuf,
        saw_durable_intent: Mutex<bool>,
    }
    impl HerdrCommandExecutor for RenameFailureHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"] => {
                    r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                }
                ["pane", "list", "--workspace", "workspace-1"] => {
                    r#"{"result":{"panes":[{"pane_id":"pane-1","terminal_id":"terminal-1","tab_id":"tab-1","workspace_id":"workspace-1","agent":"codex","agent_session":{"value":"session-1"},"agent_status":"idle"}]}}"#
                }
                ["pane", "report-metadata", "pane-1", ..] => "{}",
                ["tab", "get", "tab-1"] => r#"{"result":{"label":"Main"}}"#,
                ["tab", "rename", "tab-1", "@reviewer"] => {
                    *self.saw_durable_intent.lock().unwrap() =
                        Connection::open(&self.database_path)
                            .unwrap()
                            .query_row(
                                "SELECT pending_label = '@reviewer'
                             FROM managed_tab_labels WHERE tab_id = 'tab-1'",
                                [],
                                |row| row.get(0),
                            )
                            .unwrap();
                    return Err("tab was closed during rename".to_owned());
                }
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput {
                stdout: stdout.to_owned(),
            })
        }
    }

    struct RenameFailureIds(AtomicU64);
    impl IdGenerator for RenameFailureIds {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
        }
    }
    struct RenameFailureClock;
    impl Clock for RenameFailureClock {
        fn now_unix_ms(&self) -> u64 {
            100
        }
        fn wait_ms(&self, _duration_ms: u64) {}
    }

    let directory = TestDirectory::new();
    let herdr = RenameFailureHerdr {
        database_path: directory.path().join("namedrop.sqlite3"),
        saw_durable_intent: Mutex::new(false),
    };
    let ids = RenameFailureIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-1"),
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&herdr),
        clock: &RenameFailureClock,
        ids: &ids,
    };

    let result =
        execute_lifecycle_operation("name", serde_json::json!({"alias": "@reviewer"}), &runtime)
            .expect("durable naming must not fail because a cosmetic host label failed");

    assert_eq!(result["alias"], "@reviewer");
    assert!(
        *herdr.saw_durable_intent.lock().unwrap(),
        "the recoverable label intent must commit before the host mutation"
    );
    assert_eq!(result["tab_label"]["status"], "warning");
    assert_eq!(
        result["tab_label"]["message"],
        "Pane named, but its tab label could not be updated."
    );
    assert_eq!(
        StateStore::open(directory.path())
            .unwrap()
            .transaction(|transaction| transaction.list_pane_addresses(&namespace()))
            .unwrap()[0]
            .alias,
        "reviewer"
    );
}

#[test]
fn managed_singleton_tab_labels_restore_only_while_the_plugin_still_owns_them() {
    struct ManagedLabelHerdr {
        split: Mutex<bool>,
        label: Mutex<String>,
        database_path: PathBuf,
        saw_restore_intent: Mutex<bool>,
    }
    impl HerdrCommandExecutor for ManagedLabelHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#.to_owned()
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"] => {
                    r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#.to_owned()
                }
                ["pane", "list", "--workspace", "workspace-1"] => {
                    let mut panes = vec![serde_json::json!({
                        "pane_id": "pane-1",
                        "terminal_id": "terminal-1",
                        "tab_id": "tab-1",
                        "workspace_id": "workspace-1",
                        "agent": "codex",
                        "agent_session": {"value": "session-1"},
                        "agent_status": "idle"
                    })];
                    if *self.split.lock().unwrap() {
                        panes.push(serde_json::json!({
                            "pane_id": "pane-2",
                            "terminal_id": "terminal-2",
                            "tab_id": "tab-1",
                            "workspace_id": "workspace-1",
                            "agent": "claude",
                            "agent_status": "idle"
                        }));
                    }
                    serde_json::json!({"result": {"panes": panes}}).to_string()
                }
                ["pane", "report-metadata", "pane-1", ..] => "{}".to_owned(),
                ["tab", "get", "tab-1"] => serde_json::json!({
                    "result": {"label": self.label.lock().unwrap().clone()}
                })
                .to_string(),
                ["tab", "rename", "tab-1", label] => {
                    if *label == "My review tab" {
                        *self.saw_restore_intent.lock().unwrap() =
                            Connection::open(&self.database_path)
                                .unwrap()
                                .query_row(
                                    "SELECT pending_label = 'My review tab'
                                     FROM managed_tab_labels WHERE tab_id = 'tab-1'",
                                    [],
                                    |row| row.get(0),
                                )
                                .unwrap();
                    }
                    *self.label.lock().unwrap() = (*label).to_owned();
                    "{}".to_owned()
                }
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput { stdout })
        }
    }
    struct ManagedLabelIds(AtomicU64);
    impl IdGenerator for ManagedLabelIds {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
        }
    }
    struct ManagedLabelClock;
    impl Clock for ManagedLabelClock {
        fn now_unix_ms(&self) -> u64 {
            100
        }
        fn wait_ms(&self, _duration_ms: u64) {}
    }

    let directory = TestDirectory::new();
    let herdr = ManagedLabelHerdr {
        split: Mutex::new(false),
        label: Mutex::new("Review".to_owned()),
        database_path: directory.path().join("namedrop.sqlite3"),
        saw_restore_intent: Mutex::new(false),
    };
    let ids = ManagedLabelIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-1"),
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&herdr),
        clock: &ManagedLabelClock,
        ids: &ids,
    };

    execute_lifecycle_operation("name", serde_json::json!({"alias": "reviewer"}), &runtime)
        .unwrap();
    assert_eq!(*herdr.label.lock().unwrap(), "@reviewer");

    *herdr.label.lock().unwrap() = "My review tab".to_owned();
    *herdr.split.lock().unwrap() = true;
    execute_lifecycle_operation("reconcile", serde_json::json!({}), &runtime).unwrap();
    assert_eq!(
        *herdr.label.lock().unwrap(),
        "My review tab",
        "a human label change revokes plugin ownership"
    );

    *herdr.split.lock().unwrap() = false;
    execute_lifecycle_operation("name", serde_json::json!({"alias": "reviewer"}), &runtime)
        .unwrap();
    *herdr.split.lock().unwrap() = true;
    execute_lifecycle_operation("reconcile", serde_json::json!({}), &runtime).unwrap();
    assert_eq!(
        *herdr.label.lock().unwrap(),
        "My review tab",
        "a still-owned label restores the label it replaced when another agent joins"
    );
    assert!(
        *herdr.saw_restore_intent.lock().unwrap(),
        "the restore intent must commit before the host mutation"
    );

    *herdr.split.lock().unwrap() = false;
    execute_lifecycle_operation("reconcile", serde_json::json!({}), &runtime).unwrap();
    assert_eq!(
        *herdr.label.lock().unwrap(),
        "@reviewer",
        "the alias returns once the named agent is alone in its tab again"
    );
}

#[test]
fn name_waits_for_herdr_to_resolve_the_agent_session_then_registers() {
    assert_native_session_events(&[(
        Some("session-1"),
        serde_json::json!({
            "type": "pane_agent_status_changed",
            "pane_id": "pane-1",
            "workspace_id": "workspace-1",
            "agent_status": "idle",
            "agent": "codex",
            "display_agent": "Codex",
            "state_labels": {}
        }),
    )]);
}

#[test]
fn native_agent_detection_registers_and_releases_discovered_sessions() {
    // Herdr omits released=false, final_status and agent_status on detection.
    let detected = serde_json::json!({
        "type": "pane_agent_detected",
        "pane_id": "pane-1",
        "workspace_id": "workspace-1",
        "agent": "codex"
    });
    let released = serde_json::json!({
        "type": "pane_agent_detected",
        "pane_id": "pane-1",
        "workspace_id": "workspace-1",
        "released": true,
        "final_status": "done"
    });
    assert_native_session_events(&[
        (None, detected.clone()),
        (Some("session-1"), detected),
        // A delayed release must observe the replacement session, not clear it.
        (Some("session-2"), released.clone()),
        (None, released),
    ]);
}

fn assert_native_session_events(events: &[(Option<&'static str>, serde_json::Value)]) {
    use std::sync::Mutex;

    struct PendingSessionHerdr {
        session: Mutex<Option<&'static str>>,
    }
    impl HerdrCommandExecutor for PendingSessionHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
                        .to_owned()
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"]
                | ["worktree", "list", "--workspace", "workspace-1", "--json"] => {
                    r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                        .to_owned()
                }
                ["pane", "list", "--workspace", "workspace-1"] => {
                    let mut pane = serde_json::json!({
                        "pane_id": "pane-1",
                        "terminal_id": "terminal-1",
                        "tab_id": "tab-1",
                        "workspace_id": "workspace-1",
                        "agent": "codex",
                        "display_agent": "Codex",
                        "agent_status": "idle"
                    });
                    if let Some(session) = *self.session.lock().unwrap() {
                        pane["agent_session"] = serde_json::json!({"value": session});
                    }
                    serde_json::json!({"result": {"panes": [pane]}}).to_string()
                }
                ["pane", "report-metadata", "pane-1", ..] => "{}".to_owned(),
                ["tab", "get", "tab-1"] => {
                    r#"{"result":{"label":"Main"}}"#.to_owned()
                }
                ["tab", "rename", "tab-1", "@builder"] => "{}".to_owned(),
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput { stdout })
        }
    }
    struct PendingSessionIds(AtomicU64);
    impl IdGenerator for PendingSessionIds {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
        }
    }
    struct PendingSessionClock;
    impl Clock for PendingSessionClock {
        fn now_unix_ms(&self) -> u64 {
            100
        }
    }

    let directory = TestDirectory::new();
    let herdr = PendingSessionHerdr {
        session: Mutex::new(None),
    };
    let ids = PendingSessionIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-1"),
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&herdr),
        clock: &PendingSessionClock,
        ids: &ids,
    };

    let named =
        execute_lifecycle_operation("name", serde_json::json!({"alias": "builder"}), &runtime)
            .unwrap();
    assert_eq!(named["alias"], "@builder");
    assert_eq!(named["registered"], false);
    assert_eq!(named["ready"], false);
    assert_eq!(named["agent_session_id"], serde_json::Value::Null);
    assert_eq!(named["codex_app_server"], serde_json::Value::Null);
    assert!(
        named["native_session_setup"]["explanation"]
            .as_str()
            .unwrap()
            .contains("first prompt")
    );
    assert!(
        named["native_session_setup"]["explanation"]
            .as_str()
            .unwrap()
            .contains("queued")
    );
    assert_eq!(
        named["native_session_setup"]["command"],
        serde_json::json!(["herdr", "integration", "install", "codex"])
    );

    for (session, event) in events {
        *herdr.session.lock().unwrap() = *session;
        let report =
            execute_lifecycle_operation("event.agent-status-changed", event.clone(), &runtime)
                .unwrap();
        assert_eq!(report["handled"], true);
        let addresses =
            execute_lifecycle_operation("address.list", serde_json::json!({}), &runtime).unwrap();
        assert_eq!(addresses[0]["alias"], "builder");
        assert_eq!(addresses[0]["agent_session_id"], serde_json::json!(session));
        assert_eq!(
            addresses[0]["lifecycle"],
            if session.is_some() {
                "occupied"
            } else {
                "empty"
            }
        );
        if let Some(session) = session {
            let address_id: PaneAddressId =
                serde_json::from_value(addresses[0]["id"].clone()).unwrap();
            let registered = StateStore::open(directory.path())
                .unwrap()
                .transaction(|transaction| {
                    transaction.active_integration_registration(
                        &namespace(),
                        &address_id,
                        &AgentSessionId::new(*session),
                    )
                })
                .unwrap()
                .is_some();
            assert!(registered, "{session}");
        }
    }
}

#[test]
fn name_upgrades_an_existing_address_to_zero_turn_registration() {
    #[derive(Default)]
    struct UpgradeHerdr {
        prompts: std::sync::Mutex<Vec<String>>,
    }
    impl HerdrCommandExecutor for UpgradeHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
                        .to_owned()
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"] => {
                    r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                        .to_owned()
                }
                ["pane", "list", "--workspace", "workspace-1"] => serde_json::json!({
                    "result": {"panes": [{
                        "pane_id": "pane-1",
                        "terminal_id": "terminal-1",
                        "tab_id": "tab-1",
                        "workspace_id": "workspace-1",
                        "agent": "codex",
                        "agent_session": {"value": "session-1"},
                        "agent_status": "idle"
                    }]}
                })
                .to_string(),
                ["agent", "prompt", "pane-1", prompt] => {
                    self.prompts.lock().unwrap().push((*prompt).to_owned());
                    "{}".to_owned()
                }
                ["pane", "report-metadata", "pane-1", ..] => "{}".to_owned(),
                ["tab", "get", "tab-1"] => {
                    r#"{"result":{"label":"Main"}}"#.to_owned()
                }
                ["tab", "rename", "tab-1", "@reviewer"] => "{}".to_owned(),
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput { stdout })
        }
    }
    struct UpgradeIds(AtomicU64);
    impl IdGenerator for UpgradeIds {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
        }
    }
    struct UpgradeClock;
    impl Clock for UpgradeClock {
        fn now_unix_ms(&self) -> u64 {
            100
        }
        fn wait_ms(&self, _duration_ms: u64) {}
    }

    let directory = TestDirectory::new();
    let herdr = UpgradeHerdr::default();
    let ids = UpgradeIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-1"),
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&herdr),
        clock: &UpgradeClock,
        ids: &ids,
    };

    execute_lifecycle_operation("activate", serde_json::json!({"confirmed": true}), &runtime)
        .unwrap();
    execute_lifecycle_operation(
        "address.create",
        serde_json::json!({"preset": "reviewer", "alias": "old-reviewer"}),
        &runtime,
    )
    .unwrap();
    assert_eq!(herdr.prompts.lock().unwrap().len(), 1);

    let result =
        execute_lifecycle_operation("name", serde_json::json!({"alias": "reviewer"}), &runtime)
            .unwrap();

    assert_eq!(result["alias"], "@reviewer");
    assert_eq!(result["registered"], true);
    assert_eq!(
        herdr.prompts.lock().unwrap().len(),
        1,
        "name must not spend another agent turn on registration"
    );
}

#[test]
fn pane_role_presets_have_the_approved_identity_and_aliases() {
    let cases = [
        (
            PaneRolePreset::Planner,
            "Planner",
            "Turns goals and constraints into decisions and ordered, checkable plans.",
            "@planner",
        ),
        (
            PaneRolePreset::Implementer,
            "Implementer",
            "Carries out agreed work in small, verifiable steps and reports the results.",
            "@implementer",
        ),
        (
            PaneRolePreset::Reviewer,
            "Reviewer",
            "Independently checks proposed or completed work for correctness, intent, and risk.",
            "@reviewer",
        ),
        (
            PaneRolePreset::Researcher,
            "Researcher",
            "Investigates repository and connected knowledge sources, cross-checks evidence, and prepares durable findings for human-approved publication.",
            "@researcher",
        ),
    ];

    for (preset, name, description, alias) in cases {
        let role = preset.role();
        assert_eq!(role.name, name);
        assert_eq!(role.description, description);
        assert_eq!(role.suggested_alias, alias);
    }

    assert_eq!(normalize_alias("Code Reviewer").unwrap(), "code-reviewer");
    assert!(normalize_alias("!!!").is_err());
    assert!(validate_alias(&"a".repeat(10_000)).is_ok());
}

#[test]
fn explicit_deactivation_and_reactivation_preserve_worktree_state() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let scope = namespace();

    store
        .transaction(|transaction| {
            transaction.activate_worktree(&scope, &attribution(1))?;
            transaction.create_pane_address(&scope, &address(), &attribution(2))?;
            transaction.deactivate_worktree(&scope, &attribution(3))
        })
        .unwrap();

    store
        .transaction(|transaction| {
            let deactivated = transaction.worktree_state(&scope)?.unwrap();
            assert!(!deactivated.activated);
            assert!(deactivated.deactivated);
            assert_eq!(transaction.list_pane_addresses(&scope)?.len(), 1);

            transaction.activate_worktree(&scope, &attribution(4))?;
            let reactivated = transaction.worktree_state(&scope)?.unwrap();
            assert!(reactivated.activated);
            assert!(!reactivated.deactivated);
            assert_eq!(transaction.list_pane_addresses(&scope)?.len(), 1);
            Ok(())
        })
        .unwrap();
}

#[test]
fn restart_reconciliation_refreshes_panes_before_delivery_drain() {
    struct CountingHerdr {
        pane_lists: AtomicU64,
    }

    impl HerdrCommandExecutor for CountingHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"] => {
                    r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                }
                ["pane", "list", "--workspace", "workspace-1"] => {
                    self.pane_lists.fetch_add(1, Ordering::Relaxed);
                    r#"{"result":{"panes":[]}}"#
                }
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput {
                stdout: stdout.to_owned(),
            })
        }
    }

    struct FixedClock;
    impl Clock for FixedClock {
        fn now_unix_ms(&self) -> u64 {
            42
        }
    }

    struct FixedIds;
    impl IdGenerator for FixedIds {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-restart")
        }
    }

    let directory = TestDirectory::new();
    StateStore::open(directory.path())
        .unwrap()
        .transaction(|transaction| transaction.activate_worktree(&namespace(), &attribution(1)))
        .unwrap();
    let herdr = CountingHerdr {
        pane_lists: AtomicU64::new(0),
    };
    let runtime = namedrop::LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: None,
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&herdr),
        clock: &FixedClock,
        ids: &FixedIds,
    };

    execute_lifecycle_operation("reconcile", serde_json::json!({}), &runtime).unwrap();

    assert_eq!(
        herdr.pane_lists.load(Ordering::Relaxed),
        2,
        "initial discovery and delivery drain must each observe host state"
    );
}

#[test]
fn activation_keeps_dispatch_gated_when_initial_reconciliation_fails() {
    struct FakeHerdr;
    impl HerdrCommandExecutor for FakeHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"] => {
                    r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                }
                ["pane", "list", "--workspace", "workspace-1"] => r#"{"result":{"panes":[]}}"#,
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput {
                stdout: stdout.to_owned(),
            })
        }
    }
    struct FixedClock;
    impl Clock for FixedClock {
        fn now_unix_ms(&self) -> u64 {
            42
        }
    }
    struct FixedIds;
    impl IdGenerator for FixedIds {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-activation-failure")
        }
    }

    fn install_failure_trigger(directory: &TestDirectory) {
        Connection::open(directory.path().join("namedrop.sqlite3"))
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER fail_complete_reconciliation
                 BEFORE UPDATE OF epoch, completed ON reconciliation_epochs
                 WHEN NEW.completed = 1
                 BEGIN
                     SELECT RAISE(ABORT, 'injected reconciliation failure');
                 END;",
            )
            .unwrap();
    }

    fn completed(directory: &TestDirectory) -> bool {
        Connection::open(directory.path().join("namedrop.sqlite3"))
            .unwrap()
            .query_row(
                "SELECT completed FROM reconciliation_epochs
                 WHERE session_socket_id = 'socket-a' AND worktree_id = 'workspace-1'",
                [],
                |row| row.get(0),
            )
            .unwrap()
    }

    let first = TestDirectory::new();
    drop(StateStore::open(first.path()).unwrap());
    install_failure_trigger(&first);
    let first_runtime = namedrop::LifecycleRuntime {
        state_directory: first.path(),
        home_directory: Some(first.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: None,
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&FakeHerdr),
        clock: &FixedClock,
        ids: &FixedIds,
    };
    assert!(
        execute_lifecycle_operation(
            "activate",
            serde_json::json!({"confirmed": true}),
            &first_runtime,
        )
        .is_err()
    );
    assert!(!completed(&first));

    let reactivated = TestDirectory::new();
    StateStore::open(reactivated.path())
        .unwrap()
        .transaction(|transaction| {
            transaction.activate_worktree(&namespace(), &attribution(1))?;
            transaction.complete_reconciliation(&namespace())?;
            transaction.deactivate_worktree(&namespace(), &attribution(2))
        })
        .unwrap();
    install_failure_trigger(&reactivated);
    let reactivation_runtime = namedrop::LifecycleRuntime {
        state_directory: reactivated.path(),
        home_directory: Some(reactivated.home()),
        ..first_runtime
    };
    assert!(
        execute_lifecycle_operation(
            "activate",
            serde_json::json!({"confirmed": true}),
            &reactivation_runtime,
        )
        .is_err()
    );
    assert!(!completed(&reactivated));
}

#[test]
fn address_registry_tracks_unbound_empty_and_occupied_lifecycle() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let scope = namespace();

    store
        .transaction(|transaction| {
            transaction.activate_worktree(&scope, &attribution(1))?;
            transaction.create_pane_address(&scope, &address(), &attribution(2))?;
            let unbound = transaction.list_pane_addresses(&scope)?.remove(0);
            assert_eq!(unbound.lifecycle, PaneAddressLifecycle::Unbound);
            assert_eq!(unbound.pane_id, None);

            transaction.bind_pane(
                &scope,
                &PaneBindingId::new("binding-1"),
                &PaneAddressId::new("address-planner"),
                "pane-1",
                &attribution(3),
            )?;
            let empty = transaction.list_pane_addresses(&scope)?.remove(0);
            assert_eq!(empty.lifecycle, PaneAddressLifecycle::Empty);
            assert_eq!(empty.pane_id.as_deref(), Some("pane-1"));

            transaction.observe_agent_session(
                &scope,
                "observation-1",
                &PaneAddressId::new("address-planner"),
                &AgentSessionId::new("session-1"),
                "working",
                &attribution(4),
            )?;
            let occupied = transaction.list_pane_addresses(&scope)?.remove(0);
            assert_eq!(occupied.lifecycle, PaneAddressLifecycle::Occupied);
            assert_eq!(
                occupied.agent_session_id,
                Some(AgentSessionId::new("session-1"))
            );

            transaction.unbind_pane(
                &scope,
                &PaneAddressId::new("address-planner"),
                &attribution(5),
            )?;
            let unbound_again = transaction.list_pane_addresses(&scope)?.remove(0);
            assert_eq!(unbound_again.lifecycle, PaneAddressLifecycle::Unbound);
            assert_eq!(unbound_again.agent_session_id, None);
            Ok(())
        })
        .unwrap();
}

#[test]
fn rename_release_and_retirement_preserve_identity_boundaries() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let scope = namespace();

    store
        .transaction(|transaction| {
            transaction.activate_worktree(&scope, &attribution(1))?;
            transaction.create_pane_address(&scope, &address(), &attribution(2))?;
            transaction.replace_alias(
                &scope,
                &PaneAddressId::new("address-planner"),
                "lead-planner",
                &attribution(3),
            )
        })
        .unwrap();

    let reserved = store.transaction(|transaction| {
        transaction.create_pane_address(
            &scope,
            &address_with("address-second", "planner"),
            &attribution(4),
        )
    });
    assert_eq!(
        reserved.unwrap_err().kind(),
        StoreErrorKind::InvalidInput,
        "a renamed-away alias stays reserved until it is released"
    );

    store
        .transaction(|transaction| {
            transaction.release_alias(&scope, "planner", &attribution(5))?;
            transaction.create_pane_address(
                &scope,
                &address_with("address-second", "planner"),
                &attribution(6),
            )?;
            transaction.retire_pane_address(
                &scope,
                &PaneAddressId::new("address-planner"),
                &attribution(7),
            )?;
            transaction.release_alias(&scope, "lead-planner", &attribution(8))?;
            transaction.create_pane_address(
                &scope,
                &address_with("address-third", "lead-planner"),
                &attribution(9),
            )?;
            let visible = transaction.list_pane_addresses(&scope)?;
            assert_eq!(visible.len(), 2);
            Ok(())
        })
        .unwrap();

    let invalid_alias = store.transaction(|transaction| {
        transaction.create_pane_address(
            &scope,
            &address_with("address-invalid", "Not Valid"),
            &attribution(10),
        )
    });
    assert_eq!(
        invalid_alias.unwrap_err().kind(),
        StoreErrorKind::InvalidInput
    );
}

#[test]
fn herdr_discovery_and_restart_reconciliation_fail_closed() {
    struct FakeHerdr;
    impl HerdrCommandExecutor for FakeHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"] => {
                    r#"{"result":{"source":{"source_workspace_id":"workspace-1"},"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                }
                ["pane", "list", "--workspace", "workspace-1"] => {
                    r#"{"result":{"panes":[{"pane_id":"pane-live","terminal_id":"terminal-live","tab_id":"tab-live","workspace_id":"workspace-1","cwd":"/repo","agent_session":{"value":"session-live"},"agent_status":"idle"}]}}"#
                }
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput {
                stdout: stdout.to_owned(),
            })
        }
    }

    struct SequenceIds;
    impl IdGenerator for SequenceIds {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-reconcile")
        }
    }

    let discovered = namedrop::Host::space(
        &namedrop::TuiHost::new(&FakeHerdr),
        namedrop::SpaceLocator::Checkout(Path::new("/repo")),
        None,
    )
    .map_err(String::from)
    .unwrap();
    assert_eq!(discovered.worktree_id, WorktreeId::new("/repo"));
    assert_eq!(discovered.workspace_id, "workspace-1");
    assert_eq!(discovered.panes.len(), 1);
    assert_eq!(discovered.panes[0].terminal_id, "terminal-live");
    assert_eq!(discovered.panes[0].tab_id, "tab-live");
    assert_eq!(discovered.panes[0].workspace_id, "workspace-1");

    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let scope = namespace();
    store
        .transaction(|transaction| {
            transaction.activate_worktree(&scope, &attribution(1))?;
            transaction.create_pane_address(
                &scope,
                &address_with("address-live", "live"),
                &attribution(2),
            )?;
            transaction.create_pane_address(
                &scope,
                &address_with("address-missing", "missing"),
                &attribution(3),
            )?;
            transaction.bind_pane(
                &scope,
                &PaneBindingId::new("binding-live"),
                &PaneAddressId::new("address-live"),
                "pane-live",
                &attribution(4),
            )?;
            transaction.bind_pane(
                &scope,
                &PaneBindingId::new("binding-missing"),
                &PaneAddressId::new("address-missing"),
                "pane-missing",
                &attribution(5),
            )?;
            reconcile_discovered_panes(
                transaction,
                &scope,
                &discovered.panes,
                &SequenceIds,
                &attribution(6),
            )
        })
        .unwrap();

    store
        .transaction(|transaction| {
            let addresses = transaction.list_pane_addresses(&scope)?;
            let live = addresses
                .iter()
                .find(|record| record.id == PaneAddressId::new("address-live"))
                .unwrap();
            let missing = addresses
                .iter()
                .find(|record| record.id == PaneAddressId::new("address-missing"))
                .unwrap();
            assert_eq!(live.lifecycle, PaneAddressLifecycle::Occupied);
            assert_eq!(
                live.agent_session_id,
                Some(AgentSessionId::new("session-live"))
            );
            assert_eq!(missing.lifecycle, PaneAddressLifecycle::Unbound);
            Ok(())
        })
        .unwrap();

    let duplicate = discovered.panes[0].clone();
    store
        .transaction(|transaction| {
            transaction.rebind_pane(
                &scope,
                &PaneBindingId::new("binding-live-again"),
                &PaneAddressId::new("address-live"),
                "pane-live",
                &attribution(7),
            )?;
            reconcile_discovered_panes(
                transaction,
                &scope,
                &[duplicate.clone(), duplicate],
                &SequenceIds,
                &attribution(8),
            )?;
            let live = transaction
                .list_pane_addresses(&scope)?
                .into_iter()
                .find(|record| record.id == PaneAddressId::new("address-live"))
                .unwrap();
            assert_eq!(live.lifecycle, PaneAddressLifecycle::Unbound);
            Ok(())
        })
        .unwrap();
}

#[test]
fn workspace_discovery_rejects_successful_snapshots_that_omit_the_requested_space() {
    struct MismatchedHerdr;
    impl HerdrCommandExecutor for MismatchedHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            match args {
                ["worktree", "list", "--workspace", "space-a", "--json"] => {
                    Ok(HerdrCommandOutput {
                        stdout: r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"space-b"}]}}"#.to_owned(),
                    })
                }
                _ => panic!("pane discovery must not run after a mismatched snapshot: {args:?}"),
            }
        }
    }

    let error = namedrop::Host::space(
        &namedrop::TuiHost::new(&MismatchedHerdr),
        namedrop::SpaceLocator::Workspace("space-a"),
        None,
    )
    .map_err(String::from)
    .unwrap_err();
    assert!(error.contains("does not contain space space-a"), "{error}");
}

#[test]
fn workspace_discovery_attaches_a_second_space_on_the_source_checkout() {
    struct SharedCheckoutHerdr;
    impl HerdrCommandExecutor for SharedCheckoutHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            match args {
                ["worktree", "list", "--workspace", "space-b", "--json"] => {
                    Ok(HerdrCommandOutput {
                        stdout: r#"{"result":{"source":{"repo_key":"/repo/.git","repo_name":"repo","repo_root":"/repo","source_checkout_path":"/repo","source_workspace_id":"space-b"},"worktrees":[{"path":"/repo","open_workspace_id":"space-a","label":"repo","branch":"main","is_linked_worktree":false}]}}"#.to_owned(),
                    })
                }
                ["pane", "list", "--workspace", "space-b"] => Ok(HerdrCommandOutput {
                    stdout: r#"{"result":{"panes":[{"pane_id":"space-b:pane-1","terminal_id":"terminal-1","tab_id":"tab-1","workspace_id":"space-b"}]}}"#.to_owned(),
                }),
                _ => panic!("unexpected Herdr command: {args:?}"),
            }
        }
    }

    let discovered = namedrop::Host::space(
        &namedrop::TuiHost::new(&SharedCheckoutHerdr),
        namedrop::SpaceLocator::Workspace("space-b"),
        None,
    )
    .map_err(String::from)
    .unwrap();
    assert_eq!(discovered.workspace_id, "space-b");
    assert_eq!(discovered.worktree_id, WorktreeId::new("/repo"));
    assert!(discovered.worktree_attached);
    assert_eq!(discovered.label.as_deref(), Some("repo"));
    assert_eq!(discovered.branch.as_deref(), Some("main"));
    assert_eq!(discovered.panes.len(), 1);
    assert_eq!(discovered.panes[0].pane_id, "space-b:pane-1");
    assert_eq!(discovered.panes[0].workspace_id, "space-b");
}

#[test]
fn workspace_discovery_keeps_failing_closed_when_source_names_another_space() {
    struct LinkedDuplicateHerdr;
    impl HerdrCommandExecutor for LinkedDuplicateHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            match args {
                ["worktree", "list", "--workspace", "space-b", "--json"] => {
                    Ok(HerdrCommandOutput {
                        stdout: r#"{"result":{"source":{"source_checkout_path":"/repo","source_workspace_id":"space-c"},"worktrees":[{"path":"/repo","open_workspace_id":"space-c","label":"repo","branch":"main"},{"path":"/repo-feature","open_workspace_id":"space-a","label":"repo-feature","branch":"feature","is_linked_worktree":true}]}}"#.to_owned(),
                    })
                }
                _ => panic!(
                    "pane discovery must not run for a linked-worktree duplicate: {args:?}"
                ),
            }
        }
    }

    let error = namedrop::Host::space(
        &namedrop::TuiHost::new(&LinkedDuplicateHerdr),
        namedrop::SpaceLocator::Workspace("space-b"),
        None,
    )
    .map_err(String::from)
    .unwrap_err();
    assert!(error.contains("does not contain space space-b"), "{error}");
}

#[test]
fn agent_session_lookup_needs_one_exact_herdr_pane() {
    struct PanesHerdr;
    impl HerdrCommandExecutor for PanesHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            assert_eq!(args, ["pane", "list"]);
            let pane = |space: &str, pane: &str, session: &str| {
                serde_json::json!({
                    "pane_id": pane, "terminal_id": "t", "tab_id": "tab", "workspace_id": space,
                    "agent_session": {"value": session}
                })
            };
            Ok(HerdrCommandOutput {
                stdout: serde_json::json!({"result": {"panes": [
                    pane("w1", "w1:p1", "solo"),
                    pane("w2", "w2:p1", "twice"),
                    pane("w3", "w3:p1", "twice"),
                ]}})
                .to_string(),
            })
        }
    }

    assert_eq!(
        namedrop::locate_agent_session_pane(&namedrop::TuiHost::new(&PanesHerdr), "solo"),
        Some(("w1".to_owned(), "w1:p1".to_owned()))
    );
    assert_eq!(
        namedrop::locate_agent_session_pane(&namedrop::TuiHost::new(&PanesHerdr), "twice"),
        None
    );
    assert_eq!(
        namedrop::locate_agent_session_pane(&namedrop::TuiHost::new(&PanesHerdr), "missing"),
        None
    );
}

/// Herdr with checkout `/repo` (of repository `/repo/.git`, linked when
/// `linked`, unreported when `None`) first opened by `opener`. Each space in `stamped` carries
/// Herdr's `worktree.checkout_path` in `workspace list`; each space in
/// `listings` answers `worktree list --workspace` instead.
struct SpacesHerdr {
    opener: &'static str,
    linked: Option<bool>,
    stamped: Vec<(&'static str, &'static str)>,
    listings: Vec<(&'static str, Result<&'static str, &'static str>)>,
}

impl HerdrCommandExecutor for SpacesHerdr {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let output = |stdout: String| Ok(HerdrCommandOutput { stdout });
        match args {
            ["worktree", "list", "--cwd", "/repo", "--json"] => {
                let mut checkout =
                    serde_json::json!({"path": "/repo", "open_workspace_id": self.opener});
                if let Some(linked) = self.linked {
                    checkout["is_linked_worktree"] = linked.into();
                }
                output(
                    serde_json::json!({"result": {"source": {"repo_key": "/repo/.git",
                        "source_checkout_path": "/repo", "source_workspace_id": self.opener},
                        "worktrees": [checkout]}})
                    .to_string(),
                )
            }
            ["workspace", "list"] => {
                let spaces = std::iter::once(serde_json::json!({"workspace_id": self.opener}))
                    .chain(self.stamped.iter().map(|(id, checkout)| {
                        serde_json::json!({"workspace_id": id, "worktree": {
                            "checkout_path": checkout, "repo_key": "/repo/.git"}})
                    }))
                    .chain(
                        self.listings
                            .iter()
                            .map(|(id, _)| serde_json::json!({"workspace_id": id})),
                    )
                    .collect::<Vec<_>>();
                output(serde_json::json!({"result": {"workspaces": spaces}}).to_string())
            }
            ["worktree", "list", "--workspace", id, "--json"] => {
                match self.listings.iter().find(|(space, _)| space == id) {
                    Some((_, Ok(stdout))) => output((*stdout).to_owned()),
                    Some((_, Err(error))) => Err((*error).to_owned()),
                    None => panic!("unexpected space {id}"),
                }
            }
            ["pane", "list", "--workspace", id] if *id == self.opener => {
                output(r#"{"result":{"panes":[]}}"#.to_owned())
            }
            _ => panic!("unexpected Herdr command: {args:?}"),
        }
    }
}

const SECOND_SPACE_ON_REPO: &str = r#"{"result":{"source":{"source_checkout_path":"/repo","source_workspace_id":"space-a"},"worktrees":[{"path":"/repo","open_workspace_id":"space-b"}]}}"#;
const NOT_GIT_WORKTREE: &str = r#"{"error":{"code":"not_git_worktree"}}"#;

#[test]
fn cwd_discovery_refuses_a_checkout_two_spaces_share() {
    let herdr = SpacesHerdr {
        opener: "space-b",
        linked: Some(false),
        stamped: vec![],
        listings: vec![("space-a", Ok(SECOND_SPACE_ON_REPO))],
    };

    let error = namedrop::Host::space(
        &namedrop::TuiHost::new(&herdr),
        namedrop::SpaceLocator::Checkout(Path::new("/repo")),
        None,
    )
    .map_err(String::from)
    .unwrap_err();
    assert!(error.contains("/repo"), "{error}");
    assert!(error.contains("(space-a, space-b)"), "{error}");
    assert!(error.contains("--herdr-workspace"), "{error}");
}

#[test]
fn cwd_discovery_ignores_spaces_on_other_checkouts_or_none() {
    let herdr = SpacesHerdr {
        opener: "space-a",
        linked: Some(false),
        stamped: vec![],
        listings: vec![
            ("space-c", Err(NOT_GIT_WORKTREE)),
            (
                "space-d",
                Ok(
                    r#"{"result":{"source":{"source_checkout_path":"/other","source_workspace_id":"space-d"},"worktrees":[{"path":"/repo","open_workspace_id":"space-a"},{"path":"/other","open_workspace_id":"space-e"}]}}"#,
                ),
            ),
        ],
    };

    let discovered = namedrop::Host::space(
        &namedrop::TuiHost::new(&herdr),
        namedrop::SpaceLocator::Checkout(Path::new("/repo")),
        None,
    )
    .map_err(String::from)
    .unwrap();
    assert_eq!(discovered.workspace_id, "space-a");
    assert_eq!(discovered.worktree_id, WorktreeId::new("/repo"));
}

#[test]
fn cwd_discovery_fails_closed_when_a_space_cannot_be_read() {
    let herdr = SpacesHerdr {
        opener: "space-a",
        linked: Some(true),
        stamped: vec![],
        listings: vec![("space-b", Err("herdr: connection reset"))],
    };

    let error = namedrop::Host::space(
        &namedrop::TuiHost::new(&herdr),
        namedrop::SpaceLocator::Checkout(Path::new("/repo")),
        None,
    )
    .map_err(String::from)
    .unwrap_err();
    assert_eq!(error, "herdr: connection reset");
}

// What Herdr 0.9.1 answers a second space created with `workspace create
// --cwd` on a linked worktree: the path's first opener, and `source` naming
// the repository root's opener.
const SECOND_SPACE_ON_LINKED: &str = r#"{"result":{"source":{"repo_key":"/repo/.git","source_checkout_path":"/root"},"worktrees":[{"path":"/root"},{"path":"/repo","is_linked_worktree":true,"open_workspace_id":"space-a"}]}}"#;

#[test]
fn cwd_discovery_refuses_a_linked_checkout_a_second_space_may_share() {
    let herdr = SpacesHerdr {
        opener: "space-a",
        linked: Some(true),
        stamped: vec![],
        listings: vec![("space-b", Ok(SECOND_SPACE_ON_LINKED))],
    };

    let error = namedrop::Host::space(
        &namedrop::TuiHost::new(&herdr),
        namedrop::SpaceLocator::Checkout(Path::new("/repo")),
        None,
    )
    .map_err(String::from)
    .unwrap_err();
    assert!(error.contains("/repo"), "{error}");
    assert!(error.contains("space-a"), "{error}");
    assert!(error.contains("space-b"), "{error}");
    assert!(error.contains("--herdr-workspace"), "{error}");
}

#[test]
fn cwd_discovery_treats_an_unreported_checkout_kind_as_linked() {
    let herdr = SpacesHerdr {
        opener: "space-a",
        linked: None,
        stamped: vec![],
        listings: vec![("space-b", Ok(SECOND_SPACE_ON_LINKED))],
    };

    let error = namedrop::Host::space(
        &namedrop::TuiHost::new(&herdr),
        namedrop::SpaceLocator::Checkout(Path::new("/repo")),
        None,
    )
    .map_err(String::from)
    .unwrap_err();
    assert!(error.contains("space-b"), "{error}");
}

#[test]
fn cwd_discovery_counts_a_space_herdr_stamped_on_the_checkout() {
    let herdr = SpacesHerdr {
        opener: "space-a",
        linked: Some(false),
        stamped: vec![("space-b", "/repo"), ("space-c", "/repo-linked")],
        listings: vec![],
    };

    let error = namedrop::Host::space(
        &namedrop::TuiHost::new(&herdr),
        namedrop::SpaceLocator::Checkout(Path::new("/repo")),
        None,
    )
    .map_err(String::from)
    .unwrap_err();
    assert!(error.contains("(space-a, space-b)"), "{error}");
}

#[test]
fn cwd_discovery_places_an_unreported_space_off_a_root_checkout() {
    let herdr = SpacesHerdr {
        opener: "space-a",
        linked: Some(false),
        stamped: vec![],
        listings: vec![("space-b", Ok(SECOND_SPACE_ON_LINKED))],
    };

    assert_eq!(
        namedrop::Host::space(
            &namedrop::TuiHost::new(&herdr),
            namedrop::SpaceLocator::Checkout(Path::new("/repo")),
            None
        )
        .map_err(String::from)
        .unwrap()
        .workspace_id,
        "space-a"
    );
}

#[test]
fn cwd_discovery_skips_spaces_that_cannot_own_a_linked_checkout() {
    let herdr = SpacesHerdr {
        opener: "space-a",
        linked: Some(true),
        stamped: vec![("space-e", "/repo-other")],
        listings: vec![
            (
                "space-c",
                Err(
                    r#"{"error":{"code":"workspace_not_found","message":"workspace space-c not found"}}"#,
                ),
            ),
            (
                "space-d",
                Ok(
                    r#"{"result":{"source":{"repo_key":"/other/.git","source_checkout_path":"/other"},"worktrees":[{"path":"/other"},{"path":"/other-linked","is_linked_worktree":true,"open_workspace_id":"space-x"}]}}"#,
                ),
            ),
            (
                "space-f",
                Ok(
                    r#"{"result":{"source":{"repo_key":"/repo/.git","source_checkout_path":"/root","source_workspace_id":"space-f"},"worktrees":[{"path":"/root","open_workspace_id":"space-x"},{"path":"/repo","is_linked_worktree":true,"open_workspace_id":"space-a"}]}}"#,
                ),
            ),
        ],
    };

    assert_eq!(
        namedrop::Host::space(
            &namedrop::TuiHost::new(&herdr),
            namedrop::SpaceLocator::Checkout(Path::new("/repo")),
            None
        )
        .map_err(String::from)
        .unwrap()
        .workspace_id,
        "space-a"
    );
}

#[test]
fn a_known_pane_names_its_space() {
    struct PanesHerdr;
    impl HerdrCommandExecutor for PanesHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            assert_eq!(args, ["pane", "list"]);
            Ok(HerdrCommandOutput {
                stdout: r#"{"result":{"panes":[{"pane_id":"w1:p1","terminal_id":"t1","tab_id":"tab-1","workspace_id":"w1"},{"pane_id":"w2:p1","terminal_id":"t2","tab_id":"tab-2","workspace_id":"w2"}]}}"#.to_owned(),
            })
        }
    }

    assert_eq!(
        namedrop::pane_workspace(&namedrop::TuiHost::new(&PanesHerdr), "w2:p1").unwrap(),
        "w2"
    );
    let error =
        namedrop::pane_workspace(&namedrop::TuiHost::new(&PanesHerdr), "w9:p1").unwrap_err();
    assert!(error.contains("w9:p1"), "{error}");
    assert!(error.contains("--herdr-workspace"), "{error}");
}

#[test]
fn only_proven_same_worktree_moves_preserve_a_binding() {
    struct MoveHerdr;
    impl HerdrCommandExecutor for MoveHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"]
                | ["worktree", "list", "--workspace", "workspace-1", "--json"] => {
                    r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                }
                ["worktree", "list", "--workspace", "workspace-2", "--json"] => {
                    r#"{"result":{"worktrees":[{"path":"/other","open_workspace_id":"workspace-2"}]}}"#
                }
                ["pane", "list", "--workspace", "workspace-1"]
                | ["pane", "list", "--workspace", "workspace-2"] => r#"{"result":{"panes":[]}}"#,
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput {
                stdout: stdout.to_owned(),
            })
        }
    }
    struct FixedClock;
    impl Clock for FixedClock {
        fn now_unix_ms(&self) -> u64 {
            4
        }
    }
    struct FixedIds;
    impl IdGenerator for FixedIds {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-move")
        }
    }

    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let scope = namespace();
    store
        .transaction(|transaction| {
            transaction.activate_worktree(&scope, &attribution(1))?;
            transaction.create_pane_address(&scope, &address(), &attribution(2))?;
            transaction.bind_pane(
                &scope,
                &PaneBindingId::new("binding-1"),
                &PaneAddressId::new("address-planner"),
                "pane-old",
                &attribution(3),
            )?;
            Ok(())
        })
        .unwrap();
    drop(store);
    let runtime = namedrop::LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: None,
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&MoveHerdr),
        clock: &FixedClock,
        ids: &FixedIds,
    };
    let pane = |pane_id: &str, workspace_id: &str| {
        serde_json::json!({
            "pane_id": pane_id,
            "workspace_id": workspace_id,
            "terminal_id": "terminal-1",
            "tab_id": "tab-1",
            "focused": false,
            "agent_status": "unknown",
            "revision": 1
        })
    };
    execute_lifecycle_operation(
        "event.pane-lifecycle",
        serde_json::json!({
            "type": "pane_created",
            "pane": pane("pane-copy", "workspace-1")
        }),
        &runtime,
    )
    .unwrap();
    let mut store = StateStore::open(directory.path()).unwrap();
    store
        .transaction(|transaction| {
            assert_eq!(transaction.active_pane_bindings(&scope)?.len(), 1);
            assert_eq!(
                transaction.active_pane_bindings(&scope)?[0].pane_id,
                "pane-old"
            );
            Ok(())
        })
        .unwrap();
    execute_lifecycle_operation(
        "event.pane-lifecycle",
        serde_json::json!({
            "type": "pane_moved",
            "previous_pane_id": "pane-old",
            "previous_workspace_id": "workspace-1",
            "previous_tab_id": "tab-1",
            "pane": pane("pane-new", "workspace-1")
        }),
        &runtime,
    )
    .unwrap();
    store
        .transaction(|transaction| {
            assert_eq!(
                transaction.list_pane_addresses(&scope)?[0]
                    .pane_id
                    .as_deref(),
                Some("pane-new")
            );
            Ok(())
        })
        .unwrap();
    execute_lifecycle_operation(
        "event.pane-lifecycle",
        serde_json::json!({
            "type": "pane_moved",
            "previous_pane_id": "pane-new",
            "previous_workspace_id": "workspace-1",
            "previous_tab_id": "tab-1",
            "pane": pane("pane-elsewhere", "workspace-2"),
            "closed_tab_id": "tab-1",
            "closed_workspace_id": "workspace-1"
        }),
        &runtime,
    )
    .unwrap();
    store
        .transaction(|transaction| {
            assert_eq!(
                transaction.list_pane_addresses(&scope)?[0].lifecycle,
                PaneAddressLifecycle::Unbound
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn lifecycle_operations_activate_create_bind_and_list_through_one_contract() {
    struct FakeHerdr;
    impl HerdrCommandExecutor for FakeHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"] => {
                    r#"{"result":{"source":{"source_workspace_id":"workspace-1"},"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                }
                ["pane", "list", "--workspace", "workspace-1"] => {
                    r#"{"result":{"panes":[{"pane_id":"pane-live","terminal_id":"terminal-live","tab_id":"tab-live","workspace_id":"workspace-1","cwd":"/repo","label":"Review pane","agent":"codex","display_agent":"Codex","agent_session":{"value":"session-live"},"agent_status":"idle"}]}}"#
                }
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput {
                stdout: stdout.to_owned(),
            })
        }
    }

    struct FixedClock;
    impl Clock for FixedClock {
        fn now_unix_ms(&self) -> u64 {
            42
        }
    }

    struct SequenceIds(AtomicU64);
    impl IdGenerator for SequenceIds {
        fn next_id(&self, kind: &str) -> String {
            let sequence = self.0.fetch_add(1, Ordering::Relaxed);
            format!("{kind}-{sequence}")
        }
    }

    let directory = TestDirectory::new();
    let ids = SequenceIds(AtomicU64::new(1));
    let runtime = namedrop::LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: None,
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&FakeHerdr),
        clock: &FixedClock,
        ids: &ids,
    };

    let activated =
        execute_lifecycle_operation("activate", serde_json::json!({"confirmed": true}), &runtime)
            .unwrap();
    assert_eq!(activated["worktree_id"], "/repo");
    assert_eq!(activated["activated"], true);
    assert_eq!(
        activated["integration_packages"]["codex"]["installed"],
        true
    );
    let rules = fs::read_to_string(directory.home().join(".codex/rules/namedrop.rules")).unwrap();
    assert_eq!(rules.matches("prefix_rule(").count(), 18);
    let executable = fs::canonicalize(std::env::current_exe().unwrap()).unwrap();
    for argv in [vec!["address", "list"], vec!["complete", "--no-reply"]] {
        let mut pattern = vec![
            executable.to_string_lossy().into_owned(),
            "--plugin-state".to_owned(),
            directory.path().to_string_lossy().into_owned(),
        ];
        pattern.extend(argv.into_iter().map(str::to_owned));
        assert!(rules.contains(&format!(
            "prefix_rule(pattern={}, decision=\"allow\"",
            serde_json::to_string(&pattern).unwrap()
        )));
    }

    let created = execute_lifecycle_operation(
        "address.create",
        serde_json::json!({
            "preset": "reviewer",
            "alias": "code-reviewer",
            "pane_id": "pane-live"
        }),
        &runtime,
    )
    .unwrap();
    assert_eq!(created["alias"], "code-reviewer");
    assert_eq!(created["lifecycle"], "occupied");
    assert_eq!(created["pane_label"], "Review pane");
    assert_eq!(created["occupant_label"], "Codex");

    let listed =
        execute_lifecycle_operation("address.list", serde_json::json!({}), &runtime).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);
    assert_eq!(listed[0]["role_name"], "Reviewer");
}

#[test]
fn lifecycle_operations_mutate_addresses_without_losing_identity() {
    struct FakeHerdr;
    impl HerdrCommandExecutor for FakeHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"] => {
                    r#"{"result":{"source":{"source_workspace_id":"workspace-1"},"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                }
                ["pane", "list", "--workspace", "workspace-1"] => {
                    r#"{"result":{"panes":[{"pane_id":"pane-live","terminal_id":"terminal-live","tab_id":"tab-live","workspace_id":"workspace-1","cwd":"/repo","agent_session":null,"agent_status":null}]}}"#
                }
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput {
                stdout: stdout.to_owned(),
            })
        }
    }

    struct FixedClock;
    impl Clock for FixedClock {
        fn now_unix_ms(&self) -> u64 {
            42
        }
    }

    struct SequenceIds(AtomicU64);
    impl IdGenerator for SequenceIds {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
        }
    }

    let directory = TestDirectory::new();
    let ids = SequenceIds(AtomicU64::new(1));
    let runtime = namedrop::LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: None,
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&FakeHerdr),
        clock: &FixedClock,
        ids: &ids,
    };
    execute_lifecycle_operation("activate", serde_json::json!({"confirmed": true}), &runtime)
        .unwrap();
    let created = execute_lifecycle_operation(
        "address.create",
        serde_json::json!({
            "preset": "planner",
            "alias": "planner",
            "pane_id": "pane-live"
        }),
        &runtime,
    )
    .unwrap();
    let address_id = created["id"].as_str().unwrap();

    let renamed = execute_lifecycle_operation(
        "address.rename",
        serde_json::json!({"address_id": address_id, "alias": "lead-planner"}),
        &runtime,
    )
    .unwrap();
    assert_eq!(renamed["alias"], "lead-planner");
    execute_lifecycle_operation(
        "address.release-alias",
        serde_json::json!({"alias": "planner"}),
        &runtime,
    )
    .unwrap();
    execute_lifecycle_operation(
        "address.unbind",
        serde_json::json!({"address_id": address_id}),
        &runtime,
    )
    .unwrap();
    let rebound = execute_lifecycle_operation(
        "address.bind",
        serde_json::json!({"address_id": address_id, "pane_id": "pane-live"}),
        &runtime,
    )
    .unwrap();
    assert_eq!(rebound["pane_id"], "pane-live");
    execute_lifecycle_operation(
        "address.retire",
        serde_json::json!({"address_id": address_id}),
        &runtime,
    )
    .unwrap();
    let listed =
        execute_lifecycle_operation("address.list", serde_json::json!({}), &runtime).unwrap();
    assert!(listed.as_array().unwrap().is_empty());

    let deactivated =
        execute_lifecycle_operation("deactivate", serde_json::json!({}), &runtime).unwrap();
    assert_eq!(deactivated["deactivated"], true);
}

#[test]
fn strict_inputs_and_activation_confirmation_fail_before_mutation() {
    struct FakeHerdr;
    impl HerdrCommandExecutor for FakeHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"] => {
                    r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                }
                ["pane", "list", "--workspace", "workspace-1"] => r#"{"result":{"panes":[]}}"#,
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput {
                stdout: stdout.to_owned(),
            })
        }
    }
    struct FixedClock;
    impl Clock for FixedClock {
        fn now_unix_ms(&self) -> u64 {
            42
        }
    }
    struct FixedIds;
    impl IdGenerator for FixedIds {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-fixed")
        }
    }
    let directory = TestDirectory::new();
    let runtime = namedrop::LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: None,
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&FakeHerdr),
        clock: &FixedClock,
        ids: &FixedIds,
    };

    let preview =
        execute_lifecycle_operation("activate", serde_json::json!({}), &runtime).unwrap_err();
    assert!(preview.contains("Space=workspace-1"));
    assert!(preview.contains("settled_delay_ms:500"));
    // The confirmation discloses both shared user-level Agent Integration
    // targets, and that they are shared, before anything is written.
    for agent_runtime in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
        let target = package_directory(directory.home(), agent_runtime);
        assert!(
            preview.contains(&format!("{}: {}", agent_runtime.as_str(), target.display())),
            "{preview}"
        );
        assert!(
            !target.exists(),
            "{} was installed unconfirmed",
            target.display()
        );
    }
    assert!(
        preview.contains(
            "created or refreshed on activation and shared by every space on this machine"
        ),
        "{preview}"
    );
    assert!(
        preview.contains(&format!(
            "codex plugin marketplace add '{}' --json",
            package_directory(directory.home(), AgentRuntime::Codex).display()
        )),
        "{preview}"
    );
    for required in [
        "codex plugin add namedrop@namedrop --json",
        "approve Namedrop in /hooks",
        "run /reload-plugins",
    ] {
        assert!(preview.contains(required), "{preview}");
    }
    let mut store = StateStore::open(directory.path()).unwrap();
    assert!(
        store
            .transaction(|transaction| transaction.worktree_state(&namespace()))
            .unwrap()
            .is_none()
    );
    let unknown = execute_lifecycle_operation(
        "activate",
        serde_json::json!({"confirmed": true, "surprise": true}),
        &runtime,
    )
    .unwrap_err();
    assert!(unknown.contains("unknown field"));
}

#[test]
fn agent_status_events_replace_and_clear_the_current_occupant() {
    use std::sync::Mutex;

    struct DynamicHerdr {
        occupant: Mutex<Option<(&'static str, &'static str)>>,
    }
    impl HerdrCommandExecutor for DynamicHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
                        .to_owned()
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"]
                | ["worktree", "list", "--workspace", "workspace-1", "--json"] => {
                    r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                        .to_owned()
                }
                ["pane", "list", "--workspace", "workspace-1"] => {
                    let occupant = *self.occupant.lock().unwrap();
                    match occupant {
                        Some((session, status)) => format!(
                            r#"{{"result":{{"panes":[{{"pane_id":"pane-live","terminal_id":"terminal-live","tab_id":"tab-live","workspace_id":"workspace-1","label":"Main","agent":"codex","display_agent":"Codex","agent_session":{{"value":"{session}"}},"agent_status":"{status}"}}]}}}}"#
                        ),
                        None => {
                            r#"{"result":{"panes":[{"pane_id":"pane-live","terminal_id":"terminal-live","tab_id":"tab-live","workspace_id":"workspace-1","label":"Main","agent_session":null,"agent_status":null}]}}"#
                            .to_owned()
                        }
                    }
                }
                ["pane", "report-metadata", "pane-live", ..] => String::new(),
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput { stdout })
        }
    }
    struct FixedClock;
    impl Clock for FixedClock {
        fn now_unix_ms(&self) -> u64 {
            42
        }
    }
    struct SequenceIds(AtomicU64);
    impl IdGenerator for SequenceIds {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
        }
    }
    let herdr = DynamicHerdr {
        occupant: Mutex::new(Some(("session-1", "working"))),
    };
    let directory = TestDirectory::new();
    let ids = SequenceIds(AtomicU64::new(1));
    let runtime = namedrop::LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-live"),
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&herdr),
        clock: &FixedClock,
        ids: &ids,
    };
    execute_lifecycle_operation("activate", serde_json::json!({"confirmed": true}), &runtime)
        .unwrap();
    execute_lifecycle_operation(
        "address.create",
        serde_json::json!({"preset": "implementer"}),
        &runtime,
    )
    .unwrap();

    *herdr.occupant.lock().unwrap() = Some(("session-2", "idle"));
    execute_lifecycle_operation(
        "event.agent-status-changed",
        serde_json::json!({
            "type": "pane_agent_status_changed",
            "pane_id": "pane-live",
            "workspace_id": "workspace-1",
            "agent_status": "idle"
        }),
        &runtime,
    )
    .unwrap();
    let replaced =
        execute_lifecycle_operation("address.list", serde_json::json!({}), &runtime).unwrap();
    assert_eq!(replaced[0]["agent_session_id"], "session-2");
    assert_eq!(replaced[0]["lifecycle"], "occupied");

    *herdr.occupant.lock().unwrap() = None;
    execute_lifecycle_operation(
        "event.agent-status-changed",
        serde_json::json!({
            "type": "pane_agent_status_changed",
            "pane_id": "pane-live",
            "workspace_id": "workspace-1",
            "agent_status": "unknown"
        }),
        &runtime,
    )
    .unwrap();
    let empty =
        execute_lifecycle_operation("address.list", serde_json::json!({}), &runtime).unwrap();
    assert_eq!(empty[0]["agent_session_id"], serde_json::Value::Null);
    assert_eq!(empty[0]["lifecycle"], "empty");

    execute_lifecycle_operation(
        "event.pane-lifecycle",
        serde_json::json!({
            "type": "pane_closed",
            "pane_id": "pane-live",
            "workspace_id": "workspace-1"
        }),
        &runtime,
    )
    .unwrap();
    let remaining =
        execute_lifecycle_operation("address.list", serde_json::json!({}), &runtime).unwrap();
    assert_eq!(remaining, serde_json::json!([]));
}

#[test]
fn tab_close_cancels_outstanding_delivery_and_removes_every_attached_mention() {
    use std::sync::Mutex;

    struct ClosingTabHerdr {
        open: Mutex<bool>,
    }
    impl HerdrCommandExecutor for ClosingTabHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
                        .to_owned()
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"]
                | ["worktree", "list", "--workspace", "workspace-1", "--json"] => {
                    r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                        .to_owned()
                }
                ["pane", "list", "--workspace", "workspace-1"] => {
                    if *self.open.lock().unwrap() {
                        serde_json::json!({
                            "result": {"panes": [
                                {
                                    "pane_id": "pane-lead",
                                    "terminal_id": "terminal-lead",
                                    "tab_id": "tab-collaboration",
                                    "workspace_id": "workspace-1",
                                    "agent": "codex",
                                    "agent_session": {"value": "session-lead"},
                                    "agent_status": "idle"
                                },
                                {
                                    "pane_id": "pane-reviewer",
                                    "terminal_id": "terminal-reviewer",
                                    "tab_id": "tab-collaboration",
                                    "workspace_id": "workspace-1",
                                    "agent": "claude",
                                    "agent_session": {"value": "session-reviewer"},
                                    "agent_status": "idle"
                                }
                            ]}
                        })
                        .to_string()
                    } else {
                        r#"{"result":{"panes":[]}}"#.to_owned()
                    }
                }
                ["agent", "prompt", _, _] | ["pane", "report-metadata", ..] => {
                    "{}".to_owned()
                }
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput { stdout })
        }
    }
    struct ClosingTabClock;
    impl Clock for ClosingTabClock {
        fn now_unix_ms(&self) -> u64 {
            100
        }
    }
    struct ClosingTabIds(AtomicU64);
    impl IdGenerator for ClosingTabIds {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
        }
    }

    let directory = TestDirectory::new();
    let herdr = ClosingTabHerdr {
        open: Mutex::new(true),
    };
    let ids = ClosingTabIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-lead"),
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&herdr),
        clock: &ClosingTabClock,
        ids: &ids,
    };
    execute_lifecycle_operation("activate", serde_json::json!({"confirmed": true}), &runtime)
        .unwrap();
    let lead = execute_lifecycle_operation(
        "address.create",
        serde_json::json!({"preset": "implementer", "alias": "lead", "pane_id": "pane-lead"}),
        &runtime,
    )
    .unwrap();
    let reviewer = execute_lifecycle_operation(
        "address.create",
        serde_json::json!({"preset": "reviewer", "alias": "reviewer", "pane_id": "pane-reviewer"}),
        &runtime,
    )
    .unwrap();
    StateStore::open(directory.path())
        .unwrap()
        .transaction(|transaction| {
            let thread_id = ThreadId::new("thread-tab-close");
            let reviewer_address = PaneAddressId::new(reviewer["id"].as_str().unwrap());
            let reviewer_binding = transaction
                .active_pane_bindings(&namespace())?
                .into_iter()
                .find(|binding| binding.address_id == reviewer_address)
                .unwrap();
            transaction.create_thread(&namespace(), &thread_id, "Tab close", &attribution(101))?;
            transaction.append_message(
                &namespace(),
                &MessageDraft {
                    thread_id,
                    message_id: MessageId::new("message-tab-close"),
                    in_reply_to: None,
                    author_address_id: PaneAddressId::new(lead["id"].as_str().unwrap()),
                    author_kind: ActorKind::Human,
                    author_alias_snapshot: "lead".to_owned(),
                    author_agent_session_snapshot: Some(AgentSessionId::new("session-lead")),
                    request_id: "request-tab-close".to_owned(),
                    authored_source: "@reviewer stop when the tab closes".to_owned(),
                    body: "stop when the tab closes".to_owned(),
                    grammar_version: "mention/v1".to_owned(),
                    recipients: vec![
                        MessageRecipientDraft {
                            address_id: reviewer_address.clone(),
                            alias_snapshot: "reviewer".to_owned(),
                            agent_session_snapshot: Some(AgentSessionId::new("session-reviewer")),
                            delivery_id: DeliveryId::new("delivery-in-flight"),
                        },
                        MessageRecipientDraft {
                            address_id: PaneAddressId::new(lead["id"].as_str().unwrap()),
                            alias_snapshot: "lead".to_owned(),
                            agent_session_snapshot: Some(AgentSessionId::new("session-lead")),
                            delivery_id: DeliveryId::new("delivery-held"),
                        },
                    ],
                    context_references: Vec::new(),
                },
                &attribution(102),
            )?;
            let attempt_id = DeliveryAttemptId::new("attempt-in-flight");
            transaction.lease_mailbox_head(
                &namespace(),
                &reviewer_address,
                &LeaseDraft {
                    attempt_id: attempt_id.clone(),
                    binding_id: reviewer_binding.id,
                    agent_session_id: AgentSessionId::new("session-reviewer"),
                    retry_of_attempt_id: None,
                    owner: "test".to_owned(),
                    acquired_at_ms: 103,
                    expires_at_ms: 1_000,
                },
                &attribution(103),
            )?;
            transaction.transition_delivery_attempt(
                &namespace(),
                &attempt_id,
                DeliveryState::Submitted,
                &attribution(104),
            )
        })
        .unwrap();
    Connection::open(directory.path().join("namedrop.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE mailbox_deliveries SET state = 'held' WHERE delivery_id = 'delivery-held'",
            [],
        )
        .unwrap();
    StateStore::open(directory.path())
        .unwrap()
        .transaction(|transaction| {
            transaction.append_message(
                &namespace(),
                &MessageDraft {
                    thread_id: ThreadId::new("thread-tab-close"),
                    message_id: MessageId::new("message-tab-close-queued"),
                    in_reply_to: None,
                    author_address_id: PaneAddressId::new(lead["id"].as_str().unwrap()),
                    author_kind: ActorKind::Human,
                    author_alias_snapshot: "lead".to_owned(),
                    author_agent_session_snapshot: Some(AgentSessionId::new("session-lead")),
                    request_id: "request-tab-close-queued".to_owned(),
                    authored_source: "@reviewer queued when the tab closes".to_owned(),
                    body: "queued when the tab closes".to_owned(),
                    grammar_version: "mention/v1".to_owned(),
                    recipients: vec![MessageRecipientDraft {
                        address_id: PaneAddressId::new(reviewer["id"].as_str().unwrap()),
                        alias_snapshot: "reviewer".to_owned(),
                        agent_session_snapshot: Some(AgentSessionId::new("session-reviewer")),
                        delivery_id: DeliveryId::new("delivery-still-queued"),
                    }],
                    context_references: Vec::new(),
                },
                &attribution(106),
            )
        })
        .unwrap();

    *herdr.open.lock().unwrap() = false;
    let closed = execute_lifecycle_operation(
        "event.pane-lifecycle",
        serde_json::json!({
            "type": "tab_closed",
            "tab_id": "tab-collaboration",
            "workspace_id": "workspace-1"
        }),
        &runtime,
    )
    .unwrap();

    assert_eq!(closed["type"], "tab_unavailable");
    assert_eq!(closed["removed_address_ids"].as_array().unwrap().len(), 2);
    let remaining =
        execute_lifecycle_operation("address.list", serde_json::json!({}), &runtime).unwrap();
    assert_eq!(remaining, serde_json::json!([]));
    let connection = Connection::open(directory.path().join("namedrop.sqlite3")).unwrap();
    for delivery_id in [
        "delivery-in-flight",
        "delivery-held",
        "delivery-still-queued",
    ] {
        assert_eq!(
            connection
                .query_row(
                    "SELECT state FROM mailbox_deliveries WHERE delivery_id = ?1",
                    [delivery_id],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "cancelled",
            "{delivery_id}"
        );
    }
}

#[test]
fn agent_status_event_reconciles_every_replaced_session_before_onboarding() {
    use std::sync::Mutex;

    struct ReplacedSessionsHerdr {
        sessions: Mutex<(&'static str, &'static str)>,
        prompts: Mutex<Vec<(String, String)>>,
    }
    impl HerdrCommandExecutor for ReplacedSessionsHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
                        .to_owned()
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"]
                | ["worktree", "list", "--workspace", "workspace-1", "--json"] => {
                    r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                        .to_owned()
                }
                ["pane", "list", "--workspace", "workspace-1"] => {
                    let (first, second) = *self.sessions.lock().unwrap();
                    serde_json::json!({
                        "result": {"panes": [
                            {
                                "pane_id": "pane-first",
                                "terminal_id": "terminal-first",
                                "tab_id": "tab-1",
                                "workspace_id": "workspace-1",
                                "agent": "codex",
                                "display_agent": "Codex",
                                "agent_session": {"value": first},
                                "agent_status": "idle"
                            },
                            {
                                "pane_id": "pane-second",
                                "terminal_id": "terminal-second",
                                "tab_id": "tab-1",
                                "workspace_id": "workspace-1",
                                "agent": "claude",
                                "display_agent": "Claude Code",
                                "agent_session": {"value": second},
                                "agent_status": "idle"
                            },
                            {
                                "pane_id": "pane-author",
                                "terminal_id": "terminal-author",
                                "tab_id": "tab-1",
                                "workspace_id": "workspace-1",
                                "agent_session": null,
                                "agent_status": "unknown"
                            }
                        ]}
                    })
                    .to_string()
                }
                ["agent", "prompt", pane_id, prompt] => {
                    self.prompts
                        .lock()
                        .unwrap()
                        .push(((*pane_id).to_owned(), (*prompt).to_owned()));
                    String::new()
                }
                ["pane", "report-metadata", ..] => String::new(),
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput { stdout })
        }
    }
    struct FixedClock;
    impl Clock for FixedClock {
        fn now_unix_ms(&self) -> u64 {
            42
        }

        fn wait_ms(&self, _duration_ms: u64) {}
    }
    struct SequenceIds(AtomicU64);
    impl IdGenerator for SequenceIds {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
        }
    }

    let herdr = ReplacedSessionsHerdr {
        sessions: Mutex::new(("first-1", "second-1")),
        prompts: Mutex::new(Vec::new()),
    };
    let directory = TestDirectory::new();
    let ids = SequenceIds(AtomicU64::new(1));
    let first = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-first"),
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&herdr),
        clock: &FixedClock,
        ids: &ids,
    };
    execute_lifecycle_operation("name", serde_json::json!({"alias": "first"}), &first).unwrap();
    let second = LifecycleRuntime {
        current_pane_id: Some("pane-second"),
        ..first
    };
    execute_lifecycle_operation("name", serde_json::json!({"alias": "second"}), &second).unwrap();
    let author = LifecycleRuntime {
        current_pane_id: Some("pane-author"),
        ..first
    };
    execute_lifecycle_operation(
        "address.create",
        serde_json::json!({
            "role_name": "Author",
            "role_description": "Sends the post-clear fanout regression message.",
            "alias": "author"
        }),
        &author,
    )
    .unwrap();

    *herdr.sessions.lock().unwrap() = ("first-2", "second-2");
    let sent = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "fanout-after-clear",
            "source": "@first @second Reply with exactly: FANOUT PASS"
        }),
        &author,
    )
    .unwrap();
    assert_eq!(sent["deliveries"][0]["state"], "queued");
    assert_eq!(sent["deliveries"][1]["state"], "queued");
    assert!(sent["drain"]["submitted"].as_array().unwrap().is_empty());

    let event = execute_lifecycle_operation(
        "event.agent-status-changed",
        serde_json::json!({
            "type": "pane_agent_status_changed",
            "pane_id": "pane-first",
            "workspace_id": "workspace-1",
            "agent_status": "idle",
            "agent": "codex",
            "display_agent": "Codex",
            "state_labels": {}
        }),
        &first,
    )
    .unwrap();
    assert_eq!(event["drain"]["submitted"].as_array().unwrap().len(), 2);
    let prompts = herdr.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 2);
    assert!(prompts.iter().all(|(_, prompt)| {
        serde_json::from_str::<serde_json::Value>(prompt).unwrap()["untrusted_body"]
            == "Reply with exactly: FANOUT PASS"
    }));
    drop(prompts);

    let addresses = StateStore::open(directory.path())
        .unwrap()
        .transaction(|transaction| transaction.list_pane_addresses(&namespace()))
        .unwrap();
    for (alias, session) in [("first", "first-2"), ("second", "second-2")] {
        let address = addresses
            .iter()
            .find(|address| address.alias == alias)
            .unwrap();
        assert_eq!(address.agent_session_id, Some(AgentSessionId::new(session)));
        assert!(
            StateStore::open(directory.path())
                .unwrap()
                .transaction(|transaction| transaction.active_integration_registration(
                    &namespace(),
                    &address.id,
                    &AgentSessionId::new(session)
                ))
                .unwrap()
                .is_some()
        );
    }
}

/// Two supported agent panes and one ordinary shell pane.
struct SetupHerdr {
    prompts: std::sync::Mutex<Vec<String>>,
}

impl SetupHerdr {
    fn new() -> Self {
        Self {
            prompts: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl HerdrCommandExecutor for SetupHerdr {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let stdout = match args {
            ["workspace", "list"] => {
                r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#.to_owned()
            }
            ["worktree", "list", "--cwd", "/repo", "--json"]
            | ["worktree", "list", "--json"]
            | ["worktree", "list", "--workspace", "workspace-1", "--json"] => {
                r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                    .to_owned()
            }
            ["pane", "list", "--workspace", "workspace-1"] => serde_json::json!({
                "result": {
                    "panes": [
                        {
                            "pane_id": "pane-claude",
                            "terminal_id": "terminal-claude",
                            "tab_id": "tab-claude",
                            "workspace_id": "workspace-1",
                            "label": "Planning",
                            "agent": "claude",
                            "display_agent": "Claude Code",
                            "agent_session": {"value": "session-claude"},
                            "agent_status": "idle"
                        },
                        {
                            "pane_id": "pane-codex",
                            "terminal_id": "terminal-codex",
                            "tab_id": "tab-codex",
                            "workspace_id": "workspace-1",
                            "label": "Implementation",
                            "agent": "codex",
                            "display_agent": "Codex",
                            "agent_session": {"value": "session-codex"},
                            "agent_status": "working"
                        },
                        {
                            "pane_id": "pane-shell",
                            "terminal_id": "terminal-shell",
                            "tab_id": "tab-shell",
                            "workspace_id": "workspace-1",
                            "label": "Shell",
                            "agent_session": null,
                            "agent_status": null
                        }
                    ]
                }
            })
            .to_string(),
            ["agent", "prompt", _, prompt] => {
                self.prompts.lock().unwrap().push((*prompt).to_owned());
                "{}".to_owned()
            }
            ["pane", "report-metadata", ..] => "{}".to_owned(),
            _ => return Err(format!("unexpected Herdr command: {args:?}")),
        };
        Ok(HerdrCommandOutput { stdout })
    }
}

struct FixedSetupClock;
impl Clock for FixedSetupClock {
    fn now_unix_ms(&self) -> u64 {
        1_785_013_479_709
    }
}

struct SetupIds(AtomicU64);
impl IdGenerator for SetupIds {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
    }
}

#[test]
fn remove_unbinds_retires_and_releases_every_alias_atomically() {
    let directory = TestDirectory::new();
    let herdr = SetupHerdr::new();
    let ids = SetupIds(AtomicU64::new(1));
    let runtime = namedrop::LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: None,
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&herdr),
        clock: &FixedSetupClock,
        ids: &ids,
    };
    execute_lifecycle_operation("activate", serde_json::json!({"confirmed": true}), &runtime)
        .unwrap();
    let created = execute_lifecycle_operation(
        "address.create",
        serde_json::json!({
            "preset": "planner",
            "alias": "reviewer",
            "pane_id": "pane-claude"
        }),
        &runtime,
    )
    .unwrap();
    execute_lifecycle_operation(
        "address.rename",
        serde_json::json!({"address_id": created["id"], "alias": "collaborator"}),
        &runtime,
    )
    .unwrap();

    let removed = execute_lifecycle_operation(
        "address.remove",
        serde_json::json!({"address_id": created["id"]}),
        &runtime,
    )
    .unwrap();
    assert_eq!(removed["removed"], true);
    assert_eq!(
        removed["released_aliases"],
        serde_json::json!(["collaborator", "reviewer"])
    );
    let listed =
        execute_lifecycle_operation("address.list", serde_json::json!({}), &runtime).unwrap();
    assert!(listed.as_array().unwrap().is_empty());

    assert_eq!(
        execute_lifecycle_operation(
            "address.create",
            serde_json::json!({
                "preset": "planner",
                "alias": "reviewer",
                "pane_id": "pane-claude"
            }),
            &runtime,
        )
        .unwrap()["alias"],
        "reviewer"
    );
    assert_eq!(
        execute_lifecycle_operation(
            "address.create",
            serde_json::json!({
                "preset": "implementer",
                "alias": "collaborator",
                "pane_id": "pane-codex"
            }),
            &runtime,
        )
        .unwrap()["alias"],
        "collaborator"
    );
}

struct FixedIds(AtomicU64);
impl IdGenerator for FixedIds {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
    }
}

struct FixedClock;
impl Clock for FixedClock {
    fn now_unix_ms(&self) -> u64 {
        100
    }
}

/// One named agent pane in tab-1, with an optional plain shell beside it, an
/// optional native session and the idle label Namedrop last published.
struct AgentTabHerdr {
    shell: Mutex<bool>,
    session: Mutex<Option<&'static str>>,
    label: Mutex<String>,
    idle_label: Mutex<Option<String>>,
    identity_reports: Mutex<Vec<String>>,
}

impl AgentTabHerdr {
    fn new(shell: bool, session: Option<&'static str>) -> Self {
        Self {
            shell: Mutex::new(shell),
            session: Mutex::new(session),
            label: Mutex::new("Main".to_owned()),
            idle_label: Mutex::new(None),
            identity_reports: Mutex::new(Vec::new()),
        }
    }
}

impl HerdrCommandExecutor for AgentTabHerdr {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let stdout = match args {
            ["workspace", "list"] => {
                r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#.to_owned()
            }
            ["worktree", "list", "--cwd", "/repo", "--json"]
            | ["worktree", "list", "--json"]
            | ["worktree", "list", "--workspace", "workspace-1", "--json"] => {
                r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                    .to_owned()
            }
            ["pane", "list", "--workspace", "workspace-1"] => {
                let mut agent = serde_json::json!({
                    "pane_id": "pane-1",
                    "terminal_id": "terminal-1",
                    "tab_id": "tab-1",
                    "workspace_id": "workspace-1",
                    "agent": "codex",
                    "agent_status": "idle"
                });
                if let Some(session) = *self.session.lock().unwrap() {
                    agent["agent_session"] = serde_json::json!({"value": session});
                }
                if let Some(idle) = self.idle_label.lock().unwrap().clone() {
                    agent["state_labels"] = serde_json::json!({"idle": idle});
                }
                let mut panes = vec![agent];
                if *self.shell.lock().unwrap() {
                    panes.push(serde_json::json!({
                        "pane_id": "pane-2",
                        "terminal_id": "terminal-2",
                        "tab_id": "tab-1",
                        "workspace_id": "workspace-1",
                        "agent_status": "unknown"
                    }));
                }
                serde_json::json!({"result": {"panes": panes}}).to_string()
            }
            [
                "pane",
                "report-metadata",
                "pane-1",
                "--source",
                "namedrop-identity",
                rest @ ..,
            ] => {
                let idle = rest
                    .iter()
                    .find_map(|argument| argument.strip_prefix("idle="))
                    .unwrap()
                    .to_owned();
                self.identity_reports.lock().unwrap().push(idle.clone());
                *self.idle_label.lock().unwrap() = Some(idle);
                "{}".to_owned()
            }
            ["pane", "report-metadata", "pane-1", ..] => "{}".to_owned(),
            ["tab", "get", "tab-1"] => {
                serde_json::json!({"result": {"label": self.label.lock().unwrap().clone()}})
                    .to_string()
            }
            ["tab", "rename", "tab-1", label] => {
                *self.label.lock().unwrap() = (*label).to_owned();
                "{}".to_owned()
            }
            _ => return Err(format!("unexpected Herdr command: {args:?}")),
        };
        Ok(HerdrCommandOutput { stdout })
    }
}

fn agent_tab_runtime<'a>(
    directory: &'a TestDirectory,
    herdr: &'a AgentTabHerdr,
    ids: &'a FixedIds,
) -> LifecycleRuntime<'a, namedrop::TuiHost<&'a AgentTabHerdr>, FixedClock, FixedIds> {
    LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-1"),
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(herdr),
        clock: &FixedClock,
        ids,
    }
}

#[test]
fn a_shell_pane_beside_a_named_agent_does_not_take_its_tab_label() {
    let directory = TestDirectory::new();
    let herdr = AgentTabHerdr::new(true, Some("session-1"));
    let ids = FixedIds(AtomicU64::new(1));
    let runtime = agent_tab_runtime(&directory, &herdr, &ids);

    execute_lifecycle_operation("name", serde_json::json!({"alias": "reviewer"}), &runtime)
        .unwrap();
    assert_eq!(
        *herdr.label.lock().unwrap(),
        "@reviewer",
        "naming beside a shell still labels the tab"
    );

    *herdr.shell.lock().unwrap() = false;
    execute_lifecycle_operation("reconcile", serde_json::json!({}), &runtime).unwrap();
    *herdr.shell.lock().unwrap() = true;
    execute_lifecycle_operation("reconcile", serde_json::json!({}), &runtime).unwrap();
    assert_eq!(
        *herdr.label.lock().unwrap(),
        "@reviewer",
        "opening a shell beside the agent keeps its label"
    );
}

#[test]
fn a_native_agent_without_a_session_shows_a_connect_hint_until_it_registers() {
    let directory = TestDirectory::new();
    let herdr = AgentTabHerdr::new(false, None);
    let ids = FixedIds(AtomicU64::new(1));
    let runtime = agent_tab_runtime(&directory, &herdr, &ids);

    execute_lifecycle_operation("name", serde_json::json!({"alias": "builder"}), &runtime).unwrap();
    assert_eq!(
        herdr.idle_label.lock().unwrap().as_deref(),
        Some("@builder: send a prompt to connect")
    );

    execute_lifecycle_operation("reconcile", serde_json::json!({}), &runtime).unwrap();
    let published = herdr.identity_reports.lock().unwrap().len();

    *herdr.session.lock().unwrap() = Some("session-1");
    let event = serde_json::json!({
        "type": "pane_agent_detected",
        "pane_id": "pane-1",
        "workspace_id": "workspace-1",
        "agent": "codex"
    });
    execute_lifecycle_operation("event.agent-status-changed", event, &runtime).unwrap();
    assert_eq!(
        herdr.idle_label.lock().unwrap().as_deref(),
        Some("@builder ready")
    );

    execute_lifecycle_operation("reconcile", serde_json::json!({}), &runtime).unwrap();
    assert_eq!(
        herdr.identity_reports.lock().unwrap().len(),
        published + 1,
        "labels are republished only when the session state changes"
    );
}

/// A space whose agent panes can appear after activation, recording every
/// Herdr command so tests can count tour popups.
struct TourHerdr {
    agents: std::sync::Mutex<bool>,
    commands: std::sync::Mutex<Vec<Vec<String>>>,
}

impl TourHerdr {
    fn new(agents: bool) -> Self {
        Self {
            agents: std::sync::Mutex::new(agents),
            commands: std::sync::Mutex::new(Vec::new()),
        }
    }

    fn popup_opens(&self) -> Vec<Vec<String>> {
        self.commands
            .lock()
            .unwrap()
            .iter()
            .filter(|command| command.starts_with(&["plugin".to_owned(), "pane".to_owned()]))
            .cloned()
            .collect()
    }
}

impl HerdrCommandExecutor for TourHerdr {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        self.commands
            .lock()
            .unwrap()
            .push(args.iter().map(|argument| (*argument).to_owned()).collect());
        let pane = |id: &str, agent: bool| {
            let mut pane = serde_json::json!({
                "pane_id": id,
                "terminal_id": format!("terminal-{id}"),
                "tab_id": format!("tab-{id}"),
                "workspace_id": "workspace-1",
            });
            if agent {
                pane["agent"] = serde_json::json!("codex");
                pane["display_agent"] = serde_json::json!("Codex");
                pane["agent_session"] = serde_json::json!({"value": format!("session-{id}")});
                pane["agent_status"] = serde_json::json!("idle");
            }
            pane
        };
        let stdout = match args {
            ["workspace", "list"] => {
                r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#.to_owned()
            }
            ["worktree", "list", "--cwd", "/repo", "--json"]
            | ["worktree", "list", "--json"]
            | ["worktree", "list", "--workspace", "workspace-1", "--json"] => {
                r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                    .to_owned()
            }
            ["pane", "list", "--workspace", "workspace-1"] => {
                let agents = *self.agents.lock().unwrap();
                serde_json::json!({"result": {"panes": [
                    pane("pane-shell", false),
                    pane("pane-a", agents),
                    pane("pane-b", agents),
                ]}})
                .to_string()
            }
            ["pane", "report-metadata", ..] | ["tab", "rename", ..] => "{}".to_owned(),
            ["tab", "get", _] => r#"{"result":{"label":"Main"}}"#.to_owned(),
            ["plugin", "pane", "open", ..] => r#"{"result":{"pane_id":"popup"}}"#.to_owned(),
            _ => return Err(format!("unexpected Herdr command: {args:?}")),
        };
        Ok(HerdrCommandOutput { stdout })
    }
}

struct TourClock;
impl Clock for TourClock {
    fn now_unix_ms(&self) -> u64 {
        100
    }
    fn wait_ms(&self, _duration_ms: u64) {}
}

struct TourIds(AtomicU64);
impl IdGenerator for TourIds {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
    }
}

fn tour_runtime<'a>(
    directory: &'a TestDirectory,
    herdr: &'a TourHerdr,
    ids: &'a TourIds,
) -> LifecycleRuntime<'a, namedrop::TuiHost<&'a TourHerdr>, TourClock, TourIds> {
    LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: None,
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(herdr),
        clock: &TourClock,
        ids,
    }
}

fn expected_tour_open(directory: &TestDirectory, pane: &str) -> Vec<String> {
    [
        "plugin",
        "pane",
        "open",
        "--plugin",
        "namedrop",
        "--entrypoint",
        "tour",
        "--placement",
        "popup",
        "--width",
        "72",
        "--height",
        "24",
        "--env",
        "NAMEDROP_TARGET_WORKSPACE=workspace-1",
        "--env",
        &format!("NAMEDROP_TARGET_PANE={pane}"),
        "--env",
        &format!("NAMEDROP_TARGET_TERMINAL=terminal-{pane}"),
        "--env",
        &format!("NAMEDROP_TARGET_SESSION=session-{pane}"),
        "--env",
        &format!("HERDR_PLUGIN_STATE_DIR={}", directory.path().display()),
        "--env",
        "HERDR_SOCKET_PATH=socket-a",
        "--focus",
    ]
    .map(str::to_owned)
    .to_vec()
}

fn agent_event(event_type: &str, pane: &str) -> serde_json::Value {
    serde_json::json!({
        "type": event_type,
        "pane_id": pane,
        "workspace_id": "workspace-1",
        "agent": "codex"
    })
}

#[test]
fn first_agent_detection_opens_the_tour_once() {
    let directory = TestDirectory::new();
    let herdr = TourHerdr::new(false);
    let ids = TourIds(AtomicU64::new(1));
    let runtime = tour_runtime(&directory, &herdr, &ids);

    // Activating a space that holds no agent yet leaves the tour for later.
    let activated =
        execute_lifecycle_operation("activate", serde_json::json!({"confirmed": true}), &runtime)
            .unwrap();
    assert_eq!(activated["activated"], true);
    assert_eq!(activated["tour"]["shown"], false);
    assert_eq!(activated["tour"]["reason"], "no_agent_pane");
    assert!(herdr.popup_opens().is_empty());
    let marker = directory.path().join("tours-shown/welcome-1");
    assert!(!marker.exists());

    // The detected pane is the target even when another agent pane comes first.
    *herdr.agents.lock().unwrap() = true;
    let report = execute_lifecycle_operation(
        "event.agent-status-changed",
        agent_event("pane_agent_detected", "pane-b"),
        &runtime,
    )
    .unwrap();
    assert_eq!(report["handled"], true);
    assert!(report["drain"].is_object(), "{report}");
    assert_eq!(
        report["tour"],
        serde_json::json!({
            "shown": true,
            "presentation_id": "welcome-1",
            "pane_id": "pane-b",
            "popup": {"result": {"pane_id": "popup"}},
        })
    );
    assert_eq!(
        herdr.popup_opens(),
        [expected_tour_open(&directory, "pane-b")]
    );
    assert!(marker.is_file());

    let again = execute_lifecycle_operation(
        "event.agent-status-changed",
        agent_event("pane_agent_detected", "pane-a"),
        &runtime,
    )
    .unwrap();
    assert_eq!(again["handled"], true);
    assert_eq!(again["tour"]["shown"], false);
    assert_eq!(again["tour"]["reason"], "already_shown");
    assert_eq!(herdr.popup_opens().len(), 1, "the tour opened twice");
    assert!(marker.is_file());
}

#[test]
fn status_changes_never_open_the_tour_but_activation_with_an_agent_does_once() {
    let directory = TestDirectory::new();
    let herdr = TourHerdr::new(false);
    let ids = TourIds(AtomicU64::new(1));
    let runtime = tour_runtime(&directory, &herdr, &ids);
    execute_lifecycle_operation("activate", serde_json::json!({"confirmed": true}), &runtime)
        .unwrap();

    *herdr.agents.lock().unwrap() = true;
    let mut changed = agent_event("pane_agent_status_changed", "pane-a");
    changed["agent_status"] = serde_json::json!("working");
    for _ in 0..2 {
        let report =
            execute_lifecycle_operation("event.agent-status-changed", changed.clone(), &runtime)
                .unwrap();
        assert_eq!(report["handled"], true);
        assert!(report["drain"].is_object(), "{report}");
        assert_eq!(report["tour"], serde_json::Value::Null);
    }
    assert!(herdr.popup_opens().is_empty());
    assert!(!directory.path().join("tours-shown").exists());

    let fresh = TestDirectory::new();
    let herdr = TourHerdr::new(true);
    let runtime = tour_runtime(&fresh, &herdr, &ids);
    let activated =
        execute_lifecycle_operation("activate", serde_json::json!({"confirmed": true}), &runtime)
            .unwrap();
    assert_eq!(activated["activated"], true);
    assert_eq!(activated["tour"]["shown"], true);
    // Without a detected pane, the first agent pane in Herdr's order is shown.
    assert_eq!(activated["tour"]["pane_id"], "pane-a");
    assert_eq!(herdr.popup_opens(), [expected_tour_open(&fresh, "pane-a")]);
    assert!(fresh.path().join("tours-shown/welcome-1").is_file());

    let again =
        execute_lifecycle_operation("activate", serde_json::json!({"confirmed": true}), &runtime)
            .unwrap();
    assert_eq!(again["activated"], true);
    assert_eq!(again["tour"]["shown"], false);
    assert_eq!(again["tour"]["reason"], "already_shown");
    assert_eq!(herdr.popup_opens().len(), 1, "the tour opened twice");
}

#[test]
fn name_refuses_a_retired_or_renamed_away_alias_with_a_clear_message() {
    struct ThreePaneHerdr;
    impl HerdrCommandExecutor for ThreePaneHerdr {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            let stdout = match args {
                ["workspace", "list"] => {
                    r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
                }
                ["worktree", "list", "--cwd", "/repo", "--json"]
                | ["worktree", "list", "--json"] => {
                    r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
                }
                ["pane", "list", "--workspace", "workspace-1"] => {
                    r#"{"result":{"panes":[
                        {"pane_id":"pane-first","terminal_id":"terminal-first","tab_id":"tab-first","workspace_id":"workspace-1","cwd":"/repo","agent_session":null,"agent_status":null},
                        {"pane_id":"pane-second","terminal_id":"terminal-second","tab_id":"tab-second","workspace_id":"workspace-1","cwd":"/repo","agent_session":null,"agent_status":null},
                        {"pane_id":"pane-third","terminal_id":"terminal-third","tab_id":"tab-third","workspace_id":"workspace-1","cwd":"/repo","agent_session":null,"agent_status":null}
                    ]}}"#
                }
                ["pane", "report-metadata", ..] | ["tab", "rename", ..] => "{}",
                ["tab", "get", ..] => r#"{"result":{"label":"Main"}}"#,
                _ => return Err(format!("unexpected Herdr command: {args:?}")),
            };
            Ok(HerdrCommandOutput {
                stdout: stdout.to_owned(),
            })
        }
    }

    let directory = TestDirectory::new();
    let ids = FixedIds(AtomicU64::new(1));
    let first = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-first"),
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(&ThreePaneHerdr),
        clock: &FixedClock,
        ids: &ids,
    };
    let second = LifecycleRuntime {
        current_pane_id: Some("pane-second"),
        ..first
    };
    let third = LifecycleRuntime {
        current_pane_id: Some("pane-third"),
        ..first
    };
    let name = |runtime: &LifecycleRuntime<'_, namedrop::TuiHost<&'_ _>, _, _>, alias: &str| {
        execute_lifecycle_operation(
            "name",
            serde_json::json!({"alias": alias, "pane_type": "terminal"}),
            runtime,
        )
    };
    let address_id = |alias: &str| {
        execute_lifecycle_operation("address.list", serde_json::json!({}), &first)
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .find(|address| address["alias"] == alias)
            .unwrap_or_else(|| panic!("no address is named {alias}"))["id"]
            .clone()
    };
    name(&first, "planner").unwrap();
    name(&second, "builder").unwrap();
    execute_lifecycle_operation(
        "address.retire",
        serde_json::json!({"address_id": address_id("planner")}),
        &first,
    )
    .unwrap();
    name(&second, "lead-builder").unwrap();
    let before =
        execute_lifecycle_operation("address.list", serde_json::json!({}), &first).unwrap();

    let retired =
        "the name @planner was retired in this workspace and cannot be reused; choose a new name";
    assert_eq!(name(&third, "planner").unwrap_err(), retired);
    assert_eq!(name(&second, "planner").unwrap_err(), retired);
    assert_eq!(
        execute_lifecycle_operation(
            "address.rename",
            serde_json::json!({"address_id": address_id("lead-builder"), "alias": "planner"}),
            &first,
        )
        .unwrap_err(),
        retired
    );
    assert_eq!(
        name(&third, "builder").unwrap_err(),
        "the name @builder was renamed to @lead-builder in this workspace and cannot be reused; \
         choose a new name"
    );
    assert_eq!(
        name(&third, "lead-builder").unwrap_err(),
        "the name @lead-builder is already in use in this workspace; choose a new name"
    );
    assert_eq!(
        execute_lifecycle_operation("address.list", serde_json::json!({}), &first).unwrap(),
        before,
        "a refused name must leave every address as it was"
    );
}

#[test]
fn naming_a_codex_pane_the_shared_app_server_serves_gives_its_advice_instead_of_session_setup() {
    let directory = TestDirectory::new();
    let herdr = AgentTabHerdr::new(false, None);
    let ids = FixedIds(AtomicU64::new(1));
    let runtime = agent_tab_runtime(&directory, &herdr, &ids);
    let server = codex_app_server::start_server(&directory.home().join(".codex"));

    let named =
        execute_lifecycle_operation("name", serde_json::json!({"alias": "builder"}), &runtime)
            .unwrap();
    let advice = &named["codex_app_server"];
    assert_eq!(advice["status"], "codex_shared_app_server");
    assert_eq!(advice["summary"], codex_app_server::SUMMARY);
    assert!(
        advice["fix"]
            .as_str()
            .unwrap()
            .contains("Namedrop never changes Codex settings")
    );
    let steps = advice["steps"].as_array().unwrap();
    for command in codex_app_server::COMMANDS {
        assert!(steps.contains(&serde_json::json!(command)), "{command}");
    }
    assert_eq!(
        named["native_session_setup"],
        serde_json::Value::Null,
        "a first prompt or a Herdr reinstall cannot help a served pane"
    );
    assert_eq!(
        herdr.idle_label.lock().unwrap().as_deref(),
        Some("@builder: see namedrop doctor")
    );

    drop(server);
    execute_lifecycle_operation("reconcile", serde_json::json!({}), &runtime).unwrap();
    assert_eq!(
        herdr.idle_label.lock().unwrap().as_deref(),
        Some("@builder: send a prompt to connect"),
        "the label follows the server once it stops"
    );
    let named =
        execute_lifecycle_operation("name", serde_json::json!({"alias": "builder"}), &runtime)
            .unwrap();
    assert_eq!(named["codex_app_server"], serde_json::Value::Null);
    assert!(
        named["native_session_setup"]["explanation"]
            .as_str()
            .unwrap()
            .contains("first prompt")
    );
}
