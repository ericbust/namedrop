#[path = "support/environment.rs"]
mod environment;
use environment::CommandHome;
use std::process::Command;

fn run(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_namedrop"))
        .isolated_env(std::env::temp_dir().join(format!("nd-cli-home-{}", std::process::id())))
        .args(arguments)
        .output()
        .unwrap()
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
        assert!(body["error"]["message"].as_str().unwrap().contains(
            "unrecognized subcommand 'session-end'"
        ), "{arguments:?}: {body}");
    }
}
