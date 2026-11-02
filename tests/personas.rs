//! Persona persistence, lifecycle, and validation, exercised through the public
//! lifecycle operations and the CLI the user actually types.
#[path = "support/cleanup.rs"]
mod cleanup;

use namedrop::{
    Clock, HerdrCommandExecutor, HerdrCommandOutput, LifecycleRuntime, ProcessIds, StateNamespace,
    StateStore, execute_lifecycle_operation,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "namedrop-personas-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        cleanup::remove_fixture(&self.0).unwrap();
    }
}

/// One agent pane and one shell pane, so a persona can be refused for a named
/// terminal and an agent name can be rebound without leaving the space.
#[derive(Clone, Copy)]
struct Host;
impl HerdrCommandExecutor for Host {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let value = match args {
            ["workspace", "list"] => {
                json!({"result":{"workspaces":[{"workspace_id":"w1"}]}})
            }
            ["worktree", "list", ..] => {
                json!({"result":{"worktrees":[{"path":"/repo","open_workspace_id":"w1"}]}})
            }
            ["pane", "list", ..] => json!({"result":{"panes":[
                {"pane_id":"p1","terminal_id":"t1","tab_id":"tab1","workspace_id":"w1",
                 "agent":"codex","agent_session":{"value":"s1"},"agent_status":"idle"},
                {"pane_id":"p2","terminal_id":"t2","tab_id":"tab2","workspace_id":"w1",
                 "agent_status":"unknown"}
            ]}}),
            ["tab", "get", ..] => json!({"result":{"label":"Main"}}),
            ["pane", "report-metadata", ..] | ["tab", "rename", ..] => json!({}),
            _ => return Err(format!("unexpected command: {args:?}")),
        };
        Ok(HerdrCommandOutput {
            stdout: value.to_string(),
        })
    }
}

#[derive(Clone, Copy)]
struct Time;
impl Clock for Time {
    fn now_unix_ms(&self) -> u64 {
        1_000
    }
    fn wait_ms(&self, _: u64) {}
}

fn namespace() -> StateNamespace {
    StateNamespace::for_space("socket", "w1")
}

/// Reads persona back out of committed state rather than an operation's
/// response, which is what surviving a restart actually means.
fn saved_persona(state: &Path, address_id: &str) -> Option<String> {
    StateStore::open(state)
        .unwrap()
        .transaction(|transaction| transaction.list_pane_addresses(&namespace()))
        .unwrap()
        .into_iter()
        .find(|address| address.id.as_str() == address_id)
        .expect("the named address is not active")
        .persona
}

#[test]
fn a_persona_survives_renaming_rebinding_and_restart_until_it_is_cleared() {
    let directory = TestDirectory::new();
    let ids = ProcessIds::default();
    let agent = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: None,
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: Some("w1"),
        current_pane_id: Some("p1"),
        session_socket_id: "socket",
        herdr: &Host,
        clock: &Time,
        ids: &ids,
    };
    let run = |operation: &str, input| {
        execute_lifecycle_operation(operation, input, &agent)
            .unwrap_or_else(|error| panic!("{operation} failed: {error}"))
    };

    run(
        "name",
        json!({"alias":"reviewer","pane_type":"agent",
               "persona":"  Reviews Rust changes for correctness.  "}),
    );
    let listed: Value = run("address.list", json!({}));
    let address_id = listed[0]["id"].as_str().unwrap().to_owned();
    assert_eq!(
        saved_persona(directory.path(), &address_id).as_deref(),
        Some("Reviews Rust changes for correctness."),
        "a persona supplied while naming was not saved trimmed"
    );

    // Omitted and explicitly null input both preserve what the user saved.
    run("name", json!({"alias":"reviewer","pane_type":"agent"}));
    run(
        "name",
        json!({"alias":"reviewer","pane_type":"agent","persona":null}),
    );
    run(
        "address.rename",
        json!({"address_id":address_id,"alias":"auditor"}),
    );
    run(
        "address.bind",
        json!({"address_id":address_id,"pane_id":"p2"}),
    );
    run(
        "address.bind",
        json!({"address_id":address_id,"pane_id":"p1"}),
    );
    assert_eq!(
        saved_persona(directory.path(), &address_id).as_deref(),
        Some("Reviews Rust changes for correctness."),
        "naming again, renaming, or rebinding dropped the saved persona"
    );

    // Editing replaces it; blank text is how Generic is chosen, and stays chosen.
    let edited = run(
        "address.persona",
        json!({"address_id":address_id,"description":"Audits dependency licences."}),
    );
    assert_eq!(edited["persona"], "Audits dependency licences.");
    run(
        "address.persona",
        json!({"address_id":address_id,"description":"   "}),
    );
    run("name", json!({"alias":"auditor","pane_type":"agent"}));
    assert_eq!(
        saved_persona(directory.path(), &address_id),
        None,
        "a cleared persona came back"
    );
}

#[test]
fn persona_input_is_bounded_control_safe_and_never_clears_by_accident() {
    let directory = TestDirectory::new();
    let ids = ProcessIds::default();
    let pane = |pane_id| LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: None,
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: Some("w1"),
        current_pane_id: Some(pane_id),
        session_socket_id: "socket",
        herdr: &Host,
        clock: &Time,
        ids: &ids,
    };
    let (agent, terminal) = (pane("p1"), pane("p2"));
    execute_lifecycle_operation(
        "name",
        json!({"alias":"reviewer","pane_type":"agent"}),
        &agent,
    )
    .unwrap();
    execute_lifecycle_operation(
        "name",
        json!({"alias":"logs","pane_type":"terminal"}),
        &terminal,
    )
    .unwrap();

    let listed: Value = execute_lifecycle_operation("address.list", json!({}), &agent).unwrap();
    let address_of = |alias: &str| {
        listed
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["alias"] == alias)
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let agent_id = address_of("reviewer");
    let terminal_id = address_of("logs");
    let persona = |input| execute_lifecycle_operation("address.persona", input, &agent);

    // A forgotten description is a rejected edit, not an accidental clear.
    assert!(
        persona(json!({"address_id":agent_id}))
            .unwrap_err()
            .contains("description is required"),
        "an omitted description was accepted"
    );
    // Null and blank are the deliberate clear, and are accepted.
    assert_eq!(
        persona(json!({"address_id":agent_id,"description":null})).unwrap()["persona"],
        Value::Null
    );

    let longest = "a".repeat(namedrop::MAX_PERSONA_CHARS);
    assert_eq!(
        persona(json!({"address_id":agent_id,"description":longest})).unwrap()["persona"],
        Value::String(longest.clone())
    );
    assert!(
        persona(json!({"address_id":agent_id,"description":format!("{longest}a")}))
            .unwrap_err()
            .contains("cannot exceed"),
        "an oversized persona was accepted"
    );
    assert!(
        persona(json!({"address_id":agent_id,"description":"Reviews\u{1b}[2J code"}))
            .unwrap_err()
            .contains("control characters"),
        "a persona that can repaint a terminal was accepted"
    );
    // Invisible formatting can reorder or hide roster text; emoji joiners are fine.
    for hidden in [
        "Reviews \u{202E}tsuR code",
        "Reviews\u{200B} code",
        "\u{FEFF}Reviews",
        "Reviews\u{E0041}",
    ] {
        assert!(
            persona(json!({"address_id":agent_id,"description":hidden}))
                .unwrap_err()
                .contains("invisible formatting"),
            "a persona with invisible formatting was accepted: {hidden:?}"
        );
    }
    // A rejected edit leaves the previously saved persona alone.
    assert_eq!(
        saved_persona(directory.path(), &agent_id).as_deref(),
        Some(longest.as_str())
    );
    let emoji = "Pairs on UI work 👩\u{200D}💻";
    assert_eq!(
        persona(json!({"address_id":agent_id,"description":emoji})).unwrap()["persona"],
        Value::String(emoji.to_owned())
    );

    // Personas describe agents; a named terminal cannot hold one either way in.
    assert!(
        persona(json!({"address_id":terminal_id,"description":"Runs the build."}))
            .unwrap_err()
            .contains("terminal"),
        "a named terminal accepted a persona"
    );
    assert!(
        execute_lifecycle_operation(
            "name",
            json!({"alias":"logs","pane_type":"terminal","persona":"Runs the build."}),
            &terminal,
        )
        .unwrap_err()
        .contains("terminal"),
        "naming a terminal accepted a persona"
    );
    assert_eq!(saved_persona(directory.path(), &terminal_id), None);
}

#[test]
fn upgrading_keeps_custom_descriptions_and_starts_native_names_generic() {
    let directory = TestDirectory::new();
    let ids = ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: None,
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: Path::new("/repo"),
        workspace_id: Some("w1"),
        current_pane_id: Some("p1"),
        session_socket_id: "socket",
        herdr: &Host,
        clock: &Time,
        ids: &ids,
    };
    execute_lifecycle_operation(
        "name",
        json!({"alias":"reviewer","pane_type":"agent"}),
        &runtime,
    )
    .unwrap();
    let custom: Value = execute_lifecycle_operation(
        "address.create",
        json!({"role_name":"Planner","role_description":"Turns goals into ordered plans.",
               "alias":"planner","pane_id":"p2"}),
        &runtime,
    )
    .unwrap();
    let custom_id = custom["id"].as_str().unwrap().to_owned();
    execute_lifecycle_operation("address.unbind", json!({"address_id":custom_id}), &runtime)
        .unwrap();

    for alias in ["empty", "blank"] {
        let legacy = execute_lifecycle_operation(
            "address.create",
            json!({"role_name":alias,"role_description":"Legacy description.","alias":alias,"pane_id":"p2"}),
            &runtime,
        )
        .unwrap();
        execute_lifecycle_operation(
            "address.unbind",
            json!({"address_id":legacy["id"]}),
            &runtime,
        )
        .unwrap();
    }

    // Reproduce an installed database from before personas existed.
    let database = rusqlite::Connection::open(directory.path().join("namedrop.sqlite3")).unwrap();
    for (alias, description) in [("empty", ""), ("blank", "   ")] {
        database
            .execute(
                "UPDATE pane_roles SET description = ?1 WHERE name = ?2",
                [description, alias],
            )
            .unwrap();
    }
    database
        .execute_batch("DROP VIEW delivery_seats; DROP TABLE bridged_threads; DROP TABLE bridged_messages; DROP TABLE space_envoys; ALTER TABLE pane_addresses DROP COLUMN remote_commands_terminal_id; ALTER TABLE pane_addresses DROP COLUMN persona; PRAGMA user_version=4;")
        .unwrap();
    drop(database);

    let upgraded: Value = execute_lifecycle_operation("address.list", json!({}), &runtime).unwrap();
    let persona_of = |alias: &str| {
        upgraded
            .as_array()
            .unwrap()
            .iter()
            .find(|record| record["alias"] == alias)
            .unwrap()["persona"]
            .clone()
    };
    assert_eq!(
        persona_of("planner"),
        Value::String("Turns goals into ordered plans.".to_owned()),
        "upgrading discarded a description the user wrote"
    );
    assert_eq!(
        persona_of("reviewer"),
        Value::Null,
        "an auto-registered native name did not start Generic"
    );
    for alias in ["empty", "blank"] {
        assert_eq!(
            persona_of(alias),
            Value::Null,
            "blank legacy text must be Generic"
        );
    }

    // Choosing Generic afterwards is durable: reopening never re-derives it.
    execute_lifecycle_operation(
        "address.persona",
        json!({"address_id":custom_id,"description":""}),
        &runtime,
    )
    .unwrap();
    assert_eq!(saved_persona(directory.path(), &custom_id), None);
}

#[test]
fn the_name_command_takes_one_persona_or_generic() {
    use clap::Parser;
    use namedrop::cli::Cli;

    assert_eq!(
        Cli::try_parse_from([
            "namedrop",
            "name",
            "@reviewer",
            "--persona",
            "Reviews Rust."
        ])
        .unwrap()
        .name_persona(),
        Some("Reviews Rust.")
    );
    assert_eq!(
        Cli::try_parse_from(["namedrop", "name", "@reviewer", "--generic"])
            .unwrap()
            .name_persona(),
        Some(""),
        "--generic must clear rather than preserve"
    );
    assert_eq!(
        Cli::try_parse_from(["namedrop", "name", "@reviewer"])
            .unwrap()
            .name_persona(),
        None,
        "naming without either flag must preserve the saved persona"
    );
    assert!(
        Cli::try_parse_from([
            "namedrop",
            "name",
            "@reviewer",
            "--persona",
            "Reviews Rust.",
            "--generic"
        ])
        .is_err(),
        "--persona and --generic must be exclusive"
    );
}
