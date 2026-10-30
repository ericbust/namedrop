use super::*;
use namedrop::dashboard::snapshot::SpaceKey;

fn path(workspace: &str, message: &Value) -> String {
    let key = SpaceKey::of(&StateNamespace::for_space("socket-a", workspace));
    format!(
        "/w/{}/messages/{}/deliver",
        key.as_str(),
        message["message_id"].as_str().unwrap()
    )
}

fn deliver(server: &Server, message: &Value, aliases: &[&str]) -> (u16, Value) {
    let response = server.post(
        &path("w1", message),
        &json!({"to":aliases}).to_string(),
        &[],
    );
    (
        response.status,
        serde_json::from_str(&response.body).unwrap_or_else(|_| json!({"error":response.body})),
    )
}

#[test]
fn stored_delivery_updates_original_post_without_copying_or_rerouting() {
    let f = fixture();
    let server = Server::start(&f, r#"{"to":[],"reason":"credit only"}"#, false);
    let source = "Please ask @planner to inspect verbatim.";
    let (_, root) = post(&server, "manual-root", source, None);
    let mut stream = events(&server);
    let mut buffered = String::new();
    let initial = next_frame(&mut stream, &mut buffered);
    assert_eq!(initial["messages"].as_array().unwrap().len(), 1);
    let notes = fs::read(f.state.join("dashboard/channel-routing.jsonl")).unwrap();
    let before: (String,String,String,String) = f.db().query_row("SELECT m.message_id,m.authored_source,m.body,i.request_fingerprint FROM collaboration_messages m JOIN idempotency_records i USING(session_socket_id,worktree_id,message_id) WHERE m.request_id='manual-root'", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    let (status, receipt) = deliver(&server, &root, &["planner"]);
    assert_eq!(status, 200, "{receipt}");
    assert_eq!(receipt["message_id"], root["message_id"]);
    assert_eq!(receipt["channel"]["state"], "unrouted");
    assert_eq!(receipt["channel"]["to"], json!(["planner"]));
    assert_eq!(receipt["channel"]["reason"], "credit only");
    let changed = next_frame(&mut stream, &mut buffered);
    assert_eq!(changed["messages"].as_array().unwrap().len(), 1);
    assert_eq!(changed["messages"][0]["message_id"], root["message_id"]);
    assert_eq!(changed["messages"][0]["channel"], receipt["channel"]);
    assert_eq!(changed["messages"][0]["legs"].as_array().unwrap().len(), 1);
    let (_, replay) = deliver(&server, &root, &["planner", "planner"]);
    assert_eq!(replay, receipt);
    assert_eq!(f.count("SELECT count(*) FROM collaboration_messages"), 1);
    assert_eq!(f.count("SELECT count(*) FROM message_recipients"), 1);
    assert_eq!(f.count("SELECT count(*) FROM mailbox_deliveries"), 1);
    let after: (String,String,String,String) = f.db().query_row("SELECT m.message_id,m.authored_source,m.body,i.request_fingerprint FROM collaboration_messages m JOIN idempotency_records i USING(session_socket_id,worktree_id,message_id) WHERE m.request_id='manual-root'", [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    assert_eq!(before, after);
    assert_eq!(
        fs::read(f.state.join("dashboard/channel-routing.jsonl")).unwrap(),
        notes
    );
    assert_eq!(calls(&f).len(), 1);
}

fn wait_for(f: &Fixture, query: &str, expected: i64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while f.count(query) != expected {
        assert!(Instant::now() < deadline, "{query}");
        thread::yield_now();
    }
}

fn recipient_rows(f: &Fixture, message: &Value) -> Vec<(i64, String, String)> {
    f.db().prepare("SELECT recipient_order,address_id,alias_snapshot FROM message_recipients WHERE worktree_id='w1' AND message_id=?1 ORDER BY recipient_order")
        .unwrap().query_map([message["message_id"].as_str().unwrap()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)))
        .unwrap().collect::<Result<_,_>>().unwrap()
}

#[test]
fn delivery_accepts_all_root_outcomes_and_preserves_recipient_order() {
    for (response, expected) in [
        (r#"{"to":["planner"],"reason":"request"}"#, "routed"),
        (r#"{"to":[],"reason":"credit"}"#, "unrouted"),
        ("not json", "router_failed"),
    ] {
        let f = fixture();
        let server = Server::start(&f, response, false);
        let (_, root) = post(&server, "outcome-root", "Could @planner inspect?", None);
        assert_eq!(root["channel"]["state"], expected);
        let (status, receipt) = deliver(&server, &root, &["planner", "lead"]);
        assert_eq!(status, 200, "{receipt}");
        assert_eq!(receipt["channel"]["state"], expected);
        assert_eq!(receipt["channel"]["to"], json!(["planner", "lead"]));
        assert_eq!(
            recipient_rows(&f, &root)
                .iter()
                .map(|r| (r.0, r.2.as_str()))
                .collect::<Vec<_>>(),
            [(1, "planner"), (2, "lead")]
        );
        assert_eq!(f.count("SELECT count(*) FROM collaboration_messages"), 1);
        assert_eq!(f.count("SELECT reply_allowance FROM thread_controls"), 10);
    }
}

#[test]
fn delivery_accepts_legacy_channel_roots_and_router_marked_followups_under_either_root() {
    for channel_root in [false, true] {
        let f = fixture();
        let server = Server::start(&f, r#"{"to":["planner"],"reason":"request"}"#, false);
        let (_, root) = post(
            &server,
            "s4-root",
            if channel_root {
                "Could @planner inspect?"
            } else {
                "@planner Inspect."
            },
            None,
        );
        fs::write(
            f.root.join("router-response.json"),
            r#"{"to":[],"reason":"passing mention"}"#,
        )
        .unwrap();
        let (_, followup) = post(
            &server,
            "s4-followup",
            "Please ask @lead to review.",
            root["message_id"].as_str(),
        );
        assert_eq!(followup["channel"]["state"], "unrouted");
        let original: (String, String, String, String, String) = f.db().query_row(
            "SELECT authored_source,body,author_address_id,author_kind,request_fingerprint FROM collaboration_messages JOIN idempotency_records USING(session_socket_id,worktree_id,message_id,author_address_id,request_id) WHERE message_id=?1",
            [followup["message_id"].as_str().unwrap()],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
        ).unwrap();
        let (status, receipt) = deliver(&server, &followup, &["lead"]);
        assert_eq!(status, 200, "{receipt}");
        assert_eq!(receipt["message_id"], followup["message_id"]);
        assert_eq!(f.count("SELECT count(*) FROM collaboration_messages"), 2);
        assert_eq!(
            recipient_rows(&f, &followup)
                .iter()
                .map(|row| (row.0, row.2.as_str()))
                .collect::<Vec<_>>(),
            [(1, "planner"), (2, "lead")]
        );
        let stored: (String, String, String, String, String) = f.db().query_row(
            "SELECT authored_source,body,author_address_id,author_kind,request_fingerprint FROM collaboration_messages JOIN idempotency_records USING(session_socket_id,worktree_id,message_id,author_address_id,request_id) WHERE message_id=?1",
            [followup["message_id"].as_str().unwrap()],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
        ).unwrap();
        assert_eq!(stored, original);
        assert_eq!(stored.0, "Please ask @lead to review.");
        assert_eq!(stored.1, stored.0);
        assert_eq!(stored.2, "operator:local");
        assert_eq!(stored.3, "human");
        assert_eq!(receipt["channel"]["to"], json!(["planner", "lead"]));
        assert_eq!(receipt["channel"]["state"], "unrouted");
    }
    let f = fixture();
    let server = Server::start(&f, "not json", true);
    let _ = post(&server, "legacy-root", "Room statement.", None);
    let root = stored_root(
        &f,
        "legacy-stored",
        "operator:local",
        namedrop::ActorKind::Human,
    );
    assert_eq!(
        deliver(&server, &root, &["planner"]).1["channel"]["state"],
        "routed"
    );
}

#[test]
fn delivery_refuses_unmarked_or_nonoperator_posts_and_wrong_space() {
    let f = fixture();
    let server = Server::start(&f, r#"{"to":["planner"]}"#, false);
    let (_, explicit) = post(&server, "explicit-root", "@planner Inspect.", None);
    let (_, followup) = post(
        &server,
        "unmarked-followup",
        "Continue please.",
        explicit["message_id"].as_str(),
    );
    for message in [&explicit, &followup] {
        assert_eq!(deliver(&server, message, &["lead"]).0, 404);
    }
    let (_, root) = post(&server, "eligible-root", "Room statement.", None);
    assert_eq!(
        server
            .post(&path("w2", &root), r#"{"to":["lead"]}"#, &[])
            .status,
        404
    );
    assert_eq!(
        server
            .post(
                &path("w1", &json!({"message_id":"missing"})),
                r#"{"to":["lead"]}"#,
                &[]
            )
            .status,
        404
    );
    for (id, author, kind) in [
        (
            "agent-author",
            "address-w1-planner",
            namedrop::ActorKind::AgentSession,
        ),
        (
            "human-other-address",
            "address-w1-planner",
            namedrop::ActorKind::Human,
        ),
        (
            "operator-agent-kind",
            "operator:local",
            namedrop::ActorKind::AgentSession,
        ),
    ] {
        let root = stored_root(&f, id, author, kind);
        assert_eq!(deliver(&server, &root, &["lead"]).0, 404);
    }
}

#[test]
fn delivery_validates_the_entire_current_roster_before_any_write() {
    for invalid in ["ghost", "shell", "operator", "disabled", "retired"] {
        let f = fixture();
        f.add_terminal("w1", "shell", "pane-shell");
        if invalid == "disabled" {
            f.rename("w1", "lead", "disabled");
            f.db().execute("UPDATE pane_addresses SET collaboration_disabled=1 WHERE address_id='address-w1-lead'", []).unwrap();
        } else if invalid == "retired" {
            f.rename("w1", "lead", "retired");
            cancel_and_remove(&f, "w1", "lead");
        } else if invalid == "operator" {
            f.rename("w1", "lead", "operator");
        }
        let server = Server::start(&f, "not json", true);
        let (_, root) = post(&server, "invalid-member-root", "Room statement.", None);
        let before = f.count("SELECT count(*) FROM transition_facts");
        let (status, refused) = deliver(&server, &root, &["planner", invalid]);
        assert_eq!(status, 400, "{invalid}: {refused}");
        assert_eq!(f.count("SELECT count(*) FROM transition_facts"), before);
        assert!(recipient_rows(&f, &root).is_empty());
        assert_eq!(f.count("SELECT count(*) FROM mailbox_deliveries"), 0);
    }
}

#[test]
fn delivery_reports_current_aliases_and_deduplicates_only_addresses() {
    let f = fixture();
    let server = Server::start(&f, "not json", true);
    let (_, root) = post(&server, "identity-root", "Room statement.", None);
    let (_, first) = deliver(&server, &root, &["planner"]);
    assert_eq!(first["delivered"], json!(["planner"]));
    f.rename("w1", "planner", "renamed");
    let (_, renamed) = deliver(&server, &root, &["renamed", "renamed"]);
    assert_eq!(renamed["delivered"], json!(["renamed"]));
    assert_eq!(renamed["channel"]["to"], json!(["planner"]));
    f.release("w1", "planner");
    f.rename("w1", "lead", "planner");
    let (_, reused) = deliver(&server, &root, &["planner"]);
    assert_eq!(reused["delivered"], json!(["planner"]));
    wait_for(
        &f,
        "SELECT count(*) FROM delivery_attempts a JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,address_id,delivery_id) WHERE d.address_id='address-w1-lead'",
        1,
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let invoked =
            fs::read_to_string(f.root.join("http-herdr-invocations.jsonl")).unwrap_or_default();
        if invoked.lines().filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|call| call["args"][1] == "prompt" && call["args"][2] == "pane-1-lead"
                && serde_json::from_str::<Value>(call["args"][3].as_str().unwrap()).unwrap()["control"]["message_id"] == root["message_id"]) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "reused alias did not receive the original envelope: {invoked}"
        );
        thread::yield_now();
    }
    assert_eq!(f.count("SELECT count(*) FROM mailbox_deliveries"), 2);
    assert_eq!(
        recipient_rows(&f, &root),
        [
            (1, "address-w1-planner".into(), "planner".into()),
            (2, "address-w1-lead".into(), "planner".into()),
        ]
    );
}

#[test]
fn concurrent_delivery_requests_add_each_member_once() {
    let f = fixture();
    let server = Server::start(&f, "not json", true);
    let (_, root) = post(&server, "concurrent-root", "Room statement.", None);
    thread::scope(|scope| {
        let first = scope.spawn(|| deliver(&server, &root, &["planner", "lead"]));
        let second = scope.spawn(|| deliver(&server, &root, &["lead", "planner"]));
        let (status, receipt) = first.join().unwrap();
        assert_eq!(status, 200);
        assert_eq!(receipt["delivered"], json!(["planner", "lead"]));
        let (status, receipt) = second.join().unwrap();
        assert_eq!(status, 200);
        assert_eq!(receipt["delivered"], json!(["lead", "planner"]));
    });
    assert_eq!(recipient_rows(&f, &root).len(), 2);
    assert_eq!(f.count("SELECT count(*) FROM mailbox_deliveries"), 2);
    assert_eq!(f.count("SELECT count(*) FROM collaboration_messages"), 1);
}

#[test]
fn delivery_refuses_pending_routing_then_accepts_retry_after_completion() {
    let f = fixture();
    let server = Server::start(&f, "delayed", false);
    let (_, root) = support::raw_post(&server, "pending-root", "Could @planner inspect?", None);
    assert_eq!(root["channel"]["state"], "routing");
    assert_eq!(
        server.post(&path("w1", &root), r#"{"to":[]}"#, &[]).status,
        400
    );
    let (status, receipt) = deliver(&server, &root, &["lead"]);
    assert_eq!(status, 409, "{receipt}");
    assert!(recipient_rows(&f, &root).is_empty());
    fs::write(f.root.join("router-release"), "continue").unwrap();
    let (_, root) = post(&server, "pending-root", "Could @planner inspect?", None);
    assert_eq!(deliver(&server, &root, &["lead"]).0, 200);
}

#[test]
fn stopped_delivery_allows_only_existing_recipients_and_pause_queues_until_continue() {
    for stopped in [false, true] {
        let f = fixture();
        let server = Server::start(&f, "not json", true);
        let (_, root) = post(&server, "controlled-root", "Room statement.", None);
        assert_eq!(deliver(&server, &root, &["planner"]).0, 200);
        f.db()
            .execute(
                "UPDATE thread_controls SET paused=1,reply_allowance=0,stopped=?1",
                [stopped],
            )
            .unwrap();
        assert_eq!(deliver(&server, &root, &["planner"]).0, 200);
        let (status, receipt) = deliver(&server, &root, &["lead"]);
        assert_eq!(status, if stopped { 409 } else { 200 }, "{receipt}");
        if !stopped {
            assert_eq!(f.count("SELECT count(*) FROM delivery_attempts a JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,address_id,delivery_id) WHERE d.address_id='address-w1-lead'"), 0);
            let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
            let response = server.post(
                &format!(
                    "/w/{}/threads/{}/continue",
                    key.as_str(),
                    root["thread_id"].as_str().unwrap()
                ),
                "{}",
                &[],
            );
            assert_eq!(response.status, 200, "{}", response.body);
            wait_for(
                &f,
                "SELECT count(*) FROM delivery_attempts a JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,address_id,delivery_id) WHERE d.address_id='address-w1-lead'",
                1,
            );
        } else {
            assert_eq!(recipient_rows(&f, &root).len(), 1);
        }
    }
}

#[test]
fn delivery_preserves_message_post_auth_and_rejects_non_strict_bodies() {
    let f = fixture();
    let server = Server::start(&f, "not json", true);
    let (_, root) = post(&server, "auth-root", "Room statement.", None);
    let path = path("w1", &root);
    assert_eq!(server.request("POST", &path, &[], "{}").status, 401);
    assert_eq!(
        server
            .request("POST", &path, &[("Cookie", &server.cookie)], "{}")
            .status,
        403
    );
    for headers in [
        vec![("Host", "evil.example")],
        vec![("Origin", "https://evil.example")],
        vec![("Sec-Fetch-Site", "cross-site")],
    ] {
        assert_eq!(
            server.post(&path, r#"{"to":["planner"]}"#, &headers).status,
            403
        );
    }
    for body in [
        r#"{}"#,
        r#"{"to":[]}"#,
        r#"{"to":"planner"}"#,
        r#"{"to":["planner"],"source":"copied"}"#,
        r#"{"to":[null]}"#,
    ] {
        assert_eq!(server.post(&path, body, &[]).status, 400, "{body}");
    }
    assert!(recipient_rows(&f, &root).is_empty());
}

fn stored_root(f: &Fixture, id: &str, author: &str, kind: namedrop::ActorKind) -> Value {
    StateStore::open(&f.state)
        .unwrap()
        .transaction(|tx| {
            let namespace = StateNamespace::for_space("socket-a", "w1");
            let actor = namedrop::MutationAttribution::new(
                kind,
                "test",
                namedrop::Clock::now_unix_ms(&namedrop::SystemClock),
            );
            let thread = namedrop::ThreadId::new(format!("thread-{id}"));
            tx.create_thread(&namespace, &thread, "Stored root", &actor)?;
            let receipt = tx.append_message(
                &namespace,
                &namedrop::MessageDraft {
                    message_id: namedrop::MessageId::new(id),
                    thread_id: thread,
                    in_reply_to: None,
                    author_address_id: namedrop::PaneAddressId::new(author),
                    author_kind: kind,
                    author_alias_snapshot: "original-author".into(),
                    author_agent_session_snapshot: None,
                    request_id: id.into(),
                    authored_source: "Stored original source.".into(),
                    body: "Stored original body.".into(),
                    grammar_version: "channel/v1".into(),
                    recipients: if author == "operator:local" && kind == namedrop::ActorKind::Human
                    {
                        vec![]
                    } else {
                        vec![namedrop::MessageRecipientDraft {
                            delivery_id: namedrop::DeliveryId::new(format!("delivery-{id}")),
                            address_id: namedrop::PaneAddressId::new("address-w1-lead"),
                            alias_snapshot: "lead".into(),
                            agent_session_snapshot: None,
                        }]
                    },
                    context_references: vec![],
                },
                &actor,
            )?;
            Ok(serde_json::to_value(receipt).unwrap())
        })
        .unwrap()
}

struct PromptGate(std::path::PathBuf);
impl PromptGate {
    fn new(f: &Fixture) -> Self {
        let path = f.root.join("prompt-gate");
        fs::write(&path, "").unwrap();
        Self(path)
    }

    fn wait(&self, f: &Fixture) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f.root.join("prompt-entered").exists() {
            assert!(
                Instant::now() < deadline,
                "delivery did not reach fake prompt"
            );
            thread::yield_now();
        }
    }
}

impl Drop for PromptGate {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

fn prompted_envelope(f: &Fixture) -> Value {
    fs::read_to_string(f.root.join("http-herdr-invocations.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).unwrap())
        .find_map(|call| {
            (call["args"][0] == "agent" && call["args"][1] == "prompt")
                .then(|| serde_json::from_str(call["args"][3].as_str().unwrap()).unwrap())
        })
        .unwrap()
}

#[test]
fn delivery_answers_with_eof_before_drain_and_keeps_original_operator_envelope() {
    use std::sync::mpsc;
    let f = fixture();
    let server = Server::start(&f, "not json", true);
    let source = "A verbatim request for a stored post.";
    let (_, root) = post(&server, "eof-root", source, None);
    let gate = PromptGate::new(&f);
    let (sender, receiver) = mpsc::channel();
    thread::scope(|scope| {
        scope.spawn(|| sender.send(deliver(&server, &root, &["planner"])).unwrap());
        let early = receiver.recv_timeout(Duration::from_secs(3));
        if early.is_err() {
            drop(gate);
            panic!("delivery response waited for drain");
        }
        let (status, receipt) = early.unwrap();
        assert_eq!(status, 200, "{receipt}");
        gate.wait(&f);
        let envelope = prompted_envelope(&f);
        assert_eq!(envelope["control"]["message_id"], root["message_id"]);
        assert_eq!(envelope["control"]["thread_id"], root["thread_id"]);
        assert_eq!(envelope["control"]["sender"]["alias"], "operator");
        assert_eq!(
            envelope["control"]["sender"]["address_id"],
            "operator:local"
        );
        assert_eq!(envelope["control"]["reply_mode"], "channel");
        assert_eq!(envelope["untrusted_body"], source);
        assert!(
            envelope["trusted_instruction"]
                .as_str()
                .unwrap()
                .contains("operator")
        );
        assert!(gate.0.exists());
        let (status, later) =
            support::raw_post(&server, "during-drain", "Another room statement.", None);
        assert_eq!(status, 200, "{later}");
        drop(gate);
    });
    wait_for(
        &f,
        "SELECT COUNT(*) FROM delivery_attempt_state WHERE state IN ('submitted','acknowledged')",
        1,
    );
}

#[test]
fn committed_delivery_drains_after_the_client_disconnects() {
    use std::io::Write;
    use std::net::TcpStream;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    let f = fixture();
    let (_, root) = {
        let server = Server::start(&f, "not json", true);
        post(
            &server,
            "disconnect-root",
            "Stored disconnected request.",
            None,
        )
    };
    let server = Server::start(&f, "not json", true);
    let gate = PromptGate::new(&f);
    let routing_lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC)
        .open(f.state.join("dashboard/channel-routing.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(routing_lock.as_raw_fd(), libc::LOCK_EX) },
        0
    );
    let info: Value =
        serde_json::from_slice(&fs::read(f.state.join("dashboard/server.json")).unwrap()).unwrap();
    let port = info["port"].as_u64().unwrap() as u16;
    let csrf: Value = serde_json::from_str(
        &server
            .request("GET", "/api/session", &[("Cookie", &server.cookie)], "")
            .body,
    )
    .unwrap();
    let body = r#"{"to":["planner"]}"#;
    let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    stream.write_all(format!("POST {} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nCookie: {}\r\nX-Namedrop-Csrf: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",path("w1",&root),server.cookie,csrf["csrf_token"].as_str().unwrap(),body.len()).as_bytes()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while recipient_rows(&f, &root).len() != 1 {
        assert!(
            Instant::now() < deadline,
            "delivery did not commit recipient"
        );
        thread::yield_now();
    }
    assert!(!f.root.join("prompt-entered").exists());
    eprintln!(
        "disconnect evidence: one recipient committed while routing lock held; fake prompt not entered before reset"
    );
    let linger = libc::linger {
        l_onoff: 1,
        l_linger: 0,
    };
    assert_eq!(
        unsafe {
            libc::setsockopt(
                stream.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_LINGER,
                &linger as *const _ as *const libc::c_void,
                std::mem::size_of_val(&linger) as libc::socklen_t,
            )
        },
        0
    );
    drop(stream);
    drop(routing_lock);
    gate.wait(&f);
    assert_eq!(recipient_rows(&f, &root).len(), 1);
    drop(gate);
    wait_for(
        &f,
        "SELECT COUNT(*) FROM delivery_attempt_state WHERE state IN ('submitted','acknowledged')",
        1,
    );
    eprintln!(
        "disconnect evidence: fake prompt entered after reset and lock release; one submitted or acknowledged attempt"
    );
}

#[test]
fn delivery_joins_only_the_added_recipient() {
    let f = fixture();
    let server = Server::start(&f, "not json", true);
    let (_, root) = post(&server, "participation-root", "Room statement.", None);
    f.db()
        .execute(
            "DELETE FROM thread_participants WHERE address_id='operator:local'",
            [],
        )
        .unwrap();
    assert_eq!(deliver(&server, &root, &["planner"]).0, 200);
    assert_eq!(
        f.count("SELECT COUNT(*) FROM thread_participants WHERE address_id='operator:local'"),
        0
    );
    assert_eq!(
        f.count("SELECT COUNT(*) FROM thread_participants WHERE address_id='address-w1-planner'"),
        1
    );
    assert_eq!(f.count("SELECT COUNT(*) FROM thread_context_snapshots"), 0);
}

#[test]
fn delivery_rejects_envoys_even_when_their_alias_is_current() {
    let f = fixture();
    f.agent_send(
        "w2",
        "pane-2-lead",
        "envoy-origin",
        "#ws1 @planner Inspect.",
    );
    f.db().execute("INSERT INTO alias_claims (session_socket_id,worktree_id,alias,address_id,claim_state) SELECT session_socket_id,worktree_id,'remote-lead',address_id,'active' FROM space_envoys WHERE worktree_id='w1' LIMIT 1", []).unwrap();
    let server = Server::start(&f, "not json", true);
    let (_, root) = post(&server, "envoy-refusal", "Room statement.", None);
    assert_eq!(deliver(&server, &root, &["planner", "remote-lead"]).0, 400);
    assert!(recipient_rows(&f, &root).is_empty());
}
