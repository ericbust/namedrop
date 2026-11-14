//! The relay: a mention to an agent on another machine goes from this
//! Namedrop to the Namedrop beside that machine's Rex server, which delivers
//! it there. Replies come back by this side's pull.

mod accept;
mod capture;
pub(crate) mod receive;
mod rex;
mod store;
mod wire;
mod worker;

use crate::{MessageId, StateNamespace, ThreadId};
use std::path::Path;
use store::{Outbound, RelayStore};

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

/// This machine's name as a host label, for display: the host name up to
/// its first dot, with every byte the label rule refuses dropped.
pub(crate) fn machine_name() -> String {
    let mut buffer = [0_u8; 256];
    // SAFETY: `gethostname` writes at most the buffer's length into it.
    let named = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } == 0;
    let name: String = if named {
        let end = buffer
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(buffer.len());
        String::from_utf8_lossy(&buffer[..end])
            .split('.')
            .next()
            .unwrap_or_default()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
            .take(64)
            .collect()
    } else {
        String::new()
    };
    if name.is_empty() {
        "host".to_owned()
    } else {
        name
    }
}

/// `namedrop relay accept`: one `namedrop.relay` call B's bridge admitted
/// for the server `host_id` behind `endpoint`, read from `input`. Its
/// answer is the call's result.
pub fn accept(
    state: &Path,
    endpoint: &crate::mux::Endpoint,
    host_id: &str,
    input: &[u8],
) -> String {
    serde_json::to_string(&accept::accept(
        state,
        endpoint,
        host_id,
        input,
        &mut |space| {
            wake(state, endpoint, space);
        },
    ))
    .unwrap_or_else(|error| format!("{{\"error\":\"{error}\"}}"))
}

/// `namedrop relay worker --host <label>`: the relay's worker for `label`,
/// holding `<state>/relay/<sha256(endpoint, label)>.lock`, so a second one
/// exits at once.
pub fn run_worker(state: &Path, endpoint: &crate::mux::Endpoint, label: &str) {
    hold(state, endpoint, label, &mut || {
        worker::work(&mut worker::Run {
            state,
            endpoint,
            label,
            pause: &mut |within| {
                wait_for_new(
                    state,
                    endpoint,
                    label,
                    within,
                    std::time::Duration::from_secs(1),
                );
                true
            },
            wake: &mut |space| wake(state, endpoint, space),
        });
    });
}

/// Runs `work` while holding the worker's lock for `label`, and again while
/// an item is queued once it lets go. A send whose start found the lock
/// held just before `work` ended started no worker, so the last holder
/// looks for its item.
fn hold(state: &Path, endpoint: &crate::mux::Endpoint, label: &str, work: &mut dyn FnMut()) {
    if !valid_label(label) {
        return;
    }
    let Ok(lock) = worker_lock(state, endpoint, label) else {
        return;
    };
    while lock.try_lock().is_ok() {
        work();
        let _ = lock.unlock();
        let queued = RelayStore::read(state)
            .and_then(|store| store.queued(label, &endpoint.key))
            .is_ok_and(|queued| !queued.is_empty());
        if !queued {
            return;
        }
    }
}

/// Waits `within`, or less once `endpoint` queued an item for `label` that
/// was not queued when the wait began, looking every `step`. So a send while
/// the worker waits to collect again is delivered at once.
fn wait_for_new(
    state: &Path,
    endpoint: &crate::mux::Endpoint,
    label: &str,
    within: std::time::Duration,
    step: std::time::Duration,
) {
    let queued = || {
        RelayStore::read(state)
            .and_then(|store| store.queued(label, &endpoint.key))
            .map_or(0, |queued| queued.len())
    };
    let before = queued();
    let until = std::time::Instant::now() + within;
    loop {
        let left = until.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            return;
        }
        std::thread::sleep(left.min(step));
        if queued() > before {
            return;
        }
    }
}

fn worker_lock(
    state: &Path,
    endpoint: &crate::mux::Endpoint,
    label: &str,
) -> std::io::Result<std::fs::File> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::OpenOptionsExt;
    let directory = state.join("relay");
    std::fs::create_dir_all(&directory)?;
    let scope = crate::lower_hex(&Sha256::digest(format!("{}\0{label}", endpoint.key)));
    std::fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(directory.join(format!("{scope}.lock")))
}

/// Queues a mailbox wake for `space` and starts its worker, which is reaped
/// when it exits, so a long-lived starter keeps no zombie.
pub(crate) fn wake(state: &Path, endpoint: &crate::mux::Endpoint, space: &crate::SpaceId) {
    use crate::status_queue::{self, Marker};
    if status_queue::enqueue(state, endpoint, space, &Marker::Wake).is_ok()
        && let Ok(mut worker) = status_queue::spawn_worker(state, endpoint, space)
    {
        std::thread::spawn(move || worker.wait());
    }
}

/// Starts the relay's worker for `label` unless one runs. It is reaped when
/// it exits.
pub(crate) fn start_worker(state: &Path, endpoint: &crate::mux::Endpoint, label: &str) {
    #[cfg(test)]
    {
        let _ = (state, endpoint);
        tests_support::STARTED.with(|started| started.borrow_mut().push(label.to_owned()));
    }
    #[cfg(not(test))]
    {
        let Ok(executable) = crate::runtime_executable() else {
            return;
        };
        if let Ok(mut worker) = worker_command(&executable, state, endpoint, label).spawn() {
            std::thread::spawn(move || worker.wait());
        }
    }
}

/// `namedrop relay worker` for `label`, which reads its endpoint from the
/// flag.
fn worker_command(
    executable: &Path,
    state: &Path,
    endpoint: &crate::mux::Endpoint,
    label: &str,
) -> std::process::Command {
    use crate::{
        daemon::{self, Scope, Starter},
        locator::{Locator, Name},
    };
    let mut command = daemon::command(executable, Scope::Endpoint(endpoint), Starter::Caller);
    command.args([
        "relay",
        "worker",
        "--host",
        label,
        "--endpoint",
        &endpoint.key,
    ]);
    Locator::StateDir.pass(&mut command, Name::of(endpoint.kind), state);
    command
}

/// Starts the worker of each label of `endpoint` with an item queued or a
/// thread open, as the bridge does when it connects, so a conversation goes
/// on collecting after a restart.
pub(crate) fn start_workers(state: &Path, endpoint: &crate::mux::Endpoint) {
    let now = now_ms();
    if let Ok(store) = RelayStore::open(state, now)
        && let Ok(labels) = store.active_labels(&endpoint.key, now)
    {
        for label in labels {
            start_worker(state, endpoint, &label);
        }
    }
}

/// Runs the relay's capture after a drain in a Rex home: each new message
/// to a relay author becomes B's kept reply or A's queued answer, and each
/// label with a new item gets its worker.
pub(crate) fn capture_after_drain(state: &Path, session_socket_id: &str) {
    let endpoint = crate::mux::Endpoint::parse(session_socket_id);
    if endpoint.kind != crate::mux::MuxKind::Rex {
        return;
    }
    let _ = capture::capture(state, &endpoint, &mut |label| {
        start_worker(state, &endpoint, label);
    });
}

/// Each remote sender whose replies expired here before it collected
/// them, with how many, for Doctor. Nothing when the relay never ran.
pub(crate) fn expired_replies(state: &Path) -> Vec<(String, i64)> {
    if !state.join("relay.sqlite3").is_file() {
        return Vec::new();
    }
    RelayStore::open(state, now_ms())
        .and_then(|store| store.expired_replies())
        .unwrap_or_default()
}

/// The hosts a `send` may name a remote recipient on.
pub(crate) trait RemoteHosts {
    /// The listed labels, or `None` when the send takes no remote recipient,
    /// so that `@a@b` reads exactly as it does outside Rex.
    fn listed(&self) -> Option<&[String]>;

    /// Whether `@alias@host` names a remote agent that takes part in the
    /// thread a reply answers, which the reply addresses as a participant.
    fn participant(&self, _alias: &str, _host: &str) -> bool {
        false
    }
}

/// The remote agents of the thread a Rex reply answers, by display alias.
pub(crate) struct Participants(pub(crate) Vec<String>);

impl RemoteHosts for Participants {
    fn listed(&self) -> Option<&[String]> {
        None
    }

    fn participant(&self, alias: &str, host: &str) -> bool {
        self.0.iter().any(|known| {
            known
                .split_once('@')
                .is_some_and(|known| known == (alias, host))
        })
    }
}

/// Every reader of a message but a Rex home's `send`.
pub(crate) struct NoRemoteHosts;

impl RemoteHosts for NoRemoteHosts {
    fn listed(&self) -> Option<&[String]> {
        None
    }
}

/// The labels `rex hosts list --json` prints, byte for byte.
#[derive(Debug, Default)]
pub(crate) struct Hosts(Vec<String>);

impl RemoteHosts for Hosts {
    fn listed(&self) -> Option<&[String]> {
        Some(&self.0)
    }
}

impl Hosts {
    /// The host list of this machine's `rex`, as the relay's later calls
    /// read it.
    pub(crate) fn read() -> Result<Self, String> {
        let output = rex::run(&["hosts", "list", "--json"], None, rex::CALL)
            .map_err(|failure| format!("could not read your Rex hosts: {failure}"))?;
        let listed: serde_json::Value = serde_json::from_slice(&output)
            .map_err(|error| format!("could not read your Rex hosts: {error}"))?;
        Ok(Self(
            listed["hosts"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|entry| entry["host"]["label"].as_str().map(str::to_owned))
                .collect(),
        ))
    }
}

/// Whether `label` can name a host in a mention: 1 to 64 ASCII letters,
/// digits, `-`, and `_`. Rex accepts more, but `-S` reads a value with a
/// port as an address, and a mention token ends at whitespace.
pub(crate) fn valid_label(label: &str) -> bool {
    (1..=64).contains(&label.len())
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

/// Whether `name` can name an agent on the other side: an alias of at most
/// 64 bytes, which the other side shows as `name@host`.
pub(crate) fn valid_name(name: &str) -> bool {
    name.len() <= 64 && crate::validate_alias(name).is_ok()
}

/// The refusal for a label that is not exactly a listed, valid one.
pub(crate) fn unlisted(label: &str, listed: &[String]) -> String {
    if listed.is_empty() {
        format!("{label} is not in your Rex hosts (rex hosts list lists none)")
    } else {
        format!(
            "{label} is not in your Rex hosts (rex hosts list): {}",
            listed.join(", ")
        )
    }
}

/// One remote recipient of a send: `@alias@label`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RemoteName {
    pub(crate) alias: String,
    pub(crate) label: String,
}

impl RemoteName {
    /// The paneless author that stands for this agent in the sender's space.
    pub(crate) fn author(&self) -> crate::store::RelayAuthor {
        crate::store::RelayAuthor::new(
            crate::store::RelayOrigin::Label(&self.label),
            &self.alias,
            format!("{}@{}", self.alias, self.label),
        )
    }
}

/// Refuses a send whose `deliver` request for any remote recipient would
/// pass what the `rex` CLI sends, before anything is stored.
pub(crate) fn check_size(sender: &str, remote: &[RemoteName], text: &str) -> Result<(), String> {
    for name in remote {
        let size = wire::deliver_size(sender, &name.alias, text);
        if size > wire::REQUEST_LIMIT {
            return Err(format!(
                "the message to @{}@{} is too large to relay: its request is {size} bytes, \
                 and the limit is {}",
                name.alias,
                name.label,
                wire::REQUEST_LIMIT
            ));
        }
    }
    Ok(())
}

/// What a committed send hands the relay: the A message every remote
/// recipient's item carries.
pub(crate) struct Sent<'a> {
    pub namespace: &'a StateNamespace,
    pub sender: &'a str,
    pub message: &'a MessageId,
    pub thread: &'a ThreadId,
    pub text: &'a str,
    pub now_ms: i64,
}

/// Queues one `outbound` item per remote recipient of `sent`, and starts
/// each label's worker. A replayed send queues the same keys, which are
/// already there.
pub(crate) fn queue(state: &Path, sent: &Sent<'_>, remote: &[RemoteName]) -> Result<(), String> {
    let store = RelayStore::open(state, sent.now_ms).map_err(|error| error.to_string())?;
    for name in remote {
        store
            .queue(&Outbound {
                label: &name.label,
                message_id: sent.message.as_str(),
                recipient: &name.alias,
                endpoint: &sent.namespace.session_socket_id,
                space: sent.namespace.space_id.as_str(),
                sender: sent.sender,
                thread: sent.thread.as_str(),
                in_reply_to: None,
                text: sent.text,
                now_ms: sent.now_ms,
            })
            .map_err(|error| error.to_string())?;
    }
    let endpoint = crate::mux::Endpoint::parse(&sent.namespace.session_socket_id);
    let mut started: Vec<&str> = Vec::new();
    for name in remote {
        if !started.contains(&name.label.as_str()) {
            started.push(&name.label);
            start_worker(state, &endpoint, &name.label);
        }
    }
    Ok(())
}

/// What the send's receipt says about each remote recipient.
pub(crate) fn present(response: &mut serde_json::Value, remote: &[RemoteName]) {
    if remote.is_empty() {
        return;
    }
    response["relayed"] = remote
        .iter()
        .map(|name| {
            serde_json::json!({
                "alias": name.alias,
                "host": name.label,
                "statement": format!(
                    "@{}@{}: queued for the Namedrop on {}, which delivers it there.",
                    name.alias, name.label, name.label
                ),
            })
        })
        .collect();
}

#[cfg(test)]
pub(crate) mod tests_support {
    use std::{
        cell::RefCell,
        fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        sync::atomic::{AtomicU64, Ordering},
    };

    thread_local! {
        static PROGRAM: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
        /// The labels whose worker this thread started.
        pub(crate) static STARTED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    static NEXT: AtomicU64 = AtomicU64::new(0);

    /// The `rex` this thread's relay runs: none outside `with_rex`, so no
    /// test runs the machine's own `rex`.
    pub(crate) fn program() -> PathBuf {
        PROGRAM
            .with(|program| program.borrow().clone())
            .unwrap_or_else(|| PathBuf::from("/nonexistent/namedrop-relay-tests/rex"))
    }

    /// Runs `body` with `program` as this thread's `rex`.
    pub(crate) fn with_program<T>(program: PathBuf, body: impl FnOnce() -> T) -> T {
        PROGRAM.with(|current| *current.borrow_mut() = Some(program));
        let result = body();
        PROGRAM.with(|current| *current.borrow_mut() = None);
        result
    }

    /// A script `rex` in its own directory, whose body is `script`, and a
    /// `calls` file beside it where each run appends its arguments.
    pub(crate) struct Script {
        pub(crate) directory: PathBuf,
    }

    impl Script {
        pub(crate) fn new(script: &str) -> Self {
            let directory = std::env::temp_dir().join(format!(
                "namedrop-relay-rex-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&directory).unwrap();
            let rex = directory.join("rex");
            fs::write(
                &rex,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$*\" >> '{}'\n{script}\n",
                    directory.join("calls").display()
                ),
            )
            .unwrap();
            fs::set_permissions(&rex, fs::Permissions::from_mode(0o755)).unwrap();
            Self { directory }
        }

        pub(crate) fn calls(&self) -> Vec<String> {
            fs::read_to_string(self.directory.join("calls"))
                .map(|calls| calls.lines().map(str::to_owned).collect())
                .unwrap_or_default()
        }

        /// Runs `body` with this script as the thread's `rex`.
        pub(crate) fn run<T>(&self, body: impl FnOnce() -> T) -> T {
            with_program(self.directory.join("rex"), body)
        }
    }

    impl Drop for Script {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.directory);
        }
    }

    /// The relay database of `state`.
    pub(crate) fn store(state: &std::path::Path) -> super::RelayStore {
        super::RelayStore::open(state, super::now_ms()).unwrap()
    }

    /// Queues an item for `label` from a space of `endpoint`.
    pub(crate) fn queue(store: &super::RelayStore, label: &str, endpoint: &str) {
        store
            .queue(&super::Outbound {
                label,
                message_id: "message-1",
                recipient: "bob",
                endpoint,
                space: "session:a",
                sender: "alice",
                thread: "thread-1",
                in_reply_to: None,
                text: "hi",
                now_ms: super::now_ms(),
            })
            .unwrap();
    }

    /// A `rex` whose `hosts list --json` prints `labels` as Rex does.
    pub(crate) fn hosts(labels: &[&str]) -> Script {
        let hosts: Vec<_> = labels
            .iter()
            .map(|label| {
                serde_json::json!({"host": {"id": format!("host:{label}"), "label": label,
                    "endpoints": ["127.0.0.1:1"]}, "from": "home", "held": false})
            })
            .collect();
        let listed = serde_json::json!({
            "home": {"label": "a.s", "endpoint": "unix:///tmp/a.sock", "this_machine": false,
                "reached": true},
            "hosts": hosts,
        });
        Script::new(&format!("cat <<'EOF'\n{listed}\nEOF"))
    }

    /// Runs `body` with a `rex` whose host list is `labels`.
    pub(crate) fn with_hosts<T>(labels: &[&str], body: impl FnOnce() -> T) -> T {
        hosts(labels).run(body)
    }
}

#[cfg(test)]
pub(crate) mod tests;
