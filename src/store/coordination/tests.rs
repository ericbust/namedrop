use super::*;
use std::time::Instant;

fn fixture(coordinations: usize, facts: usize) -> Connection {
    let mut connection = Connection::open_in_memory().unwrap();
    initialize_schema(&mut connection, Path::new(".")).unwrap();
    connection
        .pragma_update(None, "foreign_keys", true)
        .unwrap();
    populate(&connection, "socket", "space", coordinations, facts);
    connection
}

fn populate(connection: &Connection, socket: &str, space: &str, count: usize, facts: usize) {
    connection.execute_batch(&format!(r#"
        BEGIN;
        INSERT INTO worktrees VALUES ('{socket}','{space}',1,0,0,0);
        INSERT INTO pane_roles VALUES ('{socket}','{space}','role','worker','');
        INSERT INTO pane_addresses(session_socket_id,worktree_id,address_id,role_id,retired,collaboration_disabled,created_at_ms)
            VALUES ('{socket}','{space}','owner','role',1,0,0);
        INSERT INTO collaboration_threads VALUES ('{socket}','{space}','thread','test',0);
        WITH RECURSIVE n(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM n WHERE i+1<{count}*9)
        INSERT INTO collaboration_messages(session_socket_id,worktree_id,message_id,thread_id,thread_order,
            author_address_id,author_alias_snapshot,author_kind,request_id,authored_source,body,grammar_version,created_at_ms)
            SELECT '{socket}','{space}','m'||i,'thread',i+1,'owner','owner','human','r'||i,'test','test','collab/v1',0 FROM n;
        INSERT INTO message_recipients(session_socket_id,worktree_id,message_id,recipient_order,address_id,alias_snapshot)
            SELECT session_socket_id,worktree_id,message_id,1,'owner','owner' FROM collaboration_messages
            WHERE session_socket_id='{socket}' AND worktree_id='{space}';
        INSERT INTO mailbox_deliveries
            SELECT session_socket_id,worktree_id,'d'||(thread_order-1),'owner',thread_order,message_id,'settled',0
            FROM collaboration_messages WHERE session_socket_id='{socket}' AND worktree_id='{space}';
        WITH RECURSIVE n(i) AS (VALUES(0) UNION ALL SELECT i+1 FROM n WHERE i+1<{count})
        INSERT INTO coordinations(session_socket_id,worktree_id,coordination_id,request_id,owner_address_id,owner_session_id,
            deliverable,outcome,report,report_message_id,wake_message_id,created_at_ms,updated_at_ms)
            SELECT '{socket}','{space}','c'||i,'r'||i,'owner','session','test','blocked','report','m'||(i*9),'m'||(i*9+1),0,0 FROM n;
        INSERT INTO coordination_assignments(session_socket_id,worktree_id,assignment_id,coordination_id,recipient_address_id,
            recipient_session_id,original_message_id,current_message_id,revision,work_outcome)
            SELECT session_socket_id,worktree_id,coordination_id,coordination_id,'owner','session',report_message_id,report_message_id,1,'blocked'
            FROM coordinations WHERE session_socket_id='{socket}' AND worktree_id='{space}';
        INSERT INTO coordination_links
            SELECT session_socket_id,worktree_id,message_id,'c'||((thread_order-1)/9),1,'request','{{}}'
            FROM collaboration_messages WHERE session_socket_id='{socket}' AND worktree_id='{space}' AND (thread_order-1)%9>=2;
        INSERT INTO attention_items
            SELECT session_socket_id,worktree_id,'a'||coordination_id,'coordination',coordination_id,'test',0,1
            FROM coordinations WHERE session_socket_id='{socket}' AND worktree_id='{space}';
        WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<{facts})
        INSERT INTO transition_facts
            SELECT '{socket}','{space}',i,
                CASE WHEN i>{facts}-{count} THEN 'attention_item' ELSE 'delivery' END,
                CASE WHEN i>{facts}-{count} THEN 'ac'||(i-({facts}-{count})-1) ELSE 'd'||i END,
                CASE WHEN i>{facts}-{count} THEN 'acknowledged' ELSE 'settled' END,'human','test',0 FROM n;
        COMMIT;
    "#)).unwrap();
}

#[test]
fn acknowledgement_predicate_matches_previous_outcomes_and_transport() {
    let mut connection = fixture(1, 1);
    populate(&connection, "other-socket", "space", 1, 1);
    populate(&connection, "socket", "other-space", 1, 1);
    connection.execute("UPDATE mailbox_deliveries SET state='queued' WHERE session_socket_id!='socket' OR worktree_id!='space'", []).unwrap();
    let transaction = StateTransaction {
        transaction: connection.transaction().unwrap(),
        state_directory: Path::new("unused"),
    };
    let ns = StateNamespace::for_space("socket", "space");
    let states = [
        "queued",
        "dispatching",
        "submitted",
        "acknowledged",
        "settled",
        "held",
        "failed",
        "cancelled",
    ];
    for outcome in ["open", "completed", "blocked", "cancelled"] {
        for report in [None, Some("report")] {
            for retired in [false, true] {
                transaction.transaction.execute("UPDATE pane_addresses SET retired=?1 WHERE session_socket_id='socket' AND worktree_id='space'", [retired]).unwrap();
                for report_state in states {
                    for message in ["m0", "m1", "m2", "m8"] {
                        for state in states {
                            transaction.transaction.execute("UPDATE coordinations SET outcome=?1,report=?2 WHERE session_socket_id='socket' AND worktree_id='space'", params![outcome, report]).unwrap();
                            transaction.transaction.execute("UPDATE mailbox_deliveries SET state='settled' WHERE session_socket_id='socket' AND worktree_id='space'", []).unwrap();
                            transaction.transaction.execute("UPDATE mailbox_deliveries SET state=?1 WHERE session_socket_id='socket' AND worktree_id='space' AND message_id='m0'", [report_state]).unwrap();
                            transaction.transaction.execute("UPDATE mailbox_deliveries SET state=?1 WHERE session_socket_id='socket' AND worktree_id='space' AND message_id=?2", params![state,message]).unwrap();
                            let previous: bool = transaction
                                .transaction
                                .query_row(
                                    include_str!("acknowledgement_before.sql"),
                                    params!["socket", "space", "c0"],
                                    |row| row.get(0),
                                )
                                .unwrap();
                            let actual = transaction
                                .coordination_attention_can_be_acknowledged(
                                    &ns,
                                    &CoordinationId::new("c0"),
                                )
                                .unwrap();
                            let report_delivery =
                                if message == "m0" { state } else { report_state };
                            let eligible = match outcome {
                                "open" => retired,
                                "completed" => report.is_some() && report_delivery == "cancelled",
                                _ => report.is_some() && report_delivery == "settled",
                            };
                            let terminal =
                                |state| matches!(state, "settled" | "failed" | "cancelled");
                            assert_eq!(
                                actual,
                                eligible && terminal(report_delivery) && terminal(state),
                                "{outcome} {report:?} {retired} {report_state} {message} {state}"
                            );
                            assert_eq!(actual, previous);
                        }
                    }
                }
            }
        }
    }
    for report_message in [None, Some("m0"), Some("absent")] {
        for wake_message in [None, Some("m0"), Some("m1"), Some("absent")] {
            transaction.transaction.execute("UPDATE coordinations SET outcome='open',report_message_id=?1,wake_message_id=?2 WHERE session_socket_id='socket' AND worktree_id='space'", params![report_message,wake_message]).unwrap();
            transaction.transaction.execute("UPDATE mailbox_deliveries SET state='queued' WHERE session_socket_id='socket' AND worktree_id='space' AND message_id='m0'", []).unwrap();
            let previous: bool = transaction
                .transaction
                .query_row(
                    include_str!("acknowledgement_before.sql"),
                    params!["socket", "space", "c0"],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                transaction
                    .coordination_attention_can_be_acknowledged(&ns, &CoordinationId::new("c0"))
                    .unwrap(),
                previous
            );
        }
    }
    assert!(
        !transaction
            .coordination_attention_can_be_acknowledged(&ns, &CoordinationId::new("missing"))
            .unwrap()
    );
}

#[test]
fn acknowledgement_uses_the_remote_delivery_progress() {
    let mut connection = fixture(1, 1);
    populate(&connection, "socket", "remote", 1, 1);
    connection.execute_batch("INSERT INTO space_envoys VALUES ('socket','space','owner','remote','owner','owner','remote','space');
        INSERT INTO bridged_messages VALUES ('socket','remote','m2','space','m2','space','remote');").unwrap();
    let transaction = StateTransaction {
        transaction: connection.transaction().unwrap(),
        state_directory: Path::new("unused"),
    };
    let ns = StateNamespace::for_space("socket", "space");
    for (state, expected) in [
        ("queued", false),
        ("held", false),
        ("settled", true),
        ("failed", true),
        ("cancelled", true),
    ] {
        transaction.transaction.execute("UPDATE mailbox_deliveries SET state=?1 WHERE worktree_id='remote' AND message_id='m2'", [state]).unwrap();
        let previous: bool = transaction
            .transaction
            .query_row(
                include_str!("acknowledgement_before.sql"),
                params!["socket", "space", "c0"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(previous, expected, "{state}");
        assert_eq!(
            transaction
                .coordination_attention_can_be_acknowledged(&ns, &CoordinationId::new("c0"))
                .unwrap(),
            expected,
            "{state}"
        );
    }
}

#[test]
fn reconciliation_stays_below_busy_timeout_at_real_state_scale() {
    let mut connection = fixture(190, 50_000);
    let mut transaction = StateTransaction {
        transaction: connection.transaction().unwrap(),
        state_directory: Path::new("unused"),
    };
    let ns = StateNamespace::for_space("socket", "space");
    let live = crate::spaces::LiveSpaces::here(crate::DiscoveredWorktree {
        worktree_id: crate::WorktreeId::new("space"),
        workspace_id: "space".into(),
        worktree_attached: true,
        label: None,
        branch: None,
        panes: vec![],
    });
    let by = MutationAttribution::new(ActorKind::Human, "test", 1);
    let start = Instant::now();
    transaction
        .reconcile_coordinations(&ns, &live, &by)
        .unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed < Duration::from_millis(750),
        "190 coordinations, 1710 deliveries and 50000 facts took {elapsed:?}"
    );
    assert_eq!(
        transaction
            .transaction
            .query_row(
                "SELECT count(*) FROM attention_items WHERE resolved_at_ms IS NULL",
                [],
                |row| row.get::<_, u32>(0)
            )
            .unwrap(),
        0
    );
    eprintln!("Reconciled 190 coordinations, 1710 deliveries and 50000 facts in {elapsed:?}");
}
