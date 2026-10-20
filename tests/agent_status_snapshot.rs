//! Snapshot collection for the agent status panel.
//!
//! The snapshot is the panel's only source of identity and dependency truth, so
//! every test here pairs a positive fixture with the violation it must refuse:
//! a replaced session, a pane in another space, a wait invented from pane text,
//! an unreadable store reported as emptiness, or a read that outruns its budget.

use namedrop::{
    ActorKind, AgentRuntime, AgentSessionId, Clock, HerdrCommandExecutor, HerdrCommandOutput,
    IdGenerator, LifecycleRuntime, MutationAttribution, PaneAddressDraft, PaneAddressId,
    PaneBindingId, PaneRoleId, StateNamespace, StateStore, collect_agent_status,
    complete_capability_profile, drain_mailboxes, execute_integration_operation,
    execute_message_operation, issue_registration_challenge,
};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    fs,
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

/// `collect_agent_status` reads `HERDR_PANE_ID` to learn which pane the panel
/// itself occupies. The process environment is global, so every collection in
/// this binary runs under this lock and nothing else here reads the
/// environment; that keeps the `set_var` calls below free of a concurrent read.
static ENVIRONMENT: Mutex<()> = Mutex::new(());

struct TestDirectory {
    root: PathBuf,
    repo: PathBuf,
    state: PathBuf,
    home: PathBuf,
}

impl TestDirectory {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-agent-status-test-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        let repo = root.join("repo");
        let state = root.join("state");
        let home = root.join("home");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(&home).unwrap();
        Self {
            root,
            repo,
            state,
            home,
        }
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Stands still until a Herdr call charges it, so a test spends the collection
/// budget exactly where it means to instead of sleeping or counting how many
/// times the code happens to read the clock.
struct TestClock {
    now_ms: AtomicU64,
    step_ms: AtomicU64,
}

impl TestClock {
    fn fixed() -> Self {
        Self {
            now_ms: AtomicU64::new(1_000),
            step_ms: AtomicU64::new(0),
        }
    }

    fn advance(&self, milliseconds: u64) {
        self.now_ms.fetch_add(milliseconds, Ordering::Relaxed);
    }

    /// Charge every reading of the clock. Used to spend the budget inside work
    /// that makes no Herdr call of its own, such as the state store read.
    fn step(&self, milliseconds: u64) {
        self.step_ms.store(milliseconds, Ordering::Relaxed);
    }
}

impl Clock for TestClock {
    fn now_unix_ms(&self) -> u64 {
        self.now_ms
            .fetch_add(self.step_ms.load(Ordering::Relaxed), Ordering::Relaxed)
    }

    fn wait_ms(&self, _duration_ms: u64) {}
}

struct SequenceIds(AtomicU64);

impl IdGenerator for SequenceIds {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
    }
}

fn pane(
    pane_id: &str,
    terminal_id: &str,
    agent: Option<&str>,
    session: Option<&str>,
    status: &str,
    title: &str,
) -> Value {
    let mut entry = json!({
        "pane_id": pane_id,
        "terminal_id": terminal_id,
        "tab_id": "tab-1",
        "workspace_id": "workspace-1",
        "cwd": "/tmp/repo",
        "agent_status": status,
        "title": title,
    });
    if let Some(agent) = agent {
        entry["agent"] = json!(agent);
    }
    if let Some(session) = session {
        entry["agent_session"] = json!({"value": session});
    }
    entry
}

fn default_panes() -> Vec<Value> {
    vec![
        pane(
            "pane-lead",
            "terminal-lead",
            Some("claude"),
            Some("session-lead"),
            "working",
            "Lead",
        ),
        pane(
            "pane-reviewer",
            "terminal-reviewer",
            Some("codex"),
            Some("session-reviewer"),
            "idle",
            "Reviewer",
        ),
        pane(
            "pane-tests",
            "terminal-tests",
            Some("claude"),
            Some("session-tests"),
            "idle",
            "Tests",
        ),
        pane(
            "pane-docs",
            "terminal-docs",
            Some("codex"),
            Some("session-docs"),
            "working",
            "Docs",
        ),
        pane(
            "pane-shell",
            "terminal-shell",
            None,
            None,
            "unknown",
            "build shell",
        ),
        pane(
            "pane-panel",
            "terminal-panel",
            None,
            None,
            "unknown",
            "namedrop agent status",
        ),
    ]
}

struct FakeHerdr {
    clock: Arc<TestClock>,
    /// Milliseconds every Herdr call costs, and the extra a pane read costs.
    command_cost_ms: AtomicU64,
    read_cost_ms: AtomicU64,
    /// Milliseconds charged per clock reading between the pane listing and the
    /// first pane read, which is exactly the window the state store read runs in.
    state_read_step_ms: AtomicU64,
    /// A pane whose output never comes back: the read spends exactly the
    /// timeout it was handed and then fails, the way a stalled subprocess does.
    stalled: Mutex<Option<String>>,
    /// Every timeout this executor was handed, per command.
    timeouts: Mutex<Vec<(String, u64)>>,
    /// After serving a read for the first pane, give the second pane a new
    /// terminal and session: the pane id stays, the pane behind it does not.
    replace_after_read: Mutex<Option<(String, String)>>,
    worktree: String,
    panes: Mutex<Vec<Value>>,
    excerpts: Mutex<BTreeMap<String, String>>,
    oversized: Mutex<Option<String>>,
    prompt_failures: Mutex<Vec<String>>,
    prompts: Mutex<Vec<(String, String)>>,
    commands: Mutex<Vec<String>>,
    cancel_on_read: Mutex<Option<Arc<AtomicBool>>>,
}

impl FakeHerdr {
    fn new(directory: &TestDirectory, clock: Arc<TestClock>) -> Self {
        let excerpts = default_panes()
            .iter()
            .map(|entry| {
                let pane_id = entry["pane_id"].as_str().unwrap().to_owned();
                let text = format!("{pane_id} recent output\nstill going\n");
                (pane_id, text)
            })
            .collect();
        Self {
            clock,
            command_cost_ms: AtomicU64::new(0),
            state_read_step_ms: AtomicU64::new(0),
            stalled: Mutex::new(None),
            timeouts: Mutex::new(Vec::new()),
            read_cost_ms: AtomicU64::new(0),
            replace_after_read: Mutex::new(None),
            worktree: directory.repo.display().to_string(),
            panes: Mutex::new(default_panes()),
            excerpts: Mutex::new(excerpts),
            oversized: Mutex::new(None),
            prompt_failures: Mutex::new(Vec::new()),
            prompts: Mutex::new(Vec::new()),
            commands: Mutex::new(Vec::new()),
            cancel_on_read: Mutex::new(None),
        }
    }

    fn set_excerpt(&self, pane_id: &str, text: &str) {
        self.excerpts
            .lock()
            .unwrap()
            .insert(pane_id.to_owned(), text.to_owned());
    }

    fn edit_pane(&self, pane_id: &str, edit: impl FnOnce(&mut Value)) {
        let mut panes = self.panes.lock().unwrap();
        let entry = panes
            .iter_mut()
            .find(|entry| entry["pane_id"] == pane_id)
            .unwrap();
        edit(entry);
    }

    /// Charge every Herdr call, and pane reads extra, against the collection
    /// budget the snapshot is spending.
    fn charge(&self, command_ms: u64, read_ms: u64) {
        self.command_cost_ms.store(command_ms, Ordering::Relaxed);
        self.read_cost_ms.store(read_ms, Ordering::Relaxed);
    }

    /// Make this pane's output read hang until its timeout runs out.
    fn stall(&self, pane_id: &str) {
        self.stalled.lock().unwrap().replace(pane_id.to_owned());
    }

    /// The timeout handed to the first command starting with `prefix`.
    fn timeout_for(&self, prefix: &str) -> u64 {
        let timeouts = self.timeouts.lock().unwrap();
        timeouts
            .iter()
            .find(|(command, _)| command.starts_with(prefix))
            .unwrap_or_else(|| panic!("no command started with {prefix}"))
            .1
    }

    /// Charge the state store read, which makes no Herdr call of its own.
    fn charge_state_read(&self, per_reading_ms: u64) {
        self.state_read_step_ms
            .store(per_reading_ms, Ordering::Relaxed);
    }

    /// Swap the pane behind `target`'s id once `trigger`'s output is read.
    fn replace_after_read(&self, trigger: &str, target: &str) {
        self.replace_after_read
            .lock()
            .unwrap()
            .replace((trigger.to_owned(), target.to_owned()));
    }

    fn remove_pane(&self, pane_id: &str) {
        self.panes
            .lock()
            .unwrap()
            .retain(|e| e["pane_id"] != pane_id);
    }

    fn envelope_for(&self, pane_id: &str) -> Value {
        let prompts = self.prompts.lock().unwrap();
        let (_, envelope) = prompts
            .iter()
            .rev()
            .find(|(pane, _)| pane == pane_id)
            .unwrap_or_else(|| panic!("nothing was dispatched to {pane_id}"));
        serde_json::from_str(envelope).unwrap()
    }
}

impl HerdrCommandExecutor for FakeHerdr {
    /// Herdr's real executor spends the timeout it is given, so this one does
    /// too: it records every timeout, and a stalled pane read burns exactly
    /// that much of the collection budget before failing.
    fn execute_bounded(
        &self,
        args: &[&str],
        timeout: Duration,
        cancelled: &AtomicBool,
        max_output_bytes: usize,
    ) -> Result<HerdrCommandOutput, String> {
        let timeout_ms = u64::try_from(timeout.as_millis()).unwrap();
        self.timeouts
            .lock()
            .unwrap()
            .push((args.join(" "), timeout_ms));
        if cancelled.load(Ordering::Relaxed) {
            return Err("Herdr read cancelled".to_owned());
        }
        if let ["pane", "read", pane_id, ..] = args
            && self.stalled.lock().unwrap().as_deref() == Some(*pane_id)
        {
            self.commands.lock().unwrap().push(args.join(" "));
            self.clock.advance(timeout_ms);
            return Err(format!("Herdr command timed out after {timeout_ms} ms"));
        }
        let output = self.execute(args)?;
        if output.stdout.len() > max_output_bytes {
            return Err("Herdr read exceeded its output limit".to_owned());
        }
        Ok(output)
    }

    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        self.commands.lock().unwrap().push(args.join(" "));
        self.clock
            .advance(self.command_cost_ms.load(Ordering::Relaxed));
        let stdout = match args {
            ["workspace", "list"] => {
                json!({"result":{"workspaces":[{"workspace_id":"workspace-1"}]}})
                    .to_string()
            }
            ["worktree", "list", "--workspace", "workspace-1", "--json"]
            | ["worktree", "list", "--cwd", _, "--json"] => {
                json!({"result":{"worktrees":[{"path":self.worktree,"open_workspace_id":"workspace-1"}]}})
                    .to_string()
            }
            ["pane", "list", "--workspace", "workspace-1"] => {
                self.clock
                    .step(self.state_read_step_ms.load(Ordering::Relaxed));
                json!({"result":{"panes":*self.panes.lock().unwrap()}}).to_string()
            }
            [
                "pane",
                "read",
                pane_id,
                "--source",
                "recent-unwrapped",
                "--lines",
                "40",
            ] => {
                self.clock.step(0);
                self.clock.advance(self.read_cost_ms.load(Ordering::Relaxed));
                if let Some(flag) = self.cancel_on_read.lock().unwrap().as_ref() {
                    flag.store(true, Ordering::Relaxed);
                }
                let replace = self
                    .replace_after_read
                    .lock()
                    .unwrap()
                    .as_ref()
                    .filter(|(trigger, _)| trigger == pane_id)
                    .map(|(_, target)| target.clone());
                if let Some(target) = replace {
                    self.edit_pane(&target, |entry| {
                        entry["terminal_id"] = json!("terminal-replacement");
                        entry["agent_session"] = json!({"value": "session-replacement"});
                    });
                }
                if self.oversized.lock().unwrap().as_deref() == Some(*pane_id) {
                    "x".repeat(128 * 1_024)
                } else {
                    self.excerpts
                        .lock()
                        .unwrap()
                        .get(*pane_id)
                        .cloned()
                        .unwrap_or_default()
                }
            }
            ["agent", "prompt", pane_id, envelope] => {
                if self.prompt_failures.lock().unwrap().iter().any(|p| p == pane_id) {
                    return Err("herdr could not tell whether the prompt arrived".to_owned());
                }
                self.prompts
                    .lock()
                    .unwrap()
                    .push(((*pane_id).to_owned(), (*envelope).to_owned()));
                "{}".to_owned()
            }
            ["pane", "report-metadata", ..] => "{}".to_owned(),
            _ => return Err(format!("unexpected Herdr command: {args:?}")),
        };
        Ok(HerdrCommandOutput { stdout })
    }
}

fn address(id: &str, alias: &str) -> PaneAddressDraft {
    PaneAddressDraft {
        id: PaneAddressId::new(id),
        role_id: PaneRoleId::new(format!("role-{id}")),
        role_name: alias.to_owned(),
        role_description: format!("{alias} test role"),
        alias: alias.to_owned(),
    }
}

/// Names four of the six panes. The shell and the panel's own pane stay
/// unnamed, so label fallback and panel exclusion are both exercised.
fn setup(directory: &TestDirectory) {
    let namespace = StateNamespace::for_space("socket-a", "workspace-1");
    let attribution = MutationAttribution::new(ActorKind::Human, "test", 1);
    StateStore::open(&directory.state)
        .unwrap()
        .transaction(|transaction| {
            transaction.activate_worktree(&namespace, &attribution)?;
            for (alias, pane_id, session) in [
                ("lead", "pane-lead", "session-lead"),
                ("reviewer", "pane-reviewer", "session-reviewer"),
                ("tests", "pane-tests", "session-tests"),
                ("docs", "pane-docs", "session-docs"),
            ] {
                let draft = address(&format!("address-{alias}"), alias);
                transaction.create_pane_address(&namespace, &draft, &attribution)?;
                transaction.bind_pane(
                    &namespace,
                    &PaneBindingId::new(format!("binding-{alias}")),
                    &draft.id,
                    pane_id,
                    &attribution,
                )?;
                transaction.observe_agent_session(
                    &namespace,
                    &format!("observation-{session}"),
                    &draft.id,
                    &AgentSessionId::new(session),
                    "idle",
                    &attribution,
                )?;
            }
            Ok(())
        })
        .unwrap();
}

fn runtime<'a>(
    directory: &'a TestDirectory,
    herdr: &'a FakeHerdr,
    clock: &'a TestClock,
    ids: &'a SequenceIds,
    pane_id: &'a str,
) -> LifecycleRuntime<'a, FakeHerdr, TestClock, SequenceIds> {
    LifecycleRuntime {
        state_directory: &directory.state,
        home_directory: Some(&directory.home),
        agent_config: namedrop::AgentConfigDirectories::default(),
        cwd: &directory.repo,
        workspace_id: Some("workspace-1"),
        current_pane_id: Some(pane_id),
        session_socket_id: "socket-a",
        herdr,
        clock,
        ids,
    }
}

fn register(
    address_id: &str,
    agent_runtime: AgentRuntime,
    runtime: &LifecycleRuntime<'_, FakeHerdr, TestClock, SequenceIds>,
) {
    let challenge =
        issue_registration_challenge(&PaneAddressId::new(address_id), agent_runtime, runtime)
            .unwrap();
    execute_integration_operation(
        "register",
        json!({
            "runtime": agent_runtime.as_str(),
            "protocol_version": "collab/v1",
            "nonce": challenge.nonce,
            "capabilities": complete_capability_profile()
        }),
        runtime,
    )
    .unwrap();
}

/// Every collection in this binary goes through here so the panel's own pane,
/// which `collect_agent_status` reads from the environment, is set under one
/// lock rather than concurrently with another test's reading.
fn collect(
    runtime: &LifecycleRuntime<'_, FakeHerdr, TestClock, SequenceIds>,
    expected_terminal: &str,
    expected_session: Option<&str>,
    panel_pane: Option<&str>,
    cancelled: &AtomicBool,
) -> Result<Value, String> {
    let _guard = ENVIRONMENT
        .lock()
        .unwrap_or_else(|error| error.into_inner());
    // SAFETY: the lock above is held by every reader of the environment in this
    // test binary, so no other thread can read it while it is written here.
    unsafe {
        match panel_pane {
            Some(pane_id) => std::env::set_var("HERDR_PANE_ID", pane_id),
            None => std::env::remove_var("HERDR_PANE_ID"),
        }
    }
    let collected = collect_agent_status(runtime, expected_terminal, expected_session, cancelled);
    unsafe { std::env::remove_var("HERDR_PANE_ID") };
    collected
}

fn pane_ids(snapshot: &Value) -> Vec<String> {
    snapshot["panes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pane| pane["pane_id"].as_str().unwrap().to_owned())
        .collect()
}

fn row<'a>(snapshot: &'a Value, pane_id: &str) -> &'a Value {
    snapshot["panes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pane| pane["pane_id"] == pane_id)
        .unwrap_or_else(|| panic!("{pane_id} is missing from the snapshot"))
}

fn warnings(snapshot: &Value) -> String {
    snapshot["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn origin_is_bound_and_peers_are_named_from_namedrop_state() {
    let directory = TestDirectory::new();
    setup(&directory);
    let clock = Arc::new(TestClock::fixed());
    let herdr = FakeHerdr::new(&directory, Arc::clone(&clock));
    let ids = SequenceIds(AtomicU64::new(1));
    let lead = runtime(&directory, &herdr, &clock, &ids, "pane-lead");
    register("address-lead", AgentRuntime::ClaudeCode, &lead);
    let database_path = directory.state.join("namedrop.sqlite3");
    let version: u32 = rusqlite::Connection::open(&database_path)
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 5, "exercise the installed runtime schema");
    let before_read = std::fs::read(&database_path).unwrap();

    let snapshot = collect(
        &lead,
        "terminal-lead",
        Some("session-lead"),
        Some("pane-panel"),
        &AtomicBool::new(false),
    )
    .unwrap();

    assert_eq!(snapshot["origin"]["workspace_id"], "workspace-1");
    assert_eq!(snapshot["origin"]["pane_id"], "pane-lead");
    assert_eq!(snapshot["origin"]["terminal_id"], "terminal-lead");
    assert_eq!(snapshot["origin"]["session_id"], "session-lead");
    assert_eq!(snapshot["origin"]["host"], "claude");
    assert_eq!(snapshot["origin"]["label"], "@lead");
    assert_eq!(snapshot["origin"]["status"], "working");
    assert_eq!(snapshot["origin"]["cwd"], "/tmp/repo");
    assert_eq!(snapshot["observed_at_ms"], 1_000);

    // The origin is described, and it is described first.
    assert_eq!(pane_ids(&snapshot).first().unwrap(), "pane-lead");
    // The panel's own pane is never its own subject.
    assert!(!pane_ids(&snapshot).iter().any(|id| id == "pane-panel"));
    assert_eq!(row(&snapshot, "pane-reviewer")["label"], "@reviewer");
    assert_eq!(row(&snapshot, "pane-reviewer")["host"], "codex");
    assert_eq!(
        row(&snapshot, "pane-reviewer")["session_id"],
        "session-reviewer"
    );
    // An unnamed shell keeps factual identity and claims no host.
    assert_eq!(row(&snapshot, "pane-shell")["label"], "build shell");
    assert_eq!(row(&snapshot, "pane-shell")["host"], Value::Null);
    assert_eq!(row(&snapshot, "pane-shell")["session_id"], Value::Null);
    assert!(
        row(&snapshot, "pane-docs")["excerpt"]
            .as_str()
            .unwrap()
            .contains("pane-docs recent output")
    );

    assert_eq!(std::fs::read(&database_path).unwrap(), before_read);
    assert_eq!(snapshot["handoffs_available"], true);
    assert_eq!(snapshot["waits"].as_array().unwrap().len(), 0);
    assert_eq!(snapshot["omitted_panes"], 0);
    assert_eq!(snapshot["warnings"].as_array().unwrap().len(), 0);
}

#[test]
fn a_replaced_identity_is_refused_rather_than_described() {
    let directory = TestDirectory::new();
    setup(&directory);
    let clock = Arc::new(TestClock::fixed());
    let herdr = FakeHerdr::new(&directory, Arc::clone(&clock));
    let ids = SequenceIds(AtomicU64::new(1));
    let lead = runtime(&directory, &herdr, &clock, &ids, "pane-lead");
    let open = AtomicBool::new(false);

    // The pane the panel was opened from now runs in a different terminal.
    let moved = collect(&lead, "terminal-old", Some("session-lead"), None, &open).unwrap_err();
    assert!(moved.contains("terminal"), "{moved}");

    // Same pane and terminal, replacement Agent Session.
    let replaced = collect(&lead, "terminal-lead", Some("session-old"), None, &open).unwrap_err();
    assert!(replaced.contains("agent session"), "{replaced}");

    // Opened from a pane with no session; an agent has since started in it.
    let appeared = collect(&lead, "terminal-lead", None, None, &open).unwrap_err();
    assert!(appeared.contains("agent session"), "{appeared}");

    // Exact identity still collects.
    assert!(collect(&lead, "terminal-lead", Some("session-lead"), None, &open).is_ok());

    herdr.remove_pane("pane-lead");
    let closed = collect(&lead, "terminal-lead", Some("session-lead"), None, &open).unwrap_err();
    assert!(closed.contains("closed"), "{closed}");
}

#[test]
fn excerpts_are_redacted_quoted_and_bounded_to_recent_output() {
    let directory = TestDirectory::new();
    setup(&directory);
    let clock = Arc::new(TestClock::fixed());
    let herdr = FakeHerdr::new(&directory, Arc::clone(&clock));
    herdr.set_excerpt(
        "pane-lead",
        &format!(
            "AWS_SECRET_ACCESS_KEY=do-not-share\n\u{1b}[31mred\u{1b}[0m\n{}\nlast line here\n",
            "filler line\n".repeat(600)
        ),
    );
    herdr
        .oversized
        .lock()
        .unwrap()
        .replace("pane-docs".to_owned());
    let ids = SequenceIds(AtomicU64::new(1));
    let lead = runtime(&directory, &herdr, &clock, &ids, "pane-lead");

    let snapshot = collect(
        &lead,
        "terminal-lead",
        Some("session-lead"),
        Some("pane-panel"),
        &AtomicBool::new(false),
    )
    .unwrap();

    let excerpt = row(&snapshot, "pane-lead")["excerpt"].as_str().unwrap();
    assert!(!excerpt.contains("do-not-share"), "{excerpt}");
    assert!(
        excerpt.len() <= 2_048,
        "excerpt kept {} bytes",
        excerpt.len()
    );
    // The tail is what describes the agent now, so the tail is what survives.
    assert!(excerpt.ends_with("last line here"), "{excerpt}");
    assert!(!excerpt.contains('\u{1b}'), "raw control sequence survived");
    // Newlines stay structure; everything else that can move a cursor is quoted.
    assert!(excerpt.contains('\n'));

    // A pane whose output blows the read limit costs its excerpt, not the snapshot.
    assert_eq!(row(&snapshot, "pane-docs")["excerpt"], "");
    assert_eq!(snapshot["omitted_panes"], 1);
    assert!(
        warnings(&snapshot).contains("output"),
        "{}",
        warnings(&snapshot)
    );
    assert_eq!(row(&snapshot, "pane-docs")["status"], "working");
}

#[test]
fn outstanding_requests_report_delivery_truth_and_never_pane_text() {
    let directory = TestDirectory::new();
    setup(&directory);
    let clock = Arc::new(TestClock::fixed());
    let herdr = FakeHerdr::new(&directory, Arc::clone(&clock));
    // A pane that merely says it is waiting is not evidence of a dependency.
    herdr.set_excerpt(
        "pane-docs",
        "I am waiting for @lead to reply before I continue\n",
    );
    let ids = SequenceIds(AtomicU64::new(1));
    let lead = runtime(&directory, &herdr, &clock, &ids, "pane-lead");
    let reviewer = runtime(&directory, &herdr, &clock, &ids, "pane-reviewer");
    let tests = runtime(&directory, &herdr, &clock, &ids, "pane-tests");
    register("address-lead", AgentRuntime::ClaudeCode, &lead);
    register("address-reviewer", AgentRuntime::Codex, &reviewer);
    register("address-tests", AgentRuntime::ClaudeCode, &tests);
    namedrop::execute_lifecycle_operation("reconcile", json!({}), &lead).unwrap();
    // @docs is named but never registered, so nothing can be submitted to it.
    herdr
        .prompt_failures
        .lock()
        .unwrap()
        .push("pane-tests".to_owned());
    execute_message_operation(
        "send",
        json!({
            "request_id": "request-1",
            "source": "@reviewer @tests @docs please check the timeout handling",
            "author_kind": "agent_session"
        }),
        &lead,
    )
    .unwrap();
    drain_mailboxes(&lead).unwrap();

    let snapshot = collect(
        &lead,
        "terminal-lead",
        Some("session-lead"),
        Some("pane-panel"),
        &AtomicBool::new(false),
    )
    .unwrap();

    assert_eq!(snapshot["handoffs_available"], true);
    let waits = snapshot["waits"].as_array().unwrap();
    assert_eq!(waits.len(), 3, "{waits:#?}");

    // Attention first: an ambiguous submission is held, not progress.
    assert_eq!(waits[0]["alias"], "tests");
    assert_eq!(waits[0]["state"], "held");
    assert!(waits[0]["reason"].as_str().unwrap().contains("held"));
    assert_eq!(waits[0]["pane_id"], "pane-tests");

    // Delivered, and honestly described as unanswered rather than as working.
    assert_eq!(waits[1]["alias"], "reviewer");
    assert_eq!(waits[1]["state"], "acknowledged");
    assert_eq!(waits[1]["reason"], "receipt recorded; no result yet");
    assert!(
        waits[1]["description"]
            .as_str()
            .unwrap()
            .starts_with("@reviewer · please check the timeout handling")
    );
    assert!(!waits[1]["request_id"].as_str().unwrap().is_empty());

    // Never delivered, with the structured reason it was not.
    assert_eq!(waits[2]["alias"], "docs");
    assert_eq!(waits[2]["state"], "queued");
    assert!(
        waits[2]["reason"]
            .as_str()
            .unwrap()
            .contains("no active Namedrop registration"),
        "{}",
        waits[2]["reason"]
    );

    // @docs said it was waiting on @lead; no structured state says so.
    assert!(!waits.iter().any(|wait| wait["alias"] == "lead"));
    assert!(!waits.iter().any(|wait| wait["pane_id"] == "pane-lead"));

    // Panes the origin is outstanding on come first, in wait order.
    assert_eq!(
        pane_ids(&snapshot)[..4],
        ["pane-lead", "pane-tests", "pane-reviewer", "pane-docs"]
    );

    // A recorded result is finished work, so it stops being a wait.
    let attempt = herdr.envelope_for("pane-reviewer");
    execute_integration_operation(
        "complete",
        json!({"attempt_id": attempt["control"]["attempt_id"]}),
        &reviewer,
    )
    .unwrap();
    let after = collect(
        &lead,
        "terminal-lead",
        Some("session-lead"),
        Some("pane-panel"),
        &AtomicBool::new(false),
    )
    .unwrap();
    let after = after["waits"].as_array().unwrap().clone();
    assert_eq!(after.len(), 2, "{after:#?}");
    assert!(!after.iter().any(|wait| wait["alias"] == "reviewer"));
}

#[test]
fn personas_come_from_saved_names_and_never_from_pane_text() {
    let directory = TestDirectory::new();
    setup(&directory);
    // Saved through the public operation, exactly as an accepted edit writes it.
    StateStore::open(&directory.state)
        .unwrap()
        .transaction(|transaction| {
            transaction.set_address_persona(
                &StateNamespace::for_space("socket-a", "workspace-1"),
                &PaneAddressId::new("address-lead"),
                Some("Leads the release."),
            )
        })
        .unwrap();
    // A description migrated from an older Namedrop: past today's bound and
    // carrying control bytes. It is written the way the upgrade wrote it,
    // bypassing the validation a new edit would face.
    let legacy = format!("Reviews\u{1b}[31m Rust {}", "changes ".repeat(200));
    let database = rusqlite::Connection::open(directory.state.join("namedrop.sqlite3")).unwrap();
    database
        .execute(
            "UPDATE pane_addresses SET persona = ?1 WHERE address_id = 'address-reviewer'",
            [&legacy],
        )
        .unwrap();
    // A name retyped from agent to terminal keeps its persona stored but
    // inert, because only explicit user direction changes one.
    database
        .execute(
            "UPDATE pane_addresses SET pane_type = 'terminal', persona = ?1
             WHERE address_id = 'address-docs'",
            ["Runs the release script."],
        )
        .unwrap();
    drop(database);

    let clock = Arc::new(TestClock::fixed());
    let herdr = FakeHerdr::new(&directory, Arc::clone(&clock));
    let ids = SequenceIds(AtomicU64::new(1));
    let lead = runtime(&directory, &herdr, &clock, &ids, "pane-lead");
    register("address-lead", AgentRuntime::ClaudeCode, &lead);

    let snapshot = collect(
        &lead,
        "terminal-lead",
        Some("session-lead"),
        Some("pane-panel"),
        &AtomicBool::new(false),
    )
    .unwrap();

    assert_eq!(snapshot["origin"]["persona"], "Leads the release.");
    assert_eq!(row(&snapshot, "pane-lead")["persona"], "Leads the release.");
    // A name with no persona is Generic: the snapshot says nothing rather than
    // inventing one from what the pane printed.
    assert_eq!(row(&snapshot, "pane-tests")["persona"], Value::Null);
    assert_eq!(row(&snapshot, "pane-shell")["persona"], Value::Null);
    assert!(
        !row(&snapshot, "pane-docs")["excerpt"]
            .as_str()
            .unwrap()
            .is_empty(),
        "the excerpt fixture stopped producing pane text"
    );
    // The panel describes what is in effect, so an inert persona is left out
    // of the snapshot and left alone in the store.
    assert_eq!(row(&snapshot, "pane-docs")["persona"], Value::Null);

    // The migrated description is displayed safely and bounded, and what is
    // saved is left untouched until the user edits it.
    let shown = row(&snapshot, "pane-reviewer")["persona"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(shown.starts_with("Reviews"), "{shown}");
    assert!(!shown.contains('\u{1b}'), "{shown}");
    assert!(shown.chars().all(|c| !c.is_control()), "{shown}");
    assert!(shown.chars().count() <= 1_000, "{}", shown.chars().count());
    let saved: String = rusqlite::Connection::open(directory.state.join("namedrop.sqlite3"))
        .unwrap()
        .query_row(
            "SELECT persona FROM pane_addresses WHERE address_id = 'address-reviewer'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(saved, legacy, "reading the panel rewrote a saved persona");
    let inert: String = rusqlite::Connection::open(directory.state.join("namedrop.sqlite3"))
        .unwrap()
        .query_row(
            "SELECT persona FROM pane_addresses WHERE address_id = 'address-docs'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(inert, "Runs the release script.");
}

#[test]
fn unreadable_namedrop_state_is_unavailable_evidence_not_zero_dependencies() {
    let directory = TestDirectory::new();
    let clock = Arc::new(TestClock::fixed());
    let herdr = FakeHerdr::new(&directory, Arc::clone(&clock));
    let ids = SequenceIds(AtomicU64::new(1));
    let lead = runtime(&directory, &herdr, &clock, &ids, "pane-lead");
    let open = AtomicBool::new(false);

    // No store at all: facts still collect, dependencies are declared unknown.
    let missing = collect(&lead, "terminal-lead", Some("session-lead"), None, &open).unwrap();
    assert_eq!(missing["handoffs_available"], false);
    assert_eq!(missing["waits"].as_array().unwrap().len(), 0);
    assert!(
        warnings(&missing).contains("no saved state"),
        "{}",
        warnings(&missing)
    );
    assert_eq!(missing["origin"]["pane_id"], "pane-lead");
    assert_eq!(row(&missing, "pane-reviewer")["label"], "Reviewer");

    // A named space whose origin never registered cannot list its requests.
    setup(&directory);
    let unregistered = collect(&lead, "terminal-lead", Some("session-lead"), None, &open).unwrap();
    assert_eq!(unregistered["handoffs_available"], false);
    assert!(
        warnings(&unregistered).contains("Integration Registration"),
        "{}",
        warnings(&unregistered)
    );
    // Names were readable, so they are still used.
    assert_eq!(unregistered["origin"]["label"], "@lead");

    // State written by a newer Namedrop is reported, never migrated or guessed.
    let database = rusqlite::Connection::open(directory.state.join("namedrop.sqlite3")).unwrap();
    database.pragma_update(None, "user_version", 6).unwrap();
    drop(database);
    let newer = collect(&lead, "terminal-lead", Some("session-lead"), None, &open).unwrap();
    assert_eq!(newer["handoffs_available"], false);
    assert!(
        warnings(&newer).contains("schema 6"),
        "{}",
        warnings(&newer)
    );
    assert_eq!(newer["waits"].as_array().unwrap().len(), 0);
    assert_eq!(newer["origin"]["terminal_id"], "terminal-lead");
    let version: u32 = rusqlite::Connection::open(directory.state.join("namedrop.sqlite3"))
        .unwrap()
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .unwrap();
    assert_eq!(version, 6, "active state must not be migrated by a read");
}

#[test]
fn collection_stops_at_its_budget_and_says_what_is_missing() {
    let directory = TestDirectory::new();
    setup(&directory);
    let clock = Arc::new(TestClock::fixed());
    let herdr = FakeHerdr::new(&directory, Arc::clone(&clock));
    // Three excerpts fit in the 2000 ms budget once the closing identity check
    // keeps its reserve; the rest of the panes go undescribed.
    herdr.charge(0, 600);
    let ids = SequenceIds(AtomicU64::new(1));
    let lead = runtime(&directory, &herdr, &clock, &ids, "pane-lead");

    let snapshot = collect(
        &lead,
        "terminal-lead",
        Some("session-lead"),
        Some("pane-panel"),
        &AtomicBool::new(false),
    )
    .unwrap();

    // Partial data is still useful: identities survived the deadline.
    assert_eq!(snapshot["origin"]["pane_id"], "pane-lead");
    assert_eq!(pane_ids(&snapshot).len(), 5);
    let described = snapshot["panes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|pane| pane["excerpt"] != "")
        .count();
    assert_eq!(described, 3);
    assert_eq!(snapshot["omitted_panes"], 5 - described);
    assert!(
        warnings(&snapshot).contains("budget"),
        "{}",
        warnings(&snapshot)
    );
}

#[test]
fn cancellation_returns_no_snapshot_at_all() {
    let directory = TestDirectory::new();
    setup(&directory);
    let clock = Arc::new(TestClock::fixed());
    let herdr = FakeHerdr::new(&directory, Arc::clone(&clock));
    let ids = SequenceIds(AtomicU64::new(1));
    let lead = runtime(&directory, &herdr, &clock, &ids, "pane-lead");

    let before = AtomicBool::new(true);
    let refused = collect(&lead, "terminal-lead", Some("session-lead"), None, &before).unwrap_err();
    assert!(refused.contains("cancelled"), "{refused}");

    // Cancelled midway: a half-collected space is discarded, not returned.
    let during = Arc::new(AtomicBool::new(false));
    herdr
        .cancel_on_read
        .lock()
        .unwrap()
        .replace(Arc::clone(&during));
    let stopped = collect(&lead, "terminal-lead", Some("session-lead"), None, &during).unwrap_err();
    assert!(stopped.contains("cancelled"), "{stopped}");
}

#[test]
fn only_the_invoking_space_is_read() {
    let directory = TestDirectory::new();
    setup(&directory);
    let clock = Arc::new(TestClock::fixed());
    let herdr = FakeHerdr::new(&directory, Arc::clone(&clock));
    herdr.edit_pane("pane-docs", |entry| {
        entry["workspace_id"] = json!("workspace-9");
    });
    let ids = SequenceIds(AtomicU64::new(1));
    let lead = runtime(&directory, &herdr, &clock, &ids, "pane-lead");

    let snapshot = collect(
        &lead,
        "terminal-lead",
        Some("session-lead"),
        Some("pane-panel"),
        &AtomicBool::new(false),
    )
    .unwrap();

    assert!(!pane_ids(&snapshot).iter().any(|id| id == "pane-docs"));
    assert!(
        warnings(&snapshot).contains("outside this space"),
        "{}",
        warnings(&snapshot)
    );
    let commands = herdr.commands.lock().unwrap().join("\n");
    assert!(!commands.contains("workspace-9"), "{commands}");
    assert!(!commands.contains("pane-docs"), "{commands}");
    // Reading is all it did: nothing here mutates Herdr or Namedrop state.
    assert!(
        commands
            .lines()
            .all(|command| command.starts_with("worktree list")
                || command.starts_with("pane list")
                || command.starts_with("pane read")),
        "{commands}"
    );
}

#[test]
fn a_crowded_space_still_produces_a_bounded_snapshot() {
    let directory = TestDirectory::new();
    setup(&directory);
    let clock = Arc::new(TestClock::fixed());
    let herdr = FakeHerdr::new(&directory, Arc::clone(&clock));
    for index in 0..40 {
        let pane_id = format!("pane-extra-{index}");
        herdr.panes.lock().unwrap().push(pane(
            &pane_id,
            &format!("terminal-extra-{index}"),
            Some("codex"),
            Some(&format!("session-extra-{index}")),
            "idle",
            "Extra",
        ));
        herdr.set_excerpt(&pane_id, "extra pane output\n");
    }
    let ids = SequenceIds(AtomicU64::new(1));
    let lead = runtime(&directory, &herdr, &clock, &ids, "pane-lead");

    let snapshot = collect(
        &lead,
        "terminal-lead",
        Some("session-lead"),
        Some("pane-panel"),
        &AtomicBool::new(false),
    )
    .unwrap();

    // 45 panes are open; the snapshot stays bounded in rows and in excerpts.
    let rows = snapshot["panes"].as_array().unwrap();
    assert_eq!(rows.len(), 32);
    assert_eq!(rows[0]["pane_id"], "pane-lead");
    assert_eq!(rows.iter().filter(|pane| pane["excerpt"] != "").count(), 8);
    // Everything it cannot describe is counted, listed or not.
    assert_eq!(snapshot["omitted_panes"], 45 - 8);
    assert!(
        warnings(&snapshot).contains("left out 13 panes beyond the 32"),
        "{}",
        warnings(&snapshot)
    );
    let excerpt_bytes: usize = rows
        .iter()
        .map(|pane| pane["excerpt"].as_str().unwrap().len())
        .sum();
    assert!(
        excerpt_bytes <= 8 * 2_048,
        "{excerpt_bytes} bytes of excerpts"
    );
}

#[test]
fn a_snapshot_whose_identities_cannot_be_rechecked_is_not_shown() {
    let directory = TestDirectory::new();
    setup(&directory);
    let clock = Arc::new(TestClock::fixed());
    let herdr = FakeHerdr::new(&directory, Arc::clone(&clock));
    // Every call is expensive enough that nothing is left for the closing
    // check. An unverifiable snapshot is refused, not shown unverified.
    herdr.charge(500, 0);
    let ids = SequenceIds(AtomicU64::new(1));
    let lead = runtime(&directory, &herdr, &clock, &ids, "pane-lead");

    let refused = collect(
        &lead,
        "terminal-lead",
        Some("session-lead"),
        Some("pane-panel"),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    assert!(refused.contains("re-checked"), "{refused}");
}

#[test]
fn a_peer_replaced_during_collection_is_dropped_rather_than_described() {
    let directory = TestDirectory::new();
    setup(&directory);
    let clock = Arc::new(TestClock::fixed());
    let herdr = FakeHerdr::new(&directory, Arc::clone(&clock));
    // A pane id is a reusable handle: the reviewer's pane is read, then the id
    // is taken over by a different terminal and session before the panel paints.
    herdr.replace_after_read("pane-reviewer", "pane-reviewer");
    let ids = SequenceIds(AtomicU64::new(1));
    let lead = runtime(&directory, &herdr, &clock, &ids, "pane-lead");

    let snapshot = collect(
        &lead,
        "terminal-lead",
        Some("session-lead"),
        Some("pane-panel"),
        &AtomicBool::new(false),
    )
    .unwrap();

    // The output was read under an identity that no longer holds, so neither
    // the excerpt nor the row survives.
    assert!(!pane_ids(&snapshot).iter().any(|id| id == "pane-reviewer"));
    assert!(
        warnings(&snapshot).contains("identity changed while"),
        "{}",
        warnings(&snapshot)
    );
    // Its absence is counted, and the panes that did hold still are described.
    assert_eq!(snapshot["omitted_panes"], 1);
    assert_eq!(pane_ids(&snapshot).len(), 4);
    assert!(
        row(&snapshot, "pane-tests")["excerpt"]
            .as_str()
            .unwrap()
            .contains("pane-tests recent output")
    );
    assert_eq!(snapshot["origin"]["pane_id"], "pane-lead");
}

#[test]
fn the_origin_replaced_during_collection_is_refused() {
    let directory = TestDirectory::new();
    setup(&directory);
    let clock = Arc::new(TestClock::fixed());
    let herdr = FakeHerdr::new(&directory, Arc::clone(&clock));
    // The invoking pane restarts its agent while the snapshot is being taken.
    herdr.replace_after_read("pane-lead", "pane-lead");
    let ids = SequenceIds(AtomicU64::new(1));
    let lead = runtime(&directory, &herdr, &clock, &ids, "pane-lead");

    let refused = collect(
        &lead,
        "terminal-lead",
        Some("session-lead"),
        Some("pane-panel"),
        &AtomicBool::new(false),
    )
    .unwrap_err();
    // Nothing collected under the old session is attributed to the new one.
    assert!(refused.contains("terminal"), "{refused}");
}

#[test]
fn a_long_page_of_requests_leaves_budget_for_the_rest_of_the_panel() {
    let directory = TestDirectory::new();
    setup(&directory);
    let clock = Arc::new(TestClock::fixed());
    let herdr = FakeHerdr::new(&directory, Arc::clone(&clock));
    let ids = SequenceIds(AtomicU64::new(1));
    let lead = runtime(&directory, &herdr, &clock, &ids, "pane-lead");
    register("address-lead", AgentRuntime::ClaudeCode, &lead);
    for (index, alias) in ["reviewer", "tests", "docs"].iter().enumerate() {
        execute_message_operation(
            "send",
            json!({
                "request_id": format!("request-{index}"),
                "source": format!("@{alias} please look at this"),
                "author_kind": "agent_session"
            }),
            &lead,
        )
        .unwrap();
    }
    // Reading the reports is expensive; the state read must give up its turn
    // rather than spend the whole collection budget on a long page.
    herdr.charge_state_read(300);

    let snapshot = collect(
        &lead,
        "terminal-lead",
        Some("session-lead"),
        Some("pane-panel"),
        &AtomicBool::new(false),
    )
    .unwrap();

    // What it did read is real, and it says the list is not the whole list.
    assert_eq!(snapshot["handoffs_available"], true);
    let waits = snapshot["waits"].as_array().unwrap().len();
    assert!(waits < 3, "read {waits} of 3 requests without giving up");
    assert!(
        warnings(&snapshot).contains("incomplete"),
        "{}",
        warnings(&snapshot)
    );
    // And the panel still got a snapshot: identities re-checked, output read.
    assert_eq!(snapshot["origin"]["pane_id"], "pane-lead");
    assert!(
        snapshot["panes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|pane| pane["excerpt"] != ""),
        "the state read consumed the whole budget"
    );
}

#[test]
fn a_stalled_pane_read_cannot_spend_the_identity_check_reserve() {
    let directory = TestDirectory::new();
    setup(&directory);
    let clock = Arc::new(TestClock::fixed());
    let herdr = FakeHerdr::new(&directory, Arc::clone(&clock));
    // The origin's own output never comes back. A read that is merely allowed
    // to start would spend the whole remaining budget and leave the closing
    // identity check nothing, so the timeout it is handed has to be short.
    herdr.stall("pane-lead");
    let ids = SequenceIds(AtomicU64::new(1));
    let lead = runtime(&directory, &herdr, &clock, &ids, "pane-lead");

    let snapshot = collect(
        &lead,
        "terminal-lead",
        Some("session-lead"),
        Some("pane-panel"),
        &AtomicBool::new(false),
    )
    .unwrap();

    // Discovery gets everything that is left; an excerpt read gets everything
    // except the reserve.
    assert_eq!(herdr.timeout_for("worktree list"), 2_000);
    assert_eq!(herdr.timeout_for("pane read pane-lead"), 2_000 - 250);
    // The reserve survived the stall, so the closing check still ran and the
    // panel still gets a factual snapshot.
    assert_eq!(snapshot["origin"]["pane_id"], "pane-lead");
    assert_eq!(snapshot["origin"]["label"], "@lead");
    assert_eq!(snapshot["origin"]["status"], "working");
    assert_eq!(pane_ids(&snapshot).len(), 5);
    assert_eq!(row(&snapshot, "pane-lead")["excerpt"], "");
    assert!(
        warnings(&snapshot).contains("timed out"),
        "{}",
        warnings(&snapshot)
    );
    assert_eq!(snapshot["omitted_panes"], 5);
}
