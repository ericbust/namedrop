//! Everything under `<state>/dashboard/`: the singleton lock, the published
//! `server.json`, the secrets, one-shot entry nonces, workspace labels, and
//! the log.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, SystemTime},
};

/// How long a printed entry link stays usable before its first visit.
const ENTRY_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct ServerInfo {
    pub pid: u32,
    pub port: u16,
    pub instance: String,
    pub started_at_ms: i64,
}

/// Held for the server's lifetime; the kernel releases it on any exit, so a
/// crashed server never leaves a stale claim behind.
pub(crate) struct SingletonLock {
    _file: File,
}

/// A 32-byte hex secret, compared in constant time and never logged. The
/// `token` is shared only by the CLI and the server; the browser's cookie is
/// the separate `session`, so neither ever leaves the state directory as the other.
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

    /// `None` while another server holds the claim. A predecessor releases
    /// it before spawning its successor, so there is nothing to wait for.
    pub fn claim(&self) -> Result<Option<SingletonLock>, String> {
        let file = self.lock_file()?;
        match file.try_lock() {
            Ok(()) => Ok(Some(SingletonLock { _file: file })),
            Err(fs::TryLockError::WouldBlock) => Ok(None),
            Err(fs::TryLockError::Error(error)) => Err(error.to_string()),
        }
    }

    /// Whether a live server holds the claim right now.
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

    /// The one way a server process starts: detached, in its own process
    /// group, belonging to no pane or workspace.
    pub fn spawn(&self) -> Result<(), String> {
        use std::os::unix::process::CommandExt;
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(self.dir.join("server.log"))
            .map_err(|e| e.to_string())?;
        Command::new(crate::runtime_executable()?)
            .arg("--plugin-state")
            .arg(&self.state)
            .arg("dashboard-server")
            .process_group(0)
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_SOCKET_PATH")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log)
            .spawn()
            .map(drop)
            .map_err(|e| format!("could not start the dashboard server: {e}"))
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
