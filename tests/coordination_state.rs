use namedrop::*;
use serde_json::{Value, json};
use std::{
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
    session_override: Mutex<Option<(&'static str, &'static str)>>,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-coordination-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        let f = Self {
            state: root.join("state"),
            home: root.join("home"),
            root,
            ids: AtomicU64::new(1),
            session_override: Mutex::new(None),
        };
        fs::create_dir_all(&f.home).unwrap();
        let ns = StateNamespace::for_space("synthetic-socket", "synthetic-workspace");
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
                            role_name: name.to_owned(),
                            role_description: "synthetic role".to_owned(),
                            alias: name.to_owned(),
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
                        "idle",
                        &by,
                    )?;
                }
                tx.complete_reconciliation(&ns)?;
                Ok(())
            })
            .unwrap();
        for name in ["lead", "worker", "reviewer"] {
            let runtime = f.runtime(name);
            let challenge = issue_registration_challenge(
                &PaneAddressId::new(name),
                AgentRuntime::Codex,
                &runtime,
            )
            .unwrap();
            execute_integration_operation("register",json!({"runtime":"codex","protocol_version":"collab/v1","nonce":challenge.nonce,"capabilities":complete_capability_profile()}),&runtime).unwrap();
        }
        f
    }
    fn runtime<'a>(
        &'a self,
        pane: &'a str,
    ) -> LifecycleRuntime<'a, namedrop::TuiHost<&'a Self>, Self, Self> {
        LifecycleRuntime {
            state_directory: &self.state,
            home_directory: Some(&self.home),
            agent_config: namedrop::AgentConfigDirectories::default(),
            cwd: &self.root,
            workspace_id: Some("synthetic-workspace"),
            current_pane_id: Some(pane),
            session_socket_id: "synthetic-socket",
            host: namedrop::TuiHost::new(self),
            clock: self,
            ids: self,
        }
    }
    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.state.join("namedrop.sqlite3")).unwrap()
    }
    fn snapshot(&self) -> std::collections::BTreeMap<String, Vec<Vec<rusqlite::types::Value>>> {
        let db = self.db();
        let tables = db.prepare("SELECT name FROM sqlite_schema WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap()
            .query_map([], |row| row.get::<_,String>(0)).unwrap().collect::<Result<Vec<_>,_>>().unwrap();
        tables
            .into_iter()
            .map(|table| {
                let mut statement = db
                    .prepare(&format!("SELECT * FROM {table} ORDER BY rowid"))
                    .unwrap();
                let columns = statement.column_count();
                let rows = statement
                    .query_map([], |row| {
                        (0..columns)
                            .map(|index| row.get(index))
                            .collect::<Result<Vec<rusqlite::types::Value>, _>>()
                    })
                    .unwrap()
                    .collect::<Result<Vec<_>, _>>()
                    .unwrap();
                (table, rows)
            })
            .collect()
    }
    fn rejects_unchanged(&self, action: impl FnOnce() -> Result<Value, String>) -> String {
        let before = self.snapshot();
        let error = action().expect_err("invalid action must reject");
        assert_eq!(
            self.snapshot(),
            before,
            "rejected action mutated durable state: {error}"
        );
        error
    }
    fn updater_state(&self) -> Value {
        let script = "import json,sys;sys.path.insert(0,sys.argv[1]);import update;s=update.durable_state([]);s['other_servers']=[];s['ready']=update.ready(s,[]);print(json.dumps(s))";
        let output = std::process::Command::new("python3")
            .args(["-B", "-c", script])
            .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts"))
            .env("HERDR_PLUGIN_STATE_DIR", &self.state)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).unwrap()
    }
    fn replace_session(&self, name: &'static str, session: &'static str) {
        *self.session_override.lock().unwrap() = Some((name, session));
        StateStore::open(&self.state)
            .unwrap()
            .transaction(|tx| {
                tx.observe_agent_session(
                    &StateNamespace::for_space("synthetic-socket", "synthetic-workspace"),
                    &format!("observation-{session}"),
                    &PaneAddressId::new(name),
                    &AgentSessionId::new(session),
                    "idle",
                    &MutationAttribution::new(ActorKind::Human, "synthetic-replacement", 100),
                )
            })
            .unwrap();
        let runtime = self.runtime(name);
        let challenge =
            issue_registration_challenge(&PaneAddressId::new(name), AgentRuntime::Codex, &runtime)
                .unwrap();
        execute_integration_operation("register",json!({"runtime":"codex","protocol_version":"collab/v1","nonce":challenge.nonce,"capabilities":complete_capability_profile()}),&runtime).unwrap();
    }
    fn handoff(&self, pane: &str, op: &str, input: Value) -> Result<Value, String> {
        execute_handoff_operation(op, input, &self.runtime(pane))
    }
    fn begin(&self, key: &str) -> Value {
        self.handoff(
            "lead",
            "handoff.begin",
            json!({"request_id":key,"deliverable":"Synthetic reviewed final report"}),
        )
        .unwrap()
    }
    fn report(&self, id: &Value) -> Value {
        self.handoff("lead", "handoff.results", json!({"coordination_id":id}))
            .unwrap()
    }
    fn message(&self, pane: &str, op: &str, input: Value) -> Result<Value, String> {
        execute_message_operation(op, input, &self.runtime(pane))
    }
    fn send(&self, id: &Value, key: &str, source: &str) -> Value {
        self.message("lead","send",json!({"request_id":key,"source":source,"author_kind":"agent_session","coordination_id":id})).unwrap()
    }
    fn attempt(&self, message: &Value, pane: &str) -> String {
        self.db().query_row("SELECT a.attempt_id FROM delivery_attempts a JOIN mailbox_deliveries d USING(session_socket_id,worktree_id,delivery_id,address_id) WHERE d.message_id=?1 AND a.address_id=?2 ORDER BY a.rowid DESC LIMIT 1",rusqlite::params![message.as_str().unwrap(),pane],|r|r.get(0)).unwrap()
    }
    fn reply_input(&self, request: &Value, pane: &str, key: &str, outcome: Option<&str>) -> Value {
        let mut value = json!({"request_id":key,"source":"@lead Synthetic work report","author_kind":"agent_session","in_reply_to":request["message_id"],"attempt_id":self.attempt(&request["message_id"],pane)});
        if let Some(outcome) = outcome {
            value["work_outcome"] = json!(outcome);
        }
        value
    }
    fn reply(&self, request: &Value, pane: &str, key: &str, outcome: Option<&str>) -> Value {
        self.message(pane, "reply", self.reply_input(request, pane, key, outcome))
            .unwrap()
    }
    fn complete(&self, message: &Value, pane: &str) {
        execute_integration_operation(
            "complete",
            json!({"attempt_id":self.attempt(message,pane)}),
            &self.runtime(pane),
        )
        .unwrap();
    }
    fn settle_mail(&self, pane: &str) {
        for _ in 0..20 {
            drain_mailboxes(&self.runtime(pane)).unwrap();
            let inbox =
                execute_integration_operation("delivery.inbox", json!({}), &self.runtime(pane))
                    .unwrap();
            let envelopes = inbox["envelopes"].as_array().unwrap();
            if envelopes.is_empty() {
                return;
            }
            for e in envelopes {
                execute_integration_operation(
                    "complete",
                    json!({"attempt_id":e["control"]["attempt_id"]}),
                    &self.runtime(pane),
                )
                .unwrap();
            }
        }
        panic!("synthetic mailbox did not settle");
    }
    fn set_delivery_state(&self, message: &Value, pane: &str, state: &str) {
        assert_eq!(
            self.db()
                .execute(
                    "UPDATE mailbox_deliveries SET state=?1 WHERE message_id=?2 AND address_id=?3",
                    rusqlite::params![state, message.as_str().unwrap(), pane],
                )
                .unwrap(),
            1
        );
    }
    fn delivery_state(&self, message: &Value, pane: &str) -> String {
        self.db()
            .query_row(
                "SELECT state FROM mailbox_deliveries WHERE message_id=?1 AND address_id=?2",
                rusqlite::params![message.as_str().unwrap(), pane],
                |row| row.get(0),
            )
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}
impl Clock for Fixture {
    fn now_unix_ms(&self) -> u64 {
        100
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
            ["workspace", "list"] => {
                json!({"result":{"workspaces":[{"workspace_id":"synthetic-workspace"}]}})
            }
            ["worktree", "list", ..] => {
                json!({"result":{"worktrees":[{"path":self.root,"open_workspace_id":"synthetic-workspace"}]}})
            }
            ["pane", "list", ..] => {
                json!({"result":{"panes":(["lead","worker","reviewer"].iter().map(|name|{
                let replacement=*self.session_override.lock().unwrap();
                json!({"pane_id":name,"terminal_id":format!("terminal-{name}"),"tab_id":"synthetic-tab","workspace_id":"synthetic-workspace","agent_session":{"value":replacement.filter(|(pane,_)|pane==name).map(|(_,session)|session.to_owned()).unwrap_or_else(||format!("session-{name}"))},"agent_status":"idle"})
            }).collect::<Vec<_>>())}})
            }
            ["agent", "prompt", ..] | ["pane", "report-metadata", ..] => json!({}),
            _ => return Err(format!("unexpected synthetic host command {args:?}")),
        };
        Ok(HerdrCommandOutput {
            stdout: stdout.to_string(),
        })
    }
}

#[test]
fn coordination_migration_and_reopen() {
    let f = Fixture::new();
    let legacy=f.message("lead","send",json!({"request_id":"legacy","source":"@worker legacy message","author_kind":"agent_session"})).unwrap();
    f.reply(&legacy, "worker", "legacy-response", None);
    f.settle_mail("lead");
    // Preserve real completed messages and dispositions alongside queued/held
    // work and pause state. Everything is confined to this synthetic database.
    let namespace = StateNamespace::for_space("synthetic-socket", "synthetic-workspace");
    let pause = MutationAttribution::new(ActorKind::Human, "synthetic-pause", 100);
    StateStore::open(&f.state)
        .unwrap()
        .transaction(|tx| tx.set_worktree_paused(&namespace, true, &pause))
        .unwrap();
    let held=f.message("lead","send",json!({"request_id":"legacy-held","source":"@worker held legacy work","author_kind":"agent_session"})).unwrap();
    let queued=f.message("lead","send",json!({"request_id":"legacy-queued","source":"@reviewer queued legacy work","author_kind":"agent_session"})).unwrap();
    f.db()
        .execute(
            "UPDATE mailbox_deliveries SET state='held' WHERE message_id=?1",
            [held["message_id"].as_str().unwrap()],
        )
        .unwrap();
    StateStore::open(&f.state)
        .unwrap()
        .transaction(|tx| {
            tx.pause_thread(
                &namespace,
                &ThreadId::new(queued["thread_id"].as_str().unwrap()),
                &pause,
            )
        })
        .unwrap();
    f.db().execute_batch("DROP INDEX transition_facts_by_entity; DROP VIEW delivery_seats; DROP TABLE bridged_threads; DROP TABLE bridged_messages; DROP TABLE space_envoys; DROP TABLE coordination_links; DROP TABLE coordination_assignments; DROP TABLE coordinations; ALTER TABLE pane_addresses DROP COLUMN remote_commands_terminal_id; ALTER TABLE pane_addresses DROP COLUMN persona; PRAGMA user_version=3;").unwrap();
    let before = f.snapshot();
    for table in [
        "pane_addresses",
        "pane_bindings",
        "agent_session_observations",
        "integration_registrations",
        "worktrees",
        "thread_controls",
        "collaboration_messages",
        "delivery_attempts",
        "delivery_dispositions",
    ] {
        assert!(
            !before[table].is_empty(),
            "legacy migration control {table} was empty"
        );
    }
    let delivery_states = f
        .db()
        .prepare("SELECT DISTINCT state FROM mailbox_deliveries ORDER BY state")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(
        delivery_states.contains(&"queued".to_owned())
            && delivery_states.contains(&"held".to_owned())
    );
    assert_eq!(f.updater_state()["ready"], false);
    assert_eq!(
        StateStore::open(&f.state)
            .unwrap()
            .schema_version()
            .unwrap(),
        7
    );
    let after = f.snapshot();
    // Migration is additive: every legacy row keeps its rows and its existing
    // column values, and may only gain new trailing columns.
    for (table, rows) in &before {
        let migrated = after
            .get(table)
            .unwrap_or_else(|| panic!("migration dropped legacy table {table}"));
        assert_eq!(
            migrated.len(),
            rows.len(),
            "migration changed rows in {table}"
        );
        for (legacy, migrated) in rows.iter().zip(migrated) {
            assert_eq!(
                &migrated[..legacy.len()],
                legacy.as_slice(),
                "migration changed legacy columns in {table}"
            );
        }
    }
    assert!(
        after["pane_addresses"]
            .iter()
            .all(|row| row.len() == before["pane_addresses"][0].len() + 2),
        "migration did not add the persona and remote-command columns"
    );
    assert_eq!(after.len(), before.len() + 6);
    assert_eq!(
        f.handoff(
            "lead",
            "handoff.results",
            json!({"message_id":legacy["message_id"]})
        )
        .unwrap()["request"],
        "legacy message"
    );
    let begin = f.begin("reopen");
    let id = &begin["coordination_id"];
    assert_eq!(f.begin("reopen")["coordination_id"], *id);
    assert!(
        f.handoff(
            "lead",
            "handoff.begin",
            json!({"request_id":"reopen","deliverable":"changed"})
        )
        .is_err()
    );
    let a = f.send(id, "assignment-a", "@worker separate assignment A");
    let b = f.send(id, "assignment-b", "@reviewer separate assignment B");
    assert_ne!(
        a["coordination"][0]["assignment_id"],
        b["coordination"][0]["assignment_id"]
    );
    drop(StateStore::open(&f.state).unwrap());
    assert_eq!(f.report(id)["assignments"].as_array().unwrap().len(), 2);
    // The updater must keep the obligation even when the lead session is gone
    // and all legacy mailboxes are terminal.
    f.db()
        .execute_batch("UPDATE mailbox_deliveries SET state='cancelled'")
        .unwrap();
    let state = f.updater_state();
    assert_eq!(state["ready"], false);
    assert!(
        state["details"]
            .as_array()
            .unwrap()
            .iter()
            .any(|detail| detail[4] == "coordination:open")
    );
}

#[test]
fn completed_resolution_cancels_only_exact_queued_assignment_replies() {
    let f = Fixture::new();
    let completed = f.begin("retire-replies");
    let completed_id = &completed["coordination_id"];
    let target_request = f.send(completed_id, "target", "@worker target assignment");
    let target_reply_v1 = f.reply(
        &target_request,
        "worker",
        "target-v1-done",
        Some("completed"),
    );
    let target_assignment = f.report(completed_id)["assignments"][0].clone();
    assert_eq!(target_assignment["revision"], 1);
    let target_request_v2 = f
        .message(
            "lead",
            "send",
            json!({"request_id":"target-v2","source":"@worker target assignment revision 2","author_kind":"agent_session","coordination_id":completed_id,"assignment_id":target_assignment["assignment_id"],"expected_revision":target_assignment["revision"]}),
        )
        .unwrap();
    let target_reply_v2 = f.reply(
        &target_request_v2,
        "worker",
        "target-v2-done",
        Some("completed"),
    );
    for reply in [&target_reply_v1, &target_reply_v2] {
        f.set_delivery_state(&reply["message_id"], "lead", "queued");
    }

    let mut preserved = vec![];
    for state in ["held", "dispatching", "submitted", "acknowledged"] {
        let request = f.send(
            completed_id,
            &format!("protected-{state}"),
            "@worker protected assignment",
        );
        let reply = f.reply(
            &request,
            "worker",
            &format!("protected-{state}-done"),
            Some("completed"),
        );
        f.set_delivery_state(&reply["message_id"], "lead", state);
        preserved.push((reply["message_id"].clone(), "lead", state));
    }

    let future_request = f.send(
        completed_id,
        "future-revision",
        "@worker future revision control",
    );
    let future_reply = f.reply(
        &future_request,
        "worker",
        "future-revision-done",
        Some("completed"),
    );
    f.set_delivery_state(&future_reply["message_id"], "lead", "queued");
    assert_eq!(
        f.db()
            .execute(
                "UPDATE coordination_links SET revision=revision+1 WHERE message_id=?1 AND kind='reply'",
                [future_reply["message_id"].as_str().unwrap()],
            )
            .unwrap(),
        1
    );
    preserved.push((future_reply["message_id"].clone(), "lead", "queued"));

    let wrong_session_request = f.send(
        completed_id,
        "wrong-session",
        "@worker wrong recipient session control",
    );
    let wrong_session_reply = f.reply(
        &wrong_session_request,
        "worker",
        "wrong-session-done",
        Some("completed"),
    );
    f.set_delivery_state(&wrong_session_reply["message_id"], "lead", "queued");
    // Corrupt only this disposable fixture after persistence so the query's
    // exact-session guard is exercised despite recipient rows being immutable.
    f.db()
        .execute_batch("DROP TRIGGER immutable_recipients_update")
        .unwrap();
    assert_eq!(
        f.db()
            .execute(
                "UPDATE message_recipients SET session_snapshot='replacement-lead-session' WHERE message_id=?1 AND address_id='lead'",
                [wrong_session_reply["message_id"].as_str().unwrap()],
            )
            .unwrap(),
        1
    );
    preserved.push((wrong_session_reply["message_id"].clone(), "lead", "queued"));

    let untracked = f
        .message(
            "worker",
            "send",
            json!({"request_id":"untracked-control","source":"@lead ordinary message","author_kind":"agent_session"}),
        )
        .unwrap();
    f.set_delivery_state(&untracked["message_id"], "lead", "queued");
    preserved.push((untracked["message_id"].clone(), "lead", "queued"));

    let other = f.begin("other-coordination");
    let other_request = f.send(
        &other["coordination_id"],
        "other-assignment",
        "@worker other coordination",
    );
    let other_reply = f.reply(&other_request, "worker", "other-done", Some("completed"));
    f.set_delivery_state(&other_reply["message_id"], "lead", "queued");
    preserved.push((other_reply["message_id"].clone(), "lead", "queued"));

    let reviewer_coordination = f
        .handoff(
            "reviewer",
            "handoff.begin",
            json!({"request_id":"reviewer-owned","deliverable":"Reviewer-owned control"}),
        )
        .unwrap();
    let reviewer_request = f
        .message(
            "reviewer",
            "send",
            json!({"request_id":"reviewer-assignment","source":"@worker reviewer-owned assignment","author_kind":"agent_session","coordination_id":reviewer_coordination["coordination_id"]}),
        )
        .unwrap();
    let reviewer_reply = f
        .message(
            "worker",
            "reply",
            json!({"request_id":"reviewer-owned-done","source":"@reviewer Synthetic work report","author_kind":"agent_session","in_reply_to":reviewer_request["message_id"],"attempt_id":f.attempt(&reviewer_request["message_id"],"worker"),"work_outcome":"completed"}),
        )
        .unwrap();
    f.set_delivery_state(&reviewer_reply["message_id"], "reviewer", "queued");
    preserved.push((reviewer_reply["message_id"].clone(), "reviewer", "queued"));

    let mut noncompleted = vec![];
    for outcome in ["blocked", "cancelled"] {
        let coordination = f.begin(&format!("{outcome}-control"));
        let request = f.send(
            &coordination["coordination_id"],
            &format!("{outcome}-assignment"),
            "@worker unfinished control",
        );
        let reply = f.reply(
            &request,
            "worker",
            &format!("{outcome}-progress"),
            Some("needs_continuation"),
        );
        f.set_delivery_state(&reply["message_id"], "lead", "queued");
        noncompleted.push((outcome, coordination["coordination_id"].clone(), reply));
    }

    f.set_delivery_state(&target_request["message_id"], "worker", "queued");
    preserved.push((target_request["message_id"].clone(), "worker", "queued"));

    let ns = StateNamespace::for_space("synthetic-socket", "synthetic-workspace");
    StateStore::open(&f.state)
        .unwrap()
        .transaction(|tx| {
            tx.set_worktree_paused(
                &ns,
                true,
                &MutationAttribution::new(ActorKind::Human, "synthetic-pause", 100),
            )
        })
        .unwrap();
    for (outcome, coordination_id, reply) in noncompleted {
        f.handoff(
            "lead",
            "handoff.resolve",
            json!({"request_id":format!("resolve-{outcome}-control"),"coordination_id":coordination_id,"outcome":outcome,"report":"Noncompleted control report","reason":"Synthetic control"}),
        )
        .unwrap();
        preserved.push((reply["message_id"].clone(), "lead", "queued"));
    }

    let report = f.report(completed_id);
    assert_eq!(report["assignments"][0]["revision"], 2);
    let reconciled = report["assignments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|assignment| {
            json!({"assignment_id":assignment["assignment_id"],"revision":assignment["revision"]})
        })
        .collect::<Vec<_>>();
    let targets = [&target_reply_v1, &target_reply_v2]
        .into_iter()
        .map(|reply| {
            let delivery: String = f
                .db()
                .query_row(
                    "SELECT delivery_id FROM mailbox_deliveries WHERE message_id=?1 AND address_id='lead'",
                    [reply["message_id"].as_str().unwrap()],
                    |row| row.get(0),
                )
                .unwrap();
            let attempts = f
                .db()
                .query_row(
                    "SELECT count(*) FROM delivery_attempts WHERE delivery_id=?1",
                    [&delivery],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap();
            let transitions = f
                .db()
                .query_row(
                    "SELECT count(*) FROM transition_facts WHERE entity_kind='mailbox_delivery' AND entity_id=?1",
                    [&delivery],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap();
            (reply["message_id"].clone(), delivery, attempts, transitions)
        })
        .collect::<Vec<_>>();
    let input = json!({"request_id":"resolve-retire-replies","coordination_id":completed_id,"outcome":"completed","report":"All assignments reconciled.","reconciled_assignments":reconciled});
    f.handoff("lead", "handoff.resolve", input.clone()).unwrap();

    for (message, _, _, _) in &targets {
        assert_eq!(f.delivery_state(message, "lead"), "cancelled");
    }
    for (message, pane, state) in preserved {
        assert_eq!(
            f.delivery_state(&message, pane),
            state,
            "unrelated or invoked delivery changed for message {message}"
        );
    }
    for (message, delivery, attempt_count, transition_count) in &targets {
        assert_eq!(
            f.db()
                .query_row(
                    "SELECT count(*) FROM collaboration_messages WHERE message_id=?1",
                    [message.as_str().unwrap()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            f.db()
                .query_row(
                    "SELECT count(*) FROM coordination_links WHERE message_id=?1 AND kind='reply'",
                    [message.as_str().unwrap()],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            1
        );
        assert_eq!(
            f.db()
                .query_row(
                    "SELECT count(*) FROM delivery_attempts WHERE delivery_id=?1",
                    [delivery],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            *attempt_count,
            "cancellation discarded attempt history"
        );
        assert_eq!(
            f.db()
                .query_row(
                    "SELECT count(*) FROM transition_facts WHERE entity_kind='mailbox_delivery' AND entity_id=?1",
                    [delivery],
                    |row| row.get::<_, i64>(0),
                )
                .unwrap(),
            transition_count + 1,
            "cancellation replaced transition history instead of appending"
        );
        assert_eq!(
            f.db()
                .query_row(
                    "SELECT transition_kind || ':' || actor_kind || ':' || actor_id FROM transition_facts WHERE entity_kind='mailbox_delivery' AND entity_id=?1 ORDER BY worktree_order DESC LIMIT 1",
                    [delivery],
                    |row| row.get::<_, String>(0),
                )
                .unwrap(),
            "cancelled:plugin:namedrop-coordination"
        );
    }
    let before_replay = f.snapshot();
    f.handoff("lead", "handoff.resolve", input).unwrap();
    assert_eq!(
        f.snapshot(),
        before_replay,
        "resolve replay was not idempotent"
    );
}

#[test]
fn coordination_work_is_not_delivery() {
    let f = Fixture::new();
    let c = f.begin("work");
    let id = &c["coordination_id"];
    let a = f.send(id, "a", "@worker first assignment");
    let b = f.send(id, "b", "@reviewer second assignment");
    f.reply(&a, "worker", "a-reply", Some("needs_continuation"));
    f.reply(&b, "reviewer", "b-reply", None);
    f.settle_mail("lead");
    let r = f.report(id);
    assert_eq!(r["outcome"], "open");
    assert_eq!(r["assignments"][0]["work_outcome"], "needs_continuation");
    assert_eq!(r["assignments"][1]["work_outcome"], "unclassified");
    assert_eq!(r["report_saved"], false);
    assert!(
        !f.handoff("lead", "handoff.status", json!({})).unwrap()["coordinations"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(f.handoff("lead","handoff.resolve",json!({"request_id":"early","coordination_id":id,"outcome":"completed","report":"premature"})).is_err());
    for (i, pane) in ["worker", "reviewer"].iter().enumerate() {
        let next=f.message("lead","send",json!({"request_id":format!("continue-{i}"),"source":format!("@{pane} finish your assignment"),"author_kind":"agent_session","coordination_id":id,"assignment_id":r["assignments"][i]["assignment_id"],"expected_revision":r["assignments"][i]["revision"]})).unwrap();
        f.reply(&next, pane, &format!("done-{i}"), Some("completed"));
    }
    f.settle_mail("lead");
    let r = f.report(id);
    let reconciled = r["assignments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| json!({"assignment_id":a["assignment_id"],"revision":a["revision"]}))
        .collect::<Vec<_>>();
    // A real foreign assignment is a stronger control than a nonexistent id.
    let foreign = f.begin("foreign-reconciliation");
    let foreign_work = f.send(
        &foreign["coordination_id"],
        "foreign-work",
        "@reviewer separate obligation",
    );
    f.reply(&foreign_work, "reviewer", "foreign-done", Some("completed"));
    f.handoff("lead", "handoff.resolve", json!({"request_id":"foreign-cancel","coordination_id":foreign["coordination_id"],"outcome":"cancelled","report":"Separate obligation cancelled.","reason":"Synthetic cancellation control"})).unwrap();
    f.settle_mail("lead");
    let input = json!({"request_id":"resolve","coordination_id":id,"outcome":"completed","report":"Synthetic implementation and review complete.","reconciled_assignments":reconciled});
    for empty in ["", " \n\t "] {
        let mut invalid = input.clone();
        invalid["report"] = json!(empty);
        assert!(
            f.rejects_unchanged(|| f.handoff("lead", "handoff.resolve", invalid))
                .contains("nonempty report")
        );
    }
    let mut invalid_lists = vec![
        json!([]),
        json!([reconciled[0]]),
        json!([reconciled[0], reconciled[0]]),
    ];
    let mut foreign_list = reconciled.clone();
    foreign_list[1] =
        json!({"assignment_id":foreign_work["coordination"][0]["assignment_id"],"revision":1});
    invalid_lists.push(json!(foreign_list));
    let mut stale = reconciled.clone();
    stale[0]["revision"] = json!(1);
    invalid_lists.push(json!(stale));
    let mut missing_field = input.clone();
    missing_field
        .as_object_mut()
        .unwrap()
        .remove("reconciled_assignments");
    f.rejects_unchanged(|| f.handoff("lead", "handoff.resolve", missing_field));
    for entries in invalid_lists {
        let mut invalid = input.clone();
        invalid["reconciled_assignments"] = entries;
        assert!(
            f.rejects_unchanged(|| f.handoff("lead", "handoff.resolve", invalid))
                .contains("every completed assignment reconciled")
        );
    }
    let resolved = f.handoff("lead", "handoff.resolve", input.clone()).unwrap();
    assert_eq!(resolved["report_saved"], true);
    assert_eq!(resolved["presentation"], "unknown");
    let before_replay = f.snapshot();
    assert_eq!(
        f.handoff("lead", "handoff.resolve", input.clone()).unwrap()["report_message_id"],
        resolved["report_message_id"]
    );
    assert_eq!(
        f.snapshot(),
        before_replay,
        "identical resolve replay changed state/outbox"
    );
    for (field, value) in [
        ("report", json!("Changed final report")),
        ("outcome", json!("cancelled")),
    ] {
        let mut changed = input.clone();
        changed[field] = value;
        if field == "outcome" {
            changed["reason"] = json!("Changed outcome");
        }
        assert!(
            f.rejects_unchanged(|| f.handoff("lead", "handoff.resolve", changed))
                .contains("already resolved with a different report")
        );
    }
    // Report text alone is not update work. Its pending delivery is, even when
    // the updater sees no owner session. A disposition permits a later update.
    let pending = f.updater_state();
    assert_eq!(pending["ready"], false);
    assert!(
        pending["details"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d[2] == *id && d[4] == "coordination:completed")
    );
    drain_mailboxes(&f.runtime("lead")).unwrap();
    let report_attempt = DeliveryAttemptId::new(f.attempt(&resolved["report_message_id"], "lead"));
    StateStore::open(&f.state)
        .unwrap()
        .transaction(|tx| {
            tx.transition_delivery_attempt(
                &StateNamespace::for_space("synthetic-socket", "synthetic-workspace"),
                &report_attempt,
                DeliveryState::Settled,
                &MutationAttribution::new(ActorKind::System, "synthetic-settled-observation", 100),
            )
        })
        .unwrap();
    assert_eq!(f.report(id)["report_delivery_settled"], true);
    assert_eq!(
        f.updater_state()["ready"],
        false,
        "settled without disposition is still pending report work"
    );
    f.complete(&resolved["report_message_id"], "lead");
    assert_eq!(
        f.updater_state()["ready"],
        true,
        "a disposed saved report must not permanently block updates"
    );
    let saved = f.report(id);
    assert_eq!(saved["report_delivery_settled"], true);
    assert_eq!(saved["presentation"], "unknown");
    assert_eq!(
        saved["report"],
        "Synthetic implementation and review complete."
    );
    for outcome in ["blocked", "cancelled"] {
        let empty = f.begin(&format!("empty-{outcome}"));
        let empty_id = &empty["coordination_id"];
        let report = format!("Synthetic zero-assignment {outcome} report.");
        let result=f.handoff("lead","handoff.resolve",json!({"request_id":format!("resolve-{outcome}"),"coordination_id":empty_id,"outcome":outcome,"report":report,"reason":"No assignments were dispatched"})).unwrap();
        assert_eq!(result["assignments"], json!([]));
        let recovered = f.report(empty_id);
        assert_eq!(recovered["coordination_id"], *empty_id);
        assert_eq!(recovered["outcome"], outcome);
        assert_eq!(recovered["report"], report);
        assert_eq!(recovered["presentation"], "unknown");
        assert_eq!(f.updater_state()["ready"], false);
        // Failed/cancelled report delivery is terminal transport, still with
        // retrievable saved text and unknown presentation.
        f.db()
            .execute(
                "UPDATE mailbox_deliveries SET state=?1 WHERE message_id=?2",
                rusqlite::params![
                    if outcome == "blocked" {
                        "failed"
                    } else {
                        "cancelled"
                    },
                    result["report_message_id"].as_str()
                ],
            )
            .unwrap();
        assert_eq!(f.updater_state()["ready"], true);
        assert_eq!(f.report(empty_id)["report"], report);
    }
    // complete.no-reply settles transport but cannot complete an assignment.
    let other = f.begin("no-reply");
    let request = f.send(
        &other["coordination_id"],
        "silent",
        "@worker transport only",
    );
    f.complete(&request["message_id"], "worker");
    assert_eq!(f.report(&other["coordination_id"])["outcome"], "open");
    assert_eq!(
        f.report(&other["coordination_id"])["assignments"][0]["work_outcome"],
        "pending"
    );
}

#[test]
fn coordination_authority_and_revisions() {
    let f = Fixture::new();
    let c = f.begin("authority");
    let id = &c["coordination_id"];
    assert!(f.handoff("worker","handoff.resolve",json!({"request_id":"foreign","coordination_id":id,"outcome":"blocked","report":"No","reason":"Not owner"})).is_err());
    *f.session_override.lock().unwrap() = Some(("lead", "replacement-session"));
    assert!(
        f.handoff(
            "lead",
            "handoff.begin",
            json!({"request_id":"replacement","deliverable":"must fail"})
        )
        .is_err()
    );
    *f.session_override.lock().unwrap() = None;
    let old_profile = complete_capability_profile()
        .iter()
        .filter(|c| {
            !matches!(
                c.as_str(),
                "handoff.adopt" | "handoff.begin" | "handoff.resolve"
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    f.db()
        .execute(
            "UPDATE integration_registrations SET capability_profile=?1 WHERE address_id='worker'",
            [serde_json::to_string(&old_profile).unwrap()],
        )
        .unwrap();
    assert!(f.message("lead","send",json!({"request_id":"unsupported","source":"@worker tracked","author_kind":"agent_session","coordination_id":id})).is_err());
    let count: i64 = f
        .db()
        .query_row("SELECT count(*) FROM collaboration_messages", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert!(f.message("lead", "send", json!({"request_id":"mixed-capabilities","source":"@reviewer @worker all or nothing","author_kind":"agent_session","coordination_id":id})).is_err());
    assert_eq!(
        f.db()
            .query_row("SELECT count(*) FROM collaboration_messages", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        count
    );
    f.db()
        .execute(
            "UPDATE integration_registrations SET capability_profile=?1 WHERE address_id='lead'",
            [serde_json::to_string(&old_profile).unwrap()],
        )
        .unwrap();
    assert!(
        f.handoff(
            "lead",
            "handoff.begin",
            json!({"request_id":"legacy-lead","deliverable":"refuse tracked mode"})
        )
        .is_err()
    );
    let ns = StateNamespace::for_space("synthetic-socket", "synthetic-workspace");
    StateStore::open(&f.state)
        .unwrap()
        .transaction(|tx| {
            tx.observe_agent_session(
                &ns,
                "legacy-observation",
                &PaneAddressId::new("lead"),
                &AgentSessionId::new("session-lead"),
                "idle",
                &MutationAttribution::new(ActorKind::System, "synthetic-observer", 100),
            )
        })
        .unwrap();
    assert!(
        StateStore::open(&f.state)
            .unwrap()
            .transaction(|tx| tx.active_integration_registration(
                &ns,
                &PaneAddressId::new("lead"),
                &AgentSessionId::new("session-lead")
            ))
            .unwrap()
            .is_some()
    );
    f.db()
        .execute(
            "UPDATE integration_registrations SET capability_profile=?1 WHERE address_id='lead'",
            [serde_json::to_string(complete_capability_profile()).unwrap()],
        )
        .unwrap();
    let legacy=f.message("lead","send",json!({"request_id":"legacy-ok","source":"@worker still supports ordinary messaging","author_kind":"agent_session"})).unwrap();
    assert!(legacy["message_id"].is_string());
    f.db()
        .execute(
            "UPDATE integration_registrations SET capability_profile=?1 WHERE address_id='worker'",
            [serde_json::to_string(complete_capability_profile()).unwrap()],
        )
        .unwrap();
    f.settle_mail("worker");
    f.db()
        .execute(
            "UPDATE pane_addresses SET pane_type='terminal' WHERE address_id='reviewer'",
            [],
        )
        .unwrap();
    assert!(f.message("lead","send",json!({"request_id":"terminal","source":"@worker @reviewer atomic refusal","author_kind":"agent_session","coordination_id":id})).is_err());
    f.db()
        .execute(
            "UPDATE pane_addresses SET pane_type='agent' WHERE address_id='reviewer'",
            [],
        )
        .unwrap();
    let a = f.send(id, "a", "@worker assignment A");
    let b = f.send(id, "b", "@reviewer assignment B");
    let aid = &a["coordination"][0]["assignment_id"];
    let bid = &b["coordination"][0]["assignment_id"];
    let mut blocked = f.reply_input(&a, "worker", "blocked", Some("blocked"));
    assert!(f.message("worker", "reply", blocked.clone()).is_err());
    blocked["blocked_reason"] = json!("Waiting for review");
    blocked["waiting_on_assignment_id"] = json!("missing-assignment");
    assert!(f.message("worker", "reply", blocked.clone()).is_err());
    let foreign = f.begin("foreign-parent");
    let foreign_assignment = f.send(
        &foreign["coordination_id"],
        "foreign-assignment",
        "@reviewer unrelated",
    );
    blocked["waiting_on_assignment_id"] =
        foreign_assignment["coordination"][0]["assignment_id"].clone();
    assert!(f.message("worker", "reply", blocked.clone()).is_err());
    blocked["waiting_on_assignment_id"] = bid.clone();
    let blocked_reply = f.message("worker", "reply", blocked.clone()).unwrap();
    let revision = f.report(id)["revision"].clone();
    assert_eq!(
        f.message("worker", "reply", blocked.clone()).unwrap()["idempotent_replay"],
        true
    );
    assert_eq!(f.report(id)["revision"], revision);
    let mut duplicate = blocked.clone();
    duplicate["request_id"] = json!("duplicate-outcome-new-key");
    assert!(f.message("worker", "reply", duplicate).is_err());
    assert_eq!(f.report(id)["revision"], revision);
    *f.session_override.lock().unwrap() = Some(("worker", "replacement-worker"));
    assert!(f.message("worker", "reply", blocked.clone()).is_err());
    *f.session_override.lock().unwrap() = None;
    blocked["blocked_reason"] = json!("changed reason");
    assert!(f.message("worker", "reply", blocked).is_err());
    let mut cycle = f.reply_input(&b, "reviewer", "cycle", Some("blocked"));
    cycle["blocked_reason"] = json!("Waiting for A");
    cycle["waiting_on_assignment_id"] = aid.clone();
    assert!(f.message("reviewer", "reply", cycle).is_err());
    let correction=f.message("lead","reply",json!({"request_id":"same-thread-correction","source":"@worker updated assignment details","author_kind":"agent_session","in_reply_to":blocked_reply["message_id"],"attempt_id":f.attempt(&blocked_reply["message_id"],"lead")})).unwrap();
    assert_eq!(correction["thread_id"], blocked_reply["thread_id"]);
    let mut wrong_sibling =
        f.reply_input(&correction, "worker", "wrong-sibling", Some("completed"));
    // Pair the current worker attempt with a different message in the same
    // thread. Recipient/thread checks alone cannot reject this combination.
    wrong_sibling["in_reply_to"] = blocked_reply["message_id"].clone();
    assert!(
        f.rejects_unchanged(|| f.message("worker", "reply", wrong_sibling))
            .contains("exact assignment message")
    );
    f.reply(
        &correction,
        "worker",
        "correction-progress",
        Some("needs_continuation"),
    );
    // Advance B before its still-outstanding first response. That delayed response
    // may settle the old attempt, but cannot overwrite revision 2's pending work.
    assert!(f.message("lead","send",json!({"request_id":"stale-continuation","source":"@reviewer stale","author_kind":"agent_session","coordination_id":id,"assignment_id":bid,"expected_revision":9})).is_err());
    assert!(f.message("lead","send",json!({"request_id":"missing-revision","source":"@reviewer missing revision","author_kind":"agent_session","coordination_id":id,"assignment_id":bid})).is_err());
    let second=f.message("lead","send",json!({"request_id":"b-next","source":"@reviewer revised assignment","author_kind":"agent_session","coordination_id":id,"assignment_id":bid,"expected_revision":1})).unwrap();
    let revision = f.report(id)["revision"].clone();
    f.reply(&b, "reviewer", "late-b", Some("completed"));
    let r = f.report(id);
    assert_eq!(r["assignments"][1]["revision"], 2);
    assert_eq!(r["assignments"][1]["work_outcome"], "pending");
    assert_eq!(r["revision"], revision);
    f.complete(&foreign_assignment["message_id"], "reviewer");
    drain_mailboxes(&f.runtime("reviewer")).unwrap();
    f.reply(&second, "reviewer", "new-b", Some("completed"));
    assert_eq!(f.report(id)["assignments"][1]["work_outcome"], "completed");
    let mut no_reason = json!({"request_id":"blocked-final","coordination_id":id,"outcome":"blocked","report":"Saved blocker"});
    assert!(
        f.handoff("lead", "handoff.resolve", no_reason.clone())
            .is_err()
    );
    no_reason["reason"] = json!("A remains unfinished");
    assert_eq!(
        f.handoff("lead", "handoff.resolve", no_reason).unwrap()["outcome"],
        "blocked"
    );
}

/// The exact sentence a blocked coordination keeps. Spelled out so a reworded
/// summary has to be reviewed rather than silently accepted.
const EXPECTED_BLOCKED_SUMMARY: &str = "This coordination was resolved blocked rather than completed. Recorded reason: Synthetic blocker. Review its saved report and decide what happens next; Namedrop will not continue it on its own.";

/// The complement of the cleared warning: every finished coordination that
/// still has something to act on keeps its item, and says which thing.
#[test]
fn finished_coordinations_keep_specific_warnings_and_stay_namespace_scoped() {
    let f = Fixture::new();
    let summary = |id: &Value| -> Option<String> {
        f.db()
            .query_row(
                "SELECT summary FROM attention_items
                 WHERE session_socket_id='synthetic-socket' AND worktree_id='synthetic-workspace'
                   AND source_kind='coordination' AND source_id=?1 AND resolved_at_ms IS NULL",
                rusqlite::params![id.as_str().unwrap()],
                |row| row.get::<_, String>(0),
            )
            .ok()
    };

    // A blocked outcome is a decision a human still owes, whatever its report
    // delivery did. It names the outcome rather than the generic old sentence.
    let blocked = f.begin("blocked-coordination")["coordination_id"].clone();
    let blocked_report = f
        .handoff(
            "lead",
            "handoff.resolve",
            json!({"request_id":"blocked-resolve","coordination_id":blocked,"outcome":"blocked","report":"Synthetic blocked report.","reason":"Synthetic blocker."}),
        )
        .unwrap();
    f.settle_mail("lead");
    assert_eq!(
        f.delivery_state(&blocked_report["report_message_id"], "lead"),
        "settled",
        "the blocked report itself was delivered"
    );
    let blocked_summary = summary(&blocked).expect("a blocked coordination keeps its item");
    // The outcome and the lead's own recorded reason both survive, and the
    // reason's own full stop is not doubled by the sentence around it.
    assert_eq!(blocked_summary, EXPECTED_BLOCKED_SUMMARY);
    // The historical outcome is untouched by any of this.
    assert_eq!(f.report(&blocked)["outcome"], "blocked");
    assert_eq!(f.report(&blocked)["report"], "Synthetic blocked report.");
    assert_eq!(f.report(&blocked)["presentation"], "unknown");

    // A completed coordination whose report delivery stopped on its own names
    // that state; the report stays readable either way.
    let stalled = f.begin("stalled-coordination")["coordination_id"].clone();
    let stalled_report = f
        .handoff(
            "lead",
            "handoff.resolve",
            json!({"request_id":"stalled-resolve","coordination_id":stalled,"outcome":"completed","report":"Synthetic completed report.","reconciled_assignments":[]}),
        )
        .unwrap();
    f.set_delivery_state(&stalled_report["report_message_id"], "lead", "held");
    drain_mailboxes(&f.runtime("lead")).unwrap();
    let stalled_summary = summary(&stalled).expect("a held report delivery keeps its item");
    // A held delivery may have been invoked ambiguously, so the state and the
    // lack of automatic progress are all that may be claimed about it.
    assert!(
        stalled_summary.contains("held")
            && stalled_summary.contains("will not progress on its own"),
        "{stalled_summary}"
    );
    assert!(
        !stalled_summary.contains("reach"),
        "a non-settled delivery must not be described as not having arrived: {stalled_summary}"
    );
    assert_eq!(f.report(&stalled)["report"], "Synthetic completed report.");

    // Settling that same delivery clears the item it had already persisted.
    f.set_delivery_state(&stalled_report["report_message_id"], "lead", "settled");
    drain_mailboxes(&f.runtime("lead")).unwrap();
    assert_eq!(
        summary(&stalled),
        None,
        "a settled report has nothing to act on"
    );
    drain_mailboxes(&f.runtime("lead")).unwrap();
    assert_eq!(
        summary(&stalled),
        None,
        "repeated reconciliation recreated it"
    );

    // A completed coordination with no saved report is not silently cleared.
    let orphan = f.begin("orphan-coordination")["coordination_id"].clone();
    assert_eq!(
        f.db()
            .execute(
                "UPDATE coordinations SET outcome='completed', report=NULL, report_message_id=NULL WHERE coordination_id=?1",
                rusqlite::params![orphan.as_str().unwrap()],
            )
            .unwrap(),
        1
    );
    drain_mailboxes(&f.runtime("lead")).unwrap();
    let orphan_summary =
        summary(&orphan).expect("a completed coordination with no report keeps its item");
    assert!(
        orphan_summary.contains("without a saved report"),
        "{orphan_summary}"
    );

    // The same holds for legacy rows that kept a report message id but no
    // saved body: a delivered pointer to nothing is not a readable report.
    assert_eq!(
        f.db()
            .execute(
                "UPDATE coordinations SET report_message_id=?2 WHERE coordination_id=?1",
                rusqlite::params![
                    orphan.as_str().unwrap(),
                    stalled_report["report_message_id"].as_str().unwrap()
                ],
            )
            .unwrap(),
        1
    );
    drain_mailboxes(&f.runtime("lead")).unwrap();
    let pointer_summary = summary(&orphan)
        .expect("a settled delivery cannot stand in for a report that was never saved");
    assert!(
        pointer_summary.contains("without a saved report"),
        "{pointer_summary}"
    );

    // A saved report with no report message recorded is not the same fact as
    // no saved report, and is not described as one.
    let unlinked = f.begin("unlinked-coordination")["coordination_id"].clone();
    let unlinked_report = f
        .handoff(
            "lead",
            "handoff.resolve",
            json!({"request_id":"unlinked-resolve","coordination_id":unlinked,"outcome":"completed","report":"Synthetic unlinked report.","reconciled_assignments":[]}),
        )
        .unwrap();
    assert_eq!(
        f.db()
            .execute(
                "UPDATE coordinations SET report_message_id=NULL WHERE coordination_id=?1",
                rusqlite::params![unlinked.as_str().unwrap()],
            )
            .unwrap(),
        1
    );
    drain_mailboxes(&f.runtime("lead")).unwrap();
    let unlinked_summary = summary(&unlinked).expect("a report with no message keeps its item");
    assert!(
        unlinked_summary.contains("no report message was recorded")
            && !unlinked_summary.contains("without a saved report"),
        "{unlinked_summary}"
    );
    assert_eq!(f.report(&unlinked)["report"], "Synthetic unlinked report.");

    // A missing delivery row proves there is no record of the delivery. It does
    // not prove the report was never queued, so nothing says that.
    assert_eq!(
        f.db()
            .execute(
                "UPDATE coordinations SET report_message_id=?2 WHERE coordination_id=?1",
                rusqlite::params![
                    unlinked.as_str().unwrap(),
                    unlinked_report["report_message_id"].as_str().unwrap()
                ],
            )
            .unwrap(),
        1
    );
    assert_eq!(
        f.db()
            .execute(
                "DELETE FROM mailbox_deliveries WHERE message_id=?1",
                rusqlite::params![unlinked_report["report_message_id"].as_str().unwrap()],
            )
            .unwrap(),
        1
    );
    drain_mailboxes(&f.runtime("lead")).unwrap();
    let missing_summary = summary(&unlinked).expect("a missing delivery record keeps its item");
    assert!(
        missing_summary.contains("no delivery record")
            && !missing_summary.contains("never queued")
            && !missing_summary.contains("reach"),
        "{missing_summary}"
    );

    // Clearing is scoped to one space: another namespace's item for the same
    // coordination id is left exactly as it was.
    let foreign = f.begin("foreign-coordination")["coordination_id"].clone();
    let foreign_report = f
        .handoff(
            "lead",
            "handoff.resolve",
            json!({"request_id":"foreign-resolve","coordination_id":foreign,"outcome":"completed","report":"Synthetic foreign report.","reconciled_assignments":[]}),
        )
        .unwrap();
    f.set_delivery_state(&foreign_report["report_message_id"], "lead", "settled");
    f.db()
        .execute(
            "INSERT INTO worktrees (session_socket_id, worktree_id, activated, paused, deactivated, created_at_ms)
             VALUES ('other-socket','other-workspace',1,0,0,1)",
            [],
        )
        .unwrap();
    f.db()
        .execute(
            "INSERT INTO attention_items (session_socket_id, worktree_id, attention_item_id,
                source_kind, source_id, summary, opened_at_ms, resolved_at_ms)
             VALUES ('other-socket','other-workspace',?1,'coordination',?2,'Another space still needs this.',1,NULL)",
            rusqlite::params![
                format!("coordination-attention:{}", foreign.as_str().unwrap()),
                foreign.as_str().unwrap()
            ],
        )
        .unwrap();
    drain_mailboxes(&f.runtime("lead")).unwrap();
    assert_eq!(summary(&foreign), None, "this space's item is cleared");
    let elsewhere: String = f
        .db()
        .query_row(
            "SELECT summary FROM attention_items
             WHERE session_socket_id='other-socket' AND source_id=?1 AND resolved_at_ms IS NULL",
            rusqlite::params![foreign.as_str().unwrap()],
            |row| row.get(0),
        )
        .expect("another space keeps its own item");
    assert_eq!(elsewhere, "Another space still needs this.");
}

#[test]
fn adoption_refusals_change_nothing() {
    let adopt = |f: &Fixture, pane: &str, id: &Value| {
        f.handoff(
            pane,
            "handoff.adopt",
            json!({"request_id":"adopt","coordination_id":id}),
        )
    };
    let retire_lead = |f: &Fixture| {
        StateStore::open(&f.state)
            .unwrap()
            .transaction(|tx| {
                tx.retire_pane_address(
                    &StateNamespace::for_space("synthetic-socket", "synthetic-workspace"),
                    &PaneAddressId::new("lead"),
                    &MutationAttribution::new(ActorKind::Human, "synthetic-retirement", 100),
                )
            })
            .unwrap();
    };
    let mut refusals = Vec::new();

    let f = Fixture::new();
    let id = f.begin("a")["coordination_id"].clone();
    f.replace_session("lead", "replacement-lead");
    let error = f.rejects_unchanged(|| adopt(&f, "worker", &id));
    refusals.push(("different address", "belongs to another address", error));

    for (case, pane, expected) in [
        (
            "retired address, own pane",
            "lead",
            "does not have an active Pane Address",
        ),
        (
            "retired address, other pane",
            "worker",
            "belongs to another address",
        ),
    ] {
        let f = Fixture::new();
        let id = f.begin("a")["coordination_id"].clone();
        retire_lead(&f);
        let error = f.rejects_unchanged(|| adopt(&f, pane, &id));
        refusals.push((case, expected, error));
    }

    let f = Fixture::new();
    let id = f.begin("a")["coordination_id"].clone();
    let error = f.rejects_unchanged(|| adopt(&f, "lead", &id));
    refusals.push(("original owner", "already owns the coordination", error));

    let f = Fixture::new();
    let id = f.begin("a")["coordination_id"].clone();
    *f.session_override.lock().unwrap() = Some(("lead", "replacement-session"));
    let error = f.rejects_unchanged(|| adopt(&f, "lead", &id));
    refusals.push((
        "live view replaced, store not observed",
        "live native Agent Session",
        error,
    ));
    *f.session_override.lock().unwrap() = None;
    let error = f.rejects_unchanged(|| adopt(&f, "lead", &id));
    refusals.push((
        "owner session still current",
        "already owns the coordination",
        error,
    ));

    for (outcome, extra) in [
        ("completed", json!({"reconciled_assignments":[]})),
        ("blocked", json!({"reason":"Synthetic blocker."})),
        ("cancelled", json!({"reason":"Synthetic cancellation."})),
    ] {
        let f = Fixture::new();
        let id = f.begin("a")["coordination_id"].clone();
        let mut resolve = json!({"request_id":"resolve","coordination_id":id,"outcome":outcome,
            "report":"Synthetic final report."});
        resolve
            .as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        f.handoff("lead", "handoff.resolve", resolve).unwrap();
        f.replace_session("lead", "replacement-lead");
        let error = f.rejects_unchanged(|| adopt(&f, "lead", &id));
        refusals.push((outcome, "coordination is already resolved", error));
        let error = f.rejects_unchanged(|| {
            f.handoff("lead", "handoff.results", json!({"coordination_id":id}))
        });
        refusals.push((
            outcome,
            "another session of this address and is already resolved",
            error,
        ));
    }

    let f = Fixture::new();
    f.replace_session("lead", "replacement-lead");
    let error = f.rejects_unchanged(|| adopt(&f, "lead", &json!("missing")));
    refusals.push(("unknown id", "does not exist in this workspace", error));

    let wrong = refusals
        .iter()
        .filter(|(_, expected, error)| !error.contains(expected))
        .collect::<Vec<_>>();
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[test]
fn adopted_then_resolved_coordination_releases_the_updater() {
    let f = Fixture::new();
    let id = f.begin("adopt-gate")["coordination_id"].clone();
    let request = f.send(&id, "gate-work", "@worker synthetic gated work");
    let reply = f.reply(&request, "worker", "gate-result", Some("completed"));
    f.replace_session("lead", "replacement-lead");
    assert_eq!(f.delivery_state(&reply["message_id"], "lead"), "held");
    let names = |state: &Value, outcome: &str| {
        state["details"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d[2] == id && d[4] == outcome)
    };

    let open = f.updater_state();
    assert_eq!(open["ready"], false);
    assert!(names(&open, "coordination:open"), "{open}");
    f.handoff(
        "lead",
        "handoff.adopt",
        json!({"request_id":"adopt-1","coordination_id":id}),
    )
    .unwrap();
    assert_eq!(f.delivery_state(&reply["message_id"], "lead"), "held");
    let adopted = f.updater_state();
    assert_eq!(adopted["ready"], false);
    assert!(names(&adopted, "coordination:open"), "{adopted}");

    let resolved = f
        .handoff(
            "lead",
            "handoff.resolve",
            json!({"request_id":"adopted-resolve","coordination_id":id,"outcome":"completed",
                "report":"Synthetic adopted report.",
                "reconciled_assignments":[{"assignment_id":request["coordination"][0]["assignment_id"],"revision":1}]}),
        )
        .unwrap();
    let pending = f.updater_state();
    assert_eq!(pending["ready"], false);
    assert!(names(&pending, "coordination:completed"), "{pending}");
    let cancelled = execute_control_operation(
        "delivery.cancel-held",
        json!({"attempt_id":f.attempt(&reply["message_id"],"lead"),"confirmed":true}),
        &f.runtime("lead"),
    )
    .unwrap();
    assert_eq!(cancelled["state"], "cancelled");
    drain_mailboxes(&f.runtime("lead")).unwrap();
    f.complete(&resolved["report_message_id"], "lead");
    let released = f.updater_state();
    assert_eq!(released["ready"], true, "{released}");
    assert!(
        !released["details"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d[2] == id),
        "{released}"
    );
}
