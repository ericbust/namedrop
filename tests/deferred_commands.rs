#![cfg(all(target_os = "linux", feature = "test-support"))]
#[path = "support/cleanup.rs"]
mod cleanup;
#[path = "support/executable.rs"]
mod executable;
mod support;
use namedrop::mux::fake::{EVERY_LISTING, FakeMux, StrictSpace, Takes};
use namedrop::{
    ActorKind, AgentRuntime, AgentSessionId, Clock, DeliveryAttemptId, DeliveryId, DeliveryState,
    HerdrCommandExecutor, HerdrCommandOutput, LeaseDraft, LifecycleRuntime, MessageDraft,
    MessageId, MessageRecipientDraft, MutationAttribution, PaneAddressId, PaneBindingId,
    ProcessIds, StateNamespace, StateStore, SystemClock, ThreadId, complete_capability_profile,
    execute_integration_operation,
};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::{fs::PermissionsExt, net::UnixListener, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};
use support::{TestDirectory, address};

struct Fixture {
    directory: TestDirectory,
    socket: String,
    herdr: PathBuf,
    _listener: UnixListener,
}

/// The reminder clock test's in-process event runs the script, because
/// its transient case relies on this call writing
/// `observed-discovery-error`. The clock subprocess lists only by chance:
/// its due reminders already await delivery when the error appears, so on a
/// `FakeMux` that wait usually times out.
struct ScriptHerdr<'a>(&'a Path);
impl HerdrCommandExecutor for ScriptHerdr<'_> {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let output = Command::new(self.0)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.0.parent().unwrap().join("home"))
            .env(
                "XDG_CONFIG_HOME",
                self.0.parent().unwrap().join("xdg-config"),
            )
            .args(args)
            .output()
            .map_err(|error| error.to_string())?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        Ok(HerdrCommandOutput {
            stdout: String::from_utf8(output.stdout).map_err(|error| error.to_string())?,
        })
    }
}

/// The world the old in-process executor answered: `/repo` open in
/// workspace-1 and one idle pane, `pane-author`, that reports a session but no
/// agent kind. It answers every `worktree list` and `pane list`, as the old
/// executor matched them with any arguments, and takes no input or effect.
fn native(socket: &str) -> StrictSpace {
    let mux = FakeMux::world()
        .endpoint(socket)
        .space("workspace-1")
        .checkout("/repo")
        .unstamped()
        .pane("pane-author")
        .terminal("terminal-author")
        .tab("tab-1")
        .session("session-author")
        .idle()
        .build();
    StrictSpace::new(mux, EVERY_LISTING, Takes::nothing())
}

fn panes(session: &str) -> Value {
    json!({"result":{"panes":[{"pane_id":"pane-author","terminal_id":"terminal-author",
        "tab_id":"tab-1","workspace_id":"workspace-1","agent_session":{"value":session},"agent_status":"idle"}]}})
}

impl Fixture {
    fn new() -> Self {
        let directory = TestDirectory::new();
        let socket = directory
            .path()
            .join("herdr.sock")
            .to_str()
            .unwrap()
            .to_owned();
        let listener = UnixListener::bind(&socket).unwrap();
        // Accept probes so the fake socket's backlog cannot stall the worker.
        listener.set_nonblocking(true).unwrap();
        let acceptor = listener.try_clone().unwrap();
        let socket_path = PathBuf::from(&socket);
        thread::spawn(move || {
            while socket_path.exists() {
                while acceptor.accept().is_ok() {}
                thread::sleep(Duration::from_millis(10));
            }
        });
        let namespace = StateNamespace::for_space(&socket, "workspace-1");
        let attribution = MutationAttribution::new(ActorKind::Human, "test", 1);
        StateStore::open(directory.path())
            .unwrap()
            .transaction(|tx| {
                tx.activate_worktree(&namespace, &attribution)?;
                tx.create_pane_address(&namespace, &address("author", "author"), &attribution)?;
                tx.create_pane_address(&namespace, &address("reviewer", "reviewer"), &attribution)?;
                tx.bind_pane(
                    &namespace,
                    &PaneBindingId::new("binding"),
                    &PaneAddressId::new("author"),
                    "pane-author",
                    &attribution,
                )?;
                tx.observe_agent_session(
                    &namespace,
                    "observation",
                    &PaneAddressId::new("author"),
                    &AgentSessionId::new("session-author"),
                    "idle",
                    &attribution,
                )?;
                tx.issue_integration_challenge(
                    &namespace,
                    "registration",
                    &PaneAddressId::new("author"),
                    AgentRuntime::Codex,
                    "nonce",
                    &attribution,
                )?;
                Ok(())
            })
            .unwrap();
        execute_integration_operation("register", json!({"runtime":"codex","protocol_version":"collab/v1","nonce":"nonce","capabilities":complete_capability_profile()}), &LifecycleRuntime {
            state_directory:directory.path(), home_directory:None, agent_config:Default::default(),
            cwd:Path::new("/repo"), workspace_id:Some("workspace-1"), current_pane_id:Some("pane-author"),
            session_socket_id:&socket, mux:&*native(&socket), clock:&SystemClock, ids:&ProcessIds::default(),
        }).unwrap();
        fs::write(
            directory.path().join("panes.json"),
            panes("session-author").to_string(),
        )
        .unwrap();
        let herdr = directory.path().join("herdr");
        executable::write_executable(
            &herdr,
            r#"#!/usr/bin/env python3
import json, sys
from pathlib import Path
root = Path(__file__).parent
args = sys.argv[1:]
with (root / 'herdr-calls').open('a') as f: f.write(json.dumps(args) + '\n')
if args[:2] == ['--socket', str(root / 'herdr.sock')]: args = args[2:]
if args[:2] == ['worktree','list'] and (root / 'discovery-error').exists():
    error = (root / 'discovery-error').read_text()
    print(error, file=sys.stderr)
    (root / 'observed-discovery-error').write_text(error)
    sys.exit(1)
if args[:2] == ['worktree','list']: print(json.dumps({'result':{'worktrees':[{'path':'/repo','open_workspace_id':'workspace-1'}]}}))
elif args[:2] == ['workspace','list']: print(json.dumps({'result':{'workspaces':[{'workspace_id':'workspace-1'}]}}))
elif args[:2] == ['pane','list']: print((root / 'panes.json').read_text())
else: print('{}')
"#,
        );
        Self {
            directory,
            socket,
            herdr,
            _listener: listener,
        }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
        command
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", self.directory.home())
            .env("XDG_CONFIG_HOME", self.directory.path().join("xdg-config"))
            .env("HERDR_BIN_PATH", &self.herdr)
            .current_dir(self.directory.path());
        command
    }

    fn reminder(&self, request: &str, delay: &str) -> Value {
        response(
            self.command()
                .arg("--plugin-state")
                .arg(self.directory.path())
                .args([
                    "reminder",
                    "set",
                    "--herdr-socket",
                    &self.socket,
                    "--herdr-workspace",
                    "workspace-1",
                    "--herdr-pane",
                    "pane-author",
                    "--input-json",
                ])
                .arg(json!({"in":delay,"body":"test wake","request_id":request}).to_string())
                .output()
                .unwrap(),
        )["data"]
            .clone()
    }

    fn cancel_reminder(&self, id: &Value) {
        response(
            self.command()
                .arg("--plugin-state")
                .arg(self.directory.path())
                .args([
                    "reminder",
                    "cancel",
                    "--herdr-socket",
                    &self.socket,
                    "--herdr-workspace",
                    "workspace-1",
                    "--herdr-pane",
                    "pane-author",
                    "--input-json",
                ])
                .arg(json!({"reminder_id":id}).to_string())
                .output()
                .unwrap(),
        );
    }

    fn wake_mailboxes(&self) {
        let output = self
            .command()
            .env("HERDR_SOCKET_PATH", &self.socket)
            .env("HERDR_WORKSPACE_ID", "workspace-1")
            .args(["hook", "mailbox-wake", "--state-directory"])
            .arg(self.directory.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "{:?}", output);
        assert!(output.stdout.is_empty());
    }

    fn clock(&self) -> Worker {
        Worker(
            self.command()
                .arg("reminder-clock")
                .arg(self.directory.path())
                .arg(&self.socket)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    /// The full result, which carries the spool fields these tests inspect.
    fn sandbox_command(&self, operation: &[&str], input: Value) -> Output {
        self.sandbox_command_with_read_only_state(&[operation, &["--json"]].concat(), input, false)
    }

    fn sandbox_command_with_read_only_state(
        &self,
        operation: &[&str],
        input: Value,
        read_only: bool,
    ) -> Output {
        let mut command = self.command();
        command
            .arg("--plugin-state")
            .arg(self.directory.path())
            .args(operation)
            .args([
                "--herdr-socket",
                &self.socket,
                "--herdr-workspace",
                "workspace-1",
                "--herdr-pane",
                "pane-author",
                "--input-json",
                &input.to_string(),
            ]);
        // This is the syscall boundary Codex denies. No test-only production switch.
        unsafe {
            command.pre_exec(move || {
                let mut filter = vec![
                    libc::sock_filter {
                        code: 0x20,
                        jt: 0,
                        jf: 0,
                        k: 0,
                    },
                    libc::sock_filter {
                        code: 0x15,
                        jt: 0,
                        jf: 1,
                        k: libc::SYS_connect as u32,
                    },
                    libc::sock_filter {
                        code: 0x06,
                        jt: 0,
                        jf: 0,
                        k: 0x0005_0000 | libc::EPERM as u32,
                    },
                    libc::sock_filter {
                        code: 0x06,
                        jt: 0,
                        jf: 0,
                        k: 0x7fff_0000,
                    },
                ];
                if read_only {
                    filter.pop();
                    for syscall in [
                        #[cfg(not(any(
                            target_arch = "aarch64",
                            target_arch = "csky",
                            target_arch = "loongarch64",
                            target_arch = "riscv32",
                            target_arch = "riscv64",
                        )))]
                        libc::SYS_chmod,
                        libc::SYS_fchmod,
                        libc::SYS_fchmodat,
                        libc::SYS_mkdirat,
                    ] {
                        filter.push(libc::sock_filter {
                            code: 0x15,
                            jt: 0,
                            jf: 1,
                            k: syscall as u32,
                        });
                        filter.push(libc::sock_filter {
                            code: 0x06,
                            jt: 0,
                            jf: 0,
                            k: 0x0005_0000 | libc::EROFS as u32,
                        });
                    }
                    filter.push(libc::sock_filter {
                        code: 0x06,
                        jt: 0,
                        jf: 0,
                        k: 0x7fff_0000,
                    });
                }
                let program = libc::sock_fprog {
                    len: filter.len() as u16,
                    filter: filter.as_ptr() as *mut _,
                };
                if libc::prctl(libc::PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0) != 0
                    || libc::prctl(libc::PR_SET_SECCOMP, 2, &program) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command.output().unwrap()
    }

    fn worker(&self) -> Worker {
        Worker(
            self.command()
                .arg("--deferred-worker")
                .arg(self.directory.path())
                .arg(&self.socket)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        )
    }

    fn spool(&self) -> PathBuf {
        fs::read_dir(self.directory.path().join("deferred-commands"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
    }

    fn message_count(&self) -> i64 {
        rusqlite::Connection::open(self.directory.path().join("namedrop.sqlite3"))
            .unwrap()
            .query_row("SELECT count(*) FROM collaboration_messages", [], |row| {
                row.get(0)
            })
            .unwrap()
    }
}

struct Worker(Child);
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn send(id: &str, body: &str) -> Value {
    json!({"request_id":id,"source":format!("@reviewer {body}"),"author_kind":"agent_session"})
}

fn response(output: Output) -> Value {
    assert!(
        output.status.success(),
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn wait_for(mut predicate: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !predicate() {
        assert!(Instant::now() < deadline, "condition did not become true");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn denied_socket_queues_before_mutation_and_restarts_recover_one_logical_command() {
    let fixture = Fixture::new();
    let input = send("original-id", "hello");
    let pending = response(fixture.sandbox_command(&["send"], input.clone()));
    assert_eq!(pending["data"]["state"], "pending");
    assert!(pending["data"]["committed"].is_null());
    assert_eq!(fixture.message_count(), 0);
    assert!(
        !fixture.directory.path().join("herdr-calls").exists(),
        "sandbox must not invoke Herdr before enqueue"
    );
    let receipt = PathBuf::from(pending["data"]["receipt"].as_str().unwrap());
    let request = fixture.spool().join(format!(
        "{}.request.json",
        pending["data"]["operation_id"].as_str().unwrap()
    ));
    let original_bytes = fs::read(&request).unwrap();
    let worker = fixture.worker();
    wait_for(|| receipt.exists());
    let result = response(fixture.sandbox_command(&["send"], input.clone()));
    assert!(result["data"]["message_id"].is_string(), "{result}");
    assert_eq!(result["data"]["dispatch"], "scheduled", "{result}");
    assert!(result["data"].get("drain").is_none(), "{result}");
    assert_eq!(fixture.message_count(), 1);
    let agent =
        response(fixture.sandbox_command_with_read_only_state(&["send"], input.clone(), false));
    assert_eq!(agent["data"]["message_id"], result["data"]["message_id"]);
    assert!(agent["data"].get("drain").is_none(), "{agent}");
    assert_eq!(fixture.message_count(), 1);
    let conflict = fixture.sandbox_command(&["send"], send("original-id", "changed"));
    assert!(!conflict.status.success());
    assert!(String::from_utf8_lossy(&conflict.stdout).contains("different command content"));
    assert_eq!(fixture.message_count(), 1);
    drop(worker);
    // Recreate a crash after the domain transaction but before receipt publication.
    fs::remove_file(&receipt).unwrap();
    fs::write(
        fixture.spool().join("pending").join(format!(
            "{}.json",
            pending["data"]["operation_id"].as_str().unwrap()
        )),
        "{}",
    )
    .unwrap();
    StateStore::open(fixture.directory.path())
        .unwrap()
        .transaction(|tx| {
            tx.unbind_pane(
                &StateNamespace::for_space(&fixture.socket, "workspace-1"),
                &PaneAddressId::new("author"),
                &MutationAttribution::new(ActorKind::Human, "test", SystemClock.now_unix_ms()),
            )
        })
        .unwrap();
    let _restarted = fixture.worker();
    wait_for(|| receipt.exists());
    let recovered: Value = serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
    assert_eq!(
        recovered["result"]["Ok"]["message_id"],
        result["data"]["message_id"]
    );
    assert_eq!(recovered["result"]["Ok"]["dispatch"], "scheduled");
    assert_eq!(fixture.message_count(), 1);
    assert_eq!(fs::read(request).unwrap(), original_bytes);
}

#[test]
fn committed_untracked_send_cannot_gain_coordination_when_first_queued() {
    let fixture = Fixture::new();
    let input = send("direct-id", "hello");
    namedrop::execute_message_operation(
        "send",
        input.clone(),
        &LifecycleRuntime {
            state_directory: fixture.directory.path(),
            home_directory: None,
            agent_config: Default::default(),
            cwd: Path::new("/repo"),
            workspace_id: Some("workspace-1"),
            current_pane_id: Some("pane-author"),
            session_socket_id: &fixture.socket,
            mux: &*native(&fixture.socket),
            clock: &SystemClock,
            ids: &ProcessIds::default(),
        },
    )
    .unwrap();
    assert_eq!(fixture.message_count(), 1);
    let mut changed = input;
    changed["coordination_id"] = json!("coordination-added-on-replay");
    let pending = response(fixture.sandbox_command(&["send"], changed));
    assert_eq!(pending["data"]["state"], "pending");
    let receipt = PathBuf::from(pending["data"]["receipt"].as_str().unwrap());
    let _worker = fixture.worker();
    wait_for(|| receipt.exists());
    let result: Value = serde_json::from_slice(&fs::read(receipt).unwrap()).unwrap();
    assert!(
        result["result"]["Err"]
            .as_str()
            .is_some_and(|error| error.contains("already used for an untracked message")),
        "{result}"
    );
    assert_eq!(fixture.message_count(), 1);
}

#[test]
fn worker_checks_native_identity_and_does_not_hold_the_runtime_command_guard() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.directory.path().join("runtime")).unwrap();
    let pending = response(fixture.sandbox_command(&["send"], send("stale-id", "hello")));
    fs::write(
        fixture.directory.path().join("panes.json"),
        panes("replacement-session").to_string(),
    )
    .unwrap();
    let _worker = fixture.worker();
    let receipt = PathBuf::from(pending["data"]["receipt"].as_str().unwrap());
    wait_for(|| receipt.exists());
    let result: Value = serde_json::from_slice(&fs::read(receipt).unwrap()).unwrap();
    assert!(
        result["result"]["Err"]
            .as_str()
            .unwrap()
            .contains("live native Agent Session")
    );
    assert_eq!(fixture.message_count(), 0);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(fixture.directory.path().join("runtime/commands.lock"))
        .unwrap();
    wait_for(|| lock.try_lock().is_ok());
    assert!(fixture.spool().join("worker.json").exists());
}

#[test]
fn roster_reads_are_fresh_and_wrong_attempts_and_arbitrary_operations_are_rejected() {
    let fixture = Fixture::new();
    let _worker = fixture.worker();
    let first = response(fixture.sandbox_command(&["address", "list"], json!({})));
    let second = response(fixture.sandbox_command(&["address", "list"], json!({})));
    assert!(first["data"].is_array(), "{first}");
    assert_eq!(first["data"], second["data"]);
    assert_eq!(
        fs::read_dir(fixture.spool())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".request.json"))
            .count(),
        2
    );
    let wrong_attempt = fixture.sandbox_command(
        &["complete", "--no-reply"],
        json!({"attempt_id":"not-an-attempt"}),
    );
    assert!(!wrong_attempt.status.success());
    assert_eq!(fixture.message_count(), 0);
    let all_workspaces = fixture.sandbox_command(&["address", "list"], json!({"workspaces":"all"}));
    assert!(!all_workspaces.status.success());
    assert!(String::from_utf8_lossy(&all_workspaces.stdout).contains("current workspace"));
    let arbitrary = fixture.sandbox_command(
        &["ack"],
        json!({"attempt_id":"attempt","argv":["sh","-c","touch forbidden"]}),
    );
    assert!(!arbitrary.status.success());
    assert!(String::from_utf8_lossy(&arbitrary.stdout).contains("unknown field"));
    let delivered = response(fixture.sandbox_command(&["send"], send("healthy-worker", "hello")));
    assert!(delivered["data"]["message_id"].is_string(), "{delivered}");
    assert_eq!(fixture.message_count(), 1);
}

/// A pointer prompt's fallback line has to answer from inside the sandbox, with
/// no escalation, or a host without hooks pays an approval on every delivery.
#[test]
fn a_sandboxed_delivery_inbox_is_answered_by_the_worker() {
    let fixture = Fixture::new();
    let _worker = fixture.worker();
    let inbox = response(fixture.sandbox_command(&["delivery", "inbox"], json!({})));
    assert_eq!(inbox["data"], json!({"envelopes": []}), "{inbox}");
    let extra = fixture.sandbox_command(&["delivery", "inbox"], json!({"attempt_id":"a"}));
    assert!(!extra.status.success());
    assert!(String::from_utf8_lossy(&extra.stdout).contains("takes no fields"));
}

/// The outside worker answers a sandboxed roster read, so it reads each
/// agent's transcript from its own environment, as a direct read would.
#[test]
fn a_sandboxed_roster_read_reports_context_size_from_the_workers_home() {
    let fixture = Fixture::new();
    let mut panes = panes("session-author");
    panes["result"]["panes"][0]["agent"] = json!("codex");
    fs::write(
        fixture.directory.path().join("panes.json"),
        panes.to_string(),
    )
    .unwrap();
    let day = fixture.directory.home().join(".codex/sessions/2026/11/07");
    fs::create_dir_all(&day).unwrap();
    fs::write(
        day.join("rollout-2026-11-07T07-39-21-session-author.jsonl"),
        format!(
            "{}\n",
            json!({"type":"event_msg","payload":{"type":"token_count",
                "info":{"last_token_usage":{"input_tokens":120_000}}}})
        ),
    )
    .unwrap();
    let _worker = fixture.worker();
    let listed = response(fixture.sandbox_command(&["address", "list"], json!({})));
    let author = listed["data"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["id"] == "author")
        .unwrap_or_else(|| panic!("{listed}"));
    assert_eq!(author["registered"], true, "{listed}");
    assert_eq!(author["context_tokens"], 120_000, "{listed}");
}

/// A compact reply is completed from its attempt before it is queued, so the
/// queue only ever holds the full form; one naming no attempt is refused first.
#[test]
fn a_compact_reply_is_completed_or_refused_before_it_is_queued() {
    let fixture = Fixture::new();
    let refused = fixture.sandbox_command(
        &["reply"],
        json!({"attempt_id":"not-an-attempt","source":"@reviewer hi"}),
    );
    assert_eq!(refused.status.code(), Some(64), "{refused:?}");
    assert!(
        String::from_utf8_lossy(&refused.stdout).contains("in_reply_to is required"),
        "{refused:?}"
    );
    assert!(
        !fixture.directory.path().join("deferred-commands").exists(),
        "a refused reply was queued"
    );
}

#[test]
fn spool_symlinks_never_read_or_overwrite_their_targets() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let outside = fixture.directory.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let spool_root = fixture.directory.path().join("deferred-commands");
    symlink(&outside, &spool_root).unwrap();
    let denied = fixture.sandbox_command(&["send"], send("symlink", "hello"));
    assert!(!denied.status.success());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    fs::remove_file(&spool_root).unwrap();
    let pending = response(fixture.sandbox_command(&["send"], send("symlink", "hello")));
    let key = pending["data"]["operation_id"].as_str().unwrap();
    let request = fixture.spool().join(format!("{key}.request.json"));
    let saved_request = fs::read(&request).unwrap();
    let target = outside.join("target");
    fs::write(&target, &saved_request).unwrap();
    fs::remove_file(&request).unwrap();
    symlink(&target, &request).unwrap();
    let output = fixture
        .command()
        .arg("--deferred-execute")
        .arg(fixture.directory.path())
        .arg(&fixture.socket)
        .arg(key)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(fixture.message_count(), 0);
    assert_eq!(fs::read(&target).unwrap(), saved_request);
    assert!(request.symlink_metadata().is_err());
    fs::write(&request, &saved_request).unwrap();
    let mut altered: Value = serde_json::from_slice(&saved_request).unwrap();
    altered["operation"] = json!("shell");
    fs::write(&request, altered.to_string()).unwrap();
    let arbitrary = fixture
        .command()
        .arg("--deferred-execute")
        .arg(fixture.directory.path())
        .arg(&fixture.socket)
        .arg(key)
        .output()
        .unwrap();
    assert!(!arbitrary.status.success());
    assert_eq!(fixture.message_count(), 0);
    fs::write(&request, &saved_request).unwrap();
    let receipt = PathBuf::from(pending["data"]["receipt"].as_str().unwrap());
    symlink(&target, &receipt).unwrap();
    let output = fixture.sandbox_command(&["send"], send("symlink", "hello"));
    assert!(!output.status.success());
    assert_eq!(fs::read(&target).unwrap(), saved_request);
    fs::remove_file(&receipt).unwrap();
    symlink(&target, fixture.spool().join("worker.json")).unwrap();
    let _worker = fixture.worker();
    wait_for(|| receipt.exists());
    assert_eq!(fs::read(target).unwrap(), saved_request);
    assert!(
        !fs::symlink_metadata(fixture.spool().join("worker.json"))
            .unwrap()
            .is_symlink()
    );
}

#[test]
fn worker_exits_after_the_last_active_space_is_deactivated() {
    let fixture = Fixture::new();
    let spool_root = fixture.directory.path().join("deferred-commands");
    fs::create_dir(&spool_root).unwrap();
    let worker_started = || {
        fs::read_dir(&spool_root).is_ok_and(|mut entries| {
            entries.any(|entry| entry.is_ok_and(|entry| entry.path().join("worker.json").exists()))
        })
    };
    assert!(!worker_started());
    let mut worker = fixture.worker();
    wait_for(worker_started);
    StateStore::open(fixture.directory.path())
        .unwrap()
        .transaction(|tx| {
            tx.deactivate_worktree(
                &StateNamespace::for_space(&fixture.socket, "workspace-1"),
                &MutationAttribution::new(ActorKind::Human, "test", SystemClock.now_unix_ms()),
            )
        })
        .unwrap();
    wait_for(|| worker.0.try_wait().unwrap().is_some());
}

#[test]
fn a_replaced_registration_cannot_execute_an_older_queued_command() {
    let fixture = Fixture::new();
    let pending = response(fixture.sandbox_command(&["send"], send("old-session", "hello")));
    StateStore::open(fixture.directory.path())
        .unwrap()
        .transaction(|tx| {
            tx.observe_agent_session(
                &StateNamespace::for_space(&fixture.socket, "workspace-1"),
                "replacement-observation",
                &PaneAddressId::new("author"),
                &AgentSessionId::new("replacement-session"),
                "idle",
                &MutationAttribution::new(ActorKind::Human, "test", SystemClock.now_unix_ms()),
            )
        })
        .unwrap();
    let _worker = fixture.worker();
    let receipt = PathBuf::from(pending["data"]["receipt"].as_str().unwrap());
    wait_for(|| receipt.exists());
    let result: Value = serde_json::from_slice(&fs::read(receipt).unwrap()).unwrap();
    assert!(
        result["result"]["Err"]
            .as_str()
            .unwrap()
            .contains("registration")
    );
    assert_eq!(fixture.message_count(), 0);
}

#[test]
fn malformed_receipt_is_removed_and_committed_command_is_recovered() {
    let fixture = Fixture::new();
    let input = send("corrupt-receipt", "original");
    let pending = response(fixture.sandbox_command(&["send"], input.clone()));
    let key = pending["data"]["operation_id"].as_str().unwrap();
    let receipt = PathBuf::from(pending["data"]["receipt"].as_str().unwrap());
    let request = fixture.spool().join(format!("{key}.request.json"));
    let original = fs::read(&request).unwrap();
    let worker = fixture.worker();
    wait_for(|| receipt.exists());
    let committed: Value = serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
    wait_for(|| {
        !fixture
            .spool()
            .join("pending")
            .join(format!("{key}.json"))
            .exists()
    });
    drop(worker);
    fs::write(&receipt, "not json").unwrap();
    fs::write(
        fixture.spool().join("pending").join(format!("{key}.json")),
        "{}",
    )
    .unwrap();
    let mut worker = fixture.worker();
    wait_for(|| {
        assert!(
            worker.0.try_wait().unwrap().is_none(),
            "malformed receipt killed worker"
        );
        fs::read(&receipt)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .is_some()
    });
    let recovered: Value = serde_json::from_slice(&fs::read(&receipt).unwrap()).unwrap();
    assert_eq!(
        recovered["result"]["Ok"]["message_id"],
        committed["result"]["Ok"]["message_id"]
    );
    assert_eq!(fs::read(request).unwrap(), original);
    assert!(!fixture.spool().join("rejected").exists());
    let next = response(fixture.sandbox_command(&["send"], send("after-corrupt-receipt", "next")));
    assert!(next["data"]["message_id"].is_string(), "{next}");
    assert_eq!(fixture.message_count(), 2);
}

#[test]
fn tampered_request_is_removed_without_retrying_or_blocking_valid_commands() {
    let fixture = Fixture::new();
    let pending =
        response(fixture.sandbox_command(&["send"], send("tampered-request", "original")));
    let key = pending["data"]["operation_id"].as_str().unwrap();
    let request = fixture.spool().join(format!("{key}.request.json"));
    let marker = fixture.spool().join("pending").join(format!("{key}.json"));
    let mut tampered: Value = serde_json::from_slice(&fs::read(&request).unwrap()).unwrap();
    tampered["input"]["source"] = json!("@reviewer tampered");
    let bytes = serde_json::to_vec(&tampered).unwrap();
    fs::write(&request, &bytes).unwrap();
    let mut worker = fixture.worker();
    wait_for(|| {
        assert!(
            worker.0.try_wait().unwrap().is_none(),
            "invalid request killed worker"
        );
        !marker.exists()
    });
    assert!(!request.exists());
    assert!(!Path::new(pending["data"]["receipt"].as_str().unwrap()).exists());
    assert_eq!(fixture.message_count(), 0);
    assert!(!fixture.spool().join("rejected").exists());
    let next = response(fixture.sandbox_command(&["send"], send("after-tampered-request", "next")));
    assert!(next["data"]["message_id"].is_string(), "{next}");
    assert_eq!(fixture.message_count(), 1);
}

#[test]
fn nonregular_records_are_removed_and_valid_commands_drain() {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, fs::symlink},
    };
    for suffix in ["request", "receipt", "lock"] {
        for fifo in [false, true] {
            let fixture = Fixture::new();
            let pending =
                response(fixture.sandbox_command(&["send"], send("invalid-record", "original")));
            let key = pending["data"]["operation_id"].as_str().unwrap();
            let record = fixture.spool().join(if suffix == "lock" {
                format!("{key}.lock")
            } else {
                format!("{key}.{suffix}.json")
            });
            let victim = fixture.directory.path().join("victim");
            fs::write(&victim, "unchanged").unwrap();
            if record.exists() {
                fs::remove_file(&record).unwrap();
            }
            if fifo {
                let path = CString::new(record.as_os_str().as_bytes()).unwrap();
                assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
            } else {
                symlink(&victim, &record).unwrap();
            }
            let _worker = fixture.worker();
            let marker = fixture.spool().join("pending").join(format!("{key}.json"));
            wait_for(|| !marker.exists());
            assert_eq!(fs::read(&victim).unwrap(), b"unchanged");
            if suffix != "receipt" {
                assert!(fs::symlink_metadata(&record).is_err());
                assert_eq!(fixture.message_count(), 0);
            } else {
                assert!(fs::symlink_metadata(&record).unwrap().is_file());
                assert_eq!(fixture.message_count(), 1);
            }
            let next = response(fixture.sandbox_command(&["send"], send("after-invalid", "next")));
            assert!(next["data"]["message_id"].is_string(), "{next}");
            assert_eq!(
                fixture.message_count(),
                if suffix == "receipt" { 2 } else { 1 }
            );
        }
    }
}

#[test]
fn producer_reads_existing_state_without_writes_or_directory_creation() {
    let fixture = Fixture::new();
    response(fixture.sandbox_command(&["send"], send("prepare-spool", "first")));
    let runtime = fixture.directory.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    let command_lock = runtime.join("commands.lock");
    fs::write(&command_lock, "").unwrap();
    fs::set_permissions(&command_lock, fs::Permissions::from_mode(0o400)).unwrap();
    let database = fixture.directory.path().join("namedrop.sqlite3");
    let before = fs::read(&database).unwrap();
    let mut entries_before: Vec<_> = fs::read_dir(fixture.directory.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    entries_before.sort();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o400)).unwrap();
    fs::set_permissions(fixture.directory.path(), fs::Permissions::from_mode(0o500)).unwrap();
    let output =
        fixture.sandbox_command_with_read_only_state(&["send"], send("read-only", "second"), true);
    let db_mode = fs::metadata(&database).unwrap().permissions().mode() & 0o777;
    let state_mode = fs::metadata(fixture.directory.path())
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    fs::set_permissions(fixture.directory.path(), fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&database, fs::Permissions::from_mode(0o600)).unwrap();
    let result = response(output);
    assert_eq!(result["data"]["state"], "pending");
    assert_eq!(db_mode, 0o400);
    assert_eq!(state_mode, 0o500);
    assert_eq!(
        fs::metadata(command_lock).unwrap().permissions().mode() & 0o777,
        0o400
    );
    assert_eq!(fs::read(&database).unwrap(), before);
    let mut entries_after: Vec<_> = fs::read_dir(fixture.directory.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    entries_after.sort();
    assert_eq!(entries_after, entries_before);
    assert_eq!(fixture.message_count(), 0);
}

#[test]
fn invalid_pending_markers_never_execute_and_other_commands_drain() {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    for marker_type in ["symlink", "fifo", "directory"] {
        let fixture = Fixture::new();
        let queued =
            response(fixture.sandbox_command(&["send"], send("bad-marker", "must not commit")));
        let key = queued["data"]["operation_id"].as_str().unwrap();
        let marker = fixture.spool().join("pending").join(format!("{key}.json"));
        fs::remove_file(&marker).unwrap();
        if marker_type == "directory" {
            fs::create_dir(&marker).unwrap();
            fs::write(marker.join("keep"), "untouched").unwrap();
        } else if marker_type == "fifo" {
            let path = CString::new(marker.as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
        } else {
            std::os::unix::fs::symlink(
                fixture.spool().join(format!("{key}.request.json")),
                &marker,
            )
            .unwrap();
        }
        let _worker = fixture.worker();
        let next = response(fixture.sandbox_command(&["send"], send("valid-marker", "next")));
        assert!(next["data"]["message_id"].is_string(), "{next}");
        assert_eq!(fixture.message_count(), 1);
        assert!(!Path::new(queued["data"]["receipt"].as_str().unwrap()).exists());
        if marker_type == "directory" {
            assert_eq!(fs::read(marker.join("keep")).unwrap(), b"untouched");
        } else {
            assert!(fs::symlink_metadata(marker).is_err());
        }
    }
}

#[test]
fn deferred_reminder_receipt_loss_preserves_one_absolute_deadline() {
    let f = Fixture::new();
    let clock_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(f.directory.path().join("reminder-clock.lock"))
        .unwrap();
    clock_lock.lock().unwrap();
    let input = json!({"in":"1m","body":"check later"});
    let queued = response(f.sandbox_command(&["reminder", "set"], input.clone()));
    let receipt = PathBuf::from(queued["data"]["receipt"].as_str().unwrap());
    let key = queued["data"]["operation_id"].as_str().unwrap();
    let worker = f.worker();
    wait_for(|| receipt.exists());
    let first = response(f.sandbox_command(&["reminder", "set"], input.clone()));
    assert!(first["data"]["reminder_id"].is_string(), "{first}");
    let first_due = first["data"]["spec"]["first_due_at_ms"].clone();
    drop(worker);
    fs::remove_file(&receipt).unwrap();
    fs::write(f.spool().join("pending").join(format!("{key}.json")), "{}").unwrap();
    let worker = f.worker();
    wait_for(|| receipt.exists());
    let replay = response(f.sandbox_command(&["reminder", "set"], input));
    assert_eq!(replay["data"]["reminder_id"], first["data"]["reminder_id"]);
    assert_eq!(replay["data"]["spec"]["first_due_at_ms"], first_due);
    let listed = response(f.sandbox_command(&["reminder", "list"], json!({})));
    if listed["data"]["state"] == "pending" {
        let list_receipt = PathBuf::from(listed["data"]["receipt"].as_str().unwrap());
        wait_for(|| list_receipt.exists());
    }
    let cancelled = response(f.sandbox_command(
        &["reminder", "cancel"],
        json!({"reminder_id":first["data"]["reminder_id"]}),
    ));
    if cancelled["data"]["state"] == "pending" {
        let path = PathBuf::from(cancelled["data"]["receipt"].as_str().unwrap());
        wait_for(|| path.exists());
    }
    drop(worker);
    let connection =
        rusqlite::Connection::open(f.directory.path().join("namedrop.sqlite3")).unwrap();
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM reminders", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        connection
            .query_row("SELECT state FROM reminders", [], |r| r.get::<_, String>(0))
            .unwrap(),
        "cancelled"
    );
}

#[test]
fn reminder_clock_singleton_wakes_early_and_releases_runtime_gate_while_asleep() {
    let f = Fixture::new();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(f.directory.path().join("reminder-clock.lock"))
        .unwrap();
    lock.lock().unwrap();
    let first = response(
        f.command()
            .arg("--plugin-state")
            .arg(f.directory.path())
            .args([
                "reminder",
                "set",
                "--herdr-socket",
                &f.socket,
                "--herdr-workspace",
                "workspace-1",
                "--herdr-pane",
                "pane-author",
                "--input-json",
            ])
            .arg(json!({"in":"1h","body":"later","request_id":"clock-first"}).to_string())
            .output()
            .unwrap(),
    );
    assert!(first["data"]["reminder_id"].is_string());
    StateStore::open(f.directory.path())
        .unwrap()
        .transaction(|tx| {
            tx.complete_reconciliation(&StateNamespace::for_space(&f.socket, "workspace-1"))
        })
        .unwrap();
    lock.unlock().unwrap();
    fs::create_dir_all(f.directory.path().join("runtime")).unwrap();
    let gate = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(f.directory.path().join("runtime/commands.lock"))
        .unwrap();
    let mut clock = Worker(
        f.command()
            .arg("reminder-clock")
            .arg(f.directory.path())
            .arg(&f.socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(|| namedrop::reminder::clock_running(f.directory.path()));
    let other = f
        .command()
        .arg("reminder-clock")
        .arg(f.directory.path())
        .arg(&f.socket)
        .output()
        .unwrap();
    assert!(other.status.success());
    assert!(other.stdout.is_empty());
    assert!(other.stderr.is_empty());
    wait_for(|| gate.try_lock().is_ok());
    gate.unlock().unwrap();
    let second = response(
        f.command()
            .arg("--plugin-state")
            .arg(f.directory.path())
            .args([
                "reminder",
                "set",
                "--herdr-socket",
                &f.socket,
                "--herdr-workspace",
                "workspace-1",
                "--herdr-pane",
                "pane-author",
                "--input-json",
            ])
            .arg(json!({"in":"1m","body":"earlier","request_id":"clock-second"}).to_string())
            .output()
            .unwrap(),
    );
    let second_id = second["data"]["reminder_id"].as_str().unwrap();
    let connection =
        rusqlite::Connection::open(f.directory.path().join("namedrop.sqlite3")).unwrap();
    let due = SystemClock.now_unix_ms() - 1;
    connection.execute("UPDATE reminders SET due_at_ms=?2,state_json=json_set(state_json,'$.due_at_ms',?2) WHERE reminder_id=?1",rusqlite::params![second_id,due as i64]).unwrap();
    fs::write(f.directory.path().join("reminder-clock.wake"), []).unwrap();
    wait_for(|| {
        connection
            .query_row(
                "SELECT count(*) FROM reminder_fires WHERE reminder_id=?1",
                [second_id],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    });
    wait_for(|| {
        connection.query_row("SELECT count(*) FROM delivery_attempts a JOIN delivery_attempt_state s USING(session_socket_id,worktree_id,attempt_id,address_id) JOIN reminder_fires f USING(session_socket_id,worktree_id,delivery_id) WHERE f.reminder_id=?1 AND s.state='acknowledged'",[second_id],|r|r.get::<_,i64>(0)).unwrap()==1
    });
    let id = second_id.to_owned();
    clock.0.kill().unwrap();
    clock.0.wait().unwrap();
    drop(clock);
    let clock = Worker(
        f.command()
            .arg("reminder-clock")
            .arg(f.directory.path())
            .arg(&f.socket)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    wait_for(|| namedrop::reminder::clock_running(f.directory.path()));
    thread::sleep(Duration::from_millis(200));
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM reminder_fires WHERE reminder_id=?1",
                [id],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    assert_eq!(connection.query_row("SELECT count(*) FROM delivery_attempts a JOIN reminder_fires f USING(session_socket_id,worktree_id,delivery_id) WHERE f.reminder_id=?1",[second_id],|r|r.get::<_,i64>(0)).unwrap(),1);
    drop(clock);
}

#[test]
fn reminder_queued_owner_maintenance_is_not_delayed_by_a_future_schedule() {
    let f = Fixture::new();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(f.directory.path().join("reminder-clock.lock"))
        .unwrap();
    lock.lock().unwrap();
    f.reminder("future", "1h");
    let earlier = f.reminder("queued", "1m");
    let connection =
        rusqlite::Connection::open(f.directory.path().join("namedrop.sqlite3")).unwrap();
    let id = earlier["reminder_id"].as_str().unwrap();
    let due = SystemClock.now_unix_ms() - 1;
    connection.execute("UPDATE reminders SET due_at_ms=?2,state_json=json_set(state_json,'$.due_at_ms',?2) WHERE reminder_id=?1",rusqlite::params![id,due as i64]).unwrap();
    let mut busy = panes("session-author");
    busy["result"]["panes"][0]["agent_status"] = json!("working");
    fs::write(f.directory.path().join("panes.json"), busy.to_string()).unwrap();
    lock.unlock().unwrap();
    let clock = f.clock();
    wait_for(|| {
        connection
            .query_row(
                "SELECT state FROM reminders WHERE reminder_id=?1",
                [id],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
            == "awaiting_delivery"
    });
    fs::write(
        f.directory.path().join("panes.json"),
        panes("replacement").to_string(),
    )
    .unwrap();
    f.wake_mailboxes();
    wait_for(|| {
        connection
            .query_row(
                "SELECT state FROM reminders WHERE reminder_id=?1",
                [id],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
            == "failed"
    });
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM delivery_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(clock);
}

#[test]
fn reminder_empty_exit_set_races_and_common_command_recovery() {
    let f = Fixture::new();
    for i in 0..4 {
        let clock = f.clock();
        let reminder = f.reminder(&format!("exit-race-{i}"), "1h");
        wait_for(|| namedrop::reminder::clock_running(f.directory.path()));
        assert_eq!(namedrop::reminder::pending_count(f.directory.path()), 1);
        f.cancel_reminder(&reminder["reminder_id"]);
        wait_for(|| !namedrop::reminder::clock_running(f.directory.path()));
        drop(clock);
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(f.directory.path().join("reminder-clock.lock"))
        .unwrap();
    lock.lock().unwrap();
    let pending = f.reminder("recover-after-kill", "1h");
    lock.unlock().unwrap();
    assert!(!namedrop::reminder::clock_running(f.directory.path()));
    let output = f
        .command()
        .arg("--plugin-state")
        .arg(f.directory.path())
        .args(["describe", "reminder.list", "--herdr-socket", &f.socket])
        .output()
        .unwrap();
    response(output);
    wait_for(|| namedrop::reminder::clock_running(f.directory.path()));
    f.cancel_reminder(&pending["reminder_id"]);
    wait_for(|| !namedrop::reminder::clock_running(f.directory.path()));
}

#[test]
fn reminder_clock_fails_deleted_workspace_and_preserves_ambiguous_transport() {
    assert_reminder_clock_discovery_failure("workspace_not_found");
}

#[test]
fn reminder_clock_keeps_transient_discovery_pending() {
    assert_reminder_clock_discovery_failure("transport_unavailable");
}

fn assert_reminder_clock_discovery_failure(error: &str) {
    let f = Fixture::new();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(f.directory.path().join("reminder-clock.lock"))
        .unwrap();
    lock.lock().unwrap();
    f.reminder("scheduled", "1h");
    let queued = f.reminder("queued", "1m");
    f.reminder("held", "1m");
    f.reminder("submitted", "1m");
    let connection =
        rusqlite::Connection::open(f.directory.path().join("namedrop.sqlite3")).unwrap();
    let id = queued["reminder_id"].as_str().unwrap();
    let due = SystemClock.now_unix_ms() - 1;
    connection.execute("UPDATE reminders SET due_at_ms=?1,state_json=json_set(state_json,'$.due_at_ms',?1) WHERE request_id!='scheduled'",[due as i64]).unwrap();
    let mut busy = panes("session-author");
    busy["result"]["panes"][0]["agent_status"] = json!("working");
    fs::write(f.directory.path().join("panes.json"), busy.to_string()).unwrap();
    lock.unlock().unwrap();
    let clock = f.clock();
    wait_for(|| {
        connection
            .query_row(
                "SELECT state FROM reminders WHERE reminder_id=?1",
                [id],
                |r| r.get::<_, String>(0),
            )
            .unwrap()
            == "awaiting_delivery"
    });
    wait_for(|| {
        connection
            .query_row(
                "SELECT count(*) FROM reminders WHERE state='awaiting_delivery'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            == 3
    });
    connection.execute_batch("UPDATE mailbox_deliveries SET state='held' WHERE delivery_id IN (SELECT f.delivery_id FROM reminder_fires f JOIN reminders r USING(session_socket_id,worktree_id,reminder_id) WHERE r.request_id='held'); UPDATE mailbox_deliveries SET state='submitted' WHERE delivery_id IN (SELECT f.delivery_id FROM reminder_fires f JOIN reminders r USING(session_socket_id,worktree_id,reminder_id) WHERE r.request_id='submitted');").unwrap();
    let error_payload = json!({"error":{"code":error,"message":"workspace_not_found text alone is not authoritative"}}).to_string();
    let error_path = f.directory.path().join("discovery-error");
    let temporary_error_path = error_path.with_extension("tmp");
    fs::write(&temporary_error_path, &error_payload).unwrap();
    fs::rename(temporary_error_path, error_path).unwrap();
    let failure = namedrop::execute_lifecycle_operation(
        "event.agent-status-changed",
        json!({"type":"pane_agent_status_changed","pane_id":"pane-author","workspace_id":"workspace-1","agent_status":"working"}),
        &LifecycleRuntime {
            state_directory:f.directory.path(),home_directory:Some(f.directory.home()),agent_config:Default::default(),cwd:Path::new("/repo"),workspace_id:Some("workspace-1"),current_pane_id:None,session_socket_id:&f.socket,mux:&ScriptHerdr(&f.herdr),clock:&SystemClock,ids:&ProcessIds::default(),
        },
    ).unwrap_err();
    assert_eq!(failure, error_payload);
    wait_for(|| {
        if error == "workspace_not_found" {
            connection
                .query_row(
                    "SELECT count(*) FROM reminders WHERE state='failed'",
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .unwrap()
                == 4
        } else {
            fs::read_to_string(f.directory.path().join("observed-discovery-error"))
                .is_ok_and(|observed| observed == error_payload)
        }
    });
    drop(clock);
    let mut statement = connection
        .prepare("SELECT state,state_json FROM reminders ORDER BY request_id")
        .unwrap();
    let states = statement
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    if error == "workspace_not_found" {
        assert!(
            states.iter().all(|(state, _)| state == "failed"),
            "deleted workspace left pending reminders: {states:?}"
        );
        assert!(
            states
                .iter()
                .all(|(_, state)| state.contains("workspace") && state.contains("gone")),
            "missing failure explanation: {states:?}"
        );
    } else {
        assert_eq!(
            states
                .iter()
                .map(|(state, _)| state.as_str())
                .collect::<Vec<_>>(),
            vec![
                "awaiting_delivery",
                "awaiting_delivery",
                "scheduled",
                "awaiting_delivery"
            ]
        );
    }
    assert_eq!(
        connection
            .query_row("SELECT d.state FROM mailbox_deliveries d JOIN reminder_fires f USING(session_socket_id,worktree_id,delivery_id) JOIN reminders r USING(session_socket_id,worktree_id,reminder_id) WHERE r.request_id='queued'", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        if error == "workspace_not_found" {
            "failed"
        } else {
            "queued"
        }
    );
    for state in ["held", "submitted"] {
        let actual: String = connection.query_row("SELECT d.state FROM mailbox_deliveries d JOIN reminder_fires f USING(session_socket_id,worktree_id,delivery_id) JOIN reminders r USING(session_socket_id,worktree_id,reminder_id) WHERE r.request_id=?1", [state], |r| r.get(0)).unwrap();
        assert_eq!(actual, state);
    }
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM delivery_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn reminder_common_command_recovers_clock_without_socket_hint() {
    let f = Fixture::new();
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(f.directory.path().join("reminder-clock.lock"))
        .unwrap();
    lock.lock().unwrap();
    let reminder = f.reminder("no-socket-recovery", "1h");
    StateStore::open(f.directory.path())
        .unwrap()
        .transaction(|tx| {
            tx.complete_reconciliation(&StateNamespace::for_space(&f.socket, "workspace-1"))
        })
        .unwrap();
    lock.unlock().unwrap();
    let clock = f.clock();
    wait_for(|| namedrop::reminder::clock_running(f.directory.path()));
    drop(clock);
    assert!(!namedrop::reminder::clock_running(f.directory.path()));
    let connection =
        rusqlite::Connection::open(f.directory.path().join("namedrop.sqlite3")).unwrap();
    let id = reminder["reminder_id"].as_str().unwrap();
    let due = SystemClock.now_unix_ms() - 1;
    connection.execute("UPDATE reminders SET due_at_ms=?2,state_json=json_set(state_json,'$.due_at_ms',?2) WHERE reminder_id=?1", rusqlite::params![id,due as i64]).unwrap();
    response(
        f.command()
            .arg("--plugin-state")
            .arg(f.directory.path())
            .args(["describe", "reminder.set"])
            .env_remove("HERDR_SOCKET_PATH")
            .output()
            .unwrap(),
    );
    let count_fires = || {
        connection
            .query_row("SELECT count(*) FROM reminder_fires", [], |r| {
                r.get::<_, i64>(0)
            })
            .unwrap()
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    while count_fires() == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        count_fires(),
        1,
        "state-scoped describe without socket hint must recover the clock and fire once"
    );
    wait_for(|| {
        connection
            .query_row(
                "SELECT count(*) FROM delivery_attempt_state WHERE state='acknowledged'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
            == 1
    });
    response(
        f.command()
            .arg("--plugin-state")
            .arg(f.directory.path())
            .args(["describe", "reminder.set"])
            .output()
            .unwrap(),
    );
    wait_for(|| !namedrop::reminder::clock_running(f.directory.path()));
    assert_eq!(count_fires(), 1);
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM delivery_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

/// The Stop leaves a delivery to a sandboxed explicit settle that is still
/// queued, and only while it has no receipt.
#[test]
fn a_queued_explicit_settle_is_visible_until_its_receipt_lands() {
    let fixture = Fixture::new();
    let state = fixture.directory.path();
    let queued = response(fixture.sandbox_command(
        &["complete", "--no-reply"],
        json!({"attempt_id":"attempt-queued"}),
    ));
    assert_eq!(queued["data"]["state"], "pending", "{queued}");
    let settle_queued = |session: &str, attempt: &str| {
        namedrop::deferred_commands::settle_queued(state, &fixture.socket, session, attempt)
    };
    assert!(settle_queued("session-author", "attempt-queued"));
    assert!(!settle_queued("session-author", "attempt-other"));
    assert!(!settle_queued("session-other", "attempt-queued"));
    let _worker = fixture.worker();
    let receipt = PathBuf::from(queued["data"]["receipt"].as_str().unwrap());
    wait_for(|| receipt.exists());
    assert!(!settle_queued("session-author", "attempt-queued"));
}

/// One outstanding attempt on `recipient`, sent by the other address.
fn attempt_on(
    fixture: &Fixture,
    recipient: &str,
    binding: &str,
    session: &str,
) -> DeliveryAttemptId {
    let namespace = StateNamespace::for_space(&fixture.socket, "workspace-1");
    let attribution = MutationAttribution::new(ActorKind::Human, "test", 1);
    let attempt = DeliveryAttemptId::new("attempt-ask");
    let author = if recipient == "author" {
        "reviewer"
    } else {
        "author"
    };
    StateStore::open(fixture.directory.path())
        .unwrap()
        .transaction(|tx| {
            if recipient != "author" {
                tx.bind_pane(
                    &namespace,
                    &PaneBindingId::new(binding),
                    &PaneAddressId::new(recipient),
                    "pane-other",
                    &attribution,
                )?;
                tx.observe_agent_session(
                    &namespace,
                    "observation-other",
                    &PaneAddressId::new(recipient),
                    &AgentSessionId::new(session),
                    "idle",
                    &attribution,
                )?;
            }
            tx.create_thread(
                &namespace,
                &ThreadId::new("thread-ask"),
                "An ask",
                &attribution,
            )?;
            tx.append_message(
                &namespace,
                &MessageDraft {
                    thread_id: ThreadId::new("thread-ask"),
                    message_id: MessageId::new("message-ask"),
                    in_reply_to: None,
                    author_address_id: PaneAddressId::new(author),
                    author_kind: ActorKind::Human,
                    author_alias_snapshot: author.to_owned(),
                    author_agent_session_snapshot: None,
                    request_id: "request-ask".to_owned(),
                    authored_source: format!("@{recipient} please answer"),
                    body: "please answer".to_owned(),
                    grammar_version: "mention-hook/v1".to_owned(),
                    recipients: vec![MessageRecipientDraft {
                        address_id: PaneAddressId::new(recipient),
                        alias_snapshot: recipient.to_owned(),
                        agent_session_snapshot: Some(AgentSessionId::new(session)),
                        delivery_id: DeliveryId::new("delivery-ask"),
                    }],
                    context_references: Vec::new(),
                },
                &attribution,
            )?;
            tx.lease_mailbox_head(
                &namespace,
                &PaneAddressId::new(recipient),
                &LeaseDraft {
                    attempt_id: attempt.clone(),
                    binding_id: PaneBindingId::new(binding),
                    agent_session_id: AgentSessionId::new(session),
                    retry_of_attempt_id: None,
                    owner: "test".into(),
                    acquired_at_ms: 2,
                    expires_at_ms: u64::MAX / 2,
                },
                &attribution,
            )?;
            tx.transition_delivery_attempt(
                &namespace,
                &attempt,
                DeliveryState::Submitted,
                &attribution,
            )?;
            Ok(())
        })
        .unwrap();
    attempt
}

fn open_warnings(fixture: &Fixture, attempt: &DeliveryAttemptId) -> Vec<String> {
    rusqlite::Connection::open(fixture.directory.path().join("namedrop.sqlite3"))
        .unwrap()
        .prepare(
            "SELECT summary FROM attention_items WHERE source_kind = 'agent_disposition'
             AND source_id = ?1 AND resolved_at_ms IS NULL",
        )
        .unwrap()
        .query_map([attempt.as_str()], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn reply_naming_a_ghost(attempt: &DeliveryAttemptId) -> Value {
    json!({"attempt_id":attempt.as_str(),"author_kind":"agent_session","in_reply_to":"message-ask",
           "request_id":"reply-refused","source":"@ghost pong"})
}

/// A sandboxed agent gets `pending` back and may never retry, so a reply the
/// worker later refuses has to warn the person.
#[test]
fn a_queued_reply_the_worker_refuses_warns_the_person() {
    let fixture = Fixture::new();
    let attempt = attempt_on(&fixture, "author", "binding", "session-author");
    let queued = response(fixture.sandbox_command(&["reply"], reply_naming_a_ghost(&attempt)));
    assert_eq!(queued["data"]["state"], "pending", "{queued}");
    let _worker = fixture.worker();
    let receipt = PathBuf::from(queued["data"]["receipt"].as_str().unwrap());
    wait_for(|| receipt.exists());
    let warnings = open_warnings(&fixture, &attempt);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("@ghost") && warnings[0].contains("queued reply"),
        "{warnings:?}"
    );
    assert!(!warnings[0].contains("final message"), "{warnings:?}");
}

#[test]
fn a_queued_no_reply_the_worker_refuses_warns_the_person() {
    let fixture = Fixture::new();
    let attempt = attempt_on(&fixture, "author", "binding", "session-author");
    let queued = response(fixture.sandbox_command(
        &["complete", "--no-reply"],
        json!({"attempt_id":attempt.as_str()}),
    ));
    assert_eq!(queued["data"]["state"], "pending", "{queued}");
    fs::write(
        fixture.directory.path().join("panes.json"),
        panes("session-replacement").to_string(),
    )
    .unwrap();
    let _worker = fixture.worker();
    let receipt = PathBuf::from(queued["data"]["receipt"].as_str().unwrap());
    wait_for(|| receipt.exists());
    let warnings = open_warnings(&fixture, &attempt);
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("queued no-reply"), "{warnings:?}");
    assert!(!warnings[0].contains("turn ended"), "{warnings:?}");
}

/// Refusing one agent's command must not open a warning on another's work.
#[test]
fn a_queued_reply_naming_another_agents_attempt_warns_nobody() {
    let fixture = Fixture::new();
    let attempt = attempt_on(&fixture, "reviewer", "binding-other", "session-other");
    let queued = response(fixture.sandbox_command(&["reply"], reply_naming_a_ghost(&attempt)));
    assert_eq!(queued["data"]["state"], "pending", "{queued}");
    let _worker = fixture.worker();
    let receipt = PathBuf::from(queued["data"]["receipt"].as_str().unwrap());
    wait_for(|| receipt.exists());
    let warnings = open_warnings(&fixture, &attempt);
    assert!(warnings.is_empty(), "{warnings:?}");
}
