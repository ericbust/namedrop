mod support;

#[path = "support/xws.rs"]
mod xws;

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

fn seed_space(store: &mut StateStore, space: &str, aliases: &[&str]) -> StateNamespace {
    seed_space_on_socket(store, "socket-a", space, aliases)
}

fn seed_space_on_socket(
    store: &mut StateStore,
    socket: &str,
    space: &str,
    aliases: &[&str],
) -> StateNamespace {
    let namespace = StateNamespace::for_space(socket, space);
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
    send_as(
        store,
        namespace,
        "mention/v1",
        message,
        from,
        to,
        body,
        in_reply_to,
    );
}

#[allow(clippy::too_many_arguments)]
fn send_as(
    store: &mut StateStore,
    namespace: &StateNamespace,
    grammar_version: &str,
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
                    grammar_version: grammar_version.to_owned(),
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

fn lease(
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
            Ok(leased.attempt_id)
        })
        .unwrap()
}

fn submit(
    store: &mut StateStore,
    namespace: &StateNamespace,
    to: &str,
) -> namedrop::DeliveryAttemptId {
    let attempt = lease(store, namespace, to);
    store
        .transaction(|tx| {
            tx.transition_delivery_attempt(
                namespace,
                &attempt,
                DeliveryState::Submitted,
                &attribution(),
            )
        })
        .unwrap();
    attempt
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
    assert_eq!(
        queued.health,
        SpaceHealth::Busy {
            in_flight: 1,
            working: 0
        }
    );

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
        "\x1b[31mred\x1b[0m <script>alert(1)</script> key ghp_abcdefghijklmnopqrstuvwxyz0123 \u{202E}txt.exe\u{200B}\u{E0041}",
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
    for invisible in ['\u{202E}', '\u{200B}', '\u{E0041}'] {
        assert!(
            !body.contains(invisible),
            "bidi overrides, zero-width and tag characters are stripped: {body:?}"
        );
    }
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
    assert_eq!(held.attention[0].subject_alias.as_deref(), Some("b"));
    assert_eq!(held.attention[0].message_id.as_deref(), Some("m1"));
    assert_eq!(held.health, SpaceHealth::NeedsYou { items: 1 });
    assert_eq!(lane(&held, "@b").activity, Activity::Blocked);
    assert_eq!(lane(&held, "@b").mailbox[0].state, DeliveryState::Held);
}

fn observe_status(
    store: &mut StateStore,
    namespace: &StateNamespace,
    alias: &str,
    status: &str,
    observed_at_ms: u64,
) {
    store
        .transaction(|tx| {
            tx.observe_agent_session(
                namespace,
                &format!("observation-{alias}-{status}-{observed_at_ms}"),
                &id(namespace, alias),
                &AgentSessionId::new(format!("session-{}-{alias}", namespace.space_id.as_str())),
                status,
                &MutationAttribution::new(ActorKind::Plugin, "test", observed_at_ms),
            )
        })
        .unwrap();
}

struct ReconcileIds(std::cell::Cell<u32>);

impl namedrop::IdGenerator for ReconcileIds {
    fn next_id(&self, kind: &str) -> String {
        self.0.set(self.0.get() + 1);
        format!("{kind}-{}", self.0.get())
    }
}

fn discovered_pane(
    namespace: &StateNamespace,
    alias: &str,
    status: &str,
) -> namedrop::DiscoveredPane {
    let space = namespace.space_id.as_str();
    namedrop::DiscoveredPane {
        pane_id: format!("pane-{space}-{alias}"),
        terminal_id: format!("term-{alias}"),
        tab_id: "tab".into(),
        workspace_id: space.into(),
        cwd: None,
        pane_label: None,
        agent_session_id: Some(AgentSessionId::new(format!("session-{space}-{alias}"))),
        agent: Some("claude".into()),
        occupant_label: None,
        agent_status: Some(status.into()),
        idle_state_label: None,
    }
}

#[test]
fn another_panes_status_event_preserves_the_blocked_agents_frame() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    let ids = ReconcileIds(std::cell::Cell::new(0));
    let mut first = None;
    for (at, status) in [(1_000, "working"), (2_000, "idle"), (3_000, "working")] {
        store
            .transaction(|tx| {
                namedrop::reconcile_discovered_panes(
                    tx,
                    &ns,
                    &[
                        discovered_pane(&ns, "a", "blocked"),
                        discovered_pane(&ns, "b", status),
                    ],
                    &ids,
                    &MutationAttribution::new(ActorKind::Plugin, "test", at),
                )
            })
            .unwrap();
        let frame = build(&store.dashboard_rows(&ns).unwrap(), None, 0);
        if at == 1_000 {
            assert_eq!(frame.attention[0].opened_at_ms, 1_000);
            first = Some(serde_json::to_value(&frame).unwrap());
        } else if at == 3_000 {
            assert_eq!(
                &serde_json::to_value(&frame).unwrap(),
                first.as_ref().unwrap(),
                "an uninterrupted permission dialog contributes the same hub frame"
            );
        }
    }
}

#[test]
fn permission_blocking_keeps_its_run_start_and_restarts_after_clearing() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a"]);
    for at in [20, 30, 40] {
        observe_status(&mut store, &ns, "a", "blocked", at);
        assert_eq!(snapshot(&mut store, &ns).attention[0].opened_at_ms, 20);
    }
    observe_status(&mut store, &ns, "a", "working", 50);
    assert!(snapshot(&mut store, &ns).attention.is_empty());
    for at in [60, 70] {
        observe_status(&mut store, &ns, "a", "blocked", at);
        assert_eq!(snapshot(&mut store, &ns).attention[0].opened_at_ms, 60);
    }
}

#[test]
fn permission_blocking_keeps_its_run_start_with_ten_thousand_observations() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let aliases: Vec<_> = (0..20).map(|index| format!("agent-{index}")).collect();
    let alias_refs: Vec<_> = aliases.iter().map(String::as_str).collect();
    let ns = seed_space(&mut store, "w1", &alias_refs);
    store
        .transaction(|tx| {
            for round in 0..500 {
                let status = if (100..200).contains(&round) || round >= 400 {
                    "blocked"
                } else {
                    "idle"
                };
                for alias in &aliases {
                    tx.observe_agent_session(
                        &ns,
                        &format!("history-{alias}-{round}"),
                        &id(&ns, alias),
                        &AgentSessionId::new(format!("session-w1-{alias}")),
                        status,
                        &MutationAttribution::new(ActorKind::Plugin, "test", 100 + round),
                    )?;
                }
            }
            Ok(())
        })
        .unwrap();

    let rows = store.dashboard_rows(&ns).unwrap();
    assert_eq!(rows.addresses.len(), aliases.len());
    for address in &rows.addresses {
        assert_eq!(address.agent_status.as_deref(), Some("blocked"));
        assert_eq!(address.blocked_since_ms, Some(500));
    }
    let current = build(&rows, None, 0);
    assert_eq!(current.attention.len(), aliases.len());
    let json = serde_json::to_value(&current).unwrap();
    for attention in json["attention"].as_array().unwrap() {
        assert_eq!(attention["source_kind"], "agent_status");
        assert_eq!(attention["opened_at_ms"], 500);
    }
}

#[test]
fn permission_blocking_starts_with_the_current_binding() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a"]);
    observe_status(&mut store, &ns, "a", "blocked", 20);
    store
        .transaction(|tx| {
            tx.unbind_pane(
                &ns,
                &id(&ns, "a"),
                &MutationAttribution::new(ActorKind::Plugin, "test", 30),
            )?;
            tx.bind_pane(
                &ns,
                &PaneBindingId::new("binding-replacement"),
                &id(&ns, "a"),
                "pane-w1-a",
                &MutationAttribution::new(ActorKind::Plugin, "test", 40),
            )
        })
        .unwrap();
    let rebound = snapshot(&mut store, &ns);
    assert_eq!(lane(&rebound, "@a").activity, Activity::Offline);
    assert!(rebound.attention.is_empty());
    for at in [40, 50] {
        observe_status(&mut store, &ns, "a", "blocked", at);
        assert_eq!(snapshot(&mut store, &ns).attention[0].opened_at_ms, 40);
    }
}

#[test]
fn permission_blocking_orders_equal_timestamp_observations_by_insertion() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a"]);
    for (index, (at, status, expected_since)) in [
        (20, "blocked", Some(20)),
        (20, "idle", None),
        (20, "blocked", Some(20)),
        (30, "blocked", Some(20)),
        (30, "idle", None),
        (40, "blocked", Some(40)),
    ]
    .into_iter()
    .enumerate()
    {
        store
            .transaction(|tx| {
                tx.observe_agent_session(
                    &ns,
                    &format!("tied-observation-{index}"),
                    &id(&ns, "a"),
                    &AgentSessionId::new("session-w1-a"),
                    status,
                    &MutationAttribution::new(ActorKind::Plugin, "test", at),
                )
            })
            .unwrap();
        let current = snapshot(&mut store, &ns);
        if let Some(since) = expected_since {
            assert_eq!(current.attention[0].opened_at_ms, since);
        } else {
            assert!(current.attention.is_empty());
            assert_eq!(lane(&current, "@a").activity, Activity::Idle);
        }
    }
}

#[test]
fn retiring_a_permission_blocked_agent_clears_its_attention() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    send(&mut store, &ns, "m1", "a", "b", "Review the diff", None);
    observe_status(&mut store, &ns, "a", "blocked", 20);
    assert_eq!(snapshot(&mut store, &ns).attention.len(), 1);
    store
        .transaction(|tx| {
            tx.retire_pane_address(
                &ns,
                &id(&ns, "a"),
                &MutationAttribution::new(ActorKind::Plugin, "test", 30),
            )
        })
        .unwrap();
    let retired = snapshot(&mut store, &ns);
    assert_eq!(lane(&retired, "@a").activity, Activity::Retired);
    assert!(retired.attention.is_empty());
    assert!(!matches!(retired.health, SpaceHealth::NeedsYou { .. }));
}

#[test]
fn a_permission_blocked_agent_needs_the_human_until_its_status_changes() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a"]);
    assert_eq!(snapshot(&mut store, &ns).health, SpaceHealth::Calm);

    observe_status(&mut store, &ns, "a", "blocked", 20);
    let blocked = snapshot(&mut store, &ns);
    assert_eq!(lane(&blocked, "@a").activity, Activity::Blocked);
    assert_eq!(blocked.health, SpaceHealth::NeedsYou { items: 1 });
    assert_eq!(blocked.attention.len(), 1);
    assert_eq!(blocked.attention[0].opened_at_ms, 20);
    assert_eq!(
        blocked.attention[0].summary.text,
        "@a is waiting on an approval or question."
    );
    let json = serde_json::to_value(&blocked).unwrap();
    assert_eq!(json["wire"], 1);
    assert_eq!(json["attention"][0]["source_kind"], "agent_status");
    assert_eq!(json["attention"][0]["opened_at_ms"], 20);
    assert_eq!(json["attention"][0]["subject_alias"], "a");
    assert_eq!(
        json["attention"][0]["subject_summary"],
        serde_json::json!({
            "text": "Waiting on an approval or question.",
            "truncated": false
        })
    );
    assert_eq!(
        json["attention"][0].get("message_id"),
        Some(&serde_json::Value::Null)
    );
    let rows = store.dashboard_rows(&ns).unwrap();
    for now_ms in [0, 2_000, 3_000] {
        let rebuilt = build(&rows, None, now_ms);
        assert_eq!(rebuilt.attention[0].opened_at_ms, 20);
        assert_eq!(
            serde_json::to_value(&rebuilt.attention).unwrap(),
            json["attention"],
            "status attention stays stable across frame times, including the hub's zero clock"
        );
    }
    assert!(
        store.dashboard_rows(&ns).unwrap().attention.is_empty(),
        "status attention is not persisted"
    );

    observe_status(&mut store, &ns, "a", "idle", 30);
    let idle = snapshot(&mut store, &ns);
    assert_eq!(lane(&idle, "@a").activity, Activity::Idle);
    assert_eq!(idle.health, SpaceHealth::Calm);
    assert!(idle.attention.is_empty());
}

#[test]
fn permission_blocked_agents_add_to_existing_attention_and_clear_individually() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b", "c"]);
    send(&mut store, &ns, "m1", "a", "c", "Run the suite", None);
    let attempt = submit(&mut store, &ns, "c");
    store
        .transaction(|tx| tx.hold_delivery_attempt(&ns, &attempt, &attribution()))
        .unwrap();
    let persisted = serde_json::to_value(&snapshot(&mut store, &ns).attention).unwrap();
    observe_status(&mut store, &ns, "a", "blocked", 20);
    observe_status(&mut store, &ns, "b", "blocked", 20);

    let blocked = snapshot(&mut store, &ns);
    assert_eq!(blocked.health, SpaceHealth::NeedsYou { items: 3 });
    assert_eq!(blocked.attention.len(), 3);
    let json = serde_json::to_value(&blocked).unwrap();
    assert_eq!(
        json["attention"][0], persisted[0],
        "stored attention keeps its wire shape"
    );
    assert_eq!(json["attention"][1]["source_kind"], "agent_status");
    assert_eq!(json["attention"][2]["source_kind"], "agent_status");
    assert_eq!(
        blocked.attention[1].summary.text,
        "@a is waiting on an approval or question."
    );
    assert_eq!(
        blocked.attention[2].summary.text,
        "@b is waiting on an approval or question."
    );

    observe_status(&mut store, &ns, "a", "idle", 30);
    let remaining = snapshot(&mut store, &ns);
    assert_eq!(remaining.health, SpaceHealth::NeedsYou { items: 2 });
    assert_eq!(remaining.attention.len(), 2);
    assert_eq!(
        remaining.attention[1].summary.text,
        "@b is waiting on an approval or question."
    );

    observe_status(&mut store, &ns, "b", "idle", 30);
    let cleared = snapshot(&mut store, &ns);
    assert_eq!(cleared.health, SpaceHealth::NeedsYou { items: 1 });
    assert_eq!(serde_json::to_value(&cleared.attention).unwrap(), persisted);
}

#[test]
fn permission_blocking_keeps_retired_offline_and_terminal_precedence() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b", "c", "d", "e"]);
    send(&mut store, &ns, "m1", "b", "a", "Old message", None);
    for alias in ["a", "b", "c", "d", "e"] {
        observe_status(&mut store, &ns, alias, "blocked", 20);
    }
    let mut rows = store.dashboard_rows(&ns).unwrap();
    for address in &mut rows.addresses {
        match address.alias.as_str() {
            "b" => address.retired = true,
            "c" => address.bound = false,
            "d" => address.occupied = false,
            "e" => address.pane_type = namedrop::PaneType::Terminal,
            _ => {}
        }
    }

    let blocked = build(&rows, None, 0);
    assert_eq!(lane(&blocked, "@b").activity, Activity::Retired);
    assert_eq!(lane(&blocked, "@c").activity, Activity::Offline);
    assert_eq!(lane(&blocked, "@d").activity, Activity::Offline);
    assert_eq!(lane(&blocked, "@e").activity, Activity::Idle);
    assert_eq!(
        blocked.attention.len(),
        1,
        "only the online nonretired agent needs approval"
    );
    assert_eq!(blocked.health, SpaceHealth::NeedsYou { items: 1 });
    assert_eq!(lane(&blocked, "@a").activity, Activity::Blocked);
    assert_eq!(
        blocked.attention[0].summary.text,
        "@a is waiting on an approval or question."
    );
}

#[test]
fn permission_blocking_outranks_acknowledged_work() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    send(&mut store, &ns, "m1", "a", "b", "Run the suite", None);
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
    observe_status(&mut store, &ns, "b", "blocked", 20);

    let blocked = snapshot(&mut store, &ns);
    assert_eq!(blocked.attention[0].subject_alias.as_deref(), Some("b"));
    assert_eq!(blocked.attention[0].message_id.as_deref(), Some("m1"));
    assert_eq!(lane(&blocked, "@b").activity, Activity::Blocked);
    assert_eq!(blocked.health, SpaceHealth::NeedsYou { items: 1 });
    assert_eq!(
        lane(&blocked, "@b").mailbox[0].state,
        DeliveryState::Acknowledged
    );

    observe_status(&mut store, &ns, "b", "idle", 30);
    let working = snapshot(&mut store, &ns);
    assert_eq!(lane(&working, "@b").activity, Activity::Working);
    assert!(working.attention.is_empty());
    assert_eq!(
        working.health,
        SpaceHealth::Busy {
            in_flight: 1,
            working: 1
        }
    );
}

#[test]
fn replies_results_and_nudges_are_not_open_requests() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b", "c"]);
    send(&mut store, &ns, "m1", "a", "b", "Review the diff", None);
    send(&mut store, &ns, "m2", "b", "a", "Two issues", Some("m1"));
    send_as(
        &mut store,
        &ns,
        "result-set/v1",
        "m3",
        "c",
        "a",
        "Findings",
        None,
    );
    send_as(
        &mut store,
        &ns,
        "coordination-nudge/v1",
        "m4",
        "c",
        "b",
        "Status?",
        None,
    );

    let snapshot = snapshot(&mut store, &ns);

    assert!(
        lane(&snapshot, "@b").waiting_on.is_empty(),
        "a reply awaits no answer: {:?}",
        lane(&snapshot, "@b").waiting_on
    );
    assert!(
        lane(&snapshot, "@c").waiting_on.is_empty(),
        "results and nudges await no answer: {:?}",
        lane(&snapshot, "@c").waiting_on
    );
    assert_eq!(lane(&snapshot, "@a").waiting_on, ["@b"]);
}

#[test]
fn acknowledged_work_outranks_an_unrelated_held_delivery() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b", "c"]);
    send(&mut store, &ns, "m1", "a", "b", "Run the suite", None);
    let held = submit(&mut store, &ns, "b");
    store
        .transaction(|tx| tx.hold_delivery_attempt(&ns, &held, &attribution()))
        .unwrap();
    store
        .transaction(|tx| {
            tx.observe_agent_session(
                &ns,
                "observation-w1-b-working",
                &id(&ns, "b"),
                &AgentSessionId::new("session-w1-b"),
                "working",
                &MutationAttribution::new(ActorKind::Plugin, "test", 20),
            )
        })
        .unwrap();

    let snapshot = snapshot(&mut store, &ns);

    assert_eq!(lane(&snapshot, "@b").activity, Activity::Working);
    assert_eq!(
        lane(&snapshot, "@b").mailbox[0].state,
        DeliveryState::Held,
        "the held delivery stays in the tray"
    );
    assert_eq!(snapshot.attention.len(), 1, "and in the attention ribbon");
}

#[test]
fn an_agent_working_with_nothing_in_transit_makes_the_space_busy() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    assert_eq!(snapshot(&mut store, &ns).health, SpaceHealth::Calm);
    store
        .transaction(|tx| {
            tx.observe_agent_session(
                &ns,
                "observation-w1-a-working",
                &id(&ns, "a"),
                &AgentSessionId::new("session-w1-a"),
                "working",
                &MutationAttribution::new(ActorKind::Plugin, "test", 20),
            )
        })
        .unwrap();

    let snapshot = snapshot(&mut store, &ns);

    assert_eq!(lane(&snapshot, "@a").activity, Activity::Working);
    assert_eq!(
        snapshot.health,
        SpaceHealth::Busy {
            in_flight: 0,
            working: 1
        }
    );
}

#[test]
fn a_retired_agent_keeps_its_lane_and_name_after_its_alias_is_released() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    send(&mut store, &ns, "m1", "b", "a", "Done, signing off", None);
    store
        .transaction(|tx| {
            tx.retire_pane_address(&ns, &id(&ns, "b"), &attribution())?;
            tx.release_alias(&ns, "b", &attribution())
        })
        .unwrap();

    let snapshot = snapshot(&mut store, &ns);

    let b = lane(&snapshot, "@b");
    assert_eq!(b.address_id, id(&ns, "b").as_str());
    assert_eq!(b.activity, Activity::Retired);
    assert_eq!(lane(&snapshot, "@a").mailbox[0].from, "@b");
}

#[test]
fn a_role_that_repeats_the_alias_is_not_a_subtitle() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a"]);
    store
        .transaction(|tx| {
            let mut lead = address("address-w1-lead", "lead");
            lead.role_name = "@lead".to_owned();
            let mut critic = address("address-w1-critic", "critic");
            critic.role_name = "Code review".to_owned();
            tx.create_pane_address(&ns, &lead, &attribution())?;
            tx.create_pane_address(&ns, &critic, &attribution())
                .map(|_| ())
        })
        .unwrap();

    let snapshot = snapshot(&mut store, &ns);

    assert_eq!(lane(&snapshot, "@lead").role, None);
    assert_eq!(
        lane(&snapshot, "@critic")
            .role
            .as_ref()
            .map(|r| r.text.as_str()),
        Some("Code review")
    );
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
    assert_eq!(json["attention"], serde_json::json!([]));
    assert_eq!(json["terminal_runs"], serde_json::json!([]));
    assert_eq!(json["space"]["key"], SpaceKey::of(&ns).as_str());
    assert_eq!(json["space"]["workspace_id"], "w1");
    assert_eq!(
        json["space"]["label"],
        serde_json::json!({"text": "namedrop/dashboard", "truncated": false})
    );
    assert_eq!(json["space"]["state"], "active");
    assert_eq!(
        json["health"],
        serde_json::json!({"level": "busy", "in_flight": 1, "working": 0})
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

#[test]
fn a_human_message_gets_the_you_lane_first() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    store
        .transaction(|tx| {
            tx.append_message(
                &ns,
                &MessageDraft {
                    thread_id: ThreadId::new("thread-w1"),
                    message_id: MessageId::new("m-human"),
                    in_reply_to: None,
                    author_address_id: id(&ns, "a"),
                    author_kind: ActorKind::Human,
                    author_alias_snapshot: "a".to_owned(),
                    author_agent_session_snapshot: None,
                    request_id: "request-human".to_owned(),
                    authored_source: "@b ship it".to_owned(),
                    body: "ship it".to_owned(),
                    grammar_version: "mention/v1".to_owned(),
                    recipients: vec![MessageRecipientDraft {
                        address_id: id(&ns, "b"),
                        alias_snapshot: "b".to_owned(),
                        agent_session_snapshot: None,
                        delivery_id: DeliveryId::new("delivery-human"),
                    }],
                    context_references: Vec::new(),
                },
                &attribution(),
            )
            .map(|_| ())
        })
        .unwrap();

    let json = serde_json::to_value(snapshot(&mut store, &ns)).unwrap();

    assert_eq!(json["participants"][0]["address_id"], "human");
    assert_eq!(json["participants"][0]["alias"], "you");
    assert_eq!(
        json["participants"][0]["kind"],
        serde_json::json!({"type": "human"})
    );
    assert_eq!(
        json["participants"][0]["waiting_on"],
        serde_json::json!(["@b"])
    );
    assert_eq!(json["messages"][0]["author"], "human");
    let b = json["participants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["alias"] == "@b")
        .unwrap();
    assert_eq!(b["mailbox"][0]["from"], "you");
}

#[test]
fn equal_timestamp_statuses_resolve_to_the_later_insert() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a"]);
    observe_status(&mut store, &ns, "a", "working", 20);
    observe_status(&mut store, &ns, "a", "idle", 20);
    assert_eq!(
        lane(&snapshot(&mut store, &ns), "@a").activity,
        Activity::Idle
    );
}

#[test]
fn a_rebound_agent_ignores_status_from_its_old_binding() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    observe_status(&mut store, &ns, "a", "working", 20);
    store
        .transaction(|tx| {
            tx.unbind_pane(
                &ns,
                &id(&ns, "a"),
                &MutationAttribution::new(ActorKind::Plugin, "test", 30),
            )?;
            tx.bind_pane(
                &ns,
                &PaneBindingId::new("binding-new"),
                &id(&ns, "a"),
                "pane-w1-a-new",
                &MutationAttribution::new(ActorKind::Plugin, "test", 30),
            )
        })
        .unwrap();
    assert_eq!(
        lane(&snapshot(&mut store, &ns), "@a").activity,
        Activity::Offline
    );
}

#[test]
fn an_unanswered_request_uses_its_current_recipient_alias_and_message() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    send(&mut store, &ns, "m1", "a", "b", "Review", None);
    let attempt = submit(&mut store, &ns, "b");
    store
        .transaction(|tx| {
            tx.acknowledge_delivery_attempt(
                &ns,
                &attempt,
                "ack-1",
                &AgentSessionId::new("session-w1-b"),
                &attribution(),
            )?;
            tx.transition_delivery_attempt(&ns, &attempt, DeliveryState::Settled, &attribution())?;
            tx.replace_alias(&ns, &id(&ns, "b"), "reviewer", &attribution())
        })
        .unwrap();

    let json = serde_json::to_value(snapshot(&mut store, &ns)).unwrap();
    assert_eq!(json["attention"].as_array().unwrap().len(), 1);
    assert_eq!(json["attention"][0]["source_kind"], "agent_disposition");
    assert_eq!(json["attention"][0]["subject_alias"], "reviewer");
    assert_eq!(json["attention"][0]["message_id"], "m1");
    assert_eq!(
        json["attention"][0]["conversation"],
        serde_json::json!({"thread_id":"thread-w1", "message_id":"m1"})
    );
    assert_eq!(
        json["attention"][0]["response"],
        serde_json::json!({"kind":"reply", "thread_id":"thread-w1", "message_id":"m1", "address_id":"address-w1-b"})
    );
    assert_eq!(json["attention"][0]["source_id"], attempt.as_str());
    assert!(json["attention"][0]["attention_id"].as_str().is_some());
    assert_eq!(json["participants"][1]["alias"], "@reviewer");
}

#[test]
fn nonmessage_attention_uses_its_source_and_serializes_absent_links_as_null() {
    use namedrop::{AttentionItemDraft, AttentionItemId, AttentionSourceKind};

    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    send(&mut store, &ns, "m1", "a", "b", "Review", None);
    store
        .transaction(|tx| {
            tx.issue_integration_challenge(
                &ns,
                "registration-1",
                &id(&ns, "b"),
                namedrop::AgentRuntime::Codex,
                "nonce",
                &attribution(),
            )?;
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
            tx.replace_alias(&ns, &id(&ns, "b"), "reviewer", &attribution())?;
            tx.retire_pane_address(&ns, &id(&ns, "a"), &attribution())?;
            tx.release_alias(&ns, "a", &attribution())?;
            for (number, kind, source) in [
                (0, AttentionSourceKind::PaneBinding, "address-w1-a"),
                (
                    1,
                    AttentionSourceKind::IntegrationRegistration,
                    "registration-1",
                ),
                (2, AttentionSourceKind::ContextGrant, "grant-1"),
                (3, AttentionSourceKind::AutomaticReplyLimit, "thread-w1"),
                (4, AttentionSourceKind::Coordination, "address-w1-b"),
                (
                    5,
                    AttentionSourceKind::IntegrationRegistration,
                    "integration-package-codex",
                ),
                (6, AttentionSourceKind::DeliveryAttempt, "missing-attempt"),
                (7, AttentionSourceKind::AgentDisposition, "missing-attempt"),
                (8, AttentionSourceKind::PaneBinding, "missing-address"),
                (
                    9,
                    AttentionSourceKind::IntegrationRegistration,
                    "missing-registration",
                ),
                (10, AttentionSourceKind::ContextGrant, "missing-grant"),
            ] {
                tx.open_attention_item(
                    &ns,
                    &AttentionItemDraft {
                        id: AttentionItemId::new(format!("attention-{number:02}")),
                        source_kind: kind,
                        source_id: source.to_owned(),
                        summary: "@misleading mentions should not determine attribution".to_owned(),
                    },
                    &attribution(),
                )?;
            }
            Ok(())
        })
        .unwrap();

    let json = serde_json::to_value(snapshot(&mut store, &ns)).unwrap();
    let cards = json["attention"].as_array().unwrap();
    assert_eq!(cards.len(), 11);
    for (number, card) in cards.iter().enumerate() {
        assert_eq!(
            card["summary"]["text"],
            "@misleading mentions should not determine attribution"
        );
        assert!(card.get("subject_summary").is_none());
        let expected = match number {
            0 => serde_json::json!("a"),
            1 | 2 => serde_json::json!("reviewer"),
            _ => serde_json::Value::Null,
        };
        assert_eq!(card.get("subject_alias"), Some(&expected), "{card}");
        assert_eq!(card["attention_id"], format!("attention-{number:02}"));
        let response = match number {
            0 => serde_json::json!({"kind":"pane", "address_id":"address-w1-a"}),
            1 | 2 => serde_json::json!({"kind":"pane", "address_id":"address-w1-b"}),
            3 => {
                serde_json::json!({"kind":"reply", "thread_id":"thread-w1", "message_id":"m1", "address_id":"address-w1-a"})
            }
            _ => serde_json::json!({"kind":"local"}),
        };
        assert_eq!(card["response"], response, "{card}");
        if number == 2 || number == 3 {
            assert_eq!(
                card["conversation"],
                serde_json::json!({"thread_id":"thread-w1", "message_id":"m1"})
            );
        } else {
            assert_eq!(card["conversation"], serde_json::Value::Null);
        }
        assert_eq!(
            card.get("message_id"),
            Some(&serde_json::Value::Null),
            "{card}"
        );
    }
}

#[test]
fn attention_links_use_the_namespace_even_when_attempt_and_address_ids_repeat() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let first = seed_space(&mut store, "w1", &["a", "b"]);
    let second = seed_space_on_socket(&mut store, "socket-b", "w1", &["a", "b"]);
    for (ns, message) in [(&first, "first-message"), (&second, "second-message")] {
        send(&mut store, ns, message, "a", "b", "Review", None);
        let attempt = submit(&mut store, ns, "b");
        store
            .transaction(|tx| tx.hold_delivery_attempt(ns, &attempt, &attribution()))
            .unwrap();
    }
    assert_eq!(
        snapshot(&mut store, &first).attention[0]
            .message_id
            .as_deref(),
        Some("first-message")
    );
    assert_eq!(
        snapshot(&mut store, &second).attention[0]
            .message_id
            .as_deref(),
        Some("second-message")
    );
}

#[test]
fn held_attention_uses_its_namespace_when_delivery_and_address_ids_repeat() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let first = seed_space(&mut store, "w1", &["a", "b"]);
    let second = seed_space_on_socket(&mut store, "socket-b", "w1", &["a", "b"]);
    let cases = [
        (&first, "first-message", "first-reviewer"),
        (&second, "second-message", "second-reviewer"),
    ];
    for (ns, message, alias) in cases {
        store
            .transaction(|tx| {
                tx.append_message(
                    ns,
                    &MessageDraft {
                        thread_id: ThreadId::new("thread-w1"),
                        message_id: MessageId::new(message),
                        in_reply_to: None,
                        author_address_id: id(ns, "a"),
                        author_kind: ActorKind::AgentSession,
                        author_alias_snapshot: "a".to_owned(),
                        author_agent_session_snapshot: Some(AgentSessionId::new("session-w1-a")),
                        request_id: format!("request-{message}"),
                        authored_source: "@b Review".to_owned(),
                        body: "Review".to_owned(),
                        grammar_version: "mention/v1".to_owned(),
                        recipients: vec![MessageRecipientDraft {
                            address_id: id(ns, "b"),
                            alias_snapshot: "b".to_owned(),
                            agent_session_snapshot: Some(AgentSessionId::new("session-w1-b")),
                            delivery_id: DeliveryId::new("shared-delivery"),
                        }],
                        context_references: Vec::new(),
                    },
                    &attribution(),
                )?;
                tx.replace_alias(ns, &id(ns, "b"), alias, &attribution())
            })
            .unwrap();
        let attempt = submit(&mut store, ns, "b");
        store
            .transaction(|tx| tx.hold_delivery_attempt(ns, &attempt, &attribution()))
            .unwrap();
    }
    for (ns, message, alias) in cases {
        let json = serde_json::to_value(snapshot(&mut store, ns)).unwrap();
        let cards = json["attention"].as_array().unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["source_kind"], "delivery_attempt");
        assert_eq!(cards[0]["message_id"], message);
        assert_eq!(cards[0]["subject_alias"], alias);
        assert_eq!(
            cards[0]["conversation"],
            serde_json::json!({"thread_id":"thread-w1", "message_id":message})
        );
        assert_eq!(
            cards[0]["response"],
            serde_json::json!({"kind":"reply", "thread_id":"thread-w1", "message_id":message, "address_id":"address-w1-b"})
        );
    }
}

#[test]
fn attention_fallback_uses_its_namespace_when_message_and_address_ids_repeat() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let first = seed_space(&mut store, "w1", &["a", "b"]);
    let second = seed_space_on_socket(&mut store, "socket-b", "w1", &["a", "b"]);
    let cases = [(&first, "first-reviewer"), (&second, "second-reviewer")];
    for (ns, alias) in cases {
        store
            .transaction(|tx| {
                tx.replace_alias(ns, &id(ns, "b"), alias, &attribution())?;
                tx.append_message(
                    ns,
                    &MessageDraft {
                        thread_id: ThreadId::new("thread-w1"),
                        message_id: MessageId::new("shared-message"),
                        in_reply_to: None,
                        author_address_id: id(ns, "a"),
                        author_kind: ActorKind::AgentSession,
                        author_alias_snapshot: "a".to_owned(),
                        author_agent_session_snapshot: Some(AgentSessionId::new("session-w1-a")),
                        request_id: "shared-request".to_owned(),
                        authored_source: format!("@{alias} Review"),
                        body: "Review".to_owned(),
                        grammar_version: "mention/v1".to_owned(),
                        recipients: vec![MessageRecipientDraft {
                            address_id: id(ns, "b"),
                            alias_snapshot: alias.to_owned(),
                            agent_session_snapshot: Some(AgentSessionId::new("session-w1-b")),
                            delivery_id: DeliveryId::new("shared-delivery"),
                        }],
                        context_references: Vec::new(),
                    },
                    &attribution(),
                )?;
                tx.replace_alias(ns, &id(ns, "b"), "renamed-reviewer", &attribution())
            })
            .unwrap();
        let attempt = submit(&mut store, ns, "b");
        store
            .transaction(|tx| tx.hold_delivery_attempt(ns, &attempt, &attribution()))
            .unwrap();
    }
    let connection = rusqlite::Connection::open(directory.path().join("namedrop.sqlite3")).unwrap();
    for (ns, alias) in cases {
        connection
            .execute(
                "INSERT INTO space_envoys (session_socket_id, worktree_id, address_id,
                origin_space_id, origin_address_id, origin_alias, origin_label, host_label)
             VALUES (?1, 'w1', ?2, 'w2', ?2, ?3, 'remote', 'local')",
                rusqlite::params![ns.session_socket_id.as_str(), id(ns, "b").as_str(), alias],
            )
            .unwrap();
    }
    for (ns, alias) in cases {
        let rows = store.dashboard_rows(ns).unwrap();
        assert!(
            rows.addresses
                .iter()
                .all(|address| address.address_id != id(ns, "b").as_str())
        );
        let json = serde_json::to_value(build(&rows, None, 0)).unwrap();
        let cards = json["attention"].as_array().unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0]["source_kind"], "delivery_attempt");
        assert_eq!(cards[0]["message_id"], "shared-message");
        assert_eq!(cards[0]["subject_alias"], alias);
    }
}

#[test]
fn nonmessage_attention_uses_its_namespace_when_registration_and_grant_ids_repeat() {
    use namedrop::{AttentionItemDraft, AttentionItemId, AttentionSourceKind};

    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let first = seed_space(&mut store, "w1", &["a", "b", "c"]);
    let second = seed_space_on_socket(&mut store, "socket-b", "w1", &["a", "b", "c"]);
    let cases = [
        (&first, "b", "c", "first-registration", "first-grantee"),
        (&second, "c", "b", "second-registration", "second-grantee"),
    ];
    for (ns, registration_target, grantee, registration_alias, grant_alias) in cases {
        send(
            &mut store,
            ns,
            "grant-request",
            "a",
            grantee,
            "Review",
            None,
        );
        store
            .transaction(|tx| {
                tx.issue_integration_challenge(
                    ns,
                    "shared-registration",
                    &id(ns, registration_target),
                    namedrop::AgentRuntime::Codex,
                    "nonce",
                    &attribution(),
                )?;
                tx.create_context_grant(
                    ns,
                    &ContextGrantDraft {
                        id: ContextGrantId::new("shared-grant"),
                        thread_id: ThreadId::new("thread-w1"),
                        source_address_id: id(ns, "a"),
                        grantee_address_id: id(ns, grantee),
                        pane_binding_id: PaneBindingId::new("binding-w1-a"),
                        agent_session_id: AgentSessionId::new("session-w1-a"),
                    },
                    &attribution(),
                )?;
                tx.replace_alias(
                    ns,
                    &id(ns, registration_target),
                    registration_alias,
                    &attribution(),
                )?;
                tx.replace_alias(ns, &id(ns, grantee), grant_alias, &attribution())?;
                for (source_kind, source_id) in [
                    (
                        AttentionSourceKind::IntegrationRegistration,
                        "shared-registration",
                    ),
                    (AttentionSourceKind::ContextGrant, "shared-grant"),
                ] {
                    tx.open_attention_item(
                        ns,
                        &AttentionItemDraft {
                            id: AttentionItemId::new(format!("attention-{source_id}")),
                            source_kind,
                            source_id: source_id.to_owned(),
                            summary: "Needs attention".to_owned(),
                        },
                        &attribution(),
                    )?;
                }
                Ok(())
            })
            .unwrap();
    }
    for (ns, _, _, registration_alias, grant_alias) in cases {
        let json = serde_json::to_value(snapshot(&mut store, ns)).unwrap();
        let cards = json["attention"].as_array().unwrap();
        assert_eq!(cards.len(), 2);
        for (kind, alias) in [
            ("integration_registration", registration_alias),
            ("context_grant", grant_alias),
        ] {
            let card = cards
                .iter()
                .find(|card| card["source_kind"] == kind)
                .unwrap();
            assert_eq!(card["subject_alias"], alias, "{kind}");
            assert_eq!(card.get("message_id"), Some(&serde_json::Value::Null));
        }
    }
}

#[test]
fn attention_links_survive_a_message_falling_outside_the_frame() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    send(&mut store, &ns, "old-request", "a", "b", "Review", None);
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
    observe_status(&mut store, &ns, "b", "blocked", 20);
    for number in 0..150 {
        send(
            &mut store,
            &ns,
            &format!("new-{number}"),
            "a",
            "a",
            "Newer message",
            None,
        );
    }
    let blocked = snapshot(&mut store, &ns);
    assert_eq!(blocked.messages.len(), 150);
    assert!(
        blocked
            .messages
            .iter()
            .all(|message| message.message_id != "old-request")
    );
    assert_eq!(
        blocked.attention[0].message_id.as_deref(),
        Some("old-request")
    );
    let card = serde_json::to_value(&blocked.attention[0]).unwrap();
    assert_eq!(card["attention_id"], "agent-status-address-w1-b");
    assert_eq!(card["source_id"], "address-w1-b");
    assert_eq!(
        card["conversation"],
        serde_json::json!({"thread_id":"thread-w1", "message_id":"old-request"})
    );
    assert_eq!(
        card["response"],
        serde_json::json!({"kind":"pane", "address_id":"address-w1-b"})
    );
    store
        .transaction(|tx| tx.hold_delivery_attempt(&ns, &attempt, &attribution()))
        .unwrap();
    let held = snapshot(&mut store, &ns);
    assert_eq!(held.attention[0].message_id.as_deref(), Some("old-request"));
    assert_eq!(held.attention[0].subject_alias.as_deref(), Some("b"));
    assert_eq!(held.attention[1].message_id, None);
}

#[test]
fn blocked_status_does_not_link_queued_or_inactive_work() {
    for state in [
        None,
        Some(DeliveryState::Dispatching),
        Some(DeliveryState::Submitted),
        Some(DeliveryState::Held),
        Some(DeliveryState::Failed),
        Some(DeliveryState::Cancelled),
        Some(DeliveryState::Settled),
    ] {
        let directory = TestDirectory::new();
        let mut store = StateStore::open(directory.path()).unwrap();
        let ns = seed_space(&mut store, "w1", &["a", "b"]);
        send(&mut store, &ns, "m1", "a", "b", "Review", None);
        if let Some(state) = state {
            let attempt = if matches!(state, DeliveryState::Dispatching | DeliveryState::Failed) {
                lease(&mut store, &ns, "b")
            } else {
                submit(&mut store, &ns, "b")
            };
            store
                .transaction(|tx| {
                    if state == DeliveryState::Settled {
                        tx.acknowledge_delivery_attempt(
                            &ns,
                            &attempt,
                            "ack-1",
                            &AgentSessionId::new("session-w1-b"),
                            &attribution(),
                        )?;
                    }
                    if !matches!(state, DeliveryState::Dispatching | DeliveryState::Submitted) {
                        tx.transition_delivery_attempt(&ns, &attempt, state, &attribution())?;
                    }
                    Ok(())
                })
                .unwrap();
        }
        observe_status(&mut store, &ns, "b", "blocked", 20);
        let frame = snapshot(&mut store, &ns);
        let card = frame
            .attention
            .iter()
            .find(|card| {
                card.source_kind == namedrop::dashboard::snapshot::AttentionSourceKind::AgentStatus
            })
            .unwrap();
        assert_eq!(card.subject_alias.as_deref(), Some("b"));
        assert_eq!(card.message_id, None, "state: {state:?}");
    }
}

#[test]
fn blocked_status_does_not_link_an_attempt_from_a_previous_session_or_binding() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    send(&mut store, &ns, "m1", "a", "b", "Review", None);
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
    store
        .transaction(|tx| {
            tx.observe_agent_session(
                &ns,
                "replacement-session",
                &id(&ns, "b"),
                &AgentSessionId::new("session-new"),
                "blocked",
                &MutationAttribution::new(ActorKind::Plugin, "test", 20),
            )
        })
        .unwrap();
    let replaced = snapshot(&mut store, &ns);
    assert_eq!(replaced.attention[0].subject_alias.as_deref(), Some("b"));
    assert_eq!(replaced.attention[0].message_id, None);

    observe_status(&mut store, &ns, "b", "blocked", 30);
    assert_eq!(
        snapshot(&mut store, &ns).attention[0].message_id.as_deref(),
        Some("m1")
    );
    store
        .transaction(|tx| {
            tx.rebind_pane(
                &ns,
                &PaneBindingId::new("binding-new"),
                &id(&ns, "b"),
                "pane-new",
                &MutationAttribution::new(ActorKind::Plugin, "test", 40),
            )?;
            tx.observe_agent_session(
                &ns,
                "rebound-blocked",
                &id(&ns, "b"),
                &AgentSessionId::new("session-w1-b"),
                "blocked",
                &MutationAttribution::new(ActorKind::Plugin, "test", 50),
            )
        })
        .unwrap();
    let rebound = snapshot(&mut store, &ns);
    let card = rebound
        .attention
        .iter()
        .find(|card| {
            card.source_kind == namedrop::dashboard::snapshot::AttentionSourceKind::AgentStatus
        })
        .unwrap();
    assert_eq!(card.subject_alias.as_deref(), Some("b"));
    assert_eq!(card.message_id, None);
}

#[test]
fn a_terminal_mention_keeps_its_request_link_beyond_the_frame_window() {
    let fixture = xws::Fixture::with_activated(1);
    fixture.register_all_activated(1);
    fixture.add_shell("w1", "pane-shell");
    namedrop::execute_lifecycle_operation(
        "name",
        serde_json::json!({"alias": "shell", "pane_type": "terminal"}),
        &fixture.runtime("w1", "pane-shell"),
    )
    .unwrap();
    let sent = fixture.agent_send("w1", "pane-1-lead", "terminal-request", "@shell echo hi");
    assert_eq!(fixture.run_terminal("w1", "pane-shell").unwrap(), 0);
    let ns = StateNamespace::for_space("socket-a", "w1");
    let mut store = StateStore::open(&fixture.state).unwrap();
    let frame = snapshot(&mut store, &ns);
    let json = serde_json::to_value(&frame).unwrap();
    assert_eq!(frame.terminal_runs.len(), 1);
    assert_eq!(json["terminal_runs"][0]["message_id"], sent["message_id"]);
    assert_eq!(json["terminal_runs"][0]["exit_code"], 0);
    assert_eq!(frame.terminal_runs[0].command.text, "echo hi");
    let request = frame
        .messages
        .iter()
        .find(|message| message.message_id == sent["message_id"].as_str().unwrap())
        .unwrap();
    let thread = request.thread_id.clone();
    for number in 0..150 {
        store
            .transaction(|tx| {
                tx.append_message(
                    &ns,
                    &MessageDraft {
                        thread_id: ThreadId::new(&thread),
                        message_id: MessageId::new(format!("new-{number}")),
                        in_reply_to: None,
                        author_address_id: PaneAddressId::new("address-w1-lead"),
                        author_kind: ActorKind::AgentSession,
                        author_alias_snapshot: "lead".to_owned(),
                        author_agent_session_snapshot: Some(AgentSessionId::new(
                            "session-pane-1-lead",
                        )),
                        request_id: format!("new-request-{number}"),
                        authored_source: "Newer message".to_owned(),
                        body: "Newer message".to_owned(),
                        grammar_version: "mention/v1".to_owned(),
                        recipients: vec![MessageRecipientDraft {
                            address_id: PaneAddressId::new("address-w1-planner"),
                            alias_snapshot: "planner".to_owned(),
                            agent_session_snapshot: Some(AgentSessionId::new(
                                "session-pane-1-planner",
                            )),
                            delivery_id: DeliveryId::new(format!("new-delivery-{number}")),
                        }],
                        context_references: Vec::new(),
                    },
                    &MutationAttribution::new(ActorKind::Plugin, "test", 100 + number),
                )
            })
            .unwrap();
    }
    let old = snapshot(&mut store, &ns);
    assert!(
        old.messages
            .iter()
            .all(|message| message.message_id != sent["message_id"].as_str().unwrap())
    );
    assert_eq!(
        serde_json::to_value(&old.terminal_runs).unwrap()[0]["message_id"],
        sent["message_id"]
    );
}

#[test]
fn history_message_json_keeps_its_existing_keys_and_values() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    send(&mut store, &ns, "m1", "a", "b", "Review", None);
    observe_status(&mut store, &ns, "b", "blocked", 20);
    let history = store.dashboard_history(&ns, None, 50).unwrap().unwrap();
    let mut rows = store.dashboard_rows(&ns).unwrap();
    rows.messages = history.messages;
    rows.legs = history.legs;
    let json = serde_json::to_value(build(&rows, None, 0).messages).unwrap();
    assert_eq!(
        json,
        serde_json::json!([{
            "message_id": "m1",
            "thread_id": "thread-w1",
            "in_reply_to": null,
            "at_ms": 10,
            "author": "address-w1-a",
            "kind": "mention",
            "body": {"text": "Review", "truncated": false},
            "legs": [{"to": "address-w1-b", "stage": {"stage": "queued"}}]
        }])
    );
}

#[test]
fn attention_tracks_current_recipient_after_rename_and_alias_reuse() {
    for unanswered in [false, true] {
        let directory = TestDirectory::new();
        let mut store = StateStore::open(directory.path()).unwrap();
        let ns = seed_space(&mut store, "w1", &["a", "b"]);
        send(&mut store, &ns, "m1", "a", "b", "Review", None);
        let attempt = submit(&mut store, &ns, "b");
        store
            .transaction(|tx| {
                if unanswered {
                    tx.acknowledge_delivery_attempt(
                        &ns,
                        &attempt,
                        "ack-1",
                        &AgentSessionId::new("session-w1-b"),
                        &attribution(),
                    )?;
                    tx.transition_delivery_attempt(
                        &ns,
                        &attempt,
                        DeliveryState::Settled,
                        &attribution(),
                    )?;
                } else {
                    tx.hold_delivery_attempt(&ns, &attempt, &attribution())?;
                }
                tx.replace_alias(&ns, &id(&ns, "b"), "reviewer", &attribution())?;
                tx.release_alias(&ns, "b", &attribution())?;
                tx.create_pane_address(&ns, &address("new-owner-of-b", "b"), &attribution())?;
                Ok(())
            })
            .unwrap();
        let frame = snapshot(&mut store, &ns);
        assert_eq!(frame.attention.len(), 1);
        assert_eq!(
            frame.attention[0].subject_alias.as_deref(),
            Some("reviewer"),
            "unanswered={unanswered}"
        );
        assert_eq!(frame.attention[0].message_id.as_deref(), Some("m1"));
    }
}

#[test]
fn multiple_recipient_attention_keeps_one_card_per_recipient() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b", "c", "d"]);
    let space = ns.space_id.as_str();
    let message = "m1";
    let from = "a";
    let to = ["b", "c", "d"];
    store
        .transaction(|tx| {
            tx.append_message(
                &ns,
                &MessageDraft {
                    thread_id: ThreadId::new(format!("thread-{space}")),
                    message_id: MessageId::new(message),
                    in_reply_to: None,
                    author_address_id: id(&ns, from),
                    author_kind: ActorKind::AgentSession,
                    author_alias_snapshot: from.to_owned(),
                    author_agent_session_snapshot: Some(AgentSessionId::new(format!(
                        "session-{space}-{from}"
                    ))),
                    request_id: format!("request-{message}"),
                    authored_source: "multi".to_owned(),
                    body: "multi".to_owned(),
                    grammar_version: "mention/v1".to_owned(),
                    recipients: to
                        .iter()
                        .map(|t| MessageRecipientDraft {
                            address_id: id(&ns, t),
                            alias_snapshot: (*t).to_owned(),
                            agent_session_snapshot: Some(AgentSessionId::new(format!(
                                "session-{space}-{t}"
                            ))),
                            delivery_id: DeliveryId::new(format!("delivery-{message}-{t}")),
                        })
                        .collect(),
                    context_references: Vec::new(),
                },
                &attribution(),
            )
            .map(|_| ())
        })
        .unwrap();
    let held_b = submit(&mut store, &ns, "b");
    let held_c = submit(&mut store, &ns, "c");
    let failed_d = lease(&mut store, &ns, "d");
    store
        .transaction(|tx| {
            tx.hold_delivery_attempt(&ns, &held_c, &attribution())?;
            tx.transition_delivery_attempt(&ns, &failed_d, DeliveryState::Failed, &attribution())?;
            tx.hold_delivery_attempt(&ns, &held_b, &attribution())
        })
        .unwrap();
    let frame = snapshot(&mut store, &ns);
    let mut cards: Vec<_> = frame
        .attention
        .iter()
        .map(|card| (card.subject_alias.as_deref(), card.message_id.as_deref()))
        .collect();
    cards.sort();
    assert_eq!(
        cards,
        vec![
            (Some("b"), Some("m1")),
            (Some("c"), Some("m1")),
            (Some("d"), Some("m1")),
        ]
    );

    store
        .transaction(|tx| {
            tx.replace_alias(&ns, &id(&ns, "c"), "remote-c", &attribution())?;
            tx.replace_alias(&ns, &id(&ns, "d"), "remote-d", &attribution())
        })
        .unwrap();
    let connection = rusqlite::Connection::open(directory.path().join("namedrop.sqlite3")).unwrap();
    for recipient in ["c", "d"] {
        connection
            .execute(
                "INSERT INTO space_envoys (session_socket_id, worktree_id, address_id,
                origin_space_id, origin_address_id, origin_alias, origin_label, host_label)
             VALUES ('socket-a', 'w1', ?1, 'w2', ?1, ?2, 'remote', 'local')",
                rusqlite::params![id(&ns, recipient).as_str(), recipient],
            )
            .unwrap();
    }
    let rows = store.dashboard_rows(&ns).unwrap();
    assert_eq!(rows.addresses.len(), 2);
    let frame = build(&rows, None, 0);
    let mut cards: Vec<_> = frame
        .attention
        .iter()
        .map(|card| (card.subject_alias.as_deref(), card.message_id.as_deref()))
        .collect();
    cards.sort();
    assert_eq!(
        cards,
        vec![
            (Some("b"), Some("m1")),
            (Some("c"), Some("m1")),
            (Some("d"), Some("m1")),
        ]
    );
}

#[test]
fn terminal_runs_keep_their_own_request_messages() {
    let fixture = xws::Fixture::with_activated(1);
    fixture.register_all_activated(1);
    fixture.add_shell("w1", "pane-shell");
    namedrop::execute_lifecycle_operation(
        "name",
        serde_json::json!({"alias": "shell", "pane_type": "terminal"}),
        &fixture.runtime("w1", "pane-shell"),
    )
    .unwrap();
    let first = fixture.agent_send("w1", "pane-1-lead", "req-one", "@shell echo one");
    assert_eq!(fixture.run_terminal("w1", "pane-shell").unwrap(), 0);
    fixture.advance(1000);
    let second = fixture.agent_send("w1", "pane-1-planner", "req-two", "@shell echo two");
    fixture.drain("w1");
    assert_eq!(fixture.run_terminal("w1", "pane-shell").unwrap(), 0);
    let ns = StateNamespace::for_space("socket-a", "w1");
    let mut store = StateStore::open(&fixture.state).unwrap();
    let frame = snapshot(&mut store, &ns);
    let runs: Vec<_> = frame
        .terminal_runs
        .iter()
        .map(|run| (run.command.text.as_str(), run.message_id.as_deref()))
        .collect();
    assert_eq!(
        runs,
        vec![
            ("echo two", second["message_id"].as_str()),
            ("echo one", first["message_id"].as_str()),
        ]
    );
}

#[test]
fn terminal_links_use_the_namespace_when_delivery_and_attempt_ids_repeat() {
    let dir = TestDirectory::new();
    let mut store = StateStore::open(dir.path()).unwrap();
    let n1 = seed_space(&mut store, "w1", &["a", "b"]);
    let n2 = seed_space_on_socket(&mut store, "socket-b", "w1", &["a", "b"]);
    for (ns, msg) in [(&n1, "m-first"), (&n2, "m-second")] {
        let space = ns.space_id.as_str();
        store
            .transaction(|tx| {
                tx.append_message(
                    ns,
                    &MessageDraft {
                        thread_id: ThreadId::new(format!("thread-{space}")),
                        message_id: MessageId::new(msg),
                        in_reply_to: None,
                        author_address_id: id(ns, "a"),
                        author_kind: ActorKind::AgentSession,
                        author_alias_snapshot: "a".to_owned(),
                        author_agent_session_snapshot: Some(AgentSessionId::new(format!(
                            "session-{space}-a"
                        ))),
                        request_id: format!("request-{msg}"),
                        authored_source: "@b echo".to_owned(),
                        body: "echo".to_owned(),
                        grammar_version: "mention/v1".to_owned(),
                        recipients: vec![MessageRecipientDraft {
                            address_id: id(ns, "b"),
                            alias_snapshot: "b".to_owned(),
                            agent_session_snapshot: Some(AgentSessionId::new(format!(
                                "session-{space}-b"
                            ))),
                            delivery_id: DeliveryId::new("delivery-shared"),
                        }],
                        context_references: Vec::new(),
                    },
                    &attribution(),
                )
                .map(|_| ())
            })
            .unwrap();
        submit(&mut store, ns, "b");
    }
    drop(store);
    let db = rusqlite::Connection::open(dir.path().join("namedrop.sqlite3")).unwrap();
    db.execute_batch("INSERT INTO terminal_executions(session_socket_id, worktree_id, attempt_id, terminal_id, command, started_at_ms, completed_at_ms, exit_code, output)
          VALUES ('socket-a','w1','attempt-w1-b','t1','echo first',50,60,0,'first'),
                 ('socket-b','w1','attempt-w1-b','t1','echo second',55,65,3,'second');").unwrap();
    let fk: i64 = db
        .query_row("SELECT COUNT(*) FROM pragma_foreign_key_check", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(fk, 0);
    let mut store = StateStore::open(dir.path()).unwrap();
    for (ns, want) in [(&n1, "m-first"), (&n2, "m-second")] {
        let frame = snapshot(&mut store, ns);
        assert_eq!(frame.terminal_runs.len(), 1);
        assert_eq!(frame.terminal_runs[0].message_id.as_deref(), Some(want));
    }
}

#[test]
fn recently_closed_members_expire_at_five_minutes_but_references_keep_them() {
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let ns = seed_space(&mut store, "w1", &["a", "b"]);
    store
        .transaction(|tx| {
            tx.retire_pane_address(
                &ns,
                &id(&ns, "b"),
                &MutationAttribution::new(ActorKind::Plugin, "test", 1_000),
            )
        })
        .unwrap();
    let rows = store.dashboard_rows(&ns).unwrap();
    let before = build(&rows, None, 300_999);
    assert_eq!(lane(&before, "@b").activity, Activity::Retired);
    assert_eq!(
        serde_json::to_value(lane(&before, "@b")).unwrap()["recently_closed"],
        true
    );
    assert_eq!(
        serde_json::to_value(lane(&before, "@a")).unwrap()["recently_closed"],
        false
    );
    let expired = build(&rows, None, 301_000);
    assert!(
        !expired
            .participants
            .iter()
            .any(|member| member.alias == "@b")
    );
    send(&mut store, &ns, "reference", "a", "a", "history", None);
    let mut rows = store.dashboard_rows(&ns).unwrap();
    rows.messages[0].author_address_id = id(&ns, "b").as_str().to_owned();
    for (now, recently_closed) in [(300_999, true), (301_000, false), (301_001, false)] {
        let frame = build(&rows, None, now);
        let member = lane(&frame, "@b");
        assert_eq!(member.activity, Activity::Retired);
        assert_eq!(
            serde_json::to_value(member).unwrap()["recently_closed"],
            recently_closed,
            "at {now}"
        );
        assert_eq!(frame.messages[0].author, id(&ns, "b").as_str());
    }

    rows.addresses
        .iter_mut()
        .find(|a| a.address_id == id(&ns, "a").as_str())
        .unwrap()
        .closed_at_ms = Some(1_000);
    assert_eq!(
        serde_json::to_value(lane(&build(&rows, None, 300_999), "@a")).unwrap()["recently_closed"],
        false
    );
    rows.addresses
        .iter_mut()
        .find(|a| a.address_id == id(&ns, "b").as_str())
        .unwrap()
        .closed_at_ms = None;
    assert_eq!(
        serde_json::to_value(lane(&build(&rows, None, 300_999), "@b")).unwrap()["recently_closed"],
        false
    );

    rows.addresses[0].address_id = namedrop::dashboard::snapshot::HUMAN.to_owned();
    assert_eq!(
        serde_json::to_value(lane(&build(&rows, None, 300_999), "you")).unwrap()["recently_closed"],
        false
    );
}

#[test]
fn coordination_attention_prefers_report_then_latest_worker_then_wake_then_request() {
    use namedrop::{AttentionItemDraft, AttentionItemId, AttentionSourceKind};
    use rusqlite::params;

    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let first = seed_space(&mut store, "w1", &["a", "b", "c"]);
    let second = seed_space_on_socket(&mut store, "socket-b", "w1", &["a", "b", "c"]);
    let third = seed_space(&mut store, "w2", &["a", "b", "c"]);
    let db = rusqlite::Connection::open(directory.path().join("namedrop.sqlite3")).unwrap();
    for (index, ns) in [&first, &second, &third].into_iter().enumerate() {
        let report = format!("report-{index}");
        for (message, from, to, reply) in [
            ("request", "a", "b", None),
            ("worker-earlier", "b", "a", Some("request")),
            ("worker-latest", "c", "a", Some("request")),
            (report.as_str(), "a", "b", Some("request")),
            ("wake", "a", "b", Some("request")),
        ] {
            send(
                &mut store,
                ns,
                message,
                from,
                to,
                "Synthetic coordination evidence",
                reply,
            );
        }
        db.execute(
            "INSERT INTO coordinations (session_socket_id,worktree_id,coordination_id,request_id,owner_address_id,owner_session_id,deliverable,report_message_id,wake_message_id,created_at_ms,updated_at_ms)
             VALUES (?1,?2,'coordination-shared','begin',?3,'session-owner','Review the worker report',?4,'wake',10,10)",
            params![ns.session_socket_id, ns.space_id.as_str(), id(ns, "a").as_str(), report],
        ).unwrap();
        for (assignment, worker, message) in [
            ("first", "b", "worker-earlier"),
            ("second", "c", "worker-latest"),
        ] {
            db.execute(
                "INSERT INTO coordination_assignments (session_socket_id,worktree_id,assignment_id,coordination_id,recipient_address_id,recipient_session_id,original_message_id,current_message_id,revision,work_outcome,last_reply_message_id)
                 VALUES (?1,?2,?3,'coordination-shared',?4,'session-worker','request','request',1,'completed',?5)",
                params![ns.session_socket_id, ns.space_id.as_str(), assignment, id(ns, worker).as_str(), message],
            ).unwrap();
        }
        store
            .transaction(|tx| {
                tx.open_attention_item(
                    ns,
                    &AttentionItemDraft {
                        id: AttentionItemId::new("attention-coordination-shared"),
                        source_kind: AttentionSourceKind::Coordination,
                        source_id: "coordination-shared".into(),
                        summary: "Coordination needs a decision".into(),
                    },
                    &attribution(),
                )
            })
            .unwrap();
    }
    send(
        &mut store,
        &second,
        "foreign-only-report",
        "a",
        "b",
        "Other namespace report",
        Some("request"),
    );
    for (stage, expected) in [
        ("report", None),
        ("worker", Some("worker-latest")),
        ("wake", Some("wake")),
        ("request", Some("request")),
    ] {
        for (index, ns) in [&first, &second, &third].into_iter().enumerate() {
            let json = serde_json::to_value(snapshot(&mut store, ns)).unwrap();
            let card = &json["attention"][0];
            let expected = expected
                .map(str::to_owned)
                .unwrap_or_else(|| format!("report-{index}"));
            assert_eq!(card["attention_id"], "attention-coordination-shared");
            assert_eq!(card["source_id"], "coordination-shared");
            assert_eq!(card["coordination_id"], "coordination-shared");
            assert_eq!(
                card["conversation"],
                serde_json::json!({"thread_id":format!("thread-{}",ns.space_id.as_str()), "message_id":expected})
            );
            assert_eq!(
                card["response"],
                serde_json::json!({"kind":"message", "address_id":id(ns,"a").as_str()})
            );
        }
        match stage {
            // A report id found only in another namespace must not mask worker evidence.
            "report" => {
                db.execute(
                    "UPDATE coordinations SET report_message_id=CASE WHEN session_socket_id='socket-b' THEN NULL ELSE 'foreign-only-report' END",
                    [],
                )
                .unwrap();
            }
            "worker" => {
                db.execute(
                    "UPDATE coordination_assignments SET last_reply_message_id=NULL",
                    [],
                )
                .unwrap();
            }
            "wake" => {
                db.execute("UPDATE coordinations SET wake_message_id=NULL", [])
                    .unwrap();
            }
            _ => {}
        }
    }
}

#[test]
fn attention_responses_respect_thread_controls_and_terminal_recipients_in_their_namespace() {
    use namedrop::{AttentionItemDraft, AttentionItemId, AttentionSourceKind};
    let directory = TestDirectory::new();
    let mut store = StateStore::open(directory.path()).unwrap();
    let spaces = [
        seed_space(&mut store, "w1", &["a", "b"]),
        seed_space_on_socket(&mut store, "socket-b", "w1", &["a", "b"]),
        seed_space(&mut store, "w2", &["a", "b"]),
    ];
    for ns in &spaces {
        send(&mut store, ns, "request", "a", "b", "Review", None);
        let attempt = submit(&mut store, ns, "b");
        store
            .transaction(|tx| {
                for (number, source, source_id) in [
                    (0, AttentionSourceKind::DeliveryAttempt, attempt.as_str()),
                    (1, AttentionSourceKind::AgentDisposition, attempt.as_str()),
                    (
                        2,
                        AttentionSourceKind::AutomaticReplyLimit,
                        &format!("thread-{}", ns.space_id.as_str()),
                    ),
                ] {
                    tx.open_attention_item(
                        ns,
                        &AttentionItemDraft {
                            id: AttentionItemId::new(format!("source-{number}")),
                            source_kind: source,
                            source_id: source_id.into(),
                            summary: "Needs a response".into(),
                        },
                        &attribution(),
                    )?;
                }
                Ok(())
            })
            .unwrap();
    }
    let db = rusqlite::Connection::open(directory.path().join("namedrop.sqlite3")).unwrap();
    for (paused, stopped, terminal, expected) in [
        (0, 0, 0, "reply"),
        (1, 0, 0, "message"),
        (0, 1, 0, "message"),
        (0, 0, 1, "pane"),
    ] {
        db.execute("INSERT INTO thread_controls (session_socket_id,worktree_id,thread_id,paused,stopped,reply_allowance) VALUES ('socket-a','w1','thread-w1',?1,?2,0) ON CONFLICT DO UPDATE SET paused=excluded.paused,stopped=excluded.stopped", rusqlite::params![paused,stopped]).unwrap();
        db.execute("UPDATE pane_addresses SET pane_type=CASE WHEN ?1 THEN 'terminal' ELSE 'agent' END WHERE session_socket_id='socket-a' AND worktree_id='w1'", [terminal]).unwrap();
        for ns in &spaces {
            let frame = serde_json::to_value(snapshot(&mut store, ns)).unwrap();
            for card in frame["attention"].as_array().unwrap() {
                assert_eq!(card["conversation"]["message_id"], "request");
                assert_eq!(
                    card["response"]["kind"],
                    if ns == &spaces[0] { expected } else { "reply" },
                    "{ns:?} {card}"
                );
            }
        }
    }
}
