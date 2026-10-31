use crate::{
    ActiveThreadResponse, AgentSessionId, DeliveryAttemptId, DeliveryState, HerdrCommandExecutor,
    HerdrCommandOutput, PaneBindingId, StateNamespace, StateStore,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, RawFd},
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    },
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::{Duration, Instant},
};

const ACQUIRE_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_MARKER: u64 = 8192;
static NEXT_MARKER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum InputOwner {
    Delivery {
        namespace: StateNamespace,
        attempt_id: DeliveryAttemptId,
        binding_id: PaneBindingId,
        agent_session_id: AgentSessionId,
    },
    Other,
}

impl InputOwner {
    pub(crate) fn delivery(
        namespace: &StateNamespace,
        attempt_id: &DeliveryAttemptId,
        binding_id: &PaneBindingId,
        agent_session_id: &AgentSessionId,
    ) -> Self {
        Self::Delivery {
            namespace: namespace.clone(),
            attempt_id: attempt_id.clone(),
            binding_id: binding_id.clone(),
            agent_session_id: agent_session_id.clone(),
        }
    }
}

#[derive(Debug)]
pub(crate) enum AcquireError {
    Contended,
    Fault(io::Error),
}

impl From<io::Error> for AcquireError {
    fn from(error: io::Error) -> Self {
        Self::Fault(error)
    }
}

impl std::fmt::Display for AcquireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Contended => write!(
                f,
                "pane effects: {}",
                io::Error::from_raw_os_error(libc::EWOULDBLOCK)
            ),
            Self::Fault(error) => error.fmt(f),
        }
    }
}

pub(crate) struct PaneEffects {
    _lock: File,
    directory: File,
    marker: CString,
    pane: String,
}

#[derive(Debug, Eq, PartialEq)]
enum MarkerOwnership {
    Match,
    LegacyAbsent,
    Reject,
}

fn checked(file: File, directory: bool) -> io::Result<File> {
    let metadata = file.metadata()?;
    if metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file() || metadata.nlink() != 1
        }
    {
        return Err(io::Error::other(
            "Namedrop's lock for this pane has unsafe owner or permissions",
        ));
    }
    Ok(file)
}

fn raw_open_at(directory: &File, name: &CString, flags: i32) -> io::Result<File> {
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            0o600,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn open_at(directory: &File, name: &CString, flags: i32) -> io::Result<File> {
    checked(raw_open_at(directory, name, flags)?, false)
}

impl PaneEffects {
    pub(crate) fn acquire(state: &Path, socket: &str, pane: &str) -> Result<Self, AcquireError> {
        Self::acquire_for(state, socket, pane, ACQUIRE_TIMEOUT)
    }

    fn acquire_for(
        state: &Path,
        socket: &str,
        pane: &str,
        timeout: Duration,
    ) -> Result<Self, AcquireError> {
        Self::acquire_for_with_lock_open(state, socket, pane, timeout, raw_open_at)
    }

    fn acquire_for_with_lock_open(
        state: &Path,
        socket: &str,
        pane: &str,
        timeout: Duration,
        mut open_lock: impl FnMut(&File, &CString, i32) -> io::Result<File>,
    ) -> Result<Self, AcquireError> {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(state)?;
        let state = checked(
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(state)?,
            true,
        )?;
        let created = unsafe { libc::mkdirat(state.as_raw_fd(), c"pane-effects".as_ptr(), 0o700) };
        if created != 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
            return Err(io::Error::last_os_error().into());
        }
        let fd = unsafe {
            libc::openat(
                state.as_raw_fd(),
                c"pane-effects".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error().into());
        }
        let directory = checked(unsafe { File::from_raw_fd(fd) }, true)?;
        let key = Sha256::digest(serde_json::to_vec(&(socket, pane)).map_err(io::Error::from)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let lock_name = CString::new(format!("{key}.lock")).map_err(io::Error::from)?;
        let lock = open_lock(&directory, &lock_name, libc::O_RDWR | libc::O_CREAT)?;
        let lock = checked(lock, false)?;
        acquire_flock(lock.as_raw_fd(), timeout)?;
        Ok(Self {
            _lock: lock,
            directory,
            marker: CString::new(format!("{key}.json")).map_err(io::Error::from)?,
            pane: pane.into(),
        })
    }

    fn record(&self, owner: &InputOwner) -> io::Result<()> {
        let temporary = CString::new(format!(
            "{}.{}.{}.tmp",
            self.marker.to_string_lossy(),
            std::process::id(),
            NEXT_MARKER.fetch_add(1, Ordering::Relaxed)
        ))?;
        let result = (|| {
            let mut file = open_at(
                &self.directory,
                &temporary,
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            )?;
            file.write_all(&serde_json::to_vec(owner)?)?;
            file.sync_all()?;
            if unsafe {
                libc::renameat(
                    self.directory.as_raw_fd(),
                    temporary.as_ptr(),
                    self.directory.as_raw_fd(),
                    self.marker.as_ptr(),
                )
            } != 0
            {
                return Err(io::Error::last_os_error());
            }
            self.directory.sync_all()
        })();
        if result.is_err() {
            unsafe {
                libc::unlinkat(self.directory.as_raw_fd(), temporary.as_ptr(), 0);
            }
        }
        result
    }

    fn ownership(&self, expected: &InputOwner) -> MarkerOwnership {
        let file = match open_at(&self.directory, &self.marker, libc::O_RDONLY) {
            Ok(file) => file,
            Err(error) => {
                return if error.kind() == io::ErrorKind::NotFound {
                    MarkerOwnership::LegacyAbsent
                } else {
                    MarkerOwnership::Reject
                };
            }
        };
        if file.metadata().map_or(true, |meta| meta.len() > MAX_MARKER) {
            return MarkerOwnership::Reject;
        }
        let mut bytes = Vec::new();
        if file.take(MAX_MARKER + 1).read_to_end(&mut bytes).is_ok()
            && serde_json::from_slice::<InputOwner>(&bytes).is_ok_and(|owner| owner == *expected)
        {
            MarkerOwnership::Match
        } else {
            MarkerOwnership::Reject
        }
    }

    pub(crate) fn prompt(
        &self,
        herdr: &impl HerdrCommandExecutor,
        owner: &InputOwner,
        body: &str,
    ) -> Result<HerdrCommandOutput, String> {
        self.record(owner)
            .map_err(|e| format!("could not record pane input ownership: {e}"))?;
        herdr.execute(&["agent", "prompt", &self.pane, body])
    }

    pub(crate) fn run_terminal(
        &self,
        herdr: &impl HerdrCommandExecutor,
        owner: &InputOwner,
        command: &str,
    ) -> Result<HerdrCommandOutput, String> {
        self.record(owner)
            .map_err(|e| format!("could not record pane input ownership: {e}"))?;
        herdr.execute(&["pane", "run", &self.pane, command])
    }
}

#[derive(Default)]
pub(crate) struct InterruptReport {
    pub interrupted_pane_ids: Vec<String>,
    pub warnings: Vec<String>,
}

pub(crate) fn interrupt_if_current(
    state: &Path,
    namespace: &StateNamespace,
    responses: &[ActiveThreadResponse],
    herdr: &impl HerdrCommandExecutor,
    cwd: Option<&Path>,
) -> InterruptReport {
    let mut report = InterruptReport::default();
    for response in responses {
        let result = (|| -> Result<(), String> {
            let effects =
                PaneEffects::acquire(state, &namespace.session_socket_id, &response.pane_id)
                    .map_err(|error| error.to_string())?;
            let live = match cwd {
                Some(cwd) => crate::discover_worktree(herdr, cwd)?,
                None => crate::discover_worktree_for_workspace(herdr, namespace.space_id.as_str())?,
            };
            let live_matches = live.workspace_id == namespace.space_id.as_str()
                && live.panes.iter().any(|pane| {
                    pane.pane_id == response.pane_id
                        && pane.agent_session_id.as_ref() == Some(&response.agent_session_id)
                        && pane.agent_status.as_deref() == Some("working")
                });
            let mut store = StateStore::open_read_only(state)
                .map_err(|e| e.to_string())?
                .ok_or("delivery state is unavailable")?;
            let current = store
                .transaction(|tx| {
                    Ok(response.binding_active
                        && tx
                            .inflight_delivery_attempts(namespace)?
                            .iter()
                            .any(|current| {
                                current.attempt_id == response.attempt_id
                                    && current.binding_id == response.binding_id
                                    && current.binding_active
                                    && current.pane_id == response.pane_id
                                    && current.agent_session_id == response.agent_session_id
                                    && matches!(
                                        current.state,
                                        DeliveryState::Submitted | DeliveryState::Acknowledged
                                    )
                            }))
                })
                .map_err(|e| e.to_string())?;
            drop(store);
            let owner = InputOwner::delivery(
                namespace,
                &response.attempt_id,
                &response.binding_id,
                &response.agent_session_id,
            );
            if !current || !live_matches {
                return Err(
                    "the stopped thread could not be proven to own its current response".into(),
                );
            }
            match effects.ownership(&owner) {
                MarkerOwnership::Match | MarkerOwnership::LegacyAbsent => {}
                MarkerOwnership::Reject => {
                    return Err("the stopped thread no longer owns its pane input".into());
                }
            }
            effects
                .record(&owner)
                .map_err(|error| format!("could not record pane input ownership: {error}"))?;
            herdr.execute(&["agent", "send-keys", &effects.pane, "ctrl+c"])?;
            Ok(())
        })();
        match result {
            Ok(()) => report.interrupted_pane_ids.push(response.pane_id.clone()),
            Err(error) => report.warnings.push(format!(
                "Could not confirm interruption of {} after stopping the thread: {error}.",
                response.pane_id
            )),
        }
    }
    report
}

fn acquire_flock(fd: RawFd, timeout: Duration) -> Result<(), AcquireError> {
    let deadline = Instant::now() + timeout;
    loop {
        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            break;
        }
        let error = io::Error::last_os_error();
        if ![Some(libc::EWOULDBLOCK), Some(libc::EAGAIN)].contains(&error.raw_os_error()) {
            return Err(AcquireError::Fault(error));
        }
        if Instant::now() >= deadline {
            return Err(AcquireError::Contended);
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        io::BufRead,
        os::unix::fs::PermissionsExt,
        process::{Child, Command, Stdio},
        sync::Mutex,
    };

    #[test]
    fn unexpected_flock_errno_is_a_fault_even_at_the_deadline() {
        let error = acquire_flock(-1, Duration::ZERO).unwrap_err();
        assert_eq!(
            error.to_string(),
            io::Error::from_raw_os_error(libc::EBADF).to_string()
        );
        assert!(
            matches!(error, AcquireError::Fault(ref error) if error.raw_os_error() == Some(libc::EBADF)),
            "{error:?}"
        );
    }

    struct State(std::path::PathBuf);
    impl State {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "namedrop-pane-effects-{}-{}",
                std::process::id(),
                NEXT_MARKER.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            Self(path)
        }
    }
    impl Drop for State {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn child(
        state: &Path,
        mode: &str,
    ) -> (ChildGuard, std::io::BufReader<std::process::ChildStdout>) {
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "pane_effects::tests::child_fixture",
                "--ignored",
                "--nocapture",
            ])
            .env("NAMEDROP_PANE_EFFECTS_TEST_STATE", state)
            .env("NAMEDROP_PANE_EFFECTS_TEST_MODE", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let output = std::io::BufReader::new(child.stdout.take().unwrap());
        (ChildGuard(child), output)
    }
    fn line(output: &mut impl std::io::BufRead, expected: &str) {
        loop {
            let mut line = String::new();
            assert!(
                output.read_line(&mut line).unwrap() != 0,
                "child exited before {expected}"
            );
            if line.trim() == expected {
                break;
            }
        }
    }

    #[test]
    #[ignore]
    fn child_fixture() {
        let state = std::env::var_os("NAMEDROP_PANE_EFFECTS_TEST_STATE").expect("fixture state");
        let state = Path::new(&state);
        let mode = std::env::var("NAMEDROP_PANE_EFFECTS_TEST_MODE").unwrap();
        if mode == "contend" {
            println!("attempting");
            io::stdout().flush().unwrap();
            assert!(
                PaneEffects::acquire_for(state, "socket", "pane", Duration::ZERO).is_err(),
                "another process's same-pane gate was bypassed"
            );
            let _other =
                PaneEffects::acquire_for(state, "socket", "other-pane", Duration::ZERO).unwrap();
            println!("excluded; other pane acquired");
            io::stdout().flush().unwrap();
            let mut release = String::new();
            io::stdin().lock().read_line(&mut release).unwrap();
            let _same = PaneEffects::acquire(state, "socket", "pane").unwrap();
            println!("acquired after release");
        } else {
            let _same = PaneEffects::acquire(state, "socket", "pane").unwrap();
            println!("holding");
            io::stdout().flush().unwrap();
            let mut release = String::new();
            io::stdin().lock().read_line(&mut release).unwrap();
        }
    }

    #[test]
    fn transient_lock_open_enoent_recovers_with_exclusion() {
        let state = State::new();
        let mut calls = 0;
        let mut first_open = None;
        let gate = PaneEffects::acquire_for_with_lock_open(
            &state.0,
            "socket",
            "pane",
            Duration::ZERO,
            |directory, name, flags| {
                calls += 1;
                let metadata = directory.metadata().unwrap();
                assert!(metadata.is_dir());
                assert!(metadata.nlink() > 0);
                assert_eq!(metadata.uid(), unsafe { libc::geteuid() });
                assert_eq!(metadata.mode() & 0o777, 0o700);
                let arguments = (directory.as_raw_fd(), name.clone(), flags);
                if calls == 1 {
                    assert_eq!(flags, libc::O_RDWR | libc::O_CREAT);
                    first_open = Some(arguments);
                    Err(io::Error::from_raw_os_error(libc::ENOENT))
                } else {
                    assert_eq!(Some(arguments), first_open);
                    raw_open_at(directory, name, flags)
                }
            },
        )
        .unwrap();
        assert_eq!(calls, 2);
        let metadata = gate._lock.metadata().unwrap();
        assert!(matches!(
            PaneEffects::acquire_for(&state.0, "socket", "pane", Duration::ZERO),
            Err(AcquireError::Contended)
        ));
        let _other =
            PaneEffects::acquire_for(&state.0, "socket", "other-pane", Duration::ZERO).unwrap();
        drop(gate);
        let gate = PaneEffects::acquire_for(&state.0, "socket", "pane", Duration::ZERO).unwrap();
        let reacquired = gate._lock.metadata().unwrap();
        assert_eq!(reacquired.dev(), metadata.dev());
        assert_eq!(reacquired.ino(), metadata.ino());
    }

    #[test]
    fn only_flock_contention_is_retryable() {
        let state = State::new();
        let gate = PaneEffects::acquire(&state.0, "socket", "pane").unwrap();
        assert!(matches!(
            PaneEffects::acquire_for(&state.0, "socket", "pane", Duration::ZERO),
            Err(AcquireError::Contended)
        ));
        drop(gate);
        fs::set_permissions(
            state.0.join("pane-effects"),
            fs::Permissions::from_mode(0o777),
        )
        .unwrap();
        assert!(matches!(
            PaneEffects::acquire_for(&state.0, "socket", "pane", Duration::ZERO),
            Err(AcquireError::Fault(error)) if error.to_string().contains("unsafe")
        ));
        assert!(matches!(
            AcquireError::from(io::Error::from_raw_os_error(libc::EWOULDBLOCK)),
            AcquireError::Fault(_)
        ));
    }

    #[test]
    fn independent_processes_exclude_same_pane_in_both_orders_and_crash_releases() {
        let state = State::new();
        let gate = PaneEffects::acquire(&state.0, "socket", "pane").unwrap();
        let inode = gate._lock.metadata().unwrap().ino();
        assert!(
            PaneEffects::acquire_for(&state.0, "socket", "pane", Duration::ZERO).is_err(),
            "independent acquisitions in one process shared a flock description"
        );
        assert!(PaneEffects::acquire_for(&state.0, "other-socket", "pane", Duration::ZERO).is_ok());
        let (mut contender, mut output) = child(&state.0, "contend");
        line(&mut output, "attempting");
        line(&mut output, "excluded; other pane acquired");
        drop(gate);
        contender
            .0
            .stdin
            .as_mut()
            .unwrap()
            .write_all(b"released\n")
            .unwrap();
        line(&mut output, "acquired after release");
        assert!(contender.0.wait().unwrap().success());
        let (mut owner, mut output) = child(&state.0, "hold");
        line(&mut output, "holding");
        assert!(PaneEffects::acquire_for(&state.0, "socket", "pane", Duration::ZERO).is_err());
        assert!(PaneEffects::acquire_for(&state.0, "socket", "other-pane", Duration::ZERO).is_ok());
        owner.0.kill().unwrap();
        owner.0.wait().unwrap();
        let gate = PaneEffects::acquire(&state.0, "socket", "pane").unwrap();
        assert_eq!(
            gate._lock.metadata().unwrap().ino(),
            inode,
            "lock inode changed"
        );
        assert_ne!(
            unsafe { libc::fcntl(gate._lock.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
    }

    struct FakeInput(Mutex<Vec<String>>);
    impl HerdrCommandExecutor for FakeInput {
        fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
            self.0.lock().unwrap().push(args.join(" "));
            Err("ambiguous transport failure".into())
        }
    }
    fn delivery() -> InputOwner {
        InputOwner::delivery(
            &StateNamespace::for_space("socket", "space"),
            &DeliveryAttemptId::new("attempt"),
            &PaneBindingId::new("binding"),
            &AgentSessionId::new("session"),
        )
    }

    #[test]
    fn later_auxiliary_input_invalidates_old_delivery_even_on_transport_error() {
        let state = State::new();
        let gate = PaneEffects::acquire(&state.0, "socket", "pane").unwrap();
        assert_eq!(gate.ownership(&delivery()), MarkerOwnership::LegacyAbsent);
        gate.record(&delivery()).unwrap();
        assert_eq!(gate.ownership(&delivery()), MarkerOwnership::Match);
        let input = FakeInput(Mutex::new(Vec::new()));
        assert!(
            gate.prompt(&input, &InputOwner::Other, "doctor or registration")
                .is_err()
        );
        assert_eq!(input.0.lock().unwrap().len(), 1);
        assert!(
            gate.ownership(&delivery()) == MarkerOwnership::Reject,
            "later prompt left old interrupt ownership intact"
        );
        let marker = state
            .0
            .join("pane-effects")
            .join(gate.marker.to_str().unwrap());
        fs::write(&marker, "{malformed").unwrap();
        assert_eq!(gate.ownership(&delivery()), MarkerOwnership::Reject);
        fs::remove_file(&marker).unwrap();
        fs::create_dir(&marker).unwrap();
        assert!(gate.prompt(&input, &delivery(), "forbidden").is_err());
        assert_eq!(
            input.0.lock().unwrap().len(),
            1,
            "physical input occurred after failed marker write"
        );
        assert_eq!(gate.ownership(&delivery()), MarkerOwnership::Reject);
    }

    #[test]
    fn first_input_creates_private_state_without_initializing_a_database() {
        let state = State::new();
        let uninitialized = state.0.join("new/namedrop");
        let gate = PaneEffects::acquire(&uninitialized, "socket", "pane").unwrap();
        gate.record(&delivery()).unwrap();
        assert_eq!(gate.ownership(&delivery()), MarkerOwnership::Match);
        assert_eq!(fs::metadata(&uninitialized).unwrap().mode() & 0o777, 0o700);
        assert!(!uninitialized.join("namedrop.sqlite3").exists());
    }

    #[test]
    fn unsafe_ownership_files_are_refused() {
        let state = State::new();
        let gate = PaneEffects::acquire(&state.0, "socket", "pane").unwrap();
        let lock = state
            .0
            .join("pane-effects")
            .join(gate.marker.to_str().unwrap().replace(".json", ".lock"));
        drop(gate);
        fs::set_permissions(&lock, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(PaneEffects::acquire(&state.0, "socket", "pane").is_err());
        fs::remove_file(&lock).unwrap();
        std::os::unix::fs::symlink(state.0.join("outside"), &lock).unwrap();
        assert!(PaneEffects::acquire(&state.0, "socket", "pane").is_err());
        assert!(!state.0.join("outside").exists());
    }

    fn raw_input_paths(directory: &Path, owner: &Path) -> Vec<std::path::PathBuf> {
        let mut violations = Vec::new();
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                violations.extend(raw_input_paths(&path, owner));
            } else if path.extension().is_some_and(|extension| extension == "rs") && path != owner {
                let compact: String = fs::read_to_string(&path)
                    .unwrap()
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .collect();
                if [
                    r#".execute(&["agent","prompt""#,
                    r#".execute(&["agent","send-keys""#,
                    r#".execute(&["pane","run""#,
                ]
                .iter()
                .any(|raw| compact.contains(raw))
                {
                    violations.push(path);
                }
            }
        }
        violations
    }

    #[test]
    fn direct_literal_physical_input_calls_are_owned_here() {
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        assert_eq!(
            raw_input_paths(&source, &source.join("pane_effects.rs")),
            Vec::<std::path::PathBuf>::new()
        );
    }

    #[test]
    fn raw_input_check_detects_new_nested_files_and_formatting() {
        let state = State::new();
        let nested = state.0.join("new/module");
        fs::create_dir_all(&nested).unwrap();
        let owner = state.0.join("pane_effects.rs");
        let bypass = nested.join("input.rs");
        for (kind, operation) in [("agent", "prompt"), ("agent", "send-keys"), ("pane", "run")] {
            let source = format!("herdr.execute ( & [ \"{kind}\" ,\n \"{operation}\", pane ] )");
            fs::write(&owner, &source).unwrap();
            fs::write(&bypass, &source).unwrap();
            assert_eq!(raw_input_paths(&state.0, &owner), vec![bypass.clone()]);
        }
        fs::write(&bypass, "effects.prompt(herdr, owner, body)").unwrap();
        assert!(raw_input_paths(&state.0, &owner).is_empty());
    }
}
