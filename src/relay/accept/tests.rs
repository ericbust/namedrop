use super::*;
use crate::{PaneAddressDraft, PaneAddressId, PaneRoleId, StateStore};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

const REX: &str = "rex:/tmp/namedrop-relay-accept.sock";
const HOST_B: &str = "host-b";

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A sandboxed agent's reply runs through the deferred executor, whose
/// recovery reads it as the reply itself does.
#[test]
fn a_deferred_reply_to_a_relayed_message_reads_its_remote_participant() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "please answer"));
    let relayed = b.rows(
        "namedrop.sqlite3",
        "SELECT message_id FROM collaboration_messages",
    )[0][0]
        .as_str()
        .unwrap()
        .to_owned();
    let mux = crate::mux::fake::FakeMux::world()
        .space("session:b1")
        .checkout("/repo-b")
        .pane("block:b1")
        .tab("window:b1")
        .session("session-bob")
        .idle()
        .build();
    let ids = crate::ProcessIds::default();
    let runtime = crate::LifecycleRuntime {
        state_directory: &b.0,
        home_directory: None,
        agent_config: Default::default(),
        cwd: Path::new("/repo-b"),
        workspace_id: Some("session:b1"),
        current_pane_id: Some("block:b1"),
        session_socket_id: REX,
        mux: &mux,
        clock: &crate::SystemClock,
        ids: &ids,
    };
    let recovered = crate::message::recover_deferred_message(
        "reply",
        json!({"request_id": "reply-1", "in_reply_to": relayed, "source": "@alice@laptop done",
            "author_kind": "agent_session", "attempt_id": "attempt-1"}),
        &StateNamespace::for_space(REX, "session:b1"),
        &PaneAddressId::new("address-session:b1-bob"),
        &crate::AgentSessionId::new("session-bob"),
        &runtime,
    );
    assert_eq!(recovered, Ok(None));
}

/// B's state: `session:b1` holds `@bob`, `@bob2`, and `@dup`, `session:b2`
/// holds `@carol` and `@dup`, and `session:b3`, paused, holds `@sleepy`.
struct B(PathBuf);

impl B {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "namedrop-relay-accept-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        let attribution = MutationAttribution::new(ActorKind::Human, "test", 1);
        StateStore::open(&path)
            .unwrap()
            .transaction(|transaction| {
                for (space, aliases) in [
                    ("session:b1", &["bob", "bob2", "dup"][..]),
                    ("session:b2", &["carol", "dup"][..]),
                    ("session:b3", &["sleepy"][..]),
                ] {
                    let namespace = StateNamespace::for_space(REX, space);
                    transaction.activate_worktree(&namespace, &attribution)?;
                    for alias in aliases {
                        let id = format!("address-{space}-{alias}");
                        transaction.create_pane_address(
                            &namespace,
                            &PaneAddressDraft {
                                id: PaneAddressId::new(&id),
                                role_id: PaneRoleId::new(format!("role-{id}")),
                                role_name: (*alias).to_owned(),
                                role_description: (*alias).to_owned(),
                                alias: (*alias).to_owned(),
                            },
                            &attribution,
                        )?;
                    }
                }
                transaction.set_worktree_paused(
                    &StateNamespace::for_space(REX, "session:b3"),
                    true,
                    &attribution,
                )
            })
            .unwrap();
        Self(path)
    }

    fn call(&self, args: &Value) -> Answer {
        self.woken(args).0
    }

    /// The answer, and each space the accept woke.
    fn woken(&self, args: &Value) -> (Answer, Vec<String>) {
        let mut woken = Vec::new();
        let answer = accept(
            &self.0,
            &Endpoint::parse(REX),
            HOST_B,
            args.to_string().as_bytes(),
            &mut |space| woken.push(space.as_str().to_owned()),
        );
        (answer, woken)
    }

    fn rows(&self, file: &str, sql: &str) -> Vec<Vec<Value>> {
        let Ok(connection) = Connection::open(self.0.join(file)) else {
            return Vec::new();
        };
        let Ok(mut statement) = connection.prepare(sql) else {
            return Vec::new();
        };
        let columns = statement.column_count();
        statement
            .query_map([], |row| {
                (0..columns)
                    .map(|column| {
                        Ok(match row.get_ref(column)? {
                            rusqlite::types::ValueRef::Null => Value::Null,
                            rusqlite::types::ValueRef::Integer(value) => json!(value),
                            value => json!(value.as_str().unwrap_or_default()),
                        })
                    })
                    .collect()
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    /// Each relayed message: space, thread, author alias, kind, request id,
    /// in reply to, body, and its delivery's recipient and state.
    fn messages(&self) -> Vec<Vec<Value>> {
        self.rows(
            "namedrop.sqlite3",
            "SELECT m.worktree_id, m.thread_id, m.author_alias_snapshot, m.author_kind,
                m.request_id, m.in_reply_to, m.body, r.alias_snapshot, d.state
             FROM collaboration_messages m
             JOIN message_recipients r USING (session_socket_id, worktree_id, message_id)
             JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, message_id, address_id)
             ORDER BY m.created_at_ms, m.rowid",
        )
    }

    fn inbound(&self) -> usize {
        self.rows("relay.sqlite3", "SELECT * FROM inbound").len()
    }
}

impl Drop for B {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn deliver(id: &str, to: &str, thread: &str, text: &str) -> Value {
    json!({
        "op": "deliver",
        "expect_host_id": HOST_B,
        "from": {"host_id": "host-a", "name": "alice", "host": "laptop"},
        "item": {"id": id, "to": to, "thread": thread, "in_reply_to": null, "text": text},
        "ack_through": 0,
    })
}

fn outcome(answer: &Answer) -> (Status, Option<String>) {
    let result = answer.result.as_ref().unwrap();
    (result.status, result.reason.clone())
}

#[test]
fn each_recipient_is_accepted_once_from_the_sender_s_relay_author() {
    let b = B::new();
    let (answer, woken) = b.woken(&deliver("m1", "bob", "t1", "hello bob"));
    assert_eq!(woken, ["session:b1"]);
    assert_eq!(
        answer,
        Answer {
            host_id: HOST_B.to_owned(),
            can_reply: true,
            result: Some(Outcome {
                id: "m1".to_owned(),
                to: "bob".to_owned(),
                status: Status::Accepted,
                reason: None,
            }),
            outbox: Vec::new(),
            more: false,
            epoch: answer.epoch.clone(),
        }
    );
    assert!(!answer.epoch.is_empty());
    let messages = b.messages();
    assert_eq!(messages.len(), 1);
    assert_eq!(
        messages[0][2..],
        [
            json!("alice@laptop"),
            json!("agent_session"),
            json!("relay:host-a:m1:bob"),
            Value::Null,
            json!("hello bob"),
            json!("bob"),
            json!("queued"),
        ]
    );
    assert_eq!(messages[0][0], "session:b1");

    let (again, woken) = b.woken(&deliver("m1", "bob", "t1", "hello bob"));
    assert_eq!(outcome(&again), (Status::Duplicate, None));
    assert!(woken.is_empty());
    assert_eq!(b.messages().len(), 1);
    assert_eq!(b.inbound(), 1);
}

#[test]
fn two_recipients_of_one_message_in_one_space_each_get_a_message() {
    let b = B::new();
    for to in ["bob", "bob2"] {
        assert_eq!(
            outcome(&b.call(&deliver("m1", to, "t1", "hi both"))).0,
            Status::Accepted
        );
    }
    let messages = b.messages();
    assert_eq!(
        messages
            .iter()
            .map(|row| (row[4].clone(), row[7].clone()))
            .collect::<Vec<_>>(),
        [
            (json!("relay:host-a:m1:bob"), json!("bob")),
            (json!("relay:host-a:m1:bob2"), json!("bob2")),
        ]
    );
    assert_eq!(
        messages[0][1], messages[1][1],
        "one B thread for A's thread"
    );
}

#[test]
fn one_sender_thread_that_reaches_two_spaces_gets_a_thread_in_each() {
    let b = B::new();
    for (id, to) in [("m1", "bob"), ("m2", "carol"), ("m3", "bob")] {
        assert_eq!(
            outcome(&b.call(&deliver(id, to, "t1", "thread one"))).0,
            Status::Accepted
        );
    }
    let messages = b.messages();
    assert_eq!(messages[0][0], "session:b1");
    assert_eq!(messages[1][0], "session:b2");
    assert_eq!(messages[2][0], "session:b1");
    assert_ne!(messages[0][1], messages[1][1]);
    assert_eq!(messages[0][1], messages[2][1]);
    assert_eq!(
        b.rows(
            "relay.sqlite3",
            "SELECT host_id, thread, space, sender, display_host FROM remote_threads ORDER BY space"
        ),
        [
            ["host-a", "t1", "session:b1", "alice", "laptop"].map(|v| json!(v)),
            ["host-a", "t1", "session:b2", "alice", "laptop"].map(|v| json!(v)),
        ]
    );
}

#[test]
fn a_name_in_two_spaces_is_ambiguous_an_unknown_one_refused_and_a_repeat_replays_it() {
    let b = B::new();
    let here = super::super::machine_name();
    let ambiguous = b.call(&deliver("m1", "dup", "t1", "which"));
    assert_eq!(
        outcome(&ambiguous),
        (
            Status::Ambiguous,
            Some(format!(
                "@dup is in more than one session on {here}: session:b1, session:b2"
            ))
        )
    );
    let unknown = b.call(&deliver("m2", "nobody", "t1", "who"));
    assert_eq!(
        outcome(&unknown),
        (Status::Refused, Some(format!("no @nobody on {here}")))
    );
    let paused = b.call(&deliver("m3", "sleepy", "t1", "wake"));
    assert_eq!(
        outcome(&paused),
        (
            Status::Refused,
            Some(format!("@sleepy is in a paused space on {here}"))
        )
    );
    // The name now exists, and the stored refusal still answers the key.
    StateStore::open(&b.0)
        .unwrap()
        .transaction(|transaction| {
            transaction.create_pane_address(
                &StateNamespace::for_space(REX, "session:b1"),
                &PaneAddressDraft {
                    id: PaneAddressId::new("address-nobody"),
                    role_id: PaneRoleId::new("role-nobody"),
                    role_name: "nobody".to_owned(),
                    role_description: "nobody".to_owned(),
                    alias: "nobody".to_owned(),
                },
                &MutationAttribution::new(ActorKind::Human, "test", 2),
            )
        })
        .unwrap();
    assert_eq!(b.call(&deliver("m2", "nobody", "t1", "who")), unknown);
    assert_eq!(b.call(&deliver("m1", "dup", "t1", "which")), ambiguous);
    assert!(b.messages().is_empty());
}

#[test]
fn each_bad_field_is_refused_by_name_and_nothing_is_stored() {
    let b = B::new();
    let long = "x".repeat(129);
    let text = "x".repeat(61 * 1024);
    for (field, path, value) in [
        ("from.host", "/from/host", json!("lap\rtop")),
        ("from.name", "/from/name", json!("ali ce")),
        ("from.name", "/from/name", json!("a".repeat(65))),
        ("item.to", "/item/to", json!("bo b")),
        ("item.id", "/item/id", json!(long)),
        ("item.thread", "/item/thread", json!("t 1")),
        ("from.host_id", "/from/host_id", json!("")),
        ("item.text", "/item/text", json!(text)),
        ("item.text", "/item/text", json!("x".repeat(60 * 1024 + 1))),
        ("item.id", "/item/id", json!("m\u{7}1")),
        ("item.id", "/item/id", json!("m\u{e9}1")),
        ("item.thread", "/item/thread", json!("t\u{1b}1")),
        ("item.in_reply_to", "/item/in_reply_to", json!("b 1")),
        ("item.in_reply_to", "/item/in_reply_to", json!("b\u{7f}1")),
        ("expect_host_id", "/expect_host_id", json!("host\u{0}b")),
        ("expect_host_id", "/expect_host_id", json!("h\u{f6}st-b")),
    ] {
        let mut args = deliver("m1", "bob", "t1", "hi");
        *args.pointer_mut(path).unwrap() = value;
        let answer = b.call(&args);
        assert_eq!(
            outcome(&answer),
            (Status::Refused, Some(format!("{field} is not valid"))),
            "{field}"
        );
    }
    assert!(b.messages().is_empty());
    assert_eq!(b.inbound(), 0);
}

#[test]
fn a_reply_to_a_message_b_relayed_goes_to_its_thread_and_another_is_refused() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "first"));
    let relayed = b.rows(
        "namedrop.sqlite3",
        "SELECT message_id, thread_id FROM collaboration_messages",
    );
    let (message, thread) = (relayed[0][0].clone(), relayed[0][1].clone());
    let mut reply = deliver("m2", "bob", "t9", "answer");
    reply["item"]["in_reply_to"] = message.clone();
    assert_eq!(outcome(&b.call(&reply)).0, Status::Accepted);
    let messages = b.messages();
    assert_eq!(messages[1][1], thread);
    assert_eq!(messages[1][5], message);

    let mut stray = deliver("m3", "bob", "t1", "stray");
    stray["item"]["in_reply_to"] = json!("message-nowhere");
    assert_eq!(
        outcome(&b.call(&stray)),
        (
            Status::Refused,
            Some(format!(
                "{} relayed no message message-nowhere",
                super::super::machine_name()
            ))
        )
    );
}

#[test]
fn the_outbox_page_drops_what_was_acked_and_stops_at_256_kib_with_more() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "first"));
    let relay = Connection::open(b.0.join("relay.sqlite3")).unwrap();
    for n in 1..=6 {
        relay
            .execute(
                "INSERT INTO outbox (host_id, re, thread, sender, message_id, text, created_at_ms)
                 VALUES (?1, 'm1', 't1', 'bob', ?2, ?3, ?4)",
                params![
                    if n == 6 { "host-c" } else { "host-a" },
                    format!("b{n}"),
                    "y".repeat(60 * 1024),
                    super::super::now_ms()
                ],
            )
            .unwrap();
    }
    let collect = |ack: i64, epoch: &str| {
        b.call(&json!({
            "op": "collect", "expect_host_id": HOST_B,
            "from": {"host_id": "host-a", "name": "alice", "host": "laptop"},
            "ack_through": ack, "epoch": epoch,
        }))
    };
    let page = collect(0, "");
    assert_eq!(page.result, None);
    assert_eq!(
        page.outbox
            .iter()
            .map(|reply| reply.seq)
            .collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert!(page.more);
    assert!(page.can_reply);
    assert_eq!(page.outbox[0].from, "bob");
    assert_eq!(page.outbox[0].re, "m1");
    let epoch = page.epoch.clone();
    assert_eq!(
        collect(4, "another-epoch").outbox.len(),
        4,
        "an ack from another outbox epoch drops nothing"
    );
    let page = collect(4, &epoch);
    assert_eq!(
        page.outbox
            .iter()
            .map(|reply| reply.seq)
            .collect::<Vec<_>>(),
        [5]
    );
    assert!(!page.more);
    assert_eq!(collect(5, &epoch).outbox, []);
    assert_eq!(
        b.rows("relay.sqlite3", "SELECT host_id FROM outbox"),
        [[json!("host-c")]]
    );
}

/// B's `@bob` in block `block:b1`, and a runtime for him.
fn bob_answers(b: &B, in_reply_to: &str, source: &str) -> Value {
    let namespace = StateNamespace::for_space(REX, "session:b1");
    let attribution = MutationAttribution::new(ActorKind::Human, "test", 3);
    StateStore::open(&b.0)
        .unwrap()
        .transaction(|transaction| {
            if transaction.active_pane_bindings(&namespace)?.is_empty() {
                transaction.bind_pane(
                    &namespace,
                    &crate::PaneBindingId::new("binding-bob"),
                    &PaneAddressId::new("address-session:b1-bob"),
                    "block:b1",
                    &attribution,
                )?;
            }
            Ok(())
        })
        .unwrap();
    let mux = crate::mux::fake::FakeMux::world()
        .space("session:b1")
        .checkout("/repo-b")
        .pane("block:b1")
        .tab("window:b1")
        .session("session-bob")
        .idle()
        .build();
    let ids = crate::ProcessIds::default();
    let runtime = crate::LifecycleRuntime {
        state_directory: &b.0,
        home_directory: None,
        agent_config: Default::default(),
        cwd: Path::new("/repo-b"),
        workspace_id: Some("session:b1"),
        current_pane_id: Some("block:b1"),
        session_socket_id: REX,
        mux: &mux,
        clock: &crate::SystemClock,
        ids: &ids,
    };
    crate::execute_message_operation(
        "reply",
        json!({"request_id": format!("reply-{}", super::super::now_ms()), "in_reply_to": in_reply_to, "source": source}),
        &runtime,
    )
    .unwrap()
}

/// On B, an answer to a relayed message becomes one kept reply for A that
/// names A's message and thread, and a long one is cut with the notice.
#[test]
fn an_answer_to_a_relayed_message_is_kept_once_for_the_sender_s_pull() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "please answer"));
    let relayed = b.rows(
        "namedrop.sqlite3",
        "SELECT message_id FROM collaboration_messages",
    )[0][0]
        .as_str()
        .unwrap()
        .to_owned();
    let first = bob_answers(&b, &relayed, "@alice@laptop short answer");
    let long = "w".repeat(70 * 1024);
    let second = bob_answers(&b, &relayed, &format!("@alice@laptop {long}"));
    let endpoint = Endpoint::parse(REX);
    let mut started = Vec::new();
    for _ in 0..2 {
        super::super::capture::capture(&b.0, &endpoint, &mut |label| {
            started.push(label.to_owned())
        })
        .unwrap();
    }
    assert!(started.is_empty(), "B starts no worker");
    let kept = b.rows(
        "relay.sqlite3",
        "SELECT host_id, re, thread, sender, message_id, text FROM outbox ORDER BY seq",
    );
    assert_eq!(kept.len(), 2);
    assert_eq!(
        kept[0],
        [
            json!("host-a"),
            json!("m1"),
            json!("t1"),
            json!("bob"),
            first["message_id"].clone(),
            json!("short answer"),
        ]
    );
    assert_eq!(kept[1][4], second["message_id"]);
    let text = kept[1][5].as_str().unwrap();
    assert!(text.len() <= 60 * 1024);
    assert!(text.starts_with("www"));
    assert!(text.ends_with(super::super::wire::CUT_NOTICE));
    let page = b.call(&json!({
        "op": "collect", "expect_host_id": HOST_B,
        "from": {"host_id": "host-a", "name": "alice", "host": "laptop"}, "ack_through": 0,
    }));
    assert_eq!(page.outbox.len(), 2);
}

#[test]
fn expired_replies_are_counted_per_remote_sender() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "hi"));
    let relay = Connection::open(b.0.join("relay.sqlite3")).unwrap();
    for (host, message) in [("host-a", "b1"), ("host-a", "b2"), ("host-c", "b3")] {
        relay
            .execute(
                "INSERT INTO outbox (host_id, re, thread, sender, message_id, text, created_at_ms)
                 VALUES (?1, 'm1', 't1', 'bob', ?2, 'old', 0)",
                params![host, message],
            )
            .unwrap();
    }
    assert_eq!(
        super::super::expired_replies(&b.0),
        [("alice@laptop".to_owned(), 2), ("host-c".to_owned(), 1)]
    );
}

/// B's agent answers a relayed message by naming its sender as the pointer
/// does, `@alice@laptop`, and the answer reaches the relay author, whose
/// delivery settles at insert.
#[test]
fn a_reply_to_a_relayed_message_reaches_its_relay_author_and_settles() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "please answer"));
    let relayed = b.rows(
        "namedrop.sqlite3",
        "SELECT message_id, author_address_id FROM collaboration_messages",
    );
    let message = relayed[0][0].as_str().unwrap().to_owned();
    let author = PaneAddressId::new(relayed[0][1].as_str().unwrap());
    let namespace = StateNamespace::for_space(REX, "session:b1");
    let attribution = MutationAttribution::new(ActorKind::Human, "test", 3);
    let mut store = StateStore::open(&b.0).unwrap();
    store
        .transaction(|transaction| {
            transaction.bind_pane(
                &namespace,
                &crate::PaneBindingId::new("binding-bob"),
                &PaneAddressId::new("address-session:b1-bob"),
                "block:b1",
                &attribution,
            )?;
            let snapshot = transaction.routing_address_by_id(&namespace, &author)?;
            assert_eq!(snapshot.alias, "alice@laptop");
            assert!(snapshot.pane_binding_id.is_none());
            Ok(())
        })
        .unwrap();
    let mux = crate::mux::fake::FakeMux::world()
        .space("session:b1")
        .checkout("/repo-b")
        .pane("block:b1")
        .tab("window:b1")
        .session("session-bob")
        .idle()
        .build();
    let ids = crate::ProcessIds::default();
    let runtime = crate::LifecycleRuntime {
        state_directory: &b.0,
        home_directory: None,
        agent_config: Default::default(),
        cwd: Path::new("/repo-b"),
        workspace_id: Some("session:b1"),
        current_pane_id: Some("block:b1"),
        session_socket_id: REX,
        mux: &mux,
        clock: &crate::SystemClock,
        ids: &ids,
    };
    let reply = crate::execute_message_operation(
        "reply",
        json!({"request_id": "reply-1", "in_reply_to": message, "source": "@alice@laptop here it is"}),
        &runtime,
    )
    .unwrap();
    assert_eq!(
        reply["recipients"],
        json!([{"address_id": author.as_str(), "alias": "alice@laptop"}])
    );
    let answer = b.rows(
        "namedrop.sqlite3",
        &format!(
            "SELECT m.body, r.alias_snapshot, d.state FROM collaboration_messages m
             JOIN message_recipients r USING (session_socket_id, worktree_id, message_id)
             JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, message_id, address_id)
             WHERE m.message_id = '{}'",
            reply["message_id"].as_str().unwrap()
        ),
    );
    assert_eq!(
        answer,
        [["here it is", "alice@laptop", "settled"].map(|v| json!(v))]
    );
}

/// An answer that names an earlier relayed message answers that one, not
/// the latest.
#[test]
fn an_answer_names_the_relayed_message_its_in_reply_to_names() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "first"));
    b.call(&deliver("m2", "bob", "t1", "second"));
    let relayed = b.rows(
        "namedrop.sqlite3",
        "SELECT message_id FROM collaboration_messages ORDER BY rowid",
    );
    bob_answers(
        &b,
        relayed[0][0].as_str().unwrap(),
        "@alice@laptop to the first",
    );
    bob_answers(
        &b,
        relayed[1][0].as_str().unwrap(),
        "@alice@laptop to the second",
    );
    super::super::capture::capture(&b.0, &Endpoint::parse(REX), &mut |_| {}).unwrap();
    assert_eq!(
        b.rows("relay.sqlite3", "SELECT re, text FROM outbox ORDER BY seq"),
        [
            [json!("m1"), json!("to the first")],
            [json!("m2"), json!("to the second")],
        ]
    );
}

/// Outside Rex the relay never runs, so a drain there opens no relay
/// database.
#[test]
fn a_drain_outside_rex_captures_nothing() {
    let b = B::new();
    super::super::capture_after_drain(&b.0, "/tmp/namedrop-relay-herdr.sock");
    assert!(!b.0.join("relay.sqlite3").exists());
    super::super::capture_after_drain(&b.0, REX);
    assert!(b.0.join("relay.sqlite3").exists());
}

/// A terminal runs a new message as a command, so B relays to one only
/// while its owner allows it.
#[test]
fn a_terminal_recipient_is_refused_unless_b_allows_terminals() {
    let b = B::new();
    Connection::open(b.0.join("namedrop.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE pane_addresses SET pane_type = 'terminal' WHERE address_id = ?1",
            ["address-session:b1-bob"],
        )
        .unwrap();
    let here = super::super::machine_name();
    let refused = b.call(&deliver("m1", "bob", "t1", "printf relayed"));
    assert_eq!(
        outcome(&refused),
        (
            Status::Refused,
            Some(format!(
                "@bob is a terminal on {here}, and {here} relays mentions only to agents"
            ))
        )
    );
    assert!(b.messages().is_empty());
    assert_eq!(
        outcome(&b.call(&deliver("m1", "bob", "t1", "printf relayed"))).0,
        Status::Refused,
        "a retry gets the stored refusal"
    );
    assert_eq!(
        outcome(&b.call(&deliver("m2", "bob2", "t1", "an agent"))).0,
        Status::Accepted
    );

    fs::create_dir_all(b.0.join("relay")).unwrap();
    fs::write(b.0.join("relay/allow-terminals"), "").unwrap();
    assert_eq!(
        outcome(&b.call(&deliver("m3", "bob", "t1", "printf relayed"))).0,
        Status::Accepted
    );
    assert_eq!(
        b.messages()
            .iter()
            .map(|row| row[7].clone())
            .collect::<Vec<_>>(),
        [json!("bob2"), json!("bob")]
    );
}

/// When one A thread reaches B from two senders, an answer to one of them
/// goes back for the A message of the sender it names, not for the latest
/// relayed message.
#[test]
fn an_answer_goes_back_for_the_relay_author_it_names() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "from alice"));
    let mut dan = deliver("m2", "bob", "t1", "from dan");
    dan["from"]["name"] = json!("dan");
    assert_eq!(outcome(&b.call(&dan)).0, Status::Accepted);
    let relayed = b.rows(
        "namedrop.sqlite3",
        "SELECT message_id FROM collaboration_messages ORDER BY rowid",
    );
    let first = bob_answers(
        &b,
        relayed[0][0].as_str().unwrap(),
        "@alice@laptop to alice",
    );
    bob_answers(
        &b,
        first["message_id"].as_str().unwrap(),
        "@alice@laptop more for alice",
    );
    super::super::capture::capture(&b.0, &Endpoint::parse(REX), &mut |_| {}).unwrap();
    assert_eq!(
        b.rows("relay.sqlite3", "SELECT re, text FROM outbox ORDER BY seq"),
        [
            [json!("m1"), json!("to alice")],
            [json!("m1"), json!("more for alice")],
        ]
    );
}

/// A reply in a thread whose mapping is gone cannot reach A, so B counts
/// it with the expired replies Doctor reports.
#[test]
fn a_reply_b_cannot_route_is_counted_for_doctor() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "please answer"));
    let relayed = b.rows(
        "namedrop.sqlite3",
        "SELECT message_id FROM collaboration_messages",
    )[0][0]
        .as_str()
        .unwrap()
        .to_owned();
    Connection::open(b.0.join("relay.sqlite3"))
        .unwrap()
        .execute("DELETE FROM remote_threads", [])
        .unwrap();
    bob_answers(&b, &relayed, "@alice@laptop too late");
    super::super::capture::capture(&b.0, &Endpoint::parse(REX), &mut |_| {}).unwrap();
    assert!(b.rows("relay.sqlite3", "SELECT * FROM outbox").is_empty());
    assert_eq!(
        super::super::expired_replies(&b.0),
        [("alice@laptop".to_owned(), 1)]
    );
}

thread_local! {
    /// What this thread's accept runs once it has chosen its B thread.
    static THREAD_CHOSEN: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

pub(super) fn thread_chosen() {
    if let Some(hook) = THREAD_CHOSEN.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
}

/// Two first items of one A thread choose their B thread one at a time,
/// so the second one, sent while the first holds its choice, lands in the
/// same B thread.
#[test]
fn two_accepts_at_once_for_one_a_thread_share_one_b_thread() {
    let b = B::new();
    let (done, finished) = std::sync::mpsc::channel();
    let path = b.0.clone();
    let second = move || {
        let answer = accept(
            &path,
            &Endpoint::parse(REX),
            HOST_B,
            deliver("m1", "bob2", "t1", "second").to_string().as_bytes(),
            &mut |_| {},
        );
        done.send(answer).unwrap();
    };
    THREAD_CHOSEN.with(|chosen| {
        *chosen.borrow_mut() = Some(Box::new(move || {
            let running = std::thread::spawn(second);
            assert!(
                finished
                    .recv_timeout(std::time::Duration::from_millis(500))
                    .is_err(),
                "the second accept waits for the first"
            );
            SECOND.with(|kept| *kept.borrow_mut() = Some((running, finished)));
        }));
    });
    assert_eq!(
        outcome(&b.call(&deliver("m1", "bob", "t1", "first"))).0,
        Status::Accepted
    );
    let (running, finished) = SECOND.with(|kept| kept.borrow_mut().take()).unwrap();
    assert_eq!(outcome(&finished.recv().unwrap()).0, Status::Accepted);
    running.join().unwrap();
    let messages = b.messages();
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0][1], messages[1][1],
        "one B thread for A's thread"
    );
    assert_eq!(
        b.rows("relay.sqlite3", "SELECT COUNT(*) FROM remote_threads"),
        [[json!(1)]]
    );
}

thread_local! {
    /// The second accept of the test above, once the first let it run.
    static SECOND: std::cell::RefCell<
        Option<(std::thread::JoinHandle<()>, std::sync::mpsc::Receiver<Answer>)>,
    > = const { std::cell::RefCell::new(None) };
}

/// A reply of control characters, each six bytes once escaped, is cut to
/// fit B's text limit as encoded, so the page still carries it and the
/// replies behind it.
#[test]
fn an_escaped_reply_is_cut_to_fit_and_never_stalls_the_outbox() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "reply please"));
    let relayed = b.rows(
        "namedrop.sqlite3",
        "SELECT message_id FROM collaboration_messages",
    )[0][0]
        .as_str()
        .unwrap()
        .to_owned();
    let text = "\u{1}".repeat(60 * 1024);
    bob_answers(&b, &relayed, &format!("@alice@laptop {text}"));
    bob_answers(&b, &relayed, "@alice@laptop behind it");
    super::super::capture::capture(&b.0, &Endpoint::parse(REX), &mut |_| {}).unwrap();
    let page = b.call(&json!({"op": "collect", "expect_host_id": HOST_B,
        "from": {"host_id": "host-a", "name": "alice", "host": "laptop"}, "ack_through": 0}));
    assert!(!page.more);
    assert_eq!(page.outbox.len(), 2);
    let cut = &page.outbox[0].text;
    assert!(cut.ends_with(super::super::wire::CUT_NOTICE));
    assert!(serde_json::to_string(cut).unwrap().len() - 2 <= TEXT_LIMIT);
    assert_eq!(page.outbox[1].text, "behind it");
}

/// Each accepted item and each kept reply in a B thread mark its mapping
/// used, so a conversation that goes on keeps its mapping.
#[test]
fn an_item_or_a_reply_in_a_b_thread_keeps_its_mapping_in_use() {
    let b = B::new();
    let hour_ago = super::super::now_ms() - 60 * 60 * 1000;
    let used = || {
        b.rows("relay.sqlite3", "SELECT used_at_ms FROM remote_threads")[0][0]
            .as_i64()
            .unwrap()
    };
    let age = || {
        Connection::open(b.0.join("relay.sqlite3"))
            .unwrap()
            .execute("UPDATE remote_threads SET used_at_ms = ?1", [hour_ago])
            .unwrap();
    };
    b.call(&deliver("m1", "bob", "t1", "first"));
    age();
    b.call(&deliver("m2", "bob", "t1", "second"));
    assert!(used() > hour_ago, "an accepted item marks it used");
    age();
    let relayed = b.rows(
        "namedrop.sqlite3",
        "SELECT message_id FROM collaboration_messages",
    )[0][0]
        .as_str()
        .unwrap()
        .to_owned();
    bob_answers(&b, &relayed, "@alice@laptop an answer");
    super::super::capture::capture(&b.0, &Endpoint::parse(REX), &mut |_| {}).unwrap();
    assert!(used() > hour_ago, "a kept reply marks it used");
}

/// A text of exactly 60 KiB is accepted, one byte more is refused.
#[test]
fn a_text_of_exactly_60_kib_is_accepted() {
    let b = B::new();
    let text = "x".repeat(TEXT_LIMIT);
    assert_eq!(
        outcome(&b.call(&deliver("m1", "bob", "t1", &text))).0,
        Status::Accepted
    );
}

/// What B keeps for one A host is that host's alone: its keys, its threads,
/// the threads a reply may answer, and the outbox its ack drops.
#[test]
fn another_host_reaches_no_key_thread_or_outbox_of_this_one() {
    let b = B::new();
    let from = |host_id: &str, mut args: Value| {
        args["from"]["host_id"] = json!(host_id);
        args
    };
    b.call(&deliver("m1", "bob", "t1", "from host-a"));
    let relayed = b.rows(
        "namedrop.sqlite3",
        "SELECT message_id FROM collaboration_messages",
    )[0][0]
        .clone();
    let same_key = b.call(&from("host-c", deliver("m1", "bob", "t1", "from host-c")));
    assert_eq!(
        outcome(&same_key).0,
        Status::Accepted,
        "host-c's m1 is not host-a's"
    );
    let threads = b.rows(
        "namedrop.sqlite3",
        "SELECT thread_id FROM collaboration_messages ORDER BY rowid",
    );
    assert_ne!(threads[0], threads[1], "host-c's t1 is another B thread");

    let mut into = from("host-c", deliver("m2", "bob", "t9", "into host-a's thread"));
    into["item"]["in_reply_to"] = relayed.clone();
    let here = super::super::machine_name();
    assert_eq!(
        outcome(&b.call(&into)),
        (
            Status::Refused,
            Some(format!(
                "{here} relayed no message {}",
                relayed.as_str().unwrap()
            ))
        )
    );

    Connection::open(b.0.join("relay.sqlite3"))
        .unwrap()
        .execute(
            "INSERT INTO outbox (host_id, re, thread, sender, message_id, text, created_at_ms)
             VALUES ('host-a', 'm1', 't1', 'bob', 'b1', 'for host-a', ?1)",
            [super::super::now_ms()],
        )
        .unwrap();
    let epoch = b.call(&deliver("m1", "bob", "t1", "again")).epoch;
    b.call(&json!({"op": "collect", "expect_host_id": HOST_B,
        "from": {"host_id": "host-c", "name": "carol", "host": "studio"},
        "ack_through": 100, "epoch": epoch}));
    assert_eq!(
        b.rows("relay.sqlite3", "SELECT message_id FROM outbox"),
        [[json!("b1")]],
        "host-c's ack drops none of host-a's replies"
    );
}

/// B tells A it can reply while it keeps fewer than 10,000 replies for A.
#[test]
fn can_reply_holds_below_10_000_kept_replies() {
    let b = B::new();
    let mut relay = Connection::open(b.0.join("relay.sqlite3")).unwrap_or_else(|_| unreachable!());
    b.call(&deliver("m1", "bob", "t1", "first"));
    let fill = |relay: &mut Connection, from: usize, to: usize| {
        let transaction = relay.transaction().unwrap();
        for n in from..to {
            transaction
                .execute(
                    "INSERT INTO outbox (host_id, re, thread, sender, message_id, text,
                        created_at_ms) VALUES ('host-a', 'm1', 't1', 'bob', ?1, 'r', ?2)",
                    params![format!("b{n}"), super::super::now_ms()],
                )
                .unwrap();
        }
        transaction.commit().unwrap();
    };
    let collect = || {
        b.call(&json!({"op": "collect", "expect_host_id": HOST_B,
            "from": {"host_id": "host-a", "name": "alice", "host": "laptop"}}))
    };
    fill(&mut relay, 0, 9_999);
    assert!(collect().can_reply);
    fill(&mut relay, 9_999, 10_000);
    assert!(!collect().can_reply);
}
