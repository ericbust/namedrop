use super::*;
use crate::mux::{PaneId, PaneSlot};
use crate::{
    ActorKind, MutationAttribution, PaneAddressDraft, PaneAddressId, PaneBindingId, PaneRoleId,
    StateNamespace,
};
use std::{
    collections::VecDeque,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

const REX: &str = "rex:/tmp/namedrop-bridge-tests.sock";

static NEXT: AtomicU64 = AtomicU64::new(0);

struct State(PathBuf);

impl State {
    /// `session:s` and `session:t` activated, and `session:u` deactivated.
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "namedrop-rex-bridge-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        let attribution = MutationAttribution::new(ActorKind::Human, "test", 10);
        StateStore::open(&path)
            .unwrap()
            .transaction(|transaction| {
                for space in ["session:s", "session:t", "session:u"] {
                    transaction
                        .activate_worktree(&StateNamespace::for_space(REX, space), &attribution)?;
                }
                transaction.activate_worktree(
                    &StateNamespace::for_space("/tmp/h.sock", "w1"),
                    &attribution,
                )?;
                transaction
                    .deactivate_worktree(&StateNamespace::for_space(REX, "session:u"), &attribution)
            })
            .unwrap();
        Self(path)
    }

    /// The events queued for `space`, in queue order.
    fn queued(&self, space: &str) -> Vec<MuxEvent> {
        let directory = status_queue::directory(&self.0, REX, space);
        let mut files: Vec<PathBuf> = fs::read_dir(directory)
            .map(|entries| entries.map(|entry| entry.unwrap().path()).collect())
            .unwrap_or_default();
        files.sort();
        files
            .iter()
            .map(|file| {
                match Marker::parse(serde_json::from_slice(&fs::read(file).unwrap()).unwrap())
                    .unwrap()
                {
                    Marker::Event(event) => event,
                    marker => panic!("{marker:?}"),
                }
            })
            .collect()
    }
}

impl Drop for State {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn endpoint() -> Endpoint {
    Endpoint::parse(REX)
}

fn slot(space: &str, pane: &str) -> PaneSlot {
    PaneSlot {
        space: SpaceId::new(space),
        pane: PaneId::new(pane),
        tab: None,
    }
}

fn resync(space: &str) -> MuxEvent {
    MuxEvent::Resync(SpaceId::new(space))
}

/// A connection's script: its `host_id` and what each `next` answers, after
/// which it ends.
struct Scripted<'w> {
    host: Option<&'static str>,
    nexts: VecDeque<Next>,
    stop: &'w Wake,
    /// Once the script runs out, waits for a stop instead of ending, and
    /// then hands on this last event that arrived as the stop did. It wakes
    /// every 20 ms meanwhile, as a real stream wakes at its idle time.
    hold: Option<MuxEvent>,
}

impl Stream for Scripted<'_> {
    fn host_id(&self) -> Option<&str> {
        self.host
    }

    fn next(&mut self, within: Duration) -> Next {
        if let Some(next) = self.nexts.pop_front() {
            return next;
        }
        if self.hold.is_none() {
            return Next::Ended("the script ran out".into());
        }
        if self.stop.sleep(within.min(Duration::from_millis(20))) {
            self.hold
                .take()
                .map_or(Next::Ended("stopped".into()), Next::Event)
        } else {
            Next::Quiet
        }
    }
}

/// One connection of a script: its `host_id`, its answers, and the event
/// it hands on once a stop ends its wait, if it waits for one.
type Connection = (Option<&'static str>, Vec<Next>, Option<MuxEvent>);

/// Each `open` takes the next connection, and a server out of them is away.
struct Script {
    connections: Mutex<VecDeque<Connection>>,
    opened: AtomicU64,
}

impl Script {
    fn new(connections: Vec<Connection>) -> Self {
        Self {
            connections: Mutex::new(connections.into()),
            opened: AtomicU64::new(0),
        }
    }
}

impl Server for Script {
    fn open<'w>(&'w self, stop: &'w Wake) -> Result<Box<dyn Stream + 'w>, String> {
        let (host, nexts, hold) = self
            .connections
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| "connection refused".to_owned())?;
        self.opened.fetch_add(1, Ordering::Relaxed);
        Ok(Box::new(Scripted {
            host,
            nexts: nexts.into(),
            stop,
            hold,
        }))
    }
}

const NO_WAIT: Away = Away {
    delays: &[Duration::ZERO],
    give_up: Duration::ZERO,
};

/// A worker that exits at once.
fn worker() -> Result<Child, String> {
    std::process::Command::new("true")
        .spawn()
        .map_err(|error| error.to_string())
}

/// Runs the bridge over `script`, away from its server as `away` says, and
/// returns the spaces whose worker each flush started, in order, and each
/// worker's pid.
fn bridged_away(
    state: &State,
    script: &Script,
    stop: &Wake,
    away: &Away,
) -> (Vec<String>, Vec<u32>) {
    let lock = Lock::take(&state.0, &endpoint()).unwrap().unwrap();
    let (mut spawned, mut pids) = (Vec::new(), Vec::new());
    bridge(
        &state.0,
        &endpoint(),
        script,
        stop,
        &lock,
        &mut |space| {
            spawned.push(space.as_str().to_owned());
            let worker = worker()?;
            pids.push(worker.id());
            Ok(worker)
        },
        away,
    );
    (spawned, pids)
}

fn bridged(state: &State, script: &Script, stop: &Wake) -> Vec<String> {
    bridged_away(state, script, stop, &NO_WAIT).0
}

#[test]
fn a_second_bridge_for_a_server_exits_at_once_and_never_dials() {
    let state = State::new();
    let socket = state.0.join("r.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let endpoint = Endpoint::parse(&format!("rex:{}", socket.display()));
    let _held = Lock::take(&state.0, &endpoint).unwrap().unwrap();
    let started = Instant::now();
    assert_eq!(run(&state.0, &endpoint), ExitCode::SUCCESS);
    assert!(started.elapsed() < Duration::from_secs(1));
    assert_eq!(
        listener.accept().map_err(|error| error.kind()).err(),
        Some(io::ErrorKind::WouldBlock),
        "the second bridge dialed the server"
    );
    assert_eq!(
        ensure(&state.0, &endpoint),
        Ok(false),
        "a held lock starts no second bridge"
    );
}

#[test]
fn each_connection_queues_one_resync_per_activated_space_and_a_server_never_reached_ends_it() {
    let state = State::new();
    let stop = Wake::new().unwrap();
    let script = Script::new(vec![
        (Some("host-a"), Vec::new(), None),
        (Some("host-a"), Vec::new(), None),
    ]);
    let spawned = bridged(&state, &script, &stop);
    assert_eq!(script.opened.load(Ordering::Relaxed), 2);
    assert_eq!(
        spawned,
        ["session:s", "session:t", "session:s", "session:t"]
    );
    for space in ["session:s", "session:t"] {
        assert_eq!(state.queued(space), [resync(space), resync(space)]);
    }
    assert!(state.queued("session:u").is_empty());
    assert!(state.queued("w1").is_empty());
    let empty = State(state.0.join("never"));
    assert!(bridged(&empty, &Script::new(Vec::new()), &stop).is_empty());
}

/// The bridge lives for days, so every worker it starts is reaped once it
/// exits rather than left a zombie.
#[test]
fn every_worker_the_bridge_starts_is_reaped_once_it_exits() {
    let state = State::new();
    let stop = Wake::new().unwrap();
    let script = Script::new(
        (0..20)
            .map(|_| (Some("host-a"), Vec::new(), None))
            .collect(),
    );
    let (spawned, pids) = bridged_away(&state, &script, &stop, &NO_WAIT);
    assert_eq!(spawned.len(), 40);
    let deadline = Instant::now() + Duration::from_secs(5);
    let left = loop {
        // SAFETY: signal 0 only asks whether each process still exists, and
        // an exited child that nobody reaped still does.
        let left: Vec<u32> = pids
            .iter()
            .copied()
            .filter(|pid| unsafe { libc::kill(*pid as libc::pid_t, 0) } == 0)
            .collect();
        if left.is_empty() || Instant::now() >= deadline {
            break left;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(left.is_empty(), "unreaped workers: {left:?}");
}

#[test]
fn a_connection_to_another_server_at_the_socket_queues_no_resync() {
    let state = State::new();
    let stop = Wake::new().unwrap();
    let script = Script::new(vec![
        (Some("host-a"), Vec::new(), None),
        (
            Some("host-b"),
            vec![Next::Event(MuxEvent::PaneClosed(slot(
                "session:s",
                "block:1",
            )))],
            None,
        ),
    ]);
    bridged(&state, &script, &stop);
    assert_eq!(
        state.queued("session:s"),
        [
            resync("session:s"),
            MuxEvent::PaneClosed(slot("session:s", "block:1"))
        ]
    );
    assert_eq!(state.queued("session:t"), [resync("session:t")]);
    let known: KnownServer = serde_json::from_slice(
        &fs::read(
            state
                .0
                .join("rex/servers")
                .join(format!("{}.json", scope(&endpoint()))),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(known.host_id, "host-a", "the first host_id stays");
}

#[test]
fn a_flush_queues_each_activated_space_s_events_and_starts_its_worker_once() {
    let state = State::new();
    let stop = Wake::new().unwrap();
    let status = |space, pane| Next::Event(MuxEvent::AgentStatusChanged(slot(space, pane)));
    let script = Script::new(vec![(
        None,
        vec![
            status("session:s", "block:1"),
            status("session:s", "block:1"),
            Next::Event(MuxEvent::PaneAppeared(slot("session:s", "block:2"))),
            Next::Quiet,
            status("session:t", "block:3"),
            status("session:u", "block:4"),
            Next::Event(MuxEvent::PaneClosed(slot("session:s", "block:2"))),
        ],
        None,
    )]);
    let spawned = bridged(&state, &script, &stop);
    assert_eq!(
        spawned,
        ["session:s", "session:t"],
        "one flush, one worker each"
    );
    assert_eq!(
        state.queued("session:s"),
        [
            resync("session:s"),
            MuxEvent::PaneAppeared(slot("session:s", "block:2")),
            MuxEvent::PaneClosed(slot("session:s", "block:2")),
        ]
    );
    assert_eq!(state.queued("session:t"), [resync("session:t")]);
    assert!(state.queued("session:u").is_empty());
}

/// SIGTERM lands while the bridge waits between flushes, as one more event
/// arrives. That event is queued, and the bridge returns without dialing
/// again.
#[test]
fn sigterm_stops_the_bridge_between_flushes() {
    let state = State::new();
    let stop = stop_on_signals().unwrap();
    let script = Script::new(vec![
        (
            None,
            vec![Next::Event(MuxEvent::PaneClosed(slot(
                "session:s",
                "block:1",
            )))],
            Some(MuxEvent::PaneAppeared(slot("session:s", "block:2"))),
        ),
        (None, Vec::new(), None),
    ]);
    let started = Instant::now();
    let spawned = std::thread::scope(|scope| {
        scope.spawn(|| {
            std::thread::sleep(Duration::from_millis(300));
            // SAFETY: the bridge's handler is installed, so the signal stops
            // it rather than this process.
            unsafe { libc::kill(libc::getpid(), libc::SIGTERM) };
        });
        bridged(&state, &script, stop)
    });
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(script.opened.load(Ordering::Relaxed), 1);
    assert_eq!(spawned, ["session:s", "session:t", "session:s"]);
    assert_eq!(
        state.queued("session:s"),
        [
            resync("session:s"),
            MuxEvent::PaneClosed(slot("session:s", "block:1")),
            MuxEvent::PaneAppeared(slot("session:s", "block:2")),
        ]
    );
}

/// A real `block_closed` line, decoded by the bridge's real stream over a
/// scripted server, flushed to the space's queue, and handled as the status
/// worker handles a marker, retires the block's name.
#[test]
fn a_scripted_block_closed_retires_its_name_through_the_queue() {
    use crate::mux::{fake::FakeMux, rex::events::tests_support};
    let line: serde_json::Value = serde_json::from_str(
        include_str!("../mux/rex/fixtures/events/lifecycle.jsonl")
            .lines()
            .nth(28)
            .unwrap(),
    )
    .unwrap();
    let session = line["payload"]["session_id"].as_str().unwrap().to_owned();
    let block = line["payload"]["event"]["payload"]["block_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let state = State::new();
    let namespace = StateNamespace::for_space(REX, &session);
    let attribution = MutationAttribution::new(ActorKind::Human, "test", 10);
    let address = PaneAddressId::new("address-closed");
    StateStore::open(&state.0)
        .unwrap()
        .transaction(|transaction| {
            transaction.activate_worktree(&namespace, &attribution)?;
            transaction.create_pane_address(
                &namespace,
                &PaneAddressDraft {
                    id: address.clone(),
                    role_id: PaneRoleId::new("role-closed"),
                    role_name: "closed".into(),
                    role_description: "closed test role".into(),
                    alias: "closed".into(),
                },
                &attribution,
            )?;
            transaction.bind_pane(
                &namespace,
                &PaneBindingId::new("binding-closed"),
                &address,
                &block,
                &attribution,
            )
        })
        .unwrap();
    let stop = Wake::new().unwrap();
    let server = tests_support::Scripted::new("host-a", vec![line]);
    struct Once<'s>(Mutex<Option<tests_support::Scripted>>, &'s Path);
    impl Server for Once<'_> {
        fn open<'w>(&'w self, stop: &'w Wake) -> Result<Box<dyn Stream + 'w>, String> {
            let server = self.0.lock().unwrap().take().ok_or("refused")?;
            Ok(Box::new(server.events(self.1, stop)))
        }
    }
    let lock = Lock::take(&state.0, &endpoint()).unwrap().unwrap();
    let mut spawned = Vec::new();
    bridge(
        &state.0,
        &endpoint(),
        &Once(Mutex::new(Some(server)), &state.0),
        &stop,
        &lock,
        &mut |space| {
            spawned.push(space.clone());
            worker()
        },
        &NO_WAIT,
    );
    assert!(spawned.contains(&SpaceId::new(&session)), "{spawned:?}");
    assert_eq!(
        state.queued(&session),
        [
            MuxEvent::Resync(SpaceId::new(&session)),
            MuxEvent::PaneClosed(slot(&session, &block)),
        ]
    );
    let mux = FakeMux::world()
        .endpoint(REX)
        .space(&session)
        .checkout("/repo")
        .build();
    let ids = crate::ProcessIds::default();
    let home = state.0.join("home");
    let runtime = crate::LifecycleRuntime {
        state_directory: &state.0,
        home_directory: Some(&home),
        agent_config: crate::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: None,
        session_socket_id: REX,
        mux: &mux,
        clock: &crate::SystemClock,
        ids: &ids,
    };
    for event in state.queued(&session) {
        crate::handle_mux_event(&runtime, &event).unwrap();
    }
    let names = StateStore::open(&state.0)
        .unwrap()
        .transaction(|transaction| transaction.list_pane_addresses(&namespace))
        .unwrap();
    assert!(names.is_empty(), "{names:?}");
}

/// Deleting the state directory under a running bridge, or replacing it,
/// ends that bridge, so the directory made again gets exactly one.
#[test]
fn a_bridge_stops_once_its_state_directory_is_deleted_or_replaced() {
    for replaced in [false, true] {
        let state = State::new();
        let stop = Wake::new().unwrap();
        let held = || (Some("host-a"), Vec::new(), Some(resync("session:s")));
        let script = Script::new(vec![held(), held()]);
        let done = std::sync::atomic::AtomicBool::new(false);
        let ended = std::thread::scope(|threads| {
            threads.spawn(|| {
                bridged(&state, &script, &stop);
                done.store(true, Ordering::SeqCst);
            });
            std::thread::sleep(Duration::from_millis(200));
            assert!(!done.load(Ordering::SeqCst), "the bridge ended early");
            fs::remove_dir_all(&state.0).unwrap();
            if replaced {
                fs::create_dir_all(state.0.join("rex-bridge")).unwrap();
                fs::write(
                    state
                        .0
                        .join("rex-bridge")
                        .join(format!("{}.lock", scope(&endpoint()))),
                    "",
                )
                .unwrap();
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while !done.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            let ended = done.load(Ordering::SeqCst);
            stop.wake();
            ended
        });
        assert!(ended, "replaced: {replaced}");
        assert_eq!(script.opened.load(Ordering::Relaxed), 1);
        let first = Lock::take(&state.0, &endpoint()).unwrap();
        assert!(first.is_some());
        assert!(Lock::take(&state.0, &endpoint()).unwrap().is_none());
    }
}
