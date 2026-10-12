use namedrop::{
    Clock, HerdrCommandExecutor, HerdrCommandOutput, LifecycleRuntime, ProcessIds,
    execute_lifecycle_operation,
};
use serde_json::{Value, json};
use std::sync::Mutex;

struct Host {
    agent: Mutex<Option<&'static str>>,
    shell_ready: Mutex<bool>,
}
impl HerdrCommandExecutor for Host {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let value = match args {
            ["workspace", "list"] => {
                json!({"result":{"workspaces":[{"workspace_id":"w1"}]}})
            }
            ["worktree", "list", ..] => {
                json!({"result":{"worktrees":[{"path":"/repo","open_workspace_id":"w1"}]}})
            }
            ["pane", "list", ..] => {
                json!({"result":{"panes":[{"pane_id":"p1","terminal_id":"t1","tab_id":"tab1","workspace_id":"w1","agent":*self.agent.lock().unwrap(),"agent_status":"unknown"}]}})
            }
            ["pane", "process-info", "--pane", "p1"] => {
                json!({"result":{"process_info":{"pane_id":"p1","shell_pid":42,"foreground_process_group_id":if *self.shell_ready.lock().unwrap(){42}else{43},"foreground_processes":[{"pid":42,"name":"zsh","argv0":"zsh"}]}}})
            }
            ["tab", "get", ..] => json!({"result":{"label":"Main"}}),
            ["pane", "report-metadata", ..] | ["tab", "rename", ..] => json!({}),
            _ => return Err(format!("unexpected command: {args:?}")),
        };
        Ok(HerdrCommandOutput {
            stdout: value.to_string(),
        })
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
    let root = std::env::temp_dir().join(format!("namedrop-terminal-name-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let host = Host {
        agent: Mutex::new(None),
        shell_ready: Mutex::new(true),
    };
    let ids = ProcessIds::default();
    let runtime = LifecycleRuntime {
        state_directory: &root,
        home_directory: None,
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: std::path::Path::new("/repo"),
        workspace_id: Some("w1"),
        current_pane_id: Some("p1"),
        session_socket_id: "socket",
        herdr: &host,
        clock: &Time,
        ids: &ids,
    };
    let name = |input| execute_lifecycle_operation("name", input, &runtime).unwrap();
    *host.shell_ready.lock().unwrap() = false;
    assert!(
        execute_lifecycle_operation("name", json!({"alias":"build"}), &runtime)
            .unwrap_err()
            .contains("uncertain")
    );
    *host.shell_ready.lock().unwrap() = true;
    assert_eq!(name(json!({"alias":"build"}))["pane_type"], "terminal");
    *host.agent.lock().unwrap() = Some("codex");
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
    std::fs::remove_dir_all(root).unwrap();
}
