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
    let guidance = delivery_command_guidance(&tool).unwrap();
    let settings: Value =
        serde_json::from_slice(&fs::read(home.join(".claude/settings.json")).unwrap()).unwrap();
    let rules = settings["permissions"]["allow"].as_array().unwrap();
    assert_eq!(operations.len(), 18);
    for operation in operations {
        let prefix = format!("{}: ", operation.token);
        let command = guidance
            .lines()
            .find_map(|line| line.strip_prefix(&prefix))
            .unwrap();
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
    let guidance = delivery_command_guidance(&tool).unwrap();
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
