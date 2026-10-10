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
    /// No tab for the idle window; the browser session rotates.
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
            return Some(Verdict::Idle);
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
        let mut keys: Vec<SpaceKey> = self.subscribers.values().map(|s| s.key.clone()).collect();
        keys.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        keys.dedup();
        let mut failed = false;
        for key in keys {
            let before = self.published.get(&key).map(|p| p.hash);
            let Some(frame) = self.publish(&key) else {
                failed = true;
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
        // An unrecorded version retries every space on the next tick.
        if !failed {
            self.data_version = Some(version);
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
        let mut hub = Hub {
            files: &files,
            instance: "test",
            idle: Duration::from_secs(60),
            store: None,
            data_version: None,
            spaces: HashMap::new(),
            subscribers: HashMap::new(),
            published: HashMap::new(),
            idle_since: None,
            unsupported_schema: false,
            last_error: None,
        };
        let (frames, inbox) = sync_channel(8);
        hub.handle(HubCommand::Subscribe {
            id: 1,
            key: SpaceKey::of(&namespace),
            frames,
        });
        hub.refresh();
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
        hub.refresh();
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
        hub.refresh();
        match inbox.try_recv() {
            Ok(Frame::Snapshot(frame)) => assert!(frame.contains("@late"), "{frame}"),
            _ => panic!("the change committed during the failed read never arrived"),
        }
        let _ = fs::remove_dir_all(&state);
    }
}
