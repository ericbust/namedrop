#![cfg(feature = "test-support")]

#[path = "support/binary.rs"]
mod binary;

#[path = "support/cleanup.rs"]
mod cleanup;

mod support;

use namedrop::{
    AgentRuntime, Clock, LifecycleRuntime, ProcessIds, execute_lifecycle_operation,
    mux::{
        AgentKind, AgentState, Effect, PaneProcess,
        fake::{EVERY_LISTING, FakeMux, StrictSpace, Takes},
    },
};
use serde_json::{Value, json};

/// Pane `p1` of space `w1`, which holds `/repo`. Herdr lists it with status
/// `unknown` and no agent or session, and `tab get` names its tab `Main`.
/// The space answers every listing and `pane process-info --pane p1`, and
/// takes `pane report-metadata` and `tab rename` but no input.
fn host(process: PaneProcess) -> StrictSpace {
    let mux = FakeMux::world()
        .endpoint("socket")
        .space("w1")
        .checkout("/repo")
        .unstamped()
        .pane("p1")
        .terminal("t1")
        .tab("tab1")
        .reports_session(false)
        .state(AgentState::parse("unknown"))
        .process(process)
        .tab_label("tab1", "Main")
        .build();
    let takes = Takes::nothing().effect(|effect| {
        matches!(
            effect,
            Effect::Identify { .. } | Effect::Badges { .. } | Effect::RenameTab { .. }
        )
    });
    StrictSpace::new(mux, EVERY_LISTING, takes)
}

/// `p1`'s shell, 42, with the foreground group Herdr reports, if any.
fn shell(group: Option<u32>, foreground: u32) -> PaneProcess {
    PaneProcess {
        shell_pid: Some(42),
        foreground_group: group,
        foreground_pids: vec![foreground],
        tty: None,
    }
}

struct Time;
impl Clock for Time {
    fn now_unix_ms(&self) -> u64 {
        1000
    }
    fn wait_ms(&self, _: u64) {}
}

#[test]
fn naming_detects_terminal_preserves_type_on_rename_and_allows_override() {
    let host = host(shell(Some(42), 42));
    let directory = support::TestDirectory::new();
    let root = directory.path();
    if std::env::var_os("NAMEDROP_TERMINAL_CLEANUP_CHILD").is_some() {
        std::fs::write(
            root.join("pane-effects-retries.log"),
            "terminal cleanup record\n",
        )
        .unwrap();
    }
    let ids = ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: root,
        home_directory: None,
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: std::path::Path::new("/repo"),
        workspace_id: Some("w1"),
        current_pane_id: Some("p1"),
        session_socket_id: "socket",
        mux: &*host,
        clock: &Time,
        ids: &ids,
    };
    let name = |input| execute_lifecycle_operation("name", input, &runtime).unwrap();
    host.set_process("p1", Some(shell(Some(43), 42)));
    assert!(
        execute_lifecycle_operation("name", json!({"alias":"build"}), &runtime)
            .unwrap_err()
            .contains("uncertain")
    );
    host.set_process("p1", Some(shell(Some(42), 42)));
    assert_eq!(name(json!({"alias":"build"}))["pane_type"], "terminal");
    host.set_agent("p1", Some(AgentKind::Known(AgentRuntime::Codex)));
    assert_eq!(
        namedrop::detect_pane_type(&runtime).unwrap(),
        Some(namedrop::PaneType::Agent)
    );
    assert_eq!(name(json!({"alias":"logs"}))["pane_type"], "terminal");
    assert_eq!(
        name(json!({"alias":"logs","pane_type":"agent"}))["pane_type"],
        "agent"
    );
    let listed: Value = execute_lifecycle_operation("address.list", json!({}), &runtime).unwrap();
    assert!(listed.to_string().contains("\"pane_type\":\"agent\""));
    drop(directory);
}

#[test]
fn a_pane_without_a_reported_foreground_group_is_uncertain() {
    let host = host(shell(None, std::process::id()));
    let directory = support::TestDirectory::new();
    let ids = ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: directory.path(),
        home_directory: None,
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: std::path::Path::new("/repo"),
        workspace_id: Some("w1"),
        current_pane_id: Some("p1"),
        session_socket_id: "socket",
        mux: &*host,
        clock: &Time,
        ids: &ids,
    };
    assert_eq!(namedrop::detect_pane_type(&runtime).unwrap(), None);
    host.set_process("p1", Some(shell(Some(42), 42)));
    assert_eq!(
        namedrop::detect_pane_type(&runtime).unwrap(),
        Some(namedrop::PaneType::Terminal)
    );
}

#[test]
fn terminal_state_cleanup_preserves_retry_records() {
    let output = crate::binary::command(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "naming_detects_terminal_preserves_type_on_rename_and_allows_override",
        ])
        .env("NAMEDROP_TERMINAL_CLEANUP_CHILD", "1")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "terminal cleanup record\n"
    );
}
