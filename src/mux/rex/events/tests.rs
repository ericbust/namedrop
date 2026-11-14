use super::*;
use crate::mux::rex::control::tests::Server;
use std::{
    fs,
    io::Write,
    path::PathBuf,
    sync::atomic::AtomicU64,
    thread::{self, JoinHandle},
};

/// Real lines of `rex events --all --json` from a scratch server: a session
/// whose block reported a status and cleared it, a split that exited, a
/// second window whose only block exited, a `rex kill`, and a second session
/// whose block changed its title and directory and rang the bell before its
/// kill. Client joins and leaves are left out, and the user's name is
/// scrubbed.
const LIFECYCLE: &str = include_str!("../fixtures/events/lifecycle.jsonl");

static NEXT: AtomicU64 = AtomicU64::new(0);

fn lines() -> Vec<Value> {
    LIFECYCLE
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

struct State(PathBuf);

impl State {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "namedrop-rex-events-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}

impl Drop for State {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Answers the hello, `server.status` as a local server with `host_id`, and
/// `session.attach_all`, then runs `script`.
fn serve<T: Send + 'static>(
    host_id: &'static str,
    script: impl FnOnce(&mut Server) -> T + Send + 'static,
) -> (UnixStream, JoinHandle<T>) {
    let (client, server) = UnixStream::pair().unwrap();
    let thread = thread::spawn(move || {
        let mut server = Server::upgrade(server);
        server.welcome();
        let status = server.message();
        assert_eq!(status["method"], "server.status");
        server.send(
            json!({"type": "response", "id": status["id"], "seq": 0, "payload": {
            "pid": std::process::id(), "host_id": host_id,
            "listeners": [{"primary": true, "transport": "unix", "addresses": ["/tmp/r.sock"]}]}}),
        );
        let attach = server.message();
        assert_eq!(attach["method"], "session.attach_all");
        assert_eq!(attach["payload"], json!({"event_scope": "all"}));
        server.send(
            json!({"type": "response", "id": attach["id"], "seq": 0, "payload": {"sessions": []}}),
        );
        script(&mut server)
    });
    (client, thread)
}

fn events<'a>(
    stream: UnixStream,
    state: &State,
    wake: &'a Wake,
    liveness: Liveness,
) -> RexEvents<'a> {
    RexEvents::over(stream, Path::new("/tmp/r.sock"), &state.0, wake, liveness).unwrap()
}

fn send_events(server: &mut Server, payloads: &[Value]) {
    for (seq, payload) in payloads.iter().enumerate() {
        server.send(json!({"type": "event", "seq": seq + 1, "payload": payload}));
    }
}

/// What line `number` of the fixture decodes to, by the decode table's row
/// for its kind, with the IDs the line names.
fn expected(number: usize, kind: &str) -> MuxEvent {
    let line = &lines()[number - 1];
    let session = |line: &Value| SpaceId::new(line["payload"]["session_id"].as_str().unwrap());
    let slot = || PaneSlot {
        space: session(line),
        pane: PaneId::new(
            line["payload"]["event"]["payload"]["block_id"]
                .as_str()
                .unwrap(),
        ),
        tab: None,
    };
    match kind {
        "appeared" => MuxEvent::PaneAppeared(slot()),
        "closed" => MuxEvent::PaneClosed(slot()),
        "status" => MuxEvent::AgentStatusChanged(slot()),
        "resync" => MuxEvent::Resync(session(line)),
        "space" => MuxEvent::SpaceClosed(session(line)),
        _ => unreachable!(),
    }
}

/// Every fixture line goes over the wire, and only the decode table's rows
/// come out, in order. `title_changed`, `pwd_changed`, `bell`, view and
/// owner changes, client events, and `session_created` yield nothing.
#[test]
fn each_row_of_the_decode_table_yields_its_event_and_every_other_line_none() {
    let state = State::new();
    let wake = Wake::new().unwrap();
    let (client, server) = serve("host-a", |server| {
        send_events(server, &lines());
        server.close_code()
    });
    let mut stream = events(client, &state, &wake, Liveness::REX);
    assert_eq!(stream.host_id(), Some("host-a"));
    let mut decoded = Vec::new();
    while let Next::Event(event) = stream.next(Duration::from_millis(300)) {
        decoded.push(event);
    }
    let rows = [
        (2, "status"),
        (4, "appeared"),
        (6, "status"),
        (11, "status"),
        (16, "status"),
        (23, "status"),
        (28, "status"),
        (29, "closed"),
        (32, "appeared"),
        (34, "status"),
        (41, "status"),
        (46, "status"),
        (47, "closed"),
        (48, "resync"),
        (53, "closed"),
        (55, "space"),
        (57, "status"),
        (67, "closed"),
        (69, "space"),
    ];
    let wanted: Vec<MuxEvent> = rows
        .iter()
        .map(|(number, kind)| expected(*number, kind))
        .collect();
    assert_eq!(decoded, wanted);
    for (number, name) in [(61, "title_changed"), (62, "pwd_changed"), (63, "bell")] {
        assert_eq!(
            lines()[number - 1]["payload"]["event"]["payload"]["name"],
            name
        );
    }
    drop(stream);
    assert_eq!(server.join().unwrap(), Some(1000));
}

/// Only the root status record speaks for the block's agent.
#[test]
fn a_status_record_other_than_the_root_s_yields_nothing() {
    let mut root = lines()[15].clone();
    assert_eq!(
        root["payload"]["event"]["payload"]["name"],
        "program_status_changed"
    );
    let mut other = root.clone();
    other["payload"]["event"]["payload"]["payload"]["id"] = json!("tool");
    let mut removed = lines()[22].clone();
    removed["payload"]["event"]["payload"]["payload"]["ids"] = json!(["tool"]);
    let state = State::new();
    let wake = Wake::new().unwrap();
    root["payload"]["event"]["payload"]["block_id"] = json!("block:root");
    let (client, _server) = serve("host-a", move |server| {
        send_events(server, &[other, removed, root]);
        server.close_code()
    });
    let mut stream = events(client, &state, &wake, Liveness::REX);
    match stream.next(Duration::from_secs(5)) {
        Next::Event(MuxEvent::AgentStatusChanged(slot)) => {
            assert_eq!(slot.pane.as_str(), "block:root");
        }
        next => panic!("{next:?}"),
    }
}

#[test]
fn a_closed_block_s_binding_refusal_and_lock_are_deleted_before_its_close_is_handed_on() {
    let state = State::new();
    let uuid = "01a1d75e-abc0-7bf6-bf78-674a5952f049";
    let files: Vec<PathBuf> = [
        format!("rex/sessions/{uuid}.json"),
        format!("rex/refusals/{uuid}.json"),
        format!("rex/locks/{uuid}.lock"),
        "rex/sessions/01a1d75e-0000-0000-0000-000000000000.json".to_owned(),
    ]
    .iter()
    .map(|file| state.0.join(file))
    .collect();
    for file in &files {
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(file, "{}").unwrap();
    }
    let closed = lines()[28].clone();
    assert_eq!(
        closed["payload"]["event"]["payload"]["block_id"],
        format!("block:{uuid}")
    );
    let wake = Wake::new().unwrap();
    let (client, _server) = serve("host-a", move |server| {
        send_events(server, &[closed]);
        server.close_code()
    });
    let mut stream = events(client, &state, &wake, Liveness::REX);
    assert!(matches!(
        stream.next(Duration::from_secs(5)),
        Next::Event(MuxEvent::PaneClosed(_))
    ));
    let left: Vec<bool> = files.iter().map(|file| file.exists()).collect();
    assert_eq!(left, [false, false, false, true]);
}

/// A server that answers nothing gets one ping after the quiet time and
/// ends the stream once the answer time passes. One that answers keeps it.
#[test]
fn a_missing_pong_ends_the_stream_and_a_pong_keeps_it() {
    let liveness = Liveness {
        quiet: Duration::from_millis(100),
        answer: Duration::from_millis(200),
    };
    for answers in [false, true] {
        let state = State::new();
        let wake = Wake::new().unwrap();
        let (client, server) = serve("host-a", move |server| {
            let (opcode, payload) = server.next_client_frame().unwrap();
            assert_eq!(opcode, 0x9, "a ping");
            if answers {
                server.send_frame(0xA, &payload);
            }
            let after = server.next_client_frame().map(|(opcode, _)| opcode);
            thread::sleep(Duration::from_millis(600));
            after
        });
        let mut stream = events(client, &state, &wake, liveness);
        let started = Instant::now();
        let next = stream.next(Duration::from_millis(500));
        let took = started.elapsed();
        if answers {
            assert_eq!(next, Next::Quiet);
            assert!(took >= Duration::from_millis(500), "{took:?}");
        } else {
            assert_eq!(
                next,
                Next::Ended("the server sent nothing for 0.2 s after a ping".to_owned())
            );
            assert!(
                (Duration::from_millis(300)..Duration::from_millis(450)).contains(&took),
                "{took:?}"
            );
        }
        drop(stream);
        let after = server.join().unwrap();
        if answers {
            assert_eq!(after, Some(0x9), "silent again, so it pings again");
        }
    }
}

/// The wait is one `poll(2)` that the wake's pipe ends at once, not a loop
/// of short reads.
#[test]
fn a_wake_ends_a_wait_at_once() {
    let state = State::new();
    let wake = Wake::new().unwrap();
    let (client, _server) = serve("host-a", |server| server.close_code());
    let mut stream = events(client, &state, &wake, Liveness::REX);
    let woken = thread::scope(|scope| {
        let waker = scope.spawn(|| {
            thread::sleep(Duration::from_millis(100));
            let woken = Instant::now();
            wake.wake();
            woken
        });
        assert_eq!(
            stream.next(Duration::from_secs(10)),
            Next::Ended("stopped".to_owned())
        );
        let ended = Instant::now();
        ended.saturating_duration_since(waker.join().unwrap())
    });
    assert!(woken < Duration::from_secs(1), "{woken:?}");
    assert!(wake.is_set());
}

fn slot(space: &str, pane: &str) -> PaneSlot {
    PaneSlot {
        space: SpaceId::new(space),
        pane: PaneId::new(pane),
        tab: None,
    }
}

#[test]
fn pending_keeps_one_status_change_per_pane_and_the_order_of_the_rest() {
    let now = Instant::now();
    let mut pending = Pending::default();
    assert_eq!(pending.due(), None);
    let events = [
        MuxEvent::AgentStatusChanged(slot("s", "b1")),
        MuxEvent::PaneAppeared(slot("s", "b2")),
        MuxEvent::AgentStatusChanged(slot("s", "b1")),
        MuxEvent::AgentStatusChanged(slot("s", "b2")),
        MuxEvent::PaneClosed(slot("s", "b2")),
        MuxEvent::AgentStatusChanged(slot("t", "b3")),
        MuxEvent::AgentStatusChanged(slot("s", "b2")),
    ];
    for (at, event) in events.into_iter().enumerate() {
        pending.note(event, now + Duration::from_millis(at as u64 * 10));
    }
    assert_eq!(pending.due(), Some(now + Duration::from_millis(110)));
    assert_eq!(
        pending.take().spaces,
        [
            (
                SpaceId::new("s"),
                vec![
                    MuxEvent::AgentStatusChanged(slot("s", "b1")),
                    MuxEvent::PaneAppeared(slot("s", "b2")),
                    MuxEvent::AgentStatusChanged(slot("s", "b2")),
                    MuxEvent::PaneClosed(slot("s", "b2")),
                ]
            ),
            (
                SpaceId::new("t"),
                vec![MuxEvent::AgentStatusChanged(slot("t", "b3"))]
            ),
        ]
    );
    assert_eq!(pending.due(), None);
    for at in (0..=280).step_by(40) {
        pending.note(
            MuxEvent::AgentStatusChanged(slot("s", "b1")),
            now + Duration::from_millis(at),
        );
    }
    assert_eq!(
        pending.due(),
        Some(now + Duration::from_millis(250)),
        "a steady run of events still flushes 250 ms after its first"
    );
}

#[test]
fn a_resync_absorbs_its_space_s_status_changes_and_a_space_closed_absorbs_everything() {
    let now = Instant::now();
    let mut pending = Pending::default();
    for event in [
        MuxEvent::AgentStatusChanged(slot("s", "b1")),
        MuxEvent::PaneAppeared(slot("s", "b2")),
        MuxEvent::AgentStatusChanged(slot("t", "b3")),
        MuxEvent::Resync(SpaceId::new("s")),
        MuxEvent::AgentStatusChanged(slot("s", "b2")),
        MuxEvent::Resync(SpaceId::new("s")),
        MuxEvent::Resync(SpaceId::new("t")),
        MuxEvent::PaneClosed(slot("u", "b4")),
        MuxEvent::SpaceClosed(SpaceId::new("u")),
        MuxEvent::PaneAppeared(slot("u", "b5")),
        MuxEvent::Resync(SpaceId::new("u")),
    ] {
        pending.note(event, now);
    }
    assert_eq!(pending.due(), Some(now + Duration::from_millis(50)));
    assert_eq!(
        pending.take().spaces,
        [
            (
                SpaceId::new("s"),
                vec![
                    MuxEvent::PaneAppeared(slot("s", "b2")),
                    MuxEvent::Resync(SpaceId::new("s")),
                ]
            ),
            (SpaceId::new("t"), vec![MuxEvent::Resync(SpaceId::new("t"))]),
            (
                SpaceId::new("u"),
                vec![MuxEvent::SpaceClosed(SpaceId::new("u"))]
            ),
        ]
    );
}

/// What a server sends before it hangs for a second without hanging up.
type Stall = Box<dyn FnOnce(&mut Server) + Send>;

/// Half a frame and then nothing.
fn half_a_frame() -> Stall {
    Box::new(|server| server.stream.write_all(&[0x81, 50, b'{']).unwrap())
}

/// The header of a 50-byte frame, then one byte of it every 20 ms until
/// the stream hangs up.
fn trickle() -> Stall {
    Box::new(|server| {
        server.stream.write_all(&[0x81, 50]).unwrap();
        for _ in 0..50 {
            thread::sleep(Duration::from_millis(20));
            if server.stream.write_all(b" ").is_err() {
                return;
            }
        }
    })
}

/// `messages` as the frames a server sends, for one write.
fn frames(messages: impl IntoIterator<Item = Value>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for message in messages {
        let text = message.to_string();
        bytes.extend([0x81, 126]);
        bytes.extend(u16::try_from(text.len()).unwrap().to_be_bytes());
        bytes.extend(text.as_bytes());
    }
    bytes
}

/// 8 MiB of `message` frames in one write, built before the server
/// answers the hello, so the stream always has bytes to read until it has
/// read them all.
fn flood(message: impl Fn(usize) -> Value) -> Stall {
    let one = frames([message(1)]).len();
    let bytes = frames((1..=(8 << 20) / one + 1).map(message));
    Box::new(move |server| {
        let _ = server.stream.write_all(&bytes);
    })
}

/// `bell` events, which yield nothing.
fn bells() -> Stall {
    let line = &lines()[62];
    flood(|seq| json!({"type": "event", "seq": seq, "payload": line}))
}

/// `bell` events, sent faster than they are read and for longer than any
/// of these waits lasts, until the stream hangs up.
fn endless_bells() -> Stall {
    let line = &lines()[62];
    let chunk = frames((1..=4096).map(|seq| json!({"type": "event", "seq": seq, "payload": line})));
    Box::new(move |server| {
        let until = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < until && server.stream.write_all(&chunk).is_ok() {}
    })
}

/// Answers to no request, which the wait reads past.
fn answers() -> Stall {
    flood(|seq| json!({"type": "response", "id": 0, "seq": seq, "payload": {}}))
}

/// What `next(within)` answers, and how long it took, from a server that
/// sends what `stall` sends and then stays connected for a second. A stop
/// is requested 100 ms in when `stops` says so.
fn wait_through(
    stall: Stall,
    liveness: Liveness,
    within: Duration,
    stops: bool,
) -> (Next, Duration) {
    let state = State::new();
    let wake = Wake::new().unwrap();
    let (client, server) = serve("host-a", move |server| {
        stall(server);
        thread::sleep(Duration::from_millis(1000));
    });
    let mut stream = events(client, &state, &wake, liveness);
    let started = Instant::now();
    let next = thread::scope(|scope| {
        if stops {
            scope.spawn(|| {
                thread::sleep(Duration::from_millis(100));
                wake.wake();
            });
        }
        stream.next(within)
    });
    let took = started.elapsed();
    drop(stream);
    server.join().unwrap();
    (next, took)
}

/// A server that stays connected but sends no whole frame the bridge
/// handles in time: the wait still ends at its deadline, at a stop, and
/// when liveness runs out.
#[test]
fn a_server_that_sends_nothing_whole_in_time_still_ends_the_wait_on_time() {
    let soon = Duration::from_millis(100)..Duration::from_millis(350);
    let stalls = [
        ("half a frame", half_a_frame as fn() -> Stall),
        ("a trickle", trickle),
        ("bells", bells),
        ("endless bells", endless_bells),
        ("answers", answers),
    ];
    for (name, stall) in stalls {
        let (next, took) = wait_through(stall(), Liveness::REX, Duration::from_millis(100), false);
        assert_eq!(next, Next::Quiet, "{name}");
        assert!(soon.contains(&took), "{name}, deadline: {took:?}");
        let (next, took) = wait_through(stall(), Liveness::REX, Duration::from_secs(10), true);
        assert_eq!(next, Next::Ended("stopped".to_owned()), "{name}");
        assert!(soon.contains(&took), "{name}, stop: {took:?}");
    }
    let quick = Liveness {
        quiet: Duration::from_millis(100),
        answer: Duration::from_millis(100),
    };
    let (next, took) = wait_through(half_a_frame(), quick, Duration::from_secs(10), false);
    assert_eq!(
        next,
        Next::Ended("the server sent nothing for 0.1 s after a ping".to_owned())
    );
    assert!(
        (Duration::from_millis(200)..Duration::from_millis(450)).contains(&took),
        "liveness: {took:?}"
    );
}

/// Messages read in one go come out without waiting for the server to
/// send more: an answer to no request is passed over, and then each event
/// comes out of its own `next`.
#[test]
fn messages_already_read_come_out_without_waiting_for_more_bytes() {
    let state = State::new();
    let wake = Wake::new().unwrap();
    let (client, server) = serve("host-a", |server| {
        let bytes = frames([
            json!({"type": "response", "id": 0, "seq": 1, "payload": {}}),
            json!({"type": "event", "seq": 2, "payload": lines()[1]}),
            json!({"type": "event", "seq": 3, "payload": lines()[3]}),
        ]);
        server.stream.write_all(&bytes).unwrap();
        thread::sleep(Duration::from_millis(1000));
    });
    let mut stream = events(client, &state, &wake, Liveness::REX);
    for (number, kind) in [(2, "status"), (4, "appeared")] {
        let started = Instant::now();
        assert_eq!(
            stream.next(Duration::from_secs(2)),
            Next::Event(expected(number, kind))
        );
        let took = started.elapsed();
        assert!(took < Duration::from_millis(100), "{kind}: {took:?}");
    }
    drop(stream);
    server.join().unwrap();
}

/// A stop wins over an event already read, as it wins over a readable
/// socket.
#[test]
fn a_stop_wins_over_an_event_already_read() {
    let state = State::new();
    let wake = Wake::new().unwrap();
    let (client, server) = serve("host-a", |server| {
        let bytes = frames([
            json!({"type": "event", "seq": 1, "payload": lines()[1]}),
            json!({"type": "event", "seq": 2, "payload": lines()[3]}),
        ]);
        server.stream.write_all(&bytes).unwrap();
        thread::sleep(Duration::from_millis(1000));
    });
    let mut stream = events(client, &state, &wake, Liveness::REX);
    assert_eq!(
        stream.next(Duration::from_secs(2)),
        Next::Event(expected(2, "status"))
    );
    wake.wake();
    assert_eq!(
        stream.next(Duration::from_secs(2)),
        Next::Ended("stopped".to_owned())
    );
    drop(stream);
    server.join().unwrap();
}

/// The wait for the rest of a frame sleeps in `poll(2)`, as every wait
/// does, rather than in short reads: 800 ms of it use almost no CPU.
#[test]
fn the_wait_for_the_rest_of_a_frame_sleeps_in_poll() {
    fn cpu() -> Duration {
        let mut now = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `clock_gettime` writes one timespec this owns.
        unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut now) };
        Duration::new(
            u64::try_from(now.tv_sec).unwrap(),
            u32::try_from(now.tv_nsec).unwrap(),
        )
    }
    let state = State::new();
    let wake = Wake::new().unwrap();
    let stall = half_a_frame();
    let (client, server) = serve("host-a", |server| {
        stall(server);
        thread::sleep(Duration::from_millis(1000));
    });
    let mut stream = events(client, &state, &wake, Liveness::REX);
    let before = cpu();
    assert_eq!(stream.next(Duration::from_millis(800)), Next::Quiet);
    let used = cpu() - before;
    assert!(used < Duration::from_micros(400), "{used:?}");
    drop(stream);
    server.join().unwrap();
}
