#![allow(dead_code)]

#[path = "pane_effects_diagnostics.rs"]
mod pane_effects_diagnostics;

use namedrop::{
    ActorKind, AgentRuntime, AgentSessionId, Clock, HerdrCommandExecutor, HerdrCommandOutput,
    IdGenerator, LifecycleRuntime, MutationAttribution, PaneAddressDraft, PaneAddressId,
    PaneBindingId, PaneRoleId, PromptFailureKind, StateNamespace, StateStore,
    complete_capability_profile, drain_mailboxes, execute_control_operation,
    execute_integration_operation, execute_lifecycle_operation, execute_message_operation,
    execute_user_prompt_submit_hook, issue_registration_challenge,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::{
        Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(1);

pub type Space = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);
pub const SPACES: [Space; 3] = [
    (
        "w1",
        "ws1",
        &[("lead", "pane-1-lead"), ("planner", "pane-1-planner")],
    ),
    (
        "w2",
        "ws2",
        &[
            ("lead", "pane-2-lead"),
            ("tester", "pane-2-tester"),
            ("scout", "pane-2-scout"),
        ],
    ),
    (
        "w3",
        "ws3",
        &[("lead", "pane-3-lead"), ("scout", "pane-3-scout")],
    ),
];

pub struct FakeHerdr {
    pub repo: String,
    pub labels: Mutex<Vec<(String, String)>>,
    /// The repository Herdr stamps on a workspace's checkout, by workspace id.
    pub repos: Mutex<Vec<(String, String)>>,
    pub workspace_list_fails: AtomicBool,
    pub prompts: Mutex<Vec<(String, String)>>,
    pub notifications: Mutex<Vec<Vec<String>>>,
    pub commands: Mutex<Vec<String>>,
    pub statuses: Mutex<BTreeMap<String, String>>,
    pub sessions: Mutex<BTreeMap<String, String>>,
    pub unreadable: Mutex<BTreeSet<String>>,
    pub failing_prompts: Mutex<BTreeSet<String>>,
    /// Shell panes as (workspace, pane, physical terminal).
    pub terminals: Mutex<Vec<(String, String, String)>>,
    pub runs: Mutex<Vec<(String, String)>>,
    pub shells_busy: AtomicBool,
}

impl FakeHerdr {
    pub fn new(repo: &std::path::Path) -> Self {
        Self {
            repo: repo.display().to_string(),
            labels: Mutex::new(
                SPACES
                    .iter()
                    .map(|(id, label, _)| ((*id).to_owned(), (*label).to_owned()))
                    .collect(),
            ),
            repos: Mutex::new(Vec::new()),
            workspace_list_fails: AtomicBool::new(false),
            prompts: Mutex::new(Vec::new()),
            notifications: Mutex::new(Vec::new()),
            commands: Mutex::new(Vec::new()),
            statuses: Mutex::new(BTreeMap::new()),
            sessions: Mutex::new(BTreeMap::new()),
            unreadable: Mutex::new(BTreeSet::new()),
            failing_prompts: Mutex::new(BTreeSet::new()),
            terminals: Mutex::new(Vec::new()),
            runs: Mutex::new(Vec::new()),
            shells_busy: AtomicBool::new(false),
        }
    }

    pub fn runs(&self, pane: &str) -> Vec<String> {
        self.runs
            .lock()
            .unwrap()
            .iter()
            .filter(|(target, _)| target == pane)
            .map(|(_, command)| command.clone())
            .collect()
    }

    pub fn is_terminal(&self, pane: &str) -> bool {
        self.terminals
            .lock()
            .unwrap()
            .iter()
            .any(|(_, shell, _)| shell == pane)
    }

    pub fn envelopes(&self, pane: &str) -> Vec<Value> {
        self.prompts
            .lock()
            .unwrap()
            .iter()
            .filter(|(target, _)| target == pane)
            .filter_map(|(_, prompt)| serde_json::from_str::<Value>(prompt).ok())
            .filter(|prompt| prompt.get("control").is_some())
            .collect()
    }

    pub fn last_envelope(&self, pane: &str) -> Value {
        self.envelopes(pane)
            .pop()
            .unwrap_or_else(|| panic!("no envelope was dispatched to {pane}"))
    }
}

impl HerdrCommandExecutor for FakeHerdr {
    fn execute(&self, args: &[&str]) -> Result<HerdrCommandOutput, String> {
        self.commands.lock().unwrap().push(args.join(" "));
        let stdout = match args {
            ["workspace", "list"] if self.workspace_list_fails.load(Ordering::Relaxed) => {
                return Err("herdr: workspace list is unavailable".to_owned());
            }
            ["workspace", "list"] => {
                let labels = self.labels.lock().unwrap();
                let repos = self.repos.lock().unwrap();
                json!({"result":{"type":"workspace_list","workspaces": labels.iter().enumerate()
                    .map(|(number, (id, label))| {
                        let mut workspace = json!({"workspace_id":id,"label":label,"number":number + 1});
                        if let Some((_, repo)) = repos.iter().find(|(space, _)| space == id) {
                            workspace["worktree"] = json!({
                                "checkout_path": format!("/worktrees/{repo}/{label}"),
                                "is_linked_worktree": true,
                                "repo_name": repo,
                            });
                        }
                        workspace
                    })
                    .collect::<Vec<_>>()}})
                .to_string()
            }
            ["worktree", "list", "--workspace", "w1", "--json"] => {
                json!({"result":{"worktrees":[{"path":self.repo,"open_workspace_id":"w1"}]}})
                    .to_string()
            }
            ["worktree", "list", "--workspace", _, "--json"] => {
                return Err(json!({"error":{"code":"not_git_worktree"}}).to_string());
            }
            ["pane", "list", "--workspace", workspace]
                if self.unreadable.lock().unwrap().contains(*workspace) =>
            {
                return Err(format!("herdr: workspace {workspace} is unavailable"));
            }
            ["pane", "list", "--workspace", workspace] => {
                let statuses = self.statuses.lock().unwrap().clone();
                let sessions = self.sessions.lock().unwrap().clone();
                let (statuses, sessions) = (&statuses, &sessions);
                let mut panes: Vec<Value> = SPACES
                    .iter()
                    .filter(|(id, _, _)| id == workspace)
                    .flat_map(|(id, _, agents)| {
                        agents.iter().enumerate().map(move |(index, (_, pane))| {
                            json!({"pane_id":pane,"terminal_id":format!("t-{pane}"),
                                   "tab_id":format!("tab-{index}"),"workspace_id":id,"agent":"codex",
                                   "agent_session":{"value":sessions.get(*pane).cloned().unwrap_or_else(|| session(pane))},
                                   "agent_status":statuses.get(*pane).map_or("idle", String::as_str)})
                        })
                    })
                    .collect();
                panes.extend(
                    self.terminals
                        .lock()
                        .unwrap()
                        .iter()
                        .filter(|(id, _, _)| id == workspace)
                        .map(|(id, pane, terminal)| {
                            json!({"pane_id":pane,"terminal_id":terminal,"tab_id":"tab-shell",
                                   "workspace_id":id,"agent":null,"agent_status":"unknown"})
                        }),
                );
                json!({"result":{"panes":panes}}).to_string()
            }
            ["pane", "process-info", "--pane", pane] if self.is_terminal(pane) => {
                let group = if self.shells_busy.load(Ordering::Relaxed) {
                    43
                } else {
                    42
                };
                json!({"result":{"process_info":{"shell_pid":42,"foreground_process_group_id":group,
                       "foreground_processes":[{"pid":std::process::id()}]}}})
                .to_string()
            }
            ["pane", "run", pane, command] => {
                self.runs
                    .lock()
                    .unwrap()
                    .push(((*pane).to_owned(), (*command).to_owned()));
                "{}".to_owned()
            }
            ["agent", "prompt", pane, _]
                if self.failing_prompts.lock().unwrap().contains(*pane) =>
            {
                return Err("definitive".to_owned());
            }
            ["agent", "prompt", pane, prompt] => {
                self.prompts
                    .lock()
                    .unwrap()
                    .push(((*pane).to_owned(), (*prompt).to_owned()));
                "{}".to_owned()
            }
            ["notification", "show", ..] => {
                self.notifications
                    .lock()
                    .unwrap()
                    .push(args.iter().map(|arg| (*arg).to_owned()).collect());
                "{}".to_owned()
            }
            ["pane", "report-metadata", ..] => "{}".to_owned(),
            _ => return Err(format!("unexpected Herdr command: {args:?}")),
        };
        Ok(HerdrCommandOutput { stdout })
    }

    fn classify_prompt_failure(&self, error: &str) -> PromptFailureKind {
        if error == "definitive" {
            PromptFailureKind::DefinitiveNonRetryable
        } else {
            PromptFailureKind::AmbiguousInvocation
        }
    }
}

pub fn git(repo: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["-c", "commit.gpgsign=false"])
        .args(args)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}");
    String::from_utf8(output.stdout).unwrap()
}

pub fn session(pane: &str) -> String {
    format!("session-{pane}")
}

pub fn address_id(workspace: &str, alias: &str) -> String {
    format!("address-{workspace}-{alias}")
}

pub struct TestClock(pub AtomicU64);
impl Clock for TestClock {
    fn now_unix_ms(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
    fn wait_ms(&self, _: u64) {}
}

pub struct SequenceIds(pub AtomicU64);
impl IdGenerator for SequenceIds {
    fn next_id(&self, kind: &str) -> String {
        format!("{kind}-{}", self.0.fetch_add(1, Ordering::Relaxed))
    }
}

pub struct Fixture {
    pub root: PathBuf,
    pub state: PathBuf,
    pub herdr: FakeHerdr,
    pub ids: SequenceIds,
    pub clock: TestClock,
}

pub type Runtime<'a> = LifecycleRuntime<'a, FakeHerdr, TestClock, SequenceIds>;

impl Fixture {
    pub fn new() -> Self {
        Self::with_activated(SPACES.len())
    }

    pub fn with_activated(activated: usize) -> Self {
        let root = std::env::temp_dir().join(format!(
            "namedrop-cross-workspace-{}-{}",
            std::process::id(),
            NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed)
        ));
        for path in ["state", "home", "repo"] {
            fs::create_dir_all(root.join(path)).unwrap();
        }
        let repo = root.join("repo");
        for args in [
            &["init", "-q", "-b", "main"][..],
            &["config", "user.name", "Cross Workspace Test"],
            &["config", "user.email", "cross@test"],
            &["commit", "-q", "--allow-empty", "-m", "initial"],
        ] {
            git(&repo, args);
        }
        let fixture = Self {
            state: root.join("state"),
            herdr: FakeHerdr::new(&repo),
            root,
            ids: SequenceIds(AtomicU64::new(1)),
            clock: TestClock(AtomicU64::new(42)),
        };
        let attribution = MutationAttribution::new(ActorKind::Human, "test", 1);
        for (workspace, _, agents) in SPACES.into_iter().take(activated) {
            let namespace = StateNamespace::for_space("socket-a", workspace);
            let mut store = StateStore::open(&fixture.state).unwrap();
            store
                .transaction(|transaction| {
                    transaction.activate_worktree(&namespace, &attribution)?;
                    for (alias, pane) in agents {
                        let draft = PaneAddressDraft {
                            id: PaneAddressId::new(address_id(workspace, alias)),
                            role_id: PaneRoleId::new(format!("role-{workspace}-{alias}")),
                            role_name: (*alias).to_owned(),
                            role_description: format!("{alias} test role"),
                            alias: (*alias).to_owned(),
                        };
                        transaction.create_pane_address(&namespace, &draft, &attribution)?;
                        transaction.bind_pane(
                            &namespace,
                            &PaneBindingId::new(format!("binding-{pane}")),
                            &draft.id,
                            pane,
                            &attribution,
                        )?;
                        transaction.observe_agent_session(
                            &namespace,
                            &format!("observation-{pane}"),
                            &draft.id,
                            &AgentSessionId::new(session(pane)),
                            "idle",
                            &attribution,
                        )?;
                    }
                    Ok(())
                })
                .unwrap();
            store
                .transaction(|transaction| transaction.complete_reconciliation(&namespace))
                .unwrap();
        }
        fixture
    }

    pub fn runtime<'a>(&'a self, workspace: &'a str, pane: &'a str) -> Runtime<'a> {
        LifecycleRuntime {
            state_directory: &self.state,
            home_directory: Some(self.root.join("home").leak()),
            agent_config: namedrop::AgentConfigDirectories::default(),
            cwd: self.root.join("repo").leak(),
            workspace_id: Some(workspace),
            current_pane_id: Some(pane),
            session_socket_id: "socket-a",
            herdr: &self.herdr,
            clock: &self.clock,
            ids: &self.ids,
        }
    }

    pub fn register_all(&self) {
        self.register_all_activated(SPACES.len());
    }

    pub fn register(&self, workspace: &str, alias: &str, pane: &str) {
        {
            {
                let runtime = self.runtime(workspace, pane);
                let challenge = issue_registration_challenge(
                    &PaneAddressId::new(address_id(workspace, alias)),
                    AgentRuntime::Codex,
                    &runtime,
                )
                .unwrap();
                execute_integration_operation(
                    "register",
                    json!({"runtime":"codex","protocol_version":"collab/v1",
                           "nonce":challenge.nonce,"capabilities":complete_capability_profile()}),
                    &runtime,
                )
                .unwrap();
            }
        }
    }

    pub fn register_all_activated(&self, activated: usize) {
        for (workspace, _, agents) in SPACES.into_iter().take(activated) {
            for (alias, pane) in agents {
                self.register(workspace, alias, pane);
            }
        }
    }

    pub fn rename(&self, workspace: &str, alias: &str, replacement: &str) {
        let namespace = StateNamespace::for_space("socket-a", workspace);
        StateStore::open(&self.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.replace_alias(
                    &namespace,
                    &PaneAddressId::new(address_id(workspace, alias)),
                    replacement,
                    &MutationAttribution::new(ActorKind::Human, "test", 2),
                )
            })
            .unwrap();
    }

    pub fn release(&self, workspace: &str, alias: &str) {
        let namespace = StateNamespace::for_space("socket-a", workspace);
        StateStore::open(&self.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.release_alias(
                    &namespace,
                    alias,
                    &MutationAttribution::new(ActorKind::Human, "test", 3),
                )
            })
            .unwrap();
    }

    pub fn remove(&self, workspace: &str, alias: &str) {
        let namespace = StateNamespace::for_space("socket-a", workspace);
        StateStore::open(&self.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.remove_pane_address(
                    &namespace,
                    &PaneAddressId::new(address_id(workspace, alias)),
                    &MutationAttribution::new(ActorKind::Human, "test", 2),
                )
            })
            .unwrap();
    }

    pub fn add_terminal(&self, workspace: &str, alias: &str, pane: &str) {
        self.add_shell(workspace, pane);
        let namespace = StateNamespace::for_space("socket-a", workspace);
        let attribution = MutationAttribution::new(ActorKind::Human, "test", 1);
        let id = PaneAddressId::new(address_id(workspace, alias));
        StateStore::open(&self.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.create_pane_address(
                    &namespace,
                    &PaneAddressDraft {
                        id: id.clone(),
                        role_id: PaneRoleId::new(format!("role-{workspace}-{alias}")),
                        role_name: alias.to_owned(),
                        role_description: format!("{alias} terminal"),
                        alias: alias.to_owned(),
                    },
                    &attribution,
                )?;
                transaction.bind_pane(
                    &namespace,
                    &PaneBindingId::new(format!("binding-{pane}")),
                    &id,
                    pane,
                    &attribution,
                )
            })
            .unwrap();
        self.sql(&format!(
            "UPDATE pane_addresses SET pane_type = 'terminal' WHERE address_id = '{}'",
            id.as_str()
        ));
    }

    pub fn add_shell(&self, workspace: &str, pane: &str) {
        self.herdr.terminals.lock().unwrap().push((
            workspace.to_owned(),
            pane.to_owned(),
            format!("t-{pane}"),
        ));
    }

    pub fn rebind(&self, workspace: &str, alias: &str, pane: &str) {
        let namespace = StateNamespace::for_space("socket-a", workspace);
        StateStore::open(&self.state)
            .unwrap()
            .transaction(|transaction| {
                transaction.rebind_pane(
                    &namespace,
                    &PaneBindingId::new(format!("binding-{pane}-{}", self.ids.next_id("rebind"))),
                    &PaneAddressId::new(address_id(workspace, alias)),
                    pane,
                    &MutationAttribution::new(ActorKind::Human, "test", 3),
                )
            })
            .unwrap();
    }

    pub fn remote_control(
        &self,
        operation: &str,
        workspace: &str,
        from_pane: &str,
        alias: &str,
    ) -> Result<Value, String> {
        execute_control_operation(
            operation,
            json!({"address_id":address_id(workspace, alias)}),
            &self.runtime(workspace, from_pane),
        )
    }

    pub fn address(&self, workspace: &str, alias: &str) -> Value {
        let listed = execute_lifecycle_operation(
            "address.list",
            json!({}),
            &self.runtime(workspace, "pane-none"),
        )
        .unwrap();
        listed
            .as_array()
            .unwrap()
            .iter()
            .find(|address| address["alias"] == alias)
            .unwrap_or_else(|| panic!("@{alias} is not listed in {workspace}: {listed}"))
            .clone()
    }

    pub fn texts(&self, query: &str) -> Vec<String> {
        self.db()
            .prepare(query)
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    pub fn delivery_states(&self, workspace: &str, message: &Value) -> Vec<String> {
        self.texts(&format!(
            "SELECT state FROM mailbox_deliveries WHERE worktree_id = '{workspace}' \
             AND message_id = '{}' ORDER BY delivery_id",
            message.as_str().unwrap()
        ))
    }

    pub fn run_terminal(&self, workspace: &str, pane: &str) -> Result<i32, String> {
        let run = self
            .herdr
            .runs(pane)
            .pop()
            .expect("a command was dispatched");
        let attempt = run.split('\'').rev().nth(1).unwrap().to_owned();
        namedrop::execute_terminal(&attempt, &self.runtime(workspace, pane))
    }

    pub fn drain(&self, workspace: &str) {
        drain_mailboxes(&self.runtime(workspace, "pane-none")).unwrap();
    }

    pub fn agent_send(&self, workspace: &str, pane: &str, request: &str, source: &str) -> Value {
        execute_message_operation(
            "send",
            json!({"request_id":request,"source":source,"author_kind":"agent_session"}),
            &self.runtime(workspace, pane),
        )
        .unwrap()
    }

    pub fn reply_to(&self, workspace: &str, pane: &str, envelope: &Value, source: &str) -> Value {
        execute_message_operation(
            "reply",
            json!({"request_id":format!("reply-{}", envelope["control"]["attempt_id"]),
                   "source":source,"author_kind":"agent_session",
                   "in_reply_to":envelope["control"]["message_id"],
                   "attempt_id":envelope["control"]["attempt_id"]}),
            &self.runtime(workspace, pane),
        )
        .unwrap()
    }

    pub fn try_send(&self, workspace: &str, pane: &str, input: Value) -> Result<Value, String> {
        execute_message_operation("send", input, &self.runtime(workspace, pane))
    }

    pub fn hook(&self, workspace: &str, pane: &str, prompt: &str) -> Option<Value> {
        execute_user_prompt_submit_hook(
            &json!({"hook_event_name":"UserPromptSubmit","prompt":prompt}).to_string(),
            &self.runtime(workspace, pane),
        )
    }

    pub fn advance(&self, ms: u64) {
        self.clock.0.fetch_add(ms, Ordering::Relaxed);
    }

    pub fn status(&self, pane: &str, status: &str) {
        self.herdr
            .statuses
            .lock()
            .unwrap()
            .insert(pane.to_owned(), status.to_owned());
    }

    pub fn db(&self) -> rusqlite::Connection {
        rusqlite::Connection::open(self.state.join("namedrop.sqlite3")).unwrap()
    }

    pub fn count(&self, query: &str) -> i64 {
        self.db().query_row(query, [], |row| row.get(0)).unwrap()
    }

    pub fn text(&self, query: &str) -> Option<String> {
        self.db().query_row(query, [], |row| row.get(0)).ok()
    }

    pub fn sql(&self, statement: &str) {
        rusqlite::Connection::open(self.state.join("namedrop.sqlite3"))
            .unwrap()
            .execute_batch(statement)
            .unwrap();
    }

    pub fn message_rows(&self) -> (i64, i64) {
        let db = rusqlite::Connection::open(self.state.join("namedrop.sqlite3")).unwrap();
        let count = |table: &str| {
            db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
                row.get(0)
            })
            .unwrap()
        };
        (count("collaboration_messages"), count("mailbox_deliveries"))
    }

    pub fn untrusted_body(envelope: &Value) -> &str {
        envelope["untrusted_body"].as_str().unwrap()
    }
}

fn fixture_workers(state: &std::path::Path) -> Vec<i32> {
    let output = std::process::Command::new("ps")
        .args(["axeww", "-o", "pid=,args="])
        .output()
        .expect("list fixture workers");
    assert!(output.status.success(), "could not list fixture workers");
    let state = format!("HERDR_PLUGIN_STATE_DIR={}", state.display());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| {
            (line.contains("hook mailbox-wake") || line.contains("hook agent-status-worker"))
                && line.split_whitespace().any(|entry| entry == state)
        })
        .map(|line| line.split_whitespace().next().unwrap().parse().unwrap())
        .collect()
}

impl Fixture {
    fn stop_workers(&self) {
        use std::{
            thread,
            time::{Duration, Instant},
        };
        let queue = self.state.join("agent-status-reconciliation");
        let mut known: BTreeSet<_> = fixture_workers(&self.state).into_iter().collect();
        let _ = fs::remove_dir_all(&queue);
        let deadline = Instant::now() + Duration::from_secs(15);
        let stop_deadline = deadline + Duration::from_secs(5);
        loop {
            let workers = fixture_workers(&self.state);
            known.extend(&workers);
            for pid in &known {
                unsafe { libc::waitpid(*pid, std::ptr::null_mut(), libc::WNOHANG) };
            }
            if workers.is_empty() {
                return;
            }
            assert!(
                Instant::now() < stop_deadline,
                "fixture workers did not stop: {workers:?}"
            );
            if Instant::now() >= deadline {
                for pid in &workers {
                    unsafe { libc::kill(*pid, libc::SIGKILL) };
                }
            }
            let _ = fs::remove_dir_all(&queue);
            thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop_workers();
        pane_effects_diagnostics::emit(&self.state);
        let _ = fs::remove_dir_all(&self.root);
    }
}
