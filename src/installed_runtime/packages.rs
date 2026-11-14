use std::{
    fs,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Owner {
    Herdr,
    Standalone,
}

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

enum Verdict {
    Write,
    Replaced,
    Foreign(PathBuf),
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
            Verdict::Replaced => Ok(false),
            Verdict::Foreign(state) => Err(format!(
                "ND-INSTALL-002: Namedrop's shared agent packages belong to {}. Use that install to update them. If it is Herdr's install, run Activate Namedrop in Herdr.",
                state.display()
            )),
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
}

pub(crate) fn package_guard(home: Option<&Path>, state: &Path) -> Result<PackageLease, String> {
    let Some(runtime) = super::directory()? else {
        return Ok(PackageLease::development());
    };
    let home = home.ok_or("no HOME is available for package ownership")?;
    if !home.is_absolute() {
        return Err("package ownership requires an absolute HOME".into());
    }
    if fs::canonicalize(&runtime).ok() != fs::canonicalize(state.join("runtime")).ok() {
        return Err("the installed runtime does not belong to this state directory".into());
    }
    locked(home, state, &runtime)
}

fn lock(path: &Path) -> Result<fs::File, String> {
    fs::create_dir_all(path.parent().ok_or("package lock has no directory")?)
        .map_err(|error| error.to_string())?;
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| error.to_string())?;
    if !file
        .metadata()
        .map_err(|error| error.to_string())?
        .is_file()
    {
        return Err("package lock is not a regular file".into());
    }
    file.lock().map_err(|error| error.to_string())?;
    Ok(file)
}

fn same(left: &Path, right: &Path) -> bool {
    left == right
        || fs::canonicalize(left)
            .ok()
            .zip(fs::canonicalize(right).ok())
            .is_some_and(|(left, right)| left == right)
}

fn locked(home: &Path, state: &Path, runtime: &Path) -> Result<PackageLease, String> {
    let user = lock(&crate::locator::package_lock(home))?;
    let local = lock(&runtime.join("packages.lock"))?;
    let selected =
        fs::canonicalize(runtime.join("current/namedrop")).map_err(|error| error.to_string())?;
    let running = std::env::current_exe()
        .and_then(fs::canonicalize)
        .map_err(|error| error.to_string())?;
    let record = crate::locator::read_install_record(&crate::locator::install_record(home)).filter(
        |record| {
            fs::symlink_metadata(record.join("runtime/namedrop")).is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        },
    );
    let verdict = if selected != running {
        Verdict::Replaced
    } else if let Some(record) = record.filter(|record| !same(record, state)) {
        let retired = match fs::read(record.join("runtime/retired.json")) {
            Ok(bytes) => Some(
                serde_json::from_slice::<serde_json::Value>(&bytes)
                    .map_err(|error| error.to_string())?,
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.to_string()),
        };
        let successor = retired
            .as_ref()
            .and_then(|value| value["successor"].as_str())
            .map(Path::new);
        owner(&record)?;
        owner(state)?;
        if successor.is_some_and(|successor| same(successor, state)) {
            Verdict::Write
        } else {
            Verdict::Foreign(record)
        }
    } else {
        Verdict::Write
    };
    let lease = PackageLease {
        _user_lock: Some(user),
        _state_lock: Some(local),
        verdict,
    };
    if lease.writable().unwrap_or(false) {
        crate::locator::write_install_record(&lease, &crate::locator::install_record(home), state)
            .map_err(|error| error.to_string())?;
    }
    Ok(lease)
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
            state
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn take(home: &Path, state: &Path) -> PackageLease {
        locked(home, state, &state.join("runtime")).unwrap()
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
        let a = f.state("a");
        let b = f.state("b");
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
        let a = f.state("a");
        let b = f.state("b");
        drop(take(&home, &a));
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
        f.state("first");
        f.state("second");
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
