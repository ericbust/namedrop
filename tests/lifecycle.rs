#![cfg(feature = "test-support")]
#[path = "support/cleanup.rs"]
mod cleanup;

#[path = "support/codex_app_server.rs"]
mod codex_app_server;
#[path = "support/delivered.rs"]
mod delivered;

use clap::Parser;
use namedrop::{
    ActorKind, AgentRuntime, AgentSessionId, Clock, DeliveryAttemptId, DeliveryId, DeliveryState,
    IdGenerator, LeaseDraft, LifecycleRuntime, MessageDraft, MessageId, MessageRecipientDraft,
    MutationAttribution, PaneAddressDraft, PaneAddressId, PaneAddressLifecycle, PaneBindingId,
    PaneRoleId, PaneRolePreset, StateNamespace, StateStore, StoreErrorKind, ThreadId, WorktreeId,
    discover_worktree, execute_lifecycle_operation, execute_message_operation,
    locate_agent_session_pane,
    mux::{
        AgentKind, AgentState, Depth, Effect, InputOutcome, MuxError, PopupScreen, PopupSpec,
        SessionClaim, TabId,
        fake::{
            CHECKOUT_LISTINGS, EVERY_LISTING, FakeMux, Listing, Read, ScopeKind, SentInput,
            StrictSpace, Surveyed, Takes, WorldBuilder,
        },
    },
    normalize_alias, package_directory, pane_workspace, reconcile_discovered_panes, validate_alias,
};
use rusqlite::Connection;
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
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
        cleanup::remove_fixture(&self.path).unwrap();
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

/// Workspace-1 on the checkout `/repo`, which `workspace list` names with no
/// worktree stamp, as every old fake in this file listed it.
fn repo_world() -> WorldBuilder {
    FakeMux::world()
        .endpoint("socket-a")
        .space("workspace-1")
        .checkout("/repo")
        .unstamped()
}

/// The effects the old fakes failed: activation's keybinding reload and tour
/// popup, and a message's notification.
fn refused(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::ReloadKeybindings | Effect::OpenPopup(_) | Effect::Notify { .. }
    )
}

/// The old fakes' `workspace list`, `worktree list --cwd /repo`, and `pane
/// list --workspace`, and `worktree list --workspace` when `per_space`. The
/// space takes what `takes` names, and the effects the old fakes failed,
/// which fail with the old words.
fn lifecycle_space(mux: FakeMux, per_space: bool, takes: Takes) -> StrictSpace {
    mux.on_apply(|_, effect| {
        if refused(effect) {
            return Err(MuxError::Unavailable(format!(
                "unexpected Herdr command: {effect:?}"
            )));
        }
        Ok(())
    });
    let mut surveys = CHECKOUT_LISTINGS.to_vec();
    if per_space {
        surveys.push((ScopeKind::Space, Depth::Panes));
    }
    StrictSpace::new(mux, &surveys, takes.effect(refused))
}

/// `pane report-metadata <pane> ..`, which carried the pane's identity and
/// its badges.
fn metadata_of(pane: &'static str) -> impl Fn(&Effect) -> bool + Send + Sync + 'static {
    move |effect| {
        matches!(
            effect,
            Effect::Identify { pane: target, .. } | Effect::Badges { pane: target, .. }
                if target.as_str() == pane
        )
    }
}

/// `tab rename tab-1 <label>`, for `only` that label when it names one.
fn renames_tab_1(only: Option<&'static str>) -> impl Fn(&Effect) -> bool + Send + Sync + 'static {
    move |effect| {
        matches!(
            effect,
            Effect::RenameTab { tab, label }
                if tab.as_str() == "tab-1" && only.is_none_or(|only| label == only)
        )
    }
}

/// `tab get` answers `Main` whatever was renamed, for `only` that tab when it
/// names one.
fn main_tab_labels(mux: &FakeMux, only: Option<&'static str>) {
    mux.on_read(move |mux, read| {
        if let Read::TabLabel(tab) = read
            && only.is_none_or(|only| tab.as_str() == only)
        {
            mux.set_tab_label(tab.as_str(), Some("Main"));
        }
        Ok(())
    });
}

#[test]
fn name_is_one_positional_cli_command() {
    let cli = namedrop::cli::Cli::try_parse_from(["namedrop", "name", "@reviewer"]).unwrap();

    assert_eq!(cli.operation(), "name");
    assert_eq!(cli.agent_name(), Some("@reviewer"));
}

#[test]
fn name_activates_binds_registers_and_labels_the_current_native_pane() {
    let space = lifecycle_space(
        repo_world()
            .pane("pane-1")
            .terminal("terminal-1")
            .tab("tab-1")
            .codex()
            .agent_label("Codex")
            .session("session-1")
            .idle()
            .build(),
        false,
        Takes::nothing()
            .effect(metadata_of("pane-1"))
            .effect(renames_tab_1(Some("@reviewer"))),
    );
    main_tab_labels(&space, Some("tab-1"));
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
    let ids = NameIds(AtomicU64::new(1));
    let codex_home = directory.path().join("custom-codex");
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories {
            codex: Some(&codex_home),
            claude: None,
        },
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-1"),
        session_socket_id: "socket-a",
        mux: &*space,
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
    let expected_spool = directory
        .path()
        .canonicalize()
        .unwrap()
        .join("deferred-commands");
    let config_path = codex_home.join("config.toml");
    let granted = fs::read_to_string(&config_path).unwrap();
    let config = granted.parse::<toml_edit::DocumentMut>().unwrap();
    assert_eq!(
        config["sandbox_workspace_write"]["writable_roots"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![expected_spool.to_str().unwrap()]
    );
    assert!(!directory.home().join(".codex/config.toml").exists());
    fs::write(&config_path, "model = 'user-model'\n").unwrap();
    let report = namedrop::diagnose_collaboration(&runtime);
    let issue = report
        .issues
        .iter()
        .find(|issue| {
            issue.code == "ND-INTEGRATION-001" && issue.explanation.contains("writable root")
        })
        .unwrap();
    assert!(issue.explanation.contains("activate Namedrop again"));
    assert_eq!(
        fs::read_to_string(&config_path).unwrap(),
        "model = 'user-model'\n"
    );
    execute_lifecycle_operation("name", serde_json::json!({"alias": "@reviewer"}), &runtime)
        .unwrap();
    assert_eq!(
        fs::read_to_string(&config_path).unwrap(),
        "model = 'user-model'\n",
        "repeated naming must not repair Codex config from inside an agent sandbox"
    );
    let repaired =
        execute_lifecycle_operation("activate", serde_json::json!({"confirmed": true}), &runtime)
            .unwrap();
    assert_eq!(
        repaired["codex_sandbox"]["writable_root"],
        expected_spool.to_str().unwrap()
    );
    assert!(
        repaired["codex_sandbox"]["guidance"]
            .as_str()
            .unwrap()
            .contains("new Codex session")
    );
    assert!(
        fs::read_to_string(&config_path)
            .unwrap()
            .contains("writable_roots")
    );
    let removed =
        execute_lifecycle_operation("deactivate", serde_json::json!({}), &runtime).unwrap();
    assert_eq!(removed["codex_sandbox"]["removed_entries"], 1);
    assert!(
        fs::read_to_string(&config_path)
            .unwrap()
            .contains("model = 'user-model'")
    );
    let effects = space.effects();
    assert!(effects.iter().any(|effect| matches!(
        effect,
        Effect::Identify { pane, alias, texts }
            if pane.as_str() == "pane-1" && alias == "reviewer" && texts.working == "@reviewer working"
    )));
    assert!(effects.contains(&Effect::RenameTab {
        tab: TabId::new("tab-1"),
        label: "@reviewer".to_owned(),
    }));
    let no_home = TestDirectory::new();
    let runtime_without_home = LifecycleRuntime {
        state_directory: no_home.path(),
        home_directory: None,
        agent_config: Default::default(),
        ..runtime
    };
    let named = execute_lifecycle_operation(
        "name",
        serde_json::json!({"alias":"@reviewer"}),
        &runtime_without_home,
    )
    .unwrap();
    assert_eq!(named["alias"], "@reviewer");
    assert!(!no_home.path().join("codex-sandbox.json").exists());
}

#[test]
fn name_leaves_a_tab_shared_by_two_agents_unchanged() {
    let space = lifecycle_space(
        repo_world()
            .pane("pane-1")
            .terminal("terminal-1")
            .tab("tab-1")
            .codex()
            .session("session-1")
            .idle()
            .pane("pane-2")
            .terminal("terminal-2")
            .tab("tab-1")
            .claude()
            .idle()
            .reports_session(false)
            .build(),
        false,
        Takes::nothing().effect(metadata_of("pane-1")),
    );
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
        mux: &*space,
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
        saw_durable_intent: Arc<Mutex<bool>>,
        space: StrictSpace,
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
    let space = lifecycle_space(
        repo_world()
            .pane("pane-1")
            .terminal("terminal-1")
            .tab("tab-1")
            .codex()
            .session("session-1")
            .idle()
            .tab_label("tab-1", "Main")
            .build(),
        false,
        Takes::nothing()
            .effect(metadata_of("pane-1"))
            .effect(renames_tab_1(Some("@reviewer"))),
    );
    let saw_durable_intent = Arc::new(Mutex::new(false));
    let (database_path, saw) = (
        directory.path().join("namedrop.sqlite3"),
        Arc::clone(&saw_durable_intent),
    );
    space.on_apply(move |_, effect| {
        if let Effect::RenameTab { label, .. } = effect
            && label == "@reviewer"
        {
            *saw.lock().unwrap() = Connection::open(&database_path)
                .unwrap()
                .query_row(
                    "SELECT pending_label = '@reviewer'
                     FROM managed_tab_labels WHERE tab_id = 'tab-1'",
                    [],
                    |row| row.get(0),
                )
                .unwrap();
            return Err(MuxError::Unavailable(
                "tab was closed during rename".to_owned(),
            ));
        }
        Ok(())
    });
    let herdr = RenameFailureHerdr {
        saw_durable_intent,
        space,
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
        mux: &*herdr.space,
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
        split: Arc<Mutex<bool>>,
        label: Arc<Mutex<String>>,
        saw_restore_intent: Arc<Mutex<bool>>,
        space: StrictSpace,
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
    let space = lifecycle_space(
        repo_world()
            .pane("pane-1")
            .terminal("terminal-1")
            .tab("tab-1")
            .codex()
            .session("session-1")
            .idle()
            .pane("pane-2")
            .terminal("terminal-2")
            .tab("tab-1")
            .claude()
            .idle()
            .reports_session(false)
            .hidden()
            .build(),
        false,
        Takes::nothing()
            .effect(metadata_of("pane-1"))
            .effect(renames_tab_1(None)),
    );
    let split = Arc::new(Mutex::new(false));
    let label = Arc::new(Mutex::new("Review".to_owned()));
    let saw_restore_intent = Arc::new(Mutex::new(false));
    let (listed, shown) = (Arc::clone(&split), Arc::clone(&label));
    space.on_read(move |mux, _| {
        if *listed.lock().unwrap() {
            mux.show_pane("pane-2");
        } else {
            mux.hide_pane("pane-2");
        }
        mux.set_tab_label("tab-1", Some(shown.lock().unwrap().as_str()));
        Ok(())
    });
    let (database_path, renamed, saw) = (
        directory.path().join("namedrop.sqlite3"),
        Arc::clone(&label),
        Arc::clone(&saw_restore_intent),
    );
    space.on_apply(move |_, effect| {
        if let Effect::RenameTab { tab, label } = effect
            && tab.as_str() == "tab-1"
        {
            if label == "My review tab" {
                *saw.lock().unwrap() = Connection::open(&database_path)
                    .unwrap()
                    .query_row(
                        "SELECT pending_label = 'My review tab'
                         FROM managed_tab_labels WHERE tab_id = 'tab-1'",
                        [],
                        |row| row.get(0),
                    )
                    .unwrap();
            }
            *renamed.lock().unwrap() = label.clone();
        }
        Ok(())
    });
    let herdr = ManagedLabelHerdr {
        split,
        label,
        saw_restore_intent,
        space,
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
        mux: &*herdr.space,
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
        session: Arc<Mutex<Option<&'static str>>>,
        space: StrictSpace,
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
    let space = lifecycle_space(
        repo_world()
            .pane("pane-1")
            .terminal("terminal-1")
            .tab("tab-1")
            .codex()
            .agent_label("Codex")
            .idle()
            .reports_session(false)
            .build(),
        true,
        Takes::nothing()
            .effect(metadata_of("pane-1"))
            .effect(renames_tab_1(Some("@builder"))),
    );
    main_tab_labels(&space, Some("tab-1"));
    let session = Arc::new(Mutex::new(None));
    let reported = Arc::clone(&session);
    space.on_read(move |mux, _| {
        match *reported.lock().unwrap() {
            Some(session) => mux.set_session("pane-1", session),
            None => mux.omit_session("pane-1"),
        }
        Ok(())
    });
    let herdr = PendingSessionHerdr { session, space };
    let ids = PendingSessionIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-1"),
        session_socket_id: "socket-a",
        mux: &*herdr.space,
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
    struct UpgradeHerdr {
        prompts: Arc<Mutex<Vec<String>>>,
        space: StrictSpace,
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
    let space = lifecycle_space(
        repo_world()
            .pane("pane-1")
            .terminal("terminal-1")
            .tab("tab-1")
            .codex()
            .session("session-1")
            .idle()
            .build(),
        false,
        Takes::nothing()
            .input(|attempt| {
                attempt.pane.as_str() == "pane-1" && matches!(attempt.input, SentInput::Prompt(_))
            })
            .effect(metadata_of("pane-1"))
            .effect(renames_tab_1(Some("@reviewer"))),
    );
    main_tab_labels(&space, Some("tab-1"));
    space.answer("pane-1", InputOutcome::Delivered);
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let sent = Arc::clone(&prompts);
    space.on_input(move |_, attempt| {
        if let SentInput::Prompt(prompt) = &attempt.input
            && attempt.pane.as_str() == "pane-1"
        {
            sent.lock().unwrap().push(prompt.clone());
        }
    });
    let herdr = UpgradeHerdr { prompts, space };
    let ids = UpgradeIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-1"),
        session_socket_id: "socket-a",
        mux: &*herdr.space,
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
    let space = lifecycle_space(repo_world().build(), false, Takes::nothing());
    let pane_lists = Arc::new(AtomicU64::new(0));
    let counted = Arc::clone(&pane_lists);
    space.on_command(move |_, command| {
        if command.listing == Some(Listing::PanesOf) {
            counted.fetch_add(1, Ordering::Relaxed);
        }
        Ok(())
    });
    let runtime = namedrop::LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: None,
        session_socket_id: "socket-a",
        mux: &*space,
        clock: &FixedClock,
        ids: &FixedIds,
    };

    execute_lifecycle_operation("reconcile", serde_json::json!({}), &runtime).unwrap();

    assert_eq!(
        pane_lists.load(Ordering::Relaxed),
        2,
        "initial discovery and delivery drain must each observe host state"
    );
}

#[test]
fn activation_keeps_dispatch_gated_when_initial_reconciliation_fails() {
    let space = lifecycle_space(repo_world().build(), false, Takes::nothing());
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
        mux: &*space,
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
    let space = lifecycle_space(
        repo_world()
            .pane("pane-live")
            .terminal("terminal-live")
            .tab("tab-live")
            .cwd("/repo")
            .session("session-live")
            .idle()
            .build(),
        false,
        Takes::nothing(),
    );

    struct SequenceIds;
    impl IdGenerator for SequenceIds {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-reconcile")
        }
    }

    let discovered = discover_worktree(&*space, Path::new("/repo")).unwrap();
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
fn agent_session_lookup_needs_one_exact_herdr_pane() {
    let space = StrictSpace::new(
        FakeMux::world()
            .space("w1")
            .pane("w1:p1")
            .terminal("t")
            .tab("tab")
            .session("solo")
            .space("w2")
            .pane("w2:p1")
            .terminal("t")
            .tab("tab")
            .session("twice")
            .space("w3")
            .pane("w3:p1")
            .terminal("t")
            .tab("tab")
            .session("twice")
            .build(),
        &[(ScopeKind::All, Depth::Panes)],
        Takes::nothing(),
    );

    assert_eq!(
        locate_agent_session_pane(&*space, "solo"),
        Some(("w1".to_owned(), "w1:p1".to_owned()))
    );
    assert_eq!(locate_agent_session_pane(&*space, "twice"), None);
    assert_eq!(locate_agent_session_pane(&*space, "missing"), None);
    assert!(space.reads().iter().all(|read| *read
        == Read::Survey {
            scope: Surveyed::All,
            depth: Depth::Panes
        }));
}

#[test]
fn a_known_pane_names_its_space() {
    let space = StrictSpace::new(
        FakeMux::world()
            .space("w1")
            .pane("w1:p1")
            .terminal("t1")
            .tab("tab-1")
            .reports_session(false)
            .space("w2")
            .pane("w2:p1")
            .terminal("t2")
            .tab("tab-2")
            .reports_session(false)
            .build(),
        &[(ScopeKind::All, Depth::Panes)],
        Takes::nothing(),
    );

    assert_eq!(pane_workspace(&*space, "w2:p1").unwrap(), "w2");
    let error = pane_workspace(&*space, "w9:p1").unwrap_err();
    assert!(error.contains("w9:p1"), "{error}");
    assert!(error.contains("--herdr-workspace"), "{error}");
    assert!(space.reads().iter().all(|read| *read
        == Read::Survey {
            scope: Surveyed::All,
            depth: Depth::Panes
        }));
}

#[test]
fn only_proven_same_worktree_moves_preserve_a_binding() {
    let space = lifecycle_space(
        repo_world()
            .space("workspace-2")
            .checkout("/other")
            .unstamped()
            .unlisted()
            .build(),
        true,
        Takes::nothing(),
    );
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
        mux: &*space,
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
    let space = lifecycle_space(
        repo_world()
            .pane("pane-live")
            .terminal("terminal-live")
            .tab("tab-live")
            .cwd("/repo")
            .title("Review pane")
            .codex()
            .agent_label("Codex")
            .session("session-live")
            .idle()
            .build(),
        false,
        Takes::nothing().input(|attempt| {
            attempt.pane.as_str() == "pane-live" && matches!(attempt.input, SentInput::Prompt(_))
        }),
    );
    // The old fake failed the registration prompt, which a legacy executor
    // reports as possibly typed.
    space.answer(
        "pane-live",
        InputOutcome::Ambiguous("unexpected Herdr command: agent prompt".to_owned()),
    );

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
        mux: &*space,
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
    assert_eq!(rules.matches("prefix_rule(").count(), 22);
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
    let space = lifecycle_space(
        repo_world()
            .pane("pane-live")
            .terminal("terminal-live")
            .tab("tab-live")
            .cwd("/repo")
            .build(),
        false,
        Takes::nothing(),
    );

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
        mux: &*space,
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
    assert_eq!(deactivated["subagent_hooks"]["removed_entries"], 12);
    for agent_runtime in [AgentRuntime::ClaudeCode, AgentRuntime::Codex] {
        let package = package_directory(directory.home(), agent_runtime);
        assert!(!package.join("scripts/subagent").exists());
        assert!(package.join("scripts/session-start").exists());
        assert!(package.join("scripts/user-prompt-submit").exists());
    }
}

#[test]
fn strict_inputs_and_activation_confirmation_fail_before_mutation() {
    let space = StrictSpace::new(repo_world().build(), CHECKOUT_LISTINGS, Takes::nothing());
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
        mux: &*space,
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
        occupant: Arc<Mutex<Option<(&'static str, &'static str)>>>,
        space: StrictSpace,
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
    let space = lifecycle_space(
        repo_world()
            .pane("pane-live")
            .terminal("terminal-live")
            .tab("tab-live")
            .title("Main")
            .codex()
            .agent_label("Codex")
            .session("session-1")
            .working()
            .build(),
        true,
        Takes::nothing()
            .input(|attempt| {
                attempt.pane.as_str() == "pane-live"
                    && matches!(attempt.input, SentInput::Prompt(_))
            })
            .effect(metadata_of("pane-live")),
    );
    // The old fake failed the registration prompt, which a legacy executor
    // reports as possibly typed.
    space.answer(
        "pane-live",
        InputOutcome::Ambiguous("unexpected Herdr command: agent prompt".to_owned()),
    );
    let occupant = Arc::new(Mutex::new(Some(("session-1", "working"))));
    let current = Arc::clone(&occupant);
    space.on_read(move |mux, _| {
        match *current.lock().unwrap() {
            Some((session, status)) => {
                mux.set_session("pane-live", session);
                mux.set_state("pane-live", AgentState::parse(status));
            }
            None => {
                mux.clear_session("pane-live");
                mux.set_agent("pane-live", None);
                mux.clear_state("pane-live");
            }
        }
        Ok(())
    });
    let herdr = DynamicHerdr { occupant, space };
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
        mux: &*herdr.space,
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
        open: Arc<Mutex<bool>>,
        space: StrictSpace,
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
    let space = lifecycle_space(
        repo_world()
            .pane("pane-lead")
            .terminal("terminal-lead")
            .tab("tab-collaboration")
            .codex()
            .session("session-lead")
            .idle()
            .pane("pane-reviewer")
            .terminal("terminal-reviewer")
            .tab("tab-collaboration")
            .claude()
            .session("session-reviewer")
            .idle()
            .build(),
        true,
        Takes::prompts_and_metadata(),
    );
    for pane in ["pane-lead", "pane-reviewer"] {
        space.answer(pane, InputOutcome::Delivered);
    }
    let open = Arc::new(Mutex::new(true));
    let listed = Arc::clone(&open);
    space.on_read(move |mux, _| {
        for pane in ["pane-lead", "pane-reviewer"] {
            if *listed.lock().unwrap() {
                mux.show_pane(pane);
            } else {
                mux.hide_pane(pane);
            }
        }
        Ok(())
    });
    let herdr = ClosingTabHerdr { open, space };
    let ids = ClosingTabIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-lead"),
        session_socket_id: "socket-a",
        mux: &*herdr.space,
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
        sessions: Arc<Mutex<(&'static str, &'static str)>>,
        prompts: Arc<Mutex<Vec<(String, String)>>>,
        space: StrictSpace,
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

    let space = lifecycle_space(
        repo_world()
            .pane("pane-first")
            .terminal("terminal-first")
            .tab("tab-1")
            .codex()
            .agent_label("Codex")
            .session("first-1")
            .idle()
            .pane("pane-second")
            .terminal("terminal-second")
            .tab("tab-1")
            .claude()
            .agent_label("Claude Code")
            .session("second-1")
            .idle()
            .pane("pane-author")
            .terminal("terminal-author")
            .tab("tab-1")
            .state(AgentState::parse("unknown"))
            .build(),
        true,
        Takes::prompts_and_metadata(),
    );
    for pane in ["pane-first", "pane-second", "pane-author"] {
        space.answer(pane, InputOutcome::Delivered);
    }
    let sessions = Arc::new(Mutex::new(("first-1", "second-1")));
    let current = Arc::clone(&sessions);
    space.on_read(move |mux, _| {
        let (first, second) = *current.lock().unwrap();
        mux.set_session("pane-first", first);
        mux.set_session("pane-second", second);
        Ok(())
    });
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let sent = Arc::clone(&prompts);
    space.on_input(move |_, attempt| {
        if let SentInput::Prompt(prompt) = &attempt.input {
            sent.lock()
                .unwrap()
                .push((attempt.pane.as_str().to_owned(), prompt.clone()));
        }
    });
    let herdr = ReplacedSessionsHerdr {
        sessions,
        prompts,
        space,
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
        mux: &*herdr.space,
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
    let sent_drain = namedrop::drain_mailboxes(&author).unwrap();
    assert_eq!(sent["deliveries"][0]["state"], "queued");
    assert_eq!(sent["deliveries"][1]["state"], "queued");
    assert!(sent_drain.submitted.is_empty());

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
        delivered::envelope_of(prompt)["untrusted_body"] == "Reply with exactly: FANOUT PASS"
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
fn setup_herdr() -> StrictSpace {
    let space = lifecycle_space(
        repo_world()
            .pane("pane-claude")
            .terminal("terminal-claude")
            .tab("tab-claude")
            .title("Planning")
            .claude()
            .agent_label("Claude Code")
            .session("session-claude")
            .idle()
            .pane("pane-codex")
            .terminal("terminal-codex")
            .tab("tab-codex")
            .title("Implementation")
            .codex()
            .agent_label("Codex")
            .session("session-codex")
            .working()
            .pane("pane-shell")
            .terminal("terminal-shell")
            .tab("tab-shell")
            .title("Shell")
            .build(),
        true,
        Takes::prompts_and_metadata(),
    );
    for pane in ["pane-claude", "pane-codex", "pane-shell"] {
        space.answer(pane, InputOutcome::Delivered);
    }
    space
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
    let herdr = setup_herdr();
    let ids = SetupIds(AtomicU64::new(1));
    let runtime = namedrop::LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: None,
        session_socket_id: "socket-a",
        mux: &*herdr,
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
    shell: Arc<Mutex<bool>>,
    agent: Arc<Mutex<Option<&'static str>>>,
    session: Arc<Mutex<Option<&'static str>>>,
    shell_session: Arc<Mutex<Option<&'static str>>>,
    label: Arc<Mutex<String>>,
    idle_label: Arc<Mutex<Option<String>>>,
    identity_reports: Arc<Mutex<Vec<String>>>,
    space: StrictSpace,
}

impl AgentTabHerdr {
    fn new(shell: bool, session: Option<&'static str>) -> Self {
        let space = lifecycle_space(
            repo_world()
                .pane("pane-1")
                .terminal("terminal-1")
                .tab("tab-1")
                .codex()
                .idle()
                .pane("pane-2")
                .terminal("terminal-2")
                .tab("tab-1")
                .state(AgentState::parse("unknown"))
                .reports_session(false)
                .build(),
            true,
            Takes::nothing()
                .effect(metadata_of("pane-1"))
                .effect(renames_tab_1(None)),
        );
        let herdr = Self {
            shell: Arc::new(Mutex::new(shell)),
            agent: Arc::new(Mutex::new(Some("codex"))),
            session: Arc::new(Mutex::new(session)),
            shell_session: Arc::new(Mutex::new(None)),
            label: Arc::new(Mutex::new("Main".to_owned())),
            idle_label: Arc::new(Mutex::new(None)),
            identity_reports: Arc::new(Mutex::new(Vec::new())),
            space,
        };
        let (shell, agent, session, shell_session, label, idle_label) = (
            Arc::clone(&herdr.shell),
            Arc::clone(&herdr.agent),
            Arc::clone(&herdr.session),
            Arc::clone(&herdr.shell_session),
            Arc::clone(&herdr.label),
            Arc::clone(&herdr.idle_label),
        );
        herdr.space.on_read(move |mux, _| {
            mux.set_agent(
                "pane-1",
                match *agent.lock().unwrap() {
                    Some("codex") => Some(AgentKind::Known(AgentRuntime::Codex)),
                    Some(other) => panic!("the fixture has no agent {other}"),
                    None => None,
                },
            );
            match *session.lock().unwrap() {
                Some(session) => mux.set_session("pane-1", session),
                None => mux.omit_session("pane-1"),
            }
            mux.set_idle_label("pane-1", idle_label.lock().unwrap().as_deref());
            if *shell.lock().unwrap() {
                mux.show_pane("pane-2");
            } else {
                mux.hide_pane("pane-2");
            }
            if let Some(session) = *shell_session.lock().unwrap() {
                mux.set_session("pane-2", session);
            }
            mux.set_tab_label("tab-1", Some(label.lock().unwrap().as_str()));
            Ok(())
        });
        let (reports, published, renamed) = (
            Arc::clone(&herdr.identity_reports),
            Arc::clone(&herdr.idle_label),
            Arc::clone(&herdr.label),
        );
        herdr.space.on_apply(move |_, effect| {
            match effect {
                Effect::Identify { pane, texts, .. } if pane.as_str() == "pane-1" => {
                    reports.lock().unwrap().push(texts.ready.clone());
                    *published.lock().unwrap() = Some(texts.ready.clone());
                }
                Effect::RenameTab { tab, label } if tab.as_str() == "tab-1" => {
                    *renamed.lock().unwrap() = label.clone();
                }
                _ => {}
            }
            Ok(())
        });
        herdr
    }
}

fn agent_tab_runtime<'a>(
    directory: &'a TestDirectory,
    herdr: &'a AgentTabHerdr,
    ids: &'a FixedIds,
) -> LifecycleRuntime<'a, FakeMux, FixedClock, FixedIds> {
    LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-1"),
        session_socket_id: "socket-a",
        mux: &herdr.space,
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

/// Herdr can report a session in a pane without the agent word that names
/// what runs there. The pane is an agent pane, but not a recognized agent.
#[test]
fn naming_a_pane_herdr_reports_only_a_session_for_succeeds() {
    let directory = TestDirectory::new();
    let herdr = AgentTabHerdr::new(false, Some("session-1"));
    *herdr.agent.lock().unwrap() = None;
    let ids = FixedIds(AtomicU64::new(1));
    let runtime = agent_tab_runtime(&directory, &herdr, &ids);

    let named =
        execute_lifecycle_operation("name", serde_json::json!({"alias": "reviewer"}), &runtime)
            .unwrap();
    assert_eq!(named["pane_type"], "agent");
    let listed = execute_lifecycle_operation("address.list", serde_json::json!({}), &runtime)
        .unwrap()
        .to_string();
    assert!(listed.contains("\"alias\":\"reviewer\""), "{listed}");
}

#[test]
fn a_session_only_pane_beside_a_named_agent_does_not_take_its_tab_label() {
    let directory = TestDirectory::new();
    let herdr = AgentTabHerdr::new(true, Some("session-1"));
    *herdr.shell_session.lock().unwrap() = Some("session-2");
    let ids = FixedIds(AtomicU64::new(1));
    let runtime = agent_tab_runtime(&directory, &herdr, &ids);

    execute_lifecycle_operation("name", serde_json::json!({"alias": "reviewer"}), &runtime)
        .unwrap();
    assert_eq!(
        *herdr.label.lock().unwrap(),
        "@reviewer",
        "naming beside a session-only pane still labels the tab"
    );

    *herdr.shell.lock().unwrap() = false;
    execute_lifecycle_operation("reconcile", serde_json::json!({}), &runtime).unwrap();
    *herdr.shell.lock().unwrap() = true;
    execute_lifecycle_operation("reconcile", serde_json::json!({}), &runtime).unwrap();
    assert_eq!(
        *herdr.label.lock().unwrap(),
        "@reviewer",
        "a session-only pane opening beside the agent keeps its label"
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

/// A space whose agent panes can appear after activation, and which opens
/// every tour popup, so tests can count them.
struct TourHerdr {
    agents: Arc<Mutex<bool>>,
    /// The shell pane reports a session but no agent word.
    session_only_shell: Arc<Mutex<bool>>,
    space: StrictSpace,
}

impl TourHerdr {
    fn new(agents: bool) -> Self {
        let mux = repo_world()
            .pane("pane-shell")
            .tab("tab-pane-shell")
            .reports_session(false)
            .pane("pane-a")
            .tab("tab-pane-a")
            .reports_session(false)
            .pane("pane-b")
            .tab("tab-pane-b")
            .reports_session(false)
            .build();
        let agents = Arc::new(Mutex::new(agents));
        let session_only_shell = Arc::new(Mutex::new(false));
        let (listed, shell) = (Arc::clone(&agents), Arc::clone(&session_only_shell));
        mux.on_read(move |mux, read| {
            if let Read::TabLabel(tab) = read {
                mux.set_tab_label(tab.as_str(), Some("Main"));
            }
            if *listed.lock().unwrap() {
                for pane in ["pane-a", "pane-b"] {
                    mux.set_agent(pane, Some(AgentKind::Known(AgentRuntime::Codex)));
                    mux.set_session(pane, &format!("session-{pane}"));
                    mux.set_state(pane, AgentState::Idle);
                }
            }
            if *shell.lock().unwrap() {
                mux.set_session("pane-shell", "session-pane-shell");
            }
            Ok(())
        });
        let failed =
            |effect: &Effect| matches!(effect, Effect::ReloadKeybindings | Effect::Notify { .. });
        mux.on_apply(move |_, effect| {
            if failed(effect) {
                return Err(MuxError::Unavailable(format!(
                    "unexpected Herdr command: {effect:?}"
                )));
            }
            Ok(())
        });
        let takes = Takes::nothing().effect(move |effect| {
            failed(effect)
                || matches!(
                    effect,
                    Effect::Identify { .. }
                        | Effect::Badges { .. }
                        | Effect::RenameTab { .. }
                        | Effect::OpenPopup(_)
                )
        });
        let mut surveys = CHECKOUT_LISTINGS.to_vec();
        surveys.push((ScopeKind::Space, Depth::Panes));
        Self {
            agents,
            session_only_shell,
            space: StrictSpace::new(mux, &surveys, takes),
        }
    }

    fn popup_opens(&self) -> Vec<Vec<String>> {
        self.space
            .attempted_effects()
            .iter()
            .filter_map(|effect| match effect {
                Effect::OpenPopup(popup) => Some(popup_names(popup)),
                _ => None,
            })
            .collect()
    }
}

/// What a popup names: its screen, then the values of the
/// `NAMEDROP_TARGET_WORKSPACE`, `_PANE`, `_TERMINAL`, and `_SESSION`,
/// `HERDR_PLUGIN_STATE_DIR`, and `HERDR_SOCKET_PATH` that the adapter's argv
/// pin in `src/mux/herdr/tests.rs` passes to `plugin pane open`.
fn popup_names(popup: &PopupSpec) -> Vec<String> {
    let session = match popup.target.agent.as_ref().map(|agent| &agent.session) {
        Some(SessionClaim::Reported(session)) => session.as_str(),
        _ => "",
    };
    vec![
        format!("{:?}", popup.screen),
        popup.target.space.as_str().to_owned(),
        popup.target.id.as_str().to_owned(),
        popup.target.terminal.as_str().to_owned(),
        session.to_owned(),
        popup.state_dir.display().to_string(),
        popup.socket.clone(),
    ]
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
) -> LifecycleRuntime<'a, FakeMux, TourClock, TourIds> {
    LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: None,
        session_socket_id: "socket-a",
        mux: &herdr.space,
        clock: &TourClock,
        ids,
    }
}

fn expected_tour_open(directory: &TestDirectory, pane: &str) -> Vec<String> {
    vec![
        format!("{:?}", PopupScreen::Tour),
        "workspace-1".to_owned(),
        pane.to_owned(),
        format!("terminal-{pane}"),
        format!("session-{pane}"),
        directory.path().display().to_string(),
        "socket-a".to_owned(),
    ]
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
            "popup": {"opened_for": "pane-b"},
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
fn the_tour_skips_a_pane_herdr_reports_only_a_session_for() {
    let ids = TourIds(AtomicU64::new(1));
    let directory = TestDirectory::new();
    let herdr = TourHerdr::new(false);
    *herdr.session_only_shell.lock().unwrap() = true;
    let runtime = tour_runtime(&directory, &herdr, &ids);
    let activated =
        execute_lifecycle_operation("activate", serde_json::json!({"confirmed": true}), &runtime)
            .unwrap();
    assert_eq!(activated["tour"]["shown"], false);
    assert_eq!(activated["tour"]["reason"], "no_agent_pane");
    assert!(herdr.popup_opens().is_empty());

    let fresh = TestDirectory::new();
    let herdr = TourHerdr::new(true);
    *herdr.session_only_shell.lock().unwrap() = true;
    let runtime = tour_runtime(&fresh, &herdr, &ids);
    let activated =
        execute_lifecycle_operation("activate", serde_json::json!({"confirmed": true}), &runtime)
            .unwrap();
    assert_eq!(activated["tour"]["pane_id"], "pane-a");
    assert_eq!(herdr.popup_opens(), [expected_tour_open(&fresh, "pane-a")]);
}

#[test]
fn name_refuses_a_retired_or_renamed_away_alias_with_a_clear_message() {
    let space = lifecycle_space(
        repo_world()
            .pane("pane-first")
            .terminal("terminal-first")
            .tab("tab-first")
            .cwd("/repo")
            .pane("pane-second")
            .terminal("terminal-second")
            .tab("tab-second")
            .cwd("/repo")
            .pane("pane-third")
            .terminal("terminal-third")
            .tab("tab-third")
            .cwd("/repo")
            .build(),
        false,
        Takes::nothing().effect(|effect| {
            matches!(
                effect,
                Effect::Identify { .. } | Effect::Badges { .. } | Effect::RenameTab { .. }
            )
        }),
    );
    main_tab_labels(&space, None);

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
        mux: &*space,
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
    let name = |runtime: &LifecycleRuntime<'_, _, _, _>, alias: &str| {
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

#[test]
fn trusted_state_worktrees_are_refused_by_activation_naming_and_doctor() {
    struct FixedClock;
    impl Clock for FixedClock {
        fn now_unix_ms(&self) -> u64 {
            42
        }
    }
    struct Ids(AtomicU64);
    impl IdGenerator for Ids {
        fn next_id(&self, kind: &str) -> String {
            format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
        }
    }
    for variant in [
        "state",
        "ancestor",
        "linked-ancestor",
        "outside",
        "linked-outside",
    ] {
        let directory = TestDirectory::new();
        let state = directory.path().join("state");
        fs::create_dir(&state).unwrap();
        let root = match variant {
            "state" => state.clone(),
            "ancestor" => directory.path().to_path_buf(),
            "outside" | "linked-outside" => {
                let outside = directory.path().join("repo");
                fs::create_dir(&outside).unwrap();
                if variant == "linked-outside" {
                    let link = directory.path().join("repo-alias");
                    std::os::unix::fs::symlink(&outside, &link).unwrap();
                    link
                } else {
                    outside
                }
            }
            _ => {
                let link = directory.path().join("linked-root");
                std::os::unix::fs::symlink(directory.path(), &link).unwrap();
                link
            }
        };
        let space = StrictSpace::new(
            FakeMux::world()
                .endpoint("socket-a")
                .space("workspace-1")
                .checkout(&root)
                .unstamped()
                .pane("pane-1")
                .terminal("terminal-1")
                .tab("tab-1")
                .codex()
                .session("session-1")
                .idle()
                .build(),
            EVERY_LISTING,
            Takes::nothing().input(|_| true).effect(|_| true),
        );
        space.answer("pane-1", InputOutcome::Delivered);
        let state = if variant == "linked-outside" {
            let alias = directory.path().join("state-parent-alias");
            std::os::unix::fs::symlink(directory.path(), &alias).unwrap();
            alias.join("state")
        } else {
            state
        };
        let ids = Ids(AtomicU64::new(1));
        let runtime = LifecycleRuntime {
            state_directory: &state,
            home_directory: Some(directory.home()),
            agent_config: namedrop::AgentConfigDirectories::default(),
            cwd: Path::new("/untrusted-cwd"),
            workspace_id: Some("workspace-1"),
            current_pane_id: Some("pane-1"),
            session_socket_id: "socket-a",
            mux: &*space,
            clock: &FixedClock,
            ids: &ids,
        };
        if variant.ends_with("outside") {
            let result = execute_lifecycle_operation(
                "activate",
                serde_json::json!({"confirmed":true}),
                &runtime,
            )
            .unwrap();
            assert_eq!(
                result["codex_sandbox"]["writable_root"],
                state
                    .canonicalize()
                    .unwrap()
                    .join("deferred-commands")
                    .to_str()
                    .unwrap()
            );
            let report = namedrop::diagnose_collaboration(&runtime);
            assert!(
                !report
                    .issues
                    .iter()
                    .any(|issue| issue.explanation.contains("trusted Namedrop state"))
            );
            continue;
        }
        for (operation, input) in [
            ("activate", serde_json::json!({"confirmed":true})),
            ("name", serde_json::json!({"alias":"reviewer"})),
        ] {
            let error = execute_lifecycle_operation(operation, input, &runtime).unwrap_err();
            assert!(
                error.contains("worktree") && error.contains("trusted Namedrop state"),
                "{variant} {operation}: {error}"
            );
            assert!(!directory.home().join(".codex/config.toml").exists());
        }
        let inactive_report = namedrop::diagnose_collaboration(&runtime);
        assert!(
            inactive_report
                .issues
                .iter()
                .any(|issue| issue.code == "ND-INTEGRATION-001"
                    && issue.explanation.contains("trusted Namedrop state"))
        );
        StateStore::open(&state)
            .unwrap()
            .transaction(|transaction| {
                transaction.activate_worktree(&namespace(), &attribution(42))
            })
            .unwrap();
        let report = namedrop::diagnose_collaboration(&runtime);
        assert!(
            report
                .issues
                .iter()
                .any(|issue| issue.code == "ND-INTEGRATION-001"
                    && issue.explanation.contains("trusted Namedrop state")),
            "{variant}: {report:?}"
        );
    }
}
