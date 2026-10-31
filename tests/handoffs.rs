#[path = "support/environment.rs"]
mod environment;
use environment::CommandHome;

use namedrop::{
    ActorKind, AgentRuntime, AgentSessionId, Clock, HerdrCommandExecutor, HerdrCommandOutput,
    IdGenerator, LifecycleRuntime, MutationAttribution, PaneAddressDraft, PaneAddressId,
    PaneBindingId, PaneRoleId, StateNamespace, StateStore, complete_capability_profile,
    execute_handoff_operation, execute_integration_operation, execute_message_operation,
    issue_registration_challenge,
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
            "namedrop-handoff-test-{}-{}",
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
    source_working: std::sync::Mutex<bool>,
    terminal: std::sync::Mutex<bool>,
    shell_ready: std::sync::Mutex<bool>,
    terminal_runs: std::sync::Mutex<Vec<String>>,
    worktree: String,
    /// Report this Space as not being a Git Worktree at all.
    no_worktree: std::sync::Mutex<bool>,
    live_pane_override: std::sync::Mutex<Option<(String, Option<String>)>>,
    /// Serialized Delivery Envelopes handed to `agent prompt`, per pane.
    prompts: std::sync::Mutex<Vec<(String, String)>>,
}

impl FakeHerdr {
    fn new(worktree: &TestDirectory) -> Self {
        Self {
            source_working: std::sync::Mutex::new(false),
            terminal: std::sync::Mutex::new(false),
            shell_ready: std::sync::Mutex::new(true),
            terminal_runs: std::sync::Mutex::new(vec![]),
            worktree: worktree.repo.display().to_string(),
            no_worktree: std::sync::Mutex::new(false),
            live_pane_override: std::sync::Mutex::new(None),
            prompts: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Herdr's answer for either Worktree lookup. `no_worktree` makes it report
    /// a Space that is not a Git Worktree, the shape `is_no_worktree_error`
    /// recognizes.
    fn worktree_list(&self) -> Result<String, String> {
        if *self.no_worktree.lock().unwrap() {
            return Err(serde_json::json!({"error":{"code":"not_git_worktree"}}).to_string());
        }
        Ok(serde_json::json!({
            "result": {
                "worktrees": [{
                    "path": self.worktree,
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
            ["pane","process-info","--pane","pane-reviewer"] => serde_json::json!({"result":{"process_info":{"shell_pid":42,"foreground_process_group_id":if *self.shell_ready.lock().unwrap(){42}else{43},"foreground_processes":[{"pid":std::process::id()}]}}}).to_string(),
            ["pane","run","pane-reviewer",command]=>{self.terminal_runs.lock().unwrap().push(command.to_string());"{}".to_owned()},
            // A Herdr pane exports HERDR_WORKSPACE_ID, so an installed session
            // resolves its Space by workspace id rather than by the invoking
            // process's directory. Either way the Space's Worktree path comes
            // from Herdr, so both lookups answer identically — unless this Space
            // has no Git Worktree, which Herdr reports as an error code.
            ["workspace", "list"] => {
                serde_json::json!({"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}).to_string()
            }
            ["worktree", "list", "--cwd", cwd, "--json"] if *cwd == self.worktree => {
                self.worktree_list()?
            }
            ["worktree", "list", "--workspace", "workspace-1", "--json"] => {
                self.worktree_list()?
            }
            ["pane", "list", "--workspace", "workspace-1"] => serde_json::json!({
                "result": {
                    "panes": [
                        {
                            "pane_id": "pane-source",
                            "terminal_id": "terminal-source",
                            "tab_id": "tab-1",
                            "workspace_id": "workspace-1",
                            "agent_session": {"value": "session-source"},
                            "agent_status": if *self.source_working.lock().unwrap() { "working" } else { "idle" }
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
            ["pane","report-metadata",..] => "{}".to_owned(),
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
        if matches!(args, ["pane", "list", ..]) && *self.terminal.lock().unwrap() {
            let mut value: serde_json::Value = serde_json::from_str(&stdout).unwrap();
            for pane in value["result"]["panes"].as_array_mut().unwrap() {
                if pane["pane_id"] == "pane-reviewer" {
                    pane.as_object_mut().unwrap().remove("agent_session");
                    pane["agent_status"] = serde_json::json!("unknown");
                }
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

#[test]
fn terminal_command_returns_exit_output_once_and_waits_for_shell() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    register("address-source", &source);
    *herdr.terminal.lock().unwrap() = true;
    let terminal = runtime(&directory, &herdr, &ids, "pane-reviewer");
    namedrop::execute_lifecycle_operation(
        "name",
        serde_json::json!({"alias":"build","pane_type":"terminal"}),
        &terminal,
    )
    .unwrap();
    *herdr.shell_ready.lock().unwrap() = false;
    let sent=execute_message_operation("send",serde_json::json!({"request_id":"terminal-request","source":"@build printf 'hello'; printf 'problem' >&2; exit 7","author_kind":"agent_session"}),&source).unwrap();
    assert!(herdr.terminal_runs.lock().unwrap().is_empty());
    assert!(
        namedrop::execute_lifecycle_operation(
            "name",
            serde_json::json!({"alias":"build","pane_type":"agent"}),
            &terminal
        )
        .unwrap_err()
        .contains("pending deliveries")
    );
    *herdr.shell_ready.lock().unwrap() = true;
    let drain = namedrop::drain_mailboxes(&source).unwrap();
    assert_eq!(drain.submitted.len(), 1);
    assert_eq!(drain.terminal_running, drain.submitted);
    let attempt = drain.submitted[0].as_str();
    // A native session appearing after dispatch prevents the helper from claiming.
    *herdr.terminal.lock().unwrap() = false;
    assert!(
        namedrop::execute_terminal(attempt, &terminal)
            .unwrap_err()
            .contains("agent")
    );
    *herdr.terminal.lock().unwrap() = true;
    assert_eq!(namedrop::execute_terminal(attempt, &terminal).unwrap(), 7);
    assert!(namedrop::execute_terminal(attempt, &terminal).is_err());
    let result = execute_handoff_operation(
        "handoff.results",
        serde_json::json!({"message_id":sent["message_id"]}),
        &source,
    )
    .unwrap();
    assert_eq!(result["complete"], true);
    assert_eq!(result["recipients"][0]["result_status"], "replied");
    let body = result["recipients"][0]["untrusted_reply"].as_str().unwrap();
    assert!(
        body.contains("exit code 7") && body.contains("hello") && body.contains("problem"),
        "{body}"
    );
    assert_eq!(herdr.terminal_runs.lock().unwrap().len(), 1);
    assert_eq!(result["recipients"][0]["terminal_result"]["exit_code"], 7);
    // Native leading mentions preserve the original request but execute only its command body.
    let hook=namedrop::execute_user_prompt_submit_hook(&serde_json::json!({"hook_event_name":"UserPromptSubmit","prompt":"@build printf 'direct command'"}).to_string(),&source).unwrap();
    assert_eq!(hook["decision"], "block");
    namedrop::drain_mailboxes(&source).unwrap();
    // Agent result delivery can also occur in this drain; take the last terminal runner's ID.
    let run = herdr.terminal_runs.lock().unwrap().last().unwrap().clone();
    let id = run.split('\'').rev().nth(1).unwrap();
    assert_eq!(namedrop::execute_terminal(id, &terminal).unwrap(), 0);
}

#[test]
fn schema_two_upgrade_preserves_names_as_agents() {
    let directory = TestDirectory::new();
    setup(&directory);
    let db = rusqlite::Connection::open(directory.state.join("namedrop.sqlite3")).unwrap();
    db.execute_batch("DROP INDEX transition_facts_by_entity; DROP VIEW delivery_seats; DROP TABLE bridged_threads; DROP TABLE bridged_messages; DROP TABLE space_envoys; DROP TABLE coordination_links; DROP TABLE coordination_assignments; DROP TABLE coordinations; DROP TABLE terminal_executions; ALTER TABLE pane_addresses DROP COLUMN pane_type; ALTER TABLE pane_addresses DROP COLUMN remote_commands_terminal_id; ALTER TABLE pane_addresses DROP COLUMN persona; PRAGMA user_version=2;").unwrap();
    drop(db);
    let mut store = StateStore::open(&directory.state).unwrap();
    assert_eq!(store.schema_version().unwrap(), 7);
    let addresses = store
        .transaction(|tx| {
            tx.list_pane_addresses(&StateNamespace::for_space("socket-a", "workspace-1"))
        })
        .unwrap();
    assert_eq!(addresses.len(), 3);
    assert!(
        addresses
            .iter()
            .all(|a| a.pane_type == namedrop::PaneType::Agent)
    );
}

#[test]
fn joined_results_do_not_block_a_source_converted_to_terminal() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let researcher = runtime(&directory, &herdr, &ids, "pane-researcher");
    register("address-source", &source);
    register("address-researcher", &researcher);
    *herdr.terminal.lock().unwrap() = true;
    let terminal = runtime(&directory, &herdr, &ids, "pane-reviewer");
    namedrop::execute_lifecycle_operation(
        "name",
        serde_json::json!({"alias":"build","pane_type":"terminal"}),
        &terminal,
    )
    .unwrap();
    let sent=execute_message_operation("send",serde_json::json!({"request_id":"group-conversion","source":"@build @researcher true","author_kind":"agent_session"}),&source).unwrap();
    namedrop::execute_lifecycle_operation(
        "name",
        serde_json::json!({"alias":"source","pane_type":"terminal"}),
        &source,
    )
    .unwrap();
    let run = herdr.terminal_runs.lock().unwrap().last().unwrap().clone();
    namedrop::execute_terminal(run.split('\'').rev().nth(1).unwrap(), &terminal).unwrap();
    let envelope = herdr.prompt_for_pane("pane-researcher");
    execute_integration_operation(
        "complete",
        serde_json::json!({"attempt_id":envelope["control"]["attempt_id"]}),
        &researcher,
    )
    .unwrap();
    let result = execute_handoff_operation(
        "handoff.results",
        serde_json::json!({"message_id":sent["message_id"]}),
        &source,
    )
    .unwrap();
    assert_eq!(result["complete"], true);
    let drain = namedrop::drain_mailboxes(&source).unwrap();
    assert!(drain.terminal_waiting.is_empty() && drain.terminal_running.is_empty());
}

#[test]
fn terminal_results_do_not_block_a_source_converted_to_terminal() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    *herdr.terminal.lock().unwrap() = true;
    let terminal = runtime(&directory, &herdr, &ids, "pane-reviewer");
    namedrop::execute_lifecycle_operation(
        "name",
        serde_json::json!({"alias":"build","pane_type":"terminal"}),
        &terminal,
    )
    .unwrap();
    let sent = execute_message_operation(
        "send",
        serde_json::json!({"request_id":"conversion","source":"@build true"}),
        &source,
    )
    .unwrap();
    namedrop::execute_lifecycle_operation(
        "name",
        serde_json::json!({"alias":"source","pane_type":"terminal"}),
        &source,
    )
    .unwrap();
    let attempt = sent["drain"]["submitted"][0].as_str().unwrap();
    assert_eq!(namedrop::execute_terminal(attempt, &terminal).unwrap(), 0);
    let drain = namedrop::drain_mailboxes(&source).unwrap();
    assert!(
        drain.submitted.is_empty()
            && drain.terminal_running.is_empty()
            && drain.terminal_waiting.is_empty()
    );
    // Sending from an already marked terminal is also accepted and cannot poison its mailbox.
    let sent = execute_message_operation(
        "send",
        serde_json::json!({"request_id":"terminal-origin","source":"@build true"}),
        &source,
    )
    .unwrap();
    assert_eq!(
        namedrop::execute_terminal(sent["drain"]["submitted"][0].as_str().unwrap(), &terminal)
            .unwrap(),
        0
    );
    assert!(
        namedrop::drain_mailboxes(&source)
            .unwrap()
            .terminal_waiting
            .is_empty()
    );
}

#[test]
fn status_reports_only_the_calling_sessions_outstanding_handoffs_without_writes() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    register("address-source", &source);
    let sent = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "status-request", "source": "@reviewer Please inspect this.",
            "author_kind": "agent_session"
        }),
        &source,
    )
    .unwrap();
    let before = fs::read(directory.state.join("namedrop.sqlite3")).unwrap();
    let prompt_count = herdr.prompts.lock().unwrap().len();
    let status =
        execute_handoff_operation("handoff.status", serde_json::json!({}), &source).unwrap();
    let handoff = &status["handoffs"][0];
    assert_eq!(handoff["message_id"], sent["message_id"]);
    assert_eq!(handoff["request"], "Please inspect this.");
    assert_eq!(handoff["elapsed_ms"], 0);
    assert_eq!(handoff["complete"], false);
    assert_eq!(handoff["recipients"][0]["target"]["alias"], "reviewer");
    assert_eq!(handoff["recipients"][0]["state"], "queued");
    assert_eq!(
        fs::read(directory.state.join("namedrop.sqlite3")).unwrap(),
        before
    );
    assert_eq!(herdr.prompts.lock().unwrap().len(), prompt_count);
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-reviewer", &reviewer);
    assert!(
        execute_handoff_operation(
            "handoff.results",
            serde_json::json!({"message_id":sent["message_id"]}),
            &reviewer
        )
        .is_err()
    );
    complete_reconciliation(&directory);
    let status =
        execute_handoff_operation("handoff.status", serde_json::json!({}), &source).unwrap();
    assert_eq!(
        status["handoffs"][0]["recipients"][0]["outlook"]["constraints"],
        serde_json::json!([])
    );
    StateStore::open(&directory.state)
        .unwrap()
        .transaction(|tx| {
            tx.pause_thread(
                &StateNamespace::for_space("socket-a", "workspace-1"),
                &namedrop::ThreadId::new(sent["thread_id"].as_str().unwrap()),
                &MutationAttribution::new(ActorKind::Human, "test", 42),
            )
        })
        .unwrap();
    let status =
        execute_handoff_operation("handoff.status", serde_json::json!({}), &source).unwrap();
    let outlook = &status["handoffs"][0]["recipients"][0]["outlook"];
    assert_eq!(outlook["constraints"], serde_json::json!(["thread_paused"]));
    assert_eq!(
        outlook["delivery_expectation"],
        "queued until constraints clear"
    );
}

fn complete_reconciliation(directory: &TestDirectory) {
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    StateStore::open(&directory.state)
        .unwrap()
        .transaction(|transaction| transaction.complete_reconciliation(&namespace))
        .unwrap();
}

#[test]
fn partial_results_leave_the_final_join_pending_and_report_each_terminal_outcome() {
    let directory = TestDirectory::new();
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    let researcher = runtime(&directory, &herdr, &ids, "pane-researcher");
    for (id, rt) in [
        ("address-source", &source),
        ("address-reviewer", &reviewer),
        ("address-researcher", &researcher),
    ] {
        register(id, rt);
    }
    let sent = execute_message_operation("send", serde_json::json!({
        "request_id":"partial-request", "source":"@reviewer @researcher Inspect this.", "author_kind":"agent_session"
    }), &source).unwrap();
    let envelope = herdr.prompt_for_pane("pane-reviewer");
    execute_message_operation("reply", serde_json::json!({
        "request_id":"partial-reply", "source":"@source Found a missing case.", "author_kind":"agent_session",
        "in_reply_to":sent["message_id"], "attempt_id":envelope["control"]["attempt_id"]
    }), &reviewer).unwrap();
    let before = fs::read(directory.state.join("namedrop.sqlite3")).unwrap();
    let partial = execute_handoff_operation(
        "handoff.results",
        serde_json::json!({"message_id":sent["message_id"]}),
        &source,
    )
    .unwrap();
    assert_eq!(partial["complete"], false);
    assert_eq!(partial["recipients"][0]["result_status"], "replied");
    assert_eq!(
        partial["recipients"][0]["untrusted_reply"],
        "Found a missing case."
    );
    assert_eq!(
        partial["recipients"][1]["result_status"],
        serde_json::Value::Null
    );
    assert_eq!(
        fs::read(directory.state.join("namedrop.sqlite3")).unwrap(),
        before
    );
    assert_eq!(partial["completed"], 1);
    assert_eq!(partial["expected"], 2);
    let returned = herdr.prompt_for_pane("pane-source");
    execute_integration_operation(
        "complete",
        serde_json::json!({"attempt_id":returned["control"]["attempt_id"]}),
        &source,
    )
    .unwrap();
    let pending = herdr.prompt_for_pane("pane-researcher");
    execute_integration_operation(
        "complete",
        serde_json::json!({"attempt_id":pending["control"]["attempt_id"]}),
        &researcher,
    )
    .unwrap();
    let complete = execute_handoff_operation(
        "handoff.results",
        serde_json::json!({"message_id":sent["message_id"]}),
        &source,
    )
    .unwrap();
    assert_eq!(complete["complete"], true);
    assert_eq!(complete["recipients"][1]["result_status"], "no_reply");
    let inbox =
        execute_integration_operation("delivery.inbox", serde_json::json!({}), &source).unwrap();
    assert_eq!(
        inbox["envelopes"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["control"]["result_set"].is_object())
            .count(),
        1
    );
    assert!(execute_handoff_operation("handoff.status", serde_json::json!({}), &source).unwrap()["handoffs"].as_array().unwrap().is_empty());
}

#[test]
fn early_one_and_two_result_reads_preserve_reply_fifo_and_final_answer_instructions() {
    let directory = TestDirectory::new();
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    let researcher = runtime(&directory, &herdr, &ids, "pane-researcher");
    for (id, rt) in [
        ("address-source", &source),
        ("address-reviewer", &reviewer),
        ("address-researcher", &researcher),
    ] {
        register(id, rt);
    }
    *herdr.source_working.lock().unwrap() = true;
    let sent = execute_message_operation("send", serde_json::json!({
        "request_id":"early-polls", "source":"@reviewer @researcher Propose five ideas each; I will rank all ten.",
        "author_kind":"agent_session"
    }), &source).unwrap();
    let mut replies = Vec::new();
    for (index, (pane, rt, body)) in [
        ("pane-reviewer", &reviewer, "Ideas A, B, C, D, E."),
        ("pane-researcher", &researcher, "Ideas F, G, H, I, J."),
    ]
    .into_iter()
    .enumerate()
    {
        let envelope = herdr.prompt_for_pane(pane);
        let reply = execute_message_operation(
            "reply",
            serde_json::json!({
                "request_id":format!("early-reply-{index}"), "source":format!("@source {body}"),
                "author_kind":"agent_session", "in_reply_to":sent["message_id"],
                "attempt_id":envelope["control"]["attempt_id"]
            }),
            rt,
        )
        .unwrap();
        replies.push(reply["message_id"].clone());
        let before = fs::read(directory.state.join("namedrop.sqlite3")).unwrap();
        let prompt_count = herdr.prompts.lock().unwrap().len();
        // Repeated polling at both 1/2 and 2/2 never dispatches or consumes work.
        for _ in 0..2 {
            let result = execute_handoff_operation(
                "handoff.results",
                serde_json::json!({"message_id":sent["message_id"]}),
                &source,
            )
            .unwrap();
            assert_eq!(result["completed"], index + 1);
            assert_eq!(result["expected"], 2);
            assert_eq!(result["complete"], index == 1);
            assert_eq!(result["recipients"][index]["untrusted_reply"], body);
            assert_eq!(
                fs::read(directory.state.join("namedrop.sqlite3")).unwrap(),
                before
            );
            assert_eq!(herdr.prompts.lock().unwrap().len(), prompt_count);
        }
    }
    assert!(
        herdr
            .prompts
            .lock()
            .unwrap()
            .iter()
            .all(|(pane, _)| pane != "pane-source")
    );

    // Yielding permits FIFO delivery: both individual replies, then the join.
    *herdr.source_working.lock().unwrap() = false;
    namedrop::drain_mailboxes(&source).unwrap();
    for reply_id in replies {
        let reply = herdr.prompt_for_pane("pane-source");
        assert_eq!(reply["control"]["message_id"], reply_id);
        assert_eq!(reply["control"]["reply_mode"], "agent_thread");
        assert!(reply["control"]["result_set"].is_null());
        execute_integration_operation(
            "complete",
            serde_json::json!({"attempt_id":reply["control"]["attempt_id"]}),
            &source,
        )
        .unwrap();
    }
    let joined = herdr.prompt_for_pane("pane-source");
    assert_eq!(joined["control"]["reply_mode"], "report_to_human");
    assert_eq!(
        joined["control"]["result_set"]["source_message_id"],
        sent["message_id"]
    );
    assert_eq!(joined["control"]["result_set"]["expected"], 2);
    assert!(
        joined["control"]["result_set"]["outcomes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|outcome| outcome["status"] == "replied")
    );
    for body in ["Ideas A, B, C, D, E.", "Ideas F, G, H, I, J."] {
        assert!(joined["untrusted_body"].as_str().unwrap().contains(body));
    }
    // This asserts the delivered instruction, NOT a model-authored final answer.
    let instruction = joined["trusted_instruction"].as_str().unwrap();
    assert!(instruction.contains("bound complete.no-reply then emit a non-empty final response"));
    assert!(instruction.contains("original user deliverable"));
    assert!(
        instruction
            .contains("Earlier handoff.results reads or progress do not fulfill this obligation")
    );
    let operations = joined["control"]["tool"]["operations"].as_array().unwrap();
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0]["token"], "complete.no-reply");
    execute_integration_operation("complete", operations[0]["input"].clone(), &source).unwrap();
    namedrop::drain_mailboxes(&source).unwrap();
    let inbox =
        execute_integration_operation("delivery.inbox", serde_json::json!({}), &source).unwrap();
    assert!(inbox["envelopes"].as_array().unwrap().is_empty());
    let prompts = herdr.prompts.lock().unwrap();
    assert_eq!(
        prompts
            .iter()
            .filter(|(pane, _)| pane == "pane-source")
            .count(),
        3
    );
    assert_eq!(prompts.len(), 5);
}

#[test]
fn groups_expand_atomically_and_replay_keeps_the_original_members() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    namedrop::execute_group_operation(
        "group.set",
        serde_json::json!({"alias":"review-team", "members":["reviewer","researcher"]}),
        &source,
    )
    .unwrap();
    let input = serde_json::json!({"request_id":"group-send", "source":"@review-team @reviewer Inspect this.", "author_kind":"agent_session"});
    let first = execute_message_operation("send", input.clone(), &source).unwrap();
    assert_eq!(
        first["recipients"],
        serde_json::json!([
            {"address_id":"address-reviewer","alias":"reviewer"},
            {"address_id":"address-researcher","alias":"researcher"}
        ])
    );
    namedrop::execute_group_operation(
        "group.set",
        serde_json::json!({"alias":"review-team", "members":["researcher"]}),
        &source,
    )
    .unwrap();
    let replay = execute_message_operation("send", input, &source).unwrap();
    assert_eq!(replay["message_id"], first["message_id"]);
    assert_eq!(replay["recipients"], first["recipients"]);
    namedrop::execute_group_operation(
        "group.remove",
        serde_json::json!({"alias":"review-team"}),
        &source,
    )
    .unwrap();
    let replay = execute_message_operation("send", serde_json::json!({"request_id":"group-send", "source":"@review-team @reviewer Inspect this.", "author_kind":"agent_session"}), &source).unwrap();
    assert_eq!(replay["recipients"], first["recipients"]);
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

fn context_pin_refs(directory: &TestDirectory) -> Vec<String> {
    pin_refs(&directory.repo)
}

/// The context pin refs of one repository, so a capture can be shown to have
/// written its pin into the Space's repository and into no other.
fn pin_refs(repo: &std::path::Path) -> Vec<String> {
    git(
        repo,
        &["for-each-ref", "--format=%(refname)", "refs/herdr-context"],
    )
    .lines()
    .map(str::to_owned)
    .collect()
}

#[test]
fn preview_shows_expanded_live_recipients_and_redacted_context_without_mutation() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    register("address-source", &source);
    namedrop::execute_group_operation(
        "group.set",
        serde_json::json!({"alias":"team", "members":["reviewer","researcher"]}),
        &source,
    )
    .unwrap();
    let before = fs::read(directory.state.join("namedrop.sqlite3")).unwrap();
    let refs = git(&directory.repo, &["show-ref"]);
    let index = fs::read(directory.repo.join(".git/index")).unwrap();
    let prompt_count = herdr.prompts.lock().unwrap().len();
    let preview = execute_handoff_operation("handoff.preview", serde_json::json!({
        "source":"@team Inspect this.",
        "context":{"snapshot":{
            "objective":"Review this", "applicable_user_instructions":[], "decisions":[], "completed_work":[], "current_work":[], "blockers":[],
            "evidence":[{"kind":"diff","locator":"src/notes.txt"},{"kind":"file","locator":".env"}]
        }}
    }), &source).unwrap();
    assert_eq!(preview["recipients"][0]["alias"], "reviewer");
    assert_eq!(
        preview["recipients"][0]["agent_session_id"],
        "session-reviewer"
    );
    assert!(
        preview["recipients"][0]["constraints"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "registration_required")
    );
    assert_eq!(preview["message"], "Inspect this.");
    assert_eq!(
        preview["context"]["snapshot"]["evidence"][1]["locator"],
        "[BLOCKED PATH]"
    );
    assert_eq!(
        fs::read(directory.state.join("namedrop.sqlite3")).unwrap(),
        before
    );
    assert_eq!(git(&directory.repo, &["show-ref"]), refs);
    assert_eq!(fs::read(directory.repo.join(".git/index")).unwrap(), index);
    assert_eq!(herdr.prompts.lock().unwrap().len(), prompt_count);
}

#[test]
fn returned_reviews_report_fresh_stale_and_unknown_against_the_captured_files() {
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
    fs::write(
        directory.repo.join("src/new.txt"),
        "new untracked evidence\n",
    )
    .unwrap();
    let sent = execute_message_operation("send",serde_json::json!({
        "request_id":"freshness", "source":"@reviewer Review these changes.","author_kind":"agent_session",
        "context":{"snapshot":{"objective":"Review changes","applicable_user_instructions":[],"decisions":[],"completed_work":[],"current_work":[],"blockers":[],
        "evidence":[{"kind":"diff","locator":"src/notes.txt"},{"kind":"diff","locator":"src/new.txt"}]}}
    }),&source).unwrap();
    let delivered = herdr.prompt_for_pane("pane-reviewer");
    execute_message_operation("reply",serde_json::json!({"request_id":"freshness-reply","source":"@source Review complete.","author_kind":"agent_session","in_reply_to":sent["message_id"],"attempt_id":delivered["control"]["attempt_id"]}),&reviewer).unwrap();
    let input = serde_json::json!({"message_id":sent["message_id"]});
    let result = execute_handoff_operation("handoff.results", input.clone(), &source).unwrap();
    assert_eq!(result["recipients"][0]["freshness"]["status"], "fresh");
    assert!(result["recipients"][0]["reviewed_git_state"]["pinned_commit"].is_string());
    fs::write(directory.repo.join("unrelated.txt"), "not reviewed").unwrap();
    let result = execute_handoff_operation("handoff.results", input.clone(), &source).unwrap();
    assert_eq!(result["recipients"][0]["freshness"]["status"], "fresh");
    fs::write(directory.repo.join("src/new.txt"), "changed after review").unwrap();
    let result = execute_handoff_operation("handoff.results", input.clone(), &source).unwrap();
    assert_eq!(result["recipients"][0]["freshness"]["status"], "stale");
    assert_eq!(
        result["recipients"][0]["freshness"]["changed_paths"],
        serde_json::json!(["src/new.txt"])
    );
    fs::write(
        directory.repo.join("src/new.txt"),
        vec![b'x'; 1024 * 1024 + 1],
    )
    .unwrap();
    let result = execute_handoff_operation("handoff.results", input, &source).unwrap();
    assert_eq!(result["recipients"][0]["freshness"]["status"], "unknown");
    let returned = herdr.prompt_for_pane("pane-source");
    assert!(
        returned["control"]["tool"]["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|op| op["token"] == "handoff.results"
                && op["input"]["message_id"] == sent["message_id"])
    );
    assert_eq!(context_pin_refs(&directory).len(), 1);
}

#[test]
fn invalid_groups_are_atomic_aliases_are_exclusive_and_members_follow_renames() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let set = |alias: &str, members: serde_json::Value| {
        namedrop::execute_group_operation(
            "group.set",
            serde_json::json!({"alias":alias,"members":members}),
            &source,
        )
    };
    set("team", serde_json::json!(["reviewer", "researcher"])).unwrap();
    for (alias, members) in [
        ("reviewer", serde_json::json!(["researcher"])),
        ("team", serde_json::json!(["reviewer", "missing"])),
        ("team", serde_json::json!(["team"])),
        ("team", serde_json::json!([])),
    ] {
        assert!(set(alias, members).is_err());
    }
    let groups =
        namedrop::execute_group_operation("group.list", serde_json::json!({}), &source).unwrap();
    assert_eq!(groups["groups"][0]["members"].as_array().unwrap().len(), 2);
    assert!(
        namedrop::execute_lifecycle_operation(
            "address.rename",
            serde_json::json!({"address_id":"address-reviewer","alias":"team"}),
            &source
        )
        .is_err()
    );
    namedrop::execute_lifecycle_operation(
        "address.rename",
        serde_json::json!({"address_id":"address-reviewer","alias":"reviewer-new"}),
        &source,
    )
    .unwrap();
    let sent = execute_message_operation(
        "send",
        serde_json::json!({"request_id":"renamed-member","source":"@team Inspect this."}),
        &source,
    )
    .unwrap();
    assert_eq!(sent["recipients"][0]["alias"], "reviewer-new");
    let other = LifecycleRuntime {
        session_socket_id: "other-socket",
        ..source
    };
    assert_eq!(
        namedrop::execute_group_operation("group.list", serde_json::json!({}), &other).unwrap()["groups"],
        serde_json::json!([])
    );
}

#[test]
fn a_schema_one_upgrade_preserves_existing_requests_and_replays() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let input =
        serde_json::json!({"request_id":"before-upgrade","source":"@reviewer Keep this request."});
    let before = execute_message_operation("send", input.clone(), &source).unwrap();
    // Remove the additive v2/v3/v4 schema to reproduce an installed v1 database.
    let db = rusqlite::Connection::open(directory.state.join("namedrop.sqlite3")).unwrap();
    db.execute_batch("DROP INDEX transition_facts_by_entity; DROP VIEW delivery_seats; DROP TABLE bridged_threads; DROP TABLE bridged_messages; DROP TABLE space_envoys; DROP TABLE coordination_links; DROP TABLE coordination_assignments; DROP TABLE coordinations; DROP TRIGGER group_alias_conflict; DROP TRIGGER agent_alias_group_conflict; DROP TABLE mention_group_members; DROP TABLE mention_groups; DROP TABLE terminal_executions; ALTER TABLE pane_addresses DROP COLUMN pane_type; ALTER TABLE pane_addresses DROP COLUMN remote_commands_terminal_id; ALTER TABLE pane_addresses DROP COLUMN persona; PRAGMA user_version=1;").unwrap();
    drop(db);
    namedrop::execute_group_operation(
        "group.set",
        serde_json::json!({"alias":"team","members":["reviewer"]}),
        &source,
    )
    .unwrap();
    let replay = execute_message_operation("send", input, &source).unwrap();
    assert_eq!(replay["message_id"], before["message_id"]);
    assert_eq!(replay["deliveries"], before["deliveries"]);
    assert_eq!(
        StateStore::open_read_only(&directory.state)
            .unwrap()
            .unwrap()
            .schema_version()
            .unwrap(),
        7
    );
}

#[test]
fn held_work_needs_attention_and_reads_cannot_recover_or_cancel_it() {
    let directory = TestDirectory::new();
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    let sent=execute_message_operation("send",serde_json::json!({"request_id":"held","source":"@reviewer Inspect this.","author_kind":"agent_session"}),&source).unwrap();
    let delivered = herdr.prompt_for_pane("pane-reviewer");
    let attempt =
        namedrop::DeliveryAttemptId::new(delivered["control"]["attempt_id"].as_str().unwrap());
    StateStore::open(&directory.state)
        .unwrap()
        .transaction(|tx| {
            tx.transition_delivery_attempt(
                &StateNamespace::for_space("socket-a", "workspace-1"),
                &attempt,
                namedrop::DeliveryState::Held,
                &MutationAttribution::new(ActorKind::Plugin, "test", 43),
            )
        })
        .unwrap();
    let before = fs::read(directory.state.join("namedrop.sqlite3")).unwrap();
    let result = execute_handoff_operation(
        "handoff.results",
        serde_json::json!({"message_id":sent["message_id"]}),
        &source,
    )
    .unwrap();
    assert_eq!(result["recipients"][0]["needs_attention"], true);
    assert_eq!(result["complete"], false);
    assert_eq!(
        fs::read(directory.state.join("namedrop.sqlite3")).unwrap(),
        before
    );
    namedrop::execute_control_operation(
        "delivery.cancel-held",
        serde_json::json!({"attempt_id":attempt,"confirmed":true}),
        &source,
    )
    .unwrap();
    let result = execute_handoff_operation(
        "handoff.results",
        serde_json::json!({"message_id":sent["message_id"]}),
        &source,
    )
    .unwrap();
    assert_eq!(result["recipients"][0]["result_status"], "cancelled");
    assert_eq!(result["complete"], true);
}

#[test]
fn stale_sessions_cannot_read_results_or_preview_and_status_pages_are_complete() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    register("address-source", &source);
    for id in ["first", "second"] {
        execute_message_operation("send",serde_json::json!({"request_id":id,"source":"@reviewer Inspect this.","author_kind":"agent_session"}),&source).unwrap();
    }
    let first =
        execute_handoff_operation("handoff.status", serde_json::json!({"limit":1}), &source)
            .unwrap();
    let second = execute_handoff_operation(
        "handoff.status",
        serde_json::json!({"limit":1,"offset":first["next_offset"]}),
        &source,
    )
    .unwrap();
    assert_eq!(first["has_more"], true);
    assert_eq!(second["has_more"], false);
    assert_ne!(
        first["handoffs"][0]["message_id"],
        second["handoffs"][0]["message_id"]
    );
    *herdr.live_pane_override.lock().unwrap() = Some((
        "pane-source".to_owned(),
        Some("replacement-session".to_owned()),
    ));
    for (op, input) in [
        ("handoff.status", serde_json::json!({})),
        (
            "handoff.results",
            serde_json::json!({"message_id":first["handoffs"][0]["message_id"]}),
        ),
        (
            "handoff.preview",
            serde_json::json!({"source":"@reviewer Inspect this."}),
        ),
    ] {
        assert!(
            execute_handoff_operation(op, input, &source)
                .unwrap_err()
                .contains("live native Agent Session")
        );
    }
}

#[test]
fn direct_group_preview_matches_the_verbatim_native_handoff() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    register("address-source", &source);
    namedrop::execute_group_operation(
        "group.set",
        serde_json::json!({"alias":"team","members":["reviewer","researcher"]}),
        &source,
    )
    .unwrap();
    let prompt = "@team Keep @missing inside the original prompt.";
    let preview = execute_handoff_operation(
        "handoff.preview",
        serde_json::json!({"source":prompt,"mode":"direct_prompt"}),
        &source,
    )
    .unwrap();
    assert_eq!(preview["message"], prompt);
    assert_eq!(preview["recipients"].as_array().unwrap().len(), 2);
    assert_eq!(preview["context_references"], serde_json::json!([]));
    let hook = namedrop::execute_user_prompt_submit_hook(
        &serde_json::json!({"hook_event_name":"UserPromptSubmit","prompt":prompt}).to_string(),
        &source,
    )
    .unwrap();
    assert_eq!(hook["decision"], "block");
    let sent = execute_handoff_operation("handoff.status", serde_json::json!({}), &source).unwrap();
    assert_eq!(sent["handoffs"][0]["request"], preview["message"]);
    assert_eq!(
        sent["handoffs"][0]["recipients"].as_array().unwrap().len(),
        preview["recipients"].as_array().unwrap().len()
    );

    assert!(
        execute_handoff_operation(
            "handoff.preview",
            serde_json::json!({"source":prompt}),
            &source
        )
        .is_err()
    );
}

#[test]
fn freshness_handles_directories_and_protected_paths_without_running_custom_filters() {
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
    let sent=execute_message_operation("send",serde_json::json!({"request_id":"directory-review","source":"@reviewer Review source files.","author_kind":"agent_session",
      "context":{"snapshot":{"objective":"Review source","applicable_user_instructions":[],"decisions":[],"completed_work":[],"current_work":[],"blockers":[],"evidence":[{"kind":"diff","locator":"src"}]}}}),&source).unwrap();
    let delivered = herdr.prompt_for_pane("pane-reviewer");
    execute_message_operation("reply",serde_json::json!({"request_id":"directory-reply","source":"@source Done.","author_kind":"agent_session","in_reply_to":sent["message_id"],"attempt_id":delivered["control"]["attempt_id"]}),&reviewer).unwrap();
    let input = serde_json::json!({"message_id":sent["message_id"]});
    assert_eq!(
        execute_handoff_operation("handoff.results", input.clone(), &source).unwrap()["recipients"]
            [0]["freshness"]["status"],
        "fresh"
    );
    fs::write(directory.repo.join("src/.env"), "SECRET=not-disclosed").unwrap();
    let result = execute_handoff_operation("handoff.results", input.clone(), &source).unwrap();
    assert_eq!(result["recipients"][0]["freshness"]["status"], "fresh");
    assert!(!result.to_string().contains("not-disclosed"));
    fs::write(directory.repo.join("src/added.rs"), "fn new_code() {}\n").unwrap();
    let result = execute_handoff_operation("handoff.results", input.clone(), &source).unwrap();
    assert_eq!(
        result["recipients"][0]["freshness"]["changed_paths"],
        serde_json::json!(["src/added.rs"])
    );
    fs::remove_file(directory.repo.join("src/added.rs")).unwrap();
    fs::write(
        directory.repo.join(".gitattributes"),
        "src/notes.txt filter=side-effect\n",
    )
    .unwrap();
    git(
        &directory.repo,
        &[
            "config",
            "filter.side-effect.clean",
            "touch filter-ran; cat",
        ],
    );
    let result = execute_handoff_operation("handoff.results", input, &source).unwrap();
    assert_eq!(result["recipients"][0]["freshness"]["status"], "unknown");
    assert!(!directory.repo.join("filter-ran").exists());
}

#[test]
fn freshness_does_not_run_git_filesystem_monitor_hooks() {
    use std::os::unix::fs::PermissionsExt;
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    let input = returned_path_review(&source, &reviewer, &herdr, "src");
    let hook = directory.repo.join(".git/fsmonitor-test");
    fs::write(&hook, "#!/bin/sh\ntouch fsmonitor-ran\nprintf 'token\\0'\n").unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o755)).unwrap();
    git(
        &directory.repo,
        &["config", "core.fsmonitor", hook.to_str().unwrap()],
    );
    let result = execute_handoff_operation("handoff.results", input, &source).unwrap();
    assert!(!directory.repo.join("fsmonitor-ran").exists());
    assert_eq!(result["recipients"][0]["freshness"]["status"], "fresh");
}

#[test]
fn freshness_keeps_unknown_evidence_when_a_reviewed_descendant_becomes_protected() {
    let directory = TestDirectory::new();
    fs::write(
        directory.repo.join("src/sibling.txt"),
        "unchanged sibling\n",
    )
    .unwrap();
    init_git_repo(&directory);
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    let input = returned_path_review(&source, &reviewer, &herdr, "src");
    fs::remove_file(directory.repo.join("src/notes.txt")).unwrap();
    std::os::unix::fs::symlink(
        directory.root.join("outside.txt"),
        directory.repo.join("src/notes.txt"),
    )
    .unwrap();
    let result = execute_handoff_operation("handoff.results", input, &source).unwrap();
    assert_eq!(result["recipients"][0]["freshness"]["status"], "unknown");
    assert!(!result.to_string().contains("outside evidence"));
    assert!(
        !result
            .to_string()
            .contains(directory.root.join("outside.txt").to_str().unwrap())
    );
}

#[test]
fn freshness_normalizes_valid_relative_evidence_paths() {
    for path in ["./src/notes.txt", "src//./notes.txt", "./src/", "./"] {
        let directory = TestDirectory::new();
        init_git_repo(&directory);
        setup(&directory);
        complete_reconciliation(&directory);
        let herdr = FakeHerdr::new(&directory);
        let ids = SequenceIds(AtomicU64::new(1));
        let source = runtime(&directory, &herdr, &ids, "pane-source");
        let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
        let input = returned_path_review(&source, &reviewer, &herdr, path);
        let result = execute_handoff_operation("handoff.results", input.clone(), &source).unwrap();
        assert_eq!(
            result["recipients"][0]["freshness"]["status"], "fresh",
            "{path}"
        );
        fs::write(directory.repo.join("src/notes.txt"), "changed\n").unwrap();
        let result = execute_handoff_operation("handoff.results", input, &source).unwrap();
        assert_eq!(
            result["recipients"][0]["freshness"]["status"], "stale",
            "{path}"
        );
        assert!(
            result["recipients"][0]["freshness"]["changed_paths"]
                .as_array()
                .unwrap()
                .contains(&serde_json::json!("src/notes.txt"))
        );
    }
}

#[test]
fn freshness_cannot_certify_ignored_evidence_that_was_never_pinned() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    fs::write(directory.repo.join("ignored.log"), "ignored evidence\n").unwrap();
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    let input = returned_path_review(&source, &reviewer, &herdr, "ignored.log");
    let result = execute_handoff_operation("handoff.results", input.clone(), &source).unwrap();
    assert_eq!(result["recipients"][0]["freshness"]["status"], "unknown");
    fs::remove_file(directory.repo.join("ignored.log")).unwrap();
    let result = execute_handoff_operation("handoff.results", input, &source).unwrap();
    assert_eq!(result["recipients"][0]["freshness"]["status"], "unknown");
}

/// Text that addresses nobody stays literal, so the hook must add nothing at
/// all. Reporting a failed handoff there tells the agent collaboration may be
/// unavailable and invites the person to retry something that was never
/// attempted — a decorator, an annotation, a scoped package name or a bare
/// at-sign is not a handoff. The boundary is "carries a Mention", not "parses
/// as a message": a Mention with no body does address someone, so its failure
/// stays reported.
#[test]
fn text_that_addresses_nobody_is_left_untouched_by_the_hook() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    register("address-source", &source);
    let hook = |prompt: &str| {
        namedrop::execute_user_prompt_submit_hook(
            &serde_json::json!({"hook_event_name":"UserPromptSubmit","prompt":prompt}).to_string(),
            &source,
        )
    };
    for prompt in [
        "@ is just a character here",
        "@",
        "@@reviewer double at-sign",
        "@types/node needs updating",
        "@-dash-leads hello",
        " @reviewer indented past column zero",
    ] {
        // The prompt is not rerouted: no block decision and no handoff
        // guidance. An ordinary turn may still carry the persona roster, which
        // is context about the space rather than anything done to the prompt.
        let output = hook(prompt);
        assert!(
            output.as_ref().is_none_or(|output| {
                output.get("decision").is_none()
                    && output
                        .pointer("/hookSpecificOutput/additionalContext")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|context| context.starts_with("Namedrop workspace roster."))
            }),
            "{prompt:?} addresses nobody, so the hook must not route it: {output:?}"
        );
    }
    // A Mention with no body addresses someone, so the send is still attempted
    // and its failure still reported. Silencing it would hide a real handoff
    // failure, so this case is deliberately outside the guard above.
    assert!(
        hook("@reviewer").is_some(),
        "a Mention with no body still reports its handoff failure"
    );
    // An alias-shaped Mention that does not resolve is indistinguishable from a
    // mistyped name, so it still earns the "check the name" guidance. That is
    // the documented miss behavior, and it shows the silence above is a grammar
    // decision rather than a disabled hook.
    for prompt in ["@missing please look", "@property is a decorator"] {
        assert_eq!(
            hook(prompt)
                .unwrap()
                .pointer("/hookSpecificOutput/additionalContext")
                .and_then(serde_json::Value::as_str)
                .map(|context| {
                    context.contains("could not resolve at least one leading recipient")
                }),
            Some(true),
            "{prompt:?}"
        );
    }
    assert_eq!(
        hook("@reviewer please look").unwrap()["decision"],
        "block",
        "a resolvable leading Mention is still routed"
    );
}

/// A returned review describes the Space Namedrop is activated for, so its
/// freshness must be judged against that Space's Worktree — never against
/// whatever directory the calling process happens to sit in. A sibling checkout
/// of the same repository shares the object database, so the immutable pin
/// resolves there and nothing fails: a directory-based comparison therefore
/// answers a confident `fresh` about a tree the Space does not have, which is
/// exactly the false-fresh the review-freshness contract forbids.
#[test]
fn freshness_judges_the_spaces_worktree_not_the_calling_directory() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    let input = returned_path_review(&source, &reviewer, &herdr, "src/notes.txt");
    // The Space's own Worktree changes a reviewed path.
    fs::write(
        directory.repo.join("src/notes.txt"),
        "changed after review\n",
    )
    .unwrap();
    let space_result =
        execute_handoff_operation("handoff.results", input.clone(), &source).unwrap();
    assert_eq!(
        space_result["recipients"][0]["freshness"]["status"],
        "stale"
    );
    // A sibling checkout of the same repository still matches the pin.
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
    let elsewhere = LifecycleRuntime {
        cwd: &sibling,
        workspace_id: Some("workspace-1"),
        ..runtime(&directory, &herdr, &ids, "pane-source")
    };
    let elsewhere_result = execute_handoff_operation("handoff.results", input, &elsewhere).unwrap();
    assert_eq!(
        elsewhere_result["recipients"][0]["freshness"]["status"], "stale",
        "freshness must describe the Space's Worktree: {}",
        elsewhere_result["recipients"][0]["freshness"]
    );
}

#[test]
fn legacy_reviews_follow_the_space_and_bound_reviews_keep_their_checkout() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    let input = returned_path_review(&source, &reviewer, &herdr, "src/notes.txt");
    assert_eq!(
        execute_handoff_operation("handoff.results", input.clone(), &source).unwrap()["recipients"]
            [0]["freshness"]["status"],
        "fresh",
        "the attached-Worktree control must pass first"
    );
    *herdr.no_worktree.lock().unwrap() = true;
    let detached = LifecycleRuntime {
        workspace_id: Some("workspace-1"),
        ..runtime(&directory, &herdr, &ids, "pane-source")
    };
    let freshness = |runtime| {
        execute_handoff_operation("handoff.results", input.clone(), runtime).unwrap()["recipients"]
            [0]["freshness"]
            .clone()
    };
    assert_eq!(freshness(&detached)["status"], "fresh");
    rewrite_stored_snapshots(
        &directory,
        "json_remove(payload_json, '$.checkout_binding')",
    );
    assert_eq!(
        freshness(&detached),
        serde_json::json!({"status":"unknown","reason":"no_attached_worktree"}),
        "a Space with no Worktree must not claim a comparison"
    );
    *herdr.no_worktree.lock().unwrap() = false;
    assert_eq!(freshness(&detached)["status"], "fresh");
    rewrite_stored_snapshots(
        &directory,
        "json_set(payload_json, '$.checkout_binding', json('{\"version\":2}'))",
    );
    assert_eq!(
        freshness(&detached),
        serde_json::json!({
            "status":"unknown",
            "reason":"checkout_unavailable",
            "checkout_reason":"unsupported_binding_version"
        })
    );
}

/// Capture and interpretation must name the same tree. Sibling worktrees share
/// one object database, so a pin taken from the caller's directory stays
/// reachable from the Space and becomes a confident verdict about a tree the
/// Space does not have. The Space's own Worktree is the baseline whatever
/// directory the send is issued from, and the pin ref belongs to the Space's
/// repository and to no other.
#[test]
fn context_capture_pins_the_spaces_worktree_not_the_calling_directory() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    // A sibling checkout of the same repository, with a reviewed file changed
    // only there.
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
    let input = returned_path_review(&source, &reviewer, &herdr, "src/notes.txt");
    let freshness =
        execute_handoff_operation("handoff.results", input.clone(), &source).unwrap()["recipients"]
            [0]["freshness"]
            .clone();
    assert_eq!(
        freshness["status"], "fresh",
        "capture must establish the Space's baseline, not the caller's: {freshness}"
    );
    // Only the Space's own Worktree can make the Space's own review stale.
    fs::write(
        directory.repo.join("src/notes.txt"),
        "the Space changed after review\n",
    )
    .unwrap();
    assert_eq!(
        execute_handoff_operation("handoff.results", input, &source).unwrap()["recipients"][0]["freshness"]
            ["status"],
        "stale"
    );

    // An unrelated repository as the caller's directory: the pin is still
    // written into the Space's repository, and never into the caller's.
    let unrelated = directory.root.join("unrelated-repo");
    fs::create_dir_all(unrelated.join("src")).unwrap();
    fs::write(unrelated.join("src/notes.txt"), "unrelated repository\n").unwrap();
    git(&unrelated, &["init", "-q", "-b", "main"]);
    git(&unrelated, &["config", "user.name", "Context Test"]);
    git(&unrelated, &["config", "user.email", "context@test"]);
    git(&unrelated, &["add", "."]);
    git(&unrelated, &["commit", "-q", "-m", "unrelated"]);
    let elsewhere = LifecycleRuntime {
        cwd: &unrelated,
        workspace_id: Some("workspace-1"),
        ..runtime(&directory, &herdr, &ids, "pane-source")
    };
    let pins_before = pin_refs(&directory.repo).len();
    execute_message_operation(
        "send",
        path_review_input("unrelated-review", "src/notes.txt"),
        &elsewhere,
    )
    .unwrap();
    assert_eq!(
        pin_refs(&directory.repo).len(),
        pins_before + 1,
        "the pin belongs to the Space's repository"
    );
    assert!(
        pin_refs(&unrelated).is_empty(),
        "no pin ref may be written into the caller's repository"
    );
}

/// File evidence and Git capture require the Space's Worktree independently:
/// disabling Git capture does not permit fallback access to caller files.
#[test]
fn a_space_without_a_worktree_refuses_to_capture_context() {
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
        let mut input = path_review_input("detached-review", "src/notes.txt");
        input["context"]["capture_git_state"] = serde_json::json!(capture);
        input["context"]["snapshot"]["evidence"] = evidence;
        let refused = execute_message_operation("send", input.clone(), &detached).unwrap_err();
        assert!(refused.contains("no attached Git Worktree"), "{refused}");
        let previewed = execute_handoff_operation(
            "handoff.preview",
            serde_json::json!({"source": input["source"], "context": input["context"]}),
            &detached,
        )
        .unwrap_err();
        assert!(
            previewed.contains("no attached Git Worktree"),
            "{previewed}"
        );
    }
    let worker = add_checkout(&directory, "worker");
    let mut input = path_review_input("detached-checkout", "src/notes.txt");
    input["context"]["capture_git_state"] = serde_json::json!(false);
    input["context"]["snapshot"]["evidence"] = serde_json::json!([]);
    input["context"]["checkout"] = serde_json::json!({"path": worker});
    let refused = execute_message_operation("send", input, &detached).unwrap_err();
    assert!(refused.contains("no attached Git Worktree"), "{refused}");
    assert!(
        context_pin_refs(&directory).is_empty(),
        "a refused capture must write no pin ref"
    );
    // The same send succeeds once the Space's Worktree is attached again.
    *herdr.no_worktree.lock().unwrap() = false;
    execute_message_operation(
        "send",
        path_review_input("attached-review", "src/notes.txt"),
        &detached,
    )
    .unwrap();
    assert_eq!(context_pin_refs(&directory).len(), 1);
}

#[test]
fn root_independent_context_can_be_previewed_sent_and_replied_without_a_worktree() {
    let directory = TestDirectory::new();
    setup(&directory);
    complete_reconciliation(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    let reviewer = runtime(&directory, &herdr, &ids, "pane-reviewer");
    register("address-source", &source);
    register("address-reviewer", &reviewer);
    *herdr.no_worktree.lock().unwrap() = true;
    let detached = LifecycleRuntime {
        workspace_id: Some("workspace-1"),
        ..source
    };
    let reviewer = LifecycleRuntime {
        workspace_id: Some("workspace-1"),
        ..reviewer
    };
    for (request, evidence) in [
        ("plain-context", serde_json::json!([])),
        (
            "pane-context",
            serde_json::json!([{"kind":"pane_output", "locator":"current"}]),
        ),
    ] {
        let mut input = path_review_input(request, "unused");
        input["context"]["capture_git_state"] = serde_json::json!(false);
        input["context"]["snapshot"]["objective"] = serde_json::json!("Review our decisions");
        input["context"]["snapshot"]["evidence"] = evidence;
        let preview = execute_handoff_operation(
            "handoff.preview",
            serde_json::json!({"source":input["source"], "context":input["context"]}),
            &detached,
        )
        .unwrap();
        assert_eq!(
            preview["context"]["snapshot"]["objective"],
            "Review our decisions"
        );
        assert!(preview["context"]["snapshot"]["git_state"].is_null());
        let sent = execute_message_operation("send", input.clone(), &detached).unwrap();
        assert_eq!(sent["context_grants"][0]["state"], "active");
        let envelope = herdr.prompt_for_pane("pane-reviewer");
        let replied = execute_message_operation(
            "reply",
            serde_json::json!({
                "request_id":format!("reply-{request}"),
                "source":"@source Here are my decisions.",
                "in_reply_to":sent["message_id"],
                "author_kind":"agent_session",
                "attempt_id":envelope["control"]["attempt_id"],
                "context":input["context"]
            }),
            &reviewer,
        )
        .unwrap();
        assert_eq!(replied["context_grants"][0]["state"], "active");
        assert_eq!(
            replied["context_grants"][0]["grantee_address_id"],
            "address-source"
        );
    }
    assert!(!directory.repo.join(".git").exists());
}

/// One agent-authored send carrying a `diff` context block for one path.
fn path_review_input(request_id: &str, path: &str) -> serde_json::Value {
    serde_json::json!({
        "request_id": request_id,
        "source": "@reviewer Review this path.",
        "author_kind": "agent_session",
        "context": {"snapshot": {
            "objective": "Review path",
            "applicable_user_instructions": [],
            "decisions": [],
            "completed_work": [],
            "current_work": [],
            "blockers": [],
            "evidence": [{"kind": "diff", "locator": path}]
        }}
    })
}

fn returned_path_review(
    source: &LifecycleRuntime<'_, namedrop::TuiHost<&'_ FakeHerdr>, FixedClock, SequenceIds>,
    reviewer: &LifecycleRuntime<'_, namedrop::TuiHost<&'_ FakeHerdr>, FixedClock, SequenceIds>,
    herdr: &FakeHerdr,
    path: &str,
) -> serde_json::Value {
    register("address-source", source);
    register("address-reviewer", reviewer);
    returned_review(
        source,
        reviewer,
        herdr,
        path_review_input("path-review", path),
    )
}

fn returned_review(
    source: &LifecycleRuntime<'_, namedrop::TuiHost<&'_ FakeHerdr>, FixedClock, SequenceIds>,
    reviewer: &LifecycleRuntime<'_, namedrop::TuiHost<&'_ FakeHerdr>, FixedClock, SequenceIds>,
    herdr: &FakeHerdr,
    input: serde_json::Value,
) -> serde_json::Value {
    let reply_id = format!("{}-reply", input["request_id"].as_str().unwrap());
    let sent = execute_message_operation("send", input, source).unwrap();
    let delivered = herdr.prompt_for_pane("pane-reviewer");
    execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id":reply_id, "source":"@source Done.", "author_kind":"agent_session",
            "in_reply_to":sent["message_id"], "attempt_id":delivered["control"]["attempt_id"]
        }),
        reviewer,
    )
    .unwrap();
    serde_json::json!({"message_id":sent["message_id"]})
}

fn rewrite_stored_snapshots(directory: &TestDirectory, payload_expression: &str) {
    rusqlite::Connection::open(directory.state.join("namedrop.sqlite3"))
        .unwrap()
        .execute_batch(&format!(
            "CREATE TEMP TABLE rewritten AS SELECT * FROM working_context_snapshots;
             DELETE FROM working_context_snapshots;
             INSERT INTO working_context_snapshots
                 SELECT session_socket_id, worktree_id, snapshot_id, grant_id, agent_session_id,
                        {payload_expression}, created_at_ms
                 FROM rewritten;
             DROP TABLE rewritten;"
        ))
        .unwrap();
}

fn add_checkout(directory: &TestDirectory, name: &str) -> std::path::PathBuf {
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

fn checkout_summary(checkout: &std::path::Path) -> serde_json::Value {
    serde_json::json!({
        "state": "bound",
        "selection": "explicit",
        "root": checkout.canonicalize().unwrap(),
        "label": checkout.file_name().unwrap().to_str().unwrap(),
        "layout": "linked",
    })
}

#[test]
fn reviews_of_an_explicit_checkout_are_judged_in_that_checkout() {
    use std::os::unix::fs::PermissionsExt;
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
    let worker = add_checkout(&directory, "worker");
    fs::write(worker.join("src/notes.txt"), "the worker's line\n").unwrap();
    fs::write(worker.join("src/other.txt"), "second reviewed path\n").unwrap();
    let mut input = path_review_input("worker-review", "src/notes.txt");
    input["context"]["checkout"] = serde_json::json!({"path": worker});
    input["context"]["snapshot"]["evidence"] = serde_json::json!([
        {"kind": "diff", "locator": "src/notes.txt"},
        {"kind": "diff", "locator": "src/other.txt"}
    ]);
    let results = returned_review(&source, &reviewer, &herdr, input.clone());
    let recipient = || {
        execute_handoff_operation("handoff.results", results.clone(), &source).unwrap()
            ["recipients"][0]
            .clone()
    };
    let reviewed = recipient();
    assert_eq!(reviewed["reviewed_checkout"], checkout_summary(&worker));
    assert_eq!(reviewed["reviewed_git_state"]["branch"], "worker");
    assert_eq!(reviewed["freshness"]["status"], "fresh", "{reviewed}");

    fs::write(directory.repo.join("src/notes.txt"), "the Space changed\n").unwrap();
    fs::write(worker.join("unrelated.txt"), "not reviewed\n").unwrap();
    assert_eq!(recipient()["freshness"]["status"], "fresh");

    let notes = worker.join("src/notes.txt");
    fs::set_permissions(&notes, fs::Permissions::from_mode(0o755)).unwrap();
    let freshness = recipient()["freshness"].clone();
    assert_eq!(freshness["status"], "stale");
    assert_eq!(
        freshness["changed_paths"],
        serde_json::json!(["src/notes.txt"])
    );
    fs::set_permissions(&notes, fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(recipient()["freshness"]["status"], "fresh");

    fs::write(&notes, "changed after review\n").unwrap();
    fs::write(worker.join("src/other.txt"), vec![b'x'; 1024 * 1024 + 1]).unwrap();
    let freshness = recipient()["freshness"].clone();
    assert_eq!(freshness["status"], "stale", "{freshness}");
    assert_eq!(freshness["unavailable_paths"][0]["path"], "src/other.txt");
    fs::write(&notes, "the worker's line\n").unwrap();
    assert_eq!(recipient()["freshness"]["status"], "unknown");

    input["request_id"] = serde_json::json!("uncaptured-review");
    input["context"]["capture_git_state"] = serde_json::json!(false);
    let results = returned_review(&source, &reviewer, &herdr, input);
    let uncaptured =
        execute_handoff_operation("handoff.results", results, &source).unwrap()["recipients"][0]
            .clone();
    assert_eq!(uncaptured["reviewed_checkout"], checkout_summary(&worker));
    assert!(uncaptured["reviewed_git_state"].is_null());
    assert_eq!(
        uncaptured["freshness"],
        serde_json::json!({"status":"unknown","reason":"no_captured_git_state"})
    );
}

#[test]
fn a_review_whose_checkout_is_gone_or_replaced_is_unknown() {
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
    let worker = add_checkout(&directory, "worker");
    let mut input = path_review_input("worker-review", "src/notes.txt");
    input["context"]["checkout"] = serde_json::json!({"path": worker});
    let results = returned_review(&source, &reviewer, &herdr, input);
    let recipient = || {
        execute_handoff_operation("handoff.results", results.clone(), &source).unwrap()
            ["recipients"][0]
            .clone()
    };
    assert_eq!(recipient()["freshness"]["status"], "fresh");
    git(
        &directory.repo,
        &["worktree", "remove", "--force", worker.to_str().unwrap()],
    );
    for reason in ["root_missing", "root_replaced"] {
        let reviewed = recipient();
        assert_eq!(
            reviewed["freshness"],
            serde_json::json!({"status":"unknown","reason":"checkout_unavailable","checkout_reason":reason})
        );
        assert_eq!(reviewed["reviewed_checkout"]["state"], "unavailable");
        assert_eq!(reviewed["reviewed_checkout"]["reason"], reason);
        if reason == "root_missing" {
            git(
                &directory.repo,
                &["worktree", "add", "-q", worker.to_str().unwrap(), "worker"],
            );
        }
    }
}

#[test]
fn previewing_an_explicit_checkout_shows_it_without_capturing_or_writing() {
    let directory = TestDirectory::new();
    init_git_repo(&directory);
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    register("address-source", &source);
    let worker = add_checkout(&directory, "worker");
    fs::write(worker.join("src/notes.txt"), "uncommitted worker work\n").unwrap();
    let plain = directory.root.join("plain");
    fs::create_dir(&plain).unwrap();
    let database = fs::read(directory.state.join("namedrop.sqlite3")).unwrap();
    let refs = git(&directory.repo, &["show-ref"]);
    let worker_index = git(&worker, &["rev-parse", "--git-path", "index"]);
    let worker_index = worker.join(worker_index.trim());
    let indexes = (
        fs::read(directory.repo.join(".git/index")).unwrap(),
        fs::read(&worker_index).unwrap(),
    );
    let prompts = herdr.prompts.lock().unwrap().len();
    let mut input = path_review_input("preview", "src/notes.txt");
    input["context"]["checkout"] = serde_json::json!({"path": worker});
    let preview = execute_handoff_operation(
        "handoff.preview",
        serde_json::json!({"source": input["source"], "context": input["context"]}),
        &source,
    )
    .unwrap();
    assert_eq!(
        preview["context"]["snapshot"]["checkout"],
        checkout_summary(&worker)
    );
    assert!(preview["context"]["snapshot"]["git_state"].is_null());
    assert_eq!(preview["context"]["git_scope"], "whole_worktree");
    input["context"]["checkout"] = serde_json::json!({"path": plain});
    let refused = execute_handoff_operation(
        "handoff.preview",
        serde_json::json!({"source": input["source"], "context": input["context"]}),
        &source,
    )
    .unwrap_err();
    assert!(refused.contains("is not a Git worktree"), "{refused}");
    assert_eq!(
        fs::read(directory.state.join("namedrop.sqlite3")).unwrap(),
        database
    );
    assert_eq!(git(&directory.repo, &["show-ref"]), refs);
    assert_eq!(
        (
            fs::read(directory.repo.join(".git/index")).unwrap(),
            fs::read(&worker_index).unwrap()
        ),
        indexes
    );
    assert_eq!(herdr.prompts.lock().unwrap().len(), prompts);
}

#[test]
fn cli_exposes_all_features_and_keeps_management_out_of_delivery_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let source = runtime(&directory, &herdr, &ids, "pane-source");
    register("address-source", &source);
    let mock = directory.root.join("herdr");
    let pane_output = herdr
        .execute(&["pane", "list", "--workspace", "workspace-1"])
        .unwrap()
        .stdout;
    let worktree_output = serde_json::json!({"result":{"worktrees":[{"path":directory.repo,"open_workspace_id":"workspace-1"}]}});
    fs::write(&mock,format!("#!/bin/sh\ncase \"$*\" in\n'pane list --workspace workspace-1') cat <<'PANES'\n{pane_output}\nPANES\n;;\n'worktree list --workspace workspace-1 --json') cat <<'WORKTREE'\n{worktree_output}\nWORKTREE\n;;\n*) exit 19;;\nesac\n")).unwrap();
    fs::set_permissions(&mock, fs::Permissions::from_mode(0o700)).unwrap();
    let run = |argv: &[&str], input: serde_json::Value| {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_namedrop"))
            .current_dir(&directory.repo)
            .isolated_home(&directory.home)
            .env("CLAUDE_CONFIG_DIR", directory.home.join(".claude"))
            .env("CODEX_HOME", directory.home.join(".codex"))
            .env_remove("XDG_CONFIG_HOME")
            .env("HERDR_BIN_PATH", &mock)
            .arg("--plugin-state")
            .arg(&directory.state)
            .args([
                "--herdr-socket",
                "socket-a",
                "--herdr-workspace",
                "workspace-1",
                "--herdr-pane",
                "pane-source",
            ])
            .args(argv)
            .arg("--input-json")
            .arg(input.to_string())
            .output()
            .unwrap();
        let result = serde_json::from_slice::<serde_json::Value>(&output.stdout)
            .unwrap_or_else(|_| panic!("{}", String::from_utf8_lossy(&output.stdout)));
        (output.status.success(), result)
    };
    let call = |argv: &[&str], input: serde_json::Value| {
        let (succeeded, result) = run(argv, input);
        assert!(succeeded, "{result}");
        result
    };
    call(
        &["group", "set"],
        serde_json::json!({"alias":"team","members":["reviewer"]}),
    );
    assert_eq!(
        call(&["group", "list"], serde_json::json!({}))["data"]["groups"][0]["alias"],
        "team"
    );
    let before = fs::read(directory.state.join("namedrop.sqlite3")).unwrap();
    call(&["handoff", "status"], serde_json::json!({}));
    assert_eq!(
        call(
            &["handoff", "preview"],
            serde_json::json!({"source":"@team Inspect this."})
        )["data"]["recipients"][0]["alias"],
        "reviewer"
    );
    assert_eq!(
        fs::read(directory.state.join("namedrop.sqlite3")).unwrap(),
        before
    );
    let (adopted, refused) = run(
        &["handoff", "adopt"],
        serde_json::json!({"request_id":"x","coordination_id":"missing"}),
    );
    assert!(!adopted, "{refused}");
    assert_eq!(refused["status"], "rejected");
    assert!(
        refused["error"]["message"]
            .as_str()
            .unwrap()
            .contains("does not exist in this workspace"),
        "{refused}"
    );
    for token in [
        "handoff.status",
        "handoff.results",
        "handoff.preview",
        "handoff.adopt",
        "group.list",
        "group.set",
        "group.remove",
    ] {
        let description = namedrop::describe_operations(Some(token)).unwrap();
        let entry = &description["operations"][0];
        assert_eq!(entry["token"], token);
        assert!(entry["input_schema"].is_object());
    }
    assert_eq!(
        namedrop::describe_operations(Some("handoff.adopt")).unwrap()["operations"][0]["input_schema"]
            ["required"],
        serde_json::json!(["request_id", "coordination_id"])
    );
    assert!(
        !namedrop::complete_capability_profile()
            .iter()
            .any(|s| matches!(s.as_str(), "group.set" | "group.remove"))
    );
    call(&["group", "remove"], serde_json::json!({"alias":"team"}));
}

/// The durable records maintenance must carry across a replacement
/// installation: addresses with their active alias and saved recipient type,
/// group membership in order, mailbox deliveries, attempt states, messages,
/// threads and integration registrations. Read straight out of SQLite so the
/// comparison does not depend on the reporting code a replacement also
/// replaces. Tables outside this list are not compared.
fn durable_records(directory: &TestDirectory) -> String {
    let database = rusqlite::Connection::open(directory.state.join("namedrop.sqlite3")).unwrap();
    let mut dump = String::new();
    for (label, query) in [
        (
            "addresses",
            "SELECT a.address_id, c.alias, a.pane_type, a.retired
               FROM pane_addresses a
               LEFT JOIN alias_claims c
                 ON c.address_id = a.address_id AND c.claim_state = 'active'
              ORDER BY a.address_id",
        ),
        (
            "groups",
            "SELECT alias, member_order, address_id FROM mention_group_members
              ORDER BY alias, member_order",
        ),
        (
            "deliveries",
            "SELECT delivery_id, address_id, state FROM mailbox_deliveries ORDER BY delivery_id",
        ),
        (
            "attempts",
            "SELECT attempt_id, address_id, state, active FROM delivery_attempt_state
              ORDER BY attempt_id",
        ),
        (
            "messages",
            "SELECT message_id, request_id, body FROM collaboration_messages ORDER BY message_id",
        ),
        (
            "threads",
            "SELECT thread_id FROM collaboration_threads ORDER BY thread_id",
        ),
        (
            "registrations",
            "SELECT address_id, agent_session_id FROM integration_registrations ORDER BY address_id",
        ),
    ] {
        let mut statement = database.prepare(query).unwrap();
        let columns = statement.column_count();
        let rows = statement
            .query_map([], |row| {
                let mut cells = Vec::new();
                for column in 0..columns {
                    cells.push(match row.get_ref(column)? {
                        rusqlite::types::ValueRef::Null => "NULL".to_owned(),
                        rusqlite::types::ValueRef::Integer(value) => value.to_string(),
                        rusqlite::types::ValueRef::Text(value) => {
                            String::from_utf8_lossy(value).into_owned()
                        }
                        // Every column selected above is TEXT or INTEGER.
                        other => format!("{other:?}"),
                    });
                }
                Ok(cells.join("|"))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        dump.push_str(&format!("[{label}]\n{}\n", rows.join("\n")));
    }
    dump
}

/// Builds the state a maintenance window has to protect: a named terminal
/// recipient, a registered agent recipient, a saved group, collaboration
/// history, one dispatched attempt held for a human, and a later delivery
/// queued behind that hold. Returns the held attempt.
fn state_worth_preserving(
    directory: &TestDirectory,
    herdr: &FakeHerdr,
    ids: &SequenceIds,
) -> namedrop::DeliveryAttemptId {
    complete_reconciliation(directory);
    // The terminal recipient is named first and stays a terminal: the host only
    // reports a shell for this pane, and its loss of an Agent Session must not
    // expire a registration taken later for a different pane.
    *herdr.terminal.lock().unwrap() = true;
    namedrop::execute_lifecycle_operation(
        "name",
        serde_json::json!({"alias": "build", "pane_type": "terminal"}),
        &runtime(directory, herdr, ids, "pane-reviewer"),
    )
    .unwrap();
    let source = runtime(directory, herdr, ids, "pane-source");
    let researcher = runtime(directory, herdr, ids, "pane-researcher");
    register("address-source", &source);
    register("address-researcher", &researcher);
    namedrop::execute_group_operation(
        "group.set",
        serde_json::json!({"alias": "review-team", "members": ["researcher", "source"]}),
        &source,
    )
    .unwrap();
    execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "before-maintenance",
            "source": "@researcher Inspect this.",
            "author_kind": "agent_session"
        }),
        &source,
    )
    .unwrap();
    let delivered = herdr.prompt_for_pane("pane-researcher");
    let attempt =
        namedrop::DeliveryAttemptId::new(delivered["control"]["attempt_id"].as_str().unwrap());
    StateStore::open(&directory.state)
        .unwrap()
        .transaction(|transaction| {
            transaction.transition_delivery_attempt(
                &StateNamespace::for_space("socket-a", "workspace-1"),
                &attempt,
                namedrop::DeliveryState::Held,
                &MutationAttribution::new(ActorKind::Plugin, "test", 43),
            )
        })
        .unwrap();
    // A second request must wait behind the hold rather than be retried.
    execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "queued-behind-the-hold",
            "source": "@researcher And then this.",
            "author_kind": "agent_session"
        }),
        &source,
    )
    .unwrap();
    attempt
}

/// Maintenance replaces the installation directory, never the durable state:
/// Herdr reports a state directory outside the package that gets replaced. When
/// the replacement installation activates, every Pane Address, alias, saved
/// recipient type, group, message, registration and queued or held Mailbox
/// Delivery is still present with its original identity, and no pending or held
/// work is resent.
#[test]
fn a_replacement_installation_preserves_every_durable_record_and_resends_nothing() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let held = state_worth_preserving(&directory, &herdr, &ids);
    let before = durable_records(&directory);
    assert!(
        before.contains("build|terminal|0"),
        "the fixture did not save a terminal recipient: {before}"
    );
    assert!(
        before.contains("review-team|0|address-researcher")
            && before.contains("review-team|1|address-source"),
        "the fixture did not save a group with ordered members: {before}"
    );
    assert!(
        before.contains(&format!("{}|address-researcher|held", held.as_str())),
        "{before}"
    );
    let dispatches = herdr.prompts.lock().unwrap().len();

    // The replacement installation is a different directory holding the same
    // release: only the executable's location changed, so activation reopens
    // the identical state directory Herdr already reported.
    let replacement = runtime(&directory, &herdr, &ids, "pane-source");
    let activated = namedrop::execute_lifecycle_operation(
        "activate",
        serde_json::json!({"confirmed": true}),
        &replacement,
    )
    .unwrap();

    // Activation alone gates dispatch behind a fresh reconciliation, so the
    // resend question is only answered once the space is draining again.
    for bucket in ["requeued", "submitted", "held", "terminal_running"] {
        assert_eq!(
            activated["drain"][bucket],
            serde_json::json!([]),
            "activating a replacement installation moved {bucket} work: {activated}"
        );
    }
    // Herdr runs the manifest's startup `reconcile` for the replacement
    // installation, which refreshes panes and resumes draining. That is where a
    // resend would show up.
    let reconciled =
        namedrop::execute_lifecycle_operation("reconcile", serde_json::json!({}), &replacement)
            .unwrap();
    for bucket in ["requeued", "submitted", "held", "failed", "expired"] {
        assert_eq!(
            reconciled["drain"][bucket],
            serde_json::json!([]),
            "reconciling a replacement installation moved {bucket} work: {reconciled}"
        );
    }
    let drained = namedrop::drain_mailboxes(&replacement).unwrap();
    assert!(
        drained.requeued.is_empty() && drained.submitted.is_empty(),
        "draining after a replacement installation resent held or blocked work: {drained:?}"
    );
    assert!(
        drained.held.is_empty() && drained.failed.is_empty() && drained.expired.is_empty(),
        "draining after a replacement installation changed a delivery disposition: {drained:?}"
    );
    assert_eq!(
        herdr.prompts.lock().unwrap().len(),
        dispatches,
        "a replacement installation resent an already dispatched delivery"
    );
    assert_eq!(
        durable_records(&directory),
        before,
        "a replacement installation changed durable collaboration records"
    );
    assert_eq!(
        StateStore::open(&directory.state)
            .unwrap()
            .schema_version()
            .unwrap(),
        7,
        "reactivation after a replacement installation changed the schema"
    );
    // The held attempt still needs a human decision, addressed to the same
    // recipient, and reads still cannot recover it.
    let report = serde_json::to_value(namedrop::diagnose_collaboration(&replacement)).unwrap();
    let issue = report["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "ND-DELIVERY-001")
        .unwrap_or_else(|| panic!("the held attempt stopped needing attention: {report}"));
    assert_eq!(issue["diagnostic"]["attempt_id"], held.as_str());
    assert_eq!(issue["diagnostic"]["address_id"], "address-researcher");

    // Positive control: the blocked work is genuinely dispatchable once a human
    // cancels the hold, so "nothing was resent" above is a property of the hold
    // rather than a fixture that cannot dispatch at all.
    namedrop::execute_control_operation(
        "delivery.cancel-held",
        serde_json::json!({"attempt_id": held.as_str(), "confirmed": true}),
        &replacement,
    )
    .unwrap();
    namedrop::drain_mailboxes(&replacement).unwrap();
    assert!(
        herdr.prompts.lock().unwrap().len() > dispatches,
        "cancelling the hold did not release the delivery queued behind it, so the \
         resend assertions above could not have observed one"
    );
    assert!(
        durable_records(&directory).contains("|address-researcher|acknowledged"),
        "the released delivery never reached its recipient: {}",
        durable_records(&directory)
    );
}

/// Rollback across schema versions is a state decision, not a code-only one. An
/// executable pointed at state a newer release already migrated fails closed
/// with the exact versions, on both the writable and the read-only path, and
/// never downgrades or rewrites what it cannot support.
#[test]
fn state_newer_than_the_executable_is_refused_instead_of_downgraded() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    state_worth_preserving(&directory, &herdr, &ids);
    let before = durable_records(&directory);
    let database = directory.state.join("namedrop.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection.execute_batch("PRAGMA user_version=8;").unwrap();
    drop(connection);

    let writable = StateStore::open(&directory.state)
        .err()
        .expect("a newer schema must not open writable");
    assert_eq!(writable.kind(), namedrop::StoreErrorKind::UnsupportedSchema);
    assert!(
        writable
            .message()
            .contains("database schema 8 is not supported; expected 7"),
        "{}",
        writable.message()
    );
    let read_only = StateStore::open_read_only(&directory.state)
        .err()
        .expect("a newer schema must not open read-only");
    assert_eq!(
        read_only.kind(),
        namedrop::StoreErrorKind::UnsupportedSchema
    );
    assert!(
        read_only
            .message()
            .contains("state has schema 8; expected 7"),
        "{}",
        read_only.message()
    );

    // What the user actually sees, and what the rollback guidance quotes.
    let report = serde_json::to_value(namedrop::diagnose_collaboration(&runtime(
        &directory,
        &herdr,
        &ids,
        "pane-source",
    )))
    .unwrap();
    assert_eq!(report["status"], "unavailable", "{report}");
    let issue = report["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "ND-INSPECT-001")
        .unwrap_or_else(|| panic!("doctor did not report the schema it cannot read: {report}"));
    assert!(
        issue["explanation"]
            .as_str()
            .unwrap()
            .contains("state has schema 8; expected 7"),
        "{issue}"
    );

    let connection = rusqlite::Connection::open(&database).unwrap();
    let version: u32 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    drop(connection);
    assert_eq!(version, 8, "a refused open rewrote the schema version");
    assert_eq!(
        durable_records(&directory),
        before,
        "a refused open changed durable collaboration records"
    );

    // The refusal is about the schema, not the directory: state back at a
    // supported version opens again. Restoring an actual backup is the next
    // test; this only shows the executable was never the broken part.
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch("DROP INDEX transition_facts_by_entity; PRAGMA user_version=6;")
        .unwrap();
    drop(connection);
    assert_eq!(
        StateStore::open(&directory.state)
            .unwrap()
            .schema_version()
            .unwrap(),
        7
    );
}

fn missing_schema_6_pieces(database: &std::path::Path) -> Vec<&'static str> {
    let connection = rusqlite::Connection::open(database).unwrap();
    let count = |query: &str| {
        connection
            .query_row(query, [], |row| row.get::<_, i64>(0))
            .unwrap()
    };
    let mut missing = Vec::new();
    if count("SELECT count(*) FROM sqlite_master WHERE type = 'view' AND name = 'delivery_seats'")
        == 0
    {
        missing.push("delivery_seats");
    }
    if count(
        "SELECT count(*) FROM pragma_table_info('pane_addresses')
          WHERE name = 'remote_commands_terminal_id'",
    ) == 0
    {
        missing.push("remote_commands_terminal_id");
    }
    missing
}

#[test]
fn pre_release_schema_6_state_is_completed_on_open() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    state_worth_preserving(&directory, &herdr, &ids);
    let before = durable_records(&directory);
    let database = directory.state.join("namedrop.sqlite3");

    for (missing, strip) in [
        (
            vec!["delivery_seats", "remote_commands_terminal_id"],
            "DROP VIEW delivery_seats; ALTER TABLE pane_addresses DROP COLUMN remote_commands_terminal_id;",
        ),
        (vec!["delivery_seats"], "DROP VIEW delivery_seats;"),
        (
            vec!["remote_commands_terminal_id"],
            "ALTER TABLE pane_addresses DROP COLUMN remote_commands_terminal_id;",
        ),
    ] {
        let connection = rusqlite::Connection::open(&database).unwrap();
        connection
            .execute_batch("DROP INDEX transition_facts_by_entity; PRAGMA user_version=6;")
            .unwrap();
        connection.execute_batch(strip).unwrap();
        drop(connection);
        assert_eq!(missing_schema_6_pieces(&database), missing);

        let listed = namedrop::execute_lifecycle_operation(
            "address.list",
            serde_json::json!({}),
            &runtime(&directory, &herdr, &ids, "pane-source"),
        )
        .unwrap_or_else(|error| panic!("pre-release schema 6 {missing:?} did not open: {error}"));
        assert!(
            listed
                .as_array()
                .unwrap()
                .iter()
                .any(|record| record["alias"] == "build"),
            "{listed}"
        );
        assert!(missing_schema_6_pieces(&database).is_empty());
        let connection = rusqlite::Connection::open(&database).unwrap();
        let version: u32 = connection
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        drop(connection);
        assert_eq!(version, 7);
        assert_eq!(
            durable_records(&directory),
            before,
            "completing the schema changed durable collaboration records"
        );
    }
}

#[test]
fn read_only_opens_name_pre_release_schema_6_and_never_complete_it() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    state_worth_preserving(&directory, &herdr, &ids);
    let before = durable_records(&directory);
    let database = directory.state.join("namedrop.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch("DROP INDEX transition_facts_by_entity; PRAGMA user_version=6; DROP VIEW delivery_seats; ALTER TABLE pane_addresses DROP COLUMN remote_commands_terminal_id;")
        .unwrap();
    drop(connection);

    let report = serde_json::to_value(namedrop::diagnose_collaboration(&runtime(
        &directory,
        &herdr,
        &ids,
        "pane-source",
    )))
    .unwrap();
    assert_eq!(report["status"], "unavailable", "{report}");
    let issue = report["issues"]
        .as_array()
        .unwrap()
        .iter()
        .find(|issue| issue["code"] == "ND-INSPECT-001")
        .unwrap_or_else(|| panic!("doctor did not report the schema it cannot read: {report}"));
    let explanation = issue["explanation"].as_str().unwrap();
    assert!(
        explanation.contains("pre-release schema 6") && explanation.contains("activat"),
        "doctor must name the pre-release shape and what completes it: {explanation}"
    );
    let read_only = StateStore::open_read_only(&directory.state)
        .err()
        .expect("pre-release schema 6 must not open read-only");
    assert_eq!(
        read_only.kind(),
        namedrop::StoreErrorKind::UnsupportedSchema
    );
    assert_eq!(
        missing_schema_6_pieces(&database),
        ["delivery_seats", "remote_commands_terminal_id"],
        "a read-only open completed the schema"
    );
    assert_eq!(durable_records(&directory), before);

    assert_eq!(
        StateStore::open(&directory.state)
            .unwrap()
            .schema_version()
            .unwrap(),
        7
    );
    assert!(missing_schema_6_pieces(&database).is_empty());
    assert!(
        StateStore::open_read_only(&directory.state)
            .unwrap()
            .is_some()
    );
}

/// The documented backup is a consistent online copy, and restoring it brings
/// back exactly the records the backup held — including a held attempt — while
/// discarding everything written after it.
#[test]
fn a_matching_backup_restores_exactly_the_records_a_rollback_needs() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    state_worth_preserving(&directory, &herdr, &ids);
    let database = directory.state.join("namedrop.sqlite3");
    let backup = directory.root.join("namedrop-backup.sqlite3");
    let recorded = durable_records(&directory);

    // A consistent copy taken while the database is open, rather than a plain
    // file copy that can capture a half-applied transaction.
    let connection = rusqlite::Connection::open(&database).unwrap();
    connection
        .execute_batch(&format!("VACUUM INTO '{}';", backup.display()))
        .unwrap();
    drop(connection);
    let copy = rusqlite::Connection::open(&backup).unwrap();
    assert_eq!(
        copy.query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "ok"
    );
    assert_eq!(
        copy.pragma_query_value(None, "user_version", |row| row.get::<_, u32>(0))
            .unwrap(),
        7,
        "the backup does not record the schema it must be restored against"
    );
    drop(copy);

    // Work done after the backup, which a rollback is expected to discard.
    namedrop::execute_group_operation(
        "group.set",
        serde_json::json!({"alias": "after-the-backup", "members": ["researcher"]}),
        &runtime(&directory, &herdr, &ids, "pane-source"),
    )
    .unwrap();
    assert_ne!(durable_records(&directory), recorded);

    // A restore usually follows a crash, which can leave a hot rollback journal
    // and hook events queued after the backup. The documented restore clears
    // both before copying, so neither is replayed over the restored database.
    let journal = directory.state.join("namedrop.sqlite3-journal");
    fs::write(&journal, b"stale rollback journal").unwrap();
    let queued = directory
        .state
        .join("agent-status-reconciliation")
        .join("scope");
    fs::create_dir_all(&queued).unwrap();
    let marker = queued.join("pending-after-the-backup");
    fs::write(&marker, b"{}").unwrap();

    fs::remove_file(&database).unwrap();
    fs::remove_file(&journal).unwrap();
    fs::remove_dir_all(directory.state.join("agent-status-reconciliation")).unwrap();
    fs::copy(&backup, &database).unwrap();

    assert!(
        !journal.exists(),
        "the restore left a stale rollback journal"
    );
    assert!(
        !marker.exists(),
        "the restore left a hook event queued after the backup"
    );
    assert_eq!(
        durable_records(&directory),
        recorded,
        "restoring the backup did not bring back the recorded records"
    );
    assert!(
        !durable_records(&directory).contains("after-the-backup"),
        "the restore kept work the backup never held"
    );
    let restored = StateStore::open(&directory.state).unwrap();
    assert_eq!(restored.schema_version().unwrap(), 7);
}

/// Backup guidance has to match how this state is actually journalled. Namedrop
/// never enables WAL, so there is no durable `-wal`/`-shm` pair to copy
/// alongside the database; the rollback journal exists only inside a write
/// transaction, which is why a consistent copy is taken through SQLite rather
/// than with `cp`.
#[test]
fn durable_state_uses_a_rollback_journal_and_never_a_wal_sidecar() {
    let directory = TestDirectory::new();
    setup(&directory);
    let herdr = FakeHerdr::new(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    state_worth_preserving(&directory, &herdr, &ids);
    let database = directory.state.join("namedrop.sqlite3");
    let connection = rusqlite::Connection::open(&database).unwrap();
    assert_eq!(
        connection
            .query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))
            .unwrap(),
        "delete",
        "backup guidance assumes a rollback journal, not WAL"
    );
    drop(connection);
    let sidecars = fs::read_dir(&directory.state)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with("-wal") || name.ends_with("-shm"))
        .collect::<Vec<_>>();
    assert!(
        sidecars.is_empty(),
        "state left a WAL sidecar the documented backup does not copy: {sidecars:?}"
    );
}
