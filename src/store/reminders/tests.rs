use super::*;
use crate::{
    AgentRuntime, DeliveryAttemptId, HerdrCommandExecutor, HerdrCommandOutput, IdGenerator,
    PaneBindingId, ProcessIds,
};
use std::cell::Cell;

struct Fixture {
    state: PathBuf,
    live_session: Cell<&'static str>,
    now: Cell<u64>,
    busy: Cell<bool>,
    discoveries: Cell<u64>,
    prompts: Cell<u64>,
    discovery_error: Cell<Option<&'static str>>,
    prompt_failure: Cell<Option<crate::PromptFailureKind>>,
}
fn ns() -> StateNamespace {
    StateNamespace::for_space("test-socket", "test-space")
}
fn by(at: u64) -> MutationAttribution {
    MutationAttribution::new(ActorKind::Plugin, "test-reminder", at)
}
fn owner() -> ReminderOwner {
    ReminderOwner {
        address: PaneAddressId::new("owner"),
        session: AgentSessionId::new("session"),
        binding: PaneBindingId::new("binding"),
        pane: "pane".into(),
    }
}
impl Fixture {
    fn new() -> Self {
        let f = Self {
            state: std::env::temp_dir().join(ProcessIds::default().next_id("test-reminders")),
            live_session: Cell::new("session"),
            now: Cell::new(60_000),
            busy: Cell::new(false),
            discoveries: Cell::new(0),
            prompts: Cell::new(0),
            discovery_error: Cell::new(None),
            prompt_failure: Cell::new(None),
        };
        f.initialize(&ns());
        f
    }
    fn initialize(&self, namespace: &StateNamespace) {
        self.tx(|tx| {
            tx.activate_worktree(namespace, &by(0))?;
            for name in ["owner", "author"] {
                tx.create_pane_address(
                    namespace,
                    &crate::PaneAddressDraft {
                        id: PaneAddressId::new(name),
                        role_id: crate::PaneRoleId::new(name),
                        role_name: name.into(),
                        role_description: name.into(),
                        alias: name.into(),
                    },
                    &by(0),
                )?;
            }
            tx.bind_pane(
                namespace,
                &PaneBindingId::new("binding"),
                &owner().address,
                "pane",
                &by(0),
            )?;
            tx.observe_agent_session(
                namespace,
                "observation",
                &owner().address,
                &owner().session,
                "idle",
                &by(0),
            )?;
            tx.issue_integration_challenge(
                namespace,
                "registration",
                &owner().address,
                AgentRuntime::Codex,
                "nonce",
                &by(0),
            )?;
            tx.complete_integration_registration(
                namespace,
                &owner().address,
                &owner().session,
                AgentRuntime::Codex,
                "collab/v1",
                "nonce",
                crate::complete_capability_profile(),
                &by(0),
            )?;
            tx.complete_reconciliation(namespace)?;
            Ok(())
        });
    }
    fn tx<T>(&self, f: impl FnOnce(&mut StateTransaction<'_>) -> Result<T, StoreError>) -> T {
        StateStore::open(&self.state)
            .unwrap()
            .transaction(f)
            .unwrap()
    }
    fn spec(&self, reply: Option<MessageId>, repeat: crate::reminder::Repeat) -> ReminderSpec {
        ReminderSpec {
            body: "check the job".into(),
            first_due_at_ms: 60_000,
            in_reply_to: reply,
            repeat,
        }
    }
    fn set(&self, id: &str, reply: Option<MessageId>, repeat: crate::reminder::Repeat) {
        self.tx(|tx| tx.set_reminder(&ns(), id.into(), owner(), self.spec(reply, repeat), id, id));
    }
    fn live(&self) -> crate::DiscoveredWorktree {
        crate::discover_worktree_for_workspace(self, "test-space").unwrap()
    }
    fn fire(&self, id: &str) {
        let live = self.live();
        self.tx(|tx| tx.enqueue_reminder(&ns(), id, &live, 60_000));
    }
    fn lease(&self, id: &str, at: u64) -> crate::LeasedDelivery {
        self.tx(|tx| {
            tx.lease_mailbox_head(
                &ns(),
                &owner().address,
                &LeaseDraft {
                    attempt_id: DeliveryAttemptId::new(id),
                    binding_id: owner().binding,
                    agent_session_id: owner().session,
                    retry_of_attempt_id: None,
                    owner: "test".into(),
                    acquired_at_ms: at,
                    expires_at_ms: at + 10_000,
                },
                &by(at),
            )
        })
        .unwrap()
    }
    fn original(&self, grammar: &str) -> MessageId {
        self.tx(|tx| {
            let message = MessageId::new("question");
            tx.create_thread(&ns(), &ThreadId::new("original-thread"), "question", &by(1))?;
            tx.append_message(
                &ns(),
                &MessageDraft {
                    thread_id: ThreadId::new("original-thread"),
                    message_id: message.clone(),
                    in_reply_to: None,
                    author_address_id: PaneAddressId::new("author"),
                    author_kind: ActorKind::Human,
                    author_alias_snapshot: "author".into(),
                    author_agent_session_snapshot: None,
                    request_id: "question".into(),
                    authored_source: "@owner check later".into(),
                    body: "check later".into(),
                    grammar_version: grammar.into(),
                    recipients: vec![crate::MessageRecipientDraft {
                        address_id: owner().address,
                        alias_snapshot: "owner".into(),
                        agent_session_snapshot: Some(owner().session),
                        delivery_id: DeliveryId::new("original-delivery"),
                    }],
                    context_references: vec![],
                },
                &by(1),
            )?;
            Ok(message)
        })
    }
    fn operator_original(&self, private: bool) -> MessageId {
        self.tx(|tx| {
            let thread = if private {
                crate::store::direct_messages::thread_id(&owner().address)
            } else {
                ThreadId::new("original-thread")
            };
            tx.ensure_operator_author(&ns(), &by(1))?;
            tx.create_thread(&ns(), &thread, "remind me", &by(1))?;
            let id = MessageId::new("question");
            tx.append_message_mode(
                &ns(),
                &MessageDraft {
                    thread_id: thread,
                    message_id: id.clone(),
                    in_reply_to: None,
                    author_address_id: PaneAddressId::new("operator:local"),
                    author_kind: ActorKind::Human,
                    author_alias_snapshot: "operator".into(),
                    author_agent_session_snapshot: None,
                    request_id: "question".into(),
                    authored_source: "@owner remind me".into(),
                    body: "remind me".into(),
                    grammar_version: if private {
                        "direct-message/v1"
                    } else {
                        "channel/v1"
                    }
                    .into(),
                    recipients: vec![crate::MessageRecipientDraft {
                        address_id: owner().address,
                        alias_snapshot: "owner".into(),
                        agent_session_snapshot: Some(owner().session),
                        delivery_id: DeliveryId::new("original-delivery"),
                    }],
                    context_references: vec![],
                },
                &by(1),
                None,
                if private {
                    MessageAppendMode::DirectMessage
                } else {
                    MessageAppendMode::Normal
                },
            )?;
            Ok(id)
        })
    }
    fn settle_original(&self) {
        self.lease("original-attempt", 2);
        self.tx(|tx| {
            let attempt = DeliveryAttemptId::new("original-attempt");
            tx.transition_delivery_attempt(&ns(), &attempt, DeliveryState::Submitted, &by(3))?;
            tx.ensure_delivery_attempt_acknowledged(&ns(), &attempt, &owner().session, &by(3))?;
            tx.record_no_reply_disposition(
                &ns(),
                &attempt,
                "original-disposition",
                &owner().session,
                &by(4),
            )
        });
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.state).unwrap();
    }
}
impl HerdrCommandExecutor for Fixture {
    fn classify_prompt_failure(&self, _error: &str) -> crate::PromptFailureKind {
        self.prompt_failure
            .get()
            .unwrap_or(crate::PromptFailureKind::AmbiguousInvocation)
    }
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let value = match args {
            ["workspace", "list"] => {
                json!({"result":{"workspaces":[{"workspace_id":"test-space"}]}})
            }
            ["worktree", "list", ..] => {
                self.discoveries.set(self.discoveries.get() + 1);
                if let Some(code) = self.discovery_error.get() {
                    return Err(json!({"error":{"code":code,"message":"workspace_not_found text alone is not authoritative"}}).to_string());
                }
                json!({"result":{"worktrees":[{"path":"/test-repo","open_workspace_id":"test-space"}]}})
            }
            ["pane", "list", ..] => {
                json!({"result":{"panes":[{"pane_id":"pane","terminal_id":"terminal","tab_id":"tab","workspace_id":"test-space","agent_session":{"value":self.live_session.get()},"agent_status":if self.busy.get() { "working" } else { "idle" }}]}})
            }
            ["pane", "report-metadata", ..] | ["notification", "show", ..] => json!({}),
            ["agent", "prompt", "pane", _] => {
                self.prompts.set(self.prompts.get() + 1);
                if self.prompt_failure.get().is_some() {
                    return Err("fake prompt failure".into());
                }
                self.now.set(900_000);
                json!({})
            }
            _ => return Err(format!("unexpected fake Herdr command {args:?}")),
        };
        Ok(HerdrCommandOutput {
            stdout: value.to_string(),
        })
    }
}

#[test]
fn reminders_set_list_cancel_limits_and_replay() {
    let f = Fixture::new();
    for i in 0..20 {
        f.set(&format!("r{i}"), None, crate::reminder::Repeat::Once);
    }
    f.tx(|tx| {
        assert_eq!(tx.list_reminders(&ns(), &owner())?.len(), 20);
        assert!(
            tx.list_reminders(
                &ns(),
                &ReminderOwner {
                    session: AgentSessionId::new("other"),
                    ..owner()
                }
            )?
            .is_empty()
        );
        assert!(
            tx.set_reminder(
                &ns(),
                "overflow".into(),
                owner(),
                f.spec(None, crate::reminder::Repeat::Once),
                "overflow",
                "overflow"
            )
            .is_err()
        );
        assert!(
            tx.cancel_reminder(
                &ns(),
                &ReminderOwner {
                    session: AgentSessionId::new("other"),
                    ..owner()
                },
                "r0",
                0
            )
            .is_err()
        );
        tx.cancel_reminder(&ns(), &owner(), "r0", 0)?;
        let replay = tx
            .replay_reminder(&ns(), &owner().session, "r0", "r0")?
            .unwrap();
        assert_eq!(replay.spec.first_due_at_ms, 60_000);
        assert_eq!(replay.state, ReminderState::Cancelled);
        assert!(
            tx.replay_reminder(&ns(), &owner().session, "r0", "different")
                .is_err()
        );
        assert_eq!(tx.list_reminders(&ns(), &owner())?.len(), 19);
        Ok(())
    });
}

#[test]
fn reminder_fire_is_atomic_and_survives_restart_without_duplicate() {
    let f = Fixture::new();
    f.set("r", None, crate::reminder::Repeat::Once);
    let live = f.live();
    let failed: Result<(), StoreError> = StateStore::open(&f.state).unwrap().transaction(|tx| {
        tx.enqueue_reminder(&ns(), "r", &live, 60_000)?;
        Err(StoreError::invalid_input("simulated crash before commit"))
    });
    assert!(failed.is_err());
    f.tx(|tx| {
        assert!(matches!(
            tx.reminder(&ns(), "r")?.state,
            ReminderState::Scheduled { .. }
        ));
        assert_eq!(
            tx.transaction
                .query_row("SELECT count(*) FROM collaboration_messages", [], |r| r
                    .get::<_, i64>(0))?,
            0
        );
        Ok(())
    });
    f.fire("r");
    f.fire("r");
    f.tx(|tx| {
        assert_eq!(
            tx.transaction
                .query_row("SELECT count(*) FROM reminder_fires", [], |r| r
                    .get::<_, i64>(0))?,
            1
        );
        assert_eq!(
            tx.transaction
                .query_row("SELECT count(*) FROM mailbox_deliveries", [], |r| r
                    .get::<_, i64>(0))?,
            1
        );
        Ok(())
    });
    let lease = f.lease("wake", 60_001);
    f.tx(|tx| {
        let envelope = tx.delivery_envelope(
            &ns(),
            &lease.attempt_id,
            crate::IntegrationToolInvocation::new("/test/namedrop", Default::default()),
        )?;
        assert_eq!(
            envelope.control.reply_mode,
            crate::DeliveryReplyMode::ReportToHuman
        );
        assert!(
            !envelope
                .control
                .tool
                .operations
                .iter()
                .any(|o| o.token == "reply")
        );
        Ok(())
    });
}

#[test]
fn reminder_fresh_reply_preserves_original_thread_after_disposition() {
    for grammar in ["collab/v1", "mention-hook/v1"] {
        assert_fresh_reminder_reply(grammar, false);
    }
}

#[test]
fn reminder_nested_ordinary_reply_preserves_original_origin() {
    assert_fresh_reminder_reply("collab/v1", true);
}

#[test]
fn reminder_nested_typed_mention_preserves_original_origin() {
    assert_fresh_reminder_reply("mention-hook/v1", true);
}

fn assert_fresh_reminder_reply(grammar: &str, nested: bool) {
    let f = Fixture::new();
    let original = f.original(grammar);
    f.settle_original();
    f.set("r", Some(original.clone()), crate::reminder::Repeat::Once);
    f.fire("r");
    if nested {
        let first = f.lease("first-wake", 60_001);
        let message = f.tx(|tx| {
            let envelope = tx.delivery_envelope(
                &ns(),
                &first.attempt_id,
                crate::IntegrationToolInvocation::new("/test/namedrop", Default::default()),
            )?;
            let set = envelope
                .control
                .tool
                .operations
                .iter()
                .find(|op| op.token == "reminder.set")
                .unwrap();
            assert_eq!(set.input["in_reply_to"], json!(envelope.control.message_id));
            tx.transition_delivery_attempt(
                &ns(),
                &first.attempt_id,
                DeliveryState::Submitted,
                &by(60_002),
            )?;
            tx.ensure_delivery_attempt_acknowledged(
                &ns(),
                &first.attempt_id,
                &owner().session,
                &by(60_002),
            )?;
            tx.record_no_reply_disposition(
                &ns(),
                &first.attempt_id,
                "first-wake-disposition",
                &owner().session,
                &by(60_003),
            )?;
            Ok(serde_json::from_value(set.input["in_reply_to"].clone()).unwrap())
        });
        f.set("nested", Some(message), crate::reminder::Repeat::Once);
        f.fire("nested");
    }
    let wake = f.lease("fresh-attempt", 60_004);
    let reply = f.tx(|tx| {
        let envelope = tx.delivery_envelope(
            &ns(),
            &wake.attempt_id,
            crate::IntegrationToolInvocation::new("/test/namedrop", Default::default()),
        )?;
        assert_ne!(envelope.control.message_id, original);
        assert_eq!(envelope.control.thread_id, ThreadId::new("original-thread"));
        assert_eq!(
            envelope.control.reply_mode,
            crate::DeliveryReplyMode::for_message_grammar(grammar)
        );
        assert_eq!(
            tx.thread_control_state(&ns(), &envelope.control.thread_id)?
                .reply_allowance,
            10
        );
        let mut reply = envelope
            .control
            .tool
            .operations
            .iter()
            .find(|o| o.token == "reply")
            .unwrap()
            .input
            .clone();
        assert_eq!(reply["in_reply_to"], json!(original));
        assert_eq!(reply["attempt_id"], "fresh-attempt");
        assert_eq!(
            reply["source"],
            json!(format!("@author {}", crate::REPLY_BODY_PLACEHOLDER))
        );
        reply["source"] = json!("@author checked the job");
        tx.transition_delivery_attempt(
            &ns(),
            &wake.attempt_id,
            DeliveryState::Submitted,
            &by(60_005),
        )?;
        tx.ensure_delivery_attempt_acknowledged(
            &ns(),
            &wake.attempt_id,
            &owner().session,
            &by(60_005),
        )?;
        Ok(reply)
    });
    let runtime = crate::LifecycleRuntime {
        state_directory: &f.state,
        home_directory: None,
        agent_config: Default::default(),
        cwd: Path::new("/test-repo"),
        workspace_id: Some("test-space"),
        current_pane_id: Some("pane"),
        session_socket_id: "test-socket",
        herdr: &f,
        clock: &crate::SystemClock,
        ids: &ProcessIds::default(),
    };
    let answer = crate::execute_message_operation("reply", reply, &runtime).unwrap();
    assert_eq!(answer["thread_id"], "original-thread");
    f.tx(|tx|{
        assert_eq!(tx.thread_control_state(&ns(),&ThreadId::new("original-thread"))?.reply_allowance,9);
        let count:i64=tx.transaction.query_row("SELECT count(*) FROM delivery_dispositions WHERE attempt_id='original-attempt' AND disposition_id='original-disposition'",[],|r|r.get(0))?;
        assert_eq!(count,1);
        if nested {
            let count: i64 = tx.transaction.query_row("SELECT count(*) FROM delivery_dispositions WHERE attempt_id='first-wake' AND disposition_id='first-wake-disposition'", [], |r| r.get(0))?;
            assert_eq!(count, 1);
        }
        assert!(tx.delivery_disposition_exists(&ns(),&wake.attempt_id)?);Ok(())
    });
}

#[test]
fn reminder_repeats_advance_only_on_success_once_at_delivery_time() {
    for repeat in [
        crate::reminder::Repeat::Count {
            interval_ms: 60_000,
            total: 2,
        },
        crate::reminder::Repeat::Until {
            interval_ms: 60_000,
            until_ms: 560_000,
        },
    ] {
        let f = Fixture::new();
        f.set("r", None, repeat);
        f.fire("r");
        f.tx(|tx| {
            assert!(matches!(
                tx.reminder(&ns(), "r")?.state,
                ReminderState::AwaitingDelivery { .. }
            ));
            Ok(())
        });
        let fire = f.lease("first", 500_000);
        let aborted: Result<(), StoreError> =
            StateStore::open(&f.state).unwrap().transaction(|tx| {
                tx.transition_delivery_attempt(
                    &ns(),
                    &fire.attempt_id,
                    DeliveryState::Submitted,
                    &by(500_000),
                )?;
                Err(StoreError::invalid_input(
                    "simulated failure after successful-delivery callback",
                ))
            });
        assert!(aborted.is_err());
        f.tx(|tx| {
            assert!(matches!(
                tx.reminder(&ns(), "r")?.state,
                ReminderState::AwaitingDelivery { .. }
            ));
            tx.transition_delivery_attempt(
                &ns(),
                &fire.attempt_id,
                DeliveryState::Submitted,
                &by(500_000),
            )?;
            assert_eq!(
                tx.reminder(&ns(), "r")?.state,
                ReminderState::Scheduled {
                    due_at_ms: 560_000,
                    ordinal: 2
                }
            );
            tx.ensure_delivery_attempt_acknowledged(
                &ns(),
                &fire.attempt_id,
                &owner().session,
                &by(510_000),
            )?;
            assert_eq!(
                tx.reminder(&ns(), "r")?.state,
                ReminderState::Scheduled {
                    due_at_ms: 560_000,
                    ordinal: 2
                }
            );
            tx.record_no_reply_disposition(
                &ns(),
                &fire.attempt_id,
                "first-done",
                &owner().session,
                &by(520_000),
            )?;
            tx.enqueue_reminder(&ns(), "r", &f.live(), 560_000)
        });
        let second = f.lease("second", 560_001);
        f.tx(|tx| {
            tx.transition_delivery_attempt(
                &ns(),
                &second.attempt_id,
                DeliveryState::Submitted,
                &by(560_002),
            )?;
            assert_eq!(tx.reminder(&ns(), "r")?.state, ReminderState::Completed);
            Ok(())
        });
    }
}

#[test]
fn reminder_cancel_and_ambiguous_transport_never_create_successors() {
    for uncertain in [false, true] {
        let f = Fixture::new();
        f.set(
            "r",
            None,
            crate::reminder::Repeat::Count {
                interval_ms: 60_000,
                total: 2,
            },
        );
        f.fire("r");
        if uncertain {
            let fire = f.lease("ambiguous", 60_001);
            f.tx(|tx| {
                tx.transition_delivery_attempt(
                    &ns(),
                    &fire.attempt_id,
                    DeliveryState::Held,
                    &by(60_002),
                )
            });
        }
        f.tx(|tx| {
            let result = tx.cancel_reminder(&ns(), &owner(), "r", 60_003)?;
            assert_eq!(result["transport_may_have_started"], uncertain);
            tx.maintain_reminder_owners(&ns(), &f.live())?;
            assert_eq!(tx.reminder(&ns(), "r")?.state, ReminderState::Cancelled);
            let state: String =
                tx.transaction
                    .query_row("SELECT state FROM mailbox_deliveries", [], |r| r.get(0))?;
            assert_eq!(state, if uncertain { "held" } else { "cancelled" });
            Ok(())
        });
    }
}

#[test]
fn reminder_owner_fences_due_dispatch_and_lease() {
    let f = Fixture::new();
    f.set("r", None, crate::reminder::Repeat::Once);
    f.live_session.set("successor");
    f.fire("r");
    f.tx(|tx| {
        assert!(matches!(
            tx.reminder(&ns(), "r")?.state,
            ReminderState::Failed { .. }
        ));
        assert_eq!(
            tx.transaction
                .query_row("SELECT count(*) FROM mailbox_deliveries", [], |r| r
                    .get::<_, i64>(0))?,
            0
        );
        Ok(())
    });
    for at_lease in [false, true] {
        let f = Fixture::new();
        f.set("r", None, crate::reminder::Repeat::Once);
        f.fire("r");
        f.tx(|tx| {
            assert!(tx.dispatch_candidate(&ns(), &owner().address)?.is_some());
            tx.observe_agent_session(
                &ns(),
                "replace",
                &owner().address,
                &AgentSessionId::new("successor"),
                "idle",
                &by(60_001),
            )?;
            tx.issue_integration_challenge(
                &ns(),
                "replacement",
                &owner().address,
                AgentRuntime::Codex,
                "replacement-nonce",
                &by(60_001),
            )?;
            tx.complete_integration_registration(
                &ns(),
                &owner().address,
                &AgentSessionId::new("successor"),
                AgentRuntime::Codex,
                "collab/v1",
                "replacement-nonce",
                crate::complete_capability_profile(),
                &by(60_001),
            )?;
            if at_lease {
                let leased = tx.lease_mailbox_head(
                    &ns(),
                    &owner().address,
                    &LeaseDraft {
                        attempt_id: DeliveryAttemptId::new("successor-attempt"),
                        binding_id: owner().binding,
                        agent_session_id: AgentSessionId::new("successor"),
                        retry_of_attempt_id: None,
                        owner: "test".into(),
                        acquired_at_ms: 60_002,
                        expires_at_ms: 70_000,
                    },
                    &by(60_002),
                )?;
                assert!(leased.is_none());
            } else {
                assert!(tx.dispatch_candidate(&ns(), &owner().address)?.is_none());
            }
            assert!(matches!(
                tx.reminder(&ns(), "r")?.state,
                ReminderState::Failed { .. }
            ));
            assert_eq!(
                tx.transaction
                    .query_row("SELECT state FROM mailbox_deliveries", [], |r| r
                        .get::<_, String>(0))?,
                "failed"
            );
            Ok(())
        });
    }
}

#[test]
fn reminder_paused_thread_keeps_ordinary_allowance_controls() {
    let f = Fixture::new();
    let original = f.original("collab/v1");
    f.settle_original();
    f.set("r", Some(original), crate::reminder::Repeat::Once);
    f.tx(|tx| {
        tx.transaction
            .execute("UPDATE thread_controls SET paused=1,reply_allowance=0", [])?;
        Ok(())
    });
    f.fire("r");
    f.tx(|tx| {
        assert!(tx.dispatch_candidate(&ns(), &owner().address)?.is_none());
        assert_eq!(
            tx.thread_control_state(&ns(), &ThreadId::new("original-thread"))?
                .reply_allowance,
            0
        );
        assert!(matches!(
            tx.reminder(&ns(), "r")?.state,
            ReminderState::AwaitingDelivery { .. }
        ));
        Ok(())
    });
}

#[test]
fn real_schema_six_fixture_migrates_live_without_relabeling() {
    let state = std::env::temp_dir().join(ProcessIds::default().next_id("test-reminder-migration"));
    fs::create_dir_all(&state).unwrap();
    let connection = Connection::open(state.join("namedrop.sqlite3")).unwrap();
    let lines: Vec<_> = include_str!("../schema-v6.txt").lines().collect();
    for line in lines
        .iter()
        .filter(|l| l.starts_with("table "))
        .chain(lines.iter().filter(|l| !l.starts_with("table ")))
    {
        let (_, sql) = line.split_once(": ").unwrap();
        connection.execute_batch(&format!("{sql};")).unwrap();
    }
    connection.execute_batch("PRAGMA user_version=6; INSERT INTO worktrees VALUES('test-socket','test-space',1,0,0,1);").unwrap();
    assert!(connection.prepare("SELECT * FROM reminders").is_err());
    drop(connection);
    let mut store = StateStore::open(&state).unwrap();
    assert_eq!(store.schema_version().unwrap(), 7);
    store
        .transaction(|tx| {
            assert!(tx.worktree_state(&ns())?.unwrap().activated);
            assert!(tx.list_reminders(&ns(), &owner())?.is_empty());
            Ok(())
        })
        .unwrap();
    drop(store);
    fs::remove_dir_all(state).unwrap();
}

#[test]
fn reminder_reply_keeps_historical_alias_refusal() {
    let f = Fixture::new();
    let original = f.original("collab/v1");
    f.settle_original();
    f.set("r", Some(original), crate::reminder::Repeat::Once);
    f.fire("r");
    let wake = f.lease("fresh", 60_001);
    let reply = f.tx(|tx| {
        tx.replace_alias(&ns(), &PaneAddressId::new("author"), "renamed", &by(60_002))?;
        let envelope = tx.delivery_envelope(
            &ns(),
            &wake.attempt_id,
            crate::IntegrationToolInvocation::new("/test/namedrop", Default::default()),
        )?;
        let mut reply = envelope
            .control
            .tool
            .operations
            .iter()
            .find(|op| op.token == "reply")
            .unwrap()
            .input
            .clone();
        assert_eq!(
            reply["source"],
            json!(format!("@author {}", crate::REPLY_BODY_PLACEHOLDER))
        );
        reply["source"] = json!("@author finished");
        tx.transition_delivery_attempt(
            &ns(),
            &wake.attempt_id,
            DeliveryState::Submitted,
            &by(60_002),
        )?;
        tx.ensure_delivery_attempt_acknowledged(
            &ns(),
            &wake.attempt_id,
            &owner().session,
            &by(60_002),
        )?;
        Ok(reply)
    });
    let runtime = crate::LifecycleRuntime {
        state_directory: &f.state,
        home_directory: None,
        agent_config: Default::default(),
        cwd: Path::new("/test-repo"),
        workspace_id: Some("test-space"),
        current_pane_id: Some("pane"),
        session_socket_id: "test-socket",
        herdr: &f,
        clock: &crate::SystemClock,
        ids: &ProcessIds::default(),
    };
    assert!(crate::execute_message_operation("reply", reply, &runtime).is_err());
    f.tx(|tx| {
        assert!(!tx.delivery_disposition_exists(&ns(), &wake.attempt_id)?);
        assert_eq!(
            tx.thread_control_state(&ns(), &ThreadId::new("original-thread"))?
                .reply_allowance,
            10
        );
        Ok(())
    });
}

#[test]
fn reminder_failure_and_raw_settled_state_do_not_advance_repeats() {
    for mailbox_state in ["failed", "settled"] {
        let f = Fixture::new();
        f.set(
            "r",
            None,
            crate::reminder::Repeat::Count {
                interval_ms: 60_000,
                total: 2,
            },
        );
        f.fire("r");
        f.tx(|tx| {
            tx.transaction
                .execute("UPDATE mailbox_deliveries SET state=?1", [mailbox_state])?;
            tx.maintain_reminder_owners(&ns(), &f.live())?;
            let state = tx.reminder(&ns(), "r")?.state;
            assert!(!matches!(
                state,
                ReminderState::Scheduled { .. } | ReminderState::Completed
            ));
            assert_eq!(
                tx.transaction
                    .query_row("SELECT count(*) FROM reminder_fires", [], |r| r
                        .get::<_, i64>(0))?,
                1
            );
            Ok(())
        });
    }
}

impl crate::Clock for Fixture {
    fn now_unix_ms(&self) -> u64 {
        self.now.get()
    }
    fn wait_ms(&self, _duration_ms: u64) {}
}

#[test]
fn reminder_fixed_delay_starts_after_a_slow_successful_prompt() {
    let f = Fixture::new();
    f.set(
        "r",
        None,
        crate::reminder::Repeat::Count {
            interval_ms: 60_000,
            total: 2,
        },
    );
    f.fire("r");
    let runtime = crate::LifecycleRuntime {
        state_directory: &f.state,
        home_directory: None,
        agent_config: Default::default(),
        cwd: Path::new("/test-repo"),
        workspace_id: Some("test-space"),
        current_pane_id: None,
        session_socket_id: "test-socket",
        herdr: &f,
        clock: &f,
        ids: &ProcessIds::default(),
    };
    let drain = crate::drain_mailboxes(&runtime).unwrap();
    assert_eq!(drain.submitted.len(), 1);
    f.tx(|tx| {
        assert_eq!(
            tx.reminder(&ns(), "r")?.state,
            ReminderState::Scheduled {
                due_at_ms: 960_000,
                ordinal: 2
            }
        );
        Ok(())
    });
}

#[test]
fn reminder_nested_pane_wake_stays_report_to_human() {
    let f = Fixture::new();
    f.set("first", None, crate::reminder::Repeat::Once);
    f.fire("first");
    let first = f.lease("first-wake", 60_001);
    let target = f.tx(|tx| {
        let envelope = tx.delivery_envelope(
            &ns(),
            &first.attempt_id,
            crate::IntegrationToolInvocation::new("/test/namedrop", Default::default()),
        )?;
        let set = envelope
            .control
            .tool
            .operations
            .iter()
            .find(|op| op.token == "reminder.set")
            .expect("pane reminder wake must offer reminder.set");
        assert_eq!(set.input["in_reply_to"], json!(envelope.control.message_id));
        Ok(serde_json::from_value(set.input["in_reply_to"].clone()).unwrap())
    });
    f.set("nested", Some(target), crate::reminder::Repeat::Once);
    f.tx(|tx| {
        tx.transition_delivery_attempt(
            &ns(),
            &first.attempt_id,
            DeliveryState::Submitted,
            &by(60_002),
        )?;
        tx.ensure_delivery_attempt_acknowledged(
            &ns(),
            &first.attempt_id,
            &owner().session,
            &by(60_002),
        )?;
        tx.record_no_reply_disposition(
            &ns(),
            &first.attempt_id,
            "pane-done",
            &owner().session,
            &by(60_003),
        )
    });
    f.fire("nested");
    let nested = f.lease("nested-wake", 60_004);
    f.tx(|tx| {
        let envelope = tx.delivery_envelope(
            &ns(),
            &nested.attempt_id,
            crate::IntegrationToolInvocation::new("/test/namedrop", Default::default()),
        )?;
        assert_eq!(
            envelope.control.reply_mode,
            crate::DeliveryReplyMode::ReportToHuman
        );
        assert!(
            envelope
                .control
                .tool
                .operations
                .iter()
                .any(|op| op.token == "complete.no-reply")
        );
        assert!(
            !envelope
                .control
                .tool
                .operations
                .iter()
                .any(|op| op.token == "reply")
        );
        assert!(tx.delivery_disposition_exists(&ns(), &first.attempt_id)?);
        Ok(())
    });
}

#[test]
fn reminder_nested_origin_authenticates_received_wake_before_normalizing() {
    let f = Fixture::new();
    let original = f.original("collab/v1");
    f.settle_original();
    f.set("first", Some(original), crate::reminder::Repeat::Once);
    f.fire("first");
    let message = MessageId::new("reminder-message:first:60000");
    f.tx(|tx| {
        let error = tx
            .set_reminder(
                &ns(),
                "unreceived".into(),
                owner(),
                f.spec(Some(message.clone()), crate::reminder::Repeat::Once),
                "unreceived",
                "unreceived",
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("delivered to this exact Agent Session"),
            "{error}"
        );
        Ok(())
    });
    f.lease("first-wake", 60_001);
    f.tx(|tx| {
        let error = tx
            .set_reminder(
                &ns(),
                "other-session".into(),
                ReminderOwner {
                    session: AgentSessionId::new("other"),
                    ..owner()
                },
                f.spec(Some(message), crate::reminder::Repeat::Once),
                "other-session",
                "other-session",
            )
            .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("delivered to this exact Agent Session"),
            "{error}"
        );
        Ok(())
    });
}

#[test]
fn reminder_refuses_unsupported_system_only_origin() {
    let f = Fixture::new();
    let original = f.original("coordination-report/v1");
    f.lease("system-origin", 2);
    f.tx(|tx| {
        let result = tx.set_reminder(
            &ns(),
            "unsupported".into(),
            owner(),
            f.spec(Some(original), crate::reminder::Repeat::Once),
            "unsupported",
            "unsupported",
        );
        let error = result.expect_err("system-only origin must not grant a bound reply");
        assert!(error.to_string().contains("system"), "{error}");
        Ok(())
    });
}

struct MeasurementHerdr<'a>(&'a Fixture);
impl HerdrCommandExecutor for MeasurementHerdr<'_> {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        self.0.execute(args)
    }
}

#[test]
fn reminder_busy_clock_work_measurement() {
    let f = Fixture::new();
    f.busy.set(true);
    f.tx(|tx| {
        tx.observe_agent_session(
            &ns(),
            "busy",
            &owner().address,
            &owner().session,
            "working",
            &by(1),
        )
    });
    f.set(
        "r",
        None,
        crate::reminder::Repeat::Count {
            interval_ms: 60_000,
            total: 2,
        },
    );
    f.fire("r");
    f.discoveries.set(0);
    let ids = ProcessIds::default();
    let mut catch_up = crate::reminder::CatchUp::default();
    let startup_scope = crate::work_counters::Scope::start();
    let startup_result =
        crate::reminder::cycle_with(&f.state, &f, &ids, |_| MeasurementHerdr(&f), &mut catch_up);
    let startup = startup_scope.finish();
    startup_result.unwrap();
    let startup_discoveries = f.discoveries.replace(0);
    let scope = crate::work_counters::Scope::start();
    let mut errors = 0;
    for second in 1..=300 {
        f.now.set(60_000 + second * 1000);
        if crate::reminder::cycle_with(&f.state, &f, &ids, |_| MeasurementHerdr(&f), &mut catch_up)
            .is_err()
        {
            errors += 1;
        }
    }
    let recurring = scope.finish();
    let discoveries = f.discoveries.get();
    assert_eq!(errors, 0);
    assert_eq!(startup.drain_errors, 0);
    assert_eq!(recurring.drain_errors, 0);
    assert_eq!(recurring.writes, 0);
    assert_eq!(recurring.drains, 0);
    assert_eq!(discoveries, 0);
    assert_eq!(f.now.get(), 360_000);
    assert!(f.busy.get());
    assert_eq!(f.prompts.get(), 0);
    let connection = Connection::open(f.state.join("namedrop.sqlite3")).unwrap();
    for table in [
        "reminder_fires",
        "mailbox_deliveries",
        "collaboration_messages",
    ] {
        assert_eq!(
            connection
                .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    assert_eq!(
        connection
            .query_row("SELECT count(*) FROM delivery_attempts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        connection
            .query_row("SELECT state FROM mailbox_deliveries", [], |r| r
                .get::<_, String>(0))
            .unwrap(),
        "queued"
    );
    f.tx(|tx| {
        assert!(matches!(
            tx.reminder(&ns(), "r")?.state,
            ReminderState::AwaitingDelivery { .. }
        ));
        Ok(())
    });
    println!(
        "IDLE_MEASUREMENT {}",
        json!({"duration_simulated_seconds":300,"startup":startup,"startup_discoveries":startup_discoveries,"recurring":recurring,"recurring_discoveries":discoveries,"errors":errors,"fires":1,"mailboxes":1,"attempts_while_busy":0,"state":"awaiting_delivery"})
    );
    f.busy.set(false);
    let home = f.state.join("home");
    fs::create_dir_all(&home).unwrap();
    let runtime = crate::LifecycleRuntime {
        state_directory: &f.state,
        home_directory: Some(&home),
        agent_config: Default::default(),
        cwd: Path::new("/test-repo"),
        workspace_id: Some("test-space"),
        current_pane_id: Some("pane"),
        session_socket_id: "test-socket",
        herdr: &f,
        clock: &f,
        ids: &ids,
    };
    crate::execute_lifecycle_operation("event.agent-status-changed", json!({"type":"pane_agent_status_changed","pane_id":"pane","workspace_id":"test-space","agent_status":"idle"}), &runtime).unwrap();
    assert_eq!(f.prompts.get(), 1);
    f.tx(|tx| {
        assert_eq!(
            tx.reminder(&ns(), "r")?.state,
            ReminderState::Scheduled {
                due_at_ms: 960_000,
                ordinal: 2
            }
        );
        Ok(())
    });
}

#[test]
fn reminder_idle_cycle_is_read_only_under_an_external_writer() {
    let f = Fixture::new();
    f.busy.set(true);
    f.set("r", None, crate::reminder::Repeat::Once);
    f.fire("r");
    let connection = Connection::open(f.state.join("namedrop.sqlite3")).unwrap();
    connection.execute_batch("BEGIN IMMEDIATE").unwrap();
    f.discoveries.set(0);
    let scope = crate::work_counters::Scope::start();
    let result = crate::reminder::cycle_with(
        &f.state,
        &f,
        &ProcessIds::default(),
        |_| MeasurementHerdr(&f),
        &mut crate::reminder::CatchUp::completed(),
    );
    let counts = scope.finish();
    connection.execute_batch("ROLLBACK").unwrap();
    assert!(result.unwrap());
    assert_eq!(counts.writes, 0);
    assert_eq!(counts.drains, 0);
    assert_eq!(f.discoveries.get(), 0);
}

#[test]
fn reminder_future_only_startup_has_no_discovery_write_or_drain() {
    let f = Fixture::new();
    f.set("future", None, crate::reminder::Repeat::Once);
    f.now.set(0);
    let scope = crate::work_counters::Scope::start();
    let result = crate::reminder::cycle_with(
        &f.state,
        &f,
        &ProcessIds::default(),
        |_| MeasurementHerdr(&f),
        &mut crate::reminder::CatchUp::default(),
    );
    let counts = scope.finish();
    assert!(result.unwrap());
    assert_eq!(counts.writes, 0);
    assert_eq!(counts.drains, 0);
    assert_eq!(f.discoveries.get(), 0);
}

#[test]
fn reminder_due_cycle_ignores_future_namespaces_and_deduplicates_catchup() {
    let f = Fixture::new();
    f.busy.set(true);
    f.set("future", None, crate::reminder::Repeat::Once);
    f.set("queued", None, crate::reminder::Repeat::Once);
    f.set("due", None, crate::reminder::Repeat::Once);
    f.fire("queued");
    f.tx(|tx| {
        tx.activate_worktree(&StateNamespace::for_space("test-socket", "future-space"), &by(0))?;
        tx.transaction.execute("UPDATE reminders SET worktree_id='future-space',due_at_ms=900000,state_json=json_set(state_json,'$.due_at_ms',900000) WHERE reminder_id='future'", [])?;
        Ok(())
    });
    f.discoveries.set(0);
    let scope = crate::work_counters::Scope::start();
    let mut visited = Vec::new();
    let result = crate::reminder::cycle_with(
        &f.state,
        &f,
        &ProcessIds::default(),
        |namespace| {
            visited.push(namespace.clone());
            MeasurementHerdr(&f)
        },
        &mut crate::reminder::CatchUp::default(),
    );
    let counts = scope.finish();
    assert!(result.unwrap());
    assert_eq!(counts.drains, 1);
    assert_eq!(visited, vec![ns(), ns()]);
    assert_eq!(f.discoveries.get(), 2);
    f.tx(|tx| {
        assert!(matches!(
            tx.reminder(&ns(), "due")?.state,
            ReminderState::AwaitingDelivery { .. }
        ));
        Ok(())
    });
}

#[test]
fn reminder_cancelled_after_plan_does_not_drain() {
    let f = Fixture::new();
    f.set("due", None, crate::reminder::Repeat::Once);
    let scope = crate::work_counters::Scope::start();
    let result = crate::reminder::cycle_with(
        &f.state,
        &f,
        &ProcessIds::default(),
        |_| {
            f.tx(|tx| tx.cancel_reminder(&ns(), &owner(), "due", 60_000));
            MeasurementHerdr(&f)
        },
        &mut crate::reminder::CatchUp::completed(),
    );
    let counts = scope.finish();
    assert!(result.unwrap());
    assert_eq!(counts.drains, 0);
    assert_eq!(f.prompts.get(), 0);
    f.tx(|tx| {
        assert_eq!(tx.reminder(&ns(), "due")?.state, ReminderState::Cancelled);
        Ok(())
    });
}

#[test]
fn reminder_terminal_mailbox_cleanup_remains_hook_visible() {
    let f = Fixture::new();
    f.set("queued", None, crate::reminder::Repeat::Once);
    f.fire("queued");
    f.tx(|tx| {
        let ReminderState::AwaitingDelivery { delivery_id, .. } =
            tx.reminder(&ns(), "queued")?.state
        else {
            panic!("queued reminder must await delivery");
        };
        assert!(tx.cancel_unsent_delivery(&ns(), &delivery_id, &by(60_001))?);
        Ok(())
    });
    assert_eq!(
        crate::spaces_to_wake(&f.state, "test-socket", "elsewhere"),
        vec!["elsewhere", "test-space"]
    );
    assert_eq!(
        crate::spaces_to_wake(&f.state, "other-socket", "elsewhere"),
        vec!["elsewhere"]
    );
    let runtime = crate::LifecycleRuntime {
        state_directory: &f.state,
        home_directory: None,
        agent_config: Default::default(),
        cwd: Path::new("/test-repo"),
        workspace_id: Some("test-space"),
        current_pane_id: None,
        session_socket_id: "test-socket",
        herdr: &f,
        clock: &f,
        ids: &ProcessIds::default(),
    };
    crate::drain_mailboxes(&runtime).unwrap();
    f.tx(|tx| {
        assert_eq!(
            tx.reminder(&ns(), "queued")?.state,
            ReminderState::Failed {
                reason: "reminder delivery cancelled".into()
            }
        );
        Ok(())
    });
    assert_eq!(f.prompts.get(), 0);
}

#[test]
fn reminder_startup_catchup_keeps_transient_failure_eligible() {
    let f = Fixture::new();
    f.busy.set(true);
    f.set("r", None, crate::reminder::Repeat::Once);
    f.fire("r");
    f.discovery_error.set(Some("transport_unavailable"));
    let mut catch_up = crate::reminder::CatchUp::default();
    let caught_up = crate::reminder::cycle_with(
        &f.state,
        &f,
        &ProcessIds::default(),
        |_| MeasurementHerdr(&f),
        &mut catch_up,
    )
    .unwrap();
    assert!(!caught_up);
    f.tx(|tx| {
        assert!(matches!(
            tx.reminder(&ns(), "r")?.state,
            ReminderState::AwaitingDelivery { .. }
        ));
        Ok(())
    });
    f.discovery_error.set(None);
    let scope = crate::work_counters::Scope::start();
    let result = crate::reminder::cycle_with(
        &f.state,
        &f,
        &ProcessIds::default(),
        |_| MeasurementHerdr(&f),
        &mut catch_up,
    );
    let counts = scope.finish();
    assert!(result.unwrap());
    assert_eq!(counts.drains, 1);
    assert_eq!(counts.drain_errors, 0);
    assert_eq!(f.prompts.get(), 0);
}

#[test]
fn reminder_enqueue_reports_only_a_new_durable_fire() {
    let f = Fixture::new();
    f.set("r", None, crate::reminder::Repeat::Once);
    let live = f.live();
    f.tx(|tx| {
        assert!(!tx.enqueue_reminder(&ns(), "r", &live, 59_999)?);
        assert!(tx.enqueue_reminder(&ns(), "r", &live, 60_000)?);
        assert!(!tx.enqueue_reminder(&ns(), "r", &live, 60_000)?);
        Ok(())
    });
}

#[test]
fn reminder_dispatch_failure_finalizes_in_the_same_drain_but_ambiguity_waits() {
    for kind in [
        crate::PromptFailureKind::DefinitiveNonRetryable,
        crate::PromptFailureKind::AmbiguousInvocation,
    ] {
        let f = Fixture::new();
        f.set(
            "r",
            None,
            crate::reminder::Repeat::Count {
                interval_ms: 60_000,
                total: 2,
            },
        );
        f.fire("r");
        f.prompt_failure.set(Some(kind));
        let runtime = crate::LifecycleRuntime {
            state_directory: &f.state,
            home_directory: None,
            agent_config: Default::default(),
            cwd: Path::new("/test-repo"),
            workspace_id: Some("test-space"),
            current_pane_id: None,
            session_socket_id: "test-socket",
            herdr: &f,
            clock: &f,
            ids: &ProcessIds::default(),
        };
        let report = crate::drain_mailboxes(&runtime).unwrap();
        assert_eq!(f.prompts.get(), 1);
        f.tx(|tx| {
            if kind == crate::PromptFailureKind::DefinitiveNonRetryable {
                assert_eq!(report.failed.len(), 1);
                assert_eq!(
                    tx.reminder(&ns(), "r")?.state,
                    ReminderState::Failed {
                        reason: "reminder delivery failed".into()
                    }
                );
            } else {
                assert_eq!(report.held.len(), 1);
                assert!(matches!(
                    tx.reminder(&ns(), "r")?.state,
                    ReminderState::AwaitingDelivery { .. }
                ));
            }
            Ok(())
        });
    }
}

struct ScopedHerdr<'a> {
    fixture: &'a Fixture,
    namespace: StateNamespace,
    error: Option<&'static str>,
}
impl HerdrCommandExecutor for ScopedHerdr<'_> {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        if args.starts_with(&["worktree", "list"])
            && let Some(code) = self.error
        {
            return Err(json!({"error":{"code":code}}).to_string());
        }
        self.fixture.execute(args).map(|output| HerdrCommandOutput {
            stdout: output
                .stdout
                .replace("test-space", self.namespace.space_id.as_str()),
        })
    }
}

#[test]
fn reminder_startup_retry_does_not_redrain_successful_busy_namespaces() {
    for error in ["transport_unavailable", "workspace_not_found"] {
        let f = Fixture::new();
        f.busy.set(true);
        f.set("busy", None, crate::reminder::Repeat::Once);
        f.fire("busy");
        let unavailable = StateNamespace::for_space("test-socket", "unavailable");
        f.initialize(&unavailable);
        let herdr = ScopedHerdr {
            fixture: &f,
            namespace: unavailable.clone(),
            error: None,
        };
        let live = crate::discover_worktree_for_workspace(&herdr, "unavailable").unwrap();
        f.tx(|tx| {
            tx.set_reminder(
                &unavailable,
                "retry".into(),
                owner(),
                f.spec(None, crate::reminder::Repeat::Once),
                "retry",
                "retry",
            )?;
            tx.enqueue_reminder(&unavailable, "retry", &live, 60_000)
        });
        let mut catch_up = crate::reminder::CatchUp::default();
        let mut visited = Vec::new();
        let first = crate::reminder::cycle_with(
            &f.state,
            &f,
            &ProcessIds::default(),
            |namespace| {
                visited.push(namespace.clone());
                ScopedHerdr {
                    fixture: &f,
                    namespace: namespace.clone(),
                    error: (namespace == &unavailable).then_some(error),
                }
            },
            &mut catch_up,
        )
        .unwrap();
        assert_eq!(first, error == "workspace_not_found");
        assert_eq!(visited.len(), 2);
        assert!(visited.contains(&ns()));
        assert!(visited.contains(&unavailable));
        visited.clear();
        let scope = crate::work_counters::Scope::start();
        let second = crate::reminder::cycle_with(
            &f.state,
            &f,
            &ProcessIds::default(),
            |namespace| {
                visited.push(namespace.clone());
                ScopedHerdr {
                    fixture: &f,
                    namespace: namespace.clone(),
                    error: (namespace == &unavailable).then_some(error),
                }
            },
            &mut catch_up,
        );
        let counts = scope.finish();
        assert_eq!(second.unwrap(), error == "workspace_not_found");
        assert_eq!(counts.writes, 0);
        if error == "workspace_not_found" {
            assert!(visited.is_empty());
            assert_eq!(counts.drains, 0);
            f.tx(|tx| {
                assert_eq!(
                    tx.reminder(&unavailable, "retry")?.state,
                    ReminderState::Failed {
                        reason: "owner workspace is gone".into()
                    }
                );
                Ok(())
            });
        } else {
            assert_eq!(visited, vec![unavailable.clone()]);
            assert_eq!(counts.drains, 1);
            assert_eq!(counts.drain_errors, 1);
            visited.clear();
            let recovered = crate::reminder::cycle_with(
                &f.state,
                &f,
                &ProcessIds::default(),
                |namespace| {
                    visited.push(namespace.clone());
                    ScopedHerdr {
                        fixture: &f,
                        namespace: namespace.clone(),
                        error: None,
                    }
                },
                &mut catch_up,
            )
            .unwrap();
            assert!(recovered);
            assert_eq!(visited, vec![unavailable]);
        }
        assert_eq!(f.prompts.get(), 0);
        f.tx(|tx| {
            assert!(matches!(
                tx.reminder(&ns(), "busy")?.state,
                ReminderState::AwaitingDelivery { .. }
            ));
            Ok(())
        });
    }
}

#[test]
fn reminder_operator_wakes_keep_dm_and_channel_reply_routes() {
    for private in [false, true] {
        let f = Fixture::new();
        let original = f.operator_original(private);
        f.settle_original();
        f.set("r", Some(original.clone()), crate::reminder::Repeat::Once);
        f.fire("r");
        let wake = f.lease("wake", 60_001);
        let input = f.tx(|tx| {
            let envelope = tx.delivery_envelope(
                &ns(),
                &wake.attempt_id,
                crate::IntegrationToolInvocation::new("/test/namedrop", Default::default()),
            )?;
            assert_eq!(
                envelope.control.reply_mode,
                if private {
                    crate::DeliveryReplyMode::DirectMessage
                } else {
                    crate::DeliveryReplyMode::Channel
                }
            );
            assert!(
                envelope
                    .control
                    .permitted_operations
                    .iter()
                    .any(|op| op == "reminder.set")
            );
            assert_ne!(envelope.control.message_id, original);
            let mut input = envelope
                .control
                .tool
                .operations
                .iter()
                .find(|op| op.token == "reply")
                .unwrap()
                .input
                .clone();
            assert_eq!(input["in_reply_to"], json!(original));
            assert_eq!(
                input["source"],
                json!(format!("@operator {}", crate::REPLY_BODY_PLACEHOLDER))
            );
            input["source"] = json!("@operator the current time");
            tx.transition_delivery_attempt(
                &ns(),
                &wake.attempt_id,
                DeliveryState::Submitted,
                &by(60_002),
            )?;
            if private {
                let thread = crate::store::direct_messages::thread_id(&owner().address);
                tx.transaction.execute(
                    "UPDATE thread_controls SET reply_allowance=0 WHERE thread_id=?1",
                    [thread.as_str()],
                )?;
            }
            Ok(input)
        });
        if private {
            assert!(
                StateStore::open(&f.state)
                    .unwrap()
                    .direct_message_peers()
                    .unwrap()
                    .iter()
                    .all(|p| p.last_agent_message_id.is_none())
            );
        }
        let ids = ProcessIds::default();
        let runtime = crate::LifecycleRuntime {
            state_directory: &f.state,
            home_directory: None,
            agent_config: Default::default(),
            cwd: Path::new("/test-repo"),
            workspace_id: Some("test-space"),
            current_pane_id: Some("pane"),
            session_socket_id: "test-socket",
            herdr: &f,
            clock: &crate::SystemClock,
            ids: &ids,
        };
        crate::execute_message_operation("reply", input, &runtime).unwrap();
        if private {
            assert!(
                StateStore::open(&f.state)
                    .unwrap()
                    .direct_message_peers()
                    .unwrap()
                    .iter()
                    .any(|p| p.last_agent_message_id.is_some())
            );
        }
        f.tx(|tx| {
            assert!(tx.delivery_disposition_exists(&ns(), &wake.attempt_id)?);
            if private {
                assert!(tx.dashboard_rows(&ns())?.messages.is_empty());
            }
            Ok(())
        });
    }
}

#[test]
fn reminder_terminal_only_answers_relay_once_in_the_original_conversation() {
    for private in [false, true] {
        let f = Fixture::new();
        let original = f.operator_original(private);
        f.settle_original();
        f.set("r", Some(original.clone()), crate::reminder::Repeat::Once);
        f.fire("r");
        let wake = f.lease("wake", 60_000);
        f.tx(|tx| {
            tx.transition_delivery_attempt(
                &ns(),
                &wake.attempt_id,
                DeliveryState::Submitted,
                &by(60_000),
            )?;
            let ending = crate::TerminalRelayEnd {
                agent_session_id: owner().session,
                ended_at_ms: 60_010,
                previous_end_ms: Some(5),
                text: "the current time".into(),
            };
            let relayed = tx.relay_terminal_ending(&ns(), &ending, &by(60_010))?;
            assert_eq!(relayed.len(), 1);
            let (parent, body): (String, String) = tx.transaction.query_row(
                "SELECT in_reply_to,body FROM collaboration_messages WHERE message_id=?1",
                [relayed[0].as_str()],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )?;
            assert_eq!(parent, original.as_str());
            assert_eq!(body, "the current time");
            assert!(
                tx.relay_terminal_ending(&ns(), &ending, &by(60_010))?
                    .is_empty()
            );
            let later = crate::TerminalRelayEnd {
                ended_at_ms: 60_020,
                ..ending
            };
            assert!(
                tx.relay_terminal_ending(&ns(), &later, &by(60_020))?
                    .is_empty()
            );
            if private {
                assert!(tx.dashboard_rows(&ns())?.messages.is_empty());
            }
            Ok(())
        });
    }
}
