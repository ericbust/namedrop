use super::*;
use crate::message::{
    carries_leading_mentions, literal_leading_mentions, parse_mentions, parse_mentions_in,
    parse_prompt_mentions,
};
use crate::mux::fake::FakeMux;
use crate::{
    ActorKind, AgentSessionId, MutationAttribution, PaneAddressDraft, PaneAddressId, PaneBindingId,
    PaneRoleId, ProcessIds, StateStore, SystemClock,
};
use rusqlite::Connection;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

const MISSING: &str = "message must begin with one or more recipient Mentions";
pub(crate) const REX: &str = "rex:/tmp/namedrop-relay-tests.sock";

fn remote(alias: &str, label: &str) -> RemoteName {
    RemoteName {
        alias: alias.to_owned(),
        label: label.to_owned(),
    }
}

/// Outside a Rex home's send, `@a@b` reads exactly as it always has.
#[test]
fn a_remote_shaped_token_reads_as_today_without_remote_hosts() {
    for parse in [parse_mentions as fn(&str) -> Result<_, String>, |source| {
        parse_mentions_in(source, &NoRemoteHosts)
    }] {
        assert_eq!(parse("@a@b hi").unwrap_err(), MISSING);
        let parsed = parse("@alice @a@b hi").unwrap();
        assert_eq!(parsed.recipients.aliases, ["alice"]);
        assert_eq!(parsed.body, "@a@b hi");
        assert!(parsed.remote.is_empty());
        let parsed = parse("@alice see @bob@devbox now").unwrap();
        assert_eq!(parsed.body, "see @bob@devbox now");
        assert_eq!(parsed.context_reference_aliases(|_| false), ["bob"]);
        assert!(parsed.remote.is_empty());
    }
}

#[test]
fn a_prompt_or_a_reply_body_naming_a_remote_host_stays_literal() {
    assert_eq!(
        parse_prompt_mentions("@bob@devbox hi").unwrap_err(),
        MISSING
    );
    assert!(!carries_leading_mentions("@bob@devbox hi"));
    assert_eq!(
        literal_leading_mentions("@bob@devbox said so"),
        "@bob@devbox said so"
    );
    let reply = parse_mentions(&format!(
        "@alice {}",
        literal_leading_mentions("@bob@devbox said so")
    ))
    .unwrap();
    assert_eq!(reply.recipients.aliases, ["alice"]);
    assert!(reply.remote.is_empty());
}

#[test]
fn a_rex_send_reads_an_exact_listed_label_as_a_remote_recipient() {
    let hosts = Hosts(vec!["studio".to_owned(), "devbox".to_owned()]);
    let parsed = parse_mentions_in("@bob@devbox hi", &hosts).unwrap();
    assert!(parsed.recipients.aliases.is_empty());
    assert_eq!(parsed.remote, [remote("bob", "devbox")]);
    assert_eq!(parsed.body, "hi");

    let parsed = parse_mentions_in(
        "@alice @bob@devbox, @bob@devbox @carol@studio: @dan hi",
        &hosts,
    )
    .unwrap();
    assert_eq!(parsed.recipients.aliases, ["alice", "dan"]);
    assert_eq!(
        parsed.remote,
        [remote("bob", "devbox"), remote("carol", "studio")]
    );
    assert_eq!(parsed.body, "hi");

    let parsed = parse_mentions_in("@alice hi @bob@devbox", &hosts).unwrap();
    assert!(parsed.remote.is_empty());
    assert_eq!(parsed.context_reference_aliases(|_| false), ["bob"]);
}

#[test]
fn a_rex_send_refuses_a_label_that_is_not_exactly_listed_and_lists_the_labels() {
    let hosts = Hosts(vec![
        "studio".to_owned(),
        "devbox".to_owned(),
        "dev.box".to_owned(),
        "dev:box".to_owned(),
    ]);
    let listed = "(rex hosts list): studio, devbox, dev.box, dev:box";
    for (source, label) in [
        ("@bob@DevBox hi", "DevBox"),
        ("@bob@devbx hi", "devbx"),
        ("@bob@dev.box hi", "dev.box"),
        ("@bob@dev:box hi", "dev:box"),
        ("@alice @bob@devbx hi", "devbx"),
    ] {
        assert_eq!(
            parse_mentions_in(source, &hosts).unwrap_err(),
            format!("{label} is not in your Rex hosts {listed}"),
            "{source}"
        );
    }
    assert_eq!(
        parse_mentions_in("@bob@devbox hi", &Hosts::default()).unwrap_err(),
        "devbox is not in your Rex hosts (rex hosts list lists none)"
    );
    assert!(
        parse_mentions_in("#ws @bob@devbox hi", &hosts)
            .unwrap_err()
            .contains("takes no workspace label")
    );
}

#[test]
fn a_label_is_1_to_64_letters_digits_hyphens_and_underscores() {
    for label in ["a", "dev-box_2", &"x".repeat(64)] {
        assert!(valid_label(label), "{label}");
    }
    for label in [
        "",
        &"x".repeat(65),
        "dev.box",
        "dev:box",
        "dev box",
        "dev\rbox",
        "é",
    ] {
        assert!(!valid_label(label), "{label:?}");
    }
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A Rex space with the named sender `@alice` in block `block:a`, and
/// `@dan`, who has no block.
pub(crate) struct Space {
    pub(crate) state: PathBuf,
    mux: FakeMux,
    socket: &'static str,
}

impl Space {
    pub(crate) fn new(socket: &'static str) -> Self {
        let state = std::env::temp_dir().join(format!(
            "namedrop-relay-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&state).unwrap();
        let mux = FakeMux::world()
            .space("session:a")
            .checkout("/repo")
            .pane("block:a")
            .tab("window:a")
            .session("session-alice")
            .idle()
            .build();
        let namespace = crate::StateNamespace::for_space(socket, "session:a");
        let attribution = MutationAttribution::new(ActorKind::Human, "test", 1);
        StateStore::open(&state)
            .unwrap()
            .transaction(|transaction| {
                transaction.activate_worktree(&namespace, &attribution)?;
                transaction.create_pane_address(
                    &namespace,
                    &PaneAddressDraft {
                        id: PaneAddressId::new("address-alice"),
                        role_id: PaneRoleId::new("role-alice"),
                        role_name: "alice".to_owned(),
                        role_description: "alice".to_owned(),
                        alias: "alice".to_owned(),
                    },
                    &attribution,
                )?;
                transaction.create_pane_address(
                    &namespace,
                    &PaneAddressDraft {
                        id: PaneAddressId::new("address-dan"),
                        role_id: PaneRoleId::new("role-dan"),
                        role_name: "dan".to_owned(),
                        role_description: "dan".to_owned(),
                        alias: "dan".to_owned(),
                    },
                    &attribution,
                )?;
                transaction.bind_pane(
                    &namespace,
                    &PaneBindingId::new("binding-alice"),
                    &PaneAddressId::new("address-alice"),
                    "block:a",
                    &attribution,
                )?;
                transaction.observe_agent_session(
                    &namespace,
                    "observation-alice",
                    &PaneAddressId::new("address-alice"),
                    &AgentSessionId::new("session-alice"),
                    "idle",
                    &attribution,
                )
            })
            .unwrap();
        Self { state, mux, socket }
    }

    pub(crate) fn send(&self, request: &str, source: &str) -> Result<serde_json::Value, String> {
        self.operate(
            "send",
            serde_json::json!({"request_id": request, "source": source}),
        )
    }

    pub(crate) fn reply(
        &self,
        request: &str,
        in_reply_to: &str,
        source: &str,
    ) -> Result<serde_json::Value, String> {
        self.operate(
            "reply",
            serde_json::json!({"request_id": request, "in_reply_to": in_reply_to, "source": source}),
        )
    }

    /// Drains the space's mailboxes as its status worker does.
    pub(crate) fn drain(&self) {
        let ids = ProcessIds::default();
        crate::drain_mailboxes(&self.runtime(&ids)).unwrap();
    }

    fn runtime<'a>(
        &'a self,
        ids: &'a ProcessIds,
    ) -> crate::LifecycleRuntime<'a, FakeMux, SystemClock, ProcessIds> {
        crate::LifecycleRuntime {
            state_directory: &self.state,
            home_directory: None,
            agent_config: Default::default(),
            cwd: std::path::Path::new("/repo"),
            workspace_id: Some("session:a"),
            current_pane_id: Some("block:a"),
            session_socket_id: self.socket,
            mux: &self.mux,
            clock: &SystemClock,
            ids,
        }
    }

    fn operate(
        &self,
        operation: &str,
        input: serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let ids = ProcessIds::default();
        crate::execute_message_operation(operation, input, &self.runtime(&ids))
    }

    pub(crate) fn outbound(&self) -> Vec<Vec<String>> {
        let Ok(relay) = Connection::open_with_flags(
            self.state.join("relay.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) else {
            return Vec::new();
        };
        relay
            .prepare(
                "SELECT label, message_id, recipient, endpoint, space, sender, thread,
                    COALESCE(in_reply_to, '-'), text, state FROM outbound ORDER BY recipient",
            )
            .unwrap()
            .query_map([], |row| {
                (0..10)
                    .map(|column| row.get(column))
                    .collect::<Result<_, _>>()
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    pub(crate) fn main(&self, sql: &str) -> Vec<Vec<String>> {
        Connection::open(self.state.join("namedrop.sqlite3"))
            .unwrap()
            .prepare(sql)
            .unwrap()
            .query_map([], |row| {
                (0..row.as_ref().column_count())
                    .map(|column| row.get(column))
                    .collect::<Result<_, _>>()
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }
}

impl Drop for Space {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.state);
    }
}

#[test]
fn a_rex_send_queues_one_item_per_remote_recipient_and_settles_its_relay_authors() {
    let space = Space::new(REX);
    let rex = tests_support::hosts(&["devbox", "studio"]);
    let sent = rex
        .run(|| space.send("request-1", "@bob@devbox @carol@studio hello there"))
        .unwrap();
    assert_eq!(rex.calls(), ["--autostart=false hosts list --json"]);
    assert_eq!(
        sent["relayed"],
        serde_json::json!([
            {"alias": "bob", "host": "devbox",
             "statement": "@bob@devbox: queued for the Namedrop on devbox, which delivers it there."},
            {"alias": "carol", "host": "studio",
             "statement": "@carol@studio: queued for the Namedrop on studio, which delivers it there."},
        ])
    );
    let message = sent["message_id"].as_str().unwrap().to_owned();
    let thread = sent["thread_id"].as_str().unwrap().to_owned();
    let row = |label: &str, recipient: &str| -> Vec<String> {
        [
            label,
            &message,
            recipient,
            REX,
            "session:a",
            "alice",
            &thread,
            "-",
            "hello there",
            "queued",
        ]
        .map(str::to_owned)
        .to_vec()
    };
    assert_eq!(
        space.outbound(),
        [row("devbox", "bob"), row("studio", "carol")]
    );
    let bob = crate::store::RelayAuthor::new(
        crate::store::RelayOrigin::Label("devbox"),
        "bob",
        "bob@devbox".to_owned(),
    );
    assert!(bob.address_id.as_str().starts_with("relay:"));
    assert_eq!(bob.address_id.as_str().len(), "relay:".len() + 16);
    assert_eq!(
        space.main(
            "SELECT r.alias_snapshot, d.state FROM message_recipients r
             JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, message_id, address_id)
             ORDER BY r.recipient_order"
        ),
        [["bob@devbox", "settled"], ["carol@studio", "settled"]]
    );
    assert_eq!(
        space.main(&format!(
            "SELECT name FROM pane_roles WHERE role_id = 'role-relay:{}'",
            &bob.address_id.as_str()["relay:".len()..]
        )),
        [["bob@devbox"]]
    );

    fs::remove_file(space.state.join("relay.sqlite3")).unwrap();
    let replay = tests_support::with_hosts(&["devbox", "studio"], || {
        space.send("request-1", "@bob@devbox @carol@studio hello there")
    })
    .unwrap();
    assert_eq!(replay["message_id"], sent["message_id"]);
    assert_eq!(replay["idempotent_replay"], true);
    assert_eq!(replay["relayed"], sent["relayed"]);
    assert_eq!(space.outbound().len(), 2);
}

#[test]
fn a_rex_send_to_a_local_and_a_remote_recipient_mails_the_local_one() {
    let space = Space::new(REX);
    let sent = tests_support::with_hosts(&["devbox"], || {
        space.send("request-1", "@dan @bob@devbox hi")
    })
    .unwrap();
    assert_eq!(
        space.main(
            "SELECT r.alias_snapshot, d.state FROM message_recipients r
             JOIN mailbox_deliveries d USING (session_socket_id, worktree_id, message_id, address_id)
             ORDER BY r.recipient_order"
        ),
        [["dan", "queued"], ["bob@devbox", "settled"]]
    );
    assert_eq!(space.outbound().len(), 1);
    assert_eq!(space.outbound()[0][1], sent["message_id"].as_str().unwrap());
}

#[test]
fn a_rex_send_to_an_unlisted_host_or_too_large_to_relay_stores_nothing() {
    let space = Space::new(REX);
    let refused = tests_support::with_hosts(&["studio", "devbox"], || {
        space.send("request-1", "@bob@devbx hi")
    })
    .unwrap_err();
    assert_eq!(
        refused,
        "devbx is not in your Rex hosts (rex hosts list): studio, devbox"
    );

    let text = "x".repeat(60 * 1024);
    let refused = tests_support::with_hosts(&["devbox"], || {
        space.send("request-2", &format!("@bob@devbox {text}"))
    })
    .unwrap_err();
    assert!(refused.contains("too large to relay"), "{refused}");
    let quotes = "\"".repeat(30 * 1024);
    let refused = tests_support::with_hosts(&["devbox"], || {
        space.send("request-3", &format!("@bob@devbox {quotes}"))
    })
    .unwrap_err();
    assert!(refused.contains("too large to relay"), "{refused}");
    let fits = "x".repeat(58 * 1024);
    tests_support::with_hosts(&["devbox"], || {
        space.send("request-4", &format!("@bob@devbox {fits}"))
    })
    .unwrap();

    assert_eq!(
        space.main("SELECT request_id FROM collaboration_messages"),
        [["request-4"]]
    );
    assert_eq!(space.outbound().len(), 1);
}

#[test]
fn a_send_outside_rex_never_reads_the_host_list() {
    let space = Space::new("/tmp/namedrop-relay-tests-herdr.sock");
    let rex = tests_support::hosts(&["devbox"]);
    let refused = rex
        .run(|| space.send("request-1", "@bob@devbox hi"))
        .unwrap_err();
    assert_eq!(refused, MISSING);
    assert!(rex.calls().is_empty());
    assert!(space.outbound().is_empty());
}

#[test]
fn no_registration_or_pane_action_takes_a_relay_address_or_role() {
    let space = Space::new(REX);
    let namespace = crate::StateNamespace::for_space(REX, "session:a");
    let attribution = MutationAttribution::new(ActorKind::Human, "test", 2);
    let bob = crate::store::RelayAuthor::new(
        crate::store::RelayOrigin::Label("devbox"),
        "bob",
        "bob@devbox".to_owned(),
    );
    let mut store = StateStore::open(&space.state).unwrap();
    store
        .transaction(|transaction| transaction.ensure_relay_author(&namespace, &bob, &attribution))
        .unwrap();
    for (id, role) in [
        (bob.address_id.as_str(), "role-new"),
        ("relay:0000000000000000", "role-new"),
        ("address-new", bob.role_id.as_str()),
    ] {
        let created = store.transaction(|transaction| {
            transaction.create_pane_address(
                &namespace,
                &PaneAddressDraft {
                    id: PaneAddressId::new(id),
                    role_id: PaneRoleId::new(role),
                    role_name: "new".to_owned(),
                    role_description: "new".to_owned(),
                    alias: "new".to_owned(),
                },
                &attribution,
            )
        });
        assert!(
            created
                .unwrap_err()
                .to_string()
                .contains("a remote agent's identity is reserved"),
            "{id} {role}"
        );
    }
    let bound = store.transaction(|transaction| {
        transaction.bind_pane(
            &namespace,
            &PaneBindingId::new("binding-bob"),
            &bob.address_id,
            "block:b",
            &attribution,
        )
    });
    assert!(
        bound
            .unwrap_err()
            .to_string()
            .contains("a remote agent's identity has no pane lifecycle")
    );
    assert!(
        store
            .transaction(|transaction| transaction.is_relay_author(&namespace, &bob.address_id))
            .unwrap()
    );
}

/// `cut` measures a text as a JSON string encodes it.
#[test]
fn a_text_is_cut_only_past_its_limit_as_encoded() {
    use super::wire::{CUT_NOTICE, cut};
    let encoded = |text: &str| serde_json::to_string(text).unwrap().len() - 2;
    let exact = format!("{}\u{1}\"é", "x".repeat(100));
    let limit = encoded(&exact);
    assert_eq!(limit, 100 + 6 + 2 + 2);
    assert_eq!(cut(&exact, limit), exact, "a text at its limit stays whole");
    let over = cut(&exact, limit - 1);
    assert!(over.ends_with(CUT_NOTICE));
    assert!(encoded(&over) < limit, "{}", encoded(&over));
    assert!(encoded(&over) > limit - 1 - 6, "{}", encoded(&over));
    let wide = "é".repeat(100);
    let cut_wide = cut(&wide, 100);
    assert!(cut_wide.ends_with(CUT_NOTICE));
    assert!(encoded(&cut_wide) <= 100);
}
