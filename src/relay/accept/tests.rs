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
        "SELECT message_id FROM collaboration_messages WHERE grammar_version <> 'relay-notice/v1'",
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
struct B(PathBuf, Option<String>);

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
        Self(path, None)
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
            self.1.as_deref(),
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
             WHERE m.grammar_version <> 'relay-notice/v1'
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
            relay: 2,
            lost: Vec::new(),
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

/// The notice an agent gets before the first message of each thread from
/// a sender whose principal names no machine, which sent `laptop` as its
/// host.
const MARKED: &str =
    "[Namedrop: laptop is the name the sending machine gave itself, which Rex did not confirm.]";

/// A sender's host is the name it gave itself, so an agent gets a notice
/// that says so before the first message of each B thread, and each body
/// stays as sent. A Tailscale caller's device name, which its server fills
/// in, names the sender instead, with no notice.
#[test]
fn a_sender_s_own_host_name_is_marked_once_a_thread_and_a_named_device_replaces_it() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "first"));
    b.call(&deliver("m2", "bob", "t1", "second"));
    b.call(&deliver("m3", "bob", "t2", "other thread"));
    let bodies: Vec<_> = b.messages().into_iter().map(|row| row[6].clone()).collect();
    assert_eq!(
        bodies,
        [json!("first"), json!("second"), json!("other thread")]
    );
    assert_eq!(
        b.rows(
            "namedrop.sqlite3",
            "SELECT body FROM collaboration_messages WHERE grammar_version = 'relay-notice/v1'"
        ),
        vec![vec![json!(MARKED)]; 2]
    );
    let mut named = B::new();
    named.1 = Some("studio".to_owned());
    named.call(&deliver("m1", "bob", "t1", "first"));
    assert!(
        named
            .rows(
                "namedrop.sqlite3",
                "SELECT body FROM collaboration_messages WHERE grammar_version = 'relay-notice/v1'"
            )
            .is_empty()
    );
    let message = &named.messages()[0];
    assert_eq!(
        (&message[2], &message[6]),
        (&json!("alice@studio"), &json!("first"))
    );
    let thread = named.rows("relay.sqlite3", "SELECT display_host FROM remote_threads");
    assert_eq!(thread, [[json!("studio")]]);
}

/// The requests of the first merged relay, `2496500e`, as it sent them,
/// name no version, so they are version 1. B takes the item, pages its
/// outbox, and answers with only the fields that build reads and its own
/// version.
#[test]
fn b_takes_the_first_relay_s_requests() {
    let b = B::new();
    let deliver: Value =
        serde_json::from_str(include_str!("../fixtures/2496500e-deliver.json")).unwrap();
    let answer = b.call(&deliver);
    assert_eq!(outcome(&answer), (Status::Accepted, None));
    let store = RelayStore::open(&b.0, crate::relay::now_ms()).unwrap();
    for seq in 1..=3 {
        let reply = crate::relay::wire::Reply {
            seq: 0,
            re: "MESSAGE".to_owned(),
            thread: "THREAD".to_owned(),
            from: "bob".to_owned(),
            message_id: format!("b-{seq}"),
            text: format!("reply {seq}"),
        };
        assert!(
            store
                .keep_reply("host-a", &reply, crate::relay::now_ms())
                .unwrap()
        );
    }
    let collect = |ack_through: i64| {
        let mut collect: Value =
            serde_json::from_str(include_str!("../fixtures/2496500e-collect.json")).unwrap();
        collect["epoch"] = json!(answer.epoch);
        collect["ack_through"] = json!(ack_through);
        b.call(&collect)
    };
    let page = collect(0);
    let texts: Vec<&str> = page
        .outbox
        .iter()
        .map(|reply| reply.text.as_str())
        .collect();
    assert_eq!(texts, ["reply 1", "reply 2", "reply 3"]);
    for (answer, keys) in [
        (
            &answer,
            &[
                "can_reply",
                "epoch",
                "host_id",
                "more",
                "outbox",
                "relay",
                "result",
            ][..],
        ),
        (
            &page,
            &["can_reply", "epoch", "host_id", "more", "outbox", "relay"][..],
        ),
    ] {
        let encoded = serde_json::to_value(answer).unwrap();
        let named: Vec<&str> = encoded
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(named, keys);
    }
    assert!(collect(3).outbox.is_empty());
    assert_eq!(store.outbox_page("host-a", 0, 0, PAGE_LIMIT).unwrap().2, 0);
}

/// B names what it let go of for A to a version 2 A, and nothing to a
/// version 1 A, which would not read it.
#[test]
fn b_tells_a_version_2_a_what_it_let_go_of() {
    let b = B::new();
    drop(RelayStore::open(&b.0, crate::relay::now_ms()).unwrap());
    let full = rusqlite::Connection::open(b.0.join("relay.sqlite3")).unwrap();
    full.execute(
        "INSERT INTO outbox_lost (host_id, re, sender, since_ms, discarded, expired,
            updated_at_ms) VALUES ('host-a', 'm1', 'bob', 5, 2, 1, ?1)",
        [crate::relay::now_ms()],
    )
    .unwrap();
    let mut collect = deliver("m1", "bob", "t1", "x");
    collect["op"] = json!("collect");
    collect.as_object_mut().unwrap().remove("item");
    assert!(b.call(&collect).lost.is_empty(), "version 1");
    collect["relay"] = json!(2);
    assert_eq!(
        b.call(&collect).lost,
        [crate::relay::wire::Lost {
            re: "m1".to_owned(),
            from: "bob".to_owned(),
            since: 5,
            discarded: 2,
            expired: 1,
        }]
    );
}

/// A request from a newer relay is read as this version: B takes the item,
/// skips what it does not know, and names its own version.
#[test]
fn b_answers_a_newer_a_as_its_own_version() {
    let b = B::new();
    let mut request = deliver("m1", "bob", "t1", "hello bob");
    request["relay"] = json!(99);
    request["future"] = json!({"field": true});
    request["item"]["future"] = json!(1);
    let answer = b.call(&request);
    assert_eq!(outcome(&answer), (Status::Accepted, None));
    assert_eq!(answer.relay, 2);
}

/// A request B cannot read, from a newer relay, is refused with both
/// versions, so A can fall back to one this B reads.
#[test]
fn b_refuses_a_newer_request_it_cannot_read_naming_both_versions() {
    let b = B::new();
    let mut request = deliver("m1", "bob", "t1", "hello bob");
    request["op"] = json!("hello");
    request["relay"] = json!(99);
    let answer = b.call(&request);
    let (status, reason) = outcome(&answer);
    assert_eq!(status, Status::Refused);
    let reason = reason.unwrap();
    assert!(
        reason.starts_with(
            "this Namedrop's relay is version 2, which cannot read a version 99 request: "
        ),
        "{reason}"
    );
    assert_eq!(answer.relay, 2);
    request["relay"] = json!(2);
    let (_, reason) = outcome(&b.call(&request));
    assert!(
        reason
            .unwrap()
            .starts_with("the request is not a relay request: "),
        "a request of this version names no versions"
    );
    assert_eq!(b.inbound(), 0);
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
        ("item.to", "/item/to", json!("b".repeat(65))),
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
        "SELECT message_id, thread_id FROM collaboration_messages WHERE grammar_version <> 'relay-notice/v1'",
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

/// A collect that names the last reply A read gets the page after it, and
/// B keeps every reply A has not acked. Another epoch's names nothing.
#[test]
fn a_page_starts_after_the_last_reply_a_read_and_b_keeps_what_a_has_not_acked() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "first"));
    let relay = Connection::open(b.0.join("relay.sqlite3")).unwrap();
    for n in 1..=6 {
        relay
            .execute(
                "INSERT INTO outbox (host_id, re, thread, sender, message_id, text, created_at_ms)
                 VALUES ('host-a', 'm1', 't1', 'bob', ?1, ?2, ?3)",
                params![
                    format!("b{n}"),
                    "y".repeat(60 * 1024),
                    super::super::now_ms()
                ],
            )
            .unwrap();
    }
    let collect = |ack: i64, after: i64, epoch: &str| {
        let page = b.call(&json!({
            "op": "collect", "expect_host_id": HOST_B,
            "from": {"host_id": "host-a", "name": "alice", "host": "laptop"},
            "ack_through": ack, "epoch": epoch, "after": after,
        }));
        let seqs: Vec<i64> = page.outbox.iter().map(|reply| reply.seq).collect();
        (seqs, page.more, page.epoch)
    };
    let (_, _, epoch) = collect(0, 0, "");
    assert_eq!(collect(1, 4, &epoch), (vec![5, 6], false, epoch.clone()));
    assert_eq!(
        collect(1, 4, "another-epoch").0,
        [2, 3, 4, 5],
        "another epoch's ack and read name nothing"
    );
    assert_eq!(collect(1, 0, &epoch).0, [2, 3, 4, 5]);
}

/// B's `@bob` in block `block:b1`, and a runtime for him.
fn bob_answers(b: &B, in_reply_to: &str, source: &str) -> Value {
    bob_tries(b, in_reply_to, source).unwrap()
}

/// `@bob`'s reply in `session:b1`, or why Namedrop refused it.
fn bob_tries(b: &B, in_reply_to: &str, source: &str) -> Result<Value, String> {
    tries_as(b, "b1", "bob", in_reply_to, source)
}

/// The reply of `alias` in `session:<space>`, from block `block:<space>`.
fn tries_as(
    b: &B,
    space: &str,
    alias: &str,
    in_reply_to: &str,
    source: &str,
) -> Result<Value, String> {
    bound(b, space, alias);
    let (block, space) = (format!("block:{space}"), format!("session:{space}"));
    let mux = crate::mux::fake::FakeMux::world()
        .space(&space)
        .checkout("/repo-b")
        .pane(&block)
        .tab("window:b1")
        .session(&format!("session-{alias}"))
        .idle()
        .build();
    let ids = crate::ProcessIds::default();
    let runtime = crate::LifecycleRuntime {
        state_directory: &b.0,
        home_directory: None,
        agent_config: Default::default(),
        cwd: Path::new("/repo-b"),
        workspace_id: Some(&space),
        current_pane_id: Some(&block),
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
    .map_err(|error| error.to_string())
}

/// Binds `alias` in `session:<space>` to block `block:<space>`, once.
fn bound(b: &B, space: &str, alias: &str) {
    let (block, space) = (format!("block:{space}"), format!("session:{space}"));
    let namespace = StateNamespace::for_space(REX, &space);
    let attribution = MutationAttribution::new(ActorKind::Human, "test", 3);
    StateStore::open(&b.0)
        .unwrap()
        .transaction(|transaction| {
            if transaction.active_pane_bindings(&namespace)?.is_empty() {
                transaction.bind_pane(
                    &namespace,
                    &crate::PaneBindingId::new(format!("binding-{alias}")),
                    &PaneAddressId::new(format!("address-{space}-{alias}")),
                    &block,
                    &attribution,
                )?;
            }
            Ok(())
        })
        .unwrap();
}

/// Gives `@bob` the message `message_id` at `at_ms`: the delivery attempt
/// the dispatcher makes as it puts the message into his pane.
fn give_bob(b: &B, message_id: &str, at_ms: i64) {
    bound(b, "b1", "bob");
    let given = Connection::open(b.0.join("namedrop.sqlite3"))
        .unwrap()
        .execute(
            "INSERT INTO delivery_attempts (session_socket_id, worktree_id, attempt_id,
                delivery_id, address_id, binding_id, agent_session_id, created_at_ms)
             SELECT session_socket_id, worktree_id, 'attempt-' || delivery_id, delivery_id,
                address_id, 'binding-bob', 'session-bob', ?2
             FROM mailbox_deliveries WHERE message_id = ?1",
            params![message_id, at_ms],
        )
        .unwrap();
    assert_eq!(given, 1);
}

/// When the message whose body ends with `text` was made.
fn made_at(b: &B, text: &str) -> i64 {
    b.rows(
        "namedrop.sqlite3",
        &format!("SELECT created_at_ms FROM collaboration_messages WHERE body LIKE '%{text}'"),
    )[0][0]
        .as_i64()
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
        "SELECT message_id FROM collaboration_messages WHERE grammar_version <> 'relay-notice/v1'",
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
        "SELECT message_id, author_address_id FROM collaboration_messages WHERE grammar_version <> 'relay-notice/v1'",
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

/// A capture reads all it needs of the main database in one transaction,
/// before its relay writes, so a relay transaction never waits on the main
/// database.
#[test]
fn a_capture_reads_the_main_database_once_before_its_relay_writes() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "first"));
    let relayed = b.rows(
        "namedrop.sqlite3",
        "SELECT message_id FROM collaboration_messages WHERE grammar_version <> 'relay-notice/v1'",
    );
    bob_answers(&b, relayed[0][0].as_str().unwrap(), "@alice@laptop answer");
    let scope = crate::work_counters::Scope::start();
    super::super::capture::capture(&b.0, &Endpoint::parse(REX), &mut |_| {}).unwrap();
    assert_eq!(scope.finish().writes, 1);
    assert_eq!(
        b.rows("relay.sqlite3", "SELECT re, text FROM outbox"),
        [[json!("m1"), json!("answer")]]
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
        "SELECT message_id FROM collaboration_messages WHERE grammar_version <> 'relay-notice/v1' ORDER BY rowid",
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
    assert_eq!(
        b.messages()[1][8],
        "queued",
        "the terminal runs the item as a new request"
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
        "SELECT message_id FROM collaboration_messages WHERE grammar_version <> 'relay-notice/v1' ORDER BY rowid",
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

/// B keeps one outbox row per B message for a host, so a B message to two
/// relay authors of one A host could keep only one. No reply reaches that:
/// a reply names only the relay author of the message it answers, and one
/// that names another relay author of the thread is refused.
#[test]
fn no_b_reply_reaches_two_relay_authors_of_one_host() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "from alice"));
    let mut dan = deliver("m2", "bob", "t1", "from dan");
    dan["from"]["name"] = json!("dan");
    b.call(&dan);
    let relayed = b.rows(
        "namedrop.sqlite3",
        "SELECT message_id FROM collaboration_messages WHERE grammar_version <> 'relay-notice/v1' ORDER BY rowid",
    );
    let to_dan = relayed[1][0].as_str().unwrap();
    for source in [
        "@alice@laptop @dan@laptop to both",
        "@dan@laptop @alice@laptop to both",
    ] {
        assert_eq!(
            bob_tries(&b, to_dan, source).err().as_deref(),
            Some("unknown Mention: @alice@laptop"),
            "{source}"
        );
    }
    bob_answers(&b, to_dan, "@dan@laptop to dan");
    super::super::capture::capture(&b.0, &Endpoint::parse(REX), &mut |_| {}).unwrap();
    assert_eq!(
        b.rows("relay.sqlite3", "SELECT re, text FROM outbox ORDER BY seq"),
        [[json!("m2"), json!("to dan")]]
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
        "SELECT message_id FROM collaboration_messages WHERE grammar_version <> 'relay-notice/v1'",
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
            None,
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
        "SELECT message_id FROM collaboration_messages WHERE grammar_version <> 'relay-notice/v1'",
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
        "SELECT message_id FROM collaboration_messages WHERE grammar_version <> 'relay-notice/v1'",
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
        "SELECT message_id FROM collaboration_messages WHERE grammar_version <> 'relay-notice/v1'",
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
        "SELECT thread_id FROM collaboration_messages WHERE grammar_version <> 'relay-notice/v1' ORDER BY rowid",
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

/// An accept that cannot take the relay's lock in time answers `retry`,
/// which A sends again, and stores nothing.
#[test]
fn an_accept_that_waits_too_long_for_the_lock_answers_retry() {
    use std::os::unix::fs::OpenOptionsExt;
    let b = B::new();
    fs::create_dir_all(b.0.join("relay")).unwrap();
    let held = fs::OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .mode(0o600)
        .open(b.0.join("relay/accept.lock"))
        .unwrap();
    held.lock().unwrap();
    let started = std::time::Instant::now();
    let answer = b.call(&deliver("m1", "bob", "t1", "hello"));
    let waited = started.elapsed();
    assert_eq!(outcome(&answer).0, Status::Retry);
    assert!(
        waited >= LOCK_WITHIN && waited < LOCK_WITHIN + std::time::Duration::from_secs(2),
        "{waited:?}"
    );
    assert!(b.messages().is_empty());
    assert_eq!(b.inbound(), 0);
    held.unlock().unwrap();
    assert_eq!(
        outcome(&b.call(&deliver("m1", "bob", "t1", "hello"))).0,
        Status::Accepted
    );
}

/// B keeps at most 10,000 replies for one A host. One more is counted for
/// Doctor with the expired replies, as one that can never reach A.
#[test]
fn a_reply_past_10_000_kept_for_a_host_is_counted_not_kept() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "please answer"));
    let relayed = b.rows(
        "namedrop.sqlite3",
        "SELECT message_id FROM collaboration_messages WHERE grammar_version <> 'relay-notice/v1'",
    )[0][0]
        .as_str()
        .unwrap()
        .to_owned();
    let mut relay = Connection::open(b.0.join("relay.sqlite3")).unwrap();
    let transaction = relay.transaction().unwrap();
    for n in 0..10_000 {
        transaction
            .execute(
                "INSERT INTO outbox (host_id, re, thread, sender, message_id, text,
                    created_at_ms) VALUES ('host-a', 'm1', 't1', 'bob', ?1, 'r', ?2)",
                params![format!("b{n}"), super::super::now_ms()],
            )
            .unwrap();
    }
    transaction.commit().unwrap();
    bob_answers(&b, &relayed, "@alice@laptop one too many");
    super::super::capture::capture(&b.0, &Endpoint::parse(REX), &mut |_| {}).unwrap();
    assert_eq!(
        b.rows("relay.sqlite3", "SELECT COUNT(*) FROM outbox"),
        [[json!(10_000)]]
    );
    assert_eq!(
        super::super::expired_replies(&b.0),
        [("alice@laptop".to_owned(), 1)]
    );
}

/// A capture that fails to pass a message and reads it again counts its
/// unroutable reply once.
#[test]
fn a_capture_read_again_counts_an_unroutable_reply_once() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "please answer"));
    let relayed = b.rows(
        "namedrop.sqlite3",
        "SELECT message_id FROM collaboration_messages WHERE grammar_version <> 'relay-notice/v1'",
    )[0][0]
        .as_str()
        .unwrap()
        .to_owned();
    let relay = Connection::open(b.0.join("relay.sqlite3")).unwrap();
    relay
        .execute_batch(
            "DELETE FROM remote_threads;
             CREATE TRIGGER refuse_capture BEFORE INSERT ON capture
             BEGIN SELECT RAISE(ABORT, 'no cursor'); END;",
        )
        .unwrap();
    bob_answers(&b, &relayed, "@alice@laptop too late");
    let endpoint = Endpoint::parse(REX);
    assert!(super::super::capture::capture(&b.0, &endpoint, &mut |_| {}).is_err());
    relay.execute_batch("DROP TRIGGER refuse_capture").unwrap();
    super::super::capture::capture(&b.0, &endpoint, &mut |_| {}).unwrap();
    assert_eq!(
        super::super::expired_replies(&b.0),
        [("alice@laptop".to_owned(), 1)]
    );
}

/// A terminal gets no host notice, and its command runs as the sender
/// wrote it.
#[test]
fn a_terminal_runs_a_self_named_sender_s_command_as_sent() {
    let b = B::new();
    Connection::open(b.0.join("namedrop.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE pane_addresses SET pane_type = 'terminal' WHERE address_id = ?1",
            ["address-session:b1-bob"],
        )
        .unwrap();
    fs::create_dir_all(b.0.join("relay")).unwrap();
    fs::write(b.0.join("relay/allow-terminals"), "").unwrap();
    assert_eq!(
        outcome(&b.call(&deliver("m1", "bob", "t9", "printf relayed"))),
        (Status::Accepted, None)
    );
    let bodies = b.rows(
        "namedrop.sqlite3",
        "SELECT body FROM collaboration_messages",
    );
    assert_eq!(bodies, [[json!("printf relayed")]]);
    let out = std::process::Command::new("/bin/sh")
        .args(["-c", bodies[0][0].as_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert_eq!(out.stdout, b"relayed");
}

/// A retry after B committed the message but not its `inbound` key, or
/// its thread's mapping either, replays the message and its notice,
/// with nothing new.
#[test]
fn a_retry_after_a_partial_commit_replays_the_message_and_its_notice() {
    let b = B::new();
    let item = deliver("m1", "bob", "t1", "hello");
    assert_eq!(outcome(&b.call(&item)), (Status::Accepted, None));
    let stored = || {
        b.rows(
            "namedrop.sqlite3",
            "SELECT message_id, body FROM collaboration_messages ORDER BY rowid",
        )
    };
    let before = stored();
    assert_eq!(before.len(), 2);
    assert_eq!(before[0][1], json!(MARKED));
    assert_eq!(before[1][1], json!("hello"));
    let relay = Connection::open(b.0.join("relay.sqlite3")).unwrap();
    for lose_thread in [false, true] {
        relay.execute("DELETE FROM inbound", []).unwrap();
        if lose_thread {
            relay.execute("DELETE FROM remote_threads", []).unwrap();
        }
        assert_eq!(outcome(&b.call(&item)), (Status::Accepted, None));
        assert_eq!(stored(), before);
        assert_eq!(outcome(&b.call(&item)), (Status::Duplicate, None));
    }
}

/// A capture whose cursor could not be written counts the reply B could
/// not keep once, when it is retried, and a capture whose snapshot is
/// behind the cursor captures nothing again.
#[test]
fn a_capture_retried_after_its_cursor_failed_counts_a_discard_once() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "please answer"));
    let relayed = b.rows(
        "namedrop.sqlite3",
        "SELECT message_id FROM collaboration_messages WHERE grammar_version <> 'relay-notice/v1'",
    )[0][0]
        .as_str()
        .unwrap()
        .to_owned();
    let relay = Connection::open(b.0.join("relay.sqlite3")).unwrap();
    relay
        .execute_batch(
            "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i+1 FROM n WHERE i<10000)
         INSERT INTO outbox(host_id,re,thread,sender,message_id,text,created_at_ms)
         SELECT 'host-a','m1','t1','bob','full-' || i,'x',9999999999999 FROM n;
         CREATE TRIGGER refuse_capture BEFORE INSERT ON capture
         BEGIN SELECT RAISE(ABORT,'no cursor'); END;",
        )
        .unwrap();
    bob_answers(&b, &relayed, "@alice@laptop one too many");
    let endpoint = Endpoint::parse(REX);
    assert!(super::super::capture::capture(&b.0, &endpoint, &mut |_| {}).is_err());
    relay.execute_batch("DROP TRIGGER refuse_capture").unwrap();
    super::super::capture::capture(&b.0, &endpoint, &mut |_| {}).unwrap();
    let counts = b.rows("relay.sqlite3", "SELECT re,discarded FROM outbox_lost");
    assert_eq!(counts, [[json!("m1"), json!(1)]]);
    let store = RelayStore::open(&b.0, crate::relay::now_ms()).unwrap();
    let cursor = store.capture_cursor(&endpoint.key).unwrap();
    store
        .captured(&endpoint.key, cursor, ("message", "session:b1"), || {
            panic!("a stale capture snapshot must not replay a committed message")
        })
        .unwrap();
}

/// An answer into a B thread that B's Automatic Reply Limit paused is not
/// refused for good: B stores nothing and says the thread is paused, or
/// `retry` to a version 1 A, which cannot read that, and takes the answer
/// once a person on B resumes the thread.
#[test]
fn an_answer_into_a_thread_b_s_limit_paused_is_taken_once_the_thread_resumes() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "first"));
    let relayed = b.rows(
        "namedrop.sqlite3",
        "SELECT message_id, thread_id FROM collaboration_messages",
    );
    let (message, thread) = (relayed[0][0].clone(), relayed[0][1].clone());
    let main = Connection::open(b.0.join("namedrop.sqlite3")).unwrap();
    main.execute(
        "INSERT INTO thread_controls (session_socket_id, worktree_id, thread_id, paused, stopped,
            reply_allowance) VALUES (?1, 'session:b1', ?2, 0, 0, 0)
         ON CONFLICT DO UPDATE SET reply_allowance = 0",
        params![REX, thread.as_str().unwrap()],
    )
    .unwrap();
    let mut answer = deliver("m2", "bob", "t1", "answer");
    answer["item"]["in_reply_to"] = message;
    let limit = Some("the Automatic Reply Limit requires human continuation".to_owned());
    assert_eq!(outcome(&b.call(&answer)), (Status::Retry, limit.clone()));
    answer["relay"] = json!(2);
    assert_eq!(outcome(&b.call(&answer)), (Status::Paused, limit));
    assert_eq!(b.inbound(), 1, "nothing is stored for the paused answer");
    assert_eq!(b.messages().len(), 1);
    main.execute("UPDATE thread_controls SET reply_allowance = 10", [])
        .unwrap();
    assert_eq!(outcome(&b.call(&answer)), (Status::Accepted, None));
    assert_eq!(b.messages().len(), 2);
}

/// The id of the message of `deliver`'s item that B stored, past the host
/// notice before it.
fn relayed_id(b: &B, item: &str) -> String {
    b.rows(
        "namedrop.sqlite3",
        &format!(
            "SELECT message_id FROM collaboration_messages
             WHERE grammar_version <> 'relay-notice/v1' AND request_id LIKE '%{item}%'"
        ),
    )[0][0]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Purges `session:<space>`, as a person does with a space they closed.
fn purge(b: &B, space: &str) {
    let space = format!("session:{space}");
    let namespace = StateNamespace::for_space(REX, &space);
    let attribution = MutationAttribution::new(ActorKind::Human, "test", 5);
    let mut main = StateStore::open(&b.0).unwrap();
    main.transaction(|transaction| transaction.deactivate_worktree(&namespace, &attribution))
        .unwrap();
    Connection::open(b.0.join("namedrop.sqlite3"))
        .unwrap()
        .execute(
            "UPDATE mailbox_deliveries SET state = 'settled' WHERE worktree_id = ?1",
            [&space],
        )
        .unwrap();
    main.purge_worktree(&namespace, &space, &attribution)
        .unwrap();
}

fn outbox_texts(b: &B) -> Vec<Vec<Value>> {
    b.rows("relay.sqlite3", "SELECT text FROM outbox ORDER BY seq")
}

/// A purge of the newest messages lets SQLite give their rowids to the next
/// ones, at or below the capture cursor, and even the cursor's own. Replies
/// made after it are still kept for A.
#[test]
fn a_reply_made_after_a_purge_of_the_newest_messages_is_kept() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "please answer"));
    let real = relayed_id(&b, "m1");
    bob_answers(&b, &real, "@alice@laptop first answer");
    b.call(&deliver("m2", "carol", "t2", "to the other space"));
    let endpoint = Endpoint::parse(REX);
    super::super::capture::capture(&b.0, &endpoint, &mut |_| {}).unwrap();
    let max = || {
        b.rows(
            "namedrop.sqlite3",
            "SELECT MAX(rowid) FROM collaboration_messages",
        )[0][0]
            .as_i64()
            .unwrap()
    };
    let cursor = max();
    purge(&b, "b2");
    assert!(max() < cursor - 1, "the purge took the newest rowids");
    for answer in ["second", "third"] {
        std::thread::sleep(std::time::Duration::from_millis(5));
        bob_answers(&b, &real, &format!("@alice@laptop {answer} answer"));
    }
    assert_eq!(max(), cursor, "the last reply took the cursor's own rowid");
    super::super::capture::capture(&b.0, &endpoint, &mut |_| {}).unwrap();
    assert_eq!(
        outbox_texts(&b),
        [
            [json!("first answer")],
            [json!("second answer")],
            [json!("third answer")]
        ]
    );
}

/// Captures take turns: one waits while another holds the capture lock,
/// since a capture may move the cursor back under it.
#[test]
fn a_capture_waits_for_another_to_finish() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "please answer"));
    let lock = super::super::capturing(&b.0).unwrap();
    let (state, (done, finished)) = (b.0.clone(), std::sync::mpsc::channel());
    let capture = std::thread::spawn(move || {
        let captured = super::super::capture::capture(&state, &Endpoint::parse(REX), &mut |_| {});
        let _ = done.send(());
        captured
    });
    assert!(
        finished
            .recv_timeout(std::time::Duration::from_millis(300))
            .is_err(),
        "it waits for the lock"
    );
    drop(lock);
    assert!(
        finished
            .recv_timeout(std::time::Duration::from_secs(10))
            .is_ok()
    );
    capture.join().unwrap().unwrap();
}

/// When the purged space held the last relay message capture read, capture
/// reads again from the last one of a space that is still there, so what
/// A already took is not kept again.
#[test]
fn after_a_purge_capture_reads_again_from_the_last_relay_message_still_there() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "please answer"));
    let to_bob = relayed_id(&b, "m1");
    bob_answers(&b, &to_bob, "@alice@laptop first answer");
    let endpoint = Endpoint::parse(REX);
    let capture = || super::super::capture::capture(&b.0, &endpoint, &mut |_| {}).unwrap();
    let acked = || {
        Connection::open(b.0.join("relay.sqlite3"))
            .unwrap()
            .execute("DELETE FROM outbox", [])
            .unwrap();
    };
    capture();
    acked();
    std::thread::sleep(std::time::Duration::from_millis(5));
    bob_answers(&b, &to_bob, "@alice@laptop second answer");
    capture();
    acked();
    b.call(&deliver("m2", "carol", "t2", "to the other space"));
    let to_carol = relayed_id(&b, "m2");
    tries_as(&b, "b2", "carol", &to_carol, "@alice@laptop carol's answer").unwrap();
    capture();
    assert_eq!(outbox_texts(&b), [[json!("carol's answer")]]);
    acked();
    purge(&b, "b2");
    std::thread::sleep(std::time::Duration::from_millis(5));
    bob_answers(&b, &to_bob, "@alice@laptop third answer");
    capture();
    assert_eq!(
        outbox_texts(&b),
        [[json!("third answer")]],
        "the answers A took are not kept again"
    );
    capture();
    assert_eq!(outbox_texts(&b), [[json!("third answer")]]);
}

/// The id of the host notice B put before the first message of a thread.
fn host_notice(b: &B) -> String {
    b.rows(
        "namedrop.sqlite3",
        "SELECT message_id FROM collaboration_messages
         WHERE grammar_version = 'relay-notice/v1'",
    )[0][0]
        .as_str()
        .unwrap()
        .to_owned()
}

/// What B's outbox pages to A.
fn page_texts(b: &B) -> Vec<String> {
    let mut collect = deliver("m1", "bob", "t1", "x");
    collect["op"] = json!("collect");
    collect.as_object_mut().unwrap().remove("item");
    b.call(&collect)
        .outbox
        .into_iter()
        .map(|reply| reply.text)
        .collect()
}

/// B's agent may answer the notice of a sender's self-named host, which the
/// relay put before the message, rather than the message itself. Once it
/// was given the message, what it says is its answer, so it is kept for A
/// as an answer to that message, and nothing is counted as let go of.
#[test]
fn an_answer_to_the_relay_s_own_notice_answers_the_message_it_told_about() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "what is 2+2?"));
    b.call(&deliver("m2", "bob", "t1", "and 3+3?"));
    let notice = host_notice(&b);
    give_bob(&b, &relayed_id(&b, "m1"), super::super::now_ms() - 1);
    bob_answers(&b, &notice, "@alice@laptop the answer is 4");
    let endpoint = Endpoint::parse(REX);
    super::super::capture::capture(&b.0, &endpoint, &mut |_| {}).unwrap();
    let kept = || b.rows("relay.sqlite3", "SELECT re, text FROM outbox ORDER BY seq");
    assert_eq!(kept(), [[json!("m1"), json!("the answer is 4")]]);
    assert!(super::super::expired_replies(&b.0).is_empty());
    std::thread::sleep(std::time::Duration::from_millis(5));
    bob_answers(&b, &relayed_id(&b, "m1"), "@alice@laptop or four");
    super::super::capture::capture(&b.0, &endpoint, &mut |_| {}).unwrap();
    assert_eq!(
        kept(),
        [
            [json!("m1"), json!("the answer is 4")],
            [json!("m1"), json!("or four")]
        ]
    );
    assert!(super::super::expired_replies(&b.0).is_empty());
}

/// An agent given the host notice as a prompt of its own may acknowledge it
/// before it is given the message the notice told about. That answers
/// nothing A sent, so B neither keeps nor counts it, and A's thread gets
/// only the answer to the message (round B's review, F1, its
/// rvb_notice_acknowledged_then_message_answered and
/// rvb_a_takes_the_acknowledgement_and_the_answer).
#[test]
fn an_acknowledgement_of_the_host_notice_is_neither_kept_nor_counted() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "t1", "what is 2+2?"));
    let endpoint = Endpoint::parse(REX);
    let capture = || super::super::capture::capture(&b.0, &endpoint, &mut |_| {}).unwrap();
    give_bob(&b, &host_notice(&b), super::super::now_ms() - 1);
    bob_answers(
        &b,
        &host_notice(&b),
        "@alice@laptop Noted, thanks for the heads-up.",
    );
    capture();
    assert_eq!(outbox_texts(&b), Vec::<Vec<Value>>::new());
    assert!(super::super::expired_replies(&b.0).is_empty());
    // An attempt made in the reply's own millisecond came after it was
    // written, as far as anyone can tell.
    std::thread::sleep(std::time::Duration::from_millis(5));
    bob_answers(&b, &host_notice(&b), "@alice@laptop Noted again.");
    give_bob(&b, &relayed_id(&b, "m1"), made_at(&b, "Noted again."));
    capture();
    assert_eq!(outbox_texts(&b), Vec::<Vec<Value>>::new());
    assert!(super::super::expired_replies(&b.0).is_empty());
    std::thread::sleep(std::time::Duration::from_millis(5));
    bob_answers(&b, &relayed_id(&b, "m1"), "@alice@laptop 4");
    capture();
    assert_eq!(
        b.rows("relay.sqlite3", "SELECT re, text FROM outbox ORDER BY seq"),
        [[json!("m1"), json!("4")]]
    );
    assert_eq!(page_texts(&b), ["4"]);
    assert!(super::super::expired_replies(&b.0).is_empty());
}

/// Activates `session:<space>` on B with an agent of each alias.
fn space(b: &B, space: &str, aliases: &[&str]) {
    let attribution = MutationAttribution::new(ActorKind::Human, "test", 7);
    StateStore::open(&b.0)
        .unwrap()
        .transaction(|transaction| {
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
            Ok(())
        })
        .unwrap();
}

/// Makes the relay database again, as an upgraded machine makes its first
/// one in the middle of the main database's history, so its floor is the
/// main database's last message.
fn made_again(b: &B) {
    fs::remove_file(b.0.join("relay.sqlite3")).unwrap();
    drop(RelayStore::open(&b.0, crate::relay::now_ms()).unwrap());
    std::thread::sleep(std::time::Duration::from_millis(5));
}

/// Leaves the relay database as `TO_3` leaves a version 2 one: the cursor
/// with no message id, a mark of no space at it, and no floor.
fn as_version_2_left_it(b: &B) {
    Connection::open(b.0.join("relay.sqlite3"))
        .unwrap()
        .execute_batch(
            "UPDATE capture SET message_id = NULL;
             DELETE FROM capture_marks;
             INSERT INTO capture_marks (endpoint, space, rowid, message_id)
                SELECT endpoint, '', rowid, NULL FROM capture;
             DELETE FROM capture_start;",
        )
        .unwrap();
}

fn acked(b: &B) {
    Connection::open(b.0.join("relay.sqlite3"))
        .unwrap()
        .execute("DELETE FROM outbox", [])
        .unwrap();
}

/// A purge that takes the relay database's floor and every message after
/// it lets the next replies take rowids below the floor. Capture reads
/// after the last message made before the floor's, so each is kept.
#[test]
fn replies_made_below_a_purged_floor_are_kept() {
    let b = B::new();
    space(&b, "session:b4", &["dave"]);
    b.call(&deliver("d0", "dave", "td0", "an old question"));
    tries_as(
        &b,
        "b4",
        "dave",
        &relayed_id(&b, "d0"),
        "@alice@laptop an old answer",
    )
    .unwrap();
    for n in 1..=4 {
        b.call(&deliver(&format!("c{n}"), "carol", "tc", "history"));
    }
    made_again(&b);
    let endpoint = Endpoint::parse(REX);
    let capture = || super::super::capture::capture(&b.0, &endpoint, &mut |_| {}).unwrap();
    b.call(&deliver("m1", "bob", "tb", "please answer"));
    bob_answers(&b, &relayed_id(&b, "m1"), "@alice@laptop bob's answer");
    capture();
    purge(&b, "b1");
    purge(&b, "b2");
    b.call(&deliver("d1", "dave", "td", "please answer"));
    let to_dave = relayed_id(&b, "d1");
    for n in 1..=5 {
        std::thread::sleep(std::time::Duration::from_millis(3));
        tries_as(
            &b,
            "b4",
            "dave",
            &to_dave,
            &format!("@alice@laptop dave {n}"),
        )
        .unwrap();
        capture();
    }
    let floor = b.rows("relay.sqlite3", "SELECT rowid FROM capture_start")[0][0]
        .as_i64()
        .unwrap();
    let below: Vec<Vec<Value>> = b.rows(
        "namedrop.sqlite3",
        &format!(
            "SELECT body FROM collaboration_messages WHERE body LIKE 'dave %' AND rowid <= {floor}"
        ),
    );
    assert!(
        below.len() >= 2,
        "replies took rowids below the floor: {below:?}"
    );
    let texts: Vec<Value> = outbox_texts(&b)
        .into_iter()
        .map(|row| row[0].clone())
        .collect();
    assert_eq!(
        texts,
        [
            "bob's answer",
            "dave 1",
            "dave 2",
            "dave 3",
            "dave 4",
            "dave 5"
        ]
        .map(|text| json!(text))
    );
    assert!(super::super::expired_replies(&b.0).is_empty());
}

/// An endpoint whose first capture comes after its floor's message is gone
/// reads below the floor too.
#[test]
fn a_first_capture_after_a_purge_of_the_floor_reads_below_it() {
    let b = B::new();
    for n in 1..=4 {
        b.call(&deliver(&format!("c{n}"), "carol", "tc", "history"));
    }
    made_again(&b);
    purge(&b, "b2");
    b.call(&deliver("m1", "bob", "tb", "please answer"));
    let to_bob = relayed_id(&b, "m1");
    for answer in ["first", "second"] {
        std::thread::sleep(std::time::Duration::from_millis(3));
        bob_answers(&b, &to_bob, &format!("@alice@laptop {answer}"));
    }
    super::super::capture::capture(&b.0, &Endpoint::parse(REX), &mut |_| {}).unwrap();
    assert_eq!(outbox_texts(&b), [[json!("first")], [json!("second")]]);
}

/// A space purged and then used again keeps the mark of its new messages,
/// which take rowids below its old mark, so a later capture that reads
/// again starts after them and keeps none of them again.
#[test]
fn a_space_used_again_after_a_purge_marks_its_new_messages() {
    let b = B::new();
    let endpoint = Endpoint::parse(REX);
    let capture = || super::super::capture::capture(&b.0, &endpoint, &mut |_| {}).unwrap();
    b.call(&deliver("m1", "carol", "tc", "please answer"));
    let to_carol = relayed_id(&b, "m1");
    tries_as(&b, "b2", "carol", &to_carol, "@alice@laptop carol 1").unwrap();
    capture();
    b.call(&deliver("m2", "bob", "tb", "please answer"));
    bob_answers(&b, &relayed_id(&b, "m2"), "@alice@laptop bob 1");
    capture();
    acked(&b);
    purge(&b, "b1");
    space(&b, "session:b1", &["bob"]);
    b.call(&deliver("m3", "bob", "tb3", "please answer"));
    let to_bob = relayed_id(&b, "m3");
    std::thread::sleep(std::time::Duration::from_millis(3));
    bob_answers(&b, &to_bob, "@alice@laptop bob 2");
    capture();
    std::thread::sleep(std::time::Duration::from_millis(3));
    tries_as(&b, "b2", "carol", &to_carol, "@alice@laptop carol 2").unwrap();
    capture();
    assert_eq!(outbox_texts(&b), [[json!("bob 2")], [json!("carol 2")]]);
    acked(&b);
    purge(&b, "b2");
    std::thread::sleep(std::time::Duration::from_millis(3));
    bob_answers(&b, &to_bob, "@alice@laptop bob 3");
    capture();
    assert_eq!(outbox_texts(&b), [[json!("bob 3")]]);
}

/// A version 3 database's floor has no message id: the first capture gives
/// it the id and creation time of the message at its rowid, so a later
/// purge of that message lets capture read below it.
#[test]
fn a_version_3_floor_takes_its_message_s_id_at_the_first_capture() {
    let b = B::new();
    for n in 1..=4 {
        b.call(&deliver(&format!("c{n}"), "carol", "tc", "history"));
    }
    made_again(&b);
    Connection::open(b.0.join("relay.sqlite3"))
        .unwrap()
        .execute_batch("UPDATE capture_start SET message_id = NULL, created_at_ms = NULL")
        .unwrap();
    let endpoint = Endpoint::parse(REX);
    let capture = || super::super::capture::capture(&b.0, &endpoint, &mut |_| {}).unwrap();
    capture();
    assert_eq!(
        b.rows(
            "relay.sqlite3",
            "SELECT message_id IS NOT NULL, created_at_ms IS NOT NULL FROM capture_start"
        ),
        [[json!(1), json!(1)]]
    );
    purge(&b, "b2");
    b.call(&deliver("m1", "bob", "tb", "please answer"));
    let to_bob = relayed_id(&b, "m1");
    for answer in ["first", "second"] {
        std::thread::sleep(std::time::Duration::from_millis(3));
        bob_answers(&b, &to_bob, &format!("@alice@laptop {answer}"));
        capture();
    }
    assert_eq!(outbox_texts(&b), [[json!("first")], [json!("second")]]);
}

/// A capture that reads again stops at the floor while its message is
/// there: the history before the relay database was made is not read
/// again, so a reply in it is neither kept nor counted.
#[test]
fn a_capture_that_reads_again_stops_at_the_floor() {
    let b = B::new();
    b.call(&deliver("m1", "bob", "tb", "please answer"));
    bob_answers(&b, &relayed_id(&b, "m1"), "@alice@laptop from before");
    made_again(&b);
    let endpoint = Endpoint::parse(REX);
    let capture = || super::super::capture::capture(&b.0, &endpoint, &mut |_| {}).unwrap();
    b.call(&deliver("m2", "carol", "tc", "please answer"));
    tries_as(
        &b,
        "b2",
        "carol",
        &relayed_id(&b, "m2"),
        "@alice@laptop carol's answer",
    )
    .unwrap();
    capture();
    assert_eq!(outbox_texts(&b), [[json!("carol's answer")]]);
    purge(&b, "b2");
    capture();
    assert_eq!(outbox_texts(&b), [[json!("carol's answer")]]);
    assert!(super::super::expired_replies(&b.0).is_empty());
}

/// A version 2 cursor is the last relay message that build read, a floor
/// of its endpoint once the first capture gives it its message's id. A
/// capture that reads again stops at it, so what A took before is not
/// kept again.
#[test]
fn a_capture_that_reads_again_stops_at_a_version_2_cursor() {
    let b = B::new();
    let endpoint = Endpoint::parse(REX);
    let capture = || super::super::capture::capture(&b.0, &endpoint, &mut |_| {}).unwrap();
    b.call(&deliver("m1", "bob", "tb", "please answer"));
    b.call(&deliver("m2", "carol", "tc", "please answer"));
    let to_carol = relayed_id(&b, "m2");
    tries_as(&b, "b2", "carol", &to_carol, "@alice@laptop carol first").unwrap();
    capture();
    acked(&b);
    as_version_2_left_it(&b);
    capture();
    assert_eq!(
        b.rows(
            "relay.sqlite3",
            "SELECT space, message_id IS NOT NULL FROM capture_marks"
        ),
        [[json!(""), json!(1)]],
        "the first capture gives the mark its message's id"
    );
    assert_eq!(
        b.rows(
            "relay.sqlite3",
            "SELECT message_id IS NOT NULL FROM capture"
        ),
        [[json!(1)]],
        "and the cursor, which reads after it, the same"
    );
    std::thread::sleep(std::time::Duration::from_millis(3));
    bob_answers(&b, &relayed_id(&b, "m1"), "@alice@laptop bob first");
    capture();
    acked(&b);
    purge(&b, "b1");
    std::thread::sleep(std::time::Duration::from_millis(3));
    tries_as(&b, "b2", "carol", &to_carol, "@alice@laptop carol second").unwrap();
    capture();
    assert_eq!(outbox_texts(&b), [[json!("carol second")]]);
}

/// When a purge takes the message of a version 2 cursor too, the replies
/// that take rowids below it are kept, as below the database's floor.
#[test]
fn replies_made_below_a_purged_version_2_cursor_are_kept() {
    let b = B::new();
    let endpoint = Endpoint::parse(REX);
    let capture = || super::super::capture::capture(&b.0, &endpoint, &mut |_| {}).unwrap();
    b.call(&deliver("m1", "bob", "tb", "please answer"));
    bob_answers(&b, &relayed_id(&b, "m1"), "@alice@laptop bob before");
    b.call(&deliver("m2", "carol", "tc", "please answer"));
    tries_as(
        &b,
        "b2",
        "carol",
        &relayed_id(&b, "m2"),
        "@alice@laptop carol first",
    )
    .unwrap();
    capture();
    acked(&b);
    as_version_2_left_it(&b);
    capture();
    purge(&b, "b2");
    std::thread::sleep(std::time::Duration::from_millis(3));
    let to_bob = relayed_id(&b, "m1");
    for n in 1..=4 {
        std::thread::sleep(std::time::Duration::from_millis(3));
        bob_answers(&b, &to_bob, &format!("@alice@laptop bob {n}"));
        capture();
    }
    let texts: Vec<Value> = outbox_texts(&b)
        .into_iter()
        .map(|row| row[0].clone())
        .collect();
    assert_eq!(
        texts,
        ["bob 1", "bob 2", "bob 3", "bob 4"].map(|text| json!(text))
    );
    assert!(super::super::expired_replies(&b.0).is_empty());
}
