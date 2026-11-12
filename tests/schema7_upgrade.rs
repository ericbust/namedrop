//! A real 1.6.0 state database upgraded by this build: nothing is lost, and the
//! delivery 1.6.0 left open still authenticates from the bytes 1.6.0 typed.
#[path = "support/cleanup.rs"]
mod cleanup;
#[path = "support/delivered.rs"]
mod delivered;
#[path = "support/strict_space.rs"]
mod strict_space;

use namedrop::{
    AgentConfigDirectories, Clock, IdGenerator, LifecycleRuntime, StateStore,
    execute_user_prompt_submit_hook,
    mux::fake::{FakeMux, SentInput},
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};
use strict_space::{StrictSpace, Takes};

const SOCKET: &str = "/tmp/nd-tb-1000-a7d69c9d70a7c962.sock";
const WORKSPACE: &str = "w1";
const CLAUDE_PANE: &str = "w1:p2";
const CODEX_PANE: &str = "w1:p3";
const CLAUDE_SESSION: &str = "e603eff8-5290-4a97-af5c-fb53c6a9b89e";
const CODEX_SESSION: &str = "01a1bc51-9c88-71e0-a2cb-6d4a6121e0bb";
const HELD_ATTEMPT: &str = "delivery-attempt-1794161504622630621-2120858-3";

static NEXT: AtomicU64 = AtomicU64::new(1);

const GOLDEN_SOCKET: &str = "/tmp/nd-golden.sock";
const GOLDEN_CLOCK: u64 = 1_794_000_705_500;

struct Fixture {
    root: PathBuf,
    state: PathBuf,
    socket: &'static str,
    clock: FixedClock,
    mux: StrictSpace,
    ids: Ids,
}

impl Fixture {
    fn new() -> Self {
        let fixture = Self::at(SOCKET, 1_794_170_000_000, REAL_PANES.to_vec());
        fixture.load(include_str!("fixtures/schema7/state.sql"));
        fixture
    }

    fn at(socket: &'static str, now_ms: u64, panes: Vec<Pane>) -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-schema7-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let state = root.join("state");
        fs::create_dir_all(&state).unwrap();
        Self {
            root,
            state,
            socket,
            clock: FixedClock(now_ms),
            mux: space_of(&panes),
            ids: Ids(AtomicU64::new(1)),
        }
    }

    fn load(&self, sql: &str) {
        let connection = rusqlite::Connection::open(self.state.join("namedrop.sqlite3")).unwrap();
        connection.execute_batch(sql).unwrap();
        connection.pragma_update(None, "user_version", 7).unwrap();
    }

    fn runtime<'a>(&'a self, pane: &'a str) -> LifecycleRuntime<'a, FakeMux, FixedClock, Ids> {
        LifecycleRuntime {
            state_directory: &self.state,
            home_directory: Some(&self.root),
            agent_config: AgentConfigDirectories::default(),
            cwd: &self.root,
            workspace_id: Some(WORKSPACE),
            current_pane_id: Some(pane),
            session_socket_id: self.socket,
            mux: &self.mux,
            clock: &self.clock,
            ids: &self.ids,
        }
    }

    fn hook(&self, pane: &str, session: &str, prompt_id: &str, prompt: &str) -> String {
        let input = json!({"hook_event_name":"UserPromptSubmit","prompt":prompt,
                           "session_id":session,"prompt_id":prompt_id});
        execute_user_prompt_submit_hook(&input.to_string(), &self.runtime(pane))
            .map(|output| output["hookSpecificOutput"]["additionalContext"].to_string())
            .unwrap_or_default()
    }

    /// The prompt as 1.6.0 typed it, with this fixture's paths back in place.
    fn typed_by_1_6_0(&self) -> String {
        let executable = namedrop::execute_integration_operation(
            "delivery.inbox",
            json!({}),
            &self.runtime(CLAUDE_PANE),
        )
        .unwrap()["envelopes"][0]["control"]["tool"]["executable"]
            .as_str()
            .unwrap()
            .to_owned();
        include_str!("fixtures/schema7/typed-envelope.txt")
            .replace("/STATE/runtime/namedrop", &executable)
            .replace("/STATE", self.state.to_str().unwrap())
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        cleanup::remove_fixture(&self.root).unwrap();
    }
}

/// `(pane, session)`, every one an agent; only the session has to match.
type Pane = (&'static str, &'static str);

const REAL_PANES: [Pane; 2] = [(CLAUDE_PANE, CLAUDE_SESSION), (CODEX_PANE, CODEX_SESSION)];

/// Space `w1` with checkout `/repo`, where every pane runs Claude Code, sits
/// idle, and stays idle after a prompt, as the 1.6.0 testbed did.
fn space_of(panes: &[Pane]) -> StrictSpace {
    let mut world = FakeMux::world().space(WORKSPACE).checkout("/repo");
    for (at, (pane, session)) in panes.iter().enumerate() {
        world = world
            .pane(pane)
            .terminal(&format!("t{at}"))
            .tab(&format!("w1:t{at}"))
            .claude()
            .session(session)
            .idle();
    }
    StrictSpace::new(
        world.build().prompts_stay_settled(),
        Takes::PromptsAndMetadata,
    )
}

struct FixedClock(u64);
impl Clock for FixedClock {
    fn now_unix_ms(&self) -> u64 {
        self.0
    }
    fn wait_ms(&self, _: u64) {}
}

struct Ids(AtomicU64);
impl IdGenerator for Ids {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-after-{}", self.0.fetch_add(1, Ordering::Relaxed))
    }
}

type Tables = BTreeMap<String, Vec<Vec<String>>>;

/// Every table's rows, reading only the columns `columns` names for it.
fn tables(path: &Path, columns: &BTreeMap<String, Vec<String>>) -> Tables {
    let connection = rusqlite::Connection::open(path).unwrap();
    columns
        .iter()
        .map(|(table, names)| {
            let list = names
                .iter()
                .map(|name| format!("CAST(\"{name}\" AS TEXT)"))
                .collect::<Vec<_>>()
                .join(",");
            let mut statement = connection
                .prepare(&format!("SELECT {list} FROM \"{table}\" ORDER BY rowid"))
                .unwrap();
            let rows = statement
                .query_map([], |row| {
                    (0..names.len())
                        .map(|at| Ok(row.get::<_, Option<String>>(at)?.unwrap_or_default()))
                        .collect::<Result<Vec<_>, _>>()
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            (table.clone(), rows)
        })
        .collect()
}

fn columns(path: &Path) -> BTreeMap<String, Vec<String>> {
    let connection = rusqlite::Connection::open(path).unwrap();
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap();
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    names
        .into_iter()
        .map(|table| {
            let mut info = connection
                .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .unwrap();
            let columns = info
                .query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            (table, columns)
        })
        .collect()
}

#[test]
fn a_real_1_6_0_state_directory_upgrades_without_losing_a_row() {
    let fixture = Fixture::new();
    let database = fixture.state.join("namedrop.sqlite3");
    let before_columns = columns(&database);
    let before = tables(&database, &before_columns);
    assert!(before["collaboration_messages"].len() >= 15);

    let store = StateStore::open(&fixture.state).unwrap();
    assert_eq!(store.schema_version().unwrap(), 8);
    drop(store);

    assert_eq!(tables(&database, &before_columns), before);
    let connection = rusqlite::Connection::open(&database).unwrap();
    let (attempts, pointed): (i64, i64) = connection
        .query_row(
            "SELECT count(*), count(pointer_nonce) FROM delivery_attempt_state",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert!(attempts > 0);
    assert_eq!(pointed, 0, "an attempt from 1.6.0 gained a pointer");
}

#[test]
fn the_delivery_1_6_0_left_open_authenticates_from_the_bytes_it_typed() {
    let fixture = Fixture::new();
    let inbox = namedrop::execute_integration_operation(
        "delivery.inbox",
        json!({}),
        &fixture.runtime(CLAUDE_PANE),
    )
    .unwrap();
    assert_eq!(inbox["envelopes"][0]["control"]["attempt_id"], HELD_ATTEMPT);

    let context = fixture.hook(
        CLAUDE_PANE,
        CLAUDE_SESSION,
        "after-upgrade",
        &fixture.typed_by_1_6_0(),
    );
    assert!(
        context.contains("Namedrop verification:"),
        "the prompt 1.6.0 typed no longer authenticates: {context}"
    );
    assert!(!context.contains("Namedrop delivery details"), "{context}");
    assert!(context.contains("Namedrop commands:"), "{context}");
    let again = fixture.hook(
        CLAUDE_PANE,
        CLAUDE_SESSION,
        "after-upgrade-2",
        &fixture.typed_by_1_6_0(),
    );
    assert!(again.contains("Namedrop verification:"), "{again}");
}

#[test]
fn after_the_upgrade_a_new_delivery_is_a_pointer_to_the_same_sessions() {
    let fixture = Fixture::new();
    let runtime = fixture.runtime(CODEX_PANE);
    namedrop::execute_message_operation(
        "send",
        json!({"request_id":"after-upgrade","source":"@claude Reply with AFTER.",
               "author_kind":"agent_session"}),
        &runtime,
    )
    .unwrap();
    namedrop::drain_mailboxes(&runtime).unwrap();
    let typed = fixture
        .mux
        .sent_to(CLAUDE_PANE)
        .into_iter()
        .rev()
        .find_map(|sent| match sent.input {
            SentInput::Prompt(prompt) => Some(prompt),
            _ => None,
        })
        .unwrap_or_else(|| panic!("nothing was typed"));
    assert!(typed.starts_with("Namedrop message "), "{typed}");
    let envelope: Value = delivered::envelope_of(&typed);
    assert_eq!(envelope["untrusted_body"], "Reply with AFTER.");
    let context = fixture.hook(CLAUDE_PANE, CLAUDE_SESSION, "new-turn", &typed);
    assert!(context.contains("Namedrop verification:"), "{context}");
    assert!(context.contains("Namedrop delivery details"), "{context}");
}

/// What 1.6.0 typed for each kind of delivery, from `generate_goldens.rs` at
/// caf657d. Authentication is byte equality, so any change to an envelope's
/// instruction, control or tool fails here, under the kind it broke.
#[test]
fn every_kind_of_delivery_1_6_0_typed_still_authenticates_unchanged() {
    const GOLDEN_PANES: [Pane; 3] = [
        ("w1:p1", "session-lead"),
        ("w1:p2", "session-reviewer"),
        ("w1:p3", "session-researcher"),
    ];
    const CHANNEL_SOCKET: &str = "/tmp/nd-tb-1000-580f53432796f20f.sock";
    const CHANNEL_PANES: [Pane; 1] = [("w1:p2", "fede1ffd-6bc2-4287-b070-894529caa1cd")];
    macro_rules! golden {
        ($kind:literal, $socket:expr, $panes:expr, $pane:literal, $session:literal) => {
            (
                $kind,
                $socket,
                $panes,
                include_str!(concat!("fixtures/schema7/golden/", $kind, ".sql")),
                include_str!(concat!("fixtures/schema7/golden/", $kind, ".prompt")),
                $pane,
                $session,
            )
        };
        ($kind:literal, $pane:literal, $session:literal) => {
            golden!($kind, GOLDEN_SOCKET, &GOLDEN_PANES[..], $pane, $session)
        };
    }
    for (kind, socket, panes, data, prompt, pane, session) in [
        golden!("agent-ask", "w1:p2", "session-reviewer"),
        golden!("person-mention", "w1:p2", "session-reviewer"),
        golden!("reply-to-source", "w1:p1", "session-lead"),
        golden!("result-set", "w1:p1", "session-lead"),
        golden!("tracked-assignment", "w1:p3", "session-researcher"),
        golden!("nudge", "w1:p1", "session-lead"),
        golden!("named-recap", "w1:p1", "session-lead"),
        golden!(
            "channel-recap",
            CHANNEL_SOCKET,
            &CHANNEL_PANES[..],
            "w1:p2",
            "fede1ffd-6bc2-4287-b070-894529caa1cd"
        ),
    ] {
        let fixture = Fixture::at(socket, GOLDEN_CLOCK, panes.to_vec());
        let state = fixture.state.to_str().unwrap();
        assert!(!data.contains("/EXE"), "{kind}");
        fixture.load(&format!(
            "{}\n{}",
            include_str!("fixtures/schema7/golden/schema.sql"),
            data.replace("/STATE", state)
        ));
        let outstanding = namedrop::execute_integration_operation(
            "delivery.inbox",
            json!({}),
            &fixture.runtime(pane),
        )
        .unwrap()["envelopes"][0]["control"]
            .clone();
        let executable = outstanding["tool"]["executable"]
            .as_str()
            .unwrap_or_else(|| panic!("{kind}: nothing is outstanding"));
        let mut typed = prompt.replace("/EXE", executable).replace("/STATE", state);
        if kind == "channel-recap" {
            // Since 1.6.1 an agent's final message counts as a reply, so the
            // recap rebuilt for this 1.6.0 post also holds Codex's answer, on
            // main as here. Every other byte must still match.
            let recap = |text: &Value| serde_json::to_string(text).unwrap();
            let typed_recap = recap(&json!(
                "person: Quick check: could you, @codex, reply with the single word GOLDENSEED?"
            ));
            assert!(typed.contains(&typed_recap), "{kind}: {typed}");
            typed = typed.replace(
                &typed_recap,
                &recap(&outstanding["untrusted_channel_recap"]),
            );
        }
        let context = fixture.hook(pane, session, "after-upgrade", &typed);
        assert!(
            context.contains("Namedrop verification:"),
            "{kind}: the prompt its release typed no longer authenticates: {context}"
        );
        assert!(context.contains("Namedrop commands:"), "{kind}: {context}");
        assert!(
            !context.contains("Namedrop delivery details"),
            "{kind}: {context}"
        );
    }
}
