use super::*;

#[test]
fn plain_post_routes_verbatim_and_replay_does_not_call_router_again() {
    let f = fixture();
    let server = Server::start(
        &f,
        r#"{"result":"{\"to\":[\"planner\",\"ghost\",\"lead\",\"planner\"],\"reason\":\"needs a plan\"}","usage":{"input_tokens":123,"output_tokens":12}}"#,
        false,
    );
    let source = "Can @planner and @lead plan this?\nKeep @ghost literal.";
    let (status, receipt) = post(&server, "plain", source, None);
    assert_eq!(status, 200, "{receipt}");
    assert_eq!(receipt["channel"]["state"], "routed");
    assert_eq!(receipt["channel"]["to"], json!(["planner", "lead"]));
    assert_eq!(receipt["channel"]["reason"], "needs a plan");
    assert!(receipt.get("_channel_routing_note").is_none());
    assert_eq!(attempt_records(&f)[0]["input_tokens"], 123);
    let (status, replay) = post(&server, "plain", source, None);
    assert_eq!(status, 200, "{replay}");
    assert_eq!(replay["message_id"], receipt["message_id"]);
    assert_eq!(replay["channel"], receipt["channel"]);
    assert_eq!(calls(&f).len(), 1);
    let stored: (String, String, String) = f.db().query_row("SELECT authored_source,body,grammar_version FROM collaboration_messages WHERE message_id=?1", [receipt["message_id"].as_str().unwrap()], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap();
    assert_eq!(stored, (source.into(), source.into(), "channel/v1".into()));
    let messages = history(&server);
    assert_eq!(messages[0]["channel"], receipt["channel"]);
    let version: u32 = f
        .db()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 6);
    let call = &calls(&f)[0];
    assert!(
        call["env"]
            .as_object()
            .unwrap()
            .keys()
            .all(|key| !key.starts_with("HERDR_") && !key.starts_with("NAMEDROP_"))
    );
    assert!(call["env"].get("CLAUDE_CODE_SESSION_ID").is_none());
    let args = call["args"].as_array().unwrap();
    assert!(!args.iter().any(|arg| arg == "--json-schema"));
    let prompt_index = args
        .iter()
        .position(|arg| arg == "--system-prompt")
        .unwrap()
        + 1;
    let (_, schema) = args[prompt_index]
        .as_str()
        .unwrap()
        .split_once("\nOutput JSON schema: ")
        .expect("plain Claude receives the schema in its trusted system prompt");
    let schema: Value = serde_json::from_str(schema).unwrap();
    let mut aliases = schema["properties"]["to"]["items"]["enum"]
        .as_array()
        .unwrap()
        .clone();
    aliases.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    assert_eq!(aliases, vec![json!("lead"), json!("planner")]);
    assert_eq!(schema["additionalProperties"], false);
    assert!(
        schema["properties"]["to"].get("uniqueItems").is_none(),
        "Codex strict output schemas reject uniqueItems; server validation deduplicates picks"
    );
}

#[test]
fn failed_routing_attempt_keeps_provider_receipt_after_post_commit() {
    let f = fixture();
    let server = Server::start(
        &f,
        r#"{"result":"not json","num_turns":2,"usage":{"input_tokens":137,"output_tokens":19},"total_cost_usd":0.0012}"#,
        false,
    );
    let (status, receipt) = post(
        &server,
        "failed-audit",
        "Please ask @planner to inspect this.",
        None,
    );
    assert_eq!(status, 200, "{receipt}");
    assert_eq!(receipt["channel"]["state"], "router_failed");
    let journal = fs::read_to_string(f.state.join("dashboard/channel-router-attempts.jsonl"))
        .expect("every actual routing call must leave an independent cost receipt");
    let records: Vec<Value> = journal
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["request_id"], "failed-audit");
    assert_eq!(records[0]["host"], "claude");
    assert_eq!(records[0]["model"], "claude-haiku-4-5");
    assert_eq!(records[0]["outcome"], "invalid_response");
    assert_eq!(records[0]["num_turns"], 2);
    assert_eq!(records[0]["input_tokens"], 137);
    assert_eq!(records[0]["output_tokens"], 19);
    assert_eq!(records[0]["provider_usd"], 0.0012);
    assert_eq!(records[0]["usage_complete"], true);
    assert!(records[0]["duration_ms"].is_u64());
    assert!(!journal.contains("not json"));
    assert!(!journal.contains("Please ask"));
    post(
        &server,
        "failed-audit",
        "Please ask @planner to inspect this.",
        None,
    );
    assert_eq!(
        fs::read_to_string(f.state.join("dashboard/channel-router-attempts.jsonl")).unwrap(),
        journal
    );
}

pub(super) fn attempt_records(f: &Fixture) -> Vec<Value> {
    fs::read_to_string(f.state.join("dashboard/channel-router-attempts.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn concurrent_replays_audit_one_call_and_commit_one_post() {
    let f = fixture();
    let server = Server::start(&f, "delayed", false);
    thread::scope(|scope| {
        let first = scope.spawn(|| {
            post(
                &server,
                "audit-race",
                "Please ask @planner to plan this.",
                None,
            )
        });
        let second = scope.spawn(|| {
            post(
                &server,
                "audit-race",
                "Please ask @planner to plan this.",
                None,
            )
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while calls(&f).is_empty() {
            assert!(
                Instant::now() < deadline,
                "one actual worker must start for the committed post"
            );
            thread::sleep(Duration::from_millis(10));
        }
        fs::write(f.root.join("router-release"), "continue").unwrap();
        let (first_status, first) = first.join().unwrap();
        let (second_status, second) = second.join().unwrap();
        assert_eq!(first_status, 200, "{first}");
        assert_eq!(second_status, 200, "{second}");
        assert_eq!(first["message_id"], second["message_id"]);
    });
    let records = attempt_records(&f);
    assert_eq!(records.len(), 1);
    assert_eq!(calls(&f).len(), 1);
    assert!(records.iter().all(|record| record["outcome"] == "routed"));
    assert_eq!(history(&server).as_array().unwrap().len(), 1);
    assert_eq!(
        fs::read_to_string(f.state.join("dashboard/channel-routing.jsonl"))
            .unwrap()
            .lines()
            .count(),
        1
    );
}

#[test]
fn failed_initial_commit_never_launches_a_model_or_records_cost() {
    let f = fixture();
    let server = Server::start(&f, "delayed", false);
    f.db().execute_batch("CREATE TRIGGER reject_audit_post BEFORE INSERT ON collaboration_messages BEGIN SELECT RAISE(ABORT, 'synthetic commit failure'); END;").unwrap();
    let (status, _) = support::raw_post(
        &server,
        "audit-rollback",
        "Please ask @planner to plan this.",
        None,
    );
    assert_ne!(status, 200);
    assert!(calls(&f).is_empty());
    assert!(attempt_records(&f).is_empty());
    assert!(history(&server).as_array().unwrap().is_empty());
    assert!(!f.state.join("dashboard/channel-routing.jsonl").exists());
}
