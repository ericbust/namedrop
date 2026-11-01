use super::*;
use namedrop::{ActorKind, Clock, MutationAttribution, StateNamespace, StateStore, TransitionKind};
use std::{
    os::unix::fs::MetadataExt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::{Duration, Instant},
};

struct TestClock;
impl Clock for TestClock {
    fn now_unix_ms(&self) -> u64 {
        120
    }
    fn wait_ms(&self, _: u64) {}
}

fn projection_waits_on_prompt_lock(state: &Path) -> bool {
    let Ok(files) = std::fs::read_dir(state.join("hc-prompts")) else {
        return false;
    };
    let locks = std::fs::read_to_string("/proc/locks").unwrap();
    let inodes = files
        .filter_map(Result::ok)
        .map(|file| file.metadata().unwrap().ino())
        .chain(
            std::fs::metadata(state.join("hc-labels.lock"))
                .ok()
                .map(|file| file.ino()),
        );
    inodes.into_iter().any(|inode| {
        locks.lines().any(|line| {
            line.contains(" -> ")
                && line
                    .split_whitespace()
                    .any(|word| word.ends_with(&format!(":{inode}")))
        })
    })
}

#[test]
fn settle_projection_waits_for_each_prompt_callers_no_input_commit() {
    for caller in ["doctor", "onboarding", "delivery"] {
        for trigger in ["status", "snapshot"] {
            assert_projection_waits(caller, trigger);
        }
    }
}

fn assert_projection_waits(caller: &str, trigger: &str) {
    let fixture = Fixture::default();
    let agent = process::AgentProcess::new();
    fixture.agent(agent.pid());
    fixture.terminals.borrow_mut()[0]["status_ms"] = json!(100);
    let state = std::env::temp_dir().join(format!(
        "namedrop-hc-prompt-race-{}-{caller}-{trigger}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&state);
    let ids = namedrop::ProcessIds::default();
    let clock = TestClock;
    let runtime = namedrop::LifecycleRuntime {
        state_directory: &state,
        home_directory: Some(&state),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new(env!("CARGO_MANIFEST_DIR")),
        workspace_id: Some("fixture-space"),
        current_pane_id: Some("t1"),
        session_socket_id: "hc:fixture",
        host: HcHost::new(&fixture),
        clock: &clock,
        ids: &ids,
    };
    namedrop::execute_lifecycle_operation("name", json!({"alias":"alice"}), &runtime).unwrap();
    let namespace = StateNamespace::for_space("hc:fixture", "fixture-space");
    let mut store = StateStore::open(&state).unwrap();
    let at = MutationAttribution::new(ActorKind::Plugin, "test", 120);
    if caller == "doctor" {
        namedrop::execute_control_operation("worktree.pause", json!({}), &runtime).unwrap();
    } else if caller == "onboarding" {
        let db = rusqlite::Connection::open(state.join("namedrop.sqlite3")).unwrap();
        db.execute(
            "UPDATE pane_roles SET description='Manually configured collaborator'",
            [],
        )
        .unwrap();
        db.execute("UPDATE integration_registrations SET active=0", [])
            .unwrap();
    } else {
        let address = store
            .transaction(|tx| tx.active_pane_bindings(&namespace))
            .unwrap()
            .remove(0)
            .address_id;
        store
            .transaction(|tx| {
                tx.create_thread(&namespace, &namedrop::ThreadId::new("thread"), "test", &at)?;
                tx.append_message(
                    &namespace,
                    &namedrop::MessageDraft {
                        thread_id: namedrop::ThreadId::new("thread"),
                        message_id: namedrop::MessageId::new("message"),
                        in_reply_to: None,
                        author_address_id: address.clone(),
                        author_kind: ActorKind::Human,
                        author_alias_snapshot: "alice".into(),
                        author_agent_session_snapshot: None,
                        request_id: "request".into(),
                        authored_source: "@alice work".into(),
                        body: "work".into(),
                        grammar_version: "mention/v1".into(),
                        recipients: vec![namedrop::MessageRecipientDraft {
                            address_id: address,
                            alias_snapshot: "alice".into(),
                            agent_session_snapshot: Some(namedrop::AgentSessionId::new(
                                "fixture-session",
                            )),
                            delivery_id: namedrop::DeliveryId::new("delivery"),
                        }],
                        context_references: vec![],
                    },
                    &at,
                )?;
                Ok(())
            })
            .unwrap();
    }
    let (start, started) = mpsc::channel();
    let (projected, finished) = mpsc::channel();
    let projection_was_early = Arc::new(AtomicBool::new(false));
    let early = projection_was_early.clone();
    let terminal = fixture.terminals.borrow()[0].clone();
    let projection_state = state.clone();
    let trigger = trigger.to_owned();
    let subscriber = std::thread::spawn(move || {
        started.recv().unwrap();
        let fixture = Fixture::default();
        *fixture.terminals.borrow_mut() = vec![terminal];
        fixture.terminals.borrow_mut()[0]["status_ms"] = json!(110);
        let ids = namedrop::ProcessIds::default();
        let runtime = namedrop::LifecycleRuntime {
            state_directory: &projection_state,
            home_directory: Some(&projection_state),
            agent_config: namedrop::AgentConfigDirectories::default(),
            cwd: Path::new(env!("CARGO_MANIFEST_DIR")),
            workspace_id: Some("fixture-space"),
            current_pane_id: None,
            session_socket_id: "hc:fixture",
            host: HcHost::new(&fixture),
            clock: &TestClock,
            ids: &ids,
        };
        let event = if trigger == "status" {
            json!({"method":"event","params":{"type":"status","terminal":"t1"}})
        } else {
            json!({"terminals":*fixture.terminals.borrow()})
        };
        let result = namedrop::host::hc::events::Subscriber::default()
            .handle_line(&event.to_string(), &runtime);
        let _ = projected.send(());
        result
    });
    let prompt_state = state.clone();
    *fixture.on_send.borrow_mut() = Some(Box::new(move || {
        StateStore::open(&prompt_state)
            .unwrap()
            .transaction(|tx| {
                tx.deactivate_worktree(
                    &StateNamespace::for_space("hc:fixture", "fixture-space"),
                    &MutationAttribution::new(ActorKind::Plugin, "test", 120),
                )
            })
            .unwrap();
        start.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if finished.try_recv().is_ok() {
                early.store(true, Ordering::SeqCst);
                break;
            }
            if projection_waits_on_prompt_lock(&prompt_state) {
                break;
            }
            if Instant::now() > deadline {
                panic!("subscriber neither projected nor blocked on the prompt lock");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }));
    *fixture.send_error.borrow_mut() =
        Some(json!({"error":{"kind":"blocked","message":"not sent"}}).to_string());
    match caller {
        "doctor" => {
            assert!(
                namedrop::request_doctor_assistance(&runtime, "fixture-session")
                    .unwrap_err()
                    .starts_with("Not sent")
            );
        }
        "onboarding" => {
            namedrop::execute_lifecycle_operation("reconcile", json!({}), &runtime).unwrap();
        }
        "delivery" => {
            assert!(
                namedrop::drain_mailboxes(&runtime)
                    .unwrap()
                    .submitted
                    .is_empty()
            );
        }
        _ => unreachable!(),
    }
    subscriber.join().unwrap().unwrap();
    let facts = store
        .transaction(|tx| tx.transition_facts(&namespace))
        .unwrap();
    let gate = facts
        .iter()
        .rev()
        .find(|fact| {
            fact.entity_kind
                == if caller == "delivery" {
                    namedrop::TransitionEntityKind::MailboxDelivery
                } else {
                    namedrop::TransitionEntityKind::PromptRecipient
                }
        })
        .unwrap();
    assert_eq!(
        gate.transition_kind,
        TransitionKind::Resumed,
        "{caller} lost the only settle before the no-input commit"
    );
    assert!(
        !projection_was_early.load(Ordering::SeqCst),
        "settlement escaped the prompt boundary"
    );
    std::fs::remove_dir_all(state).unwrap();
}
