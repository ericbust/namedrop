//! The one thread that owns the SQLite connection and every subscriber.
//! HTTP threads reach it only through `HubCommand`, so nothing is shared and
//! no read transaction outlives a single snapshot build.
use super::server_files::ServerFiles;
use super::snapshot::{self, DashboardSnapshot, SpaceKey, SpaceSummary};
use crate::{HerdrCommandExecutor, ProcessHerdr, StateNamespace, StateStore, StoreErrorKind};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    fs,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const TICK: Duration = Duration::from_millis(250);
const HERDR_TIMEOUT: Duration = Duration::from_secs(1);
const UNAVAILABLE_LIMIT: u8 = 3;
const RECOVERY_BACKOFF: Duration = Duration::from_secs(2);
const SCHEMA_BUSY_LIMIT: Duration = Duration::from_secs(5);

pub(crate) enum HubCommand {
    Subscribe {
        id: u64,
        subscription: Subscription,
        frames: SyncSender<Frame>,
        notify: Sender<HubCommand>,
    },
    Unsubscribe {
        id: u64,
    },
    Spaces {
        reply: Sender<String>,
    },
    DiscoveryReady,
}

#[derive(Clone)]
pub(crate) enum Frame {
    Snapshot(Arc<str>),
    Spaces(Arc<[SpaceEvent]>),
    Restarting,
}

pub(crate) enum Subscription {
    Space(SpaceKey),
    All,
}

#[derive(Clone)]
pub(crate) enum SpaceEvent {
    Space { key: SpaceKey, frame: Arc<str> },
    Gone { key: SpaceKey },
    Ready { keys: Vec<SpaceKey> },
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct LiveSpace {
    namespace: StateNamespace,
    label: Option<String>,
}

#[derive(Deserialize)]
struct HerdrSnapshot {
    result: HerdrSnapshotResult,
}

#[derive(Deserialize)]
struct HerdrSnapshotResult {
    snapshot: HerdrSpaces,
}

#[derive(Deserialize)]
struct HerdrSpaces {
    workspaces: Vec<HerdrSpace>,
}

#[derive(Deserialize)]
struct HerdrSpace {
    workspace_id: String,
    label: Option<String>,
}

struct Discovery {
    cancel: Arc<AtomicBool>,
    mailbox: Arc<Mutex<DiscoveryMailbox>>,
    thread: Option<JoinHandle<()>>,
}

struct DiscoveryMailbox {
    sockets: BTreeSet<String>,
    latest: HashMap<String, Vec<HerdrSpace>>,
    answered: HashSet<String>,
    failed: HashSet<String>,
    notified: bool,
}

enum DiscoveryAnswer {
    Snapshot(Vec<HerdrSpace>),
    Failed,
    Retired,
}

impl DiscoveryMailbox {
    fn answer(&mut self, socket: String, answer: DiscoveryAnswer) -> bool {
        self.answered.insert(socket.clone());
        match answer {
            DiscoveryAnswer::Snapshot(spaces) => {
                self.failed.remove(&socket);
                self.latest.insert(socket, spaces);
            }
            DiscoveryAnswer::Failed => {
                self.failed.insert(socket);
            }
            DiscoveryAnswer::Retired => {
                self.failed.insert(socket.clone());
                self.latest.insert(socket, Vec::new());
            }
        }
        !std::mem::replace(&mut self.notified, true)
    }
}

#[derive(Clone, Copy, Default)]
enum SocketState {
    #[default]
    Healthy,
    Failing {
        consecutive_unavailable: u8,
    },
    Backoff {
        retry_at: Instant,
    },
}

impl Discovery {
    fn start(
        sockets: BTreeSet<String>,
        states: Arc<Mutex<HashMap<String, SocketState>>>,
        notify: Sender<HubCommand>,
    ) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let mailbox = Arc::new(Mutex::new(DiscoveryMailbox {
            sockets,
            latest: HashMap::new(),
            answered: HashSet::new(),
            failed: HashSet::new(),
            notified: false,
        }));
        let worker_cancel = cancel.clone();
        let worker_mailbox = mailbox.clone();
        let thread =
            thread::spawn(move || Self::run(worker_cancel, worker_mailbox, states, notify));
        Self {
            cancel,
            mailbox,
            thread: Some(thread),
        }
    }

    fn stop(&self) {
        self.cancel.store(true, Ordering::Relaxed);
        if let Some(thread) = &self.thread {
            thread.thread().unpark();
        }
    }

    fn finished(&self) -> bool {
        self.thread.as_ref().is_none_or(JoinHandle::is_finished)
    }

    fn run(
        cancel: Arc<AtomicBool>,
        mailbox: Arc<Mutex<DiscoveryMailbox>>,
        states: Arc<Mutex<HashMap<String, SocketState>>>,
        notify: Sender<HubCommand>,
    ) {
        while !cancel.load(Ordering::Relaxed) {
            let started = Instant::now();
            let sockets = mailbox.lock().unwrap().sockets.clone();
            for socket in sockets {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let state = states
                    .lock()
                    .unwrap()
                    .get(&socket)
                    .copied()
                    .unwrap_or_default();
                if matches!(state, SocketState::Backoff { retry_at } if Instant::now() < retry_at) {
                    continue;
                }
                if matches!(state, SocketState::Backoff { .. }) {
                    states.lock().unwrap().insert(
                        socket.clone(),
                        SocketState::Backoff {
                            retry_at: Instant::now() + RECOVERY_BACKOFF,
                        },
                    );
                }
                let result = ProcessHerdr::default()
                    .with_socket(Some(socket.clone()))
                    .execute_bounded(
                        &["api", "snapshot"],
                        HERDR_TIMEOUT,
                        &cancel,
                        8 * 1024 * 1024,
                    )
                    .and_then(|output| {
                        serde_json::from_str::<HerdrSnapshot>(&output.stdout)
                            .map_err(|error| error.to_string())
                    });
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let answer = match result {
                    Ok(snapshot) => {
                        if !matches!(state, SocketState::Healthy) {
                            eprintln!("dashboard: live workspaces on {socket} recovered");
                        }
                        states
                            .lock()
                            .unwrap()
                            .insert(socket.clone(), SocketState::Healthy);
                        DiscoveryAnswer::Snapshot(snapshot.result.snapshot.workspaces)
                    }
                    Err(error) => {
                        if matches!(state, SocketState::Healthy) {
                            eprintln!("dashboard: live workspaces on {socket} unavailable");
                        }
                        let unavailable = serde_json::from_str::<serde_json::Value>(&error)
                            .ok()
                            .is_some_and(|value| value["error"]["code"] == "server_not_running");
                        let consecutive_unavailable = if unavailable {
                            match state {
                                SocketState::Failing {
                                    consecutive_unavailable,
                                } => consecutive_unavailable + 1,
                                _ => 1,
                            }
                        } else {
                            0
                        };
                        let (next, answer) = if matches!(state, SocketState::Backoff { .. })
                            || consecutive_unavailable >= UNAVAILABLE_LIMIT
                        {
                            (
                                SocketState::Backoff {
                                    retry_at: Instant::now() + RECOVERY_BACKOFF,
                                },
                                if unavailable {
                                    DiscoveryAnswer::Retired
                                } else {
                                    DiscoveryAnswer::Failed
                                },
                            )
                        } else {
                            (
                                SocketState::Failing {
                                    consecutive_unavailable,
                                },
                                DiscoveryAnswer::Failed,
                            )
                        };
                        states.lock().unwrap().insert(socket.clone(), next);
                        answer
                    }
                };
                let wake = mailbox.lock().unwrap().answer(socket, answer);
                if wake {
                    let _ = notify.send(HubCommand::DiscoveryReady);
                }
            }
            thread::park_timeout(TICK.saturating_sub(started.elapsed()));
        }
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) enum Verdict {
    Handoff(&'static str),
    Idle,
    Exit(&'static str),
}

impl Verdict {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Handoff(reason) | Self::Exit(reason) => reason,
            Self::Idle => "idle",
        }
    }
}

struct Subscriber {
    state: ConnectionState,
    frames: SyncSender<Frame>,
    notify: Sender<HubCommand>,
}

enum ConnectionState {
    Space(SpaceKey),
    PendingAll,
    LiveAll { known_keys: HashSet<SpaceKey> },
}

struct Published {
    expires_at_ms: Option<i64>,
    hash: [u8; 32],
    frame: Arc<str>,
}

struct KnownSpace {
    namespace: StateNamespace,
    activated: bool,
}

struct Hub<'a> {
    files: &'a ServerFiles,
    instance: &'a str,
    idle: Duration,
    store: Option<StateStore>,
    data_version: Option<i64>,
    spaces: HashMap<SpaceKey, KnownSpace>,
    subscribers: HashMap<u64, Subscriber>,
    published: HashMap<SpaceKey, Published>,
    live_spaces: HashMap<SpaceKey, LiveSpace>,
    observed_sockets: HashSet<String>,
    failed_sockets: HashSet<String>,
    unpublished_spaces: HashSet<SpaceKey>,
    discovery: Option<Discovery>,
    retry_discovery_drain: bool,
    socket_states: Arc<Mutex<HashMap<String, SocketState>>>,
    metadata_dirty: bool,
    idle_since: Option<Instant>,
    unsupported_schema: bool,
    schema_busy_since: Option<Instant>,
    last_error: Option<String>,
}

pub(crate) fn run(
    files: &ServerFiles,
    instance: &str,
    idle: Duration,
    commands: Receiver<HubCommand>,
) -> Verdict {
    let mut hub = Hub {
        files,
        instance,
        idle,
        store: None,
        data_version: None,
        spaces: HashMap::new(),
        subscribers: HashMap::new(),
        published: HashMap::new(),
        live_spaces: HashMap::new(),
        observed_sockets: HashSet::new(),
        failed_sockets: HashSet::new(),
        unpublished_spaces: HashSet::new(),
        discovery: None,
        retry_discovery_drain: false,
        socket_states: Arc::new(Mutex::new(HashMap::new())),
        metadata_dirty: false,
        idle_since: Some(Instant::now()),
        unsupported_schema: false,
        schema_busy_since: None,
        last_error: None,
    };
    let verdict = loop {
        let wait = if hub.retry_discovery_drain {
            Duration::from_millis(1)
        } else {
            TICK
        };
        match commands.recv_timeout(wait) {
            Ok(command) => hub.handle(command),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break Verdict::Exit("listener stopped"),
        }
        if let Some(verdict) = hub.verdict() {
            break verdict;
        }
        hub.refresh(None);
    };
    if matches!(verdict, Verdict::Handoff(_)) {
        for subscriber in hub.subscribers.values() {
            let _ = subscriber.frames.try_send(Frame::Restarting);
        }
        // Lets connection threads flush `restarting` before the process exits.
        std::thread::sleep(Duration::from_millis(200));
    }
    verdict
}

impl Hub<'_> {
    fn handle(&mut self, command: HubCommand) {
        match command {
            HubCommand::Subscribe {
                id,
                subscription,
                frames,
                notify,
            } => {
                let refreshed = self.refresh(Some(&subscription));
                let (state, initial) = match subscription {
                    Subscription::Space(key) => {
                        let frame = self
                            .published
                            .get(&key)
                            .map(|published| Frame::Snapshot(published.frame.clone()))
                            .or_else(|| self.publish(&key).map(Frame::Snapshot));
                        let Some(frame) = frame else { return };
                        (ConnectionState::Space(key), Some(frame))
                    }
                    Subscription::All => (ConnectionState::PendingAll, None),
                };
                let sent = initial.is_none_or(|frame| frames.try_send(frame).is_ok());
                self.subscribers.insert(
                    id,
                    Subscriber {
                        state,
                        frames,
                        notify,
                    },
                );
                self.idle_since = None;
                if !sent {
                    self.drop_subscriber(id);
                }
                self.sync_discovery();
                if refreshed {
                    self.complete_replays();
                }
            }
            HubCommand::Unsubscribe { id } => self.drop_subscriber(id),
            HubCommand::Spaces { reply } => {
                let _ = reply.send(self.spaces_json());
            }
            HubCommand::DiscoveryReady => {}
        }
    }

    fn drop_subscriber(&mut self, id: u64) {
        if let Some(gone) = self.subscribers.remove(&id) {
            if let ConnectionState::Space(key) = gone.state
                && !self.subscribers.values().any(|s| match &s.state {
                    ConnectionState::Space(other) => other == &key,
                    ConnectionState::PendingAll | ConnectionState::LiveAll { .. } => {
                        self.live_spaces.contains_key(&key)
                    }
                })
            {
                self.published.remove(&key);
            }
            if self.subscribers.is_empty() {
                self.idle_since = Some(Instant::now());
                self.published.clear();
            }
            self.sync_discovery();
        }
    }

    fn verdict(&mut self) -> Option<Verdict> {
        self.verdict_at(Instant::now())
    }

    fn verdict_at(&mut self, now: Instant) -> Option<Verdict> {
        if self.unsupported_schema {
            return Some(Verdict::Exit(
                "this release does not support the state schema",
            ));
        }
        let runtime = self.files.state().join("runtime");
        let wants_handoff = if fs::symlink_metadata(runtime.join("migration.json")).is_ok() {
            Some("an update is migrating state")
        } else if runtime.join("namedrop").exists()
            && fs::canonicalize(runtime.join("current/namedrop")).ok()
                != std::env::current_exe().and_then(fs::canonicalize).ok()
        {
            Some("a newer release is current")
        } else if self
            .store
            .as_ref()
            .is_some_and(|store| match store.schema_version() {
                Ok(version) => {
                    self.schema_busy_since = None;
                    version != crate::store::SCHEMA_VERSION
                }
                Err(error) if error.kind() == StoreErrorKind::Busy => {
                    let since = self.schema_busy_since.get_or_insert_with(|| {
                        eprintln!(
                            "dashboard: state schema read is busy; retrying for up to {} seconds",
                            SCHEMA_BUSY_LIMIT.as_secs()
                        );
                        now
                    });
                    let expired = now.duration_since(*since) >= SCHEMA_BUSY_LIMIT;
                    if expired {
                        eprintln!(
                            "dashboard: state schema read is still busy after {} seconds: {error}",
                            SCHEMA_BUSY_LIMIT.as_secs()
                        );
                    }
                    expired
                }
                Err(_) => true,
            })
        {
            Some("the state schema changed")
        } else {
            None
        };
        if let Some(reason) = wants_handoff {
            return Some(if self.subscribers.is_empty() {
                Verdict::Exit(reason)
            } else {
                Verdict::Handoff(reason)
            });
        }
        if self
            .files
            .info()
            .is_none_or(|info| info.instance != self.instance)
        {
            return Some(Verdict::Exit("server.json was replaced"));
        }
        if self
            .idle_since
            .is_some_and(|since| now.duration_since(since) >= self.idle)
        {
            return Some(Verdict::Idle);
        }
        None
    }

    fn refresh(&mut self, incoming: Option<&Subscription>) -> bool {
        if self.store.is_none() && !self.open_store() {
            return false;
        }
        let Some(store) = &self.store else {
            self.sync_discovery();
            self.complete_replays();
            return true;
        };
        let Ok(version) = store.data_version() else {
            return false;
        };
        let database_changed = self.data_version != Some(version);
        if database_changed && self.reload_spaces().is_none() {
            return false;
        }
        self.sync_discovery();
        let previous = self.live_spaces.clone();
        self.refresh_live_spaces();
        self.unpublished_spaces.extend(
            self.live_spaces
                .keys()
                .filter(|key| !previous.contains_key(*key))
                .cloned(),
        );
        self.unpublished_spaces
            .retain(|key| self.live_spaces.contains_key(key));
        self.metadata_dirty |= previous != self.live_spaces;
        let metadata_changed = std::mem::take(&mut self.metadata_dirty);
        let retention_expired = self.published.values().any(|published| {
            published
                .expires_at_ms
                .is_some_and(|expiry| now_ms() >= expiry)
        });
        if !database_changed && !metadata_changed && !retention_expired && incoming.is_none() {
            self.complete_replays();
            return true;
        }
        let all = self
            .subscribers
            .values()
            .any(|subscriber| !matches!(subscriber.state, ConnectionState::Space(_)))
            || matches!(incoming, Some(Subscription::All));
        let mut keys: Vec<_> = self
            .subscribers
            .values()
            .filter_map(|subscriber| match &subscriber.state {
                ConnectionState::Space(key) => Some(key.clone()),
                _ => None,
            })
            .collect();
        if let Some(Subscription::Space(key)) = incoming {
            keys.push(key.clone());
        }
        if all {
            keys.extend(self.live_spaces.keys().cloned());
        }
        keys.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        keys.dedup();
        let mut changes: Vec<SpaceEvent> = previous
            .keys()
            .filter(|key| !self.live_spaces.contains_key(*key))
            .cloned()
            .map(|key| SpaceEvent::Gone { key })
            .collect();
        let mut failed = false;
        let mut published_keys = HashSet::new();
        for key in keys {
            let before = self.published.get(&key).map(|p| p.hash);
            let Some(frame) = self.publish(&key) else {
                failed = true;
                continue;
            };
            published_keys.insert(key.clone());
            let content_changed = before != self.published.get(&key).map(|p| p.hash);
            let joined = self.unpublished_spaces.contains(&key);
            if !content_changed && !(all && joined) {
                continue;
            }
            let mut stale = Vec::new();
            for (id, subscriber) in &self.subscribers {
                if !content_changed
                    || !matches!(&subscriber.state, ConnectionState::Space(wanted) if wanted == &key)
                {
                    continue;
                }
                match subscriber.frames.try_send(Frame::Snapshot(frame.clone())) {
                    Ok(()) => {}
                    // A full queue means a stalled tab: dropping it forces a
                    // reconnect, whose first frame is the whole truth again.
                    Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) => stale.push(*id),
                }
            }
            for id in stale {
                self.drop_subscriber(id);
            }
            if all && self.live_spaces.contains_key(&key) {
                self.unpublished_spaces.remove(&key);
                changes.push(SpaceEvent::Space { key, frame });
            }
        }
        if all {
            let mut stale = Vec::new();
            for (id, subscriber) in &mut self.subscribers {
                let ConnectionState::LiveAll { known_keys } = &mut subscriber.state else {
                    continue;
                };
                let mut events = Vec::new();
                for change in &changes {
                    match change {
                        SpaceEvent::Space { key, .. }
                            if known_keys.contains(key)
                                || self.live_spaces.get(key).is_some_and(|space| {
                                    !self
                                        .failed_sockets
                                        .contains(&space.namespace.session_socket_id)
                                }) =>
                        {
                            events.push(change.clone());
                            known_keys.insert(key.clone());
                        }
                        SpaceEvent::Gone { key } if known_keys.remove(key) => {
                            events.push(change.clone())
                        }
                        _ => {}
                    }
                }
                for (key, space) in &self.live_spaces {
                    if !known_keys.contains(key)
                        && published_keys.contains(key)
                        && !self
                            .failed_sockets
                            .contains(&space.namespace.session_socket_id)
                        && let Some(published) = self.published.get(key)
                    {
                        events.push(SpaceEvent::Space {
                            key: key.clone(),
                            frame: published.frame.clone(),
                        });
                        known_keys.insert(key.clone());
                    }
                }
                if !events.is_empty()
                    && subscriber
                        .frames
                        .try_send(Frame::Spaces(events.into()))
                        .is_err()
                {
                    stale.push(*id);
                }
            }
            for id in stale {
                self.drop_subscriber(id);
            }
        }
        self.published.retain(|key, _| {
            self.live_spaces.contains_key(key)
                || matches!(incoming, Some(Subscription::Space(wanted)) if wanted == key)
                || self
                    .subscribers
                    .values()
                    .any(|s| matches!(&s.state, ConnectionState::Space(wanted) if wanted == key))
        });
        if !failed {
            self.data_version = Some(version);
        }
        self.metadata_dirty |= failed;
        if !failed {
            self.complete_replays();
        }
        !failed
    }

    fn complete_replays(&mut self) {
        if !self
            .subscribers
            .values()
            .any(|subscriber| matches!(subscriber.state, ConnectionState::PendingAll))
        {
            return;
        }
        let Some(discovery) = &self.discovery else {
            return;
        };
        if discovery.cancel.load(Ordering::Relaxed)
            || self
                .spaces
                .values()
                .map(|space| &space.namespace.session_socket_id)
                .chain(
                    self.live_spaces
                        .values()
                        .map(|space| &space.namespace.session_socket_id),
                )
                .any(|socket| !self.observed_sockets.contains(socket))
        {
            return;
        }
        let mut keys: Vec<_> = self
            .live_spaces
            .iter()
            .filter(|(_, space)| {
                !self
                    .failed_sockets
                    .contains(&space.namespace.session_socket_id)
            })
            .map(|(key, _)| key.clone())
            .collect();
        keys.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let initial: Option<Vec<_>> = keys
            .iter()
            .map(|key| {
                self.published.get(key).map(|published| SpaceEvent::Space {
                    key: key.clone(),
                    frame: published.frame.clone(),
                })
            })
            .collect();
        let Some(mut initial) = initial else { return };
        let known_keys: HashSet<_> = keys.iter().cloned().collect();
        initial.push(SpaceEvent::Ready { keys });
        let batch = Frame::Spaces(initial.into());
        let mut stale = Vec::new();
        for (id, subscriber) in &mut self.subscribers {
            if matches!(subscriber.state, ConnectionState::PendingAll) {
                if subscriber.frames.try_send(batch.clone()).is_ok() {
                    subscriber.state = ConnectionState::LiveAll {
                        known_keys: known_keys.clone(),
                    };
                } else {
                    stale.push(*id);
                }
            }
        }
        for id in stale {
            self.drop_subscriber(id);
        }
    }

    fn sync_discovery(&mut self) {
        let watching = self
            .subscribers
            .values()
            .any(|subscriber| !matches!(subscriber.state, ConnectionState::Space(_)));
        if !watching {
            if let Some(discovery) = &self.discovery {
                discovery.stop();
            }
            if !self.live_spaces.is_empty() {
                self.live_spaces.clear();
                self.metadata_dirty = true;
            }
            self.unpublished_spaces.clear();
            self.observed_sockets.clear();
            self.failed_sockets.clear();
            self.retry_discovery_drain = false;
            return;
        }
        if self.discovery.as_ref().is_some_and(|discovery| {
            discovery.cancel.load(Ordering::Relaxed) && discovery.finished()
        }) {
            self.discovery = None;
        }
        let sockets = self
            .spaces
            .values()
            .map(|space| space.namespace.session_socket_id.clone())
            .chain(
                self.live_spaces
                    .values()
                    .map(|space| space.namespace.session_socket_id.clone()),
            )
            .collect();
        match &self.discovery {
            None => {
                let notify = self
                    .subscribers
                    .values()
                    .find(|subscriber| !matches!(subscriber.state, ConnectionState::Space(_)))
                    .unwrap()
                    .notify
                    .clone();
                self.discovery = Some(Discovery::start(
                    sockets,
                    self.socket_states.clone(),
                    notify,
                ));
            }
            Some(discovery) if !discovery.cancel.load(Ordering::Relaxed) => {
                if let Ok(mut mailbox) = discovery.mailbox.try_lock() {
                    mailbox.sockets = sockets;
                }
            }
            Some(_) => {}
        }
    }

    fn refresh_live_spaces(&mut self) {
        self.retry_discovery_drain = false;
        let Some(discovery) = &self.discovery else {
            return;
        };
        if discovery.cancel.load(Ordering::Relaxed) {
            return;
        }
        let (latest, answered, failed) = match discovery.mailbox.try_lock() {
            Ok(mut mailbox) => {
                let mut latest = std::mem::take(&mut mailbox.latest);
                latest.retain(|socket, _| mailbox.sockets.contains(socket));
                let mut answered = std::mem::take(&mut mailbox.answered);
                answered.retain(|socket| mailbox.sockets.contains(socket));
                mailbox.notified = false;
                (latest, answered, mailbox.failed.clone())
            }
            Err(_) => {
                self.retry_discovery_drain = true;
                return;
            }
        };
        for socket in answered {
            self.metadata_dirty |= self.observed_sockets.insert(socket.clone());
            self.metadata_dirty |= if failed.contains(&socket) {
                self.failed_sockets.insert(socket)
            } else {
                self.failed_sockets.remove(&socket)
            };
        }
        for (socket, workspaces) in latest {
            self.live_spaces
                .retain(|_, space| space.namespace.session_socket_id != socket);
            for workspace in workspaces {
                let namespace = StateNamespace::for_space(&socket, workspace.workspace_id);
                self.live_spaces.insert(
                    SpaceKey::of(&namespace),
                    LiveSpace {
                        namespace,
                        label: workspace.label,
                    },
                );
            }
        }
    }

    fn open_store(&mut self) -> bool {
        match StateStore::open_read_only(self.files.state()) {
            Ok(store) => {
                // One thread reads for every space, so a long wait on a busy
                // writer would stall them all; a failed read retries next tick.
                if let Some(store) = &store
                    && let Err(error) = store.set_busy_timeout(Duration::from_millis(250))
                {
                    self.log(format!("could not set the busy timeout: {error}"));
                }
                self.store = store;
                true
            }
            Err(error) if error.kind() == StoreErrorKind::UnsupportedSchema => {
                self.unsupported_schema = true;
                false
            }
            Err(error) => {
                self.log(format!("could not open state: {error}"));
                false
            }
        }
    }

    fn log(&mut self, message: String) {
        if self.last_error.as_ref() != Some(&message) {
            eprintln!("dashboard: {message}");
            self.last_error = Some(message);
        }
    }

    fn publish(&mut self, key: &SpaceKey) -> Option<Arc<str>> {
        if self.store.is_none() {
            self.open_store();
        }
        let label = self.files.labels().remove(key.as_str());
        let now = now_ms();
        let mut expires_at_ms = None;
        let mut snapshot = match self.namespace(key) {
            Some(namespace) => {
                let rows = match self.store.as_mut()?.dashboard_rows(&namespace) {
                    Ok(rows) => rows,
                    Err(error) => {
                        self.log(format!("could not read space: {error}"));
                        return None;
                    }
                };
                expires_at_ms = rows
                    .addresses
                    .iter()
                    .filter_map(snapshot::closed_member_expires_at)
                    .filter(|expiry| *expiry > now)
                    .min();
                snapshot::build(&rows, label.as_deref(), now)
            }
            None => DashboardSnapshot::unknown(key.clone(), label.as_deref(), now),
        };
        let live_label = self
            .live_spaces
            .get(key)
            .and_then(|space| space.label.as_deref());
        if let Some(space) = self.live_spaces.get(key) {
            snapshot.space.workspace_id = space.namespace.space_id.as_str().to_owned();
        }
        let workspace_id = snapshot.space.workspace_id.clone();
        snapshot.space.set_live_label(
            &workspace_id,
            live_label,
            self.addressability(key, snapshot.space.state),
        );
        // Hash without the timestamp, so an unchanged space sends no frame.
        snapshot.generated_at_ms = 0;
        let hash: [u8; 32] = Sha256::digest(serde_json::to_vec(&snapshot).ok()?).into();
        snapshot.generated_at_ms = now;
        let frame: Arc<str> = serde_json::to_string(&snapshot).ok()?.into();
        self.published.insert(
            key.clone(),
            Published {
                expires_at_ms,
                hash,
                frame: frame.clone(),
            },
        );
        Some(frame)
    }

    fn addressability(
        &self,
        key: &SpaceKey,
        state: snapshot::SpaceState,
    ) -> Result<String, snapshot::NotAddressableReason> {
        use crate::spaces::{SpaceDirectory, SpaceEntry, names_in_scope, workspace_token};
        use snapshot::NotAddressableReason as Reason;
        let target = self.live_spaces.get(key).ok_or(Reason::Unavailable)?;
        let raw_label = target.label.as_deref().ok_or(Reason::MissingLabel)?;
        let label = crate::WorkspaceLabel::from_herdr(raw_label);
        if label.as_str().is_empty() {
            return Err(Reason::EmptyLabel);
        }
        if label.as_str() == "all" {
            return Err(Reason::ReservedAll);
        }
        if !self.spaces.get(key).is_some_and(|space| space.activated) {
            return Err(match state {
                snapshot::SpaceState::Deactivated => Reason::NotActive,
                _ => Reason::NotActivated,
            });
        }
        let entries: Vec<_> = self
            .live_spaces
            .values()
            .filter(|space| space.namespace.session_socket_id == target.namespace.session_socket_id)
            .map(|space| SpaceEntry {
                space: space.namespace.space_id.clone(),
                label: space
                    .label
                    .as_deref()
                    .map(crate::WorkspaceLabel::from_herdr),
                repo: None,
                display: space.namespace.space_id.as_str().to_owned(),
                activated: self
                    .spaces
                    .get(&SpaceKey::of(&space.namespace))
                    .is_some_and(|known| known.activated),
                names: BTreeSet::new(),
                retired: BTreeSet::new(),
            })
            .collect();
        let token = workspace_token(
            &target.namespace.space_id,
            Some(&label),
            entries
                .iter()
                .map(|entry| (&entry.space, entry.label.as_ref())),
        );
        let collision_reason = if entries.iter().any(|entry| {
            entry.space != target.namespace.space_id && entry.label.as_ref() == Some(&label)
        }) {
            Reason::SharedLabel
        } else {
            Reason::IdCollision
        };
        if token != label {
            return Err(collision_reason);
        }
        let mut others = entries;
        let index = others
            .iter()
            .position(|entry| entry.space == target.namespace.space_id)
            .ok_or(Reason::Unavailable)?;
        let here = others.swap_remove(index);
        let directory = SpaceDirectory::new(here, others);
        names_in_scope(&crate::SpaceScope::Named(vec![label.clone()]), &directory).map_err(
            |refusal| match refusal {
                crate::spaces::RouteRefusal::DuplicateLabel { .. } => collision_reason,
                crate::spaces::RouteRefusal::NotActivated { .. } => match state {
                    snapshot::SpaceState::Deactivated => Reason::NotActive,
                    _ => Reason::NotActivated,
                },
                _ => Reason::Unavailable,
            },
        )?;
        Ok(label.as_str().to_owned())
    }

    fn namespace(&mut self, key: &SpaceKey) -> Option<StateNamespace> {
        if !self.spaces.contains_key(key) && !self.live_spaces.contains_key(key) {
            self.reload_spaces();
        }
        self.spaces.get(key).map(|space| space.namespace.clone())
    }

    fn reload_spaces(&mut self) -> Option<Vec<crate::dashboard::rows::SpaceRow>> {
        let store = self.store.as_mut()?;
        let rows = match store.dashboard_spaces() {
            Ok(rows) => rows,
            Err(error) => {
                self.log(format!("could not read spaces: {error}"));
                return None;
            }
        };
        self.spaces = rows
            .iter()
            .map(|row| {
                (
                    SpaceKey::of(&row.namespace),
                    KnownSpace {
                        namespace: row.namespace.clone(),
                        activated: row.state.activated && !row.state.deactivated,
                    },
                )
            })
            .collect();
        Some(rows)
    }

    fn spaces_json(&mut self) -> String {
        if self.store.is_none() {
            self.open_store();
        }
        let labels = self.files.labels();
        let spaces: Vec<SpaceSummary> = self
            .reload_spaces()
            .unwrap_or_default()
            .iter()
            .map(|row| {
                let key = SpaceKey::of(&row.namespace);
                SpaceSummary::of(row, labels.get(key.as_str()).map(String::as_str))
            })
            .collect();
        serde_json::to_string(&spaces).unwrap_or_else(|_| "[]".to_owned())
    }
}

pub(crate) fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ActorKind, MutationAttribution, PaneAddressDraft, PaneAddressId, PaneRoleId};
    use std::sync::mpsc::sync_channel;

    fn add_address(state: &std::path::Path, namespace: &StateNamespace, alias: &str) {
        let attribution = MutationAttribution::new(ActorKind::Plugin, "test", 1);
        StateStore::open(state)
            .unwrap()
            .transaction(|tx| {
                tx.create_pane_address(
                    namespace,
                    &PaneAddressDraft {
                        id: PaneAddressId::new(format!("address-{alias}")),
                        role_id: PaneRoleId::new(format!("role-{alias}")),
                        role_name: alias.to_owned(),
                        role_description: String::new(),
                        alias: alias.to_owned(),
                    },
                    &attribution,
                )
                .map(|_| ())
            })
            .unwrap();
    }

    fn test_hub(files: &ServerFiles) -> Hub<'_> {
        Hub {
            files,
            instance: "test",
            idle: Duration::from_secs(60),
            store: None,
            data_version: None,
            spaces: HashMap::new(),
            subscribers: HashMap::new(),
            published: HashMap::new(),
            live_spaces: HashMap::new(),
            observed_sockets: HashSet::new(),
            failed_sockets: HashSet::new(),
            unpublished_spaces: HashSet::new(),
            discovery: None,
            retry_discovery_drain: false,
            socket_states: Arc::new(Mutex::new(HashMap::new())),
            metadata_dirty: false,
            idle_since: None,
            unsupported_schema: false,
            schema_busy_since: None,
            last_error: None,
        }
    }

    fn freeze_discovery(hub: &mut Hub<'_>) {
        hub.discovery = None;
        hub.discovery = Some(Discovery {
            cancel: Arc::new(AtomicBool::new(false)),
            mailbox: Arc::new(Mutex::new(DiscoveryMailbox {
                sockets: hub
                    .spaces
                    .values()
                    .map(|space| space.namespace.session_socket_id.clone())
                    .collect(),
                latest: HashMap::new(),
                answered: HashSet::new(),
                failed: HashSet::new(),
                notified: false,
            })),
            thread: None,
        });
    }

    fn schema_test_files(name: &str) -> ServerFiles {
        let state =
            std::env::temp_dir().join(format!("namedrop-hub-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&state);
        StateStore::open(&state).unwrap();
        let files = ServerFiles::at(&state).unwrap();
        let claim = files.claim().unwrap().unwrap();
        files
            .publish(
                &claim,
                &super::super::server_files::ServerInfo {
                    pid: std::process::id(),
                    port: 0,
                    instance: "test".to_owned(),
                    started_at_ms: now_ms(),
                    ui: Default::default(),
                },
            )
            .unwrap();
        files
    }

    fn schema_test_hub(files: &ServerFiles) -> Hub<'_> {
        let mut hub = test_hub(files);
        assert!(hub.open_store());
        hub.store
            .as_ref()
            .unwrap()
            .set_busy_timeout(Duration::ZERO)
            .unwrap();
        hub
    }

    fn add_schema_subscriber(hub: &mut Hub<'_>) {
        hub.subscribers.insert(
            1,
            Subscriber {
                state: ConnectionState::PendingAll,
                frames: sync_channel(1).0,
                notify: std::sync::mpsc::channel().0,
            },
        );
    }

    #[test]
    fn a_busy_schema_read_expires_after_five_continuous_seconds() {
        let files = schema_test_files("schema-expiry");
        let mut hub = schema_test_hub(&files);
        let writer = rusqlite::Connection::open(files.state().join("namedrop.sqlite3")).unwrap();
        writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let now = Instant::now();
        assert!(hub.verdict_at(now).is_none());
        assert!(hub.verdict_at(now + Duration::from_millis(4999)).is_none());
        let verdict = hub.verdict_at(now + Duration::from_secs(5));
        assert!(
            matches!(verdict, Some(Verdict::Exit("the state schema changed"))),
            "expected schema-changed exit, got {:?}",
            verdict.map(|verdict| verdict.reason())
        );
        add_schema_subscriber(&mut hub);
        assert!(matches!(
            hub.verdict_at(now + Duration::from_secs(6)),
            Some(Verdict::Handoff("the state schema changed"))
        ));
        writer.execute_batch("ROLLBACK").unwrap();
        let _ = fs::remove_dir_all(files.state());
    }

    #[test]
    fn a_successful_schema_read_resets_the_busy_budget() {
        let files = schema_test_files("schema-reset");
        let mut hub = schema_test_hub(&files);
        let writer = rusqlite::Connection::open(files.state().join("namedrop.sqlite3")).unwrap();
        let now = Instant::now();
        writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
        assert!(hub.verdict_at(now).is_none());
        assert!(hub.verdict_at(now + Duration::from_secs(4)).is_none());
        writer.execute_batch("ROLLBACK").unwrap();
        assert!(hub.verdict_at(now + Duration::from_secs(4)).is_none());
        writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
        assert!(hub.verdict_at(now + Duration::from_secs(6)).is_none());
        assert!(hub.verdict_at(now + Duration::from_millis(10999)).is_none());
        let verdict = hub.verdict_at(now + Duration::from_secs(11));
        assert!(
            matches!(verdict, Some(Verdict::Exit("the state schema changed"))),
            "expected schema-changed exit, got {:?}",
            verdict.map(|verdict| verdict.reason())
        );
        writer.execute_batch("ROLLBACK").unwrap();
        let _ = fs::remove_dir_all(files.state());
    }

    #[test]
    fn a_non_busy_schema_read_error_exits_or_hands_off_immediately() {
        let files = schema_test_files("schema-read-error");
        let mut hub = schema_test_hub(&files);
        let writer = rusqlite::Connection::open(files.state().join("namedrop.sqlite3")).unwrap();
        let now = Instant::now();
        for after_busy in [false, true] {
            hub.subscribers.clear();
            writer
                .pragma_update(None, "user_version", crate::store::SCHEMA_VERSION)
                .unwrap();
            assert!(hub.verdict_at(now).is_none());
            if after_busy {
                writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
                assert!(hub.verdict_at(now).is_none());
                writer.execute_batch("ROLLBACK").unwrap();
            }
            writer.pragma_update(None, "user_version", -1).unwrap();
            assert_eq!(
                hub.store
                    .as_ref()
                    .unwrap()
                    .schema_version()
                    .unwrap_err()
                    .kind(),
                StoreErrorKind::Storage
            );
            let verdict = hub.verdict_at(now + Duration::from_millis(1));
            assert!(
                matches!(verdict, Some(Verdict::Exit("the state schema changed"))),
                "expected schema-changed exit, got {:?}",
                verdict.map(|verdict| verdict.reason())
            );
            add_schema_subscriber(&mut hub);
            assert!(matches!(
                hub.verdict_at(now + Duration::from_millis(1)),
                Some(Verdict::Handoff("the state schema changed"))
            ));
        }
        let _ = fs::remove_dir_all(files.state());
    }

    #[test]
    fn a_busy_schema_read_preserves_other_exit_and_handoff_checks() {
        let files = schema_test_files("schema-precedence");
        let mut hub = schema_test_hub(&files);
        let writer = rusqlite::Connection::open(files.state().join("namedrop.sqlite3")).unwrap();
        writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let now = Instant::now();
        assert!(hub.verdict_at(now).is_none());
        hub.idle_since = Some(now - hub.idle);
        assert!(matches!(hub.verdict_at(now), Some(Verdict::Idle)));
        fs::remove_file(files.state().join("dashboard/server.json")).unwrap();
        assert!(matches!(
            hub.verdict_at(now),
            Some(Verdict::Exit("server.json was replaced"))
        ));
        let runtime = files.state().join("runtime");
        fs::create_dir_all(&runtime).unwrap();
        fs::write(runtime.join("namedrop"), "new release").unwrap();
        assert!(matches!(
            hub.verdict_at(now),
            Some(Verdict::Exit("a newer release is current"))
        ));
        fs::write(runtime.join("migration.json"), "{}").unwrap();
        assert!(matches!(
            hub.verdict_at(now),
            Some(Verdict::Exit("an update is migrating state"))
        ));
        add_schema_subscriber(&mut hub);
        assert!(matches!(
            hub.verdict_at(now),
            Some(Verdict::Handoff("an update is migrating state"))
        ));
        hub.unsupported_schema = true;
        assert!(matches!(
            hub.verdict_at(now),
            Some(Verdict::Exit(
                "this release does not support the state schema"
            ))
        ));
        writer.execute_batch("ROLLBACK").unwrap();
        let _ = fs::remove_dir_all(files.state());
    }

    #[test]
    fn sqlite_busy_and_locked_are_retryable_schema_read_errors() {
        for code in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED] {
            let error = crate::StoreError::from(rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(code),
                None,
            ));
            assert_eq!(error.kind(), StoreErrorKind::Busy);
        }
    }

    #[test]
    fn a_busy_schema_read_keeps_the_hub_running() {
        let state =
            std::env::temp_dir().join(format!("namedrop-hub-schema-busy-{}", std::process::id()));
        let _ = fs::remove_dir_all(&state);
        StateStore::open(&state).unwrap();
        let files = ServerFiles::at(&state).unwrap();
        let claim = files.claim().unwrap().unwrap();
        files
            .publish(
                &claim,
                &super::super::server_files::ServerInfo {
                    pid: std::process::id(),
                    port: 0,
                    instance: "test".to_owned(),
                    started_at_ms: now_ms(),
                    ui: Default::default(),
                },
            )
            .unwrap();
        let mut hub = test_hub(&files);
        assert!(hub.open_store());
        hub.store
            .as_ref()
            .unwrap()
            .set_busy_timeout(Duration::ZERO)
            .unwrap();
        let writer = rusqlite::Connection::open(state.join("namedrop.sqlite3")).unwrap();
        writer.execute_batch("BEGIN EXCLUSIVE").unwrap();
        assert_eq!(
            hub.store
                .as_ref()
                .unwrap()
                .schema_version()
                .unwrap_err()
                .kind(),
            StoreErrorKind::Busy
        );
        assert_eq!(hub.verdict().map(|verdict| verdict.reason()), None);

        writer.execute_batch("ROLLBACK").unwrap();
        assert_eq!(
            hub.store.as_ref().unwrap().schema_version().unwrap(),
            crate::store::SCHEMA_VERSION
        );
        assert_eq!(hub.verdict().map(|verdict| verdict.reason()), None);
        let _ = fs::remove_dir_all(&state);
    }

    #[test]
    fn a_changed_schema_exits_or_hands_off_to_connected_tabs() {
        let state = std::env::temp_dir().join(format!(
            "namedrop-hub-schema-changed-{}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&state);
        StateStore::open(&state).unwrap();
        let files = ServerFiles::at(&state).unwrap();
        let mut hub = test_hub(&files);
        assert!(hub.open_store());
        let writer = rusqlite::Connection::open(state.join("namedrop.sqlite3")).unwrap();
        writer
            .pragma_update(None, "user_version", crate::store::SCHEMA_VERSION + 1)
            .unwrap();
        assert!(matches!(
            hub.verdict(),
            Some(Verdict::Exit("the state schema changed"))
        ));
        hub.subscribers.insert(
            1,
            Subscriber {
                state: ConnectionState::PendingAll,
                frames: sync_channel(1).0,
                notify: std::sync::mpsc::channel().0,
            },
        );
        assert!(matches!(
            hub.verdict(),
            Some(Verdict::Handoff("the state schema changed"))
        ));
        let _ = fs::remove_dir_all(&state);
    }

    #[test]
    fn retained_member_expiry_republishes_without_a_database_change() {
        let state =
            std::env::temp_dir().join(format!("namedrop-hub-expiry-{}", std::process::id()));
        let _ = fs::remove_dir_all(&state);
        let namespace = StateNamespace::for_space("socket-a", "w1");
        let at = MutationAttribution::new(ActorKind::Plugin, "test", 1);
        StateStore::open(&state)
            .unwrap()
            .transaction(|tx| tx.activate_worktree(&namespace, &at))
            .unwrap();
        add_address(&state, &namespace, "closed");
        add_address(&state, &namespace, "reader");
        StateStore::open(&state)
            .unwrap()
            .transaction(|tx| {
                let thread = crate::ThreadId::new("history-thread");
                tx.create_thread(&namespace, &thread, "History", &at)?;
                tx.append_message(
                    &namespace,
                    &crate::MessageDraft {
                        thread_id: thread,
                        message_id: crate::MessageId::new("history-message"),
                        in_reply_to: None,
                        author_address_id: PaneAddressId::new("address-closed"),
                        author_kind: ActorKind::AgentSession,
                        author_alias_snapshot: "closed".to_owned(),
                        author_agent_session_snapshot: None,
                        request_id: "history-request".to_owned(),
                        authored_source: "History".to_owned(),
                        body: "History".to_owned(),
                        grammar_version: "note/v1".to_owned(),
                        recipients: vec![crate::MessageRecipientDraft {
                            address_id: PaneAddressId::new("address-reader"),
                            alias_snapshot: "reader".to_owned(),
                            agent_session_snapshot: None,
                            delivery_id: crate::DeliveryId::new("history-delivery"),
                        }],
                        context_references: Vec::new(),
                    },
                    &at,
                )
                .map(|_| ())
            })
            .unwrap();
        let closed_at = u64::try_from(now_ms() - 300_000 + 1_500).unwrap();
        StateStore::open(&state)
            .unwrap()
            .transaction(|tx| {
                let address = PaneAddressId::new("address-closed");
                tx.bind_pane(
                    &namespace,
                    &crate::PaneBindingId::new("binding-closed"),
                    &address,
                    "p1",
                    &at,
                )?;
                tx.retire_pane_address(
                    &namespace,
                    &address,
                    &MutationAttribution::new(ActorKind::Plugin, "test", closed_at),
                )
            })
            .unwrap();
        let files = ServerFiles::at(&state).unwrap();
        let mut hub = test_hub(&files);
        let (frames, inbox) = sync_channel(8);
        hub.handle(HubCommand::Subscribe {
            id: 1,
            subscription: Subscription::Space(SpaceKey::of(&namespace)),
            frames,
            notify: std::sync::mpsc::channel().0,
        });
        let Frame::Snapshot(initial) = inbox.recv().unwrap() else {
            panic!("snapshot")
        };
        let initial: serde_json::Value = serde_json::from_str(&initial).unwrap();
        assert!(
            initial["participants"]
                .as_array()
                .unwrap()
                .iter()
                .any(|member| member["alias"] == "@closed")
        );
        let referenced = |frame: &serde_json::Value| {
            frame["participants"]
                .as_array()
                .unwrap()
                .iter()
                .find(|member| member["alias"] == "@closed")
                .cloned()
                .expect("history must keep its retired author")
        };
        assert_eq!(referenced(&initial)["activity"], "retired");
        assert_eq!(referenced(&initial)["recently_closed"], true);
        let version = hub.data_version;
        freeze_discovery(&mut hub);
        let deadline = Instant::now() + Duration::from_secs(4);
        let expired = loop {
            hub.refresh(None);
            if let Ok(Frame::Snapshot(frame)) = inbox.try_recv() {
                break frame;
            }
            assert!(Instant::now() < deadline, "expiry did not publish a frame");
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(hub.data_version, version);
        let expired: serde_json::Value = serde_json::from_str(&expired).unwrap();
        assert_eq!(referenced(&expired)["activity"], "retired");
        assert_eq!(referenced(&expired)["recently_closed"], false);
        assert_eq!(expired["messages"][0]["author"], "address-closed");
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn a_failed_read_is_retried_on_the_next_tick() {
        let state = std::env::temp_dir().join(format!("namedrop-hub-{}", std::process::id()));
        let _ = fs::remove_dir_all(&state);
        let namespace = StateNamespace::for_space("socket-a", "w1");
        StateStore::open(&state)
            .unwrap()
            .transaction(|tx| {
                tx.activate_worktree(
                    &namespace,
                    &MutationAttribution::new(ActorKind::Plugin, "test", 1),
                )
                .map(|_| ())
            })
            .unwrap();
        let files = ServerFiles::at(&state).unwrap();
        let mut hub = test_hub(&files);
        let (frames, inbox) = sync_channel(8);
        hub.handle(HubCommand::Subscribe {
            id: 1,
            subscription: Subscription::Space(SpaceKey::of(&namespace)),
            frames,
            notify: std::sync::mpsc::channel().0,
        });
        hub.refresh(None);
        assert!(matches!(inbox.try_recv(), Ok(Frame::Snapshot(_))));

        add_address(&state, &namespace, "late");
        // A temp table shadows the real one on the hub's connection only, so
        // the read fails without another commit to bump data_version.
        let shadow = "CREATE TEMP TABLE pane_addresses(broken INTEGER)";
        hub.store
            .as_ref()
            .unwrap()
            .connection()
            .execute_batch(shadow)
            .unwrap();
        hub.refresh(None);
        assert!(
            inbox.try_recv().is_err(),
            "the failed read published nothing"
        );

        let repair = "DROP TABLE temp.pane_addresses";
        hub.store
            .as_ref()
            .unwrap()
            .connection()
            .execute_batch(repair)
            .unwrap();
        hub.refresh(None);
        match inbox.try_recv() {
            Ok(Frame::Snapshot(frame)) => assert!(frame.contains("@late"), "{frame}"),
            _ => panic!("the change committed during the failed read never arrived"),
        }
        let _ = fs::remove_dir_all(&state);
    }

    #[test]
    fn a_pending_join_retries_after_a_read_failure_and_updates_legacy_addressability() {
        let state = std::env::temp_dir().join(format!("namedrop-hub-join-{}", std::process::id()));
        let _ = fs::remove_dir_all(&state);
        let namespace = StateNamespace::for_space("socket-a", "w1");
        StateStore::open(&state)
            .unwrap()
            .transaction(|tx| {
                tx.activate_worktree(
                    &namespace,
                    &MutationAttribution::new(ActorKind::Plugin, "test", 1),
                )
            })
            .unwrap();
        let files = ServerFiles::at(&state).unwrap();
        let mut hub = test_hub(&files);
        let key = SpaceKey::of(&namespace);
        let (frames, legacy) = sync_channel(8);
        hub.handle(HubCommand::Subscribe {
            id: 1,
            subscription: Subscription::Space(key.clone()),
            frames,
            notify: std::sync::mpsc::channel().0,
        });
        assert!(matches!(legacy.try_recv(), Ok(Frame::Snapshot(_))));
        let (frames, all) = sync_channel(8);
        hub.handle(HubCommand::Subscribe {
            id: 2,
            subscription: Subscription::All,
            frames,
            notify: std::sync::mpsc::channel().0,
        });
        freeze_discovery(&mut hub);
        hub.observed_sockets.insert("socket-a".to_owned());
        hub.complete_replays();
        assert!(matches!(all.try_recv(), Ok(Frame::Spaces(_))));

        hub.live_spaces.insert(
            key.clone(),
            LiveSpace {
                namespace,
                label: None,
            },
        );
        hub.unpublished_spaces.insert(key.clone());
        hub.metadata_dirty = true;
        hub.store
            .as_ref()
            .unwrap()
            .connection()
            .execute_batch("CREATE TEMP TABLE pane_addresses(broken INTEGER)")
            .unwrap();
        hub.refresh(None);
        assert!(all.try_recv().is_err());

        hub.store
            .as_ref()
            .unwrap()
            .connection()
            .execute_batch("DROP TABLE temp.pane_addresses")
            .unwrap();
        hub.refresh(None);
        match all.try_recv() {
            Ok(Frame::Spaces(events)) => {
                assert_eq!(events.len(), 1);
                assert!(
                    matches!(&events[0], SpaceEvent::Space { key: joined, .. } if joined == &key)
                );
            }
            _ => panic!("the rejoined workspace never arrived after the read recovered"),
        }
        let Ok(Frame::Snapshot(frame)) = legacy.try_recv() else {
            panic!("the recovered discovery must update legacy addressability");
        };
        let frame: serde_json::Value = serde_json::from_str(&frame).unwrap();
        assert_eq!(frame["space"]["not_addressable_reason"], "missing_label");
        hub.refresh(None);
        assert!(
            legacy.try_recv().is_err(),
            "unchanged content sends no new frame"
        );
        let _ = fs::remove_dir_all(&state);
    }

    #[test]
    fn pending_replay_waits_through_a_failed_read_and_includes_a_concurrent_write() {
        let state = std::env::temp_dir().join(format!("namedrop-hub-ready-{}", std::process::id()));
        let _ = fs::remove_dir_all(&state);
        let namespace = StateNamespace::for_space("socket-a", "w1");
        StateStore::open(&state)
            .unwrap()
            .transaction(|tx| {
                tx.activate_worktree(
                    &namespace,
                    &MutationAttribution::new(ActorKind::Plugin, "test", 1),
                )
            })
            .unwrap();
        let files = ServerFiles::at(&state).unwrap();
        let mut hub = test_hub(&files);
        let key = SpaceKey::of(&namespace);
        let (frames, legacy) = sync_channel(8);
        hub.handle(HubCommand::Subscribe {
            id: 1,
            subscription: Subscription::Space(key.clone()),
            frames,
            notify: std::sync::mpsc::channel().0,
        });
        assert!(matches!(legacy.try_recv(), Ok(Frame::Snapshot(_))));
        let (frames, all) = sync_channel(8);
        hub.handle(HubCommand::Subscribe {
            id: 2,
            subscription: Subscription::All,
            frames,
            notify: std::sync::mpsc::channel().0,
        });
        freeze_discovery(&mut hub);
        assert!(all.try_recv().is_err());
        hub.live_spaces.insert(
            key.clone(),
            LiveSpace {
                namespace: namespace.clone(),
                label: None,
            },
        );
        hub.observed_sockets.insert("socket-a".to_owned());
        hub.metadata_dirty = true;
        hub.store
            .as_ref()
            .unwrap()
            .connection()
            .execute_batch("CREATE TEMP TABLE pane_addresses(broken INTEGER)")
            .unwrap();
        hub.refresh(None);
        assert!(
            all.try_recv().is_err(),
            "failed read completed replay using a stale cached frame"
        );
        add_address(&state, &namespace, "during-initial");
        hub.store
            .as_ref()
            .unwrap()
            .connection()
            .execute_batch("DROP TABLE temp.pane_addresses")
            .unwrap();
        hub.refresh(None);
        match all.try_recv() {
            Ok(Frame::Spaces(events)) => {
                assert_eq!(events.len(), 2);
                assert!(
                    matches!(&events[0], SpaceEvent::Space { key: observed, frame } if observed == &key && frame.contains("@during-initial"))
                );
                assert!(matches!(&events[1], SpaceEvent::Ready { keys } if keys == &vec![key]));
            }
            _ => panic!("successful retry did not finish the initial replay"),
        }
        hub.refresh(None);
        assert!(
            all.try_recv().is_err(),
            "ready repeated after successful retry"
        );
        let _ = fs::remove_dir_all(&state);
    }
}
