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
    session: String,
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
        Self {
            child,
            port: info["port"].as_u64().unwrap() as u16,
            token: fs::read_to_string(state.join("dashboard/token")).unwrap(),
            session: fs::read_to_string(state.join("dashboard/session")).unwrap(),
        }
    }

    fn get(&self, path: &str, headers: &[(&str, &str)]) -> Response {
        let mut stream = self.connect(path, headers);
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        Response::parse(&raw)
    }

    fn connect(&self, path: &str, headers: &[(&str, &str)]) -> TcpStream {
        connect(self.port, path, headers)
    }

    fn cookie(&self) -> String {
        format!("namedrop_dashboard_{}={}", self.port, self.session)
    }

    fn events(&self, key: &SpaceKey) -> EventStream {
        events(self.port, &self.session, key)
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

fn connect(port: u16, path: &str, headers: &[(&str, &str)]) -> TcpStream {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut request = format!("GET {path} HTTP/1.1\r\n");
    if !headers.iter().any(|(name, _)| *name == "Host") {
        request.push_str(&format!("Host: 127.0.0.1:{port}\r\n"));
    }
    for (name, value) in headers {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    request.push_str("\r\n");
    stream.write_all(request.as_bytes()).unwrap();
    stream
}

fn events(port: u16, session: &str, key: &SpaceKey) -> EventStream {
    let cookie = format!("namedrop_dashboard_{port}={session}");
    EventStream {
        stream: connect(
            port,
            &format!("/w/{}/events", key.as_str()),
            &[("Cookie", &cookie)],
        ),
        buffer: String::new(),
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

fn mint_entry(state: &Path) -> String {
    use std::os::unix::fs::PermissionsExt;
    let entries = state.join("dashboard/entry");
    fs::create_dir_all(&entries).unwrap();
    fs::set_permissions(&entries, fs::Permissions::from_mode(0o700)).unwrap();
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    let nonce = format!(
        "{:016x}{:016x}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    fs::write(entries.join(&nonce), "").unwrap();
    nonce
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
        &format!("{page}?n={}", mint_entry(&state(&directory))),
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

    assert!(
        server
            .get(&format!("{page}?t={}", server.token), &[])
            .status
            .contains("401"),
        "the CLI's token never opens a page"
    );
    let enter = server.get(&format!("{page}?n={}", mint_entry(&state(&directory))), &[]);
    assert!(enter.status.contains("303"), "{}", enter.status);
    assert!(enter.headers.contains(&format!("Location: {page}\r\n")));
    let set_cookie = enter
        .headers
        .lines()
        .find(|line| line.starts_with("Set-Cookie:"))
        .unwrap();
    assert!(set_cookie.contains(&server.cookie()));
    assert!(
        !set_cookie.contains(&server.token),
        "the cookie is the session secret, not the CLI token"
    );
    assert!(set_cookie.contains("HttpOnly") && set_cookie.contains("SameSite=Strict"));

    let landed = server.get(&page, &[("Cookie", &server.cookie())]);
    assert!(landed.status.contains("200"));
    assert!(
        landed
            .headers
            .contains("Content-Security-Policy: default-src 'none'; script-src 'self'")
    );
    for directive in [
        "base-uri 'none'",
        "form-action 'none'",
        "frame-ancestors 'none'",
    ] {
        assert!(landed.headers.contains(directive), "{}", landed.headers);
    }
    assert!(landed.headers.contains("X-Frame-Options: DENY\r\n"));
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
fn connections_past_the_cap_are_turned_away() {
    let directory = TestDirectory::new();
    seed(&state(&directory), "w1", &["a"]);
    let server = Server::start(&state(&directory), None);

    let held: Vec<TcpStream> = (0..64)
        .map(|_| TcpStream::connect(("127.0.0.1", server.port)).unwrap())
        .collect();
    // The server answers before reading, so the probe sends nothing that an
    // unread close would turn into a reset.
    let mut excess = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
    let mut raw = String::new();
    excess.read_to_string(&mut raw).unwrap();
    assert!(raw.starts_with("HTTP/1.1 503"), "{raw}");

    drop(held);
    let admitted = eventually(Duration::from_secs(5), || {
        // A probe that lands before a slot frees is turned away like the
        // excess one, and its unread request makes that close a reset.
        let mut stream = server.connect("/assets/app.js", &[]);
        let mut raw = String::new();
        let outcome = stream.read_to_string(&mut raw);
        if raw.starts_with("HTTP/1.1 503") {
            return None;
        }
        outcome.unwrap();
        Response::parse(&raw).status.contains("200").then_some(())
    });
    assert!(admitted.is_some(), "slots free up once connections close");
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

fn fake_herdr(root: &Path) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let herdr = root.join("herdr");
    fs::write(
        &herdr,
        r#"#!/bin/sh
case "$1 $2" in
  "workspace list")
    printf '%s\n' '{"result":{"workspaces":[
      {"workspace_id":"w0","number":0,"label":"label-w0"},
      {"workspace_id":"w1","number":1,"label":"label-w1"},
      {"workspace_id":"w2","number":2,"label":"label-w2"},
      {"workspace_id":"w3","number":3,"label":"label-w3"},
      {"workspace_id":"w4","number":4},
      {"workspace_id":"w5","number":5,"label":"  "},
      {"workspace_id":"w6","number":6,"label":"w6"}
    ]}}'
    ;;
  *) exit 1 ;;
esac
"#,
    )
    .unwrap();
    fs::set_permissions(&herdr, fs::Permissions::from_mode(0o755)).unwrap();
    herdr
}

fn open_dashboard(root: &Path, workspace: &str) -> serde_json::Value {
    let output = Command::new(env!("CARGO_BIN_EXE_namedrop"))
        .args(["--json", "--plugin-state"])
        .arg(root.join("state"))
        .args(["--herdr-socket", SOCKET, "--herdr-workspace", workspace])
        .args(["dashboard", "--no-open"])
        .env_remove("HERDR_PANE_ID")
        .env_remove("HERDR_WORKSPACE_ID")
        .env_remove("HERDR_SOCKET_PATH")
        .env_remove("HERDR_PLUGIN_STATE_DIR")
        .env("HERDR_BIN_PATH", root.join("herdr"))
        .env("NAMEDROP_DASHBOARD_IDLE_MS", "3000")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["data"].clone()
}

fn port_of(data: &serde_json::Value) -> u16 {
    let url = data["url"].as_str().unwrap();
    let authority = url.strip_prefix("http://127.0.0.1:").unwrap();
    authority.split('/').next().unwrap().parse().unwrap()
}

fn live_servers(state: &Path) -> usize {
    let output = Command::new("pgrep")
        .args(["-f", &format!("{} dashboard-server", state.display())])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).lines().count()
}

/// Stops the server `namedrop dashboard` started, which no test owns as a child.
struct StopOnDrop(PathBuf);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        let _ = Command::new("pkill")
            .args(["-f", &format!("{} dashboard-server", self.0.display())])
            .status();
        eventually(Duration::from_secs(5), || {
            (live_servers(&self.0) == 0).then_some(())
        });
    }
}

#[test]
fn every_workspace_shares_one_server_with_its_own_page() {
    let directory = TestDirectory::new();
    let state = state(&directory);
    let _stop = StopOnDrop(state.clone());
    fake_herdr(directory.path());

    let first = open_dashboard(directory.path(), "w1");
    let second = open_dashboard(directory.path(), "w2");

    assert_eq!(first["server"], "started");
    assert_eq!(second["server"], "running");
    assert_eq!(port_of(&first), port_of(&second));
    assert_ne!(first["space_key"], second["space_key"]);
    let key = SpaceKey::of(&StateNamespace::for_space(SOCKET, "w1"));
    assert_eq!(first["space_key"], key.as_str());
    assert_eq!(live_servers(&state), 1);

    let session = fs::read_to_string(state.join("dashboard/session")).unwrap();
    let frame = events(port_of(&first), &session, &key)
        .until(Duration::from_secs(5), |e| e.contains("event: snapshot"));
    assert!(
        frame.contains(r#""label":{"text":"label-w1","truncated":false}"#),
        "the CLI records the Herdr label for the server: {frame}"
    );
}

#[test]
fn workspaces_without_a_display_label_do_not_record_their_id() {
    let directory = TestDirectory::new();
    let state = state(&directory);
    let _stop = StopOnDrop(state.clone());
    fake_herdr(directory.path());

    for workspace in ["w4", "w5", "w6", "w7"] {
        open_dashboard(directory.path(), workspace);
        assert!(
            !state.join("dashboard/labels.json").exists(),
            "workspace {workspace} recorded a missing, blank, id-only, or unlisted label"
        );
    }
}

#[test]
fn an_entry_link_works_once_and_carries_no_lasting_secret() {
    let directory = TestDirectory::new();
    let state = state(&directory);
    let _stop = StopOnDrop(state.clone());
    fake_herdr(directory.path());

    let opened = open_dashboard(directory.path(), "w1");
    let url = opened["url"].as_str().unwrap();
    let token = fs::read_to_string(state.join("dashboard/token")).unwrap();
    let session = fs::read_to_string(state.join("dashboard/session")).unwrap();
    assert!(!url.contains(&token) && !url.contains(&session), "{url}");
    let path = &url[url.find("/w/").unwrap()..];

    let port = port_of(&opened);
    let enter = |path: &str| {
        let mut raw = String::new();
        connect(port, path, &[]).read_to_string(&mut raw).unwrap();
        Response::parse(&raw)
    };
    let first = enter(path);
    assert!(first.status.contains("303"), "{}", first.status);
    assert!(
        first
            .headers
            .contains(&format!("namedrop_dashboard_{port}={session};"))
    );
    assert!(
        enter(path).status.contains("401"),
        "a consumed entry link is rejected"
    );
}

#[test]
fn an_idle_exit_rotates_the_browser_session() {
    let directory = TestDirectory::new();
    let state = state(&directory);
    let ns = seed(&state, "w1", &["a"]);
    let page = format!("/w/{}", SpaceKey::of(&ns).as_str());
    let mut first = Server::start(&state, Some(300));
    let stale_cookie = first.cookie();
    assert!(first.exits_within(Duration::from_secs(3)));

    let next = Server::start(&state, None);
    assert_eq!(next.port, first.port, "the successor reuses the port");
    assert!(
        next.get(&page, &[("Cookie", &stale_cookie)])
            .status
            .contains("401")
    );
    assert_ne!(next.session, first.session);
    assert!(
        next.get(&page, &[("Cookie", &next.cookie())])
            .status
            .contains("200")
    );
}

#[test]
fn racing_opens_converge_on_one_server() {
    let directory = TestDirectory::new();
    let state = state(&directory);
    let _stop = StopOnDrop(state.clone());
    fake_herdr(directory.path());

    let opens: Vec<_> = (0..4)
        .map(|i| {
            let root = directory.path().to_owned();
            thread::spawn(move || port_of(&open_dashboard(&root, &format!("w{i}"))))
        })
        .collect();
    let ports: Vec<u16> = opens.into_iter().map(|open| open.join().unwrap()).collect();

    assert!(ports.windows(2).all(|pair| pair[0] == pair[1]), "{ports:?}");
    thread::sleep(Duration::from_millis(300));
    assert_eq!(live_servers(&state), 1);
}
