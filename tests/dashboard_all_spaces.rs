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
            "#!/bin/sh\n[ \"$1 $2\" = \"api snapshot\" ] || exit 1\ncat \"$ND_TEST_SNAPSHOT\"\n",
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
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
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
    assert_eq!(new["frame"]["space"]["workspace_id"], "");
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
        assert_eq!(space["addressable_label"], space["display_name"]);
    }
    assert!(events.next(Duration::from_millis(350)).is_none());

    let mut legacy = server.events(&format!("/w/{}/events", key("a")));
    let (event, mut frame) = legacy.next(WAIT).unwrap();
    assert_eq!(event, "snapshot");
    assert_eq!(frame["space"]["display_name"], "Same Label");
    assert_eq!(frame["space"]["addressable_label"], "Same Label");
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
