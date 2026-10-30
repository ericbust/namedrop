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
    let namespaces = StateStore::open(state)
        .and_then(|mut store| {
            store.transaction(|tx| tx.fail_abandoned_channels(crate::SystemClock.now_unix_ms()))
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
        herdr: &herdr,
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
