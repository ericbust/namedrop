use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(tag = "reason", rename_all = "snake_case")]
pub enum Retirement {
    HerdrHandoff {
        successor: PathBuf,
        at: String,
    },
    Removed {
        successor: Option<PathBuf>,
        at: String,
    },
}

impl Retirement {
    pub fn message(&self) -> String {
        match self {
            Self::HerdrHandoff { successor, .. } => format!(
                "ND-INSTALL-001: Namedrop on Rex moved to Herdr's install in {}. In each Rex session, run Activate Namedrop again and name the panes again. Restart an agent there that shows Hook failed or does not answer a mention.",
                successor.display()
            ),
            Self::Removed { .. } => {
                "Namedrop was removed from Rex. Run sh scripts/install.sh to install it again."
                    .into()
            }
        }
    }
    pub(crate) fn hands_to(&self, state: &Path) -> bool {
        matches!(self, Self::HerdrHandoff { successor, .. } if super::packages::same(successor, state))
    }
}

pub fn retirement(state: &Path) -> Result<Option<Retirement>, String> {
    let path = state.join("runtime/retired.json");
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|e| format!("Could not read {}: {e}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("Could not read {}: {e}", path.display())),
    }
}

pub(crate) fn check(state: &Path, allow_removed: bool) -> Result<(), String> {
    match retirement(state)? {
        Some(Retirement::Removed { .. }) if allow_removed => Ok(()),
        Some(marker) => Err(marker.message()),
        None => Ok(()),
    }
}

pub(crate) fn utc_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = now.as_secs() as libc::time_t;
    let mut parts: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::gmtime_r(&seconds, &mut parts);
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:09}Z",
        parts.tm_year + 1900,
        parts.tm_mon + 1,
        parts.tm_mday,
        parts.tm_hour,
        parts.tm_min,
        parts.tm_sec,
        now.subsec_nanos()
    )
}

pub(crate) fn write(state: &Path, marker: &Retirement) -> Result<(), String> {
    let runtime = state.join("runtime");
    fs::create_dir_all(&runtime).map_err(|e| e.to_string())?;
    atomic_json(
        &runtime.join("retired.json"),
        &serde_json::to_value(marker).map_err(|e| e.to_string())?,
    )
}

pub(crate) fn atomic_json(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    let parent = path.parent().ok_or("JSON file has no directory")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temp = parent.join(format!(
        ".namedrop-{}-{}.tmp",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temp)
            .map_err(|e| e.to_string())?;
        file.write_all(
            &(serde_json::to_string_pretty(value).map_err(|e| e.to_string())? + "\n").into_bytes(),
        )
        .map_err(|e| e.to_string())?;
        file.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&temp, path).map_err(|e| e.to_string())?;
        fs::File::open(parent)
            .and_then(|f| f.sync_all())
            .map_err(|e| e.to_string())
    })();
    let _ = fs::remove_file(&temp);
    result
}

pub(crate) fn exclusive(state: &Path, wait: Duration, operation: &str) -> Result<fs::File, String> {
    let path = state.join("runtime/commands.lock");
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)
        .map_err(|e| e.to_string())?;
    if !file.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("runtime command lock is not a regular file".into());
    }
    let until = Instant::now() + wait;
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(fs::TryLockError::WouldBlock) if Instant::now() < until => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Err(fs::TryLockError::WouldBlock) => {
                return Err(format!(
                    "Namedrop on Rex is busy. Close any Namedrop screen that is open in Rex, such as the tour, then run {operation} again."
                ));
            }
            Err(fs::TryLockError::Error(e)) => return Err(e.to_string()),
        }
    }
}

pub fn standalone(state: &Path) -> Result<bool, String> {
    Ok(super::packages::owner(state)? == Some(super::packages::Owner::Standalone))
}

pub fn begin_removal(state: &Path) -> Result<(), String> {
    let exclusive = exclusive(state, Duration::from_secs(60), "namedrop install --remove")?;
    check(state, true)?;
    if retirement(state)?.is_none() {
        write(
            state,
            &Retirement::Removed {
                successor: None,
                at: utc_now(),
            },
        )?;
    }
    drop(exclusive);
    let gate = super::command_guard_allow_removed(state, None)?;
    if matches!(retirement(state)?, Some(Retirement::Removed { .. })) {
        super::helpers::stop(state)?;
    }
    drop(gate);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_retired_state_creates_no_command_lock_and_removed_has_one_exception() {
        let root = std::env::temp_dir().join(format!("nd-u7-retire-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let marker = Retirement::Removed {
            successor: None,
            at: "2026-11-15T00:00:00Z".into(),
        };
        write(&root, &marker).unwrap();
        assert_eq!(retirement(&root).unwrap(), Some(marker));
        assert!(super::super::command_guard(&root, None).is_err());
        assert!(!root.join("runtime/commands.lock").exists());
        assert!(super::super::command_guard_allow_removed(&root, None).is_ok());
        write(
            &root,
            &Retirement::HerdrHandoff {
                successor: root.join("new"),
                at: "now".into(),
            },
        )
        .unwrap();
        assert!(
            super::super::command_guard_allow_removed(&root, None)
                .unwrap_err()
                .contains("ND-INSTALL-001")
        );
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn an_unfinished_migration_is_refused_in_the_owner_s_words() {
        let root = std::env::temp_dir().join(format!("nd-u8-interrupted-{}", std::process::id()));
        fs::create_dir_all(root.join("runtime")).unwrap();
        fs::write(root.join("runtime/migration.json"), "{}").unwrap();
        for (owner, words) in [
            (
                "standalone\n",
                "Namedrop installation was interrupted. Run sh scripts/install.sh again to finish; this command made no changes.",
            ),
            (
                "herdr\n",
                "Namedrop activation was interrupted. Reopen Update Namedrop to finish; this command made no changes.",
            ),
        ] {
            fs::write(root.join("runtime/owner"), owner).unwrap();
            assert_eq!(super::super::command_guard(&root, None).unwrap_err(), words);
        }
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn an_exclusive_drain_waits_without_mutating_the_state_and_times_out() {
        let root = std::env::temp_dir().join(format!("nd-u7-drain-{}", std::process::id()));
        fs::create_dir_all(root.join("runtime")).unwrap();
        let shared = super::super::command_guard(&root, None).unwrap();
        let error = exclusive(&root, Duration::from_millis(40), "Activate Namedrop").unwrap_err();
        assert_eq!(
            error,
            "Namedrop on Rex is busy. Close any Namedrop screen that is open in Rex, such as the tour, then run Activate Namedrop again."
        );
        assert!(retirement(&root).unwrap().is_none());
        drop(shared);
        assert!(exclusive(&root, Duration::ZERO, "Activate Namedrop").is_ok());
        fs::remove_dir_all(root).unwrap();
    }
}
