mod support;

use namedrop::dashboard::snapshot::SpaceKey;
use namedrop::{ActorKind, MutationAttribution, StateNamespace, StateStore};
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    net::TcpStream,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use support::{TestDirectory, address};

const SOCKET: &str = "socket-all-spaces";
const WAIT: Duration = Duration::from_secs(5);

fn attribution() -> MutationAttribution {
    MutationAttribution::new(ActorKind::Plugin, "test", 10)
}

fn namespace(id: &str) -> StateNamespace {
    StateNamespace::for_space(SOCKET, id)
}

fn key(id: &str) -> String {
    SpaceKey::of(&namespace(id)).as_str().to_owned()
}

fn activate(state: &Path, id: &str) {
    StateStore::open(state)
        .unwrap()
        .transaction(|tx| tx.activate_worktree(&namespace(id), &attribution()))
        .unwrap();
}

struct Server {
    child: Child,
    port: u16,
    cookie: String,
    root: PathBuf,
}

impl Server {
    fn start(root: &Path, workspaces: &[(&str, Option<&str>)]) -> Self {
        Self::snapshot(root, workspaces);
        let herdr = root.join("herdr");
        fs::write(
            &herdr,
            r#"#!/bin/sh
[ "$1 $2" = "api snapshot" ] || exit 1
printf '%s\n' "$HERDR_SOCKET_PATH" >> "$ND_TEST_ROOT/calls"
if [ -f "$ND_TEST_ROOT/block-$HERDR_SOCKET_PATH" ]; then
    printf '%s' "$$" > "$ND_TEST_ROOT/active-$HERDR_SOCKET_PATH"
    while [ -f "$ND_TEST_ROOT/block-$HERDR_SOCKET_PATH" ]; do sleep 0.01; done
    rm -f "$ND_TEST_ROOT/active-$HERDR_SOCKET_PATH"
fi
if [ -f "$ND_TEST_ROOT/delay-$HERDR_SOCKET_PATH" ]; then
    sleep "$(cat "$ND_TEST_ROOT/delay-$HERDR_SOCKET_PATH")"
fi
if [ -f "$ND_TEST_ROOT/down-$HERDR_SOCKET_PATH" ]; then
    printf '{"error":{"code":"server_not_running","message":"socket unavailable"}}\n' >&2
    exit 1
fi
if [ -f "$ND_TEST_ROOT/transient-$HERDR_SOCKET_PATH" ]; then
    printf 'Herdr read timed out\n' >&2
    exit 1
fi
if [ -f "$ND_TEST_ROOT/snapshot-$HERDR_SOCKET_PATH.json" ]; then
    cat "$ND_TEST_ROOT/snapshot-$HERDR_SOCKET_PATH.json"
else
    cat "$ND_TEST_SNAPSHOT"
fi
"#,
        )
        .unwrap();
        fs::set_permissions(&herdr, fs::Permissions::from_mode(0o755)).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_namedrop"))
            .arg("--plugin-state")
            .arg(root.join("state"))
            .arg("dashboard-server")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .env("HERDR_BIN_PATH", herdr)
            .env("ND_TEST_SNAPSHOT", root.join("snapshot.json"))
            .env("ND_TEST_ROOT", root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(fs::File::create(root.join("server.log")).unwrap())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + WAIT;
        let info = loop {
            if let Some(info) = fs::read(root.join("state/dashboard/server.json"))
                .ok()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .filter(|info| info["pid"] == child.id())
            {
                break info;
            }
            assert!(Instant::now() < deadline, "server did not start");
            thread::sleep(Duration::from_millis(20));
        };
        let port = u16::try_from(info["port"].as_u64().unwrap()).unwrap();
        let session = fs::read_to_string(root.join("state/dashboard/session")).unwrap();
        Self {
            child,
            port,
            cookie: format!("namedrop_dashboard_{port}={session}"),
            root: root.to_owned(),
        }
    }

    fn snapshot(root: &Path, workspaces: &[(&str, Option<&str>)]) {
        let workspaces: Vec<_> = workspaces
            .iter()
            .map(|(id, label)| json!({"workspace_id": id, "label": label}))
            .collect();
        fs::write(
            root.join("snapshot.next"),
            json!({"result": {"snapshot": {"workspaces": workspaces}}}).to_string(),
        )
        .unwrap();
        fs::rename(root.join("snapshot.next"), root.join("snapshot.json")).unwrap();
    }

    fn set_workspaces(&self, workspaces: &[(&str, Option<&str>)]) {
        Self::snapshot(&self.root, workspaces);
    }

    fn connect(&self, path: &str, headers: &[(&str, &str)]) -> TcpStream {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream.set_read_timeout(Some(WAIT)).unwrap();
        let mut request = format!("GET {path} HTTP/1.1\r\n");
        if !headers.iter().any(|(name, _)| *name == "Host") {
            request.push_str(&format!("Host: 127.0.0.1:{}\r\n", self.port));
        }
        for (name, value) in headers {
            request.push_str(&format!("{name}: {value}\r\n"));
        }
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).unwrap();
        stream
    }

    fn events(&self, path: &str) -> Events {
        Events {
            stream: self.connect(path, &[("Cookie", &self.cookie)]),
            buffer: String::new(),
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Events {
    stream: TcpStream,
    buffer: String,
}

impl Events {
    fn next(&mut self, limit: Duration) -> Option<(String, Value)> {
        let deadline = Instant::now() + limit;
        loop {
            match self.next_raw(deadline.saturating_duration_since(Instant::now())) {
                Some((event, _)) if event == "ready" => continue,
                event => return event,
            }
        }
    }

    fn next_raw(&mut self, limit: Duration) -> Option<(String, Value)> {
        let deadline = Instant::now() + limit;
        loop {
            while let Some(end) = self.buffer.find("\n\n") {
                let raw: String = self.buffer.drain(..end + 2).collect();
                let event = raw.lines().find_map(|line| line.strip_prefix("event: "));
                let data = raw.lines().find_map(|line| line.strip_prefix("data: "));
                if let (Some(event), Some(data)) = (event, data) {
                    return Some((event.to_owned(), serde_json::from_str(data).unwrap()));
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return None;
            }
            self.stream.set_read_timeout(Some(remaining)).unwrap();
            let mut bytes = [0; 65536];
            match self.stream.read(&mut bytes) {
                Ok(0) => panic!("stream closed before deadline"),
                Ok(n) => self
                    .buffer
                    .push_str(std::str::from_utf8(&bytes[..n]).unwrap()),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(error) => panic!("stream read failed: {error}"),
            }
        }
    }

    fn space(&mut self) -> Value {
        let (event, data) = self.next(WAIT).expect("space frame");
        assert_eq!(event, "space");
        assert_eq!(data["key"], data["frame"]["space"]["key"]);
        data
    }
}

#[test]
fn connect_sends_every_live_space_and_only_changed_content_follows() {
    let directory = TestDirectory::new();
    let state = directory.path().join("state");
    activate(&state, "w0");
    activate(&state, "closed");
    let ids: Vec<_> = (0..30).map(|i| format!("w{i}")).collect();
    let live: Vec<_> = ids.iter().map(|id| (id.as_str(), None)).collect();
    let server = Server::start(directory.path(), &live);
    let mut events = server.events("/api/events");
    let seen: HashSet<_> = (0..30)
        .map(|_| events.space()["key"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(seen, ids.iter().map(|id| key(id)).collect());
    assert!(events.next(Duration::from_millis(400)).is_none());

    StateStore::open(&state)
        .unwrap()
        .transaction(|tx| {
            tx.create_pane_address(&namespace("w0"), &address("late", "late"), &attribution())
        })
        .unwrap();
    let changed = events.space();
    assert_eq!(changed["key"], key("w0"));
    assert!(
        changed["frame"]["participants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["alias"] == "@late")
    );
    assert!(events.next(Duration::from_millis(400)).is_none());

    let mut reconnect = server.events("/api/events");
    let again: HashSet<_> = (0..30)
        .map(|_| reconnect.space()["key"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(seen, again);
}

#[test]
fn live_membership_changes_without_database_writes() {
    let directory = TestDirectory::new();
    activate(&directory.path().join("state"), "a");
    let server = Server::start(
        directory.path(),
        &[("a", Some("alpha")), ("b", Some("beta"))],
    );
    let mut events = server.events("/api/events");
    events.space();
    events.space();

    server.set_workspaces(&[("a", Some("alpha"))]);
    assert_eq!(
        events.next(WAIT).unwrap(),
        ("gone".to_owned(), json!({"key": key("b")}))
    );
    assert!(events.next(Duration::from_millis(350)).is_none());

    server.set_workspaces(&[("a", Some("alpha")), ("new", None)]);
    let new = events.space();
    assert_eq!(new["key"], key("new"));
    assert_eq!(new["frame"]["space"]["workspace_id"], "new");
    assert_eq!(new["frame"]["space"]["display_name"], "new");
    assert_eq!(new["frame"]["space"]["state"], "unknown");

    server.set_workspaces(&[]);
    let gone: HashSet<_> = (0..2)
        .map(|_| {
            let (event, data) = events.next(WAIT).unwrap();
            assert_eq!(event, "gone");
            data["key"].as_str().unwrap().to_owned()
        })
        .collect();
    assert_eq!(gone, HashSet::from([key("a"), key("new")]));
    server.set_workspaces(&[("a", Some("alpha"))]);
    assert_eq!(events.space()["key"], key("a"));
}

#[test]
fn labels_are_live_and_ambiguity_updates_both_workspaces() {
    let directory = TestDirectory::new();
    let state = directory.path().join("state");
    for id in ["a", "b", "c"] {
        activate(&state, id);
    }
    fs::create_dir_all(state.join("dashboard")).unwrap();
    fs::write(
        state.join("dashboard/labels.json"),
        json!({key("a"): "cached-label"}).to_string(),
    )
    .unwrap();
    let server = Server::start(
        directory.path(),
        &[
            ("a", Some("Same Label")),
            ("b", Some("same-label")),
            ("c", None),
        ],
    );
    let mut events = server.events("/api/events");
    let initial: Vec<_> = (0..3).map(|_| events.space()).collect();
    for data in &initial {
        let space = &data["frame"]["space"];
        assert!(space["addressable_label"].is_null());
        match space["workspace_id"].as_str().unwrap() {
            "a" => {
                assert_eq!(space["display_name"], "Same Label");
                assert_eq!(space["label"]["text"], "cached-label");
            }
            "b" => assert_eq!(space["display_name"], "same-label"),
            "c" => assert_eq!(space["display_name"], "c"),
            id => panic!("unexpected workspace {id}"),
        }
    }
    server.set_workspaces(&[("a", Some("Same Label")), ("b", Some("beta")), ("c", None)]);
    let updates: Vec<_> = (0..2).map(|_| events.space()).collect();
    assert_eq!(
        updates
            .iter()
            .map(|data| data["key"].as_str().unwrap().to_owned())
            .collect::<HashSet<_>>(),
        HashSet::from([key("a"), key("b")])
    );
    for data in updates {
        let space = &data["frame"]["space"];
        let expected = if space["workspace_id"] == "a" {
            "same-label"
        } else {
            "beta"
        };
        assert_eq!(space["addressable_label"], expected);
    }
    assert!(events.next(Duration::from_millis(350)).is_none());

    let mut legacy = server.events(&format!("/w/{}/events", key("a")));
    let (event, mut frame) = legacy.next(WAIT).unwrap();
    assert_eq!(event, "snapshot");
    assert_eq!(frame["space"]["display_name"], "Same Label");
    assert_eq!(frame["space"]["addressable_label"], "same-label");
    let mut all = server.events("/api/events");
    let all_a = (0..3)
        .map(|_| all.space())
        .find(|data| data["key"] == key("a"))
        .unwrap();
    let mut all_frame = all_a["frame"].clone();
    frame.as_object_mut().unwrap().remove("generated_at_ms");
    all_frame.as_object_mut().unwrap().remove("generated_at_ms");
    assert_eq!(frame, all_frame);
}

#[test]
fn discovery_failure_does_not_remove_workspaces_and_recovers_without_a_write() {
    let directory = TestDirectory::new();
    activate(&directory.path().join("state"), "a");
    let server = Server::start(directory.path(), &[("a", None)]);
    let mut events = server.events("/api/events");
    events.space();
    fs::write(directory.path().join("snapshot.json"), "{}").unwrap();
    assert!(events.next(Duration::from_millis(650)).is_none());
    server.set_workspaces(&[("a", None), ("b", None)]);
    assert_eq!(events.space()["key"], key("b"));
    assert!(events.next(Duration::from_millis(350)).is_none());
}

#[test]
fn an_unchanged_workspace_rejoins_while_a_legacy_subscription_keeps_its_cache() {
    let directory = TestDirectory::new();
    activate(&directory.path().join("state"), "a");
    let server = Server::start(directory.path(), &[("a", None), ("unknown", None)]);
    let mut legacy = server.events(&format!("/w/{}/events", key("a")));
    assert_eq!(legacy.next(WAIT).unwrap().0, "snapshot");
    let mut all = server.events("/api/events");
    all.space();
    all.space();

    let mut unknown = server.events(&format!("/w/{}/events", key("unknown")));
    let (event, mut actual) = unknown.next(WAIT).unwrap();
    assert_eq!(event, "snapshot");
    assert_eq!(actual["space"]["display_name"], "unknown");
    let header = actual["space"].as_object_mut().unwrap();
    header.remove("display_name");
    header.remove("addressable_label");
    actual["generated_at_ms"] = json!(0);
    let mut expected =
        serde_json::to_value(namedrop::dashboard::snapshot::DashboardSnapshot::unknown(
            SpaceKey::of(&namespace("unknown")),
            None,
            0,
        ))
        .unwrap();
    let header = expected["space"].as_object_mut().unwrap();
    header.remove("display_name");
    header.remove("addressable_label");
    expected["space"]["workspace_id"] = json!("unknown");
    assert_eq!(actual, expected);

    server.set_workspaces(&[("unknown", None)]);
    assert_eq!(
        all.next(WAIT).unwrap(),
        ("gone".to_owned(), json!({"key": key("a")}))
    );
    assert!(legacy.next(Duration::from_millis(350)).is_none());
    server.set_workspaces(&[("a", None), ("unknown", None)]);
    assert_eq!(all.space()["key"], key("a"));
    assert!(legacy.next(Duration::from_millis(350)).is_none());
}

#[test]
fn all_events_requires_session_cookie_and_loopback_host_without_sending_frames() {
    let directory = TestDirectory::new();
    activate(&directory.path().join("state"), "a");
    let server = Server::start(directory.path(), &[("a", None)]);
    for (headers, status) in [
        (vec![], "401 Unauthorized"),
        (vec![("Cookie", "wrong=secret")], "401 Unauthorized"),
        (
            vec![
                ("Cookie", server.cookie.as_str()),
                ("Host", "foreign.example"),
            ],
            "403 Forbidden",
        ),
        (
            vec![
                ("Cookie", server.cookie.as_str()),
                ("Sec-Fetch-Site", "cross-site"),
            ],
            "403 Forbidden",
        ),
    ] {
        let mut raw = String::new();
        server
            .connect("/api/events", &headers)
            .read_to_string(&mut raw)
            .unwrap();
        assert!(raw.starts_with(&format!("HTTP/1.1 {status}")), "{raw}");
        assert!(!raw.contains("event:"), "{raw}");
        assert!(!raw.contains(&key("a")), "{raw}");
    }
}

fn calls(root: &Path) -> Vec<String> {
    fs::read_to_string(root.join("calls"))
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn eventually(mut probe: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !probe() {
        assert!(Instant::now() < deadline, "condition did not become true");
        thread::sleep(Duration::from_millis(10));
    }
}

fn activate_on(state: &Path, socket: &str, id: &str) {
    StateStore::open(state)
        .unwrap()
        .transaction(|tx| {
            tx.activate_worktree(&StateNamespace::for_space(socket, id), &attribution())
        })
        .unwrap();
}

fn snapshot_on(root: &Path, socket: &str, spaces: &[(&str, Option<&str>)]) {
    let spaces: Vec<_> = spaces
        .iter()
        .map(|(id, label)| json!({"workspace_id":id,"label":label}))
        .collect();
    fs::write(
        root.join(format!("snapshot-{socket}.json")),
        json!({"result":{"snapshot":{"workspaces":spaces}}}).to_string(),
    )
    .unwrap();
}

#[test]
fn discovery_runs_only_for_all_space_subscribers_and_stops_with_the_last_one() {
    let directory = TestDirectory::new();
    activate(&directory.path().join("state"), "a");
    let server = Server::start(directory.path(), &[("a", None)]);
    thread::sleep(Duration::from_millis(550));
    assert_eq!(
        calls(directory.path()).len(),
        0,
        "idle dashboard spawned Herdr reads"
    );
    let mut legacy = server.events(&format!("/w/{}/events", key("a")));
    assert_eq!(legacy.next(WAIT).unwrap().0, "snapshot");
    thread::sleep(Duration::from_millis(550));
    assert_eq!(
        calls(directory.path()).len(),
        0,
        "legacy tab spawned Herdr reads"
    );
    let mut all = server.events("/api/events");
    all.space();
    assert!(!calls(directory.path()).is_empty());
    drop(all);
    thread::sleep(Duration::from_millis(1400));
    let before = calls(directory.path()).len();
    thread::sleep(Duration::from_millis(550));
    assert_eq!(
        calls(directory.path()).len(),
        before,
        "discovery continued after the last all-space tab closed"
    );
    assert!(legacy.next(Duration::from_millis(50)).is_none());
}

#[test]
fn discovery_logs_failures_and_recovery_once_per_socket_state_change() {
    let directory = TestDirectory::new();
    let state = directory.path().join("state");
    for socket in ["dead-a", "dead-b"] {
        activate_on(&state, socket, "a");
        fs::write(directory.path().join(format!("transient-{socket}")), "").unwrap();
    }
    let server = Server::start(directory.path(), &[("a", None)]);
    let mut all = server.events("/api/events");
    assert!(all.next(Duration::from_millis(1250)).is_none());
    let log = fs::read_to_string(directory.path().join("server.log")).unwrap();
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("live workspaces on"))
            .count(),
        2,
        "expected one stable failure per socket; log was {log}"
    );
    for socket in ["dead-a", "dead-b"] {
        fs::remove_file(directory.path().join(format!("transient-{socket}"))).unwrap();
    }
    all.space();
    all.space();
    let log = fs::read_to_string(directory.path().join("server.log")).unwrap();
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("recovered"))
            .count(),
        2,
        "log was {log}"
    );
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("live workspaces on"))
            .count(),
        4,
        "log was {log}"
    );
}

#[test]
fn slow_discovery_gives_every_socket_a_full_turn() {
    let directory = TestDirectory::new();
    let seconds: f64 = include_str!("../src/dashboard/hub.rs")
        .split_once("const HERDR_TIMEOUT: Duration = Duration::from_secs(")
        .expect("fixture must use the configured Herdr read deadline")
        .1
        .split_once(')')
        .unwrap()
        .0
        .parse()
        .unwrap();
    let delay = (seconds * 0.6).to_string();
    for socket in ["slow-a", "slow-b"] {
        activate_on(&directory.path().join("state"), socket, "a");
        snapshot_on(directory.path(), socket, &[("a", None)]);
        fs::write(directory.path().join(format!("delay-{socket}")), &delay).unwrap();
    }
    let server = Server::start(directory.path(), &[]);
    let mut all = server.events("/api/events");
    let mut seen = HashSet::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while seen.len() < 2 && Instant::now() < deadline {
        if let Some((event, data)) = all.next(deadline.saturating_duration_since(Instant::now())) {
            assert_eq!(event, "space");
            seen.insert(data["key"].as_str().unwrap().to_owned());
        }
    }
    let expected: HashSet<_> = ["slow-a", "slow-b"]
        .into_iter()
        .map(|socket| {
            SpaceKey::of(&StateNamespace::for_space(socket, "a"))
                .as_str()
                .to_owned()
        })
        .collect();
    assert_eq!(seen, expected, "one socket was starved");
}

#[test]
fn both_routes_publish_database_changes_while_a_herdr_read_is_still_blocked() {
    let directory = TestDirectory::new();
    let state = directory.path().join("state");
    activate(&state, "a");
    let server = Server::start(directory.path(), &[("a", None)]);
    let mut all = server.events("/api/events");
    all.space();
    let mut legacy = server.events(&format!("/w/{}/events", key("a")));
    assert_eq!(legacy.next(WAIT).unwrap().0, "snapshot");
    let block = directory.path().join(format!("block-{SOCKET}"));
    let active = directory.path().join(format!("active-{SOCKET}"));
    fs::write(&block, "").unwrap();
    eventually(|| active.exists());
    let pid = fs::read_to_string(active).unwrap();
    let alive = || {
        Command::new("kill")
            .args(["-0", &pid])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    };
    assert!(alive(), "fixture read was not blocked");
    StateStore::open(&state)
        .unwrap()
        .transaction(|tx| {
            tx.create_pane_address(
                &namespace("a"),
                &address("during-read", "during-read"),
                &attribution(),
            )
        })
        .unwrap();
    let (_, legacy_frame) = legacy
        .next(WAIT)
        .expect("legacy change during blocked read");
    assert!(
        legacy_frame["participants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["alias"] == "@during-read")
    );
    assert!(
        alive(),
        "legacy publication waited for the Herdr read to end"
    );
    let changed = all.space();
    assert!(
        changed["frame"]["participants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["alias"] == "@during-read")
    );
    assert!(
        alive(),
        "all-space publication waited for the Herdr read to end"
    );
    fs::remove_file(block).unwrap();
}

#[test]
fn repeated_unavailable_socket_sends_gone_then_stops_polling_but_timeouts_keep_spaces() {
    let directory = TestDirectory::new();
    activate(&directory.path().join("state"), "a");
    let server = Server::start(directory.path(), &[("a", None)]);
    let mut all = server.events("/api/events");
    all.space();
    let transient = directory.path().join(format!("transient-{SOCKET}"));
    fs::write(&transient, "").unwrap();
    assert!(
        all.next(Duration::from_millis(1100)).is_none(),
        "timeout removed a workspace"
    );
    fs::remove_file(transient).unwrap();
    fs::write(directory.path().join(format!("down-{SOCKET}")), "").unwrap();
    assert_eq!(
        all.next(WAIT).expect("gone after repeated unavailability"),
        ("gone".to_owned(), json!({"key":key("a")}))
    );
    let before = calls(directory.path()).len();
    thread::sleep(Duration::from_millis(700));
    assert_eq!(
        calls(directory.path()).len(),
        before,
        "retired socket was polled again"
    );
}

#[test]
fn addressable_labels_are_typeable_tokens_with_actual_socket_scoped_resolution() {
    let directory = TestDirectory::new();
    activate(&directory.path().join("state"), "project");
    for id in ["colliding", "dup-a", "own-id"] {
        activate(&directory.path().join("state"), id);
    }
    activate_on(&directory.path().join("state"), "other-socket", "other");
    snapshot_on(
        directory.path(),
        "other-socket",
        &[("other", Some("My Project"))],
    );
    let live = [
        ("project", Some("My Project")),
        ("reserved", Some("All")),
        ("colliding", Some("target")),
        ("target", None),
        ("dup-a", Some("Duplicate Label")),
        ("dup-b", Some("duplicate-label")),
        ("own-id", Some("own-id")),
        ("own-id-peer", Some("own-id")),
    ];
    let server = Server::start(directory.path(), &live);
    let mut all = server.events("/api/events");
    let frames: Vec<_> = (0..live.len() + 1).map(|_| all.space()).collect();
    let project = frames
        .iter()
        .find(|frame| frame["key"] == key("project"))
        .unwrap();
    assert_eq!(project["frame"]["space"]["display_name"], "My Project");
    assert_eq!(project["frame"]["space"]["addressable_label"], "my-project");
    let other_key = SpaceKey::of(&StateNamespace::for_space("other-socket", "other"));
    let other = frames
        .iter()
        .find(|frame| frame["key"] == other_key.as_str())
        .unwrap();
    assert_eq!(
        other["frame"]["space"]["addressable_label"], "my-project",
        "another socket's label must not create ambiguity"
    );
    for id in [
        "reserved",
        "colliding",
        "dup-a",
        "dup-b",
        "own-id",
        "own-id-peer",
    ] {
        let frame = frames.iter().find(|frame| frame["key"] == key(id)).unwrap();
        assert!(
            frame["frame"]["space"]["addressable_label"].is_null(),
            "{id} offered an ambiguous/reserved token: {frame}"
        );
    }
}

#[test]
fn operations_describes_label_resolution_unknown_display_and_connection_limit() {
    let operations = include_str!("../docs/operations.md");
    for required in [
        "`my-project`",
        "reserved `all`",
        "another workspace's id",
        "same Herdr socket",
        "saved label",
        "empty string",
        "six HTTP/1.1",
    ] {
        assert!(
            operations.contains(required),
            "operations.md omits the {required} contract"
        );
    }
}

#[test]
fn unactivated_live_workspace_carries_its_id() {
    let directory = TestDirectory::new();
    activate(&directory.path().join("state"), "activated");
    let server = Server::start(directory.path(), &[("unactivated", None)]);
    let frame = server.events("/api/events").space();
    assert_eq!(frame["frame"]["space"]["workspace_id"], "unactivated");
}

#[test]
fn closed_saved_labels_and_unknown_keys_use_the_documented_display_fallbacks() {
    let directory = TestDirectory::new();
    let state = directory.path().join("state");
    activate(&state, "closed");
    fs::create_dir_all(state.join("dashboard")).unwrap();
    fs::write(
        state.join("dashboard/labels.json"),
        json!({key("closed"):"saved label"}).to_string(),
    )
    .unwrap();
    let server = Server::start(directory.path(), &[]);
    let (_, closed) = server
        .events(&format!("/w/{}/events", key("closed")))
        .next(WAIT)
        .unwrap();
    assert_eq!(closed["space"]["display_name"], "closed");
    assert_eq!(closed["space"]["label"]["text"], "saved label");
    assert!(closed["space"]["addressable_label"].is_null());
    let (_, unknown) = server
        .events(&format!("/w/{}/events", key("unknown")))
        .next(WAIT)
        .unwrap();
    assert_eq!(unknown["space"]["display_name"], "");
    assert!(unknown["space"]["addressable_label"].is_null());
    assert!(calls(directory.path()).is_empty());
}

#[test]
fn a_retired_socket_recovers_existing_and_new_workspaces_without_reconnecting() {
    let directory = TestDirectory::new();
    let state = directory.path().join("state");
    activate(&state, "a");
    let server = Server::start(directory.path(), &[("a", None)]);
    let mut all = server.events("/api/events");
    all.space();
    let down = directory.path().join(format!("down-{SOCKET}"));
    fs::write(&down, "").unwrap();
    assert_eq!(
        all.next(WAIT).unwrap(),
        ("gone".to_owned(), json!({"key":key("a")}))
    );
    fs::remove_file(down).unwrap();
    server.set_workspaces(&[("a", None), ("b", None)]);
    activate(&state, "b");
    let mut returned = HashSet::new();
    let deadline = Instant::now() + Duration::from_secs(4);
    while returned.len() < 2 && Instant::now() < deadline {
        if let Some((event, data)) = all.next(deadline.saturating_duration_since(Instant::now())) {
            assert_eq!(event, "space");
            returned.insert(data["key"].as_str().unwrap().to_owned());
        }
    }
    assert_eq!(
        returned,
        HashSet::from([key("a"), key("b")]),
        "retired socket did not recover on the existing stream"
    );
}

#[test]
fn reconnect_starts_with_current_live_workspaces_without_stale_replay() {
    let directory = TestDirectory::new();
    activate(&directory.path().join("state"), "a");
    let server = Server::start(directory.path(), &[("a", Some("old-alpha")), ("b", None)]);
    let mut first = server.events("/api/events");
    first.space();
    first.space();
    drop(first);
    thread::sleep(Duration::from_millis(1500));
    server.set_workspaces(&[("a", Some("new-alpha"))]);
    let mut reconnect = server.events("/api/events");
    let mut observed = Vec::new();
    let deadline = Instant::now() + Duration::from_millis(1500);
    while let Some((event, data)) =
        reconnect.next(deadline.saturating_duration_since(Instant::now()))
    {
        observed.push((
            event,
            data["key"].clone(),
            data["frame"]["space"]["display_name"].clone(),
        ));
    }
    assert_eq!(
        observed,
        vec![("space".to_owned(), json!(key("a")), json!("new-alpha"))],
        "reconnect replayed cached workspaces or labels"
    );
}

#[test]
fn legacy_metadata_falls_back_after_the_last_all_space_tab_closes() {
    let directory = TestDirectory::new();
    let state = directory.path().join("state");
    activate(&state, "a");
    let server = Server::start(directory.path(), &[("a", Some("alpha"))]);
    let mut all = server.events("/api/events");
    assert_eq!(all.space()["frame"]["space"]["addressable_label"], "alpha");
    let mut legacy = server.events(&format!("/w/{}/events", key("a")));
    assert_eq!(
        legacy.next(WAIT).unwrap().1["space"]["display_name"],
        "alpha"
    );
    drop(all);
    thread::sleep(Duration::from_millis(1500));
    StateStore::open(&state)
        .unwrap()
        .transaction(|tx| {
            tx.create_pane_address(
                &namespace("a"),
                &address("after-close", "after-close"),
                &attribution(),
            )
        })
        .unwrap();
    let deadline = Instant::now() + WAIT;
    loop {
        let (_, frame) = legacy
            .next(deadline.saturating_duration_since(Instant::now()))
            .expect("legacy change after watcher closed");
        if frame["participants"]
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["alias"] == "@after-close")
        {
            assert_eq!(
                frame["space"]["display_name"], "a",
                "legacy frame used stale live label"
            );
            assert!(frame["space"]["addressable_label"].is_null());
            break;
        }
    }
}

#[test]
fn socket_failure_logging_survives_all_space_subscription_sessions() {
    let directory = TestDirectory::new();
    let state = directory.path().join("state");
    activate(&state, "a");
    activate_on(&state, "dead-forever", "dead");
    fs::write(directory.path().join("down-dead-forever"), "").unwrap();
    let server = Server::start(directory.path(), &[("a", None)]);
    for _ in 0..3 {
        let mut all = server.events("/api/events");
        all.space();
        thread::sleep(Duration::from_millis(600));
        drop(all);
        thread::sleep(Duration::from_millis(1500));
    }
    let log = fs::read_to_string(directory.path().join("server.log")).unwrap();
    assert_eq!(
        log.lines()
            .filter(|line| line.contains("dead-forever unavailable"))
            .count(),
        1,
        "same failure was logged in each subscription session: {log}"
    );
}

#[test]
fn unactivated_live_workspace_is_not_addressable_until_the_store_activates_it() {
    let directory = TestDirectory::new();
    let state = directory.path().join("state");
    activate(&state, "sponsor");
    let server = Server::start(directory.path(), &[("lonely", Some("Lonely"))]);
    let mut all = server.events("/api/events");
    let initial = all.space();
    assert_eq!(initial["frame"]["space"]["workspace_id"], "lonely");
    assert!(
        initial["frame"]["space"]["addressable_label"].is_null(),
        "unactivated workspace offered a token: {initial}"
    );
    activate(&state, "lonely");
    assert_eq!(all.space()["frame"]["space"]["addressable_label"], "lonely");
    StateStore::open(&state)
        .unwrap()
        .transaction(|tx| tx.set_worktree_paused(&namespace("lonely"), true, &attribution()))
        .unwrap();
    let paused = all.space();
    assert_eq!(paused["frame"]["space"]["state"], "paused");
    assert_eq!(paused["frame"]["space"]["addressable_label"], "lonely");
    StateStore::open(&state)
        .unwrap()
        .transaction(|tx| tx.deactivate_worktree(&namespace("lonely"), &attribution()))
        .unwrap();
    assert!(all.space()["frame"]["space"]["addressable_label"].is_null());
}

fn ready_keys(event: (String, Value)) -> HashSet<String> {
    assert_eq!(event.0, "ready");
    let keys = event.1["keys"].as_array().expect("ready keys array");
    let unique: HashSet<_> = keys
        .iter()
        .map(|key| key.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(keys.len(), unique.len(), "ready repeated a key");
    unique
}

#[test]
fn ready_lists_exactly_the_three_initial_space_keys_once() {
    let directory = TestDirectory::new();
    let state = directory.path().join("state");
    activate(&state, "a");
    let server = Server::start(directory.path(), &[("a", None), ("b", None), ("c", None)]);
    let mut all = server.events("/api/events");
    let mut preceding = HashSet::new();
    for _ in 0..3 {
        let (event, data) = all.next_raw(WAIT).expect("initial space");
        assert_eq!(event, "space");
        preceding.insert(data["key"].as_str().unwrap().to_owned());
    }
    assert_eq!(preceding, HashSet::from([key("a"), key("b"), key("c")]));
    assert_eq!(
        ready_keys(all.next_raw(WAIT).expect("ready after initial spaces")),
        preceding
    );
    StateStore::open(&state)
        .unwrap()
        .transaction(|tx| {
            tx.create_pane_address(
                &namespace("a"),
                &address("after-ready", "after-ready"),
                &attribution(),
            )
        })
        .unwrap();
    let (event, data) = all.next_raw(WAIT).expect("space after database change");
    assert_eq!(event, "space");
    assert_eq!(data["key"], key("a"));
    assert!(
        all.next_raw(Duration::from_millis(400)).is_none(),
        "ready repeated after the initial replay"
    );
}

#[test]
fn ready_empty_waits_for_a_successful_empty_discovery_answer() {
    let directory = TestDirectory::new();
    activate(&directory.path().join("state"), "a");
    let block = directory.path().join(format!("block-{SOCKET}"));
    fs::write(&block, "").unwrap();
    let server = Server::start(directory.path(), &[]);
    let mut all = server.events("/api/events");
    eventually(|| directory.path().join(format!("active-{SOCKET}")).exists());
    assert!(
        all.next_raw(Duration::from_millis(150)).is_none(),
        "unknown membership was treated as known empty"
    );
    fs::remove_file(block).unwrap();
    assert_eq!(
        all.next_raw(WAIT),
        Some(("ready".to_owned(), json!({"keys":[]})))
    );
    assert!(all.next_raw(Duration::from_millis(400)).is_none());
}

#[test]
fn ready_precedes_later_space_and_gone_on_a_second_connection() {
    let directory = TestDirectory::new();
    let state = directory.path().join("state");
    activate(&state, "a");
    let server = Server::start(directory.path(), &[("a", None)]);
    let mut first = server.events("/api/events");
    assert_eq!(first.next_raw(WAIT).unwrap().0, "space");
    assert_eq!(
        ready_keys(first.next_raw(WAIT).expect("first connection ready")),
        HashSet::from([key("a")])
    );
    let mut second = server.events("/api/events");
    assert_eq!(second.next_raw(WAIT).unwrap().0, "space");
    StateStore::open(&state)
        .unwrap()
        .transaction(|tx| {
            tx.create_pane_address(
                &namespace("a"),
                &address("concurrent", "concurrent"),
                &attribution(),
            )
        })
        .unwrap();
    assert_eq!(
        ready_keys(
            second
                .next_raw(WAIT)
                .expect("second connection ready before delta")
        ),
        HashSet::from([key("a")])
    );
    assert_eq!(second.next_raw(WAIT).unwrap().0, "space");
    assert_eq!(first.next_raw(WAIT).unwrap().0, "space");
    server.set_workspaces(&[]);
    assert_eq!(
        second.next_raw(WAIT),
        Some(("gone".to_owned(), json!({"key":key("a")})))
    );
    assert!(second.next_raw(Duration::from_millis(400)).is_none());
}

#[test]
fn reconnect_ready_excludes_a_workspace_closed_while_unwatched() {
    let directory = TestDirectory::new();
    activate(&directory.path().join("state"), "a");
    let server = Server::start(directory.path(), &[("a", None), ("b", None)]);
    let mut first = server.events("/api/events");
    for _ in 0..2 {
        assert_eq!(first.next_raw(WAIT).unwrap().0, "space");
    }
    assert_eq!(
        ready_keys(first.next_raw(WAIT).expect("initial ready")),
        HashSet::from([key("a"), key("b")])
    );
    drop(first);
    thread::sleep(Duration::from_millis(1500));
    server.set_workspaces(&[("a", None)]);
    let mut reconnect = server.events("/api/events");
    let (event, space) = reconnect.next_raw(WAIT).unwrap();
    assert_eq!(event, "space");
    assert_eq!(space["key"], key("a"));
    assert_eq!(
        ready_keys(reconnect.next_raw(WAIT).expect("fresh replay ready")),
        HashSet::from([key("a")])
    );
    assert!(reconnect.next_raw(Duration::from_millis(400)).is_none());
}

#[test]
fn ready_waits_for_every_configured_socket() {
    let directory = TestDirectory::new();
    let state = directory.path().join("state");
    for socket in ["ready-a", "ready-b"] {
        activate_on(&state, socket, "a");
        snapshot_on(directory.path(), socket, &[("a", None)]);
    }
    let block = directory.path().join("block-ready-b");
    fs::write(&block, "").unwrap();
    let server = Server::start(directory.path(), &[]);
    let mut all = server.events("/api/events");
    eventually(|| directory.path().join("active-ready-b").exists());
    let mut preceding = HashSet::new();
    if let Some((event, data)) = all.next_raw(Duration::from_millis(150)) {
        assert_eq!(event, "space", "ready preceded the second socket answer");
        preceding.insert(data["key"].as_str().unwrap().to_owned());
    }
    fs::remove_file(block).unwrap();
    loop {
        let (event, data) = all.next_raw(WAIT).expect("initial replay completion");
        if event == "ready" {
            let keys = ready_keys((event, data));
            assert_eq!(keys, preceding);
            assert_eq!(keys.len(), 2);
            break;
        }
        assert_eq!(event, "space");
        preceding.insert(data["key"].as_str().unwrap().to_owned());
    }
    assert!(all.next_raw(Duration::from_millis(400)).is_none());
}

#[test]
fn ready_empty_completes_after_a_fresh_probe_of_an_already_retired_socket() {
    let directory = TestDirectory::new();
    activate(&directory.path().join("state"), "a");
    let server = Server::start(directory.path(), &[("a", None)]);
    let mut first = server.events("/api/events");
    assert_eq!(first.next_raw(WAIT).unwrap().0, "space");
    assert_eq!(
        ready_keys(first.next_raw(WAIT).unwrap()),
        HashSet::from([key("a")])
    );
    fs::write(directory.path().join(format!("down-{SOCKET}")), "").unwrap();
    assert_eq!(first.next_raw(WAIT).unwrap().0, "gone");
    drop(first);
    thread::sleep(Duration::from_millis(1500));
    let mut reconnect = server.events("/api/events");
    assert_eq!(
        reconnect.next_raw(WAIT),
        Some(("ready".to_owned(), json!({"keys":[]})))
    );
    assert!(reconnect.next_raw(Duration::from_millis(400)).is_none());
}

#[test]
fn ready_empty_completes_without_a_database_in_a_pristine_directory() {
    let directory = TestDirectory::new();
    let server = Server::start(directory.path(), &[]);
    let mut all = server.events("/api/events");
    assert_eq!(
        all.next_raw(Duration::from_millis(600)),
        Some(("ready".to_owned(), json!({"keys":[]})))
    );
    assert!(calls(directory.path()).is_empty());
    assert!(all.next_raw(Duration::from_millis(350)).is_none());
}
