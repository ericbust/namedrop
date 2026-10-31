#[path = "fixtures/hc/process.rs"]
mod process;
use namedrop::{HcHost, HerdrCommandExecutor, HerdrCommandOutput, Host, PaneScope, PromptFailureKind};
use serde_json::{Value, json};
use std::cell::RefCell;

struct Fixture { terminal: RefCell<Value>, calls: RefCell<Vec<Vec<String>>> }
impl Fixture {
    fn new(pid: u32, program: &str) -> Self {
        let mut terminal: Value = serde_json::from_str(include_str!("fixtures/hc/get-labeled.stdout")).unwrap();
        terminal["pid"] = json!(std::os::unix::process::parent_id());
        terminal["foreground"] = json!({"pid":pid,"name":program,"argv":[program]});
        terminal["labels"]["namedrop.session.pid"] = json!(pid.to_string());
        terminal["status"] = json!({"state":"idle","app":"codex"});
        terminal["reported"] = json!(true);
        Self { terminal: RefCell::new(terminal), calls: RefCell::default() }
    }
    fn calls(&self, verb: &str) -> Vec<Vec<String>> { self.calls.borrow().iter().filter(|call| call[0] == verb).cloned().collect() }
}
impl HerdrCommandExecutor for Fixture {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        self.calls.borrow_mut().push(args.iter().map(|s| s.to_string()).collect());
        let stdout = match args {
            ["ls", "--json"] => json!({"terminals":[*self.terminal.borrow()]}).to_string(),
            ["get", "t1", "--json"] => self.terminal.borrow().to_string(),
            ["label", "t1", labels @ ..] => {
                for label in labels.iter().filter(|s| **s != "--json") {
                    let (key, value) = label.split_once('=').unwrap();
                    self.terminal.borrow_mut()["labels"][key] = json!(value);
                }
                "{}".into()
            },
            ["send", ..] => "{}".into(),
            _ => panic!("unexpected argv {args:?}"),
        };
        Ok(HerdrCommandOutput { stdout })
    }
}

#[test]
fn stale_shell_owner_cannot_receive_agent_prompts() {
    let fixture = Fixture::new(std::process::id(), "bash");
    let host = HcHost::new(&fixture);
    let pane = host.panes(PaneScope::All).unwrap().remove(0);
    assert!(pane.agent.is_none(), "sticky status.app must not classify a shell as an agent");
    assert!(pane.agent_session_id.is_none());
    fixture.calls.borrow_mut().clear();
    assert_eq!(host.prompt("t1", "hello").unwrap_err().kind, PromptFailureKind::TemporaryPreInvocation);
    assert_eq!(fixture.calls("get").len(), 1);
    assert!(fixture.calls("send").is_empty());
    assert_eq!(fixture.terminal.borrow()["labels"]["namedrop.session"], "");
    assert_eq!(fixture.terminal.borrow()["labels"]["namedrop.session.pid"], "");
    fixture.terminal.borrow_mut()["pid"] = json!(std::process::id());
    host.run_in_pane("t1", "echo allowed").unwrap();
    assert_eq!(fixture.calls("send").len(), 1);
}

#[test]
fn live_owner_with_other_foreground_defers_but_retains_session() {
    let agent = process::AgentProcess::new();
    let fixture = Fixture::new(agent.pid(), "codex");
    fixture.terminal.borrow_mut()["foreground"] = json!({"pid":std::process::id(),"name":"vim","argv":["vim"]});
    let host = HcHost::new(&fixture);
    let pane = host.panes(PaneScope::All).unwrap().remove(0);
    assert_eq!(pane.agent.as_deref(), Some("codex"));
    assert!(pane.agent_session_id.is_some());
    assert_eq!(pane.agent_status.as_deref(), Some("working"));
    fixture.calls.borrow_mut().clear();
    assert_eq!(host.prompt("t1", "hello").unwrap_err().kind, PromptFailureKind::TemporaryPreInvocation);
    assert!(host.run_in_pane("t1", "echo denied").is_err());
    assert!(fixture.calls("send").is_empty());
    assert!(fixture.calls("label").is_empty());
}

#[test]
fn ready_agent_uses_one_fresh_read_and_one_conditional_send() {
    let agent = process::AgentProcess::new();
    for status in ["idle", "done"] {
        let fixture = Fixture::new(agent.pid(), "codex");
        fixture.terminal.borrow_mut()["status"]["state"] = json!(status);
        let host = HcHost::new(&fixture);
        let pane = host.panes(PaneScope::All).unwrap().remove(0);
        assert_eq!(pane.agent_status.as_deref(), Some(status));
        fixture.calls.borrow_mut().clear();
        host.prompt("t1", "hello").unwrap();
        assert_eq!(fixture.calls("get").len(), 1);
        assert_eq!(fixture.calls("send"), vec![vec!["send", "t1", "hello", "--if", "idle", "--json"]]);
    }
}

#[test]
fn nonsettled_or_unbound_agent_never_receives_prompt() {
    let agent = process::AgentProcess::new();
    for status in [Some("working"), Some("blocked"), Some("error"), None] {
        let fixture = Fixture::new(agent.pid(), "codex");
        fixture.terminal.borrow_mut()["status"] = status.map(|state| json!({"state":state,"app":"codex"})).unwrap_or(Value::Null);
        let host = HcHost::new(&fixture);
        assert!(host.prompt("t1", "hello").is_err());
        assert_eq!(fixture.calls("get").len(), 1);
        assert!(fixture.calls("send").is_empty());
    }
    let fixture = Fixture::new(agent.pid(), "codex");
    fixture.terminal.borrow_mut()["labels"].as_object_mut().unwrap().remove("namedrop.session.pid");
    assert!(HcHost::new(&fixture).prompt("t1", "hello").is_err());
    assert!(fixture.calls("send").is_empty());
}

#[test]
fn exited_owner_is_cleared_even_when_hc_foreground_and_status_are_stale() {
    let agent = process::AgentProcess::new();
    let fixture = Fixture::new(agent.pid(), "codex");
    drop(agent);
    let host = HcHost::new(&fixture);
    let pane = host.panes(PaneScope::All).unwrap().remove(0);
    assert!(pane.agent_session_id.is_none());
    fixture.calls.borrow_mut().clear();
    assert!(host.prompt("t1", "hello").is_err());
    assert_eq!(fixture.calls("get").len(), 1);
    assert_eq!(fixture.calls("label").len(), 1);
    assert!(fixture.calls("send").is_empty());
}

#[test]
fn publish_binds_the_agent_ancestor_and_refuses_a_shell() {
    if process::in_agent_tree("publish_binds_the_agent_ancestor_and_refuses_a_shell") { return; }
    let owner = std::os::unix::process::parent_id();
    let fixture = Fixture::new(owner, "codex");
    let state = std::env::temp_dir().join(format!("hc-owner-test-{}", std::process::id()));
    let host = HcHost::new(&fixture);
    host.publish_session(&state, "t1", "new").unwrap();
    assert_eq!(fixture.terminal.borrow()["labels"]["namedrop.session.pid"], owner.to_string());
    fixture.terminal.borrow_mut()["pid"] = json!(std::process::id());
    assert!(host.publish_session(&state, "t1", "wrong").is_err());
    assert_eq!(fixture.terminal.borrow()["labels"]["namedrop.session"], "new");
    std::fs::remove_dir_all(state).unwrap();
}
