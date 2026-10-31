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
                let _ = terminals;
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
                let terminal = match runtime.host.terminal(&pane, None) {
                    Ok(terminal) => Some(terminal),
                    Err(_) if matches!(event.kind, Kind::Removed) => None,
                    Err(error) => return Err(error.to_string()),
                };
                let space = terminal
                    .as_ref()
                    .map(|terminal| terminal.workspace().to_owned())
                    .or_else(|| self.spaces.get(&pane).cloned());
                let Some(space) = space else {
                    return self.reconcile_all(runtime);
                };
                let scoped = LifecycleRuntime {
                    workspace_id: Some(&space),
                    current_pane_id: None,
                    ..*runtime
                };
                if matches!(event.kind, Kind::Status | Kind::Turn)
                    && let Some(terminal) = &terminal
                {
                    resume_settled_recipient(terminal, &scoped)?;
                }
                if let Some(terminal) = &terminal {
                    self.reconcile_move(terminal, runtime)?;
                }
                self.spaces.insert(pane.clone(), space.clone());
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
                        self.operation("event.reconcile", json!({}), &scoped)
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
                report
                    .next_grace_due_ms()
                    .into_iter()
                    .chain(report.next_lease_due_ms)
                    .min()
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
            let mut errors = Vec::new();
            for space in active_spaces(runtime)? {
                let scoped = LifecycleRuntime {
                    workspace_id: Some(&space),
                    current_pane_id: None,
                    ..*runtime
                };
                match crate::drain_mailboxes(&scoped) {
                    Ok(report) => self.remember_drain(
                        &serde_json::to_value(report).map_err(|error| error.to_string())?,
                        runtime.clock.now_unix_ms(),
                    ),
                    Err(error) => errors.push(error),
                }
            }
            if !errors.is_empty() {
                self.next_wake_ms = Some(runtime.clock.now_unix_ms().saturating_add(500));
                return Err(errors.join("; "));
            }
        }
        Ok(())
    }

    fn reconcile_all<E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        let _lock = label_lock(runtime.state_directory)?;
        let spaces: Vec<_> = StateStore::open(runtime.state_directory)
            .map_err(|error| error.to_string())?
            .dashboard_spaces()
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|row| row.namespace.session_socket_id == runtime.session_socket_id)
            .collect();
        if spaces.is_empty() {
            return Ok(());
        }
        let discovery = runtime.host.discover(None)?;
        let terminals = discovery.terminals;
        if discovery.warnings.is_empty() {
            let mut store =
                StateStore::open(runtime.state_directory).map_err(|error| error.to_string())?;
            let attribution = crate::MutationAttribution::new(
                crate::ActorKind::Plugin,
                "namedrop-hc-reconcile",
                runtime.clock.now_unix_ms(),
            );
            for row in &spaces {
                let live = terminals
                    .iter()
                    .filter(|terminal| terminal.workspace() == row.namespace.space_id.as_str())
                    .filter_map(|terminal| {
                        terminal
                            .session()
                            .map(|session| (terminal.address.clone(), AgentSessionId::new(session)))
                    })
                    .collect::<Vec<_>>();
                store
                    .transaction(|transaction| {
                        transaction.reconcile_prompt_recipients(&row.namespace, &live, &attribution)
                    })
                    .map_err(|error| error.to_string())?;
            }
        }

        let mut errors: Vec<String> = discovery
            .warnings
            .into_iter()
            .map(|warning| format!("{}: {}", warning.terminal, warning.message))
            .collect();
        for terminal in &terminals {
            if spaces
                .iter()
                .any(|row| row.namespace.space_id.as_str() == terminal.workspace())
            {
                let scoped = LifecycleRuntime {
                    workspace_id: Some(terminal.workspace()),
                    ..*runtime
                };
                if let Err(error) = resume_settled_recipient(terminal, &scoped) {
                    errors.push(error);
                }
            }
        }
        for terminal in &terminals {
            if spaces.iter().any(|row| {
                row.namespace.space_id.as_str() == terminal.workspace()
                    && row.state.activated
                    && !row.state.deactivated
            }) && terminal.labels.contains_key("namedrop.session.pid")
                && let Err(error) = runtime
                    .host
                    .invalidate_session(runtime.state_directory, &terminal.address)
            {
                errors.push(error.to_string());
            }
        }
        let mut failed_moves = BTreeSet::new();
        for terminal in &terminals {
            if let Err(error) = self.reconcile_move(terminal, runtime) {
                failed_moves.insert(terminal.address.as_str());
                errors.push(error);
            }
        }
        for row in spaces {
            let space = row.namespace.space_id.as_str();
            if !row.state.activated || row.state.deactivated {
                continue;
            }
            let scoped = LifecycleRuntime {
                workspace_id: Some(space),
                current_pane_id: None,
                ..*runtime
            };
            for terminal in terminals
                .iter()
                .filter(|terminal| terminal.workspace() == space)
            {
                if failed_moves.contains(terminal.address.as_str()) {
                    continue;
                }
                if let Err(error) = self.reconcile_name(&terminal.address, &scoped) {
                    errors.push(error);
                }
            }
            if let Err(error) = self.operation("event.reconcile", json!({}), &scoped) {
                errors.push(error);
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(errors.join("; "))
        }
    }

    fn reconcile_move<E: HerdrCommandExecutor + Copy, C: Clock, I: IdGenerator>(
        &mut self,
        terminal: &Terminal,
        runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
    ) -> Result<(), String> {
        let _lock = label_lock(runtime.state_directory)?;
        let pane = &terminal.address;
        let destination = terminal.workspace();
        let mut previous: BTreeSet<String> = self.spaces.get(pane).cloned().into_iter().collect();
        let mut store =
            StateStore::open(runtime.state_directory).map_err(|error| error.to_string())?;
        for space in active_spaces(runtime)? {
            let namespace = crate::StateNamespace::for_space(runtime.session_socket_id, &space);
            if store
                .transaction(|transaction| transaction.active_pane_bindings(&namespace))
                .map_err(|error| error.to_string())?
                .iter()
                .any(|binding| binding.pane_id == *pane)
            {
                previous.insert(space);
            }
        }
        previous.retain(|source| source != destination);
        if !previous.is_empty() {
            let namespace =
                crate::StateNamespace::for_space(runtime.session_socket_id, destination);
            let alias = committed_alias(runtime.state_directory, &namespace, pane)?;
            runtime
                .host
                .set_labels(pane, &[("namedrop.name", alias.as_deref().unwrap_or(""))])?;
        }
        for source in previous {
            let scoped = LifecycleRuntime {
                workspace_id: Some(&source),
                ..*runtime
            };
            if active(&scoped)? {
                self.operation("event.pane-lifecycle", json!({
                    "type":"pane_moved", "previous_pane_id":pane,
                    "previous_workspace_id":source, "previous_tab_id":"",
                    "pane": {"pane_id":pane, "terminal_id":pane, "workspace_id":destination,
                        "tab_id":terminal.labels.get("herdr.tab").cloned().unwrap_or_default(),
                        "focused":false, "agent_status":terminal.status.as_ref().map(|status| status.state.as_str()).unwrap_or("unknown"), "revision":0}
                }), &scoped)?;
            }
        }
        self.spaces.insert(pane.clone(), destination.to_owned());
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
        let namespace =
            crate::StateNamespace::for_space(runtime.session_socket_id, terminal.workspace());
        let stored = committed_alias(runtime.state_directory, &namespace, pane)?;
        if requested.is_empty() {
            if let Some(alias) = stored {
                runtime
                    .host
                    .set_labels(pane, &[("namedrop.name", &alias)])?;
            }
            return Ok(());
        }
        if stored.as_deref() == Some(requested) {
            return Ok(());
        }
        let scoped = LifecycleRuntime {
            current_pane_id: Some(pane),
            ..*runtime
        };
        let result = crate::lifecycle::execute_lifecycle_operation_locked(
            "event.name",
            json!({"alias":requested}),
            &scoped,
        );
        if let Ok(value) = &result
            && let Some(drain) = value.get("drain")
        {
            self.remember_drain(drain, runtime.clock.now_unix_ms());
        }
        match result {
            Err(error) => {
                let committed = committed_alias(runtime.state_directory, &namespace, pane)
                    .map_err(|read| format!("{error}; could not read committed name: {read}"))?;
                let message: String = error.chars().take(1024).collect();
                let requested: String = requested.chars().take(256).collect();
                let response = json!({"requested":requested,"message":message}).to_string();
                runtime
                    .host
                    .set_labels(
                        pane,
                        &[
                            ("namedrop.name", committed.as_deref().unwrap_or("")),
                            ("namedrop.name.error", &response),
                        ],
                    )
                    .map_err(|publish| {
                        format!("{error}; could not publish name rejection: {publish}")
                    })?;
                Err(error)
            }
            Ok(_) => {
                runtime
                    .host
                    .set_labels(pane, &[("namedrop.name.error", "")])?;
                Ok(())
            }
        }
    }
}

fn committed_alias(
    state: &Path,
    namespace: &crate::StateNamespace,
    pane: &str,
) -> Result<Option<String>, String> {
    StateStore::open(state)
        .map_err(|error| error.to_string())?
        .transaction(|transaction| {
            Ok(transaction
                .list_pane_addresses(namespace)?
                .into_iter()
                .find(|address| !address.retired && address.pane_id.as_deref() == Some(pane))
                .map(|address| address.alias))
        })
        .map_err(|error| error.to_string())
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

#[derive(Clone, Debug, Deserialize, serde::Serialize, PartialEq, Eq)]
struct BinaryIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}

impl BinaryIdentity {
    fn read(path: &Path) -> Result<Self, String> {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::metadata(path).map_err(|error| error.to_string())?;
        Ok(Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.len(),
            modified_seconds: metadata.mtime(),
            modified_nanoseconds: metadata.mtime_nsec(),
        })
    }
}

#[derive(Deserialize, serde::Serialize, PartialEq, Eq)]
struct SubscriberRecord {
    pid: u32,
    process_start: String,
    launch_path: PathBuf,
    launch_identity: BinaryIdentity,
    binary: BinaryIdentity,
}

impl SubscriberRecord {
    fn is_running(&self) -> bool {
        process_start(self.pid).as_deref() == Some(self.process_start.as_str())
            && running_binary(self.pid).as_ref() == Some(&self.binary)
    }

    fn should_stop(&self, directory: &Path, state: &Path) -> bool {
        STOP.load(std::sync::atomic::Ordering::Relaxed)
            || !self.is_current(state)
            || std::fs::read(directory.join("stop-request.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Self>(&bytes).ok())
                .is_some_and(|request| request == *self)
    }

    fn is_current(&self, state: &Path) -> bool {
        BinaryIdentity::read(&self.launch_path).as_ref() == Ok(&self.launch_identity)
            && (!state.join("runtime/namedrop").exists()
                || BinaryIdentity::read(&state.join("runtime/current/namedrop")).as_ref()
                    == Ok(&self.binary))
    }
}

#[cfg(target_os = "linux")]
fn running_binary(pid: u32) -> Option<BinaryIdentity> {
    BinaryIdentity::read(&PathBuf::from(format!("/proc/{pid}/exe"))).ok()
}

#[cfg(target_os = "macos")]
fn running_binary(pid: u32) -> Option<BinaryIdentity> {
    BinaryIdentity::read(&crate::terminal::process_executable(pid)?).ok()
}

fn subscriber_directory(state: &Path, socket: &str) -> PathBuf {
    use sha2::Digest;
    state
        .join("hc-subscribers")
        .join(crate::lower_hex(&sha2::Sha256::digest(socket.as_bytes())))
}

fn lock_file(directory: &Path, name: &str) -> Result<std::fs::File, String> {
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .map_err(|error| error.to_string())?;
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(directory.join(name))
        .map_err(|error| error.to_string())
}

fn read_record(directory: &Path) -> Option<SubscriberRecord> {
    serde_json::from_slice(&std::fs::read(directory.join("subscriber.json")).ok()?).ok()
}

fn stop_verified(directory: &Path, record: &SubscriberRecord) -> Result<(), String> {
    use std::io::Write;
    let mut request = lock_file(directory, ".stop-request.json")?;
    request.set_len(0).map_err(|error| error.to_string())?;
    request
        .write_all(&serde_json::to_vec(record).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    std::fs::rename(
        directory.join(".stop-request.json"),
        directory.join("stop-request.json"),
    )
    .map_err(|error| error.to_string())?;
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::FromRawFd;
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, record.pid, 0) };
        if fd < 0 {
            return Ok(());
        }
        let _fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd as i32) };
        if record.is_running()
            && unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    fd,
                    libc::SIGTERM,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            } < 0
        {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
    #[cfg(not(target_os = "linux"))]
    Ok(())
}

fn wait_for_lifetime(directory: &Path) -> Result<std::fs::File, String> {
    let lifetime = lock_file(directory, "lifetime.lock")?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        match lifetime.try_lock() {
            Ok(()) => return Ok(lifetime),
            Err(std::fs::TryLockError::WouldBlock) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(format!("Could not stop HC subscriber: {error}")),
        }
    }
}

pub fn stop(state: &Path, socket: &str) -> Result<(), String> {
    let directory = subscriber_directory(state, socket);
    if !directory.exists() {
        return Ok(());
    }
    let launch = lock_file(&directory, "launch.lock")?;
    launch.lock().map_err(|error| error.to_string())?;
    if let Some(record) = read_record(&directory) {
        stop_verified(&directory, &record)?;
    }
    drop(wait_for_lifetime(&directory)?);
    Ok(())
}

pub fn status(state: &Path, socket: &str) -> Result<(), String> {
    let directory = subscriber_directory(state, socket);
    let record = read_record(&directory).ok_or("The HC event subscriber is missing")?;
    if !record.is_running() {
        return Err("The HC event subscriber is stopped".to_owned());
    }
    if !record.is_current(state) {
        return Err("The HC event subscriber is running an older Namedrop binary".to_owned());
    }
    Ok(())
}

pub fn start_with_environment<E: HerdrCommandExecutor>(
    host: &HcHost<E>,
    executable: &Path,
    state: &Path,
    environment: &crate::environment::EnvironmentSnapshot,
) -> Result<(), HostError> {
    let socket = host.core_identity()?;
    let endpoint = environment.hc_endpoint()?;
    let directory = subscriber_directory(state, &socket);
    let launch = lock_file(&directory, "launch.lock")?;
    launch.lock().map_err(|error| error.to_string())?;
    let launch_identity = BinaryIdentity::read(executable)?;
    let binary = BinaryIdentity::read(&if executable == state.join("runtime/namedrop") {
        state.join("runtime/current/namedrop")
    } else {
        executable.to_owned()
    })?;
    if let Some(record) = read_record(&directory)
        && record.is_running()
    {
        if record.binary == binary
            && record.launch_identity == launch_identity
            && record.launch_path == executable
            && record.is_current(state)
        {
            return Ok(());
        }
        stop_verified(&directory, &record)?;
    }
    drop(wait_for_lifetime(&directory)?);
    let mut command = environment.hc_command(executable);
    command
        .env("HC_SOCKET", endpoint)
        .env("NAMEDROP_SUBSCRIBER_LAUNCH_PATH", executable)
        .args(["--host", "hc", "--plugin-state"])
        .arg(state)
        .arg("events")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
            }
        });
    }
    let mut child = command
        .spawn()
        .map_err(|error| format!("Could not launch HC subscriber: {error}"))?;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if read_record(&directory)
            .is_some_and(|record| record.pid == child.id() && record.is_running())
        {
            return Ok(());
        }
        if let Some(status) = child.try_wait().map_err(|error| error.to_string())? {
            return Err(format!("HC subscriber exited before becoming ready: {status}").into());
        }
        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("HC subscriber did not become ready within five seconds"
                .to_owned()
                .into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

static STOP: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

extern "C" fn request_stop(_: libc::c_int) {
    STOP.store(true, std::sync::atomic::Ordering::Relaxed);
}

pub fn run_with_environment(
    state: &Path,
    environment: &crate::environment::EnvironmentSnapshot,
) -> Result<(), String> {
    let transport = crate::ProcessHerdr::from_snapshot(crate::runtime::HostKind::Hc, environment)
        .with_state_directory(state.to_owned());
    let host = HcHost::new(&transport);
    let socket = host.core_identity()?;
    let directory = subscriber_directory(state, &socket);
    let lifetime = lock_file(&directory, "lifetime.lock")?;
    match lifetime.try_lock() {
        Ok(()) => (),
        Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
        Err(error) => return Err(error.to_string()),
    }
    let running = std::env::current_exe().map_err(|error| error.to_string())?;
    let launch_path = environment
        .var_os("NAMEDROP_SUBSCRIBER_LAUNCH_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| running.clone());
    let record = SubscriberRecord {
        launch_identity: BinaryIdentity::read(&launch_path)?,
        pid: std::process::id(),
        process_start: process_start(std::process::id()).ok_or("Missing process start identity")?,
        launch_path,
        binary: BinaryIdentity::read(&running)?,
    };
    if !record.is_current(state) {
        return Ok(());
    }
    use std::io::Write;
    let temporary_record = format!(".subscriber-{}.json", record.pid);
    let mut record_file = lock_file(&directory, &temporary_record)?;
    record_file.set_len(0).map_err(|error| error.to_string())?;
    record_file
        .write_all(&serde_json::to_vec(&record).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())?;
    std::fs::rename(
        directory.join(temporary_record),
        directory.join("subscriber.json"),
    )
    .map_err(|error| error.to_string())?;
    unsafe {
        libc::signal(
            libc::SIGTERM,
            request_stop as *const () as libc::sighandler_t,
        );
    }
    let cwd = std::env::current_dir().map_err(|error| error.to_string())?;
    let clock = crate::SystemClock;
    let ids = crate::ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: state,
        home_directory: environment.home(),
        agent_config: environment.agent_config(),
        cwd: &cwd,
        workspace_id: None,
        current_pane_id: None,
        session_socket_id: &socket,
        host,
        clock: &clock,
        ids: &ids,
    };
    let mut subscriber = Subscriber::default();
    while !record.should_stop(&directory, state) {
        let mut command = match transport.checked_command_bounded(Duration::from_millis(250), &STOP)
        {
            Ok(command) => command,
            Err(_) if record.should_stop(&directory, state) => break,
            Err(error)
                if error.starts_with("hc core is unavailable at ")
                    || error == "Herdr command timed out during hc socket preflight" =>
            {
                eprintln!("Namedrop hc reconnect: {error}");
                std::thread::sleep(Duration::from_millis(250));
                continue;
            }
            Err(error) => return Err(error),
        };
        if record.should_stop(&directory, state) {
            break;
        }
        let mut child = command
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
        'stream: loop {
            let line = receiver.recv_timeout(Duration::from_millis(250));
            if record.should_stop(&directory, state) {
                break;
            }
            let guard = loop {
                match crate::runtime_command_guard(state, Some(Duration::from_millis(250))) {
                    Ok(guard) => break guard,
                    Err(error) => {
                        eprintln!("Namedrop hc runtime: {error}");
                        if record.should_stop(&directory, state) {
                            break 'stream;
                        }
                        std::thread::sleep(Duration::from_millis(25));
                    }
                }
            };
            if record.should_stop(&directory, state) {
                break;
            }
            match line {
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
            drop(guard);
        }
        crate::runtime::stop_process_group(&mut child);
        drop(receiver);
        let _ = reader.join();
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(())
}

fn resume_settled_recipient<E: HerdrCommandExecutor, C: Clock, I>(
    terminal: &Terminal,
    runtime: &LifecycleRuntime<'_, HcHost<E>, C, I>,
) -> Result<(), String> {
    let Some(status_ms) = terminal.status_ms else {
        return Ok(());
    };
    let Occupancy::Agent {
        session: Some(session),
        state: AgentState::Foreground(Some(StatusWord::Idle | StatusWord::Done)),
        ..
    } = terminal.occupancy()
    else {
        return Ok(());
    };
    if terminal.state != "live" || runtime.workspace_id != Some(terminal.workspace()) {
        return Ok(());
    }
    let namespace =
        crate::StateNamespace::for_space(runtime.session_socket_id, terminal.workspace());
    let attribution = crate::MutationAttribution::new(
        crate::ActorKind::Plugin,
        "namedrop-hc-settle",
        runtime.clock.now_unix_ms(),
    );
    let _prompt_guard = prompt_lock(
        runtime.state_directory,
        runtime.session_socket_id,
        &terminal.address,
    )?;
    StateStore::open(runtime.state_directory)
        .map_err(|error| error.to_string())?
        .transaction(|transaction| {
            if transaction.worktree_state(&namespace)?.is_none() {
                return Ok(());
            }
            transaction.resume_prompt_after_settle(
                &namespace,
                &terminal.address,
                &AgentSessionId::new(session),
                status_ms,
                &attribution,
            )?;
            transaction.resume_mailbox_after_settle(
                &namespace,
                &terminal.address,
                &AgentSessionId::new(session),
                status_ms,
                &attribution,
            )
        })
        .map_err(|error| error.to_string())
}
