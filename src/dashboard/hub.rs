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

pub(crate) enum HubCommand {
    Subscribe {
        id: u64,
        subscription: Subscription,
        frames: SyncSender<Frame>,
    },
    Unsubscribe {
        id: u64,
    },
    Spaces {
        reply: Sender<String>,
    },
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

pub(crate) enum SpaceEvent {
    Space { key: SpaceKey, frame: Arc<str> },
    Gone { key: SpaceKey },
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
}

#[derive(Default)]
struct SocketState {
    failing: bool,
    unavailable: u8,
    retired: bool,
}

impl Discovery {
    fn start(sockets: BTreeSet<String>) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let mailbox = Arc::new(Mutex::new(DiscoveryMailbox {
            sockets,
            latest: HashMap::new(),
        }));
        let worker_cancel = cancel.clone();
        let worker_mailbox = mailbox.clone();
        let thread = thread::spawn(move || Self::run(worker_cancel, worker_mailbox));
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

    fn run(cancel: Arc<AtomicBool>, mailbox: Arc<Mutex<DiscoveryMailbox>>) {
        let mut states = HashMap::<String, SocketState>::new();
        while !cancel.load(Ordering::Relaxed) {
            let started = Instant::now();
            let sockets = mailbox.lock().unwrap().sockets.clone();
            states.retain(|socket, _| sockets.contains(socket));
            for socket in sockets {
                if cancel.load(Ordering::Relaxed) {
                    return;
                }
                let state = states.entry(socket.clone()).or_default();
                if state.retired {
                    continue;
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
                match result {
                    Ok(snapshot) => {
                        if state.failing {
                            eprintln!("dashboard: live workspaces on {socket} recovered");
                        }
                        state.failing = false;
                        state.unavailable = 0;
                        mailbox
                            .lock()
                            .unwrap()
                            .latest
                            .insert(socket, snapshot.result.snapshot.workspaces);
                    }
                    Err(error) => {
                        if !state.failing {
                            eprintln!("dashboard: live workspaces on {socket} unavailable");
                        }
                        state.failing = true;
                        let unavailable = serde_json::from_str::<serde_json::Value>(&error)
                            .ok()
                            .is_some_and(|value| value["error"]["code"] == "server_not_running");
                        state.unavailable = if unavailable {
                            state.unavailable + 1
                        } else {
                            0
                        };
                        if state.unavailable >= UNAVAILABLE_LIMIT {
                            state.retired = true;
                            mailbox.lock().unwrap().latest.insert(socket, Vec::new());
                        }
                    }
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
    subscription: Subscription,
    frames: SyncSender<Frame>,
}

struct Published {
    hash: [u8; 32],
    frame: Arc<str>,
}

struct Hub<'a> {
    files: &'a ServerFiles,
    instance: &'a str,
    idle: Duration,
    store: Option<StateStore>,
    data_version: Option<i64>,
    spaces: HashMap<SpaceKey, StateNamespace>,
    subscribers: HashMap<u64, Subscriber>,
    published: HashMap<SpaceKey, Published>,
    live_spaces: HashMap<SpaceKey, LiveSpace>,
    unpublished_spaces: HashSet<SpaceKey>,
    discovery: Option<Discovery>,
    metadata_dirty: bool,
    idle_since: Option<Instant>,
    unsupported_schema: bool,
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
        unpublished_spaces: HashSet::new(),
        discovery: None,
        metadata_dirty: false,
        idle_since: Some(Instant::now()),
        unsupported_schema: false,
        last_error: None,
    };
    let verdict = loop {
        match commands.recv_timeout(TICK) {
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
            } => {
                self.refresh(Some(&subscription));
                let frame = match &subscription {
                    Subscription::Space(key) => self
                        .published
                        .get(key)
                        .map(|published| Frame::Snapshot(published.frame.clone()))
                        .or_else(|| self.publish(key).map(Frame::Snapshot)),
                    Subscription::All => {
                        let mut keys: Vec<_> = self.live_spaces.keys().collect();
                        keys.sort_by(|a, b| a.as_str().cmp(b.as_str()));
                        let initial: Vec<_> = keys
                            .into_iter()
                            .filter_map(|key| {
                                self.published.get(key).map(|published| SpaceEvent::Space {
                                    key: key.clone(),
                                    frame: published.frame.clone(),
                                })
                            })
                            .collect();
                        Some(Frame::Spaces(initial.into()))
                    }
                };
                let Some(frame) = frame else { return };
                let sent = frames.try_send(frame).is_ok();
                self.subscribers.insert(
                    id,
                    Subscriber {
                        subscription,
                        frames,
                    },
                );
                self.idle_since = None;
                if !sent {
                    self.drop_subscriber(id);
                }
                self.sync_discovery();
            }
            HubCommand::Unsubscribe { id } => self.drop_subscriber(id),
            HubCommand::Spaces { reply } => {
                let _ = reply.send(self.spaces_json());
            }
        }
    }

    fn drop_subscriber(&mut self, id: u64) {
        if let Some(gone) = self.subscribers.remove(&id) {
            if let Subscription::Space(key) = gone.subscription
                && !self.subscribers.values().any(|s| match &s.subscription {
                    Subscription::Space(other) => other == &key,
                    Subscription::All => self.live_spaces.contains_key(&key),
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
        if self.unsupported_schema {
            return Some(Verdict::Exit(
                "this release does not support the state schema",
            ));
        }
        let runtime = self.files.state().join("runtime");
        let wants_handoff =
            if fs::symlink_metadata(runtime.join("migration.json")).is_ok() {
                Some("an update is migrating state")
            } else if runtime.join("namedrop").exists()
                && fs::canonicalize(runtime.join("current/namedrop")).ok()
                    != std::env::current_exe().and_then(fs::canonicalize).ok()
            {
                Some("a newer release is current")
            } else if self.store.as_ref().is_some_and(|store| {
                store.schema_version().ok() != Some(crate::store::SCHEMA_VERSION)
            }) {
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
            .is_some_and(|since| since.elapsed() >= self.idle)
        {
            return Some(Verdict::Idle);
        }
        None
    }

    fn refresh(&mut self, incoming: Option<&Subscription>) {
        if self.store.is_none() {
            self.open_store();
        }
        let Some(store) = &self.store else { return };
        let Ok(version) = store.data_version() else {
            return;
        };
        let database_changed = self.data_version != Some(version);
        if database_changed && self.reload_spaces().is_none() {
            return;
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
        if !database_changed && !self.metadata_dirty && incoming.is_none() {
            return;
        }
        let subscriptions: Vec<_> = self
            .subscribers
            .values()
            .map(|s| &s.subscription)
            .chain(incoming)
            .collect();
        let all = subscriptions.iter().any(|s| matches!(s, Subscription::All));
        let mut keys: Vec<SpaceKey> = subscriptions
            .into_iter()
            .filter_map(|s| match s {
                Subscription::Space(key) => Some(key.clone()),
                Subscription::All => None,
            })
            .collect();
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
        for key in keys {
            let before = self.published.get(&key).map(|p| p.hash);
            let Some(frame) = self.publish(&key) else {
                failed = true;
                continue;
            };
            let content_changed = before != self.published.get(&key).map(|p| p.hash);
            let joined = self.unpublished_spaces.contains(&key);
            if !content_changed && !(all && joined) {
                continue;
            }
            let mut stale = Vec::new();
            for (id, subscriber) in &self.subscribers {
                if !content_changed
                    || !matches!(&subscriber.subscription, Subscription::Space(wanted) if wanted == &key)
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
        if all && !changes.is_empty() {
            let batch = Frame::Spaces(changes.into());
            let stale: Vec<_> = self
                .subscribers
                .iter()
                .filter(|(_, s)| matches!(s.subscription, Subscription::All))
                .filter_map(|(id, s)| s.frames.try_send(batch.clone()).err().map(|_| *id))
                .collect();
            for id in stale {
                self.drop_subscriber(id);
            }
        }
        self.published.retain(|key, _| {
            self.live_spaces.contains_key(key)
                || matches!(incoming, Some(Subscription::Space(wanted)) if wanted == key)
                || self.subscribers.values().any(
                    |s| matches!(&s.subscription, Subscription::Space(wanted) if wanted == key),
                )
        });
        if !failed {
            self.data_version = Some(version);
            self.metadata_dirty = false;
        }
    }

    fn sync_discovery(&mut self) {
        let watching = self
            .subscribers
            .values()
            .any(|subscriber| matches!(subscriber.subscription, Subscription::All));
        if !watching {
            if let Some(discovery) = &self.discovery {
                discovery.stop();
            }
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
            .map(|namespace| namespace.session_socket_id.clone())
            .chain(
                self.live_spaces
                    .values()
                    .map(|space| space.namespace.session_socket_id.clone()),
            )
            .collect();
        match &self.discovery {
            None => self.discovery = Some(Discovery::start(sockets)),
            Some(discovery) if !discovery.cancel.load(Ordering::Relaxed) => {
                if let Ok(mut mailbox) = discovery.mailbox.try_lock() {
                    mailbox.sockets = sockets;
                }
            }
            Some(_) => {}
        }
    }

    fn refresh_live_spaces(&mut self) {
        let Some(discovery) = &self.discovery else {
            return;
        };
        if discovery.cancel.load(Ordering::Relaxed) {
            return;
        }
        let latest = match discovery.mailbox.try_lock() {
            Ok(mut mailbox) => std::mem::take(&mut mailbox.latest),
            Err(_) => return,
        };
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

    fn open_store(&mut self) {
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
            }
            Err(error) if error.kind() == StoreErrorKind::UnsupportedSchema => {
                self.unsupported_schema = true;
            }
            Err(error) => self.log(format!("could not open state: {error}")),
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
        let mut snapshot = match self.namespace(key) {
            Some(namespace) => {
                let rows = match self.store.as_mut()?.dashboard_rows(&namespace) {
                    Ok(rows) => rows,
                    Err(error) => {
                        self.log(format!("could not read space: {error}"));
                        return None;
                    }
                };
                snapshot::build(&rows, label.as_deref(), 0)
            }
            None => DashboardSnapshot::unknown(key.clone(), label.as_deref(), 0),
        };
        let live_label = self
            .live_spaces
            .get(key)
            .and_then(|space| space.label.as_deref());
        if let Some(space) = self.live_spaces.get(key) {
            snapshot.space.workspace_id = space.namespace.space_id.as_str().to_owned();
        }
        let workspace_id = snapshot.space.workspace_id.clone();
        snapshot
            .space
            .set_live_label(&workspace_id, live_label, self.addressable_label(key));
        // Hash without the timestamp, so an unchanged space sends no frame.
        let hash: [u8; 32] = Sha256::digest(serde_json::to_vec(&snapshot).ok()?).into();
        snapshot.generated_at_ms = now_ms();
        let frame: Arc<str> = serde_json::to_string(&snapshot).ok()?.into();
        self.published.insert(
            key.clone(),
            Published {
                hash,
                frame: frame.clone(),
            },
        );
        Some(frame)
    }

    fn addressable_label(&self, key: &SpaceKey) -> Option<String> {
        use crate::spaces::{SpaceDirectory, SpaceEntry, names_in_scope, workspace_token};
        let target = self.live_spaces.get(key)?;
        let folded = crate::WorkspaceLabel::from_herdr(target.label.as_deref()?);
        let label = crate::WorkspaceLabel::from_token(folded.as_str())?;
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
                activated: true,
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
        if token != label {
            return None;
        }
        let mut others = entries;
        let index = others
            .iter()
            .position(|entry| entry.space == target.namespace.space_id)?;
        let here = others.swap_remove(index);
        let directory = SpaceDirectory::new(here, others);
        names_in_scope(&crate::SpaceScope::Named(vec![label.clone()]), &directory).ok()?;
        Some(label.as_str().to_owned())
    }

    fn namespace(&mut self, key: &SpaceKey) -> Option<StateNamespace> {
        if !self.spaces.contains_key(key) && !self.live_spaces.contains_key(key) {
            self.reload_spaces();
        }
        self.spaces.get(key).cloned()
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
            .map(|row| (SpaceKey::of(&row.namespace), row.namespace.clone()))
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
            unpublished_spaces: HashSet::new(),
            discovery: None,
            metadata_dirty: false,
            idle_since: None,
            unsupported_schema: false,
            last_error: None,
        }
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
    fn a_pending_join_retries_after_a_read_failure_with_an_unchanged_legacy_cache() {
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
        });
        assert!(matches!(legacy.try_recv(), Ok(Frame::Snapshot(_))));
        let (frames, all) = sync_channel(8);
        hub.handle(HubCommand::Subscribe {
            id: 2,
            subscription: Subscription::All,
            frames,
        });
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
        assert!(
            legacy.try_recv().is_err(),
            "unchanged legacy content sends no new frame"
        );
        let _ = fs::remove_dir_all(&state);
    }
}
