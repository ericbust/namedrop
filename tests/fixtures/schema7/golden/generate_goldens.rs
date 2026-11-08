//! Writes the golden prompts and state with the 1.6.0 code. It is not built here:
//! copy it to `tests/` of a checkout of caf657d and run
//! `GOLDEN_DIR=<this directory> cargo test --locked --test generate_goldens`.
#[path = "support/cleanup.rs"]
mod cleanup;
use namedrop::{
    ActorKind, AgentRuntime, AgentSessionId, Clock, HerdrCommandExecutor, HerdrCommandOutput,
    IdGenerator, LifecycleRuntime, MutationAttribution, PaneAddressDraft, PaneAddressId,
    PaneBindingId, PaneRoleId, StateNamespace, StateStore, complete_capability_profile,
    drain_mailboxes, execute_handoff_operation, execute_integration_operation,
    execute_message_operation, execute_user_prompt_submit_hook, issue_registration_challenge,
};
use serde_json::{Value, json};
use std::{fs, path::PathBuf, process::Command, sync::Mutex, sync::atomic::*};

const SOCKET: &str = "/tmp/nd-golden.sock";
const WS: &str = "w1";
const LEAD: &str = "w1:p1";
const REVIEWER: &str = "w1:p2";
const RESEARCHER: &str = "w1:p3";
const AGENTS: [(&str, &str, &str); 3] = [
    ("lead", LEAD, "session-lead"),
    ("reviewer", REVIEWER, "session-reviewer"),
    ("researcher", RESEARCHER, "session-researcher"),
];

struct Herdr(Mutex<Vec<(String, String)>>);
impl HerdrCommandExecutor for Herdr {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let stdout = match args {
            ["workspace", "list"] => json!({"result":{"workspaces":[{"workspace_id":WS}]}}).to_string(),
            ["worktree", "list", ..] => {
                json!({"result":{"worktrees":[{"path":"/repo","open_workspace_id":WS}]}}).to_string()
            }
            ["pane", "list", ..] => json!({"result":{"panes":AGENTS.iter().enumerate().map(|(i,(_,pane,session))|
                json!({"pane_id":pane,"terminal_id":format!("t{i}"),"tab_id":format!("w1:t{i}"),"workspace_id":WS,
                       "agent":"claude","agent_session":{"value":session},"agent_status":"idle"})).collect::<Vec<_>>()}})
            .to_string(),
            ["agent", "prompt", pane, body] => {
                self.0.lock().unwrap().push(((*pane).to_owned(), (*body).to_owned()));
                "{}".to_owned()
            }
            ["pane", "report-metadata", ..] | ["notification", ..] => "{}".to_owned(),
            other => return Err(format!("unexpected Herdr command: {other:?}")),
        };
        Ok(HerdrCommandOutput { stdout })
    }
}

struct StepClock(AtomicU64);
impl Clock for StepClock {
    fn now_unix_ms(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
    fn wait_ms(&self, ms: u64) {
        self.0.fetch_add(ms.max(1), Ordering::Relaxed);
    }
}

struct Ids(AtomicU64);
impl IdGenerator for Ids {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
    }
}

struct F {
    root: PathBuf,
    state: PathBuf,
    out: PathBuf,
    herdr: Herdr,
    clock: StepClock,
    ids: Ids,
}

impl F {
    fn rt<'a>(&'a self, pane: &'a str) -> LifecycleRuntime<'a, Herdr, StepClock, Ids> {
        LifecycleRuntime {
            state_directory: &self.state,
            home_directory: Some(&self.root),
            agent_config: namedrop::AgentConfigDirectories::default(),
            cwd: &self.root,
            workspace_id: Some(WS),
            current_pane_id: Some(pane),
            session_socket_id: SOCKET,
            herdr: &self.herdr,
            clock: &self.clock,
            ids: &self.ids,
        }
    }

    fn typed(&self, pane: &str) -> String {
        let prompts = self.herdr.0.lock().unwrap();
        prompts.iter().rev().find(|(p, _)| p == pane).unwrap().1.clone()
    }

    /// The prompt as typed and the state right now, with this run's paths as tokens.
    fn snap(&self, kind: &str, pane: &str) -> Value {
        let typed = self.typed(pane);
        let envelope: Value = serde_json::from_str(&typed).unwrap();
        let exe = envelope["control"]["tool"]["executable"].as_str().unwrap().to_owned();
        let state = self.state.display().to_string();
        let tokens = |text: &str| text.replace(&exe, "/EXE").replace(&state, "/STATE");
        let dump = Command::new("sqlite3")
            .arg(self.state.join("namedrop.sqlite3"))
            .arg(".dump")
            .output()
            .unwrap();
        let dump = String::from_utf8(dump.stdout).unwrap();
        let (data, schema): (Vec<&str>, Vec<&str>) =
            dump.lines().partition(|line| line.starts_with("INSERT INTO "));
        fs::write(self.out.join("schema.sql"), schema.join("\n") + "\n").unwrap();
        fs::write(self.out.join(format!("{kind}.sql")), tokens(&data.join("\n")) + "\n").unwrap();
        fs::write(self.out.join(format!("{kind}.prompt")), tokens(&typed)).unwrap();
        assert!(!tokens(&data.join("\n")).contains(self.root.to_str().unwrap()));
        envelope
    }

    fn close(&self, pane: &str, envelope: &Value) {
        execute_integration_operation("complete", json!({"attempt_id":envelope["control"]["attempt_id"]}), &self.rt(pane)).unwrap();
    }

    fn reply(&self, pane: &str, envelope: &Value, text: &str) {
        let mut input = envelope["control"]["tool"]["operations"].as_array().unwrap().iter()
            .find(|o| o["token"] == "reply").unwrap()["input"].clone();
        input["source"] = json!(text);
        if input.get("work_outcome").is_some() {
            input["work_outcome"] = json!("completed");
            input.as_object_mut().unwrap().remove("blocked_reason");
        }
        execute_message_operation("reply", input, &self.rt(pane)).unwrap();
    }

    fn hook(&self, prompt: &str, pane: &str) {
        execute_user_prompt_submit_hook(&json!({"hook_event_name":"UserPromptSubmit","prompt":prompt}).to_string(), &self.rt(pane));
    }
}

#[test]
fn generate() {
    let out = PathBuf::from(std::env::var("GOLDEN_DIR").unwrap());
    let root = std::env::temp_dir().join(format!("nd-golden-{}", std::process::id()));
    let state = root.join("state");
    fs::create_dir_all(&state).unwrap();
    let f = F {
        root: root.clone(),
        state,
        out,
        herdr: Herdr(Mutex::new(Vec::new())),
        clock: StepClock(AtomicU64::new(1_794_000_000_000)),
        ids: Ids(AtomicU64::new(1)),
    };
    let ns = StateNamespace::for_space(SOCKET, WS);
    let at = MutationAttribution::new(ActorKind::Human, "golden", 1);
    StateStore::open(&f.state).unwrap().transaction(|tx| {
        tx.activate_worktree(&ns, &at)?;
        for (alias, pane, session) in AGENTS {
            let draft = PaneAddressDraft {
                id: PaneAddressId::new(format!("address-{alias}")),
                role_id: PaneRoleId::new(format!("role-{alias}")),
                role_name: alias.to_owned(),
                role_description: format!("Native collaborator addressed as @{alias}."),
                alias: alias.to_owned(),
            };
            tx.create_pane_address(&ns, &draft, &at)?;
            tx.bind_pane(&ns, &PaneBindingId::new(format!("binding-{alias}")), &draft.id, pane, &at)?;
            tx.observe_agent_session(&ns, &format!("observation-{alias}"), &draft.id, &AgentSessionId::new(session), "idle", &at)?;
        }
        tx.complete_reconciliation(&ns)
    }).unwrap();
    for (alias, pane, _) in AGENTS {
        let rt = f.rt(pane);
        let challenge = issue_registration_challenge(&PaneAddressId::new(format!("address-{alias}")), AgentRuntime::ClaudeCode, &rt).unwrap();
        execute_integration_operation("register", json!({"runtime":"claude-code","protocol_version":"collab/v1","nonce":challenge.nonce,"capabilities":complete_capability_profile()}), &rt).unwrap();
    }

    execute_message_operation("send", json!({"request_id":"agent-ask","source":"@reviewer Review the callback handler.","author_kind":"agent_session"}), &f.rt(LEAD)).unwrap();
    drain_mailboxes(&f.rt(LEAD)).unwrap();
    let e = f.snap("agent-ask", REVIEWER);
    f.close(REVIEWER, &e);

    f.hook("@reviewer Check the parser edge cases.", LEAD);
    drain_mailboxes(&f.rt(LEAD)).unwrap();
    let e = f.snap("person-mention", REVIEWER);
    f.reply(REVIEWER, &e, "@lead The parser is fine.");
    drain_mailboxes(&f.rt(REVIEWER)).unwrap();
    let e = f.snap("reply-to-source", LEAD);
    assert_eq!(e["control"]["reply_mode"], "report_to_human");
    f.close(LEAD, &e);

    execute_message_operation("send", json!({"request_id":"fan-out","source":"@reviewer @researcher Check the release notes.","author_kind":"agent_session"}), &f.rt(LEAD)).unwrap();
    drain_mailboxes(&f.rt(LEAD)).unwrap();
    let (a, b) = (
        serde_json::from_str::<Value>(&f.typed(REVIEWER)).unwrap(),
        serde_json::from_str::<Value>(&f.typed(RESEARCHER)).unwrap(),
    );
    f.reply(REVIEWER, &a, "@lead Notes look right.");
    f.reply(RESEARCHER, &b, "@lead Notes are complete.");
    let mut joined = false;
    for _ in 0..8 {
        drain_mailboxes(&f.rt(LEAD)).unwrap();
        let inbox = execute_integration_operation("delivery.inbox", json!({}), &f.rt(LEAD)).unwrap();
        for envelope in inbox["envelopes"].as_array().unwrap() {
            if envelope["control"]["result_set"].is_object() {
                joined = true;
            } else {
                f.close(LEAD, envelope);
            }
        }
        if joined { break; }
    }
    assert!(joined);
    let e = f.snap("result-set", LEAD);
    assert!(e["control"]["result_set"].is_object(), "{e}");
    f.close(LEAD, &e);
    drain_mailboxes(&f.rt(LEAD)).unwrap();

    let coordination = execute_handoff_operation("handoff.begin", json!({"request_id":"begin","deliverable":"A short survey"}), &f.rt(LEAD)).unwrap()["coordination_id"].clone();
    execute_message_operation("send", json!({"request_id":"assign","source":"@researcher Survey the input patterns.","author_kind":"agent_session","coordination_id":coordination}), &f.rt(LEAD)).unwrap();
    drain_mailboxes(&f.rt(LEAD)).unwrap();
    let e = f.snap("tracked-assignment", RESEARCHER);
    f.reply(RESEARCHER, &e, "@lead Three patterns.");
    let mut nudged = false;
    for _ in 0..12 {
        drain_mailboxes(&f.rt(LEAD)).unwrap();
        let inbox = execute_integration_operation("delivery.inbox", json!({}), &f.rt(LEAD)).unwrap();
        for envelope in inbox["envelopes"].as_array().unwrap() {
            if envelope["control"]["coordination_nudge"] == true {
                nudged = true;
            } else {
                f.close(LEAD, envelope);
            }
        }
        if nudged { break; }
        f.clock.wait_ms(700_000);
    }
    assert!(nudged);
    f.snap("nudge", LEAD);
    println!("clock {}", f.clock.now_unix_ms());
    fs::write(f.out.join("clock.txt"), f.clock.now_unix_ms().to_string()).unwrap();
    cleanup::remove_fixture(&root).unwrap();
}
