use super::*;
use crate::{Clock, IdGenerator, LifecycleRuntime, StateStore, execute_lifecycle_operation};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::process::Stdio;
use std::sync::mpsc;

#[derive(Deserialize)]
#[serde(untagged)]
enum StreamLine {
    Snapshot { terminals: Vec<Terminal> },
    Notification { method: Method, params: Value },
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Method {
    Event,
    Missed,
    Heartbeat,
}

#[derive(Deserialize)]
struct Event {
    #[serde(rename = "type")]
    kind: Kind,
    terminal: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Kind {
    Created,
    Restarted,
    Lost,
    Finished,
    Removed,
    Status,
    Turn,
    Labels,
    #[serde(other)]
    Ignored,
}

#[derive(Default)]
pub struct Subscriber {
    spaces: BTreeMap<String, String>,
    next_wake_ms: Option<u64>,
}

impl Subscriber {
    pub fn handle_line<E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        line: &str,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        match serde_json::from_str::<StreamLine>(line)
            .map_err(|error| format!("Invalid hc event: {error}"))?
        {
            StreamLine::Snapshot { terminals } => {
                self.spaces = terminals
                    .iter()
                    .map(|terminal| (terminal.address.clone(), terminal.workspace().to_owned()))
                    .collect();
                self.reconcile_all(runtime)
            }
            StreamLine::Notification {
                method: Method::Missed,
                ..
            } => self.reconcile_all(runtime),
            StreamLine::Notification {
                method: Method::Heartbeat,
                ..
            } => Ok(()),
            StreamLine::Notification {
                method: Method::Event,
                params,
            } => {
                let event: Event =
                    serde_json::from_value(params).map_err(|error| error.to_string())?;
                if matches!(event.kind, Kind::Ignored) {
                    return Ok(());
                }
                let pane = event.terminal.ok_or("hc terminal event has no terminal")?;
                if matches!(event.kind, Kind::Lost | Kind::Restarted) {
                    runtime
                        .host
                        .invalidate_session(runtime.state_directory, &pane)?;
                }
                let terminal = runtime.host.terminal(&pane, None).ok();
                let space = terminal
                    .as_ref()
                    .map(|terminal| terminal.workspace().to_owned())
                    .or_else(|| self.spaces.get(&pane).cloned());
                let Some(space) = space else {
                    return self.reconcile_all(runtime);
                };
                self.spaces.insert(pane.clone(), space.clone());
                let scoped = LifecycleRuntime {
                    workspace_id: Some(&space),
                    current_pane_id: None,
                    ..*runtime
                };
                if !active(&scoped)? {
                    return Ok(());
                }
                match event.kind {
                    Kind::Created | Kind::Restarted => {
                        if let Some(terminal) = terminal {
                            self.operation("event.pane-lifecycle", json!({"type":"pane_created", "pane":{
                                "pane_id":pane, "terminal_id":pane, "workspace_id":space, "tab_id":terminal.labels.get("herdr.tab").cloned().unwrap_or_default(), "focused":false, "agent_status":terminal.status.as_ref().map(|status| status.state.as_str()).unwrap_or("unknown"), "revision":0
                            }}), &scoped)?;
                        }
                        self.operation("reconcile", json!({}), &scoped)
                    }
                    Kind::Lost | Kind::Finished | Kind::Removed => {
                        let kind = if matches!(event.kind, Kind::Removed) { "pane_closed" } else { "pane_exited" };
                        self.operation("event.pane-lifecycle", json!({"type":kind, "pane_id":pane, "workspace_id":space}), &scoped)?;
                        if matches!(event.kind, Kind::Removed) { self.spaces.remove(&pane); }
                        Ok(())
                    }
                    Kind::Status | Kind::Turn => self.operation("event.agent-status-changed", json!({"type":"pane_agent_status_changed", "pane_id":pane, "workspace_id":space}), &scoped),
                    Kind::Labels => self.reconcile_name(&pane, &scoped),
                    Kind::Ignored => Ok(()),
                }
            }
        }
    }

    fn operation<E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        operation: &str,
        input: Value,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        let result = execute_lifecycle_operation(operation, input, runtime)?;
        if let Some(drain) = result.get("drain") {
            self.remember_drain(drain, runtime.clock.now_unix_ms());
        }
        Ok(())
    }

    fn remember_drain(&mut self, value: &Value, now: u64) {
        if let Ok(report) = serde_json::from_value::<crate::DrainReport>(value.clone()) {
            let due = if report.awaits_retry() {
                Some(now.saturating_add(500))
            } else {
                report.next_grace_due_ms()
            };
            if let Some(due) = due {
                self.next_wake_ms =
                    Some(self.next_wake_ms.map_or(due, |previous| previous.min(due)));
            }
        }
    }

    pub fn tick<E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        if self
            .next_wake_ms
            .is_some_and(|due| due <= runtime.clock.now_unix_ms())
        {
            self.next_wake_ms = None;
            for space in active_spaces(runtime)? {
                let scoped = LifecycleRuntime {
                    workspace_id: Some(&space),
                    current_pane_id: None,
                    ..*runtime
                };
                let report = crate::drain_mailboxes(&scoped)?;
                self.remember_drain(
                    &serde_json::to_value(report).map_err(|error| error.to_string())?,
                    runtime.clock.now_unix_ms(),
                );
            }
        }
        Ok(())
    }

    fn reconcile_all<E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        for space in active_spaces(runtime)? {
            let scoped = LifecycleRuntime {
                workspace_id: Some(&space),
                current_pane_id: None,
                ..*runtime
            };
            for terminal in runtime
                .host
                .terminals(None)?
                .iter()
                .filter(|terminal| terminal.workspace() == space)
            {
                self.spaces.insert(terminal.address.clone(), space.clone());
                self.reconcile_name(&terminal.address, &scoped)?;
            }
            self.operation("reconcile", json!({}), &scoped)?;
        }
        Ok(())
    }

    fn reconcile_name<E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        pane: &str,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        let _lock = label_lock(runtime.state_directory)?;
        // Read the current label, since a queued event can precede a release.
        let terminal = runtime.host.terminal(pane, None)?;
        let requested = terminal
            .labels
            .get("namedrop.name")
            .map_or("", String::as_str);
        if requested.is_empty() {
            return Ok(());
        }
        let namespace =
            crate::StateNamespace::for_space(runtime.session_socket_id, terminal.workspace());
        let mut store =
            StateStore::open(runtime.state_directory).map_err(|error| error.to_string())?;
        let stored = store
            .transaction(|transaction| {
                Ok(transaction
                    .list_pane_addresses(&namespace)?
                    .into_iter()
                    .find(|address| !address.retired && address.pane_id.as_deref() == Some(pane))
                    .map(|address| address.alias))
            })
            .map_err(|error| error.to_string())?;
        if stored.as_deref() == Some(requested) {
            return Ok(());
        }
        let scoped = LifecycleRuntime {
            current_pane_id: Some(pane),
            ..*runtime
        };
        let result = crate::lifecycle::execute_lifecycle_operation_locked(
            "name",
            json!({"alias":requested}),
            &scoped,
        );
        if let Ok(value) = &result
            && let Some(drain) = value.get("drain")
        {
            self.remember_drain(drain, runtime.clock.now_unix_ms());
        }
        if let Err(error) = result {
            runtime
                .host
                .set_labels(pane, &[("namedrop.name", stored.as_deref().unwrap_or(""))])?;
            eprintln!("Namedrop rejected hc name request for {pane}: {error}");
        }
        Ok(())
    }
}

fn active<E: HerdrCommandExecutor, C, I>(
    runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
) -> Result<bool, String> {
    let Some(space) = runtime.workspace_id else {
        return Ok(false);
    };
    let namespace = crate::StateNamespace::for_space(runtime.session_socket_id, space);
    StateStore::open(runtime.state_directory)
        .map_err(|error| error.to_string())?
        .transaction(|transaction| transaction.worktree_state(&namespace))
        .map(|state| state.is_some_and(|state| state.activated && !state.deactivated))
        .map_err(|error| error.to_string())
}

fn active_spaces<E: HerdrCommandExecutor, C, I>(
    runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
) -> Result<Vec<String>, String> {
    Ok(StateStore::open(runtime.state_directory)
        .map_err(|error| error.to_string())?
        .dashboard_spaces()
        .map_err(|error| error.to_string())?
        .into_iter()
        .filter(|row| {
            row.namespace.session_socket_id == runtime.session_socket_id
                && row.state.activated
                && !row.state.deactivated
        })
        .map(|row| row.namespace.space_id.as_str().to_owned())
        .collect())
}

pub fn start<E: HerdrCommandExecutor>(
    host: &HcHost<E>,
    executable: &Path,
    state: &Path,
) -> Result<(), HostError> {
    let executable = executable
        .to_str()
        .ok_or("Namedrop executable path is not UTF-8".to_owned())?;
    let state = state
        .to_str()
        .ok_or("Namedrop state path is not UTF-8".to_owned())?;
    let result = host
        .transport
        .execute(&[
            "run",
            "--bg",
            "--no-parent",
            "--rm",
            "--name",
            "namedrop-events",
            "--",
            executable,
            "--host",
            "hc",
            "--plugin-state",
            state,
            "events",
        ])
        .map_err(host_error);
    match result {
        Ok(_) => Ok(()),
        Err(error) if error.code.as_deref() == Some("name_taken") => Ok(()),
        Err(error) => Err(error),
    }
}

pub fn run(state: &Path) -> Result<(), String> {
    let transport = crate::ProcessHerdr::for_host(crate::runtime::HostKind::Hc);
    let host = HcHost::new(&transport);
    let socket = host
        .socket_id()
        .ok_or("hc did not report its core identity")?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let claude = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from);
    let codex = std::env::var_os("CODEX_HOME").map(PathBuf::from);
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let clock = crate::SystemClock;
    let ids = crate::ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: state,
        home_directory: home.as_deref(),
        agent_config: crate::AgentConfigDirectories {
            claude: claude.as_deref(),
            codex: codex.as_deref(),
        },
        cwd: &cwd,
        workspace_id: None,
        current_pane_id: None,
        session_socket_id: &socket,
        host,
        clock: &clock,
        ids: &ids,
    };
    let mut subscriber = Subscriber::default();
    loop {
        let mut child = transport
            .command()
            .args(["events", "--json"])
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| format!("Could not subscribe to hc events: {error}"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or("hc events stdout is unavailable")?;
        let (sender, receiver) = mpsc::sync_channel(1024);
        let reader = std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                if sender.send(line).is_err() {
                    break;
                }
            }
        });
        loop {
            match receiver.recv_timeout(Duration::from_millis(250)) {
                Ok(Ok(line)) => {
                    if let Err(error) = subscriber.handle_line(&line, &runtime) {
                        eprintln!("Namedrop hc event: {error}");
                    }
                }
                Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => (),
            }
            if let Err(error) = subscriber.tick(&runtime) {
                eprintln!("Namedrop hc delivery: {error}");
            }
        }
        crate::runtime::stop_process_group(&mut child);
        drop(receiver);
        let _ = reader.join();
        std::thread::sleep(Duration::from_millis(250));
    }
}
