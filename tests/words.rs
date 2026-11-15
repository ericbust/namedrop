#![cfg(feature = "test-support")]
#[path = "support/binary.rs"]
mod binary;

use clap::CommandFactory;
use namedrop::{
    mux::{fake::FakeMux, *},
    *,
};
use serde_json::json;
use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "nd-words-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(root.join("home")).unwrap();
        fs::create_dir_all(root.join("state/runtime")).unwrap();
        Self(root)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn words(surface: &str, text: impl AsRef<str>) {
    let mut text = text.as_ref().to_owned();
    let allowlist: serde_json::Value =
        serde_json::from_str(include_str!("words_allowlist.json")).unwrap();
    for entry in allowlist.as_array().unwrap() {
        let source = fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(entry["file"].as_str().unwrap()),
        )
        .unwrap();
        let line = entry["line"].as_u64().unwrap() as usize;
        assert!(
            source
                .lines()
                .nth(line - 1)
                .unwrap()
                .contains(entry["source"].as_str().unwrap()),
            "stale Words allowlist location {entry}"
        );
        text = text.replace(entry["text"].as_str().unwrap(), "<foreground refusal>");
    }
    assert!(
        !text.to_lowercase().contains("herdr"),
        "Words violation on {surface}: {text}"
    );
}

#[test]
fn rex_words_sweep() {
    fn help(mut command: clap::Command, path: &str) {
        words(path, command.render_long_help().to_string());
        for child in command.get_subcommands().cloned() {
            let name = format!("{path} {}", child.get_name());
            help(child, &name);
        }
    }
    help(cli::Cli::command_for(MuxKind::Rex), "namedrop");
    let tool = IntegrationToolInvocation::new(
        "namedrop",
        std::collections::BTreeMap::from([(
            "NAMEDROP_ENDPOINT".into(),
            "rex:/tmp/words.sock".into(),
        )]),
    );
    words(
        "registered tool catalog",
        serde_json::to_string(&tool).unwrap(),
    );
    let catalog = describe_operations_for(None, MuxKind::Rex).unwrap();
    words("describe", catalog.to_string());
    for operation in operation_catalog()
        .operations
        .iter()
        .chain(&operation_catalog().management_operations)
    {
        words(
            &format!("describe {}", operation.token),
            describe_operations_for(Some(&operation.token), MuxKind::Rex)
                .unwrap()
                .to_string(),
        );
    }
    for presentation in tour::all_for(MuxKind::Rex).unwrap() {
        words("tour", format!("{presentation:?}"));
    }
    for agent in [
        None,
        Some(AgentRuntime::ClaudeCode),
        Some(AgentRuntime::Codex),
    ] {
        for identified in [false, true] {
            let f = Fixture::new();
            let home = f.0.join("home");
            let state = f.0.join("state");
            fs::write(state.join("runtime/owner"), "standalone\n").unwrap();
            let mut world = FakeMux::world()
                .endpoint("rex:/tmp/words.sock")
                .space("s1")
                .pane("p1")
                .tab("window:w")
                .terminal("t1");
            world = match agent {
                Some(AgentRuntime::Codex) => world.codex(),
                Some(AgentRuntime::ClaudeCode) => world.claude(),
                None => world,
            };
            if agent.is_some() && identified {
                world = world.session("agent-1").idle();
            }
            let mux = world.build();
            let ids = ProcessIds::default();
            let runtime = LifecycleRuntime {
                state_directory: &state,
                home_directory: Some(&home),
                agent_config: AgentConfigDirectories::default(),
                cwd: &f.0,
                workspace_id: Some("s1"),
                current_pane_id: Some("p1"),
                session_socket_id: "rex:/tmp/words.sock",
                mux: &mux,
                clock: &SystemClock,
                ids: &ids,
            };
            let activation =
                execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime)
                    .unwrap();
            words("activate json", activation.to_string());
            words(
                "activate text",
                render_command_result_to_text(&CommandResult::success("activate", activation)),
            );
            for runtime in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
                let package = package_directory(&home, runtime);
                for file in ["SKILL.md", "tool.json"] {
                    words(
                        &format!("installed {runtime:?}/{file}"),
                        fs::read_to_string(package.join(file)).unwrap(),
                    );
                }
            }
            let before_name = diagnose_collaboration(&runtime);
            if agent.is_none() {
                assert!(before_name.issues.iter().any(|i| i.code == "ND-SETUP-003"));
            }
            words(
                "doctor before naming",
                serde_json::to_string(&before_name).unwrap(),
            );
            for (operation, input) in [
                (
                    "name",
                    json!({"alias":"words", "pane_type":if agent.is_some() {"agent"} else {"terminal"}}),
                ),
                ("address.list", json!({})),
                ("worktree.pause", json!({})),
                ("worktree.resume", json!({})),
            ] {
                let result = execute_lifecycle_operation(operation, input, &runtime);
                words(operation, format!("{result:?}"));
            }
            for input in [
                r#"{"hook_event_name":"SessionStart"}"#,
                r#"{"hook_event_name":"SessionStart","source":"compact"}"#,
            ] {
                words(
                    "session policy",
                    format!("{:?}", execute_session_start_hook(input, &runtime)),
                );
            }
            words(
                "prompt roster",
                format!(
                    "{:?}",
                    execute_user_prompt_submit_hook(
                        r#"{"hook_event_name":"UserPromptSubmit","session_id":"agent-1","prompt":"hello"}"#,
                        &runtime
                    )
                ),
            );
            let report = diagnose_collaboration(&runtime);
            words("doctor json", serde_json::to_string(&report).unwrap());
            words("doctor text", render_health_report_to_text(&report));
            words(
                "doctor assistance without identity",
                request_doctor_assistance(&runtime, "").unwrap_err(),
            );
            if agent.is_some() && !identified {
                assert!(report.issues.iter().any(|i| i.code == "ND-SESSION-002"));
            }
            for screen in [
                PopupScreen::Name,
                PopupScreen::Manage,
                PopupScreen::Doctor,
                PopupScreen::Status,
                PopupScreen::Tour,
            ] {
                words(
                    &format!("launch {screen:?}"),
                    format!("{:?}", open_popup(&runtime, screen, mux.facts("p1"))),
                );
            }
            for entry in operation_catalog()
                .operations
                .iter()
                .chain(&operation_catalog().management_operations)
            {
                let operation = if entry.token == "complete.no-reply" {
                    "complete"
                } else {
                    entry.token.as_str()
                };
                let result = if operation == "describe" {
                    describe_operations_for(None, MuxKind::Rex)
                } else if is_lifecycle_operation(operation) {
                    execute_lifecycle_operation(operation, json!({}), &runtime)
                } else if is_integration_operation(operation) {
                    execute_integration_operation(operation, json!({}), &runtime)
                } else if is_context_operation(operation) {
                    execute_context_operation(operation, json!({}), &runtime)
                } else if is_message_operation(operation) {
                    execute_message_operation(operation, json!({}), &runtime)
                } else if is_control_operation(operation) {
                    execute_control_operation(operation, json!({}), &runtime)
                } else if operation.starts_with("handoff.") {
                    execute_handoff_operation(operation, json!({}), &runtime)
                } else if operation.starts_with("group.") {
                    execute_group_operation(operation, json!({}), &runtime)
                } else if operation.starts_with("reminder.") {
                    execute_reminder_operation(operation, json!({}), &runtime)
                } else {
                    panic!("uncovered catalog operation {operation}")
                };
                words(operation, format!("{result:?}"));
            }
            let unbound = LifecycleRuntime {
                current_pane_id: None,
                ..runtime
            };
            for operation in ["name", "address.create", "address.list"] {
                words(
                    operation,
                    format!(
                        "{:?}",
                        execute_lifecycle_operation(
                            operation,
                            json!({"alias":"missing"}),
                            &unbound
                        )
                    ),
                );
            }
        }
    }
}

#[test]
fn herdr_words_are_unchanged() {
    let mut actual = cli::Cli::command_for(MuxKind::Herdr);
    let mut expected = cli::Cli::command();
    assert_eq!(
        actual.render_long_help().to_string(),
        expected.render_long_help().to_string()
    );
    assert_eq!(
        format!("{:?}", tour::all_for(MuxKind::Herdr).unwrap()),
        format!("{:?}", tour::all().unwrap())
    );
    assert_eq!(
        describe_operations_for(None, MuxKind::Herdr).unwrap(),
        describe_operations(None).unwrap()
    );
    let f = Fixture::new();
    let home = f.0.join("home");
    let state = f.0.join("state");
    let mux = FakeMux::world()
        .endpoint("/tmp/words.sock")
        .space("s1")
        .pane("p1")
        .claude()
        .build();
    let ids = ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: &state,
        home_directory: Some(&home),
        agent_config: AgentConfigDirectories::default(),
        cwd: &f.0,
        workspace_id: Some("s1"),
        current_pane_id: Some("p1"),
        session_socket_id: "/tmp/words.sock",
        mux: &mux,
        clock: &SystemClock,
        ids: &ids,
    };
    let activation =
        execute_lifecycle_operation("activate", json!({"confirmed":true}), &runtime).unwrap();
    assert!(activation.get("host").is_none());
    let rendered = render_command_result_to_text(&CommandResult::success("activate", activation));
    assert!(rendered.contains("  Check native hooks: herdr integration status\n  If setup is needed, choose your runtime:\n    herdr integration install codex\n    herdr integration install claude"));
    for agent in [AgentRuntime::Codex, AgentRuntime::ClaudeCode] {
        let package = package_directory(&home, agent);
        let descriptor = fs::read_to_string(package.join("tool.json")).unwrap();
        assert_eq!(descriptor, integration_package(agent).tool_descriptor);
        let instructions = fs::read_to_string(package.join("SKILL.md")).unwrap();
        assert!(instructions.contains("Name and @mention agents or terminals in Herdr;"));
        assert!(instructions.contains("Registration uses that prefix with Herdr flags."));
    }
    let report = diagnose_collaboration(&runtime);
    let native = report
        .issues
        .iter()
        .find(|i| i.code == "ND-SESSION-002")
        .unwrap();
    assert_eq!(native.summary, "Herdr has no session identity for pane p1.");
    assert_eq!(
        native.repair.command,
        Some(vec![
            "herdr".into(),
            "integration".into(),
            "install".into(),
            "claude".into()
        ])
    );
}

#[test]
fn words_cli_uses_the_selected_home() {
    for (endpoint, forbidden) in [
        ("rex:/tmp/words.sock", "herdr"),
        ("/tmp/words.sock", "inside Rex"),
    ] {
        let out = binary::namedrop()
            .args(["--endpoint", endpoint, "--help"])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(!text.contains(forbidden), "{text}");
        if endpoint.starts_with("rex:") {
            words("binary help", text);
        }
    }
}

#[test]
fn rex_words_screens_and_runtime_failures() {
    let out = binary::command("python3")
        .args([
            "-I",
            concat!(env!("CARGO_MANIFEST_DIR"), "/tests/words_screens.py"),
            binary::namedrop_path(),
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    words("screens and runtime", String::from_utf8_lossy(&out.stdout));
    println!("{}", String::from_utf8_lossy(&out.stdout));
}
