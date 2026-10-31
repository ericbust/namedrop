use namedrop::{
    ActorKind, AgentRuntime, AgentSessionId, Clock, HerdrCommandExecutor, HerdrCommandOutput,
    IdGenerator, LifecycleRuntime, MutationAttribution, PaneAddressDraft, PaneAddressId,
    PaneBindingId, PaneRoleId, StateNamespace, StateStore, complete_capability_profile,
    execute_context_operation, execute_human_context_operation, execute_integration_operation,
    execute_message_operation, issue_registration_challenge,
};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

struct TestDirectory {
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    /// Stands in for the user's home directory, so no test installs an Agent
    /// Integration package into the developer's real user-level skill roots.
    home: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-context-test-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        let repo = root.join("repo");
        let state = root.join("state");
        let home = root.join("home");
        fs::create_dir_all(repo.join("src")).unwrap();
        fs::create_dir_all(&home).unwrap();
        fs::write(
            repo.join("src/notes.txt"),
            "safe line\nTOKEN=top-secret-value\nghp_abcdefghijklmnopqrstuvwxyz1234567890\n",
        )
        .unwrap();
        fs::write(root.join("outside.txt"), "outside evidence").unwrap();
        Self {
            root,
            repo,
            state,
            home,
        }
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

struct FakeHerdr {
    worktree: std::sync::Mutex<String>,
    /// Report this Space as not being a Git Worktree at all.
    no_worktree: std::sync::Mutex<bool>,
    live_pane_override: std::sync::Mutex<Option<(String, Option<String>)>>,
    /// Serialized Delivery Envelopes handed to `agent prompt`, per pane.
    prompts: std::sync::Mutex<Vec<(String, String)>>,
}

impl FakeHerdr {
    fn new(worktree: &TestDirectory) -> Self {
        Self {
            worktree: std::sync::Mutex::new(worktree.repo.display().to_string()),
            no_worktree: std::sync::Mutex::new(false),
            live_pane_override: std::sync::Mutex::new(None),
            prompts: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Herdr's answer for either Worktree lookup. A Herdr pane exports
    /// `HERDR_WORKSPACE_ID`, so an installed session resolves its Space by
    /// workspace id rather than by the invoking process's directory; either way
    /// the Space's Worktree path comes from Herdr. `no_worktree` makes it report
    /// a Space that is not a Git Worktree, the shape Namedrop recognizes.
    fn worktree_list(&self) -> Result<String, String> {
        if *self.no_worktree.lock().unwrap() {
            return Err(serde_json::json!({"error":{"code":"not_git_worktree"}}).to_string());
        }
        Ok(serde_json::json!({
            "result": {
                "worktrees": [{
                    "path": *self.worktree.lock().unwrap(),
                    "open_workspace_id": "workspace-1"
                }]
            }
        })
        .to_string())
    }

    fn prompt_for_pane(&self, pane_id: &str) -> serde_json::Value {
        let prompts = self.prompts.lock().unwrap();
        let (_, envelope) = prompts
            .iter()
            .rev()
            .find(|(pane, _)| pane == pane_id)
            .unwrap_or_else(|| panic!("no envelope was dispatched to {pane_id}"));
        serde_json::from_str(envelope).unwrap()
    }
}

impl HerdrCommandExecutor for FakeHerdr {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let mut stdout = match args {
            ["workspace", "list"] => {
                serde_json::json!({"result":{"workspaces":[{"workspace_id":"workspace-1"}]}})
                    .to_string()
            }
            ["worktree", "list", "--cwd", cwd, "--json"]
                if *cwd == *self.worktree.lock().unwrap() =>
            {
                self.worktree_list()?
            }
            ["worktree", "list", "--workspace", "workspace-1", "--json"] => self.worktree_list()?,
            ["pane", "list", "--workspace", "workspace-1"] => serde_json::json!({
                "result": {
                    "panes": [
                        {
                            "pane_id": "pane-source",
                            "terminal_id": "terminal-source",
                            "tab_id": "tab-1",
                            "workspace_id": "workspace-1",
                            "agent_session": {"value": "session-source"},
                            "agent_status": "idle"
                        },
                        {
                            "pane_id": "pane-reviewer",
                            "terminal_id": "terminal-reviewer",
                            "tab_id": "tab-1",
                            "workspace_id": "workspace-1",
                            "agent_session": {"value": "session-reviewer"},
                            "agent_status": "idle"
                        },
                        {
                            "pane_id": "pane-researcher",
                            "terminal_id": "terminal-researcher",
                            "tab_id": "tab-1",
                            "workspace_id": "workspace-1",
                            "agent_session": {"value": "session-researcher"},
                            "agent_status": "idle"
                        }
                    ]
                }
            })
            .to_string(),
            [
                "pane",
                "read",
                "pane-source",
                "--source",
                "recent-unwrapped",
                "--lines",
                "40",
            ] => "pane output café\nAWS_SECRET_ACCESS_KEY=do-not-share\n".to_owned(),
            ["agent", "prompt", pane_id, envelope] => {
                self.prompts
                    .lock()
                    .unwrap()
                    .push(((*pane_id).to_owned(), (*envelope).to_owned()));
                "{}".to_owned()
            }
            [
                "pane",
                "report-metadata",
                _,
                "--source",
                "namedrop",
                "--token",
                _,
                "--token",
                _,
                "--token",
                _,
            ] => "{}".to_owned(),
            _ => return Err(format!("unexpected Herdr command: {args:?}")),
        };
        if args == ["pane", "list", "--workspace", "workspace-1"]
            && let Some((pane_id, session)) = self.live_pane_override.lock().unwrap().as_ref()
        {
            let mut value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
            let panes = value["result"]["panes"].as_array_mut().unwrap();
            if let Some(session) = session {
                panes
                    .iter_mut()
                    .find(|pane| pane["pane_id"] == *pane_id)
                    .unwrap()["agent_session"]["value"] = serde_json::json!(session);
            } else {
                panes.retain(|pane| pane["pane_id"] != *pane_id);
            }
            stdout = value.to_string();
        }
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

fn address(id: &str, alias: &str) -> PaneAddressDraft {
    PaneAddressDraft {
        id: PaneAddressId::new(id),
        role_id: PaneRoleId::new(format!("role-{id}")),
        role_name: alias.to_owned(),
        role_description: format!("{alias} test role"),
        alias: alias.to_owned(),
    }
}

fn setup(directory: &TestDirectory) {
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let attribution = MutationAttribution::new(ActorKind::Human, "test", 1);
    StateStore::open(&directory.state)
        .unwrap()
        .transaction(|transaction| {
            transaction.activate_worktree(&namespace, &attribution)?;
            for (draft, binding, pane, session) in [
                (
                    address("address-source", "source"),
                    "binding-source",
                    "pane-source",
                    "session-source",
                ),
                (
                    address("address-reviewer", "reviewer"),
                    "binding-reviewer",
                    "pane-reviewer",
                    "session-reviewer",
                ),
                (
                    address("address-researcher", "researcher"),
                    "binding-researcher",
                    "pane-researcher",
                    "session-researcher",
                ),
            ] {
                transaction.create_pane_address(&namespace, &draft, &attribution)?;
                transaction.bind_pane(
                    &namespace,
                    &PaneBindingId::new(binding),
                    &draft.id,
                    pane,
                    &attribution,
                )?;
                transaction.observe_agent_session(
                    &namespace,
                    &format!("observation-{session}"),
                    &draft.id,
                    &AgentSessionId::new(session),
                    "idle",
                    &attribution,
                )?;
            }
            Ok(())
        })
        .unwrap();
}

fn runtime<'a>(
    directory: &'a TestDirectory,
    herdr: &'a FakeHerdr,
    ids: &'a SequenceIds,
    pane_id: &'a str,
) -> LifecycleRuntime<'a, namedrop::TuiHost<&'a FakeHerdr>, FixedClock, SequenceIds> {
    LifecycleRuntime {
        state_directory: &directory.state,
        home_directory: Some(&directory.home),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: &directory.repo,
        workspace_id: None,
        current_pane_id: Some(pane_id),
        session_socket_id: "socket-a",
        host: namedrop::TuiHost::new(herdr),
        clock: &FixedClock,
        ids,
    }
}

fn register(
    address_id: &str,
    runtime: &LifecycleRuntime<'_, namedrop::TuiHost<&'_ FakeHerdr>, FixedClock, SequenceIds>,
) {
    let challenge = issue_registration_challenge(
        &PaneAddressId::new(address_id),
        AgentRuntime::Codex,
        runtime,
    )
    .unwrap();
    execute_integration_operation(
        "register",
        serde_json::json!({
            "runtime": "codex",
            "protocol_version": "collab/v1",
            "nonce": challenge.nonce,
            "capabilities": complete_capability_profile()
        }),
        runtime,
    )
    .unwrap();
}

fn create_referenced_thread(
    runtime: &LifecycleRuntime<'_, namedrop::TuiHost<&'_ FakeHerdr>, FixedClock, SequenceIds>,
) -> serde_json::Value {
    execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "request-context",
            "source": "@reviewer Please inspect @source."
        }),
        runtime,
    )
    .unwrap()
}

fn valid_snapshot() -> serde_json::Value {
    serde_json::json!({
        "objective": "Review the context boundary",
        "applicable_user_instructions": ["Do not expose TOKEN=top-secret-value"],
        "decisions": ["Use one deterministic disclosure projection"],
        "completed_work": ["Created the context reference"],
        "current_work": ["Reviewing ghp_abcdefghijklmnopqrstuvwxyz1234567890"],
        "blockers": [],
        "evidence": [
            {"kind": "pane_output", "locator": "current"},
            {"kind": "file", "locator": "src/notes.txt"}
        ]
    })
}

#[test]
fn root_independent_snapshot_can_be_published_and_read_without_a_worktree() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    *herdr.no_worktree.lock().unwrap() = true;
    let source = LifecycleRuntime {
        workspace_id: Some("workspace-1"),
        ..source
    };
    let reviewer = LifecycleRuntime {
        workspace_id: Some("workspace-1"),
        ..reviewer
    };
    for evidence in [
        serde_json::json!([]),
        serde_json::json!([{"kind":"pane_output", "locator":"current"}]),
    ] {
        let mut snapshot = valid_snapshot();
        snapshot["evidence"] = evidence;
        let published = execute_context_operation(
            "snapshot.publish",
            serde_json::json!({
                "thread_id": sent["thread_id"],
                "grantee_address_ids": ["address-reviewer"],
                "capture_git_state": false,
                "snapshot": snapshot
            }),
            &source,
        )
        .unwrap();
        let read = execute_context_operation(
            "context.read",
            serde_json::json!({
                "grant_id": published["grants"][0]["grant_id"],
                "selection": {"kind":"snapshot"}
            }),
            &reviewer,
        )
        .unwrap();
        assert_eq!(read["content"]["objective"], "Review the context boundary");
        assert_eq!(read["content"]["evidence"], snapshot["evidence"]);
        assert!(read["content"]["git_state"].is_null());
        assert!(!read["content"].to_string().contains("top-secret-value"));
    }
    assert!(!directory.repo.join(".git").exists());
}

#[test]
fn root_independent_context_inspection_preserves_authorization_without_a_worktree() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    let researcher = runtime(&directory, &herdr, &ids, "pane-researcher");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    register("address-researcher", &researcher);
    let sent = create_referenced_thread(&source);
    let reviewer = LifecycleRuntime {
        workspace_id: Some("workspace-1"),
        ..reviewer
    };
    let researcher = LifecycleRuntime {
        workspace_id: Some("workspace-1"),
        ..researcher
    };
    let inspect = serde_json::json!({
        "message_id":sent["message_id"], "reference_address_id":"address-source"
    });
    for evidence in [
        serde_json::json!([]),
        serde_json::json!([{"kind":"pane_output", "locator":"current"}]),
    ] {
        *herdr.no_worktree.lock().unwrap() = false;
        let mut snapshot = valid_snapshot();
        snapshot["evidence"] = evidence;
        let published = execute_context_operation(
            "snapshot.publish",
            serde_json::json!({
                "thread_id": sent["thread_id"],
                "grantee_address_ids": ["address-reviewer"],
                "capture_git_state": false,
                "snapshot": snapshot
            }),
            &source,
        )
        .unwrap();
        *herdr.no_worktree.lock().unwrap() = true;
        let inspected =
            execute_context_operation("context.inspect", inspect.clone(), &reviewer).unwrap();
        assert_eq!(inspected["authorized"], true);
        assert_eq!(inspected["grant_id"], published["grants"][0]["grant_id"]);
        let expected = if snapshot["evidence"].as_array().unwrap().is_empty() {
            serde_json::json!([])
        } else {
            serde_json::json!([{"kind":"pane_output", "locator":"current", "blocked":false}])
        };
        assert_eq!(inspected["evidence"], expected);
        assert!(
            execute_context_operation("context.inspect", inspect.clone(), &researcher)
                .unwrap_err()
                .contains("limited to Collaboration Thread participants")
        );
        assert!(
            execute_context_operation(
                "context.read",
                serde_json::json!({
                    "grant_id":published["grants"][0]["grant_id"],
                    "selection":{"kind":"snapshot"}
                }),
                &researcher,
            )
            .unwrap_err()
            .contains("non-delegable")
        );
    }
}

#[test]
fn a_context_reference_alone_discloses_no_context_and_starts_no_turn() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    let researcher = runtime(&directory, &herdr, &ids, "pane-researcher");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    register("address-researcher", &researcher);
    let sent = create_referenced_thread(&source);

    let inspection = execute_context_operation(
        "context.inspect",
        serde_json::json!({
            "message_id": sent["message_id"],
            "reference_address_id": "address-source"
        }),
        &reviewer,
    )
    .unwrap();

    assert_eq!(inspection["authorized"], false);
    assert_eq!(inspection["grant_state"], "none");
    assert_eq!(inspection["evidence"], serde_json::json!([]));
    assert_eq!(
        inspection["send_time"]["agent_session_id"],
        "session-source"
    );
    assert_eq!(inspection["current"]["drift"], false);
    assert!(
        execute_context_operation(
            "context.inspect",
            serde_json::json!({
                "message_id": sent["message_id"],
                "reference_address_id": "address-source"
            }),
            &researcher,
        )
        .unwrap_err()
        .contains("participants")
    );
}

#[test]
fn a_pending_grant_requires_a_valid_exact_session_snapshot_and_can_be_revoked() {
    let directory = TestDirectory::new();
    git_init_space(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);

    assert!(
        execute_context_operation(
            "context.grant",
            serde_json::json!({
                "thread_id": sent["thread_id"],
                "source_address_id": "address-source",
                "grantee_address_ids": ["address-reviewer"]
            }),
            &reviewer,
        )
        .unwrap_err()
        .contains("human action")
    );
    let pending = execute_human_context_operation(
        "context.grant",
        serde_json::json!({
            "thread_id": sent["thread_id"],
            "source_address_id": "address-source",
            "grantee_address_ids": ["address-reviewer"]
        }),
        &reviewer,
    )
    .unwrap();
    let grant_id = pending["grants"][0]["grant_id"].as_str().unwrap();
    assert_eq!(pending["grants"][0]["state"], "pending");

    let invalid = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "grant_ids": [grant_id],
            "snapshot": {
                "objective": "missing required fields"
            }
        }),
        &source,
    )
    .unwrap_err();
    assert!(invalid.contains("applicable_user_instructions"));
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    assert_eq!(
        StateStore::open(&directory.state)
            .unwrap()
            .transaction(|transaction| transaction.unresolved_attention_items(&namespace))
            .unwrap()
            .len(),
        1
    );
    let oversized = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "grant_ids": [grant_id],
            "snapshot": {
                "objective": "x".repeat(70_000),
                "applicable_user_instructions": [],
                "decisions": [],
                "completed_work": [],
                "current_work": [],
                "blockers": [],
                "evidence": []
            }
        }),
        &source,
    )
    .unwrap_err();
    assert!(oversized.contains("65536 bytes"));

    let still_pending = execute_context_operation(
        "context.inspect",
        serde_json::json!({
            "message_id": sent["message_id"],
            "reference_address_id": "address-source"
        }),
        &reviewer,
    )
    .unwrap();
    assert_eq!(still_pending["grant_state"], "pending");
    assert_eq!(still_pending["authorized"], false);

    let published = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "grant_ids": [grant_id],
            "snapshot": valid_snapshot()
        }),
        &source,
    )
    .unwrap();
    assert_eq!(published["grants"][0]["state"], "active");
    assert_eq!(
        StateStore::open(&directory.state)
            .unwrap()
            .transaction(|transaction| transaction.unresolved_attention_items(&namespace))
            .unwrap()
            .len(),
        0
    );

    let inspection = execute_context_operation(
        "context.inspect",
        serde_json::json!({
            "message_id": sent["message_id"],
            "reference_address_id": "address-source"
        }),
        &reviewer,
    )
    .unwrap();
    assert_eq!(inspection["authorized"], true);
    assert_eq!(inspection["grant_state"], "active");
    assert_eq!(inspection["evidence"].as_array().unwrap().len(), 2);
    let human_inspection = execute_human_context_operation(
        "context.inspect",
        serde_json::json!({
            "message_id": sent["message_id"],
            "reference_address_id": "address-source"
        }),
        &reviewer,
    )
    .unwrap();
    assert_eq!(human_inspection["authorized"], true);
    assert_eq!(human_inspection["grant_id"], grant_id);
    assert_eq!(human_inspection["evidence"].as_array().unwrap().len(), 2);

    let snapshot = execute_context_operation(
        "context.read",
        serde_json::json!({
            "grant_id": grant_id,
            "selection": {"kind": "snapshot"}
        }),
        &reviewer,
    )
    .unwrap();
    let disclosed = snapshot["content"].to_string();
    assert!(!disclosed.contains("top-secret-value"));
    assert!(!disclosed.contains("ghp_abcdefghijklmnopqrstuvwxyz1234567890"));
    assert!(disclosed.contains("[REDACTED"));

    let file = execute_context_operation(
        "context.read",
        serde_json::json!({
            "grant_id": grant_id,
            "selection": {
                "kind": "file",
                "locator": "src/notes.txt",
                "start_line": 1,
                "max_lines": 40
            }
        }),
        &reviewer,
    )
    .unwrap();
    assert!(file["content"].as_str().unwrap().contains("safe line"));
    assert!(
        !file["content"]
            .as_str()
            .unwrap()
            .contains("top-secret-value")
    );

    let revoked = execute_human_context_operation(
        "context.revoke",
        serde_json::json!({"grant_id": grant_id}),
        &reviewer,
    )
    .unwrap();
    assert_eq!(revoked["state"], "revoked");
    assert!(
        execute_context_operation(
            "context.read",
            serde_json::json!({
                "grant_id": grant_id,
                "selection": {"kind": "snapshot"}
            }),
            &reviewer,
        )
        .unwrap_err()
        .contains("active Context Grant")
    );
}

#[test]
fn grants_expire_on_binding_change_and_new_participants_inherit_only_a_thread_snapshot() {
    let directory = TestDirectory::new();
    git_init_space(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    let researcher = runtime(&directory, &herdr, &ids, "pane-researcher");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    register("address-researcher", &researcher);
    let sent = create_referenced_thread(&source);
    let published = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "thread_id": sent["thread_id"],
            "grantee_address_ids": ["address-reviewer"],
            "snapshot": valid_snapshot()
        }),
        &source,
    )
    .unwrap();
    let grant_id = published["grants"][0]["grant_id"].as_str().unwrap();

    let joined = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "request-add-researcher",
            "in_reply_to": sent["message_id"],
            "source": "@researcher Joining you now."
        }),
        &source,
    )
    .unwrap();
    assert_eq!(joined["thread_id"], sent["thread_id"]);
    let thread_context = execute_context_operation(
        "context.inspect",
        serde_json::json!({"thread_id": sent["thread_id"]}),
        &researcher,
    )
    .unwrap();
    assert_eq!(
        thread_context["thread_context_snapshot"]["participant_address_id"],
        "address-researcher"
    );
    assert_eq!(thread_context["grants"], serde_json::json!([]));
    assert!(
        thread_context["thread_context_snapshot"]["payload"]["current_state"]
            .as_str()
            .unwrap()
            .contains("Joining you now")
    );
    assert!(
        thread_context["thread_context_snapshot"]["payload"]
            .to_string()
            .len()
            < 4_096
    );

    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    StateStore::open(&directory.state)
        .unwrap()
        .transaction(|transaction| {
            let attribution = MutationAttribution::new(ActorKind::Human, "test", 50);
            transaction.unbind_pane(
                &namespace,
                &PaneAddressId::new("address-source"),
                &attribution,
            )?;
            transaction.bind_pane(
                &namespace,
                &PaneBindingId::new("binding-source-new"),
                &PaneAddressId::new("address-source"),
                "pane-source",
                &attribution,
            )?;
            transaction.observe_agent_session(
                &namespace,
                "observation-source-new-binding",
                &PaneAddressId::new("address-source"),
                &AgentSessionId::new("session-source"),
                "idle",
                &attribution,
            )
        })
        .unwrap();

    let inspection = execute_context_operation(
        "context.inspect",
        serde_json::json!({
            "message_id": sent["message_id"],
            "reference_address_id": "address-source"
        }),
        &reviewer,
    )
    .unwrap();
    assert_eq!(inspection["grant_state"], "expired");
    assert_eq!(inspection["authorized"], false);
    assert!(
        execute_context_operation(
            "context.read",
            serde_json::json!({
                "grant_id": grant_id,
                "selection": {"kind": "snapshot"}
            }),
            &reviewer,
        )
        .unwrap_err()
        .contains("active Context Grant")
    );
}

#[test]
fn bounded_reads_block_outside_and_credential_paths_unless_a_human_names_one_item() {
    let directory = TestDirectory::new();
    git_init_space(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    let published = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "thread_id": sent["thread_id"],
            "grantee_address_ids": ["address-reviewer"],
            "snapshot": {
                "objective": "Bound every evidence read",
                "applicable_user_instructions": [],
                "decisions": [],
                "completed_work": [],
                "current_work": [],
                "blockers": [],
                "evidence": [
                    {"kind": "file", "locator": "../outside.txt"},
                    {"kind": "file", "locator": ".env"},
                    {"kind": "commit", "locator": "deadbee:.env"},
                    {"kind": "pane_output", "locator": "current"}
                ]
            }
        }),
        &source,
    )
    .unwrap();
    let grant_id = published["grants"][0]["grant_id"].as_str().unwrap();
    let snapshot = execute_context_operation(
        "context.read",
        serde_json::json!({
            "grant_id": grant_id,
            "selection": {"kind": "snapshot"}
        }),
        &reviewer,
    )
    .unwrap();
    assert!(!snapshot["content"].to_string().contains("../outside.txt"));
    assert!(!snapshot["content"].to_string().contains("\".env\""));
    assert!(
        execute_context_operation(
            "snapshot.publish",
            serde_json::json!({
                "thread_id": sent["thread_id"],
                "grantee_address_ids": ["address-reviewer"],
                "snapshot": {
                    "objective": "Reject whole-commit reads",
                    "applicable_user_instructions": [],
                    "decisions": [],
                    "completed_work": [],
                    "current_work": [],
                    "blockers": [],
                    "evidence": [{"kind": "commit", "locator": "deadbee"}]
                }
            }),
            &source,
        )
        .unwrap_err()
        .contains("<commit>:<path>")
    );

    for locator in ["../outside.txt", ".env"] {
        let error = execute_context_operation(
            "context.read",
            serde_json::json!({
                "grant_id": grant_id,
                "selection": {
                    "kind": "file",
                    "locator": locator,
                    "start_line": 1,
                    "max_lines": 40
                }
            }),
            &reviewer,
        )
        .unwrap_err();
        assert!(error.contains("blocked"));
    }
    assert!(
        execute_context_operation(
            "context.read",
            serde_json::json!({
                "grant_id": grant_id,
                "selection": {
                    "kind": "commit",
                    "locator": "deadbee:.env",
                    "max_lines": 40
                }
            }),
            &reviewer,
        )
        .unwrap_err()
        .contains("blocked")
    );

    let pane_output = execute_context_operation(
        "context.read",
        serde_json::json!({
            "grant_id": grant_id,
            "selection": {
                "kind": "pane_output",
                "locator": "current",
                "max_lines": 40
            }
        }),
        &reviewer,
    )
    .unwrap();
    assert!(
        pane_output["content"]
            .as_str()
            .unwrap()
            .contains("pane output café")
    );
    assert!(
        !pane_output["content"]
            .as_str()
            .unwrap()
            .contains("do-not-share")
    );

    let revealed = execute_human_context_operation(
        "context.read",
        serde_json::json!({
            "grant_id": grant_id,
            "selection": {
                "kind": "file",
                "locator": "../outside.txt",
                "start_line": 1,
                "max_lines": 40
            },
            "reveal_blocked_item": "../outside.txt"
        }),
        &reviewer,
    )
    .unwrap();
    assert_eq!(revealed["content"], "outside evidence");
    assert_eq!(revealed["human_reveal"], true);
    assert!(revealed["audit_order"].as_u64().is_some());
}

fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn init_git_repo(directory: &TestDirectory) {
    git(&directory.repo, &["init", "-q", "-b", "main"]);
    git(&directory.repo, &["config", "user.name", "Context Test"]);
    git(&directory.repo, &["config", "user.email", "context@test"]);
    fs::write(directory.repo.join(".env"), "API_KEY=committed-secret\n").unwrap();
    fs::write(directory.repo.join(".gitignore"), "ignored.log\n").unwrap();
    git(&directory.repo, &["add", "."]);
    git(&directory.repo, &["commit", "-q", "-m", "initial"]);
}

fn git_init_space(directory: &TestDirectory) {
    git(&directory.repo, &["init", "-q", "-b", "main"]);
}

fn context_pin_refs(directory: &TestDirectory) -> Vec<String> {
    git(
        &directory.repo,
        &["for-each-ref", "--format=%(refname)", "refs/herdr-context"],
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

#[test]
fn credential_file_aliases_are_blocked_until_an_exact_human_reveal() {
    let directory = TestDirectory::new();
    git_init_space(&directory);
    setup(&directory);
    fs::write(
        directory.repo.join(".env"),
        "PRIVATE_NOTE=protected-alias-content\n",
    )
    .unwrap();
    fs::create_dir_all(directory.repo.join(".aws")).unwrap();
    fs::write(
        directory.repo.join(".aws/credentials"),
        "protected-directory-content\n",
    )
    .unwrap();
    std::os::unix::fs::symlink(".env", directory.repo.join("innocent.txt")).unwrap();
    std::os::unix::fs::symlink("innocent.txt", directory.repo.join("second-alias.txt")).unwrap();
    std::os::unix::fs::symlink(".aws", directory.repo.join("settings")).unwrap();
    std::os::unix::fs::symlink("../outside.txt", directory.repo.join("outside-alias.txt")).unwrap();
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    let aliases = [
        "innocent.txt",
        "second-alias.txt",
        "settings/credentials",
        "outside-alias.txt",
    ];
    let mut snapshot = valid_snapshot();
    snapshot["evidence"] = serde_json::json!(
        aliases.map(|locator| serde_json::json!({"kind":"file", "locator":locator}))
    );
    let published = execute_context_operation("snapshot.publish", serde_json::json!({
        "thread_id":sent["thread_id"], "grantee_address_ids":["address-reviewer"], "snapshot":snapshot
    }), &source).unwrap();
    let grant = &published["grants"][0]["grant_id"];
    for locator in aliases {
        let input = serde_json::json!({"grant_id":grant, "selection":{
            "kind":"file", "locator":locator, "start_line":1, "max_lines":40
        }});
        let denied = execute_context_operation("context.read", input.clone(), &reviewer);
        assert!(
            denied.is_err(),
            "alias disclosed protected content: {locator}: {denied:?}"
        );
        let mut revealed = input;
        revealed["reveal_blocked_item"] = serde_json::json!(locator);
        assert!(execute_context_operation("context.read", revealed.clone(), &reviewer).is_err());
        let allowed = execute_human_context_operation("context.read", revealed, &reviewer).unwrap();
        assert_eq!(allowed["human_reveal"], true);
        assert!(!allowed["content"].as_str().unwrap().is_empty());
    }
}

#[test]
fn context_authority_checks_live_reader_and_source_sessions_before_reconciliation() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    let publication = serde_json::json!({
        "thread_id":sent["thread_id"], "grantee_address_ids":["address-reviewer"],
        "capture_git_state":true, "snapshot":valid_snapshot()
    });
    let published =
        execute_context_operation("snapshot.publish", publication.clone(), &source).unwrap();
    let read = serde_json::json!({"grant_id":published["grants"][0]["grant_id"], "selection":{"kind":"snapshot"}});
    let inspect = serde_json::json!({"message_id":sent["message_id"], "reference_address_id":"address-source"});
    let pins = context_pin_refs(&directory);
    for pane in ["pane-reviewer", "pane-source"] {
        for session in [Some("replacement-session".to_owned()), None] {
            *herdr.live_pane_override.lock().unwrap() = Some((pane.to_owned(), session));
            for (operation, input) in [("context.read", &read), ("context.inspect", &inspect)] {
                let denied = execute_context_operation(operation, input.clone(), &reviewer);
                assert!(
                    denied.is_err(),
                    "{operation} accepted stale or missing live pane {pane}"
                );
                assert!(denied.unwrap_err().contains("live native Agent Session"));
            }
            let actor = if pane == "pane-source" {
                &source
            } else {
                &reviewer
            };
            let denied = execute_context_operation("snapshot.publish", publication.clone(), actor);
            assert!(denied.is_err());
            assert!(denied.unwrap_err().contains("live native Agent Session"));
            assert_eq!(
                context_pin_refs(&directory),
                pins,
                "unauthorized publication created a pin"
            );
        }
    }
    *herdr.live_pane_override.lock().unwrap() = None;
    assert!(execute_context_operation("context.read", read, &reviewer).is_ok());
}

#[test]
fn every_git_evidence_form_excludes_credential_directories_and_deleted_history() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    let protected = [
        ".env",
        ".env.local/details",
        ".aws/credentials",
        ".config/gcloud/configurations/config_default",
        ".docker/config.json",
        "nested/.npmrc",
        ".ENV.production",
    ];
    for path in protected {
        fs::create_dir_all(directory.repo.join(path).parent().unwrap()).unwrap();
        fs::write(
            directory.repo.join(path),
            "PRIVATE_NOTE=protected-history-marker\n",
        )
        .unwrap();
    }
    fs::write(
        directory.repo.join("src/notes.txt"),
        "public-history-marker\n",
    )
    .unwrap();
    git(&directory.repo, &["add", "."]);
    git(&directory.repo, &["commit", "-q", "-m", "evidence fixture"]);
    let commit = git(&directory.repo, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    for path in protected {
        fs::write(
            directory.repo.join(path),
            "PRIVATE_NOTE=protected-working-marker\n",
        )
        .unwrap();
    }
    fs::write(
        directory.repo.join("src/notes.txt"),
        "public-working-marker\n",
    )
    .unwrap();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    let mut snapshot = valid_snapshot();
    snapshot["evidence"] = serde_json::json!([
        {"kind":"diff", "locator":"."}, {"kind":"diff", "locator":".aws"},
        {"kind":"diff", "locator":".env"},
        {"kind":"commit", "locator":format!("{commit}:.")},
        {"kind":"commit", "locator":format!("{commit}:.env")},
        {"kind":"commit", "locator":format!("{commit}:.aws")}
    ]);
    let published = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "thread_id":sent["thread_id"], "grantee_address_ids":["address-reviewer"],
            "capture_git_state":true, "snapshot":snapshot
        }),
        &source,
    )
    .unwrap();
    let grant = &published["grants"][0]["grant_id"];
    for selection in [
        serde_json::json!({"kind":"diff", "locator":".", "max_lines":200}),
        serde_json::json!({"kind":"commit", "locator":format!("{commit}:.") ,"max_lines":200}),
        serde_json::json!({"kind":"git_state", "path":".", "start_line":1, "max_lines":200}),
    ] {
        let input = serde_json::json!({"grant_id":grant, "selection":selection});
        let result = execute_context_operation("context.read", input.clone(), &reviewer).unwrap();
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("public-"), "{result}");
        assert!(
            !content.contains("protected-"),
            "credential directory leak: {result}"
        );
        let mut human = input;
        human["reveal_blocked_item"] = selection
            .get("locator")
            .unwrap_or(&selection["path"])
            .clone();
        let result = execute_human_context_operation("context.read", human, &reviewer).unwrap();
        assert!(
            !result["content"].as_str().unwrap().contains("protected-"),
            "broad reveal leaked: {result}"
        );
    }
    for selection in [
        serde_json::json!({"kind":"diff", "locator":".aws", "max_lines":200}),
        serde_json::json!({"kind":"commit", "locator":format!("{commit}:.aws"), "max_lines":200}),
        serde_json::json!({"kind":"git_state", "path":".aws", "start_line":1, "max_lines":200}),
    ] {
        let reveal = selection.get("locator").unwrap_or(&selection["path"]);
        let result = execute_human_context_operation(
            "context.read",
            serde_json::json!({
                "grant_id":grant, "selection":selection, "reveal_blocked_item":reveal
            }),
            &reviewer,
        );
        assert!(
            result.is_err()
                || !result.unwrap()["content"]
                    .as_str()
                    .unwrap()
                    .contains("protected-")
        );
    }
    fs::remove_file(directory.repo.join(".env")).unwrap();
    let historical = serde_json::json!({"grant_id":grant, "selection":{
        "kind":"commit", "locator":format!("{commit}:.env"), "max_lines":200
    }});
    assert!(execute_context_operation("context.read", historical.clone(), &reviewer).is_err());
    let mut reveal = historical;
    reveal["reveal_blocked_item"] = serde_json::json!(format!("{commit}:.env"));
    let result = execute_human_context_operation("context.read", reveal, &reviewer).unwrap();
    assert!(
        result["content"]
            .as_str()
            .unwrap()
            .contains("protected-history-marker")
    );
}

#[test]
fn git_evidence_treats_bracket_names_literally_and_rejects_magic_pathspecs() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    for (path, content) in [("[ab].txt", "literal-old\n"), ("a.txt", "unselected-old\n")] {
        fs::write(directory.repo.join(path), content).unwrap();
    }
    git(&directory.repo, &["add", "."]);
    git(&directory.repo, &["commit", "-q", "-m", "literal paths"]);
    let commit = git(&directory.repo, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    fs::write(directory.repo.join("[ab].txt"), "literal-new\n").unwrap();
    fs::write(directory.repo.join("a.txt"), "unselected-new\n").unwrap();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    let mut snapshot = valid_snapshot();
    snapshot["evidence"] = serde_json::json!([
        {"kind":"diff", "locator":"[ab].txt"},
        {"kind":"commit", "locator":format!("{commit}:[ab].txt")}
    ]);
    let published = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "thread_id":sent["thread_id"], "grantee_address_ids":["address-reviewer"],
            "capture_git_state":true, "snapshot":snapshot
        }),
        &source,
    )
    .unwrap();
    let grant = &published["grants"][0]["grant_id"];
    for selection in [
        serde_json::json!({"kind":"diff", "locator":"[ab].txt", "max_lines":200}),
        serde_json::json!({"kind":"commit", "locator":format!("{commit}:[ab].txt"), "max_lines":200}),
        serde_json::json!({"kind":"git_state", "path":"[ab].txt", "start_line":1, "max_lines":200}),
    ] {
        let result = execute_context_operation(
            "context.read",
            serde_json::json!({
                "grant_id":grant, "selection":selection
            }),
            &reviewer,
        )
        .unwrap();
        let content = result["content"].as_str().unwrap();
        assert!(content.contains("literal-"), "{result}");
        assert!(
            !content.contains("unselected-"),
            "pathspec expanded: {result}"
        );
    }
    for path in [":(glob)**", ":(exclude)src", "*"] {
        let result = execute_context_operation(
            "context.read",
            serde_json::json!({
                "grant_id":grant, "selection":{"kind":"git_state", "path":path, "start_line":1, "max_lines":200}
            }),
            &reviewer,
        );
        assert!(
            result.is_err(),
            "accepted nonliteral evidence selector {path}: {result:?}"
        );
    }
}

#[test]
fn evidence_byte_limits_bound_files_and_git_output_even_for_one_long_line() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    fs::write(
        directory.repo.join("src/notes.txt"),
        "x".repeat(1024 * 1024 + 1),
    )
    .unwrap();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    let mut snapshot = valid_snapshot();
    snapshot["evidence"] = serde_json::json!([
        {"kind":"file", "locator":"src/notes.txt"},
        {"kind":"diff", "locator":"src/notes.txt"}
    ]);
    let published = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "thread_id":sent["thread_id"], "grantee_address_ids":["address-reviewer"],
            "capture_git_state":true, "snapshot":snapshot
        }),
        &source,
    )
    .unwrap();
    let mut unbounded = Vec::new();
    for selection in [
        serde_json::json!({"kind":"file", "locator":"src/notes.txt", "start_line":1, "max_lines":20}),
        serde_json::json!({"kind":"diff", "locator":"src/notes.txt", "max_lines":20}),
        serde_json::json!({"kind":"git_state", "path":"src/notes.txt", "start_line":1, "max_lines":20}),
    ] {
        let result = execute_context_operation(
            "context.read",
            serde_json::json!({
                "grant_id":published["grants"][0]["grant_id"], "selection":selection
            }),
            &reviewer,
        );
        match result {
            Ok(_) => unbounded.push(selection["kind"].clone()),
            Err(error) => assert!(error.contains("1048576 bytes"), "{error}"),
        }
    }
    assert!(
        unbounded.is_empty(),
        "unbounded evidence forms: {unbounded:?}"
    );
}

#[test]
fn file_paging_cannot_start_inside_a_private_key_and_disclose_its_body() {
    let directory = TestDirectory::new();
    git_init_space(&directory);
    fs::write(directory.repo.join("src/notes.txt"), "-----BEGIN PRIVATE KEY-----\nsynthetic-private-material\n-----END PRIVATE KEY-----\npublic tail\n").unwrap();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    let published = execute_context_operation("snapshot.publish", serde_json::json!({
        "thread_id":sent["thread_id"], "grantee_address_ids":["address-reviewer"], "snapshot":valid_snapshot()
    }), &source).unwrap();
    let result = execute_context_operation(
        "context.read",
        serde_json::json!({
            "grant_id":published["grants"][0]["grant_id"],
            "selection":{"kind":"file", "locator":"src/notes.txt", "start_line":2, "max_lines":3}
        }),
        &reviewer,
    )
    .unwrap();
    assert!(
        !result["content"]
            .as_str()
            .unwrap()
            .contains("synthetic-private-material")
    );
    assert!(result["content"].as_str().unwrap().contains("public tail"));
}

#[test]
fn capture_git_state_pins_handoff_work_and_pages_the_pinned_diff() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);

    // An agent never hand-authors the git_state pointers.
    let mut forged = valid_snapshot();
    forged["git_state"] = serde_json::json!({"pinned_commit": "deadbeef"});
    assert!(
        execute_context_operation(
            "snapshot.publish",
            serde_json::json!({
                "thread_id": sent["thread_id"],
                "grantee_address_ids": ["address-reviewer"],
                "snapshot": forged
            }),
            &source,
        )
        .unwrap_err()
        .contains("unknown field")
    );

    // Handoff-time work: a tracked modification, a brand-new untracked file,
    // an ignored file, and a credential file.
    fs::write(
        directory.repo.join("src/notes.txt"),
        "safe line\nreview this new line\n",
    )
    .unwrap();
    fs::write(directory.repo.join("src/new_module.rs"), "brand new work\n").unwrap();
    fs::write(directory.repo.join("ignored.log"), "IGNORED-CONTENT\n").unwrap();
    fs::write(directory.repo.join(".env"), "SETTING=uncommitted-plain\n").unwrap();

    let published = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "thread_id": sent["thread_id"],
            "grantee_address_ids": ["address-reviewer"],
            "capture_git_state": true,
            "snapshot": valid_snapshot()
        }),
        &source,
    )
    .unwrap();
    let grant_id = published["grants"][0]["grant_id"].as_str().unwrap();
    assert_eq!(published["grants"][0]["state"], "active");

    let snapshot = execute_context_operation(
        "context.read",
        serde_json::json!({"grant_id": grant_id, "selection": {"kind": "snapshot"}}),
        &reviewer,
    )
    .unwrap();
    let git_state = &snapshot["content"]["git_state"];
    let head = git(&directory.repo, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    assert_eq!(git_state["branch"], "main");
    assert_eq!(git_state["head"].as_str().unwrap(), head);
    assert_eq!(git_state["merge_base"].as_str().unwrap(), head);
    assert_eq!(git_state["dirty"], true);
    let pinned = git_state["pinned_commit"].as_str().unwrap();
    assert_ne!(pinned, head);
    let pin_id = git_state["pin_id"].as_str().unwrap();
    assert_eq!(
        context_pin_refs(&directory),
        vec![format!("refs/herdr-context/{pin_id}")]
    );

    // The sender moves on; the reviewer still reads the handoff-time diff.
    fs::write(directory.repo.join("src/notes.txt"), "safe line\n").unwrap();
    fs::remove_file(directory.repo.join("src/new_module.rs")).unwrap();
    let diff = execute_context_operation(
        "context.read",
        serde_json::json!({
            "grant_id": grant_id,
            "selection": {"kind": "git_state", "start_line": 1, "max_lines": 200}
        }),
        &reviewer,
    )
    .unwrap();
    let diff_content = diff["content"].as_str().unwrap().to_owned();
    assert!(
        diff_content.contains("review this new line"),
        "{diff_content}"
    );
    assert!(diff_content.contains("brand new work"), "{diff_content}");
    assert!(!diff_content.contains("IGNORED-CONTENT"), "{diff_content}");
    assert!(
        !diff_content.contains("uncommitted-plain"),
        "{diff_content}"
    );
    assert!(!diff_content.contains(".env"), "{diff_content}");
    assert_eq!(diff["scope"], "shared_worktree");

    // Deterministic paging over the immutable pin.
    let window = execute_context_operation(
        "context.read",
        serde_json::json!({
            "grant_id": grant_id,
            "selection": {"kind": "git_state", "start_line": 2, "max_lines": 1}
        }),
        &reviewer,
    )
    .unwrap();
    assert_eq!(
        window["content"].as_str().unwrap(),
        diff_content.lines().nth(1).unwrap()
    );

    let stat = execute_context_operation(
        "context.read",
        serde_json::json!({
            "grant_id": grant_id,
            "selection": {"kind": "git_state", "stat": true, "start_line": 1, "max_lines": 200}
        }),
        &reviewer,
    )
    .unwrap();
    let stat_content = stat["content"].as_str().unwrap();
    assert!(stat_content.contains("src/notes.txt"), "{stat_content}");
    assert!(!stat_content.contains(".env"), "{stat_content}");

    let filtered = execute_context_operation(
        "context.read",
        serde_json::json!({
            "grant_id": grant_id,
            "selection": {
                "kind": "git_state",
                "path": "src/notes.txt",
                "start_line": 1,
                "max_lines": 200
            }
        }),
        &reviewer,
    )
    .unwrap();
    assert!(
        filtered["content"]
            .as_str()
            .unwrap()
            .contains("review this new line")
    );
    assert!(
        execute_context_operation(
            "context.read",
            serde_json::json!({
                "grant_id": grant_id,
                "selection": {
                    "kind": "git_state",
                    "path": ".env",
                    "start_line": 1,
                    "max_lines": 200
                }
            }),
            &reviewer,
        )
        .unwrap_err()
        .contains("blocked")
    );

    // A human may reveal exactly one named blocked path of the pinned diff;
    // the disclosure-text redaction still applies to what it returns.
    let revealed = execute_human_context_operation(
        "context.read",
        serde_json::json!({
            "grant_id": grant_id,
            "selection": {
                "kind": "git_state",
                "path": ".env",
                "start_line": 1,
                "max_lines": 200
            },
            "reveal_blocked_item": ".env"
        }),
        &reviewer,
    )
    .unwrap();
    let revealed_content = revealed["content"].as_str().unwrap();
    assert!(
        revealed_content.contains("uncommitted-plain"),
        "{revealed_content}"
    );
    assert_eq!(revealed["human_reveal"], true);

    // A human reveal naming an unblocked pathspec gains nothing: the
    // blocked-path excludes still apply to the read.
    let unblocked_reveal = execute_human_context_operation(
        "context.read",
        serde_json::json!({
            "grant_id": grant_id,
            "selection": {
                "kind": "git_state",
                "path": ".",
                "start_line": 1,
                "max_lines": 200
            },
            "reveal_blocked_item": "."
        }),
        &reviewer,
    )
    .unwrap();
    let unblocked_content = unblocked_reveal["content"].as_str().unwrap();
    assert!(
        unblocked_content.contains("review this new line"),
        "{unblocked_content}"
    );
    assert!(
        !unblocked_content.contains("uncommitted-plain"),
        "{unblocked_content}"
    );

    // A bare publish keeps today's behavior: no git_state, no repo side effects.
    let bare = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "thread_id": sent["thread_id"],
            "grantee_address_ids": ["address-reviewer"],
            "snapshot": valid_snapshot()
        }),
        &source,
    )
    .unwrap();
    let bare_grant_id = bare["grants"][0]["grant_id"].as_str().unwrap();
    let bare_snapshot = execute_context_operation(
        "context.read",
        serde_json::json!({"grant_id": bare_grant_id, "selection": {"kind": "snapshot"}}),
        &reviewer,
    )
    .unwrap();
    assert!(bare_snapshot["content"]["git_state"].is_null());
    assert_eq!(context_pin_refs(&directory).len(), 1);
    assert!(
        execute_context_operation(
            "context.read",
            serde_json::json!({
                "grant_id": bare_grant_id,
                "selection": {"kind": "git_state", "start_line": 1, "max_lines": 200}
            }),
            &reviewer,
        )
        .unwrap_err()
        .contains("no git state")
    );
}

#[test]
fn pin_refs_of_terminal_or_unknown_grants_sweep_on_the_next_capture_publish() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    let publish = |request: &str| {
        execute_context_operation(
            "snapshot.publish",
            serde_json::json!({
                "thread_id": sent["thread_id"],
                "grantee_address_ids": ["address-reviewer"],
                "capture_git_state": true,
                "snapshot": valid_snapshot()
            }),
            &source,
        )
        .unwrap_or_else(|error| panic!("{request}: {error}"))
    };

    // A clean worktree pins HEAD itself.
    let first = publish("first");
    let first_grant_id = first["grants"][0]["grant_id"].as_str().unwrap().to_owned();
    let head = git(&directory.repo, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    let snapshot = execute_context_operation(
        "context.read",
        serde_json::json!({"grant_id": first_grant_id, "selection": {"kind": "snapshot"}}),
        &reviewer,
    )
    .unwrap();
    assert_eq!(snapshot["content"]["git_state"]["dirty"], false);
    assert_eq!(
        snapshot["content"]["git_state"]["pinned_commit"]
            .as_str()
            .unwrap(),
        head
    );
    let first_ref = context_pin_refs(&directory)[0].clone();

    // Two refs unknown to the store: a stale one (a failed publication's
    // leftover, backdated past the sweep grace) and a fresh one standing in
    // for a concurrent publish whose transaction has not committed yet.
    git(
        &directory.repo,
        &["update-ref", "refs/herdr-context/orphan-stale", &head],
    );
    let stale_loose_ref = directory.repo.join(".git/refs/herdr-context/orphan-stale");
    assert!(
        std::process::Command::new("touch")
            .args(["-t", "202001010000"])
            .arg(&stale_loose_ref)
            .status()
            .unwrap()
            .success()
    );
    git(
        &directory.repo,
        &["update-ref", "refs/herdr-context/orphan-fresh", &head],
    );

    execute_human_context_operation(
        "context.revoke",
        serde_json::json!({"grant_id": first_grant_id}),
        &reviewer,
    )
    .unwrap();

    // The next capture-publish sweeps the terminal grant's ref and the stale
    // orphan; the fresh unknown ref stays inside its in-flight grace.
    publish("second");
    let refs = context_pin_refs(&directory);
    assert_eq!(refs.len(), 2, "{refs:?}");
    assert!(!refs.contains(&first_ref), "{refs:?}");
    assert!(
        !refs.iter().any(|name| name.ends_with("orphan-stale")),
        "{refs:?}"
    );
    assert!(
        refs.iter().any(|name| name.ends_with("orphan-fresh")),
        "{refs:?}"
    );
    let second_ref = refs
        .iter()
        .find(|name| !name.ends_with("orphan-fresh"))
        .unwrap()
        .clone();

    // A still-active grant keeps its pin through later sweeps.
    publish("third");
    let refs = context_pin_refs(&directory);
    assert_eq!(refs.len(), 3, "{refs:?}");
    assert!(refs.contains(&second_ref), "{refs:?}");
}

/// Delivery dispatch drains only after reconciliation completes; context tests
/// that assert on dispatched envelopes opt in per test.
fn complete_reconciliation(directory: &TestDirectory) {
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    StateStore::open(&directory.state)
        .unwrap()
        .transaction(|transaction| transaction.complete_reconciliation(&namespace))
        .unwrap();
}

/// The delivery envelope block and the bound context.read example one
/// send-with-context recipient receives.
fn attached_context_read(envelope: &serde_json::Value) -> (&str, &serde_json::Value) {
    let grant_id = envelope["control"]["attached_context"]["grant_id"]
        .as_str()
        .unwrap_or_else(|| panic!("envelope carries no attached_context: {envelope}"));
    let read = envelope["control"]["tool"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|operation| operation["token"] == "context.read")
        .unwrap();
    (grant_id, &read["input"])
}

#[test]
fn send_with_context_activates_one_grant_per_recipient_and_binds_each_envelope() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    let researcher = runtime(&directory, &herdr, &ids, "pane-researcher");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    register("address-researcher", &researcher);
    fs::write(
        directory.repo.join("src/notes.txt"),
        "safe line\nparser change body\n",
    )
    .unwrap();

    let mut snapshot = valid_snapshot();
    snapshot["evidence"] = serde_json::json!([
        {"kind": "diff", "locator": "src/notes.txt"},
        {"kind": "diff", "locator": "src/notes.txt"},
        {"kind": "diff", "locator": ".env"}
    ]);
    let sent = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "send-context-1",
            "source": "@reviewer @researcher please review the parser change",
            "author_kind": "agent_session",
            // capture_git_state is deliberately omitted: a context handoff
            // pins git state by default.
            "context": {"snapshot": snapshot}
        }),
        &source,
    )
    .unwrap();

    let grants = sent["context_grants"].as_array().unwrap();
    assert_eq!(grants.len(), 2, "{sent}");
    for grant in grants {
        assert_eq!(grant["state"], "active", "{grant}");
        assert_eq!(grant["source_address_id"], "address-source");
    }
    assert_eq!(grants[0]["grantee_address_id"], "address-reviewer");
    assert_eq!(grants[1]["grantee_address_id"], "address-researcher");

    // Each recipient's envelope carries its exact grant id with a runnable read.
    let reviewer_envelope = herdr.prompt_for_pane("pane-reviewer");
    let researcher_envelope = herdr.prompt_for_pane("pane-researcher");
    let (reviewer_grant, reviewer_read) = attached_context_read(&reviewer_envelope);
    let (researcher_grant, _) = attached_context_read(&researcher_envelope);
    assert_eq!(reviewer_grant, grants[0]["grant_id"].as_str().unwrap());
    assert_eq!(researcher_grant, grants[1]["grant_id"].as_str().unwrap());
    assert_ne!(reviewer_grant, researcher_grant);
    assert_eq!(
        reviewer_read,
        &serde_json::json!({
            "grant_id": reviewer_grant,
            "selection": {"kind": "snapshot"}
        })
    );
    let bound_reads = reviewer_envelope["control"]["tool"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|operation| operation["token"] == "context.read")
        .map(|operation| operation["input"].clone())
        .collect::<Vec<_>>();
    assert_eq!(
        bound_reads,
        vec![
            serde_json::json!({
                "grant_id": reviewer_grant,
                "selection": {"kind": "snapshot"}
            }),
            serde_json::json!({
                "grant_id": reviewer_grant,
                "selection": {
                    "kind": "git_state",
                    "path": "src/notes.txt",
                    "start_line": 1,
                    "max_lines": 200
                }
            }),
            serde_json::json!({
                "grant_id": reviewer_grant,
                "selection": {
                    "kind": "git_state",
                    "path": "src/notes.txt",
                    "start_line": 1,
                    "max_lines": 200
                }
            })
        ]
    );
    // Body Mentions keep their meaning: no Context Reference was invented.
    assert_eq!(
        reviewer_envelope["control"]["context_references"],
        serde_json::json!([])
    );

    // The bound example runs as-is: the redacted snapshot with pinned git state.
    let snapshot =
        execute_context_operation("context.read", bound_reads[0].clone(), &reviewer).unwrap();
    let disclosed = snapshot["content"].to_string();
    assert!(!disclosed.contains("top-secret-value"));
    assert_eq!(snapshot["content"]["git_state"]["dirty"], true);
    assert_eq!(snapshot["content"]["git_state"]["branch"], "main");
    let pinned_diff =
        execute_context_operation("context.read", bound_reads[1].clone(), &reviewer).unwrap();
    assert!(
        pinned_diff["content"]
            .as_str()
            .unwrap()
            .contains("parser change body"),
        "{pinned_diff}"
    );

    // A reply can hand context back the same way, finishing the attempt.
    let attempt_id = reviewer_envelope["control"]["attempt_id"].as_str().unwrap();
    let replied = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-context-1",
            "source": "@source reviewed; here is my working context",
            "in_reply_to": sent["message_id"],
            "author_kind": "agent_session",
            "attempt_id": attempt_id,
            "context": {"capture_git_state": false, "snapshot": valid_snapshot()}
        }),
        &reviewer,
    )
    .unwrap();
    let reply_grants = replied["context_grants"].as_array().unwrap();
    assert_eq!(reply_grants.len(), 1, "{replied}");
    assert_eq!(reply_grants[0]["state"], "active");
    assert_eq!(reply_grants[0]["grantee_address_id"], "address-source");
    assert_eq!(replied["disposition"], "reply");
    let source_envelope = herdr.prompt_for_pane("pane-source");
    let (source_grant, _) = attached_context_read(&source_envelope);
    assert_eq!(source_grant, reply_grants[0]["grant_id"].as_str().unwrap());
    let handed_back = execute_context_operation(
        "context.read",
        serde_json::json!({
            "grant_id": source_grant,
            "selection": {"kind": "snapshot"}
        }),
        &source,
    )
    .unwrap();
    assert!(handed_back["content"]["git_state"].is_null());
}

#[test]
fn a_failing_send_with_context_delivers_no_message_and_no_grant() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);

    // Snapshot validation fails before any routing state is written.
    let invalid = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "send-context-invalid",
            "source": "@reviewer please review",
            "author_kind": "agent_session",
            "context": {"snapshot": {"objective": "missing required fields"}}
        }),
        &source,
    )
    .unwrap_err();
    assert!(
        invalid.contains("applicable_user_instructions"),
        "{invalid}"
    );

    // A context block is an agent-authored surface only.
    let human = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "send-context-human",
            "source": "@reviewer please review",
            "context": {"snapshot": valid_snapshot()}
        }),
        &source,
    )
    .unwrap_err();
    assert!(human.contains("agent-authored"), "{human}");

    // A grant failure inside the transaction rolls the whole send back:
    // no message, no delivery, and the orphaned pin ref stays for the sweep.
    let self_grant = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "send-context-self",
            "source": "@source note to self",
            "author_kind": "agent_session",
            "context": {
                "capture_git_state": true,
                "snapshot": valid_snapshot()
            }
        }),
        &source,
    )
    .unwrap_err();
    assert!(
        self_grant.contains("addressed only to its author"),
        "{self_grant}"
    );
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let facts = StateStore::open(&directory.state)
        .unwrap()
        .transaction(|transaction| transaction.transition_facts(&namespace))
        .unwrap();
    assert!(
        facts.iter().all(|fact| {
            fact.entity_kind != namedrop::TransitionEntityKind::CollaborationMessage
        }),
        "a failed send left a message"
    );
    assert!(herdr.prompts.lock().unwrap().is_empty());
    assert_eq!(context_pin_refs(&directory).len(), 1);

    // The successful retry then delivers, and the earlier orphan is swept by the
    // next pinning publish because no stored grant knows it (after the grace).
    let sent = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "send-context-retry",
            "source": "@reviewer please review",
            "author_kind": "agent_session",
            "context": {"capture_git_state": false, "snapshot": valid_snapshot()}
        }),
        &source,
    )
    .unwrap();
    assert_eq!(sent["context_grants"].as_array().unwrap().len(), 1);
}

#[test]
fn replaying_a_send_with_context_reuses_the_original_message_and_grants() {
    let directory = TestDirectory::new();
    git_init_space(&directory);
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let input = serde_json::json!({
        "request_id": "send-context-replay",
        "source": "@reviewer please review",
        "author_kind": "agent_session",
        "context": {"capture_git_state": false, "snapshot": valid_snapshot()}
    });

    let first = execute_message_operation("send", input.clone(), &source).unwrap();
    let replay = execute_message_operation("send", input, &source).unwrap();

    assert_eq!(replay["idempotent_replay"], true);
    assert_eq!(replay["message_id"], first["message_id"]);
    assert!(replay["context_grants"].is_null());
    let dispatched = herdr
        .prompts
        .lock()
        .unwrap()
        .iter()
        .filter(|(pane, _)| pane == "pane-reviewer")
        .count();
    assert_eq!(dispatched, 1, "a replay dispatched a second delivery");
}

/// The pull direction is a reply-grammar extension: a mentioned agent replies
/// with `request_context: true`, which creates the Pending Context Grant whose
/// id IS the request, binds it into the caller's delivery control as
/// `context_request`, and pre-binds a fulfillment reply example. The caller
/// answers with the same reply `context` block in activate-mode (`grant_ids`),
/// and the requester's notification is an ordinary attached_context delivery.
#[test]
fn a_reply_can_request_context_and_the_caller_fulfills_it() {
    let directory = TestDirectory::new();
    git_init_space(&directory);
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);

    // The caller mentions the reviewer without any context attached.
    let sent = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "send-plain-1",
            "source": "@reviewer please pick up the parser follow-up",
            "author_kind": "agent_session",
        }),
        &source,
    )
    .unwrap();
    let reviewer_envelope = herdr.prompt_for_pane("pane-reviewer");
    assert!(
        !reviewer_envelope["control"]
            .as_object()
            .unwrap()
            .contains_key("context_request")
    );

    // The reviewer asks for the caller's context in its reply.
    let requested = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-request-1",
            "source": "@source what does your working tree look like?",
            "in_reply_to": sent["message_id"],
            "author_kind": "agent_session",
            "attempt_id": reviewer_envelope["control"]["attempt_id"],
            "request_context": true,
        }),
        &reviewer,
    )
    .unwrap();
    assert_eq!(requested["disposition"], "reply");
    let request = &requested["context_request"];
    assert_eq!(request["state"], "pending", "{requested}");
    assert_eq!(request["source_address_id"], "address-source");
    assert_eq!(request["grantee_address_id"], "address-reviewer");
    let grant_id = request["grant_id"].as_str().unwrap();

    // The caller's envelope names the request and pre-binds fulfillment.
    let caller_envelope = herdr.prompt_for_pane("pane-source");
    assert_eq!(
        caller_envelope["control"]["context_request"],
        serde_json::json!({"grant_id": grant_id})
    );
    let bound_reply = caller_envelope["control"]["tool"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|operation| operation["token"] == "reply")
        .unwrap();
    assert_eq!(
        bound_reply["input"]["context"]["grant_ids"],
        serde_json::json!([grant_id])
    );

    // The caller fulfills: activate-mode names exactly the pending grant.
    let fulfilled = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-fulfill-1",
            "source": "@reviewer here is where I am",
            "in_reply_to": requested["message_id"],
            "author_kind": "agent_session",
            "attempt_id": caller_envelope["control"]["attempt_id"],
            "context": {
                "capture_git_state": false,
                "grant_ids": [grant_id],
                "snapshot": valid_snapshot(),
            },
        }),
        &source,
    )
    .unwrap();
    let grants = fulfilled["context_grants"].as_array().unwrap();
    assert_eq!(grants.len(), 1, "{fulfilled}");
    assert_eq!(grants[0]["grant_id"], grant_id);
    assert_eq!(grants[0]["state"], "active");
    assert_eq!(grants[0]["grantee_address_id"], "address-reviewer");

    // The requester's notification IS the fulfillment delivery, identical to
    // the push path: attached_context plus a runnable snapshot read.
    let fulfillment_envelope = herdr.prompt_for_pane("pane-reviewer");
    let (attached_grant, read) = attached_context_read(&fulfillment_envelope);
    assert_eq!(attached_grant, grant_id);
    let snapshot = execute_context_operation("context.read", read.clone(), &reviewer).unwrap();
    assert_eq!(
        snapshot["content"]["objective"],
        "Review the context boundary"
    );
}

/// A human Mention may ask the recipient to pull the source agent's context.
/// The typed pull is fulfilled by that source Agent Session without another
/// human prompt, then the recipient returns exactly one context-grounded answer
/// through the original relay. The final report_to_human leg cannot reply.
#[test]
fn a_context_request_decline_revokes_the_pending_grant() {
    let directory = TestDirectory::new();
    git_init_space(&directory);
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    let researcher = runtime(&directory, &herdr, &ids, "pane-researcher");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    register("address-researcher", &researcher);
    let sent = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "send-plain-2",
            "source": "@reviewer @researcher please pick this up",
            "author_kind": "agent_session",
        }),
        &source,
    )
    .unwrap();
    let reviewer_envelope = herdr.prompt_for_pane("pane-reviewer");
    let requested = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-request-2",
            "source": "@source I need your context first",
            "in_reply_to": sent["message_id"],
            "author_kind": "agent_session",
            "attempt_id": reviewer_envelope["control"]["attempt_id"],
            "request_context": true,
        }),
        &reviewer,
    )
    .unwrap();
    let grant_id = requested["context_request"]["grant_id"].as_str().unwrap();

    // Only the request's exact source may settle it; a bystander with its own
    // live attempt in the thread cannot decline on the caller's behalf.
    let researcher_envelope = herdr.prompt_for_pane("pane-researcher");
    let not_source = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-bystander-decline",
            "source": "@source never mind that request",
            "in_reply_to": sent["message_id"],
            "author_kind": "agent_session",
            "attempt_id": researcher_envelope["control"]["attempt_id"],
            "decline_context_request": grant_id,
        }),
        &researcher,
    )
    .unwrap_err();
    assert!(not_source.contains("source"), "{not_source}");

    let caller_envelope = herdr.prompt_for_pane("pane-source");
    let declined = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-decline-2",
            "source": "@reviewer cannot share right now",
            "in_reply_to": requested["message_id"],
            "author_kind": "agent_session",
            "attempt_id": caller_envelope["control"]["attempt_id"],
            "decline_context_request": grant_id,
        }),
        &source,
    )
    .unwrap();
    assert_eq!(
        declined["declined_context_request"]["state"], "revoked",
        "{declined}"
    );
    assert_eq!(declined["disposition"], "reply");

    // A revoked request cannot be read or fulfilled late.
    let read = execute_context_operation(
        "context.read",
        serde_json::json!({"grant_id": grant_id, "selection": {"kind": "snapshot"}}),
        &reviewer,
    )
    .unwrap_err();
    assert!(read.contains("active"), "{read}");
    let reviewer_decline_envelope = herdr.prompt_for_pane("pane-reviewer");
    let reconsider = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-reconsider",
            "source": "@source please reconsider",
            "in_reply_to": declined["message_id"],
            "author_kind": "agent_session",
            "attempt_id": reviewer_decline_envelope["control"]["attempt_id"],
        }),
        &reviewer,
    )
    .unwrap();
    let source_envelope = herdr.prompt_for_pane("pane-source");
    let late = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-late-fulfill",
            "source": "@reviewer changed my mind",
            "in_reply_to": reconsider["message_id"],
            "author_kind": "agent_session",
            "attempt_id": source_envelope["control"]["attempt_id"],
            "context": {
                "capture_git_state": false,
                "grant_ids": [grant_id],
                "snapshot": valid_snapshot(),
            },
        }),
        &source,
    )
    .unwrap_err();
    assert!(late.contains("pending"), "{late}");
}

/// The pull path keeps event-based expiry: a request's Pending grant is
/// pinned to the caller's live pane binding and Agent Session at request
/// time, so when the caller's Agent Session changes before it answers — the
/// binding staying put — the request expires instead of waiting forever, and
/// a late fulfillment from the caller's replacement session is rejected.
#[test]
fn a_pending_context_request_expires_when_the_callers_session_changes() {
    let directory = TestDirectory::new();
    git_init_space(&directory);
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "send-plain-4",
            "source": "@reviewer please pick this up",
            "author_kind": "agent_session",
        }),
        &source,
    )
    .unwrap();
    let reviewer_envelope = herdr.prompt_for_pane("pane-reviewer");
    let requested = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-request-4",
            "source": "@source I need your context first",
            "in_reply_to": sent["message_id"],
            "author_kind": "agent_session",
            "attempt_id": reviewer_envelope["control"]["attempt_id"],
            "request_context": true,
        }),
        &reviewer,
    )
    .unwrap();
    let grant_id = requested["context_request"]["grant_id"].as_str().unwrap();
    let caller_envelope = herdr.prompt_for_pane("pane-source");

    // The caller's Agent Session changes before it answers the request: the
    // pane keeps its binding, but a replacement session is observed on it.
    // The session comparison alone must expire the request.
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    StateStore::open(&directory.state)
        .unwrap()
        .transaction(|transaction| {
            let attribution = MutationAttribution::new(ActorKind::Human, "test", 50);
            transaction.observe_agent_session(
                &namespace,
                "observation-source-replacement-session",
                &PaneAddressId::new("address-source"),
                &AgentSessionId::new("session-source-replaced"),
                "idle",
                &attribution,
            )
        })
        .unwrap();

    // The request lapses to Expired — distinguishable from a decline's
    // Revoked — the moment any grant-touching path refreshes it. No message
    // context references the caller in a pull thread, so context.inspect
    // cannot address a request grant; the store's refreshed state is the
    // authoritative surface here.
    let refreshed = StateStore::open(&directory.state)
        .unwrap()
        .transaction(|transaction| {
            transaction.refresh_context_grant(&namespace, &namedrop::ContextGrantId::new(grant_id))
        })
        .unwrap();
    assert_eq!(refreshed.state, namedrop::ContextGrantState::Expired);

    // A late fulfillment is impossible by construction: the replacement
    // session cannot dispose the old session's attempt, so the reply gate
    // rejects it before the grant guard is even consulted. (The grant guard's
    // own not-Pending rejection is covered by the decline test above and the
    // binding-change expiry test earlier in this file.)
    let late = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-late-fulfill-4",
            "source": "@reviewer here is where I am",
            "in_reply_to": requested["message_id"],
            "author_kind": "agent_session",
            "attempt_id": caller_envelope["control"]["attempt_id"],
            "context": {
                "capture_git_state": false,
                "grant_ids": [grant_id],
                "snapshot": valid_snapshot(),
            },
        }),
        &source,
    )
    .unwrap_err();
    assert!(late.contains("exact Agent Session"), "{late}");
}

/// The request and fulfillment ride the reply gate: agent-authored replies on
/// agent_thread deliveries only, addressed to the exact counterparty, one
/// direction per reply.
#[test]
fn context_request_and_fulfillment_guard_their_reply_routes() {
    let directory = TestDirectory::new();
    git_init_space(&directory);
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    let researcher = runtime(&directory, &herdr, &ids, "pane-researcher");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    register("address-researcher", &researcher);
    let sent = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "send-plain-3",
            "source": "@reviewer please pick this up",
            "author_kind": "agent_session",
        }),
        &source,
    )
    .unwrap();
    let reviewer_envelope = herdr.prompt_for_pane("pane-reviewer");
    let attempt_id = reviewer_envelope["control"]["attempt_id"].as_str().unwrap();

    // Fulfillment rides the requesting thread's reply, never a fresh send.
    let fulfillment_on_send = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "send-grant-ids",
            "source": "@source here you go",
            "author_kind": "agent_session",
            "context": {
                "capture_git_state": false,
                "grant_ids": ["context-grant-unknown"],
                "snapshot": valid_snapshot(),
            },
        }),
        &reviewer,
    )
    .unwrap_err();
    assert!(
        fulfillment_on_send.contains("reply"),
        "{fulfillment_on_send}"
    );

    // `send` has no request grammar at all.
    let on_send = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "send-request-context",
            "source": "@source give me context",
            "author_kind": "agent_session",
            "request_context": true,
        }),
        &reviewer,
    )
    .unwrap_err();
    assert!(on_send.contains("request_context"), "{on_send}");

    // A human reply cannot carry the request marker.
    let human = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-human-request",
            "source": "@source give me context",
            "in_reply_to": sent["message_id"],
            "request_context": true,
        }),
        &reviewer,
    )
    .unwrap_err();
    assert!(human.contains("agent-authored"), "{human}");

    // One reply carries one direction: never both ask and attach.
    let both = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-both-directions",
            "source": "@source give me context",
            "in_reply_to": sent["message_id"],
            "author_kind": "agent_session",
            "attempt_id": attempt_id,
            "request_context": true,
            "context": {"capture_git_state": false, "snapshot": valid_snapshot()},
        }),
        &reviewer,
    )
    .unwrap_err();
    assert!(both.contains("request"), "{both}");

    // The request must reach the disposed delivery's sender.
    let misaddressed = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-misaddressed-request",
            "source": "@researcher what is the caller doing?",
            "in_reply_to": sent["message_id"],
            "author_kind": "agent_session",
            "attempt_id": attempt_id,
            "request_context": true,
        }),
        &reviewer,
    )
    .unwrap_err();
    assert!(misaddressed.contains("sender"), "{misaddressed}");

    // A second thread to the same pair, opened before the request so its
    // delivery reaches the caller first (one active attempt per address).
    let side_channel = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "send-side-channel",
            "source": "@source an unrelated thread",
            "author_kind": "agent_session",
        }),
        &reviewer,
    )
    .unwrap();
    let side_envelope = herdr.prompt_for_pane("pane-source");

    // A real request, then a fulfillment that dodges the requester: rejected.
    let requested = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-request-3",
            "source": "@source what does your working tree look like?",
            "in_reply_to": sent["message_id"],
            "author_kind": "agent_session",
            "attempt_id": attempt_id,
            "request_context": true,
        }),
        &reviewer,
    )
    .unwrap();
    let grant_id = requested["context_request"]["grant_id"].as_str().unwrap();

    // A fulfillment answers its request in the requesting thread: a reply in
    // the side thread to the same requester cannot activate the grant.
    let cross_thread = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-cross-thread-fulfill",
            "source": "@reviewer here is my context",
            "in_reply_to": side_channel["message_id"],
            "author_kind": "agent_session",
            "attempt_id": side_envelope["control"]["attempt_id"],
            "context": {
                "capture_git_state": false,
                "grant_ids": [grant_id],
                "snapshot": valid_snapshot(),
            },
        }),
        &source,
    )
    .unwrap_err();
    assert!(
        cross_thread.contains("same Collaboration Thread"),
        "{cross_thread}"
    );

    // Settle the side thread so the queued request delivery reaches the caller.
    execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-side-channel",
            "source": "@reviewer noted",
            "in_reply_to": side_channel["message_id"],
            "author_kind": "agent_session",
            "attempt_id": side_envelope["control"]["attempt_id"],
        }),
        &source,
    )
    .unwrap();
    let caller_envelope = herdr.prompt_for_pane("pane-source");
    assert_eq!(
        caller_envelope["control"]["context_request"]["grant_id"], grant_id,
        "{caller_envelope}"
    );
    let dodged = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-dodged-fulfill",
            "source": "@researcher here is my context",
            "in_reply_to": requested["message_id"],
            "author_kind": "agent_session",
            "attempt_id": caller_envelope["control"]["attempt_id"],
            "context": {
                "capture_git_state": false,
                "grant_ids": [grant_id],
                "snapshot": valid_snapshot(),
            },
        }),
        &source,
    )
    .unwrap_err();
    assert!(dodged.contains("grantee"), "{dodged}");

    // The failed fulfillment recorded an activation failure on the pending
    // grant for the human to see.
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let attention_items = |directory: &TestDirectory| {
        StateStore::open(&directory.state)
            .unwrap()
            .transaction(|transaction| transaction.unresolved_attention_items(&namespace))
            .unwrap()
            .len()
    };
    assert_eq!(attention_items(&directory), 1);

    // A fulfillment that fails validation before the routing transaction also
    // lands on the same open Attention Item (idempotently).
    let oversized = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-oversized-fulfill",
            "source": "@reviewer here is my context",
            "in_reply_to": requested["message_id"],
            "author_kind": "agent_session",
            "attempt_id": caller_envelope["control"]["attempt_id"],
            "context": {
                "capture_git_state": false,
                "grant_ids": [grant_id],
                "snapshot": {
                    "objective": "x".repeat(70_000),
                    "applicable_user_instructions": [],
                    "decisions": [],
                    "completed_work": [],
                    "current_work": [],
                    "blockers": [],
                    "evidence": []
                },
            },
        }),
        &source,
    )
    .unwrap_err();
    assert!(oversized.contains("65536 bytes"), "{oversized}");
    assert_eq!(attention_items(&directory), 1);

    // The correctly addressed in-thread fulfillment then succeeds and resolves
    // the failure Attention Item.
    let fulfilled = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "reply-guarded-fulfill",
            "source": "@reviewer here is my context",
            "in_reply_to": requested["message_id"],
            "author_kind": "agent_session",
            "attempt_id": caller_envelope["control"]["attempt_id"],
            "context": {
                "capture_git_state": false,
                "grant_ids": [grant_id],
                "snapshot": valid_snapshot(),
            },
        }),
        &source,
    )
    .unwrap();
    assert_eq!(fulfilled["context_grants"][0]["state"], "active");
    assert_eq!(attention_items(&directory), 0);
}

/// The pin an explicit snapshot publication takes describes the Space's own
/// Worktree. Sibling worktrees share one object database, so a pin taken from
/// the caller's directory would still resolve inside the Space and describe
/// uncommitted work the Space never had.
#[test]
fn snapshot_publication_pins_the_spaces_worktree_not_the_calling_directory() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let sibling = directory.root.join("sibling-checkout");
    git(
        &directory.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-f",
            "-b",
            "sibling",
            sibling.to_str().unwrap(),
            "HEAD",
        ],
    );
    fs::write(sibling.join("src/notes.txt"), "only the sibling changed\n").unwrap();
    let source = LifecycleRuntime {
        cwd: &sibling,
        workspace_id: Some("workspace-1"),
        ..runtime(&directory, &herdr, &ids, "pane-source")
    };
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);

    let published = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "thread_id": sent["thread_id"],
            "grantee_address_ids": ["address-reviewer"],
            "capture_git_state": true,
            "snapshot": valid_snapshot()
        }),
        &source,
    )
    .unwrap();
    let grant_id = published["grants"][0]["grant_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let snapshot = execute_context_operation(
        "context.read",
        serde_json::json!({"grant_id": grant_id, "selection": {"kind": "snapshot"}}),
        &reviewer,
    )
    .unwrap();
    let git_state = snapshot["content"]["git_state"].clone();
    let head = git(&directory.repo, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    // The Space's Worktree is clean, so the pin is its HEAD; the sibling's
    // uncommitted change would have pinned a dirty commit over it.
    assert_eq!(
        git_state["dirty"], false,
        "the pin describes the caller's tree: {git_state}"
    );
    assert_eq!(git_state["pinned_commit"].as_str().unwrap(), head);
    assert_eq!(git_state["branch"], "main");
    let pin_id = git_state["pin_id"].as_str().unwrap();
    assert_eq!(
        context_pin_refs(&directory),
        vec![format!("refs/herdr-context/{pin_id}")]
    );
    let diff = execute_context_operation(
        "context.read",
        serde_json::json!({
            "grant_id": grant_id,
            "selection": {"kind":"git_state", "path":"src/notes.txt", "start_line":1, "max_lines":200}
        }),
        &reviewer,
    )
    .unwrap();
    assert!(
        !diff["content"]
            .as_str()
            .unwrap()
            .contains("only the sibling changed"),
        "the pinned diff carries the caller's work: {}",
        diff["content"]
    );
}

/// Publishing a snapshot that needs files or Git refuses without the Space's
/// Worktree, even when the caller directory could satisfy that access.
#[test]
fn a_space_without_a_worktree_refuses_snapshot_publication() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    *herdr.no_worktree.lock().unwrap() = true;
    let detached = LifecycleRuntime {
        workspace_id: Some("workspace-1"),
        ..runtime(&directory, &herdr, &ids, "pane-source")
    };
    for (capture, evidence) in [
        (true, serde_json::json!([])),
        (
            false,
            serde_json::json!([{"kind":"file", "locator":"src/notes.txt"}]),
        ),
        (
            false,
            serde_json::json!([{"kind":"diff", "locator":"src/notes.txt"}]),
        ),
        (
            false,
            serde_json::json!([{"kind":"commit", "locator":"abcdef0:src/notes.txt"}]),
        ),
    ] {
        let mut snapshot = valid_snapshot();
        snapshot["evidence"] = evidence;
        let refused = execute_context_operation(
            "snapshot.publish",
            serde_json::json!({
                "thread_id": sent["thread_id"],
                "grantee_address_ids": ["address-reviewer"],
                "capture_git_state": capture,
                "snapshot": snapshot
            }),
            &detached,
        )
        .unwrap_err();
        assert!(refused.contains("no attached Git Worktree"), "{refused}");
    }
    assert!(
        context_pin_refs(&directory).is_empty(),
        "a refused publication must write no pin ref"
    );
}

/// `valid_snapshot` plus a `diff` pointer for the same path, so a test can read
/// both evidence forms of one file through the bounded evidence index.
fn snapshot_indexing_one_path(path: &str) -> serde_json::Value {
    let mut snapshot = valid_snapshot();
    snapshot["evidence"] = serde_json::json!([
        {"kind": "file", "locator": path},
        {"kind": "diff", "locator": path}
    ]);
    snapshot
}

/// Interpretation resolves against the same tree capture pinned. A sibling
/// checkout of the Space's repository holds different content at the same
/// path, so a read framed on the caller's directory would disclose that
/// content instead of the Space's — an answer about a tree nobody granted.
#[test]
fn context_reads_resolve_against_the_spaces_worktree_not_the_calling_directory() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    fs::write(directory.repo.join("src/notes.txt"), "the Space's line\n").unwrap();
    let published = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "thread_id": sent["thread_id"],
            "grantee_address_ids": ["address-reviewer"],
            "capture_git_state": true,
            "snapshot": snapshot_indexing_one_path("src/notes.txt")
        }),
        &source,
    )
    .unwrap();
    let grant_id = published["grants"][0]["grant_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // The reviewer reads from a sibling checkout whose same path differs.
    let sibling = directory.root.join("sibling-checkout");
    git(
        &directory.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-f",
            "-b",
            "sibling",
            sibling.to_str().unwrap(),
            "HEAD",
        ],
    );
    fs::write(sibling.join("src/notes.txt"), "the caller's line\n").unwrap();
    let elsewhere = LifecycleRuntime {
        cwd: &sibling,
        workspace_id: Some("workspace-1"),
        ..runtime(&directory, &herdr, &ids, "pane-reviewer")
    };
    let file = execute_context_operation(
        "context.read",
        serde_json::json!({
            "grant_id": grant_id,
            "selection": {"kind":"file", "locator":"src/notes.txt", "start_line":1, "max_lines":20}
        }),
        &elsewhere,
    )
    .unwrap();
    assert_eq!(
        file["content"], "the Space's line",
        "a file read disclosed the caller's tree"
    );
    let diff = execute_context_operation(
        "context.read",
        serde_json::json!({
            "grant_id": grant_id,
            "selection": {"kind":"diff", "locator":"src/notes.txt", "max_lines":200}
        }),
        &elsewhere,
    )
    .unwrap();
    let diff_content = diff["content"].as_str().unwrap();
    assert!(
        diff_content.contains("the Space's line") && !diff_content.contains("the caller's line"),
        "a diff read described the caller's tree: {diff_content}"
    );
}

#[test]
fn a_legacy_snapshot_in_a_space_without_a_worktree_refuses_context_reads_and_inspection() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    let published = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "thread_id": sent["thread_id"],
            "grantee_address_ids": ["address-reviewer"],
            "capture_git_state": true,
            "snapshot": snapshot_indexing_one_path("src/notes.txt")
        }),
        &source,
    )
    .unwrap();
    let grant_id = published["grants"][0]["grant_id"]
        .as_str()
        .unwrap()
        .to_owned();
    rewrite_stored_snapshot(
        &directory,
        &grant_id,
        "json_remove(payload_json, '$.checkout_binding')",
    );

    *herdr.no_worktree.lock().unwrap() = true;
    let detached = LifecycleRuntime {
        workspace_id: Some("workspace-1"),
        ..runtime(&directory, &herdr, &ids, "pane-reviewer")
    };
    for selection in [
        serde_json::json!({"kind":"file", "locator":"src/notes.txt", "start_line":1, "max_lines":20}),
        serde_json::json!({"kind":"diff", "locator":"src/notes.txt", "max_lines":20}),
        serde_json::json!({"kind":"git_state", "path":"src/notes.txt", "start_line":1, "max_lines":20}),
    ] {
        let refused = execute_context_operation(
            "context.read",
            serde_json::json!({"grant_id": grant_id, "selection": selection.clone()}),
            &detached,
        )
        .unwrap_err();
        assert!(
            refused.contains("no attached Git Worktree"),
            "{selection} must not be resolved without a context root: {refused}"
        );
    }
    let inspected = execute_context_operation(
        "context.inspect",
        serde_json::json!({
            "message_id": sent["message_id"],
            "reference_address_id": "address-source"
        }),
        &detached,
    )
    .unwrap_err();
    assert!(
        inspected.contains("no attached Git Worktree"),
        "an authorized inspection must not judge evidence paths without a context root: \
         {inspected}"
    );
    // The snapshot itself reaches no tree, so it still reads.
    assert!(
        execute_context_operation(
            "context.read",
            serde_json::json!({"grant_id": grant_id, "selection": {"kind":"snapshot"}}),
            &detached,
        )
        .unwrap()["content"]["objective"]
            .is_string()
    );
}

fn rewrite_stored_snapshot(directory: &TestDirectory, grant_id: &str, payload_expression: &str) {
    rusqlite::Connection::open(directory.state.join("namedrop.sqlite3"))
        .unwrap()
        .execute_batch(&format!(
            "CREATE TEMP TABLE rewritten AS
                 SELECT * FROM working_context_snapshots WHERE grant_id = '{grant_id}';
             DELETE FROM working_context_snapshots WHERE grant_id = '{grant_id}';
             INSERT INTO working_context_snapshots
                 SELECT session_socket_id, worktree_id, snapshot_id, grant_id, agent_session_id,
                        {payload_expression}, created_at_ms
                 FROM rewritten;
             DROP TABLE rewritten;"
        ))
        .unwrap();
}

fn stored_rows(directory: &TestDirectory, table: &str) -> i64 {
    rusqlite::Connection::open(directory.state.join("namedrop.sqlite3"))
        .unwrap()
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn add_checkout(directory: &TestDirectory, name: &str) -> PathBuf {
    let checkout = directory.root.join(name);
    git(
        &directory.repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            name,
            checkout.to_str().unwrap(),
            "HEAD",
        ],
    );
    checkout
}

fn canonical(path: &std::path::Path) -> String {
    path.canonicalize().unwrap().display().to_string()
}

fn read_evidence(
    reader: &LifecycleRuntime<'_, namedrop::TuiHost<&'_ FakeHerdr>, FixedClock, SequenceIds>,
    grant_id: &str,
    selection: serde_json::Value,
) -> Result<serde_json::Value, String> {
    execute_context_operation(
        "context.read",
        serde_json::json!({"grant_id": grant_id, "selection": selection}),
        reader,
    )
    .map(|read| read["content"].clone())
}

fn file_selection(path: &str) -> serde_json::Value {
    serde_json::json!({"kind":"file", "locator":path, "start_line":1, "max_lines":20})
}

fn git_state_selection(path: &str) -> serde_json::Value {
    serde_json::json!({"kind":"git_state", "path":path, "start_line":1, "max_lines":200})
}

#[test]
fn an_explicit_checkout_binds_capture_reads_and_protection_to_that_worktree() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    let worker = add_checkout(&directory, "worker");
    fs::write(worker.join("src/history.txt"), "worker history\n").unwrap();
    git(&worker, &["add", "src/history.txt"]);
    git(&worker, &["commit", "-q", "-m", "worker history"]);
    let commit = git(&worker, &["rev-parse", "HEAD"]).trim().to_owned();
    fs::write(directory.repo.join("src/notes.txt"), "the Space's line\n").unwrap();
    fs::write(worker.join("src/notes.txt"), "the worker's line\n").unwrap();
    fs::write(worker.join("src/[x].txt"), "bracketed worker file\n").unwrap();
    fs::write(directory.repo.join("src/link.txt"), "the Space's file\n").unwrap();
    std::os::unix::fs::symlink(
        directory.root.join("outside.txt"),
        worker.join("src/link.txt"),
    )
    .unwrap();
    std::os::unix::fs::symlink(
        directory.root.join("outside.txt"),
        directory.repo.join("src/history.txt"),
    )
    .unwrap();
    let mut snapshot = valid_snapshot();
    snapshot["evidence"] = serde_json::json!([
        {"kind": "file", "locator": "src/notes.txt"},
        {"kind": "diff", "locator": "src/notes.txt"},
        {"kind": "file", "locator": "src/[x].txt"},
        {"kind": "file", "locator": "src/link.txt"},
        {"kind": "commit", "locator": format!("{commit}:src/history.txt")}
    ]);
    for forged in ["checkout_binding", "checkout"] {
        let mut forged_snapshot = snapshot.clone();
        forged_snapshot[forged] = serde_json::json!({"path": worker});
        let refused = execute_context_operation(
            "snapshot.publish",
            serde_json::json!({
                "thread_id": sent["thread_id"],
                "grantee_address_ids": ["address-reviewer"],
                "snapshot": forged_snapshot
            }),
            &source,
        )
        .unwrap_err();
        assert!(refused.contains("unknown field"), "{forged}: {refused}");
    }

    let published = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "thread_id": sent["thread_id"],
            "grantee_address_ids": ["address-reviewer"],
            "capture_git_state": true,
            "checkout": {"path": worker},
            "snapshot": snapshot
        }),
        &source,
    )
    .unwrap();
    let summary = serde_json::json!({
        "state": "bound",
        "selection": "explicit",
        "root": canonical(&worker),
        "label": "worker",
        "layout": "linked",
    });
    assert_eq!(published["checkout"], summary);
    let grant_id = published["grants"][0]["grant_id"].as_str().unwrap();
    let stored =
        read_evidence(&reviewer, grant_id, serde_json::json!({"kind":"snapshot"})).unwrap();
    assert_eq!(stored["checkout"], summary);
    assert!(stored.get("checkout_binding").is_none(), "{stored}");
    assert!(!stored.to_string().contains("inode"), "{stored}");
    assert_eq!(stored["git_state"]["branch"], "worker");
    assert_eq!(stored["git_state"]["dirty"], true);
    assert_eq!(stored["git_state"]["head"].as_str().unwrap(), commit);

    let pinned = read_evidence(&reviewer, grant_id, git_state_selection("src/notes.txt")).unwrap();
    let pinned = pinned.as_str().unwrap();
    assert!(
        pinned.contains("the worker's line") && !pinned.contains("the Space's line"),
        "{pinned}"
    );
    assert_eq!(
        read_evidence(&reviewer, grant_id, file_selection("src/notes.txt")).unwrap(),
        "the worker's line"
    );
    let diff = read_evidence(
        &reviewer,
        grant_id,
        serde_json::json!({"kind":"diff", "locator":"src/notes.txt", "max_lines":200}),
    )
    .unwrap();
    assert!(
        diff.as_str().unwrap().contains("the worker's line"),
        "{diff}"
    );
    assert_eq!(
        read_evidence(&reviewer, grant_id, file_selection("src/[x].txt")).unwrap(),
        "bracketed worker file"
    );
    let escaping = read_evidence(&reviewer, grant_id, file_selection("src/link.txt")).unwrap_err();
    assert!(escaping.contains("evidence path is blocked"), "{escaping}");
    let history = read_evidence(
        &reviewer,
        grant_id,
        serde_json::json!({"kind":"commit", "locator":format!("{commit}:src/history.txt"), "max_lines":200}),
    )
    .unwrap();
    assert!(
        history.as_str().unwrap().contains("worker history"),
        "{history}"
    );
    let inspected = execute_context_operation(
        "context.inspect",
        serde_json::json!({"message_id": sent["message_id"], "reference_address_id": "address-source"}),
        &reviewer,
    )
    .unwrap();
    assert_eq!(inspected["checkout"], summary);
    let blocked = inspected["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["blocked"].as_bool().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(blocked, [false, false, false, true, false]);

    fs::write(worker.join("src/notes.txt"), "the worker changed it\n").unwrap();
    assert!(
        read_evidence(&reviewer, grant_id, git_state_selection("src/notes.txt"))
            .unwrap()
            .as_str()
            .unwrap()
            .contains("the worker's line")
    );
    assert_eq!(
        read_evidence(&reviewer, grant_id, file_selection("src/notes.txt")).unwrap(),
        "the worker changed it"
    );

    let pins = context_pin_refs(&directory);
    let mut uncaptured = valid_snapshot();
    uncaptured["evidence"] = serde_json::json!([{"kind": "file", "locator": "src/notes.txt"}]);
    let published = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "thread_id": sent["thread_id"],
            "grantee_address_ids": ["address-reviewer"],
            "checkout": {"path": worker},
            "snapshot": uncaptured
        }),
        &source,
    )
    .unwrap();
    assert_eq!(published["checkout"], summary);
    assert_eq!(context_pin_refs(&directory), pins);
    let grant_id = published["grants"][0]["grant_id"].as_str().unwrap();
    let stored =
        read_evidence(&reviewer, grant_id, serde_json::json!({"kind":"snapshot"})).unwrap();
    assert!(stored["git_state"].is_null(), "{stored}");
    assert_eq!(stored["checkout"], summary);
    assert_eq!(
        read_evidence(&reviewer, grant_id, file_selection("src/notes.txt")).unwrap(),
        "the worker changed it"
    );
}

#[test]
fn an_invalid_or_unattached_checkout_selector_writes_nothing() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    let prompts = herdr.prompts.lock().unwrap().len();
    let worker = add_checkout(&directory, "worker");
    let plain = directory.root.join("plain");
    fs::create_dir(&plain).unwrap();
    let other = directory.root.join("other");
    fs::create_dir(&other).unwrap();
    git(&other, &["init", "-q"]);
    let pointer_copy = directory.root.join("pointer-copy");
    fs::create_dir(&pointer_copy).unwrap();
    fs::copy(worker.join(".git"), pointer_copy.join(".git")).unwrap();
    let cases = [
        ("worker".to_owned(), "must be an absolute path"),
        (
            worker.join("src").display().to_string(),
            "select the worktree root itself",
        ),
        (plain.display().to_string(), "is not a Git worktree"),
        (other.display().to_string(), "different repository"),
        (
            pointer_copy.display().to_string(),
            "not a worktree registered",
        ),
        (
            directory.root.join("missing").display().to_string(),
            "does not exist",
        ),
    ];
    let mut snapshot = valid_snapshot();
    snapshot["evidence"] = serde_json::json!([{"kind": "diff", "locator": "src/notes.txt"}]);
    let attempt = |path: &str,
                   runtime: &LifecycleRuntime<
        '_,
        namedrop::TuiHost<&'_ FakeHerdr>,
        FixedClock,
        SequenceIds,
    >| {
        let published = execute_context_operation(
            "snapshot.publish",
            serde_json::json!({
                "thread_id": sent["thread_id"],
                "grantee_address_ids": ["address-reviewer"],
                "capture_git_state": true,
                "checkout": {"path": path},
                "snapshot": snapshot
            }),
            runtime,
        )
        .unwrap_err();
        let sent = execute_message_operation(
            "send",
            serde_json::json!({
                "request_id": format!("invalid-{path}"),
                "source": "@reviewer Review this.",
                "author_kind": "agent_session",
                "context": {"checkout": {"path": path}, "snapshot": snapshot}
            }),
            runtime,
        )
        .unwrap_err();
        assert_eq!(published, sent);
        published
    };
    for (path, refusal) in cases {
        let refused = attempt(&path, &source);
        assert!(refused.contains(refusal), "{path}: {refused}");
    }
    *herdr.no_worktree.lock().unwrap() = true;
    let detached = LifecycleRuntime {
        workspace_id: Some("workspace-1"),
        ..source
    };
    let refused = attempt(worker.to_str().unwrap(), &detached);
    assert!(refused.contains("no attached Git Worktree"), "{refused}");
    assert!(context_pin_refs(&directory).is_empty());
    assert_eq!(stored_rows(&directory, "context_grants"), 0);
    assert_eq!(stored_rows(&directory, "working_context_snapshots"), 0);
    assert_eq!(herdr.prompts.lock().unwrap().len(), prompts);
}

#[test]
fn a_removed_or_replaced_checkout_refuses_evidence_without_reading_the_space_instead() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    let worker = add_checkout(&directory, "worker");
    fs::write(worker.join("src/notes.txt"), "the worker's line\n").unwrap();
    std::os::unix::fs::symlink(
        directory.root.join("outside.txt"),
        worker.join("src/innocuous-name.txt"),
    )
    .unwrap();
    let head = git(&worker, &["rev-parse", "HEAD"]).trim().to_owned();
    let mut snapshot = snapshot_indexing_one_path("src/notes.txt");
    let evidence = snapshot["evidence"].as_array_mut().unwrap();
    evidence.push(serde_json::json!({"kind":"commit", "locator":format!("{head}:src/notes.txt")}));
    evidence.push(serde_json::json!({"kind":"file", "locator":"src/innocuous-name.txt"}));
    let published = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "thread_id": sent["thread_id"],
            "grantee_address_ids": ["address-reviewer"],
            "capture_git_state": true,
            "checkout": {"path": worker},
            "snapshot": snapshot
        }),
        &source,
    )
    .unwrap();
    let grant_id = published["grants"][0]["grant_id"].as_str().unwrap();
    let selections = [
        file_selection("src/notes.txt"),
        serde_json::json!({"kind":"diff", "locator":"src/notes.txt", "max_lines":20}),
        git_state_selection("src/notes.txt"),
        serde_json::json!({"kind":"commit", "locator":format!("{head}:src/notes.txt"), "max_lines":20}),
    ];
    git(
        &directory.repo,
        &["worktree", "remove", "--force", worker.to_str().unwrap()],
    );
    let assert_unavailable = |reason: &str| {
        for selection in &selections {
            let refused = read_evidence(&reviewer, grant_id, selection.clone()).unwrap_err();
            assert!(
                refused.contains(&format!("checkout is unavailable ({reason})")),
                "{selection}: {refused}"
            );
        }
        let stored =
            read_evidence(&reviewer, grant_id, serde_json::json!({"kind":"snapshot"})).unwrap();
        assert_eq!(stored["objective"], "Review the context boundary");
        assert_eq!(stored["checkout"]["state"], "unavailable");
        assert_eq!(stored["checkout"]["reason"], reason);
        assert_eq!(
            stored["checkout"]["root"],
            canonical(&directory.root).clone() + "/worker"
        );
        let inspected = execute_context_operation(
            "context.inspect",
            serde_json::json!({"message_id": sent["message_id"], "reference_address_id": "address-source"}),
            &reviewer,
        )
        .unwrap();
        assert_eq!(inspected["checkout"]["reason"], reason);
        assert_eq!(stored["evidence"][3]["locator"], "[BLOCKED PATH]");
        for entry in inspected["evidence"].as_array().unwrap() {
            assert_eq!(entry["available"], false, "{entry}");
            assert_eq!(entry["checkout_reason"], reason, "{entry}");
            assert_eq!(entry["locator"], "[BLOCKED PATH]", "{entry}");
            assert_eq!(entry["blocked"], true, "{entry}");
            assert!(
                entry["reason"]
                    .as_str()
                    .unwrap()
                    .contains("cannot be verified while the checkout is unavailable"),
                "{entry}"
            );
        }
    };
    assert_unavailable("root_missing");
    git(
        &directory.repo,
        &["worktree", "add", "-q", worker.to_str().unwrap(), "worker"],
    );
    fs::write(worker.join("src/notes.txt"), "the worker's line\n").unwrap();
    assert_unavailable("root_replaced");
}

#[test]
fn a_bound_snapshot_keeps_its_checkout_when_the_space_reattaches_or_detaches() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    let worker = add_checkout(&directory, "worker");
    fs::write(directory.repo.join("src/notes.txt"), "the Space's line\n").unwrap();
    fs::write(worker.join("src/notes.txt"), "the worker's line\n").unwrap();
    let publish = || {
        execute_context_operation(
            "snapshot.publish",
            serde_json::json!({
                "thread_id": sent["thread_id"],
                "grantee_address_ids": ["address-reviewer"],
                "snapshot": snapshot_indexing_one_path("src/notes.txt")
            }),
            &source,
        )
        .unwrap()["grants"][0]["grant_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let bound = publish();
    let legacy = publish();
    rewrite_stored_snapshot(
        &directory,
        &legacy,
        "json_remove(payload_json, '$.checkout_binding')",
    );
    let stored = read_evidence(&reviewer, &legacy, serde_json::json!({"kind":"snapshot"})).unwrap();
    assert!(stored.get("checkout").is_none(), "{stored}");
    assert!(stored.get("checkout_binding").is_none(), "{stored}");

    *herdr.worktree.lock().unwrap() = worker.display().to_string();
    let reviewer = LifecycleRuntime {
        workspace_id: Some("workspace-1"),
        ..reviewer
    };
    assert_eq!(
        read_evidence(&reviewer, &bound, file_selection("src/notes.txt")).unwrap(),
        "the Space's line"
    );
    assert_eq!(
        read_evidence(&reviewer, &legacy, file_selection("src/notes.txt")).unwrap(),
        "the worker's line"
    );
    *herdr.no_worktree.lock().unwrap() = true;
    assert_eq!(
        read_evidence(&reviewer, &bound, file_selection("src/notes.txt")).unwrap(),
        "the Space's line"
    );
    let refused = read_evidence(&reviewer, &legacy, file_selection("src/notes.txt")).unwrap_err();
    assert!(refused.contains("no attached Git Worktree"), "{refused}");
}

#[test]
fn an_unsupported_or_invalid_binding_is_unavailable_and_never_legacy() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    for (expression, reason) in [
        (
            "json_set(payload_json, '$.checkout_binding.version', 2)",
            "unsupported_binding_version",
        ),
        (
            "json_set(payload_json, '$.checkout_binding', 'not a binding')",
            "invalid_binding",
        ),
    ] {
        let published = execute_context_operation(
            "snapshot.publish",
            serde_json::json!({
                "thread_id": sent["thread_id"],
                "grantee_address_ids": ["address-reviewer"],
                "snapshot": snapshot_indexing_one_path("src/notes.txt")
            }),
            &source,
        )
        .unwrap();
        let grant_id = published["grants"][0]["grant_id"].as_str().unwrap();
        rewrite_stored_snapshot(&directory, grant_id, expression);
        let refused =
            read_evidence(&reviewer, grant_id, file_selection("src/notes.txt")).unwrap_err();
        assert!(
            refused.contains(&format!("checkout is unavailable ({reason})")),
            "{refused}"
        );
        let stored =
            read_evidence(&reviewer, grant_id, serde_json::json!({"kind":"snapshot"})).unwrap();
        assert_eq!(
            stored["checkout"],
            serde_json::json!({"state":"unavailable", "reason":reason})
        );
    }
}

#[test]
fn a_sweep_from_one_checkout_keeps_the_pin_of_an_active_grant_in_another() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    let worker = add_checkout(&directory, "worker");
    fs::write(worker.join("src/notes.txt"), "the worker's line\n").unwrap();
    let publish = |checkout: Option<&PathBuf>| {
        let mut input = serde_json::json!({
            "thread_id": sent["thread_id"],
            "grantee_address_ids": ["address-reviewer"],
            "capture_git_state": true,
            "snapshot": valid_snapshot()
        });
        if let Some(checkout) = checkout {
            input["checkout"] = serde_json::json!({"path": checkout});
        }
        let grant_id = execute_context_operation("snapshot.publish", input, &source).unwrap()
            ["grants"][0]["grant_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let stored =
            read_evidence(&reviewer, &grant_id, serde_json::json!({"kind":"snapshot"})).unwrap();
        let pin = format!(
            "refs/herdr-context/{}",
            stored["git_state"]["pin_id"].as_str().unwrap()
        );
        (grant_id, pin)
    };
    let (worker_grant, worker_pin) = publish(Some(&worker));
    assert!(
        git(&worker, &["for-each-ref", "refs/herdr-context"]).contains(&worker_pin),
        "linked worktrees share the pin namespace"
    );
    let (_, space_pin) = publish(None);
    let pins = |expected: &[&String]| {
        let mut expected = expected
            .iter()
            .map(|pin| pin.to_string())
            .collect::<Vec<_>>();
        expected.sort();
        assert_eq!(context_pin_refs(&directory), expected);
    };
    pins(&[&worker_pin, &space_pin]);
    execute_human_context_operation(
        "context.revoke",
        serde_json::json!({"grant_id": worker_grant}),
        &source,
    )
    .unwrap();
    let (_, later_pin) = publish(None);
    pins(&[&space_pin, &later_pin]);
}

#[test]
fn capture_and_reads_in_a_selected_checkout_never_run_filesystem_monitor_hooks() {
    use std::os::unix::fs::PermissionsExt;
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent = create_referenced_thread(&source);
    let worker = add_checkout(&directory, "worker");
    fs::write(worker.join("src/notes.txt"), "the worker's line\n").unwrap();
    let marker = directory.root.join("fsmonitor-ran");
    let hook = directory.root.join("fsmonitor-hook");
    fs::write(
        &hook,
        format!(
            "#!/bin/sh\ntouch '{}'\nprintf 'token\\0'\n",
            marker.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &worker,
        &["config", "core.fsmonitor", hook.to_str().unwrap()],
    );
    let published = execute_context_operation(
        "snapshot.publish",
        serde_json::json!({
            "thread_id": sent["thread_id"],
            "grantee_address_ids": ["address-reviewer"],
            "capture_git_state": true,
            "checkout": {"path": worker},
            "snapshot": snapshot_indexing_one_path("src/notes.txt")
        }),
        &source,
    )
    .unwrap();
    let grant_id = published["grants"][0]["grant_id"].as_str().unwrap();
    for selection in [
        serde_json::json!({"kind":"diff", "locator":"src/notes.txt", "max_lines":20}),
        git_state_selection("src/notes.txt"),
        file_selection("src/notes.txt"),
    ] {
        read_evidence(&reviewer, grant_id, selection).unwrap();
    }
    assert!(!marker.exists(), "a filesystem monitor hook ran");
}
