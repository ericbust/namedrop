use super::*;

fn until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready() {
        assert!(
            Instant::now() < deadline,
            "durable routing condition timed out"
        );
        thread::sleep(Duration::from_millis(10));
    }
}

fn stop_routing_thread(server: &Server, receipt: &Value) {
    let key =
        namedrop::dashboard::snapshot::SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
    let response = server.post(
        &format!(
            "/w/{}/threads/{}/stop",
            key.as_str(),
            receipt["thread_id"].as_str().unwrap()
        ),
        "{}",
        &[],
    );
    assert_eq!(response.status, 200, "{}", response.body);
}

#[test]
fn stop_during_routing_prevents_selected_and_baseline_deliveries_and_releases_mailbox() {
    for with_baseline in [false, true] {
        let f = fixture();
        let server = Server::start(&f, "delayed", false);
        let parent = with_baseline.then(|| {
            let (status, receipt) = post(&server, "baseline-root", "@lead Begin here.", None);
            assert_eq!(status, 200, "{receipt}");
            receipt
        });
        let parent_id = parent.as_ref().and_then(|p| p["message_id"].as_str());
        let source = "Could @planner review this?";
        let (status, pending) = support::raw_post(&server, "stopped-route", source, parent_id);
        assert_eq!(status, 200, "{pending}");
        until(|| calls(&f).len() == 1);
        stop_routing_thread(&server, &pending);
        fs::write(f.root.join("router-release"), "").unwrap();
        let (status, replay) = post(&server, "stopped-route", source, parent_id);
        assert_eq!(status, 200, "{replay}");
        assert_eq!(replay["message_id"], pending["message_id"]);
        assert_eq!(replay["idempotent_replay"], true);
        assert_eq!(replay["channel"]["state"], "router_failed");
        assert_eq!(replay["recipients"], json!([]));
        assert_eq!(replay["deliveries"], json!([]));
        let id = pending["message_id"].as_str().unwrap();
        for table in ["message_recipients", "mailbox_deliveries"] {
            assert_eq!(
                f.db()
                    .query_row(
                        &format!("SELECT COUNT(*) FROM {table} WHERE message_id=?1"),
                        [id],
                        |row| row.get::<_, i64>(0)
                    )
                    .unwrap(),
                0
            );
        }
        let (status, later) = post(&server, "later-task", "@planner New task.", None);
        assert_eq!(status, 200, "{later}");
        assert_ne!(later["thread_id"], pending["thread_id"]);
        until(|| {
            f.count("SELECT COUNT(*) FROM delivery_attempts a JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,address_id,delivery_id) WHERE d.message_id=(SELECT message_id FROM collaboration_messages WHERE request_id='later-task')") == 1
        });
        assert_eq!(calls(&f).len(), 1);
        eprintln!(
            "stop evidence: baseline={with_baseline}, stopped route failed with zero recipients and deliveries; later planner task reached a delivery attempt"
        );
    }
}

#[test]
fn stopped_queued_route_fails_without_starting_another_model() {
    let f = fixture();
    let server = Server::start(&f, "delayed", false);
    let source = "Could @planner review this?";
    for index in 0..4 {
        let (status, receipt) = support::raw_post(&server, &format!("busy-{index}"), source, None);
        assert_eq!(status, 200, "{receipt}");
    }
    until(|| calls(&f).len() == 4);
    let (status, pending) = support::raw_post(&server, "stopped-queued", source, None);
    assert_eq!(status, 200, "{pending}");
    assert_eq!(pending["channel"]["state"], "routing");
    stop_routing_thread(&server, &pending);
    fs::write(f.root.join("router-release"), "").unwrap();
    let (status, replay) = post(&server, "stopped-queued", source, None);
    assert_eq!(status, 200, "{replay}");
    assert_eq!(replay["message_id"], pending["message_id"]);
    assert_eq!(replay["channel"]["state"], "router_failed");
    assert_eq!(replay["recipients"], json!([]));
    assert_eq!(replay["deliveries"], json!([]));
    for index in 0..4 {
        assert_eq!(
            post(&server, &format!("busy-{index}"), source, None).1["channel"]["state"],
            "routed"
        );
    }
    assert_eq!(calls(&f).len(), 4);
    eprintln!(
        "queue stop evidence: four models ran; stopped fifth post failed with zero recipients and deliveries and no fifth model call"
    );
}

#[test]
fn four_workers_claim_four_processes_while_extra_posts_remain_durable() {
    let f = fixture();
    let server = Server::start(&f, "delayed", false);
    let mut receipts = Vec::new();
    for index in 0..7 {
        let (status, receipt) = support::raw_post(
            &server,
            &format!("queue-{index}"),
            "Could @planner review this?",
            None,
        );
        assert_eq!(status, 200, "{receipt}");
        assert_eq!(receipt["channel"]["state"], "routing");
        receipts.push(receipt);
    }
    until(|| calls(&f).len() == 4);
    assert_eq!(f.count("SELECT COUNT(*) FROM transition_facts WHERE actor_id='channel-router/v1' AND transition_kind='dispatching'"),4);
    assert_eq!(f.count("SELECT COUNT(*) FROM collaboration_messages"), 7);
    let pid =
        serde_json::from_slice::<Value>(&fs::read(f.state.join("dashboard/server.json")).unwrap())
            .unwrap()["pid"]
            .as_u64()
            .unwrap();
    let workers = fs::read_dir(format!("/proc/{pid}/task"))
        .unwrap()
        .filter_map(Result::ok)
        .filter(|task| {
            fs::read_to_string(task.path().join("comm"))
                .unwrap_or_default()
                .starts_with("channel-router-")
        })
        .count();
    assert_eq!(workers, 4);
    eprintln!(
        "queue evidence: 7 committed posts, 4 blocked model calls, {workers} owned router threads, 3 jobs waiting"
    );
    fs::write(f.root.join("router-release"), "").unwrap();
    for (index, initial) in receipts.iter().enumerate() {
        let (_, finished) = post(
            &server,
            &format!("queue-{index}"),
            "Could @planner review this?",
            None,
        );
        assert_eq!(finished["message_id"], initial["message_id"]);
        assert_eq!(finished["channel"]["state"], "routed");
    }
    assert_eq!(calls(&f).len(), 7);
    let orders: Vec<(String, i64)> = f
        .db()
        .prepare("SELECT request_id,thread_order FROM collaboration_messages ORDER BY rowid")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        orders.iter().map(|(id, _)| id.clone()).collect::<Vec<_>>(),
        (0..7).map(|i| format!("queue-{i}")).collect::<Vec<_>>()
    );
}

#[test]
fn failed_and_empty_followups_preserve_baseline_deliveries_and_all_projections_without_notes() {
    for mention_root in [false, true] {
        for failed in [false, true] {
            let f = fixture();
            let root = if mention_root {
                let mut runtime = f.runtime("w1", "unused");
                runtime.current_pane_id = None;
                namedrop::send_operator_message(
                    "mention-root",
                    "@planner Start here.",
                    None,
                    &runtime,
                )
                .unwrap()
            } else {
                plain_root(&f, "root", "Start here.")
            };
            let response = if failed {
                "not json"
            } else {
                r#"{"to":[],"reason":"credit only"}"#
            };
            let server = Server::start(&f, response, false);
            let calls_before = calls(&f).len();
            let source = "Thanks @lead for the earlier help.";
            let parent = root["message_id"].as_str();
            let (status, receipt) = post(&server, "baseline", source, parent);
            assert_eq!(status, 200, "{receipt}");
            let expected = if failed { "router_failed" } else { "unrouted" };
            assert_eq!(receipt["channel"]["state"], expected);
            assert_eq!(receipt["channel"]["mentioned"], json!(["lead"]));
            assert_eq!(recipient_aliases(&receipt), ["planner"]);
            let id = receipt["message_id"].as_str().unwrap();
            fs::remove_file(f.state.join("dashboard/channel-routing.jsonl")).unwrap();
            let (_, replay) = post(&server, "baseline", source, parent);
            assert_eq!(replay["channel"]["state"], expected);
            assert_eq!(replay["message_id"], receipt["message_id"]);
            let page = history(&server);
            let row = page
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["message_id"] == id)
                .unwrap();
            assert_eq!(row["channel"], replay["channel"]);
            let context = support::context(&server, id);
            let row = context["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["message_id"] == id)
                .unwrap();
            assert_eq!(row["channel"], replay["channel"]);
            let mut stream = events(&server);
            let frame = next_frame(&mut stream, &mut String::new());
            let row = frame["messages"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["message_id"] == id)
                .unwrap();
            assert_eq!(row["channel"], replay["channel"]);
            assert_eq!(calls(&f).len(), calls_before + 1);
        }
    }
}

#[test]
fn restart_fails_both_queued_and_dispatching_posts_without_mutating_fingerprints() {
    let f = fixture();
    let server = Server::start(&f, "delayed", false);
    for index in 0..6 {
        assert_eq!(
            support::raw_post(
                &server,
                &format!("recover-{index}"),
                "Could @planner inspect?",
                None
            )
            .0,
            200
        );
    }
    until(|| calls(&f).len() == 4);
    let before: Vec<(String, String)> = f
        .db()
        .prepare(
            "SELECT message_id,request_fingerprint FROM idempotency_records ORDER BY request_id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    drop(server);
    let server = Server::start(&f, "not json", false);
    let page = history(&server);
    assert_eq!(page.as_array().unwrap().len(), 6);
    assert!(
        page.as_array()
            .unwrap()
            .iter()
            .all(|m| m["channel"]["state"] == "router_failed")
    );
    assert_eq!(calls(&f).len(), 4);
    let after: Vec<(String, String)> = f
        .db()
        .prepare(
            "SELECT message_id,request_fingerprint FROM idempotency_records ORDER BY request_id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(before, after);
    assert_eq!(f.count("SELECT COUNT(*) FROM message_recipients"), 0);
}

#[cfg(target_os = "linux")]
#[test]
fn hard_crash_kills_router_descendants_before_successor_can_route() {
    let f = fixture();
    let server = Server::start(&f, "delayed", false);
    fs::write(
        f.root.join("fake-router"),
        r#"#!/usr/bin/python3
import json, os, subprocess, sys, time
root = os.path.dirname(__file__)
sys.stdin.read()
child = subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'])
with open(os.path.join(root,'descendant-pids.json'),'w') as out:
    json.dump([os.getpid(),child.pid],out)
while True:
    time.sleep(.01)
"#,
    )
    .unwrap();
    assert_eq!(
        support::raw_post(&server, "orphan", "Could @planner inspect?", None).0,
        200
    );
    until(|| f.root.join("descendant-pids.json").exists());
    let pids: Vec<u32> =
        serde_json::from_slice(&fs::read(f.root.join("descendant-pids.json")).unwrap()).unwrap();
    drop(server);
    let server = Server::start(&f, r#"{"to":["planner"],"reason":"review request"}"#, false);
    for pid in pids {
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).unwrap_or_default();
        eprintln!("crash evidence: previous pid {pid}, successor running, proc stat {stat:?}");
        assert!(
            stat.is_empty()
                || stat
                    .split(") ")
                    .nth(1)
                    .is_some_and(|tail| tail.starts_with('Z')),
            "orphan model process survived successor startup: {stat}"
        );
    }
    let (status, receipt) = post(&server, "successor", "Could @planner inspect?", None);
    assert_eq!(status, 200);
    assert_eq!(receipt["channel"]["state"], "routed");
}

#[test]
fn optional_failed_note_cannot_change_a_durable_empty_decision() {
    let f = fixture();
    let server = Server::start(&f, r#"{"to":[],"reason":"credit only"}"#, false);
    let (_, receipt) = post(
        &server,
        "empty-note",
        "Thanks @planner for earlier help.",
        None,
    );
    assert_eq!(receipt["channel"]["state"], "unrouted");
    let note_path = f.state.join("dashboard/channel-routing.jsonl");
    let mut note: Value =
        serde_json::from_str(fs::read_to_string(&note_path).unwrap().trim()).unwrap();
    note["router_failed"] = true.into();
    fs::write(note_path, format!("{note}\n")).unwrap();
    let (_, replay) = post(
        &server,
        "empty-note",
        "Thanks @planner for earlier help.",
        None,
    );
    assert_eq!(replay["channel"]["state"], "unrouted");
    assert_eq!(history(&server)[0]["channel"]["state"], "unrouted");
}

#[test]
fn retiring_or_disabling_a_candidate_during_routing_prevents_delivery() {
    for column in ["retired", "collaboration_disabled"] {
        let f = fixture();
        let server = Server::start(&f, "delayed", false);
        assert_eq!(
            support::raw_post(&server, "invalidated", "Could @planner inspect?", None).0,
            200
        );
        until(|| calls(&f).len() == 1);
        f.db().execute(&format!("UPDATE pane_addresses SET {column}=1 WHERE worktree_id='w1' AND address_id='address-w1-planner'"),[]).unwrap();
        fs::write(f.root.join("router-release"), "").unwrap();
        let (_, receipt) = post(&server, "invalidated", "Could @planner inspect?", None);
        assert_eq!(receipt["channel"]["state"], "unrouted");
        assert_eq!(receipt["recipients"], json!([]));
        assert_eq!(f.count("SELECT COUNT(*) FROM mailbox_deliveries"), 0);
    }
}

#[test]
fn handoff_waits_for_admitted_mutation_then_cancels_router_before_successor() {
    use std::io::Read;
    struct Gate(std::path::PathBuf);
    impl Drop for Gate {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }
    let f = fixture();
    let server = Server::start(&f, "delayed", false);
    let (_, pending) = support::raw_post(&server, "handoff-route", "Could @planner inspect?", None);
    until(|| calls(&f).len() == 1);
    let gate = Gate(f.root.join("prompt-gate"));
    fs::write(&gate.0, "").unwrap();
    assert_eq!(
        support::raw_post(&server, "admitted-send", "@lead Inspect.", None).0,
        200
    );
    until(|| f.root.join("prompt-entered").exists());
    let mut stream = events(&server);
    let mut buffered = String::new();
    next_frame(&mut stream, &mut buffered);
    fs::create_dir_all(f.state.join("runtime")).unwrap();
    fs::write(f.state.join("runtime/migration.json"), "{}").unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while !buffered.contains("event: restarting") {
        assert!(Instant::now() < deadline, "server did not begin handoff");
        let mut bytes = [0u8; 8192];
        let count = stream.read(&mut bytes).unwrap();
        assert!(count > 0);
        buffered.push_str(&String::from_utf8_lossy(&bytes[..count]));
    }
    assert_eq!(f.count("SELECT COUNT(*) FROM transition_facts WHERE actor_id='channel-router/v1' AND transition_kind='failed'"),0,"handoff cancelled routing before an admitted mutation finished");
    drop(gate);
    until(|| {
        f.count("SELECT COUNT(*) FROM transition_facts WHERE actor_id='channel-router/v1' AND transition_kind='failed'")==1
    });
    until(|| {
        f.state.join("dashboard/server.log").exists()
            && fs::read_to_string(f.state.join("dashboard/server.log"))
                .unwrap_or_default()
                .contains("activation was interrupted")
    });
    let id = pending["message_id"].as_str().unwrap();
    assert_eq!(
        f.db()
            .query_row(
                "SELECT COUNT(*) FROM message_recipients WHERE message_id=?1",
                [id],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    assert_eq!(attempt_records(&f)[0]["outcome"], "cancelled");
    fs::write(f.root.join("router-release"), "").unwrap();
    assert_eq!(calls(&f).len(), 1);
}
