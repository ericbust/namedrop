#[path = "support/binary.rs"]
mod binary;

#[path = "support/cleanup.rs"]
mod cleanup;
#[path = "support/executable.rs"]
mod executable;

mod support;

use namedrop::dashboard::snapshot::SpaceKey;
use namedrop::{
    ActorKind, AgentSessionId, DeliveryAttemptId, DeliveryId, DeliveryState, LeaseDraft,
    MessageDraft, MessageId, MessageRecipientDraft, MutationAttribution, PaneAddressId,
    PaneBindingId, StateNamespace, StateStore, ThreadId,
};
use serde_json::Value;
use std::{
    collections::HashSet,
    fs,
    io::{Read, Write},
    net::TcpStream,
    path::Path,
    process::{Child, Stdio},
    thread,
    time::{Duration, Instant},
};
use support::{TestDirectory, address};

const SOCKET: &str = "history-socket";

struct Server {
    child: Child,
    port: u16,
    session: String,
}
impl Server {
    fn start(state: &Path) -> Self {
        Self::start_binary(state, Path::new(crate::binary::namedrop_path()))
    }
    fn start_binary(state: &Path, binary: &Path) -> Self {
        Self::start_configured(state, binary, None)
    }
    fn start_configured(state: &Path, binary: &Path, herdr: Option<&Path>) -> Self {
        let mut command = crate::binary::command(binary);
        if let Some(herdr) = herdr {
            command.env("HERDR_BIN_PATH", herdr);
        }
        let child = command
            .arg("--plugin-state")
            .arg(state)
            .arg("dashboard-server")
            .env("TMPDIR", state.parent().unwrap())
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let info = loop {
            if let Ok(bytes) = fs::read(state.join("dashboard/server.json"))
                && let Ok(value) = serde_json::from_slice::<Value>(&bytes)
                && value["pid"] == child.id()
            {
                break value;
            }
            assert!(Instant::now() < deadline, "dashboard server did not start");
            thread::sleep(Duration::from_millis(20));
        };
        Self {
            child,
            port: info["port"].as_u64().unwrap() as u16,
            session: fs::read_to_string(state.join("dashboard/session")).unwrap(),
        }
    }
    fn get(&self, path: &str, cookie: bool) -> (u16, Value) {
        let raw = self.raw(path, cookie);
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        let status = head.split_whitespace().nth(1).unwrap().parse().unwrap();
        (status, serde_json::from_str(body).unwrap_or(Value::Null))
    }
    fn raw(&self, path: &str, cookie: bool) -> String {
        self.raw_with_headers(path, cookie, "")
    }
    fn raw_with_headers(&self, path: &str, cookie: bool, headers: &str) -> String {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut request = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n", self.port);
        if cookie {
            request.push_str(&format!(
                "Cookie: namedrop_dashboard_{}={}\r\n",
                self.port, self.session
            ));
        }
        request.push_str(headers);
        request.push_str("\r\n");
        stream.write_all(request.as_bytes()).unwrap();
        let mut result = String::new();
        stream.read_to_string(&mut result).unwrap();
        result
    }
    fn snapshot(&self, key: &SpaceKey) -> Value {
        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let request = format!(
            "GET /w/{}/events HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: namedrop_dashboard_{}={}\r\n\r\n",
            key.as_str(),
            self.port,
            self.port,
            self.session
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut buffer = String::new();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut chunk = [0; 65536];
            let n = stream.read(&mut chunk).unwrap();
            buffer.push_str(&String::from_utf8_lossy(&chunk[..n]));
            if let Some(line) = buffer.lines().find_map(|line| line.strip_prefix("data: "))
                && let Ok(frame) = serde_json::from_str::<Value>(line)
            {
                return frame;
            }
            assert!(n > 0 && Instant::now() < deadline, "no snapshot: {buffer}");
        }
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn at(ms: u64) -> MutationAttribution {
    MutationAttribution::new(ActorKind::Plugin, "history-test", ms)
}
fn setup(state: &Path, space: &str) -> StateNamespace {
    setup_on_socket(state, SOCKET, space)
}
fn setup_on_socket(state: &Path, socket: &str, space: &str) -> StateNamespace {
    let ns = StateNamespace::for_space(socket, space);
    StateStore::open(state)
        .unwrap()
        .transaction(|tx| {
            tx.activate_worktree(&ns, &at(1))?;
            for alias in ["a", "b"] {
                let id = format!("address-{alias}");
                tx.create_pane_address(&ns, &address(&id, alias), &at(1))?;
                tx.bind_pane(
                    &ns,
                    &PaneBindingId::new(format!("binding-{alias}")),
                    &PaneAddressId::new(&id),
                    &format!("pane-{alias}"),
                    &at(1),
                )?;
                tx.observe_agent_session(
                    &ns,
                    &format!("obs-{alias}"),
                    &PaneAddressId::new(&id),
                    &AgentSessionId::new(format!("session-{alias}")),
                    "idle",
                    &at(1),
                )?;
            }
            tx.create_thread(&ns, &ThreadId::new("thread-1"), "History", &at(1))
        })
        .unwrap();
    ns
}
fn append(state: &Path, ns: &StateNamespace, start: usize, count: usize, body: &str) {
    append_recipients(state, ns, start, count, body, false);
}
fn append_recipients(
    state: &Path,
    ns: &StateNamespace,
    start: usize,
    count: usize,
    body: &str,
    alternate: bool,
) {
    append_in_thread(state, ns, start, count, body, alternate, "thread-1");
}
#[allow(clippy::too_many_arguments)]
fn append_in_thread(
    state: &Path,
    ns: &StateNamespace,
    start: usize,
    count: usize,
    body: &str,
    alternate: bool,
    thread_id: &str,
) {
    StateStore::open(state)
        .unwrap()
        .transaction(|tx| {
            for index in start..start + count {
                let alias = if alternate && index % 2 == 0 {
                    "a"
                } else {
                    "b"
                };
                tx.append_message(
                    ns,
                    &MessageDraft {
                        thread_id: ThreadId::new(thread_id),
                        message_id: MessageId::new(format!("message-{index}")),
                        in_reply_to: (index == 119).then(|| MessageId::new("message-118")),
                        author_address_id: PaneAddressId::new("address-a"),
                        author_kind: ActorKind::AgentSession,
                        author_alias_snapshot: "a".into(),
                        author_agent_session_snapshot: Some(AgentSessionId::new("session-a")),
                        request_id: format!("request-{index}"),
                        authored_source: "@b test".into(),
                        body: body.into(),
                        grammar_version: if index == 119 {
                            "result-set/v1"
                        } else {
                            "mention/v1"
                        }
                        .into(),
                        recipients: vec![MessageRecipientDraft {
                            address_id: PaneAddressId::new(format!("address-{alias}")),
                            alias_snapshot: alias.into(),
                            agent_session_snapshot: Some(AgentSessionId::new(format!(
                                "session-{alias}"
                            ))),
                            delivery_id: DeliveryId::new(format!("delivery-{index}")),
                        }],
                        context_references: Vec::new(),
                    },
                    &at(10),
                )
                .unwrap();
            }
            Ok(())
        })
        .unwrap();
}
fn path(key: &SpaceKey, query: &str) -> String {
    format!("/w/{}/messages{query}", key.as_str())
}

#[test]
fn full_walk_matches_live_boundary_and_ignores_new_insertions() {
    let dir = TestDirectory::new();
    let state = dir.path().join("state");
    let ns = setup(&state, "w1");
    append(&state, &ns, 0, 400, "body");
    let server = Server::start(&state);
    let key = SpaceKey::of(&ns);
    let live = server.snapshot(&key);
    let live_rows = live["messages"].as_array().unwrap();
    assert_eq!(live_rows.len(), 150);
    let (_, newest) = server.get(&path(&key, "?limit=100"), true);
    assert_eq!(newest.as_array().unwrap()[0], live_rows[149]);
    let mut before = None;
    let mut ids = Vec::new();
    for page in 0..4 {
        let query = before
            .as_ref()
            .map_or("?limit=100".to_owned(), |id: &String| {
                format!("?before={id}&limit=100")
            });
        let (status, rows) = server.get(&path(&key, &query), true);
        assert_eq!(status, 200);
        let rows = rows.as_array().unwrap();
        assert_eq!(rows.len(), 100);
        if page == 0 {
            append(&state, &ns, 400, 1, "new arrival");
        }
        for row in rows {
            ids.push(row["message_id"].as_str().unwrap().to_owned());
        }
        before = Some(
            rows.last().unwrap()["message_id"]
                .as_str()
                .unwrap()
                .to_owned(),
        );
    }
    assert_eq!(ids.len(), 400);
    assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 400);
    assert_eq!(
        ids,
        (0..400)
            .rev()
            .map(|index| format!("message-{index}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        server.get(
            &path(&key, &format!("?before={}&limit=100", before.unwrap())),
            true
        ),
        (200, Value::Array(vec![]))
    );
    let boundary = live_rows.first().unwrap()["message_id"].as_str().unwrap();
    let (_, older) = server.get(&path(&key, &format!("?before={boundary}&limit=1")), true);
    assert_eq!(older[0]["message_id"], "message-249");
    assert_eq!(newest[0]["legs"].as_array().unwrap().len(), 1);
}

struct Fixture {
    _dir: TestDirectory,
    state: std::path::PathBuf,
    key: SpaceKey,
    server: Server,
}
impl Fixture {
    fn new(count: usize, body: &str) -> Self {
        let dir = TestDirectory::new();
        let state = dir.path().join("state");
        let ns = setup(&state, "w1");
        if count > 0 {
            append(&state, &ns, 0, count, body);
        }
        let key = SpaceKey::of(&ns);
        let server = Server::start(&state);
        Self {
            _dir: dir,
            state,
            key,
            server,
        }
    }
    fn get(&self, query: &str) -> (u16, Value) {
        self.server.get(&path(&self.key, query), true)
    }
}

#[test]
fn empty_workspace_returns_empty_page() {
    let f = Fixture::new(0, "body");
    assert_eq!(f.get(""), (200, Value::Array(vec![])));
}

#[test]
fn unknown_and_foreign_cursors_return_404() {
    let f = Fixture::new(1, "body");
    assert_eq!(f.get("?before=missing").0, 404);
    let other = setup(&f.state, "w2");
    append(&f.state, &other, 900, 1, "other");
    assert_eq!(f.get("?before=message-900").0, 404);
}

#[test]
fn limits_default_and_clamp() {
    let f = Fixture::new(120, "body");
    assert_eq!(f.get("?limit=0").1.as_array().unwrap().len(), 1);
    assert_eq!(f.get("?limit=500").1.as_array().unwrap().len(), 100);
    assert_eq!(f.get("").1.as_array().unwrap().len(), 50);
}

#[test]
fn malformed_and_duplicate_query_values_return_400() {
    let f = Fixture::new(1, "body");
    for query in [
        "?limit=bogus",
        "?limit=1&limit=2",
        "?before=one&before=two",
        "?before=%ZZ",
        "?before=",
    ] {
        assert_eq!(f.get(query).0, 400, "{query}");
    }
    assert_eq!(f.get("?before=message%2D0").1, Value::Array(vec![]));
}

#[test]
fn session_cookie_is_required_even_with_nonce() {
    let f = Fixture::new(1, "body");
    assert_eq!(f.server.get(&path(&f.key, ""), false).0, 401);
    assert_eq!(f.server.get(&path(&f.key, "?n=bogus"), false).0, 401);
}

#[test]
fn long_body_and_thread_fields_match_live_frame() {
    let f = Fixture::new(120, &"x".repeat(2101));
    let (_, rows) = f.get("?limit=1");
    let row = &rows[0];
    assert_eq!(row["thread_id"], "thread-1");
    assert_eq!(row["in_reply_to"], "message-118");
    assert_eq!(row["body"]["text"].as_str().unwrap().chars().count(), 2000);
    assert_eq!(row["body"]["truncated"], true);
    assert_eq!(row["legs"].as_array().unwrap().len(), 1);
    let live = f.server.snapshot(&f.key);
    assert_eq!(row, live["messages"].as_array().unwrap().last().unwrap());
}

#[test]
fn planted_api_key_is_redacted() {
    let key = format!("sk-{}", "a".repeat(30));
    let f = Fixture::new(1, &format!("secret {key} tail"));
    let (_, rows) = f.get("?limit=1");
    let text = rows[0]["body"]["text"].as_str().unwrap();
    assert!(text.contains("[REDACTED TOKEN]"), "{text}");
    assert!(!text.contains(&key));
    let (status, context) = f.server.get(
        &format!("/w/{}/messages/message-0/context", f.key.as_str()),
        true,
    );
    assert_eq!(status, 200);
    assert_eq!(context["messages"][0], rows[0]);
}

#[test]
fn history_reports_real_delivery_stages() {
    for stage in ["delivered", "working", "replied", "completed", "held"] {
        let f = Fixture::new(1, "request");
        let ns = StateNamespace::for_space(SOCKET, "w1");
        let attempt = DeliveryAttemptId::new("attempt-0");
        StateStore::open(&f.state)
            .unwrap()
            .transaction(|tx| {
                let leased = tx.lease_mailbox_head(
                    &ns,
                    &PaneAddressId::new("address-b"),
                    &LeaseDraft {
                        attempt_id: attempt.clone(),
                        binding_id: PaneBindingId::new("binding-b"),
                        agent_session_id: AgentSessionId::new("session-b"),
                        retry_of_attempt_id: None,
                        owner: "history-test".into(),
                        acquired_at_ms: 20,
                        expires_at_ms: 100_000,
                    },
                    &at(20),
                )?;
                assert!(leased.is_some());
                tx.transition_delivery_attempt(&ns, &attempt, DeliveryState::Submitted, &at(21))
            })
            .unwrap();

        if matches!(stage, "working" | "replied" | "completed") {
            StateStore::open(&f.state)
                .unwrap()
                .transaction(|tx| {
                    tx.acknowledge_delivery_attempt(
                        &ns,
                        &attempt,
                        "ack-0",
                        &AgentSessionId::new("session-b"),
                        &at(22),
                    )
                })
                .unwrap();
        }
        if stage == "replied" {
            StateStore::open(&f.state)
                .unwrap()
                .transaction(|tx| {
                    tx.append_message(
                        &ns,
                        &MessageDraft {
                            thread_id: ThreadId::new("thread-1"),
                            message_id: MessageId::new("reply-0"),
                            in_reply_to: Some(MessageId::new("message-0")),
                            author_address_id: PaneAddressId::new("address-b"),
                            author_kind: ActorKind::AgentSession,
                            author_alias_snapshot: "b".into(),
                            author_agent_session_snapshot: Some(AgentSessionId::new("session-b")),
                            request_id: "reply-request-0".into(),
                            authored_source: "@a response".into(),
                            body: "response".into(),
                            grammar_version: "mention/v1".into(),
                            recipients: vec![MessageRecipientDraft {
                                address_id: PaneAddressId::new("address-a"),
                                alias_snapshot: "a".into(),
                                agent_session_snapshot: Some(AgentSessionId::new("session-a")),
                                delivery_id: DeliveryId::new("reply-delivery-0"),
                            }],
                            context_references: Vec::new(),
                        },
                        &at(23),
                    )?;
                    tx.record_reply_disposition(
                        &ns,
                        &attempt,
                        &MessageId::new("reply-0"),
                        &AgentSessionId::new("session-b"),
                        &at(24),
                    )
                })
                .unwrap();
        }
        if stage == "completed" {
            StateStore::open(&f.state)
                .unwrap()
                .transaction(|tx| {
                    tx.transition_delivery_attempt(&ns, &attempt, DeliveryState::Settled, &at(25))
                })
                .unwrap();
        }
        if stage == "held" {
            StateStore::open(&f.state)
                .unwrap()
                .transaction(|tx| tx.hold_delivery_attempt(&ns, &attempt, &at(26)))
                .unwrap();
        }

        let (status, rows) = f.get("");
        assert_eq!(status, 200, "{stage}");
        let row = rows
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["message_id"] == "message-0")
            .unwrap();
        let expected = match stage {
            "delivered" => serde_json::json!({"stage": "delivered"}),
            "working" => serde_json::json!({"stage": "working", "acked_at_ms": 22}),
            "replied" => serde_json::json!({"stage": "replied", "reply_id": "reply-0"}),
            "completed" => serde_json::json!({"stage": "completed", "kind": "settled"}),
            "held" => serde_json::json!({"stage": "held"}),
            _ => unreachable!(),
        };
        assert_eq!(
            row["legs"],
            serde_json::json!([{"to": "address-b", "stage": expected}]),
            "{stage}"
        );
    }
}

#[test]
fn history_legs_are_scoped_to_their_worktree() {
    let dir = TestDirectory::new();
    let state = dir.path().join("state");
    let first = setup(&state, "w1");
    let second = setup(&state, "w2");
    append_recipients(&state, &first, 0, 1, "first", true);
    append(&state, &second, 0, 1, "second");
    let server = Server::start(&state);

    for (ns, recipient) in [(&first, "address-a"), (&second, "address-b")] {
        let (status, rows) = server.get(&path(&SpaceKey::of(ns), ""), true);
        assert_eq!(status, 200);
        assert_eq!(rows.as_array().unwrap().len(), 1);
        assert_eq!(rows[0]["message_id"], "message-0");
        assert_eq!(
            rows[0]["legs"],
            serde_json::json!([{"to": recipient, "stage": {"stage": "queued"}}])
        );
    }
}

#[test]
#[ignore = "release HTTP benchmark; run with --release --ignored --nocapture"]
fn history_http_benchmark() {
    if cfg!(debug_assertions) {
        panic!("run this benchmark with --release");
    }
    let binary = std::env::var_os("NAMEDROP_HISTORY_BENCH_BINARY")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate::binary::namedrop_path().into());
    for count in [10_000, 40_000] {
        let dir = TestDirectory::new();
        let state = dir.path().join("state");
        let ns = setup(&state, "benchmark");
        append_recipients(&state, &ns, 0, count, "benchmark body", true);
        let key = SpaceKey::of(&ns);
        for run in 1..=3 {
            let server = Server::start_binary(&state, &binary);
            let mut elapsed = Vec::new();
            for request in 0..=100 {
                let cursor = count - 1 - (request * 379 % (count - 51));
                let started = Instant::now();
                let (status, rows) = server.get(
                    &path(&key, &format!("?before=message-{cursor}&limit=50")),
                    true,
                );
                let duration = started.elapsed();
                assert_eq!(status, 200);
                let rows = rows.as_array().unwrap();
                assert_eq!(rows.len(), 50);
                for (offset, row) in rows.iter().enumerate() {
                    let index = cursor - 1 - offset;
                    assert_eq!(row["message_id"], format!("message-{index}"));
                    assert_eq!(row["body"]["text"], "benchmark body");
                    let alias = if index % 2 == 0 { "a" } else { "b" };
                    assert_eq!(
                        row["legs"],
                        serde_json::json!([{
                            "to": format!("address-{alias}"),
                            "stage": {"stage": "queued"}
                        }])
                    );
                }
                if request > 0 {
                    elapsed.push(duration);
                }
            }
            elapsed.sort_unstable();
            eprintln!(
                "history_http count={count} run={run} samples={} p95_ms={:.3} load={} binary={}",
                elapsed.len(),
                elapsed[94].as_secs_f64() * 1000.0,
                fs::read_to_string("/proc/loadavg").unwrap().trim(),
                binary.display(),
            );
        }
    }
}

#[test]
fn targeted_context_keeps_an_evicted_target_root_and_recent_messages() {
    let dir = TestDirectory::new();
    let state = dir.path().join("state");
    let ns = setup(&state, "w1");
    append(&state, &ns, 0, 240, "Context");
    let server = Server::start(&state);
    let key = SpaceKey::of(&ns);
    let route = format!("/w/{}/messages/message-20/context", key.as_str());
    assert_eq!(server.get(&route, false).0, 401);
    assert_eq!(server.get(&format!("{route}?n=bogus"), false).0, 401);
    for headers in [
        "Origin: http://foreign.invalid\r\n",
        "Sec-Fetch-Site: same-site\r\n",
        "Sec-Fetch-Site: cross-site\r\n",
    ] {
        let response = server.raw_with_headers(&route, true, headers);
        assert!(
            response.starts_with("HTTP/1.1 403"),
            "{}",
            response.lines().next().unwrap()
        );
    }
    let (status, context) = server.get(&route, true);
    assert_eq!(status, 200);
    assert_eq!(context["thread_id"], "thread-1");
    assert_eq!(context["message_count"], 240);
    assert_eq!(context["earlier_omitted"], true);
    let messages = context["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 102);
    assert_eq!(messages[0]["message_id"], "message-0");
    assert_eq!(messages[1]["message_id"], "message-20");
    assert_eq!(messages[101]["message_id"], "message-239");
    assert_eq!(messages[1]["legs"][0]["to"], "address-b");
    assert_eq!(
        server
            .get(
                &format!("/w/{}/messages/missing/context", key.as_str()),
                true
            )
            .0,
        404
    );
}

#[test]
fn targeted_context_is_thread_and_namespace_scoped_and_uses_the_safe_projection() {
    let dir = TestDirectory::new();
    let state = dir.path().join("state");
    let first = setup(&state, "w1");
    let second = setup(&state, "w2");
    append_recipients(&state, &first, 0, 1, "first namespace", true);
    append(&state, &second, 0, 2, "second namespace");
    let third = setup_on_socket(&state, "other-socket", "w1");
    append(&state, &third, 0, 1, "other socket");
    StateStore::open(&state)
        .unwrap()
        .transaction(|tx| tx.create_thread(&first, &ThreadId::new("other-thread"), "Other", &at(1)))
        .unwrap();
    append_in_thread(&state, &first, 99, 1, "other thread", false, "other-thread");
    let server = Server::start(&state);
    for (ns, body, recipient, thread) in [
        (&first, "first namespace", "address-a", "thread-1"),
        (&second, "second namespace", "address-b", "thread-1"),
        (&third, "other socket", "address-b", "thread-1"),
    ] {
        let route = format!(
            "/w/{}/messages/message-0/context",
            SpaceKey::of(ns).as_str()
        );
        let (status, context) = server.get(&route, true);
        assert_eq!(status, 200);
        assert_eq!(context["thread_id"], thread);
        assert_eq!(context["earlier_omitted"], false);
        assert_eq!(context["message_count"], if ns == &second { 2 } else { 1 });
        assert_eq!(context["messages"][0]["body"]["text"], body);
        assert_eq!(context["messages"][0]["legs"][0]["to"], recipient);
    }
    assert_eq!(
        server
            .get(
                &format!(
                    "/w/{}/messages/%6Dessage-1/context",
                    SpaceKey::of(&second).as_str()
                ),
                true
            )
            .0,
        200
    );
    assert_eq!(
        server
            .get(
                &format!(
                    "/w/{}/messages/%6Dessage-1/context",
                    SpaceKey::of(&first).as_str()
                ),
                true
            )
            .0,
        404
    );
}

#[test]
fn direct_message_http_routes_keep_private_history_out_of_channel_reads() {
    let directory = TestDirectory::new();
    let state = directory.path().join("state");
    let ns = setup(&state, "direct-http");
    let key = SpaceKey::of(&ns);
    let herdr = directory.path().join("fake-herdr");
    let live_panes = directory.path().join("live-panes.json");
    executable::write_executable(
        &herdr,
        r#"#!/usr/bin/env python3
import pathlib, sys
root = pathlib.Path(__file__).parent
if sys.argv[1:3] == ["worktree", "list"]:
    print('{"error":{"code":"not_git_worktree"}}', file=sys.stderr)
    sys.exit(1)
if sys.argv[1:3] == ["pane", "list"]:
    if (root / "discovery-fails").exists():
        sys.exit(1)
    print((root / "live-panes.json").read_text())
elif sys.argv[1:3] == ["workspace", "list"]:
    print('{"result":{"workspaces":[]}}')
else:
    print('{}')
"#,
    );
    fs::write(
        &live_panes,
        serde_json::json!({"result":{"panes":[{
            "pane_id":"pane-b", "terminal_id":"terminal-b", "tab_id":"tab-b",
            "workspace_id":"direct-http", "agent":"codex", "agent_status":"idle",
            "agent_session":{"value":"session-b"}
        }]}})
        .to_string(),
    )
    .unwrap();
    let server = Server::start_configured(
        &state,
        Path::new(crate::binary::namedrop_path()),
        Some(&herdr),
    );
    let path = format!("/w/{}/direct-messages/address-b/messages", key.as_str());
    assert_eq!(server.get("/api/direct-messages", false).0, 401);
    assert_eq!(server.get(&path, false).0, 401);
    let (status, empty) = server.get(&path, true);
    assert_eq!(status, 200);
    assert_eq!(
        empty,
        serde_json::json!({"thread_id":null,"messages":[],"earlier_omitted":false})
    );
    let (_, session) = server.get("/api/session", true);
    let csrf = session["csrf_token"].as_str().unwrap();
    let post = |body: Value, token: Option<&str>| {
        let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let source = body.to_string();
        let mut request = format!(
            "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: namedrop_dashboard_{}={}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
            server.port,
            server.port,
            server.session,
            source.len()
        );
        if let Some(token) = token {
            request.push_str(&format!("X-Namedrop-CSRF: {token}\r\n"));
        }
        request.push_str("\r\n");
        request.push_str(&source);
        stream.write_all(request.as_bytes()).unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let (headers, body) = raw.split_once("\r\n\r\n").unwrap();
        (
            headers
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse::<u16>()
                .unwrap(),
            serde_json::from_str::<Value>(body).unwrap_or(Value::Null),
        )
    };
    let secret = format!("sk-proj-{}UNIQUE_SECRET_TAIL", "a".repeat(164));
    let operator_body = format!(
        "{} mentioning @a {secret} Private operator ending",
        "q".repeat(1980)
    );
    let body = serde_json::json!({"request_id":"dm-http","source":operator_body});
    assert_eq!(post(body.clone(), None).0, 403);
    assert_eq!(
        post(
            serde_json::json!({"request_id":"bad","source":"Question","to":["address-a"]}),
            Some(csrf)
        )
        .0,
        400
    );
    let (status, sent) = post(body.clone(), Some(csrf));
    assert_eq!(status, 200, "{sent}");
    assert_eq!(sent["recipients"].as_array().unwrap().len(), 1);
    assert_eq!(sent["recipients"][0]["address_id"], "address-b");
    let (_, history) = server.get(&path, true);
    assert_eq!(history["messages"].as_array().unwrap().len(), 1);
    assert_eq!(history["messages"][0]["body"]["truncated"], true);
    assert!(!history.to_string().contains("sk-proj-"));
    let message = sent["message_id"].as_str().unwrap();
    assert_eq!(
        server.get(&format!("/w/{}/messages", key.as_str()), true).1,
        serde_json::json!([])
    );
    assert_eq!(
        server
            .get(
                &format!("/w/{}/messages/{message}/context", key.as_str()),
                true
            )
            .0,
        404
    );
    let raw = server.raw_with_headers(
        &format!("/w/{}/messages/{message}/body", key.as_str()),
        true,
        &format!("X-Namedrop-CSRF: {csrf}\r\n"),
    );
    assert!(raw.starts_with("HTTP/1.1 404"), "{raw}");
    let private_body = format!("{path}/{message}/body");
    assert_eq!(server.get(&private_body, false).0, 401);
    assert_eq!(server.get(&private_body, true).0, 403);
    let headers = format!("X-Namedrop-CSRF: {csrf}\r\n");
    for provenance in [
        "Origin: http://foreign.invalid\r\n",
        "Sec-Fetch-Site: same-site\r\n",
        "Sec-Fetch-Site: cross-site\r\n",
    ] {
        assert!(
            server
                .raw_with_headers(&private_body, true, &format!("{headers}{provenance}"))
                .starts_with("HTTP/1.1 403")
        );
    }
    let read_body = |route: &str| {
        let raw = server.raw_with_headers(route, true, &headers);
        let (head, body) = raw.split_once("\r\n\r\n").unwrap();
        (
            head.split_whitespace()
                .nth(1)
                .unwrap()
                .parse::<u16>()
                .unwrap(),
            serde_json::from_str::<Value>(body).unwrap_or(Value::Null),
        )
    };
    let (status, full) = read_body(&private_body);
    assert_eq!(status, 200);
    assert_eq!(full["message_id"], message);
    assert_eq!(full["body"]["truncated"], false);
    assert!(
        full["body"]["text"]
            .as_str()
            .unwrap()
            .ends_with("Private operator ending")
    );
    assert!(!full.to_string().contains("sk-proj-"));
    assert!(!full.to_string().contains("UNIQUE_SECRET_TAIL"));
    let frame = server.snapshot(&key);
    assert!(frame["messages"].as_array().unwrap().is_empty());
    assert!(frame["threads"].as_array().unwrap().is_empty());
    let reply_body = format!(
        "{} {secret} {} Agent ending",
        "r".repeat(65_530),
        "s".repeat(5000)
    );
    StateStore::open(&state)
        .unwrap()
        .transaction(|tx| {
            tx.append_message(
                &ns,
                &MessageDraft {
                    thread_id: ThreadId::new(sent["thread_id"].as_str().unwrap()),
                    message_id: MessageId::new("private-agent-reply"),
                    in_reply_to: Some(MessageId::new(message)),
                    author_address_id: PaneAddressId::new("address-b"),
                    author_kind: ActorKind::AgentSession,
                    author_alias_snapshot: "b".into(),
                    author_agent_session_snapshot: None,
                    request_id: "private-agent-reply".into(),
                    authored_source: format!("@operator {reply_body}"),
                    body: reply_body,
                    grammar_version: "mention/v1".into(),
                    recipients: vec![MessageRecipientDraft {
                        address_id: PaneAddressId::new("operator:local"),
                        alias_snapshot: "operator".into(),
                        agent_session_snapshot: None,
                        delivery_id: DeliveryId::new("private-reply-delivery"),
                    }],
                    context_references: Vec::new(),
                },
                &at(u64::try_from(
                    std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis(),
                )
                .unwrap()),
            )
        })
        .unwrap();
    let agent_body = format!("{path}/private-agent-reply/body");
    let (status, full) = read_body(&agent_body);
    assert_eq!(status, 200);
    assert_eq!(full["body"]["truncated"], true);
    assert!(full["body"]["text"].as_str().unwrap().len() <= 65_536);
    assert!(!full.to_string().contains("sk-proj-"));
    assert!(!full.to_string().contains("UNIQUE_SECRET_TAIL"));
    assert!(
        !full["body"]["text"].as_str().unwrap().contains("sk-"),
        "even a partial secret prefix must be redacted before the cap"
    );
    assert!(full["body"]["text"].as_str().unwrap().ends_with("[REDA"));
    let other_ns = setup(&state, "another-workspace");
    append(&state, &ns, 0, 1, "Public body");
    for route in [
        format!(
            "/w/{}/direct-messages/address-a/messages/{message}/body",
            key.as_str()
        ),
        format!(
            "/w/{}/direct-messages/address-b/messages/{message}/body",
            SpaceKey::of(&other_ns).as_str()
        ),
        format!("{path}/message-0/body"),
        format!("{path}/missing/body"),
        format!("/w/{}/messages/private-agent-reply/body", key.as_str()),
    ] {
        assert_eq!(read_body(&route).0, 404, "{route}");
    }
    let (status, public_body) = read_body(&format!("/w/{}/messages/message-0/body", key.as_str()));
    assert_eq!(status, 200);
    assert_eq!(public_body["body"]["text"], "Public body");
    assert_eq!(
        server
            .get(
                &format!("/w/{}/messages/private-agent-reply/context", key.as_str()),
                true
            )
            .0,
        404
    );
    let (_, peers) = server.get("/api/direct-messages", true);
    let peer = peers["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|peer| peer["address_id"] == "address-b")
        .unwrap();
    assert_eq!(peer["last_message_id"], "private-agent-reply");
    assert_eq!(peer["space_key"], key.as_str());
    assert_eq!(peer["closed"], false);
    fs::write(&live_panes, r#"{"result":{"panes":[]}}"#).unwrap();
    let (_, peers) = server.get("/api/direct-messages", true);
    let peer = peers["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|peer| peer["address_id"] == "address-b")
        .unwrap();
    assert_eq!(peer["closed"], true);
    assert_eq!(peer["presence"], "gone");
    assert_eq!(
        post(
            serde_json::json!({"request_id":"after-close","source":"Do not store"}),
            Some(csrf)
        )
        .0,
        400
    );
    fs::write(directory.path().join("discovery-fails"), b"").unwrap();
    let (_, peers) = server.get("/api/direct-messages", true);
    let peer = peers["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|peer| peer["address_id"] == "address-b")
        .unwrap();
    assert_eq!(peer["closed"], true);
    assert_eq!(peer["presence"], "gone");
    assert_eq!(
        post(
            serde_json::json!({"request_id":"discovery-unavailable","source":"Do not store"}),
            Some(csrf)
        )
        .0,
        400
    );
    assert_eq!(
        read_body(&private_body).0,
        200,
        "closed operator history remains expandable"
    );
    assert_eq!(
        read_body(&agent_body).0,
        200,
        "closed agent history remains expandable"
    );
    let (status, replay) = post(body, Some(csrf));
    assert_eq!(status, 200);
    assert_eq!(replay["message_id"], sent["message_id"]);
    assert_eq!(replay["idempotent_replay"], true);
    let (_, history) = server.get(&path, true);
    assert_eq!(history["messages"].as_array().unwrap().len(), 2);
}
