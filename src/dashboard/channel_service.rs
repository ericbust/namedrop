use crate::{Clock, StateNamespace, StateStore};
use std::{
    fs::{File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

#[derive(Clone, Default)]
pub(super) struct ChannelWake(Arc<WakeState>);

#[derive(Default)]
struct WakeState {
    generation: Mutex<u64>,
    ready: Condvar,
}

impl ChannelWake {
    pub(super) fn notify(&self) {
        let mut generation = self.0.generation.lock().unwrap_or_else(|e| e.into_inner());
        *generation = generation.wrapping_add(1);
        drop(generation);
        self.0.ready.notify_all();
    }

    fn generation(&self) -> u64 {
        *self.0.generation.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn wait(&self, observed: u64, cancel: &AtomicBool, timeout: Duration) {
        let generation = self.0.generation.lock().unwrap_or_else(|e| e.into_inner());
        drop(
            self.0
                .ready
                .wait_timeout_while(generation, timeout, |generation| {
                    *generation == observed && !cancel.load(Ordering::Acquire)
                }),
        );
    }
}

pub(super) struct ChannelService {
    wake: ChannelWake,
    cancel: Arc<AtomicBool>,
    workers: Vec<JoinHandle<()>>,
    state: PathBuf,
    ownership: Option<Arc<File>>,
}

impl ChannelService {
    pub fn start(state: &Path) -> Result<Self, String> {
        let ownership = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
            .open(state.join("dashboard/channel-manager.lock"))
            .map_err(|e| e.to_string())?;
        let meta = ownership.metadata().map_err(|e| e.to_string())?;
        if !meta.is_file() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o077 != 0 {
            return Err("unsafe channel manager ownership file".to_owned());
        }
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while unsafe { libc::flock(ownership.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EWOULDBLOCK)
                || std::time::Instant::now() >= deadline
            {
                return Err(format!("channel manager ownership: {error}"));
            }
            thread::sleep(Duration::from_millis(10));
        }
        recover(state)?;
        let ownership = Arc::new(ownership);
        #[cfg(target_os = "linux")]
        crate::agent_status_model::set_worker_lifetime_lease(Some(ownership.clone()));
        let cancel = Arc::new(AtomicBool::new(false));
        let mut service = Self {
            cancel,
            wake: ChannelWake::default(),
            workers: Vec::new(),
            state: state.to_owned(),
            ownership: Some(ownership),
        };
        for index in 0..4 {
            let state = state.to_owned();
            let cancel = service.cancel.clone();
            let wake = service.wake.clone();
            service.workers.push(
                thread::Builder::new()
                    .name(format!("channel-router-{index}"))
                    .spawn(move || worker(&state, &cancel, &wake, Duration::from_secs(5)))
                    .map_err(|e| e.to_string())?,
            );
        }
        Ok(service)
    }

    pub(super) fn wake(&self) -> ChannelWake {
        self.wake.clone()
    }

    pub fn shutdown(&mut self) -> Result<(), String> {
        self.cancel.store(true, Ordering::Release);
        self.wake.notify();
        let mut panicked = false;
        for worker in self.workers.drain(..) {
            panicked |= worker.join().is_err();
        }
        let recovery = recover(&self.state);
        #[cfg(target_os = "linux")]
        crate::agent_status_model::set_worker_lifetime_lease(None);
        self.ownership.take();
        if panicked {
            return Err("channel worker panicked".to_owned());
        }
        recovery
    }
}

impl Drop for ChannelService {
    fn drop(&mut self) {
        if self.ownership.is_some()
            && let Err(error) = self.shutdown()
        {
            eprintln!("dashboard: {error}");
        }
    }
}

fn recover(state: &Path) -> Result<(), String> {
    if !state.join("namedrop.sqlite3").is_file() {
        return Ok(());
    }
    let now = crate::SystemClock.now_unix_ms();
    let namespaces = StateStore::open(state)
        .and_then(|mut store| {
            store.transaction(|tx| {
                // Updating restarts this server with no action from the person,
                // and activation only runs when they ask for it, so old flags
                // clear here, before Channels shows its first frame.
                let by = crate::MutationAttribution::new(
                    crate::ActorKind::Plugin,
                    "namedrop-channels",
                    now,
                );
                tx.resolve_answer_attention(&by)?;
                tx.judge_open_disposition_warnings(&by)?;
                tx.judge_all_unbound_attempts(&by)?;
                tx.fail_abandoned_channels(now)
            })
        })
        .map_err(|e| e.to_string())?;
    for ns in namespaces {
        drain(state, &ns);
    }
    Ok(())
}

fn worker(state: &Path, cancel: &AtomicBool, wake: &ChannelWake, fallback: Duration) {
    let mut store = None;
    while !cancel.load(Ordering::Acquire) {
        let generation = wake.generation();
        let claim = (|| {
            if store.is_none() {
                if !state.join("namedrop.sqlite3").is_file() {
                    return Ok(None);
                }
                store = Some(StateStore::open(state)?);
            }
            store
                .as_mut()
                .unwrap()
                .transaction(|tx| tx.claim_channel(crate::SystemClock.now_unix_ms()))
        })();
        let claim = match claim {
            Ok(Some(claim)) => claim,
            Ok(None) => {
                wake.wait(generation, cancel, fallback);
                continue;
            }
            Err(error) => {
                eprintln!("dashboard: channel claim: {error}");
                wake.wait(generation, cancel, fallback);
                continue;
            }
        };
        let store = store.as_mut().unwrap();
        let rows = match store.channel_context(&claim) {
            Ok(rows) => rows,
            Err(error) => {
                eprintln!("dashboard: channel context: {error}");
                None
            }
        };
        let run = if let Some(rows) = rows.filter(|_| !cancel.load(Ordering::Acquire)) {
            crate::agent_status_model::channel_router::route_with_cancel(
                crate::agent_status_model::channel_router::RouterRequest {
                    host: claim.host,
                    rows: &rows,
                    source: &claim.draft.authored_source,
                    candidates: &claim.candidates,
                },
                cancel,
            )
        } else {
            crate::agent_status_model::channel_router::RouteRun {
                decision: crate::agent_status_model::channel_router::RoutingDecision::Failed {
                    router: "context".to_owned(),
                },
                attempt: None,
            }
        };
        if let Some(attempt) = run.attempt {
            super::snapshot::channel_routing::record_attempt(
                state,
                &claim.namespace,
                &claim.draft.request_id,
                &crate::IdGenerator::next_id(&crate::ProcessIds::default(), "router-call"),
                attempt,
            );
        }
        use crate::agent_status_model::channel_router::RoutingDecision;
        let (selected, failed, note) = match run.decision {
            RoutingDecision::Unrouted => (Vec::new(), false, None),
            RoutingDecision::Failed { router } => (
                Vec::new(),
                true,
                Some(serde_json::json!({"router":router,"router_failed":true})),
            ),
            RoutingDecision::Routed {
                to,
                reason,
                router,
                usage,
            } => {
                drop(usage);
                (
                    to,
                    false,
                    Some(
                        serde_json::json!({"reason":reason,"router":router,"router_failed":false}),
                    ),
                )
            }
        };
        if let Some(note) = note {
            let mut receipt = serde_json::json!({"message_id":claim.draft.message_id.as_str(),"channel":{"state":if failed {"router_failed"} else {"unrouted"}},"_channel_routing_note":note});
            super::snapshot::channel_routing::finish_post(state, &claim.namespace, &mut receipt);
        }
        // A failed commit retries the same decision; it never reruns the model.
        loop {
            let completed = store.transaction(|tx| {
                tx.finish_channel(
                    &claim,
                    &selected,
                    failed || cancel.load(Ordering::Acquire),
                    crate::SystemClock.now_unix_ms(),
                )
            });
            match completed {
                Ok(true) => {
                    if !cancel.load(Ordering::Acquire) {
                        drain(state, &claim.namespace);
                    }
                    break;
                }
                Ok(false) => break,
                Err(error) => {
                    eprintln!("dashboard: channel completion: {error}");
                    if cancel.load(Ordering::Acquire) {
                        break;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }
}

pub(super) fn drain(state: &Path, namespace: &StateNamespace) {
    let herdr =
        crate::ProcessHerdr::default().with_socket(Some(namespace.session_socket_id.clone()));
    let ids = crate::ProcessIds::default();
    let runtime = crate::LifecycleRuntime {
        state_directory: state,
        home_directory: None,
        agent_config: crate::AgentConfigDirectories::default(),
        cwd: Path::new("/"),
        workspace_id: Some(namespace.space_id.as_str()),
        current_pane_id: None,
        session_socket_id: &namespace.session_socket_id,
        mux: &herdr,
        clock: &crate::SystemClock,
        ids: &ids,
    };
    let receipt = serde_json::json!({"drain":crate::drain_receipt(&runtime)});
    if let Err(error) = super::http::wake_mailboxes_after_send(state, namespace, &receipt) {
        eprintln!("dashboard: channel wake: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "channel-{name}-{}",
            crate::IdGenerator::next_id(&crate::ProcessIds::default(), "test")
        ));
        std::fs::create_dir_all(root.join("dashboard")).unwrap();
        root
    }

    fn open_terminal_relay_items(store: &mut StateStore, ns: &StateNamespace) -> Vec<String> {
        store
            .transaction(|tx| tx.unresolved_attention_items(ns))
            .unwrap()
            .into_iter()
            .filter(|item| item.source_kind == crate::AttentionSourceKind::TerminalRelay)
            .map(|item| item.source_id)
            .collect()
    }

    #[test]
    fn starting_clears_flags_left_on_answers_and_keeps_scraped_and_other_warnings() {
        let root = state_root("backlog");
        let answer = StateNamespace::for_space("socket", "answer");
        let scraped = StateNamespace::for_space("socket", "scraped");
        let warning = crate::AttentionItemDraft {
            id: crate::AttentionItemId::new("unanswered"),
            source_kind: crate::AttentionSourceKind::AgentDisposition,
            source_id: "attempt".to_owned(),
            summary: "An agent ended its turn without replying.".to_owned(),
        };
        let at = crate::MutationAttribution::new(crate::ActorKind::Plugin, "test", 30);
        let mut store = StateStore::open(&root).unwrap();
        for (ns, armed) in [(&answer, false), (&scraped, true)] {
            crate::store::seed_flagged_relay(&mut store, ns, armed, "An ending");
            store
                .transaction(|tx| tx.open_attention_item(ns, &warning, &at))
                .unwrap();
        }

        let mut service = ChannelService::start(&root).unwrap();
        service.shutdown().unwrap();

        assert!(open_terminal_relay_items(&mut store, &answer).is_empty());
        assert_eq!(
            open_terminal_relay_items(&mut store, &scraped),
            ["terminal-relay:attempt"]
        );
        for ns in [&answer, &scraped] {
            let items = store
                .transaction(|tx| tx.unresolved_attention_items(ns))
                .unwrap();
            assert!(
                items
                    .iter()
                    .any(|item| item.source_kind == crate::AttentionSourceKind::AgentDisposition),
                "{items:?}"
            );
        }

        let facts = || -> i64 {
            rusqlite::Connection::open(root.join("namedrop.sqlite3"))
                .unwrap()
                .query_row("SELECT COUNT(*) FROM transition_facts", [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        let before = facts();
        let mut successor = ChannelService::start(&root).unwrap();
        successor.shutdown().unwrap();
        assert_eq!(facts(), before, "a second run changes nothing");
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Every ND-DELIVERY-003 warning in `ns` as (summary, acknowledged).
    fn disposition_warnings(root: &Path, ns: &StateNamespace) -> Vec<(String, bool)> {
        rusqlite::Connection::open(root.join("namedrop.sqlite3"))
            .unwrap()
            .prepare(
                "SELECT summary, resolved_at_ms IS NOT NULL FROM attention_items
                 WHERE worktree_id = ?1 AND source_kind = 'agent_disposition'",
            )
            .unwrap()
            .query_map([ns.space_id.as_str()], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// An earlier release warned that an agent didn't answer while its
    /// answer was held. Starting Channels withdraws that warning while the
    /// session can still answer, and says why when it can't.
    #[test]
    fn starting_judges_old_warnings_on_held_answers_again_and_keeps_the_rest() {
        let root = state_root("held-warnings");
        let mut store = StateStore::open(&root).unwrap();
        let [live, replaced, acknowledged, not_held] =
            ["live", "replaced", "acknowledged", "not-held"]
                .map(|space| StateNamespace::for_space("socket", space));
        for ns in [&live, &replaced, &acknowledged, &not_held] {
            crate::store::seed_unanswered(&mut store, ns, ns == &acknowledged);
        }
        store
            .transaction(|tx| {
                tx.observe_agent_session(
                    &replaced,
                    "observation-replaced",
                    &crate::PaneAddressId::new("agent"),
                    &crate::AgentSessionId::new("session-new"),
                    "idle",
                    &crate::MutationAttribution::new(crate::ActorKind::Plugin, "test", 20),
                )
            })
            .unwrap();
        let unanswered = disposition_warnings(&root, &not_held);
        assert_eq!(unanswered.len(), 1);
        assert!(
            unanswered[0].0.contains("ended its turn without a reply"),
            "{unanswered:?}"
        );
        for ns in [&live, &replaced, &acknowledged] {
            assert_eq!(disposition_warnings(&root, ns).len(), 1);
            crate::final_reply::hold_answer(&root, ns, "session", "attempt");
        }
        let acknowledged_before = disposition_warnings(&root, &acknowledged);

        let mut service = ChannelService::start(&root).unwrap();
        service.shutdown().unwrap();

        assert_eq!(
            disposition_warnings(&root, &live),
            [],
            "a warning on an answer that can still arrive stayed"
        );
        let lost = disposition_warnings(&root, &replaced);
        assert_eq!(lost.len(), 1, "{lost:?}");
        assert!(
            lost[0].0.contains("A new session took the agent's name") && !lost[0].1,
            "{lost:?}"
        );
        assert_eq!(
            disposition_warnings(&root, &acknowledged),
            acknowledged_before
        );
        assert_eq!(disposition_warnings(&root, &not_held), unanswered);

        let facts = || -> i64 {
            rusqlite::Connection::open(root.join("namedrop.sqlite3"))
                .unwrap()
                .query_row("SELECT COUNT(*) FROM transition_facts", [], |row| {
                    row.get(0)
                })
                .unwrap()
        };
        let before = facts();
        let mut successor = ChannelService::start(&root).unwrap();
        successor.shutdown().unwrap();
        assert_eq!(facts(), before, "a second run changes nothing");
        assert_eq!(disposition_warnings(&root, &replaced), lost);
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A drain judges an address that lost its pane only for a few minutes
    /// past the grace. Starting Channels judges one whose pane went long
    /// before, as on a machine that slept through those minutes.
    #[test]
    fn starting_flags_a_held_answer_whose_pane_went_long_ago() {
        let root = state_root("held-long-unbound");
        let mut store = StateStore::open(&root).unwrap();
        let ns = StateNamespace::for_space("socket", "gone");
        crate::final_reply::hold_answer(&root, &ns, "session", "attempt");
        crate::store::seed_unanswered(&mut store, &ns, false);
        store
            .transaction(|tx| {
                tx.unbind_pane(
                    &ns,
                    &crate::PaneAddressId::new("agent"),
                    &crate::MutationAttribution::new(crate::ActorKind::Plugin, "test", 20),
                )
            })
            .unwrap();
        assert_eq!(disposition_warnings(&root, &ns), []);

        let mut service = ChannelService::start(&root).unwrap();
        service.shutdown().unwrap();

        let lost = disposition_warnings(&root, &ns);
        assert_eq!(lost.len(), 1, "{lost:?}");
        assert!(lost[0].0.contains("pane has been gone"), "{lost:?}");
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A warning that the agent's send failed is no judgement of whether it
    /// answered, so neither starting Channels nor a session change withdraws
    /// or rewords it.
    #[test]
    fn a_failed_send_warning_on_a_held_answer_outlives_a_start_and_a_session_change() {
        let root = state_root("held-failed-send");
        let mut store = StateStore::open(&root).unwrap();
        let [failed, judged] =
            ["failed", "judged"].map(|space| StateNamespace::for_space("socket", space));
        let by = crate::MutationAttribution::new(crate::ActorKind::Plugin, "test", 20);
        for ns in [&failed, &judged] {
            crate::store::seed_unanswered(&mut store, ns, false);
            crate::final_reply::hold_answer(&root, ns, "session", "attempt");
        }
        store
            .transaction(|tx| {
                tx.flag_failed_settle(
                    &failed,
                    &crate::DeliveryAttemptId::new("attempt"),
                    crate::store::LostSettle::QueuedReply,
                    "the store was busy",
                    &by,
                )
            })
            .unwrap();
        let refused = disposition_warnings(&root, &failed);
        assert_eq!(refused.len(), 1, "{refused:?}");
        assert!(
            refused[0].0.contains("queued reply was refused"),
            "{refused:?}"
        );

        let mut service = ChannelService::start(&root).unwrap();
        service.shutdown().unwrap();
        assert_eq!(
            disposition_warnings(&root, &judged),
            [],
            "a judgement's warning on an answer that can still arrive stayed"
        );
        assert_eq!(
            disposition_warnings(&root, &failed),
            refused,
            "starting Channels changed the warning that a send failed"
        );

        store
            .transaction(|tx| {
                for ns in [&failed, &judged] {
                    tx.observe_agent_session(
                        ns,
                        "observation-new",
                        &crate::PaneAddressId::new("agent"),
                        &crate::AgentSessionId::new("session-new"),
                        "idle",
                        &by,
                    )?;
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(
            disposition_warnings(&root, &failed),
            refused,
            "a session change changed the warning that a send failed"
        );
        let lost = disposition_warnings(&root, &judged);
        assert!(
            lost.len() == 1 && lost[0].0.contains("A new session took the agent's name"),
            "{lost:?}"
        );
        drop(store);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_store_migrated_from_schema_six_keeps_its_scraped_flag_and_clears_an_answers() {
        let root = state_root("schema6-backlog");
        let database = rusqlite::Connection::open(root.join("namedrop.sqlite3")).unwrap();
        database.pragma_update(None, "foreign_keys", true).unwrap();
        for sql in [
            include_str!("../../tests/fixtures/schema6/schema.sql"),
            include_str!("../../tests/fixtures/schema6/common.sql"),
            include_str!("../../tests/fixtures/schema6/experimental.sql"),
        ] {
            database.execute_batch(sql).unwrap();
        }
        let store = StateStore::open(&root).unwrap();
        database
            .execute_batch(
                "INSERT INTO collaboration_messages (
                     session_socket_id, worktree_id, message_id, thread_id, thread_order,
                     in_reply_to, author_address_id, author_alias_snapshot, author_kind,
                     author_session_snapshot, request_id, authored_source, body,
                     grammar_version, created_at_ms)
                 SELECT session_socket_id, worktree_id, 'terminal-relay:unarmed', thread_id,
                     thread_order + 1, in_reply_to, author_address_id, author_alias_snapshot,
                     author_kind, author_session_snapshot, 'terminal-relay:unarmed',
                     authored_source, body, grammar_version, created_at_ms
                 FROM collaboration_messages WHERE message_id='terminal-relay:dm-attempt';
                 INSERT INTO attention_items
                 SELECT session_socket_id, worktree_id, 'unarmed-attention', source_kind,
                     'terminal-relay:unarmed', summary, opened_at_ms, NULL
                 FROM attention_items WHERE attention_item_id='relay-attention';
                 INSERT INTO order_counters
                 VALUES ('test-schema-socket','home','worktree_transition','',3);",
            )
            .unwrap();
        let open = || -> Vec<String> {
            database
                .prepare(
                    "SELECT source_id FROM attention_items
                     WHERE source_kind='terminal_relay' AND resolved_at_ms IS NULL
                     ORDER BY source_id",
                )
                .unwrap()
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        assert_eq!(
            open(),
            ["terminal-relay:dm-attempt", "terminal-relay:unarmed"]
        );

        let mut service = ChannelService::start(&root).unwrap();
        service.shutdown().unwrap();

        assert_eq!(open(), ["terminal-relay:dm-attempt"]);
        drop((store, database));
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn notification_between_probe_and_wait_is_not_lost() {
        let wake = ChannelWake::default();
        let observed = wake.generation();
        wake.notify();
        let start = std::time::Instant::now();
        wake.wait(observed, &AtomicBool::new(false), Duration::from_secs(2));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn one_notification_wakes_every_waiter() {
        let wake = ChannelWake::default();
        let (parked, parking) = std::sync::mpsc::channel();
        let (sent, received) = std::sync::mpsc::channel();
        let workers: Vec<_> = (0..4)
            .map(|_| {
                let wake = wake.clone();
                let parked = parked.clone();
                let sent = sent.clone();
                thread::spawn(move || {
                    let generation = wake.0.generation.lock().unwrap();
                    let observed = *generation;
                    parked.send(()).unwrap();
                    drop(
                        wake.0
                            .ready
                            .wait_timeout_while(generation, Duration::from_secs(2), |generation| {
                                *generation == observed
                            })
                            .unwrap(),
                    );
                    sent.send(()).unwrap();
                })
            })
            .collect();
        for _ in 0..4 {
            parking.recv_timeout(Duration::from_secs(1)).unwrap();
        }
        wake.notify();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        for _ in 0..4 {
            received
                .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
                .unwrap();
        }
        for worker in workers {
            worker.join().unwrap();
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn idle_workers_keep_connections_across_probes_and_cancel_promptly() {
        let root = std::env::temp_dir().join(format!(
            "channel-idle-{}",
            crate::IdGenerator::next_id(&crate::ProcessIds::default(), "test")
        ));
        std::fs::create_dir_all(root.join("dashboard")).unwrap();
        drop(StateStore::open(&root).unwrap());
        let mut service = ChannelService::start(&root).unwrap();
        let count = || {
            std::fs::read_dir("/proc/self/fd")
                .unwrap()
                .filter_map(Result::ok)
                .filter(|fd| {
                    std::fs::read_link(fd.path())
                        .is_ok_and(|path| path == root.join("namedrop.sqlite3"))
                })
                .count()
        };
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while count() != 4 {
            assert!(
                std::time::Instant::now() < deadline,
                "four idle workers must retain their database connections"
            );
            thread::sleep(Duration::from_millis(10));
        }
        use std::{ffi::CString, io::Read, os::fd::FromRawFd};
        let watcher = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(watcher >= 0);
        let mut watcher = unsafe { File::from_raw_fd(watcher) };
        let database =
            CString::new(root.join("namedrop.sqlite3").as_os_str().as_encoded_bytes()).unwrap();
        assert!(
            unsafe {
                libc::inotify_add_watch(watcher.as_raw_fd(), database.as_ptr(), libc::IN_OPEN)
            } >= 0
        );
        for _ in 0..3 {
            service.wake.notify();
            thread::sleep(Duration::from_millis(30));
            assert_eq!(count(), 4);
            let mut events = [0; 4096];
            assert_eq!(
                watcher.read(&mut events).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "idle probes must reuse their open connections"
            );
        }
        let start = std::time::Instant::now();
        service.shutdown().unwrap();
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "shutdown must wake the five-second idle wait"
        );
        assert_eq!(count(), 0);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unused_service_does_not_initialize_database() {
        let root = std::env::temp_dir().join(format!(
            "channel-unused-{}-{}",
            std::process::id(),
            crate::IdGenerator::next_id(&crate::ProcessIds::default(), "test")
        ));
        std::fs::create_dir_all(root.join("dashboard")).unwrap();
        let mut service = ChannelService::start(&root).unwrap();
        assert!(StateStore::open_read_only(&root).unwrap().is_none());
        service.shutdown().unwrap();
        assert!(StateStore::open_read_only(&root).unwrap().is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_error_still_releases_manager_ownership() {
        // Open the manager descriptor after exec so parallel tests cannot
        // retain a forked copy while this fixture checks immediate release.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "dashboard::channel_service::tests::recovery_error_ownership_fixture",
                "--ignored",
                "--nocapture",
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore = "subprocess fixture for manager ownership assertions"]
    fn recovery_error_ownership_fixture() {
        let root = std::env::temp_dir().join(format!(
            "channel-manager-{}-{}",
            std::process::id(),
            crate::IdGenerator::next_id(&crate::ProcessIds::default(), "test")
        ));
        std::fs::create_dir_all(root.join("dashboard")).unwrap();
        drop(StateStore::open(&root).unwrap());
        let mut service = ChannelService::start(&root).unwrap();
        let claim_from_process = |held: bool| {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "dashboard::channel_service::tests::manager_ownership_claim_fixture",
                    "--ignored",
                    "--nocapture",
                ])
                .env("NAMEDROP_TEST_MANAGER_STATE", &root)
                .env("NAMEDROP_TEST_MANAGER_HELD", held.to_string())
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        claim_from_process(true);
        let database = root.join("namedrop.sqlite3");
        std::fs::rename(&database, root.join("saved.sqlite3")).unwrap();
        std::fs::write(&database, b"invalid database").unwrap();
        assert!(service.shutdown().is_err());
        assert!(service.ownership.is_none());
        let claim = OpenOptions::new()
            .read(true)
            .write(true)
            .open(root.join("dashboard/channel-manager.lock"))
            .unwrap();
        let result = unsafe { libc::flock(claim.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        let error = std::io::Error::last_os_error();
        assert_eq!(result, 0, "{error}");
        drop(claim);
        claim_from_process(false);
        std::fs::rename(root.join("saved.sqlite3"), &database).unwrap();
        let mut successor = ChannelService::start(&root).unwrap();
        assert!(successor.ownership.is_some());
        successor.shutdown().unwrap();
        assert!(successor.ownership.is_none());
        drop(service);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    #[ignore = "subprocess fixture for an independent manager ownership claim"]
    fn manager_ownership_claim_fixture() {
        let root = std::env::var_os("NAMEDROP_TEST_MANAGER_STATE").unwrap();
        let held: bool = std::env::var("NAMEDROP_TEST_MANAGER_HELD")
            .unwrap()
            .parse()
            .unwrap();
        let claim = OpenOptions::new()
            .read(true)
            .write(true)
            .open(Path::new(&root).join("dashboard/channel-manager.lock"))
            .unwrap();
        let result = unsafe { libc::flock(claim.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        let error = std::io::Error::last_os_error();
        if held {
            assert_eq!(result, -1);
            assert_eq!(error.raw_os_error(), Some(libc::EWOULDBLOCK));
        } else {
            assert_eq!(result, 0, "{error}");
        }
    }
}
