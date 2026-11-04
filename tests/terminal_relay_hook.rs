#[path = "support/cleanup.rs"]
mod cleanup;

use namedrop::{
    ActorKind, AgentRuntime, AgentSessionId, Clock, DeliveryAttemptId, DeliveryId, DeliveryState,
    LeaseDraft, MessageDraft, MessageId, MessageRecipientDraft, MutationAttribution,
    PaneAddressDraft, PaneAddressId, PaneBindingId, PaneRoleId, StateNamespace, StateStore,
    SystemClock, ThreadId, install_integration_package,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    os::unix::fs::PermissionsExt,
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop stop {}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("config")).unwrap();
        Self(root)
    }

    fn command(&self, program: impl AsRef<std::ffi::OsStr>) -> Command {
        let mut command = Command::new(program);
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.0.join("home"))
            .env("XDG_CONFIG_HOME", self.0.join("config"));
        command
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        cleanup::remove_fixture(&self.0).unwrap();
    }
}

#[test]
fn both_installed_stop_hooks_capture_the_boundary_before_returning_while_the_worker_runs() {
    for runtime in [AgentRuntime::ClaudeCode, AgentRuntime::Codex] {
        let fixture = Fixture::new();
        fixture.initialize_relay();
        let herdr = fixture.0.join("herdr");
        fs::write(&herdr, "#!/bin/sh\ntouch ready\ni=0\nwhile [ ! -e release ] && [ \"$i\" -lt 200 ]; do sleep 0.01; i=$((i+1)); done\ntouch finished\nexit 1\n").unwrap();
        let package = install_integration_package(
            &fixture.0.join("home"),
            runtime,
            std::path::Path::new(env!("CARGO_BIN_EXE_namedrop")),
            &fixture.state(),
        )
        .unwrap();
        let manifest: Value =
            serde_json::from_slice(&fs::read(package.join("hooks/hooks.json")).unwrap()).unwrap();
        assert!(manifest["hooks"]["SessionStart"].is_array());
        assert!(manifest["hooks"]["UserPromptSubmit"].is_array());
        let launch = manifest["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        let identity = match runtime {
            AgentRuntime::ClaudeCode => "prompt_id",
            AgentRuntime::Codex => "turn_id",
        };
        let mut payload = json!({"hook_event_name":"Stop", "session_id":"session", "last_assistant_message":"Which one?"});
        payload[identity] = json!("first-turn");
        let mut child = fixture
            .command("/bin/sh")
            .env("HERDR_SOCKET_PATH", "test-socket")
            .env("HERDR_BIN_PATH", &herdr)
            .args(["-c", launch])
            .current_dir(&fixture.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        let sessions: Vec<_> = fs::read_dir(fixture.state().join("terminal-relay-stops"))
            .unwrap()
            .collect();
        assert_eq!(sessions.len(), 1);
        assert!(
            sessions[0]
                .as_ref()
                .unwrap()
                .path()
                .join("latest.json")
                .is_file()
        );
        assert!(
            !fixture.0.join("finished").exists(),
            "Stop waited for Herdr"
        );
        for _ in 0..200 {
            if fixture.0.join("ready").exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(fixture.0.join("ready").exists());
        fs::write(fixture.0.join("release"), "").unwrap();
        for _ in 0..200 {
            if fixture.0.join("finished").exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(fixture.0.join("finished").exists());
    }
}

#[test]
fn stop_command_is_silent_and_does_not_create_state_when_unavailable() {
    let fixture = Fixture::new();
    for input in [
        "not json".to_owned(),
        json!({"hook_event_name":"Stop", "session_id":"test", "prompt_id":"prompt", "last_assistant_message":"hello"})
            .to_string(),
    ] {
        let state = fixture.0.join("missing-state");
        let mut child = fixture
            .command(env!("CARGO_BIN_EXE_namedrop"))
            .args(["hook", "stop", "--state-directory"])
            .arg(&state)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        assert!(!state.exists());
    }
}

impl Fixture {
    fn state(&self) -> PathBuf {
        self.0.join("state")
    }

    fn namespace(&self) -> StateNamespace {
        StateNamespace::for_space("test-socket", "workspace")
    }

    fn set_herdr(&self, available: bool) {
        let script = if available {
            "#!/bin/sh\nprintf '%s' \"$PPID\" > \"$STOP_TEST_WORKER_PID\"\nprintf '%s\\n' '{\"result\":{\"panes\":[{\"pane_id\":\"pane\",\"terminal_id\":\"terminal\",\"tab_id\":\"tab\",\"workspace_id\":\"workspace\",\"agent_session\":{\"value\":\"session\"}}]}}'\n"
        } else {
            "#!/bin/sh\nprintf '%s' \"$PPID\" > \"$STOP_TEST_WORKER_PID\"\nexit 1\n"
        };
        fs::write(self.0.join("herdr"), script).unwrap();
        fs::set_permissions(self.0.join("herdr"), fs::Permissions::from_mode(0o700)).unwrap();
    }

    fn initialize_relay(&self) {
        self.set_herdr(true);
        let ns = self.namespace();
        let at = MutationAttribution::new(ActorKind::Plugin, "fixture", SystemClock.now_unix_ms());
        StateStore::open(self.state())
            .unwrap()
            .transaction(|tx| {
                tx.activate_worktree(&ns, &at)?;
                tx.create_pane_address(
                    &ns,
                    &PaneAddressDraft {
                        id: PaneAddressId::new("agent"),
                        role_id: PaneRoleId::new("role"),
                        role_name: "agent".into(),
                        role_description: "fixture".into(),
                        alias: "agent".into(),
                    },
                    &at,
                )?;
                tx.bind_pane(
                    &ns,
                    &PaneBindingId::new("binding"),
                    &PaneAddressId::new("agent"),
                    "pane",
                    &at,
                )?;
                tx.observe_agent_session(
                    &ns,
                    "observation",
                    &PaneAddressId::new("agent"),
                    &AgentSessionId::new("session"),
                    "idle",
                    &at,
                )?;
                tx.create_thread(&ns, &ThreadId::new("thread"), "Fixture", &at)?;
                Ok(())
            })
            .unwrap();
        let connection = rusqlite::Connection::open(self.state().join("namedrop.sqlite3")).unwrap();
        connection.execute_batch("INSERT INTO pane_roles (session_socket_id, worktree_id, role_id, name, description) VALUES ('test-socket','workspace','role-operator','operator','Paneless local operator.'); INSERT INTO pane_addresses (session_socket_id, worktree_id, address_id, role_id, retired, collaboration_disabled, created_at_ms) VALUES ('test-socket','workspace','operator:local','role-operator',0,0,1);").unwrap();
        self.submit("first");
    }

    fn submit(&self, id: &str) {
        let ns = self.namespace();
        let now = SystemClock.now_unix_ms();
        let at = MutationAttribution::new(ActorKind::Plugin, "fixture", now);
        StateStore::open(self.state())
            .unwrap()
            .transaction(|tx| {
                tx.append_message(
                    &ns,
                    &MessageDraft {
                        thread_id: ThreadId::new("thread"),
                        message_id: MessageId::new(id),
                        in_reply_to: None,
                        author_address_id: PaneAddressId::new("operator:local"),
                        author_kind: ActorKind::Human,
                        author_alias_snapshot: "operator".into(),
                        author_agent_session_snapshot: None,
                        request_id: id.into(),
                        authored_source: "Please choose".into(),
                        body: "Please choose".into(),
                        grammar_version: "channel/v1".into(),
                        recipients: vec![MessageRecipientDraft {
                            address_id: PaneAddressId::new("agent"),
                            alias_snapshot: "agent".into(),
                            agent_session_snapshot: None,
                            delivery_id: DeliveryId::new(format!("delivery-{id}")),
                        }],
                        context_references: Vec::new(),
                    },
                    &at,
                )?;
                let attempt = DeliveryAttemptId::new(id);
                tx.lease_mailbox_head(
                    &ns,
                    &PaneAddressId::new("agent"),
                    &LeaseDraft {
                        attempt_id: attempt.clone(),
                        binding_id: PaneBindingId::new("binding"),
                        agent_session_id: AgentSessionId::new("session"),
                        retry_of_attempt_id: None,
                        owner: "fixture".into(),
                        acquired_at_ms: now,
                        expires_at_ms: now + 30_000,
                    },
                    &at,
                )?
                .expect("operator request is leased");
                tx.transition_delivery_attempt(&ns, &attempt, DeliveryState::Submitted, &at)
            })
            .unwrap();
    }

    fn boundary(&self) -> Option<Vec<u8>> {
        let session = fs::read_dir(self.state().join("terminal-relay-stops"))
            .ok()?
            .next()?
            .ok()?;
        fs::read(session.path().join("latest.json")).ok()
    }

    fn stop(&self, identity_key: &str, identity: &str, text: &str) {
        let previous_boundary = self.boundary();
        let worker_pid_file = self.0.join("worker-pid");
        let _ = fs::remove_file(&worker_pid_file);
        let mut payload = json!({"hook_event_name":"Stop", "session_id":"session", "last_assistant_message":text});
        payload[identity_key] = json!(identity);
        let mut child = self
            .command(env!("CARGO_BIN_EXE_namedrop"))
            .env("HERDR_SOCKET_PATH", "test-socket")
            .env("STOP_TEST_WORKER_PID", &worker_pid_file)
            .env("HERDR_BIN_PATH", self.0.join("herdr"))
            .args(["hook", "stop", "--state-directory"])
            .arg(self.state())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        if self.boundary() == previous_boundary {
            assert!(
                !worker_pid_file.exists(),
                "duplicate Stop launched a worker"
            );
            return;
        }
        for _ in 0..200 {
            if worker_pid_file.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
        let pid = fs::read_to_string(worker_pid_file).expect("captured Stop launched its worker");
        {
            for _ in 0..200 {
                let stat = fs::read_to_string(format!("/proc/{pid}/stat"));
                if stat
                    .as_ref()
                    .map_or(true, |value| value.split_whitespace().nth(2) == Some("Z"))
                {
                    return;
                }
                thread::sleep(Duration::from_millis(5));
            }
            panic!("Stop worker did not exit");
        }
    }

    fn relays(&self) -> Vec<(String, String)> {
        let connection = rusqlite::Connection::open(self.state().join("namedrop.sqlite3")).unwrap();
        connection.prepare("SELECT message_id, body FROM collaboration_messages WHERE grammar_version='terminal-relay/v1' ORDER BY thread_order").unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
            .collect::<Result<_, _>>().unwrap()
    }
}

#[test]
fn a_missed_stop_cannot_relay_an_unrelated_later_answer() {
    for identity_key in ["prompt_id", "turn_id"] {
        for (database_busy, first_text) in [
            (false, "First question?"),
            (true, "First question?"),
            (false, ""),
            (true, ""),
        ] {
            let fixture = Fixture::new();
            fixture.initialize_relay();
            let connection =
                rusqlite::Connection::open(fixture.state().join("namedrop.sqlite3")).unwrap();
            if database_busy {
                connection.execute_batch("BEGIN IMMEDIATE").unwrap();
            } else {
                fixture.set_herdr(false);
            }
            fixture.stop(identity_key, "missed-turn", first_text);
            if database_busy {
                connection.execute_batch("ROLLBACK").unwrap();
            }
            fixture.set_herdr(true);
            thread::sleep(Duration::from_millis(2));
            fixture.stop(identity_key, "unrelated-turn", "Unrelated answer");
            assert!(fixture.relays().is_empty());
        }
    }
}

#[test]
fn replaying_a_native_stop_cannot_consume_the_next_delivery() {
    for identity_key in ["prompt_id", "turn_id"] {
        let fixture = Fixture::new();
        fixture.initialize_relay();
        fixture.stop(identity_key, "first-turn", "First question?");
        assert_eq!(
            fixture.relays(),
            [("terminal-relay:first".into(), "First question?".into())]
        );
        thread::sleep(Duration::from_millis(2));
        fixture.submit("second");
        fixture.stop(identity_key, "first-turn", "First question?");
        assert_eq!(fixture.relays().len(), 1);
        fixture.stop(identity_key, "second-turn", "Second question?");
        assert_eq!(
            fixture.relays(),
            [
                ("terminal-relay:first".into(), "First question?".into()),
                ("terminal-relay:second".into(), "Second question?".into()),
            ]
        );
        thread::sleep(Duration::from_millis(2));
        fixture.submit("third");
        fixture.stop(identity_key, "first-turn", "First question?");
        assert_eq!(fixture.relays().len(), 2);
        fixture.stop(identity_key, "third-turn", "Third question?");
        assert_eq!(
            fixture.relays().last().unwrap(),
            &("terminal-relay:third".into(), "Third question?".into())
        );
    }
}
