use crate::{Clock, StateNamespace, StateStore};
use std::{
    fs::{File, OpenOptions},
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt, OpenOptionsExt},
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

pub(super) struct ChannelService {
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
            workers: Vec::new(),
            state: state.to_owned(),
            ownership: Some(ownership),
        };
        for index in 0..4 {
            let state = state.to_owned();
            let cancel = service.cancel.clone();
            service.workers.push(
                thread::Builder::new()
                    .name(format!("channel-router-{index}"))
                    .spawn(move || worker(&state, &cancel))
                    .map_err(|e| e.to_string())?,
            );
        }
        Ok(service)
    }

    pub fn shutdown(&mut self) -> Result<(), String> {
        self.cancel.store(true, Ordering::Release);
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
    if StateStore::open_read_only(state)
        .map_err(|e| e.to_string())?
        .is_none()
    {
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

fn worker(state: &Path, cancel: &AtomicBool) {
    while !cancel.load(Ordering::Acquire) {
        let claim = StateStore::open_read_only(state).and_then(|store| {
            let Some(mut store) = store else {
                return Ok(None);
            };
            let pending = store.transaction(|tx| tx.has_pending_channel())?;
            drop(store);
            if !pending {
                return Ok(None);
            }
            StateStore::open(state).and_then(|mut store| {
                store.transaction(|tx| tx.claim_channel(crate::SystemClock.now_unix_ms()))
            })
        });
        let claim = match claim {
            Ok(Some(claim)) => claim,
            Ok(None) => {
                thread::sleep(Duration::from_millis(25));
                continue;
            }
            Err(error) => {
                eprintln!("dashboard: channel claim: {error}");
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        let run = crate::agent_status_model::channel_router::route_with_cancel(
            crate::agent_status_model::channel_router::RouterRequest {
                host: claim.host,
                rows: &claim.rows,
                source: &claim.draft.authored_source,
                candidates: &claim.candidates,
            },
            cancel,
        );
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
            let completed = StateStore::open(state).and_then(|mut store| {
                store.transaction(|tx| {
                    tx.finish_channel(
                        &claim,
                        &selected,
                        failed || cancel.load(Ordering::Acquire),
                        crate::SystemClock.now_unix_ms(),
                    )
                })
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
        let root = std::env::temp_dir().join(format!(
            "channel-manager-{}-{}",
            std::process::id(),
            crate::IdGenerator::next_id(&crate::ProcessIds::default(), "test")
        ));
        std::fs::create_dir_all(root.join("dashboard")).unwrap();
        drop(StateStore::open(&root).unwrap());
        let mut service = ChannelService::start(&root).unwrap();
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
        assert_eq!(
            unsafe { libc::flock(claim.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        drop(claim);
        drop(service);
        std::fs::remove_dir_all(root).unwrap();
    }
}
