//! Durable handoff of bound agent commands when the pane cannot connect to Herdr.
use crate::{
    AgentSessionId, Clock, IdGenerator, LifecycleRuntime, PaneAddressId, ProcessIds,
    StateNamespace, StateStore, StoreError, SystemClock,
    cli::RuntimeOptions,
    locator::Locator,
    mux::{Budget, Endpoint},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    env, fs, io,
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
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
    DeliveryInbox,
    ReminderSet,
    ReminderList,
    ReminderCancel,
}

impl Operation {
    fn parse(token: &str) -> Option<Self> {
        match token {
            "reminder.set" => Some(Self::ReminderSet),
            "reminder.list" => Some(Self::ReminderList),
            "reminder.cancel" => Some(Self::ReminderCancel),
            "send" => Some(Self::Send),
            "reply" => Some(Self::Reply),
            "ack" => Some(Self::Ack),
            "complete" => Some(Self::Complete),
            "address.list" => Some(Self::AddressList),
            "delivery.inbox" => Some(Self::DeliveryInbox),
            _ => None,
        }
    }

    fn token(self) -> &'static str {
        match self {
            Self::ReminderSet => "reminder.set",
            Self::ReminderList => "reminder.list",
            Self::ReminderCancel => "reminder.cancel",
            Self::Send => "send",
            Self::Reply => "reply",
            Self::Ack => "ack",
            Self::Complete => "complete",
            Self::AddressList => "address.list",
            Self::DeliveryInbox => "delivery.inbox",
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
    // Retained in the wire fingerprint for queued-request compatibility.
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

mod spool;
use spool::Dir;

fn directory(state: &Path, socket: &str) -> Result<Dir, String> {
    if !state.is_absolute() {
        return Err("deferred state must be an absolute directory".into());
    }
    Dir::open(state)
        .and_then(|state| state.child("deferred-commands"))
        .and_then(|root| root.child(&hash(socket.as_bytes())))
        .map_err(|e| e.to_string())
}

fn pending_directory(directory: &Dir) -> Result<Dir, String> {
    directory.child("pending").map_err(|e| e.to_string())
}

fn stored_caller(state: &Path, namespace: StateNamespace, pane: String) -> Result<Caller, String> {
    StateStore::open_read_only(state)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Namedrop state is missing; activate this workspace outside the sandbox before queuing commands".to_owned())?
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
        mut input: Value,
        caller: Caller,
        cwd: PathBuf,
    ) -> Result<Self, String> {
        if operation == Operation::ReminderSet && input.get("request_id").is_none() {
            let identity = hash(
                &serde_json::to_vec(&(&caller.namespace, &caller.session, &input))
                    .map_err(|e| e.to_string())?,
            );
            input
                .as_object_mut()
                .ok_or("reminder input must be an object")?
                .insert(
                    "request_id".into(),
                    json!(format!("deferred-reminder-{identity}")),
                );
        }
        match operation {
            Operation::ReminderSet => {
                crate::reminder::validate_input(input.clone())?;
            }
            Operation::ReminderList if input != json!({}) => {
                return Err("reminder.list takes no fields".into());
            }
            Operation::ReminderList => {}
            Operation::ReminderCancel => {
                required(&input, "reminder_id")?;
            }
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
            Operation::DeliveryInbox if input != json!({}) => {
                return Err("delivery.inbox takes no fields".into());
            }
            Operation::DeliveryInbox => {}
        }
        let request_id = match operation {
            Operation::Send | Operation::Reply => required(&input, "request_id")?.to_owned(),
            Operation::Ack | Operation::Complete => required(&input, "attempt_id")?.to_owned(),
            Operation::ReminderSet => required(&input, "request_id")?.to_owned(),
            Operation::ReminderCancel => required(&input, "reminder_id")?.to_owned(),
            Operation::ReminderList => ProcessIds::default().next_id("reminder-list"),
            Operation::AddressList => ProcessIds::default().next_id("roster"),
            Operation::DeliveryInbox => ProcessIds::default().next_id("inbox"),
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
            || (!matches!(
                self.operation,
                Operation::AddressList | Operation::DeliveryInbox | Operation::ReminderList
            ) && rebuilt.key != self.key)
        {
            return Err("deferred command content changed".into());
        }
        Ok(())
    }
}

fn publish(directory: &Dir, name: &str, value: &impl Serialize) -> Result<(), String> {
    let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    directory
        .publish(
            name,
            &bytes,
            &format!(".{}", ProcessIds::default().next_id("publish")),
        )
        .map_err(|e| e.to_string())
}

fn enqueue(state: &Path, request: &Request) -> Result<Dir, String> {
    let directory = directory(state, &request.caller.namespace.session_socket_id)?;
    let name = format!("{}.request.json", request.key);
    // The marker precedes the request so a published request always has a wake.
    // An interrupted producer leaves an inert marker until its request arrives.
    publish(
        &pending_directory(&directory)?,
        &format!("{}.json", request.key),
        &json!({}),
    )?;
    publish(&directory, &name, request)?;
    let existing: Request =
        serde_json::from_slice(&directory.read(&name).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
    if existing.fingerprint != request.fingerprint {
        return Err("request id was already queued with different command content".into());
    }
    Ok(directory)
}

fn read_receipt(
    directory: &Dir,
    name: &str,
    fingerprint: &str,
) -> Result<Option<Result<Value, String>>, String> {
    let bytes = match directory.read(name) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    decode_receipt(&bytes, fingerprint).map(Some)
}

fn decode_receipt(bytes: &[u8], fingerprint: &str) -> Result<Result<Value, String>, String> {
    let receipt: Receipt = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
    if receipt.fingerprint != fingerprint {
        return Err("deferred receipt content conflict".into());
    }
    Ok(receipt.result)
}

fn permission_denied(error: &io::Error) -> bool {
    matches!(error.raw_os_error(), Some(libc::EPERM | libc::EACCES))
}

/// A connection to the socket an endpoint value names, as a liveness probe.
fn probe(endpoint: &str) -> io::Result<UnixStream> {
    Endpoint::parse(endpoint).unix_socket().map_or_else(
        || {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{endpoint} names no Unix socket"),
            ))
        },
        UnixStream::connect,
    )
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
        .endpoint
        .clone()
        .or_else(|| Locator::Endpoint.env())?;
    match probe(&socket) {
        Err(error) if permission_denied(&error) => {}
        _ => return None,
    }
    Some((|| {
        let state = crate::locator::state_directory(
            options.plugin_state.clone(),
            Endpoint::parse(&socket).kind,
            |name| env::var_os(name),
        )
        .ok_or("plugin state is required for deferred commands")?;
        let workspace = options
            .space
            .clone()
            .or_else(|| Locator::Space.env())
            .ok_or("workspace is required for deferred commands")?;
        let pane = options
            .pane
            .clone()
            .or_else(|| Locator::Pane.env())
            .ok_or("pane is required for deferred commands")?;
        let caller = stored_caller(&state, StateNamespace::for_space(socket, workspace), pane)?;
        let request = Request::new(
            operation,
            input.clone(),
            caller,
            env::current_dir().map_err(|e| e.to_string())?,
        )?;
        let directory = enqueue(&state, &request)?;
        let receipt_name = format!("{}.receipt.json", request.key);
        let receipt = directory.path.join(&receipt_name);
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(result) = read_receipt(&directory, &receipt_name, &request.fingerprint)? {
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

/// Whether `session` queued a `reply` or `complete` naming `attempt_id` that
/// has no receipt yet: an explicit settle the sandbox is still carrying.
pub fn settle_queued(state: &Path, socket: &str, session: &str, attempt_id: &str) -> bool {
    let Ok(directory) = directory(state, socket) else {
        return false;
    };
    let Ok(names) = pending_directory(&directory)
        .and_then(|pending| pending.entries().map_err(|e| e.to_string()))
    else {
        return false;
    };
    names
        .iter()
        .filter_map(|name| name.to_str()?.strip_suffix(".json"))
        .any(|key| {
            directory
                .read(&format!("{key}.request.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Request>(&bytes).ok())
                .is_some_and(|request| {
                    matches!(request.operation, Operation::Reply | Operation::Complete)
                        && request.caller.session.as_str() == session
                        && request.input["attempt_id"] == attempt_id
                })
                && !directory
                    .contains(&format!("{key}.receipt.json"))
                    .unwrap_or(false)
        })
}

fn launcher(state: &Path) -> Result<PathBuf, String> {
    let launcher = state.join("runtime/namedrop");
    if launcher.is_file() {
        Ok(launcher)
    } else {
        env::current_exe().map_err(|e| e.to_string())
    }
}

/// Only a caller that can already connect to Herdr can ensure an outside owner.
pub fn ensure_worker(state: &Path, socket: &str) -> Result<(), String> {
    probe(socket).map_err(|e| e.to_string())?;
    let directory = directory(state, socket)?;
    let lock = worker_lock(&directory)?;
    if lock.try_lock().is_err() {
        return Ok(());
    }
    drop(lock);
    crate::daemon::command(
        &launcher(state)?,
        crate::daemon::Scope::Endpoint(&crate::mux::Endpoint::parse(socket)),
        crate::daemon::Starter::Caller,
    )
    .args([
        "--deferred-worker",
        state.to_str().ok_or("state path is not UTF-8")?,
        socket,
    ])
    .spawn()
    .map_err(|e| e.to_string())?;
    Ok(())
}

fn worker_lock(directory: &Dir) -> Result<fs::File, String> {
    directory.lock("worker.lock").map_err(|e| e.to_string())
}

pub fn run_worker(state: &Path, socket: &str) -> Result<(), String> {
    probe(socket).map_err(|e| e.to_string())?;
    crate::installed_runtime::reexec_if_replaced(state)?;
    let directory = directory(state, socket)?;
    let lock = worker_lock(&directory)?;
    if lock.try_lock().is_err() {
        return Ok(());
    }
    let marker = format!(".{}", ProcessIds::default().next_id("worker"));
    publish(
        &directory,
        &marker,
        &json!({"pid":std::process::id(),"started_at_ms":SystemClock.now_unix_ms()}),
    )?;
    directory
        .rename(&marker, "worker.json")
        .map_err(|e| e.to_string())?;
    let pending_directory = pending_directory(&directory)?;
    let mut last_probe = Instant::now();
    loop {
        crate::installed_runtime::reexec_if_replaced(state)?;
        if last_probe.elapsed() >= Duration::from_secs(5) {
            if probe(socket).is_err() {
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
        let mut pending = Vec::new();
        for name in pending_directory.entries().map_err(|e| e.to_string())? {
            let Some(key) = name.to_str().and_then(|name| name.strip_suffix(".json")) else {
                continue;
            };
            if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
                continue;
            }
            if pending_directory
                .check_regular(&format!("{key}.json"))
                .is_err()
            {
                if let Err(error) = pending_directory.remove(&format!("{key}.json")) {
                    eprintln!("Cannot remove invalid deferred marker {key}.json: {error}");
                }
                continue;
            }
            if directory
                .contains(&format!("{key}.request.json"))
                .map_err(|e| e.to_string())?
            {
                pending.push(key.to_owned());
            }
        }
        pending.sort();
        for key in pending {
            let mut child = crate::daemon::command(
                &launcher(state)?,
                crate::daemon::Scope::Endpoint(&crate::mux::Endpoint::parse(socket)),
                crate::daemon::Starter::Caller,
            )
            .arg("--deferred-execute")
            .arg(state)
            .arg(socket)
            .arg(&key)
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
    completed: impl FnOnce(&str, &mut Value, &RuntimeOptions),
) -> Result<(), String> {
    if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid deferred operation id".into());
    }
    let _guard = crate::runtime_command_guard(state, None)?;
    crate::installed_runtime::reexec_if_replaced(state)?;
    let directory = directory(state, socket)?;
    let pending = pending_directory(&directory)?;
    let lock = match directory.lock(&format!("{key}.lock")) {
        Ok(lock) => lock,
        Err(error) => {
            clear_pending(&pending, key)?;
            return Err(error.to_string());
        }
    };
    lock.lock().map_err(|e| e.to_string())?;
    let request_path = format!("{key}.request.json");
    let request = directory
        .read(&request_path)
        .map_err(|e| e.to_string())
        .and_then(|bytes| serde_json::from_slice::<Request>(&bytes).map_err(|e| e.to_string()))
        .and_then(|request| {
            request.validate()?;
            if request.key != key || request.caller.namespace.session_socket_id != socket {
                return Err("deferred command scope mismatch".into());
            }
            Ok(request)
        });
    let request = match request {
        Ok(request) => request,
        Err(error) => {
            let removed = directory.remove(&request_path);
            clear_pending(&pending, key)?;
            removed.map_err(|e| e.to_string())?;
            return Err(error);
        }
    };
    let receipt = format!("{key}.receipt.json");
    match directory.read(&receipt).and_then(|bytes| {
        decode_receipt(&bytes, &request.fingerprint)
            .map(|_| ())
            .map_err(io::Error::other)
    }) {
        Ok(()) => return clear_pending(&pending, key),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => {
            if let Err(error) = directory.remove(&receipt) {
                clear_pending(&pending, key)?;
                return Err(error.to_string());
            }
        }
    }
    let mut result = execute_request(state, &request);
    if let Err(error) = &result {
        warn_refused_settle(state, &request, error);
    }
    if let Ok(value) = &mut result {
        completed(
            request.operation.token(),
            value,
            &RuntimeOptions {
                plugin_state: Some(state.to_path_buf()),
                endpoint: Some(socket.to_owned()),
                space: Some(request.caller.namespace.space_id.as_str().to_owned()),
                pane: Some(request.caller.pane.clone()),
            },
        );
    }
    publish(
        &directory,
        &receipt,
        &Receipt {
            fingerprint: request.fingerprint,
            completed_at_ms: SystemClock.now_unix_ms(),
            result,
        },
    )?;
    clear_pending(&pending, key)
}

/// The agent got `pending` back and may never retry, so a settle refused here
/// has to warn the person. Any agent can queue any attempt's id, so only the
/// attempt's own session raises this.
fn warn_refused_settle(state: &Path, request: &Request, error: &str) {
    let lost = match request.operation {
        Operation::Reply => crate::store::LostSettle::QueuedReply,
        Operation::Complete => crate::store::LostSettle::QueuedNoReply,
        _ => return,
    };
    let Some(attempt) = request.input["attempt_id"].as_str() else {
        return;
    };
    let attempt = crate::DeliveryAttemptId::new(attempt);
    let by = crate::MutationAttribution::new(
        crate::ActorKind::Plugin,
        "namedrop",
        SystemClock.now_unix_ms(),
    );
    let namespace = &request.caller.namespace;
    let _ = StateStore::open(state).and_then(|mut store| {
        store.transaction(|transaction| {
            if !transaction.attempt_belongs_to(namespace, &attempt, &request.caller.session)? {
                return Ok(());
            }
            transaction.flag_failed_settle(namespace, &attempt, lost, error, &by)
        })
    });
}

fn clear_pending(pending: &Dir, key: &str) -> Result<(), String> {
    pending
        .remove(&format!("{key}.json"))
        .map_err(|e| e.to_string())
}

fn execute_request(state: &Path, request: &Request) -> Result<Value, String> {
    let herdr = crate::mux::connect(
        state,
        &Endpoint::parse(&request.caller.namespace.session_socket_id),
        Budget::ADAPTER_DEFAULT,
        Default::default(),
    )
    .map_err(|error| error.to_string())?;
    // From the worker's own environment, so a deferred `address list` reports
    // context sizes as a direct one does.
    let directories = crate::InheritedDirectories::from_environment();
    let runtime = LifecycleRuntime {
        state_directory: state,
        home_directory: directories.home(),
        agent_config: directories.agent_config(),
        cwd: state,
        workspace_id: Some(request.caller.namespace.space_id.as_str()),
        current_pane_id: Some(&request.caller.pane),
        session_socket_id: &request.caller.namespace.session_socket_id,
        mux: &herdr,
        clock: &SystemClock,
        ids: &ProcessIds::default(),
    };
    if let Some(result) = recover_committed(request, &runtime)? {
        return Ok(result);
    }
    let discovered = validate_live_caller(state, &request.caller, &herdr)?;
    let runtime = LifecycleRuntime {
        cwd: if discovered.worktree_attached {
            Path::new(discovered.worktree_id.as_str())
        } else {
            state
        },
        ..runtime
    };
    match request.operation {
        Operation::ReminderSet | Operation::ReminderList | Operation::ReminderCancel => {
            crate::reminder::execute_with_caller(
                request.operation.token(),
                request.input.clone(),
                &runtime,
                Some(&request.caller),
            )
        }
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
        Operation::DeliveryInbox => crate::integration::execute_integration_operation(
            "delivery.inbox",
            request.input.clone(),
            &runtime,
        ),
    }
}

fn validate_live_caller<E: crate::mux::MuxRead>(
    state: &Path,
    caller: &Caller,
    herdr: &E,
) -> Result<crate::DiscoveredWorktree, String> {
    if stored_caller(state, caller.namespace.clone(), caller.pane.clone())? != *caller {
        return Err("deferred command registration or Agent Session is stale".into());
    }
    let discovered =
        crate::discover_worktree_for_workspace(herdr, caller.namespace.space_id.as_str())?;
    if discovered
        .panes
        .iter()
        .find(|pane| pane.pane_id == caller.pane)
        .and_then(|pane| pane.agent_session_id())
        != Some(&caller.session)
    {
        return Err("deferred command does not match the live native Agent Session".into());
    }
    Ok(discovered)
}

fn recover_committed<M: crate::mux::Mux, C: Clock, I: IdGenerator>(
    request: &Request,
    runtime: &LifecycleRuntime<'_, M, C, I>,
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
            } else {
                result["state"] = json!("acknowledged");
            }
            Ok(Some(result))
        }
        Operation::ReminderSet => {
            let fingerprint = serde_json::to_string(&request.input).map_err(|e| e.to_string())?;
            let recovered = StateStore::open(runtime.state_directory)
                .map_err(|e| e.to_string())?
                .transaction(|tx| {
                    tx.replay_reminder(
                        &request.caller.namespace,
                        &request.caller.session,
                        required(&request.input, "request_id")
                            .map_err(StoreError::invalid_input)?,
                        &fingerprint,
                    )
                })
                .map_err(|e| e.to_string())?;
            if recovered.is_some() {
                let _ = crate::reminder::ensure_clock(
                    runtime.state_directory,
                    Some(runtime.session_socket_id),
                    crate::daemon::Starter::Caller,
                );
            }
            Ok(recovered.map(|r| json!(r)))
        }
        Operation::ReminderCancel => StateStore::open(runtime.state_directory)
            .map_err(|e| e.to_string())?
            .transaction(|tx| {
                tx.recover_reminder_cancel(
                    &request.caller.namespace,
                    &request.caller.session,
                    required(&request.input, "reminder_id").map_err(StoreError::invalid_input)?,
                )
            })
            .map_err(|e| e.to_string()),
        Operation::AddressList | Operation::DeliveryInbox | Operation::ReminderList => Ok(None),
    }
}

#[cfg(test)]
mod caller_race_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_probe_connects_to_a_herdr_socket_as_named_and_to_a_rex_socket_after_its_scheme() {
        let directory = env::temp_dir().join(format!("nd-probe-{}", std::process::id()));
        let _ = fs::remove_dir_all(&directory);
        fs::create_dir_all(&directory).unwrap();
        let socket = directory.join("s.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        let path = socket.to_str().unwrap();
        assert!(probe(path).is_ok());
        assert!(probe(&format!("rex:{path}")).is_ok());
        let error = probe("rex:s.sock").unwrap_err();
        assert_eq!(
            (error.kind(), error.to_string()),
            (
                io::ErrorKind::InvalidInput,
                "rex:s.sock names no Unix socket".to_owned()
            )
        );
        fs::remove_dir_all(&directory).unwrap();
    }

    #[test]
    fn a_deferred_command_for_a_rex_caller_connects_and_reads_the_state_as_a_herdr_one_does() {
        let state = env::temp_dir().join(format!("nd-defer-rex-{}", std::process::id()));
        let _ = fs::remove_dir_all(&state);
        fs::create_dir_all(&state).unwrap();
        let request = Request {
            operation: Operation::AddressList,
            input: json!({}),
            caller: Caller {
                namespace: StateNamespace::for_space("rex:/tmp/r.sock", "session:s"),
                pane: "block:b".into(),
                address: PaneAddressId::new("address-1"),
                session: AgentSessionId::new("session-1"),
                registration: "registration-1".into(),
            },
            cwd: state.clone(),
            key: "key-1".into(),
            fingerprint: "fingerprint-1".into(),
            created_at_ms: 0,
        };
        assert_eq!(
            execute_request(&state, &request),
            Err("Namedrop state is missing; activate this workspace outside the sandbox before queuing commands".to_owned())
        );
        assert_eq!(
            fs::read_dir(&state).unwrap().count(),
            0,
            "no store was made"
        );
        fs::remove_dir_all(&state).unwrap();
    }
}
