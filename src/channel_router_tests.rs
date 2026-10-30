use super::*;
use crate::{
    PaneType, SpaceId, StateNamespace,
    store::dashboard::{AddressRow, MessageRow, RegistrationRow},
};
use std::{fs, path::Path, process::Command};

pub(super) fn rows() -> DashboardRows {
    let addresses = [
        ("claude", PaneType::Agent, false),
        ("codex", PaneType::Agent, false),
        ("terminal", PaneType::Terminal, false),
        ("retired", PaneType::Agent, true),
        ("operator", PaneType::Agent, false),
        ("unsupported", PaneType::Agent, false),
        ("unregistered", PaneType::Agent, false),
    ]
    .into_iter()
    .map(|(alias, pane_type, retired)| AddressRow {
        address_id: if alias == "operator" {
            "operator:local".to_owned()
        } else {
            alias.to_owned()
        },
        alias: alias.to_owned(),
        role_name: alias.to_owned(),
        persona: Some("Build and review".to_owned()),
        pane_type,
        retired,
        closed_at_ms: None,
        bound: true,
        occupied: true,
        agent_status: None,
        blocked_since_ms: None,
        blocked_message_id: None,
        blocked_conversation: None,
    })
    .collect();
    let registrations = [
        ("claude", "claude-code"),
        ("codex", "codex"),
        ("terminal", "claude-code"),
        ("retired", "claude-code"),
        ("operator:local", "claude-code"),
        ("unsupported", "other"),
    ]
    .into_iter()
    .map(|(address_id, runtime)| RegistrationRow {
        address_id: address_id.to_owned(),
        runtime: runtime.to_owned(),
    })
    .collect();
    DashboardRows {
        namespace: StateNamespace::new("synthetic", SpaceId::new("w-router")),
        worktree: None,
        addresses,
        registrations,
        messages: vec![],
        legs: vec![],
        mailbox: vec![],
        threads: vec![],
        coordinations: vec![],
        assignments: vec![],
        terminal_runs: vec![],
        attention: vec![],
    }
}

pub(super) fn candidates(rows: &DashboardRows) -> Vec<RoutingAddressSnapshot> {
    rows.addresses
        .iter()
        .filter(|address| {
            matches!(address.alias.as_str(), "claude" | "codex")
                || address.alias.starts_with("member-")
        })
        .map(|address| RoutingAddressSnapshot {
            address_id: crate::PaneAddressId::new(&address.address_id),
            alias: address.alias.clone(),
            pane_binding_id: None,
            agent_session_id: None,
        })
        .collect()
}

fn test_input(rows: &DashboardRows, source: &str) -> RouterInput {
    build_input(RouterRequest {
        host: Some(AgentRuntime::ClaudeCode),
        rows,
        source,
        candidates: &candidates(rows),
    })
    .unwrap()
}

#[test]
fn input_uses_explicit_host_and_only_supplied_candidates() {
    let rows = rows();
    let members = candidates(&rows);
    let input = build_input(RouterRequest {
        host: Some(AgentRuntime::Codex),
        rows: &rows,
        source: "ask @claude",
        candidates: &members[..1],
    })
    .unwrap();
    assert_eq!(input.host, "codex");
    assert_eq!(input.aliases, ["claude"]);
    let schema: Value = serde_json::from_str(&input.schema).unwrap();
    assert_eq!(
        schema["properties"]["to"]["items"]["enum"],
        json!(["claude"])
    );
    let (to, _) = validate_response(
        &json!({"to":["codex","claude"],"reason":"request"}),
        &input.aliases,
    )
    .unwrap();
    assert_eq!(to, ["claude"]);
    assert!(
        build_input(RouterRequest {
            host: None,
            rows: &rows,
            source: "request",
            candidates: &members
        })
        .is_err()
    );
    assert!(matches!(
        route(RouterRequest {
            host: Some(AgentRuntime::ClaudeCode),
            rows: &rows,
            source: "no mention",
            candidates: &[]
        })
        .decision,
        RoutingDecision::Unrouted
    ));
}

#[test]
fn response_caps_topic_words_without_changing_recipients() {
    let input = test_input(&rows(), "ask several");
    let (to, reason) = validate_response(
        &json!({
            "to": ["codex", "ghost", "claude", "codex"],
            "reason": "Parser regression test review request",
        }),
        &input.aliases,
    )
    .unwrap();
    assert_eq!(to, ["codex", "claude"]);
    assert_eq!(reason, "Parser regression test review");
}

#[test]
fn response_only_selects_unique_roster_aliases_and_caps_its_reason() {
    let input = test_input(&rows(), "ask several");
    let (to, reason) = validate_response(
        &json!({
            "to": ["ghost", "claude", "terminal", "retired", "operator", "claude", "codex"],
            "reason": format!("\u{1b}{}\n", "é".repeat(80)),
        }),
        &input.aliases,
    )
    .unwrap();
    assert_eq!(to, ["claude", "codex"]);
    assert_eq!(reason, format!("{} topic", "é".repeat(54)));
    assert_eq!(reason.chars().count(), 60);
    assert!(!reason.chars().any(char::is_control));
    assert!(
        validate_response(
            &json!({"to": [], "reason": "room statement"}),
            &input.aliases
        )
        .unwrap()
        .0
        .is_empty()
    );
    for (invalid, error) in [
        (json!(null), "invalid router response"),
        (json!({"reason":"wrong"}), "missing router picks"),
        (
            json!({"to":"claude","reason":"wrong"}),
            "missing router picks",
        ),
        (
            json!({"to":[null],"reason":"wrong"}),
            "invalid router alias",
        ),
        (json!({"to":[]}), "missing router reason"),
        (json!({"to":[],"reason":null}), "missing router reason"),
        (json!({"to":[],"reason":7}), "missing router reason"),
    ] {
        assert_eq!(
            validate_response(&invalid, &input.aliases).unwrap_err(),
            error
        );
    }
}

#[test]
fn response_normalizes_empty_single_word_unicode_and_control_topics_idempotently() {
    let input = test_input(&rows(), "ask several");
    for (raw, expected) in [
        ("".to_owned(), String::new()),
        (" \n\t ".to_owned(), String::new()),
        ("?!...---".to_owned(), String::new()),
        ("Review".to_owned(), "Review topic".to_owned()),
        ("é".repeat(80), format!("{} topic", "é".repeat(54))),
        (
            format!("{} second", "é".repeat(80)),
            format!("{} topic", "é".repeat(54)),
        ),
        (
            format!("{} second", "é".repeat(59)),
            format!("{} topic", "é".repeat(54)),
        ),
        (
            format!("Review {}", "é".repeat(80)),
            format!("Review {}", "é".repeat(53)),
        ),
        (
            "  Parser\tregression\nTEST\0review!!! request ".to_owned(),
            "Parser regression TEST review".to_owned(),
        ),
        (
            "review-build/test,request followup".to_owned(),
            "review build test request".to_owned(),
        ),
        (
            "Claude review request".to_owned(),
            "Claude review request".to_owned(),
        ),
    ] {
        let response = json!({"to": ["codex", "ghost", "claude", "codex"], "reason": raw});
        let (to, reason) = validate_response(&response, &input.aliases).unwrap();
        assert_eq!(to, ["codex", "claude"]);
        assert_eq!(reason, expected, "raw topic: {raw:?}");
        assert!(reason.chars().count() <= 60);
        assert!(reason.is_empty() || (2..=4).contains(&topic_words(&reason).count()));
        assert!(!reason.chars().any(char::is_control));
        let normalized = json!({"to": to, "reason": reason});
        assert_eq!(
            validate_response(&normalized, &input.aliases).unwrap(),
            (to, reason)
        );
    }
}

#[test]
fn input_caps_post_roster_and_channel_recap_including_unicode_and_controls() {
    let mut rows = rows();
    for index in 0..10 {
        let alias = format!("member-{index}");
        rows.addresses.push(AddressRow {
            address_id: alias.clone(),
            alias: alias.clone(),
            role_name: alias.clone(),
            persona: Some("😀".repeat(1_000)),
            pane_type: PaneType::Agent,
            retired: false,
            closed_at_ms: None,
            bound: true,
            occupied: true,
            agent_status: Some("\n\tworking".repeat(100)),
            blocked_since_ms: None,
            blocked_message_id: None,
            blocked_conversation: None,
        });
        rows.registrations.push(RegistrationRow {
            address_id: alias,
            runtime: "codex".to_owned(),
        });
    }
    for index in 0..10 {
        rows.messages.push(MessageRow {
            channel_to: None,
            channel_state: None,
            channel_mentioned: None,
            woke: Vec::new(),
            message_id: index.to_string(),
            thread_id: "channel-thread".to_owned(),
            in_reply_to: if index == 0 {
                None
            } else {
                Some("0".to_owned())
            },
            created_at_ms: index,
            author_address_id: "claude".to_owned(),
            author_is_human: false,
            grammar_version: if index == 0 {
                "channel/v1"
            } else {
                "mention/v1"
            }
            .to_owned(),
            body: format!("{index} {}", "😀\n\t".repeat(400)),
        });
    }
    rows.messages.push(MessageRow {
        channel_to: None,
        channel_state: None,
        channel_mentioned: None,
        woke: Vec::new(),
        message_id: "unrelated".to_owned(),
        thread_id: "other-thread".to_owned(),
        in_reply_to: None,
        created_at_ms: 11,
        author_address_id: "codex".to_owned(),
        author_is_human: false,
        grammar_version: "mention/v1".to_owned(),
        body: "PRIVATE_NON_CHANNEL_BODY".to_owned(),
    });
    let input = test_input(&rows, &"😀".repeat(100));
    assert!(!input.aliases.is_empty());
    assert!(input.payload.len() + input.schema.len() + INSTRUCTIONS.len() <= MAX_INPUT_BYTES);
    let payload: Value = serde_json::from_str(&input.payload).unwrap();
    assert!(payload["post"].as_str().unwrap().chars().count() <= MAX_POST_CHARS);
    assert!(payload["roster"].as_array().unwrap().len() <= MAX_ROSTER);
    let recap = payload["recap"].as_array().unwrap();
    assert_eq!(recap.len(), 6);
    assert!(recap[0].as_str().unwrap().starts_with("claude: 4 "));
    assert!(
        recap
            .iter()
            .all(|line| line.as_str().unwrap().chars().count() <= 194)
    );
    assert!(
        recap
            .iter()
            .all(|line| !line.as_str().unwrap().chars().any(char::is_control))
    );
    assert!(
        recap
            .iter()
            .map(|line| line.as_str().unwrap().chars().count())
            .sum::<usize>()
            <= 1_200
    );
    assert!(!input.payload.contains("PRIVATE_NON_CHANNEL_BODY"));
    let source = "😀".repeat(500);
    let input = test_input(&rows, &source);
    let payload: Value = serde_json::from_str(&input.payload).unwrap();
    assert_eq!(payload["post"], source);
    assert!(payload["recap"].as_array().unwrap().len() < 6);
    assert_eq!(
        payload["roster"].as_array().unwrap().len(),
        candidates(&rows).len()
    );
}

#[test]
fn input_keeps_late_mention_and_its_request() {
    let rows = rows();
    let source = format!(
        "{}\nPlease ask @claude to review the parser regression.\n{}",
        "😀 unrelated. ".repeat(2_000),
        "Afterward. ".repeat(2_000)
    );
    let input = test_input(&rows, &source);
    let payload: Value = serde_json::from_str(&input.payload).unwrap();
    assert!(
        payload["post"]
            .as_str()
            .unwrap()
            .contains("Please ask @claude to review the parser regression.")
    );
    assert!(input.payload.len() + input.schema.len() + INSTRUCTIONS.len() <= MAX_INPUT_BYTES);
}

#[test]
fn input_rejects_roster_over_cap_instead_of_dropping_candidates() {
    let mut rows = rows();
    add_members(&mut rows, 31, "review");
    let members = candidates(&rows);
    let source = members
        .iter()
        .map(|member| format!("Ask @{} to review.", member.alias))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(matches!(
        build_input(RouterRequest {
            host: Some(AgentRuntime::ClaudeCode),
            rows: &rows,
            source: &source,
            candidates: &members
        }),
        Err(InputError::TooLarge)
    ));
}

#[test]
fn input_preserves_all_aliases_and_schema_under_emoji_budget_pressure() {
    let mut rows = rows();
    add_members(&mut rows, 30, "Build and review");
    let members = candidates(&rows);
    let source = members
        .iter()
        .map(|member| {
            format!(
                "{} Please ask @{} to review parser changes. ",
                "😀".repeat(600),
                member.alias
            )
        })
        .collect::<String>();
    let input = test_input(&rows, &source);
    let payload: Value = serde_json::from_str(&input.payload).unwrap();
    let schema: Value = serde_json::from_str(&input.schema).unwrap();
    let aliases: Vec<_> = members.iter().map(|member| member.alias.as_str()).collect();
    assert_eq!(input.aliases, aliases);
    assert_eq!(schema["properties"]["to"]["items"]["enum"], json!(aliases));
    assert_eq!(payload["roster"].as_array().unwrap().len(), 32);
    let mentions = crate::message::context_mentions(payload["post"].as_str().unwrap());
    for alias in aliases {
        assert!(
            mentions
                .iter()
                .any(|mention| mention.alias == alias && mention.workspace.is_none()),
            "missing {alias}"
        );
    }
    assert!(input.payload.len() + input.schema.len() + INSTRUCTIONS.len() <= MAX_INPUT_BYTES);
}

#[test]
fn input_rejects_minimum_roster_or_mention_context_over_budget() {
    let mut rows = rows();
    add_members(&mut rows, 30, &"😀".repeat(1_000));
    assert!(matches!(
        build_input(RouterRequest {
            host: Some(AgentRuntime::ClaudeCode),
            rows: &rows,
            source: "ask @claude",
            candidates: &candidates(&rows),
        }),
        Err(InputError::TooLarge)
    ));
    let rows = self::rows();
    let source = format!("Ask @claude to review. {} ", "😀".repeat(100)).repeat(1_000);
    assert!(matches!(
        build_input(RouterRequest {
            host: Some(AgentRuntime::ClaudeCode),
            rows: &rows,
            source: &source,
            candidates: &candidates(&rows),
        }),
        Err(InputError::TooLarge)
    ));
}

#[test]
fn mention_windows_follow_canonical_byte_spans_and_qualification() {
    let source = "😀 \\@claude `@codex` #other @claude @codex. Then ask @claude to review. `unterminated @codex";
    let mentions = crate::message::context_mention_spans(source);
    let tokens: Vec<_> = mentions
        .iter()
        .map(|(_, span)| &source[span.clone()])
        .collect();
    assert_eq!(tokens, ["@claude", "@codex", "@claude"]);
    assert!(mentions[0].0.workspace.is_some());
    assert!(mentions[1].0.workspace.is_some());
    assert!(mentions[2].0.workspace.is_none());
    assert_eq!(
        crate::message::context_mentions(source),
        mentions
            .iter()
            .map(|(mention, _)| mention.clone())
            .collect::<Vec<_>>()
    );
    let source = format!(
        "ESCAPED \\@claude. `CODE @codex`. #other @claude @codex. {} Then ask @claude to review.",
        "😀 unrelated. ".repeat(1_000)
    );
    let input = test_input(&rows(), &source);
    let payload: Value = serde_json::from_str(&input.payload).unwrap();
    let post = payload["post"].as_str().unwrap();
    assert!(post.contains("Then ask @claude to review."));
    assert!(!post.contains("ESCAPED"));
    assert!(!post.contains("CODE"));
    assert!(!post.contains("#other"));
}

fn add_members(rows: &mut DashboardRows, count: usize, persona: &str) {
    for index in 0..count {
        let alias = format!("member-{index}");
        rows.addresses.push(AddressRow {
            address_id: alias.clone(),
            alias: alias.clone(),
            role_name: alias,
            persona: Some(persona.to_owned()),
            pane_type: PaneType::Agent,
            retired: false,
            closed_at_ms: None,
            bound: true,
            occupied: true,
            agent_status: None,
            blocked_since_ms: None,
            blocked_message_id: None,
            blocked_conversation: None,
        });
    }
}

#[test]
fn provider_metrics_keep_real_turns_partial_receipts_and_unknown_fields() {
    let claude = metrics(
        "claude",
        r#"{"num_turns":3,"iteration":99,"usage":{"input_tokens":111,"output_tokens":27},"total_cost_usd":0.003}"#,
        true,
    );
    assert_eq!(claude.num_turns, Some(3));
    assert_eq!(claude.input_tokens, Some(111));
    assert_eq!(claude.output_tokens, Some(27));
    assert_eq!(claude.provider_usd, Some(0.003));
    assert!(claude.usage_complete);
    let absent = metrics(
        "claude",
        r#"{"iteration":99,"usage":{"input_tokens":0}}"#,
        true,
    );
    assert_eq!(absent.num_turns, None);
    assert_eq!(absent.input_tokens, Some(0));
    assert_eq!(absent.output_tokens, None);
    assert_eq!(absent.provider_usd, None);
    assert!(!absent.usage_complete);
    let receipts = concat!(
        "{\"type\":\"turn.started\"}\n",
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":10,\"output_tokens\":2}}\n",
        "{\"type\":\"turn.started\"}\n",
        "{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":20,\"output_tokens\":3}}\n",
    );
    let codex = metrics("codex", receipts, true);
    assert_eq!(codex.num_turns, Some(2));
    assert_eq!(codex.input_tokens, Some(30));
    assert_eq!(codex.output_tokens, Some(5));
    assert!(codex.usage_complete);
    assert_eq!(codex.provider_usd, None);
    let partial = metrics(
        "codex",
        &format!("{receipts}{{\"type\":\"turn.started\"}}\n"),
        false,
    );
    assert_eq!(partial.num_turns, Some(2));
    assert_eq!(partial.input_tokens, Some(30));
    assert_eq!(partial.output_tokens, Some(5));
    assert!(!partial.usage_complete);
    let unknown = metrics("codex", "{\"type\":\"turn.started\"}\n", false);
    assert_eq!(unknown.num_turns, None);
    assert_eq!(unknown.input_tokens, None);
    assert_eq!(unknown.output_tokens, None);
}

const FAKE_CLI: &str = r#"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
extern char **environ;
int main(int argc, char **argv) {
    char payload[65536];
    size_t bytes = fread(payload, 1, sizeof(payload) - 1, stdin);
    payload[bytes] = 0;
    FILE *record = fopen(getenv("TEST_ROUTER_RECORD"), "a");
    if (!record) return 2;
    for (int i = 0; i < argc; i++) fprintf(record, "ARG<%s>\n", argv[i]);
    fprintf(record, "INPUT<%s>\n", payload);
    for (char **entry = environ; *entry; entry++) {
        if (!strncmp(*entry, "HERDR_", 6) || !strncmp(*entry, "NAMEDROP_", 9) ||
            !strncmp(*entry, "CLAUDE_CODE_SESSION_ID=", 23) || !strncmp(*entry, "CODEX_THREAD_ID=", 16) ||
            !strncmp(*entry, "AI_AGENT=", 9) || !strncmp(*entry, "CLAUDE_CODE_MESSAGING_TOKEN=", 28))
            fprintf(record, "LEAKED<%s>\n", *entry);
    }
    fclose(record);
    if (strstr(payload, "TEST_EXIT")) {
        puts("{\"num_turns\":2,\"usage\":{\"input_tokens\":77,\"output_tokens\":9}}");
        return 7;
    }
    if (strstr(payload, "TEST_PARTIAL")) {
        puts("{\"type\":\"turn.started\"}");
        puts("{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":55,\"output_tokens\":8}}");
        puts("{\"type\":\"turn.started\"}");
        fflush(stdout); sleep(30); return 3;
    }
    if (strstr(payload, "TEST_MISSING")) {
        puts("{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":44,\"output_tokens\":7}}");
        return 0;
    }
    if (strstr(payload, "TEST_TIMEOUT")) { sleep(30); return 3; }
    const char *output = NULL;
    for (int i = 1; i < argc; i++) if (!strcmp(argv[i-1], "-o")) output = argv[i];
    const char *result = strstr(payload, "TEST_EMPTY") ? "{\"to\":[],\"reason\":\"room statement\"}" :
        "{\"to\":[\"claude\",\"ghost\",\"terminal\",\"retired\",\"operator\",\"codex\",\"claude\"],\"reason\":\"build question\"}";
    if (strstr(payload, "assert_eq!(actual, expected)"))
        result = "{\"to\":[\"codex\"],\"reason\":\"Please investigate the failed test and suggest a fix\"}";
    if (strstr(payload, "these proposed instructions"))
        result = "{\"to\":[\"claude\"],\"reason\":\"Claude review request\"}";
    if (strstr(payload, "TEST_GARBAGE")) result = "not JSON";
    if (output) {
        FILE *file = fopen(output, "w"); if (!file) return 4;
        fputs(result, file); fclose(file);
        puts("{\"type\":\"thread.started\"}");
        puts("{\"type\":\"turn.completed\",\"usage\":{\"input_tokens\":102,\"output_tokens\":14,\"cached_input_tokens\":2}}");
    } else if (strstr(payload, "TEST_GARBAGE")) puts(result);
    else printf("{\"type\":\"result\",\"result\":\"\",\"structured_output\":%s,\"usage\":{\"input_tokens\":101,\"output_tokens\":13}}\n", result);
    return 0;
}
"#;

fn run_fake_case(case: &str, directory: &Path) {
    let record = directory.join(format!("record-{case}"));
    let output = Command::new(env::current_exe().unwrap())
            .args(["--exact", "agent_status_model::channel_router::tests::fake_cli_routes_with_isolation_receipts_and_deadline", "--nocapture", "--test-threads=1"])
            .env("TEST_ROUTER_CASE", case)
            .env("TEST_ROUTER_RECORD", &record)
            .env("HOME", directory.join("home"))
            .env("CODEX_HOME", directory.join("codex-home"))
            .env("NAMEDROP_CHANNEL_ROUTER", if case == "off" { "off" } else { "on" })
            .env("NAMEDROP_AGENT_STATUS_CLAUDE_BIN", directory.join("fake-cli"))
            .env("NAMEDROP_AGENT_STATUS_CODEX_BIN", directory.join("fake-cli"))
            .env("NAMEDROP_ROUTER_EVAL_INPUT", concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/channel_router_cases.json"))
            .env("NAMEDROP_ROUTER_EVAL_OUTPUT", directory.join("eval.jsonl"))
            .env("NAMEDROP_ROUTER_EVAL_HOST", "both")
            .env("NAMEDROP_ROUTER_EVAL_FORMAT", "both")
            .env("NAMEDROP_ROUTER_EVAL_REPETITIONS", "1")
            .env("NAMEDROP_ROUTER_EVAL_DEADLINE_MS", "20000")
            .env("HERDR_UNKNOWN_FUTURE_MARKER", "synthetic-secret")
            .env("NAMEDROP_UNKNOWN_FUTURE_MARKER", "synthetic-secret")
            .env("CLAUDE_CODE_SESSION_ID", "synthetic-secret")
            .env("CODEX_THREAD_ID", "synthetic-secret")
            .env("AI_AGENT", "synthetic-secret")
            .env("CLAUDE_CODE_MESSAGING_TOKEN", "synthetic-secret")
            .output().unwrap();
    assert!(
        output.status.success(),
        "{case}: {}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if matches!(case, "off" | "nohost" | "overcap" | "oversized") {
        assert!(!record.exists(), "{case} must not spawn a worker");
    } else if case != "cancelled" {
        let recorded = fs::read_to_string(record).unwrap();
        assert!(!recorded.contains("LEAKED<"), "{case}: {recorded}");
        assert!(!recorded.contains("synthetic-secret"), "{case}: {recorded}");
        let codex = matches!(case, "codex" | "partial" | "missing" | "plaincodex");
        if case != "eval" && (!codex || case == "plaincodex") {
            assert!(!recorded.contains("ARG<--json-schema>"));
            assert!(!recorded.contains("ARG<--output-schema>"));
            assert!(recorded.contains("Output JSON schema:"));
            assert!(recorded.contains("additionalProperties"));
        }
        if codex {
            assert!(recorded.contains("ARG<gpt-5.6-luna>"));
            assert!(recorded.contains("ARG<--json>"));
            assert!(recorded.contains("ARG<--ignore-user-config>"));
            if case != "plaincodex" {
                assert!(recorded.contains("ARG<--output-schema>"));
                assert!(!recorded.contains("Output JSON schema:"));
            }
        } else {
            assert!(recorded.contains("ARG<claude-haiku-4-5>"));
            assert!(recorded.contains("ARG<--tools>\nARG<>"));
            assert!(recorded.contains("ARG<--setting-sources>\nARG<>"));
            assert!(recorded.contains("ARG<--strict-mcp-config>"));
            assert!(recorded.contains("ARG<--no-session-persistence>"));
        }
    }
}

#[test]
fn fake_cli_routes_with_isolation_receipts_and_deadline() {
    if let Ok(case) = env::var("TEST_ROUTER_CASE") {
        if case == "eval" {
            super::eval::real_host_router_corpus();
            let records: Vec<Value> =
                fs::read_to_string(env::var("NAMEDROP_ROUTER_EVAL_OUTPUT").unwrap())
                    .unwrap()
                    .lines()
                    .map(|line| serde_json::from_str(line).unwrap())
                    .collect();
            assert_eq!(records.len(), 48);
            assert_eq!(records[0]["format"], "structured");
            assert_eq!(records[1]["format"], "plain");
            assert_eq!(records[4]["format"], "plain");
            assert!(records.iter().all(|record| record["outcome"] == "routed"));
            assert!(
                records
                    .iter()
                    .all(|record| record["metrics"]["input_tokens"].is_u64())
            );
            assert_eq!(records[0]["candidates"], json!(["claude"]));
            assert!(records.iter().all(|record| {
                record["expected_topic"]
                    == json!({"min_words": 2, "max_words": 4, "omit_names": true})
            }));
            for record in &records {
                let topic = &record["topic_compliance"];
                let display_topic = &record["display_topic_compliance"];
                assert_eq!(display_topic["char_bounds_match"], true);
                assert_eq!(display_topic["word_bounds_match"], true);
                match record["id"].as_str().unwrap() {
                    "request-with-code" => {
                        assert_eq!(record["matches"], true);
                        assert_eq!(record["topic_matches"], false);
                        assert_eq!(topic["word_bounds_match"], false);
                        assert_eq!(topic["names_omitted"], true);
                        assert_eq!(record["display_topic_matches"], true);
                        assert_eq!(
                            record["raw_reason"],
                            "Please investigate the failed test and suggest a fix"
                        );
                        assert_eq!(record["reason"], "Please investigate the failed");
                        assert_eq!(topic["word_count"], 9);
                        assert_eq!(display_topic["word_count"], 4);
                    }
                    "quoted-instructions" => {
                        assert_eq!(record["matches"], true);
                        assert_eq!(record["topic_matches"], false);
                        assert_eq!(topic["word_bounds_match"], true);
                        assert_eq!(topic["names_omitted"], false);
                        assert_eq!(display_topic["names_omitted"], false);
                        assert_eq!(record["display_topic_matches"], false);
                    }
                    _ => {
                        assert_eq!(record["topic_matches"], true);
                        assert_eq!(record["display_topic_matches"], true);
                    }
                }
            }
            return;
        }
        let mut rows = rows();
        if case == "overcap" {
            add_members(&mut rows, 31, "review");
        } else if case == "oversized" {
            add_members(&mut rows, 30, &"😀".repeat(1_000));
        }
        if matches!(
            case.as_str(),
            "codex" | "partial" | "missing" | "plaincodex"
        ) {
            rows.registrations
                .retain(|registration| registration.runtime == "codex");
        }
        if case == "nohost" {
            rows.registrations.clear();
        }
        let post = match case.as_str() {
            "empty" => "TEST_EMPTY A statement for the room.",
            "garbage" => "TEST_GARBAGE",
            "timeout" => "TEST_TIMEOUT",
            "exit" => "TEST_EXIT",
            "partial" => "TEST_PARTIAL",
            "missing" => "TEST_MISSING",
            _ => "Please ask several members to check the build.",
        };
        let started = Instant::now();
        let mut members = candidates(&rows);
        if matches!(
            case.as_str(),
            "codex" | "partial" | "missing" | "plaincodex"
        ) {
            members.retain(|member| member.alias == "codex");
        }
        let request = RouterRequest {
            host: if case == "nohost" {
                None
            } else if matches!(
                case.as_str(),
                "codex" | "partial" | "missing" | "plaincodex"
            ) {
                Some(AgentRuntime::Codex)
            } else {
                Some(AgentRuntime::ClaudeCode)
            },
            rows: &rows,
            source: post,
            candidates: &members,
        };
        let run = if matches!(case.as_str(), "plainclaude" | "plaincodex") {
            run_input(build_input(request).unwrap(), DEADLINE, false)
        } else if case == "cancelled" {
            route_with_cancel(request, &AtomicBool::new(true))
        } else {
            route(request)
        };
        match case.as_str() {
            "off" | "nohost" | "overcap" | "oversized" => assert!(run.attempt.is_none()),
            "cancelled" => {
                assert_eq!(
                    run.attempt.as_ref().unwrap().outcome,
                    RouterOutcome::Cancelled
                );
            }
            "exit" => {
                let attempt = run.attempt.as_ref().unwrap();
                assert_eq!(attempt.outcome, RouterOutcome::Failed);
                assert_eq!(attempt.num_turns, Some(2));
                assert_eq!(attempt.input_tokens, Some(77));
                assert_eq!(attempt.output_tokens, Some(9));
                assert!(!attempt.usage_complete);
            }
            "partial" | "missing" => {
                let attempt = run.attempt.as_ref().unwrap();
                assert_eq!(
                    attempt.outcome,
                    if case == "partial" {
                        RouterOutcome::Timeout
                    } else {
                        RouterOutcome::MissingOutput
                    }
                );
                assert_eq!(attempt.num_turns, Some(1));
                assert_eq!(
                    attempt.input_tokens,
                    Some(if case == "partial" { 55 } else { 44 })
                );
                assert_eq!(
                    attempt.output_tokens,
                    Some(if case == "partial" { 8 } else { 7 })
                );
                assert!(!attempt.usage_complete);
            }
            "garbage" | "timeout" => {
                let attempt = run.attempt.as_ref().unwrap();
                assert_eq!(
                    attempt.outcome,
                    if case == "garbage" {
                        RouterOutcome::InvalidResponse
                    } else {
                        RouterOutcome::Timeout
                    }
                );
                assert_eq!(attempt.num_turns, None);
                assert_eq!(attempt.input_tokens, None);
                assert_eq!(attempt.output_tokens, None);
                assert!(!attempt.usage_complete);
            }
            _ => assert_eq!(run.attempt.as_ref().unwrap().outcome, RouterOutcome::Routed),
        }
        let decision = run.decision;
        match case.as_str() {
            "off" | "nohost" => assert!(matches!(decision, RoutingDecision::Unrouted)),
            "garbage" | "timeout" | "exit" | "partial" | "missing" | "overcap" | "oversized"
            | "cancelled" => {
                let RoutingDecision::Failed { router } = decision else {
                    panic!("{decision:?}")
                };
                if matches!(case.as_str(), "partial" | "missing") {
                    assert_eq!(router, format!("codex-{CODEX_MODEL}"));
                } else {
                    assert_eq!(router, CLAUDE_MODEL);
                }
                if matches!(case.as_str(), "timeout" | "partial") {
                    assert!(started.elapsed() < Duration::from_secs(20));
                }
            }
            _ => {
                let RoutingDecision::Routed {
                    to,
                    reason,
                    router,
                    usage,
                } = decision
                else {
                    panic!("{decision:?}")
                };
                if case == "empty" {
                    assert!(to.is_empty());
                    assert_eq!(reason, "room statement");
                } else if matches!(
                    case.as_str(),
                    "codex" | "partial" | "missing" | "plaincodex"
                ) {
                    assert_eq!(to, ["codex"]);
                } else {
                    assert_eq!(to, ["claude", "codex"]);
                    assert_eq!(reason, "build question");
                }
                if matches!(
                    case.as_str(),
                    "codex" | "partial" | "missing" | "plaincodex"
                ) {
                    assert_eq!(router, format!("codex-{CODEX_MODEL}"));
                    assert_eq!(
                        usage,
                        Some(
                            json!({"input_tokens":102,"output_tokens":14,"cached_input_tokens":2})
                        )
                    );
                } else {
                    assert_eq!(router, CLAUDE_MODEL);
                    assert_eq!(usage, Some(json!({"input_tokens":101,"output_tokens":13})));
                }
            }
        }
        return;
    }
    let directory = env::temp_dir().join(format!("namedrop-channel-router-{}", std::process::id()));
    fs::create_dir_all(directory.join("home")).unwrap();
    fs::create_dir_all(directory.join("codex-home")).unwrap();
    fs::write(directory.join("fake.c"), FAKE_CLI).unwrap();
    let mut compiler = Command::new("cc");
    #[cfg(target_os = "linux")]
    compiler.arg("-static");
    let compiled = compiler
        .arg("-O2")
        .arg("-o")
        .arg(directory.join("fake-cli"))
        .arg(directory.join("fake.c"))
        .output()
        .unwrap();
    assert!(
        compiled.status.success(),
        "{}",
        String::from_utf8_lossy(&compiled.stderr)
    );
    for case in [
        "mixed",
        "empty",
        "codex",
        "garbage",
        "off",
        "nohost",
        "overcap",
        "oversized",
        "cancelled",
        "timeout",
        "exit",
        "partial",
        "missing",
        "plainclaude",
        "plaincodex",
        "eval",
    ] {
        run_fake_case(case, &directory);
    }
    fs::remove_dir_all(directory).unwrap();
}

#[test]
fn input_preserves_short_post_cancellations_and_reference_framing() {
    let rows = rows();
    for source in [
        "Please ask @claude for a review. Actually, cancel that request and wake nobody.",
        "For reference only. This was already completed yesterday:\nPlease ask @claude for a review.",
    ] {
        let input = test_input(&rows, source);
        let payload: Value = serde_json::from_str(&input.payload).unwrap();
        assert_eq!(payload["post"], source);
    }
    let source = format!(
        "{}\nFor reference only. Already completed yesterday.\nAsk @claude for a review. Actually cancel that request and wake nobody.\n{}",
        "x".repeat(5000),
        "y".repeat(5000)
    );
    let input = test_input(&rows, &source);
    let payload: Value = serde_json::from_str(&input.payload).unwrap();
    let post = payload["post"].as_str().unwrap();
    assert!(post.contains("Already completed yesterday."), "{post}");
    assert!(
        post.contains("cancel that request and wake nobody"),
        "{post}"
    );
}

#[test]
fn input_keeps_complete_posts_above_old_prefix_limit_when_they_fit() {
    let rows = rows();
    let source = format!(
        "{} Please ask @claude for a review. Actually cancel that request and wake nobody.",
        "x".repeat(4500)
    );
    let input = test_input(&rows, &source);
    let payload: Value = serde_json::from_str(&input.payload).unwrap();
    assert_eq!(payload["post"], source);
}
