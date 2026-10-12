use namedrop::{
    ActorKind, AgentSessionId, Clock, HerdrCommandExecutor, HerdrCommandOutput, IdGenerator,
    LifecycleRuntime, MutationAttribution, PaneAddressDraft, PaneAddressId, PaneAddressLifecycle,
    PaneAddressRecord, PaneBindingId, PaneRoleId, StateNamespace, StateStore,
    execute_message_operation, parse_mentions, parse_prompt_mentions, recipient_outlooks,
};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

#[test]
fn mention_parser_separates_recipients_context_and_literals() {
    let source = "  @reviewer @code-reviewer Please inspect foo@bar, \\@planner, \
`@inline`, and ```\n@fenced\n``` then @planner.";

    let parsed = parse_mentions(source).unwrap();

    assert_eq!(parsed.recipient_aliases, ["reviewer", "code-reviewer"]);
    assert_eq!(
        parsed.body,
        "Please inspect foo@bar, \\@planner, `@inline`, and ```\n@fenced\n``` then @planner."
    );
    assert_eq!(parsed.context_reference_aliases, ["planner"]);
}

#[test]
fn mention_parser_rejects_missing_recipients_and_empty_bodies() {
    for source in [
        "ordinary native text @reviewer",
        "@reviewer",
        "@reviewer, body",
        "@TODO the first token is not a recipient",
    ] {
        assert!(parse_mentions(source).is_err(), "{source:?} should fail");
    }
}

#[test]
fn native_prompt_mentions_route_only_leading_recipients_and_keep_the_complete_prompt() {
    let source = "@reviewer @planner Please ask @researcher to inspect \
name@example.com, \\@literal, and `@inline`.";

    let parsed = parse_prompt_mentions(source).unwrap();

    assert_eq!(parsed.recipient_aliases, ["reviewer", "planner"]);
    assert_eq!(parsed.body, source);
    assert!(parsed.context_reference_aliases.is_empty());
    assert!(
        parse_prompt_mentions("Please ask @reviewer to inspect this.").is_err(),
        "an embedded Mention must remain available to the local agent"
    );
    assert!(
        parse_prompt_mentions(" @reviewer leading whitespace is not column zero").is_err(),
        "only a column-zero Mention takes the direct hook route"
    );
}

/// The live address projection returned by `address.list`.
fn projection(aliases: &[&str]) -> Vec<PaneAddressRecord> {
    aliases
        .iter()
        .map(|alias| PaneAddressRecord {
            pane_type: namedrop::PaneType::Agent,
            id: PaneAddressId::new(format!("address-{alias}")),
            role_id: PaneRoleId::new(format!("role-{alias}")),
            role_name: (*alias).to_owned(),
            role_description: format!("{alias} test role"),
            persona: None,
            alias: (*alias).to_owned(),
            lifecycle: PaneAddressLifecycle::Occupied,
            pane_id: Some(format!("pane-{alias}")),
            pane_label: None,
            agent_session_id: None,
            occupant_label: None,
            agent_status: Some("idle".to_owned()),
            retired: false,
            collaboration_disabled: false,
        })
        .collect()
}

#[test]
fn every_recipient_situation_states_what_sending_will_do() {
    let addresses = [
        situated("busy", PaneAddressLifecycle::Occupied, Some("working")),
        situated("starting", PaneAddressLifecycle::Occupied, None),
        situated("idle", PaneAddressLifecycle::Occupied, Some("idle")),
        situated("finished", PaneAddressLifecycle::Occupied, Some("done")),
        situated("empty", PaneAddressLifecycle::Empty, Some("empty")),
        situated("unbound", PaneAddressLifecycle::Unbound, None),
    ];
    let aliases = [
        "busy", "starting", "idle", "finished", "empty", "unbound", "unlisted",
    ]
    .map(str::to_owned);

    let outlook = recipient_outlooks(&aliases, &addresses);

    assert_eq!(
        outlook
            .iter()
            .map(|recipient| recipient.statement.as_str())
            .collect::<Vec<_>>(),
        [
            "@busy: Agent Session status working — this message stays queued until that Agent Session settles.",
            "@starting: Agent Session status unknown — this message stays queued until that Agent Session settles.",
            "@idle: Agent Session status idle — settled, so delivery is imminent.",
            "@finished: Agent Session status done — settled, so delivery is imminent.",
            "@empty: no Agent Session in the bound pane — this message queues normally with nobody there to receive it.",
            "@unbound: no bound pane — this message queues normally with nobody there to receive it."
        ],
        "an alias the projection does not list is left out; the send rejects it"
    );
    assert_eq!(outlook[0].alias, "busy");
    assert_eq!(outlook[0].agent_status.as_deref(), Some("working"));
    assert_eq!(outlook[0].lifecycle, PaneAddressLifecycle::Occupied);
    assert_eq!(outlook[5].agent_status, None);
    assert_eq!(outlook[5].lifecycle, PaneAddressLifecycle::Unbound);
}

fn situated(
    alias: &str,
    lifecycle: PaneAddressLifecycle,
    agent_status: Option<&str>,
) -> PaneAddressRecord {
    PaneAddressRecord {
        pane_type: namedrop::PaneType::Agent,
        lifecycle,
        agent_status: agent_status.map(str::to_owned),
        ..projection(&[alias]).remove(0)
    }
}

#[test]
fn mention_parser_keeps_at_sign_tokens_that_are_not_alias_shaped_literal() {
    let source = "@reviewer @TODO write this up: a bare @ is fine, @foo_bar and @bad--alias and \
@trailing- and @Planner are prose, but @planner is a Mention, and this ends with @";

    let parsed = parse_mentions(source).unwrap();

    assert_eq!(parsed.recipient_aliases, ["reviewer"]);
    assert_eq!(
        parsed.body,
        "@TODO write this up: a bare @ is fine, @foo_bar and @bad--alias and @trailing- and \
@Planner are prose, but @planner is a Mention, and this ends with @"
    );
    assert_eq!(parsed.context_reference_aliases, ["planner"]);
}

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

struct TestDirectory {
    path: PathBuf,
    home: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "namedrop-message-test-{}-{}",
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
        fs::remove_dir_all(&self.path).unwrap();
    }
}

struct FakeHerdr;

impl HerdrCommandExecutor for FakeHerdr {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let stdout = match args {
            ["workspace", "list"] => {
                r#"{"result":{"workspaces":[{"workspace_id":"workspace-1"}]}}"#
            }
            ["worktree", "list", "--cwd", "/repo", "--json"] => {
                r#"{"result":{"worktrees":[{"path":"/repo","open_workspace_id":"workspace-1"}]}}"#
            }
            ["pane", "list", "--workspace", "workspace-1"] => {
                r#"{"result":{"panes":[{"pane_id":"pane-author","terminal_id":"terminal-author","tab_id":"tab-1","workspace_id":"workspace-1","agent_session":{"value":"session-author"},"agent_status":"idle"}]}}"#
            }
            _ => return Err(format!("unexpected Herdr command: {args:?}")),
        };
        Ok(HerdrCommandOutput {
            stdout: stdout.to_owned(),
        })
    }
}

struct FixedClock;

impl Clock for FixedClock {
    fn now_unix_ms(&self) -> u64 {
        42
    }
}

struct SequenceIds(AtomicU64);

impl IdGenerator for SequenceIds {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
    }
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

fn setup_addresses(directory: &TestDirectory) {
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let attribution = MutationAttribution::new(ActorKind::Human, "test", 1);
    let mut store = StateStore::open(directory.path()).unwrap();
    store
        .transaction(|transaction| {
            transaction.activate_worktree(&namespace, &attribution)?;
            for draft in [
                address("address-author", "author"),
                address("address-reviewer", "reviewer"),
                address("address-planner", "planner"),
                address("address-researcher", "researcher"),
            ] {
                transaction.create_pane_address(&namespace, &draft, &attribution)?;
            }
            transaction.bind_pane(
                &namespace,
                &PaneBindingId::new("binding-author"),
                &PaneAddressId::new("address-author"),
                "pane-author",
                &attribution,
            )?;
            transaction.observe_agent_session(
                &namespace,
                "observation-author",
                &PaneAddressId::new("address-author"),
                &AgentSessionId::new("session-author"),
                "idle",
                &attribution,
            )
        })
        .unwrap();
}

#[test]
fn explicit_send_resolves_and_atomically_fans_out_with_a_visible_receipt() {
    let directory = TestDirectory::new();
    setup_addresses(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-author"),
        session_socket_id: "socket-a",
        herdr: &FakeHerdr,
        clock: &FixedClock,
        ids: &ids,
    };
    let input = serde_json::json!({
        "request_id": "request-1",
        "source": "@reviewer @planner Please inspect @researcher."
    });

    let first = execute_message_operation("send", input.clone(), &runtime).unwrap();
    let replay = execute_message_operation("send", input, &runtime).unwrap();

    assert_eq!(first["thread_order"], 1);
    assert_eq!(first["idempotent_replay"], false);
    assert_eq!(
        first["recipients"],
        serde_json::json!([
            {"address_id": "address-reviewer", "alias": "reviewer"},
            {"address_id": "address-planner", "alias": "planner"}
        ])
    );
    assert_eq!(
        first["context_references"],
        serde_json::json!([
            {"address_id": "address-researcher", "alias": "researcher"}
        ])
    );
    assert_eq!(first["deliveries"].as_array().unwrap().len(), 2);
    assert_eq!(replay["message_id"], first["message_id"]);
    assert_eq!(replay["deliveries"], first["deliveries"]);
    assert_eq!(replay["idempotent_replay"], true);
}

#[test]
fn a_held_mailbox_is_reported_on_new_sends_and_idempotent_replays() {
    let directory = TestDirectory::new();
    setup_addresses(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-author"),
        session_socket_id: "socket-a",
        herdr: &FakeHerdr,
        clock: &FixedClock,
        ids: &ids,
    };
    let first = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id":"held-head", "source":"@reviewer Original task"
        }),
        &runtime,
    )
    .unwrap();
    // Seed the retained mailbox state observed after a session change. The
    // runtime regression separately drives the transition that creates it.
    let connection = rusqlite::Connection::open(directory.path().join("namedrop.sqlite3")).unwrap();
    connection
        .execute(
            "UPDATE mailbox_deliveries SET state='held' WHERE delivery_id=?1",
            [first["deliveries"][0]["delivery_id"].as_str().unwrap()],
        )
        .unwrap();
    let input =
        serde_json::json!({"request_id":"behind-hold", "source":"@reviewer @planner New task"});
    for response in [
        execute_message_operation("send", input.clone(), &runtime).unwrap(),
        execute_message_operation("send", input, &runtime).unwrap(),
    ] {
        let statement = response["delivery_outlook"][0]["statement"]
            .as_str()
            .unwrap();
        assert!(
            statement.contains("held delivery") && statement.contains("doctor"),
            "{statement}"
        );
        assert!(
            !response["delivery_outlook"][1]["statement"]
                .as_str()
                .unwrap()
                .contains("held delivery")
        );
    }
    assert_eq!(
        connection
            .query_row(
                "SELECT state FROM mailbox_deliveries WHERE delivery_id=?1",
                [first["deliveries"][0]["delivery_id"].as_str().unwrap()],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "held"
    );
}

#[test]
fn agent_authored_send_persists_explicit_delegation_provenance() {
    let directory = TestDirectory::new();
    setup_addresses(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-author"),
        session_socket_id: "socket-a",
        herdr: &FakeHerdr,
        clock: &FixedClock,
        ids: &ids,
    };

    let sent = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "delegated-request",
            "source": "@reviewer @planner compare this",
            "author_kind": "agent_session"
        }),
        &runtime,
    )
    .unwrap();

    let grammar_version: String =
        rusqlite::Connection::open(directory.path().join("namedrop.sqlite3"))
            .unwrap()
            .query_row(
                "SELECT grammar_version FROM collaboration_messages WHERE message_id = ?1",
                [sent["message_id"].as_str().unwrap()],
                |row| row.get(0),
            )
            .unwrap();
    assert_eq!(grammar_version, "delegated/v1");
}

#[test]
fn agent_send_records_the_current_agent_session_as_author() {
    let directory = TestDirectory::new();
    setup_addresses(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-author"),
        session_socket_id: "socket-a",
        herdr: &FakeHerdr,
        clock: &FixedClock,
        ids: &ids,
    };
    let sent = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "agent-request",
            "source": "@planner Agent-authored message.",
            "author_kind": "agent_session"
        }),
        &runtime,
    )
    .unwrap();
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let facts = StateStore::open(directory.path())
        .unwrap()
        .transaction(|transaction| transaction.transition_facts(&namespace))
        .unwrap();
    let creation = facts
        .iter()
        .find(|fact| fact.entity_id == sent["message_id"].as_str().unwrap())
        .unwrap();

    assert_eq!(creation.actor_kind, ActorKind::AgentSession);
    assert_eq!(creation.actor_id, "session-author");
}

#[test]
fn idempotent_replay_uses_the_original_resolution_after_an_alias_rename() {
    let directory = TestDirectory::new();
    setup_addresses(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-author"),
        session_socket_id: "socket-a",
        herdr: &FakeHerdr,
        clock: &FixedClock,
        ids: &ids,
    };
    let input = serde_json::json!({
        "request_id": "stable-request",
        "source": "@reviewer Please inspect this."
    });
    let first = execute_message_operation("send", input.clone(), &runtime).unwrap();
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    StateStore::open(directory.path())
        .unwrap()
        .transaction(|transaction| {
            transaction.replace_alias(
                &namespace,
                &PaneAddressId::new("address-reviewer"),
                "code-reviewer",
                &MutationAttribution::new(ActorKind::Human, "test", 43),
            )
        })
        .unwrap();

    let replay = execute_message_operation("send", input, &runtime).unwrap();

    assert_eq!(replay["message_id"], first["message_id"]);
    assert_eq!(replay["recipients"], first["recipients"]);
    assert_eq!(replay["idempotent_replay"], true);
}

#[test]
fn reply_routes_explicitly_and_reply_all_requires_every_previous_participant() {
    let directory = TestDirectory::new();
    setup_addresses(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let author_runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-author"),
        session_socket_id: "socket-a",
        herdr: &FakeHerdr,
        clock: &FixedClock,
        ids: &ids,
    };
    let first = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "request-first",
            "source": "@reviewer @planner Please inspect @researcher."
        }),
        &author_runtime,
    )
    .unwrap();
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    StateStore::open(directory.path())
        .unwrap()
        .transaction(|transaction| {
            transaction.bind_pane(
                &namespace,
                &PaneBindingId::new("binding-reviewer"),
                &PaneAddressId::new("address-reviewer"),
                "pane-reviewer",
                &MutationAttribution::new(ActorKind::Human, "test", 44),
            )
        })
        .unwrap();
    let reviewer_runtime = LifecycleRuntime {
        current_pane_id: Some("pane-reviewer"),
        ..author_runtime
    };
    let reply = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "request-reply",
            "in_reply_to": first["message_id"],
            "source": "@author Reply with @planner."
        }),
        &reviewer_runtime,
    )
    .unwrap();

    assert_eq!(reply["thread_id"], first["thread_id"]);
    assert_eq!(reply["thread_order"], 2);
    assert_eq!(
        reply["recipients"],
        serde_json::json!([{"address_id": "address-author", "alias": "author"}])
    );
    assert_eq!(
        reply["context_references"],
        serde_json::json!([{"address_id": "address-planner", "alias": "planner"}])
    );

    let edited_reply_all = execute_message_operation(
        "reply",
        serde_json::json!({
            "request_id": "request-reply-all-edited",
            "in_reply_to": first["message_id"],
            "reply_all": true,
            "source": "@author The final explicit recipients may be edited."
        }),
        &reviewer_runtime,
    )
    .unwrap();
    assert_eq!(edited_reply_all["thread_id"], first["thread_id"]);
    assert_eq!(edited_reply_all["thread_order"], 3);
    assert_eq!(
        edited_reply_all["recipients"],
        serde_json::json!([{"address_id": "address-author", "alias": "author"}])
    );
}

#[test]
fn unresolved_or_tombstoned_mentions_reject_without_partial_mailbox_writes() {
    let directory = TestDirectory::new();
    setup_addresses(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-author"),
        session_socket_id: "socket-a",
        herdr: &FakeHerdr,
        clock: &FixedClock,
        ids: &ids,
    };
    let unknown = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "request-unknown",
            "source": "@planner @missing This must be atomic."
        }),
        &runtime,
    )
    .unwrap_err();
    assert!(unknown.contains("unknown Mention: @missing"));

    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    StateStore::open(directory.path())
        .unwrap()
        .transaction(|transaction| {
            transaction.replace_alias(
                &namespace,
                &PaneAddressId::new("address-reviewer"),
                "code-reviewer",
                &MutationAttribution::new(ActorKind::Human, "test", 45),
            )
        })
        .unwrap();
    let tombstone = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "request-tombstone",
            "source": "@reviewer This alias is retired."
        }),
        &runtime,
    )
    .unwrap_err();
    assert!(tombstone.contains("use @code-reviewer"));

    StateStore::open(directory.path())
        .unwrap()
        .transaction(|transaction| {
            transaction.retire_pane_address(
                &namespace,
                &PaneAddressId::new("address-researcher"),
                &MutationAttribution::new(ActorKind::Human, "test", 46),
            )
        })
        .unwrap();
    for (request_id, source, expected) in [
        (
            "request-body-unknown",
            "@planner Please compare @missing.",
            "unknown Mention: @missing",
        ),
        (
            "request-body-tombstone",
            "@planner Please compare @reviewer.",
            "retired Mention: @reviewer; use @code-reviewer",
        ),
        (
            "request-body-retired",
            "@planner Please ask @researcher.",
            "retired Mention: @researcher",
        ),
    ] {
        let rejected = execute_message_operation(
            "send",
            serde_json::json!({"request_id": request_id, "source": source}),
            &runtime,
        )
        .unwrap_err();
        assert!(rejected.contains(expected), "{source:?} gave {rejected:?}");
    }

    let valid = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "request-valid",
            "source": "@planner This queues after the rejected fanout."
        }),
        &runtime,
    )
    .unwrap();
    assert_eq!(valid["deliveries"][0]["mailbox_order"], 1);
}

#[test]
fn at_sign_prose_that_is_not_alias_shaped_sends_without_a_context_reference() {
    let directory = TestDirectory::new();
    setup_addresses(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-author"),
        session_socket_id: "socket-a",
        herdr: &FakeHerdr,
        clock: &FixedClock,
        ids: &ids,
    };
    let body = "@TODO wrap up: a bare @ is fine, @foo_bar and @Planner are prose, \
mail name@example.com, \\@planner is escaped, and `@planner` is code.";

    let sent = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "request-prose",
            "source": format!("@planner {body}")
        }),
        &runtime,
    )
    .unwrap();

    assert_eq!(
        sent["recipients"],
        serde_json::json!([{"address_id": "address-planner", "alias": "planner"}])
    );
    assert_eq!(sent["context_references"], serde_json::json!([]));
    assert_eq!(sent["deliveries"].as_array().unwrap().len(), 1);
    let stored_body: String = rusqlite::Connection::open(directory.path().join("namedrop.sqlite3"))
        .unwrap()
        .query_row(
            "SELECT body FROM collaboration_messages WHERE message_id = ?1",
            [sent["message_id"].as_str().unwrap()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored_body, body);
}

#[test]
fn request_ids_reject_conflicts_while_new_ids_create_new_messages() {
    let directory = TestDirectory::new();
    setup_addresses(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-author"),
        session_socket_id: "socket-a",
        herdr: &FakeHerdr,
        clock: &FixedClock,
        ids: &ids,
    };
    let first = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "request-stable",
            "source": "@planner First body."
        }),
        &runtime,
    )
    .unwrap();
    let conflict = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "request-stable",
            "source": "@planner Different body."
        }),
        &runtime,
    )
    .unwrap_err();
    assert!(conflict.contains("different message content"));
    let new_request = execute_message_operation(
        "send",
        serde_json::json!({
            "request_id": "request-new",
            "source": "@planner First body."
        }),
        &runtime,
    )
    .unwrap();
    assert_ne!(new_request["message_id"], first["message_id"]);
}

#[test]
fn request_fingerprint_matches_the_main_serialization_and_replays() {
    let directory = TestDirectory::new();
    setup_addresses(&directory);
    let ids = SequenceIds(AtomicU64::new(1));
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: Some(directory.home()),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: None,
        current_pane_id: Some("pane-author"),
        session_socket_id: "socket-a",
        herdr: &FakeHerdr,
        clock: &FixedClock,
        ids: &ids,
    };
    let input = serde_json::json!({"request_id":"main-fingerprint","source":"@reviewer Inspect."});
    execute_message_operation("send", input.clone(), &runtime).unwrap();
    let db = rusqlite::Connection::open(directory.path().join("namedrop.sqlite3")).unwrap();
    let fingerprint:String=db.query_row("SELECT request_fingerprint FROM idempotency_records WHERE request_id='main-fingerprint'",[],|row|row.get(0)).unwrap();
    let expected = r#"{"author_address_id":"address-author","author_kind":"human","authored_source":"@reviewer Inspect.","body":"Inspect.","context_references":[],"grammar_version":"mention/v1","in_reply_to":null,"recipients":[{"address_id":"address-reviewer"}]}"#;
    assert_eq!(fingerprint, expected);
    db.execute(
        "UPDATE idempotency_records SET request_fingerprint=?1 WHERE request_id='main-fingerprint'",
        [expected],
    )
    .unwrap();
    assert_eq!(
        execute_message_operation("send", input, &runtime).unwrap()["idempotent_replay"],
        true
    );
}
