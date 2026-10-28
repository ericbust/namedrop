use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsFd, AsRawFd, FromRawFd, OwnedFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
            net::UnixStream,
        },
    },
    path::{Component, Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant, SystemTime},
};

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) enum UiSource {
    #[default]
    Embedded,
    Directory(PathBuf),
}

impl UiSource {
    pub fn from_directory(directory: Option<&Path>) -> Result<Self, String> {
        let Some(path) = directory else {
            return Ok(Self::Embedded);
        };
        let canonical = fs::canonicalize(path)
            .map_err(|error| format!("dashboard UI directory {}: {error}", path.display()))?;
        if path.to_str().is_none() || canonical.to_str().is_none() {
            return Err(format!(
                "dashboard UI directory {} is not valid UTF-8",
                path.display()
            ));
        }
        open_directory(&canonical)
            .map_err(|error| format!("dashboard UI directory {}: {error}", path.display()))?;
        Ok(Self::Directory(canonical))
    }

    pub fn setting(&self) -> String {
        match self {
            Self::Embedded => "embedded UI (without --ui)".to_owned(),
            Self::Directory(path) => format!("--ui {}", path.display()),
        }
    }
}

const ENTRY_TTL: Duration = Duration::from_secs(10 * 60);
const CLAIM_READY: u8 = b'R';

struct Successor {
    executable: PathBuf,
    inherits_claim: bool,
    supports_ui: bool,
}

impl Successor {
    fn select(state: &Path, ui: &UiSource) -> Result<Self, String> {
        let runtime = state.join("runtime");
        let executable = if runtime.join("namedrop").exists() {
            fs::canonicalize(runtime.join("current/namedrop")).map_err(|e| e.to_string())?
        } else {
            fs::canonicalize(crate::runtime_executable()?).map_err(|e| e.to_string())?
        };
        let inherits_claim = probe_capability(&executable, "--inherit-claim")?;
        let supports_ui =
            matches!(ui, UiSource::Embedded) || probe_capability(&executable, "--ui")?;
        Ok(Self {
            executable,
            inherits_claim,
            supports_ui,
        })
    }
}

fn probe_capability(executable: &Path, flag: &str) -> Result<bool, String> {
    use std::os::unix::process::CommandExt;
    let (mut reader, writer) = UnixStream::pair().map_err(|e| e.to_string())?;
    reader.set_nonblocking(true).map_err(|e| e.to_string())?;
    let stderr = writer.try_clone().map_err(|e| e.to_string())?;
    let mut command = Command::new(executable);
    command.args(["dashboard-server", flag]);
    if flag == "--ui" {
        command.arg(".");
    }
    let mut child = command
        .arg("--help")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::from(OwnedFd::from(writer)))
        .stderr(Stdio::from(OwnedFd::from(stderr)))
        .spawn()
        .map_err(|e| {
            format!(
                "could not probe dashboard successor {}: {e}",
                executable.display()
            )
        })?;
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut output = Vec::new();
    let result = loop {
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => break Err(error.to_string()),
        };
        let mut buffer = [0; 4096];
        let read = reader.read(&mut buffer);
        let drained = matches!(read, Ok(0))
            || matches!(&read, Err(e) if e.kind() == io::ErrorKind::WouldBlock);
        match read {
            Ok(count) if output.len() + count > 64 * 1024 => {
                break Err(
                    "dashboard successor capability probe exceeded its output limit".to_owned(),
                );
            }
            Ok(count) => output.extend_from_slice(&buffer[..count]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => break Err(error.to_string()),
        }
        if drained {
            if let Some(status) = status {
                break capability_supported(status, &output, flag);
            }
            thread::sleep(Duration::from_millis(10));
        }
        if Instant::now() >= deadline {
            break Err("dashboard successor capability probe timed out after 5 seconds".to_owned());
        }
    };
    if result.is_err() {
        let _ = unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
        let _ = child.wait();
    }
    result
}

fn capability_supported(status: ExitStatus, output: &[u8], flag: &str) -> Result<bool, String> {
    if status.success() {
        return Ok(true);
    }
    let output = String::from_utf8_lossy(output);
    if output.contains(&format!("error: unexpected argument '{flag}' found")) {
        return Ok(false);
    }
    Err(format!(
        "dashboard successor capability probe failed ({status}): {output}"
    ))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ServerInfo {
    pub pid: u32,
    pub port: u16,
    pub instance: String,
    pub started_at_ms: i64,
    #[serde(default)]
    pub ui: UiSource,
}

/// Held for the server's lifetime; the kernel releases it on any exit, so a
/// crashed server never leaves a stale claim behind.
pub(crate) struct SingletonLock {
    file: File,
}

impl SingletonLock {
    pub fn notify_predecessor_ready(&self) -> Result<(), String> {
        let mut ready = io::stdout().lock();
        ready
            .write_all(&[CLAIM_READY])
            .and_then(|()| ready.flush())
            .map_err(|e| e.to_string())
    }
}

#[derive(Clone)]
pub(crate) struct Token(String);

impl Token {
    pub fn matches(&self, candidate: &str) -> bool {
        let (a, b) = (self.0.as_bytes(), candidate.as_bytes());
        a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

pub(crate) struct ServerFiles {
    state: PathBuf,
    dir: PathBuf,
}

impl ServerFiles {
    pub fn at(state: &Path) -> Result<Self, String> {
        let dir = state.join("dashboard");
        fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())?;
        Ok(Self {
            state: state.to_path_buf(),
            dir,
        })
    }

    pub fn state(&self) -> &Path {
        &self.state
    }

    fn lock_file(&self) -> Result<File, String> {
        private_file(&self.dir.join("server.lock"))
    }

    /// `None` while another server holds the claim.
    pub fn claim(&self) -> Result<Option<SingletonLock>, String> {
        let file = self.lock_file()?;
        match file.try_lock() {
            Ok(()) => Ok(Some(SingletonLock { file })),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(error)) => Err(error.to_string()),
        }
    }

    pub fn inherit_claim(&self) -> Result<SingletonLock, String> {
        let file = File::from(
            io::stdin()
                .as_fd()
                .try_clone_to_owned()
                .map_err(|e| e.to_string())?,
        );
        let inherited = file.metadata().map_err(|e| e.to_string())?;
        let expected = self.lock_file()?.metadata().map_err(|e| e.to_string())?;
        if (inherited.dev(), inherited.ino()) != (expected.dev(), expected.ino()) {
            return Err("inherited dashboard claim does not match server.lock".to_owned());
        }
        file.try_lock()
            .map_err(|e| format!("could not acquire inherited dashboard claim: {e}"))?;
        let null = File::open("/dev/null").map_err(|e| e.to_string())?;
        // Keep only the owned claim so a later legacy handoff can release it.
        if unsafe { libc::dup2(null.as_raw_fd(), libc::STDIN_FILENO) } < 0 {
            return Err(io::Error::last_os_error().to_string());
        }
        Ok(SingletonLock { file })
    }

    pub fn claimed(&self) -> Result<bool, String> {
        match self.lock_file()?.try_lock_shared() {
            Ok(()) => Ok(false),
            Err(fs::TryLockError::WouldBlock) => Ok(true),
            Err(fs::TryLockError::Error(error)) => Err(error.to_string()),
        }
    }

    pub fn publish(&self, _claim: &SingletonLock, info: &ServerInfo) -> Result<(), String> {
        let bytes = serde_json::to_vec(info).map_err(|e| e.to_string())?;
        self.write_atomic("server.json", &bytes)
    }

    pub fn info(&self) -> Option<ServerInfo> {
        serde_json::from_slice(&fs::read(self.dir.join("server.json")).ok()?).ok()
    }

    pub fn token(&self) -> Result<Token, String> {
        self.secret("token")
    }

    /// Kept across a handoff so open tabs survive an upgrade.
    pub fn session(&self) -> Result<Token, String> {
        self.secret("session")
    }

    /// A tab that outlives an idle exit keeps retrying the freed port; a
    /// new session means whoever binds it next receives a dead cookie.
    pub fn rotate_session(&self) -> Result<(), String> {
        match fs::remove_file(self.dir.join("session")) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error.to_string()),
            _ => Ok(()),
        }
    }

    fn entries(&self) -> PathBuf {
        self.dir.join("entry")
    }

    /// Mints the one-shot nonce an entry URL carries, so argv, terminal
    /// output and browser history hold nothing that opens a page later.
    pub fn mint_entry(&self) -> Result<String, String> {
        let entries = self.entries();
        fs::create_dir_all(&entries).map_err(|e| format!("{}: {e}", entries.display()))?;
        fs::set_permissions(&entries, fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        for entry in fs::read_dir(&entries).into_iter().flatten().flatten() {
            if !fresh(&entry.path()) {
                let _ = fs::remove_file(entry.path());
            }
        }
        let nonce = random_hex(16)?;
        OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(entries.join(&nonce))
            .map_err(|e| e.to_string())?;
        Ok(nonce)
    }

    /// True once per minted nonce: removing the file is the atomic claim.
    pub fn consume_entry(&self, nonce: &str) -> bool {
        if nonce.len() != 32
            || !nonce
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            return false;
        }
        let path = self.entries().join(nonce);
        let fresh = fresh(&path);
        fs::remove_file(&path).is_ok() && fresh
    }

    fn secret(&self, name: &str) -> Result<Token, String> {
        let path = self.dir.join(name);
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(mut file) => {
                let token = random_hex(32)?;
                file.write_all(token.as_bytes())
                    .and_then(|()| file.sync_all())
                    .map_err(|e| e.to_string())?;
                Ok(Token(token))
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                // A concurrent creator may not have written it yet.
                for _ in 0..20 {
                    let token = fs::read_to_string(&path).map_err(|e| e.to_string())?;
                    if token.len() == 64 {
                        return Ok(Token(token));
                    }
                    thread::sleep(Duration::from_millis(25));
                }
                Err(format!(
                    "dashboard {name} file is malformed; delete it to rotate"
                ))
            }
            Err(error) => Err(error.to_string()),
        }
    }

    pub fn labels(&self) -> HashMap<String, String> {
        fs::read(self.dir.join("labels.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// The CLI is the only writer; the server only reads.
    pub fn record_label(&self, key: &str, label: &str) -> Result<(), String> {
        let mut labels = self.labels();
        if labels.get(key).map(String::as_str) == Some(label) {
            return Ok(());
        }
        labels.insert(key.to_owned(), label.to_owned());
        let bytes = serde_json::to_vec(&labels).map_err(|e| e.to_string())?;
        self.write_atomic("labels.json", &bytes)
    }

    fn write_atomic(&self, name: &str, bytes: &[u8]) -> Result<(), String> {
        let target = self.dir.join(name);
        let temporary = self.dir.join(format!(".{name}.{}.tmp", std::process::id()));
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .map_err(|e| e.to_string())?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .and_then(|()| fs::rename(&temporary, &target))
            .map_err(|e| {
                let _ = fs::remove_file(&temporary);
                e.to_string()
            })
    }

    pub fn restarting(&self) -> bool {
        let runtime = self.state.join("runtime");
        fs::symlink_metadata(runtime.join("migration.json")).is_ok()
            || (runtime.join("namedrop").exists()
                && fs::canonicalize(runtime.join("current/namedrop")).ok()
                    != std::env::current_exe().and_then(fs::canonicalize).ok())
    }

    pub fn handoff(
        &self,
        ui: &UiSource,
        claim: SingletonLock,
        retire_listener: impl FnOnce() -> Result<(), String>,
    ) -> Result<(), String> {
        let mut log = self.server_log()?;
        let result = (|| {
            let _gate = crate::runtime_command_guard(&self.state, Some(Duration::from_secs(60)))?;
            let runtime = self.state.join("runtime");
            let packages = runtime
                .is_dir()
                .then(|| private_file(&runtime.join("packages.lock")))
                .transpose()?;
            if let Some(packages) = &packages {
                lock_for_handoff(packages, "runtime packages")?;
            }
            let selected = Successor::select(&self.state, ui)?;
            let phase = private_file(&self.dir.join("handoff.lock"))?;
            lock_for_handoff(&phase, "dashboard handoff")?;
            retire_listener()?;
            let previous = self.info();
            let effective_ui = if selected.supports_ui {
                ui
            } else {
                writeln!(
                    log,
                    "dashboard: dropping custom UI because successor {} does not support --ui",
                    selected.executable.display()
                )
                .map_err(|e| e.to_string())?;
                &UiSource::Embedded
            };
            let claim = if selected.inherits_claim {
                Some(claim)
            } else {
                writeln!(log, "dashboard: successor {} rejected --inherit-claim; using legacy handoff compatibility", selected.executable.display()).map_err(|e| e.to_string())?;
                drop(claim);
                None
            };
            let token = self.token()?;
            let mut child =
                self.spawn_selected(&selected.executable, effective_ui, claim.as_ref())?;
            let deadline = Instant::now() + Duration::from_secs(65);
            let result = loop {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        break Err(format!(
                            "dashboard successor exited before becoming ready: {status}"
                        ));
                    }
                    Ok(None) => {}
                    Err(error) => break Err(error.to_string()),
                }
                if let Some(info) = self.info()
                    && info.pid == child.id()
                    && previous
                        .as_ref()
                        .is_none_or(|old| info.instance != old.instance)
                    && super::answers(&info, &token).is_some()
                {
                    break Ok(());
                }
                if Instant::now() >= deadline {
                    break Err(
                        "dashboard successor did not become ready within 65 seconds".to_owned()
                    );
                }
                thread::sleep(Duration::from_millis(50));
            };
            if result.is_err() {
                let _ = unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
                let _ = child.wait();
            }
            result
        })();
        if let Err(error) = &result {
            let _ = writeln!(log, "dashboard: handoff failed: {error}");
        }
        result
    }

    pub fn handoff_reader(&self) -> Result<Option<File>, String> {
        let file = private_file(&self.dir.join("handoff.lock"))?;
        match file.try_lock_shared() {
            Ok(()) => Ok(Some(file)),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(error)) => Err(error.to_string()),
        }
    }

    fn server_log(&self) -> Result<File, String> {
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.dir.join("server.log"))
            .map_err(|e| e.to_string())
    }

    pub fn spawn(&self, ui: &UiSource) -> Result<(), String> {
        self.spawn_selected(&crate::runtime_executable()?, ui, None)?;
        Ok(())
    }

    fn spawn_selected(
        &self,
        executable: &Path,
        ui: &UiSource,
        claim: Option<&SingletonLock>,
    ) -> Result<std::process::Child, String> {
        use std::os::unix::process::CommandExt;
        let log = self.server_log()?;
        let mut command = Command::new(executable);
        command
            .arg("--plugin-state")
            .arg(&self.state)
            .arg("dashboard-server");
        if let UiSource::Directory(path) = ui {
            command.arg("--ui").arg(path);
        }
        let stdin = match claim {
            Some(claim) => {
                command.arg("--inherit-claim");
                Stdio::from(claim.file.try_clone().map_err(|e| e.to_string())?)
            }
            None => Stdio::null(),
        };
        command
            .process_group(0)
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_SOCKET_PATH")
            .stdin(stdin)
            .stdout(log.try_clone().map_err(|e| e.to_string())?)
            .stderr(log)
            .spawn()
            .map_err(|e| format!("could not start the dashboard server: {e}"))
    }
}

fn lock_for_handoff(file: &File, name: &str) -> Result<(), String> {
    let deadline = Instant::now() + Duration::from_secs(65);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(()),
            Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                thread::sleep(Duration::from_millis(50));
            }
            Err(fs::TryLockError::WouldBlock) => {
                return Err(format!(
                    "timed out waiting for {name} lock after 65 seconds"
                ));
            }
            Err(fs::TryLockError::Error(error)) => return Err(error.to_string()),
        }
    }
}

fn fresh(path: &Path) -> bool {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .is_ok_and(|modified| {
            SystemTime::now()
                .duration_since(modified)
                .is_ok_and(|age| age < ENTRY_TTL)
        })
}

fn private_file(path: &Path) -> Result<File, String> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(|e| format!("{}: {e}", path.display()))
}

pub(crate) fn random_hex(bytes: usize) -> Result<String, String> {
    let mut buffer = vec![0u8; bytes];
    File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut buffer))
        .map_err(|e| format!("could not read randomness: {e}"))?;
    Ok(buffer.iter().map(|byte| format!("{byte:02x}")).collect())
}

pub(super) fn open_directory(path: &Path) -> io::Result<File> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "directory path must be absolute",
        ));
    }
    #[cfg(target_os = "linux")]
    let search = libc::O_PATH;
    #[cfg(target_os = "macos")]
    let search = libc::O_SEARCH;
    let flags = search | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
    let fd = unsafe { libc::open(c"/".as_ptr(), flags) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut directory = unsafe { File::from_raw_fd(fd) };
    for component in path.components() {
        let Component::Normal(name) = component else {
            if component == Component::RootDir {
                continue;
            }
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid directory component",
            ));
        };
        let name = CString::new(name.as_bytes())?;
        let fd = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // openat returned a new owned descriptor.
        directory = unsafe { File::from_raw_fd(fd) };
    }
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            c".".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;

    #[test]
    fn capabilities_require_success_or_explicit_flag_rejection() {
        for flag in ["--inherit-claim", "--ui"] {
            assert!(
                capability_supported(ExitStatus::from_raw(0), b"Usage: namedrop", flag).unwrap()
            );
            let old = format!("error: unexpected argument '{flag}' found\n");
            assert!(
                !capability_supported(ExitStatus::from_raw(64 << 8), old.as_bytes(), flag).unwrap()
            );
            for output in [
                b"error: unexpected argument '--other' found".as_slice(),
                b"could not acquire --inherit-claim",
                b"dashboard successor closed its readiness pipe",
            ] {
                assert!(capability_supported(ExitStatus::from_raw(69 << 8), output, flag).is_err());
            }
        }
    }
}
