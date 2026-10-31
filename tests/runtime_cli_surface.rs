use std::process::Command;

fn run(arguments: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_namedrop"))
        .env_clear()
        .args(arguments)
        .output()
        .unwrap()
}

#[test]
fn tui_help_hides_the_internal_subscriber_command() {
    for arguments in [vec!["--help"], vec![]] {
        let output = run(&arguments);
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
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
    assert!(body["error"]["message"].as_str().unwrap().contains("unexpected argument '--print-host'"));
}
