//! The one thread that owns the SQLite connection and every subscriber.
//! HTTP threads reach it only through `HubCommand`, so nothing is shared and
//! no read transaction outlives a single snapshot build.
use super::server_files::ServerFiles;
use super::snapshot::{self, DashboardSnapshot, SpaceKey, SpaceSummary};
use crate::{StateNamespace, StateStore, StoreErrorKind};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    fs,
    sync::{
        Arc,
        mpsc::{Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const TICK: Duration = Duration::from_millis(250);

pub(crate) enum HubCommand {
    Subscribe {
        id: u64,
        key: SpaceKey,
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
    Restarting,
}

pub(crate) enum Verdict {
    /// A newer release or schema owns the state: restart into it.
    Handoff(&'static str),
    Exit(&'static str),
}

impl Verdict {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Handoff(reason) | Self::Exit(reason) => reason,
        }
    }
}

struct Subscriber {
    key: SpaceKey,
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
        hub.refresh();
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
            HubCommand::Subscribe { id, key, frames } => {
                let frame = match self.published.get(&key) {
                    Some(published) => Some(published.frame.clone()),
                    None => self.publish(&key),
                };
                let Some(frame) = frame else { return };
                let sent = frames.try_send(Frame::Snapshot(frame)).is_ok();
                self.subscribers.insert(id, Subscriber { key, frames });
                self.idle_since = None;
                if !sent {
                    self.drop_subscriber(id);
                }
            }
            HubCommand::Unsubscribe { id } => self.drop_subscriber(id),
            HubCommand::Spaces { reply } => {
                let _ = reply.send(self.spaces_json());
            }
        }
    }

    fn drop_subscriber(&mut self, id: u64) {
        if let Some(gone) = self.subscribers.remove(&id) {
            if !self.subscribers.values().any(|s| s.key == gone.key) {
                self.published.remove(&gone.key);
            }
            if self.subscribers.is_empty() {
                self.idle_since = Some(Instant::now());
            }
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
            return Some(Verdict::Exit("idle"));
        }
        None
    }

    /// Rebuilds subscribed spaces only when another connection committed.
    fn refresh(&mut self) {
        if self.store.is_none() {
            self.open_store();
        }
        let Some(store) = &self.store else { return };
        let Ok(version) = store.data_version() else {
            return;
        };
        if self.data_version == Some(version) {
            return;
        }
        self.data_version = Some(version);
        let mut keys: Vec<SpaceKey> = self.subscribers.values().map(|s| s.key.clone()).collect();
        keys.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        keys.dedup();
        for key in keys {
            let before = self.published.get(&key).map(|p| p.hash);
            let Some(frame) = self.publish(&key) else {
                continue;
            };
            if before == self.published.get(&key).map(|p| p.hash) {
                continue;
            }
            let mut stale = Vec::new();
            for (id, subscriber) in &self.subscribers {
                if subscriber.key != key {
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
        }
    }

    fn open_store(&mut self) {
        match StateStore::open_read_only(self.files.state()) {
            Ok(store) => self.store = store,
            Err(error) if error.kind() == StoreErrorKind::UnsupportedSchema => {
                self.unsupported_schema = true;
            }
            Err(error) => self.log(format!("could not open state: {error}")),
        }
    }

    /// Logs a failure once, not on every tick it repeats.
    fn log(&mut self, message: String) {
        if self.last_error.as_ref() != Some(&message) {
            eprintln!("dashboard: {message}");
            self.last_error = Some(message);
        }
    }

    /// Builds `key`'s snapshot, records its hash, and returns its frame.
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

    fn namespace(&mut self, key: &SpaceKey) -> Option<StateNamespace> {
        if !self.spaces.contains_key(key) {
            self.reload_spaces();
        }
        self.spaces.get(key).cloned()
    }

    fn reload_spaces(&mut self) -> Vec<crate::dashboard::rows::SpaceRow> {
        let Some(store) = self.store.as_mut() else {
            return Vec::new();
        };
        let rows = store.dashboard_spaces().unwrap_or_default();
        self.spaces = rows
            .iter()
            .map(|row| (SpaceKey::of(&row.namespace), row.namespace.clone()))
            .collect();
        rows
    }

    fn spaces_json(&mut self) -> String {
        if self.store.is_none() {
            self.open_store();
        }
        let labels = self.files.labels();
        let spaces: Vec<SpaceSummary> = self
            .reload_spaces()
            .iter()
            .map(|row| {
                let key = SpaceKey::of(&row.namespace);
                SpaceSummary::of(row, labels.get(key.as_str()).map(String::as_str))
            })
            .collect();
        serde_json::to_string(&spaces).unwrap_or_else(|_| "[]".to_owned())
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
        })
}
