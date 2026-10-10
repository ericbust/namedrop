use namedrop::*;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
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
    statuses: Mutex<BTreeMap<String, String>>,
    sessions: Mutex<BTreeMap<String, String>>,
    prompts: Mutex<Vec<(String, Value)>>,
}
impl Fixture {
    fn new() -> Self {
        Self::with_profile(complete_capability_profile())
    }
    fn with_profile(profile: &[String]) -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-acceptance-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let f = Self {
            state: root.join("state"),
            home: root.join("home"),
            root,
            ids: AtomicU64::new(1),
            statuses: Mutex::new(BTreeMap::from([
                ("lead".into(), "working".into()),
                ("worker".into(), "idle".into()),
                ("reviewer".into(), "idle".into()),
            ])),
            sessions: Mutex::new(BTreeMap::new()),
            prompts: Mutex::new(Vec::new()),
        };
        fs::create_dir_all(&f.home).unwrap();
        let by = MutationAttribution::new(ActorKind::Human, "synthetic-user", 1);
        StateStore::open(&f.state)
            .unwrap()
            .transaction(|tx| {
                let ns = f.ns();
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
                tx.complete_reconciliation(&ns)
            })
            .unwrap();
        for name in ["lead", "worker", "reviewer"] {
            f.register(name, profile);
        }
        f
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
    fn register(&self, pane: &str, profile: &[String]) {
        let r = self.runtime(pane);
        let c = issue_registration_challenge(&PaneAddressId::new(pane), AgentRuntime::Codex, &r)
            .unwrap();
        execute_integration_operation("register",json!({"runtime":"codex","protocol_version":"collab/v1","nonce":c.nonce,"capabilities":profile}),&r).unwrap();
    }
    fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.state.join("namedrop.sqlite3")).unwrap()
    }
    fn run(&self, pane: &str, token: &str, input: Value) -> Result<Value, String> {
        let r = self.runtime(pane);
        match token {
            "send" | "reply" => execute_message_operation(token, input, &r),
            "handoff.begin" | "handoff.resolve" | "handoff.results" | "handoff.status" => {
                execute_handoff_operation(token, input, &r)
            }
            "context.inspect" | "context.read" => execute_context_operation(token, input, &r),
            "complete.no-reply" => execute_integration_operation("complete", input, &r),
            _ => panic!("unexpected bound action {token}"),
        }
    }
    fn bound(&self, envelope: &Value, token: &str) -> Value {
        envelope["control"]["tool"]["operations"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["token"] == token)
            .unwrap()
            .clone()
    }
    fn execute(&self, pane: &str, action: Value) -> Value {
        // Execute the emitted token and input, and check its emitted argv against
        // the real catalog. Native acceptance executes that argv through the CLI.
        let token = action["token"].as_str().unwrap();
        assert_eq!(
            action["argv"],
            json!(operation_catalog().operation(token).unwrap().argv)
        );
        self.run(pane, token, action["input"].clone()).unwrap()
    }
    fn begin(&self) -> Value {
        self.run("lead","handoff.begin",json!({"request_id":"begin","deliverable":"Review two separate synthetic assignments and give one final answer."})).unwrap()["coordination_id"].clone()
    }
    fn send(&self, id: &Value, key: &str, source: &str) -> Value {
        self.run("lead","send",json!({"coordination_id":id,"request_id":key,"source":source,"author_kind":"agent_session"})).unwrap()
    }
    fn envelope(&self, pane: &str, message: &Value) -> Value {
        self.prompts
            .lock()
            .unwrap()
            .iter()
            .find(|(p, e)| p == pane && e["control"]["message_id"] == *message)
            .unwrap()
            .1
            .clone()
    }
    fn reply(&self, pane: &str, message: &Value, body: &str, outcome: Option<&str>) -> Value {
        let e = self.envelope(pane, message);
        let mut a = self.bound(&e, "reply");
        a["input"]["source"] = json!(format!("@lead {body}"));
        if let Some(outcome) = outcome {
            a["input"]["work_outcome"] = json!(outcome);
        }
        self.execute(pane, a)
    }
    fn complete(&self, pane: &str, e: &Value) {
        self.execute(pane, self.bound(e, "complete.no-reply"));
    }
    fn status(&self, pane: &str, status: &str) {
        self.statuses
            .lock()
            .unwrap()
            .insert(pane.into(), status.into());
    }
    fn drain(&self) {
        drain_mailboxes(&self.runtime("lead")).unwrap();
    }
    fn results(&self, id: &Value) -> Value {
        self.run("lead", "handoff.results", json!({"coordination_id":id}))
            .unwrap()
    }
    fn settle_receipts(&self) {
        for _ in 0..12 {
            self.drain();
            let inbox =
                execute_integration_operation("delivery.inbox", json!({}), &self.runtime("lead"))
                    .unwrap();
            let pending = inbox["envelopes"].as_array().unwrap();
            if pending.is_empty() {
                return;
            }
            for e in pending {
                if e["control"]["coordination_nudge"] == true
                    || !e["control"]["result_set"].is_null()
                    || e["control"]["reply_mode"] == "report_to_human"
                {
                    return;
                }
                self.complete("lead", e);
            }
        }
        panic!("receipts did not settle");
    }
    fn nudges(&self) -> Vec<Value> {
        self.prompts
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, e)| e["control"]["coordination_nudge"] == true)
            .map(|(_, e)| e.clone())
            .collect()
    }
    fn snapshot(&self) -> Vec<String> {
        let db = self.db();
        db.prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name").unwrap().query_map([],|r|r.get::<_,String>(0)).unwrap().map(|name| {
            let name=name.unwrap();
            let mut s=db.prepare(&format!("SELECT * FROM {name} ORDER BY rowid")).unwrap();
            let n=s.column_count();
            format!("{name}:{:?}",s.query_map([],|r|(0..n).map(|i|r.get::<_,rusqlite::types::Value>(i)).collect::<Result<Vec<_>,_>>()).unwrap().collect::<Result<Vec<_>,_>>().unwrap())
        }).collect()
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
    fn wait_ms(&self, _: u64) {}
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
        let value = match args {
            ["worktree", "list", ..] => {
                json!({"result":{"worktrees":[{"path":self.root,"open_workspace_id":"synthetic-workspace"}]}})
            }
            ["pane", "list", ..] => {
                let statuses = self.statuses.lock().unwrap();
                let sessions = self.sessions.lock().unwrap();
                json!({"result":{"panes":(["lead","worker","reviewer"].map(|p|json!({"pane_id":p,"terminal_id":format!("terminal-{p}"),"tab_id":"synthetic-tab","workspace_id":"synthetic-workspace","agent_status":statuses[p],"agent_session":{"value":sessions.get(p).cloned().unwrap_or_else(||format!("session-{p}"))}})))}})
            }
            ["agent", "prompt", pane, prompt] => {
                self.prompts
                    .lock()
                    .unwrap()
                    .push(((*pane).into(), serde_json::from_str(prompt).unwrap()));
                json!({})
            }
            ["pane", "report-metadata", ..] => json!({}),
            _ => return Err(format!("unexpected fixture command {args:?}")),
        };
        Ok(HerdrCommandOutput {
            stdout: value.to_string(),
        })
    }
}

// This oracle consumes actual results, captured delivery prompts, and persisted
// attempt/disposition identities. Its mutation controls below challenge each
// decisive criterion; a printed token alone is never acceptance evidence.
fn valid_report(e: &Value) -> bool {
    let r = &e["results"];
    let Some(report) = r["report"].as_str().filter(|v| !v.trim().is_empty()) else {
        return false;
    };
    let Some(assignments) = r["assignments"].as_array() else {
        return false;
    };
    let actual: BTreeMap<_, _> = assignments
        .iter()
        .map(|a| (a["assignment_id"].to_string(), a["revision"].clone()))
        .collect();
    let expected: BTreeMap<_, _> = e["expected"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| (a["assignment_id"].to_string(), a["revision"].clone()))
        .collect();
    let wakes = e["wakes"].as_array().unwrap();
    let Some(coordination) = r["coordination_id"].as_str() else {
        return false;
    };
    let prefix = format!("coordination-wake:{coordination}:");
    let revisions: Option<BTreeSet<i64>> = wakes
        .iter()
        .map(|w| {
            w["control"]["message_id"]
                .as_str()?
                .strip_prefix(&prefix)?
                .parse::<i64>()
                .ok()
                .filter(|revision| *revision > 0)
        })
        .collect();
    r["outcome"] == "completed"
        && r["report_saved"] == true
        && r["report_delivery_settled"] == true
        && r["presentation"] == "unknown"
        && assignments.len() == 2
        && actual.len() == 2
        && actual == expected
        && assignments.iter().all(|a| a["work_outcome"] == "completed")
        && wakes.len() == 2
        && wakes[0]["control"]["attempt_id"] != wakes[1]["control"]["attempt_id"]
        && revisions.is_some_and(|revisions| revisions.len() == 2)
        && wakes.iter().all(|w| {
            w["control"]["coordination_nudge"] == true
                && w["control"]["coordination_id"] == r["coordination_id"]
        })
        && e["report_envelope"]["control"]["message_id"] == r["report_message_id"]
        && e["report_envelope"]["control"]["coordination_id"] == r["coordination_id"]
        && e["report_envelope"]["control"]["reply_mode"] == "report_to_human"
        && e["report_envelope"]["untrusted_body"] == report
        && e["report_session"] == "session-lead"
        && e["report_dispositions"] == 1
}

fn separate_assignments() -> Value {
    let f = Fixture::new();
    let id = f.begin();
    let a = f.send(
        &id,
        "implementation",
        "@worker Compute the synthetic checksum.",
    );
    let b = f.send(
        &id,
        "review",
        "@reviewer Independently review the synthetic bounds.",
    );
    assert_ne!(a["message_id"], b["message_id"]);
    assert_ne!(
        f.envelope("worker", &a["message_id"])["untrusted_body"],
        f.envelope("reviewer", &b["message_id"])["untrusted_body"]
    );
    f.reply(
        "worker",
        &a["message_id"],
        "Partial checksum; the final edge remains.",
        Some("needs_continuation"),
    );
    f.reply(
        "reviewer",
        &b["message_id"],
        "Bounds checked, including zero.",
        Some("completed"),
    );
    assert_eq!(f.results(&id)["outcome"], "open");
    let before = f.snapshot();
    assert!(
        f.run(
            "lead",
            "handoff.resolve",
            json!({
                "request_id":"premature-final", "coordination_id":id, "outcome":"completed",
                "report":"Premature synthetic answer.", "reconciled_assignments":[
                    {"assignment_id":a["coordination"][0]["assignment_id"],"revision":1},
                    {"assignment_id":b["coordination"][0]["assignment_id"],"revision":1}]
            })
        )
        .is_err()
    );
    assert_eq!(
        before,
        f.snapshot(),
        "settled progress cannot resolve unfinished work"
    );
    f.status("lead", "idle");
    f.settle_receipts();
    let nudge = f.nudges().pop().unwrap();
    let results = f.execute("lead", f.bound(&nudge, "handoff.results"));
    assert_eq!(results["outcome"], "open");
    let mut continuation = nudge["control"]["tool"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| {
            o["token"] == "send"
                && o["input"]["assignment_id"] == a["coordination"][0]["assignment_id"]
        })
        .unwrap()
        .clone();
    continuation["input"]["source"] =
        json!("@worker Correct the final checksum edge and report the complete value.");
    f.status("lead", "working");
    let corrected = f.execute("lead", continuation.clone());
    assert_eq!(corrected["coordination"][0]["revision"], 2);
    let before = f.snapshot();
    let mut stale = continuation.clone();
    stale["input"]["request_id"] = json!("stale-correction");
    assert!(f.run("lead", "send", stale["input"].clone()).is_err());
    assert_eq!(before, f.snapshot());
    f.complete("lead", &nudge);
    let worker = f.envelope("worker", &corrected["message_id"]);
    let mut completion = f.bound(&worker, "reply");
    completion["input"]["source"] =
        json!("@lead Complete synthetic checksum: 42; zero and final edge verified.");
    completion["input"]["work_outcome"] = json!("completed");
    f.sessions
        .lock()
        .unwrap()
        .insert("worker".into(), "replacement-worker".into());
    let before = f.snapshot();
    assert!(
        f.run("worker", "reply", completion["input"].clone())
            .is_err()
    );
    assert_eq!(before, f.snapshot());
    f.sessions.lock().unwrap().remove("worker");
    f.execute("worker", completion.clone());
    let before = f.snapshot();
    f.execute("worker", completion);
    assert_eq!(
        before,
        f.snapshot(),
        "duplicate semantic reply must be idempotent"
    );
    f.status("lead", "idle");
    f.settle_receipts();
    assert_eq!(
        f.nudges().len(),
        2,
        "one nudge for each changed semantic revision"
    );
    f.drain();
    f.drain();
    assert_eq!(f.nudges().len(), 2);
    let last = f.nudges().pop().unwrap();
    f.execute("lead", f.bound(&last, "handoff.results"));
    let mut resolve = last["control"]["tool"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["token"] == "handoff.resolve" && o["input"]["outcome"] == "completed")
        .unwrap()
        .clone();
    let expected = resolve["input"]["reconciled_assignments"].clone();
    let mut empty = resolve.clone();
    empty["input"]["report"] = json!(" \n");
    let before = f.snapshot();
    assert!(
        f.run("lead", "handoff.resolve", empty["input"].clone())
            .is_err()
    );
    assert_eq!(before, f.snapshot());
    resolve["input"]["report"] =
        json!("Checksum 42 and bounds reviewed; the corrected final edge and zero case passed.");
    f.status("lead", "working");
    let saved = f.execute("lead", resolve);
    f.complete("lead", &last);
    f.status("lead", "idle");
    f.drain();
    let report = f.envelope("lead", &saved["report_message_id"]);
    f.complete("lead", &report);
    let attempt = report["control"]["attempt_id"].as_str().unwrap();
    let session: String = f
        .db()
        .query_row(
            "SELECT agent_session_id FROM delivery_attempts WHERE attempt_id=?1",
            [attempt],
            |r| r.get(0),
        )
        .unwrap();
    let count: i64 = f
        .db()
        .query_row(
            "SELECT count(*) FROM delivery_dispositions WHERE attempt_id=?1",
            [attempt],
            |r| r.get(0),
        )
        .unwrap();
    json!({"results":f.results(&id),"wakes":f.nudges(),"expected":expected,"report_envelope":report,"report_session":session,"report_dispositions":count})
}

#[test]
fn coordination_acceptance_oracle_controls() {
    let evidence = separate_assignments();
    assert!(valid_report(&evidence), "{evidence:#}");
    let mut stalled = evidence.clone();
    stalled["results"]["outcome"] = json!("open");
    assert!(!valid_report(&stalled));
    let mut empty = evidence.clone();
    empty["results"]["report"] = json!(" \n");
    assert!(!valid_report(&empty));
    let mut duplicate = evidence.clone();
    duplicate["wakes"]
        .as_array_mut()
        .unwrap()
        .push(evidence["wakes"][0].clone());
    assert!(!valid_report(&duplicate));
    // Distinct attempts do not make a repeated router wake new semantic work.
    let mut repeated_message = evidence.clone();
    repeated_message["wakes"][1]["control"]["message_id"] =
        evidence["wakes"][0]["control"]["message_id"].clone();
    assert_ne!(
        repeated_message["wakes"][0]["control"]["attempt_id"],
        repeated_message["wakes"][1]["control"]["attempt_id"]
    );
    assert!(!valid_report(&repeated_message));
    // The wire ID is a string; even a different spelling of one revision must
    // not pass as a second semantic wake (the router parses a numeric revision).
    let mut repeated_revision = evidence.clone();
    let first = evidence["wakes"][0]["control"]["message_id"]
        .as_str()
        .unwrap();
    let (prefix, revision) = first.rsplit_once(':').unwrap();
    repeated_revision["wakes"][1]["control"]["message_id"] = json!(format!("{prefix}:0{revision}"));
    assert_ne!(
        repeated_revision["wakes"][0]["control"]["message_id"],
        repeated_revision["wakes"][1]["control"]["message_id"]
    );
    assert!(!valid_report(&repeated_revision));
    for message in [
        format!("{prefix}:0"),
        format!("{prefix}:invalid"),
        "coordination-wake:foreign-synthetic-coordination:1".into(),
    ] {
        let mut invalid_identity = evidence.clone();
        invalid_identity["wakes"][1]["control"]["message_id"] = json!(message);
        assert!(!valid_report(&invalid_identity));
    }
    let mut wrong = evidence.clone();
    wrong["report_session"] = json!("replacement-lead");
    assert!(!valid_report(&wrong));
    let mut progress = evidence.clone();
    progress["results"]["assignments"][0]["work_outcome"] = json!("needs_continuation");
    assert!(!valid_report(&progress));
    let mut stale = evidence.clone();
    stale["results"]["assignments"][0]["revision"] = json!(0);
    assert!(!valid_report(&stale));
    let mut undisposed = evidence;
    undisposed["report_dispositions"] = json!(0);
    assert!(!valid_report(&undisposed));
}
#[test]
fn coordination_separate_assignments_end_to_end() {
    assert!(valid_report(&separate_assignments()));
}

#[test]
fn coordination_protocol_and_legacy_routes() {
    let f = Fixture::new();
    let id = f.begin();
    let request = f.send(
        &id,
        "tracked-join",
        "@worker @reviewer Check the same synthetic boundary.",
    );
    for pane in ["worker", "reviewer"] {
        f.reply(
            pane,
            &request["message_id"],
            "Synthetic check completed.",
            Some("completed"),
        );
    }
    f.status("lead", "idle");
    f.settle_receipts();
    let joined = f
        .prompts
        .lock()
        .unwrap()
        .iter()
        .find(|(_, e)| !e["control"]["result_set"].is_null())
        .unwrap()
        .1
        .clone();
    assert_eq!(joined["control"]["coordination_id"], id);
    let actions = joined["control"]["tool"]["operations"].as_array().unwrap();
    for token in ["address.list", "handoff.preview"] {
        assert!(actions.iter().any(|op| op["token"] == token));
    }
    assert!(actions.iter().any(|op| op["token"] == "send"
        && op["input"].get("assignment_id").is_none()
        && op["input"]["coordination_id"] == id
        && op["input"].get("context").is_some()));

    assert_eq!(
        f.execute("lead", f.bound(&joined, "handoff.results"))["outcome"],
        "open"
    );
    let mut resolve = joined["control"]["tool"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["token"] == "handoff.resolve" && o["input"]["outcome"] == "completed")
        .unwrap()
        .clone();
    resolve["input"]["report"] = json!("Both synthetic boundary checks reviewed.");
    f.status("lead", "working");
    let saved = f.execute("lead", resolve);
    f.complete("lead", &joined);
    f.status("lead", "idle");
    f.drain();
    let report = f.envelope("lead", &saved["report_message_id"]);
    f.complete("lead", &report);
    assert_eq!(f.results(&id)["presentation"], "unknown");

    let profile: Vec<_> = complete_capability_profile()
        .iter()
        .filter(|v| {
            !matches!(
                v.as_str(),
                "handoff.adopt" | "handoff.begin" | "handoff.resolve"
            )
        })
        .cloned()
        .collect();
    assert_eq!(profile.len(), 14);
    assert_eq!(complete_capability_profile().len(), 17);
    let legacy = Fixture::with_profile(&profile);
    legacy.db().execute(
        "INSERT INTO coordinations(session_socket_id,worktree_id,coordination_id,request_id,owner_address_id,owner_session_id,deliverable,created_at_ms,updated_at_ms)
         VALUES ('synthetic-socket','synthetic-workspace','older-coordination','older-begin','lead','older-lead-session','Left open by an earlier lead session',1,1)",
        [],
    ).unwrap();
    assert_eq!(
        legacy.run("lead", "handoff.status", json!({})).unwrap()["adoptable"],
        json!([]),
        "a session without the tracked-work capabilities is offered nothing to adopt"
    );
    let before = legacy.snapshot();
    assert!(
        legacy
            .run(
                "lead",
                "handoff.begin",
                json!({"request_id":"refused","deliverable":"unsupported"})
            )
            .is_err()
    );
    assert_eq!(before, legacy.snapshot());
    let sent=legacy.run("lead","send",json!({"request_id":"legacy-join","source":"@worker @reviewer Return two synthetic ideas.","author_kind":"agent_session"})).unwrap();
    for pane in ["worker", "reviewer"] {
        legacy.reply(pane, &sent["message_id"], "One synthetic idea.", None);
    }
    legacy.status("lead", "idle");
    legacy.settle_receipts();
    let joined = legacy
        .prompts
        .lock()
        .unwrap()
        .iter()
        .find(|(_, e)| !e["control"]["result_set"].is_null())
        .unwrap()
        .1
        .clone();
    assert!(joined["control"]["coordination_id"].is_null());
    assert_eq!(joined["control"]["result_set"]["expected"], 2);
    assert!(
        !joined["control"]["tool"]["operations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["token"] == "handoff.resolve")
    );
    legacy.complete("lead", &joined);
    let instruction = joined["trusted_instruction"].as_str().unwrap();
    assert!(instruction.contains("unless tracked, run bound complete.no-reply then emit a non-empty final response fulfilling the original user deliverable"));
    assert!(
        instruction.find("context_request:").unwrap()
            < instruction.find("Tracked result_set").unwrap()
    );

    // Context fulfillment keeps first priority even within tracked work.
    let context = Fixture::new();
    let id = context.begin();
    let sent = context.send(&id, "context-work", "@worker Review synthetic context.");
    let request = context.envelope("worker", &sent["message_id"]);
    let mut ask = context.bound(&request, "reply");
    ask["input"]["source"] = json!("@lead Need the synthetic context.");
    ask["input"]["request_context"] = json!(true);
    let asked = context.execute("worker", ask);
    context.status("lead", "idle");
    context.drain();
    let e = context.envelope("lead", &asked["message_id"]);
    assert!(!e["control"]["context_request"].is_null());
    let mut fulfill = context.bound(&e, "reply");
    fulfill["input"]["source"] = json!("@worker Synthetic context supplied.");
    fulfill["input"]["context"] = json!({"capture_git_state":false,"snapshot":{"objective":"Synthetic boundary review","applicable_user_instructions":[],"decisions":[],"completed_work":[],"current_work":["Review zero"],"blockers":[],"evidence":[]}});
    let supplied = context.execute("lead", fulfill);
    context.drain();
    let attached = context.envelope("worker", &supplied["message_id"]);
    let reads: Vec<_> = attached["control"]["tool"]["operations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["token"] == "context.read")
        .cloned()
        .collect();
    assert!(!reads.is_empty());
    for action in reads {
        let data = context.execute("worker", action);
        assert!(data.to_string().contains("Synthetic boundary review"));
    }
    context.reply(
        "worker",
        &supplied["message_id"],
        "Synthetic context reviewed.",
        Some("completed"),
    );
}
