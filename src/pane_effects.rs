use crate::{
    ActiveThreadResponse, AgentSessionId, DeliveryAttemptId, DeliveryState, PaneBindingId,
    StateNamespace, StateStore,
    mux::{AgentState, InputKind, InputOutcome, Key, MuxInput, PaneId, PromptRequest, Recipient},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    ffi::CString,
    fs::{File, OpenOptions},
    io::{self, Read, Write},
    marker::PhantomData,
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
/// Longer than live peek holds a pane's lock for one input: two identity
/// surveys of up to 500 ms, a store read, and a send of up to 2 s, about
/// 3.1 s in all. A thread stop that arrives during that input still
/// interrupts once the input ends.
const INTERRUPT_ACQUIRE_TIMEOUT: Duration = Duration::from_secs(5);
/// Deletes from the cursor to the start of its line in Claude Code's prompt
/// box, and at the start of a line joins it to the line above. Claude Code
/// 2.1.322 does this in a real pane, and every version from 2.1.261 carries
/// the same editing command. Its `chat:clearInput` key, Ctrl+L, only draws
/// the screen again.
const CLAUDE_CODE_DELETE_LINE: Key = Key::CtrlU;
const MAX_MARKER: u64 = 8192;
static NEXT_MARKER: AtomicU64 = AtomicU64::new(0);

enum OpenRetry {
    Directory,
    Lock,
}

impl OpenRetry {
    fn record(self, state: &File, pane: &str) {
        let operation = match self {
            Self::Directory => "directory",
            Self::Lock => "lock",
        };
        if cfg!(any(test, debug_assertions)) {
            let source = if cfg!(test) { "unit-test" } else { "runtime" };
            let record = format!(
                "pane-effects: retrying {operation} open after ENOENT for {} source={source}\n",
                pane.escape_debug()
            );
            let _ = open_at(
                state,
                &c"pane-effects-retries.log".to_owned(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_APPEND,
                pane,
            )
            .and_then(|mut file| file.write_all(record.as_bytes()));
        }
    }
}

#[derive(Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) enum InputOwner {
    Delivery {
        namespace: StateNamespace,
        attempt_id: DeliveryAttemptId,
        binding_id: PaneBindingId,
        agent_session_id: AgentSessionId,
    },
    /// A person typing a line into the dashboard's live peek.
    Dashboard,
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

#[derive(Debug)]
struct PrivateLockError(String);

impl std::fmt::Display for PrivateLockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Namedrop's pane lock for {} is not private to you",
            self.0
        )
    }
}

impl std::error::Error for PrivateLockError {}

impl AcquireError {
    pub(crate) fn with_alias(self, alias: Option<&str>) -> Self {
        match (self, alias) {
            (Self::Fault(error), Some(alias))
                if error
                    .get_ref()
                    .is_some_and(|error| error.is::<PrivateLockError>()) =>
            {
                Self::Fault(io::Error::other(PrivateLockError(format!("@{alias}"))))
            }
            (error, _) => error,
        }
    }
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
    pane: PaneId,
}

/// Input for one pane, borrowed from the `PaneEffects` lock that guards it.
/// Only this module can build one, so `MuxInput::deliver` cannot be reached
/// around the lock. Inside the crate the compiler enforces that through the
/// private fields and constructor. Outside it, code can neither fill in the
/// fields:
///
/// ```compile_fail
/// use namedrop::mux::{InputKind, PaneId, PaneInput};
/// let pane = PaneId::new("w1:p1");
/// let _ = PaneInput {
///     _lock: std::marker::PhantomData,
///     pane: &pane,
///     kind: InputKind::Line("ls"),
/// };
/// ```
///
/// nor call its constructor:
///
/// ```compile_fail
/// use namedrop::mux::{InputKind, PaneInput};
/// let _ = PaneInput::new(todo!(), InputKind::Line("ls"));
/// ```
pub struct PaneInput<'lock> {
    _lock: PhantomData<&'lock PaneEffects>,
    pane: &'lock PaneId,
    kind: InputKind<'lock>,
}

impl<'lock> PaneInput<'lock> {
    fn new(effects: &'lock PaneEffects, kind: InputKind<'lock>) -> Self {
        Self {
            _lock: PhantomData,
            pane: &effects.pane,
            kind,
        }
    }

    pub fn pane(&self) -> &PaneId {
        self.pane
    }

    pub fn kind(&self) -> InputKind<'lock> {
        self.kind
    }
}

#[derive(Debug, Eq, PartialEq)]
enum MarkerOwnership {
    Match,
    LegacyAbsent,
    Reject,
}

fn checked(file: File, directory: bool, pane: &str) -> io::Result<File> {
    let metadata = file.metadata()?;
    if metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o077 != 0
        || if directory {
            !metadata.is_dir()
        } else {
            !metadata.is_file() || metadata.nlink() != 1
        }
    {
        return Err(io::Error::other(PrivateLockError(pane.into())));
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

fn open_at(directory: &File, name: &CString, flags: i32, pane: &str) -> io::Result<File> {
    checked(raw_open_at(directory, name, flags)?, false, pane)
}

fn raw_open_directory(state: &File) -> io::Result<File> {
    let fd = unsafe {
        libc::openat(
            state.as_raw_fd(),
            c"pane-effects".as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
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
        open_lock: impl FnMut(&File, &CString, i32) -> io::Result<File>,
    ) -> Result<Self, AcquireError> {
        Self::acquire_for_with_opens(state, socket, pane, timeout, raw_open_directory, open_lock)
    }

    fn acquire_for_with_opens(
        state: &Path,
        socket: &str,
        pane: &str,
        timeout: Duration,
        mut open_directory: impl FnMut(&File) -> io::Result<File>,
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
            pane,
        )?;
        let created = unsafe { libc::mkdirat(state.as_raw_fd(), c"pane-effects".as_ptr(), 0o700) };
        if created != 0 && io::Error::last_os_error().kind() != io::ErrorKind::AlreadyExists {
            return Err(io::Error::last_os_error().into());
        }
        let directory = match open_directory(&state) {
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                OpenRetry::Directory.record(&state, pane);
                open_directory(&state)?
            }
            result => result?,
        };
        let directory = checked(directory, true, pane)?;
        let key = Sha256::digest(serde_json::to_vec(&(socket, pane)).map_err(io::Error::from)?)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let lock_name = CString::new(format!("{key}.lock")).map_err(io::Error::from)?;
        let flags = libc::O_RDWR | libc::O_CREAT;
        let lock = match open_lock(&directory, &lock_name, flags) {
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => {
                OpenRetry::Lock.record(&state, pane);
                open_lock(&directory, &lock_name, flags)?
            }
            result => result?,
        };
        let lock = checked(lock, false, pane)?;
        acquire_flock(lock.as_raw_fd(), timeout)?;
        Ok(Self {
            _lock: lock,
            directory,
            marker: CString::new(format!("{key}.json")).map_err(io::Error::from)?,
            pane: PaneId::new(pane),
        })
    }

    fn record(&self, owner: &InputOwner) -> io::Result<()> {
        self.write_marker(&serde_json::to_vec(owner)?)
    }

    fn write_marker(&self, bytes: &[u8]) -> io::Result<()> {
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
                self.pane.as_str(),
            )?;
            file.write_all(bytes)?;
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

    /// The marker's bytes, or `None` when the pane has no marker.
    fn read_marker(&self) -> io::Result<Option<Vec<u8>>> {
        let file = match open_at(
            &self.directory,
            &self.marker,
            libc::O_RDONLY,
            self.pane.as_str(),
        ) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if file.metadata()?.len() > MAX_MARKER {
            return Err(io::Error::other("pane input ownership marker is too large"));
        }
        let mut bytes = Vec::new();
        file.take(MAX_MARKER + 1).read_to_end(&mut bytes)?;
        Ok(Some(bytes))
    }

    fn restore_marker(&self, previous: Option<&[u8]>) -> io::Result<()> {
        let Some(bytes) = previous else {
            if unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.marker.as_ptr(), 0) } != 0 {
                return Err(io::Error::last_os_error());
            }
            return self.directory.sync_all();
        };
        self.write_marker(bytes)
    }

    fn ownership(&self, expected: &InputOwner) -> MarkerOwnership {
        match self.read_marker() {
            Ok(None) => MarkerOwnership::LegacyAbsent,
            Ok(Some(bytes))
                if serde_json::from_slice::<InputOwner>(&bytes)
                    .is_ok_and(|owner| owner == *expected) =>
            {
                MarkerOwnership::Match
            }
            Ok(Some(_)) | Err(_) => MarkerOwnership::Reject,
        }
    }

    /// Records `owner` as the pane's input owner, then sends. A record that
    /// fails sends nothing, and is held like any failure that cannot prove
    /// the input never arrived.
    fn owned_input(
        &self,
        mux: &impl MuxInput,
        owner: &InputOwner,
        kind: InputKind<'_>,
    ) -> InputOutcome {
        if let Err(error) = self.record(owner) {
            return InputOutcome::Ambiguous(format!(
                "could not record pane input ownership: {error}"
            ));
        }
        mux.deliver(PaneInput::new(self, kind))
    }

    pub(crate) fn prompt(
        &self,
        mux: &impl MuxInput,
        owner: &InputOwner,
        request: PromptRequest<'_>,
    ) -> InputOutcome {
        self.owned_input(mux, owner, InputKind::Prompt(request))
    }

    /// Presses the key that deletes a line of Claude Code's prompt box,
    /// `presses` times. It types nothing, so the pane's input keeps its owner.
    pub(crate) fn delete_claude_code_input(
        &self,
        mux: &impl MuxInput,
        presses: usize,
    ) -> InputOutcome {
        let keys = vec![CLAUDE_CODE_DELETE_LINE; presses];
        mux.deliver(PaneInput::new(
            self,
            InputKind::Keys {
                recipient: Recipient::Agent,
                keys: &keys,
            },
        ))
    }

    pub(crate) fn run_terminal(
        &self,
        mux: &impl MuxInput,
        owner: &InputOwner,
        command: &str,
    ) -> InputOutcome {
        self.owned_input(mux, owner, InputKind::Line(command))
    }
}

/// What a person sends from the dashboard's live peek.
pub(crate) enum DashboardInput<'a> {
    /// `pane run`. `turn` is the pane's state in the survey taken under the
    /// lock, which decides whose turn the line belongs to.
    Line {
        text: &'a str,
        turn: Option<&'a AgentState>,
    },
    /// `pane send-keys`: an answer inside the turn already running.
    Key(Key),
}

/// The pane lock as the dashboard holds it, taken in one attempt. It sends
/// only as the person, so dashboard code can neither wait behind another
/// owner nor record a delivery's ownership.
pub(crate) struct DashboardPane(PaneEffects);

impl DashboardPane {
    /// One attempt, for a person who sees `Contended` at once instead of
    /// input that lands after another owner's.
    pub(crate) fn try_acquire(
        state: &Path,
        socket: &str,
        pane: &str,
    ) -> Result<Self, AcquireError> {
        PaneEffects::acquire_for(state, socket, pane, Duration::ZERO).map(Self)
    }

    /// A key keeps the pane's input owner, as `delete_claude_code_input`
    /// does. So does a line typed while a delivery's agent is blocked on a
    /// question or a permission prompt: the line answers it, and Stop still
    /// interrupts that turn. Any other line starts the person's own turn and
    /// makes them the owner, so a stopped thread no longer interrupts it.
    pub(crate) fn input(&self, mux: &impl MuxInput, input: DashboardInput<'_>) -> InputOutcome {
        let effects = &self.0;
        match input {
            DashboardInput::Line { text, turn } => {
                let previous = effects.read_marker();
                // A working agent takes a line as the person's next
                // instruction, and Codex drops the delivered task for it. The
                // marker may also name a delivery whose turn already ended,
                // since nothing clears it when the turn ends. So a line typed
                // while the agent works is the person's turn. A blocked
                // prompt in a turn the person started without a marker write
                // looks the same, so its answer keeps the delivery's marker
                // and Stop behaves as it did before the dashboard typed.
                let answers_delivery = matches!(turn, Some(AgentState::Blocked))
                    && matches!(&previous, Ok(Some(bytes)) if matches!(
                        serde_json::from_slice(bytes),
                        Ok(InputOwner::Delivery { .. })
                    ));
                if answers_delivery {
                    return mux.deliver(PaneInput::new(effects, InputKind::Line(text)));
                }
                // A send that proves nothing was typed hands the pane back.
                // An ambiguous one keeps the person as owner, as a doctor's
                // or registration's does, since their line may have landed.
                let outcome =
                    effects.owned_input(mux, &InputOwner::Dashboard, InputKind::Line(text));
                if matches!(
                    outcome,
                    InputOutcome::Declined(..) | InputOutcome::Refused(_)
                ) && let Ok(previous) = previous
                {
                    // A restore that fails leaves the person as owner, so a
                    // stop warns instead of interrupting.
                    let _ = effects.restore_marker(previous.as_deref());
                }
                outcome
            }
            DashboardInput::Key(key) => mux.deliver(PaneInput::new(
                effects,
                InputKind::Keys {
                    recipient: Recipient::Pane,
                    keys: &[key],
                },
            )),
        }
    }
}

#[derive(Default)]
pub(crate) struct InterruptReport {
    pub interrupted_pane_ids: Vec<String>,
    pub warnings: Vec<String>,
}

impl InterruptReport {
    /// No response could be interrupted, because the multiplexer could not be
    /// reached at all.
    pub(crate) fn unreached(responses: &[ActiveThreadResponse], error: &str) -> Self {
        Self {
            interrupted_pane_ids: Vec::new(),
            warnings: responses
                .iter()
                .map(|response| unconfirmed(&response.pane_id, error))
                .collect(),
        }
    }
}

fn unconfirmed(pane_id: &str, error: &str) -> String {
    format!("Could not confirm interruption of {pane_id} after stopping the thread: {error}.")
}

pub(crate) fn interrupt_if_current(
    state: &Path,
    namespace: &StateNamespace,
    responses: &[ActiveThreadResponse],
    herdr: &impl MuxInput,
    cwd: Option<&Path>,
) -> InterruptReport {
    let mut report = InterruptReport::default();
    for response in responses {
        let result = (|| -> Result<(), String> {
            let effects = PaneEffects::acquire_for(
                state,
                &namespace.session_socket_id,
                &response.pane_id,
                INTERRUPT_ACQUIRE_TIMEOUT,
            )
            .map_err(|error| error.to_string())?;
            let live = match cwd {
                Some(cwd) => crate::discover_worktree(herdr, cwd)?,
                None => crate::discover_worktree_for_workspace(herdr, namespace.space_id.as_str())?,
            };
            let live_matches = live.workspace_id == namespace.space_id.as_str()
                && live.panes.iter().any(|pane| {
                    pane.pane_id == response.pane_id
                        && pane.agent_session_id() == Some(&response.agent_session_id)
                        && pane.agent_status == Some(crate::mux::AgentState::Working)
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
            match effects.owned_input(
                herdr,
                &owner,
                InputKind::Keys {
                    recipient: Recipient::Agent,
                    keys: &[Key::CtrlC],
                },
            ) {
                InputOutcome::Delivered => Ok(()),
                InputOutcome::Declined(_, error)
                | InputOutcome::Refused(error)
                | InputOutcome::Ambiguous(error) => Err(error),
            }
        })();
        match result {
            Ok(()) => report.interrupted_pane_ids.push(response.pane_id.clone()),
            Err(error) => report.warnings.push(unconfirmed(&response.pane_id, &error)),
        }
    }
    report
}

#[cfg(test)]
thread_local! {
    static FLOCK_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

fn acquire_flock(fd: RawFd, timeout: Duration) -> Result<(), AcquireError> {
    let deadline = Instant::now() + timeout;
    loop {
        #[cfg(test)]
        FLOCK_ATTEMPTS.set(FLOCK_ATTEMPTS.get() + 1);
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

    fn assert_retry_record(state: &State, operation: &str) {
        let log = state.0.join("pane-effects-retries.log");
        assert_eq!(
            fs::read_to_string(&log).unwrap_or_default(),
            format!(
                "pane-effects: retrying {operation} open after ENOENT for pane source=unit-test\n"
            )
        );
        let metadata = fs::metadata(log).unwrap();
        assert_eq!(metadata.mode() & 0o777, 0o600);
        assert_eq!(metadata.nlink(), 1);
    }

    #[test]
    fn retry_diagnostics_refuse_unsafe_files_without_changing_acquisition() {
        for unsafe_file in ["symlink", "hardlink", "public", "directory"] {
            let state = State::new();
            let log = state.0.join("pane-effects-retries.log");
            let outside = state.0.join("untouched");
            fs::write(&outside, "untouched").unwrap();
            match unsafe_file {
                "symlink" => std::os::unix::fs::symlink(&outside, &log).unwrap(),
                "hardlink" => fs::hard_link(&outside, &log).unwrap(),
                "public" => {
                    fs::write(&log, "untouched").unwrap();
                    fs::set_permissions(&log, fs::Permissions::from_mode(0o644)).unwrap();
                }
                "directory" => fs::create_dir(&log).unwrap(),
                _ => unreachable!(),
            }
            let mut calls = 0;
            let _gate = PaneEffects::acquire_for_with_opens(
                &state.0,
                "socket",
                "pane",
                ACQUIRE_TIMEOUT,
                |state| {
                    calls += 1;
                    if calls == 1 {
                        Err(io::Error::from_raw_os_error(libc::ENOENT))
                    } else {
                        raw_open_directory(state)
                    }
                },
                raw_open_at,
            )
            .unwrap();
            assert_eq!(fs::read_to_string(outside).unwrap(), "untouched");
            if unsafe_file != "directory" {
                assert_eq!(fs::read_to_string(log).unwrap(), "untouched");
            }
        }
    }

    #[test]
    fn a_cloned_open_description_can_hold_a_lock_after_its_gate_is_dropped() {
        let state = State::new();
        let gate = PaneEffects::acquire(&state.0, "socket", "pane").unwrap();
        let inherited = gate._lock.try_clone().unwrap();
        drop(gate);
        assert!(matches!(
            PaneEffects::acquire_for(&state.0, "socket", "pane", Duration::ZERO),
            Err(AcquireError::Contended)
        ));
        let release = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            drop(inherited);
        });
        let gate = PaneEffects::acquire_for(&state.0, "socket", "pane", ACQUIRE_TIMEOUT).unwrap();
        release.join().unwrap();
        drop(gate);
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
    fn transient_directory_open_enoent_recovers_with_exclusion() {
        let state = State::new();
        fs::DirBuilder::new()
            .mode(0o700)
            .create(state.0.join("pane-effects"))
            .unwrap();
        let mut calls = 0;
        let mut first_fd = None;
        let gate = PaneEffects::acquire_for_with_opens(
            &state.0,
            "socket",
            "pane",
            Duration::ZERO,
            |state| {
                calls += 1;
                if calls == 1 {
                    first_fd = Some(state.as_raw_fd());
                    Err(io::Error::from_raw_os_error(libc::ENOENT))
                } else {
                    assert_eq!(Some(state.as_raw_fd()), first_fd);
                    raw_open_directory(state)
                }
            },
            raw_open_at,
        )
        .unwrap_or_else(|error| panic!("directory ENOENT must recover: {error}"));
        assert_eq!(calls, 2);
        assert_retry_record(&state, "directory");
        assert_eq!(gate.directory.metadata().unwrap().mode() & 0o777, 0o700);
        assert!(matches!(
            PaneEffects::acquire_for(&state.0, "socket", "pane", Duration::ZERO),
            Err(AcquireError::Contended)
        ));
        let _other =
            PaneEffects::acquire_for(&state.0, "socket", "other-pane", Duration::ZERO).unwrap();
        drop(gate);
        PaneEffects::acquire_for(&state.0, "socket", "pane", ACQUIRE_TIMEOUT).unwrap();
    }

    #[test]
    fn raw_directory_open_faults_have_bounded_attempts() {
        for (errno, expected_calls) in [(libc::ENOENT, 2), (libc::EACCES, 1), (libc::ELOOP, 1)] {
            let state = State::new();
            let mut directory_calls = 0;
            let mut lock_calls = 0;
            let result = PaneEffects::acquire_for_with_opens(
                &state.0,
                "socket",
                "pane",
                Duration::ZERO,
                |_| {
                    directory_calls += 1;
                    Err(io::Error::from_raw_os_error(errno))
                },
                |directory, name, flags| {
                    lock_calls += 1;
                    raw_open_at(directory, name, flags)
                },
            );
            assert_eq!(directory_calls, expected_calls, "errno {errno}");
            assert_eq!(lock_calls, 0);
            assert!(matches!(
                result,
                Err(AcquireError::Fault(error)) if error.raw_os_error() == Some(errno)
            ));
        }
    }

    #[test]
    fn retried_directory_open_still_validates_privacy_before_lock_open() {
        let state = State::new();
        let directory = state.0.join("pane-effects");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(&directory, fs::Permissions::from_mode(0o777)).unwrap();
        let mut directory_calls = 0;
        let mut lock_calls = 0;
        let result = PaneEffects::acquire_for_with_opens(
            &state.0,
            "socket",
            "pane",
            Duration::ZERO,
            |state| {
                directory_calls += 1;
                if directory_calls == 1 {
                    Err(io::Error::from_raw_os_error(libc::ENOENT))
                } else {
                    raw_open_directory(state)
                }
            },
            |directory, name, flags| {
                lock_calls += 1;
                raw_open_at(directory, name, flags)
            },
        );
        assert_eq!(directory_calls, 2);
        assert_eq!(lock_calls, 0);
        assert!(matches!(
            result,
            Err(AcquireError::Fault(error)) if error.to_string()
                == "Namedrop's pane lock for pane is not private to you"
        ));
    }

    #[test]
    fn unsafe_lock_error_names_the_agent_and_keeps_the_actual_pane_lock_key() {
        let state = State::new();
        let gate = PaneEffects::acquire(&state.0, "socket", "w1:p3").unwrap();
        let lock = state
            .0
            .join("pane-effects")
            .join(gate.marker.to_str().unwrap().replace(".json", ".lock"));
        drop(gate);
        fs::hard_link(&lock, state.0.join("extra-link")).unwrap();
        let error = match PaneEffects::acquire(&state.0, "socket", "w1:p3") {
            Err(error) => error,
            Ok(_) => panic!("hard-linked lock must be refused"),
        };
        assert_eq!(
            error.to_string(),
            "Namedrop's pane lock for w1:p3 is not private to you"
        );
        assert_eq!(
            error.with_alias(Some("planner")).to_string(),
            "Namedrop's pane lock for @planner is not private to you"
        );
        assert_eq!(
            fs::read_dir(state.0.join("pane-effects")).unwrap().count(),
            1
        );
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
        assert_retry_record(&state, "lock");
        let metadata = gate._lock.metadata().unwrap();
        assert!(matches!(
            PaneEffects::acquire_for(&state.0, "socket", "pane", Duration::ZERO),
            Err(AcquireError::Contended)
        ));
        let _other =
            PaneEffects::acquire_for(&state.0, "socket", "other-pane", Duration::ZERO).unwrap();
        drop(gate);
        let gate = PaneEffects::acquire_for(&state.0, "socket", "pane", ACQUIRE_TIMEOUT).unwrap();
        let reacquired = gate._lock.metadata().unwrap();
        assert_eq!(reacquired.dev(), metadata.dev());
        assert_eq!(reacquired.ino(), metadata.ino());
    }

    #[test]
    fn raw_lock_open_faults_have_bounded_attempts() {
        for (errno, expected_calls) in [(libc::ENOENT, 2), (libc::EACCES, 1), (libc::ELOOP, 1)] {
            let state = State::new();
            let mut calls = 0;
            let result = PaneEffects::acquire_for_with_lock_open(
                &state.0,
                "socket",
                "pane",
                Duration::ZERO,
                |_, _, _| {
                    calls += 1;
                    Err(io::Error::from_raw_os_error(errno))
                },
            );
            assert_eq!(calls, expected_calls);
            assert!(matches!(
                result,
                Err(AcquireError::Fault(error)) if error.raw_os_error() == Some(errno)
            ));
        }
    }

    #[test]
    fn unsafe_lock_is_refused_without_retrying_validation() {
        for transient_enoent in [false, true] {
            let state = State::new();
            let gate = PaneEffects::acquire(&state.0, "socket", "pane").unwrap();
            gate._lock
                .set_permissions(fs::Permissions::from_mode(0o666))
                .unwrap();
            drop(gate);
            let mut calls = 0;
            let result = PaneEffects::acquire_for_with_lock_open(
                &state.0,
                "socket",
                "pane",
                Duration::ZERO,
                |directory, name, flags| {
                    calls += 1;
                    if transient_enoent && calls == 1 {
                        Err(io::Error::from_raw_os_error(libc::ENOENT))
                    } else {
                        raw_open_at(directory, name, flags)
                    }
                },
            );
            assert_eq!(calls, if transient_enoent { 2 } else { 1 });
            assert!(matches!(
                result,
                Err(AcquireError::Fault(error)) if error.to_string().contains("not private to you")
            ));
            let lock = fs::read_dir(state.0.join("pane-effects"))
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path();
            fs::set_permissions(lock, fs::Permissions::from_mode(0o600)).unwrap();
            let _gate = PaneEffects::acquire(&state.0, "socket", "pane").unwrap();
            assert!(matches!(
                PaneEffects::acquire_for(&state.0, "socket", "pane", Duration::ZERO),
                Err(AcquireError::Contended)
            ));
        }
    }

    #[test]
    fn unsafe_parent_is_refused_before_lock_open() {
        let state = State::new();
        let directory = state.0.join("pane-effects");
        fs::create_dir(&directory).unwrap();
        fs::set_permissions(directory, fs::Permissions::from_mode(0o777)).unwrap();
        let mut calls = 0;
        let result = PaneEffects::acquire_for_with_lock_open(
            &state.0,
            "socket",
            "pane",
            Duration::ZERO,
            |directory, name, flags| {
                calls += 1;
                raw_open_at(directory, name, flags)
            },
        );
        assert_eq!(calls, 0);
        assert!(matches!(
            result,
            Err(AcquireError::Fault(error)) if error.to_string().contains("not private to you")
        ));
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
            Err(AcquireError::Fault(error)) if error.to_string().contains("not private to you")
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
    impl crate::HerdrCommandExecutor for FakeInput {
        fn execute(&self, args: &[&str]) -> Result<crate::HerdrCommandOutput, String> {
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
        assert_eq!(
            gate.run_terminal(&input, &InputOwner::Other, "doctor or registration"),
            InputOutcome::Ambiguous("ambiguous transport failure".into())
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
        assert!(matches!(
            gate.run_terminal(&input, &delivery(), "forbidden"),
            InputOutcome::Ambiguous(error)
                if error.starts_with("could not record pane input ownership: ")
        ));
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

    fn author() -> crate::mux::fake::FakeMux {
        crate::mux::fake::FakeMux::world()
            .space("workspace-1")
            .pane("pane")
            .claude()
            .session("session")
            .idle()
            .build()
    }

    #[test]
    fn a_prompt_reaches_the_multiplexer_only_through_its_pane_lock() {
        use crate::mux::{
            Admit, Depth, MuxRead, Scope,
            fake::{Sent, SentInput},
        };
        let state = State::new();
        let mux = author();
        let expect = mux
            .survey(Scope::All, Depth::Panes)
            .unwrap()
            .spaces
            .remove(0)
            .panes
            .remove(0)
            .agent
            .unwrap();
        let gate = PaneEffects::acquire(&state.0, "socket", "pane").unwrap();
        let request = PromptRequest {
            expect: &expect,
            text: "hello",
            admit: Admit::Settled,
        };
        assert_eq!(
            gate.prompt(&mux, &delivery(), request),
            InputOutcome::Delivered
        );
        assert_eq!(gate.ownership(&delivery()), MarkerOwnership::Match);
        assert_eq!(
            gate.prompt(&mux, &delivery(), request),
            InputOutcome::Declined(
                crate::mux::Retry::OnEvent,
                "pane pane cannot take a prompt now".into()
            ),
            "the agent the first prompt started is working"
        );
        assert_eq!(
            gate.delete_claude_code_input(&mux, 2),
            InputOutcome::Delivered
        );
        assert_eq!(
            gate.run_terminal(&mux, &InputOwner::Other, "ls"),
            InputOutcome::Delivered
        );
        let pane = PaneId::new("pane");
        assert_eq!(
            mux.sent(),
            [
                Sent {
                    pane: pane.clone(),
                    input: SentInput::Prompt("hello".into()),
                },
                Sent {
                    pane: pane.clone(),
                    input: SentInput::Keys {
                        recipient: Recipient::Agent,
                        keys: vec![Key::CtrlU, Key::CtrlU],
                    },
                },
                Sent {
                    pane,
                    input: SentInput::Line("ls".into()),
                },
            ]
        );
        assert_eq!(gate.ownership(&delivery()), MarkerOwnership::Reject);
    }

    #[test]
    fn a_dashboard_key_never_waits_behind_a_delivery_and_keeps_its_owner() {
        use crate::mux::fake::{Sent, SentInput};
        let state = State::new();
        let delivering = PaneEffects::acquire(&state.0, "socket", "pane").unwrap();
        delivering.record(&delivery()).unwrap();
        FLOCK_ATTEMPTS.set(0);
        assert!(matches!(
            DashboardPane::try_acquire(&state.0, "socket", "pane"),
            Err(AcquireError::Contended)
        ));
        assert_eq!(
            FLOCK_ATTEMPTS.get(),
            1,
            "the dashboard tried the delivery's lock again instead of answering busy"
        );
        drop(delivering);
        let mux = author();
        let gate = DashboardPane::try_acquire(&state.0, "socket", "pane").unwrap();
        assert_eq!(
            gate.input(&mux, DashboardInput::Key(Key::Escape)),
            InputOutcome::Delivered
        );
        assert_eq!(
            gate.0.ownership(&delivery()),
            MarkerOwnership::Match,
            "a key the person pressed inside the delivery's turn kept Stop from interrupting it"
        );
        assert_eq!(
            gate.input(&mux, typed("my own prompt", Some(&AgentState::Idle))),
            InputOutcome::Delivered
        );
        assert_eq!(
            gate.0.ownership(&delivery()),
            MarkerOwnership::Reject,
            "a stopped thread would interrupt the turn the person typed"
        );
        assert_eq!(
            gate.0.ownership(&InputOwner::Dashboard),
            MarkerOwnership::Match
        );
        let pane = PaneId::new("pane");
        assert_eq!(
            mux.sent(),
            [
                Sent {
                    pane: pane.clone(),
                    input: SentInput::Keys {
                        recipient: Recipient::Pane,
                        keys: vec![Key::Escape],
                    },
                },
                Sent {
                    pane,
                    input: SentInput::Line("my own prompt".into()),
                },
            ]
        );
    }

    fn typed<'a>(text: &'a str, turn: Option<&'a AgentState>) -> DashboardInput<'a> {
        DashboardInput::Line { text, turn }
    }

    #[test]
    fn a_dashboard_line_answers_a_delivery_s_blocked_question_and_starts_its_own_turn_otherwise() {
        let unknown = AgentState::Unknown("offline".into());
        for (turn, previous, answers) in [
            (Some(&AgentState::Blocked), Some(delivery()), true),
            (Some(&AgentState::Working), Some(delivery()), false),
            (Some(&AgentState::Idle), Some(delivery()), false),
            (Some(&AgentState::Done), Some(delivery()), false),
            (Some(&unknown), Some(delivery()), false),
            (None, Some(delivery()), false),
            (Some(&AgentState::Blocked), Some(InputOwner::Other), false),
            (
                Some(&AgentState::Blocked),
                Some(InputOwner::Dashboard),
                false,
            ),
            (Some(&AgentState::Blocked), None, false),
        ] {
            let state = State::new();
            let gate = DashboardPane::try_acquire(&state.0, "socket", "pane").unwrap();
            if let Some(owner) = &previous {
                gate.0.record(owner).unwrap();
            }
            let mux = author();
            assert_eq!(gate.input(&mux, typed("2", turn)), InputOutcome::Delivered);
            let expected = if answers {
                delivery()
            } else {
                InputOwner::Dashboard
            };
            assert_eq!(
                gate.0.ownership(&expected),
                MarkerOwnership::Match,
                "a line typed while the pane was {turn:?} under {previous:?} left the wrong owner"
            );
        }
    }

    #[test]
    fn a_failed_answer_to_a_delivery_s_blocked_question_leaves_it_the_delivery_s() {
        for outcome in [
            InputOutcome::Refused("invalid_input".into()),
            InputOutcome::Declined(crate::mux::Retry::OnEvent, "pane_not_found".into()),
            InputOutcome::Ambiguous("timed out".into()),
        ] {
            let state = State::new();
            let gate = DashboardPane::try_acquire(&state.0, "socket", "pane").unwrap();
            gate.0.record(&delivery()).unwrap();
            let mux = author();
            mux.script("pane", outcome.clone());
            assert_eq!(
                gate.input(&mux, typed("yes", Some(&AgentState::Blocked))),
                outcome
            );
            assert_eq!(
                gate.0.ownership(&delivery()),
                MarkerOwnership::Match,
                "{outcome:?} took the delivery's turn from Stop"
            );
        }
    }

    #[test]
    fn a_dashboard_line_that_types_nothing_hands_the_pane_back() {
        for (previous, outcome) in [
            (
                Some(delivery()),
                InputOutcome::Refused("invalid_input".into()),
            ),
            (
                Some(delivery()),
                InputOutcome::Declined(crate::mux::Retry::OnEvent, "pane_not_found".into()),
            ),
            (None, InputOutcome::Refused("invalid_input".into())),
        ] {
            let state = State::new();
            let gate = DashboardPane::try_acquire(&state.0, "socket", "pane").unwrap();
            if let Some(owner) = &previous {
                gate.0.record(owner).unwrap();
            }
            let mux = author();
            mux.script("pane", outcome.clone());
            assert_eq!(
                gate.input(&mux, typed("refused", Some(&AgentState::Idle))),
                outcome
            );
            assert_eq!(
                gate.0.ownership(&delivery()),
                if previous.is_some() {
                    MarkerOwnership::Match
                } else {
                    MarkerOwnership::LegacyAbsent
                },
                "{outcome:?} took the pane from its owner"
            );
        }
    }

    #[test]
    fn a_dashboard_line_that_may_have_landed_keeps_the_person_as_owner() {
        let state = State::new();
        let gate = DashboardPane::try_acquire(&state.0, "socket", "pane").unwrap();
        gate.0.record(&delivery()).unwrap();
        let mux = author();
        mux.script("pane", InputOutcome::Ambiguous("timed out".into()));
        assert_eq!(
            gate.input(&mux, typed("maybe typed", Some(&AgentState::Idle))),
            InputOutcome::Ambiguous("timed out".into())
        );
        assert_eq!(gate.0.ownership(&delivery()), MarkerOwnership::Reject);
        assert_eq!(
            gate.0.ownership(&InputOwner::Dashboard),
            MarkerOwnership::Match
        );
    }

    /// Source with whitespace and string conversions removed, so
    /// `"pane" .to_owned(), String::from("run")` reads as `"pane","run"`.
    fn compact(source: &str) -> String {
        let mut text = [".to_owned()", ".to_string()", ".into()"].iter().fold(
            source.split_whitespace().collect::<String>(),
            |text, conversion| text.replace(conversion, ""),
        );
        while let Some(at) = text.find("String::from(") {
            let call = call_at(&text[at..], "String::from(");
            let argument = call["String::from(".len()..call.len() - 1].to_owned();
            text.replace_range(at..at + call.len(), &argument);
        }
        text
    }

    fn rust_sources(directory: &Path, found: &mut Vec<std::path::PathBuf>) {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                rust_sources(&path, found);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                found.push(path);
            }
        }
    }

    /// Herdr arguments that type into a pane.
    const INPUT_VERBS: &[&str] = &[
        r#""agent","prompt""#,
        r#""pane","run""#,
        r#""send-keys""#,
        r#""send-text""#,
    ];

    /// The input verbs `source` names outside a slice pattern. A pattern such
    /// as a fake's `["agent","prompt",..] =>` matches arguments; it cannot
    /// send them.
    fn input_verbs(source: &str) -> Vec<&'static str> {
        // `.arg("pane").arg("run")` names the same verb as `"pane","run"`.
        let text = compact(source).replace(").arg(", ",");
        INPUT_VERBS
            .iter()
            .copied()
            .filter(|verb| {
                text.match_indices(verb).any(|(at, _)| {
                    let after = &text[at + verb.len()..];
                    !after.find(']').is_some_and(|end| {
                        after[end + 1..].starts_with("=>") || after[end + 1..].starts_with('|')
                    })
                })
            })
            .collect()
    }

    /// Files under `src` that name a Herdr input verb, other than the Herdr
    /// adapter and this module. Until commit 16 makes the transport private
    /// to `mux/herdr/`, only these two may build the arguments that type.
    fn raw_input_outside_the_adapter(src: &Path) -> Vec<String> {
        let mut files = Vec::new();
        rust_sources(src, &mut files);
        files.sort();
        let owners = [src.join("mux/herdr.rs"), src.join("pane_effects.rs")];
        files
            .iter()
            .filter(|file| !file.starts_with(src.join("mux/herdr")) && !owners.contains(file))
            .flat_map(|file| {
                input_verbs(&fs::read_to_string(file).unwrap())
                    .into_iter()
                    .map(move |verb| format!("{} names {verb}", file.display()))
            })
            .collect()
    }

    #[test]
    fn direct_literal_physical_input_calls_are_owned_here() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        assert_eq!(raw_input_outside_the_adapter(&src), Vec::<String>::new());
    }

    #[test]
    fn raw_input_check_detects_new_nested_files_and_formatting() {
        let state = State::new();
        for directory in ["mux/herdr", "new/module"] {
            fs::create_dir_all(state.0.join(directory)).unwrap();
        }
        let bypass = state.0.join("new/module/input.rs");
        for (kind, operation) in [
            ("agent", "prompt"),
            ("pane", "run"),
            ("pane", "send-keys"),
            ("agent", "send-keys"),
            ("pane", "send-text"),
        ] {
            for source in [
                format!("herdr.execute ( & [ \"{kind}\" ,\n \"{operation}\", pane ] )"),
                format!("vec![\"{kind}\".to_owned(),\n \"{operation}\".to_owned(), pane]"),
                format!("Command::new(bin).arg(\"{kind}\")\n .arg(\"{operation}\").arg(pane)"),
                format!("vec![String::from(\"{kind}\"), String::from(\"{operation}\"), pane]"),
            ] {
                for owner in ["pane_effects.rs", "mux/herdr.rs", "mux/herdr/wire.rs"] {
                    fs::write(state.0.join(owner), &source).unwrap();
                }
                fs::write(&bypass, &source).unwrap();
                let found = raw_input_outside_the_adapter(&state.0);
                assert_eq!(found.len(), 1, "{source}: {found:?}");
                assert!(
                    found[0].starts_with(&bypass.display().to_string()),
                    "{found:?}"
                );
            }
        }
        fs::write(
            &bypass,
            "match args { [\"agent\", \"prompt\", pane, _] => {} [\"pane\", \"run\", ..] | [\"pane\", \"send-keys\", ..] => {} _ => {} }\neffects.prompt(herdr, owner, body)",
        )
        .unwrap();
        assert_eq!(
            raw_input_outside_the_adapter(&state.0),
            Vec::<String>::new()
        );
    }

    /// Text that would let dashboard code type into a pane around
    /// `DashboardPane`, and the one dashboard file each may appear in. The
    /// dashboard is a sibling of the delivery code in this crate, so no
    /// visibility can hide `PaneEffects` or `InputOwner` from it alone, and
    /// until the Herdr transport is private to `mux/herdr/` the compiler
    /// cannot stop it naming that either.
    const DASHBOARD_RAW: &[(&str, Option<&str>)] = &[
        ("ProcessHerdr", None),
        ("HerdrCommandExecutor", None),
        ("execute_bounded", None),
        (".execute(", None),
        ("PaneEffects", None),
        ("InputOwner", None),
        ("HERDR_BIN_PATH", Some("observer_guard.rs")),
        ("\"herdr\"", Some("observer_guard.rs")),
        ("TIOCSTI", None),
        ("posix_spawn", None),
        ("libc::exec", None),
        ("libc::fork", None),
        ("libc::system", None),
        (".exec()", None),
    ];

    /// Every process dashboard code starts, by file and program. Only the
    /// observer guard starts Herdr.
    const DASHBOARD_SPAWNS: &[(&str, &str)] = &[
        ("dashboard.rs", "Command::new(opener)"),
        (
            "dashboard/channel_service.rs",
            "Command::new(std::env::current_exe().unwrap())",
        ),
        (
            "dashboard/channel_service.rs",
            "Command::new(std::env::current_exe().unwrap())",
        ),
        (
            "dashboard/http.rs",
            "Command::new(crate::runtime_executable()?)",
        ),
        (
            "dashboard/live_peek.rs",
            "Command::new(std::env::current_exe()?)",
        ),
        (
            "dashboard/observer_guard.rs",
            r#"Command::new(env::var_os("HERDR_BIN_PATH").unwrap_or_else(||"herdr"))"#,
        ),
        ("dashboard/server_files.rs", "Command::new(executable)"),
        ("dashboard/server_files.rs", "Command::new(executable)"),
        ("dashboard/tailscale.rs", "Command::new(self.0)"),
        ("dashboard/tailscale.rs", "Command::new(&executable)"),
    ];

    /// Every line of dashboard code that names `Command` other than in a
    /// listed `Command::new` call, so an alias, a type path, or a second
    /// import fails the check whatever its spelling.
    const DASHBOARD_COMMAND_NAMES: &[(&str, &str)] = &[
        ("dashboard.rs", "process::{Command, ExitCode, Stdio},"),
        ("dashboard/http.rs", "process::{Command, Stdio},"),
        (
            "dashboard/live_peek.rs",
            "process::{Child, ChildStdin, ChildStdout, Command, Stdio},",
        ),
        (
            "dashboard/observer_guard.rs",
            "process::{Command, ExitCode, Stdio},",
        ),
        (
            "dashboard/server_files.rs",
            "process::{Command, ExitStatus, Stdio},",
        ),
        (
            "dashboard/skills.rs",
            r#""---\ndescription: Command duplicate\n---\n","#,
        ),
        (
            "dashboard/snapshot.rs",
            "/// Command and exit code only; terminal output never leaves the store.",
        ),
        ("dashboard/tailscale.rs", "process::{Command, Stdio},"),
    ];

    /// Each place `source` names the identifier `Command`, by line, other
    /// than as `Command::new`.
    fn command_names(source: &str) -> Vec<&str> {
        let identifier = |c: char| c.is_alphanumeric() || c == '_';
        source
            .lines()
            .flat_map(|line| {
                line.match_indices("Command")
                    .filter(move |&(at, _)| {
                        let after = &line[at + "Command".len()..];
                        !line[..at].ends_with(identifier)
                            && !after.starts_with(identifier)
                            && !after.starts_with("::new")
                    })
                    .map(move |_| line.trim())
            })
            .collect()
    }

    /// The only arguments the observer guard gives Herdr.
    const OBSERVER_ARGUMENTS: &[&str] = &[
        r#".args(["terminal","session","observe",terminal])"#,
        r#".arg("--rows")"#,
        ".arg(rows)",
        r#".arg("--cols")"#,
        ".arg(cols)",
    ];

    /// The call at the start of `text`, with its argument list when it has
    /// one.
    fn call_at<'a>(text: &'a str, call: &str) -> &'a str {
        let mut depth = 0;
        let name = call.trim_end_matches('(').len();
        for (at, character) in text.char_indices().skip(name) {
            match character {
                '(' => depth += 1,
                ')' if depth == 1 => return &text[..=at],
                ')' => depth -= 1,
                _ if depth == 0 => return &text[..at],
                _ => {}
            }
        }
        text
    }

    fn calls<'a>(text: &'a str, call: &str) -> Vec<&'a str> {
        text.match_indices(call)
            .map(|(at, _)| call_at(&text[at..], call))
            .collect()
    }

    /// Dashboard code under `src` that could type into a pane around
    /// `DashboardPane` and `interrupt_if_current`.
    fn dashboard_bypasses(src: &Path) -> Vec<String> {
        let mut files = vec![src.join("dashboard.rs")];
        rust_sources(&src.join("dashboard"), &mut files);
        files.retain(|file| file.exists());
        files.sort();
        let mut allowed = DASHBOARD_SPAWNS.to_vec();
        let mut named = DASHBOARD_COMMAND_NAMES.to_vec();
        let mut raw = Vec::new();
        for file in &files {
            let name = file.strip_prefix(src).unwrap().to_str().unwrap();
            let source = fs::read_to_string(file).unwrap();
            for line in command_names(&source) {
                match named.iter().position(|&entry| entry == (name, line)) {
                    Some(index) => {
                        named.swap_remove(index);
                    }
                    None => raw.push(format!("{name} names Command in {line}")),
                }
            }
            let text = compact(&source);
            for (token, owner) in DASHBOARD_RAW {
                if text.contains(token) && owner.is_none_or(|owner| !file.ends_with(owner)) {
                    raw.push(format!("{name} names {token}"));
                }
            }
            for verb in input_verbs(&text) {
                raw.push(format!("{name} names {verb}"));
            }
            for spawn in calls(&text, "Command::new") {
                match allowed.iter().position(|&entry| entry == (name, spawn)) {
                    Some(index) => {
                        allowed.swap_remove(index);
                    }
                    None => raw.push(format!("{name} starts {spawn}")),
                }
            }
            if name == "dashboard/observer_guard.rs" {
                for argument in calls(&text, ".arg(")
                    .into_iter()
                    .chain(calls(&text, ".args("))
                {
                    if !OBSERVER_ARGUMENTS.contains(&argument) {
                        raw.push(format!("{name} gives Herdr {argument}"));
                    }
                }
            }
        }
        raw
    }

    #[test]
    fn dashboard_types_into_a_pane_only_through_pane_effects() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        assert!(
            src.join("dashboard/live_peek.rs").exists()
                && src.join("dashboard/observer_guard.rs").exists(),
            "the scan must reach live peek and the observer guard"
        );
        let raw = dashboard_bypasses(&src);
        assert!(
            raw.is_empty(),
            "Dashboard code reaches Herdr only through `Front.mux`, a `MuxConnector`, starts Herdr only in the observer guard, and types into a pane only through `DashboardPane::input` or `interrupt_if_current` on that connection:\n{}",
            raw.join("\n")
        );
    }

    #[test]
    fn dashboard_check_catches_each_bypass() {
        let state = State::new();
        fs::create_dir_all(state.0.join("dashboard")).unwrap();
        let observer = r#"let mut command = Command::new(env::var_os("HERDR_BIN_PATH").unwrap_or_else(|| "herdr".into()));
            command.args(["terminal", "session", "observe", terminal]);
            command.arg("--rows").arg(rows.to_string());"#;
        fs::write(state.0.join("dashboard/observer_guard.rs"), observer).unwrap();
        fs::write(
            state.0.join("dashboard.rs"),
            "Command::new(opener).arg(url)",
        )
        .unwrap();
        assert_eq!(dashboard_bypasses(&state.0), Vec::<String>::new());
        let bypass = state.0.join("dashboard/thread_controls.rs");
        for (source, caught) in [
            (
                r#"std::process::Command::new(concat!("her", "dr")).arg("pane").arg("run")"#,
                r#"starts Command::new(concat!("her","dr"))"#,
            ),
            (
                r#"Command::new("herdr").args(["pane", "send-keys", pane, "enter"])"#,
                r#"names "herdr""#,
            ),
            (
                r#"ProcessHerdr::for_binary(bin).execute(&["pane", "send-keys", pane])"#,
                "names ProcessHerdr",
            ),
            (
                "PaneEffects::acquire(state, socket, pane)?.run_terminal(&herdr, &owner, text)",
                "names PaneEffects",
            ),
            (
                "effects.prompt(&herdr, &InputOwner::delivery(ns, attempt, binding, session), request)",
                "names InputOwner",
            ),
            (
                "use std::process::Command as Spawn; Spawn::new(program)",
                "names Command in use std::process::Command as Spawn; Spawn::new(program)",
            ),
            (
                "type Spawn = process::Command; Spawn::new(program)",
                "names Command in type Spawn = process::Command; Spawn::new(program)",
            ),
            (
                "<std::process::Command>::new(program)",
                "names Command in <std::process::Command>::new(program)",
            ),
            (
                r#"Command::new(bin).arg("pane").arg("run")"#,
                r#"names "pane","run""#,
            ),
            (
                r#"let argv = vec![String::from("agent"), String::from("prompt")];"#,
                r#"names "agent","prompt""#,
            ),
            (
                "let spawn = Command::new; spawn(program)",
                "starts Command::new",
            ),
            (
                r#"let argv = vec!["agent".to_owned(), "prompt".to_owned()];"#,
                r#"names "agent","prompt""#,
            ),
        ] {
            fs::write(&bypass, source).unwrap();
            let found = dashboard_bypasses(&state.0);
            assert!(
                found
                    .iter()
                    .any(|line| *line == format!("dashboard/thread_controls.rs {caught}")),
                "{source} was not caught: {found:?}"
            );
        }
        fs::remove_file(&bypass).unwrap();
        fs::write(
            state.0.join("dashboard/observer_guard.rs"),
            observer.replace(
                r#""terminal", "session", "observe""#,
                r#""pane", "send-text""#,
            ),
        )
        .unwrap();
        let found = dashboard_bypasses(&state.0);
        assert!(
            found.contains(
                &r#"dashboard/observer_guard.rs gives Herdr .args(["pane","send-text",terminal])"#
                    .to_owned()
            ),
            "{found:?}"
        );
        fs::write(
            state.0.join("dashboard.rs"),
            "Command::new(opener).arg(url); Command::new(opener).arg(url)",
        )
        .unwrap();
        assert!(
            dashboard_bypasses(&state.0)
                .contains(&"dashboard.rs starts Command::new(opener)".to_owned()),
            "a second spawn of an allowed program was not caught"
        );
    }
}
