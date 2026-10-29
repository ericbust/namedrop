#[path = "support/xws.rs"]
mod xws;

use namedrop::{
    ActorKind, MutationAttribution, PaneAddressId, PaneBindingId, StateNamespace, StateStore,
    execute_message_operation, send_operator_message,
};
use serde_json::json;
use xws::Fixture;

#[test]
fn operator_attention_uses_human_alias_without_capturing_a_pane_named_operator() {
    use namedrop::{AttentionItemDraft, AttentionItemId, AttentionSourceKind};
    let f = Fixture::new();
    f.register_all();
    f.rename("w1", "lead", "operator");
    let mut runtime = f.runtime("w1", "unused");
    runtime.current_pane_id = None;
    send_operator_message("attention-human", "@planner inspect", None, &runtime).unwrap();
    let pane_address: String = f
        .db()
        .query_row(
            "SELECT address_id FROM alias_claims WHERE session_socket_id='socket-a'
         AND worktree_id='w1' AND alias='operator' AND claim_state='active'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let ns = StateNamespace::for_space("socket-a", "w1");
    let mut store = StateStore::open(&f.state).unwrap();
    store
        .transaction(|tx| {
            for (id, source) in [("human", "operator:local"), ("pane", pane_address.as_str())] {
                tx.open_attention_item(
                    &ns,
                    &AttentionItemDraft {
                        id: AttentionItemId::new(id),
                        source_kind: AttentionSourceKind::PaneBinding,
                        source_id: source.to_owned(),
                        summary: id.to_owned(),
                    },
                    &MutationAttribution::new(ActorKind::Human, "test", 42),
                )?;
            }
            Ok(())
        })
        .unwrap();
    let rows = store.dashboard_rows(&ns).unwrap();
    let alias = |id: &str| {
        rows.attention
            .iter()
            .find(|row| row.item.id.as_str() == id)
            .unwrap()
            .subject_alias
            .as_deref()
    };
    assert_eq!(alias("pane"), Some("operator"));
    assert_eq!(alias("human"), Some("you"));
    let frame = namedrop::dashboard::snapshot::build(&rows, None, 42);
    let human = frame
        .attention
        .iter()
        .find(|item| item.summary.text == "human")
        .unwrap();
    assert_eq!(human.subject_alias.as_deref(), Some("you"));
    assert_eq!(human.message_id, None);
}

#[test]
fn operator_attention_normalizes_an_exact_remote_operator_envoy() {
    use namedrop::{AttentionItemDraft, AttentionItemId, AttentionSourceKind};
    let f = Fixture::new();
    f.register_all();
    let mut runtime = f.runtime("w1", "unused");
    runtime.current_pane_id = None;
    send_operator_message("attention-remote", "#ws2 @tester inspect", None, &runtime).unwrap();
    let envoy: String = f
        .db()
        .query_row(
            "SELECT address_id FROM space_envoys WHERE session_socket_id='socket-a'
         AND worktree_id='w2' AND origin_space_id='w1' AND origin_address_id='operator:local'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let ns = StateNamespace::for_space("socket-a", "w2");
    let mut store = StateStore::open(&f.state).unwrap();
    store
        .transaction(|tx| {
            tx.open_attention_item(
                &ns,
                &AttentionItemDraft {
                    id: AttentionItemId::new("remote-human"),
                    source_kind: AttentionSourceKind::PaneBinding,
                    source_id: envoy.clone(),
                    summary: "Remote operator".to_owned(),
                },
                &MutationAttribution::new(ActorKind::Human, "test", 42),
            )
        })
        .unwrap();
    let rows = store.dashboard_rows(&ns).unwrap();
    let attention = rows
        .attention
        .iter()
        .find(|row| row.item.id.as_str() == "remote-human")
        .unwrap();
    assert_eq!(attention.subject_alias.as_deref(), Some("you"));
    let frame = namedrop::dashboard::snapshot::build(&rows, None, 42);
    assert_eq!(frame.attention[0].subject_alias.as_deref(), Some("you"));
}

#[test]
fn paneless_operator_round_trip_uses_the_ledger_without_an_operator_pane() {
    let f = Fixture::new();
    f.register_all();
    f.rename("w1", "lead", "operator");
    f.herdr.prompts.lock().unwrap().clear();
    f.herdr.commands.lock().unwrap().clear();
    f.herdr.notifications.lock().unwrap().clear();
    let mut runtime = f.runtime("w1", "unused");
    runtime.current_pane_id = None;

    assert!(
        execute_message_operation(
            "send",
            json!({"request_id":"pane-required","source":"@planner investigate"}),
            &runtime,
        )
        .unwrap_err()
        .to_string()
        .contains("current Herdr pane")
    );
    let sent =
        send_operator_message("operator-local", "@planner investigate", None, &runtime).unwrap();
    let envelope = f.herdr.last_envelope("pane-1-planner");
    assert_eq!(envelope["control"]["sender"]["alias"], "operator");
    assert_eq!(envelope["control"]["message_id"], sent["message_id"]);
    assert_eq!(envelope["control"]["reply_mode"], "agent_thread");
    let operator = envelope["control"]["sender"]["address_id"]
        .as_str()
        .unwrap();
    let db = f.db();
    let assert_disposition =
        |workspace: &str, envelope: &serde_json::Value, reply: &serde_json::Value| {
            let disposition: (String, String) = db
                .query_row(
                    "SELECT disposition_kind, disposition_id FROM delivery_dispositions
             WHERE worktree_id = ?1 AND attempt_id = ?2",
                    [
                        workspace,
                        envelope["control"]["attempt_id"].as_str().unwrap(),
                    ],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap();
            assert_eq!(
                disposition,
                (
                    "reply".to_owned(),
                    reply["message_id"].as_str().unwrap().to_owned()
                )
            );
        };
    let count = |query: &str| {
        db.query_row(query, [operator], |row| row.get::<_, i64>(0))
            .unwrap()
    };
    assert_eq!(
        count("SELECT COUNT(*) FROM pane_addresses WHERE address_id = ?1"),
        1
    );
    for table in [
        "alias_claims",
        "pane_bindings",
        "agent_session_observations",
        "integration_registrations",
    ] {
        assert_eq!(
            count(&format!(
                "SELECT COUNT(*) FROM {table} WHERE address_id = ?1"
            )),
            0,
            "{table}"
        );
    }
    let author: (String, Option<String>) = db.query_row(
        "SELECT author_kind, author_session_snapshot FROM collaboration_messages WHERE message_id = ?1",
        [sent["message_id"].as_str().unwrap()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!(author, ("human".to_owned(), None));

    f.rename("w1", "planner", "worker");
    let replay =
        send_operator_message("operator-local", "@planner investigate", None, &runtime).unwrap();
    assert_eq!(replay["message_id"], sent["message_id"]);
    assert_eq!(replay["idempotent_replay"], true);
    assert_eq!(f.herdr.envelopes("pane-1-planner").len(), 1);
    let before_refusals = f.message_rows();
    assert!(
        send_operator_message(
            "operator-local",
            "@worker different content",
            None,
            &runtime
        )
        .is_err()
    );
    assert!(
        send_operator_message("operator-unknown", "@ghost investigate", None, &runtime)
            .unwrap_err()
            .to_string()
            .contains("ghost")
    );
    let mut missing_workspace = f.runtime("w1", "unused");
    missing_workspace.current_pane_id = None;
    missing_workspace.workspace_id = None;
    assert!(
        send_operator_message(
            "operator-no-home",
            "@worker investigate",
            None,
            &missing_workspace
        )
        .unwrap_err()
        .to_string()
        .contains("explicit source workspace")
    );
    assert_eq!(f.message_rows(), before_refusals);

    let reply = f.reply_to("w1", "pane-1-planner", &envelope, "@operator answer-local");
    assert_disposition("w1", &envelope, &reply);
    let rows = f.message_rows();
    let dispositions = f.count("SELECT COUNT(*) FROM delivery_dispositions");
    let replay = f.reply_to("w1", "pane-1-planner", &envelope, "@operator answer-local");
    assert_eq!(replay["message_id"], reply["message_id"]);
    assert_eq!(replay["idempotent_replay"], true);
    assert_eq!(f.message_rows(), rows);
    assert_eq!(
        f.count("SELECT COUNT(*) FROM delivery_dispositions"),
        dispositions
    );
    assert_eq!(reply["thread_id"], sent["thread_id"]);
    assert_eq!(f.delivery_states("w1", &reply["message_id"]), ["settled"]);
    assert_eq!(f.delivery_states("w1", &sent["message_id"]), ["settled"]);
    let target: String = db
        .query_row(
            "SELECT in_reply_to FROM collaboration_messages WHERE message_id = ?1",
            [reply["message_id"].as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(target, sent["message_id"].as_str().unwrap());
    let recipient: String = db
        .query_row(
            "SELECT address_id FROM message_recipients WHERE message_id = ?1",
            [reply["message_id"].as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(recipient, operator);
    assert!(
        f.herdr.envelopes("pane-1-lead").is_empty(),
        "the pane holding @operator must not capture the reply"
    );

    let mut projection_store = StateStore::open(&f.state).unwrap();
    let frame = namedrop::dashboard::snapshot::build(
        &projection_store
            .dashboard_rows(&StateNamespace::for_space("socket-a", "w1"))
            .unwrap(),
        None,
        42,
    );
    assert_eq!(
        frame
            .participants
            .iter()
            .filter(|lane| lane.address_id == "human")
            .count(),
        1
    );
    assert!(
        frame
            .participants
            .iter()
            .any(|lane| lane.address_id == "address-w1-lead" && lane.alias == "@operator")
    );
    let ns = StateNamespace::for_space("socket-a", "w1");
    assert!(
        StateStore::open(&f.state)
            .unwrap()
            .transaction(|tx| tx.bind_pane(
                &ns,
                &PaneBindingId::new("operator-binding"),
                &PaneAddressId::new(operator),
                "pane-1-lead",
                &MutationAttribution::new(ActorKind::Human, "test", 42),
            ))
            .is_err()
    );

    for (request, source, recipients) in [
        (
            "operator-remote",
            "#ws2 @tester investigate",
            vec![("w2", "pane-2-tester")],
        ),
        (
            "operator-all",
            "#all @lead investigate",
            vec![("w2", "pane-2-lead"), ("w3", "pane-3-lead")],
        ),
    ] {
        let sent = send_operator_message(request, source, None, &runtime).unwrap();
        for (workspace, pane) in recipients {
            let envelope = f.herdr.last_envelope(pane);
            let body = format!("{request}-{workspace}");
            let reply = f.reply_to(workspace, pane, &envelope, &format!("@operator {body}"));
            assert_disposition(workspace, &envelope, &reply);
            let rows = f.message_rows();
            let dispositions = f.count("SELECT COUNT(*) FROM delivery_dispositions");
            let replay = f.reply_to(workspace, pane, &envelope, &format!("@operator {body}"));
            assert_eq!(replay["message_id"], reply["message_id"]);
            assert_eq!(replay["idempotent_replay"], true);
            assert_eq!(f.message_rows(), rows);
            assert_eq!(
                f.count("SELECT COUNT(*) FROM delivery_dispositions"),
                dispositions
            );
            let (target, thread, state): (String, String, String) = db
                .query_row(
                    "SELECT m.in_reply_to, m.thread_id, d.state
                 FROM collaboration_messages m JOIN mailbox_deliveries d
                   USING (session_socket_id, worktree_id, message_id)
                 WHERE m.worktree_id = 'w1' AND m.body = ?1 AND d.address_id = ?2",
                    [&body, operator],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(target, sent["message_id"].as_str().unwrap());
            assert_eq!(thread, sent["thread_id"].as_str().unwrap());
            assert_eq!(state, "settled");
        }
        let replay = send_operator_message(request, source, None, &runtime).unwrap();
        assert_eq!(replay["message_id"], sent["message_id"]);
        assert_eq!(replay["idempotent_replay"], true);
    }

    f.drain("w1");
    assert_eq!(
        count(
            "SELECT COUNT(*) FROM mailbox_deliveries WHERE address_id = ?1 AND state <> 'settled'"
        ),
        0
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM delivery_attempts WHERE address_id = ?1"),
        0
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM pane_bindings WHERE address_id = ?1"),
        0
    );
    assert!(
        db.prepare("PRAGMA foreign_key_check")
            .unwrap()
            .query([])
            .unwrap()
            .next()
            .unwrap()
            .is_none()
    );
    assert!(f.herdr.notifications.lock().unwrap().is_empty());
    assert!(f.herdr.runs.lock().unwrap().is_empty());
    let prompts = f.herdr.prompts.lock().unwrap();
    assert_eq!(prompts.len(), 4);
    assert!(prompts.iter().all(|(pane, _)| {
        [
            "pane-1-planner",
            "pane-2-tester",
            "pane-2-lead",
            "pane-3-lead",
        ]
        .contains(&pane.as_str())
    }));
    println!(
        "operator round trips verified locally, through #ws2, and through #all; replies persisted and settled with zero operator attempts, bindings, notifications, or pane input"
    );
}

#[test]
fn operator_followup_and_projection_keep_one_human_lane_after_window_rolls() {
    use namedrop::MessageId;
    use namedrop::dashboard::snapshot::{HUMAN, ParticipantKind, build};
    let f = Fixture::new();
    f.register_all();
    let mut runtime = f.runtime("w1", "unused");
    runtime.current_pane_id = None;
    let sent =
        send_operator_message("thread-first", "@planner investigate", None, &runtime).unwrap();
    let envelope = f.herdr.last_envelope("pane-1-planner");
    let reply = f.reply_to("w1", "pane-1-planner", &envelope, "@operator answer");
    let target = MessageId::new(reply["message_id"].as_str().unwrap());
    let followup = send_operator_message(
        "thread-followup",
        "@planner continue",
        Some(&target),
        &runtime,
    )
    .unwrap();
    assert_eq!(followup["thread_id"], sent["thread_id"]);
    assert_eq!(
        send_operator_message(
            "thread-followup",
            "@planner continue",
            Some(&target),
            &runtime
        )
        .unwrap()["message_id"],
        followup["message_id"]
    );
    let original = MessageId::new(sent["message_id"].as_str().unwrap());
    assert!(
        send_operator_message(
            "thread-followup",
            "@planner continue",
            Some(&original),
            &runtime
        )
        .is_err()
    );
    let before = f.message_rows();
    assert!(
        send_operator_message(
            "wrong-workspace",
            "@planner continue",
            Some(&MessageId::new("missing-target")),
            &runtime
        )
        .is_err()
    );
    assert_eq!(f.message_rows(), before);
    send_operator_message(
        "waiting-request",
        "@planner another request",
        None,
        &runtime,
    )
    .unwrap();
    let ns = StateNamespace::for_space("socket-a", "w1");
    let mut store = StateStore::open(&f.state).unwrap();
    let frame = build(&store.dashboard_rows(&ns).unwrap(), None, 42);
    assert_eq!(
        frame
            .participants
            .iter()
            .filter(|lane| lane.kind == ParticipantKind::Human)
            .count(),
        1
    );
    assert!(
        frame
            .participants
            .iter()
            .all(|lane| lane.address_id != "operator:local")
    );
    let projected_reply = frame
        .messages
        .iter()
        .find(|row| row.message_id == reply["message_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(projected_reply.legs[0].to, HUMAN);
    assert!(
        frame.participants[0]
            .waiting_on
            .contains(&"@planner".to_owned())
    );
    let history = store.dashboard_history(&ns, None, 100).unwrap().unwrap();
    assert!(
        history
            .legs
            .iter()
            .any(|leg| leg.message_id == projected_reply.message_id && leg.address_id == HUMAN)
    );
    for index in 0..151 {
        f.agent_send(
            "w1",
            "pane-1-lead",
            &format!("window-{index}"),
            "@planner later",
        );
    }
    let rows = store.dashboard_rows(&ns).unwrap();
    assert!(rows.messages.iter().all(|row| !row.author_is_human));
    let frame = build(&rows, None, 42);
    assert_eq!(frame.participants[0].address_id, HUMAN);
    assert_eq!(
        frame
            .participants
            .iter()
            .filter(|lane| lane.kind == ParticipantKind::Human)
            .count(),
        1
    );
    drop(store);
    let mut reopened = StateStore::open(&f.state).unwrap();
    let frame = build(&reopened.dashboard_rows(&ns).unwrap(), None, 42);
    assert_eq!(frame.participants[0].address_id, HUMAN);
    let replay =
        send_operator_message("thread-first", "@planner investigate", None, &runtime).unwrap();
    assert_eq!(replay["message_id"], sent["message_id"]);
    let remote =
        send_operator_message("projection-remote", "#ws2 @tester inspect", None, &runtime).unwrap();
    let envelope = f.herdr.last_envelope("pane-2-tester");
    f.reply_to("w2", "pane-2-tester", &envelope, "@operator remote-answer");
    let remote_ns = StateNamespace::for_space("socket-a", "w2");
    let frame = build(&reopened.dashboard_rows(&remote_ns).unwrap(), None, 42);
    assert_eq!(
        frame
            .participants
            .iter()
            .filter(|lane| lane.kind == ParticipantKind::Human)
            .count(),
        1
    );
    assert!(
        frame
            .messages
            .iter()
            .any(|row| row.legs.iter().any(|leg| leg.to == HUMAN))
    );
    assert!(frame.messages.iter().any(|row| row.author == HUMAN));
    assert!(remote["message_id"].is_string());
}

#[test]
fn duplicate_workspace_labels_and_dashboard_hash_targets_refuse_without_messages() {
    use namedrop::dashboard::snapshot::SpaceKey;
    let f = Fixture::new();
    f.register_all();
    let mut runtime = f.runtime("w1", "unused");
    runtime.current_pane_id = None;
    f.herdr.labels.lock().unwrap()[2].1 = "ws2".to_owned();
    let before = f.message_rows();
    let error =
        send_operator_message("duplicate-label", "#ws2 @lead inspect", None, &runtime).unwrap_err();
    assert!(
        error.to_string().contains("w2") && error.to_string().contains("w3"),
        "{error}"
    );
    let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w2"));
    assert!(
        send_operator_message(
            "hash-target",
            &format!("#{} @lead inspect", key.as_str()),
            None,
            &runtime
        )
        .is_err()
    );
    assert_eq!(f.message_rows(), before);
}

#[test]
fn operator_identity_survives_reopen_and_refuses_pane_lifecycle_mutations() {
    use namedrop::{
        AgentRuntime, AgentSessionId, PaneAddressDraft, PaneRoleId, execute_lifecycle_operation,
    };
    let f = Fixture::new();
    f.register_all();
    let mut runtime = f.runtime("w1", "unused");
    runtime.current_pane_id = None;
    let sent = send_operator_message("guard-create", "@planner inspect", None, &runtime).unwrap();
    let ns = StateNamespace::for_space("socket-a", "w1");
    let id = PaneAddressId::new("operator:local");
    let attribution = MutationAttribution::new(ActorKind::Human, "test", 42);
    let binding = PaneBindingId::new("forbidden-binding");
    macro_rules! refuses {
        ($mutation:expr) => {
            assert!(
                StateStore::open(&f.state)
                    .unwrap()
                    .transaction($mutation)
                    .unwrap_err()
                    .to_string()
                    .contains("operator")
            );
        };
    }
    refuses!(|tx| tx.bind_pane(&ns, &binding, &id, "pane-1-lead", &attribution));
    refuses!(|tx| tx.rebind_pane(&ns, &binding, &id, "pane-1-lead", &attribution));
    refuses!(|tx| tx.unbind_pane(&ns, &id, &attribution));
    refuses!(|tx| tx.retire_pane_address(&ns, &id, &attribution));
    refuses!(|tx| tx.remove_pane_address(&ns, &id, &attribution));
    refuses!(|tx| tx.cancel_and_remove_pane_address(&ns, &id, &attribution));
    refuses!(|tx| tx.replace_alias(&ns, &id, "other", &attribution));
    refuses!(|tx| tx.set_address_persona(&ns, &id, Some("Changed.")));
    refuses!(|tx| tx.set_address_collaboration_disabled(&ns, &id, true, &attribution));
    refuses!(|tx| tx.observe_agent_session(
        &ns,
        "forbidden-observation",
        &id,
        &AgentSessionId::new("session"),
        "idle",
        &attribution
    ));
    refuses!(|tx| tx.issue_integration_challenge(
        &ns,
        "forbidden-registration",
        &id,
        AgentRuntime::Codex,
        "nonce",
        &attribution
    ));
    for (address, role) in [
        ("operator:local", "other-role"),
        ("other-address", "role-operator"),
    ] {
        refuses!(|tx| tx.create_pane_address(
            &ns,
            &PaneAddressDraft {
                id: PaneAddressId::new(address),
                role_id: PaneRoleId::new(role),
                role_name: "other".to_owned(),
                role_description: "other".to_owned(),
                alias: "other".to_owned(),
            },
            &attribution
        ));
    }
    for (operation, input) in [
        (
            "address.rename",
            json!({"address_id":"operator:local", "alias":"other"}),
        ),
        (
            "address.persona",
            json!({"address_id":"operator:local", "description":"other"}),
        ),
        ("address.retire", json!({"address_id":"operator:local"})),
        ("address.remove", json!({"address_id":"operator:local"})),
        (
            "address.bind",
            json!({"address_id":"operator:local", "pane_id":"pane-1-lead"}),
        ),
    ] {
        assert!(
            execute_lifecycle_operation(operation, input, &f.runtime("w1", "pane-1-lead")).is_err()
        );
    }
    assert_eq!(
        send_operator_message("guard-create", "@planner inspect", None, &runtime).unwrap()["message_id"],
        sent["message_id"]
    );
    let db = f.db();
    let stored: (String, i64, i64, String, Option<String>, Option<String>) = db.query_row(
        "SELECT role_id, retired, collaboration_disabled, pane_type, persona, remote_commands_terminal_id FROM pane_addresses WHERE worktree_id='w1' AND address_id='operator:local'",
        [], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?)),
    ).unwrap();
    assert_eq!(
        stored,
        (
            "role-operator".to_owned(),
            0,
            0,
            "agent".to_owned(),
            None,
            None
        )
    );
    for table in [
        "alias_claims",
        "pane_bindings",
        "agent_session_observations",
        "integration_registrations",
    ] {
        assert_eq!(
            f.count(&format!(
                "SELECT COUNT(*) FROM {table} WHERE address_id='operator:local'"
            )),
            0
        );
    }
}

#[test]
fn conflicting_reserved_identity_is_never_adopted_or_rewritten() {
    for conflict in [
        "role",
        "address",
        "shared-role",
        "retired",
        "bound",
        "terminal",
    ] {
        let f = Fixture::new();
        f.register_all();
        let db = f.db();
        db.execute("INSERT INTO pane_roles (session_socket_id,worktree_id,role_id,name,description) VALUES ('socket-a','w1','role-operator',?1,?2)",
            if conflict == "role" { ["wrong", "wrong"] } else { ["operator", "Paneless local operator."] }).unwrap();
        let address = if conflict == "shared-role" {
            "someone-else"
        } else {
            "operator:local"
        };
        let role = if conflict == "address" {
            "role-w1-lead"
        } else {
            "role-operator"
        };
        db.execute("INSERT INTO pane_addresses (session_socket_id,worktree_id,address_id,role_id,retired,collaboration_disabled,created_at_ms,pane_type) VALUES ('socket-a','w1',?1,?2,?3,0,1,?4)",
            rusqlite::params![address, role, conflict == "retired", if conflict == "terminal" { "terminal" } else { "agent" }]).unwrap();
        if conflict == "bound" {
            db.execute("INSERT INTO pane_bindings (session_socket_id,worktree_id,binding_id,address_id,pane_id,active,bound_at_ms) VALUES ('socket-a','w1','bad-binding','operator:local','bad-pane',1,1)", []).unwrap();
        }
        let before = f.message_rows();
        let mut runtime = f.runtime("w1", "unused");
        runtime.current_pane_id = None;
        assert!(
            send_operator_message("conflict", "@planner inspect", None, &runtime)
                .unwrap_err()
                .to_string()
                .contains("operator"),
            "{conflict}"
        );
        assert_eq!(f.message_rows(), before);
        assert_eq!(
            f.count("SELECT COUNT(*) FROM pane_addresses WHERE role_id='role-operator'"),
            if conflict == "address" { 0 } else { 1 }
        );
    }
}

#[test]
fn conflicting_operator_identity_keeps_home_and_envoy_projection_in_agent_lanes() {
    let conflicts = [
        "UPDATE pane_roles SET name='wrong' WHERE role_id='role-operator'",
        "UPDATE pane_roles SET description='wrong' WHERE role_id='role-operator'",
        "UPDATE pane_addresses SET role_id='role-w1-lead' WHERE address_id='operator:local'",
        "UPDATE pane_addresses SET retired=1 WHERE address_id='operator:local'",
        "UPDATE pane_addresses SET collaboration_disabled=1 WHERE address_id='operator:local'",
        "UPDATE pane_addresses SET pane_type='terminal' WHERE address_id='operator:local'",
        "UPDATE pane_addresses SET remote_commands_terminal_id='terminal' WHERE address_id='operator:local'",
        "UPDATE pane_addresses SET persona='legacy' WHERE address_id='operator:local'",
        "INSERT INTO pane_addresses (session_socket_id,worktree_id,address_id,role_id,retired,collaboration_disabled,created_at_ms) VALUES ('socket-a','w1','shared','role-operator',0,0,1)",
        "INSERT INTO alias_claims (session_socket_id,worktree_id,alias,address_id,claim_state) VALUES ('socket-a','w1','legacy','operator:local','tombstone')",
        "INSERT INTO pane_bindings (session_socket_id,worktree_id,binding_id,address_id,pane_id,active,bound_at_ms) VALUES ('socket-a','w1','legacy','operator:local','legacy',0,1)",
        "INSERT INTO agent_session_observations (session_socket_id,worktree_id,observation_id,address_id,agent_session_id,status,observed_at_ms) VALUES ('socket-a','w1','legacy','operator:local','legacy','idle',1)",
        "INSERT INTO integration_registrations (session_socket_id,worktree_id,registration_id,address_id,agent_session_id,runtime,protocol_version,capability_profile,active) SELECT session_socket_id,worktree_id,'legacy','operator:local',agent_session_id,runtime,protocol_version,capability_profile,0 FROM integration_registrations WHERE worktree_id='w1' AND address_id='address-w1-lead'",
    ];
    for conflict in conflicts {
        let f = Fixture::new();
        f.register_all();
        f.sql("INSERT INTO pane_roles (session_socket_id,worktree_id,role_id,name,description) VALUES ('socket-a','w1','role-operator','operator','Paneless local operator.');
            INSERT INTO pane_addresses (session_socket_id,worktree_id,address_id,role_id,retired,collaboration_disabled,created_at_ms) VALUES ('socket-a','w1','operator:local','role-operator',0,0,1);
            INSERT INTO pane_addresses (session_socket_id,worktree_id,address_id,role_id,retired,collaboration_disabled,created_at_ms) VALUES ('socket-a','w2','legacy-envoy','role-w2-lead',0,0,1);
            INSERT INTO space_envoys (session_socket_id,worktree_id,address_id,origin_space_id,origin_address_id,origin_alias,origin_label,host_label) VALUES ('socket-a','w2','legacy-envoy','w1','operator:local','operator','ws1','ws2');");
        f.sql(conflict);
        let runtime = f.runtime("w1", "unused");
        assert!(
            send_operator_message("conflict", "@planner inspect", None, &runtime)
                .unwrap_err()
                .to_string()
                .contains("operator"),
            "{conflict}"
        );
        assert_eq!(f.message_rows(), (0, 0), "{conflict}");
        let mut store = StateStore::open(&f.state).unwrap();
        let home = store
            .dashboard_rows(&StateNamespace::for_space("socket-a", "w1"))
            .unwrap();
        assert_eq!(
            home.addresses
                .iter()
                .filter(|row| row.address_id == "operator:local")
                .count(),
            1,
            "conflicting home identity must remain an ordinary record: {conflict}"
        );
        for workspace in ["w1", "w2"] {
            let frame = namedrop::dashboard::snapshot::build(
                &store
                    .dashboard_rows(&StateNamespace::for_space("socket-a", workspace))
                    .unwrap(),
                None,
                42,
            );
            assert_eq!(
                frame
                    .participants
                    .iter()
                    .filter(|lane| lane.address_id == "human")
                    .count(),
                0,
                "conflicting identity must not create a human lane in {workspace}: {conflict}"
            );
        }
    }
}

#[test]
fn conflicting_operator_pane_receives_ordinary_alias_send_without_silent_settlement() {
    let f = Fixture::new();
    f.register_all();
    f.sql("INSERT INTO pane_roles (session_socket_id,worktree_id,role_id,name,description) VALUES ('socket-a','w1','role-operator','wrong','legacy pane');
        INSERT INTO pane_addresses (session_socket_id,worktree_id,address_id,role_id,retired,collaboration_disabled,created_at_ms) VALUES ('socket-a','w1','operator:local','role-operator',0,0,1);
        UPDATE alias_claims SET address_id='operator:local' WHERE worktree_id='w1' AND address_id='address-w1-lead';
        UPDATE pane_bindings SET address_id='operator:local' WHERE worktree_id='w1' AND address_id='address-w1-lead';
        UPDATE agent_session_observations SET address_id='operator:local' WHERE worktree_id='w1' AND address_id='address-w1-lead';
        UPDATE integration_registrations SET address_id='operator:local' WHERE worktree_id='w1' AND address_id='address-w1-lead';");
    assert!(
        send_operator_message(
            "conflict",
            "@planner inspect",
            None,
            &f.runtime("w1", "unused")
        )
        .is_err()
    );
    f.herdr.prompts.lock().unwrap().clear();
    let sent = f.agent_send("w1", "pane-1-planner", "legacy-send", "@lead inspect");
    assert_eq!(
        f.delivery_states("w1", &sent["message_id"]),
        ["acknowledged"],
        "the conflicting reserved address is an ordinary pane recipient"
    );
    assert_eq!(f.herdr.envelopes("pane-1-lead").len(), 1);
    let mut store = StateStore::open(&f.state).unwrap();
    let namespace = StateNamespace::for_space("socket-a", "w1");
    let frame =
        namedrop::dashboard::snapshot::build(&store.dashboard_rows(&namespace).unwrap(), None, 42);
    let message = frame
        .messages
        .iter()
        .find(|row| row.message_id == sent["message_id"].as_str().unwrap())
        .unwrap();
    assert_eq!(message.legs[0].to, "operator:local");
    let history = store
        .dashboard_history(&namespace, None, 100)
        .unwrap()
        .unwrap();
    assert_eq!(history.legs[0].address_id, "operator:local");
    assert!(
        frame
            .participants
            .iter()
            .any(|lane| lane.address_id == "operator:local")
    );
    assert!(
        frame
            .participants
            .iter()
            .all(|lane| lane.address_id != "human")
    );
}

mod http {
    use super::*;
    use namedrop::HerdrCommandExecutor;
    use namedrop::dashboard::snapshot::SpaceKey;
    use serde_json::Value;
    use std::{
        fs,
        io::{Read, Write},
        net::{Shutdown, TcpStream},
        os::unix::fs::PermissionsExt,
        path::Path,
        process::{Child, Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    struct Server {
        child: Child,
        port: u16,
        cookie: String,
        csrf: String,
    }

    struct Response {
        status: u16,
        head: String,
        body: String,
    }

    impl Server {
        fn start(f: &Fixture, ui: Option<&Path>) -> Self {
            let mut responses = serde_json::Map::new();
            for args in [
                vec!["workspace", "list"],
                vec!["worktree", "list", "--workspace", "w1", "--json"],
                vec!["pane", "list", "--workspace", "w1"],
                vec!["pane", "list", "--workspace", "w2"],
                vec!["pane", "list", "--workspace", "w3"],
            ] {
                responses.insert(
                    args.join(" "),
                    json!(f.herdr.execute(&args).unwrap().stdout),
                );
            }
            let responses_file = f.root.join("http-herdr-responses.json");
            fs::write(&responses_file, serde_json::to_vec(&responses).unwrap()).unwrap();
            let shim = f.root.join("http-herdr");
            fs::write(
                &shim,
                r#"#!/usr/bin/python3
import json, os, sys, time
args = sys.argv[1:]
root = os.path.dirname(__file__)
def wait_for_gate(name):
    gate = os.path.join(root, name + '-gate')
    if os.path.exists(gate):
        open(os.path.join(root, name + '-started'), 'w').close()
        deadline = time.monotonic() + 15
        while os.path.exists(gate):
            if time.monotonic() >= deadline:
                sys.stderr.write('HTTP test gate timed out: ' + name)
                sys.exit(1)
            time.sleep(0.01)
if args == ['workspace', 'list']:
    wait_for_gate('http-discovery')
with open(os.path.join(root, 'http-herdr-responses.json')) as source:
    responses = json.load(source)
key = ' '.join(args)
if key in responses:
    print(responses[key])
elif args[:2] == ['worktree', 'list']:
    sys.stderr.write('{"error":{"code":"not_git_worktree"}}')
    sys.exit(1)
elif args[:2] in [['agent', 'prompt'], ['pane', 'report-metadata'], ['notification', 'show']]:
    if args[:2] == ['agent', 'prompt']:
        wait_for_gate('http-prompt')
    with open(os.path.join(root, 'http-herdr-invocations.jsonl'), 'a') as log:
        log.write(json.dumps({'args': args, 'socket': os.environ.get('HERDR_SOCKET_PATH')}) + '\n')
    print('{}')
else:
    sys.stderr.write('unexpected Herdr invocation: ' + key)
    sys.exit(1)
"#,
            )
            .unwrap();
            fs::set_permissions(&shim, fs::Permissions::from_mode(0o700)).unwrap();
            let mut command = Command::new(env!("CARGO_BIN_EXE_namedrop"));
            command
                .arg("--plugin-state")
                .arg(&f.state)
                .arg("dashboard-server")
                .env("HERDR_BIN_PATH", &shim)
                .env_remove("HERDR_SOCKET_PATH")
                .env_remove("HERDR_WORKSPACE_ID")
                .env_remove("HERDR_PANE_ID")
                .env_remove("HERDR_PLUGIN_STATE_DIR")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            if let Some(ui) = ui {
                command.arg("--ui").arg(ui);
            }
            let mut child = command.spawn().unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            let info = loop {
                if let Ok(bytes) = fs::read(f.state.join("dashboard/server.json"))
                    && let Ok(info) = serde_json::from_slice::<Value>(&bytes)
                    && info["pid"] == child.id()
                {
                    break info;
                }
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!("dashboard did not start");
                }
                thread::sleep(Duration::from_millis(20));
            };
            let port = info["port"].as_u64().unwrap() as u16;
            let session = fs::read_to_string(f.state.join("dashboard/session")).unwrap();
            let mut server = Self {
                child,
                port,
                cookie: format!("namedrop_dashboard_{port}={session}"),
                csrf: String::new(),
            };
            let response = server.request("GET", "/api/session", &[("Cookie", &server.cookie)], "");
            assert_eq!(response.status, 200, "{}", response.body);
            server.csrf = serde_json::from_str::<Value>(&response.body).unwrap()["csrf_token"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(server.csrf.len(), 64);
            server
        }

        fn raw(&self, request: &str) -> Response {
            let mut stream = TcpStream::connect(("127.0.0.1", self.port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            stream.write_all(request.as_bytes()).unwrap();
            stream.shutdown(Shutdown::Write).unwrap();
            let mut bytes = Vec::new();
            stream
                .read_to_end(&mut bytes)
                .expect("reading dashboard response without a reset");
            let raw = String::from_utf8(bytes).unwrap();
            let (head, body) = raw.split_once("\r\n\r\n").unwrap();
            Response {
                status: head.split(' ').nth(1).unwrap().parse().unwrap(),
                head: head.to_owned(),
                body: body.to_owned(),
            }
        }

        fn request(
            &self,
            method: &str,
            path: &str,
            headers: &[(&str, &str)],
            body: &str,
        ) -> Response {
            let mut request = format!("{method} {path} HTTP/1.1\r\n");
            if !headers
                .iter()
                .any(|(key, _)| key.eq_ignore_ascii_case("host"))
            {
                request.push_str(&format!("Host: 127.0.0.1:{}\r\n", self.port));
            }
            for (key, value) in headers {
                request.push_str(&format!("{key}: {value}\r\n"));
            }
            if method == "POST"
                && !headers
                    .iter()
                    .any(|(key, _)| key.eq_ignore_ascii_case("content-length"))
            {
                request.push_str(&format!("Content-Length: {}\r\n", body.len()));
            }
            request.push_str("\r\n");
            request.push_str(body);
            self.raw(&request)
        }

        fn post(&self, path: &str, body: &str, extra: &[(&str, &str)]) -> Response {
            let mut headers = vec![
                ("Cookie", self.cookie.as_str()),
                ("X-Namedrop-Csrf", self.csrf.as_str()),
                ("Content-Type", "application/json"),
            ];
            headers.extend_from_slice(extra);
            self.request("POST", path, &headers, body)
        }
    }

    impl Drop for Server {
        fn drop(&mut self) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }

    #[test]
    #[ignore = "requires an external attack script and output path; runs only against the isolated fake-Herdr server"]
    fn external_security_attack() {
        let script = std::env::var_os("NAMEDROP_SECURITY_ATTACK_SCRIPT")
            .expect("set NAMEDROP_SECURITY_ATTACK_SCRIPT to the Python attack script");
        let output_path = std::env::var_os("NAMEDROP_SECURITY_ATTACK_OUTPUT")
            .expect("set NAMEDROP_SECURITY_ATTACK_OUTPUT to the result JSON path");
        let f = Fixture::new();
        f.register_all();
        f.rename("w1", "planner", "claude");
        let server = Server::start(&f, None);
        let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
        let mut nonce = [0u8; 16];
        fs::File::open("/dev/urandom")
            .unwrap()
            .read_exact(&mut nonce)
            .unwrap();
        let nonce = nonce
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let entries = f.state.join("dashboard/entry");
        fs::create_dir_all(&entries).unwrap();
        fs::set_permissions(&entries, fs::Permissions::from_mode(0o700)).unwrap();
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(entries.join(&nonce))
            .unwrap();
        let entry = format!(
            "http://127.0.0.1:{}/w/{}?n={nonce}",
            server.port,
            key.as_str()
        );
        let output = Command::new("python3")
            .arg(script)
            .arg(entry)
            .arg(key.as_str())
            .arg(f.state.join("namedrop.sqlite3"))
            .arg(&output_path)
            .env("HERDR_BIN_PATH", f.root.join("http-herdr"))
            .env_remove("HERDR_SOCKET_PATH")
            .env_remove("HERDR_WORKSPACE_ID")
            .env_remove("HERDR_PANE_ID")
            .env_remove("HERDR_PLUGIN_STATE_DIR")
            .output()
            .unwrap();
        println!("{}", String::from_utf8_lossy(&output.stdout));
        assert!(
            output.status.success(),
            "attack process failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&fs::read(output_path).unwrap()).unwrap();
        let results = report["results"].as_array().expect("attack report results");
        assert!(!results.is_empty(), "attack script must report cases");
        let failures: Vec<_> = results
            .iter()
            .filter(|result| result["ok"] != true)
            .collect();
        assert!(failures.is_empty(), "failed attacks: {failures:?}");
    }

    #[test]
    fn unrelated_cookies_do_not_block_pages_or_operator_posts() {
        let f = Fixture::new();
        f.register_all();
        let server = Server::start(&f, None);
        let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
        for (i, prefix) in [
            "stray; ",
            "theme=a; theme=b; ",
            "namedrop_dashboard_1=a; namedrop_dashboard_1=b; ",
        ]
        .iter()
        .enumerate()
        {
            let cookie = format!("{prefix}{}", server.cookie);
            let page = server.request("GET", "/", &[("Cookie", &cookie)], "");
            assert_eq!(page.status, 200, "cookie {cookie}: {}", page.body);
            let body = json!({"request_id":format!("cookie-{i}"), "source":"@planner inspect"})
                .to_string();
            let post = server.request(
                "POST",
                &format!("/w/{}/messages", key.as_str()),
                &[
                    ("Cookie", &cookie),
                    ("X-Namedrop-Csrf", &server.csrf),
                    ("Content-Type", "application/json"),
                ],
                &body,
            );
            assert_eq!(post.status, 200, "cookie {cookie}: {}", post.body);
        }
        let duplicate = format!("{}; {}", server.cookie, server.cookie);
        assert_eq!(
            server
                .request("GET", "/", &[("Cookie", &duplicate)], "")
                .status,
            400
        );
    }

    #[test]
    fn early_refusals_are_readable_after_a_large_body() {
        let f = Fixture::new();
        f.register_all();
        let server = Server::start(&f, None);
        let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
        let body = "x".repeat(1024 * 1024);
        for (extra, expected) in [
            ("", 413),
            ("Origin: http://foreign.example\r\n", 403),
            ("Content-Length: 1\r\n", 400),
        ] {
            let request = format!(
                "POST /w/{}/messages HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\nX-Namedrop-Csrf: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n{extra}\r\n{body}",
                key.as_str(),
                server.port,
                server.cookie,
                server.csrf,
                body.len()
            );
            let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(10)))
                .unwrap();
            stream
                .write_all(request.as_bytes())
                .expect("send the entire large request");
            let mut response = Vec::new();
            stream
                .read_to_end(&mut response)
                .expect("read refusal without connection reset");
            let response = String::from_utf8(response).unwrap();
            assert!(
                response.starts_with(&format!("HTTP/1.1 {expected} ")),
                "{response}"
            );
            assert!(response.contains("Connection: close\r\n"));
        }
        assert_eq!(f.message_rows(), (0, 0));
    }

    #[test]
    fn operator_input_errors_remain_useful_bad_requests() {
        let f = Fixture::new();
        f.register_all();
        let server = Server::start(&f, None);
        let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
        let path = format!("/w/{}/messages", key.as_str());
        for (source, reply, expected) in [
            ("plain text", None, "Mention"),
            ("@ghost inspect", None, "ghost"),
            ("#missing @planner inspect", None, "missing"),
            ("@planner inspect", Some("missing-message"), "message"),
        ] {
            let response = server.post(
                &path,
                &json!({"request_id":source,"source":source,"in_reply_to":reply}).to_string(),
                &[],
            );
            assert_eq!(response.status, 400, "{}", response.body);
            assert!(response.body.contains(expected), "{}", response.body);
        }
    }

    #[test]
    fn herdr_discovery_errors_are_safe_service_failures() {
        let f = Fixture::new();
        f.register_all();
        let server = Server::start(&f, None);
        let responses = f.root.join("http-herdr-responses.json");
        let mut values: Value = serde_json::from_slice(&fs::read(&responses).unwrap()).unwrap();
        let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
        let path = format!("/w/{}/messages", key.as_str());
        for command in ["workspace list", "worktree list --workspace w1 --json"] {
            let original = values[command].clone();
            values[command] = json!("secret-herdr-diagnostic-/private/runtime");
            fs::write(&responses, serde_json::to_vec(&values).unwrap()).unwrap();
            let response = server.post(
                &path,
                &json!({"request_id":command,"source":"#all @lead inspect"}).to_string(),
                &[],
            );
            assert_eq!(response.status, 503, "{}", response.body);
            assert_eq!(
                serde_json::from_str::<Value>(&response.body).unwrap(),
                json!({"error":"Message service is temporarily unavailable."})
            );
            values[command] = original;
        }
        assert_eq!(f.message_rows(), (0, 0));
    }

    #[test]
    fn reserved_operator_conflicts_are_safe_internal_failures() {
        let f = Fixture::new();
        f.register_all();
        f.sql("INSERT INTO pane_roles (session_socket_id,worktree_id,role_id,name,description) VALUES ('socket-a','w1','role-operator','secret-wrong-role','private-detail');");
        let server = Server::start(&f, None);
        let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
        let response = server.post(
            &format!("/w/{}/messages", key.as_str()),
            &json!({"request_id":"internal","source":"@planner inspect"}).to_string(),
            &[],
        );
        assert_eq!(response.status, 500, "{}", response.body);
        assert_eq!(
            serde_json::from_str::<Value>(&response.body).unwrap(),
            json!({"error":"Message could not be sent."})
        );
        assert_eq!(f.message_rows(), (0, 0));
    }

    #[test]
    fn store_busy_errors_are_safe_service_failures() {
        let f = Fixture::new();
        f.register_all();
        let server = Server::start(&f, None);
        let db = f.db();
        db.execute_batch("BEGIN IMMEDIATE").unwrap();
        let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
        let response = server.post(
            &format!("/w/{}/messages", key.as_str()),
            &json!({"request_id":"busy","source":"@planner inspect"}).to_string(),
            &[],
        );
        db.execute_batch("ROLLBACK").unwrap();
        assert_eq!(response.status, 503, "{}", response.body);
        assert!(!response.body.contains("database"), "{}", response.body);
        assert_eq!(f.message_rows(), (0, 0));
    }

    struct Gate(std::path::PathBuf);

    impl Gate {
        fn new(f: &Fixture, name: &str) -> Self {
            let path = f.root.join(format!("{name}-gate"));
            fs::write(&path, b"").unwrap();
            Self(path)
        }
    }

    impl Drop for Gate {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    fn wait_until(description: &str, ready: impl Fn() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while !ready() {
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {description}"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn prompts(f: &Fixture) -> Vec<Value> {
        fs::read_to_string(f.root.join("http-herdr-invocations.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|call| call["args"][0] == "agent")
            .collect()
    }

    fn wait_for_submitted(f: &Fixture, count: i64) {
        wait_until("recorded prompt submission", || {
            prompts(f).len() == count as usize
                && f.count("SELECT COUNT(*) FROM delivery_attempt_state WHERE state IN ('submitted','acknowledged')") == count
        });
    }

    fn delivery_ids(receipt: &Value) -> Vec<&Value> {
        receipt["deliveries"]
            .as_array()
            .unwrap()
            .iter()
            .map(|delivery| &delivery["delivery_id"])
            .collect()
    }

    fn post_returns_before_prompt(source: &str, workspace: &str, pane: &str) {
        let f = Fixture::new();
        f.register_all();
        let server = Server::start(&f, None);
        let gate = Gate::new(&f, "http-prompt");
        let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
        let path = format!("/w/{}/messages", key.as_str());
        let body = json!({"request_id":"http-gated", "source":source}).to_string();
        let expected_rows = if workspace == "w1" { 1 } else { 2 };
        let receipt = thread::scope(|scope| {
            let (send, receive) = std::sync::mpsc::channel();
            let (server, path, body) = (&server, &path, &body);
            scope.spawn(move || send.send(server.post(path, body, &[])).unwrap());
            wait_until("blocked Herdr prompt", || {
                f.root.join("http-prompt-started").exists()
            });
            let response = receive.recv_timeout(Duration::from_secs(1));
            if response.is_err() {
                drop(gate);
                let _ = receive.recv_timeout(Duration::from_secs(5));
                panic!("POST did not reach EOF while the Herdr prompt gate stayed shut");
            }
            let response = response.unwrap();
            assert_eq!(response.status, 200, "{}", response.body);
            let receipt: Value = serde_json::from_str(&response.body).unwrap();
            assert!(receipt.get("drain").is_none());
            assert!(receipt.get("drains").is_none());
            let message_id = receipt["message_id"].as_str().unwrap();
            let stored: String = f.db().query_row(
                "SELECT m.message_id FROM collaboration_messages m JOIN idempotency_records i
                 ON i.session_socket_id=m.session_socket_id AND i.worktree_id=m.worktree_id
                 AND i.message_id=m.message_id WHERE i.request_id='http-gated' AND i.worktree_id='w1'",
                [], |row| row.get(0),
            ).unwrap();
            assert_eq!(stored, message_id);
            assert_eq!(
                f.count("SELECT COUNT(*) FROM mailbox_deliveries"),
                expected_rows
            );
            assert!(prompts(&f).is_empty());
            let replay = server.post(path, body, &[]);
            assert_eq!(replay.status, 200, "{}", replay.body);
            let replay: Value = serde_json::from_str(&replay.body).unwrap();
            assert_eq!(replay["message_id"], receipt["message_id"]);
            assert_eq!(replay["thread_id"], receipt["thread_id"]);
            assert_eq!(delivery_ids(&replay), delivery_ids(&receipt));
            assert_eq!(replay["forwarded"], receipt["forwarded"]);
            assert_eq!(replay["idempotent_replay"], true);
            assert!(f.root.join("http-prompt-gate").exists());
            assert!(prompts(&f).is_empty());
            drop(gate);
            receipt
        });
        wait_for_submitted(&f, 1);
        let prompts = prompts(&f);
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0]["args"][2], pane);
        assert_eq!(prompts[0]["socket"], "socket-a");
        let envelope: Value =
            serde_json::from_str(prompts[0]["args"][3].as_str().unwrap()).unwrap();
        assert_eq!(
            &envelope["control"]["message_id"],
            if workspace == "w1" {
                &receipt["message_id"]
            } else {
                &receipt["forwarded"][0]["message_id"]
            }
        );
        assert_eq!(f.count("SELECT COUNT(*) FROM delivery_attempts"), 1);
        assert_eq!(
            f.count("SELECT COUNT(*) FROM mailbox_deliveries"),
            expected_rows
        );
        let stored_workspace: String = f
            .db()
            .query_row(
                "SELECT worktree_id FROM mailbox_deliveries WHERE state IN ('submitted','acknowledged')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(stored_workspace, workspace);
        assert_eq!(
            f.count("SELECT COUNT(*) FROM collaboration_messages"),
            expected_rows
        );
    }

    #[test]
    fn post_reaches_eof_before_local_prompt_returns() {
        post_returns_before_prompt("@planner inspect", "w1", "pane-1-planner");
    }

    #[test]
    fn post_reaches_eof_before_forwarded_prompt_returns() {
        post_returns_before_prompt("#ws2 @tester inspect", "w2", "pane-2-tester");
    }

    #[test]
    fn disconnected_post_still_drains_home_and_forwarded_workspaces() {
        use std::os::fd::AsRawFd;
        let f = Fixture::new();
        f.register_all();
        let server = Server::start(&f, None);
        let gate = Gate::new(&f, "http-discovery");
        let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
        let body =
            json!({"request_id":"http-reset", "source":"#ws1 #ws2 @planner @tester inspect"})
                .to_string();
        let request = format!(
            "POST /w/{}/messages HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\nX-Namedrop-Csrf: {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            key.as_str(),
            server.port,
            server.cookie,
            server.csrf,
            body.len(),
        );
        let mut stream = TcpStream::connect(("127.0.0.1", server.port)).unwrap();
        stream.write_all(request.as_bytes()).unwrap();
        wait_until("Herdr discovery before commit", || {
            f.root.join("http-discovery-started").exists()
        });
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
                    (&linger as *const libc::linger).cast(),
                    std::mem::size_of_val(&linger) as libc::socklen_t,
                )
            },
            0
        );
        drop(stream);
        drop(gate);
        wait_for_submitted(&f, 2);
        let calls = prompts(&f);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["args"][2], "pane-1-planner");
        assert_eq!(calls[1]["args"][2], "pane-2-tester");
        assert_eq!(f.count("SELECT COUNT(*) FROM delivery_attempts"), 2);
        assert_eq!(f.count("SELECT COUNT(*) FROM collaboration_messages"), 2);
        assert_eq!(
            f.count("SELECT COUNT(*) FROM idempotency_records WHERE request_id='http-reset'"),
            1
        );
    }

    #[test]
    fn concurrent_identical_posts_append_and_deliver_once() {
        let f = Fixture::new();
        f.register_all();
        let server = Server::start(&f, None);
        let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
        let path = format!("/w/{}/messages", key.as_str());
        let body = json!({"request_id":"http-concurrent", "source":"@planner inspect"}).to_string();
        let barrier = std::sync::Barrier::new(2);
        let responses = thread::scope(|scope| {
            let post = || {
                barrier.wait();
                server.post(&path, &body, &[])
            };
            let first = scope.spawn(post);
            let second = scope.spawn(post);
            [first.join().unwrap(), second.join().unwrap()]
        });
        let receipts = responses.map(|response| {
            assert_eq!(response.status, 200, "{}", response.body);
            serde_json::from_str::<Value>(&response.body).unwrap()
        });
        assert_eq!(receipts[0]["message_id"], receipts[1]["message_id"]);
        assert_eq!(receipts[0]["thread_id"], receipts[1]["thread_id"]);
        assert_eq!(delivery_ids(&receipts[0]), delivery_ids(&receipts[1]));
        assert_eq!(
            receipts
                .iter()
                .filter(|receipt| receipt["idempotent_replay"] == true)
                .count(),
            1
        );
        assert_eq!(f.count("SELECT COUNT(*) FROM collaboration_messages"), 1);
        assert_eq!(f.count("SELECT COUNT(*) FROM mailbox_deliveries"), 1);
        wait_for_submitted(&f, 1);
        assert_eq!(f.count("SELECT COUNT(*) FROM delivery_attempts"), 1);
        let invocations = fs::read_to_string(f.root.join("http-herdr-invocations.jsonl")).unwrap();
        let prompts: Vec<Value> = invocations
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|call| call["args"][0] == "agent")
            .collect();
        assert_eq!(prompts.len(), 1);
        let envelope: Value =
            serde_json::from_str(prompts[0]["args"][3].as_str().unwrap()).unwrap();
        assert_eq!(envelope["control"]["message_id"], receipts[0]["message_id"]);
    }

    #[test]
    fn valid_csrf_post_uses_url_namespace_and_replays_with_bounded_strict_json() {
        let f = Fixture::new();
        f.register_all();
        let server = Server::start(&f, None);
        let key = SpaceKey::of(&StateNamespace::for_space("socket-a", "w1"));
        let path = format!("/w/{}/messages", key.as_str());
        let page = server.request(
            "GET",
            &format!("/w/{}", key.as_str()),
            &[("Cookie", &server.cookie)],
            "",
        );
        assert_eq!(page.status, 200);
        assert!(
            page.head
                .contains(&format!("X-Namedrop-Csrf: {}\r\n", server.csrf))
        );
        assert_eq!(
            page.body,
            include_str!("../src/dashboard/assets/index.html")
        );
        assert_eq!(server.request("GET", "/api/session", &[], "").status, 401);
        let body = json!({"request_id":"http-first", "source":"@planner inspect"}).to_string();
        let origin = format!("http://127.0.0.1:{}", server.port);
        let first = server.post(&path, &body, &[("Origin", &origin)]);
        assert_eq!(first.status, 200, "{}", first.body);
        let first: Value = serde_json::from_str(&first.body).unwrap();
        let replay = server.post(&path, &body, &[]);
        assert_eq!(replay.status, 200, "{}", replay.body);
        let replay: Value = serde_json::from_str(&replay.body).unwrap();
        assert_eq!(first["message_id"], replay["message_id"]);
        assert_eq!(replay["idempotent_replay"], true);
        let stored: (String, String, Option<String>) = f.db().query_row(
            "SELECT worktree_id,author_kind,author_session_snapshot FROM collaboration_messages WHERE message_id=?1",
            [first["message_id"].as_str().unwrap()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?)),
        ).unwrap();
        assert_eq!(stored, ("w1".to_owned(), "human".to_owned(), None));
        wait_for_submitted(&f, 1);
        let invocations = fs::read_to_string(f.root.join("http-herdr-invocations.jsonl")).unwrap();
        let prompts: Vec<Value> = invocations
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .filter(|call| call["args"][0] == "agent")
            .collect();
        assert_eq!(prompts.len(), 1);
        assert_eq!(prompts[0]["socket"], "socket-a");
        assert_eq!(prompts[0]["args"][2], "pane-1-planner");
        let followup = server.post(
            &path,
            &json!({
                "request_id": "http-followup", "source": "@planner continue",
                "in_reply_to": first["message_id"],
            })
            .to_string(),
            &[],
        );
        assert_eq!(followup.status, 200, "{}", followup.body);
        let followup: Value = serde_json::from_str(&followup.body).unwrap();
        assert_eq!(followup["thread_id"], first["thread_id"]);
        let before = f.message_rows();
        assert_eq!(
            server
                .post(
                    &path,
                    &json!({"request_id":"http-first", "source":"@planner changed"}).to_string(),
                    &[]
                )
                .status,
            400
        );
        let ghost = server.post(
            &path,
            &json!({"request_id":"http-ghost", "source":"@ghost inspect"}).to_string(),
            &[],
        );
        assert_eq!(ghost.status, 400);
        assert!(ghost.body.contains("ghost"));
        for body in [
            json!({"request_id":"empty-source", "source":" \t"}).to_string(),
            json!({"request_id":" ", "source":"@planner inspect"}).to_string(),
            json!({"request_id":"x".repeat(129), "source":"@planner inspect"}).to_string(),
            json!({"request_id":"bad-target", "source":"@planner inspect", "in_reply_to":"x".repeat(129)}).to_string(),
            json!({"request_id":"bad-target", "source":"@planner inspect", "in_reply_to":" "}).to_string(),
            "{\"request_id\":\"duplicate\",\"request_id\":\"other\",\"source\":\"@planner inspect\"}".to_owned(),
            "[]".to_owned(), "{".to_owned(),
        ] {
            assert_eq!(server.post(&path, &body, &[]).status, 400, "{body}");
        }
        for field in [
            "socket",
            "workspace",
            "author",
            "author_kind",
            "context",
            "attempt_id",
            "tracking",
        ] {
            let mut body =
                json!({"request_id":format!("unknown-{field}"), "source":"@planner inspect"});
            body[field] = json!("chosen-by-body");
            assert_eq!(
                server.post(&path, &body.to_string(), &[]).status,
                400,
                "{field}"
            );
        }
        let huge = json!({"request_id":"huge", "source":"x".repeat(65536)}).to_string();
        assert_eq!(server.post(&path, &huge, &[]).status, 413);
        assert_eq!(
            server
                .post("/w/00000000000000000000/messages", &body, &[])
                .status,
            404
        );
        assert_eq!(f.message_rows(), before);
    }

    #[test]
    fn csrf_origin_host_and_framing_refuse_before_any_database_is_opened() {
        let mut f = Fixture::new();
        let empty_state = f.root.join("empty-state");
        fs::create_dir(&empty_state).unwrap();
        f.state = empty_state;
        let server = Server::start(&f, None);
        let path = "/w/00000000000000000000/messages";
        let body = r#"{"request_id":"refused","source":"@planner inspect"}"#;
        assert_eq!(
            server
                .request(
                    "POST",
                    path,
                    &[
                        ("Cookie", &server.cookie),
                        ("Content-Type", "application/json")
                    ],
                    body
                )
                .status,
            403
        );
        assert_eq!(
            server
                .request(
                    "POST",
                    path,
                    &[
                        ("X-Namedrop-Csrf", &server.csrf),
                        ("Content-Type", "application/json")
                    ],
                    body
                )
                .status,
            401
        );
        assert_eq!(
            server
                .post(path, body, &[("Origin", "https://foreign.example")])
                .status,
            403
        );
        assert_eq!(server.post(path, body, &[("Origin", "null")]).status, 403);
        assert_eq!(
            server
                .post(
                    path,
                    body,
                    &[("Origin", &format!("http://localhost:{}", server.port))]
                )
                .status,
            403
        );
        assert_eq!(
            server
                .post(path, body, &[("Host", "attacker.example")])
                .status,
            403
        );
        assert_eq!(
            server
                .post(path, body, &[("Sec-Fetch-Site", "cross-site")])
                .status,
            403
        );
        assert_eq!(
            server
                .post(path, body, &[("Sec-Fetch-Site", "same-site")])
                .status,
            403
        );
        assert_eq!(
            server
                .request(
                    "POST",
                    path,
                    &[
                        ("Cookie", &server.cookie),
                        ("X-Namedrop-Csrf", "wrong"),
                        ("Content-Type", "application/json")
                    ],
                    body
                )
                .status,
            403
        );
        for name in [
            "Host",
            "Cookie",
            "X-Namedrop-Csrf",
            "Origin",
            "Content-Length",
            "Content-Type",
            "Sec-Fetch-Site",
        ] {
            let request = format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nCookie: {}\r\nX-Namedrop-Csrf: {}\r\nContent-Type: application/json\r\nOrigin: http://127.0.0.1:{}\r\nContent-Length: {}\r\nSec-Fetch-Site: same-origin\r\n{name}: duplicate\r\n\r\n{body}",
                server.port,
                server.cookie,
                server.csrf,
                server.port,
                body.len()
            );
            assert_eq!(server.raw(&request).status, 400, "duplicate {name}");
        }
        for extra in [
            "Transfer-Encoding: chunked\r\n",
            "Expect: 100-continue\r\n",
            " folded: value\r\n",
            "Bad Name: value\r\n",
            "Host : value\r\n",
        ] {
            assert_eq!(
                server
                    .post(path, body, &[("X-Injected", &format!("value\r\n{extra}"))])
                    .status,
                400,
                "{extra}"
            );
        }
        let auth = format!(
            "Host: 127.0.0.1:{}\r\nCookie: {}\r\nX-Namedrop-Csrf: {}\r\nContent-Type: application/json\r\n",
            server.port, server.cookie, server.csrf
        );
        for length in ["-1", "+1", "12x", "", "0x10"] {
            assert_eq!(
                server
                    .raw(&format!(
                        "POST {path} HTTP/1.1\r\n{auth}Content-Length: {length}\r\n\r\n{body}"
                    ))
                    .status,
                400,
                "{length}"
            );
        }
        assert_eq!(
            server
                .raw(&format!("POST {path} HTTP/1.1\r\n{auth}\r\n{body}"))
                .status,
            400
        );
        assert_eq!(
            server
                .raw(&format!(
                    "POST {path} HTTP/1.1\r\n{auth}Content-Length: 65537\r\n\r\n"
                ))
                .status,
            413
        );
        assert_eq!(
            server
                .raw(&format!(
                    "POST {path} HTTP/1.1\r\n{auth}Content-Length: 100\r\n\r\n{{}}"
                ))
                .status,
            400
        );
        assert_eq!(
            server
                .raw(&format!(
                    "POST {path} HTTP/1.1\r\n{auth}X-Padding: {}\r\n\r\n",
                    "x".repeat(8192)
                ))
                .status,
            400
        );
        assert_eq!(
            server
                .request(
                    "POST",
                    path,
                    &[
                        ("Cookie", &server.cookie),
                        ("X-Namedrop-Csrf", &server.csrf)
                    ],
                    body
                )
                .status,
            400
        );
        assert_eq!(
            server
                .request(
                    "POST",
                    path,
                    &[
                        ("Cookie", &server.cookie),
                        ("X-Namedrop-Csrf", &server.csrf),
                        ("Content-Type", "text/plain")
                    ],
                    body
                )
                .status,
            400
        );
        let duplicate_cookie = format!("{}; {}", server.cookie, server.cookie);
        assert_eq!(
            server
                .request(
                    "POST",
                    path,
                    &[
                        ("Cookie", &duplicate_cookie),
                        ("X-Namedrop-Csrf", &server.csrf),
                        ("Content-Type", "application/json")
                    ],
                    body
                )
                .status,
            400
        );
        assert_eq!(server.post(path, body, &[]).status, 404);
        assert!(
            !f.state.join("namedrop.sqlite3").exists(),
            "invalid requests must not initialize the database"
        );
    }

    #[test]
    fn custom_index_bytes_are_unchanged_and_session_tokens_are_distinct() {
        let f = Fixture::new();
        let ui = f.root.join("ui");
        fs::create_dir(&ui).unwrap();
        let asset = "<!doctype html>\n<script src=\"/app.js\"></script>\n";
        fs::write(ui.join("index.html"), asset).unwrap();
        let server = Server::start(&f, Some(&ui));
        let response = server.request("GET", "/", &[("Cookie", &server.cookie)], "");
        assert_eq!(response.status, 200);
        assert_eq!(response.body, asset);
        assert!(
            response
                .head
                .contains(&format!("X-Namedrop-Csrf: {}\r\n", server.csrf))
        );
        assert!(
            response
                .head
                .contains(&format!("Content-Length: {}\r\n", asset.len()))
        );
        let other = Fixture::new();
        let other_server = Server::start(&other, None);
        assert_ne!(server.csrf, other_server.csrf);
        assert_eq!(
            server
                .request(
                    "POST",
                    "/w/00000000000000000000/messages",
                    &[
                        ("Cookie", &server.cookie),
                        ("X-Namedrop-Csrf", &other_server.csrf),
                        ("Content-Type", "application/json")
                    ],
                    "{}"
                )
                .status,
            403
        );
    }
}
