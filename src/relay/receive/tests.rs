use super::*;
use serde_json::json;
use std::{
    cell::Cell,
    fs,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct State(PathBuf);

impl State {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "namedrop-relay-receive-{}-{}",
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

/// A receiver whose accept runs `script` under `sh`, counting each start.
fn receiver(state: &State, script: &'static str, within: Duration) -> (Receiver, Rc<Cell<u32>>) {
    let started = Rc::new(Cell::new(0));
    let counted = Rc::clone(&started);
    let receiver = Receiver::with(
        &state.0,
        Box::new(move |_host_id| {
            counted.set(counted.get() + 1);
            let mut command = Command::new("/bin/sh");
            command.args(["-c", script]);
            Ok(command)
        }),
        within,
    );
    (receiver, started)
}

fn perform(principal: Value, expect_host_id: &str) -> Perform {
    Perform {
        call_id: "call-1".to_owned(),
        args: json!({
            "op": "deliver", "expect_host_id": expect_host_id,
            "from": {"host_id": "host-a", "name": "alice", "host": "laptop"},
            "item": {"id": "m1", "to": "bob", "thread": "t1", "in_reply_to": null, "text": "hi"},
            "ack_through": 0,
        }),
        principal,
    }
}

fn status(reply: &Reply) -> (&str, &str) {
    (
        reply.1["result"]["status"].as_str().unwrap(),
        reply.1["result"]["reason"].as_str().unwrap_or_default(),
    )
}

/// Polls until a reply comes or `limit` passes.
fn answered(receiver: &mut Receiver, limit: Duration) -> Vec<Reply> {
    let until = Instant::now() + limit;
    loop {
        let replies = receiver.poll(Instant::now());
        if !replies.is_empty() || Instant::now() >= until {
            return replies;
        }
        thread::sleep(TICK);
    }
}

const ECHO: &str = r#"cat >/dev/null; printf '{"host_id":"host-b","can_reply":true,"result":{"id":"m1","to":"bob","status":"accepted"},"outbox":[],"more":false}'"#;

#[test]
fn a_principal_other_than_the_owner_or_the_tailnet_is_refused_and_nothing_starts() {
    let state = State::new();
    let (mut receiver, started) = receiver(&state, ECHO, ANSWER_WITHIN);
    // SAFETY: `getuid` has no preconditions.
    let uid = unsafe { libc::getuid() };
    for (principal, refused) in [
        (
            json!({"kind": "anonymous", "transport": "tcp"}),
            "anonymous",
        ),
        (
            json!({"kind": "peer", "transport": "unix", "uid": uid + 1}),
            "peer",
        ),
        (json!({"kind": "operator"}), "operator"),
    ] {
        let reply = receiver
            .receive(perform(principal, "host-b"), Some("host-b"))
            .expect("refused at once");
        assert_eq!(
            status(&reply),
            (
                "refused",
                format!("the relay does not accept a {refused} caller here").as_str()
            )
        );
        assert_eq!(reply.0, "call-1");
        assert_eq!(reply.1["result"]["id"], "m1");
    }
    let reply = receiver
        .receive(perform(json!({}), "host-b"), Some("host-b"))
        .unwrap();
    assert_eq!(status(&reply).0, "refused");
    assert_eq!(started.get(), 0);

    fs::create_dir_all(state.0.join("relay")).unwrap();
    fs::write(state.0.join("relay/allow-anonymous"), "").unwrap();
    for principal in [
        json!({"kind": "anonymous", "transport": "tcp"}),
        json!({"kind": "peer", "transport": "unix", "uid": uid}),
        json!({"kind": "tailscale", "login": "owner"}),
    ] {
        assert_eq!(
            receiver.receive(perform(principal, "host-b"), Some("host-b")),
            None
        );
    }
    assert_eq!(started.get(), 3);
    let mut replies = Vec::new();
    while replies.len() < 3 {
        replies.extend(answered(&mut receiver, Duration::from_secs(5)));
    }
    for reply in &replies {
        assert_eq!(status(reply).0, "accepted");
    }
    receiver.finish();
}

#[test]
fn a_call_for_another_server_is_refused_as_wrong_host_and_nothing_starts() {
    let state = State::new();
    let (mut receiver, started) = receiver(&state, ECHO, ANSWER_WITHIN);
    let uid = unsafe { libc::getuid() };
    let owner = json!({"kind": "peer", "transport": "unix", "uid": uid});
    for host_id in [Some("host-c"), None] {
        let reply = receiver
            .receive(perform(owner.clone(), "host-b"), host_id)
            .unwrap();
        assert_eq!(status(&reply), ("refused", "wrong_host"));
    }
    assert_eq!(started.get(), 0);
}

#[test]
fn an_accept_answers_with_its_output_and_a_slow_one_answers_retry() {
    let state = State::new();
    let uid = unsafe { libc::getuid() };
    let owner = json!({"kind": "peer", "transport": "unix", "uid": uid});
    let (mut receiver, _) = receiver(
        &state,
        r#"read line; printf '{"host_id":"host-b","can_reply":true,"outbox":[],"more":false,"echo":%s}' "$line""#,
        ANSWER_WITHIN,
    );
    assert_eq!(
        receiver.receive(perform(owner.clone(), "host-b"), Some("host-b")),
        None
    );
    assert!(receiver.busy());
    let replies = answered(&mut receiver, Duration::from_secs(5));
    assert_eq!(replies.len(), 1);
    assert_eq!(replies[0].1["host_id"], "host-b");
    assert!(!receiver.busy());

    let (mut receiver, _) = receiver_slow(&state);
    let started = Instant::now();
    assert_eq!(
        receiver.receive(perform(owner, "host-b"), Some("host-b")),
        None
    );
    let replies = answered(&mut receiver, Duration::from_secs(5));
    assert!(
        started.elapsed() < Duration::from_millis(900),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(status(&replies[0]), ("retry", "still deciding"));
    assert_eq!(replies[0].1["result"]["to"], "bob");
    assert!(!receiver.busy(), "an answered call needs no look");
    assert!(receiver.poll(Instant::now()).is_empty());
}

/// Whether `pid` names no process, not even a zombie.
fn reaped(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists.
    let gone = unsafe { libc::kill(pid, 0) } == -1;
    gone && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// An accept is reaped as it exits, whether its call was answered with its
/// output or with `retry`, and with no further look from the bridge.
#[test]
fn each_accept_is_reaped_as_it_exits_with_no_look() {
    let state = State::new();
    let uid = unsafe { libc::getuid() };
    for (within, script, answer) in [
        (
            ANSWER_WITHIN,
            r#"echo $$ >pid; cat >/dev/null; printf '{"host_id":"host-b","can_reply":true,"result":{"id":"m1","to":"bob","status":"accepted"},"outbox":[],"more":false}'"#,
            "accepted",
        ),
        (
            Duration::from_millis(100),
            "echo $$ >pid; cat >/dev/null; sleep 0.5; echo '{}'",
            "retry",
        ),
    ] {
        let directory = state.0.clone();
        let mut receiver = Receiver::with(
            &state.0,
            Box::new(move |_host_id| {
                let mut command = Command::new("/bin/sh");
                command.current_dir(&directory).args(["-c", script]);
                Ok(command)
            }),
            within,
        );
        let _ = fs::remove_file(state.0.join("pid"));
        receiver.receive(
            perform(json!({"kind": "peer", "uid": uid}), "host-b"),
            Some("host-b"),
        );
        let replies = answered(&mut receiver, Duration::from_secs(5));
        assert_eq!(status(&replies[0]).0, answer);
        let until = Instant::now() + Duration::from_secs(5);
        let pid = loop {
            let read = fs::read_to_string(state.0.join("pid")).unwrap_or_default();
            if let Ok(pid) = read.trim().parse::<i32>() {
                break pid;
            }
            assert!(Instant::now() < until, "the accept never wrote its pid");
            thread::sleep(TICK);
        };
        while !reaped(pid) && Instant::now() < until {
            thread::sleep(TICK);
        }
        assert!(reaped(pid), "{script}: the accept {pid} is left unreaped");
    }
}

fn receiver_slow(state: &State) -> (Receiver, Rc<Cell<u32>>) {
    receiver(
        state,
        "cat >/dev/null; sleep 1; echo '{}'",
        Duration::from_millis(200),
    )
}

#[test]
fn an_accept_whose_output_is_no_answer_answers_retry() {
    let state = State::new();
    let uid = unsafe { libc::getuid() };
    let (mut receiver, _) = receiver(&state, "cat >/dev/null; echo nonsense", ANSWER_WITHIN);
    receiver.receive(
        perform(json!({"kind": "peer", "uid": uid}), "host-b"),
        Some("host-b"),
    );
    let replies = answered(&mut receiver, Duration::from_secs(5));
    assert_eq!(
        status(&replies[0]),
        ("retry", "the relay's accept gave no answer")
    );
}

#[test]
fn finishing_waits_for_every_accept_still_running() {
    let state = State::new();
    let uid = unsafe { libc::getuid() };
    let (mut receiver, _) = receiver_slow(&state);
    receiver.receive(
        perform(json!({"kind": "peer", "uid": uid}), "host-b"),
        Some("host-b"),
    );
    let started = Instant::now();
    receiver.finish();
    assert!(
        started.elapsed() >= Duration::from_millis(700),
        "{:?}",
        started.elapsed()
    );
    assert!(!receiver.busy());
}

/// The accept names its endpoint as a flag, which is where `relay accept`
/// reads it, and gets the bridge's state directory.
#[test]
fn the_accept_command_names_the_endpoint_and_the_state() {
    let endpoint = Endpoint::parse("rex:/tmp/namedrop-relay-receive.sock");
    let command = accept_command(
        Path::new("/x/namedrop"),
        Path::new("/x/state"),
        &endpoint,
        "host-b",
    );
    let args: Vec<_> = command
        .get_args()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        args[..6],
        [
            "relay",
            "accept",
            "--host-id",
            "host-b",
            "--endpoint",
            "rex:/tmp/namedrop-relay-receive.sock"
        ]
    );
    let environment: Vec<_> = command
        .get_envs()
        .filter_map(|(key, value)| {
            Some((
                key.to_string_lossy().into_owned(),
                value?.to_string_lossy().into_owned(),
            ))
        })
        .collect();
    assert!(
        environment.contains(&("NAMEDROP_STATE_DIR".to_owned(), "/x/state".to_owned()))
            || args.windows(2).any(|pair| pair[1] == "/x/state"),
        "{args:?} {environment:?}"
    );
}
