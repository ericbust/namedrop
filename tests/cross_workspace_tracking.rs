use namedrop::{
    ActorKind, AgentSessionId, MutationAttribution, PaneAddressId, StateNamespace, StateStore,
    drain_mailboxes, execute_control_operation, execute_handoff_operation,
    execute_integration_operation, execute_message_operation, spaces_to_wake,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[path = "support/xws.rs"]
mod xws;

use xws::*;

/// The same tracked scenario with its worker in the lead's workspace or in
/// another one. Everything either user observes must match once the parts
/// that name a workspace are normalized away.
#[derive(Clone, Copy, Debug)]
enum Topology {
    Local,
    Remote,
}

struct Worker {
    workspace: &'static str,
    pane: &'static str,
    alias: &'static str,
    mention: &'static str,
}

impl Topology {
    fn worker(self) -> Worker {
        match self {
            Self::Local => Worker {
                workspace: "w1",
                pane: "pane-1-planner",
                alias: "planner",
                mention: "@planner",
            },
            Self::Remote => Worker {
                workspace: "w2",
                pane: "pane-2-tester",
                alias: "tester",
                mention: "#ws2 @tester",
            },
        }
    }
}

const LEAD: (&str, &str) = ("w1", "pane-1-lead");

fn fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture.register_all();
    fixture
}

fn handoff(fixture: &Fixture, at: (&str, &str), op: &str, input: Value) -> Result<Value, String> {
    execute_handoff_operation(op, input, &fixture.runtime(at.0, at.1))
}

fn begin(fixture: &Fixture, key: &str) -> Value {
    handoff(
        fixture,
        LEAD,
        "handoff.begin",
        json!({"request_id":key,"deliverable":"A reviewed release"}),
    )
    .unwrap()["coordination_id"]
        .clone()
}

fn assign(fixture: &Fixture, id: &Value, key: &str, mention: &str) -> Result<Value, String> {
    fixture.try_send(
        LEAD.0,
        LEAD.1,
        json!({"request_id":key,"source":format!("{mention} Review the release notes."),
               "author_kind":"agent_session","coordination_id":id}),
    )
}

fn delivered(fixture: &Fixture, workspace: &str, pane: &str) -> Value {
    fixture.drain(workspace);
    fixture.herdr.last_envelope(pane)
}

fn reply(
    fixture: &Fixture,
    worker: &Worker,
    envelope: &Value,
    key: &str,
    extra: Value,
) -> Result<Value, String> {
    let mut input = json!({"request_id":key,"source":"@lead Reviewed; two fixes needed.",
        "author_kind":"agent_session","in_reply_to":envelope["control"]["message_id"],
        "attempt_id":envelope["control"]["attempt_id"]});
    input
        .as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    execute_message_operation(
        "reply",
        input,
        &fixture.runtime(worker.workspace, worker.pane),
    )
}

fn receive_lead(fixture: &Fixture, wanted: impl Fn(&Value) -> bool) -> Value {
    for _ in 0..6 {
        fixture.drain(LEAD.0);
        let inbox = execute_integration_operation(
            "delivery.inbox",
            json!({}),
            &fixture.runtime(LEAD.0, LEAD.1),
        )
        .unwrap();
        for envelope in inbox["envelopes"].as_array().unwrap() {
            if wanted(envelope) {
                return envelope.clone();
            }
            execute_integration_operation(
                "complete",
                json!({"attempt_id":envelope["control"]["attempt_id"]}),
                &fixture.runtime(LEAD.0, LEAD.1),
            )
            .unwrap();
        }
    }
    panic!("the lead never received the expected envelope")
}

fn results(fixture: &Fixture, id: &Value) -> Value {
    handoff(
        fixture,
        LEAD,
        "handoff.results",
        json!({"coordination_id":id}),
    )
    .unwrap()
}

fn bound_operation(envelope: &Value, token: &str) -> Vec<Value> {
    envelope["control"]["tool"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|operation| operation["token"] == token)
        .map(|operation| operation["input"].clone())
        .collect()
}

fn unresolved_attention(fixture: &Fixture, workspace: &str) -> Vec<(String, String, String)> {
    let db = fixture.db();
    let mut statement = db
        .prepare(
            "SELECT source_kind, source_id, summary FROM attention_items
             WHERE worktree_id = ?1 AND resolved_at_ms IS NULL ORDER BY rowid",
        )
        .unwrap();
    statement
        .query_map([workspace], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

fn assert_nothing_mirrored(fixture: &Fixture) {
    for table in [
        "coordinations",
        "coordination_assignments",
        "coordination_links",
    ] {
        assert_eq!(
            fixture.count(&format!(
                "SELECT COUNT(*) FROM {table} WHERE worktree_id <> 'w1'"
            )),
            0,
            "{table} rows exist only in the lead's workspace"
        );
    }
}

fn row_counts(fixture: &Fixture) -> BTreeMap<&'static str, i64> {
    [
        "collaboration_messages",
        "mailbox_deliveries",
        "delivery_attempts",
        "delivery_dispositions",
        "coordinations",
        "coordination_assignments",
        "coordination_links",
        "bridged_messages",
        "attention_items",
    ]
    .into_iter()
    .map(|table| {
        (
            table,
            fixture.count(&format!("SELECT COUNT(*) FROM {table}")),
        )
    })
    .collect()
}

/// Strips what names a workspace, a recipient, or a generated id, and numbers
/// the remaining ids by first appearance.
fn normalize(value: &Value) -> Value {
    fn ids(text: &str, seen: &mut BTreeMap<String, usize>) -> String {
        let mut text = text.to_owned();
        for (from, to) in [
            ("bridge:w1:", ""),
            ("bridge:w2:", ""),
            ("#ws2 @tester", "@<worker>"),
            ("#w2 @tester", "@<worker>"),
            ("envoy:w2:address-w2-tester", "<worker-address>"),
            ("address-w2-tester", "<worker-address>"),
            ("address-w1-planner", "<worker-address>"),
            ("session-pane-2-tester", "<worker-session>"),
            ("session-pane-1-planner", "<worker-session>"),
            ("@tester", "@<worker>"),
            ("@planner", "@<worker>"),
        ] {
            text = text.replace(from, to);
        }
        if matches!(text.as_str(), "tester" | "planner") {
            return "<worker>".to_owned();
        }
        let mut out = String::new();
        let mut rest = text.as_str();
        'scan: while !rest.is_empty() {
            for kind in [
                "delivery-attempt-",
                "coordination-",
                "assignment-",
                "message-",
                "delivery-",
                "thread-",
            ] {
                if let Some(tail) = rest.strip_prefix(kind) {
                    let digits = tail.chars().take_while(char::is_ascii_digit).count();
                    if digits > 0 {
                        let id = &rest[..kind.len() + digits];
                        let next = seen.len();
                        let number = *seen.entry(id.to_owned()).or_insert(next);
                        out.push_str(&format!("<{}{number}>", kind.trim_end_matches('-')));
                        rest = &tail[digits..];
                        continue 'scan;
                    }
                }
            }
            let character = rest.chars().next().unwrap();
            out.push(character);
            rest = &rest[character.len_utf8()..];
        }
        out
    }
    fn walk(value: &Value, seen: &mut BTreeMap<String, usize>) -> Value {
        match value {
            Value::String(text) => Value::String(ids(text, seen)),
            Value::Array(items) => {
                Value::Array(items.iter().map(|item| walk(item, seen)).collect())
            }
            Value::Object(map) => Value::Object(
                map.iter()
                    .filter(|(key, _)| key.as_str() != "workspace")
                    .map(|(key, value)| (key.clone(), walk(value, seen)))
                    .collect(),
            ),
            other => other.clone(),
        }
    }
    walk(value, &mut BTreeMap::new())
}

fn parity(scenario: fn(&Fixture, Topology) -> Value) {
    let local = scenario(&fixture(), Topology::Local);
    let remote = {
        let fixture = fixture();
        let observed = scenario(&fixture, Topology::Remote);
        assert_nothing_mirrored(&fixture);
        observed
    };
    assert_eq!(
        normalize(&local),
        normalize(&remote),
        "a remote worker must look exactly like a local one\nlocal: {local:#}\nremote: {remote:#}"
    );
}

fn round_trip(fixture: &Fixture, topology: Topology) -> Value {
    let worker = topology.worker();
    let id = begin(fixture, "release-1");
    let sent = assign(fixture, &id, "assign-1", worker.mention).unwrap();
    let request = delivered(fixture, worker.workspace, worker.pane);
    let replied = reply(
        fixture,
        &worker,
        &request,
        "reply-1",
        json!({"work_outcome":"completed"}),
    )
    .unwrap();
    let answer = delivered(fixture, LEAD.0, LEAD.1);
    let before = results(fixture, &id);
    let reconciled: Vec<Value> = before["assignments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| json!({"assignment_id":a["assignment_id"],"revision":a["revision"]}))
        .collect();
    execute_integration_operation(
        "complete",
        json!({"attempt_id":answer["control"]["attempt_id"]}),
        &fixture.runtime(LEAD.0, LEAD.1),
    )
    .unwrap();
    let resolved = handoff(
        fixture,
        LEAD,
        "handoff.resolve",
        json!({"request_id":"resolve-1","coordination_id":id,"outcome":"completed",
               "report":"The release notes are reviewed.","reconciled_assignments":reconciled}),
    )
    .unwrap();
    let report = receive_lead(fixture, |envelope| {
        envelope["untrusted_body"] == "The release notes are reviewed."
    });
    json!({
        "sent": sent["coordination"],
        "request": {
            "coordination_id": request["control"]["coordination_id"],
            "assignments": request["control"]["assignments"],
            "reply": bound_operation(&request, "reply")[0]["work_outcome"],
        },
        "replied": replied["coordination"],
        "answer": {
            "body": answer["untrusted_body"],
            "assignments": answer["control"]["assignments"],
            "coordination_id": answer["control"]["coordination_id"],
        },
        "before": before["assignments"],
        "resolved": {"outcome": resolved["outcome"], "assignments": resolved["assignments"]},
        "report": {"coordination_id": report["control"]["coordination_id"], "reply_mode": report["control"]["reply_mode"]},
    })
}

#[test]
fn a_remote_worker_completes_a_tracked_handoff_exactly_like_a_local_one() {
    parity(round_trip);
}

fn restart(fixture: &Fixture, workspace: &str, alias: &str, pane: &str, session: &str) {
    fixture
        .herdr
        .sessions
        .lock()
        .unwrap()
        .insert(pane.to_owned(), session.to_owned());
    StateStore::open(&fixture.state)
        .unwrap()
        .transaction(|transaction| {
            transaction
                .observe_agent_session(
                    &StateNamespace::for_space("socket-a", workspace),
                    &format!("observation-{session}"),
                    &PaneAddressId::new(address_id(workspace, alias)),
                    &AgentSessionId::new(session),
                    "idle",
                    &MutationAttribution::new(ActorKind::Human, "test", 60),
                )
                .map(|_| ())
        })
        .unwrap();
    fixture.register(workspace, alias, pane);
}

fn continuation(fixture: &Fixture, topology: Topology) -> Value {
    let worker = topology.worker();
    let id = begin(fixture, "release-2");
    assign(fixture, &id, "assign-2", worker.mention).unwrap();
    let request = delivered(fixture, worker.workspace, worker.pane);
    reply(
        fixture,
        &worker,
        &request,
        "reply-2",
        json!({"work_outcome":"needs_continuation"}),
    )
    .unwrap();
    let answer = delivered(fixture, LEAD.0, LEAD.1);
    execute_integration_operation(
        "complete",
        json!({"attempt_id":answer["control"]["attempt_id"]}),
        &fixture.runtime(LEAD.0, LEAD.1),
    )
    .unwrap();
    let nudge = delivered(fixture, LEAD.0, LEAD.1);
    let bound = bound_operation(&nudge, "send");
    let mut next = bound
        .iter()
        .find(|input| input["assignment_id"].is_string() && input.get("context").is_none())
        .expect("the nudge binds a continuation for the worker")
        .clone();
    next["source"] = json!(
        next["source"]
            .as_str()
            .unwrap()
            .replace("<specific continuation instruction>", "Fix both issues.")
    );
    fixture.try_send(LEAD.0, LEAD.1, next.clone()).unwrap();
    let continued = delivered(fixture, worker.workspace, worker.pane);
    let mut stale = next.clone();
    stale["request_id"] = json!("continue-stale");
    let stale = fixture.try_send(LEAD.0, LEAD.1, stale).unwrap_err();
    let unexplained = reply(
        fixture,
        &worker,
        &continued,
        "reply-3",
        json!({"work_outcome":"blocked"}),
    )
    .unwrap_err();
    let explicit = reply(
        fixture,
        &worker,
        &continued,
        "reply-3",
        json!({"work_outcome":"completed","coordination_id":id}),
    )
    .unwrap_err();
    reply(
        fixture,
        &worker,
        &continued,
        "reply-3",
        json!({"work_outcome":"blocked","blocked_reason":"The CI runner is down."}),
    )
    .unwrap();
    json!({
        "nudge": nudge["untrusted_body"],
        "bound": bound,
        "continued": continued["control"]["assignments"],
        "stale": stale,
        "unexplained": unexplained,
        "explicit": explicit,
        "results": results(fixture, &id)["assignments"],
    })
}

#[test]
fn a_remote_assignment_continues_by_revision_exactly_like_a_local_one() {
    parity(continuation);
}

fn restart_and_adopt(fixture: &Fixture, topology: Topology) -> Value {
    let worker = topology.worker();
    let id = begin(fixture, "release-3");
    assign(fixture, &id, "assign-3", worker.mention).unwrap();
    let request = delivered(fixture, worker.workspace, worker.pane);
    fixture.status(LEAD.1, "working");
    restart(fixture, LEAD.0, "lead", LEAD.1, "session-lead-2");
    let rows = row_counts(fixture);
    let refused = reply(
        fixture,
        &worker,
        &request,
        "reply-4",
        json!({"work_outcome":"completed"}),
    )
    .unwrap_err();
    assert_eq!(row_counts(fixture), rows, "a refused reply writes nothing");
    let adopted = handoff(
        fixture,
        LEAD,
        "handoff.adopt",
        json!({"request_id":"adopt-1","coordination_id":id}),
    )
    .unwrap();
    let retried = reply(
        fixture,
        &worker,
        &request,
        "reply-4",
        json!({"work_outcome":"completed"}),
    )
    .unwrap();

    let second = begin(fixture, "release-4");
    assign(fixture, &second, "assign-4", worker.mention).unwrap();
    let request = delivered(fixture, worker.workspace, worker.pane);
    reply(
        fixture,
        &worker,
        &request,
        "reply-5",
        json!({"work_outcome":"completed"}),
    )
    .unwrap();
    restart(fixture, LEAD.0, "lead", LEAD.1, "session-lead-3");
    handoff(
        fixture,
        LEAD,
        "handoff.adopt",
        json!({"request_id":"adopt-2","coordination_id":second}),
    )
    .unwrap();
    let queued_reply = fixture.text(
        "SELECT d.state FROM mailbox_deliveries d
         JOIN coordination_links l ON l.worktree_id = d.worktree_id AND l.message_id = d.message_id
         WHERE d.worktree_id = 'w1' AND l.kind = 'reply' AND d.address_id = 'address-w1-lead'
         ORDER BY d.rowid DESC LIMIT 1",
    );
    if let Topology::Remote = topology {
        assert_eq!(
            fixture.text(
                "SELECT d.state FROM mailbox_deliveries d
                 JOIN collaboration_messages m USING (session_socket_id, worktree_id, message_id)
                 WHERE d.worktree_id = 'w2' AND m.request_id = 'reply-5'"
            ),
            Some("cancelled".to_owned()),
            "the worker's own reply shows the cancel adoption made in the lead's workspace"
        );
    }
    json!({
        "refused": refused,
        "adoption": adopted["adoption"]["changed"],
        "retried": retried["coordination"],
        "queued_reply": queued_reply,
        "results": results(fixture, &second)["assignments"],
    })
}

#[test]
fn a_restarted_lead_adopts_a_remote_assignment_exactly_like_a_local_one() {
    parity(restart_and_adopt);
}

fn replay(fixture: &Fixture, topology: Topology) -> Value {
    let worker = topology.worker();
    let id = begin(fixture, "release-5");
    assign(fixture, &id, "assign-5", worker.mention).unwrap();
    let request = delivered(fixture, worker.workspace, worker.pane);
    let first = reply(
        fixture,
        &worker,
        &request,
        "reply-6",
        json!({"work_outcome":"completed"}),
    )
    .unwrap();
    let rows = row_counts(fixture);
    let again = reply(
        fixture,
        &worker,
        &request,
        "reply-6",
        json!({"work_outcome":"completed"}),
    )
    .unwrap();
    assert_eq!(row_counts(fixture), rows, "a replay writes nothing");
    let changed = reply(
        fixture,
        &worker,
        &request,
        "reply-6",
        json!({"work_outcome":"blocked","blocked_reason":"Changed my mind."}),
    )
    .unwrap_err();
    let resent = assign(fixture, &id, "assign-5", worker.mention).unwrap();
    json!({
        "first": first["coordination"],
        "again": {"coordination": again["coordination"], "replay": again["idempotent_replay"]},
        "changed": changed,
        "resent": {"coordination": resent["coordination"], "replay": resent["idempotent_replay"]},
        "links": fixture.count("SELECT COUNT(*) FROM coordination_links"),
    })
}

#[test]
fn a_replayed_remote_reply_records_once_exactly_like_a_local_one() {
    parity(replay);
}

#[test]
fn a_tracked_reply_to_anyone_but_the_lead_is_refused_in_both_workspaces() {
    // A bare name from another workspace is refused before the tracked-reply rule
    // runs, so both cases name @scout, which lives beside the worker in ws2.
    let worker = Topology::Remote.worker();
    for (topology, lead, mention) in [
        (Topology::Local, ("w2", "pane-2-lead"), "@tester"),
        (Topology::Remote, LEAD, "#ws2 @tester"),
    ] {
        let fixture = fixture();
        let id = handoff(
            &fixture,
            lead,
            "handoff.begin",
            json!({"request_id":"release-6","deliverable":"A reviewed release"}),
        )
        .unwrap()["coordination_id"]
            .clone();
        fixture
            .try_send(
                lead.0,
                lead.1,
                json!({"request_id":"assign-6","source":format!("{mention} Review the release notes."),
                       "author_kind":"agent_session","coordination_id":id}),
            )
            .unwrap();
        let request = delivered(&fixture, worker.workspace, worker.pane);
        let rows = row_counts(&fixture);
        let error = reply(
            &fixture,
            &worker,
            &request,
            "reply-7",
            json!({"source":"@lead @scout Reviewed.","work_outcome":"completed"}),
        )
        .unwrap_err();
        assert_eq!(
            error,
            "a tracked reply goes only to @lead, but this one also addresses @scout. Start the \
             source with @lead alone and write any other name without the @",
            "{topology:?}"
        );
        assert_eq!(row_counts(&fixture), rows, "{topology:?}");
    }
}

#[test]
fn a_remote_group_returns_one_tracked_result_set_naming_each_member_by_workspace() {
    let fixture = fixture();
    namedrop::execute_group_operation(
        "group.set",
        json!({"alias":"crew","members":["tester","scout"]}),
        &fixture.runtime("w2", "pane-2-lead"),
    )
    .unwrap();
    let id = begin(&fixture, "release-7");
    let sent = assign(&fixture, &id, "assign-7", "#ws2 @crew").unwrap();
    assert_eq!(sent["coordination"].as_array().unwrap().len(), 2);
    for (workspace, pane, alias) in [
        ("w2", "pane-2-tester", "tester"),
        ("w2", "pane-2-scout", "scout"),
    ] {
        let worker = Worker {
            workspace,
            pane,
            alias,
            mention: "",
        };
        let request = delivered(&fixture, workspace, pane);
        reply(
            &fixture,
            &worker,
            &request,
            &format!("reply-{alias}"),
            json!({"work_outcome":"completed","source":format!("@lead {} done.", worker.alias)}),
        )
        .unwrap();
    }
    let join = receive_lead(&fixture, |envelope| {
        envelope["control"]["result_set"].is_object()
    });
    assert_eq!(
        fixture
            .herdr
            .envelopes(LEAD.1)
            .iter()
            .filter(|envelope| envelope["control"]["result_set"].is_object())
            .count(),
        1,
        "exactly one tracked result set"
    );
    let sources: Vec<String> = bound_operation(&join, "send")
        .iter()
        .filter(|input| input["assignment_id"].is_string() && input.get("context").is_none())
        .map(|input| input["source"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        sources,
        [
            "#w2 @tester <specific continuation instruction>",
            "#w2 @scout <specific continuation instruction>",
        ]
    );
    let assignments = results(&fixture, &id)["assignments"].clone();
    assert!(
        assignments
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["work_outcome"] == "completed" && a["workspace"] == "ws2"),
        "{assignments}"
    );
    assert_nothing_mirrored(&fixture);
}

#[test]
fn a_mixed_tracked_fan_out_keeps_the_local_continuation_unchanged() {
    let fixture = fixture();
    let id = begin(&fixture, "release-18");
    let sent = assign(&fixture, &id, "assign-18", "#ws1 #ws2 @planner @tester").unwrap();
    assert_eq!(sent["coordination"].as_array().unwrap().len(), 2);
    for worker in [Topology::Local.worker(), Topology::Remote.worker()] {
        let request = delivered(&fixture, worker.workspace, worker.pane);
        reply(
            &fixture,
            &worker,
            &request,
            &format!("reply-{}", worker.alias),
            json!({"work_outcome":"completed"}),
        )
        .unwrap();
    }
    let join = receive_lead(&fixture, |envelope| {
        envelope["control"]["result_set"].is_object()
    });
    let sources: Vec<Value> = bound_operation(&join, "send")
        .iter()
        .filter(|input| input["assignment_id"].is_string() && input.get("context").is_none())
        .map(|input| input["source"].clone())
        .collect();
    assert_eq!(
        sources,
        [
            "@planner <specific continuation instruction>",
            "#w2 @tester <specific continuation instruction>",
        ]
    );
    assert_eq!(
        fixture
            .herdr
            .envelopes(LEAD.1)
            .iter()
            .filter(|envelope| envelope["control"]["result_set"].is_object())
            .count(),
        1
    );
    assert_nothing_mirrored(&fixture);
}

#[test]
fn a_busy_remote_worker_holds_the_nudge_and_its_idle_wakes_the_lead() {
    let fixture = fixture();
    let worker = Topology::Remote.worker();
    let id = begin(&fixture, "release-8");
    assign(&fixture, &id, "assign-8", worker.mention).unwrap();
    let request = delivered(&fixture, worker.workspace, worker.pane);
    reply(
        &fixture,
        &worker,
        &request,
        "reply-8",
        json!({"work_outcome":"needs_continuation"}),
    )
    .unwrap();
    fixture.status(worker.pane, "working");
    let answer = delivered(&fixture, LEAD.0, LEAD.1);
    execute_integration_operation(
        "complete",
        json!({"attempt_id":answer["control"]["attempt_id"]}),
        &fixture.runtime(LEAD.0, LEAD.1),
    )
    .unwrap();
    let nudges = |fixture: &Fixture| {
        fixture
            .herdr
            .envelopes(LEAD.1)
            .into_iter()
            .filter(|envelope| envelope["control"]["coordination_nudge"] == true)
            .count()
    };
    fixture.drain("w1");
    assert_eq!(
        nudges(&fixture),
        0,
        "a working remote worker holds the nudge"
    );

    fixture.status(worker.pane, "idle");
    assert_eq!(
        spaces_to_wake(&fixture.state, "socket-a", "w2"),
        ["w2", "w1"],
        "a wake where the worker sits reconciles its lead's workspace"
    );
    for workspace in spaces_to_wake(&fixture.state, "socket-a", "w2") {
        fixture.drain(&workspace);
    }
    assert_eq!(nudges(&fixture), 1);
    assert_nothing_mirrored(&fixture);
}

#[test]
fn an_unreadable_remote_workspace_never_nudges_on_a_guess() {
    let fixture = fixture();
    let worker = Topology::Remote.worker();
    let id = begin(&fixture, "release-9");
    assign(&fixture, &id, "assign-9", worker.mention).unwrap();
    let request = delivered(&fixture, worker.workspace, worker.pane);
    reply(
        &fixture,
        &worker,
        &request,
        "reply-9",
        json!({"work_outcome":"needs_continuation"}),
    )
    .unwrap();
    let answer = delivered(&fixture, LEAD.0, LEAD.1);
    let nudged = |fixture: &Fixture| {
        fixture
            .herdr
            .envelopes(LEAD.1)
            .iter()
            .any(|envelope| envelope["control"]["coordination_nudge"] == true)
    };
    fixture
        .herdr
        .unreadable
        .lock()
        .unwrap()
        .insert("w2".to_owned());
    execute_integration_operation(
        "complete",
        json!({"attempt_id":answer["control"]["attempt_id"]}),
        &fixture.runtime(LEAD.0, LEAD.1),
    )
    .unwrap();
    fixture.drain("w1");
    assert!(
        !nudged(&fixture),
        "an unreadable worker workspace holds the nudge"
    );
    fixture.herdr.unreadable.lock().unwrap().clear();
    fixture.drain("w1");
    assert!(
        nudged(&fixture),
        "the nudge follows once the workspace reads again"
    );
}

#[test]
fn an_unanswered_remote_request_is_flagged_for_the_lead_and_acknowledged_there() {
    let fixture = fixture();
    let worker = Topology::Remote.worker();
    let id = begin(&fixture, "release-10");
    assign(&fixture, &id, "assign-10", worker.mention).unwrap();
    delivered(&fixture, worker.workspace, worker.pane);
    fixture.advance(31_000);
    fixture.drain("w2");
    let attempt = fixture
        .text(
            "SELECT a.attempt_id FROM delivery_attempts a JOIN delivery_attempt_state s
             USING (session_socket_id, worktree_id, attempt_id, address_id)
             WHERE a.worktree_id = 'w2' AND a.address_id = 'address-w2-tester' AND s.state = 'settled'",
        )
        .expect("the worker's attempt settled without an answer");
    assert!(
        unresolved_attention(&fixture, "w2").is_empty(),
        "the worker's workspace never judges a tracked request: {:?}",
        unresolved_attention(&fixture, "w2")
    );
    fixture.drain("w1");
    assert!(
        unresolved_attention(&fixture, "w1")
            .iter()
            .all(|(kind, ..)| kind != "agent_disposition"),
        "an open assignment awaiting its worker is not flagged"
    );

    handoff(
        &fixture,
        LEAD,
        "handoff.resolve",
        json!({"request_id":"resolve-10","coordination_id":id,"outcome":"blocked",
               "report":"The worker never answered.","reason":"No answer."}),
    )
    .unwrap();
    fixture.drain("w1");
    let reflected = format!("bridge:w2:{attempt}");
    let flagged = unresolved_attention(&fixture, "w1");
    assert!(
        flagged
            .iter()
            .any(|(kind, source, summary)| kind == "agent_disposition"
                && *source == reflected
                && summary.contains("unanswered when its coordination was resolved blocked")),
        "{flagged:?}"
    );
    let acknowledge = |fixture: &Fixture| {
        execute_control_operation(
            "delivery.acknowledge",
            json!({"attempt_id":reflected,"confirmed":true}),
            &fixture.runtime(LEAD.0, LEAD.1),
        )
    };
    let refused = acknowledge(&fixture).unwrap_err();
    assert!(
        refused.contains("is not retired and its session is unchanged"),
        "{refused}"
    );

    restart(
        &fixture,
        "w2",
        "tester",
        "pane-2-tester",
        "session-tester-2",
    );
    acknowledge(&fixture).unwrap();
    assert!(
        unresolved_attention(&fixture, "w1")
            .iter()
            .all(|(kind, ..)| kind != "agent_disposition"),
        "{:?}",
        unresolved_attention(&fixture, "w1")
    );
    assert_eq!(
        fixture.count(&format!(
            "SELECT COUNT(*) FROM delivery_dispositions WHERE attempt_id = '{attempt}'"
        )),
        0,
        "acknowledging in the lead's workspace disposes of nothing in the worker's"
    );
    assert_nothing_mirrored(&fixture);
}

#[test]
fn the_lead_sees_where_a_remote_assignment_really_stands() {
    let fixture = fixture();
    let worker = Topology::Remote.worker();
    let id = begin(&fixture, "release-11");
    fixture.sql("UPDATE worktrees SET paused = 1 WHERE worktree_id = 'w2'");
    let sent = assign(&fixture, &id, "assign-11", worker.mention).unwrap();
    let by_message = handoff(
        &fixture,
        LEAD,
        "handoff.results",
        json!({"message_id":sent["message_id"]}),
    )
    .unwrap();
    let recipient = &by_message["recipients"][0];
    assert_eq!(recipient["state"], "queued");
    assert_eq!(recipient["needs_attention"], false);
    assert_eq!(recipient["target"]["workspace"], "ws2");
    assert!(
        recipient["outlook"]["constraints"]
            .as_array()
            .unwrap()
            .contains(&json!("space_paused")),
        "{recipient}"
    );
    assert_eq!(
        results(&fixture, &id)["assignments"][0]["delivery"]["state"],
        "queued"
    );
    let status = handoff(&fixture, LEAD, "handoff.status", json!({})).unwrap();
    assert_eq!(
        status["handoffs"][0]["recipients"][0]["state"], "queued",
        "{status}"
    );

    fixture.advance(25 * 60 * 60 * 1_000);
    fixture.drain("w2");
    let held = results(&fixture, &id);
    assert_eq!(held["assignments"][0]["delivery"]["state"], "held");
    fixture.sql("UPDATE worktrees SET paused = 0 WHERE worktree_id = 'w2'");

    fixture
        .herdr
        .failing_prompts
        .lock()
        .unwrap()
        .insert("pane-2-scout".to_owned());
    let second = begin(&fixture, "release-12");
    let failed = assign(&fixture, &second, "assign-12", "#ws2 @scout").unwrap();
    fixture.drain("w2");
    let report = handoff(
        &fixture,
        LEAD,
        "handoff.results",
        json!({"message_id":failed["message_id"]}),
    )
    .unwrap();
    assert_eq!(report["recipients"][0]["state"], "failed", "{report}");
    assert_eq!(report["recipients"][0]["result_status"], "failed");
}

#[test]
fn a_remote_worker_without_the_handoff_capabilities_is_refused_and_nothing_is_written() {
    let fixture = fixture();
    fixture.sql(
        "DELETE FROM integration_registrations WHERE worktree_id = 'w2' AND address_id = 'address-w2-tester'",
    );
    let id = begin(&fixture, "release-13");
    let rows = row_counts(&fixture);
    let local = fixture.count("SELECT COUNT(*) FROM integration_registrations");
    let error = assign(&fixture, &id, "assign-13", "#ws2 @tester").unwrap_err();
    assert!(
        error.contains("exact-session Integration Registration"),
        "{error}"
    );
    assert_eq!(row_counts(&fixture), rows);
    assert_eq!(
        fixture.count("SELECT COUNT(*) FROM integration_registrations"),
        local
    );
}

#[test]
fn a_worker_naming_the_leads_coordination_learns_nothing_about_it() {
    let fixture = fixture();
    let id = begin(&fixture, "release-14");
    assign(&fixture, &id, "assign-14", "#ws2 @tester").unwrap();
    let rows = row_counts(&fixture);
    let at = ("w2", "pane-2-tester");
    for (op, input) in [
        (
            "handoff.resolve",
            json!({"request_id":"steal","coordination_id":"COORD","outcome":"cancelled",
                   "report":"Stolen.","reason":"Stolen."}),
        ),
        (
            "handoff.adopt",
            json!({"request_id":"steal","coordination_id":"COORD"}),
        ),
        ("handoff.results", json!({"coordination_id":"COORD"})),
    ] {
        let named = |coordination: &Value| {
            let text = input
                .to_string()
                .replace("\"COORD\"", &coordination.to_string());
            handoff(&fixture, at, op, serde_json::from_str(&text).unwrap()).unwrap_err()
        };
        assert_eq!(
            named(&id),
            named(&json!("coordination-unknown")),
            "{op} answers a real foreign id as it answers an unknown one"
        );
    }
    let send = |coordination: &Value| {
        fixture
            .try_send(
                at.0,
                at.1,
                json!({"request_id":"steal-send","source":"#ws1 @lead Done.",
                       "author_kind":"agent_session","coordination_id":coordination}),
            )
            .unwrap_err()
    };
    assert_eq!(send(&id), send(&json!("coordination-unknown")));
    assert_eq!(row_counts(&fixture), rows);
}

#[test]
fn a_tracked_copy_whose_original_is_missing_fails_closed() {
    let fixture = fixture();
    let id = begin(&fixture, "release-15");
    fixture.sql("UPDATE worktrees SET paused = 1 WHERE worktree_id = 'w2'");
    assign(&fixture, &id, "assign-15", "#ws2 @tester").unwrap();
    fixture.sql(
        "UPDATE bridged_messages SET origin_message_id = 'message-missing' WHERE worktree_id = 'w2';
         UPDATE worktrees SET paused = 0 WHERE worktree_id = 'w2';",
    );
    let error = drain_mailboxes(&fixture.runtime("w2", "pane-none")).unwrap_err();
    assert!(
        error.contains("cross-workspace record is inconsistent"),
        "{error}"
    );
    assert!(
        fixture.herdr.envelopes("pane-2-tester").is_empty(),
        "the worker never receives the request as untracked work"
    );
}

#[test]
fn an_untracked_remote_send_keeps_its_recipient_snapshot() {
    let fixture = fixture();
    fixture.agent_send(
        "w1",
        "pane-1-lead",
        "plain-1",
        "#ws2 @tester Plain request.",
    );
    assert_eq!(
        fixture.text(
            "SELECT COALESCE(session_snapshot, 'none') FROM message_recipients
             WHERE worktree_id = 'w1' AND address_id = 'envoy:w2:address-w2-tester'"
        ),
        Some("none".to_owned()),
        "an envoy recipient still pins no session of its own"
    );
}

#[test]
fn a_single_workspace_tracked_handoff_issues_the_same_herdr_commands() {
    let commands = |fixture: &Fixture| {
        fixture.herdr.commands.lock().unwrap().clear();
        round_trip(fixture, Topology::Local);
        fixture
            .herdr
            .commands
            .lock()
            .unwrap()
            .iter()
            .map(|command| command.split(" {").next().unwrap().to_owned())
            .collect::<Vec<_>>()
    };
    let single = Fixture::with_activated(1);
    single.register_all_activated(1);
    let several = fixture();
    let (single, several) = (commands(&single), commands(&several));
    assert_eq!(single, several);
    assert!(
        !single
            .iter()
            .any(|command| command.contains("w2") || command.contains("w3")),
        "{single:?}"
    );
}

#[test]
fn a_remote_reply_that_fails_at_any_step_leaves_no_partial_rows() {
    for (trigger, retried) in [
        (
            "CREATE TRIGGER inject AFTER INSERT ON bridged_messages WHEN NEW.worktree_id = 'w1'
             BEGIN SELECT RAISE(ABORT, 'injected after the forward'); END;",
            "after the forward",
        ),
        (
            "CREATE TRIGGER inject AFTER INSERT ON coordination_links WHEN NEW.kind = 'reply'
             BEGIN SELECT RAISE(ABORT, 'injected after the outcome'); END;",
            "after the outcome",
        ),
        (
            "CREATE TRIGGER inject AFTER INSERT ON delivery_dispositions WHEN NEW.worktree_id = 'w2'
             BEGIN SELECT RAISE(ABORT, 'injected after the disposition'); END;",
            "after the disposition",
        ),
    ] {
        let fixture = fixture();
        let worker = Topology::Remote.worker();
        let id = begin(&fixture, "release-16");
        assign(&fixture, &id, "assign-16", worker.mention).unwrap();
        let request = delivered(&fixture, worker.workspace, worker.pane);
        fixture.sql(trigger);
        let rows = row_counts(&fixture);
        let error = reply(
            &fixture,
            &worker,
            &request,
            "reply-16",
            json!({"work_outcome":"completed"}),
        )
        .unwrap_err();
        assert!(error.contains(retried), "{error}");
        assert_eq!(row_counts(&fixture), rows, "{retried}");
        fixture.sql("DROP TRIGGER inject;");
        reply(
            &fixture,
            &worker,
            &request,
            "reply-16",
            json!({"work_outcome":"completed"}),
        )
        .unwrap();
        assert_eq!(
            results(&fixture, &id)["assignments"][0]["work_outcome"],
            "completed",
            "{retried}"
        );
        assert_eq!(
            fixture.count("SELECT COUNT(*) FROM coordination_links WHERE kind = 'reply'"),
            1
        );
    }
}

#[test]
fn a_remote_no_reply_comes_home_without_completing_the_work() {
    let fixture = fixture();
    let worker = Topology::Remote.worker();
    let id = begin(&fixture, "release-17");
    let sent = assign(&fixture, &id, "assign-17", worker.mention).unwrap();
    let request = delivered(&fixture, worker.workspace, worker.pane);
    execute_integration_operation(
        "complete",
        json!({"attempt_id":request["control"]["attempt_id"]}),
        &fixture.runtime(worker.workspace, worker.pane),
    )
    .unwrap();
    let report = handoff(
        &fixture,
        LEAD,
        "handoff.results",
        json!({"message_id":sent["message_id"]}),
    )
    .unwrap();
    assert_eq!(report["recipients"][0]["result_status"], "no_reply");
    assert_eq!(
        results(&fixture, &id)["assignments"][0]["work_outcome"],
        "pending"
    );
}

#[test]
fn a_tracked_all_send_assigns_every_other_workspaces_name() {
    let fixture = fixture();
    let id = begin(&fixture, "release-19");
    let sent = assign(&fixture, &id, "assign-19", "#all @lead").unwrap();
    assert_eq!(sent["coordination"].as_array().unwrap().len(), 2, "{sent}");
    let assigned: Vec<Value> = results(&fixture, &id)["assignments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["recipient_address_id"].clone())
        .collect();
    assert_eq!(
        assigned,
        ["envoy:w2:address-w2-lead", "envoy:w3:address-w3-lead"],
        "the lead never assigns itself"
    );
    assert_nothing_mirrored(&fixture);
}
