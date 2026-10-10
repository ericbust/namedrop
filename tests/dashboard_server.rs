mod support;

use namedrop::dashboard::snapshot::SpaceKey;
use namedrop::{
    ActorKind, AgentSessionId, DeliveryId, MessageDraft, MessageId, MessageRecipientDraft,
    MutationAttribution, PaneAddressId, PaneBindingId, StateNamespace, StateStore, ThreadId,
};
use std::{
    fs,
    io::{Read, Write},
    net::TcpStream,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use support::{TestDirectory, address};

const SOCKET: &str = "socket-a";

struct Server {
    child: Child,
    port: u16,
    token: String,
}

impl Server {
    fn start(state: &Path, idle_ms: Option<u64>) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
        command
            .arg("--plugin-state")
            .arg(state)
            .arg("dashboard-server")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(idle) = idle_ms {
            command.env("NAMEDROP_DASHBOARD_IDLE_MS", idle.to_string());
        }
        let child = command.spawn().unwrap();
        let info = eventually(Duration::from_secs(10), || {
            let info: serde_json::Value =
                serde_json::from_slice(&fs::read(state.join("dashboard/server.json")).ok()?)
                    .ok()?;
            (info["pid"] == child.id()).then_some(info)
        })
        .expect("server.json published by the started server");
        let token = fs::read_to_string(state.join("dashboard/token")).unwrap();
        Self {
            child,
            port: info["port"].as_u64().unwrap() as u16,
            token,
        }
    }

    fn get(&self, path: &str, headers: &[(&str, &str)]) -> Response {
        let mut stream = self.connect(path, headers);
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        Response::parse(&raw)
    }

    fn connect(&self, path: &str, headers: &[(&str, &str)]) -> TcpStream {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
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

    fn cookie(&self) -> String {
        format!("namedrop_dashboard_{}={}", self.port, self.token)
    }

    fn events(&self, key: &SpaceKey) -> EventStream {
        let cookie = self.cookie();
        EventStream {
            stream: self.connect(
                &format!("/w/{}/events", key.as_str()),
                &[("Cookie", &cookie)],
            ),
            buffer: String::new(),
        }
    }

    fn exits_within(&mut self, limit: Duration) -> bool {
        eventually(limit, || self.child.try_wait().unwrap()).is_some()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Response {
    status: String,
    headers: String,
    body: String,
}

impl Response {
    fn parse(raw: &str) -> Self {
        let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((raw, ""));
        let (status, headers) = head.split_once("\r\n").unwrap_or((head, ""));
        Self {
            status: status.to_owned(),
            headers: headers.to_owned(),
            body: body.to_owned(),
        }
    }
}

struct EventStream {
    stream: TcpStream,
    buffer: String,
}

impl EventStream {
    /// Reads until one whole event satisfies `wanted`, or fails after `limit`.
    fn until(&mut self, limit: Duration, wanted: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + limit;
        loop {
            while let Some(end) = self.buffer.find("\n\n") {
                let event: String = self.buffer.drain(..end + 2).collect();
                if wanted(&event) {
                    return event;
                }
            }
            assert!(
                Instant::now() < deadline,
                "no matching event before the deadline"
            );
            let mut chunk = [0u8; 65536];
            self.stream
                .set_read_timeout(Some(
                    deadline
                        .saturating_duration_since(Instant::now())
                        .max(Duration::from_millis(10)),
                ))
                .unwrap();
            match self.stream.read(&mut chunk) {
                Ok(0) => panic!("stream closed before a matching event: {}", self.buffer),
                Ok(n) => self.buffer.push_str(&String::from_utf8_lossy(&chunk[..n])),
                Err(_) => {}
            }
        }
    }
}

fn eventually<T>(limit: Duration, mut probe: impl FnMut() -> Option<T>) -> Option<T> {
    let deadline = Instant::now() + limit;
    loop {
        if let Some(value) = probe() {
            return Some(value);
        }
        if Instant::now() >= deadline {
            return None;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn attribution() -> MutationAttribution {
    MutationAttribution::new(ActorKind::Plugin, "test", 10)
}

fn seed(state: &Path, space: &str, aliases: &[&str]) -> StateNamespace {
    let namespace = StateNamespace::for_space(SOCKET, space);
    StateStore::open(state)
        .unwrap()
        .transaction(|tx| {
            tx.activate_worktree(&namespace, &attribution())?;
            for alias in aliases {
                let id = format!("address-{alias}");
                tx.create_pane_address(&namespace, &address(&id, alias), &attribution())?;
                tx.bind_pane(
                    &namespace,
                    &PaneBindingId::new(format!("binding-{alias}")),
                    &PaneAddressId::new(&id),
                    &format!("pane-{alias}"),
                    &attribution(),
                )?;
                tx.observe_agent_session(
                    &namespace,
                    &format!("observation-{alias}"),
                    &PaneAddressId::new(&id),
                    &AgentSessionId::new(format!("session-{alias}")),
                    "idle",
                    &attribution(),
                )?;
            }
            tx.create_thread(
                &namespace,
                &ThreadId::new("thread-1"),
                "Review",
                &attribution(),
            )
        })
        .unwrap();
    namespace
}

fn send(state: &Path, namespace: &StateNamespace, message: &str) {
    StateStore::open(state)
        .unwrap()
        .transaction(|tx| {
            tx.append_message(
                namespace,
                &MessageDraft {
                    thread_id: ThreadId::new("thread-1"),
                    message_id: MessageId::new(message),
                    in_reply_to: None,
                    author_address_id: PaneAddressId::new("address-a"),
                    author_kind: ActorKind::AgentSession,
                    author_alias_snapshot: "a".to_owned(),
                    author_agent_session_snapshot: Some(AgentSessionId::new("session-a")),
                    request_id: format!("request-{message}"),
                    authored_source: "@b please look".to_owned(),
                    body: "please look".to_owned(),
                    grammar_version: "mention/v1".to_owned(),
                    recipients: vec![MessageRecipientDraft {
                        address_id: PaneAddressId::new("address-b"),
                        alias_snapshot: "b".to_owned(),
                        agent_session_snapshot: Some(AgentSessionId::new("session-b")),
                        delivery_id: DeliveryId::new(format!("delivery-{message}")),
                    }],
                    context_references: Vec::new(),
                },
                &attribution(),
            )
            .map(|_| ())
        })
        .unwrap();
}

fn state(directory: &TestDirectory) -> PathBuf {
    directory.path().join("state")
}

#[test]
fn pages_require_the_token_and_a_loopback_host() {
    let directory = TestDirectory::new();
    let ns = seed(&state(&directory), "w1", &["a", "b"]);
    let server = Server::start(&state(&directory), None);
    let page = format!("/w/{}", SpaceKey::of(&ns).as_str());

    assert!(server.get(&page, &[]).status.contains("401"));
    assert!(
        server
            .get(&page, &[("Cookie", "namedrop_dashboard_1=wrong")])
            .status
            .contains("401")
    );
    let rebinding = server.get(
        &format!("{page}?t={}", server.token),
        &[("Host", &format!("evil.example:{}", server.port))],
    );
    assert!(rebinding.status.contains("403"));
    let cross_site = server.get(
        &page,
        &[
            ("Cookie", &server.cookie()),
            ("Sec-Fetch-Site", "cross-site"),
        ],
    );
    assert!(cross_site.status.contains("403"));

    let enter = server.get(&format!("{page}?t={}", server.token), &[]);
    assert!(enter.status.contains("303"), "{}", enter.status);
    assert!(enter.headers.contains(&format!("Location: {page}\r\n")));
    let set_cookie = enter
        .headers
        .lines()
        .find(|line| line.starts_with("Set-Cookie:"))
        .unwrap();
    assert!(set_cookie.contains(&server.cookie()));
    assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Strict"));

    let landed = server.get(&page, &[("Cookie", &server.cookie())]);
    assert!(landed.status.contains("200"));
    assert!(
        landed
            .headers
            .contains("Content-Security-Policy: default-src 'none'; script-src 'self'")
    );
    assert!(landed.body.contains("/assets/app.js"));
    assert!(server.get("/assets/app.js", &[]).status.contains("200"));
    assert!(server.get("/favicon.ico", &[]).status.contains("204"));
    assert!(
        server
            .get(&format!("/healthz?t={}", server.token), &[])
            .body
            .contains("instance")
    );
    assert!(
        server
            .get("/w/not-a-key", &[("Cookie", &server.cookie())])
            .status
            .contains("404")
    );
}

#[test]
fn the_event_stream_sends_the_space_then_each_change() {
    let directory = TestDirectory::new();
    let state = state(&directory);
    let ns = seed(&state, "w1", &["a", "b"]);
    seed(&state, "w2", &["other"]);
    let server = Server::start(&state, None);
    let mut events = server.events(&SpaceKey::of(&ns));

    let first = events.until(Duration::from_secs(5), |e| e.contains("event: snapshot"));
    assert!(first.contains("\"alias\":\"@b\""));
    assert!(!first.contains("@other"), "another space's lanes leaked");

    send(&state, &ns, "message-live-1");
    events.until(Duration::from_secs(2), |e| e.contains("message-live-1"));

    let spaces = server.get("/api/spaces", &[("Cookie", &server.cookie())]);
    let spaces: serde_json::Value = serde_json::from_str(&spaces.body).unwrap();
    assert_eq!(spaces.as_array().unwrap().len(), 2);
    assert_eq!(spaces[0]["key"], SpaceKey::of(&ns).as_str());
    assert_eq!(spaces[0]["workspace_id"], "w1");
    assert_eq!(spaces[0]["state"], "active");
}

#[test]
fn an_unregistered_key_streams_an_unknown_space() {
    let directory = TestDirectory::new();
    seed(&state(&directory), "w1", &["a"]);
    let server = Server::start(&state(&directory), None);
    let key = SpaceKey::of(&StateNamespace::for_space(SOCKET, "never-activated"));

    let first = server
        .events(&key)
        .until(Duration::from_secs(5), |e| e.contains("event: snapshot"));

    assert!(first.contains("\"state\":\"unknown\""));
}

#[test]
fn the_server_releases_the_update_gate_and_restarts_for_an_update() {
    let directory = TestDirectory::new();
    let state = state(&directory);
    let ns = seed(&state, "w1", &["a", "b"]);
    fs::create_dir_all(state.join("runtime")).unwrap();
    let mut server = Server::start(&state, None);

    let gate = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(state.join("runtime/commands.lock"))
        .unwrap();
    assert!(
        eventually(Duration::from_millis(100), || gate.try_lock().ok()).is_some(),
        "an updater can take the command gate while the dashboard runs"
    );
    gate.unlock().unwrap();

    let mut events = server.events(&SpaceKey::of(&ns));
    events.until(Duration::from_secs(5), |e| e.contains("event: snapshot"));
    fs::write(state.join("runtime/migration.json"), "{}").unwrap();

    events.until(Duration::from_secs(2), |e| e.contains("event: restarting"));
    assert!(server.exits_within(Duration::from_secs(2)));
    assert!(
        eventually(Duration::from_secs(5), || {
            fs::read_to_string(state.join("dashboard/server.log"))
                .ok()
                .filter(|log| log.contains("activation was interrupted"))
        })
        .is_some(),
        "the successor waited on the update gate and stopped on the unfinished migration"
    );
}

#[test]
fn the_server_stops_once_its_last_tab_closes() {
    let directory = TestDirectory::new();
    let ns = seed(&state(&directory), "w1", &["a"]);
    let mut server = Server::start(&state(&directory), Some(300));
    let mut events = server.events(&SpaceKey::of(&ns));
    events.until(Duration::from_secs(5), |e| e.contains("event: snapshot"));
    thread::sleep(Duration::from_millis(600));
    assert!(
        server.child.try_wait().unwrap().is_none(),
        "an open tab keeps it running"
    );

    drop(events);

    assert!(server.exits_within(Duration::from_secs(3)));
}
