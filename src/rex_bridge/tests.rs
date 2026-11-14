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

    /// The events queued for `space`, in queue order. A marker still being
    /// written is not queued yet.
    fn queued(&self, space: &str) -> Vec<MuxEvent> {
        let directory = status_queue::directory(&self.0, REX, space);
        let mut files: Vec<PathBuf> = fs::read_dir(directory)
            .map(|entries| {
                entries
                    .filter_map(|entry| Some(entry.ok()?.path()))
                    .filter(|path| {
                        path.file_name()
                            .is_some_and(|name| name.to_string_lossy().starts_with("pending-"))
                    })
                    .collect()
            })
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

/// Each `open` takes the next dial: a connection, or `None` for a refusal.
/// A server out of dials refuses every one.
struct Script {
    dials: Mutex<VecDeque<Option<Connection>>>,
    opened: AtomicU64,
    /// When each dial came, refused or not.
    dialed: Mutex<Vec<Instant>>,
}

impl Script {
    fn new(connections: Vec<Connection>) -> Self {
        Self::dials(connections.into_iter().map(Some).collect())
    }

    fn dials(dials: Vec<Option<Connection>>) -> Self {
        Self {
            dials: Mutex::new(dials.into()),
            opened: AtomicU64::new(0),
            dialed: Mutex::new(Vec::new()),
        }
    }
}

impl Server for Script {
    fn open<'w>(&'w self, stop: &'w Wake) -> Result<Box<dyn Stream + 'w>, String> {
        self.dialed.lock().unwrap().push(Instant::now());
        let (host, nexts, hold) = self
            .dials
            .lock()
            .unwrap()
            .pop_front()
            .flatten()
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
    steady: Duration::ZERO,
};

/// A worker that exits at once.
fn worker() -> Result<Child, String> {
    std::process::Command::new("true")
        .spawn()
        .map_err(|error| error.to_string())
}

/// Starts workers that exit at once, and keeps the spaces they were
/// started for, in order, and their pids. The bridge reaps them as it
/// reaps real ones. When `replaced` says so, a replaced runtime takes over
/// at the first flush.
#[derive(Default)]
struct FakeHost {
    spawned: Vec<String>,
    pids: Vec<u32>,
    replaced: bool,
    workers: Workers,
}

/// Requests a stop when dropped, so a helper thread that panics still ends
/// the bridge it watches.
struct StopOnDrop<'w>(&'w Wake);

impl Drop for StopOnDrop<'_> {
    fn drop(&mut self) {
        self.0.wake();
    }
}

/// What unwinds out of a bridge whose runtime was replaced, since `exec`
/// never returns.
struct Replaced;

impl Host for FakeHost {
    fn spawn(&mut self, space: &SpaceId) -> Result<Child, String> {
        self.spawned.push(space.as_str().to_owned());
        let worker = worker()?;
        self.pids.push(worker.id());
        Ok(worker)
    }

    fn workers(&self) -> &Workers {
        &self.workers
    }

    fn reexec_if_replaced(&mut self) {
        if self.replaced {
            std::panic::resume_unwind(Box::new(Replaced));
        }
    }
}

/// Runs the bridge over `script`, away from its server as `away` says, on
/// `host`.
fn bridged_with(state: &State, script: &dyn Server, stop: &Wake, away: &Away, host: &mut FakeHost) {
    let lock = Lock::take(&state.0, &endpoint()).unwrap().unwrap();
    bridge(&state.0, &endpoint(), script, stop, &lock, host, away);
}

fn bridged_away(state: &State, script: &Script, stop: &Wake, away: &Away) -> FakeHost {
    let mut workers = FakeHost::default();
    bridged_with(state, script, stop, away, &mut workers);
    workers
}

fn bridged(state: &State, script: &Script, stop: &Wake) -> Vec<String> {
    bridged_away(state, script, stop, &NO_WAIT).spawned
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
/// exits rather than left a zombie, and is then no longer one a re-exec
/// would hand on.
#[test]
fn every_worker_the_bridge_starts_is_reaped_once_it_exits() {
    let state = State::new();
    let stop = Wake::new().unwrap();
    let script = Script::new(
        (0..20)
            .map(|_| (Some("host-a"), Vec::new(), None))
            .collect(),
    );
    let FakeHost {
        spawned,
        pids,
        workers,
        ..
    } = bridged_away(&state, &script, &stop, &NO_WAIT);
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
        if (left.is_empty() && workers.live().is_empty()) || Instant::now() >= deadline {
            break left;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert!(left.is_empty(), "unreaped workers: {left:?}");
    assert_eq!(workers.live(), "", "reaped workers still handed on");
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

impl State {
    /// `session` activated, with the name `closed` bound to `block`.
    fn name(&self, session: &str, block: &str) {
        let namespace = StateNamespace::for_space(REX, session);
        let attribution = MutationAttribution::new(ActorKind::Human, "test", 10);
        let address = PaneAddressId::new("address-closed");
        StateStore::open(&self.0)
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
                    block,
                    &attribution,
                )
            })
            .unwrap();
    }

    /// Handles each event queued for `session`, as its status worker would,
    /// over a server where `session` has no blocks, and returns the aliases
    /// left in it.
    fn handle_queued(&self, session: &str) -> Vec<String> {
        let mux = crate::mux::fake::FakeMux::world()
            .endpoint(REX)
            .space(session)
            .checkout("/repo")
            .build();
        self.handle(session, &self.queued(session), &mux)
    }

    /// Handles `events` in `session` over `mux`, as the status worker
    /// would, and returns the aliases left in it.
    fn handle(
        &self,
        session: &str,
        events: &[MuxEvent],
        mux: &crate::mux::fake::FakeMux,
    ) -> Vec<String> {
        let ids = crate::ProcessIds::default();
        let home = self.0.join("home");
        let runtime = crate::LifecycleRuntime {
            state_directory: &self.0,
            home_directory: Some(&home),
            agent_config: crate::AgentConfigDirectories::default(),
            cwd: Path::new("/repo"),
            workspace_id: None,
            current_pane_id: None,
            session_socket_id: REX,
            mux,
            clock: &crate::SystemClock,
            ids: &ids,
        };
        for event in events {
            crate::handle_mux_event(&runtime, event).unwrap();
        }
        StateStore::open(&self.0)
            .unwrap()
            .transaction(|transaction| {
                transaction.list_pane_addresses(&StateNamespace::for_space(REX, session))
            })
            .unwrap()
            .into_iter()
            .map(|address| address.alias)
            .collect()
    }
}

/// A scratch server's events for `rex move` of the only block of one
/// window into another: `window_closed`, `active_window_changed`, and
/// `session_view_changed`, and no `block_closed`. Over the bridge's real
/// stream they come out as a `Resync` of the session, and the moved block,
/// last seen in the closed window, keeps its name.
#[test]
fn a_block_moved_out_of_a_window_it_was_alone_in_keeps_its_name() {
    use crate::mux::rex::events::tests_support::Scripted;
    const MOVED: &str = "block:01a1d894-8162-7ba4-a7a0-df247750b798";
    let lines: Vec<serde_json::Value> = include_str!("../mux/rex/fixtures/events/move.jsonl")
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let window = |line: &serde_json::Value| {
        line["payload"]["event"]["payload"]["window_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let (closed, into) = (window(&lines[0]), window(&lines[1]));
    let session = lines[0]["payload"]["session_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let state = State::new();
    state.name(&session, MOVED);
    let namespace = StateNamespace::for_space(REX, &session);
    StateStore::open(&state.0)
        .unwrap()
        .transaction(|transaction| {
            transaction.observe_pane_binding_location(
                &namespace,
                &PaneAddressId::new("address-closed"),
                MOVED,
                &closed,
            )
        })
        .unwrap();
    let stop = Wake::new().unwrap();
    let mut stream = Scripted::new("host-a", lines).events(&state.0, &stop);
    let mut decoded = Vec::new();
    while let Next::Event(event) = stream.next(Duration::from_millis(300)) {
        decoded.push(event);
    }
    assert_eq!(decoded, [MuxEvent::Resync(SpaceId::new(&session))]);
    let mux = crate::mux::fake::FakeMux::world()
        .endpoint(REX)
        .space(&session)
        .checkout("/repo")
        .pane(MOVED)
        .tab(&into)
        .tab_label(&into, "shell")
        .build();
    assert_eq!(state.handle(&session, &decoded, &mux), ["closed"]);
}

/// The fixture's `block_closed` line, with its session and block.
fn block_closed() -> (serde_json::Value, String, String) {
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
    (line, session, block)
}

/// Each `open` dials a scripted server over the bridge's real stream once.
struct Once<'s>(
    Mutex<Option<crate::mux::rex::events::tests_support::Scripted>>,
    &'s Path,
);

impl Server for Once<'_> {
    fn open<'w>(&'w self, stop: &'w Wake) -> Result<Box<dyn Stream + 'w>, String> {
        let server = self.0.lock().unwrap().take().ok_or("refused")?;
        Ok(Box::new(server.events(self.1, stop)))
    }
}

/// A real `block_closed` line, decoded by the bridge's real stream over a
/// scripted server, flushed to the space's queue, and handled as the status
/// worker handles a marker, retires the block's name.
#[test]
fn a_scripted_block_closed_retires_its_name_through_the_queue() {
    use crate::mux::rex::events::tests_support::Scripted;
    let (line, session, block) = block_closed();
    let state = State::new();
    state.name(&session, &block);
    let stop = Wake::new().unwrap();
    let server = Once(
        Mutex::new(Some(Scripted::new("host-a", vec![line]))),
        &state.0,
    );
    let mut workers = FakeHost::default();
    bridged_with(&state, &server, &stop, &NO_WAIT, &mut workers);
    assert!(workers.spawned.contains(&session), "{:?}", workers.spawned);
    assert_eq!(
        state.queued(&session),
        [
            MuxEvent::Resync(SpaceId::new(&session)),
            MuxEvent::PaneClosed(slot(&session, &block)),
        ]
    );
    assert!(state.handle_queued(&session).is_empty());
}

/// A replaced runtime takes over at the flush that holds a block's close,
/// so the close is never queued. The new bridge's first connection queues a
/// `Resync`, which retires the block's name all the same.
#[test]
fn a_close_lost_to_a_re_exec_is_recovered_by_the_new_bridge_s_resync() {
    use crate::mux::rex::events::tests_support::Scripted;
    let (line, session, block) = block_closed();
    let state = State::new();
    state.name(&session, &block);
    let stop = Wake::new().unwrap();
    let old = Once(
        Mutex::new(Some(Scripted::new("host-a", vec![line]))),
        &state.0,
    );
    let mut replaced = FakeHost {
        replaced: true,
        ..FakeHost::default()
    };
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        bridged_with(&state, &old, &stop, &NO_WAIT, &mut replaced);
    }));
    assert!(unwound.is_err_and(|payload| payload.is::<Replaced>()));
    assert!(state.queued(&session).is_empty());
    assert!(replaced.spawned.is_empty());
    let new = Once(
        Mutex::new(Some(Scripted::new("host-a", Vec::new()))),
        &state.0,
    );
    bridged_with(&state, &new, &stop, &NO_WAIT, &mut FakeHost::default());
    assert_eq!(
        state.queued(&session),
        [MuxEvent::Resync(SpaceId::new(&session))]
    );
    assert!(state.handle_queued(&session).is_empty());
}

/// While the command gate or the store cannot be read, or a marker cannot
/// be written, a flush queues nothing for the space and keeps its events.
/// They are queued at the next try, a second later, once it can.
#[test]
fn events_a_flush_cannot_queue_stay_pending_until_it_can() {
    fn database(state: &State) -> PathBuf {
        state.0.join("namedrop.sqlite3")
    }
    fn gate(state: &State) -> PathBuf {
        state.0.join("runtime/commands.lock")
    }
    fn queue(state: &State) -> PathBuf {
        status_queue::directory(&state.0, REX, "session:s")
    }
    type Change = fn(&State);
    let blocks: [(&str, Change, Change); 3] = [
        (
            "gate",
            |state| fs::create_dir_all(gate(state)).unwrap(),
            |state| fs::remove_dir(gate(state)).unwrap(),
        ),
        (
            "store",
            |state| {
                fs::rename(database(state), state.0.join("saved")).unwrap();
                fs::write(database(state), "not a database").unwrap();
            },
            |state| fs::rename(state.0.join("saved"), database(state)).unwrap(),
        ),
        (
            "queue",
            |state| {
                fs::create_dir_all(queue(state).parent().unwrap()).unwrap();
                fs::write(queue(state), "not a directory").unwrap();
            },
            |state| fs::remove_file(queue(state)).unwrap(),
        ),
    ];
    for (case, block, unblock) in blocks {
        let state = State::new();
        block(&state);
        let stop = Wake::new().unwrap();
        let appeared = MuxEvent::PaneAppeared(slot("session:s", "block:2"));
        let script = Script::new(vec![(
            None,
            vec![Next::Event(appeared.clone())],
            Some(resync("session:t")),
        )]);
        let (held, waited) = std::thread::scope(|threads| {
            let held = threads.spawn(|| {
                let _stop = StopOnDrop(&stop);
                std::thread::sleep(Duration::from_millis(300));
                let held = state.queued("session:s").is_empty();
                unblock(&state);
                std::thread::sleep(Duration::from_millis(150));
                let waited = state.queued("session:s").is_empty();
                let deadline = Instant::now() + Duration::from_secs(3);
                while state.queued("session:s").is_empty() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(20));
                }
                (held, waited)
            });
            bridged(&state, &script, &stop);
            held.join().unwrap()
        });
        assert!(held, "{case}: queued while blocked");
        assert!(waited, "{case}: tried again before a second passed");
        assert!(state.queued("session:s").contains(&appeared), "{case}");
    }
}

/// A server that answers after three refusals is dialed again after each,
/// each wait at least as long as the backoff says.
#[test]
fn a_server_away_for_a_while_is_dialed_again_after_each_backoff() {
    let state = State::new();
    let stop = Wake::new().unwrap();
    let script = Script::dials(vec![
        Some((Some("host-a"), Vec::new(), None)),
        None,
        None,
        None,
        Some((Some("host-a"), Vec::new(), Some(resync("session:s")))),
    ]);
    const AWAY: Away = Away {
        delays: &[Duration::from_millis(50), Duration::from_millis(100)],
        give_up: Duration::from_secs(5),
        steady: Duration::ZERO,
    };
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let deadline = Instant::now() + Duration::from_secs(5);
            while script.opened.load(Ordering::Relaxed) < 2 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            stop.wake();
        });
        bridged_away(&state, &script, &stop, &AWAY);
    });
    assert_eq!(script.opened.load(Ordering::Relaxed), 2);
    let dialed = script.dialed.lock().unwrap();
    let gaps: Vec<Duration> = dialed.windows(2).map(|at| at[1] - at[0]).collect();
    assert_eq!(gaps.len(), 4, "{gaps:?}");
    for (gap, least) in gaps[1..].iter().zip([50, 100, 100]) {
        assert!(*gap >= Duration::from_millis(least), "{gaps:?}");
    }
}

/// A server that attaches and hangs up at once is a failed try each time:
/// the bridge backs off between them and gives up as for a server away.
#[test]
fn a_connection_that_ends_at_once_counts_as_a_failed_try() {
    let state = State::new();
    let state = State(state.0.join("no-spaces"));
    let stop = Wake::new().unwrap();
    let script = Script::new(
        (0..200)
            .map(|_| (Some("host-a"), Vec::new(), None))
            .collect(),
    );
    const AWAY: Away = Away {
        delays: &[Duration::from_millis(20)],
        give_up: Duration::from_millis(300),
        steady: Duration::from_secs(1),
    };
    let started = Instant::now();
    bridged_away(&state, &script, &stop, &AWAY);
    let took = started.elapsed();
    let opened = script.opened.load(Ordering::Relaxed);
    assert!((3..40).contains(&opened), "{opened} connections");
    assert!(
        (Duration::from_millis(300)..Duration::from_secs(2)).contains(&took),
        "{took:?}"
    );
}

/// Deleting the state directory under a running bridge, or replacing its
/// lock file, ends that bridge, so the directory made again gets exactly
/// one.
#[test]
fn a_bridge_stops_once_its_state_directory_is_deleted_or_replaced() {
    for replaced in [false, true] {
        let state = State::new();
        let stop = Wake::new().unwrap();
        let held = || (Some("host-a"), Vec::new(), Some(resync("session:s")));
        let script = Script::new(vec![held(), held()]);
        let done = std::sync::atomic::AtomicBool::new(false);
        let (early, ended) = std::thread::scope(|threads| {
            threads.spawn(|| {
                bridged(&state, &script, &stop);
                done.store(true, Ordering::SeqCst);
            });
            let _stop = StopOnDrop(&stop);
            std::thread::sleep(Duration::from_millis(200));
            let early = done.load(Ordering::SeqCst);
            if replaced {
                let lock = state
                    .0
                    .join("rex-bridge")
                    .join(format!("{}.lock", scope(&endpoint())));
                fs::write(state.0.join("new.lock"), "").unwrap();
                fs::rename(state.0.join("new.lock"), lock).unwrap();
            } else {
                fs::remove_dir_all(&state.0).unwrap();
            }
            let deadline = Instant::now() + Duration::from_secs(2);
            while !done.load(Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            (early, done.load(Ordering::SeqCst))
        });
        assert!(!early, "the bridge ended early");
        assert!(ended, "replaced: {replaced}");
        assert_eq!(script.opened.load(Ordering::Relaxed), 1);
        let first = Lock::take(&state.0, &endpoint()).unwrap();
        assert!(first.is_some());
        assert!(Lock::take(&state.0, &endpoint()).unwrap().is_none());
    }
}

/// Which image of `a_re_exec_d_bridge_reaps_the_workers_its_old_image_started`
/// a test process is, and the state directory it uses.
const IMAGE: &str = "NAMEDROP_TEST_BRIDGE_IMAGE";
const IMAGE_STATE: &str = "NAMEDROP_TEST_BRIDGE_STATE";
const REEXEC_TEST: &str =
    "rex_bridge::tests::a_re_exec_d_bridge_reaps_the_workers_its_old_image_started";

/// A worker still running when the bridge re-execs is a child of the new
/// image, and the reaper threads of the old one are gone. The new image
/// reaps it all the same. A child test process is the old image: it starts
/// a worker and re-execs through the bridge's own `Processes`, into a
/// launcher that runs this test again as the new image, which takes the
/// workers as `run` takes them and then stays up while this test watches
/// the worker.
#[test]
fn a_re_exec_d_bridge_reaps_the_workers_its_old_image_started() {
    use std::os::unix::fs::PermissionsExt;
    match std::env::var(IMAGE).as_deref() {
        Ok("old") => return old_image(),
        Ok("new") => {
            let _workers = Workers::inherited();
            std::thread::sleep(Duration::from_secs(4));
            return;
        }
        _ => {}
    }
    let state = State::new();
    let runtime = state.0.join("runtime");
    fs::create_dir_all(runtime.join("current")).unwrap();
    std::os::unix::fs::symlink("/usr/bin/true", runtime.join("current/namedrop")).unwrap();
    let exe = std::env::current_exe().unwrap();
    let launcher = runtime.join("namedrop");
    fs::write(
        &launcher,
        format!(
            "#!/bin/sh\nexec env {IMAGE}=new '{}' \"$@\"\n",
            exe.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o700)).unwrap();
    let mut image = std::process::Command::new(&exe)
        .args([REEXEC_TEST, "--exact", "--nocapture", "--test-threads=1"])
        .env(IMAGE, "old")
        .env(IMAGE_STATE, &state.0)
        .stdout(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let worker = loop {
        if let Some(pid) = fs::read_to_string(state.0.join("worker.pid"))
            .ok()
            .and_then(|pid| pid.trim().parse::<libc::pid_t>().ok())
        {
            break pid;
        }
        assert!(Instant::now() < deadline, "the old image started no worker");
        std::thread::sleep(Duration::from_millis(10));
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    // SAFETY: signal 0 only asks whether the worker still exists, and a
    // zombie still does.
    while unsafe { libc::kill(worker, 0) } == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let gone = unsafe { libc::kill(worker, 0) } != 0;
    let image_ran = image.try_wait().unwrap().is_none();
    let status = image.wait().unwrap();
    assert!(
        image_ran,
        "the new image ended before the worker was reaped"
    );
    assert!(gone, "the new image left worker {worker} unreaped");
    assert!(status.success(), "{status}");
}

/// The old image: one worker that outlives the re-exec, then the re-exec.
fn old_image() {
    let state = PathBuf::from(std::env::var_os(IMAGE_STATE).unwrap());
    let workers = Workers::default();
    let worker = sleeper();
    fs::write(state.join("worker.pid"), worker.id().to_string()).unwrap();
    workers.reap(worker);
    let endpoint = endpoint();
    Processes {
        state: &state,
        endpoint: &endpoint,
        workers,
    }
    .reexec_if_replaced();
    panic!("the bridge did not re-exec");
}

/// A worker that runs for a second.
fn sleeper() -> Child {
    std::process::Command::new("/bin/sleep")
        .arg("1")
        .spawn()
        .unwrap()
}
