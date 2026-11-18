#![cfg(feature = "test-support")]
#[path = "support/cleanup.rs"]
mod cleanup;

use namedrop::mux::{
    InputOutcome, Key, PaneId, Recipient,
    fake::{Attempt, CHECKOUT_LISTINGS, FakeMux, SentInput, StrictSpace, Takes},
};
use namedrop::{
    ActorKind, AgentSessionId, Clock, DeliveryId, IdGenerator, LifecycleRuntime, MessageDraft,
    MessageId, MessageRecipientDraft, MutationAttribution, PaneAddressDraft, PaneAddressId,
    PaneRoleId, StateNamespace, StateStore, StoreErrorKind, ThreadId, execute_control_operation,
    reconcile_discovered_panes,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

#[test]
fn confirmed_cancel_held_is_exact_and_never_retries_or_stops_an_agent() {
    use clap::Parser;
    use namedrop::{DeliveryAttemptId, DeliveryState, LeaseDraft, PaneBindingId};
    let cli = namedrop::cli::Cli::try_parse_from([
        "namedrop",
        "delivery",
        "cancel-held",
        "--input-json",
        r#"{"attempt_id":"attempt-1","confirmed":true}"#,
    ])
    .unwrap();
    assert_eq!(cli.operation(), "delivery.cancel-held");
    for state in [
        DeliveryState::Queued,
        DeliveryState::Dispatching,
        DeliveryState::Submitted,
        DeliveryState::Settled,
        DeliveryState::Held,
    ] {
        let directory = TestDirectory::new();
        let namespace = StateNamespace::for_space("socket-a", "workspace-1");
        let human = attribution(ActorKind::Human, "human", 1);
        let recipient = PaneAddressId::new("address-recipient");
        let attempt = DeliveryAttemptId::new("attempt-1");
        let mut store = StateStore::open(directory.path()).unwrap();
        store
            .transaction(|tx| {
                tx.activate_worktree(&namespace, &human)?;
                tx.create_pane_address(&namespace, &address("address-sender", "sender"), &human)?;
                tx.create_pane_address(
                    &namespace,
                    &address("address-recipient", "recipient"),
                    &human,
                )?;
                tx.bind_pane(
                    &namespace,
                    &PaneBindingId::new("binding-recipient"),
                    &recipient,
                    "pane-recipient",
                    &human,
                )?;
                tx.observe_agent_session(
                    &namespace,
                    "observation-recipient",
                    &recipient,
                    &AgentSessionId::new("session-recipient"),
                    "working",
                    &human,
                )?;
                tx.create_thread(
                    &namespace,
                    &ThreadId::new("thread-1"),
                    "Cancel test",
                    &human,
                )?;
                tx.append_message(
                    &namespace,
                    &message(
                        1,
                        &ThreadId::new("thread-1"),
                        &PaneAddressId::new("address-sender"),
                        ActorKind::Human,
                        &recipient,
                        None,
                    ),
                    &human,
                )?;
                if state != DeliveryState::Queued {
                    tx.lease_mailbox_head(
                        &namespace,
                        &recipient,
                        &LeaseDraft {
                            attempt_id: attempt.clone(),
                            binding_id: PaneBindingId::new("binding-recipient"),
                            agent_session_id: AgentSessionId::new("session-recipient"),
                            retry_of_attempt_id: None,
                            owner: "test".into(),
                            acquired_at_ms: 2,
                            expires_at_ms: 1000,
                        },
                        &human,
                    )?;
                    if matches!(state, DeliveryState::Submitted | DeliveryState::Settled) {
                        tx.transition_delivery_attempt(
                            &namespace,
                            &attempt,
                            DeliveryState::Submitted,
                            &human,
                        )?;
                    }
                    if state == DeliveryState::Settled {
                        tx.acknowledge_delivery_attempt(
                            &namespace,
                            &attempt,
                            "ack-1",
                            &AgentSessionId::new("session-recipient"),
                            &human,
                        )?;
                    }
                    if matches!(state, DeliveryState::Held | DeliveryState::Settled) {
                        tx.transition_delivery_attempt(&namespace, &attempt, state, &human)?;
                    }
                }
                Ok(())
            })
            .unwrap();
        let herdr = fake_herdr();
        let ids = SequenceIds(AtomicU64::new(1));
        let runtime = LifecycleRuntime {
            state_directory: directory.path(),
            home_directory: Some(directory.home()),
            agent_config: namedrop::AgentConfigDirectories::default(),
            cwd: Path::new("/repo"),
            workspace_id: None,
            current_pane_id: Some("pane-sender"),
            session_socket_id: "socket-a",
            mux: &*herdr,
            clock: &FixedClock,
            ids: &ids,
        };
        let before = store
            .transaction(|tx| tx.transition_facts(&namespace))
            .unwrap();
        // A delivery ID must never bypass exact attempted-delivery validation.
        for invalid in [
            serde_json::json!({"delivery_id":"delivery-1","confirmed":true}),
            serde_json::json!({"attempt_id":"attempt-1","delivery_id":"delivery-1","confirmed":true}),
            serde_json::json!({"confirmed":true}),
        ] {
            assert!(execute_control_operation("delivery.cancel-held", invalid, &runtime).is_err());
        }
        assert!(
            execute_control_operation(
                "delivery.cancel-held",
                serde_json::json!({"attempt_id":"attempt-1"}),
                &runtime
            )
            .is_err()
        );
        assert_eq!(
            store
                .transaction(|tx| tx.transition_facts(&namespace))
                .unwrap(),
            before
        );
        if state == DeliveryState::Held {
            let report = namedrop::diagnose_collaboration(&runtime);
            let action = report
                .issues
                .iter()
                .map(|issue| serde_json::to_value(issue).unwrap())
                .find(|issue| issue["repair"]["operation"] == "delivery.cancel-held")
                .unwrap();
            assert_eq!(action["diagnostic"]["attempt_id"], "attempt-1");
            assert_eq!(action["diagnostic"]["thread_id"], "thread-1");
            assert_eq!(
                action["repair"]["input"],
                serde_json::json!({"attempt_id":"attempt-1","confirmed":true})
            );
        }
        let result = execute_control_operation(
            "delivery.cancel-held",
            serde_json::json!({"attempt_id":"attempt-1","confirmed":true}),
            &runtime,
        );
        if state == DeliveryState::Held {
            let report = namedrop::diagnose_collaboration(&runtime);
            assert!(report.issues.iter().all(
                |issue| serde_json::to_value(issue).unwrap()["repair"]["operation"]
                    != "delivery.cancel-held"
            ));
            assert_eq!(result.unwrap()["state"], "cancelled");
            assert!(
                execute_control_operation(
                    "delivery.cancel-held",
                    serde_json::json!({"attempt_id":"attempt-1","confirmed":true}),
                    &runtime
                )
                .is_err()
            );
        } else {
            assert!(result.is_err(), "{state:?} was cancellable");
        }
        assert!(herdr.attempts().is_empty());
    }
}

fn settled_attempt_without_disposition(directory: &TestDirectory, reply: bool) {
    use namedrop::{DeliveryAttemptId, DeliveryState, LeaseDraft, PaneBindingId};
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let human = attribution(ActorKind::Human, "human", 1);
    let recipient = PaneAddressId::new("address-recipient");
    let session = AgentSessionId::new("session-recipient");
    let attempt = DeliveryAttemptId::new("attempt-1");
    let thread = ThreadId::new("thread-1");
    StateStore::open(directory.path())
        .unwrap()
        .transaction(|tx| {
            tx.activate_worktree(&namespace, &human)?;
            tx.create_pane_address(&namespace, &address("address-sender", "sender"), &human)?;
            tx.create_pane_address(
                &namespace,
                &address("address-recipient", "recipient"),
                &human,
            )?;
            tx.bind_pane(
                &namespace,
                &PaneBindingId::new("binding-recipient"),
                &recipient,
                "pane-recipient",
                &human,
            )?;
            tx.observe_agent_session(
                &namespace,
                "observation-recipient",
                &recipient,
                &session,
                "working",
                &human,
            )?;
            tx.create_thread(&namespace, &thread, "Disposition test", &human)?;
            let sender = PaneAddressId::new("address-sender");
            tx.append_message(
                &namespace,
                &message(1, &thread, &sender, ActorKind::Human, &recipient, None),
                &human,
            )?;
            tx.lease_mailbox_head(
                &namespace,
                &recipient,
                &LeaseDraft {
                    attempt_id: attempt.clone(),
                    binding_id: PaneBindingId::new("binding-recipient"),
                    agent_session_id: session.clone(),
                    retry_of_attempt_id: None,
                    owner: "test".into(),
                    acquired_at_ms: 2,
                    expires_at_ms: 1000,
                },
                &human,
            )?;
            tx.transition_delivery_attempt(&namespace, &attempt, DeliveryState::Submitted, &human)?;
            tx.acknowledge_delivery_attempt(&namespace, &attempt, "ack-1", &session, &human)?;
            if reply {
                tx.append_message(
                    &namespace,
                    &message(
                        2,
                        &thread,
                        &recipient,
                        ActorKind::AgentSession,
                        &sender,
                        Some(MessageId::new("message-1")),
                    ),
                    &human,
                )?;
                tx.record_reply_disposition(
                    &namespace,
                    &attempt,
                    &MessageId::new("message-2"),
                    &session,
                    &human,
                )?;
            } else {
                tx.transition_delivery_attempt(
                    &namespace,
                    &attempt,
                    DeliveryState::Settled,
                    &human,
                )?;
            }
            Ok(())
        })
        .unwrap();
}

fn retire_recipient(directory: &TestDirectory) {
    StateStore::open(directory.path())
        .unwrap()
        .transaction(|tx| {
            tx.retire_pane_address(
                &StateNamespace::for_space("socket-a", "workspace-1"),
                &PaneAddressId::new("address-recipient"),
                &attribution(ActorKind::Human, "human", 3),
            )
        })
        .unwrap();
}

#[test]
fn a_human_can_acknowledge_a_missing_disposition_only_for_a_retired_recipient() {
    use clap::Parser;
    let cli = namedrop::cli::Cli::try_parse_from([
        "namedrop",
        "delivery",
        "acknowledge",
        "--input-json",
        r#"{"attempt_id":"attempt-1","confirmed":true}"#,
    ])
    .unwrap();
    assert_eq!(cli.operation(), "delivery.acknowledge");
    assert!(namedrop::is_control_operation("delivery.acknowledge"));
    let directory = TestDirectory::new();
    settled_attempt_without_disposition(&directory, false);
    let herdr = fake_herdr();
    let ids = SequenceIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-sender"),
        session_socket_id: "socket-a",
        mux: &*herdr,
        clock: &FixedClock,
        ids: &ids,
    };
    let warns = || {
        namedrop::diagnose_collaboration(&runtime)
            .issues
            .iter()
            .any(|issue| issue.code == "ND-DELIVERY-003")
    };
    let db = || rusqlite::Connection::open(directory.path().join("namedrop.sqlite3")).unwrap();
    let state_and_dispositions = || -> (String, i64) {
        db().query_row(
            "SELECT state, (SELECT count(*) FROM delivery_dispositions WHERE attempt_id='attempt-1')
             FROM delivery_attempt_state WHERE attempt_id='attempt-1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap()
    };
    assert!(warns());
    let input = serde_json::json!({"attempt_id":"attempt-1","confirmed":true});
    let live =
        execute_control_operation("delivery.acknowledge", input.clone(), &runtime).unwrap_err();
    assert!(live.contains("not retired"), "{live}");
    assert!(
        warns(),
        "a live recipient can still record its own disposition"
    );

    retire_recipient(&directory);
    let unconfirmed = execute_control_operation(
        "delivery.acknowledge",
        serde_json::json!({"attempt_id":"attempt-1"}),
        &runtime,
    )
    .unwrap_err();
    assert!(unconfirmed.contains("confirmation"), "{unconfirmed}");
    assert!(warns());

    let result =
        execute_control_operation("delivery.acknowledge", input.clone(), &runtime).unwrap();
    assert_eq!(result["acknowledged"], true);
    assert_eq!(result["changed"], true);
    assert_eq!(state_and_dispositions(), ("settled".to_owned(), 0));
    assert!(!warns());
    for _ in 0..2 {
        namedrop::drain_mailboxes(&runtime).unwrap();
        assert!(
            !warns(),
            "reconciliation must not reopen an acknowledged warning"
        );
    }
    let repeated = execute_control_operation("delivery.acknowledge", input, &runtime).unwrap();
    assert_eq!(repeated["changed"], false);
    let acknowledgements: i64 = db()
        .query_row(
            "SELECT count(*) FROM transition_facts WHERE entity_kind='attention_item' AND transition_kind='acknowledged' AND actor_kind='human'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(acknowledgements, 1);
    assert_eq!(state_and_dispositions(), ("settled".to_owned(), 0));
    assert!(herdr.attempts().is_empty());
}

#[test]
fn disposition_acknowledgement_refuses_agents_unknown_attempts_and_recorded_dispositions() {
    use namedrop::DeliveryAttemptId;
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let acknowledge = |directory: &TestDirectory, attempt: &str, actor_kind: ActorKind| {
        StateStore::open(directory.path())
            .unwrap()
            .transaction(|tx| {
                tx.acknowledge_disposition_attention(
                    &namespace,
                    &DeliveryAttemptId::new(attempt),
                    &attribution(actor_kind, "actor", 4),
                )
            })
            .unwrap_err()
            .to_string()
    };
    let missing = TestDirectory::new();
    settled_attempt_without_disposition(&missing, false);
    retire_recipient(&missing);
    for actor_kind in [
        ActorKind::AgentSession,
        ActorKind::Plugin,
        ActorKind::System,
    ] {
        let error = acknowledge(&missing, "attempt-1", actor_kind);
        assert!(error.contains("only a human"), "{error}");
    }
    let error = acknowledge(&missing, "attempt-unknown", ActorKind::Human);
    assert!(error.contains("does not exist"), "{error}");

    let replied = TestDirectory::new();
    settled_attempt_without_disposition(&replied, true);
    retire_recipient(&replied);
    let error = acknowledge(&replied, "attempt-1", ActorKind::Human);
    assert!(error.contains("no disposition warning"), "{error}");
}

#[test]
fn expired_unsent_delivery_has_truthful_health_and_confirmed_exact_cancellation() {
    struct LateClock;
    impl Clock for LateClock {
        fn now_unix_ms(&self) -> u64 {
            86_400_100
        }
    }
    let directory = TestDirectory::new();
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let human = attribution(ActorKind::Human, "human", 1);
    let mut store = StateStore::open(directory.path()).unwrap();
    store
        .transaction(|tx| {
            tx.activate_worktree(&namespace, &human)?;
            tx.create_pane_address(&namespace, &address("address-sender", "sender"), &human)?;
            tx.create_pane_address(
                &namespace,
                &address("address-recipient", "recipient"),
                &human,
            )?;
            tx.create_thread(
                &namespace,
                &ThreadId::new("expired-thread"),
                "Expired message",
                &human,
            )?;
            tx.append_message(
                &namespace,
                &message(
                    1,
                    &ThreadId::new("expired-thread"),
                    &PaneAddressId::new("address-sender"),
                    ActorKind::Human,
                    &PaneAddressId::new("address-recipient"),
                    None,
                ),
                &human,
            )?;
            tx.complete_reconciliation(&namespace)?;
            Ok(())
        })
        .unwrap();
    let herdr = fake_herdr();
    let ids = SequenceIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-sender"),
        session_socket_id: "socket-a",
        mux: &*herdr,
        clock: &LateClock,
        ids: &ids,
    };
    assert_eq!(
        namedrop::drain_mailboxes(&runtime).unwrap().expired,
        vec![DeliveryId::new("delivery-1")]
    );
    let report = namedrop::diagnose_collaboration(&runtime);
    let issue = report
        .issues
        .iter()
        .find(|issue| issue.repair.operation.as_deref() == Some("delivery.cancel-held"))
        .expect("expired unsent delivery needs a usable cancellation target");
    assert!(issue.explanation.contains("before dispatch"), "{issue:?}");
    assert!(!issue.explanation.contains("uncertain"));
    assert_eq!(issue.diagnostic["attempt_id"], serde_json::Value::Null);
    assert_eq!(issue.diagnostic["delivery_id"], "delivery-1");
    assert_eq!(issue.diagnostic["message_id"], "message-1");
    assert_eq!(issue.diagnostic["thread_id"], "expired-thread");
    let expected = serde_json::json!({"delivery_id":"delivery-1","confirmed":true});
    assert_eq!(issue.repair.input.as_ref(), Some(&expected));
    let rendered = namedrop::render_health_report_to_text(&report);
    assert!(
        rendered.contains("delivery-1") && rendered.contains("Command:"),
        "{rendered}"
    );
    for input in [
        serde_json::json!({"delivery_id":"delivery-1"}),
        serde_json::json!({"attempt_id":"missing","delivery_id":"delivery-1","confirmed":true}),
    ] {
        assert!(execute_control_operation("delivery.cancel-held", input, &runtime).is_err());
    }
    let result =
        execute_control_operation("delivery.cancel-held", expected.clone(), &runtime).unwrap();
    assert_eq!(result["state"], "cancelled");
    assert_eq!(result["delivery_id"], "delivery-1");
    assert!(execute_control_operation("delivery.cancel-held", expected, &runtime).is_err());
    assert!(
        namedrop::diagnose_collaboration(&runtime)
            .issues
            .iter()
            .all(|issue| issue.repair.operation.as_deref() != Some("delivery.cancel-held"))
    );
    assert!(herdr.attempts().is_empty());
}

struct TestDirectory {
    path: PathBuf,
    home: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "namedrop-controls-test-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        let home = path.join("home");
        fs::create_dir(&home).unwrap();
        Self { path, home }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    /// Stands in for the user's home directory, so no test installs an Agent
    /// Integration package into the developer's real user-level skill roots.
    fn home(&self) -> &Path {
        &self.home
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        cleanup::remove_fixture(&self.path).unwrap();
    }
}

fn attribution(actor_kind: ActorKind, actor_id: &str, occurred_at_ms: u64) -> MutationAttribution {
    MutationAttribution::new(actor_kind, actor_id, occurred_at_ms)
}

fn address(id: &str, alias: &str) -> PaneAddressDraft {
    PaneAddressDraft {
        id: PaneAddressId::new(id),
        role_id: PaneRoleId::new(format!("role-{id}")),
        role_name: alias.to_owned(),
        role_description: format!("{alias} test role"),
        alias: alias.to_owned(),
    }
}

fn message(
    sequence: u64,
    thread_id: &ThreadId,
    author: &PaneAddressId,
    author_kind: ActorKind,
    recipient: &PaneAddressId,
    in_reply_to: Option<MessageId>,
) -> MessageDraft {
    MessageDraft {
        thread_id: thread_id.clone(),
        message_id: MessageId::new(format!("message-{sequence}")),
        in_reply_to,
        author_address_id: author.clone(),
        author_kind,
        author_alias_snapshot: author.as_str().to_owned(),
        author_agent_session_snapshot: (author_kind == ActorKind::AgentSession)
            .then(|| AgentSessionId::new(format!("session-{}", author.as_str()))),
        request_id: format!("request-{sequence}"),
        authored_source: "@recipient Test message.".to_owned(),
        body: "Test message.".to_owned(),
        grammar_version: "mention/v1".to_owned(),
        recipients: vec![MessageRecipientDraft {
            address_id: recipient.clone(),
            alias_snapshot: recipient.as_str().to_owned(),
            agent_session_snapshot: None,
            delivery_id: DeliveryId::new(format!("delivery-{sequence}")),
        }],
        context_references: Vec::new(),
    }
}

#[test]
fn ten_agent_replies_pause_the_thread_and_continue_grants_exactly_ten_more() {
    let directory = TestDirectory::new();
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let thread_id = ThreadId::new("thread-1");
    let human = PaneAddressId::new("address-human");
    let agent = PaneAddressId::new("address-agent");
    let mut store = StateStore::open(directory.path()).unwrap();
    store
        .transaction(|transaction| {
            transaction
                .activate_worktree(&namespace, &attribution(ActorKind::Human, "local-human", 1))?;
            transaction.create_pane_address(
                &namespace,
                &address("address-human", "human"),
                &attribution(ActorKind::Human, "local-human", 2),
            )?;
            transaction.create_pane_address(
                &namespace,
                &address("address-agent", "agent"),
                &attribution(ActorKind::Human, "local-human", 3),
            )?;
            transaction.create_thread(
                &namespace,
                &thread_id,
                "Reply limit",
                &attribution(ActorKind::Human, "local-human", 4),
            )?;
            transaction.append_message(
                &namespace,
                &message(1, &thread_id, &human, ActorKind::Human, &agent, None),
                &attribution(ActorKind::Human, "local-human", 5),
            )?;
            for sequence in 2..=11 {
                transaction.append_message(
                    &namespace,
                    &message(
                        sequence,
                        &thread_id,
                        &agent,
                        ActorKind::AgentSession,
                        &human,
                        Some(MessageId::new("message-1")),
                    ),
                    &attribution(
                        ActorKind::AgentSession,
                        "session-address-agent",
                        sequence + 4,
                    ),
                )?;
            }
            Ok(())
        })
        .unwrap();

    let state = store
        .transaction(|transaction| transaction.thread_control_state(&namespace, &thread_id))
        .unwrap();
    assert!(state.paused);
    assert!(state.reply_limit_paused);
    assert_eq!(state.reply_allowance, 0);

    let rejected = store.transaction(|transaction| {
        transaction.append_message(
            &namespace,
            &message(
                12,
                &thread_id,
                &agent,
                ActorKind::AgentSession,
                &human,
                Some(MessageId::new("message-1")),
            ),
            &attribution(ActorKind::AgentSession, "session-address-agent", 16),
        )
    });
    assert_eq!(rejected.unwrap_err().kind(), StoreErrorKind::InvalidInput);

    let continued = store
        .transaction(|transaction| {
            transaction.continue_thread(
                &namespace,
                &thread_id,
                &attribution(ActorKind::Human, "local-human", 17),
            )
        })
        .unwrap();
    assert!(!continued.paused);
    assert!(!continued.reply_limit_paused);
    assert_eq!(continued.reply_allowance, 10);

    store
        .transaction(|transaction| {
            for sequence in 12..=21 {
                transaction.append_message(
                    &namespace,
                    &message(
                        sequence,
                        &thread_id,
                        &agent,
                        ActorKind::AgentSession,
                        &human,
                        Some(MessageId::new("message-1")),
                    ),
                    &attribution(
                        ActorKind::AgentSession,
                        "session-address-agent",
                        sequence + 6,
                    ),
                )?;
            }
            Ok(())
        })
        .unwrap();
    let second_limit = store
        .transaction(|transaction| {
            Ok((
                transaction.thread_control_state(&namespace, &thread_id)?,
                transaction.unresolved_attention_items(&namespace)?,
            ))
        })
        .unwrap();
    assert!(second_limit.0.reply_limit_paused);
    assert_eq!(second_limit.0.reply_allowance, 0);
    assert_eq!(second_limit.1.len(), 1);
}

fn reply_limit_store(directory: &TestDirectory) -> StateStore {
    let mut store = StateStore::open(directory.path()).unwrap();
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let human = attribution(ActorKind::Human, "local-human", 1);
    store
        .transaction(|transaction| {
            transaction.activate_worktree(&namespace, &human)?;
            for (id, alias) in [
                ("address-human", "human"),
                ("address-agent-a", "agent-a"),
                ("address-agent-b", "agent-b"),
            ] {
                transaction.create_pane_address(&namespace, &address(id, alias), &human)?;
            }
            transaction.bind_pane(
                &namespace,
                &namedrop::PaneBindingId::new("binding-agent-b"),
                &PaneAddressId::new("address-agent-b"),
                "pane-agent-b",
                &human,
            )?;
            transaction.observe_agent_session(
                &namespace,
                "observation-agent-b",
                &PaneAddressId::new("address-agent-b"),
                &AgentSessionId::new("session-address-agent-b"),
                "working",
                &human,
            )?;
            transaction.create_thread(
                &namespace,
                &ThreadId::new("thread-1"),
                "Reply limit reset",
                &human,
            )?;
            transaction.append_message(&namespace, &human_reply(1), &human)?;
            Ok(())
        })
        .unwrap();
    store
}

fn human_reply(sequence: u64) -> MessageDraft {
    message(
        sequence,
        &ThreadId::new("thread-1"),
        &PaneAddressId::new("address-human"),
        ActorKind::Human,
        &PaneAddressId::new(if sequence.is_multiple_of(2) {
            "address-agent-b"
        } else {
            "address-agent-a"
        }),
        (sequence > 1).then(|| MessageId::new(format!("message-{}", sequence - 1))),
    )
}

fn agent_ping_pong_reply(sequence: u64) -> MessageDraft {
    let (author, recipient) = if sequence.is_multiple_of(2) {
        ("address-agent-a", "address-agent-b")
    } else {
        ("address-agent-b", "address-agent-a")
    };
    message(
        sequence,
        &ThreadId::new("thread-1"),
        &PaneAddressId::new(author),
        ActorKind::AgentSession,
        &PaneAddressId::new(recipient),
        Some(MessageId::new(format!("message-{}", sequence - 1))),
    )
}

fn append_agent_ping_pong(store: &mut StateStore, sequences: std::ops::RangeInclusive<u64>) {
    store
        .transaction(|transaction| {
            for sequence in sequences {
                let draft = agent_ping_pong_reply(sequence);
                transaction.append_message(
                    &StateNamespace::for_space("socket-a", "workspace-1"),
                    &draft,
                    &attribution(
                        ActorKind::AgentSession,
                        draft
                            .author_agent_session_snapshot
                            .as_ref()
                            .unwrap()
                            .as_str(),
                        sequence,
                    ),
                )?;
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn human_message_midway_refills_the_reply_allowance() {
    let directory = TestDirectory::new();
    let mut store = reply_limit_store(&directory);
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let thread = ThreadId::new("thread-1");
    append_agent_ping_pong(&mut store, 2..=5);
    let before = store
        .transaction(|transaction| transaction.thread_control_state(&namespace, &thread))
        .unwrap();
    assert_eq!(before.reply_allowance, 6);

    let after = store
        .transaction(|transaction| {
            transaction.append_message(
                &namespace,
                &human_reply(6),
                &attribution(ActorKind::Plugin, "human-message-relay", 6),
            )?;
            transaction.thread_control_state(&namespace, &thread)
        })
        .unwrap();
    assert_eq!(after.reply_allowance, 10);
    assert!(!after.paused);
    assert!(!after.stopped);
}

#[test]
fn human_message_continues_a_limit_pause_and_releases_a_queued_agent_delivery() {
    let directory = TestDirectory::new();
    let mut store = reply_limit_store(&directory);
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let thread = ThreadId::new("thread-1");
    let human = attribution(ActorKind::Human, "local-human", 12);
    let recipient = PaneAddressId::new("address-agent-b");
    let lease = namedrop::LeaseDraft {
        attempt_id: namedrop::DeliveryAttemptId::new("attempt-held-agent-reply"),
        binding_id: namedrop::PaneBindingId::new("binding-agent-b"),
        agent_session_id: AgentSessionId::new("session-address-agent-b"),
        retry_of_attempt_id: None,
        owner: "test".to_owned(),
        acquired_at_ms: 12,
        expires_at_ms: 112,
    };
    append_agent_ping_pong(&mut store, 2..=11);
    store
        .transaction(|transaction| {
            let state = transaction.thread_control_state(&namespace, &thread)?;
            assert!(state.reply_limit_paused);
            assert_eq!(state.reply_allowance, 0);
            let attention = transaction.unresolved_attention_items(&namespace)?;
            assert_eq!(attention.len(), 1);
            assert_eq!(
                attention[0].source_kind,
                namedrop::AttentionSourceKind::AutomaticReplyLimit
            );
            assert_eq!(attention[0].source_id, thread.as_str());
            assert!(
                transaction
                    .lease_mailbox_head(&namespace, &recipient, &lease, &human)?
                    .is_none()
            );
            Ok(())
        })
        .unwrap();

    store
        .transaction(|transaction| {
            transaction.append_message(&namespace, &human_reply(12), &human)?;
            let state = transaction.thread_control_state(&namespace, &thread)?;
            assert_eq!(state.reply_allowance, 10);
            assert!(!state.paused);
            assert!(!state.reply_limit_paused);
            assert!(
                transaction
                    .unresolved_attention_items(&namespace)?
                    .is_empty()
            );
            assert!(
                transaction
                    .transition_facts(&namespace)?
                    .iter()
                    .any(|fact| {
                        fact.entity_kind == namedrop::TransitionEntityKind::CollaborationThread
                            && fact.entity_id == thread.as_str()
                            && fact.transition_kind == namedrop::TransitionKind::Continued
                            && fact.actor_kind == ActorKind::Human
                    })
            );
            let delivered = transaction
                .lease_mailbox_head(&namespace, &recipient, &lease, &human)?
                .expect("the queued agent reply is deliverable after the human message");
            assert_eq!(delivered.delivery.message_id, MessageId::new("message-2"));
            Ok(())
        })
        .unwrap();

    append_agent_ping_pong(&mut store, 13..=22);
    let state = store
        .transaction(|transaction| transaction.thread_control_state(&namespace, &thread))
        .unwrap();
    assert_eq!(state.reply_allowance, 0);
    assert!(state.reply_limit_paused);
}

#[test]
fn human_message_refills_but_preserves_a_manual_pause() {
    let directory = TestDirectory::new();
    let mut store = reply_limit_store(&directory);
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let thread = ThreadId::new("thread-1");
    append_agent_ping_pong(&mut store, 2..=4);
    let state = store
        .transaction(|transaction| {
            let human = attribution(ActorKind::Human, "local-human", 5);
            let paused = transaction.pause_thread(&namespace, &thread, &human)?;
            assert_eq!(paused.reply_allowance, 7);
            assert!(paused.paused);
            assert!(!paused.reply_limit_paused);
            transaction.append_message(&namespace, &human_reply(5), &human)?;
            transaction.thread_control_state(&namespace, &thread)
        })
        .unwrap();
    assert_eq!(state.reply_allowance, 10);
    assert!(state.paused);
    assert!(!state.reply_limit_paused);
    assert!(!state.stopped);
}

#[test]
fn human_reply_cannot_reopen_or_refill_a_stopped_thread() {
    let directory = TestDirectory::new();
    let mut store = reply_limit_store(&directory);
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let thread = ThreadId::new("thread-1");
    append_agent_ping_pong(&mut store, 2..=4);
    let human = attribution(ActorKind::Human, "local-human", 5);
    let stopped = store
        .transaction(|transaction| transaction.stop_thread(&namespace, &thread, &human))
        .unwrap()
        .state;
    assert!(stopped.stopped);
    assert_eq!(stopped.reply_allowance, 7);
    let rejected = store
        .transaction(|transaction| transaction.append_message(&namespace, &human_reply(5), &human));
    assert_eq!(rejected.unwrap_err().kind(), StoreErrorKind::InvalidInput);
    let after = store
        .transaction(|transaction| transaction.thread_control_state(&namespace, &thread))
        .unwrap();
    assert_eq!(after, stopped);
}

#[test]
fn agent_messages_never_refill_even_with_human_mutation_attribution() {
    let directory = TestDirectory::new();
    let mut store = reply_limit_store(&directory);
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let thread = ThreadId::new("thread-1");
    append_agent_ping_pong(&mut store, 2..=4);
    store
        .transaction(|transaction| {
            let human = attribution(ActorKind::Human, "local-human", 5);
            transaction.append_message(&namespace, &agent_ping_pong_reply(5), &human)?;
            assert_eq!(
                transaction
                    .thread_control_state(&namespace, &thread)?
                    .reply_allowance,
                6
            );
            let mut non_reply = agent_ping_pong_reply(6);
            non_reply.in_reply_to = None;
            transaction.append_message(&namespace, &non_reply, &human)?;
            assert_eq!(
                transaction
                    .thread_control_state(&namespace, &thread)?
                    .reply_allowance,
                6
            );
            Ok(())
        })
        .unwrap();
}

#[test]
fn duplicate_human_message_replay_does_not_refill_again() {
    let directory = TestDirectory::new();
    let mut store = reply_limit_store(&directory);
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let thread = ThreadId::new("thread-1");
    append_agent_ping_pong(&mut store, 2..=4);
    let draft = human_reply(5);
    store
        .transaction(|transaction| {
            let receipt = transaction.append_message(
                &namespace,
                &draft,
                &attribution(ActorKind::Human, "local-human", 5),
            )?;
            assert!(!receipt.idempotent_replay);
            assert_eq!(
                transaction
                    .thread_control_state(&namespace, &thread)?
                    .reply_allowance,
                10
            );
            Ok(())
        })
        .unwrap();
    append_agent_ping_pong(&mut store, 6..=6);
    store
        .transaction(|transaction| {
            let before = transaction.thread_control_state(&namespace, &thread)?;
            assert_eq!(before.reply_allowance, 9);
            let transitions = transaction.transition_facts(&namespace)?;
            let receipt = transaction.append_message(
                &namespace,
                &draft,
                &attribution(ActorKind::Human, "local-human", 7),
            )?;
            assert!(receipt.idempotent_replay);
            assert_eq!(
                transaction.thread_control_state(&namespace, &thread)?,
                before
            );
            assert_eq!(transaction.transition_facts(&namespace)?, transitions);
            Ok(())
        })
        .unwrap();
}

#[test]
fn human_controls_hold_fifo_cancel_only_permitted_work_and_never_disable_ordinary_panes() {
    let directory = TestDirectory::new();
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let first_thread = ThreadId::new("thread-1");
    let second_thread = ThreadId::new("thread-2");
    let sender = PaneAddressId::new("address-sender");
    let recipient = PaneAddressId::new("address-recipient");
    let human = attribution(ActorKind::Human, "local-human", 1);
    let mut store = StateStore::open(directory.path()).unwrap();
    store
        .transaction(|transaction| {
            transaction.activate_worktree(&namespace, &human)?;
            transaction.create_pane_address(
                &namespace,
                &address("address-sender", "sender"),
                &human,
            )?;
            transaction.create_pane_address(
                &namespace,
                &address("address-recipient", "recipient"),
                &human,
            )?;
            for (thread_id, sequence) in [(&first_thread, 1), (&second_thread, 2)] {
                transaction.create_thread(&namespace, thread_id, "Controls", &human)?;
                transaction.append_message(
                    &namespace,
                    &message(
                        sequence,
                        thread_id,
                        &sender,
                        ActorKind::Human,
                        &recipient,
                        None,
                    ),
                    &human,
                )?;
            }
            Ok(())
        })
        .unwrap();

    store
        .transaction(|transaction| {
            assert!(
                transaction
                    .pause_thread(&namespace, &first_thread, &human)?
                    .paused
            );
            assert!(
                !transaction
                    .resume_thread(&namespace, &first_thread, &human)?
                    .paused
            );
            assert!(
                transaction
                    .set_worktree_paused(&namespace, true, &human)?
                    .paused
            );
            assert!(
                !transaction
                    .set_worktree_paused(&namespace, false, &human)?
                    .paused
            );
            assert!(
                transaction
                    .set_address_collaboration_disabled(&namespace, &sender, true, &human)?
                    .collaboration_disabled
            );
            Ok(())
        })
        .unwrap();

    let disabled_send = store.transaction(|transaction| {
        let blocked_thread = ThreadId::new("thread-blocked");
        transaction.create_thread(&namespace, &blocked_thread, "Blocked", &human)?;
        transaction.append_message(
            &namespace,
            &message(
                3,
                &blocked_thread,
                &sender,
                ActorKind::Human,
                &recipient,
                None,
            ),
            &human,
        )
    });
    assert_eq!(
        disabled_send.unwrap_err().kind(),
        StoreErrorKind::InvalidInput
    );

    store
        .transaction(|transaction| {
            transaction.set_address_collaboration_disabled(&namespace, &sender, false, &human)?;
            assert_eq!(
                transaction.cancel_sender_delivery(
                    &namespace,
                    &sender,
                    &DeliveryId::new("delivery-1"),
                    &human,
                )?,
                namedrop::DeliveryState::Cancelled
            );
            assert_eq!(
                transaction
                    .cancel_sender_delivery(
                        &namespace,
                        &recipient,
                        &DeliveryId::new("delivery-2"),
                        &human,
                    )
                    .unwrap_err()
                    .kind(),
                StoreErrorKind::InvalidInput
            );
            let stopped = transaction.stop_thread(&namespace, &second_thread, &human)?;
            assert!(stopped.state.stopped);
            assert_eq!(
                stopped.cancelled_delivery_ids,
                [DeliveryId::new("delivery-2")]
            );
            Ok(())
        })
        .unwrap();

    let stopped_reply = store.transaction(|transaction| {
        transaction.append_message(
            &namespace,
            &message(
                4,
                &second_thread,
                &recipient,
                ActorKind::Human,
                &sender,
                Some(MessageId::new("message-2")),
            ),
            &human,
        )
    });
    assert_eq!(
        stopped_reply.unwrap_err().kind(),
        StoreErrorKind::InvalidInput
    );
}

struct FixedClock;

impl Clock for FixedClock {
    fn now_unix_ms(&self) -> u64 {
        100
    }
}

struct SequenceIds(AtomicU64);

impl IdGenerator for SequenceIds {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
    }
}

/// The world the old `FakeHerdr` answered: `/repo` open in workspace-1,
/// listed through `CHECKOUT_LISTINGS` only, a sender pane with no session
/// field, and a working recipient that reports a session but no agent kind.
/// It takes only ctrl+c to the recipient's agent, as the old fake answered
/// every other key with `invalid_key` and failed every other command.
fn fake_herdr() -> StrictSpace {
    let mux = FakeMux::world()
        .endpoint("socket-a")
        .space("workspace-1")
        .checkout("/repo")
        .unstamped()
        .pane("pane-sender")
        .terminal("terminal-sender")
        .tab("tab-1")
        .reports_session(false)
        .pane("pane-recipient")
        .terminal("terminal-recipient")
        .tab("tab-1")
        .session("session-recipient")
        .working()
        .build();
    StrictSpace::new(
        mux,
        CHECKOUT_LISTINGS,
        Takes::nothing().input(|attempt| *attempt == interrupt_recipient()),
    )
}

/// ctrl+c typed to the recipient's agent. Its Herdr spelling, `ctrl+c`, is
/// pinned by the adapter's own tests.
fn interrupt_recipient() -> Attempt {
    Attempt {
        pane: PaneId::new("pane-recipient"),
        input: SentInput::Keys {
            recipient: Recipient::Agent,
            keys: vec![Key::CtrlC],
        },
        outcome: InputOutcome::Delivered,
    }
}

#[test]
fn control_operations_mutate_durable_state_and_stop_only_a_proven_thread_response() {
    let directory = TestDirectory::new();
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let thread_id = ThreadId::new("thread-1");
    let sender = PaneAddressId::new("address-sender");
    let recipient = PaneAddressId::new("address-recipient");
    let human = attribution(ActorKind::Human, "local-human", 1);
    let mut store = StateStore::open(directory.path()).unwrap();
    store
        .transaction(|transaction| {
            transaction.activate_worktree(&namespace, &human)?;
            transaction.create_pane_address(
                &namespace,
                &address("address-sender", "sender"),
                &human,
            )?;
            transaction.create_pane_address(
                &namespace,
                &address("address-recipient", "recipient"),
                &human,
            )?;
            transaction.bind_pane(
                &namespace,
                &namedrop::PaneBindingId::new("binding-sender"),
                &sender,
                "pane-sender",
                &human,
            )?;
            transaction.bind_pane(
                &namespace,
                &namedrop::PaneBindingId::new("binding-recipient"),
                &recipient,
                "pane-recipient",
                &human,
            )?;
            transaction.observe_agent_session(
                &namespace,
                "observation-recipient",
                &recipient,
                &AgentSessionId::new("session-recipient"),
                "working",
                &human,
            )?;
            transaction.create_thread(&namespace, &thread_id, "Controls", &human)?;
            transaction.append_message(
                &namespace,
                &message(1, &thread_id, &sender, ActorKind::Human, &recipient, None),
                &human,
            )?;
            let cancellable_thread = ThreadId::new("thread-cancellable");
            transaction.create_thread(
                &namespace,
                &cancellable_thread,
                "Cancellable delivery",
                &human,
            )?;
            transaction.append_message(
                &namespace,
                &message(
                    2,
                    &cancellable_thread,
                    &sender,
                    ActorKind::Human,
                    &recipient,
                    None,
                ),
                &human,
            )?;
            transaction.pause_thread(&namespace, &thread_id, &human)?;
            assert!(
                transaction
                    .lease_mailbox_head(
                        &namespace,
                        &recipient,
                        &namedrop::LeaseDraft {
                            attempt_id: namedrop::DeliveryAttemptId::new("attempt-paused",),
                            binding_id: namedrop::PaneBindingId::new("binding-recipient",),
                            agent_session_id: AgentSessionId::new("session-recipient"),
                            retry_of_attempt_id: None,
                            owner: "test".to_owned(),
                            acquired_at_ms: 2,
                            expires_at_ms: 102,
                        },
                        &human,
                    )?
                    .is_none()
            );
            transaction.resume_thread(&namespace, &thread_id, &human)?;
            transaction.set_address_collaboration_disabled(&namespace, &recipient, true, &human)?;
            assert!(
                transaction
                    .lease_mailbox_head(
                        &namespace,
                        &recipient,
                        &namedrop::LeaseDraft {
                            attempt_id: namedrop::DeliveryAttemptId::new("attempt-disabled",),
                            binding_id: namedrop::PaneBindingId::new("binding-recipient",),
                            agent_session_id: AgentSessionId::new("session-recipient"),
                            retry_of_attempt_id: None,
                            owner: "test".to_owned(),
                            acquired_at_ms: 2,
                            expires_at_ms: 102,
                        },
                        &human,
                    )?
                    .is_none()
            );
            transaction
                .set_address_collaboration_disabled(&namespace, &recipient, false, &human)?;
            transaction.set_worktree_paused(&namespace, true, &human)?;
            assert!(
                transaction
                    .lease_mailbox_head(
                        &namespace,
                        &recipient,
                        &namedrop::LeaseDraft {
                            attempt_id: namedrop::DeliveryAttemptId::new(
                                "attempt-worktree-paused",
                            ),
                            binding_id: namedrop::PaneBindingId::new(
                                "binding-recipient",
                            ),
                            agent_session_id: AgentSessionId::new("session-recipient"),
                            retry_of_attempt_id: None,
                            owner: "test".to_owned(),
                            acquired_at_ms: 2,
                            expires_at_ms: 102,
                        },
                        &human,
                    )?
                    .is_none()
            );
            transaction.set_worktree_paused(&namespace, false, &human)?;
            transaction.lease_mailbox_head(
                &namespace,
                &recipient,
                &namedrop::LeaseDraft {
                    attempt_id: namedrop::DeliveryAttemptId::new("attempt-1"),
                    binding_id: namedrop::PaneBindingId::new("binding-recipient"),
                    agent_session_id: AgentSessionId::new("session-recipient"),
                    retry_of_attempt_id: None,
                    owner: "test".to_owned(),
                    acquired_at_ms: 2,
                    expires_at_ms: 102,
                },
                &human,
            )?;
            transaction.transition_delivery_attempt(
                &namespace,
                &namedrop::DeliveryAttemptId::new("attempt-1"),
                namedrop::DeliveryState::Submitted,
                &human,
            )
        })
        .unwrap();
    let herdr = fake_herdr();
    let ids = SequenceIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-sender"),
        session_socket_id: "socket-a",
        mux: &*herdr,
        clock: &FixedClock,
        ids: &ids,
    };

    let disabled = execute_control_operation(
        "address.disable",
        serde_json::json!({"address_id": "address-sender"}),
        &runtime,
    )
    .unwrap();
    assert_eq!(disabled["collaboration_disabled"], true);
    let enabled = execute_control_operation(
        "address.enable",
        serde_json::json!({"address_id": "address-sender"}),
        &runtime,
    )
    .unwrap();
    assert_eq!(enabled["collaboration_disabled"], false);
    let worktree_paused =
        execute_control_operation("worktree.pause", serde_json::json!({}), &runtime).unwrap();
    assert_eq!(worktree_paused["state"]["paused"], true);
    let worktree_resumed =
        execute_control_operation("worktree.resume", serde_json::json!({}), &runtime).unwrap();
    assert_eq!(worktree_resumed["state"]["paused"], false);
    let paused = execute_control_operation(
        "thread.pause",
        serde_json::json!({"thread_id": "thread-1"}),
        &runtime,
    )
    .unwrap();
    assert_eq!(paused["state"]["paused"], true);
    let resumed = execute_control_operation(
        "thread.resume",
        serde_json::json!({"thread_id": "thread-1"}),
        &runtime,
    )
    .unwrap();
    assert_eq!(resumed["state"]["paused"], false);
    let stopped = execute_control_operation(
        "thread.stop",
        serde_json::json!({"thread_id": "thread-1"}),
        &runtime,
    )
    .unwrap();

    assert_eq!(stopped["state"]["stopped"], true);
    assert_eq!(
        stopped["interrupted_pane_ids"],
        serde_json::json!(["pane-recipient"])
    );
    assert_eq!(stopped["warnings"], serde_json::json!([]));
    // The exact key is part of the contract, not the fake's leniency. Its
    // Herdr spelling is `ctrl+c`, which the adapter's tests pin, because the
    // real host answers `ctrl-c` with `invalid_key`.
    assert_eq!(herdr.attempts(), [interrupt_recipient()]);
}

#[test]
fn ambiguous_binding_recovery_opens_one_attention_item_until_an_explicit_rebind() {
    let directory = TestDirectory::new();
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let address_id = PaneAddressId::new("address-agent");
    let human = attribution(ActorKind::Human, "local-human", 1);
    let ids = SequenceIds(AtomicU64::new(1));
    let mut store = StateStore::open(directory.path()).unwrap();
    store
        .transaction(|transaction| {
            transaction.activate_worktree(&namespace, &human)?;
            transaction.create_pane_address(
                &namespace,
                &address("address-agent", "agent"),
                &human,
            )?;
            transaction.bind_pane(
                &namespace,
                &namedrop::PaneBindingId::new("binding-1"),
                &address_id,
                "pane-ambiguous",
                &human,
            )?;
            reconcile_discovered_panes(
                transaction,
                &namespace,
                &[
                    namedrop::DiscoveredPane {
                        pane_id: "pane-ambiguous".to_owned(),
                        terminal_id: "terminal-a".to_owned(),
                        tab_id: "tab-a".to_owned(),
                        workspace_id: "workspace-1".to_owned(),
                        cwd: None,
                        pane_label: None,
                        agent: Some(namedrop::mux::AgentIdentity {
                            kind: namedrop::mux::AgentKind::Unreported,
                            label: None,
                            session: namedrop::mux::SessionClaim::Reported(AgentSessionId::new(
                                "session-a",
                            )),
                        }),
                        agent_status: Some(namedrop::mux::AgentState::Idle),
                        idle_state_label: None,
                    },
                    namedrop::DiscoveredPane {
                        pane_id: "pane-ambiguous".to_owned(),
                        terminal_id: "terminal-b".to_owned(),
                        tab_id: "tab-b".to_owned(),
                        workspace_id: "workspace-1".to_owned(),
                        cwd: None,
                        pane_label: None,
                        agent: Some(namedrop::mux::AgentIdentity {
                            kind: namedrop::mux::AgentKind::Unreported,
                            label: None,
                            session: namedrop::mux::SessionClaim::Reported(AgentSessionId::new(
                                "session-b",
                            )),
                        }),
                        agent_status: Some(namedrop::mux::AgentState::Idle),
                        idle_state_label: None,
                    },
                ],
                &[],
                &ids,
                &human,
            )
        })
        .unwrap();
    assert_eq!(
        store
            .transaction(|transaction| transaction.unresolved_attention_items(&namespace))
            .unwrap()
            .len(),
        1
    );

    store
        .transaction(|transaction| {
            transaction.bind_pane(
                &namespace,
                &namedrop::PaneBindingId::new("binding-2"),
                &address_id,
                "pane-recovered",
                &human,
            )
        })
        .unwrap();
    assert_eq!(
        store
            .transaction(|transaction| transaction.unresolved_attention_items(&namespace))
            .unwrap()
            .len(),
        0
    );
}

#[test]
fn thread_stop_waits_out_a_live_peek_input_that_holds_the_pane_lock() {
    use namedrop::{DeliveryAttemptId, DeliveryState, LeaseDraft, PaneBindingId};
    use sha2::{Digest, Sha256};
    use std::{
        os::{
            fd::AsRawFd,
            unix::fs::{OpenOptionsExt, PermissionsExt},
        },
        sync::mpsc,
        thread,
        time::{Duration, Instant},
    };
    let directory = TestDirectory::new();
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let thread_id = ThreadId::new("thread-1");
    let sender = PaneAddressId::new("address-sender");
    let recipient = PaneAddressId::new("address-recipient");
    let human = attribution(ActorKind::Human, "local-human", 1);
    StateStore::open(directory.path())
        .unwrap()
        .transaction(|tx| {
            tx.activate_worktree(&namespace, &human)?;
            tx.create_pane_address(&namespace, &address("address-sender", "sender"), &human)?;
            tx.create_pane_address(
                &namespace,
                &address("address-recipient", "recipient"),
                &human,
            )?;
            tx.bind_pane(
                &namespace,
                &PaneBindingId::new("binding-recipient"),
                &recipient,
                "pane-recipient",
                &human,
            )?;
            tx.observe_agent_session(
                &namespace,
                "observation-recipient",
                &recipient,
                &AgentSessionId::new("session-recipient"),
                "working",
                &human,
            )?;
            tx.create_thread(&namespace, &thread_id, "Controls", &human)?;
            tx.append_message(
                &namespace,
                &message(1, &thread_id, &sender, ActorKind::Human, &recipient, None),
                &human,
            )?;
            tx.lease_mailbox_head(
                &namespace,
                &recipient,
                &LeaseDraft {
                    attempt_id: DeliveryAttemptId::new("attempt-1"),
                    binding_id: PaneBindingId::new("binding-recipient"),
                    agent_session_id: AgentSessionId::new("session-recipient"),
                    retry_of_attempt_id: None,
                    owner: "test".to_owned(),
                    acquired_at_ms: 2,
                    expires_at_ms: 102,
                },
                &human,
            )?;
            tx.transition_delivery_attempt(
                &namespace,
                &DeliveryAttemptId::new("attempt-1"),
                DeliveryState::Submitted,
                &human,
            )
        })
        .unwrap();
    // Live peek holds this lock for up to about 3.1 s per input, longer than
    // the 2 s a delivery waits.
    let hold = Duration::from_millis(2500);
    let locks = directory.path().join("pane-effects");
    fs::create_dir_all(&locks).unwrap();
    fs::set_permissions(&locks, fs::Permissions::from_mode(0o700)).unwrap();
    let key = namedrop::lower_hex(&Sha256::digest(
        serde_json::to_vec(&("socket-a", "pane-recipient")).unwrap(),
    ));
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(locks.join(format!("{key}.lock")))
        .unwrap();
    let (held, holding) = mpsc::channel();
    let input = thread::spawn(move || {
        // SAFETY: flock borrows the descriptor this thread owns.
        assert_eq!(
            unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
            0
        );
        held.send(()).unwrap();
        thread::sleep(hold);
    });
    holding.recv().unwrap();
    let started = Instant::now();
    let herdr = fake_herdr();
    let ids = SequenceIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-sender"),
        session_socket_id: "socket-a",
        mux: &*herdr,
        clock: &FixedClock,
        ids: &ids,
    };
    let stopped = execute_control_operation(
        "thread.stop",
        serde_json::json!({"thread_id": "thread-1"}),
        &runtime,
    )
    .unwrap();
    input.join().unwrap();
    assert_eq!(stopped["warnings"], serde_json::json!([]), "{stopped}");
    assert_eq!(
        stopped["interrupted_pane_ids"],
        serde_json::json!(["pane-recipient"])
    );
    assert!(
        started.elapsed() >= hold,
        "Stop interrupted while the input still held the pane"
    );
    assert_eq!(herdr.attempts(), [interrupt_recipient()]);
}
