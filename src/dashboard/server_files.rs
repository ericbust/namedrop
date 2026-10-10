//! Everything under `<state>/dashboard/`: the singleton lock, the published
//! `server.json`, the persistent token, workspace labels, and the log.
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

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

/// The bearer secret for every page. Compared in constant time and never logged.
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

    /// `None` while another server holds the claim. Waits up to `wait`, so a
    /// successor started during a handoff outlasts its predecessor's exit.
    pub fn claim(&self, wait: Duration) -> Result<Option<SingletonLock>, String> {
        let file = self.lock_file()?;
        let deadline = Instant::now() + wait;
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Some(SingletonLock { _file: file })),
                Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(50));
                }
                Err(fs::TryLockError::WouldBlock) => return Ok(None),
                Err(fs::TryLockError::Error(error)) => return Err(error.to_string()),
            }
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
        let path = self.dir.join("token");
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
                Err("dashboard token file is malformed; delete it to rotate".to_owned())
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
