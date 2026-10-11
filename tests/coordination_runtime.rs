use namedrop::*;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};

static NEXT: AtomicU64 = AtomicU64::new(1);
struct Fixture {
    root: PathBuf,
    state: PathBuf,
    home: PathBuf,
    ids: AtomicU64,
    now: AtomicU64,
    statuses: Mutex<BTreeMap<String, String>>,
    sessions: Mutex<BTreeMap<String, String>>,
    prompts: Mutex<Vec<(String, Value)>>,
    fail_next_nudge: Mutex<bool>,
    pane_lists: AtomicU64,
    lead_working_after: AtomicU64,
    worker_working_after: AtomicU64,
    pause_on_wait_thread: Mutex<Option<ThreadId>>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-runtime-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let state = root.join("state");
        let home = root.join("home");
        fs::create_dir_all(&home).unwrap();
        let f = Self {
            root,
            state,
            home,
            ids: AtomicU64::new(1),
            now: AtomicU64::new(100),
            statuses: Mutex::new(BTreeMap::from([
                ("lead".into(), "working".into()),
                ("worker".into(), "idle".into()),
                ("reviewer".into(), "idle".into()),
            ])),
            sessions: Mutex::new(BTreeMap::new()),
            prompts: Mutex::new(Vec::new()),
            fail_next_nudge: Mutex::new(false),
            pane_lists: AtomicU64::new(0),
            lead_working_after: AtomicU64::new(u64::MAX),
            worker_working_after: AtomicU64::new(u64::MAX),
            pause_on_wait_thread: Mutex::new(None),
        };
        let ns = f.ns();
        let by = MutationAttribution::new(ActorKind::Human, "synthetic-user", 1);
        StateStore::open(&f.state)
            .unwrap()
            .transaction(|tx| {
                tx.activate_worktree(&ns, &by)?;
                for name in ["lead", "worker", "reviewer"] {
                    let id = PaneAddressId::new(name);
                    tx.create_pane_address(
                        &ns,
                        &PaneAddressDraft {
                            id: id.clone(),
                            role_id: PaneRoleId::new(format!("role-{name}")),
                            role_name: name.into(),
                            role_description: "synthetic".into(),
                            alias: name.into(),
                        },
                        &by,
                    )?;
                    tx.bind_pane(
                        &ns,
                        &PaneBindingId::new(format!("binding-{name}")),
                        &id,
                        name,
                        &by,
                    )?;
                    tx.observe_agent_session(
                        &ns,
                        &format!("observation-{name}"),
                        &id,
                        &AgentSessionId::new(format!("session-{name}")),
                        if name == "lead" { "working" } else { "idle" },
                        &by,
                    )?;
                }
                tx.complete_reconciliation(&ns)?;
                Ok(())
            })
            .unwrap();
        for name in ["lead", "worker", "reviewer"] {
            f.register(name);
        }
        f
    }
    fn register_worker(&self, name: &str) {
        self.status(name, "idle");
        let by = MutationAttribution::new(ActorKind::Human, "synthetic-worker", 100);
        let address = PaneAddressId::new(name);
        StateStore::open(&self.state)
            .unwrap()
            .transaction(|tx| {
                tx.create_pane_address(
                    &self.ns(),
                    &PaneAddressDraft {
                        id: address.clone(),
                        role_id: PaneRoleId::new(format!("role-{name}")),
                        role_name: name.into(),
                        role_description: "synthetic".into(),
                        alias: name.into(),
                    },
                    &by,
                )?;
                tx.bind_pane(
                    &self.ns(),
                    &PaneBindingId::new(format!("binding-{name}")),
                    &address,
                    name,
                    &by,
                )?;
                tx.observe_agent_session(
                    &self.ns(),
                    &format!("observation-{name}"),
                    &address,
                    &AgentSessionId::new(format!("session-{name}")),
                    "idle",
                    &by,
                )?;
                Ok(())
            })
            .unwrap();
        self.register(name);
    }
    fn register(&self, name: &str) {
        let runtime = self.runtime(name);
        let challenge =
            issue_registration_challenge(&PaneAddressId::new(name), AgentRuntime::Codex, &runtime)
                .unwrap();
        execute_integration_operation(
            "register",
            json!({"runtime":"codex","protocol_version":"collab/v1",
            "nonce":challenge.nonce,"capabilities":complete_capability_profile()}),
            &runtime,
        )
        .unwrap();
    }
    fn replace_session(&self, name: &str, session: &str) {
        self.observe_session(name, session);
        self.register(name);
    }
    fn observe_session(&self, name: &str, session: &str) {
        self.sessions
            .lock()
            .unwrap()
            .insert(name.into(), session.into());
        StateStore::open(&self.state)
            .unwrap()
            .transaction(|tx| {
                tx.observe_agent_session(
                    &self.ns(),
                    &format!("observation-{session}"),
                    &PaneAddressId::new(name),
                    &AgentSessionId::new(session),
                    "idle",
                    &MutationAttribution::new(ActorKind::Human, "synthetic-replacement", 100),
                )
                .map(|_| ())
            })
            .unwrap();
    }
    fn check_rendered_actions(&self, envelope: &Value) {
        use std::os::unix::fs::PermissionsExt;
        let hook = execute_user_prompt_submit_hook(
            &json!({"hook_event_name":"UserPromptSubmit", "prompt":envelope.to_string()})
                .to_string(),
            &self.runtime("lead"),
        )
        .unwrap();
        let guidance = hook["hookSpecificOutput"]["additionalContext"]
            .as_str()
            .unwrap();
        let echo = self.home.join("echo-argv");
        fs::write(&echo, "#!/bin/sh\nprintf '%s\n' \"$@\"\n").unwrap();
        fs::set_permissions(&echo, fs::Permissions::from_mode(0o700)).unwrap();
        let operations = envelope["control"]["tool"]["operations"]
            .as_array()
            .unwrap();
        let lines = guidance
            .lines()
            .filter_map(|line| {
                let (token, command) = line.split_once(": ")?;
                command.starts_with('\'').then_some((token, command))
            })
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), operations.len());
        for ((token, command), operation) in lines.iter().zip(operations) {
            assert_eq!(*token, operation["token"].as_str().unwrap());
            let executable = command.split_once(" --plugin-state ").unwrap().0;
            let command = command.replacen(executable, &format!("'{}'", echo.display()), 1);
            let ran = std::process::Command::new("sh")
                .args(["-c", &command])
                .output()
                .unwrap();
            assert!(ran.status.success());
            let output = String::from_utf8(ran.stdout).unwrap();
            let args = output.lines().collect::<Vec<_>>();
            let argv = operation["argv"].as_array().unwrap();
            assert_eq!(&args[..2], ["--plugin-state", self.state.to_str().unwrap()]);
            assert_eq!(
                &args[2..2 + argv.len()],
                argv.iter().map(|v| v.as_str().unwrap()).collect::<Vec<_>>()
            );
            assert_eq!(
                &args[2 + argv.len()..args.len() - 1],
                [
                    "--herdr-socket",
                    "synthetic-socket",
                    "--herdr-workspace",
                    "synthetic-workspace",
                    "--herdr-pane",
                    "lead",
                    "--input-json"
                ]
            );
            assert_eq!(
                serde_json::from_str::<Value>(args.last().unwrap()).unwrap(),
                operation["input"]
            );
        }
    }
    fn git(&self, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&self.root)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    fn ns(&self) -> StateNamespace {
        StateNamespace::for_space("synthetic-socket", "synthetic-workspace")
    }
    fn runtime<'a>(&'a self, pane: &'a str) -> LifecycleRuntime<'a, Self, Self, Self> {
        LifecycleRuntime {
            state_directory: &self.state,
            home_directory: Some(&self.home),
            agent_config: namedrop::AgentConfigDirectories::default(),
            cwd: &self.root,
            workspace_id: Some("synthetic-workspace"),
            current_pane_id: Some(pane),
            session_socket_id: "synthetic-socket",
            herdr: self,
            clock: self,
            ids: self,
        }
    }
    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.state.join("namedrop.sqlite3")).unwrap()
    }
    fn handoff(&self, op: &str, input: Value) -> Value {
        execute_handoff_operation(op, input, &self.runtime("lead")).unwrap()
    }
    fn begin(&self) -> Value {
        self.handoff("handoff.begin",json!({"request_id":"synthetic-begin","deliverable":"A reviewed synthetic final answer"}))["coordination_id"].clone()
    }
    fn send(&self, id: &Value, key: &str, target: &str) -> Value {
        execute_message_operation("send",json!({"request_id":key,"source":format!("@{target} synthetic delegated work"),"author_kind":"agent_session","coordination_id":id}),&self.runtime("lead")).unwrap()
    }
    fn attempt(&self, message: &Value, pane: &str) -> String {
        self.db().query_row("SELECT a.attempt_id FROM delivery_attempts a JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,delivery_id,address_id) WHERE d.message_id=?1 AND a.address_id=?2 ORDER BY a.rowid DESC LIMIT 1",rusqlite::params![message.as_str().unwrap(),pane],|r|r.get(0)).unwrap()
    }
    fn reply(
        &self,
        request: &Value,
        pane: &str,
        key: &str,
        outcome: &str,
        wait: Option<&Value>,
    ) -> Value {
        let mut input = json!({"request_id":key,"source":"@lead Synthetic worker result","author_kind":"agent_session","in_reply_to":request["message_id"],"attempt_id":self.attempt(&request["message_id"],pane),"work_outcome":outcome});
        if outcome == "blocked" {
            input["blocked_reason"] = json!("synthetic dependency");
        }
        if let Some(wait) = wait {
            input["waiting_on_assignment_id"] = wait.clone();
        }
        execute_message_operation("reply", input, &self.runtime(pane)).unwrap()
    }
    fn drain(&self) {
        drain_mailboxes(&self.runtime("lead")).unwrap();
    }
    fn complete(&self, attempt: &str) {
        self.complete_for(attempt, "lead");
    }
    fn complete_for(&self, attempt: &str, pane: &str) {
        execute_integration_operation(
            "complete",
            json!({"attempt_id":attempt}),
            &self.runtime(pane),
        )
        .unwrap();
    }
    fn settle_lead_inputs(&self) {
        for _ in 0..12 {
            self.drain();
            let inbox =
                execute_integration_operation("delivery.inbox", json!({}), &self.runtime("lead"))
                    .unwrap();
            let envelopes = inbox["envelopes"].as_array().unwrap();
            if envelopes.is_empty() {
                return;
            }
            for envelope in envelopes {
                if envelope["untrusted_body"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("Coordination ")
                {
                    return;
                }
                self.complete(envelope["control"]["attempt_id"].as_str().unwrap());
            }
        }
        panic!("synthetic lead mailbox did not settle")
    }
    fn status(&self, pane: &str, status: &str) {
        self.statuses
            .lock()
            .unwrap()
            .insert(pane.into(), status.into());
    }
    fn nudge_prompts(&self) -> Vec<Value> {
        self.prompts
            .lock()
            .unwrap()
            .iter()
            .filter(|(pane, prompt)| {
                pane == "lead"
                    && prompt["untrusted_body"]
                        .as_str()
                        .unwrap_or("")
                        .starts_with("Coordination ")
            })
            .map(|(_, prompt)| prompt.clone())
            .collect()
    }
    fn wake_count(&self) -> i64 {
        self.db().query_row("SELECT count(*) FROM collaboration_messages WHERE grammar_version='coordination-nudge/v1'",[],|r|r.get(0)).unwrap()
    }
    fn attention_count(&self) -> i64 {
        self.db().query_row("SELECT count(*) FROM attention_items WHERE source_kind='coordination' AND resolved_at_ms IS NULL",[],|r|r.get(0)).unwrap()
    }
    fn attention_summary(&self) -> String {
        self.db().query_row("SELECT summary FROM attention_items WHERE source_kind='coordination' AND resolved_at_ms IS NULL",[],|r|r.get(0)).unwrap()
    }
    fn delivery(&self, message: &Value, pane: &str) -> (String, String) {
        self.db().query_row("SELECT delivery_id,state FROM mailbox_deliveries WHERE message_id=?1 AND address_id=?2",rusqlite::params![message.as_str().unwrap(),pane],|r|Ok((r.get(0)?,r.get(1)?))).unwrap()
    }
    fn adopt(&self, pane: &str, key: &str, id: &Value) -> Result<Value, String> {
        execute_handoff_operation(
            "handoff.adopt",
            json!({"request_id":key,"coordination_id":id}),
            &self.runtime(pane),
        )
    }
    fn fact_count(&self, entity_kind: &str, entity_id: &str, kind: &str, actor: &str) -> i64 {
        self.db().query_row("SELECT count(*) FROM transition_facts WHERE entity_kind=?1 AND entity_id=?2 AND transition_kind=?3 AND actor_id=?4",rusqlite::params![entity_kind,entity_id,kind,actor],|r|r.get(0)).unwrap()
    }
    fn ownership(&self, id: &Value) -> (String, String, i64) {
        self.db().query_row("SELECT owner_session_id,request_id,revision FROM coordinations WHERE coordination_id=?1",[id.as_str().unwrap()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap()
    }
    fn recipient_session(&self, message: &Value, pane: &str) -> String {
        self.db().query_row("SELECT session_snapshot FROM message_recipients WHERE message_id=?1 AND address_id=?2",rusqlite::params![message.as_str().unwrap(),pane],|r|r.get(0)).unwrap()
    }
    fn attempt_state(&self, attempt: &str) -> String {
        self.db()
            .query_row(
                "SELECT state FROM delivery_attempt_state WHERE attempt_id=?1",
                [attempt],
                |r| r.get(0),
            )
            .unwrap()
    }
    fn disposition(&self, attempt: &str) -> Option<String> {
        self.db()
            .query_row(
                "SELECT disposition_kind FROM delivery_dispositions WHERE attempt_id=?1",
                [attempt],
                |r| r.get(0),
            )
            .ok()
    }
    fn unresolved_attention(&self) -> Vec<(String, String)> {
        let db = self.db();
        let mut statement = db.prepare("SELECT source_kind,source_id FROM attention_items WHERE resolved_at_ms IS NULL ORDER BY rowid").unwrap();
        let rows = statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    }
    fn prompted(&self, message: &Value, pane: &str) -> bool {
        self.prompts
            .lock()
            .unwrap()
            .iter()
            .any(|(p, prompt)| p == pane && prompt["control"]["message_id"] == *message)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
impl Clock for Fixture {
    fn now_unix_ms(&self) -> u64 {
        self.now.load(Ordering::Relaxed)
    }
    fn wait_ms(&self, _: u64) {
        if let Some(thread) = self.pause_on_wait_thread.lock().unwrap().take() {
            StateStore::open(&self.state)
                .unwrap()
                .transaction(|tx| {
                    tx.pause_thread(
                        &self.ns(),
                        &thread,
                        &MutationAttribution::new(
                            ActorKind::Human,
                            "synthetic-pause-between-snapshots",
                            self.now_unix_ms(),
                        ),
                    )
                    .map(|_| ())
                })
                .unwrap();
        }
    }
}
impl IdGenerator for Fixture {
    fn next_id(&self, kind: &str) -> String {
        format!(
            "synthetic-{kind}-{}",
            self.ids.fetch_add(1, Ordering::Relaxed)
        )
    }
}
impl HerdrCommandExecutor for Fixture {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        let stdout = match args {
            ["worktree", "list", ..] => {
                json!({"result":{"worktrees":[{"path":self.root,"open_workspace_id":"synthetic-workspace"}]}})
            }
            ["pane", "list", ..] => {
                let count = self.pane_lists.fetch_add(1, Ordering::Relaxed) + 1;
                let statuses = self.statuses.lock().unwrap();
                let sessions = self.sessions.lock().unwrap();
                json!({"result":{"panes":(statuses.keys().map(String::as_str).map(|name|json!({"pane_id":name,"agent":"codex","terminal_id":format!("terminal-{name}"),"tab_id":"synthetic-tab","workspace_id":"synthetic-workspace","agent_session":{"value":sessions.get(name).cloned().unwrap_or_else(||format!("session-{name}"))},"agent_status":if (name=="lead" && count>=self.lead_working_after.load(Ordering::Relaxed)) || (name=="worker" && count>=self.worker_working_after.load(Ordering::Relaxed)) {"working"} else {statuses.get(name).map(String::as_str).unwrap_or("unknown")}})).collect::<Vec<_>>())}})
            }
            ["agent", "prompt", pane, prompt] => {
                let envelope: Value = serde_json::from_str(prompt).unwrap();
                if envelope["untrusted_body"]
                    .as_str()
                    .unwrap_or("")
                    .starts_with("Coordination ")
                    && std::mem::take(&mut *self.fail_next_nudge.lock().unwrap())
                {
                    return Err("synthetic-preinvocation".into());
                }
                self.prompts
                    .lock()
                    .unwrap()
                    .push(((*pane).into(), envelope));
                json!({})
            }
            ["pane", "report-metadata", ..] => json!({}),
            _ => return Err(format!("unexpected synthetic host command {args:?}")),
        };
        Ok(HerdrCommandOutput {
            stdout: stdout.to_string(),
        })
    }
    fn classify_prompt_failure(&self, error: &str) -> PromptFailureKind {
        if error == "synthetic-preinvocation" {
            PromptFailureKind::TemporaryPreInvocation
        } else {
            PromptFailureKind::AmbiguousInvocation
        }
    }
}

#[test]
fn coordinator_callback_adds_two_workers_with_separate_checkout_context() {
    let f = Fixture::new();
    f.git(&["init", "-q"]);
    f.git(&["config", "user.name", "Synthetic Test"]);
    f.git(&["config", "user.email", "test@example.invalid"]);
    fs::write(f.root.join(".gitignore"), "/state/\n/home/\n/*-checkout/\n").unwrap();
    fs::write(f.root.join("notes.txt"), "space checkout\n").unwrap();
    f.git(&["add", ".gitignore", "notes.txt"]);
    f.git(&["commit", "-qm", "synthetic base"]);

    let id = f.begin();
    let first = f.send(&id, "design-first", "worker");
    f.reply(&first, "worker", "design-result", "completed", None);
    f.status("lead", "idle");
    f.settle_lead_inputs();
    let nudge = f.nudge_prompts().pop().expect("original lead callback");
    f.check_rendered_actions(&nudge);
    let operations = nudge["control"]["tool"]["operations"].as_array().unwrap();
    let send_template = operations
        .iter()
        .find(|op| {
            op["token"] == "send"
                && op["input"].get("assignment_id").is_none()
                && op["input"].get("context").is_some()
        })
        .expect("callback must bind a new assignment with authored context")["input"]
        .clone();
    let plain = operations
        .iter()
        .find(|op| {
            op["token"] == "send"
                && op["input"].get("assignment_id").is_none()
                && op["input"].get("context").is_none()
        })
        .unwrap();
    assert_ne!(send_template["request_id"], plain["input"]["request_id"]);
    assert_eq!(send_template["coordination_id"], id);
    assert!(send_template.get("expected_revision").is_none());
    let continuation = operations
        .iter()
        .find(|op| {
            op["token"] == "send"
                && op["input"]["assignment_id"] == first["coordination"][0]["assignment_id"]
                && op["input"].get("context").is_none()
        })
        .unwrap();
    let context_continuation = operations
        .iter()
        .find(|op| {
            op["token"] == "send"
                && op["input"]["assignment_id"] == first["coordination"][0]["assignment_id"]
                && op["input"].get("context").is_some()
        })
        .unwrap();
    assert_ne!(
        continuation["input"]["request_id"],
        context_continuation["input"]["request_id"]
    );
    for key in [
        "coordination_id",
        "assignment_id",
        "expected_revision",
        "source",
    ] {
        assert_eq!(
            continuation["input"][key],
            context_continuation["input"][key]
        );
    }
    let continuation_id = format!(
        "continue-{}-{}",
        nudge["control"]["attempt_id"].as_str().unwrap(),
        first["coordination"][0]["assignment_id"].as_str().unwrap()
    );
    assert_eq!(continuation["input"]["request_id"], continuation_id);
    assert_eq!(
        context_continuation["input"]["request_id"],
        format!("{continuation_id}-context")
    );
    let before = f.handoff("handoff.results", json!({"coordination_id":id}));
    let mut assignment_ids = Vec::new();
    let mut grant_ids = Vec::new();
    let mut replies = Vec::new();
    for (index, writer) in ["writer-one", "writer-two"].into_iter().enumerate() {
        let checkout = f.root.join(format!("{writer}-checkout"));
        f.git(&["worktree", "add", "-qb", writer, checkout.to_str().unwrap()]);
        let checkout = checkout.canonicalize().unwrap();
        let evidence = format!("{writer} checkout evidence");
        fs::write(checkout.join("notes.txt"), format!("{evidence}\n")).unwrap();
        f.register_worker(writer);
        let roster = operations
            .iter()
            .find(|op| op["token"] == "address.list")
            .unwrap();
        assert_eq!(roster["argv"], json!(["address", "list"]));
        assert_eq!(roster["input"], json!({}));
        let listed = execute_lifecycle_operation(
            "address.list",
            roster["input"].clone(),
            &f.runtime("lead"),
        )
        .unwrap();
        assert!(
            listed
                .as_array()
                .unwrap()
                .iter()
                .any(|a| a["alias"] == writer && a["registered"] == true),
            "{listed}"
        );

        let mut send = send_template.clone();
        send["request_id"] = json!(
            send["request_id"]
                .as_str()
                .unwrap()
                .replace("<new-assignment-key>", writer)
        );
        send["source"] = json!(format!(
            "@{writer} Implement your slice in the supplied checkout."
        ));
        send["context"]["checkout"]["path"] = json!(checkout);
        send["context"]["snapshot"]["objective"] = json!(format!("Implement the {writer} slice"));
        send["context"]["snapshot"]["evidence"] = json!([{"kind":"diff","locator":"notes.txt"}]);
        let mut preview = operations
            .iter()
            .find(|op| op["token"] == "handoff.preview" && op["input"].get("context").is_some())
            .unwrap()["input"]
            .clone();
        assert!(preview.get("coordination_id").is_none());
        assert!(preview.get("request_id").is_none());
        preview["source"] = send["source"].clone();
        preview["context"] = send["context"].clone();
        let previewed =
            execute_handoff_operation("handoff.preview", preview, &f.runtime("lead")).unwrap();
        assert_eq!(
            previewed["context"]["snapshot"]["checkout"]["root"],
            json!(checkout)
        );
        for pane in ["worker", "reviewer"] {
            assert_eq!(
                execute_message_operation("send", send.clone(), &f.runtime(pane)).unwrap_err(),
                "worker replies inherit their assignment from the bound message",
                "only the original lead may add assignments"
            );
        }
        let mut retargeted = continuation["input"].clone();
        retargeted["source"] = json!(format!("@{writer} Take over this existing assignment."));
        assert!(
            execute_message_operation("send", retargeted, &f.runtime("lead"))
                .unwrap_err()
                .contains("exact recipient session")
        );
        let mut writer_runtime = f.runtime(writer);
        writer_runtime.cwd = &checkout;
        let sent = execute_message_operation("send", send.clone(), &f.runtime("lead"))
            .expect("one callback must dispatch each independent writer with its own context");
        assert_eq!(sent["coordination"][0]["revision"], 1);
        let assignment_id = sent["coordination"][0]["assignment_id"].clone();
        assert_ne!(assignment_id, first["coordination"][0]["assignment_id"]);
        assert!(!assignment_ids.contains(&assignment_id));
        assignment_ids.push(assignment_id);
        let retry = execute_message_operation("send", send, &f.runtime("lead")).unwrap();
        assert_eq!(retry["message_id"], sent["message_id"]);
        let after = f.handoff("handoff.results", json!({"coordination_id":id}));
        assert_eq!(after["assignments"][0], before["assignments"][0]);
        assert_eq!(
            after["assignments"][index + 1]["recipient_session_id"],
            format!("session-{writer}")
        );
        let envelope = f
            .prompts
            .lock()
            .unwrap()
            .iter()
            .find(|(pane, p)| pane == writer && p["control"]["message_id"] == sent["message_id"])
            .unwrap()
            .1
            .clone();
        let grant = envelope["control"]["attached_context"]["grant_id"].clone();
        assert!(grant.is_string());
        assert!(!grant_ids.contains(&grant));
        grant_ids.push(grant);
        let worker_ops = envelope["control"]["tool"]["operations"]
            .as_array()
            .unwrap();
        assert!(worker_ops.iter().all(|op| !matches!(
            op["token"].as_str().unwrap(),
            "send" | "handoff.resolve" | "handoff.preview" | "address.list"
        )));
        let mut read_snapshot = false;
        let mut read_diff = false;
        for op in worker_ops.iter().filter(|op| op["token"] == "context.read") {
            let read =
                execute_context_operation("context.read", op["input"].clone(), &writer_runtime)
                    .unwrap();
            if op["input"]["selection"]["kind"] == "snapshot" {
                assert_eq!(read["content"]["checkout"]["root"], json!(checkout));
                read_snapshot = true;
            } else {
                assert!(read.to_string().contains(&evidence));
                let other = if writer == "writer-one" {
                    "writer-two"
                } else {
                    "writer-one"
                };
                assert!(
                    !read
                        .to_string()
                        .contains(&format!("{other} checkout evidence"))
                );
                read_diff = true;
            }
        }
        assert!(read_snapshot && read_diff);
        let mut reply =
            worker_ops.iter().find(|op| op["token"] == "reply").unwrap()["input"].clone();
        reply["source"] = json!(format!(
            "@lead The {writer} slice is completed and checked."
        ));
        reply["work_outcome"] = json!("completed");
        replies.push((writer, checkout, reply));
    }
    assert_eq!(
        send_template["request_id"],
        format!(
            "assign-{}-<new-assignment-key>-context",
            nudge["control"]["attempt_id"].as_str().unwrap()
        )
    );
    assert_eq!(
        plain["input"]["request_id"],
        format!(
            "assign-{}-<new-assignment-key>-plain",
            nudge["control"]["attempt_id"].as_str().unwrap()
        )
    );
    f.complete(nudge["control"]["attempt_id"].as_str().unwrap());
    for (writer, checkout, reply) in replies {
        let mut writer_runtime = f.runtime(writer);
        writer_runtime.cwd = &checkout;
        execute_message_operation("reply", reply, &writer_runtime).unwrap();
    }
    f.settle_lead_inputs();
    let final_nudge = f.nudge_prompts().pop().unwrap();
    let final_ops = final_nudge["control"]["tool"]["operations"]
        .as_array()
        .unwrap();
    let results_input = final_ops
        .iter()
        .find(|op| op["token"] == "handoff.results")
        .unwrap()["input"]
        .clone();
    let results = f.handoff("handoff.results", results_input);
    assert_eq!(results["assignments"].as_array().unwrap().len(), 3);
    assert!(
        results["assignments"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["work_outcome"] == "completed")
    );
    let mut resolve = final_ops
        .iter()
        .find(|op| op["token"] == "handoff.resolve" && op["input"]["outcome"] == "completed")
        .unwrap()["input"]
        .clone();
    assert_eq!(
        resolve["reconciled_assignments"].as_array().unwrap().len(),
        3
    );
    assert!(
        resolve["reconciled_assignments"]
            .as_array()
            .unwrap()
            .iter()
            .all(|a| a["revision"] == 1)
    );
    resolve["report"] = json!("Design and both writer slices reviewed.");
    assert_eq!(f.handoff("handoff.resolve", resolve)["report_saved"], true);
}

#[test]
fn new_assignment_variants_do_not_collide_with_authored_key_suffixes() {
    let f = Fixture::new();
    let id = f.begin();
    let design = f.send(&id, "design", "worker");
    f.reply(&design, "worker", "design-done", "completed", None);
    f.status("lead", "idle");
    f.settle_lead_inputs();
    let callback = f.nudge_prompts().pop().unwrap();
    let operations = callback["control"]["tool"]["operations"]
        .as_array()
        .unwrap();
    let mut sent_messages = Vec::new();
    let mut assignments = Vec::new();
    for (key, recipient, with_context) in [
        ("slice", "worker", true),
        ("slice-context", "reviewer", false),
    ] {
        let mut send = operations
            .iter()
            .find(|op| {
                op["token"] == "send"
                    && op["input"].get("assignment_id").is_none()
                    && op["input"].get("context").is_some() == with_context
            })
            .unwrap()["input"]
            .clone();
        send["request_id"] = json!(
            send["request_id"]
                .as_str()
                .unwrap()
                .replace("<new-assignment-key>", key)
        );
        send["source"] = json!(format!("@{recipient} Implement the {key} slice."));
        if with_context {
            send["context"].as_object_mut().unwrap().remove("checkout");
            send["context"]["capture_git_state"] = json!(false);
            send["context"]["snapshot"]["objective"] = json!("Context for the slice");
        }
        let sent = execute_message_operation("send", send.clone(), &f.runtime("lead"))
            .expect("distinct intent keys and variants must dispatch independently");
        assert!(!sent_messages.contains(&sent["message_id"]));
        sent_messages.push(sent["message_id"].clone());
        let assignment = sent["coordination"][0]["assignment_id"].clone();
        assert!(!assignments.contains(&assignment));
        assignments.push(assignment);
        assert_eq!(sent["coordination"][0]["revision"], 1);
        assert_eq!(sent.get("context_grants").is_some(), with_context);
        let envelope = f
            .prompts
            .lock()
            .unwrap()
            .iter()
            .find(|(pane, e)| pane == recipient && e["control"]["message_id"] == sent["message_id"])
            .unwrap()
            .1
            .clone();
        assert_eq!(
            envelope["control"].get("attached_context").is_some(),
            with_context
        );
        if with_context {
            let read = envelope["control"]["tool"]["operations"]
                .as_array()
                .unwrap()
                .iter()
                .find(|op| op["token"] == "context.read")
                .unwrap()["input"]
                .clone();
            assert_eq!(
                execute_context_operation("context.read", read, &f.runtime(recipient)).unwrap()["content"]
                    ["objective"],
                "Context for the slice"
            );
        }
        let retry = execute_message_operation("send", send, &f.runtime("lead")).unwrap();
        assert_eq!(retry["idempotent_replay"], true);
        assert_eq!(retry["message_id"], sent["message_id"]);
        assert_eq!(retry["coordination"], sent["coordination"]);
    }
    let results = f.handoff("handoff.results", json!({"coordination_id":id}));
    assert_eq!(results["assignments"].as_array().unwrap().len(), 3);
    assert_eq!(
        results["assignments"][0]["assignment_id"],
        design["coordination"][0]["assignment_id"]
    );
    assert_eq!(results["assignments"][0]["revision"], 1);
}

#[test]
fn same_request_and_source_with_changed_context_replays_original_snapshot() {
    let f = Fixture::new();
    let id = f.begin();
    let mut input = json!({"request_id":"context-replay", "source":"@worker Review the attached context.",
        "author_kind":"agent_session", "coordination_id":id,
        "context":{"capture_git_state":false, "snapshot":{
            "objective":"original context", "applicable_user_instructions":[], "decisions":[],
            "completed_work":[], "current_work":[], "blockers":[], "evidence":[]}}});
    let sent = execute_message_operation("send", input.clone(), &f.runtime("lead")).unwrap();
    let before = f.handoff("handoff.results", json!({"coordination_id":id}));
    input["context"]["snapshot"]["objective"] = json!("different context, same request and source");
    let replay = execute_message_operation("send", input.clone(), &f.runtime("lead")).unwrap();
    assert_eq!(replay["idempotent_replay"], true);
    assert_eq!(replay["message_id"], sent["message_id"]);
    assert_eq!(
        f.handoff("handoff.results", json!({"coordination_id":id}))["assignments"],
        before["assignments"]
    );
    let envelope = f
        .prompts
        .lock()
        .unwrap()
        .iter()
        .find(|(pane, e)| pane == "worker" && e["control"]["message_id"] == sent["message_id"])
        .unwrap()
        .1
        .clone();
    let read = envelope["control"]["tool"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|op| op["token"] == "context.read")
        .unwrap()["input"]
        .clone();
    let snapshot = execute_context_operation("context.read", read, &f.runtime("worker")).unwrap();
    assert_eq!(snapshot["content"]["objective"], "original context");
    input["source"] = json!("@worker A different assignment cannot reuse that request ID.");
    assert!(execute_message_operation("send", input, &f.runtime("lead")).is_err());
}

#[test]
fn coordinator_bound_sends_keep_exact_owner_and_worker_sessions() {
    for retired in [false, true] {
        let f = Fixture::new();
        let id = f.begin();
        let first = f.send(&id, "first", "worker");
        f.reply(&first, "worker", "first-result", "completed", None);
        f.status("lead", "idle");
        f.settle_lead_inputs();
        let nudge = f.nudge_prompts().pop().unwrap();
        let ops = nudge["control"]["tool"]["operations"].as_array().unwrap();
        let mut continuation = ops
            .iter()
            .find(|op| {
                op["token"] == "send"
                    && op["input"].get("assignment_id").is_some()
                    && op["input"].get("context").is_none()
            })
            .unwrap()["input"]
            .clone();
        continuation["source"] = json!("@worker Continue the original assignment.");
        let mut new_assignment = ops
            .iter()
            .find(|op| {
                op["token"] == "send"
                    && op["input"].get("assignment_id").is_none()
                    && op["input"].get("context").is_none()
            })
            .unwrap()["input"]
            .clone();
        new_assignment["request_id"] = json!(
            new_assignment["request_id"]
                .as_str()
                .unwrap()
                .replace("<new-assignment-key>", "replacement")
        );
        new_assignment["source"] = json!("@worker Start a separate assignment.");
        let replacement_session = if retired {
            StateStore::open(&f.state)
                .unwrap()
                .transaction(|tx| {
                    tx.retire_pane_address(
                        &f.ns(),
                        &PaneAddressId::new("worker"),
                        &MutationAttribution::new(ActorKind::Human, "synthetic-retirement", 100),
                    )
                })
                .unwrap();
            let refused =
                execute_message_operation("send", new_assignment.clone(), &f.runtime("lead"))
                    .unwrap_err();
            assert!(
                refused.contains("retired Mention: @worker")
                    || refused.contains("unknown Mention: @worker"),
                "unexpected retired-alias refusal: {refused}"
            );
            f.register_worker("replacement");
            new_assignment["source"] = json!("@replacement Start a separate assignment.");
            "session-replacement"
        } else {
            f.replace_session("worker", "replacement-worker");
            "replacement-worker"
        };
        assert!(execute_message_operation("send", continuation, &f.runtime("lead")).is_err());
        let sent =
            execute_message_operation("send", new_assignment.clone(), &f.runtime("lead")).unwrap();
        assert_eq!(sent["coordination"][0]["revision"], 1);
        let results = f.handoff("handoff.results", json!({"coordination_id":id}));
        assert_eq!(
            results["assignments"][0]["assignment_id"],
            first["coordination"][0]["assignment_id"]
        );
        assert_ne!(
            sent["coordination"][0]["assignment_id"],
            first["coordination"][0]["assignment_id"]
        );
        assert_eq!(
            results["assignments"][0]["recipient_session_id"],
            "session-worker"
        );
        assert_eq!(results["assignments"][0]["revision"], 1);
        assert_eq!(
            results["assignments"][1]["recipient_session_id"],
            replacement_session
        );
        f.replace_session("lead", "replacement-lead");
        assert!(
            execute_message_operation("send", new_assignment, &f.runtime("lead")).is_err(),
            "even a registered replacement cannot replay the original owner's bound send"
        );
        assert!(
            execute_integration_operation("delivery.inbox", json!({}), &f.runtime("lead")).unwrap()
                ["envelopes"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn coordination_idle_wake_and_escalation() {
    let f = Fixture::new();
    let id = f.begin();
    let a = f.send(&id, "first", "worker");
    let b = f.send(&id, "second", "reviewer");
    f.reply(&a, "worker", "first-result", "needs_continuation", None);
    f.reply(&b, "reviewer", "second-result", "completed", None);
    assert_eq!(f.wake_count(), 0);
    f.status("lead", "idle");
    f.settle_lead_inputs();
    f.drain();
    assert_eq!(f.wake_count(), 1);
    let nudges = f.nudge_prompts();
    assert_eq!(nudges.len(), 1);
    let control = &nudges[0]["control"];
    assert_eq!(control["coordination_id"], id);
    assert_eq!(control["coordination_nudge"], true);
    assert!(
        nudges[0]["trusted_instruction"]
            .as_str()
            .unwrap()
            .contains("control.coordination_nudge true")
    );
    let worker_request = f
        .prompts
        .lock()
        .unwrap()
        .iter()
        .find(|(_, p)| p["control"]["message_id"] == a["message_id"])
        .unwrap()
        .1
        .clone();
    assert_eq!(worker_request["control"]["coordination_nudge"], false);
    let operations = control["tool"]["operations"].as_array().unwrap();
    assert!(
        operations
            .iter()
            .any(|o| o["token"] == "handoff.results" && o["input"]["coordination_id"] == id)
    );
    assert!(operations.iter().any(|o| o["token"] == "send"
        && o["input"]["assignment_id"] == a["coordination"][0]["assignment_id"]
        && o["input"]["expected_revision"] == 1));
    assert!(
        operations
            .iter()
            .any(|o| o["token"] == "handoff.resolve" && o["input"]["coordination_id"] == id)
    );
    f.complete(control["attempt_id"].as_str().unwrap());
    f.drain();
    f.drain();
    assert_eq!(f.wake_count(), 1);
    assert_eq!(f.nudge_prompts().len(), 1);
    assert_eq!(f.attention_count(), 1);

    let actions = Fixture::new();
    let id = actions.begin();
    let request = actions.send(&id, "action-first", "worker");
    actions.reply(
        &request,
        "worker",
        "action-progress",
        "needs_continuation",
        None,
    );
    actions.status("lead", "idle");
    actions.settle_lead_inputs();
    let nudge = actions.nudge_prompts();
    assert_eq!(nudge.len(), 1);
    let bound = nudge[0]["control"]["tool"]["operations"]
        .as_array()
        .unwrap();
    let results = bound
        .iter()
        .find(|o| o["token"] == "handoff.results")
        .unwrap()["input"]
        .clone();
    assert_eq!(
        actions.handoff("handoff.results", results)["coordination_id"],
        id
    );
    let mut send = bound.iter().find(|o| o["token"] == "send").unwrap()["input"].clone();
    send["source"] = json!("@worker synthetic followup from bound input");
    let sent = execute_message_operation("send", send, &actions.runtime("lead")).unwrap();
    assert_eq!(sent["coordination"][0]["revision"], 2);
    let mut resolve = bound
        .iter()
        .find(|o| o["token"] == "handoff.resolve" && o["input"]["outcome"] == "blocked")
        .unwrap()["input"]
        .clone();
    resolve["report"] = json!("Synthetic bound final report.");
    resolve["reason"] = json!("Synthetic blocked work.");
    let saved = actions.handoff("handoff.resolve", resolve);
    assert_eq!(saved["report_saved"], true);
    assert_eq!(saved["presentation"], "unknown");

    let held = Fixture::new();
    let id = held.begin();
    let request = held.send(&id, "held-first", "worker");
    held.reply(
        &request,
        "worker",
        "held-result",
        "needs_continuation",
        None,
    );
    held.status("lead", "idle");
    held.settle_lead_inputs();
    let nudge = held.nudge_prompts();
    assert_eq!(nudge.len(), 1);
    StateStore::open(&held.state)
        .unwrap()
        .transaction(|tx| {
            tx.hold_delivery_attempt(
                &held.ns(),
                &DeliveryAttemptId::new(nudge[0]["control"]["attempt_id"].as_str().unwrap()),
                &MutationAttribution::new(ActorKind::Plugin, "synthetic-host", 100),
            )
        })
        .unwrap();
    held.drain();
    assert_eq!(held.wake_count(), 1);
    assert_eq!(held.attention_count(), 1);
}

#[test]
fn coordination_wait_and_authority_controls() {
    let f = Fixture::new();
    let id = f.begin();
    let a = f.send(&id, "first", "worker");
    let b = f.send(&id, "second", "reviewer");
    f.status("lead", "idle");
    f.drain();
    assert_eq!(f.wake_count(), 0, "pending dispatchable work must defer");
    f.status("lead", "working");
    f.reply(
        &a,
        "worker",
        "blocked",
        "blocked",
        Some(&b["coordination"][0]["assignment_id"]),
    );
    f.status("lead", "blocked");
    f.drain();
    assert_eq!(f.wake_count(), 0);
    f.status("lead", "unknown");
    f.drain();
    assert_eq!(f.wake_count(), 0);
    f.status("lead", "idle");
    f.settle_lead_inputs();
    f.drain();
    assert_eq!(f.wake_count(), 0, "live reviewer dependency must defer");
    f.status("lead", "working");
    f.reply(&b, "reviewer", "dependency-done", "completed", None);
    let ns = f.ns();
    let by = MutationAttribution::new(ActorKind::Human, "synthetic-pause", 100);
    StateStore::open(&f.state)
        .unwrap()
        .transaction(|tx| tx.set_worktree_paused(&ns, true, &by))
        .unwrap();
    f.status("lead", "idle");
    f.drain();
    assert_eq!(f.wake_count(), 0);
    StateStore::open(&f.state)
        .unwrap()
        .transaction(|tx| tx.set_worktree_paused(&ns, false, &by))
        .unwrap();
    f.sessions
        .lock()
        .unwrap()
        .insert("lead".into(), "replacement-lead".into());
    f.drain();
    assert_eq!(f.wake_count(), 0);
    assert_eq!(f.attention_count(), 1);
    f.sessions.lock().unwrap().remove("lead");
    f.settle_lead_inputs();
    f.drain();
    assert_eq!(
        f.wake_count(),
        1,
        "idle wait on completed dependency cannot suppress continuation"
    );
    let disabled = Fixture::new();
    let id = disabled.begin();
    let request = disabled.send(&id, "disabled-first", "worker");
    let reply = disabled.reply(
        &request,
        "worker",
        "disabled-result",
        "needs_continuation",
        None,
    );
    disabled.status("lead", "idle");
    disabled.drain();
    let ns = disabled.ns();
    let by = MutationAttribution::new(ActorKind::Human, "synthetic-disable", 100);
    StateStore::open(&disabled.state)
        .unwrap()
        .transaction(|tx| {
            tx.set_address_collaboration_disabled(&ns, &PaneAddressId::new("lead"), true, &by)
                .map(|_| ())
        })
        .unwrap();
    disabled.complete(&disabled.attempt(&reply["message_id"], "lead"));
    disabled.drain();
    assert_eq!(disabled.wake_count(), 0);
    assert_eq!(disabled.attention_count(), 1);
    StateStore::open(&disabled.state)
        .unwrap()
        .transaction(|tx| {
            tx.set_address_collaboration_disabled(&ns, &PaneAddressId::new("lead"), false, &by)
                .map(|_| ())
        })
        .unwrap();
    disabled.drain();
    assert_eq!(disabled.wake_count(), 1);

    let active = Fixture::new();
    let id = active.begin();
    let request = active.send(&id, "active-first", "worker");
    active.reply(
        &request,
        "worker",
        "active-progress",
        "needs_continuation",
        None,
    );
    active.status("worker", "working");
    active.status("lead", "idle");
    active.settle_lead_inputs();
    assert_eq!(
        active.wake_count(),
        0,
        "a worker actively continuing its work defers the lead wake"
    );
    active.status("worker", "idle");
    active.drain();
    assert_eq!(
        active.wake_count(),
        1,
        "an idle continuation chain must surface to the lead"
    );

    let paused = Fixture::new();
    let id = paused.begin();
    paused.status("worker", "working");
    let assignment = paused.send(&id, "paused-work", "worker");
    let thread = ThreadId::new(assignment["thread_id"].as_str().unwrap());
    let by = MutationAttribution::new(ActorKind::Human, "synthetic-pause", 100);
    StateStore::open(&paused.state)
        .unwrap()
        .transaction(|tx| tx.pause_thread(&paused.ns(), &thread, &by).map(|_| ()))
        .unwrap();
    paused.status("worker", "idle");
    paused.status("lead", "idle");
    paused.drain();
    assert_eq!(
        paused.wake_count(),
        0,
        "paused assignment must not spawn a new thread wake"
    );
    assert_eq!(paused.attention_count(), 1);
    StateStore::open(&paused.state)
        .unwrap()
        .transaction(|tx| tx.resume_thread(&paused.ns(), &thread, &by).map(|_| ()))
        .unwrap();
    paused.drain();
    assert_eq!(paused.wake_count(), 0);
    assert_eq!(paused.attention_count(), 0);
    assert!(
        !paused
            .attempt(&assignment["message_id"], "worker")
            .is_empty(),
        "explicit resume dispatches the original assignment"
    );

    let inbox = Fixture::new();
    let id = inbox.begin();
    let assignment = inbox.send(&id, "paused-return", "worker");
    let reply = inbox.reply(
        &assignment,
        "worker",
        "paused-reply",
        "needs_continuation",
        None,
    );
    let thread = ThreadId::new(reply["thread_id"].as_str().unwrap());
    StateStore::open(&inbox.state)
        .unwrap()
        .transaction(|tx| tx.pause_thread(&inbox.ns(), &thread, &by).map(|_| ()))
        .unwrap();
    inbox.status("lead", "idle");
    inbox.drain();
    assert_eq!(inbox.wake_count(), 0);
    assert_eq!(
        inbox.attention_count(),
        1,
        "paused lead FIFO must remain visible"
    );
    StateStore::open(&inbox.state)
        .unwrap()
        .transaction(|tx| tx.resume_thread(&inbox.ns(), &thread, &by).map(|_| ()))
        .unwrap();
    inbox.drain();
    inbox.complete(&inbox.attempt(&reply["message_id"], "lead"));
    inbox.drain();
    assert_eq!(
        inbox.wake_count(),
        1,
        "explicit resume restores the normal lead continuation path"
    );
}

#[test]
fn coordination_restart_and_race_controls() {
    let f = Fixture::new();
    let id = f.begin();
    let a = f.send(&id, "first", "worker");
    let reply = f.reply(&a, "worker", "first-result", "needs_continuation", None);
    f.status("lead", "idle");
    f.drain();
    // complete first resolves its own session, then drain takes two live snapshots.
    let next = f.pane_lists.load(Ordering::Relaxed) + 3;
    f.lead_working_after.store(next, Ordering::Relaxed);
    f.complete(&f.attempt(&reply["message_id"], "lead"));
    assert_eq!(f.wake_count(), 1);
    assert!(
        f.nudge_prompts().is_empty(),
        "second live snapshot became working"
    );
    let saved = f.handoff("handoff.results", json!({"coordination_id":id}));
    assert_eq!(saved["wake_revision"], saved["revision"]);
    f.sessions
        .lock()
        .unwrap()
        .insert("lead".into(), "replacement-lead".into());
    f.drain();
    assert_eq!(f.nudge_prompts().len(), 0);
    assert_eq!(f.attention_count(), 1);
    f.sessions.lock().unwrap().remove("lead");
    // A new owner action advances the semantic revision while the old wake is queued.
    f.send(&id, "second", "reviewer");
    f.drain();
    let state: String = f
        .db()
        .query_row(
            "SELECT state FROM mailbox_deliveries WHERE message_id=?1",
            [saved["wake_message_id"].as_str().unwrap()],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(state, "cancelled");
    assert!(f.nudge_prompts().is_empty());
    f.lead_working_after.store(u64::MAX, Ordering::Relaxed);
    f.status("lead", "idle");
    f.drain();
    assert_eq!(
        f.wake_count(),
        1,
        "new pending worker remains a real dependency"
    );
    drop(StateStore::open(&f.state).unwrap());
    f.drain();
    assert_eq!(f.wake_count(), 1);

    // A proven pre-invocation retry is queued and safe to cancel when the
    // semantic revision advances; the earlier failed attempt remains in history.
    let retry = Fixture::new();
    let retry_id = retry.begin();
    let request = retry.send(&retry_id, "retry-first", "worker");
    let reply = retry.reply(
        &request,
        "worker",
        "retry-result",
        "needs_continuation",
        None,
    );
    retry.status("lead", "idle");
    retry.drain();
    *retry.fail_next_nudge.lock().unwrap() = true;
    retry.complete(&retry.attempt(&reply["message_id"], "lead"));
    let old =
        retry.handoff("handoff.results", json!({"coordination_id":retry_id}))["wake_message_id"]
            .as_str()
            .unwrap()
            .to_owned();
    let queued: String = retry
        .db()
        .query_row(
            "SELECT state FROM mailbox_deliveries WHERE message_id=?1",
            [&old],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(queued, "queued");
    retry.status("lead", "working");
    let resolved=retry.handoff("handoff.resolve",json!({"request_id":"retry-resolve","coordination_id":retry_id,"outcome":"blocked","report":"Synthetic resolution while wake queued.","reason":"Synthetic blocker."}));
    assert_eq!(resolved["report_saved"], true);
    retry.drain();
    let cancelled: String = retry
        .db()
        .query_row(
            "SELECT state FROM mailbox_deliveries WHERE message_id=?1",
            [&old],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(cancelled, "cancelled");

    let worker = Fixture::new();
    let id = worker.begin();
    let request = worker.send(&id, "worker-race", "worker");
    let reply = worker.reply(
        &request,
        "worker",
        "worker-race-result",
        "needs_continuation",
        None,
    );
    worker.status("lead", "idle");
    worker.drain();
    worker.worker_working_after.store(
        worker.pane_lists.load(Ordering::Relaxed) + 3,
        Ordering::Relaxed,
    );
    worker.complete(&worker.attempt(&reply["message_id"], "lead"));
    assert_eq!(worker.wake_count(), 1);
    assert!(
        worker.nudge_prompts().is_empty(),
        "worker activity in the second snapshot prevents lease"
    );
    worker
        .worker_working_after
        .store(u64::MAX, Ordering::Relaxed);
    worker.drain();
    assert_eq!(
        worker.nudge_prompts().len(),
        1,
        "a later idle observation resumes the same wake"
    );

    let pause_race = Fixture::new();
    let id = pause_race.begin();
    let request = pause_race.send(&id, "pause-race", "worker");
    let reply = pause_race.reply(
        &request,
        "worker",
        "pause-race-result",
        "needs_continuation",
        None,
    );
    pause_race.status("lead", "idle");
    pause_race.drain();
    let thread = ThreadId::new(request["thread_id"].as_str().unwrap());
    *pause_race.pause_on_wait_thread.lock().unwrap() = Some(thread.clone());
    pause_race.complete(&pause_race.attempt(&reply["message_id"], "lead"));
    assert_eq!(pause_race.wake_count(), 1);
    assert!(
        pause_race.nudge_prompts().is_empty(),
        "pause between snapshots prevents the lease"
    );
    StateStore::open(&pause_race.state)
        .unwrap()
        .transaction(|tx| {
            tx.resume_thread(
                &pause_race.ns(),
                &thread,
                &MutationAttribution::new(ActorKind::Human, "synthetic-resume", 100),
            )
            .map(|_| ())
        })
        .unwrap();
    pause_race.drain();
    assert_eq!(pause_race.nudge_prompts().len(), 1);
}

#[test]
fn a_human_can_acknowledge_a_resolved_warning_without_rewriting_its_history() {
    use clap::Parser;
    let cli = namedrop::cli::Cli::try_parse_from(["namedrop", "handoff", "acknowledge"]).unwrap();
    assert_eq!(cli.operation(), "handoff.acknowledge");
    assert!(namedrop::is_control_operation("handoff.acknowledge"));
    for outcome in ["blocked", "cancelled"] {
        let f = Fixture::new();
        let id = f.begin();
        let resolved = f.handoff("handoff.resolve", json!({"request_id":"resolve", "coordination_id":id,
            "outcome":outcome, "report":"Original report, retained verbatim.", "reason":"Original reason."}));
        f.status("lead", "idle");
        f.drain();
        f.complete(&f.attempt(&resolved["report_message_id"], "lead"));
        let before = f.handoff("handoff.results", json!({"coordination_id":id}));
        assert_eq!(before["report_delivery_state"], "settled");
        assert_eq!(f.attention_count(), 1);
        let input = json!({"coordination_id":id, "confirmed":true});
        let mut unconfirmed = input.clone();
        unconfirmed["confirmed"] = json!(false);
        assert!(
            execute_control_operation("handoff.acknowledge", unconfirmed, &f.runtime("lead"))
                .unwrap_err()
                .contains("confirmation")
        );
        assert_eq!(f.attention_count(), 1);
        let result =
            execute_control_operation("handoff.acknowledge", input.clone(), &f.runtime("lead"))
                .unwrap();
        assert_eq!(result["acknowledged"], true);
        assert_eq!(result["changed"], true);
        for _ in 0..2 {
            f.drain();
            assert_eq!(
                f.attention_count(),
                0,
                "reconciliation must honor the acknowledgement"
            );
        }
        assert!(
            !diagnose_collaboration(&f.runtime("lead"))
                .issues
                .iter()
                .any(|i| i.code == "ND-COORDINATION-001")
        );
        let repeated =
            execute_control_operation("handoff.acknowledge", input, &f.runtime("lead")).unwrap();
        assert_eq!(repeated["changed"], false);
        assert_eq!(
            f.handoff("handoff.results", json!({"coordination_id":id})),
            before
        );
        let count: i64 = f.db().query_row("SELECT count(*) FROM transition_facts WHERE entity_kind='attention_item' AND transition_kind='acknowledged' AND actor_kind='human'", [], |r| r.get(0)).unwrap();
        assert_eq!(
            count, 1,
            "one durable human acknowledgement, even after retry"
        );
    }
}

#[test]
fn a_human_can_acknowledge_an_open_handoff_whose_lead_is_retired() {
    let f = Fixture::new();
    let id = f.begin();
    let request = f.send(&id, "retired-lead-work", "worker");
    let reply = f.reply(&request, "worker", "retired-lead-result", "completed", None);
    f.status("lead", "idle");
    f.drain();
    f.status("lead", "working");
    f.complete(&f.attempt(&reply["message_id"], "lead"));
    f.settle_lead_inputs();
    let by = MutationAttribution::new(ActorKind::Human, "synthetic-retirement", 100);
    StateStore::open(&f.state)
        .unwrap()
        .transaction(|tx| tx.retire_pane_address(&f.ns(), &PaneAddressId::new("lead"), &by))
        .unwrap();
    f.status("lead", "idle");
    f.drain();
    assert_eq!(f.attention_count(), 1);
    assert!(
        diagnose_collaboration(&f.runtime("lead"))
            .issues
            .iter()
            .any(|i| i.code == "ND-COORDINATION-001")
    );
    let history = || -> Vec<String> {
        let db = f.db();
        let coordination: String = db.query_row(
            "SELECT json_array(outcome,revision,report,reason,report_message_id,wake_revision,wake_message_id) FROM coordinations WHERE coordination_id=?1",
            [id.as_str().unwrap()], |r| r.get(0)).unwrap();
        let mut statement = db
            .prepare("SELECT delivery_id||':'||state FROM mailbox_deliveries ORDER BY delivery_id")
            .unwrap();
        let mut rows: Vec<String> = statement
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        rows.insert(0, coordination);
        rows
    };
    let before = history();
    assert!(before[0].starts_with("[\"open\""), "{}", before[0]);

    let input = json!({"coordination_id":id, "confirmed":true});
    let mut unconfirmed = input.clone();
    unconfirmed["confirmed"] = json!(false);
    assert!(
        execute_control_operation("handoff.acknowledge", unconfirmed, &f.runtime("lead"))
            .unwrap_err()
            .contains("confirmation")
    );
    assert_eq!(f.attention_count(), 1);
    let agent = MutationAttribution::new(ActorKind::AgentSession, "synthetic-agent", 100);
    let refused = StateStore::open(&f.state)
        .unwrap()
        .transaction(|tx| {
            tx.acknowledge_coordination_attention(
                &f.ns(),
                &CoordinationId::new(id.as_str().unwrap()),
                &agent,
            )
        })
        .unwrap_err();
    assert!(refused.to_string().contains("only a human"), "{refused}");
    assert_eq!(f.attention_count(), 1);

    let result =
        execute_control_operation("handoff.acknowledge", input.clone(), &f.runtime("lead"))
            .unwrap();
    assert_eq!(result["acknowledged"], true);
    assert_eq!(result["changed"], true);
    for _ in 0..2 {
        f.drain();
        assert_eq!(
            f.attention_count(),
            0,
            "reconciliation must honor the acknowledgement of a retired lead"
        );
    }
    assert!(
        !diagnose_collaboration(&f.runtime("lead"))
            .issues
            .iter()
            .any(|i| i.code == "ND-COORDINATION-001")
    );
    assert_eq!(
        history(),
        before,
        "acknowledgement must not rewrite the handoff"
    );
    let repeated =
        execute_control_operation("handoff.acknowledge", input, &f.runtime("lead")).unwrap();
    assert_eq!(repeated["changed"], false);
    let count: i64 = f.db().query_row("SELECT count(*) FROM transition_facts WHERE entity_kind='attention_item' AND transition_kind='acknowledged' AND actor_kind='human'", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1);
}

#[test]
fn acknowledgement_refuses_a_retired_lead_with_unfinished_worker_delivery() {
    let f = Fixture::new();
    let id = f.begin();
    let request = f.send(&id, "unfinished-worker-work", "worker");
    let by = MutationAttribution::new(ActorKind::Human, "synthetic-retirement", 100);
    StateStore::open(&f.state)
        .unwrap()
        .transaction(|tx| tx.retire_pane_address(&f.ns(), &PaneAddressId::new("lead"), &by))
        .unwrap();
    f.status("lead", "idle");
    f.drain();
    assert_ne!(f.delivery(&request["message_id"], "worker").1, "settled");
    assert_eq!(f.attention_count(), 1);
    assert!(
        execute_control_operation(
            "handoff.acknowledge",
            json!({"coordination_id":id, "confirmed":true}),
            &f.runtime("lead")
        )
        .unwrap_err()
        .contains("unfinished")
    );
    f.drain();
    assert_eq!(f.attention_count(), 1);
}

#[test]
fn acknowledgement_refuses_open_foreign_and_unfinished_handoffs() {
    let f = Fixture::new();
    let id = f.begin();
    let input = json!({"coordination_id":id, "confirmed":true});
    assert!(
        execute_control_operation("handoff.acknowledge", input.clone(), &f.runtime("lead"))
            .unwrap_err()
            .contains("blocked or cancelled")
    );
    let mut foreign = f.runtime("lead");
    foreign.session_socket_id = "other-socket";
    assert!(
        execute_control_operation("handoff.acknowledge", input.clone(), &foreign)
            .unwrap_err()
            .contains("this workspace")
    );
    let _request = f.send(&id, "still-working", "worker");
    let resolved = f.handoff(
        "handoff.resolve",
        json!({"request_id":"resolve", "coordination_id":id,
        "outcome":"blocked", "report":"Still waiting on a worker.", "reason":"Worker unfinished."}),
    );
    assert!(
        execute_control_operation("handoff.acknowledge", input.clone(), &f.runtime("lead"))
            .unwrap_err()
            .contains("unfinished")
    );
    f.status("lead", "idle");
    f.drain();
    f.complete(&f.attempt(&resolved["report_message_id"], "lead"));
    assert_eq!(
        f.handoff("handoff.results", json!({"coordination_id":id}))["report_delivery_state"],
        "settled"
    );
    assert!(
        execute_control_operation("handoff.acknowledge", input, &f.runtime("lead"))
            .unwrap_err()
            .contains("unfinished"),
        "settling the report cannot hide unfinished worker delivery"
    );
    assert_eq!(f.attention_count(), 1);
    f.sessions
        .lock()
        .unwrap()
        .insert("worker".into(), "replacement-worker".into());
    f.drain();
    assert_eq!(f.delivery(&_request["message_id"], "worker").1, "held");
    assert!(
        execute_control_operation(
            "handoff.acknowledge",
            json!({"coordination_id":id, "confirmed":true}),
            &f.runtime("lead")
        )
        .unwrap_err()
        .contains("unfinished"),
        "held work still needs a human decision of its own"
    );
}

#[test]
fn a_human_can_acknowledge_a_completed_handoff_whose_report_delivery_was_cancelled() {
    let f = Fixture::new();
    let id = f.begin();
    let request = f.send(&id, "cancelled-report-work", "worker");
    f.reply(
        &request,
        "worker",
        "cancelled-report-result",
        "completed",
        None,
    );
    let resolved = f.handoff(
        "handoff.resolve",
        json!({
            "request_id":"cancelled-report-resolve",
            "coordination_id":id,
            "outcome":"completed",
            "report":"Synthetic completed report.",
            "reconciled_assignments":[{
                "assignment_id":request["coordination"][0]["assignment_id"],
                "revision":request["coordination"][0]["revision"]
            }]
        }),
    );
    f.status("lead", "idle");
    f.drain();
    let attempt = f.attempt(&resolved["report_message_id"], "lead");
    StateStore::open(&f.state)
        .unwrap()
        .transaction(|tx| {
            tx.hold_delivery_attempt(
                &f.ns(),
                &DeliveryAttemptId::new(&attempt),
                &MutationAttribution::new(ActorKind::Plugin, "synthetic-host", 100),
            )
        })
        .unwrap();
    let cancelled = execute_control_operation(
        "delivery.cancel-held",
        json!({"attempt_id":attempt, "confirmed":true}),
        &f.runtime("lead"),
    )
    .unwrap();
    assert_eq!(cancelled["state"], "cancelled");
    f.drain();
    assert_eq!(f.attention_count(), 1);
    assert!(
        f.attention_summary()
            .starts_with("The saved coordination report's delivery is cancelled"),
        "{}",
        f.attention_summary()
    );
    assert!(
        diagnose_collaboration(&f.runtime("lead"))
            .issues
            .iter()
            .any(|i| i.code == "ND-COORDINATION-001")
    );
    let history = || -> Vec<String> {
        let db = f.db();
        let coordination: String = db.query_row(
            "SELECT json_array(outcome,revision,report,reason,report_message_id,wake_revision,wake_message_id) FROM coordinations WHERE coordination_id=?1",
            [id.as_str().unwrap()], |r| r.get(0)).unwrap();
        let mut statement = db
            .prepare("SELECT delivery_id||':'||state FROM mailbox_deliveries ORDER BY delivery_id")
            .unwrap();
        let mut rows: Vec<String> = statement
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        rows.insert(0, coordination);
        rows
    };
    let before = history();

    let input = json!({"coordination_id":id, "confirmed":true});
    let mut unconfirmed = input.clone();
    unconfirmed["confirmed"] = json!(false);
    assert!(
        execute_control_operation("handoff.acknowledge", unconfirmed, &f.runtime("lead"))
            .unwrap_err()
            .contains("confirmation")
    );
    assert_eq!(f.attention_count(), 1);
    let result =
        execute_control_operation("handoff.acknowledge", input.clone(), &f.runtime("lead"))
            .expect("a completed handoff whose report delivery was cancelled can be acknowledged");
    assert_eq!(result["acknowledged"], true);
    assert_eq!(result["changed"], true);
    assert_eq!(f.attention_count(), 0);
    for _ in 0..2 {
        f.drain();
        assert_eq!(
            f.attention_count(),
            0,
            "reconciliation must honor the acknowledgement of a cancelled report delivery"
        );
    }
    assert!(
        !diagnose_collaboration(&f.runtime("lead"))
            .issues
            .iter()
            .any(|i| i.code == "ND-COORDINATION-001")
    );
    assert_eq!(
        history(),
        before,
        "acknowledgement must not rewrite the handoff"
    );
    let results = f.handoff("handoff.results", json!({"coordination_id":id}));
    assert_eq!(results["outcome"], "completed");
    assert_eq!(results["report"], "Synthetic completed report.");
    assert_eq!(results["report_delivery_state"], "cancelled");
    let repeated =
        execute_control_operation("handoff.acknowledge", input, &f.runtime("lead")).unwrap();
    assert_eq!(repeated["changed"], false);
    let count: i64 = f.db().query_row("SELECT count(*) FROM transition_facts WHERE entity_kind='attention_item' AND transition_kind='acknowledged' AND actor_kind='human'", [], |r| r.get(0)).unwrap();
    assert_eq!(count, 1);
}

#[test]
fn acknowledgement_refuses_a_completed_handoff_whose_report_delivery_is_unfinished() {
    let f = Fixture::new();
    let id = f.begin();
    let request = f.send(&id, "unfinished-report-work", "worker");
    f.reply(
        &request,
        "worker",
        "unfinished-report-result",
        "completed",
        None,
    );
    let resolved = f.handoff(
        "handoff.resolve",
        json!({
            "request_id":"unfinished-report-resolve",
            "coordination_id":id,
            "outcome":"completed",
            "report":"Synthetic completed report.",
            "reconciled_assignments":[{
                "assignment_id":request["coordination"][0]["assignment_id"],
                "revision":request["coordination"][0]["revision"]
            }]
        }),
    );
    let refusal = "only a human may acknowledge a blocked or cancelled coordination, an open one \
                   whose lead address is retired, or a completed one whose report delivery was \
                   cancelled";
    let input = json!({"coordination_id":id, "confirmed":true});
    let acknowledge =
        || execute_control_operation("handoff.acknowledge", input.clone(), &f.runtime("lead"));
    assert_eq!(
        f.delivery(&resolved["report_message_id"], "lead").1,
        "queued"
    );
    assert_eq!(acknowledge().unwrap_err(), refusal);

    f.status("lead", "idle");
    f.drain();
    let attempt = f.attempt(&resolved["report_message_id"], "lead");
    assert_eq!(
        f.delivery(&resolved["report_message_id"], "lead").1,
        "acknowledged",
        "the fake host receives the report, which is in flight rather than settled"
    );
    assert_eq!(acknowledge().unwrap_err(), refusal);

    StateStore::open(&f.state)
        .unwrap()
        .transaction(|tx| {
            tx.hold_delivery_attempt(
                &f.ns(),
                &DeliveryAttemptId::new(&attempt),
                &MutationAttribution::new(ActorKind::Plugin, "synthetic-host", 100),
            )
        })
        .unwrap();
    f.drain();
    assert_eq!(f.delivery(&resolved["report_message_id"], "lead").1, "held");
    assert_eq!(acknowledge().unwrap_err(), refusal);
    assert_eq!(f.attention_count(), 1);
}

#[test]
fn coordination_report_visibility_and_empty_turn() {
    let empty = Fixture::new();
    let empty_id = empty.begin();
    empty.status("lead", "idle");
    empty.drain();
    assert_eq!(empty.wake_count(), 0);
    assert_eq!(empty.attention_count(), 1);
    let empty_report=empty.handoff("handoff.resolve",json!({"request_id":"empty-blocked","coordination_id":empty_id,"outcome":"blocked","report":"No work was dispatched; the synthetic task is blocked.","reason":"Synthetic input missing."}));
    assert_eq!(empty_report["report_saved"], true);
    assert_eq!(empty_report["presentation"], "unknown");
    let f = Fixture::new();
    let id = f.begin();
    let a = f.send(&id, "first", "worker");
    let worker_reply = f.reply(&a, "worker", "first-result", "needs_continuation", None);
    let resolved=f.handoff("handoff.resolve",json!({"request_id":"blocked-report","coordination_id":id,"outcome":"blocked","report":"Synthetic report with a concrete blocker.","reason":"Synthetic worker needs another review."}));
    assert_eq!(resolved["report_saved"], true);
    assert_eq!(resolved["presentation"], "unknown");
    assert!(resolved["report_message_id"].is_string());
    let saved = f.handoff("handoff.results", json!({"coordination_id":id}));
    assert_eq!(saved["report"], "Synthetic report with a concrete blocker.");
    assert_eq!(saved["report_delivery_settled"], false);
    assert_eq!(f.attention_count(), 1);
    f.status("lead", "idle");
    f.drain();
    f.complete(&f.attempt(&worker_reply["message_id"], "lead"));
    f.drain();
    let prompt = f
        .prompts
        .lock()
        .unwrap()
        .iter()
        .find(|(_, p)| p["control"]["message_id"] == saved["report_message_id"])
        .unwrap()
        .1
        .clone();
    assert_eq!(prompt["control"]["reply_mode"], "report_to_human");
    assert_eq!(
        prompt["control"]["tool"]["operations"][0]["token"],
        "complete.no-reply"
    );
    f.complete(prompt["control"]["attempt_id"].as_str().unwrap());
    let after = f.handoff("handoff.results", json!({"coordination_id":id}));
    assert_eq!(after["report_delivery_state"], "settled");
    assert_eq!(after["report_delivery_settled"], true);
    assert_eq!(after["presentation"], "unknown");
    assert_eq!(f.attention_count(), 1);
}

#[test]
fn coordination_retired_participant_recovery() {
    let lead_lost = Fixture::new();
    let id = lead_lost.begin();
    let request = lead_lost.send(&id, "retired-lead-work", "worker");
    let reply = lead_lost.reply(&request, "worker", "retired-lead-result", "completed", None);
    lead_lost.status("lead", "idle");
    lead_lost.drain();
    lead_lost.status("lead", "working");
    lead_lost.complete(&lead_lost.attempt(&reply["message_id"], "lead"));
    lead_lost.settle_lead_inputs();
    let by = MutationAttribution::new(ActorKind::Human, "synthetic-retirement", 100);
    StateStore::open(&lead_lost.state)
        .unwrap()
        .transaction(|tx| tx.retire_pane_address(&lead_lost.ns(), &PaneAddressId::new("lead"), &by))
        .unwrap();
    lead_lost.status("lead", "idle");
    let unrelated = execute_message_operation(
        "send",
        json!({"request_id":"unrelated-live-work","source":"@worker Independent synthetic request","author_kind":"agent_session"}),
        &lead_lost.runtime("reviewer"),
    )
    .unwrap();
    lead_lost.drain();
    assert_eq!(lead_lost.wake_count(), 0);
    assert_eq!(
        lead_lost.attention_count(),
        1,
        "retired lead needs one owner-loss attention"
    );
    assert!(
        lead_lost
            .prompts
            .lock()
            .unwrap()
            .iter()
            .any(|(pane, envelope)| {
                pane == "worker" && envelope["control"]["message_id"] == unrelated["message_id"]
            }),
        "retired coordination must not block unrelated live dispatch"
    );

    let worker_lost = Fixture::new();
    let id = worker_lost.begin();
    let request = worker_lost.send(&id, "retired-worker-work", "worker");
    let reply = worker_lost.reply(&request, "worker", "worker-completed", "completed", None);
    StateStore::open(&worker_lost.state)
        .unwrap()
        .transaction(|tx| {
            tx.retire_pane_address(&worker_lost.ns(), &PaneAddressId::new("worker"), &by)
        })
        .unwrap();
    worker_lost.status("lead", "idle");
    worker_lost.drain();
    worker_lost.complete(&worker_lost.attempt(&reply["message_id"], "lead"));
    worker_lost.drain();
    let wake = worker_lost
        .nudge_prompts()
        .pop()
        .expect("lead continuation");
    let operations = wake["control"]["tool"]["operations"].as_array().unwrap();
    assert!(
        !operations
            .iter()
            .any(|op| op["token"] == "send" && op["input"].get("assignment_id").is_some()),
        "retired worker has no continuation target"
    );
    assert!(
        operations
            .iter()
            .any(|op| op["token"] == "send" && op["input"].get("assignment_id").is_none()),
        "a retired worker does not prevent new assignments"
    );
    let results = operations
        .iter()
        .find(|op| op["token"] == "handoff.results")
        .unwrap()["input"]
        .clone();
    let saved = worker_lost.handoff("handoff.results", results);
    assert_eq!(saved["assignments"][0]["work_outcome"], "completed");
    let mut resolve = operations
        .iter()
        .find(|op| op["token"] == "handoff.resolve" && op["input"]["outcome"] == "completed")
        .unwrap()["input"]
        .clone();
    assert_eq!(
        resolve["reconciled_assignments"][0]["assignment_id"],
        request["coordination"][0]["assignment_id"]
    );
    resolve["report"] = json!("Synthetic completed report after original worker retired.");
    assert_eq!(
        worker_lost.handoff("handoff.resolve", resolve)["report_saved"],
        true
    );
}

#[test]
fn a_stale_wake_is_rendered_stale_in_flight_and_settles_as_no_reply_after_the_grace() {
    let f = Fixture::new();
    let id = f.begin();
    let first = f.send(&id, "historical-first", "worker");
    let first_reply = f.reply(
        &first,
        "worker",
        "historical-progress",
        "needs_continuation",
        None,
    );
    f.status("lead", "idle");
    f.drain();
    f.complete(&f.attempt(&first_reply["message_id"], "lead"));
    let old = f.nudge_prompts().pop().expect("old wake");
    let old_attempt = old["control"]["attempt_id"].as_str().unwrap().to_owned();
    let old_operations = old["control"]["tool"]["operations"].as_array().unwrap();
    let mut send = old_operations
        .iter()
        .find(|op| op["token"] == "send")
        .unwrap()["input"]
        .clone();
    send["source"] = json!("@worker Synthetic second round");
    let second = execute_message_operation("send", send, &f.runtime("lead")).unwrap();
    f.status("worker", "working");

    f.now.store(10_100, Ordering::Relaxed);
    f.drain();
    assert_eq!(
        f.attempt_state(&old_attempt),
        "acknowledged",
        "inside the grace the stale wake is still in flight"
    );
    let inbox =
        execute_integration_operation("delivery.inbox", json!({}), &f.runtime("lead")).unwrap();
    let stale = inbox["envelopes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["control"]["attempt_id"] == old_attempt)
        .expect("stale wake in flight");
    assert_eq!(stale["control"]["coordination_id"], id);
    assert_eq!(stale["control"]["coordination_nudge_stale"], true);
    let stale_operations = stale["control"]["tool"]["operations"].as_array().unwrap();
    assert!(
        stale_operations
            .iter()
            .any(|op| op["token"] == "handoff.results")
    );
    assert!(stale_operations.iter().all(|op| !matches!(
        op["token"].as_str(),
        Some("send" | "handoff.resolve" | "address.list" | "handoff.preview")
    )));
    assert!(
        stale["control"]["permitted_operations"]
            .as_array()
            .unwrap()
            .iter()
            .all(|op| !matches!(
                op.as_str(),
                Some("send" | "handoff.resolve" | "address.list" | "handoff.preview")
            ))
    );
    assert!(
        stale["trusted_instruction"]
            .as_str()
            .unwrap()
            .contains("coordination_nudge_stale true")
    );

    f.now.store(60_100, Ordering::Relaxed);
    f.drain();
    assert_eq!(f.attempt_state(&old_attempt), "settled");
    assert_eq!(f.disposition(&old_attempt).as_deref(), Some("no_reply"));
    assert!(
        f.unresolved_attention()
            .iter()
            .all(|(kind, _)| kind != "agent_disposition"),
        "a settled stale wake must not raise ND-DELIVERY-003: {:?}",
        f.unresolved_attention()
    );
    let inbox =
        execute_integration_operation("delivery.inbox", json!({}), &f.runtime("lead")).unwrap();
    assert!(
        inbox["envelopes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|e| e["control"]["attempt_id"] != old_attempt),
        "a settled stale wake leaves the inbox"
    );

    let second_reply = f.reply(&second, "worker", "historical-done", "completed", None);
    f.status("worker", "idle");
    let mut completed_second_reply = false;
    for _ in 0..8 {
        f.drain();
        let inbox =
            execute_integration_operation("delivery.inbox", json!({}), &f.runtime("lead")).unwrap();
        for envelope in inbox["envelopes"].as_array().unwrap() {
            if envelope["control"]["coordination_nudge"] == true {
                continue;
            }
            completed_second_reply |=
                envelope["control"]["message_id"] == second_reply["message_id"];
            f.complete(envelope["control"]["attempt_id"].as_str().unwrap());
        }
        if f.wake_count() == 2 {
            break;
        }
    }
    assert!(
        completed_second_reply,
        "second worker reply reached the original lead"
    );
    assert_eq!(f.wake_count(), 2);
    let current = f.nudge_prompts().pop().expect("current wake");
    let inbox =
        execute_integration_operation("delivery.inbox", json!({}), &f.runtime("lead")).unwrap();
    let envelopes = inbox["envelopes"].as_array().unwrap();
    let latest = envelopes
        .iter()
        .find(|e| e["control"]["attempt_id"] == current["control"]["attempt_id"])
        .expect("current obligation");
    assert_eq!(latest["control"]["coordination_nudge_stale"], false);
    assert!(
        latest["control"]["tool"]["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|op| op["token"] == "handoff.resolve")
    );
    assert!(
        envelopes
            .iter()
            .all(|e| e["control"]["attempt_id"] != old_attempt)
    );

    let before = f.handoff("handoff.results", json!({"coordination_id":id}));
    f.sessions
        .lock()
        .unwrap()
        .insert("lead".into(), "replacement-lead".into());
    assert!(
        execute_integration_operation(
            "complete",
            json!({"attempt_id":old_attempt}),
            &f.runtime("lead")
        )
        .is_err(),
        "replacement session cannot settle the historical attempt"
    );
    f.sessions.lock().unwrap().remove("lead");
    f.complete(&old_attempt);
    let after = f.handoff("handoff.results", json!({"coordination_id":id}));
    assert_eq!(after["revision"], before["revision"]);
    assert_eq!(after["wake_message_id"], before["wake_message_id"]);
    assert_eq!(f.wake_count(), 2);
    let remaining =
        execute_integration_operation("delivery.inbox", json!({}), &f.runtime("lead")).unwrap();
    assert!(
        remaining["envelopes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["control"]["attempt_id"] == current["control"]["attempt_id"])
    );
}

#[test]
fn completed_resolution_retires_all_queued_replies_through_reconciled_revision() {
    let f = Fixture::new();
    let id = f.begin();
    let request = f.send(&id, "retire-assignment", "worker");
    let first_reply = f.reply(
        &request,
        "worker",
        "retire-progress",
        "needs_continuation",
        None,
    );
    let continuation = execute_message_operation(
        "send",
        json!({
            "request_id":"retire-continuation",
            "source":"@worker Synthetic continued assignment",
            "author_kind":"agent_session",
            "coordination_id":id,
            "assignment_id":request["coordination"][0]["assignment_id"],
            "expected_revision":request["coordination"][0]["revision"]
        }),
        &f.runtime("lead"),
    )
    .unwrap();
    let reply = f.reply(&continuation, "worker", "retire-result", "completed", None);
    let ordinary = execute_message_operation(
        "send",
        json!({"request_id":"ordinary-note","source":"@lead Synthetic ordinary note","author_kind":"agent_session"}),
        &f.runtime("reviewer"),
    )
    .unwrap();
    let results = f.handoff("handoff.results", json!({"coordination_id":id}));
    assert_eq!(results["assignments"][0]["revision"], 2);
    assert_eq!(results["assignments"][0]["work_outcome"], "completed");
    assert_eq!(
        results["assignments"][0]["last_reply_message_id"],
        reply["message_id"]
    );
    let (first_reply_delivery, first_reply_state) = f.delivery(&first_reply["message_id"], "lead");
    let (reply_delivery, reply_state) = f.delivery(&reply["message_id"], "lead");
    assert_eq!(first_reply_state, "queued");
    assert_eq!(reply_state, "queued");
    assert_eq!(f.delivery(&ordinary["message_id"], "lead").1, "queued");

    let resolve = json!({
        "request_id":"retire-resolve",
        "coordination_id":id,
        "outcome":"completed",
        "report":"Synthetic completed report remains deliverable.",
        "reconciled_assignments":[{
            "assignment_id":continuation["coordination"][0]["assignment_id"],
            "revision":continuation["coordination"][0]["revision"]
        }]
    });
    assert!(
        execute_handoff_operation("handoff.resolve", resolve.clone(), &f.runtime("reviewer"))
            .is_err(),
        "another address cannot retire the owner's mailbox"
    );
    f.sessions
        .lock()
        .unwrap()
        .insert("lead".into(), "replacement-lead".into());
    assert!(
        execute_handoff_operation("handoff.resolve", resolve.clone(), &f.runtime("lead")).is_err(),
        "a replacement session cannot retire the original owner's mailbox"
    );
    assert_eq!(f.delivery(&first_reply["message_id"], "lead").1, "queued");
    assert_eq!(f.delivery(&reply["message_id"], "lead").1, "queued");
    f.sessions.lock().unwrap().remove("lead");

    let resolved = f.handoff("handoff.resolve", resolve.clone());
    let report_message = resolved["report_message_id"].clone();
    assert_eq!(
        f.delivery(&first_reply["message_id"], "lead").1,
        "cancelled"
    );
    assert_eq!(f.delivery(&reply["message_id"], "lead").1, "cancelled");
    assert_eq!(f.delivery(&ordinary["message_id"], "lead").1, "queued");
    assert_eq!(f.delivery(&report_message, "lead").1, "queued");
    let db = f.db();
    for reply in [&first_reply, &reply] {
        assert_eq!(
            db.query_row(
                "SELECT body FROM collaboration_messages WHERE message_id=?1",
                [reply["message_id"].as_str().unwrap()],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "Synthetic worker result"
        );
    }
    let link_count = |reply: &Value, assignment: &Value| {
        db.query_row(
            "SELECT count(*) FROM coordination_links WHERE message_id=?1 AND assignment_id=?2 AND revision=?3 AND kind='reply'",
            rusqlite::params![reply["message_id"].as_str().unwrap(),assignment["assignment_id"].as_str().unwrap(),assignment["revision"].as_i64().unwrap()],
            |r| r.get::<_, i64>(0)
        )
        .unwrap()
    };
    assert_eq!(link_count(&first_reply, &request["coordination"][0]), 1);
    assert_eq!(link_count(&reply, &continuation["coordination"][0]), 1);
    let cancelled = |delivery: &str| {
        f.db()
            .query_row(
                "SELECT count(*) FROM transition_facts WHERE entity_kind='mailbox_delivery' AND entity_id=?1 AND transition_kind='cancelled'",
                [delivery],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
    };
    assert_eq!(cancelled(&first_reply_delivery), 1);
    assert_eq!(cancelled(&reply_delivery), 1);
    let replay = f.handoff("handoff.resolve", resolve);
    assert_eq!(replay["report_message_id"], report_message);
    assert_eq!(
        cancelled(&first_reply_delivery),
        1,
        "idempotent replay adds no audit event"
    );
    assert_eq!(cancelled(&reply_delivery), 1);
    assert_eq!(
        f.db()
            .query_row(
                "SELECT count(*) FROM collaboration_messages WHERE grammar_version='coordination-report/v1'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );

    f.status("lead", "idle");
    f.drain();
    assert!(
        f.prompts
            .lock()
            .unwrap()
            .iter()
            .all(|(_, prompt)| { prompt["control"]["message_id"] != first_reply["message_id"] })
    );
    assert!(
        f.prompts
            .lock()
            .unwrap()
            .iter()
            .all(|(_, prompt)| { prompt["control"]["message_id"] != reply["message_id"] })
    );
    assert!(
        f.prompts
            .lock()
            .unwrap()
            .iter()
            .any(|(_, prompt)| { prompt["control"]["message_id"] == ordinary["message_id"] })
    );
    f.complete(&f.attempt(&ordinary["message_id"], "lead"));
    f.drain();
    assert!(f.prompts.lock().unwrap().iter().any(|(_, prompt)| {
        prompt["control"]["message_id"] == report_message
            && prompt["control"]["reply_mode"] == "report_to_human"
    }));
}

#[test]
fn completed_resolution_preserves_nonqueued_replies() {
    for state in ["submitted", "acknowledged", "held"] {
        let f = Fixture::new();
        let id = f.begin();
        let request = f.send(&id, &format!("{state}-assignment"), "worker");
        f.status("lead", "idle");
        let reply = f.reply(
            &request,
            "worker",
            &format!("{state}-result"),
            "completed",
            None,
        );
        let attempt = f.attempt(&reply["message_id"], "lead");
        if state == "submitted" {
            let db = f.db();
            db.execute(
                "UPDATE delivery_attempt_state SET state='submitted',active=1 WHERE attempt_id=?1",
                [&attempt],
            )
            .unwrap();
            db.execute(
                "UPDATE mailbox_deliveries SET state='submitted' WHERE message_id=?1",
                [reply["message_id"].as_str().unwrap()],
            )
            .unwrap();
        } else if state == "held" {
            StateStore::open(&f.state)
                .unwrap()
                .transaction(|tx| {
                    tx.hold_delivery_attempt(
                        &f.ns(),
                        &DeliveryAttemptId::new(&attempt),
                        &MutationAttribution::new(ActorKind::Plugin, "synthetic-host", 100),
                    )
                })
                .unwrap();
        }
        assert_eq!(f.delivery(&reply["message_id"], "lead").1, state);
        f.status("lead", "working");
        f.handoff(
            "handoff.resolve",
            json!({
                "request_id":format!("{state}-resolve"),
                "coordination_id":id,
                "outcome":"completed",
                "report":format!("Synthetic {state} report remains transport-owned."),
                "reconciled_assignments":[{
                    "assignment_id":request["coordination"][0]["assignment_id"],
                    "revision":request["coordination"][0]["revision"]
                }]
            }),
        );
        assert_eq!(
            f.delivery(&reply["message_id"], "lead").1,
            state,
            "resolve must not settle or cancel an invoked delivery"
        );
    }
}

#[test]
fn noncompleted_resolution_preserves_queued_replies() {
    for outcome in ["blocked", "cancelled"] {
        let f = Fixture::new();
        let id = f.begin();
        let request = f.send(&id, &format!("{outcome}-assignment"), "worker");
        let reply = f.reply(
            &request,
            "worker",
            &format!("{outcome}-result"),
            "completed",
            None,
        );
        assert_eq!(f.delivery(&reply["message_id"], "lead").1, "queued");
        f.handoff(
            "handoff.resolve",
            json!({
                "request_id":format!("{outcome}-resolve"),
                "coordination_id":id,
                "outcome":outcome,
                "report":format!("Synthetic {outcome} final report."),
                "reason":format!("Synthetic {outcome} reason.")
            }),
        );
        assert_eq!(
            f.delivery(&reply["message_id"], "lead").1,
            "queued",
            "non-completed resolution keeps existing delivery semantics"
        );
    }
}

/// Reconciliation used to re-raise an attention item for every non-open
/// coordination that had a saved report, on every run. A finished coordination
/// whose report was saved and delivered therefore warned the human forever,
/// with nothing to act on. Clearing it must not make genuine problems quiet.
#[test]
fn completed_settled_report_clears_its_warning_and_stays_clear() {
    let f = Fixture::new();
    let id = f.begin();
    let request = f.send(&id, "settled-assignment", "worker");
    let reply = f.reply(&request, "worker", "settled-result", "completed", None);
    let resolved = f.handoff(
        "handoff.resolve",
        json!({
            "request_id":"settled-resolve",
            "coordination_id":id,
            "outcome":"completed",
            "report":"Synthetic completed report was delivered.",
            "reconciled_assignments":[{
                "assignment_id":request["coordination"][0]["assignment_id"],
                "revision":request["coordination"][0]["revision"]
            }]
        }),
    );
    assert_eq!(resolved["report_saved"], true);
    assert_eq!(resolved["outcome"], "completed");

    // Before the report delivery settles there is something to look at, and
    // that is the attention item the fix has to clear later rather than never
    // create: it is already persisted by the time settlement happens.
    // A completed resolution retires the exact queued assignment replies, so
    // the saved report is what the lead's mailbox dispatches next.
    let _ = &reply;
    f.status("lead", "idle");
    f.drain();
    assert_eq!(
        f.attention_count(),
        1,
        "an undelivered report is still worth attention"
    );

    let prompt = f
        .prompts
        .lock()
        .unwrap()
        .iter()
        .find(|(_, p)| p["control"]["message_id"] == resolved["report_message_id"])
        .expect("the saved report is delivered to its lead")
        .1
        .clone();
    f.complete(prompt["control"]["attempt_id"].as_str().unwrap());
    let after = f.handoff("handoff.results", json!({"coordination_id":id}));
    assert_eq!(after["report_delivery_state"], "settled");
    assert_eq!(after["report_delivery_settled"], true);

    // Nothing is left to ask a human for, so the stale item is resolved.
    f.drain();
    assert_eq!(
        f.attention_count(),
        0,
        "a completed coordination with a saved, settled report has nothing to act on"
    );
    // And reconciliation must not raise it again on every later run.
    f.drain();
    f.drain();
    assert_eq!(
        f.attention_count(),
        0,
        "repeated reconciliation recreated the warning"
    );

    // Settled transport is not human consumption, and clearing the warning
    // does not claim it is.
    let final_report = f.handoff("handoff.results", json!({"coordination_id":id}));
    assert_eq!(final_report["presentation"], "unknown");
    assert_eq!(
        final_report["report"],
        "Synthetic completed report was delivered."
    );
    assert_eq!(final_report["outcome"], "completed");
    assert_eq!(final_report["assignments"][0]["work_outcome"], "completed");
}

#[test]
fn restarted_lead_adopts_and_continues_its_coordination() {
    let f = Fixture::new();
    let id = f.begin();
    let first = f.send(&id, "first", "worker");
    let pending = f.send(&id, "pending", "reviewer");
    let first_reply = f.reply(&first, "worker", "first-result", "needs_continuation", None);
    f.status("lead", "idle");
    f.drain();
    f.status("lead", "working");
    f.complete(&f.attempt(&first_reply["message_id"], "lead"));
    let (_, _, before_revision) = f.ownership(&id);
    f.replace_session("lead", "replacement-lead");
    f.drain();

    assert_eq!(f.attention_count(), 1);
    let summary = f.attention_summary();
    assert!(summary.contains("handoff adopt"), "{summary}");
    let status = f.handoff("handoff.status", json!({}));
    assert_eq!(status["coordinations"], json!([]));
    assert_eq!(status["adoptable"][0]["coordination_id"], id);
    assert_eq!(status["adoptable"][0]["owner_session_id"], "session-lead");
    let refused = execute_handoff_operation(
        "handoff.results",
        json!({"coordination_id":id}),
        &f.runtime("lead"),
    )
    .unwrap_err();
    assert!(refused.contains("handoff adopt"), "{refused}");

    let adopted = f.adopt("lead", "adopt-1", &id).unwrap();
    assert_eq!(adopted["adoption"]["changed"], true);
    assert_eq!(
        adopted["adoption"]["previous_owner_session_id"],
        "session-lead"
    );
    assert_eq!(adopted["adoption"]["owner_session_id"], "replacement-lead");
    assert_eq!(adopted["owner_session_id"], "replacement-lead");
    assert_eq!(adopted["revision"], before_revision + 1);
    assert_eq!(
        f.attention_count(),
        0,
        "adoption resolves the restart warning without a drain"
    );
    assert_eq!(
        f.fact_count(
            "coordination_ownership",
            &format!("{}:session-lead", id.as_str().unwrap()),
            "adopted",
            "replacement-lead"
        ),
        1
    );
    assert_eq!(
        f.ownership(&id),
        (
            "replacement-lead".to_owned(),
            "adopt-1".to_owned(),
            before_revision + 1
        )
    );

    let results = f.handoff("handoff.results", json!({"coordination_id":id}));
    assert_eq!(results["owner_session_id"], "replacement-lead");
    assert_eq!(
        results["assignments"][0]["work_outcome"],
        "needs_continuation"
    );
    let status = f.handoff("handoff.status", json!({}));
    assert_eq!(status["coordinations"][0]["coordination_id"], id);
    assert_eq!(status["adoptable"], json!([]));

    let progress = f.reply(
        &pending,
        "reviewer",
        "pending-progress",
        "needs_continuation",
        None,
    );
    assert_eq!(
        f.recipient_session(&progress["message_id"], "lead"),
        "replacement-lead"
    );
    let pending_assignment = pending["coordination"][0]["assignment_id"].clone();
    let pending_outcome = |f: &Fixture| {
        f.handoff("handoff.results", json!({"coordination_id":id}))["assignments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|a| a["assignment_id"] == pending_assignment)
            .unwrap()["work_outcome"]
            .clone()
    };
    assert_eq!(pending_outcome(&f), "needs_continuation");
    let pending_continuation = execute_message_operation(
        "send",
        json!({
            "request_id":"pending-2",
            "author_kind":"agent_session",
            "coordination_id":id,
            "assignment_id":pending_assignment,
            "expected_revision":1,
            "source":"@reviewer Continue."
        }),
        &f.runtime("lead"),
    )
    .unwrap();
    assert_eq!(pending_continuation["coordination"][0]["revision"], 2);
    f.reply(
        &pending_continuation,
        "reviewer",
        "pending-done",
        "completed",
        None,
    );
    assert_eq!(pending_outcome(&f), "completed");

    let second = f.send(&id, "second", "reviewer");
    assert_eq!(second["coordination"][0]["revision"], 1);
    let continuation = execute_message_operation(
        "send",
        json!({
            "request_id":"first-2",
            "author_kind":"agent_session",
            "coordination_id":id,
            "assignment_id":first["coordination"][0]["assignment_id"],
            "expected_revision":1,
            "source":"@worker Continue."
        }),
        &f.runtime("lead"),
    )
    .unwrap();
    assert_eq!(continuation["coordination"][0]["revision"], 2);
    f.reply(&continuation, "worker", "first-done", "completed", None);
    f.reply(&second, "reviewer", "second-done", "completed", None);
    let resolved = f.handoff(
        "handoff.resolve",
        json!({
            "request_id":"adopted-resolve",
            "coordination_id":id,
            "outcome":"completed",
            "report":"Synthetic adopted coordination completed.",
            "reconciled_assignments":[
                {"assignment_id":first["coordination"][0]["assignment_id"],"revision":2},
                {"assignment_id":pending["coordination"][0]["assignment_id"],"revision":2},
                {"assignment_id":second["coordination"][0]["assignment_id"],"revision":1}
            ]
        }),
    );
    assert_eq!(resolved["outcome"], "completed");
    assert_eq!(
        f.recipient_session(&resolved["report_message_id"], "lead"),
        "replacement-lead"
    );
}

#[test]
fn adoption_is_idempotent_for_the_same_caller() {
    let f = Fixture::new();
    let id = f.begin();
    let other = f.handoff(
        "handoff.begin",
        json!({"request_id":"shared","deliverable":"A second synthetic deliverable"}),
    )["coordination_id"]
        .clone();
    let (_, _, before_revision) = f.ownership(&id);
    f.replace_session("lead", "replacement-lead");
    assert_eq!(
        f.adopt("lead", "adopt-1", &id).unwrap()["adoption"]["changed"],
        true
    );

    let repeated = f.adopt("lead", "adopt-1", &id).unwrap();
    assert_eq!(repeated["adoption"]["changed"], false);
    assert_eq!(
        repeated["adoption"]["previous_owner_session_id"],
        "session-lead"
    );
    let different = f.adopt("lead", "adopt-other", &id).unwrap_err();
    assert!(
        different.contains("coordination was already adopted with a different request id"),
        "{different}"
    );
    let begun = execute_handoff_operation(
        "handoff.begin",
        json!({"request_id":"adopt-1","deliverable":"A deliverable under the adopt request id"}),
        &f.runtime("lead"),
    )
    .unwrap_err();
    assert!(
        begun.contains("request id was already used to adopt a coordination"),
        "{begun}"
    );
    assert_eq!(
        f.fact_count(
            "coordination_ownership",
            &format!("{}:session-lead", id.as_str().unwrap()),
            "adopted",
            "replacement-lead"
        ),
        1
    );
    assert_eq!(
        f.ownership(&id),
        (
            "replacement-lead".to_owned(),
            "adopt-1".to_owned(),
            before_revision + 1
        )
    );

    f.handoff(
        "handoff.begin",
        json!({"request_id":"shared","deliverable":"The replacement's own deliverable"}),
    );
    let collision = f.adopt("lead", "shared", &other).unwrap_err();
    assert!(
        collision.contains("request id was already used for another coordination"),
        "{collision}"
    );
    assert_eq!(f.ownership(&other).0, "session-lead");

    f.handoff(
        "handoff.resolve",
        json!({"request_id":"adopted-blocked","coordination_id":id,"outcome":"blocked",
            "report":"Synthetic adopted report.","reason":"Synthetic blocker."}),
    );
    assert_eq!(
        f.adopt("lead", "adopt-1", &id).unwrap()["adoption"]["changed"],
        false
    );
}

#[test]
fn superseded_owner_session_cannot_act() {
    let f = Fixture::new();
    let id = f.begin();
    f.send(&id, "first", "worker");
    f.replace_session("lead", "replacement-lead");
    f.adopt("lead", "adopt-1", &id).unwrap();
    f.observe_session("lead", "session-lead");

    let refusals = [
        execute_handoff_operation(
            "handoff.results",
            json!({"coordination_id":id}),
            &f.runtime("lead"),
        ),
        execute_handoff_operation(
            "handoff.resolve",
            json!({"request_id":"stale-resolve","coordination_id":id,"outcome":"blocked",
                "report":"Stale report.","reason":"Stale owner."}),
            &f.runtime("lead"),
        ),
        execute_message_operation(
            "send",
            json!({"request_id":"stale-send","source":"@reviewer stale work",
                "author_kind":"agent_session","coordination_id":id}),
            &f.runtime("lead"),
        ),
    ];
    for refused in refusals {
        let refused = refused.unwrap_err();
        assert!(
            refused.contains("another session of this address"),
            "{refused}"
        );
    }
    let status = f.handoff("handoff.status", json!({}));
    assert_eq!(status["coordinations"], json!([]));
    assert_eq!(
        status["adoptable"][0]["owner_session_id"],
        "replacement-lead"
    );
    assert_eq!(f.ownership(&id).0, "replacement-lead");
    assert_eq!(
        f.fact_count(
            "coordination_ownership",
            &format!("{}:session-lead", id.as_str().unwrap()),
            "adopted",
            "replacement-lead"
        ),
        1
    );
}

#[test]
fn tracked_send_names_the_restarted_owner_not_a_worker() {
    let f = Fixture::new();
    let id = f.begin();
    let first = f.send(&id, "first", "worker");
    f.replace_session("lead", "replacement-lead");
    let worker_error = "worker replies inherit their assignment from the bound message";

    assert_eq!(
        execute_message_operation(
            "send",
            json!({"request_id":"worker-send","source":"@reviewer worker-made assignment",
                "author_kind":"agent_session","coordination_id":id}),
            &f.runtime("worker"),
        )
        .unwrap_err(),
        worker_error
    );
    assert_eq!(
        execute_message_operation(
            "reply",
            json!({"request_id":"redundant-ids","source":"@lead Synthetic worker result",
                "author_kind":"agent_session","in_reply_to":first["message_id"],
                "attempt_id":f.attempt(&first["message_id"],"worker"),
                "work_outcome":"completed","coordination_id":id}),
            &f.runtime("worker"),
        )
        .unwrap_err(),
        worker_error
    );

    assert_eq!(
        execute_message_operation(
            "reply",
            json!({"request_id":"before-adopt","source":"@lead Synthetic worker result",
                "author_kind":"agent_session","in_reply_to":first["message_id"],
                "attempt_id":f.attempt(&first["message_id"],"worker"),
                "work_outcome":"completed"}),
            &f.runtime("worker"),
        )
        .unwrap_err(),
        "the lead session restarted; it must run handoff adopt before tracked replies are accepted, so retry this reply after it does"
    );

    let restarted = "coordination belongs to another session of this address; the restarted owner session can transfer it with handoff adopt";
    assert_eq!(
        execute_message_operation(
            "send",
            json!({"request_id":"fresh","source":"@reviewer new work",
                "author_kind":"agent_session","coordination_id":id}),
            &f.runtime("lead"),
        )
        .unwrap_err(),
        restarted
    );
    assert_eq!(
        execute_message_operation(
            "send",
            json!({"request_id":"fresh-continuation","source":"@worker Continue.",
                "author_kind":"agent_session","coordination_id":id,
                "assignment_id":first["coordination"][0]["assignment_id"],"expected_revision":1}),
            &f.runtime("lead"),
        )
        .unwrap_err(),
        restarted
    );
}

#[test]
fn adoption_retires_the_stale_wake_and_wakes_the_new_session() {
    let f = Fixture::new();
    let id = f.begin();
    let a = f.send(&id, "first", "worker");
    let reply = f.reply(&a, "worker", "first-result", "needs_continuation", None);
    f.status("lead", "idle");
    f.drain();
    let next = f.pane_lists.load(Ordering::Relaxed) + 3;
    f.lead_working_after.store(next, Ordering::Relaxed);
    f.complete(&f.attempt(&reply["message_id"], "lead"));
    assert_eq!(f.wake_count(), 1);
    assert!(f.nudge_prompts().is_empty());
    let stale_wake =
        f.handoff("handoff.results", json!({"coordination_id":id}))["wake_message_id"].clone();
    assert_eq!(f.delivery(&stale_wake, "lead").1, "queued");

    f.replace_session("lead", "replacement-lead");
    f.drain();
    assert_eq!(
        f.attention_summary(),
        "The queued continuation lost its exact lead session. The current session on this address can take over this coordination with handoff adopt."
    );
    assert_eq!(f.delivery(&stale_wake, "lead").1, "queued");
    f.adopt("lead", "adopt-1", &id).unwrap();
    assert_eq!(f.attention_count(), 0);
    assert_eq!(
        f.delivery(&stale_wake, "lead").1,
        "queued",
        "adoption itself does not drain"
    );
    f.drain();
    let (stale_delivery, state) = f.delivery(&stale_wake, "lead");
    assert_eq!(state, "cancelled");
    assert_eq!(
        f.fact_count(
            "mailbox_delivery",
            &stale_delivery,
            "cancelled",
            "namedrop-coordination"
        ),
        1
    );

    f.lead_working_after.store(u64::MAX, Ordering::Relaxed);
    f.status("lead", "idle");
    f.drain();
    assert_eq!(f.wake_count(), 2);
    let results = f.handoff("handoff.results", json!({"coordination_id":id}));
    let fresh_wake = results["wake_message_id"].clone();
    assert_ne!(fresh_wake, stale_wake);
    assert_eq!(results["wake_revision"], results["revision"]);
    assert_eq!(f.recipient_session(&fresh_wake, "lead"), "replacement-lead");
    assert!(
        f.nudge_prompts()
            .iter()
            .any(|prompt| prompt["control"]["message_id"] == fresh_wake)
    );
}

#[test]
fn adoption_cancels_queued_replies_pinned_to_the_superseded_session() {
    let f = Fixture::new();
    let id = f.begin();
    let held_request = f.send(&id, "held-work", "worker");
    let held_reply = f.reply(&held_request, "worker", "held-result", "completed", None);
    f.now
        .store(100 + 24 * 60 * 60 * 1_000 + 1, Ordering::Relaxed);
    f.drain();
    assert_eq!(f.delivery(&held_reply["message_id"], "lead").1, "held");
    let request = f.send(&id, "pinned-work", "worker");
    let reply = f.reply(&request, "worker", "pinned-result", "completed", None);
    assert_eq!(f.delivery(&reply["message_id"], "lead").1, "queued");
    f.replace_session("lead", "replacement-lead");
    let tables = || {
        let db = f.db();
        [
            "message_recipients",
            "collaboration_messages",
            "coordination_assignments",
            "coordination_links",
        ]
        .map(|table| {
            let mut statement = db
                .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                .unwrap();
            let columns = statement.column_count();
            statement
                .query_map([], |row| {
                    (0..columns)
                        .map(|index| row.get(index))
                        .collect::<Result<Vec<rusqlite::types::Value>, _>>()
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        })
    };
    let before = tables();

    f.adopt("lead", "adopt-1", &id).unwrap();
    assert_eq!(
        tables(),
        before,
        "adoption rewrites no message, recipient, assignment or link"
    );
    let (reply_delivery, state) = f.delivery(&reply["message_id"], "lead");
    assert_eq!(state, "cancelled");
    assert_eq!(
        f.fact_count(
            "mailbox_delivery",
            &reply_delivery,
            "cancelled",
            "replacement-lead"
        ),
        1
    );
    assert_eq!(
        f.recipient_session(&reply["message_id"], "lead"),
        "session-lead"
    );
    let (held_delivery, state) = f.delivery(&held_reply["message_id"], "lead");
    assert_eq!(state, "held", "adoption leaves a held reply alone");
    let cancelled = execute_control_operation(
        "delivery.cancel-held",
        json!({"delivery_id":held_delivery,"confirmed":true}),
        &f.runtime("lead"),
    )
    .unwrap();
    assert_eq!(cancelled["state"], "cancelled");

    let resolved = f.handoff(
        "handoff.resolve",
        json!({
            "request_id":"adopted-resolve",
            "coordination_id":id,
            "outcome":"completed",
            "report":"Synthetic adopted report.",
            "reconciled_assignments":[
                {"assignment_id":held_request["coordination"][0]["assignment_id"],"revision":1},
                {"assignment_id":request["coordination"][0]["assignment_id"],"revision":1}
            ]
        }),
    );
    assert_eq!(resolved["outcome"], "completed");
    f.status("lead", "idle");
    f.drain();
    f.complete(&f.attempt(&resolved["report_message_id"], "lead"));
    assert_eq!(
        f.handoff("handoff.results", json!({"coordination_id":id}))["report_delivery_state"],
        "settled"
    );
}

#[test]
fn a_worker_naming_itself_at_the_start_of_a_tracked_reply_still_reaches_the_lead() {
    let f = Fixture::new();
    let id = f.begin();
    let request = f.send(&id, "self-label", "worker");
    let reply = execute_message_operation(
        "reply",
        json!({"request_id":"self-label-result","source":"@lead @worker top 3: the parser is fine",
            "author_kind":"agent_session","in_reply_to":request["message_id"],
            "attempt_id":f.attempt(&request["message_id"],"worker"),"work_outcome":"completed"}),
        &f.runtime("worker"),
    )
    .expect("the author's own name is a label, not a second recipient");
    let recipients: Vec<&str> = reply["deliveries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["address_id"].as_str().unwrap())
        .collect();
    assert_eq!(recipients, ["lead"]);
}

#[test]
fn a_tracked_reply_that_also_names_another_agent_says_which_one_and_how_to_fix_it() {
    let f = Fixture::new();
    let id = f.begin();
    let request = f.send(&id, "extra-recipient", "worker");
    let refused = execute_message_operation(
        "reply",
        json!({"request_id":"extra-recipient-result","source":"@lead @reviewer agreed with this",
            "author_kind":"agent_session","in_reply_to":request["message_id"],
            "attempt_id":f.attempt(&request["message_id"],"worker"),"work_outcome":"completed"}),
        &f.runtime("worker"),
    )
    .unwrap_err();
    assert!(refused.contains("@lead"), "{refused}");
    assert!(refused.contains("@reviewer"), "{refused}");
    assert!(refused.contains("without the @"), "{refused}");
}

#[test]
fn a_message_addressed_only_to_its_author_is_refused() {
    let f = Fixture::new();
    let refused = execute_message_operation(
        "send",
        json!({"request_id":"self-only","source":"@worker note to self","author_kind":"agent_session"}),
        &f.runtime("worker"),
    )
    .unwrap_err();
    assert!(refused.contains("only to its author"), "{refused}");
}

/// Namestack verify run v10111443b186, capture `restart-adopt-stall`: the
/// restarted sub-lead resolved its coordination inside the post-adoption
/// nudge's turn and ended that turn without the nudge's bound complete. The
/// wake attempt then sat acknowledged with no disposition for five idle
/// minutes, the report_to_human delivery stayed queued behind it, and once a
/// later prompt got things moving the wake settled with no disposition and
/// Doctor kept ND-DELIVERY-003 for the rest of the run.
#[test]
fn a_nudge_the_lead_acted_on_but_never_completed_settles_as_no_reply_when_its_turn_ends() {
    let f = Fixture::new();
    let id = f.begin();
    let a = f.send(&id, "first", "worker");
    f.reply(&a, "worker", "first-result", "completed", None);
    f.status("lead", "idle");
    f.settle_lead_inputs();
    let nudge = f.nudge_prompts().pop().expect("the idle lead is nudged");
    let wake_attempt = nudge["control"]["attempt_id"].as_str().unwrap().to_owned();
    assert_eq!(f.attempt_state(&wake_attempt), "acknowledged");

    f.status("lead", "working");
    let resolved = f.handoff(
        "handoff.resolve",
        json!({
            "request_id":"nudge-turn-resolve",
            "coordination_id":id,
            "outcome":"completed",
            "report":"Synthetic final report.",
            "reconciled_assignments":[
                {"assignment_id":a["coordination"][0]["assignment_id"],"revision":1}
            ]
        }),
    );
    let report = resolved["report_message_id"].clone();
    f.status("lead", "idle");

    // Inside the acknowledgement grace the host's idle reading may still lag
    // the submission, so the wake stays acknowledged and the report waits.
    f.now.store(100 + 10_000, Ordering::Relaxed);
    f.drain();
    assert_eq!(f.attempt_state(&wake_attempt), "acknowledged");
    assert_eq!(f.delivery(&report, "lead").1, "queued");
    assert!(!f.prompted(&report, "lead"));

    // Past the grace with the session idle the nudge is finished: no reply was
    // ever permitted, the coordination already records what the lead did, and
    // nothing is left for a human to decide about the nudge itself.
    f.now.store(100 + 31_000, Ordering::Relaxed);
    f.drain();
    assert_eq!(f.attempt_state(&wake_attempt), "settled");
    assert_eq!(f.disposition(&wake_attempt).as_deref(), Some("no_reply"));
    assert!(
        f.unresolved_attention()
            .iter()
            .all(|(kind, _)| kind != "agent_disposition"),
        "a settled nudge must not raise ND-DELIVERY-003: {:?}",
        f.unresolved_attention()
    );
    assert!(
        f.prompted(&report, "lead"),
        "the report must reach the lead"
    );
    assert_eq!(f.delivery(&report, "lead").1, "acknowledged");

    let attention = f.unresolved_attention();
    f.drain();
    assert_eq!(f.disposition(&wake_attempt).as_deref(), Some("no_reply"));
    assert_eq!(f.unresolved_attention(), attention);
    f.complete(&wake_attempt);
    assert_eq!(f.disposition(&wake_attempt).as_deref(), Some("no_reply"));

    f.complete(&f.attempt(&report, "lead"));
    assert_eq!(f.delivery(&report, "lead").1, "settled");
    assert_eq!(f.unresolved_attention(), Vec::<(String, String)>::new());
}

#[test]
fn an_ignored_nudge_settles_as_no_reply_and_the_coordination_is_flagged_for_a_decision() {
    let f = Fixture::new();
    let id = f.begin();
    let a = f.send(&id, "first", "worker");
    f.reply(&a, "worker", "first-result", "completed", None);
    f.status("lead", "idle");
    f.settle_lead_inputs();
    let nudge = f.nudge_prompts().pop().expect("the idle lead is nudged");
    let wake_attempt = nudge["control"]["attempt_id"].as_str().unwrap().to_owned();

    f.now.store(100 + 31_000, Ordering::Relaxed);
    f.drain();
    assert_eq!(f.attempt_state(&wake_attempt), "settled");
    assert_eq!(f.disposition(&wake_attempt).as_deref(), Some("no_reply"));
    let attention = f.unresolved_attention();
    assert!(
        attention
            .iter()
            .all(|(kind, _)| kind != "agent_disposition"),
        "an ignored nudge must not raise ND-DELIVERY-003: {attention:?}"
    );
    assert_eq!(
        attention
            .iter()
            .filter(|(kind, _)| kind == "coordination")
            .count(),
        1,
        "{attention:?}"
    );
    assert_eq!(
        f.attention_summary(),
        "The one continuation for this revision ended or is held without a new work decision. Review handoff.results and act explicitly; Namedrop will not repeat the prompt."
    );
}
