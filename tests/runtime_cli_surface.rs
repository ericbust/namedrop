#[path = "support/environment.rs"]
mod environment;
use environment::CommandHome;
use std::process::Command;

fn run(arguments: &[&str]) -> std::process::Output {
    run_with_environment(arguments, &[])
}

fn run_with_environment(arguments: &[&str], variables: &[(&str, &str)]) -> std::process::Output {
    static SEQUENCE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let home = std::env::temp_dir().join(format!(
        "nd-cli-home-{}-{}",
        std::process::id(),
        SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let output = Command::new(env!("CARGO_BIN_EXE_namedrop"))
        .isolated_env(&home)
        .env("HC_SOCKET", home.join("absent.sock"))
        .env("HC_BIN_PATH", "/bin/false")
        .env("HERDR_BIN_PATH", "/bin/false")
        .env("NAMEDROP_STATE_DIR", home.join("state"))
        .env("HERDR_PLUGIN_STATE_DIR", home.join("state"))
        .envs(variables.iter().copied())
        .args(arguments)
        .output()
        .unwrap();
    assert!(
        !home.exists(),
        "help/error invocation created state at {home:?}"
    );
    output
}

#[test]
fn tui_help_hides_the_internal_subscriber_command() {
    for arguments in [vec!["--help"], vec![]] {
        let output = run(&arguments);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!text.contains("  events"), "{text}");
    }
}

#[test]
fn tui_events_rejection_uses_the_standard_json_envelope() {
    let output = run(&["--host", "tui", "--json", "events"]);
    assert_eq!(output.status.code(), Some(64));
    assert!(output.stderr.is_empty());
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["protocol_version"], "collab/v1");
    assert_eq!(body["error"]["code"], "invalid_request");
    assert_eq!(body["error"]["message"], "events requires --host hc");
}

#[test]
fn ensure_is_hidden_and_rejected_under_tui() {
    let output = run(&["--host", "tui", "events", "--ensure"]);
    assert_eq!(output.status.code(), Some(64));
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(body["error"]["message"], "events requires --host hc");

    for arguments in [vec!["events", "--help"], vec!["help", "events"]] {
        let output = run(&arguments);
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!text.contains("--ensure"), "{arguments:?}: {text}");
    }
    let output = run(&["--host", "tui", "events", "--ensure", "--help"]);
    assert_eq!(output.status.code(), Some(64));
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unexpected argument '--ensure'")
    );
}

#[test]
fn print_host_is_not_a_product_flag() {
    let output = run(&["--json", "--print-host", "describe"]);
    assert_eq!(output.status.code(), Some(64));
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unexpected argument '--print-host'")
    );
}

#[test]
fn tui_hook_help_does_not_advertise_session_end() {
    for arguments in [
        vec!["hook", "--help"],
        vec!["--host", "tui", "hook", "--help"],
        vec!["hook", "--host=tui", "--help"],
    ] {
        let output = run(&arguments);
        assert!(output.status.success(), "{arguments:?}: {output:?}");
        assert!(output.stderr.is_empty());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(!text.contains("session-end"), "{arguments:?}: {text}");
        assert!(text.contains("session-start"), "{text}");
    }
}

#[test]
fn tui_session_end_help_uses_the_unknown_subcommand_envelope() {
    for arguments in [
        vec!["hook", "session-end", "--help"],
        vec!["hook", "session-end", "-h"],
        vec!["help", "hook", "session-end"],
        vec!["hook", "help", "session-end"],
        vec!["--host", "tui", "hook", "session-end", "--help"],
        vec!["hook", "--host=tui", "session-end", "--help"],
        vec!["hook", "session-end", "--host", "tui", "--help"],
    ] {
        let output = run(&arguments);
        assert_eq!(output.status.code(), Some(64), "{arguments:?}: {output:?}");
        assert!(output.stderr.is_empty());
        let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(body["protocol_version"], "collab/v1");
        assert_eq!(body["status"], "rejected");
        assert_eq!(body["error"]["code"], "invalid_request");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("unrecognized subcommand 'session-end'"),
            "{arguments:?}: {body}"
        );
    }
}

#[test]
fn hook_help_obeys_explicit_host_precedence_and_flag_placement() {
    let ambient = [
        ("HC_TERMINAL_ID", "inherited-terminal"),
        ("HERDR_PANE_ID", "inherited-pane"),
    ];
    for arguments in [
        vec!["--host=tui", "--hc-terminal", "other", "hook", "--help"],
        vec!["hook", "--help", "--host", "tui"],
        vec!["--herdr-pane", "pane", "hook", "--help"],
        vec!["--input-json=--host=hc", "--host=tui", "hook", "--help"],
        vec!["--input-json", "hook", "--host=tui", "hook", "--help"],
        vec!["--host=tui", "hook", "--help", "--", "--host=hc"],
    ] {
        let output = run_with_environment(&arguments, &ambient);
        assert!(output.status.success(), "{arguments:?}: {output:?}");
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(!text.contains("session-end"), "{arguments:?}: {text}");
    }
    for arguments in [
        vec!["--host=hc", "--herdr-pane", "pane", "hook", "--help"],
        vec!["hook", "--help", "--host", "hc"],
        vec!["--host=hc", "hook", "--help", "--", "--host=tui"],
        vec!["hook", "session-end", "--help", "--host", "hc"],
        vec!["--host=hc", "help", "hook", "session-end"],
        vec!["--host=hc", "hook", "help", "session-end"],
        vec!["--hc-terminal", "terminal", "hook", "session-end", "--help"],
    ] {
        let output = run_with_environment(&arguments, &ambient);
        assert!(output.status.success(), "{arguments:?}: {output:?}");
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains("session-end")
        );
    }
}

#[test]
fn session_end_keeps_normal_hook_parsing_and_quiet_cross_host_decline() {
    use clap::Parser;

    for host in ["tui", "hc"] {
        let cli = namedrop::cli::Cli::try_parse_from([
            "namedrop",
            "--host",
            host,
            "--hc-hook",
            "hook",
            "session-end",
            "--state-directory",
            "/unused-hook-state",
        ])
        .unwrap();
        assert_eq!(cli.operation(), "hook.session-end");
        assert_eq!(
            cli.session_end_hook_state_directory(),
            Some(std::path::Path::new("/unused-hook-state"))
        );
    }
    let output = run(&[
        "--hc-hook",
        "hook",
        "session-end",
        "--state-directory",
        "/unused-hook-state",
    ]);
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
}

#[test]
fn help_does_not_probe_hosts_when_unchanged_or_explicitly_selected() {
    use std::os::unix::fs::PermissionsExt;

    let root = std::env::temp_dir().join(format!("nd-cli-tripwire-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let marker = root.join("called");
    let executable = root.join("host");
    std::fs::write(
        &executable,
        format!(
            "#!/bin/sh\nprintf called > '{}'\nexit 97\n",
            marker.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
    let variables = [
        ("HC_TERMINAL_ID", "inherited-terminal"),
        ("HERDR_PANE_ID", "inherited-pane"),
        ("HC_BIN_PATH", executable.to_str().unwrap()),
        ("HERDR_BIN_PATH", executable.to_str().unwrap()),
    ];
    for arguments in [
        vec!["--help"],
        vec!["--version"],
        vec!["describe", "--help"],
        vec!["events", "--help"],
        vec!["hook", "session-start", "--help"],
        vec!["--host=tui", "hook", "--help"],
        vec!["--host=hc", "hook", "session-end", "--help"],
    ] {
        let output = run_with_environment(&arguments, &variables);
        assert!(output.status.success(), "{arguments:?}: {output:?}");
        assert!(!marker.exists(), "{arguments:?} contacted a host");
    }
    let output = run_with_environment(&["--host=tui", "hook", "session-end", "--help"], &variables);
    assert_eq!(output.status.code(), Some(64));
    assert!(!marker.exists(), "TUI error contacted a host");
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn tui_hook_missing_command_lists_only_supported_hooks() {
    let output = run(&["hook"]);
    assert_eq!(output.status.code(), Some(64));
    assert!(output.stderr.is_empty());
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let message = body["error"]["message"].as_str().unwrap();
    assert!(message.contains("agent-status-changed"), "{message}");
    assert!(message.contains("session-start"), "{message}");
    assert!(!message.contains("session-end"), "{message}");
}

#[test]
fn tui_hook_typo_suggests_only_session_start() {
    let output = run(&["hook", "session-ed"]);
    assert_eq!(output.status.code(), Some(64));
    assert!(output.stderr.is_empty());
    let body: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let message = body["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("tip: a similar subcommand exists: 'session-start'"),
        "{message}"
    );
    assert!(!message.contains("session-end"), "{message}");
}

#[test]
fn hc_hook_missing_command_and_typo_preserve_output() {
    let output = run(&["--host", "hc", "hook"]);
    assert_eq!(output.status.code(), Some(64));
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        r#"{"protocol_version":"collab/v1","status":"rejected","operation":null,"error":{"code":"invalid_request","message":"Runtime hooks installed for supported agents\n\nUsage: namedrop hook [OPTIONS] <COMMAND>\n\nCommands:\n  agent-status-changed  \n  agent-status-worker   \n  mailbox-wake          \n  session-start         \n  session-end           \n  user-prompt-submit    \n  help                  Print this message or the help of the given subcommand(s)\n\nOptions:\n      --json                         Print the complete JSON response, including when running in a terminal\n      --human                        Print readable output, including when piping or saving it to a file\n      --input-json <JSON>            Structured input for agent-facing operations. Run `describe <operation-token>` for the schema and examples [default: {}]\n      --input-json-file <PATH>       Read structured input from a file without shell substitution\n      --host <HOST>                  [possible values: tui, hc]\n      --hc-terminal <TERMINAL>       \n      --hc-space <SPACE>             \n      --plugin-state <PATH>          \n      --herdr-socket <SOCKET>        \n      --herdr-workspace <WORKSPACE>  \n      --herdr-pane <PANE>            \n  -h, --help                         Print help\n"}}
"#
    );
    let output = run(&["--host", "hc", "hook", "session-ed"]);
    assert_eq!(output.status.code(), Some(64));
    assert!(output.stderr.is_empty());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap(),
        r#"{"protocol_version":"collab/v1","status":"rejected","operation":null,"error":{"code":"invalid_request","message":"error: unrecognized subcommand 'session-ed'\n\n  tip: some similar subcommands exist: 'session-start', 'session-end'\n\nUsage: namedrop hook [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.\n"}}
"#
    );
}

#[test]
fn hook_typo_with_malformed_runtime_flags_preserves_original_error() {
    let expected = br#"{"protocol_version":"collab/v1","status":"rejected","operation":null,"error":{"code":"invalid_request","message":"error: unrecognized subcommand 'session-ed'\n\n  tip: some similar subcommands exist: 'session-start', 'session-end'\n\nUsage: namedrop hook [OPTIONS] <COMMAND>\n\nFor more information, try '--help'.\n"}}
"#;
    let mut mismatches = Vec::new();
    for (host, variables) in [
        ("tui", vec![("HERDR_PANE_ID", "inherited-pane")]),
        ("hc", vec![("HC_TERMINAL_ID", "inherited-terminal")]),
    ] {
        for arguments in [
            vec!["hook", "session-ed", "--hc-terminal"],
            vec!["hook", "session-ed", "--host", "bogus"],
            vec!["hook", "session-ed", "--herdr-pane"],
            vec!["hook", "session-ed", "--plugin-state"],
        ] {
            let output = run_with_environment(&arguments, &variables);
            assert_eq!(output.status.code(), Some(64), "{host}: {arguments:?}");
            assert!(
                output.stderr.is_empty(),
                "{host}: {arguments:?}: {output:?}"
            );
            if output.stdout != expected {
                mismatches.push(format!("{host}: {arguments:?}: {output:?}"));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[test]
fn hook_help_with_malformed_runtime_flags_preserves_runtime_error() {
    let missing_host = br#"{"protocol_version":"collab/v1","status":"rejected","operation":null,"error":{"code":"invalid_request","message":"error: a value is required for '--host <HOST>' but none was supplied\n  [possible values: tui, hc]\n\nFor more information, try '--help'.\n"}}
"#.as_slice();
    let invalid_host = br#"{"protocol_version":"collab/v1","status":"rejected","operation":null,"error":{"code":"invalid_request","message":"error: invalid value 'bogus' for '--host <HOST>'\n  [possible values: tui, hc]\n\nFor more information, try '--help'.\n"}}
"#.as_slice();
    let missing_pane = br#"{"protocol_version":"collab/v1","status":"rejected","operation":null,"error":{"code":"invalid_request","message":"error: a value is required for '--herdr-pane <PANE>' but none was supplied\n\nFor more information, try '--help'.\n"}}
"#.as_slice();
    let missing_plugin_state = br#"{"protocol_version":"collab/v1","status":"rejected","operation":null,"error":{"code":"invalid_request","message":"error: a value is required for '--plugin-state <PATH>' but none was supplied\n\nFor more information, try '--help'.\n"}}
"#.as_slice();
    let mut mismatches = Vec::new();
    for (host, variables) in [
        ("tui", vec![("HERDR_PANE_ID", "inherited-pane")]),
        ("hc", vec![("HC_TERMINAL_ID", "inherited-terminal")]),
    ] {
        for (arguments, expected) in [
            (vec!["hook", "--help", "--host"], missing_host),
            (vec!["hook", "--help", "--host", "bogus"], invalid_host),
            (vec!["--host", "bogus", "hook", "--help"], invalid_host),
            (vec!["hook", "-h", "--herdr-pane"], missing_pane),
            (
                vec!["hook", "session-end", "--help", "--plugin-state"],
                missing_plugin_state,
            ),
        ] {
            let output = run_with_environment(&arguments, &variables);
            if output.status.code() != Some(64)
                || !output.stderr.is_empty()
                || output.stdout != expected
            {
                mismatches.push(format!("{host}: {arguments:?}: {output:?}"));
            }
        }
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}
