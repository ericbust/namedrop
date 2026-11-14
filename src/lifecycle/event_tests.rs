use super::{EventError, handle_mux_event};
use crate::mux::{
    AgentState, Gone, MuxError, MuxEvent, PaneId, PaneSlot,
    fake::{FakeMux, Read, Surveyed},
};
use crate::reminder::{ReminderOwner, ReminderSpec, Repeat};
use crate::{
    ActorKind, AgentSessionId, Clock, DeliveryAttemptId, DeliveryId, DeliveryState, IdGenerator,
    LeaseDraft, LifecycleRuntime, MessageDraft, MessageId, MessageRecipientDraft,
    MutationAttribution, PaneAddressDraft, PaneAddressId, PaneBindingId, PaneRoleId, SpaceId,
    StateNamespace, StateStore, ThreadId,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

const REX: &str = "rex:/tmp/namedrop-event-tests.sock";
const SPACE: &str = "session:a";
const OTHER: &str = "session:b";

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    state: PathBuf,
    mux: FakeMux,
    endpoint: &'static str,
}

struct At100;

impl Clock for At100 {
    fn now_unix_ms(&self) -> u64 {
        100
    }
}

struct Ids(AtomicU64);

impl IdGenerator for Ids {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
    }
}

/// `SPACE` holds `pane-a` and `pane-b`, each with an agent working in the
/// session its address is bound to, and `OTHER` holds `pane-c`.
fn world(endpoint: &str) -> FakeMux {
    FakeMux::world()
        .endpoint(endpoint)
        .space(SPACE)
        .checkout("/repo")
        .pane("pane-a")
        .claude()
        .session("session-a")
        .working()
        .pane("pane-b")
        .codex()
        .session("session-b")
        .working()
        .space(OTHER)
        .checkout("/other")
        .pane("pane-c")
        .claude()
        .session("session-c")
        .working()
        .build()
}

fn attribution() -> MutationAttribution {
    MutationAttribution::new(ActorKind::Human, "test", 10)
}

fn address(id: &str) -> PaneAddressDraft {
    PaneAddressDraft {
        id: PaneAddressId::new(id),
        role_id: PaneRoleId::new(format!("role-{id}")),
        role_name: id.to_owned(),
        role_description: format!("{id} test role"),
        alias: id.to_owned(),
    }
}

impl Fixture {
    /// Both spaces activated, with an address named for and bound to each
    /// pane, its session observed, and a mention to it submitted.
    fn new(endpoint: &'static str) -> Self {
        let state = std::env::temp_dir().join(format!(
            "namedrop-event-tests-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(state.join("home")).unwrap();
        let fixture = Self {
            state,
            mux: world(endpoint),
            endpoint,
        };
        for (space, pane) in [(SPACE, "a"), (SPACE, "b"), (OTHER, "c")] {
            fixture.name(space, pane);
        }
        fixture
    }

    fn namespace(&self, space: &str) -> StateNamespace {
        StateNamespace::for_space(self.endpoint, space)
    }

    fn name(&self, space: &str, pane: &str) {
        let namespace = self.namespace(space);
        let id = PaneAddressId::new(pane);
        let session = AgentSessionId::new(format!("session-{pane}"));
        let binding = PaneBindingId::new(format!("binding-{pane}"));
        let attempt = DeliveryAttemptId::new(format!("attempt-{pane}"));
        let thread = ThreadId::new(format!("thread-{pane}"));
        let author = PaneAddressId::new(format!("author-{pane}"));
        StateStore::open(&self.state)
            .unwrap()
            .transaction(|transaction| {
                if transaction.worktree_state(&namespace)?.is_none() {
                    transaction.activate_worktree(&namespace, &attribution())?;
                }
                transaction.create_pane_address(
                    &namespace,
                    &address(author.as_str()),
                    &attribution(),
                )?;
                transaction.create_pane_address(&namespace, &address(pane), &attribution())?;
                transaction.bind_pane(
                    &namespace,
                    &binding,
                    &id,
                    &format!("pane-{pane}"),
                    &attribution(),
                )?;
                transaction.observe_agent_session(
                    &namespace,
                    &format!("observation-{pane}"),
                    &id,
                    &session,
                    "working",
                    &attribution(),
                )?;
                transaction.create_thread(&namespace, &thread, "Test thread", &attribution())?;
                transaction.complete_reconciliation(&namespace)?;
                transaction.append_message(
                    &namespace,
                    &MessageDraft {
                        thread_id: thread.clone(),
                        message_id: MessageId::new(format!("message-{pane}")),
                        in_reply_to: None,
                        author_address_id: author.clone(),
                        author_kind: ActorKind::Human,
                        author_alias_snapshot: author.as_str().to_owned(),
                        author_agent_session_snapshot: None,
                        request_id: format!("request-{pane}"),
                        authored_source: format!("@{pane} please take a look"),
                        body: "please take a look".to_owned(),
                        grammar_version: "mention/v1".to_owned(),
                        recipients: vec![MessageRecipientDraft {
                            address_id: id.clone(),
                            alias_snapshot: pane.to_owned(),
                            agent_session_snapshot: Some(session.clone()),
                            delivery_id: DeliveryId::new(format!("delivery-{pane}")),
                        }],
                        context_references: Vec::new(),
                    },
                    &attribution(),
                )?;
                transaction.lease_mailbox_head(
                    &namespace,
                    &id,
                    &LeaseDraft {
                        attempt_id: attempt.clone(),
                        binding_id: binding.clone(),
                        agent_session_id: session.clone(),
                        retry_of_attempt_id: None,
                        owner: "test-dispatcher".to_owned(),
                        acquired_at_ms: 20,
                        expires_at_ms: 30_020,
                    },
                    &attribution(),
                )?;
                transaction.transition_delivery_attempt(
                    &namespace,
                    &attempt,
                    DeliveryState::Submitted,
                    &attribution(),
                )?;
                transaction.set_reminder(
                    &namespace,
                    format!("reminder-{pane}"),
                    ReminderOwner {
                        address: id.clone(),
                        session: session.clone(),
                        binding: binding.clone(),
                        pane: format!("pane-{pane}"),
                    },
                    ReminderSpec {
                        body: "check the job".into(),
                        first_due_at_ms: 60_000,
                        in_reply_to: None,
                        repeat: Repeat::Once,
                    },
                    &format!("reminder-request-{pane}"),
                    &format!("reminder-request-{pane}"),
                )?;
                Ok(())
            })
            .unwrap();
    }

    fn handle(&self, event: MuxEvent) -> Result<serde_json::Value, EventError> {
        let ids = Ids(AtomicU64::new(1));
        let home = self.state.join("home");
        handle_mux_event(
            &LifecycleRuntime {
                state_directory: &self.state,
                home_directory: Some(&home),
                agent_config: crate::AgentConfigDirectories::default(),
                cwd: Path::new("/repo"),
                workspace_id: None,
                current_pane_id: None,
                session_socket_id: self.endpoint,
                mux: &self.mux,
                clock: &At100,
                ids: &ids,
            },
            &event,
        )
    }

    /// The aliases still named in `space`, not counting the mentions'
    /// authors.
    fn names(&self, space: &str) -> Vec<String> {
        let mut names: Vec<String> = StateStore::open(&self.state)
            .unwrap()
            .transaction(|transaction| transaction.list_pane_addresses(&self.namespace(space)))
            .unwrap()
            .into_iter()
            .map(|address| address.id.as_str().to_owned())
            .filter(|id| !id.starts_with("author-"))
            .collect();
        names.sort();
        names
    }

    fn query(&self, sql: &str, id: &str) -> String {
        rusqlite::Connection::open(self.state.join("namedrop.sqlite3"))
            .unwrap()
            .query_row(sql, [id], |row| row.get(0))
            .unwrap()
    }

    fn reminder(&self, pane: &str) -> String {
        self.query(
            "SELECT state FROM reminders WHERE reminder_id = ?1",
            &format!("reminder-{pane}"),
        )
    }

    fn attempt(&self, pane: &str) -> DeliveryState {
        let space = if pane == "c" { OTHER } else { SPACE };
        StateStore::open(&self.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.delivery_attempt_state(
                    &self.namespace(space),
                    &DeliveryAttemptId::new(format!("attempt-{pane}")),
                )
            })
            .unwrap()
    }

    fn activated(&self, space: &str) -> bool {
        StateStore::open(&self.state)
            .unwrap()
            .transaction(|transaction| transaction.worktree_state(&self.namespace(space)))
            .unwrap()
            .is_some_and(|state| state.activated && !state.deactivated)
    }

    /// Every survey of `space` answers that the space is gone.
    fn lose(&self, space: &'static str) {
        self.mux.on_read(move |_, read| match read {
            Read::Survey {
                scope: Surveyed::Space(id),
                ..
            } if id.as_str() == space => Err(MuxError::Gone(
                Gone::Space,
                format!("session {space} not found"),
            )),
            _ => Ok(()),
        });
    }

    fn assert_space_closed(&self) {
        assert!(self.names(SPACE).is_empty(), "{:?}", self.names(SPACE));
        for pane in ["a", "b"] {
            assert_eq!(self.reminder(pane), "failed");
            assert_eq!(self.attempt(pane), DeliveryState::Cancelled);
        }
        assert!(!self.activated(SPACE));
        assert_eq!(self.names(OTHER), ["c"]);
        assert_eq!(self.reminder("c"), "scheduled");
        assert_eq!(self.attempt("c"), DeliveryState::Submitted);
        assert!(self.activated(OTHER));
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.state);
    }
}

fn slot(space: &str, pane: &str) -> PaneSlot {
    PaneSlot {
        space: SpaceId::new(space),
        pane: PaneId::new(pane),
        tab: None,
    }
}

#[test]
fn a_pane_closed_retires_its_name() {
    let fixture = Fixture::new(REX);
    fixture.mux.close_pane("pane-a");
    fixture
        .handle(MuxEvent::PaneClosed(slot(SPACE, "pane-a")))
        .unwrap();
    assert_eq!(fixture.names(SPACE), ["b"]);
    assert_eq!(fixture.attempt("a"), DeliveryState::Cancelled);
    assert_eq!(fixture.attempt("b"), DeliveryState::Submitted);
}

#[test]
fn a_space_closed_retires_every_name_in_its_space_and_fails_its_reminders() {
    let fixture = Fixture::new(REX);
    let closed = fixture
        .handle(MuxEvent::SpaceClosed(SpaceId::new(SPACE)))
        .unwrap();
    assert_eq!(closed["removed_address_ids"].as_array().unwrap().len(), 4);
    fixture.assert_space_closed();
    fixture
        .handle(MuxEvent::SpaceClosed(SpaceId::new(SPACE)))
        .unwrap();
    fixture.assert_space_closed();
}

#[test]
fn a_resync_that_finds_the_space_gone_closes_it() {
    let fixture = Fixture::new(REX);
    fixture.lose(SPACE);
    fixture
        .handle(MuxEvent::Resync(SpaceId::new(SPACE)))
        .unwrap();
    fixture.assert_space_closed();
}

#[test]
fn a_resync_keeps_an_attempt_whose_session_runs_and_holds_one_whose_program_was_replaced() {
    let fixture = Fixture::new(REX);
    fixture.mux.set_session("pane-b", "session-b2");
    fixture.mux.set_state("pane-b", AgentState::Working);
    fixture
        .handle(MuxEvent::Resync(SpaceId::new(SPACE)))
        .unwrap();
    assert_eq!(fixture.attempt("a"), DeliveryState::Submitted);
    assert_eq!(fixture.attempt("b"), DeliveryState::Held);
    assert_eq!(fixture.names(SPACE), ["a", "b"]);
    assert!(fixture.activated(SPACE));
}

#[test]
fn a_gone_space_is_definitive_and_a_timeout_retries() {
    let fixture = Fixture::new(REX);
    fixture.lose(SPACE);
    assert!(matches!(
        fixture.handle(MuxEvent::PaneClosed(slot(SPACE, "pane-a"))),
        Err(EventError::Definitive(_))
    ));
    fixture.mux.on_read(|_, read| match read {
        Read::Survey {
            scope: Surveyed::Space(id),
            ..
        } if id.as_str() == OTHER => Err(MuxError::Timeout(
            "Rex request timed out after 5000ms".into(),
        )),
        _ => Ok(()),
    });
    for event in [
        MuxEvent::AgentStatusChanged(slot(OTHER, "pane-c")),
        MuxEvent::Resync(SpaceId::new(OTHER)),
    ] {
        assert_eq!(
            fixture.handle(event),
            Err(EventError::Retry(
                "Rex request timed out after 5000ms".to_owned()
            ))
        );
    }
    assert_eq!(fixture.names(OTHER), ["c"]);
}

#[test]
fn an_event_s_changes_are_recorded_as_its_endpoint_s_multiplexer() {
    for (endpoint, actor) in [(REX, "rex"), ("/tmp/herdr.sock", "herdr")] {
        let fixture = Fixture::new(endpoint);
        fixture.mux.close_pane("pane-a");
        fixture
            .handle(MuxEvent::PaneClosed(slot(SPACE, "pane-a")))
            .unwrap();
        assert_eq!(
            fixture.query(
                "SELECT actor_kind || ':' || actor_id FROM transition_facts
                 WHERE entity_id = ?1 ORDER BY rowid DESC LIMIT 1",
                "attempt-a",
            ),
            format!("plugin:{actor}")
        );
    }
}

/// Another pane's status change surveys the space after a block closed and
/// before its close is handled. A Rex block ID is never reused, so once Rex
/// says the block is gone the survey retires the name and cancels its mail,
/// as the close would. Herdr reuses pane IDs, and its survey only unbinds
/// the name, as it always has, with no lookup of the pane.
#[test]
fn a_rex_survey_retires_the_name_of_a_bound_block_it_no_longer_lists() {
    for (endpoint, left) in [(REX, vec!["b"]), ("/tmp/herdr.sock", vec!["a", "b"])] {
        let fixture = Fixture::new(endpoint);
        fixture.mux.close_pane("pane-a");
        fixture
            .handle(MuxEvent::AgentStatusChanged(slot(SPACE, "pane-b")))
            .unwrap();
        assert_eq!(fixture.names(SPACE), left, "{endpoint}");
        let looked_up = fixture
            .mux
            .reads()
            .contains(&Read::Process(PaneId::new("pane-a")));
        assert_eq!(looked_up, endpoint == REX, "{endpoint}");
        if endpoint == REX {
            assert_eq!(fixture.attempt("a"), DeliveryState::Cancelled);
        }
        fixture
            .handle(MuxEvent::PaneClosed(slot(SPACE, "pane-a")))
            .unwrap();
        assert_eq!(fixture.names(SPACE), left, "{endpoint}");
        assert_eq!(fixture.attempt("b"), DeliveryState::Submitted);
    }
}

/// A survey that leaves out a live block, as an incomplete listing does,
/// retires nothing. Rex is asked about the block alone, and a lookup that
/// finds it, times out, or cannot reach Rex keeps its name and its mail for
/// the next survey.
#[test]
fn a_rex_survey_keeps_the_name_of_a_bound_block_it_leaves_out_but_rex_still_has() {
    let faults: [Option<MuxError>; 3] = [
        None,
        Some(MuxError::Timeout(
            "Rex request timed out after 5000ms".into(),
        )),
        Some(MuxError::Unavailable("could not reach Rex".into())),
    ];
    for fault in faults {
        for event in [
            MuxEvent::AgentStatusChanged(slot(SPACE, "pane-b")),
            MuxEvent::Resync(SpaceId::new(SPACE)),
        ] {
            let fixture = Fixture::new(REX);
            fixture.mux.hide_pane("pane-a");
            let injected = fault.clone();
            fixture.mux.on_read(move |_, read| match (read, &injected) {
                (Read::Process(pane), Some(fault)) if pane.as_str() == "pane-a" => {
                    Err(fault.clone())
                }
                _ => Ok(()),
            });
            fixture.handle(event.clone()).unwrap();
            assert_eq!(fixture.names(SPACE), ["a", "b"], "{fault:?} {event:?}");
            assert_ne!(fixture.attempt("a"), DeliveryState::Cancelled);
        }
    }
}

/// A block named after a survey was taken is missing from it. Rex still has
/// the block, so its name stays.
#[test]
fn a_block_named_after_a_rex_survey_was_taken_keeps_its_name() {
    let fixture = Fixture::new(REX);
    let state = fixture.state.clone();
    let named = std::sync::atomic::AtomicBool::new(false);
    fixture.mux.on_read(move |mux, read| {
        if matches!(read, Read::Survey { .. }) && !named.swap(true, Ordering::SeqCst) {
            mux.open_pane(SPACE, |space| space.pane("pane-n").hidden());
            let namespace = StateNamespace::for_space(REX, SPACE);
            StateStore::open(&state)
                .unwrap()
                .transaction(|transaction| {
                    transaction.create_pane_address(&namespace, &address("n"), &attribution())?;
                    transaction.bind_pane(
                        &namespace,
                        &PaneBindingId::new("binding-n"),
                        &PaneAddressId::new("n"),
                        "pane-n",
                        &attribution(),
                    )
                })
                .unwrap();
        }
        Ok(())
    });
    fixture
        .handle(MuxEvent::AgentStatusChanged(slot(SPACE, "pane-b")))
        .unwrap();
    assert_eq!(fixture.names(SPACE), ["a", "b", "n"]);
}

/// A block's close was never handled, as when the bridge re-execs at the
/// flush that held it. The new bridge's `Resync` retires the name.
#[test]
fn a_resync_retires_the_name_of_a_block_whose_close_was_lost() {
    let fixture = Fixture::new(REX);
    fixture.mux.close_pane("pane-a");
    fixture
        .handle(MuxEvent::Resync(SpaceId::new(SPACE)))
        .unwrap();
    assert_eq!(fixture.names(SPACE), ["b"]);
    assert_eq!(fixture.attempt("a"), DeliveryState::Cancelled);
    assert_eq!(fixture.attempt("b"), DeliveryState::Submitted);
    assert_eq!(fixture.names(OTHER), ["c"]);
}

/// Only a name actively bound to the gone block is retired: not one a
/// person parked with `address unbind` whose last block it was, and not
/// one bound to another block since, even by a clock that stepped back.
#[test]
fn a_rex_close_and_survey_keep_every_name_not_bound_to_the_gone_block() {
    let parked = |transaction: &mut crate::StateTransaction<'_>, namespace: &StateNamespace| {
        transaction.unbind_pane(namespace, &PaneAddressId::new("a"), &attribution())?;
        transaction.create_pane_address(namespace, &address("z"), &attribution())?;
        transaction.bind_pane(
            namespace,
            &PaneBindingId::new("binding-z"),
            &PaneAddressId::new("z"),
            "pane-a",
            &attribution(),
        )
    };
    let rebound_earlier = |transaction: &mut crate::StateTransaction<'_>,
                           namespace: &StateNamespace| {
        transaction.unbind_pane(namespace, &PaneAddressId::new("b"), &attribution())?;
        transaction.rebind_pane(
            namespace,
            &PaneBindingId::new("binding-a2"),
            &PaneAddressId::new("a"),
            "pane-b",
            &MutationAttribution::new(ActorKind::Human, "test", 5),
        )
    };
    type Setup =
        fn(&mut crate::StateTransaction<'_>, &StateNamespace) -> Result<(), crate::StoreError>;
    let setups: [(&str, Setup); 2] = [("parked", parked), ("rebound earlier", rebound_earlier)];
    for (case, setup) in setups {
        for events in [
            vec![MuxEvent::PaneClosed(slot(SPACE, "pane-a"))],
            vec![MuxEvent::AgentStatusChanged(slot(SPACE, "pane-b"))],
            vec![MuxEvent::Resync(SpaceId::new(SPACE))],
        ] {
            let fixture = Fixture::new(REX);
            StateStore::open(&fixture.state)
                .unwrap()
                .transaction(|transaction| setup(transaction, &fixture.namespace(SPACE)))
                .unwrap();
            fixture.mux.close_pane("pane-a");
            for event in events {
                fixture.handle(event).unwrap();
            }
            assert_eq!(fixture.names(SPACE), ["a", "b"], "{case}");
            assert_ne!(fixture.attempt("a"), DeliveryState::Cancelled, "{case}");
        }
    }
}

/// The person's own address has no pane, so a closed space keeps it.
#[test]
fn a_space_closed_keeps_the_person_s_own_address() {
    let fixture = Fixture::new(REX);
    let operator = StateStore::open(&fixture.state)
        .unwrap()
        .transaction(|transaction| {
            transaction.ensure_operator_author(&fixture.namespace(SPACE), &attribution())
        })
        .unwrap()
        .address_id;
    fixture
        .handle(MuxEvent::SpaceClosed(SpaceId::new(SPACE)))
        .unwrap();
    fixture.assert_space_closed();
    let kept = StateStore::open(&fixture.state)
        .unwrap()
        .transaction(|transaction| {
            transaction.is_operator_address(&fixture.namespace(SPACE), &operator)
        })
        .unwrap();
    assert!(kept);
}

/// Only a gone space or pane is definitive. A server that is down, too
/// slow, too large in its answer, or garbled is tried again.
#[test]
fn only_a_gone_space_or_pane_is_definitive() {
    let failures = [
        MuxError::Gone(Gone::Space, "session gone".into()),
        MuxError::Timeout("timed out".into()),
        MuxError::TooLarge("too large".into()),
        MuxError::Unavailable("no server".into()),
        MuxError::Protocol("garbled".into()),
    ];
    for failure in failures {
        let definitive = matches!(failure, MuxError::Gone(..));
        let fixture = Fixture::new(REX);
        let failed = std::sync::Mutex::new(Some(failure));
        fixture.mux.on_read(move |_, read| match read {
            Read::Survey {
                scope: Surveyed::Space(id),
                ..
            } if id.as_str() == SPACE => failed.lock().unwrap().take().map_or(Ok(()), Err),
            _ => Ok(()),
        });
        let handled = fixture.handle(MuxEvent::PaneClosed(slot(SPACE, "pane-a")));
        assert_eq!(
            matches!(handled, Err(EventError::Definitive(_))),
            definitive,
            "{handled:?}"
        );
        assert_eq!(
            matches!(handled, Err(EventError::Retry(_))),
            !definitive,
            "{handled:?}"
        );
    }
}

/// A space Namedrop is no longer activated for is left alone by a
/// `Resync`, whatever its server now holds.
#[test]
fn a_resync_of_a_deactivated_space_changes_nothing() {
    let fixture = Fixture::new(REX);
    StateStore::open(&fixture.state)
        .unwrap()
        .transaction(|transaction| {
            transaction.deactivate_worktree(&fixture.namespace(SPACE), &attribution())
        })
        .unwrap();
    fixture.mux.close_pane("pane-a");
    assert!(matches!(
        fixture.handle(MuxEvent::Resync(SpaceId::new(SPACE))),
        Err(EventError::Definitive(_))
    ));
    assert_eq!(fixture.names(SPACE), ["a", "b"]);
    assert_eq!(fixture.attempt("a"), DeliveryState::Submitted);
}
