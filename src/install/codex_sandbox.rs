use super::{conflict, replace_if_changed};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::ErrorKind,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};
use toml_edit::{DocumentMut, Item};

const RECORD: &str = "codex-sandbox.json";

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    config: PathBuf,
    root: String,
    pending: Option<Pending>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pending {
    before: Option<String>,
    after: String,
}

fn read(path: &Path) -> Result<Option<String>, String> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_symlink() => {
            return Err(conflict(path, "is a symbolic link"));
        }
        Err(error) if error.kind() != ErrorKind::NotFound => return Err(error.to_string()),
        _ => {}
    }
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(conflict(path, &error.to_string())),
    }
}

fn lock(path: &Path) -> Result<fs::File, String> {
    read(path)?;
    fs::create_dir_all(path.parent().expect("lock parent")).map_err(|e| e.to_string())?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
        .map_err(|e| conflict(path, &e.to_string()))?;
    file.lock().map_err(|e| e.to_string())?;
    Ok(file)
}

fn hash(text: &str) -> String {
    crate::lower_hex(&Sha256::digest(text.as_bytes()))
}

fn record(state: &Path) -> Result<Option<Grant>, String> {
    let Some(text) = read(&state.join(RECORD))? else {
        return Ok(None);
    };
    let grant: Option<Grant> = serde_json::from_str(&text).map_err(|e| e.to_string())?;
    if let Some(grant) = &grant
        && (!grant.config.is_absolute()
            || grant.config.file_name().is_none_or(|n| n != "config.toml")
            || (grant.root != root(state)? && grant.root != physical_state(state)?))
    {
        return Err("invalid Codex sandbox ownership record".into());
    }
    Ok(grant)
}

fn save(state: &Path, grant: Option<&Grant>) -> Result<(), String> {
    replace_if_changed(
        &state.join(RECORD),
        &serde_json::to_string_pretty(&grant).map_err(|e| e.to_string())?,
        Some(0o600),
    )
}

fn resolve_directory(path: &Path) -> Result<PathBuf, String> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(_) => {
                return fs::canonicalize(ancestor)
                    .map(|mut physical| {
                        let suffix = path.strip_prefix(ancestor).expect("ancestor");
                        if !suffix.as_os_str().is_empty() {
                            physical.push(suffix);
                        }
                        physical
                    })
                    .map_err(|error| conflict(ancestor, &error.to_string()));
            }
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => return Err(conflict(ancestor, &error.to_string())),
        }
    }
    Err(conflict(path, "has no existing directory ancestor"))
}

fn physical_state(state: &Path) -> Result<String, String> {
    if !state.is_absolute()
        || state
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err("Namedrop state directory must be absolute without parent components".into());
    }
    let state: PathBuf = state.components().collect();
    match fs::symlink_metadata(&state) {
        Ok(metadata) if metadata.is_symlink() => {
            return Err(conflict(&state, "is a symbolic link"));
        }
        Err(error) if error.kind() != ErrorKind::NotFound => {
            return Err(conflict(&state, &error.to_string()));
        }
        _ => {}
    }
    resolve_directory(&state)?
        .to_str()
        .map(str::to_owned)
        .ok_or_else(|| "Namedrop state directory must be UTF-8".into())
}

pub(crate) fn root(state: &Path) -> Result<String, String> {
    let spool = PathBuf::from(physical_state(state)?).join("deferred-commands");
    match fs::symlink_metadata(&spool) {
        Ok(metadata) if metadata.is_symlink() => {
            return Err(conflict(&spool, "is a symbolic link"));
        }
        Ok(metadata) if !metadata.is_dir() => {
            return Err(conflict(&spool, "is not a directory"));
        }
        Err(error) if error.kind() != ErrorKind::NotFound => {
            return Err(conflict(&spool, &error.to_string()));
        }
        _ => {}
    }
    Ok(spool.to_str().expect("UTF-8 state path").to_owned())
}

fn exposes_trusted_state(state: &Path, writable: &Path) -> bool {
    state.starts_with(writable)
        || (writable.starts_with(state) && !writable.starts_with(state.join("deferred-commands")))
}

pub(crate) fn check_worktree(state: &Path, worktree: &Path) -> Result<(), String> {
    let state = physical_state(state)?;
    let physical_worktree = resolve_directory(worktree)?;
    if exposes_trusted_state(Path::new(&state), &physical_worktree) {
        return Err(format!(
            "unsafe Space worktree {} exposes trusted Namedrop state through Codex's implicit workspace write access; move the Space worktree outside the Namedrop state directory and its ancestors, or relocate Namedrop state outside the worktree, then activate Namedrop again and start a new Codex session",
            worktree.display()
        ));
    }
    Ok(())
}

struct Config {
    path: PathBuf,
    before: Option<String>,
    document: DocumentMut,
    mode: u32,
}
impl Config {
    fn read(path: &Path) -> Result<Self, String> {
        let before = read(path)?;
        let document = before
            .as_deref()
            .unwrap_or("")
            .parse::<DocumentMut>()
            .map_err(|e| conflict(path, &format!("is invalid TOML: {e}")))?;
        if let Some(table) = document.get("sandbox_workspace_write") {
            let table = table
                .as_table_like()
                .ok_or_else(|| conflict(path, "sandbox_workspace_write must be a table"))?;
            if let Some(roots) = table.get("writable_roots") {
                let roots = roots
                    .as_array()
                    .ok_or_else(|| conflict(path, "writable_roots must be an array of strings"))?;
                if roots.iter().any(|v| v.as_str().is_none()) {
                    return Err(conflict(path, "writable_roots must be an array of strings"));
                }
            }
        }
        let mode = fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o7777)
            .unwrap_or(0o600);
        Ok(Self {
            path: path.to_owned(),
            before,
            document,
            mode,
        })
    }
    fn contains(&self, root: &str) -> bool {
        self.document
            .get("sandbox_workspace_write")
            .and_then(Item::as_table_like)
            .and_then(|t| t.get("writable_roots"))
            .and_then(Item::as_array)
            .is_some_and(|a| a.iter().any(|v| v.as_str() == Some(root)))
    }
    fn reject_broad_roots(&self, state: &Path) -> Result<(), String> {
        if let Some(roots) = self
            .document
            .get("sandbox_workspace_write")
            .and_then(Item::as_table_like)
            .and_then(|table| table.get("writable_roots"))
            .and_then(Item::as_array)
        {
            for root in roots.iter().filter_map(|value| value.as_str()) {
                let path = Path::new(root);
                if path.is_absolute() && exposes_trusted_state(state, &resolve_directory(path)?) {
                    return Err(conflict(
                        &self.path,
                        &format!(
                            "unsafe user-owned writable root {root:?} covers trusted Namedrop state; remove that broad root manually, then activate Namedrop again and start a new Codex session"
                        ),
                    ));
                }
            }
        }
        Ok(())
    }
    fn add(&mut self, root: &str) {
        if self.document.get("sandbox_workspace_write").is_none() {
            self.document["sandbox_workspace_write"] = Item::Table(toml_edit::Table::new());
        }
        let table = self.document["sandbox_workspace_write"]
            .as_table_like_mut()
            .expect("validated table");
        if table.get("writable_roots").is_none() {
            table.insert("writable_roots", toml_edit::value(toml_edit::Array::new()));
        }
        table
            .get_mut("writable_roots")
            .and_then(Item::as_array_mut)
            .expect("validated roots")
            .push(root);
    }
    fn remove(&mut self, root: &str) -> bool {
        let Some(array) = self
            .document
            .get_mut("sandbox_workspace_write")
            .and_then(Item::as_table_like_mut)
            .and_then(|t| t.get_mut("writable_roots"))
            .and_then(Item::as_array_mut)
        else {
            return false;
        };
        let Some(index) = array.iter().position(|v| v.as_str() == Some(root)) else {
            return false;
        };
        array.remove(index);
        true
    }
    fn publish(&self) -> Result<(), String> {
        if read(&self.path)? != self.before {
            return Err(conflict(
                &self.path,
                "changed during the sandbox grant update",
            ));
        }
        replace_if_changed(&self.path, &self.document.to_string(), Some(self.mode))
    }
}

fn recover(state: &Path, mut grant: Grant, config: &Config) -> Result<Option<Grant>, String> {
    if let Some(pending) = &grant.pending {
        let actual = config.before.as_deref().map(hash);
        if actual.as_ref() == Some(&pending.after) {
            grant.pending = None;
            save(state, Some(&grant))?;
        } else if actual == pending.before {
            save(state, None)?;
            return Ok(None);
        } else {
            return Err(conflict(
                &grant.config,
                "changed after an interrupted Codex sandbox grant; inspect the ownership record before repair",
            ));
        }
    }
    Ok(Some(grant))
}

fn remove_owned(state: &Path) -> Result<usize, String> {
    let Some(grant) = record(state)? else {
        return Ok(0);
    };
    let _config_guard = lock(&grant.config.with_file_name(".namedrop-sandbox.lock"))?;
    let mut config = Config::read(&grant.config)?;
    let Some(grant) = recover(state, grant, &config)? else {
        return Ok(0);
    };
    let removed = config.remove(&grant.root);
    if removed {
        config.publish()?;
    }
    save(state, None)?;
    Ok(usize::from(removed))
}

// Lifecycle callers hold the state transaction through activation and this write.
pub(crate) fn activate(directory: &Path, state: &Path) -> Result<(), String> {
    if !directory.is_absolute() {
        return Err("CODEX_HOME must be absolute".into());
    }
    let root = root(state)?;
    let directory = resolve_directory(directory)?;
    let path = directory.join("config.toml");
    let _state_guard = lock(&state.join("codex-sandbox.lock"))?;
    Config::read(&path)?;
    if record(state)?.is_some_and(|g| g.config != path || g.root != root) {
        remove_owned(state)?;
    }
    let _config_guard = lock(&directory.join(".namedrop-sandbox.lock"))?;
    let mut config = Config::read(&path)?;
    if let Some(grant) = record(state)? {
        recover(state, grant, &config)?;
    }
    config.reject_broad_roots(Path::new(&physical_state(state)?))?;
    fs::DirBuilder::new()
        .mode(0o700)
        .recursive(true)
        .create(&root)
        .map_err(|error| conflict(Path::new(&root), &error.to_string()))?;
    if config.contains(&root) {
        return Ok(());
    }
    config.add(&root);
    let mut grant = Grant {
        config: path,
        root,
        pending: Some(Pending {
            before: config.before.as_deref().map(hash),
            after: hash(&config.document.to_string()),
        }),
    };
    save(state, Some(&grant))?;
    config.publish()?;
    grant.pending = None;
    save(state, Some(&grant))
}

pub(crate) fn remove(state: &Path) -> Result<usize, String> {
    root(state)?;
    let (_package_guard, current) = crate::installed_runtime::package_guard()?;
    if !current {
        return Ok(0);
    }
    crate::StateStore::open(state)
        .map_err(|e| e.to_string())?
        .transaction(|tx| {
            if tx.any_activated_worktrees()? {
                return Ok(0);
            }
            let _guard = lock(&state.join("codex-sandbox.lock"))
                .map_err(crate::StoreError::invalid_input)?;
            remove_owned(state).map_err(crate::StoreError::invalid_input)
        })
        .map_err(|e| e.to_string())
}

pub(super) fn inspect(directory: &Path, state: &Path) -> Result<(), String> {
    let root = root(state)?;
    let directory = resolve_directory(directory)?;
    let path = directory.join("config.toml");
    let grant = record(state)?;
    if grant.is_some_and(|g| g.config != path || g.root != root || g.pending.is_some()) {
        return Err(conflict(
            &path,
            "has a stale or pending Codex sandbox grant; activate Namedrop again to repair it",
        ));
    }
    let config = Config::read(&path)?;
    config.reject_broad_roots(Path::new(&physical_state(state)?))?;
    if !config.contains(&root) {
        return Err(conflict(
            &path,
            "is missing the Namedrop deferred-commands writable root; activate Namedrop again, then start a new Codex session. Explicit Codex permission profiles require their own deferred-commands-directory grant",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "namedrop-codex-grant-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path.canonicalize().unwrap())
        }
        fn home(&self) -> PathBuf {
            self.0.join("custom-codex")
        }
        fn state(&self) -> PathBuf {
            self.0.join("state")
        }
        fn write(&self, text: &str) {
            fs::create_dir_all(self.home()).unwrap();
            fs::write(self.home().join("config.toml"), text).unwrap();
        }
        fn text(&self) -> String {
            fs::read_to_string(self.home().join("config.toml")).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn narrow_grant_excludes_trusted_state_and_creates_spool() {
        let f = Fixture::new();
        activate(&f.home(), &f.state()).unwrap();
        let config = Config::read(&f.home().join("config.toml")).unwrap();
        let spool = f.state().join("deferred-commands");
        assert!(spool.is_dir());
        assert!(config.contains(spool.to_str().unwrap()));
        assert!(!config.contains(f.state().to_str().unwrap()));
        let roots = config.document["sandbox_workspace_write"]["writable_roots"]
            .as_array()
            .unwrap();
        for protected in [
            f.state().join("runtime/namedrop"),
            f.state().join("namedrop.sqlite3"),
        ] {
            assert!(
                !roots
                    .iter()
                    .any(|root| protected.starts_with(root.as_str().unwrap()))
            );
        }
    }

    fn legacy_grant(f: &Fixture) {
        f.write(&format!(
            "model = 'keep'\n[sandbox_workspace_write]\nwritable_roots = ['/user/root', {:?}]\n",
            f.state().to_str().unwrap()
        ));
        fs::create_dir_all(f.state()).unwrap();
        save(
            &f.state(),
            Some(&Grant {
                config: f.home().join("config.toml"),
                root: f.state().to_str().unwrap().into(),
                pending: None,
            }),
        )
        .unwrap();
    }

    #[test]
    fn owned_legacy_grant_is_stale_then_migrated_and_cleaned_up() {
        let f = Fixture::new();
        legacy_grant(&f);
        let before = f.text();
        assert!(
            inspect(&f.home(), &f.state())
                .unwrap_err()
                .contains("stale")
        );
        assert_eq!(f.text(), before);
        activate(&f.home(), &f.state()).unwrap();
        let config = Config::read(&f.home().join("config.toml")).unwrap();
        assert!(!config.contains(f.state().to_str().unwrap()));
        assert!(config.contains(f.state().join("deferred-commands").to_str().unwrap()));
        inspect(&f.home(), &f.state()).unwrap();
        let migrated = f.text();
        activate(&f.home(), &f.state()).unwrap();
        assert_eq!(f.text(), migrated);
        assert_eq!(remove(&f.state()).unwrap(), 1);
        assert!(f.text().contains("/user/root"));
        assert!(f.text().contains("model = 'keep'"));
    }

    #[test]
    fn interrupted_legacy_removal_recovers_without_regranting_state() {
        let f = Fixture::new();
        legacy_grant(&f);
        super::super::FAIL_DIRECTORY_SYNC
            .with(|fault| *fault.borrow_mut() = Some(f.home().join("config.toml")));
        assert!(activate(&f.home(), &f.state()).is_err());
        super::super::FAIL_DIRECTORY_SYNC.with(|fault| *fault.borrow_mut() = None);
        assert!(
            !Config::read(&f.home().join("config.toml"))
                .unwrap()
                .contains(f.state().to_str().unwrap())
        );
        activate(&f.home(), &f.state()).unwrap();
        inspect(&f.home(), &f.state()).unwrap();
        assert_eq!(remove(&f.state()).unwrap(), 1);
    }

    #[test]
    fn pending_legacy_publication_recovers_before_migration() {
        for published in [false, true] {
            let f = Fixture::new();
            legacy_grant(&f);
            let after = f.text();
            let before = "model = 'keep'\n";
            save(
                &f.state(),
                Some(&Grant {
                    config: f.home().join("config.toml"),
                    root: f.state().to_str().unwrap().into(),
                    pending: Some(Pending {
                        before: Some(hash(before)),
                        after: hash(&after),
                    }),
                }),
            )
            .unwrap();
            if !published {
                f.write(before);
            }
            activate(&f.home(), &f.state()).unwrap();
            inspect(&f.home(), &f.state()).unwrap();
            assert!(
                !Config::read(&f.home().join("config.toml"))
                    .unwrap()
                    .contains(f.state().to_str().unwrap())
            );
            assert_eq!(remove(&f.state()).unwrap(), 1);
        }
    }

    #[test]
    fn legacy_migration_preserves_existing_user_spool_grant() {
        let f = Fixture::new();
        legacy_grant(&f);
        let mut config = Config::read(&f.home().join("config.toml")).unwrap();
        config.add(f.state().join("deferred-commands").to_str().unwrap());
        config.publish().unwrap();
        activate(&f.home(), &f.state()).unwrap();
        inspect(&f.home(), &f.state()).unwrap();
        assert!(record(&f.state()).unwrap().is_none());
        assert_eq!(remove(&f.state()).unwrap(), 0);
    }

    #[test]
    fn legacy_migration_removes_owned_state_before_refusing_user_ancestor() {
        let f = Fixture::new();
        legacy_grant(&f);
        let mut config = Config::read(&f.home().join("config.toml")).unwrap();
        config.add(f.0.to_str().unwrap());
        config.publish().unwrap();
        assert!(
            activate(&f.home(), &f.state())
                .unwrap_err()
                .contains("unsafe")
        );
        let config = Config::read(&f.home().join("config.toml")).unwrap();
        assert!(!config.contains(f.state().to_str().unwrap()));
        assert!(config.contains(f.0.to_str().unwrap()));
        assert!(!config.contains(f.state().join("deferred-commands").to_str().unwrap()));
        assert_eq!(remove(&f.state()).unwrap(), 0);
    }

    #[test]
    fn unowned_broad_grant_is_preserved_but_reported_unsafe() {
        for suffix in ["", "..", "runtime", "namedrop.sqlite3", "runtime/new"] {
            let f = Fixture::new();
            let broad = f.state().join(suffix);
            fs::create_dir_all(f.state().join("runtime")).unwrap();
            crate::StateStore::open(f.state()).unwrap();
            f.write(&format!(
                "[sandbox_workspace_write]\nwritable_roots = [{:?}]\n",
                broad.to_str().unwrap()
            ));
            let before = f.text();
            assert!(
                activate(&f.home(), &f.state())
                    .unwrap_err()
                    .contains("unsafe")
            );
            let error = inspect(&f.home(), &f.state()).unwrap_err();
            assert!(error.contains("unsafe"), "{error}");
            assert!(error.contains("remove"), "{error}");
            assert_eq!(remove(&f.state()).unwrap(), 0);
            assert_eq!(f.text(), before);
            assert!(
                Config::read(&f.home().join("config.toml"))
                    .unwrap()
                    .contains(broad.to_str().unwrap())
            );
        }
    }

    #[test]
    fn user_spool_descendants_are_allowed_and_linked_trusted_roots_are_refused() {
        for suffix in ["deferred-commands", "deferred-commands/nested", "runtime"] {
            let f = Fixture::new();
            fs::create_dir_all(f.state()).unwrap();
            let alias = f.0.join("alias");
            std::os::unix::fs::symlink(&f.0, &alias).unwrap();
            let user_root = alias.join("state").join(suffix);
            f.write(&format!(
                "[sandbox_workspace_write]\nwritable_roots = [{:?}]\n",
                user_root.to_str().unwrap()
            ));
            if suffix == "runtime" {
                let before = f.text();
                assert!(
                    activate(&f.home(), &f.state())
                        .unwrap_err()
                        .contains("unsafe")
                );
                assert!(
                    inspect(&f.home(), &f.state())
                        .unwrap_err()
                        .contains("unsafe")
                );
                assert_eq!(before, f.text());
            } else {
                activate(&f.home(), &f.state()).unwrap();
                inspect(&f.home(), &f.state()).unwrap();
                remove(&f.state()).unwrap();
                assert!(
                    Config::read(&f.home().join("config.toml"))
                        .unwrap()
                        .contains(user_root.to_str().unwrap())
                );
            }
        }
    }

    #[test]
    fn linked_spool_leaf_is_refused_by_all_grant_operations() {
        let f = Fixture::new();
        let victim = f.0.join("victim");
        fs::create_dir(&victim).unwrap();
        fs::create_dir(f.state()).unwrap();
        std::os::unix::fs::symlink(&victim, f.state().join("deferred-commands")).unwrap();
        assert!(
            activate(&f.home(), &f.state())
                .unwrap_err()
                .contains("symbolic link")
        );
        assert!(
            inspect(&f.home(), &f.state())
                .unwrap_err()
                .contains("symbolic link")
        );
        assert!(remove(&f.state()).unwrap_err().contains("symbolic link"));
        assert!(!f.home().exists());
        assert_eq!(fs::read_dir(&victim).unwrap().count(), 0);
    }

    #[test]
    fn linked_state_leaf_cannot_grant_victim_directory() {
        let f = Fixture::new();
        let victim = f.0.join("victim");
        fs::create_dir(&victim).unwrap();
        std::os::unix::fs::symlink(&victim, f.state()).unwrap();
        for suffix in ["", "/", "/."] {
            let state = PathBuf::from(format!("{}{suffix}", f.state().display()));
            let result = activate(&f.home(), &state);
            assert!(!f.home().exists(), "activation created a redirected grant");
            assert_eq!(fs::read_dir(&victim).unwrap().count(), 0);
            assert!(result.unwrap_err().contains("symbolic link"));
        }
    }

    #[test]
    fn linked_state_leaf_cannot_validate_victim_grant() {
        let f = Fixture::new();
        let victim = f.0.join("victim");
        fs::create_dir(&victim).unwrap();
        std::os::unix::fs::symlink(&victim, f.state()).unwrap();
        f.write(&format!(
            "[sandbox_workspace_write]\nwritable_roots = [{:?}]\n",
            victim.to_str().unwrap()
        ));
        let before = f.text();
        assert!(
            inspect(&f.home(), &f.state())
                .unwrap_err()
                .contains("symbolic link")
        );
        assert_eq!(f.text(), before);
        assert_eq!(fs::read_dir(&victim).unwrap().count(), 0);
    }

    #[test]
    fn linked_state_leaf_removal_does_not_open_victim_store() {
        let f = Fixture::new();
        let victim = f.0.join("victim");
        fs::create_dir(&victim).unwrap();
        std::os::unix::fs::symlink(&victim, f.state()).unwrap();
        let result = remove(&f.state());
        assert_eq!(fs::read_dir(&victim).unwrap().count(), 0);
        assert!(result.unwrap_err().contains("symbolic link"));
        assert!(!f.home().exists());
    }

    #[test]
    fn linked_parent_grants_physical_root_and_supports_owned_lifecycle() {
        let f = Fixture::new();
        let physical = f.0.canonicalize().unwrap();
        let alias = f.0.join("alias");
        std::os::unix::fs::symlink(&physical, &alias).unwrap();
        let home = alias.join("custom-codex");
        let state = alias.join("missing/state");
        let error = inspect(&home, &state).unwrap_err();
        assert!(error.contains("missing"), "{error}");
        assert!(!home.exists());
        assert!(!state.exists());
        f.write("model = 'keep'\n");
        fs::set_permissions(home.join("config.toml"), fs::Permissions::from_mode(0o640)).unwrap();
        activate(&home, &state).unwrap();
        let expected_state = physical.join("missing/state");
        let expected = expected_state.join("deferred-commands");
        let config = Config::read(&home.join("config.toml")).unwrap();
        let roots = config.document["sandbox_workspace_write"]["writable_roots"]
            .as_array()
            .unwrap();
        assert_eq!(roots.len(), 1);
        assert_eq!(roots.get(0).unwrap().as_str(), expected.to_str());
        let first = f.text();
        inspect(&home, &state).unwrap();
        inspect(&physical.join("custom-codex"), &expected_state).unwrap();
        activate(&physical.join("custom-codex"), &expected_state).unwrap();
        assert_eq!(f.text(), first);
        assert_eq!(
            fs::metadata(home.join("config.toml"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o640
        );
        assert_eq!(remove(&state).unwrap(), 1);
        assert_eq!(remove(&state).unwrap(), 0);
        assert!(f.text().contains("model = 'keep'"));
        assert!(
            !Config::read(&home.join("config.toml"))
                .unwrap()
                .contains(expected.to_str().unwrap())
        );
    }

    #[test]
    fn dangling_directory_links_are_not_treated_as_missing_directories() {
        let f = Fixture::new();
        let alias = f.0.join("dangling");
        let missing = f.0.join("absent");
        std::os::unix::fs::symlink(&missing, &alias).unwrap();
        assert!(activate(&alias, &f.state()).is_err());
        assert!(activate(&f.home(), &alias.join("state")).is_err());
        assert!(inspect(&alias, &f.state()).is_err());
        assert!(inspect(&f.home(), &alias.join("state")).is_err());
        assert!(!missing.exists());
        assert!(!f.home().exists());
        assert!(!f.state().exists());
    }

    #[test]
    fn sandbox_file_symlinks_are_refused_under_linked_parents() {
        for leaf in [
            "config.toml",
            RECORD,
            "codex-sandbox.lock",
            ".namedrop-sandbox.lock",
        ] {
            let f = Fixture::new();
            let alias = f.0.join("alias");
            std::os::unix::fs::symlink(f.0.canonicalize().unwrap(), &alias).unwrap();
            let home = alias.join("custom-codex");
            let state = alias.join("state");
            fs::create_dir_all(&home).unwrap();
            fs::create_dir_all(&state).unwrap();
            let target = f.0.join("untouched");
            fs::write(&target, "").unwrap();
            let path = if leaf == RECORD || leaf == "codex-sandbox.lock" {
                state.join(leaf)
            } else {
                home.join(leaf)
            };
            std::os::unix::fs::symlink(&target, &path).unwrap();
            let error = activate(&home, &state).unwrap_err();
            assert!(
                error.contains(&path.file_name().unwrap().to_string_lossy().to_string()),
                "{error}"
            );
            assert!(error.contains("symbolic link"), "{error}");
            assert!(fs::symlink_metadata(&path).unwrap().is_symlink());
            assert_eq!(fs::read_to_string(&target).unwrap(), "");
            if leaf == RECORD || leaf == "config.toml" {
                assert!(
                    inspect(&home, &state)
                        .unwrap_err()
                        .contains("symbolic link")
                );
            }
        }
    }

    #[test]
    fn activation_grants_exact_spool_preserves_settings_mode_and_is_idempotent() {
        use std::os::unix::fs::MetadataExt;
        let f = Fixture::new();
        f.write("# keep this\nmodel = 'example'\napproval_policy = 'on-request'\n[sandbox_workspace_write]\nwritable_roots = ['/user/root']\nnetwork_access = false\n");
        fs::set_permissions(
            f.home().join("config.toml"),
            fs::Permissions::from_mode(0o640),
        )
        .unwrap();
        activate(&f.home(), &f.state()).unwrap();
        let first = f.text();
        assert!(first.contains("# keep this"));
        assert!(first.contains("model = 'example'"));
        assert!(first.contains("approval_policy = 'on-request'"));
        assert!(first.contains("network_access = false"));
        let config = Config::read(&f.home().join("config.toml")).unwrap();
        assert!(config.contains(f.state().join("deferred-commands").to_str().unwrap()));
        assert!(config.contains("/user/root"));
        assert!(!config.contains(f.0.to_str().unwrap()));
        let metadata = fs::metadata(f.home().join("config.toml")).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o640);
        activate(&f.home(), &f.state()).unwrap();
        assert_eq!(f.text(), first);
        assert_eq!(
            metadata.ino(),
            fs::metadata(f.home().join("config.toml")).unwrap().ino()
        );
        assert_eq!(remove(&f.state()).unwrap(), 1);
        assert!(f.text().contains("/user/root"));
        assert!(
            !Config::read(&f.home().join("config.toml"))
                .unwrap()
                .contains(f.state().join("deferred-commands").to_str().unwrap())
        );
    }

    #[test]
    fn preexisting_root_is_never_owned_or_removed() {
        let f = Fixture::new();
        f.write(&format!(
            "[sandbox_workspace_write]\nwritable_roots = [{:?}]\n",
            f.state().join("deferred-commands").to_str().unwrap()
        ));
        let before = f.text();
        activate(&f.home(), &f.state()).unwrap();
        assert!(record(&f.state()).unwrap().is_none());
        assert_eq!(remove(&f.state()).unwrap(), 0);
        assert_eq!(before, f.text());
    }

    #[test]
    fn inspection_is_read_only_and_detects_missing_and_moved_grants() {
        let f = Fixture::new();
        assert!(
            inspect(&f.home(), &f.state())
                .unwrap_err()
                .contains("missing")
        );
        assert!(!f.home().exists());
        assert!(!f.state().exists());
        activate(&f.home(), &f.state()).unwrap();
        inspect(&f.home(), &f.state()).unwrap();
        let next = f.0.join("other-codex");
        assert!(inspect(&next, &f.state()).unwrap_err().contains("stale"));
        assert!(!next.exists());
        activate(&next, &f.state()).unwrap();
        assert!(
            !Config::read(&f.home().join("config.toml"))
                .unwrap()
                .contains(f.state().join("deferred-commands").to_str().unwrap())
        );
        inspect(&next, &f.state()).unwrap();
        fs::write(next.join("config.toml"), "model = 'user edit'\n").unwrap();
        assert!(inspect(&next, &f.state()).unwrap_err().contains("missing"));
        assert_eq!(
            fs::read_to_string(next.join("config.toml")).unwrap(),
            "model = 'user edit'\n"
        );
    }

    #[test]
    fn malformed_and_symlinked_configs_are_not_modified() {
        let f = Fixture::new();
        for text in [
            "not valid TOML",
            "sandbox_workspace_write = 3",
            "[sandbox_workspace_write]\nwritable_roots = ['/root', 1]",
        ] {
            f.write(text);
            assert!(activate(&f.home(), &f.state()).is_err());
            assert_eq!(f.text(), text);
            assert!(record(&f.state()).unwrap().is_none());
        }
        let config = f.home().join("config.toml");
        let target = f.0.join("user-config");
        fs::rename(&config, &target).unwrap();
        std::os::unix::fs::symlink(&target, &config).unwrap();
        assert!(
            activate(&f.home(), &f.state())
                .unwrap_err()
                .contains("symbolic link")
        );
        assert!(fs::symlink_metadata(config).unwrap().is_symlink());
    }

    #[test]
    fn last_active_space_removes_the_grant() {
        let f = Fixture::new();
        activate(&f.home(), &f.state()).unwrap();
        let mut store = crate::StateStore::open(f.state()).unwrap();
        let first = crate::StateNamespace::for_space("socket", "first");
        let second = crate::StateNamespace::for_space("socket", "second");
        let actor = crate::MutationAttribution::new(crate::ActorKind::Human, "test", 1);
        store
            .transaction(|tx| {
                tx.activate_worktree(&first, &actor)?;
                tx.activate_worktree(&second, &actor)
            })
            .unwrap();
        assert_eq!(remove(&f.state()).unwrap(), 0);
        store
            .transaction(|tx| tx.deactivate_worktree(&first, &actor))
            .unwrap();
        assert_eq!(remove(&f.state()).unwrap(), 0);
        store
            .transaction(|tx| tx.deactivate_worktree(&second, &actor))
            .unwrap();
        assert_eq!(remove(&f.state()).unwrap(), 1);
    }

    #[test]
    fn interrupted_publication_recovers_ownership_without_config_secrets() {
        let f = Fixture::new();
        f.write("api_key = 'PRIVATE'\n");
        super::super::FAIL_DIRECTORY_SYNC
            .with(|fault| *fault.borrow_mut() = Some(f.home().join("config.toml")));
        assert!(activate(&f.home(), &f.state()).is_err());
        super::super::FAIL_DIRECTORY_SYNC.with(|fault| *fault.borrow_mut() = None);
        assert!(
            !fs::read_to_string(f.state().join(RECORD))
                .unwrap()
                .contains("PRIVATE")
        );
        assert!(
            inspect(&f.home(), &f.state())
                .unwrap_err()
                .contains("pending")
        );
        activate(&f.home(), &f.state()).unwrap();
        inspect(&f.home(), &f.state()).unwrap();
        assert_eq!(remove(&f.state()).unwrap(), 1);
    }
}
