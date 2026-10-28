use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    ffi::CString,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsFd, AsRawFd, FromRawFd},
        unix::{
            ffi::OsStrExt,
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Component, Path, PathBuf},
    process::{Command, Stdio},
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
        // Stdin must survive the launcher exec, but not helpers spawned by this server.
        let flags = unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_GETFD) };
        if flags < 0
            || unsafe { libc::fcntl(libc::STDIN_FILENO, libc::F_SETFD, flags | libc::FD_CLOEXEC) }
                < 0
        {
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

    pub fn spawn(&self, ui: &UiSource, claim: Option<&SingletonLock>) -> Result<(), String> {
        use std::os::unix::process::CommandExt;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.dir.join("server.log"))
            .map_err(|e| e.to_string())?;
        let mut command = Command::new(crate::runtime_executable()?);
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
        let mut child = command
            .process_group(0)
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_SOCKET_PATH")
            .stdin(stdin)
            .stdout(if claim.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stderr(log)
            .spawn()
            .map_err(|e| format!("could not start the dashboard server: {e}"))?;
        let Some(mut ready) = child.stdout.take() else {
            return Ok(());
        };
        let deadline = Instant::now() + Duration::from_secs(65);
        let result = loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    break Err(format!(
                        "dashboard successor exited before adopting its claim: {status}"
                    ));
                }
                Ok(None) => {}
                Err(error) => break Err(error.to_string()),
            }
            let mut descriptor = libc::pollfd {
                fd: ready.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let polled = unsafe { libc::poll(&mut descriptor, 1, 50) };
            if polled < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                break Err(error.to_string());
            }
            if polled > 0 {
                let mut byte = [0];
                break match ready.read(&mut byte) {
                    Ok(1) if byte[0] == CLAIM_READY => Ok(()),
                    Ok(_) => Err("dashboard successor closed its readiness pipe".to_owned()),
                    Err(error) => Err(error.to_string()),
                };
            }
            if Instant::now() >= deadline {
                break Err(
                    "dashboard successor did not adopt its claim within 65 seconds".to_owned(),
                );
            }
        };
        if result.is_err() {
            // Launcher helpers can inherit the claim too, so stop the whole child group.
            let _ = unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
            let _ = child.wait();
        }
        result
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
    let mut directory = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")?;
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
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // openat returned a new owned descriptor.
        directory = unsafe { File::from_raw_fd(fd) };
    }
    Ok(directory)
}
