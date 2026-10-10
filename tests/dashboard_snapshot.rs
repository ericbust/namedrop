mod support;

use namedrop::dashboard::snapshot::{
    Activity, DashboardSnapshot, LegStage, Participant, SpaceHealth, SpaceKey, SpaceState, build,
};
use namedrop::{
    ActorKind, AgentSessionId, ContextGrantDraft, ContextGrantId, ContextSnapshotId, DeliveryId,
    DeliveryState, LeaseDraft, MessageDraft, MessageId, MessageRecipientDraft, MutationAttribution,
    PaneAddressId, PaneBindingId, StateNamespace, StateStore, ThreadId,
};
use support::{TestDirectory, address};

fn attribution() -> MutationAttribution {
    MutationAttribution::new(ActorKind::Plugin, "test", 10)
}

/// Activates `space` with bound, occupied agents named by `aliases` and one thread.
fn seed_space(store: &mut StateStore, space: &str, aliases: &[&str]) -> StateNamespace {
    let namespace = StateNamespace::for_space("socket-a", space);
    store
        .transaction(|tx| {
            tx.activate_worktree(&namespace, &attribution())?;
            for alias in aliases {
                let id = format!("address-{space}-{alias}");
                tx.create_pane_address(&namespace, &address(&id, alias), &attribution())?;
                tx.bind_pane(
                    &namespace,
                    &PaneBindingId::new(format!("binding-{space}-{alias}")),
                    &PaneAddressId::new(&id),
                    &format!("pane-{space}-{alias}"),
                    &attribution(),
                )?;
                tx.observe_agent_session(
                    &namespace,
                    &format!("observation-{space}-{alias}"),
                    &PaneAddressId::new(&id),
                    &AgentSessionId::new(format!("session-{space}-{alias}")),
                    "idle",
                    &attribution(),
                )?;
            }
            tx.create_thread(
                &namespace,
                &ThreadId::new(format!("thread-{space}")),
                "Token rotation",
                &attribution(),
            )
        })
        .unwrap();
    namespace
}

fn id(namespace: &StateNamespace, alias: &str) -> PaneAddressId {
    PaneAddressId::new(format!("address-{}-{alias}", namespace.space_id.as_str()))
}

fn send(
    store: &mut StateStore,
    namespace: &StateNamespace,
    message: &str,
    from: &str,
    to: &str,
    body: &str,
    in_reply_to: Option<&str>,
) {
    let space = namespace.space_id.as_str();
    store
        .transaction(|tx| {
            tx.append_message(
                namespace,
                &MessageDraft {
                    thread_id: ThreadId::new(format!("thread-{space}")),
                    message_id: MessageId::new(message),
                    in_reply_to: in_reply_to.map(MessageId::new),
                    author_address_id: id(namespace, from),
                    author_kind: ActorKind::AgentSession,
                    author_alias_snapshot: from.to_owned(),
                    author_agent_session_snapshot: Some(AgentSessionId::new(format!(
                        "session-{space}-{from}"
                    ))),
                    request_id: format!("request-{message}"),
                    authored_source: format!("@{to} {body}"),
                    body: body.to_owned(),
                    grammar_version: "mention/v1".to_owned(),
                    recipients: vec![MessageRecipientDraft {
                        address_id: id(namespace, to),
                        alias_snapshot: to.to_owned(),
                        agent_session_snapshot: Some(AgentSessionId::new(format!(
                            "session-{space}-{to}"
                        ))),
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

/// Leases `to`'s mailbox head and submits it into the agent's session.
fn submit(
    store: &mut StateStore,
    namespace: &StateNamespace,
    to: &str,
) -> namedrop::DeliveryAttemptId {
    let space = namespace.space_id.as_str();
    store
        .transaction(|tx| {
            let leased = tx
                .lease_mailbox_head(
                    namespace,
                    &id(namespace, to),
                    &LeaseDraft {
                        attempt_id: namedrop::DeliveryAttemptId::new(format!(
                            "attempt-{space}-{to}"
                        )),
                        binding_id: PaneBindingId::new(format!("binding-{space}-{to}")),
                        agent_session_id: AgentSessionId::new(format!("session-{space}-{to}")),
                        retry_of_attempt_id: None,
                        owner: "test".to_owned(),
                        acquired_at_ms: 10,
                        expires_at_ms: 100_000,
                    },
                    &attribution(),
                )?
                .expect("a queued delivery to lease");
            tx.transition_delivery_attempt(
                namespace,
                &leased.attempt_id,
                DeliveryState::Submitted,
                &attribution(),
            )?;
            Ok(leased.attempt_id)
        })
        .unwrap()
}

fn snapshot(store: &mut StateStore, namespace: &StateNamespace) -> DashboardSnapshot {
    build(
        &store.dashboard_rows(namespace).unwrap(),
        Some("namedrop/dashboard"),
        1,
    )
}

fn lane<'a>(snapshot: &'a DashboardSnapshot, alias: &str) -> &'a Participant {
    snapshot
        .participants
        .iter()
        .find(|p| p.alias == alias)
        .unwrap_or_else(|| panic!("no lane for {alias}"))
}

#[test]
fn a_request_moves_from_queued_to_working_to_replied() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    send(&mut store, &ns, "m1", "a", "b", "Review the diff", None);

    let queued = snapshot(&mut store, &ns);
    assert_eq!(queued.messages[0].legs[0].stage, LegStage::Queued);
    assert_eq!(lane(&queued, "@b").mailbox.len(), 1);
    assert_eq!(lane(&queued, "@b").mailbox[0].from, "@a");
    assert_eq!(lane(&queued, "@a").waiting_on, ["@b"]);
    assert_eq!(lane(&queued, "@a").activity, Activity::Waiting);
    assert_eq!(queued.health, SpaceHealth::Busy { in_flight: 1 });

    let attempt = submit(&mut store, &ns, "b");
    store
        .transaction(|tx| {
            tx.acknowledge_delivery_attempt(
                &ns,
                &attempt,
                "ack-1",
                &AgentSessionId::new("session-w1-b"),
                &attribution(),
            )
        })
        .unwrap();
    let working = snapshot(&mut store, &ns);
    assert!(matches!(
        working.messages[0].legs[0].stage,
        LegStage::Working { .. }
    ));
    assert_eq!(lane(&working, "@b").activity, Activity::Working);

    send(&mut store, &ns, "m2", "b", "a", "Two issues", Some("m1"));
    store
        .transaction(|tx| {
            tx.record_reply_disposition(
                &ns,
                &attempt,
                &MessageId::new("m2"),
                &AgentSessionId::new("session-w1-b"),
                &attribution(),
            )
        })
        .unwrap();
    let replied = snapshot(&mut store, &ns);
    assert_eq!(
        replied.messages[0].legs[0].stage,
        LegStage::Replied {
            reply_id: "m2".to_owned()
        }
    );
    assert!(lane(&replied, "@b").mailbox.is_empty());
    assert!(lane(&replied, "@a").waiting_on.is_empty());
    assert_eq!(
        replied
            .messages
            .iter()
            .map(|m| m.message_id.as_str())
            .collect::<Vec<_>>(),
        ["m1", "m2"],
        "messages run oldest first"
    );
}

#[test]
fn a_snapshot_shows_only_its_own_space() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let w1 = seed_space(&mut store, "w1", &["a", "b"]);
    let w2 = seed_space(&mut store, "w2", &["c", "d"]);
    send(&mut store, &w1, "m-w1", "a", "b", "first space body", None);
    send(&mut store, &w2, "m-w2", "c", "d", "OTHER-SPACE-BODY", None);

    let json = serde_json::to_string(&snapshot(&mut store, &w1)).unwrap();

    assert!(json.contains("m-w1"));
    for leaked in ["m-w2", "OTHER-SPACE-BODY", "@c", "@d", "address-w2"] {
        assert!(!json.contains(leaked), "{leaked} leaked into w1: {json}");
    }
}

#[test]
fn context_payloads_and_secrets_never_reach_the_wire() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    send(
        &mut store,
        &ns,
        "m1",
        "a",
        "b",
        "\x1b[31mred\x1b[0m <script>alert(1)</script> key ghp_abcdefghijklmnopqrstuvwxyz0123",
        None,
    );
    store
        .transaction(|tx| {
            tx.create_context_grant(
                &ns,
                &ContextGrantDraft {
                    id: ContextGrantId::new("grant-1"),
                    thread_id: ThreadId::new("thread-w1"),
                    source_address_id: id(&ns, "a"),
                    grantee_address_id: id(&ns, "b"),
                    pane_binding_id: PaneBindingId::new("binding-w1-a"),
                    agent_session_id: AgentSessionId::new("session-w1-a"),
                },
                &attribution(),
            )?;
            tx.activate_context_grant(
                &ns,
                &ContextGrantId::new("grant-1"),
                &id(&ns, "a"),
                &AgentSessionId::new("session-w1-a"),
                &ContextSnapshotId::new("snapshot-1"),
                &serde_json::json!({"notes": "SECRET-PAYLOAD-123"}),
                &[],
                &attribution(),
            )
            .map(|_| ())
        })
        .unwrap();

    let snapshot = snapshot(&mut store, &ns);
    let json = serde_json::to_string(&snapshot).unwrap();

    assert!(!json.contains("SECRET-PAYLOAD-123"));
    assert!(!json.contains("ghp_abcdefghijklmnopqrstuvwxyz0123"));
    let body = &snapshot.messages[0].body.text;
    assert!(
        !body.contains('\x1b'),
        "control characters are stripped: {body:?}"
    );
    assert!(
        body.contains("<script>alert(1)</script>"),
        "markup stays literal text; the browser renders it with textContent"
    );
    assert!(body.contains("[REDACTED TOKEN]"));
}

#[test]
fn a_held_delivery_needs_the_human_and_blocks_its_recipient() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    send(&mut store, &ns, "m1", "a", "b", "Run the suite", None);
    let attempt = submit(&mut store, &ns, "b");
    store
        .transaction(|tx| tx.hold_delivery_attempt(&ns, &attempt, &attribution()))
        .unwrap();

    let held = snapshot(&mut store, &ns);

    assert_eq!(held.messages[0].legs[0].stage, LegStage::Held);
    assert_eq!(held.attention.len(), 1);
    assert_eq!(held.health, SpaceHealth::NeedsYou { items: 1 });
    assert_eq!(lane(&held, "@b").activity, Activity::Blocked);
    assert_eq!(lane(&held, "@b").mailbox[0].state, DeliveryState::Held);
}

#[test]
fn a_space_without_activation_reads_as_not_activated() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = StateNamespace::for_space("socket-a", "never-activated");

    let snapshot = snapshot(&mut store, &ns);

    assert_eq!(snapshot.space.state, SpaceState::NotActivated);
    assert_eq!(snapshot.health, SpaceHealth::Inactive);
    assert!(snapshot.participants.is_empty());
}

#[test]
fn the_wire_json_matches_the_frozen_contract() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    send(&mut store, &ns, "m1", "a", "b", "Review", None);

    let json = serde_json::to_value(snapshot(&mut store, &ns)).unwrap();

    assert_eq!(json["wire"], 1);
    assert_eq!(json["space"]["key"], SpaceKey::of(&ns).as_str());
    assert_eq!(json["space"]["workspace_id"], "w1");
    assert_eq!(
        json["space"]["label"],
        serde_json::json!({"text": "namedrop/dashboard", "truncated": false})
    );
    assert_eq!(json["space"]["state"], "active");
    assert_eq!(
        json["health"],
        serde_json::json!({"level": "busy", "in_flight": 1})
    );
    let b = &json["participants"][1];
    assert_eq!(b["alias"], "@b");
    assert_eq!(
        b["kind"],
        serde_json::json!({"type": "agent", "runtime": null})
    );
    assert_eq!(b["activity"], "idle");
    assert_eq!(b["mailbox"][0]["state"], "queued");
    assert_eq!(b["mailbox"][0]["from"], "@a");
    let message = &json["messages"][0];
    assert_eq!(message["author"], "address-w1-a");
    assert_eq!(message["kind"], "mention");
    assert_eq!(
        message["legs"][0],
        serde_json::json!({"to": "address-w1-b", "stage": {"stage": "queued"}})
    );
    assert_eq!(
        json["threads"][0],
        serde_json::json!({
            "thread_id": "thread-w1",
            "title": {"text": "Token rotation", "truncated": false},
            "message_count": 1
        })
    );
}

#[test]
fn space_keys_are_twenty_hex_chars_and_parse_only_that_shape() {
    let key = SpaceKey::of(&StateNamespace::for_space("/run/herdr.sock", "wN"));
    assert_eq!(key.as_str().len(), 20);
    assert_eq!(SpaceKey::parse(key.as_str()), Some(key.clone()));
    for bad in [
        "",
        "../etc/passwd",
        "ABCDEF0123456789abcd",
        "0123456789abcdef012",
    ] {
        assert_eq!(SpaceKey::parse(bad), None, "{bad:?}");
    }
}
