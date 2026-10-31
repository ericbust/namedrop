use crate::{Clock, HerdrCommandExecutor, HerdrCommandOutput, IdGenerator};
use std::{
    env, fs,
    io::{Read, Seek, SeekFrom},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq, clap::ValueEnum)]
pub enum HostKind {
    Tui,
    Hc,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct HostSignals {
    pub host: Option<HostKind>,
    pub tui_transport: bool,
    pub hc_transport: bool,
    pub plugin_state: bool,
}

impl HostSignals {
    pub fn explicit_selection(self) -> Result<Option<HostKind>, String> {
        if (self.tui_transport && self.hc_transport)
            || (self.host == Some(HostKind::Tui) && self.hc_transport)
            || (self.host == Some(HostKind::Hc) && self.tui_transport)
            || (self.host.is_none() && self.plugin_state && self.hc_transport)
        {
            return Err("Host flags conflict: --host must agree with --herdr-socket/--herdr-workspace/--herdr-pane or --hc-terminal/--hc-space; --plugin-state without --host selects tui.".to_owned());
        }
        Ok(self.host.or({
            if self.tui_transport || self.plugin_state {
                Some(HostKind::Tui)
            } else if self.hc_transport {
                Some(HostKind::Hc)
            } else {
                None
            }
        }))
    }
}

impl HostKind {
    pub fn detect(
        signals: HostSignals,
        variable: impl Fn(&str) -> Option<String>,
    ) -> Result<Self, String> {
        if let Some(host) = signals.explicit_selection()? {
            return Ok(host);
        }
        let Some(terminal) = variable("HC_TERMINAL_ID") else {
            return Ok(Self::Tui);
        };
        let environment = crate::environment::EnvironmentSnapshot::from_variables(
            [
                "HOME",
                "XDG_STATE_HOME",
                "XDG_CONFIG_HOME",
                "XDG_DATA_HOME",
                "XDG_RUNTIME_DIR",
                "HC_SOCKET",
                "HC_TERMINAL_SOCKET",
                "HC_SESSION",
                "HERDR_PANE_ID",
                "HERDR_SOCKET_PATH",
                "HERDR_BIN_PATH",
                "HC_BIN_PATH",
                "NAMEDROP_HERDR_TIMEOUT_MS",
                "PATH",
            ]
            .into_iter()
            .filter_map(|key| variable(key).map(|value| (key.into(), value.into())))
            .chain(std::iter::once(("HC_TERMINAL_ID".into(), terminal.into()))),
        );
        Self::from_snapshot(None, &environment)
    }

    pub fn from_snapshot(
        explicit: Option<Self>,
        environment: &crate::environment::EnvironmentSnapshot,
    ) -> Result<Self, String> {
        if let Some(host) = explicit {
            return Ok(host);
        }
        let variable = |key| environment.var(key);
        let Some(terminal) = variable("HC_TERMINAL_ID") else {
            return Ok(Self::Tui);
        };
        let Some(pane) = variable("HERDR_PANE_ID") else {
            return Ok(if variable("HERDR_SOCKET_PATH").is_some() {
                Self::Tui
            } else {
                Self::Hc
            });
        };
        Ok(Self::innermost_host(&pane, &terminal, environment).unwrap_or(Self::Tui))
    }

    fn innermost_host(
        pane: &str,
        terminal: &str,
        environment: &crate::environment::EnvironmentSnapshot,
    ) -> Option<Self> {
        use crate::Budget;
        let budget = IdentityBudget {
            deadline: Instant::now() + Duration::from_millis(1500),
            cancelled: AtomicBool::new(false),
        };
        let tui = crate::TuiHost::new(ProcessHerdr::from_snapshot(Self::Tui, environment));
        let hc = crate::HcHost::new(ProcessHerdr::from_snapshot(Self::Hc, environment));
        let tui_pid = tui.shell_pid(pane, &budget)?;
        let hc_pid = hc.shell_pid(terminal, &budget)?;
        let mut ancestors = Vec::new();
        let mut pid = std::process::id();
        for _ in 0..1024 {
            budget.remaining().ok()?;
            pid = crate::terminal::parent_pid(pid)?;
            if pid == 0 {
                break;
            }
            ancestors.push(pid);
        }
        budget.remaining().ok()?;
        if pid != 0 {
            return None;
        }
        let tui_position = ancestors.iter().position(|pid| *pid == tui_pid);
        let hc_position = ancestors.iter().position(|pid| *pid == hc_pid);
        Some(match (tui_position, hc_position) {
            (None, _) => Self::Hc,
            (Some(tui), Some(hc)) if hc < tui => Self::Hc,
            _ => Self::Tui,
        })
    }

    pub fn from_environment(explicit: Option<Self>) -> Result<Self, String> {
        Self::from_snapshot(
            explicit,
            &crate::environment::EnvironmentSnapshot::capture(),
        )
    }
}

struct IdentityBudget {
    deadline: Instant,
    cancelled: AtomicBool,
}

impl crate::Budget for IdentityBudget {
    fn remaining(&self) -> Result<Duration, String> {
        self.deadline
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or_else(|| "Host identity deadline exceeded".into())
    }

    fn cancelled(&self) -> &AtomicBool {
        &self.cancelled
    }

    fn max_output_bytes(&self) -> usize {
        512 * 1024
    }
}

pub fn hc_state_root(variable: impl Fn(&str) -> Option<String>) -> Result<PathBuf, String> {
    variable("XDG_STATE_HOME")
        .filter(|value| !value.is_empty())
        .map(std::path::PathBuf::from)
        .or_else(|| {
            variable("HOME").map(|home| std::path::PathBuf::from(home).join(".local/state"))
        })
        .ok_or_else(|| "Set XDG_STATE_HOME, HOME, or --plugin-state for hc.".to_owned())
}

pub(crate) fn hc_session(variable: impl Fn(&str) -> Option<String>) -> Result<String, String> {
    let session = variable("HC_SESSION").unwrap_or_default();
    let session = session.trim();
    let session = if session.is_empty() {
        "default"
    } else {
        session
    };
    if session == "."
        || session == ".."
        || session
            .chars()
            .any(|c| c.is_control() || "/\\:*?\"<>|".contains(c))
    {
        return Err("HC_SESSION is not a valid session name.".to_owned());
    }
    Ok(session.to_owned())
}

pub fn hc_state_directory(variable: impl Fn(&str) -> Option<String>) -> Result<PathBuf, String> {
    Ok(hc_state_root(&variable)?
        .join("namedrop/hc")
        .join(hc_session(variable)?))
}

pub fn hc_socket_path(variable: impl Fn(&str) -> Option<String>) -> Result<PathBuf, String> {
    hc_socket_path_os(|key| variable(key).map(Into::into))
}

pub(crate) fn hc_socket_path_os(
    variable: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Result<PathBuf, String> {
    let session = hc_session(|key| variable(key)?.into_string().ok())?;
    for key in ["HC_SOCKET", "HC_TERMINAL_SOCKET"] {
        if let Some(socket) = variable(key).filter(|value| !value.is_empty()) {
            return Ok(socket.into());
        }
    }
    let config_root = variable("XDG_CONFIG_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| variable("HOME").map(|home| PathBuf::from(home).join(".config")));
    if let Some(root) = config_root {
        let path = root.join("herdr-core/config.toml");
        match fs::read_to_string(&path) {
            Ok(contents) => {
                let config = contents
                    .parse::<toml_edit::DocumentMut>()
                    .map_err(|error| format!("Invalid hc config {}: {error}", path.display()))?;
                if let Some(socket) = config.get("socket") {
                    let socket = socket
                        .as_str()
                        .filter(|socket| !socket.is_empty())
                        .ok_or_else(|| {
                            format!(
                                "Invalid hc config {}: socket must be a nonempty string",
                                path.display()
                            )
                        })?;
                    return Ok(socket.into());
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("Cannot read hc config {}: {error}", path.display())),
        }
    }
    let runtime = variable("XDG_RUNTIME_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            // SAFETY: getuid has no arguments or memory preconditions.
            let root = PathBuf::from(format!("/run/user/{}", unsafe { libc::getuid() }));
            root.is_dir().then_some(root)
        });
    let directory = match runtime {
        Some(root) => root.join("herdr-core"),
        None => variable("XDG_STATE_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| variable("HOME").map(|home| PathBuf::from(home).join(".local/state")))
            .ok_or("Set XDG_STATE_HOME, HOME, or --plugin-state for hc.")?
            .join("herdr-core")
            .join(&session),
    };
    Ok(directory.join(format!("{session}.sock")))
}

fn check_hc_budget(deadline: Instant, cancelled: &AtomicBool) -> Result<(), String> {
    if cancelled.load(Ordering::Relaxed) {
        Err("Herdr read cancelled".into())
    } else if Instant::now() >= deadline {
        Err("Herdr command timed out during hc socket preflight".into())
    } else {
        Ok(())
    }
}

fn connect_hc_socket(path: &Path, deadline: Instant, cancelled: &AtomicBool) -> Result<(), String> {
    use std::os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, net::UnixStream},
    };
    check_hc_budget(deadline, cancelled)?;
    // SAFETY: zero initializes the sockaddr padding and the pathname terminator.
    let mut address: libc::sockaddr_un = unsafe { std::mem::zeroed() };
    let bytes = path.as_os_str().as_bytes();
    if bytes.is_empty() || bytes.contains(&0) {
        return Err(format!("Invalid hc socket path: {}", path.display()));
    }
    if bytes.len() >= address.sun_path.len() {
        return Err(format!(
            "hc socket path is too long ({} bytes; maximum {} bytes): {}. Set HC_SOCKET to a shorter path matching the running core's socket.",
            bytes.len(),
            address.sun_path.len() - 1,
            path.display()
        ));
    }
    address.sun_family = libc::AF_UNIX as libc::sa_family_t;
    for (destination, source) in address.sun_path.iter_mut().zip(bytes) {
        *destination = *source as libc::c_char;
    }
    let unavailable =
        |error: std::io::Error| format!("hc core is unavailable at {}: {error}", path.display());
    // SAFETY: socket creates a new descriptor and does not access caller memory.
    let fd = unsafe { libc::socket(libc::AF_UNIX, libc::SOCK_STREAM, 0) };
    if fd < 0 {
        return Err(unavailable(std::io::Error::last_os_error()));
    }
    // SAFETY: fd is a newly created stream socket, owned only by this stream.
    let stream = unsafe { UnixStream::from_raw_fd(fd) };
    // SAFETY: fd is live and F_SETFD takes an integer flag argument.
    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
        return Err(unavailable(std::io::Error::last_os_error()));
    }
    stream.set_nonblocking(true).map_err(unavailable)?;
    loop {
        check_hc_budget(deadline, cancelled)?;
        // SAFETY: address is initialized and its full size matches the supplied pointer.
        let result = unsafe {
            libc::connect(
                stream.as_raw_fd(),
                (&address as *const libc::sockaddr_un).cast(),
                std::mem::size_of_val(&address) as libc::socklen_t,
            )
        };
        if result == 0 {
            return check_hc_budget(deadline, cancelled);
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EISCONN) => return check_hc_budget(deadline, cancelled),
            Some(libc::EINPROGRESS | libc::EALREADY | libc::EINTR | libc::EAGAIN) => {
                thread::sleep(
                    Duration::from_millis(5)
                        .min(deadline.saturating_duration_since(Instant::now())),
                );
            }
            _ => return Err(unavailable(error)),
        }
    }
}

static NEXT_HERDR_OUTPUT_FILE: AtomicU64 = AtomicU64::new(1);
const DEFAULT_HERDR_TIMEOUT_MS: u64 = 10_000;
const MAX_HERDR_TIMEOUT_MS: u64 = 60_000;

pub struct ProcessHerdr {
    binary: String,
    host: HostKind,
    timeout: Duration,
    socket: Option<String>,
    child_executable: Option<std::path::PathBuf>,
    environment: crate::environment::EnvironmentSnapshot,
}

impl Default for ProcessHerdr {
    fn default() -> Self {
        Self::for_host(HostKind::Tui)
    }
}

impl ProcessHerdr {
    pub fn for_host(host: HostKind) -> Self {
        Self::from_snapshot(host, &crate::environment::EnvironmentSnapshot::capture())
    }

    pub fn from_snapshot(
        host: HostKind,
        environment: &crate::environment::EnvironmentSnapshot,
    ) -> Self {
        if host == HostKind::Hc {
            let _ = environment.hc_endpoint();
        }
        Self {
            host,
            binary: environment
                .var(if host == HostKind::Hc {
                    "HC_BIN_PATH"
                } else {
                    "HERDR_BIN_PATH"
                })
                .unwrap_or_else(|| if host == HostKind::Hc { "hc" } else { "herdr" }.to_owned()),
            socket: None,
            child_executable: None,
            timeout: Duration::from_millis(
                environment
                    .var("NAMEDROP_HERDR_TIMEOUT_MS")
                    .and_then(|value| value.parse::<u64>().ok())
                    .filter(|ms| *ms > 0)
                    .unwrap_or(DEFAULT_HERDR_TIMEOUT_MS)
                    .min(MAX_HERDR_TIMEOUT_MS),
            ),
            environment: environment.clone(),
        }
    }

    pub fn command(&self) -> Command {
        match self.host {
            HostKind::Hc => self.environment.hc_command(&self.binary),
            HostKind::Tui => {
                let mut command = self.environment.tui_command(&self.binary);
                if let Some(socket) = &self.socket {
                    command.env("HERDR_SOCKET_PATH", socket);
                }
                command
            }
        }
    }

    /// Checks the selected existing core before constructing an hc command.
    /// The core can still stop between this check and the child's connection.
    pub fn checked_command(&self) -> Result<Command, String> {
        self.checked_command_bounded(self.timeout, &AtomicBool::new(false))
    }

    pub fn checked_command_bounded(
        &self,
        timeout: Duration,
        cancelled: &AtomicBool,
    ) -> Result<Command, String> {
        self.checked_command_until(Instant::now() + self.bounded_timeout(timeout), cancelled)
    }

    fn checked_command_until(
        &self,
        deadline: Instant,
        cancelled: &AtomicBool,
    ) -> Result<Command, String> {
        if self.host == HostKind::Tui {
            return Ok(self.command());
        }
        check_hc_budget(deadline, cancelled)?;
        let socket = self.environment.hc_endpoint()?;
        connect_hc_socket(socket, deadline, cancelled)?;
        let mut command = self.command();
        command.env("HC_SOCKET", socket);
        Ok(command)
    }

    pub fn with_child_executable(mut self, executable: std::path::PathBuf) -> Self {
        self.child_executable = Some(executable);
        self
    }

    pub fn with_socket(mut self, socket: Option<String>) -> Self {
        self.socket = socket;
        self
    }

    fn output_file(&self, stream: &str) -> Result<fs::File, String> {
        for _ in 0..16 {
            let path = env::temp_dir().join(format!(
                ".namedrop-herdr-{stream}-{}-{}",
                std::process::id(),
                NEXT_HERDR_OUTPUT_FILE.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(file) => {
                    fs::remove_file(&path).map_err(|error| {
                        format!("failed to secure Herdr command output: {error}")
                    })?;
                    return Ok(file);
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!("failed to capture Herdr command output: {error}"));
                }
            }
        }
        Err("failed to reserve Herdr command output".to_owned())
    }

    fn read_output(
        &self,
        mut file: fs::File,
        stream: &str,
        max_output_bytes: usize,
    ) -> Result<Vec<u8>, String> {
        file.seek(SeekFrom::Start(0))
            .map_err(|error| format!("failed to read Herdr command {stream}: {error}"))?;
        let mut bytes = Vec::new();
        file.take(max_output_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| format!("failed to read Herdr command {stream}: {error}"))?;
        if bytes.len() > max_output_bytes {
            return Err("Herdr read exceeded its output limit".to_owned());
        }
        Ok(bytes)
    }

    fn command_timeout(&self, args: &[&str]) -> Duration {
        if self.host == HostKind::Hc && args.first() == Some(&"send") {
            Duration::from_secs(20)
        } else {
            self.timeout
        }
    }

    fn output(&self, args: &[&str]) -> Result<Output, String> {
        self.output_bounded(
            args,
            self.command_timeout(args),
            &AtomicBool::new(false),
            usize::MAX,
        )
    }

    fn bounded_timeout(&self, timeout: Duration) -> Duration {
        if self.host == HostKind::Hc {
            timeout.min(Duration::from_millis(MAX_HERDR_TIMEOUT_MS))
        } else {
            timeout.min(self.timeout)
        }
    }

    fn output_bounded(
        &self,
        args: &[&str],
        timeout: Duration,
        cancelled: &AtomicBool,
        max_output_bytes: usize,
    ) -> Result<Output, String> {
        if cancelled.load(Ordering::Relaxed) {
            return Err("Herdr read cancelled".to_owned());
        }
        let timeout = self.bounded_timeout(timeout);
        let hc_deadline = (self.host == HostKind::Hc).then(|| Instant::now() + timeout);
        let mut command = match hc_deadline {
            Some(deadline) => self.checked_command_until(deadline, cancelled)?,
            None => self.command(),
        };
        if self.host == HostKind::Hc && args.first() == Some(&"run") {
            let separator = args
                .iter()
                .position(|argument| *argument == "--")
                .filter(|index| *index + 1 < args.len())
                .ok_or("hc run requires an executable after --")?;
            let executable = match &self.child_executable {
                Some(executable) => executable.clone(),
                None => env::current_exe().map_err(|error| error.to_string())?,
            };
            command
                .args(&args[..=separator])
                .arg(executable)
                .arg("--hc-exec-child")
                .arg("--")
                .args(&args[separator + 1..]);
        } else {
            command.args(args);
        }
        command.process_group(0);
        let stdout = self.output_file("stdout")?;
        let stderr = self.output_file("stderr")?;
        if let Some(deadline) = hc_deadline {
            check_hc_budget(deadline, cancelled)?;
        }
        let mut child = command
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout.try_clone().map_err(|error| {
                format!("failed to capture Herdr command stdout: {error}")
            })?))
            .stderr(Stdio::from(stderr.try_clone().map_err(|error| {
                format!("failed to capture Herdr command stderr: {error}")
            })?))
            .spawn()
            .map_err(|error| format!("failed to run {}: {error}", self.binary))?;
        let deadline = hc_deadline.unwrap_or_else(|| Instant::now() + timeout);
        let status = loop {
            let oversized = [&stdout, &stderr].iter().any(|file| {
                file.metadata()
                    .map(|meta| meta.len() > max_output_bytes as u64)
                    .unwrap_or(true)
            });
            if cancelled.load(Ordering::Relaxed) || oversized {
                stop_process_group(&mut child);
                return Err(if oversized {
                    "Herdr read exceeded its output limit"
                } else {
                    "Herdr read cancelled"
                }
                .to_owned());
            }
            if let Some(status) = child
                .try_wait()
                .map_err(|error| format!("failed to wait for {}: {error}", self.binary))?
            {
                break status;
            }
            if Instant::now() >= deadline {
                stop_process_group(&mut child);
                return Err(format!(
                    "Herdr command timed out after {} ms",
                    timeout.as_millis()
                ));
            }
            thread::sleep(Duration::from_millis(5));
        };
        if [&stdout, &stderr].iter().any(|file| {
            file.metadata()
                .map(|meta| meta.len() > max_output_bytes as u64)
                .unwrap_or(true)
        }) {
            return Err("Herdr read exceeded its output limit".to_owned());
        }
        let stdout = self.read_output(stdout, "stdout", max_output_bytes)?;
        let stderr = self.read_output(stderr, "stderr", max_output_bytes)?;
        Ok(Output {
            status,
            stdout,
            stderr,
        })
    }
}

pub(crate) fn remove_tui_environment(command: &mut Command) {
    use std::os::unix::ffi::OsStrExt;
    for (key, _) in env::vars_os() {
        if key.as_bytes().starts_with(b"HERDR_") {
            command.env_remove(key);
        }
    }
}

pub fn exec_hc_child(arguments: &[std::ffi::OsString]) -> std::io::Error {
    let Some((program, arguments)) = arguments.split_first() else {
        return std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "hc child executable is missing",
        );
    };
    let mut command = Command::new(program);
    command.args(arguments);
    remove_tui_environment(&mut command);
    command.exec()
}

impl HerdrCommandExecutor for ProcessHerdr {
    fn environment_snapshot(&self) -> Option<&crate::environment::EnvironmentSnapshot> {
        Some(&self.environment)
    }

    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let output = self.output(args)?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        Ok(HerdrCommandOutput {
            stdout: String::from_utf8(output.stdout).map_err(|error| error.to_string())?,
        })
    }

    fn execute_bounded(
        &self,
        args: &[&str],
        timeout: Duration,
        cancelled: &AtomicBool,
        max_output_bytes: usize,
    ) -> Result<HerdrCommandOutput, String> {
        let output = self.output_bounded(args, timeout, cancelled, max_output_bytes)?;
        if !output.status.success() {
            return Err(String::from_utf8_lossy(&output.stderr).trim().to_owned());
        }
        Ok(HerdrCommandOutput {
            stdout: String::from_utf8(output.stdout).map_err(|error| error.to_string())?,
        })
    }

    fn classify_prompt_failure(&self, error: &str) -> crate::PromptFailureKind {
        let code = serde_json::from_str::<serde_json::Value>(error)
            .ok()
            .and_then(|value| {
                value
                    .pointer("/error/code")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned)
            });
        match code.as_deref() {
            // `herdr agent prompt --help` (0.8.2): "If the agent is already
            // blocked, submission is rejected with `agent_blocked` before any
            // input is sent." Like `agent_busy`, the recipient never saw the
            // prompt, so requeueing cannot duplicate an invocation.
            Some(
                "agent_not_found" | "pane_not_found" | "agent_not_ready" | "agent_busy"
                | "agent_blocked" | "conflict" | "server_busy",
            ) => crate::PromptFailureKind::TemporaryPreInvocation,
            Some(
                "invalid_request" | "invalid_args" | "invalid_input" | "unsupported"
                | "unknown_command",
            ) => crate::PromptFailureKind::DefinitiveNonRetryable,
            _ => crate::PromptFailureKind::AmbiguousInvocation,
        }
    }
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now_unix_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must follow the Unix epoch")
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

pub struct ProcessIds {
    sequence: AtomicU64,
}

impl Default for ProcessIds {
    fn default() -> Self {
        Self {
            sequence: AtomicU64::new(1),
        }
    }
}

impl IdGenerator for ProcessIds {
    fn next_id(&self, kind: &str) -> String {
        let unix_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock must follow the Unix epoch")
            .as_nanos();
        format!(
            "{kind}-{unix_nanos}-{}-{}",
            std::process::id(),
            self.sequence.fetch_add(1, Ordering::Relaxed)
        )
    }
}

/// Kills `child` and its whole process group. Each child runs in its own
/// group, so cancellation also stops descendants that inherited the private
/// output descriptors. The signal goes through the syscall because procps
/// `/bin/kill` parses `-<pid>` as an option and signals the wrong group.
pub(crate) fn stop_process_group(child: &mut Child) {
    if let Some(pid) = libc::pid_t::try_from(child.id())
        .ok()
        .filter(|pid| *pid > 1)
    {
        // SAFETY: kill(2) only takes a pid and a signal.
        unsafe { libc::kill(-pid, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

#[cfg(test)]
#[path = "../tests/support/hc_socket.rs"]
pub(crate) mod hc_socket_fixture;

#[cfg(test)]
mod bounded_read_tests {
    use super::*;

    #[test]
    fn malformed_hc_socket_paths_remain_invalid() {
        for path in ["", "bad\0socket"] {
            let error = connect_hc_socket(
                Path::new(path),
                Instant::now() + Duration::from_secs(1),
                &AtomicBool::new(false),
            )
            .unwrap_err();
            assert_eq!(error, format!("Invalid hc socket path: {path}"));
        }
    }

    fn run_with_hc_socket(test: &str) -> bool {
        if env::var("NAMEDROP_TEST_HC_SOCKET").as_deref() == Ok(test) {
            return false;
        }
        let root = env::temp_dir().join(format!("nd-runtime-{}-{test}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("hc.sock");
        let listener = hc_socket_fixture::HcSocket::bind(&path).unwrap();
        use crate::test_environment::CommandHome;
        let output = Command::new(env::current_exe().unwrap())
            .isolated_home(root.join("home"))
            .args([
                "--exact",
                &format!("runtime::bounded_read_tests::{test}"),
                "--nocapture",
            ])
            .env("HC_SOCKET", &path)
            .env("HC_SESSION", "default")
            .env("NAMEDROP_TEST_HC_SOCKET", test)
            .output()
            .unwrap();
        drop(listener);
        fs::remove_dir_all(root).unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        true
    }

    #[test]
    fn hc_commands_remove_terminal_identity_but_retain_core_routing() {
        let executor = ProcessHerdr::for_host(HostKind::Hc);
        let command = executor.command();
        use std::os::unix::ffi::OsStrExt;
        assert!(command.get_envs().all(|(key, value)| value.is_none()
            || (!key.as_bytes().starts_with(b"HERDR_")
                && key != "HC_TERMINAL_ID"
                && key != "HC_TERMINAL_SOCKET")));
        if let Ok(endpoint) = executor.environment.hc_endpoint() {
            assert!(
                command
                    .get_envs()
                    .any(|(key, value)| key == "HC_SOCKET" && value == Some(endpoint.as_os_str()))
            );
        }
    }

    #[test]
    fn hc_bounded_reads_use_the_remaining_budget() {
        if run_with_hc_socket("hc_bounded_reads_use_the_remaining_budget") {
            return;
        }
        let executor = ProcessHerdr {
            binary: "/bin/sh".into(),
            host: HostKind::Hc,
            timeout: Duration::from_millis(10),
            socket: None,
            child_executable: None,
            environment: crate::environment::EnvironmentSnapshot::capture(),
        };
        assert_eq!(
            executor.bounded_timeout(Duration::from_secs(120)),
            Duration::from_secs(60)
        );
        assert_eq!(
            executor.bounded_timeout(Duration::MAX),
            Duration::from_secs(60)
        );
        let output = executor
            .execute_bounded(
                &["-c", "sleep 0.05; printf done"],
                Duration::from_secs(1),
                &AtomicBool::new(false),
                100,
            )
            .unwrap();
        assert_eq!(output.stdout, "done");
        assert!(
            executor
                .execute_bounded(
                    &["-c", "sleep 1"],
                    Duration::from_millis(20),
                    &AtomicBool::new(false),
                    100
                )
                .unwrap_err()
                .contains("timed out after 20 ms")
        );
    }

    #[test]
    fn only_hc_sends_receive_the_fixed_twenty_second_deadline() {
        for host in [HostKind::Hc, HostKind::Tui] {
            for seconds in [1, 10, 30, 60] {
                let configured = Duration::from_secs(seconds);
                let executor = ProcessHerdr {
                    host,
                    timeout: configured,
                    ..ProcessHerdr::default()
                };
                let send_timeout = executor
                    .bounded_timeout(executor.command_timeout(&["send", "t1", "hello", "--json"]));
                assert_eq!(
                    send_timeout,
                    if host == HostKind::Hc {
                        Duration::from_secs(20)
                    } else {
                        configured
                    },
                    "{host:?} configured for {seconds} seconds"
                );
                assert_eq!(
                    executor.command_timeout(&["get", "t1", "--json"]),
                    configured
                );
                assert_eq!(
                    executor.bounded_timeout(Duration::from_secs(120)),
                    if host == HostKind::Hc {
                        Duration::from_secs(60)
                    } else {
                        configured
                    }
                );
            }
        }
    }

    #[test]
    fn hc_send_survives_the_fixed_server_waits() {
        if run_with_hc_socket("hc_send_survives_the_fixed_server_waits") {
            return;
        }
        let root = env::temp_dir().join(format!("namedrop-hc-send-timeout-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let binary = root.join("fake-hc");
        fs::write(&binary, "#!/bin/sh\nsleep 10.3\nprintf '{}'\n").unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o700)).unwrap();
        let executor = ProcessHerdr {
            binary: binary.to_string_lossy().into_owned(),
            host: HostKind::Hc,
            timeout: Duration::from_millis(DEFAULT_HERDR_TIMEOUT_MS),
            socket: None,
            child_executable: None,
            environment: crate::environment::EnvironmentSnapshot::capture(),
        };
        let result = executor.execute(&["send", "t1", "hello", "--json"]);
        fs::remove_dir_all(root).unwrap();
        assert!(result.is_ok(), "{result:?}");
    }

    #[test]
    fn bounded_reads_enforce_output_deadline_and_cancellation() {
        let executor = ProcessHerdr {
            binary: "/bin/sh".to_owned(),
            host: HostKind::Tui,
            timeout: Duration::from_secs(2),
            socket: None,
            child_executable: None,
            environment: crate::environment::EnvironmentSnapshot::capture(),
        };
        let cancelled = AtomicBool::new(false);
        let deadline = Duration::from_millis(80);
        assert_eq!(
            executor
                .execute_bounded(&["-c", "printf ok"], deadline, &cancelled, 2)
                .unwrap()
                .stdout,
            "ok"
        );
        assert!(
            executor
                .execute_bounded(&["-c", "printf too-long"], deadline, &cancelled, 2)
                .unwrap_err()
                .contains("output limit")
        );
        let start = Instant::now();
        assert!(
            executor
                .execute_bounded(&["-c", "sleep 10"], deadline, &cancelled, 100)
                .unwrap_err()
                .contains("timed out")
        );
        assert!(start.elapsed() < Duration::from_secs(1));
        thread::scope(|scope| {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(30));
                cancelled.store(true, Ordering::Relaxed);
            });
            assert!(
                executor
                    .execute_bounded(&["-c", "sleep 10"], Duration::from_secs(1), &cancelled, 100)
                    .unwrap_err()
                    .contains("cancelled")
            );
        });
    }

    #[test]
    fn stopping_a_child_kills_its_whole_process_group_and_nothing_else() {
        let pid_file = env::temp_dir().join(format!(
            ".namedrop-process-group-test-{}",
            std::process::id()
        ));
        let _ = fs::remove_file(&pid_file);
        let mut sentinel = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let mut child = Command::new("sh")
            .args([
                "-c",
                &format!("sleep 30 & echo $! > '{}'; wait", pid_file.display()),
            ])
            .process_group(0)
            .spawn()
            .unwrap();
        let grandchild = wait_until(Duration::from_secs(5), || {
            fs::read_to_string(&pid_file)
                .ok()
                .and_then(|text| text.trim().parse::<u32>().ok())
        })
        .expect("the child never reported its grandchild");
        let _ = fs::remove_file(&pid_file);

        stop_process_group(&mut child);

        assert!(
            wait_until(Duration::from_secs(5), || (!is_running(grandchild))
                .then_some(()))
            .is_some(),
            "grandchild {grandchild} survived stopping the child's process group"
        );
        assert!(
            sentinel.try_wait().unwrap().is_none(),
            "a process outside the child's group was killed"
        );
        let _ = sentinel.kill();
        let _ = sentinel.wait();
    }

    fn wait_until<T>(limit: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
        let deadline = Instant::now() + limit;
        loop {
            if let Some(value) = probe() {
                return Some(value);
            }
            if Instant::now() >= deadline {
                return None;
            }
            thread::sleep(Duration::from_millis(10));
        }
    }

    /// A reaped or zombie process no longer runs, which is what `kill -KILL`
    /// must achieve whether or not an init has collected it yet.
    fn is_running(pid: u32) -> bool {
        let state = Command::new("ps")
            .args(["-o", "stat=", "-p", &pid.to_string()])
            .output()
            .unwrap()
            .stdout;
        let state = String::from_utf8_lossy(&state);
        let state = state.trim();
        !state.is_empty() && !state.starts_with('Z')
    }
}

#[cfg(test)]
mod host_selection_tests {
    use super::*;

    #[test]
    fn detection_obeys_explicit_then_innermost_host() {
        let cases = [
            (None, vec![], Some(HostKind::Tui)),
            (None, vec!["HC_SESSION", "HC_SOCKET"], Some(HostKind::Tui)),
            (None, vec!["HC_TERMINAL_ID"], Some(HostKind::Hc)),
            (None, vec!["HERDR_SOCKET_PATH"], Some(HostKind::Tui)),
            (
                Some(HostKind::Hc),
                vec!["HERDR_PANE_ID"],
                Some(HostKind::Hc),
            ),
            (
                Some(HostKind::Tui),
                vec!["HC_TERMINAL_ID"],
                Some(HostKind::Tui),
            ),
        ];
        for (explicit, keys, expected) in cases {
            assert_eq!(
                HostKind::detect(
                    HostSignals {
                        host: explicit,
                        ..HostSignals::default()
                    },
                    |key| { keys.contains(&key).then(|| "value".into()) }
                )
                .ok(),
                expected
            );
        }
    }

    #[test]
    fn tui_only_detection_reads_one_environment_variable() {
        let reads = std::cell::RefCell::new(Vec::new());
        assert_eq!(
            HostKind::detect(HostSignals::default(), |key| {
                reads.borrow_mut().push(key.to_owned());
                (key == "HERDR_SOCKET_PATH").then(|| "socket".into())
            })
            .unwrap(),
            HostKind::Tui
        );
        assert_eq!(*reads.borrow(), ["HC_TERMINAL_ID"]);
    }

    #[test]
    fn explicit_host_and_transport_flags_do_not_read_environment() {
        for (explicit, tui, hc, state, expected) in [
            (Some(HostKind::Hc), true, true, false, None),
            (Some(HostKind::Tui), true, true, false, None),
            (Some(HostKind::Hc), true, false, false, None),
            (Some(HostKind::Tui), false, true, false, None),
            (Some(HostKind::Hc), false, false, true, Some(HostKind::Hc)),
            (Some(HostKind::Hc), false, true, true, Some(HostKind::Hc)),
            (Some(HostKind::Tui), false, false, true, Some(HostKind::Tui)),
            (None, true, false, false, Some(HostKind::Tui)),
            (None, false, true, false, Some(HostKind::Hc)),
            (None, false, false, true, Some(HostKind::Tui)),
            (None, false, true, true, None),
            (None, true, true, false, None),
        ] {
            assert_eq!(
                HostKind::detect(
                    HostSignals {
                        host: explicit,
                        tui_transport: tui,
                        hc_transport: hc,
                        plugin_state: state
                    },
                    |_| panic!("unexpected environment read")
                )
                .ok(),
                expected
            );
        }
    }

    #[test]
    fn hc_state_uses_xdg_and_default_session() {
        let vars = |key: &str| match key {
            "XDG_STATE_HOME" => Some("/state".into()),
            "HOME" => Some("/home/test".into()),
            _ => None,
        };
        assert_eq!(
            hc_state_directory(vars).unwrap(),
            std::path::Path::new("/state/namedrop/hc/default")
        );
        assert_eq!(
            hc_state_directory(|key| match key {
                "HOME" => Some("/home/test".into()),
                "HC_SESSION" => Some(" lane ".into()),
                _ => None,
            })
            .unwrap(),
            std::path::Path::new("/home/test/.local/state/namedrop/hc/lane")
        );
        assert!(
            hc_state_directory(|key| Some(
                if key == "HC_SESSION" {
                    "../escape"
                } else {
                    "/state"
                }
                .into()
            ))
            .is_err()
        );
    }
}
