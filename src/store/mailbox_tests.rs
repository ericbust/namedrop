use super::*;
use crate::{
    AgentSessionId, DeliveryAttemptId, DeliveryId, MessageRecipientDraft, PaneBindingId, PaneRoleId,
};

fn at(time: u64) -> MutationAttribution {
    MutationAttribution::new(ActorKind::Human, "test", time)
}

fn setup(tx: &mut StateTransaction<'_>, ns: &StateNamespace) -> Result<(), StoreError> {
    tx.activate_worktree(ns, &at(1))?;
    tx.create_pane_address(
        ns,
        &PaneAddressDraft {
            id: PaneAddressId::new("address"),
            role_id: PaneRoleId::new("role"),
            role_name: "recipient".into(),
            role_description: "Recipient".into(),
            alias: "recipient".into(),
        },
        &at(1),
    )?;
    tx.bind_pane(
        ns,
        &PaneBindingId::new("binding"),
        &PaneAddressId::new("address"),
        "pane",
        &at(1),
    )?;
    tx.observe_agent_session(
        ns,
        "observation",
        &PaneAddressId::new("address"),
        &AgentSessionId::new("session"),
        "idle",
        &at(1),
    )?;
    tx.create_thread(ns, &ThreadId::new("thread"), "Mailbox", &at(1))?;
    tx.append_message(
        ns,
        &MessageDraft {
            thread_id: ThreadId::new("thread"),
            message_id: MessageId::new("message"),
            in_reply_to: None,
            author_address_id: PaneAddressId::new("address"),
            author_kind: ActorKind::Human,
            author_alias_snapshot: "recipient".into(),
            author_agent_session_snapshot: None,
            request_id: "request".into(),
            authored_source: "@recipient hello".into(),
            body: "hello".into(),
            grammar_version: "mention/v1".into(),
            context_references: vec![],
            recipients: vec![MessageRecipientDraft {
                address_id: PaneAddressId::new("address"),
                alias_snapshot: "recipient".into(),
                agent_session_snapshot: None,
                delivery_id: DeliveryId::new("delivery"),
            }],
        },
        &at(1),
    )?;
    Ok(())
}

fn lease(id: &str) -> LeaseDraft {
    LeaseDraft {
        attempt_id: DeliveryAttemptId::new(id),
        binding_id: PaneBindingId::new("binding"),
        agent_session_id: AgentSessionId::new("session"),
        retry_of_attempt_id: None,
        owner: "test".into(),
        acquired_at_ms: 10,
        expires_at_ms: 1000,
    }
}

#[test]
fn mailbox_refusal_owns_its_pause_without_pausing_exception_prompts() {
    let directory = super::tests::TestDirectory::new();
    let ns = StateNamespace::for_space("socket", "space");
    let mut store = StateStore::open(&directory.0).unwrap();
    store
        .transaction(|tx| {
            setup(tx, &ns)?;
            assert!(
                tx.lease_mailbox_head(
                    &ns,
                    &PaneAddressId::new("address"),
                    &lease("attempt"),
                    &at(10)
                )?
                .is_some()
            );
            tx.return_delivery_attempt_to_queue(
                &ns,
                &DeliveryAttemptId::new("attempt"),
                DeliveryReturnReason::DeferredUntilSettle {
                    observed_status_ms: Some(100),
                },
                &at(120),
            )?;
            let facts = tx.transition_facts(&ns)?;
            assert!(
                !facts
                    .iter()
                    .any(|fact| fact.entity_kind == TransitionEntityKind::PromptRecipient),
                "ordinary mailbox refusal wrote prompt_recipient facts"
            );
            assert!(facts.iter().any(|fact| fact.entity_kind
                == TransitionEntityKind::MailboxDelivery
                && fact.entity_id == "delivery"
                && fact.transition_kind == TransitionKind::Paused));
            assert!(facts.iter().any(|fact| fact.entity_kind
                == TransitionEntityKind::MailboxDelivery
                && fact.entity_id == "delivery"
                && fact.transition_kind == TransitionKind::Observed
                && fact.occurred_at_ms == 100));
            assert_eq!(
                tx.delivery_attempt_state(&ns, &DeliveryAttemptId::new("attempt"))?,
                DeliveryState::Cancelled
            );
            assert!(tx.unresolved_attention_items(&ns)?.is_empty());
            assert!(!tx.prompt_is_deferred(&ns, "pane", &AgentSessionId::new("session"))?);
            Ok(())
        })
        .unwrap();
}

#[test]
fn binding_location_accepts_missing_tab_but_requires_pane() {
    let directory = super::tests::TestDirectory::new();
    let ns = StateNamespace::for_space("socket", "space");
    StateStore::open(&directory.0)
        .unwrap()
        .transaction(|tx| {
            setup(tx, &ns)?;
            tx.observe_pane_binding_location(&ns, &PaneAddressId::new("address"), "pane", "tab")?;
            tx.observe_pane_binding_location(&ns, &PaneAddressId::new("address"), "pane", "")?;
            assert_eq!(tx.active_pane_bindings(&ns)?[0].tab_id, None);
            assert!(
                tx.observe_pane_binding_location(&ns, &PaneAddressId::new("address"), "", "tab")
                    .is_err()
            );
            Ok(())
        })
        .unwrap();
}

const GATE_QUERY: &str = "SELECT gate.transition_kind, COALESCE(observed.occurred_at_ms, 0)
FROM transition_facts gate LEFT JOIN transition_facts observed
ON observed.session_socket_id=gate.session_socket_id AND observed.worktree_id=gate.worktree_id
AND observed.worktree_order=gate.worktree_order-1 AND observed.entity_kind=gate.entity_kind
AND observed.entity_id=gate.entity_id AND observed.transition_kind='observed'
WHERE gate.session_socket_id=?1 AND gate.worktree_id=?2
AND gate.entity_kind='prompt_recipient' AND gate.entity_id=?3
AND gate.transition_kind IN ('paused','resumed') ORDER BY gate.worktree_order DESC LIMIT 1";

#[test]
fn recipient_gate_work_is_bounded_despite_unrelated_history() {
    let directory = super::tests::TestDirectory::new();
    let ns = StateNamespace::for_space("socket", "space");
    let mut store = StateStore::open(&directory.0).unwrap();
    store.transaction(|tx| {
        tx.activate_worktree(&ns, &at(1))?;
        tx.defer_prompt_until_settle(&ns, "pane", &AgentSessionId::new("session"), Some(100), &at(120))?;
        tx.transaction.execute_batch("WITH RECURSIVE n(x) AS (VALUES(10) UNION ALL SELECT x+1 FROM n WHERE x<200009)
INSERT INTO transition_facts SELECT 'socket','space',x,'mailbox_delivery','unrelated-'||x,'queued','human','test',1 FROM n;")?;
        let mut stmt = tx.transaction.prepare(GATE_QUERY)?;
        let result: (String, i64) = stmt.query_row(params!["socket", "space", prompt_recipient_key("pane", &AgentSessionId::new("session"))], |row| Ok((row.get(0)?, row.get(1)?)))?;
        assert_eq!(result, ("paused".into(), 100));
        let steps = stmt.get_status(rusqlite::StatementStatus::VmStep);
        assert!(steps < 1000, "recipient gate executed {steps} VM steps for one matching recipient");
        Ok(())
    }).unwrap();
}

fn ready(tx: &mut StateTransaction<'_>, ns: &StateNamespace) -> Result<(), StoreError> {
    let address = PaneAddressId::new("address");
    tx.issue_integration_challenge(
        ns,
        "registration",
        &address,
        AgentRuntime::ClaudeCode,
        "nonce",
        &at(2),
    )?;
    tx.complete_integration_registration(
        ns,
        &address,
        &AgentSessionId::new("session"),
        AgentRuntime::ClaudeCode,
        crate::INTEGRATION_PROTOCOL_VERSION,
        "nonce",
        &crate::complete_capability_profile(),
        &at(3),
    )?;
    tx.complete_reconciliation(ns)?;
    Ok(())
}

#[test]
fn mailbox_pause_survives_reopen_and_only_matching_newer_settle_resumes_head() {
    let directory = super::tests::TestDirectory::new();
    let ns = StateNamespace::for_space("ordinary-socket", "space");
    let address = PaneAddressId::new("address");
    let session = AgentSessionId::new("session");
    let hc = crate::runtime::HostKind::Hc;
    let mut store = StateStore::open(&directory.0).unwrap();
    store
        .transaction(|tx| {
            setup(tx, &ns)?;
            ready(tx, &ns)?;
            assert!(tx.dispatch_candidate_for_host(&ns, &address, hc)?.is_some());
            assert!(
                tx.lease_mailbox_head_for_host(&ns, &address, &lease("attempt"), hc, &at(10))?
                    .is_some()
            );
            tx.return_delivery_attempt_to_queue(
                &ns,
                &DeliveryAttemptId::new("attempt"),
                DeliveryReturnReason::DeferredUntilSettle {
                    observed_status_ms: Some(100),
                },
                &at(120),
            )
        })
        .unwrap();
    drop(store);
    StateStore::open(&directory.0)
        .unwrap()
        .transaction(|tx| {
            assert!(tx.dispatch_candidate_for_host(&ns, &address, hc)?.is_none());
            assert!(tx.dispatch_candidate(&ns, &address)?.is_some());
            for (pane, session_id, status) in [
                ("pane", "session", 99),
                ("pane", "session", 100),
                ("other", "session", 101),
                ("pane", "replacement", 101),
            ] {
                tx.resume_mailbox_after_settle(
                    &ns,
                    pane,
                    &AgentSessionId::new(session_id),
                    status,
                    &at(200),
                )?;
                assert!(
                    tx.lease_mailbox_head_for_host(&ns, &address, &lease("retry"), hc, &at(200))?
                        .is_none()
                );
            }
            tx.pause_thread(&ns, &ThreadId::new("thread"), &at(201))?;
            tx.resume_thread(&ns, &ThreadId::new("thread"), &at(202))?;
            assert!(tx.dispatch_candidate_for_host(&ns, &address, hc)?.is_none());
            tx.resume_mailbox_after_settle(&ns, "pane", &session, 101, &at(210))?;
            assert!(tx.dispatch_candidate_for_host(&ns, &address, hc)?.is_some());
            assert!(
                tx.lease_mailbox_head_for_host(&ns, &address, &lease("retry"), hc, &at(210))?
                    .is_some()
            );
            tx.return_delivery_attempt_to_queue(
                &ns,
                &DeliveryAttemptId::new("retry"),
                DeliveryReturnReason::DeferredUntilSettle {
                    observed_status_ms: None,
                },
                &at(220),
            )?;
            tx.resume_mailbox_after_settle(&ns, "pane", &session, 0, &at(230))?;
            assert!(tx.dispatch_candidate_for_host(&ns, &address, hc)?.is_none());
            tx.resume_mailbox_after_settle(&ns, "pane", &session, 1, &at(240))?;
            assert!(tx.dispatch_candidate_for_host(&ns, &address, hc)?.is_some());
            Ok(())
        })
        .unwrap();
}

#[test]
fn exception_pause_never_gates_mailbox_and_dead_exception_sessions_are_reconciled() {
    let directory = super::tests::TestDirectory::new();
    let ns = StateNamespace::for_space("hc:misleading", "space");
    let session = AgentSessionId::new("session");
    StateStore::open(&directory.0)
        .unwrap()
        .transaction(|tx| {
            setup(tx, &ns)?;
            ready(tx, &ns)?;
            tx.defer_prompt_until_settle(&ns, "pane", &session, Some(100), &at(120))?;
            tx.defer_prompt_until_settle(
                &ns,
                "dead-pane",
                &AgentSessionId::new("dead-session"),
                Some(100),
                &at(120),
            )?;
            for kind in [crate::runtime::HostKind::Tui, crate::runtime::HostKind::Hc] {
                assert!(
                    tx.dispatch_candidate_for_host(&ns, &PaneAddressId::new("address"), kind)?
                        .is_some()
                );
            }
            assert!(
                tx.lease_mailbox_head_for_host(
                    &ns,
                    &PaneAddressId::new("address"),
                    &lease("attempt"),
                    crate::runtime::HostKind::Hc,
                    &at(130)
                )?
                .is_some()
            );
            tx.reconcile_prompt_recipients(&ns, &[("pane".into(), session.clone())], &at(140))?;
            assert!(tx.prompt_is_deferred(&ns, "pane", &session)?);
            assert!(!tx.prompt_is_deferred(
                &ns,
                "dead-pane",
                &AgentSessionId::new("dead-session")
            )?);
            tx.reconcile_prompt_recipients(&ns, &[], &at(150))?;
            assert!(!tx.prompt_is_deferred(&ns, "pane", &session)?);
            let before = tx.transition_facts(&ns)?.len();
            tx.reconcile_prompt_recipients(&ns, &[], &at(160))?;
            assert_eq!(before, tx.transition_facts(&ns)?.len());
            Ok(())
        })
        .unwrap();
}

#[test]
fn schema_six_upgrade_adds_entity_index_without_changing_facts() {
    let directory = super::tests::TestDirectory::new();
    let ns = StateNamespace::for_space("socket", "space");
    let mut store = StateStore::open(&directory.0).unwrap();
    let before = store
        .transaction(|tx| {
            setup(tx, &ns)?;
            tx.transition_facts(&ns)
        })
        .unwrap();
    store
        .connection
        .execute_batch("DROP INDEX transition_facts_by_entity; PRAGMA user_version=6;")
        .unwrap();
    drop(store);
    let mut store = StateStore::open(&directory.0).unwrap();
    assert_eq!(store.schema_version().unwrap(), 7);
    assert_eq!(
        store.transaction(|tx| tx.transition_facts(&ns)).unwrap(),
        before
    );
    assert_eq!(
        store
            .connection
            .query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='transition_facts_by_entity'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
}

#[test]
fn tui_dispatch_and_lease_skip_settle_queries_and_hc_queries_only_selected_heads() {
    unsafe extern "C" fn trace(
        _: u32,
        context: *mut std::ffi::c_void,
        statement: *mut std::ffi::c_void,
        _: *mut std::ffi::c_void,
    ) -> i32 {
        let sql = unsafe { rusqlite::ffi::sqlite3_sql(statement.cast()) };
        if !sql.is_null() {
            let sql = unsafe { std::ffi::CStr::from_ptr(sql) }.to_string_lossy();
            if sql.contains("FROM transition_facts gate") {
                unsafe { &mut *context.cast::<Vec<String>>() }.push(sql.into_owned());
            }
        }
        0
    }
    for kind in [crate::runtime::HostKind::Tui, crate::runtime::HostKind::Hc] {
        let directory = super::tests::TestDirectory::new();
        let ns = StateNamespace::for_space("hc:prefix-is-not-host-kind", "space");
        let address = PaneAddressId::new("address");
        let mut store = StateStore::open(&directory.0).unwrap();
        store
            .transaction(|tx| {
                setup(tx, &ns)?;
                ready(tx, &ns)
            })
            .unwrap();
        let mut queries = Vec::<String>::new();
        unsafe {
            rusqlite::ffi::sqlite3_trace_v2(
                store.connection.handle(),
                rusqlite::ffi::SQLITE_TRACE_STMT as u32,
                Some(trace),
                (&mut queries as *mut Vec<String>).cast(),
            );
        }
        store
            .transaction(|tx| {
                assert!(
                    tx.dispatch_candidate_for_host(&ns, &address, kind)?
                        .is_some()
                );
                assert!(
                    tx.lease_mailbox_head_for_host(
                        &ns,
                        &address,
                        &lease("attempt"),
                        kind,
                        &at(10)
                    )?
                    .is_some()
                );
                assert!(
                    tx.dispatch_candidate_for_host(&ns, &address, kind)?
                        .is_none()
                );
                assert!(
                    tx.lease_mailbox_head_for_host(
                        &ns,
                        &address,
                        &lease("another"),
                        kind,
                        &at(11)
                    )?
                    .is_none()
                );
                Ok(())
            })
            .unwrap();
        unsafe {
            rusqlite::ffi::sqlite3_trace_v2(
                store.connection.handle(),
                0,
                None,
                std::ptr::null_mut(),
            );
        }
        assert_eq!(
            queries.len(),
            if kind == crate::runtime::HostKind::Tui {
                0
            } else {
                2
            },
            "{queries:?}"
        );
        assert!(
            queries
                .iter()
                .all(|sql| sql.contains("gate.entity_kind = 'mailbox_delivery'"))
        );
    }
}

#[test]
fn replacement_session_owns_only_its_own_mailbox_pause_and_unused_attempt_can_cancel() {
    let directory = super::tests::TestDirectory::new();
    let ns = StateNamespace::for_space("socket", "space");
    let address = PaneAddressId::new("address");
    let hc = crate::runtime::HostKind::Hc;
    StateStore::open(&directory.0)
        .unwrap()
        .transaction(|tx| {
            setup(tx, &ns)?;
            assert!(
                tx.lease_mailbox_head_for_host(&ns, &address, &lease("old"), hc, &at(10))?
                    .is_some()
            );
            tx.return_delivery_attempt_to_queue(
                &ns,
                &DeliveryAttemptId::new("old"),
                DeliveryReturnReason::DeferredUntilSettle {
                    observed_status_ms: Some(100),
                },
                &at(120),
            )?;
            tx.observe_agent_session(
                &ns,
                "replacement-observation",
                &address,
                &AgentSessionId::new("replacement"),
                "idle",
                &at(130),
            )?;
            let mut replacement = lease("new");
            replacement.agent_session_id = AgentSessionId::new("replacement");
            assert!(
                tx.lease_mailbox_head_for_host(&ns, &address, &replacement, hc, &at(140))?
                    .is_some()
            );
            tx.return_delivery_attempt_to_queue(
                &ns,
                &DeliveryAttemptId::new("new"),
                DeliveryReturnReason::DeferredUntilSettle {
                    observed_status_ms: Some(200),
                },
                &at(210),
            )?;
            tx.resume_mailbox_after_settle(
                &ns,
                "pane",
                &AgentSessionId::new("session"),
                300,
                &at(220),
            )?;
            assert!(
                tx.lease_mailbox_head_for_host(&ns, &address, &replacement, hc, &at(230))?
                    .is_none()
            );
            assert_eq!(
                tx.hold_expired_queued_deliveries(&ns, 1, &at(86_400_001))?,
                [DeliveryId::new("delivery")]
            );
            assert_eq!(
                tx.cancel_unsent_held_delivery(&ns, &DeliveryId::new("delivery"), &at(86_400_002))?,
                DeliveryState::Cancelled
            );
            Ok(())
        })
        .unwrap();
}

fn legacy_refusal(
    tx: &mut StateTransaction<'_>,
    ns: &StateNamespace,
    attempt: &str,
    status: u64,
) -> Result<Vec<u64>, StoreError> {
    assert!(
        tx.lease_mailbox_head(
            ns,
            &PaneAddressId::new("address"),
            &lease(attempt),
            &at(status)
        )?
        .is_some()
    );
    tx.return_delivery_attempt_to_queue(
        ns,
        &DeliveryAttemptId::new(attempt),
        DeliveryReturnReason::DeferredUntilSettle {
            observed_status_ms: Some(status),
        },
        &at(status + 10),
    )?;
    let orders = tx
        .transition_facts(ns)?
        .into_iter()
        .filter(|fact| {
            fact.entity_kind == TransitionEntityKind::MailboxDelivery
                && matches!(
                    fact.transition_kind,
                    TransitionKind::Observed | TransitionKind::Paused
                )
        })
        .map(|fact| fact.worktree_order)
        .collect();
    let trigger: String = tx.transaction.query_row(
        "SELECT sql FROM sqlite_schema WHERE name='immutable_transitions_update'",
        [],
        |r| r.get(0),
    )?;
    tx.transaction
        .execute_batch("DROP TRIGGER immutable_transitions_update;")?;
    tx.transaction.execute("UPDATE transition_facts SET entity_kind='prompt_recipient',entity_id=?3 WHERE session_socket_id=?1 AND worktree_id=?2 AND entity_kind='mailbox_delivery' AND transition_kind IN ('observed','paused')", params![ns.session_socket_id,ns.space_id.as_str(),prompt_recipient_key("pane", &AgentSessionId::new("session"))])?;
    tx.transaction.execute_batch(&trigger)?;
    Ok(orders)
}

#[test]
fn v6_mailbox_deferrals_migrate_exact_facts_and_latest_resume_state() {
    for (cycles, resumed) in [(1, false), (1, true), (2, false), (2, true)] {
        let directory = super::tests::TestDirectory::new();
        let ns = StateNamespace::for_space("socket", "space");
        let address = PaneAddressId::new("address");
        let hc = crate::runtime::HostKind::Hc;
        let mut store = StateStore::open(&directory.0).unwrap();
        let (before, converted_orders) = store
            .transaction(|tx| {
                setup(tx, &ns)?;
                ready(tx, &ns)?;
                let mut orders = Vec::new();
                for cycle in 0..cycles {
                    let status = 100 * (cycle + 1);
                    orders.extend(legacy_refusal(tx, &ns, &format!("legacy-{cycle}"), status)?);
                    if cycle + 1 < cycles || resumed {
                        tx.resume_prompt_after_settle(
                            &ns,
                            "pane",
                            &AgentSessionId::new("session"),
                            status + 1,
                            &at(status + 20),
                        )?;
                    }
                }
                tx.defer_prompt_until_settle(
                    &ns,
                    "doctor",
                    &AgentSessionId::new("doctor-session"),
                    Some(500),
                    &at(510),
                )?;
                if resumed {
                    tx.defer_prompt_until_settle(
                        &ns,
                        "pane",
                        &AgentSessionId::new("session"),
                        Some(600),
                        &at(610),
                    )?;
                }
                Ok((tx.transition_facts(&ns)?, orders))
            })
            .unwrap();
        store
            .connection
            .execute_batch("DROP INDEX transition_facts_by_entity; PRAGMA user_version=6;")
            .unwrap();
        drop(store);
        let mut store = StateStore::open(&directory.0).unwrap();
        let migrated = store
            .transaction(|tx| {
                let facts = tx.transition_facts(&ns)?;
                for old in &before {
                    let mut expected = old.clone();
                    if converted_orders.contains(&old.worktree_order) {
                        expected.entity_kind = TransitionEntityKind::MailboxDelivery;
                        expected.entity_id = "delivery".into();
                    }
                    assert_eq!(
                        facts
                            .iter()
                            .find(|f| f.worktree_order == old.worktree_order),
                        Some(&expected),
                        "cycles={cycles}, resumed={resumed}"
                    );
                }
                assert_eq!(
                    tx.dispatch_candidate_for_host(&ns, &address, hc)?.is_some(),
                    resumed,
                    "cycles={cycles}, resumed={resumed}"
                );
                assert!(tx.prompt_is_deferred(
                    &ns,
                    "doctor",
                    &AgentSessionId::new("doctor-session")
                )?);
                assert!(
                    tx.transaction
                        .execute("UPDATE transition_facts SET occurred_at_ms=0", [])
                        .is_err()
                );
                Ok(facts)
            })
            .unwrap();
        drop(store);
        let mut reopened = StateStore::open(&directory.0).unwrap();
        assert_eq!(
            reopened.transaction(|tx| tx.transition_facts(&ns)).unwrap(),
            migrated
        );
        reopened
            .connection
            .execute_batch("DROP INDEX transition_facts_by_entity; PRAGMA user_version=6;")
            .unwrap();
        drop(reopened);
        assert_eq!(
            StateStore::open(&directory.0)
                .unwrap()
                .transaction(|tx| tx.transition_facts(&ns))
                .unwrap(),
            migrated
        );
    }
}

#[test]
fn v6_unused_refusal_remains_cancellable_after_upgrade() {
    let directory = super::tests::TestDirectory::new();
    let ns = StateNamespace::for_space("socket", "space");
    let mut store = StateStore::open(&directory.0).unwrap();
    store
        .transaction(|tx| {
            setup(tx, &ns)?;
            legacy_refusal(tx, &ns, "legacy", 100)?;
            tx.hold_expired_queued_deliveries(&ns, 1, &at(86_400_001))?;
            Ok(())
        })
        .unwrap();
    store
        .connection
        .execute_batch("DROP INDEX transition_facts_by_entity; PRAGMA user_version=6;")
        .unwrap();
    drop(store);
    StateStore::open(&directory.0)
        .unwrap()
        .transaction(|tx| {
            assert_eq!(
                tx.cancel_unsent_held_delivery(&ns, &DeliveryId::new("delivery"), &at(86_400_002))?,
                DeliveryState::Cancelled
            );
            Ok(())
        })
        .unwrap();
}
