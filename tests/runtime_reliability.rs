#[path = "support/cleanup.rs"]
mod cleanup;

use namedrop::{
    ActorKind, AgentSessionId, DeliveryAttemptId, DeliveryId, DeliveryState, LeaseDraft,
    MessageDraft, MessageId, MessageRecipientDraft, MutationAttribution, PaneAddressDraft,
    PaneAddressId, PaneBindingId, PaneRoleId, StateNamespace, StateStore, ThreadId,
};
use std::{
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

static NEXT: AtomicU64 = AtomicU64::new(1);
const HERDR_SCRIPT: &str = "case \"$1 $2\" in\n\
    'worktree list') printf '%s\\n' '{\"result\":{\"worktrees\":[{\"path\":\"/repo\",\"open_workspace_id\":\"workspace-1\"}]}}';;\n\
    'pane list') printf '%s\\n' '{\"result\":{\"panes\":[{\"pane_id\":\"pane-recipient\",\"terminal_id\":\"terminal-1\",\"tab_id\":\"tab-1\",\"workspace_id\":\"workspace-1\",\"agent_session\":{\"value\":\"session-1\"},\"agent_status\":\"idle\"}]}}';;\n\
    'pane process-info') printf '{\"result\":{\"process_info\":{\"pane_id\":\"pane-recipient\",\"shell_pid\":%s,\"foreground_process_group_id\":%s,\"foreground_processes\":[]}}}\\n' \"$TEST_SHELL_PID\" \"$TEST_SHELL_PID\";;\n\
    *) printf '%s\\n' '{}';;\nesac\n";

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-runtime-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("home")).unwrap();
        let fixture = Self { root };
        fixture.set_herdr(HERDR_SCRIPT);
        fixture
    }

    fn state(&self) -> PathBuf {
        self.root.join("state")
    }

    fn namespace(&self) -> StateNamespace {
        StateNamespace::for_space("test-socket", "workspace-1")
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
        command
            .env("HERDR_PLUGIN_STATE_DIR", self.state())
            .env("HERDR_SOCKET_PATH", "test-socket")
            .env("HERDR_WORKSPACE_ID", "workspace-1")
            .env("HERDR_PANE_ID", "pane-recipient")
            // The test process is an ancestor of every process it starts, so
            // the fake pane's shell is this process.
            .env("TEST_SHELL_PID", std::process::id().to_string())
            .env("HERDR_BIN_PATH", self.root.join("herdr"))
            .env("NAMEDROP_TEST_FIXTURE", &self.root)
            .env("HOME", self.root.join("home"))
            .env("CLAUDE_CONFIG_DIR", self.root.join("home").join(".claude"))
            .env("CODEX_HOME", self.root.join("home").join(".codex"))
            .env_remove("XDG_CONFIG_HOME")
            .env("CODEX_HOME", self.root.join("home/.codex"))
            .env_remove("NAMEDROP_HERDR_TIMEOUT_MS");
        command
    }

    fn set_herdr(&self, body: &str) {
        let path = self.root.join("herdr");
        fs::write(&path, format!("#!/bin/sh\n{body}")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn initialize(&self) {
        let namespace = self.namespace();
        let attribution = MutationAttribution::new(ActorKind::Plugin, "fixture", now_ms());
        StateStore::open(self.state())
            .unwrap()
            .transaction(|tx| {
                tx.activate_worktree(&namespace, &attribution)?;
                for alias in ["author", "recipient"] {
                    tx.create_pane_address(
                        &namespace,
                        &PaneAddressDraft {
                            id: PaneAddressId::new(alias),
                            role_id: PaneRoleId::new(format!("role-{alias}")),
                            role_name: alias.to_owned(),
                            role_description: "Runtime fixture".to_owned(),
                            alias: alias.to_owned(),
                        },
                        &attribution,
                    )?;
                }
                tx.bind_pane(
                    &namespace,
                    &PaneBindingId::new("binding-1"),
                    &PaneAddressId::new("recipient"),
                    "pane-recipient",
                    &attribution,
                )?;
                tx.observe_agent_session(
                    &namespace,
                    "initial-observation",
                    &PaneAddressId::new("recipient"),
                    &AgentSessionId::new("session-1"),
                    "working",
                    &attribution,
                )?;
                tx.create_thread(
                    &namespace,
                    &ThreadId::new("thread-1"),
                    "Fixture",
                    &attribution,
                )?;
                tx.complete_reconciliation(&namespace)?;
                Ok(())
            })
            .unwrap();
    }

    fn active_lease(&self) -> DeliveryAttemptId {
        self.queue_message();
        let attempt_id = DeliveryAttemptId::new("active-attempt");
        let namespace = self.namespace();
        let now = now_ms();
        let attribution = MutationAttribution::new(ActorKind::Plugin, "fixture", now);
        StateStore::open(self.state())
            .unwrap()
            .transaction(|tx| {
                assert!(
                    tx.lease_mailbox_head(
                        &namespace,
                        &PaneAddressId::new("recipient"),
                        &LeaseDraft {
                            attempt_id: attempt_id.clone(),
                            binding_id: PaneBindingId::new("binding-1"),
                            agent_session_id: AgentSessionId::new("session-1"),
                            retry_of_attempt_id: None,
                            owner: "active-dispatcher".to_owned(),
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
        attempt_id
    }

    /// One mention the recipient's session has already received, created at
    /// `created_at_ms`, so a test can place it inside or past the
    /// acknowledgement grace.
    fn acknowledged_attempt(&self, created_at_ms: u64) -> DeliveryAttemptId {
        self.queue_message();
        let attempt_id = DeliveryAttemptId::new("acknowledged-attempt");
        let namespace = self.namespace();
        let attribution = MutationAttribution::new(ActorKind::Plugin, "fixture", created_at_ms);
        StateStore::open(self.state())
            .unwrap()
            .transaction(|tx| {
                assert!(
                    tx.lease_mailbox_head(
                        &namespace,
                        &PaneAddressId::new("recipient"),
                        &LeaseDraft {
                            attempt_id: attempt_id.clone(),
                            binding_id: PaneBindingId::new("binding-1"),
                            agent_session_id: AgentSessionId::new("session-1"),
                            retry_of_attempt_id: None,
                            owner: "fixture-dispatcher".to_owned(),
                            acquired_at_ms: created_at_ms,
                            expires_at_ms: created_at_ms + 30_000,
                        },
                        &attribution
                    )?
                    .is_some()
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
                    "fixture-receipt",
                    &AgentSessionId::new("session-1"),
                    &attribution,
                )?;
                Ok(())
            })
            .unwrap();
        attempt_id
    }

    fn queue_message(&self) {
        self.queue_message_numbered(1, "review");
    }

    /// Append one more message to the recipient's mailbox. `number` keys the
    /// message, request and delivery ids, so callers can queue several and
    /// observe the order they dispatch in.
    fn queue_message_numbered(&self, number: u8, body: &str) {
        let namespace = self.namespace();
        let attribution = MutationAttribution::new(ActorKind::Plugin, "fixture", now_ms());
        StateStore::open(self.state())
            .unwrap()
            .transaction(|tx| {
                tx.append_message(
                    &namespace,
                    &MessageDraft {
                        thread_id: ThreadId::new("thread-1"),
                        message_id: MessageId::new(format!("message-{number}")),
                        in_reply_to: None,
                        author_address_id: PaneAddressId::new("author"),
                        author_kind: ActorKind::Human,
                        author_alias_snapshot: "author".to_owned(),
                        author_agent_session_snapshot: None,
                        request_id: format!("request-{number}"),
                        authored_source: format!("@recipient {body}"),
                        body: body.to_owned(),
                        grammar_version: "mention/v1".to_owned(),
                        recipients: vec![MessageRecipientDraft {
                            address_id: PaneAddressId::new("recipient"),
                            alias_snapshot: "recipient".to_owned(),
                            agent_session_snapshot: Some(AgentSessionId::new("session-1")),
                            delivery_id: DeliveryId::new(format!("delivery-{number}")),
                        }],
                        context_references: vec![],
                    },
                    &attribution,
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn connect_recipient(&self) {
        self.set_herdr(&HERDR_SCRIPT.replace(
            "\"agent_status\":\"idle\"",
            "\"agent\":\"claude\",\"agent_status\":\"idle\"",
        ));
        namedrop::install_integration_package(
            &self.root.join("home"),
            namedrop::AgentRuntime::ClaudeCode,
            &fs::canonicalize(env!("CARGO_BIN_EXE_namedrop")).unwrap(),
            &self.state(),
        )
        .unwrap();
        self.register_current_session("registration-1", "nonce-1");
    }

    /// Register whichever Agent Session the pane currently reports. A
    /// registration is pinned to one session, so a restarted helper takes a new
    /// one before it can be dispatched to again.
    fn register_current_session(&self, registration_id: &str, nonce: &str) {
        // A challenge already open for this session is returned as it stands,
        // so the nonce to register with is the one that comes back, not the one
        // asked for: reconciling a restarted pane opens one of its own.
        let challenge = StateStore::open(self.state())
            .unwrap()
            .transaction(|tx| {
                tx.issue_integration_challenge(
                    &self.namespace(),
                    registration_id,
                    &PaneAddressId::new("recipient"),
                    namedrop::AgentRuntime::ClaudeCode,
                    nonce,
                    &MutationAttribution::new(ActorKind::Plugin, "fixture", now_ms()),
                )
            })
            .unwrap();
        let register = self.command().args(["register", "--input-json"])
            .arg(serde_json::json!({"runtime":"claude-code", "protocol_version":"collab/v1", "nonce":challenge.nonce, "capabilities":namedrop::complete_capability_profile()}).to_string())
            .output().unwrap();
        assert!(register.status.success(), "{register:?}");
    }

    /// What the Agent Session in the bound pane is currently owed, read through
    /// the same command an agent runs.
    fn inbox(&self) -> Vec<serde_json::Value> {
        let output = self.command().args(["delivery", "inbox"]).output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        json["data"]["envelopes"].as_array().cloned().unwrap()
    }

    fn inbox_bodies(&self) -> Vec<String> {
        self.inbox()
            .iter()
            .map(|envelope| envelope["untrusted_body"].as_str().unwrap().to_owned())
            .collect()
    }

    fn attempt_state(&self, attempt: &DeliveryAttemptId) -> DeliveryState {
        StateStore::open_read_only(self.state())
            .unwrap()
            .unwrap()
            .transaction(|tx| tx.delivery_attempt_state(&self.namespace(), attempt))
            .unwrap()
    }

    /// The `drain` report a synchronous `reconcile` publishes at the command
    /// boundary, so a test can see which bucket a prompt rejection landed in.
    fn reconcile_drain(&self) -> serde_json::Value {
        let output = self.command().arg("reconcile").output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        json["data"]["drain"].clone()
    }

    /// `(agent prompt invocations, invocations that accepted the submission)`
    /// as recorded by the fixture Herdr script.
    fn prompt_counts(&self) -> (usize, usize) {
        let count = |name: &str| {
            fs::read_to_string(self.root.join(name))
                .map(|log| log.len())
                .unwrap_or(0)
        };
        (count("prompts"), count("accepted-prompts"))
    }

    fn queue_status(&self, name: &str) -> PathBuf {
        self.queue_status_for(name, "pane-recipient")
    }

    fn queue_status_for(&self, name: &str, pane_id: &str) -> PathBuf {
        use sha2::{Digest, Sha256};
        let scope = namedrop::lower_hex(&Sha256::digest("test-socket\0workspace-1"));
        let directory = self.state().join("agent-status-reconciliation").join(scope);
        fs::create_dir_all(&directory).unwrap();
        let queued = directory.join(format!("queued-{name}"));
        let marker = directory.join(format!("pending-{name}"));
        fs::write(&queued, format!(r#"{{"type":"pane_agent_status_changed","pane_id":"{pane_id}","workspace_id":"workspace-1","agent_status":"idle"}}"#)).unwrap();
        fs::rename(queued, &marker).unwrap();
        marker
    }

    fn worker(&self) -> Worker {
        Worker(
            self.command()
                .args(["hook", "agent-status-worker"])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    fn recipient_status(&self) -> String {
        StateStore::open_read_only(self.state())
            .unwrap()
            .unwrap()
            .transaction(|tx| {
                Ok(tx
                    .list_pane_addresses(&self.namespace())?
                    .into_iter()
                    .find(|address| address.id == PaneAddressId::new("recipient"))
                    .unwrap()
                    .agent_status
                    .unwrap())
            })
            .unwrap()
    }
}

struct Worker(Child);

impl Worker {
    fn finish(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success(), "worker exit: {status}");
                return;
            }
            assert!(Instant::now() < deadline, "worker did not finish");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        cleanup::remove_fixture(&self.root).unwrap();
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap()
}

#[test]
fn mailbox_wake_preserves_live_leases_while_explicit_reconcile_recovers_crashes() {
    let fixture = Fixture::new();
    fixture.initialize();
    let attempt = fixture.active_lease();
    let wake = fixture
        .command()
        .args(["hook", "mailbox-wake", "--state-directory"])
        .arg(fixture.state())
        .output()
        .unwrap();
    assert!(wake.status.success(), "{wake:?}");
    assert_eq!(fixture.attempt_state(&attempt), DeliveryState::Dispatching);
    let restart = fixture.command().arg("reconcile").output().unwrap();
    assert!(restart.status.success(), "{restart:?}");
    assert_eq!(fixture.attempt_state(&attempt), DeliveryState::Held);
}

#[test]
fn status_worker_retries_database_contention_without_losing_the_event() {
    let fixture = Fixture::new();
    fixture.initialize();
    let marker = fixture.queue_status("busy");
    let connection = rusqlite::Connection::open(fixture.state().join("namedrop.sqlite3")).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    let mut worker = fixture.worker();
    // Outlast the store's two-second busy timeout, then allow the same durable
    // event to finish without requiring another Herdr status notification.
    thread::sleep(Duration::from_millis(2_700));
    connection.execute_batch("ROLLBACK").unwrap();
    worker.finish();
    assert_eq!(fixture.recipient_status(), "idle");
    assert!(!marker.exists());
}

#[test]
fn status_worker_recovers_after_the_host_connection_returns() {
    let fixture = Fixture::new();
    fixture.initialize();
    fixture.set_herdr(&format!(
        "if [ ! -f \"$NAMEDROP_TEST_FIXTURE/host-ready\" ]; then\n\
           touch \"$NAMEDROP_TEST_FIXTURE/host-failed\"\n\
           printf '%s\\n' '{{\"error\":{{\"code\":\"connection_refused\",\"message\":\"host restarting\"}}}}' >&2\n\
           exit 1\nfi\n{HERDR_SCRIPT}"
    ));
    let marker = fixture.queue_status("host-unavailable");
    let mut worker = fixture.worker();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !fixture.root.join("host-failed").exists() {
        assert!(Instant::now() < deadline, "host failure was not reached");
        thread::sleep(Duration::from_millis(10));
    }
    fs::write(fixture.root.join("host-ready"), "ready").unwrap();
    worker.finish();
    assert_eq!(fixture.recipient_status(), "idle");
    assert!(!marker.exists());
}

#[test]
fn unreadable_event_survives_worker_restart_and_finishes_after_access_returns() {
    let fixture = Fixture::new();
    fixture.initialize();
    let marker = fixture.queue_status("temporarily-unreadable");
    fs::set_permissions(&marker, fs::Permissions::from_mode(0o000)).unwrap();
    let mut worker = fixture.worker();
    thread::sleep(Duration::from_secs(1));
    assert!(
        marker.exists(),
        "a read failure must not delete durable work"
    );
    worker.0.kill().unwrap();
    worker.0.wait().unwrap();
    fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).unwrap();
    worker = fixture.worker();
    worker.finish();
    assert_eq!(fixture.recipient_status(), "idle");
    assert!(!marker.exists());
}

#[test]
fn concurrent_status_workers_leave_no_pending_wakes_during_repeated_handoffs() {
    let fixture = Fixture::new();
    fixture.initialize();
    let mut markers = Vec::new();
    for wave in 0..8 {
        let mut workers = Vec::new();
        for producer in 0..4 {
            markers.push(fixture.queue_status(&format!("{wave}-{producer}")));
            workers.push(fixture.worker());
            thread::yield_now();
        }
        for worker in &mut workers {
            worker.finish();
        }
    }
    assert_eq!(fixture.recipient_status(), "idle");
    assert!(
        markers.iter().all(|marker| !marker.exists()),
        "a completed worker left a wake stranded"
    );
}

#[test]
fn ordinary_cli_calls_have_a_finite_default_herdr_deadline() {
    let fixture = Fixture::new();
    fixture.set_herdr("exec sleep 60\n");
    let mut child = Worker(
        fixture
            .command()
            .args(["address", "list"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(12);
    let status = loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            break status;
        }
        assert!(
            Instant::now() < deadline,
            "ordinary CLI call has no finite default deadline"
        );
        thread::sleep(Duration::from_millis(20));
    };
    let mut output = String::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_string(&mut output)
        .unwrap();
    assert!(!status.success());
    assert!(
        output.contains("Herdr command timed out after 10000 ms"),
        "{output}"
    );
}

#[test]
fn timeout_overrides_are_bounded_and_cannot_disable_command_execution() {
    let fixture = Fixture::new();
    fixture.initialize();
    for value in ["0", "invalid", "18446744073709551615"] {
        let output = fixture
            .command()
            .arg("reconcile")
            .env("NAMEDROP_HERDR_TIMEOUT_MS", value)
            .output()
            .unwrap();
        assert!(output.status.success(), "override {value}: {output:?}");
    }
    fixture.set_herdr("exec sleep 60\n");
    let started = Instant::now();
    let output = fixture
        .command()
        .args(["address", "list"])
        .env("NAMEDROP_HERDR_TIMEOUT_MS", "25")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("timed out after 25 ms"));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn timeout_and_unknown_prompt_results_never_authorize_an_automatic_resend() {
    use namedrop::{HerdrCommandExecutor, ProcessHerdr, PromptFailureKind};
    let herdr = ProcessHerdr::default();
    for error in [
        "Herdr command timed out after 10000 ms",
        "connection closed after writing prompt",
        r#"{"error":{"code":"connection_refused"}}"#,
        r#"{"error":{"code":"unexpected"}}"#,
        // Both can only be reported after Herdr accepted the submission, so
        // the recipient may already have the prompt.
        r#"{"error":{"code":"agent_prompt_stalled"}}"#,
        r#"{"error":{"code":"timeout"}}"#,
    ] {
        assert_eq!(
            herdr.classify_prompt_failure(error),
            PromptFailureKind::AmbiguousInvocation,
            "{error}"
        );
    }
    // Herdr rejects both before any input is sent. The `agent_blocked` payload
    // is the verbatim stderr of `herdr agent prompt` 0.8.2 against a blocked
    // recipient, exit code 1.
    for error in [
        r#"{"error":{"code":"agent_busy"}}"#,
        r#"{"error":{"code":"agent_blocked","message":"agent w1:p1 is blocked and requires interactive input"},"id":"cli:agent:prompt"}"#,
    ] {
        assert_eq!(
            herdr.classify_prompt_failure(error),
            PromptFailureKind::TemporaryPreInvocation,
            "{error}"
        );
    }
}

#[test]
fn temporary_prompt_failures_only_request_polling_without_a_guaranteed_event() {
    use namedrop::{HerdrCommandExecutor, ProcessHerdr, PromptFailureKind};
    let herdr = ProcessHerdr::default();
    for (codes, expected) in [
        (
            &["server_busy", "conflict", "agent_not_ready"][..],
            PromptFailureKind::TemporaryPreInvocationWithoutEvent,
        ),
        (
            &[
                "agent_busy",
                "agent_blocked",
                "agent_not_found",
                "pane_not_found",
            ][..],
            PromptFailureKind::TemporaryPreInvocation,
        ),
    ] {
        for code in codes {
            let kind = herdr
                .classify_prompt_failure(&serde_json::json!({"error": {"code": code}}).to_string());
            assert_eq!(kind, expected, "{code}");
            assert!(kind.is_temporary_pre_invocation());
        }
    }
    assert!(!PromptFailureKind::AmbiguousInvocation.is_temporary_pre_invocation());
    assert!(!PromptFailureKind::DefinitiveNonRetryable.is_temporary_pre_invocation());
}

#[test]
fn doctor_exit_codes_match_the_json_health_report() {
    let fixture = Fixture::new();
    let check = |args: &[&str], expected_code, expected_status| {
        let output = fixture.command().args(args).arg("doctor").output().unwrap();
        let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(json["data"]["status"], expected_status, "{json}");
        assert_eq!(output.status.code(), Some(expected_code), "{json}");
    };
    check(&[], 1, "needs_attention");
    fixture.set_herdr("printf '%s\\n' 'host unavailable' >&2\nexit 1\n");
    // A down host cannot vouch for the inherited pane, so its own error is
    // the answer; an explicit pane still gets the report.
    let output = fixture.command().arg("doctor").output().unwrap();
    let json: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(json["status"], "rejected", "{json}");
    assert!(
        json["error"]["message"]
            .as_str()
            .is_some_and(|message| message.contains("host unavailable")),
        "{json}"
    );
    assert_eq!(output.status.code(), Some(2), "{json}");
    check(&["--herdr-pane", "pane-recipient"], 2, "unavailable");

    fixture.initialize();
    fixture.connect_recipient();
    check(&[], 0, "healthy");
}

#[test]
fn temporary_prompt_rejections_are_retried_without_a_second_status_event() {
    for mailbox_wake in [false, true] {
        let fixture = Fixture::new();
        fixture.initialize();
        fixture.connect_recipient();
        fixture.queue_message();
        let script = fs::read_to_string(fixture.root.join("herdr")).unwrap();
        fixture.set_herdr(&format!(
            "if [ \"$1 $2\" = 'agent prompt' ] && [ ! -f \"$NAMEDROP_TEST_FIXTURE/accept-prompt\" ]; then\n\
             touch \"$NAMEDROP_TEST_FIXTURE/prompt-rejected\"\n\
             printf '%s\\n' '{{\"error\":{{\"code\":\"agent_busy\"}}}}' >&2\nexit 1\nfi\n{script}"
        ));
        let mut worker = if mailbox_wake {
            Worker(
                fixture
                    .command()
                    .args(["hook", "mailbox-wake", "--state-directory"])
                    .arg(fixture.state())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .unwrap(),
            )
        } else {
            fixture.queue_status("prompt-rejection");
            fixture.worker()
        };
        let deadline = Instant::now() + Duration::from_secs(5);
        while !fixture.root.join("prompt-rejected").exists() {
            assert!(
                Instant::now() < deadline,
                "prompt rejection was not reached: mailbox_wake={mailbox_wake}"
            );
            thread::sleep(Duration::from_millis(10));
        }
        fs::write(fixture.root.join("accept-prompt"), "ready").unwrap();
        worker.finish();
        let inbox = fixture
            .command()
            .args(["delivery", "inbox"])
            .output()
            .unwrap();
        assert!(inbox.status.success(), "{inbox:?}");
        let json: serde_json::Value = serde_json::from_slice(&inbox.stdout).unwrap();
        assert_eq!(
            json["data"]["envelopes"][0]["untrusted_body"], "review",
            "mailbox_wake={mailbox_wake}: {json}"
        );
    }
}

/// Herdr 0.8.2 `agent prompt --help`: "If the agent is already blocked,
/// submission is rejected with `agent_blocked` before any input is sent." The
/// recipient therefore never saw the prompt, so the delivery belongs back in
/// the queue and must dispatch later exactly once.
///
/// `agent_prompt_stalled` and `timeout` can only follow an *accepted*
/// submission, so they remain genuinely uncertain, as does any code this
/// version of Namedrop does not recognize: those are held for a human and are
/// never resent automatically.
///
/// Two messages are queued so the recipient's FIFO order is observable: a
/// requeued attempt must stay the head and dispatch before the message behind
/// it, while a held one must step aside without being resent.
#[test]
fn blocked_recipients_requeue_while_uncertain_prompt_results_stay_held() {
    for (code, requeues) in [
        ("agent_busy", true),
        ("agent_blocked", true),
        ("agent_prompt_stalled", false),
        ("timeout", false),
        ("a_code_this_version_does_not_know", false),
    ] {
        let fixture = Fixture::new();
        fixture.initialize();
        fixture.connect_recipient();
        fixture.queue_message();
        fixture.queue_message_numbered(2, "second");
        let script = fs::read_to_string(fixture.root.join("herdr")).unwrap();
        fixture.set_herdr(&format!(
            "if [ \"$1 $2\" = 'agent prompt' ]; then\n\
             printf 'x' >> \"$NAMEDROP_TEST_FIXTURE/prompts\"\n\
             if [ ! -f \"$NAMEDROP_TEST_FIXTURE/accept-prompt\" ]; then\n\
             printf '%s\\n' '{{\"error\":{{\"code\":\"{code}\"}}}}' >&2\nexit 1\nfi\n\
             printf 'x' >> \"$NAMEDROP_TEST_FIXTURE/accepted-prompts\"\nfi\n{script}"
        ));

        let rejected = fixture.reconcile_drain();
        let (expected, empty) = if requeues {
            ("requeued", "held")
        } else {
            ("held", "requeued")
        };
        assert_eq!(
            rejected[expected].as_array().map(Vec::len),
            Some(1),
            "{code} must be reported as {expected}: {rejected}"
        );
        assert_eq!(
            rejected[empty].as_array().map(Vec::len),
            Some(0),
            "{code} must not be reported as {empty}: {rejected}"
        );
        assert_eq!(
            rejected["submitted"].as_array().map(Vec::len),
            Some(0),
            "{code}: {rejected}"
        );
        // The `prompts` count of 1 is the positive control for the log the
        // `accepted-prompts` count of 0 is read from: the script ran, wrote one
        // invocation, and accepted none of it.
        assert_eq!(fixture.prompt_counts(), (1, 0), "{code}");

        // Now let the recipient accept input and drain twice. A requeued
        // attempt kept its FIFO head, so it dispatches once and `second` waits
        // behind it — one attempt is in flight per recipient until it settles.
        // A held attempt is neither resent nor skipped: it keeps the mailbox
        // stalled behind it until a human cancels that exact delivery, which is
        // what stops uncertain work from ever being invoked twice.
        fs::write(fixture.root.join("accept-prompt"), "ready").unwrap();
        let dispatches = usize::from(requeues);
        for round in 0..2 {
            let drain = fixture.reconcile_drain();
            assert_eq!(
                drain["submitted"].as_array().map(Vec::len),
                Some(if round == 0 { dispatches } else { 0 }),
                "{code} round {round}: {drain}"
            );
            assert_eq!(
                fixture.prompt_counts(),
                (1 + dispatches, dispatches),
                "{code} round {round} must reach the recipient exactly once"
            );
        }

        let inbox = fixture
            .command()
            .args(["delivery", "inbox"])
            .output()
            .unwrap();
        assert!(inbox.status.success(), "{code}: {inbox:?}");
        let json: serde_json::Value = serde_json::from_slice(&inbox.stdout).unwrap();
        let bodies: Vec<&str> = json["data"]["envelopes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|envelope| envelope["untrusted_body"].as_str().unwrap())
            .collect();
        let expected: &[&str] = if requeues { &["review"] } else { &[] };
        assert_eq!(bodies, expected, "{code}: {json}");
    }
}

#[test]
fn terminal_recovery_resolves_superseded_warnings_and_preserves_current_failure() {
    for cancelled in [true, false] {
        let fixture = Fixture::new();
        fixture.initialize();
        let rejected = fixture.active_lease();
        let current = DeliveryAttemptId::new("retry-attempt");
        let namespace = fixture.namespace();
        let now = now_ms();
        let attribution = MutationAttribution::new(ActorKind::Plugin, "fixture", now);
        StateStore::open(fixture.state())
            .unwrap()
            .transaction(|tx| {
                tx.return_delivery_attempt_to_queue(&namespace, &rejected, &attribution)?;
                assert!(
                    tx.lease_mailbox_head(
                        &namespace,
                        &PaneAddressId::new("recipient"),
                        &LeaseDraft {
                            attempt_id: current.clone(),
                            binding_id: PaneBindingId::new("binding-1"),
                            agent_session_id: AgentSessionId::new("session-1"),
                            retry_of_attempt_id: Some(rejected.clone()),
                            owner: "retry-dispatcher".to_owned(),
                            acquired_at_ms: now,
                            expires_at_ms: now + 30_000,
                        },
                        &attribution
                    )?
                    .is_some()
                );
                tx.transition_delivery_attempt(
                    &namespace,
                    &current,
                    if cancelled {
                        DeliveryState::Held
                    } else {
                        DeliveryState::Failed
                    },
                    &attribution,
                )
            })
            .unwrap();
        if cancelled {
            let before = StateStore::open_read_only(fixture.state())
                .unwrap()
                .unwrap()
                .transaction(|tx| tx.unresolved_attention_items(&namespace))
                .unwrap();
            assert!(
                before.iter().any(|item| item.source_id == current.as_str()),
                "held uncertainty must remain visible"
            );
            let result = fixture
                .command()
                .args(["delivery", "cancel-held", "--input-json"])
                .arg(serde_json::json!({"attempt_id":current, "confirmed":true}).to_string())
                .output()
                .unwrap();
            assert!(result.status.success(), "{result:?}");
        }
        let attention = StateStore::open_read_only(fixture.state())
            .unwrap()
            .unwrap()
            .transaction(|tx| tx.unresolved_attention_items(&namespace))
            .unwrap();
        assert!(
            attention
                .iter()
                .all(|item| item.source_id != rejected.as_str()),
            "superseded warning survived terminal recovery: {attention:?}"
        );
        assert_eq!(
            attention
                .iter()
                .any(|item| item.source_id == current.as_str()),
            !cancelled
        );
    }
}

/// The fixture host with a Claude-hosted recipient pane reporting `session`, and
/// every `agent prompt` invocation logged so a dispatch can be counted.
fn recipient_host(session: &str) -> String {
    format!(
        "if [ \"$1 $2\" = 'agent prompt' ]; then\n\
         printf 'x' >> \"$NAMEDROP_TEST_FIXTURE/prompts\"\n\
         printf 'x' >> \"$NAMEDROP_TEST_FIXTURE/accepted-prompts\"\nfi\n{}",
        HERDR_SCRIPT
            .replace(
                "\"agent_status\":\"idle\"",
                "\"agent\":\"claude\",\"agent_status\":\"idle\"",
            )
            .replace("session-1", session)
    )
}

/// Live evidence (`.unlazy/personas/live/evidence/state.sqlite3`): one address
/// holds mailbox_order 1 settled, 2 `held` (delivery-1790095905919372000-72383-3,
/// attempt ...-72383-4, active 0) and 3 `queued`
/// (delivery-1790096605315391000-88449-2, the reply that came back after the
/// helper session was restarted).
///
/// Two independent rules produce that, and both are deliberate. Reconcile holds
/// an in-flight attempt whose Agent Session no longer matches the pane, because
/// whether that session acted can no longer be established. An address is then a
/// dispatch candidate only while no earlier delivery sits outside
/// settled/failed/cancelled, so the mailbox keeps its order instead of stepping
/// over uncertain work. The reply behind the hold therefore waits, the restarted
/// session reads an empty inbox, and only an explicit human cancel of that exact
/// attempt releases it.
#[test]
fn a_restarted_session_holds_its_attempt_and_the_reply_behind_it_waits_for_a_human() {
    let fixture = Fixture::new();
    fixture.initialize();
    fixture.connect_recipient();
    fixture.set_herdr(&recipient_host("session-1"));
    fixture.queue_message();

    let first = fixture.reconcile_drain();
    assert_eq!(
        first["submitted"].as_array().map(Vec::len),
        Some(1),
        "the first message must reach the original session: {first}"
    );
    let held = DeliveryAttemptId::new(
        fixture.inbox()[0]["control"]["attempt_id"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    // Reading the inbox is what records receipt, so the attempt is
    // acknowledged; whether the session acted on it is still unknown.
    assert_eq!(fixture.attempt_state(&held), DeliveryState::Acknowledged);

    // The helper restarts: same pane, same address, new Agent Session. The
    // reconcile that observes the new session is also what holds the attempt
    // the old one was given; registering the new session follows it.
    fixture.set_herdr(&recipient_host("session-2"));
    fixture.reconcile_drain();
    assert_eq!(
        fixture.attempt_state(&held),
        DeliveryState::Held,
        "an attempt whose Agent Session is gone must be held, not assumed done"
    );
    fixture.register_current_session("registration-2", "nonce-2");

    // Onboarding the new session prompts the pane too, so dispatches are
    // counted as a delta from here rather than from zero.
    let onboarded = fixture.prompt_counts();

    // The reply comes back after the restart and queues behind that held work.
    fixture.queue_message_numbered(2, "reply");
    for round in 0..2 {
        let drain = fixture.reconcile_drain();
        assert_eq!(
            drain["submitted"].as_array().map(Vec::len),
            Some(0),
            "round {round}: the reply must not step over the held delivery: {drain}"
        );
    }
    assert!(
        fixture.inbox_bodies().is_empty(),
        "the restarted session must see nothing while a held delivery blocks its mailbox"
    );
    assert_eq!(
        fixture.prompt_counts(),
        onboarded,
        "nothing may be re-invoked while the hold stands"
    );

    // The documented recovery, and the only one: a human cancels that exact
    // held attempt after deciding what it did.
    let cancel = fixture
        .command()
        .args(["delivery", "cancel-held", "--input-json"])
        .arg(serde_json::json!({"attempt_id": held.as_str(), "confirmed": true}).to_string())
        .output()
        .unwrap();
    assert!(cancel.status.success(), "{cancel:?}");

    // Cancelling drains as it returns, so the reply is already on its way.
    assert_eq!(
        fixture.inbox_bodies(),
        vec!["reply".to_owned()],
        "cancelling the head must release the reply behind it"
    );
    assert_eq!(
        fixture.prompt_counts(),
        (onboarded.0 + 1, onboarded.1 + 1),
        "the reply must reach the new session exactly once"
    );
}

/// The positive control for the test above: the same restart and the same reply,
/// with nothing in flight when the session changed. Nothing is held, so the new
/// session inherits an empty mailbox and the reply dispatches at once. The
/// blockage is inherited from uncertain work, never from restarting.
#[test]
fn a_restarted_session_with_nothing_in_flight_receives_the_same_reply_at_once() {
    let fixture = Fixture::new();
    fixture.initialize();
    fixture.connect_recipient();
    fixture.set_herdr(&recipient_host("session-2"));
    fixture.reconcile_drain();
    fixture.register_current_session("registration-2", "nonce-2");
    let onboarded = fixture.prompt_counts();

    fixture.queue_message_numbered(2, "reply");
    let drain = fixture.reconcile_drain();
    assert_eq!(
        drain["submitted"].as_array().map(Vec::len),
        Some(1),
        "an unblocked mailbox must dispatch the reply: {drain}"
    );
    assert_eq!(fixture.inbox_bodies(), vec!["reply".to_owned()]);
    assert_eq!(
        fixture.prompt_counts(),
        (onboarded.0 + 1, onboarded.1 + 1),
        "the reply must reach the new session exactly once"
    );
}

/// One attempt carries one terminal disposition. A helper that replies and then
/// reaches for a second disposition is rejected, and the reply it already sent
/// stands: the attempt stays settled and nothing is re-delivered.
#[test]
fn a_second_disposition_on_an_answered_attempt_is_rejected_and_changes_nothing() {
    let fixture = Fixture::new();
    fixture.initialize();
    fixture.connect_recipient();
    fixture.set_herdr(&recipient_host("session-1"));
    fixture.queue_message();
    fixture.reconcile_drain();

    let envelope = fixture.inbox().first().cloned().expect("an owed envelope");
    let attempt = envelope["control"]["attempt_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let message = envelope["control"]["message_id"]
        .as_str()
        .unwrap()
        .to_owned();

    let replied = fixture
        .command()
        .args(["reply", "--input-json"])
        .arg(
            serde_json::json!({
                "attempt_id": attempt,
                "author_kind": "agent_session",
                "in_reply_to": message,
                "request_id": "reply-1",
                "source": "@author reviewed",
            })
            .to_string(),
        )
        .output()
        .unwrap();
    assert!(replied.status.success(), "{replied:?}");
    let replied: serde_json::Value = serde_json::from_slice(&replied.stdout).unwrap();
    assert_eq!(replied["data"]["disposition"], "reply");

    let second = fixture
        .command()
        .args(["complete", "--no-reply", "--input-json"])
        .arg(serde_json::json!({"attempt_id": attempt}).to_string())
        .output()
        .unwrap();
    assert!(
        !second.status.success(),
        "a second disposition must be rejected: {second:?}"
    );
    assert_eq!(
        fixture.attempt_state(&DeliveryAttemptId::new(attempt)),
        DeliveryState::Settled,
        "the rejected disposition must leave the answered attempt settled"
    );
    assert!(
        fixture.inbox_bodies().is_empty(),
        "a rejected disposition must not re-owe the envelope"
    );
}

/// Namestack verify run v10111443b186, capture `restart-adopt-stall`: an
/// acknowledged delivery settles by observation only once the session is idle
/// and thirty seconds have passed since the attempt was created, yet every
/// drain is event-driven. A turn that ended inside those thirty seconds left
/// its acknowledged attempt untouched for five idle minutes, and the mailbox
/// behind it with it. The idle event is the last one the host sends, so the
/// worker it starts has to carry the attempt through the grace itself.
#[test]
fn status_worker_settles_an_acknowledged_attempt_once_the_idle_grace_elapses() {
    let fixture = Fixture::new();
    fixture.initialize();
    let attempt = fixture.acknowledged_attempt(now_ms() - 29_000);
    let marker = fixture.queue_status("turn-ended");
    let mut worker = fixture.worker();
    worker.finish();
    assert_eq!(
        fixture.attempt_state(&attempt),
        DeliveryState::Settled,
        "the one idle event must see the attempt through its grace"
    );
    assert!(!marker.exists());
}

#[test]
fn status_worker_settles_an_attempt_whose_event_pane_closed_during_the_grace() {
    let fixture = Fixture::new();
    fixture.initialize();
    fixture.set_herdr(&format!(
        "if [ \"$1 $2\" = 'pane list' ] && [ -f \"$NAMEDROP_TEST_FIXTURE/pane-event-live\" ]; then\n\
           printf '%s\\n' '{{\"result\":{{\"panes\":[{{\"pane_id\":\"pane-recipient\",\"terminal_id\":\"terminal-1\",\"tab_id\":\"tab-1\",\"workspace_id\":\"workspace-1\",\"agent_session\":{{\"value\":\"session-1\"}},\"agent_status\":\"idle\"}},{{\"pane_id\":\"pane-event\",\"terminal_id\":\"terminal-2\",\"tab_id\":\"tab-2\",\"workspace_id\":\"workspace-1\"}}]}}}}'\n\
           exit 0\nfi\n{HERDR_SCRIPT}"
    ));
    fs::write(fixture.root.join("pane-event-live"), "live").unwrap();
    let attempt = fixture.acknowledged_attempt(now_ms() - 25_000);
    let marker = fixture.queue_status_for("event-pane-closes", "pane-event");
    let mut worker = fixture.worker();
    let deadline = Instant::now() + Duration::from_secs(3);
    while marker.exists() {
        assert!(
            Instant::now() < deadline,
            "the worker must finish the event marker before waiting out the grace"
        );
        thread::sleep(Duration::from_millis(10));
    }
    fs::remove_file(fixture.root.join("pane-event-live")).unwrap();
    worker.finish();
    assert_eq!(fixture.attempt_state(&attempt), DeliveryState::Settled);
}

/// A hook that fires while the worker waits out a grace cannot start its own
/// worker, because this one holds the lock, so its marker has no reader but
/// this loop. The wait has to notice it instead of sleeping on.
#[test]
fn status_worker_serves_a_marker_written_during_a_grace_wait_without_delay() {
    let fixture = Fixture::new();
    fixture.initialize();
    fixture.set_herdr(&format!(
        "if [ \"$1 $2\" = 'pane list' ]; then\n\
           printf '%s\\n' '{{\"result\":{{\"panes\":[{{\"pane_id\":\"pane-recipient\",\"terminal_id\":\"terminal-1\",\"tab_id\":\"tab-1\",\"workspace_id\":\"workspace-1\",\"agent_session\":{{\"value\":\"session-1\"}},\"agent_status\":\"idle\"}},{{\"pane_id\":\"pane-event\",\"terminal_id\":\"terminal-2\",\"tab_id\":\"tab-2\",\"workspace_id\":\"workspace-1\"}}]}}}}'\n\
           exit 0\nfi\n{HERDR_SCRIPT}"
    ));
    let attempt = fixture.acknowledged_attempt(now_ms() - 24_000);
    let first = fixture.queue_status("turn-ended");
    let mut worker = fixture.worker();
    let deadline = Instant::now() + Duration::from_secs(3);
    while first.exists() {
        assert!(
            Instant::now() < deadline,
            "the first marker must finish before the grace wait"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let written = Instant::now();
    let second = fixture.queue_status_for("other-pane-idle", "pane-event");
    let deadline = Instant::now() + Duration::from_secs(8);
    while second.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    let served_after = written.elapsed();
    eprintln!("marker written during the grace wait was served after {served_after:?}");
    assert!(
        !second.exists() && served_after < Duration::from_secs(1),
        "a marker written during a grace wait was served after {served_after:?}"
    );
    worker.finish();
    assert_eq!(fixture.attempt_state(&attempt), DeliveryState::Settled);
}

fn temporary_prompt_rejection_retries_without_another_event(code: &str, rejections: usize) {
    let fixture = Fixture::new();
    fixture.initialize();
    fixture.connect_recipient();
    fixture.queue_message();
    let script = fs::read_to_string(fixture.root.join("herdr")).unwrap();
    fixture.set_herdr(&format!(
        r#"if [ "$1 $2" = 'agent prompt' ]; then
             printf 'x' >> "$NAMEDROP_TEST_FIXTURE/prompts"
             if [ "$(wc -c < "$NAMEDROP_TEST_FIXTURE/prompts")" -le {rejections} ]; then
             printf '%s\n' '{{"error":{{"code":"{code}"}}}}' >&2
             exit 1
             fi
             printf 'x' >> "$NAMEDROP_TEST_FIXTURE/accepted-prompts"
             fi
             {script}"#
    ));
    let rejected = fixture.reconcile_drain();
    assert_eq!(
        rejected["requeued"].as_array().map(Vec::len),
        Some(1),
        "{code}: {rejected}"
    );
    let deadline = Instant::now() + Duration::from_secs(if rejections > 1 { 30 } else { 5 });
    while fixture.prompt_counts().1 == 0 {
        assert!(
            Instant::now() < deadline,
            "{code} stranded without another event: {:?}",
            fixture.prompt_counts()
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fixture.prompt_counts(), (rejections + 1, 1), "{code}");
    let deadline = Instant::now() + Duration::from_secs(5);
    let inbox = loop {
        let inbox = fixture.inbox();
        if !inbox.is_empty() {
            break inbox;
        }
        assert!(
            Instant::now() < deadline,
            "{code}: accepted prompt was not recorded in the inbox"
        );
        thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(inbox.len(), 1, "{code}");
    assert_eq!(inbox[0]["untrusted_body"], "review", "{code}");
    let completed = fixture
        .command()
        .args(["complete", "--no-reply", "--input-json"])
        .arg(serde_json::json!({"attempt_id":inbox[0]["control"]["attempt_id"]}).to_string())
        .output()
        .unwrap();
    assert!(completed.status.success(), "{code}: {completed:?}");
    use sha2::{Digest, Sha256};
    let scope = namedrop::lower_hex(&Sha256::digest("test-socket\0workspace-1"));
    let directory = fixture
        .state()
        .join("agent-status-reconciliation")
        .join(scope);
    let deadline = Instant::now() + Duration::from_secs(5);
    while fs::read_dir(&directory)
        .unwrap()
        .filter_map(Result::ok)
        .any(|entry| entry.file_name().to_string_lossy().starts_with("pending-"))
    {
        assert!(
            Instant::now() < deadline,
            "{code}: settled worker still has pending work"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(fixture.prompt_counts(), (rejections + 1, 1), "{code}");
}

#[test]
fn server_busy_retries_without_another_event() {
    temporary_prompt_rejection_retries_without_another_event("server_busy", 1);
}

#[test]
fn conflict_retries_without_another_event() {
    temporary_prompt_rejection_retries_without_another_event("conflict", 1);
}

#[test]
fn agent_not_ready_retries_without_another_event() {
    temporary_prompt_rejection_retries_without_another_event("agent_not_ready", 1);
}

#[test]
fn repeated_eventless_rejections_deliver_when_the_recipient_becomes_ready() {
    temporary_prompt_rejection_retries_without_another_event("server_busy", 7);
}

fn wait_for_runtime(description: &str, timeout_secs: u64, mut observed: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(timeout_secs);
    while !observed() {
        assert!(Instant::now() < deadline, "{description}");
        thread::sleep(Duration::from_millis(10));
    }
}

fn worker_at_capped_eventless_retry(
    fixture: &Fixture,
    before_retry: impl FnOnce(),
) -> (Worker, PathBuf) {
    fixture.initialize();
    fixture.connect_recipient();
    fixture.queue_message();
    let script = fs::read_to_string(fixture.root.join("herdr")).unwrap();
    fixture.set_herdr(&format!(
        r#"if [ "$1 $2" = 'agent prompt' ]; then
             if [ "$3" = 'pane-grace' ]; then
               printf 'x' >> "$NAMEDROP_TEST_FIXTURE/other-prompts"
               printf '%s\n' '{{}}'
               exit 0
             fi
             printf 'x' >> "$NAMEDROP_TEST_FIXTURE/prompts"
             count=$(wc -c < "$NAMEDROP_TEST_FIXTURE/prompts")
             if [ "$count" -eq 8 ]; then
               touch "$NAMEDROP_TEST_FIXTURE/setup-entered"
               while [ ! -f "$NAMEDROP_TEST_FIXTURE/release-setup" ]; do sleep 0.01; done
               sleep 0.2
             fi
             if [ "$count" -eq 9 ]; then
               touch "$NAMEDROP_TEST_FIXTURE/capped-retry-entered"
               while [ ! -f "$NAMEDROP_TEST_FIXTURE/release-capped-retry" ]; do sleep 0.01; done
             fi
             if [ "$count" -eq 10 ] && [ -f "$NAMEDROP_TEST_FIXTURE/slow-handler-enabled" ]; then
               touch "$NAMEDROP_TEST_FIXTURE/slow-prompt-entered"
               sleep 1.25
               touch "$NAMEDROP_TEST_FIXTURE/slow-prompt-completed"
             fi
             printf '%s\n' '{{"error":{{"code":"server_busy"}}}}' >&2
             exit 1
           fi
           if [ "$1 $2" = 'pane list' ]; then
             printf '%s\n' '{{"result":{{"panes":[{{"pane_id":"pane-recipient","terminal_id":"terminal-1","tab_id":"tab-1","workspace_id":"workspace-1","agent":"claude","agent_session":{{"value":"session-1"}},"agent_status":"idle"}},{{"pane_id":"pane-grace","terminal_id":"terminal-2","tab_id":"tab-2","workspace_id":"workspace-1","agent":"claude","agent_session":{{"value":"session-grace"}},"agent_status":"idle"}}]}}}}'
             exit 0
           fi
           {script}"#
    ));
    let markers: Vec<_> = (0..8)
        .map(|number| fixture.queue_status(&format!("setup-{number}")))
        .collect();
    let worker = fixture.worker();
    wait_for_runtime("worker did not reach the queued-event barrier", 30, || {
        fixture.root.join("setup-entered").exists()
    });
    before_retry();
    fs::write(fixture.root.join("release-setup"), "ready").unwrap();
    wait_for_runtime("worker did not reach the capped retry barrier", 30, || {
        fixture.root.join("capped-retry-entered").exists()
    });
    assert_eq!(fixture.prompt_counts(), (9, 0));
    assert!(markers.iter().all(|marker| !marker.exists()));
    let pending: Vec<_> = fs::read_dir(markers[0].parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("pending-")
        })
        .collect();
    assert_eq!(pending.len(), 1);
    let input: serde_json::Value = serde_json::from_slice(&fs::read(&pending[0]).unwrap()).unwrap();
    assert_eq!(input, serde_json::json!({"operation": "mailbox-wake"}));
    (worker, pending.into_iter().next().unwrap())
}

#[test]
fn eventless_backoff_serves_new_markers_at_the_retry_cap() {
    let fixture = Fixture::new();
    let (_worker, capped_marker) = worker_at_capped_eventless_retry(&fixture, || {});
    fs::write(fixture.root.join("release-capped-retry"), "ready").unwrap();
    wait_for_runtime("capped retry did not finish its marker", 30, || {
        !capped_marker.exists()
    });
    assert_eq!(fixture.prompt_counts(), (9, 0));

    fs::write(fixture.root.join("slow-handler-enabled"), "ready").unwrap();
    let started = Instant::now();
    let fresh = fixture.queue_status("fresh-during-capped-wait");
    wait_for_runtime(
        "fresh marker was blocked by the 60 s eventless backoff",
        10,
        || !fresh.exists(),
    );
    assert!(fixture.root.join("slow-prompt-entered").exists());
    assert!(fixture.root.join("slow-prompt-completed").exists());
    assert_eq!(fixture.prompt_counts(), (10, 0));
    assert!(started.elapsed() >= Duration::from_secs(1));
    eprintln!(
        "fresh marker consumed after slow handler at {:?}, prompts={:?}",
        started.elapsed(),
        fixture.prompt_counts()
    );
}

#[test]
fn eventless_backoff_serves_other_recipients_grace_at_the_retry_cap() {
    let fixture = Fixture::new();
    let namespace = fixture.namespace();
    let attempt = DeliveryAttemptId::new("grace-attempt");
    let (_worker, capped_marker) = worker_at_capped_eventless_retry(&fixture, || {
        let created = now_ms() - 25_000;
        let attribution = MutationAttribution::new(ActorKind::Plugin, "fixture", created);
        StateStore::open(fixture.state())
            .unwrap()
            .transaction(|tx| {
                tx.bind_pane(
                    &namespace,
                    &PaneBindingId::new("binding-grace"),
                    &PaneAddressId::new("author"),
                    "pane-grace",
                    &attribution,
                )?;
                tx.observe_agent_session(
                    &namespace,
                    "grace-observation",
                    &PaneAddressId::new("author"),
                    &AgentSessionId::new("session-grace"),
                    "idle",
                    &attribution,
                )?;
                tx.append_message(
                    &namespace,
                    &MessageDraft {
                        thread_id: ThreadId::new("thread-1"),
                        message_id: MessageId::new("grace-message"),
                        in_reply_to: None,
                        author_address_id: PaneAddressId::new("recipient"),
                        author_kind: ActorKind::Human,
                        author_alias_snapshot: "recipient".to_owned(),
                        author_agent_session_snapshot: None,
                        request_id: "grace-request".to_owned(),
                        authored_source: "@author review".to_owned(),
                        body: "review".to_owned(),
                        grammar_version: "mention/v1".to_owned(),
                        recipients: vec![MessageRecipientDraft {
                            address_id: PaneAddressId::new("author"),
                            alias_snapshot: "author".to_owned(),
                            agent_session_snapshot: Some(AgentSessionId::new("session-grace")),
                            delivery_id: DeliveryId::new("grace-delivery"),
                        }],
                        context_references: vec![],
                    },
                    &attribution,
                )?;
                assert!(
                    tx.lease_mailbox_head(
                        &namespace,
                        &PaneAddressId::new("author"),
                        &LeaseDraft {
                            attempt_id: attempt.clone(),
                            binding_id: PaneBindingId::new("binding-grace"),
                            agent_session_id: AgentSessionId::new("session-grace"),
                            retry_of_attempt_id: None,
                            owner: "fixture".to_owned(),
                            acquired_at_ms: created,
                            expires_at_ms: created + 30_000,
                        },
                        &attribution,
                    )?
                    .is_some()
                );
                tx.transition_delivery_attempt(
                    &namespace,
                    &attempt,
                    DeliveryState::Submitted,
                    &attribution,
                )?;
                tx.acknowledge_delivery_attempt(
                    &namespace,
                    &attempt,
                    "grace-receipt",
                    &AgentSessionId::new("session-grace"),
                    &attribution,
                )?;
                Ok(())
            })
            .unwrap();
    });
    assert_eq!(fixture.attempt_state(&attempt), DeliveryState::Acknowledged);
    fs::write(fixture.root.join("release-capped-retry"), "ready").unwrap();
    wait_for_runtime("capped retry did not finish its marker", 30, || {
        !capped_marker.exists()
    });
    wait_for_runtime(
        "60 s eventless backoff delayed another recipient's grace",
        10,
        || fixture.attempt_state(&attempt) == DeliveryState::Settled,
    );
    let facts = StateStore::open_read_only(fixture.state())
        .unwrap()
        .unwrap()
        .transaction(|tx| tx.transition_facts(&namespace))
        .unwrap();
    let transition_time = |kind| {
        facts
            .iter()
            .find(|fact| {
                fact.entity_kind == namedrop::TransitionEntityKind::DeliveryAttempt
                    && fact.entity_id == attempt.as_str()
                    && fact.transition_kind == kind
            })
            .unwrap()
            .occurred_at_ms
    };
    let due = transition_time(namedrop::TransitionKind::Acknowledged) + 30_000;
    let settled = transition_time(namedrop::TransitionKind::Settled);
    assert!(settled >= due, "grace settled before its deadline");
    assert!(
        settled < due + 10_000,
        "grace settlement exceeded its watchdog"
    );
    eprintln!(
        "independent grace settled {} ms after its deadline without another marker, prompts={:?}",
        settled - due,
        fixture.prompt_counts()
    );
}

#[test]
fn eventless_rejections_keep_pane_lock_deferrals_on_the_short_retry() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::OpenOptionsExt;

    let fixture = Fixture::new();
    let namespace = fixture.namespace();
    let mut pane_lock = None;
    let (_worker, mixed_marker) = worker_at_capped_eventless_retry(&fixture, || {
        let attribution = MutationAttribution::new(ActorKind::Plugin, "fixture", now_ms());
        StateStore::open(fixture.state())
            .unwrap()
            .transaction(|tx| {
                tx.bind_pane(
                    &namespace,
                    &PaneBindingId::new("binding-other"),
                    &PaneAddressId::new("author"),
                    "pane-grace",
                    &attribution,
                )?;
                tx.observe_agent_session(
                    &namespace,
                    "other-observation",
                    &PaneAddressId::new("author"),
                    &AgentSessionId::new("session-grace"),
                    "idle",
                    &attribution,
                )?;
                tx.append_message(
                    &namespace,
                    &MessageDraft {
                        thread_id: ThreadId::new("thread-1"),
                        message_id: MessageId::new("other-message"),
                        in_reply_to: None,
                        author_address_id: PaneAddressId::new("recipient"),
                        author_kind: ActorKind::Human,
                        author_alias_snapshot: "recipient".to_owned(),
                        author_agent_session_snapshot: None,
                        request_id: "other-request".to_owned(),
                        authored_source: "@author review".to_owned(),
                        body: "review".to_owned(),
                        grammar_version: "mention/v1".to_owned(),
                        recipients: vec![MessageRecipientDraft {
                            address_id: PaneAddressId::new("author"),
                            alias_snapshot: "author".to_owned(),
                            agent_session_snapshot: Some(AgentSessionId::new("session-grace")),
                            delivery_id: DeliveryId::new("other-delivery"),
                        }],
                        context_references: vec![],
                    },
                    &attribution,
                )?;
                Ok(())
            })
            .unwrap();
        let key = namedrop::lower_hex(&Sha256::digest(
            serde_json::to_vec(&("test-socket", "pane-grace")).unwrap(),
        ));
        let lock = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(
                fixture
                    .state()
                    .join("pane-effects")
                    .join(format!("{key}.lock")),
            )
            .unwrap();
        lock.try_lock().unwrap();
        pane_lock = Some(lock);
        let challenge = StateStore::open(fixture.state())
            .unwrap()
            .transaction(|tx| {
                tx.issue_integration_challenge(
                    &namespace,
                    "registration-other",
                    &PaneAddressId::new("author"),
                    namedrop::AgentRuntime::ClaudeCode,
                    "nonce-other",
                    &attribution,
                )
            })
            .unwrap();
        let registered = fixture
            .command()
            .env("HERDR_PANE_ID", "pane-grace")
            .args(["register", "--input-json"])
            .arg(
                serde_json::json!({
                    "runtime": "claude-code",
                    "protocol_version": "collab/v1",
                    "nonce": challenge.nonce,
                    "capabilities": namedrop::complete_capability_profile(),
                })
                .to_string(),
            )
            .output()
            .unwrap();
        assert!(registered.status.success(), "{registered:?}");
        let scope = namedrop::lower_hex(&Sha256::digest("test-socket\0workspace-1"));
        let directory = fixture
            .state()
            .join("agent-status-reconciliation")
            .join(scope);
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_string_lossy();
            if name.starts_with("pending-") && !name.starts_with("pending-setup-") {
                fs::remove_file(path).unwrap();
            }
        }
    });
    fs::write(fixture.root.join("release-capped-retry"), "ready").unwrap();
    wait_for_runtime("mixed drain did not finish its marker", 30, || {
        !mixed_marker.exists()
    });
    assert!(!fixture.root.join("other-prompts").exists());
    drop(pane_lock);
    let released = Instant::now();
    wait_for_runtime(
        "rejection backoff delayed a released pane lock beyond 5 s",
        8,
        || fixture.root.join("other-prompts").exists(),
    );
    assert_eq!(fs::read(fixture.root.join("other-prompts")).unwrap(), b"x");
    eprintln!(
        "deferred recipient prompted {:?} after pane lock release",
        released.elapsed()
    );
}
