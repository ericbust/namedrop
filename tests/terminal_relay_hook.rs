#[path = "support/cleanup.rs"]
mod cleanup;

use namedrop::{
    ActorKind, AgentRuntime, AgentSessionId, Clock, DeliveryAttemptId, DeliveryId, DeliveryState,
    LeaseDraft, MessageDraft, MessageId, MessageRecipientDraft, MutationAttribution,
    PaneAddressDraft, PaneAddressId, PaneBindingId, PaneRoleId, StateNamespace, StateStore,
    SystemClock, ThreadId, final_reply::HostTurn, install_integration_package,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    os::{
        fd::AsRawFd,
        unix::{fs::PermissionsExt, net::UnixStream, process::CommandExt},
    },
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

/// A claim as the prompt hook arms it, for a Stop of `turn` to settle.
fn arm_claim(fixture: &Fixture, turn: &HostTurn) {
    let key = serde_json::to_vec(&("test-socket", "session")).unwrap();
    let session: String = Sha256::digest(key)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let directory = fixture.state().join("final-reply").join(session);
    fs::create_dir_all(&directory).unwrap();
    let claim = json!([{
        "attempt_id": "first",
        "namespace": fixture.namespace(),
        "location": {"state": fixture.state(), "socket": "test-socket",
                     "workspace": "workspace", "pane": "pane"},
        "turn": turn.as_str(),
        "plan": {"no_reply": "answers_own"},
        "state": "open",
        "armed_at_ms": SystemClock.now_unix_ms(),
        "updated_at_ms": SystemClock.now_unix_ms(),
    }]);
    fs::write(directory.join("claims.json"), claim.to_string()).unwrap();
}

#[test]
fn both_installed_stop_hooks_return_before_the_worker_settles_a_claim() {
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
        let (identity, turn) = match runtime {
            AgentRuntime::ClaudeCode => ("prompt_id", HostTurn::from_ids(Some("first-turn"), None)),
            AgentRuntime::Codex => ("turn_id", HostTurn::from_ids(None, Some("first-turn"))),
        };
        arm_claim(&fixture, &turn.unwrap());
        let mut payload = json!({"hook_event_name":"Stop", "session_id":"session", "last_assistant_message":"Thanks, noted."});
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
        assert!(fixture.0.join("ready").exists(), "the worker never settled");
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

    fn stop(&self, identity_key: &str, identity: &str, text: &str) {
        let worker_pid_file = self.0.join("worker-pid");
        let _ = fs::remove_file(&worker_pid_file);
        let (mut worker_done, worker_lifetime) = UnixStream::pair().unwrap();
        let worker_fd = worker_lifetime.as_raw_fd();
        let payload = format!(
            "{{\"hook_event_name\":\"Stop\",\"session_id\":\"session\",\"{identity_key}\":{},\"last_assistant_message\":{}}}",
            serde_json::to_string(identity).unwrap(),
            serde_json::to_string(text).unwrap(),
        );
        let mut command = self.command(env!("CARGO_BIN_EXE_namedrop"));
        // Inherit this descriptor through Stop and its detached worker; EOF waits for both.
        unsafe {
            command.pre_exec(move || {
                let flags = libc::fcntl(worker_fd, libc::F_GETFD);
                if flags == -1
                    || libc::fcntl(worker_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) == -1
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command
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
        drop(worker_lifetime);
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.as_bytes())
            .unwrap_or_else(|error| {
                assert!(
                    payload.len() > 1024 * 1024 && error.kind() == std::io::ErrorKind::BrokenPipe,
                    "{error}"
                );
            });
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert!(output.stdout.is_empty());
        assert!(output.stderr.is_empty());
        let mut completion = libc::pollfd {
            fd: worker_done.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        assert_eq!(
            unsafe { libc::poll(&mut completion, 1, 5000) },
            1,
            "Stop completion wait failed or timed out"
        );
        assert_ne!(
            completion.revents & (libc::POLLIN | libc::POLLHUP),
            0,
            "unexpected Stop completion events: {}",
            completion.revents
        );
        assert_eq!(
            worker_done
                .read(&mut [0])
                .expect("Stop completion read failed"),
            0,
            "unexpected Stop completion bytes"
        );
        assert!(
            !worker_pid_file.exists(),
            "a Stop with no claim reached Herdr"
        );
    }

    fn relays(&self) -> Vec<(String, String)> {
        let connection = rusqlite::Connection::open(self.state().join("namedrop.sqlite3")).unwrap();
        connection.prepare("SELECT message_id, body FROM collaboration_messages WHERE grammar_version='terminal-relay/v1' ORDER BY thread_order").unwrap()
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?))).unwrap()
            .collect::<Result<_, _>>().unwrap()
    }
}

/// Only a claim the prompt hook armed settles anything. A Stop whose turn
/// armed none, such as one the person typed into the pane, posts nothing
/// even while a message from the person is outstanding.
#[test]
fn a_stop_without_a_claim_relays_nothing() {
    for identity_key in ["prompt_id", "turn_id"] {
        let fixture = Fixture::new();
        fixture.initialize_relay();
        fixture.stop(identity_key, "typed-turn", "Which file did you change?");
        fixture.stop(
            identity_key,
            "oversized-turn",
            &"x".repeat(1024 * 1024 + 100),
        );
        assert!(fixture.relays().is_empty(), "{:?}", fixture.relays());
        assert!(!fixture.state().join("final-reply").exists());
    }
}
