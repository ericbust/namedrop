use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Owner {
    Herdr,
    Standalone,
}

/// A state's `runtime/owner`. A published runtime without one is Herdr's,
/// and a state with only a store has no owner.
pub(crate) fn owner(state: &Path) -> Result<Option<Owner>, String> {
    match fs::read_to_string(state.join("runtime/owner")) {
        Ok(text) => match text.trim() {
            "herdr" => Ok(Some(Owner::Herdr)),
            "standalone" => Ok(Some(Owner::Standalone)),
            _ => Err(format!(
                "invalid Namedrop runtime owner in {}",
                state.display()
            )),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(state
            .join("runtime/current")
            .exists()
            .then_some(Owner::Herdr)),
        Err(error) => Err(error.to_string()),
    }
}

/// Who takes a lease: it decides how long the lease waits for the per-user
/// lock, whether it records this state, and what it does under a record it
/// may not take.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Writer {
    /// `activate` and an explicit package install. Waits for the lock,
    /// records this state, and reports an owner it may not displace.
    Activation,
    Handoff,
    /// Another command, such as `name`. Waits and records this state. Under
    /// a standalone record, Herdr leaves the packages to its next activation.
    Command,
    /// `deactivate`'s removals. Waits, and records nothing.
    Removal,
    /// The refresh that lifecycle events, `reconcile`, and the status worker
    /// run. Waits at most [`REFRESH_WAIT`] for the per-user lock, and leaves
    /// the packages to the next activation when it cannot take the lock.
    Refresh,
}

impl Writer {
    pub(crate) fn for_operation(operation: &str) -> Self {
        match operation {
            "activate" | "install" => Self::Activation,
            "deactivate" | "install.remove" => Self::Removal,
            "reconcile" => Self::Refresh,
            op if op.starts_with("event.") || op == "hook.agent-status-worker" => Self::Refresh,
            _ => Self::Command,
        }
    }
}

const REFRESH_WAIT: Duration = Duration::from_secs(1);

enum Verdict {
    Write,
    /// Writes nothing and reports nothing: the runtime was replaced, the
    /// per-user lock is busy, or Herdr meets a standalone record outside its
    /// activation.
    Leave,
    Refuse {
        state: PathBuf,
        owner: Owner,
    },
}

pub(crate) struct PackageLease {
    _user_lock: Option<fs::File>,
    _state_lock: Option<fs::File>,
    verdict: Verdict,
}

impl PackageLease {
    pub(crate) fn writable(&self) -> Result<bool, String> {
        match &self.verdict {
            Verdict::Write => Ok(true),
            Verdict::Leave => Ok(false),
            Verdict::Refuse { state, owner } => Err(refusal(state, *owner)),
        }
    }

    pub(crate) fn require_write(&self) -> Result<(), String> {
        if self.writable()? {
            Ok(())
        } else {
            Err("the running Namedrop runtime is no longer selected".into())
        }
    }

    pub(super) fn development() -> Self {
        Self {
            _user_lock: None,
            _state_lock: None,
            verdict: Verdict::Write,
        }
    }

    fn leave() -> Self {
        Self {
            _user_lock: None,
            _state_lock: None,
            verdict: Verdict::Leave,
        }
    }
}

fn refusal(state: &Path, owner: Owner) -> String {
    match owner {
        Owner::Herdr => format!(
            "ND-INSTALL-002: Namedrop's shared agent packages belong to Herdr's install in {}. Run Activate Namedrop in Herdr to update them.",
            state.display()
        ),
        Owner::Standalone => format!(
            "ND-INSTALL-002: Namedrop's shared agent packages belong to the Rex install in {}. Use that install to update them.",
            state.display()
        ),
    }
}

pub(crate) fn package_guard(
    home: Option<&Path>,
    state: &Path,
    writer: Writer,
) -> Result<PackageLease, String> {
    let Some(runtime) = super::directory()? else {
        return Ok(PackageLease::development());
    };
    locked(home, state, &runtime, writer)
}

/// The refusal a refresh from this runtime would report, read without the
/// locks, so the status hook can say it without waiting or writing a file.
pub(crate) fn refusal_notice(home: Option<&Path>, state: &Path) -> Option<String> {
    let runtime = super::directory().ok()??;
    let mine = owner(runtime.parent()?).ok()?.unwrap_or(Owner::Herdr);
    match decide(mine, state, home?, Writer::Refresh).ok()? {
        Verdict::Refuse { state, owner } => Some(refusal(&state, owner)),
        Verdict::Write | Verdict::Leave => None,
    }
}

fn open_lock(path: &Path, name: &str) -> std::io::Result<fs::File> {
    fs::create_dir_all(
        path.parent()
            .ok_or_else(|| std::io::Error::other("package lock has no directory"))?,
    )?;
    if fs::symlink_metadata(path).is_ok_and(|metadata| !metadata.is_file()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name} is not a regular file"),
        ));
    }
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{name} is not a regular file"),
        ));
    }
    Ok(file)
}

fn lock_file(path: &Path, name: &str) -> Result<fs::File, String> {
    open_lock(path, name).map_err(|e| e.to_string())
}

pub(crate) fn preflight_handoff(home: &Path) -> Result<(), String> {
    let path = crate::locator::package_lock(home);
    open_lock(&path, "package lock")
        .map(|_| ())
        .map_err(|e| user_lock_error(&path, &e))
}

fn user_lock_error(path: &Path, error: &std::io::Error) -> String {
    format!(
        "Namedrop could not open its package lock {}: {error}.",
        path.display()
    )
}

fn lock(path: &Path, name: &str) -> Result<fs::File, String> {
    let file = lock_file(path, name)?;
    file.lock().map_err(|error| error.to_string())?;
    Ok(file)
}

pub(crate) fn same(left: &Path, right: &Path) -> bool {
    left == right
        || fs::canonicalize(left)
            .ok()
            .zip(fs::canonicalize(right).ok())
            .is_some_and(|(left, right)| left == right)
}

fn locked(
    home: Option<&Path>,
    state: &Path,
    runtime: &Path,
    writer: Writer,
) -> Result<PackageLease, String> {
    let mine = owner(
        runtime
            .parent()
            .ok_or("the runtime has no state directory")?,
    )?
    .unwrap_or(Owner::Herdr);
    // Herdr's runtime serves any state directory, as it did before the lease.
    if mine == Owner::Standalone && !same(runtime, &state.join("runtime")) {
        return Err("the installed runtime does not belong to this state directory".into());
    }
    // Without an absolute HOME there is no package slot, so only the
    // per-state lock applies, as before the lease.
    let home = home.filter(|home| home.is_absolute());
    let mut fallback = false;
    let user = match home.map(crate::locator::package_lock) {
        None => None,
        Some(path) => match open_lock(&path, "package lock") {
            Ok(file) => {
                if writer == Writer::Refresh {
                    let deadline = Instant::now() + REFRESH_WAIT;
                    loop {
                        match file.try_lock() {
                            Ok(()) => break,
                            Err(fs::TryLockError::WouldBlock) if Instant::now() < deadline => {
                                std::thread::sleep(Duration::from_millis(20))
                            }
                            Err(_) => return Ok(PackageLease::leave()),
                        }
                    }
                } else {
                    file.lock().map_err(|e| user_lock_error(&path, &e))?;
                }
                Some(file)
            }
            Err(error) => {
                let shared_failure = error.kind() == std::io::ErrorKind::InvalidInput
                    || matches!(
                        error.raw_os_error(),
                        Some(
                            libc::EACCES | libc::EROFS | libc::ENOTDIR | libc::EISDIR | libc::ELOOP
                        )
                    );
                if mine == Owner::Herdr && writer != Writer::Handoff && shared_failure {
                    fallback = true;
                    eprintln!(
                        "{} It wrote under the lock of {} only.",
                        user_lock_error(&path, &error),
                        state.display()
                    );
                    None
                } else if writer == Writer::Refresh {
                    return Ok(PackageLease::leave());
                } else {
                    return Err(user_lock_error(&path, &error));
                }
            }
        },
    };
    let local = lock(&runtime.join("packages.lock"), "runtime package lock")?;
    let selected =
        fs::canonicalize(runtime.join("current/namedrop")).map_err(|error| error.to_string())?;
    let running = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|error| error.to_string())?;
    let verdict = match home {
        _ if selected != running => Verdict::Leave,
        Some(home) => decide(mine, state, home, writer)?,
        None => Verdict::Write,
    };
    let lease = PackageLease {
        _user_lock: user,
        _state_lock: Some(local),
        verdict,
    };
    if let Some(home) = home
        && matches!(lease.verdict, Verdict::Write)
        && !matches!(writer, Writer::Removal | Writer::Handoff)
    {
        let path = crate::locator::install_record(home);
        if let Err(error) = crate::locator::write_install_record(&lease, &path, state) {
            if !fallback {
                return Err(error.to_string());
            }
            eprintln!(
                "Namedrop could not record its state directory in {}: {error}",
                path.display()
            );
        }
    }
    Ok(lease)
}

/// UI.md's owner rules: a record names another state only when that state's
/// launcher still runs, a handoff toward this state admits it, and only a
/// standalone install and a Herdr one exclude each other.
fn decide(mine: Owner, state: &Path, home: &Path, writer: Writer) -> Result<Verdict, String> {
    let Some(record) = crate::locator::read_install_record(&crate::locator::install_record(home))
        .filter(|record| {
            !same(record, state)
                && fs::symlink_metadata(record.join("runtime/namedrop")).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
        })
    else {
        return Ok(Verdict::Write);
    };
    if super::retirement(&record)?.is_some_and(|marker| {
        marker.hands_to(state) || matches!(marker, super::Retirement::Removed { .. })
    }) {
        return Ok(Verdict::Write);
    }
    Ok(match (mine, owner(&record)?) {
        (_, None) | (Owner::Herdr, Some(Owner::Herdr)) => Verdict::Write,
        (Owner::Herdr, Some(Owner::Standalone))
            if !matches!(writer, Writer::Activation | Writer::Handoff) =>
        {
            Verdict::Leave
        }
        (_, Some(owner)) => Verdict::Refuse {
            state: record,
            owner,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "nd-lease-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn state(&self, name: &str) -> PathBuf {
            self.owned(name, None)
        }
        fn owned(&self, name: &str, owner: Option<&str>) -> PathBuf {
            let state = self.0.join(name);
            let runtime = state.join("runtime");
            fs::create_dir_all(runtime.join("current")).unwrap();
            std::os::unix::fs::symlink(
                std::env::current_exe().unwrap(),
                runtime.join("current/namedrop"),
            )
            .unwrap();
            fs::write(runtime.join("namedrop"), "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(runtime.join("namedrop"), fs::Permissions::from_mode(0o755))
                .unwrap();
            if let Some(owner) = owner {
                fs::write(runtime.join("owner"), owner).unwrap();
            }
            state
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn take(home: &Path, state: &Path) -> PackageLease {
        take_as(home, state, Writer::Activation)
    }
    fn take_as(home: &Path, state: &Path, writer: Writer) -> PackageLease {
        locked(Some(home), state, &state.join("runtime"), writer).unwrap()
    }
    fn record(home: &Path) -> Option<PathBuf> {
        crate::locator::read_install_record(&crate::locator::install_record(home))
    }
    #[test]
    fn owners_distinguish_published_legacy_and_unpublished_states() {
        let f = Fixture::new();
        let store = f.0.join("store");
        fs::create_dir_all(&store).unwrap();
        assert_eq!(owner(&store).unwrap(), None);
        let state = f.state("legacy");
        assert_eq!(owner(&state).unwrap(), Some(Owner::Herdr));
        fs::write(state.join("runtime/owner"), "standalone\n").unwrap();
        assert_eq!(owner(&state).unwrap(), Some(Owner::Standalone));
        fs::write(state.join("runtime/owner"), "herdr\n").unwrap();
        assert_eq!(owner(&state).unwrap(), Some(Owner::Herdr));
    }
    #[test]
    fn the_first_lease_claims_the_slot_before_a_second_install_can_publish() {
        let f = Fixture::new();
        let home = f.0.join("home");
        let a = f.owned("a", Some("standalone"));
        let b = f.owned("b", Some("standalone"));
        drop(take(&home, &a));
        assert_eq!(
            crate::locator::read_install_record(&crate::locator::install_record(&home)),
            Some(a.clone())
        );
        let denied = take(&home, &b).writable().unwrap_err();
        assert!(denied.contains("ND-INSTALL-002") && denied.contains(a.to_str().unwrap()));
        assert_eq!(
            crate::locator::read_install_record(&crate::locator::install_record(&home)),
            Some(a)
        );
    }
    #[test]
    fn a_stale_record_and_a_retired_predecessor_admit_the_selected_runtime() {
        let f = Fixture::new();
        let home = f.0.join("home");
        let a = f.owned("a", Some("standalone"));
        let b = f.state("b");
        drop(take(&home, &a));
        assert!(take(&home, &b).writable().is_err());
        fs::write(
            a.join("runtime/retired.json"),
            serde_json::json!({"successor":b,"reason":"herdr_handoff","at":"now"}).to_string(),
        )
        .unwrap();
        assert!(take(&home, &b).writable().unwrap());
        assert_eq!(
            crate::locator::read_install_record(&crate::locator::install_record(&home)),
            Some(b.clone())
        );
        fs::remove_file(b.join("runtime/namedrop")).unwrap();
        assert!(take(&home, &a).writable().unwrap());
    }
    #[test]
    fn only_a_standalone_install_and_a_herdr_install_exclude_each_other() {
        let f = Fixture::new();
        let home = f.0.join("home");
        let herdr = f.state("herdr");
        let second_herdr = f.owned("second-herdr", Some("herdr"));
        let standalone = f.owned("standalone", Some("standalone"));
        drop(take(&home, &herdr));
        // A second Herdr state takes the record and the packages, as before the lease.
        assert!(take(&home, &second_herdr).writable().unwrap());
        assert_eq!(record(&home), Some(second_herdr.clone()));
        let denied = take(&home, &standalone).writable().unwrap_err();
        assert!(denied.contains("ND-INSTALL-002") && denied.contains("Herdr's install"));
        assert!(denied.contains(second_herdr.to_str().unwrap()));
        assert_eq!(record(&home), Some(second_herdr.clone()));

        let home = f.0.join("standalone-home");
        drop(take(&home, &standalone));
        let denied = take(&home, &herdr).writable().unwrap_err();
        assert!(denied.contains("ND-INSTALL-002") && denied.contains("Rex install"));
        assert!(denied.contains(standalone.to_str().unwrap()));
        for writer in [Writer::Command, Writer::Removal, Writer::Refresh] {
            assert!(
                !take_as(&home, &herdr, writer).writable().unwrap(),
                "{writer:?}"
            );
        }
        assert_eq!(record(&home), Some(standalone));
    }
    #[test]
    fn a_removal_records_nothing_and_a_refresh_leaves_a_busy_lock() {
        let f = Fixture::new();
        let home = f.0.join("home");
        let state = f.state("herdr");
        assert!(take_as(&home, &state, Writer::Removal).writable().unwrap());
        assert_eq!(record(&home), None);
        let held = take(&home, &state);
        let started = Instant::now();
        assert!(!take_as(&home, &state, Writer::Refresh).writable().unwrap());
        assert!(started.elapsed() < REFRESH_WAIT * 3);
        drop(held);
        assert!(take_as(&home, &state, Writer::Refresh).writable().unwrap());
    }
    #[test]
    fn a_herdr_runtime_serves_another_state_and_a_standalone_one_does_not() {
        let f = Fixture::new();
        let home = f.0.join("home");
        let herdr = f.state("herdr");
        let standalone = f.owned("standalone", Some("standalone"));
        let other = f.0.join("other");
        fs::create_dir_all(&other).unwrap();
        let lease = locked(
            Some(&home),
            &other,
            &herdr.join("runtime"),
            Writer::Activation,
        );
        assert!(lease.unwrap().writable().unwrap());
        assert_eq!(record(&home), Some(other.clone()));
        let refused = locked(
            None,
            &other,
            &standalone.join("runtime"),
            Writer::Activation,
        );
        assert!(refused.is_err_and(|error| error.contains("does not belong")));
        let lease = locked(None, &herdr, &herdr.join("runtime"), Writer::Activation);
        assert!(lease.unwrap().writable().unwrap());
    }
    #[test]
    fn a_replaced_runtime_reports_no_shell_command_and_writes_none() {
        let f = Fixture::new();
        let home = f.0.join("home");
        let state = f.state("herdr");
        let link = state.join("runtime/current/namedrop");
        fs::remove_file(&link).unwrap();
        std::os::unix::fs::symlink(state.join("runtime/namedrop"), &link).unwrap();
        let lease = take(&home, &state);
        assert!(!lease.writable().unwrap());
        let launcher = state.join("runtime/namedrop");
        let reported = crate::install::install_shell_command(&lease, &home, &launcher, &state);
        // `scripts/setup.sh` reads `next_step` and `path` from any value but null.
        assert_eq!(reported, Ok(serde_json::Value::Null));
        assert!(!home.join(".local").exists());
        assert_eq!(record(&home), None);
    }
    #[test]
    fn removed_records_admit_a_new_owner_and_herdr_falls_back_only_on_shared_lock_failures() {
        let f = Fixture::new();
        let home = f.0.join("home");
        let old = f.owned("old", Some("standalone"));
        let herdr = f.state("herdr");
        drop(take(&home, &old));
        fs::write(
            old.join("runtime/retired.json"),
            serde_json::json!({"reason":"removed","successor":null,"at":"now"}).to_string(),
        )
        .unwrap();
        assert!(take(&home, &herdr).writable().unwrap());
        assert_eq!(record(&home), Some(herdr.clone()));
        let path = crate::locator::package_lock(&home);
        fs::remove_file(&path).unwrap();
        fs::create_dir(&path).unwrap();
        assert!(take(&home, &herdr).writable().unwrap());
        assert!(
            locked(Some(&home), &old, &old.join("runtime"), Writer::Activation)
                .err()
                .unwrap()
                .contains(path.to_str().unwrap())
        );
        assert!(
            preflight_handoff(&home)
                .unwrap_err()
                .contains(path.to_str().unwrap())
        );
        fs::remove_dir_all(home.join(".config")).unwrap();
        fs::write(home.join(".config"), "file").unwrap();
        assert!(take(&home, &herdr).writable().unwrap());
    }

    #[test]
    fn lifecycle_operations_derive_one_lease_kind() {
        for operation in [
            "name",
            "address.create",
            "address.rename",
            "address.release",
        ] {
            assert_eq!(Writer::for_operation(operation), Writer::Command);
        }
        for operation in [
            "event.pane-lifecycle",
            "event.agent-status-changed",
            "reconcile",
            "hook.agent-status-worker",
        ] {
            assert_eq!(Writer::for_operation(operation), Writer::Refresh);
        }
        for operation in ["activate", "install"] {
            assert_eq!(Writer::for_operation(operation), Writer::Activation);
        }
        assert_eq!(Writer::for_operation("deactivate"), Writer::Removal);
    }

    #[test]
    fn lease_process_child() {
        let Some(root) = std::env::var_os("NAMEDROP_TEST_LEASE_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        let name = std::env::var("NAMEDROP_TEST_LEASE_NAME").unwrap();
        let state = root.join(&name);
        let lease = take(&root.join("home"), &state);
        let verdict = lease.writable();
        if name == "first" {
            assert!(verdict.unwrap());
        } else {
            assert!(verdict.unwrap_err().contains("ND-INSTALL-002"));
        }
        fs::write(root.join(format!("{name}-held")), "held").unwrap();
        if name == "first" {
            let until = Instant::now() + Duration::from_secs(10);
            while !root.join("release").exists() {
                assert!(Instant::now() < until);
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }
    #[test]
    fn two_processes_with_different_xdg_roots_serialize_on_the_home_lock() {
        let f = Fixture::new();
        f.owned("first", Some("standalone"));
        f.owned("second", Some("standalone"));
        let child = |name: &str| {
            Command::new(std::env::current_exe().unwrap())
                .env_clear()
                .env("HOME", f.0.join("home"))
                .env("XDG_CONFIG_HOME", f.0.join(name))
                .env("NAMEDROP_TEST_LEASE_ROOT", &f.0)
                .env("NAMEDROP_TEST_LEASE_NAME", name)
                .args([
                    "--exact",
                    "installed_runtime::packages::tests::lease_process_child",
                    "--nocapture",
                ])
                .spawn()
                .unwrap()
        };
        let mut first = child("first");
        let until = Instant::now() + Duration::from_secs(10);
        while !f.0.join("first-held").exists() {
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(10));
        }
        let mut second = child("second");
        std::thread::sleep(Duration::from_millis(150));
        assert!(!f.0.join("second-held").exists());
        fs::write(f.0.join("release"), "go").unwrap();
        assert!(first.wait().unwrap().success());
        assert!(second.wait().unwrap().success());
        assert!(f.0.join("home/.config/namedrop/packages.lock").is_file());
        assert!(!f.0.join("first/namedrop/packages.lock").exists());
        assert!(!f.0.join("second/namedrop/packages.lock").exists());
    }
}
