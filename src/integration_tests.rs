use super::*;
use std::{fs, os::unix::fs::PermissionsExt, process::Command};

#[path = "../tests/support/bash_rules.rs"]
mod bash_rules;
use bash_rules::bash_rule_matches;

#[test]
fn claude_delivery_guidance_matches_managed_rules_for_every_automatic_operation() {
    let root = std::env::temp_dir().join(format!("namedrop-guidance-{}", std::process::id()));
    let home = root.join("home");
    let executable = root.join("runtime with spaces/namedrop");
    let state = root.join("plugin state");
    fs::create_dir_all(&state).unwrap();
    crate::install_integration_package(&home, AgentRuntime::ClaudeCode, &executable, &state)
        .unwrap();
    let catalog = operation_catalog();
    let operations = catalog
        .operations
        .iter()
        .chain(
            catalog
                .management_operations
                .iter()
                .filter(|op| op.automatic_permission),
        )
        .collect::<Vec<_>>();
    let tool = json!({
        "executable":executable,
        "environment":{"HERDR_PLUGIN_STATE_DIR":state,"HERDR_SOCKET_PATH":"socket-a","HERDR_WORKSPACE_ID":"workspace-1","HERDR_PANE_ID":"pane-agent"},
        "input_flag":"--input-json",
        "operations":operations.iter().map(|op| json!({"token":op.token,"argv":op.argv,"input":{}})).collect::<Vec<_>>()
    });
    // A context reference is what gives context.inspect a line.
    let guidance =
        delivery_command_guidance(&json!({"context_references":[{}], "tool": tool})).unwrap();
    let settings: Value =
        serde_json::from_slice(&fs::read(home.join(".claude/settings.json")).unwrap()).unwrap();
    let rules = settings["permissions"]["allow"].as_array().unwrap();
    assert_eq!(operations.len(), 22);
    for operation in operations {
        let prefix = format!("{}: ", operation.token);
        let command = guidance.lines().find_map(|line| line.strip_prefix(&prefix));
        if ["reminder.set", "handoff.preview"].contains(&operation.token.as_str()) {
            assert!(
                command.is_none(),
                "{} was rendered: {guidance}",
                operation.token
            );
            continue;
        }
        let command = command.unwrap();
        assert!(
            rules
                .iter()
                .any(|rule| bash_rule_matches(rule.as_str().unwrap(), command)),
            "{} guidance has no managed permission: {command}",
            operation.token
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn delivery_guidance_quotes_shell_sensitive_arguments_and_preserves_safe_tokens() {
    let root =
        std::env::temp_dir().join(format!("namedrop-quoted-guidance-{}", std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let executable = root.join("fake runtime");
    fs::write(
        &executable,
        "#!/usr/bin/env python3\nimport json, sys\nprint(json.dumps(sys.argv[1:]))\n",
    )
    .unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
    let marker = root.join("should-not-exist");
    let argv = vec![
        "send".to_owned(),
        "--no-reply".to_owned(),
        "AZaz09._/:=@+,-".to_owned(),
        "has space".to_owned(),
        "has'apostrophe".to_owned(),
        format!("$(touch '{}')", marker.display()),
        "a;b".to_owned(),
        "".to_owned(),
    ];
    let tool = json!({"executable":executable,"environment":{},"input_flag":"--input-json","operations":[{"token":"send","argv":argv,"input":{}}]});
    let guidance = delivery_command_guidance(&json!({"tool": tool})).unwrap();
    let command = guidance
        .lines()
        .find_map(|line| line.strip_prefix("send: "))
        .unwrap();
    assert!(
        command.contains(" send --no-reply AZaz09._/:=@+,- "),
        "safe argv tokens must be bare: {command}"
    );
    let output = Command::new("/bin/sh")
        .args(["-c", command])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let actual: Vec<String> = serde_json::from_slice(&output.stdout).unwrap();
    let mut expected = argv;
    expected.extend(["--input-json".to_owned(), "{}".to_owned()]);
    assert_eq!(actual, expected);
    assert!(!marker.exists(), "guidance executed shell substitution");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn delivery_guidance_says_only_what_its_own_lines_ask_for() {
    let guidance = |operations: Value| {
        delivery_command_guidance(&json!({"tool":{
            "executable":"/bin/namedrop","environment":{},"input_flag":"--input-json",
            "operations":operations
        }}))
        .unwrap()
    };
    let prose = |guidance: &str| guidance.lines().next().unwrap().to_owned();
    let complete = json!({"token":"complete.no-reply","argv":["complete","--no-reply"],"input":{"attempt_id":"attempt-1"}});

    let returned = prose(&guidance(json!([complete])));
    assert_eq!(returned, format!("{COMMAND_HEADER} {SANDBOX_SENTENCE}"));

    let reply = json!({"token":"reply","argv":["reply"],"input":{"attempt_id":"attempt-1","source":"@lead <reply body>"}});
    let asked = prose(&guidance(json!([reply, complete])));
    assert!(
        asked.starts_with(COMMAND_HEADER) && asked.ends_with(SANDBOX_SENTENCE),
        "{asked}"
    );
    for (need, sentence) in COMMAND_SENTENCES {
        assert_eq!(
            asked.contains(sentence),
            matches!(need, CommandNeed::AuthoredText),
            "{asked}"
        );
    }

    let mut tracked = reply.clone();
    tracked["input"]["work_outcome"] = json!("needs_continuation");
    let assign = json!({"token":"send","argv":["send"],"input":{
        "request_id":"assign-attempt-1-<new-assignment-key>-context",
        "source":"@<recipient> <specific new assignment instruction>",
        "context":{"snapshot":{"objective":"<context objective>","decisions":[]}}}});
    let nudged = prose(&guidance(json!([tracked, assign])));
    for (_, sentence) in COMMAND_SENTENCES {
        assert!(nudged.contains(sentence), "{sentence} is missing: {nudged}");
    }
}

fn rendered_tokens(guidance: &str) -> Vec<&str> {
    guidance
        .lines()
        .skip(1)
        .map(|line| line.split_once(": ").unwrap().0)
        .collect()
}

const UNRENDERED_LEAD: &str = "No line is shown for ";

/// The tokens the unrendered sentence names, or `None` when it is absent.
fn named_unrendered(guidance: &str) -> Option<Vec<&str>> {
    let prose = guidance.lines().next().unwrap();
    let (_, rest) = prose.split_once(UNRENDERED_LEAD)?;
    let named = rest.split_once(';').unwrap().0;
    Some(
        named
            .split(", ")
            .map(|token| token.trim_matches('`'))
            .collect(),
    )
}

#[test]
fn delivery_guidance_leaves_rarely_used_operations_to_control_tool() {
    let rendered = delivery_command_guidance(&json!({"context_references":[],"tool":{
        "executable":"/bin/namedrop","environment":{},"input_flag":"--input-json",
        "operations":[
            {"token":"reminder.set","argv":["reminder","set"],"input":{"body":"<what to do later>"}},
            {"token":"context.inspect","argv":["context","inspect"],"input":{"thread_id":"t"}},
            {"token":"handoff.preview","argv":["handoff","preview"],"input":{"source":"@<recipient> <x>"}},
            {"token":"handoff.preview","argv":["handoff","preview"],"input":{"source":"@<recipient> <x>","context":{"snapshot":{}}}},
            {"token":"context.read","argv":["context","read"],"input":{"grant_id":"g"}},
            {"token":"context.read","argv":["context","read"],"input":{"grant_id":"<grant_id from context.inspect>","selection":{"kind":"snapshot"}}},
            {"token":"complete.no-reply","argv":["complete","--no-reply"],"input":{"attempt_id":"a"}}
        ]
    }}))
    .unwrap();
    assert_eq!(
        rendered_tokens(&rendered),
        ["context.read", "complete.no-reply"]
    );
    assert!(
        !rendered.contains("snapshot contents"),
        "an unrendered line still drove the prose: {rendered}"
    );
    // context.read keeps a line, so only the operations with none are named.
    assert_eq!(
        named_unrendered(&rendered).unwrap(),
        ["reminder.set", "context.inspect", "handoff.preview"]
    );
}

#[test]
fn a_context_reference_renders_context_inspect_and_the_read_that_waits_on_it() {
    let guidance = |references: Value| {
        delivery_command_guidance(&json!({"context_references":references,"tool":{
            "executable":"/bin/namedrop","environment":{},"input_flag":"--input-json",
            "operations":[
                {"token":"reply","argv":["reply"],"input":{"attempt_id":"a","source":"@lead <reply body>"}},
                {"token":"reminder.set","argv":["reminder","set"],"input":{"body":"<what to do later>"}},
                {"token":"context.inspect","argv":["context","inspect"],"input":{"message_id":"m","reference_address_id":"r"}},
                {"token":"context.read","argv":["context","read"],"input":{"grant_id":"<grant_id from context.inspect>","selection":{"kind":"snapshot"}}}
            ]
        }}))
        .unwrap()
    };
    let referenced = guidance(json!([{"address_id":"r"}]));
    assert_eq!(
        rendered_tokens(&referenced),
        ["reply", "context.inspect", "context.read"]
    );
    assert_eq!(named_unrendered(&referenced).unwrap(), ["reminder.set"]);

    for references in [json!([]), Value::Null] {
        let unreferenced = guidance(references);
        assert_eq!(rendered_tokens(&unreferenced), ["reply"]);
        assert_eq!(
            named_unrendered(&unreferenced).unwrap(),
            ["reminder.set", "context.inspect", "context.read"]
        );
    }
}

#[test]
fn the_unrendered_sentence_says_how_to_build_a_line_in_rendered_order() {
    let guidance = delivery_command_guidance(&json!({"tool":{
        "executable":"/bin/namedrop","environment":{},"input_flag":"--input-json",
        "operations":[{"token":"reminder.set","argv":["reminder","set"],"input":{}}]
    }}))
    .unwrap();
    let prose = guidance.lines().next().unwrap();
    let order = [
        "`control.tool`",
        "executable",
        "`--plugin-state`",
        "argv",
        "`--herdr-socket`",
        "`--herdr-workspace`",
        "`--herdr-pane`",
        "input flag",
        "JSON",
    ]
    .map(|part| {
        prose
            .find(part)
            .unwrap_or_else(|| panic!("{part} is missing: {prose}"))
    });
    assert!(order.is_sorted(), "{prose}");
}

#[test]
fn a_reply_line_drops_only_the_fields_the_cli_derives_to_the_same_value() {
    let rendered = |input: Value| {
        let guidance = delivery_command_guidance(&json!({
            "attempt_id":"attempt-1","message_id":"message-1",
            "tool":{"executable":"/bin/namedrop","environment":{},"input_flag":"--input-json",
                "operations":[{"token":"reply","argv":["reply"],"input":input}]}
        }))
        .unwrap();
        let line = guidance.lines().nth(1).unwrap().to_owned();
        serde_json::from_str::<Value>(
            line.rsplit_once(" --input-json '")
                .unwrap()
                .1
                .strip_suffix('\'')
                .unwrap(),
        )
        .unwrap()
    };
    let bound = json!({"attempt_id":"attempt-1","source":"@lead <reply body>",
        "request_id":"reply-attempt-1","author_kind":"agent_session","in_reply_to":"message-1",
        "work_outcome":"needs_continuation"});
    assert_eq!(
        rendered(bound.clone()),
        json!({"attempt_id":"attempt-1","source":"@lead <reply body>","work_outcome":"needs_continuation"})
    );
    let mut unusual = bound;
    unusual["request_id"] = json!("custom");
    unusual["in_reply_to"] = json!("message-2");
    let kept = rendered(unusual);
    assert_eq!(kept["request_id"], "custom");
    assert_eq!(kept["in_reply_to"], "message-2");
    assert!(kept.get("author_kind").is_none(), "{kept}");
}

fn final_reply_control(tracked: bool) -> Value {
    let operation =
        |token: &str, argv: Value, input: Value| json!({"token":token,"argv":argv,"input":input});
    let mut reply = json!({"request_id":"reply-attempt-1","source":"@lead <reply body>",
        "in_reply_to":"message-1","author_kind":"agent_session","attempt_id":"attempt-1"});
    if tracked {
        reply["work_outcome"] = json!("needs_continuation");
    }
    json!({"attempt_id":"attempt-1","message_id":"message-1","context_references":[],"tool":{
    "executable":"/state/runtime/namedrop","input_flag":"--input-json",
    "environment":{"HERDR_PLUGIN_STATE_DIR":"/state","HERDR_SOCKET_PATH":"/tmp/herdr.sock",
                   "HERDR_WORKSPACE_ID":"w1","HERDR_PANE_ID":"w1:p3"},
    "operations":[
        operation("reply", json!(["reply"]), reply),
        operation("complete.no-reply", json!(["complete","--no-reply"]), json!({"attempt_id":"attempt-1"})),
        operation("reminder.set", json!(["reminder","set"]), json!({"in":"<duration at least 60s>"})),
        operation("context.inspect", json!(["context","inspect"]), json!({"thread_id":"thread-1"})),
        operation("context.read", json!(["context","read"]), json!({"grant_id":"<grant_id from context.inspect>"})),
    ]}})
}

#[test]
fn a_final_reply_delivery_says_what_the_stop_does_and_shows_only_the_other_line() {
    use crate::final_reply::{Armed, NoReplyCase};
    let control = final_reply_control(false);
    let reply = Armed::Reply {
        template: Value::Null,
        tracked: false,
    };
    let claude = final_reply_guidance(&control, &reply).unwrap();
    assert!(
        claude.starts_with(
            "Namedrop commands: your final message is your reply to @lead. Namedrop sends it when your turn ends, so run no settle command and write no status line. If your turn ends while you wait on a helper or background work, Namedrop sends nothing and you reply later. For no reply, run this line first."
        ),
        "{claude}"
    );
    assert_eq!(rendered_tokens(&claude), ["complete.no-reply"]);
    assert!(
        claude.contains(SANDBOX_SENTENCE) && !claude.contains("work_outcome"),
        "{claude}"
    );

    let tracked = final_reply_guidance(
        &final_reply_control(true),
        &Armed::Reply {
            template: Value::Null,
            tracked: true,
        },
    )
    .unwrap();
    assert!(tracked.contains(FINAL_REPLY_OUTCOME), "{tracked}");
    assert!(
        !tracked.contains("work_outcome is yours to set"),
        "{tracked}"
    );

    let own = final_reply_guidance(&control, &Armed::NoReply(NoReplyCase::AnswersOwn)).unwrap();
    assert!(
        own.contains("To answer @lead instead, run this line."),
        "{own}"
    );
    assert_eq!(rendered_tokens(&own), ["reply"]);
    assert!(
        own.contains(COMMAND_SENTENCES[0].1) && own.contains(COMMAND_SENTENCES[1].1),
        "{own}"
    );

    for (case, text) in [
        (NoReplyCase::ReportToHuman, REPORT_TO_HUMAN_GUIDANCE),
        (NoReplyCase::ResultSet, RESULT_SET_GUIDANCE),
    ] {
        assert_eq!(
            final_reply_guidance(&control, &Armed::NoReply(case)).unwrap(),
            text
        );
    }
    let relayed = Armed::NoReply(NoReplyCase::Relayed);
    assert_eq!(
        final_reply_guidance(&control, &relayed).unwrap(),
        format!("{RELAYED_GUIDANCE} {FINAL_REPLY_WAITING}")
    );
    let mut channel = control.clone();
    channel["reply_mode"] = json!("channel");
    let channel = final_reply_guidance(&channel, &relayed).unwrap();
    assert!(
        channel.starts_with(&format!(
            "{RELAYED_GUIDANCE} {FINAL_REPLY_WAITING}{RELAYED_CHANNEL}"
        )),
        "{channel}"
    );
    assert_eq!(rendered_tokens(&channel), ["reply"]);
    for guidance in [&claude, &tracked, &own, &channel] {
        assert!(guidance.starts_with("Namedrop commands: "), "{guidance}");
        assert!(!guidance.contains("operator"), "{guidance}");
    }
}
