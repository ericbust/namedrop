#[cfg(test)]
use super::claude_allow_rules;
use super::{ReplacementError, conflict, replace_if_changed, replace_with_status};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::ErrorKind,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
mod object;
use object::OrderedObject;

const RECORD_FILE: &str = "claude-permissions.json";
pub(super) const REPAIR_GUIDANCE: &str = "Claude agents will be prompted until settings are fixed and Namedrop is reactivated. Fix the reported conflict, then reactivate Namedrop. Loading the skill grants no permissions.";

/// Entries contain only inserted occurrences. Pending changes are durable before
/// settings publication and carry both snapshots to distinguish crash phases.
#[derive(Clone, Default, Serialize, Deserialize)]
struct OwnedPermissions {
    settings_path: PathBuf,
    entries: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pending: Option<PendingUpdate>,
}
#[derive(Clone, Serialize, Deserialize)]
struct PendingUpdate {
    next: Box<OwnedPermissions>,
    updates: Vec<JournalUpdate>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalUpdate {
    path: PathBuf,
    before_hash: Option<String>,
    after_hash: Option<String>,
    added: Vec<String>,
    removed: Vec<String>,
    mode: u32,
}
fn snapshot_hash(text: &Option<String>) -> Option<String> {
    text.as_ref()
        .map(|text| crate::lower_hex(&Sha256::digest(text.as_bytes())))
}
#[derive(Clone)]
struct SettingsUpdate {
    path: PathBuf,
    before: Option<String>,
    after: Option<String>,
    mode: u32,
    added: Vec<String>,
    removed: Vec<String>,
}

pub(super) fn settings_path(directory: &Path) -> Result<PathBuf, String> {
    if !directory.is_absolute() {
        return Err(
            "CLAUDE_CONFIG_DIR must be absolute to install Namedrop permissions".to_owned(),
        );
    }
    Ok(directory.join("settings.json"))
}
fn verify_path(path: &Path) -> Result<(), String> {
    for parent in path.ancestors().take(2) {
        match fs::symlink_metadata(parent) {
            Ok(metadata)
                if metadata.is_symlink()
                    || (parent != path && !metadata.is_dir())
                    || (parent == path && !metadata.is_file()) =>
            {
                return Err(conflict(parent, "is not a regular file or directory"));
            }
            Err(error) if error.kind() != ErrorKind::NotFound => {
                return Err(conflict(parent, &error.to_string()));
            }
            _ => {}
        }
    }
    Ok(())
}
fn read_text(path: &Path) -> Result<Option<String>, String> {
    verify_path(path)?;
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(conflict(path, &error.to_string())),
    }
}
fn read_record(state: &Path) -> Result<OwnedPermissions, String> {
    let path = state.join(RECORD_FILE);
    let Some(text) = read_text(&path)? else {
        return Ok(OwnedPermissions::default());
    };
    let record: OwnedPermissions = serde_json::from_str(&text)
        .map_err(|error| conflict(&path, &format!("has invalid ownership JSON: {error}")))?;
    if (!record.entries.is_empty() && !record.settings_path.is_absolute())
        || record.pending.as_ref().is_some_and(|pending| {
            !pending.next.settings_path.is_absolute()
                || pending.next.pending.is_some()
                || pending
                    .updates
                    .iter()
                    .any(|update| !update.path.is_absolute())
        })
    {
        return Err(conflict(
            &path,
            "ownership settings paths must be absolute and pending updates cannot nest",
        ));
    }
    Ok(record)
}
fn write_record(state: &Path, record: &OwnedPermissions) -> Result<(), String> {
    replace_if_changed(
        &state.join(RECORD_FILE),
        &format!(
            "{}\n",
            serde_json::to_string_pretty(record).map_err(|error| error.to_string())?
        ),
        Some(0o600),
    )
}
struct Settings {
    path: PathBuf,
    original: Option<String>,
    value: OrderedObject,
    permissions: OrderedObject,
    allow: Vec<Box<RawValue>>,
    mode: u32,
    dirty: bool,
    added: Vec<String>,
    removed: Vec<String>,
}
impl Settings {
    fn read(path: &Path) -> Result<Self, String> {
        let original = read_text(path)?;
        let value: OrderedObject = serde_json::from_str(original.as_deref().unwrap_or("{}"))
            .map_err(|error| conflict(path, &format!("is not valid JSON object: {error}")))?;
        let permissions = match value.get("permissions") {
            Some(raw) => serde_json::from_str(raw.get()).map_err(|error| {
                conflict(
                    path,
                    &format!("permissions must be an object without duplicate keys: {error}"),
                )
            })?,
            None => OrderedObject::default(),
        };
        let allow = match permissions.get("allow") {
            Some(raw) => serde_json::from_str(raw.get())
                .map_err(|_| conflict(path, "permissions.allow must be an array"))?,
            None => Vec::new(),
        };
        let mode = fs::metadata(path)
            .map(|metadata| metadata.permissions().mode() & 0o7777)
            .unwrap_or(0o600);
        Ok(Self {
            path: path.to_owned(),
            original,
            value,
            permissions,
            allow,
            mode,
            dirty: false,
            added: Vec::new(),
            removed: Vec::new(),
        })
    }
    fn contains(&self, entry: &str) -> bool {
        self.allow
            .iter()
            .any(|raw| serde_json::from_str::<String>(raw.get()).is_ok_and(|value| value == entry))
    }
    fn add(&mut self, entry: &str) -> Result<(), String> {
        self.allow
            .push(serde_json::value::to_raw_value(entry).map_err(|error| error.to_string())?);
        self.dirty = true;
        self.added.push(entry.to_owned());
        Ok(())
    }
    fn remove(&mut self, entry: &str) -> bool {
        if let Some(index) = self.allow.iter().position(|raw| {
            serde_json::from_str::<String>(raw.get()).is_ok_and(|value| value == entry)
        }) {
            self.allow.remove(index);
            self.dirty = true;
            self.removed.push(entry.to_owned());
            true
        } else {
            false
        }
    }
    fn update(&self) -> Result<SettingsUpdate, String> {
        let after = if self.dirty {
            let mut value = self.value.clone();
            let mut permissions = self.permissions.clone();
            permissions.set("allow", &self.allow)?;
            value.set("permissions", &permissions)?;
            Some(format!(
                "{}\n",
                serde_json::to_string_pretty(&value).map_err(|error| error.to_string())?
            ))
        } else {
            self.original.clone()
        };
        Ok(SettingsUpdate {
            path: self.path.clone(),
            before: self.original.clone(),
            after,
            mode: self.mode,
            added: self.added.clone(),
            removed: self.removed.clone(),
        })
    }
    #[cfg(test)]
    fn publish(&self) -> Result<(), String> {
        self.update()?
            .publish()
            .map(|_| ())
            .map_err(|error| error.message)
    }
}
impl SettingsUpdate {
    fn journal(&self) -> JournalUpdate {
        JournalUpdate {
            path: self.path.clone(),
            before_hash: snapshot_hash(&self.before),
            after_hash: snapshot_hash(&self.after),
            added: self.added.clone(),
            removed: self.removed.clone(),
            mode: self.mode,
        }
    }
    fn publish(&self) -> Result<bool, ReplacementError> {
        let actual = read_text(&self.path)?;
        if actual != self.before {
            return Err(
                conflict(&self.path, "changed during the pending permissions update").into(),
            );
        }
        if self.before == self.after {
            return Ok(false);
        }
        self.write(&self.after)?;
        Ok(true)
    }
    fn write(&self, text: &Option<String>) -> Result<(), ReplacementError> {
        match text {
            Some(text) => {
                fs::create_dir_all(self.path.parent().expect("settings parent"))
                    .map_err(|error| conflict(&self.path, &error.to_string()))?;
                replace_with_status(&self.path, text, Some(self.mode))
            }
            None => match fs::remove_file(&self.path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                Err(error) => Err(conflict(&self.path, &error.to_string()).into()),
            },
        }
    }
    fn rollback(&self) -> Result<(), String> {
        let actual = read_text(&self.path)?;
        if actual == self.before {
            return Ok(());
        }
        if actual != self.after {
            return Err(conflict(
                &self.path,
                "changed before rollback; the pending record was retained",
            ));
        }
        self.write(&self.before).map_err(|error| error.message)
    }
}
fn lock(state: &Path) -> Result<fs::File, String> {
    let path = state.join("claude-permissions.lock");
    verify_path(&path)?;
    fs::create_dir_all(state).map_err(|error| conflict(state, &error.to_string()))?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&path)
        .map_err(|error| conflict(&path, &error.to_string()))?;
    file.lock()
        .map_err(|error| conflict(&path, &error.to_string()))?;
    Ok(file)
}
fn recover(state: &Path) -> Result<OwnedPermissions, String> {
    let record = read_record(state)?;
    let Some(pending) = &record.pending else {
        return Ok(record);
    };
    let mut changed = false;
    for update in &pending.updates {
        let hash = snapshot_hash(&read_text(&update.path)?);
        changed |= hash != update.before_hash && hash != update.after_hash;
    }
    if changed {
        return discard_pending(state, record);
    }
    for update in &pending.updates {
        if snapshot_hash(&read_text(&update.path)?) == update.after_hash {
            continue;
        }
        let mut settings = Settings::read(&update.path)?;
        for entry in &update.removed {
            settings.remove(entry);
        }
        for entry in &update.added {
            settings.add(entry)?;
        }
        let mut replacement = settings.update()?;
        replacement.mode = update.mode;
        if snapshot_hash(&replacement.after) != update.after_hash {
            return discard_pending(state, record);
        }
        replacement.publish().map_err(|error| error.message)?;
    }
    write_record(state, &pending.next)?;
    Ok(*pending.next.clone())
}
fn discard_pending(state: &Path, mut record: OwnedPermissions) -> Result<OwnedPermissions, String> {
    record.pending = None;
    if !record.entries.is_empty() {
        let settings = Settings::read(&record.settings_path)?;
        record.entries.retain(|entry| settings.contains(entry));
    }
    write_record(state, &record)?;
    eprintln!(
        "Namedrop discarded a pending Claude permissions update that cannot be replayed; leftover rules may need review."
    );
    Ok(record)
}

fn publish(
    state: &Path,
    next: OwnedPermissions,
    updates: Vec<SettingsUpdate>,
) -> Result<(), String> {
    let old = read_record(state)?;
    let changed = updates.iter().any(|update| update.before != update.after);
    if !changed
        && old.settings_path == next.settings_path
        && old.entries == next.entries
        && old.pending.is_none()
    {
        return Ok(());
    }
    let mut pending = old.clone();
    pending.pending = Some(PendingUpdate {
        next: Box::new(next.clone()),
        updates: updates.iter().map(SettingsUpdate::journal).collect(),
    });
    write_record(state, &pending)?;
    let mut applied: Vec<&SettingsUpdate> = Vec::new();
    for update in &updates {
        match update.publish() {
            Ok(true) => applied.push(update),
            Ok(false) => {}
            Err(error) => {
                if error.applied {
                    return Err(error.message);
                }
                for previous in applied.into_iter().rev() {
                    if let Err(rollback) = previous.rollback() {
                        return Err(format!("{error}; rollback also failed: {rollback}"));
                    }
                }
                write_record(state, &old)?;
                return Err(error.message);
            }
        }
    }
    // If this rename fails or the process stops, the durable pending record
    // still owns the intended inserted occurrences and refresh can finish it.
    write_record(state, &next)
}
#[cfg(test)]
pub(super) fn refresh(directory: &Path, executable: &Path, state: &Path) -> Result<(), String> {
    refresh_with_rules(directory, state, claude_allow_rules(executable, state)?)
}
pub(super) fn refresh_with_rules(
    directory: &Path,
    state: &Path,
    desired: Vec<String>,
) -> Result<(), String> {
    let path = settings_path(directory)?;
    let _guard = lock(state)?;
    let owned = recover(state)?;
    let mut settings = Settings::read(&path)?;
    let mut updates = Vec::new();
    let mut entries = Vec::new();
    if !owned.entries.is_empty() && owned.settings_path != path {
        let mut previous = Settings::read(&owned.settings_path)?;
        for entry in &owned.entries {
            previous.remove(entry);
        }
        updates.push(previous.update()?);
    } else {
        for entry in &owned.entries {
            if desired.contains(entry) && settings.contains(entry) {
                entries.push(entry.clone());
            } else {
                settings.remove(entry);
            }
        }
    }
    for entry in desired {
        if !settings.contains(&entry) {
            settings.add(&entry)?;
            entries.push(entry);
        }
    }
    updates.push(settings.update()?);
    publish(
        state,
        OwnedPermissions {
            settings_path: path,
            entries,
            pending: None,
        },
        updates,
    )
}
pub(crate) fn remove_claude_permissions(state: &Path) -> Result<usize, String> {
    let (_package_guard, current) = crate::installed_runtime::package_guard()?;
    if !current {
        return Ok(0);
    }
    let _guard = lock(state)?;
    if crate::StateStore::open(state)
        .map_err(|error| error.to_string())?
        .transaction(|tx| tx.any_activated_worktrees())
        .map_err(|error| error.to_string())?
    {
        return Ok(0);
    }
    let mut owned = recover(state)?;
    if owned.entries.is_empty() {
        return Ok(0);
    }
    let mut settings = Settings::read(&owned.settings_path)?;
    let mut removed = 0;
    for entry in &owned.entries {
        removed += usize::from(settings.remove(entry));
    }
    owned.entries.clear();
    publish(state, owned, vec![settings.update()?])?;
    Ok(removed)
}
pub(super) fn inspect_with_rules(
    directory: &Path,
    state: &Path,
    desired: Vec<String>,
) -> Result<(), String> {
    let path = settings_path(directory)?;
    let owned = read_record(state)?;
    if owned.pending.is_some() {
        return Err(conflict(
            &path,
            "has a pending Claude permissions update; activate again to reconcile it",
        ));
    }
    let settings = Settings::read(&path)?;
    if (!owned.entries.is_empty() && owned.settings_path != path)
        || owned.entries.iter().any(|entry| !desired.contains(entry))
    {
        return Err(conflict(
            &path,
            "has stale managed Claude collaboration permissions; activate again to refresh them",
        ));
    }
    if desired.iter().any(|entry| !settings.contains(entry)) {
        return Err(conflict(
            &path,
            "is missing Claude collaboration permissions; activate again to repair them",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_environment::CommandHome;
    #[test]
    fn pending_journal_omits_settings_secrets() {
        let root = std::env::temp_dir().join(format!("nd21-journal-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let path = root.join("settings.json");
        fs::write(
            &path,
            r#"{"env":{"TOKEN":"PRIVATE_TOKEN"},"apiKeyHelper":"PRIVATE_HELPER"}"#,
        )
        .unwrap();
        let mut settings = Settings::read(&path).unwrap();
        settings.add("Read").unwrap();
        let next = OwnedPermissions {
            settings_path: path.clone(),
            entries: vec!["Read".into()],
            pending: None,
        };
        let record = OwnedPermissions {
            pending: Some(PendingUpdate {
                next: Box::new(next),
                updates: vec![settings.update().unwrap().journal()],
            }),
            ..OwnedPermissions::default()
        };
        write_record(&root, &record).unwrap();
        let text = fs::read_to_string(root.join(RECORD_FILE)).unwrap();
        fs::remove_dir_all(&root).unwrap();
        for secret in ["PRIVATE_TOKEN", "PRIVATE_HELPER", "apiKeyHelper"] {
            assert!(!text.contains(secret), "{text}");
        }
    }
    #[test]
    fn post_rename_sync_failure_retains_a_private_recoverable_journal() {
        let root = std::env::temp_dir().join(format!("nd21-sync-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let directory = root.join("claude");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("settings.json");
        fs::write(
            &path,
            r#"{"env":{"TOKEN":"PRIVATE_TOKEN"},"apiKeyHelper":"PRIVATE_HELPER"}"#,
        )
        .unwrap();
        super::super::FAIL_DIRECTORY_SYNC.with(|fault| *fault.borrow_mut() = Some(path.clone()));
        let result = refresh(&directory, Path::new("/opt/namedrop"), &root);
        super::super::FAIL_DIRECTORY_SYNC.with(|fault| *fault.borrow_mut() = None);
        assert!(result.is_err());
        assert!(fs::read_to_string(&path).unwrap().contains("Bash("));
        let record = fs::read_to_string(root.join(RECORD_FILE)).unwrap();
        assert!(
            serde_json::from_str::<serde_json::Value>(&record)
                .unwrap()
                .get("pending")
                .is_some(),
            "{record}"
        );
        for secret in ["PRIVATE_TOKEN", "PRIVATE_HELPER", "apiKeyHelper"] {
            assert!(!record.contains(secret), "{record}");
        }
        let recovered = recover(&root).unwrap();
        assert!(!recovered.entries.is_empty());
        assert!(recovered.pending.is_none());
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn rollback_of_an_absent_unchanged_file_succeeds() {
        let path =
            std::env::temp_dir().join(format!("nd21-never-created-{}.json", std::process::id()));
        let settings = Settings::read(&path).unwrap();
        settings.update().unwrap().rollback().unwrap();
    }
    #[test]
    fn atomic_settings_replacement_preserves_mode_under_a_restrictive_umask() {
        const CHILD: &str = "NAMEDROP_MODE_TEST_CHILD";
        let root = std::env::temp_dir().join(format!("nd21-mode-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        if std::env::var_os(CHILD).is_some() {
            let path = root.join("settings.json");
            fs::write(&path, "{}\n").unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
            let mut settings = Settings::read(&path).unwrap();
            settings.add("Read").unwrap();
            settings.publish().unwrap();
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o7777,
                0o664
            );
            fs::remove_dir_all(root).unwrap();
            return;
        }
        let result = std::process::Command::new("sh")
            .args(["-c", "umask 077; exec \"$@\"", "mode-proof"])
            .arg(std::env::current_exe().unwrap())
            .args(["--exact", "install::claude_permissions::tests::atomic_settings_replacement_preserves_mode_under_a_restrictive_umask", "--nocapture"])
            .env(CHILD,"1").isolated_home(&root).env("CLAUDE_CONFIG_DIR",root.join(".claude")).env("CODEX_HOME",root.join(".codex"))
            .output().unwrap();
        fs::remove_dir_all(root).unwrap();
        assert!(result.status.success(), "{result:?}");
    }
}
