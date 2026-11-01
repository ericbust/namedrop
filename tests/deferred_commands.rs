#![cfg(target_os = "linux")]
mod support;
use namedrop::{
    ActorKind, AgentRuntime, AgentSessionId, Clock, HerdrCommandExecutor, HerdrCommandOutput,
    LifecycleRuntime, MutationAttribution, PaneAddressId, PaneBindingId, ProcessIds,
    StateNamespace, StateStore, SystemClock, complete_capability_profile,
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

struct NativeHerdr;
impl HerdrCommandExecutor for NativeHerdr {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let value = match args {
            ["worktree", "list", ..] => {
                json!({"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}})
            }
            ["workspace", "list"] => {
                json!({"result":{"workspaces":[{"workspace_id":"workspace-1"}]}})
            }
            ["pane", "list", ..] => panes("session-author"),
            _ => return Err(format!("unexpected Herdr command {args:?}")),
        };
        Ok(HerdrCommandOutput {
            stdout: value.to_string(),
        })
    }
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
            session_socket_id:&socket, herdr:&NativeHerdr, clock:&SystemClock, ids:&ProcessIds::default(),
        }).unwrap();
        fs::write(
            directory.path().join("panes.json"),
            panes("session-author").to_string(),
        )
        .unwrap();
        let herdr = directory.path().join("herdr");
        fs::write(&herdr, r#"#!/usr/bin/env python3
import json, sys
from pathlib import Path
root = Path(__file__).parent
args = sys.argv[1:]
with (root / 'herdr-calls').open('a') as f: f.write(json.dumps(args) + '\n')
if args[:2] == ['--socket', str(root / 'herdr.sock')]: args = args[2:]
if args[:2] == ['worktree','list']: print(json.dumps({'result':{'worktrees':[{'path':'/repo','open_workspace_id':'workspace-1'}]}}))
elif args[:2] == ['workspace','list']: print(json.dumps({'result':{'workspaces':[{'workspace_id':'workspace-1'}]}}))
elif args[:2] == ['pane','list']: print((root / 'panes.json').read_text())
else: print('{}')
"#).unwrap();
        fs::set_permissions(&herdr, fs::Permissions::from_mode(0o700)).unwrap();
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
            .env("HERDR_BIN_PATH", &self.herdr)
            .current_dir(self.directory.path());
        command
    }

    fn sandbox_command(&self, operation: &[&str], input: Value) -> Output {
        self.sandbox_command_with_read_only_state(operation, input, false)
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
            herdr: &NativeHerdr,
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
    let mut worker = fixture.worker();
    wait_for(|| {
        fixture.directory.path().join("deferred-commands").exists()
            && fixture.spool().join("worker.json").exists()
    });
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
