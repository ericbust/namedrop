use super::*;
use namedrop::dashboard::snapshot::SpaceKey;

fn held_thread(f: &Fixture) -> Value {
    f.clock.0.store(
        namedrop::Clock::now_unix_ms(&namedrop::SystemClock),
        std::sync::atomic::Ordering::Relaxed,
    );
    let mut parent = plain_root(f, "controls-root", "Please collaborate.");
    for index in 0..10 {
        let (pane, mention) = if index % 2 == 0 {
            ("pane-1-planner", "lead")
        } else {
            ("pane-1-lead", "planner")
        };
        parent = agent_reply(
            f,
            &format!("controls-reply-{index}"),
            &parent,
            pane,
            &format!("@operator Ask @{mention} next."),
        )
        .unwrap();
    }
    parent
}

fn control_path(workspace: &str, thread: &Value, action: &str) -> String {
    let key = SpaceKey::of(&StateNamespace::for_space("socket-a", workspace));
    format!(
        "/w/{}/threads/{}/{action}",
        key.as_str(),
        thread["thread_id"].as_str().unwrap()
    )
}

fn controls(f: &Fixture, parent: &Value) -> Value {
    let mut store = StateStore::open(&f.state).unwrap();
    serde_json::to_value(
        store
            .transaction(|tx| {
                tx.thread_control_state(
                    &StateNamespace::for_space("socket-a", "w1"),
                    &namedrop::ThreadId::new(parent["thread_id"].as_str().unwrap()),
                )
            })
            .unwrap(),
    )
    .unwrap()
}

#[test]
fn browser_controls_continue_and_stop_release_the_reply_limit_mailbox_hold() {
    for action in ["continue", "stop"] {
        let f = fixture();
        let parent = held_thread(&f);
        assert_eq!(controls(&f, &parent)["reply_limit_paused"], true);
        let server = Server::start(&f, "not json", false);
        let (status, later) = post(&server, "later-thread", "@planner Later task.", None);
        assert_eq!(status, 200, "{later}");
        assert_eq!(f.count("SELECT count(*) FROM delivery_attempts a JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,address_id,delivery_id) WHERE d.message_id=(SELECT message_id FROM collaboration_messages WHERE request_id='later-thread')"), 0);
        let response = server.post(&control_path("w1", &parent, action), "{}", &[]);
        assert_eq!(response.status, 200, "{}", response.body);
        let before_replay = f.count("SELECT count(*) FROM transition_facts");
        let replay = server.post(&control_path("w1", &parent, action), "{}", &[]);
        assert_eq!(replay.status, 200, "{}", replay.body);
        assert_eq!(
            f.count("SELECT count(*) FROM transition_facts"),
            before_replay
        );
        if action == "continue" {
            assert_eq!(controls(&f, &parent)["reply_allowance"], 10);
            assert_eq!(controls(&f, &parent)["paused"], false);
            assert_eq!(f.count("SELECT count(*) FROM delivery_attempts a JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,address_id,delivery_id) WHERE d.message_id=(SELECT message_id FROM collaboration_messages WHERE request_id='controls-reply-9')"), 1);
        } else {
            assert_eq!(controls(&f, &parent)["stopped"], true);
            assert_eq!(f.count("SELECT count(*) FROM delivery_attempts a JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,address_id,delivery_id) WHERE d.message_id=(SELECT message_id FROM collaboration_messages WHERE request_id='later-thread')"), 1);
            let continued = server.post(&control_path("w1", &parent, "continue"), "{}", &[]);
            assert_eq!(continued.status, 200, "{}", continued.body);
            assert_eq!(controls(&f, &parent)["stopped"], true);
            assert_eq!(controls(&f, &parent)["reply_allowance"], 0);
        }
        assert_eq!(f.count("SELECT count(*) FROM attention_items WHERE source_kind='automatic_reply_limit' AND resolved_at_ms IS NULL"), 0);
    }
}

#[test]
fn browser_thread_controls_enforce_message_post_auth_and_workspace_scope() {
    let f = fixture();
    let parent = held_thread(&f);
    let server = Server::start(&f, "not json", false);
    for action in ["continue", "stop"] {
        let path = control_path("w1", &parent, action);
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
            assert_eq!(server.post(&path, "{}", &headers).status, 403);
        }
        assert_eq!(
            server
                .post(&control_path("w2", &parent, action), "{}", &[])
                .status,
            404
        );
        assert_eq!(
            server
                .post(
                    &path.replace(parent["thread_id"].as_str().unwrap(), "missing"),
                    "{}",
                    &[]
                )
                .status,
            404
        );
    }
    assert_eq!(controls(&f, &parent)["reply_limit_paused"], true);
    assert_eq!(controls(&f, &parent)["stopped"], false);
}

#[test]
fn browser_controls_attention_exposes_thread_and_actions() {
    let f = fixture();
    let parent = held_thread(&f);
    let server = Server::start(&f, "not json", false);
    let mut events = events(&server);
    let frame = next_frame(&mut events, &mut String::new());
    let item = frame["attention"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["source_kind"] == "automatic_reply_limit")
        .unwrap();
    assert_eq!(item["thread_id"], parent["thread_id"]);
    assert_eq!(item["actions"], json!(["continue", "stop"]));
}

#[test]
fn browser_controls_stop_wins_a_concurrent_continue() {
    let f = fixture();
    let parent = held_thread(&f);
    let server = Server::start(&f, "not json", false);
    thread::scope(|scope| {
        let continued =
            scope.spawn(|| server.post(&control_path("w1", &parent, "continue"), "{}", &[]));
        let stopped = scope.spawn(|| server.post(&control_path("w1", &parent, "stop"), "{}", &[]));
        for response in [continued.join().unwrap(), stopped.join().unwrap()] {
            assert_eq!(response.status, 200, "{}", response.body);
        }
    });
    assert_eq!(controls(&f, &parent)["stopped"], true);
    let retry = server.post(&control_path("w1", &parent, "continue"), "{}", &[]);
    assert_eq!(retry.status, 200, "{}", retry.body);
    assert_eq!(controls(&f, &parent)["stopped"], true);
}

#[test]
fn browser_controls_stop_interrupts_only_the_proven_current_thread_response_once() {
    for state in ["working", "idle", "new-session", "inactive-binding"] {
        let f = fixture();
        let parent = plain_root(&f, "interrupt-root", "Please inspect.");
        f.herdr.statuses.lock().unwrap().insert(
            "pane-1-planner".into(),
            if state == "idle" { "idle" } else { "working" }.into(),
        );
        if state == "new-session" {
            f.herdr
                .sessions
                .lock()
                .unwrap()
                .insert("pane-1-planner".into(), "replacement-session".into());
        }
        if state == "inactive-binding" {
            f.db()
                .execute(
                    "UPDATE pane_bindings SET active=0 WHERE pane_id='pane-1-planner'",
                    [],
                )
                .unwrap();
        }
        let server = Server::start(&f, "not json", false);
        for _ in 0..2 {
            let response = server.post(&control_path("w1", &parent, "stop"), "{}", &[]);
            assert_eq!(response.status, 200, "{}", response.body);
        }
        let interrupts: Vec<Value> =
            fs::read_to_string(f.root.join("http-herdr-invocations.jsonl"))
                .unwrap_or_default()
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).unwrap())
                .filter(|call| call["args"][1] == "send-keys")
                .collect();
        if state == "working" {
            assert_eq!(interrupts.len(), 1);
            assert_eq!(
                interrupts[0]["args"],
                json!(["agent", "send-keys", "pane-1-planner", "ctrl+c"])
            );
        } else {
            assert!(interrupts.is_empty(), "{state}: {interrupts:?}");
        }
    }
}

struct DiscoveryGate(std::path::PathBuf);

impl DiscoveryGate {
    fn new(f: &Fixture) -> Self {
        let gate = Self(f.root.join("discovery-gate"));
        fs::write(&gate.0, "").unwrap();
        gate
    }

    fn wait(&self) {
        let entered = self.0.with_file_name("discovery-entered");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !entered.exists() {
            assert!(Instant::now() < deadline, "stop did not enter discovery");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for DiscoveryGate {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[test]
fn browser_controls_stop_does_not_interrupt_a_later_thread_in_the_same_session() {
    let f = fixture();
    let parent = plain_root(&f, "original-thread", "Please inspect.");
    let attempt = f.text("SELECT attempt_id FROM delivery_attempts").unwrap();
    let session = xws::session("pane-1-planner");
    f.status("pane-1-planner", "working");
    let server = Server::start(&f, "not json", true);
    let gate = DiscoveryGate::new(&f);
    thread::scope(|scope| {
        let stop = scope.spawn(|| server.post(&control_path("w1", &parent, "stop"), "{}", &[]));
        gate.wait();
        assert_eq!(controls(&f, &parent)["stopped"], true);
        StateStore::open(&f.state)
            .unwrap()
            .transaction(|tx| {
                let namespace = StateNamespace::for_space("socket-a", "w1");
                let attribution = namedrop::MutationAttribution::new(
                    namedrop::ActorKind::AgentSession,
                    &session,
                    43,
                );
                let attempt = namedrop::DeliveryAttemptId::new(&attempt);
                tx.acknowledge_delivery_attempt(
                    &namespace,
                    &attempt,
                    "original-ack",
                    &namedrop::AgentSessionId::new(&session),
                    &attribution,
                )?;
                tx.transition_delivery_attempt(
                    &namespace,
                    &attempt,
                    namedrop::DeliveryState::Settled,
                    &attribution,
                )
            })
            .unwrap();
        f.status("pane-1-planner", "idle");
        f.clock.0.store(
            namedrop::Clock::now_unix_ms(&namedrop::SystemClock),
            std::sync::atomic::Ordering::Relaxed,
        );
        let unrelated = namedrop::send_operator_message(
            "unrelated-thread",
            "@planner Work on a different task.",
            None,
            &f.runtime("w1", "pane-1-lead"),
        )
        .unwrap();
        assert_ne!(unrelated["thread_id"], parent["thread_id"]);
        assert_eq!(
            f.count(
                "SELECT COUNT(*) FROM delivery_attempt_state WHERE active=1 AND state IN ('submitted','acknowledged')"
            ),
            1,
            "{unrelated}"
        );
        assert_eq!(f.text("SELECT a.agent_session_id FROM delivery_attempts a JOIN delivery_attempt_state s USING(session_socket_id,worktree_id,attempt_id,address_id) WHERE s.active=1"), Some(session));
        f.status("pane-1-planner", "working");
        drop(gate);
        let response = stop.join().unwrap();
        assert_eq!(response.status, 200, "{}", response.body);
        let receipt: Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(receipt["interrupted_pane_ids"], json!([]), "{receipt}");
        assert_eq!(receipt["warnings"].as_array().unwrap().len(), 1);
    });
    let calls = fs::read_to_string(f.root.join("http-herdr-invocations.jsonl")).unwrap_or_default();
    assert!(
        !calls
            .lines()
            .any(|line| { serde_json::from_str::<Value>(line).unwrap()["args"][1] == "send-keys" }),
        "interrupted unrelated work: {calls}"
    );
    assert_eq!(
        f.count("SELECT COUNT(*) FROM delivery_attempt_state WHERE active=1 AND state IN ('submitted','acknowledged')"),
        1
    );
}

#[test]
fn browser_controls_stop_rechecks_the_binding_after_discovery() {
    let f = fixture();
    let parent = plain_root(&f, "retired-binding", "Please inspect.");
    f.status("pane-1-planner", "working");
    let server = Server::start(&f, "not json", true);
    let gate = DiscoveryGate::new(&f);
    thread::scope(|scope| {
        let stop = scope.spawn(|| server.post(&control_path("w1", &parent, "stop"), "{}", &[]));
        gate.wait();
        assert_eq!(controls(&f, &parent)["stopped"], true);
        f.db()
            .execute(
                "UPDATE pane_bindings SET active=0 WHERE pane_id='pane-1-planner'",
                [],
            )
            .unwrap();
        drop(gate);
        let response = stop.join().unwrap();
        assert_eq!(response.status, 200, "{}", response.body);
        let receipt: Value = serde_json::from_str(&response.body).unwrap();
        assert_eq!(receipt["interrupted_pane_ids"], json!([]), "{receipt}");
        assert_eq!(receipt["warnings"].as_array().unwrap().len(), 1);
    });
}

#[test]
fn browser_thread_controls_refuse_mutations_during_handoff() {
    use std::{io::Read, os::fd::AsRawFd, sync::mpsc};
    struct PromptGate(std::path::PathBuf);
    impl Drop for PromptGate {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    for action in ["stop", "continue"] {
        let f = fixture();
        let server = Server::start(&f, "not json", true);
        let gate = PromptGate(f.root.join("prompt-gate"));
        fs::write(&gate.0, "").unwrap();
        let (_, parent) = support::raw_post(&server, "handoff-control", "@planner Inspect.", None);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !f.root.join("prompt-entered").exists() {
            assert!(Instant::now() < deadline, "mutation did not enter drain");
            thread::yield_now();
        }
        fs::create_dir_all(f.state.join("runtime")).unwrap();
        let packages = fs::File::create(f.state.join("runtime/packages.lock")).unwrap();
        fs::set_permissions(
            f.state.join("runtime/packages.lock"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert_eq!(
            unsafe { libc::flock(packages.as_raw_fd(), libc::LOCK_EX) },
            0
        );
        let mut stream = events(&server);
        let mut buffered = String::new();
        next_frame(&mut stream, &mut buffered);
        fs::write(f.state.join("runtime/migration.json"), "{}").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !buffered.contains("event: restarting") {
            assert!(Instant::now() < deadline, "server did not start handoff");
            let mut bytes = [0u8; 8192];
            let count = stream.read(&mut bytes).unwrap();
            assert!(count > 0);
            buffered.push_str(&String::from_utf8_lossy(&bytes[..count]));
        }
        let (sender, receiver) = mpsc::channel();
        thread::scope(|scope| {
            scope.spawn(|| {
                sender
                    .send(server.post(&control_path("w1", &parent, action), "{}", &[]))
                    .unwrap()
            });
            let early = receiver.recv_timeout(Duration::from_millis(200));
            drop(gate);
            let response =
                early.unwrap_or_else(|_| receiver.recv_timeout(Duration::from_secs(5)).unwrap());
            assert_eq!(response.status, 503, "{action}: {}", response.body);
            assert_eq!(controls(&f, &parent)["stopped"], false);
        });
        drop(packages);
    }
}
