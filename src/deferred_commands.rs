//! Durable handoff of bound agent commands when the pane cannot connect to Herdr.
use crate::{
    AgentSessionId, Clock, IdGenerator, LifecycleRuntime, PaneAddressId, ProcessHerdr, ProcessIds,
    StateNamespace, StateStore, StoreError, SystemClock, cli::RuntimeOptions,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    env, fs,
    io::{self, Read, Write},
    os::unix::{fs::OpenOptionsExt, net::UnixStream, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum Operation {
    Send,
    Reply,
    Ack,
    Complete,
    AddressList,
}

impl Operation {
    fn parse(token: &str) -> Option<Self> {
        match token {
            "send" => Some(Self::Send),
            "reply" => Some(Self::Reply),
            "ack" => Some(Self::Ack),
            "complete" => Some(Self::Complete),
            "address.list" => Some(Self::AddressList),
            _ => None,
        }
    }

    fn token(self) -> &'static str {
        match self {
            Self::Send => "send",
            Self::Reply => "reply",
            Self::Ack => "ack",
            Self::Complete => "complete",
            Self::AddressList => "address.list",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct Caller {
    namespace: StateNamespace,
    pane: String,
    address: PaneAddressId,
    session: AgentSessionId,
    registration: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Request {
    operation: Operation,
    input: Value,
    caller: Caller,
    cwd: PathBuf,
    key: String,
    fingerprint: String,
    created_at_ms: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    fingerprint: String,
    completed_at_ms: u64,
    result: Result<Value, String>,
}

fn hash(bytes: &[u8]) -> String {
    crate::lower_hex(&Sha256::digest(bytes))
}

fn directory(state: &Path, socket: &str) -> Result<PathBuf, String> {
    if !state.is_absolute() || !fs::symlink_metadata(state).is_ok_and(|m| m.is_dir()) {
        return Err("deferred state must be an absolute directory, not a symbolic link".into());
    }
    let mut path = state.to_path_buf();
    for component in ["deferred-commands".to_owned(), hash(socket.as_bytes())] {
        path.push(component);
        match fs::create_dir(&path) {
            Ok(()) => fs::File::open(path.parent().expect("spool directory has a parent"))
                .and_then(|parent| parent.sync_all())
                .map_err(|e| e.to_string())?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
        if !fs::symlink_metadata(&path)
            .map_err(|e| e.to_string())?
            .is_dir()
        {
            return Err("deferred spool must be a directory, not a symbolic link".into());
        }
    }
    Ok(path)
}

fn pending_directory(directory: &Path) -> Result<PathBuf, String> {
    let pending = directory.join("pending");
    match fs::create_dir(&pending) {
        Ok(()) => fs::File::open(directory)
            .and_then(|parent| parent.sync_all())
            .map_err(|e| e.to_string())?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.to_string()),
    }
    if !fs::symlink_metadata(&pending)
        .map_err(|e| e.to_string())?
        .is_dir()
    {
        return Err("deferred pending queue must be a directory, not a symbolic link".into());
    }
    Ok(pending)
}

fn read_file(path: &Path) -> io::Result<Vec<u8>> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("deferred record is not a regular file"));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn stored_caller(state: &Path, namespace: StateNamespace, pane: String) -> Result<Caller, String> {
    StateStore::open(state)
        .map_err(|e| e.to_string())?
        .transaction(|tx| Caller::read(tx, namespace, pane))
        .map_err(|e| e.to_string())
}

impl Caller {
    fn read(
        tx: &crate::StateTransaction<'_>,
        namespace: StateNamespace,
        pane: String,
    ) -> Result<Self, StoreError> {
        let address = tx.routing_author_for_pane(&namespace, &pane)?;
        let session = address.agent_session_id.ok_or_else(|| {
            StoreError::invalid_input("deferred command requires a current Agent Session")
        })?;
        let registration = tx
            .active_integration_registration(&namespace, &address.address_id, &session)?
            .ok_or_else(|| {
                StoreError::invalid_input(
                    "deferred command requires an active exact-session registration",
                )
            })?;
        Ok(Self {
            namespace,
            pane,
            address: address.address_id,
            session,
            registration: registration.registration_id,
        })
    }

    pub(crate) fn check(
        &self,
        tx: &crate::StateTransaction<'_>,
        namespace: &StateNamespace,
    ) -> Result<(), StoreError> {
        if *namespace != self.namespace
            || Self::read(tx, namespace.clone(), self.pane.clone())? != *self
        {
            return Err(StoreError::invalid_input(
                "deferred command registration or Agent Session is stale",
            ));
        }
        Ok(())
    }
}

fn required<'a>(input: &'a Value, field: &str) -> Result<&'a str, String> {
    input[field]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{field} is required"))
}

impl Request {
    fn new(
        operation: Operation,
        input: Value,
        caller: Caller,
        cwd: PathBuf,
    ) -> Result<Self, String> {
        match operation {
            Operation::Send | Operation::Reply => {
                crate::validate_message_input(operation.token(), input.clone())?;
                if input["author_kind"] != "agent_session" {
                    return Err("deferred messages require author_kind agent_session".into());
                }
            }
            Operation::Ack | Operation::Complete => {
                #[derive(Deserialize)]
                #[serde(deny_unknown_fields)]
                struct Attempt {
                    attempt_id: String,
                }
                let attempt: Attempt =
                    serde_json::from_value(input.clone()).map_err(crate::input_schema_error)?;
                if attempt.attempt_id.is_empty() {
                    return Err("attempt_id is required".into());
                }
            }
            Operation::AddressList if input != json!({}) => {
                return Err("deferred address.list is limited to the current workspace".into());
            }
            Operation::AddressList => {}
        }
        let request_id = match operation {
            Operation::Send | Operation::Reply => required(&input, "request_id")?.to_owned(),
            Operation::Ack | Operation::Complete => required(&input, "attempt_id")?.to_owned(),
            Operation::AddressList => ProcessIds::default().next_id("roster"),
        };
        let family = match operation {
            Operation::Send | Operation::Reply => "message",
            other => other.token(),
        };
        let key = hash(
            &serde_json::to_vec(&(&caller.namespace, &caller.session, family, request_id))
                .map_err(|e| e.to_string())?,
        );
        let fingerprint = hash(
            &serde_json::to_vec(&(operation, &input, &caller, &cwd)).map_err(|e| e.to_string())?,
        );
        Ok(Self {
            operation,
            input,
            caller,
            cwd,
            key,
            fingerprint,
            created_at_ms: SystemClock.now_unix_ms(),
        })
    }

    fn validate(&self) -> Result<(), String> {
        if !self.cwd.is_absolute()
            || self.key.len() != 64
            || !self.key.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("invalid deferred command identity".into());
        }
        let rebuilt = Self::new(
            self.operation,
            self.input.clone(),
            self.caller.clone(),
            self.cwd.clone(),
        )?;
        if rebuilt.fingerprint != self.fingerprint
            || (self.operation != Operation::AddressList && rebuilt.key != self.key)
        {
            return Err("deferred command content changed".into());
        }
        Ok(())
    }
}

fn publish(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let parent = path.parent().ok_or("missing spool directory")?;
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let temporary = parent.join(format!(".{}", ProcessIds::default().next_id("publish")));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(&serde_json::to_vec(value).map_err(io::Error::other)?)?;
        file.sync_all()?;
        fs::hard_link(&temporary, path)?;
        fs::File::open(parent)?.sync_all()
    })();
    let _ = fs::remove_file(temporary);
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

fn enqueue(state: &Path, request: &Request) -> Result<PathBuf, String> {
    let directory = directory(state, &request.caller.namespace.session_socket_id)?;
    let path = directory.join(format!("{}.request.json", request.key));
    // The marker precedes the request so a published request always has a wake.
    // An interrupted producer leaves an inert marker until its request arrives.
    publish(
        &pending_directory(&directory)?.join(format!("{}.json", request.key)),
        &json!({}),
    )?;
    publish(&path, request)?;
    let existing: Request = serde_json::from_slice(&read_file(&path).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    if existing.fingerprint != request.fingerprint {
        return Err("request id was already queued with different command content".into());
    }
    Ok(directory.join(format!("{}.receipt.json", request.key)))
}

fn read_receipt(path: &Path, fingerprint: &str) -> Result<Option<Result<Value, String>>, String> {
    let bytes = match read_file(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let receipt: Receipt = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    if receipt.fingerprint != fingerprint {
        return Err("deferred receipt content conflict".into());
    }
    Ok(Some(receipt.result))
}

fn permission_denied(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EPERM | libc::EACCES))
}

/// Runs before runtime discovery or any mutating handler. A late handler error
/// must never enter this fallback because the command may already have committed.
pub fn try_defer(
    operation: &str,
    input: &Value,
    options: &RuntimeOptions,
) -> Option<Result<Value, String>> {
    let operation = Operation::parse(operation)?;
    let socket = options
        .herdr_socket
        .clone()
        .or_else(|| env::var("HERDR_SOCKET_PATH").ok())?;
    match UnixStream::connect(&socket) {
        Err(error) if permission_denied(&error) => {}
        _ => return None,
    }
    Some((|| {
        let state = options
            .plugin_state
            .clone()
            .or_else(|| env::var_os("HERDR_PLUGIN_STATE_DIR").map(PathBuf::from))
            .ok_or("plugin state is required for deferred commands")?;
        let workspace = options
            .herdr_workspace
            .clone()
            .or_else(|| env::var("HERDR_WORKSPACE_ID").ok())
            .ok_or("workspace is required for deferred commands")?;
        let pane = options
            .herdr_pane
            .clone()
            .or_else(|| env::var("HERDR_PANE_ID").ok())
            .ok_or("pane is required for deferred commands")?;
        let caller = stored_caller(&state, StateNamespace::for_space(socket, workspace), pane)?;
        let request = Request::new(
            operation,
            input.clone(),
            caller,
            env::current_dir().map_err(|e| e.to_string())?,
        )?;
        let receipt = enqueue(&state, &request)?;
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = read_receipt(&receipt, &request.fingerprint)? {
                return result;
            }
            if Instant::now() >= deadline {
                return Ok(
                    json!({"state":"pending", "operation_id":request.key, "committed":null,
                    "receipt":receipt, "guidance":"The outside Namedrop worker has not returned a result. Retry the exact command to recover its durable result."}),
                );
            }
            thread::sleep(Duration::from_millis(25));
        }
    })())
}

fn launcher(state: &Path) -> Result<PathBuf, String> {
    let launcher = state.join("runtime/namedrop");
    if launcher.is_file() {
        Ok(launcher)
    } else {
        env::current_exe().map_err(|e| e.to_string())
    }
}

fn reexec_if_updated(state: &Path) -> Result<(), String> {
    if state.join("runtime/namedrop").is_file()
        && fs::canonicalize(state.join("runtime/current/namedrop")).ok()
            != env::current_exe().and_then(fs::canonicalize).ok()
    {
        return Err(Command::new(launcher(state)?)
            .args(env::args_os().skip(1))
            .exec()
            .to_string());
    }
    Ok(())
}

/// Only a caller that can already connect to Herdr can ensure an outside owner.
pub fn ensure_worker(state: &Path, socket: &str) -> Result<(), String> {
    UnixStream::connect(socket).map_err(|e| e.to_string())?;
    let directory = directory(state, socket)?;
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let lock = worker_lock(&directory)?;
    if lock.try_lock().is_err() {
        return Ok(());
    }
    drop(lock);
    Command::new(launcher(state)?)
        .args([
            "--deferred-worker",
            state.to_str().ok_or("state path is not UTF-8")?,
            socket,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn worker_lock(directory: &Path) -> Result<fs::File, String> {
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join("worker.lock"))
        .map_err(|e| e.to_string())?;
    if !lock.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("deferred worker lock is not a regular file".into());
    }
    Ok(lock)
}

pub fn run_worker(state: &Path, socket: &str) -> Result<(), String> {
    UnixStream::connect(socket).map_err(|e| e.to_string())?;
    reexec_if_updated(state)?;
    let directory = directory(state, socket)?;
    fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
    let lock = worker_lock(&directory)?;
    if lock.try_lock().is_err() {
        return Ok(());
    }
    let marker = directory.join(format!(".{}", ProcessIds::default().next_id("worker")));
    publish(
        &marker,
        &json!({"pid":std::process::id(),"started_at_ms":SystemClock.now_unix_ms()}),
    )?;
    fs::rename(marker, directory.join("worker.json")).map_err(|e| e.to_string())?;
    let mut last_probe = Instant::now();
    loop {
        reexec_if_updated(state)?;
        if last_probe.elapsed() >= Duration::from_secs(5) {
            if UnixStream::connect(socket).is_err() {
                return Ok(());
            }
            let active = StateStore::open_read_only(state)
                .map_err(|e| e.to_string())?
                .map(|mut store| store.transaction(|tx| tx.deferred_worker_active(socket)))
                .transpose()
                .map_err(|e| e.to_string())?
                .unwrap_or(false);
            if !active {
                return Ok(());
            }
            last_probe = Instant::now();
        }
        let pending_directory = pending_directory(&directory)?;
        let mut pending = fs::read_dir(&pending_directory)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .filter_map(|entry| {
                let name = entry.file_name();
                let key = name.to_str()?.strip_suffix(".json")?;
                (key.len() == 64
                    && key.bytes().all(|b| b.is_ascii_hexdigit())
                    && directory.join(format!("{key}.request.json")).exists())
                .then(|| key.to_owned())
            })
            .collect::<Vec<_>>();
        pending.sort();
        for key in pending {
            let request: Result<Request, _> =
                read_file(&directory.join(format!("{key}.request.json")))
                    .and_then(|bytes| serde_json::from_slice(&bytes).map_err(io::Error::other));
            if let Ok(request) = request
                && read_receipt(
                    &directory.join(format!("{key}.receipt.json")),
                    &request.fingerprint,
                )?
                .is_some()
            {
                fs::remove_file(pending_directory.join(format!("{key}.json")))
                    .map_err(|e| e.to_string())?;
                fs::File::open(&pending_directory)
                    .and_then(|file| file.sync_all())
                    .map_err(|e| e.to_string())?;
                continue;
            }
            let mut child = Command::new(launcher(state)?)
                .arg("--deferred-execute")
                .arg(state)
                .arg(socket)
                .arg(&key)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0)
                .spawn()
                .map_err(|e| e.to_string())?;
            let deadline = Instant::now() + Duration::from_secs(60);
            loop {
                if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                    break;
                }
                if Instant::now() >= deadline {
                    crate::runtime::stop_process_group(&mut child);
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
        }
        thread::sleep(Duration::from_millis(100));
    }
}

pub fn execute_queued(
    state: &Path,
    socket: &str,
    key: &str,
    completed: impl FnOnce(&Value, &RuntimeOptions),
) -> Result<(), String> {
    if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid deferred operation id".into());
    }
    let _guard = crate::runtime_command_guard(state)?;
    reexec_if_updated(state)?;
    let directory = directory(state, socket)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(directory.join(format!("{key}.lock")))
        .map_err(|e| e.to_string())?;
    if !lock.metadata().map_err(|e| e.to_string())?.is_file() {
        return Err("deferred command lock is not a regular file".into());
    }
    lock.lock().map_err(|e| e.to_string())?;
    let request: Request = serde_json::from_slice(
        &read_file(&directory.join(format!("{key}.request.json"))).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    request.validate()?;
    if request.key != key || request.caller.namespace.session_socket_id != socket {
        return Err("deferred command scope mismatch".into());
    }
    let receipt = directory.join(format!("{key}.receipt.json"));
    if read_receipt(&receipt, &request.fingerprint)?.is_some() {
        return Ok(());
    }
    let result = execute_request(state, &request);
    if let Ok(value) = &result {
        completed(
            value,
            &RuntimeOptions {
                plugin_state: Some(state.to_path_buf()),
                herdr_socket: Some(socket.to_owned()),
                herdr_workspace: Some(request.caller.namespace.space_id.as_str().to_owned()),
                herdr_pane: Some(request.caller.pane.clone()),
            },
        );
    }
    publish(
        &receipt,
        &Receipt {
            fingerprint: request.fingerprint,
            completed_at_ms: SystemClock.now_unix_ms(),
            result,
        },
    )
}

fn execute_request(state: &Path, request: &Request) -> Result<Value, String> {
    let herdr = ProcessHerdr::default()
        .with_socket(Some(request.caller.namespace.session_socket_id.clone()));
    let runtime = LifecycleRuntime {
        state_directory: state,
        home_directory: None,
        agent_config: crate::AgentConfigDirectories::default(),
        cwd: &request.cwd,
        workspace_id: Some(request.caller.namespace.space_id.as_str()),
        current_pane_id: Some(&request.caller.pane),
        session_socket_id: &request.caller.namespace.session_socket_id,
        herdr: &herdr,
        clock: &SystemClock,
        ids: &ProcessIds::default(),
    };
    if let Some(result) = recover_committed(request, &runtime)? {
        return Ok(result);
    }
    validate_live_caller(state, &request.caller, &herdr)?;
    match request.operation {
        Operation::Send | Operation::Reply => {
            crate::message::execute_message_operation_with_caller(
                request.operation.token(),
                request.input.clone(),
                &runtime,
                Some(&request.caller),
            )
        }
        Operation::Ack | Operation::Complete => crate::integration::execute_attempt_operation(
            request.operation.token(),
            request.input.clone(),
            &runtime,
            Some(&request.caller),
        ),
        Operation::AddressList => {
            crate::execute_lifecycle_operation("address.list", request.input.clone(), &runtime)
        }
    }
}

fn validate_live_caller<E: crate::HerdrCommandExecutor>(
    state: &Path,
    caller: &Caller,
    herdr: &E,
) -> Result<(), String> {
    if stored_caller(state, caller.namespace.clone(), caller.pane.clone())? != *caller {
        return Err("deferred command registration or Agent Session is stale".into());
    }
    let discovered =
        crate::discover_worktree_for_workspace(herdr, caller.namespace.space_id.as_str())?;
    if discovered
        .panes
        .iter()
        .find(|pane| pane.pane_id == caller.pane)
        .and_then(|pane| pane.agent_session_id.as_ref())
        != Some(&caller.session)
    {
        return Err("deferred command does not match the live native Agent Session".into());
    }
    Ok(())
}

fn recover_committed<E: crate::HerdrCommandExecutor, C: Clock, I: IdGenerator>(
    request: &Request,
    runtime: &LifecycleRuntime<'_, E, C, I>,
) -> Result<Option<Value>, String> {
    match request.operation {
        Operation::Send | Operation::Reply => crate::message::recover_deferred_message(
            request.operation.token(),
            request.input.clone(),
            &request.caller.namespace,
            &request.caller.address,
            &request.caller.session,
            runtime,
        ),
        Operation::Ack | Operation::Complete => {
            let completed = StateStore::open(runtime.state_directory)
                .map_err(|e| e.to_string())?
                .transaction(|tx| {
                    tx.deferred_attempt_committed(
                        &request.caller.namespace,
                        required(&request.input, "attempt_id")
                            .map_err(StoreError::invalid_input)?,
                        &request.caller.session,
                        request.operation == Operation::Complete,
                    )
                })
                .map_err(|e| e.to_string())?;
            if !completed {
                return Ok(None);
            }
            let mut result = json!({"attempt_id":request.input["attempt_id"]});
            if request.operation == Operation::Complete {
                result["disposition"] = json!("no_reply");
                result["drain"] = crate::drain_receipt(runtime);
            } else {
                result["state"] = json!("acknowledged");
            }
            Ok(Some(result))
        }
        Operation::AddressList => Ok(None),
    }
}
