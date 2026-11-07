#[path = "support/cleanup.rs"]
mod cleanup;

#[test]
fn populated_schema_six_histories_migrate_through_the_cli_without_losing_values() {
    let output = std::process::Command::new("python3")
        .env("NAMEDROP_TEST_BINARY", env!("CARGO_BIN_EXE_namedrop"))
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/schema6_migration.py"
        ))
        .output()
        .expect("Python 3 is required for schema 6 migration verification");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn migrated_experimental_store_opens_again_while_the_first_handle_is_alive() {
    let state = std::env::temp_dir().join(format!("namedrop-schema6-api-{}", std::process::id()));
    let _cleanup = cleanup::FixtureDirectory(state.clone());
    std::fs::create_dir_all(&state).unwrap();
    let database = rusqlite::Connection::open(state.join("namedrop.sqlite3")).unwrap();
    database.pragma_update(None, "foreign_keys", true).unwrap();
    for sql in [
        include_str!("fixtures/schema6/schema.sql"),
        include_str!("fixtures/schema6/common.sql"),
        include_str!("fixtures/schema6/experimental.sql"),
    ] {
        database.execute_batch(sql).unwrap();
    }
    let parent = namedrop::subagent::ParentIdentity::new(
        "test-schema-socket",
        "home",
        "worker-pane",
        "worker-session",
    )
    .unwrap();
    namedrop::subagent::record(
        &state,
        &parent,
        namedrop::subagent::Event::Start {
            id: "running-child".into(),
            generation: None,
            kind: Some("explorer".into()),
            description: Some("Keep this child".into()),
        },
        214,
    )
    .unwrap();
    let first = namedrop::StateStore::open(&state).unwrap();
    assert_eq!(first.schema_version().unwrap(), 7);
    let second = namedrop::StateStore::open(&state).unwrap();
    assert_eq!(second.schema_version().unwrap(), 7);
    drop((first, second));
    assert_eq!(
        namedrop::StateStore::open(&state)
            .unwrap()
            .schema_version()
            .unwrap(),
        7
    );
    assert_eq!(
        database.query_row(
            "SELECT body FROM collaboration_messages WHERE message_id='terminal-relay:dm-attempt'",
            [], |row| row.get::<_, String>(0)
        ).unwrap(),
        "Keep this terminal answer"
    );
    let children = namedrop::subagent::running(&state, &parent, 220, None);
    assert_eq!(children.len(), 1);
    assert_eq!(children[0].id, "running-child");
    assert_eq!(children[0].description.as_deref(), Some("Keep this child"));
}
